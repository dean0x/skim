//! Content block classification.
//!
//! Classifies text content into one of six classes:
//!
//! | Class | Description |
//! |-------|-------------|
//! | [`Class::Code`] | Fenced code block with an optional language tag |
//! | [`Class::Json`] | Top-level JSON object or array |
//! | [`Class::Log`] | Log output matching structured log-line heuristics |
//! | [`Class::Text`] | Plain prose (default category) |
//! | [`Class::Mixed`] | Content containing fenced code among prose |
//! | [`Class::Unknown`] | No rule fired, or block is exempt from classification |
//!
//! # Classification order (fixed, deterministic)
//!
//! 1. **Fence-tagged code:** If the text starts with ` ``` ` (with an optional
//!    language tag), route to [`Class::Code`] with the tag as `language_hint`.
//!    In v1, unfenced code is NOT detected — it falls to `text` (follow-up: #326).
//! 2. **JSON:** If the trimmed text starts with `{` or `[`, attempt `serde_json` parse.
//!    If successful, route to [`Class::Json`]. Partial JSON stays as text.
//! 3. **Log:** If at least 50% of non-empty lines match log-line heuristics
//!    (timestamp prefix, log-level keyword, or bracketed-prefix pattern), route to
//!    [`Class::Log`].
//! 4. **Mixed code-in-prose:** If the text contains at least one fenced code block
//!    but is not itself purely a fence block, route to [`Class::Mixed`] with the
//!    first fence's language hint.
//! 5. **Text:** Default — prose, unfenced code, and anything else.
//! 6. **Unknown:** Returned only for exempt blocks (see below) or when explicitly
//!    requested for a block that cannot be classified.
//!
//! # Exempt blocks
//!
//! These block types MUST return [`Class::Unknown`] if a class is requested:
//!
//! - `tool_use` input (Anthropic) — opaque model-generated arguments
//! - `thinking` (Anthropic) — opaque reasoning tokens
//! - `tool_calls[].function.arguments` (OpenAI) — opaque function arguments
//! - `tool_call_id` (OpenAI) — correlation identifier
//! - `reasoning` (OpenAI) — reasoning-model content
//! - Any unrecognized / opaque block type
//!
//! This list is the known opaque set as of the current provider schemas. The
//! default-deny catch-all (returning `unknown` for any unrecognized field) ensures
//! byte-faithfulness even as schemas evolve. (Resolved Decision 6.)

use serde::{Deserialize, Serialize};

/// A content classification result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Classification {
    /// The detected class.
    pub class: Class,

    /// Optional language hint (present when `class` is `Code` or `Mixed` and a
    /// fence tag was detected).
    pub language_hint: Option<String>,
}

impl Classification {
    /// Create a `Code` classification with an optional language tag.
    pub fn code(lang: Option<String>) -> Self {
        Self {
            class: Class::Code,
            language_hint: lang,
        }
    }

    /// Create a `Json` classification.
    pub fn json() -> Self {
        Self {
            class: Class::Json,
            language_hint: None,
        }
    }

    /// Create a `Log` classification.
    pub fn log() -> Self {
        Self {
            class: Class::Log,
            language_hint: None,
        }
    }

    /// Create a `Text` classification.
    pub fn text() -> Self {
        Self {
            class: Class::Text,
            language_hint: None,
        }
    }

    /// Create a `Mixed` classification with an optional language tag.
    pub fn mixed(lang: Option<String>) -> Self {
        Self {
            class: Class::Mixed,
            language_hint: lang,
        }
    }

    /// Create an `Unknown` classification.
    pub fn unknown() -> Self {
        Self {
            class: Class::Unknown,
            language_hint: None,
        }
    }
}

/// The six content classes.
///
/// This enum is exhaustive — only these six values are ever returned by the
/// classifier (AC13). If no rule fires, `Unknown` is returned rather than a
/// best-effort guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Class {
    /// A fenced code block (starts with ` ``` `).
    Code,
    /// A top-level JSON object or array.
    Json,
    /// Log output matching structured log-line heuristics.
    Log,
    /// Plain prose text.
    Text,
    /// Prose containing at least one embedded fenced code block.
    Mixed,
    /// No rule fired, or block is exempt from classification.
    Unknown,
}

