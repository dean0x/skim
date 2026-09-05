//! Shared timing helpers for the scaling guards in `pseudo.rs` and `minimal.rs`.
//!
//! # Sampling rule (workspace standard)
//!
//! **Ratio gates** (doubling-N tests: assert `ratio < THRESHOLD`) use the
//! **minimum** of 5 samples.  Rationale: scheduler preemptions inflate exactly
//! one sample per measurement; taking the minimum discards that spike while
//! keeping the true algorithmic signal.  A scheduler spike makes an O(N)
//! ratio *look* super-linear; the minimum is the honest floor.
//!
//! **Absolute gates** (single-threshold tests: assert `t >= FLOOR` or
//! `t < BUDGET`) use the **median** of 5 samples.  Rationale: the minimum
//! could be unrepresentatively fast (warm branch-predictor, etc.), making
//! the noise-floor assertion vacuous; the median is a better estimate of
//! the typical cost.
//!
//! This two-rule taxonomy explains *both* existing precedents in the
//! codebase — ratio guards and the absolute cubic smoke test — without
//! introducing a third answer.
//!
//! # Parse hoisting
//!
//! All helpers hoist the tree-sitter parse **outside** the 5-sample loop.
//! We are measuring the transform walk, not the parser.  A parse inside the
//! loop inflates every sample by a constant (fast) parse cost and narrows
//! the ratio signal; hoisting it keeps the measurement honest.
//!
//! # Threshold derivation
//!
//! The ratio threshold 2.8 ≈ 2^1.5 is the exponent-space midpoint between
//! linear growth (2^1.0 = 2.0×) and quadratic growth (2^2.0 = 4.0×) on a
//! 2× input doubling.  Choosing the midpoint gives equal log-scale margin to
//! both sides, whereas an empirically fitted constant (the historical 2.5)
//! drifts as hardware changes.  Measured ratios for correct implementations
//! sit in the 1.8×–2.1× range; 2.8 leaves ~35% headroom.

/// Run `f` exactly 5 times and return `(min, median)`.
///
/// - Use **`min`** for *ratio gates*: `let ratio = t2_min / t1_min; assert!(ratio < 2.8);`
/// - Use **`median`** for *absolute gates*: `assert!(t1_median >= FLOOR);`
///
/// The parse step must be *outside* the closure — `f` should only run the
/// transform, not re-parse the source.
pub(crate) fn time_5<F: Fn() -> f64>(f: F) -> (f64, f64) {
    let mut s: [f64; 5] = std::array::from_fn(|_| f());
    let min = s.iter().cloned().fold(f64::INFINITY, f64::min);
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    (min, s[2]) // (min, median)
}
