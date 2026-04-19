//! Subcommand infrastructure for skim CLI.
//!
//! Provides pre-parse routing for optional subcommands while keeping
//! backward compatibility with file-first invocations. Also provides shared
//! helper functions used by subcommand parsers (arg inspection, flag injection,
//! command execution with three-tier parse degradation).

mod agents;
mod build;
mod completions;
mod discover;
mod file;
mod git;
mod hook_log;
mod hooks;
mod infra;
mod init;
mod integrity;
mod learn;
mod lint;
mod log;
mod pkg;
mod rewrite;
mod session;
mod stats;
mod test;

use std::borrow::Cow;
use std::io::{self, Read, Write};
use std::process::ExitCode;

use crate::output::ParseResult;
use crate::runner::{CommandOutput, CommandRunner};

/// Known subcommands that the pre-parse router will recognize.
///
/// IMPORTANT: Only register subcommands we will actually implement.
/// Keep this list exact — no broad patterns. See GRANITE lesson #336.
pub(crate) const KNOWN_SUBCOMMANDS: &[&str] = &[
    "agents",
    "build",
    "completions",
    "discover",
    "file",
    "git",
    "infra",
    "init",
    "learn",
    "lint",
    "log",
    "pkg",
    "rewrite",
    "stats",
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

/// Extract the `--show-stats` flag from args, returning filtered args and whether
/// the flag was present.
///
/// This centralises the pattern that was previously copy-pasted across build,
/// git, and test subcommand entry points.
pub(crate) fn extract_show_stats(args: &[String]) -> (Vec<String>, bool) {
    let show_stats = args.iter().any(|a| a == "--show-stats");
    let filtered: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "--show-stats")
        .cloned()
        .collect();
    (filtered, show_stats)
}

/// Extract the `--json` flag from args, returning filtered args and whether
/// the flag was present.
///
/// This centralises the pattern that was previously copy-pasted across git,
/// lint, and pkg subcommand entry points.
pub(crate) fn extract_json_flag(args: &[String]) -> (Vec<String>, bool) {
    let is_json = args.iter().any(|a| a == "--json");
    let filtered: Vec<String> = args
        .iter()
        .filter(|a| a.as_str() != "--json")
        .cloned()
        .collect();
    (filtered, is_json)
}