/// Classify a text string.
///
/// Applies the fixed detection rules in order and returns a [`Classification`].
/// This function is pure and deterministic — identical input always produces
/// identical output (AC6).
///
/// # Examples
///
/// ```
/// use rskim_llm::classify::{classify, Class};
///
/// let c = classify("```rust\nfn main() {}\n```");
/// assert_eq!(c.class, Class::Code);
/// assert_eq!(c.language_hint.as_deref(), Some("rust"));
///
/// let c = classify(r#"{"key": "value"}"#);
/// assert_eq!(c.class, Class::Json);
///
/// let c = classify("Hello, world!");
/// assert_eq!(c.class, Class::Text);
/// ```
pub fn classify(text: &str) -> Classification {
    // Rule 1: Fence-tagged code block
    if let Some(result) = try_classify_fenced(text) {
        return result;
    }

    // Rule 2: JSON (starts with { or [)
    if try_classify_json(text) {
        return Classification::json();
    }

    // Rule 3: Log output
    if try_classify_log(text) {
        return Classification::log();
    }

    // Rule 4: Mixed (prose with embedded fenced blocks)
    if let Some(lang) = try_classify_mixed(text) {
        return Classification::mixed(lang);
    }

    // Rule 5: Default — text
    Classification::text()
}

/// Try to classify as a pure fenced code block.
///
/// Returns `Some(Classification)` if the text is a single fenced code block
/// (optionally with a language tag on the opening fence line).
fn try_classify_fenced(text: &str) -> Option<Classification> {
    let trimmed = text.trim();
    if !trimmed.starts_with("```") {
        return None;
    }

    // Must end with ``` (possibly with trailing whitespace)
    if !trimmed.ends_with("```") {
        return None;
    }

    // Must be more than just the fence markers
    if trimmed.len() < 6 {
        return None;
    }

    // Extract language tag from the first line
    let first_line_end = trimmed.find('\n').unwrap_or(trimmed.len());
    let first_line = &trimmed[3..first_line_end]; // skip the opening ```
    let lang = first_line.trim();

    // Verify the closing fence is on its own line (not the same line as opening)
    if !trimmed.contains('\n') {
        return None;
    }

    let lang_hint = if lang.is_empty() {
        None
    } else {
        Some(lang.to_string())
    };
    Some(Classification::code(lang_hint))
}

/// Try to classify as a JSON value.
///
/// Returns true if the trimmed text starts with `{` or `[` and parses as valid JSON.
fn try_classify_json(text: &str) -> bool {
    let trimmed = text.trim();
    if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
        return false;
    }
    // Attempt full parse — no partial JSON
    serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
}

/// Try to classify as log output.
///
/// Uses heuristics mirrored from rskim's `compress_log` handler (follow-up for
/// shared extraction: #327). A block is classified as log if at least 50% of its
/// non-empty lines match log-line patterns.
fn try_classify_log(text: &str) -> bool {
    let non_empty: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if non_empty.is_empty() {
        return false;
    }

    let matching = non_empty.iter().filter(|&&line| is_log_line(line)).count();

    // At least 50% of non-empty lines must match (aggregate heuristic, OQ7)
    matching * 2 >= non_empty.len()
}

/// Test whether a single line matches log-line heuristics.
fn is_log_line(line: &str) -> bool {
    let trimmed = line.trim();

    // Pattern 1: timestamp prefix (ISO-8601-like, or unix timestamp)
    // e.g. "2024-01-15T10:30:00Z", "2024-01-15 10:30:00", "[2024-01-15]"
    if has_timestamp_prefix(trimmed) {
        return true;
    }

    // Pattern 2: log-level keyword at the start or in brackets
    // e.g. "ERROR:", "WARN:", "[INFO]", "[DEBUG]", "error:"
    if has_log_level_prefix(trimmed) {
        return true;
    }

    // Pattern 3: bracketed prefix pattern common in structured logs
    // e.g. "[component] message", "(module) message"
    if has_bracketed_prefix(trimmed) {
        return true;
    }

    false
}

