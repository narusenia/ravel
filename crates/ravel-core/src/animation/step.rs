// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Step curves: an ordered set of keys whose value is *held* until the next
//! one.
//!
//! A [`KeyframeCurve`](super::curve::KeyframeCurve) interpolates between its
//! keyframes, which needs a midpoint between two values. Strings have none —
//! there is no value half way between `"Hello"` and `"World"` — so an
//! animatable string is a [`StepCurve`] instead: sampling answers with the
//! last key at or before the frame, and nothing else. That also means the
//! curve editor never sees one; `ParameterValue::channels` keeps returning
//! `None` for it (the discrete-keyframes plan, unit 2).
//!
//! `T` is generic because the mechanism is, not because more instantiations
//! are planned: `StepCurve<String>` is the only one, and a `Bool` or an enum
//! would be added the day a requirement asks for it.

/// A single step key: a value anchored at a frame, held until the next key.
///
/// There are no tangents and no interpolation mode — a step curve has one
/// shape, and a key that could interpolate would be a key whose type promises
/// a midpoint it cannot produce.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StepKey<T> {
    pub frame: u64,
    pub value: T,
}

/// An ordered set of held values sampled at arbitrary frames.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StepCurve<T> {
    /// Keys, always sorted ascending by `frame` with unique frames.
    keys: Vec<StepKey<T>>,
    /// Value returned while the curve has no keys.
    default_value: T,
}

impl<T: Default> Default for StepCurve<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> StepCurve<T> {
    /// An empty curve whose sample is `default_value` until a key is added.
    pub fn new(default_value: T) -> Self {
        Self {
            keys: Vec::new(),
            default_value,
        }
    }

    /// A curve holding `value` from `frame` onward — the shape the keyframe
    /// toggle produces when it re-types a constant parameter, which keeps the
    /// old constant as the default so removing the key restores it.
    pub fn keyed(frame: u64, value: T) -> Self
    where
        T: Clone,
    {
        let mut curve = Self::new(value.clone());
        curve.insert(frame, value);
        curve
    }

    /// Number of keys.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Whether the curve has no keys (its sample is the default value).
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Read-only view of the keys, sorted by frame.
    pub fn keys(&self) -> &[StepKey<T>] {
        &self.keys
    }

    /// The value the curve returns while it has no keys.
    pub fn default_value(&self) -> &T {
        &self.default_value
    }

    /// Whether a key sits exactly at `frame`.
    pub fn contains_key(&self, frame: u64) -> bool {
        self.index_of(frame).is_some()
    }

    fn index_of(&self, frame: u64) -> Option<usize> {
        self.keys.binary_search_by_key(&frame, |k| k.frame).ok()
    }

    /// Insert (or overwrite) the key at `frame`, keeping the curve sorted.
    pub fn insert(&mut self, frame: u64, value: T) {
        let key = StepKey { frame, value };
        match self.keys.binary_search_by_key(&frame, |k| k.frame) {
            Ok(i) => self.keys[i] = key,
            Err(i) => self.keys.insert(i, key),
        }
    }

    /// Remove the key at `frame`, returning it if it existed.
    pub fn remove(&mut self, frame: u64) -> Option<StepKey<T>> {
        self.index_of(frame).map(|i| self.keys.remove(i))
    }

    /// Move the key at `old_frame` to `new_frame`, preserving its value. An
    /// existing key at `new_frame` is overwritten. Returns `true` on success.
    pub fn move_key(&mut self, old_frame: u64, new_frame: u64) -> bool {
        if old_frame == new_frame {
            return self.contains_key(old_frame);
        }
        let Some(i) = self.index_of(old_frame) else {
            return false;
        };
        let key = self.keys.remove(i);
        self.insert(new_frame, key.value);
        true
    }

