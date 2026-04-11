//! Git status compression — porcelain v2 parsing.

use std::process::ExitCode;

use crate::cmd::extract_output_format;
use crate::output::canonical::GitResult;

use super::run_parsed_command;

/// Returns `true` for flags that conflict with the `--porcelain=v2` flag the
/// handler injects.  These are `-s`, `--short`, `--porcelain`, and any
/// `--porcelain=*` variant.
fn is_conflicting_status_flag(s: &str) -> bool {
    s == "-s" || s == "--short" || s == "--porcelain" || s.starts_with("--porcelain=")
}

/// Run `git status` with compression.
///
/// Strips user-supplied format flags (`-s`, `--short`, `--porcelain`,
/// `--porcelain=*`) before forwarding to git so they cannot conflict with the
/// `--porcelain=v2` flag that the handler injects for structured parsing.
pub(super) fn run_status(
    global_flags: &[String],
    args: &[String],
    show_stats: bool,
) -> anyhow::Result<ExitCode> {
    // Strip conflicting format flags — handler injects --porcelain=v2 itself.
    let stripped_args: Vec<String> = args
        .iter()
        .filter(|a| !is_conflicting_status_flag(a.as_str()))
        .cloned()
        .collect();

    let (filtered_args, output_format) = extract_output_format(&stripped_args);

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend([
        "status".to_string(),
        "--porcelain=v2".to_string(),
        "--branch".to_string(),
    ]);
    full_args.extend_from_slice(&filtered_args);

    let label = super::build_analytics_label("status", args, show_stats);

    run_parsed_command(&full_args, show_stats, output_format, false, label, parse_status)
}

/// Accumulated per-category file lists from a porcelain v2 status parse.
#[derive(Default)]
struct StatusCategories {
    branch: String,
    staged: Vec<String>,
    modified: Vec<String>,
    untracked: Vec<String>,
    renamed: Vec<String>,
    unmerged: Vec<String>,
}

impl StatusCategories {
    /// Classify and accumulate a single porcelain v2 output line.
    fn classify_line(&mut self, line: &str) {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            self.branch = head.to_string();
            return;
        }

        if line.starts_with('#') {
            return;
        }

        if line.starts_with('?') {
            // Untracked: "? <path>"
            self.untracked
                .push(line.get(2..).unwrap_or_default().to_string());
            return;
        }

        if line.starts_with('u') {
            // Unmerged: "u <xy> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>"
            self.unmerged.push(extract_last_path(line));
            return;
        }

        if line.starts_with('2') {
            // Renamed/copied: "2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X_score> <path>\t<origPath>"
            self.renamed.push(extract_renamed_path(line));
            return;
        }

        if line.starts_with('1') {
            // Tracked changed: "1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>"
            // XY: index change (X) and worktree change (Y)
            let xy = extract_xy(line);
            let path = extract_last_path(line);

            let x = xy.chars().next().unwrap_or('.');
            let y = xy.chars().nth(1).unwrap_or('.');

            if x != '.' {
                self.staged.push(format!("{}{}", stage_prefix(x), path));
            }
            if y != '.' {
                self.modified
                    .push(format!("{}{}", worktree_prefix(y), path));
            }
        }
    }

    /// Build a compressed GitResult from the accumulated categories.
    ///
    /// Shows ALL files — no cap per GRANITE #618 lesson.
    fn build_result(self) -> GitResult {
        let mut details: Vec<String> = Vec::new();

        if !self.branch.is_empty() {
            details.push(format!("branch: {}", self.branch));
        }
        for f in &self.staged {
            details.push(format!("staged: {f}"));
        }
        for f in &self.modified {
            details.push(format!("modified: {f}"));
        }
        for f in &self.untracked {
            details.push(format!("untracked: {f}"));
        }
        for f in &self.renamed {
            details.push(format!("renamed: {f}"));
        }
        for f in &self.unmerged {
            details.push(format!("unmerged: {f}"));
        }

        let total_changes = self.staged.len()
            + self.modified.len()
            + self.untracked.len()
            + self.renamed.len()
            + self.unmerged.len();

        let summary = if total_changes == 0 {
            "clean".to_string()
        } else {
            let mut parts: Vec<String> = Vec::new();
            if !self.staged.is_empty() {
                parts.push(format!("{} staged", self.staged.len()));
            }
            if !self.modified.is_empty() {
                parts.push(format!("{} modified", self.modified.len()));
            }
            if !self.untracked.is_empty() {
                parts.push(format!("{} untracked", self.untracked.len()));
            }
            if !self.renamed.is_empty() {
                parts.push(format!("{} renamed", self.renamed.len()));
            }
            if !self.unmerged.is_empty() {
                parts.push(format!("{} unmerged", self.unmerged.len()));
            }
            parts.join(", ")
        };

        GitResult::new("status".to_string(), summary, details)
    }
}

