//! Command execution infrastructure for skim CLI.
//!
//! Provides the types and functions that handle running external commands,
//! parsing their output through the three-tier degradation pipeline, and
//! recording analytics.

use std::borrow::Cow;
use std::io::{self, Write};
use std::process::ExitCode;

use crate::output::ParseResult;
use crate::runner::{CommandOutput, CommandRunner};

// ============================================================================
// Net-savings guard (#317 / Cluster C)
// ============================================================================

/// Outcome of the token-based net-savings decision.
///
/// Determines whether skim should emit the compressed body or fall back to the
/// literal raw output. The guard only ever moves output *toward* more-complete
/// raw — outcomes are "keep compressed" or "emit literal raw" — so it
/// strengthens the #317 invariant and cannot conflict with `elision_marker` /
/// `guardrail.rs`.  Applying it after `guardrail.rs` already chose raw is a
/// safe no-op: `Passthrough` at that point means raw == compressed.
///
/// **Reconciliation with `output/guardrail.rs`:**
/// `guardrail.rs` applies a ≥256-byte floor and is wired into the file-transform
/// path (`process.rs`) and `git/show.rs`.  This enum applies token-based savings
/// to the *command-handler* sinks that guardrail.rs does not cover (execution,
/// git, build, test, log).  There is no double-guard conflict: if guardrail.rs
/// already emitted raw, `savings_decision` sees raw == compressed and the tie rule
/// returns `Passthrough` — but since raw == compressed at that point, emitting raw
/// is identical to emitting compressed, so the outcome is the same either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub(crate) enum SavingsDecision {
    /// Compressed body is strictly smaller (in tokens, or bytes when the
    /// tokenizer is unavailable) — emit it.
    Keep,
    /// Compressed body is equal or larger — emit raw verbatim instead.
    Passthrough,
}

/// Decide whether to emit `compressed` or fall back to `raw`.
///
/// **Conservative rule:** keep compressed IFF `compressed_tokens < raw_tokens`
/// (strictly less).  Tie (equal) or larger → `Passthrough`.
/// This is the verbatim user decision: *"conservative — keep compressed ONLY IF
/// strictly smaller than raw, measured in tokens, always."*
/// Boundary: saving exactly 0 tokens → Passthrough; saving 1 token → Keep.
///
/// **Tokenizer-unavailable fallback:** if `count_token_pair` returns `(None, None)`
/// (counter init failed), fall back to a **byte** comparison:
/// keep iff `compressed.len() < raw.len()` (strictly less).
/// Never panics, never expands.
///
/// **Comparison normalization:** trailing whitespace is trimmed from both sides
/// before comparison so a single trailing newline does not flip the decision
/// arbitrarily (e.g. `println!` always appends `\n`; the raw command may or may
/// not end with `\n`).  This keeps boundary cases stable.
///
/// **Empty-raw behaviour:** if raw is empty/whitespace-only, compressed output is
/// NOT strictly smaller (0 < 0 fails) → Passthrough (emit raw, i.e. nothing).
/// A silent command stays silent, matching the raw tool exactly.
///
/// **JSON exempt:** callers are responsible for not calling this function when
/// `output_format == OutputFormat::Json`.  JSON responses must never be rewritten
/// to non-JSON; the guard only applies to `OutputFormat::Text` paths.
///
/// **Already-passthrough exempt:** if the parse tier is already `"passthrough"`,
/// `compressed` IS the raw body (no re-encoding occurred); skip the guard.
///
/// **#317 invariant:** this guard only ever moves output toward *more-complete
/// raw*.  It can never show LESS than raw.
///
/// **Size cap (performance):** for inputs above 64 MiB the tokenizer cost may
/// be significant.  Above that threshold the function falls back to byte
/// comparison, consistent with the "never expand" promise while keeping latency
/// sub-millisecond.
pub(crate) fn savings_decision(raw: &str, compressed: &str) -> SavingsDecision {
    /// 64 MiB — above this threshold skip tokenization (performance cap).
    /// Matches the stdin size cap documented in CLAUDE.md.
    const TOKEN_SIZE_CAP: usize = 64 * 1024 * 1024;

    // Normalize whitespace from both ends so leading/trailing formatting
    // (e.g., a `println!` trailing newline, or a leading space before "OK")
    // does not flip a tie.  We compare trimmed lengths; the actual emitted
    // bytes are unchanged.
    let raw_t = raw.trim();
    let comp_t = compressed.trim();

    // Conservative rule: keep compressed IFF strictly smaller than raw.
    //
    // Tie (equal tokens/bytes) or larger → Passthrough.  This is intentionally
    // conservative: the guard only ever moves output toward more-complete raw, so
    // it cannot show LESS than the raw tool.  A tie means no savings; the raw form
    // is equally complete and always safe to emit.
    //
    // Empty-raw case: if raw is empty/whitespace-only, comp_t.len() > 0 means
    // compressed is NOT strictly smaller (0 < n fails "comp < raw").  The uniform
    // rule therefore emits raw (nothing) — which is the faithful "never expand"
    // behaviour: a silent command stays silent, matching the raw tool exactly.
    //
    // Oversized inputs (> 64 MiB): tokenization is skipped for performance;
    // byte comparison is used instead — consistent with the "never expand" promise.

    // Fast path: if compressed is already strictly shorter by bytes, check with tokens.
    // If compressed is >= raw by bytes, decide immediately (Passthrough on tie/larger).
    if raw.len() > TOKEN_SIZE_CAP || compressed.len() > TOKEN_SIZE_CAP {
        // Above cap: byte comparison only (no tokenisation).
        return if comp_t.len() < raw_t.len() {
            SavingsDecision::Keep
        } else {
            // Tie or larger — Passthrough.
            SavingsDecision::Passthrough
        };
    }

    // Below cap: use tokens for the final decision, bytes for early exit.
    if comp_t.len() >= raw_t.len() {
        // Bytes say compressed is not shorter (tie or larger) — Passthrough immediately.
        // This covers the empty-raw case: raw_t.is_empty() means raw_t.len() == 0,
        // so comp_t.len() >= 0 is always true → Passthrough (emit raw, i.e. nothing).
        return SavingsDecision::Passthrough;
    }

    // comp_t.len() < raw_t.len() here — bytes say compressed is strictly shorter.
    // Confirm with token counts; if the tokenizer says compressed uses MORE tokens
    // than raw (byte-compression but token-expansion), passthrough.
    match crate::process::count_token_pair(raw_t, comp_t) {
        (Some(raw_tok), Some(comp_tok)) => {
            if comp_tok < raw_tok {
                // Strictly fewer tokens — keep compressed.
                SavingsDecision::Keep
            } else {
                // Token tie or token-expansion even though bytes were shorter → Passthrough.
                SavingsDecision::Passthrough
            }
        }
        // Tokenizer unavailable: byte comparison says comp_t.len() < raw_t.len() → Keep.
        _ => SavingsDecision::Keep,
    }
}

