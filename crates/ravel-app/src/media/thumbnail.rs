// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Asynchronous thumbnail generation with memory and disk caching.
//!
//! Memory-cache hits intentionally do not stat the source on every render.
//! Callers must call [`ThumbnailCache::invalidate`] when a known external edit
//! or relink changes a source at the same path.

use gpui::Context;
use ravel_core::color::{ColorSpace, to_display_rgba8};
use ravel_core::types::FrameBuffer;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::cache::{CacheKey, DiskCache};

const MEMORY_CACHE_CAPACITY: usize = 128;
const THUMBNAIL_LONG_EDGE: u32 = 256;
const THUMBNAIL_CACHE_VERSION: u32 = 1;

/// How the source's representative frame should be decoded.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ThumbnailSource {
    /// A video or other FFmpeg-readable container, sampled at 10% duration.
    Container,
    /// A single still image.
    Still,
    /// The representative (first) file of an image sequence.
    Sequence,
}

impl ThumbnailSource {
    fn cache_tag(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::Still => "still",
            Self::Sequence => "sequence",
        }
    }

    fn derivative_key(self, input_color_space: ColorSpace) -> String {
        format!(
            "thumbnail-png-long-edge={THUMBNAIL_LONG_EDGE}-source={}-space={input_color_space:?}-v{THUMBNAIL_CACHE_VERSION}",
            self.cache_tag()
        )
    }
}

/// Current state of a requested thumbnail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThumbnailState {
    /// PNG-encoded thumbnail bytes.
    Ready(Arc<[u8]>),
    /// Generation or disk loading is running in the background.
    Pending,
    /// This source cannot produce a thumbnail; callers should use a type icon.
    Unavailable,
}

/// Failure while decoding or encoding a thumbnail.
#[derive(Debug, thiserror::Error)]
pub enum ThumbnailError {
    #[error("media decoding is unavailable: {0}")]
    DecodeUnavailable(String),
    #[error("decoded frame has invalid dimensions or pixel data")]
    InvalidFrame,
    #[error("failed to encode thumbnail PNG: {0}")]
    Encode(#[from] image::ImageError),
}

/// Injectable decode function used by tests and alternate media backends.
///
/// The frame comes back **in the working space**: the generator decodes
/// `input_color_space` (the asset's resolved input colour space) on the way
/// in, and [`encode_thumbnail`] applies the display transform on the way out.
pub type ThumbnailGenerator = Arc<
    dyn Fn(&Path, ThumbnailSource, ColorSpace) -> Result<FrameBuffer, ThumbnailError> + Send + Sync,
>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ThumbnailRequest {
    path: PathBuf,
    source: ThumbnailSource,
    input_color_space: ColorSpace,
}

enum WorkerOutcome {
    Ready(CacheKey, Arc<[u8]>),
    Unavailable { failed_key: Option<CacheKey> },
}

#[derive(Clone)]
struct ThumbnailResolver {
    disk: DiskCache,
    generator: ThumbnailGenerator,
}

impl ThumbnailResolver {
    fn resolve(&self, request: &ThumbnailRequest) -> WorkerOutcome {
        let Some(key) = DiskCache::key(
            &request.path,
            &request.source.derivative_key(request.input_color_space),
        ) else {
            return WorkerOutcome::Unavailable { failed_key: None };
        };
        if self.disk.is_failed(&key) {
            return WorkerOutcome::Unavailable { failed_key: None };
        }
        if let Some(bytes) = self.disk.load(&key) {
            return WorkerOutcome::Ready(key, Arc::from(bytes));
        }

        let bytes = match (self.generator)(&request.path, request.source, request.input_color_space)
            .and_then(|frame| encode_thumbnail(&frame))
        {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %request.path.display(),
                    source = ?request.source,
                    "failed to generate media thumbnail"
                );
                return WorkerOutcome::Unavailable {
                    failed_key: Some(key),
                };
            }
        };

        if let Err(error) = self.disk.store(&key, &bytes) {
            tracing::debug!(
                %error,
                path = %request.path.display(),
                "continuing with memory-only media thumbnail"
            );
        }
        WorkerOutcome::Ready(key, Arc::from(bytes))
    }
}

