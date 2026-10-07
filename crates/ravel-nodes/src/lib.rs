// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Built-in node processors for the Ravel DAG evaluation pipeline.
//!
//! Each module implements [`ravel_core::eval::NodeProcessor`] for one of the
//! registered built-in node types. GPU-accelerated processors use
//! [`ravel_gpu`] for shader compilation and texture management.

pub mod attribute;
pub mod blur;
pub mod color;
pub mod color_correct;
pub mod comp;
pub mod comp_info;
pub mod constant;
pub mod display;
pub use display::{DisplayFrame, DisplayTransform};
pub mod eval_hooks;
pub use eval_hooks::GpuEvalHooks;
pub mod field;
pub mod flatten;
pub mod geometry;
pub mod geometry_ops;
mod gpu_util;
pub use gpu_util::{GpuImage, begin_upload_scope, clone_frame_value, ensure_cpu, ensure_gpu};
pub mod layer_info;
pub mod layer_ref;
pub mod math;
pub mod media;
pub mod merge;
pub mod net;
pub mod rasterize;
pub mod scatter;
pub mod scene;
pub mod shape;
pub mod style;
pub mod subnet;
pub mod text;
pub mod transform;
pub mod transform_section;
pub mod vector;

use ravel_core::eval::{EvalContext, ProcessorRegistry};
use ravel_core::graph::{Graph, Node};
use ravel_core::registry::builtin;
use ravel_gpu::{GpuContext, ShaderManager, TexturePool};
use ravel_media::frame_cache::MediaFrameCache;
use std::sync::{Arc, Mutex};

/// Per-axis scale from composition-space coordinates to output-canvas pixels.
pub(crate) fn composition_scale(ctx: &EvalContext) -> (f64, f64) {
    ctx.comp_to_canvas_scale()
}

/// Preserve the outer composition-to-canvas scale for a new coordinate basis.
pub(crate) fn scaled_resolution(ctx: &EvalContext, comp_resolution: (u32, u32)) -> (u32, u32) {
    let scale = composition_scale(ctx);
    (
        (comp_resolution.0 as f64 * scale.0).round() as u32,
        (comp_resolution.1 as f64 * scale.1).round() as u32,
    )
}

/// Register a [`NodeProcessor`] for every node in `graph` whose `type_key`
/// matches a built-in processor, recursing into subnet inner graphs
/// (REQ-LAYER-003).
///
/// Nodes with unrecognized type keys are silently skipped — they may be
/// handled by plugins or user scripts.
/// Takes any [`ProcessorRegistry`] — an
/// [`Evaluator`](ravel_core::eval::Evaluator) directly, or the restricted
/// view an evaluation worker hook is given.
pub fn register_all_processors<R: ProcessorRegistry + ?Sized>(
    evaluator: &mut R,
    graph: &Graph,
    ctx: &GpuContext,
    shaders: &mut ShaderManager,
    pool: &Arc<Mutex<TexturePool>>,
    media_frames: &MediaFrameCache,
) {
    let span = tracing::debug_span!("register_processors", nodes = graph.nodes().count());
    let _guard = span.enter();
    for node in graph.nodes() {
        if let Some(proc) = processor_for_node(node, ctx, shaders, pool, media_frames) {
            evaluator.register(node.id, proc);
        }
        if let Some(inner) = node.subnet.as_deref() {
            register_all_processors(evaluator, inner, ctx, shaders, pool, media_frames);
        }
    }
}

/// Convenience constructor for a standalone eval-worker texture pool.
///
/// One pool per evaluation worker: GPU node processors allocate their
/// intermediates and resident outputs from it, and `GpuFrameBuffer` handles
/// return textures on drop. This pool owns a fixed 512 MiB idle budget and
/// answers to nobody — for tests, examples and benchmarks. The application
/// uses [`shared_texture_pool_with_budget`], whose idle allowance is the VRAM
/// the shared `CacheBudget` has left.
pub fn shared_texture_pool(ctx: &GpuContext) -> Arc<Mutex<TexturePool>> {
    Arc::new(Mutex::new(TexturePool::new(ctx.clone(), 512 * 1024 * 1024)))
}

/// The eval-worker texture pool, subordinate to the process cache budget.
///
/// The production entry point (`CACHE-3`): resident textures and pooled
/// textures are then charged to one VRAM total, and the pool's idle share is
/// the residual rather than a second, independent ceiling.
pub fn shared_texture_pool_with_budget(
    ctx: &GpuContext,
    budget: ravel_core::cache_budget::SharedCacheBudget,
) -> Arc<Mutex<TexturePool>> {
    Arc::new(Mutex::new(TexturePool::with_shared_budget(
        ctx.clone(),
        budget,
    )))
}