use super::{is_passthrough_mode, read_stdin_bounded, should_read_stdin};
use super::{scrub_db_args, scrub_infra_args};

/// Controls the output format of parsed command results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OutputFormat {
    /// Render the parsed result as human-readable text (default).
    #[default]
    Text,
    /// Serialize the parsed result as JSON (for `--json` flag).
    Json,
}

/// Cross-cutting configuration for subcommand execution.
///
/// Bundles the fields every family dispatcher receives identically, reducing
/// the positional parameter list to `(args, ctx)` at every call boundary.
///
/// ## Relationship to `RecordingContext`
///
/// Each family dispatcher constructs a [`crate::analytics::RecordingContext`]
/// from `analytics_enabled`, `session_id`, and the handler-local `command_type`,
/// then threads it directly through to [`ParsedCommandConfig::rec`].  The two
/// structs are intentionally separate: `RunContext` owns its strings while
/// `RecordingContext` borrows them (`Copy`, zero-allocation threading through
/// call chains).
pub(crate) struct RunContext {
    pub show_stats: bool,
    pub json_output: bool,
    pub analytics_enabled: bool,
    /// Optional session ID from `AnalyticsConfig::session_id`.
    /// Used by family dispatchers when constructing `RecordingContext`.
    pub session_id: Option<String>,
}

impl RunContext {
    /// Convert `json_output` to the corresponding [`OutputFormat`].
    pub(crate) fn output_format(&self) -> OutputFormat {
        if self.json_output {
            OutputFormat::Json
        } else {
            OutputFormat::Text
        }
    }
}

/// Configuration for running an external command with parsed output.
///
/// Groups the cross-cutting parameters for [`run_parsed_command_with_mode`]
/// to reduce its positional parameter count.
///
/// ## Analytics threading
///
/// `rec` carries the full [`crate::analytics::RecordingContext`] constructed
/// once by each family dispatcher.  `run_parsed_command_with_mode` calls
/// `rec.with_tier(result.tier_name())` at the recording site — no
/// decompose-then-reconstruct at the call site.
pub(crate) struct ParsedCommandConfig<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub env_overrides: &'a [(&'a str, &'a str)],
    pub install_hint: &'a str,
    pub use_stdin: bool,
    pub show_stats: bool,
    pub output_format: OutputFormat,
    /// Family name used to build analytics labels (e.g. `"lint"`, `"infra"`, `"file"`).
    ///
    /// Analytics labels are recorded as `"skim {family} {program} {args}"`. Without
    /// this field the label was `"skim {program} {args}"`, which dropped the family
    /// name and made the analytics dashboard ambiguous when multiple families share
    /// tool names (e.g., `cargo` appears in both `build` and `pkg`). (PF-022)
    pub family: &'a str,
    /// When `true`, skip ANSI escape stripping on the raw command output.
    ///
    /// `strip_ansi_escapes` treats ASCII control codes — including `\t` (0x09) —
    /// as part of escape sequences and drops them. DB tools emit tab-separated
    /// (TSV) output; stripping would remove tab separators and cause all DB
    /// parsers to fall through to Passthrough. DB tools set `true`;
    /// all other families set `false`.
    pub skip_ansi_strip: bool,
    /// Recording context constructed once by the family dispatcher.
    /// `run_parsed_command_with_mode` annotates `parse_tier` via
    /// `rec.with_tier(result.tier_name())` before passing to `try_record_command`.
    pub rec: crate::analytics::RecordingContext<'a>,
    /// Non-zero exit codes this tool's parser meaningfully compresses
    /// (e.g. `&[1]` for grep "no matches"). Any other non-zero exit — or a
    /// signal kill — forwards raw stdout+stderr instead of compressing. (#317)
    pub expected_exit_codes: &'a [i32],
    /// When `true`, forward child stderr verbatim to skim's stderr on the
    /// compressed path. Set for tools whose parsers only consume stdout, so
    /// warnings/diagnostics on stderr are never silently dropped. (#317)
    pub forward_stderr: bool,
    /// When `true`, skip the net-savings guard for this command (#317 / Cluster C).
    ///
    /// The guard normally prevents skim from emitting compressed output that is
    /// larger (in tokens/bytes) than the raw tool output.  Some tools are exempt
    /// because their output can legitimately restructure or reformat data in ways
    /// that are more token-efficient for an LLM even when byte counts are similar:
    ///
    /// - `gh` — streaming / API responses where the skim summary is semantically
    ///   richer than the raw JSON wire bytes (spec: "Exempt: `gh` streaming").
    /// - `heatmap` — always produces structured human-readable output; no "raw"
    ///   baseline is meaningful (spec: "Exempt: `heatmap`").
    ///
    /// Default: `false` (guard enabled).
    pub skip_net_savings_guard: bool,
}

/// How a child process's exit status should steer output handling. (#317)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitDisposition {
    /// Exit 0 — compress normally.
    Success,
    /// A non-zero code the tool's parser meaningfully compresses
    /// (e.g. grep 1 = no matches, cargo test 101 = test failures).
    ExpectedFailure,
    /// Any other non-zero code, or a signal kill (`None`) — the output is a
    /// diagnostic the parser was never designed for; forward it raw.
    UnexpectedFailure,
}

