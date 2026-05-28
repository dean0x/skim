//! Types for the co-change validation benchmark.
//!
//! All types derive `Debug`, `Clone`, `Serialize`, and `Deserialize` so results
//! can be written to JSON and reloaded for post-hoc analysis.

use serde::{Deserialize, Serialize};

// ============================================================================
// Per-threshold metrics
// ============================================================================

/// Precision, recall, and F1 at a single Jaccard threshold.
///
/// Two averaging strategies are reported:
/// - **Macro** (per-commit): average precision/recall over all multi-file test
///   commits that have at least one known query file.
/// - **Micro** (per-query): aggregate over individual file queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThresholdMetrics {
    /// Jaccard similarity threshold used to decide "predicted" vs "not predicted".
    pub threshold: f64,
    /// Macro-averaged precision (per-commit).
    pub macro_precision: f64,
    /// Macro-averaged recall (per-commit).
    pub macro_recall: f64,
    /// Macro-averaged F1 (per-commit).
    pub macro_f1: f64,
    /// Micro-averaged precision (per-query).
    pub micro_precision: f64,
    /// Micro-averaged recall (per-query).
    pub micro_recall: f64,
    /// Micro-averaged F1 (per-query).
    pub micro_f1: f64,
    /// Number of multi-file test commits used for macro averaging.
    pub commit_count: usize,
    /// Total number of individual file queries used for micro averaging.
    pub query_count: usize,
}

// ============================================================================
// Per-repo result
// ============================================================================

/// Validation result for a single repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoCochangeResult {
    /// Full repository URL.
    pub repo_url: String,
    /// Short name derived from the URL (last path segment without `.git`).
    pub repo_name: String,
    /// HEAD SHA at validation time.
    pub head_sha: String,
    /// Number of commits in the training split.
    pub train_commits: usize,
    /// Number of commits in the test split.
    pub test_commits: usize,
    /// Number of multi-file commits in the test split (used for macro averaging).
    pub multi_file_test_commits: usize,
    /// Number of single-file commits in the test split (skipped by evaluator).
    pub single_file_test_commits: usize,
    /// Number of test-split file references that could not be mapped to a
    /// training FileId and were therefore excluded from recall computation.
    pub unmapped_files_in_test: usize,
    /// Total distinct files in the co-change matrix.
    pub file_count: usize,
    /// Total co-change pairs in the matrix.
    pub pair_count: usize,
    /// Commits skipped because they touched too many files (bulk refactors).
    pub commits_skipped_too_large: usize,
    /// Unix timestamp at the train/test split boundary (first test commit's
    /// timestamp, or 0 if no temporal split was performed).
    pub split_timestamp: i64,
    /// Per-threshold precision/recall results.
    pub metrics_by_threshold: Vec<ThresholdMetrics>,
    /// Whether this repo passed the quality gates.
    pub quality_gate_passed: bool,
    /// Human-readable reason when `quality_gate_passed == false`.
    pub quality_gate_reason: Option<String>,
    /// Error message if the repo could not be processed at all.
    pub error: Option<String>,
}

// ============================================================================
// Aggregate result
// ============================================================================

/// Full co-change validation result across all repos.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CochangeValidationResult {
    /// Per-repo results (includes repos that failed quality gates or errored).
    pub repos: Vec<RepoCochangeResult>,
    /// Macro-average across repos that passed quality gates.
    pub aggregate_metrics: Vec<ThresholdMetrics>,
    /// Thresholds evaluated (in ascending order).
    pub thresholds: Vec<f64>,
    /// Deny-list patterns applied before quality gate checks.
    pub deny_list_patterns: Vec<String>,
    /// Run-level metadata for reproducibility.
    pub run_metadata: RunMetadata,
}

/// Run-level metadata for reproducibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMetadata {
    /// ISO-8601 timestamp when the run started.
    pub timestamp: String,
    /// Path to the corpus config file used.
    pub corpus_config_path: String,
    /// Per-repo manifests with train/test split details.
    pub repo_manifests: Vec<RepoManifest>,
}