    /// Sample the curve at `frame`, which may sit between integer frames.
    ///
    /// * Empty curve → the default value.
    /// * Before the first key → the first key's value (extrapolation = hold,
    ///   the same rule [`KeyframeCurve`](super::curve::KeyframeCurve) uses).
    /// * Otherwise → the last key at or before `frame`.
    pub fn sample(&self, frame: f64) -> &T {
        // Number of keys at or before `frame`; 0 means the frame sits before
        // the first key, where the hold rule reaches backwards to it.
        let at_or_before = self.keys.partition_point(|k| (k.frame as f64) <= frame);
        match self.keys.get(at_or_before.saturating_sub(1)) {
            Some(key) => &key.value,
            None => &self.default_value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve() -> StepCurve<String> {
        let mut c = StepCurve::new("fallback".to_string());
        c.insert(10, "ten".to_string());
        c.insert(20, "twenty".to_string());
        c
    }

    /// The value switches exactly at a key's frame and is held until the next.
    #[test]
    fn a_key_holds_until_the_next_one() {
        let c = curve();
        assert_eq!(c.sample(10.0), "ten");
        assert_eq!(c.sample(15.0), "ten");
        assert_eq!(c.sample(19.9), "ten");
        assert_eq!(c.sample(20.0), "twenty");
        assert_eq!(c.sample(1000.0), "twenty");
    }

    /// Before the first key the first key's value applies, not the default:
    /// the default only describes a curve with nothing in it.
    #[test]
    fn frames_before_the_first_key_read_the_first_key() {
        let c = curve();
        assert_eq!(c.sample(0.0), "ten");
        assert_eq!(c.sample(9.9), "ten");
    }

    #[test]
    fn an_empty_curve_reads_its_default() {
        let c: StepCurve<String> = StepCurve::new("fallback".to_string());
        assert!(c.is_empty());
        assert_eq!(c.sample(0.0), "fallback");
        assert_eq!(c.sample(500.0), "fallback");
    }

    /// Inserting out of order keeps the storage sorted — sampling is a
    /// partition point over it, so an unsorted key would read as the wrong
    /// value rather than as an error.
    #[test]
    fn keys_stay_sorted_and_unique() {
        let mut c = StepCurve::new(String::new());
        for frame in [30, 10, 20, 10] {
            c.insert(frame, frame.to_string());
        }
        assert_eq!(
            c.keys().iter().map(|k| k.frame).collect::<Vec<_>>(),
            vec![10, 20, 30]
        );
        assert_eq!(c.len(), 3);
        assert_eq!(c.sample(25.0), "20");
    }

    #[test]
    fn remove_reports_what_it_removed() {
        let mut c = curve();
        assert_eq!(c.remove(20).map(|k| k.value), Some("twenty".to_string()));
        assert!(c.remove(20).is_none());
        assert_eq!(c.sample(25.0), "ten");
        assert_eq!(c.remove(10).map(|k| k.value), Some("ten".to_string()));
        assert!(c.is_empty());
        assert_eq!(c.sample(25.0), "fallback");
    }

    #[test]
    fn move_key_carries_the_value_and_overwrites_the_target() {
        let mut c = curve();
        assert!(c.move_key(10, 15));
        assert_eq!(c.sample(15.0), "ten");
        assert_eq!(c.sample(14.0), "ten", "the first key still holds backwards");
        assert!(c.move_key(15, 15), "moving onto itself is a no-op success");
        assert!(!c.move_key(11, 12), "no key at 11");
        assert!(c.move_key(15, 20), "overwrites the key at 20");
        assert_eq!(c.len(), 1);
        assert_eq!(c.sample(20.0), "ten");
    }

    /// `keyed` is what the keyframe toggle uses: the key holds the constant
    /// the parameter had, and the default keeps it so removing the key
    /// restores the same string.
    #[test]
    fn keyed_seeds_both_the_key_and_the_default() {
        let c = StepCurve::keyed(7, "seed".to_string());
        assert!(c.contains_key(7));
        assert_eq!(c.default_value(), "seed");
        assert_eq!(c.sample(0.0), "seed");
    }
}
