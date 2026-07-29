// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Phase 2 completion tests (`eval-render-performance-plan.md`): GPU node
//! chains keep intermediates resident in VRAM with zero CPU readbacks, and
//! the resident path is pixel-equivalent to staging through the CPU between
//! nodes. Requires a GPU adapter; tests skip gracefully without one.

use ravel_core::animation::channel::AnimationChannel;
use ravel_core::composition::compile::compile_composition;
use ravel_core::composition::{BlendMode, Composition, Document, Layer};
use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{
    CompId, DataTypeId, EdgeId, InputPortIndex, LayerId, NodeId, OutputPortIndex,
};
use ravel_core::network as net;
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::types::{FrameBuffer, FrameRate, NodeData};
use ravel_gpu::{GpuContext, GpuFrameBuffer, ShaderManager};
use ravel_nodes::{register_all_processors, shared_texture_pool};
use std::sync::Arc;

const SRC: u64 = 1;
const BLUR: u64 = 2;
const CC: u64 = 3;
const MERGE: u64 = 4;

fn nid(raw: u64) -> NodeId {
    NodeId::new(raw)
}

fn ctx() -> EvalContext {
    EvalContext::new(0, FrameRate::new(30, 1), (32, 32))
}

struct FbSource(FrameBuffer);

impl NodeProcessor for FbSource {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ravel_core::eval::ResolvedParams,
        _scope: &mut dyn ravel_core::eval::EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        Ok(Arc::new(self.0.clone()))
    }
}

fn gradient_fb(width: u32, height: u32) -> FrameBuffer {
    let mut data = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            data.extend_from_slice(&[x as f32 / width as f32, y as f32 / height as f32, 0.5, 1.0]);
        }
    }
    FrameBuffer {
        width,
        height,
        data: Arc::from(data),
    }
}

fn solid_fb(width: u32, height: u32, rgba: [f32; 4]) -> FrameBuffer {
    FrameBuffer {
        width,
        height,
        data: Arc::from(rgba.repeat((width * height) as usize)),
    }
}

/// source → blur → color_correct → merge.A, source → merge.B
fn effect_graph() -> Graph {
    let source = Node::new(nid(SRC), "test.source").with_output("output", DataTypeId::FRAME_BUFFER);
    let blur = Node::new(nid(BLUR), "blur")
        .with_input("image", &[DataTypeId::FRAME_BUFFER])
        .with_output("output", DataTypeId::FRAME_BUFFER)
        .with_param("radius", ParameterValue::Float(2.0));
    let cc = Node::new(nid(CC), "color_correct")
        .with_input("image", &[DataTypeId::FRAME_BUFFER])
        .with_output("output", DataTypeId::FRAME_BUFFER)
        .with_param("brightness", ParameterValue::Float(0.1))
        .with_param("contrast", ParameterValue::Float(1.1))
        .with_param("saturation", ParameterValue::Float(0.9));
    let merge = Node::new(nid(MERGE), "merge")
        .with_input("A", &[DataTypeId::FRAME_BUFFER])
        .with_input("B", &[DataTypeId::FRAME_BUFFER])
        .with_output("output", DataTypeId::FRAME_BUFFER)
        .with_param("operation", ParameterValue::String("over".into()))
        .with_param("mix", ParameterValue::Float(1.0));

    Graph::new()
        .add_node(source)
        .unwrap()
        .add_node(blur)
        .unwrap()
        .add_node(cc)
        .unwrap()
        .add_node(merge)
        .unwrap()
        .add_edge(
            EdgeId::new(1),
            nid(SRC),
            OutputPortIndex(0),
            nid(BLUR),
            InputPortIndex(0),
        )
        .unwrap()
        .add_edge(
            EdgeId::new(2),
            nid(BLUR),
            OutputPortIndex(0),
            nid(CC),
            InputPortIndex(0),
        )
        .unwrap()
        .add_edge(
            EdgeId::new(3),
            nid(CC),
            OutputPortIndex(0),
            nid(MERGE),
            InputPortIndex(0),
        )
        .unwrap()
        .add_edge(
            EdgeId::new(4),
            nid(SRC),
            OutputPortIndex(0),
            nid(MERGE),
            InputPortIndex(1),
        )
        .unwrap()
}

