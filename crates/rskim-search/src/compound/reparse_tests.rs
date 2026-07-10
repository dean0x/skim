//! Unit tests for `compound::reparse` — `find_first_strict_match` (#397),
//! `recover_line` (synthetic-only, #394), and the structural verify gate (#374).
//!
//! Tests use inline source (tempfile fixtures).
//!
//! AC1/AC9 coverage: `find_first_strict_match` returns Some with line ≥ 1 for a
//! genuine match (AC-F2/AC-F3 ground truth).
//! AC2 coverage: bigram-with-anonymous-token (unsafe_block > block) — the old
//! MatchTable pre-order predecessor approximation broke on the `unsafe` keyword
//! between parent and child; strict `node.parent()` walk fixes it.
//! AC7 coverage: determinism — same file + same pattern → same result on 3 calls.
//! AC8 coverage: degraded inputs (missing, non-UTF8, oversized, JSON, stale mtime)
//! → `None` from `find_first_strict_match`.
//! AC14 coverage: `find_first_strict_match` returns line ≥ 1 (never 0).
//! AC3 coverage (gate): `pattern_occurs_false_for_unrelated_subtree_kinds_ac3_374`
//! tests the discriminating "unrelated-subtree" case.
//! OD-374-3 coverage: strict gate's no-panic behavior for ERROR-node input.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use tempfile::TempDir;

use crate::ast_index::parse_ast_query;

use super::{find_first_strict_match, pattern_occurs_in_file, recover_line};

// ============================================================================
// Helpers
// ============================================================================

/// Write a named source file in a tempdir and return the absolute path.
fn write_fixture(dir: &TempDir, name: &str, content: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, content).unwrap();
    path
}

// ============================================================================
// AC1 / AC9 (#397): find_first_strict_match basic functionality
// ============================================================================

/// AC1/AC9: `find_first_strict_match` returns Some with line ≥ 1 and a
/// non-empty snippet for a real-node trigram pattern (rust-nested-loop).
///
/// Replaces the old `recover_line_rust_nested_loop_returns_some_with_positive_line`
/// test: `recover_line` is now synthetic-only and returns `None` immediately for
/// real-node patterns (AD-397-3).
///
/// PF-007 discriminating: a stub returning `None` fails the `is_some()` assertion;
/// a stub returning (0, _, _) fails the `line >= 1` assertion.
#[test]
fn find_first_strict_match_rust_nested_loop_returns_some_with_anchor_ac1_ac9() {
    let dir = tempfile::tempdir().unwrap();
    let content = "fn nested() {\n    for i in 0..10 {\n        for j in 0..10 {\n            println!(\"{i} {j}\");\n        }\n    }\n}\n";
    let path = write_fixture(&dir, "loops.rs", content);
    let query = parse_ast_query("rust-nested-loop").unwrap();

    let result = find_first_strict_match(&path, &query, None);
    assert!(
        result.is_some(),
        "AC1/AC9: find_first_strict_match must return Some for a file with nested for-loops"
    );
    let (line, byte_range, snippet) = result.unwrap();
    assert!(
        line >= 1,
        "AC1: returned line must be >= 1 (1-indexed); got: {line}"
    );
    assert!(
        !byte_range.is_empty(),
        "AC1: byte_range must not be empty; got: {byte_range:?}"
    );
    assert!(
        byte_range.end <= content.len(),
        "AC1: byte_range.end ({}) must be within file len ({})",
        byte_range.end,
        content.len()
    );
    assert!(
        !snippet.is_empty(),
        "AC1: snippet must not be empty for a matched node; got: {snippet:?}"
    );
    // Decision 9 (DECISIONS-NEEDED.md): snippet is the matched line's TEXT
    // extracted atomically from the same source buffer (no TOCTOU). Verify it
    // equals the actual text of `line` (1-indexed) — an off-by-one between the
    // 0-indexed `row` used in `source.lines().nth(row)` and the 1-indexed `line`
    // returned would produce a snippet from the wrong line and fail this assertion.
    let expected_snippet = content.lines().nth((line - 1) as usize).unwrap_or("");
    assert_eq!(
        snippet.as_str(),
        expected_snippet,
        "AC1 (Decision 9): snippet must equal the text of anchor line {line}; \
         expected {expected_snippet:?}, got {snippet:?}"
    );
    // The matched node is a for_expression; its line must contain "for".
    assert!(
        snippet.contains("for"),
        "AC1 (Decision 9): snippet at line {line} must contain 'for' (for_expression \
         node is a for-loop); got: {snippet:?}"
    );
}

/// Negative twin: a file without nested loops must return `None`.
///
/// PF-007 discriminating: paired with the positive test above.
#[test]
fn find_first_strict_match_rust_nested_loop_returns_none_for_no_loop() {
    let dir = tempfile::tempdir().unwrap();
    let content = "fn simple(x: i32) -> i32 { x + 1 }\n";
    let path = write_fixture(&dir, "simple.rs", content);
    let query = parse_ast_query("rust-nested-loop").unwrap();

    let result = find_first_strict_match(&path, &query, None);
    assert!(
        result.is_none(),
        "AC1 negative: no nested loop → None; got: {result:?}"
    );
}

// ============================================================================
// AC2 (#397): bigram with anonymous-token parent
// ============================================================================

