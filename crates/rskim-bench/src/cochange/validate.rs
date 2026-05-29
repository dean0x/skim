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

/// Maximum number of test commits processed by [`evaluate_at_thresholds`].
///
/// A repo with 100k test commits × 100 mapped files × 20k candidates would
/// require 200 billion jaccard calls.  Exceeding this limit aborts evaluation
/// with an error so the benchmark run does not stall indefinitely.
const MAX_TEST_COMMITS: usize = 50_000;

/// Maximum number of files in a single test commit included in evaluation.
///
/// A commit touching 1 000 mapped files would produce 1 000 × 20 000 = 20M
/// jaccard calls and roughly 320 MB of cached tuples for that commit alone.
/// Commits exceeding this limit are skipped silently; the commit still
/// contributes its unmapped-file count but is excluded from metric averaging.
const MAX_FILES_PER_COMMIT: usize = 500;

/// Maximum number of commits loaded by [`parse_history`] before evaluation.
///
/// `parse_history` with `lookback_days = 0` loads the entire repo history with
/// no upper bound, which can OOM on unexpectedly large repos.  If the parsed
/// commit count exceeds this limit, the repo is rejected before the build/
/// evaluate phase.
const MAX_COMMITS_FOR_PARSE: usize = 500_000;

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
///
/// # Errors
///
/// Returns an error if the number of unique paths exceeds [`u32::MAX`], which
/// would overflow the [`FileId`] counter.
pub fn build_path_map(commits: &[CommitInfo]) -> anyhow::Result<HashMap<PathBuf, FileId>> {
    let unique_paths: BTreeSet<&PathBuf> = commits
        .iter()
        .flat_map(|c| c.changed_files.iter().map(|f| &f.path))
        .collect();
    if unique_paths.len() > u32::MAX as usize {
        anyhow::bail!(
            "too many unique paths ({}) for FileId(u32)",
            unique_paths.len()
        );
    }
    Ok(unique_paths
        .into_iter()
        .enumerate()
        .map(|(i, p)| (p.clone(), FileId(i as u32)))
        .collect())
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

    // Guard against O(commits × F²) wall-time explosion.
    if test_commits.len() > MAX_TEST_COMMITS {
        anyhow::bail!(
            "test commit count {} exceeds limit {} — raise MAX_TEST_COMMITS or reduce test set",
            test_commits.len(),
            MAX_TEST_COMMITS
        );
    }

    let mut unmapped_files_total = 0usize;
    let mut accumulators = EvalAccumulators::new(thresholds.len());

    // Pre-allocate scratch buffers reused across commits to avoid per-commit
    // heap allocations in the hot evaluation loop.
    //
    // jaccard_scratch: outer Vec pre-sized to MAX_FILES_PER_COMMIT so it is
    // never reallocated; inner Vecs are cleared and refilled each commit.
    let mut jaccard_scratch: Vec<Vec<(FileId, f64)>> = Vec::with_capacity(MAX_FILES_PER_COMMIT);
    // all_known_scratch: single HashSet of mapped file IDs for this commit,
    // replacing the Q per-query actual HashSets (fixes O(Q²) allocation).
    let mut all_known_scratch: HashSet<FileId> = HashSet::with_capacity(MAX_FILES_PER_COMMIT);
    // known_ids: reused across commits to avoid per-commit allocation.
    let mut known_ids: Vec<FileId> = Vec::with_capacity(MAX_FILES_PER_COMMIT);
    // predicted_scratch: reused across (threshold, query) pairs to avoid Q×T
    // HashSet allocations per commit.
    let mut predicted_scratch: HashSet<FileId> = HashSet::with_capacity(MAX_FILES_PER_COMMIT);

    for commit in test_commits {
        // Resolve all file IDs for this commit. Track unmapped files.
        known_ids.clear();
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

        // Skip bulk-refactor commits to avoid ~320 MB of jaccard cache per commit.
        if known_ids.len() > MAX_FILES_PER_COMMIT {
            continue;
        }

        // Pre-compute jaccard scores once, then sweep thresholds.
        // Jaccard values are threshold-independent: computing them once and sweeping
        // thresholds reduces total work by a factor of T (number of thresholds).

        // Reuse jaccard_scratch: truncate to known_ids length (extending with empty
        // inner Vecs if needed), then clear and refill each inner Vec.
        // jaccard_scratch[q_idx] = Vec of (candidate_id, jaccard) pairs where j > 0.
        fill_jaccard_scratch(&known_ids, &all_file_ids, reader, &mut jaccard_scratch)?;

        // Build a single HashSet of all mapped file IDs in this commit.
        // Used in sweep_thresholds to replace Q per-query actual HashSets.
        all_known_scratch.clear();
        all_known_scratch.extend(known_ids.iter().copied());

        // Sweep thresholds over the cached jaccard values.
        sweep_thresholds(
            &jaccard_scratch,
            &all_known_scratch,
            &known_ids,
            thresholds,
            &mut accumulators,
            &mut predicted_scratch,
        );
    }

    let metrics = accumulators.finalize(thresholds);
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
            ));
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
    // Capture lengths before moving ownership of split.train into build_and_evaluate.
    let train_commits = split.train.len();
    let eval = match build_and_evaluate(split.train, &split.test, thresholds) {
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
        train_commits,
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
        return thresholds.iter().map(|&t| zero_metrics(t)).collect();
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
                return zero_metrics(threshold);
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

/// Build a [`ThresholdMetrics`] with all-zero values for a given threshold.
///
/// Used as the fallback when no repos pass quality gates or no repos contribute
/// data at a particular threshold.
fn zero_metrics(threshold: f64) -> ThresholdMetrics {
    ThresholdMetrics {
        threshold,
        macro_precision: 0.0,
        macro_recall: 0.0,
        macro_f1: 0.0,
        micro_precision: 0.0,
        micro_recall: 0.0,
        micro_f1: 0.0,
        commit_count: 0,
        query_count: 0,
    }
}

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

    // Guard against OOM on unexpectedly large repos.
    if all_commits.len() > MAX_COMMITS_FOR_PARSE {
        anyhow::bail!(
            "parsed commit count {} exceeds MAX_COMMITS_FOR_PARSE ({}) — repo too large",
            all_commits.len(),
            MAX_COMMITS_FOR_PARSE
        );
    }

    for commit in &mut all_commits {
        filter_denied(&mut commit.changed_files);
    }

    Ok((head_sha, all_commits))
}

