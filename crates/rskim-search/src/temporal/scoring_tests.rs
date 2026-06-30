//! Tests for [`decay_weight`], [`compute_file_risk_scores`], and
//! [`compute_file_temporal_stats`].
//!
//! Written test-first (RED phase) following the TDD cycle.
//! Groups match the plan: decay_weight unit tests, basic cases,
//! acceptance criteria, fix density specifics, edge cases, and determinism.

use std::path::PathBuf;

use crate::temporal::{compute_file_risk_scores, compute_file_temporal_stats, decay_weight};
use crate::types::{CommitInfo, FileChangeInfo};

// ============================================================================
// Test infrastructure
// ============================================================================

const EPSILON: f64 = 1e-9;
/// Deterministic "now" timestamp (~2023-11-14 UTC).
const NOW: u64 = 1_700_000_000;
const DAY: u64 = 86_400;
const HALF_LIFE: f64 = 30.0;

fn make_commit(ts: u64, message: &str, files: &[&str]) -> CommitInfo {
    CommitInfo {
        hash: format!("{ts:040x}"),
        timestamp: i64::try_from(ts).expect("timestamp overflow in test helper"),
        author: "test".to_string(),
        message: message.to_string(),
        changed_files: files
            .iter()
            .map(|p| FileChangeInfo {
                path: PathBuf::from(p),
                additions: 1,
                deletions: 0,
            })
            .collect(),
    }
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < EPSILON
}

// ============================================================================
// Group 1: decay_weight unit tests
// ============================================================================

/// Zero elapsed → weight = 1.0 (no decay).
#[test]
fn decay_zero_elapsed() {
    assert!(approx_eq(decay_weight(0.0, HALF_LIFE), 1.0));
}

/// One half-life elapsed → weight ≈ 1/e ≈ 0.3679.
#[test]
fn decay_one_half_life() {
    let w = decay_weight(30.0, HALF_LIFE);
    // exp(-1) ≈ 0.36787944117
    assert!((w - std::f64::consts::E.recip()).abs() < 1e-9);
}

/// Two half-lives elapsed → weight ≈ e^{-2} ≈ 0.1353.
#[test]
fn decay_two_half_lives() {
    let w = decay_weight(60.0, HALF_LIFE);
    let expected = (-2.0_f64).exp();
    assert!((w - expected).abs() < 1e-9);
}

/// Very large elapsed → weight > 0, < 1e-10, and finite (no underflow to zero or NaN).
#[test]
fn decay_very_large_elapsed() {
    let w = decay_weight(10_000.0, HALF_LIFE);
    assert!(w.is_finite());
    assert!(w > 0.0);
    assert!(w < 1e-10);
}

/// Negative elapsed is clamped to 1.0 (future commits treated as "now").
#[test]
fn decay_negative_elapsed_clamped() {
    // decay_weight with negative elapsed: exp(-(-5)/30) = exp(0.167) > 1 → clamped to 1.0
    assert!(approx_eq(decay_weight(-5.0, HALF_LIFE), 1.0));
}

/// Weight is monotonically decreasing as elapsed increases.
#[test]
fn decay_monotonically_decreasing() {
    let days = [0.0_f64, 10.0, 20.0, 30.0, 60.0, 90.0];
    let weights: Vec<f64> = days.iter().map(|&d| decay_weight(d, HALF_LIFE)).collect();
    for window in weights.windows(2) {
        assert!(
            window[0] >= window[1],
            "{} should be >= {}",
            window[0],
            window[1]
        );
    }
}

/// All outputs are in [0.0, 1.0] for diverse inputs.
#[test]
fn decay_always_in_unit_range() {
    let test_inputs = [
        (0.0, 1.0),
        (1.0, 30.0),
        (365.0, 30.0),
        (10_000.0, 1.0),
        (-100.0, 30.0),
        (0.001, 0.001),
        (999_999.0, 365.0),
        (0.0, 7.0),
        (30.0, 7.0),
        (90.0, 90.0),
    ];
    for (elapsed, half_life) in test_inputs {
        let w = decay_weight(elapsed, half_life);
        assert!(
            (0.0..=1.0_f64).contains(&w),
            "out of range for ({elapsed}, {half_life}): {w}"
        );
        assert!(w.is_finite(), "non-finite for ({elapsed}, {half_life})");
    }
}

/// `decay_weight` with zero half-life panics unconditionally (assert!).
#[test]
#[should_panic(expected = "half_life_days must be positive and finite")]
fn decay_zero_half_life_panics() {
    let _ = decay_weight(1.0, 0.0);
}

/// `decay_weight` with NaN half-life panics unconditionally (assert!).
#[test]
#[should_panic(expected = "half_life_days must be positive and finite")]
fn decay_nan_half_life_panics() {
    let _ = decay_weight(1.0, f64::NAN);
}

/// `decay_weight` with NaN elapsed_days — result must be finite and in [0.0, 1.0].
///
/// NaN inputs must not propagate or cause a panic; the function should return a
/// well-defined value in the valid output range.
#[test]
fn decay_nan_elapsed_does_not_propagate() {
    let w = decay_weight(f64::NAN, HALF_LIFE);
    assert!(
        w.is_finite() && (0.0..=1.0_f64).contains(&w),
        "expected finite value in [0,1] for NaN elapsed, got {w}"
    );
}

