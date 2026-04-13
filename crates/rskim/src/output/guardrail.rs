//! Output guardrail: prevents emitting compressed output that is larger than raw (#53)
//!
//! Two-tier comparison:
//! 1. Byte fast path: if compressed.len() <= raw.len(), pass through
//! 2. Token slow path: count tokens of both, if compressed tokens > raw tokens, trigger
//!
//! When triggered, emits a warning to stderr and returns the raw output instead.

use std::io::{self, Write};

use anyhow::Result;

/// Outcome of the guardrail check
#[derive(Debug)]
pub(crate) enum GuardrailOutcome {
    /// Compressed output is smaller or equal — use it
    Passed { output: String },
    /// Compressed output is larger than raw — use raw instead
    Triggered { output: String },
}

impl GuardrailOutcome {
    /// Returns true if the guardrail was triggered (compressed was larger)
    pub(crate) fn was_triggered(&self) -> bool {
        matches!(self, GuardrailOutcome::Triggered { .. })
    }

    /// Consume the outcome and return the output string
    pub(crate) fn into_output(self) -> String {
        match self {
            GuardrailOutcome::Passed { output } | GuardrailOutcome::Triggered { output } => output,
        }
    }
}

/// Minimum raw content size (bytes) for the guardrail to activate.
///
/// Tiny files naturally have higher overhead from transformation markers
/// (e.g., `/* ... */`), which is expected and not a sign of a problem.
/// The guardrail only applies to files large enough that compression
/// should genuinely reduce size.
const MIN_RAW_SIZE_FOR_GUARDRAIL: usize = 256;

/// Apply the output guardrail, writing warnings to the provided writer.
///
/// Two-tier comparison:
/// 1. Skip if raw is too small (< 256 bytes) — overhead is expected for tiny files
/// 2. Byte fast path: if `compressed.len() <= raw.len()`, immediately pass
/// 3. Token slow path: count tokens of both; if compressed tokens > raw tokens, trigger
///
/// On trigger: writes `[skim:guardrail] compressed output larger than raw; emitting raw`
/// to the writer and returns `Triggered { output: raw }`.
///
/// Takes ownership of both strings to avoid unnecessary cloning on the fast path.
pub(crate) fn apply(
    raw: String,
    compressed: String,
    writer: &mut impl Write,
) -> Result<GuardrailOutcome> {
    // Tier 0: Skip for tiny files — transformation overhead is expected
    if raw.len() < MIN_RAW_SIZE_FOR_GUARDRAIL {
        return Ok(GuardrailOutcome::Passed { output: compressed });
    }

    // Tier 1: Byte fast path
    if compressed.len() <= raw.len() {
        return Ok(GuardrailOutcome::Passed { output: compressed });
    }

    // Tier 2: Token slow path
    let raw_tokens = crate::tokens::count_tokens(&raw)?;
    let compressed_tokens = crate::tokens::count_tokens(&compressed)?;

    if compressed_tokens > raw_tokens {
        writeln!(
            writer,
            "[skim:guardrail] compressed output larger than raw; emitting raw"
        )?;
        Ok(GuardrailOutcome::Triggered { output: raw })
    } else {
        Ok(GuardrailOutcome::Passed { output: compressed })
    }
}

/// Convenience wrapper: apply the guardrail with stderr as the warning writer.
pub(crate) fn apply_to_stderr(raw: String, compressed: String) -> Result<GuardrailOutcome> {
    apply(raw, compressed, &mut io::stderr())
}

/// Line-count guardrail for annotated diff output.
///
/// Standard byte/token comparison is not suitable for annotated diff output:
/// line-number prefixes (e.g. `+42 fn foo()`) add bytes to every line without
/// adding content. The true measure of compression is whether AST elision
/// reduced the number of lines shown.
///
/// Two-tier comparison:
/// 1. Tier 1: if `compressed_lines <= raw_lines`, pass immediately.
/// 2. Tier 2: token slow path — count tokens of both; if compressed tokens
///    exceed raw tokens, trigger.
///
/// Emits the same `[skim:guardrail]` warning to stderr on trigger.
///
/// Takes ownership of both strings to avoid unnecessary cloning on the fast path.
pub(crate) fn apply_line_count_to_stderr(
    raw: String,
    compressed: String,
    writer: &mut impl Write,
) -> Result<GuardrailOutcome> {
    // Tier 0: Skip for tiny raw output — transformation overhead is expected.
    if raw.len() < MIN_RAW_SIZE_FOR_GUARDRAIL {
        return Ok(GuardrailOutcome::Passed { output: compressed });
    }

    // Tier 1: Line-count fast path.
    // Line-number annotations add bytes per line but not lines. Fewer or equal
    // lines means AST elision is working even when byte count is higher.
    let raw_lines = raw.lines().count();
    let compressed_lines = compressed.lines().count();
    if compressed_lines <= raw_lines {
        return Ok(GuardrailOutcome::Passed { output: compressed });
    }

    // Tier 2: Token slow path.
    let raw_tokens = crate::tokens::count_tokens(&raw)?;
    let compressed_tokens = crate::tokens::count_tokens(&compressed)?;

    if compressed_tokens > raw_tokens {
        writeln!(
            writer,
            "[skim:guardrail] compressed output larger than raw; emitting raw"
        )?;
        Ok(GuardrailOutcome::Triggered { output: raw })
    } else {
        Ok(GuardrailOutcome::Passed { output: compressed })
    }
}

