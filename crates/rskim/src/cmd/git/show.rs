//! `skim git show` handler — commit and file-content compression (#132).
//!
//! Dispatches on argument shape:
//! - **File-content mode**: first non-flag token contains `:` → applies skim's
//!   source transform to the file content.
//! - **Commit mode**: first non-flag token has no `:`, or no args (defaults to
//!   HEAD) → parses the commit header + diff, renders with the AST-aware diff
//!   pipeline.
//! - **Passthrough cases**: multi-ref args, stat-family flags, annotated tags,
//!   unsupported file extensions, or parse failures.
//!
//! # Three-tier degradation
//! Commit mode:
//!   Tier 1: parse header + AST-aware diff render.
//!   Tier 2: parse header + raw diff hunk render (AST unavailable).
//!   Tier 3: guardrail fallback to raw git output (compressed > raw).
//!
//! File-content mode:
//!   Tier 1: language supported → transform via rskim-core.
//!   Tier 2: unsupported language → passthrough.
//!   Tier 3: guardrail fallback (transform inflated output) → raw.
//!
//! # Design decisions
//!
//! **AD-7** — Dispatch-on-arg-shape.
//!
//! The single entry point [`run_show`] inspects the first non-flag argument to
//! determine which of the three modes to enter (file-content, commit, multi-ref
//! passthrough). This avoids a separate subcommand (`show-file` / `show-commit`)
//! and mirrors `git show`'s own ambiguity resolution: the presence of `:` in a
//! token unambiguously signals a tree-object ref, while its absence means a
//! commit-ish. All other dispatch logic (passthrough flags, `--json` rejection,
//! annotated-tag detection) is layered on top of this primary shape test.
//!
//! **AD-8 (2026-04-11)** — Commit body and merge-parent preservation.
//!
//! `CommitHeader` now captures `body` (full multi-paragraph commit message
//! below the subject line) and `parents` (the tail of `Merge: ` header lines,
//! stored as a structured `Option<String>` field rather than inlined into the
//! body). `parents` is rendered as `Merge: {parents}` before the summary line;
//! `body` is appended as `\n\n{body}` only when non-empty, so subject-only
//! commits remain compact. GPG/SSH signature blocks (`gpgsig `/ `mergetag `
//! lines and their continuation lines) appear before the blank separator and
//! are silently skipped — they are implementation artefacts, not user content.

use std::path::Path;
use std::process::ExitCode;

use rskim_core::{Language, TransformConfig};

use crate::cmd::{extract_output_format, user_has_flag, OutputFormat};
use crate::output::canonical::{DiffFileEntry, ShowCommitResult};
use crate::runner::CommandRunner;

use rayon::prelude::*;

use super::diff::{
    parse_unified_diff, render_diff_file, DiffMode, MAX_AST_FILE_COUNT, PARALLEL_THRESHOLD,
};
use super::{build_analytics_label, finalize_git_output_owned, map_exit_code, run_passthrough};

// ============================================================================
// Utilities
// ============================================================================

