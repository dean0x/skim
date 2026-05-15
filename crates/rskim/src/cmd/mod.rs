//! Subcommand infrastructure for skim CLI.
//!
//! Provides pre-parse routing for optional subcommands while keeping
//! backward compatibility with file-first invocations. Also provides shared
//! helper functions used by subcommand parsers (arg inspection, flag injection,
//! command execution with three-tier parse degradation).
//!
//! # Dispatcher behavioral models
//!
//! There are two distinct behaviors for multi-category dispatchers, chosen
//! intentionally based on how each tool is typically used in practice:
//!
//! **Strict dispatchers** (`cargo`, `go`): Unknown subcommands print an error
//! and return `ExitCode::FAILURE`. These tools have well-defined, finite
//! subcommand sets that skim supports comprehensively. An unrecognized subcommand
//! almost certainly means a typo or a command that belongs in a different context.
//!
//! **Passthrough dispatchers** (`swift`, `dotnet`): Unknown subcommands are
//! forwarded verbatim to the underlying tool and return its exit code. These
//! tools expose a wide surface area of lifecycle subcommands (`build`, `run`,
//! `publish`, `package`, `restore`, …) that agents invoke routinely. Blocking
//! unknown subcommands would make skim unusable in normal project workflows;
//! passthrough lets skim compress only what it understands while staying
//! transparent for everything else.

mod agents;
mod build;
mod completions;
mod db;
mod discover;
mod file;
mod git;
mod heatmap;
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
mod search;
mod session;
pub(crate) mod session_sidecar;
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

