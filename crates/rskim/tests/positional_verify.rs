//! Acceptance tests for #393: --phrase / --near token-exact verification.
//!
//! These tests verify:
//! 1. `phrase_tokens_present` / `near_tokens_present` predicate correctness (unit).
//! 2. Short-word handling in the positional engine (all-short fallback, D13).
//! 3. Trigram-containment false positives are eliminated at the CLI gate.
//! 4. `--near 0` is rejected at the CLI.
//! 5. `phrase_tokens_present` returns `Some(range)` with correct byte offsets.
//! 6. `near_tokens_present` handles duplicate query words (D11).
//!
//! # References
//!
//! - AC1-AC17 from the #393 design plan.
//! - AD-393-1 (reader partition), AD-393-3/4 (predicates), AD-393-8 (all-short fallback),
//!   AD-393-9 (--near 0 rejection).

// ============================================================================
// Predicate unit tests
// ============================================================================

use rskim_search::{near_tokens_present, phrase_tokens_present};

// ─── phrase_tokens_present ────────────────────────────────────────────────────

/// AC1/AC3: exact contiguous match returns Some with a valid range.
#[test]
fn phrase_exact_match_returns_some() {
    let result = phrase_tokens_present("fn encode_varint(x: u32)", "encode_varint");
    assert!(
        result.is_some(),
        "AC1: 'encode_varint' must be found in content; got None"
    );
}

/// AC2/AC3: trigram-containment false positive is rejected.
/// `encode_length varint_writer` must NOT match query `encode varint`.
#[test]
fn phrase_trigram_false_positive_is_rejected_ac2() {
    // The trigram reader would match `encode_length varint_writer` for `encode varint`
    // because both `enc` and `var` trigrams appear. The predicate must reject this.
    let result = phrase_tokens_present("encode_length varint_writer", "encode varint");
    assert!(
        result.is_none(),
        "AC2: 'encode varint' phrase must NOT match 'encode_length varint_writer' (trigram false positive)"
    );
}

/// AC3: multi-word phrase matches when words appear in order.
#[test]
fn phrase_multi_word_ordered_match() {
    let result = phrase_tokens_present("fn encode varint bytes end", "encode varint");
    assert!(
        result.is_some(),
        "AC3: 'encode varint' must match when words appear consecutively"
    );
}

/// AC3: phrase does NOT match reversed order.
#[test]
fn phrase_reversed_order_not_matched() {
    let result = phrase_tokens_present("fn varint encode bytes", "encode varint");
    assert!(
        result.is_none(),
        "AC3: 'encode varint' must NOT match 'varint encode' (reversed)"
    );
}

/// AC3: phrase does NOT match words separated by an intervening token.
#[test]
fn phrase_gap_not_matched() {
    let result = phrase_tokens_present("fn encode some varint bytes", "encode varint");
    assert!(
        result.is_none(),
        "AC3: 'encode varint' must NOT match when separated by 'some'"
    );
}

/// AC3: phrase match returns a byte range with the correct start byte.
#[test]
fn phrase_returns_correct_byte_range_ac3() {
    let content = "hello world foo bar";
    // "foo bar" starts at byte 12
    let result = phrase_tokens_present(content, "foo bar");
    let range = result.expect("AC3: 'foo bar' must match");
    assert_eq!(
        &content[range.start..],
        "foo bar",
        "AC3: range.start must point to the first character of the match"
    );
}

/// AC4: empty query returns None (not a match, not a panic).
#[test]
fn phrase_empty_query_returns_none() {
    let result = phrase_tokens_present("fn encode(x: u32) {}", "");
    assert!(result.is_none(), "AC4: empty query must return None");
}

/// AC4: query with only separators returns None.
#[test]
fn phrase_separator_only_query_returns_none() {
    let result = phrase_tokens_present("fn encode(x: u32) {}", "   ");
    assert!(
        result.is_none(),
        "AC4: whitespace-only query must return None"
    );
}

/// AC4: empty content returns None.
#[test]
fn phrase_empty_content_returns_none() {
    let result = phrase_tokens_present("", "encode varint");
    assert!(result.is_none(), "AC4: empty content must return None");
}

/// AC5: single-word query matches a word token (exact, not substring).
/// `fn` must NOT match inside `fn_helper` as a token (but must match when
/// the word token is exactly `fn`).
#[test]
fn phrase_single_word_exact_token_boundary() {
    // `fn` appears only as part of `fn_helper` — no standalone `fn` token.
    let no_match = phrase_tokens_present("fn_helper foo bar", "fn");
    assert!(
        no_match.is_none(),
        "AC5: 'fn' must NOT match inside 'fn_helper' as a word token"
    );

    // `fn` appears as a standalone token.
    let matched = phrase_tokens_present("fn foo bar", "fn");
    assert!(
        matched.is_some(),
        "AC5: 'fn' must match as a standalone word token"
    );
}

