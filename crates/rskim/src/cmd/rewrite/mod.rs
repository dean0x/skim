//! Command rewrite engine (#43, #44, #132)
//!
//! Rewrites common developer commands into skim equivalents using a two-layer
//! rule system:
//!
//! **Layer 1 — Declarative prefix-swap table**: Ordered longest-prefix-first.
//! Each rule maps a command prefix (e.g. `["cargo", "test"]`) to a skim
//! equivalent (e.g. `["skim", "test", "cargo"]`), with optional skip-flags
//! that suppress the rewrite when present.
//!
//! **Layer 2 — Custom handlers**: For commands requiring argument inspection
//! (cat, head, tail) where simple prefix matching is insufficient.
//!
//! **Hook mode** (`--hook`): Runs as an agent PreToolUse hook via `HookProtocol`.
//! Reads JSON from stdin, extracts the command field (agent-specific), rewrites if
//! matched, and emits agent-specific hook-protocol JSON. Each agent's
//! `format_response()` controls the response shape — see `hooks/` module.
//!
//! **Tri-state classification** (`classify_command`, AD-2): Exposes a richer
//! API used by `discover` to distinguish between commands that are genuinely
//! rewritten, commands whose output is already compact (acknowledged), and
//! commands that are true compression gaps.

mod acknowledge;
mod compound;
mod engine;
mod handlers;
mod hook;
mod rules;
mod suggest;
mod types;

use std::io::{self, BufRead, IsTerminal, Read};
use std::process::ExitCode;

use acknowledge::is_segment_ack;
use compound::{split_compound, try_rewrite_compound};
use engine::try_rewrite;
use hook::{parse_agent_flag, run_hook_mode};
use suggest::{print_help, print_suggest};
use types::{CommandSegment, CompoundOp, CompoundSplitResult, RewriteCategory, RewriteResult};

// Re-export the clap command for completions.rs
pub(super) use suggest::command;

// ============================================================================
// Public API for other modules
// ============================================================================

/// Tri-state classification of a shell command (AD-2).
///
/// Used by `discover` and the `rewrite` CLI to distinguish genuine compression
/// gaps from already-optimal commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandClassification {
    /// Command matches a rewrite rule and should be replaced with this string.
    Rewritten(String),
    /// Command is intentionally left alone — its output is already near-optimal.
    AlreadyCompact,
    /// No rule matched and no acknowledgement exists — this is a compression gap.
    Unhandled,
}

/// Classify a shell command as `Rewritten`, `AlreadyCompact`, or `Unhandled`.
///
/// Handles both simple and compound commands (via `&&`, `||`, `;`, `|`).
/// Returns `Unhandled` for empty input, already-skim commands, and bail
/// cases (heredocs, subshells, backticks).
///
/// # AD-3 CLI behavior
/// The `rewrite` CLI maps:
/// - `Rewritten(s)` → print `s`, exit 0
/// - `AlreadyCompact` → print original command, exit 0
/// - `Unhandled` → print nothing, exit 1
pub(crate) fn classify_command(command: &str) -> CommandClassification {
    let command = command.trim();
    if command.is_empty() || command.starts_with("skim ") {
        return CommandClassification::Unhandled;
    }

    // Fast path: no compound operators — classify single segment directly.
    if !has_compound_operators(command) {
        let tokens: Vec<&str> = command.split_whitespace().collect();
        return classify_segment(&tokens);
    }

    // Compound: split and classify.
    match split_compound(command) {
        CompoundSplitResult::Bail => CommandClassification::Unhandled,
        CompoundSplitResult::Simple(tokens) => {
            let refs: Vec<&str> = tokens.iter().map(|s| s.as_str()).collect();
            classify_segment(&refs)
        }
        CompoundSplitResult::Compound(segments) => classify_compound(command, &segments),
    }
}

/// Check if a command would be rewritten, returning the rewritten form.
///
/// Thin wrapper around `classify_command` that preserves the existing
/// `Option<String>` API for backwards compatibility with discover tests.
///
/// Returns `Some(rewritten_command)` if the command matches a rewrite rule,
/// `None` if no rewrite applies (including skim commands, empty input,
/// unsupported shell syntax, and acknowledged-compact commands).
// Kept for backward-compatibility; primary callers are tests in discover.rs.
#[allow(dead_code)]
pub(crate) fn would_rewrite(command: &str) -> Option<String> {
    match classify_command(command) {
        CommandClassification::Rewritten(s) => Some(s),
        _ => None,
    }
}