/// `decay_weight` with positive Infinity elapsed_days — result must be finite and in [0.0, 1.0].
///
/// exp(-∞) = 0.0, which is a valid lower bound.
#[test]
fn decay_positive_infinity_elapsed() {
    let w = decay_weight(f64::INFINITY, HALF_LIFE);
    assert!(
        w.is_finite() && (0.0..=1.0_f64).contains(&w),
        "expected finite value in [0,1] for +Inf elapsed, got {w}"
    );
    // exp(-Inf) = 0.0 clamped → 0.0
    assert!(approx_eq(w, 0.0), "expected 0.0, got {w}");
}

/// `decay_weight` with negative Infinity elapsed_days — result must be clamped to 1.0.
///
/// exp(+∞) = +∞ → clamped to 1.0.
#[test]
fn decay_negative_infinity_elapsed() {
    let w = decay_weight(f64::NEG_INFINITY, HALF_LIFE);
    assert!(
        w.is_finite() && (0.0..=1.0_f64).contains(&w),
        "expected finite value in [0,1] for -Inf elapsed, got {w}"
    );
    // exp(+Inf) = +Inf → clamped to 1.0
    assert!(approx_eq(w, 1.0), "expected 1.0, got {w}");
}

/// `compute_file_risk_scores` with zero half-life panics unconditionally (assert!).
#[test]
#[should_panic(expected = "half_life_days must be positive")]
fn compute_scores_zero_half_life_panics() {
    let commits = vec![make_commit(NOW, "feat", &["a.rs"])];
    let _ = compute_file_risk_scores(&commits, NOW, 0.0);
}

/// `compute_file_risk_scores` with negative half-life panics unconditionally (assert!).
#[test]
#[should_panic(expected = "half_life_days must be positive")]
fn compute_scores_negative_half_life_panics() {
    let commits = vec![make_commit(NOW, "feat", &["a.rs"])];
    let _ = compute_file_risk_scores(&commits, NOW, -30.0);
}

// ============================================================================
// Group 2: Basic cases
// ============================================================================

/// Empty commit slice → empty HashMap.
#[test]
fn empty_commits() {
    let scores = compute_file_risk_scores(&[], NOW, HALF_LIFE);
    assert!(scores.is_empty());
}

/// Single non-fix commit at NOW for one file → hotspot=1.0, fix_density=0.0.
#[test]
fn single_commit_single_file() {
    let commits = vec![make_commit(NOW, "add feature", &["a.rs"])];
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    assert_eq!(scores.len(), 1);
    let s = &scores["a.rs"];
    assert!(approx_eq(s.hotspot, 1.0), "hotspot={}", s.hotspot);
    assert!(
        approx_eq(s.fix_density, 0.0),
        "fix_density={}",
        s.fix_density
    );
}

/// Single fix commit at NOW for one file → hotspot=1.0, fix_density=1.0.
#[test]
fn single_fix_commit() {
    let commits = vec![make_commit(NOW, "fix: null dereference", &["a.rs"])];
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    let s = &scores["a.rs"];
    assert!(approx_eq(s.hotspot, 1.0), "hotspot={}", s.hotspot);
    assert!(
        approx_eq(s.fix_density, 1.0),
        "fix_density={}",
        s.fix_density
    );
}

/// Single commit touching multiple files — all files share the same decay weight
/// and normalize to hotspot=1.0.
///
/// Verifies that a single wide-impact commit distributes the same weight to every
/// file it touches, so all three reach the maximum after normalization.
#[test]
fn single_commit_multiple_files_same_weight() {
    let commits = vec![make_commit(
        NOW - 10 * DAY,
        "feat: wide change",
        &["a.rs", "b.rs", "c.rs"],
    )];
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    assert_eq!(scores.len(), 3, "expected 3 file entries");

    // All three files have the same raw weight; after max-normalization each is 1.0.
    let expected = scores["a.rs"].hotspot;
    assert!(
        approx_eq(expected, 1.0),
        "a.rs hotspot should be 1.0, got {expected}"
    );
    assert!(
        approx_eq(scores["b.rs"].hotspot, expected),
        "b.rs hotspot {} != a.rs hotspot {}",
        scores["b.rs"].hotspot,
        expected
    );
    assert!(
        approx_eq(scores["c.rs"].hotspot, expected),
        "c.rs hotspot {} != a.rs hotspot {}",
        scores["c.rs"].hotspot,
        expected
    );
}

/// Two commits at NOW, each touching a different file → both hotspot=1.0
/// (both have same weight so both normalize to max).
#[test]
fn two_files_equal_weight() {
    let commits = vec![
        make_commit(NOW, "add a", &["a.rs"]),
        make_commit(NOW, "add b", &["b.rs"]),
    ];
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    assert_eq!(scores.len(), 2);
    let sa = &scores["a.rs"];
    let sb = &scores["b.rs"];
    assert!(approx_eq(sa.hotspot, 1.0));
    assert!(approx_eq(sb.hotspot, 1.0));
}

// ============================================================================
// Group 3: Acceptance criteria
// ============================================================================

