// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shell transform math: a layer's own matrix and its parent chain
//! (REQ-LAYER-001).
//!
//! This is the single source of truth for "where does a layer's content land
//! in the composition". The renderer (`comp.transform`) and the viewer's bbox
//! / hit-test overlay both compose the chain from here, so the drawn pixels
//! and the drawn bounding box can never disagree.
//!
//! Parenting is a transform relationship only: an ancestor contributes its
//! matrix regardless of its own solo / mute state (REQ-LAYER-001). Muting a
//! parent hides the parent's own pixels; it does not detach its children.
//!
//! Every layer's channels are evaluated at **that layer's** local frame
//! (REQ-LAYER-006), so a parent with a different time placement than its
//! child still animates on its own timing.

use crate::composition::{Composition, Layer};
use crate::eval::EvalContext;

// ===========================================================================
// 2D affine matrix
// ===========================================================================

/// Row-major 2×3 affine matrix: `x' = m0·x + m1·y + m2`, `y' = m3·x + m4·y + m5`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine(pub [f32; 6]);

/// `self ∘ rhs`: apply `rhs` first, then `self` — the usual matrix product,
/// so a parent chain composes as `parent * child`.
impl std::ops::Mul for Affine {
    type Output = Affine;

    fn mul(self, rhs: Affine) -> Affine {
        let a = self.0;
        let b = rhs.0;
        Affine([
            a[0] * b[0] + a[1] * b[3],
            a[0] * b[1] + a[1] * b[4],
            a[0] * b[2] + a[1] * b[5] + a[2],
            a[3] * b[0] + a[4] * b[3],
            a[3] * b[1] + a[4] * b[4],
            a[3] * b[2] + a[4] * b[5] + a[5],
        ])
    }
}

impl Affine {
    pub const IDENTITY: Affine = Affine([1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);

    pub fn apply(self, x: f32, y: f32) -> (f32, f32) {
        let m = self.0;
        (m[0] * x + m[1] * y + m[2], m[3] * x + m[4] * y + m[5])
    }

    /// Inverse, or `None` when the matrix is singular (e.g. zero scale).
    pub fn inverse(self) -> Option<Affine> {
        let m = self.0;
        let det = m[0] * m[4] - m[1] * m[3];
        if det.abs() < 1e-10 {
            return None;
        }
        let inv_det = 1.0 / det;
        let a = m[4] * inv_det;
        let b = -m[1] * inv_det;
        let d = -m[3] * inv_det;
        let e = m[0] * inv_det;
        Some(Affine([
            a,
            b,
            -(a * m[2] + b * m[5]),
            d,
            e,
            -(d * m[2] + e * m[5]),
        ]))
    }

    pub fn is_identity(self) -> bool {
        let m = self.0;
        let i = Affine::IDENTITY.0;
        m.iter().zip(i).all(|(a, b)| (a - b).abs() < 1e-6)
    }
}

// ===========================================================================
// Layer matrices
// ===========================================================================

/// The layer's local transform matrix at its local frame `lf`:
/// `T(position) · R(rotation°) · S(scale) · T(-anchor)`.
///
/// Translation is expressed on the output canvas: composition-space positions
/// are scaled by the context's comp-to-canvas factor, which is `1.0` for
/// UI-side contexts (the viewer evaluates in composition space).
pub fn layer_matrix(layer: &Layer, lf: u64, ctx: &EvalContext) -> Affine {
    let t = &layer.transform;
    let ax = t.anchor_point[0].evaluate(lf, ctx);
    let ay = t.anchor_point[1].evaluate(lf, ctx);
    let px = t.position[0].evaluate(lf, ctx);
    let py = t.position[1].evaluate(lf, ctx);
    let sx = t.scale[0].evaluate(lf, ctx);
    let sy = t.scale[1].evaluate(lf, ctx);
    let rot = t.rotation.evaluate(lf, ctx).to_radians();
    let (sin, cos) = rot.sin_cos();

    // T(px, py) · R · S · T(-ax, -ay), composed directly.
    let mut matrix = Affine([
        cos * sx,
        -sin * sy,
        px - (cos * sx * ax - sin * sy * ay),
        sin * sx,
        cos * sy,
        py - (sin * sx * ax + cos * sy * ay),
    ]);
    let (scale_x, scale_y) = ctx.comp_to_canvas_scale();
    matrix.0[2] *= scale_x as f32;
    matrix.0[5] *= scale_y as f32;
    matrix
}

/// The layer's world matrix: the whole parent chain composed onto the layer's
/// own matrix.
///
/// Ancestors contribute whether or not they survive solo / mute filtering —
/// parenting is independent of visibility (REQ-LAYER-001) — and each
/// ancestor's channels are evaluated at that ancestor's own local frame
/// (REQ-LAYER-006). Parent cycles are rejected by validation; a visited guard
/// keeps evaluation robust regardless.
pub fn world_matrix(comp: &Composition, layer: &Layer, ctx: &EvalContext) -> Affine {
    let mut matrix = layer_matrix(layer, layer.local_frame(ctx.frame), ctx);
    for ancestor in comp.ancestors(layer) {
        matrix = layer_matrix(ancestor, ancestor.local_frame(ctx.frame), ctx) * matrix;
    }
    matrix
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::channel::AnimationChannel;
    use crate::graph::Graph;
    use crate::id::{CompId, LayerId};
    use crate::types::FrameRate;

    fn ctx() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }

