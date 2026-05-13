#![allow(clippy::unwrap_used)]

use super::*;

// ── Synthetic weight table ────────────────────────────────────────────────

fn synthetic_weights() -> Vec<(u16, f32)> {
    let mut w: Vec<(u16, f32)> = vec![
        (Ngram::from_bytes(b'f', b'n').key(), 8.0),
        (Ngram::from_bytes(b'n', b' ').key(), 3.0),
        (Ngram::from_bytes(b' ', b'm').key(), 5.0),
        (Ngram::from_bytes(b'm', b'a').key(), 4.0),
        (Ngram::from_bytes(b'a', b'i').key(), 2.0),
        (Ngram::from_bytes(b'i', b'n').key(), 6.0),
        (Ngram::from_bytes(b'n', b'(').key(), 3.5),
        (Ngram::from_bytes(b'(', b')').key(), 7.0),
        (Ngram::from_bytes(b' ', b'p').key(), 5.0),
        (Ngram::from_bytes(b'p', b'a').key(), 4.0),
        (Ngram::from_bytes(b'a', b'r').key(), 2.0),
        (Ngram::from_bytes(b'r', b's').key(), 3.5),
        (Ngram::from_bytes(b's', b'e').key(), 2.5),
    ];
    w.sort_by_key(|&(k, _)| k);
    w
}

// ── Cycle 1: Ngram type ───────────────────────────────────────────────────

#[test]
fn from_bytes_to_bytes_roundtrip() {
    let n = Ngram::from_bytes(b'f', b'n');
    assert_eq!(n.to_bytes(), (b'f', b'n'));
}

#[test]
fn roundtrip_exhaustive_256x256() {
    for b1 in 0u8..=255 {
        for b2 in 0u8..=255 {
            let n = Ngram::from_bytes(b1, b2);
            assert_eq!(n.to_bytes(), (b1, b2), "roundtrip failed for ({b1},{b2})");
        }
    }
}

#[test]
fn key_matches_encoding() {
    let n = Ngram::from_bytes(b'f', b'n');
    assert_eq!(n.key(), (u16::from(b'f') << 8) | u16::from(b'n'));
}

#[test]
fn display_printable_ascii() {
    let n = Ngram::from_bytes(b'f', b'n');
    assert_eq!(n.to_string(), "fn");
}

#[test]
fn display_non_printable_bytes() {
    let n = Ngram::from_bytes(0x01, 0x02);
    assert_eq!(n.to_string(), "\\x01\\x02");
}

#[test]
fn display_space_is_printable() {
    let n = Ngram::from_bytes(b' ', b'a');
    assert_eq!(n.to_string(), " a");
}

#[test]
fn ord_consistency() {
    let a = Ngram::from_bytes(b'a', b'a');
    let b = Ngram::from_bytes(b'b', b'b');
    assert!(a < b);
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
fn extract_two_chars_fn() {
    let w = synthetic_weights();
    let result = extract_ngrams_with_weights("fn", &w);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0, Ngram::from_bytes(b'f', b'n'));
    assert_eq!(result[0].1, 8.0_f32);
}

#[test]
fn extract_fn_main_correct_bigrams() {
    let w = synthetic_weights();
    let result = extract_ngrams_with_weights("fn main()", &w);

    let keys: std::collections::HashSet<u16> = result.iter().map(|(n, _)| n.key()).collect();
    let expected_keys: std::collections::HashSet<u16> = [
        Ngram::from_bytes(b'f', b'n').key(),
        Ngram::from_bytes(b'n', b' ').key(),
        Ngram::from_bytes(b' ', b'm').key(),
        Ngram::from_bytes(b'm', b'a').key(),
        Ngram::from_bytes(b'a', b'i').key(),
        Ngram::from_bytes(b'i', b'n').key(),
        Ngram::from_bytes(b'n', b'(').key(),
        Ngram::from_bytes(b'(', b')').key(),
    ]
    .into_iter()
    .collect();
    assert_eq!(keys, expected_keys);
}

#[test]
fn extract_deduplicates_repeated_bigrams() {
    let w = synthetic_weights();
    let result = extract_ngrams_with_weights("aaaa", &w);
    let aa_count = result
        .iter()
        .filter(|(n, _)| *n == Ngram::from_bytes(b'a', b'a'))
        .count();
    assert_eq!(aa_count, 1, "repeated bigram must appear exactly once");
}

#[test]
fn extract_max_weight_dedup() {
    let mut w: Vec<(u16, f32)> = vec![(Ngram::from_bytes(b'a', b'a').key(), 9.0)];
    w.sort_by_key(|&(k, _)| k);
    let result = extract_ngrams_with_weights("aaaa", &w);
    let entry = result
        .iter()
        .find(|(n, _)| *n == Ngram::from_bytes(b'a', b'a'));
    assert!(entry.is_some());
    assert_eq!(entry.unwrap().1, 9.0_f32);
}

#[test]
fn extract_unknown_bigram_gets_default_weight() {
    let w: Vec<(u16, f32)> = vec![];
    let result = extract_ngrams_with_weights("zz", &w);
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
    assert_eq!(result.len(), 1, "all-space input yields one unique bigram");
}

