// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Headless model for the attribute spreadsheet panel (REQ-CORE-010 inspection
//! UI, `attribute-spreadsheet-plan.md` unit 3).
//!
//! Everything the panel decides from a [`Geometry`] alone lives here: which
//! columns a domain has, in which order, what a cell reads as, and why a sheet
//! has nothing to show. The GPUI side (`ravel-app`) is the `TableDelegate`
//! adapter around it — the rendering, the domain tab bar and the evaluated
//! value it reads.

use crate::panel::PanelKind;
use ravel_core::geometry::{AttrName, AttributeArray, AttributeType, Domain, Geometry, names};

/// Every domain, in tab order.
pub const DOMAINS: [Domain; 4] = [
    Domain::Point,
    Domain::Primitive,
    Domain::Instance,
    Domain::Detail,
];

/// The reserved attributes that lead the column order, in the order they are
/// listed here.
///
/// `AttributeSet` is a `HashMap`, so a stable order has to be imposed. Plain
/// name sorting alone would bury `P` between `alpha` and `pscale`; the standard
/// names come first in a reading order, and everything else follows sorted by
/// name (`AttributeSet::describe` already sorts).
const STANDARD_ORDER: [&str; 11] = [
    names::INDEX,
    names::ID,
    names::P,
    names::N,
    names::CD,
    names::ALPHA,
    names::PSCALE,
    names::ROT,
    names::ORIENT,
    names::SCALE,
    names::SCALE3,
];

/// One column of the sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetColumn {
    /// Header text and, for attribute columns, the attribute name.
    pub name: AttrName,
    /// `None` for the synthetic element-number column, which reads no
    /// attribute.
    pub ty: Option<AttributeType>,
}

impl SheetColumn {
    /// Whether this is the synthetic element-number column.
    pub fn is_row_number(&self) -> bool {
        self.ty.is_none()
    }
}

/// Why the sheet is showing a message instead of rows.
///
/// Four rather than the three the plan first named: a target that has been
/// declared but not yet evaluated is not the same as a node that will never
/// produce geometry, and saying "this node outputs no geometry" about a node
/// that does — for the frame between the selection and the evaluation — is a
/// wrong sentence on screen, not a slow one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetEmpty {
    /// Nothing names a node to inspect: no selection, a target that is not
    /// nodes (a layer, a composition, a media asset), or a selection made in a
    /// composition that is no longer the active one.
    NoSelection,
    /// A node is selected, but it declares no geometry output (`rasterize`,
    /// say), so no evaluation is even requested for it.
    NoGeometryOutput,
    /// The target was declared and its result has not arrived (or is not a
    /// geometry).
    Pending,
    /// The geometry is there; the chosen domain holds no elements.
    NoElements,
}

impl SheetEmpty {
    /// The locale key of the line the panel prints.
    pub fn message_key(self) -> &'static str {
        match self {
            Self::NoSelection => "attribute_spreadsheet.empty.no_selection",
            Self::NoGeometryOutput => "attribute_spreadsheet.empty.no_geometry_output",
            Self::Pending => "attribute_spreadsheet.empty.pending",
            Self::NoElements => "attribute_spreadsheet.empty.no_elements",
        }
    }
}

/// Headless view state: the domain tab the sheet is showing.
#[derive(Clone, Debug)]
pub struct AttributeSpreadsheetPanel {
    domain: Domain,
}

impl Default for AttributeSpreadsheetPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl AttributeSpreadsheetPanel {
    pub const KIND: PanelKind = PanelKind::AttributeSpreadsheet;

    pub fn new() -> Self {
        Self {
            domain: Domain::Point,
        }
    }

    pub fn domain(&self) -> Domain {
        self.domain
    }

    /// Switches the domain tab. Returns whether it moved, so the caller can
    /// skip a repaint that would draw the identical sheet.
    pub fn set_domain(&mut self, domain: Domain) -> bool {
        let changed = self.domain != domain;
        self.domain = domain;
        changed
    }
}