/// File with more recent commits ranks higher in hotspot than a rarely-changed file.
#[test]
fn high_frequency_ranks_higher() {
    let mut commits = Vec::new();
    for i in 0..50 {
        commits.push(make_commit(NOW - i * DAY, "feat: something", &["hot.rs"]));
    }
    for i in 0..5 {
        commits.push(make_commit(NOW - i * DAY, "feat: something", &["cold.rs"]));
    }
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    assert!(scores["hot.rs"].hotspot > scores["cold.rs"].hotspot);
}

/// The maximum hotspot across all files must be exactly 1.0.
#[test]
fn hotspot_max_is_one() {
    let commits = vec![
        make_commit(NOW, "feat", &["a.rs", "b.rs"]),
        make_commit(NOW - 5 * DAY, "feat", &["b.rs", "c.rs"]),
        make_commit(NOW - 10 * DAY, "feat", &["a.rs"]),
    ];
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    let max = scores
        .values()
        .map(|s| s.hotspot)
        .fold(f64::NEG_INFINITY, f64::max);
    assert!(approx_eq(max, 1.0), "max={max}");
}

/// All non-fix commits → all files have fix_density=0.0.
#[test]
fn zero_fix_density() {
    let commits: Vec<CommitInfo> = (0..10)
        .map(|i| make_commit(NOW - i * DAY, "add feature", &["a.rs"]))
        .collect();
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    for (path, s) in &scores {
        assert!(
            approx_eq(s.fix_density, 0.0),
            "{path}: fix_density={}",
            s.fix_density
        );
    }
}

/// All fix commits → all files have fix_density=1.0.
#[test]
fn all_fix_density_one() {
    let commits: Vec<CommitInfo> = (0..5)
        .map(|i| make_commit(NOW - i * DAY, "fix: something", &["a.rs"]))
        .collect();
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    for (path, s) in &scores {
        assert!(
            approx_eq(s.fix_density, 1.0),
            "{path}: fix_density={}",
            s.fix_density
        );
    }
}

/// All scores in [0.0, 1.0] for mixed commits and multiple files.
#[test]
fn scores_in_unit_range() {
    let files = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"];
    let messages = [
        "fix: bug",
        "feat: thing",
        "bug: crash",
        "add stuff",
        "revert bad",
    ];
    let mut commits = Vec::new();
    for (i, (file, msg)) in files
        .iter()
        .zip(messages.iter())
        .cycle()
        .take(20)
        .enumerate()
    {
        commits.push(make_commit(NOW - (i as u64) * DAY, msg, &[file]));
    }
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    for (path, s) in &scores {
        assert!(
            s.hotspot >= 0.0 && s.hotspot <= 1.0,
            "{path}: hotspot={}",
            s.hotspot
        );
        assert!(
            s.fix_density >= 0.0 && s.fix_density <= 1.0,
            "{path}: fix_density={}",
            s.fix_density
        );
    }
}

/// File with a recent commit ranks higher in hotspot than a file with only an old commit.
#[test]
fn decay_affects_ranking() {
    let commits = vec![
        make_commit(NOW, "feat", &["new.rs"]),
        make_commit(NOW - 60 * DAY, "feat", &["old.rs"]),
    ];
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    assert!(scores["new.rs"].hotspot > scores["old.rs"].hotspot);
}

// ============================================================================
// Group 4: Fix density specifics
// ============================================================================

/// 2 fix + 2 non-fix commits at same timestamp → density ≈ 0.5.
#[test]
fn mixed_fix_density() {
    let commits = vec![
        make_commit(NOW, "fix: bug", &["a.rs"]),
        make_commit(NOW, "fix: crash", &["a.rs"]),
        make_commit(NOW, "feat: add x", &["a.rs"]),
        make_commit(NOW, "refactor: cleanup", &["a.rs"]),
    ];
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    let density = scores["a.rs"].fix_density;
    assert!(
        (density - 0.5).abs() < EPSILON,
        "expected ~0.5, got {density}"
    );
}

/// Fix commit is recent; non-fix is old → decay-weighted density > 0.5.
#[test]
fn density_decay_weighted() {
    let commits = vec![
        make_commit(NOW, "fix: bug", &["a.rs"]),
        make_commit(NOW - 90 * DAY, "feat: add thing", &["a.rs"]),
    ];
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    let density = scores["a.rs"].fix_density;
    assert!(density > 0.5, "expected > 0.5, got {density}");
}

/// "a.rs" appears only in fix commits → density=1.0; "b.rs" only in non-fix → density=0.0.
#[test]
fn per_file_independent_density() {
    let commits = vec![
        make_commit(NOW, "fix: bug", &["a.rs"]),
        make_commit(NOW, "feat: add", &["b.rs"]),
    ];
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    assert!(approx_eq(scores["a.rs"].fix_density, 1.0));
    assert!(approx_eq(scores["b.rs"].fix_density, 0.0));
}

/// All fix keywords are recognized: fix:, bug:, hotfix:, patch:, revert:.
#[test]
fn fix_keywords_recognized() {
    let commits = vec![
        make_commit(NOW, "fix: null pointer", &["a.rs"]),
        make_commit(NOW, "bug: crash on startup", &["a.rs"]),
        make_commit(NOW, "hotfix: security patch", &["a.rs"]),
        make_commit(NOW, "patch: minor correction", &["a.rs"]),
        make_commit(NOW, "revert: bad feature", &["a.rs"]),
    ];
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    assert!(
        approx_eq(scores["a.rs"].fix_density, 1.0),
        "fix_density={}",
        scores["a.rs"].fix_density
    );
}