// ============================================================================
// classify_command internals
// ============================================================================

/// Return `true` when `s` contains a shell compound operator (`&&`, `||`, `;`, `|`).
///
/// Used as a fast-path gate before invoking the full compound-split state machine.
fn has_compound_operators(s: &str) -> bool {
    s.contains("&&") || s.contains("||") || s.contains(';') || s.contains('|')
}

/// Classification result for a single command segment (not a full command string).
#[derive(Debug, Clone)]
enum SegmentClassification {
    /// Segment matches a rewrite rule — store the rewritten tokens.
    Rewritten(Vec<String>),
    /// Segment is acknowledged compact — store original tokens for passthrough.
    AlreadyCompact(Vec<String>),
    /// No rule matched.
    NoMatch(Vec<String>),
}

/// Classify a tokenized single (non-compound) command segment.
fn classify_segment(tokens: &[&str]) -> CommandClassification {
    if is_segment_ack(tokens) {
        return CommandClassification::AlreadyCompact;
    }
    match try_rewrite(tokens) {
        Some(r) => CommandClassification::Rewritten(r.tokens.join(" ")),
        None => CommandClassification::Unhandled,
    }
}

/// Classify a tokenized segment, returning the fine-grained `SegmentClassification`.
fn classify_segment_fine(tokens: &[&str]) -> SegmentClassification {
    let owned: Vec<String> = tokens.iter().map(|s| s.to_string()).collect();
    if is_segment_ack(tokens) {
        return SegmentClassification::AlreadyCompact(owned);
    }
    match try_rewrite(tokens) {
        Some(r) => SegmentClassification::Rewritten(r.tokens),
        None => SegmentClassification::NoMatch(owned),
    }
}

/// Classify a compound command (segments connected by `&&`, `||`, `;`, `|`).
///
/// Rules (per AD-2):
/// - Any `NoMatch` segment → `Unhandled` (a compression gap exists).
/// - All `AlreadyCompact` → `AlreadyCompact`.
/// - Mix of `Rewritten` + `AlreadyCompact` → `Rewritten(reconstructed)`.
///
/// Pipe policy (mirrors `try_rewrite_compound_pipe`): only the first segment
/// of a pipe expression is classified for rewriting. Subsequent pipe stages
/// are left unchanged. This prevents wrapping `git diff | less` into
/// `skim git diff | skim less`.
fn classify_compound(original: &str, segments: &[CommandSegment]) -> CommandClassification {
    if segments.is_empty() {
        return CommandClassification::Unhandled;
    }

    // Check if this is a pipe expression (any segment has a Pipe operator).
    let has_pipe = segments
        .iter()
        .any(|s| s.trailing_operator == Some(CompoundOp::Pipe));

    if has_pipe {
        return classify_compound_pipe(original, segments);
    }

    // For &&/||/; — classify each segment independently.
    let mut any_rewritten = false;
    let mut any_unhandled = false;
    let mut parts: Vec<String> = Vec::new();

    for seg in segments {
        let token_refs: Vec<&str> = seg.tokens.iter().map(|s| s.as_str()).collect();
        let classification = classify_segment_fine(&token_refs);

        let segment_text = match classification {
            SegmentClassification::Rewritten(rewritten_tokens) => {
                any_rewritten = true;
                rewritten_tokens.join(" ")
            }
            SegmentClassification::AlreadyCompact(original_tokens) => {
                // Passthrough: AlreadyCompact segments use their original text.
                original_tokens.join(" ")
            }
            SegmentClassification::NoMatch(original_tokens) => {
                any_unhandled = true;
                original_tokens.join(" ")
            }
        };

        parts.push(segment_text);
        if let Some(op) = seg.trailing_operator {
            parts.push(op.as_str().to_string());
        }
    }

    if any_unhandled {
        return CommandClassification::Unhandled;
    }

    if any_rewritten {
        return CommandClassification::Rewritten(parts.join(" "));
    }

    // All segments were AlreadyCompact.
    CommandClassification::AlreadyCompact
}

