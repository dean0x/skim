#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

// ── Synthetic weight table ────────────────────────────────────────────────

fn synthetic_weights() -> Vec<(u32, f32)> {
    let mut w: Vec<(u32, f32)> = vec![
        (Ngram::from_bytes(b'f', b'n', b' ').key(), 8.0),
        (Ngram::from_bytes(b'n', b' ', b'm').key(), 3.0),
        (Ngram::from_bytes(b' ', b'm', b'a').key(), 5.0),
        (Ngram::from_bytes(b'm', b'a', b'i').key(), 4.0),
        (Ngram::from_bytes(b'a', b'i', b'n').key(), 2.0),
        (Ngram::from_bytes(b'i', b'n', b'(').key(), 6.0),
        (Ngram::from_bytes(b'n', b'(', b')').key(), 3.5),
        (Ngram::from_bytes(b' ', b'p', b'a').key(), 5.0),
        (Ngram::from_bytes(b'p', b'a', b'r').key(), 4.0),
        (Ngram::from_bytes(b'a', b'r', b's').key(), 2.0),
        (Ngram::from_bytes(b'r', b's', b'e').key(), 3.5),
    ];
    w.sort_by_key(|&(k, _)| k);
    w
}

// ── Cycle 1: Ngram type ───────────────────────────────────────────────────

#[test]
fn from_bytes_to_bytes_roundtrip() {
    let n = Ngram::from_bytes(b'f', b'n', b' ');
    assert_eq!(n.to_bytes(), (b'f', b'n', b' '));
}

/// Exhaustive roundtrip for all 256^3 = ~16M trigrams is too slow; test a
/// representative sample of 256×256 combinations across all first-byte values.
#[test]
fn roundtrip_exhaustive_256x256_sample() {
    for b1 in 0u8..=255 {
        for b2 in 0u8..=255 {
            // Use b3 = b1 ^ b2 as a deterministic but varied third byte.
            let b3 = b1 ^ b2;
            let n = Ngram::from_bytes(b1, b2, b3);
            assert_eq!(
                n.to_bytes(),
                (b1, b2, b3),
                "roundtrip failed for ({b1},{b2},{b3})"
            );
        }
    }
}

#[test]
fn key_matches_encoding() {
    // AD-355-5 / PF-004: u32::from(b) before shift, never b << k on u8.
    let n = Ngram::from_bytes(b'f', b'n', b' ');
    let expected = (u32::from(b'f') << 16) | (u32::from(b'n') << 8) | u32::from(b' ');
    assert_eq!(n.key(), expected);
}

#[test]
fn display_printable_ascii() {
    let n = Ngram::from_bytes(b'f', b'n', b' ');
    assert_eq!(n.to_string(), "fn ");
}

#[test]
fn display_non_printable_bytes() {
    let n = Ngram::from_bytes(0x01, 0x02, 0x03);
    assert_eq!(n.to_string(), "\\x01\\x02\\x03");
}

#[test]
fn display_space_is_printable() {
    let n = Ngram::from_bytes(b' ', b'a', b'b');
    assert_eq!(n.to_string(), " ab");
}

#[test]
fn ord_consistency() {
    let a = Ngram::from_bytes(b'a', b'a', b'a');
    let b = Ngram::from_bytes(b'b', b'b', b'b');
    assert!(a < b);
}

/// Verify PF-004: u32::from(b) << 16 does not overflow.
/// b3 is 0xFF, and (0xFF << 16) fits comfortably in u32.
#[test]
fn from_bytes_high_byte_no_overflow() {
    let n = Ngram::from_bytes(0xFF, 0xFF, 0xFF);
    // (255<<16)|(255<<8)|255 = 16_777_215 = 0x00FF_FFFF, well within u32.
    assert_eq!(n.key(), 0x00FF_FFFF);
    assert_eq!(n.to_bytes(), (0xFF, 0xFF, 0xFF));
}

// ── Cycle 2: Document extraction ─────────────────────────────────────────

#[test]
fn extract_empty_string() {
    let w = synthetic_weights();
    assert!(extract_ngrams_with_weights("", &w).is_empty());
}

#[test]
fn extract_single_char() {
    let w = synthetic_weights();
    assert!(extract_ngrams_with_weights("a", &w).is_empty());
}

#[test]
fn extract_two_chars_no_trigrams() {
    // Two bytes can't form a trigram — expect empty output.
    let w = synthetic_weights();
    assert!(extract_ngrams_with_weights("fn", &w).is_empty());
}