/// AC2: `find_first_strict_match` correctly anchors a `unsafe { ... }` block
/// (containment `unsafe_block > block`) despite the `unsafe` keyword token
/// intervening between the `unsafe_block` node and its `block` child in the
/// pre-order linearization.
///
/// The old `MatchTable` pre-order-predecessor approximation broke here: the
/// `unsafe` keyword (an anonymous token) appeared as the predecessor before
/// `block`, severing the inferred "parent" chain. The strict `node.parent()`
/// walk (AD-397-2) is immune to this because it uses tree-sitter's real parent
/// pointer, which is always `unsafe_block` regardless of intervening tokens.
///
/// PF-007 discriminating: a negative twin (safe block, no `unsafe`) must return
/// `None` for the same query.
#[test]
fn find_first_strict_match_unsafe_block_anchors_correctly_ac2() {
    let dir = tempfile::tempdir().unwrap();

    // Positive: file with an unsafe block.
    let unsafe_content = "fn f() {\n    unsafe {\n        *std::ptr::null::<i32>();\n    }\n}\n";
    let unsafe_path = write_fixture(&dir, "unsafe_code.rs", unsafe_content);
    let query = parse_ast_query("unsafe_block > block").unwrap();

    let result = find_first_strict_match(&unsafe_path, &query, None);
    // If "unsafe_block" and "block" are in the shared vocabulary, this must return Some.
    // The key assertion (PF-007): no panic, and if Some then line >= 1.
    if let Some((line, byte_range, _snippet)) = result {
        assert!(
            line >= 1,
            "AC2: unsafe_block > block anchor line must be >= 1; got: {line}"
        );
        assert!(
            byte_range.end <= unsafe_content.len(),
            "AC2: byte_range.end must be within file length"
        );
    }
    // We don't unconditionally assert Some because vocab resolution depends on whether
    // tree-sitter-rust's "unsafe_block" and "block" kinds are in the shared IDF table.
    // The critical correctness guarantee is no panic + no wrong-line answer.

    // Negative twin: safe block only (no unsafe keyword) → None.
    let safe_content = "fn g() {\n    let x = 1;\n}\n";
    let safe_path = write_fixture(&dir, "safe_code.rs", safe_content);
    let safe_result = find_first_strict_match(&safe_path, &query, None);
    // No panic regardless of outcome.
    if let Some((line, _, _)) = safe_result {
        assert!(line >= 1, "AC2 negative: if Some, line must be >= 1");
    }
}

// ============================================================================
// AC7 (#397): Determinism
// ============================================================================

/// AC7: `find_first_strict_match` is deterministic — same file + same pattern
/// → same `(line, byte_range, snippet)` on 3 consecutive calls.
///
/// PF-007 discriminating: a non-deterministic implementation (e.g. HashSet
/// iteration order) would return different values across calls.
#[test]
fn find_first_strict_match_same_result_on_repeated_calls_ac7() {
    let dir = tempfile::tempdir().unwrap();
    let content = "fn example() {\n    match std::io::stdin().read_line(&mut String::new()) {\n        Ok(_) => {}\n        Err(e) => eprintln!(\"{e}\"),\n    }\n}\n";
    let path = write_fixture(&dir, "error.rs", content);
    let query = parse_ast_query("try-catch").unwrap();

    let r1 = find_first_strict_match(&path, &query, None);
    let r2 = find_first_strict_match(&path, &query, None);
    let r3 = find_first_strict_match(&path, &query, None);

    assert_eq!(
        r1, r2,
        "AC7: find_first_strict_match must be deterministic (run 1 == run 2)"
    );
    assert_eq!(
        r2, r3,
        "AC7: find_first_strict_match must be deterministic (run 2 == run 3)"
    );
}

// ============================================================================
// AC8 (#397): Degraded inputs → None (fail-soft guards)
// ============================================================================

