//! Tests for the snippet extraction module (snippet.rs).

#![allow(clippy::unwrap_used)]
#![allow(clippy::single_range_in_vec_init)]

use std::fs;

use tempfile::tempdir;

use super::{SnippetOutcome, extract_context_window, extract_snippet};

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
    };

    let result = extract_snippet(&root, "src/mod.rs", &[0..2], Some(&entry));
    // If the file's actual mtime doesn't match the stale manifest mtime, return Stale.
    // (The file was just written so its mtime should be much newer than epoch+1.)
    assert!(
        matches!(result, SnippetOutcome::Stale),
        "stale mtime in manifest → Stale, got {result:?}"
    );
}
