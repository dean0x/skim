//! Tests for the snippet extraction module (snippet.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::single_range_in_vec_init)]

use std::fs;

use tempfile::tempdir;

use rskim_search::query_substring_present;

use super::{
    SnippetOutcome, VerifyMode, extract_context_window, extract_snippet, extract_snippet_and_verify,
};

// ============================================================================
// extract_context_window
// ============================================================================

#[test]
fn test_extract_context_window_middle() {
    let content = "line1\nline2\nline3\nline4\nline5\n";
    let lines = extract_context_window(content, 3, 1);
    // Should have lines 2, 3, 4
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].line_number, 2);
    assert_eq!(lines[1].line_number, 3);
    assert_eq!(lines[1].content, "line3");
    assert!(lines[1].is_match, "line 3 is the match line");
    assert_eq!(lines[2].line_number, 4);
    assert!(!lines[0].is_match);
    assert!(!lines[2].is_match);
}

#[test]
fn test_extract_context_window_at_start() {
    // Match is on line 1 with context=2 — can't go before line 1
    let content = "line1\nline2\nline3\nline4\n";
    let lines = extract_context_window(content, 1, 2);
    // Lines 1, 2, 3
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].line_number, 1);
    assert!(lines[0].is_match);
}

#[test]
fn test_extract_context_window_at_end() {
    let content = "line1\nline2\nline3\n";
    let lines = extract_context_window(content, 3, 2);
    // Lines 1, 2, 3 (can't go after line 3)
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2].line_number, 3);
    assert!(lines[2].is_match);
}

#[test]
fn test_extract_context_window_context_zero() {
    let content = "line1\nline2\nline3\n";
    let lines = extract_context_window(content, 2, 0);
    // Only the match line
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].line_number, 2);
    assert!(lines[0].is_match);
}

#[test]
fn test_extract_context_window_single_line_file() {
    let content = "only line\n";
    let lines = extract_context_window(content, 1, 3);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].line_number, 1);
    assert!(lines[0].is_match);
}

// ============================================================================
// extract_snippet
// ============================================================================

#[test]
fn test_extract_snippet_returns_none_for_empty_positions() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let file_path = root.join("src").join("lib.rs");
    fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    fs::write(&file_path, "fn foo() {}\n").unwrap();

    let result = extract_snippet(&root, "src/lib.rs", &[], None);
    assert!(
        matches!(result, SnippetOutcome::Unavailable),
        "empty positions → Unavailable"
    );
}

#[test]
fn test_extract_snippet_returns_none_for_deleted_file() {
    let dir = tempdir().unwrap();
    let result = extract_snippet(dir.path(), "src/deleted.rs", &[0..3], None);
    assert!(
        matches!(result, SnippetOutcome::Unavailable),
        "deleted file → Unavailable"
    );
}

#[test]
fn test_extract_snippet_basic_match() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let content = "fn foo() {}\nfn bar() {}\nfn baz() {}\n";
    fs::write(src_dir.join("lib.rs"), content).unwrap();

    let result = extract_snippet(&root, "src/lib.rs", &[0..3], None);
    let SnippetOutcome::Ok {
        match_line,
        context: ctx,
        ..
    } = result
    else {
        panic!("expected Ok, got {result:?}");
    };
    assert_eq!(match_line, 1, "match at offset 0 → line 1");
    assert!(!ctx.lines.is_empty());
    // The match line should be marked
    let matched = ctx.lines.iter().find(|l| l.is_match).unwrap();
    assert_eq!(matched.line_number, 1);
    assert!(matched.content.contains("fn foo"));
}

#[test]
fn test_extract_snippet_computes_line_range() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    // 5 lines: "aa\n" = 3 bytes each, last "ee\n" = 3 bytes
    // line 1 offset 0, line 2 offset 3, line 3 offset 6, line 4 offset 9, line 5 offset 12
    let content = "aa\nbb\ncc\ndd\nee\n";
    fs::write(src_dir.join("multi.rs"), content).unwrap();

    // Match positions on line 2 (offset 3) and line 4 (offset 9)
    let result = extract_snippet(&root, "src/multi.rs", &[3..5, 9..11], None);
    let SnippetOutcome::Ok {
        match_line,
        line_range,
        ..
    } = result
    else {
        panic!("expected Ok, got {result:?}");
    };
    assert_eq!(match_line, 2, "primary match line from first position");
    assert_eq!(
        line_range,
        2..5,
        "line_range spans lines 2-4 inclusive (2..5 exclusive)"
    );
}