/// Entity state for non-blocking thumbnail requests.
///
/// Construct this inside a GPUI entity and let panels observe that entity.
/// [`Self::get_or_request`] performs only small in-memory lookups on the caller
/// thread; filesystem metadata, disk-cache access, decoding, scaling, and PNG
/// encoding run on GPUI's background executor. Completion calls `cx.notify()`,
/// so observers can fetch the new [`ThumbnailState::Ready`] value.
pub struct ThumbnailCache {
    resolver: ThumbnailResolver,
    memory: MemoryLru,
    resolved: HashMap<ThumbnailRequest, CacheKey>,
    in_flight: HashMap<ThumbnailRequest, u64>,
    unavailable: HashSet<ThumbnailRequest>,
    generations: HashMap<PathBuf, u64>,
}

impl ThumbnailCache {
    /// Use an injected application configuration root.
    ///
    /// The root is the caller's decision because it is a *setting*
    /// ([`crate::app_settings::cache_root`], `SET-8`), and reading it here
    /// would put a second answer next to the one the panel already has.
    pub fn new(root: Option<PathBuf>) -> Self {
        Self::with_generator(root, Arc::new(default_thumbnail_frame))
    }

    /// Use an injected decoder and configuration root.
    pub fn with_generator(root: Option<PathBuf>, generator: ThumbnailGenerator) -> Self {
        Self::with_capacity(root, generator, MEMORY_CACHE_CAPACITY)
    }

    fn with_capacity(
        root: Option<PathBuf>,
        generator: ThumbnailGenerator,
        capacity: usize,
    ) -> Self {
        Self {
            resolver: ThumbnailResolver {
                disk: DiskCache::new_with_extension(root, "thumbnails", "png"),
                generator,
            },
            memory: MemoryLru::new(capacity),
            resolved: HashMap::new(),
            in_flight: HashMap::new(),
            unavailable: HashSet::new(),
            generations: HashMap::new(),
        }
    }

    /// Return a cached thumbnail or kick off background work for a miss.
    ///
    /// `input_color_space` is the asset's **resolved** input colour space
    /// ([`MediaAssetEntry::input_color_space`]); the frame is decoded through
    /// it and the display transform is applied before quantisation, so a
    /// linear source (EXR / HDR) no longer thumbnails dark.
    ///
    /// No filesystem or decode work occurs synchronously. Repeated calls while
    /// a request is pending share the same task, and failed requests remain
    /// unavailable until explicitly invalidated.
    ///
    /// [`MediaAssetEntry::input_color_space`]: ravel_core::composition::MediaAssetEntry::input_color_space
    pub fn get_or_request(
        &mut self,
        path: &Path,
        source: ThumbnailSource,
        input_color_space: ColorSpace,
        cx: &mut Context<Self>,
    ) -> ThumbnailState {
        let request = ThumbnailRequest {
            path: path.to_path_buf(),
            source,
            input_color_space,
        };
        if let Some(state) = self.cached_state(&request) {
            return state;
        }
        if !request.path.is_absolute() {
            self.unavailable.insert(request);
            return ThumbnailState::Unavailable;
        }

        let generation = self.generation(&request.path);
        self.in_flight.insert(request.clone(), generation);
        let resolver = self.resolver.clone();
        let worker_request = request.clone();
        let worker = cx.background_executor().spawn(async move {
            catch_unwind(AssertUnwindSafe(|| resolver.resolve(&worker_request))).unwrap_or_else(
                |panic| {
                    tracing::warn!(
                        path = %worker_request.path.display(),
                        source = ?worker_request.source,
                        panic = %panic_message(panic.as_ref()),
                        "media thumbnail worker panicked"
                    );
                    WorkerOutcome::Unavailable { failed_key: None }
                },
            )
        });
        cx.spawn(async move |this, cx| {
            let outcome = worker.fallible().await.unwrap_or_else(|| {
                tracing::warn!(
                    path = %request.path.display(),
                    source = ?request.source,
                    "media thumbnail worker was cancelled"
                );
                WorkerOutcome::Unavailable { failed_key: None }
            });
            let update_result = this.update(cx, |this, cx| {
                this.finish(request.clone(), generation, outcome);
                cx.notify();
            });
            if update_result.is_err() {
                tracing::warn!(
                    path = %request.path.display(),
                    source = ?request.source,
                    "thumbnail cache entity disappeared before worker completion"
                );
            }
        })
        .detach();

        ThumbnailState::Pending
    }

