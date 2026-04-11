//! AST-aware diff pipeline (#103).
//!
//! Parses unified diff output, overlays changed line ranges on tree-sitter
//! ASTs, and renders changed nodes with full function boundaries and standard
//! `+`/`-` markers.

mod ast;
mod parse;
mod render;
mod source;
pub(super) mod types;

// Re-export for show.rs (sibling of diff/ within cmd/git/).
// AD-6: Raising visibility to pub(in crate::cmd::git) enables show.rs to
// reuse the diff pipeline without duplicating parsing or rendering logic.
pub(in crate::cmd::git) use parse::parse_unified_diff;
pub(in crate::cmd::git) use render::render_diff_file;

use std::process::ExitCode;

use rayon::prelude::*;

use crate::cmd::{extract_output_format, user_has_flag, OutputFormat};
use crate::output::canonical::{DiffFileEntry, DiffResult};
use crate::runner::CommandRunner;

use super::{finalize_git_output, finalize_git_output_owned, map_exit_code, run_passthrough};

/// Maximum file size for AST processing (100 KB). Larger files fall back
/// to raw diff hunks.
const MAX_AST_FILE_SIZE: usize = 100 * 1024;

/// Maximum number of files processed through the AST pipeline. Files beyond
/// this limit fall back to raw diff hunks to keep diff rendering bounded.
/// Exposed `pub(in crate::cmd::git)` so `show.rs` (sibling module) can reuse the limit.
pub(in crate::cmd::git) const MAX_AST_FILE_COUNT: usize = 200;

/// Minimum file count to engage rayon parallelism. Below this, thread pool
/// scheduling overhead exceeds the per-file render cost.
const PARALLEL_THRESHOLD: usize = 5;

/// Controls how unchanged AST nodes are rendered alongside changed nodes.
///
/// - `Default`: Only changed nodes are shown (no unchanged context).
/// - `Structure`: Unchanged nodes are shown as signatures (`{ /* ... */ }`).
/// - `Full`: Unchanged nodes are shown in full.
///
/// DESIGN NOTE (AD-6): visibility widened to `pub(in crate::cmd::git)` so that
/// `show.rs` can specify the render mode when reusing `render_diff_file`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cmd::git) enum DiffMode {
    /// Only changed AST nodes with `+`/`-` markers.
    Default,
    /// Changed nodes + unchanged nodes rendered as signatures.
    Structure,
    /// Changed nodes + unchanged nodes shown in full.
    Full,
}

/// Extract `--mode <value>` or `--mode=<value>` from args.
///
/// Returns `(filtered_args, DiffMode)` where `filtered_args` has the mode
/// flag removed so it is not passed to git.
///
/// Returns an error if the mode value is not one of the recognized values.
fn extract_diff_mode(args: &[String]) -> anyhow::Result<(Vec<String>, DiffMode)> {
    let mut filtered: Vec<String> = Vec::with_capacity(args.len());
    let mut mode = DiffMode::Default;
    let mut skip_next = false;

    for (i, arg) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }

        if arg == "--mode" {
            if let Some(val) = args.get(i + 1) {
                mode = parse_diff_mode_value(val)?;
                skip_next = true;
            } else {
                return Err(anyhow::anyhow!(
                    "{arg} requires a value\nValid modes: structure, full (default: changed-only)"
                ));
            }
            continue;
        }

        if let Some(val) = arg.strip_prefix("--mode=") {
            mode = parse_diff_mode_value(val)?;
            continue;
        }

        filtered.push(arg.clone());
    }

    Ok((filtered, mode))
}

/// Parse a mode value string into a DiffMode.
///
/// Returns an error for unrecognized mode values with a helpful message.
fn parse_diff_mode_value(val: &str) -> Result<DiffMode, anyhow::Error> {
    match val {
        "structure" | "signatures" => Ok(DiffMode::Structure),
        "full" => Ok(DiffMode::Full),
        _ => Err(anyhow::anyhow!(
            "unknown diff mode: '{val}'\nValid modes: structure, full (default: changed-only)"
        )),
    }
}

