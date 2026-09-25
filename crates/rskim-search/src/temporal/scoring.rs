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

/// Seconds in one day, used to convert Unix-timestamp differences to fractional days.
const SECS_PER_DAY: f64 = 86_400.0;

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

/// z-score for a two-sided 95% confidence interval (1.96).
///
/// Used by [`wilson_lower_bound`]. Fixed at 95% per AD-378-1 — Wilson is
/// parameter-free at a chosen confidence level, so this is the only constant
/// the volume-weighting formula needs (the old `VOLUME_REF` saturation
/// reference is removed entirely).
const WILSON_Z_95: f64 = 1.96;

/// Wilson score-interval lower bound for a binomial proportion at 95% confidence.
///
/// Returns the lower bound of the [Wilson score interval](https://en.wikipedia.org/wiki/Binomial_proportion_confidence_interval#Wilson_score_interval)
/// for `successes` out of `trials`, clamped to `[0.0, 1.0]`. This is the
/// statistically correct way to rank a proportion (here: fix-commit density)
/// **while accounting for sample size** — small samples are self-suppressed
/// toward zero, large samples approach the raw ratio.
///
/// # Why Wilson (AD-378-1)
///
/// Ranking by a bare ratio `successes / trials` saturates on tiny samples: a
/// 1-fix/1-commit file ties a 50-fix/50-commit file at `1.0`, burying genuinely
/// fix-prone files (the #378 saturation bug). Wilson reduces a 1/1 to ~0.21 and
/// a 50/50 to ~0.93 with **no tuned constant** — the only parameter is the
/// confidence level (95%, [`WILSON_Z_95`]).
///
/// # Boundary semantics (AC4)
///
/// - `wilson_lower_bound(0, 0)` returns exactly `0.0` (no observations → no
///   evidence of risk; avoids a `0/0` NaN).
/// - `successes == 0` returns exactly `0.0` (the lower bound is clamped at 0).
/// - `successes` is clamped to `trials` before computation so an out-of-range
///   caller can never produce `p_hat > 1.0`.
///
/// # Examples
///
/// ```rust
/// use rskim_search::wilson_lower_bound;
///
/// assert_eq!(wilson_lower_bound(0, 0), 0.0);
/// assert_eq!(wilson_lower_bound(0, 5), 0.0);
/// // A 1/1 sample is heavily discounted vs a 50/50 sample.
/// assert!(wilson_lower_bound(1, 1) < wilson_lower_bound(50, 50));
/// // Result is always a valid probability.
/// let lb = wilson_lower_bound(3, 8);
/// assert!((0.0..=1.0).contains(&lb));
/// ```
#[must_use]
#[inline]
pub fn wilson_lower_bound(successes: u32, trials: u32) -> f64 {
    // No observations → no evidence; return 0.0 rather than 0/0 = NaN (AC4).
    if trials == 0 {
        return 0.0;
    }
    // Clamp successes <= trials so a malformed caller cannot yield p_hat > 1.0.
    let successes = successes.min(trials);
    let n = f64::from(trials);
    let phat = f64::from(successes) / n;
    let z = WILSON_Z_95;
    let z2 = z * z;

    // Wilson score interval lower bound:
    //   (phat + z²/2n − z·sqrt(phat(1−phat)/n + z²/4n²)) / (1 + z²/n)
    let denom = 1.0 + z2 / n;
    let centre = phat + z2 / (2.0 * n);
    let margin = z * (phat * (1.0 - phat) / n + z2 / (4.0 * n * n)).sqrt();
    let lower = (centre - margin) / denom;

    // Clamp into [0,1]: floating-point error near phat==0 can drift slightly
    // negative, and the formula is bounded above by 1.0 analytically.
    lower.clamp(0.0, 1.0)
}

