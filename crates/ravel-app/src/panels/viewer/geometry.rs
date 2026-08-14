// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What the Viewer reads off an *evaluated* geometry.
//!
//! Before this module the Viewer reconstructed a shape's rectangle from its
//! parameters, through a `match` on `type_key` that had to be edited for every
//! new shape node and that no operator downstream of it — `geometry.transform`,
//! `scatter.*` — was reflected in. The bounds, the points and the paths drawn
//! now all come from the value the evaluator produced, so a node this crate has
//! never heard of outlines correctly and a transformed shape outlines where it
//! actually is.
//!
//! Nothing here guesses. A target whose result has not arrived, failed, or is
//! not a geometry reads as `None`, and the overlay draws nothing rather than
//! falling back to a parameter reading — a bbox that jumps from an estimate to
//! the truth is worse than one that appears a frame late.

use ravel_core::composition::Document;
use ravel_core::geometry::{Domain, Geometry};
use ravel_core::id::{DataTypeId, NodeId, OutputPortIndex};
use ravel_core::types::NodeData;
use ravel_ui::document::NetworkPath;
use std::sync::Arc;

use super::CompRect;
use super::overlay::{OverlayContext, OverlayTarget};

/// Upper bound on the marks one geometry contributes, per kind.
///
/// A scatter easily places tens of thousands of instances; drawing one screen
/// square each would cost more than the frame under it and read as a solid
/// block anyway. The cap is on *elements sampled*, evenly strided, so what is
/// drawn stays representative of the whole rather than of its first corner.
pub const MAX_DRAWN_POINTS: usize = 2000;

/// Upper bound on path primitives outlined, for the same reason.
pub const MAX_DRAWN_PATHS: usize = 256;

/// Upper bound on vertices drawn per path primitive.
pub const MAX_PATH_VERTICES: usize = 2000;

/// The geometry behind an evaluated value, or `None` when the node produced
/// something else.
pub fn as_geometry(value: &Arc<dyn NodeData>) -> Option<&Geometry> {
    value.downcast_ref::<Geometry>()
}

/// Axis-aligned bounds of everything a geometry places: point positions and
/// instance positions together.
///
/// The **union** of the two domains, unlike
/// [`ravel_core::geometry::ops::bounds_center`], which takes the first
/// non-empty one. A bbox that dropped the instance domain would cut off a
/// scatter's copies; one that dropped points would cut off the curve they were
/// scattered along. `None` when the geometry places nothing at all — an empty
/// geometry has no rectangle, and a zero-sized one at the origin would be a
/// lie drawn on screen.
///
/// **Walked in full on every call, including once per pointer move** (the hover
/// hint asks for the selected nodes' bounds, and a click asks for every node's).
/// Measured rather than assumed, release build, per call:
///
/// | points | per call |
/// |---|---|
/// | 1 000 | 0.37 µs |
/// | 10 000 | 1.9 µs |
/// | 100 000 | 20 µs |
/// | 1 000 000 | 197 µs |
///
/// A pointer move pays this for the handful of selected nodes, so even a
/// hundred-thousand-point geometry costs ~0.1% of a 60 Hz frame. Caching the
/// rectangle at press time would buy that back and cost a second source of
/// truth for what the bbox is — worth doing only if a profile ever shows this
/// line, which at these numbers it will not. Unlike `MED-GPU-04`, the work here
/// is `O(points)` once per input event, not `O(primitives x resolution)` per
/// frame.
pub fn geometry_bounds(geometry: &Geometry) -> Option<CompRect> {
    let (mut min_x, mut min_y) = (f32::INFINITY, f32::INFINITY);
    let (mut max_x, mut max_y) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    let mut any = false;
    for domain in [Domain::Point, Domain::Instance] {
        let Some(Ok(positions)) = geometry.positions(domain) else {
            continue;
        };
        for index in 0..positions.len() {
            let Some(p) = positions.get3(index) else {
                continue;
            };
            any = true;
            min_x = min_x.min(p.0);
            min_y = min_y.min(p.1);
            max_x = max_x.max(p.0);
            max_y = max_y.max(p.1);
        }
    }
    any.then_some(CompRect {
        x: min_x,
        y: min_y,
        w: max_x - min_x,
        h: max_y - min_y,
    })
}

/// Every position a geometry places, points and instances alike, capped at
/// [`MAX_DRAWN_POINTS`] per domain by even striding.
///
/// Instances are included because that is what makes a `scatter.*` visible:
/// its copies live in the instance domain and the point domain holds only the
/// source it scattered along.
pub fn geometry_points(geometry: &Geometry) -> Vec<(f32, f32)> {
    let mut out = Vec::new();
    for domain in [Domain::Point, Domain::Instance] {
        let Some(Ok(positions)) = geometry.positions(domain) else {
            continue;
        };
        let count = positions.len();
        let stride = count.div_ceil(MAX_DRAWN_POINTS).max(1);
        for index in (0..count).step_by(stride) {
            if let Some(p) = positions.get3(index) {
                out.push((p.0, p.1));
            }
        }
    }
    out
}