/// The locale key of a domain tab's label.
pub fn domain_label_key(domain: Domain) -> &'static str {
    match domain {
        Domain::Point => "attribute_spreadsheet.domain.point",
        Domain::Primitive => "attribute_spreadsheet.domain.primitive",
        Domain::Instance => "attribute_spreadsheet.domain.instance",
        Domain::Detail => "attribute_spreadsheet.domain.detail",
    }
}

/// How many elements `domain` holds — the row count of the sheet, and the
/// count each domain tab prints.
///
/// The primitive domain counts primitives rather than attribute rows: a
/// geometry may carry paths with no primitive attributes at all, and a sheet
/// claiming zero primitives for a shape that has ten would be reporting on the
/// attribute set rather than on the geometry.
pub fn element_count(geometry: &Geometry, domain: Domain) -> usize {
    match domain {
        Domain::Point => geometry.point_count(),
        Domain::Primitive => geometry.primitive_count(),
        Domain::Instance => geometry.instance_count(),
        Domain::Detail => geometry.detail().element_count(),
    }
}

/// The columns of `domain`: the element number, then the standard attributes
/// present, then the rest by name.
pub fn columns(geometry: &Geometry, domain: Domain) -> Vec<SheetColumn> {
    let listing = geometry.attribute_set(domain).describe();
    let mut columns = Vec::with_capacity(listing.len() + 1);
    columns.push(SheetColumn {
        name: AttrName::from("#"),
        ty: None,
    });
    for standard in STANDARD_ORDER {
        if let Some((name, ty)) = listing.iter().find(|(name, _)| name == standard) {
            columns.push(SheetColumn {
                name: name.clone(),
                ty: Some(*ty),
            });
        }
    }
    for (name, ty) in &listing {
        if STANDARD_ORDER.contains(&name.as_str()) {
            continue;
        }
        columns.push(SheetColumn {
            name: name.clone(),
            ty: Some(*ty),
        });
    }
    columns
}

/// The text of one cell.
///
/// An attribute column shorter than the row count — which a malformed geometry
/// can produce, since uniform length is validated on insert rather than on
/// every mutation — reads as empty rather than panicking on the index.
pub fn cell_text(geometry: &Geometry, domain: Domain, row: usize, column: &SheetColumn) -> String {
    if column.is_row_number() {
        return row.to_string();
    }
    let Some(values) = geometry.attribute_set(domain).get(column.name.as_str()) else {
        return String::new();
    };
    format_element(values, row)
}

/// One element of a column, formatted for display.
fn format_element(values: &AttributeArray, index: usize) -> String {
    match values {
        AttributeArray::F32(v) => v.get(index).map(|x| format_f32(*x)).unwrap_or_default(),
        AttributeArray::Vec2(v) => v
            .get(index)
            .map(|x| format!("({}, {})", format_f32(x.0), format_f32(x.1)))
            .unwrap_or_default(),
        AttributeArray::Vec3(v) => v
            .get(index)
            .map(|x| {
                format!(
                    "({}, {}, {})",
                    format_f32(x.0),
                    format_f32(x.1),
                    format_f32(x.2)
                )
            })
            .unwrap_or_default(),
        AttributeArray::Vec4(v) => v
            .get(index)
            .map(|x| {
                format!(
                    "({}, {}, {}, {})",
                    format_f32(x.0),
                    format_f32(x.1),
                    format_f32(x.2),
                    format_f32(x.3)
                )
            })
            .unwrap_or_default(),
        AttributeArray::Color(v) => v
            .get(index)
            .map(|x| {
                format!(
                    "({}, {}, {}, {})",
                    format_f32(x.r),
                    format_f32(x.g),
                    format_f32(x.b),
                    format_f32(x.a)
                )
            })
            .unwrap_or_default(),
        AttributeArray::I32(v) => v.get(index).map(i32::to_string).unwrap_or_default(),
        AttributeArray::Bool(v) => v.get(index).map(bool::to_string).unwrap_or_default(),
        AttributeArray::Str(v) => v.get(index).cloned().unwrap_or_default(),
    }
}

