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
pub(crate) mod ux;

use std::borrow::Cow;
use std::io::{self, Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use crate::output::ParseResult;
use crate::runner::{CommandOutput, CommandRunner};

// ============================================================================
// Stdin reading
// ============================================================================

/// Default timeout for command execution (5 minutes).
///
/// Applied to all [`CommandRunner`] sites that don't have an explicit longer
/// timeout (build commands use 600 s because compile times can be substantial).
pub(crate) const DEFAULT_CMD_TIMEOUT: Duration = Duration::from_secs(300);

/// Determine whether to read piped stdin instead of spawning the command.
///
/// Returns `true` when stdin is not a terminal AND `args` is empty. The
/// `args.is_empty()` guard is critical: without it, subprocess contexts
/// (Claude Code, CI) where stdin is never a terminal would always read from
/// empty stdin instead of spawning the runner.
pub(crate) fn should_read_stdin(args: &[String]) -> bool {
    use std::io::IsTerminal;
    !std::io::stdin().is_terminal() && args.is_empty()
}

/// Maximum bytes read from stdin.
///
/// Re-exported from [`crate::runner::MAX_OUTPUT_BYTES`] so the stdin cap and
/// the pipe-capture cap stay in sync automatically — no duplicated literal.
pub(crate) use crate::runner::MAX_OUTPUT_BYTES as MAX_STDIN_BYTES;

/// Core bounded read loop, injectable for testing.
///
/// Reads from `reader` in 8 KiB chunks until EOF or `max_bytes` is exceeded.
/// Valid UTF-8 is zero-copy (Vec moved into String); invalid UTF-8 falls back
/// to lossy U+FFFD replacement — matching `read_pipe` in `runner.rs`.
///
/// Returns an error if the total bytes read would exceed `max_bytes`.
pub(crate) fn read_bounded(mut reader: impl Read, max_bytes: usize) -> anyhow::Result<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        if buf.len() + n > max_bytes {
            anyhow::bail!("input exceeded {} byte limit", max_bytes);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Ok(String::from_utf8(buf)
        .unwrap_or_else(|e| String::from_utf8_lossy(&e.into_bytes()).into_owned()))
}

/// Read all of stdin into a `String`, capped at [`MAX_STDIN_BYTES`].
///
/// Thin wrapper around [`read_bounded`] that supplies `stdin().lock()` as the
/// reader. Production code calls this; tests call `read_bounded` directly with
/// an in-memory cursor.
pub(crate) fn read_stdin_bounded() -> anyhow::Result<String> {
    read_bounded(io::stdin().lock(), MAX_STDIN_BYTES)
}

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

/// Core truthy-value check for a `SKIM_PASSTHROUGH` value already extracted as
/// a `&str`.
///
/// Returns `true` for `"1"`, `"true"`, `"yes"` (case-insensitive).
///
/// This is the single authoritative definition of "truthy". All other helpers
/// delegate here so the truthy set is never duplicated.
pub(crate) fn check_passthrough_str(val: &str) -> bool {
    matches!(val.to_lowercase().as_str(), "1" | "true" | "yes")
}

/// Pure function version of [`is_passthrough_mode`] — avoids process-wide env var
/// mutation in tests.
///
/// Returns `true` for `"1"`, `"true"`, `"yes"` (case-insensitive).
/// Delegates to [`check_passthrough_str`] so the truthy definition stays in one place.
pub(crate) fn check_passthrough_value(val: Option<String>) -> bool {
    val.map(|v| check_passthrough_str(&v)).unwrap_or(false)
}

/// Known subcommands that the pre-parse router will recognize.
///
/// IMPORTANT: Only register subcommands we will actually implement.
/// Keep this list exact — no broad patterns. See GRANITE lesson #336.
///
/// v2.8.0: Flat dispatch — tool names promoted to top-level subcommands.
/// Category prefixes (build, lint, pkg, infra, file, test) removed.
/// Deprecated category names retained here for backward-compat; dispatch()
/// prints a deprecation warning and forwards to the appropriate handler.
pub(crate) const KNOWN_SUBCOMMANDS: &[&str] = &[
    // Meta/utility (unchanged)
    "agents",
    "completions",
    "discover",
    "git",
    "init",
    "learn",
    "log",
    "rewrite",
    "stats",
    // Multi-category dispatchers
    "cargo",
    "go",
    // Test runners
    "jest",
    "pytest",
    "vitest",
    // Build tools
    "tsc",
    // Linters (11)
    "biome",
    "black",
    "dprint",
    "eslint",
    "gofmt",
    "golangci",
    "mypy",
    "oxlint",
    "prettier",
    "ruff",
    "rustfmt",
    // Package managers
    "npm",
    "pnpm",
    "pip",
    // Infrastructure
    "aws",
    "curl",
    "gh",
    "wget",
    // File operations
    "find",
    "grep",
    "ls",
    "rg",
    "tree",
    // Deprecated v2.7 category subcommands — kept for backward compatibility.
    // dispatch() emits a deprecation warning and forwards to the handler.
    "test",
    "build",
    "lint",
    "pkg",
    "file",
    "infra",
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
    /// Recording context constructed once by the family dispatcher.
    /// `run_parsed_command_with_mode` annotates `parse_tier` via
    /// `rec.with_tier(result.tier_name())` before passing to `try_record_command`.
    pub rec: crate::analytics::RecordingContext<'a>,
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
        output_format,
        family,
        rec,
    } = config;

    let Some(output) = obtain_output(program, args, env_overrides, install_hint, use_stdin)? else {
        return Ok(ExitCode::FAILURE);
    };

    // Passthrough mode: bypass all compression and forward raw output.
    if is_passthrough_mode() {
        let code = output.exit_code.unwrap_or(1);
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
        rec.with_tier(result.tier_name()),
        output.stdout,
        compressed,
        format_analytics_label(family, program, &args.join(" ")),
        output.duration,
    );

    Ok(ExitCode::from(code.clamp(0, 255) as u8))
}

