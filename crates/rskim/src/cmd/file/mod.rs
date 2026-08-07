//! File operations handler — dispatches to file tool parsers (#116)
//!
//! Called via flat dispatch: `skim <tool> [args...]`. Supported tools:
//! `df`, `diff`, `du`, `env`, `find`, `grep`, `ls`, `printenv`, `ps`, `rg`, `tree`, `wc`.
//!
//! ## Shared passthrough helpers
//!
//! [`passthrough_parse`] is the single implementation of the byte-faithful native
//! passthrough contract (ADR-009) for pure-passthrough file handlers (df, du,
//! find, grep, ps, rg, wc).  All modules delegate to it so a future fidelity
//! fix lands in one place.
//!
//! [`passthrough_config`] is the SINGLE write-point for `skip_ansi_strip: true`
//! across the whole passthrough family; [`run_passthrough_tool`] combines it with
//! [`super::run_tool`] so each handler's `run` function reduces to one call.

use super::{RunContext, ToolRunConfig, run_tool};
use crate::output::ParseResult;
use crate::output::canonical::FileResult;
use crate::runner::CommandOutput;

pub(crate) mod df;
pub(crate) mod diff;
pub(crate) mod du;
pub(crate) mod env;
pub(crate) mod find;
pub(crate) mod grep;
pub(crate) mod ls;
pub(crate) mod ps;
pub(crate) mod rg;
pub(crate) mod wc;

use std::process::ExitCode;

use super::extract_show_stats;

/// Known file tools that the file handler can dispatch to.
const KNOWN_TOOLS: &[&str] = &[
    "df", "diff", "du", "env", "find", "grep", "ls", "printenv", "ps", "rg", "tree", "wc",
];

/// Shared pure-passthrough parse helper for df, du, find, grep, ps, rg, wc,
/// and any future byte-faithful pass-through file handlers.
///
/// Returns [`ParseResult::RawPassthrough`] — a payload-less signal that
/// `execution.rs` should serve `CommandOutput::stdout` byte-faithfully without
/// cloning it into the parse result.  The byte-faithful contract (ADR-009) lives
/// in one place; a future fidelity fix (e.g., adjusting how TAB bytes are
/// handled) touches only this function, not N identical copies.
///
/// The `_output` parameter is intentionally ignored: the whole point of
/// `RawPassthrough` is that the parse result carries no payload — the original
/// `CommandOutput::stdout` buffer is served directly by the caller.
pub(super) fn passthrough_parse(_output: &CommandOutput) -> ParseResult<FileResult> {
    ParseResult::RawPassthrough
}

/// Per-tool variation for a pure-passthrough file handler.
///
/// Pass to [`passthrough_config`] or [`run_passthrough_tool`] so all
/// boilerplate — including the single write of `skip_ansi_strip: true` —
/// lives in exactly one place for the whole family.
pub(super) struct PassthroughSpec<'a> {
    /// Binary name (e.g. `"grep"`, `"wc"`).
    pub program: &'a str,
    /// Text printed when the tool binary is not found.
    pub install_hint: &'a str,
    /// Non-zero exit codes the tool exits on a benign outcome.
    ///
    /// `grep`/`rg` use `&[1]` (no match is not an error).
    /// Most passthrough tools use `&[]`.
    pub expected_exit_codes: &'a [i32],
}

/// Build a [`ToolRunConfig`] for a pure-passthrough file handler.
///
/// `skip_ansi_strip: true` is written **here and only here** for the whole
/// passthrough family.  The ANSI-strip step in `cmd/execution.rs` runs BEFORE
/// `parse()` and shadows the `output` binding, so a `RawPassthrough` result
/// serves the already-stripped bytes; any wrapper whose bytes reach the reader
/// unparsed MUST have this flag set to `true`.
///
/// This is a `const fn` so callers can use it in `const` contexts with zero
/// run-time allocation.  The 64 MiB ceiling is enforced upstream by the runner;
/// no second buffer is created here.
///
/// # Convention note
///
/// Rust cannot prevent a future author from hand-rolling a `ToolRunConfig`
/// literal and bypassing this constructor.  This is a convention backed by
/// `test_passthrough_config_always_sets_skip_ansi_strip` in this module —
/// not a compiler guarantee.
pub(super) const fn passthrough_config<'a>(spec: PassthroughSpec<'a>) -> ToolRunConfig<'a> {
    ToolRunConfig {
        program: spec.program,
        env_overrides: &[],
        install_hint: spec.install_hint,
        family: "file",
        skip_ansi_strip: true, // THE single write for the whole passthrough family
        command_type: crate::analytics::CommandType::FileOps,
        expected_exit_codes: spec.expected_exit_codes,
        forward_stderr: true,
        skip_net_savings_guard: false,
        synthesize_success_line: None,
        injected_format_flag: None,
    }
}

/// Execute a pure-passthrough file tool.
///
/// Combines [`passthrough_config`] and [`run_tool`] so each handler's `run`
/// function is a single call.  `parse_fn` is kept as a parameter — the 16
/// existing `test_parse_impl_is_passthrough` unit tests call each module's
/// `parse_impl` directly, keeping that symbol reachable and dead-code-lint-clean.
pub(super) fn run_passthrough_tool(
    spec: PassthroughSpec<'_>,
    args: &[String],
    ctx: &RunContext,
    parse_fn: impl FnOnce(&CommandOutput) -> ParseResult<FileResult>,
) -> anyhow::Result<std::process::ExitCode> {
    run_tool(passthrough_config(spec), args, ctx, |_| {}, parse_fn)
}

/// Maximum path/match entries shown in output (truncation cap).
pub(crate) const MAX_DISPLAY_ENTRIES: usize = 100;

