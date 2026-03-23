//! Subcommand infrastructure for skim CLI.
//!
//! Provides pre-parse routing for optional subcommands while keeping
//! backward compatibility with file-first invocations. Also provides shared
//! helper functions used by subcommand parsers (arg inspection, flag injection,
//! command execution with three-tier parse degradation).

mod build;
mod completions;
mod discover;
mod git;
mod init;
mod learn;
mod rewrite;
mod session;
mod test;

use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;

use crate::output::ParseResult;
use crate::runner::{CommandOutput, CommandRunner};

/// Known subcommands that the pre-parse router will recognize.
///
/// IMPORTANT: Only register subcommands we will actually implement.
/// Keep this list exact — no broad patterns. See GRANITE lesson #336.
pub(crate) const KNOWN_SUBCOMMANDS: &[&str] = &[
    "build",
    "completions",
    "discover",
    "git",
    "init",
    "learn",
    "rewrite",
    "test",
];

/// Check whether `name` is a registered subcommand.
pub(crate) fn is_known_subcommand(name: &str) -> bool {
    KNOWN_SUBCOMMANDS.contains(&name)
}

// ============================================================================
// Shared helpers for subcommand parsers
// ============================================================================

/// Check whether the user-supplied args already contain any of the given flags.
///
/// Accepts multiple flag prefixes (e.g., `&["--color", "-c"]`) for checking
/// equivalent flag aliases. Matches both `--flag` and `--flag=value` forms.
pub(crate) fn user_has_flag(args: &[String], flags: &[&str]) -> bool {
    args.iter().any(|a| {
        flags.iter().any(|flag| {
            a == flag || (a.starts_with(flag) && a.as_bytes().get(flag.len()) == Some(&b'='))
        })
    })
}

/// Inject a flag before the `--` separator, or at the end if no separator exists.
///
/// This ensures injected flags (like `--message-format=json`) appear in the
/// flags section, not after `--` where they would be treated as positional args
/// by the underlying tool.
pub(crate) fn inject_flag_before_separator(args: &mut Vec<String>, flag: &str) {
    if let Some(pos) = args.iter().position(|a| a == "--") {
        args.insert(pos, flag.to_string());
    } else {
        args.push(flag.to_string());
    }
}

/// Execute an external command, parse its output, and emit the result.
///
/// Convenience wrapper that auto-detects stdin piping via `is_terminal()`.
/// Use [`run_parsed_command_with_mode`] when you need explicit control
/// over stdin vs execute behavior.
#[allow(dead_code)]
pub(crate) fn run_parsed_command<T>(
    program: &str,
    args: &[String],
    env_overrides: &[(&str, &str)],
    install_hint: &str,
    parse: impl FnOnce(&CommandOutput, &[String]) -> ParseResult<T>,
) -> anyhow::Result<ExitCode>
where
    T: AsRef<str>,
{
    let use_stdin = !io::stdin().is_terminal();
    run_parsed_command_with_mode(program, args, env_overrides, install_hint, use_stdin, parse)
}

