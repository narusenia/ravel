// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Scene assembly (CPU-only): `scene.add`, `scene.merge`, and
//! `scene.camera`.
//!
//! These processors build the [`Scene`] value the renderer consumes; none of
//! them draws anything and none of them touches a geometry's `P` attribute.
//! Projection happens inside `scene.render`, so a point keeps exactly one
//! position (`docs/specifications/procedural-geometry.md`).

use anyhow::{Context as _, bail};
use ravel_core::eval::{EvalContext, EvalScope, NodeProcessor, ResolvedParams};
use ravel_core::geometry::Geometry;
use ravel_core::graph::Node;
use ravel_core::id::DataTypeId;
use ravel_core::scene::camera::{PROJECTION_ORTHOGRAPHIC, PROJECTION_PERSPECTIVE};
use ravel_core::scene::{Camera, Projection, Scene, SceneContent, Transform3D};
use ravel_core::types::NodeData;
use std::sync::Arc;

/// Wrap an evaluated input value as scene content.
///
/// A geometry and a scene are cloned into an `Arc`: both are copy-on-write
/// containers (attribute columns and the object list are already shared), so
/// this copies a handful of handles rather than any payload.
///
/// A frame buffer is rejected rather than wrapped. The `object` port does not
/// declare `FRAME_BUFFER`, so the editor refuses the edge in the first place;
/// this arm answers the value that still arrives — a project written by a
/// build that accepted one, or a port widened by a future edit — with the
/// route to take instead of a silent pass-through.
fn scene_content(value: &Arc<dyn NodeData>) -> anyhow::Result<SceneContent> {
    if let Some(geometry) = value.downcast_ref::<Geometry>() {
        return Ok(SceneContent::Geometry(Arc::new(geometry.clone())));
    }
    if let Some(scene) = value.downcast_ref::<Scene>() {
        return Ok(SceneContent::Scene(Arc::new(scene.clone())));
    }
    if value.data_type_id() == DataTypeId::FRAME_BUFFER {
        bail!(
            "scene.add: a frame buffer cannot be placed in a scene directly. Insert a \
             `geometry.from_image` node, which wraps it as a geometry that carries it, and \
             connect that to the object port instead"
        );
    }
    bail!(
        "scene.add: the object must be a geometry or a scene, but its data type is {}",
        value.data_type_id().raw()
    )
}

/// The scene an input port carries, or an empty scene when it is unconnected.
fn scene_or_empty(inputs: &[Option<Arc<dyn NodeData>>], index: usize) -> anyhow::Result<Scene> {
    match inputs.get(index).and_then(|input| input.as_ref()) {
        None => Ok(Scene::new()),
        Some(value) => value
            .downcast_ref::<Scene>()
            .cloned()
            .with_context(|| format!("input {index} is not a Scene")),
    }
}

/// `scene.add`: place a geometry or a nested scene into a scene with a 3D
/// transform.
///
/// The transform is read per frame from the resolved parameters, so every
/// component sits on the unified animation channel. Rotation is Euler angles
/// in degrees applied Z → Y → X; a nested scene's own transforms compose
/// under this one, which is what makes the parent/child hierarchy work.
pub struct SceneAddProcessor;

impl SceneAddProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for SceneAddProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let scene = scene_or_empty(inputs, 1).context("scene.add")?;

        let Some(object) = inputs.first().and_then(|input| input.as_ref()) else {
            // Nothing to place: the incoming scene passes through unchanged so
            // an unconnected object port does not break a chain.
            return Ok(Arc::new(scene));
        };

        let transform = Transform3D {
            translate: params.vec3_or("translate", [0.0, 0.0, 0.0]),
            rotate: params.vec3_or("rotation", [0.0, 0.0, 0.0]),
            scale: params.vec3_or("scale", [1.0, 1.0, 1.0]),
            pivot: params.vec3_or("pivot", [0.0, 0.0, 0.0]),
        };
        let content = scene_content(object)?;
        Ok(Arc::new(scene.with_object(
            ravel_core::scene::SceneObject::new(content, transform),
        )))
    }
}

/// `scene.merge`: the union of two scenes, objects and cameras alike.
pub struct SceneMergeProcessor;