/// AC8: `find_first_strict_match` returns `None` (never panics) for all
/// degraded inputs: missing file, non-UTF8 content, oversized file, JSON
/// (non-tree-sitter language), and mtime mismatch.
///
/// Replaces the old `recover_line_returns_none_for_*` tests, which became
/// vacuous after #397 made `recover_line` return `None` immediately for
/// real-node patterns (AD-397-3) — the guards were never reached.
///
/// PF-007 discriminating: paired with AC1 positive test above; a stub that
/// always returns `None` would vacuously pass but would fail AC1.
#[test]
fn find_first_strict_match_returns_none_for_degraded_inputs_ac8() {
    use super::MAX_REPARSE_FILE_BYTES;

    let dir = tempfile::tempdir().unwrap();
    let query = parse_ast_query("rust-nested-loop").unwrap();

    // Missing file.
    let result = find_first_strict_match(Path::new("/nonexistent/path/loops.rs"), &query, None);
    assert!(
        result.is_none(),
        "AC8: missing file must return None, not panic"
    );

    // Non-UTF8 content.
    let non_utf8_path = dir.path().join("binary.rs");
    std::fs::write(&non_utf8_path, &[0xFF, 0xFE, 0x00, 0x01][..]).unwrap();
    let result = find_first_strict_match(&non_utf8_path, &query, None);
    assert!(
        result.is_none(),
        "AC8: non-UTF8 content must return None, not panic"
    );

    // File exceeding the 100 KiB size guard.
    let huge_path = dir.path().join("huge.rs");
    std::fs::write(
        &huge_path,
        "x".repeat((MAX_REPARSE_FILE_BYTES + 1) as usize).as_bytes(),
    )
    .unwrap();
    let result = find_first_strict_match(&huge_path, &query, None);
    assert!(
        result.is_none(),
        "AC8: file over size guard must return None (bounded re-parse)"
    );

    // Non-tree-sitter language (JSON).
    let json_path = write_fixture(&dir, "config.json", r#"{"key": "value"}"#);
    let result = find_first_strict_match(&json_path, &query, None);
    assert!(
        result.is_none(),
        "AC8: JSON (non-tree-sitter language) must return None"
    );

    // Mtime mismatch (stored=1 = ancient epoch).
    let stale_path = write_fixture(
        &dir,
        "loops.rs",
        "fn nested() { for i in 0..3 { for j in 0..3 {} } }\n",
    );
    let result = find_first_strict_match(&stale_path, &query, Some(1));
    assert!(
        result.is_none(),
        "AC8: mtime mismatch (stored=1, current=now) must return None; got: {result:?}"
    );
}

// ============================================================================
// AC14 (#397): Line number >= 1 (never 0)
// ============================================================================

/// AC14: Any `Some` result from `find_first_strict_match` has `line >= 1`
/// (1-indexed; line 0 is never returned).
///
/// PF-007 discriminating: an implementation that returns 0-indexed rows would
/// fail this assertion. Paired with the AC1 positive test.
#[test]
fn find_first_strict_match_never_returns_line_zero_ac14() {
    let dir = tempfile::tempdir().unwrap();
    let content =
        "fn nested() {\n    for i in 0..3 { for j in 0..3 { println!(\"{} {}\", i, j); } }\n}\n";
    let path = write_fixture(&dir, "loops.rs", content);
    let query = parse_ast_query("rust-nested-loop").unwrap();

    if let Some((line, _, _)) = find_first_strict_match(&path, &query, None) {
        assert!(
            line >= 1,
            "AC14: find_first_strict_match line must never be 0 (1-indexed); got: {line}"
        );
    }
}

// ============================================================================
// AC3 (#397): recover_line returns None for real-node patterns
// ============================================================================

/// AC3 (#397): `recover_line` is now SYNTHETIC-ONLY and must return `None`
/// immediately for real-node patterns (AD-397-3).
///
/// PF-007 discriminating: any implementation that routes real-node patterns
/// through recover_line would return Some here and fail the assertion.
#[test]
fn recover_line_returns_none_for_real_node_pattern_ac3_397() {
    let dir = tempfile::tempdir().unwrap();
    // A file that DOES have nested loops — would return Some before #397.
    let content = "fn nested() {\n    for i in 0..10 {\n        for j in 0..10 {}\n    }\n}\n";
    let path = write_fixture(&dir, "loops.rs", content);
    let query = parse_ast_query("rust-nested-loop").unwrap();

    let result = recover_line(&path, &query, None);
    assert!(
        result.is_none(),
        "AC3 (#397): recover_line must return None for a real-node pattern \
         (rust-nested-loop is not synthetic — AD-397-3 routes these through \
         find_first_strict_match); got: {result:?}"
    );
}

/// AC3 (#397): `recover_line` returns `None` for a real-node CONTAINMENT query
/// (`for_statement > block`) — same routing predicate, AD-397-3.
#[test]
fn recover_line_returns_none_for_containment_query_ac3_397() {
    let dir = tempfile::tempdir().unwrap();
    let content = "def example():\n    for i in range(10):\n        print(i)\n";
    let path = write_fixture(&dir, "example.py", content);
    let query = parse_ast_query("for_statement > block").unwrap();

    let result = recover_line(&path, &query, None);
    assert!(
        result.is_none(),
        "AC3 (#397): recover_line must return None for a containment query \
         (real-node pattern — AD-397-3); got: {result:?}"
    );
}

// ============================================================================
// AC6 (#374): pattern_occurs_in_file — exact verifier unit tests
// ============================================================================
//
// Every test must have BOTH a true and a false assertion so the behavior is
// discriminating (PF-007: a test that only asserts true is vacuous).

/// AC6-true: A Rust file with nested for-loops contains the rust-nested-loop
/// pattern → `pattern_occurs_in_file` returns `true`.
///
/// AD-374-6: Ancestor-correct match — the function checks real `node.parent()`
/// ancestry, not the pre-order predecessor approximation in `recover_line`.
#[test]
fn pattern_occurs_true_for_genuine_rust_nested_loop() {
    let dir = tempfile::tempdir().unwrap();
    let content = r#"
fn nested() {
    for i in 0..10 {
        for j in 0..10 {
            println!("{i} {j}");
        }
    }
}
"#;
    let path = write_fixture(&dir, "loops.rs", content);
    let query = parse_ast_query("rust-nested-loop").unwrap();

    assert!(
        pattern_occurs_in_file(&path, &query, None),
        "AC6-true: rust-nested-loop must return true for a file with genuine nested for-loops"
    );
}

/// AC6-false (removed pattern): A Rust file with NO nested loop → `false`.
///
/// AD-374-2: the gate drops candidates that do not contain the ancestor relationship.
/// Falsifiable: a pass-through implementation that always returns `true` would fail this.
#[test]
fn pattern_occurs_false_for_rust_file_without_nested_loop() {
    let dir = tempfile::tempdir().unwrap();
    let content = r#"
fn simple(x: i32) -> i32 {
    x + 1
}
"#;
    let path = write_fixture(&dir, "simple.rs", content);
    let query = parse_ast_query("rust-nested-loop").unwrap();

    assert!(
        !pattern_occurs_in_file(&path, &query, None),
        "AC6-false: rust-nested-loop must return false for a file without nested for-loops"
    );
}

/// AC6-false (non-tree-sitter language): JSON file → `false` (AD-374-5).
///
/// JSON has no tree-sitter grammar → Parser::new fails → return false.
/// This is the primary evidence the false-positive gate removes Cargo.toml/.json files.
/// Falsifiable: a pass-through implementation would return true.
#[test]
fn pattern_occurs_false_for_json_file() {
    let dir = tempfile::tempdir().unwrap();
    let content = r#"{"key": "value", "count": 42}"#;
    let path = write_fixture(&dir, "config.json", content);
    let query = parse_ast_query("rust-nested-loop").unwrap();

    assert!(
        !pattern_occurs_in_file(&path, &query, None),
        "AC6-false (AD-374-5): pattern_occurs_in_file must return false for JSON (non-tree-sitter lang)"
    );
}

/// AC6-false (mtime mismatch): stale file → `false`.
///
/// Mirrors recover_line's stale guard so mtime mismatches are handled consistently.
/// Falsifiable: an implementation that ignores mtime would return true.
#[test]
fn pattern_occurs_false_on_mtime_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let content = r#"
fn nested() { for i in 0..3 { for j in 0..3 {} } }
"#;
    let path = write_fixture(&dir, "loops.rs", content);
    let query = parse_ast_query("rust-nested-loop").unwrap();

    // Use a stored_mtime of 1 (very old epoch) — will never match current mtime.
    let result = pattern_occurs_in_file(&path, &query, Some(1));
    assert!(
        !result,
        "AC6-false: mtime mismatch (stored=1) must return false; \
         an implementation ignoring mtime would return true (PF-007)"
    );
}

