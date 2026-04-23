//! Generic log compression subcommand (#116).
//!
//! Compresses log output by deduplicating messages, stripping debug lines,
//! and collapsing stack traces. stdin-primary: `kubectl logs deploy/foo | skim log`
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON log lines (structured logging with level/msg fields)
//! - **Tier 2 (Degraded)**: Regex on common log formats (timestamp + level)
//! - **Tier 3 (Passthrough)**: Raw output

use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::output::canonical::{LogEntry, LogResult};
use crate::output::ParseResult;

/// Maximum input lines before truncation.
const MAX_INPUT_LINES: usize = 100_000;

/// Maximum frames buffered in `pending_stack` before the oldest is dropped.
///
/// Keeps memory at O(PENDING_STACK_CAP) regardless of input length.
/// `flush_stack_frames` retains only the last 3 frames, so a cap of 4 is
/// sufficient: the sliding window holds the 4 most-recent frames, and the
/// flush then drops 1 more, yielding exactly the last 3 in output.
const PENDING_STACK_CAP: usize = 4;

/// Matches ISO8601 / common log timestamp prefix to strip before dedup.
/// e.g. `2024-01-15T10:30:00Z `, `2024-01-15 10:30:00 `, `[2024-01-15T10:30:00]`
static RE_LOG_TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\[?\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?\]?\s*",
    )
    .unwrap()
});

/// Matches bracket-style level: `[ERROR]`, `[INFO]`, etc.
static RE_LOG_LEVEL_BRACKET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(?i)(ERROR|WARN|WARNING|INFO|DEBUG|TRACE)\]\s*(.*)").unwrap());

/// Matches bare-level format: `ERROR message` or `ERROR: message`
static RE_LOG_LEVEL_BARE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?i)(ERROR|WARN|WARNING|INFO|DEBUG|TRACE):?\s+(.*)").unwrap());

/// Matches Java/Node.js/Python stack trace lines.
///
/// # AD-LOG-10 (2026-04-11) — Multi-language stack trace patterns
/// - Java/Node.js: `    at <method>` (leading whitespace + "at ")
/// - Python: `  File "...", line N` (leading whitespace + 'File "')
///
/// Both patterns are anchored to leading whitespace so they don't match
/// regular log lines that happen to contain these substrings mid-sentence.
static RE_LOG_STACK_TRACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:^\s+at\s+|^\s+File\s+")"#).unwrap());

// ============================================================================
// Flags
// ============================================================================

