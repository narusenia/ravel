// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The transform repeater: `count` copies of a source, copy `i` placed by a
//! per-copy transform composed `i` times.
//!
//! The result has the same shape `scatter.*` produces — an instance domain
//! with `index` / `P` / `rot` / `scale` and the source in `instance_source` —
//! so nothing downstream can tell the two apart. A `shear` column is added
//! only when composing a non-uniform scale with a turn makes one appear.

use std::sync::Arc;

use super::{AttributeArray, Geometry, GeometryError, InstanceTransform, names};
use crate::types::Vec2;

/// Most copies one repeat makes. A parameter is user-typed, and an
/// unbounded `count` is an allocation the user did not mean to ask for.
pub const MAX_REPEAT_COUNT: usize = 100_000;

/// `count` placements: copy 0 is the identity and copy `i` is `step` applied
/// `i` times. [`InstanceTransform::compose`] is exact, so a step whose scale
/// reaches zero stays finite (a collapsed copy has scale 0) rather than
/// turning NaN.
pub fn repeat_placements(step: InstanceTransform, count: usize) -> Vec<InstanceTransform> {
    let mut placements = Vec::with_capacity(count);
    let mut current = InstanceTransform::IDENTITY;
    for _ in 0..count {
        placements.push(current);
        current = InstanceTransform::compose(step, current);
    }
    placements
}

/// Builds the instance geometry for `count` copies of `source`
/// (see [`repeat_placements`]). `count` is clamped to [`MAX_REPEAT_COUNT`].
pub fn repeat(
    source: Option<Arc<Geometry>>,
    count: usize,
    step: InstanceTransform,
) -> Result<Geometry, GeometryError> {
    let placements = repeat_placements(step, count.min(MAX_REPEAT_COUNT));
    let mut out = Geometry::new();
    let instances = out.instances_mut();
    instances.insert(
        names::INDEX,
        AttributeArray::I32((0..placements.len() as i32).collect()),
    )?;
    instances.insert(
        names::P,
        AttributeArray::Vec2(placements.iter().map(|p| p.offset).collect()),
    )?;
    instances.insert(
        names::ROT,
        AttributeArray::F32(placements.iter().map(|p| p.rot).collect()),
    )?;
    instances.insert(
        names::SCALE,
        AttributeArray::Vec2(placements.iter().map(|p| p.scale).collect()),
    )?;
    if placements.iter().any(|p| p.shear != 0.0) {
        instances.insert(
            names::SHEAR,
            AttributeArray::F32(placements.iter().map(|p| p.shear).collect()),
        )?;
    }
    out.set_instance_source(source);
    Ok(out)
}

