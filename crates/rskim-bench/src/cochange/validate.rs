//! Co-change validation pipeline.
//!
//! # Pipeline
//!
//! For each repository:
//!
//! 1. Clone full history via [`clone_with_history`].
//! 2. Parse history via [`GixSource::parse_history`].
//! 3. Apply deny-list filter to each commit's changed files.
//! 4. Check quality gates (≥50 multi-file commits, ≥6-month span).
//! 5. Temporal split (80/20 by default).
//! 6. Build co-change matrix from training commits.
//! 7. Evaluate at all requested Jaccard thresholds.
//! 8. Return [`RepoCochangeResult`].
//!
//! # Performance
//!
//! The evaluator calls [`CochangeMatrixReader::jaccard`] (O(log n)) for every
//! (query, candidate) pair.  It does **not** call `pairs_for_file` (which is
//! O(n) for the `file_b` prefix scan) to keep evaluation O(F² log P) where F
//! is the number of mapped files and P is the pair count.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rskim_search::cochange::{CochangeMatrixBuilder, CochangeMatrixReader};
use rskim_search::temporal::GixSource;
use rskim_search::{CommitInfo, FileId, HistoryResult, SearchError, TemporalSource};

use super::deny_list::filter_denied;
use super::temporal_split::temporal_split;
use super::types::{RepoCochangeResult, ThresholdMetrics};
use rskim_research::config::RepoEntry;

/// Minimum number of multi-file commits required to pass the quality gate.
const MIN_MULTI_FILE_COMMITS: usize = 50;

/// Minimum repository age in seconds to pass the quality gate.
const MIN_HISTORY_SECONDS: i64 = 6 * 30 * 24 * 60 * 60; // approximately 6 months

// ============================================================================
// Path map construction
// ============================================================================

/// Build a mapping from repo-relative file path to a sequential [`FileId`].
///
/// Paths are collected from all changed files across every commit, sorted
/// alphabetically, and assigned IDs `0, 1, 2, …`.  Sorting ensures the
/// mapping is deterministic regardless of commit traversal order.
#[must_use]
pub fn build_path_map(commits: &[CommitInfo]) -> HashMap<PathBuf, FileId> {
    let mut paths: Vec<PathBuf> = commits
        .iter()
        .flat_map(|c| c.changed_files.iter().map(|f| f.path.clone()))
        .collect();
    paths.sort_unstable();
    paths.dedup();
    paths
        .into_iter()
        .enumerate()
        .map(|(i, p)| (p, FileId(i as u32)))
        .collect()
}

// ============================================================================
// Metric helpers (pure, no I/O)
// ============================================================================

/// Compute precision: |predicted ∩ actual| / |predicted|.
///
/// Returns `0.0` when the predicted set is empty.
#[must_use]
pub fn compute_precision(predicted: &HashSet<FileId>, actual: &HashSet<FileId>) -> f64 {
    if predicted.is_empty() {
        return 0.0;
    }
    let intersection = predicted.intersection(actual).count();
    intersection as f64 / predicted.len() as f64
}

/// Compute recall: |predicted ∩ actual| / |actual|.
///
/// Returns `0.0` when the actual set is empty.
#[must_use]
pub fn compute_recall(predicted: &HashSet<FileId>, actual: &HashSet<FileId>) -> f64 {
    if actual.is_empty() {
        return 0.0;
    }
    let intersection = predicted.intersection(actual).count();
    intersection as f64 / actual.len() as f64
}

/// Compute F1 score: 2 * precision * recall / (precision + recall).
///
/// Returns `0.0` when both precision and recall are zero.
#[must_use]
pub fn compute_f1(precision: f64, recall: f64) -> f64 {
    let denom = precision + recall;
    if denom == 0.0 {
        return 0.0;
    }
    2.0 * precision * recall / denom
}

// ============================================================================
// Quality gate
// ============================================================================

