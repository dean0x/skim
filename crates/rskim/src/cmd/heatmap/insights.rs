//! Ranked insight engine for `skim heatmap --insights`.
//!
//! Pure functions, no I/O. [`compute_insights`] scans a [`HeatmapResult`]
//! against the thresholds and gates of `noise.rs` and gives every finding in
//! one rank order. [`build_insights_result`] keeps the findings up to the cap
//! and builds the compact file and module lists from the shown findings only.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use super::noise::{
    self, COUPLING_WARNING, ENCAPSULATION_CRITICAL, ENCAPSULATION_WARNING, FIX_RISK_CRITICAL,
    FIX_RISK_WARNING, MIN_FILE_SUPPORT, MIN_PAIR_SUPPORT, MODULE_MIN_COMMITS, STABILITY_CRITICAL,
    STABILITY_WARNING,
};
use super::types::{
    CompactFileEntry, CompactModuleEntry, Count, FileMetrics, HeatmapResult, Insight,
    InsightCategory, InsightCounts, InsightsResult, ModuleHealth, Severity,
};
use super::view::{JSON_VERSION, repo_name};

// ============================================================================
// Internal helpers
// ============================================================================

/// The parts of a finding that do not depend on its severity.
struct Finding<'a> {
    category: InsightCategory,
    file: &'a str,
    metric_value: f64,
    support: usize,
    score: f64,
}

impl Finding<'_> {
    fn insight(self, severity: Severity, message: String) -> Insight {
        Insight {
            severity,
            category: self.category,
            file: self.file.to_string(),
            message,
            metric_value: self.metric_value,
            support: self.support,
            score: self.score,
        }
    }
}

/// The severity of a value against two bounds: critical past the critical
/// bound, warning past the warning bound, else no finding.
///
/// For metrics where **lower** is worse (stability, encapsulation), pass
/// `value < CRITICAL_BOUND` and `value < WARNING_BOUND`. For metrics where
/// **higher** is worse (fix risk), pass `value > BOUND`.
fn grade(past_critical: bool, past_warning: bool) -> Option<Severity> {
    if past_critical {
        Some(Severity::Critical)
    } else if past_warning {
        Some(Severity::Warning)
    } else {
        None
    }
}

/// `(100 - pct) / 100`, kept in `0.0..=1.0`.
fn inverse_share(pct: f64) -> f64 {
    ((100.0 - pct) / 100.0).clamp(0.0, 1.0)
}

// ============================================================================
// Public API
// ============================================================================

/// Every finding of `result`, in rank order.
///
/// A finding needs the support minimum of its kind: 5 commits for the file
/// kinds (stability, fix risk, bus factor), 3 shared commits for coupling and
/// 5 commits for a module. Fix risk also needs the lift gate and keeps its
/// severity on `combined_pct`; bus factor needs the bus-factor gate. Coupling
/// reads the canonical coupling graph: one finding per code-to-code pair.
///
/// Rank order: severity (critical first), then score desc, then support
/// desc, then path asc. The category and the message break the last ties,
/// so the order is total and two runs give the same findings.
pub(crate) fn compute_insights(result: &HeatmapResult) -> Vec<Insight> {
    let base_rate = noise::base_rate(result.fix_commits_total, result.window.commits_analyzed);
    let mut insights: Vec<Insight> = Vec::new();

    for file in result
        .files
        .iter()
        .filter(|f| f.churn.commits >= MIN_FILE_SUPPORT)
    {
        push_file_findings(&mut insights, file, base_rate, result.window_authors);
    }

    for edge in &result.coupling_graph {
        let code_pair = !noise::is_doc_path(&edge.a) && !noise::is_doc_path(&edge.b);
        if !code_pair || edge.support < MIN_PAIR_SUPPORT || edge.confidence <= COUPLING_WARNING {
            continue;
        }
        let conf_pct = edge.confidence * 100.0;
        let finding = Finding {
            category: InsightCategory::Coupling,
            file: &edge.a,
            metric_value: edge.confidence,
            support: edge.support,
            score: edge.confidence,
        };
        insights.push(finding.insight(
            Severity::Warning,
            format!(
                "tightly coupled with {} ({conf_pct:.1}% confidence, {} co-changes)",
                edge.b, edge.support
            ),
        ));
    }

    for module in result
        .modules
        .iter()
        .filter(|m| m.total_commits >= MODULE_MIN_COMMITS)
    {
        let pct = module.encapsulation_pct;
        let commits = module.total_commits;
        let Some(severity) = grade(pct < ENCAPSULATION_CRITICAL, pct < ENCAPSULATION_WARNING)
        else {
            continue;
        };
        let level = match severity {
            Severity::Critical => "poor",
            Severity::Warning => "weak",
        };
        let finding = Finding {
            category: InsightCategory::Encapsulation,
            file: &module.path,
            metric_value: pct,
            support: commits,
            score: inverse_share(pct),
        };
        insights.push(finding.insight(
            severity,
            format!("{level} encapsulation ({pct:.1}%, {commits} commits)"),
        ));
    }

    insights.sort_by(insight_order);
    insights
}