/// The per-copy step from the node's own units (degrees).
pub fn repeat_step(translate: Vec2, rotate_degrees: f32, scale: Vec2) -> InstanceTransform {
    InstanceTransform {
        offset: translate,
        rot: rotate_degrees.to_radians(),
        scale,
        shear: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placements(step: InstanceTransform, count: usize) -> Geometry {
        repeat(None, count, step).unwrap()
    }

    fn column<'a>(g: &'a Geometry, name: &str) -> &'a AttributeArray {
        g.instances().get(name).unwrap().as_ref()
    }

    fn p(g: &Geometry) -> Vec<Vec2> {
        column(g, names::P).as_vec2(names::P).unwrap().to_vec()
    }

    #[test]
    fn a_single_copy_sits_where_the_source_is() {
        let source = Arc::new(Geometry::from_points(vec![Vec2(3.0, 4.0)]));
        let g = repeat(
            Some(source.clone()),
            1,
            repeat_step(Vec2(9.0, 9.0), 45.0, Vec2(2.0, 2.0)),
        )
        .unwrap();
        assert_eq!(g.instance_count(), 1);
        assert_eq!(p(&g), [Vec2(0.0, 0.0)]);
        assert_eq!(column(&g, names::ROT).as_f32(names::ROT).unwrap(), [0.0]);
        assert_eq!(
            column(&g, names::SCALE).as_vec2(names::SCALE).unwrap(),
            [Vec2(1.0, 1.0)]
        );
        assert!(Arc::ptr_eq(g.instance_source().unwrap(), &source));
    }

    #[test]
    fn copy_i_is_the_step_applied_i_times() {
        let step = repeat_step(Vec2(5.0, 1.0), 20.0, Vec2(0.9, 0.9));
        let g = placements(step, 6);
        let expected = |i: usize| {
            let mut point = Vec2(0.0, 0.0);
            for _ in 0..i {
                point = step.apply(point);
            }
            point
        };
        for (i, got) in p(&g).into_iter().enumerate() {
            let want = expected(i);
            assert!(
                (got.0 - want.0).abs() < 1e-4 && (got.1 - want.1).abs() < 1e-4,
                "copy {i}: {got:?} vs {want:?}"
            );
        }
        // rot adds and a uniform scale multiplies, per copy.
        let rot = column(&g, names::ROT).as_f32(names::ROT).unwrap();
        let scale = column(&g, names::SCALE).as_vec2(names::SCALE).unwrap();
        for i in 0..6 {
            assert!((rot[i] - i as f32 * 20f32.to_radians()).abs() < 1e-5);
            assert!((scale[i].0 - 0.9f32.powi(i as i32)).abs() < 1e-5);
        }
        // A pure translation step is the arithmetic progression.
        let line = placements(repeat_step(Vec2(2.0, -1.0), 0.0, Vec2(1.0, 1.0)), 4);
        assert_eq!(
            p(&line),
            [
                Vec2(0.0, 0.0),
                Vec2(2.0, -1.0),
                Vec2(4.0, -2.0),
                Vec2(6.0, -3.0)
            ]
        );
    }

    /// A step whose scale is 0 collapses every copy after the first. They are
    /// kept (count and `index` stay as asked), carry scale 0, and nothing goes
    /// NaN, so the rasterizer draws nothing for them rather than garbage.
    #[test]
    fn a_scale_that_collapses_to_zero_stays_finite_and_keeps_the_count() {
        for rotate in [0.0, 30.0] {
            let g = placements(repeat_step(Vec2(4.0, 0.0), rotate, Vec2(0.0, 0.0)), 4);
            assert_eq!(g.instance_count(), 4);
            let scale = column(&g, names::SCALE).as_vec2(names::SCALE).unwrap();
            assert_eq!(scale[0], Vec2(1.0, 1.0));
            for s in &scale[1..] {
                assert_eq!(*s, Vec2(0.0, 0.0));
            }
            for point in p(&g) {
                assert!(point.0.is_finite() && point.1.is_finite());
            }
        }
        // Underflow of a decaying scale ends at zero the same way.
        let g = placements(repeat_step(Vec2(1.0, 0.0), 0.0, Vec2(0.1, 0.1)), 60);
        let last = *column(&g, names::SCALE)
            .as_vec2(names::SCALE)
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(last, Vec2(0.0, 0.0));
    }

    #[test]
    fn non_uniform_scale_with_a_turn_writes_the_shear_it_needs() {
        let step = repeat_step(Vec2(1.0, 0.0), 30.0, Vec2(2.0, 1.0));
        let g = placements(step, 3);
        let shear = column(&g, names::SHEAR).as_f32(names::SHEAR).unwrap();
        assert_eq!(shear[0], 0.0);
        assert!(shear[2] != 0.0);
        // Uniform scale needs none.
        let g = placements(repeat_step(Vec2(1.0, 0.0), 30.0, Vec2(2.0, 2.0)), 3);
        assert!(g.instances().get(names::SHEAR).is_none());
    }

    #[test]
    fn count_zero_is_empty_and_a_huge_count_is_clamped() {
        assert_eq!(
            placements(InstanceTransform::IDENTITY, 0).instance_count(),
            0
        );
        let g = placements(InstanceTransform::IDENTITY, MAX_REPEAT_COUNT + 5);
        assert_eq!(g.instance_count(), MAX_REPEAT_COUNT);
    }
}
