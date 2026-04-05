//! BM25F scoring formula with per-field boost weights.
//!
//! Implements the Okapi BM25F variant that weights term frequencies
//! per semantic field (type definitions, function signatures, etc.)
//! using the boost factors from [`SearchField::default_boost`].
//!
//! # Formula
//!
//! ```text
//! score(q, d) = Σ_t  IDF(t) × (tf_weighted / (k1 + tf_weighted))
//!
//! where:
//!   tf_weighted = Σ_f (boost_f × tf_f) / (1 + b × (dl / avgdl - 1))
//!   IDF(t)      = ln((N - df + 0.5) / (df + 0.5) + 1)
//! ```

use crate::SearchField;

use super::Bm25Params;

/// BM25F scorer with configurable parameters.
///
/// Created once per query (since `doc_count` is fixed for a given index snapshot)
/// and reused across all terms and documents.
pub struct Bm25Scorer {
    params: Bm25Params,
    doc_count: u64,
}

impl Bm25Scorer {
    /// Create a new scorer from BM25 parameters and the total number of indexed documents.
    #[must_use]
    pub fn new(params: Bm25Params, doc_count: u64) -> Self {
        Self { params, doc_count }
    }

    /// Score a single query term against one document.
    ///
    /// # Parameters
    ///
    /// - `field_tfs`: per-field term frequencies `(field, tf)` for this document.
    ///   Only fields where `tf > 0` need to be present.
    /// - `doc_len`: total token count of the document (used for length normalization).
    /// - `df`: document frequency — number of documents in the index containing this term.
    ///
    /// Returns `0.0` if `field_tfs` is empty or `df == 0`.
    pub fn score_term(&self, field_tfs: &[(SearchField, u16)], doc_len: u32, df: u64) -> f32 {
        if field_tfs.is_empty() || df == 0 {
            return 0.0;
        }

        let idf = self.idf(df);
        let tf_weighted = self.tf_weighted(field_tfs, doc_len);

        let k1 = self.params.k1;
        idf * (tf_weighted / (k1 + tf_weighted))
    }

    // -------------------------------------------------------------------------
    // Private helpers
    // -------------------------------------------------------------------------

    /// IDF component:  ln((N - df + 0.5) / (df + 0.5) + 1)
    fn idf(&self, df: u64) -> f32 {
        let n = self.doc_count as f32;
        let df = df as f32;
        // Clamp to avoid negative IDF when df > doc_count (shouldn't happen, but guards are cheap).
        let numerator = (n - df + 0.5).max(0.0);
        let denominator = df + 0.5;
        ((numerator / denominator) + 1.0).ln()
    }

    /// Weighted TF component across all fields:
    ///
    /// ```text
    /// tf_weighted = Σ_f (boost_f × tf_f) / (1 + b × (dl / avgdl - 1))
    /// ```
    ///
    /// Length normalization is applied globally (not per-field) per classic BM25F.
    fn tf_weighted(&self, field_tfs: &[(SearchField, u16)], doc_len: u32) -> f32 {
        let b = self.params.b;
        let avg_doc_len = self.params.avg_doc_len;

        // Length normalization factor.
        // Guard: if avg_doc_len is 0, treat dl/avgdl as 1.0 → factor = 1.0.
        let len_norm = if avg_doc_len > 0.0 {
            1.0 + b * (doc_len as f32 / avg_doc_len - 1.0)
        } else {
            1.0
        };

        // Guard: prevent division by zero (can only happen if b=1, doc_len=0, avg=0).
        if len_norm <= 0.0 {
            return 0.0;
        }

        let boosted_tf: f32 = field_tfs
            .iter()
            .map(|(field, tf)| field.default_boost() * (*tf as f32))
            .sum();

        boosted_tf / len_norm
    }
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexical::Bm25Params;
    use crate::SearchField;

    fn default_scorer(doc_count: u64) -> Bm25Scorer {
        Bm25Scorer::new(Bm25Params::default(), doc_count)
    }