/// Check that a commit slice meets quality requirements.
///
/// # Errors
///
/// Returns a human-readable error string (not an `anyhow::Error`) when the
/// quality gate fails so it can be stored directly in
/// [`RepoCochangeResult::quality_gate_reason`].
pub fn check_quality_gates(commits: &[CommitInfo]) -> Result<(), String> {
    // Count multi-file commits (≥2 files after deny-list filtering).
    let multi_file_count = commits
        .iter()
        .filter(|c| c.changed_files.len() >= 2)
        .count();

    if multi_file_count < MIN_MULTI_FILE_COMMITS {
        return Err(format!(
            "only {multi_file_count} multi-file commits (need ≥{MIN_MULTI_FILE_COMMITS})"
        ));
    }

    // Check history span.
    if commits.len() >= 2 {
        let (min_ts, max_ts) = commits.iter().fold((i64::MAX, i64::MIN), |(lo, hi), c| {
            (lo.min(c.timestamp), hi.max(c.timestamp))
        });
        let span = max_ts - min_ts;
        if span < MIN_HISTORY_SECONDS {
            return Err(format!(
                "history span {span}s is less than required {MIN_HISTORY_SECONDS}s (≈6 months)"
            ));
        }
    }

    Ok(())
}

// ============================================================================
// Threshold evaluation
// ============================================================================

