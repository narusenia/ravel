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