/// The stability, fix-risk and bus-factor findings of one file at or above
/// the file support minimum.
fn push_file_findings(
    insights: &mut Vec<Insight>,
    file: &FileMetrics,
    base_rate: f64,
    window_authors: usize,
) {
    let support = file.churn.commits;
    let path = file.path.as_str();

    // Stability (lower score = worse)
    let stability = file.stability_score;
    if let Some(severity) = grade(
        stability < STABILITY_CRITICAL,
        stability < STABILITY_WARNING,
    ) {
        let level = match severity {
            Severity::Critical => "critically unstable",
            Severity::Warning => "moderate instability",
        };
        let finding = Finding {
            category: InsightCategory::Stability,
            file: path,
            metric_value: f64::from(stability),
            support,
            score: inverse_share(f64::from(stability)),
        };
        insights.push(finding.insight(
            severity,
            format!("{level} (score {stability}/100, {support} commits)"),
        ));
    }

    // Fix risk: the lift gate, then the severity on combined_pct
    let fix = &file.fix_risk;
    if noise::passes_lift(fix.fix_commits, support, base_rate) {
        let combined = fix.combined_pct;
        if let Some(severity) = grade(combined > FIX_RISK_CRITICAL, combined > FIX_RISK_WARNING) {
            let level = match severity {
                Severity::Critical => "high",
                Severity::Warning => "elevated",
            };
            let finding = Finding {
                category: InsightCategory::FixRisk,
                file: path,
                metric_value: combined,
                support,
                score: noise::wilson_lower_bound(fix.fix_commits, support),
            };
            insights.push(finding.insight(
                severity,
                format!(
                    "{level} fix-risk ({combined:.1}% combined, {}/{support} fix commits)",
                    fix.fix_commits
                ),
            ));
        }
    }

    // Bus factor (always a warning when the gate passes)
    let pct = file.authors.top_author_pct;
    if noise::passes_bus_gate(window_authors, support, pct) {
        let count = file.authors.count;
        let finding = Finding {
            category: InsightCategory::BusFactor,
            file: path,
            metric_value: pct,
            support,
            score: (pct / 100.0).clamp(0.0, 1.0),
        };
        insights.push(finding.insight(
            Severity::Warning,
            format!("bus-factor risk ({pct:.1}% top author, {count} author(s), {support} commits)"),
        ));
    }
}

/// The insight rank order (see [`compute_insights`]).
fn insight_order(x: &Insight, y: &Insight) -> Ordering {
    x.severity
        .cmp(&y.severity)
        .then_with(|| y.score.total_cmp(&x.score))
        .then_with(|| y.support.cmp(&x.support))
        .then_with(|| x.file.cmp(&y.file))
        .then_with(|| x.category.cmp(&y.category))
        .then_with(|| x.message.cmp(&y.message))
}

/// The `--insights` result: the first `cap` findings of `ranked` (already in
/// rank order), their count, the files and modules of those findings, and
/// every warning of the run.
pub(crate) fn build_insights_result(
    result: &HeatmapResult,
    mut ranked: Vec<Insight>,
    cap: usize,
) -> InsightsResult {
    let total = ranked.len();
    ranked.truncate(cap);
    InsightsResult {
        version: JSON_VERSION,
        repository: repo_name(&result.repository).to_string(),
        window: result.window.clone(),
        counts: InsightCounts {
            insights: Count {
                total,
                shown: ranked.len(),
            },
        },
        top_files: build_compact_files(result, &ranked),
        flagged_modules: build_flagged_modules(result, &ranked),
        insights: ranked,
        warnings: result.warnings.clone(),
    }
}

