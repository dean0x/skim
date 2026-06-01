//! Cargo build/check/clippy/fmt output compression (#51)
//!
//! Handlers for four `cargo` subcommands:
//!
//! - **`cargo build` / `cargo check` / `cargo clippy`:** Three-tier NDJSON parser.
//!   - **Tier 1 (JSON):** Parse `--message-format=json` NDJSON from stdout.
//!     Track warnings/errors from `compiler-message` events, detect success
//!     from `build-finished` event.
//!   - **Tier 2 (regex):** Fall back to regex matching on stderr for
//!     `error[E\d+]` patterns when JSON parsing is unavailable.
//!   - **Tier 3 (passthrough):** Return raw output when nothing can be parsed.
//!
//! - **`cargo fmt`:** Passthrough-or-success parser. Empty combined output
//!   signals success; any non-empty output is passed through unchanged.

use std::collections::BTreeMap;
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;

use super::run_parsed_command;
use crate::cmd::{combine_output, inject_flag_before_separator, user_has_flag};
use crate::output::ParseResult;
use crate::output::canonical::BuildResult;
use crate::runner::CommandOutput;

// ============================================================================
// Compiled regex patterns (compiled once via LazyLock)
// ============================================================================

static CARGO_ERROR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"error\[E\d+\]").expect("valid regex"));

static CARGO_WARNING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^warning:").expect("valid regex"));

static CARGO_ERROR_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(error\[E\d+\]:.+)").expect("valid regex"));

// ============================================================================
// Public entry points
// ============================================================================

/// Run `cargo build` with output compression.
///
/// Injects `--message-format=json` if not already set by the user, then
/// parses the NDJSON output through the three-tier parser.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    run_with_json_format("build", args, show_stats, rec)
}

/// Run `cargo check` with output compression.
///
/// Injects `--message-format=json` if not already set by the user, then
/// parses the NDJSON output through the same three-tier parser as cargo build.
/// `cargo check` verifies types and borrow rules without producing an artifact,
/// so its JSON schema is identical to `cargo build`'s.
pub(crate) fn run_check(
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    run_with_json_format("check", args, show_stats, rec)
}

/// Run `cargo fmt` with output compression.
///
/// `cargo fmt` reformats source in-place and emits output only on error.
/// An empty combined output is treated as success. Non-empty output (e.g.
/// diff output from `--check` mode falling through, or rustfmt errors)
/// is passed through unchanged.
///
/// Note: `cargo fmt --check` is ACKed at the engine level (AD-RW-11) and
/// never reaches this handler. This handler covers bare `cargo fmt` and
/// `cargo fmt -- [rustfmt args]` (apply mode).
pub(crate) fn run_fmt(
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    let mut full_args = vec!["fmt".to_string()];
    full_args.extend_from_slice(args);

    run_parsed_command(
        "cargo",
        &full_args,
        &[],
        "install Rust from https://rustup.rs",
        show_stats,
        rec,
        parse_fmt,
    )
}

/// Run `cargo clippy` with output compression.
///
/// Same JSON injection and parsing as cargo build, but with clippy-specific
/// grouping of warnings by lint rule code.
pub(crate) fn run_clippy(
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    run_with_json_format("clippy", args, show_stats, rec)
}

/// Shared implementation for `run`, `run_check`, and `run_clippy`.
///
/// All three subcommands inject `--message-format=json` and use the same
/// three-tier NDJSON parser. Only the subcommand token differs.
fn run_with_json_format(
    subcmd: &str,
    args: &[String],
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,
) -> anyhow::Result<ExitCode> {
    let mut full_args = vec![subcmd.to_string()];
    full_args.extend_from_slice(args);

    if !user_has_flag(&full_args, &["--message-format"]) {
        inject_flag_before_separator(&mut full_args, "--message-format=json");
    }

    run_parsed_command(
        "cargo",
        &full_args,
        &[("CARGO_TERM_COLOR", "never")],
        "install Rust from https://rustup.rs",
        show_stats,
        rec,
        parse,
    )
}

// ============================================================================
// Parsers
// ============================================================================