impl SceneMergeProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for SceneMergeProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        inputs: &[Option<Arc<dyn NodeData>>],
        _params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let slot = |index: usize| -> Option<&Scene> {
            inputs
                .get(index)
                .and_then(|input| input.as_ref())
                .and_then(|input| input.downcast_ref::<Scene>())
        };
        match (slot(0), slot(1)) {
            (None, None) => Ok(Arc::new(Scene::new())),
            // One side missing: share the other input wholesale rather than
            // rebuilding an identical list.
            (Some(_), None) => Ok(inputs[0].as_ref().expect("A present").clone()),
            (None, Some(_)) => Ok(inputs[1].as_ref().expect("B present").clone()),
            (Some(a), Some(b)) => Ok(Arc::new(a.merged(b))),
        }
    }
}

/// `scene.camera`: a scene holding one camera and no objects.
///
/// The aspect ratio is deliberately **not** baked in here. It comes from the
/// composition resolution at the moment a projection matrix is needed
/// ([`ravel_core::scene::camera::aspect_ratio`]), so the same camera value
/// stays correct when it is rendered into a different canvas.
pub struct SceneCameraProcessor;

impl SceneCameraProcessor {
    pub fn from_node(_node: &Node) -> Self {
        Self
    }
}

impl NodeProcessor for SceneCameraProcessor {
    fn process(
        &self,
        _node: &Node,
        _ctx: &EvalContext,
        _inputs: &[Option<Arc<dyn NodeData>>],
        params: &ResolvedParams,
        _scope: &mut dyn EvalScope,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let defaults = Camera::default();
        let projection = match params.str_or("projection", PROJECTION_PERSPECTIVE) {
            PROJECTION_ORTHOGRAPHIC => Projection::Orthographic {
                height: params.f32_or(
                    "ortho_height",
                    ravel_core::scene::camera::DEFAULT_ORTHOGRAPHIC_HEIGHT,
                ),
            },
            // An unknown projection name (a hand-edited project, a future
            // kind read by an older build) falls back to perspective rather
            // than failing the evaluation.
            _ => Projection::Perspective {
                fov_y_degrees: params
                    .f32_or("fov", ravel_core::scene::camera::DEFAULT_FOV_Y_DEGREES),
            },
        };

        let camera = Camera {
            position: params.vec3_or("position", defaults.position),
            target: params.vec3_or("target", defaults.target),
            up: defaults.up,
            projection,
            near: params.f32_or("near", defaults.near),
            far: params.f32_or("far", defaults.far),
        };
        Ok(Arc::new(Scene::from_camera(camera)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::animation::channel::AnimationChannel;
    use ravel_core::animation::curve::KeyframeCurve;
    use ravel_core::animation::interpolation::Interpolation;
    use ravel_core::eval::Evaluator;
    use ravel_core::graph::{Graph, InputPort, Parameter, ParameterValue};
    use ravel_core::id::{EdgeId, InputPortIndex, NodeId, OutputPortIndex};
    use ravel_core::registry::{NodeRegistry, builtin};
    use ravel_core::types::{FrameBuffer, FrameRate, Vec2};

    fn ctx(comp: (u32, u32)) -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), comp)
    }

