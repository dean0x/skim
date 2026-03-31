//! Unified diff parsing — hunk extraction and file status detection.

use std::sync::LazyLock;

use regex::Regex;

use super::types::{DiffHunk, FileDiff, FileMetadata};
use crate::output::canonical::DiffFileStatus;

/// Matches hunk headers: `@@ -N,M +N,M @@ optional context`
static HUNK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^@@\s+-(\d+)(?:,(\d+))?\s+\+(\d+)(?:,(\d+))?\s+@@").expect("valid regex")
});

/// Parse a hunk header line: `@@ -N,M +N,M @@`
///
/// Returns `(old_start, old_count, new_start, new_count)` on success.
pub(super) fn parse_hunk_header(line: &str) -> Option<(usize, usize, usize, usize)> {
    let caps = HUNK_RE.captures(line)?;
    let old_start: usize = caps.get(1)?.as_str().parse().ok()?;
    let old_count: usize = caps.get(2).map_or(1, |m| m.as_str().parse().unwrap_or(1));
    let new_start: usize = caps.get(3)?.as_str().parse().ok()?;
    let new_count: usize = caps.get(4).map_or(1, |m| m.as_str().parse().unwrap_or(1));
    Some((old_start, old_count, new_start, new_count))
}

/// Scan extended headers from a `diff --git` block.
///
/// Starting at `start`, reads lines until a hunk header (`@@`) or the next
/// `diff --git` header. Returns the collected metadata and the index of
/// the next unprocessed line.
pub(super) fn scan_extended_headers(lines: &[&str], start: usize) -> (FileMetadata, usize) {
    let mut meta = FileMetadata {
        is_binary: false,
        is_new: false,
        is_deleted: false,
        is_renamed: false,
        rename_from: None,
        file_minus: String::new(),
        file_plus: String::new(),
    };

    let mut i = start;
    while i < lines.len() && !lines[i].starts_with("diff --git ") {
        let l = lines[i];

        if l.starts_with("new file mode") {
            meta.is_new = true;
        } else if l.starts_with("deleted file mode") {
            meta.is_deleted = true;
        } else if l.starts_with("rename from ") {
            meta.is_renamed = true;
            meta.rename_from = Some(l.strip_prefix("rename from ").unwrap_or("").to_string());
        } else if l.starts_with("rename to ") {
            meta.is_renamed = true;
        } else if l.starts_with("Binary files") && l.contains("differ") {
            meta.is_binary = true;
        } else if l.starts_with("--- ") {
            meta.file_minus = l.strip_prefix("--- ").unwrap_or("").to_string();
        } else if l.starts_with("+++ ") {
            meta.file_plus = l.strip_prefix("+++ ").unwrap_or("").to_string();
        } else if l.starts_with("@@") {
            // Hunk header — extended headers are done, stop before consuming it
            break;
        }

        i += 1;
    }

    (meta, i)
}

/// Collect hunks from diff lines starting at `start`.
///
/// Reads hunk headers (`@@`) and their patch lines until the next `diff --git`
/// header or end of input. Returns the hunks and the index of the next
/// unprocessed line.
pub(super) fn collect_hunks<'a>(lines: &[&'a str], start: usize) -> (Vec<DiffHunk<'a>>, usize) {
    let mut hunks: Vec<DiffHunk<'a>> = Vec::new();
    let mut i = start;

    while i < lines.len() && !lines[i].starts_with("diff --git ") {
        let l = lines[i];

        if l.starts_with("@@") {
            if let Some((old_start, old_count, new_start, new_count)) = parse_hunk_header(l) {
                let mut patch_lines: Vec<&'a str> = Vec::new();
                i += 1;

                while i < lines.len() {
                    let pl = lines[i];
                    if pl.starts_with("diff --git ") || pl.starts_with("@@") {
                        break;
                    }
                    // Only keep actual patch lines (+, -, space, or \ no newline)
                    if pl.starts_with('+')
                        || pl.starts_with('-')
                        || pl.starts_with(' ')
                        || pl.starts_with('\\')
                    {
                        patch_lines.push(pl);
                    }
                    i += 1;
                }

                hunks.push(DiffHunk {
                    old_start,
                    old_count,
                    new_start,
                    new_count,
                    patch_lines,
                });
                continue;
            }
        }
        i += 1;
    }

    (hunks, i)
}

