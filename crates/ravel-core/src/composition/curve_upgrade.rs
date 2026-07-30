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
//! # Matching the v5 reader
//!
//! The old pipeline was `parse_curve` (in `ravel-nodes`) feeding
//! `CurveRemapField::new`, which sorted the points, and `remap_curve`, which
//! walked them as straight segments and held both ends. The upgraded curve
//! reproduces that, so an old project evaluates the same:
//!
//! * **Unreadable entries are skipped, not fatal.** `parse_curve` collected
//!   through a `filter_map`, so `"0:0,broken,1:1"` was a two-point curve.
//!   Rejecting the whole string would silently reshape such a file. Only a
//!   string with *no* readable point falls back to [`CurveParam::identity`].
//! * **Order does not matter.** `CurveRemapField::new` sorted by input value
//!   before evaluating, so a stored string in any order behaved as if sorted.
//! * **Repeated inputs keep the last point** (see the divergence below).
//! * Segments are [`Interpolation::Linear`] and [`CurveParam::evaluate`]
//!   clamps outside the point range, as the old reader did.
//!
//! Two deliberate divergences, both confined to input a Ravel build never
//! wrote:
//!
//! * **A point at a non-finite input or output is dropped.** Rust parses
//!   `"nan"` and `"inf"`, so `parse_curve` accepted them and produced a curve
//!   that poisoned every sample downstream. A `CurveParam` cannot order such a
//!   point at all.
//! * **A repeated input was a step, and the step is lost.** Two points at one
//!   input `xd` made the old curve discontinuous there: it took the *first*
//!   duplicate's output up to and including `xd`, and the *last* one's after.
//!   A `CurveParam` maps one input to one output, so one of the two has to go
//!   — the last wins, matching [`CurveParam::insert_point`] and the
//!   deserializer, so every entry point collapses a repeat the same way. The
//!   upgraded curve therefore agrees with v5 strictly after `xd` and differs
//!   over the segment arriving at it.
//!
//! Whatever is dropped is logged. Silently changing a curve would change a
//! composition's look with no trace.

use crate::graph::{Graph, ParameterValue};
use crate::param_curve::CurveParam;
use std::sync::Arc;

/// Every `(type_key, parameter key)` whose curve was stored as text before
/// v6. New curve parameters are declared as [`ParameterValue::Curve`] from
/// the start and never appear here.
const TEXT_CURVE_PARAMS: &[(&str, &str)] = &[("field.curve_remap", "points")];

/// What reading one v5 control-point string produced.
struct Parsed {
    /// The curve, or `None` when the string held no readable point at all.
    curve: Option<CurveParam>,
    /// Non-empty entries that were not a readable `input:output` pair of
    /// finite numbers.
    skipped: usize,
    /// Points that shared an input value with a later one and lost.
    collapsed: usize,
}