    fn translated(name: &str, x: f32, y: f32) -> Layer {
        let mut layer = Layer::new(LayerId::next(), name, Graph::new());
        layer.transform.position = [AnimationChannel::constant(x), AnimationChannel::constant(y)];
        layer
    }

    fn comp_with(layers: Vec<Layer>) -> Composition {
        let mut comp = Composition::new(
            CompId::next(),
            "Comp",
            (1920, 1080),
            FrameRate::new(30, 1),
            300,
        );
        for layer in layers {
            comp.layers.push_back(layer);
        }
        comp
    }

    #[test]
    fn affine_inverse_roundtrip() {
        let m = Affine([1.5, 0.2, 10.0, -0.3, 2.0, -4.0]);
        let inv = m.inverse().unwrap();
        let (x, y) = m.apply(3.0, 7.0);
        let (rx, ry) = inv.apply(x, y);
        assert!((rx - 3.0).abs() < 1e-4 && (ry - 7.0).abs() < 1e-4);
    }

    #[test]
    fn singular_matrix_has_no_inverse() {
        assert!(Affine([0.0; 6]).inverse().is_none());
    }

    #[test]
    fn identity_detection() {
        assert!(Affine::IDENTITY.is_identity());
        assert!(!Affine([1.0, 0.0, 5.0, 0.0, 1.0, 0.0]).is_identity());
    }

    #[test]
    fn layer_translation_scales_from_comp_to_canvas_space() {
        let mut layer = Layer::new(LayerId::new(1), "Layer", Graph::new());
        layer.transform.anchor_point[0] = AnimationChannel::constant(4.0);
        layer.transform.anchor_point[1] = AnimationChannel::constant(8.0);
        layer.transform.position[0] = AnimationChannel::constant(20.0);
        layer.transform.position[1] = AnimationChannel::constant(24.0);
        let ctx =
            EvalContext::new(0, FrameRate::new(30, 1), (64, 64)).with_comp_resolution((128, 128));

        let matrix = layer_matrix(&layer, 0, &ctx);
        assert_eq!(matrix, Affine([1.0, 0.0, 8.0, 0.0, 1.0, 8.0]));
    }

    #[test]
    fn parent_chain_composes_bottom_up() {
        let grandparent = translated("grandparent", 100.0, 0.0);
        let parent = translated("parent", 0.0, 50.0).with_parent(grandparent.id);
        let child = translated("child", 5.0, 5.0).with_parent(parent.id);
        let comp = comp_with(vec![grandparent, parent, child.clone()]);

        let m = world_matrix(&comp, &child, &ctx());
        assert_eq!((m.0[2], m.0[5]), (105.0, 55.0));
    }

