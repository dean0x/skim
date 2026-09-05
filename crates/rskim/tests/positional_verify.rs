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

use rskim_search::{near_tokens_present, phrase_near_tokens_present, phrase_tokens_present};

// Imports used by CLI integration tests below.
use assert_cmd::Command;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::ops::Range;
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

/// AC3: phrase match returns a byte range covering the exact matched phrase.
#[test]
fn phrase_returns_correct_byte_range_ac3() {
    let content = "hello world foo bar baz";
    // "foo bar" is at bytes 12..19 in this string.
    let result = phrase_tokens_present(content, "foo bar");
    let range = result.expect("AC3: 'foo bar' must match");
    // Validate BOTH start and end — `range.end` drives compute_line_range re-anchoring
    // (AD-393-3/D9); pinning it prevents a regression where end points past the match.
    // Previously only range.start was checked (the slice-to-end assertion passed
    // incidentally because "foo bar" was the last token pair — adding trailing text
    // would break it despite a correct range).
    assert_eq!(
        &content[range.clone()],
        "foo bar",
        "AC3: range must point to the exact span of 'foo bar' (start={}, end={})",
        range.start,
        range.end,
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

/// AC14: `phrase_tokens_present` is case-sensitive byte-exact (AD-355-3).
///
/// A future `.to_lowercase()` normalization would make both assertions pass
/// when they should differ — this test is the discriminating guard.
#[test]
fn phrase_case_sensitive_ac14() {
    // Wrong case → no match.
    let wrong_case = phrase_tokens_present("fn Encode varint foo", "encode varint");
    assert!(
        wrong_case.is_none(),
        "AC14: 'Encode varint' must NOT match query 'encode varint' (case-sensitive)"
    );

    // Exact case → match.
    let exact_case = phrase_tokens_present("fn encode varint foo", "encode varint");
    assert!(
        exact_case.is_some(),
        "AC14: 'encode varint' must match query 'encode varint' (case-sensitive)"
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

/// AC14: `near_tokens_present` is case-sensitive byte-exact (AD-355-3).
///
/// A future `.to_lowercase()` normalization would make both assertions pass
/// when they should differ — this test is the discriminating guard.
#[test]
fn near_case_sensitive_ac14() {
    // Wrong case → no match.
    let wrong_case = near_tokens_present("fn Encode some varint foo", "encode varint", 3);
    assert!(
        wrong_case.is_none(),
        "AC14: 'Encode ... varint' must NOT match query 'encode varint' (case-sensitive)"
    );

    // Exact case → match.
    let exact_case = near_tokens_present("fn encode some varint foo", "encode varint", 3);
    assert!(
        exact_case.is_some(),
        "AC14: 'encode ... varint' must match query 'encode varint' (case-sensitive)"
    );
}

// ─── phrase_near_tokens_present k==1 branch ───────────────────────────────────

/// k==1 branch of phrase_near_tokens_present: single-word query, word present.
///
/// The k==1 branch returns the leftmost occurrence directly (span is trivially 0).
/// A broken k==1 path (e.g., falling into the greedy multi-word scan with k<2)
/// would either panic (debug_assert fires) or return wrong results.
///
/// Guards finding: ac403_18(a) only asserts the verify_mode label, never that
/// a file is returned or that the range covers the word.
#[test]
fn phrase_near_k1_single_word_present_returns_correct_range() {
    // Single word "encode" present as a standalone token.
    let content = "fn encode varint bytes";
    // word ordinals: fn(0), encode(1), varint(2), bytes(3)
    // k==1 branch: base = d[0][0] = 1, c_words[1] = (3, 9) → "encode"
    let result = phrase_near_tokens_present(content, "encode", 5);
    let range = result.expect("k==1: single-word 'encode' must match");
    assert_eq!(
        &content[range], "encode",
        "k==1: range must cover exactly 'encode' (not more, not less)"
    );
}

/// k==1 branch of phrase_near_tokens_present: single-word query, word absent → None.
#[test]
fn phrase_near_k1_single_word_absent_returns_none() {
    let result = phrase_near_tokens_present("fn bar baz", "encode", 5);
    assert!(
        result.is_none(),
        "k==1: 'encode' absent from content must return None"
    );
}

/// k==1 branch of phrase_near_tokens_present: multiple occurrences → returns LEFTMOST.
///
/// The k==1 branch unconditionally takes d[0][0] (first in ascending ordinal order),
/// which is the leftmost occurrence. A broken implementation taking any other
/// occurrence would produce a range.start that does not match byte 4 below.
#[test]
fn phrase_near_k1_returns_leftmost_occurrence() {
    // "zzz encode yyy encode"
    // word ordinals: zzz(0), encode(1), yyy(2), encode(3)
    // d[0](encode) = [1, 3]; k==1 branch: base = d[0][0] = 1
    // c_words[1] = "encode" at bytes 4..10 (after "zzz ")
    let content = "zzz encode yyy encode";
    let result = phrase_near_tokens_present(content, "encode", 5);
    let range = result.expect("k==1: 'encode' must match");
    assert_eq!(
        range.start, 4,
        "k==1: must return LEFTMOST 'encode' at byte 4, not the second at byte 15; \
         got range.start={}",
        range.start
    );
    assert_eq!(
        &content[range], "encode",
        "k==1: range must cover exactly 'encode'"
    );
}

// ─── word-boundary parity ─────────────────────────────────────────────────────

/// D10: collect_word_spans boundary rule must match word_token_indices.
/// Non-ASCII characters (e.g. é = U+00E9, bytes 0xC3 0xA9) are word SEPARATORS,
/// not word bytes.  The test must make the pass/fail outcome depend on whether
/// é actually splits the surrounding bytes into two tokens.
///
/// Discriminating design:
/// - Content `"caf\u{00e9}bar"` (bytes: c,a,f,0xC3,0xA9,b,a,r) — if é is a
///   separator, this tokenises to ["caf", "bar"]; if é were a word byte it
///   would be one combined token.
/// - Query `"caf bar"` (two tokens ["caf", "bar"]) → Some only when é splits.
/// - Complement: `"cafbar"` (one token "cafbar") must NOT match query "caf bar"
///   (two tokens) — proving that the split is caused by the é separator, not
///   by some generic run-boundary coincidence.
#[test]
fn phrase_non_ascii_treated_as_separator() {
    // POSITIVE: é between two ASCII runs causes them to be separate tokens.
    // If é were a word byte, "caf\u{00e9}bar" would be one token; the two-token
    // query "caf bar" would then NOT match → this assertion would fail.
    let split = phrase_tokens_present("caf\u{00e9}bar", "caf bar");
    assert!(
        split.is_some(),
        "D10: non-ASCII byte (é=0xC3,0xA9) must act as a separator; \
         'caf' and 'bar' must be distinct tokens and 'caf bar' must match \
         'caf\u{00e9}bar'"
    );

    // COMPLEMENT (discriminating): if the é were absent ("cafbar" is one token)
    // then "caf bar" (two tokens) must NOT match.  This confirms that the
    // separator behaviour — not some other coincidence — is what allows the
    // match above.
    let no_split = phrase_tokens_present("cafbar", "caf bar");
    assert!(
        no_split.is_none(),
        "D10: complement — 'cafbar' is one token; two-token query 'caf bar' must NOT match"
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
/// lexical path, including the BM25F candidate-pool branch exercised by
/// multi-word queries (the `else` branch in execute_query).
#[test]
fn cli_ac13_nonpositional_no_regression() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // authenticate.rs: contains "authenticate" AND "impl Iterator".
    // other.rs: contains "login" only — no "authenticate", no "Iterator".
    // iter.rs:  contains "impl Iterator" but NOT "authenticate".
    fs::write(
        proj.path().join("authenticate.rs"),
        "fn authenticate(user: &str) -> bool { true }\nimpl Iterator for Auth {}\n",
    )
    .unwrap();
    fs::write(
        proj.path().join("other.rs"),
        "fn login(user: &str) -> bool { false }\n",
    )
    .unwrap();
    fs::write(
        proj.path().join("iter.rs"),
        "pub struct Iter;\nimpl Iterator for Iter { fn next(&mut self) -> Option<()> { None } }\n",
    )
    .unwrap();
    build_index(proj.path(), cache.path());

    // Single-token query: exercises the pure-lexical path.
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

    // Determinism check — result set must be identical on repeat.
    let j2 = search_json(proj.path(), cache.path(), &["authenticate"]);
    assert_eq!(
        paths(&j1),
        paths(&j2),
        "AC13: repeated identical query must return the same result set (no non-determinism)"
    );

    // Multi-word non-positional query "impl Iterator": exercises the BM25F
    // candidate-pool branch (the `else` arm in execute_query / search that
    // newly threads VerifyMode::Substring).  Both authenticate.rs and iter.rs
    // must be found; other.rs must not.
    let j3 = search_json(proj.path(), cache.path(), &["impl Iterator"]);
    let got3 = paths(&j3);
    assert!(
        got3.contains("authenticate.rs"),
        "AC13: multi-word 'impl Iterator' must find authenticate.rs (contains both words); got {got3:?}"
    );
    assert!(
        got3.contains("iter.rs"),
        "AC13: multi-word 'impl Iterator' must find iter.rs (contains both words); got {got3:?}"
    );
    assert!(
        !got3.contains("other.rs"),
        "AC13: multi-word 'impl Iterator' must NOT find other.rs (contains neither word); got {got3:?}"
    );
}

// ── AC15: bounded scan — phrase exits 0 with correct results ─────────────────

/// AC15 (simplified): `--phrase` on a normal-sized corpus exits 0, returns
/// valid JSON, and includes only files containing the token-exact phrase.
///
/// Full AC15 (5,000-file corpus + >5MB oversized-needle file) is omitted from
/// local CI because creating a >5MB file in a test is prohibitively slow.
/// The bounded-scan predicate path (AD-393-10) is covered by the unit test
/// `extract_snippet_and_verify_phrase_mode_verifies` in snippet_tests.rs.
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
/// contain the token-exact phrase.  Tested on two sub-cases:
/// 1. Long-word phrase ("in the loop"): "the" (3 bytes) and "loop" (4 bytes) are
///    positionable; "in" (2 bytes) is short but the query is NOT all-short.
///    This exercises the positional path where at least one positionable word exists.
/// 2. All-short phrase ("fn of"): BOTH words are <3 bytes so every word is below
///    the trigram threshold — the engine falls through to `short_query_fallback`,
///    which applies lang filtering at a different site.  This exercises the
///    all-short fallback + --lang composition (D17 / AC16 explicit requirement).
#[test]
fn cli_ac16_lang_filter_on_positional_and_fallback() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // a.rs (Rust): contains both test phrases.
    fs::write(proj.path().join("a.rs"), "// in the loop\nfn of() {}\n").unwrap();
    // a.py (Python): same phrases, different language.
    fs::write(proj.path().join("a.py"), "# in the loop\n# fn of\n").unwrap();
    build_index(proj.path(), cache.path());

    // Sub-case 1: long-word phrase, positional path, --lang rust.
    // ("the" = 3 bytes → positionable; "in" = 2 bytes → short but not all-short.)
    let json1 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "in the loop", "--lang", "rust"],
    );
    let got1 = paths(&json1);
    assert!(
        got1.contains("a.rs"),
        "AC16 sub-case 1: --lang rust must include a.rs (positional path); got {got1:?}"
    );
    assert!(
        !got1.contains("a.py"),
        "AC16 sub-case 1: --lang rust must EXCLUDE a.py (positional path); got {got1:?}"
    );
    assert_eq!(
        got1,
        HashSet::from(["a.rs".to_string()]),
        "AC16 sub-case 1: result must be EXACTLY {{a.rs}}; got {got1:?}"
    );

    // Sub-case 2: all-short phrase ("fn" = 2 bytes, "of" = 2 bytes — both below
    // the 3-byte trigram threshold).  The engine takes the short_query_fallback
    // path, which must still honour --lang.
    let json2 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "fn of", "--lang", "rust"],
    );
    let got2 = paths(&json2);
    assert!(
        got2.contains("a.rs"),
        "AC16 sub-case 2: --lang rust must include a.rs (all-short fallback); got {got2:?}"
    );
    assert!(
        !got2.contains("a.py"),
        "AC16 sub-case 2: --lang rust must EXCLUDE a.py (all-short fallback); got {got2:?}"
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

    // PF-007: discriminating positive assertion — peer.rs MUST appear.
    // Without temporal data the lexical UNION layer still yields peer.rs (it
    // contains the exact phrase "encode varint" as adjacent word tokens), and
    // it passes Phrase verification.  An empty result set would satisfy only
    // the superstring.rs exclusion assertion below, hiding a fully-broken path.
    assert!(
        got_pos.contains("peer.rs"),
        "AC17 control: peer.rs (contains exact phrase 'encode varint') must be PRESENT \
         in --phrase + --blast-radius results; got {got_pos:?}"
    );

    // Exclusion assertion: superstring false positive must not appear.
    assert!(
        !got_pos.contains("superstring.rs"),
        "AC17 control: superstring.rs must be excluded from --phrase + --blast-radius; got {got_pos:?}"
    );
}

// ============================================================================
// AC-403: --phrase --near N composition (#403)
// ============================================================================
//
// All E2E tests use isolated SKIM_CACHE_DIR via tempdir() — no writes to
// ~/.cache/skim/.  PF-013 (lib test binary hangs) is avoided by using the
// binary path, not `cargo test -p rskim-search --lib`.
// PF-010 (shared target/ lock) is avoided by running tests one at a time.
//
// Spanlab corpus (used by AC-403-1 through AC-403-4):
//   adj.rs  "// alpha beta gamma"          adjacent, ordered, span=2
//   span.rs "// alpha xx beta xx gamma"    ordered, span=4 (not adjacent)
//   rev.rs  "// gamma xx beta xx alpha"    reversed order, span=4
//   sup.rs  "// alphabet betamax gammaray" superstrings (no whole-word match)
//
// Shortlab corpus (used by AC-403-5):
//   hit.rs  "// in xx of"                  ordered, span=2 (all-short words)
//   far.rs  "// in xx xx xx of"            ordered, span=4 (all-short words)
//   rev.rs  "// of xx in"                  reversed order, span=2 (all-short)

/// Build the spanlab corpus and index.
fn build_spanlab(proj: &Path, cache: &Path) {
    fs::write(proj.join("adj.rs"), "// alpha beta gamma\n").unwrap();
    fs::write(proj.join("span.rs"), "// alpha xx beta xx gamma\n").unwrap();
    fs::write(proj.join("rev.rs"), "// gamma xx beta xx alpha\n").unwrap();
    fs::write(proj.join("sup.rs"), "// alphabet betamax gammaray\n").unwrap();
    build_index(proj, cache);
}

/// Run `skim search <extra_args> --root <proj>` and return the raw process output.
/// Does NOT require --json or successful exit, so it works for notice-only tests.
fn raw_search_output(proj: &Path, cache: &Path, extra_args: &[&str]) -> std::process::Output {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("search")
        .args(extra_args)
        .args(["--root"])
        .arg(proj)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

// ── AC-403-1: composed ordered + total-span semantic ─────────────────────────

/// AC-403-1: --phrase --near N applies ORDERED + TOTAL-SPAN semantics.
///
/// - adj.rs  (span 2): present at --phrase --near 2, --near 3, --near 4
/// - span.rs (span 4): absent at N=2 and N=3, present at N=4
///
/// The N=2 discriminator is the KEY: a per-adjacent-pair implementation
/// (each gap 2) would incorrectly include span.rs at N=2.  Total-span
/// correctly excludes it (total span 4 > 2).
#[test]
fn ac403_1_composed_ordered_total_span_semantic() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    build_spanlab(proj.path(), cache.path());

    // --phrase --near 2: adj.rs only (span 2 <= 2; span.rs has span 4 > 2).
    let j2 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "2", "alpha beta gamma"],
    );
    let got2 = paths(&j2);
    assert!(
        got2.contains("adj.rs"),
        "AC-403-1 N=2: adj.rs (span 2) must be present; got {got2:?}"
    );
    assert!(
        !got2.contains("span.rs"),
        "AC-403-1 N=2: span.rs (span 4) must be ABSENT (total span > N); got {got2:?}. \
         NOTE: if span.rs is present, this is the per-adjacent-pair bug — total-span is required."
    );

    // --phrase --near 3: adj.rs only (span 2 <= 3; span.rs span 4 > 3).
    let j3 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "3", "alpha beta gamma"],
    );
    let got3 = paths(&j3);
    assert!(
        got3.contains("adj.rs"),
        "AC-403-1 N=3: adj.rs (span 2) must be present; got {got3:?}"
    );
    assert!(
        !got3.contains("span.rs"),
        "AC-403-1 N=3: span.rs (span 4) must be ABSENT; got {got3:?}"
    );

    // --phrase --near 4: adj.rs + span.rs (both span <= 4, ordered).
    let j4 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "4", "alpha beta gamma"],
    );
    let got4 = paths(&j4);
    assert!(
        got4.contains("adj.rs"),
        "AC-403-1 N=4: adj.rs must be present; got {got4:?}"
    );
    assert!(
        got4.contains("span.rs"),
        "AC-403-1 N=4: span.rs (span 4) must be present at N=4; got {got4:?}"
    );
    assert!(
        !got4.contains("sup.rs"),
        "AC-403-1 N=4: sup.rs (superstrings) must be ABSENT; got {got4:?}"
    );
    assert!(
        !got4.contains("rev.rs"),
        "AC-403-1 N=4: rev.rs (reversed order) must be ABSENT from --phrase --near 4; got {got4:?}"
    );
}

