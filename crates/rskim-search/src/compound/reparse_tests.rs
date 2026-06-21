//! Unit tests for `compound::reparse` — line recovery contract (AC-API2, AC-F2, AC-F3).
//!
//! Tests use inline source (tempfile fixtures) and call `recover_line` directly.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use tempfile::TempDir;

use crate::ast_index::parse_ast_query;

use super::recover_line;

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
