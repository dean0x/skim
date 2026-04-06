//! N-gram extraction from source text.
//!
//! Provides two extraction modes:
//! - [`extract_ngrams`] — index-time: all bigrams with border weighting
//! - [`extract_query_ngrams`] — query-time: unique bigrams up to cap

use rustc_hash::FxHashMap;

use super::Ngram;

/// Maximum number of bigrams selected for a query (covering-set limit).
const MAX_QUERY_NGRAMS: usize = 32;

/// Weight assigned to bigrams at word borders (first or last bigram of a token).
const BORDER_WEIGHT: f32 = 2.0;

/// Default weight for interior bigrams.
const INTERIOR_WEIGHT: f32 = 1.0;

/// Extract weighted n-grams from source text.
///
/// Used at index time — extracts ALL bigrams with border weighting.
///
/// # Algorithm
///
/// 1. Split text into whitespace-delimited tokens.
/// 2. For each token, slide a 2-byte window across the raw bytes.
/// 3. The first and last bigram of each token receive a 2× weight boost.
/// 4. Interior bigrams receive 1.0 weight.
/// 5. Weights are accumulated across the entire text (repeated bigrams sum).
///
/// # Edge cases
///
/// - Empty text → empty vec
/// - Single byte (< 2 bytes) → empty vec
/// - All whitespace → empty vec
/// - Unicode multi-byte chars → byte-level bigrams (valid for UTF-8 byte sequences)
/// - Repeated bigrams → accumulated weight in result
#[must_use = "returns the extracted bigrams with weights; ignoring them discards indexing data"]
pub fn extract_ngrams(text: &str) -> Vec<(Ngram, f32)> {
    if text.len() < 2 {
        return Vec::new();
    }

    // Accumulate weights per Ngram.
    let mut acc: FxHashMap<Ngram, f32> = FxHashMap::default();

    for token in text.split_ascii_whitespace() {
        let bytes = token.as_bytes();
        if bytes.len() < 2 {
            // Single-byte token: no bigram possible.
            continue;
        }

        let last_start = bytes.len() - 2;

        for i in 0..=last_start {
            let ng = Ngram::from_bytes(&bytes[i..i + 2]);
            // Border bigrams (first or last position within a token) receive 2×weight.
            // Interior bigrams receive 1×weight. A 2-char token has a single bigram that
            // is both first and last — it is treated as a border bigram once (2.0), not
            // double-counted.
            let weight = if i == 0 || i == last_start {
                BORDER_WEIGHT
            } else {
                INTERIOR_WEIGHT
            };
            *acc.entry(ng).or_insert(0.0) += weight;
        }
    }

    acc.into_iter().collect()
}