/// Evaluate blast-radius predictions at multiple Jaccard thresholds.
///
/// For each multi-file test commit:
///   For each file in the commit that maps to a training [`FileId`] (query):
///     For every other known file (candidate):
///       Compute `jaccard(query, candidate)`.
///       If `jaccard >= threshold` AND `jaccard > 0`: include in predicted set.
///   Compute per-commit (macro) precision/recall.
/// Also accumulate micro-level (per-query) precision/recall.
///
/// # Returns
///
/// `(metrics_by_threshold, unmapped_files_count)`
///
/// `unmapped_files_count` is the number of file references in test commits
/// that had no mapping in `path_map` (and were therefore excluded from recall).
pub fn evaluate_at_thresholds(
    reader: &CochangeMatrixReader,
    test_commits: &[CommitInfo],
    path_map: &HashMap<PathBuf, FileId>,
    thresholds: &[f64],
) -> anyhow::Result<(Vec<ThresholdMetrics>, usize)> {
    // Collect all FileIds known to the matrix (from path_map).
    let all_file_ids: Vec<FileId> = {
        let mut ids: Vec<FileId> = path_map.values().copied().collect();
        ids.sort_unstable();
        ids
    };

    let mut unmapped_files_total = 0usize;

    // Per-threshold accumulators for macro (commit-level) averages.
    // Index aligns with `thresholds`.
    let n_thresholds = thresholds.len();
    let mut macro_precision_sum = vec![0.0f64; n_thresholds];
    let mut macro_recall_sum = vec![0.0f64; n_thresholds];
    let mut macro_commit_count = vec![0usize; n_thresholds];

    // Per-threshold accumulators for micro (query-level) averages.
    let mut micro_tp = vec![0usize; n_thresholds]; // true positives
    let mut micro_predicted = vec![0usize; n_thresholds]; // predicted set sizes
    let mut micro_actual = vec![0usize; n_thresholds]; // actual set sizes
    let mut micro_query_count = vec![0usize; n_thresholds];

    for commit in test_commits {
        // Resolve all file IDs for this commit. Track unmapped files.
        let mut known_ids: Vec<FileId> = Vec::new();
        let mut unmapped = 0usize;
        for fc in &commit.changed_files {
            match path_map.get(&fc.path) {
                Some(&fid) => known_ids.push(fid),
                None => unmapped += 1,
            }
        }
        unmapped_files_total += unmapped;

        // Multi-file test commits (with ≥2 mapped files) drive macro averaging.
        // Single-file commits are skipped for macro averaging but still
        // contribute to the unmapped file count.
        if known_ids.len() < 2 {
            continue;
        }

        // The "actual" co-change set for this commit is all *other* known files
        // (everything that changed together with the query file in this commit).
        // We build it for each query as the commit set minus the query itself.
        for ti in 0..n_thresholds {
            let threshold = thresholds[ti];

            let mut commit_precision_sum = 0.0f64;
            let mut commit_recall_sum = 0.0f64;
            let query_count = known_ids.len();

            for &query_id in &known_ids {
                // Build predicted set: files with jaccard >= threshold.
                let mut predicted: HashSet<FileId> = HashSet::new();
                for &candidate_id in &all_file_ids {
                    if candidate_id == query_id {
                        continue; // skip self-pair
                    }
                    // jaccard(a, a) == 0.0 by design; threshold is applied here.
                    match reader.jaccard(query_id, candidate_id) {
                        Ok(j) if j > 0.0 && j >= threshold => {
                            predicted.insert(candidate_id);
                        }
                        Ok(_) => {}
                        Err(SearchError::IndexCorrupted(msg)) => {
                            return Err(anyhow::anyhow!("matrix corrupted: {msg}"));
                        }
                        Err(e) => return Err(anyhow::anyhow!("jaccard error: {e}")),
                    }
                }

                // Build actual set: all other known IDs in this commit.
                let actual: HashSet<FileId> =
                    known_ids.iter().copied().filter(|&id| id != query_id).collect();

                let p = compute_precision(&predicted, &actual);
                let r = compute_recall(&predicted, &actual);
                commit_precision_sum += p;
                commit_recall_sum += r;

                // Micro accumulation.
                let tp = predicted.intersection(&actual).count();
                micro_tp[ti] += tp;
                micro_predicted[ti] += predicted.len();
                micro_actual[ti] += actual.len();
                micro_query_count[ti] += 1;
            }

            // Macro: average over queries within this commit, then accumulate.
            let commit_avg_precision = commit_precision_sum / query_count as f64;
            let commit_avg_recall = commit_recall_sum / query_count as f64;
            macro_precision_sum[ti] += commit_avg_precision;
            macro_recall_sum[ti] += commit_avg_recall;
            macro_commit_count[ti] += 1;
        }
    }

    // Assemble ThresholdMetrics for each threshold.
    let metrics: Vec<ThresholdMetrics> = (0..n_thresholds)
        .map(|ti| {
            let commit_count = macro_commit_count[ti];
            let (macro_p, macro_r) = if commit_count > 0 {
                (
                    macro_precision_sum[ti] / commit_count as f64,
                    macro_recall_sum[ti] / commit_count as f64,
                )
            } else {
                (0.0, 0.0)
            };

            let micro_p = if micro_predicted[ti] > 0 {
                micro_tp[ti] as f64 / micro_predicted[ti] as f64
            } else {
                0.0
            };
            let micro_r = if micro_actual[ti] > 0 {
                micro_tp[ti] as f64 / micro_actual[ti] as f64
            } else {
                0.0
            };

            ThresholdMetrics {
                threshold: thresholds[ti],
                macro_precision: macro_p,
                macro_recall: macro_r,
                macro_f1: compute_f1(macro_p, macro_r),
                micro_precision: micro_p,
                micro_recall: micro_r,
                micro_f1: compute_f1(micro_p, micro_r),
                commit_count,
                query_count: micro_query_count[ti],
            }
        })
        .collect();

    Ok((metrics, unmapped_files_total))
}

// ============================================================================
// Per-repo orchestrator
// ============================================================================