/// Determine the file status, display path, and optional old_path from diff
/// header paths and extended header metadata.
pub(super) fn resolve_file_info(
    a_path: &str,
    b_path: &str,
    meta: &FileMetadata,
) -> (DiffFileStatus, String, Option<String>) {
    let status = if meta.is_binary {
        DiffFileStatus::Binary
    } else if meta.is_new || meta.file_minus == "/dev/null" || meta.file_minus == "a//dev/null" {
        DiffFileStatus::Added
    } else if meta.is_deleted || meta.file_plus == "/dev/null" || meta.file_plus == "b//dev/null" {
        DiffFileStatus::Deleted
    } else if meta.is_renamed {
        DiffFileStatus::Renamed
    } else {
        DiffFileStatus::Modified
    };

    let path = if status == DiffFileStatus::Deleted {
        strip_ab_prefix(a_path)
    } else {
        strip_ab_prefix(b_path)
    };

    let old_path = if meta.is_renamed {
        meta.rename_from
            .clone()
            .or_else(|| Some(strip_ab_prefix(a_path)))
    } else {
        None
    };

    (status, path, old_path)
}

/// Parse unified diff output into a list of per-file diffs.
///
/// Handles standard `git diff --no-color` output including:
/// - New files (`--- /dev/null`)
/// - Deleted files (`+++ /dev/null`)
/// - Renamed files (`rename from` / `rename to`)
/// - Binary files (`Binary files ... differ`)
pub(super) fn parse_unified_diff<'a>(output: &'a str) -> Vec<FileDiff<'a>> {
    let mut files: Vec<FileDiff<'a>> = Vec::new();
    let lines: Vec<&str> = output.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        // Skip lines until a diff header
        if !lines[i].starts_with("diff --git ") {
            i += 1;
            continue;
        }

        let (a_path, b_path) = parse_diff_git_header(lines[i]);
        i += 1;

        let (meta, next_i) = scan_extended_headers(&lines, i);
        i = next_i;

        let (status, path, old_path) = resolve_file_info(&a_path, &b_path, &meta);

        let hunks = if meta.is_binary {
            // Skip remaining lines for binary files
            while i < lines.len() && !lines[i].starts_with("diff --git ") {
                i += 1;
            }
            Vec::new()
        } else {
            let (h, next_i) = collect_hunks(&lines, i);
            i = next_i;
            h
        };

        files.push(FileDiff {
            path,
            old_path,
            status,
            hunks,
        });
    }

    files
}

/// Parse the `diff --git a/path b/path` header to extract both paths.
pub(super) fn parse_diff_git_header(line: &str) -> (String, String) {
    // Format: "diff --git a/path b/path"
    // Handle paths with spaces by splitting on " b/"
    let rest = line.strip_prefix("diff --git ").unwrap_or(line);

    // Find the boundary between a/path and b/path.
    // We use rfind so that paths containing " b/" in a directory name
    // are split at the *last* occurrence (which is the real separator).
    if let Some(pos) = rest.rfind(" b/") {
        let a_part = &rest[..pos];
        let b_part = &rest[pos + 1..];
        (a_part.to_string(), b_part.to_string())
    } else if let Some(pos) = rest.rfind(" b\\") {
        let a_part = &rest[..pos];
        let b_part = &rest[pos + 1..];
        (a_part.to_string(), b_part.to_string())
    } else {
        // Fallback: split on last space (handles unusual path formats)
        if let Some(pos) = rest.rfind(' ') {
            let a_part = &rest[..pos];
            let b_part = &rest[pos + 1..];
            (a_part.to_string(), b_part.to_string())
        } else {
            // No separator found — treat entire string as b-path
            (rest.to_string(), rest.to_string())
        }
    }
}