/// Parse `cargo fmt` output.
///
/// `cargo fmt` writes to combined stdout+stderr only when it encounters
/// errors (e.g. `rustfmt` not installed, unformatted files in `--check` mode
/// that bypass the ACK path). An empty combined output signals success.
/// Any non-empty output is passed through unchanged.
fn parse_fmt(output: &CommandOutput) -> ParseResult<BuildResult> {
    let combined = combine_output(output);
    let trimmed = combined.trim();
    if trimmed.is_empty() {
        ParseResult::Full(BuildResult::new(true, 0, 0, None, vec![]))
    } else {
        ParseResult::Passthrough(trimmed.to_string())
    }
}

/// Parse cargo build/clippy output through three degradation tiers.
fn parse(output: &CommandOutput) -> ParseResult<BuildResult> {
    // Tier 1: JSON parse of stdout NDJSON
    if let Some(result) = try_tier1_json(&output.stdout) {
        return result;
    }

    // Tier 2: Regex on stderr
    if let Some(result) = try_tier2_regex(&output.stderr) {
        return result;
    }

    // Tier 3: Passthrough
    let combined = if output.stderr.is_empty() {
        output.stdout.clone()
    } else if output.stdout.is_empty() {
        output.stderr.clone()
    } else {
        format!("{}\n{}", output.stdout, output.stderr)
    };

    ParseResult::Passthrough(combined)
}

