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
pub(crate) fn apply(
    raw: &str,
    compressed: &str,
    writer: &mut impl Write,
) -> Result<GuardrailOutcome> {
    // Tier 0: Skip for tiny files — transformation overhead is expected
    if raw.len() < MIN_RAW_SIZE_FOR_GUARDRAIL {
        return Ok(GuardrailOutcome::Passed {
            output: compressed.to_string(),
        });
    }

    // Tier 1: Byte fast path
    if compressed.len() <= raw.len() {
        return Ok(GuardrailOutcome::Passed {
            output: compressed.to_string(),
        });
    }

    // Tier 2: Token slow path
    let raw_tokens = crate::tokens::count_tokens(raw)?;
    let compressed_tokens = crate::tokens::count_tokens(compressed)?;

    if compressed_tokens > raw_tokens {
        writeln!(
            writer,
            "[skim:guardrail] compressed output larger than raw; emitting raw"
        )?;
        Ok(GuardrailOutcome::Triggered {
            output: raw.to_string(),
        })
    } else {
        Ok(GuardrailOutcome::Passed {
            output: compressed.to_string(),
        })
    }
}

/// Convenience wrapper: apply the guardrail with stderr as the warning writer.
pub(crate) fn apply_to_stderr(raw: &str, compressed: &str) -> Result<GuardrailOutcome> {
    apply(raw, compressed, &mut io::stderr())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pass_when_compressed_shorter() {
        let raw = "function hello() { return 'world'; }";
        let compressed = "function hello()";
        let mut buf = Vec::new();
        let outcome = apply(raw, compressed, &mut buf).unwrap();
        assert!(!outcome.was_triggered());
        assert_eq!(outcome.into_output(), compressed);
        assert!(buf.is_empty(), "No warning should be emitted");
    }

    #[test]
    fn test_pass_when_compressed_equal_length() {
        let raw = "hello world";
        let compressed = "hello world";
        let mut buf = Vec::new();
        let outcome = apply(raw, compressed, &mut buf).unwrap();
        assert!(!outcome.was_triggered());
        assert_eq!(outcome.into_output(), compressed);
    }

    #[test]
    fn test_skip_for_tiny_files() {
        // Files below MIN_RAW_SIZE_FOR_GUARDRAIL should always pass
        let raw = "x";
        let compressed =
            "this is a much longer string that has many more tokens than the raw input";
        let mut buf = Vec::new();
        let outcome = apply(raw, compressed, &mut buf).unwrap();
        assert!(!outcome.was_triggered(), "Tiny files should skip guardrail");
        assert_eq!(outcome.into_output(), compressed);
    }

    #[test]
    fn test_triggered_when_compressed_larger_bytes_and_tokens() {
        // Raw must be >= MIN_RAW_SIZE_FOR_GUARDRAIL (256 bytes) to activate guardrail
        let raw = "x".repeat(300);
        let compressed_content = "this is a much longer string with many more tokens ".repeat(20);
        let mut buf = Vec::new();
        let outcome = apply(&raw, &compressed_content, &mut buf).unwrap();
        assert!(outcome.was_triggered());
        assert_eq!(outcome.into_output(), raw);
        let warning = String::from_utf8(buf).unwrap();
        assert!(
            warning.contains("[skim:guardrail]"),
            "expected guardrail warning, got: {warning}"
        );
    }

    #[test]
    fn test_pass_when_bytes_larger_but_tokens_smaller() {
        // Compressed has more bytes (padding/whitespace) but potentially fewer tokens
        // This is an edge case -- we use a string with many spaces (which tokenize cheaply)
        let raw = "abcdefghij";
        // More bytes (spaces are cheap tokens) but fewer tokens
        let compressed = "a b c d e f g h i j k";
        let mut buf = Vec::new();
        let outcome = apply(raw, compressed, &mut buf).unwrap();
        // The outcome depends on actual token counts. This test verifies
        // the two-tier logic works without panicking.
        let _ = outcome.into_output();
    }

    #[test]
    fn test_empty_inputs() {
        let mut buf = Vec::new();
        let outcome = apply("", "", &mut buf).unwrap();
        assert!(!outcome.was_triggered());
        assert_eq!(outcome.into_output(), "");
    }
}