/// A float at four significant digits.
///
/// Not four decimals: an attribute column mixes magnitudes freely (a `pscale`
/// of `8`, a `P` in the thousands, a normalised `u` in the thousandths) and a
/// fixed decimal count prints either noise or nothing for one of them.
///
/// Non-finite values print as `NaN` / `inf` / `-inf` rather than being hidden.
/// A NaN in a column is exactly what an inspection panel exists to show — it is
/// how a division by zero upstream becomes visible — so it is never formatted
/// away into `0.000`.
///
/// For the same reason a magnitude outside what fixed point can hold at four
/// significant digits switches to scientific notation instead of being clamped
/// into it. Clamping prints `1.0e-8` as `0.000000`: a non-zero value reading as
/// zero is the same lie as a hidden NaN, and at the other end it prints digits
/// an `f32` never carried.
pub fn format_f32(value: f32) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    if value == 0.0 {
        return "0.000".to_string();
    }
    let exponent = value.abs().log10().floor() as i32;
    if !FIXED_POINT_EXPONENTS.contains(&exponent) {
        return format!("{value:.3e}");
    }
    let decimals = (3 - exponent) as usize;
    format!("{value:.decimals$}")
}

/// The decimal exponents `format_f32` renders in fixed point: those where
/// `3 - exponent` decimals land in `0..=6` and so print exactly four
/// significant digits.
const FIXED_POINT_EXPONENTS: std::ops::RangeInclusive<i32> = -3..=3;