// ============================================================================
// Group 5: Edge cases
// ============================================================================

/// Commit with timestamp in the future → treated as elapsed=0, hotspot=1.0.
#[test]
fn future_timestamp() {
    let commits = vec![make_commit(NOW + 10 * DAY, "feat", &["a.rs"])];
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    assert!(approx_eq(scores["a.rs"].hotspot, 1.0));
}

/// Commit with negative timestamp → no panic, score in [0, 1].
#[test]
fn negative_timestamp() {
    // Construct directly because make_commit uses ts as i64 internally but u64 input.
    let commit = CommitInfo {
        hash: "0000000000000000000000000000000000000000".to_string(),
        timestamp: -1000_i64,
        author: "test".to_string(),
        message: "feat: old".to_string(),
        changed_files: vec![FileChangeInfo {
            path: PathBuf::from("old.rs"),
            additions: 1,
            deletions: 0,
        }],
    };
    let scores = compute_file_risk_scores(&[commit], NOW, HALF_LIFE);
    let s = &scores["old.rs"];
    assert!(
        s.hotspot >= 0.0 && s.hotspot <= 1.0,
        "hotspot={}",
        s.hotspot
    );
    assert!(
        s.fix_density >= 0.0 && s.fix_density <= 1.0,
        "fix_density={}",
        s.fix_density
    );
    assert!(s.hotspot.is_finite());
    assert!(s.fix_density.is_finite());
}

/// Very old commits (10,000 days) → finite scores in [0, 1], no NaN.
#[test]
fn very_old_commits() {
    let commits: Vec<CommitInfo> = (0..5)
        .map(|i| {
            let ts = if NOW > 10_000 * DAY + i * DAY {
                NOW - 10_000 * DAY - i * DAY
            } else {
                0
            };
            CommitInfo {
                hash: format!("{i:040}"),
                timestamp: ts as i64,
                author: "test".to_string(),
                message: "feat: ancient".to_string(),
                changed_files: vec![FileChangeInfo {
                    path: PathBuf::from(format!("file{i}.rs")),
                    additions: 1,
                    deletions: 0,
                }],
            }
        })
        .collect();
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    for (path, s) in &scores {
        assert!(s.hotspot.is_finite(), "{path}: hotspot is not finite");
        assert!(
            s.fix_density.is_finite(),
            "{path}: fix_density is not finite"
        );
        assert!(
            s.hotspot >= 0.0 && s.hotspot <= 1.0,
            "{path}: hotspot={}",
            s.hotspot
        );
        assert!(
            s.fix_density >= 0.0 && s.fix_density <= 1.0,
            "{path}: fix_density={}",
            s.fix_density
        );
    }
}

/// Single file in all commits → hotspot = 1.0 (max-normalized to itself).
#[test]
fn single_file_all_commits() {
    let commits: Vec<CommitInfo> = (0..10)
        .map(|i| make_commit(NOW - i * DAY, "feat: something", &["only.rs"]))
        .collect();
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    assert_eq!(scores.len(), 1);
    assert!(approx_eq(scores["only.rs"].hotspot, 1.0));
}

/// Every file in the result has both `hotspot` and `fix_density` in range [0, 1].
#[test]
fn all_files_have_valid_scores() {
    let commits = vec![
        make_commit(NOW, "fix: bug", &["a.rs", "b.rs"]),
        make_commit(NOW - DAY, "feat", &["b.rs", "c.rs"]),
    ];
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    for (path, s) in &scores {
        assert!(s.hotspot >= 0.0 && s.hotspot <= 1.0, "{path}");
        assert!(s.fix_density >= 0.0 && s.fix_density <= 1.0, "{path}");
    }
}

// ============================================================================
// Group 6: Determinism
// ============================================================================

/// Calling compute_file_risk_scores 50 times with the same input yields identical results.
#[test]
fn deterministic_results() {
    let commits: Vec<CommitInfo> = (0..10)
        .map(|i| {
            let msg = if i % 3 == 0 {
                "fix: something"
            } else {
                "feat: thing"
            };
            make_commit(NOW - i * DAY, msg, &["a.rs", "b.rs"])
        })
        .collect();

    let first = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    for _ in 0..49 {
        let result = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
        for (path, s) in &result {
            let f = &first[path];
            assert!(approx_eq(s.hotspot, f.hotspot), "{path}: hotspot differs");
            assert!(
                approx_eq(s.fix_density, f.fix_density),
                "{path}: fix_density differs"
            );
        }
    }
}