/// Classify an exit code against a tool's expected non-zero codes.
///
/// Must be called on the raw `Option<i32>` BEFORE any `unwrap_or` default:
/// a signal kill (`None`) is always an [`ExitDisposition::UnexpectedFailure`].
fn classify_exit(code: Option<i32>, expected: &[i32]) -> ExitDisposition {
    match code {
        Some(0) => ExitDisposition::Success,
        Some(c) if expected.contains(&c) => ExitDisposition::ExpectedFailure,
        _ => ExitDisposition::UnexpectedFailure,
    }
}

/// Merge stdout and stderr into a single string for fallback parsing.
///
/// Returns a `Cow::Borrowed` reference to stdout when stderr is empty
/// (zero-copy fast path), or a `Cow::Owned` concatenation otherwise.
pub(crate) fn combine_output(output: &CommandOutput) -> Cow<'_, str> {
    if output.stderr.is_empty() {
        Cow::Borrowed(&output.stdout)
    } else {
        Cow::Owned(format!("{}\n{}", output.stdout, output.stderr))
    }
}

/// Obtain command output from stdin or by spawning the command.
///
/// When `use_stdin` is `true`, reads stdin first. If stdin contains only
/// whitespace (e.g., a CI pipe that opens but writes nothing), the function
/// falls through silently to the spawn path so the real command runs with
/// its actual exit code instead of producing empty output.
///
/// Returns `None` when the program is not found (install hint already
/// printed to stderr). The caller should return `ExitCode::FAILURE`.
fn obtain_output(
    program: &str,
    args: &[String],
    env_overrides: &[(&str, &str)],
    install_hint: &str,
    use_stdin: bool,
) -> anyhow::Result<Option<CommandOutput>> {
    if use_stdin {
        let stdin_buf = read_stdin_bounded()?;
        if stdin_buf.bytes().any(|b| !b.is_ascii_whitespace()) {
            return Ok(Some(CommandOutput {
                stdout: stdin_buf,
                stderr: String::new(),
                exit_code: Some(0),
                duration: std::time::Duration::ZERO,
            }));
        }
    }

    let runner = CommandRunner::new();
    let args_str: Vec<&str> = args.iter().map(String::as_str).collect();
    match runner.run_with_env(program, &args_str, env_overrides) {
        Ok(out) => Ok(Some(out)),
        Err(e) => {
            if crate::runner::is_spawn_error(&e) {
                eprintln!("error: '{program}' not found");
                eprintln!("hint: {install_hint}");
                return Ok(None);
            }
            Err(e)
        }
    }
}

/// Serialize a parsed result to a string without writing to stdout.
///
/// Produces the same bytes that `render_output` would write, so callers can
/// apply the net-savings guard (`savings_decision`) before deciding which string
/// to actually emit.  `render_output` is kept as a convenience wrapper for
/// paths that never need the guard (e.g. JSON output, which is exempt).
fn serialize_output<T>(
    result: &ParseResult<T>,
    output_format: OutputFormat,
) -> anyhow::Result<String>
where
    T: AsRef<str> + serde::Serialize,
{
    match output_format {
        OutputFormat::Json => Ok(result.to_json_envelope()?),
        OutputFormat::Text => {
            let content = result.content();
            if content.is_empty() || content.ends_with('\n') {
                Ok(content.to_string())
            } else {
                Ok(format!("{content}\n"))
            }
        }
    }
}

/// Write a pre-serialized string to stdout.
fn write_to_stdout(s: &str) -> anyhow::Result<()> {
    let mut handle = io::stdout().lock();
    write!(handle, "{s}")?;
    handle.flush()?;
    Ok(())
}

/// Render parsed result to stdout, returning the output string for analytics.
fn render_output<T>(result: &ParseResult<T>, output_format: OutputFormat) -> anyhow::Result<String>
where
    T: AsRef<str> + serde::Serialize,
{
    let s = serialize_output(result, output_format)?;
    write_to_stdout(&s)?;
    Ok(s)
}

/// Write raw command output to stdout/stderr and return the process exit code.
///
/// Used by the passthrough fast-path in [`run_parsed_command_with_mode`] when
/// `SKIM_PASSTHROUGH=1` is set. Forwards stdout/stderr verbatim without any
/// compression or parsing.
fn passthrough_raw(output: &CommandOutput) -> anyhow::Result<ExitCode> {
    let code = output.exit_code.unwrap_or(1);
    let mut out = io::stdout().lock();
    write!(out, "{}", output.stdout)?;
    out.flush()?;
    if !output.stderr.is_empty() {
        let mut err = io::stderr().lock();
        write!(err, "{}", output.stderr)?;
        err.flush()?;
    }
    Ok(ExitCode::from(code.clamp(0, 255) as u8))
}

/// Tools for which exit code 1 means "no match" / "differs" — a benign
/// informational result that must not trigger the compressed-output hint.
///
/// These tools emit exit 1 when they find no matches or detect a difference,
/// which is not an error: the silence (or diff) IS the output.  Printing
/// "[skim] compressed output (exit 1)" is misleading — it implies something
/// went wrong when it did not.  Exit ≥ 2 for these tools IS a real error
/// (e.g., grep syntax error, diff read failure) and DOES get the hint.
/// Fix B (fix/rewrite-hook-falseneg).
const BENIGN_EXIT1_PROGRAMS: &[&str] = &["grep", "rg", "diff"];

/// Decide whether [`record_and_report`] should emit the compressed-output hint.
///
/// This is the single source of truth for the notice-matrix decision (#317),
/// extracted as a pure function so it is unit-testable without spawning a
/// process — the test and the production path call the *same* code, so a
/// regression in any of the three conditions is caught (PF-007: a test that
/// re-derives the expression inline asserts nothing).
///
/// Emit the hint when ALL hold:
/// - `code != 0` — exit 0 never gets a hint.
/// - `tier_name != "passthrough"` — a verbatim body needs no escape-hatch
///   notice (it already matches the raw tool, e.g. grep's no-match silence).
/// - NOT a benign exit-1 (Fix B): `code == 1` for a [`BENIGN_EXIT1_PROGRAMS`]
///   tool is "no match"/"differs", a normal informational result. Exit ≥ 2 for
///   those tools is a real error and still gets the hint.
///
/// Unexpected failures (codes outside `expected_exit_codes`) raw-forward and
/// return BEFORE reaching `record_and_report`, so a non-zero `code` seen here
/// is always an EXPECTED failure the parser meaningfully compresses.
fn should_emit_compressed_hint(program: &str, code: i32, tier_name: &str) -> bool {
    let is_benign_exit1 = code == 1 && BENIGN_EXIT1_PROGRAMS.contains(&program);
    code != 0 && tier_name != "passthrough" && !is_benign_exit1
}

