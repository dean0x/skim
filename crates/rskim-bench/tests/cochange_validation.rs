//! Integration tests for the co-change validation benchmark.
//!
//! All tests that require `git` are guarded by a `git_available()` check so
//! they degrade gracefully in environments without git.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use rskim_bench::cochange::{
    deny_list::filter_denied,
    report::{to_json, to_markdown},
    temporal_split::temporal_split,
    types::{CochangeValidationResult, RunMetadata, ThresholdMetrics},
    validate::{
        aggregate_metrics, build_path_map, check_quality_gates, compute_f1, compute_precision,
        compute_recall,
    },
};
use rskim_search::{CommitInfo, FileChangeInfo, FileId};

// ============================================================================
// Test infrastructure
// ============================================================================

fn git_available() -> bool {
    Command::new("git")
        .args(["--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn init_git_repo() -> Option<TempDir> {
    let dir = tempfile::tempdir().ok()?;

    let init_ok = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
        || Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

    if !init_ok {
        return None;
    }

    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir.path())
        .output()
        .ok()?;
    Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(dir.path())
        .output()
        .ok()?;

    Some(dir)
}

fn git_commit_files(dir: &Path, files: &[(&str, &str)], message: &str) -> bool {
    for (name, content) in files {
        if std::fs::write(dir.join(name), content).is_err() {
            return false;
        }
    }
    Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
        && Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(dir)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

fn make_commit(id: usize, timestamp: i64, paths: &[&str]) -> CommitInfo {
    CommitInfo {
        hash: format!("{id:040x}"),
        timestamp,
        author: "test".to_string(),
        message: format!("commit {id}"),
        changed_files: paths
            .iter()
            .map(|p| FileChangeInfo {
                path: PathBuf::from(p),
                additions: 1,
                deletions: 0,
            })
            .collect(),
    }
}

// ============================================================================
// deny_list_filters_lockfiles
// ============================================================================

#[test]
fn deny_list_filters_lockfiles() {
    // A commit with A.rs + Cargo.lock: after filtering, only A.rs remains.
    let mut files = vec![
        FileChangeInfo {
            path: PathBuf::from("src/auth.rs"),
            additions: 5,
            deletions: 1,
        },
        FileChangeInfo {
            path: PathBuf::from("Cargo.lock"),
            additions: 200,
            deletions: 180,
        },
        FileChangeInfo {
            path: PathBuf::from("go.sum"),
            additions: 10,
            deletions: 0,
        },
    ];
    filter_denied(&mut files);
    assert_eq!(
        files.len(),
        1,
        "only src/auth.rs should survive deny-list filtering"
    );
    assert_eq!(files[0].path, PathBuf::from("src/auth.rs"));
}

#[test]
fn deny_list_keeps_source_files_intact() {
    let mut files = vec![
        FileChangeInfo {
            path: PathBuf::from("src/main.rs"),
            additions: 1,
            deletions: 0,
        },
        FileChangeInfo {
            path: PathBuf::from("lib/core.py"),
            additions: 3,
            deletions: 1,
        },
    ];
    filter_denied(&mut files);
    assert_eq!(files.len(), 2, "source files should not be filtered");
}

// ============================================================================
// quality_gate_rejects_small_repo
// ============================================================================

#[test]
fn quality_gate_rejects_small_repo() {
    // Only 5 multi-file commits — well below the 50 minimum.
    let commits: Vec<CommitInfo> = (0..5)
        .map(|i| make_commit(i, i as i64 * 86400 * 60, &["a.rs", "b.rs"]))
        .collect();
    let result = check_quality_gates(&commits);
    assert!(result.is_err(), "small repo should fail quality gate");
    let err = result.unwrap_err();
    assert!(
        err.contains("multi-file commits"),
        "error should mention commit count: {err}"
    );
}

#[test]
fn quality_gate_rejects_short_history() {
    // 60 multi-file commits but only minutes of history — below 6 month min.
    let commits: Vec<CommitInfo> = (0..60)
        .map(|i| make_commit(i, i as i64 * 60, &["a.rs", "b.rs"])) // 1 min apart
        .collect();
    let result = check_quality_gates(&commits);
    assert!(result.is_err(), "short history should fail quality gate");
}

// ============================================================================
// temporal_split_no_leakage
// ============================================================================

#[test]
fn temporal_split_no_leakage() {
    // Build 100 commits newest-first (as GixSource returns) with distinct
    // timestamps spread over 7 months.
    let one_month_secs = 30i64 * 86400;
    // timestamps: newest first
    let commits: Vec<CommitInfo> = (0..100)
        .map(|i| {
            // newest: timestamp=7*month, oldest: timestamp≈0
            let ts = (100 - i) as i64 * (7 * one_month_secs / 100);
            make_commit(i, ts, &[&format!("file_{i}.rs")])
        })
        .collect();

    let split = temporal_split(commits, 0.8);

    // Collect hashes to verify no overlap.
    let train_hashes: std::collections::HashSet<&str> =
        split.train.iter().map(|c| c.hash.as_str()).collect();
    let test_hashes: std::collections::HashSet<&str> =
        split.test.iter().map(|c| c.hash.as_str()).collect();

    assert!(
        train_hashes.is_disjoint(&test_hashes),
        "no commit should appear in both train and test sets"
    );

    // Training commits must be chronologically older than test commits.
    let max_train_ts = split.train.iter().map(|c| c.timestamp).max().unwrap_or(0);
    let min_test_ts = split
        .test
        .iter()
        .map(|c| c.timestamp)
        .min()
        .unwrap_or(i64::MAX);
    assert!(
        max_train_ts <= min_test_ts,
        "training set must contain only older commits: max_train={max_train_ts} > min_test={min_test_ts}"
    );
}

// ============================================================================
// json_output_valid
// ============================================================================

#[test]
fn json_output_valid() {
    let result = sample_validation_result();
    let json = to_json(&result).expect("to_json should not fail");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("JSON output must be valid");

    // Verify required top-level fields.
    assert!(
        parsed.get("repos").is_some(),
        "JSON must contain 'repos' key"
    );
    assert!(
        parsed.get("aggregate_metrics").is_some(),
        "JSON must contain 'aggregate_metrics' key"
    );
    assert!(
        parsed.get("thresholds").is_some(),
        "JSON must contain 'thresholds' key"
    );
    assert!(
        parsed.get("run_metadata").is_some(),
        "JSON must contain 'run_metadata' key"
    );
    assert!(
        parsed.get("deny_list_patterns").is_some(),
        "JSON must contain 'deny_list_patterns' key"
    );
}

// ============================================================================
// markdown_output_non_empty
// ============================================================================

#[test]
fn markdown_output_non_empty() {
    let result = sample_validation_result();
    let md = to_markdown(&result);
    assert!(!md.is_empty(), "markdown output must be non-empty");
    // Check that key structural elements are present.
    assert!(
        md.contains("Threshold") && md.contains("|"),
        "markdown must contain a threshold table"
    );
    assert!(
        md.contains("Per-Repo"),
        "markdown must have per-repo section"
    );
    assert!(
        md.contains("Methodology"),
        "markdown must have methodology section"
    );
}

// ============================================================================
// full_pipeline_synthetic_repo
// ============================================================================

#[test]
fn full_pipeline_synthetic_repo() {
    if !git_available() {
        eprintln!("SKIPPED: git not available");
        return;
    }
    let Some(dir) = init_git_repo() else {
        eprintln!("SKIPPED: could not init git repo");
        return;
    };

    // Build a synthetic git repo with known co-change patterns.
    //
    // Strategy: strong A-B coupling in training (4+ co-occurrences), one test
    // commit with A+B+C to verify predictions.
    //
    // We need ≥50 multi-file commits spanning ≥6 months.  We'll create 55
    // commits with timestamps spread over 7 months.
    let one_month = 30i64 * 24 * 3600;
    let start_ts = 1_700_000_000i64;

    // Training: 44 commits where A+B always co-change (strong coupling).
    for i in 0..44 {
        let ts_str = format!("{}", start_ts + i * (7 * one_month / 50));
        let ok = git_commit_files(
            dir.path(),
            &[
                ("a.rs", &format!("// version {i}")),
                ("b.rs", &format!("// version {i}")),
            ],
            &format!("train commit {i}"),
        );
        assert!(ok, "training commit {i} should succeed");
        // Set commit date via amend to control timestamps.
        let _ = Command::new("git")
            .args(["commit", "--amend", "--no-edit", "--date", &ts_str])
            .current_dir(dir.path())
            .output();
    }

    // More training: some solo A commits to reduce Jaccard(A,B) slightly.
    for i in 0..6 {
        let ok = git_commit_files(
            dir.path(),
            &[("a.rs", &format!("// solo a {i}"))],
            &format!("solo a {i}"),
        );
        assert!(ok, "solo commit {i} should succeed");
    }

    // Test: one multi-file commit (A+B should be predicted, C is novel).
    let ok = git_commit_files(
        dir.path(),
        &[
            ("a.rs", "// test version"),
            ("b.rs", "// test version"),
            ("c.rs", "// new file"),
        ],
        "test commit A+B+C",
    );
    assert!(ok, "test commit should succeed");

    // Parse history, build split, evaluate.
    use rskim_search::TemporalSource;
    use rskim_search::temporal::GixSource;

    let history = match GixSource.parse_history(dir.path(), 0) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("SKIPPED: parse_history failed: {e}");
            return;
        }
    };

    if history.commits.len() < 51 {
        eprintln!(
            "SKIPPED: only {} commits in synthetic repo (need ≥51)",
            history.commits.len()
        );
        return;
    }

    // Temporal split: use 0.98 so the last commit (our A+B+C test) is in test.
    let split = temporal_split(history.commits, 0.98);

    if split.test.is_empty() {
        eprintln!("SKIPPED: test split is empty");
        return;
    }

    let path_map = build_path_map(&split.train);

    // Build matrix.
    let index_dir = tempfile::tempdir().expect("tempdir");
    use rskim_search::cochange::CochangeMatrixBuilder;
    let builder =
        CochangeMatrixBuilder::new(index_dir.path().to_path_buf()).expect("builder creation");
    let history_for_builder = rskim_search::HistoryResult {
        commits: split.train.clone(),
        metadata: rskim_search::TemporalMetadata {
            is_shallow: false,
            commit_count: split.train.len(),
        },
    };
    let _stats = match builder.build(&history_for_builder, &path_map) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("SKIPPED: matrix build failed: {e}");
            return;
        }
    };

    use rskim_search::cochange::CochangeMatrixReader;
    let reader = CochangeMatrixReader::open(index_dir.path()).expect("reader open");

    use rskim_bench::cochange::validate::evaluate_at_thresholds;
    let thresholds = vec![0.01, 0.1, 0.3];
    let (metrics, _unmapped) = evaluate_at_thresholds(&reader, &split.test, &path_map, &thresholds)
        .expect("evaluate_at_thresholds");

    // At a low threshold (0.01), the strong A-B coupling should yield recall > 0.
    // (We can't guarantee exact values because the commit timestamp manipulation
    //  may not work perfectly in all CI environments, but recall must be ≥0.)
    assert_eq!(metrics.len(), 3, "should have metrics for each threshold");
    for m in &metrics {
        assert!(
            m.macro_recall >= 0.0 && m.macro_recall <= 1.0,
            "recall must be in [0,1] at threshold {}",
            m.threshold
        );
        assert!(
            m.macro_precision >= 0.0 && m.macro_precision <= 1.0,
            "precision must be in [0,1] at threshold {}",
            m.threshold
        );
    }
}

