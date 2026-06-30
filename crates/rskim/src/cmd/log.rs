//! Generic log compression subcommand (#116).
//!
//! Compresses log output by deduplicating messages, stripping debug lines,
//! and collapsing stack traces. stdin-primary: `kubectl logs deploy/foo | skim log`
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON log lines (structured logging with level/msg fields)
//! - **Tier 2 (Degraded)**: Regex on common log formats (timestamp + level)
//! - **Tier 3 (Passthrough)**: Raw output
//!
//! # R1 — canonical compress_log lives in rskim-compress (AC26 / #327)
//!
//! The `compress_log` function delegates to `rskim_compress::log::compress_log`.
//! `rskim-core` MUST NOT gain `regex` (AC26), so the log machinery lives in the
//! `rskim-compress` crate where `regex` is an allowed dep.

use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;

use crate::output::ParseResult;
use crate::output::canonical::LogResult;

// ============================================================================
// Flags
// ============================================================================

#[derive(Debug, Default)]
pub(crate) struct LogFlags {
    pub(crate) no_dedup: bool,
    pub(crate) keep_timestamps: bool,
    pub(crate) keep_debug: bool,
    pub(crate) debug_only: bool,
    pub(crate) show_stats: bool,
    pub(crate) json_output: bool,
}

fn parse_flags(args: &[String]) -> LogFlags {
    let mut flags = LogFlags::default();
    for arg in args {
        match arg.as_str() {
            "--no-dedup" => flags.no_dedup = true,
            "--keep-timestamps" => flags.keep_timestamps = true,
            "--keep-debug" => flags.keep_debug = true,
            "--debug-only" => flags.debug_only = true,
            "--show-stats" => flags.show_stats = true,
            "--json" => flags.json_output = true,
            unknown if unknown.starts_with("--") => {
                let safe = crate::cmd::sanitize_for_display(unknown);
                eprintln!("warning: unknown flag '{safe}' — ignoring");
            }
            _ => {}
        }
    }
    flags
}

// ============================================================================
// Entry point
// ============================================================================

/// Run the `skim log` subcommand.
///
/// Reads from stdin (mandatory — log is stdin-primary).
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Issue 1: check terminal BEFORE flag parsing, regardless of args.
    // Without this, `skim log --keep-debug` with no pipe hangs on stdin.
    if io::stdin().is_terminal() {
        eprintln!("error: 'skim log' reads from stdin. Pipe log output to it:");
        eprintln!("  kubectl logs deploy/foo | skim log");
        eprintln!("  cat app.log | skim log");
        return Ok(ExitCode::FAILURE);
    }

    let flags = parse_flags(args);
    // Issue 5: capture start time before reading stdin for real duration tracking.
    let start = std::time::Instant::now();
    let stdin_buf = super::read_stdin_bounded()?;
    let raw_input = stdin_buf.as_str();
    let result = compress_log(raw_input, &flags);

    // Net-savings guard (Cluster C / #317):
    // For log, "raw" = stdin_buf (the user's log input).
    // Exempt: JSON output (must never rewrite to non-JSON) and already-passthrough tier.
    // The guard is applied before emit_result so we can choose what to write.
    let parse_tier = result.tier_name();
    let (compressed, effective_tier) = if !flags.json_output && parse_tier != "passthrough" {
        let compressed_str = result.content().to_string();
        match crate::cmd::execution::savings_decision(raw_input, &compressed_str) {
            crate::cmd::execution::SavingsDecision::Keep => {
                // Emit compressed normally.
                let s = emit_result(&result, &flags)?;
                (s, parse_tier)
            }
            crate::cmd::execution::SavingsDecision::Passthrough => {
                // Emit raw verbatim to stdout.
                let tier = crate::cmd::execution::emit_raw_passthrough(raw_input)?;
                (raw_input.to_string(), tier)
            }
        }
    } else {
        let s = emit_result(&result, &flags)?;
        (s, parse_tier)
    };

    // Issue 4: compute token counts before analytics to avoid re-tokenizing in
    // the background thread (avoids copying up to 64 MiB via raw_input.to_string()).
    let duration = start.elapsed();
    let (raw_tokens, compressed_tokens) = crate::process::count_token_pair(raw_input, &compressed);

    if flags.show_stats {
        crate::process::report_token_stats(raw_tokens, compressed_tokens, "");
    }

    record_analytics(
        analytics.enabled,
        raw_tokens,
        compressed_tokens,
        duration,
        effective_tier,
        analytics.session_id.as_deref(),
    );
    Ok(ExitCode::SUCCESS)
}

/// Record analytics for this command invocation (fire-and-forget).
fn record_analytics(
    enabled: bool,
    raw_tokens: Option<usize>,
    compressed_tokens: Option<usize>,
    duration: std::time::Duration,
    tier: &str,
    session_id: Option<&str>,
) {
    crate::analytics::try_record_command_with_counts(
        crate::analytics::RecordingContext {
            enabled,
            command_type: crate::analytics::CommandType::Log,
            parse_tier: Some(tier),
            session_id,
        },
        raw_tokens.unwrap_or(0),
        compressed_tokens.unwrap_or(0),
        "skim log".to_string(),
        duration,
    );
}

