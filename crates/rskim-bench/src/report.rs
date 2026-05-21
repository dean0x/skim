//! Report generation — JSON and Markdown table output.
//!
//! Both formats include:
//! - Per-repo train + test metrics
//! - Aggregate (macro-average) train + test metrics
//! - Tuning convergence trace (if provided)

use rskim_search::SearchField;

use crate::types::{BenchResult, ConfigMetrics, TuningResult};

/// Serialise a `BenchResult` (and optional tuning result) to a JSON string.
///
/// # Errors
///
/// Returns an error if serialisation fails (should not happen in practice
/// with the types used here).
pub fn to_json(result: &BenchResult, tuning: Option<&TuningResult>) -> anyhow::Result<String> {
    let mut obj = serde_json::to_value(result)?;

    if let Some(t) = tuning {
        let tuning_val = serde_json::to_value(t)?;
        if let serde_json::Value::Object(ref mut map) = obj {
            map.insert("tuning".to_string(), tuning_val);
        }
    }

    Ok(serde_json::to_string_pretty(&obj)?)
}

/// Convert a `SearchField` snake_case name to PascalCase for display.
///
/// Derives the display name from `SearchField::name()` so that adding a new
/// variant only requires updating one place (the authoritative `name()` match
/// in rskim-search). A bench-only PascalCase variant is not warranted; deriving
/// it programmatically keeps the two crates in sync without cross-crate coupling.
fn field_display_name(field: SearchField) -> String {
    field
        .name()
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect()
}

/// Render the tuning summary, convergence trace, and field boosts table.
fn tuning_section(t: &TuningResult) -> String {
    let mut md = String::new();

    md.push_str("## Tuning Results\n\n");
    md.push_str(&format!("- Best k1: {:.3}\n", t.best_k1));
    md.push_str(&format!("- Best train MRR: {:.4}\n", t.best_train_mrr));
    md.push_str(&format!("- Passes needed: {}\n\n", t.passes_needed));

    if !t.convergence_history.is_empty() {
        md.push_str("### Convergence Trace\n\n");
        md.push_str("| Pass | Parameter | From | To | MRR Improvement |\n");
        md.push_str("|------|-----------|------|----|-----------------|\n");
        for step in &t.convergence_history {
            md.push_str(&format!(
                "| {} | {} | {:.4} | {:.4} | +{:.6} |\n",
                step.pass, step.parameter, step.from_value, step.to_value, step.mrr_improvement,
            ));
        }
        md.push('\n');
    }

    md.push_str("### Best Field Boosts\n\n");
    md.push_str("| Field | Boost | b |\n");
    md.push_str("|-------|-------|---|\n");
    for (i, field) in SearchField::ALL.iter().enumerate() {
        let name = field_display_name(*field);
        let boost = t.best_field_boosts[i];
        let b = t.best_field_b[i];
        md.push_str(&format!("| {name} | {boost:.2} | {b:.2} |\n"));
    }
    md.push('\n');

    md
}

/// Render a `BenchResult` as a Markdown report string.
pub fn to_markdown(result: &BenchResult, tuning: Option<&TuningResult>) -> String {
    let mut md = String::new();

    md.push_str("# BM25F Benchmark Report\n\n");

    // Aggregate results
    md.push_str("## Aggregate Results (Macro-Average)\n\n");
    md.push_str("### Train Split\n\n");
    md.push_str(&metrics_table(&result.aggregate_train));
    md.push_str("\n### Test Split\n\n");
    md.push_str(&metrics_table(&result.aggregate_test));
    md.push('\n');

    // Per-repo results
    for repo in &result.repos {
        let repo_name = repo.repo_url.rsplit('/').next().unwrap_or(&repo.repo_url);
        md.push_str(&format!("## Repo: {repo_name}\n\n"));
        md.push_str(&format!("- Qrels: {}\n\n", repo.qrel_count));

        md.push_str("### Train Split\n\n");
        md.push_str(&metrics_table(&repo.train_metrics));
        md.push_str("\n### Test Split\n\n");
        md.push_str(&metrics_table(&repo.test_metrics));
        md.push('\n');
    }

    // Tuning convergence trace
    if let Some(t) = tuning {
        md.push_str(&tuning_section(t));
    }

    md
}

