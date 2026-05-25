//! diff parser with `-u` injection and unified diff output parsing.
//!
//! Parses standalone `diff -u` output into structured `FileResult`.
//!
//! IMPORTANT: `parse_unified_diff` from `cmd::git::diff` expects `diff --git a/path b/path`
//! headers. Standalone `diff -u` produces `--- file1` / `+++ file2` headers instead.
//! This module implements a LOCAL parser `try_parse_standalone_unified` that handles
//! the standalone format — do NOT reuse `parse_unified_diff` here.
//!
//! Exit code semantics:
//! - 0: Files are identical (no output)
//! - 1: Files differ (normal diff output)
//! - 2: Error (e.g., file not found) → Passthrough
//!
//! Tiers:
//! - **Tier 1 (Full)**: Parse unified diff, count insertions/deletions per file
//! - **Tier 3 (Passthrough)**: Exit code 2 (error) or unrecognized output

use std::process::ExitCode;

use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

use super::MAX_DISPLAY_ENTRIES;
use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "diff",
    env_overrides: &[],
    install_hint: "diff is typically pre-installed on Unix systems",
    family: "file",
    skip_ansi_strip: false,
    command_type: CommandType::FileOps,
};

/// Run `skim diff [args...]`.
pub(crate) fn run(args: &[String], ctx: &crate::cmd::RunContext) -> anyhow::Result<ExitCode> {
    run_tool(CONFIG, args, ctx, prepare_args, parse_impl)
}

/// Inject `-u` (unified diff) if not already present.
fn prepare_args(args: &mut Vec<String>) {
    let already_has_unified = args.iter().any(|a| {
        a == "-u" || a == "--unified" || a.starts_with("-U") || a.starts_with("--unified=")
    });
    if !already_has_unified {
        // Insert at the beginning so it precedes file arguments
        args.insert(0, "-u".to_string());
    }
}

/// Three-tier parse function for diff output.
fn parse_impl(output: &CommandOutput) -> ParseResult<FileResult> {
    // Exit code 2 means error (e.g., missing file) → passthrough
    if output.exit_code == Some(2) {
        return ParseResult::Passthrough(output.stdout.clone());
    }

    // Exit code 0 with empty stdout: files are identical
    if output.exit_code == Some(0) && output.stdout.trim().is_empty() {
        let result = FileResult::new(
            "diff".to_string(),
            0,
            0,
            vec!["files are identical".to_string()],
            None,
        );
        return ParseResult::Full(result);
    }

    if let Some(result) = try_parse_standalone_unified(&output.stdout) {
        return ParseResult::Full(result);
    }

    ParseResult::Passthrough(output.stdout.clone())
}

// ============================================================================
// Tier 1: standalone unified diff parser
//
// Handles `diff -u file1 file2` and `diff -ru dir1 dir2` output.
// Does NOT handle `git diff` output (which has `diff --git a/path b/path` headers).
// ============================================================================

/// Per-file diff statistics.
struct FileStat {
    path: String,
    insertions: usize,
    deletions: usize,
}

/// Mutable accumulator state for the standalone unified diff parser.
struct DiffParserState {
    file_stats: Vec<FileStat>,
    current_path: Option<String>,
    current_insertions: usize,
    current_deletions: usize,
    in_hunk: bool,
}

impl DiffParserState {
    fn new() -> Self {
        Self {
            file_stats: Vec::new(),
            current_path: None,
            current_insertions: 0,
            current_deletions: 0,
            in_hunk: false,
        }
    }

    /// Flush the current in-progress file stat and reset all accumulators.
    fn flush_current(&mut self) {
        if let Some(path) = self.current_path.take() {
            self.file_stats.push(FileStat {
                path,
                insertions: self.current_insertions,
                deletions: self.current_deletions,
            });
        }
        self.current_insertions = 0;
        self.current_deletions = 0;
        self.in_hunk = false;
    }
}

fn try_parse_standalone_unified(stdout: &str) -> Option<FileResult> {
    if stdout.trim().is_empty() {
        return None;
    }

    let mut state = DiffParserState::new();

    for line in stdout.lines() {
        // `diff -ru` recursive header line: "diff -ru dir1/file dir2/file"
        // This precedes the --- / +++ headers; skip it but use it as a hint
        if line.starts_with("diff ") && !line.starts_with("diff --git ") {
            state.flush_current();
            continue;
        }

        // `--- path\tdate` header — marks start of a new file diff.
        // Note: standalone `diff -u` uses literal file paths (which may begin
        // with any prefix). We do NOT skip `a/` paths here — that would
        // incorrectly reject diffs between files in directories named `a/`.
        // Git-format diffs are identified by the `diff --git` line, not by
        // the `a/` prefix in `---` / `+++` lines.
        if let Some(rest) = line.strip_prefix("--- ") {
            state.flush_current();
            // Extract path (strip optional tab+timestamp suffix)
            let path = rest.split('\t').next().unwrap_or(rest).trim().to_string();
            state.current_path = Some(path);
            continue;
        }

        // `+++ path\tdate` — confirms the new-file side; update path if `---` was /dev/null
        if let Some(rest) = line.strip_prefix("+++ ") {
            // If the old path was /dev/null, use the new path
            if state.current_path.as_deref() == Some("/dev/null") {
                let path = rest.split('\t').next().unwrap_or(rest).trim().to_string();
                state.current_path = Some(path);
            }
            continue;
        }

        // `@@ ... @@` hunk header — entering a hunk
        if line.starts_with("@@ ") {
            state.in_hunk = true;
            continue;
        }

        if !state.in_hunk {
            continue;
        }

        // Count insertions and deletions (not the +++ / --- header lines)
        if line.starts_with('+') {
            state.current_insertions += 1;
        } else if line.starts_with('-') {
            state.current_deletions += 1;
        }
    }

    // Flush last file
    state.flush_current();

    build_file_result(state.file_stats)
}

