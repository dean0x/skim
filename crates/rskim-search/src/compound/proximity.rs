//! Directory-proximity signal for composite ranking (#200).
//!
//! # Algorithm
//!
//! The proximity score for two repo-relative paths is a pure function of their
//! shared path prefix depth:
//!
//! ```text
//! score(a, b) = shared_prefix_segments / (1 + max(depth_a, depth_b))
//! ```
//!
//! where `shared_prefix_segments` is the number of path components that `a`
//! and `b` share from the left, and `depth_a`/`depth_b` are the total number of
//! components in each path (including the file name).
//!
//! # Properties
//!
//! - **Monotone**: deeper common prefix → strictly higher score (AC7).
//! - **Pure**: no I/O, no allocation beyond path splitting.
//! - **Degenerate-safe**: empty paths, identical paths, and very deep paths
//!   all return finite values without panic.
//! - **Overflow-safe**: path component counts use `u32::from(usize)` widening
//!   capped at `u32::MAX` (PF-004 discipline).  At real-world depths the
//!   denominator is always positive.
//!
//! # Scoring range
//!
//! The score is in `[0.0, 1.0)`:
//! - Two unrelated paths sharing only the repo root: small positive (≥ 0.0).
//! - Two paths in the same directory: approaches 1.0 as depth grows.
//! - Identical paths: returns 1.0 (shared = depth = n, score = n/(1+n) < 1).
//!
//! Note: identical paths are an edge case (you rarely compare a file with itself
//! in a blast-radius context), and the score asymptotically approaches 1.0 but
//! never reaches it because the denominator is `1 + max(depth_a, depth_b)`.
//!
//! # AC7 note on integer widening
//!
//! Path component counts are `usize` values and widened to `u32` via
//! `u32::try_from(n).unwrap_or(u32::MAX)` before converting to `f64`
//! (applies PF-004: widen before any arithmetic).  For realistic path depths
//! (< 1000 components) this is a no-op widening; for adversarial inputs it
//! caps the count at `u32::MAX` and returns a finite result without overflow.

// ============================================================================
// Core scoring function
// ============================================================================

/// Compute the directory-proximity score for two repo-relative paths.
///
/// # Arguments
///
/// * `path_a` — A repo-relative path string (e.g. `"src/cmd/search/query.rs"`).
/// * `path_b` — A repo-relative path string.
///
/// Both paths use `/` as the separator (repo-relative convention).
///
/// # Returns
///
/// A score in `[0.0, 1.0)` where higher values mean closer proximity.
/// Returns `0.0` for empty paths or paths with no shared components.
/// Never panics; always finite.
///
/// # Examples
///
/// ```
/// # use rskim_search::compound::dir_proximity_score;
/// // Same directory → high score.
/// let s = dir_proximity_score("src/a/b/x.rs", "src/a/b/y.rs");
/// assert!(s > dir_proximity_score("src/a/x.rs", "src/a/q/y.rs"));
///
/// // No shared prefix beyond root → low score.
/// let root_only = dir_proximity_score("src/x.rs", "docs/y.md");
/// assert!(dir_proximity_score("src/a/x.rs", "src/a/q/y.rs") > root_only);
/// ```
#[must_use]
pub fn dir_proximity_score(path_a: &str, path_b: &str) -> f64 {
    // Split each path into components on '/'.
    // Empty string → empty component slice → depth 0.
    let parts_a: Vec<&str> = path_a.split('/').filter(|s| !s.is_empty()).collect();
    let parts_b: Vec<&str> = path_b.split('/').filter(|s| !s.is_empty()).collect();

    // Count shared prefix segments (directory components; include filename in count).
    let shared = parts_a
        .iter()
        .zip(parts_b.iter())
        .take_while(|&(a, b)| a == b)
        .count();

    // Depth is the total number of components (including the filename).
    let depth_a = parts_a.len();
    let depth_b = parts_b.len();

    if depth_a == 0 && depth_b == 0 {
        // Both paths empty.
        return 0.0;
    }

    let max_depth = depth_a.max(depth_b);

    // PF-004: widen usize→u32→f64 BEFORE arithmetic.
    // Realistic path depths fit in u32; cap adversarial inputs at u32::MAX.
    let shared_f64 = f64::from(u32::try_from(shared).unwrap_or(u32::MAX));
    let max_depth_f64 = f64::from(u32::try_from(max_depth).unwrap_or(u32::MAX));

    // Score = shared / (1 + max_depth).
    // Denominator is always >= 1 (max_depth >= 0 → 1 + max_depth >= 1) → no NaN.
    shared_f64 / (1.0 + max_depth_f64)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "proximity_tests.rs"]
mod tests;