/// Convert `&[String]` to `Vec<&str>` for [`CommandRunner::run`].
///
/// Repeated at call sites in this file; extracted to eliminate boilerplate.
/// We intentionally keep this local rather than changing `CommandRunner::run`'s
/// signature, since that would touch >3 files across the codebase (rewrite,
/// build, test, git modules all share the same pattern).
#[inline]
fn as_str_slice(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

/// Extract the path portion from a `<ref>:<path>` token.
///
/// Git disallows `:` in ref names, so any `:` is a ref/path separator and
/// the path is everything after the last `:`.
///
/// - `HEAD:foo.rs`                 → `foo.rs`
/// - `:foo.rs`                     → `foo.rs` (empty ref = index)
/// - `refs/heads/main:src/lib.rs`  → `src/lib.rs`
/// - `abc:path/with:colon.rs`      → `colon.rs` (splits at last `:`)
///
/// If no `:` is present the whole token is returned unchanged (defensive
/// fallback — `run_show_file_content` is only reached when `detect_show_mode`
/// already confirmed a `:` exists).
#[inline]
fn split_refpath(refpath: &str) -> &str {
    refpath
        .rfind(':')
        .map(|pos| &refpath[pos + 1..])
        .unwrap_or(refpath)
}

// ============================================================================
// Mode detection
// ============================================================================

/// Result of analysing `git show` arguments.
#[derive(Debug, PartialEq)]
enum ShowMode {
    /// `git show [flags] <ref>:<path>` — show file content at a tree ref.
    FileContent {
        /// Full argument token containing the `<ref>:<path>` form.
        refpath: String,
    },
    /// `git show [flags] [<ref>]` — show commit (default: HEAD).
    Commit,
    /// Multiple non-flag tokens without `:` — out of scope, passthrough.
    MultiRef,
}

/// Analyse `show` subcommand args to determine dispatch mode.
///
/// Scans for the first non-flag token:
/// - Contains `:` → `FileContent`.
/// - Exactly one non-flag non-`--` token, no `:` → `Commit`.
/// - Zero non-flag tokens → `Commit` (defaults to HEAD).
/// - Two or more non-flag tokens without `:` → `MultiRef`.
fn detect_show_mode(args: &[String]) -> ShowMode {
    let mut non_flag_count: usize = 0;
    let mut past_separator = false;

    for arg in args {
        if arg == "--" {
            past_separator = true;
            continue;
        }
        if past_separator {
            // Everything after `--` is a path filter, not a ref.
            // Path filters don't change commit vs multi-ref detection.
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        // Non-flag token.
        if arg.contains(':') {
            return ShowMode::FileContent {
                refpath: arg.clone(),
            };
        }
        non_flag_count += 1;
    }

    match non_flag_count {
        0 | 1 => ShowMode::Commit,
        _ => ShowMode::MultiRef,
    }
}

// ============================================================================
// Passthrough flags
// ============================================================================

/// Flags that bypass show compression and go directly to git.
///
/// These produce specialized output (stats, raw metadata) that skim's
/// parser cannot meaningfully compress.
const PASSTHROUGH_FLAGS: &[&str] = &[
    "--stat",
    "--shortstat",
    "--numstat",
    "--name-only",
    "--name-status",
    "--raw",
    "--check",
    "--format",
    "--pretty",
];

// ============================================================================
// Entry point
// ============================================================================

/// Run the `git show` subcommand.
///
/// Called from `cmd/git/mod.rs` with global_flags already split off and
/// `show_stats` extracted. `args` contains everything after `show`.
pub(super) fn run_show(
    global_flags: &[String],
    args: &[String],
    show_stats: bool,
    analytics_enabled: bool,
) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_show_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Passthrough for stat-family and format flags.
    if user_has_flag(args, PASSTHROUGH_FLAGS) {
        return run_passthrough(global_flags, "show", args, show_stats, analytics_enabled);
    }

    match detect_show_mode(args) {
        ShowMode::MultiRef => {
            run_passthrough(global_flags, "show", args, show_stats, analytics_enabled)
        }
        ShowMode::FileContent { refpath } => {
            run_show_file_content(global_flags, args, &refpath, show_stats, analytics_enabled)
        }
        ShowMode::Commit => {
            let (git_args, output_format) = extract_output_format(args);
            run_show_commit(
                global_flags,
                &git_args,
                args,
                output_format,
                show_stats,
                analytics_enabled,
            )
        }
    }
}

// ============================================================================
// Commit mode
// ============================================================================

/// Parsed fields from a `git show` commit header.
#[derive(Debug, Default)]
struct CommitHeader {
    /// Full 40-character commit hash.
    hash: String,
    /// Author name and email.
    author: String,
    /// Commit date string.
    date: String,
    /// First (subject) line of the commit message.
    subject: String,
    /// Full commit message body below the subject line (may be empty).
    ///
    /// # AD-8 (2026-04-11)
    /// Multi-paragraph bodies are preserved verbatim with 4-space indent stripped.
    /// Empty when the commit has only a subject line.
    body: String,
    /// Tail of a `Merge: ` header line, when present (e.g. `"abc123 def456"`).
    ///
    /// # AD-8 (2026-04-11)
    /// Stored as a structured field rather than inlined into `body` so that
    /// `ShowCommitResult::render` can emit `Merge: {parents}` as a dedicated
    /// prefix line. Octopus merges have all parent hashes in one space-separated
    /// string — the tail is stored unchanged.
    parents: Option<String>,
}

/// Walk `header_region` lines and populate `header` with extracted fields.
///
/// Returns the accumulated body lines as `Vec<&str>` slices borrowed from
/// `header_region`, avoiding per-line allocations (zero-copy body accumulation).
///
/// # State machine
/// - Phase 0 (`in_body == false`): parse git trailer lines (`commit `,
///   `Author: `, `Date: `, `Merge: `). All other lines (gpgsig, mergetag,
///   continuation lines starting with a space) are silently skipped per AD-8.
/// - Phase 1 (`in_body == true`, `subject_captured == false`): capture the
///   subject (first non-blank body line), stripping the canonical 4-space indent.
/// - Phase 2 (`in_body == true`, `subject_captured == true`): accumulate body
///   lines, stripping 4-space indent.
fn parse_header_lines<'a>(header_region: &'a str, header: &mut CommitHeader) -> Vec<&'a str> {
    // Extract the trimmed value from a `Key: value` header line.
    let header_value = |line: &str, prefix: &str| -> String {
        line.strip_prefix(prefix)
            .unwrap_or_default()
            .trim()
            .to_string()
    };

    let mut in_body = false;
    let mut subject_captured = false;
    let mut body_lines: Vec<&'a str> = Vec::new();

    for line in header_region.lines() {
        if !in_body {
            if line.starts_with("commit ") {
                header.hash = header_value(line, "commit ");
            } else if line.starts_with("Merge: ") {
                // AD-8: capture merge parents as structured field.
                header.parents = Some(header_value(line, "Merge: "));
            } else if line.starts_with("Author: ") {
                header.author = header_value(line, "Author: ");
            } else if line.starts_with("Date: ") {
                header.date = header_value(line, "Date: ");
            } else if line.is_empty() && !header.hash.is_empty() {
                in_body = true;
            }
        } else if !subject_captured {
            // Phase 1: first non-blank line is the subject.
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                header.subject = line.strip_prefix("    ").unwrap_or(trimmed).to_string();
                subject_captured = true;
            }
        } else {
            // Phase 2: borrow each body line slice — no allocation per line.
            body_lines.push(line.strip_prefix("    ").unwrap_or(line));
        }
    }

    body_lines
}

/// Trim leading and trailing blank lines from accumulated body slices and join.
///
/// Leading blanks arise from the blank separator between subject and body.
/// Trailing blanks arise from the header_region split position.
/// Returns an empty `String` when no non-blank lines remain.
fn trim_body_blanks(body_lines: &[&str]) -> String {
    let start = body_lines
        .iter()
        .position(|l| !l.trim().is_empty())
        .unwrap_or(body_lines.len());
    let end = body_lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map(|p| p + 1)
        .unwrap_or(0);

    if start < end {
        body_lines[start..end].join("\n")
    } else {
        String::new()
    }
}

/// Parse the commit header and split off the diff body from `git show` output.
///
/// Returns `(header, diff_body)` where `diff_body` starts at the first
/// `diff --git` line, or is empty if no diff is present.
///
/// Returns `None` when the output does not start with `commit ` (e.g., annotated
/// tags) — those fall back to passthrough.
///
/// # Line-ending handling
/// The diff-body split uses a direct substring search (`str::find`) rather
/// than summing per-line byte lengths. This is robust to CRLF endings,
/// missing trailing newlines, and other quirks that would misalign a
/// hand-rolled byte counter. Git outputs LF by default but users may pipe
/// through tools that introduce CRLF.
///
/// # Signature blocks (AD-8)
/// `gpgsig ` and `mergetag ` header lines (and their multi-line continuations
/// that start with a space) appear between the `commit ` line and the blank
/// separator. They are silently skipped — they are implementation artefacts,
/// not user-authored content. The skip is implicit: only lines whose prefixes
/// are explicitly recognised (`commit `, `Author: `, `Date: `, `Merge: `) are
/// captured; everything else is ignored.
fn parse_commit_header(raw: &str) -> Option<(CommitHeader, &str)> {
    // Annotated tags start with `tag ` not `commit `.
    if !raw.starts_with("commit ") {
        return None;
    }

    // Locate the split position between the commit header and the diff body.
    // The leading `\n` anchors the match to the start of a line to avoid
    // false positives inside commit message bodies that might mention
    // `diff --git` textually.
    let split_pos = raw
        .find("\ndiff --git ")
        .map(|p| p + 1)
        .unwrap_or(raw.len());
    let (header_region, diff_body) = raw.split_at(split_pos);

    let mut header = CommitHeader::default();
    let body_lines = parse_header_lines(header_region, &mut header);

    if header.hash.is_empty() {
        return None;
    }

    header.body = trim_body_blanks(&body_lines);

    Some((header, diff_body))
}

