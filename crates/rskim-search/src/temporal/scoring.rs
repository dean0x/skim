//! Hotspot and risk scoring for the temporal search layer.
//!
//! Pure computation over a `&[CommitInfo]` slice — no I/O, no failure modes.
//!
//! # Hotspot scoring
//!
//! Ranks files by recent commit activity using 30-day and 90-day windows plus
//! an exponential decay weight (half-life = 30 days). The file with the highest
//! decayed weight receives `score = 1.0`; all others are normalized relative to
//! it. Files with no commits in the 90-day window are excluded.
//!
//! # Risk scoring
//!
//! Ranks files by fix-commit density (`fix_commits / total_commits`). Files
//! with fewer than [`MIN_COMMITS_FOR_RISK`] total commits receive
//! `fix_density = 0.0` to suppress false 1/1 = 1.0 positives. The file with
//! the highest density receives `score = 1.0`.

use std::path::PathBuf;

use rustc_hash::FxHashMap;

use crate::temporal::types::{CommitInfo, HotspotScore, RiskScore};

// ============================================================================
// Constants
// ============================================================================

/// Half-life for the exponential decay weight (in days).
const HALF_LIFE_DAYS: f32 = 30.0;

/// 30-day lookback window in seconds.
const HOTSPOT_30D_WINDOW: u64 = 30 * 86_400;

/// 90-day lookback window in seconds.
const HOTSPOT_90D_WINDOW: u64 = 90 * 86_400;

/// Minimum total commits required for a file to receive a non-zero risk score.
/// Below this threshold, a single fix on a barely-touched file would produce
/// a misleadingly high density.
const MIN_COMMITS_FOR_RISK: u32 = 3;

// ============================================================================
// Public API
// ============================================================================

/// Compute hotspot scores for all files referenced in `commits`.
///
/// Each file receives a normalized score in `[0, 1]` based on its exponentially
/// decayed commit weight. The `now` parameter is the current time as Unix epoch
/// seconds; it is injected so tests are deterministic.
///
/// # Behaviour
///
/// - Commits timestamped *after* `now` are skipped (clock-skew guard).
/// - Files with zero commits in the 90-day window are excluded.
/// - Output is sorted by score descending, then path ascending for tie-breaking.
/// - Returns an empty vec for empty input or when all commits are future-dated.
#[must_use = "hotspot_scores returns the scores; discarding them is likely a bug"]
pub fn hotspot_scores(commits: &[CommitInfo], now: u64) -> Vec<HotspotScore> {
    // Accumulate per-file: (commit_count_30d, commit_count_90d, decayed_weight)
    let mut per_file: FxHashMap<PathBuf, (u32, u32, f32)> = FxHashMap::default();

    for commit in commits {
        // Skip future-dated commits (clock skew).
        if commit.timestamp > now {
            continue;
        }
        let age_secs = now - commit.timestamp;
        let age_days = age_secs as f32 / 86_400.0;

        let in_30d = age_secs <= HOTSPOT_30D_WINDOW;
        let in_90d = age_secs <= HOTSPOT_90D_WINDOW;

        // Decay: weight halves every 30 days.
        // exp(-ln(2) * age_days / half_life) = 2^(-age_days / half_life)
        let decay = (-(2.0_f32).ln() * age_days / HALF_LIFE_DAYS).exp();

        for file in &commit.changed_files {
            let entry = per_file.entry(file.clone()).or_insert((0, 0, 0.0));
            if in_30d {
                entry.0 += 1;
            }
            if in_90d {
                entry.1 += 1;
            }
            entry.2 += decay;
        }
    }

    // Normalize decayed_weight to [0, 1] by dividing by the maximum.
    let max_weight = per_file
        .values()
        .map(|(_, _, w)| *w)
        .fold(0.0_f32, f32::max);

    let mut scores: Vec<HotspotScore> = per_file
        .into_iter()
        // Git-history-only scope: exclude files with no activity in 90 days.
        .filter(|(_, (_, c90, _))| *c90 > 0)
        .map(|(path, (c30, c90, w))| {
            let score = if max_weight > 0.0 {
                w / max_weight
            } else {
                0.0
            };
            HotspotScore {
                path,
                commit_count_30d: c30,
                commit_count_90d: c90,
                score,
            }
        })
        .collect();

    // Deterministic sort: score descending, path ascending for tie-breaking.
    scores.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });

    scores
}

