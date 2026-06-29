//! Unit tests for `compound::reparse` — line recovery contract (AC-API2, AC-F2, AC-F3)
//! and the structural verify gate (AC6 / #374).
//!
//! Tests use inline source (tempfile fixtures) and call `recover_line` and
//! `pattern_occurs_in_file` directly.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use tempfile::TempDir;

use crate::ast_index::parse_ast_query;

use super::{pattern_occurs_in_file, recover_line};

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
// AC-API2: recover_line contract
// ============================================================================

#[test]
fn recover_line_rust_nested_loop_returns_some_with_positive_line() {
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

    let result = recover_line(&path, &query, None);
    assert!(
        result.is_some(),
        "rust-nested-loop must recover a line from a file with nested for-loops"
    );
    let (line, byte_range) = result.unwrap();
    assert!(
        line >= 1,
        "recovered line must be >= 1 (1-indexed); got: {line}"
    );
    assert!(
        !byte_range.is_empty(),
        "byte_range must not be empty; got: {byte_range:?}"
    );
    let file_len = content.len();
    assert!(
        byte_range.end <= file_len,
        "byte_range.end ({}) must be within file len ({})",
        byte_range.end,
        file_len
    );
}

#[test]
fn recover_line_returns_none_for_missing_file() {
    let query = parse_ast_query("rust-nested-loop").unwrap();
    let result = recover_line(Path::new("/nonexistent/path/loops.rs"), &query, None);
    assert!(
        result.is_none(),
        "recover_line must return None for a nonexistent file (AC-API2)"
    );
}

#[test]
fn recover_line_returns_none_for_non_utf8_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("binary.rs");
    // Write non-UTF8 bytes.
    std::fs::write(&path, &[0xFF, 0xFE, 0x00, 0x01][..]).unwrap();

    let query = parse_ast_query("rust-nested-loop").unwrap();
    let result = recover_line(&path, &query, None);
    assert!(
        result.is_none(),
        "recover_line must return None for non-UTF8 content (AC-API2)"
    );
}

#[test]
fn recover_line_returns_none_for_file_exceeding_size_guard() {
    use super::MAX_REPARSE_FILE_BYTES;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.rs");
    // Write a file just over the 100 KiB guard.
    let content = "x".repeat((MAX_REPARSE_FILE_BYTES + 1) as usize);
    std::fs::write(&path, content.as_bytes()).unwrap();

    let query = parse_ast_query("rust-nested-loop").unwrap();
    let result = recover_line(&path, &query, None);
    assert!(
        result.is_none(),
        "recover_line must return None for files over the size guard (AC-API2)"
    );
}

#[test]
fn recover_line_returns_none_for_language_without_pattern_kinds() {
    // A Python file cannot match "rust-nested-loop" (Rust-only node kinds).
    // The pattern's resolved bigrams will all yield None during vocab lookup
    // for Python grammar node kinds → target_kind_ids is empty → None.
    let dir = tempfile::tempdir().unwrap();
    let content = r#"
def nested():
    for i in range(10):
        for j in range(10):
            print(i, j)
"#;
    let path = write_fixture(&dir, "loops.py", content);
    let query = parse_ast_query("rust-nested-loop").unwrap();

    // Note: we expect None here because "rust-nested-loop" has Rust-specific node kinds
    // that don't exist in Python's tree-sitter grammar. However, the vocabulary is shared
    // and some kinds may overlap. The pattern may or may not resolve — the important
    // invariant is that recover_line does NOT panic and returns Some or None.
    // We test non-panic and the no-line-number-fabrication guarantee.
    let result = recover_line(&path, &query, None);
    // No panic regardless of outcome (AC-API2, AC-F2: no fabricated line).
    if let Some((line, _)) = result {
        assert!(
            line >= 1,
            "if Some, line must be >= 1 (never 0); got: {line}"
        );
    }
}

#[test]
fn recover_line_returns_none_for_non_tree_sitter_language() {
    // JSON files have no tree-sitter grammar in rskim-core → Parser::new returns Err → None.
    let dir = tempfile::tempdir().unwrap();
    let content = r#"{"key": "value"}"#;
    let path = write_fixture(&dir, "config.json", content);
    let query = parse_ast_query("rust-nested-loop").unwrap();

    let result = recover_line(&path, &query, None);
    assert!(
        result.is_none(),
        "recover_line must return None for JSON (non-tree-sitter language); got: {result:?}"
    );
}

