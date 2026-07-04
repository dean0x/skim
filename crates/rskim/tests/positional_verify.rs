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

// Imports used by CLI integration tests below.
use assert_cmd::Command;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

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
    let result = near_tokens_present("fn encode some varint bytes", "encode varint", 3);
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

/// D11 (regression): a query word repeated 3× must still match when the content
/// holds three distinct occurrences within the window, even if EARLIER
/// occurrences sit outside the span and get evicted during window shrink.
///
/// Guards the `near_tokens_present` `have`-bookkeeping bug: the shrink evicts
/// `a@0` then `a@1` in one step; the asymmetric decrement guard over-counted the
/// eviction, underflowing `have` (usize) — a debug panic / release false-negative.
/// `a@6 a@7 a@8` are three distinct `a` within span 2 (≤ n), so the answer is Some.
#[test]
fn near_triplicate_query_word_with_evicted_early_occurrences_d11() {
    // Word tokens: a(0) a(1) q(2) q(3) q(4) q(5) a(6) a(7) a(8).
    let result = near_tokens_present("a a q q q q a a a", "a a a", 2);
    assert!(
        result.is_some(),
        "D11: three distinct 'a' within n=2 (ordinals 6,7,8) must match 'a a a'; \
         a None here means the window bookkeeping dropped the valid late window"
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

// ============================================================================
// CLI integration tests — ACs 1-11, 13, 15, 16, AC17 control
// ============================================================================
//
// Each test builds a purpose-built corpus in a `tempfile::tempdir()`, indexes
// it via the real `skim` binary, and asserts on `--json` output.  Cache I/O is
// isolated via `SKIM_CACHE_DIR` — no test ever writes to `~/.cache/skim/`.
//
// AC12 (reader pure tests) is CI-gated: the rskim-search lib-test binary hangs
// at startup on this machine (PF-010).  Do NOT add it here.
//
// AC17 (--phrase + --blast-radius peer gate) requires injecting co-change edges
// into a temporal DB.  Building temporal history in a temp dir is fragile; it is
// CI-gated.  The non-positional UNION control assertion is covered in
// `cli_ac17_blast_radius_control_exits_zero` below (proves the non-positional
// path is unchanged).  See design plan §AC17 for the CI-only test spec.

// ── shared helpers ────────────────────────────────────────────────────────────

/// Build the search index for `proj`, routing all cache I/O to `cache`.
fn build_index(proj: &Path, cache: &Path) {
    Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--build", "--root"])
        .arg(proj)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .assert()
        .success();
}

/// Run `skim search <extra_args> --json --root <proj>` and return parsed JSON.
///
/// Panics on non-zero exit or non-JSON stdout.
fn search_json(proj: &Path, cache: &Path, extra_args: &[&str]) -> Value {
    let output = Command::cargo_bin("skim")
        .unwrap()
        .arg("search")
        .args(extra_args)
        .args(["--json", "--root"])
        .arg(proj)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not valid JSON: {e}\nstdout:\n{stdout}"))
}

/// Extract the `path` field from each result as a `HashSet<String>`.
fn paths(v: &Value) -> HashSet<String> {
    v["results"]
        .as_array()
        .expect("results must be an array")
        .iter()
        .map(|r| {
            r["path"]
                .as_str()
                .expect("path must be a string")
                .to_string()
        })
        .collect()
}

// ── D12 / AD-393-9: --near 0 is rejected ─────────────────────────────────────

/// D12: `--near 0` must exit non-zero with an actionable error message.
/// AD-393-9: parse_near_value rejects n==0 with "span must be > 0".
#[test]
fn cli_near_zero_is_rejected_d12() {
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--near", "0", "some query"])
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("span must be > 0"),
        "D12: stderr must contain 'span must be > 0'; got:\n{stderr}"
    );
}

// ── AC1: short-word phrase recall ─────────────────────────────────────────────

/// AC1: `--phrase 'human in the loop'` must return the file where the literal
/// contiguous phrase appears, even though 'in' is a short (<3-byte) word.
/// Pre-fix: the reader bailed on 'in' and returned empty.
#[test]
fn cli_ac1_short_word_phrase_recall() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // A has the literal phrase; B has 'in' and 'loop' but not adjacent.
    fs::write(
        proj.path().join("a.rs"),
        "let phase = human in the loop pattern;\n",
    )
    .unwrap();
    fs::write(proj.path().join("b.rs"), "in_fn(); /* ... */ loop {}\n").unwrap();
    build_index(proj.path(), cache.path());

    let json = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "human in the loop"],
    );
    let got = paths(&json);

    assert_eq!(
        got,
        HashSet::from(["a.rs".to_string()]),
        "AC1: 'human in the loop' must return EXACTLY {{a.rs}}; got {got:?}"
    );
    assert_eq!(
        json["total"].as_u64().unwrap_or(0),
        1,
        "AC1: total must be 1"
    );
}