/// Parse porcelain v2 status output into a compressed GitResult.
fn parse_status(output: &str) -> GitResult {
    let mut cats = StatusCategories::default();
    for line in output.lines() {
        cats.classify_line(line);
    }
    cats.build_result()
}

/// Extract XY field from porcelain v2 tracked entry line.
/// Format: "1 <XY> <rest...>"
fn extract_xy(line: &str) -> String {
    line.split_whitespace().nth(1).unwrap_or("..").to_string()
}

/// Extract the path from a porcelain v2 line using fixed field counts.
///
/// Type 1 entries: "1 XY sub mH mI mW hH hI <path>" (8 fields before path)
/// Unmerged entries: "u XY sub m1 m2 m3 mW h1 h2 h3 <path>" (10 fields before path)
///
/// Uses `splitn` with the correct field count so paths with spaces are preserved.
fn extract_last_path(line: &str) -> String {
    let field_count = if line.starts_with('u') {
        // Unmerged: 10 fixed fields + path
        11
    } else {
        // Type 1: 8 fixed fields + path
        9
    };

    let fields: Vec<&str> = line.splitn(field_count, ' ').collect();
    fields.last().unwrap_or(&"").to_string()
}

/// Extract the renamed path from a porcelain v2 type 2 entry.
/// Format: "2 XY sub mH mI mW hH hI X_score <path>\t<origPath>"
fn extract_renamed_path(line: &str) -> String {
    // Porcelain v2 type-2 entries always contain a tab separator:
    // "2 XY sub mH mI mW hH hI X_score <path>\t<origPath>"
    let Some(tab_pos) = line.find('\t') else {
        // Malformed input: no tab found; fall back to the path portion after the prefix
        return line.get(2..).unwrap_or_default().to_string();
    };
    let before_tab = &line[..tab_pos];
    let after_tab = &line[tab_pos + 1..];
    // Field 10 (0-indexed 9) is the new path; use splitn to preserve spaces in path
    let new_path = before_tab.splitn(10, ' ').last().unwrap_or_default();
    format!("{after_tab} -> {new_path}")
}

/// Map index status character to a display prefix.
fn stage_prefix(c: char) -> &'static str {
    match c {
        'M' => "M ",
        'A' => "A ",
        'D' => "D ",
        'R' => "R ",
        'C' => "C ",
        _ => "",
    }
}