/// Shorter half-life penalizes old commits more than longer half-life.
#[test]
fn half_life_parameter_varies() {
    // One recent commit, one old commit (60 days ago), same file.
    let commits = vec![
        make_commit(NOW, "feat: recent", &["a.rs"]),
        make_commit(NOW - 60 * DAY, "feat: old", &["b.rs"]),
    ];
    let scores_short = compute_file_risk_scores(&commits, NOW, 7.0);
    let scores_long = compute_file_risk_scores(&commits, NOW, 365.0);

    // With short half-life, "b.rs" is much less significant relative to "a.rs"
    // → shorter half-life gives lower hotspot for the old file.
    let ratio_short = scores_short["b.rs"].hotspot / scores_short["a.rs"].hotspot;
    let ratio_long = scores_long["b.rs"].hotspot / scores_long["a.rs"].hotspot;
    assert!(
        ratio_short < ratio_long,
        "short={ratio_short}, long={ratio_long}"
    );
}

// ============================================================================
// Group: compute_file_temporal_stats
// ============================================================================

/// Empty commit slice → empty HashMap.
#[test]
fn temporal_stats_empty_commits() {
    let stats = compute_file_temporal_stats(&[], NOW);
    assert!(stats.is_empty());
}

/// Single commit today, non-fix, 1 file → {30d:1, 90d:1, total:1, fix:0}.
#[test]
fn temporal_stats_single_commit_today() {
    let commits = vec![make_commit(NOW, "feat: add feature", &["a.rs"])];
    let stats = compute_file_temporal_stats(&commits, NOW);
    let s = &stats["a.rs"];
    assert_eq!(s.changes_30d, 1);
    assert_eq!(s.changes_90d, 1);
    assert_eq!(s.total_commits, 1);
    assert_eq!(s.fix_commits, 0);
}

/// Single fix commit → fix_commits = 1.
#[test]
fn temporal_stats_fix_commit() {
    let commits = vec![make_commit(NOW, "fix: crash on startup", &["b.rs"])];
    let stats = compute_file_temporal_stats(&commits, NOW);
    let s = &stats["b.rs"];
    assert_eq!(s.fix_commits, 1);
    assert_eq!(s.total_commits, 1);
}

/// Commit at 45 days ago → in 90d window but not 30d window.
#[test]
fn temporal_stats_commit_at_45_days() {
    let commits = vec![make_commit(NOW - 45 * DAY, "feat: old", &["c.rs"])];
    let stats = compute_file_temporal_stats(&commits, NOW);
    let s = &stats["c.rs"];
    assert_eq!(s.changes_30d, 0);
    assert_eq!(s.changes_90d, 1);
    assert_eq!(s.total_commits, 1);
}

/// Commit at 100 days ago → outside both windows, but counted in total.
#[test]
fn temporal_stats_commit_at_100_days() {
    let commits = vec![make_commit(NOW - 100 * DAY, "feat: ancient", &["d.rs"])];
    let stats = compute_file_temporal_stats(&commits, NOW);
    let s = &stats["d.rs"];
    assert_eq!(s.changes_30d, 0);
    assert_eq!(s.changes_90d, 0);
    assert_eq!(s.total_commits, 1);
}

/// Commit at exactly 30.0 days → included in changes_30d (boundary inclusive).
#[test]
fn temporal_stats_boundary_30_days() {
    let commits = vec![make_commit(NOW - 30 * DAY, "feat: boundary", &["e.rs"])];
    let stats = compute_file_temporal_stats(&commits, NOW);
    let s = &stats["e.rs"];
    assert_eq!(
        s.changes_30d, 1,
        "commit at exactly 30 days must be included in 30d window"
    );
    assert_eq!(s.changes_90d, 1);
}

/// Commit at exactly 90.0 days → included in changes_90d (boundary inclusive).
#[test]
fn temporal_stats_boundary_90_days() {
    let commits = vec![make_commit(NOW - 90 * DAY, "feat: boundary90", &["f.rs"])];
    let stats = compute_file_temporal_stats(&commits, NOW);
    let s = &stats["f.rs"];
    assert_eq!(s.changes_30d, 0);
    assert_eq!(
        s.changes_90d, 1,
        "commit at exactly 90 days must be included in 90d window"
    );
    assert_eq!(s.total_commits, 1);
}

/// Future-dated commit (timestamp > now_epoch) → elapsed = 0, counted in both windows.
#[test]
fn temporal_stats_future_commit() {
    let future_ts = NOW + 10 * DAY;
    let commits = vec![make_commit(future_ts, "feat: future", &["g.rs"])];
    let stats = compute_file_temporal_stats(&commits, NOW);
    let s = &stats["g.rs"];
    assert_eq!(s.changes_30d, 1);
    assert_eq!(s.changes_90d, 1);
    assert_eq!(s.total_commits, 1);
}

/// Commit touches files a.rs and b.rs → both files get independent entries.
#[test]
fn temporal_stats_multiple_files() {
    let commits = vec![make_commit(NOW, "feat: multi", &["a.rs", "b.rs"])];
    let stats = compute_file_temporal_stats(&commits, NOW);
    assert!(stats.contains_key("a.rs"));
    assert!(stats.contains_key("b.rs"));
    let a = &stats["a.rs"];
    let b = &stats["b.rs"];
    assert_eq!(a.total_commits, 1);
    assert_eq!(b.total_commits, 1);
}

