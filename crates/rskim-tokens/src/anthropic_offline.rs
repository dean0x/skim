//! Offline Anthropic token approximation.
//!
//! # Method
//!
//! Count the text with the embedded `cl100k_base` tokenizer, then apply a fixed
//! uplift: `ceil(cl100k_count × UPLIFT_FACTOR)`, where `UPLIFT_FACTOR = 1.25`.
//!
//! # Basis
//!
//! Anthropic's public guidance states that tiktoken-style counts undercount
//! Claude tokens by approximately 15–20% on typical text (and more on
//! code or non-English content). A factor of **1.25** covers the upper end of
//! the documented 15–20% band and provides margin for code and multilingual
//! content.
//!
//! # Approximation notice — **not a measurement**
//!
//! The 1.25 factor is a **documented directional default**, not an empirically
//! measured calibration. The only binding per-document gate is:
//! `anthropic_offline_count ≥ cl100k_count` for every document (guaranteed by
//! construction because `UPLIFT_FACTOR ≥ 1.0`).
//!
//! Accurate calibration against live Anthropic API counts is planned for a
//! post-PRISM-#645 benchmarking pass. Until then, treat this counter as an
//! **upper-bound approximation** suitable for budget estimation, not exact
//! billing calculation.
//!
//! # Network isolation
//!
//! This module performs **zero network I/O**. It relies only on the embedded
//! `cl100k_base` vocabulary (compiled in by `tiktoken-rs`). For a live count
//! via the Anthropic API, enable the `net-anthropic` feature and use
//! `net::AnthropicNetworkCounter` (available only when that feature is active).

/// Uplift factor applied to cl100k counts to approximate Anthropic token usage.
///
/// Basis: Anthropic's ~15–20% undercount guidance (upper-band floor) plus
/// margin for code and non-English content. This is an approximation label,
/// not an empirical measurement.
pub const UPLIFT_FACTOR: f64 = 1.25;

/// Count tokens using the offline Anthropic approximation.
///
/// Applies `ceil(cl100k_count × UPLIFT_FACTOR)` to the given pre-computed
/// cl100k count.
///
/// # Arithmetic safety
///
/// To avoid integer overflow when `cl100k_count` is large, the multiplication
/// is performed in `f64` (exact for values up to 2^53), then the result is
/// saturated to `usize::MAX` before truncation. This means the result is
/// always `≥ cl100k_count` for any finite input (avoids PF-004).
///
/// # Examples
///
/// ```
/// use rskim_tokens::anthropic_offline::count_anthropic_offline;
///
/// // Empty input → 0
/// assert_eq!(count_anthropic_offline(0), 0);
///
/// // Per-document >= cl100k guarantee holds for all inputs
/// let cl100k_count = 80usize;
/// let approx = count_anthropic_offline(cl100k_count);
/// assert!(approx >= cl100k_count);
/// ```
#[must_use]
pub fn count_anthropic_offline(cl100k_count: usize) -> usize {
    if cl100k_count == 0 {
        return 0;
    }

    // Widen to f64 before multiply to avoid wrapping on large counts (avoids PF-004).
    // f64 is exact for integers up to 2^53 (~9 × 10^15), which exceeds realistic
    // token counts by many orders of magnitude.
    let widened = cl100k_count as f64;
    let product = widened * UPLIFT_FACTOR;

    // Saturate at usize::MAX rather than panic/wrap on astronomically large inputs.
    // f64::ceil on a finite positive value is always finite and >= widened, so the
    // final cast is safe after the saturation check.
    let ceiling = product.ceil();
    if ceiling >= usize::MAX as f64 {
        usize::MAX
    } else {
        // SAFETY: ceiling is finite, non-negative, and < usize::MAX as f64
        ceiling as usize
    }
}