/// Aggregate per-file stats into a `FileResult` summary.
///
/// Returns `None` if no file stats were collected (unrecognised format).
fn build_file_result(file_stats: Vec<FileStat>) -> Option<FileResult> {
    if file_stats.is_empty() {
        return None;
    }

    let file_count = file_stats.len();
    let total_insertions: usize = file_stats.iter().map(|f| f.insertions).sum();
    let total_deletions: usize = file_stats.iter().map(|f| f.deletions).sum();

    let shown = file_count.min(MAX_DISPLAY_ENTRIES);
    let entries: Vec<String> = file_stats
        .iter()
        .take(MAX_DISPLAY_ENTRIES)
        .map(|f| format!("{}: +{}, -{}", f.path, f.insertions, f.deletions))
        .collect();

    let footer = format!(
        "{file_count} file{} changed, {total_insertions} insertion{}(+), {total_deletions} deletion{}(-)",
        if file_count == 1 { "" } else { "s" },
        if total_insertions == 1 { " " } else { "s " },
        if total_deletions == 1 { " " } else { "s " },
    );

    Some(FileResult::new(
        "diff".to_string(),
        file_count,
        shown,
        entries,
        Some(footer),
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_support::{load_fixture, make_output_full};

    // ---- prepare_args tests ----

    #[test]
    fn test_prepare_args_injects_u() {
        let mut args = vec!["file1.txt".to_string(), "file2.txt".to_string()];
        prepare_args(&mut args);
        assert_eq!(args[0], "-u", "Should inject -u at position 0");
    }

    #[test]
    fn test_prepare_args_no_inject_when_short_u_present() {
        let mut args = vec!["-u".to_string(), "file1.txt".to_string()];
        prepare_args(&mut args);
        assert_eq!(args.iter().filter(|a| a.as_str() == "-u").count(), 1);
    }

    #[test]
    fn test_prepare_args_no_inject_when_unified_present() {
        let mut args = vec!["--unified".to_string(), "file1.txt".to_string()];
        prepare_args(&mut args);
        assert!(!args.contains(&"-u".to_string()));
    }

    #[test]
    fn test_prepare_args_no_inject_when_U3_present() {
        let mut args = vec!["-U3".to_string(), "file1.txt".to_string()];
        prepare_args(&mut args);
        assert!(!args.contains(&"-u".to_string()));
    }

    // ---- parse_impl tier tests ----

    #[test]
    fn test_tier1_unified_single_file() {
        let input = load_fixture("file", "diff_unified.txt");
        let result = try_parse_standalone_unified(&input);
        assert!(result.is_some(), "Expected parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.total_count, 1, "Single file diff");
        assert!(
            result.entries[0].contains("src/main.rs"),
            "Entry should contain file path, got: {}",
            result.entries[0]
        );
    }

    #[test]
    fn test_tier1_unified_multi_file() {
        let input = load_fixture("file", "diff_multi_file.txt");
        let result = try_parse_standalone_unified(&input);
        assert!(result.is_some(), "Expected parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.total_count, 3, "Three files in diff_multi_file.txt");
    }

    #[test]
    fn test_tier1_counts_insertions_deletions() {
        let input = load_fixture("file", "diff_unified.txt");
        let result = try_parse_standalone_unified(&input).unwrap();
        let entry = &result.entries[0];
        // The fixture has 3 insertions (+use std::fs, +println skim, +println Version)
        // and 2 deletions (-println Hello, -// old comment)
        assert!(entry.contains('+'), "Entry should show insertions: {entry}");
        assert!(entry.contains('-'), "Entry should show deletions: {entry}");
        // Footer shows total change summary
        let footer = result.footer.as_ref().unwrap();
        assert!(
            footer.contains("changed"),
            "Footer should show files changed"
        );
    }

    #[test]
    fn test_exit_code_1_is_success() {
        let input = load_fixture("file", "diff_unified.txt");
        let output = make_output_full(&input, "", Some(1));
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Exit code 1 means files differ — should produce Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_exit_code_2_is_error() {
        let output = make_output_full("diff: file not found", "", Some(2));
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Exit code 2 is an error — should be Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_tier3_empty_passthrough() {
        // Non-zero exit, empty output (not exit 0 which means identical)
        let output = make_output_full("", "", Some(2));
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Empty output on error should be passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_identical_files() {
        // Exit code 0, empty stdout: files are identical
        let output = make_output_full("", "", Some(0));
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Identical files should produce Full result, got {}",
            result.tier_name()
        );
        if let ParseResult::Full(fr) = result {
            assert!(
                fr.entries.iter().any(|e| e.contains("identical")),
                "Should report files are identical"
            );
        }
    }
}
