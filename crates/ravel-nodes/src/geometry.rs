// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Geometry-level operations (CPU-only): `geometry.transform` and
//! `geometry.merge`.
//!
//! Operate on whole [`Geometry`] values with copy-on-write attribute
//! columns — untouched columns keep sharing their `Arc` with the input.

use anyhow::Context as _;
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{
    AttributeArray, AttributeSet, ConnectInterpolation, ConnectMode, Domain, Geometry,
    InstanceImage, InstanceSource, bounds_center, connect, names,
};
use ravel_core::graph::Node;
use ravel_core::types::{Color, NodeData, Vec2, Vec3, Vec4};
use std::sync::Arc;

fn geometry_input<'a>(
    inputs: &'a [Option<Arc<dyn NodeData>>],
    index: usize,
    processor: &str,
) -> anyhow::Result<&'a Geometry> {
    inputs
        .get(index)
        .and_then(|input| input.as_ref())
        .and_then(|input| input.downcast_ref::<Geometry>())
        .with_context(|| format!("{processor}: input {index} is not Geometry"))
}

/// `geometry.transform`: scale → rotate → translate around a pivot,
/// applied to the point-domain `P` column and, when instances exist, to
/// the instance placement (`P` transformed, `rot` offset by the rotation,
/// `scale` multiplied component-wise in the instance's local axes).
///
/// `use_centroid` (default on) pivots on the bounding-box center of the
/// point positions (instance positions when there are no points);
/// otherwise `pivot` is used.
///
/// The number of components follows the `P` column of each domain
/// (REQ-3D-003). A `Vec2` column is transformed exactly as it always was —
/// `translate.z` / `rotation.x` / `rotation.y` / `scale.z` have nothing to act
/// on and are ignored. A `Vec3` column uses all three components, rotating by
/// the Euler angles in the fixed ZYX order of the procedural geometry spec.
///
/// Instance `rot` (F32) and `scale` (Vec2) are 2D-only standard attributes, so
/// they keep composing with the Z rotation and the xy scale whatever the
/// dimension of the instance `P`; per-instance 3D orientation arrives with the
/// `orient` / `scale3` attributes.
pub struct GeometryTransformProcessor;

impl GeometryTransformProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for GeometryTransformProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let geometry = geometry_input(inputs, 0, "geometry.transform")?;

        let [tx, ty, tz] = params.vec3_or("translate", [0.0, 0.0, 0.0]);
        let translate = Vec2(tx, ty);
        let euler = params.vec3_or("rotation", [0.0, 0.0, 0.0]);
        let rotation = euler[2].to_radians();
        let [sx, sy, sz] = params.vec3_or("scale", [1.0, 1.0, 1.0]);
        let scale = Vec2(sx, sy);
        // Only the components a domain actually carries can do anything, so
        // "identity" is a wider condition for a 3D geometry than a 2D one.
        let spatial = has_spatial_positions(geometry)?;

        let planar_identity =
            translate == Vec2(0.0, 0.0) && rotation == 0.0 && scale == Vec2(1.0, 1.0);
        let identity = planar_identity
            && (!spatial || (tz == 0.0 && euler[0] == 0.0 && euler[1] == 0.0 && sz == 1.0));
        if identity {
            // Identity: share the input wholesale.
            return Ok(inputs[0].as_ref().expect("checked above").clone());
        }

        let pivot3 = if params.bool_or("use_centroid", true) {
            bounds_center(geometry).unwrap_or(Vec3(0.0, 0.0, 0.0))
        } else {
            let [px, py, pz] = params.vec3_or("pivot", [0.0, 0.0, 0.0]);
            Vec3(px, py, pz)
        };
        let pivot = Vec2(pivot3.0, pivot3.1);

        let (sin_r, cos_r) = rotation.sin_cos();
        let apply = |p: Vec2| -> Vec2 {
            let local = Vec2((p.0 - pivot.0) * scale.0, (p.1 - pivot.1) * scale.1);
            Vec2(
                pivot.0 + translate.0 + cos_r * local.0 - sin_r * local.1,
                pivot.1 + translate.1 + sin_r * local.0 + cos_r * local.1,
            )
        };
        let (sin_x, cos_x) = euler[0].to_radians().sin_cos();
        let (sin_y, cos_y) = euler[1].to_radians().sin_cos();
        let apply3 = |p: Vec3| -> Vec3 {
            let local = Vec3(
                (p.0 - pivot3.0) * scale.0,
                (p.1 - pivot3.1) * scale.1,
                (p.2 - pivot3.2) * sz,
            );
            // ZYX intrinsic: Z first, then Y, then X.
            let z = Vec3(
                cos_r * local.0 - sin_r * local.1,
                sin_r * local.0 + cos_r * local.1,
                local.2,
            );
            let y = Vec3(cos_y * z.0 + sin_y * z.2, z.1, -sin_y * z.0 + cos_y * z.2);
            Vec3(
                pivot3.0 + tx + y.0,
                pivot3.1 + ty + cos_x * y.1 - sin_x * y.2,
                pivot3.2 + tz + sin_x * y.1 + cos_x * y.2,
            )
        };

        let mut out = geometry.clone();
        if out.points().get(names::P).is_some() {
            transform_positions(out.points_mut(), &apply, &apply3)?;
        }
        if out.detail().get(names::ANCHOR).is_some() {
            for anchor in out
                .detail_mut()
                .make_mut(names::ANCHOR)?
                .as_vec2_mut(names::ANCHOR)?
            {
                *anchor = apply(*anchor);
            }
        }
        if out.instance_count() > 0 {
            if out.instances().get(names::P).is_some() {
                transform_positions(out.instances_mut(), &apply, &apply3)?;
            }
            // Valid instance geometry may omit rot/scale — consumers
            // default them to 0 / (1,1) — so materialize the column from
            // its implicit default before composing.
            let count = out.instance_count();
            if rotation != 0.0 {
                if out.instances().get(names::ROT).is_none() {
                    out.instances_mut()
                        .insert(names::ROT, AttributeArray::F32(vec![0.0; count]))?;
                }
                for r in out
                    .instances_mut()
                    .make_mut(names::ROT)?
                    .as_f32_mut(names::ROT)?
                {
                    *r += rotation;
                }
            }
            if scale != Vec2(1.0, 1.0) {
                if out.instances().get(names::SCALE).is_none() {
                    out.instances_mut().insert(
                        names::SCALE,
                        AttributeArray::Vec2(vec![Vec2(1.0, 1.0); count]),
                    )?;
                }
                for s in out
                    .instances_mut()
                    .make_mut(names::SCALE)?
                    .as_vec2_mut(names::SCALE)?
                {
                    *s = Vec2(s.0 * scale.0, s.1 * scale.1);
                }
            }
        }
        Ok(Arc::new(out))
    }
}