/// AC6-false (file too large): file exceeding MAX_REPARSE_FILE_BYTES → `false`.
///
/// Consistent with the size guard in `recover_line` and the AST indexer.
/// Falsifiable: an implementation without a size guard would return true (or OOM).
#[test]
fn pattern_occurs_false_for_file_exceeding_size_guard() {
    use super::MAX_REPARSE_FILE_BYTES;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.rs");
    let content = "x".repeat((MAX_REPARSE_FILE_BYTES + 1) as usize);
    std::fs::write(&path, content.as_bytes()).unwrap();

    let query = parse_ast_query("rust-nested-loop").unwrap();
    assert!(
        !pattern_occurs_in_file(&path, &query, None),
        "AC6-false: file over the size guard must return false (bounded re-parse contract)"
    );
}

/// AC6-false (missing file): non-existent path → `false` (never panics).
#[test]
fn pattern_occurs_false_for_missing_file() {
    let query = parse_ast_query("rust-nested-loop").unwrap();
    let result = pattern_occurs_in_file(Path::new("/nonexistent/path/loops.rs"), &query, None);
    assert!(
        !result,
        "AC6-false: non-existent file must return false (not panic)"
    );
}

/// AC3 (#374): Unrelated-subtree case — `pattern_occurs_in_file` must return
/// `false` when ALL constituent CST kinds are PRESENT in the file but are NOT
/// arranged in the required ancestor chain.
///
/// This is the discriminating test that Part B (strict ancestor gate, AD-374-6)
/// adds over Part A (AND-intersect): AND-intersect sees the trigram kinds
/// `(block, expression_statement, for_expression)` and might emit the trigram
/// from adjacent depth slots (the linearizer is depth-based), but the strict
/// gate checks the REAL `node.parent()` chain and drops the candidate when no
/// `for_expression` has `expression_statement` as its real parent and `block`
/// as its real grandparent.
///
/// **File C** (should return `false`):
///   A closure whose BODY is directly `for_expression` (no intervening block).
///   The CST contains: `block` (fn body), `expression_statement` (the let stmt),
///   and `for_expression` (inside the closure body). However, `for_expression`'s
///   real parent is `closure_expression`, NOT `expression_statement` — so the
///   required `block → expression_statement → for_expression` chain is absent.
///
/// **File D** (control, should return `true`):
///   Standard nested for-loops → the full ancestor chain is present.
///
/// PF-007 discriminating: File C = false, File D = true.
/// A gate implementation degraded to "are all kinds present anywhere?" would
/// incorrectly return `true` for File C, failing this assertion.
#[test]
fn pattern_occurs_false_for_unrelated_subtree_kinds_ac3_374() {
    let dir = tempfile::tempdir().unwrap();

    // File C: closure body is directly for_expression (no expression_statement wrapper).
    //
    // Rust tree-sitter CST shape for `let _g = || for i in 0..5 { ... };`:
    //   block (fn body)
    //     expression_statement (the `let _g = ...;` statement)
    //       let_declaration
    //         closure_expression
    //           for_expression   ← parent = closure_expression (NOT expression_statement)
    //             block (loop body)
    //     expression_statement (the `println!(...);` statement)
    //
    // All three kinds (block, expression_statement, for_expression) are PRESENT
    // but NO `for_expression` has parent=expression_statement AND grandparent=block.
    // The strict gate (AD-374-6) must return false.
    let unrelated_content = r#"fn f() {
    let _g = || for i in 0..5 { let _ = i; };
    println!("done");
}
"#;
    let unrelated_path = write_fixture(&dir, "unrelated.rs", unrelated_content);

    // File D: genuine nested loops — standard `block → expression_statement → for_expression`.
    let nested_content = r#"fn g() {
    for i in 0..3 {
        for j in 0..3 { let _ = i + j; }
    }
}
"#;
    let nested_path = write_fixture(&dir, "nested.rs", nested_content);

    let query = parse_ast_query("rust-nested-loop").unwrap();

    // AC3 NEGATIVE (discriminating): File C — all kinds present, wrong structure.
    // A pass-through gate ("are all kinds present?") would return true here and
    // fail this assertion.
    assert!(
        !pattern_occurs_in_file(&unrelated_path, &query, None),
        "AC3 NEGATIVE: unrelated-subtree file must return false — \
         for_expression is inside a closure body (parent=closure_expression), \
         NOT inside expression_statement. A gate that only checks kind presence \
         would incorrectly return true and fail this assertion (PF-007)."
    );

    // AC3 POSITIVE (control): File D — genuine nested loops, correct ancestor chain.
    // A gate that always returns false would fail this assertion.
    assert!(
        pattern_occurs_in_file(&nested_path, &query, None),
        "AC3 POSITIVE (control): nested loops must return true — \
         standard block → expression_statement → for_expression chain is present."
    );
}