#[derive(Debug, Default)]
struct LogFlags {
    no_dedup: bool,
    keep_timestamps: bool,
    keep_debug: bool,
    debug_only: bool,
    show_stats: bool,
    json_output: bool,
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
    let compressed = emit_result(&result, &flags)?;

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
        result.tier_name(),
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
) {
    crate::analytics::try_record_command_with_counts(
        enabled,
        raw_tokens.unwrap_or(0),
        compressed_tokens.unwrap_or(0),
        "skim log".to_string(),
        crate::analytics::CommandType::Log,
        duration,
        Some(tier),
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
// Compression pipeline
// ============================================================================

/// Compress log lines into a structured ParseResult<LogResult>.
fn compress_log(input: &str, flags: &LogFlags) -> ParseResult<LogResult> {
    // Try Tier 1: structured JSON logs
    if let Some(result) = try_parse_json_logs(input, flags) {
        return ParseResult::Full(result);
    }

    // Try Tier 2: regex-based log formats
    if let Some(result) = try_parse_regex_logs(input, flags) {
        return ParseResult::Degraded(
            result,
            vec!["log: no structured entries found, using regex".to_string()],
        );
    }

    // Tier 3: passthrough
    ParseResult::Passthrough(input.to_string())
}

// ============================================================================
// Tier 1: structured JSON log lines
// ============================================================================

fn try_parse_json_logs(input: &str, flags: &LogFlags) -> Option<LogResult> {
    let first_line = input.lines().find(|l| !l.trim().is_empty())?;
    // Probe first line; bail if not JSON
    let _probe: Value = serde_json::from_str(first_line.trim()).ok()?;

    let mut all_entries: Vec<(Option<String>, String)> = Vec::with_capacity(256);
    let mut total_lines = 0usize;

    for line in input.lines().take(MAX_INPUT_LINES) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        total_lines += 1;

        let Ok(obj) = serde_json::from_str::<Value>(trimmed) else {
            // Non-JSON line mixed in — treat as plain message
            all_entries.push((None, trimmed.to_string()));
            continue;
        };

        let level = extract_json_level(&obj);
        let message = extract_json_message(&obj).unwrap_or_else(|| trimmed.to_string());
        all_entries.push((level, message));
    }

    Some(apply_compression(all_entries, total_lines, 0, flags))
}

/// Maximum byte length for a JSON log-level field.
///
/// Level values ("ERROR", "WARN", "INFO", "DEBUG", "TRACE") are well under
/// 32 bytes; anything longer is either malformed or adversarial.
const MAX_JSON_LEVEL_LEN: usize = 32;

/// Maximum byte length for a JSON log-message field.
///
/// 16 KiB preserves meaningful log text while bounding adversarial allocation.
/// Truncated values get a "[truncated]" suffix so consumers can detect elision.
const MAX_JSON_MSG_LEN: usize = 16 * 1024;

fn extract_json_level(obj: &Value) -> Option<String> {
    for key in &["level", "severity", "lvl", "log_level"] {
        if let Some(v) = obj.get(key).and_then(|v| v.as_str()) {
            // Fast path: level values ("ERROR", "WARN", "INFO", "DEBUG",
            // "TRACE") are overwhelmingly ASCII and well under 32 bytes.
            // Byte-length check is a reliable proxy for char-length when
            // the value is ASCII (byte == char), and level fields in
            // non-ASCII locales still fit comfortably within the 32-byte
            // cap (e.g. "WARNUNG" is 7 bytes / 7 chars, "ERREUR" is 6/6).
            // Only adversarially long or non-BMP level values take the slow
            // path, which is the case the cap exists to bound.
            if v.len() <= MAX_JSON_LEVEL_LEN {
                return Some(v.to_uppercase()); // 1 allocation
            }
            // Slow path: truncate at a char boundary to avoid splitting
            // multi-byte sequences, then uppercase.
            let truncated: String = v.chars().take(MAX_JSON_LEVEL_LEN).collect(); // alloc #1
            return Some(truncated.to_uppercase()); // alloc #2
        }
    }
    None
}

fn extract_json_message(obj: &Value) -> Option<String> {
    for key in &["msg", "message", "text", "body"] {
        if let Some(v) = obj.get(key).and_then(|v| v.as_str()) {
            if v.len() <= MAX_JSON_MSG_LEN {
                return Some(v.to_string());
            }
            // Truncate at a char boundary; append a marker so consumers know.
            let mut s = String::with_capacity(MAX_JSON_MSG_LEN + 11);
            for c in v.chars().take(MAX_JSON_MSG_LEN) {
                s.push(c);
            }
            s.push_str("[truncated]");
            return Some(s);
        }
    }
    None
}

// ============================================================================
// Tier 2: regex-based log formats
// ============================================================================

/// Parse regex-based log formats into a LogResult.
///
/// # AD-LOG-10 (2026-04-11) — Stack trace capture and last-3-frame elision
/// Stack trace lines (Java `at …`, Python `File "…"`) are buffered in
/// `pending_stack`. When the next log line is encountered, the accumulated
/// frames are attached to the previous entry's message:
///
/// 1. Total frame count is recorded as `total_frames`.
/// 2. Keep only the **last** 3 frames (preserving their relative order):
///    `pending_stack.iter().rev().take(3).rev()`.
/// 3. The frames are appended as a newline-joined suffix to the previous
///    entry's message.
/// 4. Elided count: `total_frames.saturating_sub(3)` — accumulated across
///    all entries and surfaced in `LogResult::stack_frames_elided`.
fn try_parse_regex_logs(input: &str, flags: &LogFlags) -> Option<LogResult> {
    let mut all_entries: Vec<(Option<String>, String)> = Vec::with_capacity(256);
    let mut total_lines = 0usize;
    let mut found_structured = false;
    // Stack trace capture state (AD-LOG-10)
    let mut pending_stack: Vec<String> = Vec::new();
    let mut total_stack_frames_elided: usize = 0;

    for line in input.lines().take(MAX_INPUT_LINES) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            // Blank lines flush the pending stack (end of exception block).
            if !pending_stack.is_empty() && !all_entries.is_empty() {
                flush_stack_frames(
                    &mut all_entries,
                    &mut pending_stack,
                    &mut total_stack_frames_elided,
                );
            }
            continue;
        }

        // Check on the original (untrimmed) line to detect leading whitespace.
        if RE_LOG_STACK_TRACE.is_match(line) {
            // Sliding-window cap: drop the oldest frame and count it as elided
            // immediately, keeping memory at O(PENDING_STACK_CAP).
            if pending_stack.len() >= PENDING_STACK_CAP {
                pending_stack.remove(0);
                total_stack_frames_elided += 1;
            }
            pending_stack.push(trimmed.to_string());
            continue;
        }

        // New log line: flush any accumulated stack frames onto the previous entry.
        if !pending_stack.is_empty() && !all_entries.is_empty() {
            flush_stack_frames(
                &mut all_entries,
                &mut pending_stack,
                &mut total_stack_frames_elided,
            );
        }

        total_lines += 1;

        let without_ts = strip_timestamp(trimmed, flags.keep_timestamps);
        if let Some((level, message)) = classify_log_line(without_ts) {
            all_entries.push((Some(level), message));
            found_structured = true;
        } else {
            all_entries.push((None, without_ts.to_string()));
        }
    }

    // Flush any trailing stack frames at end-of-input.
    if !pending_stack.is_empty() && !all_entries.is_empty() {
        flush_stack_frames(
            &mut all_entries,
            &mut pending_stack,
            &mut total_stack_frames_elided,
        );
    }

    // Issue 8: if no structured log levels were found, entries are plain text —
    // return None to fall through to Passthrough rather than producing a
    // misleading Degraded result.
    if !found_structured {
        return None;
    }

    Some(apply_compression(
        all_entries,
        total_lines,
        total_stack_frames_elided,
        flags,
    ))
}

