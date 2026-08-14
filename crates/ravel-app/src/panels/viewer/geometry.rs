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
use ravel_core::geometry::{AttributeArray, Domain, Geometry};
use ravel_core::id::{DataTypeId, NodeId, OutputPortIndex};
use ravel_core::types::{NodeData, magnitude};
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

/// Upper bound on element index labels drawn per geometry.
///
/// Far tighter than [`MAX_DRAWN_POINTS`] because a label is text: two thousand
/// numbers over a point cloud is an unreadable grey block, and each one is a
/// GPUI element rather than a quad.
pub const MAX_DRAWN_LABELS: usize = 64;

/// The even stride that keeps `count` elements under `max`.
///
/// The one place the "thin it out, never truncate" rule is expressed: a cap
/// applied by `take` would keep only the first corner of a point cloud, while
/// striding keeps what is drawn representative of the whole.
pub fn stride_for(count: usize, max: usize) -> usize {
    count.div_ceil(max.max(1)).max(1)
}

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

/// One drawn element of a geometry.
///
/// The overlay's single unit of "an element is here": the point marker, the
/// attribute arrow, the index label and the group colour are all derived from
/// the same mark, so a label cannot end up on an element whose arrow was drawn
/// somewhere else, and the row an attribute column is read at is the row the
/// position came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeometryMark {
    /// Which domain the element belongs to — attribute columns are per domain.
    pub domain: Domain,
    /// Element index inside `domain`: the number an index label shows and the
    /// row every attribute column is read at.
    pub index: usize,
    /// Position in the geometry's own (layer-local) space.
    pub position: (f32, f32),
}

/// Every position a geometry places, points and instances alike, capped at
/// [`MAX_DRAWN_POINTS`] per domain by even striding.
///
/// Instances are included because that is what makes a `scatter.*` visible:
/// its copies live in the instance domain and the point domain holds only the
/// source it scattered along.
pub fn geometry_marks(geometry: &Geometry) -> Vec<GeometryMark> {
    let mut out = Vec::new();
    for domain in [Domain::Point, Domain::Instance] {
        let Some(Ok(positions)) = geometry.positions(domain) else {
            continue;
        };
        let count = positions.len();
        for index in (0..count).step_by(stride_for(count, MAX_DRAWN_POINTS)) {
            if let Some(p) = positions.get3(index) {
                out.push(GeometryMark {
                    domain,
                    index,
                    position: (p.0, p.1),
                });
            }
        }
    }
    out
}

/// The marks that carry an index label: [`geometry_marks`] thinned again to
/// [`MAX_DRAWN_LABELS`].
///
/// A second stride rather than a second walk, so every label sits on a mark
/// that was actually drawn.
pub fn label_marks(marks: &[GeometryMark]) -> Vec<GeometryMark> {
    marks
        .iter()
        .copied()
        .step_by(stride_for(marks.len(), MAX_DRAWN_LABELS))
        .collect()
}

/// The planar reading of the attribute `name` on `domain`, or `None` when the
/// geometry has no such column or it holds something with no direction.
///
/// `Vec3` and `Vec4` columns are projected rather than refused: a 3D normal
/// still has a 2D direction on the canvas the overlay draws on, which is what
/// makes `N` drawable at all.
pub fn vector_column(geometry: &Geometry, domain: Domain, name: &str) -> Option<Vec<(f32, f32)>> {
    super::field::planar_values(geometry.attribute_set(domain).get(name)?)
}

