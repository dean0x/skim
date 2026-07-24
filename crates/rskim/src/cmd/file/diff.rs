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

use crate::output::canonical::FileResult;
use crate::output::{ParseResult, strip_ansi};
use crate::runner::CommandOutput;

use super::MAX_DISPLAY_ENTRIES;
use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "diff",
    env_overrides: &[],
    install_hint: "diff is typically pre-installed on Unix systems",
    family: "file",
    // standalone `diff -u` emits `--- path\t<mtime>` headers and the parser
    // splits on `\t` (see `try_parse_standalone_unified`); diff emits no ANSI.
    // `strip_ansi_escapes` drops `\t`, fusing path and mtime before parsing.
    skip_ansi_strip: true,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[1],
    forward_stderr: true,
    skip_net_savings_guard: false,
    synthesize_success_line: None,
    injected_format_flag: None,
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

/// Per-file diff statistics and patch content.
struct FileStat {
    path: String,
    insertions: usize,
    deletions: usize,
    /// Hunk headers (`@@ ... @@`) and patch body lines (`+`, `-`, ` `).
    /// Retained so `build_file_result` can emit actual content, not just a stat.
    patch_lines: Vec<String>,
}

/// Mutable accumulator state for the standalone unified diff parser.
struct DiffParserState {
    file_stats: Vec<FileStat>,
    current_path: Option<String>,
    current_insertions: usize,
    current_deletions: usize,
    /// Hunk lines accumulated for the current file.
    current_patch_lines: Vec<String>,
    in_hunk: bool,
}

