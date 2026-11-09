// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GPU-resident geometry (`docs/implementation/gpu-resident-geometry-plan.md`,
//! unit 1).
//!
//! [`GpuGeometry`] is the GPU counterpart of [`ravel_core::geometry::Geometry`]
//! the way [`GpuFrameBuffer`](crate::GpuFrameBuffer) is the counterpart of the
//! CPU frame: it shares [`DataTypeId::GEOMETRY`], reports
//! [`NodeData::is_gpu_resident`], and the CPU type stays the reference
//! implementation and the golden-test oracle.
//!
//! ## What is resident
//!
//! **Attribute columns only**, as one storage buffer per `(domain, name)`.
//! Topology — the primitive ranges, the mesh index blob and the instance
//! sources — stays on the CPU, in a `Geometry` stripped of its resident
//! columns (the plan's "属性列だけを GPU に置く").
//!
//! Two kinds of column deliberately stay CPU-resident:
//!
//! * **`Str` columns** have no GPU layout (the specification calls them a
//!   low-frequency use).
//! * **The `Detail` domain** — one value per attribute. A 256-byte buffer per
//!   scalar is pure overhead, and a shader takes such a value as a uniform the
//!   CPU already holds.
//!
//! ## Element layout
//!
//! Each column is a tightly packed WGSL `array<T>` (see [`column_stride`]):
//!
//! | `AttributeType` | WGSL | stride | note |
//! |---|---|---|---|
//! | `F32` | `f32` | 4 | |
//! | `Vec2` | `vec2<f32>` | 8 | |
//! | `Vec3` | `vec3<f32>` | 16 | WGSL array stride; `w` is written 0 and dropped on readback |
//! | `Vec4` | `vec4<f32>` | 16 | |
//! | `Color` | `vec4<f32>` | 16 | `r g b a` order |
//! | `I32` | `i32` | 4 | |
//! | `Bool` | `u32` | 4 | 0 / 1; readback maps non-zero to `true` |
//! | `Str` | — | — | CPU-resident |
//!
//! The buffer holds the column's size class (see
//! [`BufferKey`](crate::BufferKey)), larger than its data, so the element
//! count travels with the column ([`GpuColumn::len`]); a shader is told the
//! count, never asked for `arrayLength`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use ravel_core::geometry::{
    AttrName, AttributeArray, AttributeSet, AttributeType, Domain, Geometry, InstanceSource,
    Primitive,
};
use ravel_core::id::DataTypeId;
use ravel_core::types::{Color, NodeData, Vec2, Vec3, Vec4};

use crate::buffer_pool::{BufferPool, BufferUsage, PooledBuffer};
use crate::device::GpuContext;
use crate::error::{GpuError, GpuResult};

/// Bytes one element of a resident column of type `ty` occupies, or `None`
/// for a type with no GPU layout (`Str`).
pub const fn column_stride(ty: AttributeType) -> Option<u32> {
    match ty {
        AttributeType::F32 | AttributeType::I32 | AttributeType::Bool => Some(4),
        AttributeType::Vec2 => Some(8),
        AttributeType::Vec3 | AttributeType::Vec4 | AttributeType::Color => Some(16),
        AttributeType::Str => None,
    }
}

/// The WGSL element type a resident column of `ty` is declared as
/// (`array<T>`), or `None` for a CPU-only type.
pub const fn column_wgsl_type(ty: AttributeType) -> Option<&'static str> {
    // One table, owned by the WGSL the fields emit (`ravel-core`), so a
    // generated shader and a column's layout cannot disagree.
    ravel_core::geometry::wgsl_element_type(ty)
}

/// Whether a column of `ty` in `domain` is made resident.
fn is_resident(domain: Domain, ty: AttributeType) -> bool {
    domain != Domain::Detail && column_stride(ty).is_some()
}

