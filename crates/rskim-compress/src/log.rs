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
// Unit tests
//
// Comprehensive coverage ported from rskim/src/cmd/log.rs (the original location
// before #327 / R1 extraction). These tests exercise the canonical implementation
// that lives here. The rskim binary's cmd/log.rs retains only CLI-glue and
// delegation smoke tests.
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::path::Component;

    fn make_flags() -> LogFlags {
        LogFlags::default()
    }

    /// Load a test fixture from `tests/fixtures/log/{name}`.
    ///
    /// Both components must be single normal path components (no `..`, `/`, `\`).
    /// Panics with a clear message if the fixture cannot be read.
    fn load_fixture(name: &str) -> String {
        fn is_single_normal(s: &str) -> bool {
            let mut it = std::path::Path::new(s).components();
            matches!(it.next(), Some(Component::Normal(_))) && it.next().is_none()
        }
        assert!(
            is_single_normal(name),
            "load_fixture: name must be a single path component, got {name:?}"
        );
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/log");
        path.push(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    // ============================================================================
    // Tier detection tests
    // ============================================================================

    mod tier_detection_tests {
        use super::*;

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
            assert!(
                content.contains('\u{d7}') || content.contains("×"),
                "dedup should show multiply marker: {content}"
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

    // ============================================================================
    // LogFlags and LogResult type tests
    // ============================================================================

    mod type_tests {
        use super::*;

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
    }

    // ============================================================================
    // Debug filter tests
    // ============================================================================

    mod debug_filter_tests {
        use super::*;

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
            assert!(
                filtered
                    .iter()
                    .all(|(l, _)| { matches!(l.as_deref(), Some("DEBUG") | Some("TRACE")) })
            );
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
    }

    // ============================================================================
    // Deduplication tests
    // ============================================================================

    mod dedup_tests {
        use super::*;

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
    }

    // ============================================================================
    // Timestamp tests
    // ============================================================================

    mod timestamp_tests {
        use super::*;

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
    }

    // ============================================================================
    // JSON field tests
    // ============================================================================

    mod json_field_tests {
        use super::*;

        #[test]
        fn test_extract_json_level_variants() {
            let obj: serde_json::Value =
                serde_json::from_str(r#"{"level": "info", "msg": "test"}"#).unwrap();
            let level = extract_json_level(&obj);
            assert_eq!(level.as_deref(), Some("INFO"));

            let obj2: serde_json::Value =
                serde_json::from_str(r#"{"severity": "warn", "message": "test"}"#).unwrap();
            let level2 = extract_json_level(&obj2);
            assert_eq!(level2.as_deref(), Some("WARN"));
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

    // ============================================================================
    // Stack trace tests
    // ============================================================================

    mod stack_trace_tests {
        use super::*;

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

        #[test]
        fn test_stack_trace_attached_to_entry_via_compress_log() {
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
                display.contains("+2 frames elided"),
                "Footer must appear when frames are elided: {display}"
            );
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
                display.contains("+17 frames elided"),
                "Output must contain elision summary; got: {display}"
            );

            // Frame 1 must have been dropped by the cap.
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
    }

    // ============================================================================
    // Python continuation predicate tests
    // ============================================================================

    mod python_continuation_tests {
        use super::*;

        /// Empty line is not a continuation.
        #[test]
        fn test_is_python_continuation_empty_line() {
            assert!(!is_python_continuation(""));
        }

        /// Whitespace-only line is not a continuation (trimmed is empty).
        #[test]
        fn test_is_python_continuation_whitespace_only() {
            assert!(!is_python_continuation("   "));
            assert!(!is_python_continuation("\t"));
        }

        /// Non-whitespace-leading line is not a continuation (no leading indent).
        #[test]
        fn test_is_python_continuation_no_leading_whitespace() {
            assert!(!is_python_continuation("raise ValueError"));
            assert!(!is_python_continuation("INFO: something"));
        }

        /// A line matching RE_LOG_STACK_TRACE would have been consumed by step 2 at the
        /// call site, so is_python_continuation doesn't need to re-check it. Verify the
        /// predicate itself returns true for indented stack-frame-looking content (the call
        /// site guard prevents misuse).
        #[test]
        fn test_is_python_continuation_indented_file_line_structural() {
            // This IS indented, so the structural predicate returns true.
            // The call site (step 3) only runs after step 2 fails, so indented File
            // lines are caught by RE_LOG_STACK_TRACE first; this test documents the
            // responsibility boundary.
            assert!(is_python_continuation(
                "  File \"/app/foo.py\", line 10, in bar"
            ));
        }

        /// A valid continuation: indented source-preview line.
        #[test]
        fn test_is_python_continuation_valid_source_preview() {
            assert!(is_python_continuation("    do_thing()"));
            assert!(is_python_continuation("\t    return value"));
        }
    }

    // ============================================================================
    // Python traceback end-to-end tests
    // ============================================================================

    mod python_traceback_tests {
        use super::*;

        /// AD-LOG-10 / issue #137: Python `File "..."` stack traces with source-preview
        /// lines are recognised. The fixture has 4 logical frames (4 `File` lines), each
        /// with one source-preview line. Cap = 4 frames → 1 elided, last 3 kept.
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
            // The last frame (threading.py) and its source-preview must be kept.
            assert!(
                error_entry.message.contains("threading.py"),
                "Last Python frame must be kept: {}",
                error_entry.message
            );
            assert!(
                error_entry.message.contains("self.run()"),
                "Source-preview of last frame must be kept: {}",
                error_entry.message
            );
            // 4 logical frames, 3 kept → 1 elided.
            assert_eq!(
                result.stack_frames_elided, 1,
                "Should elide 1 of 4 Python logical frames"
            );
        }

        /// MAX_CONTINUATIONS_PER_FRAME cap: a single `File "..."` frame with more than
        /// 4 continuation lines must keep only the first 4; excess lines are silently
        /// dropped (not counted as frames or new entries).
        ///
        /// Input: ERROR + 1 File frame + 6 continuation lines + INFO.
        /// Expected: only the first 4 continuation lines are appended to the frame;
        /// lines 5 and 6 are dropped. The INFO entry is a clean, separate entry.
        #[test]
        fn test_max_continuations_per_frame_enforced() {
            let input = concat!(
                "ERROR: too many continuations\n",
                "  File \"/app/m.py\", line 1, in fn\n",
                "    continuation_1\n",
                "    continuation_2\n",
                "    continuation_3\n",
                "    continuation_4\n",
                "    continuation_5\n", // beyond cap — must be dropped
                "    continuation_6\n", // beyond cap — must be dropped
                "INFO: done\n",
            );
            let flags = make_flags();
            let result = try_parse_regex_logs(input, &flags).unwrap();

            let error_entry = result
                .entries
                .iter()
                .find(|e| e.level.as_deref() == Some("ERROR"))
                .expect("ERROR entry must exist");

            // First 4 continuations must be kept.
            assert!(
                error_entry.message.contains("continuation_1"),
                "continuation_1 must be kept: {}",
                error_entry.message
            );
            assert!(
                error_entry.message.contains("continuation_4"),
                "continuation_4 must be kept: {}",
                error_entry.message
            );

            // Lines 5 and 6 must be dropped (beyond MAX_CONTINUATIONS_PER_FRAME = 4).
            assert!(
                !error_entry.message.contains("continuation_5"),
                "continuation_5 must be dropped (beyond cap): {}",
                error_entry.message
            );
            assert!(
                !error_entry.message.contains("continuation_6"),
                "continuation_6 must be dropped (beyond cap): {}",
                error_entry.message
            );

            // INFO must be a separate, clean entry — not contaminated by excess continuations.
            let info_entry = result
                .entries
                .iter()
                .find(|e| e.level.as_deref() == Some("INFO"))
                .expect("INFO entry must exist");
            assert!(
                !info_entry.message.contains("continuation"),
                "INFO entry must not contain dropped continuation text: {}",
                info_entry.message
            );
        }

        /// The "During handling of the above exception" chained-exception separator
        /// must flush the pending stack and become an unstructured entry — same
        /// behaviour as the "direct cause" variant already covered by the fixture tests.
        ///
        /// Input: ERROR + 1 frame + separator + 1 frame + INFO.
        /// Expected:
        /// - 2 log-level entries: ERROR and INFO.
        /// - stack_frames_elided == 0 (each chain has 1 frame, well under the cap).
        /// - ERROR entry contains the first File frame.
        /// - Separator text does not appear inside ERROR or INFO messages.
        #[test]
        fn test_chained_separator_during_handling() {
            let input = concat!(
                "ERROR: outer failure\n",
                "Traceback (most recent call last):\n",
                "  File \"/app/inner.py\", line 3, in do_inner\n",
                "    inner()\n",
                "ValueError: inner error\n",
                "\n",
                "During handling of the above exception, another exception occurred:\n",
                "\n",
                "Traceback (most recent call last):\n",
                "  File \"/app/outer.py\", line 7, in do_outer\n",
                "    outer()\n",
                "RuntimeError: outer error\n",
                "INFO: recovered\n",
            );
            let flags = make_flags();
            let result = try_parse_regex_logs(input, &flags).unwrap();

            // No frames should be elided — each chain has ≤ 3 frames.
            assert_eq!(
                result.stack_frames_elided, 0,
                "Expected 0 elided frames; got {}",
                result.stack_frames_elided
            );

            // ERROR and INFO must exist as log-level entries.
            assert!(
                result
                    .entries
                    .iter()
                    .any(|e| e.level.as_deref() == Some("ERROR")),
                "ERROR entry must exist"
            );
            assert!(
                result
                    .entries
                    .iter()
                    .any(|e| e.level.as_deref() == Some("INFO")),
                "INFO entry must exist"
            );

            // INFO entry must not contain any traceback or frame text.
            let info_entry = result
                .entries
                .iter()
                .find(|e| e.level.as_deref() == Some("INFO"))
                .unwrap();
            assert!(
                !info_entry.message.contains("File"),
                "INFO entry must not contain File frame text: {}",
                info_entry.message
            );
            assert!(
                !info_entry.message.contains("During handling"),
                "INFO entry must not contain separator text: {}",
                info_entry.message
            );
        }

        /// issue #137: Source-preview lines are attached to the preceding logical frame,
        /// not counted as separate frames. The fixture has 4 `File` lines each with one
        /// source-preview → last 3 logical frames kept, 1 elided.
        #[test]
        fn test_python_source_preview_lines_attached_to_frame() {
            let input = load_fixture("stack_trace_python.txt");
            let flags = make_flags();
            let result = try_parse_regex_logs(&input, &flags).unwrap();
            let error_entry = result
                .entries
                .iter()
                .find(|e| e.level.as_deref() == Some("ERROR"))
                .expect("ERROR entry must exist");
            // Last 3 logical frames are kept: threading.py/_bootstrap, run/handle_request, run/run.
            assert!(
                error_entry.message.contains("self.run()"),
                "Source-preview of last frame (threading.py) must be present: {}",
                error_entry.message
            );
            assert!(
                error_entry.message.contains("handle_request(payload)"),
                "Source-preview of 3rd-from-last frame must be present: {}",
                error_entry.message
            );
            assert!(
                error_entry.message.contains("return parse_value(data)"),
                "Source-preview of 2nd-from-last frame must be present: {}",
                error_entry.message
            );
            // First logical frame (parse_value) is elided — its source must be absent.
            assert!(
                !error_entry.message.contains("result = int(value)"),
                "Source-preview of elided frame must not appear: {}",
                error_entry.message
            );
        }

        /// issue #137: Source-preview lines must not inflate the logical frame count.
        /// 4 `File` lines → 4 logical frames → 1 elided (not 8 lines → 7 elided).
        #[test]
        fn test_python_source_preview_not_counted_as_frame() {
            let input = load_fixture("stack_trace_python.txt");
            let flags = make_flags();
            let result = try_parse_regex_logs(&input, &flags).unwrap();
            assert_eq!(
                result.stack_frames_elided, 1,
                "4 logical frames with source-preview → 1 elided; got {}",
                result.stack_frames_elided
            );
        }

        /// issue #137: Traceback headers must be attached to the preceding entry,
        /// never emitted as standalone entries with a message starting with "Traceback".
        #[test]
        fn test_python_chained_traceback_headers_attached() {
            let input = load_fixture("stack_trace_python_chained.txt");
            let flags = make_flags();
            let result = try_parse_regex_logs(&input, &flags).unwrap();
            // No entry should have a message that begins with "Traceback"
            for entry in &result.entries {
                assert!(
                    !entry.message.starts_with("Traceback"),
                    "Traceback header leaked as standalone entry: {:?}",
                    entry.message
                );
            }
        }

        /// issue #137: Chained exception — each chain has 2 frames (≤ 3), so none elided.
        #[test]
        fn test_python_chained_traceback_elision_count() {
            let input = load_fixture("stack_trace_python_chained.txt");
            let flags = make_flags();
            let result = try_parse_regex_logs(&input, &flags).unwrap();
            assert_eq!(
                result.stack_frames_elided, 0,
                "Both chains have ≤ 3 frames each; expected 0 elided, got {}",
                result.stack_frames_elided
            );
        }

        /// issue #137: The INFO entry must not contain `File` or `Traceback` text
        /// from the chained tracebacks.
        #[test]
        fn test_python_chained_traceback_info_not_contaminated() {
            let input = load_fixture("stack_trace_python_chained.txt");
            let flags = make_flags();
            let result = try_parse_regex_logs(&input, &flags).unwrap();
            let info_entry = result
                .entries
                .iter()
                .find(|e| e.level.as_deref() == Some("INFO"))
                .expect("INFO entry must exist");
            assert!(
                !info_entry.message.contains("File"),
                "INFO entry must not contain stack frame text: {}",
                info_entry.message
            );
            assert!(
                !info_entry.message.contains("Traceback"),
                "INFO entry must not contain Traceback text: {}",
                info_entry.message
            );
        }

        /// issue #137: PEP 657 caret lines (`~~~~~~^~~~~~~~`) are attached to the
        /// preceding `File` logical frame, not stripped.
        #[test]
        fn test_python_pep657_caret_lines_attached() {
            let input = load_fixture("stack_trace_python_pep657.txt");
            let flags = make_flags();
            let result = try_parse_regex_logs(&input, &flags).unwrap();
            let error_entry = result
                .entries
                .iter()
                .find(|e| e.level.as_deref() == Some("ERROR"))
                .expect("ERROR entry must exist");
            assert!(
                error_entry.message.contains("~~~~~~^~~~~~~~"),
                "PEP 657 caret line must be attached to its frame: {}",
                error_entry.message
            );
        }

        /// issue #137: PEP 657 caret lines must not inflate the logical frame count.
        /// 3 `File` lines → 3 logical frames → 0 elided.
        #[test]
        fn test_python_pep657_not_counted_as_frame() {
            let input = load_fixture("stack_trace_python_pep657.txt");
            let flags = make_flags();
            let result = try_parse_regex_logs(&input, &flags).unwrap();
            assert_eq!(
                result.stack_frames_elided, 0,
                "3 logical frames (with PEP 657 carets) → 0 elided; got {}",
                result.stack_frames_elided
            );
        }

        /// issue #137: Source-preview lines must not create additional log entries.
        /// ERROR + 2 File/source pairs + INFO → exactly 2 entries (ERROR and INFO).
        #[test]
        fn test_source_preview_line_does_not_create_entry() {
            let input = concat!(
                "ERROR: something failed\n",
                "  File \"/app/foo.py\", line 10, in bar\n",
                "    do_thing()\n",
                "  File \"/app/baz.py\", line 20, in qux\n",
                "    call_other()\n",
                "INFO: done\n",
            );
            let flags = make_flags();
            let result = try_parse_regex_logs(input, &flags).unwrap();
            assert_eq!(
                result.entries.len(),
                2,
                "Only ERROR and INFO entries expected; got {:?}",
                result
                    .entries
                    .iter()
                    .map(|e| (&e.level, &e.message))
                    .collect::<Vec<_>>()
            );
        }

        /// issue #137: A standalone Traceback (no preceding log-level line) with one
        /// File/source and an exception line (no log-level prefix) must be parsed as
        /// structured output (`try_parse_regex_logs` returns `Some`), because
        /// `RE_LOG_STACK_TRACE` match sets `found_structured = true`.
        #[test]
        fn test_found_structured_gate_with_traceback_only() {
            let input = concat!(
                "ERROR: startup failed\n",
                "Traceback (most recent call last):\n",
                "  File \"/app/main.py\", line 5, in <module>\n",
                "    start()\n",
                "AssertionError: precondition failed\n",
            );
            let flags = make_flags();
            let result = try_parse_regex_logs(input, &flags);
            assert!(
                result.is_some(),
                "Input with ERROR + Traceback + File + AssertionError must parse as structured"
            );
        }

        /// issue #137: A source-preview line that itself contains a log-level keyword
        /// (e.g. `"ERROR: bad input"`) must NOT be misclassified as a new log entry.
        #[test]
        fn test_python_source_line_with_log_keyword_not_misclassified() {
            // The source-preview line contains "ERROR: bad input" — this must stay
            // attached to the File frame, not become a separate ERROR entry.
            let input = concat!(
                "ERROR: validation failed\n",
                "Traceback (most recent call last):\n",
                "  File \"/app/v.py\", line 7, in validate\n",
                "    raise ValueError(\"ERROR: bad input\")\n",
                "INFO: fallback used\n",
            );
            let flags = make_flags();
            let result = try_parse_regex_logs(input, &flags).unwrap();
            let error_entries: Vec<_> = result
                .entries
                .iter()
                .filter(|e| e.level.as_deref() == Some("ERROR"))
                .collect();
            assert_eq!(
                error_entries.len(),
                1,
                "Source-preview with log keyword must not create extra ERROR entry; got: {:?}",
                error_entries.iter().map(|e| &e.message).collect::<Vec<_>>()
            );
        }

        /// issue #137: Pending stack cap counts logical frames; source-preview lines
        /// don't consume extra cap slots.
        ///
        /// ERROR + 10 logical frames (each with source-preview) + INFO:
        /// cap = 4 → 7 cap-evictions, flush keeps last 3 → total 7 elided.
        /// The last frame's source-preview is present; the first frame's source is absent.
        #[test]
        fn test_pending_stack_cap_with_source_preview() {
            let mut input = String::from("ERROR: overflow\n");
            for i in 1..=10 {
                input.push_str(&format!(
                    "  File \"/app/mod.py\", line {i}, in call{i}\n    call{i}()\n"
                ));
            }
            input.push_str("INFO: done\n");

            let flags = make_flags();
            let result = try_parse_regex_logs(&input, &flags).unwrap();

            // 10 logical frames: cap evicts 7 incrementally, flush drops 0 more (3 kept) → 7 elided.
            assert_eq!(
                result.stack_frames_elided, 7,
                "10 logical frames → 7 elided (last 3 kept); got {}",
                result.stack_frames_elided
            );

            let error_entry = result
                .entries
                .iter()
                .find(|e| e.level.as_deref() == Some("ERROR"))
                .expect("ERROR entry must exist");

            // Last frame (call10) source must be present.
            assert!(
                error_entry.message.contains("call10()"),
                "Last frame source-preview must be kept: {}",
                error_entry.message
            );
            // First frame (call1) source must be absent (evicted by cap).
            assert!(
                !error_entry.message.contains("call1()"),
                "Evicted frame source-preview must not appear: {}",
                error_entry.message
            );
        }

        /// issue #137: A `Traceback` header at the start (no preceding entry) becomes
        /// an unstructured entry that is later flushed so it does not appear in the
        /// INFO entry's message.
        #[test]
        fn test_traceback_header_only_input() {
            let input = concat!(
                "ERROR: boot failed\n",
                "Traceback (most recent call last):\n",
                "INFO: continuing\n",
            );
            let flags = make_flags();
            let result = try_parse_regex_logs(input, &flags).unwrap();
            let info_entry = result
                .entries
                .iter()
                .find(|e| e.level.as_deref() == Some("INFO"))
                .expect("INFO entry must exist");
            assert!(
                !info_entry.message.contains("Traceback"),
                "Traceback must not bleed into INFO entry: {}",
                info_entry.message
            );
            // The Traceback text is attached to the preceding ERROR entry.
            let error_entry = result
                .entries
                .iter()
                .find(|e| e.level.as_deref() == Some("ERROR"))
                .expect("ERROR entry must exist");
            assert!(
                error_entry.message.contains("Traceback"),
                "Traceback header must be attached to ERROR entry: {}",
                error_entry.message
            );
        }

        /// issue #137: The rendered output from the Python fixture must be coherent —
        /// contains ValueError, the elision marker, and both log levels.
        #[test]
        fn test_python_rendered_output_coherent() {
            let input = load_fixture("stack_trace_python.txt");
            let flags = make_flags();
            let result = try_parse_regex_logs(&input, &flags).unwrap();
            let display = result.as_ref();
            assert!(
                display.contains("ValueError"),
                "Rendered output must contain exception type: {display}"
            );
            assert!(
                display.contains("frames elided"),
                "Rendered output must contain elision marker: {display}"
            );
            assert!(
                display.contains("ERROR:"),
                "Rendered output must contain ERROR level: {display}"
            );
            assert!(
                display.contains("INFO:"),
                "Rendered output must contain INFO level: {display}"
            );
        }

        /// issue #137: The chained exception rendered output must contain both error
        /// types and the INFO recovery message as a separate entry.
        #[test]
        fn test_chained_rendered_output_not_split() {
            let input = load_fixture("stack_trace_python_chained.txt");
            let flags = make_flags();
            let result = try_parse_regex_logs(&input, &flags).unwrap();
            let display = result.as_ref();
            assert!(
                display.contains("DatabaseError"),
                "Rendered output must contain DatabaseError: {display}"
            );
            assert!(
                display.contains("ServiceError"),
                "Rendered output must contain ServiceError: {display}"
            );
            // INFO entry must be a separate line, not embedded in error entries.
            let info_entry = result
                .entries
                .iter()
                .find(|e| e.level.as_deref() == Some("INFO"))
                .expect("INFO entry must exist");
            assert!(
                info_entry.message.contains("recovered"),
                "INFO entry must contain recovery message: {}",
                info_entry.message
            );
        }

        /// TESTING-2: Python exception-type lines (e.g. `DatabaseError: msg`, `ServiceError: msg`)
        /// that appear after stack frames — after the stack is flushed — must be preserved as
        /// unstructured entries (level = None), not silently dropped.
        #[test]
        fn test_python_exception_type_lines_preserved_as_unstructured() {
            // Traceback + File frame + exception-type line (no log-level prefix).
            // The exception-type line must appear in all_entries with level=None.
            let input = concat!(
                "ERROR: startup failed\n",
                "Traceback (most recent call last):\n",
                "  File \"/app/db.py\", line 42, in connect\n",
                "    conn = engine.connect()\n",
                "DatabaseError: connection refused\n",
                "INFO: retrying\n",
            );
            let flags = make_flags();
            let result = try_parse_regex_logs(input, &flags).unwrap();

            // DatabaseError: line must be present as an unstructured entry.
            let has_db_error = result
                .entries
                .iter()
                .any(|e| e.level.is_none() && e.message.contains("DatabaseError"));
            assert!(
                has_db_error,
                "DatabaseError exception-type line must be preserved as an unstructured entry; entries: {:?}",
                result
                    .entries
                    .iter()
                    .map(|e| (&e.level, &e.message))
                    .collect::<Vec<_>>()
            );
        }

        #[test]
        fn test_orphaned_stack_frames_fall_through_to_passthrough() {
            // Stack frames with no preceding log entry should return None
            // (passthrough), not Some with 0 entries that silently drops content.
            let input = concat!(
                "  File \"/app/foo.py\", line 10, in bar\n",
                "    x = do_stuff()\n",
                "  File \"/app/baz.py\", line 20, in qux\n",
                "    y = crash()\n",
            );
            let flags = make_flags();
            let result = try_parse_regex_logs(input, &flags);
            assert!(
                result.is_none(),
                "Orphaned stack frames (no log entry) must fall through to passthrough"
            );
        }

        #[test]
        fn test_orphaned_frames_do_not_leak_across_blank_lines() {
            // Orphaned frames before a blank line must not attach to a later entry.
            let input = concat!(
                "  File \"/app/old.py\", line 1, in stale_func\n",
                "    old_code()\n",
                "\n",
                "ERROR: real problem here\n",
                "  File \"/app/new.py\", line 2, in new_func\n",
                "    new_code()\n",
            );
            let flags = make_flags();
            let result = try_parse_regex_logs(input, &flags).expect("Should parse the ERROR entry");
            let error_entry = result
                .entries
                .iter()
                .find(|e| e.message.contains("real problem"))
                .expect("ERROR entry must exist");
            assert!(
                !error_entry.message.contains("stale_func"),
                "Orphaned frame from before blank line must not leak onto the ERROR entry; got: {:?}",
                error_entry.message
            );
            assert!(
                error_entry.message.contains("new_func"),
                "Frame after the ERROR entry should be attached; got: {:?}",
                error_entry.message
            );
        }
    }
}