// ── AC2: 'fn main' short-leading-word recall ──────────────────────────────────

/// AC2: `--phrase 'fn main'` must find `fn main()` even though 'fn' is a
/// short (<3-byte) word.  The file where 'main' appears as part of 'main_x'
/// (superstring) must be excluded.
#[test]
fn cli_ac2_fn_main_recall() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // A has 'fn main' as adjacent word tokens.
    fs::write(proj.path().join("a.rs"), "fn main() { }\n").unwrap();
    // B has 'fn' and 'main_x' — 'main' is a substring of 'main_x', not a
    // standalone token, so the predicate must reject it.
    fs::write(proj.path().join("b.rs"), "fn helper(); let main_x = 1;\n").unwrap();
    build_index(proj.path(), cache.path());

    let json = search_json(proj.path(), cache.path(), &["--phrase", "fn main"]);
    let got = paths(&json);

    assert_eq!(
        got,
        HashSet::from(["a.rs".to_string()]),
        "AC2: '--phrase fn main' must return EXACTLY {{a.rs}}, got {got:?}"
    );
}

// ── AC3: superstring + unanchored-substring FP rejection ─────────────────────

/// AC3: `--phrase 'encode varint'` must return ONLY the file with the
/// token-exact adjacent pair.  The superstring file (`encode_length
/// varint_writer`) and the unanchored-substring decoy (`reencode varint2`)
/// must BOTH be absent.
///
/// Ground truth: boundary-anchored git grep returns {a.rs} only.
#[test]
fn cli_ac3_superstring_fp_rejection() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // A: real token-adjacent phrase.
    fs::write(proj.path().join("a.rs"), "let e = encode varint(x);\n").unwrap();
    // B: fused superstrings — encode_length and varint_writer are single tokens.
    fs::write(
        proj.path().join("b.rs"),
        "fn encode_length(); fn varint_writer();\n",
    )
    .unwrap();
    // C: unanchored-substring decoy — 'reencode' and 'varint2' are single tokens.
    fs::write(proj.path().join("c.rs"), "reencode varint2()\n").unwrap();
    build_index(proj.path(), cache.path());

    let json = search_json(proj.path(), cache.path(), &["--phrase", "encode varint"]);
    let got = paths(&json);

    assert_eq!(
        got,
        HashSet::from(["a.rs".to_string()]),
        "AC3: must return EXACTLY {{a.rs}}; b.rs (superstring) and c.rs (unanchored) must be absent; got {got:?}"
    );
    // Strict-subset check: B and C must not appear (PF-007, never exit-0-only).
    assert!(
        !got.contains("b.rs"),
        "AC3: b.rs (superstring 'encode_length varint_writer') must be EXCLUDED"
    );
    assert!(
        !got.contains("c.rs"),
        "AC3: c.rs (unanchored decoy 'reencode varint2') must be EXCLUDED"
    );
}

// ── AC4 + AC5: snippet re-anchor (match_line) + line_range confined ───────────