// ── AC-403-2: identity (--phrase --near (k-1) == --phrase) ───────────────────

/// AC-403-2: For a k-word query, --phrase --near (k-1) MUST produce the same
/// result set as --phrase alone.  k=3, so k-1=2.
///
/// Also verifies the predicate-level identity:
/// phrase_near_tokens_present(content, query, k-1) matches phrase_tokens_present(content, query).
#[test]
fn ac403_2_identity_phrase_near_k_minus_1_equals_phrase() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    build_spanlab(proj.path(), cache.path());

    // CLI identity.
    let j_phrase = search_json(proj.path(), cache.path(), &["--phrase", "alpha beta gamma"]);
    let j_phrase_near_2 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "2", "alpha beta gamma"],
    );
    let got_phrase = paths(&j_phrase);
    let got_pn2 = paths(&j_phrase_near_2);

    assert!(
        !got_phrase.is_empty(),
        "AC-403-2: --phrase must return at least one result (adj.rs); got empty"
    );
    assert_eq!(
        got_phrase, got_pn2,
        "AC-403-2: --phrase and --phrase --near 2 must return the same set for 3-word query; \
         phrase={got_phrase:?} phrase_near_2={got_pn2:?}"
    );

    // Predicate-level identity on adj.rs and span.rs content.
    let adj_content = "// alpha beta gamma\n";
    let span_content = "// alpha xx beta xx gamma\n";
    let query = "alpha beta gamma";

    // adj.rs: adjacent — both phrase and phrase_near(k-1=2) match.
    assert_eq!(
        phrase_tokens_present(adj_content, query).is_some(),
        phrase_near_tokens_present(adj_content, query, 2).is_some(),
        "AC-403-2 predicate identity on adj.rs: phrase and phrase_near(2) must agree"
    );
    // span.rs: gapped — neither phrase nor phrase_near(k-1=2) matches.
    assert_eq!(
        phrase_tokens_present(span_content, query).is_some(),
        phrase_near_tokens_present(span_content, query, 2).is_some(),
        "AC-403-2 predicate identity on span.rs: phrase and phrase_near(2) must agree (both false)"
    );
}

