// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `.ravprj` v5 → v6: convert the control points a curve parameter used to
//! store as text into a [`ParameterValue::Curve`].
//!
//! Before v6, `field.curve_remap` kept its control points in a
//! `"in:out,in:out"` string that Properties offered as a plain text field.
//! Node parameters are free key/value pairs, so a v5 document's
//! `points: String(..)` still deserializes intact — it merely stops matching
//! what the template declares. The upgrade is therefore a typed pass over the
//! loaded [`Document`](super::Document), like the v4 → v5 vector fold beside
//! it, rather than a step in the untyped `manifest.json` chain (which never
//! sees `document/main.ron`).
//!
//! The conversion preserves behaviour exactly: the v5 string was read as
//! straight lines between its control points, so the upgraded curve uses
//! [`Interpolation::Linear`] and the same points, and
//! [`CurveParam::evaluate`] clamps outside the point range exactly as the old
//! reader did.
//!
//! A string that cannot be read falls back to [`CurveParam::identity`] and is
//! logged. Silently dropping a curve would change a composition's look with
//! no trace; the identity curve at least leaves the field it remaps intact.

use crate::graph::{Graph, ParameterValue};
use crate::param_curve::CurveParam;
use std::sync::Arc;

/// Every `(type_key, parameter key)` whose curve was stored as text before
/// v6. New curve parameters are declared as [`ParameterValue::Curve`] from
/// the start and never appear here.
const TEXT_CURVE_PARAMS: &[(&str, &str)] = &[("field.curve_remap", "points")];

/// Parse the v5 control-point string: `"0:0,0.5:0.8,1:1"`.
///
/// `None` when any non-empty entry is not an `input:output` pair of finite
/// numbers, or when the string carries no points at all. Partial parsing is
/// deliberately rejected — a curve missing one of its control points has a
/// different shape, and silently reshaping it is the failure this upgrade
/// exists to avoid.
fn parse_v5_points(text: &str) -> Option<CurveParam> {
    let mut points: Vec<(f32, f32)> = Vec::new();
    for entry in text.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (input, output) = entry.split_once(':')?;
        let x: f32 = input.trim().parse().ok()?;
        let y: f32 = output.trim().parse().ok()?;
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        points.push((x, y));
    }
    (!points.is_empty()).then(|| CurveParam::linear(points))
}

/// Upgrade every text-stored curve parameter in `graph`, descending into
/// subnets.
pub(super) fn upgrade_graph(graph: &Graph) -> Graph {
    super::graph_walk::map_subnets(graph, &upgrade_level)
}

