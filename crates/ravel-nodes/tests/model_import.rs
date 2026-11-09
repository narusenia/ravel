// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Model import (REQ-3D-008): glTF / GLB / OBJ into a `Scene`.
//!
//! Expected coordinates are **computed by hand** in the comments next to each
//! assertion, in the file's basis (+Y up, +Z toward the viewer) and in scene
//! space (+Y down, +Z away): `(x, y, z) -> (x, -y, -z)`. The loader's own
//! conversion function is never called from here.
//!
//! The fixture `wedge` is deliberately asymmetric: a triangle whose apex is up
//! (+Y) and tilted toward the viewer (+Z), so a flipped up axis, a flipped
//! facing, a swapped winding or a negated rotation each produce a different,
//! checkable picture.

use ravel_core::composition::{Document, MediaAssetEntry};
use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor, ResolvedParams};
use ravel_core::geometry::{AttributeArray, Geometry, Primitive, names};
use ravel_core::graph::{Graph, Node, ParameterValue};
use ravel_core::id::{AssetId, EdgeId, InputPortIndex, NodeId, OutputPortIndex};
use ravel_core::registry::NodeRegistry;
use ravel_core::registry::builtin::register_builtins;
use ravel_core::scene::{Camera, Projection, Scene, SceneContent, SceneObject, Transform3D};
use ravel_core::types::{FrameBuffer, FrameRate, NodeData, PortRecord};
use ravel_nodes::model::{self, MeshImportProcessor};
use ravel_nodes::scene_render::SceneRenderProcessor;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/models")
        .join(name)
}

fn ctx() -> EvalContext {
    EvalContext::new(0, FrameRate::new(30, 1), (64, 64))
}

fn points(geometry: &Geometry) -> Vec<[f32; 3]> {
    match geometry.points().get(names::P).map(|p| p.as_ref()) {
        Some(AttributeArray::Vec3(values)) => values.iter().map(|v| [v.0, v.1, v.2]).collect(),
        other => panic!("P is not a Vec3 column: {other:?}"),
    }
}

fn normals(geometry: &Geometry) -> Vec<[f32; 3]> {
    match geometry.points().get(names::N).map(|n| n.as_ref()) {
        Some(AttributeArray::Vec3(values)) => values.iter().map(|v| [v.0, v.1, v.2]).collect(),
        other => panic!("N is not a Vec3 column: {other:?}"),
    }
}

fn assert_close(actual: [f32; 3], expected: [f32; 3], label: &str) {
    for axis in 0..3 {
        assert!(
            (actual[axis] - expected[axis]).abs() < 1e-4,
            "{label}: expected {expected:?}, got {actual:?}"
        );
    }
}