/// Validate co-change predictions for a single repository.
///
/// This is the top-level orchestrator that ties together cloning, history
/// parsing, filtering, quality gating, splitting, matrix building, and
/// evaluation.
pub fn validate_repo(
    entry: &RepoEntry,
    corpus_dir: &Path,
    thresholds: &[f64],
    train_fraction: f64,
) -> anyhow::Result<RepoCochangeResult> {
    let repo_name = entry
        .url
        .rsplit('/')
        .next()
        .unwrap_or("unknown")
        .trim_end_matches(".git")
        .to_string();

    let dest = corpus_dir.join(&repo_name);

    // 1. Clone with full history (idempotent if already present).
    if let Err(e) = rskim_research::clone::clone_with_history(&entry.url, &dest) {
        return Ok(error_result(
            entry,
            &repo_name,
            format!("clone failed: {e:#}"),
        ));
    }

    // Capture HEAD SHA for the manifest.
    let head_sha = capture_head_sha(&dest).unwrap_or_else(|_| "unknown".to_string());

    // 2. Parse full history (lookback_days = 0 = all history).
    let history: HistoryResult = match GixSource.parse_history(&dest, 0) {
        Ok(h) => h,
        Err(e) => {
            return Ok(error_result(
                entry,
                &repo_name,
                format!("parse_history failed: {e:#}"),
            ));
        }
    };

    // 3. Apply deny-list filter in-place to every commit.
    let mut all_commits = history.commits;
    for commit in &mut all_commits {
        filter_denied(&mut commit.changed_files);
    }

    // 4. Quality gate on full commit list.
    if let Err(reason) = check_quality_gates(&all_commits) {
        return Ok(RepoCochangeResult {
            repo_url: entry.url.clone(),
            repo_name,
            head_sha,
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
            quality_gate_reason: Some(reason),
            error: None,
        });
    }

    // 5. Temporal split (input is newest-first from GixSource).
    let split = temporal_split(&all_commits, train_fraction);

    // 6. Build path_map from training commits.
    let path_map = build_path_map(&split.train);

    // 7. Build co-change matrix in a tempdir.
    let index_dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            return Ok(error_result(
                entry,
                &repo_name,
                format!("tempdir failed: {e:#}"),
            ));
        }
    };

    let builder = match CochangeMatrixBuilder::new(index_dir.path().to_path_buf()) {
        Ok(b) => b,
        Err(e) => {
            return Ok(error_result(
                entry,
                &repo_name,
                format!("builder creation failed: {e:#}"),
            ));
        }
    };

    let history_for_builder = rskim_search::HistoryResult {
        commits: split.train.clone(),
        metadata: rskim_search::TemporalMetadata {
            is_shallow: false,
            commit_count: split.train.len(),
        },
    };

    let stats = match builder.build(&history_for_builder, &path_map) {
        Ok(s) => s,
        Err(SearchError::CapacityExceeded(msg)) => {
            return Ok(error_result(
                entry,
                &repo_name,
                format!("capacity exceeded: {msg}"),
            ));
        }
        Err(e) => {
            return Ok(error_result(
                entry,
                &repo_name,
                format!("matrix build failed: {e:#}"),
            ));
        }
    };

    // 8. Open reader.
    let reader = match CochangeMatrixReader::open(index_dir.path()) {
        Ok(r) => r,
        Err(e) => {
            return Ok(error_result(
                entry,
                &repo_name,
                format!("reader open failed: {e:#}"),
            ));
        }
    };

    // 9. Evaluate at all thresholds.
    let (metrics, unmapped) = match evaluate_at_thresholds(&reader, &split.test, &path_map, thresholds) {
        Ok(r) => r,
        Err(e) => {
            return Ok(error_result(
                entry,
                &repo_name,
                format!("evaluation failed: {e:#}"),
            ));
        }
    };

    let multi_file_test = split.test.iter().filter(|c| c.changed_files.len() >= 2).count();
    let single_file_test = split.test.len() - multi_file_test;

    Ok(RepoCochangeResult {
        repo_url: entry.url.clone(),
        repo_name,
        head_sha,
        train_commits: split.train.len(),
        test_commits: split.test.len(),
        multi_file_test_commits: multi_file_test,
        single_file_test_commits: single_file_test,
        unmapped_files_in_test: unmapped,
        file_count: stats.file_count as usize,
        pair_count: stats.pair_count as usize,
        commits_skipped_too_large: stats.commits_skipped_too_large as usize,
        split_timestamp: split.split_timestamp,
        metrics_by_threshold: metrics,
        quality_gate_passed: true,
        quality_gate_reason: None,
        error: None,
    })
}