// ── AC-403-2 (D-1 rank-identity gap): short-word query set-identity + rank-divergence ──

/// AC-403-2 (D-1 rank-identity gap): documents and validates the known rank-identity
/// limitation for the D-1 IDENTITY (`--phrase --near(k-1)` == `--phrase`).
///
/// The D-1 identity is **predicate-level** (set membership): after verification both
/// paths produce the same file set.  Reader-level rank order may differ when the query
/// contains sub-3-byte words: `count_phrase_near_alignments` operates on positioned
/// (≥3-byte) words only, using a span bound that is LOOSER than the exact ordinal gaps
/// encoded in `count_phrase_alignments`.
///
/// # Corpus (query: "alpha fn beta", k=3, k-1=2, "fn" is 2 bytes → short/dropped)
///
/// Positioned words: {alpha, beta}.  offsets = [0, 2] (beta must be 2 ordinals after
/// alpha, leaving room for the dropped "fn").
///
/// - a.rs: "alpha fn beta\nalpha fn beta\nalpha fn beta\n"
///   * count_phrase_alignments  = 3  (each pair matches exact gap 2)
///   * count_phrase_near_alignments(span=2) = 3  (same: gap 1<p<2+1, all ≤ ceiling)
///   * verify passes (contains "alpha fn beta")
///
/// - b.rs: "alpha beta\nalpha beta\nalpha beta\nalpha fn beta\n"
///   * count_phrase_alignments  = 1  (only the final "alpha fn beta" pair, gap=2)
///   * count_phrase_near_alignments(span=2) = 4  (three "alpha beta" gap-1 pairs + one gap-2)
///   * verify passes (contains "alpha fn beta")
///
/// # Set-identity (no limit)
///
/// Both paths return {a.rs, b.rs} — the verify gate guarantees set-identity.
///
/// # Rank-divergence with --limit 1
///
/// * `--phrase` ranks by count_phrase_alignments: a.rs(3) > b.rs(1) → **a.rs wins**.
/// * `--phrase --near 2` ranks by count_phrase_near_alignments: b.rs(4) > a.rs(3) → **b.rs wins**.
///
/// With --limit 1 the surviving sets differ: {a.rs} vs {b.rs}.  This is **expected,
/// documented behavior** — the D-1 guarantee is set-identity, not rank-identity.
/// See `count_phrase_near_alignments` (reader.rs) and `phrase_near_tokens_present`
/// (types.rs) for the full design rationale.
#[test]
fn ac403_2_d1_rank_identity_gap_short_word() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // a.rs: three exact "alpha fn beta" phrases.
    // count_phrase_alignments = 3; count_phrase_near_alignments(span=2) = 3.
    fs::write(
        proj.path().join("a.rs"),
        "alpha fn beta\nalpha fn beta\nalpha fn beta\n",
    )
    .unwrap();

    // b.rs: three "alpha beta" pairs (gap 1) + one "alpha fn beta" (gap 2).
    // count_phrase_alignments = 1 (only the exact gap-2 pair).
    // count_phrase_near_alignments(span=2) = 4 (three gap-1 pairs count too).
    fs::write(
        proj.path().join("b.rs"),
        "alpha beta\nalpha beta\nalpha beta\nalpha fn beta\n",
    )
    .unwrap();

    build_index(proj.path(), cache.path());

    // ── Part 1: set-identity (no limit) ──────────────────────────────────────
    //
    // Verify that the D-1 set-identity holds: both --phrase and --phrase --near 2
    // return the same set {a.rs, b.rs} when no limit is applied.
    let j_phrase = search_json(proj.path(), cache.path(), &["--phrase", "alpha fn beta"]);
    let j_near2 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "2", "alpha fn beta"],
    );
    let phrase_set = paths(&j_phrase);
    let near2_set = paths(&j_near2);

    assert!(
        phrase_set.contains("a.rs") && phrase_set.contains("b.rs"),
        "AC-403-2 D-1 set-identity: --phrase must return both a.rs and b.rs; got {phrase_set:?}"
    );
    assert_eq!(
        phrase_set, near2_set,
        "AC-403-2 D-1 set-identity: --phrase and --phrase --near 2 must return the same set \
         (set-identity guaranteed by the verify gate); phrase={phrase_set:?} near2={near2_set:?}"
    );

    // ── Part 2: rank-divergence with tight --limit (documented known gap) ────
    //
    // With --limit 1, the rank order from the reader determines which file survives.
    // --phrase ranks by count_phrase_alignments: a.rs(3) > b.rs(1) → a.rs.
    // --phrase --near 2 ranks by count_phrase_near_alignments: b.rs(4) > a.rs(3) → b.rs.
    // This is the D-1 rank-identity gap: set-identity holds without limit, but tight
    // limits expose reader-score divergence for short-word queries.
    let j_phrase_limit1 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--limit", "1", "alpha fn beta"],
    );
    let j_near2_limit1 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "2", "--limit", "1", "alpha fn beta"],
    );

    let phrase_limit1_path = j_phrase_limit1["results"][0]["path"]
        .as_str()
        .expect("AC-403-2 D-1 rank gap: --phrase --limit 1 must return a result")
        .to_string();
    let near2_limit1_path = j_near2_limit1["results"][0]["path"]
        .as_str()
        .expect("AC-403-2 D-1 rank gap: --phrase --near 2 --limit 1 must return a result")
        .to_string();

    // --phrase ranks by exact phrase count (gap=2 only): a.rs wins (score 3 vs 1).
    assert_eq!(
        phrase_limit1_path, "a.rs",
        "AC-403-2 D-1 rank gap: --phrase --limit 1 must return a.rs (score 3 > 1 from \
         count_phrase_alignments with offsets [0,2]); got {phrase_limit1_path:?}"
    );
    // --phrase --near 2 ranks by loose near count (gap 1 or 2): b.rs wins (score 4 vs 3).
    // This divergence from --phrase is the known rank-identity gap — NOT a correctness bug
    // (both returned files genuinely contain 'alpha fn beta', verify passes for both).
    assert_eq!(
        near2_limit1_path, "b.rs",
        "AC-403-2 D-1 rank gap: --phrase --near 2 --limit 1 must return b.rs (score 4 > 3 \
         from count_phrase_near_alignments counting 'alpha beta' gap-1 pairs); \
         got {near2_limit1_path:?}. \
         If both return the same file, the rank-identity gap was closed — update this test \
         to document the new behavior."
    );
}

// ── AC-403-3: monotonicity ────────────────────────────────────────────────────

/// AC-403-3: --phrase --near N MUST NOT return any file that --near N does not
/// return.  Adding --phrase (ordering constraint) may only narrow, never grow.
///
/// At N=4 on the spanlab corpus:
///   --near 4        returns {adj.rs, span.rs, rev.rs}  (unordered, any span<=4)
///   --phrase --near 4  returns {adj.rs, span.rs}       (ordered, span<=4)
///   --phrase        returns {adj.rs}                   (adjacent, ordered)
///
/// Monotonicity chain: phrase_near_4 ⊆ near_4 and phrase ⊆ phrase_near_4.
#[test]
fn ac403_3_monotonicity() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    build_spanlab(proj.path(), cache.path());

    let j_near4 = search_json(
        proj.path(),
        cache.path(),
        &["--near", "4", "alpha beta gamma"],
    );
    let j_pn4 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "4", "alpha beta gamma"],
    );
    let j_phrase = search_json(proj.path(), cache.path(), &["--phrase", "alpha beta gamma"]);

    let near4 = paths(&j_near4);
    let pn4 = paths(&j_pn4);
    let phrase = paths(&j_phrase);

    // All sets must be non-empty (AC-403-3 requires a discriminating corpus).
    assert!(
        !near4.is_empty(),
        "AC-403-3: --near 4 must return >= 1 file"
    );
    assert!(
        !pn4.is_empty(),
        "AC-403-3: --phrase --near 4 must return >= 1 file"
    );
    assert!(
        !phrase.is_empty(),
        "AC-403-3: --phrase must return >= 1 file"
    );

    // Monotonicity: pn4 ⊆ near4.
    for file in &pn4 {
        assert!(
            near4.contains(file.as_str()),
            "AC-403-3: {file} is in --phrase --near 4 but NOT in --near 4 — MONOTONICITY VIOLATED"
        );
    }

    // Monotonicity: phrase ⊆ pn4.
    for file in &phrase {
        assert!(
            pn4.contains(file.as_str()),
            "AC-403-3: {file} is in --phrase but NOT in --phrase --near 4 — MONOTONICITY VIOLATED"
        );
    }

    // Strict-subset check: rev.rs (reversed order) must be in near4 but NOT pn4.
    assert!(
        near4.contains("rev.rs"),
        "AC-403-3: rev.rs (reversed order) must be in --near 4; got {near4:?}"
    );
    assert!(
        !pn4.contains("rev.rs"),
        "AC-403-3: rev.rs (reversed order) must be ABSENT from --phrase --near 4; got {pn4:?}"
    );
}

// ── AC-403-4: order enforcement + superstring rejection ──────────────────────