/// Extract query n-grams as the unique set of bigrams, limited to [`MAX_QUERY_NGRAMS`].
///
/// Used at query time. For 2-byte n-grams the greedy covering-set optimization
/// is over-engineered: adjacent bigrams share at most one byte, so coverage
/// overlap is minimal. Taking all unique bigrams up to the cap is O(n) and
/// produces equivalent recall at query time.
///
/// # Algorithm
///
/// 1. Slide a 2-byte window over the trimmed query bytes.
/// 2. Insert each bigram into a seen-set (deduplication).
/// 3. Collect up to [`MAX_QUERY_NGRAMS`] unique bigrams, each weighted 1.0.
///
/// # Edge cases
///
/// - Empty query → empty vec
/// - Single byte → empty vec (no bigram)
/// - All whitespace → empty vec
#[must_use = "returns the query bigrams needed for index lookup; ignoring them skips the search"]
pub fn extract_query_ngrams(query: &str) -> Vec<(Ngram, f32)> {
    // Reject queries that are entirely whitespace — they carry no searchable signal.
    let trimmed = query.trim();
    if trimmed.len() < 2 {
        return Vec::new();
    }
    let bytes = trimmed.as_bytes();

    let total_positions = bytes.len().saturating_sub(1); // number of bigram start positions
    if total_positions == 0 {
        return Vec::new();
    }

    // Collect all unique bigrams up to the cap.
    let mut seen: rustc_hash::FxHashSet<Ngram> = rustc_hash::FxHashSet::default();
    let mut selected: Vec<(Ngram, f32)> = Vec::new();

    for i in 0..total_positions {
        if selected.len() >= MAX_QUERY_NGRAMS {
            break;
        }
        let ng = Ngram::from_bytes(&bytes[i..i + 2]);
        if seen.insert(ng) {
            selected.push((ng, INTERIOR_WEIGHT));
        }
    }

    selected
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // extract_ngrams — basic correctness
    // -----------------------------------------------------------------------

    #[test]
    fn empty_string_returns_empty() {
        assert!(extract_ngrams("").is_empty());
    }

    #[test]
    fn single_char_returns_empty() {
        assert!(extract_ngrams("a").is_empty());
    }

    #[test]
    fn all_whitespace_returns_empty() {
        assert!(extract_ngrams("   \t\n  ").is_empty());
    }

    #[test]
    fn two_char_token_produces_one_ngram() {
        let result = extract_ngrams("fn");
        // "fn" → one bigram b"fn". The sole bigram is both the first and last border,
        // but border weighting is applied once (OR, not AND), yielding 2.0.
        assert_eq!(result.len(), 1);
        let (ng, w) = result[0];
        assert_eq!(ng, Ngram::from_bytes(b"fn"));
        assert!((w - 2.0).abs() < f32::EPSILON, "expected 2.0, got {w}");
    }

    #[test]
    fn three_char_token_border_weighting() {
        // "foo" → bigrams: b"fo" (pos 0, border), b"oo" (pos 1, border, last)
        let result = extract_ngrams("foo");
        assert_eq!(result.len(), 2);

        let map: FxHashMap<Ngram, f32> = result.into_iter().collect();
        let fo = Ngram::from_bytes(b"fo");
        let oo = Ngram::from_bytes(b"oo");

        assert!(
            (map[&fo] - 2.0).abs() < f32::EPSILON,
            "fo should be 2.0 (first border)"
        );
        assert!(
            (map[&oo] - 2.0).abs() < f32::EPSILON,
            "oo should be 2.0 (last border)"
        );
    }

    #[test]
    fn four_char_token_interior_weight() {
        // "abcd" → bigrams: "ab"(border 2.0), "bc"(interior 1.0), "cd"(border 2.0)
        let result = extract_ngrams("abcd");
        let map: FxHashMap<Ngram, f32> = result.into_iter().collect();

        assert!((map[&Ngram::from_bytes(b"ab")] - 2.0).abs() < f32::EPSILON);
        assert!((map[&Ngram::from_bytes(b"bc")] - 1.0).abs() < f32::EPSILON);
        assert!((map[&Ngram::from_bytes(b"cd")] - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn repeated_ngrams_accumulate() {
        // "ab ab" → "ab" appears as border bigram twice → 2.0 + 2.0 = 4.0
        let result = extract_ngrams("ab ab");
        let map: FxHashMap<Ngram, f32> = result.into_iter().collect();
        let ab = Ngram::from_bytes(b"ab");
        assert!(
            (map[&ab] - 4.0).abs() < f32::EPSILON,
            "expected 4.0, got {}",
            map[&ab]
        );
    }

    #[test]
    fn repeated_chars_produce_single_ngram() {
        // "aaaa" → only bigram is b"aa", appears 3 times (pos 0 border, pos 1 interior, pos 2 border)
        let result = extract_ngrams("aaaa");
        assert_eq!(result.len(), 1);
        let (ng, w) = result[0];
        assert_eq!(ng, Ngram::from_bytes(b"aa"));
        // pos 0 (border=2) + pos 1 (interior=1) + pos 2 (border=2) = 5.0
        assert!((w - 5.0).abs() < f32::EPSILON, "expected 5.0, got {w}");
    }

    #[test]
    fn multi_word_text_independent_border_weighting() {
        // "fn foo" → token "fn" is 2-char: bigram "fn" at 2.0 (first==last border, applied once)
        //           token "foo": "fo" at 2.0 (first border), "oo" at 2.0 (last border)
        let result = extract_ngrams("fn foo");
        let map: FxHashMap<Ngram, f32> = result.into_iter().collect();

        let fn_ng = Ngram::from_bytes(b"fn");
        assert!((map[&fn_ng] - 2.0).abs() < f32::EPSILON);

        let fo = Ngram::from_bytes(b"fo");
        assert!((map[&fo] - 2.0).abs() < f32::EPSILON);
    }

    // -----------------------------------------------------------------------
    // extract_ngrams — Unicode / CJK
    // -----------------------------------------------------------------------

    #[test]
    fn unicode_multibyte_works_at_byte_level() {
        // "é" is 2 bytes (0xC3 0xA9) — produces one bigram
        let result = extract_ngrams("é");
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn cjk_three_byte_chars_produce_bigrams() {
        // "中" is 3 bytes (0xE4 0xB8 0xAD) → bigrams from byte windows
        let result = extract_ngrams("中");
        assert_eq!(result.len(), 2); // two 2-byte windows in a 3-byte token
    }

    // -----------------------------------------------------------------------
    // extract_query_ngrams — basic correctness
    // -----------------------------------------------------------------------

    #[test]
    fn query_empty_returns_empty() {
        assert!(extract_query_ngrams("").is_empty());
    }

    #[test]
    fn query_single_char_returns_empty() {
        assert!(extract_query_ngrams("a").is_empty());
    }

    #[test]
    fn query_all_whitespace_returns_empty() {
        assert!(extract_query_ngrams("   ").is_empty());
    }

    #[test]
    fn query_two_chars_returns_one_ngram() {
        let result = extract_query_ngrams("fn");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, Ngram::from_bytes(b"fn"));
        assert!((result[0].1 - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn query_covers_all_positions() {
        // For "abcd", all three unique bigrams ("ab","bc","cd") are included.
        let result = extract_query_ngrams("abcd");
        assert_eq!(result.len(), 3);

        // All weights are uniform 1.0.
        for (_, w) in &result {
            assert!((*w - 1.0).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn query_no_duplicates_in_result() {
        // Even with repeated chars, each Ngram appears at most once.
        let result = extract_query_ngrams("aabbaabb");
        let ngrams: Vec<Ngram> = result.iter().map(|(ng, _)| *ng).collect();
        let unique: std::collections::HashSet<u64> = ngrams.iter().map(|ng| ng.as_u64()).collect();
        assert_eq!(
            ngrams.len(),
            unique.len(),
            "duplicate ngrams in query result"
        );
    }

    #[test]
    fn query_respects_max_limit() {
        // Generate a very long query to test the 32-bigram cap.
        let long_query: String = (0..200u8).map(|b| b as char).collect();
        let result = extract_query_ngrams(&long_query);
        assert!(
            result.len() <= MAX_QUERY_NGRAMS,
            "exceeded MAX_QUERY_NGRAMS"
        );
    }

    #[test]
    fn query_uniform_weights() {
        let result = extract_query_ngrams("hello world");
        for (_, w) in result {
            assert!(
                (w - 1.0).abs() < f32::EPSILON,
                "expected uniform 1.0, got {w}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Performance smoke test (correctness only — timing verified separately)
    // -----------------------------------------------------------------------

    #[test]
    fn large_text_does_not_panic() {
        // ~30KB of repeated ASCII — simulates a 1000-line file.
        let text: String =
            "fn process_item(item: Item) -> Result<Output> { /* ... */ }\n".repeat(500);
        let result = extract_ngrams(&text);
        // Must produce some ngrams and not explode.
        assert!(!result.is_empty());
    }
}
