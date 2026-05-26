//! Temporal hotspot and bug-fix density scoring with exponential decay.
//!
//! All functions are pure (no I/O, no side effects). Consumers supply the current
//! epoch timestamp as `now_epoch` so that tests are fully deterministic.
//!
//! # Algorithm overview
//!
//! [`compute_file_risk_scores`] performs a single pass over the commit list,
//! accumulating decay-weighted totals per file. Hotspot scores are then
//! max-normalized so the busiest file always scores 1.0. Fix density is the
//! ratio of fix-weighted touches to total weighted touches per file.
//!
//! [`compute_file_temporal_stats`] computes raw (non-decay-weighted) commit
//! counts per file within 30-day and 90-day windows, for use in the persistence
//! layer.
use std::collections::{HashMap, HashSet};

use crate::types::{CommitInfo, FileRiskScores, FileTemporalStats};

/// Default e-folding time (in days) used when callers do not supply a custom value.
///
/// **Naming note:** `half_life_days` follows the heatmap module convention and
/// matches the parameter name in [`decay_weight`]. The underlying formula is
/// `exp(-t / half_life_days)`, which is technically an *e-folding* decay — the
/// value reaches `1/e ≈ 0.368` (not `0.5`) after one period. The doc comments
/// say "~37%" throughout to reflect this accurately.
///
/// A 30-day period means commits from one month ago contribute ~37% as much
/// weight as a commit made today.
pub const DEFAULT_HALF_LIFE_DAYS: f64 = 30.0;

/// Exponential decay weight for a single commit.
///
/// Returns `exp(-elapsed_days / half_life_days)`, clamped to `[0.0, 1.0]`.
/// A negative `elapsed_days` (future commit) is treated as zero elapsed time
/// and therefore returns `1.0`.
///
/// **Naming note:** The parameter is called `half_life_days` to match the
/// heatmap module convention, but the formula is an *e-folding* decay — the
/// weight reaches `1/e ≈ 0.368` (not `0.5`) after one `half_life_days`
/// period. This is intentional and documented in [`DEFAULT_HALF_LIFE_DAYS`].
///
/// # Panics
///
/// Panics when `half_life_days <= 0.0` or is not finite.
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
    assert!(
        half_life_days > 0.0 && half_life_days.is_finite(),
        "half_life_days must be positive and finite, got {half_life_days}"
    );
    // Treat NaN elapsed as zero (present-moment weight = 1.0) to prevent
    // NaN propagation into accumulators. clamp() alone does not sanitize NaN.
    let elapsed = if elapsed_days.is_nan() {
        0.0
    } else {
        elapsed_days
    };
    (-elapsed / half_life_days).exp().clamp(0.0, 1.0)
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
    assert!(half_life_days > 0.0, "half_life_days must be positive");

    if commits.is_empty() {
        return HashMap::new();
    }

    // Pre-classify fix commits once to avoid repeated regex evaluation in the hot loop.
    let fix_flags: Vec<bool> = commits
        .iter()
        .map(|c| super::is_fix_commit(&c.message))
        .collect();

    // Accumulate per-file (weighted_total, weighted_fix_total).
    // Unique files are typically 5–20× fewer than commits; use a conservative
    // fraction of commit count rather than commits.len() which over-allocates.
    let capacity = (commits.len() / 4).clamp(64, 50_000);
    let mut accum: HashMap<String, (f64, f64)> = HashMap::with_capacity(capacity);

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
            // Avoid allocating a String for already-seen paths: probe with a
            // borrowed &str first, only calling into_owned() for new entries.
            // Reduces allocations from O(total_file_touches) to O(unique_files).
            let path_cow = file.path_str();
            let path_ref: &str = &path_cow;
            let (weighted_total, weighted_fix_total) = if let Some(v) = accum.get_mut(path_ref) {
                v
            } else {
                accum.entry(path_cow.into_owned()).or_insert((0.0, 0.0))
            };
            *weighted_total += w;
            if is_fix {
                *weighted_fix_total += w;
            }
        }
    }

    // Find the maximum weighted total for normalization.
    let max_total = accum
        .values()
        .map(|&(total, _)| total)
        .fold(0.0_f64, f64::max);

    // Build final scores.
    accum
        .into_iter()
        .map(|(path, (total, fix_total))| {
            let hotspot = if max_total > 0.0 {
                total / max_total
            } else {
                0.0
            };
            let fix_density = if total > f64::EPSILON {
                fix_total / total
            } else {
                0.0
            };
            (
                path,
                FileRiskScores {
                    hotspot,
                    fix_density,
                },
            )
        })
        .collect()
}

