//! Threshold-filtered insight engine for `skim heatmap --insights`.
//!
//! Pure functions, no I/O. Scans [`HeatmapResult`] against hardcoded thresholds
//! and produces a sorted [`Vec<Insight>`] and an [`InsightsResult`] for rendering.

use super::types::{
    CompactFileEntry, CompactModuleEntry, HeatmapResult, Insight, InsightsResult, Severity,
};

// ============================================================================
// Threshold constants
// ============================================================================

/// Stability score below which a file is CRITICAL.
const STABILITY_CRITICAL: u8 = 40;
/// Stability score below which a file is a WARNING (but above critical).
const STABILITY_WARNING: u8 = 70;

/// Fix-risk combined_pct above which a file is CRITICAL.
const FIX_RISK_CRITICAL: f64 = 50.0;
/// Fix-risk combined_pct above which a file is a WARNING (but below critical).
const FIX_RISK_WARNING: f64 = 30.0;

/// Encapsulation_pct below which a module is CRITICAL.
const ENCAPSULATION_CRITICAL: f64 = 40.0;
/// Encapsulation_pct below which a module is a WARNING (but above critical).
const ENCAPSULATION_WARNING: f64 = 60.0;

/// Coupling confidence above which a coupling pair is a WARNING.
const COUPLING_WARNING: f64 = 0.8;

// ============================================================================
// Public API
// ============================================================================

/// Scan all files and modules in `result`, applying hardcoded thresholds to
/// produce a sorted list of insights.
///
/// Sorting rules:
/// 1. By severity ascending (Critical first — `Severity::Critical < Severity::Warning` via Ord).
/// 2. Within same severity, by `metric_value` descending (worst first).
pub(crate) fn compute_insights(result: &HeatmapResult) -> Vec<Insight> {
    let mut insights: Vec<Insight> = Vec::new();

    for file in &result.files {
        // Stability
        let stability = file.stability_score;
        if stability < STABILITY_CRITICAL {
            insights.push(Insight {
                severity: Severity::Critical,
                category: "stability".to_string(),
                file: file.path.clone(),
                message: format!(
                    "{}: critically unstable (score {}/100)",
                    file.path, stability
                ),
                metric_value: f64::from(stability),
            });
        } else if stability < STABILITY_WARNING {
            insights.push(Insight {
                severity: Severity::Warning,
                category: "stability".to_string(),
                file: file.path.clone(),
                message: format!(
                    "{}: moderate instability (score {}/100)",
                    file.path, stability
                ),
                metric_value: f64::from(stability),
            });
        }

        // Fix risk (skip if insufficient data)
        if !file.fix_risk.insufficient_data {
            let combined = file.fix_risk.combined_pct;
            if combined > FIX_RISK_CRITICAL {
                insights.push(Insight {
                    severity: Severity::Critical,
                    category: "fix_risk".to_string(),
                    file: file.path.clone(),
                    message: format!("{}: high fix-risk ({combined:.1}% combined)", file.path),
                    metric_value: combined,
                });
            } else if combined > FIX_RISK_WARNING {
                insights.push(Insight {
                    severity: Severity::Warning,
                    category: "fix_risk".to_string(),
                    file: file.path.clone(),
                    message: format!("{}: elevated fix-risk ({combined:.1}% combined)", file.path),
                    metric_value: combined,
                });
            }
        }

        // Bus factor
        if file.authors.single_owner_risk {
            let pct = file.authors.top_author_pct;
            let count = file.authors.count;
            insights.push(Insight {
                severity: Severity::Warning,
                category: "bus_factor".to_string(),
                file: file.path.clone(),
                message: format!(
                    "{}: bus-factor risk ({pct:.1}%, {count} author(s))",
                    file.path
                ),
                metric_value: pct,
            });
        }

        // Coupling — emit one Warning per coupling partner above threshold
        for entry in &file.blast_radius {
            if entry.confidence > COUPLING_WARNING {
                let conf_pct = entry.confidence * 100.0;
                let support = entry.support;
                insights.push(Insight {
                    severity: Severity::Warning,
                    category: "coupling".to_string(),
                    file: file.path.clone(),
                    message: format!(
                        "{}: tightly coupled with {} ({conf_pct:.1}% confidence, {support} co-changes)",
                        file.path, entry.path
                    ),
                    metric_value: entry.confidence,
                });
            }
        }
    }

    // Module encapsulation
    for module in &result.modules {
        let pct = module.encapsulation_pct;
        if pct < ENCAPSULATION_CRITICAL {
            insights.push(Insight {
                severity: Severity::Critical,
                category: "encapsulation".to_string(),
                file: module.path.clone(),
                message: format!("{}: poor encapsulation ({pct:.1}%)", module.path),
                metric_value: pct,
            });
        } else if pct < ENCAPSULATION_WARNING {
            insights.push(Insight {
                severity: Severity::Warning,
                category: "encapsulation".to_string(),
                file: module.path.clone(),
                message: format!("{}: weak encapsulation ({pct:.1}%)", module.path),
                metric_value: pct,
            });
        }
    }

    // Sort: Critical first (ascending severity), then by metric_value descending
    // within same severity.
    // For stability, lower score = worse. We invert stability metric for descending
    // sort (worst stability is smallest value, so we negate to sort desc).
    insights.sort_by(|a, b| {
        a.severity.cmp(&b.severity).then_with(|| {
            // For severity descending by metric: higher = worse except stability/encapsulation
            // where lower = worse. The metric_value is already stored as the raw metric,
            // so we need category-aware comparison.
            let a_sort = sort_key(a);
            let b_sort = sort_key(b);
            b_sort.total_cmp(&a_sort)
        })
    });

    insights
}

