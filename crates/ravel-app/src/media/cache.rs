// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Process-external cache for derived media data.

use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

/// SHA-256 identity of a source file and derivative parameters.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CacheKey(String);

impl CacheKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A namespaced disk cache with a process-local negative cache.
///
/// `root` is the application configuration root. Cached files are stored under
/// `root/cache/<namespace>`. Passing `None` disables disk persistence while
/// keeping key calculation and the negative cache usable.
#[derive(Clone, Debug)]
pub struct DiskCache {
    directory: Option<PathBuf>,
    extension: Option<String>,
    failed: Arc<Mutex<FailedEntries>>,
}

#[derive(Debug, Default)]
struct FailedEntries {
    keys: HashSet<CacheKey>,
    by_source: HashMap<PathBuf, HashSet<CacheKey>>,
}

impl DiskCache {
    /// Create a cache rooted below an injected application configuration path.
    pub fn new(root: Option<PathBuf>, namespace: &str) -> Self {
        Self::with_extension(root, namespace, None)
    }

    /// Create a cache using Ravel's global configuration directory.
    pub fn global(namespace: &str) -> Self {
        Self::new(ravel_project::paths::global_config_dir(), namespace)
    }

    /// Create a cache whose entries use a filename extension.
    pub fn new_with_extension(root: Option<PathBuf>, namespace: &str, extension: &str) -> Self {
        Self::with_extension(root, namespace, Some(extension))
    }

    /// Create an extended cache using Ravel's global configuration directory.
    pub fn global_with_extension(namespace: &str, extension: &str) -> Self {
        Self::new_with_extension(
            ravel_project::paths::global_config_dir(),
            namespace,
            extension,
        )
    }

    fn with_extension(root: Option<PathBuf>, namespace: &str, extension: Option<&str>) -> Self {
        let namespace_is_segment = matches!(
            Path::new(namespace)
                .components()
                .collect::<Vec<_>>()
                .as_slice(),
            [Component::Normal(_)]
        );
        let directory = if namespace_is_segment {
            root.map(|root| root.join("cache").join(namespace))
        } else {
            tracing::warn!(
                namespace,
                "disabling media disk cache with invalid namespace"
            );
            None
        };
        let extension = extension
            .filter(|extension| {
                !extension.is_empty() && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
            .map(str::to_owned);

        Self {
            directory,
            extension,
            failed: Arc::new(Mutex::new(FailedEntries::default())),
        }
    }

    /// Build a key from an absolute path, its current mtime and size, and an
    /// optional caller-defined derivative identifier.
    ///
    /// Waveform generation can use `DiskCache::global("waveforms")` and put
    /// the waveform segment length in `extra`; no audio-layer dependency is
    /// needed in this cache module.
    pub fn key(path: &Path, extra: &str) -> Option<CacheKey> {
        if !path.is_absolute() {
            tracing::warn!(path = %path.display(), "cannot key media cache with a relative path");
            return None;
        }

        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "failed to stat media cache source");
                return None;
            }
        };
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "failed to read media source mtime");
                return None;
            }
        };

        let mut hasher = Sha256::new();
        hash_field(&mut hasher, path.as_os_str().as_encoded_bytes());
        hasher.update(metadata.len().to_le_bytes());
        match modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => {
                hasher.update([0]);
                hasher.update(duration.as_secs().to_le_bytes());
                hasher.update(duration.subsec_nanos().to_le_bytes());
            }
            Err(error) => {
                let duration = error.duration();
                hasher.update([1]);
                hasher.update(duration.as_secs().to_le_bytes());
                hasher.update(duration.subsec_nanos().to_le_bytes());
            }
        }
        hash_field(&mut hasher, extra.as_bytes());

        Some(CacheKey(format!("{:x}", hasher.finalize())))
    }

    /// Load an entry. Missing or unreadable entries are cache misses.
    pub fn load(&self, key: &CacheKey) -> Option<Vec<u8>> {
        let path = self.entry_path(key)?;
        match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "failed to read media cache entry");
                None
            }
        }
    }

    /// Atomically store an entry through a temporary file in the destination
    /// directory. A disabled cache accepts the write as a no-op.
    pub fn store(&self, key: &CacheKey, bytes: &[u8]) -> io::Result<()> {
        let Some(path) = self.entry_path(key) else {
            return Ok(());
        };
        let directory = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "media cache entry path has no parent directory",
            )
        })?;
        if let Err(error) = fs::create_dir_all(directory) {
            tracing::warn!(%error, path = %directory.display(), "failed to create media cache directory");
            return Err(error);
        }

        let (temporary_path, mut temporary_file) = match create_temporary_file(directory, key) {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(%error, path = %directory.display(), "failed to create media cache temporary file");
                return Err(error);
            }
        };

        if let Err(error) = temporary_file
            .write_all(bytes)
            .and_then(|()| temporary_file.sync_all())
        {
            cleanup_temporary_file(&temporary_path);
            tracing::warn!(%error, path = %temporary_path.display(), "failed to write media cache temporary file");
            return Err(error);
        }
        drop(temporary_file);

        if let Err(error) = fs::rename(&temporary_path, &path) {
            if error.kind() == io::ErrorKind::AlreadyExists && path.is_file() {
                cleanup_temporary_file(&temporary_path);
                return Ok(());
            }
            cleanup_temporary_file(&temporary_path);
            tracing::warn!(%error, from = %temporary_path.display(), to = %path.display(), "failed to publish media cache entry");
            return Err(error);
        }
        Ok(())
    }

    /// Remember a generation failure for the lifetime of this cache.
    pub fn mark_failed(&self, source: &Path, key: CacheKey) {
        let mut failed = self
            .failed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        failed.keys.insert(key.clone());
        failed
            .by_source
            .entry(source.to_path_buf())
            .or_default()
            .insert(key);
    }

    /// Whether generation already failed for this key in this process.
    pub fn is_failed(&self, key: &CacheKey) -> bool {
        self.failed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys
            .contains(key)
    }

    /// Forget every process-local generation failure recorded for `source`.
    pub fn clear_failed_for_source(&self, source: &Path) {
        let mut failed = self
            .failed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(keys) = failed.by_source.remove(source) else {
            return;
        };
        for key in keys {
            failed.keys.remove(&key);
        }
    }

    fn entry_path(&self, key: &CacheKey) -> Option<PathBuf> {
        self.directory.as_ref().map(|directory| {
            let mut file_name = key.0.clone();
            if let Some(extension) = &self.extension {
                file_name.push('.');
                file_name.push_str(extension);
            }
            directory.join(file_name)
        })
    }
}

fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(bytes.len().to_le_bytes());
    hasher.update(bytes);
}

fn create_temporary_file(directory: &Path, key: &CacheKey) -> io::Result<(PathBuf, fs::File)> {
    for _ in 0..16 {
        let serial = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let file_name = format!(".{}.{}.{serial}.tmp", key.as_str(), std::process::id());
        let path = directory.join(file_name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique media cache temporary file",
    ))
}

fn cleanup_temporary_file(path: &Path) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(%error, path = %path.display(), "failed to remove media cache temporary file");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_file(temp: &tempfile::TempDir, bytes: &[u8]) -> PathBuf {
        let path = temp.path().join("source.mov");
        fs::write(&path, bytes).expect("write source fixture");
        path
    }

    #[test]
    fn key_includes_source_metadata_and_extra_identifier() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let path = source_file(&temp, b"first");
        let first = DiskCache::key(&path, "segment=1").expect("key source");
        let other_extra = DiskCache::key(&path, "segment=2").expect("key source");
        assert_eq!(first.as_str().len(), 64);
        assert_ne!(first, other_extra);

        fs::write(&path, b"different-size").expect("replace source fixture");
        let changed = DiskCache::key(&path, "segment=1").expect("key changed source");
        assert_ne!(first, changed);
    }

    #[test]
    fn store_and_load_round_trip_without_temporary_files() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let source = source_file(&temp, b"source");
        let cache =
            DiskCache::new_with_extension(Some(temp.path().to_path_buf()), "thumbnails", "png");
        let key = DiskCache::key(&source, "").expect("key source");

        cache.store(&key, b"png bytes").expect("store cache entry");
        assert_eq!(cache.load(&key).as_deref(), Some(b"png bytes".as_slice()));

        let directory = temp.path().join("cache/thumbnails");
        let names = fs::read_dir(directory)
            .expect("read cache directory")
            .map(|entry| entry.expect("read cache entry").file_name())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [std::ffi::OsString::from(format!("{}.png", key.as_str()))]
        );
    }

    #[test]
    fn disabled_disk_cache_is_a_safe_no_op() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let source = source_file(&temp, b"source");
        let key = DiskCache::key(&source, "").expect("key source");
        let cache = DiskCache::new(None, "thumbnails");

        cache.store(&key, b"ignored").expect("disabled store");
        assert_eq!(cache.load(&key), None);
        cache.mark_failed(&source, key.clone());
        assert!(cache.is_failed(&key));
        cache.clear_failed_for_source(&source);
        assert!(!cache.is_failed(&key));
    }

    #[test]
    fn relative_paths_cannot_be_keyed() {
        assert_eq!(DiskCache::key(Path::new("relative.mov"), ""), None);
    }
}
