//! Three-tier parse degradation output (#41)
//!
//! Provides ParseResult enum (Full/Degraded/Passthrough), output cleaning
//! (ANSI stripping, progress line collapsing, deduplication), token-aware
//! truncation, and filter transparency headers.

// Infrastructure module — consumers arrive in later Phase B tickets.
#![allow(dead_code)]

pub(crate) mod canonical;
pub(crate) mod guardrail;
pub(crate) mod tee;

use std::io::{self, Write};

use crate::tokens;

// ============================================================================
// ParseResult<T> — three-tier parse degradation
// ============================================================================

/// Result of parsing external process output through three degradation tiers.
///
/// - `Full`: clean parse, no issues
/// - `Degraded`: partially parsed with warning markers
/// - `Passthrough`: unparseable, returned as-is (always `String`)
#[derive(Debug, Clone)]
pub(crate) enum ParseResult<T> {
    Full(T),
    Degraded(T, Vec<String>),
    /// Always `String` regardless of `T` — content could not be parsed.
    Passthrough(String),
}

impl<T> ParseResult<T> {
    /// Returns `true` if this is a `Full` result.
    pub(crate) fn is_full(&self) -> bool {
        matches!(self, ParseResult::Full(_))
    }

    /// Returns `true` if this is a `Degraded` result.
    pub(crate) fn is_degraded(&self) -> bool {
        matches!(self, ParseResult::Degraded(_, _))
    }

    /// Returns `true` if this is a `Passthrough` result.
    pub(crate) fn is_passthrough(&self) -> bool {
        matches!(self, ParseResult::Passthrough(_))
    }

    /// Returns the tier name as a static string.
    pub(crate) fn tier_name(&self) -> &'static str {
        match self {
            ParseResult::Full(_) => "full",
            ParseResult::Degraded(_, _) => "degraded",
            ParseResult::Passthrough(_) => "passthrough",
        }
    }
}

impl<T: AsRef<str>> ParseResult<T> {
    /// Read access to inner content for all tiers.
    pub(crate) fn content(&self) -> &str {
        match self {
            ParseResult::Full(inner) | ParseResult::Degraded(inner, _) => inner.as_ref(),
            ParseResult::Passthrough(s) => s.as_str(),
        }
    }

    /// Write degradation markers to the given writer.
    ///
    /// - `Full`: writes nothing
    /// - `Degraded`: writes each marker as a warning line
    /// - `Passthrough`: writes a notice line
    pub(crate) fn emit_markers(&self, writer: &mut impl Write) -> io::Result<()> {
        match self {
            ParseResult::Full(_) => Ok(()),
            ParseResult::Degraded(_, markers) => {
                for marker in markers {
                    writeln!(writer, "[warning] {marker}")?;
                }
                Ok(())
            }
            ParseResult::Passthrough(_) => {
                writeln!(writer, "[notice] output passed through without parsing")
            }
        }
    }
}

impl<T: Into<String>> ParseResult<T> {
    /// Consuming access to inner content as `String`.
    pub(crate) fn into_content(self) -> String {
        match self {
            ParseResult::Full(inner) | ParseResult::Degraded(inner, _) => inner.into(),
            ParseResult::Passthrough(s) => s,
        }
    }
}

// ============================================================================
// OutputMode
// ============================================================================

/// Controls how much cleaning is applied to passthrough output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputMode {
    /// Full cleaning pipeline: ANSI strip + progress collapse + deduplication.
    Compact,
    /// Minimal cleaning: ANSI strip only (preserves progress lines and duplicates).
    Verbose,
}

impl OutputMode {
    /// Derive output mode from a process exit code.
    ///
    /// - `0` → `Compact` (success, clean output)
    /// - non-zero → `Verbose` (failure, preserve everything for debugging)
    pub(crate) fn from_exit_code(code: i32) -> Self {
        if code == 0 {
            OutputMode::Compact
        } else {
            OutputMode::Verbose
        }
    }

    /// Derive output mode from an optional exit code.
    ///
    /// - `Some(code)` → delegates to `from_exit_code`
    /// - `None` (signal kill, no exit code) → `Verbose`
    pub(crate) fn from_optional_exit_code(code: Option<i32>) -> Self {
        code.map_or(OutputMode::Verbose, Self::from_exit_code)
    }
}

