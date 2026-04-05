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
use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::output::canonical::{LogEntry, LogResult};
use crate::output::ParseResult;

/// Maximum input lines before truncation.
const MAX_INPUT_LINES: usize = 100_000;

/// Matches ISO8601 / common log timestamp prefix to strip before dedup.
/// e.g. `2024-01-15T10:30:00Z `, `2024-01-15 10:30:00 `, `[2024-01-15T10:30:00]`
static RE_TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[?\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?\]?\s*")
        .unwrap()
});

/// Matches bracket-style level: `[ERROR]`, `[INFO]`, etc.
static RE_LEVEL_BRACKET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(?i)(ERROR|WARN|WARNING|INFO|DEBUG|TRACE)\]\s*(.*)").unwrap());

/// Matches bare-level format: `ERROR message` or `ERROR: message`
static RE_LEVEL_BARE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?i)(ERROR|WARN|WARNING|INFO|DEBUG|TRACE):?\s+(.*)").unwrap());

/// Matches Java/Node.js stack trace lines.
static RE_STACK_TRACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+at\s+").unwrap());

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
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let flags = parse_flags(args);

    // Read stdin — log is stdin-only
    if io::stdin().is_terminal() && args.is_empty() {
        eprintln!("error: 'skim log' reads from stdin. Pipe log output to it:");
        eprintln!("  kubectl logs deploy/foo | skim log");
        eprintln!("  cat app.log | skim log");
        return Ok(ExitCode::FAILURE);
    }

    const MAX_STDIN_BYTES: u64 = 64 * 1024 * 1024;
    let mut stdin_buf = String::new();
    let bytes_read = io::stdin()
        .take(MAX_STDIN_BYTES)
        .read_to_string(&mut stdin_buf)?;
    if bytes_read as u64 >= MAX_STDIN_BYTES {
        anyhow::bail!("stdin input exceeded 64 MiB limit");
    }

    let raw_input = stdin_buf.as_str();
    let result = compress_log(raw_input, &flags);

    // Emit output
    let compressed = emit_result(&result, &flags)?;

    if flags.show_stats {
        let (orig, comp) = crate::process::count_token_pair(raw_input, &compressed);
        crate::process::report_token_stats(orig, comp, "");
    }

    // Record analytics (fire-and-forget)
    if crate::analytics::is_analytics_enabled() {
        crate::analytics::try_record_command(
            raw_input.to_string(),
            compressed,
            "skim log".to_string(),
            crate::analytics::CommandType::Log,
            std::time::Duration::ZERO,
            Some(result.tier_name()),
        );
    }

    Ok(ExitCode::SUCCESS)
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
        return ParseResult::Degraded(result, vec!["regex fallback".to_string()]);
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

    let mut all_entries: Vec<(Option<String>, String)> = Vec::new();
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

    Some(apply_compression(all_entries, total_lines, flags))
}

fn extract_json_level(obj: &Value) -> Option<String> {
    for key in &["level", "severity", "lvl", "log_level"] {
        if let Some(v) = obj.get(key).and_then(|v| v.as_str()) {
            return Some(v.to_uppercase());
        }
    }
    None
}

fn extract_json_message(obj: &Value) -> Option<String> {
    for key in &["msg", "message", "text", "body"] {
        if let Some(v) = obj.get(key).and_then(|v| v.as_str()) {
            return Some(v.to_string());
        }
    }
    None
}

// ============================================================================
// Tier 2: regex-based log formats
// ============================================================================

fn try_parse_regex_logs(input: &str, flags: &LogFlags) -> Option<LogResult> {
    let mut all_entries: Vec<(Option<String>, String)> = Vec::new();
    let mut total_lines = 0usize;
    let mut found_structured = false;

    for line in input.lines().take(MAX_INPUT_LINES) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Skip stack trace lines — check original line (preserves leading whitespace).
        // Count these separately so they don't inflate the dedup statistics.
        if RE_STACK_TRACE.is_match(line) {
            continue;
        }
        total_lines += 1;

        // Strip timestamp prefix
        let without_ts = if flags.keep_timestamps {
            trimmed
        } else {
            RE_TIMESTAMP.find(trimmed)
                .map(|m| &trimmed[m.end()..])
                .unwrap_or(trimmed)
        };

        if let Some(caps) = RE_LEVEL_BRACKET.captures(without_ts) {
            let level = caps[1].to_uppercase();
            let message = caps[2].trim().to_string();
            all_entries.push((Some(level), message));
            found_structured = true;
        } else if let Some(caps) = RE_LEVEL_BARE.captures(without_ts) {
            let level = caps[1].to_uppercase();
            let message = caps[2].trim().to_string();
            all_entries.push((Some(level), message));
            found_structured = true;
        } else {
            all_entries.push((None, without_ts.to_string()));
        }
    }

    if !found_structured && all_entries.is_empty() {
        return None;
    }

    Some(apply_compression(all_entries, total_lines, flags))
}

// ============================================================================
// Shared compression logic
// ============================================================================

fn apply_compression(
    all_entries: Vec<(Option<String>, String)>,
    total_lines: usize,
    flags: &LogFlags,
) -> LogResult {
    let mut debug_hidden = 0usize;
    // HashMap<normalized_message, index in output_entries>
    let mut dedup_map: HashMap<String, usize> = HashMap::new();
    let mut output_entries: Vec<LogEntry> = Vec::new();

    for (level, message) in all_entries {
        let is_debug = level
            .as_deref()
            .map(|l| matches!(l, "DEBUG" | "TRACE"))
            .unwrap_or(false);

        // Filter by mode
        if flags.debug_only {
            if !is_debug {
                continue;
            }
        } else if is_debug && !flags.keep_debug {
            debug_hidden += 1;
            continue;
        }

        let normalized = message.to_lowercase();

        if flags.no_dedup {
            output_entries.push(LogEntry {
                level: level.clone(),
                message: message.clone(),
                count: 1,
            });
        } else if let Some(&idx) = dedup_map.get(&normalized) {
            output_entries[idx].count += 1;
        } else {
            let idx = output_entries.len();
            dedup_map.insert(normalized, idx);
            output_entries.push(LogEntry {
                level: level.clone(),
                message: message.clone(),
                count: 1,
            });
        }
    }

    let unique_messages = output_entries.len();
    let deduplicated_count = total_lines
        .saturating_sub(unique_messages)
        .saturating_sub(debug_hidden);

    LogResult::new(
        total_lines,
        unique_messages,
        debug_hidden,
        deduplicated_count,
        output_entries,
        flags.debug_only,
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
        assert_eq!(result.debug_hidden, 0, "Expected no DEBUG lines hidden with --keep-debug");
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
        assert_eq!(result.unique_messages, 3, "With --no-dedup, all entries kept");
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
            result.entries.iter().all(|e| {
                e.level.as_deref() == Some("DEBUG")
            }),
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

        let obj2: Value = serde_json::from_str(r#"{"severity": "warn", "message": "test"}"#).unwrap();
        let level2 = extract_json_level(&obj2);
        assert_eq!(level2.as_deref(), Some("WARN"));
    }

    #[test]
    fn test_stack_trace_lines_skipped() {
        let input = "ERROR: something failed\n    at main() line 5\n    at run() line 10\nINFO: continuing\n";
        let flags = make_flags();
        let result = try_parse_regex_logs(input, &flags).unwrap();
        // Stack trace lines should be skipped
        assert!(
            result.entries.iter().all(|e| !e.message.contains("at main()")),
            "Stack trace lines should not appear in entries"
        );
    }
}