/// Parameters for recording token savings and emitting the analytics event.
///
/// Bundles the fields that [`record_and_report`] needs, replacing the
/// eight-positional-parameter signature and removing the
/// `#[allow(clippy::too_many_arguments)]` suppression.  Follows the same
/// parameter-bundling pattern as [`ParsedCommandConfig`] and [`ToolRunConfig`].
struct RecordReport<'a> {
    show_stats: bool,
    code: i32,
    program: &'a str,
    original_stdout: String,
    compressed: String,
    rec: crate::analytics::RecordingContext<'a>,
    tier_name: &'static str,
    label: String,
    duration: std::time::Duration,
}

/// Record token savings and emit the analytics event for a completed command.
///
/// Separated from [`run_parsed_command_with_mode`] so the core parsing/rendering
/// pipeline is readable as a linear sequence of steps.
fn record_and_report(report: RecordReport<'_>) {
    let RecordReport {
        show_stats,
        code,
        program,
        original_stdout,
        compressed,
        rec,
        tier_name,
        label,
        duration,
    } = report;

    // Notice matrix (#317). Unexpected failures already raw-forwarded and
    // returned before reaching this point, so a non-zero `code` here is an
    // EXPECTED failure (a code the parser meaningfully compresses):
    // - tier Full/Degraded → surface the escape hatch: the body was re-encoded.
    // - tier Passthrough → silent: the body is already verbatim, so any notice
    //   would be noise the raw tool does not emit (grep's no-match silence).
    //
    // Fix B (fix/rewrite-hook-falseneg): suppress hint for BENIGN_EXIT1_PROGRAMS
    // at exit 1.  For these tools, exit 1 is "no match"/"differs" — a normal
    // informational result.  Exit ≥ 2 is a real error and still shows the hint.
    // The decision lives in `should_emit_compressed_hint` so it is unit-tested
    // against the same code path (PF-007).
    if should_emit_compressed_hint(program, code, tier_name) {
        eprintln!("{}", crate::output::compressed_output_hint(code));
    }

    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(&original_stdout, &compressed);
        crate::process::report_token_stats(orig, comp, "");
    }

    crate::analytics::try_record_command(
        rec.with_tier(tier_name),
        original_stdout,
        compressed,
        label,
        duration,
    );
}

/// Execute an external command, parse its output, and emit the result.
///
/// This is the standard entry point for subcommand parsers that follow the
/// three-tier degradation pattern. Delegates stdin/spawn to [`obtain_output`]
/// and rendering to [`render_output`].
///
/// `config.use_stdin` — when `true`, reads stdin instead of spawning the command.
/// Callers should set this based on their own heuristics (e.g., only read
/// stdin when no user args are provided AND stdin is piped).
pub(crate) fn run_parsed_command_with_mode<T>(
    config: ParsedCommandConfig<'_>,
    parse: impl FnOnce(&CommandOutput) -> ParseResult<T>,
) -> anyhow::Result<ExitCode>
where
    T: AsRef<str> + serde::Serialize,
{
    run_parsed_command_with_exit(config, parse, |_| None)
}

/// [`run_parsed_command_with_mode`] with a parser-derived exit code (#317).
///
/// `derive_exit` inspects the parsed result and may return a non-zero exit
/// code. The final exit is `max(child_exit, derived)` — needed on the stdin
/// path, where `obtain_output` fabricates `exit_code: Some(0)` and a piped
/// failing test run would otherwise exit 0.
pub(crate) fn run_parsed_command_with_exit<T>(
    config: ParsedCommandConfig<'_>,
    parse: impl FnOnce(&CommandOutput) -> ParseResult<T>,
    derive_exit: impl FnOnce(&ParseResult<T>) -> Option<i32>,
) -> anyhow::Result<ExitCode>
where
    T: AsRef<str> + serde::Serialize,
{
    let ParsedCommandConfig {
        program,
        args,
        env_overrides,
        install_hint,
        use_stdin,
        show_stats,
        output_format,
        family,
        skip_ansi_strip,
        rec,
        expected_exit_codes,
        forward_stderr,
        skip_net_savings_guard,
    } = config;

    let Some(output) = obtain_output(program, args, env_overrides, install_hint, use_stdin)? else {
        return Ok(ExitCode::FAILURE);
    };

    // Passthrough mode: bypass all compression and forward raw output.
    if is_passthrough_mode() {
        return passthrough_raw(&output);
    }

    // Unexpected failure (#317): the parser was never designed for this
    // output — compressing it would hide the very diagnostic the agent needs.
    // Forward raw stdout+stderr byte-faithfully (checked BEFORE ANSI
    // stripping) and record zero savings under the "raw" tier.
    if classify_exit(output.exit_code, expected_exit_codes) == ExitDisposition::UnexpectedFailure {
        match output.exit_code {
            Some(code) => {
                eprintln!("[skim] {program} exited {code}; raw output (not compressed).")
            }
            None => eprintln!("[skim] {program} killed by signal; raw output (not compressed)."),
        }
        let label = format_analytics_label(family, program, &args.join(" "));
        crate::analytics::try_record_command(
            rec.with_tier("raw"),
            output.stdout.clone(),
            output.stdout.clone(),
            label,
            output.duration,
        );
        return passthrough_raw(&output);
    }

    // Child stderr to forward verbatim on the compressed path (#317).
    // Captured before ANSI stripping so the forwarded bytes are faithful.
    let stderr_to_forward = if forward_stderr && !output.stderr.is_empty() {
        Some(output.stderr.clone())
    } else {
        None
    };

    // Some tools must NOT have ANSI escape sequences stripped: strip_ansi_escapes
    // treats ASCII control codes — including \t (0x09) — as part of escape
    // sequences and drops them. DB tools emit tab-separated (TSV) output; stripping
    // would remove tab separators and cause all DB parsers to fall through to
    // Passthrough. Callers signal this via `config.skip_ansi_strip`.
    let output = if skip_ansi_strip {
        output
    } else {
        CommandOutput {
            stdout: crate::output::strip_ansi(&output.stdout),
            stderr: crate::output::strip_ansi(&output.stderr),
            ..output
        }
    };

    let result = parse(&output);
    let _ = result.emit_markers(&mut io::stderr().lock());
    // max(child, derived): the stdin path fabricates child exit 0, so a
    // parser-derived failure code (e.g. cargo fail count > 0) wins (#317).
    let code = output
        .exit_code
        .unwrap_or(1)
        .max(derive_exit(&result).unwrap_or(0));
    let label = format_analytics_label(family, program, &args.join(" "));
    let tier_name = result.tier_name();

    // Net-savings guard (Cluster C / #317):
    // Serialize first without writing, so we can apply savings_decision
    // before committing to stdout.
    //
    // Exemptions:
    // - JSON output: must never be rewritten to non-JSON.
    // - Already-passthrough tier: compressed IS the raw body (no re-encoding);
    //   guard would be a no-op but skipping avoids double tokenization.
    //
    // "raw" baseline for this sink = post-ANSI-strip stdout (`output.stdout`).
    // This is the correct baseline because ANSI stripping is already applied
    // above; the user's terminal would see the same stripped bytes.
    let (compressed, effective_tier) = if output_format == OutputFormat::Text
        && tier_name != "passthrough"
        && !skip_net_savings_guard
    {
        let compressed_str = serialize_output(&result, output_format)?;
        match savings_decision(&output.stdout, &compressed_str) {
            SavingsDecision::Keep => {
                write_to_stdout(&compressed_str)?;
                (compressed_str, tier_name)
            }
            SavingsDecision::Passthrough => {
                // Emit raw verbatim; record analytics under "passthrough" tier
                // so `should_emit_compressed_hint` stays silent (passthrough tier
                // never gets the hint — the body is already verbatim raw).
                let raw = &output.stdout;
                let mut out = io::stdout().lock();
                write!(out, "{raw}")?;
                if !raw.is_empty() && !raw.ends_with('\n') {
                    writeln!(out)?;
                }
                out.flush()?;
                (raw.clone(), "passthrough")
            }
        }
    } else {
        // JSON or already-passthrough: write normally, no guard needed.
        let s = render_output(&result, output_format)?;
        (s, tier_name)
    };

    if let Some(err_text) = stderr_to_forward {
        let mut err = io::stderr().lock();
        write!(err, "{err_text}")?;
        if !err_text.ends_with('\n') {
            writeln!(err)?;
        }
        err.flush()?;
    }

    record_and_report(RecordReport {
        show_stats,
        code,
        program,
        original_stdout: output.stdout,
        compressed,
        rec,
        tier_name: effective_tier,
        label,
        duration: output.duration,
    });

    Ok(ExitCode::from(code.clamp(0, 255) as u8))
}

