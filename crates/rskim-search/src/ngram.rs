//! Sparse n-gram extraction for the rskim-search lexical index.
//!
//! This module provides the [`Ngram`] newtype (a `u32` trigram) and two pairs of
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
//! Trigrams are encoded as `(b1 << 16) | (b2 << 8) | b3`, matching the encoding used by
//! [`crate::weights::TRIGRAM_WEIGHTS`].  Weights are looked up via binary search on the
//! sorted table, falling back to [`crate::weights::DEFAULT_WEIGHT`] for unknown trigrams.
//!
//! # AD-355-5: Width move u16 → u32 for trigrams
//!
//! The key type was widened from `u16` (bigram, 2-byte windows) to `u32` (trigram,
//! 3-byte windows) to restore IDF selectivity: with 65 536 possible bigrams, ≈24.9%
//! of the corpus space maps to `DEFAULT_WEIGHT`, causing near-uniform BM25F scores
//! and missed exact matches. Trigrams address `16 777 216` distinct keys, yielding
//! significantly higher IDF dispersion.
//!
//! PF-004 applies: always widen `u8` bytes to `u32` **before** shift arithmetic
//! (`u32::from(b1) << 16`, never `b1 << 16`), so intermediate results never
//! overflow a `u8`.

use std::collections::HashMap;
use std::fmt;

use crate::weights::{TRIGRAM_WEIGHTS, lookup_weight};

// Re-export DEFAULT_WEIGHT so tests can reach it via `use super::*`.
#[cfg(test)]
use crate::weights::DEFAULT_WEIGHT;

// ============================================================================
// Constants
// ============================================================================

/// Multiplier applied to trigrams that fall at a token border (first/last 3 bytes
/// of any whitespace-delimited token) during query extraction.
///
/// Validated empirically at 3.5× — token-boundary trigrams are significantly more
/// discriminating than interior trigrams for code search.
pub const BORDER_MULTIPLIER: f32 = 3.5;

// ============================================================================
// Ngram newtype
// ============================================================================

/// A three-byte trigram encoded as a `u32`.
///
/// The encoding is: `key = (b1 << 16) | (b2 << 8) | b3`.
///
/// This matches the encoding used in [`crate::weights::TRIGRAM_WEIGHTS`], enabling
/// O(log *n*) weight lookup via binary search.
///
/// # AD-355-5 / PF-004
///
/// The key type was widened from `u16` (bigram) to `u32` (trigram) for IDF
/// selectivity (#355 Part B). All byte-to-key shifts are performed on
/// `u32`-widened values (`u32::from(b)`) — never on bare `u8` — to prevent
/// overflow. (#358 will own the v3→v4 format bump for posting compression.)
#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Ngram(pub(crate) u32);

impl Ngram {
    /// Encode three bytes into an [`Ngram`].
    ///
    /// `(b1 << 16) | (b2 << 8) | b3` — PF-004: widen each byte to `u32` before
    /// shifting to prevent intermediate overflow.
    #[must_use]
    #[inline]
    pub fn from_bytes(b1: u8, b2: u8, b3: u8) -> Self {
        Self((u32::from(b1) << 16) | (u32::from(b2) << 8) | u32::from(b3))
    }

    /// Construct an [`Ngram`] directly from a raw `u32` key.
    ///
    /// Intended for internal crate use where the key is already in the encoded
    /// `(b1 << 16) | (b2 << 8) | b3` form (e.g. when iterating over a HashMap
    /// of `u32` keys built from [`from_bytes`]).  External callers should use
    /// [`from_bytes`] to guarantee the correct encoding.
    #[must_use]
    #[inline]
    pub(crate) fn from_raw(key: u32) -> Self {
        Self(key)
    }

    /// Decode an [`Ngram`] back into its three component bytes `(b1, b2, b3)`.
    #[must_use]
    #[inline]
    pub fn to_bytes(self) -> (u8, u8, u8) {
        (
            ((self.0 >> 16) & 0xFF) as u8,
            ((self.0 >> 8) & 0xFF) as u8,
            (self.0 & 0xFF) as u8,
        )
    }