/// The little-endian bytes of `column` in the layout above, or `None` for a
/// type without one. Floats keep their exact bit patterns (NaN payloads,
/// signed zero).
fn encode_column(column: &AttributeArray) -> Option<Vec<u8>> {
    fn flat<T, const N: usize>(items: &[T], lane: impl Fn(&T) -> [f32; N]) -> Vec<f32> {
        items.iter().flat_map(lane).collect()
    }
    let floats = match column {
        AttributeArray::F32(v) => return Some(bytemuck::cast_slice(v).to_vec()),
        AttributeArray::I32(v) => return Some(bytemuck::cast_slice(v).to_vec()),
        AttributeArray::Bool(v) => {
            let words: Vec<u32> = v.iter().map(|&b| u32::from(b)).collect();
            return Some(bytemuck::cast_slice(&words).to_vec());
        }
        AttributeArray::Vec2(v) => flat(v, |p| [p.0, p.1]),
        AttributeArray::Vec3(v) => flat(v, |p| [p.0, p.1, p.2, 0.0]),
        AttributeArray::Vec4(v) => flat(v, |p| [p.0, p.1, p.2, p.3]),
        AttributeArray::Color(v) => flat(v, |c| [c.r, c.g, c.b, c.a]),
        AttributeArray::Str(_) => return None,
    };
    Some(bytemuck::cast_slice(&floats).to_vec())
}

/// The inverse of [`encode_column`]: `len` elements of `ty` from `bytes`.
fn decode_column(ty: AttributeType, bytes: &[u8], len: usize) -> GpuResult<AttributeArray> {
    let stride = column_stride(ty)
        .ok_or_else(|| GpuError::GeometryLayout(format!("{ty} has no GPU layout")))?
        as usize;
    if bytes.len() != len * stride {
        return Err(GpuError::GeometryLayout(format!(
            "{ty} column readback is {} bytes, expected {}",
            bytes.len(),
            len * stride
        )));
    }
    // `pod_collect_to_vec` copes with the byte slice's arbitrary alignment.
    let words_f32 = || bytemuck::pod_collect_to_vec::<u8, f32>(bytes);
    let lanes = |n: usize| {
        let all = words_f32();
        all.chunks_exact(n).map(<[f32]>::to_vec).collect::<Vec<_>>()
    };
    Ok(match ty {
        AttributeType::F32 => AttributeArray::F32(words_f32()),
        AttributeType::I32 => AttributeArray::I32(bytemuck::pod_collect_to_vec(bytes)),
        AttributeType::Bool => AttributeArray::Bool(
            bytemuck::pod_collect_to_vec::<u8, u32>(bytes)
                .into_iter()
                .map(|w| w != 0)
                .collect(),
        ),
        AttributeType::Vec2 => {
            AttributeArray::Vec2(lanes(2).iter().map(|l| Vec2(l[0], l[1])).collect())
        }
        AttributeType::Vec3 => {
            AttributeArray::Vec3(lanes(4).iter().map(|l| Vec3(l[0], l[1], l[2])).collect())
        }
        AttributeType::Vec4 => AttributeArray::Vec4(
            lanes(4)
                .iter()
                .map(|l| Vec4(l[0], l[1], l[2], l[3]))
                .collect(),
        ),
        AttributeType::Color => AttributeArray::Color(
            lanes(4)
                .iter()
                .map(|l| Color {
                    r: l[0],
                    g: l[1],
                    b: l[2],
                    a: l[3],
                })
                .collect(),
        ),
        AttributeType::Str => unreachable!("rejected by column_stride above"),
    })
}

/// Returns the lease to its pool exactly once, when the last
/// [`GpuColumn`] clone drops (the same shape as `GpuFrameBuffer`'s handle).
struct ColumnLease {
    pool: Weak<Mutex<BufferPool>>,
    buffer: Option<PooledBuffer>,
}

impl Drop for ColumnLease {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take()
            && let Some(pool) = self.pool.upgrade()
            && let Ok(mut pool) = pool.lock()
        {
            pool.release(buffer);
        }
    }
}

/// One attribute column resident in GPU memory. Cloning shares the buffer.
#[derive(Clone)]
pub struct GpuColumn {
    lease: Arc<ColumnLease>,
    ty: AttributeType,
    len: usize,
}

impl GpuColumn {
    /// Element type of the column.
    pub fn attr_type(&self) -> AttributeType {
        self.ty
    }

    /// Number of elements (the buffer is larger than `len * stride`).
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the column has no elements.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The storage buffer holding the column.
    pub fn buffer(&self) -> &PooledBuffer {
        self.lease.buffer.as_ref().expect("present until drop")
    }