    /// Forget all state for `path`, allowing the next request to restat it.
    ///
    /// Call this when an imported asset is relinked or known to have changed.
    /// Running work is not cancelled, but its old-generation result is ignored.
    pub fn invalidate(&mut self, path: &Path) {
        let generation = self.generations.entry(path.to_path_buf()).or_default();
        *generation = generation.wrapping_add(1);

        let mut removed_keys = Vec::new();
        self.resolved.retain(|request, key| {
            if request.path == path {
                removed_keys.push(key.clone());
                false
            } else {
                true
            }
        });
        for key in removed_keys {
            self.memory.remove(&key);
        }
        self.unavailable.retain(|request| request.path != path);
        self.in_flight.retain(|request, _| request.path != path);
        self.resolver.disk.clear_failed_for_source(path);
    }

    fn cached_state(&mut self, request: &ThumbnailRequest) -> Option<ThumbnailState> {
        if self.unavailable.contains(request) {
            return Some(ThumbnailState::Unavailable);
        }
        if self.in_flight.contains_key(request) {
            return Some(ThumbnailState::Pending);
        }
        let key = self.resolved.get(request)?;
        self.memory.get(key).map(ThumbnailState::Ready)
    }

    fn finish(&mut self, request: ThumbnailRequest, generation: u64, outcome: WorkerOutcome) {
        if self.generation(&request.path) != generation {
            if self.in_flight.get(&request) == Some(&generation) {
                self.in_flight.remove(&request);
            }
            return;
        }
        self.in_flight.remove(&request);
        match outcome {
            WorkerOutcome::Ready(key, bytes) => {
                self.memory.insert(key.clone(), bytes);
                self.resolved.insert(request, key);
            }
            WorkerOutcome::Unavailable { failed_key } => {
                if let Some(key) = failed_key {
                    self.resolver.disk.mark_failed(&request.path, key);
                }
                self.unavailable.insert(request);
            }
        }
    }

    fn generation(&self, path: &Path) -> u64 {
        self.generations.get(path).copied().unwrap_or(0)
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> &str {
    if let Some(message) = panic.downcast_ref::<&'static str>() {
        message
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message
    } else {
        "non-string panic payload"
    }
}

struct MemoryLru {
    capacity: usize,
    entries: HashMap<CacheKey, Arc<[u8]>>,
    order: VecDeque<CacheKey>,
}

impl MemoryLru {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &CacheKey) -> Option<Arc<[u8]>> {
        let value = self.entries.get(key).cloned()?;
        self.touch(key);
        Some(value)
    }

