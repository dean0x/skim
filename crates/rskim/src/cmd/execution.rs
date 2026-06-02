//! Command execution infrastructure for skim CLI.
//!
//! Provides the types and functions that handle running external commands,
//! parsing their output through the three-tier degradation pipeline, and
//! recording analytics.

use std::borrow::Cow;
use std::io::{self, Write};
use std::process::ExitCode;

use crate::output::ParseResult;
use crate::runner::{CommandOutput, CommandRunner};

use super::{DEFAULT_CMD_TIMEOUT, is_passthrough_mode, read_stdin_bounded, should_read_stdin};
use super::{scrub_db_args, scrub_infra_args};

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
/// Bundles the fields every family dispatcher receives identically, reducing
/// the positional parameter list to `(args, ctx)` at every call boundary.
///
/// ## Relationship to `RecordingContext`
///
/// Each family dispatcher constructs a [`crate::analytics::RecordingContext`]
/// from `analytics_enabled`, `session_id`, and the handler-local `command_type`,
/// then threads it directly through to [`ParsedCommandConfig::rec`].  The two
/// structs are intentionally separate: `RunContext` owns its strings while
/// `RecordingContext` borrows them (`Copy`, zero-allocation threading through
/// call chains).
pub(crate) struct RunContext {
    pub show_stats: bool,
    pub json_output: bool,
    pub analytics_enabled: bool,
    /// Optional session ID from `AnalyticsConfig::session_id`.
    /// Used by family dispatchers when constructing `RecordingContext`.
    pub session_id: Option<String>,
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
///
/// ## Analytics threading
///
/// `rec` carries the full [`crate::analytics::RecordingContext`] constructed
/// once by each family dispatcher.  `run_parsed_command_with_mode` calls
/// `rec.with_tier(result.tier_name())` at the recording site — no
/// decompose-then-reconstruct at the call site.
pub(crate) struct ParsedCommandConfig<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub env_overrides: &'a [(&'a str, &'a str)],
    pub install_hint: &'a str,
    pub use_stdin: bool,
    pub show_stats: bool,
    pub output_format: OutputFormat,
    /// Family name used to build analytics labels (e.g. `"lint"`, `"infra"`, `"file"`).
    ///
    /// Analytics labels are recorded as `"skim {family} {program} {args}"`. Without
    /// this field the label was `"skim {program} {args}"`, which dropped the family
    /// name and made the analytics dashboard ambiguous when multiple families share
    /// tool names (e.g., `cargo` appears in both `build` and `pkg`). (PF-022)
    pub family: &'a str,
    /// When `true`, skip ANSI escape stripping on the raw command output.
    ///
    /// `strip_ansi_escapes` treats ASCII control codes — including `\t` (0x09) —
    /// as part of escape sequences and drops them. DB tools emit tab-separated
    /// (TSV) output; stripping would remove tab separators and cause all DB
    /// parsers to fall through to Passthrough. DB tools set `true`;
    /// all other families set `false`.
    pub skip_ansi_strip: bool,
    /// Recording context constructed once by the family dispatcher.
    /// `run_parsed_command_with_mode` annotates `parse_tier` via
    /// `rec.with_tier(result.tier_name())` before passing to `try_record_command`.
    pub rec: crate::analytics::RecordingContext<'a>,
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

/// Obtain command output from stdin or by spawning the command.
///
/// When `use_stdin` is `true`, reads stdin first. If stdin contains only
/// whitespace (e.g., a CI pipe that opens but writes nothing), the function
/// falls through silently to the spawn path so the real command runs with
/// its actual exit code instead of producing empty output.
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
    if use_stdin {
        let stdin_buf = read_stdin_bounded()?;
        if stdin_buf.bytes().any(|b| !b.is_ascii_whitespace()) {
            return Ok(Some(CommandOutput {
                stdout: stdin_buf,
                stderr: String::new(),
                exit_code: Some(0),
                duration: std::time::Duration::ZERO,
            }));
        }
    }