/// The files of the shown findings, once each, in finding order.
fn build_compact_files(result: &HeatmapResult, shown: &[Insight]) -> Vec<CompactFileEntry> {
    let by_path: HashMap<&str, &FileMetrics> =
        result.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    shown
        .iter()
        .filter(|i| i.category != InsightCategory::Encapsulation)
        .filter_map(|i| by_path.get(i.file.as_str()).copied())
        .filter(|f| seen.insert(f.path.as_str()))
        .map(|f| CompactFileEntry {
            path: f.path.clone(),
            stability: f.stability_score,
            churn_commits: f.churn.commits,
            fix_risk_pct: f.fix_risk.combined_pct,
            bus_factor_risk: noise::passes_bus_gate(
                result.window_authors,
                f.churn.commits,
                f.authors.top_author_pct,
            ),
        })
        .collect()
}

/// The modules of the shown encapsulation findings, in finding order.
fn build_flagged_modules(result: &HeatmapResult, shown: &[Insight]) -> Vec<CompactModuleEntry> {
    let by_path: HashMap<&str, &ModuleHealth> = result
        .modules
        .iter()
        .map(|m| (m.path.as_str(), m))
        .collect();
    shown
        .iter()
        .filter(|i| i.category == InsightCategory::Encapsulation)
        .filter_map(|i| by_path.get(i.file.as_str()).copied())
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::cmd::heatmap::types::{
        AuthorMetrics, ChurnMetrics, CouplingEdge, FixRiskMetrics, WindowInfo,
    };

    const EPS: f64 = 1e-9;

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

    /// No files or modules; 10 commits, 2 of them fix commits (base rate
    /// 20%), 2 distinct authors.
    fn make_empty_result() -> HeatmapResult {
        HeatmapResult {
            generated_at: "2025-01-01T00:00:00Z".to_string(),
            repository: "test-repo".to_string(),
            window: make_window(),
            files: vec![],
            modules: vec![],
            coupling_graph: vec![],
            excluded_patterns: vec![],
            warnings: vec![],
            file_targets: None,
            fix_commits_total: 2,
            window_authors: 2,
        }
    }

    /// Params struct for `make_file` — named fields instead of a long
    /// positional signature.
    struct FileSpec<'a> {
        path: &'a str,
        commits: usize,
        stability: u8,
        combined_pct: f64,
        fix_commits: usize,
        top_author_pct: f64,
        author_count: usize,
    }

    // The defaults pass no insight threshold, so a test sets only the fields
    // that it checks.
    impl Default for FileSpec<'_> {
        fn default() -> Self {
            Self {
                path: "",
                commits: MIN_FILE_SUPPORT,
                stability: 80,
                combined_pct: 0.0,
                fix_commits: 0,
                top_author_pct: 50.0,
                author_count: 2,
            }
        }
    }

    fn make_file(spec: FileSpec<'_>) -> FileMetrics {
        FileMetrics {
            path: spec.path.to_string(),
            churn: ChurnMetrics {
                commits: spec.commits,
                rate: 0.1,
            },
            stability_score: spec.stability,
            authors: AuthorMetrics {
                count: spec.author_count,
                top_author_pct: spec.top_author_pct,
                single_owner_risk: spec.top_author_pct > 80.0,
            },
            fix_risk: FixRiskMetrics {
                keyword_pct: spec.combined_pct / 2.0,
                proximity_pct: spec.combined_pct / 2.0,
                combined_pct: spec.combined_pct,
                fix_commits: spec.fix_commits,
                insufficient_data: spec.commits < MIN_FILE_SUPPORT,
            },
            blast_radius: vec![],
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

    fn make_edge(a: &str, b: &str, confidence: f64, support: usize) -> CouplingEdge {
        CouplingEdge {
            a: a.to_string(),
            b: b.to_string(),
            confidence,
            support,
        }
    }

    /// `(category, file)` of each insight, in order.
    fn kinds(insights: &[Insight]) -> Vec<(InsightCategory, &str)> {
        insights
            .iter()
            .map(|i| (i.category, i.file.as_str()))
            .collect()
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
        result.files.push(make_file(FileSpec {
            path: "a.rs",
            stability: 39,
            ..Default::default()
        }));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].severity, Severity::Critical);
        assert_eq!(insights[0].category, InsightCategory::Stability);
        // The message gives the number and the support, not the path (the
        // rendering layer adds the path).
        assert_eq!(
            insights[0].message,
            "critically unstable (score 39/100, 5 commits)"
        );
        assert_eq!(insights[0].support, 5);
        assert!((insights[0].score - 0.61).abs() < EPS);
    }

    #[test]
    fn test_stability_warning_threshold() {
        let mut result = make_empty_result();
        result.files.push(make_file(FileSpec {
            path: "a.rs",
            stability: 55,
            ..Default::default()
        }));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].severity, Severity::Warning);
        assert_eq!(insights[0].category, InsightCategory::Stability);
        assert!(insights[0].message.contains("moderate instability"));
    }

    #[test]
    fn test_stability_above_threshold() {
        let mut result = make_empty_result();
        result.files.push(make_file(FileSpec {
            path: "a.rs",
            stability: 70,
            ..Default::default()
        }));
        let insights = compute_insights(&result);
        // stability=70 is AT the warning threshold, not below — no insight
        assert!(insights.is_empty(), "score=70 should yield no insight");
    }

    // -----------------------------------------------------------------------
    // Fix risk tests: lift gate, then severity on combined_pct
    // -----------------------------------------------------------------------

    #[test]
    fn test_fix_risk_critical() {
        let mut result = make_empty_result();
        result.files.push(make_file(FileSpec {
            path: "b.rs",
            combined_pct: 50.1,
            fix_commits: 5,
            ..Default::default()
        }));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].severity, Severity::Critical);
        assert_eq!(insights[0].category, InsightCategory::FixRisk);
        assert_eq!(
            insights[0].message,
            "high fix-risk (50.1% combined, 5/5 fix commits)"
        );
        // Score = Wilson lower bound of 5 of 5.
        assert!((insights[0].score - noise::wilson_lower_bound(5, 5)).abs() < EPS);
    }

    #[test]
    fn test_fix_risk_warning() {
        let mut result = make_empty_result();
        result.files.push(make_file(FileSpec {
            path: "b.rs",
            combined_pct: 35.0,
            fix_commits: 5,
            ..Default::default()
        }));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].severity, Severity::Warning);
        assert_eq!(insights[0].category, InsightCategory::FixRisk);
        assert!(insights[0].message.contains("elevated fix-risk"));
    }

    #[test]
    fn test_fix_risk_below_threshold() {
        let mut result = make_empty_result();
        result.files.push(make_file(FileSpec {
            path: "b.rs",
            combined_pct: 30.0,
            fix_commits: 5,
            ..Default::default()
        }));
        let insights = compute_insights(&result);
        // combined_pct=30.0 is AT the warning threshold, not above — no insight
        assert!(insights.is_empty(), "combined=30% should yield no insight");
    }

    #[test]
    fn test_fix_risk_needs_the_lift_gate() {
        let mut result = make_empty_result();
        // 1 fix commit of 5: the Wilson lower bound (0.04) is below the 20%
        // base rate, so a high combined_pct alone gives no finding.
        result.files.push(make_file(FileSpec {
            path: "b.rs",
            combined_pct: 80.0,
            fix_commits: 1,
            ..Default::default()
        }));
        assert!(compute_insights(&result).is_empty());
    }

    #[test]
    fn test_fix_risk_has_no_finding_at_a_degenerate_base_rate() {
        let mut result = make_empty_result();
        result.fix_commits_total = 0;
        result.files.push(make_file(FileSpec {
            path: "b.rs",
            combined_pct: 80.0,
            fix_commits: 5,
            ..Default::default()
        }));
        assert!(compute_insights(&result).is_empty());
    }

    // -----------------------------------------------------------------------
    // Bus factor tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_bus_factor_warning() {
        let mut result = make_empty_result();
        result.files.push(make_file(FileSpec {
            path: "c.rs",
            top_author_pct: 90.0,
            author_count: 2,
            ..Default::default()
        }));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].severity, Severity::Warning);
        assert_eq!(insights[0].category, InsightCategory::BusFactor);
        assert_eq!(
            insights[0].message,
            "bus-factor risk (90.0% top author, 2 author(s), 5 commits)"
        );
        assert!((insights[0].score - 0.9).abs() < EPS);
    }

    #[test]
    fn test_bus_factor_no_risk() {
        let mut result = make_empty_result();
        result.files.push(make_file(FileSpec {
            path: "c.rs",
            top_author_pct: 60.0,
            author_count: 3,
            ..Default::default()
        }));
        let insights = compute_insights(&result);
        assert!(insights.is_empty(), "no bus-factor risk at a 60% share");
    }

    #[test]
    fn test_bus_factor_has_no_finding_in_a_one_author_window() {
        let mut result = make_empty_result();
        result.window_authors = 1;
        result.files.push(make_file(FileSpec {
            path: "c.rs",
            top_author_pct: 100.0,
            author_count: 1,
            ..Default::default()
        }));
        assert!(compute_insights(&result).is_empty());
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
        assert_eq!(insights[0].category, InsightCategory::Encapsulation);
        assert_eq!(
            insights[0].message,
            "poor encapsulation (30.0%, 10 commits)"
        );
        assert!((insights[0].score - 0.7).abs() < EPS);
    }

    #[test]
    fn test_encapsulation_warning() {
        let mut result = make_empty_result();
        result.modules.push(make_module("src/", 50.0));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].severity, Severity::Warning);
        assert_eq!(insights[0].category, InsightCategory::Encapsulation);
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
    // Coupling tests: canonical graph edges, code to code
    // -----------------------------------------------------------------------

    #[test]
    fn test_coupling_warning() {
        let mut result = make_empty_result();
        result
            .coupling_graph
            .push(make_edge("a.rs", "b.rs", 0.85, 5));
        let insights = compute_insights(&result);
        assert_eq!(insights.len(), 1);
        assert_eq!(insights[0].severity, Severity::Warning);
        assert_eq!(insights[0].category, InsightCategory::Coupling);
        assert_eq!(insights[0].file, "a.rs");
        assert_eq!(
            insights[0].message,
            "tightly coupled with b.rs (85.0% confidence, 5 co-changes)"
        );
        assert_eq!(insights[0].support, 5);
    }

    #[test]
    fn test_coupling_below_threshold() {
        let mut result = make_empty_result();
        result
            .coupling_graph
            .push(make_edge("a.rs", "b.rs", 0.80, 5));
        let insights = compute_insights(&result);
        // confidence=0.80 is AT the threshold (not strictly above) — no insight
        assert!(
            insights.is_empty(),
            "confidence=0.80 should yield no coupling insight"
        );
    }

    #[test]
    fn test_coupling_skips_a_pair_with_a_doc_endpoint() {
        let mut result = make_empty_result();
        result
            .coupling_graph
            .push(make_edge("README.md", "src/lib.rs", 0.95, 9));
        result
            .coupling_graph
            .push(make_edge("docs/guide.rst", "src/lib.rs", 0.95, 9));
        assert!(compute_insights(&result).is_empty());
    }

    // -----------------------------------------------------------------------
    // Support minimums
    // -----------------------------------------------------------------------

    /// One file, one pair and one module that are bad on every kind, at
    /// `file_commits`, `pair_support` and `module_commits`.
    fn bad_on_every_kind(
        file_commits: usize,
        pair_support: usize,
        module_commits: usize,
    ) -> HeatmapResult {
        let mut result = make_empty_result();
        result.window.commits_analyzed = 100;
        result.fix_commits_total = 10;
        result.files.push(make_file(FileSpec {
            path: "src/bad.rs",
            commits: file_commits,
            stability: 10,
            combined_pct: 100.0,
            fix_commits: file_commits,
            top_author_pct: 100.0,
            author_count: 1,
        }));
        result
            .coupling_graph
            .push(make_edge("src/x.rs", "src/y.rs", 1.0, pair_support));
        let mut module = make_module("src", 0.0);
        module.total_commits = module_commits;
        result.modules.push(module);
        result
    }

    #[test]
    fn test_no_finding_is_below_its_support_minimum() {
        let below = bad_on_every_kind(
            MIN_FILE_SUPPORT - 1,
            MIN_PAIR_SUPPORT - 1,
            MODULE_MIN_COMMITS - 1,
        );
        let insights = compute_insights(&below);
        assert!(
            insights.is_empty(),
            "no finding below support, so no CRITICAL either: {:?}",
            kinds(&insights)
        );

        let at = bad_on_every_kind(MIN_FILE_SUPPORT, MIN_PAIR_SUPPORT, MODULE_MIN_COMMITS);
        let mut found: Vec<InsightCategory> =
            compute_insights(&at).iter().map(|i| i.category).collect();
        found.sort();
        assert_eq!(
            found,
            [
                InsightCategory::Stability,
                InsightCategory::FixRisk,
                InsightCategory::BusFactor,
                InsightCategory::Coupling,
                InsightCategory::Encapsulation,
            ]
        );
    }

    #[test]
    fn test_module_finding_needs_five_commits_of_support() {
        let module_at = |commits: usize| {
            let mut result = make_empty_result();
            let mut module = make_module("src", 0.0);
            module.total_commits = commits;
            result.modules.push(module);
            compute_insights(&result)
        };

        assert!(module_at(4).is_empty(), "4 commits give no module finding");
        let at_five = module_at(5);
        assert_eq!(at_five.len(), 1);
        assert_eq!(at_five[0].category, InsightCategory::Encapsulation);
        assert_eq!(at_five[0].support, 5);
    }

    // -----------------------------------------------------------------------
    // Rank order
    // -----------------------------------------------------------------------

    #[test]
    fn test_insights_rank_severity_then_score_then_support_then_path() {
        let mut result = make_empty_result();
        // Critical stability, score 0.8, support 5.
        result.files.push(make_file(FileSpec {
            path: "b.rs",
            stability: 20,
            ..Default::default()
        }));
        // Critical fix risk, score = Wilson bound of 5 of 5 (0.57).
        result.files.push(make_file(FileSpec {
            path: "c.rs",
            combined_pct: 60.0,
            fix_commits: 5,
            ..Default::default()
        }));
        // Warning bus factor, score 0.9, support 5.
        result.files.push(make_file(FileSpec {
            path: "f.rs",
            top_author_pct: 90.0,
            ..Default::default()
        }));
        // Warning stability, score 0.45.
        result.files.push(make_file(FileSpec {
            path: "g.rs",
            stability: 55,
            ..Default::default()
        }));
        // Warning stability, score 0.45: the same score and support as g.rs.
        result.files.push(make_file(FileSpec {
            path: "e.rs",
            stability: 55,
            ..Default::default()
        }));
        // Warning coupling, score 0.9, support 3.
        result
            .coupling_graph
            .push(make_edge("d.rs", "z.rs", 0.9, 3));
        // Critical encapsulation, score 0.8, support 10.
        let mut module = make_module("a", 20.0);
        module.total_commits = 10;
        result.modules.push(module);

        let insights = compute_insights(&result);
        assert_eq!(
            kinds(&insights),
            [
                (InsightCategory::Encapsulation, "a"),
                (InsightCategory::Stability, "b.rs"),
                (InsightCategory::FixRisk, "c.rs"),
                (InsightCategory::BusFactor, "f.rs"),
                (InsightCategory::Coupling, "d.rs"),
                (InsightCategory::Stability, "e.rs"),
                (InsightCategory::Stability, "g.rs"),
            ]
        );
    }

    #[test]
    fn test_multiple_categories_same_file() {
        let mut result = make_empty_result();
        // stability critical + fix_risk critical
        result.files.push(make_file(FileSpec {
            path: "a.rs",
            stability: 39,
            combined_pct: 55.0,
            fix_commits: 5,
            ..Default::default()
        }));
        let insights = compute_insights(&result);
        let categories: Vec<InsightCategory> = insights.iter().map(|i| i.category).collect();
        assert_eq!(
            categories,
            [InsightCategory::Stability, InsightCategory::FixRisk]
        );
    }

    // -----------------------------------------------------------------------
    // build_insights_result: the cap and the shown lists
    // -----------------------------------------------------------------------

    /// Three critical stability findings (a.rs, b.rs, c.rs in rank order)
    /// and one warning encapsulation finding (module `m`).
    fn four_findings() -> HeatmapResult {
        let mut result = make_empty_result();
        for (path, stability) in [("c.rs", 30), ("a.rs", 10), ("b.rs", 20)] {
            result.files.push(make_file(FileSpec {
                path,
                stability,
                ..Default::default()
            }));
        }
        result.files.push(make_file(FileSpec {
            path: "clean.rs",
            ..Default::default()
        }));
        result.modules.push(make_module("m", 50.0));
        result
    }

    #[test]
    fn test_build_insights_result_gives_the_repository_name_not_the_path() {
        let mut result = four_findings();
        result.repository = "/home/dev/work/test-repo".to_string();
        let ir = build_insights_result(&result, compute_insights(&result), 20);
        assert_eq!(ir.repository, "test-repo");
    }

    #[test]
    fn test_build_insights_result_keeps_every_run_warning() {
        let mut result = four_findings();
        result.warnings = vec!["first".into(), "second".into(), "third".into()];
        let ir = build_insights_result(&result, compute_insights(&result), 1);
        assert_eq!(ir.warnings, ["first", "second", "third"]);
    }

    #[test]
    fn test_build_insights_result_caps_findings_and_lists_only_shown_files() {
        let result = four_findings();
        let ir = build_insights_result(&result, compute_insights(&result), 2);
        assert_eq!(ir.version, 2);
        assert_eq!(ir.counts.insights, Count { total: 4, shown: 2 });
        assert_eq!(
            kinds(&ir.insights),
            [
                (InsightCategory::Stability, "a.rs"),
                (InsightCategory::Stability, "b.rs"),
            ]
        );
        let files: Vec<&str> = ir.top_files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(files, ["a.rs", "b.rs"]);
        assert!(
            ir.flagged_modules.is_empty(),
            "the module finding is not shown"
        );
    }

    #[test]
    fn test_build_insights_result_flags_the_modules_of_shown_findings() {
        let result = four_findings();
        let ir = build_insights_result(&result, compute_insights(&result), 20);
        assert_eq!(ir.counts.insights, Count { total: 4, shown: 4 });
        let modules: Vec<&str> = ir.flagged_modules.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(modules, ["m"]);
        let files: Vec<&str> = ir.top_files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(files, ["a.rs", "b.rs", "c.rs"], "clean.rs has no finding");
    }

    #[test]
    fn test_build_compact_files_fields() {
        let mut result = make_empty_result();
        result.files.push(make_file(FileSpec {
            path: "src/main.rs",
            stability: 39,
            combined_pct: 25.0,
            top_author_pct: 90.0,
            author_count: 2,
            ..Default::default()
        }));
        let ir = build_insights_result(&result, compute_insights(&result), 20);
        assert_eq!(ir.top_files.len(), 1, "one entry for two findings");
        let entry = &ir.top_files[0];
        assert_eq!(entry.path, "src/main.rs");
        assert_eq!(entry.stability, 39);
        assert_eq!(entry.churn_commits, 5);
        assert!((entry.fix_risk_pct - 25.0).abs() < EPS);
        assert!(entry.bus_factor_risk);
    }

    #[test]
    fn test_build_compact_files_bus_factor_risk_needs_the_bus_gate() {
        let mut result = make_empty_result();
        result.window_authors = 1;
        result.files.push(make_file(FileSpec {
            path: "src/main.rs",
            stability: 39,
            top_author_pct: 100.0,
            author_count: 1,
            ..Default::default()
        }));
        let ir = build_insights_result(&result, compute_insights(&result), 20);
        assert_eq!(ir.top_files.len(), 1);
        assert!(
            !ir.top_files[0].bus_factor_risk,
            "a one-author window has no bus-factor risk"
        );
    }
}
