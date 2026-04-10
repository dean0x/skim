//! Unified diff parsing — hunk extraction and file status detection.

use std::sync::LazyLock;

use regex::Regex;

use super::types::{DiffHunk, FileChange, FileDiff, FileMetadata};
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
        change: FileChange::Modified,
        file_minus: String::new(),
        file_plus: String::new(),
    };

    let mut i = start;
    while i < lines.len() && !lines[i].starts_with("diff --git ") {
        let line = lines[i];

        if line.starts_with("new file mode") {
            meta.change = FileChange::New;
        } else if line.starts_with("deleted file mode") {
            meta.change = FileChange::Deleted;
        } else if line.starts_with("rename from ") {
            let from = line
                .strip_prefix("rename from ")
                .unwrap_or_default()
                .to_string();
            meta.change = FileChange::Renamed { from: Some(from) };
        } else if line.starts_with("rename to ") {
            // Only update if not already set to Renamed (rename from comes first)
            if !matches!(meta.change, FileChange::Renamed { .. }) {
                meta.change = FileChange::Renamed { from: None };
            }
        } else if line.starts_with("Binary files") && line.contains("differ") {
            meta.change = FileChange::Binary;
        } else if line.starts_with("--- ") {
            meta.file_minus = line.strip_prefix("--- ").unwrap_or_default().to_string();
        } else if line.starts_with("+++ ") {
            meta.file_plus = line.strip_prefix("+++ ").unwrap_or_default().to_string();
        } else if line.starts_with("@@") {
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
        let line = lines[i];

        if line.starts_with("@@") {
            if let Some((old_start, old_count, new_start, new_count)) = parse_hunk_header(line) {
                let mut patch_lines: Vec<&'a str> = Vec::new();
                i += 1;

                while i < lines.len() {
                    let patch_line = lines[i];
                    if patch_line.starts_with("diff --git ") || patch_line.starts_with("@@") {
                        break;
                    }
                    // Only keep actual patch lines (+, -, space, or \ no newline)
                    if patch_line.starts_with('+')
                        || patch_line.starts_with('-')
                        || patch_line.starts_with(' ')
                        || patch_line.starts_with('\\')
                    {
                        patch_lines.push(patch_line);
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
    let status = match &meta.change {
        FileChange::Binary => DiffFileStatus::Binary,
        FileChange::New => DiffFileStatus::Added,
        FileChange::Deleted => DiffFileStatus::Deleted,
        FileChange::Renamed { .. } => DiffFileStatus::Renamed,
        FileChange::Modified => {
            // Also check --- / +++ paths as fallback for diffs without explicit headers
            if meta.file_minus == "/dev/null" || meta.file_minus == "a//dev/null" {
                DiffFileStatus::Added
            } else if meta.file_plus == "/dev/null" || meta.file_plus == "b//dev/null" {
                DiffFileStatus::Deleted
            } else {
                DiffFileStatus::Modified
            }
        }
    };

    let path = if status == DiffFileStatus::Deleted {
        strip_ab_prefix(a_path)
    } else {
        strip_ab_prefix(b_path)
    };

    let old_path = if let FileChange::Renamed { from } = &meta.change {
        from.clone().or_else(|| Some(strip_ab_prefix(a_path)))
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
pub(in crate::cmd::git) fn parse_unified_diff<'a>(output: &'a str) -> Vec<FileDiff<'a>> {
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

        let hunks = if meta.change == FileChange::Binary {
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
    // Try " b/" first, then " b\" (Windows), then fall back to last space.
    let sep = rest
        .rfind(" b/")
        .or_else(|| rest.rfind(" b\\"))
        .or_else(|| rest.rfind(' '));
    if let Some(pos) = sep {
        (rest[..pos].to_string(), rest[pos + 1..].to_string())
    } else {
        // No separator found — treat entire string as b-path
        (rest.to_string(), rest.to_string())
    }
}

/// Strip the `a/` or `b/` prefix from a diff path.
pub(super) fn strip_ab_prefix(path: &str) -> String {
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
        .to_string()
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

    // ========================================================================
    // scan_extended_headers direct unit tests (parse:51:testing)
    // ========================================================================

    #[test]
    fn test_scan_extended_headers_new_file() {
        let lines = vec![
            "new file mode 100644",
            "index 0000000..abc1234",
            "--- /dev/null",
            "+++ b/src/new.rs",
        ];
        let (meta, idx) = scan_extended_headers(&lines, 0);
        assert_eq!(meta.change, FileChange::New);
        assert_eq!(meta.file_minus, "/dev/null");
        assert_eq!(meta.file_plus, "b/src/new.rs");
        assert_eq!(idx, lines.len(), "should consume all lines");
    }

    #[test]
    fn test_scan_extended_headers_deleted_file() {
        let lines = vec![
            "deleted file mode 100644",
            "index abc1234..0000000",
            "--- a/src/old.rs",
            "+++ /dev/null",
        ];
        let (meta, idx) = scan_extended_headers(&lines, 0);
        assert_eq!(meta.change, FileChange::Deleted);
        assert_eq!(meta.file_minus, "a/src/old.rs");
        assert_eq!(meta.file_plus, "/dev/null");
        assert_eq!(idx, lines.len());
    }

    #[test]
    fn test_scan_extended_headers_rename_with_from() {
        let lines = vec![
            "similarity index 95%",
            "rename from src/utils/helpers.ts",
            "rename to src/utils/format.ts",
            "index abc..def 100644",
            "--- a/src/utils/helpers.ts",
            "+++ b/src/utils/format.ts",
        ];
        let (meta, idx) = scan_extended_headers(&lines, 0);
        assert_eq!(
            meta.change,
            FileChange::Renamed {
                from: Some("src/utils/helpers.ts".to_string())
            }
        );
        assert_eq!(idx, lines.len());
    }

    #[test]
    fn test_scan_extended_headers_rename_to_only() {
        // `rename to` without a preceding `rename from` — sets Renamed { from: None }
        let lines = vec!["rename to src/new.rs"];
        let (meta, _) = scan_extended_headers(&lines, 0);
        assert_eq!(meta.change, FileChange::Renamed { from: None });
    }

    #[test]
    fn test_scan_extended_headers_binary() {
        let lines = vec!["Binary files a/img.png and b/img.png differ"];
        let (meta, idx) = scan_extended_headers(&lines, 0);
        assert_eq!(meta.change, FileChange::Binary);
        assert_eq!(idx, 1);
    }

    #[test]
    fn test_scan_extended_headers_stops_at_hunk_header() {
        let lines = vec![
            "index abc..def 100644",
            "--- a/src/main.rs",
            "+++ b/src/main.rs",
            "@@ -1,3 +1,4 @@",
            " context",
        ];
        let (meta, idx) = scan_extended_headers(&lines, 0);
        assert_eq!(meta.change, FileChange::Modified);
        // Must stop before consuming the @@ line
        assert_eq!(idx, 3, "should stop at the @@ line");
    }

    #[test]
    fn test_scan_extended_headers_stops_at_next_diff_header() {
        let lines = vec!["index abc..def 100644", "diff --git a/other.rs b/other.rs"];
        let (meta, idx) = scan_extended_headers(&lines, 0);
        assert_eq!(meta.change, FileChange::Modified);
        assert_eq!(idx, 1, "should stop at the diff --git line");
    }

    #[test]
    fn test_scan_extended_headers_start_offset() {
        // Verify the `start` parameter is respected
        let lines = vec![
            "diff --git a/ignore.rs b/ignore.rs",
            "new file mode 100644",
            "--- /dev/null",
            "+++ b/ignore.rs",
        ];
        let (meta, idx) = scan_extended_headers(&lines, 1);
        assert_eq!(meta.change, FileChange::New);
        assert_eq!(idx, lines.len());
    }

    // ========================================================================
    // collect_hunks direct unit tests (parse:97:testing)
    // ========================================================================

    #[test]
    fn test_collect_hunks_single_hunk() {
        let lines = vec![
            "@@ -1,3 +1,4 @@",
            " context",
            "-old line",
            "+new line",
            "+added line",
            " context",
        ];
        let (hunks, idx) = collect_hunks(&lines, 0);
        assert_eq!(hunks.len(), 1);
        let h = &hunks[0];
        assert_eq!(h.old_start, 1);
        assert_eq!(h.old_count, 3);
        assert_eq!(h.new_start, 1);
        assert_eq!(h.new_count, 4);
        assert_eq!(h.patch_lines.len(), 5);
        assert_eq!(idx, lines.len());
    }

    #[test]
    fn test_collect_hunks_two_hunks() {
        let lines = vec![
            "@@ -1,2 +1,2 @@",
            "-a",
            "+b",
            "@@ -10,2 +10,3 @@",
            " ctx",
            "+new",
            " ctx",
        ];
        let (hunks, idx) = collect_hunks(&lines, 0);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].old_start, 1);
        assert_eq!(hunks[1].old_start, 10);
        assert_eq!(hunks[1].new_count, 3);
        assert_eq!(idx, lines.len());
    }

    #[test]
    fn test_collect_hunks_stops_at_next_diff_header() {
        let lines = vec![
            "@@ -1,1 +1,1 @@",
            "-x",
            "+y",
            "diff --git a/other.rs b/other.rs",
            "@@ -5,1 +5,1 @@",
        ];
        let (hunks, idx) = collect_hunks(&lines, 0);
        assert_eq!(hunks.len(), 1, "should stop before next diff --git header");
        assert_eq!(idx, 3, "should point at the diff --git line");
    }

    #[test]
    fn test_collect_hunks_skips_non_patch_lines() {
        // Lines without +/-/space/\ prefix should be silently skipped
        let lines = vec!["@@ -1,1 +1,1 @@", "index abc..def", "+added"];
        let (hunks, _) = collect_hunks(&lines, 0);
        assert_eq!(hunks[0].patch_lines.len(), 1);
        assert_eq!(hunks[0].patch_lines[0], "+added");
    }

    #[test]
    fn test_collect_hunks_empty_input() {
        let (hunks, idx) = collect_hunks(&[], 0);
        assert!(hunks.is_empty());
        assert_eq!(idx, 0);
    }

    // ========================================================================
    // resolve_file_info direct unit tests (parse:143:testing)
    // ========================================================================

    #[test]
    fn test_resolve_file_info_binary() {
        let meta = FileMetadata {
            change: FileChange::Binary,
            file_minus: String::new(),
            file_plus: String::new(),
        };
        let (status, path, old_path) = resolve_file_info("a/img.png", "b/img.png", &meta);
        assert_eq!(status, DiffFileStatus::Binary);
        assert_eq!(path, "img.png");
        assert!(old_path.is_none());
    }

    #[test]
    fn test_resolve_file_info_new_file() {
        let meta = FileMetadata {
            change: FileChange::New,
            file_minus: "/dev/null".to_string(),
            file_plus: "b/src/new.rs".to_string(),
        };
        let (status, path, old_path) = resolve_file_info("a/src/new.rs", "b/src/new.rs", &meta);
        assert_eq!(status, DiffFileStatus::Added);
        assert_eq!(path, "src/new.rs");
        assert!(old_path.is_none());
    }

    #[test]
    fn test_resolve_file_info_deleted_file_uses_a_path() {
        let meta = FileMetadata {
            change: FileChange::Deleted,
            file_minus: "a/src/old.rs".to_string(),
            file_plus: "/dev/null".to_string(),
        };
        let (status, path, old_path) = resolve_file_info("a/src/old.rs", "b/src/old.rs", &meta);
        assert_eq!(status, DiffFileStatus::Deleted);
        assert_eq!(path, "src/old.rs");
        assert!(old_path.is_none());
    }

    #[test]
    fn test_resolve_file_info_renamed_with_from() {
        let meta = FileMetadata {
            change: FileChange::Renamed {
                from: Some("src/utils/helpers.ts".to_string()),
            },
            file_minus: "a/src/utils/helpers.ts".to_string(),
            file_plus: "b/src/utils/format.ts".to_string(),
        };
        let (status, path, old_path) =
            resolve_file_info("a/src/utils/helpers.ts", "b/src/utils/format.ts", &meta);
        assert_eq!(status, DiffFileStatus::Renamed);
        assert_eq!(path, "src/utils/format.ts");
        assert_eq!(old_path.as_deref(), Some("src/utils/helpers.ts"));
    }

    #[test]
    fn test_resolve_file_info_renamed_without_from_falls_back_to_a_path() {
        // `rename to` only — no `rename from` header captured
        let meta = FileMetadata {
            change: FileChange::Renamed { from: None },
            file_minus: "a/src/old.rs".to_string(),
            file_plus: "b/src/new.rs".to_string(),
        };
        let (status, path, old_path) = resolve_file_info("a/src/old.rs", "b/src/new.rs", &meta);
        assert_eq!(status, DiffFileStatus::Renamed);
        assert_eq!(path, "src/new.rs");
        // Falls back to stripping a_path
        assert_eq!(old_path.as_deref(), Some("src/old.rs"));
    }

    #[test]
    fn test_resolve_file_info_modified_fallback_dev_null_plus() {
        // No mode header, but +++ /dev/null signals deletion
        let meta = FileMetadata {
            change: FileChange::Modified,
            file_minus: "a/src/old.rs".to_string(),
            file_plus: "/dev/null".to_string(),
        };
        let (status, path, _) = resolve_file_info("a/src/old.rs", "b/src/old.rs", &meta);
        assert_eq!(status, DiffFileStatus::Deleted);
        assert_eq!(path, "src/old.rs");
    }

    #[test]
    fn test_resolve_file_info_modified_fallback_dev_null_minus() {
        // No mode header, but --- /dev/null signals addition
        let meta = FileMetadata {
            change: FileChange::Modified,
            file_minus: "/dev/null".to_string(),
            file_plus: "b/src/new.rs".to_string(),
        };
        let (status, path, _) = resolve_file_info("a/src/new.rs", "b/src/new.rs", &meta);
        assert_eq!(status, DiffFileStatus::Added);
        assert_eq!(path, "src/new.rs");
    }

    // ========================================================================
    // parse_diff_git_header rfind edge case (parse:205:regression)
    // ========================================================================

    /// Regression test: a path where a directory component is literally named "b"
    /// (e.g. `src/b/foo.rs`). With `find` the first " b/" occurrence would split
    /// incorrectly; `rfind` always finds the true a/b separator.
    #[test]
    fn test_parse_diff_git_header_path_with_b_directory() {
        // Both sides have "src/b/" as a directory — the separator is the *last* " b/"
        let (a, b) = parse_diff_git_header("diff --git a/src/b/foo.rs b/src/b/foo.rs");
        assert_eq!(a, "a/src/b/foo.rs");
        assert_eq!(b, "b/src/b/foo.rs");
    }

    #[test]
    fn test_parse_diff_git_header_multiple_b_components() {
        // Deeper nesting: a/b/b/file.rs  b/b/b/file.rs
        let (a, b) = parse_diff_git_header("diff --git a/b/b/file.rs b/b/b/file.rs");
        assert_eq!(a, "a/b/b/file.rs");
        assert_eq!(b, "b/b/b/file.rs");
    }
}