    /// Return the raw `u32` key.
    #[must_use]
    #[inline]
    pub fn key(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Ngram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (b1, b2, b3) = self.to_bytes();
        for b in [b1, b2, b3] {
            if b.is_ascii_graphic() || b == b' ' {
                write!(f, "{}", b as char)?;
            } else {
                write!(f, "\\x{b:02X}")?;
            }
        }
        Ok(())
    }
}

// ============================================================================
// Private helpers
// ============================================================================

/// Compute token-border byte ranges for `query`.
///
/// For each whitespace-delimited token starting at byte offset `start` with
/// byte length `len`:
///
/// - **`len <= 2`**: the range `[start.saturating_sub(1), (start+2).min(bytes.len()))` is
///   marked, so trigrams on either side of short tokens are treated as borders.
/// - **`len >= 3`**: the first-3-byte range `[start, start+3)` and the last-3-byte range
///   `[end-3, end)` are pushed (only when they do not fully overlap).
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

        if len <= 2 {
            // Short tokens: widen the border region to cover the full token.
            let lo = start.saturating_sub(1);
            let hi = (start + 2).min(bytes.len());
            ranges.push((lo, hi));
        } else {
            let first_border_end = (start + 3).min(end);
            ranges.push((start, first_border_end));

            let last_border_start = end.saturating_sub(3);
            if last_border_start >= first_border_end {
                ranges.push((last_border_start, end));
            }
        }
    }

    ranges
}

/// Returns `true` if a trigram starting at `trigram_pos` overlaps any token-border range.
///
/// A trigram at position `p` covers bytes `[p, p+2]`.  It overlaps `[lo, hi)` when
/// `p + 2 >= lo && p < hi`.
///
/// Used only in tests; production code uses the O(1) border bitmap instead.
#[cfg(test)]
#[inline]
fn is_border_trigram(trigram_pos: usize, border_ranges: &[(usize, usize)]) -> bool {
    border_ranges
        .iter()
        .any(|&(lo, hi)| trigram_pos + 2 >= lo && trigram_pos < hi)
}

// ============================================================================
// Document extraction
// ============================================================================

/// Extract weighted trigrams from `text` using the provided sorted weight table.
///
/// For each byte triple in `text`, the IDF weight is looked up via binary search.
/// When the same trigram appears at multiple positions the **maximum** weight is kept
/// (max-weight deduplication).
///
/// Output is **unsorted** and suitable for building a posting list.
///
/// # Arguments
///
/// * `text` — source text (UTF-8; multi-byte sequences are treated as raw bytes).
/// * `weights` — sorted `(trigram_key, idf_weight)` slice, e.g. [`TRIGRAM_WEIGHTS`].
///
/// # Panics
///
/// Never panics — byte scanning is infallible.
#[must_use]
pub fn extract_ngrams_with_weights(text: &str, weights: &[(u32, f32)]) -> Vec<(Ngram, f32)> {
    debug_assert!(
        weights.windows(2).all(|w| w[0].0 <= w[1].0),
        "weights must be sorted by key"
    );

    let bytes = text.as_bytes();
    let capacity = bytes.len().min(256);
    let mut map: HashMap<u32, f32> = HashMap::with_capacity(capacity);

    for window in bytes.windows(3) {
        let key = Ngram::from_bytes(window[0], window[1], window[2]).key();
        let w = lookup_weight(key, weights);
        let entry = map.entry(key).or_insert(0.0_f32);
        *entry = entry.max(w);
    }

    map.into_iter()
        .map(|(key, w)| (Ngram::from_raw(key), w))
        .collect()
}

