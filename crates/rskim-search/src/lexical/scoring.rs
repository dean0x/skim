//! BM25F scoring and dominant-field computation.
//!
//! # Formula
//!
//! ```text
//! score(q,d) = Σ_t  IDF(t) * tf_weighted(t,d) / (tf_weighted(t,d) + k1)
//!
//! where tf_weighted(t,d) = Σ_f  boost_f * tf(t,d,f) / (1 + b_f * (dl_f/avdl_f - 1))
//! ```
//!
//! All intermediate arithmetic is performed in `f64` to avoid precision loss
//! when accumulating across many fields.

use crate::SearchField;

use super::config::{BM25FConfig, FIELD_COUNT};

/// Compute the BM25F score contribution for a single query term.
///
/// # Arguments
///
/// * `idf` — inverse document frequency for this term (from the bigram weight table).
/// * `field_tfs` — per-field raw term frequencies for this document and term.
/// * `field_lengths` — per-field byte lengths for this document.
/// * `avg_field_lengths` — average per-field byte lengths across the corpus.
/// * `config` — BM25F scoring parameters.
///
/// # Returns
///
/// The BM25F score contribution as `f64`.  Returns `0.0` when `tf_weighted` is
/// zero (term not present in this document) to avoid division by zero.
#[must_use]
pub fn bm25f_score(
    idf: f64,
    field_tfs: &[f32; FIELD_COUNT],
    field_lengths: &[u32; FIELD_COUNT],
    avg_field_lengths: &[f32; FIELD_COUNT],
    config: &BM25FConfig,
) -> f64 {
    let k1 = f64::from(config.k1);

    // Compute the normalised, boosted TF sum over all fields.
    let mut tf_weighted: f64 = 0.0;

    for i in 0..FIELD_COUNT {
        let boost = f64::from(config.field_boosts[i]);
        if boost == 0.0 {
            // Field is disabled — skip to avoid unnecessary work.
            continue;
        }

        let tf = f64::from(field_tfs[i]);
        if tf == 0.0 {
            // Term does not appear in this field — nothing to accumulate.
            continue;
        }

        let b = f64::from(config.field_b[i]);
        let dl = f64::from(field_lengths[i]);

        // Guard: if average field length is zero, treat normalisation ratio as 1.0
        // to avoid division by zero for fields that happen to be empty everywhere.
        let adl = if avg_field_lengths[i] > 0.0 {
            f64::from(avg_field_lengths[i])
        } else {
            1.0
        };

        // Okapi BM25 length normalisation factor for this field.
        let norm = 1.0 - b + b * (dl / adl);

        // Guard: norm can be zero when b=1.0 and dl=0 (all field bytes absent).
        // Treat as 1.0 to avoid division by zero producing NaN/Inf.
        let norm = if norm <= 0.0 { 1.0 } else { norm };

        // Normalised TF for this field, boosted by the field weight.
        let tf_norm = tf / norm;
        tf_weighted += boost * tf_norm;
    }

    if tf_weighted == 0.0 {
        return 0.0;
    }

    // BM25F saturation formula: IDF × (tf_weighted / (tf_weighted + k1))
    idf * (tf_weighted / (tf_weighted + k1))
}

/// Per-field-saturated BM25F scorer for the exact-symbol query path.
///
/// # AD-411-3
///
/// Formula:  `score = idf × Σ_f boost_f × (tf_f / (tf_f + k1))`
///
/// Unlike [`bm25f_score`] (which saturates the *weighted sum*), this function
/// saturates each field contribution independently.  As `tf_f → ∞`, field `f`
/// asymptotes to its boost (`boost_f × 1`).  A single high-boost definition
/// occurrence therefore dominates any number of low-boost call-site occurrences:
///
/// ```text
/// 1 FnSig (boost=8): 8 × (1 / 2.2) ≈ 3.64
/// 52 FnBody (boost=1): 1 × (52 / 53.2) ≈ 0.98   → def wins
/// ```
///
/// `idf = 1.0` is the conventional choice for the exact-symbol path (single
/// term; ranking-neutral uniform scale across all documents for that term).
///
/// `field_b = 0` (set by [`BM25FConfig::for_exact_symbol`]) means `field_lengths`
/// and `avg_field_lengths` are irrelevant here; the function ignores them.
///
/// # Arguments
///
/// * `idf` — inverse document frequency; use `1.0` on the exact-symbol path.
/// * `field_tfs` — per-field whole-token occurrence counts for this document.
/// * `config` — BM25F config; must have `field_b = [0; 8]` for exact-symbol use.
#[must_use]
pub fn bm25f_per_field_saturated_score(
    idf: f64,
    field_tfs: &[f32; FIELD_COUNT],
    config: &BM25FConfig,
) -> f64 {
    let k1 = f64::from(config.k1);
    let mut score: f64 = 0.0;
    for (i, &tf_val) in field_tfs.iter().enumerate() {
        let boost = f64::from(config.field_boosts[i]);
        if boost == 0.0 {
            continue;
        }
        let tf = f64::from(tf_val);
        if tf == 0.0 {
            continue;
        }
        // Per-field saturation: this field's contribution is capped at `boost`
        // as tf → ∞ (denominator tf + k1 grows with tf, ratio → 1).
        score += boost * (tf / (tf + k1));
    }
    idf * score
}

/// Return the [`SearchField`] with the highest term frequency in this document.
///
/// When multiple fields share the maximum TF (including ties at zero), the
/// field with the lowest discriminant wins — making the result deterministic.
/// Ties are resolved by discriminant order so searches always produce the same
/// ranking regardless of HashMap iteration order.
#[must_use]
pub fn dominant_field(field_tfs: &[f32; FIELD_COUNT]) -> SearchField {
    let mut best_field = SearchField::Other; // discriminant 7 — fallback
    let mut best_tf = 0.0f32;

    // Walk in discriminant order (0..FIELD_COUNT) so the lowest discriminant
    // wins on ties — deterministic without sorting.
    for (i, &tf) in field_tfs.iter().enumerate() {
        if tf > best_tf {
            best_tf = tf;
            // SAFETY: discriminants 0..7 are always valid SearchField variants.
            best_field = SearchField::from_discriminant(i as u8).unwrap_or(SearchField::Other);
        }
    }

    best_field
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "scoring_tests.rs"]
mod tests;
