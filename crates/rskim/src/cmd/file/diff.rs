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
//! - **Tier 1 (Full)**: Parse unified diff; emit a stat header and full patch content
//!   per file (up to `MAX_DISPLAY_ENTRIES`). Files past the cap are noted in the footer
//!   via an exact-count elision marker (`[skim] N files omitted … SKIM_PASSTHROUGH=1`).
//! - **Tier 3 (Passthrough)**: Exit code 2 (error) or unrecognized output

use std::process::ExitCode;

use crate::output::canonical::FileResult;
use crate::output::{ParseResult, strip_ansi};
use crate::runner::CommandOutput;

use super::{MAX_DISPLAY_ENTRIES, MAX_INPUT_LINES};
use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "diff",
    env_overrides: &[],
    install_hint: "diff is typically pre-installed on Unix systems",
    family: "file",
    // skip_ansi_strip MUST be true for two independent reasons (ADR-012):
    //
    // 1. PF-006 tab-preservation: standalone `diff -u` emits
    //    `--- path\t<mtime>` headers; the parser splits on `\t`
    //    (see `try_parse_standalone_unified`).  Setting this flag prevents
    //    `strip_escape_sequences` from running at all, guaranteeing the `\t`
    //    delimiter survives regardless of buffer contents.
    //
    // 2. ADR-012 content-byte fidelity: hunk body lines (`+`/`-`/` `) carry
    //    file CONTENT that may contain ESC/CSI bytes.  Stripping them with
    //    `strip_escape_sequences` would diverge from the raw `diff` output
    //    without a loss marker — a #317 compress-never-truncate violation.
    //    The tool's OWN colorization is neutralized at the child-invocation
    //    boundary via `--no-color` (on `git diff`/`git show`; standalone
    //    `diff` never emits ANSI itself).  Content bytes must reach the reader
    //    byte-faithfully on both passthrough and skim-synthesized renders.
    //
    // Corollary: per-field `strip_ansi` on the already-split PATH field (lines
    // ~303/313) remains correct — the `\t` delimiter has been consumed by
    // `split('\t')` before `strip_ansi` sees the token, so PF-006 does not
    // apply and ANSI in PATH metadata is safely stripped without data loss.
    skip_ansi_strip: true,
    command_type: CommandType::FileOps,
    expected_exit_codes: &[1],
    forward_stderr: true,
    skip_net_savings_guard: false,
    synthesize_success_line: None,
    injected_format_flag: None,
    raw_override: None,
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
    /// Empty for files past `MAX_DISPLAY_ENTRIES` (retention stops at the display cap;
    /// `patch_line_count` carries the accurate total for those files).
    patch_lines: Vec<String>,
    /// Total count of lines that were or would have been pushed to `patch_lines`
    /// (hunk headers + body lines).  Always incremented even for files past the
    /// display cap where `patch_lines` is empty, so `build_file_result` can
    /// compute an accurate `total_count` without re-scanning (security-03 / Issue 1b).
    patch_line_count: usize,
}

/// Mutable accumulator state for the standalone unified diff parser.
struct DiffParserState {
    file_stats: Vec<FileStat>,
    current_path: Option<String>,
    current_insertions: usize,
    current_deletions: usize,
    /// Hunk lines accumulated for the current file.
    /// Empty for files past `MAX_DISPLAY_ENTRIES` — only `current_patch_line_count`
    /// is incremented once the display cap is reached (security-03 / Issue 1b).
    current_patch_lines: Vec<String>,
    /// Total patch lines (hunk headers + body) for the current file.
    /// Always incremented regardless of the display cap so `build_file_result`
    /// can produce an accurate `total_count`.
    current_patch_line_count: usize,
    in_hunk: bool,
    /// Remaining old-side lines expected in the current hunk (`-a,b` → `b`).
    /// Decremented by `-` (deletion) and ` ` (context) lines.
    hunk_old_remaining: usize,
    /// Remaining new-side lines expected in the current hunk (`+c,d` → `d`).
    /// Decremented by `+` (insertion) and ` ` (context) lines.
    hunk_new_remaining: usize,
}