/// The geometric normal of triangle `(a, b, c)` from its winding.
fn face_normal(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> [f32; 3] {
    let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let n = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    [n[0] / length, n[1] / length, n[2] / length]
}

fn objects(scene: &Scene) -> Vec<&SceneObject> {
    scene.objects().iter().collect()
}

fn geometry_of(object: &SceneObject) -> &Arc<Geometry> {
    match &object.content {
        SceneContent::Geometry(geometry) => geometry,
        SceneContent::Scene(_) => panic!("expected a geometry object"),
    }
}

fn nested_of(object: &SceneObject) -> &Arc<Scene> {
    match &object.content {
        SceneContent::Scene(scene) => scene,
        SceneContent::Geometry(_) => panic!("expected a nested scene"),
    }
}

// ---------------------------------------------------------------------------
// OBJ
// ---------------------------------------------------------------------------

/// `wedge.obj` holds a triangle and a quad (`o wedge`, `o slab`); the quad is
/// triangulated, and the objects stay separate.
#[test]
fn an_obj_loads_one_geometry_per_object_with_triangulated_faces() {
    let scene = model::load(&fixture("wedge.obj")).expect("loads");
    let all = objects(&scene);
    assert_eq!(all.len(), 2);

    let wedge = geometry_of(all[0]);
    assert_eq!(wedge.point_count(), 3);
    assert_eq!(wedge.primitive_count(), 1);
    assert_eq!(wedge.indices(), &[0, 1, 2]);

    let slab = geometry_of(all[1]);
    assert_eq!(slab.point_count(), 4);
    assert_eq!(slab.indices().len(), 6, "a quad is two triangles");
    slab.validate().expect("a consistent mesh");
}

/// The file is +Y up: apex `(0, 2, 0.5)` above the base `y = 0`, facing
/// `+Z`. In scene space "up" is `-Y` and "toward the viewer" is `-Z`, so the
/// apex lands at `(0, -2, -0.5)` and the normal at
/// `(0, 0.2425, -0.9701)` (the file's `(0, -0.2425, 0.9701)` negated in y, z).
#[test]
fn an_obj_is_converted_from_y_up_to_scene_space() {
    let scene = model::load(&fixture("wedge.obj")).unwrap();
    let wedge = geometry_of(objects(&scene)[0]);
    let p = points(wedge);
    assert_close(p[0], [-1.0, 0.0, 0.0], "base left");
    assert_close(p[1], [1.0, 0.0, 0.0], "base right");
    assert_close(p[2], [0.0, -2.0, -0.5], "apex");
    for n in normals(wedge) {
        assert_close(n, [0.0, 0.242536, -0.970143], "wedge normal");
    }
}

/// Winding and normal must still agree after the conversion: the stored
/// normal is the triangle's own normal in the file, so if only one of the two
/// were converted (or the winding were swapped), the normal computed from the
/// converted vertices would point the other way.
#[test]
fn converted_winding_agrees_with_converted_normals() {
    for name in ["wedge.obj", "wedge.gltf"] {
        let scene = model::load(&fixture(name)).unwrap();
        let wedge = model::flatten_to_geometry(&scene).unwrap();
        let p = points(&wedge);
        let n = normals(&wedge);
        for triangle in wedge.indices().chunks(3).take(1) {
            let [a, b, c] = [0, 1, 2].map(|k| triangle[k] as usize);
            let computed = face_normal(p[a], p[b], p[c]);
            for vertex in [a, b, c] {
                let dot: f32 = (0..3).map(|k| computed[k] * n[vertex][k]).sum();
                assert!(
                    dot > 0.999,
                    "{name}: winding and stored normal disagree ({dot})"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Up is up when drawn
// ---------------------------------------------------------------------------

fn pixel_camera(size: u32) -> Camera {
    let c = size as f32 / 2.0;
    Camera::looking_at([c, c, -100.0], [c, c, 0.0])
        .with_projection(Projection::Orthographic {
            height: size as f32,
        })
        .with_clip_range(1.0, 1000.0)
}

fn render(scene: &Scene) -> FrameBuffer {
    let out = SceneRenderProcessor::from_node(&Node::new(NodeId::new(1), "scene.render"))
        .process(
            &Node::new(NodeId::new(1), "scene.render"),
            &ctx(),
            &[Some(Arc::new(scene.clone()) as Arc<dyn NodeData>)],
            &ResolvedParams::default(),
            &mut Evaluator::new(),
        )
        .expect("scene.render succeeds");
    out.downcast_ref::<FrameBuffer>()
        .expect("a CPU frame buffer")
        .clone()
}

fn alpha(frame: &FrameBuffer, x: u32, y: u32) -> f32 {
    frame.as_f32()[((y * frame.width + x) * 4 + 3) as usize]
}

/// The wedge, scaled by 10 and placed with its base at pixel row 50 under a
/// camera where scene `(x, y)` is pixel `(x, y)`: the apex is `2 * 10 = 20`
/// rows *above* the base, at row 30. A model imported upside down would
/// instead hang below the base (rows 50..70) and leave row 35 empty.
fn assert_drawn_upright(scene: &Scene, label: &str) {
    let placed = Transform3D {
        translate: [32.0, 50.0, 0.0],
        scale: [10.0, 10.0, 10.0],
        ..Transform3D::IDENTITY
    };
    let framed = Scene::from_camera(pixel_camera(64))
        .with_object(SceneObject::scene(Arc::new(scene.clone()), placed));
    let frame = render(&framed);
    assert!(
        alpha(&frame, 32, 35) > 0.5,
        "{label}: the apex region is covered"
    );
    assert!(
        alpha(&frame, 32, 45) > 0.5,
        "{label}: the base region is covered"
    );
    assert!(
        alpha(&frame, 32, 25) < 0.5,
        "{label}: nothing above the apex"
    );
    assert!(
        alpha(&frame, 32, 55) < 0.5,
        "{label}: nothing below the base"
    );
}

/// OBJ's second object (`slab`) is a flat quad at `y = 0`; keep only the wedge
/// so the picture is the one the comment above describes.
fn first_object_only(scene: &Scene) -> Scene {
    Scene::new().with_object(objects(scene)[0].clone())
}

#[test]
fn a_y_up_obj_is_not_drawn_upside_down() {
    let scene = model::load(&fixture("wedge.obj")).unwrap();
    assert_drawn_upright(&first_object_only(&scene), "wedge.obj");
}

// ---------------------------------------------------------------------------
// glTF: hand-built files, so every buffer source and a hierarchy can be shown
// ---------------------------------------------------------------------------

const WEDGE_POSITIONS: [[f32; 3]; 3] = [[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 2.0, 0.5]];
const WEDGE_NORMAL: [f32; 3] = [0.0, -0.242536, 0.970143];

/// Positions, normals, then three `u16` indices padded to four bytes.
fn wedge_bin() -> Vec<u8> {
    let mut bin = Vec::new();
    for p in WEDGE_POSITIONS {
        p.iter()
            .for_each(|v| bin.extend_from_slice(&v.to_le_bytes()));
    }
    for _ in 0..3 {
        WEDGE_NORMAL
            .iter()
            .for_each(|v| bin.extend_from_slice(&v.to_le_bytes()));
    }
    [0u16, 1, 2]
        .iter()
        .for_each(|i| bin.extend_from_slice(&i.to_le_bytes()));
    bin.extend_from_slice(&[0, 0]);
    bin
}

/// The wedge as a glTF document whose single buffer is `buffer` (the JSON of
/// the `buffers[0]` object), placed by `nodes` with `scene_nodes` as roots.
fn wedge_json(buffer: &str, nodes: &str, scene_nodes: &str) -> String {
    format!(
        r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":{scene_nodes}}}],
"nodes":{nodes},
"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0,"NORMAL":1}},"indices":2}}]}}],
"buffers":[{buffer}],
"bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":36}},{{"buffer":0,"byteOffset":36,"byteLength":36}},{{"buffer":0,"byteOffset":72,"byteLength":6}}],
"accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[-1,0,0],"max":[1,2,0.5]}},{{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"}},{{"bufferView":2,"componentType":5123,"count":3,"type":"SCALAR"}}]}}"#
    )
}