#[test]
fn test_extract_snippet_stale_mtime_returns_none() {
    use crate::cmd::search::manifest::{ManifestEntry, encode_field_map};

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let file_path = src_dir.join("mod.rs");
    fs::write(&file_path, "fn stale() {}\n").unwrap();

    // Use a mtime far in the past (1970-01-01) for the manifest entry.
    // The file's actual mtime will be current — so they won't match.
    let stale_mtime = 1u64; // 1 second after epoch
    let entry = ManifestEntry {
        path: "src/mod.rs".to_string(),
        sha256: "a".repeat(64),
        lang: "rust".to_string(),
        field_map: encode_field_map(&[]),
        mtime: Some(stale_mtime),
        size: None,
    };

    let result = extract_snippet(&root, "src/mod.rs", &[0..2], Some(&entry));
    // If the file's actual mtime doesn't match the stale manifest mtime, return Stale.
    // (The file was just written so its mtime should be much newer than epoch+1.)
    assert!(
        matches!(result, SnippetOutcome::Stale),
        "stale mtime in manifest → Stale, got {result:?}"
    );
}

// ============================================================================
// query_substring_present — unit tests (PF-007: discriminating observables)
// ============================================================================

/// Single token: present in content → true.
#[test]
fn test_query_substring_present_single_token_found() {
    // Discriminating: must return true precisely because "authenticate" is in content.
    assert!(
        query_substring_present(
            "pub fn authenticate(token: &str) -> bool { !token.is_empty() }",
            "authenticate"
        ),
        "should find 'authenticate' as a literal substring"
    );
}

/// Single token: absent from content → false (AC2 — gibberish → not found).
///
/// PF-007: this test asserts the discriminating negative: a query provably
/// absent from the content must return false, so that the caller drops the
/// candidate from the verified result set.
#[test]
fn test_query_substring_present_single_token_absent() {
    // "zqxfjklm" is a gibberish sequence that cannot appear in natural code.
    assert!(
        !query_substring_present(
            "pub fn authenticate(token: &str) -> bool { !token.is_empty() }",
            "zqxfjklm"
        ),
        "gibberish token must not be found (AC2 — verified result set excludes it)"
    );
}

/// AND-of-tokens: all tokens present → true (AD-355-3 multi-term semantics).
#[test]
fn test_query_substring_present_multi_token_all_found() {
    let content = "pub fn authenticate(token: &str) -> bool { !token.is_empty() }";
    assert!(
        query_substring_present(content, "authenticate token"),
        "both 'authenticate' and 'token' are present — AND-of-tokens must be true"
    );
}

/// AND-of-tokens: one token absent → false (AC2 for multi-term).
///
/// PF-007: removing the absent-token check would turn this test into a false
/// positive — the test fails the moment OR-semantics are accidentally used.
#[test]
fn test_query_substring_present_multi_token_one_absent() {
    let content = "pub fn authenticate(token: &str) -> bool { !token.is_empty() }";
    assert!(
        !query_substring_present(content, "authenticate zqxfjklm"),
        "'zqxfjklm' is absent — AND requires ALL tokens; result must be false"
    );
}

/// Case-sensitive: lowercase query does NOT match uppercase-only text (AD-355-3).
#[test]
fn test_query_substring_present_case_sensitive() {
    assert!(
        !query_substring_present("pub fn Authenticate() {}", "authenticate"),
        "match is case-sensitive; 'authenticate' must not match 'Authenticate'"
    );
}

/// Empty query (no tokens after splitting) → false (defense-in-depth, Finding 15).
///
/// Prior to #355 cycle-2, an empty query returned vacuously true (`.all()` over
/// an empty iterator).  The defense-in-depth fix (Finding 15) makes the empty-
/// token case explicit: an empty/whitespace-only query is treated as "not present"
/// so that a future caller that skips the is_empty() guard cannot silently admit
/// all candidates.  The CLI dispatch already rejects empty queries before calling
/// this function, so the behavior change only affects edge cases in tests.
#[test]
fn test_query_substring_present_empty_query_returns_false() {
    assert!(
        !query_substring_present("any content", ""),
        "empty query: no tokens → false (defense-in-depth, not vacuously true)"
    );
}

// ============================================================================
// extract_snippet_and_verify — AD-355-7 empty-positions path
// ============================================================================

