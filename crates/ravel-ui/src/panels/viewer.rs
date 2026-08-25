// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Headless state for the viewer panel.
//!
//! The viewer evaluates the active composition at a **fraction** of the
//! composition resolution rather than at a hidden absolute cap
//! (REQ-UI-004, `done/viewer-preview-resolution-plan.md`). A factor keeps the
//! meaning of a setting the same for a 1080p and an 8K composition — the user
//! can predict how coarse the preview is — and it leaves a path to inspecting
//! the output at composition resolution, which an absolute cap denied
//! entirely.
//!
//! The factor is view state, not document content: it says how the user is
//! looking at the composition right now, so it never reaches `.ravprj`.

use crate::command::CommandId;
use ravel_core::color::DisplayChannel;
use serde::{Deserialize, Serialize};

/// Preview resolution factor applied to the composition resolution before the
/// viewer evaluates it.
///
/// [`ViewerResolution::Half`] is the default. On a 1080p composition it
/// evaluates 960x540, which costs about what the hidden 1024 px long-edge cap
/// it replaces did (1024x576): `perf-baseline.md`, section
/// "ビューア経路の表示解像度", measures about 15.8 ms for 1080p against about
/// 5.7 ms for 1024x576, so defaulting to `Full` would make every session
/// three times slower than the one before it. The default therefore preserves
/// the responsiveness users already have, and `Full` is the deliberate
/// "stop and check the result" choice.
///
/// There is no `1/3`: it sits between the two useful steps without adding a
/// decision the user can make confidently, and it can be added later if the
/// need shows up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewerResolution {
    /// Evaluate at the composition resolution.
    Full,
    /// Evaluate at half the composition resolution on each axis.
    #[default]
    Half,
    /// Evaluate at a quarter of the composition resolution on each axis.
    Quarter,
}

impl ViewerResolution {
    /// Every factor, in decreasing quality order. The order the UI offers
    /// them in.
    pub const ALL: [ViewerResolution; 3] = [Self::Full, Self::Half, Self::Quarter];

    /// The divisor this factor applies to each axis.
    pub fn divisor(self) -> u32 {
        match self {
            Self::Full => 1,
            Self::Half => 2,
            Self::Quarter => 4,
        }
    }

    /// Locale key for this factor's name.
    ///
    /// The key, not the text: `ravel-ui` does not depend on i18n, so the
    /// display boundary in `ravel-app` resolves it (the same shape as
    /// [`crate::ToolKind::label_key`]).
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Full => "viewer.resolution_full",
            Self::Half => "viewer.resolution_half",
            Self::Quarter => "viewer.resolution_quarter",
        }
    }

    /// The next factor in [`ViewerResolution::ALL`], wrapping past the last.
    ///
    /// The factors are one ordered axis, so the command system gets a single
    /// cycling command rather than three "set to X" commands: one chord walks
    /// the axis and the keybinding list stays one row instead of three.
    pub fn cycled(self) -> Self {
        let index = Self::ALL.iter().position(|factor| *factor == self);
        // `ALL` contains every variant, so the fallback is unreachable; a 0
        // start is still the honest answer if a variant is ever left out.
        Self::ALL[(index.unwrap_or(0) + 1) % Self::ALL.len()]
    }

    /// One step coarser, saturating at [`ViewerResolution::Quarter`].
    ///
    /// This is the adaptive step: while the user is dragging, scrubbing or
    /// editing a parameter, the viewer evaluates at this instead of the
    /// selection and comes back when the input stops (`VRES-4`). It saturates
    /// rather than wrapping — the point is to make the preview cheaper, and
    /// `Quarter` going to `Full` mid-gesture would do the exact opposite.
    /// `Quarter` therefore never adapts at all, which is also the honest
    /// answer for a user who has already asked for the cheapest preview.
    pub fn lowered(self) -> Self {
        match self {
            Self::Full => Self::Half,
            Self::Half | Self::Quarter => Self::Quarter,
        }
    }

    /// The evaluation resolution for a composition sized `(w, h)`.
    ///
    /// Rounding is **`div_ceil`**, chosen over `round` plus a `max(1)` clamp
    /// for two reasons:
    ///
    /// - it cannot produce a zero-sized buffer from a non-empty composition,
    ///   so the degenerate case is excluded by construction instead of by a
    ///   clamp somebody can forget: a 1x1 composition stays 1x1 at
    ///   [`ViewerResolution::Quarter`];
    /// - rounding up means the preview is never *smaller* than the exact
    ///   fraction, so the viewer's evaluation-buffer-to-composition transform
    ///   (`done/viewer-comp-coordinate-scale-plan.md`) magnifies slightly
    ///   less rather than slightly more.
    ///
    /// The aspect ratio is preserved up to the sub-pixel rounding of each
    /// axis, because the same divisor is applied to both.
    pub fn apply(self, (w, h): (u32, u32)) -> (u32, u32) {
        let divisor = self.divisor();
        (w.div_ceil(divisor), h.div_ceil(divisor))
    }
}

