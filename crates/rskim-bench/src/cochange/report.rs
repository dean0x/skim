//! Report generation for co-change validation results.
//!
//! Two formats are supported:
//! - **JSON** — full machine-readable output via `serde_json`.
//! - **Markdown** — human-readable report with threshold sweep table,
//!   per-repo breakdown, methodology section, and reproducibility manifest.

use super::types::{CochangeValidationResult, RepoCochangeResult, ThresholdMetrics};

// ============================================================================
// JSON
// ============================================================================

/// Serialise a [`CochangeValidationResult`] to a pretty-printed JSON string.
///
/// # Errors
///
/// Returns an error if serialisation fails (should not happen in practice with
/// the types used here).
pub fn to_json(result: &CochangeValidationResult) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(result)?)
}

// ============================================================================
// Markdown
// ============================================================================

/// Render a [`CochangeValidationResult`] as a Markdown report.
///
/// Sections:
/// 1. Summary — best threshold by macro F1, aggregate precision/recall.
/// 2. Threshold sweep table.
/// 3. Per-repo breakdown.
/// 4. Methodology.
/// 5. Reproducibility manifest.
#[must_use]
pub fn to_markdown(result: &CochangeValidationResult) -> String {
    let mut md = String::new();

    md.push_str("# Co-change Validation Report\n\n");

    // --- 1. Summary ---
    md.push_str("## Summary\n\n");
    if let Some(best) = best_by_macro_f1(&result.aggregate_metrics) {
        md.push_str(&format!(
            "- **Best threshold (macro F1):** {:.2} — F1 {:.4}, P {:.4}, R {:.4}\n",
            best.threshold, best.macro_f1, best.macro_precision, best.macro_recall,
        ));
    } else {
        md.push_str("- No aggregate metrics available (all repos failed quality gates).\n");
    }
    let passing = result
        .repos
        .iter()
        .filter(|r| r.quality_gate_passed && r.error.is_none())
        .count();
    let total = result.repos.len();
    md.push_str(&format!(
        "- **Repos evaluated:** {passing}/{total} passed quality gates\n"
    ));
    md.push_str(&format!(
        "- **Run timestamp:** {}\n\n",
        result.run_metadata.timestamp
    ));

    // --- 2. Threshold sweep table ---
    md.push_str("## Threshold Sweep (Aggregate)\n\n");
    if result.aggregate_metrics.is_empty() {
        md.push_str("(no aggregate metrics)\n\n");
    } else {
        md.push_str(
            "| Threshold | Macro P | Macro R | Macro F1 | Micro P | Micro R | Micro F1 | Commits | Queries |\n",
        );
        md.push_str(
            "|-----------|---------|---------|----------|---------|---------|----------|---------|--------|\n",
        );
        for m in &result.aggregate_metrics {
            md.push_str(&threshold_row(m));
        }
        md.push('\n');
    }

    // --- 3. Per-repo breakdown ---
    md.push_str("## Per-Repo Results\n\n");
    for repo in &result.repos {
        md.push_str(&repo_section(repo));
    }

    // --- 4. Methodology ---
    md.push_str("## Methodology\n\n");
    md.push_str(
        "- **Train/test split:** 80/20 (chronological, oldest commits train, newest test)\n",
    );
    md.push_str("- **Quality gates:** ≥50 multi-file commits, ≥6 months history span\n");
    md.push_str("- **Precision:** |predicted ∩ actual| / |predicted| per query\n");
    md.push_str(
        "- **Recall:** |predicted ∩ actual| / |actual| per query (unmapped files excluded)\n",
    );
    md.push_str("- **Macro average:** per-commit, then averaged across commits\n");
    md.push_str("- **Micro average:** accumulated over all individual queries\n");
    md.push_str("- **Deny-list patterns applied:**\n");
    for pattern in &result.deny_list_patterns {
        md.push_str(&format!("  - `{pattern}`\n"));
    }
    md.push('\n');

    // --- 5. Reproducibility manifest ---
    md.push_str("## Reproducibility Manifest\n\n");
    md.push_str(&format!(
        "- **Corpus config:** `{}`\n\n",
        result.run_metadata.corpus_config_path
    ));
    md.push_str("| Repo | HEAD SHA | Train Cutoff | Train Commits | Test Commits |\n");
    md.push_str("|------|----------|--------------|---------------|--------------|\n");
    for m in &result.run_metadata.repo_manifests {
        let short_name = m.repo_url.rsplit('/').next().unwrap_or(&m.repo_url);
        let short_sha = &m.head_sha[..m.head_sha.len().min(8)];
        md.push_str(&format!(
            "| {} | `{}` | {} | {} | {} |\n",
            short_name,
            short_sha,
            m.train_cutoff_timestamp,
            m.train_commit_count,
            m.test_commit_count,
        ));
    }
    md.push('\n');

    md
}