/// AD-355-7 / PF-007: when match_positions is empty (short-query fallback from
/// the reader), extract_snippet_and_verify must still read the file and run
/// query_substring_present.  It returns Unavailable (no context window without a
/// byte offset) but verified=true for files that contain the query.
///
/// Discriminating observable (PF-007): verified must be TRUE for a file that
/// contains the query, so the caller includes it in results.  If the empty-
/// positions early-exit were restored, verified would be false and the file would
/// be silently dropped — the bug the AD-355-7 fix addresses.
#[test]
fn test_extract_snippet_and_verify_empty_positions_file_contains_query_ad355_7() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "fn foo() {}\n").unwrap();

    // Empty positions — simulates the short-query (AD-355-7) fallback.
    let (outcome, verified) =
        extract_snippet_and_verify(&root, "src/lib.rs", &[], None, "fn", VerifyMode::Substring);

    // File contains "fn" → verified must be true so the caller keeps it.
    assert!(
        verified,
        "AD-355-7: file containing 'fn' with empty positions must be verified=true; got verified={verified}, outcome={outcome:?}"
    );
    // No snippet can be produced without a position.
    assert!(
        matches!(outcome, SnippetOutcome::Unavailable),
        "AD-355-7: empty positions → SnippetOutcome::Unavailable; got {outcome:?}"
    );
}

/// AD-355-7: when positions are empty and the file does NOT contain the query,
/// verified must be false — the verify gate still filters out non-matching files.
#[test]
fn test_extract_snippet_and_verify_empty_positions_file_absent_query_ad355_7() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "struct Foo {}\n").unwrap();

    let (_, verified) =
        extract_snippet_and_verify(&root, "src/lib.rs", &[], None, "fn", VerifyMode::Substring);

    // File does NOT contain "fn" → verified must be false.
    assert!(
        !verified,
        "AD-355-7: file not containing 'fn' with empty positions must be verified=false"
    );
}

/// Whitespace-only query → false (same defense-in-depth as empty query).
#[test]
fn test_query_substring_present_whitespace_only_query_returns_false() {
    assert!(
        !query_substring_present("any content", "   "),
        "whitespace-only query: no tokens → false (defense-in-depth, Finding 15)"
    );
}

// ============================================================================
// extract_snippet_and_verify — Phrase / Near verify paths (AD-393-10)
// ============================================================================

/// AD-393-10 / AC15: `extract_snippet_and_verify` with `VerifyMode::Phrase`
/// must return `verified=true` when the file contains the exact phrase as
/// adjacent word tokens, and `verified=false` when it contains only a
/// trigram-containment false positive.
///
/// This test directly covers the Phrase predicate dispatch path in
/// `run_verify_predicate_with_range` and the re-anchor logic (AD-393-6).
/// It is the unit-level complement to the CLI integration test
/// `cli_ac15_phrase_exits_zero_with_correct_results`.
///
/// Note: this test uses files well under MAX_SNIPPET_FILE_BYTES so it exercises
/// the NORMAL full-read branch (not the bounded-scan branch). The bounded-scan
/// path (`file_size > MAX_SNIPPET_FILE_BYTES`) is covered separately by
/// `extract_snippet_and_verify_large_file_bounded_scan_ac15`.
///
/// PF-007: each assertion is discriminating — the pass/fail outcome depends on
/// whether the phrase is present as exact word tokens, not merely as trigrams.
#[test]
fn extract_snippet_and_verify_phrase_mode_verifies() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::create_dir_all(root.join("src")).unwrap();

    // File A: contains the exact phrase "encode varint" as adjacent word tokens.
    let content_match = "fn process() {\n    let v = encode varint(x);\n}\n";
    fs::write(root.join("src/match.rs"), content_match).unwrap();

    // File B: trigram-containment false positive — same trigrams but NO exact
    // phrase (encode_length and varint_writer are single tokens).
    let content_fp = "fn process() {\n    let v = encode_length varint_writer(x);\n}\n";
    fs::write(root.join("src/fp.rs"), content_fp).unwrap();

    // Phrase verify: match.rs must be verified (exact phrase present).
    let (_, verified_match) = extract_snippet_and_verify(
        &root,
        "src/match.rs",
        &[32..32], // approximate position — re-anchor uses predicate range
        None,
        "encode varint",
        VerifyMode::Phrase,
    );
    assert!(
        verified_match,
        "AD-393-10: file containing exact phrase must be verified=true with VerifyMode::Phrase"
    );

    // Phrase verify: fp.rs must NOT be verified (superstring false positive).
    let (_, verified_fp) = extract_snippet_and_verify(
        &root,
        "src/fp.rs",
        &[32..32],
        None,
        "encode varint",
        VerifyMode::Phrase,
    );
    assert!(
        !verified_fp,
        "AD-393-10: superstring file must be verified=false with VerifyMode::Phrase \
         (trigram-containment false positive must be rejected)"
    );
}