/// Compute risk scores for all files referenced in `commits`.
///
/// Each file receives a normalized score in `[0, 1]` based on its fix-commit
/// density (`fix_commits / total_commits`). Files with fewer than
/// [`MIN_COMMITS_FOR_RISK`] total commits receive `fix_density = 0.0`.
///
/// # Behaviour
///
/// - All commits in the slice are used; time-based filtering is the caller's
///   responsibility (apply via `parse_history` lookback days).
/// - Output is sorted by score descending, then path ascending for tie-breaking.
/// - Returns an empty vec for empty input.
#[must_use = "risk_scores returns the scores; discarding them is likely a bug"]
pub fn risk_scores(commits: &[CommitInfo]) -> Vec<RiskScore> {
    // Accumulate per-file: (total_commits, fix_commits)
    let mut per_file: FxHashMap<PathBuf, (u32, u32)> = FxHashMap::default();

    for commit in commits {
        for file in &commit.changed_files {
            let entry = per_file.entry(file.clone()).or_insert((0, 0));
            entry.0 += 1;
            if commit.is_fix {
                entry.1 += 1;
            }
        }
    }

    // Compute fix_density per file; zero below minimum sample size.
    let raw: Vec<(PathBuf, u32, u32, f32)> = per_file
        .into_iter()
        .map(|(path, (total, fixes))| {
            let density = if total < MIN_COMMITS_FOR_RISK {
                0.0
            } else {
                fixes as f32 / total as f32
            };
            (path, total, fixes, density)
        })
        .collect();

    // Normalize score to [0, 1] by dividing by max density.
    let max_density = raw.iter().map(|(_, _, _, d)| *d).fold(0.0_f32, f32::max);

    let mut scores: Vec<RiskScore> = raw
        .into_iter()
        .map(|(path, total, fixes, density)| {
            let score = if max_density > 0.0 {
                density / max_density
            } else {
                0.0
            };
            RiskScore {
                path,
                total_commits: total,
                fix_commits: fixes,
                fix_density: density,
                score,
            }
        })
        .collect();

    // Deterministic sort: score descending, path ascending for tie-breaking.
    scores.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });

    scores
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_commit(hash: &str, timestamp: u64, is_fix: bool, files: &[&str]) -> CommitInfo {
        CommitInfo {
            hash: hash.to_string(),
            timestamp,
            message: hash.to_string(),
            is_fix,
            changed_files: files.iter().map(|p| PathBuf::from(p)).collect(),
        }
    }

    // ---- hotspot ----

    #[test]
    fn hotspot_empty_returns_empty() {
        let result = hotspot_scores(&[], 0);
        assert!(result.is_empty());
    }

    #[test]
    fn decay_halves_every_30_days() {
        let now: u64 = 86_400 * 100; // day 100
        let commits = vec![
            make_commit("a", now, false, &["a.rs"]), // 0 days old → weight ≈ 1.0
            make_commit("b", now - 30 * 86_400, false, &["b.rs"]), // 30 days old → weight ≈ 0.5
        ];
        let scores = hotspot_scores(&commits, now);

        let find = |name: &str| {
            scores
                .iter()
                .find(|s| s.path == PathBuf::from(name))
                .unwrap_or_else(|| unreachable!("missing {name}"))
        };

        let a = find("a.rs");
        let b = find("b.rs");

        // a.rs: max weight → score = 1.0
        assert!(
            (a.score - 1.0).abs() < 0.01,
            "a.score expected ~1.0, got {}",
            a.score
        );
        // b.rs: weight ≈ 0.5 → score ≈ 0.5
        assert!(
            (b.score - 0.5).abs() < 0.05,
            "b.score expected ~0.5, got {}",
            b.score
        );
    }

    #[test]
    fn hotspot_excludes_90d_cold_files() {
        let now: u64 = 86_400 * 200;
        // Commit is 100 days old — outside 90-day window.
        let commits = vec![make_commit("a", now - 100 * 86_400, false, &["cold.rs"])];
        let result = hotspot_scores(&commits, now);
        assert!(
            result.is_empty(),
            "file older than 90 days must be excluded; got {result:?}"
        );
    }

    #[test]
    fn hotspot_normalized_score_range() {
        let now: u64 = 86_400 * 100;
        let commits = vec![
            make_commit("a", now, false, &["a.rs"]),
            make_commit("b", now - 10 * 86_400, false, &["b.rs"]),
            make_commit("c", now - 20 * 86_400, false, &["c.rs"]),
        ];
        let scores = hotspot_scores(&commits, now);
        assert!(!scores.is_empty());

        let has_max = scores.iter().any(|s| (s.score - 1.0).abs() < 1e-6);
        assert!(has_max, "at least one score must equal 1.0");

        for s in &scores {
            assert!(
                s.score >= 0.0 && s.score <= 1.0,
                "score out of [0,1]: {}",
                s.score
            );
        }
    }

    // ---- risk ----

    #[test]
    fn risk_empty_returns_empty() {
        let result = risk_scores(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn risk_minimum_sample_size() {
        // 1 commit total, 1 fix → below MIN_COMMITS_FOR_RISK → density must be 0.0
        let commits = vec![make_commit("a", 0, true, &["rare.rs"])];
        let scores = risk_scores(&commits);
        let entry = scores
            .iter()
            .find(|s| s.path == PathBuf::from("rare.rs"))
            .unwrap_or_else(|| unreachable!("rare.rs must be present"));
        assert_eq!(
            entry.fix_density, 0.0,
            "below MIN_COMMITS_FOR_RISK → density must be 0.0"
        );
    }

    #[test]
    fn risk_density_computation() {
        // 10 total commits touching "main.rs", 3 of which are fixes.
        let mut commits = Vec::new();
        for i in 0..10_u64 {
            commits.push(make_commit(
                &format!("c{i}"),
                i * 100,
                i < 3, // first 3 are fixes
                &["main.rs"],
            ));
        }
        let scores = risk_scores(&commits);
        let entry = scores
            .iter()
            .find(|s| s.path == PathBuf::from("main.rs"))
            .unwrap_or_else(|| unreachable!("main.rs must be present"));
        assert!(
            (entry.fix_density - 0.3).abs() < 1e-6,
            "expected fix_density 0.3, got {}",
            entry.fix_density
        );
    }

    #[test]
    fn risk_normalized_score_range() {
        let commits = vec![
            make_commit("a", 0, false, &["a.rs"]),
            make_commit("b", 1, false, &["a.rs"]),
            make_commit("c", 2, true, &["a.rs"]),
            make_commit("d", 3, false, &["b.rs"]),
            make_commit("e", 4, true, &["b.rs"]),
            make_commit("f", 5, true, &["b.rs"]),
        ];
        let scores = risk_scores(&commits);
        assert!(!scores.is_empty());

        let has_max = scores.iter().any(|s| (s.score - 1.0).abs() < 1e-6);
        assert!(has_max, "at least one score must equal 1.0");

        for s in &scores {
            assert!(
                s.score >= 0.0 && s.score <= 1.0,
                "score out of [0,1]: {}",
                s.score
            );
        }
    }
}
