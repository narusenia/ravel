// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The Viewer's motion path: where a layer's `position` has been and where it
//! is going, with its keys grabbable in place.
//!
//! Three properties this overlay keeps:
//!
//! - **It asks for no evaluation.** A trajectory is two animation channels read
//!   at a list of frames, which is a document reading — nothing about it needs
//!   the evaluator, and returning an [`OverlayTarget`] would add a scoped target
//!   per selected layer to every viewer request for a picture that does not
//!   depend on one. [`ViewerOverlay::eval_targets`] therefore stays the trait
//!   default, and a test pins that.
//! - **Bounded work.** The polyline spans the layer's whole display interval,
//!   which can be tens of thousands of frames, so the frames sampled are strided
//!   down to [`MAX_MOTION_SAMPLES`]. The keys are capped the same way.
//! - **Drawing and grabbing share one resolution.** [`MotionPath::resolve`]
//!   produces the sampled polyline and the key points once; `paint`, `handles`
//!   and `drag` all read that, so a key cannot be drawn where it cannot be
//!   grabbed.
//!
//! The interval is taken from the layer's `[in_frame, out_frame)` rather than
//! asked about the current frame: a trajectory is about the frames the playhead
//! is *not* on, so there is no "is this layer showing" question here — and
//! therefore no [`ravel_core::composition::Layer::local_frame`] clamp to walk
//! into.
//!
//! Spatial bezier handles are deliberately absent (the plan's 非対象): `position`
//! is `[AnimationChannel; 2]`, two independent time curves, and spatial
//! interpolation would mean changing that representation everywhere.

use gpui::Hsla;
use ravel_core::composition::Layer;
use ravel_core::composition::transform::{Affine, world_matrix};
use ravel_core::eval::EvalContext;
use ravel_core::id::{CompId, LayerId};
use ravel_ui::ToolKind;

use super::ViewerPointerHint;
use super::geometry::stride_for;
use super::overlay::{
    Axis, DragModifiers, OverlayContext, OverlayEdit, OverlayHandle, OverlayHandleId, OverlayId,
    OverlayPainter, ShellChannel, ViewerOverlay, paint_handle_mark, priority,
};

/// Hard ceiling on trajectory samples, and on key points, per layer.
///
/// A layer can be ten thousand frames long, and the polyline is rebuilt on every
/// render and every pointer move. Two hundred and fifty-six segments already
/// draw a smooth curve at any zoom the panel offers.
pub const MAX_MOTION_SAMPLES: usize = 256;

/// The trajectory: dimmer than the selection accent, because it is context for
/// the layer rather than a thing being pointed at.
const PATH_COLOR: Hsla = Hsla {
    h: 0.58,
    s: 0.45,
    l: 0.75,
    a: 0.7,
};

/// The key marks, in the selection accent: these are grabbable.
const KEY_COLOR: Hsla = Hsla {
    h: 0.58,
    s: 0.7,
    l: 0.6,
    a: 0.95,
};

/// Screen-pixel side length of a key mark.
const KEY_MARK_PX: f32 = 7.0;

/// One layer's position trajectory, resolved once per paint or pointer event.
struct MotionPath {
    comp: CompId,
    layer: LayerId,
    /// The parent chain's matrix; identity without a parent. `position` is
    /// expressed in this space, so it is also what turns a channel value into a
    /// canvas point — and its inverse is what turns a dragged canvas point back
    /// into one.
    ///
    /// Taken at the frame on screen, the same reading
    /// [`ShellManipulator`](super::overlay::ShellManipulator) uses. A parent
    /// that is itself animated therefore shows the child's path through the
    /// parent's *current* pose, which is what makes the drawn path agree with
    /// the layer drawn under it.
    parent: Affine,
    /// The trajectory as canvas-space points, in frame order.
    points: Vec<(f32, f32)>,
    /// The layer-local frame of each key and where it sits on the canvas.
    keys: Vec<(u64, (f32, f32))>,
}