/// Whether any positional domain of `geometry` carries three-dimensional `P`.
fn has_spatial_positions(geometry: &Geometry) -> anyhow::Result<bool> {
    for domain in [Domain::Point, Domain::Instance] {
        if let Some(positions) = geometry.positions(domain)
            && positions
                .context("geometry.transform: P is not a position column")?
                .dimension()
                == 3
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Rewrites a domain's `P` with the transform of its own dimension.
fn transform_positions(
    attributes: &mut AttributeSet,
    apply: &impl Fn(Vec2) -> Vec2,
    apply3: &impl Fn(Vec3) -> Vec3,
) -> anyhow::Result<()> {
    match attributes.make_mut(names::P)? {
        AttributeArray::Vec2(values) => values.iter_mut().for_each(|p| *p = apply(*p)),
        AttributeArray::Vec3(values) => values.iter_mut().for_each(|p| *p = apply3(*p)),
        other => anyhow::bail!(
            "geometry.transform: P is {}, expected Vec2 or Vec3",
            other.attr_type()
        ),
    }
    Ok(())
}

/// `geometry.merge`: concatenates two geometries.
///
/// Points, primitives (vertex ranges re-based onto the combined point
/// list), and instances are appended A-then-B. Attribute columns are the
/// **union** of both sides; a column missing on one side is filled with
/// the typed zero for that side's rows (Houdini semantics). A same-name
/// type conflict is an error. Detail attributes take A wholesale (B's
/// detail only when A has none), and merging two distinct instance
/// sources is unsupported. An unconnected or empty input passes the
/// other side through.
pub struct GeometryMergeProcessor;

impl GeometryMergeProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for GeometryMergeProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let slot = |index: usize| -> Option<&Geometry> {
            inputs
                .get(index)
                .and_then(|input| input.as_ref())
                .and_then(|input| input.downcast_ref::<Geometry>())
        };
        let (a, b) = (slot(0), slot(1));
        let is_empty = |g: &Geometry| {
            g.point_count() == 0
                && g.primitive_count() == 0
                && g.instance_count() == 0
                // A detail-only side still contributes to the merge.
                && g.detail().element_count() == 0
        };
        match (a, b) {
            (None, None) => return Ok(Arc::new(Geometry::new())),
            // One side missing or empty: share the other input wholesale.
            (Some(_), None) | (Some(_), Some(_)) if b.is_none_or(is_empty) => {
                return Ok(inputs[0].as_ref().expect("a present").clone());
            }
            (None, Some(_)) | (Some(_), Some(_)) if a.is_none_or(is_empty) => {
                return Ok(inputs[1].as_ref().expect("b present").clone());
            }
            _ => {}
        }
        let (a, b) = (a.expect("checked"), b.expect("checked"));

        let mut out = Geometry::new();
        // Fill lengths come from the domain's element count, not the
        // attribute set's column length — a side may have primitives (or
        // points/instances) without any attribute columns on that domain.
        *out.points_mut() = concat_attribute_sets(
            a.points(),
            b.points(),
            (a.point_count(), b.point_count()),
            Domain::Point,
        )?;
        *out.primitive_attrs_mut() = concat_attribute_sets(
            a.primitive_attrs(),
            b.primitive_attrs(),
            (a.primitive_count(), b.primitive_count()),
            Domain::Primitive,
        )?;
        *out.instances_mut() = concat_attribute_sets(
            a.instances(),
            b.instances(),
            (a.instance_count(), b.instance_count()),
            Domain::Instance,
        )?;
        // Detail is not a concatenable domain: A wins wholesale.
        *out.detail_mut() = if a.detail().element_count() > 0 {
            a.detail().clone()
        } else {
            b.detail().clone()
        };

        // Primitives are kind-agnostic here: both variants relocate by the
        // same two offsets. A's index blob has to land first so its ranges
        // stay correct unshifted, and B's shifts by however long A's was.
        let point_offset = a.point_count();
        let a_indices = out.extend_indices(a.indices());
        for prim in a.primitives() {
            out.push_primitive(prim.shifted(0, a_indices));
        }
        let b_indices = out.extend_indices(b.indices());
        for prim in b.primitives() {
            out.push_primitive(prim.shifted(point_offset, b_indices));
        }

        // Sources are moved, never inspected: an image source merges by the
        // same rule a geometry one does.
        match (a.sources(), b.sources()) {
            (sources_a, sources_b)
                if !sources_a.is_empty()
                    && !sources_b.is_empty()
                    && (sources_a.len() != sources_b.len()
                        || sources_a
                            .iter()
                            .zip(sources_b)
                            .any(|(source_a, source_b)| !source_a.ptr_eq(source_b))) =>
            {
                anyhow::bail!(
                    "geometry.merge: merging two distinct instance sources is unsupported"
                )
            }
            (sources_a, sources_b) => {
                out.set_sources(if sources_a.is_empty() {
                    sources_b.to_vec()
                } else {
                    sources_a.to_vec()
                });
            }
        }
        Ok(Arc::new(out))
    }
}

/// Concatenates the union of both sides' columns; rows missing on one side
/// are filled with that column type's zero value.
fn concat_attribute_sets(
    a: &AttributeSet,
    b: &AttributeSet,
    (len_a, len_b): (usize, usize),
    domain: Domain,
) -> anyhow::Result<AttributeSet> {
    let mut out = AttributeSet::new();
    let names: Vec<&str> = a
        .iter()
        .map(|(name, _)| name.as_str())
        .chain(
            b.iter()
                .filter(|(name, _)| a.get(name).is_none())
                .map(|(name, _)| name.as_str()),
        )
        .collect();
    for name in names {
        let column = match (a.get(name), b.get(name)) {
            (Some(ca), Some(cb)) if ca.attr_type() != cb.attr_type() => anyhow::bail!(
                "geometry.merge: {domain:?} attribute {name:?} type mismatch ({} vs {})",
                ca.attr_type(),
                cb.attr_type()
            ),
            (ca, cb) => {
                let proto = ca.or(cb).expect("name came from one side");
                concat_columns(
                    ca.map(Arc::as_ref),
                    cb.map(Arc::as_ref),
                    proto,
                    len_a,
                    len_b,
                )
            }
        };
        out.insert(name.to_owned(), column)?;
    }
    Ok(out)
}

/// `a ++ b` with typed-zero fill for a missing side.
fn concat_columns(
    a: Option<&AttributeArray>,
    b: Option<&AttributeArray>,
    proto: &AttributeArray,
    len_a: usize,
    len_b: usize,
) -> AttributeArray {
    macro_rules! concat_as {
        ($variant:ident, $zero:expr) => {{
            let mut merged = match a {
                Some(AttributeArray::$variant(v)) => v.clone(),
                _ => vec![$zero; len_a],
            };
            match b {
                Some(AttributeArray::$variant(v)) => merged.extend(v.iter().cloned()),
                _ => merged.extend(std::iter::repeat_n($zero, len_b)),
            }
            AttributeArray::$variant(merged)
        }};
    }
    match proto {
        AttributeArray::F32(_) => concat_as!(F32, 0.0),
        AttributeArray::Vec2(_) => concat_as!(Vec2, Vec2(0.0, 0.0)),
        AttributeArray::Vec3(_) => concat_as!(Vec3, Vec3(0.0, 0.0, 0.0)),
        AttributeArray::Vec4(_) => concat_as!(Vec4, Vec4(0.0, 0.0, 0.0, 0.0)),
        AttributeArray::Color(_) => concat_as!(Color, Color::TRANSPARENT),
        AttributeArray::I32(_) => concat_as!(I32, 0),
        AttributeArray::Bool(_) => concat_as!(Bool, false),
        AttributeArray::Str(_) => concat_as!(Str, String::new()),
    }
}

/// `geometry.connect`: run one path through the points without adding any.
///
/// `mode` picks the points and their order (`order` / `nearest` / `group`),
/// `interpolation` decides whether the path is straight or gets Catmull-Rom
/// `in_tan` / `out_tan` written for `rasterize` to curve.
pub struct GeometryConnectProcessor;

impl GeometryConnectProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for GeometryConnectProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let geometry = geometry_input(inputs, 0, "geometry.connect")?;
        let group = params.str_or("group", "");
        let mode = match params.str_or("mode", "order") {
            "nearest" => ConnectMode::Nearest,
            "group" => ConnectMode::Group(group),
            _ => ConnectMode::Order,
        };
        let interpolation = match params.str_or("interpolation", "linear") {
            "bezier" => ConnectInterpolation::Bezier,
            _ => ConnectInterpolation::Linear,
        };
        Ok(Arc::new(connect(
            geometry,
            mode,
            interpolation,
            params.bool_or("closed", false),
        )?))
    }
}