/// Convenience wrapper: line-count guardrail with stderr as the warning writer.
///
/// Use this instead of `apply_to_stderr` when the compressed output contains
/// per-line annotation overhead (e.g. line numbers in diff output) that should
/// not count against the compression budget.
pub(crate) fn apply_line_count_guardrail(raw: String, compressed: String) -> Result<GuardrailOutcome> {
    apply_line_count_to_stderr(raw, compressed, &mut io::stderr())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pass_when_compressed_shorter() {
        let raw = "function hello() { return 'world'; }".to_string();
        let compressed = "function hello()".to_string();
        let mut buf = Vec::new();
        let outcome = apply(raw, compressed.clone(), &mut buf).unwrap();
        assert!(!outcome.was_triggered());
        assert_eq!(outcome.into_output(), compressed);
        assert!(buf.is_empty(), "No warning should be emitted");
    }

    #[test]
    fn test_pass_when_compressed_equal_length() {
        let raw = "hello world".to_string();
        let compressed = "hello world".to_string();
        let mut buf = Vec::new();
        let outcome = apply(raw, compressed.clone(), &mut buf).unwrap();
        assert!(!outcome.was_triggered());
        assert_eq!(outcome.into_output(), compressed);
    }

    #[test]
    fn test_skip_for_tiny_files() {
        // Files below MIN_RAW_SIZE_FOR_GUARDRAIL should always pass
        let raw = "x".to_string();
        let compressed =
            "this is a much longer string that has many more tokens than the raw input".to_string();
        let mut buf = Vec::new();
        let outcome = apply(raw, compressed.clone(), &mut buf).unwrap();
        assert!(!outcome.was_triggered(), "Tiny files should skip guardrail");
        assert_eq!(outcome.into_output(), compressed);
    }

    #[test]
    fn test_triggered_when_compressed_larger_bytes_and_tokens() {
        // Raw must be >= MIN_RAW_SIZE_FOR_GUARDRAIL (256 bytes) to activate guardrail
        let raw = "x".repeat(300);
        let compressed_content = "this is a much longer string with many more tokens ".repeat(20);
        let mut buf = Vec::new();
        let outcome = apply(raw.clone(), compressed_content, &mut buf).unwrap();
        assert!(outcome.was_triggered());
        assert_eq!(outcome.into_output(), raw);
        let warning = String::from_utf8(buf).unwrap();
        assert!(
            warning.contains("[skim:guardrail]"),
            "expected guardrail warning, got: {warning}"
        );
    }

    // Input < MIN_RAW_SIZE_FOR_GUARDRAIL (256 bytes), so Tier 0 skips all checks
    #[test]
    fn test_tiny_file_skips_guardrail_even_when_compressed_larger() {
        // Compressed has more bytes (padding/whitespace) but potentially fewer tokens
        // This is an edge case -- we use a string with many spaces (which tokenize cheaply)
        let raw = "abcdefghij".to_string();
        // More bytes (spaces are cheap tokens) but fewer tokens
        let compressed = "a b c d e f g h i j k".to_string();
        let mut buf = Vec::new();
        let outcome = apply(raw, compressed, &mut buf).unwrap();
        // The outcome depends on actual token counts. This test verifies
        // the two-tier logic works without panicking.
        let _ = outcome.into_output();
    }

    #[test]
    fn test_empty_inputs() {
        let mut buf = Vec::new();
        let outcome = apply(String::new(), String::new(), &mut buf).unwrap();
        assert!(!outcome.was_triggered());
        assert_eq!(outcome.into_output(), "");
    }

    // ========================================================================
    // apply_line_count_to_stderr tests (diff path guardrail)
    // ========================================================================

    /// Line-number annotations make each line longer but don't add lines.
    /// A diff with line numbers should pass when line count is equal.
    #[test]
    fn test_line_count_passes_when_annotated_lines_equal_raw() {
        // Simulate raw diff: 5 lines, each without line-number prefix
        let raw = "fn foo() {\n    let x = 1;\n    let y = 2;\n    x + y\n}\n".repeat(60);
        // Annotated output: same 5 lines but with `+42 ` prefix — more bytes, same count
        let annotated_lines: Vec<String> = raw
            .lines()
            .enumerate()
            .map(|(i, l)| format!("+{} {l}", i + 1))
            .collect();
        let compressed = annotated_lines.join("\n") + "\n";

        // compressed has more bytes than raw, but same line count
        assert!(
            compressed.len() > raw.len(),
            "compressed should be larger in bytes"
        );
        assert_eq!(
            compressed.lines().count(),
            raw.lines().count(),
            "line counts should match"
        );

        let mut buf = Vec::new();
        let outcome = apply_line_count_to_stderr(raw, compressed.clone(), &mut buf).unwrap();
        assert!(
            !outcome.was_triggered(),
            "guardrail should not trigger when line counts are equal"
        );
        assert_eq!(outcome.into_output(), compressed);
        assert!(buf.is_empty(), "no warning should be emitted");
    }

    /// AST elision removes unchanged functions — fewer lines is valid compression
    /// even when remaining lines are byte-heavier due to annotations.
    #[test]
    fn test_line_count_passes_when_compressed_has_fewer_lines() {
        // Raw diff: 20 lines (simulating a full function diff)
        let raw = (1..=20)
            .map(|i| format!("line {i}: some content here"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        // Compressed: only 5 lines shown (AST elision kept only changed nodes)
        // but each line has a long line-number prefix making byte count larger
        let compressed = (1..=5)
            .map(|i| format!("+{:>4} some content here with extra annotation overhead padding", i))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";

        assert!(
            compressed.lines().count() < raw.lines().count(),
            "compressed should have fewer lines"
        );

        let mut buf = Vec::new();
        let outcome = apply_line_count_to_stderr(raw, compressed.clone(), &mut buf).unwrap();
        assert!(
            !outcome.was_triggered(),
            "fewer lines = valid compression even with byte overhead"
        );
        assert_eq!(outcome.into_output(), compressed);
    }

    /// When the compressed output has more lines than raw, it genuinely inflated —
    /// trigger the guardrail.
    #[test]
    fn test_line_count_triggers_when_compressed_has_more_lines_and_tokens() {
        // Raw: 5 lines
        let raw_line = "x".repeat(60);
        let raw = std::iter::repeat(raw_line.as_str())
            .take(5)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        // Ensure raw >= MIN_RAW_SIZE_FOR_GUARDRAIL
        assert!(
            raw.len() >= MIN_RAW_SIZE_FOR_GUARDRAIL,
            "raw must be >= {MIN_RAW_SIZE_FOR_GUARDRAIL} bytes to activate guardrail"
        );

        // Compressed: 50 lines of short content (more lines, also more tokens)
        let compressed = "token ".repeat(10) + "\n";
        let compressed = compressed.repeat(50);
        assert!(
            compressed.lines().count() > raw.lines().count(),
            "compressed should have more lines than raw"
        );

        let mut buf = Vec::new();
        let outcome = apply_line_count_to_stderr(raw.clone(), compressed, &mut buf).unwrap();
        assert!(
            outcome.was_triggered(),
            "guardrail should trigger when compressed has more lines and more tokens"
        );
        assert_eq!(outcome.into_output(), raw);
        let warning = String::from_utf8(buf).unwrap();
        assert!(
            warning.contains("[skim:guardrail]"),
            "expected guardrail warning, got: {warning}"
        );
    }

    /// Tiny raw output (< MIN_RAW_SIZE_FOR_GUARDRAIL) always passes the line-count guardrail.
    #[test]
    fn test_line_count_skips_for_tiny_raw() {
        let raw = "x\ny\n".to_string(); // 4 bytes, well below 256
        let compressed = (0..100).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        let mut buf = Vec::new();
        let outcome = apply_line_count_to_stderr(raw, compressed.clone(), &mut buf).unwrap();
        assert!(
            !outcome.was_triggered(),
            "Tier 0 should skip for tiny files"
        );
        assert_eq!(outcome.into_output(), compressed);
    }
}
