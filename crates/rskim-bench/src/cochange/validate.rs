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

use std::collections::{BTreeSet, HashMap, HashSet};
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

/// Maximum number of distinct files evaluated per threshold sweep.
///
/// Each query-candidate pair requires one `jaccard` call, so evaluation cost
/// is O(F² × T) where F is the file count and T is the threshold count.  A
/// 20 000-file repo with 10 thresholds requires ~4 billion jaccard calls,
/// which is prohibitively slow.  Repos exceeding this limit are rejected
/// before evaluation begins.
const MAX_FILES_FOR_EVALUATION: usize = 20_000;

// ============================================================================
// Path map construction
// ============================================================================

/// Build a mapping from repo-relative file path to a sequential [`FileId`].
///
/// Paths are collected from all changed files across every commit, sorted
/// alphabetically, and assigned IDs `0, 1, 2, …`.  Sorting ensures the
/// mapping is deterministic regardless of commit traversal order.
///
/// Uses a [`BTreeSet`] to deduplicate while maintaining sort order, avoiding
/// the clone-all-then-dedup pattern that discards most duplicates.
#[must_use]
pub fn build_path_map(commits: &[CommitInfo]) -> HashMap<PathBuf, FileId> {
    let unique_paths: BTreeSet<&PathBuf> = commits
        .iter()
        .flat_map(|c| c.changed_files.iter().map(|f| &f.path))
        .collect();
    assert!(
        unique_paths.len() <= u32::MAX as usize,
        "too many unique paths ({}) for FileId(u32)",
        unique_paths.len()
    );
    unique_paths
        .into_iter()
        .enumerate()
        .map(|(i, p)| (p.clone(), FileId(i as u32)))
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
/// Returns an [`anyhow::Error`] with a human-readable message when the quality
/// gate fails.  At the call site, convert to a `String` via `.to_string()` and
/// store in [`RepoCochangeResult::quality_gate_reason`].
pub fn check_quality_gates(commits: &[CommitInfo]) -> anyhow::Result<()> {
    // Count multi-file commits (≥2 files after deny-list filtering).
    let multi_file_count = commits
        .iter()
        .filter(|c| c.changed_files.len() >= 2)
        .count();

    if multi_file_count < MIN_MULTI_FILE_COMMITS {
        anyhow::bail!(
            "only {multi_file_count} multi-file commits (need ≥{MIN_MULTI_FILE_COMMITS})"
        );
    }

    // Check history span.
    if commits.len() >= 2 {
        let (min_ts, max_ts) = commits.iter().fold((i64::MAX, i64::MIN), |(lo, hi), c| {
            (lo.min(c.timestamp), hi.max(c.timestamp))
        });
        let span = max_ts - min_ts;
        if span < MIN_HISTORY_SECONDS {
            anyhow::bail!(
                "history span {span}s is less than required {MIN_HISTORY_SECONDS}s (≈6 months)"
            );
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

    // Guard against O(F²) explosion for very large repositories.
    if all_file_ids.len() > MAX_FILES_FOR_EVALUATION {
        anyhow::bail!(
            "file count {} exceeds evaluation limit {} — skip this repo or raise MAX_FILES_FOR_EVALUATION",
            all_file_ids.len(),
            MAX_FILES_FOR_EVALUATION
        );
    }

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

        // Pre-compute jaccard scores and actual sets once, then sweep thresholds.
        // Jaccard values are threshold-independent: computing them once and sweeping
        // thresholds reduces total work by a factor of T (number of thresholds).

        // jaccard_cache[q_idx] = Vec of (candidate_id, jaccard) pairs where j > 0.
        // Only positive values are retained so the threshold sweep is cheap.
        let mut jaccard_cache: Vec<Vec<(FileId, f64)>> = Vec::with_capacity(known_ids.len());
        for &query_id in &known_ids {
            let mut pairs: Vec<(FileId, f64)> = Vec::new();
            for &candidate_id in &all_file_ids {
                if candidate_id == query_id {
                    continue; // skip self-pair
                }
                match reader.jaccard(query_id, candidate_id) {
                    Ok(j) if j > 0.0 => {
                        pairs.push((candidate_id, j));
                    }
                    Ok(_) => {}
                    Err(SearchError::IndexCorrupted(msg)) => {
                        return Err(anyhow::anyhow!("matrix corrupted: {msg}"));
                    }
                    Err(e) => return Err(anyhow::anyhow!("jaccard error: {e}")),
                }
            }
            jaccard_cache.push(pairs);
        }

        // Pre-compute the "actual" co-change set for each query in this commit:
        // all other known file IDs changed in the same commit.
        let actual_sets: Vec<HashSet<FileId>> = known_ids
            .iter()
            .map(|&query_id| {
                known_ids
                    .iter()
                    .copied()
                    .filter(|&id| id != query_id)
                    .collect()
            })
            .collect();

        // Sweep thresholds over the cached jaccard values.
        let query_count = known_ids.len();
        for ti in 0..n_thresholds {
            let threshold = thresholds[ti];

            let mut commit_precision_sum = 0.0f64;
            let mut commit_recall_sum = 0.0f64;

            for (q_idx, _query_id) in known_ids.iter().enumerate() {
                // Apply threshold filter to cached jaccard values.
                let predicted: HashSet<FileId> = jaccard_cache[q_idx]
                    .iter()
                    .filter(|&&(_, j)| j >= threshold)
                    .map(|&(cid, _)| cid)
                    .collect();

                let actual = &actual_sets[q_idx];
                let p = compute_precision(&predicted, actual);
                let r = compute_recall(&predicted, actual);
                commit_precision_sum += p;
                commit_recall_sum += r;

                // Micro accumulation.
                let tp = predicted.intersection(actual).count();
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
/// This is the top-level orchestrator: it converts errors from the two
/// sub-phases (clone/parse and build/evaluate) into soft failure fields on
/// [`RepoCochangeResult`] so a single broken repo does not abort the whole run.
pub fn validate_repo(
    entry: &RepoEntry,
    corpus_dir: &Path,
    thresholds: &[f64],
    train_fraction: f64,
) -> anyhow::Result<RepoCochangeResult> {
    let repo_name = match rskim_research::clone::extract_repo_name(&entry.url) {
        Ok(name) => name,
        Err(e) => {
            return Ok(error_result(
                entry,
                "unknown",
                format!("invalid repo URL (path traversal guard): {e:#}"),
            ))
        }
    };

    let dest = corpus_dir.join(&repo_name);

    // Phase 1: clone, parse history, and apply deny-list filter.
    let (head_sha, all_commits) = match clone_and_parse(&entry.url, &dest) {
        Ok(r) => r,
        Err(e) => return Ok(error_result(entry, &repo_name, format!("{e:#}"))),
    };

    // 4. Quality gate on full commit list.
    if let Err(e) = check_quality_gates(&all_commits) {
        return Ok(RepoCochangeResult {
            repo_url: entry.url.clone(),
            repo_name,
            head_sha,
            quality_gate_reason: Some(e.to_string()),
            ..Default::default()
        });
    }

    // 5. Temporal split (input is newest-first from GixSource).
    // Pass ownership to avoid cloning the full commit list.
    let split = temporal_split(all_commits, train_fraction);

    // Phase 2: build co-change matrix and evaluate at all thresholds.
    let eval = match build_and_evaluate(&split.train, &split.test, thresholds) {
        Ok(r) => r,
        Err(e) => return Ok(error_result(entry, &repo_name, format!("{e:#}"))),
    };

    let multi_file_test = split
        .test
        .iter()
        .filter(|c| c.changed_files.len() >= 2)
        .count();
    let single_file_test = split.test.len() - multi_file_test;

    Ok(RepoCochangeResult {
        repo_url: entry.url.clone(),
        repo_name,
        head_sha,
        train_commits: split.train.len(),
        test_commits: split.test.len(),
        multi_file_test_commits: multi_file_test,
        single_file_test_commits: single_file_test,
        unmapped_files_in_test: eval.unmapped,
        file_count: eval.file_count,
        pair_count: eval.pair_count,
        commits_skipped_too_large: eval.commits_skipped_too_large,
        split_timestamp: split.split_timestamp,
        metrics_by_threshold: eval.metrics,
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
///
/// # Averaging semantics
///
/// The returned [`ThresholdMetrics`] fields have the following meanings at the
/// **aggregate** level:
///
/// - `macro_precision` / `macro_recall` — macro-average of per-repo macro
///   precision/recall.
/// - `micro_precision` / `micro_recall` — macro-average of per-repo micro
///   precision/recall.  Note: this is *not* a true cross-repo micro-average
///   (which would require summing raw TP/predicted/actual counts across repos).
///   The field names are inherited from [`ThresholdMetrics`] for schema
///   compatibility; at this aggregate level they represent the average of each
///   repo's true-micro value.
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
                if let Some(m) = repo
                    .metrics_by_threshold
                    .get(ti)
                    .filter(|m| (m.threshold - threshold).abs() < 1e-9)
                {
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

/// Return type for [`build_and_evaluate`].
struct EvalResult {
    metrics: Vec<ThresholdMetrics>,
    unmapped: usize,
    file_count: usize,
    pair_count: usize,
    commits_skipped_too_large: usize,
}

/// Phase 1: clone, parse full history, apply deny-list filter.
///
/// Returns `(head_sha, filtered_commits)` on success.
fn clone_and_parse(url: &str, dest: &Path) -> anyhow::Result<(String, Vec<CommitInfo>)> {
    // 1. Clone with full history (idempotent if already present).
    rskim_research::clone::clone_with_history(url, dest)
        .map_err(|e| anyhow::anyhow!("clone failed: {e:#}"))?;

    // Capture HEAD SHA for the manifest.
    let head_sha = capture_head_sha(dest).unwrap_or_else(|_| "unknown".to_string());

    // 2. Parse full history (lookback_days = 0 = all history).
    let history: HistoryResult = GixSource
        .parse_history(dest, 0)
        .map_err(|e| anyhow::anyhow!("parse_history failed: {e:#}"))?;

    // 3. Apply deny-list filter in-place to every commit.
    let mut all_commits = history.commits;
    for commit in &mut all_commits {
        filter_denied(&mut commit.changed_files);
    }

    Ok((head_sha, all_commits))
}

/// Phase 2: build co-change matrix and evaluate at all thresholds.
///
/// Accepts the train and test splits as slices so the caller retains ownership
/// for counting commits and computing statistics in the result.
fn build_and_evaluate(
    train: &[CommitInfo],
    test: &[CommitInfo],
    thresholds: &[f64],
) -> anyhow::Result<EvalResult> {
    // 6. Build path_map from training commits.
    let path_map = build_path_map(train);

    // 7. Build co-change matrix in a tempdir.
    let index_dir =
        tempfile::tempdir().map_err(|e| anyhow::anyhow!("tempdir failed: {e:#}"))?;

    let builder = CochangeMatrixBuilder::new(index_dir.path().to_path_buf())
        .map_err(|e| anyhow::anyhow!("builder creation failed: {e:#}"))?;

    let history_for_builder = rskim_search::HistoryResult {
        commits: train.to_vec(),
        metadata: rskim_search::TemporalMetadata {
            is_shallow: false,
            commit_count: train.len(),
        },
    };

    let stats = builder
        .build(&history_for_builder, &path_map)
        .map_err(|e| match e {
            SearchError::CapacityExceeded(msg) => anyhow::anyhow!("capacity exceeded: {msg}"),
            other => anyhow::anyhow!("matrix build failed: {other:#}"),
        })?;

    // 8. Open reader.
    let reader = CochangeMatrixReader::open(index_dir.path())
        .map_err(|e| anyhow::anyhow!("reader open failed: {e:#}"))?;

    // 9. Evaluate at all thresholds.
    let (metrics, unmapped) = evaluate_at_thresholds(&reader, test, &path_map, thresholds)
        .map_err(|e| anyhow::anyhow!("evaluation failed: {e:#}"))?;

    Ok(EvalResult {
        metrics,
        unmapped,
        file_count: stats.file_count as usize,
        pair_count: stats.pair_count as usize,
        commits_skipped_too_large: stats.commits_skipped_too_large as usize,
    })
}

fn error_result(entry: &RepoEntry, repo_name: &str, error: String) -> RepoCochangeResult {
    RepoCochangeResult {
        repo_url: entry.url.clone(),
        repo_name: repo_name.to_string(),
        head_sha: "unknown".to_string(),
        error: Some(error),
        ..Default::default()
    }
}

/// Timeout for `git rev-parse HEAD` (seconds).
const GIT_SHA_TIMEOUT_SECS: u64 = 30;

fn capture_head_sha(repo_path: &Path) -> anyhow::Result<String> {
    use std::sync::mpsc;
    use std::time::Duration;

    let child = std::process::Command::new("git")
        .arg("-c")
        .arg("credential.helper=")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "HEAD"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("git rev-parse spawn: {e}"))?;

    let child_id = child.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = child.wait_with_output();
        let _ = tx.send(result);
    });

    match rx.recv_timeout(Duration::from_secs(GIT_SHA_TIMEOUT_SECS)) {
        Ok(Ok(output)) => {
            if !output.status.success() {
                anyhow::bail!("git rev-parse HEAD failed");
            }
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        Ok(Err(e)) => Err(anyhow::anyhow!("git rev-parse wait error: {e}")),
        Err(_timeout) => {
            #[cfg(unix)]
            {
                // SAFETY: kill(2) is always safe to call with a valid pid.
                unsafe {
                    libc::kill(child_id as libc::pid_t, libc::SIGKILL);
                }
            }
            #[cfg(not(unix))]
            {
                let _ = std::process::Command::new("taskkill")
                    .args(["/F", "/PID", &child_id.to_string()])
                    .status();
            }
            anyhow::bail!("git rev-parse HEAD timed out after {GIT_SHA_TIMEOUT_SECS}s");
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;

    use rskim_search::FileId;

    use super::*;
    use crate::cochange::test_utils::make_commit;

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
        assert!(result.unwrap_err().to_string().contains("multi-file commits"));
    }

    #[test]
    fn quality_gate_fails_short_history() {
        // 60 multi-file commits but only 1 day span.
        let commits: Vec<CommitInfo> = (0..60)
            .map(|i| make_commit(i, i as i64 * 100, &["a.rs", "b.rs"])) // seconds apart
            .collect();
        let result = check_quality_gates(&commits);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("span"));
    }
}