/// AD-393-10 / Near path: `extract_snippet_and_verify` with `VerifyMode::Near(n)`
/// must return `verified=true` when the query words are within n word-token
/// positions and `verified=false` when they are farther apart.
///
/// Guards the Near predicate dispatch path in `run_verify_predicate_with_range`.
#[test]
fn extract_snippet_and_verify_near_mode_verifies() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::create_dir_all(root.join("src")).unwrap();

    // "encode" and "varint" appear within 2 word-token positions.
    fs::write(root.join("src/near.rs"), "fn f() { encode some varint; }\n").unwrap();
    // "encode" and "varint" are more than 2 positions apart.
    fs::write(root.join("src/far.rs"), "fn f() { encode a b c varint; }\n").unwrap();

    let (_, v_near) = extract_snippet_and_verify(
        &root,
        "src/near.rs",
        &[10..10],
        None,
        "encode varint",
        VerifyMode::Near(2),
    );
    assert!(
        v_near,
        "AD-393-10: words within n=2 positions must be verified=true with VerifyMode::Near(2)"
    );

    let (_, v_far) = extract_snippet_and_verify(
        &root,
        "src/far.rs",
        &[10..10],
        None,
        "encode varint",
        VerifyMode::Near(2),
    );
    assert!(
        !v_far,
        "AD-393-10: words beyond n=2 positions must be verified=false with VerifyMode::Near(2)"
    );
}

// ============================================================================
// AD-396: Substring anchor correctness (AC1/AC2/AC5/AC6/AC10)
// ============================================================================

/// AC2 / AC5 — Decoy-prefix fixture: an "encode_header" line ABOVE an
/// "encode_varint" line must NOT be the anchor.
///
/// This is the exact repro shape from the bug report: the trigram reader emits
/// a position for "encode_header" (which shares trigrams with "encode_varint"),
/// causing the old code to anchor on the wrong line. After #396, the anchor is
/// content-derived and must land on the true "encode_varint" line.
///
/// PF-007: the discriminating observable is `match_line` (and `is_match=true`
/// on that line), not just exit-0.
#[test]
fn test_anchor_does_not_land_on_decoy_line_ac2() {
    use super::extract_snippet_and_verify;
    use std::fs;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::create_dir_all(root.join("src")).unwrap();

    // Decoy on line 1, true match on line 2.
    // Old code anchored on line 1 (encode_header), new code must anchor on line 2.
    let content = "fn encode_header(buf: &[u8]) {}\nfn encode_varint(n: u64) -> u64 { n }\n";
    fs::write(root.join("src/codec.rs"), content).unwrap();

    // Simulate a match_positions pointing at the DECOY byte (offset 3 = 'e' in
    // "encode_header" on line 1) — the old anchor bug.
    let decoy_pos = vec![3usize..8]; // "encod" within encode_header
    let (outcome, verified) = extract_snippet_and_verify(
        &root,
        "src/codec.rs",
        &decoy_pos,
        None,
        "encode_varint",
        super::VerifyMode::Substring,
    );

    assert!(
        verified,
        "AC2: encode_varint is present → verified must be true; got {verified}"
    );

    let super::SnippetOutcome::Ok {
        match_line,
        line_range,
        context: ctx,
    } = outcome
    else {
        panic!("expected SnippetOutcome::Ok; got {outcome:?}");
    };

    // AC2: must NOT anchor on decoy line 1 (encode_header).
    assert_ne!(
        match_line, 1,
        "AC2: anchor must NOT be on the decoy line 1 (encode_header); got match_line={match_line}"
    );
    // AC2: must anchor on the true match line 2 (encode_varint).
    assert_eq!(
        match_line, 2,
        "AC2: anchor must be on line 2 (encode_varint); got match_line={match_line}"
    );

    // AC5: the is_match=true snippet line must agree with match_line.
    let is_match_line = ctx.lines.iter().find(|l| l.is_match).map(|l| l.line_number);
    assert_eq!(
        is_match_line,
        Some(2),
        "AC5: is_match=true snippet line must be line 2; got {is_match_line:?}"
    );
    // AC5: the marked snippet line must contain encode_varint.
    let marked_content = ctx
        .lines
        .iter()
        .find(|l| l.is_match)
        .map(|l| l.content.as_str())
        .unwrap_or("");
    assert!(
        marked_content.contains("encode_varint"),
        "AC5: is_match line must contain encode_varint; got {marked_content:?}"
    );

    // AC6: line_range must be the single anchor line {{n, n+1}}.
    assert_eq!(
        line_range,
        2..3,
        "AC6: line_range must be single anchor line {{2, 3}}; got {line_range:?}"
    );
}

