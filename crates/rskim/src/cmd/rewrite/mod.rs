//! Command rewrite engine (#43, #44)
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

mod compound;
mod engine;
mod handlers;
mod hook;
mod rules;
mod suggest;
mod types;

use std::io::{self, BufRead, IsTerminal, Read};
use std::process::ExitCode;

use compound::{split_compound, try_rewrite_compound};
use engine::try_rewrite;
use hook::{parse_agent_flag, run_hook_mode};
use suggest::{print_help, print_suggest};
use types::{CompoundSplitResult, RewriteCategory, RewriteResult};

// Re-export the clap command for completions.rs
pub(super) use suggest::command;

// ============================================================================
// Public API for other modules
// ============================================================================

/// Check if a command would be rewritten, returning the rewritten form.
///
/// Used by `discover` to avoid maintaining a separate heuristic that mirrors
/// the rewrite engine's declarative rule table.
///
/// Returns `Some(rewritten_command)` if the command matches a rewrite rule,
/// `None` if no rewrite applies (including skim commands, empty input, and
/// unsupported shell syntax).
pub(crate) fn would_rewrite(command: &str) -> Option<String> {
    let command = command.trim();
    if command.is_empty() || command.starts_with("skim ") {
        return None;
    }

    // Fast path: no compound operators — skip split_compound entirely.
    let has_operator_chars = command.contains("&&")
        || command.contains("||")
        || command.contains(';')
        || command.contains('|');

    if !has_operator_chars {
        let tokens: Vec<&str> = command.split_whitespace().collect();
        return try_rewrite(&tokens).map(|r| r.tokens.join(" "));
    }

    // Compound command handling
    match split_compound(command) {
        CompoundSplitResult::Bail => None,
        CompoundSplitResult::Simple(tokens) => {
            let refs: Vec<&str> = tokens.iter().map(|s| s.as_str()).collect();
            try_rewrite(&refs).map(|r| r.tokens.join(" "))
        }
        CompoundSplitResult::Compound(segments) => {
            try_rewrite_compound(&segments).map(|r| r.tokens.join(" "))
        }
    }
}

// ============================================================================
// Entry point
// ============================================================================

/// Run the `rewrite` subcommand. Returns the process exit code.
///
/// Exit code semantics:
/// - 0: rewrite found, printed to stdout (or hook mode always)
/// - 1: no rewrite match (or compound command, or invalid input)
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

    // Fast path: if no compound operator chars are present, skip split_compound
    // entirely and avoid the second tokenization pass.
    let has_operator_chars = original.contains("&&")
        || original.contains("||")
        || original.contains(';')
        || original.contains('|');
    if !has_operator_chars {
        let token_refs: Vec<&str> = tokens.iter().map(|s| s.as_str()).collect();
        let result = try_rewrite(&token_refs);
        return emit_rewrite_result(suggest_mode, &original, result, false);
    }

    // Split into compound segments (or simple if no operators found)
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
///
/// Joins the rewrite tokens and extracts the category, bridging the gap
/// between the internal `RewriteResult` type and the `emit_result` API.
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
