// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Handing a [`RenderPlan`] to the render worker and waiting for it.
//!
//! # Why the wait is a loop and not `shutdown()`
//!
//! [`RenderQueue::shutdown`] consumes the queue, so a thread blocked in it
//! cannot also call [`RenderQueue::cancel`] — and cancelling is exactly what
//! an interrupt has to do. The events therefore come back through a channel
//! and this polls it, which leaves the queue reachable for the whole render
//! and gives the interrupt somewhere to land. `shutdown` still runs, after
//! the terminal event, where the worker is already idle and the join is
//! immediate.
//!
//! The cancellation itself is an [`AtomicBool`] rather than a callback so
//! that the signal handler — which must be `'static` and must do almost
//! nothing — only sets a flag, and so a test can trigger the same path
//! without a signal (Windows has none to send).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossbeam_channel::RecvTimeoutError;
use ravel_core::cache_budget::SharedCacheBudget;
use ravel_core::runtime::eval_service::EvalWorkerHooks;
use ravel_core::runtime::{JobProgress, RenderEvent, RenderJob, RenderQueue};
use ravel_media::encode::ImageSequenceEncoder;

use crate::error::CliError;
use crate::plan::RenderPlan;
use crate::report::Reporter;

/// How often the wait wakes to notice an interrupt. Short enough that Ctrl-C
/// feels immediate, long enough that an idle render costs nothing.
const POLL: Duration = Duration::from_millis(100);

/// A request to stop, shared with whatever notices one.
#[derive(Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the render to stop at the next frame boundary. Idempotent, and
    /// safe to call from a signal handler.
    pub fn request(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Render `plan` and return how many frames it wrote.
///
/// `hooks` decide which processors the private evaluator gets: the GPU ones
/// in the binary, a stub in the tests that do not need a device.
///
/// `budget` is the same one the hooks were built with, so the worker's node
/// cache, the texture pool and the shared decode cache are counted against
/// one ceiling rather than three (`cache-plan.md`, `CACHE-3`).
pub fn execute<H: EvalWorkerHooks>(
    hooks: H,
    budget: SharedCacheBudget,
    plan: &RenderPlan,
    cancel: &CancelFlag,
    reporter: &mut dyn Reporter,
) -> Result<u64, CliError> {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut queue = RenderQueue::spawn_with_budget(hooks, budget, move |event| {
        // A send that fails means this process is already tearing down; the
        // worker must not panic over it.
        let _ = tx.send(event);
    });

    let job = RenderJob::new(
        plan.document.clone(),
        plan.comp,
        plan.range.clone(),
        Box::new(ImageSequenceEncoder::new(plan.output.clone())),
        plan.render_output(),
    )
    .with_overwrite(plan.overwrite);
    let id = queue.submit(job);

    let mut progress: Option<JobProgress> = None;
    let mut requested = false;
    let outcome = loop {
        if !requested && cancel.is_requested() {
            queue.cancel(id);
            requested = true;
        }
        let event = match rx.recv_timeout(POLL) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            // The worker dropped its sender without a terminal event, which
            // it only does by dying. `submit` reports the case it can see;
            // this covers the one it cannot.
            Err(RecvTimeoutError::Disconnected) => {
                break Err(CliError::Internal(
                    "the render worker stopped without reporting".to_string(),
                ));
            }
        };

        match &mut progress {
            Some(progress) => {
                progress.observe(&event);
            }
            None => progress = JobProgress::started(&event),
        }
        if let Some(progress) = &progress {
            reporter.update(progress);
        }

        match event {
            RenderEvent::Started { .. } | RenderEvent::Progress { .. } => {}
            RenderEvent::Completed { frames, .. } => break Ok(frames),
            RenderEvent::Cancelled { .. } => break Err(CliError::Cancelled),
            RenderEvent::Failed { error, .. } => break Err(error.into()),
        }
    };

    // The worker is idle by now — it has emitted the job's terminal event —
    // so this join returns at once and guarantees the files are closed
    // before the process exits.
    queue.shutdown();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancel_flag_starts_clear_and_latches() {
        let flag = CancelFlag::new();
        assert!(!flag.is_requested());
        let clone = flag.clone();
        clone.request();
        assert!(flag.is_requested(), "the flag is shared, not copied");
        clone.request();
        assert!(flag.is_requested(), "requesting twice is not a toggle");
    }
}