/// Prepend a tool name to an arg slice.
fn prepend(tool: &str, args: &[String]) -> Vec<String> {
    let mut v = vec![tool.to_string()];
    v.extend_from_slice(args);
    v
}

/// Shared scaffolding for multi-category dispatchers (`cargo`, `go`, …).
///
/// Handles flag interleaving: `skim cargo --show-stats test` works because
/// we skip leading flags to find the first positional (the subcommand token),
/// then the caller decides which args to forward.
///
/// Returns `Ok(Some((subcmd_str, subcmd_idx)))` when a subcommand is found, or
/// `Ok(None)` after printing the missing-subcommand error (caller should return
/// `ExitCode::FAILURE`).  The `tool` parameter is used only in the error message.
fn extract_subcmd<'a>(
    tool: &str,
    args: &'a [String],
    usage: &str,
    supported: &str,
) -> anyhow::Result<Option<(&'a str, usize)>> {
    match args.iter().position(|a| !a.starts_with('-')) {
        Some(idx) => Ok(Some((args[idx].as_str(), idx))),
        None => {
            eprintln!(
                "skim {tool}: missing subcommand\n\n\
                 Usage: {usage}\n\n\
                 Supported subcommands: {supported}"
            );
            Ok(None)
        }
    }
}

/// Build a `Vec<String>` with `tool` prepended and the element at `skip_idx`
/// removed, pre-allocating the exact capacity needed.
fn prepend_without(tool: &str, args: &[String], skip_idx: usize) -> Vec<String> {
    debug_assert!(skip_idx < args.len(), "skip_idx {skip_idx} out of bounds for args len {}", args.len());
    let mut v = Vec::with_capacity(args.len()); // remove one, prepend one → same len
    v.push(tool.to_string());
    v.extend(
        args.iter()
            .enumerate()
            .filter_map(|(i, s)| (i != skip_idx).then(|| s.clone())),
    );
    v
}