/// Outcome of invoking `git show` via [`run_git_show_raw`].
///
/// Split into `Success` and `Failure` so callers can record failure analytics
/// at the call site without losing the stdout that git produced on the error
/// path (e.g. partial output from `git show INVALID`). Mirrors the
/// non-zero-exit recording pattern established in `run_parsed_command`
/// (cmd/git/mod.rs) and `diff/mod.rs`.
enum ShowRawOutcome {
    Success {
        stdout: String,
        duration: std::time::Duration,
    },
    Failure {
        stdout: String,
        exit_code: ExitCode,
        duration: std::time::Duration,
    },
}

/// Execute `git show` and return a structured outcome.
///
/// On non-zero exit the error streams are forwarded to the terminal, and the
/// stdout, exit code, and duration are returned via [`ShowRawOutcome::Failure`]
/// so the caller can record analytics for the failed invocation (Commit 9).
fn run_git_show_raw(
    global_flags: &[String],
    git_args: &[String],
) -> anyhow::Result<ShowRawOutcome> {
    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend(["show".to_string(), "--no-color".to_string()]);
    full_args.extend_from_slice(git_args);

    let runner = CommandRunner::new(None);
    let output = runner.run("git", &as_str_slice(&full_args))?;

    if output.exit_code != Some(0) {
        if !output.stderr.is_empty() {
            eprint!("{}", output.stderr);
        }
        if !output.stdout.is_empty() {
            print!("{}", output.stdout);
        }
        return Ok(ShowRawOutcome::Failure {
            stdout: output.stdout,
            exit_code: map_exit_code(output.exit_code),
            duration: output.duration,
        });
    }

    Ok(ShowRawOutcome::Success {
        stdout: output.stdout,
        duration: output.duration,
    })
}

/// Parse the commit body and render it into a `ShowCommitResult`.
///
/// # SRP note
///
/// This function both *parses* (`parse_commit_header`, `parse_unified_diff`)
/// and *renders* (`render_diff_file`, `ShowCommitResult::new`). Splitting
/// these into a pure-parse step and a pure-render step would reduce the hot-path
/// scan from two O(n) passes to one, but requires exposing an intermediate
/// `ParsedCommit` struct. That refactor is deferred until a second caller
/// emerges; until then the dual responsibility is documented here so it is not
/// silently expanded.
///
/// Returns `None` when the raw output does not represent a regular commit
/// (e.g., annotated tag, blob, tree) — the caller should passthrough in that
/// case. When `Some`, the returned result contains the rendered diff text and
/// metadata ready for format dispatch.
fn render_show_diff(
    raw: &str,
    global_flags: &[String],
    git_args: &[String],
) -> Option<ShowCommitResult> {
    let (header, diff_body) = parse_commit_header(raw)?;

    let file_diffs = parse_unified_diff(diff_body);

    // Mirror run_diff's parallel dispatch: use rayon when file count exceeds
    // PARALLEL_THRESHOLD, serial otherwise.  `par_iter().collect()` preserves
    // insertion order so output is deterministic regardless of scheduling.
    let render_one = |i: usize, fd: &_| {
        let rendered = render_diff_file(
            fd,
            global_flags,
            git_args,
            DiffMode::Default,
            i >= MAX_AST_FILE_COUNT,
        );
        let entry = DiffFileEntry {
            path: fd.path.clone(),
            status: fd.status.clone(),
            changed_regions: fd.hunks.len(),
        };
        (rendered, entry)
    };

    let rendered_files: Vec<(String, DiffFileEntry)> = if file_diffs.len() >= PARALLEL_THRESHOLD {
        file_diffs
            .par_iter()
            .enumerate()
            .map(|(i, fd)| render_one(i, fd))
            .collect()
    } else {
        file_diffs
            .iter()
            .enumerate()
            .map(|(i, fd)| render_one(i, fd))
            .collect()
    };

    let mut rendered_diff = String::new();
    let mut diff_file_entries: Vec<DiffFileEntry> = Vec::with_capacity(rendered_files.len());
    for (rendered, entry) in rendered_files {
        rendered_diff.push_str(&rendered);
        diff_file_entries.push(entry);
    }

    Some(ShowCommitResult::new(
        header.hash,
        header.author,
        header.date,
        header.subject,
        header.body,
        header.parents,
        diff_file_entries,
        &rendered_diff,
    ))
}

/// Dispatch `ShowCommitResult` to the requested output format and record stats.
///
/// Accepts ownership of `raw` to avoid cloning for the common text+analytics
/// path. The `label` is pre-built lazily by the caller (empty string when
/// neither stats nor analytics are active).
///
/// Both output formats use [`finalize_git_output_owned`] to move strings
/// directly into the analytics call — eliminating the conditional `Option`
/// clone dance that previously required a TOCTOU double-check of
/// an analytics-enabled global (MEDIUM-11, MEDIUM-22).
fn emit_show_commit(
    result: ShowCommitResult,
    raw: String,
    label: String,
    output_format: OutputFormat,
    show_stats: bool,
    analytics_enabled: bool,
    duration: std::time::Duration,
) -> anyhow::Result<()> {
    match output_format {
        OutputFormat::Json => {
            // JSON: serialise result directly; guardrail is irrelevant here
            // because the JSON output is never substituted for raw text.
            // Running the guardrail on JSON would double the memory cost and
            // could spuriously emit `[skim:guardrail]` to stderr.
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| anyhow::anyhow!("failed to serialize show result: {e}"))?;
            println!("{json}");
            finalize_git_output_owned(
                raw,
                json,
                label,
                show_stats,
                analytics_enabled,
                crate::analytics::CommandType::Git,
                duration,
                Some("full"),
            );
        }
        OutputFormat::Text => {
            // Apply guardrail: if compressed output is larger than raw, emit raw.
            // `into_rendered` consumes result and returns the pre-built String
            // directly, avoiding the extra allocation `to_string()` would incur.
            let result_str = result.into_rendered();
            // Clone raw only when the caller will actually consume it: either
            // --show-stats is printing token counts or analytics is recording.
            // Guarding here avoids a full memcpy (~100-500 KB) on the no-telemetry
            // hot path (HIGH-1).  The owned variant then moves both strings into
            // `finalize_git_output_owned` without further cloning (MEDIUM-22).
            let raw_for_record = if show_stats || analytics_enabled {
                raw.clone()
            } else {
                String::new()
            };
            let guardrail = crate::output::guardrail::apply_to_stderr(raw, result_str)?;
            let final_output = guardrail.into_output();
            print!("{final_output}");
            finalize_git_output_owned(
                raw_for_record,
                final_output,
                label,
                show_stats,
                analytics_enabled,
                crate::analytics::CommandType::Git,
                duration,
                Some("full"),
            );
        }
    }
    Ok(())
}

