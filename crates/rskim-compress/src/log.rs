//! Log compression module — `compress_log` and supporting types.
//!
//! # R1 — Host in rskim-compress, NOT rskim-core (AC26 / #327)
//!
//! The 304-plan §2 originally said "move `compress_log` into rskim-core."
//! That VIOLATES AC26: `rskim-core` is the pure AST transform library and
//! MUST NOT gain `regex` as a dependency (verified: rskim-core/Cargo.toml has
//! zero regex refs today). The `rskim-compress` crate is allowed to depend on
//! `regex`, so `compress_log`, `LogFlags`, `LogResult`, `LogEntry`, and
//! `ParseResult` are hosted HERE instead.
//!
//! The `rskim` binary's `cmd/log.rs` is re-pointed to call
//! `rskim_compress::log::compress_log` (AC25 — no behavior change; existing log
//! tests must stay green before and after). This deviation is documented citing
//! AC26 + #327 (log-rule library extraction ticket).
//!
//! # Three-tier parse result
//!
//! - **Full**: clean structured-JSON parse
//! - **Degraded**: regex-based parse (partial structure)
//! - **Passthrough**: raw log output forwarded unchanged
//!
//! # Three-tier compression pipeline
//!
//! 1. `try_parse_json_logs` — JSON log lines (structured)
//! 2. `try_parse_regex_logs` — timestamp + level regex patterns
//! 3. Passthrough — no structure detected

use std::collections::{HashMap, VecDeque};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ============================================================================
// Public types (ParseResult, LogEntry, LogResult, LogFlags)
// ============================================================================

/// Result of parsing external process output through three degradation tiers.
///
/// - `Full`: clean parse, no issues
/// - `Degraded`: partially parsed with warning markers
/// - `Passthrough`: unparseable, returned as-is (always `String`)
///
/// This type is the public re-export from rskim-compress so the `rskim` binary
/// can import `rskim_compress::log::ParseResult` instead of the private
/// `crate::output::ParseResult`. Behavior is identical (R1 / AC25).
#[derive(Debug, Clone)]
pub enum ParseResult<T> {
    /// Clean parse — fully structured output.
    Full(T),
    /// Partially parsed with warning markers.
    Degraded(T, Vec<String>),
    /// Unparseable — content returned as-is.
    Passthrough(String),
}

impl<T> ParseResult<T> {
    /// Returns `true` if this is a `Full` result.
    pub fn is_full(&self) -> bool {
        matches!(self, ParseResult::Full(_))
    }

    /// Returns `true` if this is a `Degraded` result.
    pub fn is_degraded(&self) -> bool {
        matches!(self, ParseResult::Degraded(_, _))
    }

    /// Returns `true` if this is a `Passthrough` result.
    pub fn is_passthrough(&self) -> bool {
        matches!(self, ParseResult::Passthrough(_))
    }

    /// Returns the tier name as a static string.
    pub fn tier_name(&self) -> &'static str {
        match self {
            ParseResult::Full(_) => "full",
            ParseResult::Degraded(_, _) => "degraded",
            ParseResult::Passthrough(_) => "passthrough",
        }
    }
}

impl<T: AsRef<str>> ParseResult<T> {
    /// Read access to inner content for all tiers.
    pub fn content(&self) -> &str {
        match self {
            ParseResult::Full(inner) | ParseResult::Degraded(inner, _) => inner.as_ref(),
            ParseResult::Passthrough(s) => s.as_str(),
        }
    }
}

impl<T: serde::Serialize> ParseResult<T> {
    /// Serialize as a JSON envelope (used by `--json` output in the rskim binary).
    pub fn to_json_envelope(&self) -> serde_json::Result<String> {
        match self {
            ParseResult::Full(inner) => serde_json::to_string(inner),
            ParseResult::Degraded(inner, warnings) => {
                let val = serde_json::json!({
                    "tier": "degraded",
                    "warnings": warnings,
                    "result": inner,
                });
                serde_json::to_string(&val)
            }
            ParseResult::Passthrough(raw) => {
                let val = serde_json::json!({
                    "tier": "passthrough",
                    "raw": raw,
                });
                serde_json::to_string(&val)
            }
        }
    }
}