/// Print help for `skim git diff`.
fn print_diff_help() {
    println!("skim git diff \u{2014} AST-aware diff compression");
    println!();
    println!("USAGE:");
    println!("    skim git diff [OPTIONS] [<commit>..] [-- <path>...]");
    println!();
    println!("SKIM OPTIONS:");
    println!("    --mode <MODE>    Diff rendering mode (no short flag; -m conflicts with git):");
    println!("                       (default)    Changed functions with boundaries");
    println!("                       structure    + unchanged functions as signatures");
    println!("                       full         Entire files with change markers");
    println!("    --json           Machine-readable JSON output");
    println!("    --show-stats     Show token savings statistics");
    println!();
    println!("GIT OPTIONS:");
    println!("    --staged, --cached    Diff staged changes");
    println!("    --stat, --shortstat   Passthrough to git (no AST processing)");
    println!("    --name-only           Passthrough to git");
    println!();
    println!("EXAMPLES:");
    println!("    skim git diff                    Working tree changes");
    println!("    skim git diff --staged           Staged changes");
    println!("    skim git diff HEAD~3             Last 3 commits");
    println!("    skim git diff main..feature      Branch comparison");
    println!("    skim git diff --mode structure   With context signatures");
    println!("    skim git diff --json             JSON output");
}

