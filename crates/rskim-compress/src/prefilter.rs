//! Per-class byte-size prefilter for the block compression router (#304 Phase 3, P0.1 #427).
//!
//! # AD-010 — Determinism
//!
//! All cutoffs are **byte-size-based only** (never time-based). No `SystemTime`,
//! `Instant`, `rand`, or `getrandom` is used here or transitively. The `clippy.toml`
//! `disallowed-methods` gate enforces this crate-wide.
//!
//! # AD-010 (Step 7 / AC22) — Per-class thresholds + min-size floor
//!
//! ## P0.1 / ADR-007 — Code class is always Passthrough
//!
//! `Class::Code` routes to `EngineTarget::Passthrough` in `engine_for_class` (P0.1).
//! The prefilter is never reached for code blocks — they skip engine dispatch entirely.
//! `class_max` for `Class::Code` returns 0; the prefilter is never reached for code
//! blocks because `engine_for_class` routes them to `EngineTarget::Passthrough` first.
//!
//! ## Basis for threshold values (ADR-003 / PF-005)
//!
//! **Provisional basis — profile not yet measured.** These constants are *conservative*
//! provisions selected from first-principles, not from a measured payload profile.
//! The profile deliverable (Phase 4 benches / AC24) will produce empirical p50/p95
//! figures; when that profile lands, update these constants and the basis documentation.
//!
//! - **Safety is unconditional**: the never-inflate byte gate (AD-008) ensures that any
//!   block above a threshold that somehow slips through still cannot produce output
//!   larger than input. The prefilter is a *latency guard*, not a correctness guard.
//!
//! - **Why a min floor?** Very small blocks (< `MIN_SIZE_FLOOR` bytes) cannot yield
//!   meaningful compression. The floor avoids wasting engine CPU on blocks guaranteed
//!   to be too small to benefit.
//!
//! - **JSON / Log / Mixed thresholds** follow first-principles rationale:
//!   engines are structurally cheaper than tree-sitter, but large blocks (> 64 KB)
//!   are unusual in real chat payloads and should skip to bound worst-case latency.
//!
//! ## Profile deliverable anchor (Phase 4)
//!
//! When Phase 4 produces the payload profile (`skim bench` / AC24):
//! 1. Record p50/p95/p99 block sizes per class from a representative corpus.
//! 2. Set `MAX_JSON_BYTES`, `MAX_LOG_BYTES`, `MAX_MIXED_BYTES` to 2× p95 block sizes.
//! 3. Update the basis documentation to cite the profile date + corpus.
//! 4. Re-run AC22 and AC24 to confirm the new thresholds hold.

use rskim_llm::Class;

// ============================================================================
// Prefilter constants
// ============================================================================

/// Minimum block size (bytes) eligible for compression.
///
/// Blocks smaller than this floor are forwarded byte-identical: they are too
/// small to yield meaningful structure compression.
///
/// ## Basis (provisional — see module-level note)
///
/// 64 bytes ≈ one short code line. A single-line block that rskim-core would
/// render as an identical single-signature line gains no tokens from structure
/// mode. The floor eliminates these no-op engine runs.
pub const MIN_SIZE_FLOOR: usize = 64;

/// Maximum JSON block size eligible for compression.
///
/// ## Basis (provisional — see module-level note)
///
/// Set at 64 KiB. The JSON engine is structurally cheaper than tree-sitter,
/// but very large JSON objects (> 64 KiB) in chat payloads are uncommon and
/// may exceed the `MAX_JSON_DEPTH=500` / `MAX_JSON_KEYS=10_000` bounds anyway.
pub const MAX_JSON_BYTES: usize = 64 * 1024; // 64 KiB

/// Maximum log block size eligible for compression.
///
/// ## Basis (provisional — see module-level note)
///
/// Set at 64 KiB. The log engine is line-scan based (cheaper than tree-sitter),
/// but very large log dumps in chat payloads are unusual. This bounds worst-case
/// latency from the dedup + regex passes.
pub const MAX_LOG_BYTES: usize = 64 * 1024; // 64 KiB

/// Maximum mixed block size eligible for compression.
///
/// ## Basis (provisional — see module-level note)
///
/// Set at 64 KiB. Mixed blocks are typically prose + code fences, and large
/// mixed blocks (> 64 KiB) are rare in chat payloads.
pub const MAX_MIXED_BYTES: usize = 64 * 1024; // 64 KiB

// ============================================================================
// Prefilter verdict
// ============================================================================