    /// Composition order matters as soon as the parent rotates or scales:
    /// the child's own translation must be measured in the parent's frame
    /// (`parent * child`), not the composition's. Translations alone commute,
    /// so this is the case that pins the operand order.
    #[test]
    fn parent_rotation_applies_to_the_childs_offset() {
        use crate::animation::channel::AnimationChannel;

        let mut parent = translated("parent", 10.0, 0.0);
        parent.transform.rotation = AnimationChannel::constant(90.0);
        parent.transform.scale = [
            AnimationChannel::constant(2.0),
            AnimationChannel::constant(2.0),
        ];
        let child = translated("child", 5.0, 0.0).with_parent(parent.id);
        let comp = comp_with(vec![parent, child.clone()]);

        // The child sits 5 to the parent's right; the parent scales by 2 and
        // turns 90° (y down), so the child lands 10 below the parent's origin.
        let m = world_matrix(&comp, &child, &ctx());
        let (x, y) = m.apply(0.0, 0.0);
        assert!(
            (x - 10.0).abs() < 1e-4 && (y - 10.0).abs() < 1e-4,
            "child origin at ({x}, {y})"
        );
    }

    /// Parenting is a transform relationship, not a visibility one: muting or
    /// un-soloing a parent must not detach its children (REQ-LAYER-001).
    #[test]
    fn muted_and_unsoloed_parents_still_transform_children() {
        let mut parent = translated("parent", 100.0, 50.0);
        parent.muted = true;
        let child = translated("child", 0.0, 0.0).with_parent(parent.id);
        let comp = comp_with(vec![parent.clone(), child.clone()]);
        let m = world_matrix(&comp, &child, &ctx());
        assert_eq!((m.0[2], m.0[5]), (100.0, 50.0));

        // A solo elsewhere in the comp filters the parent out of the render
        // but leaves the parenting relationship intact.
        parent.muted = false;
        let mut other = translated("other", 0.0, 0.0);
        other.solo = true;
        let comp = comp_with(vec![parent, child.clone(), other]);
        let m = world_matrix(&comp, &child, &ctx());
        assert_eq!((m.0[2], m.0[5]), (100.0, 50.0));
    }

    /// Each layer's channels are sampled at its own local frame
    /// (REQ-LAYER-006): a parent placed later on the timeline animates on its
    /// own timing, not the child's.
    #[test]
    fn ancestors_evaluate_at_their_own_local_frame() {
        use crate::animation::curve::KeyframeCurve;
        use crate::animation::interpolation::Interpolation;

        let mut parent = Layer::new(LayerId::next(), "parent", Graph::new()).with_time(10, 0, 100);
        let mut curve = KeyframeCurve::new();
        curve.insert(0, 0.0, Interpolation::Linear);
        curve.insert(10, 100.0, Interpolation::Linear);
        parent.transform.position[0] = AnimationChannel::keyframes(curve);

        let child = translated("child", 0.0, 0.0)
            .with_parent(parent.id)
            .with_time(0, 0, 100);
        let comp = comp_with(vec![parent, child.clone()]);

        // Comp frame 10 is the parent's local frame 0 → its animation has not
        // started, so the child inherits no offset.
        let at_10 = EvalContext::new(10, FrameRate::new(30, 1), (1920, 1080));
        assert_eq!(world_matrix(&comp, &child, &at_10).0[2], 0.0);
        // Comp frame 20 is the parent's local frame 10 → full offset.
        let at_20 = EvalContext::new(20, FrameRate::new(30, 1), (1920, 1080));
        assert_eq!(world_matrix(&comp, &child, &at_20).0[2], 100.0);
    }

    #[test]
    fn parent_cycles_terminate() {
        let a_id = LayerId::next();
        let b_id = LayerId::next();
        let a = Layer::new(a_id, "a", Graph::new()).with_parent(b_id);
        let b = Layer::new(b_id, "b", Graph::new()).with_parent(a_id);
        let comp = comp_with(vec![a.clone(), b]);
        assert!(world_matrix(&comp, &a, &ctx()).is_identity());
    }
}