fn print_help() {
    println!("skim log [flags]");
    println!();
    println!("  Compress log output from stdin for AI context windows.");
    println!("  Deduplicates messages, strips debug lines, collapses stack traces.");
    println!();
    println!("Usage:");
    println!("  kubectl logs deploy/foo | skim log");
    println!("  cat app.log | skim log");
    println!();
    println!("Flags:");
    println!("  --no-dedup          Disable message deduplication");
    println!("  --keep-timestamps   Preserve timestamp prefixes");
    println!("  --keep-debug        Show all levels including DEBUG/TRACE");
    println!("  --debug-only        Show ONLY DEBUG/TRACE lines");
    println!("  --json              Emit structured JSON output");
    println!("  --show-stats        Show token statistics");
}

/// Build the clap `Command` definition for shell completions.
pub(super) fn command() -> clap::Command {
    clap::Command::new("log")
        .about("Compress log output from stdin for AI context windows")
        .arg(
            clap::Arg::new("no-dedup")
                .long("no-dedup")
                .action(clap::ArgAction::SetTrue)
                .help("Disable message deduplication"),
        )
        .arg(
            clap::Arg::new("keep-timestamps")
                .long("keep-timestamps")
                .action(clap::ArgAction::SetTrue)
                .help("Preserve timestamp prefixes"),
        )
        .arg(
            clap::Arg::new("keep-debug")
                .long("keep-debug")
                .action(clap::ArgAction::SetTrue)
                .help("Show all levels including DEBUG/TRACE"),
        )
        .arg(
            clap::Arg::new("debug-only")
                .long("debug-only")
                .action(clap::ArgAction::SetTrue)
                .help("Show ONLY DEBUG/TRACE lines"),
        )
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Emit structured JSON output"),
        )
        .arg(
            clap::Arg::new("show-stats")
                .long("show-stats")
                .action(clap::ArgAction::SetTrue)
                .help("Show token statistics"),
        )
}

// ============================================================================
// Delegation wrapper
// ============================================================================

/// Compress log lines into a structured ParseResult<LogResult>.
///
/// # R1 — Delegates to rskim-compress (AC26 / #327)
///
/// The canonical `compress_log` implementation lives in `rskim_compress::log`.
/// `rskim-core` MUST NOT gain `regex` (AC26: pure transform lib, zero regex refs
/// today). This wrapper converts between the binary's internal types and the
/// rskim-compress public types.
///
/// Behavior is identical to the original implementation (AC25 — no regression).
pub(crate) fn compress_log(input: &str, flags: &LogFlags) -> ParseResult<LogResult> {
    // Convert local LogFlags → rskim-compress LogFlags (identical fields).
    let compress_flags = rskim_compress::log::LogFlags {
        no_dedup: flags.no_dedup,
        keep_timestamps: flags.keep_timestamps,
        keep_debug: flags.keep_debug,
        debug_only: flags.debug_only,
        show_stats: flags.show_stats,
        json_output: flags.json_output,
    };

    // Call the canonical implementation in rskim-compress (#327 / R1).
    match rskim_compress::log::compress_log(input, &compress_flags) {
        rskim_compress::log::ParseResult::Full(r) => ParseResult::Full(convert_log_result(r)),
        rskim_compress::log::ParseResult::Degraded(r, w) => {
            ParseResult::Degraded(convert_log_result(r), w)
        }
        rskim_compress::log::ParseResult::Passthrough(s) => ParseResult::Passthrough(s),
    }
}

/// Convert from `rskim_compress::log::LogResult` to `crate::output::canonical::LogResult`.
///
/// Both types have identical fields; this conversion exists because the binary's
/// `output::canonical::LogResult` is used widely in the rskim codebase
/// (`emit_result`, tests, `output::canonical` tests) and cannot be replaced by
/// the rskim-compress type without a broader refactor (deferred to a later ticket).
fn convert_log_result(r: rskim_compress::log::LogResult) -> LogResult {
    let entries: Vec<crate::output::canonical::LogEntry> = r
        .entries
        .into_iter()
        .map(|e| crate::output::canonical::LogEntry {
            level: e.level,
            message: e.message,
            count: e.count,
        })
        .collect();
    LogResult::new_with_stack(
        r.total_lines,
        r.unique_messages,
        r.debug_hidden,
        r.deduplicated_count,
        entries,
        r.debug_only,
        r.stack_frames_elided,
    )
}

// ============================================================================
// Output emission
// ============================================================================

fn emit_result(result: &ParseResult<LogResult>, flags: &LogFlags) -> anyhow::Result<String> {
    let mut stdout = io::stdout().lock();

    let content = if flags.json_output {
        let json_str = result.to_json_envelope()?;
        writeln!(stdout, "{json_str}")?;
        stdout.flush()?;
        json_str
    } else {
        let text = result.content();
        write!(stdout, "{text}")?;
        if !text.is_empty() && !text.ends_with('\n') {
            writeln!(stdout)?;
        }
        stdout.flush()?;
        text.to_string()
    };

    Ok(content)
}