    fn insert(&mut self, key: CacheKey, value: Arc<[u8]>) {
        if self.capacity == 0 {
            return;
        }
        self.entries.insert(key.clone(), value);
        self.touch(&key);
        while self.entries.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn remove(&mut self, key: &CacheKey) {
        self.entries.remove(key);
        if let Some(index) = self.order.iter().position(|candidate| candidate == key) {
            self.order.remove(index);
        }
    }

    fn touch(&mut self, key: &CacheKey) {
        if let Some(index) = self.order.iter().position(|candidate| candidate == key) {
            self.order.remove(index);
        }
        self.order.push_back(key.clone());
    }
}

fn encode_thumbnail(frame: &FrameBuffer) -> Result<Vec<u8>, ThumbnailError> {
    let pixel_count = (frame.width as usize)
        .checked_mul(frame.height as usize)
        .and_then(|count| count.checked_mul(4))
        .ok_or(ThumbnailError::InvalidFrame)?;
    if frame.width == 0 || frame.height == 0 || frame.as_f32().len() != pixel_count {
        return Err(ThumbnailError::InvalidFrame);
    }

    // Resize in the working space — linear light. Filtering after the
    // display transform would average encoded sRGB values, and a checker of
    // 0.0 / 1.0 would shrink to 128 instead of 188: the dark-thumbnail
    // symptom of MED-APP-32 all over again. The transform is the last step
    // before quantisation, so at 1:1 an sRGB source still keeps its exact
    // bytes (`ingest_and_display_round_trip_every_code` pins the identity).
    let image = image::ImageBuffer::<image::Rgba<f32>, _>::from_raw(
        frame.width,
        frame.height,
        frame.as_f32().to_vec(),
    )
    .ok_or(ThumbnailError::InvalidFrame)?;
    let long_edge = frame.width.max(frame.height);
    let (width, height) = if long_edge <= THUMBNAIL_LONG_EDGE {
        (frame.width, frame.height)
    } else {
        let width = ((u64::from(frame.width) * u64::from(THUMBNAIL_LONG_EDGE))
            / u64::from(long_edge))
        .max(1) as u32;
        let height = ((u64::from(frame.height) * u64::from(THUMBNAIL_LONG_EDGE))
            / u64::from(long_edge))
        .max(1) as u32;
        (width, height)
    };
    let resized = if (width, height) == (frame.width, frame.height) {
        image
    } else {
        image::imageops::resize(&image, width, height, image::imageops::FilterType::Lanczos3)
    };

    let mut pixels = Vec::with_capacity((width * height) as usize * 4);
    for px in resized.pixels() {
        pixels.extend_from_slice(&to_display_rgba8(px.0));
    }
    let resized =
        image::RgbaImage::from_raw(width, height, pixels).ok_or(ThumbnailError::InvalidFrame)?;

    let mut output = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(resized).write_to(&mut output, image::ImageFormat::Png)?;
    Ok(output.into_inner())
}

#[cfg(feature = "ffmpeg")]
fn default_thumbnail_frame(
    path: &Path,
    source: ThumbnailSource,
    input_color_space: ColorSpace,
) -> Result<FrameBuffer, ThumbnailError> {
    use ravel_core::media::MediaReader as _;

    match source {
        ThumbnailSource::Container => {
            let mut reader = ravel_media::decoder::FfmpegDecoder::open(path)
                .map_err(|error| ThumbnailError::DecodeUnavailable(error.to_string()))?
                .with_input_color_space(input_color_space);
            let stream = reader
                .info()
                .first_video()
                .cloned()
                .ok_or_else(|| ThumbnailError::DecodeUnavailable("no video stream".into()))?;
            let duration = reader
                .info()
                .duration_secs
                .or(stream.duration_secs)
                .unwrap_or(0.0);
            let mut frame = (duration * 0.1 * stream.frame_rate.as_f64())
                .floor()
                .max(0.0) as u64;
            if let Some(frame_count) = stream.frame_count
                && frame_count > 0
            {
                frame = frame.min(frame_count - 1);
            }
            reader
                .decode_video_frame(stream.stream_index, frame)
                .map_err(|error| ThumbnailError::DecodeUnavailable(error.to_string()))
        }
        ThumbnailSource::Still | ThumbnailSource::Sequence => {
            ravel_media::image_seq::read_image_frame_in(path, input_color_space)
                .map_err(|error| ThumbnailError::DecodeUnavailable(error.to_string()))
        }
    }
}

#[cfg(not(feature = "ffmpeg"))]
fn default_thumbnail_frame(
    _path: &Path,
    _source: ThumbnailSource,
    _input_color_space: ColorSpace,
) -> Result<FrameBuffer, ThumbnailError> {
    Err(ThumbnailError::DecodeUnavailable(
        "the `ffmpeg` feature of ravel-app is disabled".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, Entity, TestAppContext};
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn source_file(temp: &tempfile::TempDir, name: &str) -> PathBuf {
        let path = temp.path().join(name);
        fs::write(&path, b"media fixture").expect("write media fixture");
        path
    }

    fn successful_generator(calls: Arc<AtomicUsize>) -> ThumbnailGenerator {
        Arc::new(move |_path, _source, _space| {
            calls.fetch_add(1, Ordering::SeqCst);
            let mut data = Vec::with_capacity(512 * 128 * 4);
            for _ in 0..(512 * 128) {
                data.extend_from_slice(&[1.0, 0.5, 0.0, 1.0]);
            }
            Ok(FrameBuffer::from_f32(512, 128, data))
        })
    }

    fn solid_frame(width: u32, height: u32, rgba: [f32; 4]) -> FrameBuffer {
        let mut data = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..width as usize * height as usize {
            data.extend_from_slice(&rgba);
        }
        FrameBuffer::from_f32(width, height, data)
    }

    fn request(path: &Path) -> ThumbnailRequest {
        ThumbnailRequest {
            path: path.to_path_buf(),
            source: ThumbnailSource::Container,
            input_color_space: ColorSpace::SRGB,
        }
    }

    fn resolve_synchronously(cache: &mut ThumbnailCache, request: &ThumbnailRequest) {
        let generation = cache.generation(&request.path);
        let outcome = cache.resolver.resolve(request);
        cache.finish(request.clone(), generation, outcome);
    }

    fn ready_bytes(cache: &mut ThumbnailCache, request: &ThumbnailRequest) -> Arc<[u8]> {
        match cache.cached_state(request) {
            Some(ThumbnailState::Ready(bytes)) => bytes,
            state => panic!("expected ready thumbnail, got {state:?}"),
        }
    }

    fn cache_entity(
        cx: &mut TestAppContext,
        root: Option<PathBuf>,
        generator: ThumbnailGenerator,
    ) -> Entity<ThumbnailCache> {
        cx.new(|_| ThumbnailCache::with_generator(root, generator))
    }

    fn get_or_request(
        cache: &Entity<ThumbnailCache>,
        path: &Path,
        source: ThumbnailSource,
        cx: &mut TestAppContext,
    ) -> ThumbnailState {
        cache.update(cx, |cache, cx| {
            cache.get_or_request(path, source, ColorSpace::SRGB, cx)
        })
    }

    fn expect_ready(state: ThumbnailState) -> Arc<[u8]> {
        match state {
            ThumbnailState::Ready(bytes) => bytes,
            state => panic!("expected ready thumbnail, got {state:?}"),
        }
    }

    #[test]
    fn derivative_key_includes_the_decode_source() {
        assert_eq!(
            ThumbnailSource::Container.derivative_key(ColorSpace::SRGB),
            "thumbnail-png-long-edge=256-source=container-space=ColorSpace { primaries: Rec709, transfer: Srgb }-v1"
        );
        assert_ne!(
            ThumbnailSource::Container.derivative_key(ColorSpace::SRGB),
            ThumbnailSource::Still.derivative_key(ColorSpace::SRGB)
        );
        assert_ne!(
            ThumbnailSource::Still.derivative_key(ColorSpace::SRGB),
            ThumbnailSource::Sequence.derivative_key(ColorSpace::SRGB)
        );
    }

    /// The resolved input colour space is part of the cache identity: the
    /// same file relinked or reinterpreted as another space must not be
    /// served the other space's thumbnail.
    #[test]
    fn derivative_key_includes_the_colour_space() {
        assert_ne!(
            ThumbnailSource::Still.derivative_key(ColorSpace::SRGB),
            ThumbnailSource::Still.derivative_key(ColorSpace::LINEAR_REC709)
        );
    }

    /// A linear source is display-encoded before quantisation: linear 0.5
    /// displays as 188, not 128 (MED-APP-32).
    #[test]
    fn linear_frames_are_display_encoded() {
        let frame = solid_frame(64, 64, [0.5, 0.5, 0.5, 1.0]);
        let png = encode_thumbnail(&frame).expect("encode thumbnail");
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .expect("decode thumbnail png")
            .to_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0, [188, 188, 188, 255]);
        // Values above 1.0 saturate at the top code, not at 1.0's code.
        let frame = solid_frame(64, 64, [4.0, 4.0, 4.0, 1.0]);
        let png = encode_thumbnail(&frame).expect("encode thumbnail");
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .expect("decode thumbnail png")
            .to_rgba8();
        assert_eq!(decoded.get_pixel(0, 0).0, [255, 255, 255, 255]);
    }

    /// A downscaled linear source must be filtered **in linear light**. A
    /// 1-pixel checkerboard of 0.0 and 1.0 averages to linear 0.5 at 2:1,
    /// which displays as ~188; filtering after the display transform would
    /// average the encoded 0 and 255 to ~128 — the dark thumbnail of
    /// MED-APP-32 again, only at shrink time.
    #[test]
    fn downscaled_linear_frames_are_filtered_in_linear_light() {
        let mut data = Vec::with_capacity(512 * 512 * 4);
        for y in 0..512 {
            for x in 0..512 {
                let value = ((x + y) % 2) as f32;
                data.extend_from_slice(&[value, value, value, 1.0]);
            }
        }
        let frame = FrameBuffer::from_f32(512, 512, data);
        let png = encode_thumbnail(&frame).expect("encode thumbnail");
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .expect("decode thumbnail png")
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (256, 256));
        for xy in [(10, 10), (128, 128), (200, 40)] {
            let px = decoded.get_pixel(xy.0, xy.1).0;
            assert!(
                (183..=193).contains(&px[0]),
                "pixel at {xy:?} is {px:?} — filtered in the encoded space"
            );
        }
    }