/// Check for a timestamp-like prefix.
fn has_timestamp_prefix(line: &str) -> bool {
    // ISO-8601 date: starts with 4 digits, dash, 2 digits (e.g. 2024-01-)
    let bytes = line.as_bytes();
    if bytes.len() >= 7
        && bytes[0..4].iter().all(|b| b.is_ascii_digit())
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
    {
        return true;
    }

    // Unix timestamp: starts with 10 digits (seconds since epoch)
    if bytes.len() >= 10 && bytes[..10].iter().all(|b| b.is_ascii_digit()) {
        return true;
    }

    // Bracketed timestamp: "[2024-..."
    if let Some(inner) = line.strip_prefix('[') {
        let close = inner.find(']').unwrap_or(inner.len());
        let candidate = &inner[..close];
        if candidate.len() >= 7 {
            let cb = candidate.as_bytes();
            if cb[..4].iter().all(|b| b.is_ascii_digit()) && cb[4] == b'-' {
                return true;
            }
        }
    }

    false
}

/// Check for a log-level keyword prefix.
fn has_log_level_prefix(line: &str) -> bool {
    const LEVELS: &[&str] = &[
        "ERROR", "WARN", "WARNING", "INFO", "DEBUG", "TRACE", "FATAL", "CRITICAL", "error", "warn",
        "warning", "info", "debug", "trace", "fatal", "critical", "ERR", "WRN", "INF", "DBG",
        "TRC",
    ];

    for level in LEVELS {
        // Bare prefix: "ERROR:" or "ERROR " or "ERROR\t"
        if let Some(rest) = line.strip_prefix(level)
            && (rest.is_empty()
                || rest.starts_with(':')
                || rest.starts_with(' ')
                || rest.starts_with('\t'))
        {
            return true;
        }
        // Bracketed: "[ERROR]" or "[INFO]"
        let bracketed = format!("[{level}]");
        if line.starts_with(&bracketed) {
            return true;
        }
    }
    false
}

/// Check for a generic bracketed prefix pattern.
fn has_bracketed_prefix(line: &str) -> bool {
    if !line.starts_with('[') {
        return false;
    }
    let Some(close) = line.find(']') else {
        return false;
    };
    // Bracket must close reasonably early (not a long JSON array)
    if close > 32 {
        return false;
    }
    // Must be followed by a space or colon
    let after = &line[close + 1..];
    after.starts_with(' ') || after.starts_with(':')
}

