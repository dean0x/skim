//! Sparse n-gram extraction for the rskim-search lexical index.
//!
//! This module provides the [`Ngram`] newtype (a `u16` bigram) and two pairs of
//! extraction functions:
//!
//! - [`extract_ngrams`] / [`extract_ngrams_with_weights`] — document extraction with
//!   max-weight deduplication (unsorted output).
//! - [`extract_query_ngrams`] / [`extract_query_ngrams_with_weights`] — query extraction
//!   with border-weighted selectivity and greedy covering-set selection (sorted by
//!   weight descending).
//!
//! # Design
//!
//! Bigrams are encoded as `(high_byte << 8) | low_byte`, matching the encoding used by
//! [`crate::weights::BIGRAM_WEIGHTS`].  Weights are looked up via binary search on the
//! sorted table, falling back to [`crate::weights::DEFAULT_WEIGHT`] for unknown pairs.

use std::collections::HashMap;
use std::fmt;

use crate::weights::{BIGRAM_WEIGHTS, DEFAULT_WEIGHT};

// ─────────────────────────────────────────────────────────────────────────────
// Constants
// ─────────────────────────────────────────────────────────────────────────────

/// Multiplier applied to bigrams that fall at a token border (first/last 2 bytes
/// of any whitespace-delimited token) during query extraction.
///
/// Validated empirically at 3.5× — token-boundary bigrams are significantly more
/// discriminating than interior bigrams for code search.
pub const BORDER_MULTIPLIER: f32 = 3.5;

// ─────────────────────────────────────────────────────────────────────────────
// Ngram newtype
// ─────────────────────────────────────────────────────────────────────────────

/// A byte-pair bigram encoded as a `u16`.
///
/// The high byte is the first byte of the pair, the low byte is the second:
/// `key = (b1 << 8) | b2`.  This matches the encoding used in
/// [`crate::weights::BIGRAM_WEIGHTS`], enabling O(log *n*) weight lookup via
/// binary search.
#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Ngram(pub u16);

impl Ngram {
    /// Encode two bytes into an [`Ngram`].
    ///
    /// `(b1 << 8) | b2` — the high byte is `b1`, the low byte is `b2`.
    #[must_use]
    #[inline]
    pub fn from_bytes(b1: u8, b2: u8) -> Self {
        Self((u16::from(b1) << 8) | u16::from(b2))
    }

    /// Decode an [`Ngram`] back into its two component bytes `(b1, b2)`.
    #[must_use]
    #[inline]
    pub fn to_bytes(self) -> (u8, u8) {
        ((self.0 >> 8) as u8, (self.0 & 0xFF) as u8)
    }

    /// Return the raw `u16` key.
    #[must_use]
    #[inline]
    pub fn key(self) -> u16 {
        self.0
    }
}

impl fmt::Display for Ngram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (b1, b2) = self.to_bytes();
        for b in [b1, b2] {
            if b.is_ascii_graphic() || b == b' ' {
                write!(f, "{}", b as char)?;
            } else {
                write!(f, "\\x{b:02X}")?;
            }
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Private helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Look up the IDF weight for a bigram key in a sorted `(key, weight)` slice.
///
/// Falls back to [`DEFAULT_WEIGHT`] when the key is absent.
#[inline]
fn lookup_weight(key: u16, weights: &[(u16, f32)]) -> f32 {
    weights
        .binary_search_by_key(&key, |&(k, _)| k)
        .ok()
        .map(|idx| weights[idx].1)
        .unwrap_or(DEFAULT_WEIGHT)
}

/// Compute token-border byte ranges for `query`.
///
/// For each whitespace-delimited token starting at byte offset `start` with
/// byte length `len`:
///
/// - **`len == 1`**: the range `[start.saturating_sub(1), (start+1).min(bytes.len()))` is
///   marked, so bigrams on either side of the lone byte are treated as borders.
/// - **`len >= 2`**: the first-2-byte range `[start, start+2)` and the last-2-byte range
///   `[end-2, end)` are pushed (only when they do not fully overlap).
///
/// Returns a list of `[lo, hi)` half-open byte intervals.
fn token_border_ranges(query: &str) -> Vec<(usize, usize)> {
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let bytes = query.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let end = i; // exclusive
        let len = end - start;

        if len == 1 {
            let lo = start.saturating_sub(1);
            let hi = (start + 1).min(bytes.len());
            ranges.push((lo, hi));
        } else {
            let first_border_end = (start + 2).min(end);
            ranges.push((start, first_border_end));

            let last_border_start = end.saturating_sub(2);
            if last_border_start >= first_border_end {
                ranges.push((last_border_start, end));
            }
        }
    }

    ranges
}

/// Returns `true` if a bigram starting at `bigram_pos` overlaps any token-border range.
///
/// A bigram at position `p` covers bytes `[p, p+1]`.  It overlaps `[lo, hi)` when
/// `p + 1 >= lo && p < hi`.
#[inline]
fn is_border_bigram(bigram_pos: usize, border_ranges: &[(usize, usize)]) -> bool {
    border_ranges
        .iter()
        .any(|&(lo, hi)| bigram_pos + 1 >= lo && bigram_pos < hi)
}

// ─────────────────────────────────────────────────────────────────────────────
// Document extraction
// ─────────────────────────────────────────────────────────────────────────────