/// Classify a pipe expression.
///
/// Only the first segment (output producer) is considered for rewriting.
/// If the first segment is `AlreadyCompact`, the whole pipe is `AlreadyCompact`.
/// If the first segment is `NoMatch` (or unclassified), the whole pipe is `Unhandled`.
fn classify_compound_pipe(_original: &str, segments: &[CommandSegment]) -> CommandClassification {
    if segments.is_empty() {
        return CommandClassification::Unhandled;
    }

    let first = &segments[0];
    let token_refs: Vec<&str> = first.tokens.iter().map(|s| s.as_str()).collect();

    // Check exclusion list (sources like find/rg/ls whose pipe output should not
    // be rewritten). Reuse the same logic as try_rewrite_compound_pipe.
    let env_split = engine::strip_env_vars(&token_refs);
    if let Some(cmd) = token_refs.get(env_split) {
        if compound::PIPE_EXCLUDED_SOURCES.contains(cmd) {
            return CommandClassification::Unhandled;
        }
    }

    let first_classification = classify_segment_fine(&token_refs);

    match first_classification {
        SegmentClassification::AlreadyCompact(_) => CommandClassification::AlreadyCompact,
        SegmentClassification::NoMatch(_) => CommandClassification::Unhandled,
        SegmentClassification::Rewritten(rewritten_tokens) => {
            // Reconstruct: rewritten first segment | rest unchanged.
            let mut parts: Vec<String> = Vec::new();
            parts.push(rewritten_tokens.join(" "));

            for (idx, seg) in segments.iter().enumerate() {
                if idx == 0 {
                    if let Some(op) = seg.trailing_operator {
                        parts.push(op.as_str().to_string());
                    }
                    continue;
                }
                parts.push(seg.tokens.join(" "));
                if let Some(op) = seg.trailing_operator {
                    parts.push(op.as_str().to_string());
                }
            }

            CommandClassification::Rewritten(parts.join(" "))
        }
    }
}

// ============================================================================
// Entry point
// ============================================================================

/// Run the `rewrite` subcommand. Returns the process exit code.
///
/// Exit code semantics (AD-3):
/// - 0: rewrite found (printed to stdout), or AlreadyCompact (original printed to stdout)
/// - 1: no rewrite match (Unhandled) or invalid input
///
/// For compound commands (`&&`, `||`, `;`, `|`), the original rewrite semantics
/// apply: ANY matched segment causes a rewrite (leaving unmatched segments as-is).
/// For simple (non-compound) commands, `classify_command` is used which also
/// handles the `AlreadyCompact` case.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    // Handle --help / -h
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Hook mode: run as agent PreToolUse hook (#44)
    if args.iter().any(|a| a == "--hook") {
        // Parse optional --agent flag
        let agent = parse_agent_flag(args);
        return run_hook_mode(agent);
    }

    // Check for --suggest flag (must be first non-help flag)
    let suggest_mode = args.first().is_some_and(|a| a == "--suggest");

    // Collect command tokens: skip leading --suggest if present
    let positional_start = if suggest_mode { 1 } else { 0 };
    let positional_args: Vec<&str> = args[positional_start..]
        .iter()
        .map(|s| s.as_str())
        .collect();

    // Get command tokens from positional args or stdin
    let tokens: Vec<String> = if positional_args.is_empty() {
        // Try reading from stdin if it's piped
        if io::stdin().is_terminal() {
            return emit_result(suggest_mode, "", None, false);
        }
        // Read one line from stdin, capped at 4 KiB to prevent unbounded allocation.
        // Uses take() to bound memory before reading, so even input without a newline
        // cannot cause unbounded allocation.
        let mut line = String::new();
        io::BufReader::new(io::stdin().lock().take(4096)).read_line(&mut line)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return emit_result(suggest_mode, "", None, false);
        }
        trimmed.split_whitespace().map(String::from).collect()
    } else {
        positional_args.iter().map(|s| s.to_string()).collect()
    };

    if tokens.is_empty() {
        return emit_result(suggest_mode, "", None, false);
    }

    let original = tokens.join(" ");

    // Fast path: if no compound operator chars are present, use classify_command
    // which also handles the AlreadyCompact case (AD-3).
    if !has_compound_operators(&original) {
        let token_refs: Vec<&str> = tokens.iter().map(|s| s.as_str()).collect();

        // Check AlreadyCompact first (acknowledged-compact commands, AD-2/AD-3).
        if is_segment_ack(&token_refs) {
            if suggest_mode {
                // AlreadyCompact is not a rewrite match — report as no-match in suggest.
                print_suggest(&original, None, false);
                return Ok(ExitCode::SUCCESS);
            }
            // AD-3: print original command unchanged, exit 0.
            println!("{original}");
            return Ok(ExitCode::SUCCESS);
        }

        // Normal rewrite path — uses the real RewriteResult (with correct category).
        let result = try_rewrite(&token_refs);
        return emit_rewrite_result(suggest_mode, &original, result, false);
    }

    // Compound commands: use original try_rewrite_compound semantics (any match wins).
    // AlreadyCompact detection for compound commands is provided by classify_command
    // for discover purposes, not for the CLI rewrite subcommand.
    match split_compound(&original) {
        CompoundSplitResult::Bail => emit_result(suggest_mode, &original, None, false),
        CompoundSplitResult::Simple(simple_tokens) => {
            let token_refs: Vec<&str> = simple_tokens.iter().map(|s| s.as_str()).collect();
            let result = try_rewrite(&token_refs);
            emit_rewrite_result(suggest_mode, &original, result, false)
        }
        CompoundSplitResult::Compound(segments) => {
            let result = try_rewrite_compound(&segments);
            emit_rewrite_result(suggest_mode, &original, result, true)
        }
    }
}

