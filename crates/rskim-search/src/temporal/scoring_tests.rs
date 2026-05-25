//! Tests for [`decay_weight`] and [`compute_file_risk_scores`].
//!
//! Written test-first (RED phase) following the TDD cycle.
//! Groups match the plan: decay_weight unit tests, basic cases,
//! acceptance criteria, fix density specifics, edge cases, and determinism.

use std::path::PathBuf;

use crate::temporal::{compute_file_risk_scores, decay_weight, DEFAULT_HALF_LIFE_DAYS};
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
        timestamp: ts as i64,
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
        assert!(window[0] >= window[1], "{} should be >= {}", window[0], window[1]);
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
        assert!(w >= 0.0 && w <= 1.0, "out of range for ({elapsed}, {half_life}): {w}");
        assert!(w.is_finite(), "non-finite for ({elapsed}, {half_life})");
    }
}

/// `decay_weight` with zero half-life should panic in debug builds (debug_assert).
#[test]
#[cfg(debug_assertions)]
#[should_panic]
fn decay_zero_half_life_panics() {
    let _ = decay_weight(1.0, 0.0);
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
    assert!(approx_eq(s.fix_density, 0.0), "fix_density={}", s.fix_density);
}

/// Single fix commit at NOW for one file → hotspot=1.0, fix_density=1.0.
#[test]
fn single_fix_commit() {
    let commits = vec![make_commit(NOW, "fix: null dereference", &["a.rs"])];
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    let s = &scores["a.rs"];
    assert!(approx_eq(s.hotspot, 1.0), "hotspot={}", s.hotspot);
    assert!(approx_eq(s.fix_density, 1.0), "fix_density={}", s.fix_density);
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
    let max = scores.values().map(|s| s.hotspot).fold(f64::NEG_INFINITY, f64::max);
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
        assert!(approx_eq(s.fix_density, 0.0), "{path}: fix_density={}", s.fix_density);
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
        assert!(approx_eq(s.fix_density, 1.0), "{path}: fix_density={}", s.fix_density);
    }
}

/// All scores in [0.0, 1.0] for mixed commits and multiple files.
#[test]
fn scores_in_unit_range() {
    let files = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"];
    let messages = ["fix: bug", "feat: thing", "bug: crash", "add stuff", "revert bad"];
    let mut commits = Vec::new();
    for (i, (file, msg)) in files.iter().zip(messages.iter()).cycle().take(20).enumerate() {
        commits.push(make_commit(NOW - (i as u64) * DAY, msg, &[file]));
    }
    let scores = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    for (path, s) in &scores {
        assert!(s.hotspot >= 0.0 && s.hotspot <= 1.0, "{path}: hotspot={}", s.hotspot);
        assert!(s.fix_density >= 0.0 && s.fix_density <= 1.0, "{path}: fix_density={}", s.fix_density);
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
    assert!((density - 0.5).abs() < EPSILON, "expected ~0.5, got {density}");
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
    assert!(approx_eq(scores["a.rs"].fix_density, 1.0), "fix_density={}", scores["a.rs"].fix_density);
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
    assert!(s.hotspot >= 0.0 && s.hotspot <= 1.0, "hotspot={}", s.hotspot);
    assert!(s.fix_density >= 0.0 && s.fix_density <= 1.0, "fix_density={}", s.fix_density);
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
        assert!(s.fix_density.is_finite(), "{path}: fix_density is not finite");
        assert!(s.hotspot >= 0.0 && s.hotspot <= 1.0, "{path}: hotspot={}", s.hotspot);
        assert!(s.fix_density >= 0.0 && s.fix_density <= 1.0, "{path}: fix_density={}", s.fix_density);
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
    // Every entry has both scores — guaranteed by FileRiskScores struct.
    for (path, s) in &scores {
        let _hotspot: f64 = s.hotspot;
        let _fix_density: f64 = s.fix_density;
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
            let msg = if i % 3 == 0 { "fix: something" } else { "feat: thing" };
            make_commit(NOW - i * DAY, msg, &["a.rs", "b.rs"])
        })
        .collect();

    let first = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
    for _ in 0..49 {
        let result = compute_file_risk_scores(&commits, NOW, HALF_LIFE);
        for (path, s) in &result {
            let f = &first[path];
            assert!(approx_eq(s.hotspot, f.hotspot), "{path}: hotspot differs");
            assert!(approx_eq(s.fix_density, f.fix_density), "{path}: fix_density differs");
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
    assert!(ratio_short < ratio_long, "short={ratio_short}, long={ratio_long}");
}

/// DEFAULT_HALF_LIFE_DAYS constant is 30.0.
#[test]
fn default_half_life_is_30() {
    assert!((DEFAULT_HALF_LIFE_DAYS - 30.0).abs() < EPSILON);
}
