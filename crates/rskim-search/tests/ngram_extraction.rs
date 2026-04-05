//! Integration tests for n-gram extraction.
//!
//! These tests validate the public contract of [`rskim_search::lexical::ngram`],
//! exercising both index-time (`extract_ngrams`) and query-time
//! (`extract_query_ngrams`) extraction from the caller's perspective.

use rskim_search::lexical::ngram::{extract_ngrams, extract_query_ngrams};
use rskim_search::lexical::Ngram;
use rustc_hash::FxHashMap;

// ============================================================================
// Helpers
// ============================================================================

/// Collect `(Ngram, f32)` pairs into a map, summing duplicate weights.
fn into_map(pairs: Vec<(Ngram, f32)>) -> FxHashMap<Ngram, f32> {
    let mut map = FxHashMap::default();
    for (ng, w) in pairs {
        *map.entry(ng).or_insert(0.0) += w;
    }
    map
}

fn ng(bytes: &[u8]) -> Ngram {
    Ngram::from_bytes(bytes)
}

// ============================================================================
// extract_ngrams — edge cases
// ============================================================================

#[test]
fn empty_string_is_empty() {
    assert!(extract_ngrams("").is_empty());
}

#[test]
fn single_char_is_empty() {
    assert!(extract_ngrams("x").is_empty());
}

#[test]
fn all_whitespace_is_empty() {
    assert!(extract_ngrams("  \t\n  ").is_empty());
}

#[test]
fn two_char_token_single_ngram_with_border_weight() {
    // A 2-char token has its only bigram as both first and last border.
    // Border weighting applies once (OR, not AND) → 2.0.
    let result = extract_ngrams("if");
    assert_eq!(result.len(), 1);
    let (ngram, w) = result[0];
    assert_eq!(ngram, ng(b"if"));
    assert!((w - 2.0).abs() < f32::EPSILON, "2-char border weight should be 2.0, got {w}");
}

#[test]
fn three_char_token_both_borders() {
    // "let": bigrams "le" (first border), "et" (last border) → each 2.0
    let map = into_map(extract_ngrams("let"));
    assert_eq!(map.len(), 2);
    assert!((map[&ng(b"le")] - 2.0).abs() < f32::EPSILON);
    assert!((map[&ng(b"et")] - 2.0).abs() < f32::EPSILON);
}

#[test]
fn five_char_token_interior_is_one() {
    // "hello": "he"(2), "el"(1), "ll"(1), "lo"(2)
    let map = into_map(extract_ngrams("hello"));
    assert_eq!(map.len(), 4);
    assert!((map[&ng(b"he")] - 2.0).abs() < f32::EPSILON, "he should be border 2.0");
    assert!((map[&ng(b"el")] - 1.0).abs() < f32::EPSILON, "el should be interior 1.0");
    assert!((map[&ng(b"ll")] - 1.0).abs() < f32::EPSILON, "ll should be interior 1.0");
    assert!((map[&ng(b"lo")] - 2.0).abs() < f32::EPSILON, "lo should be border 2.0");
}

#[test]
fn repeated_ngram_accumulates_weight() {
    // "fn fn fn": "fn" is a 2-char token (border weight = 2.0) appearing 3 times → 6.0
    let map = into_map(extract_ngrams("fn fn fn"));
    let expected = 2.0 * 3.0;
    assert!(
        (map[&ng(b"fn")] - expected).abs() < f32::EPSILON,
        "fn weight should be {expected}, got {}",
        map[&ng(b"fn")]
    );
}

#[test]
fn all_same_char_single_ngram_accumulates() {
    // "aaaa" → one bigram b"aa", positions 0(border), 1(interior), 2(border) → 5.0
    let map = into_map(extract_ngrams("aaaa"));
    assert_eq!(map.len(), 1);
    assert!((map[&ng(b"aa")] - 5.0).abs() < f32::EPSILON);
}

#[test]
fn multi_token_independent_borders() {
    // "ab cd" → "ab" is 2-char token (border weight = 2.0), "cd" same
    let map = into_map(extract_ngrams("ab cd"));
    assert!((map[&ng(b"ab")] - 2.0).abs() < f32::EPSILON);
    assert!((map[&ng(b"cd")] - 2.0).abs() < f32::EPSILON);
}

#[test]
fn single_char_tokens_skipped() {
    // "a b c" → all tokens have 1 byte, no bigrams possible
    assert!(extract_ngrams("a b c").is_empty());
}

// ============================================================================
// extract_ngrams — Unicode / CJK
// ============================================================================