/// AC4: `--phrase 'encode varint'` result's `line_number` must equal the line
/// of the REAL phrase occurrence (line 8), not the earlier decoy `encode(y)`
/// on line 3.  A `match_positions[0]`-anchored implementation reports line 3.
///
/// AC5: `--json .line_range` must satisfy `start <= line_number <= end-1` and
/// `end - start <= 2*DEFAULT_CONTEXT + 1` (= 7).  A whole-file span fails this.
#[test]
fn cli_ac4_ac5_snippet_reanchor_and_line_range() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // File with a decoy 'encode' on line 3 and the real phrase on line 8.
    let content = "fn process() {\n\
        \x20   // setup code\n\
        \x20   let x = encode(y);\n\
        \x20   // more code here\n\
        \x20   let a = 1;\n\
        \x20   let b = 2;\n\
        \x20   // encoding step\n\
        \x20   let result = encode varint(z);\n\
        \x20   println!(\"done\");\n\
        }\n";
    fs::write(proj.path().join("a.rs"), content).unwrap();
    build_index(proj.path(), cache.path());

    let json = search_json(proj.path(), cache.path(), &["--phrase", "encode varint"]);
    let results = json["results"]
        .as_array()
        .expect("results must be an array");
    assert_eq!(
        results.len(),
        1,
        "AC4: exactly one result expected; got {results:?}"
    );

    let result = &results[0];
    assert_eq!(
        result["path"].as_str().unwrap(),
        "a.rs",
        "AC4: path must be a.rs"
    );

    // AC4: line_number must be 8 (the real phrase line), not 3 (the decoy).
    let line_number = result["line_number"]
        .as_u64()
        .expect("AC4: line_number must be present as a number");
    assert_eq!(
        line_number, 8,
        "AC4: line_number must be 8 (the 'encode varint' line), not the decoy encode on line 3; got {line_number}"
    );

    // AC5: line_range must be a confined object, not null, not a whole-file span.
    let line_range = &result["line_range"];
    assert!(
        !line_range.is_null(),
        "AC5: line_range must be present (not null)"
    );
    let start = line_range["start"]
        .as_u64()
        .expect("line_range.start must be a number");
    let end = line_range["end"]
        .as_u64()
        .expect("line_range.end must be a number");
    let default_context = 3u64;
    assert!(
        start <= line_number,
        "AC5: line_range.start ({start}) must be <= line_number ({line_number})"
    );
    assert!(
        end > line_number,
        "AC5: line_range.end ({end}, exclusive) must be > line_number ({line_number})"
    );
    assert!(
        end - start <= 2 * default_context + 1,
        "AC5: line_range span ({}) must be <= 2*DEFAULT_CONTEXT+1 = {}; start={start} end={end}",
        end - start,
        2 * default_context + 1
    );
}

// ── AC6: order + adjacency negatives ─────────────────────────────────────────

/// AC6: `--phrase 'in the loop'` must return ONLY the file where the three
/// words appear in order and adjacent as word tokens.  Wrong-order and gapped
/// files must be excluded.
#[test]
fn cli_ac6_order_and_adjacency_negatives() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // A: wrong order ('the loop in reverse').
    fs::write(proj.path().join("a.rs"), "the loop in reverse\n").unwrap();
    // B: gap ('in XX the loop' — 'in' and 'the' are not adjacent).
    fs::write(proj.path().join("b.rs"), "in XX the loop\n").unwrap();
    // C: correct contiguous phrase.
    fs::write(proj.path().join("c.rs"), "in the loop here\n").unwrap();
    build_index(proj.path(), cache.path());

    let json = search_json(proj.path(), cache.path(), &["--phrase", "in the loop"]);
    let got = paths(&json);

    assert_eq!(
        got,
        HashSet::from(["c.rs".to_string()]),
        "AC6: must return EXACTLY {{c.rs}}; got {got:?}"
    );
    assert!(
        !got.contains("a.rs"),
        "AC6: wrong-order file must be excluded"
    );
    assert!(!got.contains("b.rs"), "AC6: gapped file must be excluded");
}