/// Build the built-in processor for a single `node`, or `None` when its
/// `type_key` is not a built-in (plugin space).
///
/// Processors never capture parameter values — the evaluator resolves them
/// per frame into [`ravel_core::eval::ResolvedParams`] — so parameter edits
/// only require dirty marking, not a rebuild.
///
/// A type whose template declares a transform section comes back wrapped in
/// [`transform_section::wrap`], which applies the node's own `translate` /
/// `rotation` / `scale` to the geometry it produced.
pub fn processor_for_node(
    node: &Node,
    ctx: &GpuContext,
    shaders: &mut ShaderManager,
    pool: &Arc<Mutex<TexturePool>>,
    media_frames: &MediaFrameCache,
) -> Option<Arc<dyn ravel_core::eval::NodeProcessor>> {
    let processor: Option<Arc<dyn ravel_core::eval::NodeProcessor>> = match node.type_key.as_str() {
        "attribute.set" => Some(Arc::new(attribute::AttributeSetProcessor::from_node(node))),
        "attribute.delete" => Some(Arc::new(attribute::AttributeDeleteProcessor::from_node(
            node,
        ))),
        "attribute.promote" => Some(Arc::new(attribute::AttributePromoteProcessor::from_node(
            node,
        ))),
        "attribute.transfer" => Some(Arc::new(attribute::AttributeTransferProcessor::from_node(
            node,
        ))),
        "attribute.path_sample" => Some(Arc::new(attribute::PathSampleProcessor::from_node(node))),
        "attribute.curveu" => Some(Arc::new(attribute::CurveUProcessor::from_node(node))),
        "text.font" => Some(Arc::new(text::FontProcessor::from_node(node))),
        "text.layout" => Some(Arc::new(text::LayoutProcessor::from_node(node))),
        "text.to_path" => Some(Arc::new(text::ToPathProcessor::from_node(node))),
        "text.on_path" => Some(Arc::new(text::OnPathProcessor::from_node(node))),
        "style.fill" => Some(Arc::new(style::StyleFillProcessor::from_node(node))),
        "style.stroke" => Some(Arc::new(style::StyleStrokeProcessor::from_node(node))),
        "style.dash" => Some(Arc::new(style::StyleDashProcessor::from_node(node))),
        "constant" => Some(Arc::new(constant::ConstantProcessor::from_node(node))),
        "constant.color" => Some(Arc::new(constant::ColorConstantProcessor::from_node(node))),
        builtin::CONSTANT_VEC2 => Some(Arc::new(constant::VectorConstantProcessor::new(
            ravel_core::id::DataTypeId::VEC2,
        ))),
        builtin::CONSTANT_VEC3 => Some(Arc::new(constant::VectorConstantProcessor::new(
            ravel_core::id::DataTypeId::VEC3,
        ))),
        builtin::CONSTANT_VEC4 => Some(Arc::new(constant::VectorConstantProcessor::new(
            ravel_core::id::DataTypeId::VEC4,
        ))),
        "math.scalar" => Some(Arc::new(math::MathScalarProcessor::from_node(node))),
        "math.remap" => Some(Arc::new(math::MathRemapProcessor::from_node(node))),
        "math.curve" => Some(Arc::new(math::MathCurveProcessor::from_node(node))),
        "color.ramp" => Some(Arc::new(color::ColorRampProcessor::from_node(node))),
        builtin::VECTOR_CONSTRUCT_VEC2 => Some(Arc::new(vector::VectorConstructProcessor::new(
            vector::VectorArity::Vec2,
        ))),
        builtin::VECTOR_CONSTRUCT_VEC3 => Some(Arc::new(vector::VectorConstructProcessor::new(
            vector::VectorArity::Vec3,
        ))),
        builtin::VECTOR_CONSTRUCT_VEC4 => Some(Arc::new(vector::VectorConstructProcessor::new(
            vector::VectorArity::Vec4,
        ))),
        builtin::VECTOR_SPLIT_VEC2 => Some(Arc::new(vector::VectorSplitProcessor::new(
            vector::VectorArity::Vec2,
        ))),
        builtin::VECTOR_SPLIT_VEC3 => Some(Arc::new(vector::VectorSplitProcessor::new(
            vector::VectorArity::Vec3,
        ))),
        builtin::VECTOR_SPLIT_VEC4 => Some(Arc::new(vector::VectorSplitProcessor::new(
            vector::VectorArity::Vec4,
        ))),
        builtin::VECTOR_SWIZZLE_VEC2 => Some(Arc::new(vector::VectorSwizzleProcessor::new(
            vector::VectorArity::Vec2,
        ))),
        builtin::VECTOR_SWIZZLE_VEC3 => Some(Arc::new(vector::VectorSwizzleProcessor::new(
            vector::VectorArity::Vec3,
        ))),
        builtin::VECTOR_SWIZZLE_VEC4 => Some(Arc::new(vector::VectorSwizzleProcessor::new(
            vector::VectorArity::Vec4,
        ))),
        builtin::VECTOR_LENGTH => Some(Arc::new(vector::VectorLengthProcessor)),
        builtin::VECTOR_NORMALIZE_VEC2 => Some(Arc::new(vector::VectorNormalizeProcessor::new(
            vector::VectorArity::Vec2,
        ))),
        builtin::VECTOR_NORMALIZE_VEC3 => Some(Arc::new(vector::VectorNormalizeProcessor::new(
            vector::VectorArity::Vec3,
        ))),
        builtin::VECTOR_NORMALIZE_VEC4 => Some(Arc::new(vector::VectorNormalizeProcessor::new(
            vector::VectorArity::Vec4,
        ))),
        builtin::VECTOR_DOT => Some(Arc::new(vector::VectorDotProcessor)),
        builtin::VECTOR_CROSS_VEC2 => Some(Arc::new(vector::VectorCrossProcessor::new(
            vector::VectorArity::Vec2,
        ))),
        builtin::VECTOR_CROSS_VEC3 => Some(Arc::new(vector::VectorCrossProcessor::new(
            vector::VectorArity::Vec3,
        ))),
        // Every rasterize node takes the resident GPU path, synthetic or not.
        // `shape_layer_golden` used to pin the synthetic ones to the CPU
        // reference implementation; it now requires the two to agree instead,
        // which is what that pin was standing in for.
        "rasterize" => Some(Arc::new(rasterize::RasterizeProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        "color_correct" => Some(Arc::new(color_correct::ColorCorrectProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        "blur" => Some(Arc::new(blur::BlurProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        "transform" => Some(Arc::new(transform::TransformProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        "merge" => Some(Arc::new(merge::MergeProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        "geometry.transform" => Some(Arc::new(geometry::GeometryTransformProcessor::from_node(
            node,
        ))),
        "geometry.merge" => Some(Arc::new(geometry::GeometryMergeProcessor::from_node(node))),
        "geometry.connect" => Some(Arc::new(geometry::GeometryConnectProcessor::from_node(
            node,
        ))),
        "geometry.sort" => Some(Arc::new(geometry::GeometrySortProcessor::from_node(node))),
        "geometry.group_index" => Some(Arc::new(
            geometry_ops::GeometryGroupIndexProcessor::from_node(node),
        )),
        "geometry.repeat" => Some(Arc::new(geometry_ops::GeometryRepeatProcessor::from_node(
            node,
        ))),
        "geometry.bend" => Some(Arc::new(geometry_ops::GeometryDeformProcessor::bend())),
        "geometry.twist" => Some(Arc::new(geometry_ops::GeometryDeformProcessor::twist())),
        "geometry.taper" => Some(Arc::new(geometry_ops::GeometryDeformProcessor::taper())),
        "geometry.from_image" => Some(Arc::new(geometry::GeometryFromImageProcessor::from_node(
            node,
        ))),
        "scene.add" => Some(Arc::new(scene::SceneAddProcessor::from_node(node))),
        "scene.merge" => Some(Arc::new(scene::SceneMergeProcessor::from_node(node))),
        "scene.camera" => Some(Arc::new(scene::SceneCameraProcessor::from_node(node))),
        "field.noise" => Some(Arc::new(field::NoiseFieldProcessor::from_node(node))),
        "field.direction_to" => Some(Arc::new(field::DirectionToFieldProcessor)),
        "field.curl_noise" => Some(Arc::new(field::CurlNoiseFieldProcessor)),
        "field.gradient" => Some(Arc::new(field::GradientFieldProcessor)),
        "field.radial" => Some(Arc::new(field::RadialFieldProcessor)),
        "field.falloff" => Some(Arc::new(field::FalloffFieldProcessor::from_node(node))),
        "field.curve_remap" => Some(Arc::new(field::CurveRemapFieldProcessor::from_node(node))),
        "field.ramp" => Some(Arc::new(field::RampFieldProcessor::from_node(node))),
        "field.expression" => Some(Arc::new(field::ExpressionFieldProcessor::from_node(node))),
        "field.add" => Some(Arc::new(field::AddFieldProcessor)),
        "field.multiply" => Some(Arc::new(field::MultiplyFieldProcessor)),
        "field.max" => Some(Arc::new(field::MaxFieldProcessor)),
        "field.blend" => Some(Arc::new(field::BlendFieldProcessor::from_node(node))),
        "field.length" => Some(Arc::new(field::LengthFieldProcessor)),
        "field.angle" => Some(Arc::new(field::AngleFieldProcessor)),
        "field.component" => Some(Arc::new(field::ComponentFieldProcessor)),
        builtin::FIELD_COMPOSE_VEC2 => Some(Arc::new(field::ComposeFieldProcessor::new(2))),
        builtin::FIELD_COMPOSE_VEC3 => Some(Arc::new(field::ComposeFieldProcessor::new(3))),
        builtin::FIELD_COMPOSE_VEC4 => Some(Arc::new(field::ComposeFieldProcessor::new(4))),
        "field.attribute" => Some(Arc::new(field::AttributeFieldProcessor::from_node(node))),
        "field.time" => Some(Arc::new(field::TimeFieldProcessor)),
        "field.constant" => Some(Arc::new(field::ConstantFieldProcessor)),
        "field.apply" => Some(Arc::new(field::ApplyFieldProcessor::from_node(node))),
        // Shape generators
        "shape.rect" => Some(Arc::new(shape::RectProcessor::from_node(node))),
        "shape.ellipse" => Some(Arc::new(shape::EllipseProcessor::from_node(node))),
        "shape.polygon" => Some(Arc::new(shape::PolygonProcessor::from_node(node))),
        "shape.star" => Some(Arc::new(shape::StarProcessor::from_node(node))),
        "shape.line" => Some(Arc::new(shape::LineProcessor::from_node(node))),
        "shape.grid" => Some(Arc::new(shape::GridProcessor::from_node(node))),
        "shape.custom_path" => Some(Arc::new(shape::CustomPathProcessor::from_node(node))),
        // Scatter / instance duplication
        "scatter.grid" => Some(Arc::new(scatter::GridProcessor::from_node(node))),
        "scatter.circular" => Some(Arc::new(scatter::CircularProcessor::from_node(node))),
        "scatter.path_array" => Some(Arc::new(scatter::PathArrayProcessor::from_node(node))),
        "scatter.scatter" => Some(Arc::new(scatter::ScatterProcessor::from_node(node))),
        // Composition shell (synthetic) nodes
        "comp.background" => Some(Arc::new(comp::CompBackgroundProcessor::from_node(node))),
        "comp.network" => Some(Arc::new(comp::CompNetworkProcessor::from_node(node))),
        "comp.transform" => Some(Arc::new(comp::CompTransformGpuProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        // The GPU version is the default path; `comp::CompOpacityProcessor`
        // stays public as the CPU reference tests register explicitly.
        "comp.opacity" => Some(Arc::new(comp::CompOpacityGpuProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        // User-placed image nodes (not shell nodes): they read nothing from
        // the `Document`.
        "comp.solid" => Some(Arc::new(comp::CompSolidProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        "comp.fill" => Some(Arc::new(comp::CompColorizeProcessor::new(
            comp::ColorizeKind::Fill,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.tint" => Some(Arc::new(comp::CompColorizeProcessor::new(
            comp::ColorizeKind::Tint,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.brightness_contrast" => Some(Arc::new(comp::CompGradeProcessor::new(
            comp::GradeKind::BrightnessContrast,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.hue_saturation" => Some(Arc::new(comp::CompGradeProcessor::new(
            comp::GradeKind::HueSaturation,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.levels" => Some(Arc::new(comp::CompGradeProcessor::new(
            comp::GradeKind::Levels,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.curves" => Some(Arc::new(comp::CompGradeProcessor::new(
            comp::GradeKind::Curves,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.lift_gamma_gain" => Some(Arc::new(comp::CompGradeProcessor::new(
            comp::GradeKind::LiftGammaGain,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.hsl_curves" => Some(Arc::new(comp::CompGradeProcessor::new(
            comp::GradeKind::HslCurves,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.directional_blur" => Some(Arc::new(comp::CompDistortProcessor::new(
            comp::DistortKind::DirectionalBlur,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.radial_blur" => Some(Arc::new(comp::CompDistortProcessor::new(
            comp::DistortKind::RadialBlur,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.sharpen" => Some(Arc::new(comp::CompDistortProcessor::new(
            comp::DistortKind::Sharpen,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.warp" => Some(Arc::new(comp::CompDistortProcessor::new(
            comp::DistortKind::Warp,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.lens_distortion" => Some(Arc::new(comp::CompDistortProcessor::new(
            comp::DistortKind::LensDistortion,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.ripple" => Some(Arc::new(comp::CompDistortProcessor::new(
            comp::DistortKind::Ripple,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.mirror" => Some(Arc::new(comp::CompTileProcessor::new(
            comp::TileKind::Mirror,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.tile" => Some(Arc::new(comp::CompTileProcessor::new(
            comp::TileKind::Tile,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.mask" => Some(Arc::new(comp::CompMaskProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        "comp.key" => Some(Arc::new(comp::CompKeyProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.gradient" => Some(Arc::new(comp::CompGenerateProcessor::new(
            comp::GenerateKind::Gradient,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.noise" => Some(Arc::new(comp::CompGenerateProcessor::new(
            comp::GenerateKind::Noise,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.fractal" => Some(Arc::new(comp::CompGenerateProcessor::new(
            comp::GenerateKind::Fractal,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.checkerboard" => Some(Arc::new(comp::CompGenerateProcessor::new(
            comp::GenerateKind::Checkerboard,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.glow" => Some(Arc::new(comp::CompStylizeProcessor::new(
            comp::StylizeKind::Glow,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.drop_shadow" => Some(Arc::new(comp::CompStylizeProcessor::new(
            comp::StylizeKind::DropShadow,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.stroke" => Some(Arc::new(comp::CompStylizeProcessor::new(
            comp::StylizeKind::Stroke,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.emboss" => Some(Arc::new(comp::CompStylizeProcessor::new(
            comp::StylizeKind::Emboss,
            ctx.clone(),
            shaders,
            pool.clone(),
        ))),
        "comp.alpha" => Some(Arc::new(comp::CompAlphaProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        // One pipeline serves every blend mode; `comp::CompMergeProcessor`
        // stays public as the CPU reference tests register explicitly.
        t if t.starts_with("comp.merge.") => Some(Arc::new(comp::CompMergeGpuProcessor::new(
            ctx.clone(),
            shaders,
            pool.clone(),
            node,
        ))),
        // Media: `video` is the pre-rename alias persisted documents may
        // still carry in memory; loading normalizes it to `media`
        // (Document::normalize_node_type_aliases).
        "media" | "video" => Some(Arc::new(media::MediaProcessor::from_node(
            node,
            media_frames,
        ))),
        // Scene information (REQ-LAYER-002/005) — Document reads, no pull
        "comp.info" => Some(Arc::new(comp_info::CompInfoProcessor::from_node(node))),
        // Cross-layer reference (REQ-LAYER-005)
        "layer.info" => Some(Arc::new(layer_info::LayerInfoProcessor::from_node(node))),
        "layer.ref" => Some(Arc::new(layer_ref::LayerRefProcessor::from_node(node))),
        // Nested network (REQ-LAYER-003)
        "subnet" => Some(Arc::new(subnet::SubnetProcessor::from_node(node))),
        // Network interface nodes
        "net.in" => Some(Arc::new(net::NetInProcessor::from_node(node))),
        "net.out" => Some(Arc::new(net::NetOutProcessor::from_node(node))),
        _ => None,
    };
    // One site decides whether a built-in carries its own transform section,
    // and the registry answers whether it does — no consumer repeats the
    // `type_key` match (`node-transform-section-plan.md`, TFORM-1).
    processor.map(|inner| transform_section::wrap(node, inner))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::{EvalContext, Evaluator};
    use ravel_core::geometry::Geometry;
    use ravel_core::graph::{Node, ParameterValue};
    use ravel_core::id::{DataTypeId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::types::{FrameBuffer, FrameRate, Scalar};

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (4, 4))
    }

    fn solid_fb(width: u32, height: u32, r: f32, g: f32, b: f32, a: f32) -> FrameBuffer {
        let n = (width * height) as usize;
        let mut data = Vec::with_capacity(n * 4);
        for _ in 0..n {
            data.extend_from_slice(&[r, g, b, a]);
        }
        FrameBuffer::from_f32(width, height, data)
    }

    /// RESP-3 (issue HIGH-06): a document with N nodes of a GPU type must not
    /// pay N shader compilations and N pipeline creations. The pipeline depends
    /// on the shader and the layout, never on the node.
    #[test]
    fn gpu_nodes_of_one_type_share_a_pipeline() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let pool = shared_texture_pool(&gpu);
        let frames = MediaFrameCache::standalone();

        let blur = |id: u64, radius: f32| {
            Node::new(NodeId::new(id), "blur")
                .with_input("input", &[DataTypeId::FRAME_BUFFER])
                .with_output("output", DataTypeId::FRAME_BUFFER)
                .with_param("radius", ParameterValue::Float(radius))
        };

        let first = blur(1, 4.0);
        let _ =
            processor_for_node(&first, &gpu, &mut shaders, &pool, &frames).expect("blur processor");
        let after_first = shaders.created_pipeline_count();
        assert_eq!(after_first, 1, "the first blur node builds the pipeline");

        for id in 2..=8 {
            let node = blur(id, id as f32);
            let _ = processor_for_node(&node, &gpu, &mut shaders, &pool, &frames)
                .expect("blur processor");
        }
        assert_eq!(
            shaders.created_pipeline_count(),
            after_first,
            "further blur nodes must reuse it"
        );
        assert_eq!(shaders.cached_module_count(), 1, "and one compiled module");
    }

    /// The GPU processors are the ones that hold nothing off their node, so a
    /// parameter edit can invalidate instead of rebuilding them. Everything that
    /// captures node state keeps the conservative default.
    #[test]
    fn gpu_processors_opt_out_of_rebuild_on_node_change() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let pool = shared_texture_pool(&gpu);
        let frames = MediaFrameCache::standalone();

        let frame_node = |id: u64, type_key: &str| {
            Node::new(NodeId::new(id), type_key)
                .with_input("input", &[DataTypeId::FRAME_BUFFER])
                .with_output("output", DataTypeId::FRAME_BUFFER)
        };
        // The shell processors belong here too: they resolve their layer from
        // the `Document` at process time, so a layer edit must invalidate
        // rather than rebuild (and recompile the shader).
        for (id, type_key) in [
            "blur",
            "color_correct",
            "transform",
            "merge",
            "rasterize",
            "comp.opacity",
            "comp.solid",
            "comp.fill",
            "comp.tint",
            "comp.alpha",
            "comp.brightness_contrast",
            "comp.hue_saturation",
            "comp.levels",
            "comp.curves",
            "comp.lift_gamma_gain",
            "comp.hsl_curves",
            "comp.directional_blur",
            "comp.radial_blur",
            "comp.sharpen",
            "comp.warp",
            "comp.lens_distortion",
            "comp.ripple",
            "comp.mirror",
            "comp.tile",
            "comp.mask",
            "comp.key",
            "comp.gradient",
            "comp.noise",
            "comp.fractal",
            "comp.checkerboard",
            "comp.glow",
            "comp.drop_shadow",
            "comp.stroke",
            "comp.emboss",
            "comp.transform",
            "comp.merge.normal",
            "comp.merge.adjustment",
        ]
        .iter()
        .enumerate()
        {
            let node = frame_node(id as u64 + 1, type_key);
            let proc = processor_for_node(&node, &gpu, &mut shaders, &pool, &frames)
                .unwrap_or_else(|| panic!("no processor for {type_key}"));
            assert!(
                !proc.rebuild_on_node_change(),
                "{type_key} captures nothing from its node and must not be rebuilt"
            );
        }

        // A processor that reads the node at construction must say so.
        let constant = Node::new(NodeId::new(99), "constant")
            .with_output("value", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(1.0));
        let proc =
            processor_for_node(&constant, &gpu, &mut shaders, &pool, &frames).expect("processor");
        assert!(
            proc.rebuild_on_node_change(),
            "a node-state processor must keep the conservative default"
        );
    }

    /// The four user-placed image nodes are registered *and* evaluable: a chain
    /// built from the registry's own templates (so a template without a
    /// processor, or a processor keyed on another name, fails here) renders.
    #[test]
    fn solid_tint_alpha_chain_built_from_templates_evaluates() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let mut reg = ravel_core::registry::NodeRegistry::new();
        builtin::register_builtins(&mut reg);

        let node = |id: u64, key: &str| reg.create_node(key, NodeId::new(id)).expect(key);
        let mut graph = Graph::new();
        for (id, key) in [(1, "comp.solid"), (2, "comp.tint"), (3, "comp.alpha")] {
            graph = graph.add_node(node(id, key)).unwrap();
        }
        for (edge, from, to) in [(1, 1, 2), (2, 2, 3)] {
            graph = graph
                .add_edge(
                    EdgeId::new(edge),
                    NodeId::new(from),
                    OutputPortIndex(0),
                    NodeId::new(to),
                    InputPortIndex(0),
                )
                .unwrap();
        }
        // Default `comp.solid` is opaque white; default tint maps white to
        // white; default `comp.alpha` inverts: white at alpha 0.
        let mut ev = Evaluator::new();
        let pool = shared_texture_pool(&gpu);
        let frames = MediaFrameCache::standalone();
        register_all_processors(&mut ev, &graph, &gpu, &mut shaders, &pool, &frames);
        let out = ev.evaluate(&graph, NodeId::new(3), &ctx()).unwrap();
        let fb = out
            .downcast_ref::<ravel_gpu::GpuFrameBuffer>()
            .expect("the chain stays GPU-resident")
            .to_frame_buffer()
            .expect("readback");
        assert_eq!((fb.width, fb.height), (4, 4));
        for px in fb.as_f32().chunks_exact(4) {
            assert_eq!(px, [1.0, 1.0, 1.0, 0.0]);
        }
    }

    /// `(type_key, param, from, to, fixed)`: every animatable parameter of the
    /// colour-adjustment nodes, with two values that change a mid-tone pixel
    /// and the other parameters it needs held at.
    type AnimCase = (
        &'static str,
        &'static str,
        [f32; 4],
        [f32; 4],
        &'static [(&'static str, f32)],
    );
    const GRADE_ANIMATION: &[AnimCase] = &[
        (
            "comp.brightness_contrast",
            "brightness",
            [0.0; 4],
            [0.5; 4],
            &[],
        ),
        (
            "comp.brightness_contrast",
            "contrast",
            [1.0; 4],
            [2.0; 4],
            &[],
        ),
        (
            "comp.brightness_contrast",
            "pivot",
            [0.5; 4],
            [0.0; 4],
            &[("contrast", 2.0)],
        ),
        ("comp.hue_saturation", "hue", [0.0; 4], [120.0; 4], &[]),
        ("comp.hue_saturation", "saturation", [1.0; 4], [0.0; 4], &[]),
        ("comp.levels", "in_black", [0.0; 4], [0.25; 4], &[]),
        ("comp.levels", "in_white", [1.0; 4], [0.5; 4], &[]),
        ("comp.levels", "gamma", [1.0; 4], [2.0; 4], &[]),
        ("comp.levels", "out_black", [0.0; 4], [0.5; 4], &[]),
        ("comp.levels", "out_white", [1.0; 4], [0.5; 4], &[]),
        (
            "comp.lift_gamma_gain",
            "lift",
            [0.0; 4],
            [-0.5, 0.0, 0.0, 0.0],
            &[],
        ),
        (
            "comp.lift_gamma_gain",
            "gamma",
            [1.0; 4],
            [2.0, 1.0, 1.0, 1.0],
            &[],
        ),
        (
            "comp.lift_gamma_gain",
            "gain",
            [1.0; 4],
            [2.0, 1.0, 1.0, 1.0],
            &[],
        ),
        ("color_correct", "temperature", [0.0; 4], [1.0; 4], &[]),
        ("color_correct", "tint", [0.0; 4], [1.0; 4], &[]),
        ("color_correct", "exposure", [0.0; 4], [1.0; 4], &[]),
        ("color_correct", "brightness", [0.0; 4], [0.5; 4], &[]),
        ("color_correct", "contrast", [1.0; 4], [2.0; 4], &[]),
        ("color_correct", "saturation", [1.0; 4], [0.0; 4], &[]),
        (
            "color_correct",
            "highlights",
            [0.0; 4],
            [1.0; 4],
            &[("exposure", 1.3)],
        ),
        ("color_correct", "shadows", [0.0; 4], [1.0; 4], &[]),
        (
            "color_correct",
            "whites",
            [0.0; 4],
            [1.0; 4],
            &[("exposure", 1.3)],
        ),
        (
            "color_correct",
            "blacks",
            [0.0; 4],
            [1.0; 4],
            &[("exposure", -1.5)],
        ),
        ("color_correct", "vibrance", [0.0; 4], [1.0; 4], &[]),
        (
            "color_correct",
            "lift",
            [0.0; 4],
            [-0.5, 0.0, 0.0, 0.0],
            &[],
        ),
        (
            "color_correct",
            "gamma",
            [1.0; 4],
            [2.0, 1.0, 1.0, 1.0],
            &[],
        ),
        ("color_correct", "gain", [1.0; 4], [2.0, 1.0, 1.0, 1.0], &[]),
        ("color_correct", "fade", [0.0; 4], [0.5; 4], &[]),
        ("color_correct", "vignette", [0.0; 4], [-1.0; 4], &[]),
        (
            "color_correct",
            "vignette_midpoint",
            [0.5; 4],
            [0.9; 4],
            &[("vignette", -1.0)],
        ),
        (
            "color_correct",
            "vignette_feather",
            [0.5; 4],
            [0.1; 4],
            &[("vignette", -1.0)],
        ),
    ];
    const GRADE_NODES: [&str; 7] = [
        "color_correct",
        "comp.brightness_contrast",
        "comp.hue_saturation",
        "comp.levels",
        "comp.curves",
        "comp.lift_gamma_gain",
        "comp.hsl_curves",
    ];

    /// `comp.solid` (a mid-tone colour) feeding `type_key`, evaluated at
    /// `frame` through `register_all_processors`. `param` replaces the node's
    /// default: keyframed `from` at frame 0 to `to` at frame 10.
    fn grade_pixel(
        type_key: &str,
        animated: Option<(&str, [f32; 4], [f32; 4])>,
        fixed: &[(&str, f32)],
        frame: u64,
    ) -> [f32; 4] {
        use ravel_core::animation::Interpolation;
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::animation::curve::KeyframeCurve;

        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let mut reg = ravel_core::registry::NodeRegistry::new();
        builtin::register_builtins(&mut reg);

        let mut solid = reg.create_node("comp.solid", NodeId::new(1)).unwrap();
        let mut node = reg.create_node(type_key, NodeId::new(2)).expect(type_key);
        let set = |node: &mut Node, key: &str, value: ParameterValue| {
            node.parameters
                .iter_mut()
                .find(|p| p.key == key)
                .unwrap_or_else(|| panic!("{type_key} has no {key}"))
                .value = value;
        };
        let color = [0.6, 0.3, 0.2, 1.0];
        set(
            &mut solid,
            "color",
            ParameterValue::Channel4(color.map(AnimationChannel::constant)),
        );
        for (key, v) in fixed {
            set(&mut node, key, ParameterValue::Float(*v));
        }
        if let Some((key, from, to)) = animated {
            let keyed = |i: usize| {
                let mut curve = KeyframeCurve::new();
                curve.insert(0, from[i], Interpolation::Linear);
                curve.insert(10, to[i], Interpolation::Linear);
                AnimationChannel::keyframes(curve)
            };
            let template_value = node
                .parameters
                .iter()
                .find(|p| p.key == key)
                .unwrap()
                .value
                .clone();
            let value = match template_value {
                ParameterValue::Float(_) => ParameterValue::Channel(keyed(0)),
                ParameterValue::Channel3(_) => {
                    ParameterValue::Channel3([keyed(0), keyed(1), keyed(2)])
                }
                ParameterValue::Channel4(_) => {
                    ParameterValue::Channel4([keyed(0), keyed(1), keyed(2), keyed(3)])
                }
                other => panic!("{type_key}.{key} is {other:?}, not animatable"),
            };
            set(&mut node, key, value);
        }
        let graph = Graph::new()
            .add_node(solid)
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
        let pool = shared_texture_pool(&gpu);
        let frames = MediaFrameCache::standalone();
        register_all_processors(&mut ev, &graph, &gpu, &mut shaders, &pool, &frames);
        let out = ev
            .evaluate(
                &graph,
                NodeId::new(2),
                &EvalContext::new(frame, FrameRate::new(30, 1), (4, 4)),
            )
            .unwrap();
        let fb = out
            .downcast_ref::<ravel_gpu::GpuFrameBuffer>()
            .expect("GPU-resident")
            .to_frame_buffer()
            .expect("readback");
        let px = &fb.as_f32()[..4];
        [px[0], px[1], px[2], px[3]]
    }

    /// The image-effect nodes of the effects library (blur / distortion, then
    /// mirror / tile / mask / key).
    const FX_NODES: [&str; 10] = [
        "comp.directional_blur",
        "comp.radial_blur",
        "comp.sharpen",
        "comp.warp",
        "comp.lens_distortion",
        "comp.ripple",
        "comp.mirror",
        "comp.tile",
        "comp.mask",
        "comp.key",
    ];

    /// Every effect node, built from the registry's own template, evaluates
    /// through `register_all_processors` (a template without a processor, or a
    /// processor keyed on another name, fails here) and, with its default
    /// parameters set to the neutral values, keeps the image's size.
    #[test]
    fn every_distort_template_has_a_processor_and_evaluates() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let mut reg = ravel_core::registry::NodeRegistry::new();
        builtin::register_builtins(&mut reg);
        for type_key in FX_NODES {
            let mut graph = Graph::new()
                .add_node(reg.create_node("comp.solid", NodeId::new(1)).unwrap())
                .unwrap()
                .add_node(reg.create_node(type_key, NodeId::new(2)).unwrap())
                .unwrap()
                .add_edge(
                    EdgeId::new(1),
                    NodeId::new(1),
                    OutputPortIndex(0),
                    NodeId::new(2),
                    InputPortIndex(0),
                )
                .unwrap();
            // The second input: a warp's map, a mask's geometry.
            let second = match type_key {
                "comp.warp" => Some("comp.solid"),
                "comp.mask" => Some("shape.rect"),
                _ => None,
            };
            if let Some(second) = second {
                graph = graph
                    .add_node(reg.create_node(second, NodeId::new(3)).unwrap())
                    .unwrap()
                    .add_edge(
                        EdgeId::new(2),
                        NodeId::new(3),
                        OutputPortIndex(0),
                        NodeId::new(2),
                        InputPortIndex(1),
                    )
                    .unwrap();
            }
            let mut ev = Evaluator::new();
            let pool = shared_texture_pool(&gpu);
            let frames = MediaFrameCache::standalone();
            register_all_processors(&mut ev, &graph, &gpu, &mut shaders, &pool, &frames);
            let out = ev.evaluate(&graph, NodeId::new(2), &ctx()).unwrap();
            let fb = out
                .downcast_ref::<ravel_gpu::GpuFrameBuffer>()
                .unwrap_or_else(|| panic!("{type_key} stays GPU-resident"))
                .to_frame_buffer()
                .expect("readback");
            assert_eq!((fb.width, fb.height), (4, 4), "{type_key}");
            if type_key == "comp.mask" {
                // Which pixels the default rectangle covers is `rasterize`'s
                // business (tested in `comp/mask.rs`); here the chain only has
                // to evaluate.
                continue;
            }
            // Opaque white in: a uniform image stays uniform whatever the
            // distortion (edge clamp), so the template defaults cannot have
            // produced anything but white.
            for px in fb.as_f32().chunks_exact(4) {
                for (g, w) in px.iter().zip([1.0f32; 4]) {
                    assert!((g - w).abs() < 1e-5, "{type_key}: {px:?}");
                }
            }
        }
    }

    /// Every numeric parameter of an FX-2 node is a unified animation channel
    /// (`Float` / `Channel2` / `Channel4`), none a plain value the animation system cannot
    /// reach; the strings are the one dropdown.
    #[test]
    fn distort_parameters_are_animation_channels() {
        let mut reg = ravel_core::registry::NodeRegistry::new();
        builtin::register_builtins(&mut reg);
        for type_key in FX_NODES {
            for p in &reg.get(type_key).unwrap().default_params {
                match p.value {
                    ParameterValue::Float(_)
                    | ParameterValue::Channel2(_)
                    | ParameterValue::Channel4(_) => {}
                    // The dropdowns and the switches are not animatable.
                    ParameterValue::String(_) => assert!(
                        p.key == "mode",
                        "{type_key}.{} is a string but not a dropdown",
                        p.key
                    ),
                    ParameterValue::Bool(_) => assert_eq!(p.key, "invert"),
                    ref other => panic!("{type_key}.{} is {other:?}", p.key),
                }
            }
        }
    }

    /// An animated FX-2 parameter changes the picture between frames: the
    /// processor reads each one per frame under the right key.
    #[test]
    fn distort_parameters_animate_through_the_unified_channels() {
        use ravel_core::animation::Interpolation;
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::animation::curve::KeyframeCurve;

        // `(type_key, param, from, to, mode, fixed floats)`; Channel2 params key
        // both components. The fixed floats keep the animated parameter
        // visible (a blur angle is moot at length 0).
        type Case = (
            &'static str,
            &'static str,
            f32,
            f32,
            Option<&'static str>,
            &'static [(&'static str, f32)],
        );
        let cases: &[Case] = &[
            ("comp.directional_blur", "length", 0.0, 6.0, None, &[]),
            (
                "comp.directional_blur",
                "angle",
                0.0,
                90.0,
                None,
                &[("length", 6.0)],
            ),
            ("comp.radial_blur", "angle", 0.0, 60.0, Some("spin"), &[]),
            ("comp.radial_blur", "zoom", 0.0, 0.8, Some("zoom"), &[]),
            (
                "comp.radial_blur",
                "center",
                0.5,
                0.0,
                Some("spin"),
                &[("angle", 60.0)],
            ),
            ("comp.sharpen", "amount", 0.0, 3.0, None, &[]),
            ("comp.sharpen", "radius", 0.0, 3.0, None, &[("amount", 3.0)]),
            ("comp.warp", "amount", 0.0, 3.0, None, &[]),
            ("comp.lens_distortion", "amount", 0.0, 0.9, None, &[]),
            (
                "comp.lens_distortion",
                "center",
                0.5,
                0.0,
                None,
                &[("amount", 0.9)],
            ),
            ("comp.ripple", "amplitude", 0.0, 3.0, None, &[]),
            ("comp.ripple", "wavelength", 20.0, 3.0, None, &[]),
            ("comp.ripple", "phase", 0.0, 0.3, None, &[]),
            ("comp.ripple", "center", 0.5, 0.0, None, &[]),
            ("comp.tile", "columns", 1.0, 3.0, None, &[]),
            ("comp.tile", "rows", 1.0, 3.0, None, &[]),
            ("comp.key", "tolerance", 0.0, 1.0, None, &[]),
            (
                "comp.key",
                "softness",
                0.0,
                1.0,
                None,
                &[("tolerance", 0.5)],
            ),
            // Luma key: key luminance 0 -> 1 flips which checker squares go.
            (
                "comp.key",
                "key_color",
                0.0,
                1.0,
                Some("luma"),
                &[("tolerance", 0.45)],
            ),
        ];
        let at = |type_key: &str,
                  key: &str,
                  from: f32,
                  to: f32,
                  mode: Option<&str>,
                  fixed: &[(&str, f32)],
                  frame: u64|
         -> Vec<f32> {
            let gpu = GpuContext::new_blocking().expect("GPU required");
            let mut shaders = ShaderManager::new(gpu.clone());
            let mut reg = ravel_core::registry::NodeRegistry::new();
            builtin::register_builtins(&mut reg);
            // The source has structure (a checkerboard): every effect of a
            // uniform image is invisible. `comp.warp` gets a varying map too.
            let mut node = reg.create_node(type_key, NodeId::new(2)).unwrap();
            let keyed = |a: f32, b: f32| {
                let mut curve = KeyframeCurve::new();
                curve.insert(0, a, Interpolation::Linear);
                curve.insert(10, b, Interpolation::Linear);
                AnimationChannel::keyframes(curve)
            };
            let slot = node.parameters.iter_mut().find(|p| p.key == key).unwrap();
            slot.value = match slot.value {
                ParameterValue::Float(_) => ParameterValue::Channel(keyed(from, to)),
                ParameterValue::Channel2(_) => {
                    ParameterValue::Channel2([keyed(from, to), keyed(from, to)])
                }
                ParameterValue::Channel4(_) => ParameterValue::Channel4([
                    keyed(from, to),
                    keyed(from, to),
                    keyed(from, to),
                    keyed(from, to),
                ]),
                ref other => panic!("{type_key}.{key} is {other:?}"),
            };
            if let Some(mode) = mode {
                node.parameters
                    .iter_mut()
                    .find(|p| p.key == "mode")
                    .unwrap()
                    .value = ParameterValue::String(mode.into());
            }
            for (key, v) in fixed {
                node.parameters
                    .iter_mut()
                    .find(|p| p.key == *key)
                    .unwrap()
                    .value = ParameterValue::Float(*v);
            }
            let graph = Graph::new()
                .add_node(
                    Node::new(NodeId::new(1), "test.image")
                        .with_output("output", DataTypeId::FRAME_BUFFER),
                )
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
            let graph = if type_key == "comp.warp" {
                graph
                    .add_node(
                        Node::new(NodeId::new(3), "test.map")
                            .with_output("output", DataTypeId::FRAME_BUFFER),
                    )
                    .unwrap()
                    .add_edge(
                        EdgeId::new(2),
                        NodeId::new(3),
                        OutputPortIndex(0),
                        NodeId::new(2),
                        InputPortIndex(1),
                    )
                    .unwrap()
            } else {
                graph
            };
            let image = FrameBuffer::from_f32(
                8,
                8,
                (0..64)
                    .flat_map(|i| {
                        let v = if (i % 8 + i / 8) % 2 == 0 { 0.9 } else { 0.1 };
                        [v, 0.2 + v * 0.5, 0.3, 1.0]
                    })
                    .collect(),
            );
            let map = FrameBuffer::from_f32(
                8,
                8,
                (0..64)
                    .flat_map(|i| [(i % 8) as f32 / 7.0, (i / 8) as f32 / 7.0, 0.5, 1.0])
                    .collect(),
            );
            struct Fixed(FrameBuffer);
            impl ravel_core::eval::NodeProcessor for Fixed {
                fn process(
                    &self,
                    _node: &Node,
                    _ctx: &EvalContext,
                    _inputs: &[Option<Arc<dyn ravel_core::types::NodeData>>],
                    _params: &ravel_core::eval::ResolvedParams,
                    _scope: &mut dyn ravel_core::eval::EvalScope,
                ) -> anyhow::Result<Arc<dyn ravel_core::types::NodeData>> {
                    Ok(Arc::new(self.0.clone()))
                }
            }
            let mut ev = Evaluator::new();
            ev.register(NodeId::new(1), Arc::new(Fixed(image)));
            ev.register(NodeId::new(3), Arc::new(Fixed(map)));
            let pool = shared_texture_pool(&gpu);
            let frames = MediaFrameCache::standalone();
            register_all_processors(&mut ev, &graph, &gpu, &mut shaders, &pool, &frames);
            let out = ev
                .evaluate(
                    &graph,
                    NodeId::new(2),
                    &EvalContext::new(frame, FrameRate::new(30, 1), (8, 8)),
                )
                .unwrap();
            out.downcast_ref::<ravel_gpu::GpuFrameBuffer>()
                .expect("GPU-resident")
                .to_frame_buffer()
                .expect("readback")
                .as_f32()
                .to_vec()
        };
        for &(type_key, key, from, to, mode, fixed) in cases {
            let a = at(type_key, key, from, to, mode, fixed, 0);
            let b = at(type_key, key, from, to, mode, fixed, 10);
            assert!(
                a.iter().zip(&b).any(|(x, y)| (x - y).abs() > 1e-3),
                "{type_key}.{key} did not change the output between frames"
            );
        }
    }

    /// The dropdown values the mirror and key templates offer are exactly the
    /// ones their processors understand.
    #[test]
    fn comp_mirror_and_key_template_modes_are_all_known_to_the_processor() {
        let mut reg = ravel_core::registry::NodeRegistry::new();
        builtin::register_builtins(&mut reg);
        let offered = |type_key: &str| -> Vec<String> {
            reg.param_options(type_key, "mode")
                .expect("mode options")
                .to_vec()
        };
        assert_eq!(offered("comp.mirror"), builtin::COMP_MIRROR_MODES);
        assert_eq!(offered("comp.key"), builtin::COMP_KEY_MODES);
        for mode in offered("comp.mirror") {
            assert!(comp::comp_mirror_mode_is_known(&mode), "{mode}");
        }
        for mode in offered("comp.key") {
            assert!(comp::comp_key_mode_is_known(&mode), "{mode}");
        }
    }

    /// The dropdown values the radial blur template offers are exactly the
    /// ones the processor understands.
    #[test]
    fn comp_radial_blur_template_modes_are_all_known_to_the_processor() {
        let mut reg = ravel_core::registry::NodeRegistry::new();
        builtin::register_builtins(&mut reg);
        let offered = reg
            .param_options("comp.radial_blur", "mode")
            .expect("mode options");
        assert_eq!(
            offered.iter().map(String::as_str).collect::<Vec<_>>(),
            builtin::COMP_RADIAL_BLUR_MODES
        );
        for mode in offered {
            assert!(comp::radial_blur_mode_is_known(mode), "{mode}");
        }
        assert!(!comp::radial_blur_mode_is_known("no_such_mode"));
    }

    /// Every colour-adjustment node, built from the registry's own template,
    /// evaluates through `register_all_processors` and, with its default
    /// parameters, leaves a pixel unchanged (alpha included).
    #[test]
    fn every_grade_template_has_a_processor_and_defaults_to_the_identity() {
        for type_key in GRADE_NODES {
            let px = grade_pixel(type_key, None, &[], 0);
            for (got, want) in px.iter().zip([0.6, 0.3, 0.2, 1.0]) {
                assert!((got - want).abs() < 1e-6, "{type_key}: {px:?}");
            }
        }
    }

    /// Every animatable parameter of the colour-adjustment nodes rides the
    /// unified animation channel: keyframed, it changes the output between
    /// frame 0 and frame 10, so the processor reads it per frame under the
    /// right key. The `Curve` parameters are structural (their shape is not
    /// animatable), and the guard below keeps this table complete.
    #[test]
    fn grade_parameters_animate_through_the_unified_channels() {
        for &(type_key, key, from, to, fixed) in GRADE_ANIMATION {
            let a = grade_pixel(type_key, Some((key, from, to)), fixed, 0);
            let b = grade_pixel(type_key, Some((key, from, to)), fixed, 10);
            assert!(
                a.iter().zip(&b).any(|(x, y)| (x - y).abs() > 1e-3),
                "{type_key}.{key} did not change the output: {a:?} vs {b:?}"
            );
        }

        let mut reg = ravel_core::registry::NodeRegistry::new();
        builtin::register_builtins(&mut reg);
        for type_key in GRADE_NODES {
            for p in &reg.get(type_key).unwrap().default_params {
                match p.value {
                    ParameterValue::Curve(_) => {}
                    ParameterValue::Float(_)
                    | ParameterValue::Channel3(_)
                    | ParameterValue::Channel4(_) => assert!(
                        GRADE_ANIMATION
                            .iter()
                            .any(|c| c.0 == type_key && c.1 == p.key),
                        "{type_key}.{} is animatable but not covered above",
                        p.key
                    ),
                    ref other => panic!("{type_key}.{} is {other:?}", p.key),
                }
            }
        }
    }

    /// The dropdown values the template offers are exactly the ones the
    /// processor understands.
    #[test]
    fn comp_alpha_template_modes_are_all_known_to_the_processor() {
        let mut reg = ravel_core::registry::NodeRegistry::new();
        builtin::register_builtins(&mut reg);
        let offered = reg
            .param_options("comp.alpha", "mode")
            .expect("mode options");
        assert_eq!(
            offered.iter().map(String::as_str).collect::<Vec<_>>(),
            builtin::COMP_ALPHA_MODES
        );
        assert!(comp::comp_alpha_mode_is_known("invert"));
        for mode in offered {
            assert!(comp::comp_alpha_mode_is_known(mode), "{mode}");
        }
        assert!(!comp::comp_alpha_mode_is_known("no_such_mode"));
    }

    #[test]
    fn register_all_covers_constant() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());

        let node = Node::new(NodeId::new(1), "constant")
            .with_output("value", DataTypeId::SCALAR)
            .with_param("value", ParameterValue::Float(7.0));
        let graph = Graph::new().add_node(node).unwrap();

        let mut ev = Evaluator::new();
        let pool = shared_texture_pool(&gpu);
        let frames = MediaFrameCache::standalone();
        register_all_processors(&mut ev, &graph, &gpu, &mut shaders, &pool, &frames);

        let out = ev.evaluate(&graph, NodeId::new(1), &ctx()).unwrap();
        let s = out.downcast_ref::<Scalar>().unwrap();
        assert!((s.0 - 7.0).abs() < f32::EPSILON);
    }

    #[test]
    fn register_all_covers_gpu_nodes() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());

        // constant(0.5) feeds a FrameBuffer-producing chain is hard to test
        // without a FrameBuffer source. Instead test that color_correct registers
        // correctly by building: color_correct node.
        let cc_node = Node::new(NodeId::new(1), "color_correct")
            .with_input("image", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("brightness", ParameterValue::Float(0.0))
            .with_param("contrast", ParameterValue::Float(1.0))
            .with_param("saturation", ParameterValue::Float(1.0));
        let graph = Graph::new().add_node(cc_node).unwrap();

        let mut ev = Evaluator::new();
        let pool = shared_texture_pool(&gpu);
        let frames = MediaFrameCache::standalone();
        register_all_processors(&mut ev, &graph, &gpu, &mut shaders, &pool, &frames);

        // Processor is registered → is_dirty == true.
        assert!(ev.is_dirty(NodeId::new(1)));
    }

    /// The shell compiler marks the nodes it inserts `synthetic`, and a
    /// rasterize node that carries the flag used to be handed the CPU
    /// reference implementation. Both kinds now stay resident, so a
    /// composition previewed through the shell chain never reads a frame back
    /// just to hand it to the next GPU node.
    #[test]
    fn processor_factory_selects_gpu_for_every_rasterize_node() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let pool = shared_texture_pool(&gpu);
        let frames = MediaFrameCache::standalone();
        let mut shaders = ShaderManager::new(gpu.clone());
        let node = Node::new(NodeId::new(1), "rasterize");
        let mut scope = Evaluator::new();
        let geo: Arc<dyn ravel_core::types::NodeData> = Arc::new(Geometry::new());
        let processor = processor_for_node(&node, &gpu, &mut shaders, &pool, &frames).unwrap();
        let out = processor
            .process(
                &node,
                &ctx(),
                &[Some(geo.clone())],
                &ravel_core::eval::ResolvedParams::default(),
                &mut scope,
            )
            .unwrap();
        assert!(out.downcast_ref::<ravel_gpu::GpuFrameBuffer>().is_some());

        let mut synthetic = node.clone();
        synthetic.metadata.synthetic = true;
        let processor = processor_for_node(&synthetic, &gpu, &mut shaders, &pool, &frames).unwrap();
        let out = processor
            .process(
                &synthetic,
                &ctx(),
                &[Some(geo)],
                &ravel_core::eval::ResolvedParams::default(),
                &mut scope,
            )
            .unwrap();
        assert!(out.downcast_ref::<ravel_gpu::GpuFrameBuffer>().is_some());
    }

    #[test]
    fn unknown_type_key_skipped_silently() {
        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());

        let node =
            Node::new(NodeId::new(1), "unknown_plugin_node").with_output("out", DataTypeId::SCALAR);
        let graph = Graph::new().add_node(node).unwrap();

        let mut ev = Evaluator::new();
        let pool = shared_texture_pool(&gpu);
        let frames = MediaFrameCache::standalone();
        register_all_processors(&mut ev, &graph, &gpu, &mut shaders, &pool, &frames);

        // No processor registered → is_dirty returns false (not in dirty set).
        assert!(!ev.is_dirty(NodeId::new(1)));
    }

    #[test]
    fn integration_merge_two_constants_through_color_correct() {
        // Graph:
        //  const_a(value=0.3) → A \
        //                            merge(over) → color_correct(brightness=0.1)
        //  const_b(value=0.6) → B /
        //
        // Constants output Scalar, but merge expects FrameBuffer. To test the full
        // pipeline E2E, we build a simpler graph: two color_correct nodes feeding
        // into merge.

        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());

        // We'll manually provide FrameBuffer inputs and test the chain:
        // color_correct(identity) → merge(add)

        let cc_a = Node::new(NodeId::new(1), "color_correct")
            .with_input("image", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("brightness", ParameterValue::Float(0.0))
            .with_param("contrast", ParameterValue::Float(1.0))
            .with_param("saturation", ParameterValue::Float(1.0));

        let cc_b = Node::new(NodeId::new(2), "color_correct")
            .with_input("image", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("brightness", ParameterValue::Float(0.0))
            .with_param("contrast", ParameterValue::Float(1.0))
            .with_param("saturation", ParameterValue::Float(1.0));

        let merge = Node::new(NodeId::new(3), "merge")
            .with_input("A", &[DataTypeId::FRAME_BUFFER])
            .with_input("B", &[DataTypeId::FRAME_BUFFER])
            .with_output("output", DataTypeId::FRAME_BUFFER)
            .with_param("operation", ParameterValue::String("add".into()))
            .with_param("mix", ParameterValue::Float(1.0));

        let graph = Graph::new()
            .add_node(cc_a)
            .unwrap()
            .add_node(cc_b)
            .unwrap()
            .add_node(merge)
            .unwrap()
            .add_edge(
                EdgeId::new(1),
                NodeId::new(1),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(2),
                NodeId::new(2),
                OutputPortIndex(0),
                NodeId::new(3),
                InputPortIndex(1),
            )
            .unwrap();

        let mut ev = Evaluator::new();
        let pool = shared_texture_pool(&gpu);
        let frames = MediaFrameCache::standalone();
        register_all_processors(&mut ev, &graph, &gpu, &mut shaders, &pool, &frames);

        // color_correct nodes have no upstream inputs, so we need to provide them
        // manually. For a true E2E test with FrameBuffer sources we'd need a
        // "generate" node. Instead, directly register stub processors that emit
        // solid FrameBuffers.
        struct FbSource(FrameBuffer);
        impl ravel_core::eval::NodeProcessor for FbSource {
            fn process(
                &self,
                _node: &Node,
                _ctx: &EvalContext,
                _inputs: &[Option<Arc<dyn ravel_core::types::NodeData>>],
                _params: &ravel_core::eval::ResolvedParams,
                _scope: &mut dyn ravel_core::eval::EvalScope,
            ) -> anyhow::Result<Arc<dyn ravel_core::types::NodeData>> {
                Ok(Arc::new(self.0.clone()))
            }
        }

        ev.register(
            NodeId::new(1),
            Arc::new(FbSource(solid_fb(4, 4, 0.3, 0.0, 0.0, 1.0))),
        );
        ev.register(
            NodeId::new(2),
            Arc::new(FbSource(solid_fb(4, 4, 0.0, 0.5, 0.0, 1.0))),
        );

        let out = ev.evaluate(&graph, NodeId::new(3), &ctx()).unwrap();
        let fb = out
            .downcast_ref::<ravel_gpu::GpuFrameBuffer>()
            .expect("merge output stays GPU-resident")
            .to_frame_buffer()
            .unwrap();

        assert_eq!(fb.width, 4);
        assert_eq!(fb.height, 4);
        // add mode: (0.3, 0.0, 0.0) + (0.0, 0.5, 0.0) = (0.3, 0.5, 0.0)
        assert!((fb.as_f32()[0] - 0.3).abs() < 0.02, "r={}", fb.as_f32()[0]);
        assert!((fb.as_f32()[1] - 0.5).abs() < 0.02, "g={}", fb.as_f32()[1]);
        assert!(fb.as_f32()[2] < 0.02, "b={}", fb.as_f32()[2]);
    }

    /// The generators (no input) and the stylize nodes (one image input) of
    /// the effects library.
    const GENERATE_NODES: [&str; 4] = [
        "comp.gradient",
        "comp.noise",
        "comp.fractal",
        "comp.checkerboard",
    ];
    const STYLIZE_NODES: [&str; 4] = [
        "comp.glow",
        "comp.drop_shadow",
        "comp.stroke",
        "comp.emboss",
    ];

    /// Evaluate `type_key` (built from the registry's template) over a fixed
    /// 8 x 8 test image at `frame`, with `animated` keyframed from `from` at
    /// frame 0 to `to` at frame 10 and the other values set as given. A
    /// generator ignores the image.
    fn fx_output(
        type_key: &str,
        animated: Option<(&str, f32, f32)>,
        floats: &[(&str, f32)],
        strings: &[(&str, &str)],
        frame: u64,
    ) -> Vec<f32> {
        use ravel_core::animation::Interpolation;
        use ravel_core::animation::channel::AnimationChannel;
        use ravel_core::animation::curve::KeyframeCurve;

        let gpu = GpuContext::new_blocking().expect("GPU required");
        let mut shaders = ShaderManager::new(gpu.clone());
        let mut reg = ravel_core::registry::NodeRegistry::new();
        builtin::register_builtins(&mut reg);
        let mut node = reg.create_node(type_key, NodeId::new(2)).unwrap();
        fn slot<'a>(node: &'a mut Node, key: &str) -> &'a mut ravel_core::graph::Parameter {
            node.parameters
                .iter_mut()
                .find(|p| p.key == key)
                .unwrap_or_else(|| panic!("{} has no {key}", node.type_key))
        }
        // A two-component parameter takes the number on both components.
        for (key, v) in floats {
            let slot = slot(&mut node, key);
            slot.value = match slot.value {
                ParameterValue::Channel2(_) => ParameterValue::vec2(*v, *v),
                _ => ParameterValue::Float(*v),
            };
        }
        for (key, v) in strings {
            slot(&mut node, key).value = ParameterValue::String((*v).into());
        }
        if let Some((key, from, to)) = animated {
            let keyed = || {
                let mut curve = KeyframeCurve::new();
                curve.insert(0, from, Interpolation::Linear);
                curve.insert(10, to, Interpolation::Linear);
                AnimationChannel::keyframes(curve)
            };
            let slot = slot(&mut node, key);
            slot.value = match slot.value {
                ParameterValue::Float(_) => ParameterValue::Channel(keyed()),
                ParameterValue::Channel2(_) => ParameterValue::Channel2([keyed(), keyed()]),
                ParameterValue::Channel4(_) => {
                    ParameterValue::Channel4([keyed(), keyed(), keyed(), keyed()])
                }
                ref other => panic!("{type_key}.{key} is {other:?}, not animatable"),
            };
        }
        let mut graph = Graph::new().add_node(node).unwrap();
        // A checker of opaque coloured and fully transparent pixels: every
        // stylize node has an edge and a coverage to work with.
        let image = FrameBuffer::from_f32(
            8,
            8,
            (0..64)
                .flat_map(|i| {
                    if (i % 8 + i / 8) % 2 == 0 {
                        [0.9, 0.6, 0.3, 1.0]
                    } else {
                        [0.0; 4]
                    }
                })
                .collect(),
        );
        struct Fixed(FrameBuffer);
        impl ravel_core::eval::NodeProcessor for Fixed {
            fn process(
                &self,
                _node: &Node,
                _ctx: &EvalContext,
                _inputs: &[Option<Arc<dyn ravel_core::types::NodeData>>],
                _params: &ravel_core::eval::ResolvedParams,
                _scope: &mut dyn ravel_core::eval::EvalScope,
            ) -> anyhow::Result<Arc<dyn ravel_core::types::NodeData>> {
                Ok(Arc::new(self.0.clone()))
            }
        }
        if !GENERATE_NODES.contains(&type_key) {
            graph = graph
                .add_node(
                    Node::new(NodeId::new(1), "test.image")
                        .with_output("output", DataTypeId::FRAME_BUFFER),
                )
                .unwrap()
                .add_edge(
                    EdgeId::new(1),
                    NodeId::new(1),
                    OutputPortIndex(0),
                    NodeId::new(2),
                    InputPortIndex(0),
                )
                .unwrap();
        }
        let mut ev = Evaluator::new();
        ev.register(NodeId::new(1), Arc::new(Fixed(image)));
        let pool = shared_texture_pool(&gpu);
        let frames = MediaFrameCache::standalone();
        register_all_processors(&mut ev, &graph, &gpu, &mut shaders, &pool, &frames);
        let out = ev
            .evaluate(
                &graph,
                NodeId::new(2),
                &EvalContext::new(frame, FrameRate::new(30, 1), (8, 8)),
            )
            .unwrap();
        out.downcast_ref::<ravel_gpu::GpuFrameBuffer>()
            .unwrap_or_else(|| panic!("{type_key} stays GPU-resident"))
            .to_frame_buffer()
            .expect("readback")
            .as_f32()
            .to_vec()
    }

    /// Every generator and stylize template has a processor under its own key
    /// and evaluates from the registry's defaults, to a frame of the
    /// evaluation resolution.
    #[test]
    fn every_generate_and_stylize_template_evaluates() {
        for type_key in GENERATE_NODES.iter().chain(&STYLIZE_NODES) {
            let out = fx_output(type_key, None, &[], &[], 0);
            assert_eq!(out.len(), 8 * 8 * 4, "{type_key}");
            assert!(out.iter().all(|v| v.is_finite()), "{type_key}");
            assert!(out.iter().any(|v| *v != 0.0), "{type_key} drew nothing");
        }
    }

    /// Every numeric parameter is a unified animation channel; the ramp is the
    /// structural parameter every ramp consumer shares; each string is a
    /// dropdown whose values the processor understands.
    #[test]
    fn generate_and_stylize_parameters_are_animation_channels() {
        let mut reg = ravel_core::registry::NodeRegistry::new();
        builtin::register_builtins(&mut reg);
        for type_key in GENERATE_NODES.iter().chain(&STYLIZE_NODES) {
            for p in &reg.get(type_key).unwrap().default_params {
                match p.value {
                    ParameterValue::Float(_)
                    | ParameterValue::Channel2(_)
                    | ParameterValue::Channel4(_) => {}
                    ParameterValue::Ramp(_) => assert_eq!(p.key, "stops"),
                    ParameterValue::String(_) => assert!(
                        reg.param_options(type_key, &p.key).is_some(),
                        "{type_key}.{} is a string but not a dropdown",
                        p.key
                    ),
                    ref other => panic!("{type_key}.{} is {other:?}", p.key),
                }
            }
        }
        let offered = |type_key: &str, key: &str| -> Vec<String> {
            reg.param_options(type_key, key).expect("options").to_vec()
        };
        assert_eq!(
            offered("comp.gradient", "type"),
            builtin::COMP_GRADIENT_TYPES
        );
        assert_eq!(offered("comp.fractal", "type"), builtin::COMP_FRACTAL_TYPES);
        assert_eq!(
            offered("comp.stroke", "position"),
            builtin::COMP_STROKE_POSITIONS
        );
        for t in offered("comp.gradient", "type") {
            assert!(comp::comp_gradient_type_is_known(&t), "{t}");
        }
        for t in offered("comp.fractal", "type") {
            assert!(comp::comp_fractal_type_is_known(&t), "{t}");
        }
        for t in offered("comp.stroke", "position") {
            assert!(comp::comp_stroke_position_is_known(&t), "{t}");
        }
    }

    /// Every animatable parameter of the generators and stylize nodes changes
    /// the picture between frame 0 and frame 10: the processor reads it per
    /// frame under the right key.
    #[test]
    fn generate_and_stylize_parameters_animate_through_the_unified_channels() {
        type Case = (
            &'static str,
            &'static str,
            f32,
            f32,
            &'static [(&'static str, f32)],
            &'static [(&'static str, &'static str)],
        );
        let cases: &[Case] = &[
            ("comp.gradient", "start", 0.0, 0.4, &[], &[]),
            ("comp.gradient", "end", 1.0, 0.6, &[], &[]),
            ("comp.noise", "scale", 40.0, 10.0, &[], &[]),
            ("comp.noise", "octaves", 1.0, 4.0, &[], &[]),
            (
                "comp.noise",
                "roughness",
                0.1,
                0.9,
                &[("octaves", 4.0)],
                &[],
            ),
            ("comp.noise", "seed", 0.0, 5.0, &[], &[]),
            ("comp.noise", "offset", 0.0, 20.0, &[], &[]),
            ("comp.noise", "color_a", 0.0, 0.5, &[], &[]),
            ("comp.noise", "color_b", 1.0, 0.5, &[], &[]),
            ("comp.fractal", "center", -0.5, 0.2, &[], &[]),
            ("comp.fractal", "zoom", 1.0, 4.0, &[], &[]),
            ("comp.fractal", "iterations", 3.0, 60.0, &[], &[]),
            (
                "comp.fractal",
                "julia",
                -0.8,
                0.3,
                &[],
                &[("type", "julia")],
            ),
            ("comp.fractal", "inside", 0.0, 1.0, &[], &[]),
            ("comp.checkerboard", "size", 2.0, 3.0, &[], &[]),
            ("comp.checkerboard", "offset", 0.0, 1.0, &[], &[]),
            ("comp.checkerboard", "color_a", 1.0, 0.0, &[], &[]),
            (
                "comp.checkerboard",
                "color_b",
                0.0,
                1.0,
                &[("size", 2.0)],
                &[],
            ),
            ("comp.glow", "radius", 0.0, 4.0, &[], &[]),
            ("comp.glow", "intensity", 0.0, 2.0, &[], &[]),
            ("comp.glow", "color", 1.0, 0.0, &[], &[]),
            ("comp.drop_shadow", "color", 0.0, 1.0, &[], &[]),
            ("comp.drop_shadow", "opacity", 0.0, 1.0, &[], &[]),
            ("comp.drop_shadow", "offset", 0.0, 2.0, &[], &[]),
            ("comp.drop_shadow", "softness", 0.0, 3.0, &[], &[]),
            ("comp.stroke", "color", 1.0, 0.0, &[("width", 1.0)], &[]),
            ("comp.stroke", "width", 0.0, 2.0, &[], &[]),
            ("comp.emboss", "angle", 0.0, 180.0, &[], &[]),
            ("comp.emboss", "amount", 0.0, 2.0, &[], &[]),
            ("comp.emboss", "distance", 1.0, 2.0, &[], &[]),
        ];
        // Every numeric parameter is covered by a case.
        let mut reg = ravel_core::registry::NodeRegistry::new();
        builtin::register_builtins(&mut reg);
        for type_key in GENERATE_NODES.iter().chain(&STYLIZE_NODES) {
            for p in &reg.get(type_key).unwrap().default_params {
                if matches!(
                    p.value,
                    ParameterValue::Float(_)
                        | ParameterValue::Channel2(_)
                        | ParameterValue::Channel4(_)
                ) {
                    assert!(
                        cases.iter().any(|c| c.0 == *type_key && c.1 == p.key),
                        "{type_key}.{} has no animation case",
                        p.key
                    );
                }
            }
        }
        for &(type_key, key, from, to, floats, strings) in cases {
            let at = |frame| fx_output(type_key, Some((key, from, to)), floats, strings, frame);
            let (a, b) = (at(0), at(10));
            assert!(
                a.iter().zip(&b).any(|(x, y)| (x - y).abs() > 1e-3),
                "{type_key}.{key} did not change the output between frames"
            );
        }
    }
}