/// Volume-weighted bug-fix risk score: decay-weighted fix proportion × Wilson-confidence proportion.
///
/// `risk_score = decay_fix_factor * wilson_lower_bound(fix_commits, total_commits)`.
///
/// This is the persisted `RiskRow.risk_score` used to rank files under
/// `skim search --risky`. It is the product of two `[0.0, 1.0]` proportions
/// (AD-378-1):
///
/// - `decay_fix_factor`: the **decay-weighted fix proportion** from
///   [`compute_file_risk_scores`] (`FileRiskScores::fix_density`) =
///   `Σ decay·is_fix / Σ decay` over the file's commits. Because the decay
///   weight appears in **both** the numerator and denominator it largely
///   cancels: this factor is the share of (recency-weighted) touches that were
///   fixes, NOT a pure recency weight. In particular, for an all-fix file every
///   touch is a fix so the factor is exactly `1.0` regardless of how old the
///   commits are. Recency only shifts this factor when a file mixes fix and
///   non-fix commits at different ages (a recent fix among older features
///   raises it; an old fix among recent features lowers it).
/// - [`wilson_lower_bound`]`(fix_commits, total_commits)`: the confidence-adjusted
///   fix proportion read from the **raw** lifetime commit counts — *how much
///   evidence* there is, in `[0.0, 1.0]`. Reading raw counts here (not the
///   decay-weighted ratio) avoids a decay/raw-count sample-size mismatch. This
///   is the factor that actually fixes the #378 saturation bug: it is what
///   suppresses a tiny-sample file (a 1-fix/1-commit file whose
///   `decay_fix_factor` is also `1.0`) below a high-volume one.
///
/// The product stays in `[0.0, 1.0]` (both factors are in `[0,1]`), preserving
/// the `RiskRow::risk_score` doc contract and `{:.3}` formatting (AC3).
///
/// # Separation from `fix_density` (AD-378-3)
///
/// This is intentionally **distinct** from `RiskRow::fix_density`, which is the
/// bare raw ratio `fix_commits / total_commits` shown in the `Fix%` column —
/// note that even the `decay_fix_factor` input here (the *decay-weighted* fix
/// proportion) is a different quantity from that raw ratio. For any file with
/// `fix_commits != total_commits` the persisted `risk_score` and `fix_density`
/// differ, proving the two-field separation (AC5).
///
/// # Grounding (AD-378-2)
///
/// The choice of Wilson+decay over the bare ratio is validated by a temporal
/// predict-future-fixes backtest (ADR-003, tied to #361): risk is computed from
/// commits before a cutoff `T`, each file is labelled by whether it received a
/// fix-commit *after* `T` (reusing the heatmap fix-after-touch classifier
/// [`is_fix_commit`]), and rankers are scored by precision@N / NDCG against the
/// held-out future fixes. Wilson+decay MUST score >= the bare-ratio baseline
/// (AC9; see `risk_score_wilson_decay_beats_bare_ratio_on_backtest` in the unit
/// tests). The previously-used `VOLUME_REF` percentile grounding is removed
/// along with the constant.
///
/// # Boundary semantics (AC4)
///
/// `risk_score_wilson_decay(_, _, 0)` returns exactly `0.0` (no commits → the
/// Wilson factor is `0.0`, so the product is `0.0` regardless of the decay
/// factor).
///
/// # Examples
///
/// ```rust
/// use rskim_search::risk_score_wilson_decay;
///
/// // No commits → 0.0 regardless of decay factor.
/// assert_eq!(risk_score_wilson_decay(0.9, 0, 0), 0.0);
/// // With equal decay-weighted proportion (both all-fix → 1.0), a 1/1 file
/// // ranks strictly below a 50/50 file because Wilson suppresses the tiny
/// // sample (AC1).
/// let small = risk_score_wilson_decay(1.0, 1, 1);
/// let large = risk_score_wilson_decay(1.0, 50, 50);
/// assert!(small < large);
/// ```
#[must_use]
#[inline]
pub fn risk_score_wilson_decay(decay_fix_factor: f64, fix_commits: u32, total_commits: u32) -> f64 {
    decay_fix_factor * wilson_lower_bound(fix_commits, total_commits)
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
            (now_epoch - ts) as f64 / SECS_PER_DAY
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

/// Populate `buf` with the deduplicated set of file paths touched by `files`.
///
/// `buf` is cleared before insertion. Each path appears at most once regardless
/// of how many times the same file is listed in `files`. The caller reuses the
/// buffer across commits to avoid repeated allocation.
fn dedup_changed_files(files: &[crate::types::FileChangeInfo], buf: &mut HashSet<String>) {
    buf.clear();
    for file in files {
        buf.insert(file.path_str().into_owned());
    }
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
            (now_epoch - ts) as f64 / SECS_PER_DAY
        } else {
            // Future commit: treat as elapsed = 0 (within both windows).
            0.0
        };

        let in_30d = elapsed_days <= 30.0;
        let in_90d = elapsed_days <= 90.0;

        // Collect unique file paths for this commit into the reused buffer.
        dedup_changed_files(&commit.changed_files, &mut seen_in_commit);

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
