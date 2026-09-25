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
                tier_name
            }
            crate::cmd::execution::SavingsDecision::Passthrough => {
                // Emit raw verbatim, each stream on the descriptor the child
                // wrote it to. The guard compared against `raw_cow` (merged),
                // but emitting `raw_cow` would relocate the child's stderr onto
                // skim's stdout — the bytes are identical, the descriptors are
                // not, and only the descriptors are observable to `2>`.
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

    // Report token stats if requested. count_token_pair takes &str so we
    // borrow through the Cow without forcing an allocation.
    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(raw_cow.as_ref(), content);
        crate::process::report_token_stats(orig, comp, "");
    }

    // Exit code: `max(child, derived)`, the same shape
    // `execution::run_parsed_command_with_fallback` uses (and the same
    // `derive_exit` shape `cmd/test/cargo.rs` passes it).
    //
    // The child's own code is the floor. Collapsing the result to
    // `ExitCode::SUCCESS`/`FAILURE` flattened every non-zero child exit to 1:
    // `skim cargo check` on a crate that fails to compile reported 1 instead of
    // cargo's 101, and `skim make` with no makefile reported 1 instead of
    // make's 2 — callers keying on `$?` saw a code the raw tool never produced.
    //
    // The derived code is the floor's complement, not a replacement: a parser
    // that saw failure still forces a non-zero exit when the child exited 0.
    // Build parsers do produce that combination — gradle prints `BUILD FAILED`
    // on a zero exit, and maven omits `BUILD SUCCESS` — so `max` is required
    // here, and `unwrap_or(1)` keeps a signal kill (`None`) non-zero.
    // Passthrough / RawPassthrough fall to `_`: they carry no parser verdict of
    // their own, so the child's code is the whole answer.
    let derived_exit = match &result {
        ParseResult::Full(r) | ParseResult::Degraded(r, _) if !r.success => Some(1),
        _ => None,
    };
    let code = output.exit_code.unwrap_or(1).max(derived_exit.unwrap_or(0));

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

    Ok(ExitCode::from(code.clamp(0, 255) as u8))
}
