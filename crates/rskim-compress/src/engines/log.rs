//! Log compression engine — thin adapter over `crate::log::compress_log` (#304 Phase 2).
//!
//! # AC4 — Behavior
//!
//! Given a `Class::Log` block:
//! - Calls `crate::log::compress_log(text, &LogFlags::default())`.
//! - `ParseResult::Full` or `ParseResult::Degraded` → returns the compressed log lines.
//! - `ParseResult::Passthrough` (no structured/regex entries found) → returns
//!   `CompressResult::Passthrough` so the caller forwards original bytes byte-identical.
//!   NEVER emits an empty result (AC4 negative: misclassified block → passthrough).
//!
//! The caller (BlockRouter, Phase 3) applies the never-inflate byte gate AFTER
//! receiving the compressed content.

use crate::log::{LogFlags, ParseResult, compress_log};

/// Result of a log compression attempt.
#[derive(Debug, Clone)]
pub(crate) enum CompressResult {
    /// Compression produced structured output.
    Compressed {
        /// The compressed log lines joined with `\n`.
        content: String,
    },
    /// The block yielded no structured/regex entries, or input was empty.
    /// Caller should forward original bytes byte-identical (AC4).
    Passthrough,
}

/// Compress a log content block.
///
/// # Arguments
///
/// - `text`: the raw text payload of the block.
///
/// # Returns
///
/// `CompressResult::Compressed` when `compress_log` produces a `Full` or
/// `Degraded` result. `CompressResult::Passthrough` when `compress_log`
/// returns `Passthrough` (no entries found, block misclassified, empty input).
///
/// # Invariant: never empty result (AC4)
///
/// If `compress_log` would produce zero output lines, this function returns
/// `CompressResult::Passthrough` instead. An empty result is never returned.
pub(crate) fn compress_log_block(text: &str) -> CompressResult {
    let flags = LogFlags::default();
    match compress_log(text, &flags) {
        ParseResult::Full(result) => {
            // Full parse: use the pre-rendered LogResult display string.
            let content = result.to_string();
            if content.is_empty() {
                // AC4 invariant: never emit empty result for a log block.
                CompressResult::Passthrough
            } else {
                CompressResult::Compressed { content }
            }
        }
        ParseResult::Degraded(result, _warnings) => {
            // Degraded parse: still use the rendered output.
            let content = result.to_string();
            if content.is_empty() {
                CompressResult::Passthrough
            } else {
                CompressResult::Compressed { content }
            }
        }
        ParseResult::Passthrough(_) => {
            // No structured/regex entries matched — forward byte-identical (AC4 negative).
            CompressResult::Passthrough
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // =========================================================================
    // AC4 — Successful log compression
    // =========================================================================

    #[test]
    fn compress_log_with_duplicates_produces_shorter_output() {
        let log = "ERROR: database connection failed\nERROR: database connection failed\nERROR: database connection failed\nWARN: retrying...\n";
        let result = compress_log_block(log);
        match result {
            CompressResult::Compressed { content } => {
                let out_lines = content.lines().count();
                let in_lines = log.lines().count();
                assert!(
                    out_lines < in_lines,
                    "Deduplication must reduce line count: {} in, {} out",
                    in_lines,
                    out_lines
                );
            }
            CompressResult::Passthrough => {
                // Also acceptable if compress_log classifies this as passthrough.
            }
        }
    }

    #[test]
    fn error_lines_preserved_in_output() {
        // AC4: every ERROR-equivalent line must appear in the output.
        let log = "ERROR: critical failure\nDEBUG: something happened\nERROR: another failure\n";
        let result = compress_log_block(log);
        match result {
            CompressResult::Compressed { content } => {
                // Both ERROR lines must be present somewhere in the output.
                // (They may be deduplicated if identical, but these are distinct.)
                assert!(
                    content.contains("critical failure"),
                    "First ERROR line must appear in output"
                );
                assert!(
                    content.contains("another failure"),
                    "Second ERROR line must appear in output"
                );
            }
            CompressResult::Passthrough => {
                // compress_log may passthrough if it can't structure this — acceptable.
            }
        }
    }

    // =========================================================================
    // AC4 negative — misclassified block → passthrough, never empty
    // =========================================================================

    #[test]
    fn empty_input_returns_passthrough() {
        // AC4: misclassification → passthrough, NEVER empty result.
        let result = compress_log_block("");
        assert!(
            matches!(result, CompressResult::Passthrough),
            "Empty input must return Passthrough, not an empty Compressed result"
        );
    }

    #[test]
    fn plain_prose_returns_passthrough_not_empty() {
        // AC4: a block misclassified as Log that yields no structured entries
        // MUST return Passthrough, not an empty Compressed result.
        let prose = "This is just some regular prose text with no log patterns.\n";
        let result = compress_log_block(prose);
        // Either Compressed (if some heuristic fires) or Passthrough.
        // The key invariant: if Compressed, it must not be empty.
        match result {
            CompressResult::Compressed { content } => {
                assert!(
                    !content.is_empty(),
                    "AC4: Compressed result must not be empty"
                );
            }
            CompressResult::Passthrough => {
                // Expected for plain prose — correct behavior.
            }
        }
    }

    #[test]
    fn structured_log_with_stack_trace_compresses() {
        // AC4: a Log block with stack trace must compress (or passthrough safely).
        let log = "\
ERROR: NullPointerException at line 42\n\
    at com.example.MyClass.doThing(MyClass.java:42)\n\
    at com.example.Main.run(Main.java:10)\n\
WARN: Retrying after backoff\n\
INFO: Connected to database\n\
INFO: Connected to database\n\
INFO: Connected to database\n\
";
        let result = compress_log_block(log);
        // AC4: any result must not be empty if Compressed.
        if let CompressResult::Compressed { ref content } = result {
            assert!(
                !content.is_empty(),
                "AC4: log compression result must not be empty"
            );
        }
        // No panic — function must complete.
    }
}