/// Reproducibility manifest for a single repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoManifest {
    /// Full repository URL.
    pub repo_url: String,
    /// HEAD SHA at validation time.
    pub head_sha: String,
    /// Unix timestamp at which the temporal split was made.
    pub train_cutoff_timestamp: i64,
    /// Number of training commits below the cutoff.
    pub train_commit_count: usize,
    /// Number of test commits at or above the cutoff.
    pub test_commit_count: usize,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn sample_threshold_metrics() -> ThresholdMetrics {
        ThresholdMetrics {
            threshold: 0.1,
            macro_precision: 0.75,
            macro_recall: 0.60,
            macro_f1: 0.67,
            micro_precision: 0.80,
            micro_recall: 0.55,
            micro_f1: 0.65,
            commit_count: 42,
            query_count: 120,
        }
    }

    fn sample_repo_result() -> RepoCochangeResult {
        RepoCochangeResult {
            repo_url: "https://github.com/example/repo".to_string(),
            repo_name: "repo".to_string(),
            head_sha: "a".repeat(40),
            train_commits: 80,
            test_commits: 20,
            multi_file_test_commits: 15,
            single_file_test_commits: 5,
            unmapped_files_in_test: 3,
            file_count: 150,
            pair_count: 500,
            commits_skipped_too_large: 2,
            split_timestamp: 1_700_000_000,
            metrics_by_threshold: vec![sample_threshold_metrics()],
            quality_gate_passed: true,
            quality_gate_reason: None,
            error: None,
        }
    }

    fn sample_validation_result() -> CochangeValidationResult {
        CochangeValidationResult {
            repos: vec![sample_repo_result()],
            aggregate_metrics: vec![sample_threshold_metrics()],
            thresholds: vec![0.01, 0.05, 0.1, 0.2],
            deny_list_patterns: vec!["Cargo.lock".to_string(), "package-lock.json".to_string()],
            run_metadata: RunMetadata {
                timestamp: "2024-01-15T10:30:00Z".to_string(),
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

    #[test]
    fn threshold_metrics_serde_roundtrip() {
        let original = sample_threshold_metrics();
        let json = serde_json::to_string(&original).unwrap();
        let restored: ThresholdMetrics = serde_json::from_str(&json).unwrap();
        assert!((restored.threshold - original.threshold).abs() < f64::EPSILON);
        assert!((restored.macro_f1 - original.macro_f1).abs() < f64::EPSILON);
        assert_eq!(restored.commit_count, original.commit_count);
        assert_eq!(restored.query_count, original.query_count);
    }

    #[test]
    fn repo_result_serde_roundtrip() {
        let original = sample_repo_result();
        let json = serde_json::to_string(&original).unwrap();
        let restored: RepoCochangeResult = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.repo_url, original.repo_url);
        assert_eq!(restored.quality_gate_passed, original.quality_gate_passed);
        assert!(restored.error.is_none());
    }

    #[test]
    fn repo_result_with_error_serde_roundtrip() {
        let mut result = sample_repo_result();
        result.quality_gate_passed = false;
        result.quality_gate_reason = Some("too few commits".to_string());
        result.error = Some("clone failed".to_string());

        let json = serde_json::to_string(&result).unwrap();
        let restored: RepoCochangeResult = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.quality_gate_reason.as_deref(), Some("too few commits"));
        assert_eq!(restored.error.as_deref(), Some("clone failed"));
    }

    #[test]
    fn validation_result_serde_roundtrip() {
        let original = sample_validation_result();
        let json = serde_json::to_string_pretty(&original).unwrap();
        let restored: CochangeValidationResult = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.repos.len(), original.repos.len());
        assert_eq!(restored.thresholds, original.thresholds);
        assert_eq!(
            restored.run_metadata.corpus_config_path,
            original.run_metadata.corpus_config_path
        );
    }

    #[test]
    fn repo_manifest_serde_roundtrip() {
        let manifest = RepoManifest {
            repo_url: "https://github.com/example/test".to_string(),
            head_sha: "b".repeat(40),
            train_cutoff_timestamp: 1_600_000_000,
            train_commit_count: 100,
            test_commit_count: 25,
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let restored: RepoManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.train_cutoff_timestamp, manifest.train_cutoff_timestamp);
    }
}