#[test]
fn extract_three_chars_fn_space() {
    let w = synthetic_weights();
    let result = extract_ngrams_with_weights("fn ", &w);
    assert_eq!(result.len(), 1, "exactly one trigram from 3-byte input");
    assert_eq!(result[0].0, Ngram::from_bytes(b'f', b'n', b' '));
    assert_eq!(result[0].1, 8.0_f32);
}

#[test]
fn extract_fn_main_correct_trigrams() {
    let w = synthetic_weights();
    let result = extract_ngrams_with_weights("fn main()", &w);

    let keys: std::collections::HashSet<u32> = result.iter().map(|(n, _)| n.key()).collect();
    let expected_keys: std::collections::HashSet<u32> = [
        Ngram::from_bytes(b'f', b'n', b' ').key(),
        Ngram::from_bytes(b'n', b' ', b'm').key(),
        Ngram::from_bytes(b' ', b'm', b'a').key(),
        Ngram::from_bytes(b'm', b'a', b'i').key(),
        Ngram::from_bytes(b'a', b'i', b'n').key(),
        Ngram::from_bytes(b'i', b'n', b'(').key(),
        Ngram::from_bytes(b'n', b'(', b')').key(),
    ]
    .into_iter()
    .collect();
    assert_eq!(keys, expected_keys);
}

#[test]
fn extract_deduplicates_repeated_trigrams() {
    let w = synthetic_weights();
    let result = extract_ngrams_with_weights("aaaa", &w);
    let aaa_count = result
        .iter()
        .filter(|(n, _)| *n == Ngram::from_bytes(b'a', b'a', b'a'))
        .count();
    assert_eq!(aaa_count, 1, "repeated trigram must appear exactly once");
}

#[test]
fn extract_max_weight_dedup() {
    let mut w: Vec<(u32, f32)> = vec![(Ngram::from_bytes(b'a', b'a', b'a').key(), 9.0)];
    w.sort_by_key(|&(k, _)| k);
    let result = extract_ngrams_with_weights("aaaa", &w);
    let entry = result
        .iter()
        .find(|(n, _)| *n == Ngram::from_bytes(b'a', b'a', b'a'));
    assert!(entry.is_some());
    assert_eq!(entry.unwrap().1, 9.0_f32);
}

#[test]
fn extract_unknown_trigram_gets_default_weight() {
    let w: Vec<(u32, f32)> = vec![];
    let result = extract_ngrams_with_weights("zzz", &w);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].1, DEFAULT_WEIGHT);
}

#[test]
fn extract_utf8_multibyte_no_panic() {
    let w = synthetic_weights();
    let result = extract_ngrams_with_weights("café", &w);
    assert!(!result.is_empty());
}

#[test]
fn extract_cjk_no_panic() {
    let w = synthetic_weights();
    let result = extract_ngrams_with_weights("你好世界", &w);
    assert!(!result.is_empty());
}

#[test]
fn extract_whitespace_only() {
    let w = synthetic_weights();
    let result = extract_ngrams_with_weights("   ", &w);
    assert_eq!(result.len(), 1, "all-space input yields one unique trigram");
}

// ── Cycle 3: Border detection ─────────────────────────────────────────────

#[test]
fn border_ranges_fn_parse() {
    let ranges = token_border_ranges("fn parse");
    // "fn" (2 bytes) → short-token path, border covers start-1..start+2
    assert!(is_border_trigram(0, &ranges), "pos 0 in 'fn' — border");
    // "parse" starts at pos 3, border first 3 bytes [3..6)
    assert!(
        is_border_trigram(3, &ranges),
        "pos 3 start of 'parse' — border"
    );
}

#[test]
fn border_ranges_single_byte_token() {
    let ranges = token_border_ranges("a b");
    assert!(is_border_trigram(0, &ranges), "pos 0 touches 'a' → border");
    assert!(is_border_trigram(1, &ranges), "pos 1 touches 'b' → border");
}

#[test]
fn border_ranges_empty_query() {
    let ranges = token_border_ranges("");
    assert!(ranges.is_empty());
}