/// Convert an insight's metric_value to a "badness" score for descending sort.
/// Higher badness = worse = should appear first.
fn sort_key(insight: &Insight) -> f64 {
    match insight.category.as_str() {
        // Lower score = worse for these → invert so larger sort key = worse
        "stability" | "encapsulation" => 100.0 - insight.metric_value,
        // Higher value = worse for fix_risk, bus_factor, coupling → use as-is
        _ => insight.metric_value,
    }
}

/// Assemble a compact [`InsightsResult`] from the full heatmap data and computed insights.
pub(crate) fn build_insights_result(
    result: &HeatmapResult,
    insights: Vec<Insight>,
) -> InsightsResult {
    InsightsResult {
        version: 1,
        repository: result.repository.clone(),
        window: result.window.clone(),
        insights,
        top_files: build_compact_files(result),
        flagged_modules: build_flagged_modules(result),
    }
}

/// Condense all files to [`CompactFileEntry`] (5-field summary).
pub(crate) fn build_compact_files(result: &HeatmapResult) -> Vec<CompactFileEntry> {
    result
        .files
        .iter()
        .map(|f| CompactFileEntry {
            path: f.path.clone(),
            stability: f.stability_score,
            churn_commits: f.churn.commits,
            fix_risk_pct: f.fix_risk.combined_pct,
            bus_factor_risk: f.authors.single_owner_risk,
        })
        .collect()
}

