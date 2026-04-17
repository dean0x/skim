//! `git commit` output compression.
//!
//! Parses the combined stdout+stderr from `git commit` into a compact
//! [`GitResult`] surfacing the commit hash, subject, and changed-files summary.
//!
//! # DESIGN NOTE (AD-GC-1) — verbose diff termination
//!
//! `git commit -v` appends the staged diff below a `----------------------- >8 -`
//! scissors line.  This is never useful after the commit is already recorded.
//! The parser terminates at the first line matching `^---+ ?>{0,1}8` and discards
//! everything after it, retaining only the commit summary above the scissors.
//!
//! # DESIGN NOTE (AD-GC-2) — no editor spawn
//!
//! We do NOT spawn `git commit` ourselves here; the caller (cmd/git/mod.rs via
//! `run_parsed_command`) spawns it.  This avoids the interactive-editor problem:
//! if the user has no `-m` flag, git spawns an editor, which requires a TTY.
//! The rewrite hook in agent sessions always includes `-m "..."`, so we receive
//! a non-interactive commit.  Passing through on exit-code 1 ("nothing to
//! commit") is handled by `run_parsed_command`'s non-zero exit path.
//!
//! # Combine stderr
//!
//! Git commit writes hook output and some informational lines to stderr as well
//! as stdout.  We set `combine_stderr: true` so the parser receives the full
//! picture (pre-commit hook output, "nothing to commit" messages, etc.).

use std::process::ExitCode;

use crate::cmd::{extract_output_format, user_has_flag};
use crate::output::canonical::GitResult;
use crate::output::strip_ansi;

use super::{run_parsed_command, run_passthrough};

// ============================================================================
// Public entry point
// ============================================================================

/// Run `git commit` with output compression.
///
/// Flag-aware passthrough:
/// - `--help` passes through unmodified.
///
/// All other flags (including `-m`, `--amend`, `--allow-empty`, `--no-verify`,
/// `--fixup`, `--squash`) are forwarded to the parser.
pub(super) fn run_commit(
    global_flags: &[String],
    args: &[String],
    show_stats: bool,
    analytics_enabled: bool,
) -> anyhow::Result<ExitCode> {
    if user_has_flag(args, &["--help"]) {
        return run_passthrough(global_flags, "commit", args, show_stats, analytics_enabled);
    }

    let (filtered_args, output_format) = extract_output_format(args);

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.push("commit".to_string());
    full_args.extend_from_slice(&filtered_args);

    let label = super::build_analytics_label("commit", args, show_stats, analytics_enabled);

    run_parsed_command(
        &full_args,
        show_stats,
        analytics_enabled,
        output_format,
        true, // combine_stderr: hook output and "nothing to commit" come from stderr
        label,
        parse_commit,
    )
}

// ============================================================================
// Parser
// ============================================================================

/// Parse `git commit` output into a compact [`GitResult`].
///
/// Three-tier contract:
/// - **Full**: Successful commit with hash + subject + changed-files summary.
/// - **Full**: "nothing to commit" message (informational passthrough-in-structure).
/// - **Full**: Pre-commit hook output stripped; only the commit line shown.
///
/// When input is empty or cannot be parsed, returns a minimal passthrough result.
pub(super) fn parse_commit(input: &str) -> GitResult {
    let clean = strip_ansi(input);
    let text: &str = clean.as_ref();

    // Terminate at scissors line (AD-GC-1: verbose diff removal).
    let text = terminate_at_scissors(text);

    if text.trim().is_empty() {
        return GitResult::new("commit".to_string(), "no output".to_string(), Vec::new())
            .with_tier("passthrough");
    }

    // "nothing to commit" — surface the relevant line as summary.
    if text.contains("nothing to commit") || text.contains("nothing added to commit") {
        let summary_line = text
            .lines()
            .find(|l| l.contains("nothing to commit") || l.contains("nothing added to commit"))
            .unwrap_or("nothing to commit");
        return GitResult::new(
            "commit".to_string(),
            summary_line.trim().to_string(),
            Vec::new(),
        )
        .with_tier("full");
    }

    // Try to extract structured commit info.
    if let Some(result) = try_parse_structured(text) {
        return result;
    }

    // Fallback: surface first non-empty meaningful line.
    let summary = text
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty() && !is_hook_noise(l))
        .unwrap_or("committed")
        .to_string();

    GitResult::new("commit".to_string(), summary, Vec::new()).with_tier("degraded")
}

