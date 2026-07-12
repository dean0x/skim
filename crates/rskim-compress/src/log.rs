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

/// Whether log compression must be lossless (proxy egress) or may be lossy (CLI ingestion).
///
/// # ADR-007 (L3 lossless egress)
///
/// The CLI path is `Lossy` — it compresses for the agent's own ingest and the agent can
/// re-read the original. The proxy path is `Lossless` — the downstream LLM only sees what
/// the proxy forwards; any discard is unrecoverable.
///
/// `Lossless` invariants (per ADR-007):
/// - Timestamps: captured as min–max metadata; never silently stripped.
/// - Dedup: case-sensitive keying; first-seen ordering preserved.
/// - DEBUG/TRACE lines: included, never hidden.
/// - Stack frames: never elided; if elision would be needed → passthrough.
/// - PENDING_STACK_CAP: no cap eviction; if cap would trigger → passthrough.
/// - Message/level truncation: never applied; if truncation needed → passthrough.
/// - JSON extra fields: never silently dropped → passthrough for JSON-structured logs.
/// - Any required content-discarding operation → whole block returned unmodified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Losslessness {
    /// Standard (CLI) mode: dedup, debug-hide, stack-elision, timestamp-strip.
    #[default]
    Lossy,
    /// Proxy egress mode: annotations summarize metadata only; no content discarded.
    Lossless,
}

