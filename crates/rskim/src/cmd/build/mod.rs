//! Build output compression (#51)
//!
//! Handles build tool output for cargo, clippy, make, and tsc using three-tier
//! parse degradation. Called via flat dispatch (`skim tsc`) or multi-category
//! dispatch (`skim cargo build`, `skim cargo clippy`). Supports both direct
//! invocation and piped stdin.

pub(crate) mod cargo;
pub(crate) mod gradle;
pub(crate) mod make;
pub(crate) mod maven;
pub(crate) mod tsc;

use std::process::ExitCode;

use crate::output::ParseResult;
use crate::output::canonical::BuildResult;
use crate::runner::{CommandOutput, CommandRunner};

// ============================================================================
// Public dispatch
// ============================================================================

/// Dispatch build tool handlers.
///
/// Called by flat dispatch (`skim tsc`) or multi-category dispatch
/// (`skim cargo build`, `skim cargo clippy`). The `args` slice has the
/// tool name prepended by the caller.
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    // Handle --help / -h
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let (filtered_args, show_stats) = crate::cmd::extract_show_stats(args);

    let (sub, remaining) = match filtered_args.split_first() {
        Some((first, rest)) => (Some(first.as_str()), rest),
        None => (None, [].as_slice()),
    };

    let rec = crate::analytics::RecordingContext {
        enabled: analytics.enabled,
        command_type: crate::analytics::CommandType::Build,
        parse_tier: None,
        session_id: analytics.session_id.as_deref(),
    };
    match sub {
        Some("build") => cargo::run(remaining, show_stats, rec),
        Some("check") => cargo::run_check(remaining, show_stats, rec),
        Some("fmt") => cargo::run_fmt(remaining, show_stats, rec),
        Some("clippy") => cargo::run_clippy(remaining, show_stats, rec),
        Some(program @ ("gradle" | "gradlew")) => gradle::run(program, remaining, show_stats, rec),
        Some("make") => make::run(remaining, show_stats, rec),
        Some(program @ ("mvn" | "mvnw" | "maven")) => {
            maven::run(program, remaining, show_stats, rec)
        }
        Some("tsc") => tsc::run(remaining, show_stats, rec),
        Some(unknown) => {
            // Defensive branch: flat dispatch always prepends a known tool name
            // before calling this function, so this arm is only reachable via
            // internal routing bugs.
            let safe_unknown = crate::cmd::sanitize_for_display(unknown);
            eprintln!(
                "skim: unknown subcommand '{safe_unknown}'\n\
                 Supported tools: cargo (subcommands: build, check, fmt, clippy), gradle, gradlew, make, mvn, mvnw, tsc"
            );
            Ok(ExitCode::FAILURE)
        }
        None => {
            eprintln!(
                "skim: missing build tool\n\n\
                 Usage: skim cargo build [args...]\n\
                 Usage: skim cargo check [args...]\n\
                 Usage: skim cargo fmt [args...]\n\
                 Usage: skim gradle [args...]\n\
                 Usage: skim make [args...]\n\
                 Usage: skim mvn [args...]\n\
                 Usage: skim tsc [args...]\n\n\
                 Supported tools: cargo (subcommands: build, check, fmt, clippy), gradle, gradlew, make, mvn, mvnw, tsc"
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!(
        "skim {{cargo build|cargo check|cargo fmt|cargo clippy|gradle|make|mvn|tsc}} [args...]"
    );
    println!();
    println!("  Run build tools and compress output for AI context windows.");
    println!();
    println!("Available tools:");
    println!("  cargo            Run cargo with output compression");
    println!("    build          Run cargo build");
    println!("    check          Run cargo check");
    println!("    fmt            Run cargo fmt");
    println!("    clippy         Run cargo clippy");
    println!("  gradle           Run Gradle with output compression (also: gradlew)");
    println!("  make             Run GNU make with output compression");
    println!("  mvn              Run Maven with output compression (also: mvnw)");
    println!("  tsc              Run TypeScript compiler with output compression");
    println!();
    println!("Flags:");
    println!("  --show-stats    Show token statistics");
    println!();
    println!("Examples:");
    println!("  skim cargo build");
    println!("  skim cargo build --release");
    println!("  skim cargo check");
    println!("  skim cargo fmt");
    println!("  skim cargo clippy -- -W clippy::pedantic");
    println!("  skim gradle build");
    println!("  skim make");
    println!("  skim make -j4 all");
    println!("  skim mvn compile");
    println!("  skim tsc --noEmit");
}

// Shared helpers (user_has_flag, inject_flag_before_separator) are in crate::cmd

// ============================================================================
// Disclosure policy
// ============================================================================

/// Number of diagnostics the parser summarised, for the ADR-011 class-1
/// disclosure in [`run_parsed_command`]'s `Keep` arm.
///
/// Read from the parsed result rather than from the rendered text, so the count
/// is the parser's OWN and not a re-scan of its output — a re-scan would count
/// whatever the renderer happened to keep, which is the number the marker must
/// not report.
///
/// Both passthrough tiers report `0`, and that is a correctness requirement
/// rather than a default: they carry no parser verdict, the child's own bytes
/// reach the reader with every snippet, `help:` line and explain hint intact, so
/// a disclosure there would describe a loss that did not happen. The `Keep` arm
/// additionally gates on `> 0` — a build with nothing to report drops no
/// diagnostic bodies, so its summary line IS the whole truth.
#[must_use]
fn summarised_diagnostics(result: &ParseResult<BuildResult>) -> usize {
    match result {
        ParseResult::Full(r) | ParseResult::Degraded(r, _) => r.errors + r.warnings,
        ParseResult::Passthrough(_) | ParseResult::RawPassthrough => 0,
    }
}

/// Warnings the parser replaced with a by-lint-code roll-up, or 0.
///
/// The second half of the same ADR-011 class-1 disclosure. Where
/// [`summarised_diagnostics`] sizes what was summarised,
/// this sizes a loss that fires only above `cargo::WARNING_DETAIL_MAX`: the
/// roll-up keeps every warning's COUNT and drops every warning's message text
/// and `file:line`. Naming only the parser's fixed discard set at that volume
/// is the misstatement ADR-011's 2026-09-24 amendment ranks below omission.
///
/// Gated on `warnings_rolled_up` rather than on the count crossing the bound,
/// because the parser's own choice is the fact — the flag is set by
/// [`crate::output::canonical::WarningChannel::RollUp`] together with the
/// strings it describes, so the two cannot disagree. Reading the bound here
/// would re-derive the verdict from a second place and could drift from it.
///
/// Returns `r.warnings` and not `r.warning_messages.len()`: the roll-up's
/// buckets are fewer than the warnings they stand for (51 warnings can be one
/// `dead_code` line), and the marker reports what the reader LOST, not how many
/// lines replaced it.
///
/// Both passthrough tiers report `0` for the same reason they do above: raw
/// carries every warning body, so there is no roll-up to disclose.
#[must_use]
fn rolled_up_warnings(result: &ParseResult<BuildResult>) -> usize {
    match result {
        ParseResult::Full(r) | ParseResult::Degraded(r, _) if r.warnings_rolled_up => r.warnings,
        _ => 0,
    }
}

// ============================================================================
// Exit-code policy
// ============================================================================

/// Narrow a child process's exit status to the `u8` code skim returns for it.
///
/// Out-of-range (Windows NTSTATUS, negative) is a failure, never a success.
///
/// Three kinds of input, one rule — **any** non-zero child status stays
/// non-zero:
///
/// - `None` — the child was killed by a Unix signal and never chose a code
///   (`CommandOutput::exit_code` is `None` only there; see `runner.rs`).
///   Reported as `1`.
/// - A **negative** `i32` — Windows only. `ExitStatus::code()` never returns
///   `None` there and hands back the raw process status, so an NTSTATUS crash
///   arrives negative: `STATUS_ACCESS_VIOLATION` is `-1073741819`,
///   `STATUS_CONTROL_C_EXIT` is `-1073741510`. Both `i32::max(0)` and
///   `clamp(0, 255)` map such a status to `0`, i.e. report a **crashed child as
///   success**. `x86_64-pc-windows-msvc` is a shipped release target and a
///   wrapped build tool is exactly what a CI job keys `$?` on, so that is a
///   security-relevant gate inversion and the reason this helper exists.
///   Reported as `1`.
/// - `0..=255` — the child's own code, forwarded verbatim so `cargo`'s `101`
///   and `make`'s `2` survive the wrapper. A status above `255` (also
///   Windows-only) saturates to `255` rather than wrapping through a bare
///   `as u8`, which would turn `256` into `0` — the same inversion from the
///   other end.
///
/// `clamp(1, 255)` rather than `clamp(0, 255)` is the whole mechanism: the
/// lower bound is what keeps a negative status non-zero.
///
/// Eight further `clamp(0, 255) as u8` exit sites carry the identical
/// negative-reads-as-success hole and should adopt this helper:
/// `cmd/execution.rs:921`, `:1021`, `:1539`; `cmd/dispatch.rs:405`, `:582`;
/// `cmd/infra/gh/streaming.rs:562`; `cmd/test/shared.rs:479`;
/// `cmd/file/passthrough_stream.rs:192`. It is defined here only because this is
/// the module the fix landed in; its natural home is `cmd::execution`, beside
/// `pipe_closed_exit`, and `pub(crate) mod build` makes it importable from all
/// eight in the meantime.
#[must_use]
pub(crate) fn exit_code_from_status(status: Option<i32>) -> u8 {
    match status {
        // Signal kill: no code of the child's own, and not a success.
        None => 1,
        Some(0) => 0,
        Some(code) => code.clamp(1, 255) as u8,
    }
}

/// Resolve the process exit code for a parsed build run: `max(child, derived)`,
/// with the child's status narrowed fail-closed by [`exit_code_from_status`].
///
/// This is the same shape `execution::run_parsed_command_with_fallback` uses
/// (and the same `derive_exit` shape `cmd/test/cargo.rs` passes it).
///
/// # The child's code is the floor
///
/// Collapsing the result to `ExitCode::SUCCESS`/`FAILURE` flattened every
/// non-zero child exit to 1: `skim cargo check` on a crate that fails to
/// compile reported 1 instead of cargo's 101, and `skim make` with no makefile
/// reported 1 instead of make's 2 — callers keying on `$?` saw a code the raw
/// tool never produced.
///
/// # The parser's verdict raises, and deliberately never lowers
///
/// The derived code is the floor's complement, not a replacement: a parser that
/// saw failure still forces a non-zero exit when the child exited 0. Build
/// parsers do produce that combination — gradle prints `BUILD FAILED` on a zero
/// exit, and maven omits `BUILD SUCCESS` — so the `max` is required.
///
/// The **lowering** direction is gone on purpose, and this is the only record of
/// it: before the widening, a `ParseResult::Full(r)` with `r.success` could pull
/// the exit back down to 0, masking a non-zero child code. Only `cargo` can
/// reach that state at all — it derives `success` from the NDJSON
/// `build-finished` event independently of the child's code (`cargo.rs:438-441`,
/// `:499`), while gradle/maven/make/tsc all conjoin `exit_code == Some(0)` — and
/// when it does, the child's code is the answer a caller asked for. A parser
/// verdict must never be able to turn a failing child into a passing `$?`.
/// `test_parser_success_does_not_lower_a_non_zero_child_code` pins that.
///
/// `Passthrough` / `RawPassthrough` fall to `_`: they carry no parser verdict of
/// their own, so the child's code is the whole answer.
///
/// Taking the `max` in `u8` space after narrowing is equivalent to taking it in
/// `i32` space and narrowing after, because `derived` is only ever 0 or 1 and
/// the narrowing maps every failing status to at least 1.
#[must_use]
fn resolve_exit_code(child: Option<i32>, result: &ParseResult<BuildResult>) -> u8 {
    let derived: u8 = match result {
        ParseResult::Full(r) | ParseResult::Degraded(r, _) if !r.success => 1,
        _ => 0,
    };
    exit_code_from_status(child).max(derived)
}

/// Execute an external command, parse its output, and emit the result.
///
/// Three-tier degradation:
/// - `Full`: clean JSON/regex parse succeeded
/// - `Degraded`: partial parse with warnings
/// - `Passthrough`: raw output returned as-is
///
/// # Design note: divergence from [`super::run_parsed_command_with_mode`]
///
/// This function intentionally uses `bail!` on spawn failure rather than
/// returning `Ok(None)` as `execution::obtain_output` does. The difference is
/// semantic: build commands have no stdin-passthrough path, so a missing
/// executable is always a hard error rather than a soft "try stdin instead"
/// fallback. The two patterns are not consolidatable without changing that
/// behaviour.
///
/// # Arguments
///
/// * `program` - The executable name (e.g., "cargo", "tsc")
/// * `args` - Arguments to pass to the program
/// * `env_vars` - Environment variable overrides for the child process
/// * `install_hint` - Hint message shown if the program is not found
/// * `parser` - Function to parse the `CommandOutput` into a `ParseResult<BuildResult>`
pub(super) fn run_parsed_command(
    program: &str,
    args: &[String],
    env_vars: &[(&str, &str)],
    install_hint: &str,
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
    parser: fn(&CommandOutput) -> ParseResult<BuildResult>,
) -> anyhow::Result<ExitCode> {
    let runner = CommandRunner::new();

    let str_args: Vec<&str> = args.iter().map(String::as_str).collect();

    let output = match runner.run_with_env(program, &str_args, env_vars) {
        Ok(output) => output,
        Err(e) => {
            if crate::runner::is_spawn_error(&e) {
                anyhow::bail!(
                    "{program}: command not found\n\
                     Hint: {install_hint}"
                );
            }
            return Err(e);
        }
    };

    // Strip ANSI escape codes before parsing. Some build tools emit color codes
    // even with NO_COLOR=1, matching the shared run_parsed_command_with_mode pattern.
    let output = CommandOutput {
        stdout: crate::output::strip_ansi(&output.stdout),
        stderr: crate::output::strip_ansi(&output.stderr),
        ..output
    };

    let result = parser(&output);

    // Emit markers to stderr (warnings, notices)
    let _ = result.emit_markers(&mut std::io::stderr().lock());

    // Combine stdout+stderr for stats, analytics, and the net-savings guard.
    // "raw" for build = stdout + stderr (both carry diagnostic content).
    // Hold as Cow to avoid an unconditional String clone: Borrowed when stderr
    // is empty (fast path), Owned only when both streams are non-empty.
    //
    // MEASUREMENT ONLY. This merged view is the ADR-001 guard baseline and the
    // analytics/`--show-stats` input, and it must stay merged: what the user
    // would have seen with skim bypassed entirely is both streams together.
    // It is deliberately NOT what gets emitted — writing it to stdout is what
    // put the child's stderr on skim's fd 1, so the raw-emission arms below
    // hand `output.stdout` and `output.stderr` to `emit_raw_passthrough_split`
    // instead. Separating the two leaves the compress/no-compress decision and
    // every recorded token count exactly as they were.
    let raw_cow = super::combine_output(&output);

    // Net-savings guard (Cluster C / #317):
    // Build handlers always emit text (no --json path through this function).
    // Skip the guard when the tier is already "passthrough" (raw IS the body).
    //
    // "raw" baseline = combine_output (stdout+stderr) to match what the user
    // would see if skim were bypassed entirely.

    // Diagnostics the parser summarised, for the ADR-011 class-1 disclosure in
    // the `Keep` arm below. Extracted to [`summarised_diagnostics`], which is
    // where the "parser's own count, not a re-scan" rule is documented and which
    // is unit-testable without a spawn.
    let diagnostics = summarised_diagnostics(&result);
    // Same disclosure, second class. Read here rather than in the `Keep` arm
    // for the same reason as the line above: `result.content()` borrows
    // `result` for the rest of the function.
    let rolled_up = rolled_up_warnings(&result);

    let content = result.content();
    let tier_name = result.tier_name();
    let effective_tier = if tier_name != "passthrough" {
        match crate::cmd::execution::savings_decision(raw_cow.as_ref(), content) {
            crate::cmd::execution::SavingsDecision::Keep => {
                if !content.is_empty()
                    && crate::cmd::execution::write_line_to_stdout(content)?
                        == crate::cmd::execution::StdoutStatus::PipeClosed
                {
                    return Ok(crate::cmd::execution::pipe_closed_exit());
                }
                // ADR-011 class-1 disclosure, gated on TWO conditions.
                //
                // (1) `diagnostics > 0`: a build with nothing to report drops no
                //     diagnostic bodies, so the summary line IS the whole truth
                //     and a marker would be a false claim of loss.
                // (2) This arm only. On the `Passthrough` arm below the child's
                //     own bytes reach the reader with every snippet, `help:` line
                //     and explain hint intact — disclosing a loss there would
                //     describe something that did not happen.
                //
                // Unconditional by class: loss-bearing, therefore NOT gated on
                // SKIM_DEBUG. A closed stdout pipe returns above, so the marker
                // is never printed to a reader who has already departed.
                //
                // NOT charged against the guard. `savings_decision` above prices
                // the stdout bodies only, exactly as it did before this marker
                // existed, so the compress/no-compress verdict and every recorded
                // token count are untouched. It reaches
                // `output::fidelity::decide`, which is literally
                // `decide_with_notice(raw, compressed, None)` — so
                // `fidelity::decide_with_notice` is the gate that would charge
                // this marker, and `savings_decision` carries no `notice`
                // parameter deliberately: the command path's stdout accounting
                // still has the success-line hole the ADR-011 census recorded,
                // and a stderr-only charge would make a partial accounting read
                // as a complete one. Charge both together or neither.
                if diagnostics > 0 {
                    let _ = crate::cmd::execution::write_line_to_stderr(
                        &crate::output::diagnostics_summary_marker(program, diagnostics, rolled_up),
                    );
                }
                tier_name
            }
            crate::cmd::execution::SavingsDecision::Passthrough => {
                // Emit raw verbatim, each stream on the descriptor the child
                // wrote it to. The guard compared against `raw_cow` (merged),
                // but emitting `raw_cow` would relocate the child's stderr onto
                // skim's stdout — the bytes are identical, the descriptors are
                // not, and only the descriptors are observable to `2>`.
                //
                // ============================================================
                // PF-024 IS UNREMEDIATED HERE — "RAW" MEANS THE INJECTED
                // COMMAND'S OUTPUT, NOT THE USER'S
                // ============================================================
                //
                // `raw` on this arm is `output.stdout`/`output.stderr` from the
                // command skim SYNTHESISED, not the one the user typed.
                // `cargo.rs::run_with_json_format` injects
                // `--message-format=json` whenever the user supplied no
                // `--message-format`, so a user who typed `skim cargo build`
                // and lands here is served NDJSON — a different FORMAT from the
                // one they asked for, and (measured elsewhere on `diff` and
                // `git status`) typically MORE bytes than never invoking skim.
                // The ADR-001 baseline above reads the same injected stream, so
                // the guard's "did I help?" question is asked against a command
                // the user never ran. That is PF-024's literal defect, and it is
                // open for the whole build family: neither `raw_override` nor
                // `RawFallback` appears anywhere under `cmd/build/`.
                //
                // WHY IT IS RECORDED RATHER THAN FIXED HERE. The two available
                // fixes both cost more than this change is scoped for.
                // (1) `raw_override` — the field `cmd/git/mod.rs` uses — is
                //     EAGER: it re-runs the user's un-injected command to have
                //     its bytes on hand, which doubles the cost of every build,
                //     and a build is the most expensive thing skim wraps.
                // (2) Threading a lazy `RawFallback` (re-run only when the
                //     guard actually decides to serve raw) is the intended
                //     eventual fix and is the right shape, but it belongs in
                //     `cmd/execution.rs` next to `ParsedCommandConfig`, so it is
                //     a wider change than this emission split.
                //
                // Do not read the `gh` half of this branch — where PF-024 WAS
                // closed — as evidence that the pitfall is retired. It is not,
                // and silence here is what would make it look retired.
                let (tier, status) = crate::cmd::execution::emit_raw_passthrough_split(
                    &output.stdout,
                    &output.stderr,
                )?;
                if status == crate::cmd::execution::StdoutStatus::PipeClosed {
                    return Ok(crate::cmd::execution::pipe_closed_exit());
                }
                tier
            }
        }
    } else {
        // Already passthrough — the parser re-encoded nothing, so serve the
        // child's own bytes split across fd 1 / fd 2 rather than `content`,
        // which every tier-3 parser builds by merging the two streams (and
        // which `cargo fmt` additionally trims). Emitting the streams directly
        // also drops the unconditional trailing newline `write_line_to_stdout`
        // appended, so the forward is byte-faithful in both directions.
        let (tier, status) =
            crate::cmd::execution::emit_raw_passthrough_split(&output.stdout, &output.stderr)?;
        if status == crate::cmd::execution::StdoutStatus::PipeClosed {
            return Ok(crate::cmd::execution::pipe_closed_exit());
        }
        tier
    };

    // ========================================================================
    // TWO DELIBERATE, USER-VISIBLE DIVERGENCES FROM THE PRE-SPLIT EMISSION —
    // DO NOT "RESTORE" EITHER OF THEM
    // ========================================================================
    //
    // Both raw arms above hand `output.stdout`/`output.stderr` to
    // `emit_raw_passthrough_split`, which is byte-faithful by construction and
    // therefore differs from what this function used to write, in two ways a
    // reader will notice:
    //
    // (1) DESCRIPTORS. Diagnostics the child wrote to fd 2 now LEAVE on fd 2,
    //     where `combine_output` used to merge them onto fd 1. So
    //     `skim cargo fmt --check > out.txt` captures less than it did, and
    //     `2> err.txt` captures more. The relocation was the defect and the
    //     split is the fix — see `emit_raw_passthrough_split`'s own doc comment
    //     for why concatenating two streams into one descriptor is not a
    //     faithful forward.
    //
    // (2) NO TRAILING NEWLINE, on either arm. `emit_raw_passthrough_split`
    //     passes `ensure_trailing_newline: false` for both streams, so a child
    //     whose output does not end in `\n` is forwarded without one — where the
    //     previous path appended one unconditionally on the already-passthrough
    //     arm and conditionally on the other, and where `combine_output`'s join
    //     newline also no longer lands on fd 1. This is not an oversight and it
    //     is not fixable by adding a `writeln!`: a byte skim invents is a byte
    //     the raw tool did not emit, which is precisely what "forward raw
    //     verbatim" forbids. A later reader who finds the missing final newline
    //     surprising is looking at the contract, not at a bug.
    //
    // Both are observable to anyone piping or redirecting `skim cargo …`, so
    // they belong in the CHANGELOG and in CLAUDE.md — not only here.

    // Report token stats if requested. count_token_pair takes &str so we
    // borrow through the Cow without forcing an allocation.
    //
    // PRICE WHAT WAS EMITTED, NOT WHAT WAS PARSED. On both raw arms the reader
    // received `output.stdout` + `output.stderr` and never `content`, which
    // every tier-3 parser builds by merging the two streams and which
    // `cargo fmt` additionally TRIMS. Charging `content` against the raw
    // baseline there reports a saving no reader received; on the
    // already-passthrough arm that report is newly wrong, because pre-split
    // `content` WAS the emitted body. `effective_tier` is the record of which
    // arm ran, so it is also the correct selector: a raw arm served the child's
    // own bytes, whose honest pair is raw against raw — 0% saved.
    //
    // KNOWN REMAINING DIVERGENCE, recorded rather than left silent: the
    // analytics row below still passes `content` as its compressed body while
    // labelling the row `"passthrough"`, so such a row can persist a saving that
    // was never served. Not fixed here because it changes recorded
    // `token_savings` semantics — a wider blast radius than one `--show-stats`
    // stderr line, and it belongs with the analytics schema rather than with
    // this emission change.
    if show_stats {
        let served: &str = if effective_tier == "passthrough" {
            raw_cow.as_ref()
        } else {
            content
        };
        let (orig, comp) = crate::tokens::count_token_pair(raw_cow.as_ref(), served);
        crate::process::report_token_stats(orig, comp, "");
    }

    // Exit code: [`resolve_exit_code`] owns the whole rule — the child's code is
    // the floor, a negative (Windows NTSTATUS) child code is a failure rather
    // than a success, and the parser's verdict may only raise it. Both
    // directions and the removed lowering branch are documented there.
    //
    // FORWARDING THE CHILD'S CODE OVERLAPS SKIM'S OWN EXIT TABLE, deliberately.
    // `skim make` with no makefile now exits 2 — the code CLAUDE.md's exit-code
    // table reserves for skim's own parse error — and a failed compile through
    // `skim cargo check` exits 101. Forwarding is the point: a wrapper that
    // renumbers its child's status is not transparent, and a caller cannot tell
    // "make found no makefile" from "skim exits 1 for everything". So the
    // documented table is what has to widen, not this line. CLAUDE.md and the
    // CHANGELOG both have to say that the build family forwards the child's code
    // and that 2 is therefore no longer skim-exclusive.
    let code = resolve_exit_code(output.exit_code, &result);

    // Record analytics (fire-and-forget, non-blocking).
    // Use effective_tier (may be "passthrough" if the net-savings guard fired).
    // try_record_command takes ownership, so convert to String here — the
    // single call site where ownership is actually required.
    crate::analytics::try_record_command(
        rec.with_tier(effective_tier),
        raw_cow.into_owned(),
        content.to_string(),
        super::format_analytics_label("build", program, &args.join(" ")),
        output.duration,
    );

    Ok(ExitCode::from(code))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// A `BuildResult` carrying only the verdict the exit rule reads.
    fn verdict(success: bool) -> BuildResult {
        BuildResult::new(success, 0, 0, None, Vec::new())
    }

    // ------------------------------------------------------------------------
    // exit_code_from_status — any non-zero child status stays non-zero
    // ------------------------------------------------------------------------

    #[test]
    fn test_signal_kill_is_a_failure() {
        // `exit_code` is `None` only on a Unix signal kill (runner.rs).
        assert_eq!(exit_code_from_status(None), 1);
    }

    #[test]
    fn test_zero_stays_zero() {
        assert_eq!(exit_code_from_status(Some(0)), 0);
    }

    #[test]
    fn test_child_codes_are_forwarded_verbatim() {
        // cargo's 101 (compile failure) and make's 2 (no makefile) are the codes
        // the widening exists to preserve.
        assert_eq!(exit_code_from_status(Some(101)), 101);
        assert_eq!(exit_code_from_status(Some(2)), 2);
        assert_eq!(exit_code_from_status(Some(1)), 1);
        assert_eq!(exit_code_from_status(Some(255)), 255);
    }

    #[test]
    fn test_negative_windows_status_never_reads_as_success() {
        // Windows `ExitStatus::code()` returns the raw process status, so an
        // NTSTATUS crash arrives negative (STATUS_ACCESS_VIOLATION,
        // STATUS_CONTROL_C_EXIT). `max(0)` and `clamp(0, 255)` each map those to
        // 0 — a crashed child reported as a passing build.
        for status in [-1_073_741_819, -1_073_741_510, -1, i32::MIN] {
            let code = exit_code_from_status(Some(status));
            assert_ne!(code, 0, "negative status must not exit 0");
            assert_eq!(code, 1, "negative status must exit 1");
        }
    }

    #[test]
    fn test_out_of_range_high_status_saturates_instead_of_wrapping() {
        // A bare `as u8` would turn 256 into 0 — the same inversion from the
        // other end of the range.
        assert_ne!(exit_code_from_status(Some(256)), 0);
        assert_eq!(exit_code_from_status(Some(256)), 255);
        assert_eq!(exit_code_from_status(Some(i32::MAX)), 255);
    }

    // ------------------------------------------------------------------------
    // resolve_exit_code — the two arms a crashed build actually reaches
    // ------------------------------------------------------------------------

    #[test]
    fn test_negative_child_code_is_non_zero_on_the_passthrough_arm() {
        // `Passthrough` sets no derived verdict, so nothing else raises the
        // floor above zero: this arm is where the inversion was observable.
        let result = ParseResult::<BuildResult>::Passthrough("raw".to_string());
        let code = resolve_exit_code(Some(-1_073_741_819), &result);
        assert_ne!(code, 0, "a crashed child must not exit 0");
        assert_eq!(code, 1);
    }

    #[test]
    fn test_negative_child_code_is_non_zero_on_the_raw_passthrough_arm() {
        let result = ParseResult::<BuildResult>::RawPassthrough;
        let code = resolve_exit_code(Some(-1_073_741_819), &result);
        assert_ne!(code, 0, "a crashed child must not exit 0");
        assert_eq!(code, 1);
    }

    #[test]
    fn test_signal_kill_is_non_zero_on_both_passthrough_arms() {
        let passthrough = ParseResult::<BuildResult>::Passthrough(String::new());
        assert_eq!(resolve_exit_code(None, &passthrough), 1);
        let raw = ParseResult::<BuildResult>::RawPassthrough;
        assert_eq!(resolve_exit_code(None, &raw), 1);
    }

    #[test]
    fn test_parser_success_does_not_lower_a_non_zero_child_code() {
        // Pins the WIDENING as deliberate. Pre-widening, `Full(r)` with
        // `r.success` could pull the exit down to 0. Only cargo reaches that
        // state — it derives `success` from the NDJSON `build-finished` event
        // independently of the child's code — and when it does, the child's 101
        // is the answer the caller asked for.
        let result = ParseResult::Full(verdict(true));
        assert_eq!(resolve_exit_code(Some(101), &result), 101);
    }

    #[test]
    fn test_parser_failure_raises_a_zero_child_code() {
        // gradle prints `BUILD FAILED` on a zero exit; maven omits
        // `BUILD SUCCESS`. The derived verdict is what keeps those non-zero.
        let full = ParseResult::Full(verdict(false));
        assert_eq!(resolve_exit_code(Some(0), &full), 1);
        let degraded = ParseResult::Degraded(verdict(false), vec!["m".to_string()]);
        assert_eq!(resolve_exit_code(Some(0), &degraded), 1);
    }

    #[test]
    fn test_parser_failure_does_not_mask_a_larger_child_code() {
        let degraded = ParseResult::Degraded(verdict(false), vec!["m".to_string()]);
        assert_eq!(resolve_exit_code(Some(101), &degraded), 101);
    }

    #[test]
    fn test_clean_run_exits_zero() {
        let result = ParseResult::Full(verdict(true));
        assert_eq!(resolve_exit_code(Some(0), &result), 0);
    }

    // ------------------------------------------------------------------------
    // summarised_diagnostics
    // ------------------------------------------------------------------------

    #[test]
    fn test_summarised_diagnostics_counts_errors_and_warnings() {
        let inner = BuildResult::new(false, 3, 2, None, Vec::new());
        assert_eq!(summarised_diagnostics(&ParseResult::Full(inner)), 5);
    }

    #[test]
    fn test_summarised_diagnostics_is_zero_when_nothing_was_reported() {
        // The `Keep` arm gates the ADR-011 marker on `> 0`: a clean build drops
        // no diagnostic bodies, so a marker there would claim a loss that did
        // not happen.
        let result = ParseResult::Full(verdict(true));
        assert_eq!(summarised_diagnostics(&result), 0);
    }

    #[test]
    fn test_summarised_diagnostics_is_zero_on_both_passthrough_arms() {
        let body = ParseResult::<BuildResult>::Passthrough("error: boom".into());
        assert_eq!(summarised_diagnostics(&body), 0);
        let raw = ParseResult::<BuildResult>::RawPassthrough;
        assert_eq!(summarised_diagnostics(&raw), 0);
    }
}
