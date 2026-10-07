// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Build a group (a `Bool` attribute) from element indices.
//!
//! The group convention flags elements with a `Bool` column, but nothing made
//! one that is not "every element". This writes it from a range expression:
//! `"3"`, `"3-7"` (inclusive), `"3,5,9"`, and a stride `"0-20:2"`.
//!
//! Anything the text cannot select — a token that does not parse, an index past
//! the domain's end — warns and is ignored rather than failing the evaluation,
//! the same rule an unusable `group` name follows.

use super::{AttributeArray, Domain, Geometry, GeometryError};

/// One `start[-end][:step]` token. `end` is inclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexRange {
    pub start: usize,
    pub end: usize,
    pub step: usize,
}

/// Parses a comma-separated range expression. Tokens that do not parse
/// (including a descending range and a zero stride) warn and are skipped.
pub fn parse_index_ranges(text: &str) -> Vec<IndexRange> {
    text.split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .filter_map(|token| {
            let parsed = parse_token(token);
            if parsed.is_none() {
                tracing::warn!(token, "index range token is not valid; ignoring it");
            }
            parsed
        })
        .collect()
}

fn parse_token(token: &str) -> Option<IndexRange> {
    let (span, step) = match token.split_once(':') {
        Some((span, step)) => (span, step.trim().parse::<usize>().ok()?),
        None => (token, 1),
    };
    let (start, end) = match span.split_once('-') {
        Some((start, end)) => (
            start.trim().parse::<usize>().ok()?,
            end.trim().parse::<usize>().ok()?,
        ),
        None => {
            let index = span.trim().parse::<usize>().ok()?;
            (index, index)
        }
    };
    (step > 0 && start <= end).then_some(IndexRange { start, end, step })
}

/// Flags for `count` elements: `true` where an in-range index is named.
/// Indices at or past `count` warn once per range and are dropped.
pub fn select_indices(ranges: &[IndexRange], count: usize) -> Vec<bool> {
    let mut flags = vec![false; count];
    for range in ranges {
        if range.end >= count {
            tracing::warn!(
                start = range.start,
                end = range.end,
                count,
                "index range reaches past the domain; ignoring the excess"
            );
        }
        let mut index = range.start;
        while index <= range.end && index < count {
            flags[index] = true;
            index += range.step;
        }
    }
    flags
}

/// Writes the `Bool` column `name` on `domain`, flagging the elements `range`
/// names (or, with `invert`, every other one).
pub fn group_index(
    geometry: &Geometry,
    domain: Domain,
    range: &str,
    name: &str,
    invert: bool,
) -> Result<Geometry, GeometryError> {
    let count = match domain {
        Domain::Point => geometry.point_count(),
        Domain::Primitive => geometry.primitive_count(),
        Domain::Instance => geometry.instance_count(),
        Domain::Detail => 1,
    };
    let mut flags = select_indices(&parse_index_ranges(range), count);
    if invert {
        flags.iter_mut().for_each(|flag| *flag = !*flag);
    }
    let mut result = geometry.clone();
    result
        .attribute_set_mut(domain)
        .insert(name, AttributeArray::Bool(flags))?;
    result.validate()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Vec2;

    fn selected(text: &str, count: usize) -> Vec<usize> {
        select_indices(&parse_index_ranges(text), count)
            .iter()
            .enumerate()
            .filter_map(|(i, on)| on.then_some(i))
            .collect()
    }

    #[test]
    fn each_notation_parses() {
        let r = |start, end, step| IndexRange { start, end, step };
        assert_eq!(parse_index_ranges("3"), [r(3, 3, 1)]);
        assert_eq!(parse_index_ranges("3-7"), [r(3, 7, 1)]);
        assert_eq!(
            parse_index_ranges("3,5,9"),
            [r(3, 3, 1), r(5, 5, 1), r(9, 9, 1)]
        );
        assert_eq!(parse_index_ranges("0-20:2"), [r(0, 20, 2)]);
        assert_eq!(
            parse_index_ranges(" 1 , 4 - 6 : 2 "),
            [r(1, 1, 1), r(4, 6, 2)]
        );
        assert_eq!(parse_index_ranges(""), []);
    }

    #[test]
    fn each_notation_selects() {
        assert_eq!(selected("3", 10), [3]);
        assert_eq!(selected("3-7", 10), [3, 4, 5, 6, 7]);
        assert_eq!(selected("3,5,9", 10), [3, 5, 9]);
        assert_eq!(selected("0-20:2", 10), [0, 2, 4, 6, 8]);
        assert_eq!(selected("1-9:4", 10), [1, 5, 9]);
        assert_eq!(selected("0,0-2", 10), [0, 1, 2], "overlap is a union");
    }

    #[test]
    fn malformed_tokens_are_ignored_not_fatal() {
        assert_eq!(parse_index_ranges("x"), []);
        assert_eq!(parse_index_ranges("7-3"), [], "descending");
        assert_eq!(parse_index_ranges("0-9:0"), [], "zero stride");
        assert_eq!(parse_index_ranges("-3"), [], "negative");
        assert_eq!(selected("2,bogus,4", 10), [2, 4]);
    }

    #[test]
    fn out_of_range_indices_are_ignored() {
        assert_eq!(selected("8,50", 10), [8]);
        assert_eq!(selected("3-100", 5), [3, 4]);
        assert_eq!(selected("0", 0), Vec::<usize>::new());
    }

    #[test]
    fn group_index_writes_a_bool_column_and_inverts() {
        let geometry = Geometry::from_points(vec![Vec2(0.0, 0.0); 5]);
        let column = |g: &Geometry| match g.points().get("sel").unwrap().as_ref() {
            AttributeArray::Bool(v) => v.clone(),
            other => panic!("not Bool: {other:?}"),
        };
        let on = group_index(&geometry, Domain::Point, "1,3", "sel", false).unwrap();
        assert_eq!(column(&on), [false, true, false, true, false]);
        let off = group_index(&geometry, Domain::Point, "1,3", "sel", true).unwrap();
        assert_eq!(column(&off), [true, false, true, false, true]);
        // Out of range: still a full-length column, nothing flagged.
        let none = group_index(&geometry, Domain::Point, "99", "sel", false).unwrap();
        assert_eq!(column(&none), [false; 5]);
    }
}