/// Flush `pending_stack` onto the last entry in `all_entries`.
///
/// Keeps the last 3 frames (in original order) and appends them as a
/// newline-joined suffix. Increments `elided` by the number dropped.
///
/// Both `all_entries` and `pending_stack` must remain `&mut Vec` rather than
/// slices: `all_entries` uses `.last_mut()` (Vec-level borrow), `pending_stack`
/// calls `.clear()` which is not available on `&mut [_]`.
#[allow(clippy::ptr_arg)]
fn flush_stack_frames(
    all_entries: &mut Vec<(Option<String>, String)>,
    pending_stack: &mut Vec<String>,
    elided: &mut usize,
) {
    let total = pending_stack.len();
    let skip = total.saturating_sub(3);
    *elided += skip;

    if let Some((_, msg)) = all_entries.last_mut() {
        for frame in pending_stack.iter().skip(skip) {
            msg.push('\n');
            msg.push_str(frame);
        }
    }

    pending_stack.clear();
}

/// Strip the timestamp prefix from a log line, unless `keep_timestamps` is true.
fn strip_timestamp(line: &str, keep_timestamps: bool) -> &str {
    if keep_timestamps {
        line
    } else {
        RE_LOG_TIMESTAMP
            .find(line)
            .map(|m| &line[m.end()..])
            .unwrap_or(line)
    }
}

/// Classify a single log line (after timestamp stripping) into `(level, message)`.
///
/// Returns `None` if the line has no recognised log level prefix.
fn classify_log_line(line: &str) -> Option<(String, String)> {
    if let Some(caps) = RE_LOG_LEVEL_BRACKET.captures(line) {
        return Some((caps[1].to_uppercase(), caps[2].trim().to_string()));
    }
    if let Some(caps) = RE_LOG_LEVEL_BARE.captures(line) {
        return Some((caps[1].to_uppercase(), caps[2].trim().to_string()));
    }
    None
}

// ============================================================================
// Shared compression logic
// ============================================================================