/// Build a standardized analytics label: `"skim {family} {program} {rest}"`.
///
/// Centralises the label format so streaming and non-streaming code paths
/// cannot drift.  `rest` is the pre-joined argument string (may be empty).
///
/// Sensitive flags are redacted before the label is stored to prevent
/// credentials persisting in the analytics SQLite database:
///
/// - `"db"` family: passwords, usernames, hostnames (psql/mysql flags).
/// - `"infra"` family: Authorization headers, `--token`, `--password`,
///   `--secret`, `--api-key`, and similar flags used by curl, aws, gh, etc.
pub(crate) fn format_analytics_label(family: &str, program: &str, rest: &str) -> String {
    if rest.is_empty() {
        return format!("skim {family} {program}");
    }
    let scrubbed_rest = match family {
        "db" => scrub_db_args(rest),
        "infra" => scrub_infra_args(rest),
        _ => rest.to_string(),
    };
    format!("skim {family} {program} {scrubbed_rest}")
}

/// Cross-cutting configuration for a single-tool execution.
///
/// Unifies `DbToolConfig`, `InfraToolConfig`, `FileToolConfig`, and
/// `LinterConfig` into one struct.  The two new fields (`family`,
/// `skip_ansi_strip`) are the only differences between the four original
/// family-specific configs; all other fields are structurally identical.
///
/// ## Relationship to `ParsedCommandConfig`
///
/// `ToolRunConfig` is the caller-facing API; `ParsedCommandConfig` is the
/// internal config consumed by `run_parsed_command_with_mode`.  `run_tool`
/// bridges the two, translating caller fields plus `family`/`skip_ansi_strip`
/// into the full `ParsedCommandConfig`.
///
/// The split is intentional: `ToolRunConfig` carries only static, caller-supplied
/// fields.  `ParsedCommandConfig` additionally requires runtime-computed fields
/// (`use_stdin`, `show_stats`, `output_format`, `rec`) derived from `RunContext`
/// and the actual argument list — values unavailable at `ToolRunConfig`
/// construction time.  `Into<ParsedCommandConfig>` would therefore be unsound
/// without also accepting `&[String]` and `&RunContext`, which defeats the
/// purpose of a simple `Into` bridge.  `run_tool` IS the bridge.
pub(crate) struct ToolRunConfig<'a> {
    /// Binary name of the tool (e.g., "psql", "eslint").
    pub program: &'a str,
    /// Environment variable overrides for the child process.
    pub env_overrides: &'a [(&'a str, &'a str)],
    /// Hint printed when the tool binary is not found.
    pub install_hint: &'a str,
    /// Family name for analytics labels (e.g. `"db"`, `"infra"`, `"lint"`).
    pub family: &'a str,
    /// When `true`, skip ANSI escape stripping on the raw command output.
    ///
    /// Set `true` for DB tools (TSV output) and DNS tools (tab field separators).
    /// See `ParsedCommandConfig::skip_ansi_strip` for full rationale.
    pub skip_ansi_strip: bool,
    /// Analytics command type for recording.
    pub command_type: crate::analytics::CommandType,
    /// Non-zero exit codes this tool's parser meaningfully compresses.
    /// See [`ParsedCommandConfig::expected_exit_codes`]. (#317)
    pub expected_exit_codes: &'a [i32],
    /// Forward child stderr verbatim on the compressed path.
    /// See [`ParsedCommandConfig::forward_stderr`]. (#317)
    pub forward_stderr: bool,
    /// Skip the net-savings guard.
    /// See [`ParsedCommandConfig::skip_net_savings_guard`]. (#317)
    pub skip_net_savings_guard: bool,
}

