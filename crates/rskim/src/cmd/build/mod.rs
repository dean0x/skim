//! Build output compression (#51)
//!
//! Executes build tools (cargo, clippy, tsc) and compresses their output
//! using three-tier parse degradation. Supports both direct invocation and
//! piped stdin.

pub(crate) mod cargo;
pub(crate) mod tsc;

use std::process::ExitCode;
use std::time::Duration;

use crate::output::canonical::BuildResult;
use crate::output::ParseResult;
use crate::runner::{CommandOutput, CommandRunner};

// ============================================================================
// Public dispatch
// ============================================================================

/// Dispatch the `build` subcommand.
///
/// Usage: `skim build {cargo|clippy|tsc} [args...]`
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

    match sub {
        Some("cargo") => cargo::run(remaining, show_stats, analytics.enabled),
        Some("clippy") => cargo::run_clippy(remaining, show_stats, analytics.enabled),
        Some("tsc") => tsc::run(remaining, show_stats, analytics.enabled),
        Some(unknown) => {
            let safe_unknown = crate::cmd::sanitize_for_display(unknown);
            anyhow::bail!(
                "unknown build tool: '{safe_unknown}'\n\n\
                 Usage: skim build {{cargo|clippy|tsc}} [args...]\n\n\
                 Supported tools: cargo, clippy, tsc"
            );
        }
        None => {
            anyhow::bail!(
                "missing required argument: <TOOL>\n\n\
                 Usage: skim build {{cargo|clippy|tsc}} [args...]\n\n\
                 Supported tools: cargo, clippy, tsc"
            );
        }
    }
}

fn print_help() {
    println!("skim build");
    println!();
    println!("  Execute build tools and compress output for AI context windows.");
    println!();
    println!("USAGE:");
    println!("  skim build <TOOL> [args...]");
    println!();
    println!("TOOLS:");
    println!("  cargo      Run cargo build with output compression");
    println!("  clippy     Run cargo clippy with output compression");
    println!("  tsc        Run TypeScript compiler with output compression");
    println!();
    println!("EXAMPLES:");
    println!("  skim build cargo");
    println!("  skim build cargo --release");
    println!("  skim build clippy -- -W clippy::pedantic");
    println!("  skim build tsc --noEmit");
}

// Shared helpers (user_has_flag, inject_flag_before_separator) are in crate::cmd

/// Execute an external command, parse its output, and emit the result.
///
/// Three-tier degradation:
/// - `Full`: clean JSON/regex parse succeeded
/// - `Degraded`: partial parse with warnings
/// - `Passthrough`: raw output returned as-is
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
    analytics_enabled: bool,
    parser: fn(&CommandOutput) -> ParseResult<BuildResult>,
) -> anyhow::Result<ExitCode> {
    let runner = CommandRunner::new(Some(Duration::from_secs(600)));

    let str_args: Vec<&str> = args.iter().map(String::as_str).collect();

    let output = match runner.run_with_env(program, &str_args, env_vars) {
        Ok(output) => output,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("failed to execute") {
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
    let stderr = std::io::stderr();
    let mut stderr_handle = stderr.lock();
    let _ = result.emit_markers(&mut stderr_handle);

    // Print the result content to stdout
    let content = result.content();
    if !content.is_empty() {
        println!("{content}");
    }

    // Combine stdout+stderr for stats and analytics
    let raw_text = if output.stderr.is_empty() {
        output.stdout.clone()
    } else {
        format!("{}\n{}", output.stdout, output.stderr)
    };

    // Report token stats if requested
    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(&raw_text, result.content());
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
    crate::analytics::try_record_command(
        analytics_enabled,
        raw_text,
        result.content().to_string(),
        format!("skim build {program} {}", args.join(" ")),
        crate::analytics::CommandType::Build,
        output.duration,
        Some(result.tier_name()),
    );

    Ok(exit_code)
}