const UPRIGHT: &str = r#"[{"mesh":0}]"#;

/// A binary glTF: the JSON chunk (space-padded) then the `BIN` chunk.
fn glb(json: &str, bin: &[u8]) -> Vec<u8> {
    let mut json = json.as_bytes().to_vec();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(bin);
    out
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// A GLB binary chunk and an external `.bin` beside a `.gltf` are both buffer
/// sources; each must yield the same upright wedge as the OBJ.
#[test]
fn glb_and_external_buffers_load_the_same_mesh_as_the_obj() {
    let dir = tempfile::tempdir().unwrap();
    let bin = wedge_bin();
    let glb_path = write(
        dir.path(),
        "wedge.glb",
        &glb(
            &wedge_json(
                &format!(r#"{{"byteLength":{}}}"#, bin.len()),
                UPRIGHT,
                "[0]",
            ),
            &bin,
        ),
    );
    write(dir.path(), "wedge.bin", &bin);
    let external_path = write(
        dir.path(),
        "external.gltf",
        wedge_json(
            &format!(r#"{{"byteLength":{},"uri":"wedge.bin"}}"#, bin.len()),
            UPRIGHT,
            "[0]",
        )
        .as_bytes(),
    );

    let obj = model::load(&fixture("wedge.obj")).unwrap();
    let expected = points(geometry_of(objects(&obj)[0]));
    for path in [glb_path, external_path] {
        let scene = model::load(&path).unwrap();
        let geometry = geometry_of(objects(&scene)[0]);
        for (actual, expected) in points(geometry).into_iter().zip(&expected) {
            assert_close(actual, *expected, &path.display().to_string());
        }
        assert_eq!(geometry.indices(), &[0, 1, 2]);
        assert_drawn_upright(&scene, &path.display().to_string());
    }
}

/// `wedge.gltf` (a `data:` URI buffer) holds `parent -> child` and a separate
/// `spinner`, all instancing one mesh. The hierarchy must arrive as nested
/// scenes with the child's own placement on the inner object.
#[test]
fn a_gltf_hierarchy_becomes_nested_scenes() {
    let scene = model::load(&fixture("wedge.gltf")).unwrap();
    let roots = objects(&scene);
    assert_eq!(roots.len(), 2, "parent and spinner");

    // `parent` has no mesh and one child: a nested scene holding the child.
    let parent_scene = nested_of(roots[0]);
    assert_eq!(parent_scene.object_count(), 1);
    // glTF translation (10, 0, 0) -> scene (10, 0, 0) (y and z negate; -0 == 0).
    assert_eq!(roots[0].transform.translate, [10.0, 0.0, 0.0]);
    // `child`: glTF (0, 5, 0) -> scene (0, -5, 0).
    let child = objects(parent_scene)[0];
    assert_eq!(child.transform.translate, [0.0, -5.0, 0.0]);
    // `spinner` is a plain mesh at the root.
    assert!(matches!(roots[1].content, SceneContent::Geometry(_)));

    // One glTF mesh instanced by two nodes is one shared geometry.
    assert!(Arc::ptr_eq(geometry_of(child), geometry_of(roots[1])));
}

/// World positions of the nested file, by hand. glTF space, apex
/// `C = (0, 2, 0.5)`, base `A = (-1, 0, 0)`, `B = (1, 0, 0)`.
///
/// * `child`: rotate +90 deg about Z, `(x, y) -> (-y, x)`, then `+(0, 5, 0)`
///   (child) and `+(10, 0, 0)` (parent). `A -> (10, 4, 0)`, `B -> (10, 6, 0)`,
///   `C -> (8, 5, 0.5)`. Scene space negates y and z: `A (10, -4, 0)`,
///   `B (10, -6, 0)`, `C (8, -5, -0.5)`. The normal `(0, -0.2425, 0.9701)`
///   rotates to `(0.2425, 0, 0.9701)` and converts to `(0.2425, 0, -0.9701)`.
/// * `spinner`: rotate +90 deg about X, `(y, z) -> (-z, y)`.
///   `A -> (-1, 0, 0)`, `B -> (1, 0, 0)`, `C -> (0, -0.5, 2)`; scene
///   `C (0, 0.5, -2)`. The normal becomes `(0, -0.9701, -0.2425)` and then
///   `(0, 0.9701, 0.2425)`.
///
/// A rotation sign lost in the conversion turns +90 into -90 and moves `C`
/// to a different quadrant, so this fails for it, which a translation-only
/// hierarchy would not.
#[test]
fn a_gltf_hierarchy_composes_rotations_and_translations_in_scene_space() {
    let scene = model::load(&fixture("wedge.gltf")).unwrap();
    let flat = model::flatten_to_geometry(&scene).unwrap();
    let p = points(&flat);
    let n = normals(&flat);
    assert_eq!(p.len(), 6);

    assert_close(p[0], [10.0, -4.0, 0.0], "child A");
    assert_close(p[1], [10.0, -6.0, 0.0], "child B");
    assert_close(p[2], [8.0, -5.0, -0.5], "child C");
    assert_close(p[3], [-1.0, 0.0, 0.0], "spinner A");
    assert_close(p[4], [1.0, 0.0, 0.0], "spinner B");
    assert_close(p[5], [0.0, 0.5, -2.0], "spinner C");
    for k in 0..3 {
        assert_close(n[k], [0.242536, 0.0, -0.970143], "child normal");
        assert_close(n[3 + k], [0.0, 0.970143, 0.242536], "spinner normal");
    }
    assert_eq!(flat.primitive_count(), 2);
    flat.validate().unwrap();
}

/// +90 deg about Y is the ZYX Euler singularity, and a model turned to face
/// sideways is the commonest rotation in the wild. glTF `R_y`:
/// `x' = x cos + z sin`, `z' = -x sin + z cos`, so `A (-1, 0, 0) -> (0, 0, 1)`,
/// `B (1, 0, 0) -> (0, 0, -1)`, `C (0, 2, 0.5) -> (0.5, 2, 0)`; scene space
/// negates y and z.
#[test]
fn a_quarter_turn_about_y_survives_the_euler_singularity() {
    let bin = wedge_bin();
    let json = wedge_json(
        &format!(r#"{{"byteLength":{}}}"#, bin.len()),
        r#"[{"mesh":0,"rotation":[0,0.70710678,0,0.70710678]}]"#,
        "[0]",
    );
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "turned.glb", &glb(&json, &bin));
    let flat = model::flatten_to_geometry(&model::load(&path).unwrap()).unwrap();
    let p = points(&flat);
    assert_close(p[0], [0.0, 0.0, -1.0], "A");
    assert_close(p[1], [0.0, 0.0, 1.0], "B");
    assert_close(p[2], [0.5, -2.0, 0.0], "C");
}

/// A non-uniformly scaled node must light with the inverse transpose, not
/// the matrix itself. Scaling the wedge by `(1, 2, 1)` stretches its edges to
/// `AB = (2, 0, 0)` and `AC = (1, 4, 0.5)`, whose cross product is
/// `(0, -1, 8) / sqrt(65) = (0, -0.12403, 0.99228)` in the file's basis, so
/// `(0, 0.12403, -0.99228)` in scene space. Transforming the normal by the
/// scale itself would give `(0, -0.4472, 0.8944)` instead.
#[test]
fn flattened_normals_follow_the_inverse_transpose_of_a_scale() {
    let bin = wedge_bin();
    let json = wedge_json(
        &format!(r#"{{"byteLength":{}}}"#, bin.len()),
        r#"[{"mesh":0,"scale":[1,2,1]}]"#,
        "[0]",
    );
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "scaled.glb", &glb(&json, &bin));
    let flat = model::flatten_to_geometry(&model::load(&path).unwrap()).unwrap();
    let p = points(&flat);
    assert_close(
        face_normal(p[0], p[1], p[2]),
        [0.0, 0.12403, -0.99228],
        "geometric normal",
    );
    for n in normals(&flat) {
        assert_close(n, [0.0, 0.12403, -0.99228], "baked normal");
    }
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

fn load_error(name: &str, bytes: &[u8]) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), name, bytes);
    format!("{:#}", model::load(&path).expect_err("must be refused"))
}

#[test]
fn malformed_files_are_refused_with_a_reason() {
    assert!(load_error("broken.glb", b"glTF\x02\0\0\0 not a model").contains("broken.glb"));
    assert!(load_error("broken.gltf", b"{ not json").contains("not a valid glTF"));
    assert!(load_error("empty.obj", b"# nothing here\n").contains("no triangle"));
    assert!(load_error("model.fbx", b"x").contains("not a glTF"));
}

#[test]
fn a_gltf_whose_buffer_is_shorter_than_declared_is_refused() {
    let json = wedge_json(r#"{"byteLength":80,"uri":"short.bin"}"#, UPRIGHT, "[0]");
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), "short.bin", &wedge_bin()[..40]);
    let path = write(dir.path(), "short.gltf", json.as_bytes());
    let error = format!("{:#}", model::load(&path).unwrap_err());
    assert!(error.contains("declares 80"), "{error}");
}