/// Commit lists a.rs twice in changed_files → deduplicated, total_commits = 1.
#[test]
fn temporal_stats_dedup_within_commit() {
    // Build the commit manually with a duplicate file path.
    let commit = CommitInfo {
        hash: "0".repeat(40),
        timestamp: i64::try_from(NOW).expect("timestamp overflow"),
        author: "test".to_string(),
        message: "feat: dup".to_string(),
        changed_files: vec![
            FileChangeInfo {
                path: std::path::PathBuf::from("a.rs"),
                additions: 1,
                deletions: 0,
            },
            FileChangeInfo {
                path: std::path::PathBuf::from("a.rs"),
                additions: 2,
                deletions: 0,
            },
        ],
    };
    let stats = compute_file_temporal_stats(&[commit], NOW);
    let s = &stats["a.rs"];
    assert_eq!(
        s.total_commits, 1,
        "duplicate file in single commit must be counted once"
    );
    assert_eq!(s.changes_30d, 1);
}

/// Three separate commits all touch the same file → total_commits = 3.
#[test]
fn temporal_stats_multiple_commits_same_file() {
    let commits = vec![
        make_commit(NOW, "feat: first", &["lib.rs"]),
        make_commit(NOW - 10 * DAY, "feat: second", &["lib.rs"]),
        make_commit(NOW - 20 * DAY, "feat: third", &["lib.rs"]),
    ];
    let stats = compute_file_temporal_stats(&commits, NOW);
    let s = &stats["lib.rs"];
    assert_eq!(s.total_commits, 3);
    assert_eq!(s.changes_30d, 3);
    assert_eq!(s.changes_90d, 3);
}

/// Negative timestamp → clamped to 0, very large elapsed → outside both windows.
#[test]
fn temporal_stats_negative_timestamp() {
    let commit = CommitInfo {
        hash: "1".repeat(40),
        timestamp: -1_000_000,
        author: "test".to_string(),
        message: "feat: ancient".to_string(),
        changed_files: vec![FileChangeInfo {
            path: std::path::PathBuf::from("old.rs"),
            additions: 1,
            deletions: 0,
        }],
    };
    let stats = compute_file_temporal_stats(&[commit], NOW);
    let s = &stats["old.rs"];
    // Clamped to 0 → ts = 0, elapsed = NOW/86400 days (≫ 90 days).
    assert_eq!(s.changes_30d, 0, "negative timestamp should be outside 30d");
    assert_eq!(s.changes_90d, 0, "negative timestamp should be outside 90d");
    assert_eq!(s.total_commits, 1, "should still be counted in total");
}

// ============================================================================
// #378 — Volume-weighting: wilson_lower_bound + risk_score_wilson_decay
// ============================================================================

use crate::temporal::{is_fix_commit, risk_score_wilson_decay, wilson_lower_bound};

/// AC4 (boundary): wilson_lower_bound(0, 0) MUST return exactly 0.0.
#[test]
fn wilson_zero_trials_is_exactly_zero() {
    assert_eq!(
        wilson_lower_bound(0, 0),
        0.0,
        "no observations → exactly 0.0 (no 0/0 NaN)"
    );
}

/// AC4 (boundary): zero successes MUST yield exactly 0.0 (lower bound clamped at 0).
#[test]
fn wilson_zero_successes_is_exactly_zero() {
    for trials in [1u32, 5, 50, 5000] {
        assert_eq!(
            wilson_lower_bound(0, trials),
            0.0,
            "0 successes / {trials} trials → Wilson LB clamped to 0.0"
        );
    }
}

/// AC1 (core saturation fix): a 1/1 sample MUST score strictly below a 50/50
/// sample, and the magnitudes match the documented Wilson values (~0.21 < ~0.93).
#[test]
fn wilson_small_sample_below_large_sample() {
    let small = wilson_lower_bound(1, 1);
    let large = wilson_lower_bound(50, 50);
    assert!(
        small < large,
        "Wilson(1,1)={small:.4} must be < Wilson(50,50)={large:.4} (AC1 saturation fix)"
    );
    // Documented magnitudes (AD-378-1): 1/1 ≈ 0.21, 50/50 ≈ 0.93.
    assert!(
        (0.15..0.30).contains(&small),
        "Wilson(1,1) should be ~0.21, got {small:.4}"
    );
    assert!(
        (0.85..0.97).contains(&large),
        "Wilson(50,50) should be ~0.93, got {large:.4}"
    );
}

/// AC3 (output range): wilson_lower_bound MUST stay finite within [0,1] across a
/// grid of (successes, trials) including the degenerate and large-n cases.
#[test]
fn wilson_output_always_in_unit_range() {
    for &trials in &[0u32, 1, 5, 50, 5000] {
        // Probe successes from 0..=trials plus an out-of-range value (clamped).
        let probes = [0, trials / 4, trials / 2, trials, trials.saturating_add(7)];
        for &successes in &probes {
            let lb = wilson_lower_bound(successes, trials);
            assert!(
                lb.is_finite() && (0.0..=1.0).contains(&lb),
                "Wilson({successes},{trials}) = {lb} out of [0,1] / not finite"
            );
        }
    }
}