impl DiffParserState {
    fn new() -> Self {
        Self {
            file_stats: Vec::new(),
            current_path: None,
            current_insertions: 0,
            current_deletions: 0,
            current_patch_lines: Vec::new(),
            current_patch_line_count: 0,
            in_hunk: false,
            hunk_old_remaining: 0,
            hunk_new_remaining: 0,
        }
    }

    /// Flush the current in-progress file stat and reset all accumulators.
    ///
    /// Both `current_patch_lines` and `current_patch_line_count` are taken
    /// unconditionally (rust-07): taking only inside the `if let Some(path)` arm
    /// would allow orphan lines/counts to leak into the next file's patch content
    /// when a flush fires with no current path.
    fn flush_current(&mut self) {
        let patch_lines = std::mem::take(&mut self.current_patch_lines);
        let patch_line_count = self.current_patch_line_count;
        if let Some(path) = self.current_path.take() {
            self.file_stats.push(FileStat {
                path,
                insertions: self.current_insertions,
                deletions: self.current_deletions,
                patch_lines,
                patch_line_count,
            });
        }
        self.current_insertions = 0;
        self.current_deletions = 0;
        self.current_patch_line_count = 0;
        self.in_hunk = false;
        self.hunk_old_remaining = 0;
        self.hunk_new_remaining = 0;
    }
}

/// Parse the old-side and new-side line counts from a `@@ -a[,b] +c[,d] @@ …`
/// hunk header.  Returns `(old_count, new_count)` where each count is the number
/// of lines from that file side appearing in the hunk body.
///
/// Per unified-diff convention the `,count` suffix is omitted when count == 1,
/// so `@@ -5 +5 @@` is read as `(1, 1)`.  A `,0` suffix (e.g. `-0,0`) is
/// explicit and means 0 lines — used for pure-insertion or pure-deletion hunks.
fn parse_hunk_counts(header: &str) -> (usize, usize) {
    let after_prefix = header.strip_prefix("@@ ").unwrap_or(header);
    let mut tokens = after_prefix.split_ascii_whitespace();
    let old_range = tokens.next().unwrap_or("-0,0");
    let new_range = tokens.next().unwrap_or("+0,0");
    (
        parse_hunk_range_count(old_range),
        parse_hunk_range_count(new_range),
    )
}

/// Extract the line count from a hunk range token (`-a`, `-a,b`, `+c`, `+c,d`).
///
/// Returns `b` / `d`.  When the comma-and-count suffix is absent, returns 1
/// (the implicit count for single-line ranges per unified-diff convention).
fn parse_hunk_range_count(range: &str) -> usize {
    let digits = range.trim_start_matches(['-', '+']);
    if let Some(comma_pos) = digits.find(',') {
        digits[comma_pos + 1..].parse().unwrap_or(0)
    } else {
        // No comma: unified-diff convention = count of 1 (unless the number
        // itself is 0, which means the whole range is empty — very rare).
        if digits == "0" { 0 } else { 1 }
    }
}