/// AC-403-4: --phrase --near N must exclude files whose query words appear only
/// out of query order (rev.rs) and superstring files (sup.rs), at any N.
#[test]
fn ac403_4_order_enforcement_and_superstring_rejection() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    build_spanlab(proj.path(), cache.path());

    // rev.rs must be absent from --phrase --near 4 (reversed order).
    let j4 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "4", "alpha beta gamma"],
    );
    let got4 = paths(&j4);
    assert!(
        !got4.contains("rev.rs"),
        "AC-403-4: rev.rs (gamma xx beta xx alpha — reversed) must be ABSENT at --phrase --near 4; got {got4:?}"
    );

    // rev.rs must be absent at any large N (order cannot flip).
    let j100 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "100", "alpha beta gamma"],
    );
    let got100 = paths(&j100);
    assert!(
        !got100.contains("rev.rs"),
        "AC-403-4: rev.rs must be ABSENT at --phrase --near 100; got {got100:?}"
    );

    // sup.rs must be absent (superstrings: 'alphabet' != token 'alpha').
    assert!(
        !got4.contains("sup.rs"),
        "AC-403-4: sup.rs (alphabet/betamax/gammaray superstrings) must be ABSENT; got {got4:?}"
    );

    // sup.rs absent from pure --phrase too.
    let j_phrase = search_json(proj.path(), cache.path(), &["--phrase", "alpha beta gamma"]);
    assert!(
        !paths(&j_phrase).contains("sup.rs"),
        "AC-403-4: sup.rs must be ABSENT from --phrase; got {:?}",
        paths(&j_phrase)
    );

    // rev.rs absent from pure --phrase too (reversed order, not adjacent).
    assert!(
        !paths(&j_phrase).contains("rev.rs"),
        "AC-403-4: rev.rs must be ABSENT from --phrase; got {:?}",
        paths(&j_phrase)
    );
}

// ── AC-403-5: all-short-word path (words < 3 bytes) ──────────────────────────

/// AC-403-5: --phrase --near N on a query where all words are < 3 bytes.
///
/// "in" and "of" are each 2 bytes — they cannot form a trigram.  The reader
/// falls back to the all-files candidate set with score 0.0 (AD-355-7).
/// The per-file verify predicate phrase_near_tokens_present still enforces the
/// ordered + total-span contract.
///
/// Shortlab corpus:
///   hit.rs  "// in xx of"       span=2  (in=0, xx=1, of=2)
///   far.rs  "// in xx xx xx of" span=4  (in=0, of=4)
///   rev.rs  "// of xx in"       reversed
#[test]
fn ac403_5_all_short_word_path() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    fs::write(proj.path().join("hit.rs"), "// in xx of\n").unwrap();
    fs::write(proj.path().join("far.rs"), "// in xx xx xx of\n").unwrap();
    fs::write(proj.path().join("rev.rs"), "// of xx in\n").unwrap();
    build_index(proj.path(), cache.path());

    // --phrase --near 2: hit.rs (span=2 <= 2, ordered). far.rs (span=4 > 2) out.
    let j2 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "2", "in of"],
    );
    let got2 = paths(&j2);
    assert!(
        got2.contains("hit.rs"),
        "AC-403-5: hit.rs (span 2) must be present at --phrase --near 2; got {got2:?}"
    );
    assert!(
        !got2.contains("far.rs"),
        "AC-403-5: far.rs (span 4 > 2) must be ABSENT at --phrase --near 2; got {got2:?}"
    );
    assert!(
        !got2.contains("rev.rs"),
        "AC-403-5: rev.rs (reversed) must be ABSENT at --phrase --near 2; got {got2:?}"
    );

    // --phrase --near 1: empty (hit.rs span=2 > 1).
    let j1 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "1", "in of"],
    );
    let got1 = paths(&j1);
    assert!(
        got1.is_empty(),
        "AC-403-5: --phrase --near 1 must return empty (minimum span is 2); got {got1:?}"
    );

    // --near 4: hit.rs + far.rs (span <= 4, any order). rev.rs has span=2 <= 4, also included.
    let j_near4 = search_json(proj.path(), cache.path(), &["--near", "4", "in of"]);
    let got_near4 = paths(&j_near4);
    assert!(
        got_near4.contains("far.rs"),
        "AC-403-5: far.rs must be present in --near 4; got {got_near4:?}"
    );

    // AD-355-7: all-short fallback score should be 0.0 for hit.rs.
    let hit_result = j2["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["path"].as_str().unwrap_or("") == "hit.rs");
    if let Some(hit) = hit_result {
        let score = hit["score"].as_f64().unwrap_or(1.0);
        assert!(
            score == 0.0,
            "AC-403-5 AD-355-7: all-short-word fallback score must be 0.0; got {score}"
        );
    }
}

// ── AC-403-6: inert-flag notice on non-text arms ──────────────────────────────

/// AC-403-6: --phrase or --near on any non-text arm (--build, standalone temporal,
/// etc.) must emit a notice to stderr.  Exit code is 0.  stdout is unchanged.
///
/// Tested arms: --build and standalone --hot (no text query).
#[test]
fn ac403_6_inert_flag_notice_on_nontex_arms() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // --phrase --build: notice fires, build runs, exit 0.
    let out_build = raw_search_output(proj.path(), cache.path(), &["--phrase", "--build"]);
    let stderr_build = String::from_utf8(out_build.stderr).unwrap();
    assert!(
        out_build.status.success(),
        "AC-403-6: --phrase --build must exit 0; stderr: {stderr_build}"
    );
    assert!(
        stderr_build.contains("skim search: note:"),
        "AC-403-6: --phrase --build must emit an inert-flag notice to stderr; got: {stderr_build:?}"
    );
    assert!(
        stderr_build.contains("--phrase"),
        "AC-403-6: notice must name the --phrase flag; got: {stderr_build:?}"
    );

    // --near 5 --build: notice fires for --near too.
    let out_near_build = raw_search_output(proj.path(), cache.path(), &["--near", "5", "--build"]);
    let stderr_near = String::from_utf8(out_near_build.stderr).unwrap();
    assert!(
        out_near_build.status.success(),
        "AC-403-6: --near 5 --build must exit 0; stderr: {stderr_near}"
    );
    assert!(
        stderr_near.contains("skim search: note:"),
        "AC-403-6: --near 5 --build must emit an inert-flag notice; got: {stderr_near:?}"
    );

    // --phrase --near 5 --build: combined notice.
    let out_both = raw_search_output(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "5", "--build"],
    );
    let stderr_both = String::from_utf8(out_both.stderr).unwrap();
    assert!(
        out_both.status.success(),
        "AC-403-6: --phrase --near 5 --build must exit 0; stderr: {stderr_both}"
    );
    assert!(
        stderr_both.contains("skim search: note:"),
        "AC-403-6: --phrase --near 5 --build must emit notice; got: {stderr_both:?}"
    );
}

// ── AC-403-7: notice does not over-fire ──────────────────────────────────────

/// AC-403-7: --phrase or --near MUST NOT emit the inert-flag notice when a
/// text query IS present (the flags are active, not inert).
#[test]
fn ac403_7_notice_does_not_overfire() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // Build a minimal corpus.
    fs::write(proj.path().join("a.rs"), "fn alpha_beta() {}\n").unwrap();
    build_index(proj.path(), cache.path());

    // --phrase + text: no inert-flag notice.
    let out_phrase = raw_search_output(proj.path(), cache.path(), &["--phrase", "--json", "alpha"]);
    let stderr_phrase = String::from_utf8(out_phrase.stderr).unwrap();
    assert!(
        !stderr_phrase.contains("has no effect"),
        "AC-403-7: --phrase + text must NOT emit inert notice; stderr: {stderr_phrase:?}"
    );

    // --near 3 + 2-word text: no inert-flag notice (>1 word → --near is active).
    // "fn alpha_beta" has two word tokens (fn, alpha_beta) so near_diagnostic_notice
    // returns None (n=3 ≥ word_count-1=1).  Contrast with ac403_8 which asserts the
    // single-word case DOES emit a notice.
    let out_near = raw_search_output(
        proj.path(),
        cache.path(),
        &["--near", "3", "--json", "fn alpha_beta"],
    );
    let stderr_near = String::from_utf8(out_near.stderr).unwrap();
    assert!(
        !stderr_near.contains("has no effect"),
        "AC-403-7: --near 3 + 2-word text must NOT emit inert notice; stderr: {stderr_near:?}"
    );
}

// ── AC-403-8: degenerate --near diagnostic ────────────────────────────────────

/// AC-403-8: structurally-degenerate --near queries (single-word, or
/// N < word_count - 1) emit a stderr notice.  Exit code is 0.
#[test]
fn ac403_8_degenerate_near_diagnostic() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    fs::write(proj.path().join("a.rs"), "fn alpha beta gamma() {}\n").unwrap();
    build_index(proj.path(), cache.path());

    // Single-word query + --near N: degenerate (no other words to be near).
    let out_single = raw_search_output(
        proj.path(),
        cache.path(),
        &["--near", "5", "--json", "alpha"],
    );
    let stderr_single = String::from_utf8(out_single.stderr).unwrap();
    assert!(
        out_single.status.success(),
        "AC-403-8: single-word --near must exit 0; stderr: {stderr_single}"
    );
    assert!(
        stderr_single.contains("skim search: note:"),
        "AC-403-8: single-word --near must emit a diagnostic notice; got: {stderr_single:?}"
    );

    // N < word_count - 1: 3-word query, N=1 < 2 = word_count - 1.
    let out_impossible = raw_search_output(
        proj.path(),
        cache.path(),
        &["--near", "1", "--json", "alpha beta gamma"],
    );
    let stderr_impossible = String::from_utf8(out_impossible.stderr).unwrap();
    assert!(
        out_impossible.status.success(),
        "AC-403-8: impossible --near must exit 0; stderr: {stderr_impossible}"
    );
    assert!(
        stderr_impossible.contains("skim search: note:"),
        "AC-403-8: N < word_count-1 must emit diagnostic notice; got: {stderr_impossible:?}"
    );

    // Control: N == word_count - 1 (satisfiable minimum) — no notice.
    let out_min = raw_search_output(
        proj.path(),
        cache.path(),
        &["--near", "2", "--json", "alpha beta gamma"],
    );
    let stderr_min = String::from_utf8(out_min.stderr).unwrap();
    // Note: the notice check only applies to "cannot match" cases.
    // N=2 is the smallest satisfiable span for a 3-word query.
    assert!(
        !stderr_min.contains("cannot match"),
        "AC-403-8 control: --near 2 on 3-word query is satisfiable; must NOT emit 'cannot match'; \
         got: {stderr_min:?}"
    );
}

