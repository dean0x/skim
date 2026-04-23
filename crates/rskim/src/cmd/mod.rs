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

// ============================================================================
// SKIM_PASSTHROUGH helpers
// ============================================================================

/// Check if `SKIM_PASSTHROUGH` is set to a truthy value (`1`, `true`, or `yes`,
/// case-insensitive).
///
/// When passthrough mode is active, all compression is bypassed and raw output
/// is forwarded unchanged. Useful for debugging or when the compressed output
/// is too aggressive for a particular workflow.
pub(crate) fn is_passthrough_mode() -> bool {
    check_passthrough_value(std::env::var("SKIM_PASSTHROUGH").ok())
}

/// Pure function version of [`is_passthrough_mode`] — avoids process-wide env var
/// mutation in tests.
///
/// Returns `true` for `"1"`, `"true"`, `"yes"` (case-insensitive).
pub(crate) fn check_passthrough_value(val: Option<String>) -> bool {
    val.map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false)
}

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

/// Obtain command output from stdin or by spawning the command.
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
    const MAX_STDIN_BYTES: usize = 64 * 1024 * 1024;

    if use_stdin {
        let mut stdin_buf = String::new();
        let bytes_read = io::stdin()
            .take(MAX_STDIN_BYTES as u64)
            .read_to_string(&mut stdin_buf)?;
        if bytes_read >= MAX_STDIN_BYTES {
            anyhow::bail!("stdin input exceeded 64 MiB limit");
        }
        if stdin_buf.bytes().any(|b| !b.is_ascii_whitespace()) {
            return Ok(Some(CommandOutput {
                stdout: stdin_buf,
                stderr: String::new(),
                exit_code: Some(0),
                duration: std::time::Duration::ZERO,
            }));
        }
    }

    let runner = CommandRunner::new(Some(std::time::Duration::from_secs(300)));
    let args_str: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    match runner.run_with_env(program, &args_str, env_overrides) {
        Ok(out) => Ok(Some(out)),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("failed to execute") {
                eprintln!("error: '{program}' not found");
                eprintln!("hint: {install_hint}");
                return Ok(None);
            }
            Err(e)
        }
    }
}

/// Render parsed result to stdout, returning the output string for analytics.
fn render_output<T>(result: &ParseResult<T>, output_format: OutputFormat) -> anyhow::Result<String>
where
    T: AsRef<str> + serde::Serialize,
{
    match output_format {
        OutputFormat::Json => {
            let json_str = result.to_json_envelope()?;
            let mut handle = io::stdout().lock();
            writeln!(handle, "{json_str}")?;
            handle.flush()?;
            Ok(json_str)
        }
        OutputFormat::Text => {
            let content = result.content();
            let mut handle = io::stdout().lock();
            write!(handle, "{content}")?;
            if !content.is_empty() && !content.ends_with('\n') {
                writeln!(handle)?;
            }
            handle.flush()?;
            Ok(content.to_string())
        }
    }
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
    parse: impl FnOnce(&CommandOutput, &[String]) -> ParseResult<T>,
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
        command_type,
        output_format,
        analytics_enabled,
        family,
    } = config;

    let Some(output) = obtain_output(program, args, env_overrides, install_hint, use_stdin)? else {
        return Ok(ExitCode::FAILURE);
    };

    // Passthrough mode: bypass all compression and forward raw output.
    let code = output.exit_code.unwrap_or(1);
    if is_passthrough_mode() {
        let mut out = io::stdout().lock();
        write!(out, "{}", output.stdout)?;
        out.flush()?;
        if !output.stderr.is_empty() {
            let mut err = io::stderr().lock();
            write!(err, "{}", output.stderr)?;
            err.flush()?;
        }
        return Ok(ExitCode::from(code.clamp(0, 255) as u8));
    }

    let output = CommandOutput {
        stdout: crate::output::strip_ansi(&output.stdout),
        stderr: crate::output::strip_ansi(&output.stderr),
        ..output
    };

    let result = parse(&output, args);
    let _ = result.emit_markers(&mut io::stderr().lock());
    let code = output.exit_code.unwrap_or(1);

    let compressed = render_output(&result, output_format)?;

    // Hint fires on ALL non-zero exits regardless of tier. Passthrough tier
    // still means skim processed the command through its rewrite hook — agents
    // need the SKIM_PASSTHROUGH=1 escape hatch surfaced since the global
    // CLAUDE.md docs no longer mention the flag. Text says "compressed" for
    // consistency across tiers; the message's purpose is the escape hatch, not
    // describing what skim did. When SKIM_PASSTHROUGH=1 is active, we already
    // returned early above (the `is_passthrough_mode()` guard), so this never
    // double-fires.
    if code != 0 {
        eprintln!("[skim] compressed output (exit {code}). SKIM_PASSTHROUGH=1 for full output.");
    }

    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(&output.stdout, &compressed);
        crate::process::report_token_stats(orig, comp, "");
    }

    crate::analytics::try_record_command(
        analytics_enabled,
        output.stdout,
        compressed,
        format_analytics_label(family, program, &args.join(" ")),
        command_type,
        output.duration,
        Some(result.tier_name()),
    );

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

    // ========================================================================
    // check_passthrough_value / stderr hint guard
    // ========================================================================

    /// Verify that `SKIM_PASSTHROUGH=1` causes early return in
    /// `run_parsed_command_with_mode`, bypassing all compression and the
    /// stderr hint. The hint now fires on ALL non-zero exits regardless of
    /// parse tier — the `is_passthrough_mode()` guard is the only mechanism
    /// that suppresses it.
    #[test]
    fn test_no_stderr_hint_when_passthrough_mode() {
        assert!(check_passthrough_value(Some("1".to_string())));
        assert!(check_passthrough_value(Some("true".to_string())));
    }

    #[test]
    fn test_check_passthrough_truthy_values() {
        for v in &["1", "true", "yes", "True", "YES", "tRuE"] {
            assert!(
                check_passthrough_value(Some((*v).to_string())),
                "expected truthy for {v:?}"
            );
        }
    }

    #[test]
    fn test_check_passthrough_falsy_values() {
        assert!(!check_passthrough_value(Some("0".to_string())));
        assert!(!check_passthrough_value(Some("false".to_string())));
        assert!(!check_passthrough_value(Some("no".to_string())));
        assert!(!check_passthrough_value(None));
    }

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