// ============================================================================
// Tests — CLI glue and delegation smoke tests only
//
// Logic tests (parsing, dedup, stack collapse, level classification, JSON/regex
// paths, etc.) live in rskim-compress::log where the implementation lives.
// See crates/rskim-compress/src/log.rs.
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_utils::load_fixture;

    fn make_flags() -> LogFlags {
        LogFlags::default()
    }

    // ============================================================================
    // Flag-parsing tests (CLI glue)
    // ============================================================================

    mod flag_tests {
        use super::*;

        #[test]
        fn test_parse_flags_defaults() {
            let flags = parse_flags(&[]);
            assert!(!flags.no_dedup);
            assert!(!flags.keep_timestamps);
            assert!(!flags.keep_debug);
            assert!(!flags.debug_only);
            assert!(!flags.show_stats);
            assert!(!flags.json_output);
        }

        #[test]
        fn test_parse_flags_all_set() {
            let args: Vec<String> = vec![
                "--no-dedup".into(),
                "--keep-timestamps".into(),
                "--keep-debug".into(),
                "--debug-only".into(),
                "--show-stats".into(),
                "--json".into(),
            ];
            let flags = parse_flags(&args);
            assert!(flags.no_dedup);
            assert!(flags.keep_timestamps);
            assert!(flags.keep_debug);
            assert!(flags.debug_only);
            assert!(flags.show_stats);
            assert!(flags.json_output);
        }

        #[test]
        fn test_unknown_flag_warning_does_not_panic() {
            // Unknown flags should warn to stderr but not crash.
            let args: Vec<String> = vec!["--unknown-flag".into(), "--keep-debug".into()];
            let flags = parse_flags(&args);
            // Known flag still parsed correctly despite unknown neighbor
            assert!(flags.keep_debug);
        }
    }

    // ============================================================================
    // Delegation smoke tests: ensure compress_log wrapper routes correctly
    // ============================================================================

    mod delegation_tests {
        use super::*;

        /// JSON input → Full tier (delegation smoke: wrapper converts tier correctly).
        #[test]
        fn test_compress_log_json_produces_full() {
            let input = load_fixture("log", "json_structured.jsonl");
            let flags = make_flags();
            let result = compress_log(&input, &flags);
            assert!(
                result.is_full(),
                "JSON log should produce Full tier via delegation, got {}",
                result.tier_name()
            );
        }

        /// Plaintext log → Degraded tier (delegation smoke: wrapper converts tier correctly).
        #[test]
        fn test_compress_log_plaintext_produces_degraded() {
            let input = load_fixture("log", "plaintext_mixed.txt");
            let flags = make_flags();
            let result = compress_log(&input, &flags);
            assert!(
                result.is_degraded(),
                "Plaintext log should produce Degraded tier via delegation, got {}",
                result.tier_name()
            );
        }

        /// Plain text without log levels → Passthrough (delegation smoke).
        #[test]
        fn test_plain_text_without_levels_returns_passthrough() {
            let input = "some plain text\nanother line\nno levels here\n";
            let flags = make_flags();
            let result = compress_log(input, &flags);
            assert!(
                result.is_passthrough(),
                "Plain text without log levels should produce Passthrough via delegation, got {}",
                result.tier_name()
            );
        }

        /// Flags are forwarded correctly: --keep-timestamps flag propagates through
        /// the delegation layer and affects output (no struct mismatch).
        #[test]
        fn test_flags_forwarded_keep_timestamps() {
            // With keep_timestamps=true, timestamps are preserved and Tier 2 regex cannot
            // strip them for level detection → falls through to Passthrough for this format.
            let input =
                "2024-01-15T10:30:00Z [INFO] server started\n2024-01-15T10:30:01Z [INFO] ready\n";
            let flags = LogFlags {
                keep_timestamps: true,
                ..LogFlags::default()
            };
            let result = compress_log(input, &flags);
            // Passthrough preserves content including timestamps.
            assert!(
                result.is_passthrough(),
                "TIMESTAMP [LEVEL] format with keep_timestamps falls to Passthrough; got {}",
                result.tier_name()
            );
            assert!(
                result.content().contains("2024-01-15"),
                "Passthrough should preserve raw content including timestamps"
            );
        }

        /// Flags are forwarded correctly: --no-dedup flag propagates through the
        /// delegation layer (no struct mismatch that would silently use defaults).
        #[test]
        fn test_flags_forwarded_no_dedup() {
            // With no_dedup=true, repeated identical lines are NOT merged.
            let input = "INFO: hello\nINFO: hello\nINFO: hello\n";
            let flags = LogFlags {
                no_dedup: true,
                ..LogFlags::default()
            };
            let result = compress_log(input, &flags);
            // Tier 2 parse → Degraded; --no-dedup keeps all 3 entries separate.
            assert!(
                !result.is_passthrough(),
                "Structured input should not be passthrough; got {}",
                result.tier_name()
            );
            // Content should not show the ×3 dedup marker.
            assert!(
                !result.content().contains('\u{d7}'),
                "With --no-dedup forwarded, no dedup multiplication marker expected; got: {}",
                result.content()
            );
        }
    }
}
