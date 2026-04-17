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
//! **Tri-state classification** (`classify_command`, AD-RW-2): Exposes a richer
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
use engine::{matches_catch_all_rule, try_rewrite};
use hook::{parse_agent_flag, run_hook_mode};
use suggest::{print_help, print_suggest};
use types::{CommandSegment, CompoundOp, CompoundSplitResult, RewriteCategory, RewriteResult};

// Re-export the clap command for completions.rs
pub(super) use suggest::command;

// ============================================================================
// Public API for other modules
// ============================================================================

/// Tri-state classification of a shell command (AD-RW-2).
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
/// # AD-RW-3 CLI behavior
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
        CompoundSplitResult::Compound(segments) => classify_compound(&segments),
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
///
/// # Mixed-compound semantics (AD-RW-2 / regression-2)
///
/// For compound commands containing a segment with no match, this function
/// returns `None` — even if other segments would rewrite successfully.
/// This changed from the old behavior (which could return `Some` for
/// "any-match wins").  The new rule is:
///
/// - `"cargo test && cargo clippy"` → `Some(...)` (all segments rewrite)
/// - `"cargo test && echo done"` → `None` (one segment is `Unhandled`)
/// - `"git worktree list && cargo test"` → `Some(...)` (AlreadyCompact + Rewritten)
///
/// If you need the full tri-state result, call `classify_command` directly.
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
/// Single-pass byte scan: stops at the first compound-operator byte rather than
/// doing four independent `contains` calls across the full string.
fn has_compound_operators(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'|' | b';' => return true,
            b'&' if bytes.get(i + 1) == Some(&b'&') => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// Classification result for a single command segment (not a full command string).
#[derive(Debug, Clone)]
enum SegmentClassification {
    /// Segment matches a rewrite rule — store the rewritten tokens.
    Rewritten(Vec<String>),
    /// Segment is acknowledged compact — store original tokens for passthrough.
    AlreadyCompact(Vec<String>),
    /// No rule matched. Carries no payload: callers that reach this branch
    /// return `Unhandled` immediately without inspecting the tokens.
    NoMatch,
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
///
/// The `owned: Vec<String>` clone is deferred to the branches that actually need
/// it (`AlreadyCompact`). On the hot `Rewritten` path the engine already returns
/// its own token vector, so no additional clone is required. `NoMatch` carries no
/// payload because all call sites return `Unhandled` immediately on that branch.
fn classify_segment_fine(tokens: &[&str]) -> SegmentClassification {
    if is_segment_ack(tokens) {
        let owned: Vec<String> = tokens.iter().map(|s| s.to_string()).collect();
        return SegmentClassification::AlreadyCompact(owned);
    }
    match try_rewrite(tokens) {
        Some(r) => SegmentClassification::Rewritten(r.tokens),
        None => SegmentClassification::NoMatch,
    }
}

/// Per-segment classification tuple used in `classify_compound`.
///
/// Carries the segment classification, the trailing operator, and a reference
/// to the stripped redirects so they can be spliced back in Pass 3.
type ClassifiedSegment<'a> = (SegmentClassification, Option<CompoundOp>, &'a [(usize, String)]);

/// Classify a compound command (segments connected by `&&`, `||`, `;`, `|`).
///
/// Rules (per AD-RW-2):
/// - Any `NoMatch` segment → `Unhandled` (a compression gap exists).
/// - All `AlreadyCompact` → `AlreadyCompact`.
/// - Mix of `Rewritten` + `AlreadyCompact` → `Rewritten(reconstructed)`.
///
/// Pipe policy (mirrors `try_rewrite_compound_pipe`): only the first segment
/// of a pipe expression is classified for rewriting. Subsequent pipe stages
/// are left unchanged. This prevents wrapping `git diff | less` into
/// `skim git diff | skim less`.
///
/// Implementation uses a three-pass approach to eliminate mutable flags:
/// 1. Classify every segment into a `Vec<SegmentClassification>`.
/// 2. Early-return `Unhandled` if any segment is `NoMatch`.
/// 3. Reconstruct the compound string from all classified segments.
fn classify_compound(segments: &[CommandSegment]) -> CommandClassification {
    if segments.is_empty() {
        return CommandClassification::Unhandled;
    }

    // Check if this is a pipe expression (any segment has a Pipe operator).
    let has_pipe = segments
        .iter()
        .any(|s| s.trailing_operator == Some(CompoundOp::Pipe));

    if has_pipe {
        return classify_compound_pipe(segments);
    }

    // Pass 1: classify all segments.
    // The tuple carries `stripped_redirects` so Pass 3 can splice them back
    // into the reconstructed command string (Issue #2 / AD-RW-2).
    let classified: Vec<ClassifiedSegment<'_>> = segments
        .iter()
        .map(|seg| {
            let token_refs: Vec<&str> = seg.tokens.iter().map(|s| s.as_str()).collect();
            (
                classify_segment_fine(&token_refs),
                seg.trailing_operator,
                seg.stripped_redirects.as_slice(),
            )
        })
        .collect();

    // Pass 2: early-exit on any NoMatch.
    if classified
        .iter()
        .any(|(c, _, _)| matches!(c, SegmentClassification::NoMatch))
    {
        return CommandClassification::Unhandled;
    }

    // Pass 3: reconstruct compound string; track whether any segment rewrote.
    let any_rewritten = classified
        .iter()
        .any(|(c, _, _)| matches!(c, SegmentClassification::Rewritten(_)));

    if !any_rewritten {
        return CommandClassification::AlreadyCompact;
    }

    let mut parts: Vec<String> = Vec::new();
    for (classification, op, redirects) in classified {
        let segment_text = match classification {
            SegmentClassification::Rewritten(mut tokens)
            | SegmentClassification::AlreadyCompact(mut tokens) => {
                // Splice stripped redirects back so they are not silently lost.
                // Mirrors the pattern in `try_rewrite_compound` / compound.rs.
                // SEE: AD-RW-2 (Issue #2).
                for (_idx, tok) in redirects {
                    tokens.push(tok.clone());
                }
                tokens.join(" ")
            }
            // NoMatch is unreachable here: Pass 2 already returned Unhandled if
            // any segment was NoMatch. Kept for exhaustiveness.
            SegmentClassification::NoMatch => unreachable!("NoMatch filtered in Pass 2"),
        };
        parts.push(segment_text);
        if let Some(op) = op {
            parts.push(op.as_str().to_string());
        }
    }

    CommandClassification::Rewritten(parts.join(" "))
}

/// Classify a pipe expression.
///
/// Only the first segment (output producer) is considered for rewriting.
/// If the first segment is `AlreadyCompact`, the whole pipe is `AlreadyCompact`.
/// If the first segment is `NoMatch` (or unclassified), the whole pipe is `Unhandled`.
fn classify_compound_pipe(segments: &[CommandSegment]) -> CommandClassification {
    if segments.is_empty() {
        return CommandClassification::Unhandled;
    }

    let first = &segments[0];
    let token_refs: Vec<&str> = first.tokens.iter().map(|s| s.as_str()).collect();

    // Do not classify catch-all rules on the pipe-source side (e.g. `ls | wc -l`).
    // Mirrors the same check in `try_rewrite_compound_pipe`.  The `is_catch_all`
    // flag on the matching rule replaces the removed PIPE_EXCLUDED_SOURCES constant.
    // SEE: AD-RW-2.
    if matches_catch_all_rule(&token_refs) {
        return CommandClassification::Unhandled;
    }

    let first_classification = classify_segment_fine(&token_refs);

    match first_classification {
        SegmentClassification::AlreadyCompact(_) => CommandClassification::AlreadyCompact,
        SegmentClassification::NoMatch => CommandClassification::Unhandled,
        SegmentClassification::Rewritten(mut rewritten_tokens) => {
            // Reconstruct: rewritten first segment | rest unchanged.
            // Splice redirects back for the first segment (Issue #2 / AD-RW-2).
            for (_idx, tok) in &first.stripped_redirects {
                rewritten_tokens.push(tok.clone());
            }
            let mut parts: Vec<String> = Vec::new();
            parts.push(rewritten_tokens.join(" "));

            for (idx, seg) in segments.iter().enumerate() {
                if idx == 0 {
                    if let Some(op) = seg.trailing_operator {
                        parts.push(op.as_str().to_string());
                    }
                    continue;
                }
                // Restore redirects for non-rewritten pipe segments.
                let mut seg_tokens = seg.tokens.clone();
                for (_i, tok) in &seg.stripped_redirects {
                    seg_tokens.push(tok.clone());
                }
                parts.push(seg_tokens.join(" "));
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
/// Exit code semantics (AD-RW-3):
/// - 0: rewrite found (printed to stdout), or AlreadyCompact (original printed to stdout)
/// - 1: no rewrite match (Unhandled) or invalid input
///
/// Control flow shape:
/// 1. `--help` / `--hook` are handled before touching tokens.
/// 2. `collect_input_tokens` reads from positional args or stdin.
/// 3. `run_classify_and_emit` classifies once and dispatches on the tri-state.
pub(crate) fn run(
    args: &[String],
    _analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    // Handle --help / -h
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Hook mode: run as agent PreToolUse hook (#44)
    if args.iter().any(|a| a == "--hook") {
        let agent = parse_agent_flag(args);
        return run_hook_mode(agent);
    }

    let suggest_mode = args.first().is_some_and(|a| a == "--suggest");
    let positional_start = if suggest_mode { 1 } else { 0 };
    let positional_args: Vec<&str> = args[positional_start..]
        .iter()
        .map(|s| s.as_str())
        .collect();

    let tokens = match collect_input_tokens(&positional_args)? {
        Some(t) => t,
        None => return emit_result(suggest_mode, "", None, false),
    };

    run_classify_and_emit(suggest_mode, &tokens)
}

/// Collect command tokens from positional args or a single stdin line.
///
/// Returns `Ok(None)` when there is nothing to classify (empty input or
/// interactive stdin), and `Ok(Some(tokens))` otherwise.
///
/// # Design decision (2026-04-11, AD-RW-13)
/// Positional args are flattened via `split_whitespace` so that both shell
/// invocation shapes produce the same token sequence:
///
/// - `skim rewrite prettier --check src/`       → 3 args → 3 tokens
/// - `skim rewrite 'prettier --check src/'`     → 1 arg  → 3 tokens
///
/// Without the flatten, the second form would classify as a single-token
/// command `"prettier --check src/"` which matches no rule and no ACK prefix,
/// silently returning `Unhandled` (observed as empty stdout with exit 1 in
/// user-facing scenarios). The flatten is safe because shell-level argument
/// splitting removes whitespace at token boundaries — any whitespace inside
/// a single arg is present either because the user quoted a whole command
/// string (the intended case) or because the user quoted a value containing
/// whitespace (e.g., `--format='%H %s'`). The second case is rare in
/// rewrite-triggering commands and the passthrough path still handles it
/// downstream.
fn collect_input_tokens(positional_args: &[&str]) -> anyhow::Result<Option<Vec<String>>> {
    if positional_args.is_empty() {
        // Try reading from stdin if it's piped
        if io::stdin().is_terminal() {
            return Ok(None);
        }
        // Read one line from stdin, capped at 4 KiB to prevent unbounded allocation.
        // Uses take() to bound memory before reading, so even input without a newline
        // cannot cause unbounded allocation.
        let mut line = String::new();
        io::BufReader::new(io::stdin().lock().take(4096)).read_line(&mut line)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        return Ok(Some(trimmed.split_whitespace().map(String::from).collect()));
    }
    let tokens: Vec<String> = positional_args
        .iter()
        .flat_map(|s| s.split_whitespace())
        .map(String::from)
        .collect();
    if tokens.is_empty() {
        return Ok(None);
    }
    Ok(Some(tokens))
}

/// Classify `tokens` and emit the result.
///
/// This is the single dispatch point after input collection. It handles the
/// three branches (simple / compound-bail / compound-match) uniformly via
/// `emit_result` / `emit_rewrite_result`.
///
/// For compound commands (`&&`, `||`, `;`, `|`), the CLI uses
/// `try_rewrite_compound` semantics (any-match wins, leaving unmatched
/// segments as-is), which is distinct from `classify_command` used by
/// `discover` for per-segment gap detection.
///
/// # DESIGN NOTE — intentional CLI / discover split
///
/// The CLI (`rewrite` subcommand) deliberately uses `try_rewrite_compound`
/// (binary match/no-match) for compound commands rather than the tri-state
/// `classify_command`. Reasons:
///
/// 1. **User-visible output contract**: the CLI prints either a rewritten
///    command or exits 1. A third "AlreadyCompact" state would change the
///    contract for users who rely on exit codes in shell scripts.
/// 2. **`classify_command` is discover-only**: it was introduced to give
///    `discover` fine-grained gap detection (AD-RW-2). Its `AlreadyCompact`
///    variant has no meaningful mapping to CLI exit codes.
///
/// If the CLI contract is ever extended (e.g. exit code 2 for AlreadyCompact),
/// this function can be migrated to `classify_command` and the simple-command
/// fast-path below already uses it implicitly via `is_segment_ack`.
fn run_classify_and_emit(suggest_mode: bool, tokens: &[String]) -> anyhow::Result<ExitCode> {
    let original = tokens.join(" ");

    // Fast path: if no compound operator chars are present, use classify_command
    // which also handles the AlreadyCompact case (AD-RW-3).
    if !has_compound_operators(&original) {
        let token_refs: Vec<&str> = tokens.iter().map(|s| s.as_str()).collect();

        // Check AlreadyCompact first (acknowledged-compact commands, AD-RW-2/AD-RW-3).
        if is_segment_ack(&token_refs) {
            if suggest_mode {
                // AlreadyCompact is not a rewrite match — report as no-match in suggest.
                print_suggest(&original, None, false);
                return Ok(ExitCode::SUCCESS);
            }
            // AD-RW-3: print original command unchanged, exit 0.
            println!("{original}");
            return Ok(ExitCode::SUCCESS);
        }

        // Normal rewrite path — uses the real RewriteResult (with correct category).
        let result = try_rewrite(&token_refs);
        return emit_rewrite_result(suggest_mode, &original, result.as_ref(), false);
    }

    // Compound commands: use original try_rewrite_compound semantics (any match wins).
    // AlreadyCompact detection for compound commands is provided by classify_command
    // for discover purposes, not for the CLI rewrite subcommand.
    match split_compound(&original) {
        CompoundSplitResult::Bail => emit_result(suggest_mode, &original, None, false),
        CompoundSplitResult::Simple(simple_tokens) => {
            let token_refs: Vec<&str> = simple_tokens.iter().map(|s| s.as_str()).collect();
            let result = try_rewrite(&token_refs);
            emit_rewrite_result(suggest_mode, &original, result.as_ref(), false)
        }
        CompoundSplitResult::Compound(segments) => {
            let result = try_rewrite_compound(&segments);
            emit_rewrite_result(suggest_mode, &original, result.as_ref(), true)
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
    result: Option<&RewriteResult>,
    compound: bool,
) -> anyhow::Result<ExitCode> {
    let rewritten = result.map(|r| r.tokens.join(" "));
    let match_info = result
        .zip(rewritten.as_ref())
        .map(|(r, s)| (s.as_str(), r.category));
    emit_result(suggest_mode, original, match_info, compound)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // classify_command() — tri-state API tests (AD-RW-2)
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

    /// Stripped redirects must survive classify_compound reconstruction (Issue #2 / AD-RW-2).
    ///
    /// `cargo test 2>&1 && cargo build` — the `2>&1` is stripped before rule matching
    /// and must be spliced back into the rewritten compound string so it is not
    /// silently dropped from the discover suggestion.
    #[test]
    fn test_classify_compound_preserves_stripped_redirects() {
        let result = classify_command("cargo test 2>&1 && cargo build");
        match result {
            CommandClassification::Rewritten(s) => {
                assert!(
                    s.contains("2>&1"),
                    "Stripped redirect must be preserved in rewritten compound: {s}"
                );
            }
            other => panic!("Expected Rewritten, got {other:?}"),
        }
    }

    /// Stripped redirects must survive classify_compound_pipe reconstruction (Issue #2).
    ///
    /// `cargo test 2>&1 | head` — the `2>&1` is stripped before rule matching
    /// and must be spliced back into the rewritten pipe command.
    #[test]
    fn test_classify_compound_pipe_preserves_stripped_redirects() {
        let result = classify_command("cargo test 2>&1 | head");
        match result {
            CommandClassification::Rewritten(s) => {
                assert!(
                    s.contains("2>&1"),
                    "Stripped redirect must be preserved in rewritten pipe: {s}"
                );
                assert!(s.contains("| head"), "Pipe consumer must be preserved: {s}");
            }
            other => panic!("Expected Rewritten, got {other:?}"),
        }
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

    /// `git diff --stat` now rewrites (--stat removed from skip list per AD-RW-4).
    /// The diff handler detects --stat via user_has_flag and calls run_passthrough,
    /// so the user sees byte-identical git output.
    #[test]
    fn test_would_rewrite_git_diff_stat_rewrites() {
        let result = would_rewrite("git diff --stat");
        assert_eq!(
            result,
            Some("skim git diff --stat".to_string()),
            "git diff --stat must rewrite after AD-RW-4 skip-list trim"
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

    /// Regression test for mixed-compound semantics (regression-2 / AD-RW-2).
    ///
    /// `would_rewrite` wraps `classify_command`, which returns `Unhandled` when
    /// ANY segment of a compound command has no match.  A compound like
    /// `"cargo test && echo done"` has one rewritable segment (`cargo test`) and
    /// one unhandled segment (`echo done`), so `classify_command` returns
    /// `Unhandled` and `would_rewrite` returns `None`.
    ///
    /// This is intentional: `would_rewrite` is a conservative API — `None` means
    /// "the full compound cannot be cleanly rewritten".  Callers that need
    /// per-segment resolution should use `classify_command` directly.
    #[test]
    fn test_would_rewrite_mixed_compound_returns_none() {
        // One rewritable segment + one unhandled segment → None.
        assert_eq!(
            would_rewrite("cargo test && echo done"),
            None,
            "Mixed compound with an unhandled segment must return None"
        );
        // Sanity: pure-rewritable compound still returns Some.
        assert!(
            would_rewrite("cargo test && cargo clippy").is_some(),
            "All-rewritable compound must return Some"
        );
    }

    // ========================================================================
    // has_compound_operators() — byte-scanner edge cases
    // ========================================================================

    #[test]
    fn test_has_compound_operators_empty() {
        assert!(!has_compound_operators(""), "empty string has no operators");
    }

    #[test]
    fn test_has_compound_operators_single_char_no_op() {
        assert!(!has_compound_operators("a"), "single non-op char");
        assert!(!has_compound_operators("x"), "single non-op char x");
    }

    #[test]
    fn test_has_compound_operators_pipe() {
        assert!(has_compound_operators("git log | less"), "| is an operator");
        assert!(has_compound_operators("|"), "bare | is an operator");
    }

    #[test]
    fn test_has_compound_operators_semicolon() {
        assert!(has_compound_operators("echo a; echo b"), "; is an operator");
        assert!(has_compound_operators(";"), "bare ; is an operator");
    }

    #[test]
    fn test_has_compound_operators_double_ampersand() {
        assert!(
            has_compound_operators("cargo test && cargo clippy"),
            "&& is an operator"
        );
        assert!(has_compound_operators("&&"), "bare && is an operator");
    }

    #[test]
    fn test_has_compound_operators_single_ampersand_is_not_compound() {
        // A lone `&` (background job) is intentionally NOT treated as a
        // compound operator by this scanner; only `&&` triggers it.
        assert!(
            !has_compound_operators("cargo test &"),
            "trailing single & is not a compound operator"
        );
        assert!(
            !has_compound_operators("&"),
            "bare single & is not a compound operator"
        );
    }

    #[test]
    fn test_has_compound_operators_double_pipe() {
        // `||` starts with `|` which is immediately detected as an operator.
        assert!(
            has_compound_operators("cmd1 || cmd2"),
            "|| contains | which is an operator"
        );
    }

    #[test]
    fn test_has_compound_operators_pipe_ampersand_combo() {
        // `|&` starts with `|` — detected on the first byte.
        assert!(
            has_compound_operators("cmd |& tee out.txt"),
            "|& starts with | which is an operator"
        );
    }

    #[test]
    fn test_has_compound_operators_lookahead_at_end() {
        // `bytes.get(i + 1) == Some(&b'&')` must return false (not panic)
        // when the trailing byte is a lone `&` at end-of-string.
        assert!(
            !has_compound_operators("cmd &"),
            "trailing lone & without a second & is not an operator"
        );
        // But trailing `&&` is valid.
        assert!(
            has_compound_operators("cmd &&"),
            "trailing && is a compound operator"
        );
    }

    #[test]
    fn test_has_compound_operators_plain_command() {
        assert!(
            !has_compound_operators("git status"),
            "plain command has no compound operator"
        );
        assert!(
            !has_compound_operators("cargo test --lib"),
            "cargo test with flags has no compound operator"
        );
    }

    // ========================================================================
    // collect_input_tokens() — edge-case coverage (AD-RW-13)
    // ========================================================================

    /// Helper: invoke collect_input_tokens with a set of &str positional args.
    fn tokens_from(args: &[&str]) -> Option<Vec<String>> {
        collect_input_tokens(args).expect("collect_input_tokens must not error")
    }

    /// Empty positional args list with no stdin → returns None.
    ///
    /// Note: this test is only meaningful when stdin is not a pipe (i.e. when
    /// running interactively).  In CI, stdin is typically not a TTY so the
    /// function reads stdin; passing an empty slice here avoids that branch.
    /// The test verifies the `tokens.is_empty()` guard inside the function.
    #[test]
    fn test_collect_input_tokens_empty_slice_is_none() {
        // An all-whitespace single arg produces no tokens → None.
        assert_eq!(
            tokens_from(&["   "]),
            None,
            "all-whitespace single arg must return None"
        );
    }

    /// Convert a `&[&str]` literal into `Vec<String>` for assertion comparisons.
    fn sv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    /// Single multi-word quoted arg tokenizes the same as equivalent multi-arg form.
    ///
    /// Regression for the AD-RW-13 fix: `skim rewrite 'prettier --check src/'`
    /// (shell passes one arg) must tokenize identically to
    /// `skim rewrite prettier --check src/` (three separate args).
    #[test]
    fn test_collect_input_tokens_single_quoted_equals_multi_arg() {
        let single = tokens_from(&["prettier --check src/"]);
        let multi = tokens_from(&["prettier", "--check", "src/"]);
        assert_eq!(
            single, multi,
            "single-quoted arg must produce same tokens as multi-arg form"
        );
        assert_eq!(
            single,
            Some(sv(&["prettier", "--check", "src/"])),
            "expected 3 tokens"
        );
    }

    /// Tab characters inside a single arg are treated as whitespace (split_whitespace).
    #[test]
    fn test_collect_input_tokens_tab_as_whitespace() {
        let result = tokens_from(&["cargo\ttest"]);
        assert_eq!(
            result,
            Some(sv(&["cargo", "test"])),
            "tab must be treated as whitespace"
        );
    }

    /// Multiple consecutive spaces inside a single arg collapse to one split boundary.
    #[test]
    fn test_collect_input_tokens_consecutive_spaces() {
        let result = tokens_from(&["cargo  test  --release"]);
        assert_eq!(
            result,
            Some(sv(&["cargo", "test", "--release"])),
            "consecutive spaces must collapse to single boundaries"
        );
    }

    /// Mixed quoted + bare args: flat_map over all positional args.
    ///
    /// `skim rewrite 'cargo test' --extra` produces positional args
    /// `["cargo test", "--extra"]`, which should flat_map to
    /// `["cargo", "test", "--extra"]`.
    #[test]
    fn test_collect_input_tokens_mixed_quoted_and_bare() {
        let result = tokens_from(&["cargo test", "--extra"]);
        assert_eq!(
            result,
            Some(sv(&["cargo", "test", "--extra"])),
            "mixed quoted + bare args must flat_map to unified token list"
        );
    }

    /// Empty string arg inside a multi-arg slice contributes no tokens.
    #[test]
    fn test_collect_input_tokens_empty_string_arg_ignored() {
        // ["", "cargo", "test"] → the empty arg contributes nothing.
        let result = tokens_from(&["", "cargo", "test"]);
        assert_eq!(
            result,
            Some(sv(&["cargo", "test"])),
            "empty string arg must contribute no tokens"
        );
    }

    /// Single non-empty arg with no spaces produces a single-token result.
    #[test]
    fn test_collect_input_tokens_single_word() {
        let result = tokens_from(&["pytest"]);
        assert_eq!(
            result,
            Some(sv(&["pytest"])),
            "single word must produce single token"
        );
    }

    /// All-whitespace multi-arg slice produces None.
    #[test]
    fn test_collect_input_tokens_all_whitespace_multi() {
        let result = tokens_from(&[" ", "\t", "  "]);
        assert_eq!(
            result, None,
            "all-whitespace multi-arg must return None (no tokens)"
        );
    }
}
