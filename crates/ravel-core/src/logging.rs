// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Structured logging via `tracing`.
//!
//! Log level is controlled by the `RAVEL_LOG` environment variable using
//! `tracing_subscriber`'s `EnvFilter` syntax (e.g. `RAVEL_LOG=info`,
//! `RAVEL_LOG=ravel_core=debug,warn`). Falls back to `info` when unset.
//!
//! In release builds, logs are also written to rotating files under the
//! application log directory.
//!
//! Console output goes to **stderr**, never stdout: `ravel-cli` puts its
//! machine-readable output on stdout, and a consumer of that stream must
//! never have to filter diagnostics out of it.

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::Directive;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

/// Targets whose diagnostics are pinned quieter than whatever the filter asks
/// for, because they report a condition Ravel is not in a position to act on
/// and repeat it often enough to bury the rest of the log.
///
/// Applied on top of the caller's directives rather than folded into the
/// default, so `RAVEL_LOG=debug` stays usable: raising the level to chase a
/// Ravel bug must not also turn one of these back on.
const PINNED_QUIET: &[&str] = &[
    // gpui emits this per accessibility tree update because Zed — whose
    // widgets gpui was extracted from — exposes no accessible elements yet.
    // It says nothing about Ravel's own tree and fires while the window is
    // merely open.
    "gpui::window::a11y=error",
];

/// Guard that must be held alive for the lifetime of the application to keep
/// the non-blocking file writer flushing. Drop it to flush and close the log
/// file.
pub struct LogGuard {
    _file_guard: Option<WorkerGuard>,
}

/// Applies [`PINNED_QUIET`] on top of `base`.
///
/// Separate from [`init_logging`] because that function installs a *global*
/// subscriber, which a test can neither install twice nor observe. The
/// suppression is the whole point of the list, so it has to be assertable on
/// its own.
fn pin_quiet_targets(base: EnvFilter) -> EnvFilter {
    let mut filter = base;
    for directive in PINNED_QUIET {
        // Parsed from a literal the test below pins, so a typo is a test
        // failure rather than a silently ignored directive.
        filter = filter.add_directive(
            directive
                .parse::<Directive>()
                .expect("a pinned-quiet directive is a literal"),
        );
    }
    filter
}