// ── AC-403-16: deterministic anchor range (predicate unit) ───────────────────

/// AC-403-16: phrase_near_tokens_present must return the range anchored at
/// the FIRST (leftmost-base) occurrence of the first query word.
///
/// Content: "alpha zzz alpha xx beta"
/// Query:   "alpha beta"
/// n=4:     first alpha at ord 0, beta at ord 4, span=4 <= 4.
///          The range MUST start at byte 0 (first alpha), NOT byte 10 (second alpha).
#[test]
fn ac403_16_deterministic_anchor_range() {
    let content = "alpha zzz alpha xx beta";
    let query = "alpha beta";

    let result = phrase_near_tokens_present(content, query, 4);
    let range = result.expect(
        "AC-403-16: 'alpha ... beta' with n=4 must match (span 4 from first alpha to beta)",
    );

    // Range must start at byte 0 (first alpha), not byte 10 (second alpha).
    assert_eq!(
        range.start, 0,
        "AC-403-16: range must start at byte 0 (first 'alpha'), got start={}; \
         content={content:?}",
        range.start
    );
    // Range must cover all the way to the end of 'beta'.
    // "alpha zzz alpha xx beta" → 'beta' ends at byte 23.
    assert_eq!(
        range.end,
        content.len(),
        "AC-403-16: range.end must cover 'beta' at end of content; \
         got end={}, content_len={}",
        range.end,
        content.len()
    );
    let span_text = &content[range];
    assert!(
        span_text.starts_with("alpha"),
        "AC-403-16: range must start with 'alpha'; got {span_text:?}"
    );
    assert!(
        span_text.ends_with("beta"),
        "AC-403-16: range must end with 'beta'; got {span_text:?}"
    );
}

// ── D-1 multi-base fallback (predicate) ──────────────────────────────────────

/// D-1 greedy multi-base fallback (predicate): the FIRST base in d[0] fails its
/// fixed ceiling, and a LATER base rescues the match.
///
/// Content: `"alpha zzz zzz zzz alpha zzz beta"`
///   word ordinals: alpha(0) zzz(1) zzz(2) zzz(3) alpha(4) zzz(5) beta(6)
///   d[0](alpha) = [0, 4],  d[1](beta) = [6]
///
/// With n=3:
///   base=alpha@0 → ceiling = 0+3 = 3; beta@6 > 3 → **FAIL** (first base fails).
///   base=alpha@4 → ceiling = 4+3 = 7; beta@6 ≤ 7 → **MATCH** (second base rescues).
///
/// A regression that only tries d[0][0] and returns None on failure (no loop
/// continuation) would cause a false-negative here.  Every existing predicate
/// test had the first base succeed, so such a regression would be undetected.
#[test]
fn phrase_near_multi_base_first_fails_second_rescues() {
    let content = "alpha zzz zzz zzz alpha zzz beta";
    //  bytes:      0-4   6-8  10-12 14-16 18-22 24-26 28-31
    //  ordinals:    0     1     2     3     4     5     6

    // n=3: base@0 ceiling=3, beta@6>3 → FAIL; base@4 ceiling=7, beta@6≤7 → MATCH.
    let result = phrase_near_tokens_present(content, "alpha beta", 3);
    let range = result.expect(
        "D-1 multi-base: base alpha@0 fails ceiling 3 (beta@6 > 3); \
         base alpha@4 must rescue (beta@6 ≤ 7); got None — \
         likely the greedy loop does not continue past the first failing base",
    );

    // Range must start at the SECOND alpha (byte 18), not the first (byte 0).
    // If loop breaks on first failure we'd get None (caught above).
    // If range.start==0 the first alpha was used but should have been rejected.
    assert_eq!(
        range.start, 18,
        "D-1 multi-base: range.start must be 18 (second alpha, the rescuing base), \
         not 0 (first alpha, which exceeds its ceiling); got start={}",
        range.start
    );
    assert_eq!(
        range.end, 32,
        "D-1 multi-base: range.end must be 32 (end of 'beta'); got end={}",
        range.end
    );

    // Control: n=1 — both bases fail (beta@6 > 0+1=1 and beta@6 > 4+1=5).
    let control = phrase_near_tokens_present(content, "alpha beta", 1);
    assert!(
        control.is_none(),
        "D-1 multi-base control: n=1, both alpha@0 (ceiling 1) and alpha@4 (ceiling 5) \
         fail (beta@6 > both); got Some"
    );
}

// ── D-1 overflow-widening regression (low risk, but contractual) ─────────────

/// D-1 overflow-widening: `phrase_near_tokens_present` with n == u32::MAX must
/// not panic and must correctly accept ordered pairs at any word-ordinal gap.
///
/// **What this test proves on 64-bit CI (the common CI target):**
/// On a 64-bit host `usize` is 64-bit, so `n as usize` = 4_294_967_295 — well
/// below `usize::MAX`.  `base.saturating_add(n_span)` never actually saturates
/// here, and the 32-bit wrap regression the function name references cannot
/// manifest.  The discriminating value of this test on 64-bit CI is therefore:
///
///   (a) **Span check is active**: n = 2 correctly rejects span-3 (ceiling 2 <
///       ordinal 3), while n = u32::MAX accepts it — proving the ceiling
///       comparison (`p ≤ ceiling`) is actually applied for large-n values and
///       not silently bypassed.  Adjacent-only pairs (span 1) would pass even
///       with n = 1, so they give no evidence the large-n value matters.
///
///   (b) **No panic**: base contract for any u32 input including boundary values.
///
/// **On 32-bit targets (hypothetical):**
/// A cast-before-add regression (e.g. `(base + n as u32) as usize`) would
/// overflow when `base > 0`, producing a tiny ceiling that incorrectly rejects
/// matches.  `saturating_add` clamps to `usize::MAX` and avoids this.  That
/// specific regression cannot be exercised on a 64-bit host; checks (a) and (b)
/// above are the strongest test available on a 64-bit target.
#[test]
fn phrase_near_saturating_add_large_n_no_panic_d1() {
    // Discriminating check (64-bit guard): span-3 pair with n = 2 (reject) vs
    // n = u32::MAX (accept).
    //
    // Content:  "alpha x y beta"  → ordinals alpha(0), x(1), y(2), beta(3)
    // Span:     beta(3) − alpha(0) = 3.
    // n = 2: ceiling = 0.saturating_add(2) = 2; beta@3 > 2 → None.
    // n = u32::MAX: ceiling = 0.saturating_add(u32::MAX as usize) >> 3 → Some.
    let span3 = "alpha x y beta";
    let reject = phrase_near_tokens_present(span3, "alpha beta", 2);
    assert!(
        reject.is_none(),
        "D-1 overflow-widening: n=2 must reject span-3 pair (span check is active)"
    );
    let accept = phrase_near_tokens_present(span3, "alpha beta", u32::MAX);
    assert!(
        accept.is_some(),
        "D-1 overflow-widening: n=u32::MAX must accept span-3 pair without panic"
    );

    // Adjacent pair: original no-panic smoke check (span 1 ≤ u32::MAX, trivially passes).
    let adj = phrase_near_tokens_present("alpha beta", "alpha beta", u32::MAX);
    assert!(
        adj.is_some(),
        "D-1 overflow-widening: n=u32::MAX must not panic and must match adjacent pair"
    );

    // Non-zero base: ordinal 1 as base, n = u32::MAX - 1.
    // Ceiling = 1.saturating_add((u32::MAX - 1) as usize) — valid on 64-bit
    // (= 1 + 4_294_967_294 = 4_294_967_295, no overflow).
    let nonzero = phrase_near_tokens_present("zzz alpha beta", "alpha beta", u32::MAX - 1);
    assert!(
        nonzero.is_some(),
        "D-1 overflow-widening: n=u32::MAX-1 with nonzero base must not panic and must match"
    );
}

// ── AC-403-17: total-span vs per-pair discriminator ───────────────────────────

/// AC-403-17: The TOTAL-SPAN N discriminator.
///
/// Corpus: one file with content "alpha xx beta xx gamma"
///   word tokens: alpha(0), xx(1), beta(2), xx(3), gamma(4)
///   total span = 4 (gamma ordinal - alpha ordinal)
///
/// --phrase --near 2: MUST return 0 results.
///   Total span 4 > 2.  A per-adjacent-pair implementation (each gap=2)
///   would incorrectly return 1 result here — this test catches that bug.
///
/// --phrase --near 4: MUST return 1 result (total span 4 <= 4, ordered).
#[test]
fn ac403_17_total_span_discriminator() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    fs::write(proj.path().join("file.rs"), "// alpha xx beta xx gamma\n").unwrap();
    build_index(proj.path(), cache.path());

    // --phrase --near 2: empty (total span 4 > 2).
    let j2 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "2", "alpha beta gamma"],
    );
    let got2 = paths(&j2);
    assert!(
        got2.is_empty(),
        "AC-403-17: --phrase --near 2 MUST return empty on content with total span 4; got {got2:?}. \
         A per-adjacent-pair implementation would incorrectly match (2+2=2 per gap). \
         This is the total-span discriminating test."
    );

    // --phrase --near 4: exactly 1 result (total span 4 <= 4, alpha before gamma in order).
    let j4 = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "4", "alpha beta gamma"],
    );
    let count4 = j4["total"].as_u64().unwrap_or(0);
    assert_eq!(
        count4, 1,
        "AC-403-17: --phrase --near 4 must return exactly 1 result; got {count4}"
    );
}

// ── AC-403-18: verify_mode JSON field ────────────────────────────────────────