/// Resolve the skim cache directory for use by callers outside the `cmd` module.
///
/// Delegates to [`hook_log::CacheEnv`] so that `SKIM_CACHE_DIR` overrides are
/// respected consistently everywhere.
pub(crate) fn resolve_cache_dir() -> Option<std::path::PathBuf> {
    hook_log::CacheEnv::from_process().resolve_cache_dir()
}

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
/// v2.8.0: Flat dispatch — tool names are top-level subcommands.
///
/// NOTE: This array is NOT used by the dispatch router. Its current purposes are:
///   1. Shell completion candidates (completions subcommand)
///   2. Sync-guard test (`test_dispatch_covers_all_known_subcommands`) — asserts
///      every registered name reaches a match arm in `dispatch()` without panicking.
pub(crate) const KNOWN_SUBCOMMANDS: &[&str] = &[
    // Meta/utility (unchanged)
    "agents",
    "completions",
    "discover",
    "git",
    "heatmap",
    "init",
    "learn",
    "log",
    "rewrite",
    "search",
    "stats",
    // Multi-category dispatchers
    "cargo",
    "go",
    // Test runners
    "cypress",
    "dotnet",
    "jest",
    "playwright",
    "pytest",
    "swift",
    "vitest",
    // Build tools
    "gradle",
    "gradlew",
    "make",
    "mvn",
    "mvnw",
    "tsc",
    // Linters (13)
    "biome",
    "black",
    "dprint",
    "eslint",
    "gofmt",
    "golangci",
    "mypy",
    "oxlint",
    "prettier",
    "rubocop",
    "ruff",
    "rustfmt",
    "swiftlint",
    // Package managers
    "npm",
    "pip",
    "pnpm",
    "yarn",
    // Infrastructure
    "aws",
    "curl",
    "docker",
    "gh",
    "kubectl",
    "terraform",
    "wget",
    // Database
    "mysql",
    "psql",
    "sqlite3",
    // File operations
    "df",
    "diff",
    "du",
    "env",
    "find",
    "grep",
    "ls",
    "printenv",
    "ps",
    "rg",
    "tree",
    "wc",
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
    /// When `true`, skip ANSI escape stripping on the raw command output.
    ///
    /// `strip_ansi_escapes` treats ASCII control codes — including `\t` (0x09) —
    /// as part of escape sequences and drops them. DB tools emit tab-separated
    /// (TSV) output; stripping would remove tab separators and cause all DB
    /// parsers to fall through to Passthrough. Set `true` in `run_db_tool`,
    /// `false` for all other families.
    pub skip_ansi_strip: bool,
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
        skip_ansi_strip,
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
    let mut v = Vec::with_capacity(args.len() + 1);
    v.push(tool.to_string());
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
    debug_assert!(
        skip_idx < args.len(),
        "skip_idx {skip_idx} out of bounds for args len {}",
        args.len()
    );
    let mut v = Vec::with_capacity(args.len()); // remove one, prepend one → same len
    v.push(tool.to_string());
    v.extend(
        args.iter()
            .enumerate()
            .filter(|(i, _)| *i != skip_idx)
            .map(|(_, s)| s.clone()),
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
                "skim cargo: unknown subcommand '{safe}'\n\n\
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

    let Some((subcmd, idx)) = extract_subcmd("go", args, "skim go <test> [args...]", "test")?
    else {
        return Ok(ExitCode::FAILURE);
    };

    match subcmd {
        "test" => test::run(&prepend_without("go", args, idx), analytics),
        unknown => {
            let safe = sanitize_for_display(unknown);
            eprintln!(
                "skim go: unknown subcommand '{safe}'\n\n\
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

fn print_swift_help() {
    print!(
        "skim swift\n\
         \n\
           Swift subcommand compression\n\
         \n\
         Usage: skim swift <SUBCOMMAND> [args...]\n\
         \n\
         Subcommands:\n\
           test       Run and compress swift test output\n\
         \n\
         Other subcommands (build, run, etc.) are passed through unmodified.\n\
         \n\
         Examples:\n\
           skim swift test\n\
           skim swift test --filter MyTests\n"
    );
}

fn print_dotnet_help() {
    print!(
        "skim dotnet\n\
         \n\
           .NET subcommand compression\n\
         \n\
         Usage: skim dotnet <SUBCOMMAND> [args...]\n\
         \n\
         Subcommands:\n\
           test       Run and compress dotnet test output\n\
         \n\
         Other subcommands (build, run, publish, restore, etc.) are passed through unmodified.\n\
         \n\
         Examples:\n\
           skim dotnet test\n\
           skim dotnet test --filter Category=Unit\n"
    );
}

/// Run a program with the given args and env vars, printing stdout/stderr and
/// returning the process exit code. Used by passthrough dispatchers for unknown
/// subcommands that skim does not compress.
fn run_raw_passthrough(
    program: &str,
    args: &[String],
    env: &[(&str, &str)],
) -> anyhow::Result<ExitCode> {
    let runner = crate::runner::CommandRunner::new(Some(DEFAULT_CMD_TIMEOUT));
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = runner.run_with_env(program, &arg_refs, env)?;
    print!("{}", output.stdout);
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }
    let code = output.exit_code.unwrap_or(1).clamp(0, 255) as u8;
    Ok(ExitCode::from(code))
}

/// Route `skim swift <subcmd> [args...]` to the correct category handler.
///
/// Only `swift test` is compressed. Other `swift` subcommands (build, run, etc.)
/// pass through as raw to avoid interrupting normal swift workflows.
fn dispatch_swift(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_swift_help();
        return Ok(ExitCode::SUCCESS);
    }

    let Some((subcmd, idx)) = extract_subcmd("swift", args, "skim swift <test> [args...]", "test")?
    else {
        return Ok(ExitCode::FAILURE);
    };

    match subcmd {
        "test" => test::run(&prepend_without("swift", args, idx), analytics),
        unknown => {
            // Unknown swift subcommand → raw passthrough (passthrough dispatcher model)
            let safe = sanitize_for_display(unknown);
            eprintln!(
                "skim swift: unknown subcommand '{safe}' — passing through\n\
                 Supported subcommands: test"
            );
            let mut all_args: Vec<String> = vec![unknown.to_string()];
            all_args.extend(
                args.iter()
                    .enumerate()
                    .filter(|(i, _)| *i != idx)
                    .map(|(_, s)| s.clone()),
            );
            run_raw_passthrough("swift", &all_args, &[])
        }
    }
}

/// Route `skim dotnet <subcmd> [args...]` to the correct category handler.
///
/// Only `dotnet test` is compressed. Other `dotnet` subcommands (build, run, publish, etc.)
/// pass through as raw to avoid interrupting normal dotnet workflows.
fn dispatch_dotnet(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_dotnet_help();
        return Ok(ExitCode::SUCCESS);
    }

    let Some((subcmd, idx)) =
        extract_subcmd("dotnet", args, "skim dotnet <test> [args...]", "test")?
    else {
        return Ok(ExitCode::FAILURE);
    };

    match subcmd {
        "test" => test::run(&prepend_without("dotnet", args, idx), analytics),
        unknown => {
            // Unknown dotnet subcommand → raw passthrough (passthrough dispatcher model)
            let safe = sanitize_for_display(unknown);
            eprintln!(
                "skim dotnet: unknown subcommand '{safe}' — passing through\n\
                 Supported subcommands: test"
            );
            let mut all_args: Vec<String> = vec![unknown.to_string()];
            all_args.extend(
                args.iter()
                    .enumerate()
                    .filter(|(i, _)| *i != idx)
                    .map(|(_, s)| s.clone()),
            );
            // DOTNET_CLI_UI_LANGUAGE forces English output for reliable parsing
            // even in passthrough mode, matching the compressed-path behavior.
            run_raw_passthrough("dotnet", &all_args, &[("DOTNET_CLI_UI_LANGUAGE", "en-US")])
        }
    }
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
        "heatmap" => heatmap::run(args, analytics),
        "init" => init::run(args, analytics),
        "learn" => learn::run(args, analytics),
        "log" => log::run(args, analytics),
        "rewrite" => rewrite::run(args, analytics),
        "search" => search::run(args, analytics),
        "stats" => stats::run(args, analytics),

        // Multi-category dispatchers
        "cargo" => dispatch_cargo(args, analytics),
        "go" => dispatch_go(args, analytics),

        // Multi-category dispatchers for tools with subcommands
        "swift" => dispatch_swift(args, analytics),
        "dotnet" => dispatch_dotnet(args, analytics),

        // Direct-to-category routing (prepend tool name for category dispatcher)
        "cypress" | "jest" | "playwright" | "pytest" | "vitest" => {
            test::run(&prepend(subcommand, args), analytics)
        }
        "gradle" | "gradlew" | "make" | "mvn" | "mvnw" | "tsc" => {
            build::run(&prepend(subcommand, args), analytics)
        }
        "biome" | "black" | "dprint" | "eslint" | "gofmt" | "golangci" | "mypy" | "oxlint"
        | "prettier" | "rubocop" | "ruff" | "rustfmt" | "swiftlint" => {
            lint::run(&prepend(subcommand, args), analytics)
        }
        "npm" | "pip" | "pnpm" | "yarn" => pkg::run(&prepend(subcommand, args), analytics),
        "aws" | "curl" | "docker" | "gh" | "kubectl" | "terraform" | "wget" => {
            infra::run(&prepend(subcommand, args), analytics)
        }
        "mysql" | "psql" | "sqlite3" => db::run(&prepend(subcommand, args), analytics),
        "df" | "diff" | "du" | "env" | "find" | "grep" | "ls" | "printenv" | "ps" | "rg"
        | "tree" | "wc" => file::run(&prepend(subcommand, args), analytics),

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
///
/// For the `"db"` family, sensitive flags (passwords, usernames, hostnames)
/// are redacted before the label is stored.  DB commands frequently embed
/// credentials in positional flags (`-p S3cret`, `--password=S3cret`,
/// `-U admin`, `--user=admin`, `-h myhost`, `--host=myhost`), and these
/// must not persist to the analytics SQLite database.
pub(crate) fn format_analytics_label(family: &str, program: &str, rest: &str) -> String {
    if rest.is_empty() {
        format!("skim {family} {program}")
    } else if family == "db" {
        let scrubbed = scrub_db_args(rest);
        format!("skim {family} {program} {scrubbed}")
    } else {
        format!("skim {family} {program} {rest}")
    }
}

/// Scrub credential values from a DB tool argument string.
///
/// DB CLIs accept credentials as flag-value pairs.  This function replaces the
/// value of every sensitive flag with `[REDACTED]` so that analytics labels
/// never persist passwords, usernames, or hostnames to disk.
///
/// # Flags redacted
///
/// | Short form  | Long form        | Tools      |
/// |-------------|------------------|------------|
/// | `-p`        | `--password`     | mysql      |
/// | `-P`        | (none)           | mysql port |
/// | `-U`        | `--username`     | psql       |
/// | `-u`        | `--user`         | mysql      |
/// | `-h`        | `--host`         | psql/mysql |
/// | `-W`        | `--password`     | psql       |
///
/// Both space-separated (`-p S3cret`) and equals-joined (`--password=S3cret`)
/// forms are redacted.
///
/// # Design
///
/// Operates on the pre-joined argument string (one token at a time after
/// splitting on whitespace) because that is what the call site produces.
/// This avoids a separate allocation path for every DB command invocation.
///
/// SQL query arguments (positional, no flag prefix) are preserved verbatim —
/// only known sensitive flag values are redacted.
///
/// Handles:
/// 1. Connection string URIs (`postgresql://user:pass@host`, `mysql://user:pass@host`)
/// 2. `--flag=value` form for sensitive and config-file flags
/// 3. Attached short flags with no space: `-pPassword`, `-uroot`, `-Uadmin`
/// 4. Space-separated sensitive flags: `-p secret`, `--password secret`
/// 5. `--defaults-file` / `--defaults-extra-file` MySQL config file flags
/// 6. `-P` (port) is NOT redacted — it is not a credential
pub(crate) fn scrub_db_args(args: &str) -> String {
    /// Flags whose *immediately following* space-separated token is a credential.
    /// Note: `-P` (port) intentionally omitted — it is not a credential.
    const SENSITIVE_FLAGS: &[&str] = &[
        "-p",
        "-U",
        "-u",
        "-h",
        "-W",
        "--password",
        "--user",
        "--username",
        "--host",
    ];
    /// Short flags that may have their value *attached* with no space (e.g. `-pS3cret`).
    const ATTACHED_PREFIXES: &[&str] = &["-p", "-u", "-U"];
    /// MySQL config-file flags whose value (path) must also be redacted.
    const CONFIG_FILE_FLAGS: &[&str] = &["--defaults-file", "--defaults-extra-file"];

    let tokens: Vec<&str> = args.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;

    while i < tokens.len() {
        let tok = tokens[i];

        // 1. Connection string URIs: postgresql://user:pass@host, mysql://user:pass@host
        if (tok.starts_with("postgresql://")
            || tok.starts_with("postgres://")
            || tok.starts_with("mysql://"))
            && tok.contains('@')
        {
            out.push("[REDACTED_URI]".to_string());
            i += 1;
            continue;
        }

        // 2. `--flag=value` form (sensitive flags and config-file flags)
        if let Some(eq_pos) = tok.find('=') {
            let flag = &tok[..eq_pos];
            if SENSITIVE_FLAGS.contains(&flag) || CONFIG_FILE_FLAGS.contains(&flag) {
                out.push(format!("{flag}=[REDACTED]"));
                i += 1;
                continue;
            }
        }

        // 3. Attached short flags: -pPassword, -uroot, -Uadmin (no space between flag and value)
        //    Only applies to single-dash (short) flags that are NOT `--` prefixed.
        if !tok.starts_with("--")
            && let Some(&prefix) = ATTACHED_PREFIXES
                .iter()
                .find(|&&p| tok.starts_with(p) && tok.len() > p.len())
        {
            out.push(format!("{prefix}[REDACTED]"));
            i += 1;
            continue;
        }

        // 4. Space-separated sensitive flags and config-file flags
        if SENSITIVE_FLAGS.contains(&tok) || CONFIG_FILE_FLAGS.contains(&tok) {
            out.push(tok.to_string());
            i += 1;
            // Redact the following value token if present.
            if i < tokens.len() {
                out.push("[REDACTED]".to_string());
                i += 1;
            }
            continue;
        }

        out.push(tok.to_string());
        i += 1;
    }

    out.join(" ")
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

    /// Out-of-bounds skip_idx fires the debug_assert (debug builds only).
    ///
    /// This test documents the invariant: callers are responsible for passing a
    /// valid index.  The assert only fires in debug builds (`cfg(debug_assertions)`),
    /// so this test is gated on that condition.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "skip_idx 1 out of bounds for args len 1")]
    fn test_prepend_without_panics_on_out_of_bounds() {
        let args: Vec<String> = vec!["test".into()];
        prepend_without("cargo", &args, 1); // skip_idx=1 is out of bounds for len 1
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
            // when --help is the only arg.
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
                eprintln!("dispatch() panicked for '{subcommand}' — panic payload: {msg}");
            }
            assert!(
                result.is_ok(),
                "dispatch() panicked for known subcommand '{subcommand}': \
                 handler should not panic (check handler implementation)"
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

    // ========================================================================
    // scrub_db_args tests
    // ========================================================================

    #[test]
    fn test_scrub_db_args_mysql_short_password() {
        // `mysql -u root -p S3cret -e 'SELECT 1'` — space-separated short flag
        let input = "-u root -p S3cret -e SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("root"),
            "username after -u must be redacted: {result}"
        );
        assert!(
            !result.contains("S3cret"),
            "password after -p must be redacted: {result}"
        );
        assert!(
            result.contains("[REDACTED]"),
            "redaction marker must appear: {result}"
        );
        // SQL query must be preserved
        assert!(result.contains("SELECT"), "SQL must be preserved: {result}");
    }

    #[test]
    fn test_scrub_db_args_psql_equals_form() {
        // `psql --host=myhost --username=admin -c 'SELECT 1'`
        let input = "--host=myhost --username=admin -c SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("myhost"),
            "--host=value must be redacted: {result}"
        );
        assert!(
            !result.contains("admin"),
            "--username=value must be redacted: {result}"
        );
        // Flag names must be preserved (--host=[REDACTED], not just [REDACTED]).
        assert!(
            result.contains("--host="),
            "flag name --host must be retained: {result}"
        );
        assert!(
            result.contains("--username="),
            "flag name --username must be retained: {result}"
        );
        assert!(
            result.contains("-c"),
            "non-sensitive flag preserved: {result}"
        );
        assert!(result.contains("SELECT"), "SQL must be preserved: {result}");
    }

    #[test]
    fn test_scrub_db_args_no_credentials_unchanged() {
        // `-e 'SELECT 1'` contains no credential flags — returned as-is.
        let input = "-e SELECT 1 FROM users";
        let result = scrub_db_args(input);
        assert_eq!(result, input, "args with no credentials must be unchanged");
    }

    #[test]
    fn test_scrub_db_args_empty_string() {
        assert_eq!(scrub_db_args(""), "");
    }

    #[test]
    fn test_scrub_db_args_dangling_sensitive_flag() {
        // Sensitive flag at end of arg string with no following value.
        let input = "-c SELECT 1 -p";
        let result = scrub_db_args(input);
        assert!(result.contains("-p"), "dangling flag kept: {result}");
        // No [REDACTED] since there was no following token to redact.
        // The flag itself is not a secret; only the value after it is.
        assert!(
            !result.contains("[REDACTED]"),
            "no token to redact: {result}"
        );
    }

    // ========================================================================
    // format_analytics_label with db family scrubs credentials
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
    fn test_format_analytics_label_non_db_not_scrubbed() {
        // For non-db families the args are forwarded verbatim.
        let label = format_analytics_label("infra", "kubectl", "get pods -n myns");
        assert!(
            label.contains("myns"),
            "non-db family must not scrub args: {label}"
        );
    }

    #[test]
    fn test_format_analytics_label_db_empty_rest() {
        let label = format_analytics_label("db", "psql", "");
        assert_eq!(label, "skim db psql");
    }

    // ========================================================================
    // Fix 2: credential scrubbing gaps
    // ========================================================================

    #[test]
    fn test_scrub_db_args_mysql_attached_password() {
        // `-pS3cret -e SELECT 1` — password attached to -p with no space
        let input = "-pS3cret -e SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("S3cret"),
            "attached password must be redacted: {result}"
        );
        assert!(
            result.contains("-p[REDACTED]"),
            "redacted form must preserve flag name: {result}"
        );
        assert!(result.contains("SELECT"), "SQL must be preserved: {result}");
    }

    #[test]
    fn test_scrub_db_args_attached_user() {
        // `-uroot -pS3cret -e SELECT 1` — both user and password attached with no space
        let input = "-uroot -pS3cret -e SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("root"),
            "attached username must be redacted: {result}"
        );
        assert!(
            !result.contains("S3cret"),
            "attached password must be redacted: {result}"
        );
        assert!(result.contains("SELECT"), "SQL must be preserved: {result}");
    }

    #[test]
    fn test_scrub_db_args_connection_uri_psql() {
        // `postgresql://admin:hunter2@db.prod:5432/myapp -c SELECT 1`
        let input = "postgresql://admin:hunter2@db.prod:5432/myapp -c SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("admin"),
            "username in URI must be redacted: {result}"
        );
        assert!(
            !result.contains("hunter2"),
            "password in URI must be redacted: {result}"
        );
        assert!(
            result.contains("[REDACTED_URI]"),
            "URI redaction marker must appear: {result}"
        );
        assert!(result.contains("SELECT"), "SQL must be preserved: {result}");
    }

    #[test]
    fn test_scrub_db_args_connection_uri_mysql() {
        // `mysql://root:password@localhost/db -e SHOW TABLES`
        let input = "mysql://root:password@localhost/db -e SHOW TABLES";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("password"),
            "password in URI must be redacted: {result}"
        );
        assert!(
            result.contains("[REDACTED_URI]"),
            "URI redaction marker must appear: {result}"
        );
        assert!(
            result.contains("SHOW TABLES"),
            "SQL must be preserved: {result}"
        );
    }

    #[test]
    fn test_scrub_db_args_defaults_file_equals() {
        // `--defaults-file=/home/user/.my.cnf -e SELECT 1`
        let input = "--defaults-file=/home/user/.my.cnf -e SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("/home/user/.my.cnf"),
            "config file path must be redacted: {result}"
        );
        assert!(
            result.contains("--defaults-file=[REDACTED]"),
            "flag name must be preserved: {result}"
        );
    }

    #[test]
    fn test_scrub_db_args_defaults_file_space() {
        // `--defaults-file /home/user/.my.cnf -e SELECT 1`
        let input = "--defaults-file /home/user/.my.cnf -e SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            !result.contains("/home/user/.my.cnf"),
            "config file path in space-sep form must be redacted: {result}"
        );
        assert!(
            result.contains("--defaults-file"),
            "flag name must be preserved: {result}"
        );
    }

    #[test]
    fn test_scrub_db_args_port_not_redacted() {
        // `-P 3306 -e SELECT 1` — port is not a credential and must be preserved
        let input = "-P 3306 -e SELECT 1";
        let result = scrub_db_args(input);
        assert!(
            result.contains("3306"),
            "port number must NOT be redacted: {result}"
        );
        assert!(
            !result.contains("[REDACTED]"),
            "no redaction should occur for port: {result}"
        );
    }
}