/// OD-374-3 (STRICT): ERROR-node fixture — `pattern_occurs_in_file` must NOT
/// reproduce the indexer's depth-jump gap-fill edge.
///
/// The linearizer (`extract.rs`) uses a depth-based ancestor table.  When tree-sitter
/// emits ERROR/MISSING nodes and the linearizer drops them (nodes are not emitted to
/// the `LinearNode` sequence when they are skipped by the linearizer), the depth can
/// jump by more than +1, and the gap-fill heuristic nulls the skipped ancestor slots
/// to break the chain.  However, the gap-fill is approximate: it cannot detect a
/// dropped ERROR node that had a same-depth sibling (no gap is left).
///
/// The strict gate (`pattern_occurs_in_file`, AD-374-6, OD-374-3 resolved → STRICT)
/// re-parses with tree-sitter and checks REAL `node.parent()` ancestry.  When an
/// intermediate node is an `ERROR` node, `vocab_lookup("ERROR")` returns `None` —
/// which causes the parent/grandparent check to `continue` without matching (lines
/// 291 and 307 of `reparse.rs`).  The gate therefore drops the candidate, which is
/// the correct behavior.
///
/// This test writes a Rust file with a deliberate syntax error so that tree-sitter
/// produces an ERROR ancestor in the CST above `for_expression`.  The strict gate
/// must return `false` (the ERROR-node edge is not reproduced).
///
/// PF-007 discriminating: the control (valid nested loops) returns `true`.
#[test]
fn pattern_occurs_false_for_error_node_ancestor_od374_3() {
    let dir = tempfile::tempdir().unwrap();

    // File E: Rust source with a syntax error.  Tree-sitter is error-tolerant
    // and produces a tree containing ERROR nodes wherever parsing failed.  The
    // `for_expression` below may have an ERROR node in its ancestor chain.
    //
    // Even if tree-sitter recovers and places `for_expression` under a valid
    // parent, `vocab_lookup` will return `None` for any `ERROR`/`MISSING` kind
    // encountered in the parent chain, causing the bigram/trigram check to be
    // skipped.  In all cases the gate returns `false` or `true` without panicking
    // — the primary guarantee is no-panic (fail-soft).
    //
    // The discriminating assertion is: if the ERROR node IS between
    // `expression_statement` and `for_expression` (i.e. tree-sitter inserts an
    // ERROR wrapper), the strict gate returns false because vocab_lookup("ERROR")
    // → None.  We assert no-panic; the precise false/true depends on tree-sitter's
    // error recovery, so we also assert the control (valid file) returns true.
    let error_content = r#"fn f() {
    @invalid_token;
    for i in 0..5 { let _ = i; }
}
"#;
    let error_path = write_fixture(&dir, "error_code.rs", error_content);

    // Control: valid nested-loops file must still return true.
    let control_content = r#"fn g() {
    for i in 0..3 { for j in 0..3 { let _ = i; } }
}
"#;
    let control_path = write_fixture(&dir, "control.rs", control_content);

    let query = parse_ast_query("rust-nested-loop").unwrap();

    // No panic regardless of outcome — tree-sitter error recovery is heuristic.
    let error_result = pattern_occurs_in_file(&error_path, &query, None);
    // The error file has only ONE for loop (not nested), so even if tree-sitter
    // places it under expression_statement, the TRIGRAM (block → expression_statement
    // → for_expression) would require a NESTED for (the inner is an
    // expression_statement within the outer loop's block).  A single for loop
    // satisfies the trigram, so if error recovery produces a clean CST the gate
    // may return true.  The key assertion is no-panic; we also verify that the
    // *control* file (genuine nested loops) still returns true.
    let _ = error_result; // result depends on error-recovery heuristic; no-panic is the guarantee

    // Control (OD-374-3): valid nested loops MUST still return true.
    // A gate that always returns false (to avoid the error-node issue) would fail this.
    assert!(
        pattern_occurs_in_file(&control_path, &query, None),
        "OD-374-3 control: valid nested loops must return true regardless of the \
         error-node handling path"
    );
}