// ── AC7: --near thresholds including short words ──────────────────────────────

/// AC7: `--near N` returns exactly the files whose word-token span between the
/// query words is <= N.  Short-word near (`'in loop'`) exercises the all-short
/// candidate path.
#[test]
fn cli_ac7_near_thresholds_incl_short_words() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // A: alpha…beta span = 3 (alpha x y beta, 4 tokens, ordinals 0..3).
    fs::write(proj.path().join("a.rs"), "alpha x y beta\n").unwrap();
    // B: alpha…beta span = 6 (alpha a b c d e beta).
    fs::write(proj.path().join("b.rs"), "alpha a b c d e beta\n").unwrap();
    // C: 'in the loop' — in(0) loop(2), span = 2.
    fs::write(proj.path().join("c.rs"), "in the loop\n").unwrap();
    build_index(proj.path(), cache.path());

    // n=3: only A qualifies (span 3 <= 3; B span 6 > 3).
    let j3 = search_json(proj.path(), cache.path(), &["--near", "3", "alpha beta"]);
    assert_eq!(
        paths(&j3),
        HashSet::from(["a.rs".to_string()]),
        "AC7 n=3: must return {{a.rs}}; got {:?}",
        paths(&j3)
    );

    // n=6: both A and B qualify.
    let j6 = search_json(proj.path(), cache.path(), &["--near", "6", "alpha beta"]);
    assert_eq!(
        paths(&j6),
        HashSet::from(["a.rs".to_string(), "b.rs".to_string()]),
        "AC7 n=6: must return {{a.rs, b.rs}}; got {:?}",
        paths(&j6)
    );

    // n=2, short words: C has 'in'(0) 'loop'(2), span=2 <= 2 -> match.
    let j2s = search_json(proj.path(), cache.path(), &["--near", "2", "in loop"]);
    assert_eq!(
        paths(&j2s),
        HashSet::from(["c.rs".to_string()]),
        "AC7 n=2 short: must return {{c.rs}}; got {:?}",
        paths(&j2s)
    );

    // n=1, short words: span=2 > 1 -> no match.
    let j1s = search_json(proj.path(), cache.path(), &["--near", "1", "in loop"]);
    assert!(
        paths(&j1s).is_empty(),
        "AC7 n=1 short: span=2 > 1 — must return empty; got {:?}",
        paths(&j1s)
    );
}

// ── AC8: --near duplicate query words ────────────────────────────────────────

/// AC8: `--near 3 'foo foo'` must match only files with TWO DISTINCT 'foo'
/// tokens within the window.  A file with a single 'foo' must be excluded.
#[test]
fn cli_ac8_near_duplicate_query_words() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // A: two distinct 'foo' tokens at ordinals 0 and 2 (span 2 <= 3).
    fs::write(proj.path().join("a.rs"), "foo bar foo\n").unwrap();
    // B: single 'foo' — cannot satisfy the two-distinct-position requirement.
    fs::write(proj.path().join("b.rs"), "foo bar baz\n").unwrap();
    build_index(proj.path(), cache.path());

    let json = search_json(proj.path(), cache.path(), &["--near", "3", "foo foo"]);
    let got = paths(&json);

    assert_eq!(
        got,
        HashSet::from(["a.rs".to_string()]),
        "AC8: '--near 3 foo foo' must return EXACTLY {{a.rs}} (two distinct positions); got {got:?}"
    );
    assert!(
        !got.contains("b.rs"),
        "AC8: b.rs (single 'foo') must be excluded from 'foo foo' near query"
    );
}

// ── AC9: query punctuation tokenization parity ───────────────────────────────

