//! `skim git show` handler — commit and file-content compression (#132).
//!
//! Dispatches on argument shape:
//! - **File-content mode**: first non-flag token contains `:` → serves the blob
//!   verbatim, or a transformed view when the caller passed `--mode`.
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
//! File-content mode has no tier ladder on the default path: the blob is served
//! verbatim (ADR-022, below). The ladder exists only under an opt-in `--mode`:
//!   Tier 1: language supported → transform via rskim-core.
//!   Tier 2: unsupported language → raw.
//!   Tier 3: transform error, or guardrail fallback (transform did not shrink) → raw.
//!
//! # Design decisions
//!
//! **ADR-022 (2026-09-28)** — `<rev>:<path>` is a machine contract; `--mode` is
//! the opt-in view. **Supersedes `AD-GIT-SHOW-PSEUDO`.**
//!
//! `git show <rev>:<path>` is git's blob-extraction syntax and is contractually
//! the file's exact historical bytes, so [`run_show_file_content`] serves them
//! verbatim by default — no transform is attempted. The previous default routed
//! the blob through `Mode::Pseudo`, which measured 18 of 50 lines differing on a
//! TypeScript blob (`ok: boolean;` → `ok`, `const out: T[] = [];` →
//! `const out = []`): a lossy answer to a request for exact bytes is
//! indistinguishable from the file having been different.
//!
//! Three things move together, because any one alone leaves the defect reachable:
//!
//! 1. **The default is verbatim.** Blob extraction is a contract, not a view
//!    request — the same reasoning the shared contract-flag gate applies to
//!    `--porcelain`/`-z`/`--stat`, extended from flags to syntax.
//! 2. **`--mode=<m>` selects a view** ([`extract_show_mode_flag`],
//!    [`select_file_content_view`]). Before this it reached the child `git show`
//!    and returned `fatal: unrecognized argument: --mode=full`, exit 1, zero
//!    stdout — so the escape hatch the lossy default needed did not exist on
//!    this subcommand, though it works on `git diff`.
//! 3. **A lossy view discloses itself** (ADR-011 class 1, unconditional). No
//!    `lossy_view_marker` call existed anywhere under `cmd/git/` before this,
//!    although `process.rs` emits one for the identical transform on the file
//!    path, so the loss was silent even under `SKIM_DEBUG=1`.
//!
//! **AD-GIT-7** — Dispatch-on-arg-shape.
//!
//! The single entry point [`run_show`] inspects the first non-flag argument to
//! determine which of the three modes to enter (file-content, commit, multi-ref
//! passthrough). This avoids a separate subcommand (`show-file` / `show-commit`)
//! and mirrors `git show`'s own ambiguity resolution: the presence of `:` in a
//! token unambiguously signals a tree-object ref, while its absence means a
//! commit-ish. All other dispatch logic (passthrough flags, `--json` rejection,
//! annotated-tag detection) is layered on top of this primary shape test.
//!
//! **AD-GIT-8 (2026-04-11)** — Commit body and merge-parent preservation.
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

use rskim_core::{Language, Mode};

use crate::cmd::execution as exec;
use crate::cmd::{OutputFormat, extract_output_format, user_has_flag};
use crate::output::canonical::{DiffFileEntry, ShowCommitResult};
use crate::output::fidelity::Completeness;
use crate::runner::CommandRunner;

use rayon::prelude::*;

use super::diff::{
    DiffMode, MAX_AST_FILE_COUNT, PARALLEL_THRESHOLD, parse_unified_diff, render_diff_file,
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
// View selection (`--mode`) — ADR-022
// ============================================================================

/// The `--mode` vocabulary, for error messages.
///
/// Ordered with `full` first because that is the byte-faithful default's
/// explicit spelling and therefore the value a caller who hit a lossy view
/// wants, not because the list is alphabetical.
const SHOW_MODE_VALUES: &str = "full, minimal, pseudo, structure, signatures, types";

/// Parse a `--mode` value into an [`rskim_core::Mode`].
///
/// The vocabulary is delegated to [`Mode::parse`] rather than restated, so a
/// mode added to `rskim-core` needs no second table here to drift out of step.
///
/// This is deliberately NOT `diff/mod.rs`'s `parse_diff_mode_value`, which
/// answers a different question: `DiffMode` controls how *unchanged* AST context
/// is rendered around a hunk (`Default`/`Structure`/`Full`) and has no `pseudo`,
/// while a blob has no hunks and every `Mode` applies to it. Reusing that type
/// would have forced a mapping from a three-value enum onto a six-value one;
/// reusing the transform's own `Mode` needs no mapping at all.
fn parse_show_mode_value(val: &str) -> anyhow::Result<Mode> {
    Mode::parse(val)
        .ok_or_else(|| anyhow::anyhow!("unknown mode: '{val}'\nValid modes: {SHOW_MODE_VALUES}"))
}

/// Split `--mode <value>` / `--mode=<value>` off the argument list.
///
/// Returns `(git_args, mode)` where `git_args` has the flag removed, so it never
/// reaches the child `git show`. That leak was facet 3 of the ADR-022 defect:
/// `--mode=full` returned `fatal: unrecognized argument: --mode=full` and
/// `--mode full` returned `fatal: ambiguous argument 'full'`, both exit 1 with
/// zero stdout — so the flag did not silently no-op, it failed the command.
///
/// Nothing is stripped after a bare `--`: past the POSIX end-of-options marker
/// every token is a path filter, and a file literally named `--mode=full` must
/// reach git intact. This matches `dispatch::strip_skim_flags`, which documents
/// the same rule for every skim-owned flag.
///
/// The last occurrence wins when the flag is repeated, matching how a repeated
/// flag behaves everywhere else in skim.
fn extract_show_mode_flag(args: &[String]) -> anyhow::Result<(Vec<String>, Option<Mode>)> {
    let mut git_args: Vec<String> = Vec::with_capacity(args.len());
    let mut mode: Option<Mode> = None;
    let mut skip_next = false;
    let mut past_separator = false;

    for (i, arg) in args.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if past_separator {
            git_args.push(arg.clone());
            continue;
        }
        if arg == "--" {
            past_separator = true;
            git_args.push(arg.clone());
            continue;
        }
        if arg == "--mode" {
            let Some(val) = args.get(i + 1) else {
                return Err(anyhow::anyhow!(
                    "--mode requires a value\nValid modes: {SHOW_MODE_VALUES}"
                ));
            };
            mode = Some(parse_show_mode_value(val)?);
            skip_next = true;
            continue;
        }
        if let Some(val) = arg.strip_prefix("--mode=") {
            mode = Some(parse_show_mode_value(val)?);
            continue;
        }
        git_args.push(arg.clone());
    }

    Ok((git_args, mode))
}

/// Which view `git show <rev>:<path>` serves.
///
/// The type exists so the choice is a *value* a test can assert on. The
/// observable alone cannot carry it: under a guardrail fallback stdout is the
/// source file, and the source file contains the very annotations a lossy view
/// would have stripped — so "stdout contains `id: UserId;`" passes against a
/// completely unfixed binary (PF-025). `Verbatim` is not reachable at all
/// without the fix, which is what makes it a real precondition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileContentView {
    /// Serve git's bytes verbatim. No transform is attempted, so no mode,
    /// language, guardrail or marker is in play.
    Verbatim,
    /// Serve the view the caller opted into with `--mode`.
    Transformed(Mode),
}