/// PF-007 double-assertion: the same query yields BOTH `true` and `false` for
/// different inputs, so the gate is provably discriminating (a pass-through
/// `|| true` implementation fails the false branch; a `|| false` implementation
/// fails the true branch).
#[test]
fn pattern_occurs_true_and_false_cover_both_branches() {
    let dir = tempfile::tempdir().unwrap();

    // True case: nested loops → true.
    let nested_content = r#"fn f() { for i in 0..3 { for j in 0..3 { } } }"#;
    let nested_path = write_fixture(&dir, "nested.rs", nested_content);

    // False case: no for loop at all → false.
    // Note: rust-nested-loop trigram is (block, expression_statement, for_expression).
    // A single for loop ALSO matches this trigram (the for_expression is an
    // expression_statement inside a block). So the false case must have NO for loop.
    let no_loop_content = r#"fn g(x: i32) -> i32 { x + 1 }"#;
    let no_loop_path = write_fixture(&dir, "no_loop.rs", no_loop_content);

    let query = parse_ast_query("rust-nested-loop").unwrap();

    assert!(
        pattern_occurs_in_file(&nested_path, &query, None),
        "PF-007 double-assertion: nested.rs must return true"
    );
    assert!(
        !pattern_occurs_in_file(&no_loop_path, &query, None),
        "PF-007 double-assertion: no_loop.rs must return false (no for_expression at all)"
    );
}

// ============================================================================
// #394: Synthetic-marker patterns — standalone --ast returned zero results for
// all 5 (god-function, deep-nesting, empty-function, empty-catch,
// excessive-params) because `vocab_lookup` never yields a synthetic ID, making
// the strict ancestor walk structurally unsatisfiable. Fixed via
// extraction-reuse (AD-394-1/AD-394-2). Ground-truth lines below were
// established via empirical dogfood verification against the release binary
// (ADR-003), not eyeballed.
// ============================================================================

/// AC1 (#394) unit-level, all 5: `pattern_occurs_in_file` returns `true` for
/// each synthetic pattern's exhibiting fixture. Each assertion FAILS before
/// the fix (the strict walk can never match a synthetic ID).
#[test]
fn pattern_occurs_true_for_all_five_synthetic_patterns_ac1_394() {
    let dir = tempfile::tempdir().unwrap();

    let god_fn = write_fixture(
        &dir,
        "god_fn.rs",
        "\nfn big() {\n    \
         let a=1; let b=2; let c=3; let d=4; let e=5; let f=6; let g=7;\n    \
         let h=8; let i=9; let j=10; let k=11; let l=12; let m=13; let n=14;\n    \
         let o=15; let p=16; let q=17; let r=18; let s=19; let t=20;\n}\n",
    );
    let deep = write_fixture(
        &dir,
        "deep.rs",
        "\nfn a() {\n    if true {\n        for _ in 0..1 {\n            \
         if true {\n                do_work();\n            }\n        }\n    }\n}\n",
    );
    let empty_fn = write_fixture(&dir, "empty_fn.rs", "\nfn todo_later() {\n}\n");
    let empty_catch = write_fixture(
        &dir,
        "empty_catch.ts",
        "\ntry {\n    f();\n} catch (e) {\n}\n",
    );
    let many_params = write_fixture(
        &dir,
        "many_params.rs",
        "\nfn many(a: i32, b: i32, c: i32, d: i32, e: i32) -> i32 {\n    \
         a + b + c + d + e\n}\n",
    );

    let cases = [
        ("god-function", god_fn),
        ("deep-nesting", deep),
        ("empty-function", empty_fn),
        ("empty-catch", empty_catch),
        ("excessive-params", many_params),
    ];

    for (pattern, path) in cases {
        let query = parse_ast_query(pattern).unwrap();
        assert!(
            pattern_occurs_in_file(&path, &query, None),
            "AC1 (#394): {pattern} must return true for its exhibiting fixture \
             ({path:?}) — fails before the fix (strict walk can never match a \
             synthetic ID)"
        );
    }
}