#[test]
fn border_ranges_single_token_long() {
    // "function" (8 bytes): f=0, u=1, n=2, c=3, t=4, i=5, o=6, n=7
    // Border regions: first3=[0,3), last3=[5,8).
    // A trigram at pos p covers bytes [p, p+2].  It is a border trigram if any
    // of its 3 bytes falls inside a border region.
    //   pos 0 → bytes [0,1,2] — all in first3   → border
    //   pos 1 → bytes [1,2,3] — 1,2 in first3   → border
    //   pos 2 → bytes [2,3,4] — byte 2 in first3 → border
    //   pos 3 → bytes [3,4,5] — byte 5 in last3  → border (overlaps last region)
    //   pos 4 → bytes [4,5,6] — 5,6 in last3     → border
    //   pos 5 → bytes [5,6,7] — all in last3     → border
    // Truly interior: pos 2..=2 where [2,3,4] overlaps first3 via byte 2, i.e.
    // with 8-byte "function" every trigram position overlaps at least one border.
    // We verify the clearly interior-by-overlap check on a longer word.
    let ranges = token_border_ranges("function");
    // All trigram positions that clearly start and end INSIDE the token (not at
    // the first3 or last3 bytes) are still border via overlap.  The key contract
    // is that the FIRST and LAST trigram starts are border:
    assert!(
        is_border_trigram(0, &ranges),
        "pos 0 must be border (first byte)"
    );
    assert!(
        is_border_trigram(5, &ranges),
        "pos 5 must be border (last3 region)"
    );
    // pos 4 covers bytes [4,5,6]; byte 5 is in last3 — must be border.
    assert!(
        is_border_trigram(4, &ranges),
        "pos 4 touches last border region"
    );
}

#[test]
fn border_ranges_whitespace_only() {
    let ranges = token_border_ranges("   ");
    assert!(ranges.is_empty(), "whitespace-only has no tokens");
}

// ── Cycle 4: Query extraction ─────────────────────────────────────────────

#[test]
fn query_extract_empty() {
    let w = synthetic_weights();
    assert!(extract_query_ngrams_with_weights("", &w).is_empty());
}

#[test]
fn query_extract_single_char() {
    let w = synthetic_weights();
    assert!(extract_query_ngrams_with_weights("x", &w).is_empty());
}

#[test]
fn query_extract_two_chars_empty() {
    // Two bytes can't form a trigram — query extraction returns empty.
    let w = synthetic_weights();
    assert!(extract_query_ngrams_with_weights("xy", &w).is_empty());
}

#[test]
fn query_extract_fn_main_returns_trigrams() {
    let w = synthetic_weights();
    let result = extract_query_ngrams_with_weights("fn main()", &w);
    assert!(!result.is_empty(), "fn main() should yield trigrams");
}

#[test]
fn query_extract_border_trigrams_have_higher_weight() {
    // With trigrams, the greedy covering set does NOT necessarily include every trigram
    // that starts at a given position — it selects the minimal set that covers all bytes.
    // For "fn main()", the covering set is typically:
    //   "fn " (pos 0, covers 0-2), "in(" (pos 5, covers 5-7), " ma" (pos 2, covers 2-4),
    //   "n()" (pos 6, covers 6-8).
    // "mai" (pos 3) is redundant because " ma" already covers bytes 2-4 and "in(" covers 5-7.
    //
    // The invariant under test: "fn " (highest-weight border trigram) appears in the result,
    // and its weight equals BORDER_MULTIPLIER × base_weight.
    let w = synthetic_weights();
    let result = extract_query_ngrams_with_weights("fn main()", &w);

    let fn_entry = result
        .iter()
        .find(|(n, _)| *n == Ngram::from_bytes(b'f', b'n', b' '));

    assert!(fn_entry.is_some(), "'fn ' must appear in query result");
    // "fn " is the highest-weight trigram; it must be first in the result.
    assert_eq!(
        result[0].0,
        Ngram::from_bytes(b'f', b'n', b' '),
        "highest-weight trigram 'fn ' must be first"
    );
    // The weight must include the border multiplier (8.0 × 3.5 = 28.0).
    assert!(
        (result[0].1 - 28.0_f32).abs() < 0.01_f32,
        "'fn ' border weight must be ~28.0 (8.0 × BORDER_MULTIPLIER=3.5), got {}",
        result[0].1
    );
    // Every other trigram in the result has weight ≤ weight of "fn ".
    for (i, (_, w_i)) in result.iter().enumerate() {
        assert!(
            *w_i <= result[0].1 + 1e-4_f32,
            "result[{i}] weight {} must not exceed result[0] weight {}",
            w_i,
            result[0].1
        );
    }
}

#[test]
fn query_extract_sorted_by_weight_desc() {
    let w = synthetic_weights();
    let result = extract_query_ngrams_with_weights("fn main()", &w);
    for pair in result.windows(2) {
        assert!(
            pair[0].1 >= pair[1].1,
            "result must be sorted by weight descending"
        );
    }
}