/// Map the caller's `--mode` (or its absence) to a view — ADR-022.
///
/// - No `--mode` → [`FileContentView::Verbatim`]. Blob extraction is a contract.
/// - `--mode=full` → [`FileContentView::Verbatim`] as well, and via the *same*
///   branch rather than through `rskim_core::transform` with [`Mode::Full`].
///   `Mode::Full` is defined as "no transformation — return original source", so
///   serving the blob verbatim honours it exactly; routing it through the
///   transform would make byte-faithfulness contingent on tree-sitter parsing
///   the blob and on the guardrail's tie rule electing raw, when neither has any
///   say in what `full` means. It also buys the caller a documented escape from
///   a lossy view they asked for by mistake.
/// - Any other mode → [`FileContentView::Transformed`], which transforms AND
///   discloses (ADR-011 class 1).
fn select_file_content_view(mode: Option<Mode>) -> FileContentView {
    match mode {
        None | Some(Mode::Full) => FileContentView::Verbatim,
        Some(m) => FileContentView::Transformed(m),
    }
}

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
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_show_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Passthrough for stat-family and format flags.
    if user_has_flag(args, PASSTHROUGH_FLAGS) {
        return run_passthrough(global_flags, "show", args, show_stats, rec);
    }

    match detect_show_mode(args) {
        ShowMode::MultiRef => run_passthrough(global_flags, "show", args, show_stats, rec),
        ShowMode::FileContent { refpath } => {
            run_show_file_content(global_flags, args, &refpath, show_stats, rec)
        }
        ShowMode::Commit => {
            let (git_args, output_format) = extract_output_format(args);
            run_show_commit(
                global_flags,
                &git_args,
                args,
                output_format,
                show_stats,
                rec,
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
    /// # AD-GIT-8 (2026-04-11)
    /// Multi-paragraph bodies are preserved verbatim with 4-space indent stripped.
    /// Empty when the commit has only a subject line.
    body: String,
    /// Tail of a `Merge: ` header line, when present (e.g. `"abc123 def456"`).
    ///
    /// # AD-GIT-8 (2026-04-11)
    /// Stored as a structured field rather than inlined into `body` so that
    /// `ShowCommitResult::render` can emit `Merge: {parents}` as a dedicated
    /// prefix line. Octopus merges have all parent hashes in one space-separated
    /// string — the tail is stored unchanged.
    parents: Option<String>,
}

/// Parse phase for [`parse_header_lines`].
///
/// Replaces the `in_body: bool` + `subject_captured: bool` pair. The two
/// booleans encoded three sequential, non-overlapping states that only ever
/// advanced in one direction (Headers → AwaitingSubject → Body). An enum
/// makes all three phases explicit and eliminates the impossible state where
/// `in_body == false` but `subject_captured == true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommitParsePhase {
    /// Parsing git trailer lines (`commit`, `Author`, `Date`, `Merge`).
    Headers,
    /// Blank separator consumed; waiting for the first non-blank body line
    /// (the commit subject).
    AwaitingSubject,
    /// Subject captured; accumulating remaining body lines.
    Body,
}

/// Walk `header_region` lines and populate `header` with extracted fields.
///
/// Returns the accumulated body lines as `Vec<&str>` slices borrowed from
/// `header_region`, avoiding per-line allocations (zero-copy body accumulation).
///
/// # State machine
/// - `CommitParsePhase::Headers`: parse git trailer lines (`commit `,
///   `Author: `, `Date: `, `Merge: `). All other lines (gpgsig, mergetag,
///   continuation lines starting with a space) are silently skipped per AD-GIT-8.
/// - `CommitParsePhase::AwaitingSubject`: capture the subject (first non-blank
///   body line), stripping the canonical 4-space indent.
/// - `CommitParsePhase::Body`: accumulate body lines, stripping 4-space indent.
fn parse_header_lines<'a>(header_region: &'a str, header: &mut CommitHeader) -> Vec<&'a str> {
    // Extract the trimmed value from a `Key: value` header line.
    let header_value = |line: &str, prefix: &str| -> String {
        line.strip_prefix(prefix)
            .unwrap_or_default()
            .trim()
            .to_string()
    };

    let mut phase = CommitParsePhase::Headers;
    let mut body_lines: Vec<&'a str> = Vec::new();

    for line in header_region.lines() {
        match phase {
            CommitParsePhase::Headers => {
                if line.starts_with("commit ") {
                    header.hash = header_value(line, "commit ");
                } else if line.starts_with("Merge: ") {
                    // AD-GIT-8: capture merge parents as structured field.
                    header.parents = Some(header_value(line, "Merge: "));
                } else if line.starts_with("Author: ") {
                    header.author = header_value(line, "Author: ");
                } else if line.starts_with("Date: ") {
                    header.date = header_value(line, "Date: ");
                } else if line.is_empty() && !header.hash.is_empty() {
                    phase = CommitParsePhase::AwaitingSubject;
                }
            }
            CommitParsePhase::AwaitingSubject => {
                // First non-blank line is the subject.
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    header.subject = line.strip_prefix("    ").unwrap_or(trimmed).to_string();
                    phase = CommitParsePhase::Body;
                }
            }
            CommitParsePhase::Body => {
                // Borrow each body line slice — no allocation per line.
                body_lines.push(line.strip_prefix("    ").unwrap_or(line));
            }
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
/// # Signature blocks (AD-GIT-8)
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
    /// The downstream reader closed the pipe while the failure streams were
    /// being forwarded.  The caller must stop and return `pipe_closed_exit()`
    /// without recording analytics, matching every other pipe-closed path.
    PipeClosed,
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

    let runner = CommandRunner::new();
    let output = runner.run("git", &as_str_slice(&full_args))?;

    if output.exit_code != Some(0) {
        if !output.stderr.is_empty()
            && exec::write_to_stderr(&output.stderr)? == exec::StdoutStatus::PipeClosed
        {
            return Ok(ShowRawOutcome::PipeClosed);
        }
        if !output.stdout.is_empty()
            && exec::write_to_stdout(&output.stdout)? == exec::StdoutStatus::PipeClosed
        {
            return Ok(ShowRawOutcome::PipeClosed);
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
            true, // is_show: source must be read from the commit, not the working tree
        );
        // D3 (issue #510): carry raw hunk content so --json consumers get the
        // full patch body, mirroring render_and_format in diff/mod.rs.  This is
        // what lets `emit_show_commit` declare [`Completeness::Reencoded`] — all
        // content is faithfully represented in a different encoding, so ADR-011
        // class-1 disclosure is not owed.
        let patch = {
            use std::fmt::Write as _;
            let mut buf = String::new();
            for hunk in &fd.hunks {
                let _ = writeln!(
                    buf,
                    "@@ -{},{} +{},{} @@",
                    hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
                );
                for line in &hunk.patch_lines {
                    buf.push_str(line);
                    buf.push('\n');
                }
            }
            if buf.is_empty() { None } else { Some(buf) }
        };
        let entry = DiffFileEntry {
            path: fd.path.clone(),
            status: fd.status.clone(),
            changed_regions: fd.hunks.len(),
            patch,
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
    rec: crate::analytics::RecordingContext<'_>,
    duration: std::time::Duration,
) -> anyhow::Result<exec::StdoutStatus> {
    let rec_full = rec.with_tier("full");
    match output_format {
        OutputFormat::Json => {
            // JSON: serialise result directly; guardrail is irrelevant here
            // because the JSON output is never substituted for raw text.
            // Running the guardrail on JSON would double the memory cost and
            // could spuriously emit `[skim:guardrail]` to stderr.
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| anyhow::anyhow!("failed to serialize show result: {e}"))?;
            // ADR-011 / D1 declaration — derived, not hard-coded.
            //
            // Header fields (hash, author, date, subject, body, parents) and
            // every hunk body (`DiffFileEntry::patch`, D3 / #510) are carried
            // when the commit touches text files only.  In that case no
            // disclosure is owed (`Reencoded`).
            //
            // What this envelope does NOT reproduce is `gpgsig`/`mergetag` header
            // lines (per AD-GIT-8 these are implementation artefacts, not user
            // content) plus the extended diff headers noted in `diff/mod.rs`.
            // `Notes:` blocks ARE captured: `parse_header_lines` enters `Body`
            // phase after the subject line and accumulates all remaining lines in
            // `header_region` — including any `Notes:` block — so they appear in
            // `CommitHeader::body` and are serialised.
            //
            // Binary files, 100%-similarity renames, and `old mode`/`new mode`-only
            // changes produce no hunk content, so `patch` is `None` — a real
            // information drop that requires `Lossy` and an ADR-011 class-1 marker.
            let completeness = if result.files.iter().all(|f| f.patch.is_some()) {
                Completeness::Reencoded
            } else {
                Completeness::Lossy
            };
            if exec::emit_json_envelope(
                &json,
                completeness,
                "git",
                None,
                exec::LineTermination::Newline,
            )? == exec::StdoutStatus::PipeClosed
            {
                return Ok(exec::StdoutStatus::PipeClosed);
            }
            finalize_git_output_owned(raw, json, label, show_stats, rec_full, duration);
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
            let raw_for_record = if show_stats || rec.enabled {
                raw.clone()
            } else {
                String::new()
            };
            let guardrail = crate::output::guardrail::apply_to_stderr(raw, result_str)?;
            let final_output = guardrail.into_output();
            if exec::write_to_stdout(&final_output)? == exec::StdoutStatus::PipeClosed {
                return Ok(exec::StdoutStatus::PipeClosed);
            }
            finalize_git_output_owned(
                raw_for_record,
                final_output,
                label,
                show_stats,
                rec_full,
                duration,
            );
        }
    }
    Ok(exec::StdoutStatus::Written)
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
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    let (raw, duration) = match run_git_show_raw(global_flags, git_args)? {
        ShowRawOutcome::Success { stdout, duration } => (stdout, duration),
        ShowRawOutcome::PipeClosed => return Ok(exec::pipe_closed_exit()),
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
                build_analytics_label("show", original_args, show_stats, rec.enabled),
                show_stats,
                rec.with_tier("passthrough"),
                duration,
            );
            return Ok(exit_code);
        }
    };

    // Built before the `render_show_diff` check so both the passthrough and
    // the normal path share the same label (HIGH-3).  Derived from *original*
    // args (before `--json` extraction) so the DB records the full invocation.
    let label = build_analytics_label("show", original_args, show_stats, rec.enabled);

    let Some(result) = render_show_diff(&raw, global_flags, git_args) else {
        // Not a regular commit (annotated tag, blob, tree, etc.) — passthrough.
        // Route through finalize so the analytics DB records a zero-compression
        // entry instead of silently dropping the invocation (HIGH-3).
        // raw == output; move raw into the passthrough variant: 1 allocation
        // (clone) on the analytics path, 0 when disabled (PF-018 resolution).
        if exec::write_to_stdout(&raw)? == exec::StdoutStatus::PipeClosed {
            return Ok(exec::pipe_closed_exit());
        }
        super::finalize_git_output_passthrough(
            raw,
            label,
            show_stats,
            rec.with_tier("passthrough"),
            duration,
        );
        return Ok(ExitCode::SUCCESS);
    };

    if emit_show_commit(result, raw, label, output_format, show_stats, rec, duration)?
        == exec::StdoutStatus::PipeClosed
    {
        return Ok(exec::pipe_closed_exit());
    }
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// File-content mode
// ============================================================================