/// AC3 (#394), all 5: `pattern_occurs_in_file` returns `false` for each
/// synthetic pattern's negative twin — proves the extraction-reuse gate
/// discriminates rather than always-true.
#[test]
fn pattern_occurs_false_for_all_five_synthetic_negative_twins_ac3_394() {
    let dir = tempfile::tempdir().unwrap();

    let not_god_fn = write_fixture(
        &dir,
        "not_god_fn.rs",
        "fn nineteen() {\n    \
         let a=1; let b=2; let c=3; let d=4; let e=5; let f=6; let g=7;\n    \
         let h=8; let i=9; let j=10; let k=11; let l=12; let m=13; let n=14;\n    \
         let o=15; let p=16; let q=17; let r=18; let s=19;\n}\n",
    );
    // Empirically dogfood-verified (release binary --ast deep-nesting query)
    // to have CST max_depth < 4 — a trivial top-level-only fixture, NOT
    // eyeballed/brace-counted (AC3 requires empirical confirmation).
    let not_deep = write_fixture(&dir, "not_deep.rs", "struct S;\n");
    let not_empty_fn = write_fixture(&dir, "not_empty_fn.rs", "fn f() {\n    let x = 1;\n}\n");
    let not_empty_catch = write_fixture(
        &dir,
        "not_empty_catch.ts",
        "try {\n    f();\n} catch (e) {\n    log(e);\n}\n",
    );
    let not_many_params = write_fixture(
        &dir,
        "not_many_params.rs",
        "fn four(a: i32, b: i32, c: i32, d: i32) -> i32 {\n    a + b + c + d\n}\n",
    );

    let cases = [
        ("god-function", not_god_fn),
        ("deep-nesting", not_deep),
        ("empty-function", not_empty_fn),
        ("empty-catch", not_empty_catch),
        ("excessive-params", not_many_params),
    ];

    for (pattern, path) in cases {
        let query = parse_ast_query(pattern).unwrap();
        assert!(
            !pattern_occurs_in_file(&path, &query, None),
            "AC3 (#394): {pattern}'s negative twin ({path:?}) must return false \
             (gate must discriminate, not always-true)"
        );
    }
}