    fn data_bytes(&self) -> u64 {
        self.len as u64 * u64::from(column_stride(self.ty).expect("resident type"))
    }

    /// Upload `column` into a buffer leased from `pool` (one counted upload).
    /// `None` for a type with no GPU layout (`Str`). The buffer returns to the
    /// pool when the last clone of the column drops.
    pub fn upload(
        ctx: &GpuContext,
        pool: &Arc<Mutex<BufferPool>>,
        column: &AttributeArray,
    ) -> Option<Self> {
        let bytes = encode_column(column)?;
        let buffer = pool
            .lock()
            .expect("buffer pool poisoned")
            .acquire(bytes.len() as u64, BufferUsage::COLUMN);
        // Outside the pool lock, as texture uploads are.
        crate::transfer::upload_buffer(ctx, &buffer, &bytes);
        Some(Self::adopt(pool, buffer, column.attr_type(), column.len()))
    }

    /// Wrap a buffer a pass has filled with `len` elements of `ty` (laid out as
    /// [`column_stride`] says) as a column, returned to `pool` on the last drop.
    pub fn adopt(
        pool: &Arc<Mutex<BufferPool>>,
        buffer: PooledBuffer,
        ty: AttributeType,
        len: usize,
    ) -> Self {
        debug_assert!(
            column_stride(ty)
                .is_some_and(|stride| u64::from(stride) * len as u64 <= buffer.capacity()),
            "the buffer must hold the column"
        );
        Self {
            lease: Arc::new(ColumnLease {
                pool: Arc::downgrade(pool),
                buffer: Some(buffer),
            }),
            ty,
            len,
        }
    }

    /// Read the column back to the CPU. Blocks (and flushes a batch that writes
    /// it); a true boundary.
    pub fn read(&self, ctx: &GpuContext) -> GpuResult<AttributeArray> {
        let bytes = crate::transfer::begin_read_buffer(ctx, self.buffer(), self.data_bytes())?
            .wait_into_vec()?;
        decode_column(self.ty, &bytes, self.len)
    }
}

/// Element counts per domain, which the stripped host geometry cannot report
/// once its columns have moved to the GPU.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    points: usize,
    primitives: usize,
    instances: usize,
}

/// A geometry whose numeric attribute columns live in VRAM.
#[derive(Clone)]
pub struct GpuGeometry {
    ctx: GpuContext,
    columns: HashMap<(Domain, AttrName), GpuColumn>,
    /// The CPU geometry minus every resident column: topology, instance
    /// sources, and the columns that stay on the host.
    host: Arc<Geometry>,
    counts: Counts,
}

impl std::fmt::Debug for GpuGeometry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuGeometry")
            .field("columns", &self.columns.len())
            .field("points", &self.counts.points)
            .field("primitives", &self.counts.primitives)
            .field("instances", &self.counts.instances)
            .finish_non_exhaustive()
    }
}

const RESIDENT_DOMAINS: [Domain; 3] = [Domain::Point, Domain::Primitive, Domain::Instance];

fn set_mut(geometry: &mut Geometry, domain: Domain) -> &mut AttributeSet {
    match domain {
        Domain::Point => geometry.points_mut(),
        Domain::Primitive => geometry.primitive_attrs_mut(),
        Domain::Instance => geometry.instances_mut(),
        Domain::Detail => geometry.detail_mut(),
    }
}

impl GpuGeometry {
    /// Upload `geometry`'s numeric columns into pool buffers and keep the rest
    /// on the host (the inverse of [`GpuGeometry::to_geometry`]). Every column
    /// is one counted upload on the context.
    pub fn from_geometry(
        ctx: GpuContext,
        pool: &Arc<Mutex<BufferPool>>,
        geometry: &Geometry,
    ) -> GpuResult<Self> {
        let mut host = geometry.clone();
        let mut columns = HashMap::new();
        for domain in RESIDENT_DOMAINS {
            for (name, column) in geometry.attribute_set(domain).iter() {
                if !is_resident(domain, column.attr_type()) {
                    continue;
                }
                let uploaded =
                    GpuColumn::upload(&ctx, pool, column).expect("resident types encode");
                set_mut(&mut host, domain).remove(name);
                columns.insert((domain, name.clone()), uploaded);
            }
        }
        Ok(Self {
            ctx,
            columns,
            host: Arc::new(host),
            counts: Counts {
                points: geometry.point_count(),
                primitives: geometry.primitive_count(),
                instances: geometry.instance_count(),
            },
        })
    }