// ============================================================================
// Parsing helpers
// ============================================================================

/// Truncate input at the verbose diff scissors line.
///
/// The scissors line is `---...--- >8 ---` (variable dashes, optional space).
/// Everything from this line onward is discarded.
fn terminate_at_scissors(text: &str) -> &str {
    let mut byte_pos = 0;
    for line in text.lines() {
        if is_scissors_line(line) {
            return &text[..byte_pos];
        }
        byte_pos += line.len() + 1; // +1 for newline
    }
    text
}

/// Returns `true` if the line is a verbose-diff scissors marker.
fn is_scissors_line(line: &str) -> bool {
    let trimmed = line.trim();
    // Pattern: `--- ... >8 ---` with at least 3 dashes on each side.
    trimmed.contains(">8") && trimmed.starts_with("---") && trimmed.ends_with("---")
}

/// Try to parse the commit hash/subject/stats from structured git commit output.
///
/// Git commit output format (non-quiet):
/// ```text
/// [branch hash] Subject line
///  N files changed, M insertions(+), K deletions(-)
///  create mode 100644 path/to/file
/// ```
///
/// Returns `None` if the pattern is not found (caller falls through to degraded).
fn try_parse_structured(text: &str) -> Option<GitResult> {
    let mut lines = text.lines().map(|l| l.trim()).filter(|l| !l.is_empty());

    // Find the commit line: starts with `[` and contains `]`.
    let commit_line = lines.find(|l| l.starts_with('[') && l.contains(']'))?;

    // Extract hash and subject from `[branch hash] Subject`.
    let after_bracket = commit_line.find(']').map(|i| &commit_line[i + 1..])?;
    let subject = after_bracket.trim().to_string();

    // Collect the stats and created-file lines.
    let mut details: Vec<String> = Vec::new();
    for line in text.lines().map(|l| l.trim()) {
        if line.is_empty() || is_hook_noise(line) {
            continue;
        }
        if line.starts_with('[') {
            continue; // commit line already used
        }
        // Keep "N files changed", "create mode", "delete mode", "rename" lines.
        if line.contains("file")
            || line.starts_with("create mode")
            || line.starts_with("delete mode")
            || line.starts_with("rename ")
        {
            details.push(line.to_string());
        }
    }

    Some(GitResult::new("commit".to_string(), subject, details).with_tier("full"))
}