/// Outcome of the prefilter check for a candidate block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefilterVerdict {
    /// Block is eligible for compression — proceed with the engine.
    Eligible,
    /// Block is too small (below `MIN_SIZE_FLOOR`) or too large (above the
    /// per-class maximum) — forward byte-identical, emit `Passthrough` record.
    Skip,
}

/// Check whether a candidate block should be routed to an engine.
///
/// Returns [`PrefilterVerdict::Skip`] when:
/// - `block_len < MIN_SIZE_FLOOR` (too small to benefit), OR
/// - `block_len > max_bytes_for_class(class)` (too large to be latency-safe).
///
/// Returns [`PrefilterVerdict::Eligible`] otherwise.
///
/// # AD-010
///
/// This check is **byte-size-based only** — no timing, no randomness.
pub fn prefilter_check(class: Class, block_len: usize) -> PrefilterVerdict {
    // Below minimum floor → always skip.
    if block_len < MIN_SIZE_FLOOR {
        return PrefilterVerdict::Skip;
    }
    // Above per-class maximum → skip.
    let max = class_max(class);
    if block_len > max {
        return PrefilterVerdict::Skip;
    }
    PrefilterVerdict::Eligible
}

/// Return the per-class maximum byte size threshold.
///
/// `Class::Code` returns 0 (always-skip) — code blocks route to Passthrough
/// before reaching the prefilter (P0.1 / ADR-007). The 0 here is a safety
/// backstop; in practice `engine_for_class` returns Passthrough for Code blocks
/// and `apply_engine` never calls the prefilter for them.
fn class_max(class: Class) -> usize {
    match class {
        Class::Json => MAX_JSON_BYTES,
        Class::Log => MAX_LOG_BYTES,
        Class::Mixed => MAX_MIXED_BYTES,
        // Code: always passthrough (P0.1 / ADR-007); prefilter never reached.
        // Text/Unknown: passthrough classes; prefilter moot; 0 = always-skip backstop.
        _ => 0,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn below_floor_always_skip() {
        assert_eq!(
            prefilter_check(Class::Code, MIN_SIZE_FLOOR - 1),
            PrefilterVerdict::Skip,
            "block below MIN_SIZE_FLOOR must be skipped regardless of class"
        );
    }

    /// P0.1 / ADR-007 — Code blocks always Skip in the prefilter (class_max = 0).
    ///
    /// In practice `engine_for_class` returns Passthrough before the prefilter is
    /// called for Code blocks; this test documents the defensive backstop.
    #[test]
    fn code_class_always_skips_prefilter() {
        // Even at exactly MIN_SIZE_FLOOR, a Code block must Skip (class_max = 0 → block_len > 0).
        assert_eq!(
            prefilter_check(Class::Code, MIN_SIZE_FLOOR),
            PrefilterVerdict::Skip,
            "P0.1: Code class must always Skip in prefilter (class_max = 0)"
        );
        // And well above floor.
        assert_eq!(
            prefilter_check(Class::Code, 128 * 1024),
            PrefilterVerdict::Skip,
            "P0.1: large Code block must also Skip via prefilter backstop"
        );
    }

    #[test]
    fn at_floor_eligible_json() {
        assert_eq!(
            prefilter_check(Class::Json, MIN_SIZE_FLOOR),
            PrefilterVerdict::Eligible,
            "JSON block at exactly MIN_SIZE_FLOOR must be eligible"
        );
    }

    #[test]
    fn json_log_mixed_thresholds() {
        // All three non-code engine classes at their max are eligible.
        assert_eq!(
            prefilter_check(Class::Json, MAX_JSON_BYTES),
            PrefilterVerdict::Eligible
        );
        assert_eq!(
            prefilter_check(Class::Log, MAX_LOG_BYTES),
            PrefilterVerdict::Eligible
        );
        assert_eq!(
            prefilter_check(Class::Mixed, MAX_MIXED_BYTES),
            PrefilterVerdict::Eligible
        );
        // One byte above each maximum is skipped.
        assert_eq!(
            prefilter_check(Class::Json, MAX_JSON_BYTES + 1),
            PrefilterVerdict::Skip
        );
        assert_eq!(
            prefilter_check(Class::Log, MAX_LOG_BYTES + 1),
            PrefilterVerdict::Skip
        );
        assert_eq!(
            prefilter_check(Class::Mixed, MAX_MIXED_BYTES + 1),
            PrefilterVerdict::Skip
        );
    }
}