impl<T: Into<String>> ParseResult<T> {
    /// Consuming access to inner content as `String`.
    pub fn into_content(self) -> String {
        match self {
            ParseResult::Full(inner) | ParseResult::Degraded(inner, _) => inner.into(),
            ParseResult::Passthrough(s) => s,
        }
    }
}

/// A single log entry with optional level and deduplication count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Log level (e.g., "ERROR", "WARN", "INFO"), if detected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// The log message (timestamp-stripped, deduplicated).
    pub message: String,
    /// How many times this message appeared in the input.
    pub count: usize,
}

/// Result of log compression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogResult {
    /// Total number of non-blank input lines processed.
    pub total_lines: usize,
    /// Number of unique messages after deduplication.
    pub unique_messages: usize,
    /// Number of DEBUG/TRACE lines hidden (unless `--keep-debug`).
    pub debug_hidden: usize,
    /// Number of duplicate messages removed.
    pub deduplicated_count: usize,
    /// Compressed log entries.
    pub entries: Vec<LogEntry>,
    /// True when `--debug-only` mode was requested.
    #[serde(default)]
    pub debug_only: bool,
    /// Number of stack frames elided from all captured traces (last 3 per trace kept).
    ///
    /// # AD-LOG-10 (2026-04-11)
    /// When non-zero, a `(+{n} frames elided)` footer is appended to the text
    /// render so agents know stack traces were truncated.
    #[serde(default)]
    pub stack_frames_elided: usize,
    /// Pre-rendered display string (not serialized; recomputed after deserialization).
    #[serde(default, skip_serializing)]
    rendered: String,
}

impl LogResult {
    /// Create a new `LogResult` with pre-computed rendered output.
    pub fn new(
        total_lines: usize,
        unique_messages: usize,
        debug_hidden: usize,
        deduplicated_count: usize,
        entries: Vec<LogEntry>,
        debug_only: bool,
    ) -> Self {
        Self::new_with_stack(
            total_lines,
            unique_messages,
            debug_hidden,
            deduplicated_count,
            entries,
            debug_only,
            0,
        )
    }

    /// Create a new `LogResult` with stack frame elision count.
    pub fn new_with_stack(
        total_lines: usize,
        unique_messages: usize,
        debug_hidden: usize,
        deduplicated_count: usize,
        entries: Vec<LogEntry>,
        debug_only: bool,
        stack_frames_elided: usize,
    ) -> Self {
        let rendered = Self::render(
            total_lines,
            unique_messages,
            debug_hidden,
            deduplicated_count,
            &entries,
            debug_only,
            stack_frames_elided,
        );
        Self {
            total_lines,
            unique_messages,
            debug_hidden,
            deduplicated_count,
            entries,
            debug_only,
            stack_frames_elided,
            rendered,
        }
    }

    /// Recompute rendered field if empty (e.g., after deserialization).
    pub fn ensure_rendered(&mut self) {
        if self.rendered.is_empty() {
            self.rendered = Self::render(
                self.total_lines,
                self.unique_messages,
                self.debug_hidden,
                self.deduplicated_count,
                &self.entries,
                self.debug_only,
                self.stack_frames_elided,
            );
        }
    }