#[test]
fn a_gltf_with_a_missing_buffer_file_is_refused() {
    let json = wedge_json(r#"{"byteLength":80,"uri":"absent.bin"}"#, UPRIGHT, "[0]");
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "absent.gltf", json.as_bytes());
    let error = format!("{:#}", model::load(&path).unwrap_err());
    assert!(error.contains("absent.bin"), "{error}");
}

#[test]
fn an_index_past_the_vertices_is_refused_instead_of_reaching_the_renderer() {
    let mut bin = wedge_bin();
    bin[72..74].copy_from_slice(&9u16.to_le_bytes());
    let json = wedge_json(
        &format!(r#"{{"byteLength":{}}}"#, bin.len()),
        UPRIGHT,
        "[0]",
    );
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "wild.glb", &glb(&json, &bin));
    let error = format!("{:#}", model::load(&path).unwrap_err());
    assert!(error.contains("past the 3 vertices"), "{error}");
}

// ---------------------------------------------------------------------------
// The node
// ---------------------------------------------------------------------------

/// A node of the registered template with `overrides` written over its
/// default parameters, so the ports and defaults are the shipped ones.
fn node_of(type_key: &str, id: u64, overrides: &[(&str, ParameterValue)]) -> Node {
    let mut registry = NodeRegistry::new();
    register_builtins(&mut registry);
    let mut node = registry.create_node(type_key, NodeId::new(id)).unwrap();
    for (key, value) in overrides {
        let slot = node
            .parameters
            .iter_mut()
            .find(|parameter| parameter.key == *key)
            .unwrap_or_else(|| panic!("{type_key} has no {key} parameter"));
        slot.value = value.clone();
    }
    node
}