/// Extract weighted trigrams from `text` using the production [`TRIGRAM_WEIGHTS`] table.
///
/// Convenience wrapper around [`extract_ngrams_with_weights`].
/// Output is unsorted; all unique trigrams with their max IDF weight are returned.
#[must_use]
pub fn extract_ngrams(text: &str) -> Vec<(Ngram, f32)> {
    extract_ngrams_with_weights(text, TRIGRAM_WEIGHTS)
}

// ============================================================================
// Query extraction
// ============================================================================

/// Extract a border-weighted covering set of trigrams from `query` using the provided
/// sorted weight table.
///
/// This is the query-side counterpart to [`extract_ngrams_with_weights`].  It applies
/// a [`BORDER_MULTIPLIER`] bonus to trigrams that fall at token boundaries (first/last
/// 3 bytes of each whitespace-delimited token), then runs a greedy covering-set
/// heuristic that selects trigrams in descending weighted-IDF order until every byte
/// position in the query is covered.
///
/// Output is sorted by weight **descending** — highest-selectivity trigrams first.
///
/// # Short-query behaviour (< 3 bytes)
///
/// Queries shorter than 3 bytes (e.g. `"fn"`, `"if"`) cannot produce any trigrams,
/// so this function returns an empty `Vec`.  Callers that need to support short queries
/// must handle the empty case themselves — typically by falling back to a full-scan
/// candidate set that is then narrowed by a literal substring verify step.
///
/// The [`crate::index::reader::NgramIndexReader`] implements this fallback via AD-355-7
/// in its `search()` method.
///
/// # Arguments
///
/// * `query` — search query string.
/// * `weights` — sorted `(trigram_key, idf_weight)` slice, e.g. [`TRIGRAM_WEIGHTS`].
///
/// # Panics
///
/// Never panics — byte scanning is infallible.
#[must_use]
pub fn extract_query_ngrams_with_weights(query: &str, weights: &[(u32, f32)]) -> Vec<(Ngram, f32)> {
    debug_assert!(
        weights.windows(2).all(|w| w[0].0 <= w[1].0),
        "weights must be sorted by key"
    );

    let bytes = query.as_bytes();
    if bytes.len() < 3 {
        return vec![];
    }

    // Build O(n) border bitmap: border_bitmap[p] == true when byte p is in any border range.
    // A trigram at position `p` covers bytes [p, p+2]; it is a border trigram when any of
    // border_bitmap[p], border_bitmap[p+1], or border_bitmap[p+2] is true — equivalent to
    // the previous `is_border_trigram` linear scan but O(1) per lookup after O(n+r) preprocessing.
    let border_ranges = token_border_ranges(query);
    let mut border_bitmap = vec![false; bytes.len()];
    for (lo, hi) in &border_ranges {
        for b in border_bitmap[*lo..*hi].iter_mut() {
            *b = true;
        }
    }

    // Build candidates: (Ngram, border_weighted_idf, position)
    let mut candidates: Vec<(Ngram, f32, usize)> = bytes
        .windows(3)
        .enumerate()
        .map(|(pos, window)| {
            let ngram = Ngram::from_bytes(window[0], window[1], window[2]);
            let base_w = lookup_weight(ngram.key(), weights);
            let multiplier =
                if border_bitmap[pos] || border_bitmap[pos + 1] || border_bitmap[pos + 2] {
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
        if !covered[pos] || !covered[pos + 1] || !covered[pos + 2] {
            for slot in &mut covered[pos..pos + 3] {
                if !*slot {
                    *slot = true;
                    uncovered_count -= 1;
                }
            }
            selected.push((ngram, w));
        }
        if uncovered_count == 0 {
            break;
        }
    }

    // `candidates` is sorted descending and `selected` is built in that order,
    // so the output is already sorted by weight descending.
    selected
}

/// Extract a border-weighted covering set of trigrams from `query` using the
/// production [`TRIGRAM_WEIGHTS`] table.
///
/// Convenience wrapper around [`extract_query_ngrams_with_weights`].
/// Output is sorted by weight descending.
#[must_use]
pub fn extract_query_ngrams(query: &str) -> Vec<(Ngram, f32)> {
    extract_query_ngrams_with_weights(query, TRIGRAM_WEIGHTS)
}

// ============================================================================
// Query-shape predicate (AD-372-5)
// ============================================================================

/// Return `true` iff `query` is an exact-symbol query — a single contiguous
/// token that is non-empty, at least 3 bytes, and contains no interior ASCII
/// whitespace.
///
/// # AD-372-5: Single source of truth for "exact-symbol mode"
///
/// Both [`crate::index::reader::NgramIndexReader::search`] (which branches on
/// query shape) and `rskim::cmd::search::query` (which decides whether to bypass
/// `LEXICAL_CANDIDATE_POOL_K`) consult this predicate.  Keeping the definition
/// in one place prevents the two layers from diverging.
///
/// **Semantics:** `query.trim()` must be non-empty, `>= 3` bytes, and satisfy
/// `split_whitespace().count() == 1`.  Leading/trailing whitespace is ignored;
/// interior whitespace (space, tab, or any ASCII whitespace byte) causes the
/// predicate to return `false` and routes the query to the BM25F UNION path.
///
/// # Examples
///
/// ```rust
/// use rskim_search::ngram::is_single_token;
/// assert!(is_single_token("foo"));
/// assert!(is_single_token("foo::bar"));
/// assert!(is_single_token("  foo  "));
/// assert!(!is_single_token("foo bar"));
/// assert!(!is_single_token("fn"));
/// assert!(!is_single_token("a\tb"));
/// assert!(!is_single_token(""));
/// ```
#[must_use]
pub fn is_single_token(query: &str) -> bool {
    // AD-372-5: non-empty after trim, >= 3 bytes, exactly one whitespace-token.
    let trimmed = query.trim();
    trimmed.len() >= 3 && trimmed.split_whitespace().count() == 1
}

// ============================================================================
// Query positional tokens (v5, #392 / #380 Phase 2)
// ============================================================================

/// A query word-token and its within-word trigrams (v5 positional search, #392).
///
/// `token_off` is the word's ordinal from [`crate::lexical::word_token_indices`]
/// (0-based, contiguous in query order). `trigrams` are the DEDUPED trigrams that
/// lie ENTIRELY within the word (all three bytes share this word); a word < 3
/// bytes yields an empty `trigrams` (it cannot be located positionally).
#[derive(Debug, Clone)]
pub struct QueryToken {
    pub token_off: u32,
    pub trigrams: Vec<Ngram>,
}

/// Split `query` into word tokens (`[A-Za-z0-9_]+` maximal runs) and, for each,
/// collect its within-word trigrams (deduped by key). Words < 3 bytes yield an
/// empty trigram list. Returned in query order; `tokens[k].token_off == k`.
///
/// Cross-word/border trigrams are intentionally EXCLUDED so that `--near`
/// (non-adjacent) matches are not forced into adjacency.
#[must_use]
pub fn extract_query_positional_tokens(query: &str) -> Vec<QueryToken> {
    let bytes = query.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let tok_of_byte = crate::lexical::word_token_indices(query);
    let mut tokens: Vec<QueryToken> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if !is_word(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_word(bytes[i]) {
            i += 1;
        }
        let end = i; // exclusive
        let token_off = tok_of_byte[start];
        let mut trigrams: Vec<Ngram> = Vec::new();
        if end - start >= 3 {
            for j in start..=(end - 3) {
                trigrams.push(Ngram::from_bytes(bytes[j], bytes[j + 1], bytes[j + 2]));
            }
            trigrams.sort_unstable_by_key(|n| n.key());
            trigrams.dedup_by_key(|n| n.key());
        }
        tokens.push(QueryToken {
            token_off,
            trigrams,
        });
    }
    tokens
}

#[cfg(test)]
#[path = "ngram_tests.rs"]
mod tests;