impl MotionPath {
    fn resolve(ctx: &OverlayContext) -> Option<Self> {
        let (document, resolution, playback) = ctx.resolved()?;
        let comp_id = ctx.layer_selection.comp()?;
        // Exactly one layer: two trajectories at once say nothing about which
        // key belongs to which layer.
        let [layer_id] = ctx.layer_selection.layers() else {
            return None;
        };
        let comp = document.get_composition(comp_id)?;
        let layer = comp.get_layer(*layer_id)?;
        let keyed = keyed_frames(layer);
        // Nothing to draw for a layer that does not move: one key is a static
        // position and no key at all is a constant.
        if keyed.len() < 2 {
            return None;
        }
        let eval = EvalContext::new(playback.frame, playback.fps, resolution);
        let parent = layer
            .parent
            .and_then(|id| comp.get_layer(id))
            .map(|parent| world_matrix(comp, parent, &eval))
            .unwrap_or(Affine::IDENTITY);
        let at = |local_frame: f64| {
            let point = (
                layer.transform.position[0].evaluate(local_frame, &eval),
                layer.transform.position[1].evaluate(local_frame, &eval),
            );
            parent.apply(point.0, point.1)
        };
        Some(Self {
            comp: comp_id,
            layer: *layer_id,
            parent,
            points: sample_frames(layer.in_frame, layer.out_frame)
                .into_iter()
                .map(|frame| at(frame as f64))
                .collect(),
            keys: keyed
                .into_iter()
                .map(|frame| (frame, at(frame as f64)))
                .collect(),
        })
    }

    /// The two channel writes a key drag makes: both components of `position`,
    /// at the frame the grabbed key sits on.
    ///
    /// `Some(frame)` is what keeps the curve a curve — the write lands on that
    /// key instead of collapsing the channel — and the frame is the key's own
    /// rather than the playhead's, because a motion path drag moves *that* key.
    fn key_edits(&self, frame: u64, target: (f32, f32)) -> Option<OverlayEdit> {
        let local = self.parent.inverse()?.apply(target.0, target.1);
        let write = |axis: Axis, value: f32| OverlayEdit::LayerTransform {
            comp: self.comp,
            layer: self.layer,
            channel: ShellChannel::Position(axis),
            value,
            local_frame: Some(frame),
        };
        Some(OverlayEdit::Batch(vec![
            write(Axis::X, local.0),
            write(Axis::Y, local.1),
        ]))
    }
}

/// The layer-local frames carrying a `position` key, either component's, inside
/// the layer's display interval and capped at [`MAX_MOTION_SAMPLES`].
///
/// The union of the two components: `position` is animated by keying x and y
/// independently, so a key on either is a point of the path. Keys outside
/// `[in_frame, out_frame)` are left out — the trajectory does not reach them, so
/// a mark there would sit off the line it belongs to.
fn keyed_frames(layer: &Layer) -> Vec<u64> {
    use ravel_core::animation::channel::ChannelSource;

    let mut frames: Vec<u64> = layer
        .transform
        .position
        .iter()
        .filter_map(|channel| match &channel.source {
            ChannelSource::Keyframes(curve) => Some(curve),
            _ => None,
        })
        .flat_map(|curve| curve.keyframes().iter().map(|key| key.frame))
        .filter(|frame| *frame >= layer.in_frame && *frame < layer.out_frame)
        .collect();
    frames.sort_unstable();
    frames.dedup();
    let stride = stride_for(frames.len(), MAX_MOTION_SAMPLES);
    frames.into_iter().step_by(stride).collect()
}

/// The layer-local frames the trajectory is sampled at: `[in, out)` strided down
/// to [`MAX_MOTION_SAMPLES`].
///
/// The last frame of the interval is appended when the stride would have skipped
/// it, so the drawn path reaches the end of the layer instead of stopping a
/// stride short of it.
fn sample_frames(in_frame: u64, out_frame: u64) -> Vec<u64> {
    let last = out_frame.saturating_sub(1);
    if last <= in_frame {
        return vec![in_frame];
    }
    let span = (last - in_frame + 1) as usize;
    let stride = stride_for(span, MAX_MOTION_SAMPLES) as u64;
    let mut frames: Vec<u64> = (in_frame..=last).step_by(stride as usize).collect();
    if frames.last() != Some(&last) {
        frames.push(last);
    }
    frames
}

/// The trajectory of the selected layer's `position`, with a grabbable mark on
/// every keyed frame.
pub struct MotionPathOverlay;

impl MotionPathOverlay {
    pub const ID: OverlayId = OverlayId("viewer.motion_path");
    /// Screen-pixel grab radius, the size the mark is drawn at so a grabbable
    /// key is always a visible one.
    const HIT_RADIUS_PX: f32 = 8.0;
}

