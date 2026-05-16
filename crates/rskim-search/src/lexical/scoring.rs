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
    for i in 0..FIELD_COUNT {
        if field_tfs[i] > best_tf {
            best_tf = field_tfs[i];
            // SAFETY: discriminants 0..7 are always valid SearchField variants.
            best_field = SearchField::from_discriminant(i as u8)
                .unwrap_or(SearchField::Other);
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