    fn render(
        total_lines: usize,
        unique_messages: usize,
        debug_hidden: usize,
        deduplicated_count: usize,
        entries: &[LogEntry],
        debug_only: bool,
        stack_frames_elided: usize,
    ) -> String {
        use std::fmt::Write as FmtWrite;

        let mut output = if debug_only {
            format!("debug: {debug_hidden} lines")
        } else {
            format!(
                "{total_lines} lines \u{2192} {unique_messages} unique ({deduplicated_count} duplicates removed)"
            )
        };

        if !debug_only && debug_hidden > 0 {
            let _ = write!(
                output,
                "\n{debug_hidden} debug lines hidden (skim log --debug-only)"
            );
        }

        for entry in entries {
            match &entry.level {
                Some(level) => {
                    if entry.count > 1 {
                        let _ = write!(
                            output,
                            "\n {level}: {} (\u{d7}{})",
                            entry.message, entry.count
                        );
                    } else {
                        let _ = write!(output, "\n {level}: {}", entry.message);
                    }
                }
                None => {
                    if entry.count > 1 {
                        let _ = write!(output, "\n {} (\u{d7}{})", entry.message, entry.count);
                    } else {
                        let _ = write!(output, "\n {}", entry.message);
                    }
                }
            }
        }

        // AD-LOG-10: append elision footer when frames were truncated.
        if stack_frames_elided > 0 {
            let _ = write!(output, "\n(+{stack_frames_elided} frames elided)");
        }

        output
    }
}

impl AsRef<str> for LogResult {
    fn as_ref(&self) -> &str {
        &self.rendered
    }
}

impl std::fmt::Display for LogResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.rendered)
    }
}

/// Flags controlling log compression behavior.
///
/// Parsed from CLI args by the rskim binary's `cmd/log.rs`.
#[derive(Debug, Default)]
pub struct LogFlags {
    /// Disable message deduplication.
    pub no_dedup: bool,
    /// Preserve timestamp prefixes (default: strip).
    pub keep_timestamps: bool,
    /// Show all levels including DEBUG/TRACE.
    pub keep_debug: bool,
    /// Show ONLY DEBUG/TRACE lines.
    pub debug_only: bool,
    /// Show token statistics after compression.
    pub show_stats: bool,
    /// Emit structured JSON output.
    pub json_output: bool,
}

// ============================================================================
// Internal constants
// ============================================================================

/// Maximum input lines before truncation.
const MAX_INPUT_LINES: usize = 100_000;

/// Maximum logical frames buffered in `pending_stack` before the oldest is dropped.
///
/// Keeps memory at O(PENDING_STACK_CAP) regardless of input length.
/// `flush_stack_frames` retains only the last 3 frames.
const PENDING_STACK_CAP: usize = 4;

/// Maximum continuation lines (source-preview / PEP 657 caret) appended to a
/// single logical frame.
const MAX_CONTINUATIONS_PER_FRAME: usize = 4;

/// Maximum byte length for a JSON log-level field.
const MAX_JSON_LEVEL_LEN: usize = 32;

/// Maximum byte length for a JSON log-message field.
const MAX_JSON_MSG_LEN: usize = 16 * 1024;

// ============================================================================
// Static regex patterns
// ============================================================================

/// Matches ISO8601 / common log timestamp prefix to strip before dedup.
///
/// # Safety
///
/// `Regex::new` is called with a statically known, valid pattern.
/// The `#[allow(clippy::expect_used)]` suppression is intentional: LazyLock
/// initialization cannot return a `Result`, and a pattern compile failure here
/// is a programming error (not a runtime error), making `expect` the correct
/// tool. The pattern is validated by the unit tests at crate load time.
#[allow(clippy::expect_used)]
static RE_LOG_TIMESTAMP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\[?\d{4}-\d{2}-\d{2}[T ]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?(?:Z|[+-]\d{2}:?\d{2})?\]?\s*",
    )
    .expect("static regex RE_LOG_TIMESTAMP is valid")
});

/// Matches bracket-style level: `[ERROR]`, `[INFO]`, etc.
#[allow(clippy::expect_used)]
static RE_LOG_LEVEL_BRACKET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[(?i)(ERROR|WARN|WARNING|INFO|DEBUG|TRACE)\]\s*(.*)")
        .expect("static regex RE_LOG_LEVEL_BRACKET is valid")
});

/// Matches bare-level format: `ERROR message` or `ERROR: message`.
#[allow(clippy::expect_used)]
static RE_LOG_LEVEL_BARE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?i)(ERROR|WARN|WARNING|INFO|DEBUG|TRACE):?\s+(.*)")
        .expect("static regex RE_LOG_LEVEL_BARE is valid")
});