/// Execute an external command, parse its output, and emit the result.
///
/// This is the standard entry point for subcommand parsers that follow the
/// three-tier degradation pattern. It handles:
/// 1. Stdin piping (when `use_stdin` is true, read stdin instead of running command)
/// 2. Running the command with environment overrides
/// 3. Calling the parser function on the output
/// 4. Emitting the parsed result to stdout
/// 5. Mapping the exit code
///
/// `use_stdin` — when `true`, reads stdin instead of spawning the command.
/// Callers should set this based on their own heuristics (e.g., only read
/// stdin when no user args are provided AND stdin is piped).
pub(crate) fn run_parsed_command_with_mode<T>(
    program: &str,
    args: &[String],
    env_overrides: &[(&str, &str)],
    install_hint: &str,
    use_stdin: bool,
    parse: impl FnOnce(&CommandOutput, &[String]) -> ParseResult<T>,
) -> anyhow::Result<ExitCode>
where
    T: AsRef<str>,
{
    /// Maximum bytes we will read from stdin (64 MiB), consistent with the
    /// runner's `MAX_OUTPUT_BYTES` limit for command output pipes.
    const MAX_STDIN_BYTES: u64 = 64 * 1024 * 1024;

    let output = if use_stdin {
        // Piped stdin mode: read stdin instead of executing the command.
        // Size-limited to prevent unbounded memory growth from runaway pipes.
        let mut stdin_buf = String::new();
        let bytes_read = io::stdin()
            .take(MAX_STDIN_BYTES)
            .read_to_string(&mut stdin_buf)?;
        if bytes_read as u64 >= MAX_STDIN_BYTES {
            anyhow::bail!("stdin input exceeded 64 MiB limit");
        }
        CommandOutput {
            stdout: stdin_buf,
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        }
    } else {
        // Execute the command
        let runner = CommandRunner::new(Some(std::time::Duration::from_secs(300)));
        let args_str: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        match runner.run_with_env(program, &args_str, env_overrides) {
            Ok(out) => out,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("failed to execute") {
                    eprintln!("error: '{program}' not found");
                    eprintln!("hint: {install_hint}");
                    return Ok(ExitCode::FAILURE);
                }
                return Err(e);
            }
        }
    };

    // Strip ANSI escape codes from output before parsing. Even with NO_COLOR=1
    // set on the child process, some tools may still emit escape sequences.
    // This is a cheap, universally useful safety net for all parsers.
    let output = CommandOutput {
        stdout: crate::output::strip_ansi(&output.stdout),
        stderr: crate::output::strip_ansi(&output.stderr),
        ..output
    };

    let result = parse(&output, args);

    // Emit markers (warnings/notices) to stderr
    let stderr_stream = io::stderr();
    let mut stderr_handle = stderr_stream.lock();
    let _ = result.emit_markers(&mut stderr_handle);
    drop(stderr_handle);

    // Emit content to stdout
    let stdout_stream = io::stdout();
    let mut stdout_handle = stdout_stream.lock();
    write!(stdout_handle, "{}", result.content())?;
    // Ensure trailing newline
    if !result.content().is_empty() && !result.content().ends_with('\n') {
        writeln!(stdout_handle)?;
    }
    stdout_handle.flush()?;

    // Map exit code: preserve full 0-255 exit code granularity from the
    // underlying process. This maintains documented semantics (0=success,
    // 1=error, 2=parse error, 3=unsupported language) for downstream consumers.
    let code = output.exit_code.unwrap_or(1);
    Ok(ExitCode::from(code.clamp(0, 255) as u8))
}

/// Dispatch a subcommand by name. Returns the process exit code.
///
/// Exit code semantics (GRANITE lesson — exit code corruption is P1):
/// - `--help` / `-h`: prints description to stdout, returns SUCCESS
/// - Otherwise: prints "not yet implemented" to stderr, returns FAILURE
pub(crate) fn dispatch(subcommand: &str, args: &[String]) -> anyhow::Result<ExitCode> {
    if !is_known_subcommand(subcommand) {
        anyhow::bail!(
            "Unknown subcommand: '{subcommand}'\n\
             Available subcommands: {}\n\
             Run 'skim --help' for usage information",
            KNOWN_SUBCOMMANDS.join(", ")
        );
    }

    // Dispatch implemented subcommands
    match subcommand {
        "build" => return build::run(args),
        "completions" => return completions::run(args),
        "discover" => return discover::run(args),
        "git" => return git::run(args),
        "learn" => return learn::run(args),
        "init" => return init::run(args),
        "rewrite" => return rewrite::run(args),
        "test" => return test::run(args),
        _ => {}
    }

    // Check for --help / -h in remaining args
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        println!("skim {subcommand}");
        println!();
        println!("  Status: not yet implemented");
        println!();
        println!("  This subcommand is planned for a future release.");
        println!("  See: https://github.com/dean0x/skim/issues/19");
        return Ok(ExitCode::SUCCESS);
    }

    eprintln!("skim {subcommand}: not yet implemented");
    eprintln!();
    eprintln!("This subcommand is planned for a future release.");
    eprintln!("See: https://github.com/dean0x/skim/issues/19");

    Ok(ExitCode::FAILURE)
}