/// AC-403-18: the `verify_mode` field in --json output.
///
/// (a) --phrase --near N: field == "phrase_near"
/// (b) no positional flags: field absent (not null) — byte-identity for existing callers
/// (c) --phrase: field == "phrase"
/// (d) --near N: field == "near"
#[test]
fn ac403_18_verify_mode_json_field() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // Build a tiny corpus so queries return a valid JSON envelope.
    fs::write(
        proj.path().join("a.rs"),
        "fn alpha() { beta(); gamma(); }\n",
    )
    .unwrap();
    build_index(proj.path(), cache.path());

    // (a) --phrase --near 5 → verify_mode == "phrase_near"
    let j_pn = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "5", "alpha"],
    );
    assert_eq!(
        j_pn["verify_mode"].as_str(),
        Some("phrase_near"),
        "AC-403-18(a): verify_mode must be 'phrase_near' with --phrase --near; got {:?}",
        j_pn["verify_mode"]
    );

    // (b) no flags → verify_mode absent (not present, not null).
    let j_none = search_json(proj.path(), cache.path(), &["alpha"]);
    assert!(
        j_none.get("verify_mode").is_none(),
        "AC-403-18(b): verify_mode must be ABSENT (not null) without positional flags; \
         got {:?}",
        j_none.get("verify_mode")
    );

    // (c) --phrase → verify_mode == "phrase"
    let j_p = search_json(proj.path(), cache.path(), &["--phrase", "alpha"]);
    assert_eq!(
        j_p["verify_mode"].as_str(),
        Some("phrase"),
        "AC-403-18(c): verify_mode must be 'phrase' with --phrase; got {:?}",
        j_p["verify_mode"]
    );

    // (d) --near 5 → verify_mode == "near"
    let j_n = search_json(proj.path(), cache.path(), &["--near", "5", "alpha"]);
    assert_eq!(
        j_n["verify_mode"].as_str(),
        Some("near"),
        "AC-403-18(d): verify_mode must be 'near' with --near; got {:?}",
        j_n["verify_mode"]
    );
}

// ── AC-403-18 k==1 E2E: single-word --phrase --near must return the file ──────

/// AC-403-18 (k==1 E2E): a single-word `--phrase --near N` query must return
/// the file containing the word AND produce a non-null `line_number`.
///
/// `ac403_18(a)` only asserts the `verify_mode` label ("phrase_near") — it does
/// not verify that `a.rs` actually appears in `results`.  A k==1 branch returning
/// an empty result set or the wrong anchor would pass the label check but fail here.
///
/// This test also exercises the k==1 branch of `count_phrase_near_alignments`
/// (reader.rs), which returns `d[0].len()` for single positioned-word queries.
#[test]
fn ac403_18_k1_single_word_phrase_near_returns_file() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // a.rs contains "encode_varint" as a standalone word token (underscore is a word byte).
    fs::write(
        proj.path().join("a.rs"),
        "fn encode_varint(x: u64) -> Vec<u8> { vec![] }\n",
    )
    .unwrap();
    // b.rs contains neither "encode_varint" nor any standalone "encode" token.
    fs::write(
        proj.path().join("b.rs"),
        "fn other_function() { let x = 1; }\n",
    )
    .unwrap();
    build_index(proj.path(), cache.path());

    // Single-word query with --phrase --near N exercises the k==1 branch of both
    // phrase_near_tokens_present (verify gate) and count_phrase_near_alignments (scorer).
    let json = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "5", "encode_varint"],
    );

    let got = paths(&json);
    assert!(
        got.contains("a.rs"),
        "AC-403-18 k==1: a.rs (contains 'encode_varint') must be in results; got {got:?}. \
         A broken k==1 branch returning empty or wrong anchor would fail here."
    );
    assert!(
        !got.contains("b.rs"),
        "AC-403-18 k==1: b.rs (no 'encode_varint' token) must be absent; got {got:?}"
    );

    // verify_mode must still be "phrase_near" for --phrase --near (unchanged).
    assert_eq!(
        json["verify_mode"].as_str(),
        Some("phrase_near"),
        "AC-403-18 k==1: verify_mode must be 'phrase_near'; got {:?}",
        json["verify_mode"]
    );

    // line_number must be present (non-null) — the k==1 anchor returns the
    // leftmost occurrence's byte range, which must resolve to a valid line.
    let result = &json["results"].as_array().expect("results array")[0];
    assert!(
        result["line_number"].as_u64().is_some(),
        "AC-403-18 k==1: line_number must be present (non-null) for a single-word match; \
         got {:?}",
        result["line_number"]
    );
}

// ── AC-403-2 (predicate): phrase_near golden-fixture differential identity ───

/// AC-403-2 (predicate extension): phrase_near_tokens_present(c, q, k-1)
/// must match phrase_tokens_present(c, q) on the existing golden phrase fixtures.
///
/// Tests the identity on representative cases from the existing suite.
#[test]
fn ac403_2_predicate_differential_identity() {
    // Cases from existing phrase_tokens_present tests.
    let cases: &[(&str, &str)] = &[
        ("fn encode_varint(x: u32)", "encode_varint"), // single-word
        ("fn encode varint bytes end", "encode varint"), // two-word, adjacent
        ("fn varint encode bytes", "encode varint"),   // reversed — both None
        ("fn encode some varint bytes", "encode varint"), // gapped — both None
        ("hello world foo bar baz", "foo bar"),        // two-word, mid-string
        ("in the loop here", "in the loop"),           // three short words
        ("fn main() { }", "fn main"),                  // short leading word
    ];

    for (content, query) in cases {
        let word_count = query.split_whitespace().count();
        // k-1 is the identity span.  For single-word, k-1=0 would always match
        // (span of one word is always 0), so skip k=1 for this test.
        if word_count < 2 {
            continue;
        }
        let k_minus_1 = (word_count - 1) as u32;

        let phrase_result = phrase_tokens_present(content, query);
        let pn_result = phrase_near_tokens_present(content, query, k_minus_1);

        assert_eq!(
            phrase_result.is_some(),
            pn_result.is_some(),
            "AC-403-2 identity: content={content:?} query={query:?} k-1={k_minus_1}: \
             phrase={phrase_result:?} phrase_near(k-1)={pn_result:?}"
        );
    }
}

// ── AC-403-2 (strengthened): exact Option<Range> identity on all 11 golden fixtures ──

/// AC-403-2 (strengthened): phrase_near_tokens_present(c, q, k-1) must return
/// the IDENTICAL `Option<Range<usize>>` as phrase_tokens_present(c, q) for each of
/// the 11 existing golden phrase fixtures at positional_verify.rs:33-172.
///
/// Replaces the weaker `is_some()`-only check with `assert_eq!` on the full
/// Option<Range<usize>> (byte-exact range identity, not just presence/absence).
///
/// Single-word fixtures (word_count < 2) are skipped -- the identity k-1=0 is
/// degenerate for single-word queries.
///
/// Byte ranges for the "Some" fixtures were computed from the content strings and
/// are cross-checked against the existing phrase_returns_correct_byte_range_ac3
/// and phrase_case_sensitive_ac14 unit tests.
#[test]
fn ac403_2_predicate_identity_all_golden_fixtures() {
    // Each entry: (content, query, expected_Option_Range).
    // NOTE: is_word_byte includes '_' so "encode_varint" is ONE token.
    // Fixtures are listed in declaration order (lines 33-172 of this file).
    let cases: &[(&str, &str, Option<Range<usize>>)] = &[
        // Fixture 1 -- phrase_exact_match_returns_some
        // query "encode_varint" is 1 word (underscore is a word byte) -> SKIP
        (
            "fn encode_varint(x: u32)",
            "encode_varint",
            None, /* SKIP: single word */
        ),
        // Fixture 2 -- phrase_trigram_false_positive_is_rejected_ac2
        // "encode_length" is ONE token; query word "encode" is absent -> None.
        ("encode_length varint_writer", "encode varint", None),
        // Fixture 3 -- phrase_multi_word_ordered_match
        // "encode" = bytes 3..9, "varint" = bytes 10..16.
        ("fn encode varint bytes end", "encode varint", Some(3..16)),
        // Fixture 4 -- phrase_reversed_order_not_matched
        // "encode" at ord 2, "varint" at ord 1 -- reversed, no forward placement.
        ("fn varint encode bytes", "encode varint", None),
        // Fixture 5 -- phrase_gap_not_matched
        // "encode" ord 1, "some" ord 2, "varint" ord 3 -- gap of 1 for k-1=1 ceiling -> None.
        ("fn encode some varint bytes", "encode varint", None),
        // Fixture 6 -- phrase_returns_correct_byte_range_ac3
        // "foo" = bytes 12..15, "bar" = bytes 16..19.
        // (Verified by the existing byte-range unit test.)
        ("hello world foo bar baz", "foo bar", Some(12..19)),
        // Fixture 7 -- phrase_empty_query_returns_none (word_count=0) -> SKIP
        (
            "fn encode(x: u32) {}",
            "",
            None, /* SKIP: empty query */
        ),
        // Fixture 8 -- phrase_separator_only_query_returns_none (word_count=0) -> SKIP
        (
            "fn encode(x: u32) {}",
            "   ",
            None, /* SKIP: whitespace-only query */
        ),
        // Fixture 9 -- phrase_empty_content_returns_none
        // Empty content -> c_words is empty -> None.
        ("", "encode varint", None),
        // Fixture 10a -- phrase_single_word_exact_token_boundary (no-match sub-case)
        // query "fn" is 1 word -> SKIP
        ("fn_helper foo bar", "fn", None /* SKIP: single word */),
        // Fixture 10b -- phrase_single_word_exact_token_boundary (match sub-case)
        // query "fn" is 1 word -> SKIP
        ("fn foo bar", "fn", None /* SKIP: single word */),
        // Fixture 11a -- phrase_case_sensitive_ac14 (wrong-case sub-case)
        // "Encode" != "encode" (case-sensitive); d[0] for "encode" is empty -> None.
        ("fn Encode varint foo", "encode varint", None),
        // Fixture 11b -- phrase_case_sensitive_ac14 (exact-case sub-case)
        // "encode" = bytes 3..9, "varint" = bytes 10..16.
        ("fn encode varint foo", "encode varint", Some(3..16)),
    ];

    for &(content, query, ref expected) in cases {
        let word_count = query.split_whitespace().count();
        if word_count < 2 {
            // Degenerate: single-word or empty query; k-1=0 or k-1<0.  Skip.
            continue;
        }
        let k_minus_1 = (word_count - 1) as u32;

        let phrase_result = phrase_tokens_present(content, query);

        // Sanity: phrase_tokens_present must agree with the documented expected range.
        assert_eq!(
            phrase_result, *expected,
            "AC-403-2 GOLDEN FIXTURE MISMATCH: phrase_tokens_present({content:?}, {query:?}) \
             expected {expected:?}, got {phrase_result:?}. \
             The expected range table in this test may need to be updated if the \
             implementation changed."
        );

        let pn_result = phrase_near_tokens_present(content, query, k_minus_1);

        // Full Option<Range> identity -- not just is_some() parity.
        assert_eq!(
            pn_result, *expected,
            "AC-403-2 IDENTITY VIOLATION: phrase_near_tokens_present({content:?}, {query:?}, \
             k-1={k_minus_1}) must return the IDENTICAL Option<Range> as \
             phrase_tokens_present (both should be {expected:?}); got {pn_result:?}. \
             This means the greedy fixed-ceiling scan diverges from the adjacent-scan \
             at k-1 boundary -- a regression in the identity invariant."
        );
    }
}