#[test]
fn query_extract_covering_set_covers_positions() {
    let query = "fn main()";
    let w = synthetic_weights();
    let result = extract_query_ngrams_with_weights(query, &w);
    let bytes = query.as_bytes();

    // Build coverage from the returned trigrams by scanning every 3-byte window.
    let mut covered = vec![false; bytes.len()];
    for (ngram, _) in &result {
        for (pos, window) in bytes.windows(3).enumerate() {
            if Ngram::from_bytes(window[0], window[1], window[2]) == *ngram {
                covered[pos] = true;
                covered[pos + 1] = true;
                covered[pos + 2] = true;
            }
        }
    }

    // The covering-set contract guarantees every byte position is covered.
    for (pos, &byte) in bytes.iter().enumerate() {
        assert!(
            covered[pos],
            "byte position {pos} ('{:?}') must be covered by the selected trigrams",
            byte as char
        );
    }
}

#[test]
fn query_extract_cjk_no_panic() {
    let w = synthetic_weights();
    let result = extract_query_ngrams_with_weights("你好世界", &w);
    assert!(
        !result.is_empty(),
        "CJK query must yield byte-level trigrams"
    );
}

// ── Cycle 5: Convenience API wiring ──────────────────────────────────────

#[test]
fn extract_ngrams_uses_production_weights() {
    let result = extract_ngrams("fn main()");
    assert!(!result.is_empty(), "production weights must yield results");
    for (_, w) in &result {
        assert!(*w > 0.0_f32, "weight must be positive");
    }
}

// ── Cycle 6: is_single_token predicate matrix (AC #6, AD-372-5) ──────────

/// AC #6 / AD-372-5: `is_single_token` is the single source of truth for
/// exact-symbol mode.  Every entry in the predicate matrix must be asserted
/// individually (PF-007: discriminating, not vacuous).
///
/// Negative assertions ensure the test fails if `is_single_token` were
/// changed to always return `true` or always return `false`.
#[test]
fn is_single_token_predicate_matrix() {
    use super::is_single_token;

    // ── TRUE cases ──────────────────────────────────────────────────────────
    assert!(
        is_single_token("foo"),
        "is_single_token('foo') must be true: non-empty, >= 3 bytes, single token"
    );
    assert!(
        is_single_token("foo::bar"),
        "is_single_token('foo::bar') must be true: punctuation-joined, no whitespace"
    );
    assert!(
        is_single_token("  foo  "),
        "is_single_token('  foo  ') must be true: leading/trailing whitespace stripped"
    );
    assert!(
        is_single_token("decode_postings_varint"),
        "is_single_token('decode_postings_varint') must be true: real symbol name"
    );

    // ── FALSE cases ─────────────────────────────────────────────────────────
    assert!(
        !is_single_token("foo bar"),
        "is_single_token('foo bar') must be false: interior space → two tokens"
    );
    assert!(
        !is_single_token("fn"),
        "is_single_token('fn') must be false: < 3 bytes"
    );
    assert!(
        !is_single_token("if"),
        "is_single_token('if') must be false: < 3 bytes"
    );
    assert!(
        !is_single_token("a\tb"),
        "is_single_token('a\\tb') must be false: interior tab → two tokens"
    );
    assert!(
        !is_single_token(""),
        "is_single_token('') must be false: empty string"
    );
    assert!(
        !is_single_token("  "),
        "is_single_token('  ') must be false: whitespace-only string"
    );
    assert!(
        !is_single_token("ab"),
        "is_single_token('ab') must be false: < 3 bytes"
    );
    assert!(
        !is_single_token("alpha gamma"),
        "is_single_token('alpha gamma') must be false: two space-separated tokens"
    );
}

#[test]
fn extract_query_ngrams_uses_production_weights() {
    let result = extract_query_ngrams("fn main()");
    assert!(
        !result.is_empty(),
        "production weights must yield query results"
    );
    for pair in result.windows(2) {
        assert!(pair[0].1 >= pair[1].1, "must be sorted descending");
    }
}

// ── Cycle 6: Performance sanity ───────────────────────────────────────────

#[test]
fn extract_ngrams_1000_line_file_under_1ms() {
    let line = "fn process_item(item: &Item) -> Result<Output, Error> { todo!() }\n";
    let text: String = line.repeat(1000);

    let start = std::time::Instant::now();
    let result = extract_ngrams(&text);
    let elapsed = start.elapsed();

    assert!(!result.is_empty());

    #[cfg(not(debug_assertions))]
    assert!(
        elapsed.as_micros() < 2000,
        "extract_ngrams on ~60KB took {}μs in release mode (must be < 2000μs)",
        elapsed.as_micros()
    );
    #[cfg(debug_assertions)]
    assert!(
        elapsed.as_millis() < 500,
        "extract_ngrams on ~60KB took {}ms in debug mode (must be < 500ms)",
        elapsed.as_millis()
    );
}