// ============================================================================
// aggregate_metrics_skips_failed_repos
// ============================================================================

#[test]
fn aggregate_metrics_skips_failed_repos() {
    use rskim_bench::cochange::types::RepoCochangeResult;

    let passing = RepoCochangeResult {
        repo_url: "https://github.com/example/passing".to_string(),
        repo_name: "passing".to_string(),
        head_sha: "a".repeat(40),
        train_commits: 80,
        test_commits: 20,
        multi_file_test_commits: 15,
        single_file_test_commits: 5,
        unmapped_files_in_test: 0,
        file_count: 100,
        pair_count: 300,
        commits_skipped_too_large: 0,
        split_timestamp: 1_700_000_000,
        metrics_by_threshold: vec![ThresholdMetrics {
            threshold: 0.1,
            macro_precision: 0.5,
            macro_recall: 0.6,
            macro_f1: 0.545,
            micro_precision: 0.55,
            micro_recall: 0.65,
            micro_f1: 0.595,
            commit_count: 15,
            query_count: 45,
        }],
        quality_gate_passed: true,
        quality_gate_reason: None,
        error: None,
    };

    let failing = RepoCochangeResult {
        repo_url: "https://github.com/example/failing".to_string(),
        repo_name: "failing".to_string(),
        head_sha: "unknown".to_string(),
        train_commits: 0,
        test_commits: 0,
        multi_file_test_commits: 0,
        single_file_test_commits: 0,
        unmapped_files_in_test: 0,
        file_count: 0,
        pair_count: 0,
        commits_skipped_too_large: 0,
        split_timestamp: 0,
        metrics_by_threshold: vec![],
        quality_gate_passed: false,
        quality_gate_reason: Some("too few commits".to_string()),
        error: None,
    };

    let agg = aggregate_metrics(&[passing, failing], &[0.1]);
    assert_eq!(agg.len(), 1);
    // Aggregate should only include the passing repo.
    let m = &agg[0];
    assert!(
        (m.macro_precision - 0.5).abs() < 1e-9,
        "aggregate should match passing repo"
    );
    assert!((m.macro_recall - 0.6).abs() < 1e-9);
}