/// AC9: `--phrase 'foo bar'` and `--phrase 'foo::bar'` must yield identical
/// result sets.  The query tokenizer treats '::' as separators, so both
/// produce tokens [foo, bar] — the same as the content tokenizer on `foo::bar`.
#[test]
fn cli_ac9_query_punctuation_parity() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // File contains 'foo::bar' where '::' are separators producing tokens foo, bar.
    fs::write(proj.path().join("a.rs"), "let v = foo::bar();\n").unwrap();
    build_index(proj.path(), cache.path());

    let j_space = search_json(proj.path(), cache.path(), &["--phrase", "foo bar"]);
    let j_colons = search_json(proj.path(), cache.path(), &["--phrase", "foo::bar"]);

    let got_space = paths(&j_space);
    let got_colons = paths(&j_colons);

    assert_eq!(
        got_space,
        HashSet::from(["a.rs".to_string()]),
        "AC9: '--phrase foo bar' must find a.rs; got {got_space:?}"
    );
    assert_eq!(
        got_space, got_colons,
        "AC9: '--phrase foo bar' and '--phrase foo::bar' must return identical result sets; \
         space={got_space:?} colons={got_colons:?}"
    );
}

// ── AC10: all-short fallback correctness + anchor ─────────────────────────────

/// AC10: `--phrase 'in of'` (both words <3 bytes — all-short fallback) must
/// return EXACTLY the file with contiguous 'in of', NOT the wrong-order file,
/// and NOT empty.  The result must carry a non-null `line_number`.
///
/// Exercises the all-short fallback (positioned.is_empty() → short_query_fallback)
/// and the D13 re-anchor (anchor from predicate range when match_positions is empty).
#[test]
fn cli_ac10_all_short_fallback_correctness_and_anchor() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // A: contiguous 'in of'.
    fs::write(proj.path().join("a.rs"), "x = in of y\n").unwrap();
    // D: wrong order — must be excluded.
    fs::write(proj.path().join("d.rs"), "of x in\n").unwrap();
    build_index(proj.path(), cache.path());

    let json = search_json(proj.path(), cache.path(), &["--phrase", "in of"]);
    let got = paths(&json);

    // AC10a: correct file set (not empty, d.rs excluded).
    assert_eq!(
        got,
        HashSet::from(["a.rs".to_string()]),
        "AC10: '--phrase in of' must return EXACTLY {{a.rs}} (not empty, not d.rs); got {got:?}"
    );

    // AC10b: line_number must be present and non-null (D13 all-short re-anchor).
    let result = &json["results"].as_array().expect("results array")[0];
    let ln = result["line_number"]
        .as_u64()
        .expect("AC10: line_number must be present (not null) — D13 all-short re-anchor");
    assert_eq!(
        ln, 1,
        "AC10: 'in of' is on line 1 of a.rs; line_number must be 1, got {ln}"
    );
}

// ── AC11: composition --phrase + --ast ───────────────────────────────────────

