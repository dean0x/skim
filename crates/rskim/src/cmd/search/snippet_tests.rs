//! Tests for the snippet extraction module (snippet.rs).

#![allow(clippy::unwrap_used)]
#![allow(clippy::single_range_in_vec_init)]

use std::fs;

use tempfile::tempdir;

use super::{byte_offset_to_line, extract_context_window, extract_snippet};

// ============================================================================
// byte_offset_to_line
// ============================================================================

#[test]
fn test_byte_offset_to_line_start_of_file() {
    let content = b"line1\nline2\nline3\n";
    assert_eq!(byte_offset_to_line(content, 0), 1, "offset 0 → line 1");
}

#[test]
fn test_byte_offset_to_line_second_line() {
    let content = b"line1\nline2\nline3\n";
    // "line2" starts at offset 6
    assert_eq!(
        byte_offset_to_line(content, 6),
        2,
        "start of line2 → line 2"
    );
}

#[test]
fn test_byte_offset_to_line_middle_of_line() {
    let content = b"hello\nworld\n";
    // offset 8 is in "world" (after the 'o')
    assert_eq!(
        byte_offset_to_line(content, 8),
        2,
        "middle of second line → line 2"
    );
}

#[test]
fn test_byte_offset_to_line_last_line_no_trailing_newline() {
    let content = b"a\nb\nc";
    // offset 4 is 'c' on line 3 (no trailing newline)
    assert_eq!(byte_offset_to_line(content, 4), 3);
}

#[test]
fn test_byte_offset_to_line_empty_file() {
    let content = b"";
    // Edge: empty file, offset 0
    assert_eq!(byte_offset_to_line(content, 0), 1);
}

#[test]
fn test_byte_offset_to_line_offset_at_newline() {
    let content = b"abc\ndef\n";
    // offset 3 is the newline at end of "abc" — still on line 1
    assert_eq!(byte_offset_to_line(content, 3), 1);
}

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
    assert!(result.is_none(), "empty positions → None");
}

#[test]
fn test_extract_snippet_returns_none_for_deleted_file() {
    let dir = tempdir().unwrap();
    let result = extract_snippet(dir.path(), "src/deleted.rs", &[0..3], None);
    assert!(result.is_none(), "deleted file → None");
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
    assert!(result.is_some(), "valid file + positions → Some");
    let (line_no, ctx) = result.unwrap();
    assert_eq!(line_no, 1, "match at offset 0 → line 1");
    assert!(!ctx.lines.is_empty());
    // The match line should be marked
    let match_line = ctx.lines.iter().find(|l| l.is_match).unwrap();
    assert_eq!(match_line.line_number, 1);
    assert!(match_line.content.contains("fn foo"));
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
    // If the file's actual mtime doesn't match the stale manifest mtime, return None.
    // (The file was just written so its mtime should be much newer than epoch+1.)
    // This test may be timing-sensitive on platforms where the FS clock resolution
    // is very coarse, but 1-second-since-epoch vs "now" is safe.
    assert!(
        result.is_none(),
        "stale mtime in manifest → None (file may have changed)"
    );
}