// ============================================================================
// Aggregate
// ============================================================================

/// Macro-average metrics across repos that passed quality gates.
///
/// Only repos with `quality_gate_passed == true` and `error == None` are
/// included in the aggregate.
#[must_use]
pub fn aggregate_metrics(
    repos: &[RepoCochangeResult],
    thresholds: &[f64],
) -> Vec<ThresholdMetrics> {
    let passing: Vec<&RepoCochangeResult> = repos
        .iter()
        .filter(|r| r.quality_gate_passed && r.error.is_none())
        .collect();

    if passing.is_empty() {
        return thresholds
            .iter()
            .map(|&t| ThresholdMetrics {
                threshold: t,
                macro_precision: 0.0,
                macro_recall: 0.0,
                macro_f1: 0.0,
                micro_precision: 0.0,
                micro_recall: 0.0,
                micro_f1: 0.0,
                commit_count: 0,
                query_count: 0,
            })
            .collect();
    }

    thresholds
        .iter()
        .enumerate()
        .map(|(ti, &threshold)| {
            let mut mp_sum = 0.0f64;
            let mut mr_sum = 0.0f64;
            let mut mip_sum = 0.0f64;
            let mut mir_sum = 0.0f64;
            let mut count = 0usize;
            let mut total_commits = 0usize;
            let mut total_queries = 0usize;

            for repo in &passing {
                if let Some(m) = repo.metrics_by_threshold.get(ti).filter(|m| (m.threshold - threshold).abs() < 1e-9) {
                    mp_sum += m.macro_precision;
                    mr_sum += m.macro_recall;
                    mip_sum += m.micro_precision;
                    mir_sum += m.micro_recall;
                    count += 1;
                    total_commits += m.commit_count;
                    total_queries += m.query_count;
                }
            }

            if count == 0 {
                return ThresholdMetrics {
                    threshold,
                    macro_precision: 0.0,
                    macro_recall: 0.0,
                    macro_f1: 0.0,
                    micro_precision: 0.0,
                    micro_recall: 0.0,
                    micro_f1: 0.0,
                    commit_count: 0,
                    query_count: 0,
                };
            }

            let macro_p = mp_sum / count as f64;
            let macro_r = mr_sum / count as f64;
            let micro_p = mip_sum / count as f64;
            let micro_r = mir_sum / count as f64;

            ThresholdMetrics {
                threshold,
                macro_precision: macro_p,
                macro_recall: macro_r,
                macro_f1: compute_f1(macro_p, macro_r),
                micro_precision: micro_p,
                micro_recall: micro_r,
                micro_f1: compute_f1(micro_p, micro_r),
                commit_count: total_commits,
                query_count: total_queries,
            }
        })
        .collect()
}

// ============================================================================
// Private helpers
// ============================================================================

fn error_result(entry: &RepoEntry, repo_name: &str, error: String) -> RepoCochangeResult {
    RepoCochangeResult {
        repo_url: entry.url.clone(),
        repo_name: repo_name.to_string(),
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
        quality_gate_reason: None,
        error: Some(error),
    }
}