/// Why [`run_show_file_content`] is serving git's bytes verbatim.
///
/// Replaces a bare `tier: u8`. One number decided both the analytics tier and
/// the debug banner, so the two could not be worded independently — and ADR-022
/// adds a path that shares the tier but is emphatically *not* a fallback, so
/// they now have to be. The `u8` also needed a `_ => None` arm for values the
/// type permitted and no call site produced; this enum has no such arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawReason {
    /// ADR-022 contract path: `<rev>:<path>` is git's blob-extraction syntax,
    /// so the default — and its explicit spelling `--mode=full` — serve the
    /// file's exact historical bytes. Nothing was attempted and nothing failed.
    BlobContract,
    /// No supported language for the path's extension, or a serde-based one.
    UnsupportedLanguage,
    /// `rskim_core::transform` returned an error under an opt-in `--mode`.
    TransformFailed,
}

impl RawReason {
    /// Canonical analytics `parse_tier` spelling.
    ///
    /// These values are unchanged from the `u8` table they replace, so no
    /// recorded row's tier moves: unsupported-language was `2` → `"degraded"`
    /// and transform-failure was `3` → `"passthrough"`. `BlobContract` joins
    /// `"passthrough"` because raw == output there too, which is also what
    /// `exec::emit_raw_passthrough` records for every other subcommand.
    fn tier(self) -> &'static str {
        match self {
            Self::UnsupportedLanguage => "degraded",
            Self::BlobContract | Self::TransformFailed => "passthrough",
        }
    }

    /// The `SKIM_DEBUG`-gated banner for this path.
    ///
    /// ADR-011 class 2: every arm here serves raw bytes, so the reader sees
    /// nothing less than the tool would have shown and the notice is pure
    /// context tax by default. `BlobContract` gets its own wording because
    /// calling the contract default a "fallback" would misdescribe it — the
    /// class-2/class-1 split is about loss, and this text is what someone
    /// diagnosing "why did skim not compress this?" reads.
    fn banner(self) -> &'static str {
        match self {
            Self::BlobContract => {
                "[skim] git show: <rev>:<path> is blob extraction — serving git's bytes verbatim \
                 (ADR-022); pass --mode=<structure|pseudo|…> for a compressed view"
            }
            Self::UnsupportedLanguage => {
                "[skim] git show: falling back to raw (no supported language for this path)"
            }
            Self::TransformFailed => "[skim] git show: falling back to raw (transform failed)",
        }
    }
}

/// Emit raw `git show` output unchanged and record analytics/stats.
///
/// Three paths share it, all of them emitting git's bytes byte-for-byte:
/// the ADR-022 blob contract (the default), an unsupported extension, and a
/// transform error under an opt-in `--mode`. The guardrail-fallback case does
/// NOT call this function; the guardrail's `into_output()` returns the raw
/// string directly and the Tier-1 `finalize_git_output` call records the
/// zero-compression result inline (MEDIUM-23).
///
/// Centralising them here ensures consistent analytics accounting: raw == output
/// so the DB records zero compression gain, matching the behaviour of
/// `run_passthrough` for other subcommands.
///
/// Byte-faithfulness is a property of the sink, not of this function's
/// intentions: `exec::write_to_stdout` passes `ensure_trailing_newline: false`,
/// so a blob with no final newline does not acquire one. Do not reroute this to
/// `exec::emit_raw_passthrough`, whose guard appends one.
///
/// `label` is a pre-built String passed from [`run_show_file_content`].  It is
/// computed via a guarded `if/else` that returns `String::new()` when neither
/// `--show-stats` nor analytics are enabled, so each branch allocates at most
/// one String — allocation is deferred to branch-time, not cached (MEDIUM-24).
///
/// The banner ([`RawReason::banner`]) reaches stderr only when debug output is
/// enabled (`--debug` / `SKIM_DEBUG=1`). On the default silent path the
/// passthrough still fires and analytics are recorded — only the informational
/// banner is suppressed, because no-loss notices are debug-gated (ADR-011
/// class 2).
fn passthrough_file_content(
    raw: String,
    label: String,
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
    duration: std::time::Duration,
    reason: RawReason,
) -> anyhow::Result<exec::StdoutStatus> {
    crate::debug_log!("{}", reason.banner());
    if exec::write_to_stdout(&raw)? == exec::StdoutStatus::PipeClosed {
        return Ok(exec::StdoutStatus::PipeClosed);
    }
    // raw == output (passthrough); move raw into finalize_git_output_passthrough
    // so the analytics path clones once and moves once — 1 allocation total
    // instead of 2 (PF-018 resolution).
    super::finalize_git_output_passthrough(
        raw,
        label,
        show_stats,
        rec.with_tier(reason.tier()),
        duration,
    );
    Ok(exec::StdoutStatus::Written)
}