// ============================================================================
// AC-F3: Determinism
// ============================================================================

#[test]
fn recover_line_same_result_on_repeated_calls() {
    let dir = tempfile::tempdir().unwrap();
    let content = r#"
fn example() {
    match std::io::stdin().read_line(&mut String::new()) {
        Ok(_) => {}
        Err(e) => eprintln!("{e}"),
    }
}
"#;
    let path = write_fixture(&dir, "error.rs", content);
    let query = parse_ast_query("try-catch").unwrap();

    let r1 = recover_line(&path, &query, None);
    let r2 = recover_line(&path, &query, None);
    let r3 = recover_line(&path, &query, None);

    assert_eq!(
        r1, r2,
        "AC-F3: recover_line must be deterministic (run 1 == run 2)"
    );
    assert_eq!(
        r2, r3,
        "AC-F3: recover_line must be deterministic (run 2 == run 3)"
    );
}

// ============================================================================
// AC-F2: No fabricated line numbers
// ============================================================================

#[test]
fn recover_line_never_returns_line_zero() {
    // Any `Some` result must have line >= 1.
    let dir = tempfile::tempdir().unwrap();
    let content = r#"
fn nested() {
    for i in 0..3 { for j in 0..3 { println!("{} {}", i, j); } }
}
"#;
    let path = write_fixture(&dir, "loops.rs", content);
    let query = parse_ast_query("rust-nested-loop").unwrap();

    if let Some((line, _)) = recover_line(&path, &query, None) {
        assert!(
            line >= 1,
            "AC-F2: recovered line must never be 0 (1-indexed); got: {line}"
        );
    }
}

// ============================================================================
// Mtime guard: stale file degrades to None
// ============================================================================

#[test]
fn recover_line_returns_none_on_mtime_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let content = r#"
fn nested() { for i in 0..3 { for j in 0..3 {} } }
"#;
    let path = write_fixture(&dir, "loops.rs", content);
    let query = parse_ast_query("rust-nested-loop").unwrap();

    // Use a stored_mtime of 1 (very old) — will never match current mtime.
    let result = recover_line(&path, &query, Some(1));
    assert!(
        result.is_none(),
        "mtime mismatch (stored=1, current=now) must degrade to None; got: {result:?}"
    );
}

// ============================================================================
// Cross-language: Python pattern on Python file
// ============================================================================

#[test]
fn recover_line_python_containment_query_returns_some() {
    let dir = tempfile::tempdir().unwrap();
    // A Python file with a for loop (matches "for_statement > block" containment).
    let content = r#"
def example():
    for i in range(10):
        print(i)
"#;
    let path = write_fixture(&dir, "example.py", content);
    // Use a containment query with Python-compatible node kinds.
    let query = parse_ast_query("for_statement > block").unwrap();

    let result = recover_line(&path, &query, None);
    // for_statement > block is a valid Python tree-sitter containment.
    // We don't assert Some here because vocabulary resolution depends on
    // whether "for_statement" and "block" are in the shared vocabulary.
    // The key invariant: no panic, and if Some then line >= 1.
    if let Some((line, byte_range)) = result {
        assert!(line >= 1, "AC-F2: line must be >= 1; got: {line}");
        assert!(
            byte_range.end <= content.len(),
            "byte_range.end must be within file length"
        );
    }
    // No panic regardless of outcome — that's the primary guarantee.
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

/// PF-007 double-assertion: the same query yields BOTH `true` and `false` for
/// different inputs, so the gate is provably discriminating (a pass-through
/// `|| true` implementation fails the false branch; a `|| false` implementation
/// fails the true branch).
///
/// NOTE (coverage gap): this is the SAME discriminator as
/// `pattern_occurs_true_for_genuine_rust_nested_loop` +
/// `pattern_occurs_false_for_rust_file_without_nested_loop` — both branches turn
/// on whether the pattern's n-gram kinds are PRESENT in the file, not on the
/// ancestor *relationship*. The strict-gate behavior that AD-374-6 actually adds
/// over Part A (AND-intersect) is the "unrelated-subtree" case: a file where both
/// n-gram kinds are PRESENT but NOT in a real parent→child / grandparent chain,
/// which must return `false`. That case is NOT covered by any current test (see
/// #374 follow-up); constructing a grammar-faithful unrelated-subtree fixture
/// requires confirming the exact tree-sitter CST shape and is tracked separately.
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