    /// Read every resident column back and rebuild the CPU geometry. Blocks;
    /// call only at true CPU boundaries (CPU-only nodes, the spreadsheet,
    /// goldens, export).
    ///
    /// All copies are submitted before the first wait, so the readbacks
    /// overlap instead of serialising on each other's latency.
    pub fn to_geometry(&self) -> GpuResult<Geometry> {
        let pending = self
            .columns
            .iter()
            .map(|(key, column)| {
                crate::transfer::begin_read_buffer(&self.ctx, column.buffer(), column.data_bytes())
                    .map(|read| (key, column, read))
            })
            .collect::<GpuResult<Vec<_>>>()?;
        let mut geometry = (*self.host).clone();
        for ((domain, name), column, read) in pending {
            let bytes = read.wait_into_vec()?;
            let array = decode_column(column.ty, &bytes, column.len)?;
            set_mut(&mut geometry, *domain)
                .insert(name.clone(), array)
                .map_err(|e| GpuError::GeometryLayout(e.to_string()))?;
        }
        Ok(geometry)
    }

    /// The context this geometry's GPU work is submitted through.
    pub fn context(&self) -> &GpuContext {
        &self.ctx
    }

    /// The resident column `name` of `domain`, if there is one. A `Str`
    /// column, or anything in `Detail`, is not here: see
    /// [`Self::host_attribute`].
    pub fn column(&self, domain: Domain, name: &str) -> Option<&GpuColumn> {
        self.columns.get(&(domain, AttrName::from(name)))
    }

    /// Every resident column.
    pub fn columns(&self) -> impl Iterator<Item = (Domain, &AttrName, &GpuColumn)> {
        self.columns.iter().map(|((d, n), c)| (*d, n, c))
    }

    /// This geometry with `name` of `domain` replaced by (or added as) the
    /// resident `column`. Every other column is shared, not copied; a CPU
    /// column of the same name is dropped so the name has one home.
    pub fn with_column(&self, domain: Domain, name: &str, column: GpuColumn) -> Self {
        let mut host = (*self.host).clone();
        set_mut(&mut host, domain).remove(name);
        let mut columns = self.columns.clone();
        columns.insert((domain, AttrName::from(name)), column);
        Self {
            ctx: self.ctx.clone(),
            columns,
            host: Arc::new(host),
            counts: self.counts,
        }
    }

    /// Type and length of attribute `name` of `domain`, wherever it lives
    /// (resident or on the host).
    pub fn attribute_info(&self, domain: Domain, name: &str) -> Option<(AttributeType, usize)> {
        self.column(domain, name)
            .map(|column| (column.attr_type(), column.len()))
            .or_else(|| {
                self.host_attribute(domain, name)
                    .map(|column| (column.attr_type(), column.len()))
            })
    }

    /// The CPU geometry (topology and host columns) with only the listed
    /// resident columns read back: enough for code that derives something from a
    /// few columns without paying for a full [`Self::to_geometry`]. A name that
    /// is not resident is skipped.
    pub fn host_with_columns(&self, wanted: &[(Domain, &str)]) -> GpuResult<Geometry> {
        let mut geometry = (*self.host).clone();
        for (domain, name) in wanted {
            if let Some(column) = self.column(*domain, name) {
                set_mut(&mut geometry, *domain)
                    .insert(*name, column.read(&self.ctx)?)
                    .map_err(|e| GpuError::GeometryLayout(e.to_string()))?;
            }
        }
        Ok(geometry)
    }

    /// A column that stayed on the CPU (`Str`, or any `Detail` attribute).
    pub fn host_attribute(&self, domain: Domain, name: &str) -> Option<&Arc<AttributeArray>> {
        self.host.attribute_set(domain).get(name)
    }

    /// Point count (resident `P` and friends all have this length).
    pub fn point_count(&self) -> usize {
        self.counts.points
    }

    /// Primitive count.
    pub fn primitive_count(&self) -> usize {
        self.counts.primitives
    }

    /// Instance count.
    pub fn instance_count(&self) -> usize {
        self.counts.instances
    }