/// AC10 (negative): a <3-byte query ("fn") with empty match_positions must
/// return `verified` based on content BUT `SnippetOutcome::Unavailable`
/// (snippet-less, no anchor), preserving the pre-#396 short-query behaviour.
///
/// PF-007: discriminating — if the AD-396-5 guard were removed, a file
/// containing "fn" with a content-derived anchor would receive a snippet,
/// breaking the AC10 invariant.
#[test]
fn test_short_query_substring_remains_snippet_less_ac10() {
    use super::extract_snippet_and_verify;
    use std::fs;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "fn foo() {}\nfn bar() {}\n").unwrap();

    // Empty positions simulates the AD-355-7 short-query fallback (< 3 bytes).
    let (outcome, verified) = extract_snippet_and_verify(
        &root,
        "src/lib.rs",
        &[], // empty positions — short-query fallback
        None,
        "fn", // 2-byte query, shorter than trigram threshold
        super::VerifyMode::Substring,
    );

    // verified must reflect content (file has "fn").
    assert!(
        verified,
        "AC10: file containing 'fn' must have verified=true even for short-query fallback"
    );
    // AD-396-5: snippet must NOT be produced (Unavailable).
    assert!(
        matches!(outcome, super::SnippetOutcome::Unavailable),
        "AC10: short-query with empty positions must return Unavailable; got {outcome:?}"
    );
}

/// AC10 guard-boundary (positive): a 3-byte single-token query with non-empty
/// positions MUST receive a correct anchor (not snippet-less).
///
/// PF-007: discriminating — confirms the AD-396-5 null-anchor guard does NOT
/// fire for normal (≥3-byte, non-empty-positions) queries.
#[test]
fn test_three_byte_query_gets_anchor_ac10_boundary() {
    use super::extract_snippet_and_verify;
    use std::fs;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::create_dir_all(root.join("src")).unwrap();
    // "foo" is a 3-byte token that exists on line 2.
    fs::write(root.join("src/lib.rs"), "header line\nfoo function here\n").unwrap();

    // Non-empty positions (normal path, not short-query fallback).
    // The actual byte offset of "foo" in the content is 12 (after "header line\n").
    let positions = vec![12usize..15];
    let (outcome, verified) = extract_snippet_and_verify(
        &root,
        "src/lib.rs",
        &positions,
        None,
        "foo", // 3-byte query — above the trigram threshold
        super::VerifyMode::Substring,
    );

    assert!(
        verified,
        "AC10-boundary: 3-byte query in content → verified=true"
    );
    assert!(
        matches!(outcome, super::SnippetOutcome::Ok { match_line: 2, .. }),
        "AC10-boundary: 3-byte query must produce Ok with anchor on line 2; got {outcome:?}"
    );
}

/// AD-396-3 / AC8 proxy: a file containing only the FIRST of two query tokens
/// must NOT be returned as verified for a two-token query (no verify-gate widening).
///
/// This is the E2E proxy for the unit-level AD-396-3 equivalence test
/// (which lives in rskim-search/src/types.rs and may not run locally due to
/// the lib-test-hang caveat). The observable here is verified=false.
///
/// PF-007: the discriminating observable is verified=false (gate not widened).
/// If substring_first_anchor returned Some when only the first token is present,
/// verified would be true and false-positives would enter the result set.
#[test]
fn test_two_token_query_first_only_present_not_verified_ac8() {
    use super::extract_snippet_and_verify;
    use std::fs;
    use tempfile::tempdir;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::create_dir_all(root.join("src")).unwrap();
    // File contains "encode_varint" but NOT "check_staleness".
    fs::write(
        root.join("src/codec.rs"),
        "fn encode_varint(n: u64) -> u64 { n }\n",
    )
    .unwrap();

    let positions = vec![3usize..16]; // approximate positions
    let (_, verified) = extract_snippet_and_verify(
        &root,
        "src/codec.rs",
        &positions,
        None,
        "encode_varint check_staleness", // second token absent
        super::VerifyMode::Substring,
    );

    assert!(
        !verified,
        "AC8: file missing 'check_staleness' must NOT be verified for 2-token query; \
         got verified=true (would widen the verify gate)"
    );
}

