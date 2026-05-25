/// Temporal hotspot and bug-fix density scoring with exponential decay.
///
/// All functions are pure (no I/O, no side effects). Consumers supply the current
/// epoch timestamp as `now_epoch` so that tests are fully deterministic.
///
/// # Algorithm overview
///
/// [`compute_file_risk_scores`] performs a single pass over the commit list,
/// accumulating decay-weighted totals per file. Hotspot scores are then
/// max-normalized so the busiest file always scores 1.0. Fix density is the
/// ratio of fix-weighted touches to total weighted touches per file.
use std::collections::HashMap;

use crate::types::{CommitInfo, FileRiskScores};

/// Default half-life (in days) used when callers do not supply a custom value.
///
/// A 30-day half-life means commits from one month ago contribute ~37% as much
/// weight as a commit made today.
pub const DEFAULT_HALF_LIFE_DAYS: f64 = 30.0;

/// Exponential decay weight for a single commit.
///
/// Returns `exp(-elapsed_days / half_life_days)`, clamped to `[0.0, 1.0]`.
/// A negative `elapsed_days` (future commit) is treated as zero elapsed time
/// and therefore returns `1.0`.
///
/// # Panics
///
/// Panics in debug builds when `half_life_days <= 0.0`.
///
/// # Examples
///
/// ```rust
/// use rskim_search::decay_weight;
///
/// let w = decay_weight(0.0, 30.0);
/// assert_eq!(w, 1.0);
///
/// let w_half = decay_weight(30.0, 30.0);
/// // ≈ 1/e ≈ 0.368
/// assert!((w_half - std::f64::consts::E.recip()).abs() < 1e-9);
/// ```
#[must_use]
#[inline]
pub fn decay_weight(elapsed_days: f64, half_life_days: f64) -> f64 {
    debug_assert!(half_life_days > 0.0);
    (-elapsed_days / half_life_days).exp().clamp(0.0, 1.0)
}

/// Compute per-file hotspot and bug-fix density scores from a git commit history.
///
/// # Parameters
///
/// - `commits`: Slice of [`CommitInfo`] values (any order).
/// - `now_epoch`: Current Unix timestamp in seconds (used for elapsed-time computation).
///   Pass a fixed value in tests for full determinism.
/// - `half_life_days`: Exponential decay half-life in days. Use [`DEFAULT_HALF_LIFE_DAYS`]
///   unless you have a domain-specific reason to change it.
///
/// # Returns
///
/// A [`HashMap`] mapping file path strings to [`FileRiskScores`]. The map is empty when
/// `commits` is empty. Hotspot scores are max-normalized so the highest-activity file
/// always scores `1.0`.
///
/// # Algorithm
///
/// Single pass over commits:
/// 1. Pre-classify each commit once with [`super::is_fix_commit`].
/// 2. Compute `decay_weight` for each commit based on its timestamp.
/// 3. Accumulate `(weighted_total, weighted_fix_total)` per file path.
/// 4. Max-normalize `weighted_total` → `hotspot`.
/// 5. Compute `weighted_fix_total / weighted_total` → `fix_density`.
#[must_use]
pub fn compute_file_risk_scores(
    commits: &[CommitInfo],
    now_epoch: u64,
    half_life_days: f64,
) -> HashMap<String, FileRiskScores> {
    debug_assert!(half_life_days > 0.0);

    if commits.is_empty() {
        return HashMap::new();
    }

    // Pre-classify fix commits once to avoid repeated regex evaluation in the hot loop.
    let fix_flags: Vec<bool> = commits
        .iter()
        .map(|c| super::is_fix_commit(&c.message))
        .collect();

    // Accumulate per-file (weighted_total, weighted_fix_total).
    // Pre-allocate with a reasonable bound; commits.len() is an upper bound
    // on distinct files (each commit touches ≥1 file).
    let mut accum: HashMap<String, (f64, f64)> =
        HashMap::with_capacity(commits.len().min(50_000));

    for (commit, &is_fix) in commits.iter().zip(fix_flags.iter()) {
        // Clamp negative timestamps to 0 before converting.
        let ts = commit.timestamp.max(0) as u64;

        let elapsed = if now_epoch >= ts {
            (now_epoch - ts) as f64 / 86_400.0
        } else {
            // Future commit: treat as elapsed = 0.
            0.0
        };

        let w = decay_weight(elapsed, half_life_days);

        for file in &commit.changed_files {
            let path = file.path_str().into_owned();
            let entry = accum.entry(path).or_insert((0.0, 0.0));
            entry.0 += w;
            if is_fix {
                entry.1 += w;
            }
        }
    }

    // Find the maximum weighted total for normalization.
    let max_total = accum.values().map(|(total, _)| *total).fold(0.0_f64, f64::max);

    // Build final scores.
    accum
        .into_iter()
        .map(|(path, (total, fix_total))| {
            let hotspot = if max_total > 0.0 { total / max_total } else { 0.0 };
            let fix_density = if total > f64::EPSILON { fix_total / total } else { 0.0 };
            (path, FileRiskScores { hotspot, fix_density })
        })
        .collect()
}

// ============================================================================
// Co-located tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "scoring_tests.rs"]
mod tests;