/// Maximum lines accepted by structured parsers in this family.
///
/// Exceeding this bound never truncates: parsers return `None` so the caller
/// degrades to lossless `Passthrough` (raw output is already bounded at the
/// runner's 64 MiB cap). (#317)
pub(crate) const MAX_INPUT_LINES: usize = 100_000;

/// Entry point for `skim <tool> [args...]` (file handler).
///
/// If no tool is specified or `--help` is passed, prints usage and exits.
/// `-h` is intentionally NOT intercepted here: file tools use `-h` with
/// tool-level semantics (`grep -h` = no-filename, `ls -h`/`du -h`/`df -h` =
/// human-readable sizes), mirroring the `db/mod.rs` hostname-flag precedent.
/// Use `skim <tool> --help` to see this usage text.
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| a == "--help") {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let (filtered_args, show_stats) = extract_show_stats(args);
    let (filtered_args, json_output) = super::extract_json_flag(&filtered_args);

    let Some((tool_name, tool_args)) = filtered_args.split_first() else {
        print_help();
        return Ok(ExitCode::SUCCESS);
    };

    let ctx = super::RunContext {
        show_stats,
        json_output,
        analytics_enabled: analytics.enabled,
        session_id: analytics.session_id.clone(),
    };

    match tool_name.as_str() {
        "df" => df::run(tool_args, &ctx),
        "diff" => diff::run(tool_args, &ctx),
        "du" => du::run(tool_args, &ctx),
        "env" | "printenv" => env::run(tool_args, &ctx),
        "find" => find::run(tool_args, &ctx),
        "grep" => grep::run(tool_args, &ctx),
        "ls" => ls::run(tool_args, &ctx, "ls"),
        "ps" => ps::run(tool_args, &ctx),
        "rg" => rg::run(tool_args, &ctx),
        "tree" => ls::run(tool_args, &ctx, "tree"),
        "wc" => wc::run(tool_args, &ctx),
        _ => {
            let safe_tool = super::sanitize_for_display(tool_name);
            eprintln!(
                "skim: unknown tool '{safe_tool}'\n\
                 Available tools: {}\n\
                 Run 'skim <tool> --help' for usage information",
                KNOWN_TOOLS.join(", ")
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!("skim <tool> [args...]");
    println!();
    println!("  Run file operation tools and parse the output for AI context windows.");
    println!();
    println!("Available tools:");
    for tool in KNOWN_TOOLS {
        println!("  {tool}");
    }
    println!();
    println!("Flags:");
    println!("  --json          Emit structured JSON output");
    println!("  --show-stats    Show token statistics");
    println!();
    println!("Examples:");
    println!("  skim find . -name '*.rs'       Find Rust files");
    println!("  skim ls -la                    List files with details");
    println!("  skim tree src/                 Directory tree");
    println!("  skim grep -rn 'TODO' src/      Grep recursively");
    println!("  skim rg 'fn main' src/         Ripgrep search");
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_for_display_clean_input() {
        assert_eq!(crate::cmd::sanitize_for_display("find"), "find");
    }

    #[test]
    fn test_sanitize_for_display_rejects_non_ascii() {
        let input = "tool\x1b[31mred\x1b[0m";
        let sanitized = crate::cmd::sanitize_for_display(input);
        assert!(!sanitized.contains('\x1b'));
    }

    // ========================================================================
    // passthrough_config: reintroduction guard
    // ========================================================================

    /// Verify that every call to `passthrough_config` produces a config with
    /// `skip_ansi_strip: true`.
    ///
    /// Because all passthrough tools call this constructor (via
    /// `run_passthrough_tool`), a newly added passthrough tool is covered
    /// automatically — no new assertion needed.
    ///
    /// NOTE: This is a convention backed by this audit test, not a compiler
    /// guarantee.  A future author can hand-roll a `ToolRunConfig` literal
    /// and bypass the constructor.
    #[test]
    fn test_passthrough_config_always_sets_skip_ansi_strip() {
        let spec = PassthroughSpec {
            program: "test-tool",
            install_hint: "install hint",
            expected_exit_codes: &[],
        };
        let config = passthrough_config(spec);
        assert!(
            config.skip_ansi_strip,
            "passthrough_config must always set skip_ansi_strip: true — \
             the ANSI-strip step in execution.rs runs before parse() and \
             shadows the output binding; RawPassthrough serves those stripped \
             bytes, so any unparsed passthrough MUST set this flag"
        );
    }

    /// Verify that `expected_exit_codes` passes through the constructor unchanged.
    ///
    /// `grep` and `rg` exit 1 on no match — this is benign.  Dropping `&[1]`
    /// from their spec silently reclassifies exit 1 as `UnexpectedFailure`, a
    /// real regression.  The test covers both the grep/rg case (`&[1]`) and the
    /// common case (`&[]`) to confirm neither inherits the other's codes.
    #[test]
    fn test_passthrough_config_expected_exit_codes_pass_through() {
        // grep and rg: exit 1 == "no match" (benign)
        for program in &["grep", "rg"] {
            let config = passthrough_config(PassthroughSpec {
                program,
                install_hint: "hint",
                expected_exit_codes: &[1],
            });
            assert_eq!(
                config.expected_exit_codes,
                &[1],
                "{program} config must carry expected_exit_codes: [1]"
            );
        }

        // A tool with no special exit codes must not inherit &[1]
        let config = passthrough_config(PassthroughSpec {
            program: "wc",
            install_hint: "hint",
            expected_exit_codes: &[],
        });
        assert!(
            config.expected_exit_codes.is_empty(),
            "wc config must not carry any expected_exit_codes"
        );
    }
}
