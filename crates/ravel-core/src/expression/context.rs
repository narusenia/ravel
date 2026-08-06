// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Values for the declared vocabularies, in slot order.
//!
//! [`Program::evaluate`](super::Program::evaluate) reads its variables from a
//! slice indexed by [`VarSlot`](super::VarSlot), and a [`Scope`](super::Scope)
//! hands out those slots in declaration order. **The order is the contract**:
//! a caller that fills the slice in a different order does not get an error,
//! it gets an expression that silently reads a different variable.
//!
//! So the slice is not built by writing values into hand-counted positions. It
//! is built by walking the same `&[&str]` the scope was constructed from
//! ([`PARAMETER_VARIABLES`], [`FIELD_VARIABLES`]) and asking for each name's
//! value. Adding a variable then means adding it to the name list and to
//! [`value_of`]; getting the order wrong is not expressible, and forgetting
//! the value is a test failure rather than a silent zero.

use crate::eval::EvalContext;

use super::scope::{FIELD_VARIABLES, PARAMETER_VARIABLES};

/// Length of the slice [`parameter_values`] fills.
pub const PARAMETER_VALUE_COUNT: usize = PARAMETER_VARIABLES.len();

/// Length of the slice [`field_values`] fills.
pub const FIELD_VALUE_COUNT: usize = PARAMETER_VALUE_COUNT + FIELD_VARIABLES.len();

/// What a field expression knows beyond the evaluation context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldContext {
    /// Number of elements in the batch being sampled, for `elem.count`.
    pub element_count: usize,
}

/// Values for [`Scope::parameter_context`](super::Scope::parameter_context).
///
/// `frame` is the continuous frame position the channel is being sampled at —
/// layer-local, and fractional under motion blur or time remapping — not
/// `ctx.frame`. `time` is derived from it (`frame / fps`) rather than read from
/// [`EvalContext::time`], so that the two variables always describe the same
/// instant even when the caller samples a layer at an offset position.
pub fn parameter_values(frame: f64, ctx: &EvalContext) -> [f64; PARAMETER_VALUE_COUNT] {
    let mut values = [0.0; PARAMETER_VALUE_COUNT];
    fill(&mut values, PARAMETER_VARIABLES, frame, ctx, None);
    values
}

/// Values for [`Scope::field_context`](super::Scope::field_context).
///
/// The parameter vocabulary is a prefix of the field one, so the shared slots
/// keep their indices and only `elem.count` is appended. Built once per batch,
/// not once per element: nothing here varies across the elements of one
/// sample.
pub fn field_values(
    frame: f64,
    ctx: &EvalContext,
    field: FieldContext,
) -> [f64; FIELD_VALUE_COUNT] {
    let mut values = [0.0; FIELD_VALUE_COUNT];
    let (shared, extra) = values.split_at_mut(PARAMETER_VALUE_COUNT);
    fill(shared, PARAMETER_VARIABLES, frame, ctx, Some(field));
    fill(extra, FIELD_VARIABLES, frame, ctx, Some(field));
    values
}

fn fill(
    values: &mut [f64],
    names: &[&str],
    frame: f64,
    ctx: &EvalContext,
    field: Option<FieldContext>,
) {
    debug_assert_eq!(values.len(), names.len(), "one value per declared name");
    for (slot, name) in names.iter().enumerate() {
        // `unwrap_or(0.0)` is unreachable for a declared name, and
        // `every_declared_name_has_a_value` is what keeps it that way.
        values[slot] = value_of(name, frame, ctx, field).unwrap_or(0.0);
    }
}

/// The value of one declared name, or `None` if this module does not know it.
fn value_of(name: &str, frame: f64, ctx: &EvalContext, field: Option<FieldContext>) -> Option<f64> {
    let (width, height) = ctx.resolution;
    let (comp_width, comp_height) = ctx.comp_resolution;
    Some(match name {
        "frame" => frame,
        "time" => frame / ctx.fps.as_f64(),
        "fps" => ctx.fps.as_f64(),
        "res.width" => f64::from(width),
        "res.height" => f64::from(height),
        // A zero-sized target yields `inf` (or `NaN` for 0/0) rather than a
        // substituted number: the language propagates IEEE results and the
        // channel boundary is the one place that turns a non-finite value into
        // a default.
        "res.aspect" => f64::from(width) / f64::from(height),
        "comp.width" => f64::from(comp_width),
        "comp.height" => f64::from(comp_height),
        "comp.aspect" => f64::from(comp_width) / f64::from(comp_height),
        "elem.count" => field?.element_count as f64,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::Scope;
    use crate::types::FrameRate;

    const FPS: FrameRate = FrameRate { num: 30, den: 1 };

    fn ctx() -> EvalContext {
        EvalContext::new(0, FPS, (1920, 1080)).with_comp_resolution((1280, 720))
    }

    /// The guard against a silent zero: every name either scope declares must
    /// be answered here.
    #[test]
    fn every_declared_name_has_a_value() {
        let field = FieldContext { element_count: 4 };
        for name in PARAMETER_VARIABLES.iter().chain(FIELD_VARIABLES) {
            assert!(
                value_of(name, 0.0, &ctx(), Some(field)).is_some(),
                "no value bound for the declared name `{name}`"
            );
        }
    }

    /// The guard against a shifted slot: read each value back through the slot
    /// the scope itself assigns, not through a hand-written index.
    #[test]
    fn parameter_values_land_in_the_slots_the_scope_assigns() {
        let scope = Scope::parameter_context();
        let values = parameter_values(7.5, &ctx());
        let at = |name: &str| values[scope.slot(name).expect("declared").index()];

        assert_eq!(at("frame"), 7.5);
        assert_eq!(at("time"), 7.5 / 30.0);
        assert_eq!(at("fps"), 30.0);
        assert_eq!(at("res.width"), 1920.0);
        assert_eq!(at("res.height"), 1080.0);
        assert_eq!(at("res.aspect"), 1920.0 / 1080.0);
        assert_eq!(at("comp.width"), 1280.0);
        assert_eq!(at("comp.height"), 720.0);
        assert_eq!(at("comp.aspect"), 1280.0 / 720.0);
    }

    #[test]
    fn field_values_extend_the_parameter_ones_without_moving_them() {
        let scope = Scope::field_context();
        let parameters = parameter_values(3.0, &ctx());
        let values = field_values(3.0, &ctx(), FieldContext { element_count: 128 });

        assert_eq!(&values[..PARAMETER_VALUE_COUNT], &parameters[..]);
        assert_eq!(
            values[scope.slot("elem.count").expect("declared").index()],
            128.0
        );
        for (index, name) in PARAMETER_VARIABLES.iter().enumerate() {
            assert_eq!(scope.slot(name).map(|slot| slot.index()), Some(index));
        }
    }

    #[test]
    fn a_zero_sized_resolution_propagates_ieee_rather_than_substituting() {
        let ctx = EvalContext::new(0, FPS, (1920, 0));
        let scope = Scope::parameter_context();
        let values = parameter_values(0.0, &ctx);
        assert!(values[scope.slot("res.aspect").expect("declared").index()].is_infinite());
    }
}