fn try_parse_standalone_unified(stdout: &str) -> Option<FileResult> {
    if stdout.trim().is_empty() {
        return None;
    }

    // Sibling-standard bound (security-03, complexity-05, architecture-03,
    // performance-05): on very large diffs (e.g. vendor bumps) the stateful
    // parser accumulates O(diff bytes) of heap Strings per line.  Return None
    // here so the caller degrades to lossless Passthrough (#317).  A mid-loop
    // `break` on a stateful hunk-budget parser leaves inconsistent state, so the
    // early-return is the safe implementation for this parser (per `mod.rs` doc).
    if stdout.lines().count() > MAX_INPUT_LINES {
        return None;
    }

    let mut state = DiffParserState::new();

    for line in stdout.lines() {
        // CRITICAL (security-01, rust-05): while inside a hunk body, ALL lines
        // are body content — regardless of their prefix.  A deleted SQL/Lua/
        // Haskell comment `-- …` is emitted by diff as `--- …`; without this
        // guard it would match the `--- ` file-header check below, trigger a
        // false flush_current(), fabricate a phantom path, and silently drop
        // the remainder of the hunk (violating #317 compress-never-truncate).
        //
        // The line-count budget from `@@ -a,b +c,d @@` is the authority:
        // while either old or new remaining count is non-zero, we are in the
        // hunk body and no line can be a structural header.
        if state.in_hunk {
            // Only retain patch content for files within the display cap
            // (security-03 / Issue 1b): stop accumulating once we have
            // MAX_DISPLAY_ENTRIES complete files to avoid O(entire diff) heap
            // retention.  `current_patch_line_count` always increments so that
            // `build_file_result` can compute an accurate `total_count`.
            if state.file_stats.len() < MAX_DISPLAY_ENTRIES {
                state.current_patch_lines.push(line.to_string());
            }
            state.current_patch_line_count += 1;
            match line.chars().next() {
                Some('+') => {
                    state.current_insertions += 1;
                    state.hunk_new_remaining = state.hunk_new_remaining.saturating_sub(1);
                }
                Some('-') => {
                    state.current_deletions += 1;
                    state.hunk_old_remaining = state.hunk_old_remaining.saturating_sub(1);
                }
                Some(' ') => {
                    // Context line: counts against both sides.
                    state.hunk_old_remaining = state.hunk_old_remaining.saturating_sub(1);
                    state.hunk_new_remaining = state.hunk_new_remaining.saturating_sub(1);
                }
                // A fully empty line inside an open hunk is a blank context
                // line: diff emits it as a lone space, but whitespace-stripping
                // tools reduce it to "" — both count against both sides.
                None => {
                    state.hunk_old_remaining = state.hunk_old_remaining.saturating_sub(1);
                    state.hunk_new_remaining = state.hunk_new_remaining.saturating_sub(1);
                }
                // `\ No newline at end of file` and similar annotation lines
                // are not content lines and must not consume the budget.
                _ => {}
            }
            if state.hunk_old_remaining == 0 && state.hunk_new_remaining == 0 {
                state.in_hunk = false;
            }
            continue;
        }

        // Outside a hunk — process structural diff lines.

        // `diff -ru` recursive header line: "diff -ru dir1/file dir2/file"
        // This precedes the --- / +++ headers; skip it but use it as a hint.
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

        // `+++ path\tdate` — confirms the new-file side; update path if `---` was /dev/null.
        if let Some(rest) = line.strip_prefix("+++ ") {
            // If the old path was /dev/null, use the new path.
            if state.current_path.as_deref() == Some("/dev/null") {
                // Same as --- case: split('\t') already consumed the tab; PF-006 safe.
                let path = strip_ansi(rest.split('\t').next().unwrap_or(rest).trim());
                state.current_path = Some(path);
            }
            continue;
        }

        // `@@ … @@` hunk header — parse the line-count budget and enter hunk mode.
        if line.starts_with("@@ ") {
            let (old_count, new_count) = parse_hunk_counts(line);
            state.hunk_old_remaining = old_count;
            state.hunk_new_remaining = new_count;
            // Only enter body-scan mode when the budget is non-empty; an
            // all-zero hunk (`@@ -0,0 +0,0 @@`) is valid but has no body lines.
            state.in_hunk = old_count > 0 || new_count > 0;
            // Conditional retention: only push the header for files within the
            // display cap; always count it for accurate total_count (Issue 1b).
            if state.file_stats.len() < MAX_DISPLAY_ENTRIES {
                state.current_patch_lines.push(line.to_string());
            }
            state.current_patch_line_count += 1;
            continue;
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

    // Total entry count across ALL files (including those past the display cap):
    // each file contributes 1 stat-header entry plus N patch-body lines.
    // Use `patch_line_count` (not `patch_lines.len()`) because files past the
    // display cap have `patch_lines` cleared while `patch_line_count` stays
    // accurate (security-03 / Issue 1b).  This keeps total_count entry-denominated,
    // satisfying the FileResult contract: shown_count == entries.len() and
    // total_count >= shown_count (architecture-05, complexity-09, regression-11).
    let total_entry_count: usize = file_stats.iter().map(|f| 1 + f.patch_line_count).sum();

    let shown_file_count = file_count.min(MAX_DISPLAY_ENTRIES);
    let mut entries: Vec<String> = Vec::new();

    for file in file_stats.into_iter().take(MAX_DISPLAY_ENTRIES) {
        // Per-file stat header (compact — no timestamps or `---`/`+++` lines).
        entries.push(format!(
            "{}: +{}, -{}",
            file.path, file.insertions, file.deletions
        ));
        // Move patch_lines without cloning — `file_stats` is owned and all
        // `.iter()` aggregations above have already completed (performance-04,
        // complexity-04, rust-01).
        entries.extend(file.patch_lines);
    }

    // shown_count == entries.len() — the FileResult contract.
    let shown_entry_count = entries.len();

    let stats_footer = format!(
        "{file_count} file{} changed, {total_insertions} insertion{}(+), {total_deletions} deletion{}(-)",
        if file_count == 1 { "" } else { "s" },
        if total_insertions == 1 { " " } else { "s " },
        if total_deletions == 1 { " " } else { "s " },
    );

    // Elision marker (loss-bearing, unconditional per ADR-011 — must carry exact
    // counts and SKIM_PASSTHROUGH=1 hint).  Passed as the footer parameter to
    // match the sibling-handler pattern (du/df/env/find/ps/wc/ls all pass the
    // marker as `footer`, not as an entry — consistency-09).  The stats summary
    // is appended after the marker so both remain visible to the reader.
    let footer = match crate::output::elision_marker(shown_file_count, file_count, "files") {
        Some(marker) => Some(format!("{marker}\n{stats_footer}")),
        None => Some(stats_footer),
    };

    Some(FileResult::new(
        "diff".to_string(),
        total_entry_count,
        shown_entry_count,
        entries,
        footer,
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
    // skip_ansi_strip MUST be true for two independent reasons (ADR-012):
    // (1) PF-006: prevents `strip_escape_sequences` from running at all, so the
    //     `\t` delimiter in `--- path\t<mtime>` headers is guaranteed to survive
    //     before `split('\t')` can separate them (see `try_parse_standalone_unified`).
    // (2) ADR-012: hunk body lines are file CONTENT; stripping ESC/CSI bytes with
    //     `strip_escape_sequences` would diverge from the raw tool without a loss
    //     marker (#317).  See `test_adr012_esc_in_diff_body_line_survives` for the
    //     content half, which pairs with `test_ansi_in_diff_path_is_stripped` to
    //     document the metadata-vs-content split ADR-012 draws.

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn test_config_skip_ansi_strip_is_true() {
        assert!(
            CONFIG.skip_ansi_strip,
            "diff CONFIG.skip_ansi_strip must be true — \
             (1) PF-006: flag prevents strip_escape_sequences from running, guaranteeing \
             \\t in --- headers survives; \
             (2) ADR-012: ESC/CSI bytes in diff body CONTENT must survive byte-faithfully"
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
        // total_count and shown_count are now entry-denominated (stat headers +
        // patch lines), not file-denominated.  Verify the FileResult contract
        // (architecture-05): shown_count == entries.len().
        assert_eq!(
            result.shown_count,
            result.entries.len(),
            "shown_count must equal entries.len()"
        );
        // The single-file summary is visible in the footer.
        assert!(
            result.footer.as_deref().unwrap_or("").contains("1 file"),
            "Footer should report 1 file changed; footer={:?}",
            result.footer
        );
        assert!(
            result.entries[0].contains("src/main.rs"),
            "First entry should contain file path, got: {}",
            result.entries[0]
        );
    }

    #[test]
    fn test_tier1_unified_multi_file() {
        let input = load_fixture("file", "diff_multi_file.txt");
        let result = try_parse_standalone_unified(&input);
        assert!(result.is_some(), "Expected parse to succeed");
        let result = result.unwrap();
        // File count is now carried in the footer, not in total_count (which is
        // entry-denominated).  Verify the FileResult contract and the footer.
        assert_eq!(
            result.shown_count,
            result.entries.len(),
            "shown_count must equal entries.len()"
        );
        assert!(
            result.footer.as_deref().unwrap_or("").contains("3 files"),
            "Footer should report 3 files changed; footer={:?}",
            result.footer
        );
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

    /// ESC/CSI bytes present in a diff BODY line (file CONTENT) must survive
    /// byte-faithfully into the rendered entries (ADR-012).
    ///
    /// This is the complement of `test_ansi_in_diff_path_is_stripped`, which
    /// pins the opposite behavior for the PATH field.  Together the two tests
    /// document the metadata-vs-content split ADR-012 draws:
    ///
    ///   - PATH field (metadata): ANSI is stripped via per-field `strip_ansi`
    ///     after `split('\t')` has already consumed the tab delimiter, so
    ///     PF-006 does not apply and stripping is safe.
    ///   - Body lines (CONTENT): ESC/CSI bytes pass through unmodified —
    ///     stripping them would diverge from the raw `diff` tool without a
    ///     loss marker, violating #317 compress-never-truncate.
    #[test]
    fn test_adr012_esc_in_diff_body_line_survives() {
        // A body line whose CONTENT contains an ESC/CSI sequence.  Simulates a
        // file that stores ANSI bytes verbatim (e.g. a snapshot of coloured
        // terminal output stored as a text fixture).
        let input = "--- a/file.txt\t2026-07-25 10:00:00.000000000 +0000\n\
                     +++ b/file.txt\t2026-07-25 10:05:00.000000000 +0000\n\
                     @@ -1,1 +1,1 @@\n\
                     -old line\n\
                     +\x1b[32mcolored new line\x1b[0m\n";
        let result = try_parse_standalone_unified(input);
        assert!(
            result.is_some(),
            "must parse diff containing ESC bytes in a body line"
        );
        let result = result.unwrap();

        // entries[0] is the stat header; subsequent entries include the hunk
        // header and body lines.  Find the insertion body line by prefix.
        let insertion = result
            .entries
            .iter()
            .find(|e| e.starts_with('+') && e.contains("colored new line"));
        assert!(
            insertion.is_some(),
            "insertion body line must appear in entries: {:?}",
            result.entries
        );
        let insertion = insertion.unwrap();
        assert!(
            insertion.contains('\x1b'),
            "ADR-012: ESC byte in diff body CONTENT must survive byte-faithfully \
             (skip_ansi_strip: true covers both \\t delimiters AND content-borne \
             ESC sequences); got: {insertion:?}"
        );
        assert!(
            insertion.contains("\x1b[32m"),
            "ADR-012: full CSI sequence must survive intact in diff body CONTENT; \
             got: {insertion:?}"
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
    ///
    /// After the architecture-05 / consistency-09 fixes:
    /// - total_count and shown_count are ENTRY-denominated (1 stat header +
    ///   N patch lines per file), not file-denominated.
    /// - The elision marker lives in the `footer` field (matching all sibling
    ///   handlers: du/df/env/find/ps/wc/ls), NOT in `entries`.
    #[test]
    fn test_dir_diff_capped_with_elision_marker() {
        // Build a synthetic dir diff with MAX_DISPLAY_ENTRIES + 1 files.
        // Each file produces: @@ -1,1 +1,1 @@  →  1 old line, 1 new line.
        // Body: "@@ -1,1 +1,1 @@" + "-old N" + "+new N" = 3 patch lines.
        // Total entries per file = 1 stat header + 3 patch lines = 4.
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

        const ENTRIES_PER_FILE: usize = 4; // 1 stat header + 3 patch lines
        assert_eq!(
            result.total_count,
            (cap + 1) * ENTRIES_PER_FILE,
            "total_count is entry-denominated: (cap+1) files × {ENTRIES_PER_FILE} entries each"
        );
        assert_eq!(
            result.shown_count,
            cap * ENTRIES_PER_FILE,
            "shown_count is entry-denominated: cap files × {ENTRIES_PER_FILE} entries each"
        );
        // FileResult contract (architecture-05): shown_count == entries.len().
        assert_eq!(
            result.shown_count,
            result.entries.len(),
            "shown_count must equal entries.len()"
        );

        // Elision marker must appear in the FOOTER (consistency-09), not in entries.
        // Assert the full first line of the footer — ADR-011 requires loss-bearing
        // markers to carry exact counts and the SKIM_PASSTHROUGH=1 hint (testing-11).
        let footer = result.footer.as_deref().unwrap_or("");
        let marker_line = footer.lines().next().unwrap_or("");
        let total_files = cap + 1;
        let omitted = total_files - cap; // = 1
        let expected_marker = format!(
            "[skim] {omitted} files omitted ({cap} of {total_files} shown) \u{2014} SKIM_PASSTHROUGH=1 for full output"
        );
        assert_eq!(
            marker_line,
            expected_marker.as_str(),
            "elision marker must carry exact counts and SKIM_PASSTHROUGH=1 hint (ADR-011); footer={footer:?}"
        );
        // Stats summary must also be in the footer.
        assert!(
            footer.contains("changed"),
            "stats summary must be in footer; footer={footer:?}"
        );
        // Entries must NOT contain the elision marker (it has moved to footer).
        assert!(
            !result.entries.iter().any(|e| e.contains("[skim]")),
            "elision marker must not appear in entries after move to footer"
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

    // ---- regression tests for security-01 / rust-05: hunk-body-confusion ----
    //
    // A deleted line whose content begins with `-- ` (SQL/Lua/Haskell comment)
    // is emitted by diff as `--- …` (the `-` diff prefix plus the `-- ` content).
    // Before the hunk-budget fix, `--- …` matched the file-header check BEFORE
    // the in-hunk guard, triggering flush_current() + a phantom path + silent
    // content drop — a #317 violation.

    /// Core regression: deleting a SQL comment `-- SELECT 1` produces a diff
    /// body line `--- SELECT 1` that must be treated as body content, never as
    /// a `--- path` file header.
    #[test]
    fn test_sql_comment_deletion_not_treated_as_file_header() {
        // Unified diff that removes a SQL/Lua comment line.
        // The hunk header `@@ -1,3 +1,2 @@` budgets 3 old lines and 2 new lines.
        // The deleted comment line `-- SELECT 1` → `--- SELECT 1` in diff output
        // (starts with `--- ` — the false-positive header pattern).
        let input = concat!(
            "--- old.sql\t2026-07-25 10:00:00\n",
            "+++ new.sql\t2026-07-25 10:05:00\n",
            "@@ -1,3 +1,2 @@\n",
            " SELECT 1;\n",
            "--- SELECT 1 -- remove this comment\n", // deleted SQL comment line
            " SELECT 2;\n",
        );
        let result = try_parse_standalone_unified(input);
        assert!(
            result.is_some(),
            "must parse diff containing deleted SQL comment"
        );
        let result = result.unwrap();

        // Contract: exactly 1 file — no phantom file fabricated from the comment line.
        assert!(
            result.footer.as_deref().unwrap_or("").contains("1 file"),
            "must parse as exactly 1 file, not more; footer={:?}",
            result.footer
        );

        // The deleted SQL comment must appear in entries as a patch body line.
        assert!(
            result
                .entries
                .iter()
                .any(|e| e.contains("-- SELECT 1 -- remove this comment")),
            "deleted SQL comment must appear as body line; entries={:?}",
            result.entries
        );

        // The hunk header must appear.
        assert!(
            result.entries.iter().any(|e| e.starts_with("@@ -1,3")),
            "hunk header must appear in entries; entries={:?}",
            result.entries
        );

        // FileResult contract: shown_count == entries.len().
        assert_eq!(
            result.shown_count,
            result.entries.len(),
            "shown_count must equal entries.len()"
        );
    }

    /// Edge case: the `+++ ` header false-positive.  An added C++ increment line
    /// `++ counter` would appear as `+++ counter` in diff output and could
    /// previously misfire the `+++ ` header check while in a hunk.
    #[test]
    fn test_added_plus_plus_line_not_treated_as_new_file_header() {
        // Diff that adds a line beginning with `++ ` (e.g. C++ `++counter`).
        // In diff output this becomes `+++ counter` — the `+++ ` header pattern.
        let input = concat!(
            "--- old.cpp\t2026-07-25 10:00:00\n",
            "+++ new.cpp\t2026-07-25 10:05:00\n",
            "@@ -1,1 +1,2 @@\n",
            " int x = 0;\n",
            "+++ x; // C++ increment\n", // added line starting with `++ ` → `+++ ` in diff
        );
        let result = try_parse_standalone_unified(input);
        assert!(result.is_some(), "must parse diff with added `++ ` line");
        let result = result.unwrap();

        // Exactly 1 file — no phantom file created from the `+++ x` insertion line.
        assert!(
            result.footer.as_deref().unwrap_or("").contains("1 file"),
            "must parse as exactly 1 file; footer={:?}",
            result.footer
        );

        // The added `++ x` line must appear in entries as a body line.
        assert!(
            result
                .entries
                .iter()
                .any(|e| e.contains("++ x; // C++ increment")),
            "added `++ ` line must appear as body line; entries={:?}",
            result.entries
        );

        assert_eq!(
            result.shown_count,
            result.entries.len(),
            "shown_count must equal entries.len()"
        );
    }

    /// rust-07 regression: patch lines must not leak across file boundaries
    /// when flush_current fires with no current_path (e.g., from a `diff -ru`
    /// header before the first `---` line).  Before the fix, patch_lines were
    /// only taken inside the `if let Some(path)` arm, so orphan lines could
    /// accumulate and be attributed to the next file.
    #[test]
    fn test_flush_without_path_does_not_leak_patch_lines() {
        // This diff starts with a `diff -ru` header (no preceding `---`), which
        // fires flush_current with current_path == None.  The subsequent file's
        // patch content must contain only its own lines, not any phantom lines
        // from the orphaned accumulator.
        let input = concat!(
            "diff -ru dir1/a.txt dir2/a.txt\n",
            "--- dir1/a.txt\t2026-07-25\n",
            "+++ dir2/a.txt\t2026-07-25\n",
            "@@ -1,1 +1,1 @@\n",
            "-old\n",
            "+new\n",
        );
        let result = try_parse_standalone_unified(input).expect("must parse");
        // Exactly 1 file — the flush with no path must not create a phantom file.
        assert!(
            result.footer.as_deref().unwrap_or("").contains("1 file"),
            "must parse as exactly 1 file; footer={:?}",
            result.footer
        );
        // The sole file's entries are the stat header + 3 patch lines.
        assert_eq!(
            result.entries.len(),
            4, // stat header + @@ header + deletion + insertion
            "exactly 4 entries expected; entries={:?}",
            result.entries
        );
        assert_eq!(result.shown_count, result.entries.len());
    }
}