/// Matches Java/Node.js/Python stack trace lines.
///
/// # AD-LOG-10 (2026-04-11) — Multi-language stack trace patterns
/// - Java/Node.js: `    at <method>` (leading whitespace + "at ")
/// - Python: `  File "...", line N` (leading whitespace + 'File "')
#[allow(clippy::expect_used)]
static RE_LOG_STACK_TRACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:^\s+at\s+|^\s+File\s+")"#).expect("static regex RE_LOG_STACK_TRACE is valid")
});

// ============================================================================
// Internal types
// ============================================================================

/// Tracks whether the parser is inside a Python `File "..."` logical frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameContext {
    /// No active Python frame.
    Idle,
    /// Inside a Python `File "..."` logical frame.
    PythonFrame {
        /// Number of continuation lines already appended to the current frame.
        continuation_count: usize,
    },
}

// ============================================================================
// Public entry point
// ============================================================================

/// Compress log lines into a structured `ParseResult<LogResult>`.
///
/// Three-tier pipeline:
/// 1. JSON structured logs → `ParseResult::Full`
/// 2. Regex pattern logs → `ParseResult::Degraded`
/// 3. No structure detected → `ParseResult::Passthrough`
///
/// # AC25 — No regression
///
/// Behaviour is identical to the original `cmd/log.rs::compress_log` in the
/// rskim binary. The rskim binary's handler is re-pointed here (R1 / #327).
pub fn compress_log(input: &str, flags: &LogFlags) -> ParseResult<LogResult> {
    if let Some(result) = try_parse_json_logs(input, flags) {
        return ParseResult::Full(result);
    }
    if let Some(result) = try_parse_regex_logs(input, flags) {
        return ParseResult::Degraded(
            result,
            vec!["log: no structured entries found, using regex".to_string()],
        );
    }
    ParseResult::Passthrough(input.to_string())
}

// ============================================================================
// Tier 1: structured JSON log lines
// ============================================================================

fn try_parse_json_logs(input: &str, flags: &LogFlags) -> Option<LogResult> {
    let first_line = input.lines().find(|l| !l.trim().is_empty())?;
    // Probe first line; bail if not JSON.
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
            all_entries.push((None, trimmed.to_string()));
            continue;
        };

        let level = extract_json_level(&obj);
        let message = extract_json_message(&obj).unwrap_or_else(|| trimmed.to_string());
        all_entries.push((level, message));
    }

    Some(apply_compression(all_entries, total_lines, 0, flags))
}