    #[test]
    fn empty_field_tfs_returns_zero() {
        let scorer = default_scorer(100);
        let score = scorer.score_term(&[], 50, 5);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn zero_df_returns_zero() {
        let scorer = default_scorer(100);
        let score = scorer.score_term(&[(SearchField::FunctionSignature, 3)], 50, 0);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn zero_doc_len_does_not_crash() {
        let scorer = default_scorer(100);
        let score = scorer.score_term(&[(SearchField::FunctionSignature, 1)], 0, 5);
        assert!(score.is_finite());
    }

    #[test]
    fn zero_avg_doc_len_does_not_crash() {
        let params = Bm25Params {
            avg_doc_len: 0.0,
            ..Bm25Params::default()
        };
        let scorer = Bm25Scorer::new(params, 100);
        let score = scorer.score_term(&[(SearchField::TypeDefinition, 2)], 100, 5);
        assert!(score.is_finite());
        assert!(score > 0.0);
    }

    #[test]
    fn high_df_gives_low_score() {
        let scorer = default_scorer(100);
        // df = N → IDF ≈ ln(0.5/100.5 + 1) ≈ very small
        let score = scorer.score_term(&[(SearchField::FunctionBody, 5)], 50, 100);
        assert!(score >= 0.0);
        assert!(score < 0.1);
    }

    #[test]
    fn low_df_gives_high_idf() {
        let scorer = default_scorer(1000);
        let score_rare = scorer.score_term(&[(SearchField::TypeDefinition, 3)], 50, 1);
        let score_common = scorer.score_term(&[(SearchField::TypeDefinition, 3)], 50, 900);
        assert!(
            score_rare > score_common,
            "rare term should score higher: rare={score_rare}, common={score_common}"
        );
    }

    #[test]
    fn type_definition_outscores_string_literal_for_same_tf() {
        // TypeDefinition boost = 5.0, StringLiteral boost = 0.5
        // Same tf=2, same doc_len, same df → TypeDefinition must win.
        let scorer = default_scorer(100);
        let score_type_def = scorer.score_term(&[(SearchField::TypeDefinition, 2)], 50, 5);
        let score_string_lit = scorer.score_term(&[(SearchField::StringLiteral, 2)], 50, 5);
        assert!(
            score_type_def > score_string_lit,
            "TypeDefinition (boost 5.0) should outscore StringLiteral (boost 0.5): {score_type_def} vs {score_string_lit}"
        );
    }

    #[test]
    fn multi_field_score_exceeds_single_field() {
        // Matching in multiple fields should score higher than one field alone.
        let scorer = default_scorer(100);
        let multi = scorer.score_term(
            &[
                (SearchField::TypeDefinition, 1),
                (SearchField::SymbolName, 1),
            ],
            50,
            5,
        );
        let single = scorer.score_term(&[(SearchField::TypeDefinition, 1)], 50, 5);
        assert!(multi > single);
    }

    #[test]
    fn score_is_non_negative() {
        let scorer = default_scorer(50);
        let score = scorer.score_term(&[(SearchField::Comment, 1)], 200, 10);
        assert!(score >= 0.0);
    }

    #[test]
    fn function_signature_outscores_function_body() {
        // FunctionSignature boost = 4.0 > FunctionBody boost = 1.0
        let scorer = default_scorer(200);
        let sig = scorer.score_term(&[(SearchField::FunctionSignature, 1)], 100, 5);
        let body = scorer.score_term(&[(SearchField::FunctionBody, 1)], 100, 5);
        assert!(
            sig > body,
            "FunctionSignature (boost 4.0) must outscore FunctionBody (boost 1.0)"
        );
    }

    #[test]
    fn score_is_always_non_negative_and_finite() {
        let scorer = default_scorer(500);
        let cases: &[(&[(SearchField, u16)], u32, u64)] = &[
            (&[(SearchField::TypeDefinition, 1)], 100, 1),
            (&[(SearchField::Comment, 10)], 5000, 499),
            (&[(SearchField::SymbolName, 0)], 50, 20),
            (&[(SearchField::ImportExport, 7)], 200, 50),
            (&[(SearchField::FunctionBody, 100)], 1, 1),
        ];
        for (field_tfs, doc_len, df) in cases {
            let score = scorer.score_term(field_tfs, *doc_len, *df);
            assert!(score.is_finite(), "score must be finite: {score}");
            assert!(score >= 0.0, "score must be non-negative: {score}");
        }
    }
}