fn capture_head_sha(repo_path: &Path) -> anyhow::Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| anyhow::anyhow!("git rev-parse: {e}"))?;
    if !output.status.success() {
        anyhow::bail!("git rev-parse HEAD failed");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;

    use rskim_search::{CommitInfo, FileChangeInfo, FileId};

    use super::*;

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

    // --- Precision / Recall / F1 ---

    #[test]
    fn precision_perfect() {
        let predicted: HashSet<FileId> = [FileId(0), FileId(1)].into_iter().collect();
        let actual: HashSet<FileId> = [FileId(0), FileId(1), FileId(2)].into_iter().collect();
        // 2 / 2 = 1.0
        assert!((compute_precision(&predicted, &actual) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn precision_zero_predicted() {
        let predicted: HashSet<FileId> = HashSet::new();
        let actual: HashSet<FileId> = [FileId(0)].into_iter().collect();
        assert_eq!(compute_precision(&predicted, &actual), 0.0);
    }

    #[test]
    fn recall_perfect() {
        let predicted: HashSet<FileId> = [FileId(0), FileId(1), FileId(2)].into_iter().collect();
        let actual: HashSet<FileId> = [FileId(0), FileId(1)].into_iter().collect();
        // |{0,1} ∩ {0,1,2}| / |{0,1}| = 2/2 = 1.0
        assert!((compute_recall(&predicted, &actual) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn recall_zero_actual() {
        let predicted: HashSet<FileId> = [FileId(0)].into_iter().collect();
        let actual: HashSet<FileId> = HashSet::new();
        assert_eq!(compute_recall(&predicted, &actual), 0.0);
    }

    #[test]
    fn f1_both_zero() {
        assert_eq!(compute_f1(0.0, 0.0), 0.0);
    }

    #[test]
    fn f1_perfect() {
        assert!((compute_f1(1.0, 1.0) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn f1_harmonic_mean() {
        // p=0.5, r=1.0 → F1 = 2*0.5*1.0/1.5 = 0.6667
        let f1 = compute_f1(0.5, 1.0);
        assert!((f1 - 2.0 / 3.0).abs() < 1e-9);
    }

    // --- build_path_map determinism ---

    #[test]
    fn path_map_deterministic() {
        let commits = vec![
            make_commit(0, 100, &["z.rs", "a.rs"]),
            make_commit(1, 200, &["m.rs", "a.rs"]),
        ];
        let map1 = build_path_map(&commits);
        let map2 = build_path_map(&commits);
        assert_eq!(map1, map2, "path_map must be deterministic");
    }

    #[test]
    fn path_map_sorted_alphabetically() {
        let commits = vec![make_commit(0, 100, &["z.rs", "a.rs", "m.rs"])];
        let map = build_path_map(&commits);
        // a.rs → 0, m.rs → 1, z.rs → 2
        assert_eq!(*map.get(&PathBuf::from("a.rs")).unwrap(), FileId(0));
        assert_eq!(*map.get(&PathBuf::from("m.rs")).unwrap(), FileId(1));
        assert_eq!(*map.get(&PathBuf::from("z.rs")).unwrap(), FileId(2));
    }

    // --- Quality gates ---

    #[test]
    fn quality_gate_passes_with_enough_commits() {
        // Build 60 multi-file commits spanning >6 months.
        let commits: Vec<CommitInfo> = (0..60)
            .map(|i| {
                // Spread over 7 months (roughly 210 days * 86400 s/day).
                make_commit(i, i as i64 * (210 * 86400 / 60), &["a.rs", "b.rs"])
            })
            .collect();
        assert!(check_quality_gates(&commits).is_ok());
    }

    #[test]
    fn quality_gate_fails_too_few_commits() {
        let commits: Vec<CommitInfo> = (0..5)
            .map(|i| make_commit(i, i as i64 * 86400 * 60, &["a.rs", "b.rs"]))
            .collect();
        let result = check_quality_gates(&commits);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("multi-file commits"));
    }

    #[test]
    fn quality_gate_fails_short_history() {
        // 60 multi-file commits but only 1 day span.
        let commits: Vec<CommitInfo> = (0..60)
            .map(|i| make_commit(i, i as i64 * 100, &["a.rs", "b.rs"])) // seconds apart
            .collect();
        let result = check_quality_gates(&commits);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("span"));
    }
}
