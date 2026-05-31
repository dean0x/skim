//! IDF computation for AST bigrams and trigrams.
//!
//! Reuses the same smoothed IDF formula as the lexical bigram module:
//! `idf = ln(N / (df + 1)) + 1.0`

use std::collections::HashMap;

use crate::ast_types::{
    AstBigram, AstBigramWeight, AstTrigram, AstTrigramWeight, NodeKindVocabulary,
    decode_ast_bigram, decode_ast_trigram,
};
use crate::idf::compute_idf;

/// Compute IDF weights for all bigrams in `df_map`, filtered by `threshold`.
///
/// Uses the same `compute_idf(df, total_docs)` formula as the lexical module
/// (`ln(N / (df + 1)) + 1.0`). Results are sorted by IDF descending (most
/// discriminating bigrams first).
///
/// Returns an empty vec when `total_docs == 0`.
#[must_use]
pub fn compute_ast_bigram_weights(
    df_map: &HashMap<AstBigram, u32>,
    total_docs: u32,
    threshold: f32,
    vocab: &NodeKindVocabulary,
) -> Vec<AstBigramWeight> {
    if total_docs == 0 {
        return Vec::new();
    }

    let mut weights: Vec<AstBigramWeight> = df_map
        .iter()
        .filter_map(|(&bigram, &df)| {
            let idf = compute_idf(df, total_docs);
            if idf < threshold {
                return None;
            }

            let (parent_id, child_id) = decode_ast_bigram(bigram);
            let parent_kind = vocab.resolve(parent_id)?.to_string();
            let child_kind = vocab.resolve(child_id)?.to_string();

            Some(AstBigramWeight {
                parent_kind,
                child_kind,
                bigram,
                idf,
            })
        })
        .collect();

    // Sort by IDF descending — most discriminating first.
    weights.sort_by(|a, b| {
        b.idf
            .partial_cmp(&a.idf)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    weights
}

/// Compute IDF weights for all trigrams in `df_map`, filtered by `threshold`.
///
/// Same semantics as [`compute_ast_bigram_weights`] but for trigrams.
///
/// Returns an empty vec when `total_docs == 0`.
#[must_use]
pub fn compute_ast_trigram_weights(
    df_map: &HashMap<AstTrigram, u32>,
    total_docs: u32,
    threshold: f32,
    vocab: &NodeKindVocabulary,
) -> Vec<AstTrigramWeight> {
    if total_docs == 0 {
        return Vec::new();
    }

    let mut weights: Vec<AstTrigramWeight> = df_map
        .iter()
        .filter_map(|(&trigram, &df)| {
            let idf = compute_idf(df, total_docs);
            if idf < threshold {
                return None;
            }

            let (gp_id, parent_id, child_id) = decode_ast_trigram(trigram);
            let grandparent_kind = vocab.resolve(gp_id)?.to_string();
            let parent_kind = vocab.resolve(parent_id)?.to_string();
            let child_kind = vocab.resolve(child_id)?.to_string();

            Some(AstTrigramWeight {
                grandparent_kind,
                parent_kind,
                child_kind,
                trigram,
                idf,
            })
        })
        .collect();

    weights.sort_by(|a, b| {
        b.idf
            .partial_cmp(&a.idf)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    weights
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::ast_types::{NodeKindVocabulary, encode_ast_bigram, encode_ast_trigram};

    /// Build a small vocabulary for testing.
    fn make_vocab() -> (NodeKindVocabulary, u16, u16, u16) {
        let mut vocab = NodeKindVocabulary::new();
        let id_a = vocab.get_or_insert("function_item");
        let id_b = vocab.get_or_insert("identifier");
        let id_c = vocab.get_or_insert("block");
        vocab.stabilize();
        // After stabilize: alphabetical → "block"=0, "function_item"=1, "identifier"=2
        let id_a2 = vocab.get("function_item").unwrap();
        let id_b2 = vocab.get("identifier").unwrap();
        let id_c2 = vocab.get("block").unwrap();
        let _ = (id_a, id_b, id_c);
        (vocab, id_a2, id_b2, id_c2)
    }

    // ── bigram weights ─────────────────────────────────────────────────────

    #[test]
    fn bigram_idf_formula_matches_lexical() {
        let (vocab, id_a, id_b, _id_c) = make_vocab();
        let bigram = encode_ast_bigram(id_a, id_b);
        let mut df_map = HashMap::new();
        df_map.insert(bigram, 1u32);

        let weights = compute_ast_bigram_weights(&df_map, 1000, 0.0, &vocab);
        assert_eq!(weights.len(), 1);

        // Compare with direct idf::compute_idf call.
        let expected = crate::idf::compute_idf(1, 1000);
        assert!(
            (weights[0].idf - expected).abs() < 1e-5,
            "IDF {:.6} should match {:.6}",
            weights[0].idf,
            expected
        );
    }

    #[test]
    fn bigram_threshold_filters_low_idf() {
        let (vocab, id_a, id_b, _id_c) = make_vocab();

        let rare_bigram = encode_ast_bigram(id_a, id_b);
        let common_bigram = encode_ast_bigram(id_b, id_a);

        let mut df_map = HashMap::new();
        df_map.insert(rare_bigram, 1u32); // high IDF
        df_map.insert(common_bigram, 1000u32); // low IDF (≈ 1.0)

        let weights = compute_ast_bigram_weights(&df_map, 1000, 5.0, &vocab);
        // Only the rare bigram should survive threshold = 5.0
        assert_eq!(weights.len(), 1);
        assert_eq!(weights[0].bigram, rare_bigram);
    }

    #[test]
    fn bigram_empty_df_map_returns_empty() {
        let (vocab, _, _, _) = make_vocab();
        let weights = compute_ast_bigram_weights(&HashMap::new(), 1000, 0.0, &vocab);
        assert!(weights.is_empty());
    }

    #[test]
    fn bigram_zero_total_docs_returns_empty() {
        let (vocab, id_a, id_b, _) = make_vocab();
        let mut df_map = HashMap::new();
        df_map.insert(encode_ast_bigram(id_a, id_b), 1u32);
        let weights = compute_ast_bigram_weights(&df_map, 0, 0.0, &vocab);
        assert!(weights.is_empty(), "zero total_docs must return empty vec");
    }

    #[test]
    fn bigram_sorted_by_idf_descending() {
        let (vocab, id_a, id_b, id_c) = make_vocab();
        let rare = encode_ast_bigram(id_a, id_c);
        let common = encode_ast_bigram(id_b, id_c);

        let mut df_map = HashMap::new();
        df_map.insert(rare, 1u32); // high IDF
        df_map.insert(common, 500u32); // lower IDF

        let weights = compute_ast_bigram_weights(&df_map, 1000, 0.0, &vocab);
        assert!(
            weights[0].idf >= weights[1].idf,
            "weights should be sorted descending: {:.4} >= {:.4}",
            weights[0].idf,
            weights[1].idf
        );
    }

    // ── trigram weights ────────────────────────────────────────────────────

    #[test]
    fn trigram_formula_matches_lexical() {
        let (vocab, id_a, id_b, id_c) = make_vocab();
        let trigram = encode_ast_trigram(id_a, id_b, id_c);
        let mut df_map = HashMap::new();
        df_map.insert(trigram, 2u32);

        let weights = compute_ast_trigram_weights(&df_map, 500, 0.0, &vocab);
        assert_eq!(weights.len(), 1);

        let expected = crate::idf::compute_idf(2, 500);
        assert!(
            (weights[0].idf - expected).abs() < 1e-5,
            "trigram IDF {:.6} should match {:.6}",
            weights[0].idf,
            expected
        );
    }

    #[test]
    fn trigram_zero_total_docs_returns_empty() {
        let (vocab, id_a, id_b, id_c) = make_vocab();
        let mut df_map = HashMap::new();
        df_map.insert(encode_ast_trigram(id_a, id_b, id_c), 1u32);
        let weights = compute_ast_trigram_weights(&df_map, 0, 0.0, &vocab);
        assert!(weights.is_empty());
    }

    #[test]
    fn high_df_yields_low_idf() {
        let (vocab, id_a, id_b, id_c) = make_vocab();
        let universal = encode_ast_bigram(id_a, id_b);
        let rare = encode_ast_bigram(id_b, id_c);

        let n = 10_000u32;
        let mut df_map = HashMap::new();
        df_map.insert(universal, n); // df == N → IDF ≈ 1.0
        df_map.insert(rare, 1u32); // df == 1 → high IDF

        let weights = compute_ast_bigram_weights(&df_map, n, 0.0, &vocab);
        let universal_weight = weights.iter().find(|w| w.bigram == universal).unwrap().idf;
        let rare_weight = weights.iter().find(|w| w.bigram == rare).unwrap().idf;

        assert!(
            rare_weight > universal_weight,
            "rare IDF {rare_weight:.4} should exceed universal IDF {universal_weight:.4}"
        );
    }
}
