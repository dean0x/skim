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
//!
//! ## Two sinks, one entry point
//!
//! [`run_passthrough_tool`] is the single entry point for the family, and it is
//! also where the buffered-vs-streamed decision lives.  It knows *statically*
//! that the tool is pure passthrough — `parse_impl` always returns
//! `ParseResult::RawPassthrough` — without needing `parse()` to have run, which
//! is exactly what lets the default path stream (`passthrough_stream`, #495)
//! instead of buffering the child's whole stdout.  See
//! [`choose_passthrough_sink`] for the four cases that still need the complete
//! string.

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
mod passthrough_stream;
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
/// literal and bypassing this constructor, so this is a convention, not a
/// compiler guarantee.  Two things back it up:
///
/// - `test_passthrough_config_always_sets_skip_ansi_strip` (this module) pins
///   the constructor itself, so the flag cannot be flipped here.
/// - a `debug_assert!` in `cmd/execution.rs` (just after `parse()`) rejects any
///   `RawPassthrough` result produced under `skip_ansi_strip: false`, which is
///   what catches a hand-rolled bypass — in any family, not just this one.
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
        // `parse_impl` always returns `RawPassthrough`, so the net-savings guard
        // never runs for any passthrough-family tool; `false` is the semantically
        // correct default (ADR-001 — guard is for tools that might emit a larger
        // compressed view than the raw; passthrough has no compressed view at all).
        skip_net_savings_guard: false,
        synthesize_success_line: None,
        injected_format_flag: None,
        raw_override: None,
        never_passthrough: false,
    }
}

/// Which sink serves a pure-passthrough run.
///
/// See [`choose_passthrough_sink`] for why the second one still exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub(super) enum PassthroughSink {
    /// This family's own streaming sink
    /// ([`passthrough_stream::run_passthrough_streamed`]).
    Streamed,
    /// Hand the run to the shared `execution::run_tool` pipeline.
    ///
    /// Named for what it does in three of its four cases.  The fourth,
    /// `SKIM_PASSTHROUGH=1`, is *not* buffered: `execution.rs` has its own
    /// streamed escape hatch with a stricter byte contract, and routing here is
    /// how the family defers to it.  See [`choose_passthrough_sink`].
    Buffered,
}

/// Inputs to the buffered-vs-streamed decision.
///
/// A struct rather than four positional `bool`s so a transposed argument at the
/// call site is a compile error rather than a silent routing bug.
#[derive(Debug, Clone, Copy)]
pub(super) struct SinkInputs {
    /// `--json` was requested.
    pub json_output: bool,
    /// `--show-stats` was requested.
    pub show_stats: bool,
    /// The tool's input comes from stdin, not from a spawned child.
    pub reads_stdin: bool,
    /// `SKIM_PASSTHROUGH=1` is set.
    pub passthrough_mode: bool,
}

/// Decide which sink a pure-passthrough run uses.
///
/// Streaming is the default for this family: `parse_impl` always returns
/// `ParseResult::RawPassthrough`, so there is no compressed view and therefore
/// nothing the ADR-001 net-savings guard could compare.  Four cases route to
/// `execution::run_tool` instead:
///
/// - **`json_output`** — the `{"tier":"passthrough","raw":…}` envelope has to
///   embed the whole body as a JSON string; it cannot be emitted incrementally.
/// - **`show_stats`** — `record_and_report` tokenizes the raw and compressed
///   strings to display a token count.  The streamed sink never holds either, so
///   approximating would silently change a user-visible number.  This exclusion
///   exists specifically to keep that number identical.
/// - **`reads_stdin`** — there is no child process to stream from.
/// - **`passthrough_mode`** — a *byte-contract* exclusion, not a buffering one.
///
/// # Why `passthrough_mode` stays in the condition
///
/// `SKIM_PASSTHROUGH=1` now streams too — `execution::stream_passthrough_raw`
/// drives the same shared pump this family does — so "streaming is a separate
/// change" is no longer the reason.  The reason is that the two sinks have
/// **different byte contracts**, and the escape hatch needs the stricter one:
///
/// | | family sink | escape hatch |
/// |---|---|---|
/// | trailing-newline guard | on (parity with `emit_raw_passthrough`) | **off — byte-exact** |
/// | notices | tier + exit-disposition notices | none |
/// | analytics | records a `passthrough`/`raw` row | records nothing |
///
/// Dropping the field would serve `SKIM_PASSTHROUGH=1 skim grep …` from the
/// family sink, which appends a newline the raw tool never emitted — precisely
/// the divergence the escape hatch exists to escape.
/// `t14_escape_hatch_does_not_append_a_trailing_newline` pins this.
///
/// Pure and `const` so the routing rule is unit-testable without spawning
/// anything.
pub(super) const fn choose_passthrough_sink(inputs: SinkInputs) -> PassthroughSink {
    if inputs.json_output || inputs.show_stats || inputs.reads_stdin || inputs.passthrough_mode {
        PassthroughSink::Buffered
    } else {
        PassthroughSink::Streamed
    }
}

