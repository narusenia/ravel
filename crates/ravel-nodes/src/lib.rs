// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Built-in node processors for the Ravel DAG evaluation pipeline.
//!
//! Each module implements [`ravel_core::eval::NodeProcessor`] for one of the
//! registered built-in node types. GPU-accelerated processors use
//! [`ravel_gpu`] for shader compilation and texture management.

pub mod attribute;
pub mod blur;
pub mod color_correct;
pub mod comp;
pub mod constant;
pub mod display;
pub use display::{DisplayFrame, DisplayTransform};
pub mod eval_hooks;
pub use eval_hooks::GpuEvalHooks;
pub mod field;
pub mod flatten;
pub mod geometry;
mod gpu_util;
pub use gpu_util::{GpuImage, begin_upload_scope, clone_frame_value, ensure_cpu, ensure_gpu};
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
pub mod transform;
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
pub fn processor_for_node(
    node: &Node,
    ctx: &GpuContext,
    shaders: &mut ShaderManager,
    pool: &Arc<Mutex<TexturePool>>,
    media_frames: &MediaFrameCache,
) -> Option<Arc<dyn ravel_core::eval::NodeProcessor>> {
    let processor: Option<Arc<dyn ravel_core::eval::NodeProcessor>> = match node.type_key.as_str() {
        "attribute.set" => Some(Arc::new(attribute::AttributeSetProcessor::from_node(node))),
        "attribute.promote" => Some(Arc::new(attribute::AttributePromoteProcessor::from_node(
            node,
        ))),
        "attribute.transfer" => Some(Arc::new(attribute::AttributeTransferProcessor::from_node(
            node,
        ))),
        "attribute.path_sample" => Some(Arc::new(attribute::PathSampleProcessor::from_node(node))),
        "attribute.curveu" => Some(Arc::new(attribute::CurveUProcessor::from_node(node))),
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
        "geometry.from_image" => Some(Arc::new(geometry::GeometryFromImageProcessor::from_node(
            node,
        ))),
        "scene.add" => Some(Arc::new(scene::SceneAddProcessor::from_node(node))),
        "scene.merge" => Some(Arc::new(scene::SceneMergeProcessor::from_node(node))),
        "scene.camera" => Some(Arc::new(scene::SceneCameraProcessor::from_node(node))),
        "field.noise" => Some(Arc::new(field::NoiseFieldProcessor::from_node(node))),
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
        // Cross-layer reference (REQ-LAYER-005)
        "layer.ref" => Some(Arc::new(layer_ref::LayerRefProcessor::from_node(node))),
        // Nested network (REQ-LAYER-003)
        "subnet" => Some(Arc::new(subnet::SubnetProcessor::from_node(node))),
        // Network interface nodes
        "net.in" => Some(Arc::new(net::NetInProcessor::from_node(node))),
        "net.out" => Some(Arc::new(net::NetOutProcessor::from_node(node))),
        _ => None,
    };
    processor
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
}