/// Parse the v5 control-point string: `"0:0,0.5:0.8,1:1"`.
///
/// Unreadable entries are skipped and counted rather than failing the whole
/// string — the v5 reader collected through a `filter_map` and did the same,
/// so rejecting them would reshape a partly damaged file instead of
/// preserving what it still had. Empty entries (a trailing separator) are
/// not damage and are not counted.
fn parse_v5_points(text: &str) -> Parsed {
    let mut points: Vec<(f32, f32)> = Vec::new();
    let mut skipped = 0;
    for entry in text.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let point = entry
            .split_once(':')
            .and_then(|(input, output)| {
                Some((
                    input.trim().parse::<f32>().ok()?,
                    output.trim().parse().ok()?,
                ))
            })
            .filter(|(x, y): &(f32, f32)| x.is_finite() && y.is_finite());
        match point {
            Some(point) => points.push(point),
            None => skipped += 1,
        }
    }
    if points.is_empty() {
        return Parsed {
            curve: None,
            skipped,
            collapsed: 0,
        };
    }
    // `CurveParam::linear` sorts and lets the last point at a repeated input
    // win — the two rules the v5 reader's stable sort plus zero-width segment
    // handling amount to.
    let curve = CurveParam::linear(points.iter().copied());
    let collapsed = points.len() - curve.len();
    Parsed {
        curve: Some(curve),
        skipped,
        collapsed,
    }
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
            let parsed = parse_v5_points(text);
            let curve = match parsed.curve {
                Some(curve) => {
                    if parsed.skipped > 0 || parsed.collapsed > 0 {
                        tracing::warn!(
                            node = id.raw(),
                            key = param.key,
                            stored = text,
                            skipped = parsed.skipped,
                            collapsed = parsed.collapsed,
                            "kept the readable curve control points and dropped the rest"
                        );
                    }
                    curve
                }
                None => {
                    tracing::warn!(
                        node = id.raw(),
                        key = param.key,
                        stored = text,
                        skipped = parsed.skipped,
                        "no readable curve control points; using the identity curve"
                    );
                    CurveParam::identity()
                }
            };
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

    /// Only a string with *nothing* readable in it becomes the identity. The
    /// v5 reader dropped bad entries one at a time, so falling back for the
    /// whole string would reshape a partly damaged curve.
    #[test]
    fn a_string_with_no_readable_point_becomes_the_identity_curve() {
        for stored in ["", "garbage", "0:zero", "nan:1", "inf:0,0:inf", ":,:"] {
            let graph = Graph::new().add_node(v5_curve(1, stored)).unwrap();
            assert_eq!(
                curve_of(&upgrade_graph(&graph), 1),
                CurveParam::identity(),
                "{stored:?}"
            );
        }
    }

    /// A partly damaged string keeps the points it still has.
    #[test]
    fn unreadable_entries_are_skipped_and_the_rest_survives() {
        for (stored, expected) in [
            ("0:0,broken,1:1", vec![(0.0, 0.0), (1.0, 1.0)]),
            ("0:0,1:zero,2:4", vec![(0.0, 0.0), (2.0, 4.0)]),
            // Rust parses "nan" / "inf", so the v5 reader accepted them and
            // poisoned the curve. They are dropped like any other bad entry.
            ("0:0,nan:5,1:2", vec![(0.0, 0.0), (1.0, 2.0)]),
            ("0:0,1:inf,2:3", vec![(0.0, 0.0), (2.0, 3.0)]),
        ] {
            let graph = Graph::new().add_node(v5_curve(1, stored)).unwrap();
            let curve = curve_of(&upgrade_graph(&graph), 1);
            let points: Vec<(f32, f32)> = curve.points().iter().map(|p| (p.x, p.y)).collect();
            assert_eq!(points, expected, "{stored:?}");
        }
    }

    /// The one place the upgrade knowingly changes a curve: a repeated input
    /// was a step, and a `CurveParam` cannot hold two outputs at one input.
    /// The last point wins, so the segment *arriving* at the repeat moves to
    /// it while everything after is unchanged.
    #[test]
    fn a_repeated_input_loses_its_step_to_the_last_point() {
        let stored = "0:0,0.5:1,0.5:9,1:1";
        let graph = Graph::new().add_node(v5_curve(1, stored)).unwrap();
        let curve = curve_of(&upgrade_graph(&graph), 1);
        // v5 stepped from 1 up to 9 at x = 0.5; the upgraded curve rises to 9.
        assert!((v5_remap(stored, 0.5) - 1.0).abs() < 1e-6);
        assert!((curve.evaluate(0.5) - 9.0).abs() < 1e-6);
        assert!((v5_remap(stored, 0.25) - 0.5).abs() < 1e-6);
        assert!((curve.evaluate(0.25) - 4.5).abs() < 1e-6);
        // After the repeat both descend 9 → 1 over the same segment.
        for step in 11..=20 {
            let x = step as f32 / 20.0;
            assert!(
                (v5_remap(stored, x) - curve.evaluate(x)).abs() < 1e-6,
                "at {x}"
            );
        }
    }

    /// Two points at one input collapse to the later one — the same rule
    /// [`CurveParam::insert_point`] and the deserializer apply.
    #[test]
    fn repeated_inputs_keep_the_last_point() {
        for (stored, expected) in [
            (
                "0:0,0.5:1,0.5:9,1:1",
                vec![(0.0, 0.0), (0.5, 9.0), (1.0, 1.0)],
            ),
            // Order in the string does not matter: the reader sorted.
            (
                "0.5:9,1:1,0.5:1,0:0",
                vec![(0.0, 0.0), (0.5, 1.0), (1.0, 1.0)],
            ),
            ("0:5,0:9,1:1", vec![(0.0, 9.0), (1.0, 1.0)]),
            ("0:0,1:2,1:7", vec![(0.0, 0.0), (1.0, 7.0)]),
        ] {
            let graph = Graph::new().add_node(v5_curve(1, stored)).unwrap();
            let curve = curve_of(&upgrade_graph(&graph), 1);
            let points: Vec<(f32, f32)> = curve.points().iter().map(|p| (p.x, p.y)).collect();
            assert_eq!(points, expected, "{stored:?}");
        }
    }

    /// The v5 pipeline, reproduced verbatim from the implementation this
    /// upgrade replaced: `parse_curve` (`ravel-nodes`), the sort
    /// `CurveRemapField::new` applied to its output, and `remap_curve`
    /// (`geometry::field`). The upgraded curve must agree with it, which is
    /// what "an old project evaluates the same" means.
    fn v5_remap(text: &str, value: f32) -> f32 {
        let mut points = v5_parse(text);
        if points.is_empty() {
            points = vec![(0.0, 0.0), (1.0, 1.0)];
        }
        // `CurveRemapField::new` sorted before evaluating, and `sort_by` is
        // stable, so points at one input kept their stored order.
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

    /// `parse_curve` verbatim: a `filter_map` that drops what it cannot read.
    fn v5_parse(text: &str) -> Vec<(f32, f32)> {
        text.split(',')
            .filter_map(|point| {
                let (input, output) = point.split_once(':')?;
                Some((input.trim().parse().ok()?, output.trim().parse().ok()?))
            })
            .collect()
    }

    /// Inputs the string stores more than one point at. The v5 curve stepped
    /// there — first duplicate up to and including that input, last one after
    /// — so the upgraded curve is only claimed to agree strictly after it (see
    /// the module docs, and `repeated_inputs_keep_the_last_point` for what the
    /// collapse does instead).
    fn v5_duplicated_inputs(text: &str) -> Vec<f32> {
        let points = v5_parse(text);
        points
            .iter()
            .enumerate()
            .filter(|(index, (x, _))| points[..*index].iter().any(|(other, _)| other == x))
            .map(|(_, (x, _))| *x)
            .collect()
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
            // Partly damaged: the readable points still describe the curve.
            "0:0,broken,1:1",
            "0:0,1:zero,2:4",
            "0:0,,1:2,",
            // Repeated inputs: identical away from the repeated value itself.
            "0:0,0.5:1,0.5:9,1:1",
            "0.5:9,1:1,0.5:1,0:0",
            "0:5,0:9,1:1",
            "0:0,1:2,1:7",
        ] {
            let graph = Graph::new().add_node(v5_curve(1, stored)).unwrap();
            let curve = curve_of(&upgrade_graph(&graph), 1);
            let duplicated = v5_duplicated_inputs(stored);
            for step in -20..=60 {
                let x = step as f32 / 20.0;
                if duplicated.iter().any(|repeated| x <= *repeated) {
                    continue;
                }
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