/// The path primitives of a geometry as polylines, each with its closed flag.
///
/// Meshes are skipped: an outline of a triangulated fill is the fill's own
/// silhouette, which the bbox already stands in for, and drawing every edge of
/// a dense mesh would obscure the picture it annotates.
pub fn geometry_paths(geometry: &Geometry) -> Vec<(Vec<(f32, f32)>, bool)> {
    let Some(Ok(positions)) = geometry.positions(Domain::Point) else {
        return Vec::new();
    };
    geometry
        .primitives()
        .iter()
        .filter_map(|primitive| match primitive {
            ravel_core::geometry::Primitive::Path { verts, closed } => {
                let count = verts.len();
                let stride = count.div_ceil(MAX_PATH_VERTICES).max(1);
                let points: Vec<_> = verts
                    .clone()
                    .step_by(stride)
                    .filter_map(|index| positions.get3(index).map(|p| (p.0, p.1)))
                    .collect();
                (points.len() >= 2).then_some((points, *closed))
            }
            ravel_core::geometry::Primitive::Mesh { .. } => None,
        })
        .take(MAX_DRAWN_PATHS)
        .collect()
}

/// The evaluated geometry of `node` in `network`, in that network's own
/// (layer-local) coordinate space.
pub fn evaluated_geometry(
    ctx: &OverlayContext,
    network: &NetworkPath,
    node: NodeId,
) -> Option<Arc<dyn NodeData>> {
    ctx.eval_result(&OverlayTarget {
        network: network.clone(),
        node,
        output: geometry_output_port(ctx.document.as_ref()?, network, node)?,
    })
}

/// Bounds of `node`'s evaluated geometry, in the network's own space.
pub fn evaluated_bounds(
    ctx: &OverlayContext,
    network: &NetworkPath,
    node: NodeId,
) -> Option<CompRect> {
    geometry_bounds(as_geometry(&evaluated_geometry(ctx, network, node)?)?)
}

/// The output port of `node` that carries geometry, if it has one.
///
/// Declared by the node instance rather than by a `type_key` table, which is
/// the whole point: a shape node nobody has written yet already declares a
/// `GEOMETRY` output and is therefore already drawable.
pub fn geometry_output_port(
    document: &Document,
    network: &NetworkPath,
    node: NodeId,
) -> Option<OutputPortIndex> {
    let node = ravel_ui::document::resolve_network(document, network)?.node(node)?;
    geometry_port_of(node)
}

fn geometry_port_of(node: &ravel_core::graph::Node) -> Option<OutputPortIndex> {
    node.outputs
        .iter()
        .position(|port| port.data_type == DataTypeId::GEOMETRY)
        .map(|index| OutputPortIndex(index as u32))
}

/// Every node of `network` that declares a geometry output, as overlay
/// targets.
///
/// All of them rather than only the selected ones, because the Viewer's click
/// test picks a node by the geometry it draws: bounds for the unselected nodes
/// are what makes a shape selectable at all. They cost little — the shell
/// evaluation for the frame underneath has already run every node upstream of
/// the layer's output, so those are cache hits.
pub fn geometry_targets(document: &Document, network: &NetworkPath) -> Vec<OverlayTarget> {
    let Some(graph) = ravel_ui::document::resolve_network(document, network) else {
        return Vec::new();
    };
    graph
        .nodes()
        .filter_map(|node| {
            Some(OverlayTarget {
                network: network.clone(),
                node: node.id,
                output: geometry_port_of(node)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::geometry::Primitive;
    use ravel_core::types::Vec2;

    #[test]
    fn bounds_cover_points_and_instances_together() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(2.0, 1.0)]);
        geometry
            .instances_mut()
            .insert(
                ravel_core::geometry::names::P,
                ravel_core::geometry::AttributeArray::Vec2(vec![Vec2(-1.0, 5.0)]),
            )
            .unwrap();
        let bounds = geometry_bounds(&geometry).expect("both domains place something");
        assert_eq!(bounds.x, -1.0);
        assert_eq!(bounds.y, 0.0);
        assert_eq!(bounds.w, 3.0);
        assert_eq!(bounds.h, 5.0);
    }

    #[test]
    fn an_empty_geometry_has_no_bounds() {
        assert!(geometry_bounds(&Geometry::new()).is_none());
    }

    #[test]
    fn points_are_strided_rather_than_truncated() {
        let count = MAX_DRAWN_POINTS * 3;
        let geometry =
            Geometry::from_points((0..count).map(|i| Vec2(i as f32, 0.0)).collect::<Vec<_>>());
        let points = geometry_points(&geometry);
        assert!(points.len() <= MAX_DRAWN_POINTS, "{}", points.len());
        // Striding, not truncation: the last element of the cloud is reached.
        assert!(
            points.last().expect("points drawn").0 as usize >= count - 3,
            "the cap kept only the head of the cloud: {:?}",
            points.last()
        );
    }

    #[test]
    fn a_path_primitive_becomes_a_polyline_and_a_mesh_does_not() {
        let mut geometry = Geometry::from_points(vec![
            Vec2(0.0, 0.0),
            Vec2(1.0, 0.0),
            Vec2(1.0, 1.0),
            Vec2(5.0, 5.0),
            Vec2(6.0, 5.0),
            Vec2(6.0, 6.0),
        ]);
        geometry.push_primitive(Primitive::Path {
            verts: 0..3,
            closed: true,
        });
        geometry.push_mesh(3..6, &[0, 1, 2]);
        let paths = geometry_paths(&geometry);
        assert_eq!(paths.len(), 1, "the mesh was outlined too");
        assert_eq!(paths[0].0.len(), 3);
        assert!(paths[0].1, "the closed flag was dropped");
    }
}