fn import_node() -> Node {
    node_of(
        "mesh.import",
        1,
        &[(
            "asset_id",
            ParameterValue::String(AssetId::new(1).to_param_value()),
        )],
    )
}

fn evaluator_with(entry: MediaAssetEntry) -> Evaluator {
    let mut ev = Evaluator::new();
    ev.set_document(Arc::new(
        Document::default().with_media_asset_entry(AssetId::new(1), entry),
    ));
    ev.register(
        NodeId::new(1),
        Arc::new(MeshImportProcessor::from_node(&import_node())),
    );
    ev
}

fn import_outputs(ev: &mut Evaluator) -> anyhow::Result<(Scene, Geometry)> {
    let graph = Graph::new().add_node(import_node()).unwrap();
    let out = ev.evaluate(&graph, NodeId::new(1), &ctx())?;
    let record = out.downcast_ref::<PortRecord>().expect("two outputs");
    Ok((
        record.0[0].downcast_ref::<Scene>().expect("scene").clone(),
        record.0[1]
            .downcast_ref::<Geometry>()
            .expect("geometry")
            .clone(),
    ))
}

#[test]
fn the_node_reads_the_asset_through_its_resolved_path() {
    let mut ev = evaluator_with(MediaAssetEntry::from_absolute(fixture("wedge.obj")));
    let (scene, geometry) = import_outputs(&mut ev).unwrap();
    assert_eq!(scene.object_count(), 2);
    assert_eq!(geometry.point_count(), 7, "3 + 4 points, merged");
    assert_eq!(geometry.primitive_count(), 2);
}