// ============================================================================
// Private helpers
// ============================================================================

fn best_by_macro_f1(metrics: &[ThresholdMetrics]) -> Option<&ThresholdMetrics> {
    metrics.iter().max_by(|a, b| {
        a.macro_f1
            .partial_cmp(&b.macro_f1)
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn threshold_row(m: &ThresholdMetrics) -> String {
    format!(
        "| {:.2} | {:.4} | {:.4} | {:.4} | {:.4} | {:.4} | {:.4} | {} | {} |\n",
        m.threshold,
        m.macro_precision,
        m.macro_recall,
        m.macro_f1,
        m.micro_precision,
        m.micro_recall,
        m.micro_f1,
        m.commit_count,
        m.query_count,
    )
}

fn repo_section(repo: &RepoCochangeResult) -> String {
    let mut md = String::new();
    md.push_str(&format!("### {}\n\n", repo.repo_name));

    if let Some(ref err) = repo.error {
        md.push_str(&format!("**Error:** {err}\n\n"));
        return md;
    }

    if !repo.quality_gate_passed {
        let reason = repo
            .quality_gate_reason
            .as_deref()
            .unwrap_or("unknown reason");
        md.push_str(&format!("**Quality gate failed:** {reason}\n\n"));
        return md;
    }

    md.push_str(&format!(
        "- Commits: {} train / {} test ({} multi-file, {} single-file)\n",
        repo.train_commits,
        repo.test_commits,
        repo.multi_file_test_commits,
        repo.single_file_test_commits,
    ));
    md.push_str(&format!(
        "- Matrix: {} files, {} pairs\n",
        repo.file_count, repo.pair_count,
    ));
    if repo.commits_skipped_too_large > 0 {
        md.push_str(&format!(
            "- Commits skipped (too large): {}\n",
            repo.commits_skipped_too_large,
        ));
    }
    if repo.unmapped_files_in_test > 0 {
        md.push_str(&format!(
            "- Unmapped files in test: {}\n",
            repo.unmapped_files_in_test,
        ));
    }
    md.push('\n');

    if repo.metrics_by_threshold.is_empty() {
        md.push_str("(no metrics)\n\n");
        return md;
    }

    md.push_str("| Threshold | Macro P | Macro R | Macro F1 | Micro P | Micro R | Micro F1 |\n");
    md.push_str("|-----------|---------|---------|----------|---------|---------|----------|\n");
    for m in &repo.metrics_by_threshold {
        md.push_str(&format!(
            "| {:.2} | {:.4} | {:.4} | {:.4} | {:.4} | {:.4} | {:.4} |\n",
            m.threshold,
            m.macro_precision,
            m.macro_recall,
            m.macro_f1,
            m.micro_precision,
            m.micro_recall,
            m.micro_f1,
        ));
    }
    md.push('\n');

    md
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::cochange::types::{
        CochangeValidationResult, RepoCochangeResult, RepoManifest, RunMetadata, ThresholdMetrics,
    };

    fn sample_threshold(t: f64) -> ThresholdMetrics {
        ThresholdMetrics {
            threshold: t,
            macro_precision: 0.6,
            macro_recall: 0.5,
            macro_f1: 0.545,
            micro_precision: 0.7,
            micro_recall: 0.45,
            micro_f1: 0.548,
            commit_count: 30,
            query_count: 90,
        }
    }

    fn sample_result() -> CochangeValidationResult {
        CochangeValidationResult {
            repos: vec![
                RepoCochangeResult {
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
                    commits_skipped_too_large: 1,
                    split_timestamp: 1_700_000_000,
                    metrics_by_threshold: vec![sample_threshold(0.1), sample_threshold(0.2)],
                    quality_gate_passed: true,
                    quality_gate_reason: None,
                    error: None,
                },
                RepoCochangeResult {
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
                },
            ],
            aggregate_metrics: vec![sample_threshold(0.1), sample_threshold(0.2)],
            thresholds: vec![0.1, 0.2],
            deny_list_patterns: vec!["Cargo.lock".to_string(), "go.sum".to_string()],
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

    // --- JSON ---

    #[test]
    fn json_output_is_valid() {
        let result = sample_result();
        let json = to_json(&result).unwrap();
        let _: serde_json::Value = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn json_output_contains_required_fields() {
        let result = sample_result();
        let json = to_json(&result).unwrap();
        assert!(json.contains("repos"));
        assert!(json.contains("aggregate_metrics"));
        assert!(json.contains("thresholds"));
        assert!(json.contains("run_metadata"));
        assert!(json.contains("deny_list_patterns"));
    }

    #[test]
    fn json_includes_per_repo_data() {
        let result = sample_result();
        let json = to_json(&result).unwrap();
        assert!(json.contains("quality_gate_passed"));
        assert!(json.contains("metrics_by_threshold"));
    }

    // --- Markdown ---

    #[test]
    fn markdown_output_non_empty() {
        let result = sample_result();
        let md = to_markdown(&result);
        assert!(!md.is_empty());
    }

    #[test]
    fn markdown_contains_threshold_table_header() {
        let result = sample_result();
        let md = to_markdown(&result);
        assert!(
            md.contains("Threshold"),
            "markdown should contain threshold table header"
        );
        assert!(md.contains("Macro P"));
        assert!(md.contains("Macro R"));
        assert!(md.contains("Macro F1"));
        assert!(md.contains("Micro P"));
    }

    #[test]
    fn markdown_contains_repo_breakdown() {
        let result = sample_result();
        let md = to_markdown(&result);
        assert!(md.contains("repo"), "should include repo name");
        assert!(
            md.contains("Per-Repo Results"),
            "should have per-repo section"
        );
    }

    #[test]
    fn markdown_contains_methodology_section() {
        let result = sample_result();
        let md = to_markdown(&result);
        assert!(md.contains("Methodology"));
        assert!(md.contains("quality gates"));
    }

    #[test]
    fn markdown_contains_reproducibility_section() {
        let result = sample_result();
        let md = to_markdown(&result);
        assert!(md.contains("Reproducibility Manifest"));
        assert!(md.contains("cochange-corpus.toml"));
    }

    #[test]
    fn markdown_failed_repo_shows_reason() {
        let result = sample_result();
        let md = to_markdown(&result);
        assert!(md.contains("failing"));
        assert!(md.contains("too few commits"));
    }

    #[test]
    fn markdown_shows_best_threshold() {
        let result = sample_result();
        let md = to_markdown(&result);
        assert!(
            md.contains("Best threshold"),
            "should highlight best threshold"
        );
    }

    #[test]
    fn empty_aggregate_metrics_handled() {
        let mut result = sample_result();
        result.aggregate_metrics.clear();
        let md = to_markdown(&result);
        assert!(md.contains("no aggregate metrics"));
    }
}