/// Map worktree status character to a display prefix.
fn worktree_prefix(c: char) -> &'static str {
    match c {
        'M' => "M ",
        'D' => "D ",
        _ => "",
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use std::sync::LazyLock;

    /// Matches diff stat lines: " file | 42 +++++---"
    static STAT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*(.+?)\s+\|\s+(\d+)\s+([+-]+)").unwrap());

    /// Matches diff stat summary lines: "3 files changed, ..."
    static SUMMARY_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(\d+)\s+files?\s+changed").unwrap());

    /// Matches binary diff stat lines: " file.bin | Bin 0 -> 1234 bytes"
    static BINARY_STAT_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*(.+?)\s+\|\s+Bin\s+").unwrap());

    /// Parse `git diff --stat` output into a compressed GitResult.
    ///
    /// Retained for testing and potential future use (e.g., `--mode stat`).
    fn parse_diff_stat(output: &str) -> GitResult {
        let mut file_stats: Vec<String> = Vec::new();
        let mut summary_line = String::new();

        for line in output.lines() {
            if let Some(caps) = STAT_RE.captures(line) {
                let file = caps.get(1).map_or("", |m| m.as_str()).trim();
                let count = caps.get(2).map_or("", |m| m.as_str());
                let changes = caps.get(3).map_or("", |m| m.as_str());
                file_stats.push(format!("{file} | {count} {changes}"));
                continue;
            }

            // Binary files appear as "file.bin | Bin 0 -> 1234 bytes"
            if let Some(caps) = BINARY_STAT_RE.captures(line) {
                let file = caps.get(1).map_or("", |m| m.as_str()).trim();
                file_stats.push(format!("{file} | Bin"));
                continue;
            }

            if SUMMARY_RE.is_match(line) {
                summary_line = line.trim().to_string();
            }
        }

        if summary_line.is_empty() && file_stats.is_empty() {
            return GitResult::new("diff".to_string(), "no changes".to_string(), vec![]);
        }

        if summary_line.is_empty() {
            summary_line = format!("{} files changed", file_stats.len());
        }

        GitResult::new("diff".to_string(), summary_line, file_stats)
    }

    // ========================================================================
    // parse_status tests
    // ========================================================================

    #[test]
    fn test_parse_status_clean() {
        let output = "# branch.oid abc123\n# branch.head main\n";
        let result = parse_status(output);
        assert_eq!(result.summary, "clean");
        // Details should contain branch info only
        assert!(result.details.iter().any(|d| d.contains("branch: main")));
    }

    #[test]
    fn test_parse_status_dirty() {
        let output = include_str!("../../../tests/fixtures/cmd/git/status_dirty.txt");
        let result = parse_status(output);

        // Verify summary contains counts
        assert!(
            result.summary.contains("staged"),
            "expected 'staged' in summary, got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("modified"),
            "expected 'modified' in summary, got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("untracked"),
            "expected 'untracked' in summary, got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("renamed"),
            "expected 'renamed' in summary, got: {}",
            result.summary
        );
    }

    #[test]
    fn test_parse_status_shows_all_files() {
        // Generate 25 untracked files — ensure no cap
        let mut output = String::from("# branch.head main\n");
        for i in 0..25 {
            output.push_str(&format!("? file_{i}.txt\n"));
        }

        let result = parse_status(&output);

        // All 25 should appear in details (no 5/5/3 cap like GRANITE #618)
        let untracked_count = result
            .details
            .iter()
            .filter(|d| d.starts_with("untracked:"))
            .count();
        assert_eq!(
            untracked_count, 25,
            "expected all 25 untracked files, got {untracked_count}"
        );
    }

    // ========================================================================
    // parse_diff_stat tests
    // ========================================================================

    #[test]
    fn test_parse_diff_stat() {
        let output = include_str!("../../../tests/fixtures/cmd/git/diff_stat.txt");
        let result = parse_diff_stat(output);

        assert!(
            result.summary.contains("3 files changed"),
            "expected '3 files changed' in summary, got: {}",
            result.summary
        );
        assert_eq!(result.details.len(), 3, "expected 3 file stat entries");
    }

    #[test]
    fn test_parse_diff_stat_empty() {
        let result = parse_diff_stat("");
        assert_eq!(result.summary, "no changes");
        assert!(result.details.is_empty());
    }

    // ========================================================================
    // Paths with spaces in git status
    // ========================================================================

    #[test]
    fn test_extract_last_path_with_spaces() {
        // Type 1 entry with space in path: 8 fixed fields + path
        let line = "1 M. N... 100644 100644 100644 abc1234 def5678 src/my file.rs";
        assert_eq!(extract_last_path(line), "src/my file.rs");
    }

    #[test]
    fn test_parse_status_path_with_spaces() {
        let output = "# branch.head main\n\
                      1 M. N... 100644 100644 100644 abc1234 def5678 src/my file.rs\n";
        let result = parse_status(output);
        assert!(
            result.details.iter().any(|d| d.contains("my file.rs")),
            "expected path with spaces in details, got: {:?}",
            result.details
        );
    }

    // ========================================================================
    // Unmerged entries in status
    // ========================================================================

    #[test]
    fn test_parse_status_unmerged_entries() {
        let output = "# branch.head main\n\
                      u UU N... 100644 100644 100644 100644 abc1234 def5678 ghi9012 src/conflict.rs\n";
        let result = parse_status(output);
        assert!(
            result.summary.contains("unmerged"),
            "expected 'unmerged' in summary, got: {}",
            result.summary
        );
        assert!(
            result
                .details
                .iter()
                .any(|d| d.contains("unmerged:") && d.contains("conflict.rs")),
            "expected unmerged detail for conflict.rs, got: {:?}",
            result.details
        );
    }

    // ========================================================================
    // Fallback guarantee: parse_status must not panic on unexpected input
    // (Step 7e)
    // ========================================================================

    #[test]
    fn test_parse_status_garbage_input_no_panic() {
        // Feed unexpected garbage — must produce a valid result, never panic.
        // The parser may interpret "unexpected garbage input" as an unmerged line
        // (because the line starts with 'u'). The key contract is: no panic, valid result.
        let result = parse_status("unexpected garbage input");
        // Must return a well-formed GitResult (non-empty summary, valid details vec).
        assert!(!result.summary.is_empty(), "Summary must not be empty");
        // Details may or may not be populated depending on how the line is classified.
        // We just verify the result is structurally valid (no panic, fields accessible).
        let _ = result.details.len();
    }

    #[test]
    fn test_parse_status_null_bytes_no_panic() {
        // Feed null bytes and other control characters — must not panic.
        let result = parse_status("\x00\x01\x02\x03 binary garbage");
        assert!(!result.summary.is_empty(), "summary must be non-empty");
    }

    #[test]
    fn test_parse_status_partial_v2_line_no_panic() {
        // Truncated porcelain v2 lines — handler must degrade gracefully.
        let output = "# branch.head main\n1\n1 M\n1 M. \n";
        let result = parse_status(output);
        assert!(
            result.details.iter().any(|d| d.contains("branch: main")),
            "branch line should still be parsed"
        );
        // Truncated type-1 lines should produce no panics, possibly no detail entries.
    }

    // ========================================================================
    // Flag-stripping predicate
    // ========================================================================

    /// Thin test wrapper around the module-scope predicate.
    fn strip_conflicting_flags(args: &[&str]) -> Vec<String> {
        args.iter()
            .filter(|a| !is_conflicting_status_flag(a))
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn test_flag_stripping_removes_conflicting_flags() {
        // Each of these must be stripped individually.
        assert!(
            strip_conflicting_flags(&["-s"]).is_empty(),
            "-s must be stripped"
        );
        assert!(
            strip_conflicting_flags(&["--short"]).is_empty(),
            "--short must be stripped"
        );
        assert!(
            strip_conflicting_flags(&["--porcelain"]).is_empty(),
            "--porcelain must be stripped"
        );
        assert!(
            strip_conflicting_flags(&["--porcelain=v1"]).is_empty(),
            "--porcelain=v1 must be stripped"
        );
        assert!(
            strip_conflicting_flags(&["--porcelain=v2"]).is_empty(),
            "--porcelain=v2 must be stripped"
        );
    }

    #[test]
    fn test_flag_stripping_preserves_non_conflicting_flags() {
        let input = ["--branch", "--", "path/to/file"];
        let result = strip_conflicting_flags(&input);
        assert_eq!(
            result,
            vec!["--branch", "--", "path/to/file"],
            "non-conflicting flags must be preserved"
        );
    }

    #[test]
    fn test_flag_stripping_mixed_input() {
        // Conflicting flags are stripped; non-conflicting flags are preserved.
        let input = [
            "-s",
            "--branch",
            "--short",
            "--",
            "--porcelain=v1",
            "path/to/file",
        ];
        let result = strip_conflicting_flags(&input);
        assert_eq!(
            result,
            vec!["--branch", "--", "path/to/file"],
            "only conflicting flags must be stripped from mixed input"
        );
    }

    // ========================================================================
    // Binary files in diff stat
    // ========================================================================

    #[test]
    fn test_parse_diff_stat_binary_files() {
        let output = " src/main.rs   | 15 +++++++++------\n\
                       image.png     | Bin 0 -> 1234 bytes\n\
                       2 files changed, 10 insertions(+), 5 deletions(-)\n";
        let result = parse_diff_stat(output);
        assert_eq!(
            result.details.len(),
            2,
            "expected 2 file stat entries (1 text + 1 binary), got: {:?}",
            result.details
        );
        assert!(
            result
                .details
                .iter()
                .any(|d| d.contains("image.png") && d.contains("Bin")),
            "expected binary file entry, got: {:?}",
            result.details
        );
    }
}