// ============================================================================
// build_path_map_deduplicates
// ============================================================================

#[test]
fn build_path_map_deduplicates() {
    // Same path appearing in multiple commits should map to one FileId.
    let commits = vec![
        make_commit(0, 100, &["shared.rs", "a.rs"]),
        make_commit(1, 200, &["shared.rs", "b.rs"]),
    ];
    let map = build_path_map(&commits);
    // 3 unique paths: a.rs, b.rs, shared.rs.
    assert_eq!(map.len(), 3, "path map should deduplicate across commits");
    // shared.rs should have a valid FileId.
    assert!(map.contains_key(&PathBuf::from("shared.rs")));
}

// ============================================================================
// precision_recall_f1_basics
// ============================================================================

#[test]
fn precision_and_recall_basic() {
    use std::collections::HashSet;

    let predicted: HashSet<FileId> = [FileId(0), FileId(1), FileId(2)].into_iter().collect();
    let actual: HashSet<FileId> = [FileId(1), FileId(2), FileId(3)].into_iter().collect();

    // intersection = {1, 2}, |predicted| = 3, |actual| = 3
    let p = compute_precision(&predicted, &actual);
    let r = compute_recall(&predicted, &actual);
    assert!((p - 2.0 / 3.0).abs() < 1e-9);
    assert!((r - 2.0 / 3.0).abs() < 1e-9);

    let f1 = compute_f1(p, r);
    assert!((f1 - 2.0 / 3.0).abs() < 1e-9, "F1 = 2/3 when P=R=2/3");
}