/// Run `git show` in commit mode: parse header + AST-aware diff.
///
/// `original_args` is the full args slice before `--json` extraction, used to
/// build the analytics label.  This preserves the `--json` flag in the label
/// so the analytics DB can distinguish `skim git show HEAD --json` from
/// `skim git show HEAD`, matching the label convention in `diff/mod.rs`.
fn run_show_commit(
    global_flags: &[String],
    git_args: &[String],
    original_args: &[String],
    output_format: OutputFormat,
    show_stats: bool,
    analytics_enabled: bool,
) -> anyhow::Result<ExitCode> {
    let (raw, duration) = match run_git_show_raw(global_flags, git_args)? {
        ShowRawOutcome::Success { stdout, duration } => (stdout, duration),
        ShowRawOutcome::Failure {
            stdout,
            exit_code,
            duration,
        } => {
            // Record analytics on the failure path so the DB reflects failed
            // `git show` invocations (Commit 9). raw == compressed
            // (passthrough semantics) — mirrors run_parsed_command and
            // run_diff non-zero-exit recording. Move stdout: 1 allocation
            // (clone) on the analytics path, 0 when disabled (PF-018).
            super::finalize_git_output_passthrough(
                stdout,
                build_analytics_label("show", original_args, show_stats, analytics_enabled),
                show_stats,
                analytics_enabled,
                crate::analytics::CommandType::Git,
                duration,
                Some("passthrough"),
            );
            return Ok(exit_code);
        }
    };

    // Built before the `render_show_diff` check so both the passthrough and
    // the normal path share the same label (HIGH-3).  Derived from *original*
    // args (before `--json` extraction) so the DB records the full invocation.
    let label = build_analytics_label("show", original_args, show_stats, analytics_enabled);

    let Some(result) = render_show_diff(&raw, global_flags, git_args) else {
        // Not a regular commit (annotated tag, blob, tree, etc.) — passthrough.
        // Route through finalize so the analytics DB records a zero-compression
        // entry instead of silently dropping the invocation (HIGH-3).
        // raw == output; move raw into the passthrough variant: 1 allocation
        // (clone) on the analytics path, 0 when disabled (PF-018 resolution).
        print!("{raw}");
        super::finalize_git_output_passthrough(
            raw,
            label,
            show_stats,
            analytics_enabled,
            crate::analytics::CommandType::Git,
            duration,
            Some("passthrough"),
        );
        return Ok(ExitCode::SUCCESS);
    };

    emit_show_commit(
        result,
        raw,
        label,
        output_format,
        show_stats,
        analytics_enabled,
        duration,
    )?;
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// File-content mode
// ============================================================================

/// Emit raw `git show` output unchanged and record analytics/stats.
///
/// Tiers 2 (unsupported extension) and 3 (transform error) share this path.
/// The guardrail-inflate case (Tier 1 with inflate) does NOT call this
/// function; the guardrail's `into_output()` returns the raw string directly
/// and the Tier-1 `finalize_git_output` call records the zero-compression
/// result inline (MEDIUM-23).
///
/// Centralising tiers 2 and 3 here ensures consistent analytics accounting:
/// raw == output so the DB records zero compression gain, matching the
/// behaviour of `run_passthrough` for other subcommands.
///
/// `label` is a pre-built String passed from `run_show_file_content`.  It is
/// computed via a guarded `if/else` that returns `String::new()` when neither
/// `--show-stats` nor analytics are enabled, so each branch allocates at most
/// one String — allocation is deferred to branch-time, not cached (MEDIUM-24).
///
/// Emits a stderr notice matching `diff/mod.rs:301` `[skim:guardrail]` style so
/// callers can observe which tier was selected without parsing structured output.
fn passthrough_file_content(
    raw: String,
    label: String,
    show_stats: bool,
    analytics_enabled: bool,
    duration: std::time::Duration,
    tier: u8,
) {
    eprintln!("[skim] git show: falling back to raw (tier {tier})");
    print!("{raw}");
    // raw == output (passthrough); move raw into finalize_git_output_passthrough
    // so the analytics path clones once and moves once — 1 allocation total
    // instead of 2 (PF-018 resolution).
    // Map numeric tier to canonical tier-name strings for the analytics DB.
    let tier_name: Option<&'static str> = match tier {
        1 => Some("full"),
        2 => Some("degraded"),
        3 => Some("passthrough"),
        _ => None,
    };
    super::finalize_git_output_passthrough(
        raw,
        label,
        show_stats,
        analytics_enabled,
        crate::analytics::CommandType::Git,
        duration,
        tier_name,
    );
}

