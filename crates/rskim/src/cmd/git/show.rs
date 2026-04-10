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

use std::path::Path;
use std::process::ExitCode;

use rskim_core::{Language, TransformConfig};

use crate::cmd::{extract_output_format, user_has_flag, OutputFormat};
use crate::output::canonical::{DiffFileEntry, ShowCommitResult};
use crate::runner::CommandRunner;

use super::diff::{parse_unified_diff, render_diff_file, DiffMode};
use super::{map_exit_code, run_passthrough};

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
// Helpers
// ============================================================================

/// Record token stats to stderr and fire-and-forget analytics for a `git show` operation.
///
/// Takes both strings by reference so the common disabled-analytics path
/// avoids cloning potentially large outputs. Cloning happens only when
/// `is_analytics_enabled()` is true, matching the pattern used by
/// `run_passthrough` in `git/mod.rs`.
fn record_show_result(
    raw: &str,
    output: &str,
    command_label: String,
    show_stats: bool,
    duration: std::time::Duration,
) {
    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(raw, output);
        crate::process::report_token_stats(orig, comp, "");
    }
    if crate::analytics::is_analytics_enabled() {
        // Clone only when analytics is actually going to consume the strings.
        crate::analytics::try_record_command(
            raw.to_string(),
            output.to_string(),
            command_label,
            crate::analytics::CommandType::Git,
            duration,
            None,
        );
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
) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_show_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Passthrough for stat-family and format flags.
    if user_has_flag(args, PASSTHROUGH_FLAGS) {
        return run_passthrough(global_flags, "show", args, show_stats);
    }

    match detect_show_mode(args) {
        ShowMode::MultiRef => run_passthrough(global_flags, "show", args, show_stats),
        ShowMode::FileContent { refpath } => {
            run_show_file_content(global_flags, args, &refpath, show_stats)
        }
        ShowMode::Commit => {
            let (git_args, output_format) = extract_output_format(args);
            run_show_commit(global_flags, &git_args, args, output_format, show_stats)
        }
    }
}

// ============================================================================
// Commit mode
// ============================================================================

/// Parsed fields from a `git show` commit header.
#[derive(Debug, Default)]
struct CommitHeader {
    hash: String,
    author: String,
    date: String,
    subject: String,
}