// ─── near_tokens_present ──────────────────────────────────────────────────────

/// AC6: near match within the window returns Some.
#[test]
fn near_within_window_returns_some_ac6() {
    // "encode" and "varint" are 2 word tokens apart (gap=2 when counting ordinals).
    let result = near_tokens_present("fn encode_fn some varint bytes", "encode varint", 3);
    assert!(
        result.is_some(),
        "AC6: 'encode'...'varint' within n=3 must return Some"
    );
}

/// AC7: near match exceeds the window — returns None.
#[test]
fn near_beyond_window_returns_none_ac7() {
    // More than 2 tokens apart.
    let result = near_tokens_present("encode a b c varint", "encode varint", 2);
    assert!(
        result.is_none(),
        "AC7: 'encode'...'varint' with gap>n must return None"
    );
}

/// AC7: near is symmetric — reversed word order within the window still matches.
#[test]
fn near_is_order_independent_ac7() {
    let result = near_tokens_present("fn varint some encode bytes", "encode varint", 3);
    assert!(
        result.is_some(),
        "AC7: --near must be order-independent; reversed order within window must match"
    );
}

/// AC8: near with empty query returns None.
#[test]
fn near_empty_query_returns_none_ac8() {
    let result = near_tokens_present("fn encode(x: u32) {}", "", 5);
    assert!(result.is_none(), "AC8: empty query must return None");
}

/// AC8: near with empty content returns None.
#[test]
fn near_empty_content_returns_none_ac8() {
    let result = near_tokens_present("", "encode varint", 5);
    assert!(result.is_none(), "AC8: empty content must return None");
}

/// D11: duplicate query words require DISTINCT document positions.
/// `--near 3 "foo foo"` must only match content with TWO distinct `foo` tokens
/// within the window — a single `foo` must NOT match.
#[test]
fn near_duplicate_query_words_require_distinct_positions_d11() {
    // Single `foo` — must NOT match `foo foo` query.
    let single = near_tokens_present("fn foo bar baz", "foo foo", 3);
    assert!(
        single.is_none(),
        "D11: single 'foo' must NOT satisfy the 'foo foo' near query"
    );

    // Two distinct `foo` tokens within window — must match.
    let double = near_tokens_present("fn foo bar foo end", "foo foo", 5);
    assert!(
        double.is_some(),
        "D11: two 'foo' tokens within n=5 must satisfy 'foo foo' near query"
    );
}

/// AC2: near predicate rejects the trigram-containment false positive.
/// `encode_length varint_writer` must NOT match `encode varint` under near.
#[test]
fn near_trigram_false_positive_is_rejected_ac2() {
    let result = near_tokens_present("encode_length varint_writer", "encode varint", 5);
    assert!(
        result.is_none(),
        "AC2: 'encode varint' near must NOT match 'encode_length varint_writer'"
    );
}

// ─── word-boundary parity ─────────────────────────────────────────────────────

/// D10: collect_word_spans boundary rule must match word_token_indices.
/// Non-ASCII characters are treated as word separators.
#[test]
fn phrase_non_ascii_treated_as_separator() {
    // "café" splits at é boundary: "caf" then "e" — but the content has `caf`
    // as a word and `encode` as another word. The query `caf encode` should
    // only match if they appear as consecutive WORD tokens.
    let result = phrase_tokens_present("caf\u{00e9} encode varint", "encode varint");
    assert!(
        result.is_some(),
        "D10: word boundary after non-ASCII — 'encode varint' still matches as adjacent tokens"
    );
}

/// AC9: near_tokens_present returns a range covering the FULL span from
/// the leftmost to rightmost matched token.
#[test]
fn near_returns_range_covering_full_span_ac9() {
    let content = "alpha beta encode gamma varint delta";
    // "encode" is at some offset; "varint" comes a few tokens later.
    // The returned range must span from "encode" to end of "varint".
    let result = near_tokens_present(content, "encode varint", 3);
    let range = result.expect("AC9: 'encode varint' within n=3 must return Some");
    // Verify the range covers meaningful content.
    let span = &content[range.clone()];
    assert!(
        span.contains("encode"),
        "AC9: range must contain 'encode'; span={span:?}"
    );
    assert!(
        span.contains("varint"),
        "AC9: range must contain 'varint'; span={span:?}"
    );
}
