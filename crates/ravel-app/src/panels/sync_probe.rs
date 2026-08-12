// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Call counters for the document-mirroring panel sync functions.
//!
//! GPUI coalesces the `cx.notify()` calls of one effect cycle, so counting
//! observer invocations cannot tell whether a panel rebuilt once or five
//! times — the notifications merge before the repaint. What costs is the sync
//! function itself (a `Composition` deep compare, a full row walk, a section
//! rebuild), so that is what is counted here: one increment at the top of each
//! function, read back by tests that drive a gesture and assert the total.
//!
//! Only in debug builds. [`record`] compiles to nothing with
//! `debug_assertions` off, so a release binary pays neither the counter nor
//! the thread-local lookup. [`count`] and [`reset`] exist only in debug, which
//! makes a test that reads them a compile error in release rather than a test
//! that silently asserts zero — gate such tests with `#[cfg(debug_assertions)]`.
//!
//! The counters are thread-local, not global atomics: panels sync on the UI
//! thread, which under `#[gpui::test]` is the test's own thread, so two tests
//! running in parallel cannot see each other's counts and no test has to
//! serialize against the rest of the suite.

/// A document-mirroring sync function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelSync {
    /// `PropertiesGpuiPanel::refresh_values`.
    PropertiesRefresh,
    /// `TimelineGpuiPanel::sync_from_project`.
    TimelineSync,
    /// `OutlinerGpuiPanel::rebuild_rows`.
    OutlinerRows,
    /// `MediaBinGpuiPanel::rebuild_rows`.
    MediaBinRows,
}

#[cfg(debug_assertions)]
const SYNC_KINDS: usize = 4;

#[cfg(debug_assertions)]
impl PanelSync {
    fn index(self) -> usize {
        match self {
            Self::PropertiesRefresh => 0,
            Self::TimelineSync => 1,
            Self::OutlinerRows => 2,
            Self::MediaBinRows => 3,
        }
    }
}

#[cfg(debug_assertions)]
thread_local! {
    static COUNTS: std::cell::Cell<[u64; SYNC_KINDS]> =
        const { std::cell::Cell::new([0; SYNC_KINDS]) };
}

/// Count one execution of `which`.
#[cfg(debug_assertions)]
pub fn record(which: PanelSync) {
    COUNTS.with(|counts| {
        let mut values = counts.get();
        values[which.index()] += 1;
        counts.set(values);
    });
}

/// Count one execution of `which`. Compiled out in release builds.
#[cfg(not(debug_assertions))]
#[inline(always)]
pub fn record(_which: PanelSync) {}

/// Executions of `which` recorded on this thread since the last [`reset`].
#[cfg(debug_assertions)]
pub fn count(which: PanelSync) -> u64 {
    COUNTS.with(|counts| counts.get()[which.index()])
}

/// Forget every count recorded on this thread.
#[cfg(debug_assertions)]
pub fn reset() {
    COUNTS.with(|counts| counts.set([0; SYNC_KINDS]));
}

#[cfg(all(test, debug_assertions))]
mod tests {
    use super::*;

    #[test]
    fn counts_are_per_kind_and_reset() {
        reset();
        record(PanelSync::TimelineSync);
        record(PanelSync::TimelineSync);
        record(PanelSync::OutlinerRows);
        assert_eq!(count(PanelSync::TimelineSync), 2);
        assert_eq!(count(PanelSync::OutlinerRows), 1);
        assert_eq!(count(PanelSync::PropertiesRefresh), 0);
        assert_eq!(count(PanelSync::MediaBinRows), 0);
        reset();
        assert_eq!(count(PanelSync::TimelineSync), 0);
    }
}
