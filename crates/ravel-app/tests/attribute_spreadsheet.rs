// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What the attribute spreadsheet shows for a real node's output
//! (`attribute-spreadsheet-plan.md` unit 4).
//!
//! The panel's own unit tests build geometries by hand. This one runs the
//! actual `scatter.grid` processor, because the claim being checked is about
//! that node: selecting it must put `index` / `P` / `rot` / `scale` on the
//! instance tab. A hand-built geometry cannot notice the day a scatter stops
//! writing one of them.

use ravel_core::eval::{EvalContext, Evaluator, NodeProcessor, ResolvedParams, ResolvedValue};
use ravel_core::geometry::{Domain, Geometry};
use ravel_core::graph::Node;
use ravel_core::id::{DataTypeId, NodeId};
use ravel_core::types::FrameRate;
use ravel_nodes::scatter::GridProcessor;
use ravel_ui::panels::attribute_spreadsheet::{cell_text, columns, element_count};

/// One `scatter.grid` output with `count_x * count_y` instances.
fn scatter_grid(count_x: i32, count_y: i32) -> Geometry {
    let node = Node::new(NodeId::new(1), "scatter.grid")
        .with_input("instance_source", &[DataTypeId::GEOMETRY])
        .with_output("output", DataTypeId::GEOMETRY);
    let mut params = ResolvedParams::default();
    params.set("count_x", ResolvedValue::Int(count_x));
    params.set("count_y", ResolvedValue::Int(count_y));
    let mut scope = Evaluator::new();
    let value = GridProcessor
        .process(
            &node,
            &EvalContext::new(0, FrameRate::new(30, 1), (64, 64)),
            &[None],
            &params,
            &mut scope,
        )
        .expect("scatter.grid evaluates without a source");
    value
        .downcast_ref::<Geometry>()
        .expect("scatter.grid produces a geometry")
        .clone()
}

#[test]
fn a_scatter_grid_lists_index_p_rot_and_scale_on_the_instance_tab() {
    let geometry = scatter_grid(4, 3);
    let instance_columns = columns(&geometry, Domain::Instance);
    let names: Vec<&str> = instance_columns
        .iter()
        .map(|column| column.name.as_str())
        .collect();
    assert_eq!(names, ["#", "index", "P", "rot", "scale"]);
    assert_eq!(element_count(&geometry, Domain::Instance), 12);
}

/// Ten thousand instances are a row count, not a limit: nothing in the sheet
/// caps or strides the rows, and every cell is reachable.
///
/// The scroll cost itself is `uniform_list`'s (only the visible rows are ever
/// rendered), so what this pins is the part that would break it — a row count
/// that lies, or a cell lookup whose cost grows with the row index.
#[test]
fn ten_thousand_instances_are_all_addressable() {
    let geometry = scatter_grid(100, 100);
    assert_eq!(element_count(&geometry, Domain::Instance), 10_000);

    let columns = columns(&geometry, Domain::Instance);
    for row in [0, 1, 5_000, 9_999] {
        for column in &columns {
            assert!(
                !cell_text(&geometry, Domain::Instance, row, column).is_empty(),
                "row {row} column {} reads empty",
                column.name
            );
        }
    }
    // Past the end reads empty rather than panicking, which is what a stale
    // row index arriving from a scroll in flight would do.
    assert_eq!(
        cell_text(&geometry, Domain::Instance, 10_000, &columns[1]),
        ""
    );
}