#[test]
fn gpu_chain_evaluates_with_zero_intermediate_readbacks() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = shared_texture_pool(&gpu);
    let graph = effect_graph();

    let mut evaluator = Evaluator::new();
    register_all_processors(&mut evaluator, &graph, &gpu, &mut shaders, &pool);
    evaluator.register(nid(SRC), Arc::new(FbSource(gradient_fb(32, 32))));

    let before = gpu.transfer_stats();
    let out = evaluator.evaluate(&graph, nid(MERGE), &ctx()).unwrap();
    let delta = before.delta(&gpu.transfer_stats());

    // The CPU source is uploaded where it enters the GPU chain (blur, and
    // merge input B); every intermediate stays resident.
    assert_eq!(delta.readbacks, 0, "no intermediate readbacks: {delta:?}");
    assert_eq!(delta.uploads, 2, "source uploads only: {delta:?}");

    // The chain output is a GPU handle; displaying it costs exactly one
    // readback.
    let frame = out
        .downcast_ref::<GpuFrameBuffer>()
        .expect("merge output stays GPU-resident");
    let before = gpu.transfer_stats();
    let fb = frame.to_frame_buffer().unwrap();
    let delta = before.delta(&gpu.transfer_stats());
    assert_eq!(delta.readbacks, 1);
    assert_eq!(fb.width, 32);
    assert!(fb.data.iter().any(|v| *v > 0.0), "non-empty output");
}

#[test]
fn resident_path_matches_cpu_staged_path() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = shared_texture_pool(&gpu);

    let blur_node = Node::new(nid(BLUR), "blur").with_param("radius", ParameterValue::Float(3.0));
    let cc_node = Node::new(nid(CC), "color_correct")
        .with_param("brightness", ParameterValue::Float(0.2))
        .with_param("contrast", ParameterValue::Float(1.2))
        .with_param("saturation", ParameterValue::Float(0.8));
    let blur =
        ravel_nodes::blur::BlurProcessor::new(gpu.clone(), &mut shaders, pool.clone(), &blur_node);
    let cc = ravel_nodes::color_correct::ColorCorrectProcessor::new(
        gpu.clone(),
        &mut shaders,
        pool.clone(),
        &cc_node,
    );

    let source = gradient_fb(16, 16);
    let ctx = ctx();
    let params = ravel_core::eval::ResolvedParams::default();
    let mut scope = ravel_core::eval::Evaluator::new();

    // Resident: blur → cc with the intermediate staying in VRAM.
    let blurred = blur
        .process(
            &blur_node,
            &ctx,
            &[Some(Arc::new(source))],
            &params,
            &mut scope,
        )
        .unwrap();
    let corrected = cc
        .process(
            &cc_node,
            &ctx,
            &[Some(blurred.clone())],
            &params,
            &mut scope,
        )
        .unwrap();
    let resident = corrected
        .downcast_ref::<GpuFrameBuffer>()
        .unwrap()
        .to_frame_buffer()
        .unwrap();

    // Staged: read the blur result back to the CPU and re-upload it.
    let blurred_cpu = blurred
        .downcast_ref::<GpuFrameBuffer>()
        .unwrap()
        .to_frame_buffer()
        .unwrap();
    let corrected_staged = cc
        .process(
            &cc_node,
            &ctx,
            &[Some(Arc::new(blurred_cpu))],
            &params,
            &mut scope,
        )
        .unwrap();
    let staged = corrected_staged
        .downcast_ref::<GpuFrameBuffer>()
        .unwrap()
        .to_frame_buffer()
        .unwrap();

    assert_eq!(resident.data.len(), staged.data.len());
    for (i, (a, b)) in resident.data.iter().zip(staged.data.iter()).enumerate() {
        assert!(
            (a - b).abs() < 1e-5,
            "pixel component {i} differs: resident={a}, staged={b}"
        );
    }
}

fn shell_composition(layers: usize) -> (Graph, NodeId, Arc<Document>, Vec<NodeId>) {
    let mut registry = NodeRegistry::new();
    register_builtins(&mut registry);
    let modes = [
        BlendMode::Normal,
        BlendMode::Add,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
    ];
    let mut composition = Composition::new(
        CompId::new(1),
        "GPU shell regression",
        (32, 32),
        FrameRate::new(30, 1),
        30,
    );
    let mut sources = Vec::with_capacity(layers);

    for index in 0..layers {
        let base = 10_000 + index as u64 * 10;
        let source_id = nid(base);
        sources.push(source_id);
        let source =
            Node::new(source_id, "test.source").with_output("output", DataTypeId::FRAME_BUFFER);
        let blur = registry.create_node("blur", nid(base + 1)).unwrap();
        let out = Node::new(nid(base + 2), net::NET_OUT_TYPE_KEY)
            .with_input(net::PORT_FRAME, &[DataTypeId::FRAME_BUFFER]);
        let network = Graph::new()
            .add_node(source)
            .unwrap()
            .add_node(blur)
            .unwrap()
            .add_node(out)
            .unwrap()
            .add_edge(
                EdgeId::new(base),
                source_id,
                OutputPortIndex(0),
                nid(base + 1),
                InputPortIndex(0),
            )
            .unwrap()
            .add_edge(
                EdgeId::new(base + 1),
                nid(base + 1),
                OutputPortIndex(0),
                nid(base + 2),
                InputPortIndex(0),
            )
            .unwrap();
        let mut layer = Layer::new(
            LayerId::new(index as u64 + 1),
            format!("Layer {index}"),
            network,
        )
        .with_time(0, 0, 30)
        .with_blend_mode(modes[index % modes.len()]);
        layer.transform.position[0] = AnimationChannel::constant(1.0 + index as f32);
        layer.opacity = AnimationChannel::constant(0.8);
        composition = composition.add_layer(layer);
    }

    let compiled = compile_composition(&composition, Graph::new()).unwrap();
    let document = Arc::new(Document::default().with_composition(composition));
    (compiled.graph, compiled.output_node, document, sources)
}