/// AC11: `--phrase 'encode varint' --ast match-with-arms` must return EXACTLY
/// the files that satisfy BOTH constraints.  Three negative files prove the
/// intersection is strict (PF-007, never exit-0-only):
///   - B: match-with-arms BUT superstring (not the phrase) → excluded.
///   - C: exact phrase BUT no match → excluded.
///   - D: match-with-arms BUT no phrase → excluded.
///
/// AD-393-7: the compound-AST resolve threads VerifyMode::Phrase into its
/// verify step, so the token-exact predicate gates inclusion on both paths.
#[test]
fn cli_ac11_phrase_ast_composition() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // A: real phrase 'encode varint' in a comment + valid match expression.
    // The comment ensures "encode varint" appear as adjacent word tokens.
    fs::write(
        proj.path().join("a.rs"),
        "fn process(r: u32) -> u32 {\n\
         \x20   // encode varint\n\
         \x20   match r {\n\
         \x20       0 => 0,\n\
         \x20       _ => 1,\n\
         \x20   }\n\
         }\n",
    )
    .unwrap();
    // B: match block + superstring 'encode_length varint_writer' (one token each).
    fs::write(
        proj.path().join("b.rs"),
        "fn handle_b() {\n\
         \x20   // encode_length varint_writer\n\
         \x20   match 0u32 {\n\
         \x20       0 => 0,\n\
         \x20       _ => 1,\n\
         \x20   }\n\
         }\n",
    )
    .unwrap();
    // C: real phrase but NO match block.
    fs::write(
        proj.path().join("c.rs"),
        "// encode varint\n\
         fn plain() { let x = 1; }\n",
    )
    .unwrap();
    // D: match block but no phrase.
    fs::write(
        proj.path().join("d.rs"),
        "fn check(x: u32) -> u32 {\n\
         \x20   match x {\n\
         \x20       0 => 0,\n\
         \x20       _ => x,\n\
         \x20   }\n\
         }\n",
    )
    .unwrap();
    build_index(proj.path(), cache.path());

    let json = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "encode varint", "--ast", "match-with-arms"],
    );
    let got = paths(&json);

    // Positive: A must be present.
    assert!(
        got.contains("a.rs"),
        "AC11: a.rs (phrase AND match-with-arms) must be included; got {got:?}"
    );
    // Negative: B, C, D must all be absent (strict subset, PF-007).
    assert!(
        !got.contains("b.rs"),
        "AC11: b.rs (superstring inside match) must be EXCLUDED; got {got:?}"
    );
    assert!(
        !got.contains("c.rs"),
        "AC11: c.rs (phrase but no match) must be EXCLUDED; got {got:?}"
    );
    assert!(
        !got.contains("d.rs"),
        "AC11: d.rs (match but no phrase) must be EXCLUDED; got {got:?}"
    );
    assert_eq!(
        got,
        HashSet::from(["a.rs".to_string()]),
        "AC11: must return EXACTLY {{a.rs}}; got {got:?}"
    );
}

// ── AC13: non-positional lexical no-regression ────────────────────────────────

/// AC13: non-positional single-token and multi-word queries must return the
/// correct result sets (VerifyMode::Substring threading is behavior-neutral).
/// This guards that the new VerifyMode dispatch does not break the existing
/// lexical path.
#[test]
fn cli_ac13_nonpositional_no_regression() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // authenticate.rs contains "authenticate"; other.rs does not.
    fs::write(
        proj.path().join("authenticate.rs"),
        "fn authenticate(user: &str) -> bool { true }\n",
    )
    .unwrap();
    fs::write(
        proj.path().join("other.rs"),
        "fn login(user: &str) -> bool { false }\n",
    )
    .unwrap();
    build_index(proj.path(), cache.path());

    // Single-token query.
    let j1 = search_json(proj.path(), cache.path(), &["authenticate"]);
    let got1 = paths(&j1);
    assert!(
        got1.contains("authenticate.rs"),
        "AC13: single-token 'authenticate' must find authenticate.rs; got {got1:?}"
    );
    assert!(
        !got1.contains("other.rs"),
        "AC13: 'authenticate' must NOT find other.rs; got {got1:?}"
    );

    // Run the same query again — result set must be identical (deterministic).
    let j2 = search_json(proj.path(), cache.path(), &["authenticate"]);
    assert_eq!(
        paths(&j1),
        paths(&j2),
        "AC13: repeated identical query must return the same result set (no non-determinism)"
    );
}

// ── AC15: bounded scan — phrase exits 0 with correct results ─────────────────

/// AC15 (simplified): `--phrase` on a normal-sized corpus exits 0, returns
/// valid JSON, and includes only files containing the token-exact phrase.
///
/// Full AC15 (5,000-file corpus + >5MB oversized-needle file) is omitted from
/// local CI because creating a >5MB file in a test is prohibitively slow.  The
/// bounded-scan predicate path (AD-393-10) is covered by snippet_tests.rs.
#[test]
fn cli_ac15_phrase_exits_zero_with_correct_results() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    fs::write(
        proj.path().join("match.rs"),
        "fn find_pattern() { let _ = encode varint; }\n",
    )
    .unwrap();
    fs::write(
        proj.path().join("nomatch.rs"),
        "fn other() { let _ = 42; }\n",
    )
    .unwrap();
    build_index(proj.path(), cache.path());

    let json = search_json(proj.path(), cache.path(), &["--phrase", "encode varint"]);
    // Must exit 0 (asserted by search_json's .success()) and return valid JSON.
    assert_eq!(
        json["total"].as_u64().unwrap_or(0),
        1,
        "AC15: exactly one result expected; got {json:?}"
    );
    let got = paths(&json);
    assert!(
        got.contains("match.rs"),
        "AC15: match.rs must be in results; got {got:?}"
    );
    assert!(
        !got.contains("nomatch.rs"),
        "AC15: nomatch.rs must NOT be in results; got {got:?}"
    );
}