    let runner = CommandRunner::new(Some(DEFAULT_CMD_TIMEOUT));
    let args_str: Vec<&str> = args.iter().map(String::as_str).collect();
    match runner.run_with_env(program, &args_str, env_overrides) {
        Ok(out) => Ok(Some(out)),
        Err(e) => {
            if crate::runner::is_spawn_error(&e) {
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

/// Write raw command output to stdout/stderr and return the process exit code.
///
/// Used by the passthrough fast-path in [`run_parsed_command_with_mode`] when
/// `SKIM_PASSTHROUGH=1` is set. Forwards stdout/stderr verbatim without any
/// compression or parsing.
fn passthrough_raw(output: &CommandOutput) -> anyhow::Result<ExitCode> {
    let code = output.exit_code.unwrap_or(1);
    let mut out = io::stdout().lock();
    write!(out, "{}", output.stdout)?;
    out.flush()?;
    if !output.stderr.is_empty() {
        let mut err = io::stderr().lock();
        write!(err, "{}", output.stderr)?;
        err.flush()?;
    }
    Ok(ExitCode::from(code.clamp(0, 255) as u8))
}

/// Parameters for recording token savings and emitting the analytics event.
///
/// Bundles the fields that [`record_and_report`] needs, replacing the
/// eight-positional-parameter signature and removing the
/// `#[allow(clippy::too_many_arguments)]` suppression.  Follows the same
/// parameter-bundling pattern as [`ParsedCommandConfig`] and [`ToolRunConfig`].
struct RecordReport<'a> {
    show_stats: bool,
    code: i32,
    original_stdout: String,
    compressed: String,
    rec: crate::analytics::RecordingContext<'a>,
    tier_name: &'static str,
    label: String,
    duration: std::time::Duration,
}

/// Record token savings and emit the analytics event for a completed command.
///
/// Separated from [`run_parsed_command_with_mode`] so the core parsing/rendering
/// pipeline is readable as a linear sequence of steps.
fn record_and_report(report: RecordReport<'_>) {
    let RecordReport {
        show_stats,
        code,
        original_stdout,
        compressed,
        rec,
        tier_name,
        label,
        duration,
    } = report;

    // Hint fires on ALL non-zero exits regardless of tier. Passthrough tier
    // still means skim processed the command through its rewrite hook — agents
    // need the SKIM_PASSTHROUGH=1 escape hatch surfaced since the global
    // CLAUDE.md docs no longer mention the flag. Text says "compressed" for
    // consistency across tiers; the message's purpose is the escape hatch, not
    // describing what skim did. When SKIM_PASSTHROUGH=1 is active, we already
    // returned early in run_parsed_command_with_mode (the is_passthrough_mode()
    // guard), so this never double-fires.
    if code != 0 {
        eprintln!("[skim] compressed output (exit {code}). SKIM_PASSTHROUGH=1 for full output.");
    }

    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(&original_stdout, &compressed);
        crate::process::report_token_stats(orig, comp, "");
    }

    crate::analytics::try_record_command(
        rec.with_tier(tier_name),
        original_stdout,
        compressed,
        label,
        duration,
    );
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
    parse: impl FnOnce(&CommandOutput) -> ParseResult<T>,
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
        output_format,
        family,
        skip_ansi_strip,
        rec,
    } = config;

    let Some(output) = obtain_output(program, args, env_overrides, install_hint, use_stdin)? else {
        return Ok(ExitCode::FAILURE);
    };

    // Passthrough mode: bypass all compression and forward raw output.
    if is_passthrough_mode() {
        return passthrough_raw(&output);
    }

    // Some tools must NOT have ANSI escape sequences stripped: strip_ansi_escapes
    // treats ASCII control codes — including \t (0x09) — as part of escape
    // sequences and drops them. DB tools emit tab-separated (TSV) output; stripping
    // would remove tab separators and cause all DB parsers to fall through to
    // Passthrough. Callers signal this via `config.skip_ansi_strip`.
    let output = if skip_ansi_strip {
        output
    } else {
        CommandOutput {
            stdout: crate::output::strip_ansi(&output.stdout),
            stderr: crate::output::strip_ansi(&output.stderr),
            ..output
        }
    };

    let result = parse(&output);
    let _ = result.emit_markers(&mut io::stderr().lock());
    let code = output.exit_code.unwrap_or(1);
    let label = format_analytics_label(family, program, &args.join(" "));
    let tier_name = result.tier_name();

    let compressed = render_output(&result, output_format)?;

    record_and_report(RecordReport {
        show_stats,
        code,
        original_stdout: output.stdout,
        compressed,
        rec,
        tier_name,
        label,
        duration: output.duration,
    });

    Ok(ExitCode::from(code.clamp(0, 255) as u8))
}

/// Build a standardized analytics label: `"skim {family} {program} {rest}"`.
///
/// Centralises the label format so streaming and non-streaming code paths
/// cannot drift.  `rest` is the pre-joined argument string (may be empty).
///
/// Sensitive flags are redacted before the label is stored to prevent
/// credentials persisting in the analytics SQLite database:
///
/// - `"db"` family: passwords, usernames, hostnames (psql/mysql flags).
/// - `"infra"` family: Authorization headers, `--token`, `--password`,
///   `--secret`, `--api-key`, and similar flags used by curl, aws, gh, etc.
pub(crate) fn format_analytics_label(family: &str, program: &str, rest: &str) -> String {
    if rest.is_empty() {
        format!("skim {family} {program}")
    } else if family == "db" {
        let scrubbed = scrub_db_args(rest);
        format!("skim {family} {program} {scrubbed}")
    } else if family == "infra" {
        let scrubbed = scrub_infra_args(rest);
        format!("skim {family} {program} {scrubbed}")
    } else {
        format!("skim {family} {program} {rest}")
    }
}

/// Cross-cutting configuration for a single-tool execution.
///
/// Unifies `DbToolConfig`, `InfraToolConfig`, `FileToolConfig`, and
/// `LinterConfig` into one struct.  The two new fields (`family`,
/// `skip_ansi_strip`) are the only differences between the four original
/// family-specific configs; all other fields are structurally identical.
///
/// ## Relationship to `ParsedCommandConfig`
///
/// `ToolRunConfig` is the caller-facing API; `ParsedCommandConfig` is the
/// internal config consumed by `run_parsed_command_with_mode`.  `run_tool`
/// bridges the two, translating caller fields plus `family`/`skip_ansi_strip`
/// into the full `ParsedCommandConfig`.
///
/// The split is intentional: `ToolRunConfig` carries only static, caller-supplied
/// fields.  `ParsedCommandConfig` additionally requires runtime-computed fields
/// (`use_stdin`, `show_stats`, `output_format`, `rec`) derived from `RunContext`
/// and the actual argument list — values unavailable at `ToolRunConfig`
/// construction time.  `Into<ParsedCommandConfig>` would therefore be unsound
/// without also accepting `&[String]` and `&RunContext`, which defeats the
/// purpose of a simple `Into` bridge.  `run_tool` IS the bridge.
pub(crate) struct ToolRunConfig<'a> {
    /// Binary name of the tool (e.g., "psql", "eslint").
    pub program: &'a str,
    /// Environment variable overrides for the child process.
    pub env_overrides: &'a [(&'a str, &'a str)],
    /// Hint printed when the tool binary is not found.
    pub install_hint: &'a str,
    /// Family name for analytics labels (e.g. `"db"`, `"infra"`, `"lint"`).
    pub family: &'a str,
    /// When `true`, skip ANSI escape stripping on the raw command output.
    ///
    /// Set `true` for DB tools (TSV output) and DNS tools (tab field separators).
    /// See `ParsedCommandConfig::skip_ansi_strip` for full rationale.
    pub skip_ansi_strip: bool,
    /// Analytics command type for recording.
    pub command_type: crate::analytics::CommandType,
}