/// AD-393-10 / AC15: large-file bounded-scan path — `VerifyMode::Phrase` and
/// `VerifyMode::Near` must return `verified=false` when the file exceeds
/// `MAX_SNIPPET_FILE_BYTES` and the only occurrence of the phrase/words sits
/// beyond `MAX_VERIFY_SCAN_BYTES` (the scan cap).
///
/// This test exercises the `file_size > MAX_SNIPPET_FILE_BYTES` branch in
/// `extract_snippet_and_verify` (snippet.rs:285) by creating a sparse file via
/// `seek-past-end + write` so the OS reports the full size in metadata without
/// allocating the intermediate bytes. The "hole" reads as zero bytes, which
/// contain no word tokens matching the query — giving a clean, fast, and
/// discriminating negative result.
///
/// PF-007 discriminating observable: the test DEPENDS on the bounded scan cap —
/// if the cap were removed and the full file were scanned, the phrase would be
/// found and `verified` would flip to `true`, causing the assertion to fail.
/// This is the guard that ensures the `.take(needed as u64)` cap in snippet.rs
/// remains load-bearing.
///
/// Note: no assertion is made on SnippetOutcome because the large-file branch
/// always returns `SnippetOutcome::Unavailable` (by design — snippeting a 5MB+
/// file would allocate the entire contents).
#[test]
fn extract_snippet_and_verify_large_file_bounded_scan_ac15() {
    use std::io::{Seek, SeekFrom, Write};

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    fs::create_dir_all(root.join("src")).unwrap();

    // MAX_SNIPPET_FILE_BYTES = 5 * 1024 * 1024 (5 MiB). We create a sparse file
    // whose reported size is 5 MiB + 200 bytes by seeking to that offset and
    // writing the needle. The leading 5 MiB "hole" reads as zero bytes and
    // contains no word tokens, so the bounded scan (which only reads the first
    // MIN(file_size, MAX_VERIFY_SCAN_BYTES) = 5 MiB bytes) will not find the phrase.
    const FIVE_MIB: u64 = 5 * 1024 * 1024;
    let file_path = root.join("src/large.rs");
    {
        let mut f = std::fs::File::create(&file_path).unwrap();
        // Seek PAST the 5 MiB mark so the file's reported size exceeds
        // MAX_SNIPPET_FILE_BYTES, then write the needle only at this offset.
        f.seek(SeekFrom::Start(FIVE_MIB + 200)).unwrap();
        f.write_all(b"encode varint").unwrap();
        f.flush().unwrap();
    }

    // Sanity-check: file size must exceed FIVE_MIB so the large-file branch fires.
    let reported_size = std::fs::metadata(&file_path).unwrap().len();
    assert!(
        reported_size > FIVE_MIB,
        "test setup: sparse file must report size > 5 MiB; got {reported_size}"
    );

    // Phrase mode: the needle sits beyond the scan cap → verified=false (dropped).
    let (_, verified_phrase) = extract_snippet_and_verify(
        &root,
        "src/large.rs",
        &[], // no approximate positions — large-file path ignores them
        None,
        "encode varint",
        VerifyMode::Phrase,
    );
    assert!(
        !verified_phrase,
        "AC15: large file with phrase only beyond MAX_VERIFY_SCAN_BYTES must be \
         verified=false with VerifyMode::Phrase (bounded-scan cap enforced)"
    );

    // Near mode: same file, same expectation.
    let (_, verified_near) = extract_snippet_and_verify(
        &root,
        "src/large.rs",
        &[],
        None,
        "encode varint",
        VerifyMode::Near(5),
    );
    assert!(
        !verified_near,
        "AC15: large file with words only beyond MAX_VERIFY_SCAN_BYTES must be \
         verified=false with VerifyMode::Near(5) (bounded-scan cap enforced)"
    );
}