/// Emit the final result of a rewrite attempt.
///
/// In suggest mode, always prints JSON and returns SUCCESS.
/// In normal mode, prints the rewritten command on match (SUCCESS) or
/// returns FAILURE silently on no match.
fn emit_result(
    suggest_mode: bool,
    original: &str,
    result: Option<(&str, RewriteCategory)>,
    compound: bool,
) -> anyhow::Result<ExitCode> {
    if suggest_mode {
        print_suggest(original, result, compound);
        return Ok(ExitCode::SUCCESS);
    }
    match result {
        Some((rewritten, _)) => {
            println!("{rewritten}");
            Ok(ExitCode::SUCCESS)
        }
        None => Ok(ExitCode::FAILURE),
    }
}

/// Convert a `RewriteResult` into the final output via `emit_result`.
fn emit_rewrite_result(
    suggest_mode: bool,
    original: &str,
    result: Option<RewriteResult>,
    compound: bool,
) -> anyhow::Result<ExitCode> {
    let rewritten = result.as_ref().map(|r| r.tokens.join(" "));
    let match_info = result
        .as_ref()
        .zip(rewritten.as_ref())
        .map(|(r, s)| (s.as_str(), r.category));
    emit_result(suggest_mode, original, match_info, compound)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // classify_command() — tri-state API tests (AD-2)
    // ========================================================================

    #[test]
    fn test_classify_simple_rewritten() {
        assert_eq!(
            classify_command("git show HEAD"),
            CommandClassification::Rewritten("skim git show HEAD".to_string()),
            "git show HEAD must be classified as Rewritten"
        );
    }

    #[test]
    fn test_classify_simple_already_compact() {
        assert_eq!(
            classify_command("git worktree list"),
            CommandClassification::AlreadyCompact,
            "git worktree list must be classified as AlreadyCompact"
        );
    }

    #[test]
    fn test_classify_simple_unhandled() {
        assert_eq!(
            classify_command("echo hello"),
            CommandClassification::Unhandled,
            "echo hello is not rewritable or acknowledged"
        );
    }

    #[test]
    fn test_classify_compound_all_rewritten() {
        let result = classify_command("cargo test && cargo clippy");
        match result {
            CommandClassification::Rewritten(s) => {
                assert!(
                    s.contains("skim test cargo"),
                    "Expected skim test cargo in output, got: {s}"
                );
                assert!(
                    s.contains("skim build clippy"),
                    "Expected skim build clippy in output, got: {s}"
                );
                assert!(s.contains("&&"), "Expected && operator in output, got: {s}");
            }
            other => panic!("Expected Rewritten, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_compound_mixed_rewritten_ack() {
        let result = classify_command("git worktree list && git show HEAD");
        match result {
            CommandClassification::Rewritten(s) => {
                assert!(
                    s.contains("git worktree list"),
                    "AlreadyCompact segment must pass through unchanged: {s}"
                );
                assert!(
                    s.contains("skim git show HEAD"),
                    "Rewritten segment must be rewritten: {s}"
                );
            }
            other => panic!("Expected Rewritten for mixed ack+rewritten, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_compound_all_ack() {
        let result = classify_command("git worktree list && git worktree list");
        assert_eq!(
            result,
            CommandClassification::AlreadyCompact,
            "All-ack compound must be AlreadyCompact"
        );
    }

    #[test]
    fn test_classify_compound_any_nomatch() {
        let result = classify_command("git worktree list && echo done");
        assert_eq!(
            result,
            CommandClassification::Unhandled,
            "Any NoMatch segment in compound must make the whole thing Unhandled"
        );
    }

    #[test]
    fn test_classify_pipe_first_segment_rewritten() {
        let result = classify_command("git show HEAD | less");
        match result {
            CommandClassification::Rewritten(s) => {
                assert!(
                    s.contains("skim git show HEAD"),
                    "First pipe segment must be rewritten: {s}"
                );
                assert!(s.contains("| less"), "Pipe consumer must be preserved: {s}");
            }
            other => panic!("Expected Rewritten for pipe with rewritable first seg, got {other:?}"),
        }
    }

    #[test]
    fn test_classify_pipe_first_segment_ack() {
        let result = classify_command("git worktree list | wc -l");
        assert_eq!(
            result,
            CommandClassification::AlreadyCompact,
            "Pipe with AlreadyCompact first segment must be AlreadyCompact"
        );
    }

    #[test]
    fn test_classify_already_skim_returns_unhandled() {
        assert_eq!(
            classify_command("skim git show HEAD"),
            CommandClassification::Unhandled,
            "Already-skim commands must return Unhandled"
        );
    }

    #[test]
    fn test_classify_empty_returns_unhandled() {
        assert_eq!(
            classify_command(""),
            CommandClassification::Unhandled,
            "Empty input must return Unhandled"
        );
        assert_eq!(
            classify_command("   "),
            CommandClassification::Unhandled,
            "Whitespace-only input must return Unhandled"
        );
    }

    // ========================================================================
    // would_rewrite() API tests
    // ========================================================================

    #[test]
    fn test_would_rewrite_git_status_with_s() {
        assert_eq!(
            would_rewrite("git status -s"),
            Some("skim git status -s".to_string()),
            "git status -s should rewrite (handler strips -s)"
        );
    }

    #[test]
    fn test_would_rewrite_git_log_oneline() {
        let result = would_rewrite("git log --oneline -5");
        assert!(
            result.is_some(),
            "git log --oneline -5 should rewrite (handler strips --oneline)"
        );
        let rewritten = result.unwrap();
        assert!(
            rewritten.starts_with("skim git log"),
            "Expected 'skim git log ...' prefix, got: {rewritten}"
        );
    }

    #[test]
    fn test_would_rewrite_already_skim_returns_none() {
        assert_eq!(
            would_rewrite("skim git status"),
            None,
            "Already-skim commands must not be rewritten"
        );
    }

    #[test]
    fn test_would_rewrite_empty_returns_none() {
        assert_eq!(would_rewrite(""), None, "Empty input must return None");
        assert_eq!(
            would_rewrite("   "),
            None,
            "Whitespace-only input must return None"
        );
    }

    #[test]
    fn test_would_rewrite_non_rewritable_returns_none() {
        assert_eq!(
            would_rewrite("python3 -c 'print(1)'"),
            None,
            "python3 -c is not a rewritable pattern"
        );
    }

    /// `git diff --stat` now rewrites (--stat removed from skip list per AD-4).
    /// The diff handler detects --stat via user_has_flag and calls run_passthrough,
    /// so the user sees byte-identical git output.
    #[test]
    fn test_would_rewrite_git_diff_stat_rewrites() {
        let result = would_rewrite("git diff --stat");
        assert_eq!(
            result,
            Some("skim git diff --stat".to_string()),
            "git diff --stat must rewrite after AD-4 skip-list trim"
        );
    }

    #[test]
    fn test_would_rewrite_gh_pr_list_json_rewrites() {
        let result = would_rewrite("gh pr list --json number");
        assert!(result.is_some(), "gh pr list --json should now rewrite");
        let rewritten = result.unwrap();
        assert!(
            rewritten.contains("skim infra gh pr list"),
            "Expected 'skim infra gh pr list' in output, got: {rewritten}"
        );
    }

    #[test]
    fn test_would_rewrite_jest_rewrites() {
        assert_eq!(
            would_rewrite("jest src/"),
            Some("skim test jest src/".to_string()),
            "jest should rewrite to skim test jest"
        );
    }

    #[test]
    fn test_would_rewrite_npx_jest_rewrites() {
        assert_eq!(
            would_rewrite("npx jest src/"),
            Some("skim test jest src/".to_string()),
            "npx jest should rewrite to skim test jest"
        );
    }
}