impl ViewerOverlay for MotionPathOverlay {
    fn id(&self) -> OverlayId {
        Self::ID
    }

    fn priority(&self) -> i32 {
        priority::MOTION_PATH
    }

    fn is_active(&self, ctx: &OverlayContext) -> bool {
        // Only the Select tool, for the reason the two manipulators state: the
        // overlay hit test runs before `select_mouse_down` / `shape_mouse_down`,
        // so a live key mark under Rect / Ellipse / Hand / Zoom would answer the
        // press those tools are waiting for.
        ctx.tool == Some(ToolKind::Select) && MotionPath::resolve(ctx).is_some()
    }

    fn paint(&self, ctx: &OverlayContext, painter: &mut OverlayPainter) {
        let Some(path) = MotionPath::resolve(ctx) else {
            return;
        };
        painter.stroke_comp_polyline(&path.points, false, 1.0, PATH_COLOR);
        for (_, position) in &path.keys {
            paint_handle_mark(painter, *position, KEY_MARK_PX, KEY_COLOR);
        }
    }

    fn handles(&self, ctx: &OverlayContext) -> Vec<OverlayHandle> {
        let Some(path) = MotionPath::resolve(ctx) else {
            return Vec::new();
        };
        path.keys
            .into_iter()
            .map(|(frame, position)| OverlayHandle {
                overlay: Self::ID,
                id: OverlayHandleId::MotionKey(frame),
                position,
                hit_radius_px: Self::HIT_RADIUS_PX,
                hint: ViewerPointerHint::MovableBody,
                draggable: true,
            })
            .collect()
    }