/// Route `skim cargo <subcmd> [args...]` to the correct category handler.
fn dispatch_cargo(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_cargo_help();
        return Ok(ExitCode::SUCCESS);
    }

    let Some((subcmd, idx)) = extract_subcmd(
        "cargo",
        args,
        "skim cargo <test|build|clippy|audit|nextest> [args...]",
        "test, nextest, build, clippy, audit",
    )?
    else {
        return Ok(ExitCode::FAILURE);
    };

    match subcmd {
        "test" | "t" => test::run(&prepend_without("cargo", args, idx), analytics),
        // nextest: keep the "nextest" token — the test handler uses it to select
        // the nextest parse path instead of the plain cargo-test path.
        "nextest" => test::run(&prepend("cargo", args), analytics),
        "build" | "b" => build::run(&prepend_without("cargo", args, idx), analytics),
        "clippy" => build::run(&prepend_without("clippy", args, idx), analytics),
        // audit: keep "audit" in args — pkg::run uses it to select the audit parser.
        "audit" => pkg::run(&prepend("cargo", args), analytics),
        unknown => {
            let safe = sanitize_for_display(unknown);
            eprintln!(
                "skim cargo: unsupported subcommand '{safe}'\n\n\
                 Usage: skim cargo <test|build|clippy|audit|nextest> [args...]\n\n\
                 Supported subcommands: test, nextest, build, clippy, audit"
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Route `skim go <subcmd> [args...]` to the correct category handler.
fn dispatch_go(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_go_help();
        return Ok(ExitCode::SUCCESS);
    }

    let Some((subcmd, idx)) = extract_subcmd(
        "go",
        args,
        "skim go <test> [args...]",
        "test",
    )?
    else {
        return Ok(ExitCode::FAILURE);
    };

    match subcmd {
        "test" => test::run(&prepend_without("go", args, idx), analytics),
        unknown => {
            let safe = sanitize_for_display(unknown);
            eprintln!(
                "skim go: unsupported subcommand '{safe}'\n\n\
                 Usage: skim go <test> [args...]\n\n\
                 Supported subcommands: test"
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

fn print_cargo_help() {
    print!(
        "skim cargo\n\
         \n\
           Cargo subcommand compression\n\
         \n\
         Usage: skim cargo <SUBCOMMAND> [args...]\n\
         \n\
         Subcommands:\n\
           test (t)   Run and compress cargo test output\n\
           nextest    Run and compress cargo nextest output\n\
           build (b)  Run and compress cargo build output\n\
           clippy     Run and compress cargo clippy output\n\
           audit      Run and compress cargo audit output\n\
         \n\
         Examples:\n\
           skim cargo test\n\
           skim cargo t          (alias for test)\n\
           skim cargo build --release\n\
           skim cargo b --release  (alias for build)\n\
           skim cargo clippy -- -D warnings\n\
           skim cargo audit\n"
    );
}

fn print_go_help() {
    print!(
        "skim go\n\
         \n\
           Go subcommand compression\n\
         \n\
         Usage: skim go <SUBCOMMAND> [args...]\n\
         \n\
         Subcommands:\n\
           test       Run and compress go test output\n\
         \n\
         Examples:\n\
           skim go test ./...\n\
           skim go test -v ./pkg/...\n"
    );
}

/// Emit a v2.8.0 deprecation warning for an old category subcommand and
/// forward to its handler.
///
/// Centralises the `eprintln!` + forward pattern so each deprecated match arm
/// is a single call instead of a repeated 4-line block.
fn dispatch_deprecated(
    category: &str,
    hint: &str,
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
    handler: fn(&[String], &crate::analytics::AnalyticsConfig) -> anyhow::Result<ExitCode>,
) -> anyhow::Result<ExitCode> {
    eprintln!(
        "skim: '{category}' category subcommand is deprecated since v2.8.0.\n\
         {hint}"
    );
    handler(args, analytics)
}

/// Dispatch a subcommand by name. Returns the process exit code.
///
/// v2.8.0: Flat dispatch — tool names are top-level subcommands.
/// `cargo` and `go` use multi-category dispatchers; other tools route
/// directly to their category handler with the tool name prepended.
pub(crate) fn dispatch(
    subcommand: &str,
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    match subcommand {
        // Unchanged meta/utility
        "agents" => agents::run(args, analytics),
        "completions" => completions::run(args, analytics),
        "discover" => discover::run(args, analytics),
        "git" => git::run(args, analytics),
        "init" => init::run(args, analytics),
        "learn" => learn::run(args, analytics),
        "log" => log::run(args, analytics),
        "rewrite" => rewrite::run(args, analytics),
        "stats" => stats::run(args, analytics),

        // Multi-category dispatchers
        "cargo" => dispatch_cargo(args, analytics),
        "go" => dispatch_go(args, analytics),

        // Direct-to-category routing (prepend tool name for category dispatcher)
        "jest" | "pytest" | "vitest" => test::run(&prepend(subcommand, args), analytics),
        "tsc" => build::run(&prepend(subcommand, args), analytics),
        "biome" | "black" | "dprint" | "eslint" | "gofmt" | "golangci" | "mypy" | "oxlint"
        | "prettier" | "ruff" | "rustfmt" => lint::run(&prepend(subcommand, args), analytics),
        "npm" | "pnpm" | "pip" => pkg::run(&prepend(subcommand, args), analytics),
        "aws" | "curl" | "gh" | "wget" => infra::run(&prepend(subcommand, args), analytics),
        "find" | "grep" | "ls" | "rg" | "tree" => {
            file::run(&prepend(subcommand, args), analytics)
        }

        // Deprecated v2.7 category subcommands — forward with deprecation warning.
        // args are passed as-is; each category handler accepts the old format.
        "test" => dispatch_deprecated(
            "test",
            "Use the tool name directly, e.g.: skim jest, skim pytest, skim vitest,\n\
             or for cargo: skim cargo test",
            args, analytics, test::run,
        ),
        "build" => dispatch_deprecated(
            "build",
            "Use the tool name directly, e.g.: skim tsc\n\
             or for cargo: skim cargo build",
            args, analytics, build::run,
        ),
        "lint" => dispatch_deprecated(
            "lint",
            "Use the tool name directly, e.g.: skim eslint, skim ruff, skim mypy",
            args, analytics, lint::run,
        ),
        "pkg" => dispatch_deprecated(
            "pkg",
            "Use the tool name directly, e.g.: skim npm, skim pnpm, skim pip",
            args, analytics, pkg::run,
        ),
        "file" => dispatch_deprecated(
            "file",
            "Use the tool name directly, e.g.: skim find, skim grep, skim ls, skim rg, skim tree",
            args, analytics, file::run,
        ),
        "infra" => dispatch_deprecated(
            "infra",
            "Use the tool name directly, e.g.: skim gh, skim aws, skim curl, skim wget",
            args, analytics, infra::run,
        ),

        _ => {
            let safe = sanitize_for_display(subcommand);
            anyhow::bail!(
                "Unknown subcommand: '{safe}'\n\
                 Available subcommands: {}\n\
                 Run 'skim --help' for usage information",
                KNOWN_SUBCOMMANDS.join(", ")
            );
        }
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
    fn test_check_passthrough_str_truthy_values() {
        for v in &["1", "true", "yes", "True", "YES", "tRuE"] {
            assert!(check_passthrough_str(v), "expected truthy for {v:?}");
        }
    }

    #[test]
    fn test_check_passthrough_str_falsy_values() {
        assert!(!check_passthrough_str("0"));
        assert!(!check_passthrough_str("false"));
        assert!(!check_passthrough_str("no"));
        assert!(!check_passthrough_str(""));
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

    // ========================================================================
    // should_read_stdin tests
    // ========================================================================

    #[test]
    fn test_should_read_stdin_false_when_args_present() {
        let args = vec!["--run".to_string(), "math".to_string()];
        assert!(
            !should_read_stdin(&args),
            "non-empty args must prevent stdin mode"
        );
    }

    #[test]
    fn test_should_read_stdin_args_gate_short_circuits() {
        for args in [
            vec!["run".to_string()],
            vec!["--reporter=verbose".to_string()],
            vec!["--reporter=verbose".to_string(), "math".to_string()],
            vec!["src/utils.test.ts".to_string()],
        ] {
            assert!(
                !should_read_stdin(&args),
                "should_read_stdin must return false for args: {args:?}"
            );
        }
    }

    #[test]
    fn test_should_read_stdin_empty_args_defers_to_terminal() {
        use std::io::IsTerminal;
        let result = should_read_stdin(&[]);
        // In `cargo test`, stdin is typically a terminal → false.
        // The point is that empty args don't unconditionally force stdin mode;
        // it still checks is_terminal().
        assert_eq!(result, !std::io::stdin().is_terminal());
    }

    // ========================================================================
    // read_bounded tests
    // ========================================================================

    #[test]
    fn test_read_bounded_under_limit() {
        let data = b"hello world";
        let result = read_bounded(data.as_ref(), 1024).unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_read_bounded_exactly_at_limit_is_ok() {
        // buf.len() + n > max_bytes triggers the error, so exactly max_bytes
        // bytes must succeed.
        let data = vec![b'A'; 100];
        let result = read_bounded(data.as_slice(), 100).unwrap();
        assert_eq!(result.len(), 100);
    }

    #[test]
    fn test_read_bounded_over_limit_returns_error() {
        let data = vec![b'X'; 200];
        let err = read_bounded(data.as_slice(), 100).unwrap_err();
        assert!(
            err.to_string().contains("exceeded"),
            "expected 'exceeded' in error message, got: {err}"
        );
    }

    #[test]
    fn test_read_bounded_invalid_utf8_falls_back_to_lossy() {
        // 0xFF is not valid UTF-8; the function must fall back to lossy
        // conversion and include the U+FFFD replacement character.
        let data: &[u8] = &[0xFF, 0xFE, b'o', b'k'];
        let result = read_bounded(data, 1024).unwrap();
        assert!(
            result.contains('\u{FFFD}'),
            "expected U+FFFD replacement, got: {result:?}"
        );
    }

    #[test]
    fn test_read_bounded_empty_input() {
        let result = read_bounded(b"".as_ref(), 1024).unwrap();
        assert!(result.is_empty());
    }

    // ========================================================================
    // extract_subcmd tests
    // ========================================================================

    /// Happy path: first non-flag arg is the subcommand.
    #[test]
    fn test_extract_subcmd_finds_first_positional() {
        let args: Vec<String> = vec!["test".into(), "--release".into()];
        let result = extract_subcmd("cargo", &args, "usage", "test").unwrap();
        assert_eq!(result, Some(("test", 0)));
    }

    /// Flags before the subcommand are skipped; the positional is found at the
    /// correct index so `prepend_without` will remove the right element.
    #[test]
    fn test_extract_subcmd_skips_leading_flags() {
        let args: Vec<String> = vec!["--show-stats".into(), "build".into(), "--release".into()];
        let result = extract_subcmd("cargo", &args, "usage", "build").unwrap();
        assert_eq!(result, Some(("build", 1)));
    }

    /// When every arg starts with `-` there is no subcommand; the function
    /// prints the error message and returns `None` (caller returns FAILURE).
    #[test]
    fn test_extract_subcmd_returns_none_when_all_flags() {
        let args: Vec<String> = vec!["--show-stats".into(), "--json".into()];
        let result = extract_subcmd("cargo", &args, "usage", "test").unwrap();
        assert!(result.is_none());
    }

    /// Empty arg slice → no subcommand found, returns `None`.
    #[test]
    fn test_extract_subcmd_empty_args() {
        let args: Vec<String> = vec![];
        let result = extract_subcmd("cargo", &args, "usage", "test").unwrap();
        assert!(result.is_none());
    }

    // ========================================================================
    // prepend_without tests
    // ========================================================================

    /// Removes an element from the middle and prepends the tool name.
    #[test]
    fn test_prepend_without_removes_middle_element() {
        let args: Vec<String> = vec!["--show-stats".into(), "test".into(), "--release".into()];
        // skip_idx=1 removes "test"; result is ["cargo", "--show-stats", "--release"]
        let result = prepend_without("cargo", &args, 1);
        assert_eq!(result, vec!["cargo", "--show-stats", "--release"]);
    }

    /// Removes the first element and prepends the tool name.
    #[test]
    fn test_prepend_without_removes_first_element() {
        let args: Vec<String> = vec!["test".into(), "--release".into()];
        // skip_idx=0 removes "test"; result is ["cargo", "--release"]
        let result = prepend_without("cargo", &args, 0);
        assert_eq!(result, vec!["cargo", "--release"]);
    }

    /// Removes the last element and prepends the tool name.
    #[test]
    fn test_prepend_without_removes_last_element() {
        let args: Vec<String> = vec!["--release".into(), "test".into()];
        // skip_idx=1 removes "test"; result is ["cargo", "--release"]
        let result = prepend_without("cargo", &args, 1);
        assert_eq!(result, vec!["cargo", "--release"]);
    }

    /// Single-element slice: removes that element, leaving only the tool name.
    #[test]
    fn test_prepend_without_single_element_slice() {
        let args: Vec<String> = vec!["test".into()];
        let result = prepend_without("cargo", &args, 0);
        assert_eq!(result, vec!["cargo"]);
    }

    // ========================================================================
    // dispatch() coverage — KNOWN_SUBCOMMANDS sync guard
    // ========================================================================

    /// Verify that every entry in KNOWN_SUBCOMMANDS routes through dispatch()
    /// without panicking.
    ///
    /// dispatch() calls real subcommand handlers which may fail for unrelated
    /// reasons (missing binary, empty args), but they must never panic. Any
    /// panic here means a match arm is missing for a registered subcommand.
    #[test]
    fn test_dispatch_covers_all_known_subcommands() {
        use std::panic;

        for &subcommand in KNOWN_SUBCOMMANDS {
            // Pass --help so handlers exit cleanly rather than spawning real
            // processes. Most category handlers print help and return SUCCESS
            // when --help is the only arg; the deprecated category arms forward
            // straight to the handler with the same args.
            let args: Vec<String> = vec!["--help".to_string()];

            // AnalyticsConfig is not UnwindSafe, so construct it inside the closure.
            let result = panic::catch_unwind(|| {
                let a = crate::analytics::AnalyticsConfig {
                    enabled: false,
                    session_id: None,
                    input_cost_per_mtok: None,
                };
                dispatch(subcommand, &args, &a)
            });

            if let Err(ref payload) = result {
                // Surface the panic payload so non-routing panics (real bugs) are
                // distinguishable from missing-match-arm panics in CI output.
                let msg = payload
                    .downcast_ref::<String>()
                    .map(|s| s.as_str())
                    .or_else(|| payload.downcast_ref::<&str>().copied())
                    .unwrap_or("<non-string panic payload>");
                eprintln!(
                    "dispatch() panicked for '{subcommand}' — panic payload: {msg}"
                );
            }
            assert!(
                result.is_ok(),
                "dispatch() panicked for known subcommand '{subcommand}': \
                 entry is in KNOWN_SUBCOMMANDS but has no match arm in dispatch()"
            );
        }

        // Also verify that an unknown name correctly returns an Err from dispatch().
        let analytics = crate::analytics::AnalyticsConfig {
            enabled: false,
            session_id: None,
            input_cost_per_mtok: None,
        };
        let unknown_result = dispatch("__unknown_xyz__", &[], &analytics);
        assert!(
            unknown_result.is_err(),
            "dispatch() should return Err for unknown subcommand"
        );
        let err_msg = unknown_result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Unknown subcommand"),
            "error message should mention 'Unknown subcommand', got: {err_msg}"
        );
    }
}