/// An offline asset (no resolved path) or a file that stopped parsing leaves
/// empty outputs, not a failed evaluation — the same degradation `media`
/// has. An unset `asset_id` is a graph mistake and stays an error.
#[test]
fn an_offline_or_unreadable_asset_degrades_to_empty_outputs() {
    let mut offline = MediaAssetEntry::from_absolute(fixture("wedge.obj"));
    offline.resolved = None;
    let (scene, geometry) = import_outputs(&mut evaluator_with(offline)).unwrap();
    assert_eq!((scene.object_count(), geometry.point_count()), (0, 0));

    let dir = tempfile::tempdir().unwrap();
    let junk = write(dir.path(), "junk.obj", b"nothing");
    let (scene, geometry) =
        import_outputs(&mut evaluator_with(MediaAssetEntry::from_absolute(junk))).unwrap();
    assert_eq!((scene.object_count(), geometry.point_count()), (0, 0));

    let unset = node_of("mesh.import", 1, &[]);
    let mut ev = Evaluator::new();
    ev.set_document(Arc::new(Document::default()));
    ev.register(
        NodeId::new(1),
        Arc::new(MeshImportProcessor::from_node(&unset)),
    );
    let graph = Graph::new().add_node(unset).unwrap();
    assert!(ev.evaluate(&graph, NodeId::new(1), &ctx()).is_err());
}

