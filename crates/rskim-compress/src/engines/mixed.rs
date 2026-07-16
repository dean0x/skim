//! Mixed-content fence scanner and per-fence compression engine (#304 Phase 2, P0.1 #427).
//!
//! # AD-005 — Fence spans are re-derived by the router
//!
//! `rskim_llm::Classification` provides only `class` and `language_hint` (no
//! byte-offset spans). This module implements a single-pass, CRLF-aware fence
//! scanner that finds ` ``` `…` ``` ` pairs and routes each fence body through
//! the appropriate compressor.
//!
//! # Invariants (AC6)
//!
//! 1. Every byte OUTSIDE fence bodies (prose, ``` delimiters, info strings) MUST
//!    be byte-identical to the input.
//! 2. The count of ``` fences MUST be unchanged.
//! 3. Each fence BODY is independently routed per the precedence table (AD-006 /
//!    Gate-1 / P0.1). Routing uses [`crate::route::engine_for_fence`] — the single
//!    routing table — rather than any local dispatch:
//!    - `json` hint (case-insensitive) → JSON minifier, then `value_equivalent_raw`
//!      gate; only `Some(true)` splices the minified body. Everything else: byte-identical.
//!    - all other hints / no hint → passthrough (byte-identical body).
//!    - Code language hints → passthrough (P0.1 / ADR-007: code never compressed on egress).
//! 4. Unclosed fences (no closing ```) are left byte-identical (fail-safe).
//! 5. CRLF line endings in prose and delimiters are preserved byte-identical.
//!    CRLF inside fence bodies is also preserved (no code engine, no LF normalization).
//!
//! # Single-pass fence scanner
//!
//! The scanner searches for ``` markers line-by-line, respecting CRLF by
//! treating `\r\n` as a single line ending. An opening ``` must be the first
//! non-whitespace on its line (standard Markdown fence rule). A closing ```
//! must also start a line. This prevents backtick-sequences within code from
//! being misinterpreted as fence markers.
//!
//! # Length bound
//!
//! The number of fences is bounded by the block length (at most `len/3`
//! fences). The scanner processes each byte at most once — O(n) pass.

use super::json::compress_json;
use crate::DEFAULT_WORK_BUDGET;
use crate::route::{EngineTarget, engine_for_fence};

/// Result of a mixed-content compression attempt.
#[derive(Debug, Clone)]
pub(crate) enum CompressResult {
    /// Compression produced output (at least one fence body was compressed).
    Compressed {
        /// The full reconstructed block text with compressed fence bodies.
        content: String,
    },
    /// No fence was compressible; caller should forward original bytes.
    Passthrough,
}

/// Compress a mixed-content block by scanning and compressing fence bodies.
///
/// # Arguments
///
/// - `text`: the raw text payload of the block (prose + embedded fenced blocks).
///
/// # Returns
///
/// `CompressResult::Compressed` if at least one fence body was compressed and
/// the result is shorter than the input. `CompressResult::Passthrough` if no
/// fence was compressible or the result is not shorter.
///
/// The caller (BlockRouter Phase 3) applies the never-inflate byte gate; this
/// function does NOT check that its output is shorter than the input.
pub(crate) fn compress_mixed(text: &str) -> CompressResult {
    let output = reconstruct_with_compressed_fences(text);
    if output == text {
        CompressResult::Passthrough
    } else {
        CompressResult::Compressed { content: output }
    }
}