// ── AC-403-10: text + --near N composition with --ast and --blast-radius ──────

/// AC-403-10a: `text + --near N + --ast <pattern>` must return a non-empty
/// result set that is a STRICT SUBSET of `text + --near N` alone.
///
/// Corpus:
///   a.rs  "encode xx varint" (span 2 <= N=3) + match expression  -> in composed
///   b.rs  "encode xx varint" (span 2 <= N=3), no match            -> in near alone, NOT composed
///   c.rs  "encode aa bb cc varint" (span 4 > N=3) + match        -> excluded by near
///
/// Subset property: composed ⊆ near_alone.
/// Strict: b.rs is in near_alone but NOT in composed (explicit exclusion assertion).
#[test]
fn ac403_10_near_ast_composition_strict_subset() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // a.rs: "encode xx varint" tokens (span 2) inside a match block.
    fs::write(
        proj.path().join("a.rs"),
        "fn process(r: u32) -> u32 {\n\
         \x20   // encode xx varint\n\
         \x20   match r {\n\
         \x20       0 => 0,\n\
         \x20       _ => 1,\n\
         \x20   }\n\
         }\n",
    )
    .unwrap();

    // b.rs: same encode/varint span but NO match expression.
    // query_substring_present: "encode" present, "varint" present -> passes Substring.
    // Near(3): span=2 <=3 -> passes Near verify.
    // --ast match-with-arms: no match block -> EXCLUDED from composed.
    fs::write(
        proj.path().join("b.rs"),
        "// encode xx varint\nfn plain() { let x = 1; }\n",
    )
    .unwrap();

    // c.rs: match block present but encode..varint span=4 > N=3 -> excluded by --near.
    fs::write(
        proj.path().join("c.rs"),
        "fn check(r: u32) -> u32 {\n\
         \x20   // encode aa bb cc varint\n\
         \x20   match r {\n\
         \x20       0 => 0,\n\
         \x20       _ => 1,\n\
         \x20   }\n\
         }\n",
    )
    .unwrap();

    build_index(proj.path(), cache.path());

    // Baseline: text + --near 3 alone.
    let j_near = search_json(proj.path(), cache.path(), &["--near", "3", "encode varint"]);
    let near_set = paths(&j_near);

    // Composed: text + --near 3 + --ast match-with-arms.
    let j_composed = search_json(
        proj.path(),
        cache.path(),
        &["--near", "3", "--ast", "match-with-arms", "encode varint"],
    );
    let composed_set = paths(&j_composed);

    // Composed must be non-empty.
    assert!(
        !composed_set.is_empty(),
        "AC-403-10a: composed (--near 3 + --ast match-with-arms) must return >= 1 result; got empty"
    );

    // Positive: a.rs satisfies BOTH near and ast.
    assert!(
        composed_set.contains("a.rs"),
        "AC-403-10a: a.rs (encode/varint span=2 AND match-with-arms) must be in composed; \
         got {composed_set:?}"
    );

    // Strict subset: every file in composed must also be in near_alone.
    for file in &composed_set {
        assert!(
            near_set.contains(file.as_str()),
            "AC-403-10a: {file} is in (--near 3 + --ast) but NOT in (--near 3 alone) -- \
             MONOTONICITY VIOLATED; near_set={near_set:?}"
        );
    }

    // Exclusion: b.rs has near=true but no match -> absent from composed.
    assert!(
        !composed_set.contains("b.rs"),
        "AC-403-10a: b.rs (span<=3 but no match-with-arms) must be EXCLUDED from composed; \
         got {composed_set:?}"
    );

    // Exclusion: c.rs has match but span>3 -> excluded by near.
    assert!(
        !composed_set.contains("c.rs"),
        "AC-403-10a: c.rs (match-with-arms but span>3) must be EXCLUDED from composed; \
         got {composed_set:?}"
    );
}

/// AC-403-10b: `text + --near N + --blast-radius FILE` must return a non-empty
/// result set that is a STRICT SUBSET of `text + --blast-radius FILE` alone.
///
/// Corpus:
///   target.rs  the blast-radius reference file
///   a.rs       "encode xx varint" (span 2 <= N=3)           -> in both
///   b.rs       "encode aa bb cc varint" (span 4 > N=3)      -> in blast alone, NOT composed
///
/// Without temporal data the blast-radius path falls back to lexical union using
/// Substring verify (query_substring_present: EACH token must appear anywhere in
/// content).  b.rs contains both "encode" and "varint" as substrings so it passes
/// Substring verify but fails Near(3) verify (span 4 > 3).
///
/// Subset property: composed ⊆ blast_alone.
/// Strict: b.rs is in blast_alone but NOT in composed (explicit exclusion assertion).
#[test]
fn ac403_10_near_blast_radius_composition_strict_subset() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    fs::write(proj.path().join("target.rs"), "fn target() {}\n").unwrap();

    // a.rs: encode(0) xx(1) varint(2) -- span=2 <=3. Both in blast_alone and composed.
    fs::write(proj.path().join("a.rs"), "// encode xx varint\n").unwrap();

    // b.rs: encode(0) aa(1) bb(2) cc(3) varint(4) -- span=4 >3.
    // Substring verify passes (each token present); Near(3) verify fails.
    // -> in blast_alone (non-positional), EXCLUDED from composed.
    fs::write(proj.path().join("b.rs"), "// encode aa bb cc varint\n").unwrap();

    build_index(proj.path(), cache.path());

    // Baseline: text + --blast-radius alone (Substring verify).
    let j_blast = search_json(
        proj.path(),
        cache.path(),
        &["encode varint", "--blast-radius", "target.rs"],
    );
    let blast_set = paths(&j_blast);

    // Composed: text + --near 3 + --blast-radius (Near(3) verify).
    let j_composed = search_json(
        proj.path(),
        cache.path(),
        &[
            "--near",
            "3",
            "encode varint",
            "--blast-radius",
            "target.rs",
        ],
    );
    let composed_set = paths(&j_composed);

    // Composed must be non-empty.
    assert!(
        !composed_set.is_empty(),
        "AC-403-10b: composed (--near 3 + --blast-radius) must return >= 1 result; got empty. \
         Check that a.rs was indexed and span=2 <=3 passes Near(3) verify."
    );

    // Positive: a.rs (span=2 <=3) must appear in composed.
    assert!(
        composed_set.contains("a.rs"),
        "AC-403-10b: a.rs (encode/varint span=2) must be in composed; got {composed_set:?}"
    );

    // Strict subset: every file in composed must also be in blast_alone.
    for file in &composed_set {
        assert!(
            blast_set.contains(file.as_str()),
            "AC-403-10b: {file} is in (--near 3 + --blast-radius) but NOT in (--blast-radius alone) -- \
             MONOTONICITY VIOLATED; blast_set={blast_set:?}"
        );
    }

    // Exclusion: b.rs passes Substring verify but Near(3) span=4>3 -> excluded.
    assert!(
        !composed_set.contains("b.rs"),
        "AC-403-10b: b.rs (encode/varint span=4 > near N=3) must be EXCLUDED from composed; \
         got {composed_set:?}"
    );
}

// ── AC-403-16 (E2E): line_number anchors at FIRST matched word; line_range clamp ──