// ============================================================================
// PassthroughCleaner — module-level functions
// ============================================================================

/// Strip ANSI escape sequences from the input string.
///
/// Delegates to `strip_ansi_escapes::strip_str()`.
pub(crate) fn strip_ansi(input: &str) -> String {
    strip_ansi_escapes::strip_str(input)
}

/// Collapse progress lines that use carriage return (`\r`) overwriting.
///
/// Terminal progress bars use `\r` (without `\n`) to overwrite the current line.
/// This function keeps only the last segment of each `\r`-separated group.
///
/// 1. Normalizes `\r\n` to `\n` first (Windows line endings are real newlines)
/// 2. Splits on `\n`
/// 3. For each line containing bare `\r`: splits on `\r`, keeps last non-empty segment
/// 4. Rejoins with `\n`
pub(crate) fn collapse_progress_lines(input: &str) -> String {
    // Normalize \r\n to \n so Windows line endings are not treated as progress
    let normalized = input.replace("\r\n", "\n");

    normalized
        .split('\n')
        .map(|line| {
            if line.contains('\r') {
                line.split('\r').rfind(|s| !s.is_empty()).unwrap_or("")
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Deduplicate consecutive identical lines, preserving blank lines between
/// different content blocks.
pub(crate) fn deduplicate_consecutive_lines(input: &str) -> String {
    let mut result = Vec::new();
    let mut prev: Option<&str> = None;

    for line in input.split('\n') {
        match prev {
            Some(previous) if previous == line && !line.is_empty() => {
                // Skip consecutive duplicate (non-blank) line
            }
            _ => {
                result.push(line);
            }
        }
        prev = Some(line);
    }

    result.join("\n")
}

/// Full cleaning pipeline: collapse progress → strip ANSI → deduplicate.
///
/// The order matters: progress line collapsing must happen before ANSI stripping
/// because `strip_ansi_escapes` drops `\r` bytes (they are C0 control codes).
/// If ANSI were stripped first, there would be no `\r` for progress collapsing.
pub(crate) fn clean(input: &str) -> String {
    let collapsed = collapse_progress_lines(input);
    let stripped = strip_ansi(&collapsed);
    deduplicate_consecutive_lines(&stripped)
}

/// Clean with mode-dependent behavior.
///
/// - `Compact`: full pipeline (collapse progress → strip ANSI → deduplicate)
/// - `Verbose`: ANSI strip only (preserves progress lines and duplicates for debugging)
pub(crate) fn clean_with_mode(input: &str, mode: OutputMode) -> String {
    match mode {
        OutputMode::Compact => clean(input),
        OutputMode::Verbose => strip_ansi(input),
    }
}

// ============================================================================
// PassthroughTruncator
// ============================================================================

/// Token-aware truncation of passthrough content.
pub(crate) struct PassthroughTruncator;

impl PassthroughTruncator {
    /// Truncate content to fit within a token budget.
    ///
    /// Algorithm:
    /// - Split into lines, accumulate tokens line by line (O(n) forward scan)
    /// - Stop when budget is reached, append `\n[... truncated N lines]`
    /// - Edge case: single very long line → byte-level binary search for
    ///   approximate token boundary
    /// - Edge case: `token_budget == 0` → return just the truncation marker
    pub(crate) fn truncate_to_budget(content: &str, token_budget: usize) -> anyhow::Result<String> {
        // Edge case: zero budget
        if token_budget == 0 {
            let total_lines = content.lines().count();
            if total_lines == 0 && content.is_empty() {
                return Ok(String::new());
            }
            return Ok(format!("\n[... truncated {total_lines} lines]"));
        }

        // Check if content already fits
        let total_tokens = tokens::count_tokens(content)?;
        if total_tokens <= token_budget {
            return Ok(content.to_string());
        }

        let lines: Vec<&str> = content.split('\n').collect();

        // Edge case: single line (no \n breaks) that exceeds budget
        if lines.len() == 1 {
            return Self::truncate_single_line(content, token_budget);
        }

        // Forward scan: accumulate tokens line by line
        let mut accumulated_tokens: usize = 0;
        let mut kept_lines: Vec<&str> = Vec::new();

        for (i, line) in lines.iter().enumerate() {
            // Count tokens for this line (include the \n separator except for last line)
            let line_with_newline = if i < lines.len() - 1 {
                format!("{line}\n")
            } else {
                line.to_string()
            };
            let line_tokens = tokens::count_tokens(&line_with_newline)?;

            if accumulated_tokens + line_tokens > token_budget {
                // This line would exceed budget
                let truncated_count = lines.len() - kept_lines.len();
                let mut result = kept_lines.join("\n");
                result.push_str(&format!("\n[... truncated {truncated_count} lines]"));
                return Ok(result);
            }

            accumulated_tokens += line_tokens;
            kept_lines.push(line);
        }

        // Should not reach here given the early total_tokens check, but be safe
        Ok(content.to_string())
    }

    /// Truncate a single long line using byte-level binary search to find
    /// the approximate token boundary.
    fn truncate_single_line(line: &str, token_budget: usize) -> anyhow::Result<String> {
        let bytes = line.as_bytes();
        let mut lo: usize = 0;
        let mut hi: usize = bytes.len();
        let mut best: usize = 0;

        while lo <= hi {
            let mid = lo + (hi - lo) / 2;

            // Find a valid UTF-8 char boundary at or before `mid`
            let boundary = Self::floor_char_boundary(line, mid);

            let substr = &line[..boundary];
            let count = tokens::count_tokens(substr)?;

            if count <= token_budget {
                best = boundary;
                if mid == bytes.len() || boundary == bytes.len() {
                    break;
                }
                lo = mid + 1;
            } else {
                if mid == 0 {
                    break;
                }
                hi = mid.saturating_sub(1);
            }
        }

        let mut result = line[..best].to_string();
        result.push_str("\n[... truncated 1 lines]");
        Ok(result)
    }

    /// Find the largest char boundary <= `index` in the string.
    fn floor_char_boundary(s: &str, index: usize) -> usize {
        if index >= s.len() {
            return s.len();
        }
        // Walk backwards to find a valid char boundary
        let mut i = index;
        while i > 0 && !s.is_char_boundary(i) {
            i -= 1;
        }
        i
    }
}

// ============================================================================
// FilterTransparencyHeader
// ============================================================================

/// Emits filter transparency headers to stderr for observability.
///
/// Format: `[skim:{filter_name}:{tier}] {input} → {output} ({savings}%)`
pub(crate) struct FilterTransparencyHeader;

impl FilterTransparencyHeader {
    /// Emit a filter transparency header to the given writer (DI for testing).
    pub(crate) fn emit_to(
        writer: &mut impl Write,
        filter_name: &str,
        tier: &str,
        input_tokens: usize,
        output_tokens: usize,
    ) -> io::Result<()> {
        let savings = if input_tokens == 0 || output_tokens >= input_tokens {
            0
        } else {
            // Use u128 to avoid overflow when (input - output) * 100 exceeds usize::MAX.
            ((input_tokens - output_tokens) as u128 * 100 / input_tokens as u128) as usize
        };

        writeln!(
            writer,
            "[skim:{filter_name}:{tier}] {} \u{2192} {} ({savings}%)",
            tokens::format_number(input_tokens),
            tokens::format_number(output_tokens),
        )
    }

    /// Convenience wrapper: emit to stderr, locking once for the whole write.
    pub(crate) fn emit(filter_name: &str, tier: &str, input_tokens: usize, output_tokens: usize) {
        let stderr = io::stderr();
        let mut handle = stderr.lock();
        // Best-effort: ignore write errors to stderr
        let _ = Self::emit_to(&mut handle, filter_name, tier, input_tokens, output_tokens);
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // ParseResult tests
    // ========================================================================

    #[test]
    fn test_full_is_full() {
        let result: ParseResult<String> = ParseResult::Full("hello".to_string());
        assert!(result.is_full());
        assert!(!result.is_degraded());
        assert!(!result.is_passthrough());
    }

    #[test]
    fn test_degraded_is_degraded() {
        let markers = vec!["missing field".to_string(), "bad format".to_string()];
        let result: ParseResult<String> =
            ParseResult::Degraded("partial".to_string(), markers.clone());
        assert!(result.is_degraded());
        assert!(!result.is_full());
        assert!(!result.is_passthrough());
        // Markers are accessible
        if let ParseResult::Degraded(_, m) = &result {
            assert_eq!(m.len(), 2);
            assert_eq!(m[0], "missing field");
            assert_eq!(m[1], "bad format");
        } else {
            panic!("expected Degraded variant");
        }
    }

    #[test]
    fn test_passthrough_is_passthrough() {
        let result: ParseResult<String> = ParseResult::Passthrough("raw output".to_string());
        assert!(result.is_passthrough());
        assert!(!result.is_full());
        assert!(!result.is_degraded());
    }

    #[test]
    fn test_tier_names() {
        let full: ParseResult<String> = ParseResult::Full("a".to_string());
        let degraded: ParseResult<String> =
            ParseResult::Degraded("b".to_string(), vec!["w".to_string()]);
        let passthrough: ParseResult<String> = ParseResult::Passthrough("c".to_string());

        assert_eq!(full.tier_name(), "full");
        assert_eq!(degraded.tier_name(), "degraded");
        assert_eq!(passthrough.tier_name(), "passthrough");
    }

    #[test]
    fn test_content_accessor() {
        let full: ParseResult<String> = ParseResult::Full("full content".to_string());
        let degraded: ParseResult<String> =
            ParseResult::Degraded("degraded content".to_string(), vec![]);
        let passthrough: ParseResult<String> =
            ParseResult::Passthrough("passthrough content".to_string());

        assert_eq!(full.content(), "full content");
        assert_eq!(degraded.content(), "degraded content");
        assert_eq!(passthrough.content(), "passthrough content");
    }

    #[test]
    fn test_into_content() {
        let full: ParseResult<String> = ParseResult::Full("full".to_string());
        assert_eq!(full.into_content(), "full");

        let degraded: ParseResult<String> =
            ParseResult::Degraded("degraded".to_string(), vec!["m".to_string()]);
        assert_eq!(degraded.into_content(), "degraded");

        let passthrough: ParseResult<String> = ParseResult::Passthrough("passthrough".to_string());
        assert_eq!(passthrough.into_content(), "passthrough");
    }

    #[test]
    fn test_emit_markers_full_is_noop() {
        let result: ParseResult<String> = ParseResult::Full("ok".to_string());
        let mut buf = Vec::new();
        result.emit_markers(&mut buf).unwrap();
        assert!(buf.is_empty(), "Full should write nothing");
    }

    #[test]
    fn test_emit_markers_degraded_writes_warnings() {
        let markers = vec!["issue one".to_string(), "issue two".to_string()];
        let result: ParseResult<String> = ParseResult::Degraded("content".to_string(), markers);
        let mut buf = Vec::new();
        result.emit_markers(&mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("[warning] issue one"),
            "expected warning for first marker, got: {output}"
        );
        assert!(
            output.contains("[warning] issue two"),
            "expected warning for second marker, got: {output}"
        );
    }

    #[test]
    fn test_emit_markers_passthrough_writes_notice() {
        let result: ParseResult<String> = ParseResult::Passthrough("raw".to_string());
        let mut buf = Vec::new();
        result.emit_markers(&mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("[notice]"),
            "expected notice in output, got: {output}"
        );
    }

    // ========================================================================
    // PassthroughCleaner tests
    // ========================================================================

    #[test]
    fn test_strip_ansi_removes_color_codes() {
        let input = "\x1b[31mred\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, "red");
    }

    #[test]
    fn test_strip_ansi_preserves_plain_text() {
        let input = "plain text with no escapes";
        let result = strip_ansi(input);
        assert_eq!(result, "plain text with no escapes");
    }

    #[test]
    fn test_strip_ansi_handles_only_escape_codes() {
        // Input that is entirely ANSI escape sequences
        let input = "\x1b[31m\x1b[1m\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, "");
    }

    #[test]
    fn test_collapse_progress_single_cr_line() {
        // "a\rb\rc" → "c" (last segment wins)
        let input = "a\rb\rc";
        let result = collapse_progress_lines(input);
        assert_eq!(result, "c");
    }

    #[test]
    fn test_collapse_progress_multiline_mixed() {
        // Lines with \r get collapsed, lines without are preserved
        let input = "normal line\nfoo\rbar\rbaz\nanother normal";
        let result = collapse_progress_lines(input);
        assert_eq!(result, "normal line\nbaz\nanother normal");
    }

    #[test]
    fn test_collapse_handles_windows_line_endings() {
        // \r\n should be normalized to \n, not treated as progress overwrite
        let input = "line1\r\nline2\r\nline3";
        let result = collapse_progress_lines(input);
        assert_eq!(result, "line1\nline2\nline3");
    }

    #[test]
    fn test_deduplicate_consecutive_identical() {
        let input = "a\na\na\nb\nb\nc";
        let result = deduplicate_consecutive_lines(input);
        assert_eq!(result, "a\nb\nc");
    }

    #[test]
    fn test_deduplicate_preserves_non_consecutive() {
        let input = "a\nb\na";
        let result = deduplicate_consecutive_lines(input);
        assert_eq!(result, "a\nb\na");
    }

    #[test]
    fn test_clean_full_pipeline() {
        // Input has ANSI codes, progress lines with \r, and duplicate lines
        let input = "\x1b[32mhello\x1b[0m\nfoo\rbar\rbaz\nbaz\nbaz\nend";
        let result = clean(input);
        // After collapse: "hello\nbaz\nbaz\nbaz\nend" (after ANSI strip)
        // After dedup: "hello\nbaz\nend"
        assert_eq!(result, "hello\nbaz\nend");
    }

    #[test]
    fn test_clean_with_mode_compact() {
        let input = "\x1b[31mred\x1b[0m\nred\nred";
        let result = clean_with_mode(input, OutputMode::Compact);
        // Strip ANSI → "red\nred\nred", dedup → "red"
        assert_eq!(result, "red");
    }

    #[test]
    fn test_clean_with_mode_verbose() {
        // Verbose: only ANSI stripped, progress and duplicates preserved
        let input = "\x1b[31mred\x1b[0m\nred\nred";
        let result = clean_with_mode(input, OutputMode::Verbose);
        // ANSI stripped only, duplicates preserved
        assert_eq!(result, "red\nred\nred");
    }

    #[test]
    fn test_clean_empty_input() {
        assert_eq!(clean(""), "");
    }

    // ========================================================================
    // PassthroughTruncator tests
    // ========================================================================

    #[test]
    fn test_truncate_within_budget() {
        // Short content should be returned unchanged
        let content = "hello world";
        let budget = 1000;
        let result = PassthroughTruncator::truncate_to_budget(content, budget).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn test_truncate_over_budget_reduces() {
        // Create content that exceeds budget
        let lines: Vec<String> = (0..100)
            .map(|i| format!("This is line number {i} with some extra content to use tokens"))
            .collect();
        let content = lines.join("\n");

        let budget = 20;
        let result = PassthroughTruncator::truncate_to_budget(&content, budget).unwrap();

        // Output tokens should be within budget (approximately — the truncation marker
        // adds a few tokens, but the kept lines should be <= budget)
        let output_tokens = tokens::count_tokens(&result).unwrap();
        // Allow some margin for the truncation marker itself
        assert!(
            output_tokens < budget + 20,
            "output tokens ({output_tokens}) should be close to budget ({budget})"
        );
        assert!(
            result.len() < content.len(),
            "truncated output should be shorter"
        );
    }

    #[test]
    fn test_truncate_appends_marker() {
        let lines: Vec<String> = (0..50)
            .map(|i| format!("Line {i} with enough content to generate several tokens"))
            .collect();
        let content = lines.join("\n");

        let budget = 10;
        let result = PassthroughTruncator::truncate_to_budget(&content, budget).unwrap();

        assert!(
            result.contains("[... truncated"),
            "expected truncation marker in output, got: {result}"
        );
    }

    #[test]
    fn test_truncate_empty_input() {
        let result = PassthroughTruncator::truncate_to_budget("", 100).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_truncate_single_long_line() {
        // A single very long line with no \n
        let long_line = "word ".repeat(500);
        let budget = 10;
        let result = PassthroughTruncator::truncate_to_budget(&long_line, budget).unwrap();

        assert!(
            result.contains("[... truncated"),
            "expected truncation marker for single long line"
        );
        assert!(
            result.len() < long_line.len(),
            "truncated output should be shorter than input"
        );
    }

    // ========================================================================
    // FilterTransparencyHeader tests
    // ========================================================================

    #[test]
    fn test_header_format() {
        let mut buf = Vec::new();
        FilterTransparencyHeader::emit_to(&mut buf, "test-filter", "full", 1000, 200).unwrap();
        let output = String::from_utf8(buf).unwrap();

        assert!(
            output.contains("[skim:test-filter:full]"),
            "expected header prefix, got: {output}"
        );
        assert!(
            output.contains("\u{2192}"),
            "expected Unicode arrow, got: {output}"
        );
        assert!(
            output.contains("80%"),
            "expected 80% savings, got: {output}"
        );
    }

    #[test]
    fn test_header_zero_input_tokens() {
        let mut buf = Vec::new();
        FilterTransparencyHeader::emit_to(&mut buf, "filter", "passthrough", 0, 0).unwrap();
        let output = String::from_utf8(buf).unwrap();

        assert!(
            output.contains("0%"),
            "expected 0% savings for zero input, got: {output}"
        );
        // Should not panic from division by zero
    }

    #[test]
    fn test_header_output_exceeds_input_no_underflow() {
        // When output_tokens > input_tokens, savings should be 0% (not panic from usize underflow)
        let mut buf = Vec::new();
        FilterTransparencyHeader::emit_to(&mut buf, "filter", "degraded", 100, 200).unwrap();
        let output = String::from_utf8(buf).unwrap();

        assert!(
            output.contains("0%"),
            "expected 0% savings when output exceeds input, got: {output}"
        );
    }

    #[test]
    fn test_header_uses_format_number() {
        let mut buf = Vec::new();
        FilterTransparencyHeader::emit_to(&mut buf, "f", "full", 1_500_000, 500_000).unwrap();
        let output = String::from_utf8(buf).unwrap();

        assert!(
            output.contains("1,500,000"),
            "expected thousands-separated input tokens, got: {output}"
        );
        assert!(
            output.contains("500,000"),
            "expected thousands-separated output tokens, got: {output}"
        );
    }

    // ========================================================================
    // OutputMode tests
    // ========================================================================

    #[test]
    fn test_from_exit_code_zero_is_compact() {
        assert_eq!(OutputMode::from_exit_code(0), OutputMode::Compact);
    }

    #[test]
    fn test_from_exit_code_nonzero_is_verbose() {
        assert_eq!(OutputMode::from_exit_code(1), OutputMode::Verbose);
    }

    #[test]
    fn test_from_exit_code_negative_is_verbose() {
        assert_eq!(OutputMode::from_exit_code(-1), OutputMode::Verbose);
    }

    #[test]
    fn test_from_optional_none_is_verbose() {
        assert_eq!(
            OutputMode::from_optional_exit_code(None),
            OutputMode::Verbose
        );
    }

    #[test]
    fn test_from_optional_some_zero_is_compact() {
        assert_eq!(
            OutputMode::from_optional_exit_code(Some(0)),
            OutputMode::Compact
        );
    }

    #[test]
    fn test_from_optional_some_nonzero_is_verbose() {
        assert_eq!(
            OutputMode::from_optional_exit_code(Some(1)),
            OutputMode::Verbose
        );
    }

    #[test]
    fn test_truncate_zero_budget_non_empty() {
        let content = "line one\nline two\nline three";
        let result = PassthroughTruncator::truncate_to_budget(content, 0).unwrap();
        assert!(
            result.contains("[... truncated 3 lines]"),
            "expected truncation marker for 3 lines, got: {result}"
        );
        assert!(
            !result.contains("line one"),
            "expected no original content in zero-budget output, got: {result}"
        );
    }

    // ========================================================================
    // Adversarial PassthroughCleaner tests
    // ========================================================================

    #[test]
    fn test_collapse_trailing_cr() {
        // "hello\r" → last segment after \r is empty, rfind non-empty → "hello"
        assert_eq!(collapse_progress_lines("hello\r"), "hello");
    }

    #[test]
    fn test_collapse_only_cr_no_newline() {
        // "foo\rbar\rbaz" (no \n) → single line, split on \r, last non-empty → "baz"
        assert_eq!(collapse_progress_lines("foo\rbar\rbaz"), "baz");
    }

    #[test]
    fn test_collapse_mixed_cr_crlf() {
        // "foo\rbar\r\nbaz" → normalize \r\n to \n → "foo\rbar\nbaz"
        // First line "foo\rbar" → collapsed to "bar"
        // Second line "baz" → unchanged
        assert_eq!(collapse_progress_lines("foo\rbar\r\nbaz"), "bar\nbaz");
    }

    #[test]
    fn test_collapse_reversed_line_ending() {
        // "\n\r" is NOT a Windows line ending — the \r is on the next line
        // "line1\n\rline2" → normalize (no \r\n found) → split on \n → ["line1", "\rline2"]
        // Second line has \r: split → ["", "line2"], rfind non-empty → "line2"
        assert_eq!(collapse_progress_lines("line1\n\rline2"), "line1\nline2");
    }

    #[test]
    fn test_deduplicate_preserves_blank_lines() {
        // Blank lines should never be deduplicated (they separate content blocks)
        assert_eq!(deduplicate_consecutive_lines("a\n\n\nb"), "a\n\n\nb");
    }

    #[test]
    fn test_deduplicate_trailing_whitespace_differs() {
        // "hello" and "hello " are different strings — both should be kept
        assert_eq!(
            deduplicate_consecutive_lines("hello\nhello \nhello"),
            "hello\nhello \nhello"
        );
    }

    #[test]
    fn test_clean_unicode_with_ansi() {
        let input = "\x1b[31m🦀 hello\x1b[0m";
        assert_eq!(strip_ansi(input), "🦀 hello");
    }

    #[test]
    fn test_clean_8bit_color_codes() {
        // 256-color ANSI: ESC[38;5;196m (foreground color 196 = bright red)
        let input = "\x1b[38;5;196mred\x1b[0m";
        assert_eq!(strip_ansi(input), "red");
    }

    // ========================================================================
    // Adversarial PassthroughTruncator tests
    // ========================================================================

    #[test]
    fn test_truncate_budget_one_token() {
        // With budget of 1, almost all multi-token content should be truncated
        let content = "This is a fairly long line with many words\nAnother line here\nAnd a third";
        let result = PassthroughTruncator::truncate_to_budget(content, 1).unwrap();
        assert!(
            result.contains("[... truncated"),
            "expected truncation marker with budget=1, got: {result}"
        );
        assert!(
            result.len() < content.len(),
            "expected shorter output with budget=1"
        );
    }

    #[test]
    fn test_truncate_unicode_char_boundaries() {
        // Many multi-byte characters — binary search must not split a char
        let content = "🦀".repeat(200);
        let budget = 5;
        let result = PassthroughTruncator::truncate_to_budget(&content, budget).unwrap();
        // Must be valid UTF-8 (would panic on construction if not)
        assert!(
            result.contains("[... truncated"),
            "expected truncation marker for long unicode content"
        );
    }

    #[test]
    fn test_truncate_only_blank_lines() {
        // Input is only newlines — should not panic
        let content = "\n\n\n\n\n";
        let result = PassthroughTruncator::truncate_to_budget(content, 2).unwrap();
        // Either fits (blank lines are cheap tokens) or truncates gracefully
        assert!(!result.is_empty() || content.is_empty());
    }

    #[test]
    fn test_truncate_content_exactly_at_budget() {
        // Content that fits exactly should be returned unchanged
        let content = "hello";
        let tokens = tokens::count_tokens(content).unwrap();
        let result = PassthroughTruncator::truncate_to_budget(content, tokens).unwrap();
        assert_eq!(
            result, content,
            "content at exact budget should be unchanged"
        );
    }

    #[test]
    fn test_truncate_mixed_newline_styles() {
        // Content with \r\n — truncator splits on \n, leaving \r at end of lines
        let content = "line1\r\nline2\r\nline3\r\nline4\r\nline5\r\nline6\r\nline7\r\nline8\r\nline9\r\nline10";
        let result = PassthroughTruncator::truncate_to_budget(content, 5).unwrap();
        // Should not panic and should produce valid output
        assert!(!result.is_empty());
    }

    // ========================================================================
    // Adversarial FilterTransparencyHeader tests
    // ========================================================================

    #[test]
    fn test_header_large_token_no_overflow() {
        // usize::MAX / 2 * 100 would overflow usize — verify no panic
        let mut buf = Vec::new();
        FilterTransparencyHeader::emit_to(&mut buf, "filter", "full", usize::MAX / 2, 0).unwrap();
        let output = String::from_utf8(buf).unwrap();

        // Should produce ~100% savings without panic
        assert!(
            output.contains("100%") || output.contains("99%"),
            "expected ~100% savings for huge input with zero output, got: {output}"
        );
    }
}