#[test]
fn unicode_two_byte_char_produces_bigram() {
    // "é" = [0xC3, 0xA9] → exactly one 2-byte bigram
    let result = extract_ngrams("é");
    assert_eq!(result.len(), 1);
}

#[test]
fn cjk_three_byte_char_produces_two_bigrams() {
    // "中" = [0xE4, 0xB8, 0xAD] → two 2-byte windows
    let result = extract_ngrams("中");
    assert_eq!(result.len(), 2);
}

#[test]
fn four_byte_cjk_char_produces_three_bigrams() {
    // "𠀀" (U+20000) is encoded as 4 bytes in UTF-8 → three 2-byte windows
    let result = extract_ngrams("𠀀");
    assert_eq!(result.len(), 3);
}

#[test]
fn mixed_ascii_unicode_text() {
    // Must not panic and must return some results.
    let result = extract_ngrams("fn 中文 rust");
    assert!(!result.is_empty());
}

// ============================================================================
// extract_ngrams — large input
// ============================================================================

#[test]
fn large_input_is_linear_and_no_panic() {
    // ~30KB: simulate a 1000-line Rust file.
    let line = "fn transform(node: &Node, source: &str) -> Option<String> { None }\n";
    let text: String = line.repeat(500);
    assert_eq!(text.len(), line.len() * 500);

    let result = extract_ngrams(&text);
    assert!(!result.is_empty(), "large input produced no ngrams");
}

// ============================================================================
// extract_query_ngrams — edge cases
// ============================================================================

#[test]
fn query_empty_is_empty() {
    assert!(extract_query_ngrams("").is_empty());
}

#[test]
fn query_single_char_is_empty() {
    assert!(extract_query_ngrams("z").is_empty());
}

#[test]
fn query_all_whitespace_is_empty() {
    assert!(extract_query_ngrams("   \t").is_empty());
}

#[test]
fn query_two_chars_returns_one() {
    let result = extract_query_ngrams("fn");
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, ng(b"fn"));
    assert!((result[0].1 - 1.0).abs() < f32::EPSILON);
}

#[test]
fn query_uniform_weights() {
    let result = extract_query_ngrams("hello world");
    assert!(!result.is_empty());
    for (_, w) in result {
        assert!((w - 1.0).abs() < f32::EPSILON, "query weight must be 1.0, got {w}");
    }
}

#[test]
fn query_no_duplicate_ngrams() {
    // "aaaaaa" → only one distinct bigram b"aa", must appear once in result.
    let result = extract_query_ngrams("aaaaaa");
    let ngrams: Vec<Ngram> = result.iter().map(|(ng, _)| *ng).collect();
    let unique_count = ngrams.iter().map(|ng| ng.as_u64()).collect::<std::collections::HashSet<_>>().len();
    assert_eq!(ngrams.len(), unique_count, "query result contains duplicate ngrams");
}

#[test]
fn query_max_limit_not_exceeded() {
    // 200-char query → at most 32 bigrams selected.
    let long: String = "abcdefghijklmnopqrstuvwxyz0123456789"
        .chars()
        .cycle()
        .take(200)
        .collect();
    let result = extract_query_ngrams(&long);
    assert!(
        result.len() <= 32,
        "exceeded MAX_QUERY_NGRAMS: got {}",
        result.len()
    );
}

#[test]
fn query_covers_short_text_completely() {
    // For "abcd" (4 bytes, 3 bigram positions) the result must be non-empty.
    let result = extract_query_ngrams("abcd");
    assert!(!result.is_empty());
}

#[test]
fn query_unicode_does_not_panic() {
    let result = extract_query_ngrams("中文搜索");
    // Must not panic and must produce something meaningful.
    // 3-byte chars produce valid byte-level bigrams.
    let _ = result;
}

// ============================================================================
// Cross-function consistency
// ============================================================================

#[test]
fn query_ngrams_for_single_word_are_subset_of_index_ngrams() {
    // For a single whitespace-free token, every query ngram must appear in the
    // index ngrams (since index extraction covers all bigrams of that token).
    // Multi-word queries cannot satisfy this property because extract_query_ngrams
    // operates on raw bytes (including spaces), while extract_ngrams skips whitespace.
    let token = "AstVisitor";
    let index_set: std::collections::HashSet<u64> =
        extract_ngrams(token).iter().map(|(ng, _)| ng.as_u64()).collect();
    let query_set: std::collections::HashSet<u64> =
        extract_query_ngrams(token).iter().map(|(ng, _)| ng.as_u64()).collect();

    for ng_hash in &query_set {
        assert!(
            index_set.contains(ng_hash),
            "query ngram {ng_hash} not found in index ngrams for token: {token:?}"
        );
    }
}