/// Why the sheet cannot show rows, or `None` when it can.
///
/// Takes what the caller already resolved rather than the globals it resolved
/// them from, which is what keeps the ordering of the four cases testable
/// without an application.
pub fn empty_state(
    has_node_target: bool,
    has_geometry_output: bool,
    geometry: Option<&Geometry>,
    domain: Domain,
) -> Option<SheetEmpty> {
    if !has_node_target {
        return Some(SheetEmpty::NoSelection);
    }
    if !has_geometry_output {
        return Some(SheetEmpty::NoGeometryOutput);
    }
    let Some(geometry) = geometry else {
        return Some(SheetEmpty::Pending);
    };
    (element_count(geometry, domain) == 0).then_some(SheetEmpty::NoElements)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::geometry::Primitive;
    use ravel_core::types::{Color, Vec2, Vec3, Vec4};

    fn grid() -> Geometry {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(10.0, 0.0)]);
        geometry
            .points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![8.0, 8.0]))
            .unwrap();
        geometry
            .points_mut()
            .insert(
                names::CD,
                AttributeArray::Color(vec![
                    Color::new(1.0, 0.0, 0.0, 1.0),
                    Color::new(1.0, 0.5, 0.0, 1.0),
                ]),
            )
            .unwrap();
        geometry
            .instances_mut()
            .insert(names::P, AttributeArray::Vec2(vec![Vec2(1.0, 2.0)]))
            .unwrap();
        geometry
            .instances_mut()
            .insert(names::ROT, AttributeArray::F32(vec![0.5]))
            .unwrap();
        geometry
    }

    fn names_of(columns: &[SheetColumn]) -> Vec<&str> {
        columns.iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn columns_lead_with_the_element_number_then_standard_attributes() {
        let columns = columns(&grid(), Domain::Point);
        assert_eq!(names_of(&columns), ["#", "index", "P", "Cd", "pscale"]);
        assert!(columns[0].is_row_number());
        assert_eq!(columns[2].ty, Some(AttributeType::Vec2));
    }

    #[test]
    fn non_standard_attributes_follow_sorted_by_name() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0)]);
        for name in ["zeta", "alpha_custom", "mid"] {
            geometry
                .points_mut()
                .insert(name, AttributeArray::F32(vec![1.0]))
                .unwrap();
        }
        assert_eq!(
            names_of(&columns(&geometry, Domain::Point)),
            ["#", "index", "P", "alpha_custom", "mid", "zeta"]
        );
    }

    #[test]
    fn switching_domains_swaps_the_rows_and_the_columns() {
        let geometry = grid();
        let mut panel = AttributeSpreadsheetPanel::new();
        assert_eq!(panel.domain(), Domain::Point);
        assert_eq!(element_count(&geometry, panel.domain()), 2);
        assert_eq!(
            names_of(&columns(&geometry, panel.domain())),
            ["#", "index", "P", "Cd", "pscale"]
        );

        assert!(panel.set_domain(Domain::Instance));
        assert_eq!(element_count(&geometry, panel.domain()), 1);
        assert_eq!(
            names_of(&columns(&geometry, panel.domain())),
            ["#", "P", "rot"]
        );

        assert!(!panel.set_domain(Domain::Instance));
    }

    #[test]
    fn the_primitive_domain_counts_primitives_not_attribute_rows() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1.0, 0.0)]);
        geometry.push_primitive(Primitive::Path {
            verts: 0..2,
            closed: false,
        });
        assert_eq!(geometry.primitive_attrs().element_count(), 0);
        assert_eq!(element_count(&geometry, Domain::Primitive), 1);
    }

    #[test]
    fn cells_render_per_type() {
        let mut geometry = Geometry::from_points(vec![Vec2(0.0, 0.0), Vec2(1234.5678, -0.25)]);
        geometry
            .points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![8.0, 0.0125]))
            .unwrap();
        geometry
            .points_mut()
            .insert(names::FILL, AttributeArray::Bool(vec![true, false]))
            .unwrap();
        geometry
            .points_mut()
            .insert(
                "label",
                AttributeArray::Str(vec!["a".to_string(), "b".to_string()]),
            )
            .unwrap();
        geometry
            .points_mut()
            .insert(
                "velocity",
                AttributeArray::Vec3(vec![Vec3(1.0, 2.0, 3.0), Vec3(0.0, 0.0, 0.0)]),
            )
            .unwrap();
        geometry
            .points_mut()
            .insert(
                "bounds",
                AttributeArray::Vec4(vec![Vec4(1.0, 2.0, 3.0, 4.0), Vec4(0.0, 0.0, 0.0, 0.0)]),
            )
            .unwrap();
        geometry
            .points_mut()
            .insert(names::ID, AttributeArray::I32(vec![-12, 7]))
            .unwrap();
        let columns = columns(&geometry, Domain::Point);
        let text = |row: usize, name: &str| {
            let column = columns.iter().find(|c| c.name == name).unwrap();
            cell_text(&geometry, Domain::Point, row, column)
        };

        assert_eq!(text(0, "#"), "0");
        assert_eq!(text(1, "#"), "1");
        assert_eq!(text(0, "index"), "0");
        assert_eq!(text(0, "P"), "(0.000, 0.000)");
        assert_eq!(text(1, "P"), "(1235, -0.2500)");
        assert_eq!(text(0, "pscale"), "8.000");
        assert_eq!(text(1, "pscale"), "0.01250");
        // Component order and count are part of the reading, not decoration:
        // a swapped `Vec3` is a wrong value on screen with no way to notice.
        assert_eq!(text(0, "velocity"), "(1.000, 2.000, 3.000)");
        assert_eq!(text(0, "bounds"), "(1.000, 2.000, 3.000, 4.000)");
        assert_eq!(text(0, "id"), "-12");
        assert_eq!(text(1, "id"), "7");
        assert_eq!(text(0, "fill"), "true");
        assert_eq!(text(1, "fill"), "false");
        assert_eq!(text(0, "label"), "a");
    }

    /// A non-zero value must never read as zero. Padding `1.0e-8` out to
    /// `0.000000` is the same lie as formatting a NaN away: the panel exists
    /// to show what the value is, and "0" is not what that value is.
    #[test]
    fn values_too_small_or_too_large_for_fixed_point_switch_to_scientific() {
        assert_eq!(format_f32(0.0), "0.000");
        assert_eq!(format_f32(1.0e-8), "1.000e-8");
        assert_eq!(format_f32(-1.0e-8), "-1.000e-8");
        assert_ne!(format_f32(1.0e-8), format_f32(0.0));
        assert_eq!(format_f32(1.5e10), "1.500e10");
        assert_eq!(format_f32(-2.5e7), "-2.500e7");
        // The band that fixed point can hold at four significant digits is
        // unchanged.
        assert_eq!(format_f32(1234.5678), "1235");
        assert_eq!(format_f32(0.001234), "0.001234");
    }

    #[test]
    fn colors_render_their_four_components() {
        let geometry = grid();
        let columns = columns(&geometry, Domain::Point);
        let column = columns.iter().find(|c| c.name == names::CD).unwrap();
        assert_eq!(
            cell_text(&geometry, Domain::Point, 1, column),
            "(1.000, 0.5000, 0.000, 1.000)"
        );
    }

    /// A NaN reaching a cell is the thing the panel exists to make visible.
    #[test]
    fn non_finite_values_are_printed_not_hidden() {
        assert_eq!(format_f32(f32::NAN), "NaN");
        assert_eq!(format_f32(f32::INFINITY), "inf");
        assert_eq!(format_f32(f32::NEG_INFINITY), "-inf");

        let mut geometry = Geometry::from_points(vec![Vec2(f32::NAN, f32::INFINITY)]);
        geometry
            .points_mut()
            .insert(names::PSCALE, AttributeArray::F32(vec![f32::NAN]))
            .unwrap();
        let columns = columns(&geometry, Domain::Point);
        let position = columns.iter().find(|c| c.name == names::P).unwrap();
        assert_eq!(
            cell_text(&geometry, Domain::Point, 0, position),
            "(NaN, inf)"
        );
        let pscale = columns.iter().find(|c| c.name == names::PSCALE).unwrap();
        assert_eq!(cell_text(&geometry, Domain::Point, 0, pscale), "NaN");
    }

    #[test]
    fn a_row_past_the_end_of_a_column_reads_empty() {
        let geometry = grid();
        let columns = columns(&geometry, Domain::Point);
        let column = columns.iter().find(|c| c.name == names::P).unwrap();
        assert_eq!(cell_text(&geometry, Domain::Point, 99, column), "");
    }

    /// A column the domain does not carry — a caller holding a column list
    /// built for another domain — reads empty rather than inventing a value.
    #[test]
    fn a_column_the_domain_does_not_carry_reads_empty() {
        let geometry = grid();
        let instance_columns = columns(&geometry, Domain::Instance);
        let rot = instance_columns
            .iter()
            .find(|c| c.name == names::ROT)
            .unwrap();
        assert_eq!(cell_text(&geometry, Domain::Point, 0, rot), "");
    }

    #[test]
    fn the_three_empty_states_are_told_apart() {
        let geometry = grid();
        assert_eq!(
            empty_state(false, false, None, Domain::Point),
            Some(SheetEmpty::NoSelection)
        );
        assert_eq!(
            empty_state(true, false, None, Domain::Point),
            Some(SheetEmpty::NoGeometryOutput)
        );
        assert_eq!(
            empty_state(true, true, Some(&Geometry::new()), Domain::Point),
            Some(SheetEmpty::NoElements)
        );
        assert_eq!(
            empty_state(true, true, Some(&geometry), Domain::Point),
            None
        );
    }

    /// A declared target whose result has not arrived must not be reported as
    /// a node that outputs no geometry.
    #[test]
    fn a_pending_evaluation_is_its_own_state() {
        assert_eq!(
            empty_state(true, true, None, Domain::Point),
            Some(SheetEmpty::Pending)
        );
    }

    #[test]
    fn an_empty_domain_of_a_non_empty_geometry_is_empty() {
        let geometry = grid();
        assert_eq!(element_count(&geometry, Domain::Detail), 0);
        assert_eq!(
            empty_state(true, true, Some(&geometry), Domain::Detail),
            Some(SheetEmpty::NoElements)
        );
    }

    #[test]
    fn every_empty_state_has_its_own_message() {
        let keys = [
            SheetEmpty::NoSelection,
            SheetEmpty::NoGeometryOutput,
            SheetEmpty::Pending,
            SheetEmpty::NoElements,
        ]
        .map(SheetEmpty::message_key);
        let unique: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len());
    }
}