fn evaluate_shell_chain(
    graph: &Graph,
    output: NodeId,
    document: Arc<Document>,
    sources: &[NodeId],
    gpu: &GpuContext,
    cpu_shell: bool,
) -> Arc<dyn NodeData> {
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = shared_texture_pool(gpu);
    let mut evaluator = Evaluator::new();
    register_all_processors(&mut evaluator, graph, gpu, &mut shaders, &pool);
    for composition in document.compositions.values() {
        for layer in &composition.layers {
            register_all_processors(&mut evaluator, &layer.network, gpu, &mut shaders, &pool);
        }
    }
    if cpu_shell {
        for node in graph.nodes() {
            let processor: Option<Arc<dyn NodeProcessor>> = match node.type_key.as_str() {
                "comp.transform" => Some(Arc::new(
                    ravel_nodes::comp::CompTransformProcessor::from_node(node),
                )),
                "comp.opacity" => Some(Arc::new(
                    ravel_nodes::comp::CompOpacityProcessor::from_node(node),
                )),
                key if key.starts_with("comp.merge.") => Some(Arc::new(
                    ravel_nodes::comp::CompMergeProcessor::from_node(node),
                )),
                _ => None,
            };
            if let Some(processor) = processor {
                evaluator.register(node.id, processor);
            }
        }
    }
    for (index, source) in sources.iter().enumerate() {
        evaluator.register(
            *source,
            Arc::new(FbSource(solid_fb(
                32,
                32,
                [0.05 * index as f32, 0.2, 0.4, 0.7],
            ))),
        );
    }
    evaluator.set_document(document);
    evaluator.evaluate(graph, output, &ctx()).unwrap()
}

#[test]
fn ten_layer_shell_chain_has_no_intermediate_readbacks_and_matches_cpu() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let (graph, output, document, sources) = shell_composition(10);

    let before = gpu.transfer_stats();
    let gpu_out = evaluate_shell_chain(&graph, output, document.clone(), &sources, &gpu, false);
    let resident_delta = before.delta(&gpu.transfer_stats());
    assert_eq!(
        resident_delta.readbacks, 0,
        "shell intermediates stay resident: {resident_delta:?}"
    );
    let gpu_frame = gpu_out.downcast_ref::<GpuFrameBuffer>().unwrap();
    let before = gpu.transfer_stats();
    let gpu_pixels = gpu_frame.to_frame_buffer().unwrap();
    assert_eq!(
        before.delta(&gpu.transfer_stats()).readbacks,
        1,
        "final display readback only"
    );

    let cpu_out = evaluate_shell_chain(&graph, output, document, &sources, &gpu, true);
    let cpu_pixels = ravel_nodes::ensure_cpu(cpu_out.as_ref()).unwrap();
    assert_eq!(gpu_pixels.data.len(), cpu_pixels.data.len());
    for (index, (gpu_value, cpu_value)) in gpu_pixels
        .data
        .iter()
        .zip(cpu_pixels.data.iter())
        .enumerate()
    {
        assert!(
            (gpu_value - cpu_value).abs() < 2e-4,
            "component {index}: gpu={gpu_value}, cpu={cpu_value}"
        );
    }
}

#[test]
fn dropping_cached_results_returns_textures_to_the_pool() {
    let Ok(gpu) = GpuContext::new_blocking() else {
        eprintln!("skipping: no GPU adapter available");
        return;
    };
    let mut shaders = ShaderManager::new(gpu.clone());
    let pool = shared_texture_pool(&gpu);
    let graph = effect_graph();

    let mut evaluator = Evaluator::new();
    register_all_processors(&mut evaluator, &graph, &gpu, &mut shaders, &pool);
    evaluator.register(nid(SRC), Arc::new(FbSource(gradient_fb(32, 32))));
    let out = evaluator.evaluate(&graph, nid(MERGE), &ctx()).unwrap();

    // Cached results (and the returned handle) hold pool textures; dropping
    // both must return every resident texture to the pool for reuse.
    drop(out);
    let idle_before = pool.lock().unwrap().idle_count();
    evaluator.invalidate_all();
    let idle_after = pool.lock().unwrap().idle_count();
    assert!(
        idle_after > idle_before,
        "cache invalidation must release resident textures ({idle_before} -> {idle_after})"
    );
}