/// AC2 (conditional monotonicity): for FIXED proportion p_hat = s/n, the Wilson
/// lower bound is monotone non-decreasing in n (more evidence at the same
/// proportion never lowers the score).
#[test]
fn wilson_monotone_in_n_at_fixed_proportion() {
    // p_hat ∈ {0.25, 0.5, 1.0}; n grid chosen so s = p_hat * n is integral.
    let cases: &[(u32, u32)] = &[(1, 4), (1, 2), (1, 1)]; // (num, den) for p_hat
    for &(num, den) in cases {
        let ns = [den, den * 2, den * 5, den * 10, den * 50, den * 100];
        let mut prev = f64::NEG_INFINITY;
        for &n in &ns {
            let s = num * (n / den); // exact since n is a multiple of den
            let lb = wilson_lower_bound(s, n);
            assert!(
                lb >= prev - 1e-12,
                "Wilson must be non-decreasing in n at fixed p_hat={num}/{den}: \
                 n={n} s={s} lb={lb:.6} < prev={prev:.6}"
            );
            prev = lb;
        }
    }
}

/// AC1 (full score): risk_score_wilson_decay reproduces the small-below-large
/// ordering at equal decay, and equals decay × Wilson exactly.
#[test]
fn risk_score_small_sample_below_large_sample() {
    let small = risk_score_wilson_decay(1.0, 1, 1);
    let large = risk_score_wilson_decay(1.0, 50, 50);
    assert!(
        small < large,
        "risk_score(1.0,1,1)={small:.4} must be < risk_score(1.0,50,50)={large:.4} (AC1)"
    );
    // Composition identity: decay × Wilson.
    let expected = 0.5 * wilson_lower_bound(3, 8);
    assert!(
        (risk_score_wilson_decay(0.5, 3, 8) - expected).abs() < 1e-12,
        "risk_score must equal decay_fix_factor * wilson_lower_bound"
    );
}

/// AC4 (boundary): risk_score_wilson_decay(_, _, 0) MUST return exactly 0.0
/// regardless of the decay factor.
#[test]
fn risk_score_zero_total_commits_is_exactly_zero() {
    for decay in [0.0, 0.5, 0.9, 1.0] {
        assert_eq!(
            risk_score_wilson_decay(decay, 0, 0),
            0.0,
            "0 total commits → exactly 0.0 (Wilson factor is 0) for decay={decay}"
        );
    }
}

/// AC3 (output range): risk_score_wilson_decay stays in [0,1] across a grid of
/// decay factors and (fix_commits ≤ total_commits) counts.
#[test]
fn risk_score_output_always_in_unit_range() {
    for &decay in &[0.0, 0.25, 0.5, 1.0] {
        for &total in &[0u32, 1, 5, 50, 5000] {
            for &fix in &[0, total / 2, total] {
                let s = risk_score_wilson_decay(decay, fix, total);
                assert!(
                    s.is_finite() && (0.0..=1.0).contains(&s),
                    "risk_score(decay={decay}, fix={fix}, total={total}) = {s} out of [0,1]"
                );
            }
        }
    }
}

// ----------------------------------------------------------------------------
// AC9 / AD-378-2 (grounding): temporal predict-future-fixes backtest.
//
// Methodology (ADR-003): risk is computed from commits BEFORE a cutoff T; each
// file is labelled by whether it received a fix-commit AFTER T (reusing the
// `is_fix_commit` fix-after-touch classifier); both rankers are scored by
// precision@N against the held-out future fixes. Wilson+decay MUST score >= the
// bare-ratio baseline. This test FAILS if Wilson+decay underperforms baseline —
// i.e. if the volume-weighting (the `risk_score_wilson_decay` Wilson factor)
// were dropped, the bare-ratio ranker would be misled by saturated tiny samples
// and the assert would break.
// ----------------------------------------------------------------------------

/// Build the BEFORE-T training history: a deterministic set of files spanning
/// the saturation trap (tiny 1/1 100%-fix files) and genuinely fix-prone
/// high-volume files. Returns the commit list.
///
/// `t_cutoff` is the cutoff epoch; all returned commits are strictly before it.
fn backtest_before_commits(t_cutoff: u64) -> Vec<CommitInfo> {
    // Helper: a commit `days_before` the cutoff.
    let before = |days_before: u64, msg: &str, files: &[&str]| {
        make_commit(t_cutoff - days_before * DAY, msg, files)
    };

    let mut commits = Vec::new();

    // --- TRAP: tiny-sample 100%-fix files (bare ratio = 1.0, Wilson ≈ 0.21) ---
    // Each appears in exactly ONE fix commit before T and is NEVER fixed after T.
    // The bare-ratio ranker puts these at the very top (ratio 1.0); Wilson does not.
    for i in 0..6 {
        let f = format!("trap_tiny_{i}.rs");
        commits.push(before(10 + i, "fix: one-off typo", &[f.as_str()]));
    }

    // --- SIGNAL: high-volume genuinely fix-prone files ---
    // ~40 fixes / ~50 commits → bare ratio 0.8 (BELOW the traps' 1.0) but a far
    // higher Wilson LB (≈ 0.69). These ARE fixed again after T.
    for hot in ["hot_core_a.rs", "hot_core_b.rs", "hot_core_c.rs"] {
        for k in 0..50u64 {
            // 40 of 50 commits are fixes; vary the day so timestamps are distinct.
            let msg = if k < 40 { "fix: recurring bug" } else { "add: feature" };
            commits.push(before(1 + (k % 60), msg, &[hot]));
        }
    }

    // --- NOISE: high-volume low-fix files (bare ratio ≈ 0.1, never fixed after T) ---
    for cold in ["stable_x.rs", "stable_y.rs"] {
        for k in 0..40u64 {
            let msg = if k < 4 { "fix: rare" } else { "refactor: cleanup" };
            commits.push(before(2 + (k % 50), msg, &[cold]));
        }
    }

    commits
}

