//! Package manager output compression (#105)
//!
//! Routes `skim pkg <tool> [subcmd] [args...]` to the appropriate package
//! manager parser. Currently supported tools: `npm`, `pnpm`, `pip`, `cargo`.

mod cargo;
mod npm;
mod pip;
mod pnpm;

use std::io::IsTerminal;
use std::process::ExitCode;

use crate::output::ParseResult;
use crate::runner::CommandOutput;

/// Known package manager tools that `skim pkg` can dispatch to.
const KNOWN_TOOLS: &[&str] = &["npm", "pnpm", "pip", "cargo"];

/// Entry point for `skim pkg <tool> [subcmd] [args...]`.
///
/// If no tool is specified or `--help` / `-h` is passed, prints usage
/// and exits. Otherwise dispatches to the tool-specific handler.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
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

    let tool = tool_name.as_str();

    match tool {
        "npm" => npm::run(tool_args, show_stats, json_output),
        "pnpm" => pnpm::run(tool_args, show_stats, json_output),
        "pip" => pip::run(tool_args, show_stats, json_output),
        "cargo" => cargo::run(tool_args, show_stats, json_output),
        _ => {
            let safe_tool = crate::cmd::sanitize_for_display(tool);
            eprintln!(
                "skim pkg: unknown tool '{safe_tool}'\n\
                 Available tools: {}\n\
                 Run 'skim pkg --help' for usage information",
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
    inject_flags: impl FnOnce(&mut Vec<String>),
    parse_fn: impl FnOnce(&CommandOutput) -> ParseResult<T>,
) -> anyhow::Result<ExitCode>
where
    T: AsRef<str> + serde::Serialize,
{
    let mut cmd_args: Vec<String> = vec![config.subcommand.to_string()];
    cmd_args.extend(user_args.iter().cloned());
    inject_flags(&mut cmd_args);

    let use_stdin = !std::io::stdin().is_terminal() && user_args.is_empty();

    crate::cmd::run_parsed_command_with_mode(
        crate::cmd::ParsedCommandConfig {
            program: config.program,
            args: &cmd_args,
            env_overrides: config.env_overrides,
            install_hint: config.install_hint,
            use_stdin,
            show_stats,
            command_type: crate::analytics::CommandType::Pkg,
            output_format: crate::cmd::OutputFormat::default(),
        },
        |output, _args| parse_fn(output),
    )
}

fn print_help() {
    println!("skim pkg <tool> [subcmd] [args...]");
    println!();
    println!("  Parse package manager output for AI context windows.");
    println!();
    println!("Available tools:");
    for tool in KNOWN_TOOLS {
        println!("  {tool}");
    }
    println!();
    println!("Examples:");
    println!("  skim pkg npm install              Run npm install");
    println!("  skim pkg npm audit                Run npm audit");
    println!("  skim pkg npm outdated             Run npm outdated");
    println!("  skim pkg pip install flask        Run pip install flask");
    println!("  skim pkg pip check                Run pip check");
    println!("  skim pkg cargo audit              Run cargo audit");
    println!("  skim pkg pnpm install             Run pnpm install");
    println!("  npm install 2>&1 | skim pkg npm install  Pipe npm output");
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
        assert_eq!(crate::cmd::sanitize_for_display("hello-world"), "hello-world");
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