/// Filter entries by debug/trace level according to flags.
///
/// Returns `(filtered_entries, debug_hidden_count)`.
fn filter_debug_entries(
    entries: Vec<(Option<String>, String)>,
    flags: &LogFlags,
) -> (Vec<(Option<String>, String)>, usize) {
    let mut debug_hidden = 0usize;
    let mut filtered = Vec::with_capacity(entries.len());

    for (level, message) in entries {
        let is_debug = level
            .as_deref()
            .map(|l| matches!(l, "DEBUG" | "TRACE"))
            .unwrap_or(false);

        if flags.debug_only {
            if is_debug {
                filtered.push((level, message));
            }
        } else if is_debug && !flags.keep_debug {
            debug_hidden += 1;
        } else {
            filtered.push((level, message));
        }
    }

    (filtered, debug_hidden)
}

/// Deduplicate entries by level-aware normalized key, incrementing count on collision.
///
/// # AD-LOG-10 (2026-04-11) — Level-aware dedup
/// The dedup key is `"{level}|{normalized_message}"` rather than just the
/// normalized message. Previously an ERROR and a WARN with the same text were
/// merged into a single entry, losing the level distinction. With level-aware
/// keys, `ERROR: foo` and `WARN: foo` remain separate entries, so agents can
/// accurately count ERROR vs WARN occurrences.
///
/// The `"-"` placeholder is used for entries with no level (e.g., unstructured
/// plain-text lines) to keep the key format consistent.
fn deduplicate_entries(entries: Vec<(Option<String>, String)>, no_dedup: bool) -> Vec<LogEntry> {
    // Issue 6: pre-size the dedup map and output vec to avoid repeated reallocation.
    let mut dedup_map: HashMap<String, usize> = HashMap::with_capacity(1024);
    let mut output_entries: Vec<LogEntry> = Vec::with_capacity(256);

    // Scratch buffer reused across iterations: zero allocations on dedup-hit,
    // one allocation on dedup-miss (the key clone for the map insert).
    let mut key_buf = String::with_capacity(128);

    for (level, message) in entries {
        // AD-LOG-10: key includes level so ERROR+WARN with same text are NOT merged.
        // Build the key in-place to avoid the two-allocation format!() pattern
        // (to_lowercase() intermediate + format! output).
        key_buf.clear();
        key_buf.push_str(level.as_deref().unwrap_or("-"));
        key_buf.push('|');
        for c in message.chars() {
            for lc in c.to_lowercase() {
                key_buf.push(lc);
            }
        }

        if no_dedup {
            // Issue 3: `level` and `message` are owned — no clone needed.
            output_entries.push(LogEntry {
                level,
                message,
                count: 1,
            });
        } else if let Some(&idx) = dedup_map.get(key_buf.as_str()) {
            output_entries[idx].count += 1;
        } else {
            let idx = output_entries.len();
            dedup_map.insert(key_buf.clone(), idx);
            // Issue 3: `level` and `message` are owned — no clone needed.
            output_entries.push(LogEntry {
                level,
                message,
                count: 1,
            });
        }
    }

    output_entries
}

/// Assemble the final LogResult from deduplicated entries and counters.
fn build_log_result(
    output_entries: Vec<LogEntry>,
    total_lines: usize,
    debug_hidden: usize,
    debug_only: bool,
    stack_frames_elided: usize,
) -> LogResult {
    let unique_messages = output_entries.len();
    let deduplicated_count = total_lines
        .saturating_sub(unique_messages)
        .saturating_sub(debug_hidden);

    LogResult::new_with_stack(
        total_lines,
        unique_messages,
        debug_hidden,
        deduplicated_count,
        output_entries,
        debug_only,
        stack_frames_elided,
    )
}