/// AC-403-16 (E2E extension): in `--json` output for `--phrase --near N`, the
/// `line_number` field MUST point to the line of the FIRST matched word (the
/// leftmost greedy base), not the last matched word.
///
/// Also verifies the existing `line_range` clamp invariant:
///   `end - start <= 2 * DEFAULT_CONTEXT + 1` (= 7).
///
/// Corpus: one file where "alpha" is on line 5 and "beta" is on line 7.
/// With `--phrase --near 6 "alpha beta"` the greedy scan places "alpha" as base.
/// `line_number` must be 5 (alpha's line), not 7 (beta's line).
#[test]
fn ac403_16_e2e_line_number_anchors_at_first_word_multi_line() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // Lines 1-8; "alpha" on line 5, "beta" on line 7.
    //
    // Token ordinals (0-based):
    //   header(0) comment(1) fn(2) setup(3) fn(4) helper(5) fn(6) configure(7)
    //   alpha(8) config(9) start(10) bridge(11) comment(12) here(13) beta(14) ...
    //
    // span(alpha, beta) = 14 - 8 = 6 <= N=6 -> match with --phrase --near 6.
    let content = "// header comment\n\
                   fn setup() {}\n\
                   fn helper() {}\n\
                   fn configure() {}\n\
                   alpha config start;\n\
                   // bridge comment here\n\
                   beta result end;\n\
                   // footer line\n";

    fs::write(proj.path().join("multi.rs"), content).unwrap();
    build_index(proj.path(), cache.path());

    let json = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "6", "alpha beta"],
    );
    let results = json["results"]
        .as_array()
        .expect("AC-403-16 E2E: results must be an array");

    assert_eq!(
        results.len(),
        1,
        "AC-403-16 E2E: exactly one result expected; got {results:?}"
    );
    let result = &results[0];

    // line_number must be 5 (the line where "alpha" -- the FIRST matched word -- appears).
    let line_number = result["line_number"]
        .as_u64()
        .expect("AC-403-16 E2E: line_number must be present as a number");
    assert_eq!(
        line_number, 5,
        "AC-403-16 E2E: line_number must be 5 (line of 'alpha', the FIRST matched word), \
         not 7 (line of 'beta', the LAST matched word); got {line_number}. \
         A re-anchor implementation that uses the last-word position would fail here."
    );

    // line_range must satisfy the clamp invariant: end - start <= 7.
    let line_range = &result["line_range"];
    assert!(
        !line_range.is_null(),
        "AC-403-16 E2E: line_range must be present (not null)"
    );
    let lr_start = line_range["start"]
        .as_u64()
        .expect("AC-403-16 E2E: line_range.start must be a number");
    let lr_end = line_range["end"]
        .as_u64()
        .expect("AC-403-16 E2E: line_range.end must be a number");

    assert!(
        lr_end - lr_start <= 7,
        "AC-403-16 E2E: line_range span ({}) must be <= 7 (2*DEFAULT_CONTEXT+1); \
         start={lr_start} end={lr_end}",
        lr_end - lr_start
    );
    assert!(
        lr_start <= line_number,
        "AC-403-16 E2E: line_range.start ({lr_start}) must be <= line_number ({line_number})"
    );
    assert!(
        lr_end > line_number,
        "AC-403-16 E2E: line_range.end ({lr_end}, exclusive) must be > line_number ({line_number})"
    );
}

// ── D-1 multi-base fallback (E2E) ────────────────────────────────────────────

/// D-1 multi-base fallback (E2E): when the first occurrence of the base word
/// exceeds the --near ceiling, a later occurrence must rescue the match.
///
/// Corpus:
///   a.rs — "alpha" on line 1 (too far from beta) AND line 2 with adjacent "beta"
///   b.rs — "alpha" only on line 1, far from beta — never rescued
///
/// File a.rs token ordinals (words in comment sections):
///   Line 1 "// alpha alone here":  alpha(0) alone(1) here(2)
///   Line 2 "// alpha beta":         alpha(3) beta(4)
///
///   d[0](alpha) = [0, 3],  d[1](beta) = [4]
///
/// With `--phrase --near 2`:
///   base=alpha@0 → ceiling 2; beta@4 > 2 → FAIL (first base fails).
///   base=alpha@3 → ceiling 5; beta@4 ≤ 5 → MATCH (second base rescues).
///
/// A regression to first-base-only would return an empty result set for a.rs,
/// causing it to appear absent.  This test catches that regression.
#[test]
fn ac403_d1_multi_base_e2e_second_base_rescues() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    // a.rs: "alpha" on line 1 (ordinal 0, far from beta) and line 2 (ordinal 3,
    // adjacent to beta at ordinal 4).  With --phrase --near 2:
    //   base@0 → ceiling=2; beta@4 > 2 → FAIL.
    //   base@3 → ceiling=5; beta@4 ≤ 5 → MATCH.
    fs::write(
        proj.path().join("a.rs"),
        "// alpha alone here\n// alpha beta\n",
    )
    .unwrap();
    // b.rs: "alpha" at ordinal 0 only; beta at ordinal 7 (far).
    // base@0 → ceiling=2; beta@7 > 2 → FAIL; no further bases.
    fs::write(
        proj.path().join("b.rs"),
        "// alpha alone here nothing here nothing here beta\n",
    )
    .unwrap();
    build_index(proj.path(), cache.path());

    let json = search_json(
        proj.path(),
        cache.path(),
        &["--phrase", "--near", "2", "alpha beta"],
    );
    let got = paths(&json);

    assert!(
        got.contains("a.rs"),
        "D-1 multi-base E2E: a.rs must be returned — second alpha (ordinal 3) rescues the \
         match (beta at ordinal 4, span=1 ≤ 2); got {got:?}. \
         A first-base-only implementation would miss this file."
    );
    assert!(
        !got.contains("b.rs"),
        "D-1 multi-base E2E: b.rs must be absent (only alpha at ordinal 0, \
         beta at ordinal 7, span=7 > 2); got {got:?}"
    );

    // line_number must anchor at line 2 (the RESCUING alpha on line 2 + beta on line 2),
    // NOT line 1 (the FAILING first alpha on line 1).
    let result = json["results"]
        .as_array()
        .expect("results must be an array")
        .iter()
        .find(|r| r["path"].as_str().unwrap_or("") == "a.rs")
        .expect("a.rs must be present in results");
    let ln = result["line_number"]
        .as_u64()
        .expect("D-1 multi-base E2E: line_number must be present");
    assert_eq!(
        ln, 2,
        "D-1 multi-base E2E: line_number must be 2 (rescuing alpha on line 2), \
         not 1 (failing alpha on line 1); got {ln}"
    );
}

// ── AC-403-12: --phrase --near 20 latency within 2.0x of --phrase alone ───────

/// AC-403-12: measured release-binary latency on the real repo corpus.
///
/// Builds a search index over the workspace root (the ~676-file skim-search
/// corpus) and times `--phrase <query>` vs `--phrase --near 20 <query>` over
/// three runs.  The median composed-query time must be within 2.0x the median
/// baseline time.
///
/// Skipped when the release binary does not exist (requires `cargo build
/// --release` first).  Relative latency is what matters; the 2.0x bound is
/// generous enough that debug-build timing differences do not affect pass/fail.
#[test]
fn ac403_12_phrase_near_latency_within_2x_baseline() {
    use std::time::Instant;

    // Workspace root -- two parents above CARGO_MANIFEST_DIR (crates/rskim).
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ parent")
        .parent()
        .expect("workspace root");

    // Prefer the release binary for meaningful latency numbers.
    let release_bin = workspace_root.join("target/release/skim");
    if !release_bin.exists() {
        // Skip gracefully -- the test is invalid without the release binary.
        eprintln!(
            "ac403_12: SKIP -- release binary not found at {release_bin:?}; \
             run 'cargo build --release --bin skim' first"
        );
        return;
    }

    let cache = tempfile::tempdir().unwrap();

    // Build the index over the workspace corpus.
    let build_status = std::process::Command::new(&release_bin)
        .args(["search", "--build", "--root"])
        .arg(workspace_root)
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .status()
        .expect("ac403_12: failed to invoke skim search --build");
    assert!(
        build_status.success(),
        "ac403_12: skim search --build must succeed on the workspace corpus"
    );

    // Query that returns results in the real codebase (tokens present in types.rs).
    const QUERY: &str = "phrase tokens";
    const RUNS: usize = 3;

    // Warm-up: two runs to amortize filesystem cache effects.
    for _ in 0..2 {
        let _ = std::process::Command::new(&release_bin)
            .args(["search", "--phrase", QUERY, "--root"])
            .arg(workspace_root)
            .env("SKIM_CACHE_DIR", cache.path())
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("NO_COLOR", "1")
            .output()
            .unwrap();
    }

    // Measure --phrase baseline.
    let mut baseline_ns: Vec<u128> = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let t0 = Instant::now();
        let status = std::process::Command::new(&release_bin)
            .args(["search", "--phrase", QUERY, "--root"])
            .arg(workspace_root)
            .env("SKIM_CACHE_DIR", cache.path())
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("NO_COLOR", "1")
            .status()
            .unwrap();
        baseline_ns.push(t0.elapsed().as_nanos());
        assert!(
            status.success(),
            "ac403_12: --phrase baseline query must exit 0"
        );
    }

    // Measure --phrase --near 20 composed.
    let mut composed_ns: Vec<u128> = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        let t0 = Instant::now();
        let status = std::process::Command::new(&release_bin)
            .args(["search", "--phrase", "--near", "20", QUERY, "--root"])
            .arg(workspace_root)
            .env("SKIM_CACHE_DIR", cache.path())
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("NO_COLOR", "1")
            .status()
            .unwrap();
        composed_ns.push(t0.elapsed().as_nanos());
        assert!(
            status.success(),
            "ac403_12: --phrase --near 20 query must exit 0"
        );
    }

    // Take median of three runs.
    baseline_ns.sort_unstable();
    composed_ns.sort_unstable();
    let baseline_median = baseline_ns[RUNS / 2];
    let composed_median = composed_ns[RUNS / 2];

    // If both queries complete in under 1ms the corpus is too small to produce a
    // stable latency ratio.  Silently setting ratio=0.0 would make the assert
    // trivially pass and hide that the performance bound was never checked.
    // Instead, skip the numeric bound explicitly so the skip is visible in output,
    // and rely on the exit-status assertions already run in the loops above.
    if baseline_median < 1_000_000 {
        eprintln!(
            "ac403_12: baseline under 1ms ({} ns median) — skipping numeric ratio assertion; \
             the 2.0x latency bound requires a corpus large enough to show measurable query \
             latency (run this test against the real skim-search workspace, ~676 files, for a \
             binding measurement)",
            baseline_median
        );
        return;
    }

    let ratio = composed_median as f64 / baseline_median as f64;
    assert!(
        ratio <= 2.0,
        "AC-403-12: --phrase --near 20 latency ({} ms median) must be within 2.0x the \
         --phrase-alone baseline ({} ms median); ratio={:.2}x. \
         A >2x ratio suggests O(candidates * window) behavior instead of O(candidates).",
        composed_median / 1_000_000,
        baseline_median / 1_000_000,
        ratio
    );
}