/// Locale key for a display channel's name (`INSP-2`).
///
/// A free function because [`DisplayChannel`] belongs to `ravel-core`, where
/// the pixels are: the transform that isolates a channel is a colour
/// operation, and the enum has to reach `ravel-nodes`, which does not depend
/// on this crate. The UI vocabulary — what the mode is *called* — stays here,
/// the same split [`ViewerResolution::label_key`] makes.
pub fn display_channel_label_key(channel: DisplayChannel) -> &'static str {
    match channel {
        DisplayChannel::Rgb => "viewer.channel_rgb",
        DisplayChannel::Red => "viewer.channel_red",
        DisplayChannel::Green => "viewer.channel_green",
        DisplayChannel::Blue => "viewer.channel_blue",
        DisplayChannel::Alpha => "viewer.channel_alpha",
        DisplayChannel::AlphaMatte => "viewer.channel_alpha_matte",
    }
}

/// The channel `cmd` selects, `None` for every other command.
///
/// [`DisplayChannel::AlphaMatte`] is deliberately absent: it has no command
/// (see [`CommandId::ViewerChannelRgb`]), so the host's dispatch cannot reach
/// it and the toolbar menu is the only way in.
pub fn display_channel_from_command(cmd: CommandId) -> Option<DisplayChannel> {
    match cmd {
        CommandId::ViewerChannelRgb => Some(DisplayChannel::Rgb),
        CommandId::ViewerChannelRed => Some(DisplayChannel::Red),
        CommandId::ViewerChannelGreen => Some(DisplayChannel::Green),
        CommandId::ViewerChannelBlue => Some(DisplayChannel::Blue),
        CommandId::ViewerChannelAlpha => Some(DisplayChannel::Alpha),
        _ => None,
    }
}

/// How the viewer's pixel readout prints a channel value (`INSP-3`).
///
/// Both forms report the **evaluated** value — what the graph produced, in the
/// working space — and differ only in the scale they print it on.
/// [`PixelReadoutFormat::Byte`] is therefore the same number quantised to
/// 0–255, *not* the display-encoded byte on screen: the display transform runs
/// on the evaluation worker with the user's LUT, and a readout that silently
/// re-applied a second, LUT-less approximation of it would report values no
/// node in the graph ever held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PixelReadoutFormat {
    /// The value as evaluated, four decimals. The default: values outside
    /// 0–1 are ordinary in linear light and only this form can show them.
    #[default]
    Float,
    /// The same value on a 0–255 scale, for comparing against 8-bit sources.
    Byte,
}

impl PixelReadoutFormat {
    /// Both forms, in the order the UI offers them.
    pub const ALL: [PixelReadoutFormat; 2] = [Self::Float, Self::Byte];