/// Parse commit header lines (up to the first blank line after the message).
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
fn parse_commit_header(raw: &str) -> Option<(CommitHeader, &str)> {
    let mut header = CommitHeader::default();

    // Annotated tags start with `tag ` not `commit `.
    if !raw.starts_with("commit ") {
        return None;
    }

    // Locate the start of the diff body using a direct substring search.
    // The leading `\n` anchors the match to the start of a line to avoid
    // false positives inside commit message bodies that might mention
    // `diff --git` textually.
    let diff_body = match raw.find("\ndiff --git ") {
        Some(pos) => &raw[pos + 1..], // +1 to skip the anchoring newline
        None => "",
    };

    // Walk the header region (everything before the diff body, or the
    // whole string if there is no diff body) to extract hash/author/date
    // /subject. `lines()` handles both LF and CRLF endings.
    let header_region = if diff_body.is_empty() {
        raw
    } else {
        // Safe: diff_body is a suffix of raw; subtracting its length from
        // raw.len() yields the header region's end in bytes.
        &raw[..raw.len() - diff_body.len()]
    };

    let mut in_body = false;
    for line in header_region.lines() {
        if in_body {
            // First non-blank line after the blank separator is the subject.
            let trimmed = line.trim();
            if !trimmed.is_empty() && header.subject.is_empty() {
                header.subject = trimmed.to_string();
            }
        } else if line.starts_with("commit ") {
            header.hash = line
                .strip_prefix("commit ")
                .unwrap_or_default()
                .trim()
                .to_string();
        } else if line.starts_with("Author: ") {
            header.author = line
                .strip_prefix("Author: ")
                .unwrap_or_default()
                .trim()
                .to_string();
        } else if line.starts_with("Date: ") {
            header.date = line
                .strip_prefix("Date: ")
                .unwrap_or_default()
                .trim()
                .to_string();
        } else if line.is_empty() && !header.hash.is_empty() {
            in_body = true;
        }
    }

    if header.hash.is_empty() {
        return None;
    }

    Some((header, diff_body))
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
) -> anyhow::Result<ExitCode> {
    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.extend(["show".to_string(), "--no-color".to_string()]);
    full_args.extend_from_slice(git_args);

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
        return Ok(map_exit_code(output.exit_code));
    }

    let raw = output.stdout;
    let duration = output.duration;

    // Parse the commit header. Annotated tags and other non-commit objects fall back.
    let (header, diff_body) = match parse_commit_header(&raw) {
        Some(result) => result,
        None => {
            // Passthrough: not a regular commit (annotated tag, blob, tree, etc.)
            print!("{raw}");
            return Ok(ExitCode::SUCCESS);
        }
    };

    // Render the diff body using the AST-aware pipeline.
    let file_diffs = parse_unified_diff(diff_body);
    let mut rendered_diff = String::new();
    let mut diff_file_entries: Vec<DiffFileEntry> = Vec::new();

    for (i, file_diff) in file_diffs.iter().enumerate() {
        let skip_ast = i >= super::diff::MAX_AST_FILE_COUNT;
        let rendered = render_diff_file(
            file_diff,
            global_flags,
            git_args,
            DiffMode::Default,
            skip_ast,
        );
        rendered_diff.push_str(&rendered);

        diff_file_entries.push(DiffFileEntry {
            path: file_diff.path.clone(),
            status: file_diff.status.clone(),
            changed_regions: file_diff.hunks.len(),
        });
    }

    let result = ShowCommitResult::new(
        header.hash,
        header.author,
        header.date,
        header.subject,
        diff_file_entries,
        rendered_diff,
    );

    // Build the analytics label from the original args (before --json extraction)
    // so the DB sees `skim git show HEAD --json` vs `skim git show HEAD` as
    // distinct entries — matching the convention used by `diff/mod.rs:286`.
    let label = format!("skim git show {}", original_args.join(" "));

    match output_format {
        OutputFormat::Json => {
            // JSON: serialise result directly; guardrail is irrelevant here
            // because the JSON output is never substituted for raw text.
            // Running the guardrail on JSON would double the memory cost and
            // could spuriously emit `[skim:guardrail]` to stderr.
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| anyhow::anyhow!("failed to serialize show result: {e}"))?;
            println!("{json}");
            record_show_result(&raw, &json, label, show_stats, duration);
        }
        OutputFormat::Text => {
            // Apply guardrail: if compressed output is larger than raw, emit raw.
            // Move `raw` into the guardrail to avoid cloning; clone only when
            // analytics recording will actually use it.
            let result_str = result.to_string();
            let record_raw = if show_stats || crate::analytics::is_analytics_enabled() {
                Some(raw.clone())
            } else {
                None
            };
            let guardrail = crate::output::guardrail::apply_to_stderr(raw, result_str)?;
            let final_output = guardrail.into_output();
            print!("{final_output}");
            // Use the pre-move clone when stats/analytics need the original raw.
            let raw_ref = record_raw.as_deref().unwrap_or("");
            record_show_result(raw_ref, &final_output, label, show_stats, duration);
        }
    }

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// File-content mode
// ============================================================================