/// Run `git show <ref>:<path>` in file-content mode.
///
/// Four-tier dispatch:
///   Tier 0: `--json` flag → exit 2 (unsupported).
///   Tier 1: language supported → transform via rskim-core + guardrail.
///            Guardrail-inflate sub-case: guardrail returns raw; recorded
///            inline by the Tier-1 `finalize_git_output` call (not via
///            `passthrough_file_content`).
///   Tier 2: unsupported or serde-based extension → `passthrough_file_content`.
///   Tier 3: transform error → `passthrough_file_content`.
fn run_show_file_content(
    global_flags: &[String],
    args: &[String],
    refpath: &str,
    show_stats: bool,
    analytics_enabled: bool,
) -> anyhow::Result<ExitCode> {
    // --json is not meaningful for file-content mode.
    if user_has_flag(args, &["--json"]) {
        eprintln!(
            "Error: --json is not supported for `git show <ref>:<path>` (file-content mode); \
             the output is already the compressed artifact"
        );
        return Ok(ExitCode::from(2));
    }

    // Extract the path component from `<ref>:<path>` (everything after the last `:`).
    // Git disallows `:` inside ref names, so any `:` in the token is a ref/path separator.
    let path_str = split_refpath(refpath);

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.push("show".to_string());
    // --no-color matches commit-mode's run_git_show_raw: prevents ANSI escapes
    // from user configs that set `color.ui = always` (MEDIUM-17).
    full_args.push("--no-color".to_string());
    // Pass through all original args (show.rs does not strip args for file-content mode).
    full_args.extend_from_slice(args);

    let runner = CommandRunner::new(None);
    let output = runner.run("git", &as_str_slice(&full_args))?;

    if output.exit_code != Some(0) {
        if !output.stderr.is_empty() {
            eprint!("{}", output.stderr);
        }
        if !output.stdout.is_empty() {
            print!("{}", output.stdout);
        }
        let exit_code = output.exit_code;
        // Record analytics on the error path so the DB reflects failed
        // invocations (e.g. `git show HEAD:missing.rs`). Move stdout: 1
        // allocation (clone) on the analytics path, 0 when disabled (PF-018).
        super::finalize_git_output_passthrough(
            output.stdout,
            build_analytics_label("show", args, show_stats, analytics_enabled),
            show_stats,
            analytics_enabled,
            crate::analytics::CommandType::Git,
            output.duration,
            Some("passthrough"),
        );
        return Ok(map_exit_code(exit_code));
    }

    let raw = output.stdout;
    let duration = output.duration;

    let label = build_analytics_label("show", args, show_stats, analytics_enabled);

    // Detect language from path extension.
    let lang = Language::from_path(Path::new(path_str)).filter(|l| !l.is_serde_based());

    let Some(lang) = lang else {
        // Tier 2: unsupported or serde-based language — passthrough.
        // Move raw: the else branch always returns, so Rust knows raw is
        // available after the let-else for the Tier 1 path.
        passthrough_file_content(raw, label, show_stats, analytics_enabled, duration, 2);
        return Ok(ExitCode::SUCCESS);
    };

    // Tier 1: transform in memory.
    let config = TransformConfig::default();
    let transformed = match rskim_core::transform(&raw, lang, config.mode) {
        Ok(t) => t,
        Err(e) => {
            // Tier 3: transform failed — fall back to raw passthrough.
            // Record as a zero-compression pass so analytics and --show-stats
            // remain consistent with the unsupported-language branch above.
            // Move raw: the Err arm always returns, so Rust knows raw is
            // available after the match for the Ok path.
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "[skim:debug] git show file-content transform failed for {path_str}: {e}"
                );
            }
            passthrough_file_content(raw, label, show_stats, analytics_enabled, duration, 3);
            return Ok(ExitCode::SUCCESS);
        }
    };

    // Guardrail: if transformation inflated the output, emit raw.
    // Clone raw only here (Tier 1 success path), not on every branch (MEDIUM-18).
    // `apply_to_stderr` takes ownership of raw; clone it first so we can pass
    // the original into `finalize_git_output_owned` without a second allocation.
    let raw_for_record = if show_stats || analytics_enabled {
        raw.clone()
    } else {
        String::new()
    };
    let guardrail = crate::output::guardrail::apply_to_stderr(raw, transformed)?;
    let final_output = guardrail.into_output();

    print!("{final_output}");
    // Both raw_for_record and final_output are owned Strings; use the owned
    // variant to move them directly into analytics, avoiding two extra .to_string()
    // clones that the borrowed finalize_git_output would incur (HIGH-3, PF-018).
    finalize_git_output_owned(
        raw_for_record,
        final_output,
        label,
        show_stats,
        analytics_enabled,
        crate::analytics::CommandType::Git,
        duration,
        Some("full"),
    );

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Help
// ============================================================================