    fn geometry() -> Geometry {
        Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0)])
    }

    /// A source node whose value the test supplies directly, standing in for
    /// whatever upstream produces a geometry / frame buffer / scene.
    struct ValueSource(Arc<dyn NodeData>);

    impl NodeProcessor for ValueSource {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            Ok(Arc::clone(&self.0))
        }
    }

    fn template(type_key: &str) -> ravel_core::registry::NodeTemplate {
        let mut registry = NodeRegistry::new();
        builtin::register_builtins(&mut registry);
        registry
            .get(type_key)
            .expect("built-in template is registered")
            .clone()
    }

    /// Build a one-node graph from `type_key`'s template, feed each
    /// `(port, value)` pair from a source node, and evaluate it.
    fn run(
        type_key: &str,
        params: Vec<Parameter>,
        wired: Vec<(u32, Arc<dyn NodeData>)>,
        ctx: &EvalContext,
    ) -> anyhow::Result<Arc<dyn NodeData>> {
        let target = NodeId::new(1);
        let mut node = template(type_key).create_node(target);
        for param in params {
            match node.parameters.iter_mut().find(|p| p.key == param.key) {
                Some(slot) => *slot = param,
                None => node.parameters.push(param),
            }
        }
        let mut graph = Graph::new().add_node(node).unwrap();

        let mut evaluator = Evaluator::new();
        evaluator.register(target, processor(type_key));
        for (port, value) in wired {
            let source_id = NodeId::new(100 + port as u64);
            let source =
                Node::new(source_id, "test.source").with_output("output", value.data_type_id());
            graph = graph
                .add_node(source)
                .unwrap()
                .add_edge(
                    EdgeId::new(200 + port as u64),
                    source_id,
                    OutputPortIndex(0),
                    target,
                    InputPortIndex(port),
                )
                .unwrap();
            evaluator.register(source_id, Arc::new(ValueSource(value)));
        }
        Ok(evaluator.evaluate(&graph, target, ctx)?)
    }

    fn processor(type_key: &str) -> Arc<dyn NodeProcessor> {
        match type_key {
            "scene.add" => Arc::new(SceneAddProcessor),
            "scene.merge" => Arc::new(SceneMergeProcessor),
            "scene.camera" => Arc::new(SceneCameraProcessor),
            other => panic!("no processor for {other}"),
        }
    }

    fn as_scene(value: &Arc<dyn NodeData>) -> &Scene {
        value.downcast_ref::<Scene>().expect("output is a Scene")
    }

    fn assert_close(actual: f32, expected: f32, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-4,
            "{what}: expected {expected}, got {actual}"
        );
    }

    fn vec3_param(key: &str, x: f32, y: f32, z: f32) -> Parameter {
        Parameter {
            key: key.into(),
            value: ParameterValue::vec3(x, y, z),
        }
    }

    fn float_param(key: &str, value: f32) -> Parameter {
        Parameter {
            key: key.into(),
            value: ParameterValue::Float(value),
        }
    }

    fn string_param(key: &str, value: &str) -> Parameter {
        Parameter {
            key: key.into(),
            value: ParameterValue::String(value.into()),
        }
    }

    // -----------------------------------------------------------------------
    // scene.add
    // -----------------------------------------------------------------------

    #[test]
    fn scene_add_places_a_geometry_with_its_transform() {
        let out = run(
            "scene.add",
            vec![vec3_param("translate", 30.0, 40.0, 50.0)],
            vec![(0, Arc::new(geometry()))],
            &ctx((1920, 1080)),
        )
        .expect("evaluation succeeds");
        let scene = as_scene(&out);
        assert_eq!(scene.object_count(), 1);
        assert!(matches!(
            scene.objects()[0].content,
            SceneContent::Geometry(_)
        ));
        assert_eq!(scene.objects()[0].transform.translate, [30.0, 40.0, 50.0]);
        assert_eq!(scene.flatten().len(), 1);
        assert_eq!(
            scene.flatten()[0]
                .world_transform
                .transform_point3([0.0, 0.0, 0.0]),
            [30.0, 40.0, 50.0]
        );
    }

    #[test]
    fn scene_add_accumulates_into_the_incoming_scene() {
        let existing: Arc<dyn NodeData> = Arc::new(Scene::new().with_object(
            ravel_core::scene::SceneObject::geometry(Arc::new(geometry()), Transform3D::IDENTITY),
        ));
        let out = run(
            "scene.add",
            vec![],
            vec![(0, Arc::new(geometry())), (1, existing)],
            &ctx((1920, 1080)),
        )
        .expect("evaluation succeeds");
        assert_eq!(as_scene(&out).object_count(), 2);
    }

    #[test]
    fn scene_add_passes_the_scene_through_when_no_object_is_connected() {
        let existing: Arc<dyn NodeData> = Arc::new(Scene::from_camera(Camera::default()));
        let out = run("scene.add", vec![], vec![(1, existing)], &ctx((1920, 1080)))
            .expect("evaluation succeeds");
        assert_eq!(as_scene(&out).object_count(), 0);
        assert_eq!(as_scene(&out).camera_count(), 1);
    }

    #[test]
    fn scene_add_with_nothing_connected_yields_an_empty_scene() {
        let out = run("scene.add", vec![], vec![], &ctx((1920, 1080)))
            .expect("an unconnected node still evaluates");
        assert_eq!(as_scene(&out).object_count(), 0);
    }

    /// A frame buffer is not placeable: the evaluation fails loudly and names
    /// the node that converts one, rather than passing the object through or
    /// panicking. The `object` port no longer declares `FRAME_BUFFER`, so
    /// this is the second line of defence — a value that reaches the
    /// processor anyway.
    #[test]
    fn scene_add_rejects_a_frame_buffer_and_names_the_conversion() {
        let frame: Arc<dyn NodeData> = Arc::new(FrameBuffer::new_zeroed(1280, 720));
        let Err(error) = run("scene.add", vec![], vec![(0, frame)], &ctx((1920, 1080))) else {
            panic!("a frame buffer cannot be placed in a scene");
        };
        let message = format!("{error:#}");
        assert!(
            message.contains("geometry.from_image"),
            "the error must name the conversion node: {message}"
        );
        assert!(
            message.contains("Insert a"),
            "the error must tell the user to insert the conversion node: {message}"
        );
        assert!(
            !message.contains("does not have yet"),
            "the conversion node exists now, so the message must not still call it missing: \
             {message}"
        );
    }

    /// The `object` port does not accept a frame buffer, which is what stops
    /// the edge from being drawn in the first place.
    #[test]
    fn the_object_port_does_not_accept_a_frame_buffer() {
        let port: &InputPort = &template("scene.add").inputs[0];
        assert!(!port.accepted_types.contains(&DataTypeId::FRAME_BUFFER));
    }

    /// The nesting case: a scene on the `object` port becomes a child whose
    /// own transforms compose under the parent's.
    #[test]
    fn scene_add_nests_a_scene_and_composes_the_transforms() {
        let child: Arc<dyn NodeData> = Arc::new(Scene::new().with_object(
            ravel_core::scene::SceneObject::geometry(
                Arc::new(geometry()),
                Transform3D::from_translation([10.0, 0.0, 0.0]),
            ),
        ));
        let out = run(
            "scene.add",
            vec![vec3_param("translate", 0.0, 5.0, 0.0)],
            vec![(0, child)],
            &ctx((1920, 1080)),
        )
        .expect("evaluation succeeds");
        let scene = as_scene(&out);
        assert_eq!(scene.object_count(), 1, "the child is one object");
        assert!(matches!(scene.objects()[0].content, SceneContent::Scene(_)));

        let flat = scene.flatten();
        assert_eq!(flat.len(), 1, "flattening drops the nesting");
        assert_eq!(
            flat[0].world_transform.transform_point3([0.0, 0.0, 0.0]),
            [10.0, 5.0, 0.0]
        );
    }

    /// `scene.add`'s transform is animatable: a keyframed component resolves
    /// per frame, and the placed object follows.
    #[test]
    fn scene_add_transform_is_keyframable() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 100.0, Interpolation::Linear);
        let animated = Parameter {
            key: "translate".into(),
            value: ParameterValue::Channel3([
                AnimationChannel::keyframes(curve),
                AnimationChannel::constant(0.0),
                AnimationChannel::constant(0.0),
            ]),
        };

        let mut translations = Vec::new();
        for frame in [0u64, 5, 10] {
            let out = run(
                "scene.add",
                vec![animated.clone()],
                vec![(0, Arc::new(geometry()))],
                &EvalContext::new(frame, FrameRate::new(30, 1), (1920, 1080)),
            )
            .expect("evaluation succeeds");
            translations.push(as_scene(&out).objects()[0].transform.translate[0]);
        }
        assert_close(translations[0], 0.0, "frame 0");
        assert_close(translations[1], 50.0, "frame 5 interpolates");
        assert_close(translations[2], 100.0, "frame 10");
    }

    /// A channel that runs `from` → `to` over frames 0..10, for keyframe
    /// coverage. Component `component` of an arity-`arity` parameter is the
    /// animated one; the rest stay at `rest`.
    fn animated_component(
        key: &str,
        arity: usize,
        component: usize,
        from: f32,
        to: f32,
        rest: f32,
    ) -> Parameter {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, from, Interpolation::Linear);
        curve.insert(10, to, Interpolation::Linear);
        let channels: Vec<AnimationChannel> = (0..arity)
            .map(|index| {
                if index == component {
                    AnimationChannel::keyframes(curve.clone())
                } else {
                    AnimationChannel::constant(rest)
                }
            })
            .collect();
        Parameter {
            key: key.into(),
            value: ParameterValue::from_channels(None, channels).expect("arity 1..=4"),
        }
    }

    /// **Every** animatable component of `scene.add`'s transform follows its
    /// keyframes — not just `translate.x`. A regression on any one of
    /// `rotation`, `scale`, `pivot`, or a Y/Z component would otherwise pass.
    #[test]
    fn every_scene_add_transform_component_is_keyframable() {
        // (key, rest value for the untouched components, how to read the field)
        type Read = fn(&Transform3D) -> [f32; 3];
        let cases: [(&str, f32, Read); 4] = [
            ("translate", 0.0, |t| t.translate),
            ("rotation", 0.0, |t| t.rotate),
            ("scale", 1.0, |t| t.scale),
            ("pivot", 0.0, |t| t.pivot),
        ];

        for (key, rest, read) in cases {
            for component in 0..3 {
                let param = animated_component(key, 3, component, 0.0, 90.0, rest);
                let value_at = |frame: u64| -> [f32; 3] {
                    let out = run(
                        "scene.add",
                        vec![param.clone()],
                        vec![(0, Arc::new(geometry()))],
                        &EvalContext::new(frame, FrameRate::new(30, 1), (1920, 1080)),
                    )
                    .expect("evaluation succeeds");
                    read(&as_scene(&out).objects()[0].transform)
                };

                for (frame, expected) in [(0u64, 0.0f32), (5, 45.0), (10, 90.0)] {
                    let actual = value_at(frame);
                    assert_close(
                        actual[component],
                        expected,
                        &format!("{key}[{component}] at frame {frame}"),
                    );
                    for (other, value) in actual.iter().enumerate() {
                        if other != component {
                            assert_close(
                                *value,
                                rest,
                                &format!("{key}[{other}] must stay at its constant"),
                            );
                        }
                    }
                }
            }
        }
    }

    /// **Every** animatable `scene.camera` parameter follows its keyframes,
    /// vector components included, and the value reaches the matrices.
    #[test]
    fn every_scene_camera_parameter_is_keyframable() {
        type ReadVec = fn(&Camera) -> [f32; 3];
        let vectors: [(&str, ReadVec); 2] =
            [("position", |c| c.position), ("target", |c| c.target)];

        for (key, read) in vectors {
            for component in 0..3 {
                let param = animated_component(key, 3, component, -200.0, 200.0, 0.0);
                for (frame, expected) in [(0u64, -200.0f32), (5, 0.0), (10, 200.0)] {
                    let out = run(
                        "scene.camera",
                        vec![param.clone()],
                        vec![],
                        &EvalContext::new(frame, FrameRate::new(30, 1), (1920, 1080)),
                    )
                    .expect("evaluation succeeds");
                    let camera = *as_scene(&out).primary_camera().expect("one camera");
                    assert_close(
                        read(&camera)[component],
                        expected,
                        &format!("{key}[{component}] at frame {frame}"),
                    );
                }
            }
        }

        type ReadScalar = fn(&Camera) -> f32;
        // (key, from, to, extra params the reading needs, how to read it)
        let scalars: [(&str, f32, f32, bool, ReadScalar); 4] = [
            ("near", 1.0, 101.0, false, |c| c.near),
            ("far", 1000.0, 3000.0, false, |c| c.far),
            ("fov", 20.0, 100.0, false, |c| match c.projection {
                Projection::Perspective { fov_y_degrees } => fov_y_degrees,
                Projection::Orthographic { .. } => panic!("perspective expected"),
            }),
            ("ortho_height", 200.0, 1000.0, true, |c| {
                match c.projection {
                    Projection::Orthographic { height } => height,
                    Projection::Perspective { .. } => panic!("orthographic expected"),
                }
            }),
        ];

        for (key, from, to, orthographic, read) in scalars {
            let animated = animated_component(key, 1, 0, from, to, 0.0);
            for (frame, expected) in [(0u64, from), (5, (from + to) * 0.5), (10, to)] {
                let mut params = vec![animated.clone()];
                if orthographic {
                    params.push(string_param("projection", PROJECTION_ORTHOGRAPHIC));
                }
                let out = run(
                    "scene.camera",
                    params,
                    vec![],
                    &EvalContext::new(frame, FrameRate::new(30, 1), (1920, 1080)),
                )
                .expect("evaluation succeeds");
                let camera = *as_scene(&out).primary_camera().expect("one camera");
                assert_close(read(&camera), expected, &format!("{key} at frame {frame}"));
            }
        }
    }

    /// An animated `near` / `far` reaches the projection matrix, so the
    /// keyframes are not merely stored on the camera value.
    #[test]
    fn keyframed_clip_planes_reach_the_projection_matrix() {
        let animated = animated_component("far", 1, 0, 1000.0, 5000.0, 0.0);
        let depth_of = |frame: u64, z: f32| -> f32 {
            let context = EvalContext::new(frame, FrameRate::new(30, 1), (1920, 1080));
            let out = run("scene.camera", vec![animated.clone()], vec![], &context)
                .expect("evaluation succeeds");
            as_scene(&out)
                .primary_camera()
                .expect("one camera")
                .projection_matrix_for(&context)
                .transform_point3([0.0, 0.0, z])[2]
        };
        // A fixed view depth sits nearer the far plane as the far plane
        // retreats, so its normalized depth has to fall.
        let early = depth_of(0, 900.0);
        let late = depth_of(10, 900.0);
        assert!(
            early > late,
            "a retreating far plane must lower a fixed depth: {early} then {late}"
        );
        // And the far plane itself always lands at 1.
        assert_close(depth_of(0, 1000.0), 1.0, "far plane at frame 0");
        assert_close(depth_of(10, 5000.0), 1.0, "far plane at frame 10");
    }

    #[test]
    fn scene_add_rejects_a_value_that_is_not_placeable() {
        let scalar: Arc<dyn NodeData> = Arc::new(ravel_core::types::Scalar(1.0));
        let Err(error) = run("scene.add", vec![], vec![(0, scalar)], &ctx((1920, 1080))) else {
            panic!("a scalar cannot be placed in a scene");
        };
        assert!(
            format!("{error:#}").contains("must be a geometry or a scene"),
            "unexpected error: {error:#}"
        );
    }

    // -----------------------------------------------------------------------
    // scene.merge
    // -----------------------------------------------------------------------

    #[test]
    fn scene_merge_unions_objects_and_cameras() {
        let a: Arc<dyn NodeData> = Arc::new(
            Scene::new()
                .with_object(ravel_core::scene::SceneObject::geometry(
                    Arc::new(geometry()),
                    Transform3D::IDENTITY,
                ))
                .with_camera(Camera::default()),
        );
        let b: Arc<dyn NodeData> = Arc::new(Scene::new().with_object(
            ravel_core::scene::SceneObject::geometry(Arc::new(geometry()), Transform3D::IDENTITY),
        ));
        let out = run(
            "scene.merge",
            vec![],
            vec![(0, a), (1, b)],
            &ctx((1920, 1080)),
        )
        .expect("evaluation succeeds");
        assert_eq!(as_scene(&out).object_count(), 2);
        assert_eq!(as_scene(&out).camera_count(), 1);
    }

    #[test]
    fn scene_merge_handles_missing_sides() {
        let a: Arc<dyn NodeData> = Arc::new(Scene::from_camera(Camera::default()));
        let only_a = run(
            "scene.merge",
            vec![],
            vec![(0, Arc::clone(&a))],
            &ctx((1920, 1080)),
        )
        .expect("evaluation succeeds");
        assert_eq!(as_scene(&only_a).camera_count(), 1);

        let only_b = run("scene.merge", vec![], vec![(1, a)], &ctx((1920, 1080)))
            .expect("evaluation succeeds");
        assert_eq!(as_scene(&only_b).camera_count(), 1);

        let neither =
            run("scene.merge", vec![], vec![], &ctx((1920, 1080))).expect("evaluation succeeds");
        assert_eq!(as_scene(&neither).camera_count(), 0);
        assert_eq!(as_scene(&neither).object_count(), 0);
    }

    /// A camera scene and an object scene merge into one renderable scene,
    /// which is how `scene.camera` reaches `scene.render`.
    #[test]
    fn a_camera_scene_merges_into_an_object_scene() {
        let camera_scene = run(
            "scene.camera",
            vec![vec3_param("position", 0.0, 0.0, -800.0)],
            vec![],
            &ctx((1920, 1080)),
        )
        .expect("evaluation succeeds");
        let objects: Arc<dyn NodeData> = Arc::new(Scene::new().with_object(
            ravel_core::scene::SceneObject::geometry(Arc::new(geometry()), Transform3D::IDENTITY),
        ));
        let merged = run(
            "scene.merge",
            vec![],
            vec![(0, objects), (1, camera_scene)],
            &ctx((1920, 1080)),
        )
        .expect("evaluation succeeds");
        let scene = as_scene(&merged);
        assert_eq!(scene.object_count(), 1);
        assert_eq!(
            scene
                .primary_camera()
                .expect("a camera was merged in")
                .position,
            [0.0, 0.0, -800.0]
        );
    }

    // -----------------------------------------------------------------------
    // scene.camera
    // -----------------------------------------------------------------------

    #[test]
    fn scene_camera_produces_a_scene_holding_one_camera() {
        let out =
            run("scene.camera", vec![], vec![], &ctx((1920, 1080))).expect("evaluation succeeds");
        let scene = as_scene(&out);
        assert_eq!(scene.object_count(), 0);
        assert_eq!(scene.camera_count(), 1);
        assert_eq!(scene.primary_camera(), Some(&Camera::default()));
    }

    #[test]
    fn scene_camera_reads_its_parameters() {
        let out = run(
            "scene.camera",
            vec![
                vec3_param("position", 1.0, 2.0, -300.0),
                vec3_param("target", 4.0, 5.0, 6.0),
                float_param("fov", 35.0),
                float_param("near", 2.0),
                float_param("far", 4000.0),
            ],
            vec![],
            &ctx((1920, 1080)),
        )
        .expect("evaluation succeeds");
        let camera = *as_scene(&out).primary_camera().expect("one camera");
        assert_eq!(camera.position, [1.0, 2.0, -300.0]);
        assert_eq!(camera.target, [4.0, 5.0, 6.0]);
        assert_eq!(camera.near, 2.0);
        assert_eq!(camera.far, 4000.0);
        assert_eq!(
            camera.projection,
            Projection::Perspective {
                fov_y_degrees: 35.0
            }
        );
    }

    #[test]
    fn scene_camera_selects_the_orthographic_projection() {
        let out = run(
            "scene.camera",
            vec![
                string_param("projection", PROJECTION_ORTHOGRAPHIC),
                float_param("ortho_height", 720.0),
            ],
            vec![],
            &ctx((1920, 1080)),
        )
        .expect("evaluation succeeds");
        assert_eq!(
            as_scene(&out)
                .primary_camera()
                .expect("one camera")
                .projection,
            Projection::Orthographic { height: 720.0 }
        );
    }

    #[test]
    fn scene_camera_falls_back_to_perspective_for_an_unknown_projection() {
        let out = run(
            "scene.camera",
            vec![string_param("projection", "fisheye")],
            vec![],
            &ctx((1920, 1080)),
        )
        .expect("an unknown projection must not fail the evaluation");
        assert!(matches!(
            as_scene(&out)
                .primary_camera()
                .expect("one camera")
                .projection,
            Projection::Perspective { .. }
        ));
    }

    /// Camera parameters are on the unified animation channel: keyframing
    /// `fov` changes the projection matrix over time (REQ-3D-002).
    #[test]
    fn scene_camera_parameters_are_keyframable() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 30.0, Interpolation::Linear);
        curve.insert(10, 90.0, Interpolation::Linear);
        let animated = Parameter {
            key: "fov".into(),
            value: ParameterValue::Channel(AnimationChannel::keyframes(curve)),
        };

        let mut focals = Vec::new();
        for frame in [0u64, 5, 10] {
            let context = EvalContext::new(frame, FrameRate::new(30, 1), (1920, 1080));
            let out = run("scene.camera", vec![animated.clone()], vec![], &context)
                .expect("evaluation succeeds");
            let camera = *as_scene(&out).primary_camera().expect("one camera");
            let Projection::Perspective { fov_y_degrees } = camera.projection else {
                panic!("perspective projection");
            };
            focals.push((
                fov_y_degrees,
                camera.projection_matrix_for(&context).element(1, 1),
            ));
        }
        assert_close(focals[0].0, 30.0, "fov at frame 0");
        assert_close(focals[1].0, 60.0, "fov at frame 5");
        assert_close(focals[2].0, 90.0, "fov at frame 10");
        assert!(
            focals[0].1 > focals[1].1 && focals[1].1 > focals[2].1,
            "a widening field of view shrinks the vertical focal length: {focals:?}"
        );
        assert_close(focals[2].1, 1.0, "90 degrees gives a focal length of 1");
    }

    /// A keyframed position drives the view matrix, which is the other half
    /// of "camera parameters can be keyframed".
    #[test]
    fn a_keyframed_camera_position_moves_the_view() {
        let mut curve = KeyframeCurve::new();
        curve.insert(0, -1000.0, Interpolation::Linear);
        curve.insert(10, -500.0, Interpolation::Linear);
        let animated = Parameter {
            key: "position".into(),
            value: ParameterValue::Channel3([
                AnimationChannel::constant(0.0),
                AnimationChannel::constant(0.0),
                AnimationChannel::keyframes(curve),
            ]),
        };

        let depth_at = |frame: u64| -> f32 {
            let context = EvalContext::new(frame, FrameRate::new(30, 1), (1920, 1080));
            let out = run("scene.camera", vec![animated.clone()], vec![], &context)
                .expect("evaluation succeeds");
            as_scene(&out)
                .primary_camera()
                .expect("one camera")
                .view_matrix()
                .transform_point3([0.0, 0.0, 0.0])[2]
        };
        assert_close(depth_at(0), 1000.0, "frame 0");
        assert_close(depth_at(5), 750.0, "frame 5");
        assert_close(depth_at(10), 500.0, "frame 10");
    }

    /// The projection's aspect ratio comes from the composition resolution,
    /// end to end through the evaluator.
    #[test]
    fn the_projected_aspect_ratio_follows_the_composition_resolution() {
        let camera_in = |comp: (u32, u32)| -> Camera {
            let out = run("scene.camera", vec![], vec![], &ctx(comp)).expect("evaluation succeeds");
            *as_scene(&out).primary_camera().expect("one camera")
        };

        let wide_ctx = ctx((1920, 1080));
        let square_ctx = ctx((1080, 1080));
        let wide = camera_in((1920, 1080)).projection_matrix_for(&wide_ctx);
        let square = camera_in((1080, 1080)).projection_matrix_for(&square_ctx);

        assert_close(
            wide.element(0, 0) * (16.0 / 9.0),
            square.element(0, 0),
            "the horizontal scale carries the aspect ratio",
        );
        assert_close(
            wide.element(1, 1),
            square.element(1, 1),
            "the vertical field of view does not",
        );
    }

    // -----------------------------------------------------------------------
    // Registration
    // -----------------------------------------------------------------------

    #[test]
    fn the_templates_declare_scene_typed_ports() {
        let add = template("scene.add");
        assert_eq!(
            add.inputs[0].accepted_types,
            vec![DataTypeId::GEOMETRY, DataTypeId::SCENE]
        );
        assert_eq!(add.inputs[1].accepted_types, vec![DataTypeId::SCENE]);
        assert_eq!(add.outputs[0].data_type, DataTypeId::SCENE);

        let merge = template("scene.merge");
        assert_eq!(merge.inputs.len(), 2);
        assert_eq!(merge.outputs[0].data_type, DataTypeId::SCENE);

        let camera = template("scene.camera");
        assert!(camera.inputs.is_empty(), "a camera is a source node");
        assert_eq!(camera.outputs[0].data_type, DataTypeId::SCENE);
        assert_eq!(
            camera.param_option_values("projection"),
            Some(
                &[
                    PROJECTION_PERSPECTIVE.to_string(),
                    PROJECTION_ORTHOGRAPHIC.to_string()
                ][..]
            )
        );
    }

    /// Every transform and camera parameter is channel-backed, which is what
    /// puts it on the unified animation channel.
    #[test]
    fn the_animatable_parameters_are_channel_backed() {
        for (type_key, keys) in [
            (
                "scene.add",
                &["translate", "rotation", "scale", "pivot"][..],
            ),
            (
                "scene.camera",
                &["position", "target", "fov", "near", "far", "ortho_height"][..],
            ),
        ] {
            let node = template(type_key).create_node(NodeId::new(1));
            for key in keys {
                let param = node
                    .parameters
                    .iter()
                    .find(|p| p.key == *key)
                    .unwrap_or_else(|| panic!("{type_key} declares {key}"));
                assert!(
                    param.value.channels().is_some(),
                    "{type_key}.{key} must be channel-backed to be keyframable"
                );
            }
        }
    }

    #[test]
    fn the_scene_input_port_only_accepts_a_scene() {
        // A guard against a future edit widening the port: the nesting case is
        // the `object` port, not the accumulator.
        let port: &InputPort = &template("scene.add").inputs[1];
        assert_eq!(port.accepted_types, vec![DataTypeId::SCENE]);
        assert!(!port.is_variadic);
    }
}