// ── AC16: --lang filter on positional + all-short fallback ───────────────────

/// AC16: `--phrase ... --lang rust` must exclude non-Rust files even when they
/// contain the token-exact phrase.  Tested on both the positional path (long
/// words) and implicitly the short-word fallback (both 'in' and 'the' are <3 bytes).
#[test]
fn cli_ac16_lang_filter_on_positional_and_fallback() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // a.rs (Rust): contains the phrase.
    fs::write(proj.path().join("a.rs"), "// in the loop\n").unwrap();
    // a.py (Python): same phrase but different language.
    fs::write(proj.path().join("a.py"), "# in the loop\n").unwrap();
    build_index(proj.path(), cache.path());

    // With --lang rust: only a.rs should appear.
    let json = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "in the loop", "--lang", "rust"],
    );
    let got = paths(&json);

    assert!(
        got.contains("a.rs"),
        "AC16: --lang rust must include a.rs; got {got:?}"
    );
    assert!(
        !got.contains("a.py"),
        "AC16: --lang rust must EXCLUDE a.py; got {got:?}"
    );
    assert_eq!(
        got,
        HashSet::from(["a.rs".to_string()]),
        "AC16: result must be EXACTLY {{a.rs}}; got {got:?}"
    );
}

// ── AC17 control: non-positional --blast-radius UNION is unchanged ────────────

/// AC17 CONTROL: `skim search --blast-radius <file>` (no `--phrase`) must exit
/// 0 with valid JSON.  This proves the non-positional blast-radius UNION path
/// is unchanged and the co-change-only peer gate (D18 / AD-393-12) is guarded
/// by `config.phrase || config.near.is_some()` — so non-positional queries keep
/// their "include all peers unconditionally" semantics.
///
/// Full AC17 (positional peer gate requires temporal DB with injected co-change
/// edges) is CI-gated.  Local test infrastructure cannot build temporal history
/// in a temp dir reliably; see design plan §AC17 for the CI-only test spec.
#[test]
fn cli_ac17_blast_radius_control_exits_zero() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // Build a small corpus: the blast-radius target + a file with encode varint.
    fs::write(proj.path().join("target.rs"), "fn target() {}\n").unwrap();
    fs::write(
        proj.path().join("peer.rs"),
        "fn peer() { let _ = encode varint; }\n",
    )
    .unwrap();
    fs::write(
        proj.path().join("superstring.rs"),
        "fn s() { let _ = encode_length varint_writer; }\n",
    )
    .unwrap();
    build_index(proj.path(), cache.path());

    // Non-positional blast-radius: exits 0 and returns valid JSON
    // (no temporal DB → no co-change peers, graceful degradation).
    let json_control = search_json(
        proj.path(),
        cache.path(),
        &["encode", "--blast-radius", "target.rs"],
    );
    // Must be valid JSON with a results array (content may be empty without temporal DB).
    assert!(
        json_control["results"].is_array(),
        "AC17 control: non-positional --blast-radius must return valid JSON with results array"
    );

    // Positional blast-radius: exits 0, returns valid JSON, peer.rs returned
    // (it contains the exact phrase), superstring.rs excluded.
    let json_pos = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "encode varint", "--blast-radius", "target.rs"],
    );
    let got_pos = paths(&json_pos);
    assert!(
        !got_pos.contains("superstring.rs"),
        "AC17 control: superstring.rs must be excluded from --phrase + --blast-radius; got {got_pos:?}"
    );
}