fn print_show_help() {
    println!("skim git show \u{2014} commit and file-content compression");
    println!();
    println!("USAGE:");
    println!("    skim git show [OPTIONS] [<commit>]");
    println!("    skim git show [OPTIONS] <ref>:<path>");
    println!();
    println!("MODES:");
    println!("    Commit mode   : show commit header + AST-aware diff");
    println!("    File mode     : show transformed file content at a ref");
    println!();
    println!("OPTIONS:");
    println!("    --json           Machine-readable JSON output (commit mode only)");
    println!("    --show-stats     Show token savings statistics");
    println!();
    println!("PASSTHROUGH FLAGS (no compression):");
    println!("    --stat, --shortstat, --numstat, --name-only, --name-status");
    println!("    --raw, --check, --format, --pretty");
    println!();
    println!("NOTES:");
    println!("    --json is not supported in file-content mode (<ref>:<path>).");
    println!("    Passing --json with a file-content ref exits with code 2.");
    println!();
    println!("EXAMPLES:");
    println!("    skim git show HEAD");
    println!("    skim git show HEAD:src/main.rs");
    println!("    skim git show HEAD:README.md      # unsupported ext → raw passthrough");
    println!("    skim git show abc123 --json");
    println!("    skim git show v1.0.0              # annotated tag → passthrough");
    println!("    skim git show --stat HEAD         # passthrough to git");
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Mode detection tests
    // ========================================================================

    #[test]
    fn test_detect_file_content_mode_simple() {
        let args: Vec<String> = vec!["HEAD:foo.rs".into()];
        assert_eq!(
            detect_show_mode(&args),
            ShowMode::FileContent {
                refpath: "HEAD:foo.rs".to_string()
            }
        );
    }

    #[test]
    fn test_detect_file_content_mode_with_slashes_in_ref() {
        let args: Vec<String> = vec!["refs/heads/main:src/lib.rs".into()];
        match detect_show_mode(&args) {
            ShowMode::FileContent { refpath } => {
                assert_eq!(refpath, "refs/heads/main:src/lib.rs");
            }
            other => panic!("Expected FileContent, got {other:?}"),
        }
    }

    #[test]
    fn test_detect_file_content_mode_empty_ref() {
        // `:foo.rs` — empty ref means index.
        let args: Vec<String> = vec![":foo.rs".into()];
        match detect_show_mode(&args) {
            ShowMode::FileContent { refpath } => {
                assert_eq!(refpath, ":foo.rs");
            }
            other => panic!("Expected FileContent, got {other:?}"),
        }
    }

    #[test]
    fn test_detect_commit_mode_single_ref() {
        let args: Vec<String> = vec!["abc123".into()];
        assert_eq!(detect_show_mode(&args), ShowMode::Commit);
    }

    #[test]
    fn test_detect_commit_mode_default_head() {
        let args: Vec<String> = vec![];
        assert_eq!(detect_show_mode(&args), ShowMode::Commit);
    }

    #[test]
    fn test_detect_commit_mode_with_path_filter() {
        // `HEAD -- foo.rs` — path filter after `--` does not count as a second ref.
        let args: Vec<String> = vec!["HEAD".into(), "--".into(), "foo.rs".into()];
        assert_eq!(detect_show_mode(&args), ShowMode::Commit);
    }

    #[test]
    fn test_detect_multiple_refs_passthrough() {
        let args: Vec<String> = vec!["HEAD".into(), "HEAD~1".into()];
        assert_eq!(detect_show_mode(&args), ShowMode::MultiRef);
    }

    #[test]
    fn test_detect_flags_ignored_in_mode_detection() {
        // Flags before the ref should not count as non-flag tokens.
        let args: Vec<String> = vec!["--no-color".into(), "HEAD:src/main.rs".into()];
        match detect_show_mode(&args) {
            ShowMode::FileContent { refpath } => {
                assert_eq!(refpath, "HEAD:src/main.rs");
            }
            other => panic!("Expected FileContent, got {other:?}"),
        }
    }

    // ========================================================================
    // Commit header parsing tests
    // ========================================================================

    #[test]
    fn test_parse_commit_header_basic() {
        let fixture = include_str!("../../../tests/fixtures/cmd/git/show_commit.txt");
        let (header, diff_body) = parse_commit_header(fixture).expect("should parse commit");
        assert_eq!(
            &header.hash[..7],
            "abc1234",
            "hash prefix must be 'abc1234', got: {}",
            header.hash
        );
        assert_eq!(
            header.author, "Jane Dev <jane@example.com>",
            "author must match exactly"
        );
        assert_eq!(
            header.subject, "feat: add user authentication handler",
            "subject must match exactly"
        );
        assert!(
            diff_body.starts_with("diff --git "),
            "diff body must start with 'diff --git ', got: {:?}",
            &diff_body[..diff_body.len().min(40)]
        );
    }

    #[test]
    fn test_parse_commit_header_annotated_tag_returns_none() {
        let fixture = include_str!("../../../tests/fixtures/cmd/git/show_tag.txt");
        assert!(
            parse_commit_header(fixture).is_none(),
            "annotated tag output must return None (falls back to passthrough)"
        );
    }

    #[test]
    fn test_parse_commit_header_empty_returns_none() {
        assert!(parse_commit_header("").is_none());
    }

    /// CRLF line endings must not misalign the `diff_body` split.
    ///
    /// Earlier the parser walked `byte_pos` by `line.len() + 1`, which
    /// under-counted CRLF endings by 1 byte per line. With multi-line
    /// headers the diff body slice would start mid-byte and break the
    /// unified-diff parser downstream. The find-based implementation is
    /// line-ending-agnostic.
    #[test]
    fn test_parse_commit_header_crlf_line_endings() {
        let raw = "commit abc1234\r\n\
                   Author: Test <t@t.com>\r\n\
                   Date:   Thu Apr 10 12:00:00 2025\r\n\
                   \r\n\
                       feat: crlf subject\r\n\
                   \r\n\
                   diff --git a/x.rs b/x.rs\r\n\
                   index aaa..bbb 100644\r\n";
        let (header, diff_body) = parse_commit_header(raw).expect("CRLF commit should parse");
        assert!(
            header.hash.starts_with("abc1234"),
            "hash must be parsed with CRLF, got: {:?}",
            header.hash
        );
        assert_eq!(header.subject, "feat: crlf subject");
        assert!(
            diff_body.starts_with("diff --git "),
            "diff_body must start exactly at `diff --git ` (no stray \\r or header bytes): {:?}",
            &diff_body[..diff_body.len().min(40)]
        );
    }

    /// A commit with no trailing newline must still parse cleanly.
    #[test]
    fn test_parse_commit_header_no_trailing_newline() {
        let raw = "commit abc1234\nAuthor: Test\nDate: now\n\n    subject";
        let (header, diff_body) =
            parse_commit_header(raw).expect("missing-trailing-newline commit should parse");
        assert_eq!(header.subject, "subject");
        assert!(
            diff_body.is_empty(),
            "empty diff body for header-only commit"
        );
    }

    // ========================================================================
    // Commit mode reuse of AST renderer
    // ========================================================================

    #[test]
    fn test_commit_mode_parses_fixture_and_renders_diff() {
        let fixture = include_str!("../../../tests/fixtures/cmd/git/show_commit.txt");
        let (header, diff_body) = parse_commit_header(fixture).unwrap();
        let file_diffs = parse_unified_diff(diff_body);
        assert!(
            !file_diffs.is_empty(),
            "fixture must produce at least one file diff"
        );

        // Render each file — should not panic.
        for (i, fd) in file_diffs.iter().enumerate() {
            let rendered = render_diff_file(fd, &[], &[], DiffMode::Default, i >= 200);
            assert!(!rendered.is_empty(), "render should produce output");
        }

        // The ShowCommitResult should include header fields.
        let result = ShowCommitResult::new(
            header.hash,
            header.author,
            header.date,
            header.subject,
            String::new(),
            None,
            vec![],
            "diff output",
        );
        let rendered = result.to_string();
        assert!(
            rendered.contains("abc1234"),
            "hash must appear in rendered output"
        );
        assert!(rendered.contains("feat: add user authentication handler"));
    }

    // ========================================================================
    // File-content mode language detection
    // ========================================================================

    #[test]
    fn test_file_content_mode_language_detection_rs() {
        let path = Path::new("src/main.rs");
        let lang = Language::from_path(path);
        assert!(lang.is_some(), "Rust files must have a detected language");
        assert!(!lang.unwrap().is_serde_based());
    }

    #[test]
    fn test_file_content_mode_language_detection_unknown() {
        let path = Path::new("file.lock");
        let lang = Language::from_path(path).filter(|l| !l.is_serde_based());
        assert!(lang.is_none(), ".lock files have no supported language");
    }

    #[test]
    fn test_file_content_mode_transforms_supported_language() {
        // Transform the Rust fixture in-memory and verify token reduction.
        let source = include_str!("../../../tests/fixtures/cmd/git/show_file.rs");
        let lang = Language::from_path(Path::new("show_file.rs")).unwrap();
        let config = TransformConfig::default();
        let transformed = rskim_core::transform(source, lang, config.mode).unwrap();
        assert!(
            transformed.len() < source.len(),
            "transform must shrink the source ({} → {})",
            source.len(),
            transformed.len()
        );
    }

    // ========================================================================
    // Passthrough flags
    // ========================================================================

    #[test]
    fn test_stat_family_flag_passthrough_detection() {
        let args: Vec<String> = vec!["--stat".into(), "HEAD".into()];
        assert!(
            user_has_flag(&args, PASSTHROUGH_FLAGS),
            "--stat must trigger passthrough"
        );
    }

    #[test]
    fn test_format_flag_passthrough_detection() {
        let args: Vec<String> = vec!["--format=%H".into()];
        assert!(
            user_has_flag(&args, PASSTHROUGH_FLAGS),
            "--format=... must trigger passthrough"
        );
    }

    #[test]
    fn test_no_passthrough_flags_does_not_trigger() {
        let args: Vec<String> = vec!["HEAD".into()];
        assert!(!user_has_flag(&args, PASSTHROUGH_FLAGS));
    }

    // ========================================================================
    // --json rejection in file-content mode
    // ========================================================================

    /// `--json` in file-content mode must exit 2.
    ///
    /// Tests the actual `run_show_file_content` entry path: the function must
    /// return `ExitCode::from(2)` immediately when `--json` is present, without
    /// spawning a git process (no real git invocation needed here).
    ///
    /// The full E2E path (real binary + stderr message) is covered by
    /// `test_skim_git_show_file_content_json_rejected` in `tests/cli_git.rs`.
    #[test]
    fn test_file_content_mode_json_rejected() {
        let global_flags: Vec<String> = vec![];
        let args: Vec<String> = vec!["HEAD:src/main.rs".into(), "--json".into()];
        let result = run_show_file_content(&global_flags, &args, "HEAD:src/main.rs", false, false)
            .expect("run_show_file_content must not return an anyhow error for --json rejection");
        assert_eq!(
            result,
            ExitCode::from(2),
            "--json in file-content mode must return exit code 2"
        );
    }

    // ========================================================================
    // render_show_diff: unit tests for the pure rendering helper
    // ========================================================================

    /// `render_show_diff` with a well-formed header + no diff body returns Some
    /// with a result that carries the expected header fields.
    ///
    /// This is the Tier-2 path (header parsed, zero AST files) and confirms
    /// the result is reachable and contains correct metadata.
    #[test]
    fn test_render_show_diff_header_only_commit() {
        let raw = "commit abc1234\nAuthor: Jane Dev <jane@example.com>\nDate: Thu Apr 10 2025\n\n    feat: header only\n";
        let result = render_show_diff(raw, &[], &[]);
        let result = result.expect("well-formed commit without diff must produce Some");
        let rendered = result.to_string();
        assert!(
            rendered.contains("abc1234"),
            "rendered output must include the commit hash"
        );
        assert!(
            rendered.contains("feat: header only"),
            "rendered output must include the commit subject"
        );
    }

    /// `render_show_diff` with input that does not start with `commit ` returns None,
    /// verifying the annotated-tag / blob passthrough path is reachable.
    #[test]
    fn test_render_show_diff_non_commit_returns_none() {
        let raw = "tag v1.0.0\nTagger: Someone\nDate: ...\n\n    Release notes\n";
        assert!(
            render_show_diff(raw, &[], &[]).is_none(),
            "non-commit raw output must return None (passthrough path)"
        );
    }

    /// `render_show_diff` with the full fixture produces a result containing
    /// the file path from the diff — verifying the Tier-1 (AST) path is
    /// exercised end-to-end through the pure helper.
    #[test]
    fn test_render_show_diff_full_fixture_tier1() {
        let fixture = include_str!("../../../tests/fixtures/cmd/git/show_commit.txt");
        let result =
            render_show_diff(fixture, &[], &[]).expect("fixture commit must render successfully");
        let rendered = result.to_string();
        assert!(
            rendered.contains("abc1234"),
            "hash must appear in Tier-1 rendered output"
        );
        assert!(
            rendered.contains("feat: add user authentication handler"),
            "subject must appear in Tier-1 rendered output"
        );
    }

    // ========================================================================
    // Show no panic on malformed input
    // ========================================================================

    #[test]
    fn test_show_no_panic_on_malformed_commit_header() {
        // Input that does not start with "commit " must return None.
        // parse_commit_header returns None for anything that isn't a regular commit
        // preamble, including garbage bytes, annotated-tag output, etc.
        let garbage = "\x00\x01\x02\x03 garbage bytes here";
        let result = parse_commit_header(garbage);
        assert!(
            result.is_none(),
            "malformed input must return None, not panic or produce a header"
        );
    }

    #[test]
    fn test_show_no_panic_on_empty_diff_body() {
        // A commit with no diff body should parse successfully and produce an
        // empty file list.  The conditional `if let` was silently passing when
        // parse_commit_header returned None — now we assert the expected shape.
        let raw = "commit abc1234\nAuthor: Test <t@t.com>\nDate: Thu\n\n    subject\n";
        let (header, diff_body) =
            parse_commit_header(raw).expect("well-formed header-only commit must parse");
        assert_eq!(
            header.subject, "subject",
            "subject must be parsed from indented commit message line"
        );
        let files = parse_unified_diff(diff_body);
        assert!(
            files.is_empty(),
            "header-only commit (no diff --git lines) must produce zero FileDiff entries"
        );
    }

    // ========================================================================
    // PASSTHROUGH_FLAGS coverage (complexity-7)
    // ========================================================================

    /// Every entry in `PASSTHROUGH_FLAGS` must trigger the passthrough branch.
    ///
    /// `user_has_flag` does prefix matching, so `--format` catches `--format=%H`
    /// and similar. This table-driven test documents every flag and asserts
    /// that none has been accidentally dropped or misspelled.
    #[test]
    fn test_passthrough_flags_all_rewrite_correctly() {
        // For each flag, construct a minimal args slice that contains it,
        // then verify `user_has_flag` fires.  The second element is a
        // representative value — some flags take `=value`, some stand alone.
        let cases: &[(&str, &str)] = &[
            ("--stat", "--stat"),
            ("--shortstat", "--shortstat"),
            ("--numstat", "--numstat"),
            ("--name-only", "--name-only"),
            ("--name-status", "--name-status"),
            ("--raw", "--raw"),
            ("--check", "--check"),
            ("--format", "--format=%H"),
            ("--pretty", "--pretty=oneline"),
        ];

        assert_eq!(
            cases.len(),
            PASSTHROUGH_FLAGS.len(),
            "test case count ({}) does not match PASSTHROUGH_FLAGS len ({}); \
             update this test when the constant changes",
            cases.len(),
            PASSTHROUGH_FLAGS.len()
        );

        for (flag_key, arg_value) in cases {
            let args: Vec<String> = vec![arg_value.to_string(), "HEAD".to_string()];
            assert!(
                user_has_flag(&args, PASSTHROUGH_FLAGS),
                "flag '{flag_key}' (arg '{arg_value}') must trigger passthrough via user_has_flag"
            );
        }
    }

    // ========================================================================
    // split_refpath — ref/path extraction
    // ========================================================================

    /// `split_refpath` must extract the path component from every `<ref>:<path>`
    /// shape that `git show` accepts, including edge cases that the inline `rfind`
    /// previously handled without test coverage.
    #[test]
    fn test_split_refpath_simple() {
        assert_eq!(split_refpath("HEAD:foo.rs"), "foo.rs");
    }

    #[test]
    fn test_split_refpath_empty_ref() {
        // `:foo.rs` — empty ref means the index (staging area).
        assert_eq!(split_refpath(":foo.rs"), "foo.rs");
    }

    #[test]
    fn test_split_refpath_slashes_in_ref() {
        assert_eq!(split_refpath("refs/heads/main:src/lib.rs"), "src/lib.rs");
    }

    #[test]
    fn test_split_refpath_colon_in_path() {
        // `abc:path/with:colon.rs` — splits at the LAST `:`, yielding `colon.rs`.
        // Git ref names cannot contain `:`, so the first colon is unambiguously the
        // ref/path separator.  Colons in file paths are uncommon on most OSes and
        // rfind still gives a safe result (the shortest unambiguous path suffix).
        assert_eq!(split_refpath("abc:path/with:colon.rs"), "colon.rs");
    }

    #[test]
    fn test_split_refpath_no_colon_returns_whole_token() {
        // Defensive fallback: no `:` → whole token returned.
        assert_eq!(split_refpath("HEAD"), "HEAD");
    }

    // ========================================================================
    // Tier-2 render_show_diff: unsupported extension falls back to raw hunks
    // ========================================================================

    /// When `render_show_diff` encounters a diff that contains only files with
    /// extensions that have no tree-sitter support, the rendered output still
    /// returns Some (the diff pipeline falls back to raw-hunk passthrough for
    /// those files) — confirming the Tier-2 path is reachable.
    #[test]
    fn test_render_show_diff_unsupported_extension_yields_some() {
        // Synthetic commit with a `.lock` file diff — no tree-sitter language.
        let raw = "commit deadbeef\n\
                   Author: Test <t@t.com>\n\
                   Date:   Thu Apr 10 2025\n\
                   \n\
                       chore: update lockfile\n\
                   \n\
                   diff --git a/Cargo.lock b/Cargo.lock\n\
                   index aaa..bbb 100644\n\
                   --- a/Cargo.lock\n\
                   +++ b/Cargo.lock\n\
                   @@ -1,2 +1,3 @@\n\
                    unchanged\n\
                   +added line\n\
                    unchanged\n";
        let result = render_show_diff(raw, &[], &[]);
        let result = result.expect("valid commit with unsupported-language diff must return Some");
        let rendered = result.to_string();
        // ShowCommitResult::render uses only the first 7 chars of the hash.
        assert!(
            rendered.contains("deadbee"),
            "commit hash (short) must appear in Tier-2 rendered output, got: {rendered}"
        );
        assert!(
            rendered.contains("chore: update lockfile"),
            "subject must appear in Tier-2 rendered output, got: {rendered}"
        );
    }

    // ========================================================================
    // AD-8: body and parents parsing tests
    // ========================================================================

    #[test]
    fn test_parse_commit_header_multi_paragraph_body() {
        let fixture = include_str!("../../../tests/fixtures/cmd/git/show_multi_paragraph.txt");
        let (header, _diff_body) =
            parse_commit_header(fixture).expect("multi-paragraph commit must parse");
        assert!(
            header.body.contains("paragraph 1"),
            "body must contain paragraph 1: {:?}",
            header.body
        );
        assert!(
            header.body.contains("paragraph 2"),
            "body must contain paragraph 2: {:?}",
            header.body
        );
        assert!(
            header.body.contains("paragraph 3"),
            "body must contain paragraph 3: {:?}",
            header.body
        );
    }

    #[test]
    fn test_parse_commit_header_merge_parents() {
        let fixture = include_str!("../../../tests/fixtures/cmd/git/show_merge.txt");
        let (header, _diff_body) = parse_commit_header(fixture).expect("merge commit must parse");
        assert_eq!(
            header.parents,
            Some("abc123 def456 fed321".to_string()),
            "octopus merge parents must be captured: {:?}",
            header.parents
        );
    }

    #[test]
    fn test_parse_commit_header_signed_commit() {
        let fixture = include_str!("../../../tests/fixtures/cmd/git/show_signed.txt");
        let (header, _diff_body) = parse_commit_header(fixture).expect("signed commit must parse");
        // Body should not contain PGP signature content.
        assert!(
            !header.body.contains("BEGIN PGP SIGNATURE"),
            "PGP signature block must be silently skipped: {:?}",
            header.body
        );
        assert!(
            !header.body.contains("END PGP SIGNATURE"),
            "PGP signature block end must be silently skipped: {:?}",
            header.body
        );
        // The actual commit body should be present.
        assert!(
            header.body.contains("This commit body should appear"),
            "commit body must be preserved in signed commit: {:?}",
            header.body
        );
    }

    #[test]
    fn test_parse_commit_header_empty_body() {
        let fixture = include_str!("../../../tests/fixtures/cmd/git/show_empty_body.txt");
        let (header, _diff_body) =
            parse_commit_header(fixture).expect("subject-only commit must parse");
        assert!(
            header.body.is_empty(),
            "subject-only commit must have empty body: {:?}",
            header.body
        );
        assert!(
            header.parents.is_none(),
            "non-merge commit must have no parents: {:?}",
            header.parents
        );
    }
}