/// Compute raw per-file commit counts within 30-day and 90-day windows.
///
/// # Parameters
///
/// - `commits`: Slice of [`CommitInfo`] values (any order).
/// - `now_epoch`: Current Unix timestamp in seconds (used for elapsed-time
///   computation). Pass a fixed value in tests for full determinism.
///
/// # Returns
///
/// A [`HashMap`] mapping file path strings to [`FileTemporalStats`]. The map is
/// empty when `commits` is empty.
///
/// # Algorithm
///
/// Single pass over commits:
/// 1. Pre-classify each commit once with [`super::is_fix_commit`].
/// 2. Compute `elapsed_days` for each commit; negative timestamps are clamped
///    to `0`, future commits are treated as `elapsed_days = 0.0`.
/// 3. For each commit, deduplicate the touched file list (a file listed twice in
///    one commit's `changed_files` is counted once).
/// 4. For each unique file in the commit: increment `total_commits` (always),
///    `fix_commits` (when the commit is a fix), `changes_30d` (when
///    `elapsed_days <= 30.0`), and `changes_90d` (when `elapsed_days <= 90.0`).
///
/// Boundary semantics: a commit at exactly `30.0` or `90.0` days is **included**
/// in the respective window (`<=` comparison).
#[must_use]
pub fn compute_file_temporal_stats(
    commits: &[CommitInfo],
    now_epoch: u64,
) -> HashMap<String, FileTemporalStats> {
    if commits.is_empty() {
        return HashMap::new();
    }

    let capacity = (commits.len() / 4).clamp(64, 50_000);
    let mut accum: HashMap<String, FileTemporalStats> = HashMap::with_capacity(capacity);
    // Per-commit deduplication buffer — reused across iterations.
    let mut seen_in_commit: HashSet<String> = HashSet::new();

    for commit in commits {
        let is_fix = super::is_fix_commit(&commit.message);

        // Clamp negative timestamps to 0 before converting to u64.
        let ts = commit.timestamp.max(0) as u64;
        let elapsed_days: f64 = if now_epoch >= ts {
            (now_epoch - ts) as f64 / 86_400.0
        } else {
            // Future commit: treat as elapsed = 0 (within both windows).
            0.0
        };

        let in_30d = elapsed_days <= 30.0;
        let in_90d = elapsed_days <= 90.0;

        // Collect unique file paths for this commit.
        // Borrow-first: check seen_in_commit with &str before calling into_owned(),
        // so duplicate paths within a commit do not allocate a new String.
        seen_in_commit.clear();
        for file in &commit.changed_files {
            let path_cow = file.path_str();
            let path_ref: &str = &path_cow;
            if !seen_in_commit.contains(path_ref) {
                seen_in_commit.insert(path_cow.into_owned());
            }
        }

        for path in &seen_in_commit {
            // Borrow-first: probe accum with &str before allocating for new entries.
            let entry = if let Some(v) = accum.get_mut(path.as_str()) {
                v
            } else {
                accum.entry(path.clone()).or_default()
            };
            entry.total_commits = entry.total_commits.saturating_add(1);
            if is_fix {
                entry.fix_commits = entry.fix_commits.saturating_add(1);
            }
            if in_30d {
                entry.changes_30d = entry.changes_30d.saturating_add(1);
            }
            if in_90d {
                entry.changes_90d = entry.changes_90d.saturating_add(1);
            }
        }
    }

    accum
}

// ============================================================================
// Co-located tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "scoring_tests.rs"]
mod tests;