/// Execute a pure-passthrough file tool.
///
/// Routes to the streaming sink ([`passthrough_stream::run_passthrough_streamed`])
/// or the buffered one ([`passthrough_config`] + [`run_tool`]) per
/// [`choose_passthrough_sink`].  Branching *here* rather than inside
/// `execution.rs` is what makes streaming safe: this is the single entry point
/// for the family, and it knows *statically* that the tool is pure passthrough
/// without needing `parse()` to have run.
///
/// `parse_fn` is kept as a parameter — the buffered sink needs it, and the 16
/// existing `test_parse_impl_is_passthrough` unit tests call each module's
/// `parse_impl` directly, keeping that symbol reachable and dead-code-lint-clean.
pub(super) fn run_passthrough_tool(
    spec: PassthroughSpec<'_>,
    args: &[String],
    ctx: &RunContext,
    parse_fn: impl FnOnce(&CommandOutput) -> ParseResult<FileResult>,
) -> anyhow::Result<std::process::ExitCode> {
    let sink = choose_passthrough_sink(SinkInputs {
        json_output: ctx.json_output,
        show_stats: ctx.show_stats,
        reads_stdin: super::should_read_stdin(args),
        passthrough_mode: super::is_passthrough_mode(),
    });

    match sink {
        PassthroughSink::Streamed => passthrough_stream::run_passthrough_streamed(&spec, args, ctx),
        PassthroughSink::Buffered => {
            run_tool(passthrough_config(spec), args, ctx, |_| {}, parse_fn)
        }
    }
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
        "env" | "printenv" => {
            // D2: when any arg contains '=' this is a VAR=value assignment invocation:
            //   env FOO=1 npm test  →  exec the real `env` binary unchanged (B2)
            // The env handler (CONFIG.program = "printenv") cannot understand VAR=val
            // syntax — it would forward the tokens as file arguments to printenv.
            // (Consistent with skip_if_middle_contains_eq on the rewrite surface.)
            //
            // SECURITY (PF-012 / testing-14): when the child program is itself
            // `env` or `printenv` (e.g. `env FOO=1 printenv`), routing to the
            // real `env` binary would execute `printenv` unmediated — leaking
            // the environment including redaction-mandatory keys.  Route those
            // shapes to the skim env handler instead.
            //
            // ACCEPTED LIMITATION: `env FOO=1 sh -c env` still leaks because
            // the child is `sh` and skim cannot inspect what an arbitrary child
            // will print.  This shape is pinned in the regression test suite.
            if tool_name.as_str() == "env" && tool_args.iter().any(|a| a.contains('=')) {
                // Find the first non-assignment arg (the child program name).
                let child_idx = tool_args.iter().position(|a| !a.contains('='));
                let child_prog = child_idx
                    .and_then(|i| tool_args.get(i))
                    .map(String::as_str);
                if matches!(child_prog, Some("env") | Some("printenv")) {
                    // Child is env/printenv: pass the args AFTER the child name
                    // to the redacting handler.  The VAR=val overrides are not
                    // applied (skim calls `printenv` directly, not via `env`),
                    // but the output is fully redacted — the security property.
                    let after_child: Vec<String> = child_idx
                        .map(|i| tool_args[i + 1..].to_vec())
                        .unwrap_or_default();
                    env::run(&after_child, &ctx)
                } else {
                    // General case (e.g. `env FOO=1 npm test`): exec the real
                    // `env` binary unchanged so VAR=val is honoured by the OS.
                    super::run_raw_passthrough("env", tool_args, &[])
                }
            } else {
                env::run(tool_args, &ctx)
            }
        }
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
    /// SCOPE — read before citing this test as coverage.  It pins the
    /// CONSTRUCTOR, so a new tool that goes through `run_passthrough_tool` is
    /// correct by construction and needs no assertion of its own.  It does NOT
    /// detect a new tool that hand-rolls a `ToolRunConfig` literal with
    /// `skip_ansi_strip: false` — this test would still pass.  That case is
    /// caught instead by the `debug_assert!` in `cmd/execution.rs` after
    /// `parse()`, which fires on any `RawPassthrough` produced under a
    /// `false` flag.  Neither mechanism is a compiler guarantee.
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

    // ========================================================================
    // choose_passthrough_sink: the buffered-vs-streamed routing rule
    //
    // SCOPE — pure-function tests over the routing decision.  They exercise
    // NEITHER the rewrite engine NOR the PATH-wrapper dispatch front-end; the
    // e2e coverage of the two sinks lives in tests/cli_e2e_pipe_fidelity.rs.
    // ========================================================================

    /// The plain case — no flags, a real child process — streams.
    #[test]
    fn test_plain_invocation_streams() {
        assert_eq!(
            choose_passthrough_sink(SinkInputs {
                json_output: false,
                show_stats: false,
                reads_stdin: false,
                passthrough_mode: false,
            }),
            PassthroughSink::Streamed,
            "streaming is the default for a family that has no compressed view"
        );
    }

    /// Each exclusion independently routes away from the family sink.
    ///
    /// Table-driven so a newly added exclusion cannot be half-wired: the
    /// assertion names the reason it exists, and every field is exercised on its
    /// own rather than only in combination.
    #[test]
    fn test_each_exclusion_forces_the_buffered_sink() {
        let base = SinkInputs {
            json_output: false,
            show_stats: false,
            reads_stdin: false,
            passthrough_mode: false,
        };
        let cases: [(&str, SinkInputs); 4] = [
            (
                "--json needs the whole body inside a JSON string",
                SinkInputs {
                    json_output: true,
                    ..base
                },
            ),
            (
                "--show-stats tokenizes both strings to display a count",
                SinkInputs {
                    show_stats: true,
                    ..base
                },
            ),
            (
                "stdin input has no child process to stream from",
                SinkInputs {
                    reads_stdin: true,
                    ..base
                },
            ),
            (
                "SKIM_PASSTHROUGH=1 needs the escape hatch's byte-exact contract \
                 (no trailing-newline guard, no notices, no analytics) — it streams \
                 too, but from execution::stream_passthrough_raw, not from here",
                SinkInputs {
                    passthrough_mode: true,
                    ..base
                },
            ),
        ];

        for (reason, inputs) in cases {
            assert_eq!(
                choose_passthrough_sink(inputs),
                PassthroughSink::Buffered,
                "must not use the family streaming sink: {reason}"
            );
        }
    }
}