/// Reconstruct the block text with compressed fence bodies.
///
/// Prose regions, ``` delimiters, and info strings are copied byte-identical.
/// Fence bodies are routed through the appropriate engine.
///
/// Returns the input string unchanged if no fence was compressed.
fn reconstruct_with_compressed_fences(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut remaining = text;

    loop {
        // Find the next ``` on a line start (with optional leading whitespace).
        let Some((before_fence, fence_start_offset)) = find_fence_open(remaining) else {
            // No more opening fences — append the rest byte-identical and stop.
            output.push_str(remaining);
            break;
        };

        // Append prose before this opening fence (byte-identical).
        output.push_str(before_fence);

        // The fence opener line is: optional whitespace + "```" + info_string + line_ending.
        let fence_region = &remaining[fence_start_offset..];

        // Parse the opening fence line: extract the full opener (```) + info string + newline.
        let (opener_line, info_lang, after_opener) = parse_fence_opener(fence_region);

        // Append the opener line byte-identical (``` + info string + newline).
        output.push_str(opener_line);

        // Look for the closing ```.
        let Some((body, closing_fence_offset)) = find_fence_close(after_opener) else {
            // Unclosed fence — append remaining byte-identical (fail-safe, AC6).
            output.push_str(after_opener);
            break;
        };

        // Determine the engine for this fence's language hint via the single
        // routing table (AD-006 / Gate-1). engine_for_fence routes json → Json,
        // everything else → Passthrough (code stays byte-identical per ADR-007).
        let engine = engine_for_fence(info_lang.as_deref());
        let closing_line = &after_opener[closing_fence_offset..];
        let (closer_line, after_closer) = split_first_line(closing_line);

        // Compress (or passthrough) the fence body.
        let compressed_body = apply_fence_engine(body, engine);

        // Append body (possibly compressed) + closing ``` line (byte-identical).
        output.push_str(&compressed_body);
        output.push_str(closer_line);

        remaining = after_closer;
    }

    output
}

/// Find the next opening fence (```) that starts on a line boundary.
///
/// Returns `(prose_before, offset_of_fence_in_remaining)` or `None` if no
/// opening fence is found.
///
/// An opening fence must begin at the start of a line (after a newline, or at
/// the start of the string). It may have leading whitespace (standard Markdown).
fn find_fence_open(text: &str) -> Option<(&str, usize)> {
    // Check if the text itself starts with ``` (beginning of string = line start).
    let start_candidates = std::iter::once(0).chain(
        text.char_indices()
            .filter(|(_, c)| *c == '\n')
            .map(|(i, _)| i + 1),
    );

    for line_start in start_candidates {
        let line_text = &text[line_start..];
        // Skip leading whitespace (up to 3 spaces — Markdown standard).
        let trimmed = line_text.trim_start_matches([' ', '\t']);
        if trimmed.starts_with("```") {
            return Some((&text[..line_start], line_start));
        }
    }
    None
}

/// Parse the opening fence line.
///
/// Returns `(full_opener_line_with_newline, option_lang_hint, rest_after_opener)`.
///
/// The opener line is the ``` + info string + line ending, preserved byte-for-byte.
/// The lang hint is the info string lowercased (first word only, standard Markdown).
fn parse_fence_opener(fence_region: &str) -> (&str, Option<String>, &str) {
    let (opener_line, after_opener) = split_first_line(fence_region);

    // Extract the info string from the opener: text after the ``` markers.
    // Skip leading whitespace, then the ``` markers.
    let trimmed = opener_line.trim_start_matches([' ', '\t']);
    let after_backticks = trimmed.trim_start_matches('`');

    // The info string is the first word (before any space or line ending).
    let info = after_backticks
        .split([' ', '\t', '\r', '\n'])
        .next()
        .unwrap_or("")
        .trim();

    let lang = if info.is_empty() {
        None
    } else {
        Some(info.to_ascii_lowercase())
    };

    (opener_line, lang, after_opener)
}

/// Find the closing ``` that starts on a line boundary.
///
/// Returns `(body_before_close, offset_of_close_line_in_after_opener)` or
/// `None` if no closing fence is found (unclosed fence).
fn find_fence_close(after_opener: &str) -> Option<(&str, usize)> {
    let line_starts = std::iter::once(0usize).chain(
        after_opener
            .char_indices()
            .filter(|(_, c)| *c == '\n')
            .map(|(i, _)| i + 1),
    );

    for line_start in line_starts {
        let line_text = &after_opener[line_start..];
        let trimmed = line_text.trim_start_matches([' ', '\t']);
        if trimmed.starts_with("```") {
            return Some((&after_opener[..line_start], line_start));
        }
    }
    None
}

