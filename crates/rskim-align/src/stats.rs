//! Alignment statistics: counts, flags, and content-free SHA-256 hashes.
//!
//! `AlignStats` carries observability data for one `align()` call. It contains
//! ONLY hashes and counts — no message substrings, no auth material (AC18).
//!
//! # AD-AN-5
//!
//! `AlignStats` is the statistics record type used by the alignment recorder.
//! It must not carry content, credentials, or auth material: only SHA-256
//! digests of the full input/output (for identity verification), byte lengths,
//! and integer counters.

use sha2::{Digest, Sha256};

// ============================================================================
// Public: AlignStats
// ============================================================================

/// Statistics for one KV-cache alignment operation.
///
/// # AC18 — Content-free record
///
/// This struct contains **only** SHA-256 hashes and integer counts — no
/// message substrings, no tool descriptions, no auth material. It is safe
/// to log, record in analytics, or pass across trust boundaries.
///
/// # AD-AN-5
///
/// This type is the analytics record produced by `align()`. The proxy adapter
/// passes it to an `AlignmentRecorder` (Phase 4). The recorder writes it to
/// the `alignment_decisions` analytics table.
#[derive(Debug, Clone, Default)]
#[must_use]
pub struct AlignStats {
    /// Whether the `tools` or `functions` value actually changed during canonicalization
    /// (key-sort, element reorder, or whitespace removal). False when the span was
    /// already canonical or absent.
    pub tools_key_sorted: bool,
    /// Whether canonicalization made the body strictly smaller (whitespace removed).
    pub spans_compacted: bool,
    /// Number of `cache_control` markers injected by skim.
    pub skim_breakpoints_injected: usize,
    /// Number of `cache_control` markers already present (from the client).
    pub client_breakpoint_count: usize,
    /// Number of volatile pattern warnings emitted.
    pub volatile_warn_count: usize,
    /// Whether this call returned a fail-open (passthrough) result.
    pub fail_open: bool,
    /// SHA-256 digest of the input bytes (before alignment).
    pub input_sha256: [u8; 32],
    /// SHA-256 digest of the output bytes (after alignment / passthrough).
    pub output_sha256: [u8; 32],
    /// Byte length of the input.
    pub input_len: usize,
    /// Byte length of the output.
    pub output_len: usize,
}

impl AlignStats {
    /// Construct a fail-open stats record for a given input.
    ///
    /// Used when alignment fails and the original bytes are returned.
    /// Output equals input (SHA-256 equal passthrough).
    pub fn fail_open_from_input(input: &[u8]) -> Self {
        let digest = sha256(input);
        Self {
            fail_open: true,
            input_sha256: digest,
            output_sha256: digest,
            input_len: input.len(),
            output_len: input.len(),
            ..Default::default()
        }
    }

    /// Construct a successful alignment stats record.
    #[allow(clippy::too_many_arguments)]
    pub fn success(
        input: &[u8],
        output: &[u8],
        tools_key_sorted: bool,
        spans_compacted: bool,
        skim_breakpoints_injected: usize,
        client_breakpoint_count: usize,
        volatile_warn_count: usize,
    ) -> Self {
        Self {
            tools_key_sorted,
            spans_compacted,
            skim_breakpoints_injected,
            client_breakpoint_count,
            volatile_warn_count,
            fail_open: false,
            input_sha256: sha256(input),
            output_sha256: sha256(output),
            input_len: input.len(),
            output_len: output.len(),
        }
    }
}

// ============================================================================
// SHA-256 helper
// ============================================================================

/// Compute the SHA-256 digest of `data`.
///
/// Used for content-free identity verification in [`AlignStats`].
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(data);
    h.finalize().into()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn sha256_empty_matches_known_digest() {
        // SHA-256 of empty string
        let expected: [u8; 32] = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14, 0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f,
            0xb9, 0x24, 0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c, 0xa4, 0x95, 0x99, 0x1b,
            0x78, 0x52, 0xb8, 0x55,
        ];
        assert_eq!(sha256(b""), expected);
    }

    #[test]
    fn fail_open_stats_sha256_equal() {
        let input = b"test input";
        let stats = AlignStats::fail_open_from_input(input);
        assert!(stats.fail_open);
        assert_eq!(stats.input_sha256, stats.output_sha256);
        assert_eq!(stats.input_len, input.len());
        assert_eq!(stats.output_len, input.len());
        assert_eq!(stats.skim_breakpoints_injected, 0);
        assert_eq!(stats.client_breakpoint_count, 0);
    }

    #[test]
    fn success_stats_fields() {
        let input = b"input";
        let output = b"output123";
        let stats = AlignStats::success(input, output, true, true, 2, 1, 0);
        assert!(!stats.fail_open);
        assert!(stats.tools_key_sorted);
        assert!(stats.spans_compacted);
        assert_eq!(stats.skim_breakpoints_injected, 2);
        assert_eq!(stats.client_breakpoint_count, 1);
        assert_eq!(stats.input_len, 5);
        assert_eq!(stats.output_len, 9);
        assert_ne!(stats.input_sha256, stats.output_sha256);
    }

    #[test]
    fn default_stats_all_zero() {
        let s = AlignStats::default();
        assert!(!s.fail_open);
        assert!(!s.tools_key_sorted);
        assert_eq!(s.skim_breakpoints_injected, 0);
        assert_eq!(s.input_len, 0);
    }
}