/// Try to classify as mixed (prose with embedded fenced code blocks).
///
/// Returns `Some(language_hint)` if the text contains at least one fenced code
/// block that is NOT the entire content of the text.
fn try_classify_mixed(text: &str) -> Option<Option<String>> {
    // Already handled as pure Code above; here we look for embedded fences in prose
    let trimmed = text.trim();

    // Find the first occurrence of ```
    let fence_pos = trimmed.find("```")?;

    // If the fence is at position 0, this was already handled by try_classify_fenced.
    // But try_classify_fenced might have returned None (e.g., unclosed fence).
    // Here we are looking for fences embedded in prose (not at start).
    if fence_pos == 0 {
        // Only reached if try_classify_fenced returned None (e.g., unclosed fence).
        // Find the end of the fence tag line to extract language.
        let after_fence = &trimmed[3..];
        let tag_end = after_fence.find('\n').unwrap_or(after_fence.len());
        let lang = after_fence[..tag_end].trim();
        let lang_hint = if lang.is_empty() {
            None
        } else {
            Some(lang.to_string())
        };
        return Some(lang_hint);
    }

    // Fence is not at the start — extract language hint from the fence line
    let fence_line_start = fence_pos + 3;
    let fence_line_text = &trimmed[fence_line_start..];
    let tag_end = fence_line_text.find('\n').unwrap_or(fence_line_text.len());
    let lang = fence_line_text[..tag_end].trim();
    let lang_hint = if lang.is_empty() {
        None
    } else {
        Some(lang.to_string())
    };

    Some(lang_hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_fenced_code_rust() {
        let text = "```rust\nfn main() {}\n```";
        let c = classify(text);
        assert_eq!(c.class, Class::Code);
        assert_eq!(c.language_hint.as_deref(), Some("rust"));
    }

    #[test]
    fn pure_fenced_code_no_lang() {
        let text = "```\nsome code\n```";
        let c = classify(text);
        assert_eq!(c.class, Class::Code);
        assert_eq!(c.language_hint, None);
    }

    #[test]
    fn json_object() {
        let c = classify(r#"{"key": "value"}"#);
        assert_eq!(c.class, Class::Json);
    }

    #[test]
    fn json_array() {
        let c = classify("[1, 2, 3]");
        assert_eq!(c.class, Class::Json);
    }

    #[test]
    fn partial_json_is_text() {
        let c = classify("{invalid json");
        assert_eq!(c.class, Class::Text);
    }

    #[test]
    fn plain_text() {
        let c = classify("Hello, world! This is a test.");
        assert_eq!(c.class, Class::Text);
    }

    #[test]
    fn log_lines_iso_timestamp() {
        let text = "2024-01-15T10:30:00Z INFO Starting server\n2024-01-15T10:30:01Z INFO Listening on :8080";
        let c = classify(text);
        assert_eq!(c.class, Class::Log);
    }

    #[test]
    fn log_lines_level_prefix() {
        let text = "ERROR: database connection failed\nWARN: retrying...\nINFO: connected";
        let c = classify(text);
        assert_eq!(c.class, Class::Log);
    }

    #[test]
    fn mixed_prose_with_code() {
        let text = "Here is some code:\n```python\nprint('hello')\n```\nThat was the code.";
        let c = classify(text);
        assert_eq!(c.class, Class::Mixed);
    }

    #[test]
    fn deterministic_1000_runs() {
        let inputs = [
            "```rust\nfn x() {}\n```",
            r#"{"a": 1}"#,
            "2024-01-01 INFO test\n2024-01-01 WARN test2",
            "plain text",
            "prose\n```python\ncode\n```\nmore prose",
        ];
        for input in &inputs {
            let first = classify(input);
            for _ in 0..999 {
                assert_eq!(
                    classify(input),
                    first,
                    "non-deterministic for input: {input:?}"
                );
            }
        }
    }

    #[test]
    fn unknown_is_never_guessed() {
        // The unknown class is only returned for exempt blocks, not as a fallback guess
        // from classify() — the fallback is Text. This test verifies the 6 classes cover all outputs.
        let samples = [
            ("```rust\nfn x() {}\n```", Class::Code),
            (r#"{"a":1}"#, Class::Json),
            ("ERROR: fail\nWARN: retry", Class::Log),
            ("hello world", Class::Text),
            ("prose\n```py\ncode\n```\nmore", Class::Mixed),
        ];
        for (text, expected) in &samples {
            assert_eq!(classify(text).class, *expected);
        }
    }

    #[test]
    fn json_lines_is_text_not_json() {
        // JSON-lines (multiple JSON objects on separate lines) is NOT valid JSON
        let text = "{\"a\":1}\n{\"b\":2}";
        let c = classify(text);
        // serde_json won't parse this as a single value
        assert_eq!(c.class, Class::Text);
    }

    #[test]
    fn single_fence_prose_boundary() {
        // A text that starts with ``` but doesn't close properly — not pure Code
        let text = "```python\nsome code here\nno closing fence";
        let c = classify(text);
        // Should be Mixed (has a fence but isn't pure code)
        assert!(matches!(c.class, Class::Mixed));
    }

    #[test]
    fn indented_code_in_prose_is_text() {
        // Indented code blocks (without fence markers) are not detected in v1 (#326)
        let text = "Here is some code:\n    def foo():\n        pass\nEnd of example.";
        let c = classify(text);
        assert_eq!(c.class, Class::Text);
    }
}
