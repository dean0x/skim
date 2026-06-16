//! Package manager handler — dispatches to package manager parsers (#105)
//!
//! Called via flat dispatch: `skim <tool> [subcmd] [args...]`. Supported
//! tools: `npm`, `pnpm`, `pip`, `cargo`.

mod cargo;
mod npm;
mod pip;
mod pnpm;
mod yarn;

use std::process::ExitCode;

use crate::output::ParseResult;
use crate::runner::CommandOutput;

/// Known package manager tools that the pkg handler can dispatch to.
const KNOWN_TOOLS: &[&str] = &["cargo", "npm", "pip", "pnpm", "yarn"];

/// Entry point for `skim <tool> [subcmd] [args...]` (pkg handler).
///
/// If no tool is specified or `--help` / `-h` is passed, prints usage
/// and exits. Otherwise dispatches to the tool-specific handler.
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let (filtered_args, show_stats) = crate::cmd::extract_show_stats(args);
    let (json_args, json_output) = crate::cmd::extract_json_flag(&filtered_args);

    let Some((tool_name, tool_args)) = json_args.split_first() else {
        print_help();
        return Ok(ExitCode::SUCCESS);
    };

    let rec = crate::analytics::RecordingContext {
        enabled: analytics.enabled,
        command_type: crate::analytics::CommandType::Pkg,
        parse_tier: None,
        session_id: analytics.session_id.as_deref(),
    };

    match tool_name.as_str() {
        "npm" => npm::run(tool_args, show_stats, json_output, rec),
        "pnpm" => pnpm::run(tool_args, show_stats, json_output, rec),
        "pip" => pip::run(tool_args, show_stats, json_output, rec),
        "cargo" => cargo::run(tool_args, show_stats, json_output, rec),
        "yarn" => yarn::run(tool_args, show_stats, json_output, rec),
        tool => {
            let safe_tool = crate::cmd::sanitize_for_display(tool);
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

/// Re-export shared helper for child module use.
pub(super) use super::combine_output;

/// Configuration for a package subcommand invocation.
///
/// Groups the constant parts of a `run_parsed_command_with_mode` call so
/// that each `run_*` function only specifies what differs (flag injection
/// and parse function).
pub(super) struct PkgSubcommandConfig<'a> {
    pub program: &'a str,
    pub subcommand: &'a str,
    /// Exit codes that mean "ran fine, findings present" rather than an
    /// unexpected failure (#317). Most subcommands exit `1` on findings, but
    /// `yarn audit` uses a severity bitmask (`1|2|4|8|16`, OR-combined), so it
    /// declares the full `1..=31` range. Threaded into `classify_exit` so a
    /// findings exit is compressed instead of raw-forwarded.
    pub expected_exit_codes: &'a [i32],
    /// Whether to forward child stderr verbatim on the compressed path (#317).
    /// All pkg subcommands are stdout-only parsers, so this is `false` at every
    /// construction site — but it must be explicit so every site is auditable and
    /// uniform with `ToolRunConfig` / `ParsedCommandConfig`.
    pub forward_stderr: bool,
    pub env_overrides: &'a [(&'a str, &'a str)],
    pub install_hint: &'a str,
}

/// Shared helper that eliminates the repetitive `run_*` boilerplate
/// across all package manager subcommand parsers.
///
/// Builds the argument list, applies flag injection, detects stdin,
/// and delegates to `run_parsed_command_with_mode`.
pub(super) fn run_pkg_subcommand<T>(
    config: PkgSubcommandConfig<'_>,
    user_args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
    inject_flags: impl FnOnce(&mut Vec<String>),
    parse_fn: impl FnOnce(&CommandOutput) -> ParseResult<T>,
) -> anyhow::Result<ExitCode>
where
    T: AsRef<str> + serde::Serialize,
{
    let mut cmd_args: Vec<String> = vec![config.subcommand.to_string()];
    cmd_args.extend(user_args.iter().cloned());
    inject_flags(&mut cmd_args);

    let use_stdin = crate::cmd::should_read_stdin(user_args);

    crate::cmd::run_parsed_command_with_mode(
        crate::cmd::ParsedCommandConfig {
            program: config.program,
            args: &cmd_args,
            env_overrides: config.env_overrides,
            install_hint: config.install_hint,
            use_stdin,
            show_stats,
            output_format: crate::cmd::OutputFormat::default(),
            family: "pkg",
            skip_ansi_strip: false,
            rec,
            // Per-subcommand: `1` for most (npm audit vulnerabilities, npm
            // outdated packages, pip check conflicts), the severity-bitmask
            // range for `yarn audit`. See `PkgSubcommandConfig`.
            expected_exit_codes: config.expected_exit_codes,
            // All pkg parsers consume stdout only; stderr is never forwarded.
            // Threaded from the construction site so every caller is explicit
            // and auditable (#317).
            forward_stderr: config.forward_stderr,
        },
        |output| parse_fn(output),
    )
}

fn print_help() {
    println!("skim <tool> [subcmd] [args...]");
    println!();
    println!("  Parse package manager output for AI context windows.");
    println!();
    println!("Available tools:");
    for tool in KNOWN_TOOLS {
        println!("  {tool}");
    }
    println!();
    println!("Examples:");
    println!("  skim npm install              Run npm install");
    println!("  skim npm audit                Run npm audit");
    println!("  skim npm outdated             Run npm outdated");
    println!("  skim pip install flask        Run pip install flask");
    println!("  skim pip check                Run pip check");
    println!("  skim cargo audit              Run cargo audit");
    println!("  skim pnpm install             Run pnpm install");
    println!("  npm install 2>&1 | skim npm install  Pipe npm output");
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::*;

    // ========================================================================
    // sanitize_for_display
    // ========================================================================

    #[test]
    fn test_sanitize_ascii_passthrough() {
        assert_eq!(
            crate::cmd::sanitize_for_display("hello-world"),
            "hello-world"
        );
    }

    #[test]
    fn test_sanitize_strips_escape_sequences() {
        // ANSI escape: \x1b[31m (set red color)
        let malicious = "\x1b[31mevil\x1b[0m";
        let sanitized = crate::cmd::sanitize_for_display(malicious);
        assert!(!sanitized.contains('\x1b'));
        assert_eq!(sanitized, "?[31mevil?[0m");
    }

    #[test]
    fn test_sanitize_truncates_long_input() {
        let long_input = "a".repeat(200);
        let sanitized = crate::cmd::sanitize_for_display(&long_input);
        assert_eq!(sanitized.len(), 64);
    }

    #[test]
    fn test_sanitize_replaces_control_chars() {
        let input = "hello\0world\ttab\nnewline";
        let sanitized = crate::cmd::sanitize_for_display(input);
        assert_eq!(sanitized, "hello?world?tab?newline");
    }

    // ========================================================================
    // combine_output
    // ========================================================================

    #[test]
    fn test_combine_output_empty_stderr() {
        let output = crate::runner::CommandOutput {
            stdout: "hello".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let combined = combine_output(&output);
        assert!(matches!(combined, Cow::Borrowed(_)));
        assert_eq!(combined.as_ref(), "hello");
    }

    #[test]
    fn test_combine_output_with_stderr() {
        let output = crate::runner::CommandOutput {
            stdout: "hello".to_string(),
            stderr: "warning".to_string(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let combined = combine_output(&output);
        assert!(matches!(combined, Cow::Owned(_)));
        assert_eq!(combined.as_ref(), "hello\nwarning");
    }
}