impl DiffParserState {
    fn new() -> Self {
        Self {
            file_stats: Vec::new(),
            current_path: None,
            current_insertions: 0,
            current_deletions: 0,
            current_patch_lines: Vec::new(),
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
                patch_lines: std::mem::take(&mut self.current_patch_lines),
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
            // Extract path (strip optional tab+timestamp suffix), then strip any
            // ANSI escape sequences from the path before re-emitting it.
            // strip_ansi is safe on the already-split field: the tab has been
            // consumed by split('\t'), and file paths don't contain tabs — no
            // PF-006 risk of tab destruction on an already-delimited field.
            let path = strip_ansi(rest.split('\t').next().unwrap_or(rest).trim());
            state.current_path = Some(path);
            continue;
        }

        // `+++ path\tdate` — confirms the new-file side; update path if `---` was /dev/null
        if let Some(rest) = line.strip_prefix("+++ ") {
            // If the old path was /dev/null, use the new path
            if state.current_path.as_deref() == Some("/dev/null") {
                // Same as --- case: split('\t') already consumed the tab; PF-006 safe.
                let path = strip_ansi(rest.split('\t').next().unwrap_or(rest).trim());
                state.current_path = Some(path);
            }
            continue;
        }

        // `@@ ... @@` hunk header — entering a hunk; retain for content rendering.
        if line.starts_with("@@ ") {
            state.in_hunk = true;
            state.current_patch_lines.push(line.to_string());
            continue;
        }

        if !state.in_hunk {
            continue;
        }

        // Count insertions/deletions and retain the line for content rendering.
        // Context lines (` `) are also retained so hunks are readable.
        state.current_patch_lines.push(line.to_string());
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

/// Aggregate per-file stats and patch content into a `FileResult`.
///
/// Each shown file contributes a stat header (`path: +N, -M`) followed by its
/// hunk lines (`@@ ... @@`, `+line`, `-line`, ` context`).  This preserves
/// the actual changed content rather than emitting a diffstat-only summary
/// (which would violate the compress-never-truncate rule, #317).
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
    let mut entries: Vec<String> = Vec::new();

    for file in file_stats.iter().take(MAX_DISPLAY_ENTRIES) {
        // Per-file stat header (compact — no timestamps or `---`/`+++` lines).
        entries.push(format!(
            "{}: +{}, -{}",
            file.path, file.insertions, file.deletions
        ));
        // Actual patch content: hunk headers + insertion/deletion/context lines.
        entries.extend(file.patch_lines.iter().cloned());
    }

    // Elision marker when the directory diff exceeds the display cap (#317).
    if let Some(marker) = crate::output::elision_marker(shown, file_count, "files") {
        entries.push(marker);
    }

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
    use crate::cmd::test_utils::{load_fixture, make_output_full};

    // --- config-lock tests ---
    //
    // skip_ansi_strip MUST be true: standalone `diff -u` emits
    // `--- path\t<mtime>` headers and the parser splits on `\t`
    // (see `try_parse_standalone_unified` — the `--- `/`+++ ` header handlers).
    // `strip_ansi_escapes` would drop the `\t`, fusing path and timestamp
    // into a single token before the parser sees it. diff emits no ANSI.

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn test_config_skip_ansi_strip_is_true() {
        assert!(
            CONFIG.skip_ansi_strip,
            "diff CONFIG.skip_ansi_strip must be true — \
             strip_ansi_escapes drops \\t, gluing path to mtime in --- headers"
        );
    }

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
    #[allow(non_snake_case)] // `U3` refers to the `-U3` diff flag under test; renaming the
    // test would obscure intent. Annotated rather than renamed to keep test identity stable.
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

    /// ANSI escape sequences in a diff path header must be stripped from the
    /// re-emitted entry.  With `skip_ansi_strip:true` on diff's CONFIG, ANSI
    /// bytes reach the parser raw; this pins the defence-in-depth behaviour on
    /// the already-split `path` field (avoids PF-006 — strip_ansi is safe here
    /// because the `\t` delimiter has already been consumed by `split('\t')`).
    #[test]
    fn test_ansi_in_diff_path_is_stripped() {
        let input = "--- \x1b[1ma/src/main.rs\x1b[0m\t2026-05-01 10:00:00.000000000 +0000\n\
                     +++ b/src/main.rs\t2026-05-01 10:05:00.000000000 +0000\n\
                     @@ -1,1 +1,1 @@\n\
                     -old\n\
                     +new\n";
        let result = try_parse_standalone_unified(input);
        assert!(
            result.is_some(),
            "must parse unified diff with ANSI in path"
        );
        let result = result.unwrap();
        assert!(
            result.entries[0].contains("a/src/main.rs"),
            "path must appear without ESC codes; got: {}",
            result.entries[0]
        );
        assert!(
            !result.entries[0].contains('\x1b'),
            "entry must not contain ESC bytes; got: {}",
            result.entries[0]
        );
    }

    /// Verifies that adding ANSI defence does not regress the PR's core fix:
    /// a genuine `--- path\t<mtime>` header still splits correctly on `\t`
    /// (the tab delimiter must NOT be destroyed by the per-field strip_ansi call).
    #[test]
    fn test_tab_delimiter_survives_strip_ansi_on_path_field() {
        // Header with clean path — confirms the split still works after the
        // strip_ansi wrapping even when no ESC codes are present.
        let input = "--- a/src/lib.rs\t2026-05-01 10:00:00.000000000 +0000\n\
                     +++ b/src/lib.rs\t2026-05-01 10:05:00.000000000 +0000\n\
                     @@ -1,1 +1,1 @@\n\
                     -old\n\
                     +new\n";
        let result = try_parse_standalone_unified(input);
        assert!(result.is_some(), "must parse clean unified diff");
        let result = result.unwrap();
        // entries[0] is the per-file stat header — path must not be fused with mtime.
        assert_eq!(
            result.entries[0], "a/src/lib.rs: +1, -1",
            "path must not be fused with mtime; got: {}",
            result.entries[0]
        );
        // Fix 2: patch content must follow the stat header.
        assert!(
            result.entries.iter().any(|e| e.starts_with("@@")),
            "hunk header must appear in entries: {:?}",
            result.entries
        );
        assert!(
            result.entries.iter().any(|e| e == "-old"),
            "deletion line must appear: {:?}",
            result.entries
        );
        assert!(
            result.entries.iter().any(|e| e == "+new"),
            "insertion line must appear: {:?}",
            result.entries
        );
    }

    /// Fix 2: `skim diff a b` must include actual changed lines, not just a
    /// diffstat.  Each file gets a stat header followed by its patch content.
    #[test]
    fn test_patch_content_included_in_entries() {
        // Use concat! so the leading space in context lines is preserved.
        // Rust's backslash line-continuation strips all leading whitespace on
        // the next source line, which would silently drop the diff's ` ` prefix.
        let input = concat!(
            "--- old.txt\t2026-07-25 10:00:00\n",
            "+++ new.txt\t2026-07-25 10:05:00\n",
            "@@ -1,4 +1,4 @@\n",
            " context before\n",
            "-removed line\n",
            "+added line\n",
            " context after\n",
        );
        let result = try_parse_standalone_unified(input).expect("must parse");
        // Stat header is entries[0]
        assert!(
            result.entries[0].contains("old.txt"),
            "stat header must have path: {:?}",
            result.entries[0]
        );
        // Hunk header
        assert!(
            result.entries.iter().any(|e| e.starts_with("@@ -1,4")),
            "hunk header must be present: {:?}",
            result.entries
        );
        // Deletion line
        assert!(
            result.entries.iter().any(|e| e == "-removed line"),
            "deletion must be present: {:?}",
            result.entries
        );
        // Insertion line
        assert!(
            result.entries.iter().any(|e| e == "+added line"),
            "insertion must be present: {:?}",
            result.entries
        );
        // Context lines
        assert!(
            result.entries.iter().any(|e| e == " context before"),
            "context line must be present: {:?}",
            result.entries
        );
    }

    /// Fix 2: directory diff with more than MAX_DISPLAY_ENTRIES files must emit
    /// an elision marker rather than silently dropping files (#317).
    #[test]
    fn test_dir_diff_capped_with_elision_marker() {
        // Build a synthetic dir diff with MAX_DISPLAY_ENTRIES + 1 files.
        let cap = super::super::MAX_DISPLAY_ENTRIES;
        let mut input = String::new();
        for i in 0..=cap {
            input.push_str(&format!(
                "diff -ru dir1/file{i}.txt dir2/file{i}.txt\n\
                 --- dir1/file{i}.txt\t2026-07-25\n\
                 +++ dir2/file{i}.txt\t2026-07-25\n\
                 @@ -1,1 +1,1 @@\n\
                 -old {i}\n\
                 +new {i}\n"
            ));
        }
        let result = try_parse_standalone_unified(&input).expect("must parse");
        assert_eq!(
            result.total_count,
            cap + 1,
            "total_count must be cap+1 files"
        );
        assert_eq!(result.shown_count, cap, "shown_count must be capped");
        // Elision marker must appear in entries
        assert!(
            result
                .entries
                .iter()
                .any(|e| e.contains("[skim]") && e.contains("omitted")),
            "elision marker must appear when files are capped: {:?}",
            result.entries.last()
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