/// Extract `--json` flag from args and return the corresponding [`OutputFormat`].
///
/// Convenience wrapper that combines [`extract_json_flag`] with `OutputFormat`
/// conversion, keeping subcommand handlers consistent.
pub(crate) fn extract_output_format(args: &[String]) -> (Vec<String>, OutputFormat) {
    let (filtered, is_json) = extract_json_flag(args);
    let fmt = if is_json {
        OutputFormat::Json
    } else {
        OutputFormat::Text
    };
    (filtered, fmt)
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
/// Bundles the three boolean flags that every family dispatcher receives
/// identically, reducing the 4-parameter `(args, show_stats, json_output,
/// analytics_enabled)` signature to `(args, ctx)` at every call boundary.
pub(crate) struct RunContext {
    pub show_stats: bool,
    pub json_output: bool,
    pub analytics_enabled: bool,
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
pub(crate) struct ParsedCommandConfig<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub env_overrides: &'a [(&'a str, &'a str)],
    pub install_hint: &'a str,
    pub use_stdin: bool,
    pub show_stats: bool,
    pub command_type: crate::analytics::CommandType,
    pub output_format: OutputFormat,
    pub analytics_enabled: bool,
    /// Family name used to build analytics labels (e.g. `"lint"`, `"infra"`, `"file"`).
    ///
    /// Analytics labels are recorded as `"skim {family} {program} {args}"`. Without
    /// this field the label was `"skim {program} {args}"`, which dropped the family
    /// name and made the analytics dashboard ambiguous when multiple families share
    /// tool names (e.g., `cargo` appears in both `build` and `pkg`). (PF-022)
    pub family: &'a str,
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
/// `config.use_stdin` — when `true`, reads stdin instead of spawning the command.
/// Callers should set this based on their own heuristics (e.g., only read
/// stdin when no user args are provided AND stdin is piped).
pub(crate) fn run_parsed_command_with_mode<T>(
    config: ParsedCommandConfig<'_>,
    parse: impl FnOnce(&CommandOutput, &[String]) -> ParseResult<T>,
) -> anyhow::Result<ExitCode>
where
    T: AsRef<str> + serde::Serialize,
{
    /// Maximum bytes we will read from stdin (64 MiB), consistent with the
    /// runner's `MAX_OUTPUT_BYTES` limit for command output pipes.
    const MAX_STDIN_BYTES: u64 = 64 * 1024 * 1024;

    let ParsedCommandConfig {
        program,
        args,
        env_overrides,
        install_hint,
        use_stdin,
        show_stats,
        command_type,
        output_format,
        analytics_enabled,
        family,
    } = config;

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
    let _ = result.emit_markers(&mut io::stderr().lock());

    // Capture exit code before moving stdout into analytics
    let code = output.exit_code.unwrap_or(1);

    // Render output and capture the compressed content string for stats/analytics.
    let compressed: String = match output_format {
        OutputFormat::Json => {
            let json_str = result.to_json_envelope()?;
            let mut handle = io::stdout().lock();
            writeln!(handle, "{json_str}")?;
            handle.flush()?;
            json_str
        }
        OutputFormat::Text => {
            let content = result.content();
            let mut handle = io::stdout().lock();
            write!(handle, "{content}")?;
            if !content.is_empty() && !content.ends_with('\n') {
                writeln!(handle)?;
            }
            handle.flush()?;
            content.to_string()
        }
    };

    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(&output.stdout, &compressed);
        crate::process::report_token_stats(orig, comp, "");
    }

    // Record analytics (fire-and-forget, non-blocking).
    crate::analytics::try_record_command(
        analytics_enabled,
        output.stdout,
        compressed,
        format_analytics_label(family, program, &args.join(" ")),
        command_type,
        output.duration,
        Some(result.tier_name()),
    );

    // Map exit code: preserve full 0-255 exit code granularity from the
    // underlying process. This maintains documented semantics (0=success,
    // 1=error, 2=parse error, 3=unsupported language) for downstream consumers.
    Ok(ExitCode::from(code.clamp(0, 255) as u8))
}

/// Dispatch a subcommand by name. Returns the process exit code.
///
/// Exit code semantics (GRANITE lesson — exit code corruption is P1):
/// - `--help` / `-h`: prints description to stdout, returns SUCCESS
/// - Otherwise: prints "not yet implemented" to stderr, returns FAILURE
pub(crate) fn dispatch(
    subcommand: &str,
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if !is_known_subcommand(subcommand) {
        anyhow::bail!(
            "Unknown subcommand: '{subcommand}'\n\
             Available subcommands: {}\n\
             Run 'skim --help' for usage information",
            KNOWN_SUBCOMMANDS.join(", ")
        );
    }

    match subcommand {
        "agents" => agents::run(args, analytics),
        "build" => build::run(args, analytics),
        "completions" => completions::run(args, analytics),
        "discover" => discover::run(args, analytics),
        "file" => file::run(args, analytics),
        "git" => git::run(args, analytics),
        "infra" => infra::run(args, analytics),
        "init" => init::run(args, analytics),
        "learn" => learn::run(args, analytics),
        "lint" => lint::run(args, analytics),
        "log" => log::run(args, analytics),
        "pkg" => pkg::run(args, analytics),
        "rewrite" => rewrite::run(args, analytics),
        "stats" => stats::run(args, analytics),
        "test" => test::run(args, analytics),
        // Unreachable: is_known_subcommand guard above rejects unknown names
        _ => unreachable!("unknown subcommand '{subcommand}' passed is_known_subcommand guard"),
    }
}

// ============================================================================
// Shared analytics label helper
// ============================================================================

/// Build a standardized analytics label: `"skim {family} {program} {rest}"`.
///
/// Centralises the label format so streaming and non-streaming code paths
/// cannot drift.  `rest` is the pre-joined argument string (may be empty).
pub(crate) fn format_analytics_label(family: &str, program: &str, rest: &str) -> String {
    if rest.is_empty() {
        format!("skim {family} {program}")
    } else {
        format!("skim {family} {program} {rest}")
    }
}

// ============================================================================
// Shared security helper
// ============================================================================

/// Sanitize user input for safe display in error messages.
///
/// Filters to printable ASCII characters to prevent terminal escape
/// injection attacks. Non-printable and non-ASCII bytes are replaced
/// with `?`, and the string is truncated to 64 characters.
pub(crate) fn sanitize_for_display(input: &str) -> String {
    input
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_graphic() || c == ' ' {
                c
            } else {
                '?'
            }
        })
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_flag_present() {
        let args: Vec<String> = vec!["--json".into(), "--cached".into()];
        let (filtered, is_json) = extract_json_flag(&args);
        assert!(is_json);
        assert_eq!(filtered, vec!["--cached"]);
    }

    #[test]
    fn test_extract_json_flag_absent() {
        let args: Vec<String> = vec!["--cached".into()];
        let (filtered, is_json) = extract_json_flag(&args);
        assert!(!is_json);
        assert_eq!(filtered, vec!["--cached"]);
    }

    #[test]
    fn test_sanitize_for_display_clean_input() {
        assert_eq!(sanitize_for_display("hello-world"), "hello-world");
    }

    #[test]
    fn test_sanitize_for_display_rejects_non_ascii() {
        let input = "tool\x1b[31mred\x1b[0m";
        let sanitized = sanitize_for_display(input);
        assert!(!sanitized.contains('\x1b'));
    }

    #[test]
    fn test_sanitize_for_display_truncates_at_64() {
        let long_input = "a".repeat(100);
        let sanitized = sanitize_for_display(&long_input);
        assert_eq!(sanitized.len(), 64);
    }
}