/// Upgrade one graph's own nodes, ignoring its subnets — the shared walk
/// visits those separately.
fn upgrade_level(graph: &Graph) -> Graph {
    let mut upgraded = graph.clone();
    for id in upgraded.node_ids().collect::<Vec<_>>() {
        let Some(node) = upgraded.node(id) else {
            continue;
        };
        let keys: Vec<&str> = TEXT_CURVE_PARAMS
            .iter()
            .filter(|(type_key, _)| *type_key == node.type_key)
            .map(|(_, key)| *key)
            .collect();
        if keys.is_empty() {
            continue;
        }
        let mut updated = (**node).clone();
        let mut changed = false;
        for param in updated.parameters.iter_mut() {
            if !keys.contains(&param.key.as_str()) {
                continue;
            }
            // A parameter that is already a `Curve` is a v6 document (or a
            // graph upgraded earlier in this pass); anything else is left for
            // the processor to fall back on.
            let ParameterValue::String(text) = &param.value else {
                continue;
            };
            let curve = parse_v5_points(text).unwrap_or_else(|| {
                tracing::warn!(
                    node = id.raw(),
                    key = param.key,
                    stored = text,
                    "unreadable curve control points; using the identity curve"
                );
                CurveParam::identity()
            });
            param.value = ParameterValue::Curve(curve);
            changed = true;
        }
        if changed {
            upgraded = upgraded.replace_node(Arc::new(updated));
        }
    }
    upgraded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::interpolation::Interpolation;
    use crate::id::{DataTypeId, NodeId};

    /// A v5 `field.curve_remap` with its control points stored as text.
    fn v5_curve(id: u64, points: &str) -> crate::graph::Node {
        crate::graph::Node::new(NodeId::new(id), "field.curve_remap")
            .with_input("field", &[DataTypeId::FIELD])
            .with_output("field", DataTypeId::FIELD)
            .with_param("points", ParameterValue::String(points.into()))
    }

    fn curve_of(graph: &Graph, id: u64) -> CurveParam {
        graph
            .node(NodeId::new(id))
            .unwrap_or_else(|| panic!("node {id}"))
            .parameters
            .iter()
            .find(|p| p.key == "points")
            .unwrap_or_else(|| panic!("node {id} points"))
            .value
            .as_curve()
            .unwrap_or_else(|| panic!("node {id} points is not a Curve"))
            .clone()
    }

    #[test]
    fn a_stored_string_becomes_the_same_curve() {
        let graph = Graph::new()
            .add_node(v5_curve(1, "0:0,0.5:0.8,1:1"))
            .unwrap();
        let curve = curve_of(&upgrade_graph(&graph), 1);
        let xs: Vec<f32> = curve.points().iter().map(|p| p.x).collect();
        let ys: Vec<f32> = curve.points().iter().map(|p| p.y).collect();
        assert_eq!(xs, vec![0.0, 0.5, 1.0]);
        assert_eq!(ys, vec![0.0, 0.8, 1.0]);
        assert!(
            curve
                .points()
                .iter()
                .all(|p| p.interpolation == Interpolation::Linear),
            "the v5 reader drew straight lines between control points"
        );
    }

    /// Whitespace and a trailing separator were accepted by the v5 reader and
    /// still are.
    #[test]
    fn spacing_and_a_trailing_separator_are_tolerated() {
        let graph = Graph::new()
            .add_node(v5_curve(1, " 0 : 0 , 1 : 2 ,"))
            .unwrap();
        let curve = curve_of(&upgrade_graph(&graph), 1);
        assert_eq!(curve.len(), 2);
        assert_eq!(curve.evaluate(0.5), 1.0);
    }

    #[test]
    fn unparseable_points_fall_back_to_the_identity_curve() {
        for stored in ["", "garbage", "0:0,broken", "0:zero", "nan:1"] {
            let graph = Graph::new().add_node(v5_curve(1, stored)).unwrap();
            assert_eq!(
                curve_of(&upgrade_graph(&graph), 1),
                CurveParam::identity(),
                "{stored:?}"
            );
        }
    }

    /// The v5 reader, reproduced verbatim from the implementation this
    /// upgrade replaced (`parse_curve` in `ravel-nodes` plus `remap_curve` in
    /// `geometry::field`). The upgraded curve must agree with it everywhere,
    /// which is what "an old project evaluates the same" means.
    fn v5_remap(text: &str, value: f32) -> f32 {
        let mut points: Vec<(f32, f32)> = text
            .split(',')
            .filter_map(|point| {
                let (input, output) = point.split_once(':')?;
                Some((input.trim().parse().ok()?, output.trim().parse().ok()?))
            })
            .collect();
        if points.is_empty() {
            points = vec![(0.0, 0.0), (1.0, 1.0)];
        }
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        let Some(&(first_x, first_y)) = points.first() else {
            return value;
        };
        if value <= first_x {
            return first_y;
        }
        for pair in points.windows(2) {
            let [(x0, y0), (x1, y1)] = pair else {
                continue;
            };
            if value <= *x1 {
                let width = x1 - x0;
                return if width.abs() <= f32::EPSILON {
                    *y1
                } else {
                    y0 + (y1 - y0) * ((value - x0) / width)
                };
            }
        }
        points.last().map_or(value, |point| point.1)
    }

    #[test]
    fn an_upgraded_curve_evaluates_exactly_as_the_v5_reader_did() {
        for stored in [
            "0:0,1:1",
            "0:0,0.5:0.8,1:1",
            "0:1,1:0",
            "1:10,0:0,0.5:2",
            "-1:-4,0:0,2:6",
            "0.25:3",
        ] {
            let graph = Graph::new().add_node(v5_curve(1, stored)).unwrap();
            let curve = curve_of(&upgrade_graph(&graph), 1);
            for step in -20..=40 {
                let x = step as f32 / 20.0;
                let (before, after) = (v5_remap(stored, x), curve.evaluate(x));
                assert!(
                    (before - after).abs() < 1e-6,
                    "{stored:?} at {x}: {before} != {after}"
                );
            }
        }
    }

    #[test]
    fn upgrading_is_idempotent() {
        let graph = Graph::new().add_node(v5_curve(1, "0:0,1:4")).unwrap();
        let once = upgrade_graph(&graph);
        assert_eq!(curve_of(&upgrade_graph(&once), 1), curve_of(&once, 1));
    }

    #[test]
    fn subnet_inner_graphs_are_upgraded() {
        let inner = Graph::new().add_node(v5_curve(1, "0:0,1:3")).unwrap();
        let outer = Graph::new()
            .add_node(
                crate::graph::Node::new(NodeId::new(2), "subnet")
                    .with_subnet(inner)
                    .with_output("out", DataTypeId::FIELD),
            )
            .unwrap();
        let subnet = upgrade_graph(&outer)
            .node(NodeId::new(2))
            .unwrap()
            .subnet
            .clone()
            .expect("subnet preserved");
        assert_eq!(curve_of(&subnet, 1).evaluate(1.0), 3.0);
    }

    /// Nodes of other types keep their string parameters: only the declared
    /// curve parameters are retyped.
    #[test]
    fn unrelated_string_parameters_are_untouched() {
        let node = crate::graph::Node::new(NodeId::new(1), "field.expression")
            .with_output("field", DataTypeId::FIELD)
            .with_param("expression", ParameterValue::String("0:0,1:1".into()));
        let upgraded = upgrade_graph(&Graph::new().add_node(node).unwrap());
        assert_eq!(
            upgraded
                .node(NodeId::new(1))
                .unwrap()
                .parameters
                .iter()
                .find(|p| p.key == "expression")
                .map(|p| &p.value),
            Some(&ParameterValue::String("0:0,1:1".into()))
        );
    }
}