/// Tier 1: Parse NDJSON lines from cargo's `--message-format=json` output.
///
/// Looks for:
/// - `{"reason":"compiler-message",...}` entries to count warnings/errors
/// - `{"reason":"build-finished","success":true/false}` for final status
fn try_tier1_json(stdout: &str) -> Option<ParseResult<BuildResult>> {
    let mut warnings: usize = 0;
    let mut errors: usize = 0;
    let mut error_messages: Vec<String> = Vec::new();
    let mut warning_codes: BTreeMap<String, usize> = BTreeMap::new();
    let mut found_build_finished = false;
    let mut success = false;

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let json: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let reason = json.get("reason").and_then(|v| v.as_str());

        match reason {
            Some("compiler-message") => {
                if let Some(message) = json.get("message") {
                    let level = message.get("level").and_then(|v| v.as_str()).unwrap_or("");
                    let msg_text = message
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let code = message
                        .get("code")
                        .and_then(|v| v.get("code"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    // Extract file and line from spans
                    let location = message
                        .get("spans")
                        .and_then(|v| v.as_array())
                        .and_then(|spans| spans.first())
                        .map(|span| {
                            let file = span
                                .get("file_name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");
                            let line = span.get("line_start").and_then(|v| v.as_u64()).unwrap_or(0);
                            format!("{file}:{line}")
                        })
                        .unwrap_or_default();

                    match level {
                        "error" => {
                            errors += 1;
                            let formatted = if !code.is_empty() && !location.is_empty() {
                                format!("error[{code}]: {msg_text} in {location}")
                            } else if !code.is_empty() {
                                format!("error[{code}]: {msg_text}")
                            } else if !location.is_empty() {
                                format!("error: {msg_text} in {location}")
                            } else {
                                format!("error: {msg_text}")
                            };
                            error_messages.push(formatted);
                        }
                        "warning" => {
                            warnings += 1;
                            if !code.is_empty() {
                                *warning_codes.entry(code.to_string()).or_insert(0) += 1;
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("build-finished") => {
                found_build_finished = true;
                success = json
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
            }
            _ => {}
        }
    }

    // Require the build-finished event for a Full result
    if !found_build_finished {
        return None;
    }

    // For clippy: append grouped warning code summaries to error_messages.
    // These are only rendered when `!success` (see BuildResult::render), so on
    // a successful clippy run they are silently carried but not displayed.
    // Acceptable for v1 — a dedicated `warning_messages` field can be added if
    // we need to render warnings on success in the future.
    if !warning_codes.is_empty() {
        for (code, count) in &warning_codes {
            error_messages.push(format!("{code}: {count} occurrence(s)"));
        }
    }

    let duration_ms = None; // Cargo doesn't report build duration in JSON
    let result = BuildResult::new(success, warnings, errors, duration_ms, error_messages);

    Some(ParseResult::Full(result))
}

/// Tier 2: Regex-based fallback parsing on stderr.
///
/// Matches `error[E\d+]` and `warning:` patterns to approximate counts.
fn try_tier2_regex(stderr: &str) -> Option<ParseResult<BuildResult>> {
    if stderr.trim().is_empty() {
        return None;
    }

    let error_count = CARGO_ERROR_RE.find_iter(stderr).count();
    let warning_count = CARGO_WARNING_RE.find_iter(stderr).count();

    if error_count == 0 && warning_count == 0 {
        return None;
    }

    // Extract error messages from lines matching the pattern
    let error_messages: Vec<String> = CARGO_ERROR_LINE_RE
        .captures_iter(stderr)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .collect();

    let success = error_count == 0;
    let result = BuildResult::new(success, warning_count, error_count, None, error_messages);

    Some(ParseResult::Degraded(
        result,
        vec!["cargo build: structured parse failed, using regex".to_string()],
    ))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_support::{load_fixture, make_output_full};

    // ========================================================================
    // Tier 1: JSON parsing
    // ========================================================================

    #[test]
    fn test_tier1_build_success() {
        let stdout = load_fixture("build", "cargo_build_ok.json");
        let output = make_output_full(&stdout, "", Some(0));
        let result = parse(&output);

        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(build_result) = &result {
            assert!(build_result.success, "expected success");
            assert_eq!(build_result.errors, 0);
        }
    }

    #[test]
    fn test_tier1_build_failure() {
        let stdout = load_fixture("build", "cargo_build_fail.json");
        let output = make_output_full(&stdout, "", Some(101));
        let result = parse(&output);

        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(build_result) = &result {
            assert!(!build_result.success, "expected failure");
            assert!(build_result.errors > 0, "expected errors > 0");
        }
    }

    #[test]
    fn test_tier1_clippy_warnings() {
        let stdout = load_fixture("build", "clippy_warnings.json");
        let output = make_output_full(&stdout, "", Some(0));
        let result = parse(&output);

        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(build_result) = &result {
            assert_eq!(build_result.warnings, 2, "expected 2 warnings");
            assert!(build_result.success, "expected success");
        }
    }

    #[test]
    fn test_tier1_clippy_warning_codes_grouped() {
        let stdout = load_fixture("build", "clippy_warnings.json");
        let output = make_output_full(&stdout, "", Some(0));
        let result = parse(&output);

        if let ParseResult::Full(build_result) = &result {
            // Warning codes should be grouped and appended to error_messages
            assert!(
                build_result
                    .error_messages
                    .iter()
                    .any(|m| m.contains("dead_code")),
                "expected warning code 'dead_code' in error_messages, got: {:?}",
                build_result.error_messages
            );
            // The fixture has 2 dead_code warnings, so the grouped entry
            // should reflect the count
            assert!(
                build_result
                    .error_messages
                    .iter()
                    .any(|m| m.contains("2 occurrence(s)")),
                "expected '2 occurrence(s)' in error_messages, got: {:?}",
                build_result.error_messages
            );
        } else {
            panic!("expected Full result");
        }
    }

    #[test]
    fn test_flag_injection_skipped() {
        // If user already has --message-format=json2, we should not inject our own
        let args = vec!["--message-format=json2".to_string()];
        assert!(
            user_has_flag(&args, &["--message-format"]),
            "should detect existing --message-format flag"
        );
    }

    #[test]
    fn test_user_message_format_skips_injection_and_falls_through() {
        // When user provides --message-format=short, we skip JSON injection.
        // Cargo then emits human-readable text instead of JSON, so tier 1
        // (JSON) fails and the output falls through to tier 2 or tier 3.
        //
        // Simulate: cargo outputs human text to stderr (no JSON on stdout).
        let stderr = "error[E0308]: mismatched types\n  --> src/main.rs:10:5\n";
        let output = make_output_full("", stderr, Some(101));

        // Verify flag detection prevents injection
        let user_args = vec!["build".to_string(), "--message-format=short".to_string()];
        assert!(
            user_has_flag(&user_args, &["--message-format"]),
            "should detect user's --message-format flag"
        );

        // Verify parser still works via tier 2 regex fallback
        let result = parse(&output);
        assert!(
            result.is_degraded(),
            "expected Degraded (tier 2) when JSON unavailable, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Degraded(build_result, _) = &result {
            assert_eq!(build_result.errors, 1, "expected 1 error from regex tier");
            assert!(!build_result.success, "expected failure");
        }
    }

    // ========================================================================
    // Tier 2: Regex fallback
    // ========================================================================

    #[test]
    fn test_tier2_regex_errors() {
        let stderr = "error[E0308]: mismatched types\n  --> src/main.rs:10:5\nerror[E0425]: cannot find value\n";
        let output = make_output_full("", stderr, Some(101));
        let result = parse(&output);

        assert!(
            result.is_degraded(),
            "expected Degraded, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Degraded(build_result, markers) = &result {
            assert_eq!(build_result.errors, 2, "expected 2 errors from regex");
            assert!(!build_result.success, "expected failure");
            assert!(
                markers.contains(&"cargo build: structured parse failed, using regex".to_string())
            );
        }
    }

    // ========================================================================
    // Tier 3: Passthrough
    // ========================================================================

    #[test]
    fn test_tier3_passthrough() {
        let output = make_output_full("some random output", "", Some(0));
        let result = parse(&output);

        assert!(
            result.is_passthrough(),
            "expected Passthrough, got {:?}",
            result.tier_name()
        );
    }

    // ========================================================================
    // cargo fmt parser
    // ========================================================================

    #[test]
    fn test_parse_fmt_empty_output_is_success() {
        let output = make_output_full("", "", Some(0));
        let result = parse_fmt(&output);
        assert!(
            result.is_full(),
            "expected Full, got {:?}",
            result.tier_name()
        );
        if let ParseResult::Full(build_result) = &result {
            assert!(build_result.success, "expected success for empty output");
            assert_eq!(build_result.errors, 0);
            assert_eq!(build_result.warnings, 0);
        }
    }

    #[test]
    fn test_parse_fmt_whitespace_only_is_success() {
        let output = make_output_full("  \n\n", " \t\n", Some(0));
        let result = parse_fmt(&output);
        assert!(result.is_full(), "expected Full for whitespace-only output");
        if let ParseResult::Full(build_result) = &result {
            assert!(build_result.success);
        }
    }

    #[test]
    fn test_parse_fmt_error_output_is_passthrough() {
        let stderr = "error: rustfmt not installed\n";
        let output = make_output_full("", stderr, Some(1));
        let result = parse_fmt(&output);
        assert!(
            result.is_passthrough(),
            "expected Passthrough for error output, got {:?}",
            result.tier_name()
        );
        assert!(result.content().contains("rustfmt not installed"));
    }

    #[test]
    fn test_parse_fmt_stdout_and_stderr_separated_by_newline() {
        // When both stdout and stderr have content, they must be joined with a
        // newline separator so the last line of stdout and first line of stderr
        // are not merged into a single line. Regression test for the
        // format!("{}{}") → combine_output fix.
        let stdout = "stdout line";
        let stderr = "stderr line";
        let output = make_output_full(stdout, stderr, Some(1));
        let result = parse_fmt(&output);
        assert!(
            result.is_passthrough(),
            "expected Passthrough for non-empty output, got {:?}",
            result.tier_name()
        );
        let content = result.content();
        // Lines must appear as separate lines, not merged into "stdout linestderr line"
        assert!(
            content.contains("stdout line\nstderr line")
                || (content.contains("stdout line") && content.contains("stderr line")),
            "stdout and stderr must both appear in combined output: {content:?}"
        );
        assert!(
            !content.contains("stdout linestderr line"),
            "stdout and stderr must be separated by a newline, not concatenated directly: {content:?}"
        );
    }

    // ========================================================================
    // Helper tests
    // ========================================================================

    #[test]
    fn test_inject_flag_before_separator() {
        let mut args = vec![
            "build".to_string(),
            "--release".to_string(),
            "--".to_string(),
            "-W".to_string(),
            "clippy::pedantic".to_string(),
        ];
        inject_flag_before_separator(&mut args, "--message-format=json");
        assert_eq!(args[2], "--message-format=json");
        assert_eq!(args[3], "--");
    }

    #[test]
    fn test_inject_flag_no_separator() {
        let mut args = vec!["build".to_string(), "--release".to_string()];
        inject_flag_before_separator(&mut args, "--message-format=json");
        assert_eq!(args.last().unwrap(), "--message-format=json");
    }

    #[test]
    fn test_user_has_flag_present() {
        let args = vec!["build".to_string(), "--message-format=json2".to_string()];
        assert!(user_has_flag(&args, &["--message-format"]));
    }

    #[test]
    fn test_user_has_flag_absent() {
        let args = vec!["build".to_string(), "--release".to_string()];
        assert!(!user_has_flag(&args, &["--message-format"]));
    }
}
