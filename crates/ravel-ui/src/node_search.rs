// Copyright 2026 Ravel Contributors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Query filtering and ranking for the node search palette (DISC-3).
//!
//! The palette's candidates are built host-side (labels and descriptions are
//! resolved through the locale catalogs there); this module is the pure part
//! — given the resolved strings, which candidates match a query and in what
//! order they appear. Matching is a case-insensitive substring test over the
//! label, the `type_key` and the description, so a Japanese query matches
//! Japanese text unchanged and a type name such as `shape.rect` finds its node
//! whatever the label's language is. Recently used type keys rank first, and
//! the three match tiers rank label over `type_key` over description.

use ravel_core::registry::NodeCategory;

/// One node type the search palette can offer.
///
/// `label` and `description` are display strings, already resolved through
/// the active locale by the host (`ravel-app::node_locale`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchCandidate {
    pub type_key: String,
    pub label: String,
    pub description: Option<String>,
    pub category: NodeCategory,
}

/// Returns the indices into `candidates` that pass `category` and `query`,
/// best first.
///
/// Ranking, strongest signal first:
///
/// 1. where the query matched: label, then `type_key`, then description (an
///    empty query matches every label, so all candidates pass);
/// 2. recently used types (`recents`, most-recent-first) outrank unused ones,
///    keeping their recency order;
/// 3. ties break by label, then by `type_key` for a stable order.
pub fn filter_candidates(
    candidates: &[SearchCandidate],
    query: &str,
    category: Option<NodeCategory>,
    recents: &[String],
) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    let match_tier = |candidate: &SearchCandidate| {
        if candidate.label.to_lowercase().contains(&query) {
            Some(0u8)
        } else if candidate.type_key.to_lowercase().contains(&query) {
            Some(1u8)
        } else if candidate
            .description
            .as_ref()
            .is_some_and(|d| d.to_lowercase().contains(&query))
        {
            Some(2u8)
        } else {
            None
        }
    };
    let recent_rank = |type_key: &str| recents.iter().position(|key| key == type_key);

    let mut matched: Vec<(u8, Option<usize>, usize)> = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| category.is_none_or(|c| candidate.category == c))
        .filter_map(|(index, candidate)| {
            match_tier(candidate).map(|tier| (tier, recent_rank(&candidate.type_key), index))
        })
        .collect();
    matched.sort_by(|(tier_a, recent_a, index_a), (tier_b, recent_b, index_b)| {
        tier_a
            .cmp(tier_b)
            .then_with(|| {
                recent_a
                    .unwrap_or(usize::MAX)
                    .cmp(&recent_b.unwrap_or(usize::MAX))
            })
            .then_with(|| {
                candidates[*index_a]
                    .label
                    .to_lowercase()
                    .cmp(&candidates[*index_b].label.to_lowercase())
            })
            .then_with(|| {
                candidates[*index_a]
                    .type_key
                    .cmp(&candidates[*index_b].type_key)
            })
    });
    matched.into_iter().map(|(_, _, index)| index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(
        type_key: &str,
        label: &str,
        description: Option<&str>,
        category: NodeCategory,
    ) -> SearchCandidate {
        SearchCandidate {
            type_key: type_key.into(),
            label: label.into(),
            description: description.map(str::to_string),
            category,
        }
    }

    fn keys<'a>(candidates: &'a [SearchCandidate], indices: &[usize]) -> Vec<&'a str> {
        indices
            .iter()
            .map(|index| candidates[*index].type_key.as_str())
            .collect()
    }

    #[test]
    fn a_japanese_query_matches_japanese_labels_and_descriptions() {
        let candidates = vec![
            candidate(
                "field.noise",
                "ノイズフィールド",
                Some("座標に対して値を返す場を生成する"),
                NodeCategory::Field,
            ),
            candidate("blur", "ブラー", Some("画像をぼかす"), NodeCategory::Image),
        ];

        assert_eq!(filter_candidates(&candidates, "ノイズ", None, &[]), vec![0]);
        // A term that appears only in the description still matches.
        assert_eq!(filter_candidates(&candidates, "ぼかす", None, &[]), vec![1]);
        assert!(filter_candidates(&candidates, "グロー", None, &[]).is_empty());
    }

    #[test]
    fn matching_is_case_insensitive() {
        let candidates = vec![candidate(
            "blur",
            "Blur",
            Some("Soft edge"),
            NodeCategory::Image,
        )];
        assert_eq!(filter_candidates(&candidates, "BLUR", None, &[]), vec![0]);
        assert_eq!(filter_candidates(&candidates, "soft", None, &[]), vec![0]);
    }

    #[test]
    fn a_label_match_outranks_a_description_only_match() {
        let candidates = vec![
            candidate("a", "Alpha", Some("contains blur"), NodeCategory::Image),
            candidate("b", "Blur", None, NodeCategory::Image),
        ];
        assert_eq!(
            keys(
                &candidates,
                &filter_candidates(&candidates, "blur", None, &[])
            ),
            vec!["b", "a"]
        );
    }

    #[test]
    fn a_type_key_query_matches_whatever_language_the_label_is_in() {
        // The label never contains "shape.rect", so only the type_key tier can
        // find it — in a Japanese UI as much as in an English one.
        for label in ["矩形", "Rectangle"] {
            let candidates = vec![
                candidate("shape.rect", label, None, NodeCategory::Geometry),
                candidate("blur", "Blur", None, NodeCategory::Image),
            ];
            assert_eq!(
                keys(
                    &candidates,
                    &filter_candidates(&candidates, "shape.rect", None, &[])
                ),
                vec!["shape.rect"]
            );
        }
    }

    #[test]
    fn a_label_match_outranks_a_type_key_match_which_outranks_a_description() {
        let candidates = vec![
            candidate("a.rect", "Alpha", None, NodeCategory::Geometry),
            candidate("b", "Beta", Some("draws a rect"), NodeCategory::Geometry),
            candidate("c", "Rect", None, NodeCategory::Geometry),
        ];
        assert_eq!(
            keys(
                &candidates,
                &filter_candidates(&candidates, "rect", None, &[])
            ),
            vec!["c", "a.rect", "b"]
        );
    }

    #[test]
    fn the_category_filter_excludes_other_categories() {
        let candidates = vec![
            candidate("blur", "Blur", None, NodeCategory::Image),
            candidate("field.noise", "Noise", None, NodeCategory::Field),
        ];
        assert_eq!(
            filter_candidates(&candidates, "", Some(NodeCategory::Field), &[]),
            vec![1]
        );
    }

    #[test]
    fn recently_used_types_rank_first() {
        let candidates = vec![
            candidate("blur", "Blur", None, NodeCategory::Image),
            candidate("merge", "Merge", None, NodeCategory::Image),
            candidate("rasterize", "Rasterize", None, NodeCategory::Image),
        ];
        let recents = vec!["rasterize".to_string(), "blur".to_string()];

        // Empty query: recency order first, the rest alphabetical.
        assert_eq!(
            keys(
                &candidates,
                &filter_candidates(&candidates, "", None, &recents)
            ),
            vec!["rasterize", "blur", "merge"]
        );
        // With a query, recents still break ties inside the same match tier
        // ("blur" has no "e" and drops out).
        assert_eq!(
            keys(
                &candidates,
                &filter_candidates(&candidates, "e", None, &recents)
            ),
            vec!["rasterize", "merge"]
        );
    }

    #[test]
    fn an_empty_query_keeps_everything_ranked_by_label() {
        let candidates = vec![
            candidate("b", "Beta", None, NodeCategory::Image),
            candidate("a", "Alpha", None, NodeCategory::Image),
        ];
        assert_eq!(
            keys(
                &candidates,
                &filter_candidates(&candidates, "  ", None, &[])
            ),
            vec!["a", "b"]
        );
    }
}