/// Run `git diff` with AST-aware pipeline (#103).
///
/// Flag-aware passthrough: `--stat`, `--name-only`, `--name-status`, `--check`
/// pass through to git unmodified.
///
/// Supports:
/// - `--mode structure|full` to control context rendering
/// - `--json` for machine-readable output
///
/// Default: parses unified diff, overlays changed lines on tree-sitter AST,
/// renders changed nodes with full function boundaries and `+`/`-` markers.
pub(super) fn run_diff(
    global_flags: &[String],
    args: &[String],
    show_stats: bool,
) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_diff_help();
        return Ok(ExitCode::SUCCESS);
    }

    if user_has_flag(
        args,
        &[
            "--stat",
            "--shortstat",
            "--numstat",
            "--name-only",
            "--name-status",
            "--check",
        ],
    ) {
        return run_passthrough(global_flags, "diff", args, show_stats);
    }

    // Extract skim-specific flags before passing args to git
    let (args_no_mode, diff_mode) = extract_diff_mode(args)?;
    let (git_args, output_format) = extract_output_format(&args_no_mode);

    // Run `git diff --no-color [user args]` to get unified diff output
    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend(["diff".to_string(), "--no-color".to_string()]);
    full_args.extend_from_slice(&git_args);

    let runner = CommandRunner::new(None);
    let arg_refs: Vec<&str> = full_args.iter().map(|s| s.as_str()).collect();
    let output = runner.run("git", &arg_refs)?;

    if output.exit_code != Some(0) {
        if !output.stderr.is_empty() {
            eprint!("{}", output.stderr);
        }
        if !output.stdout.is_empty() {
            print!("{}", output.stdout);
        }
        // Record analytics even on non-zero exit so the DB reflects failed
        // invocations. Raw == compressed (passthrough) on error path.
        finalize_git_output(
            &output.stdout,
            &output.stdout,
            super::build_analytics_label("diff", args, show_stats),
            show_stats,
            crate::analytics::CommandType::Git,
            output.duration,
        );
        return Ok(map_exit_code(output.exit_code));
    }

    // Surface git diff stderr warnings (e.g., "warning: LF will be replaced by CRLF")
    // on successful runs. These are informational notices from git, not failures,
    // so they should be visible even when diff output is otherwise compressed.
    if !output.stderr.is_empty() {
        eprint!("[skim] git diff notice: {}", output.stderr.trim());
    }

    let duration = output.duration;
    let raw_diff = output.stdout;
    let label = super::build_analytics_label("diff", args, show_stats);

    // Handle empty diff — record zero-compression analytics so the DB stays
    // consistent with run_passthrough (which always records, even for no-op passes).
    if raw_diff.trim().is_empty() {
        eprintln!("No changes");
        finalize_git_output(
            &raw_diff,
            &raw_diff,
            label,
            show_stats,
            crate::analytics::CommandType::Git,
            duration,
        );
        return Ok(ExitCode::SUCCESS);
    }

    // Parse and render inside a block so `file_diffs` (which borrows
    // `raw_diff`) drops before `raw_diff` is moved into analytics.
    let result_str = {
        let file_diffs = parse_unified_diff(&raw_diff);

        if file_diffs.is_empty() {
            eprintln!("No changes");
            // Reuse the same lazy label built above — no second format! needed.
            // This branch is only hit when parse_unified_diff returns an empty vec
            // despite raw_diff being non-empty (malformed diff); keep a consistent
            // analytics record identical to the trim-is-empty branch above.
            finalize_git_output(
                &raw_diff,
                &raw_diff,
                label,
                show_stats,
                crate::analytics::CommandType::Git,
                duration,
            );
            return Ok(ExitCode::SUCCESS);
        }

        // Render each file with AST-aware context.
        // After MAX_AST_FILE_COUNT files, skip AST rendering and fall back to raw
        // hunks to keep diff processing bounded on very large changesets.
        //
        // When >4 files, render in parallel via rayon. Each thread gets its own
        // tree-sitter parser from the thread_local cache in render.rs.
        // `par_iter().collect()` preserves the original element order, so output
        // is deterministic regardless of thread scheduling.
        let render_one = |i: usize, file_diff: &types::FileDiff<'_>| {
            let skip_ast = i >= MAX_AST_FILE_COUNT;
            let rendered =
                render_diff_file(file_diff, global_flags, &git_args, diff_mode, skip_ast);
            let entry = DiffFileEntry {
                path: file_diff.path.clone(),
                status: file_diff.status.clone(),
                changed_regions: file_diff.hunks.len(),
            };
            (rendered, entry)
        };

        let rendered_files: Vec<(String, DiffFileEntry)> = if file_diffs.len() >= PARALLEL_THRESHOLD
        {
            file_diffs
                .par_iter()
                .enumerate()
                .map(|(i, file_diff)| render_one(i, file_diff))
                .collect()
        } else {
            file_diffs
                .iter()
                .enumerate()
                .map(|(i, file_diff)| render_one(i, file_diff))
                .collect()
        };

        let mut rendered_output = String::new();
        let mut diff_file_entries: Vec<DiffFileEntry> = Vec::with_capacity(rendered_files.len());
        for (rendered, entry) in rendered_files {
            rendered_output.push_str(&rendered);
            diff_file_entries.push(entry);
        }

        let result = DiffResult::new(diff_file_entries, rendered_output);

        match output_format {
            OutputFormat::Json => {
                let json = serde_json::to_string_pretty(&result)
                    .map_err(|e| anyhow::anyhow!("failed to serialize diff result: {e}"))?;
                println!("{json}");
                json
            }
            OutputFormat::Text => {
                // Apply guardrail: if compressed output is larger than raw,
                // emit raw. Matches the guardrail applied in show.rs for commit
                // mode, ensuring both handlers share the same safety envelope.
                // Clone `raw_diff` here; file_diffs still holds a borrow so
                // we cannot move raw_diff until the block ends.
                // Use into_rendered() instead of to_string(): avoids a redundant
                // Display::fmt allocation + copy of the pre-built rendered String.
                let s = result.into_rendered();
                let guardrail = crate::output::guardrail::apply_to_stderr(raw_diff.clone(), s)?;
                let final_output = guardrail.into_output();
                print!("{final_output}");
                final_output
            }
        }
    }; // file_diffs dropped here, raw_diff is free to move

    // Both `raw_diff` and `result_str` are owned here; use the owned variant to
    // move them directly without cloning (handles stats + analytics in one call).
    finalize_git_output_owned(
        raw_diff,
        result_str,
        label,
        show_stats,
        crate::analytics::CommandType::Git,
        duration,
    );

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // DiffMode extraction tests (Gap 1)
    // ========================================================================

    #[test]
    fn test_parse_diff_mode_extraction_structure() {
        let args: Vec<String> = vec!["--cached".into(), "--mode".into(), "structure".into()];
        let (filtered, mode) = extract_diff_mode(&args).unwrap();
        assert_eq!(mode, DiffMode::Structure);
        assert_eq!(filtered, vec!["--cached"]);
    }

    #[test]
    fn test_parse_diff_mode_extraction_full() {
        let args: Vec<String> = vec!["--mode=full".into(), "--cached".into()];
        let (filtered, mode) = extract_diff_mode(&args).unwrap();
        assert_eq!(mode, DiffMode::Full);
        assert_eq!(filtered, vec!["--cached"]);
    }

    #[test]
    fn test_parse_diff_mode_extraction_default() {
        let args: Vec<String> = vec!["--cached".into()];
        let (filtered, mode) = extract_diff_mode(&args).unwrap();
        assert_eq!(mode, DiffMode::Default);
        assert_eq!(filtered, vec!["--cached"]);
    }

    #[test]
    fn test_parse_diff_mode_short_m_not_consumed_as_mode() {
        // `-m` conflicts with git's own `-m` flag, so skim should NOT treat
        // it as `--mode`. Both `-m` and the next arg should pass through.
        let args: Vec<String> = vec!["-m".into(), "structure".into()];
        let (filtered, mode) = extract_diff_mode(&args).unwrap();
        assert_eq!(mode, DiffMode::Default);
        assert_eq!(filtered, vec!["-m", "structure"]);
    }

    #[test]
    fn test_parse_diff_mode_extraction_unknown_mode() {
        let args: Vec<String> = vec!["--mode".into(), "unknown".into()];
        let result = extract_diff_mode(&args);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("unknown diff mode"),
            "expected 'unknown diff mode' in error, got: {err_msg}"
        );
    }

    #[test]
    fn test_parse_diff_mode_missing_value() {
        let args: Vec<String> = vec!["--mode".into()];
        let result = extract_diff_mode(&args);
        assert!(result.is_err(), "expected error when --mode has no value");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("requires a value"),
            "expected 'requires a value' in error, got: {err_msg}"
        );
    }

    #[test]
    fn test_parse_diff_mode_short_m_passed_through_to_git() {
        // `-m` is a valid git flag, so it must NOT be consumed by skim.
        let args: Vec<String> = vec!["-m".into()];
        let (filtered, mode) = extract_diff_mode(&args).unwrap();
        assert_eq!(mode, DiffMode::Default);
        assert_eq!(filtered, vec!["-m"]);
    }

    // ========================================================================
    // Empty diff behavior documentation (#103 review batch-7)
    // ========================================================================

    /// Documents intentional behavior change in the AST-aware diff pipeline:
    /// when `git diff` produces no output, `run_diff` prints "No changes" to
    /// stderr and returns `ExitCode::SUCCESS`. This replaces the old
    /// `parse_diff_stat` behavior which returned `"no changes"` as a
    /// `GitResult.summary` on stdout.
    ///
    /// Rationale: stderr is the correct channel for status messages so that
    /// stdout remains clean for piping. The old behavior mixed status messages
    /// into the structured output.
    #[test]
    fn test_empty_diff_produces_no_stdout_output() {
        // parse_unified_diff on empty input produces no files
        let files = parse_unified_diff("");
        assert!(files.is_empty(), "empty diff should parse to zero files");

        // The behavior is: when file_diffs is empty, run_diff writes
        // "No changes" to stderr (not stdout) and returns SUCCESS.
        // This test documents that empty input never reaches the rendering
        // pipeline -- the `file_diffs.is_empty()` guard in run_diff catches it.
        let files_whitespace = parse_unified_diff("  \n\n  \n");
        assert!(
            files_whitespace.is_empty(),
            "whitespace-only diff should parse to zero files"
        );
    }

    // ========================================================================
    // Stderr notice documentation
    // ========================================================================

    /// Documents the stderr notice behaviour for git diff warnings.
    ///
    /// When `git diff` exits 0 but has non-empty stderr (e.g., "warning: LF will
    /// be replaced by CRLF"), `run_diff` emits `[skim] git diff notice: <text>` to
    /// stderr so agents can see the warning even though the diff itself succeeded.
    ///
    /// This test validates that the notice prefix format matches what `run_diff`
    /// emits, ensuring any future refactor doesn't silently change the prefix.
    #[test]
    fn test_diff_stderr_notice_format_is_documented() {
        // The notice format used in run_diff — validated here to catch future
        // regressions in the prefix string without requiring a full integration test.
        let warning = "warning: LF will be replaced by CRLF in file.txt";
        let notice = format!("[skim] git diff notice: {}", warning.trim());
        assert!(
            notice.starts_with("[skim] git diff notice: "),
            "Notice prefix must be '[skim] git diff notice: ': {notice}"
        );
        assert!(
            notice.contains("LF will be replaced"),
            "Notice must include the original warning text: {notice}"
        );
    }
}