fn apply_compression(
    all_entries: Vec<(Option<String>, String)>,
    total_lines: usize,
    stack_frames_elided: usize,
    flags: &LogFlags,
) -> LogResult {
    let (filtered, debug_hidden) = filter_debug_entries(all_entries, flags);
    let output_entries = deduplicate_entries(filtered, flags.no_dedup);
    build_log_result(
        output_entries,
        total_lines,
        debug_hidden,
        flags.debug_only,
        stack_frames_elided,
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
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_flags() -> LogFlags {
        LogFlags::default()
    }

    fn load_fixture(name: &str) -> String {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/cmd/log");
        path.push(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    #[test]
    fn test_tier1_json_structured() {
        let input = load_fixture("json_structured.jsonl");
        let flags = make_flags();
        let result = try_parse_json_logs(&input, &flags);
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        assert!(result.total_lines > 0);
    }

    #[test]
    fn test_tier2_plaintext_mixed() {
        let input = load_fixture("plaintext_mixed.txt");
        let flags = make_flags();
        let result = try_parse_regex_logs(&input, &flags);
        assert!(result.is_some(), "Expected Tier 2 regex parse to succeed");
        let result = result.unwrap();
        assert!(result.total_lines > 0);
    }

    #[test]
    fn test_debug_hidden_by_default() {
        let input = load_fixture("debug_heavy.txt");
        let flags = make_flags(); // keep_debug = false
        let result = try_parse_regex_logs(&input, &flags).unwrap();
        assert!(result.debug_hidden > 0, "Expected DEBUG lines to be hidden");
    }

    #[test]
    fn test_debug_kept_with_keep_debug() {
        let input = load_fixture("debug_heavy.txt");
        let flags = LogFlags {
            keep_debug: true,
            ..Default::default()
        };
        let result = try_parse_regex_logs(&input, &flags).unwrap();
        assert_eq!(
            result.debug_hidden, 0,
            "Expected no DEBUG lines hidden with --keep-debug"
        );
    }

    #[test]
    fn test_dedup_reduces_entries() {
        let input = load_fixture("duplicate_heavy.txt");
        let flags = make_flags();
        let result = try_parse_regex_logs(&input, &flags).unwrap();
        assert!(
            result.unique_messages < result.total_lines,
            "Expected dedup to reduce entry count: {} unique vs {} total",
            result.unique_messages,
            result.total_lines
        );
    }

    #[test]
    fn test_no_dedup_flag() {
        let input = "INFO: hello\nINFO: hello\nINFO: hello\n";
        let flags = LogFlags {
            no_dedup: true,
            ..Default::default()
        };
        let result = try_parse_regex_logs(input, &flags).unwrap();
        assert_eq!(
            result.unique_messages, 3,
            "With --no-dedup, all entries kept"
        );
    }

    #[test]
    fn test_debug_only_flag() {
        let input = "INFO: normal line\nDEBUG: debug line\nERROR: error\n";
        let flags = LogFlags {
            debug_only: true,
            ..Default::default()
        };
        let result = try_parse_regex_logs(input, &flags).unwrap();
        assert!(
            result
                .entries
                .iter()
                .all(|e| { e.level.as_deref() == Some("DEBUG") }),
            "With --debug-only, only DEBUG entries should appear"
        );
    }

    #[test]
    fn test_compress_log_json_produces_full() {
        let input = load_fixture("json_structured.jsonl");
        let flags = make_flags();
        let result = compress_log(&input, &flags);
        assert!(
            result.is_full(),
            "JSON log should produce Full tier, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_compress_log_plaintext_produces_degraded() {
        let input = load_fixture("plaintext_mixed.txt");
        let flags = make_flags();
        let result = compress_log(&input, &flags);
        assert!(
            result.is_degraded(),
            "Plaintext log should produce Degraded tier, got {}",
            result.tier_name()
        );
    }

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
    fn test_extract_json_level_variants() {
        let obj: Value = serde_json::from_str(r#"{"level": "info", "msg": "test"}"#).unwrap();
        let level = extract_json_level(&obj);
        assert_eq!(level.as_deref(), Some("INFO"));

        let obj2: Value =
            serde_json::from_str(r#"{"severity": "warn", "message": "test"}"#).unwrap();
        let level2 = extract_json_level(&obj2);
        assert_eq!(level2.as_deref(), Some("WARN"));
    }

    #[test]
    fn test_stack_trace_lines_attached_to_entry() {
        // AD-LOG-10: stack trace lines are now attached to the preceding entry, not skipped.
        let input = "ERROR: something failed\n    at main() line 5\n    at run() line 10\nINFO: continuing\n";
        let flags = make_flags();
        let result = try_parse_regex_logs(input, &flags).unwrap();
        // The error entry should have both stack frames appended (only 2 frames, none elided).
        let error_entry = result
            .entries
            .iter()
            .find(|e| e.level.as_deref() == Some("ERROR"))
            .expect("ERROR entry must exist");
        assert!(
            error_entry.message.contains("at main()"),
            "Stack frame should be attached to error entry: {}",
            error_entry.message
        );
        assert!(
            error_entry.message.contains("at run()"),
            "Second stack frame should be attached: {}",
            error_entry.message
        );
        // INFO entry should not contain stack frames
        let info_entry = result
            .entries
            .iter()
            .find(|e| e.level.as_deref() == Some("INFO"))
            .expect("INFO entry must exist");
        assert!(
            !info_entry.message.contains("at main()"),
            "Stack frames must not bleed into INFO entry: {}",
            info_entry.message
        );
    }

    /// AD-LOG-10: ERROR and WARN entries with the same text must NOT merge.
    #[test]
    fn test_log_dedup_preserves_level() {
        let input = load_fixture("dedup_error_warn.txt");
        let flags = make_flags();
        let result = try_parse_regex_logs(&input, &flags).unwrap();
        // ERROR+ERROR dedup'd to 1 ERROR(×2), WARN+WARN dedup'd to 1 WARN(×2),
        // INFO stays as 1 INFO(×1) — 3 distinct entries total.
        assert_eq!(
            result.entries.len(),
            3,
            "ERROR and WARN with same message must remain separate: {:?}",
            result
                .entries
                .iter()
                .map(|e| (&e.level, &e.message, e.count))
                .collect::<Vec<_>>()
        );
        let error = result
            .entries
            .iter()
            .find(|e| e.level.as_deref() == Some("ERROR"))
            .unwrap();
        assert_eq!(error.count, 2, "ERROR must appear ×2");
        let warn = result
            .entries
            .iter()
            .find(|e| e.level.as_deref() == Some("WARN"))
            .unwrap();
        assert_eq!(warn.count, 2, "WARN must appear ×2");
    }

    /// AD-LOG-10: 5-frame Java trace → last 3 kept, 2 elided.
    #[test]
    fn test_log_stack_elision_keeps_last_3() {
        let input = load_fixture("stack_trace_java.txt");
        let flags = make_flags();
        let result = try_parse_regex_logs(&input, &flags).unwrap();
        let error_entry = result
            .entries
            .iter()
            .find(|e| e.level.as_deref() == Some("ERROR"))
            .expect("ERROR entry must exist");
        // Last 3 of the 5 frames must be present.
        assert!(
            error_entry.message.contains("OrderService.run"),
            "3rd-from-last frame must be kept: {}",
            error_entry.message
        );
        assert!(
            error_entry.message.contains("Main.main"),
            "Last frame must be kept: {}",
            error_entry.message
        );
        // First 2 frames (process, handle) must be absent.
        assert!(
            !error_entry.message.contains("OrderService.process"),
            "Elided frame must not appear: {}",
            error_entry.message
        );
    }

    /// AD-LOG-10: elided count renders in the footer when non-zero.
    #[test]
    fn test_log_stack_elision_footer() {
        let input = load_fixture("stack_trace_java.txt");
        let flags = make_flags();
        let result = try_parse_regex_logs(&input, &flags).unwrap();
        // 5 frames total, 3 kept → 2 elided.
        assert_eq!(result.stack_frames_elided, 2, "Should elide 2 of 5 frames");
        let display = result.as_ref();
        assert!(
            display.contains("+2 stack frames elided"),
            "Footer must appear when frames are elided: {display}"
        );
    }

    /// AD-LOG-10: Python `File "..."` stack traces are recognised.
    #[test]
    fn test_log_python_traceback_recognised() {
        let input = load_fixture("stack_trace_python.txt");
        let flags = make_flags();
        let result = try_parse_regex_logs(&input, &flags).unwrap();
        let error_entry = result
            .entries
            .iter()
            .find(|e| e.level.as_deref() == Some("ERROR"))
            .expect("ERROR entry must exist");
        // Python traces use `File "…"` — must be attached and last-3 kept.
        assert!(
            error_entry.message.contains("threading.py"),
            "Last Python frame must be kept: {}",
            error_entry.message
        );
        // 4 frames total, 3 kept → 1 elided.
        assert_eq!(
            result.stack_frames_elided, 1,
            "Should elide 1 of 4 Python frames"
        );
    }

    #[test]
    fn test_keep_timestamps_strips_by_default() {
        // With keep_timestamps=false (default) on TIMESTAMP [LEVEL] message format:
        // the timestamp prefix is stripped before level detection, so entries should
        // not contain timestamp text.
        let input =
            "2024-01-15T10:30:00Z [INFO] server started\n2024-01-15T10:30:01Z [INFO] ready\n";
        let flags_strip = make_flags();
        let result_strip = try_parse_regex_logs(input, &flags_strip).unwrap();
        assert!(
            result_strip
                .entries
                .iter()
                .all(|e| !e.message.contains("2024-01-15")),
            "Default should strip timestamps from messages"
        );
    }

    #[test]
    fn test_keep_timestamps_passthrough_preserves_raw() {
        // With keep_timestamps=true on TIMESTAMP [LEVEL] message format: the regex
        // parser cannot detect log levels (anchored at ^, timestamp comes first), so
        // try_parse_regex_logs returns None and compress_log falls through to Passthrough.
        // Passthrough preserves the raw input verbatim, including timestamps.
        let input =
            "2024-01-15T10:30:00Z [INFO] server started\n2024-01-15T10:30:01Z [INFO] ready\n";
        let flags_keep = LogFlags {
            keep_timestamps: true,
            ..LogFlags::default()
        };
        let result = compress_log(input, &flags_keep);
        // Tier 2 cannot detect structure when timestamps block the ^ anchor, so
        // output falls to Passthrough — raw content is preserved including timestamps.
        assert!(
            result.is_passthrough(),
            "TIMESTAMP [LEVEL] format with keep_timestamps falls to Passthrough (level detection blocked by timestamp prefix)"
        );
        assert!(
            result.content().contains("2024-01-15"),
            "Passthrough should preserve raw content including timestamps"
        );
    }

    #[test]
    fn test_plain_text_without_levels_returns_passthrough() {
        // Issue 8: plain text with no log levels should fall through to Passthrough,
        // not produce a misleading Degraded result.
        let input = "some plain text\nanother line\nno levels here\n";
        let flags = make_flags();
        let result = compress_log(input, &flags);
        assert!(
            result.is_passthrough(),
            "Plain text without log levels should produce Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_unknown_flag_warning_does_not_panic() {
        // Issue 7: unknown flags should warn to stderr but not crash.
        let args: Vec<String> = vec!["--unknown-flag".into(), "--keep-debug".into()];
        let flags = parse_flags(&args);
        // Known flag still parsed correctly despite unknown neighbor
        assert!(flags.keep_debug);
    }

    #[test]
    fn test_filter_debug_entries_debug_only() {
        let entries = vec![
            (Some("INFO".to_string()), "info msg".to_string()),
            (Some("DEBUG".to_string()), "debug msg".to_string()),
            (Some("TRACE".to_string()), "trace msg".to_string()),
            (Some("ERROR".to_string()), "error msg".to_string()),
        ];
        let flags = LogFlags {
            debug_only: true,
            ..Default::default()
        };
        let (filtered, hidden) = filter_debug_entries(entries, &flags);
        assert_eq!(hidden, 0);
        assert_eq!(filtered.len(), 2);
        assert!(filtered
            .iter()
            .all(|(l, _)| { matches!(l.as_deref(), Some("DEBUG") | Some("TRACE")) }));
    }

    #[test]
    fn test_filter_debug_entries_hidden_by_default() {
        let entries = vec![
            (Some("INFO".to_string()), "info msg".to_string()),
            (Some("DEBUG".to_string()), "debug msg".to_string()),
            (Some("TRACE".to_string()), "trace msg".to_string()),
        ];
        let flags = LogFlags::default(); // keep_debug = false
        let (filtered, hidden) = filter_debug_entries(entries, &flags);
        assert_eq!(hidden, 2);
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn test_deduplicate_entries_counts_duplicates() {
        let entries = vec![
            (Some("INFO".to_string()), "hello".to_string()),
            (Some("INFO".to_string()), "hello".to_string()),
            (Some("INFO".to_string()), "world".to_string()),
        ];
        let output = deduplicate_entries(entries, false);
        assert_eq!(output.len(), 2);
        let hello = output.iter().find(|e| e.message == "hello").unwrap();
        assert_eq!(hello.count, 2);
    }

    /// Regression test for the unbounded `pending_stack` growth fix (batch-3-log).
    ///
    /// Feeds an ERROR line followed by 20 stack-trace frames. Before the fix,
    /// all 20 frames would be accumulated in `pending_stack` before
    /// `flush_stack_frames` discarded 17 of them — wasting ~17× the necessary
    /// allocation. After the fix, `pending_stack` never exceeds PENDING_STACK_CAP
    /// frames; the elision is counted incrementally as frames arrive.
    ///
    /// Invariants verified:
    /// 1. `stack_frames_elided` == 20 - 3 == 17 (last 3 kept, rest elided).
    /// 2. The rendered output contains the elision summary line.
    /// 3. None of the elided frames appear in the rendered output.
    #[test]
    fn test_pending_stack_cap_elides_excess_frames() {
        // Build input: one ERROR line + 20 stack frames + a follow-up INFO line
        // to trigger the final flush.
        let mut input = String::from("ERROR: something went wrong\n");
        for i in 1..=20 {
            input.push_str(&format!(
                "    at com.example.Service.frame{i}(Service.java:{i})\n"
            ));
        }
        input.push_str("INFO: recovered\n");

        let flags = make_flags();
        let result = try_parse_regex_logs(&input, &flags).unwrap();

        // 20 frames total, last 3 kept → 17 elided.
        assert_eq!(
            result.stack_frames_elided, 17,
            "Expected 17 elided frames (20 total, last 3 kept), got {}",
            result.stack_frames_elided
        );

        let display = result.as_ref();

        // Rendered output must surface the elision count.
        assert!(
            display.contains("+17 stack frames elided"),
            "Output must contain elision summary; got: {display}"
        );

        // Frame 1 must have been dropped by the cap. Match on the exact frame
        // name to avoid substring collisions with "frame10"–"frame17".
        assert!(
            !display.contains("frame1(Service.java:1)"),
            "Elided frame1 must not appear in output; got: {display}"
        );

        // The last 3 frames (18, 19, 20) must be present.
        assert!(
            display.contains("frame18"),
            "frame18 (3rd-from-last) must be kept; got: {display}"
        );
        assert!(
            display.contains("frame20"),
            "frame20 (last) must be kept; got: {display}"
        );
    }

    /// Regression test: extract_json_message truncates oversized fields.
    ///
    /// A pathological JSON line with a message field larger than MAX_JSON_MSG_LEN
    /// must be truncated to MAX_JSON_MSG_LEN chars and suffixed with "[truncated]".
    #[test]
    fn test_extract_json_message_truncates_large_field() {
        let large_msg = "A".repeat(MAX_JSON_MSG_LEN + 100);
        let obj = serde_json::json!({ "msg": large_msg });
        let result = extract_json_message(&obj).unwrap();
        assert!(
            result.ends_with("[truncated]"),
            "Oversized message must end with [truncated]; got len={}",
            result.len()
        );
        // The result must not exceed MAX_JSON_MSG_LEN chars + len("[truncated]").
        assert!(
            result.chars().count() <= MAX_JSON_MSG_LEN + 11,
            "Truncated message must not exceed bound; got len={}",
            result.len()
        );
    }

    /// Regression test: extract_json_level truncates oversized level fields.
    #[test]
    fn test_extract_json_level_truncates_large_field() {
        let large_level = "X".repeat(MAX_JSON_LEVEL_LEN + 50);
        let obj = serde_json::json!({ "level": large_level });
        let result = extract_json_level(&obj).unwrap();
        assert_eq!(
            result.chars().count(),
            MAX_JSON_LEVEL_LEN,
            "Level must be truncated to MAX_JSON_LEVEL_LEN chars"
        );
    }
}