/// Every planar-vector attribute name a geometry carries on the drawn domains,
/// sorted, for the picker that chooses which one to draw.
///
/// `P` is left out although it is a vector column: an arrow from a point to its
/// own coordinates doubled says nothing, and the position is already drawn as
/// the mark the arrow would start from.
pub fn vector_attribute_names(geometry: &Geometry) -> Vec<String> {
    let mut names: Vec<String> = [Domain::Point, Domain::Instance]
        .into_iter()
        .flat_map(|domain| geometry.attribute_set(domain).iter())
        .filter(|(name, column)| {
            name.as_str() != ravel_core::geometry::names::P
                && super::field::planar_values(column).is_some()
        })
        .map(|(name, _)| name.to_string())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Arrow segments for the vector attribute `name`, in the geometry's own
/// space: one `(tail, tip)` pair per mark that has a finite vector there.
///
/// `reach` bounds the longest arrow. The scale only ever *shrinks*
/// (`min(1, reach / longest)`), so a velocity of `(3, 4)` is drawn five units
/// long — the value itself, which is what makes an arrow readable as a
/// magnitude — while a vector that would streak across the whole picture is
/// pulled back to `reach`. Non-finite vectors are skipped rather than scaled:
/// one `NaN` reaching `longest` would shorten every other arrow to nothing.
pub fn attribute_arrows(
    geometry: &Geometry,
    marks: &[GeometryMark],
    name: &str,
    reach: f32,
) -> Vec<((f32, f32), (f32, f32))> {
    let columns = [
        (Domain::Point, vector_column(geometry, Domain::Point, name)),
        (
            Domain::Instance,
            vector_column(geometry, Domain::Instance, name),
        ),
    ];
    let vector_of = |mark: &GeometryMark| {
        let column = columns
            .iter()
            .find(|(domain, _)| *domain == mark.domain)?
            .1
            .as_ref()?;
        let vector = *column.get(mark.index)?;
        (vector.0.is_finite() && vector.1.is_finite()).then_some(vector)
    };
    let longest = marks
        .iter()
        .filter_map(vector_of)
        .map(|v| magnitude(2, [v.0, v.1, 0.0, 0.0]))
        .fold(0.0f32, f32::max);
    if !longest.is_finite() || longest <= f32::EPSILON {
        return Vec::new();
    }
    let scale = (reach / longest).min(1.0);
    marks
        .iter()
        .filter_map(|mark| {
            let vector = vector_of(mark)?;
            Some((
                mark.position,
                (
                    mark.position.0 + vector.0 * scale,
                    mark.position.1 + vector.1 * scale,
                ),
            ))
        })
        .collect()
}

/// The groups of `domain`, in name order.
///
/// A group is a `Bool` attribute — Ravel declares no group type of its own
/// (`docs/specifications/procedural-geometry.md`, 要素スコープ), so the
/// columns of that type *are* the groups. Name order rather than the
/// attribute set's hash order, so "the first group an element is in" is a
/// stable answer.
pub fn group_columns(geometry: &Geometry, domain: Domain) -> Vec<(&str, &[bool])> {
    let mut groups: Vec<(&str, &[bool])> = geometry
        .attribute_set(domain)
        .iter()
        .filter_map(|(name, column)| match column.as_ref() {
            AttributeArray::Bool(values) => Some((name.as_str(), values.as_slice())),
            _ => None,
        })
        .collect();
    groups.sort_by_key(|(name, _)| *name);
    groups
}

/// The first group of `columns` that element `index` belongs to.
///
/// The name rather than its position in the list: the colour is derived from
/// the name, so a group keeps its colour whichever domain it sits on and
/// whichever other groups appear beside it.
pub fn mark_group<'a>(columns: &[(&'a str, &[bool])], index: usize) -> Option<&'a str> {
    columns
        .iter()
        .find(|(_, values)| values.get(index) == Some(&true))
        .map(|(name, _)| *name)
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
                let points: Vec<_> = verts
                    .clone()
                    .step_by(stride_for(count, MAX_PATH_VERTICES))
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
        let points = geometry_marks(&geometry);
        assert!(points.len() <= MAX_DRAWN_POINTS, "{}", points.len());
        // Striding, not truncation: the last element of the cloud is reached.
        assert!(
            points.last().expect("points drawn").position.0 as usize >= count - 3,
            "the cap kept only the head of the cloud: {:?}",
            points.last()
        );
    }

    /// Two points carrying a `velocity` column, the particle case unit 8 is
    /// asked to draw without a path of its own.
    fn moving_points() -> Geometry {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0)]);
        geometry
            .points_mut()
            .insert(
                ravel_core::geometry::names::VELOCITY,
                AttributeArray::Vec2(vec![Vec2(3.0, 4.0), Vec2(-6.0, 8.0)]),
            )
            .unwrap();
        geometry
    }

    /// Completion criterion: a known `Vec2` attribute is drawn as arrows whose
    /// direction *and* length are the values themselves.
    #[test]
    fn a_vec2_attribute_becomes_arrows_of_its_own_direction_and_length() {
        let geometry = moving_points();
        let marks = geometry_marks(&geometry);
        // The longest vector here is 10 units, well inside a 100-unit reach, so
        // nothing is shortened and the arrows are the attribute.
        let arrows = attribute_arrows(&geometry, &marks, "velocity", 100.0);
        assert_eq!(
            arrows,
            vec![((0.0, 0.0), (3.0, 4.0)), ((10.0, 0.0), (4.0, 8.0)),]
        );

        // The cap only ever shortens, and it shortens every arrow by the same
        // factor: halving the reach halves both.
        let capped = attribute_arrows(&geometry, &marks, "velocity", 5.0);
        assert_eq!(
            capped,
            vec![((0.0, 0.0), (1.5, 2.0)), ((10.0, 0.0), (7.0, 4.0)),]
        );
    }

    /// Completion criterion: a geometry without the attribute draws nothing.
    #[test]
    fn an_absent_or_directionless_attribute_draws_no_arrows() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0)]);
        let marks = geometry_marks(&geometry);
        assert!(attribute_arrows(&geometry, &marks, "velocity", 100.0).is_empty());

        // A column of the right name and the wrong type has no direction in it.
        geometry
            .points_mut()
            .insert("velocity", AttributeArray::F32(vec![1.0, 2.0]))
            .unwrap();
        assert!(attribute_arrows(&geometry, &marks, "velocity", 100.0).is_empty());
        assert!(vector_column(&geometry, Domain::Point, "velocity").is_none());
        assert!(vector_attribute_names(&geometry).is_empty());
    }

    /// A `NaN` is not a short arrow, it is an unpaintable one — and letting it
    /// reach `longest` would shrink every other arrow to nothing.
    #[test]
    fn a_non_finite_vector_is_skipped_without_shrinking_the_others() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0)]);
        geometry
            .points_mut()
            .insert(
                "velocity",
                AttributeArray::Vec2(vec![Vec2(3.0, 4.0), Vec2(f32::NAN, f32::INFINITY)]),
            )
            .unwrap();
        let marks = geometry_marks(&geometry);
        assert_eq!(
            attribute_arrows(&geometry, &marks, "velocity", 100.0),
            vec![((0.0, 0.0), (3.0, 4.0))]
        );
    }

    /// Completion criterion: past the cap the labels are thinned, and thinned
    /// rather than truncated — the end of the cloud still carries one.
    #[test]
    fn index_labels_are_thinned_past_their_cap() {
        let count = MAX_DRAWN_LABELS * 3 + 1;
        let geometry =
            Geometry::from_points((0..count).map(|i| Vec2(i as f32, 0.0)).collect::<Vec<_>>());
        let marks = geometry_marks(&geometry);
        assert_eq!(marks.len(), count, "the mark cap did not apply here");
        let labels = label_marks(&marks);
        assert!(
            labels.len() <= MAX_DRAWN_LABELS,
            "{} labels drawn",
            labels.len()
        );
        assert!(
            labels.last().expect("labels drawn").index >= count - 4,
            "the cap kept only the head of the cloud: {:?}",
            labels.last()
        );
        // Under the cap nothing is dropped.
        let few = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0)]);
        assert_eq!(label_marks(&geometry_marks(&few)).len(), 2);
    }

    /// Groups are `Bool` columns (`procedural-geometry.md`, 要素スコープ), and
    /// "the first group" is decided in name order so a colour is stable.
    #[test]
    fn groups_are_the_bool_columns_in_name_order() {
        let mut geometry =
            Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0), Vec2(2.0, 0.0)]);
        geometry
            .points_mut()
            .insert("odd", AttributeArray::Bool(vec![false, true, false]))
            .unwrap();
        geometry
            .points_mut()
            .insert("even", AttributeArray::Bool(vec![true, false, false]))
            .unwrap();
        geometry
            .points_mut()
            .insert("pscale", AttributeArray::F32(vec![1.0, 1.0, 1.0]))
            .unwrap();
        let columns = group_columns(&geometry, Domain::Point);
        assert_eq!(
            columns.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            vec!["even", "odd"],
            "a non-bool column was taken for a group, or the order is unstable"
        );
        assert_eq!(mark_group(&columns, 0), Some("even"));
        assert_eq!(mark_group(&columns, 1), Some("odd"));
        assert_eq!(mark_group(&columns, 2), None, "element 2 is in no group");
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