/// Extract weighted bigrams from `text` using the provided sorted weight table.
///
/// For each byte pair in `text`, the IDF weight is looked up via binary search.
/// When the same bigram appears at multiple positions the **maximum** weight is kept
/// (max-weight deduplication).
///
/// Output is **unsorted** and suitable for building a posting list.
///
/// # Arguments
///
/// * `text` — source text (UTF-8; multi-byte sequences are treated as raw bytes).
/// * `weights` — sorted `(bigram_key, idf_weight)` slice, e.g. [`BIGRAM_WEIGHTS`].
///
/// # Panics
///
/// Never panics — byte scanning is infallible.
#[must_use]
pub fn extract_ngrams_with_weights(text: &str, weights: &[(u16, f32)]) -> Vec<(Ngram, f32)> {
    debug_assert!(
        weights.windows(2).all(|w| w[0].0 <= w[1].0),
        "weights must be sorted by key"
    );

    let bytes = text.as_bytes();
    let capacity = bytes.len().min(256);
    let mut map: HashMap<u16, f32> = HashMap::with_capacity(capacity);

    for window in bytes.windows(2) {
        let key = Ngram::from_bytes(window[0], window[1]).key();
        let w = lookup_weight(key, weights);
        let entry = map.entry(key).or_insert(0.0_f32);
        *entry = entry.max(w);
    }

    map.into_iter().map(|(key, w)| (Ngram(key), w)).collect()
}

/// Extract weighted bigrams from `text` using the production [`BIGRAM_WEIGHTS`] table.
///
/// Convenience wrapper around [`extract_ngrams_with_weights`].
/// Output is unsorted; all unique bigrams with their max IDF weight are returned.
#[must_use]
pub fn extract_ngrams(text: &str) -> Vec<(Ngram, f32)> {
    extract_ngrams_with_weights(text, BIGRAM_WEIGHTS)
}

// ─────────────────────────────────────────────────────────────────────────────
// Query extraction
// ─────────────────────────────────────────────────────────────────────────────

/// Extract a border-weighted covering set of bigrams from `query` using the provided
/// sorted weight table.
///
/// This is the query-side counterpart to [`extract_ngrams_with_weights`].  It applies
/// a [`BORDER_MULTIPLIER`] bonus to bigrams that fall at token boundaries (first/last
/// 2 bytes of each whitespace-delimited token), then runs a greedy covering-set
/// heuristic that selects bigrams in descending weighted-IDF order until every byte
/// position in the query is covered.
///
/// Output is sorted by weight **descending** — highest-selectivity bigrams first.
///
/// # Arguments
///
/// * `query` — search query string.
/// * `weights` — sorted `(bigram_key, idf_weight)` slice, e.g. [`BIGRAM_WEIGHTS`].
///
/// # Panics
///
/// Never panics — byte scanning is infallible.
#[must_use]
pub fn extract_query_ngrams_with_weights(query: &str, weights: &[(u16, f32)]) -> Vec<(Ngram, f32)> {
    debug_assert!(
        weights.windows(2).all(|w| w[0].0 <= w[1].0),
        "weights must be sorted by key"
    );

    let bytes = query.as_bytes();
    if bytes.len() < 2 {
        return vec![];
    }

    // Build O(n) border bitmap: border_bitmap[p] == true when byte p is in any border range.
    // A bigram at position `p` covers bytes [p, p+1]; it is a border bigram when either
    // border_bitmap[p] or border_bitmap[p+1] is true — equivalent to the previous
    // `is_border_bigram` linear scan but O(1) per lookup after O(n+r) preprocessing.
    let border_ranges = token_border_ranges(query);
    let mut border_bitmap = vec![false; bytes.len()];
    for (lo, hi) in &border_ranges {
        for b in border_bitmap[*lo..*hi].iter_mut() {
            *b = true;
        }
    }

    // Build candidates: (Ngram, border_weighted_idf, position)
    let mut candidates: Vec<(Ngram, f32, usize)> = bytes
        .windows(2)
        .enumerate()
        .map(|(pos, window)| {
            let ngram = Ngram::from_bytes(window[0], window[1]);
            let base_w = lookup_weight(ngram.key(), weights);
            let multiplier = if border_bitmap[pos] || border_bitmap[pos + 1] {
                BORDER_MULTIPLIER
            } else {
                1.0_f32
            };
            let weighted = (f64::from(base_w) * f64::from(multiplier)) as f32;
            (ngram, weighted, pos)
        })
        .collect();

    // Sort by weighted IDF descending.
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1));

    // Greedy covering set.
    let mut covered = vec![false; bytes.len()];
    let mut uncovered_count = bytes.len();
    let mut selected: Vec<(Ngram, f32)> = Vec::new();

    for (ngram, w, pos) in candidates {
        if !covered[pos] || !covered[pos + 1] {
            if !covered[pos] {
                covered[pos] = true;
                uncovered_count -= 1;
            }
            if !covered[pos + 1] {
                covered[pos + 1] = true;
                uncovered_count -= 1;
            }
            selected.push((ngram, w));
        }
        if uncovered_count == 0 {
            break;
        }
    }

    // Output sorted by weight descending (already in insertion order from greedy,
    // but re-sort to guarantee the contract even for ties).
    selected.sort_by(|a, b| b.1.total_cmp(&a.1));
    selected
}

/// Extract a border-weighted covering set of bigrams from `query` using the
/// production [`BIGRAM_WEIGHTS`] table.
///
/// Convenience wrapper around [`extract_query_ngrams_with_weights`].
/// Output is sorted by weight descending.
#[must_use]
pub fn extract_query_ngrams(query: &str) -> Vec<(Ngram, f32)> {
    extract_query_ngrams_with_weights(query, BIGRAM_WEIGHTS)
}

#[cfg(test)]
#[path = "ngram_tests.rs"]
mod tests;
