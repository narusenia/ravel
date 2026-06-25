// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Type-safe newtype identifiers for tracks and clips.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

static TRACK_ID_COUNTER: AtomicU64 = AtomicU64::new(1);
static CLIP_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TrackId(u64);

impl TrackId {
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub fn next() -> Self {
        Self(TRACK_ID_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    pub fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for TrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "TrackId({})", self.0)
    }
}

impl fmt::Display for TrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "track:{}", self.0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ClipId(u64);

impl ClipId {
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }

    pub fn next() -> Self {
        Self(CLIP_ID_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    pub fn raw(self) -> u64 {
        self.0
    }
}

impl fmt::Debug for ClipId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClipId({})", self.0)
    }
}

impl fmt::Display for ClipId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "clip:{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_and_clip_ids_are_distinct_types() {
        let t = TrackId::new(1);
        let c = ClipId::new(1);
        assert_eq!(t.raw(), c.raw());
    }

    #[test]
    fn next_ids_are_monotonic() {
        let a = TrackId::next();
        let b = TrackId::next();
        assert!(b.raw() > a.raw());

        let ca = ClipId::next();
        let cb = ClipId::next();
        assert!(cb.raw() > ca.raw());
    }

    #[test]
    fn display_formatting() {
        assert_eq!(format!("{}", TrackId::new(42)), "track:42");
        assert_eq!(format!("{}", ClipId::new(7)), "clip:7");
    }

    #[test]
    fn serde_roundtrip() {
        let t = TrackId::new(99);
        let s = ron::to_string(&t).unwrap();
        let back: TrackId = ron::from_str(&s).unwrap();
        assert_eq!(t, back);

        let c = ClipId::new(55);
        let s = ron::to_string(&c).unwrap();
        let back: ClipId = ron::from_str(&s).unwrap();
        assert_eq!(c, back);
    }
}