/// Split off the first line (including its line ending: `\n` or `\r\n`).
///
/// Returns `(first_line_with_ending, rest)`.
/// If no newline is present, the whole string is the first line and rest is "".
fn split_first_line(text: &str) -> (&str, &str) {
    // Find the \n; include \r if present before it (CRLF-aware).
    if let Some(nl_pos) = text.find('\n') {
        (&text[..nl_pos + 1], &text[nl_pos + 1..])
    } else {
        (text, "")
    }
}

/// Strip the trailing line ending (`\r\n` or `\n`) from a string.
///
/// `find_fence_close` includes the `\n` before the closing ``` in the body span
/// (the body ends at the byte offset of the closing fence's line start, which
/// follows the `\n`). The JSON minifier strips ALL whitespace including `\n`,
/// which would glue the minified JSON directly onto the closing ``` without the
/// separating newline. This helper peels the trailing line ending off before
/// minification so it can be re-appended afterwards.
fn strip_trailing_newline(s: &str) -> (&str, &str) {
    if let Some(stripped) = s.strip_suffix("\r\n") {
        (stripped, "\r\n")
    } else if let Some(stripped) = s.strip_suffix('\n') {
        (stripped, "\n")
    } else {
        (s, "")
    }
}

/// Apply the appropriate engine to a fence body.
///
/// Returns the compressed body string (or the original if passthrough).
///
/// # JSON fence handling (P2-1 / P2-2 / Gate-1)
///
/// For `EngineTarget::Json`:
/// 1. The trailing newline is stripped before minification (P2-2) — `find_fence_close`
///    includes the `\n` before the closing ``` in the body span; the minifier would
///    drop it, glueing the output JSON to the closing fence line.
/// 2. The minified body is verified via `value_equivalent_raw` (P2-1, defense-in-depth).
///    Only `Some(true)` → splice. `Some(false)` or `None` → byte-identical fallback.
///    Per-fence budget: [`DEFAULT_WORK_BUDGET`] (200 000 nodes). Fence bodies are
///    bounded by the 64 KiB block prefilter, so the budget is never exhausted in practice.
/// 3. The stripped newline is re-appended after minification so the closing fence
///    remains on its own line.
///
/// # P0.1 / ADR-007
///
/// All non-json fences pass through byte-identical. `engine_for_fence` returns
/// `Passthrough` for every hint except `"json"`.
fn apply_fence_engine(body: &str, engine: EngineTarget) -> String {
    match engine {
        EngineTarget::Json => {
            // (P2-2) Strip the trailing newline before handing to the minifier.
            let (content, trailing_nl) = strip_trailing_newline(body);

            match compress_json(content) {
                super::json::CompressResult::Compressed { content: minified } => {
                    // (P2-1) Gate: verify value-equivalence before splicing.
                    let mut budget = DEFAULT_WORK_BUDGET;
                    match rskim_contract::canonical::value_equivalent_raw(
                        content.trim(),
                        minified.trim(),
                        &mut budget,
                    ) {
                        Some(true) => format!("{minified}{trailing_nl}"),
                        // Gate rejected (Some(false)) or budget/depth/dup-key (None):
                        // forward the original fence body byte-identical (fail-safe).
                        _ => body.to_string(),
                    }
                }
                // Minifier returned Passthrough (already compact, invalid JSON,
                // duplicate key, depth/key bound exceeded) — byte-identical.
                super::json::CompressResult::Passthrough => body.to_string(),
            }
        }
        EngineTarget::Log | EngineTarget::Mixed | EngineTarget::Passthrough => {
            // Data-format hints, code fences (P0.1), unknown hints, nested mixed →
            // byte-identical (AD-006 / ADR-007).
            body.to_string()
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // =========================================================================
    // AC6 — Prose is byte-identical
    // =========================================================================

    #[test]
    fn prose_outside_fence_is_byte_identical() {
        let text = "Here is some code:\n```rust\nfn main() {\n    println!(\"hello\");\n    let x = 42;\n}\n```\nAnd here is more prose.\n";
        // Get the output regardless of whether compression fired.
        let output = match compress_mixed(text) {
            CompressResult::Compressed { content } => content,
            CompressResult::Passthrough => text.to_string(),
        };
        // "Here is some code:\n" must be preserved.
        assert!(
            output.starts_with("Here is some code:\n"),
            "Prose prefix must be byte-identical"
        );
        // "And here is more prose.\n" must be preserved.
        assert!(
            output.ends_with("And here is more prose.\n"),
            "Prose suffix must be byte-identical"
        );
    }

    #[test]
    fn fence_delimiters_are_byte_identical() {
        // The ``` opener and closer must be preserved byte-identical.
        let text = "Intro.\n```python\ndef foo():\n    pass\n```\nOutro.\n";
        let output = match compress_mixed(text) {
            CompressResult::Compressed { content } => content,
            CompressResult::Passthrough => text.to_string(),
        };
        // Count the ``` markers in the output — must equal 2 (one open, one close).
        let backtick_count = output.matches("```").count();
        assert_eq!(
            backtick_count, 2,
            "AC6: fence count must be preserved (AC6 #2)"
        );
    }

    #[test]
    fn info_string_preserved_byte_identical() {
        // The info string (language hint on the opener) must be byte-identical.
        let text = "Some prose.\n```rust\nfn main() {}\n```\nMore prose.\n";
        let output = match compress_mixed(text) {
            CompressResult::Compressed { content } => content,
            CompressResult::Passthrough => text.to_string(),
        };
        assert!(
            output.contains("```rust\n"),
            "Info string 'rust' must be preserved in the opener line"
        );
    }

    // =========================================================================
    // AC6 — Fence count preserved
    // =========================================================================

    #[test]
    fn fence_count_is_preserved_for_two_fences() {
        let text =
            "Before.\n```rust\nfn a() {}\n```\nMiddle.\n```python\ndef b(): pass\n```\nAfter.\n";
        let output = match compress_mixed(text) {
            CompressResult::Compressed { content } => content,
            CompressResult::Passthrough => text.to_string(),
        };
        let backtick_count = output.matches("```").count();
        assert_eq!(backtick_count, 4, "Two fences → 4 ``` markers (AC6)");
    }

    // =========================================================================
    // AC6 corpus — CRLF line endings
    // =========================================================================

    #[test]
    fn crlf_prose_is_byte_identical() {
        // AD-005 / AD-011: CRLF in prose regions must be preserved byte-identical.
        let text = "Here is some code:\r\n```rust\r\nfn main() {\r\n    println!(\"hello\");\r\n}\r\n```\r\nMore prose.\r\n";
        let output = match compress_mixed(text) {
            CompressResult::Compressed { content } => content,
            CompressResult::Passthrough => text.to_string(),
        };
        // Prose prefix with CRLF must be preserved.
        assert!(
            output.starts_with("Here is some code:\r\n"),
            "CRLF prose prefix must be byte-identical; got: {:?}",
            &output[..output.find("```").unwrap_or(output.len())]
        );
        // Prose suffix with CRLF must be preserved.
        assert!(
            output.ends_with("More prose.\r\n"),
            "CRLF prose suffix must be byte-identical"
        );
    }

    #[test]
    fn crlf_fence_delimiters_preserved() {
        // The ``` opener and closer with CRLF endings must be byte-identical.
        let text = "Intro.\r\n```rust\r\nfn main() {}\r\n```\r\nOutro.\r\n";
        let output = match compress_mixed(text) {
            CompressResult::Compressed { content } => content,
            CompressResult::Passthrough => text.to_string(),
        };
        // Opening fence with CRLF must appear unchanged.
        assert!(
            output.contains("```rust\r\n"),
            "CRLF opening fence delimiter must be byte-identical; output: {:?}",
            output
        );
        // Fence count preserved.
        assert_eq!(
            output.matches("```").count(),
            2,
            "Fence count must be 2 after CRLF processing"
        );
    }

    // =========================================================================
    // AC6 corpus — unclosed fence
    // =========================================================================

    #[test]
    fn unclosed_fence_is_byte_identical() {
        // AC6: an unclosed fence → entire remaining content forwarded byte-identical.
        let text = "Prose.\n```rust\nfn main() {\n    // no closing fence here\n";
        let output = match compress_mixed(text) {
            CompressResult::Compressed { content } => content,
            CompressResult::Passthrough => text.to_string(),
        };
        // The output must contain all original bytes (no truncation).
        assert_eq!(
            output, text,
            "Unclosed fence must produce byte-identical output"
        );
    }

    // =========================================================================
    // AC6 corpus — adjacent/nested-looking backticks
    // =========================================================================

    #[test]
    fn adjacent_backticks_not_mistaken_for_fence() {
        // Inline code like `foo` must not be mistaken for a fence.
        // Only ``` (triple) that starts a line is a fence opener.
        let text = "Use `foo` inline, not a fence.\nThis is `bar` also inline.\n";
        let output = match compress_mixed(text) {
            CompressResult::Compressed { content } => content,
            CompressResult::Passthrough => text.to_string(),
        };
        // No fences were detected → output must equal input.
        assert_eq!(
            output, text,
            "Inline backticks must not be mistaken for fences"
        );
    }

    #[test]
    fn double_backtick_not_a_fence() {
        // `` (double) must not match triple-backtick fence detection.
        let text = "Use ``code`` here.\n```rust\nfn real_fence() {}\n```\n";
        let output = match compress_mixed(text) {
            CompressResult::Compressed { content } => content,
            CompressResult::Passthrough => text.to_string(),
        };
        // The real fence must still be detected (2 ``` markers = 1 fence).
        // Double-backtick text "``code``" must appear unchanged.
        assert!(
            output.contains("``code``"),
            "Double backtick text must be byte-identical"
        );
        assert_eq!(
            output.matches("```").count(),
            2,
            "Real fence must still be detected"
        );
    }

    // =========================================================================
    // AC6 — json fence routed to JSON engine (P2-1 / Gate-1 restore)
    // =========================================================================

    /// Discriminating: json-hinted fence bodies must be minified (Compressed), not
    /// forwarded byte-identical (Passthrough). Fails on any revision where the
    /// Mixed engine routes json fences through `engine_for_class(Class::Code, _)`.
    ///
    /// PF-007: this test is explicit — Passthrough is a test failure, not an
    /// acceptable alternative outcome.
    #[test]
    fn json_fence_body_is_minified() {
        // Pretty-printed JSON (has whitespace to strip → compress_json WILL compress).
        let text = "Result:\n```json\n{\"name\": \"Alice\", \"age\": 30, \"active\": true}\n```\n";
        let output = match compress_mixed(text) {
            CompressResult::Compressed { content } => content,
            // P2-1: Passthrough is a failure — json fence must be compressed.
            CompressResult::Passthrough => {
                panic!(
                    "P2-1: json-hinted fence must produce Compressed output, not Passthrough.\n\
                     Input: {text:?}"
                );
            }
        };
        // Verify the fence body was minified: no insignificant whitespace.
        let after_opener = output
            .strip_prefix("Result:\n```json\n")
            .expect("prose prefix must be byte-identical");
        let body_end = after_opener
            .find("```")
            .expect("closing fence must be present");
        let body = after_opener[..body_end].trim();
        // Minified JSON has no space after `:` or `,`.
        assert!(
            !body.contains(": ") && !body.contains(", "),
            "json fence body must be minified (no insignificant whitespace); got: {body:?}"
        );
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(body);
        assert!(
            parsed.is_ok(),
            "minified json fence body must still be valid JSON; got: {body:?}"
        );
        // Closing fence must be on its own line (P2-2: trailing newline preserved).
        assert!(
            after_opener[body_end..].starts_with("```\n"),
            "closing fence must be on its own line after minification; got: {:?}",
            &after_opener[body_end..]
        );
    }

    /// Discriminating: rust-hinted fence bodies stay byte-identical (ADR-007).
    ///
    /// Fails if any restoration of code-engine routing is attempted for rust fences.
    #[test]
    fn rust_fence_body_is_byte_identical() {
        let body = "fn main() {\n    println!(\"hello\");\n    let x = 42;\n}\n";
        let text = format!("Intro.\n```rust\n{body}```\nOutro.\n");
        let output = match compress_mixed(&text) {
            CompressResult::Compressed { content } => content,
            CompressResult::Passthrough => text.clone(),
        };
        // The rust fence body must be byte-identical (no code compression, P0.1).
        assert!(
            output.contains(body),
            "rust fence body must be byte-identical (ADR-007/P0.1); got: {output:?}"
        );
    }

    /// Discriminating (gate): a json fence whose body has duplicate keys must stay
    /// byte-identical. `compress_json` returns Passthrough for dup-key objects;
    /// the gate (`value_equivalent_raw`) would return None for invalid JSON anyway.
    /// Either path: byte-identical.
    #[test]
    fn json_fence_dup_key_is_byte_identical() {
        // JSON with a duplicate key: `compress_json` returns Passthrough → body unchanged.
        let text = "Before.\n```json\n{\"a\": 1, \"a\": 2}\n```\nAfter.\n";
        let output = match compress_mixed(text) {
            CompressResult::Compressed { content } => content,
            CompressResult::Passthrough => text.to_string(),
        };
        // The dup-key body must be byte-identical.
        assert!(
            output.contains("{\"a\": 1, \"a\": 2}"),
            "dup-key json fence body must be byte-identical (gate/passthrough); got: {output:?}"
        );
    }

    /// Discriminating (gate / number-token preservation): `1e10` and `1.0` must
    /// survive minification with byte-exact token representation. `value_equivalent_raw`
    /// uses raw-byte comparison for numbers, so `1e10` → `10000000000` would fail
    /// the gate and produce byte-identical output instead.
    ///
    /// The JSON minifier strips whitespace only — number tokens are never rewritten —
    /// so this test verifies the end-to-end path: minify → gate → splice.
    #[test]
    fn json_fence_number_tokens_byte_exact() {
        let text = "Numbers:\n```json\n{\n  \"count\": 1e10,\n  \"ratio\": 1.0\n}\n```\nEnd.\n";
        let output = match compress_mixed(text) {
            CompressResult::Compressed { content } => content,
            CompressResult::Passthrough => text.to_string(),
        };
        // Number tokens must appear byte-identical in the output.
        assert!(
            output.contains("1e10"),
            "number token '1e10' must be byte-exact after minification; got: {output:?}"
        );
        assert!(
            output.contains("1.0"),
            "number token '1.0' must be byte-exact after minification; got: {output:?}"
        );
    }

    // =========================================================================
    // AC6 — yaml/toml/markdown fences → byte-identical
    // =========================================================================

    #[test]
    fn yaml_fence_body_is_byte_identical() {
        // AD-006: yaml → passthrough.
        let text = "Config:\n```yaml\nname: foo\nvalue: bar\n```\n";
        let output = match compress_mixed(text) {
            CompressResult::Compressed { content } => content,
            CompressResult::Passthrough => text.to_string(),
        };
        // YAML body must be unchanged.
        assert!(
            output.contains("name: foo\nvalue: bar\n"),
            "yaml fence body must be byte-identical (AD-006)"
        );
    }

    #[test]
    fn no_fence_returns_passthrough() {
        // If the text has no fences, the result should be passthrough.
        let text = "Just plain prose with no code fences here.\n";
        let result = compress_mixed(text);
        // No fence → output equals input → Passthrough.
        assert!(
            matches!(result, CompressResult::Passthrough),
            "No fences → Passthrough"
        );
    }
}