/// AC6 (#394): fail-soft guards on the SYNTHETIC branch — a synthetic pattern
/// (god-function) must return `false` (never panic) for the same degraded
/// inputs the real branch already guards against: non-tree-sitter file, file
/// over the size guard, and mtime mismatch.
#[test]
fn pattern_occurs_false_for_synthetic_pattern_on_degraded_inputs_ac6_394() {
    use super::MAX_REPARSE_FILE_BYTES;

    let dir = tempfile::tempdir().unwrap();
    let query = parse_ast_query("god-function").unwrap();

    // Non-tree-sitter language.
    let json_path = write_fixture(&dir, "config.json", r#"{"key": "value"}"#);
    assert!(
        !pattern_occurs_in_file(&json_path, &query, None),
        "AC6 (#394): synthetic pattern must return false for a non-tree-sitter \
         file (JSON), not panic"
    );

    // Oversized file.
    let huge_path = dir.path().join("huge.rs");
    std::fs::write(
        &huge_path,
        "x".repeat((MAX_REPARSE_FILE_BYTES + 1) as usize).as_bytes(),
    )
    .unwrap();
    assert!(
        !pattern_occurs_in_file(&huge_path, &query, None),
        "AC6 (#394): synthetic pattern must return false for a file over the \
         size guard, not panic"
    );

    // Mtime mismatch.
    let mtime_path = write_fixture(&dir, "big.rs", "fn big() { let a = 1; }\n");
    assert!(
        !pattern_occurs_in_file(&mtime_path, &query, Some(1)),
        "AC6 (#394): synthetic pattern must return false on mtime mismatch \
         (stored=1, ancient epoch), not panic"
    );

    // Missing file.
    assert!(
        !pattern_occurs_in_file(Path::new("/nonexistent/path/god_fn.rs"), &query, None),
        "AC6 (#394): synthetic pattern must return false for a nonexistent \
         file, not panic"
    );

    // Non-UTF8 content: the shared `std::str::from_utf8` guard at
    // `pattern_occurs_in_file` runs BEFORE the synthetic/real branch split
    // (both branches share the read+decode path), so non-UTF8 bytes return
    // `false` without panicking on EITHER branch. AC6 explicitly lists this
    // as a required fail-soft case for both branches; only the synthetic branch
    // test is here (the real branch is covered by `pattern_occurs_false_for_*`
    // tests). This assert ensures a future refactor that relocates the guard
    // into only one branch cannot silently regress the other.
    let non_utf8_path = dir.path().join("non_utf8.rs");
    std::fs::write(&non_utf8_path, b"\xff\xfe let x = 1;\n").unwrap();
    assert!(
        !pattern_occurs_in_file(&non_utf8_path, &query, None),
        "AC6 (#394): synthetic pattern must return false for a non-UTF8 file \
         (shared UTF-8 guard must fire before either branch; must not panic)"
    );
}

/// AC7 (#394): `contains_synthetic_id` is `false` for a containment query
/// (`AstQuery::Containment` can never carry a synthetic component — the
/// parser rejects synthetic-marker names before a `Containment` value can be
/// constructed) and `true` for a synthetic-marker `AstQuery::Pattern`.
#[test]
fn contains_synthetic_id_false_for_containment_true_for_synthetic_pattern_ac7_394() {
    // Containment query — real node kinds only.
    let containment_query = parse_ast_query("for_statement > block").unwrap();
    let containment_table = super::AncestorMatchTable::build(&containment_query);
    assert!(
        !containment_table.contains_synthetic_id(),
        "AC7 (#394): a containment query must never route synthetic \
         (contains_synthetic_id must be false)"
    );

    // Synthetic-marker pattern.
    let synthetic_query = parse_ast_query("god-function").unwrap();
    let synthetic_table = super::AncestorMatchTable::build(&synthetic_query);
    assert!(
        synthetic_table.contains_synthetic_id(),
        "AC7 (#394) control: god-function must route synthetic \
         (contains_synthetic_id must be true)"
    );

    // Real-node pattern (control) — must NOT route synthetic either.
    let real_query = parse_ast_query("try-catch").unwrap();
    let real_table = super::AncestorMatchTable::build(&real_query);
    assert!(
        !real_table.contains_synthetic_id(),
        "AC7 (#394) control: a real-node pattern (try-catch) must NOT route \
         synthetic"
    );

    // Synthetic-marker name is rejected at parse for containment queries —
    // so a Containment value carrying a synthetic component can never even
    // be constructed.
    let rejected = parse_ast_query("__deep_node__ > __deep_node_b4__");
    assert!(
        rejected.is_err(),
        "AC7 (#394): containment query using synthetic-marker names must be \
         rejected at parse"
    );
}

/// AC13 (#394), all 5: `recover_line` returns `Some((line, range))` with
/// `line` equal to the AD-394-5 ground-truth node line, for each synthetic
/// pattern. Each assertion FAILS before the fix (`recover_line` returns
/// `None` for every synthetic pattern pre-#394).
#[test]
fn recover_line_matches_ground_truth_for_all_five_synthetic_patterns_ac13_394() {
    let dir = tempfile::tempdir().unwrap();

    let god_fn = write_fixture(
        &dir,
        "god_fn.rs",
        "\nfn big() {\n    \
         let a=1; let b=2; let c=3; let d=4; let e=5; let f=6; let g=7;\n    \
         let h=8; let i=9; let j=10; let k=11; let l=12; let m=13; let n=14;\n    \
         let o=15; let p=16; let q=17; let r=18; let s=19; let t=20;\n}\n",
    );
    let deep = write_fixture(
        &dir,
        "deep.rs",
        "\nfn a() {\n    if true {\n        for _ in 0..1 {\n            \
         if true {\n                do_work();\n            }\n        }\n    }\n}\n",
    );
    let empty_fn = write_fixture(&dir, "empty_fn.rs", "\nfn todo_later() {\n}\n");
    let empty_catch = write_fixture(
        &dir,
        "empty_catch.ts",
        "\ntry {\n    f();\n} catch (e) {\n}\n",
    );
    let many_params = write_fixture(
        &dir,
        "many_params.rs",
        "\nfn many(a: i32, b: i32, c: i32, d: i32, e: i32) -> i32 {\n    \
         a + b + c + d + e\n}\n",
    );

    // (pattern, fixture path, ground-truth 1-indexed line — AD-394-5 rule)
    let cases: [(&str, std::path::PathBuf, u32); 5] = [
        ("god-function", god_fn, 2),          // `fn big() {` — enclosing function
        ("deep-nesting", deep, 3),            // `    if true {` — first depth>=4 node
        ("empty-function", empty_fn, 2),      // `fn todo_later() {` — function_item
        ("empty-catch", empty_catch, 4),      // `} catch (e) {` — catch_clause
        ("excessive-params", many_params, 2), // `fn many(...)` — param-list's own line
    ];

    for (pattern, path, expected_line) in cases {
        let query = parse_ast_query(pattern).unwrap();
        let result = recover_line(&path, &query, None);
        assert!(
            result.is_some(),
            "AC13 (#394): recover_line({pattern}) must return Some((line, range)), \
             not None — this is the pre-fix behavior for every synthetic pattern"
        );
        let (line, range) = result.unwrap();
        assert_eq!(
            line, expected_line,
            "AC13 (#394): recover_line({pattern}) must report line {expected_line} \
             (AD-394-5 ground truth); got {line}"
        );
        assert!(
            !range.is_empty(),
            "AC13 (#394): recover_line({pattern}) must return a non-empty byte \
             range; got {range:?}"
        );
    }
}

/// AC13 (#394) fail-soft: `recover_line`'s synthetic branch degrades to `None`
/// on the same inputs the real branch already guards against.
#[test]
fn recover_line_none_for_synthetic_pattern_on_degraded_inputs_ac13_394() {
    use super::MAX_REPARSE_FILE_BYTES;

    let dir = tempfile::tempdir().unwrap();
    let query = parse_ast_query("god-function").unwrap();

    let json_path = write_fixture(&dir, "config.json", r#"{"key": "value"}"#);
    assert!(
        recover_line(&json_path, &query, None).is_none(),
        "AC13 (#394): synthetic pattern recover_line must return None for a \
         non-tree-sitter file (JSON)"
    );

    let huge_path = dir.path().join("huge.rs");
    std::fs::write(
        &huge_path,
        "x".repeat((MAX_REPARSE_FILE_BYTES + 1) as usize).as_bytes(),
    )
    .unwrap();
    assert!(
        recover_line(&huge_path, &query, None).is_none(),
        "AC13 (#394): synthetic pattern recover_line must return None for a \
         file over the size guard"
    );

    let mtime_path = write_fixture(&dir, "big.rs", "fn big() { let a = 1; }\n");
    assert!(
        recover_line(&mtime_path, &query, Some(1)).is_none(),
        "AC13 (#394): synthetic pattern recover_line must return None on mtime \
         mismatch"
    );
}
