// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Headless state of the render queue panel (`render-export-plan.md`,
//! unit 5): one row per submitted job, folded from the worker's event stream.
//!
//! The arithmetic — "job 3, 47 of 120 frames, running" — is
//! [`JobProgress`]'s, which lives in `ravel-core` precisely so the panel and
//! `ravel-cli`'s progress line read the same numbers. What this adds is the
//! part a stream of events cannot carry: the row exists from the moment the
//! job is **submitted**, before the worker has picked it up, and it remembers
//! what the user asked for (which composition, which directory) so a finished
//! row still says what it produced.
//!
//! No user-visible text. States are locale keys and the failure message is
//! [`RenderError`](ravel_core::runtime::RenderError)'s own `Display`, a
//! diagnostic — the host turns both into sentences.

use ravel_core::runtime::{JobProgress, JobState, RenderEvent, RenderJobId};
use std::path::PathBuf;

/// One submitted job as the panel shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderQueueRow {
    job: RenderJobId,
    composition: String,
    directory: PathBuf,
    /// Frames the range covers, known at submission — so a queued row can
    /// already say how much work it is.
    total_frames: u64,
    /// `None` until [`RenderEvent::Started`] arrives; the job is queued
    /// behind the ones before it until then.
    progress: Option<JobProgress>,
}

impl RenderQueueRow {
    pub fn job(&self) -> RenderJobId {
        self.job
    }

    /// Name of the composition the job renders, as it was at submission.
    pub fn composition(&self) -> &str {
        &self.composition
    }

    /// Directory the frames go into.
    pub fn directory(&self) -> &std::path::Path {
        &self.directory
    }

    /// Frames the range covers. Taken from the worker once it reports them,
    /// so the row cannot disagree with the job that is actually running.
    pub fn total_frames(&self) -> u64 {
        self.progress
            .as_ref()
            .map_or(self.total_frames, JobProgress::total_frames)
    }

    /// Frames written so far; zero while the job is queued.
    pub fn rendered(&self) -> u64 {
        self.progress.as_ref().map_or(0, JobProgress::rendered)
    }

    /// Fraction of the range written, in `0.0..=1.0`.
    pub fn fraction(&self) -> f32 {
        self.progress.as_ref().map_or(0.0, JobProgress::fraction)
    }

    /// Whether the job has stopped, however it stopped. A queued job has not.
    pub fn is_finished(&self) -> bool {
        self.progress.as_ref().is_some_and(JobProgress::is_finished)
    }

    /// Whether the job can still be cancelled — queued or running.
    pub fn is_cancellable(&self) -> bool {
        !self.is_finished()
    }

    /// Locale key of the row's state word.
    pub fn state_key(&self) -> &'static str {
        match self.progress.as_ref().map(JobProgress::state) {
            None => "render_queue.state.queued",
            Some(JobState::Running) => "render_queue.state.running",
            Some(JobState::Completed) => "render_queue.state.completed",
            Some(JobState::Cancelled) => "render_queue.state.cancelled",
            Some(JobState::Failed { .. }) => "render_queue.state.failed",
        }
    }

    /// The diagnostic a failed job carries, for the row's detail line.
    pub fn failure(&self) -> Option<&str> {
        match self.progress.as_ref().map(JobProgress::state) {
            Some(JobState::Failed { message }) => Some(message.as_str()),
            _ => None,
        }
    }
}

/// Every job this session has submitted, newest last.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RenderQueueRows {
    rows: Vec<RenderQueueRow>,
}

impl RenderQueueRows {
    /// Record a job the queue has just accepted.
    ///
    /// Called with the id [`submit`](ravel_core::runtime::RenderQueue::submit)
    /// returned, which is why a row never has to be invented from an event:
    /// even a job the worker never sees (`WorkerGone`) has a row to attach
    /// its failure to.
    pub fn submitted(
        &mut self,
        job: RenderJobId,
        composition: impl Into<String>,
        directory: impl Into<PathBuf>,
        total_frames: u64,
    ) {
        self.rows.push(RenderQueueRow {
            job,
            composition: composition.into(),
            directory: directory.into(),
            total_frames,
            progress: None,
        });
    }

    /// Fold `event` into the row it belongs to. Returns whether a row changed
    /// — a panel that redraws on every event of every job would repaint for
    /// jobs it is not showing.
    pub fn observe(&mut self, event: &RenderEvent) -> bool {
        let Some(row) = self.rows.iter_mut().find(|row| row.job == event.job()) else {
            return false;
        };
        match &mut row.progress {
            Some(progress) => progress.observe(event),
            // `Started` is the only event that opens a tracker; anything
            // before it (there is nothing) would have no frame total to
            // count against.
            slot @ None => match JobProgress::started(event) {
                Some(progress) => {
                    *slot = Some(progress);
                    true
                }
                None => false,
            },
        }
    }