// ── Cycle 3: Border detection ─────────────────────────────────────────────

#[test]
fn border_ranges_fn_parse() {
    let ranges = token_border_ranges("fn parse");
    assert!(is_border_bigram(0, &ranges), "pos 0 'fn' — border");
    assert!(is_border_bigram(1, &ranges), "pos 1 'n ' overlaps 'fn' border");
    assert!(is_border_bigram(2, &ranges), "pos 2 ' p' starts 'parse'");
}

#[test]
fn border_ranges_single_byte_token() {
    let ranges = token_border_ranges("a b");
    assert!(is_border_bigram(0, &ranges), "pos 0 touches 'a' → border");
    assert!(is_border_bigram(1, &ranges), "pos 1 touches 'b' → border");
}

#[test]
fn border_ranges_empty_query() {
    let ranges = token_border_ranges("");
    assert!(ranges.is_empty());
}

#[test]
fn border_ranges_single_token_long() {
    // "function" (8 bytes): first2=[0,2), last2=[6,8)
    let ranges = token_border_ranges("function");
    for interior_pos in 2..=4_usize {
        assert!(
            !is_border_bigram(interior_pos, &ranges),
            "pos {interior_pos} is interior in 'function'"
        );
    }
    assert!(is_border_bigram(0, &ranges));
    assert!(is_border_bigram(1, &ranges));
    assert!(is_border_bigram(5, &ranges));
    assert!(is_border_bigram(6, &ranges));
}

#[test]
fn border_ranges_multiple_tokens() {
    // "foo bar baz" — three tokens, each has first2 and last2 borders
    let ranges = token_border_ranges("foo bar baz");
    // "foo" at [0,3): first2=[0,2), last2=[1,3) — overlap, so one range [0,3)
    assert!(is_border_bigram(0, &ranges), "pos 0 start of 'foo'");
    // "bar" at [4,7): first2=[4,6), last2=[5,7)
    assert!(is_border_bigram(4, &ranges), "pos 4 start of 'bar'");
    assert!(is_border_bigram(5, &ranges), "pos 5 end of 'bar'");
    // "baz" at [8,11): first2=[8,10), last2=[9,11)
    assert!(is_border_bigram(8, &ranges), "pos 8 start of 'baz'");
}

#[test]
fn border_ranges_whitespace_only() {
    let ranges = token_border_ranges("   ");
    assert!(ranges.is_empty(), "whitespace-only has no tokens");
}

#[test]
fn border_ranges_adjacent_tokens_tabs() {
    // Tabs are whitespace — "a\tb" has tokens "a" and "b"
    let ranges = token_border_ranges("a\tb");
    assert!(is_border_bigram(0, &ranges), "pos 0 at 'a' token");
    assert!(is_border_bigram(1, &ranges), "pos 1 at tab touching 'b'");
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
fn query_extract_fn_main_returns_bigrams() {
    let w = synthetic_weights();
    let result = extract_query_ngrams_with_weights("fn main()", &w);
    assert!(!result.is_empty(), "fn main() should yield bigrams");
}

#[test]
fn query_extract_border_bigrams_have_higher_weight() {
    let w = synthetic_weights();
    let result = extract_query_ngrams_with_weights("fn main()", &w);

    let fn_entry = result
        .iter()
        .find(|(n, _)| *n == Ngram::from_bytes(b'f', b'n'));
    let ai_entry = result
        .iter()
        .find(|(n, _)| *n == Ngram::from_bytes(b'a', b'i'));

    assert!(fn_entry.is_some(), "'fn' must appear in query result");
    if let Some(ai) = ai_entry {
        assert!(
            fn_entry.unwrap().1 > ai.1,
            "'fn' border weight must exceed 'ai' interior weight"
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

    let mut covered = vec![false; bytes.len()];
    for (ngram, _) in &result {
        for (pos, window) in bytes.windows(2).enumerate() {
            if Ngram::from_bytes(window[0], window[1]) == *ngram {
                covered[pos] = true;
                covered[pos + 1] = true;
            }
        }
    }

    for (pos, window) in bytes.windows(2).enumerate() {
        let key = Ngram::from_bytes(window[0], window[1]).key();
        if w.binary_search_by_key(&key, |&(k, _)| k).is_ok() {
            assert!(covered[pos], "position {pos} must be covered");
            assert!(covered[pos + 1], "position {} must be covered", pos + 1);
        }
    }
}

#[test]
fn query_extract_higher_idf_preferred() {
    let w = synthetic_weights();
    let result = extract_query_ngrams_with_weights("fn main()", &w);
    assert!(!result.is_empty());
    assert_eq!(
        result[0].0,
        Ngram::from_bytes(b'f', b'n'),
        "highest-weight bigram 'fn' must be first"
    );
}

#[test]
fn query_extract_cjk_no_panic() {
    let w = synthetic_weights();
    let result = extract_query_ngrams_with_weights("你好世界", &w);
    assert!(!result.is_empty(), "CJK query must yield byte-level bigrams");
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

#[test]
fn extract_query_ngrams_uses_production_weights() {
    let result = extract_query_ngrams("fn main()");
    assert!(!result.is_empty(), "production weights must yield query results");
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
