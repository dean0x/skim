//! Build output compression (#51)
//!
//! Handles build tool output for cargo, clippy, make, and tsc using three-tier
//! parse degradation. Called via flat dispatch (`skim tsc`) or multi-category
//! dispatch (`skim cargo build`, `skim cargo clippy`). Supports both direct
//! invocation and piped stdin.

pub(crate) mod cargo;
pub(crate) mod make;
pub(crate) mod tsc;

use std::process::ExitCode;
use std::time::Duration;

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
        Some("cargo") => cargo::run(remaining, show_stats, rec),
        Some("clippy") => cargo::run_clippy(remaining, show_stats, rec),
        Some("make") => make::run(remaining, show_stats, rec),
        Some("tsc") => tsc::run(remaining, show_stats, rec),
        Some(unknown) => {
            // Defensive branch: flat dispatch always prepends a known tool name
            // (cargo/clippy/make/tsc) before calling this function, so this arm is
            // only reachable via internal routing bugs. Use eprintln! + FAILURE
            // (not bail!) consistent with sibling handlers (pkg, lint, test).
            let safe_unknown = crate::cmd::sanitize_for_display(unknown);
            eprintln!(
                "skim: unknown subcommand '{safe_unknown}'\n\
                 Supported tools: cargo, clippy, make, tsc"
            );
            Ok(ExitCode::FAILURE)
        }
        None => {
            eprintln!(
                "skim: missing build tool\n\n\
                 Usage: skim cargo build [args...]\n\
                 Usage: skim make [args...]\n\
                 Usage: skim tsc [args...]\n\n\
                 Supported tools: cargo, clippy, make, tsc"
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_help() {
    println!("skim {{cargo build|cargo clippy|make|tsc}} [args...]");
    println!();
    println!("  Run build tools and compress output for AI context windows.");
    println!();
    println!("Available tools:");
    println!("  cargo      Run cargo build with output compression");
    println!("  clippy     Run cargo clippy with output compression");
    println!("  make       Run GNU make with output compression");
    println!("  tsc        Run TypeScript compiler with output compression");
    println!();
    println!("Flags:");
    println!("  --show-stats    Show token statistics");
    println!();
    println!("Examples:");
    println!("  skim cargo build");
    println!("  skim cargo build --release");
    println!("  skim cargo clippy -- -W clippy::pedantic");
    println!("  skim make");
    println!("  skim make -j4 all");
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
/// returning `Ok(None)` as [`super::obtain_output`] does. The difference is
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
    let runner = CommandRunner::new(Some(Duration::from_secs(600)));

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

    // Print the result content to stdout
    let content = result.content();
    if !content.is_empty() {
        println!("{content}");
    }

    // Combine stdout+stderr for stats and analytics.
    // Hold as Cow to avoid an unconditional String clone: Borrowed when stderr
    // is empty (fast path), Owned only when both streams are non-empty.
    let raw_cow = super::combine_output(&output);

    // Report token stats if requested. count_token_pair takes &str so we
    // borrow through the Cow without forcing an allocation.
    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(raw_cow.as_ref(), result.content());
        crate::process::report_token_stats(orig, comp, "");
    }

    // Determine exit code from the parsed result
    let exit_code = match &result {
        ParseResult::Full(build_result) | ParseResult::Degraded(build_result, _) => {
            if build_result.success {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        ParseResult::Passthrough(_) => {
            // Use the original process exit code
            match output.exit_code {
                Some(0) => ExitCode::SUCCESS,
                _ => ExitCode::FAILURE,
            }
        }
    };

    // Record analytics (fire-and-forget, non-blocking).
    // try_record_command takes ownership, so convert to String here — the
    // single call site where ownership is actually required.
    crate::analytics::try_record_command(
        rec.with_tier(result.tier_name()),
        raw_cow.into_owned(),
        result.content().to_string(),
        super::format_analytics_label("build", program, &args.join(" ")),
        output.duration,
    );

    Ok(exit_code)
}