/// Run `git show <ref>:<path>` in file-content mode.
///
/// Applies skim's source transformation when the file extension is supported.
/// Falls back to passthrough for unsupported extensions or parse failures.
fn run_show_file_content(
    global_flags: &[String],
    args: &[String],
    refpath: &str,
    show_stats: bool,
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
    let path_str = refpath
        .rfind(':')
        .map(|pos| &refpath[pos + 1..])
        .unwrap_or(refpath);

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.push("show".to_string());
    // Pass through all original args (show.rs does not strip args for file-content mode).
    full_args.extend_from_slice(args);

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
        return Ok(map_exit_code(output.exit_code));
    }

    let raw = output.stdout;
    let duration = output.duration;

    // Detect language from path extension.
    let lang = Language::from_path(Path::new(path_str)).filter(|l| !l.is_serde_based());

    let Some(lang) = lang else {
        // Unsupported or serde-based language — passthrough.
        print!("{raw}");
        let label = format!("skim git show {}", args.join(" "));
        // Raw equals output on passthrough; pass the same ref twice.
        record_show_result(&raw, &raw, label, show_stats, duration);
        return Ok(ExitCode::SUCCESS);
    };

    // Transform in memory.
    let config = TransformConfig::default();
    let transformed = match rskim_core::transform(&raw, lang, config.mode) {
        Ok(t) => t,
        Err(e) => {
            // Transform failed — fall back to raw passthrough.
            // Record as a zero-compression pass so analytics and --show-stats
            // remain consistent with the unsupported-language branch above.
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "[skim:debug] git show file-content transform failed for {path_str}: {e}"
                );
            }
            print!("{raw}");
            let label = format!("skim git show {}", args.join(" "));
            record_show_result(&raw, &raw, label, show_stats, duration);
            return Ok(ExitCode::SUCCESS);
        }
    };

    // Guardrail: if transformation inflated the output, emit raw.
    let guardrail = crate::output::guardrail::apply_to_stderr(raw.clone(), transformed)?;
    let final_output = guardrail.into_output();

    print!("{final_output}");
    let label = format!("skim git show {}", args.join(" "));
    record_show_result(&raw, &final_output, label, show_stats, duration);

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
    println!("EXAMPLES:");
    println!("    skim git show HEAD");
    println!("    skim git show HEAD:src/main.rs");
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
        assert_eq!(&header.hash[..7], "abc1234");
        assert!(
            header.author.contains("Jane Dev"),
            "expected author to contain 'Jane Dev', got: {}",
            header.author
        );
        assert_eq!(header.subject, "feat: add user authentication handler");
        assert!(
            diff_body.contains("diff --git"),
            "diff body should start with diff --git"
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
            vec![],
            "diff output".to_string(),
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

    #[test]
    fn test_file_content_mode_passthrough_for_unknown_extension() {
        // `.lock` has no language → passthrough (no transform).
        let path = Path::new("Cargo.lock");
        let lang = Language::from_path(path).filter(|l| !l.is_serde_based());
        assert!(
            lang.is_none(),
            "Cargo.lock must not have a tree-sitter language"
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

    /// `--json` in file-content mode must be detected and rejected.
    ///
    /// The actual exit code (2) is verified by the E2E integration test
    /// `test_skim_git_show_file_content_json_rejected` in `tests/cli_git.rs`.
    #[test]
    fn test_file_content_mode_json_rejected() {
        // run_show_file_content checks user_has_flag first before spawning git.
        // Confirm the guard fires so the exit-2 path is taken.
        let args_with_json: Vec<String> = vec!["HEAD:src/main.rs".into(), "--json".into()];
        assert!(
            user_has_flag(&args_with_json, &["--json"]),
            "--json must be detected in file-content args"
        );

        let args_without_json: Vec<String> = vec!["HEAD:src/main.rs".into()];
        assert!(
            !user_has_flag(&args_without_json, &["--json"]),
            "flag must not fire when --json is absent"
        );
    }

    // ========================================================================
    // Guardrail: verify the call chain compiles and functions are visible
    // ========================================================================

    /// Documents the guardrail path: if transform produces a larger output,
    /// the guardrail emits the raw string. We verify this with synthetic data.
    #[test]
    fn test_guardrail_fallback_when_transform_inflates() {
        // Raw must be >= MIN_RAW_SIZE_FOR_GUARDRAIL (256 bytes) to activate the guardrail.
        // Use 300 bytes of raw, then an inflated output with substantially more tokens.
        let raw = "x".repeat(300);
        let inflated = "this is a much longer string with many more tokens ".repeat(20);
        let mut buf = Vec::new();
        let outcome = crate::output::guardrail::apply(raw.clone(), inflated, &mut buf).unwrap();
        // Guardrail should have triggered and returned the raw content.
        assert!(
            outcome.was_triggered(),
            "guardrail must trigger when output inflates"
        );
        assert_eq!(outcome.into_output(), raw);
    }

    // ========================================================================
    // Show no panic on malformed input
    // ========================================================================

    #[test]
    fn test_show_no_panic_on_malformed_commit_header() {
        // Garbage input should either return None or a partially-filled header.
        let garbage = "\x00\x01\x02\x03 garbage bytes here";
        let result = parse_commit_header(garbage);
        // Should not panic — either None (doesn't start with "commit ") or Some.
        let _ = result;
    }

    #[test]
    fn test_show_no_panic_on_empty_diff_body() {
        // A commit with no diff body should produce an empty file list.
        let raw = "commit abc1234\nAuthor: Test <t@t.com>\nDate: Thu\n\n    subject\n";
        let result = parse_commit_header(raw);
        if let Some((header, diff_body)) = result {
            assert_eq!(header.subject, "subject");
            let files = parse_unified_diff(diff_body);
            assert!(files.is_empty());
        }
    }
}