/// Run `git show <ref>:<path>` in file-content mode.
///
/// Dispatch, in the order the checks fire:
///   `--json`               → exit 2 (unsupported; a blob has no JSON encoding).
///   a bad `--mode` value   → `Err` (exit 1, message names the vocabulary).
///   no `--mode`/`=full`    → blob served verbatim (ADR-022). No transform is
///                            attempted, so no language detection, no guardrail
///                            and no marker.
///   `--mode=<m>`, Tier 1   → transform via rskim-core + guardrail, and a
///                            class-1 marker when the transformed view is the
///                            one served.
///   `--mode=<m>`, Tier 2   → unsupported or serde-based extension → raw.
///   `--mode=<m>`, Tier 3   → transform error → raw. The guardrail-fallback
///                            sub-case also serves raw, recorded inline by the
///                            Tier-1 `finalize_git_output` call rather than via
///                            [`passthrough_file_content`].
fn run_show_file_content(
    global_flags: &[String],
    args: &[String],
    refpath: &str,
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    // --json is not meaningful for file-content mode.
    if user_has_flag(args, &["--json"]) {
        eprintln!(
            "Error: --json is not supported for `git show <ref>:<path>` (file-content mode); \
             blob extraction answers with the file's bytes, which have no JSON encoding"
        );
        return Ok(ExitCode::from(2));
    }

    // ADR-022 part 2: take `--mode` out of the argv before git sees it. Left in,
    // it reaches the child and hard-errors (`fatal: unrecognized argument`).
    let (git_args, requested_mode) = extract_show_mode_flag(args)?;
    let view = select_file_content_view(requested_mode);

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.push("show".to_string());
    // --no-color matches commit-mode's run_git_show_raw: prevents ANSI escapes
    // from user configs that set `color.ui = always` (MEDIUM-17).  It is
    // byte-neutral for blob extraction — `git show --no-color <rev>:<path>` is
    // byte-identical to both `git show <rev>:<path>` and
    // `git cat-file blob <rev>:<path>`, measured — so it does not compromise the
    // ADR-022 verbatim contract.
    full_args.push("--no-color".to_string());
    // Every other argument reaches git unchanged; only `--mode` is skim-owned.
    full_args.extend_from_slice(&git_args);

    let runner = CommandRunner::new();
    let output = runner.run("git", &as_str_slice(&full_args))?;

    if output.exit_code != Some(0) {
        if !output.stderr.is_empty()
            && exec::write_to_stderr(&output.stderr)? == exec::StdoutStatus::PipeClosed
        {
            return Ok(exec::pipe_closed_exit());
        }
        if !output.stdout.is_empty()
            && exec::write_to_stdout(&output.stdout)? == exec::StdoutStatus::PipeClosed
        {
            return Ok(exec::pipe_closed_exit());
        }
        let exit_code = output.exit_code;
        // Record analytics on the error path so the DB reflects failed
        // invocations (e.g. `git show HEAD:missing.rs`). Move stdout: 1
        // allocation (clone) on the analytics path, 0 when disabled (PF-018).
        super::finalize_git_output_passthrough(
            output.stdout,
            build_analytics_label("show", args, show_stats, rec.enabled),
            show_stats,
            rec.with_tier("passthrough"),
            output.duration,
        );
        return Ok(map_exit_code(exit_code));
    }

    let raw = output.stdout;
    let duration = output.duration;

    let label = build_analytics_label("show", args, show_stats, rec.enabled);

    // ADR-022: `<rev>:<path>` is git's blob-extraction syntax, and its contract
    // is the file's exact historical bytes. Serve them, and attempt nothing —
    // this returns before language detection, the transform and the guardrail,
    // so there is no path on which a byte can move. A transform reached only by
    // an explicit `--mode` is a view the caller asked for; one reached by
    // default is an answer to a question nobody asked.
    let FileContentView::Transformed(mode) = view else {
        // Move raw: the branch always returns, so Rust knows raw is still
        // available below for the transformed path.
        if passthrough_file_content(
            raw,
            label,
            show_stats,
            rec,
            duration,
            RawReason::BlobContract,
        )? == exec::StdoutStatus::PipeClosed
        {
            return Ok(exec::pipe_closed_exit());
        }
        return Ok(ExitCode::SUCCESS);
    };

    // Extract the path component from `<ref>:<path>` (everything after the last `:`).
    // Git disallows `:` inside ref names, so any `:` in the token is a ref/path separator.
    let path_str = split_refpath(refpath);

    // Detect language from path extension.
    let lang = Language::from_path(Path::new(path_str)).filter(|l| !l.is_serde_based());

    let Some(lang) = lang else {
        // Tier 2: unsupported or serde-based language — passthrough.
        // Move raw: the else branch always returns, so Rust knows raw is
        // available after the let-else for the Tier 1 path.
        if passthrough_file_content(
            raw,
            label,
            show_stats,
            rec,
            duration,
            RawReason::UnsupportedLanguage,
        )? == exec::StdoutStatus::PipeClosed
        {
            return Ok(exec::pipe_closed_exit());
        }
        return Ok(ExitCode::SUCCESS);
    };

    // Tier 1: transform in memory, in the mode the caller named.
    let transformed = match rskim_core::transform(&raw, lang, mode) {
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
            if passthrough_file_content(
                raw,
                label,
                show_stats,
                rec,
                duration,
                RawReason::TransformFailed,
            )? == exec::StdoutStatus::PipeClosed
            {
                return Ok(exec::pipe_closed_exit());
            }
            return Ok(ExitCode::SUCCESS);
        }
    };

    // Guardrail: if the transform did not shrink the output, emit raw.
    // Clone raw only here (Tier 1 success path), not on every branch (MEDIUM-18).
    // `apply_to_stderr` takes ownership of raw; clone it first so we can pass
    // the original into `finalize_git_output_owned` without a second allocation.
    let raw_for_record = if show_stats || rec.enabled {
        raw.clone()
    } else {
        String::new()
    };
    // The UNCHARGED shim, deliberately. `apply_to_stderr_with_notice` would
    // price the marker below into this verdict, and ADR-011's 2026-09-28
    // amendment defers that refinement: it is an ADR-001/ADR-003 budget
    // conversation, not a line change, and every other call site under
    // `cmd/git/` is on the uncharged shim too. The consequence is stated rather
    // than hidden: a `--mode` view that saves less than the marker costs is
    // still served, and is then a net expansion. That is tolerable here in a way
    // it was not before, because reaching this line at all now requires the
    // caller to have typed `--mode`.
    let guardrail = crate::output::guardrail::apply_to_stderr(raw, transformed)?;
    // Captured before `into_output` consumes the outcome. `Triggered` means the
    // guard served raw, so the reader lost nothing and is owed no marker; the
    // `Passed` branch is strictly-smaller-than-raw and therefore necessarily
    // differs from it.
    let served_transformed = !guardrail.was_triggered();
    let final_output = guardrail.into_output();

    if exec::write_to_stdout(&final_output)? == exec::StdoutStatus::PipeClosed {
        return Ok(exec::pipe_closed_exit());
    }

    // ADR-011 class 1 — UNCONDITIONAL, never gated behind SKIM_DEBUG. The
    // reader is seeing something different from the file's bytes and this line
    // is the only signal of it. Shape and wording mirror
    // `process.rs::write_result_and_stats`, which emits the same marker for the
    // same transform on the file-read path; before ADR-022 no `lossy_view_marker`
    // call existed anywhere under `cmd/git/`, which is why the identical loss was
    // silent here and disclosed there.
    //
    // `origin: None` — the direct-invocation form — is correct and is not an
    // omission of `crate::output::rewrite_origin()`. That vocabulary is
    // `cat`/`head`/`tail`, and no hook rewrite produces a `git show`; reading a
    // stale `SKIM_REWRITTEN_FROM` out of the environment here would render
    // `transformed view (cat → skim --mode=pseudo)` for a command the caller
    // typed themselves. The remedy the marker prints is literally reachable:
    // `SKIM_PASSTHROUGH=1 skim git show <rev>:<path>` is honoured by the
    // convergence gate in `cmd/dispatch.rs` and returns git's bytes (measured).
    if let Some(marker) =
        crate::output::lossy_view_marker(None, mode.name(), usize::from(served_transformed), 1)
    {
        // `let _ =` on the panic-free sink, mirroring the class-1 JSON marker at
        // `cmd/execution.rs:448` and the build-family notice at
        // `cmd/build/mod.rs:411`. Two properties are deliberate: `eprintln!`
        // panics on a closed stderr and would turn a disclosure into exit 101,
        // and a stderr that cannot be written is not a reason to fail a run
        // whose stdout already succeeded — there is simply no reader left to
        // disclose to.
        let _ = exec::write_line_to_stderr(&marker);
    }

    // Both raw_for_record and final_output are owned Strings; use the owned
    // variant to move them directly into analytics, avoiding two extra .to_string()
    // clones that the borrowed finalize_git_output would incur (HIGH-3, PF-018).
    finalize_git_output_owned(
        raw_for_record,
        final_output,
        label,
        show_stats,
        rec.with_tier("full"),
        duration,
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
    println!("    File mode     : show the file's exact bytes at a ref, or a");
    println!("                    transformed view when --mode is given");
    println!();
    println!("OPTIONS:");
    println!("    --json           Machine-readable JSON output (commit mode only)");
    println!("    --mode <MODE>    File mode only: transform the blob instead of");
    println!("                     serving its bytes.  One of:");
    println!("                       full        the bytes, unchanged (the default)");
    println!("                       minimal     drop non-doc comments");
    println!("                       pseudo      drop comments and syntax noise");
    println!("                       structure   signatures only, bodies removed");
    println!("                       signatures  callable signatures only");
    println!("                       types       type definitions only");
    println!("    --show-stats     Show token savings statistics");
    println!();
    println!("PASSTHROUGH FLAGS (no compression):");
    println!("    --stat, --shortstat, --numstat, --name-only, --name-status");
    println!("    --raw, --check, --format, --pretty");
    println!();
    println!("NOTES:");
    println!("    <ref>:<path> is git's blob-extraction syntax, so by default it");
    println!("    serves the file's exact historical bytes — no transform.  A");
    println!("    lossy --mode announces itself on stderr (ADR-022).");
    println!("    --json is not supported in file-content mode (<ref>:<path>).");
    println!("    Passing --json with a file-content ref exits with code 2.");
    println!();
    println!("EXAMPLES:");
    println!("    skim git show HEAD");
    println!("    skim git show HEAD:src/main.rs    # exact bytes of the blob");
    println!("    skim git show --mode=structure HEAD:src/main.rs   # opt-in view");
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

    // `TransformConfig` is no longer a production import: ADR-022 removed the
    // hardcoded `TransformConfig::with_mode(Mode::Pseudo)` and the transform
    // takes a `Mode` directly.  Only tests still reach for the config type.
    use rskim_core::TransformConfig;

    /// `Vec<String>` from a slice of `&str`, for argv fixtures.
    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

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
            let rendered = render_diff_file(fd, &[], &[], DiffMode::Default, i >= 200, false);
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
        let rec = crate::analytics::RecordingContext {
            enabled: false,
            command_type: crate::analytics::CommandType::Git,
            parse_tier: None,
            session_id: None,
        };
        let result = run_show_file_content(&global_flags, &args, "HEAD:src/main.rs", false, rec)
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
    // AD-GIT-8: body and parents parsing tests
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

    // ========================================================================
    // ADR-022: `<rev>:<path>` is blob extraction — verbatim by default,
    // `--mode` is the opt-in view.  SUPERSEDES `AD-GIT-SHOW-PSEUDO` ("Fix D").
    //
    // The three tests below are the amended `test_fix_d_*` trio that pinned the
    // reversed behaviour.  Each pins the successor of the contract it used to
    // pin; none of them was left in place beside a new one, because two tests
    // asserting opposite defaults is how a reverted reversal ships green.
    // ========================================================================

    /// There is NO default transform mode on the file-content path.
    ///
    /// This slot used to assert the hardcoded
    /// `TransformConfig::with_mode(Mode::Pseudo)` that sat in
    /// [`run_show_file_content`] — `AD-GIT-SHOW-PSEUDO`'s mode constant. ADR-022
    /// deletes that constant: `<rev>:<path>` is git's blob-extraction syntax and
    /// its contract is the file's exact historical bytes, so the default serves
    /// them and attempts nothing.
    ///
    /// What is pinned now is the ABSENCE of a default: with no `--mode`,
    /// [`select_file_content_view`] must answer [`FileContentView::Verbatim`].
    /// Any regression that reinstates a default transform — in any mode, not
    /// just `Pseudo` — makes this arm return `Transformed(_)` and fails here.
    #[test]
    fn test_blob_default_selects_the_verbatim_view_not_a_transform() {
        assert_eq!(
            select_file_content_view(None),
            FileContentView::Verbatim,
            "ADR-022: with no --mode, `git show <rev>:<path>` must serve the blob \
             verbatim; a default transform is exactly the AD-GIT-SHOW-PSEUDO \
             behaviour this entry reverses"
        );

        // Discrimination check, and the reason `TransformConfig` survives in
        // this module at all: its default is a TRANSFORMING mode, so a future
        // author who reaches for the config type to supply the default would
        // reintroduce a lossy blob view.  The production path no longer
        // constructs a `TransformConfig` on any branch.
        assert_eq!(
            TransformConfig::default().mode,
            Mode::Structure,
            "TransformConfig::default() is Structure — a transforming mode.  It \
             can therefore never be the source of the verbatim default, which is \
             why `run_show_file_content` does not consult it"
        );
    }

    /// The default view loses nothing, and a view is lossy only when NAMED.
    ///
    /// This slot used to assert that the default mode preserved function bodies
    /// — the argument `AD-GIT-SHOW-PSEUDO` made for choosing `Pseudo` over
    /// `Structure`. ADR-022 reaches the same end by a stronger route: bodies
    /// reach the reader because the whole blob reaches the reader, byte for
    /// byte, so no mode has to be chosen for its generosity.
    ///
    /// Pinned as the complete mapping table [`select_file_content_view`] owns.
    /// Enumerating every `Mode` rather than spot-checking one is deliberate: the
    /// defect being reversed was a mode reaching this path that nobody named, so
    /// the interesting property is which modes are reachable WITHOUT a `--mode`
    /// — exactly one, and it is the identity.
    #[test]
    fn test_blob_default_loses_nothing_and_only_a_named_mode_transforms() {
        // The two spellings of "serve the bytes": absence, and `full`.
        for (requested, label) in [(None, "no --mode"), (Some(Mode::Full), "--mode=full")] {
            assert_eq!(
                select_file_content_view(requested),
                FileContentView::Verbatim,
                "{label} must serve the blob verbatim (ADR-022)"
            );
        }

        // Every remaining mode is reachable ONLY by being named, and each maps
        // to itself — no mode is silently substituted for another.
        for m in [
            Mode::Minimal,
            Mode::Pseudo,
            Mode::Structure,
            Mode::Signatures,
            Mode::Types,
        ] {
            assert_eq!(
                select_file_content_view(Some(m)),
                FileContentView::Transformed(m),
                "--mode={} must select that exact transform, and only because the \
                 caller named it",
                m.name()
            );
        }
    }

    /// The opt-in modes really do remove content, so neither `--mode` nor its
    /// marker is vacuous.
    ///
    /// This slot used to justify `AD-GIT-SHOW-PSEUDO`'s choice of `Pseudo` over
    /// `Structure` by showing the two differ at the `rskim_core::transform`
    /// layer. That discrimination is still load-bearing, for two new reasons:
    ///
    /// 1. A `--mode` vocabulary whose values produced the same bytes would be a
    ///    flag with nothing behind it.
    /// 2. It supplies the KNOWN-LOSSY input that
    ///    [`test_lossy_mode_emits_the_class_one_marker_on_stderr`] needs. PF-025
    ///    rule 1: an assertion about disclosure has to be made against content
    ///    that is actually lost, or it passes for the wrong reason.
    ///
    /// The tokens below live ONLY inside function bodies in the fixture (not in
    /// any signature, doc comment, `use`, or struct field), so Structure — which
    /// keeps signatures and imports — must drop every one of them.
    ///
    /// Note what this does NOT pin, so it is not mistaken for a guard it is not:
    /// it calls `transform` with explicit modes and says nothing about which
    /// mode the production path selects. That is
    /// [`test_blob_default_selects_the_verbatim_view_not_a_transform`]'s job.
    #[test]
    fn test_opt_in_modes_really_transform_so_the_flag_is_not_vacuous() {
        let source = include_str!("../../../tests/fixtures/cmd/git/show_file.rs");
        let lang = Language::from_path(std::path::Path::new("show_file.rs"))
            .expect("show_file.rs must be detected as Rust");

        let structure_out = rskim_core::transform(source, lang, Mode::Structure)
            .expect("Structure transform must succeed");
        let pseudo_out = rskim_core::transform(source, lang, Mode::Pseudo)
            .expect("Pseudo transform must succeed");

        // Body-only tokens: a method call, an error string, and a stdlib call
        // that each appear solely inside a collapsed function body.
        for t in [
            "find_user_by_username",
            "Invalid credentials",
            "duration_since",
        ] {
            assert!(
                pseudo_out.contains(t),
                "--mode=pseudo must retain body token {t:?}, which is what \
                 distinguishes it from --mode=structure in the flag's \
                 vocabulary; got: {pseudo_out:?}"
            );
            assert!(
                !structure_out.contains(t),
                "--mode=structure must strip body token {t:?}; two modes that \
                 produced the same bytes would make --mode a flag with nothing \
                 behind it; got: {structure_out:?}"
            );
        }

        // Both named modes are LOSSY against the blob, which is what makes the
        // ADR-011 class-1 marker owed at all.  A mode whose output equalled the
        // source would owe nothing and would make a disclosure test green for
        // the wrong reason.
        for (out, m) in [
            (&pseudo_out, Mode::Pseudo),
            (&structure_out, Mode::Structure),
        ] {
            assert_ne!(
                out.as_str(),
                source,
                "--mode={} must differ from the blob, or the class-1 marker \
                 asserted elsewhere would be disclosing nothing",
                m.name()
            );
        }
    }

    // ========================================================================
    // ADR-022 regression tests — one per measured facet
    //
    // Facet 1 (silent lossy transform): the default serves the blob's bytes.
    // Facet 2 (stderr completely empty): a lossy `--mode` discloses itself.
    // Facet 3 (`--mode` exits 1 via git): the flag is skim-owned and parsed.
    //
    // Home: in-file `#[cfg(test)]`, run via `-p rskim --bins`.  `rskim` is
    // bin-only, `run_show_file_content` writes straight to the process's stdout
    // and returns only an `ExitCode`, so no test in any layer can capture the
    // served bytes from it.  What CAN be pinned exactly is (a) the byte identity
    // of the two git invocations, measured against a hermetic repo, and (b) the
    // view-selection decision, which is a pure function of the argv.  Facet 1's
    // remaining link — that the `Verbatim` branch hands `raw` to
    // `exec::write_to_stdout`, whose `ensure_trailing_newline: false` adds no
    // byte — is structural: there is no transform between the two, and no
    // fixture can make one appear.
    // ========================================================================

    /// The TypeScript blob the ADR-022 tests commit and read back.
    ///
    /// Two properties are load-bearing and neither is incidental:
    ///
    /// - **No trailing newline.** The byte comparison would not notice an
    ///   appended `\n` on a file that already ends in one, and appending one is
    ///   a live defect class in this tree (`emit_raw_passthrough`'s guard does
    ///   exactly that, which is why [`passthrough_file_content`] must keep using
    ///   `exec::write_to_stdout`).
    /// - **Enough annotation to be worth stripping.** Measured on the uncharged
    ///   git-show path at `c2b4378` — **before F1/F1b**: 442 B → 338 B under
    ///   `Mode::Pseudo`, 104 B of headroom over the guard, with `id: UserId;`
    ///   rendering as `id`.  Both of those are now stale.  F1/F1b restored the
    ///   type-level member annotations and the `;` separating them, so
    ///   `id: UserId;` survives verbatim and the four `interface User` members
    ///   regain 37 B — putting the served side near 375 B and the headroom near
    ///   67 B.  Those two are DERIVED from the `c2b4378` figure plus that byte
    ///   delta, not re-measured; only the 442 B is verified.  Nothing here is
    ///   asserted: the precondition below re-derives the guard verdict at run
    ///   time, which is what keeps the test honest as the transform moves.
    ///
    /// The first comment is the module header, which #476 preserves in every
    /// language; the second sits below the header run and is removed. Both are
    /// worded so a reader of the fixture is not misled about which is which.
    const BLOB_FIXTURE: &str = "\
// module header — preserved in every language (#476)
type UserId = string;

interface User {
    id: UserId;
    name: string;
    email: string;
    active: boolean;
}

// a comment below the header run, which pseudo removes
class Registry {
    private users: User[] = [];

    add(u: User): void {
        this.users.push(u);
    }

    find(id: UserId): User | null {
        return this.users.find((u) => u.id === id) || null;
    }
}";

    /// Run `git` in `dir` with a fully hermetic environment and return stdout.
    ///
    /// PF-009: a bare `git init` is NOT hermetic — it inherits the developer's
    /// global and system config, so `commit.gpgsign`, `init.defaultBranch`,
    /// `core.autocrlf` or a missing `user.email` each turn a passing test into a
    /// machine-dependent one. Both config files are pointed at `/dev/null` and
    /// the identity comes from the environment, so nothing outside this
    /// function's `TempDir` can reach the repo.
    ///
    /// PF-031: no SHA is pinned and no repository history is read. The commit is
    /// created here and addressed as `HEAD`.
    #[cfg(unix)]
    fn git_stdout(dir: &std::path::Path, args: &[&str]) -> Vec<u8> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .unwrap_or_else(|e| {
                panic!("hermetic setup: `git {}` spawn failed: {e}", args.join(" "))
            });
        assert!(
            out.status.success(),
            "hermetic setup: `git {}` failed; stderr={}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }

    /// FACET 1 — the default serves the blob's exact bytes.
    ///
    /// `git show --no-color <rev>:<path>` — the invocation
    /// [`run_show_file_content`] makes — must be byte-identical to
    /// `git cat-file blob <rev>:<path>`, the narrowest way to ask git for a
    /// blob's contents. That equality is what makes the injected `--no-color`
    /// safe to keep on the verbatim path, and it is measured here rather than
    /// asserted from git's documentation.
    ///
    /// # Precondition (mandatory — this fix can fake its own success)
    ///
    /// Under a guardrail fallback, stdout IS the source file, and the source
    /// file contains the very `id: UserId;` a lossy view would have destroyed.
    /// So "stdout contains `id: UserId;`" passes green against a completely
    /// unfixed binary (PF-025). Two assertions run FIRST to exclude that:
    ///
    /// 1. `Mode::Pseudo` on this blob produces something DIFFERENT from it —
    ///    the fixture is a known-lossy input, not one the transform happens to
    ///    leave alone.
    /// 2. The ADR-001 guard — the same uncharged `guardrail::apply` the
    ///    production path calls — elects `Passed` for that transform, so the
    ///    pre-ADR-022 default WOULD have served the lossy view. If it elected
    ///    `Triggered`, raw would reach stdout on an unfixed binary too and this
    ///    test would prove nothing.
    ///
    /// A failure of either assertion means the fixture stopped exhibiting the
    /// defect and the test has gone vacuous — it must be read as "re-derive the
    /// fixture", never as "the guard now protects us".
    #[cfg(unix)]
    #[test]
    fn test_blob_default_serves_bytes_identical_to_git_cat_file_blob() {
        // ---- precondition: the fixture is a KNOWN-LOSSY input ----
        let transformed = rskim_core::transform(BLOB_FIXTURE, Language::TypeScript, Mode::Pseudo)
            .expect("Pseudo transform of the fixture must succeed");
        assert_ne!(
            transformed.as_str(),
            BLOB_FIXTURE,
            "PRECONDITION FAILED: Mode::Pseudo no longer changes BLOB_FIXTURE, so \
             this test would pass against an unfixed binary.  Re-derive the \
             fixture; do not weaken the assertion"
        );
        let mut banner = Vec::new();
        let outcome =
            crate::output::guardrail::apply(BLOB_FIXTURE.to_string(), transformed, &mut banner)
                .expect("guardrail must not error");
        assert!(
            banner.is_empty(),
            "PRECONDITION FAILED: the guardrail wrote a banner, which it only \
             does on the raw-fallback path; got: {}",
            String::from_utf8_lossy(&banner)
        );
        assert!(
            !outcome.was_triggered(),
            "PRECONDITION FAILED: the ADR-001 guard now elects raw for this \
             fixture under Mode::Pseudo, so an UNFIXED binary would also put the \
             blob's bytes on stdout and the byte comparison below would prove \
             nothing (PF-025).  Re-derive the fixture"
        );

        // ---- the measurement ----
        let dir = tempfile::tempdir().expect("tempdir must succeed");
        let repo = dir.path();
        git_stdout(repo, &["init", "-q", "-b", "main", "."]);
        std::fs::write(repo.join("blob.ts"), BLOB_FIXTURE).expect("write blob.ts");
        git_stdout(repo, &["add", "blob.ts"]);
        git_stdout(repo, &["commit", "-q", "-m", "seed"]);

        let ground_truth = git_stdout(repo, &["cat-file", "blob", "HEAD:blob.ts"]);
        assert_eq!(
            ground_truth,
            BLOB_FIXTURE.as_bytes(),
            "hermetic setup: the committed blob must round-trip unchanged — a \
             mismatch here means git config leaked in (core.autocrlf), not that \
             skim is wrong"
        );

        let via_show = git_stdout(repo, &["show", "--no-color", "HEAD:blob.ts"]);
        assert_eq!(
            via_show, ground_truth,
            "`git show --no-color <rev>:<path>` must be byte-identical to \
             `git cat-file blob <rev>:<path>`.  ADR-022's verbatim default hands \
             these exact bytes to exec::write_to_stdout, so if the injected \
             --no-color ever moved a byte the contract would be broken at the \
             source rather than in skim's own code"
        );
        assert!(
            !via_show.ends_with(b"\n"),
            "BLOB_FIXTURE must keep its missing final newline: it is what makes \
             this comparison able to catch an appended one"
        );

        // ---- the served view is the verbatim one ----
        // This is the assertion an unfixed binary cannot satisfy.  `Verbatim` is
        // not reachable at all before ADR-022: the old code ran
        // `TransformConfig::with_mode(Mode::Pseudo)` unconditionally.
        let (git_args, requested) =
            extract_show_mode_flag(&argv(&["HEAD:blob.ts"])).expect("a bare refpath must parse");
        assert_eq!(requested, None, "no --mode was given");
        assert_eq!(
            select_file_content_view(requested),
            FileContentView::Verbatim,
            "the default must select the verbatim view, which is the branch that \
             hands these bytes to stdout untouched"
        );
        assert_eq!(
            git_args,
            argv(&["HEAD:blob.ts"]),
            "the refpath must reach git unchanged"
        );
    }

    /// FACET 3 — `--mode` is skim-owned and never reaches git.
    ///
    /// Measured at `c2b4378`: `--mode=full` returned
    /// `fatal: unrecognized argument: --mode=full` and `--mode full` returned
    /// `fatal: ambiguous argument 'full'`, both exit 1 with zero stdout. So the
    /// flag did not silently no-op — it broke the command, which is why ADR-022
    /// records that the escape hatch the lossy default needed did not exist on
    /// this subcommand at all.
    ///
    /// The contract has two halves and this test asserts both: the value is
    /// understood, AND no `--mode` token survives into the argv handed to the
    /// child. Asserting only the first would leave the `fatal:` reachable.
    #[test]
    fn test_mode_flag_is_taken_off_the_argv_and_never_reaches_git() {
        // Both spellings, and `--mode=full` in particular, because that is the
        // documented escape a reader of the marker is told to use.
        for spelling in [
            argv(&["--mode=full", "HEAD:src/lib.rs"]),
            argv(&["--mode", "full", "HEAD:src/lib.rs"]),
            argv(&["HEAD:src/lib.rs", "--mode=full"]),
        ] {
            let (git_args, requested) = extract_show_mode_flag(&spelling)
                .unwrap_or_else(|e| panic!("--mode=full must not error, got: {e}"));
            assert_eq!(
                requested,
                Some(Mode::Full),
                "{spelling:?} must parse as Mode::Full"
            );
            assert_eq!(
                select_file_content_view(requested),
                FileContentView::Verbatim,
                "{spelling:?}: --mode=full means the bytes, unchanged"
            );
            assert_eq!(
                git_args,
                argv(&["HEAD:src/lib.rs"]),
                "{spelling:?}: every --mode token must be gone from the argv git \
                 receives — leaving one is what produced `fatal: unrecognized \
                 argument`"
            );
        }

        // A transforming mode is extracted identically: the flag is skim-owned
        // regardless of its value.
        let (git_args, requested) =
            extract_show_mode_flag(&argv(&["--mode=structure", "HEAD:src/lib.rs"]))
                .expect("--mode=structure must parse");
        assert_eq!(requested, Some(Mode::Structure));
        assert_eq!(git_args, argv(&["HEAD:src/lib.rs"]));

        // POSIX end-of-options: past a bare `--` every token is a path filter,
        // so a file literally named `--mode=full` must reach git intact.  This
        // matches `dispatch::strip_skim_flags`.
        let (git_args, requested) =
            extract_show_mode_flag(&argv(&["HEAD:x.ts", "--", "--mode=full"]))
                .expect("a separator must not error");
        assert_eq!(
            requested, None,
            "nothing after `--` is a skim flag, however it is spelled"
        );
        assert_eq!(git_args, argv(&["HEAD:x.ts", "--", "--mode=full"]));
    }

    /// FACET 3 (error half) — a bad `--mode` value is refused here, not forwarded.
    ///
    /// The value has to be rejected by skim rather than handed to git, because
    /// git's own diagnostic for it names neither `--mode` nor the vocabulary:
    /// `fatal: ambiguous argument 'bogus': unknown revision or path not in the
    /// working tree`. A caller who mistypes a mode must be told which modes
    /// exist.
    #[test]
    fn test_bad_mode_value_is_rejected_rather_than_forwarded_to_git() {
        let err = extract_show_mode_flag(&argv(&["--mode=bogus", "HEAD:x.ts"]))
            .expect_err("an unknown mode must be an error, not a silent default");
        let msg = err.to_string();
        assert!(
            msg.contains("bogus"),
            "the message must quote the rejected value; got: {msg}"
        );
        assert!(
            msg.contains("full") && msg.contains("pseudo"),
            "the message must name the vocabulary so the caller can correct it; \
             got: {msg}"
        );

        // `--mode` with nothing after it must not swallow the refpath as its
        // value, and must not reach git either.
        let err = extract_show_mode_flag(&argv(&["--mode"]))
            .expect_err("a valueless --mode must be an error");
        assert!(err.to_string().contains("requires a value"), "got: {}", err);
    }

    /// FACET 2 — a lossy `--mode` discloses itself on stderr (ADR-011 class 1).
    ///
    /// Measured at `c2b4378`: stderr was 0 bytes on all seven blobs tested,
    /// even under `SKIM_DEBUG=1`, and `lossy_view_marker` had no call site
    /// anywhere under `cmd/git/` — while `process.rs` emitted one for the
    /// identical transform on the file-read path. A class-1 marker is
    /// unconditional whenever the reader sees something different from raw, so
    /// the absence was a violation, not a policy.
    ///
    /// Asserted on the marker CONSTRUCTOR the production path calls, with the
    /// production path's own arguments. The class clause itself is owned and
    /// tested by `output::mode_class_label`; duplicating its wording here would
    /// pin a table this module does not own. What is pinned here is the mode
    /// spelling (which view you got) and the remedy (how to escape it).
    ///
    /// The `differing == 0` half is the anti-vacuity assertion, and it is the
    /// one that matters: it proves the marker is TIED to the guard having served
    /// the transform rather than emitted on every `--mode` invocation. Without
    /// it, a marker hardcoded to fire always would pass the first half.
    #[test]
    fn test_lossy_mode_emits_the_class_one_marker_on_stderr() {
        for m in [
            Mode::Minimal,
            Mode::Pseudo,
            Mode::Structure,
            Mode::Signatures,
            Mode::Types,
        ] {
            assert_eq!(
                select_file_content_view(Some(m)),
                FileContentView::Transformed(m),
                "--mode={} must reach the transforming branch, which is the only \
                 branch that discloses",
                m.name()
            );

            // `differing = 1` — the guard served the transformed view.
            let marker =
                crate::output::lossy_view_marker(None, m.name(), 1, 1).unwrap_or_else(|| {
                    panic!(
                        "--mode={} serves a view that differs from the blob, so \
                         ADR-011 class 1 owes an unconditional marker",
                        m.name()
                    )
                });
            assert!(
                marker.starts_with(&format!("[skim] {} view:", m.name())),
                "the marker must name the mode that was served, so the reader \
                 knows WHICH view they got; got: {marker}"
            );
            assert!(
                marker.contains("SKIM_PASSTHROUGH=1"),
                "ADR-011 class 1 requires the remedy clause.  It is literally \
                 reachable on this path: the convergence gate in cmd/dispatch.rs \
                 honours SKIM_PASSTHROUGH for `skim git show` and re-execs git \
                 (measured); got: {marker}"
            );

            // `differing = 0` — the guard served raw, nothing was lost, and a
            // marker here would be an ADR-011 class-2 banner masquerading as a
            // class-1 disclosure.
            assert_eq!(
                crate::output::lossy_view_marker(None, m.name(), 0, 1),
                None,
                "--mode={}: no marker is owed when the guardrail served raw — \
                 this is what ties the disclosure to actual loss rather than to \
                 the mere presence of --mode",
                m.name()
            );
        }
    }

    /// Every `RawReason` records a tier the analytics schema already knows.
    ///
    /// The enum replaced a `tier: u8` whose mapping table had a `_ => None` arm.
    /// The two pre-existing spellings must not move — a tier rename would
    /// silently reclassify recorded rows — and the new contract path must not
    /// invent a fourth vocabulary word.
    #[test]
    fn test_raw_reason_tiers_match_the_analytics_vocabulary() {
        assert_eq!(RawReason::UnsupportedLanguage.tier(), "degraded");
        assert_eq!(RawReason::TransformFailed.tier(), "passthrough");
        assert_eq!(
            RawReason::BlobContract.tier(),
            "passthrough",
            "raw == output on the contract path, which is the same thing \
             exec::emit_raw_passthrough records for every other subcommand"
        );

        // The contract path is not a fallback and must not say it is: calling
        // the ADR-022 default a fallback is how a reader concludes skim failed
        // at something.
        assert!(
            !RawReason::BlobContract.banner().contains("falling back"),
            "the ADR-022 default attempted nothing and failed at nothing; got: {}",
            RawReason::BlobContract.banner()
        );
        for r in [RawReason::UnsupportedLanguage, RawReason::TransformFailed] {
            assert!(
                r.banner().contains("falling back"),
                "{r:?} IS a fallback and should read as one; got: {}",
                r.banner()
            );
        }
    }

    // `--json` is still refused in file-content mode, and the exit code plus the
    // `--json is not supported` prefix are already pinned — in this module by
    // `test_file_content_mode_json_rejected`, and end-to-end by
    // `test_skim_git_show_file_content_json_rejected` (tests/cli_git.rs).
    // ADR-022 rewrote only the message's REASON clause ("the output is already
    // the compressed artifact" was true of the lossy default and is false of a
    // verbatim blob), which neither assertion reads.  A third test here would
    // assert what those two already do.
}