    /// The rows, oldest first.
    pub fn rows(&self) -> &[RenderQueueRow] {
        &self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Jobs that have not reached a terminal event — what a session being
    /// torn down asks the queue to cancel.
    pub fn unfinished(&self) -> Vec<RenderJobId> {
        self.rows
            .iter()
            .filter(|row| !row.is_finished())
            .map(|row| row.job)
            .collect()
    }

    /// Whether any row is still queued or running.
    pub fn has_unfinished(&self) -> bool {
        self.rows.iter().any(|row| !row.is_finished())
    }

    /// Drop the rows that have stopped, keeping the ones still working.
    /// Returns whether anything went.
    pub fn clear_finished(&mut self) -> bool {
        let before = self.rows.len();
        self.rows.retain(|row| !row.is_finished());
        self.rows.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ravel_core::runtime::RenderError;

    /// The queue numbers jobs from one; a headless test only needs the
    /// identity, which is what [`RenderJobId::from_raw`] names.
    fn job_id(raw: u64) -> RenderJobId {
        RenderJobId::from_raw(raw)
    }

    fn rows_with_one_job(total: u64) -> (RenderQueueRows, RenderJobId) {
        let mut rows = RenderQueueRows::default();
        let job = job_id(1);
        rows.submitted(job, "shot 010", "/tmp/out", total);
        (rows, job)
    }

    #[test]
    fn a_submitted_job_is_queued_until_the_worker_picks_it_up() {
        let (rows, job) = rows_with_one_job(120);
        let row = &rows.rows()[0];
        assert_eq!(row.job(), job);
        assert_eq!(row.state_key(), "render_queue.state.queued");
        assert_eq!(row.total_frames(), 120, "known before the worker says so");
        assert_eq!(row.rendered(), 0);
        assert_eq!(row.fraction(), 0.0);
        assert!(row.is_cancellable());
        assert!(rows.has_unfinished());
    }

    #[test]
    fn progress_is_folded_from_the_workers_events() {
        let (mut rows, job) = rows_with_one_job(4);
        assert!(rows.observe(&RenderEvent::Started {
            job,
            total_frames: 4
        }));
        assert_eq!(rows.rows()[0].state_key(), "render_queue.state.running");

        assert!(rows.observe(&RenderEvent::Progress {
            job,
            frame: 1,
            rendered: 2,
            total_frames: 4,
        }));
        let row = &rows.rows()[0];
        assert_eq!(row.rendered(), 2);
        assert_eq!(row.fraction(), 0.5);

        assert!(rows.observe(&RenderEvent::Completed { job, frames: 4 }));
        let row = &rows.rows()[0];
        assert_eq!(row.state_key(), "render_queue.state.completed");
        assert_eq!(row.fraction(), 1.0);
        assert!(row.is_finished());
        assert!(!row.is_cancellable());
        assert!(!rows.has_unfinished());
    }

    #[test]
    fn a_failed_job_keeps_the_diagnostic_for_its_detail_line() {
        let (mut rows, job) = rows_with_one_job(4);
        rows.observe(&RenderEvent::Started {
            job,
            total_frames: 4,
        });
        rows.observe(&RenderEvent::Failed {
            job,
            error: RenderError::EmptyRange { start: 5, end: 5 },
        });
        let row = &rows.rows()[0];
        assert_eq!(row.state_key(), "render_queue.state.failed");
        assert!(
            row.failure().is_some_and(|m| m.contains("5..5")),
            "the row carries the worker's own diagnostic",
        );
    }

    #[test]
    fn a_cancelled_job_reports_how_far_it_got() {
        let (mut rows, job) = rows_with_one_job(50);
        rows.observe(&RenderEvent::Started {
            job,
            total_frames: 50,
        });
        rows.observe(&RenderEvent::Cancelled {
            job,
            frames_rendered: 7,
        });
        let row = &rows.rows()[0];
        assert_eq!(row.state_key(), "render_queue.state.cancelled");
        assert_eq!(row.rendered(), 7);
        assert!(row.is_finished());
    }

    #[test]
    fn events_for_a_job_with_no_row_change_nothing() {
        let (mut rows, job) = rows_with_one_job(4);
        let other = job_id(9);
        assert_ne!(other, job);
        assert!(!rows.observe(&RenderEvent::Started {
            job: other,
            total_frames: 4,
        }));
        assert_eq!(rows.rows().len(), 1);
        assert_eq!(rows.rows()[0].state_key(), "render_queue.state.queued");
    }

    #[test]
    fn clearing_keeps_the_jobs_that_are_still_working() {
        let (mut rows, first) = rows_with_one_job(4);
        let second = job_id(2);
        rows.submitted(second, "shot 020", "/tmp/other", 8);
        rows.observe(&RenderEvent::Started {
            job: first,
            total_frames: 4,
        });
        rows.observe(&RenderEvent::Completed {
            job: first,
            frames: 4,
        });

        assert_eq!(rows.unfinished(), vec![second]);
        assert!(rows.clear_finished());
        assert_eq!(rows.rows().len(), 1);
        assert_eq!(rows.rows()[0].job(), second);
        assert!(!rows.clear_finished(), "nothing left to clear");
    }
}