    /// An sRGB source keeps the file's exact bytes: the working-space ingest
    /// of an sRGB code and the display transform are exact inverses, so
    /// nothing about an integer source's thumbnail changes.
    #[test]
    fn srgb_frames_keep_their_bytes() {
        let mut codes = Vec::new();
        let mut data = Vec::new();
        for i in 0..(64 * 64) {
            let rgba = [
                (i % 256) as u8,
                ((i * 3) % 256) as u8,
                ((i * 7) % 256) as u8,
                ((i * 13) % 256) as u8,
            ];
            codes.push(rgba);
            data.extend_from_slice(&ravel_core::color::ingest_rgba8(rgba, ColorSpace::SRGB));
        }
        let frame = FrameBuffer::from_f32(64, 64, data);
        let png = encode_thumbnail(&frame).expect("encode thumbnail");
        let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .expect("decode thumbnail png")
            .to_rgba8();
        for (i, rgba) in codes.iter().enumerate() {
            let px = decoded.get_pixel((i % 64) as u32, (i / 64) as u32).0;
            assert_eq!(&px, rgba, "pixel {i}");
        }
    }

    #[gpui::test]
    fn decode_sources_do_not_share_negative_cache_entries(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = source_file(&temp, "ambiguous.dat");
        let calls = Arc::new(AtomicUsize::new(0));
        let generator_calls = calls.clone();
        let generator: ThumbnailGenerator = Arc::new(move |_path, source, _space| {
            generator_calls.fetch_add(1, Ordering::SeqCst);
            match source {
                ThumbnailSource::Container => {
                    Err(ThumbnailError::DecodeUnavailable("not a container".into()))
                }
                ThumbnailSource::Still | ThumbnailSource::Sequence => {
                    Ok(solid_frame(8, 8, [0.0, 1.0, 0.0, 1.0]))
                }
            }
        });
        let cache = cache_entity(cx, Some(temp.path().to_path_buf()), generator);

        assert_eq!(
            get_or_request(&cache, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Pending
        );
        cx.run_until_parked();
        assert_eq!(
            get_or_request(&cache, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Unavailable
        );

        assert_eq!(
            get_or_request(&cache, &path, ThumbnailSource::Still, cx),
            ThumbnailState::Pending
        );
        cx.run_until_parked();
        assert!(matches!(
            get_or_request(&cache, &path, ThumbnailSource::Still, cx),
            ThumbnailState::Ready(_)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[gpui::test]
    fn memory_and_disk_hits_do_not_decode_again(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = source_file(&temp, "clip.mov");
        let calls = Arc::new(AtomicUsize::new(0));
        let generator = successful_generator(calls.clone());
        let cache = cache_entity(cx, Some(temp.path().to_path_buf()), generator.clone());
        let notifications = Arc::new(AtomicUsize::new(0));
        let observer_calls = notifications.clone();
        let _observer = cx.update(|cx| {
            cx.observe(&cache, move |_, _| {
                observer_calls.fetch_add(1, Ordering::SeqCst);
            })
        });

        let (first, duplicate) = cache.update(cx, |cache, cx| {
            (
                cache.get_or_request(&path, ThumbnailSource::Container, ColorSpace::SRGB, cx),
                cache.get_or_request(&path, ThumbnailSource::Container, ColorSpace::SRGB, cx),
            )
        });
        assert_eq!(first, ThumbnailState::Pending);
        assert_eq!(duplicate, ThumbnailState::Pending);
        cx.run_until_parked();

        let first_bytes = expect_ready(get_or_request(
            &cache,
            &path,
            ThumbnailSource::Container,
            cx,
        ));
        let second_bytes = expect_ready(get_or_request(
            &cache,
            &path,
            ThumbnailSource::Container,
            cx,
        ));
        assert!(Arc::ptr_eq(&first_bytes, &second_bytes));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "duplicate or memory hit decoded"
        );
        assert_eq!(notifications.load(Ordering::SeqCst), 1);

        drop(cache);
        let reopened = cache_entity(cx, Some(temp.path().to_path_buf()), generator);
        assert_eq!(
            get_or_request(&reopened, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Pending
        );
        cx.run_until_parked();
        let disk_bytes = expect_ready(get_or_request(
            &reopened,
            &path,
            ThumbnailSource::Container,
            cx,
        ));
        assert_eq!(disk_bytes.as_ref(), first_bytes.as_ref());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "disk hit decoded");

        let decoded = image::load_from_memory(&disk_bytes).expect("decode cached PNG");
        assert_eq!((decoded.width(), decoded.height()), (256, 64));
    }

    #[gpui::test]
    fn deleting_disk_cache_causes_regeneration(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = source_file(&temp, "clip.mov");
        let calls = Arc::new(AtomicUsize::new(0));
        let generator = successful_generator(calls.clone());

        let first = cache_entity(cx, Some(temp.path().to_path_buf()), generator.clone());
        assert_eq!(
            get_or_request(&first, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Pending
        );
        cx.run_until_parked();
        assert!(matches!(
            get_or_request(&first, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Ready(_)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        drop(first);
        fs::remove_dir_all(temp.path().join("cache/thumbnails")).expect("delete thumbnail cache");

        let regenerated = cache_entity(cx, Some(temp.path().to_path_buf()), generator);
        assert_eq!(
            get_or_request(&regenerated, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Pending
        );
        cx.run_until_parked();
        assert!(matches!(
            get_or_request(&regenerated, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Ready(_)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[gpui::test]
    fn failed_generation_is_not_retried(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = source_file(&temp, "unsupported.mov");
        let calls = Arc::new(AtomicUsize::new(0));
        let generator_calls = calls.clone();
        let generator: ThumbnailGenerator = Arc::new(move |_path, _source, _space| {
            generator_calls.fetch_add(1, Ordering::SeqCst);
            Err(ThumbnailError::DecodeUnavailable("unsupported".into()))
        });
        let cache = cache_entity(cx, Some(temp.path().to_path_buf()), generator);

        assert_eq!(
            get_or_request(&cache, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Pending
        );
        cx.run_until_parked();

        assert_eq!(
            get_or_request(&cache, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Unavailable
        );
        assert_eq!(
            get_or_request(&cache, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Unavailable
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "negative cache decoded");
    }

    #[gpui::test]
    fn panicking_generation_settles_as_unavailable(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = source_file(&temp, "panic.mov");
        let calls = Arc::new(AtomicUsize::new(0));
        let generator_calls = calls.clone();
        let generator: ThumbnailGenerator = Arc::new(move |_path, _source, _space| {
            generator_calls.fetch_add(1, Ordering::SeqCst);
            panic!("injected thumbnail panic");
        });
        let cache = cache_entity(cx, None, generator);

        assert_eq!(
            get_or_request(&cache, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Pending
        );
        cx.run_until_parked();
        assert_eq!(
            get_or_request(&cache, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Unavailable
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[gpui::test]
    fn invalidate_clears_failure_and_retries(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = source_file(&temp, "retry.mov");
        let calls = Arc::new(AtomicUsize::new(0));
        let generator_calls = calls.clone();
        let generator: ThumbnailGenerator = Arc::new(move |_path, _source, _space| {
            if generator_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(ThumbnailError::DecodeUnavailable("first attempt".into()))
            } else {
                Ok(solid_frame(16, 8, [1.0, 0.0, 0.0, 1.0]))
            }
        });
        let cache = cache_entity(cx, Some(temp.path().to_path_buf()), generator);

        assert_eq!(
            get_or_request(&cache, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Pending
        );
        cx.run_until_parked();
        assert_eq!(
            get_or_request(&cache, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Unavailable
        );

        cache.update(cx, |cache, _| cache.invalidate(&path));
        assert_eq!(
            get_or_request(&cache, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Pending
        );
        cx.run_until_parked();
        assert!(matches!(
            get_or_request(&cache, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Ready(_)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[gpui::test]
    fn invalidate_discards_an_in_flight_result(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = source_file(&temp, "changing.mov");
        let calls = Arc::new(AtomicUsize::new(0));
        let generator_calls = calls.clone();
        let generator: ThumbnailGenerator = Arc::new(move |_path, _source, _space| {
            let call = generator_calls.fetch_add(1, Ordering::SeqCst);
            let red = if call == 0 { 0.25 } else { 0.75 };
            Ok(solid_frame(1, 1, [red, 0.0, 0.0, 1.0]))
        });
        let cache = cache_entity(cx, None, generator);

        assert_eq!(
            get_or_request(&cache, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Pending
        );
        while calls.load(Ordering::SeqCst) == 0 {
            assert!(cx.background_executor.tick());
        }
        cache.update(cx, |cache, _| cache.invalidate(&path));
        assert_eq!(
            get_or_request(&cache, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Pending
        );
        cx.run_until_parked();

        let bytes = expect_ready(get_or_request(
            &cache,
            &path,
            ThumbnailSource::Container,
            cx,
        ));
        let image = image::load_from_memory(&bytes)
            .expect("decode thumbnail")
            .into_rgba8();
        // The frame is working-space (linear), so the thumbnail is display
        // encoded: sRGB(0.75) quantises to 225, not the raw 191.
        assert_eq!(image.get_pixel(0, 0).0, [225, 0, 0, 255]);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[gpui::test]
    fn missing_global_cache_root_still_generates_in_memory(cx: &mut TestAppContext) {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = source_file(&temp, "clip.mov");
        let calls = Arc::new(AtomicUsize::new(0));
        let generator = successful_generator(calls.clone());
        let cache = cx.new(|_| ThumbnailCache::with_generator(None, generator));

        assert_eq!(
            get_or_request(&cache, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Pending
        );
        cx.run_until_parked();
        assert!(matches!(
            get_or_request(&cache, &path, ThumbnailSource::Container, cx),
            ThumbnailState::Ready(_)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn lru_evicts_the_least_recently_used_entry() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut cache = ThumbnailCache::with_capacity(None, successful_generator(calls), 2);
        let first = request(&source_file(&temp, "first.mov"));
        let second = request(&source_file(&temp, "second.mov"));
        let third = request(&source_file(&temp, "third.mov"));

        resolve_synchronously(&mut cache, &first);
        resolve_synchronously(&mut cache, &second);
        ready_bytes(&mut cache, &first);
        resolve_synchronously(&mut cache, &third);

        assert!(cache.cached_state(&first).is_some());
        assert!(cache.cached_state(&second).is_none());
        assert!(cache.cached_state(&third).is_some());
    }
}