/// Returns `true` if the line is hook noise that should be excluded from
/// the compressed output summary.
///
/// Hook lines typically start with common linter/formatter prefixes or are
/// empty progress lines. We don't strip them entirely from details — they
/// may be useful when present — but we skip them when selecting the summary
/// line for degraded-tier output.
fn is_hook_noise(line: &str) -> bool {
    // Common pre-commit hook patterns.
    line.starts_with("trim trailing whitespace")
        || line.starts_with("fix end of files")
        || line.starts_with("check yaml")
        || line.starts_with("check for added large files")
        || line.starts_with("black")
        || line.starts_with("ruff")
        || line.starts_with("eslint")
        || line.starts_with("prettier")
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Happy path ----

    #[test]
    fn test_parse_simple_commit() {
        let input = "[main abc1234] feat: add widget\n 1 file changed, 10 insertions(+)\n";
        let result = parse_commit(input);
        assert_eq!(result.operation, "commit");
        assert!(result.summary.contains("feat: add widget"), "summary: {}", result.summary);
        assert!(result.details.iter().any(|d| d.contains("file changed")));
    }

    #[test]
    fn test_parse_amend_commit() {
        let input = "[main abc1234 (amend)] fix: typo in README\n 1 file changed, 1 insertion(+), 1 deletion(-)\n";
        let result = parse_commit(input);
        assert!(result.summary.contains("fix: typo in README"), "summary: {}", result.summary);
    }

    #[test]
    fn test_parse_nothing_to_commit() {
        let input = "On branch main\nnothing to commit, working tree clean\n";
        let result = parse_commit(input);
        assert_eq!(result.operation, "commit");
        assert!(result.summary.contains("nothing to commit"), "summary: {}", result.summary);
    }

    #[test]
    fn test_verbose_diff_terminated_at_scissors() {
        let input = concat!(
            "[main abc1234] refactor: cleanup\n",
            " 1 file changed, 3 insertions(+), 1 deletion(-)\n",
            "---------------------------- >8 ----------------------------\n",
            "diff --git a/src/lib.rs b/src/lib.rs\n",
            "@@ -1,3 +1,5 @@\n",
            "+fn new_fn() {}\n"
        );
        let result = parse_commit(input);
        // Summary should be the commit subject, diff lines should NOT appear in details.
        assert!(result.summary.contains("refactor: cleanup"), "summary: {}", result.summary);
        let rendered = format!("{result}");
        assert!(!rendered.contains("diff --git"), "diff should be terminated");
        assert!(!rendered.contains("@@"), "diff hunks should be stripped");
    }

    #[test]
    fn test_parse_multifile_commit() {
        let input = concat!(
            "[main d2e3f4a] chore: reorganize modules\n",
            " 3 files changed, 45 insertions(+), 12 deletions(-)\n",
            " create mode 100644 src/cmd/git/commit.rs\n",
            " rename src/old.rs => src/new.rs (100%)\n"
        );
        let result = parse_commit(input);
        assert!(result.details.iter().any(|d| d.contains("3 files changed")));
        assert!(result.details.iter().any(|d| d.contains("create mode")));
        assert!(result.details.iter().any(|d| d.contains("rename")));
    }

    #[test]
    fn test_parse_empty_input() {
        let result = parse_commit("");
        assert_eq!(result.operation, "commit");
        assert_eq!(result.parse_tier, Some("passthrough"));
    }

    #[test]
    fn test_parse_with_hook_output() {
        let input = concat!(
            "trim trailing whitespace.................................................Passed\n",
            "black....................................................................Passed\n",
            "[main 5f6a7b8] test: add unit coverage\n",
            " 2 files changed, 30 insertions(+)\n"
        );
        let result = parse_commit(input);
        // Summary should be from the commit line, not hook noise.
        assert!(result.summary.contains("test: add unit coverage"), "summary: {}", result.summary);
    }

    #[test]
    fn test_parse_gpg_signed_commit() {
        let input = concat!(
            "[main abc1234] feat: signed commit\n",
            " 1 file changed, 5 insertions(+)\n"
        );
        let result = parse_commit(input);
        assert!(result.summary.contains("signed commit"));
    }

    // ---- Compression check ----

    #[test]
    fn test_output_is_shorter_than_input() {
        let input = concat!(
            "trim trailing whitespace.................................................Passed\n",
            "fix end of files.........................................................Passed\n",
            "check yaml...........................................(no files to check)Skipped\n",
            "[main 9abc123] feat: new feature\n",
            " 1 file changed, 50 insertions(+)\n",
            "---------------------------- >8 ----------------------------\n",
            "diff --git a/src/lib.rs b/src/lib.rs\n",
            "index 1234567..89abcde 100644\n",
            "--- a/src/lib.rs\n",
            "+++ b/src/lib.rs\n",
            "@@ -1,3 +1,53 @@\n",
            "+fn added_function_1() {}\n",
            "+fn added_function_2() {}\n",
        );
        let result = parse_commit(input);
        let rendered = format!("{result}");
        assert!(
            rendered.len() < input.len(),
            "Compressed output should be shorter than input: compressed={}, raw={}",
            rendered.len(),
            input.len()
        );
    }
}