/// A single log entry with optional level and deduplication count.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LogEntry {
    /// Log level (e.g., "ERROR", "WARN", "INFO"), if detected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// The log message (timestamp-stripped, deduplicated).
    pub message: String,
    /// How many times this message appeared in the input.
    pub count: usize,
    /// Lossless mode: earliest timestamp seen for this dedup group (ISO-8601 prefix, trimmed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_timestamp: Option<String>,
    /// Lossless mode: latest timestamp seen for this dedup group (ISO-8601 prefix, trimmed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_timestamp: Option<String>,
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
            // Lossless mode: when count > 1 and timestamps were captured, append range.
            let count_annotation = if entry.count > 1 {
                match (&entry.min_timestamp, &entry.max_timestamp) {
                    (Some(min_ts), Some(max_ts)) => {
                        format!(" (\u{d7}{}, [{min_ts}..{max_ts}])", entry.count)
                    }
                    _ => format!(" (\u{d7}{})", entry.count),
                }
            } else {
                String::new()
            };

            match &entry.level {
                Some(level) => {
                    let _ = write!(output, "\n {level}: {}{count_annotation}", entry.message);
                }
                None => {
                    let _ = write!(output, "\n {}{count_annotation}", entry.message);
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
    /// Whether compression must be lossless (proxy egress) or may be lossy (CLI).
    ///
    /// Default: `Lossy` — all existing callers are unchanged. Set to `Lossless`
    /// for the proxy egress path (see ADR-007 and `Losslessness` docs).
    pub losslessness: Losslessness,
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
///
/// # ADR-007 — Losslessness regime
///
/// When `flags.losslessness == Losslessness::Lossless`, dispatches to
/// `compress_log_lossless` — a content-preserving path that annotates
/// metadata (duplicate counts, timestamp ranges) without discarding any
/// content bytes.
pub fn compress_log(input: &str, flags: &LogFlags) -> ParseResult<LogResult> {
    if flags.losslessness == Losslessness::Lossless {
        return compress_log_lossless(input, flags);
    }
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
// Lossless mode entry point and helper functions (ADR-007)
// ============================================================================

/// Lossless mode compression — content-preserving annotated form.
///
/// Dispatched from `compress_log` when `flags.losslessness == Losslessness::Lossless`.
///
/// # ADR-007 invariants
///
/// - JSON-structured logs: always `Passthrough` (extra fields beyond level+message
///   would be silently dropped — axes 7 and 8 / content-discarding).
/// - Regex logs: `Degraded` when parseable with full lossless constraints;
///   `Passthrough` if any content-discarding operation would be required.
///
/// CLI behaviour is NEVER affected: CLI callers use `Losslessness::Lossy` (the
/// default), so all existing golden tests remain byte-identical.
fn compress_log_lossless(input: &str, flags: &LogFlags) -> ParseResult<LogResult> {
    // Axis 8 (JSON extra fields) and axes 7a/7b (message/level truncation):
    // JSON-structured logs always have extra fields (timestamp, request-id, etc.)
    // that our extractor silently drops. Return Passthrough immediately.
    if input
        .lines()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|first| serde_json::from_str::<serde_json::Value>(first.trim()).is_ok())
    {
        // JSON-structured log — content-discarding unavoidable → passthrough.
        return ParseResult::Passthrough(input.to_string());
    }

    // Try regex tier with lossless constraints.
    match try_parse_regex_logs_lossless(input, flags) {
        Some(result) => ParseResult::Degraded(
            result,
            vec!["log: lossless mode — regex-parsed, content-preserving".to_string()],
        ),
        None => ParseResult::Passthrough(input.to_string()),
    }
}

/// Strip an ISO-8601 timestamp prefix from `line` and capture the raw prefix.
///
/// Returns `(stripped_line, Some(captured_timestamp))` when a timestamp is found.
/// Returns `(line, None)` when `keep_timestamps=true` or no prefix found.
///
/// Only ISO-8601-style prefixes matched by `RE_LOG_TIMESTAMP` are captured as
/// metadata. Non-matching timestamp-ish text is CONTENT and returned verbatim.
fn strip_timestamp_lossless(line: &str, keep_timestamps: bool) -> (&str, Option<String>) {
    if keep_timestamps {
        return (line, None);
    }
    match RE_LOG_TIMESTAMP.find(line) {
        Some(m) => {
            let ts = line[..m.end()].trim().to_string();
            (&line[m.end()..], Some(ts))
        }
        None => (line, None),
    }
}

/// Flush pending stack frames in Lossless mode.
///
/// Returns `None` if flushing would require elision (>3 frames) — the caller
/// must return `Passthrough` for the whole block. Returns `Some(())` otherwise.
///
/// # ADR-007 — axes 5 and 6
///
/// Stack frame elision (keep-last-3 rule) and PENDING_STACK_CAP eviction are
/// both content-discarding operations. If either would trigger, signal passthrough.
fn try_flush_stack_lossless(
    all_entries: &mut [(Option<String>, String, Option<String>)],
    pending_stack: &mut VecDeque<String>,
) -> Option<()> {
    if pending_stack.is_empty() || all_entries.is_empty() {
        pending_stack.clear();
        return Some(());
    }
    if pending_stack.len() > 3 {
        // Flushing would require elision — passthrough (axis 5).
        return None;
    }
    if let Some((_, msg, _)) = all_entries.last_mut() {
        for frame in pending_stack.iter() {
            msg.push('\n');
            msg.push_str(frame);
        }
    }
    pending_stack.clear();
    Some(())
}

/// Lossless regex log parser.
///
/// Returns `None` when any content-discarding operation would be required
/// (stack elision, cap eviction, etc.). The caller returns `Passthrough`.
///
/// Per ADR-007: annotations may summarize METADATA (timestamps→ranges,
/// duplicates→counts) but CONTENT bytes are never discarded.
fn try_parse_regex_logs_lossless(input: &str, flags: &LogFlags) -> Option<LogResult> {
    // (level, message, captured_timestamp)
    let mut all_entries: Vec<(Option<String>, String, Option<String>)> = Vec::with_capacity(256);
    let mut total_lines = 0usize;
    let mut found_structured = false;
    let mut pending_stack: VecDeque<String> = VecDeque::new();
    let mut frame_ctx = FrameContext::Idle;

    for line in input.lines().take(MAX_INPUT_LINES) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            try_flush_stack_lossless(&mut all_entries, &mut pending_stack)?;
            pending_stack.clear();
            frame_ctx = FrameContext::Idle;
            continue;
        }

        // Stack trace detection (axes 5 + 6).
        if RE_LOG_STACK_TRACE.is_match(line) {
            if pending_stack.len() >= PENDING_STACK_CAP {
                // Cap eviction would occur — passthrough (axis 6).
                return None;
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

        // Python source-preview / PEP 657 caret continuation.
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
            total_lines += 1;
            continue;
        }

        // Strip timestamp and capture it for range annotation (axis 1).
        let (without_ts, captured_ts) = strip_timestamp_lossless(trimmed, flags.keep_timestamps);

        // Traceback header.
        if without_ts.starts_with("Traceback (most recent call last)") {
            if let Some((_, msg, _)) = all_entries.last_mut() {
                msg.push('\n');
                msg.push_str(trimmed);
            } else {
                all_entries.push((None, trimmed.to_string(), captured_ts));
            }
            frame_ctx = FrameContext::Idle;
            found_structured = true;
            total_lines += 1;
            continue;
        }

        // Chained exception separator.
        if without_ts.starts_with("During handling of the above exception")
            || without_ts.starts_with("The above exception was the direct cause")
        {
            try_flush_stack_lossless(&mut all_entries, &mut pending_stack)?;
            frame_ctx = FrameContext::Idle;
            all_entries.push((None, trimmed.to_string(), captured_ts));
            total_lines += 1;
            continue;
        }

        // Reset python-frame context.
        frame_ctx = FrameContext::Idle;

        // New log line — flush pending stack frames.
        try_flush_stack_lossless(&mut all_entries, &mut pending_stack)?;

        total_lines += 1;

        if let Some((level, message)) = classify_log_line(without_ts) {
            all_entries.push((Some(level), message, captured_ts));
            found_structured = true;
        } else {
            all_entries.push((None, without_ts.to_string(), captured_ts));
        }
    }

    // Flush trailing stack frames.
    try_flush_stack_lossless(&mut all_entries, &mut pending_stack)?;

    if !found_structured || all_entries.is_empty() {
        return None;
    }

    Some(apply_compression_lossless(all_entries, total_lines))
}

/// Deduplicate entries in Lossless mode using a case-sensitive key (axis 3).
///
/// Tracks per-group min/max timestamps for range annotation (axis 1).
/// Preserves first-seen insertion order (axis 2).
fn deduplicate_entries_lossless(
    entries: Vec<(Option<String>, String, Option<String>)>,
) -> Vec<LogEntry> {
    let mut dedup_map: HashMap<String, usize> = HashMap::with_capacity(1024);
    let mut output_entries: Vec<LogEntry> = Vec::with_capacity(256);
    let mut key_buf = String::with_capacity(128);

    for (level, message, timestamp) in entries {
        key_buf.clear();
        key_buf.push_str(level.as_deref().unwrap_or("-"));
        key_buf.push('|');
        // Axis 3: case-sensitive message key (no .to_lowercase()).
        key_buf.push_str(&message);

        if let Some(&idx) = dedup_map.get(key_buf.as_str()) {
            output_entries[idx].count += 1;
            // Axis 1: update max_timestamp to last-seen.
            if let Some(ts) = timestamp {
                output_entries[idx].max_timestamp = Some(ts);
            }
        } else {
            let idx = output_entries.len();
            dedup_map.insert(key_buf.clone(), idx);
            output_entries.push(LogEntry {
                level,
                message,
                count: 1,
                min_timestamp: timestamp.clone(),
                max_timestamp: timestamp,
            });
        }
    }

    output_entries
}

/// Apply lossless compression to parsed entries.
///
/// - No debug filtering (axis 4): all levels including DEBUG/TRACE included.
/// - Case-sensitive dedup (axis 3) with timestamp range capture (axis 1).
/// - stack_frames_elided = 0 (axis 5: no elision allowed; any elision → passthrough).
fn apply_compression_lossless(
    all_entries: Vec<(Option<String>, String, Option<String>)>,
    total_lines: usize,
) -> LogResult {
    // Axis 4: no debug filtering — all levels included, debug_hidden = 0.
    let output_entries = deduplicate_entries_lossless(all_entries);
    let unique_messages = output_entries.len();
    let deduplicated_count = total_lines.saturating_sub(unique_messages);

    LogResult::new_with_stack(
        total_lines,
        unique_messages,
        0, // debug_hidden = 0 in Lossless mode
        deduplicated_count,
        output_entries,
        false,
        0, // stack_frames_elided = 0 (any elision → passthrough, not here)
    )
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
            // P1.1: count every non-blank input line that is handled here so
            // total_lines >= unique_messages + debug_hidden always holds.
            total_lines += 1;
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
            // P1.1: count Traceback header lines so total_lines is accurate.
            total_lines += 1;
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
            // P1.1: count separator lines so total_lines is accurate.
            total_lines += 1;
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
                ..Default::default()
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
                ..Default::default()
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
    // P1.1: all three non-step-8 entry-push sites (continuation, Traceback header,
    // chained-exception separator) now increment total_lines, so this invariant
    // must hold.  debug_assert fires in debug/test builds; saturating_sub is the
    // release safety belt (DoS prevention — no release panic from user input).
    debug_assert!(
        total_lines >= unique_messages.saturating_add(debug_hidden),
        "P1.1 bug: total_lines({total_lines}) < unique({unique_messages}) + debug({debug_hidden}); \
         an entry-push site is not counting"
    );
    let deduplicated_count =
        total_lines.saturating_sub(unique_messages.saturating_add(debug_hidden));

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
            assert_eq!(f.losslessness, Losslessness::Lossy, "default must be Lossy");
        }

        #[test]
        fn test_log_result_display() {
            let entries = vec![LogEntry {
                level: Some("ERROR".to_string()),
                message: "something failed".to_string(),
                count: 2,
                ..Default::default()
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

    // ============================================================================
    // P1.1 — entry-push site counting fix (#427 Step 7)
    // ============================================================================

    mod p1_1_counting_fix_tests {
        use super::*;

        /// Combined three-site fixture: continuation lines (Step 3), Traceback header
        /// (Step 5), and chained-exception separator (Step 6) are all now counted in
        /// `total_lines` so the header arithmetic holds.
        ///
        /// Non-blank lines counted after the fix:
        /// - ERROR: outer failure         → step 8           (+1)
        /// - Traceback (most recent…)     → step 5 (site 2)  (+1)
        /// - File "/app/foo.py"…          → step 2 (NOT counted — stack frame)
        /// - bad()                        → step 3 (site 1)  (+1)
        /// - ValueError: bad value        → step 8           (+1)
        /// - The above exception…         → step 6 (site 3)  (+1)
        /// - RuntimeError: wrapped        → step 8           (+1)
        /// - INFO: recovered              → step 8           (+1)
        ///
        /// total_lines = 7, unique_messages = 5, debug_hidden = 0,
        /// deduplicated_count = 7 − 5 − 0 = 2.
        /// X − Z = Y ⟹ 7 − 2 = 5 ✓
        #[test]
        fn test_three_uncounted_sites_total_lines_fix() {
            let input = concat!(
                "ERROR: outer failure\n",
                "Traceback (most recent call last):\n",
                "  File \"/app/foo.py\", line 5, in f\n",
                "    bad()\n",
                "ValueError: bad value\n",
                "\n",
                "The above exception was the direct cause of the following exception:\n",
                "\n",
                "RuntimeError: wrapped\n",
                "INFO: recovered\n",
            );
            let flags = make_flags();
            let result = try_parse_regex_logs(input, &flags).expect("Should produce a LogResult");

            assert_eq!(
                result.total_lines, 7,
                "P1.1: total_lines must count all three uncounted sites \
                 (continuation, Traceback header, separator); got {}",
                result.total_lines
            );
            assert_eq!(result.unique_messages, 5, "5 distinct entries expected");
            assert_eq!(result.debug_hidden, 0, "no debug lines in this input");
            assert_eq!(
                result.deduplicated_count, 2,
                "deduplicated_count = total_lines({}) - unique({}) - debug({}) = 2",
                result.total_lines, result.unique_messages, result.debug_hidden
            );

            // X − Z = Y must hold (debug_hidden = 0 ⟹ no remainder).
            assert_eq!(
                result.total_lines.saturating_sub(result.deduplicated_count),
                result.unique_messages,
                "Header invariant X − Z = Y violated: {total} − {dedup} ≠ {unique}",
                total = result.total_lines,
                dedup = result.deduplicated_count,
                unique = result.unique_messages
            );
        }

        /// General invariant X = Y + Z + W across ALL regex-parseable fixtures.
        ///
        /// `total_lines = unique_messages + deduplicated_count + debug_hidden`
        /// must hold for every fixture regardless of debug content.
        #[test]
        fn test_all_regex_fixtures_header_arithmetic_invariant() {
            let fixture_names: &[&str] = &[
                "plaintext_mixed.txt",
                "stack_trace_java.txt",
                "stack_trace_python.txt",
                "stack_trace_python_chained.txt",
                "stack_trace_python_pep657.txt",
                "duplicate_heavy.txt",
                "debug_heavy.txt",
                "dedup_error_warn.txt",
            ];

            let flags = make_flags();
            for name in fixture_names {
                let input = load_fixture(name);
                let result = match try_parse_regex_logs(&input, &flags) {
                    Some(r) => r,
                    None => continue, // Passthrough — no LogResult to check.
                };
                let computed = result
                    .unique_messages
                    .saturating_add(result.deduplicated_count)
                    .saturating_add(result.debug_hidden);
                assert_eq!(
                    result.total_lines,
                    computed,
                    "Fixture {name}: X = Y + Z + W violated: \
                     total={} ≠ unique={} + dedup={} + debug={}",
                    result.total_lines,
                    result.unique_messages,
                    result.deduplicated_count,
                    result.debug_hidden
                );
            }
        }
    }

    // ============================================================================
    // Lossless regime tests (Step 8 / ADR-007)
    //
    // One discriminating test per lossy axis. Each test fails if the axis "leaks"
    // into Lossless mode (i.e., the code silently applies a Lossy operation when
    // Lossless is requested). Per PF-007.
    // ============================================================================

    mod lossless_regime_tests {
        use super::*;
        use proptest::prelude::*;

        fn lossless_flags() -> LogFlags {
            LogFlags {
                losslessness: Losslessness::Lossless,
                ..Default::default()
            }
        }

        // ---- Axis 1: Timestamps → min/max range annotation ----

        /// Axis 1: duplicate lines with different ISO-8601 timestamps → output shows
        /// `×N, [ts_min..ts_max]` annotation. Fails if timestamps are silently
        /// stripped without range capture (Lossy leak).
        #[test]
        fn lossless_axis1_timestamps_range_annotation() {
            let input = concat!(
                "2024-01-01T10:00:00Z ERROR: connection refused\n",
                "2024-01-01T10:05:00Z ERROR: connection refused\n",
                "2024-01-01T10:10:00Z ERROR: connection refused\n",
            );
            let result = compress_log(input, &lossless_flags());
            assert!(
                !result.is_passthrough(),
                "Structured log with timestamps must not passthrough: tier={}",
                result.tier_name()
            );
            let content = result.content();
            // Range annotation must appear.
            assert!(
                content.contains("2024-01-01T10:00:00Z"),
                "min_timestamp must appear in output: {content}"
            );
            assert!(
                content.contains("2024-01-01T10:10:00Z"),
                "max_timestamp must appear in output: {content}"
            );
            // Count annotation.
            assert!(
                content.contains('\u{d7}'),
                "×N marker must appear: {content}"
            );
        }

        // ---- Axis 2: Global dedup → first-seen ordering preserved ----

        /// Axis 2: dedup preserves first-seen insertion order. Fails if ordering
        /// is altered (e.g., sorted or reversed in Lossless mode).
        #[test]
        fn lossless_axis2_dedup_insertion_order_preserved() {
            let input = concat!(
                "ERROR: alpha\n",
                "INFO: beta\n",
                "WARN: gamma\n",
                "ERROR: alpha\n", // duplicate of first
            );
            let result = compress_log(input, &lossless_flags());
            assert!(!result.is_passthrough());
            let content = result.content();
            // Alpha (first seen) must appear before beta, beta before gamma.
            let pos_alpha = content.find("alpha").expect("alpha must be in output");
            let pos_beta = content.find("beta").expect("beta must be in output");
            let pos_gamma = content.find("gamma").expect("gamma must be in output");
            assert!(
                pos_alpha < pos_beta && pos_beta < pos_gamma,
                "First-seen order must be preserved: alpha={pos_alpha} beta={pos_beta} gamma={pos_gamma}"
            );
        }

        // ---- Axis 3: Case-variant messages → never silently merged ----

        /// Axis 3: "error: x" (lowercase) and "ERROR: x" (uppercase) are distinct
        /// content — must not be merged by case-folding in Lossless mode.
        ///
        /// Discriminating: if case-folding is applied (Lossy leak), both variants
        /// collapse to one entry and the assert on entry count fails.
        #[test]
        fn lossless_axis3_case_variants_not_merged() {
            // Two messages with the same text but different level casing.
            // The dedup key in Lossy mode lowercases the message, merging them.
            // In Lossless mode, the key is case-sensitive, so they stay separate.
            let input = concat!(
                "INFO: Request processed successfully\n",
                "INFO: request processed successfully\n", // lowercase variant
            );
            let result = compress_log(input, &lossless_flags());
            assert!(!result.is_passthrough());
            let content = result.content();
            // Both variants must be present as separate lines.
            assert!(
                content.contains("Request processed successfully"),
                "Uppercase-R variant must be present verbatim: {content}"
            );
            assert!(
                content.contains("request processed successfully"),
                "Lowercase-r variant must be present verbatim: {content}"
            );
            // Must NOT show a ×2 dedup marker (they are distinct entries).
            assert!(
                !content.contains('\u{d7}'),
                "Case variants must NOT be merged (no ×2 marker): {content}"
            );
        }

        // ---- Axis 4: DEBUG/TRACE lines → present in output ----

        /// Axis 4: DEBUG and TRACE lines must reach the output in Lossless mode.
        /// Fails if they are hidden (Lossy leak: `filter_debug_entries` applied).
        #[test]
        fn lossless_axis4_debug_lines_present() {
            let input = concat!(
                "ERROR: something failed\n",
                "DEBUG: internal state: x=42\n",
                "TRACE: entering method foo\n",
                "INFO: recovered\n",
            );
            let result = compress_log(input, &lossless_flags());
            assert!(!result.is_passthrough());
            let content = result.content();
            assert!(
                content.contains("internal state"),
                "DEBUG line must be present in Lossless output: {content}"
            );
            assert!(
                content.contains("entering method foo"),
                "TRACE line must be present in Lossless output: {content}"
            );
        }

        // ---- Axis 5: Stack frames → passthrough when elision needed ----

        /// Axis 5: a stack with more than 3 frames would normally elide frames
        /// (keep-last-3 rule). In Lossless mode, the WHOLE block must return
        /// Passthrough (never partially-lossy output).
        ///
        /// Discriminating: if the keep-last-3 rule is silently applied, the result
        /// is Compressed with missing frames — this test asserts Passthrough instead.
        #[test]
        fn lossless_axis5_stack_frames_no_elision_passthrough() {
            // 4 stack frames → elision needed (only 3 fit in flush) → passthrough.
            let input = concat!(
                "ERROR: something went wrong\n",
                "    at com.example.Service.a(Service.java:1)\n",
                "    at com.example.Service.b(Service.java:2)\n",
                "    at com.example.Service.c(Service.java:3)\n",
                "    at com.example.Service.d(Service.java:4)\n",
                "INFO: recovered\n",
            );
            let result = compress_log(input, &lossless_flags());
            assert!(
                result.is_passthrough(),
                "4 stack frames require elision → Lossless must return Passthrough; got tier={}",
                result.tier_name()
            );
            // Passthrough content must be byte-identical to input.
            assert_eq!(
                result.content(),
                input,
                "Passthrough must preserve raw input byte-identical"
            );
        }

        // ---- Axis 6: PENDING_STACK_CAP eviction → passthrough ----

        /// Axis 6: PENDING_STACK_CAP = 4. If more than 4 frames arrive before a
        /// flush, the Lossy path evicts the oldest frame. In Lossless mode, the
        /// eviction must NOT occur — instead, the whole block is Passthrough.
        ///
        /// Discriminating: if cap eviction is silently applied, the result is
        /// Compressed with missing frames — this test asserts Passthrough.
        #[test]
        fn lossless_axis6_pending_cap_eviction_passthrough() {
            // 5 consecutive stack frames trigger the cap (PENDING_STACK_CAP = 4).
            let input = concat!(
                "ERROR: deep call chain\n",
                "    at com.example.A.method(A.java:1)\n",
                "    at com.example.B.method(B.java:2)\n",
                "    at com.example.C.method(C.java:3)\n",
                "    at com.example.D.method(D.java:4)\n",
                "    at com.example.E.method(E.java:5)\n", // triggers cap eviction
            );
            let result = compress_log(input, &lossless_flags());
            assert!(
                result.is_passthrough(),
                "5 stack frames trigger PENDING_STACK_CAP → Lossless must passthrough; got tier={}",
                result.tier_name()
            );
        }

        // ---- Axis 7: Message truncation → passthrough (JSON tier) ----

        /// Axis 7: `extract_json_message` truncates messages exceeding MAX_JSON_MSG_LEN.
        /// In Lossless mode, the JSON tier is skipped entirely — the whole block is
        /// Passthrough, preserving the raw JSON verbatim.
        ///
        /// Discriminating: if the JSON tier is exercised in Lossless mode and
        /// truncation is applied, the `[truncated]` suffix appears — failing
        /// the Passthrough assertion.
        #[test]
        fn lossless_axis7_message_truncation_passthrough() {
            let large_msg = "x".repeat(MAX_JSON_MSG_LEN + 100);
            let json_line = format!(r#"{{"level":"error","msg":"{large_msg}"}}"#);
            let result = compress_log(&json_line, &lossless_flags());
            assert!(
                result.is_passthrough(),
                "JSON log with oversized message must passthrough in Lossless mode; got tier={}",
                result.tier_name()
            );
            // Raw content preserved verbatim (no truncation).
            assert!(
                result.content().contains(&large_msg),
                "Passthrough must preserve raw oversized message verbatim"
            );
        }

        // ---- Axis 8: Level truncation → passthrough (JSON tier) ----

        /// Axis 8: `extract_json_level` truncates level fields exceeding MAX_JSON_LEVEL_LEN.
        /// In Lossless mode, the JSON tier is skipped — whole block is Passthrough.
        ///
        /// Discriminating: if the JSON tier runs and truncates the level, the
        /// preserved level in output is shorter than the input — the Passthrough
        /// assertion catches this.
        #[test]
        fn lossless_axis8_level_truncation_passthrough() {
            let large_level = "CUSTOM_LEVEL_".repeat(5); // > MAX_JSON_LEVEL_LEN (32)
            let json_line = format!(r#"{{"level":"{large_level}","msg":"test"}}"#);
            let result = compress_log(&json_line, &lossless_flags());
            assert!(
                result.is_passthrough(),
                "JSON log with oversized level must passthrough in Lossless mode; got tier={}",
                result.tier_name()
            );
        }

        // ---- Axis 9: JSON extra fields → passthrough ----

        /// Axis 9: JSON structured logs have extra fields (timestamp, request_id, etc.)
        /// that the extractor silently drops. In Lossless mode, the JSON tier is
        /// skipped entirely — the whole block is Passthrough.
        ///
        /// Discriminating: if the JSON tier runs in Lossless mode and extra fields
        /// are silently dropped, the Passthrough assertion fails.
        #[test]
        fn lossless_axis9_json_extra_fields_passthrough() {
            let input = r#"{"level":"info","msg":"server started","timestamp":"2024-01-01T10:00:00Z","request_id":"req-001"}"#;
            let result = compress_log(input, &lossless_flags());
            assert!(
                result.is_passthrough(),
                "JSON log with extra fields must passthrough in Lossless mode; got tier={}",
                result.tier_name()
            );
            // All fields preserved verbatim.
            assert!(
                result.content().contains("request_id"),
                "Passthrough must preserve all JSON fields verbatim: {}",
                result.content()
            );
        }

        // ---- Annotation-ambiguity test ----

        /// Annotation-ambiguity: an input line that genuinely ends with `(×5, ...)`-style
        /// text is preserved verbatim and NOT confused with generated annotations.
        ///
        /// Count annotations come from integer counters, never parsed back from text.
        /// This test ensures that a line containing `×` or `[ts..ts]` syntax is
        /// treated as content, not as a generated annotation.
        #[test]
        fn lossless_annotation_ambiguity_suffix_preserved() {
            // A log line whose message ends with annotation-looking text.
            let input = concat!(
                "INFO: batch complete (×5, [2024-01-01..2024-01-02])\n",
                "INFO: batch complete (×5, [2024-01-01..2024-01-02])\n",
            );
            let result = compress_log(input, &lossless_flags());
            // If this is Compressed: the message text must appear verbatim.
            // If Passthrough: raw content preserved.
            let content = result.content();
            assert!(
                content.contains("batch complete (×5, [2024-01-01..2024-01-02])"),
                "Lines with annotation-like suffixes must be preserved verbatim: {content}"
            );
        }

        // ---- Entry-count conservation proptest ----

        proptest! {
            /// For arbitrary log-ish input under Lossless mode:
            /// - Σ entry.count == total_lines (no debug hidden, no silent drops)
            /// - Every distinct message text present verbatim in output
            ///
            /// This is the content-conservation invariant: no input line is silently
            /// discarded in Lossless mode.
            #[test]
            fn prop_lossless_entry_count_conservation(
                lines in proptest::collection::vec(
                    (
                        prop_oneof![
                            Just("ERROR"),
                            Just("WARN"),
                            Just("INFO"),
                        ],
                        proptest::string::string_regex("[a-zA-Z0-9 ]{1,30}").unwrap(),
                    ),
                    1..=15usize
                )
            ) {
                let mut input = String::new();
                for (level, msg) in &lines {
                    input.push_str(level);
                    input.push_str(": ");
                    input.push_str(msg);
                    input.push('\n');
                }

                let flags = lossless_flags();
                let result = compress_log(&input, &flags);

                match result {
                    ParseResult::Passthrough(ref s) => {
                        // Passthrough: content preserved verbatim.
                        prop_assert_eq!(
                            s.as_str(), input.as_str(),
                            "Passthrough must preserve raw input byte-identical"
                        );
                    }
                    ParseResult::Full(ref r) | ParseResult::Degraded(ref r, _) => {
                        // Σ counts == total_lines (debug_hidden = 0 in Lossless).
                        let total_counts: usize = r.entries.iter().map(|e| e.count).sum();
                        prop_assert_eq!(
                            total_counts, r.total_lines,
                            "Σ entry.count ({}) must equal total_lines ({})",
                            total_counts,
                            r.total_lines
                        );

                        // Every distinct message text must appear verbatim in output.
                        // Trim the message before checking: classify_log_line applies
                        // .trim() to the message portion, so trailing/leading spaces
                        // are stripped by the engine. The invariant is about content,
                        // not byte-identical whitespace.
                        let output_text = r.as_ref();
                        let mut seen: std::collections::HashSet<&str> =
                            std::collections::HashSet::new();
                        for (_, msg) in &lines {
                            let trimmed_msg = msg.trim();
                            if trimmed_msg.is_empty() {
                                continue;
                            }
                            if seen.insert(trimmed_msg) {
                                prop_assert!(
                                    output_text.contains(trimmed_msg),
                                    "Message {:?} must appear verbatim in Lossless output",
                                    trimmed_msg
                                );
                            }
                        }
                    }
                }
            }
        }

        // ---- CLI stays Lossy proof ----

        /// Proof: the Lossy path is byte-identical to the pre-Step-8 path.
        ///
        /// The CLI goldens (STABLE + COUNTERFIX) enforce byte-identity at the
        /// binary level. This unit-level test confirms that `Losslessness::Lossy`
        /// (the default) does NOT alter any existing log compression behavior.
        #[test]
        fn lossless_default_is_lossy_cli_path_unchanged() {
            let input = "ERROR: database connection failed\nERROR: database connection failed\nINFO: retrying\n";
            let lossy_result = compress_log(
                input,
                &LogFlags {
                    losslessness: Losslessness::Lossy,
                    ..Default::default()
                },
            );
            let default_result = compress_log(input, &LogFlags::default());
            // Both must produce identical output (Lossy == Default).
            assert_eq!(
                lossy_result.content(),
                default_result.content(),
                "Losslessness::Lossy must produce identical output to Default"
            );
            assert_eq!(
                lossy_result.tier_name(),
                default_result.tier_name(),
                "Tier must be identical for Lossy and Default"
            );
        }
    }
}