/// The parse is kept per `(path, content_revision)`: overwriting the file in
/// place must show once the revision is bumped, and must not re-read before.
#[test]
fn the_parse_is_reused_until_the_content_revision_moves() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("model.obj");
    std::fs::copy(fixture("wedge.obj"), &path).unwrap();
    // One processor across runs, as the evaluator keeps one per node; a fresh
    // evaluator each time so its own result cache cannot answer instead.
    let processor = Arc::new(MeshImportProcessor::from_node(&import_node()));
    let point_count = |revision: u64| {
        let mut entry = MediaAssetEntry::from_absolute(&path);
        entry.content_revision = revision;
        let mut ev = Evaluator::new();
        ev.set_document(Arc::new(
            Document::default().with_media_asset_entry(AssetId::new(1), entry),
        ));
        ev.register(NodeId::new(1), processor.clone());
        import_outputs(&mut ev).unwrap().1.point_count()
    };
    assert_eq!(point_count(0), 7);
    std::fs::write(&path, "o one\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n").unwrap();
    assert_eq!(point_count(0), 7, "same revision keeps the parsed model");
    assert_eq!(point_count(1), 3, "a new revision re-reads the file");
}

// ---------------------------------------------------------------------------
// Imported meshes work with the geometry and field nodes
// ---------------------------------------------------------------------------

/// `mesh.import` -> `geometry.transform` -> `field.apply` (alpha) -> drawn.
/// The mesh survives both operators, the field writes a column on it, and the
/// result renders translucent where the model is.
#[test]
fn an_imported_mesh_can_be_transformed_and_modulated_then_rendered() {
    use ravel_nodes::field::{ApplyFieldProcessor, ConstantFieldProcessor};
    use ravel_nodes::geometry::GeometryTransformProcessor;

    let import = import_node();
    let transform = node_of(
        "geometry.transform",
        2,
        &[
            ("translate", ParameterValue::vec3(32.0, 50.0, 0.0)),
            ("scale", ParameterValue::vec3(10.0, 10.0, 10.0)),
            ("use_centroid", ParameterValue::Bool(false)),
        ],
    );
    let constant = node_of(
        "field.constant",
        3,
        &[("value", ParameterValue::Float(0.5))],
    );
    let apply = node_of(
        "field.apply",
        4,
        &[("target", ParameterValue::String("alpha".into()))],
    );

    let mut graph = Graph::new();
    for node in [&import, &transform, &constant, &apply] {
        graph = graph.add_node(node.clone()).unwrap();
    }
    for (edge, from, from_port, to, to_port) in [
        (1u64, 1u64, 1u32, 2u64, 0u32),
        (2, 2, 0, 4, 0),
        (3, 3, 0, 4, 1),
    ] {
        graph = graph
            .add_edge(
                EdgeId::new(edge),
                NodeId::new(from),
                OutputPortIndex(from_port),
                NodeId::new(to),
                InputPortIndex(to_port),
            )
            .unwrap();
    }
    let mut ev = evaluator_with(MediaAssetEntry::from_absolute(fixture("wedge.obj")));
    ev.register(
        NodeId::new(2),
        Arc::new(GeometryTransformProcessor::from_node(&transform)),
    );
    ev.register(NodeId::new(3), Arc::new(ConstantFieldProcessor));
    ev.register(
        NodeId::new(4),
        Arc::new(ApplyFieldProcessor::from_node(&apply)),
    );
    let out = ev.evaluate(&graph, NodeId::new(4), &ctx()).unwrap();
    let modulated = out.downcast_ref::<Geometry>().expect("geometry").clone();

    assert!(
        modulated
            .primitives()
            .iter()
            .all(|p| matches!(p, Primitive::Mesh { .. }))
    );
    assert_eq!(modulated.primitive_count(), 2);
    let alphas = modulated.points().get(names::ALPHA).expect("alpha column");
    assert!(
        alphas
            .as_f32(names::ALPHA)
            .unwrap()
            .iter()
            .all(|a| (a - 0.5).abs() < 1e-6),
        "the field wrote 0.5 on every vertex"
    );
    // Scale 10 about the origin then +(32, 50, 0): wedge apex scene
    // (0, -2, -0.5) -> (32, 30, -5).
    let apex = points(&modulated)[2];
    assert_close(apex, [32.0, 30.0, -5.0], "transformed apex");

    let frame = render(
        &Scene::from_camera(pixel_camera(64)).with_object(SceneObject::geometry(
            Arc::new(modulated),
            Transform3D::IDENTITY,
        )),
    );
    let a = alpha(&frame, 32, 40);
    assert!(
        (a - 0.5).abs() < 0.05,
        "translucent where the wedge is, got {a}"
    );
    assert!(alpha(&frame, 32, 25) < 0.01, "and empty above it");
}