    /// Primitive ranges (CPU-side topology).
    pub fn primitives(&self) -> &[Primitive] {
        self.host.primitives()
    }

    /// The mesh index blob (CPU-side topology).
    pub fn indices(&self) -> &[u32] {
        self.host.indices()
    }

    /// Instance sources (CPU-side topology).
    pub fn sources(&self) -> &[InstanceSource] {
        self.host.sources()
    }
}

impl NodeData for GpuGeometry {
    fn data_type_id(&self) -> DataTypeId {
        DataTypeId::GEOMETRY
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn is_gpu_resident(&self) -> bool {
        true
    }

    fn byte_size(&self) -> u64 {
        // The buffers' capacities (VRAM, the size class actually allocated)
        // plus the host remainder, which carries the instance sources.
        self.columns
            .values()
            .fold(size_of::<Self>() as u64, |total, c| {
                total.saturating_add(c.buffer().capacity())
            })
            .saturating_add(self.host.byte_size())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One column of every type, two elements, with values that expose a
    /// lossy path: signed zero, a NaN payload, extremes, an `i32::MIN`.
    fn every_column() -> Vec<AttributeArray> {
        let nan = f32::from_bits(0x7fc0_1234);
        vec![
            AttributeArray::F32(vec![-0.0, nan]),
            AttributeArray::Vec2(vec![Vec2(f32::MAX, f32::MIN_POSITIVE), Vec2(-0.0, nan)]),
            AttributeArray::Vec3(vec![Vec3(1.0, -2.0, 3.5), Vec3(nan, 0.0, -0.0)]),
            AttributeArray::Vec4(vec![
                Vec4(1.0, 2.0, 3.0, 4.0),
                Vec4(-1.0, nan, 0.5, f32::MAX),
            ]),
            AttributeArray::Color(vec![
                Color {
                    r: 0.1,
                    g: 0.2,
                    b: 0.3,
                    a: 0.4,
                },
                Color {
                    r: 1.0,
                    g: nan,
                    b: -0.0,
                    a: 0.0,
                },
            ]),
            AttributeArray::I32(vec![i32::MIN, i32::MAX]),
            AttributeArray::Bool(vec![true, false]),
        ]
    }

    /// Equality that treats NaN as its bit pattern, so a round trip that
    /// canonicalises a NaN or turns `-0.0` into `0.0` fails.
    fn bits(column: &AttributeArray) -> Vec<u32> {
        let f = |x: &f32| x.to_bits();
        match column {
            AttributeArray::F32(v) => v.iter().map(f).collect(),
            AttributeArray::Vec2(v) => v
                .iter()
                .flat_map(|p| [p.0, p.1])
                .map(|x| x.to_bits())
                .collect(),
            AttributeArray::Vec3(v) => v
                .iter()
                .flat_map(|p| [p.0, p.1, p.2])
                .map(|x| x.to_bits())
                .collect(),
            AttributeArray::Vec4(v) => v
                .iter()
                .flat_map(|p| [p.0, p.1, p.2, p.3])
                .map(|x| x.to_bits())
                .collect(),
            AttributeArray::Color(v) => v
                .iter()
                .flat_map(|c| [c.r, c.g, c.b, c.a])
                .map(|x| x.to_bits())
                .collect(),
            AttributeArray::I32(v) => v.iter().map(|&x| x as u32).collect(),
            AttributeArray::Bool(v) => v.iter().map(|&b| u32::from(b)).collect(),
            AttributeArray::Str(_) => unreachable!(),
        }
    }

    #[test]
    fn encode_decode_is_bit_exact_for_every_non_str_type() {
        for column in every_column() {
            let ty = column.attr_type();
            let bytes = encode_column(&column).expect("non-Str encodes");
            assert_eq!(
                bytes.len(),
                column.len() * column_stride(ty).unwrap() as usize,
                "{ty}"
            );
            let back = decode_column(ty, &bytes, column.len()).unwrap();
            assert_eq!(bits(&back), bits(&column), "{ty} changed in the round trip");
        }
    }

    #[test]
    fn str_has_no_layout_and_is_refused() {
        let column = AttributeArray::Str(vec!["a".into()]);
        assert!(encode_column(&column).is_none());
        assert!(column_stride(AttributeType::Str).is_none());
        assert!(column_wgsl_type(AttributeType::Str).is_none());
        assert!(decode_column(AttributeType::Str, &[], 0).is_err());
    }

    #[test]
    fn a_wrong_sized_readback_is_an_error_not_a_short_column() {
        assert!(decode_column(AttributeType::Vec2, &[0; 12], 2).is_err());
    }

    #[test]
    fn vec3_pads_to_sixteen_bytes_with_a_zero_w() {
        let bytes = encode_column(&AttributeArray::Vec3(vec![Vec3(1.0, 2.0, 3.0)])).unwrap();
        let words = bytemuck::pod_collect_to_vec::<u8, f32>(&bytes);
        assert_eq!(words, vec![1.0, 2.0, 3.0, 0.0]);
    }

    // ----- GPU-backed ------------------------------------------------------

    fn rig() -> Option<(GpuContext, Arc<Mutex<BufferPool>>)> {
        let Some(ctx) = GpuContext::new_blocking().ok() else {
            eprintln!("skipping: no GPU adapter available");
            return None;
        };
        let pool = Arc::new(Mutex::new(BufferPool::new(ctx.clone(), 64 * 1024 * 1024)));
        Some((ctx, pool))
    }

    /// Points carrying every column type (and a Str), plus a primitive column,
    /// an instance column and a detail value.
    fn sample_geometry() -> Geometry {
        let mut g = Geometry::from_points(vec![Vec2(0.0, 1.0), Vec2(2.0, 3.0)]);
        for (i, column) in every_column().into_iter().enumerate() {
            g.points_mut().insert(format!("c{i}"), column).unwrap();
        }
        g.points_mut()
            .insert("label", AttributeArray::Str(vec!["a".into(), "b".into()]))
            .unwrap();
        g.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        g.primitive_attrs_mut()
            .insert("w", AttributeArray::F32(vec![0.5]))
            .unwrap();
        g.instances_mut()
            .insert("P", AttributeArray::Vec2(vec![Vec2(1.0, 2.0)]))
            .unwrap();
        g.instances_mut()
            .insert("s", AttributeArray::Vec2(vec![Vec2(1.0, 2.0)]))
            .unwrap();
        g.detail_mut()
            .insert("note", AttributeArray::F32(vec![9.0]))
            .unwrap();
        g
    }

    #[test]
    fn upload_then_readback_round_trips_every_column_exactly() {
        let Some((ctx, pool)) = rig() else { return };
        let source = sample_geometry();
        assert_eq!(
            source.validate(),
            Ok(()),
            "the fixture itself must be valid"
        );
        let resident = GpuGeometry::from_geometry(ctx, &pool, &source).unwrap();
        let back = resident.to_geometry().unwrap();

        for domain in [
            Domain::Point,
            Domain::Primitive,
            Domain::Instance,
            Domain::Detail,
        ] {
            let (want, got) = (source.attribute_set(domain), back.attribute_set(domain));
            assert_eq!(want.describe(), got.describe(), "{domain:?}");
            for (name, column) in want.iter() {
                let other = got.get(name).unwrap();
                match column.as_ref() {
                    AttributeArray::Str(_) => assert_eq!(column, other),
                    c => assert_eq!(bits(other), bits(c), "{domain:?}/{name}"),
                }
            }
        }
        assert_eq!(back.primitives(), source.primitives());
        assert_eq!(back.validate(), Ok(()));
    }

    #[test]
    fn a_str_column_stays_on_the_host_while_the_rest_is_resident() {
        let Some((ctx, pool)) = rig() else { return };
        let resident = GpuGeometry::from_geometry(ctx, &pool, &sample_geometry()).unwrap();

        assert!(
            resident.column(Domain::Point, "label").is_none(),
            "Str must not be resident"
        );
        assert!(resident.host_attribute(Domain::Point, "label").is_some());
        for name in ["P", "c0", "c1", "c2", "c3", "c4", "c5", "c6"] {
            assert!(
                resident.column(Domain::Point, name).is_some(),
                "{name} resident"
            );
            assert!(
                resident.host_attribute(Domain::Point, name).is_none(),
                "{name} left the host"
            );
        }
        assert!(resident.column(Domain::Primitive, "w").is_some());
        assert!(resident.column(Domain::Instance, "s").is_some());
        // Detail is single values: host-resident by design.
        assert!(resident.column(Domain::Detail, "note").is_none());
        assert!(resident.host_attribute(Domain::Detail, "note").is_some());
        assert_eq!(resident.point_count(), 2);
        assert!(NodeData::is_gpu_resident(&resident));
        assert_eq!(NodeData::data_type_id(&resident), DataTypeId::GEOMETRY);
    }

    #[test]
    fn dropping_the_last_handle_returns_every_buffer_and_the_next_upload_reuses_them() {
        let Some((ctx, pool)) = rig() else { return };
        let source = sample_geometry();
        let first = GpuGeometry::from_geometry(ctx.clone(), &pool, &source).unwrap();
        let clone = first.clone();
        let created = pool.lock().unwrap().total_created();
        let columns = first.columns().count();

        drop(first);
        assert_eq!(
            pool.lock().unwrap().idle_count(),
            0,
            "a clone still holds them"
        );
        drop(clone);
        assert_eq!(
            pool.lock().unwrap().idle_count(),
            columns,
            "returned on last drop"
        );

        let second = GpuGeometry::from_geometry(ctx, &pool, &source).unwrap();
        assert_eq!(
            pool.lock().unwrap().total_created(),
            created,
            "an identical re-upload must allocate nothing"
        );
        assert_eq!(pool.lock().unwrap().idle_count(), 0);
        drop(second);
    }

    #[test]
    fn an_empty_geometry_round_trips() {
        let Some((ctx, pool)) = rig() else { return };
        let empty = Geometry::from_points(Vec::new());
        let back = GpuGeometry::from_geometry(ctx, &pool, &empty)
            .unwrap()
            .to_geometry()
            .unwrap();
        assert_eq!(back.point_count(), 0);
    }

    /// A column uploaded on its own reads back bit for bit, and `with_column`
    /// swaps exactly one column while sharing the rest.
    #[test]
    fn a_standalone_column_round_trips_and_replaces_one_column() {
        let Some((ctx, pool)) = rig() else { return };
        for column in every_column() {
            let uploaded = GpuColumn::upload(&ctx, &pool, &column).expect("a GPU layout");
            assert_eq!(uploaded.attr_type(), column.attr_type());
            assert_eq!(bits(&uploaded.read(&ctx).unwrap()), bits(&column));
        }
        let mut cpu = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 1.0)]);
        cpu.points_mut()
            .insert("w", AttributeArray::F32(vec![1.0, 2.0]))
            .unwrap();
        cpu.points_mut()
            .insert("tag", AttributeArray::Str(vec!["a".into(), "b".into()]))
            .unwrap();
        let resident = GpuGeometry::from_geometry(ctx.clone(), &pool, &cpu).unwrap();
        let replacement =
            GpuColumn::upload(&ctx, &pool, &AttributeArray::F32(vec![9.0, 8.0])).unwrap();
        let swapped = resident.with_column(Domain::Point, "w", replacement);
        let back = swapped.to_geometry().unwrap();
        assert_eq!(
            back.points().get("w").unwrap().as_f32("w").unwrap(),
            &[9.0, 8.0]
        );
        // The original is immutable, and the other columns are the same buffers.
        assert_eq!(
            resident
                .to_geometry()
                .unwrap()
                .points()
                .get("w")
                .unwrap()
                .as_f32("w")
                .unwrap(),
            &[1.0, 2.0]
        );
        assert_eq!(
            swapped.column(Domain::Point, "P").unwrap().buffer().id(),
            resident.column(Domain::Point, "P").unwrap().buffer().id()
        );
        // Resident and host columns both answer `attribute_info`; absent ones do not.
        assert_eq!(
            swapped.attribute_info(Domain::Point, "w"),
            Some((AttributeType::F32, 2))
        );
        assert_eq!(
            swapped.attribute_info(Domain::Point, "tag"),
            Some((AttributeType::Str, 2))
        );
        assert_eq!(swapped.attribute_info(Domain::Point, "nope"), None);
        assert!(GpuColumn::upload(&ctx, &pool, &AttributeArray::Str(vec!["x".into()])).is_none());
    }
}