/// Strip the `a/` or `b/` prefix from a diff path.
pub(super) fn strip_ab_prefix(path: &str) -> String {
    if let Some(stripped) = path.strip_prefix("a/") {
        stripped.to_string()
    } else if let Some(stripped) = path.strip_prefix("b/") {
        stripped.to_string()
    } else {
        path.to_string()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Hunk header parsing tests (#103)
    // ========================================================================

    #[test]
    fn test_parse_hunk_header_basic() {
        let result = parse_hunk_header("@@ -1,5 +1,8 @@ function foo() {");
        assert_eq!(result, Some((1, 5, 1, 8)));
    }

    #[test]
    fn test_parse_hunk_header_single_line() {
        let result = parse_hunk_header("@@ -1 +1 @@");
        assert_eq!(result, Some((1, 1, 1, 1)));
    }

    #[test]
    fn test_parse_hunk_header_with_context_label() {
        let result = parse_hunk_header("@@ -10,3 +12,5 @@ impl MyStruct {");
        assert_eq!(result, Some((10, 3, 12, 5)));
    }

    #[test]
    fn test_parse_hunk_header_malformed() {
        assert!(parse_hunk_header("not a hunk header").is_none());
        assert!(parse_hunk_header("@@ invalid @@").is_none());
        assert!(parse_hunk_header("--- a/file.rs").is_none());
    }

    #[test]
    fn test_parse_hunk_header_zero_count() {
        // New file with no old content
        let result = parse_hunk_header("@@ -0,0 +1,12 @@");
        assert_eq!(result, Some((0, 0, 1, 12)));
    }

    #[test]
    fn test_parse_hunk_header_large_line_numbers() {
        let result = parse_hunk_header("@@ -1000,50 +1050,60 @@");
        assert_eq!(result, Some((1000, 50, 1050, 60)));
    }

    // ========================================================================
    // Unified diff parsing tests (#103)
    // ========================================================================

    #[test]
    fn test_parse_unified_diff_single_file() {
        let input = include_str!("../../../../tests/fixtures/cmd/diff/single_file.diff");
        let files = parse_unified_diff(input);

        assert_eq!(files.len(), 1, "expected 1 file");
        assert_eq!(files[0].path, "src/auth/middleware.ts");
        assert_eq!(files[0].status, DiffFileStatus::Modified);
        assert_eq!(files[0].hunks.len(), 1, "expected 1 hunk");
    }

    #[test]
    fn test_parse_unified_diff_multi_file() {
        let input = include_str!("../../../../tests/fixtures/cmd/diff/multi_file.diff");
        let files = parse_unified_diff(input);

        assert_eq!(files.len(), 2, "expected 2 files");
        assert_eq!(files[0].path, "src/api/routes.ts");
        assert_eq!(files[0].status, DiffFileStatus::Modified);
        assert_eq!(files[1].path, "src/api/handlers.ts");
        assert_eq!(files[1].status, DiffFileStatus::Modified);
        assert_eq!(files[1].hunks.len(), 2, "expected 2 hunks for handlers.ts");
    }

    #[test]
    fn test_parse_unified_diff_new_file() {
        let input = include_str!("../../../../tests/fixtures/cmd/diff/new_file.diff");
        let files = parse_unified_diff(input);

        assert_eq!(files.len(), 1, "expected 1 file");
        assert_eq!(files[0].path, "src/utils/validator.ts");
        assert_eq!(files[0].status, DiffFileStatus::Added);
        assert_eq!(files[0].hunks.len(), 1, "expected 1 hunk");
        // All lines should be additions
        assert!(
            files[0].hunks[0]
                .patch_lines
                .iter()
                .all(|l| l.starts_with('+')),
            "all lines in new file should be additions"
        );
    }

    #[test]
    fn test_parse_unified_diff_deleted_file() {
        let input = include_str!("../../../../tests/fixtures/cmd/diff/deleted_file.diff");
        let files = parse_unified_diff(input);

        assert_eq!(files.len(), 1, "expected 1 file");
        assert_eq!(files[0].path, "src/legacy/old_auth.ts");
        assert_eq!(files[0].status, DiffFileStatus::Deleted);
        // All lines should be deletions
        assert!(
            files[0].hunks[0]
                .patch_lines
                .iter()
                .all(|l| l.starts_with('-')),
            "all lines in deleted file should be deletions"
        );
    }

    #[test]
    fn test_parse_unified_diff_renamed_file() {
        let input = include_str!("../../../../tests/fixtures/cmd/diff/renamed_file.diff");
        let files = parse_unified_diff(input);

        assert_eq!(files.len(), 1, "expected 1 file");
        assert_eq!(files[0].path, "src/utils/format.ts");
        assert_eq!(files[0].status, DiffFileStatus::Renamed);
        assert_eq!(
            files[0].old_path.as_deref(),
            Some("src/utils/helpers.ts"),
            "expected old path for rename"
        );
    }

    #[test]
    fn test_parse_unified_diff_binary_file() {
        let input = include_str!("../../../../tests/fixtures/cmd/diff/binary_file.diff");
        let files = parse_unified_diff(input);

        assert_eq!(files.len(), 1, "expected 1 file");
        assert_eq!(files[0].path, "assets/logo.png");
        assert_eq!(files[0].status, DiffFileStatus::Binary);
        assert!(
            files[0].hunks.is_empty(),
            "binary files should have no hunks"
        );
    }

    // ========================================================================
    // File status detection tests (#103)
    // ========================================================================

    #[test]
    fn test_file_status_from_new_file() {
        let diff = "diff --git a/new.ts b/new.ts\nnew file mode 100644\nindex 0000000..abc1234\n--- /dev/null\n+++ b/new.ts\n@@ -0,0 +1,3 @@\n+line 1\n+line 2\n+line 3\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].status, DiffFileStatus::Added);
    }

    #[test]
    fn test_file_status_from_deleted_file() {
        let diff = "diff --git a/old.ts b/old.ts\ndeleted file mode 100644\nindex abc1234..0000000\n--- a/old.ts\n+++ /dev/null\n@@ -1,3 +0,0 @@\n-line 1\n-line 2\n-line 3\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].status, DiffFileStatus::Deleted);
    }

    #[test]
    fn test_file_status_modified() {
        let diff = "diff --git a/mod.ts b/mod.ts\nindex abc..def 100644\n--- a/mod.ts\n+++ b/mod.ts\n@@ -1,3 +1,4 @@\n line 1\n-line 2\n+line 2 modified\n+line 2b\n line 3\n";
        let files = parse_unified_diff(diff);
        assert_eq!(files[0].status, DiffFileStatus::Modified);
    }

    // ========================================================================
    // Hunk content extraction tests (#103)
    // ========================================================================

    #[test]
    fn test_hunk_content_single_file() {
        let input = include_str!("../../../../tests/fixtures/cmd/diff/single_file.diff");
        let files = parse_unified_diff(input);

        let hunk = &files[0].hunks[0];
        assert_eq!(hunk.old_start, 5);
        assert_eq!(hunk.old_count, 7);
        assert_eq!(hunk.new_start, 5);
        assert_eq!(hunk.new_count, 10);

        // Should contain both + and - lines
        let has_additions = hunk.patch_lines.iter().any(|l| l.starts_with('+'));
        let has_deletions = hunk.patch_lines.iter().any(|l| l.starts_with('-'));
        assert!(has_additions, "expected additions in hunk");
        assert!(has_deletions, "expected deletions in hunk");
    }

    #[test]
    fn test_hunk_content_new_file() {
        let input = include_str!("../../../../tests/fixtures/cmd/diff/new_file.diff");
        let files = parse_unified_diff(input);

        let hunk = &files[0].hunks[0];
        assert_eq!(hunk.old_start, 0);
        assert_eq!(hunk.old_count, 0);
        assert_eq!(hunk.new_start, 1);
        assert_eq!(hunk.new_count, 12);
    }

    // ========================================================================
    // Empty diff edge case (#103)
    // ========================================================================

    #[test]
    fn test_parse_unified_diff_empty() {
        let files = parse_unified_diff("");
        assert!(files.is_empty());
    }

    #[test]
    fn test_parse_unified_diff_whitespace_only() {
        let files = parse_unified_diff("  \n\n  \n");
        assert!(files.is_empty());
    }

    // ========================================================================
    // strip_ab_prefix tests (#103)
    // ========================================================================

    #[test]
    fn test_strip_ab_prefix() {
        assert_eq!(strip_ab_prefix("a/src/main.rs"), "src/main.rs");
        assert_eq!(strip_ab_prefix("b/src/main.rs"), "src/main.rs");
        assert_eq!(strip_ab_prefix("src/main.rs"), "src/main.rs");
        assert_eq!(strip_ab_prefix("/dev/null"), "/dev/null");
    }

    // ========================================================================
    // parse_diff_git_header tests (#103)
    // ========================================================================

    #[test]
    fn test_parse_diff_git_header_simple() {
        let (a, b) = parse_diff_git_header("diff --git a/src/main.rs b/src/main.rs");
        assert_eq!(a, "a/src/main.rs");
        assert_eq!(b, "b/src/main.rs");
    }

    #[test]
    fn test_parse_diff_git_header_different_paths() {
        let (a, b) = parse_diff_git_header("diff --git a/old/path.ts b/new/path.ts");
        assert_eq!(a, "a/old/path.ts");
        assert_eq!(b, "b/new/path.ts");
    }

    #[test]
    fn test_parse_diff_git_header_fallback_no_b_prefix() {
        // Unusual format without " b/" — falls back to last-space split
        let (a, b) = parse_diff_git_header("diff --git a-path b-path");
        assert_eq!(a, "a-path");
        assert_eq!(b, "b-path");
    }

    #[test]
    fn test_parse_diff_git_header_no_separator() {
        // Degenerate input with no space after stripping prefix
        let (a, b) = parse_diff_git_header("diff --git noseparator");
        // Both should be the same — no split possible
        assert_eq!(a, "noseparator");
        assert_eq!(b, "noseparator");
    }
}