fn metrics_table(metrics: &[ConfigMetrics]) -> String {
    if metrics.is_empty() {
        return "(no results)\n".to_string();
    }
    let mut table = String::new();
    table.push_str("| Config | MRR | P@5 | P@10 | Queries | @Rank1 |\n");
    table.push_str("|--------|-----|-----|------|---------|--------|\n");
    for m in metrics {
        table.push_str(&format!(
            "| {} | {:.4} | {:.4} | {:.4} | {} | {} |\n",
            m.config_name,
            m.mrr,
            m.precision_at_5,
            m.precision_at_10,
            m.query_count,
            m.found_at_rank_1,
        ));
    }
    table
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code — unwrap acceptable for test assertions
mod tests {
    use super::*;
    use crate::types::RepoBenchResult;
    use rskim_search::FIELD_COUNT;

    fn sample_result() -> BenchResult {
        let metrics = vec![
            ConfigMetrics {
                config_name: "uniform".to_string(),
                mrr: 0.42,
                precision_at_5: 0.25,
                precision_at_10: 0.15,
                query_count: 20,
                found_at_rank_1: 8,
            },
            ConfigMetrics {
                config_name: "default_8field".to_string(),
                mrr: 0.65,
                precision_at_5: 0.40,
                precision_at_10: 0.25,
                query_count: 20,
                found_at_rank_1: 13,
            },
        ];
        BenchResult {
            repos: vec![RepoBenchResult {
                repo_url: "https://github.com/example/repo".to_string(),
                train_metrics: metrics.clone(),
                test_metrics: metrics.clone(),
                qrel_count: 30,
            }],
            aggregate_train: metrics.clone(),
            aggregate_test: metrics,
        }
    }

    #[test]
    fn json_output_is_valid() {
        let result = sample_result();
        let json_str = to_json(&result, None).unwrap();
        let _: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    }

    #[test]
    fn json_output_contains_aggregate_keys() {
        let result = sample_result();
        let json_str = to_json(&result, None).unwrap();
        assert!(json_str.contains("aggregate_train"));
        assert!(json_str.contains("aggregate_test"));
        assert!(json_str.contains("repos"));
    }

    #[test]
    fn json_output_includes_tuning_when_provided() {
        let result = sample_result();
        let tuning = TuningResult {
            best_k1: 1.5,
            best_field_boosts: [5.0; FIELD_COUNT],
            best_field_b: [0.75; FIELD_COUNT],
            best_train_mrr: 0.75,
            convergence_history: vec![],
            passes_needed: 2,
        };
        let json_str = to_json(&result, Some(&tuning)).unwrap();
        assert!(json_str.contains("tuning"));
        assert!(json_str.contains("best_k1"));
    }

    #[test]
    fn markdown_output_contains_headers() {
        let result = sample_result();
        let md = to_markdown(&result, None);
        assert!(md.contains("# BM25F Benchmark Report"));
        assert!(md.contains("## Aggregate Results"));
        assert!(md.contains("### Train Split"));
        assert!(md.contains("### Test Split"));
    }

    #[test]
    fn markdown_output_contains_config_names() {
        let result = sample_result();
        let md = to_markdown(&result, None);
        assert!(md.contains("uniform"));
        assert!(md.contains("default_8field"));
    }

    #[test]
    fn markdown_includes_tuning_section() {
        let result = sample_result();
        let tuning = TuningResult {
            best_k1: 1.5,
            best_field_boosts: [5.0; FIELD_COUNT],
            best_field_b: [0.75; FIELD_COUNT],
            best_train_mrr: 0.75,
            convergence_history: vec![crate::types::ConvergenceStep {
                pass: 1,
                parameter: "k1".to_string(),
                from_value: 1.2,
                to_value: 1.5,
                mrr_improvement: 0.05,
            }],
            passes_needed: 1,
        };
        let md = to_markdown(&result, Some(&tuning));
        assert!(md.contains("## Tuning Results"));
        assert!(md.contains("### Convergence Trace"));
        assert!(md.contains("### Best Field Boosts"));
    }

    #[test]
    fn markdown_empty_metrics_shows_placeholder() {
        let empty_result = BenchResult {
            repos: vec![],
            aggregate_train: vec![],
            aggregate_test: vec![],
        };
        let md = to_markdown(&empty_result, None);
        assert!(md.contains("(no results)"));
    }
}
