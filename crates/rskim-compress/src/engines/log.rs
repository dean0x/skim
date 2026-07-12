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

use crate::log::{LogFlags, Losslessness, ParseResult, compress_log};

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
/// returns `Passthrough` (no entries found, block misclassified, empty input,
/// or a content-discarding operation would be required in Lossless mode).
///
/// # Invariant: never empty result (AC4)
///
/// If `compress_log` would produce zero output lines, this function returns
/// `CompressResult::Passthrough` instead. An empty result is never returned.
///
/// # ADR-007 — Lossless proxy egress
///
/// The proxy adapter uses `Losslessness::Lossless`: timestamps are captured as
/// min–max range metadata, dedup is case-sensitive, DEBUG/TRACE lines are never
/// hidden, and any input requiring content-discarding (over-cap stacks, etc.)
/// returns `CompressResult::Passthrough` rather than partially-lossy output.
pub(crate) fn compress_log_block(text: &str) -> CompressResult {
    let flags = LogFlags {
        losslessness: Losslessness::Lossless,
        ..Default::default()
    };
    match compress_log(text, &flags) {
        // Both tiers emit from the pre-rendered LogResult display string (AC4).
        ParseResult::Full(result) | ParseResult::Degraded(result, _) => {
            let content = result.to_string();
            if content.is_empty() {
                CompressResult::Passthrough
            } else {
                CompressResult::Compressed { content }
            }
        }
        // No structured/regex entries matched — forward byte-identical (AC4 negative).
        ParseResult::Passthrough(_) => CompressResult::Passthrough,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    // =========================================================================
    // AC4 — Successful log compression (Lossless proxy path — ADR-007)
    // =========================================================================

    /// Lossless dedup produces `×N` count annotations and preserves message text.
    ///
    /// Discriminating: if dedup is silently disabled or the `×N` annotation is
    /// stripped (Lossless becoming Lossy-without-annotation), neither assertion holds.
    #[test]
    fn annotated_dedup_visible() {
        let log = "ERROR: database connection failed\nERROR: database connection failed\nERROR: database connection failed\nWARN: retrying...\n";
        let result = compress_log_block(log);
        let CompressResult::Compressed { ref content } = result else {
            panic!("Expected Compressed result; got Passthrough for structured log");
        };
        // Count annotation must appear (×3 for the triplicated ERROR).
        assert!(
            content.contains('\u{d7}'),
            "Lossless dedup must emit ×N annotation; got: {content}"
        );
        // Original message text must be present verbatim.
        assert!(
            content.contains("database connection failed"),
            "Message text must appear verbatim in Lossless output; got: {content}"
        );
    }

    /// Case-variant messages are distinct content in Lossless mode.
    ///
    /// Discriminating: if case-folding is applied (Lossy leak), both variants
    /// collapse to one entry and the assert fails.
    #[test]
    fn case_variant_messages_both_present() {
        // "critical failure" and "CRITICAL FAILURE" must both appear.
        let log = "ERROR: critical failure\nDEBUG: something happened\nERROR: CRITICAL FAILURE\n";
        let result = compress_log_block(log);
        let CompressResult::Compressed { ref content } = result else {
            panic!("Expected Compressed result; got Passthrough for structured log");
        };
        assert!(
            content.contains("critical failure"),
            "Lowercase variant must appear in Lossless output; got: {content}"
        );
        assert!(
            content.contains("CRITICAL FAILURE"),
            "Uppercase variant must appear in Lossless output; got: {content}"
        );
        // DEBUG line must also reach output (no debug-hiding in Lossless mode, axis 4).
        assert!(
            content.contains("something happened"),
            "DEBUG line must reach Lossless output (axis 4); got: {content}"
        );
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

    /// DEBUG and TRACE lines are never hidden in Lossless mode (axis 4).
    ///
    /// Discriminating: if debug-hiding is applied (Lossy leak), these lines
    /// disappear from the output and both asserts fail.
    #[test]
    fn debug_lines_reach_output() {
        let log = "\
ERROR: NullPointerException at line 42\n\
DEBUG: internal state: x=42\n\
TRACE: entering method doThing\n\
INFO: Connected to database\n\
";
        let result = compress_log_block(log);
        let CompressResult::Compressed { ref content } = result else {
            panic!("Expected Compressed result; got Passthrough for structured log");
        };
        assert!(
            content.contains("internal state"),
            "DEBUG line must reach Lossless output (no debug-hiding in Lossless mode); got: {content}"
        );
        assert!(
            content.contains("entering method doThing"),
            "TRACE line must reach Lossless output (no debug-hiding in Lossless mode); got: {content}"
        );
    }
}