    fn drag(
        &self,
        handle: &OverlayHandle,
        delta: (f32, f32),
        _modifiers: DragModifiers,
        ctx: &OverlayContext,
    ) -> Option<OverlayEdit> {
        let path = MotionPath::resolve(ctx)?;
        let frame = handle.id.motion_key()?;
        // The mark's own point is the grabbed one, so repeated calls during a
        // gesture stay absolute instead of compounding onto their own preview.
        let target = (handle.position.0 + delta.0, handle.position.1 + delta.1);
        path.key_edits(frame, target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::{LayerSelection, PlaybackPosition};
    use ravel_core::animation::channel::{AnimationChannel, ChannelSource};
    use ravel_core::animation::curve::{Keyframe, KeyframeCurve};
    use ravel_core::animation::interpolation::Interpolation;
    use ravel_core::composition::{Composition, Document};
    use ravel_core::graph::Graph;
    use ravel_core::types::{FrameRate, Vec2};

    /// A channel keyed at `(frame, value)` with `interpolation` leaving every
    /// key.
    fn keyed(points: &[(u64, f32)], interpolation: Interpolation) -> AnimationChannel {
        let mut curve = KeyframeCurve::new();
        for (frame, value) in points {
            curve.insert(*frame, *value, interpolation);
        }
        AnimationChannel::keyframes(curve)
    }

    /// One composition holding one selected layer whose `position` is keyed as
    /// given.
    ///
    /// **No evaluated results**, on purpose: a trajectory is a document reading,
    /// and a fixture that needed results would hide an overlay that asks for
    /// them. The layer's network still holds a node with a `GEOMETRY` output, so
    /// that "the motion path declares no target" is a claim about this overlay
    /// rather than about a network that had no target to declare.
    fn context(
        x: AnimationChannel,
        y: AnimationChannel,
        out_frame: u64,
    ) -> (OverlayContext, CompId, LayerId) {
        use ravel_core::graph::Node;
        use ravel_core::id::{DataTypeId, NodeId};

        let comp_id = CompId::next();
        let layer_id = LayerId::next();
        let network = Graph::new()
            .add_node(
                Node::new(NodeId::next(), "shape.rect")
                    .with_output("geometry", DataTypeId::GEOMETRY),
            )
            .unwrap();
        let mut layer = Layer::new(layer_id, "Layer", network).with_time(0, 0, out_frame.max(1));
        layer.transform.position = [x, y];
        let comp = Composition::new(
            comp_id,
            "Comp",
            (1920, 1080),
            FrameRate::new(30, 1),
            out_frame.max(1),
        )
        .add_layer(layer);
        let ctx = OverlayContext {
            resolution: Some((1920, 1080)),
            playback: Some(PlaybackPosition::default()),
            document: Some(Document::default().with_composition(comp)),
            layer_selection: LayerSelection::of(comp_id, vec![layer_id]),
            tool: Some(ToolKind::Select),
            ..OverlayContext::default()
        };
        (ctx, comp_id, layer_id)
    }

    /// The fixture used by most tests: a straight linear move from (100, 100) to
    /// (300, 200) over frames 0..=60, on a 120-frame layer.
    fn linear_context() -> (OverlayContext, CompId, LayerId) {
        context(
            keyed(&[(0, 100.0), (60, 300.0)], Interpolation::Linear),
            keyed(&[(0, 100.0), (60, 200.0)], Interpolation::Linear),
            120,
        )
    }

    fn path_of(ctx: &OverlayContext) -> MotionPath {
        MotionPath::resolve(ctx).expect("the fixture layer has a keyed position")
    }

    /// Perpendicular distance of `point` from the line through `a` and `b`.
    fn deviation(a: (f32, f32), b: (f32, f32), point: (f32, f32)) -> f32 {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let length = dx.hypot(dy);
        ((point.0 - a.0) * dy - (point.1 - a.1) * dx).abs() / length
    }

    /// Completion criterion: two linearly interpolated keys give a straight
    /// trajectory.
    #[test]
    fn two_linear_keys_sample_to_a_straight_line() {
        let (ctx, ..) = linear_context();
        let path = path_of(&ctx);
        assert!(path.points.len() > 2, "the path is a polyline");
        let (first, last) = (path.points[0], *path.points.last().unwrap());
        for point in &path.points {
            assert!(
                deviation(first, last, *point) < 1e-2,
                "{point:?} left the straight line from {first:?} to {last:?}"
            );
        }
        // The keys sit on the trajectory, at the values the channels hold.
        assert_eq!(
            path.keys,
            vec![(0, (100.0, 100.0)), (60, (300.0, 200.0))],
            "the key marks are not the keyed values"
        );
    }

    /// Completion criterion: a bezier-interpolated channel samples as a curve.
    #[test]
    fn a_bezier_channel_samples_to_a_curve() {
        // Bezier with tangents that ease x out slowly and in hard, so x lags
        // behind the straight line while y ramps linearly.
        let mut curve = KeyframeCurve::new();
        curve.insert_keyframe(
            Keyframe::new(0, 100.0, Interpolation::Bezier)
                .with_tangents(Vec2(0.0, 0.0), Vec2(40.0, 0.0)),
        );
        curve.insert_keyframe(
            Keyframe::new(60, 300.0, Interpolation::Bezier)
                .with_tangents(Vec2(-40.0, 0.0), Vec2(0.0, 0.0)),
        );
        let (ctx, ..) = context(
            AnimationChannel::keyframes(curve),
            keyed(&[(0, 100.0), (60, 200.0)], Interpolation::Linear),
            120,
        );
        let path = path_of(&ctx);
        let (first, last) = (path.points[0], *path.points.last().unwrap());
        let worst = path
            .points
            .iter()
            .map(|point| deviation(first, last, *point))
            .fold(0.0f32, f32::max);
        assert!(
            worst > 1.0,
            "the bezier channel was sampled as a straight line (worst deviation {worst})"
        );
    }

    /// Completion criterion: the sample count stays under the cap however long
    /// the layer is.
    #[test]
    fn the_sample_count_stays_under_its_cap() {
        let (ctx, ..) = context(
            keyed(&[(0, 0.0), (9_000, 500.0)], Interpolation::Linear),
            keyed(&[(0, 0.0), (9_000, 500.0)], Interpolation::Linear),
            10_000,
        );
        let path = path_of(&ctx);
        assert!(
            path.points.len() <= MAX_MOTION_SAMPLES + 1,
            "{} samples for a 10 000 frame layer",
            path.points.len()
        );
        // Strided, not truncated: the path still reaches the end of the layer.
        assert!(
            path.points.last().unwrap().0 > 490.0,
            "the cap cut the trajectory short: {:?}",
            path.points.last()
        );

        // The key marks are capped the same way.
        let mut curve = KeyframeCurve::new();
        for frame in 0..(MAX_MOTION_SAMPLES as u64 * 4) {
            curve.insert(frame, frame as f32, Interpolation::Linear);
        }
        let (ctx, ..) = context(
            AnimationChannel::keyframes(curve),
            keyed(&[(0, 0.0), (900, 10.0)], Interpolation::Linear),
            1_000,
        );
        assert!(path_of(&ctx).keys.len() <= MAX_MOTION_SAMPLES);
    }

    /// Completion criterion: dragging a key writes both components of
    /// `position` at that key's own frame, and leaves the other keys alone.
    #[test]
    fn dragging_a_key_writes_both_components_at_that_frame() {
        let (ctx, comp, layer) = linear_context();
        let overlay = MotionPathOverlay;
        let handles = overlay.handles(&ctx);
        assert_eq!(handles.len(), 2, "one handle per key");
        let handle = handles
            .iter()
            .find(|handle| handle.id == OverlayHandleId::MotionKey(60))
            .expect("the second key is grabbable");

        let edit = overlay
            .drag(handle, (25.0, -15.0), DragModifiers::default(), &ctx)
            .expect("a key drag writes something");
        match &edit {
            OverlayEdit::Batch(edits) => assert_eq!(
                edits.len(),
                2,
                "a key drag has to write both components: {edits:?}"
            ),
            other => panic!("expected a batch of both components, got {other:?}"),
        }

        let updated = edit
            .apply(ctx.document.as_ref().unwrap())
            .expect("the edit applies");
        let transform = &updated
            .get_composition(comp)
            .unwrap()
            .get_layer(layer)
            .unwrap()
            .transform;
        for (axis, expected) in [(0usize, 325.0f32), (1, 185.0)] {
            let ChannelSource::Keyframes(curve) = &transform.position[axis].source else {
                panic!("component {axis} was flattened out of a curve");
            };
            let keys: Vec<_> = curve
                .keyframes()
                .iter()
                .map(|key| (key.frame, key.value))
                .collect();
            assert_eq!(
                keys.len(),
                2,
                "component {axis} gained or lost a key: {keys:?}"
            );
            assert_eq!(keys[0].0, 0, "the first key moved");
            assert_eq!(keys[1].0, 60, "the write landed on another frame");
            assert!(
                (keys[1].1 - expected).abs() < 1e-3,
                "component {axis} key value {} != {expected}",
                keys[1].1
            );
        }
        // The frame the key sits on is what was written, not the playhead's.
        assert_eq!(
            transform.position[0].evaluate(0.0, &eval()),
            100.0,
            "the untouched key changed value"
        );
    }

    /// The path of a parented layer goes through the parent's transform, so it
    /// lies where the layer is drawn — and a key drag comes back the same way.
    #[test]
    fn a_parented_layer_draws_and_drags_through_its_parents_transform() {
        let (ctx, comp, layer) = linear_context();
        // A non-uniform scale *and* a rotation: an identity-like parent would
        // let an implementation that ignores the chain pass this test.
        let parent_id = LayerId::next();
        let mut parent = Layer::new(parent_id, "Parent", Graph::new()).with_time(0, 0, 120);
        parent.transform.position = [
            AnimationChannel::constant(40.0),
            AnimationChannel::constant(70.0),
        ];
        parent.transform.scale = [
            AnimationChannel::constant(2.0),
            AnimationChannel::constant(0.5),
        ];
        parent.transform.rotation = AnimationChannel::constant(30.0);
        let document =
            ravel_ui::document::add_layer(ctx.document.as_ref().unwrap(), comp, parent).unwrap();
        let document = ravel_ui::document::update_layer(&document, comp, layer, |layer| {
            layer.parent = Some(parent_id)
        })
        .unwrap();
        let mut ctx = ctx;
        ctx.document = Some(document);

        let world = {
            let document = ctx.document.as_ref().unwrap();
            let composition = document.get_composition(comp).unwrap();
            world_matrix(
                composition,
                composition.get_layer(parent_id).unwrap(),
                &eval(),
            )
        };
        let path = path_of(&ctx);
        let expected = world.apply(100.0, 100.0);
        assert!(
            (path.keys[0].1.0 - expected.0).abs() < 1e-3
                && (path.keys[0].1.1 - expected.1).abs() < 1e-3,
            "the key was drawn at {:?} instead of the parent's {expected:?}",
            path.keys[0].1
        );

        // And the drag inverts the same chain: moving the mark by (30, 10) on
        // the canvas has to leave it under the pointer.
        let handle = MotionPathOverlay
            .handles(&ctx)
            .into_iter()
            .find(|handle| handle.id == OverlayHandleId::MotionKey(0))
            .expect("the first key is grabbable");
        let edit = MotionPathOverlay
            .drag(&handle, (30.0, 10.0), DragModifiers::default(), &ctx)
            .expect("a parented key drag writes something");
        let updated = edit.apply(ctx.document.as_ref().unwrap()).unwrap();
        let mut moved = ctx.clone();
        moved.document = Some(updated);
        let drawn = path_of(&moved).keys[0].1;
        assert!(
            (drawn.0 - (expected.0 + 30.0)).abs() < 1e-2
                && (drawn.1 - (expected.1 + 10.0)).abs() < 1e-2,
            "the dragged key landed at {drawn:?} rather than under the pointer"
        );
    }

    /// Completion criterion: drawing the trajectory issues no evaluation
    /// request. The whole overlay is a document reading, so it declares no
    /// target — and adding one would put a scoped target in every viewer
    /// request for a picture that does not need it.
    #[test]
    fn the_trajectory_asks_for_no_evaluation() {
        let (ctx, ..) = linear_context();
        assert!(
            MotionPathOverlay.is_active(&ctx),
            "the fixture has to reach the code path that could ask"
        );
        assert!(
            MotionPathOverlay.eval_targets(&ctx).is_empty(),
            "the motion path declared an evaluation target"
        );

        // And through the registry: keying a layer's position must not grow the
        // request the viewer posts.
        let registry = super::super::overlay::OverlayRegistry::builtin();
        let (still, ..) = context(
            AnimationChannel::constant(100.0),
            AnimationChannel::constant(100.0),
            120,
        );
        assert_eq!(
            registry.eval_targets(&ctx),
            registry.eval_targets(&still),
            "the motion path changed what the viewer asks the evaluator for"
        );
    }

    /// The overlay stands down for anything that is not one moving layer.
    #[test]
    fn the_path_needs_one_layer_that_actually_moves() {
        let (still, ..) = context(
            AnimationChannel::constant(100.0),
            AnimationChannel::constant(100.0),
            120,
        );
        assert!(
            !MotionPathOverlay.is_active(&still),
            "a constant has no path"
        );

        let (one_key, ..) = context(
            keyed(&[(0, 100.0)], Interpolation::Linear),
            AnimationChannel::constant(100.0),
            120,
        );
        assert!(
            !MotionPathOverlay.is_active(&one_key),
            "a single key is a static position"
        );

        let (mut ctx, comp, layer) = linear_context();
        ctx.layer_selection = LayerSelection::of(comp, vec![layer, LayerId::next()]);
        assert!(
            !MotionPathOverlay.is_active(&ctx),
            "two selected layers have no single trajectory"
        );

        let (mut ctx, ..) = linear_context();
        for tool in [
            ToolKind::Rect,
            ToolKind::Ellipse,
            ToolKind::Pen,
            ToolKind::Hand,
            ToolKind::Zoom,
        ] {
            ctx.tool = Some(tool);
            assert!(
                !MotionPathOverlay.is_active(&ctx),
                "{tool:?} would lose the press it is waiting for"
            );
        }
    }

    /// Keys outside the display interval are not drawn: the trajectory does not
    /// reach them, so a mark there would sit off the line it belongs to.
    #[test]
    fn keys_outside_the_display_interval_are_left_out() {
        let (ctx, ..) = context(
            keyed(
                &[(0, 0.0), (30, 100.0), (200, 900.0)],
                Interpolation::Linear,
            ),
            keyed(&[(0, 0.0), (30, 50.0)], Interpolation::Linear),
            60,
        );
        assert_eq!(
            path_of(&ctx)
                .keys
                .iter()
                .map(|(frame, _)| *frame)
                .collect::<Vec<_>>(),
            vec![0, 30],
            "a key past out_frame was drawn"
        );
    }

    fn eval() -> EvalContext {
        EvalContext::new(0, FrameRate::new(30, 1), (1920, 1080))
    }
}