/// Collect only modules below the encapsulation warning threshold (60%).
pub(crate) fn build_flagged_modules(result: &HeatmapResult) -> Vec<CompactModuleEntry> {
    result
        .modules
        .iter()
        .filter(|m| m.encapsulation_pct < ENCAPSULATION_WARNING)
        .map(|m| CompactModuleEntry {
            path: m.path.clone(),
            encapsulation_pct: m.encapsulation_pct,
            cross_boundary: m.cross_boundary_commits,
        })
        .collect()
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::cmd::heatmap::types::{
        AuthorMetrics, ChurnMetrics, CouplingEdge, CouplingEntry, FileMetrics, FixRiskMetrics,
        ModuleHealth, WindowInfo,
    };

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn make_window() -> WindowInfo {
        WindowInfo {
            mode: "90d".to_string(),
            since: "2024-10-01".to_string(),
            until: "2025-01-01".to_string(),
            commits_analyzed: 10,
            effective_strategy: None,
        }
    }

    fn make_empty_result() -> HeatmapResult {
        HeatmapResult {
            version: 1,
            generated_at: "2025-01-01T00:00:00Z".to_string(),
            repository: "test-repo".to_string(),
            window: make_window(),
            files: vec![],
            modules: vec![],
            coupling_graph: vec![],
            excluded_patterns: vec![],
            warnings: vec![],
            file_targets: None,
        }
    }

    fn make_file(
        path: &str,
        stability: u8,
        combined_pct: f64,
        insufficient_data: bool,
        single_owner_risk: bool,
        top_author_pct: f64,
        author_count: usize,
        blast_radius: Vec<CouplingEntry>,
    ) -> FileMetrics {
        FileMetrics {
            path: path.to_string(),
            churn: ChurnMetrics {
                commits: 5,
                rate: 0.1,
            },
            stability_score: stability,
            authors: AuthorMetrics {
                count: author_count,
                top_author_pct,
                single_owner_risk,
            },
            fix_risk: FixRiskMetrics {
                keyword_pct: combined_pct / 2.0,
                proximity_pct: combined_pct / 2.0,
                combined_pct,
                insufficient_data,
            },
            blast_radius,
        }
    }

    fn make_module(path: &str, encapsulation_pct: f64) -> ModuleHealth {
        ModuleHealth {
            path: path.to_string(),
            encapsulation_pct,
            files_count: 3,
            total_commits: 10,
            cross_boundary_commits: 3,
        }
    }

    // -----------------------------------------------------------------------
    // Stability threshold tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_empty_result_no_insights() {
        let result = make_empty_result();
        let insights = compute_insights(&result);
        assert!(insights.is_empty(), "empty result should yield no insights");
    }

    #[test]
    fn test_stability_critical_threshold() {
        let mut result = make_empty_result();
        result
            .files
            .push(make_file("a.rs", 39, 0.0, true, false, 50.0, 2, vec![]));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].severity, Severity::Critical);
        assert_eq!(insights[0].category, "stability");
        assert!(
            insights[0].message.contains("critically unstable"),
            "message: {}",
            insights[0].message
        );
    }

    #[test]
    fn test_stability_warning_threshold() {
        let mut result = make_empty_result();
        result
            .files
            .push(make_file("a.rs", 55, 0.0, true, false, 50.0, 2, vec![]));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].severity, Severity::Warning);
        assert_eq!(insights[0].category, "stability");
        assert!(insights[0].message.contains("moderate instability"));
    }

    #[test]
    fn test_stability_above_threshold() {
        let mut result = make_empty_result();
        result
            .files
            .push(make_file("a.rs", 70, 0.0, true, false, 50.0, 2, vec![]));
        let insights = compute_insights(&result);
        // stability=70 is AT the warning threshold, not below — no insight
        assert!(insights.is_empty(), "score=70 should yield no insight");
    }

    // -----------------------------------------------------------------------
    // Fix risk threshold tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_fix_risk_critical() {
        let mut result = make_empty_result();
        result
            .files
            .push(make_file("b.rs", 80, 50.1, false, false, 50.0, 2, vec![]));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].severity, Severity::Critical);
        assert_eq!(insights[0].category, "fix_risk");
        assert!(insights[0].message.contains("high fix-risk"));
    }

    #[test]
    fn test_fix_risk_warning() {
        let mut result = make_empty_result();
        result
            .files
            .push(make_file("b.rs", 80, 35.0, false, false, 50.0, 2, vec![]));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].severity, Severity::Warning);
        assert_eq!(insights[0].category, "fix_risk");
        assert!(insights[0].message.contains("elevated fix-risk"));
    }

    #[test]
    fn test_fix_risk_below_threshold() {
        let mut result = make_empty_result();
        result
            .files
            .push(make_file("b.rs", 80, 30.0, false, false, 50.0, 2, vec![]));
        let insights = compute_insights(&result);
        // combined_pct=30.0 is AT the warning threshold, not above — no insight
        assert!(insights.is_empty(), "combined=30% should yield no insight");
    }

    #[test]
    fn test_fix_risk_insufficient_data_skipped() {
        let mut result = make_empty_result();
        result
            .files
            .push(make_file("b.rs", 80, 80.0, true, false, 50.0, 2, vec![]));
        let insights = compute_insights(&result);
        assert!(
            insights.is_empty(),
            "insufficient_data=true should skip fix-risk insight"
        );
    }

    // -----------------------------------------------------------------------
    // Bus factor tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_bus_factor_warning() {
        let mut result = make_empty_result();
        result
            .files
            .push(make_file("c.rs", 80, 0.0, true, true, 90.0, 1, vec![]));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].severity, Severity::Warning);
        assert_eq!(insights[0].category, "bus_factor");
        assert!(insights[0].message.contains("bus-factor risk"));
    }

    #[test]
    fn test_bus_factor_no_risk() {
        let mut result = make_empty_result();
        result
            .files
            .push(make_file("c.rs", 80, 0.0, true, false, 60.0, 3, vec![]));
        let insights = compute_insights(&result);
        assert!(insights.is_empty(), "no bus-factor risk when false");
    }

    // -----------------------------------------------------------------------
    // Encapsulation tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_encapsulation_critical() {
        let mut result = make_empty_result();
        result.modules.push(make_module("src/", 30.0));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].severity, Severity::Critical);
        assert_eq!(insights[0].category, "encapsulation");
        assert!(insights[0].message.contains("poor encapsulation"));
    }

    #[test]
    fn test_encapsulation_warning() {
        let mut result = make_empty_result();
        result.modules.push(make_module("src/", 50.0));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].severity, Severity::Warning);
        assert_eq!(insights[0].category, "encapsulation");
        assert!(insights[0].message.contains("weak encapsulation"));
    }

    #[test]
    fn test_encapsulation_above_threshold() {
        let mut result = make_empty_result();
        result.modules.push(make_module("src/", 60.0));
        let insights = compute_insights(&result);
        // encapsulation_pct=60.0 is AT the warning threshold, not below — no insight
        assert!(
            insights.is_empty(),
            "60% encapsulation should yield no insight"
        );
    }

    // -----------------------------------------------------------------------
    // Coupling tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_coupling_warning() {
        let mut result = make_empty_result();
        result.files.push(make_file(
            "a.rs",
            80,
            0.0,
            true,
            false,
            50.0,
            2,
            vec![CouplingEntry {
                path: "b.rs".to_string(),
                confidence: 0.85,
                support: 5,
            }],
        ));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].severity, Severity::Warning);
        assert_eq!(insights[0].category, "coupling");
        assert!(insights[0].message.contains("tightly coupled with b.rs"));
    }

    #[test]
    fn test_coupling_below_threshold() {
        let mut result = make_empty_result();
        result.files.push(make_file(
            "a.rs",
            80,
            0.0,
            true,
            false,
            50.0,
            2,
            vec![CouplingEntry {
                path: "b.rs".to_string(),
                confidence: 0.80,
                support: 5,
            }],
        ));
        let insights = compute_insights(&result);
        // confidence=0.80 is AT the threshold (not strictly above) — no insight
        assert!(
            insights.is_empty(),
            "confidence=0.80 should yield no coupling insight"
        );
    }

    // -----------------------------------------------------------------------
    // Sorting tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_sorted_critical_first() {
        let mut result = make_empty_result();
        // Warning first in data
        result
            .files
            .push(make_file("a.rs", 55, 0.0, true, false, 50.0, 2, vec![])); // warning
        result
            .files
            .push(make_file("b.rs", 39, 0.0, true, false, 50.0, 2, vec![])); // critical
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 2);
        assert_eq!(
            insights[0].severity,
            Severity::Critical,
            "critical should be first"
        );
        assert_eq!(
            insights[1].severity,
            Severity::Warning,
            "warning should be second"
        );
    }

    #[test]
    fn test_sorted_by_metric_within_severity() {
        let mut result = make_empty_result();
        // Two criticals: score 39 and score 20 (20 is worse)
        result
            .files
            .push(make_file("a.rs", 39, 0.0, true, false, 50.0, 2, vec![]));
        result
            .files
            .push(make_file("b.rs", 20, 0.0, true, false, 50.0, 2, vec![]));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 2);
        // b.rs (score 20) is worse, should appear first
        assert!(
            insights[0].file.contains("b.rs"),
            "b.rs (score 20) should be first, got: {}",
            insights[0].file
        );
        assert!(
            insights[1].file.contains("a.rs"),
            "a.rs (score 39) should be second, got: {}",
            insights[1].file
        );
    }

    #[test]
    fn test_multiple_categories_same_file() {
        let mut result = make_empty_result();
        // stability critical + fix_risk critical
        result
            .files
            .push(make_file("a.rs", 39, 55.0, false, false, 50.0, 2, vec![]));
        let insights = compute_insights(&result);
        assert!(
            insights.len() >= 2,
            "same file can have multiple insights: stability + fix_risk"
        );
        let categories: Vec<&str> = insights.iter().map(|i| i.category.as_str()).collect();
        assert!(categories.contains(&"stability"));
        assert!(categories.contains(&"fix_risk"));
    }

    // -----------------------------------------------------------------------
    // build_compact_files
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_compact_files() {
        let mut result = make_empty_result();
        result.files.push(make_file(
            "src/main.rs",
            75,
            25.0,
            false,
            true,
            90.0,
            1,
            vec![],
        ));
        let compact = build_compact_files(&result);
        assert_eq!(compact.len(), 1);
        assert_eq!(compact[0].path, "src/main.rs");
        assert_eq!(compact[0].stability, 75);
        assert_eq!(compact[0].churn_commits, 5);
        assert!((compact[0].fix_risk_pct - 25.0).abs() < 1e-9);
        assert!(compact[0].bus_factor_risk);
    }

    // -----------------------------------------------------------------------
    // build_flagged_modules
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_flagged_modules() {
        let mut result = make_empty_result();
        result.modules.push(make_module("src/", 50.0)); // below threshold → flagged
        result.modules.push(make_module("tests/", 65.0)); // above threshold → not flagged
        let flagged = build_flagged_modules(&result);
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0].path, "src/");
    }

    // -----------------------------------------------------------------------
    // build_insights_result
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_insights_result_structure() {
        let mut result = make_empty_result();
        result
            .files
            .push(make_file("a.rs", 39, 0.0, true, false, 50.0, 2, vec![]));
        result.modules.push(make_module("src/", 50.0));

        let insights = compute_insights(&result);
        let ir = build_insights_result(&result, insights);

        assert_eq!(ir.version, 1);
        assert_eq!(ir.repository, "test-repo");
        assert!(!ir.insights.is_empty());
        assert!(!ir.top_files.is_empty());
        assert!(!ir.flagged_modules.is_empty());
    }
}