/// Phase 2: build co-change matrix and evaluate at all thresholds.
///
/// Takes `train` by value to avoid cloning potentially large `Vec<CommitInfo>`.
/// The caller must capture `split.train.len()` before moving ownership here.
fn build_and_evaluate(
    train: Vec<CommitInfo>,
    test: &[CommitInfo],
    thresholds: &[f64],
) -> anyhow::Result<EvalResult> {
    // 6. Build path_map from training commits.
    let path_map = build_path_map(&train)
        .map_err(|e| anyhow::anyhow!("path map construction failed: {e:#}"))?;

    // 7. Build co-change matrix in a tempdir.
    let index_dir = tempfile::tempdir().map_err(|e| anyhow::anyhow!("tempdir failed: {e:#}"))?;

    let builder = CochangeMatrixBuilder::new(index_dir.path().to_path_buf())
        .map_err(|e| anyhow::anyhow!("builder creation failed: {e:#}"))?;

    let commit_count = train.len();
    let history_for_builder = rskim_search::HistoryResult {
        commits: train,
        metadata: rskim_search::TemporalMetadata {
            is_shallow: false,
            commit_count,
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

// ============================================================================
// EvalAccumulators — cohesive accumulator state for sweep_thresholds
// ============================================================================

/// Per-threshold accumulator state for one evaluation run.
///
/// Bundles the 7 parallel index-aligned arrays that were previously passed as
/// separate mutable slice parameters to `sweep_thresholds`.  Each index `ti`
/// corresponds to one Jaccard threshold.
struct EvalAccumulators {
    /// Sum of per-commit macro-precision values, one entry per threshold.
    macro_precision_sum: Vec<f64>,
    /// Sum of per-commit macro-recall values, one entry per threshold.
    macro_recall_sum: Vec<f64>,
    /// Number of commits contributing to macro averages, one entry per threshold.
    macro_commit_count: Vec<usize>,
    /// Micro true-positive count, one entry per threshold.
    micro_tp: Vec<usize>,
    /// Micro predicted-set size sum, one entry per threshold.
    micro_predicted: Vec<usize>,
    /// Micro actual-set size sum, one entry per threshold.
    micro_actual: Vec<usize>,
    /// Number of query-file observations, one entry per threshold.
    micro_query_count: Vec<usize>,
}

impl EvalAccumulators {
    /// Allocate zero-initialised accumulators for `n_thresholds` thresholds.
    fn new(n_thresholds: usize) -> Self {
        Self {
            macro_precision_sum: vec![0.0f64; n_thresholds],
            macro_recall_sum: vec![0.0f64; n_thresholds],
            macro_commit_count: vec![0usize; n_thresholds],
            micro_tp: vec![0usize; n_thresholds],
            micro_predicted: vec![0usize; n_thresholds],
            micro_actual: vec![0usize; n_thresholds],
            micro_query_count: vec![0usize; n_thresholds],
        }
    }

    /// Accumulate one observation with the "actual" set expressed as
    /// `all_known` minus `query_id`.
    ///
    /// This avoids materializing a per-query actual [`HashSet`]: the
    /// intersection count is computed by iterating `predicted` and checking
    /// membership in `all_known`, excluding the query itself.
    ///
    /// `actual_size` must equal `all_known.len() - 1` (the caller pre-computes
    /// this once per commit since it is identical for every query).
    fn accumulate_excluding(
        &mut self,
        ti: usize,
        predicted: &HashSet<FileId>,
        all_known: &HashSet<FileId>,
        query_id: FileId,
        actual_size: usize,
    ) -> (f64, f64) {
        // Count |predicted ∩ actual| where actual = all_known \ {query_id}.
        let intersection = predicted
            .iter()
            .filter(|&&id| id != query_id && all_known.contains(&id))
            .count();

        let precision = if predicted.is_empty() {
            0.0
        } else {
            intersection as f64 / predicted.len() as f64
        };
        let recall = if actual_size == 0 {
            0.0
        } else {
            intersection as f64 / actual_size as f64
        };

        // Micro accumulation.
        self.micro_tp[ti] += intersection;
        self.micro_predicted[ti] += predicted.len();
        self.micro_actual[ti] += actual_size;
        self.micro_query_count[ti] += 1;

        (precision, recall)
    }

    /// Assemble the final [`ThresholdMetrics`] vector from accumulated state.
    ///
    /// Extracts the metrics-assembly phase that previously lived inline at the
    /// end of `evaluate_at_thresholds` (lines ~302-339 prior to this refactor).
    fn finalize(self, thresholds: &[f64]) -> Vec<ThresholdMetrics> {
        thresholds
            .iter()
            .enumerate()
            .map(|(ti, &threshold)| {
                let commit_count = self.macro_commit_count[ti];
                let (macro_p, macro_r) = if commit_count > 0 {
                    (
                        self.macro_precision_sum[ti] / commit_count as f64,
                        self.macro_recall_sum[ti] / commit_count as f64,
                    )
                } else {
                    (0.0, 0.0)
                };

                let micro_p = if self.micro_predicted[ti] > 0 {
                    self.micro_tp[ti] as f64 / self.micro_predicted[ti] as f64
                } else {
                    0.0
                };
                let micro_r = if self.micro_actual[ti] > 0 {
                    self.micro_tp[ti] as f64 / self.micro_actual[ti] as f64
                } else {
                    0.0
                };

                ThresholdMetrics {
                    threshold,
                    macro_precision: macro_p,
                    macro_recall: macro_r,
                    macro_f1: compute_f1(macro_p, macro_r),
                    micro_precision: micro_p,
                    micro_recall: micro_r,
                    micro_f1: compute_f1(micro_p, micro_r),
                    commit_count,
                    query_count: self.micro_query_count[ti],
                }
            })
            .collect()
    }
}

// ============================================================================
// evaluate_at_thresholds helpers (decomposed from the original 175-line body)
// ============================================================================

/// Fill `scratch` in-place with per-query jaccard pairs for a single commit.
///
/// For each `query_id` in `known_ids`, scans all `all_file_ids` and retains
/// `(candidate_id, jaccard)` pairs where `jaccard > 0.0`.  Self-pairs are
/// skipped.  `scratch` is aligned with `known_ids` after the call: it is
/// truncated or extended as needed, and each inner Vec is cleared before
/// being refilled.
///
/// Reusing the outer and inner Vecs across commits eliminates the per-commit
/// heap allocation that `compute_jaccard_cache` (the previous allocation-
/// returning form) incurred on every call.
///
/// # Errors
///
/// Propagates [`SearchError::IndexCorrupted`] and unexpected jaccard errors.
fn fill_jaccard_scratch(
    known_ids: &[FileId],
    all_file_ids: &[FileId],
    reader: &CochangeMatrixReader,
    scratch: &mut Vec<Vec<(FileId, f64)>>,
) -> anyhow::Result<()> {
    let q = known_ids.len();
    // Grow the outer vec if this commit has more queries than any previous one.
    while scratch.len() < q {
        scratch.push(Vec::new());
    }
    // Shrink the outer vec to exactly q entries (does not free inner memory).
    scratch.truncate(q);

    for (q_idx, &query_id) in known_ids.iter().enumerate() {
        scratch[q_idx].clear();
        build_jaccard_pairs_into(query_id, all_file_ids, reader, &mut scratch[q_idx])?;
    }
    Ok(())
}

/// Fill `out` with positive jaccard pairs for a single query file against all candidates.
///
/// Appends `(candidate_id, jaccard)` for every candidate where `jaccard > 0.0`.
/// Self-pairs (where `candidate_id == query_id`) are excluded.
/// Callers must clear `out` before calling if reuse is intended.
fn build_jaccard_pairs_into(
    query_id: FileId,
    all_file_ids: &[FileId],
    reader: &CochangeMatrixReader,
    out: &mut Vec<(FileId, f64)>,
) -> anyhow::Result<()> {
    for &candidate_id in all_file_ids {
        if candidate_id == query_id {
            continue; // skip self-pair
        }
        match reader.jaccard(query_id, candidate_id) {
            Ok(j) if j > 0.0 => out.push((candidate_id, j)),
            Ok(_) => {}
            Err(SearchError::IndexCorrupted(msg)) => {
                return Err(anyhow::anyhow!("matrix corrupted: {msg}"));
            }
            Err(e) => return Err(anyhow::anyhow!("jaccard error: {e}")),
        }
    }
    Ok(())
}

/// Sweep all thresholds over pre-computed jaccard pairs for one commit.
///
/// Updates `accumulators` in-place.  `predicted_scratch` is a caller-owned
/// [`HashSet`] that is cleared and reused for every (threshold, query) pair
/// to avoid Q×T allocations per commit.
///
/// `all_known` is the set of all mapped file IDs in this commit.  For each
/// query the "actual" co-change set is `all_known` minus the query itself,
/// so no per-query actual HashSet is materialized (fixes the O(Q²) allocation).
fn sweep_thresholds(
    jaccard_cache: &[Vec<(FileId, f64)>],
    all_known: &HashSet<FileId>,
    known_ids: &[FileId],
    thresholds: &[f64],
    accumulators: &mut EvalAccumulators,
    predicted_scratch: &mut HashSet<FileId>,
) {
    let query_count = known_ids.len();
    // actual_size is the same for every query in this commit.
    let actual_size = query_count.saturating_sub(1);

    for (ti, &threshold) in thresholds.iter().enumerate() {
        let mut commit_precision_sum = 0.0f64;
        let mut commit_recall_sum = 0.0f64;

        for q_idx in 0..query_count {
            let query_id = known_ids[q_idx];

            // Reuse scratch set: clear then extend with threshold-filtered candidates.
            predicted_scratch.clear();
            predicted_scratch.extend(
                jaccard_cache[q_idx]
                    .iter()
                    .filter(|&&(_, j)| j >= threshold)
                    .map(|&(cid, _)| cid),
            );

            let (p, r) = accumulators.accumulate_excluding(
                ti,
                predicted_scratch,
                all_known,
                query_id,
                actual_size,
            );
            commit_precision_sum += p;
            commit_recall_sum += r;
        }

        // Macro: average over queries within this commit, then accumulate.
        accumulators.macro_precision_sum[ti] += commit_precision_sum / query_count as f64;
        accumulators.macro_recall_sum[ti] += commit_recall_sum / query_count as f64;
        accumulators.macro_commit_count[ti] += 1;
    }
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
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-c")
        .arg("credential.helper=")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "HEAD"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());

    let output = rskim_research::clone::git_output_with_timeout(
        cmd,
        "git rev-parse HEAD",
        GIT_SHA_TIMEOUT_SECS,
    )?;

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
    use std::collections::{HashMap, HashSet};
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
        let map1 = build_path_map(&commits).expect("build_path_map 1");
        let map2 = build_path_map(&commits).expect("build_path_map 2");
        assert_eq!(map1, map2, "path_map must be deterministic");
    }

    #[test]
    fn path_map_sorted_alphabetically() {
        let commits = vec![make_commit(0, 100, &["z.rs", "a.rs", "m.rs"])];
        let map = build_path_map(&commits).expect("build_path_map");
        // a.rs → 0, m.rs → 1, z.rs → 2
        assert_eq!(*map.get(&PathBuf::from("a.rs")).unwrap(), FileId(0));
        assert_eq!(*map.get(&PathBuf::from("m.rs")).unwrap(), FileId(1));
        assert_eq!(*map.get(&PathBuf::from("z.rs")).unwrap(), FileId(2));
    }

    // --- evaluate_at_thresholds: empty / unmappable commit path ---

    #[test]
    fn evaluate_at_thresholds_zero_metrics_when_all_commits_unmappable() {
        // Build a reader from an empty history (no training commits → empty matrix).
        // Test commits contain only files absent from path_map, so known_ids.len() < 2
        // for every commit.  evaluate_at_thresholds must return zero metrics without
        // panicking or returning an error.
        use rskim_search::cochange::{CochangeMatrixBuilder, CochangeMatrixReader};
        use rskim_search::{HistoryResult, TemporalMetadata};

        let index_dir = tempfile::tempdir().expect("tempdir");
        let builder = CochangeMatrixBuilder::new(index_dir.path().to_path_buf()).expect("builder");
        let empty_history = HistoryResult {
            commits: vec![],
            metadata: TemporalMetadata {
                is_shallow: false,
                commit_count: 0,
            },
        };
        let empty_path_map = HashMap::new();
        builder
            .build(&empty_history, &empty_path_map)
            .expect("build empty matrix");
        let reader = CochangeMatrixReader::open(index_dir.path()).expect("reader open");

        // Test commits: one single-file commit (skipped: known_ids.len() < 2)
        // and one commit whose only file is not in path_map (skipped: unmapped).
        let test_commits = vec![
            make_commit(0, 100, &["unmapped_a.rs"]),
            make_commit(1, 200, &["unmapped_b.rs"]),
        ];
        let thresholds = vec![0.1, 0.3];

        let (metrics, unmapped) =
            evaluate_at_thresholds(&reader, &test_commits, &empty_path_map, &thresholds)
                .expect("evaluate_at_thresholds must not error on all-unmapped commits");

        assert_eq!(
            metrics.len(),
            thresholds.len(),
            "must return one ThresholdMetrics per threshold"
        );
        assert_eq!(
            unmapped, 2,
            "both files are unmapped; expected unmapped_files_total=2, got {unmapped}"
        );
        for m in &metrics {
            assert_eq!(
                m.macro_precision, 0.0,
                "macro_precision must be 0.0 when no commits contribute; threshold={}",
                m.threshold
            );
            assert_eq!(
                m.macro_recall, 0.0,
                "macro_recall must be 0.0 when no commits contribute; threshold={}",
                m.threshold
            );
            assert_eq!(
                m.commit_count, 0,
                "commit_count must be 0 when all commits are skipped; threshold={}",
                m.threshold
            );
        }
    }

    #[test]
    fn evaluate_at_thresholds_zero_metrics_for_single_file_commits() {
        // Single-file commits (after path mapping) have known_ids.len() < 2 and
        // must be skipped for macro averaging, producing zero metrics.
        use rskim_search::cochange::{CochangeMatrixBuilder, CochangeMatrixReader};
        use rskim_search::{HistoryResult, TemporalMetadata};

        let index_dir = tempfile::tempdir().expect("tempdir");

        // Build a path_map with two files so the matrix is non-empty, but
        // test commits will only touch one file each.
        let train_commits = vec![make_commit(0, 100, &["a.rs", "b.rs"])];
        let path_map = build_path_map(&train_commits).expect("build_path_map");

        let builder = CochangeMatrixBuilder::new(index_dir.path().to_path_buf()).expect("builder");
        let history = HistoryResult {
            commits: train_commits,
            metadata: TemporalMetadata {
                is_shallow: false,
                commit_count: 1,
            },
        };
        builder.build(&history, &path_map).expect("build matrix");
        let reader = CochangeMatrixReader::open(index_dir.path()).expect("reader open");

        // Each test commit touches only one mapped file → known_ids.len() == 1 → skipped.
        let test_commits = vec![
            make_commit(1, 200, &["a.rs"]),
            make_commit(2, 300, &["b.rs"]),
        ];
        let thresholds = vec![0.1];

        let (metrics, _unmapped) =
            evaluate_at_thresholds(&reader, &test_commits, &path_map, &thresholds)
                .expect("evaluate_at_thresholds must succeed on single-file test commits");

        assert_eq!(metrics.len(), 1);
        assert_eq!(
            metrics[0].commit_count, 0,
            "single-file commits must not contribute to macro averaging"
        );
        assert_eq!(
            metrics[0].macro_precision, 0.0,
            "macro_precision must be 0.0 when all test commits are single-file"
        );
        assert_eq!(
            metrics[0].macro_recall, 0.0,
            "macro_recall must be 0.0 when all test commits are single-file"
        );
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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("multi-file commits")
        );
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