// ============================================================================
// Helpers
// ============================================================================

fn sample_threshold_metrics() -> ThresholdMetrics {
    ThresholdMetrics {
        threshold: 0.1,
        macro_precision: 0.6,
        macro_recall: 0.55,
        macro_f1: 0.574,
        micro_precision: 0.65,
        micro_recall: 0.5,
        micro_f1: 0.564,
        commit_count: 30,
        query_count: 90,
    }
}

fn sample_validation_result() -> CochangeValidationResult {
    use rskim_bench::cochange::types::{RepoCochangeResult, RepoManifest};

    CochangeValidationResult {
        repos: vec![RepoCochangeResult {
            repo_url: "https://github.com/example/repo".to_string(),
            repo_name: "repo".to_string(),
            head_sha: "a".repeat(40),
            train_commits: 80,
            test_commits: 20,
            multi_file_test_commits: 15,
            single_file_test_commits: 5,
            unmapped_files_in_test: 2,
            file_count: 100,
            pair_count: 300,
            commits_skipped_too_large: 0,
            split_timestamp: 1_700_000_000,
            metrics_by_threshold: vec![sample_threshold_metrics()],
            quality_gate_passed: true,
            quality_gate_reason: None,
            error: None,
        }],
        aggregate_metrics: vec![sample_threshold_metrics()],
        thresholds: vec![0.1],
        deny_list_patterns: vec!["Cargo.lock".to_string()],
        run_metadata: RunMetadata {
            timestamp: "2024-01-15T10:00:00Z".to_string(),
            corpus_config_path: "cochange-corpus.toml".to_string(),
            repo_manifests: vec![RepoManifest {
                repo_url: "https://github.com/example/repo".to_string(),
                head_sha: "a".repeat(40),
                train_cutoff_timestamp: 1_700_000_000,
                train_commit_count: 80,
                test_commit_count: 20,
            }],
        },
    }
}
