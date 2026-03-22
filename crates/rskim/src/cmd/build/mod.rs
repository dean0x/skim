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
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    // Handle --help / -h
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let sub = args.first().map(String::as_str);
    let remaining = if args.len() > 1 { &args[1..] } else { &[] };

    match sub {
        Some("cargo") => cargo::run(remaining),
        Some("clippy") => cargo::run_clippy(remaining),
        Some("tsc") => tsc::run(remaining),
        Some(unknown) => {
            anyhow::bail!(
                "unknown build tool: '{unknown}'\n\n\
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

// ============================================================================
// Shared helpers
// ============================================================================

/// Check whether user already passed a flag matching the given prefix.
///
/// Returns `true` if any arg equals `prefix` exactly or starts with
/// `prefix=` (e.g., `--message-format=json`). This avoids false positives
/// from hypothetical flags that share a common prefix.
pub(super) fn user_has_flag(args: &[String], prefix: &str) -> bool {
    args.iter()
        .any(|a| a == prefix || a.starts_with(&format!("{prefix}=")))
}

/// Inject a flag before the `--` separator, or at the end if no separator exists.
///
/// This ensures injected flags (like `--message-format=json`) appear in the
/// correct position relative to any `--` separator that separates cargo flags
/// from rustc flags.
pub(super) fn inject_flag_before_separator(args: &mut Vec<String>, flag: &str) {
    if let Some(pos) = args.iter().position(|a| a == "--") {
        args.insert(pos, flag.to_string());
    } else {
        args.push(flag.to_string());
    }
}

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

    Ok(exit_code)
}