/// `geometry.from_image`: wrap a frame buffer as one instance that stamps it.
///
/// The output is the smallest geometry that draws the picture once: a single
/// instance at the origin whose only source is the image. Point and primitive
/// domains stay empty — an image is not a shape, and giving it a placeholder
/// rectangle of points would make every downstream operator act on vertices
/// that mean nothing.
///
/// Feeding the result to `scatter.*` nests it one level deep and repeats the
/// picture; that is the whole point (`REQ-MOGRAPH-001`, decision 3 of
/// `docs/implementation/done/image-instancing-plan.md`).
///
/// The frame is wrapped in whichever representation it arrived in, CPU or
/// GPU-resident: no conversion, and therefore no readback (decision 6). The
/// rectangle is the source's own pixel resolution centred on the origin, so
/// the aspect ratio holds by construction and **scaling a copy up makes it
/// blurry** — the image is not re-evaluated at the copy's resolution
/// (decisions 1 and 5).
pub struct GeometryFromImageProcessor;

impl GeometryFromImageProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for GeometryFromImageProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let frame = inputs
            .first()
            .and_then(|input| input.as_ref())
            .context("geometry.from_image: input 0 is not connected")?;
        let (width, height) = crate::gpu_util::frame_size(frame.as_ref()).with_context(|| {
            format!(
                "geometry.from_image: the input must be a frame buffer, but its data type is {}",
                frame.data_type_id().raw()
            )
        })?;

        let mut geo = Geometry::new();
        geo.instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(0.0, 0.0)]))?;
        geo.instances_mut()
            .insert(names::INDEX, AttributeArray::I32(vec![0]))?;
        geo.set_sources(vec![InstanceSource::Image(InstanceImage::new(
            Arc::clone(frame),
            width,
            height,
        )?)]);
        geo.validate()?;
        Ok(Arc::new(geo))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::Evaluator;
    use ravel_core::geometry::Primitive;
    use ravel_core::graph::{Graph, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::types::{FrameBuffer, FrameRate};

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (64, 64))
    }

    /// Two points around (2, 0)–(4, 0); bbox center (3, 0).
    fn source_geometry() -> Geometry {
        Geometry::from_points(vec![Vec2(2.0, 0.0), Vec2(4.0, 0.0)])
    }

    /// Source node that always emits the given geometry `Arc`.
    struct Fixed(Arc<Geometry>);
    impl NodeProcessor for Fixed {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(self.0.clone())
        }
    }

    // -----------------------------------------------------------------------
    // geometry.from_image
    // -----------------------------------------------------------------------

    /// Run `geometry.from_image` on one already-evaluated frame value.
    fn from_image(frame: Option<Arc<dyn NodeData>>) -> anyhow::Result<Arc<dyn NodeData>> {
        let node = Node::new(NodeId::new(1), "geometry.from_image")
            .with_input("image", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::GEOMETRY);
        let mut scope = Evaluator::new();
        GeometryFromImageProcessor.process(
            &node,
            &ctx(),
            &[frame],
            &ResolvedParams::default(),
            &mut scope,
        )
    }

    fn as_geometry(value: &Arc<dyn NodeData>) -> &Geometry {
        value
            .downcast_ref::<Geometry>()
            .expect("output is Geometry")
    }

    /// The output shape: one instance at the origin whose only source is the
    /// image, and nothing in the point or primitive domains.
    #[test]
    fn from_image_outputs_one_instance_stamping_the_image() {
        let out = from_image(Some(Arc::new(FrameBuffer::new_zeroed(320, 180)))).unwrap();
        let geo = as_geometry(&out);

        assert_eq!(geo.instance_count(), 1);
        assert_eq!(geo.point_count(), 0, "an image is not a shape");
        assert_eq!(geo.primitive_count(), 0);
        assert_eq!(geo.validate(), Ok(()), "the instance domain owes a P");

        let positions = geo
            .instances()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap();
        assert_eq!(positions, &[Vec2(0.0, 0.0)]);
        assert_eq!(
            geo.instances()
                .get(names::INDEX)
                .unwrap()
                .as_i32(names::INDEX),
            Ok(&[0][..])
        );

        assert_eq!(geo.sources().len(), 1);
        let image = geo.sources()[0].image().expect("the source is an image");
        assert_eq!((image.width(), image.height()), (320, 180));
        // The rectangle is the source's pixel size, centred on the origin.
        let rect = image.rect();
        assert_eq!((rect.x, rect.y), (-160.0, -90.0));
        assert_eq!((rect.width, rect.height), (320.0, 180.0));
    }

    /// The frame arrives and leaves as the very same value: no copy, no
    /// conversion, and therefore no readback whichever representation it is
    /// in.
    #[test]
    fn from_image_wraps_a_cpu_frame_without_converting_it() {
        let frame: Arc<dyn NodeData> = Arc::new(FrameBuffer::new_zeroed(16, 8));
        let out = from_image(Some(Arc::clone(&frame))).unwrap();
        let held = as_geometry(&out).sources()[0]
            .image()
            .expect("the source is an image")
            .frame()
            .clone();
        assert!(
            Arc::ptr_eq(&held, &frame),
            "the frame must be held as it arrived"
        );
        assert!(!as_geometry(&out).is_gpu_resident());
    }

    /// The GPU half of the same claim: a resident frame goes in, the identical
    /// handle comes out, and no transfer is recorded. Skipped without an
    /// adapter.
    #[test]
    fn from_image_wraps_a_gpu_frame_without_reading_it_back() {
        let Ok(gpu) = ravel_gpu::GpuContext::new_blocking() else {
            eprintln!("skipping: no GPU adapter available");
            return;
        };
        let pool = crate::shared_texture_pool(&gpu);
        let resident = ravel_gpu::GpuFrameBuffer::from_frame_buffer(
            gpu.clone(),
            &pool,
            &FrameBuffer::new_zeroed(16, 8),
        )
        .expect("upload");
        let frame: Arc<dyn NodeData> = Arc::new(resident);

        let before = gpu.transfer_stats();
        let out = from_image(Some(Arc::clone(&frame))).unwrap();
        let delta = before.delta(&gpu.transfer_stats());
        assert_eq!(delta.readbacks, 0, "wrapping must not read back: {delta:?}");

        let geo = as_geometry(&out);
        let image = geo.sources()[0].image().expect("the source is an image");
        assert_eq!((image.width(), image.height()), (16, 8));
        assert!(
            Arc::ptr_eq(image.frame(), &frame),
            "the resident handle must be held as it arrived"
        );
        assert!(
            geo.is_gpu_resident(),
            "a geometry holding a resident frame is resident"
        );
    }

    /// Feeding the output to a scatter puts the image two levels down, well
    /// inside the rasterizer's nesting limit — which is what makes "repeat an
    /// image" work with no new mechanism.
    #[test]
    fn a_scattered_image_geometry_nests_two_levels_deep() {
        let image = from_image(Some(Arc::new(FrameBuffer::new_zeroed(32, 32)))).unwrap();
        let node = Node::new(NodeId::new(2), "scatter.grid")
            .with_input("instance_source", &[DataTypeId::GEOMETRY])
            .with_output("output", DataTypeId::GEOMETRY);
        let mut params = ResolvedParams::default();
        params.set("count_x", ravel_core::eval::ResolvedValue::Int(3));
        params.set("count_y", ravel_core::eval::ResolvedValue::Int(2));
        let mut scope = Evaluator::new();
        let out = crate::scatter::GridProcessor
            .process(&node, &ctx(), &[Some(image)], &params, &mut scope)
            .unwrap();

        let scattered = as_geometry(&out);
        assert_eq!(scattered.instance_count(), 6, "3 x 2 copies");

        // Walk down to the image, counting the levels the rasterizer would
        // recurse through.
        let mut depth = 1;
        let mut level = scattered.sources()[0]
            .geometry()
            .expect("the scatter stamps the from_image geometry")
            .clone();
        while let Some(inner) = level.sources().first().and_then(|s| s.geometry()) {
            depth += 1;
            level = inner.clone();
        }
        depth += 1;
        assert!(
            level.sources()[0].image().is_some(),
            "the bottom of the nesting is the image"
        );
        assert_eq!(depth, 2, "from_image → scatter is two levels");
        assert!(
            depth < crate::rasterize::MAX_INSTANCE_DEPTH,
            "two levels stay inside the rasterizer's limit"
        );
    }

    #[test]
    fn from_image_rejects_a_value_that_is_not_a_frame_buffer() {
        let Err(error) = from_image(Some(Arc::new(source_geometry()))) else {
            panic!("a geometry is not a frame buffer");
        };
        assert!(
            format!("{error:#}").contains("must be a frame buffer"),
            "unexpected error: {error:#}"
        );

        let Err(error) = from_image(None) else {
            panic!("an unconnected input has no image");
        };
        assert!(
            format!("{error:#}").contains("not connected"),
            "unexpected error: {error:#}"
        );
    }

    fn eval_connect(params: &[(&str, ParameterValue)], geo: Arc<Geometry>) -> Geometry {
        let source =
            Node::new(NodeId::new(1), "test.source").with_output("out", DataTypeId::GEOMETRY);
        let mut node = Node::new(NodeId::new(2), "geometry.connect")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_output("geometry", DataTypeId::GEOMETRY);
        for (key, value) in params {
            node = node.with_param(*key, value.clone());
        }
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(node)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(Fixed(geo)));
        ev.register(NodeId::new(2), Arc::new(GeometryConnectProcessor));
        let output = ev.evaluate(&graph, NodeId::new(2), &ctx()).unwrap();
        output.downcast_ref::<Geometry>().unwrap().clone()
    }

    /// `bezier` has to reach the renderer as a curve, not just as two extra
    /// attribute columns: this flattens the result with
    /// [`crate::flatten::flatten_path`], the one function both the CPU and
    /// GPU rasterize paths consume (`rasterize::path_polyline`).
    #[test]
    fn connect_bezier_reaches_rasterize_as_a_curve() {
        // An L: the straight version is exactly this control polygon, so any
        // vertex off it comes from the tangents.
        let corner = Arc::new(Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(10.0, 0.0),
            Vec2(10.0, 10.0),
        ]));
        let flattened = |geometry: &Geometry| {
            let positions = geometry
                .points()
                .get(names::P)
                .unwrap()
                .as_vec2(names::P)
                .unwrap()
                .to_vec();
            let column = |name: &str| {
                geometry
                    .points()
                    .get(name)
                    .map(|c| c.as_vec2(name).unwrap().to_vec())
            };
            crate::flatten::flatten_path(
                &positions,
                column(names::IN_TAN).as_deref(),
                column(names::OUT_TAN).as_deref(),
                false,
            )
        };

        let straight = eval_connect(
            &[("interpolation", ParameterValue::String("linear".into()))],
            corner.clone(),
        );
        assert_eq!(
            flattened(&straight),
            [Vec2(0.0, 0.0), Vec2(10.0, 0.0), Vec2(10.0, 10.0)],
            "linear stays on the control polygon"
        );

        let curved = eval_connect(
            &[("interpolation", ParameterValue::String("bezier".into()))],
            corner,
        );
        let polyline = flattened(&curved);
        assert!(
            polyline.len() > 3,
            "a curved segment subdivides: {polyline:?}"
        );
        assert!(
            polyline.iter().any(|point| point.1 < -0.1),
            "the curve leaves the straight chord: {polyline:?}"
        );
    }

    #[test]
    fn connect_processor_defaults_to_one_open_path_in_index_order() {
        let cloud = Arc::new(Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(10.0, 0.0),
            Vec2(5.0, 9.0),
        ]));
        let wired = eval_connect(&[], cloud);
        assert_eq!(wired.primitive_count(), 1);
        assert!(matches!(
            wired.primitives()[0],
            Primitive::Path {
                verts: std::ops::Range { start: 0, end: 3 },
                closed: false,
            }
        ));
    }

    fn eval_transform(params: &[(&str, ParameterValue)], geo: Arc<Geometry>) -> Arc<dyn NodeData> {
        let source =
            Node::new(NodeId::new(1), "test.source").with_output("output", DataTypeId::GEOMETRY);
        let mut node = Node::new(NodeId::new(2), "geometry.transform")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_output("output", DataTypeId::GEOMETRY);
        for (key, value) in params {
            node = node.with_param(*key, value.clone());
        }
        let graph = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(node)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(2),
                InputPortIndex(0),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(Fixed(geo)));
        ev.register(NodeId::new(2), Arc::new(GeometryTransformProcessor));
        ev.evaluate(&graph, NodeId::new(2), &ctx()).unwrap()
    }

    fn transformed(params: &[(&str, ParameterValue)], geo: Geometry) -> Geometry {
        eval_transform(params, Arc::new(geo))
            .downcast_ref::<Geometry>()
            .unwrap()
            .clone()
    }

    fn point_positions(geo: &Geometry) -> Vec<Vec2> {
        geo.points()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap()
            .to_vec()
    }

    fn anchor(geometry: &Geometry) -> Vec2 {
        geometry
            .detail()
            .get(names::ANCHOR)
            .expect("anchor present")
            .as_vec2(names::ANCHOR)
            .unwrap()[0]
    }

    #[test]
    fn translate_moves_points() {
        let out = transformed(
            &[("translate", ParameterValue::vec3(10.0, -5.0, 0.0))],
            source_geometry(),
        );
        assert_eq!(
            point_positions(&out),
            vec![Vec2(12.0, -5.0), Vec2(14.0, -5.0)]
        );
    }

    #[test]
    fn rotation_uses_degrees_around_the_centroid() {
        // 90° around bbox center (3, 0): (2,0)→(3,-1), (4,0)→(3,1).
        let out = transformed(
            &[("rotation", ParameterValue::vec3(0.0, 0.0, 90.0))],
            source_geometry(),
        );
        let pos = point_positions(&out);
        assert!((pos[0].0 - 3.0).abs() < 1e-5 && (pos[0].1 + 1.0).abs() < 1e-5);
        assert!((pos[1].0 - 3.0).abs() < 1e-5 && (pos[1].1 - 1.0).abs() < 1e-5);
    }

    /// `rotation` is a `Channel3` of Euler degrees whose Z component is the
    /// former scalar. The 2D result must be bit-identical to rotating by that
    /// scalar, and the X / Y components must not perturb it.
    #[test]
    fn euler_rotation_z_reproduces_the_scalar_rotation_bit_for_bit() {
        let degrees = 37.5f32;
        let reference = {
            // The pre-fold arithmetic: one `to_radians`, one `sin_cos`,
            // rotation about the bbox center (3, 0).
            let (sin_r, cos_r) = degrees.to_radians().sin_cos();
            let pivot = Vec2(3.0, 0.0);
            source_geometry()
                .points()
                .get(names::P)
                .unwrap()
                .as_vec2(names::P)
                .unwrap()
                .iter()
                .map(|p| {
                    let local = Vec2(p.0 - pivot.0, p.1 - pivot.1);
                    Vec2(
                        pivot.0 + cos_r * local.0 - sin_r * local.1,
                        pivot.1 + sin_r * local.0 + cos_r * local.1,
                    )
                })
                .collect::<Vec<_>>()
        };
        let folded = transformed(
            &[("rotation", ParameterValue::vec3(0.0, 0.0, degrees))],
            source_geometry(),
        );
        assert_eq!(point_positions(&folded), reference);
        // X and Y Euler components are inert in the 2D pipeline.
        let with_xy = transformed(
            &[("rotation", ParameterValue::vec3(11.0, -22.0, degrees))],
            source_geometry(),
        );
        assert_eq!(point_positions(&with_xy), reference);
    }

    /// The Z defaults of the folded `Channel3` parameters are inert: a
    /// translate of 0, a scale of 1 and a rotation of (0, 0, 0) still take
    /// the identity fast path.
    #[test]
    fn channel3_z_defaults_keep_the_identity_fast_path() {
        let input = Arc::new(source_geometry());
        let out = eval_transform(
            &[
                ("translate", ParameterValue::vec3(0.0, 0.0, 0.0)),
                ("scale", ParameterValue::vec3(1.0, 1.0, 1.0)),
                ("rotation", ParameterValue::vec3(0.0, 0.0, 0.0)),
            ],
            input.clone(),
        );
        assert_eq!(
            point_positions(out.downcast_ref::<Geometry>().unwrap()),
            point_positions(&input),
            "the Z defaults leave the geometry untouched"
        );
    }

    fn point_positions3(geo: &Geometry) -> Vec<Vec3> {
        geo.points()
            .get(names::P)
            .unwrap()
            .as_vec3(names::P)
            .unwrap()
            .to_vec()
    }

    /// Two points around (2, 0, 1)–(4, 0, 3); bbox center (3, 0, 2).
    fn spatial_geometry() -> Geometry {
        Geometry::from_points3(vec![Vec3(2.0, 0.0, 1.0), Vec3(4.0, 0.0, 3.0)])
    }

    #[test]
    fn translate_and_scale_use_every_component_of_a_three_dimensional_p() {
        let out = transformed(
            &[
                ("use_centroid", ParameterValue::Bool(false)),
                ("translate", ParameterValue::vec3(10.0, -5.0, 7.0)),
                ("scale", ParameterValue::vec3(1.0, 1.0, 2.0)),
            ],
            spatial_geometry(),
        );
        assert_eq!(
            point_positions3(&out),
            vec![Vec3(12.0, -5.0, 9.0), Vec3(14.0, -5.0, 13.0)],
            "z is scaled and translated like x and y"
        );
        assert_eq!(out.validate(), Ok(()));
    }

    /// The spec fixes the Euler order at ZYX (Z applied first). A 90° Y
    /// rotation of the unit x axis has to land on -z, which only holds for
    /// that handedness.
    #[test]
    fn euler_rotation_follows_the_fixed_zyx_order() {
        let unit_axes = || {
            Geometry::from_points3(vec![
                Vec3(1.0, 0.0, 0.0),
                Vec3(0.0, 1.0, 0.0),
                Vec3(0.0, 0.0, 1.0),
            ])
        };
        let close = |actual: Vec3, expected: Vec3| {
            assert!(
                (actual.0 - expected.0).abs() < 1e-6
                    && (actual.1 - expected.1).abs() < 1e-6
                    && (actual.2 - expected.2).abs() < 1e-6,
                "{actual:?} != {expected:?}"
            );
        };
        let rotated = |degrees: [f32; 3]| {
            transformed(
                &[
                    ("use_centroid", ParameterValue::Bool(false)),
                    (
                        "rotation",
                        ParameterValue::vec3(degrees[0], degrees[1], degrees[2]),
                    ),
                ],
                unit_axes(),
            )
        };

        let about_x = point_positions3(&rotated([90.0, 0.0, 0.0]));
        close(about_x[1], Vec3(0.0, 0.0, 1.0));
        close(about_x[2], Vec3(0.0, -1.0, 0.0));

        let about_y = point_positions3(&rotated([0.0, 90.0, 0.0]));
        close(about_y[0], Vec3(0.0, 0.0, -1.0));
        close(about_y[2], Vec3(1.0, 0.0, 0.0));

        let about_z = point_positions3(&rotated([0.0, 0.0, 90.0]));
        close(about_z[0], Vec3(0.0, 1.0, 0.0));
        close(about_z[1], Vec3(-1.0, 0.0, 0.0));

        // Z then Y: x → y (by Z) → still y (Y leaves y alone).
        let zy = point_positions3(&rotated([0.0, 90.0, 90.0]));
        close(zy[0], Vec3(0.0, 1.0, 0.0));
        // z → z (by Z) → x (by Y): the order would swap this if X ran first.
        close(zy[2], Vec3(1.0, 0.0, 0.0));
    }

    /// A `Vec2` column has no third component to act on, so the extra channels
    /// stay inert exactly as they were before 3D positions existed — including
    /// the identity fast path.
    #[test]
    fn two_dimensional_positions_ignore_the_spatial_channels() {
        let input = Arc::new(source_geometry());
        let spatial_only = [
            ("translate", ParameterValue::vec3(0.0, 0.0, 9.0)),
            ("rotation", ParameterValue::vec3(30.0, 45.0, 0.0)),
            ("scale", ParameterValue::vec3(1.0, 1.0, 5.0)),
        ];
        let out = eval_transform(&spatial_only, input.clone());
        assert!(
            std::ptr::eq(out.downcast_ref::<Geometry>().unwrap(), input.as_ref()),
            "z-only channels are still the identity for a 2D geometry"
        );

        // And with a real 2D transform on top, the result is the 2D one.
        let mut combined = spatial_only.to_vec();
        combined[0] = ("translate", ParameterValue::vec3(10.0, -5.0, 9.0));
        assert_eq!(
            point_positions(&transformed(&combined, source_geometry())),
            point_positions(&transformed(
                &[("translate", ParameterValue::vec3(10.0, -5.0, 0.0))],
                source_geometry()
            ))
        );
    }

    /// The same channels are *not* inert once the geometry is 3D, so the
    /// identity fast path has to widen with the dimension.
    #[test]
    fn spatial_channels_are_not_the_identity_for_a_three_dimensional_p() {
        let input = Arc::new(spatial_geometry());
        let out = eval_transform(
            &[("translate", ParameterValue::vec3(0.0, 0.0, 9.0))],
            input.clone(),
        );
        assert!(!std::ptr::eq(
            out.downcast_ref::<Geometry>().unwrap(),
            input.as_ref()
        ));
        assert_eq!(
            point_positions3(out.downcast_ref::<Geometry>().unwrap()),
            vec![Vec3(2.0, 0.0, 10.0), Vec3(4.0, 0.0, 12.0)]
        );
    }

    /// A 2D instance source placed by 3D instances: each domain is transformed
    /// at its own dimension, and the 2D-only `rot` / `scale` still compose.
    #[test]
    fn instance_placement_transforms_at_its_own_dimension() {
        let mut geo = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(2.0, 0.0)]);
        geo.instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec3(vec![Vec3(2.0, 0.0, 5.0), Vec3(-2.0, 0.0, -5.0)]),
            )
            .unwrap();

        let out = transformed(
            &[
                ("use_centroid", ParameterValue::Bool(false)),
                ("translate", ParameterValue::vec3(0.0, 0.0, 1.0)),
                ("scale", ParameterValue::vec3(1.0, 1.0, 2.0)),
            ],
            geo,
        );
        assert_eq!(
            out.instances()
                .get(names::P)
                .unwrap()
                .as_vec3(names::P)
                .unwrap(),
            &[Vec3(2.0, 0.0, 11.0), Vec3(-2.0, 0.0, -9.0)]
        );
        assert_eq!(
            point_positions(&out),
            vec![Vec2(0.0, 0.0), Vec2(2.0, 0.0)],
            "the 2D point domain is untouched by the z-only transform"
        );
        assert_eq!(out.validate(), Ok(()));
    }

    #[test]
    fn scale_applies_before_rotation_around_an_explicit_pivot() {
        // Pivot (0,0), scale x2, rotate 90°, translate (1,0):
        // (2,0) → scale (4,0) → rotate (0,4) → translate (1,4).
        let out = transformed(
            &[
                ("use_centroid", ParameterValue::Bool(false)),
                ("scale", ParameterValue::vec3(2.0, 2.0, 1.0)),
                ("rotation", ParameterValue::vec3(0.0, 0.0, 90.0)),
                ("translate", ParameterValue::vec3(1.0, 0.0, 0.0)),
            ],
            source_geometry(),
        );
        let pos = point_positions(&out);
        assert!((pos[0].0 - 1.0).abs() < 1e-5 && (pos[0].1 - 4.0).abs() < 1e-5);
        assert!((pos[1].0 - 1.0).abs() < 1e-5 && (pos[1].1 - 8.0).abs() < 1e-5);
    }

    #[test]
    fn translate_rotate_and_scale_transform_the_anchor_like_a_point() {
        let mut geometry = source_geometry();
        geometry
            .detail_mut()
            .insert(names::ANCHOR, AttributeArray::Vec2(vec![Vec2(2.0, 1.0)]))
            .unwrap();
        let out = transformed(
            &[
                ("use_centroid", ParameterValue::Bool(false)),
                ("scale", ParameterValue::vec3(2.0, 3.0, 1.0)),
                ("rotation", ParameterValue::vec3(0.0, 0.0, 90.0)),
                ("translate", ParameterValue::vec3(2.0, -1.0, 0.0)),
            ],
            geometry,
        );
        let transformed_anchor = anchor(&out);
        assert!((transformed_anchor.0 + 1.0).abs() < 1e-5);
        assert!((transformed_anchor.1 - 3.0).abs() < 1e-5);
    }

    #[test]
    fn instances_compose_placement_rotation_and_scale() {
        let mut geo = Geometry::new();
        geo.instances_mut()
            .insert(names::INDEX, AttributeArray::I32(vec![0]))
            .unwrap();
        geo.instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(2.0, 0.0)]))
            .unwrap();
        geo.instances_mut()
            .insert(names::ROT, AttributeArray::F32(vec![0.5]))
            .unwrap();
        geo.instances_mut()
            .insert(names::SCALE, AttributeArray::Vec2(vec![Vec2(2.0, 3.0)]))
            .unwrap();

        let out = transformed(
            &[
                ("use_centroid", ParameterValue::Bool(false)),
                ("rotation", ParameterValue::vec3(0.0, 0.0, 90.0)),
                ("scale", ParameterValue::vec3(2.0, 2.0, 1.0)),
            ],
            geo,
        );
        let p = out
            .instances()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap()[0];
        // (2,0) → scale (4,0) → rotate 90° → (0,4).
        assert!((p.0 - 0.0).abs() < 1e-5 && (p.1 - 4.0).abs() < 1e-5);
        let rot = out
            .instances()
            .get(names::ROT)
            .unwrap()
            .as_f32(names::ROT)
            .unwrap()[0];
        assert!((rot - (0.5 + std::f32::consts::FRAC_PI_2)).abs() < 1e-5);
        let scale = out
            .instances()
            .get(names::SCALE)
            .unwrap()
            .as_vec2(names::SCALE)
            .unwrap()[0];
        assert_eq!(scale, Vec2(4.0, 6.0));
    }

    #[test]
    fn identity_shares_the_input_arc() {
        let mut geometry = source_geometry();
        geometry
            .detail_mut()
            .insert(names::ANCHOR, AttributeArray::Vec2(vec![Vec2(3.0, 0.0)]))
            .unwrap();
        let input = Arc::new(geometry);
        let out = eval_transform(&[], input.clone());
        let out_geo = out.downcast_ref::<Geometry>().unwrap();
        assert!(
            std::ptr::eq(out_geo, input.as_ref()),
            "identity must pass the input Arc through untouched"
        );
    }

    /// Instances without rot/scale columns (valid — consumers default them
    /// to 0 / (1,1)) gain materialized columns so the composition reaches
    /// the nested instance source.
    #[test]
    fn instances_gain_missing_rot_and_scale_columns() {
        let mut geo = Geometry::new();
        geo.instances_mut()
            .insert(names::INDEX, AttributeArray::I32(vec![0, 1]))
            .unwrap();
        geo.instances_mut()
            .insert(
                names::P,
                AttributeArray::Vec2(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0)]),
            )
            .unwrap();

        let out = transformed(
            &[
                ("use_centroid", ParameterValue::Bool(false)),
                ("rotation", ParameterValue::vec3(0.0, 0.0, 90.0)),
                ("scale", ParameterValue::vec3(2.0, 3.0, 1.0)),
            ],
            geo,
        );
        let rot = out
            .instances()
            .get(names::ROT)
            .expect("rot column materialized")
            .as_f32(names::ROT)
            .unwrap()
            .to_vec();
        assert_eq!(rot.len(), 2);
        assert!((rot[0] - std::f32::consts::FRAC_PI_2).abs() < 1e-5);
        let scale = out
            .instances()
            .get(names::SCALE)
            .expect("scale column materialized")
            .as_vec2(names::SCALE)
            .unwrap()
            .to_vec();
        assert_eq!(scale, vec![Vec2(2.0, 3.0), Vec2(2.0, 3.0)]);
    }

    #[test]
    fn untouched_columns_keep_structural_sharing() {
        let mut geo = source_geometry();
        geo.points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![1.0, 2.0]))
            .unwrap();
        geo.detail_mut()
            .insert(names::ANCHOR, AttributeArray::Vec2(vec![Vec2(3.0, 0.0)]))
            .unwrap();
        geo.detail_mut()
            .insert("tag", AttributeArray::Str(vec!["source".to_owned()]))
            .unwrap();
        let input = Arc::new(geo);
        let out = transformed(
            &[("translate", ParameterValue::vec3(1.0, 0.0, 0.0))],
            (*input).clone(),
        );
        // P was rewritten; pscale still shares the input's column.
        let shared = Arc::ptr_eq(
            input.points().get(names::PSCALE).unwrap(),
            out.points().get(names::PSCALE).unwrap(),
        );
        assert!(shared, "pscale column must stay shared");
        assert!(
            !Arc::ptr_eq(
                input.points().get(names::P).unwrap(),
                out.points().get(names::P).unwrap(),
            ),
            "P column must be copied on write"
        );
        assert!(
            !Arc::ptr_eq(
                input.detail().get(names::ANCHOR).unwrap(),
                out.detail().get(names::ANCHOR).unwrap(),
            ),
            "anchor column must be copied on write"
        );
        assert!(
            Arc::ptr_eq(
                input.detail().get("tag").unwrap(),
                out.detail().get("tag").unwrap(),
            ),
            "untouched detail columns must stay shared"
        );
    }

    #[test]
    fn transform_does_not_materialize_a_missing_anchor() {
        let mut geometry = source_geometry();
        geometry
            .detail_mut()
            .insert("tag", AttributeArray::Str(vec!["source".to_owned()]))
            .unwrap();
        let input = geometry.clone();
        let out = transformed(
            &[("translate", ParameterValue::vec3(1.0, 0.0, 0.0))],
            geometry,
        );

        assert!(out.detail().get(names::ANCHOR).is_none());
        assert!(Arc::ptr_eq(
            input.detail().get("tag").unwrap(),
            out.detail().get("tag").unwrap(),
        ));
    }

    fn eval_merge(a: Option<Arc<Geometry>>, b: Option<Arc<Geometry>>) -> Arc<dyn NodeData> {
        let node = Node::new(NodeId::new(3), "geometry.merge")
            .with_input("A", &[DataTypeId::GEOMETRY])
            .with_input("B", &[DataTypeId::GEOMETRY])
            .with_output("output", DataTypeId::GEOMETRY);
        let mut graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(3), Arc::new(GeometryMergeProcessor));
        for (slot, geo) in [(0u32, a), (1u32, b)] {
            let Some(geo) = geo else { continue };
            let id = NodeId::new(10 + slot as u64);
            let source = Node::new(id, "test.source").with_output("output", DataTypeId::GEOMETRY);
            graph = graph
                .add_node(source)
                .unwrap()
                .add_edge(
                    EdgeId::new(20 + slot as u64),
                    id,
                    OutputPortIndex(0),
                    NodeId::new(3),
                    InputPortIndex(slot),
                )
                .unwrap();
            ev.register(id, Arc::new(Fixed(geo)));
        }
        ev.evaluate(&graph, NodeId::new(3), &ctx()).unwrap()
    }

    /// A `Vec3` `P` with `Primitive::Path` is a 3D polyline, one of the four
    /// combinations the dimension and the primitive kind produce. It has to
    /// survive construction, merging (which re-bases vertex ranges) and
    /// transformation without ever going through a 2D column.
    #[test]
    fn three_dimensional_polylines_merge_and_transform() {
        let leg = |z: f32| {
            let mut geo = Geometry::from_points3(vec![
                Vec3(0.0, 0.0, z),
                Vec3(4.0, 0.0, z),
                Vec3(4.0, 4.0, z),
            ]);
            geo.push_primitive(Primitive::Path {
                verts: 0..3,
                closed: false,
            });
            geo
        };
        assert_eq!(leg(1.0).validate(), Ok(()));

        let merged = eval_merge(Some(Arc::new(leg(1.0))), Some(Arc::new(leg(-1.0))));
        let merged = merged.downcast_ref::<Geometry>().unwrap();
        assert_eq!(merged.validate(), Ok(()));
        assert_eq!(merged.point_count(), 6);
        assert_eq!(
            merged.primitives()[1],
            Primitive::Path {
                verts: 3..6,
                closed: false
            },
            "the second path is re-based onto the combined point list"
        );
        assert_eq!(
            point_positions3(merged)
                .iter()
                .map(|p| p.2)
                .collect::<Vec<_>>(),
            vec![1.0, 1.0, 1.0, -1.0, -1.0, -1.0],
            "depth survives the merge"
        );

        let moved = transformed(
            &[
                ("use_centroid", ParameterValue::Bool(false)),
                ("translate", ParameterValue::vec3(0.0, 0.0, 10.0)),
            ],
            merged.clone(),
        );
        assert_eq!(
            point_positions3(&moved)
                .iter()
                .map(|p| p.2)
                .collect::<Vec<_>>(),
            vec![11.0, 11.0, 11.0, 9.0, 9.0, 9.0]
        );
        assert_eq!(moved.primitives(), merged.primitives());
    }

    /// Closed unit-square path with a `pscale` column A-side only.
    fn geo_a() -> Geometry {
        let mut geo = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(1.0, 0.0),
            Vec2(1.0, 1.0),
            Vec2(0.0, 1.0),
        ]);
        geo.push_primitive(Primitive::Path {
            verts: 0..4,
            closed: true,
        });
        geo.points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![1.0, 2.0, 3.0, 4.0]))
            .unwrap();
        geo
    }

    /// Open two-point path with a `Cd` column B-side only.
    fn geo_b() -> Geometry {
        let mut geo = Geometry::from_points(vec![Vec2(5.0, 5.0), Vec2(6.0, 5.0)]);
        geo.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        geo.points_mut()
            .insert(
                names::CD,
                AttributeArray::Vec3(vec![Vec3(1.0, 0.0, 0.0), Vec3(0.0, 1.0, 0.0)]),
            )
            .unwrap();
        geo
    }

    /// Merge is kind-agnostic. Both index blobs are concatenated and each
    /// mesh's two ranges move; the index *values* stay put because they are
    /// relative to `verts.start`, which is what makes the concatenation a
    /// copy rather than a remap.
    #[test]
    fn merge_rebases_meshes_and_concatenates_their_indices() {
        let mut a = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(1.0, 0.0),
            Vec2(1.0, 1.0),
            Vec2(0.0, 1.0),
        ]);
        a.push_mesh(0..4, &[0, 1, 2, 0, 2, 3]);

        let mut b = Geometry::from_points(vec![Vec2(5.0, 5.0), Vec2(6.0, 5.0), Vec2(6.0, 6.0)]);
        b.push_mesh(0..3, &[0, 1, 2]);

        let out = eval_merge(Some(Arc::new(a)), Some(Arc::new(b)));
        let geo = out.downcast_ref::<Geometry>().unwrap();

        assert_eq!(geo.validate(), Ok(()));
        assert_eq!(geo.point_count(), 7);
        assert_eq!(geo.primitive_count(), 2);
        assert_eq!(
            geo.indices(),
            &[0, 1, 2, 0, 2, 3, 0, 1, 2],
            "B's triangles are appended verbatim"
        );

        let Primitive::Mesh { verts, indices } = &geo.primitives()[1] else {
            panic!("B contributed a mesh");
        };
        assert_eq!(*verts, 4..7, "B's vertex range re-based past A's points");
        assert_eq!(*indices, 6..9, "B's index range re-based past A's indices");
        assert_eq!(geo.mesh_indices(&geo.primitives()[1]), Some(&[0, 1, 2][..]));
    }

    /// A path and a mesh in the same merge each relocate by their own offset
    /// without disturbing the other.
    #[test]
    fn merge_mixes_paths_and_meshes() {
        let mut b = Geometry::from_points(vec![Vec2(5.0, 5.0), Vec2(6.0, 5.0), Vec2(6.0, 6.0)]);
        b.push_mesh(0..3, &[0, 1, 2]);

        let out = eval_merge(Some(Arc::new(geo_a())), Some(Arc::new(b)));
        let geo = out.downcast_ref::<Geometry>().unwrap();

        assert_eq!(geo.validate(), Ok(()));
        assert!(matches!(
            &geo.primitives()[0],
            Primitive::Path { verts, closed: true } if *verts == (0..4)
        ));
        assert!(matches!(
            &geo.primitives()[1],
            Primitive::Mesh { verts, indices } if *verts == (4..7) && *indices == (0..3)
        ));
        assert_eq!(geo.indices(), &[0, 1, 2], "only B carried triangles");
    }

    #[test]
    fn merge_concatenates_points_and_rebases_primitives() {
        let out = eval_merge(Some(Arc::new(geo_a())), Some(Arc::new(geo_b())));
        let geo = out.downcast_ref::<Geometry>().unwrap();
        assert_eq!(geo.point_count(), 6);
        assert_eq!(point_positions(geo)[4], Vec2(5.0, 5.0));
        assert_eq!(geo.primitive_count(), 2);
        let Primitive::Path { verts, closed } = &geo.primitives()[1] else {
            panic!("B contributed a path, not a mesh");
        };
        assert_eq!(*verts, 4..6, "B's vertex range re-based past A's points");
        assert!(!closed);
    }

    #[test]
    fn merge_unions_attributes_with_typed_zero_fill() {
        let out = eval_merge(Some(Arc::new(geo_a())), Some(Arc::new(geo_b())));
        let geo = out.downcast_ref::<Geometry>().unwrap();
        let pscale = geo
            .points()
            .get(names::PSCALE)
            .unwrap()
            .as_f32(names::PSCALE)
            .unwrap();
        assert_eq!(pscale, [1.0, 2.0, 3.0, 4.0, 0.0, 0.0]);
        let cd = geo
            .points()
            .get(names::CD)
            .unwrap()
            .as_vec3(names::CD)
            .unwrap();
        assert_eq!(cd[..4], vec![Vec3(0.0, 0.0, 0.0); 4]);
        assert_eq!(cd[4], Vec3(1.0, 0.0, 0.0));
    }

    #[test]
    fn merge_type_conflict_is_an_error() {
        let mut conflicted = geo_b();
        conflicted
            .points_mut()
            .insert(names::PSCALE, AttributeArray::I32(vec![1, 2]))
            .unwrap();
        let node = Node::new(NodeId::new(3), "geometry.merge")
            .with_input("A", &[DataTypeId::GEOMETRY])
            .with_input("B", &[DataTypeId::GEOMETRY])
            .with_output("output", DataTypeId::GEOMETRY);
        let graph = Graph::new()
            .add_node(node)
            .unwrap()
            .add_node(
                Node::new(NodeId::new(10), "test.source")
                    .with_output("output", DataTypeId::GEOMETRY),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(11), "test.source")
                    .with_output("output", DataTypeId::GEOMETRY),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(20),
                NodeId::new(10),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(21),
                NodeId::new(11),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(1),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(3), Arc::new(GeometryMergeProcessor));
        ev.register(NodeId::new(10), Arc::new(Fixed(Arc::new(geo_a()))));
        ev.register(NodeId::new(11), Arc::new(Fixed(Arc::new(conflicted))));
        assert!(ev.evaluate(&graph, NodeId::new(3), &ctx()).is_err());
    }

    /// A side with primitives but no primitive attributes still yields
    /// full-length merged columns (fill length = primitive count, not the
    /// empty attribute set's element count).
    #[test]
    fn merge_fills_primitive_attrs_for_the_attributeless_side() {
        let a = geo_a(); // 1 primitive, no primitive attrs.
        let mut b = geo_b(); // 1 primitive...
        b.primitive_attrs_mut()
            .insert("mat", AttributeArray::I32(vec![7]))
            .unwrap();
        let out = eval_merge(Some(Arc::new(a)), Some(Arc::new(b)));
        let geo = out.downcast_ref::<Geometry>().unwrap();
        assert_eq!(geo.primitive_count(), 2);
        let mat = geo
            .primitive_attrs()
            .get("mat")
            .unwrap()
            .as_i32("mat")
            .unwrap();
        assert_eq!(mat, [0, 7], "A's row zero-filled, B's row appended");
    }

    #[test]
    fn merge_concatenates_instances() {
        let instance_geo = |x: f32| {
            let mut geo = Geometry::new();
            geo.instances_mut()
                .insert(names::INDEX, AttributeArray::I32(vec![0]))
                .unwrap();
            geo.instances_mut()
                .insert(names::P, AttributeArray::Vec2(vec![Vec2(x, 0.0)]))
                .unwrap();
            geo
        };
        let out = eval_merge(
            Some(Arc::new(instance_geo(1.0))),
            Some(Arc::new(instance_geo(2.0))),
        );
        let geo = out.downcast_ref::<Geometry>().unwrap();
        assert_eq!(geo.instance_count(), 2);
        let p = geo
            .instances()
            .get(names::P)
            .unwrap()
            .as_vec2(names::P)
            .unwrap();
        assert_eq!(p, [Vec2(1.0, 0.0), Vec2(2.0, 0.0)]);
    }

    #[test]
    fn merge_preserves_matching_plural_instance_sources() {
        let first = Arc::new(Geometry::from_points(vec![Vec2(0.0, 0.0)]));
        let second = Arc::new(Geometry::from_points(vec![Vec2(1.0, 0.0)]));
        let instance_geo = |x: f32| {
            let mut geo = Geometry::new();
            geo.instances_mut()
                .insert(names::P, AttributeArray::Vec2(vec![Vec2(x, 0.0)]))
                .unwrap();
            geo.set_instance_sources(vec![first.clone(), second.clone()]);
            geo
        };

        let out = eval_merge(
            Some(Arc::new(instance_geo(1.0))),
            Some(Arc::new(instance_geo(2.0))),
        );
        let merged = out.downcast_ref::<Geometry>().unwrap();
        let sources = merged.sources();
        assert_eq!(sources.len(), 2);
        assert!(Arc::ptr_eq(sources[0].geometry().unwrap(), &first));
        assert!(Arc::ptr_eq(sources[1].geometry().unwrap(), &second));
    }

    #[test]
    fn merge_rejects_conflicting_plural_instance_sources() {
        let shared = Arc::new(Geometry::from_points(vec![Vec2(0.0, 0.0)]));
        let mut a = geo_a();
        a.set_instance_sources(vec![shared.clone()]);
        let mut b = geo_b();
        b.set_instance_sources(vec![
            shared,
            Arc::new(Geometry::from_points(vec![Vec2(1.0, 0.0)])),
        ]);

        let node = Node::new(NodeId::new(3), "geometry.merge")
            .with_input("A", &[DataTypeId::GEOMETRY])
            .with_input("B", &[DataTypeId::GEOMETRY])
            .with_output("output", DataTypeId::GEOMETRY);
        let graph = Graph::new()
            .add_node(node)
            .unwrap()
            .add_node(
                Node::new(NodeId::new(10), "test.source")
                    .with_output("output", DataTypeId::GEOMETRY),
            )
            .unwrap()
            .add_node(
                Node::new(NodeId::new(11), "test.source")
                    .with_output("output", DataTypeId::GEOMETRY),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(20),
                NodeId::new(10),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(21),
                NodeId::new(11),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(1),
            )
            .unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(3), Arc::new(GeometryMergeProcessor));
        ev.register(NodeId::new(10), Arc::new(Fixed(Arc::new(a))));
        ev.register(NodeId::new(11), Arc::new(Fixed(Arc::new(b))));

        assert!(ev.evaluate(&graph, NodeId::new(3), &ctx()).is_err());
    }

    /// A detail-only side is not "empty": its detail survives the merge
    /// (A's detail wins; here A is the detail-only side).
    #[test]
    fn merge_keeps_a_detail_only_side() {
        let mut detail_only = Geometry::new();
        detail_only
            .detail_mut()
            .insert("resolution", AttributeArray::Vec2(vec![Vec2(64.0, 64.0)]))
            .unwrap();
        let out = eval_merge(Some(Arc::new(detail_only)), Some(Arc::new(geo_b())));
        let geo = out.downcast_ref::<Geometry>().unwrap();
        assert_eq!(geo.point_count(), 2, "B's points survive");
        let res = geo
            .detail()
            .get("resolution")
            .expect("A's detail survives")
            .as_vec2("resolution")
            .unwrap();
        assert_eq!(res, [Vec2(64.0, 64.0)]);
    }

    #[test]
    fn merge_with_one_side_missing_or_empty_passes_through() {
        let input = Arc::new(geo_a());
        // B unconnected: A's Arc passes through untouched.
        let out = eval_merge(Some(input.clone()), None);
        assert!(std::ptr::eq(
            out.downcast_ref::<Geometry>().unwrap(),
            input.as_ref()
        ));
        // A empty: B passes through.
        let out = eval_merge(Some(Arc::new(Geometry::new())), Some(input.clone()));
        assert!(std::ptr::eq(
            out.downcast_ref::<Geometry>().unwrap(),
            input.as_ref()
        ));
        // Both missing: empty result, no error.
        let out = eval_merge(None, None);
        assert_eq!(out.downcast_ref::<Geometry>().unwrap().point_count(), 0);
    }

    #[test]
    fn missing_input_is_an_error() {
        let node = Node::new(NodeId::new(1), "geometry.transform")
            .with_input("geometry", &[DataTypeId::GEOMETRY])
            .with_output("output", DataTypeId::GEOMETRY);
        let graph = Graph::new().add_node(node).unwrap();
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(GeometryTransformProcessor));
        assert!(ev.evaluate(&graph, NodeId::new(1), &ctx()).is_err());
    }

    #[test]
    fn is_not_time_dependent() {
        assert!(!GeometryTransformProcessor.is_time_dependent());
    }
}