    /// Locale key for this form's name. The key, not the text — the same split
    /// [`ViewerResolution::label_key`] makes.
    pub fn label_key(self) -> &'static str {
        match self {
            Self::Float => "viewer.pixel_readout_float",
            Self::Byte => "viewer.pixel_readout_byte",
        }
    }

    /// The other form. Two of them, so the command is one toggle rather than a
    /// pair of "set to X" commands.
    pub fn toggled(self) -> Self {
        match self {
            Self::Float => Self::Byte,
            Self::Byte => Self::Float,
        }
    }

    /// One channel value, printed.
    pub fn channel(self, value: f32) -> String {
        match self {
            Self::Float => format!("{value:.4}"),
            Self::Byte => ravel_core::color::quantize_u8(value).to_string(),
        }
    }
}

/// The evaluation-buffer pixel under a composition-space point (`INSP-3`).
///
/// The viewer evaluates at a fraction of the composition resolution
/// ([`ViewerResolution`]), so the buffer the values live in is usually smaller
/// than the coordinate space the pointer is resolved in. Both resolutions
/// travel with the frame (`ViewerFrame::Frame`) precisely so this conversion
/// can be exact rather than assumed.
///
/// `None` when the point is outside the composition — the readout then shows
/// nothing rather than clamping to the nearest edge pixel, which would report
/// a value for a place the pointer is not.
///
/// The composition domain is half-open, `[0, width) x [0, height)`: pixel *n*
/// covers `[n, n+1)`, so the composition's own last pixel is the last index and
/// `width` exactly is already outside. The scale is applied before the floor
/// (not the floor before the scale) so that the mapping is the buffer's own
/// pixel grid rather than the composition's rounded twice; the `min` afterwards
/// only guards the float multiply landing exactly on the upper bound.
pub fn comp_to_buffer_index(
    (x, y): (f32, f32),
    (comp_width, comp_height): (u32, u32),
    (buffer_width, buffer_height): (u32, u32),
) -> Option<(u32, u32)> {
    if comp_width == 0 || comp_height == 0 || buffer_width == 0 || buffer_height == 0 {
        return None;
    }
    if !(x >= 0.0 && y >= 0.0) || x >= comp_width as f32 || y >= comp_height as f32 {
        return None;
    }
    let scaled = |value: f32, comp: u32, buffer: u32| {
        ((value * buffer as f32 / comp as f32).floor() as u32).min(buffer - 1)
    };
    Some((
        scaled(x, comp_width, buffer_width),
        scaled(y, comp_height, buffer_height),
    ))
}