/// Initialize the global tracing subscriber.
///
/// * `env_key` — environment variable name for the filter directive
///   (e.g. `"RAVEL_LOG"`).
/// * `log_dir` — if `Some`, log files are written to this directory with
///   daily rotation. Pass `None` to skip file logging (e.g. in tests).
///
/// Returns `Ok(LogGuard)` on success. Returns `Err` if a global subscriber
/// was already installed (safe to ignore in tests).
pub fn init_logging(
    env_key: &str,
    log_dir: Option<&std::path::Path>,
) -> Result<LogGuard, anyhow::Error> {
    let env_filter = pin_quiet_targets(
        EnvFilter::try_from_env(env_key).unwrap_or_else(|_| EnvFilter::new("info")),
    );

    // `with_writer` is not optional here: `fmt::layer()` defaults to
    // **stdout**, and a diagnostic on stdout is indistinguishable from
    // output. `ravel-cli` writes its machine-readable progress and its
    // listings there, so a log line landing in the same stream turns valid
    // JSON into something no consumer can parse.
    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_thread_names(true)
        .compact();

    let (file_layer, file_guard) = if let Some(dir) = log_dir {
        let file_appender = tracing_appender::rolling::daily(dir, "ravel.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        let layer = fmt::layer()
            .with_writer(non_blocking)
            .with_ansi(false)
            .json();
        (Some(layer), Some(guard))
    } else {
        (None, None)
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stderr_layer)
        .with(file_layer)
        .try_init()?;

    Ok(LogGuard {
        _file_guard: file_guard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tracing_log::NormalizeEvent as _;

    /// The a11y target the pinned list silences.
    const A11Y_TARGET: &str = "gpui::window::a11y";
    /// A target of Ravel's own, which the suppression must not touch.
    const RAVEL_TARGET: &str = "ravel_core::logging";

    /// How many events reached a layer, **per target**.
    ///
    /// Per target rather than a total for two reasons. One is correctness: a
    /// total of 1 also passes when the a11y warning survives and Ravel's own
    /// is dropped — the exact inversion of what the suppression is for. The
    /// other is isolation: one of these tests installs a **global**
    /// subscriber, and the harness runs the crate's tests as threads of one
    /// process, so a total would also count whatever unrelated tests emit.
    #[derive(Clone, Default)]
    struct TargetCounts {
        a11y: Arc<AtomicUsize>,
        ravel: Arc<AtomicUsize>,
    }

    impl TargetCounts {
        fn get(&self) -> (usize, usize) {
            (
                self.a11y.load(Ordering::Relaxed),
                self.ravel.load(Ordering::Relaxed),
            )
        }
    }

    struct CountEvents(TargetCounts);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CountEvents {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            // `tracing-log` dispatches every bridged record under one static
            // metadata whose target is `"log"`; the record's own target is
            // what `normalized_metadata` gives back. Reading it here is what
            // lets the bridged test recognise its own events instead of
            // counting every bridged record in the process.
            let normalized = event.normalized_metadata();
            let target = normalized
                .as_ref()
                .map_or_else(|| event.metadata().target(), |meta| meta.target());
            match target {
                A11Y_TARGET => self.0.a11y.fetch_add(1, Ordering::Relaxed),
                RAVEL_TARGET => self.0.ravel.fetch_add(1, Ordering::Relaxed),
                _ => 0,
            };
        }
    }

    /// Emits one a11y warning and one ordinary warning under `directives`,
    /// returning how many of each reached a layer.
    fn events_passing(directives: &str) -> (usize, usize) {
        let counts = TargetCounts::default();
        let subscriber = tracing_subscriber::registry()
            .with(pin_quiet_targets(EnvFilter::new(directives)))
            .with(CountEvents(counts.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                target: "gpui::window::a11y",
                "expected an empty a11y tree update (only the root node), but got 47 nodes"
            );
            tracing::warn!(target: "ravel_core::logging", "something Ravel can act on");
        });
        counts.get()
    }

    /// `init_logging` unwraps these, so a malformed one would panic at startup
    /// — in a release build, on a user's machine, before any window exists.
    #[test]
    fn every_pinned_quiet_directive_parses() {
        for directive in PINNED_QUIET {
            assert!(
                directive.parse::<Directive>().is_ok(),
                "{directive:?} is not a filter directive"
            );
        }
    }

    /// The warning the pinned list exists to silence is gone at the default
    /// level, and Ravel's own warnings are not collateral.
    #[test]
    fn the_a11y_warning_is_dropped_and_other_warnings_are_not() {
        assert_eq!(events_passing("info"), (0, 1));
    }

    /// Raising the level to chase a Ravel bug must not bring it back — that is
    /// the reason the directive is applied on top of the caller's rather than
    /// folded into the default.
    #[test]
    fn raising_the_level_does_not_unmute_the_a11y_warning() {
        for directives in ["debug", "trace", "ravel_core=trace,warn"] {
            assert_eq!(events_passing(directives), (0, 1), "{directives}");
        }
    }

    /// The real path, which the two tests above do **not** exercise: gpui emits
    /// this warning with `log::warn!`, not `tracing::warn!`
    /// (`gpui/src/window/a11y.rs`). It reaches the filter at all only because
    /// `SubscriberInitExt::try_init` installs a `LogTracer` when the
    /// `tracing-log` feature is on — which it is, by default, through
    /// `tracing-subscriber`. That is an implicit dependency of the suppression
    /// on a default feature of a transitive crate, so it is pinned here: if the
    /// bridge ever goes away, the directive silently stops doing anything and
    /// only this test notices.
    ///
    /// Global rather than scoped because `log` has no thread-local default —
    /// `LogTracer` routes into whichever subscriber is globally installed. This
    /// is the only test in the crate that sets one.
    #[test]
    fn the_a11y_warning_is_dropped_when_it_arrives_through_the_log_facade() {
        let counts = TargetCounts::default();
        let subscriber = tracing_subscriber::registry()
            .with(pin_quiet_targets(EnvFilter::new("info")))
            .with(CountEvents(counts.clone()));
        tracing::subscriber::set_global_default(subscriber)
            .expect("no other test installs a global subscriber");
        tracing_log::LogTracer::init().expect("the log bridge installs once");

        log::warn!(
            target: "gpui::window::a11y",
            "expected an empty a11y tree update (only the root node), but got 47 nodes"
        );
        log::warn!(target: "ravel_core::logging", "something Ravel can act on");

        assert_eq!(
            counts.get(),
            (0, 1),
            "the a11y warning must be dropped and the ordinary one kept"
        );
    }
}