/// Precision@N for a ranker: of the top-`n` files it ranks, the fraction that
/// were actually fixed after the cutoff (`future_fixed`).
fn precision_at_n(
    ranked: &[String],
    future_fixed: &std::collections::HashSet<String>,
    n: usize,
) -> f64 {
    let top = &ranked[..n.min(ranked.len())];
    if top.is_empty() {
        return 0.0;
    }
    let hits = top.iter().filter(|f| future_fixed.contains(*f)).count();
    hits as f64 / top.len() as f64
}

/// AC9 (grounding, ADR-003): Wilson+decay precision@N MUST be >= the bare-ratio
/// baseline on a temporal predict-future-fixes backtest. Falsifiable: if the
/// Wilson volume-weighting were removed (ranker reverts to the bare ratio), the
/// saturated tiny-sample traps would dominate the top of the ranking, the
/// future-fix-prone files would be buried, and Wilson precision would NO LONGER
/// exceed baseline — failing this assertion.
#[test]
fn risk_score_wilson_decay_beats_bare_ratio_on_backtest() {
    use std::collections::HashSet;

    // Cutoff T well after the epoch so all "before" timestamps are positive.
    let t_cutoff: u64 = NOW;

    // ---- Phase 1: compute risk from commits BEFORE T (two rankers) ----
    let before = backtest_before_commits(t_cutoff);
    // Decay factor source (decay-weighted fix proportion) — the real build input.
    let risk_scores = compute_file_risk_scores(&before, t_cutoff, HALF_LIFE);
    // Raw lifetime (fix_commits, total_commits) per file — the Wilson input.
    let stats = compute_file_temporal_stats(&before, t_cutoff);

    // Build (file, bare_ratio, wilson_decay) tuples over the union of files.
    let mut files: Vec<String> = stats.keys().cloned().collect();
    files.sort(); // deterministic base order before sorting by score
    let bare_ratio = |f: &str| -> f64 {
        let s = &stats[f];
        if s.total_commits > 0 {
            f64::from(s.fix_commits) / f64::from(s.total_commits)
        } else {
            0.0
        }
    };
    let wilson_decay = |f: &str| -> f64 {
        let s = &stats[f];
        let decay = risk_scores.get(f).map(|r| r.fix_density).unwrap_or(0.0);
        risk_score_wilson_decay(decay, s.fix_commits, s.total_commits)
    };

    // Rank each way (DESC by score, ties broken by path ASC for determinism).
    let rank_by = |score: &dyn Fn(&str) -> f64| -> Vec<String> {
        let mut v = files.clone();
        v.sort_by(|a, b| {
            score(b)
                .partial_cmp(&score(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.cmp(b))
        });
        v
    };
    let ranked_bare = rank_by(&bare_ratio);
    let ranked_wilson = rank_by(&wilson_decay);

    // ---- Phase 2: label each file by a fix-commit AFTER T (held-out) ----
    // Only the genuinely fix-prone "hot_core_*" files receive future fixes.
    let after_commits = [
        make_commit(t_cutoff + DAY, "fix: regression after release", &["hot_core_a.rs"]),
        make_commit(t_cutoff + 2 * DAY, "fix: edge case", &["hot_core_b.rs"]),
        make_commit(t_cutoff + 3 * DAY, "fix: crash on startup", &["hot_core_c.rs"]),
    ];
    let mut future_fixed: HashSet<String> = HashSet::new();
    for c in &after_commits {
        if is_fix_commit(&c.message) {
            for f in &c.changed_files {
                future_fixed.insert(f.path.to_string_lossy().into_owned());
            }
        }
    }
    assert_eq!(
        future_fixed.len(),
        3,
        "backtest fixture must produce exactly 3 held-out future-fixed files"
    );

    // ---- Phase 3: score both rankers by precision@N ----
    let n = future_fixed.len(); // N = number of true future-fixed files (3).
    let p_bare = precision_at_n(&ranked_bare, &future_fixed, n);
    let p_wilson = precision_at_n(&ranked_wilson, &future_fixed, n);

    // AC9: Wilson+decay MUST be at least as good as the bare-ratio baseline.
    assert!(
        p_wilson >= p_bare,
        "AC9 backtest: Wilson+decay precision@{n} ({p_wilson:.3}) must be >= \
         bare-ratio baseline ({p_bare:.3})"
    );
    // Strengthen falsifiability: the trap proves Wilson STRICTLY beats the bare
    // ratio here — the bare ranker tops out on saturated 1/1 traps (none future-
    // fixed) so its precision@N is 0, while Wilson surfaces the hot_core files.
    assert!(
        p_wilson > p_bare,
        "AC9 backtest: with saturated tiny-sample traps present, Wilson+decay \
         precision@{n} ({p_wilson:.3}) must STRICTLY exceed the bare-ratio \
         baseline ({p_bare:.3}) — a dropped-Wilson regression would break this"
    );
}