/// The readout line: the composition pixel under the pointer, then its four
/// channels.
///
/// No locale key: every token is either a number or the channel initial, and
/// `R G B A` is the vocabulary of the domain rather than of a language. The
/// *names* around it — the toolbar button, the format menu — are localised.
pub fn pixel_readout_text(
    (x, y): (f32, f32),
    rgba: [f32; 4],
    format: PixelReadoutFormat,
) -> String {
    let channels: Vec<String> = ["R", "G", "B", "A"]
        .iter()
        .zip(rgba)
        .map(|(name, value)| format!("{name} {}", format.channel(value)))
        .collect();
    format!(
        "{}, {}   {}",
        x.floor() as i64,
        y.floor() as i64,
        channels.join("  ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::eval::{
        EvalContext, EvalScope, Evaluator, NodeProcessor, Quality, ResolvedParams,
    };
    use ravel_core::graph::{Graph, Node};
    use ravel_core::id::{DataTypeId, NodeId};
    use ravel_core::types::{FrameRate, NodeData, Scalar};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn default_is_half() {
        assert_eq!(ViewerResolution::default(), ViewerResolution::Half);
    }

    #[test]
    fn cycling_walks_every_factor_and_wraps() {
        let mut factor = ViewerResolution::ALL[0];
        let mut visited = Vec::new();
        for _ in 0..ViewerResolution::ALL.len() {
            visited.push(factor);
            factor = factor.cycled();
        }
        // Every factor once, in the order the picker lists them, and back to
        // the start: a cycle that skips one leaves it reachable only from the
        // menu, and one that does not wrap dead-ends on `Quarter`.
        assert_eq!(visited, ViewerResolution::ALL.to_vec());
        assert_eq!(factor, ViewerResolution::ALL[0]);
    }

    #[test]
    fn lowering_walks_one_step_down_and_stops_at_quarter() {
        assert_eq!(ViewerResolution::Full.lowered(), ViewerResolution::Half);
        assert_eq!(ViewerResolution::Half.lowered(), ViewerResolution::Quarter);
        // Saturation, not wrapping: `cycled()` is the wrapping one, and
        // reusing it here would make a mid-gesture `Quarter` jump to `Full`,
        // i.e. make the preview four times *more* expensive exactly while the
        // user is dragging.
        assert_eq!(
            ViewerResolution::Quarter.lowered(),
            ViewerResolution::Quarter
        );
        // One step, never two: a `Full` selection must not land on `Quarter`.
        assert_ne!(ViewerResolution::Full.lowered(), ViewerResolution::Quarter);
    }

    #[test]
    fn label_keys_are_distinct() {
        let mut keys: Vec<_> = ViewerResolution::ALL
            .iter()
            .map(|factor| factor.label_key())
            .collect();
        keys.sort_unstable();
        let total = keys.len();
        keys.dedup();
        // Two factors sharing a key makes the toolbar name the wrong one, and
        // the i18n coverage test cannot see it — both keys exist.
        assert_eq!(keys.len(), total, "two factors share a label key");
    }

    /// Every channel has a name of its own, and every mode a command exists
    /// for is reachable from that command — with `AlphaMatte` the deliberate
    /// exception, which this test states rather than assumes.
    #[test]
    fn display_channel_keys_are_distinct_and_commands_reach_every_mode_but_the_matte() {
        let mut keys: Vec<_> = DisplayChannel::ALL
            .iter()
            .copied()
            .map(display_channel_label_key)
            .collect();
        keys.sort_unstable();
        let total = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), total, "two channels share a label key");

        let reachable: Vec<DisplayChannel> = CommandId::all()
            .filter_map(display_channel_from_command)
            .collect();
        for channel in DisplayChannel::ALL {
            assert_eq!(
                reachable.contains(&channel),
                channel != DisplayChannel::AlphaMatte,
                "{channel:?} is reachable from a command when it should not be, \
                 or unreachable when it should be",
            );
        }
        // One command per mode: a second command mapped to the same channel
        // would leave another one dead while every assertion above passed.
        assert_eq!(reachable.len(), DisplayChannel::ALL.len() - 1);
    }

    #[test]
    fn full_evaluates_at_composition_resolution() {
        assert_eq!(ViewerResolution::Full.apply((1920, 1080)), (1920, 1080));
        assert_eq!(ViewerResolution::Full.apply((3840, 2160)), (3840, 2160));
        assert_eq!(ViewerResolution::Full.apply((1, 1)), (1, 1));
    }

    #[test]
    fn half_and_quarter_divide_each_axis() {
        assert_eq!(ViewerResolution::Half.apply((1920, 1080)), (960, 540));
        assert_eq!(ViewerResolution::Quarter.apply((1920, 1080)), (480, 270));
        // Portrait comps scale the same way — the divisor is per axis, not
        // per long edge, so the aspect ratio is preserved.
        assert_eq!(ViewerResolution::Half.apply((1080, 1920)), (540, 960));
        assert_eq!(ViewerResolution::Quarter.apply((3840, 2160)), (960, 540));
    }

    #[test]
    fn odd_resolutions_round_up() {
        // 1921/2 = 960.5 and 1081/2 = 540.5: rounding up keeps the preview
        // from being smaller than the exact fraction, and keeps the two axes
        // within a pixel of the composition's aspect ratio.
        assert_eq!(ViewerResolution::Half.apply((1921, 1081)), (961, 541));
        assert_eq!(ViewerResolution::Quarter.apply((1921, 1081)), (481, 271));
        assert_eq!(ViewerResolution::Quarter.apply((999, 333)), (250, 84));
    }

    #[test]
    fn tiny_resolutions_never_collapse_to_zero() {
        for factor in ViewerResolution::ALL {
            for size in [(1, 1), (2, 1), (1, 3), (3, 3), (4, 1)] {
                let (w, h) = factor.apply(size);
                assert!(w >= 1 && h >= 1, "{factor:?} on {size:?} gave {w}x{h}");
            }
        }
    }

    /// The edges, on a buffer the same size as the composition: the first
    /// composition pixel is buffer index 0 and the last is `width - 1`, with
    /// nothing lost or doubled in between.
    ///
    /// Sub-pixel positions inside one composition pixel all resolve to it —
    /// the pointer lands somewhere in the middle of a pixel far more often
    /// than exactly on its corner.
    #[test]
    fn the_buffer_index_does_not_slip_at_the_edges() {
        let comp = (1920, 1080);
        assert_eq!(comp_to_buffer_index((0.0, 0.0), comp, comp), Some((0, 0)));
        assert_eq!(comp_to_buffer_index((0.99, 0.5), comp, comp), Some((0, 0)));
        assert_eq!(comp_to_buffer_index((1.0, 1.0), comp, comp), Some((1, 1)));
        assert_eq!(
            comp_to_buffer_index((1919.0, 1079.0), comp, comp),
            Some((1919, 1079)),
            "the last composition pixel must be the last buffer index"
        );
        assert_eq!(
            comp_to_buffer_index((1919.999, 1079.999), comp, comp),
            Some((1919, 1079)),
            "the far corner of the last pixel is still that pixel"
        );
    }

    /// Outside the composition there is no value to report — on either axis,
    /// on either side, and at the exclusive upper bound.
    ///
    /// Clamping instead would be worse than useless: the readout would follow
    /// the pointer off the frame while reporting the edge pixel's colour.
    #[test]
    fn a_point_outside_the_composition_has_no_buffer_index() {
        let comp = (1920, 1080);
        for outside in [
            (-0.001, 500.0),
            (500.0, -0.001),
            (-1.0, -1.0),
            (1920.0, 500.0),
            (500.0, 1080.0),
            (5000.0, 5000.0),
            (f32::NAN, 500.0),
            (500.0, f32::NAN),
        ] {
            assert_eq!(
                comp_to_buffer_index(outside, comp, comp),
                None,
                "{outside:?} is outside the composition"
            );
        }
        // A degenerate frame or composition has no pixel either.
        assert_eq!(comp_to_buffer_index((0.0, 0.0), (0, 0), (4, 4)), None);
        assert_eq!(comp_to_buffer_index((0.0, 0.0), (4, 4), (0, 0)), None);
    }

    /// The whole reason the conversion exists: the viewer normally evaluates
    /// below composition resolution, so composition coordinates are **not**
    /// buffer indices.
    ///
    /// Every factor, and both edges of each — a mapping that used the
    /// composition resolution for the buffer would read past the end of the
    /// smaller frame, and one that divided instead of scaling would land on
    /// the wrong pixel everywhere but the origin.
    #[test]
    fn a_smaller_evaluation_buffer_still_names_the_right_pixel() {
        let comp = (1920, 1080);
        for factor in ViewerResolution::ALL {
            let buffer = factor.apply(comp);
            assert_eq!(
                comp_to_buffer_index((0.0, 0.0), comp, buffer),
                Some((0, 0)),
                "{factor:?}"
            );
            assert_eq!(
                comp_to_buffer_index((1919.5, 1079.5), comp, buffer),
                Some((buffer.0 - 1, buffer.1 - 1)),
                "{factor:?}: the last composition pixel must reach the last \
                 buffer pixel and no further"
            );
            // The middle of the composition is the middle of the buffer,
            // whatever the factor.
            assert_eq!(
                comp_to_buffer_index((960.0, 540.0), comp, buffer),
                Some((buffer.0 / 2, buffer.1 / 2)),
                "{factor:?}"
            );
        }
        // Two composition pixels share a buffer pixel at Half, and the second
        // one must not spill into the next index.
        let half = ViewerResolution::Half.apply(comp);
        assert_eq!(comp_to_buffer_index((2.0, 2.0), comp, half), Some((1, 1)));
        assert_eq!(comp_to_buffer_index((3.0, 3.0), comp, half), Some((1, 1)));
        assert_eq!(comp_to_buffer_index((4.0, 4.0), comp, half), Some((2, 2)));
        // An odd composition rounds its buffer up, so the extra buffer column
        // exists but nothing maps onto it — reading it would be reading
        // padding.
        let odd = ViewerResolution::Half.apply((1921, 1081));
        assert_eq!(odd, (961, 541));
        assert_eq!(
            comp_to_buffer_index((1920.5, 1080.5), (1921, 1081), odd),
            Some((960, 540))
        );
    }

    /// The two forms differ in scale only, and both name the same four
    /// channels in the same order.
    #[test]
    fn the_readout_prints_the_evaluated_value_in_both_forms() {
        let comp = (12.7, 34.2);
        let rgba = [0.5, 0.25, 0.0, 1.0];
        assert_eq!(
            pixel_readout_text(comp, rgba, PixelReadoutFormat::Float),
            "12, 34   R 0.5000  G 0.2500  B 0.0000  A 1.0000"
        );
        assert_eq!(
            pixel_readout_text(comp, rgba, PixelReadoutFormat::Byte),
            "12, 34   R 128  G 64  B 0  A 255"
        );
        // Linear light goes above 1.0 and below 0.0; the float form has to
        // show that rather than clamp it, which is the reason it is default.
        assert_eq!(
            PixelReadoutFormat::Float.channel(2.5),
            "2.5000",
            "the float form must not clamp"
        );
        assert_eq!(PixelReadoutFormat::Float.channel(-0.25), "-0.2500");
        assert_eq!(PixelReadoutFormat::Byte.channel(2.5), "255");
        assert_eq!(PixelReadoutFormat::Byte.channel(-0.25), "0");
    }

    #[test]
    fn the_readout_format_toggles_between_both_forms_and_names_them_apart() {
        assert_eq!(PixelReadoutFormat::default(), PixelReadoutFormat::Float);
        assert_eq!(
            PixelReadoutFormat::Float.toggled(),
            PixelReadoutFormat::Byte
        );
        assert_eq!(
            PixelReadoutFormat::Byte.toggled(),
            PixelReadoutFormat::Float
        );
        assert_ne!(
            PixelReadoutFormat::Float.label_key(),
            PixelReadoutFormat::Byte.label_key(),
            "the two forms share a label key"
        );
    }

    /// A source that counts how often it actually ran, so a cache hit is
    /// distinguishable from a recompute.
    struct CountingSource(Arc<AtomicUsize>);

    impl NodeProcessor for CountingSource {
        fn process(
            &self,
            _node: &Node,
            _ctx: &EvalContext,
            _inputs: &[Option<Arc<dyn NodeData>>],
            _params: &ResolvedParams,
            _scope: &mut dyn EvalScope,
        ) -> anyhow::Result<Arc<dyn NodeData>> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(Arc::new(Scalar(1.0)))
        }
    }

    /// Results evaluated under different factors must never stand in for one
    /// another: a `Quarter` result served as `Full` would show the user a
    /// coarse preview while the UI claims full resolution. The composition
    /// resolution stays fixed here — exactly as the viewer request builds it —
    /// so the only thing that moves is the factor.
    #[test]
    fn results_are_not_reused_across_factors() {
        const COMP: (u32, u32) = (1920, 1080);
        let graph = Graph::new()
            .add_node(Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR))
            .unwrap();
        let runs = Arc::new(AtomicUsize::new(0));
        let mut evaluator = Evaluator::new();
        evaluator.register(NodeId::new(1), Arc::new(CountingSource(runs.clone())));

        let ctx = |factor: ViewerResolution| {
            EvalContext::new(0, FrameRate::new(24, 1), factor.apply(COMP))
                .with_comp_resolution(COMP)
        };

        let mut expected = 0;
        for factor in ViewerResolution::ALL {
            evaluator
                .evaluate(&graph, NodeId::new(1), &ctx(factor))
                .unwrap();
            expected += 1;
            assert_eq!(
                runs.load(Ordering::Relaxed),
                expected,
                "{factor:?} reused another factor's result"
            );
            // The same factor twice in a row is a hit, so the recompute above
            // is attributable to the factor and not to a cache that never
            // stores anything.
            evaluator
                .evaluate(&graph, NodeId::new(1), &ctx(factor))
                .unwrap();
            assert_eq!(runs.load(Ordering::Relaxed), expected);
        }

        // Going back to a factor evaluated earlier recomputes too: the
        // evaluator keeps one entry per node, so the previous factor's value
        // was replaced rather than kept alongside.
        evaluator
            .evaluate(&graph, NodeId::new(1), &ctx(ViewerResolution::Full))
            .unwrap();
        assert_eq!(runs.load(Ordering::Relaxed), expected + 1);
    }

    #[test]
    fn serde_roundtrip_uses_snake_case() {
        for factor in ViewerResolution::ALL {
            let json = serde_json::to_string(&factor).unwrap();
            assert_eq!(
                serde_json::from_str::<ViewerResolution>(&json).unwrap(),
                factor
            );
        }
        assert_eq!(
            serde_json::from_str::<ViewerResolution>("\"quarter\"").unwrap(),
            ViewerResolution::Quarter
        );
    }

    /// The preview factor and the quality stage are independent axes: the
    /// factor scales the evaluation buffer, the stage decides how many
    /// samples go into it. Every pairing is a distinct cache entry —
    /// `Full` x `Preview` (inspect the framing at full size while staying
    /// responsive) and `Quarter` x `Final` (check the real sample count
    /// cheaply) both have to be reachable, so neither axis may shadow the
    /// other.
    #[test]
    fn quality_stage_and_preview_factor_do_not_shadow_each_other() {
        const COMP: (u32, u32) = (1920, 1080);
        let graph = Graph::new()
            .add_node(Node::new(NodeId::new(1), "test").with_output("out", DataTypeId::SCALAR))
            .unwrap();
        let runs = Arc::new(AtomicUsize::new(0));
        let mut evaluator = Evaluator::new();
        evaluator.register(NodeId::new(1), Arc::new(CountingSource(runs.clone())));

        let ctx = |factor: ViewerResolution, quality| {
            EvalContext::new(0, FrameRate::new(24, 1), factor.apply(COMP))
                .with_comp_resolution(COMP)
                .with_quality(quality)
        };

        let mut expected = 0;
        for factor in ViewerResolution::ALL {
            for quality in [Quality::Preview, Quality::Final] {
                evaluator
                    .evaluate(&graph, NodeId::new(1), &ctx(factor, quality))
                    .unwrap();
                expected += 1;
                assert_eq!(
                    runs.load(Ordering::Relaxed),
                    expected,
                    "{factor:?} x {quality:?} reused another pairing's result"
                );
                // The same pairing twice is a hit, so each pairing stands on
                // its own rather than never caching at all.
                evaluator
                    .evaluate(&graph, NodeId::new(1), &ctx(factor, quality))
                    .unwrap();
                assert_eq!(runs.load(Ordering::Relaxed), expected);
            }
        }
    }
}
