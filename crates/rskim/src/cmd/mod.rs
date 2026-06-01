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
pub(crate) mod build;
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
pub(crate) mod lint;
mod log;
mod pkg;
mod rewrite;
mod search;
mod session;
pub(crate) mod session_sidecar;
mod stats;
pub(crate) mod test;
pub(crate) mod ux;

use std::io::{self, Read};
use std::sync::LazyLock;
use std::time::Duration;

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

/// Cached `~/.skim/bin/` path — computed once on first access via [`LazyLock`].
///
/// `dirs::home_dir()` + two `.join()` calls allocate a fresh `PathBuf` on every
/// invocation. Since the home directory never changes within a process, computing
/// it once and caching it eliminates repeated allocation.  Returns `None` when
/// the home directory cannot be determined.
///
/// Matches the same pattern used by [`WRAPPER_TARGETS`].
static SKIM_WRAPPERS_DIR: LazyLock<Option<std::path::PathBuf>> =
    LazyLock::new(|| dirs::home_dir().map(|h| h.join(".skim").join("bin")));

/// Single authoritative source for `~/.skim/bin/` — the PATH-wrappers directory.
///
/// Returns `None` when the home directory cannot be determined. Both
/// `main::strip_skim_wrappers_from_path` (recursion prevention) and
/// `cmd::init::wrappers::wrappers_dir` (installer/uninstaller) delegate here so
/// that a future directory change requires only one edit.
///
/// Backed by [`SKIM_WRAPPERS_DIR`] — zero allocation on every call. Callers that
/// need an owned `PathBuf` can call `.to_path_buf()` on the returned `&'static Path`.
pub(crate) fn skim_wrappers_dir() -> Option<&'static std::path::Path> {
    SKIM_WRAPPERS_DIR.as_deref()
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

mod registry;
pub(crate) use registry::{
    KNOWN_SUBCOMMANDS, is_known_subcommand, is_meta_subcommand, wrapper_targets,
};

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

mod execution;
pub(crate) use execution::{
    OutputFormat, ParsedCommandConfig, RunContext, ToolRunConfig, combine_output,
    format_analytics_label, run_parsed_command_with_mode, run_tool,
};

mod dispatch;
pub(crate) use dispatch::{dispatch, run_raw_passthrough};

mod security;
pub(crate) use security::{sanitize_for_display, scrub_db_args};

#[cfg(test)]
pub(crate) mod test_support;

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
}