fn extract_json_level(obj: &Value) -> Option<String> {
    for key in &["level", "severity", "lvl", "log_level"] {
        if let Some(v) = obj.get(key).and_then(|v| v.as_str()) {
            if v.len() <= MAX_JSON_LEVEL_LEN {
                return Some(v.to_uppercase());
            }
            let truncated: String = v.chars().take(MAX_JSON_LEVEL_LEN).collect();
            return Some(truncated.to_uppercase());
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

fn is_python_continuation(line: &str) -> bool {
    let first = line.as_bytes().first().copied().unwrap_or(0);
    first.is_ascii_whitespace() && !line.trim().is_empty()
}

/// Parse regex-based log formats into a `LogResult`.
///
/// # AD-LOG-10 (2026-04-11) — Stack trace capture and last-3-frame elision
///
/// Stack trace lines are buffered in `pending_stack`. When the next log line
/// arrives, the accumulated frames are attached to the previous entry's message.
/// Only the last 3 frames are kept; the rest are counted in `stack_frames_elided`.
fn try_parse_regex_logs(input: &str, flags: &LogFlags) -> Option<LogResult> {
    let mut all_entries: Vec<(Option<String>, String)> = Vec::with_capacity(256);
    let mut total_lines = 0usize;
    let mut found_structured = false;
    let mut pending_stack: VecDeque<String> = VecDeque::new();
    let mut total_stack_frames_elided: usize = 0;
    let mut frame_ctx = FrameContext::Idle;

    for line in input.lines().take(MAX_INPUT_LINES) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            try_flush_stack(
                &mut all_entries,
                &mut pending_stack,
                &mut total_stack_frames_elided,
            );
            pending_stack.clear();
            frame_ctx = FrameContext::Idle;
            continue;
        }

        // Step 2: stack trace detection (original untrimmed line for leading whitespace).
        if RE_LOG_STACK_TRACE.is_match(line) {
            if pending_stack.len() >= PENDING_STACK_CAP {
                pending_stack.pop_front();
                total_stack_frames_elided += 1;
            }
            pending_stack.push_back(trimmed.to_string());
            frame_ctx = if trimmed.starts_with("File ") {
                FrameContext::PythonFrame {
                    continuation_count: 0,
                }
            } else {
                FrameContext::Idle
            };
            found_structured = true;
            continue;
        }

        // Step 3: Python source-preview / PEP 657 caret continuation.
        if let FrameContext::PythonFrame {
            ref mut continuation_count,
        } = frame_ctx
            && is_python_continuation(line)
        {
            debug_assert!(
                !pending_stack.is_empty(),
                "FrameContext::PythonFrame requires a frame in pending_stack"
            );
            if *continuation_count < MAX_CONTINUATIONS_PER_FRAME {
                if let Some(last_frame) = pending_stack.back_mut() {
                    last_frame.push('\n');
                    last_frame.push_str(trimmed);
                }
                *continuation_count += 1;
            }
            continue;
        }

        // Step 4: strip timestamp once.
        let without_ts = strip_timestamp(trimmed, flags.keep_timestamps);

        // Step 5: Traceback header.
        if without_ts.starts_with("Traceback (most recent call last)") {
            if let Some((_, msg)) = all_entries.last_mut() {
                msg.push('\n');
                msg.push_str(trimmed);
            } else {
                all_entries.push((None, trimmed.to_string()));
            }
            frame_ctx = FrameContext::Idle;
            found_structured = true;
            continue;
        }

        // Step 6: chained exception separator.
        if without_ts.starts_with("During handling of the above exception")
            || without_ts.starts_with("The above exception was the direct cause")
        {
            try_flush_stack(
                &mut all_entries,
                &mut pending_stack,
                &mut total_stack_frames_elided,
            );
            frame_ctx = FrameContext::Idle;
            all_entries.push((None, trimmed.to_string()));
            continue;
        }

        // Step 7: reset python-frame context.
        frame_ctx = FrameContext::Idle;

        // Step 8: new log line — flush stack frames onto previous entry.
        try_flush_stack(
            &mut all_entries,
            &mut pending_stack,
            &mut total_stack_frames_elided,
        );

        total_lines += 1;

        if let Some((level, message)) = classify_log_line(without_ts) {
            all_entries.push((Some(level), message));
            found_structured = true;
        } else {
            all_entries.push((None, without_ts.to_string()));
        }
    }

    // Flush trailing stack frames.
    try_flush_stack(
        &mut all_entries,
        &mut pending_stack,
        &mut total_stack_frames_elided,
    );

    if !found_structured || all_entries.is_empty() {
        return None;
    }

    Some(apply_compression(
        all_entries,
        total_lines,
        total_stack_frames_elided,
        flags,
    ))
}

#[allow(clippy::ptr_arg)]
fn flush_stack_frames(
    all_entries: &mut Vec<(Option<String>, String)>,
    pending_stack: &mut VecDeque<String>,
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

#[allow(clippy::ptr_arg)]
#[inline]
fn try_flush_stack(
    all_entries: &mut Vec<(Option<String>, String)>,
    pending_stack: &mut VecDeque<String>,
    elided: &mut usize,
) {
    if !pending_stack.is_empty() && !all_entries.is_empty() {
        flush_stack_frames(all_entries, pending_stack, elided);
    }
}

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

/// Deduplicate entries by level-aware normalized key.
///
/// # AD-LOG-10 (2026-04-11) — Level-aware dedup
///
/// The dedup key is `"{level}|{normalized_message}"`. ERROR and WARN with the
/// same text remain separate entries.
fn deduplicate_entries(entries: Vec<(Option<String>, String)>, no_dedup: bool) -> Vec<LogEntry> {
    let mut dedup_map: HashMap<String, usize> = HashMap::with_capacity(1024);
    let mut output_entries: Vec<LogEntry> = Vec::with_capacity(256);
    let mut key_buf = String::with_capacity(128);

    for (level, message) in entries {
        key_buf.clear();
        key_buf.push_str(level.as_deref().unwrap_or("-"));
        key_buf.push('|');
        for c in message.chars() {
            for lc in c.to_lowercase() {
                key_buf.push(lc);
            }
        }

        if no_dedup {
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
            output_entries.push(LogEntry {
                level,
                message,
                count: 1,
            });
        }
    }

    output_entries
}

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
// Unit tests (mirrors the tests in rskim/src/cmd/log.rs)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_flags() -> LogFlags {
        LogFlags::default()
    }

    #[test]
    fn test_compress_log_passthrough_for_plain_text() {
        let input = "some plain text\nanother line\nno levels here\n";
        let result = compress_log(input, &make_flags());
        assert!(
            result.is_passthrough(),
            "plain text without log levels → Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_compress_log_json_produces_full() {
        let input = r#"{"level":"INFO","msg":"server started"}
{"level":"ERROR","msg":"connection failed"}
"#;
        let result = compress_log(input, &make_flags());
        assert!(
            result.is_full(),
            "JSON log should produce Full tier, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_compress_log_regex_produces_degraded() {
        let input = "ERROR: something failed\nINFO: all good\n";
        let result = compress_log(input, &make_flags());
        assert!(
            result.is_degraded(),
            "Regex log should produce Degraded tier, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_compress_log_dedup_reduces_entries() {
        let input = "ERROR: conn failed\nERROR: conn failed\nINFO: started\n";
        let result = compress_log(input, &make_flags());
        assert!(!result.is_passthrough(), "should parse with log structure");
        let content = result.content();
        // "×2" indicates deduplication occurred
        assert!(
            content.contains('\u{d7}') || content.contains("×"),
            "dedup should show multiply marker: {content}"
        );
    }

    #[test]
    fn test_log_flags_default() {
        let f = LogFlags::default();
        assert!(!f.no_dedup);
        assert!(!f.keep_timestamps);
        assert!(!f.keep_debug);
        assert!(!f.debug_only);
        assert!(!f.show_stats);
        assert!(!f.json_output);
    }

    #[test]
    fn test_log_result_display() {
        let entries = vec![LogEntry {
            level: Some("ERROR".to_string()),
            message: "something failed".to_string(),
            count: 2,
        }];
        let result = LogResult::new(10, 1, 0, 9, entries, false);
        let display = result.as_ref();
        assert!(display.contains("10 lines"), "should show total: {display}");
        assert!(display.contains("×2"), "should show dedup count: {display}");
    }

    #[test]
    fn test_stack_trace_attached_to_entry() {
        let input =
            "ERROR: something failed\n    at main() line 5\n    at run() line 10\nINFO: ok\n";
        let result = compress_log(input, &make_flags());
        assert!(!result.is_passthrough());
        let content = result.content();
        assert!(
            content.contains("at main()") || content.contains("at run()"),
            "stack frames should be in output: {content}"
        );
    }

    #[test]
    fn test_parse_result_tier_names() {
        let full: ParseResult<LogResult> =
            ParseResult::Full(LogResult::new(1, 1, 0, 0, vec![], false));
        assert_eq!(full.tier_name(), "full");
        let degraded: ParseResult<LogResult> =
            ParseResult::Degraded(LogResult::new(1, 1, 0, 0, vec![], false), vec![]);
        assert_eq!(degraded.tier_name(), "degraded");
        let pt: ParseResult<String> = ParseResult::Passthrough("raw".to_string());
        assert_eq!(pt.tier_name(), "passthrough");
    }
}