/// Execute a tool, parse its output, and emit the result.
///
/// Single generic implementation that replaces `run_db_tool`, `run_infra_tool`,
/// `run_file_tool`, and `run_linter`.  Each family-specific runner had an
/// identical body; the only differences were `family`, `skip_ansi_strip`, and
/// `command_type`, which are now carried in `ToolRunConfig`.
///
/// ## Constraints
///
/// `build::run_parsed_command` is intentionally **not** replaced: it has a
/// different call shape (no `ctx: &RunContext`, different analytics path).
/// `run_pkg_subcommand` is also excluded: it has a different signature.
pub(crate) fn run_tool<T>(
    config: ToolRunConfig<'_>,
    args: &[String],
    ctx: &RunContext,
    prepare_args: impl FnOnce(&mut Vec<String>),
    parse_fn: impl FnOnce(&CommandOutput) -> ParseResult<T>,
) -> anyhow::Result<std::process::ExitCode>
where
    T: AsRef<str> + serde::Serialize,
{
    let mut cmd_args = args.to_vec();
    prepare_args(&mut cmd_args);
    let use_stdin = should_read_stdin(args);
    run_parsed_command_with_mode(
        ParsedCommandConfig {
            program: config.program,
            args: &cmd_args,
            env_overrides: config.env_overrides,
            install_hint: config.install_hint,
            use_stdin,
            show_stats: ctx.show_stats,
            output_format: ctx.output_format(),
            family: config.family,
            skip_ansi_strip: config.skip_ansi_strip,
            rec: crate::analytics::RecordingContext {
                enabled: ctx.analytics_enabled,
                command_type: config.command_type,
                parse_tier: None,
                session_id: ctx.session_id.as_deref(),
            },
        },
        parse_fn,
    )
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // format_analytics_label tests
    // ========================================================================

    #[test]
    fn test_format_analytics_label_db_scrubs_credentials() {
        // Simulate: skim db psql -h myhost -U admin -c SELECT 1
        let label = format_analytics_label("db", "psql", "-h myhost -U admin -c SELECT 1");
        assert!(
            !label.contains("myhost"),
            "hostname must be redacted from db analytics label: {label}"
        );
        assert!(
            !label.contains("admin"),
            "username must be redacted from db analytics label: {label}"
        );
        assert!(
            label.contains("[REDACTED]"),
            "redaction marker must be present: {label}"
        );
    }

    #[test]
    fn test_format_analytics_label_non_sensitive_infra_not_scrubbed() {
        // Non-sensitive infra args (no auth flags) are forwarded verbatim.
        let label = format_analytics_label("infra", "kubectl", "get pods -n myns");
        assert!(
            label.contains("myns"),
            "non-sensitive infra args must not be scrubbed: {label}"
        );
    }

    #[test]
    fn test_format_analytics_label_infra_scrubs_token() {
        // Sensitive --token flag must be redacted for the infra family.
        let label = format_analytics_label("infra", "gh", "--token ghp_secrettoken repo list");
        assert!(
            !label.contains("ghp_secrettoken"),
            "token value must be redacted from infra analytics label: {label}"
        );
        assert!(
            label.contains("[REDACTED]"),
            "redaction marker must be present: {label}"
        );
        assert!(
            label.contains("repo list"),
            "non-sensitive args must be preserved: {label}"
        );
    }

    #[test]
    fn test_format_analytics_label_db_empty_rest() {
        let label = format_analytics_label("db", "psql", "");
        assert_eq!(label, "skim db psql");
    }

    // ========================================================================
    // combine_output tests
    // ========================================================================

    fn make_cmd_output(stdout: &str, stderr: &str) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        }
    }

    #[test]
    fn test_combine_output_empty_stderr_borrows() {
        // Fast path: empty stderr must return Cow::Borrowed (zero-copy).
        let output = make_cmd_output("hello world", "");
        let combined = combine_output(&output);
        assert!(
            matches!(combined, Cow::Borrowed(_)),
            "empty stderr must produce Cow::Borrowed (zero-copy): {combined:?}"
        );
        assert_eq!(combined.as_ref(), "hello world");
    }

    #[test]
    fn test_combine_output_non_empty_stderr_concatenates() {
        // Slow path: non-empty stderr triggers owned concatenation.
        let output = make_cmd_output("stdout line", "stderr line");
        let combined = combine_output(&output);
        assert!(
            matches!(combined, Cow::Owned(_)),
            "non-empty stderr must produce Cow::Owned (concatenation): {combined:?}"
        );
        assert_eq!(combined.as_ref(), "stdout line\nstderr line");
    }

    #[test]
    fn test_combine_output_both_empty_borrows() {
        // Both empty: stdout is empty string; stderr is empty so fast path applies.
        let output = make_cmd_output("", "");
        let combined = combine_output(&output);
        assert!(
            matches!(combined, Cow::Borrowed(_)),
            "both empty must produce Cow::Borrowed: {combined:?}"
        );
        assert_eq!(combined.as_ref(), "");
    }
}