/// Execute a tool, parse its output, and emit the result.
///
/// Single generic implementation that replaces `run_db_tool`, `run_infra_tool`,
/// `run_file_tool`, and `run_linter`.  Each family-specific runner had an
/// identical body; the only differences were `family`, `skip_ansi_strip`, and
/// `command_type`, which are now carried in `ToolRunConfig`.
///
/// ## Constraints
///
/// `build::run_parsed_command` is intentionally **not** replaced: it has a
/// different call shape (no `ctx: &RunContext`, different analytics path).
/// `run_pkg_subcommand` is also excluded: it has a different signature.
pub(crate) fn run_tool<T>(
    config: ToolRunConfig<'_>,
    args: &[String],
    ctx: &RunContext,
    prepare_args: impl FnOnce(&mut Vec<String>),
    parse_fn: impl FnOnce(&CommandOutput) -> ParseResult<T>,
) -> anyhow::Result<std::process::ExitCode>
where
    T: AsRef<str> + serde::Serialize,
{
    let mut cmd_args = args.to_vec();
    prepare_args(&mut cmd_args);
    let use_stdin = should_read_stdin(args);
    run_parsed_command_with_mode(
        ParsedCommandConfig {
            program: config.program,
            args: &cmd_args,
            env_overrides: config.env_overrides,
            install_hint: config.install_hint,
            use_stdin,
            show_stats: ctx.show_stats,
            output_format: ctx.output_format(),
            family: config.family,
            skip_ansi_strip: config.skip_ansi_strip,
            rec: crate::analytics::RecordingContext {
                enabled: ctx.analytics_enabled,
                command_type: config.command_type,
                parse_tier: None,
                session_id: ctx.session_id.as_deref(),
            },
            expected_exit_codes: config.expected_exit_codes,
            forward_stderr: config.forward_stderr,
            skip_net_savings_guard: config.skip_net_savings_guard,
        },
        parse_fn,
    )
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // classify_exit tests (#317)
    // ========================================================================

    #[test]
    fn test_classify_exit_zero_is_success() {
        assert_eq!(classify_exit(Some(0), &[]), ExitDisposition::Success);
        assert_eq!(classify_exit(Some(0), &[1, 2]), ExitDisposition::Success);
    }

    #[test]
    fn test_classify_exit_expected_code() {
        assert_eq!(
            classify_exit(Some(1), &[1]),
            ExitDisposition::ExpectedFailure
        );
        assert_eq!(
            classify_exit(Some(101), &[101]),
            ExitDisposition::ExpectedFailure
        );
    }

    #[test]
    fn test_classify_exit_unexpected_code() {
        // grep exit 2 = real error (e.g. missing file) — never compress.
        assert_eq!(
            classify_exit(Some(2), &[1]),
            ExitDisposition::UnexpectedFailure
        );
        assert_eq!(
            classify_exit(Some(1), &[]),
            ExitDisposition::UnexpectedFailure
        );
    }

    #[test]
    fn test_classify_exit_signal_kill_is_always_unexpected() {
        // None (signal kill) must classify BEFORE any unwrap_or(1) default:
        // even if 1 is expected, a signal kill is not an expected failure.
        assert_eq!(
            classify_exit(None, &[1]),
            ExitDisposition::UnexpectedFailure
        );
        assert_eq!(classify_exit(None, &[]), ExitDisposition::UnexpectedFailure);
    }

    // ========================================================================
    // format_analytics_label tests
    // ========================================================================

    #[test]
    fn test_format_analytics_label_db_scrubs_credentials() {
        // Simulate: skim db psql -h myhost -U admin -c SELECT 1
        let label = format_analytics_label("db", "psql", "-h myhost -U admin -c SELECT 1");
        assert!(
            !label.contains("myhost"),
            "hostname must be redacted from db analytics label: {label}"
        );
        assert!(
            !label.contains("admin"),
            "username must be redacted from db analytics label: {label}"
        );
        assert!(
            label.contains("[REDACTED]"),
            "redaction marker must be present: {label}"
        );
    }

    #[test]
    fn test_format_analytics_label_non_sensitive_infra_not_scrubbed() {
        // Non-sensitive infra args (no auth flags) are forwarded verbatim.
        let label = format_analytics_label("infra", "kubectl", "get pods -n myns");
        assert!(
            label.contains("myns"),
            "non-sensitive infra args must not be scrubbed: {label}"
        );
    }

    #[test]
    fn test_format_analytics_label_infra_scrubs_token() {
        // Sensitive --token flag must be redacted for the infra family.
        let label = format_analytics_label("infra", "gh", "--token ghp_secrettoken repo list");
        assert!(
            !label.contains("ghp_secrettoken"),
            "token value must be redacted from infra analytics label: {label}"
        );
        assert!(
            label.contains("[REDACTED]"),
            "redaction marker must be present: {label}"
        );
        assert!(
            label.contains("repo list"),
            "non-sensitive args must be preserved: {label}"
        );
    }

    #[test]
    fn test_format_analytics_label_db_empty_rest() {
        let label = format_analytics_label("db", "psql", "");
        assert_eq!(label, "skim db psql");
    }

    // ========================================================================
    // combine_output tests
    // ========================================================================

    #[test]
    fn test_combine_output_empty_stderr_borrows() {
        // Fast path: empty stderr must return Cow::Borrowed (zero-copy).
        let output = crate::cmd::test_utils::make_output_full("hello world", "", Some(0));
        let combined = combine_output(&output);
        assert!(
            matches!(combined, Cow::Borrowed(_)),
            "empty stderr must produce Cow::Borrowed (zero-copy): {combined:?}"
        );
        assert_eq!(combined.as_ref(), "hello world");
    }

    #[test]
    fn test_combine_output_non_empty_stderr_concatenates() {
        // Slow path: non-empty stderr triggers owned concatenation.
        let output =
            crate::cmd::test_utils::make_output_full("stdout line", "stderr line", Some(0));
        let combined = combine_output(&output);
        assert!(
            matches!(combined, Cow::Owned(_)),
            "non-empty stderr must produce Cow::Owned (concatenation): {combined:?}"
        );
        assert_eq!(combined.as_ref(), "stdout line\nstderr line");
    }

    #[test]
    fn test_combine_output_both_empty_borrows() {
        // Both empty: stdout is empty string; stderr is empty so fast path applies.
        let output = crate::cmd::test_utils::make_output_full("", "", Some(0));
        let combined = combine_output(&output);
        assert!(
            matches!(combined, Cow::Borrowed(_)),
            "both empty must produce Cow::Borrowed: {combined:?}"
        );
        assert_eq!(combined.as_ref(), "");
    }

    // ========================================================================
    // BENIGN_EXIT1_PROGRAMS guard (Fix B, fix/rewrite-hook-falseneg)
    // ========================================================================

    // These tests drive the real `should_emit_compressed_hint` decision used by
    // `record_and_report` (PF-007): each one would FAIL if the production guard
    // regressed (e.g. dropping `program` from the check, flipping `code == 1`,
    // or removing the `!is_benign_exit1` term). The Full and Degraded tier names
    // exercise the non-passthrough branch where the hint is live.

    /// grep exit 1 = "no match" — benign; the compressed-output hint is suppressed.
    #[test]
    fn test_benign_exit1_grep() {
        assert!(
            BENIGN_EXIT1_PROGRAMS.contains(&"grep"),
            "grep must be in BENIGN_EXIT1_PROGRAMS"
        );
        assert!(
            !should_emit_compressed_hint("grep", 1, "full"),
            "grep exit 1 is benign — hint must be suppressed"
        );
        assert!(
            !should_emit_compressed_hint("grep", 1, "degraded"),
            "grep exit 1 is benign at the degraded tier too"
        );
    }

    /// rg exit 1 = "no match" — benign; hint suppressed.
    #[test]
    fn test_benign_exit1_rg() {
        assert!(
            BENIGN_EXIT1_PROGRAMS.contains(&"rg"),
            "rg must be in BENIGN_EXIT1_PROGRAMS"
        );
        assert!(
            !should_emit_compressed_hint("rg", 1, "full"),
            "rg exit 1 is benign — hint must be suppressed"
        );
    }

    /// diff exit 1 = "files differ" — benign; hint suppressed.
    #[test]
    fn test_benign_exit1_diff() {
        assert!(
            BENIGN_EXIT1_PROGRAMS.contains(&"diff"),
            "diff must be in BENIGN_EXIT1_PROGRAMS"
        );
        assert!(
            !should_emit_compressed_hint("diff", 1, "full"),
            "diff exit 1 is benign — hint must be suppressed"
        );
    }

    /// grep exit 2 = real error (e.g., syntax error) — NOT benign; hint fires.
    #[test]
    fn test_grep_exit2_is_not_benign() {
        assert!(
            should_emit_compressed_hint("grep", 2, "full"),
            "grep exit 2 is a real error — hint must fire"
        );
    }

    /// A non-benign tool (e.g., cargo) at exit 1 still gets the hint.
    #[test]
    fn test_non_benign_tool_exit1_is_not_suppressed() {
        assert!(
            should_emit_compressed_hint("cargo", 1, "full"),
            "cargo exit 1 is not benign — hint must still fire"
        );
    }

    /// Passthrough tier is always silent: the body is already verbatim, so even
    /// a non-benign non-zero exit emits no hint (would duplicate raw behavior).
    #[test]
    fn test_passthrough_tier_never_hints() {
        assert!(
            !should_emit_compressed_hint("cargo", 1, "passthrough"),
            "passthrough tier must never emit the compressed-output hint"
        );
        assert!(
            !should_emit_compressed_hint("grep", 2, "passthrough"),
            "passthrough tier is silent even for a real grep error"
        );
    }

    /// Exit 0 never emits the hint, regardless of tier.
    #[test]
    fn test_exit0_never_hints() {
        assert!(
            !should_emit_compressed_hint("cargo", 0, "full"),
            "exit 0 must never emit the compressed-output hint"
        );
        assert!(
            !should_emit_compressed_hint("grep", 0, "degraded"),
            "exit 0 is success — no hint"
        );
    }

    /// Lint and pkg tools at exit 1 are NOT in BENIGN_EXIT1_PROGRAMS, so the
    /// hint MUST fire when they produce a compressed (Full/Degraded) body.
    ///
    /// This complements the grep/rg/diff benign-suppression tests: those assert
    /// the hint is suppressed; these assert it is NOT suppressed for families
    /// where exit 1 means "lint violations found" or "package op failed" — a
    /// real problem, not a normal informational result.
    ///
    /// Discriminates against a future regression that blanket-suppresses exit-1
    /// for ALL programs regardless of BENIGN_EXIT1_PROGRAMS membership.
    #[test]
    fn test_lint_exit1_is_not_suppressed() {
        // eslint exit 1 = lint violations found — not benign; hint must fire.
        assert!(
            !BENIGN_EXIT1_PROGRAMS.contains(&"eslint"),
            "eslint must NOT be in BENIGN_EXIT1_PROGRAMS"
        );
        assert!(
            should_emit_compressed_hint("eslint", 1, "full"),
            "eslint exit 1 is lint violations — hint must fire (not suppressed)"
        );
        assert!(
            should_emit_compressed_hint("eslint", 1, "degraded"),
            "eslint exit 1 hint must fire at the degraded tier too"
        );
    }

    /// pkg tool (cargo subcommand) at exit 1 — hint fires.
    #[test]
    fn test_pkg_exit1_is_not_suppressed() {
        // npm exit 1 = package operation error — not a benign "no result".
        assert!(
            !BENIGN_EXIT1_PROGRAMS.contains(&"npm"),
            "npm must NOT be in BENIGN_EXIT1_PROGRAMS"
        );
        assert!(
            should_emit_compressed_hint("npm", 1, "full"),
            "npm exit 1 is a real error — hint must fire"
        );
    }

    // ========================================================================
    // savings_decision tests (Cluster C / #317)
    // Conservative rule: Keep IFF compressed strictly smaller; tie → Passthrough.
    // ========================================================================

    // -- Boundary tests: exactly 0 tokens saved → Passthrough; 1 token → Keep --

    /// Empty raw, empty compressed: tie (0 == 0) → Passthrough.
    /// A silent command stays silent; emitting nothing matches the raw tool.
    #[test]
    fn savings_decision_empty_raw_empty_compressed_passthrough() {
        assert_eq!(
            savings_decision("", ""),
            SavingsDecision::Passthrough,
            "empty tie → Passthrough (conservative: strictly-smaller-to-keep)"
        );
    }

    /// Empty raw, non-empty compressed: compressed is NOT strictly smaller (0 < n fails) →
    /// Passthrough.  The conservative rule means a silent command stays silent.
    #[test]
    fn savings_decision_empty_raw_non_empty_compressed_passthrough() {
        assert_eq!(
            savings_decision("", "OK warnings: 0 errors: 0\n"),
            SavingsDecision::Passthrough,
            "non-empty compressed vs empty raw: compressed is not strictly smaller → Passthrough"
        );
    }

    /// Exactly 0 tokens saved (identical strings) — tie → Passthrough.
    #[test]
    fn savings_decision_identical_input_passthrough() {
        let text = "hello world\n";
        assert_eq!(
            savings_decision(text, text),
            SavingsDecision::Passthrough,
            "tie (identical strings) → Passthrough (strictly-smaller rule)"
        );
    }

    /// Compressed strictly shorter by bytes → Keep.
    #[test]
    fn savings_decision_shorter_compressed_keep() {
        let raw = "a".repeat(100);
        let compressed = "a".repeat(50);
        assert_eq!(savings_decision(&raw, &compressed), SavingsDecision::Keep);
    }

    /// Compressed is strictly longer → Passthrough (never expand).
    #[test]
    fn savings_decision_longer_compressed_passthrough() {
        let raw = "short\n";
        let compressed = raw.repeat(3); // 3× raw is longer
        assert_eq!(
            savings_decision(raw, &compressed),
            SavingsDecision::Passthrough
        );
    }

    /// Trailing-newline normalisation: `println!` appends `\n` to the compressed
    /// string; the raw command may not end with `\n`.  After trimming both sides
    /// the trimmed lengths are EQUAL — a tie — so the conservative rule gives
    /// Passthrough (tie is not strictly smaller).
    #[test]
    fn savings_decision_trailing_newline_tie_passthrough() {
        let raw = "same content"; // no trailing newline
        let compressed = "same content\n"; // println! adds newline
        assert_eq!(
            savings_decision(raw, compressed),
            SavingsDecision::Passthrough,
            "trailing-newline tie: trimmed lengths equal → Passthrough (strictly-smaller rule)"
        );
    }

    /// Compressed shorter even after trailing-newline trim → Keep.
    #[test]
    fn savings_decision_shorter_after_trim_keep() {
        let raw = "aaabbbccc"; // 9 bytes, no newline
        let compressed = "abc\n"; // 4 bytes trimmed = 3 < 9
        assert_eq!(savings_decision(raw, compressed), SavingsDecision::Keep);
    }

    /// Strict-expansion passthrough boundary: compressed is exactly raw+1 byte → Passthrough.
    #[test]
    fn savings_decision_one_byte_expansion_passthrough() {
        let raw = "hello";
        let compressed = "hello!"; // 6 bytes > 5 bytes: strictly longer
        assert_eq!(
            savings_decision(raw, compressed),
            SavingsDecision::Passthrough
        );
    }

    /// Boundary: compressed is exactly raw minus 1 byte (strictly shorter) → Keep.
    #[test]
    fn savings_decision_one_byte_saving_keep() {
        let raw = "helloX"; // 6 bytes
        let compressed = "hello"; // 5 bytes — 1 byte strictly smaller
        assert_eq!(
            savings_decision(raw, compressed),
            SavingsDecision::Keep,
            "saving exactly 1 byte → Keep"
        );
    }

    /// Large input above TOKEN_SIZE_CAP (64 MiB): falls back to byte comparison.
    /// Compressed strictly shorter → Keep.
    #[test]
    fn savings_decision_above_cap_bytes_keep() {
        let raw = "x".repeat(65 * 1024 * 1024); // 65 MiB
        let compressed = "x".repeat(1024); // much shorter
        assert_eq!(savings_decision(&raw, &compressed), SavingsDecision::Keep);
    }

    /// Large input above TOKEN_SIZE_CAP: compressed STRICTLY LARGER → Passthrough.
    #[test]
    fn savings_decision_above_cap_bytes_passthrough() {
        let raw = "x".repeat(65 * 1024 * 1024); // 65 MiB
        let compressed = "y".repeat(65 * 1024 * 1024 + 1); // 1 byte strictly longer
        assert_eq!(
            savings_decision(&raw, &compressed),
            SavingsDecision::Passthrough
        );
    }

    /// Large input above TOKEN_SIZE_CAP: same-size → Passthrough (tie rule applies above cap too).
    #[test]
    fn savings_decision_above_cap_bytes_tie_passthrough() {
        let raw = "x".repeat(65 * 1024 * 1024); // 65 MiB
        let compressed = "y".repeat(65 * 1024 * 1024); // same size — tie
        assert_eq!(
            savings_decision(&raw, &compressed),
            SavingsDecision::Passthrough,
            "above-cap tie → Passthrough (strictly-smaller rule)"
        );
    }

    /// Verify the must_use attribute fires (compile-time; checked via doc).
    /// Property: savings_decision never returns Keep when compressed is not strictly
    /// smaller than raw (by trimmed bytes, as the byte gate fires first).
    #[test]
    fn savings_decision_keep_always_means_compressed_strictly_shorter_bytes() {
        // For all (raw, compressed) pairs where Keep is returned,
        // compressed.trim().len() MUST be < raw.trim().len().
        let cases = vec![
            ("abcdef", "ab"),
            ("line1\nline2\nline3\n", "summary\n"),
            ("long raw content here", "short"),
        ];
        for (raw, compressed) in cases {
            let decision = savings_decision(raw, compressed);
            if decision == SavingsDecision::Keep {
                assert!(
                    compressed.trim().len() < raw.trim().len(),
                    "Keep returned but compressed is not strictly shorter: raw={raw:?} comp={compressed:?}"
                );
            }
        }
    }
}
