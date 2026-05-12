//! Rendering layer for `skim heatmap` output.
//!
//! Two formats: JSON (via serde_json) and human-readable text (via colored).
//!
//! Color handling: the `colored` crate automatically respects `NO_COLOR`,
//! `TERM=dumb`, and non-TTY output. No manual `NO_COLOR` detection is needed.

use colored::Colorize;

use super::types::{HeatmapResult, Insight, InsightsResult, Severity};

// ============================================================================
// JSON output
// ============================================================================

/// Render the heatmap result as pretty-printed JSON.
pub(crate) fn render_json(result: &HeatmapResult) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(result)?)
}

// ============================================================================
// Text output
// ============================================================================

/// Render the heatmap result as a human-readable text report.
///
/// Color application is unconditional: the `colored` crate strips ANSI codes
/// automatically when `NO_COLOR` is set or stdout is not a TTY.
pub(crate) fn render_text(result: &HeatmapResult, top_n: usize) -> String {
    let mut out = String::new();

    // Header
    let scope_suffix = match &result.file_targets {
        Some(targets) => format!(" (scoped to {} files)", targets.len()),
        None => String::new(),
    };
    let header = format!(
        "─── Heatmap: {} ({} commits, {} window){} ───",
        result.repository, result.window.commits_analyzed, result.window.mode, scope_suffix
    );
    out.push_str(&header.bold().to_string());
    out.push('\n');

    if !result.warnings.is_empty() {
        for w in &result.warnings {
            out.push_str(&format!("  ⚠  {w}\n"));
        }
    }
    out.push('\n');

    render_top_churn(&mut out, result, top_n);
    render_blast_radius(&mut out, result, top_n);
    render_fix_risk(&mut out, result, top_n);
    render_module_health(&mut out, result, top_n);
    render_bus_factor(&mut out, result, top_n);

    out
}

fn render_top_churn(out: &mut String, result: &HeatmapResult, top_n: usize) {
    section_header(out, "Top Churn:");
    let mut files_by_churn: Vec<_> = result.files.iter().collect();
    files_by_churn.sort_by_key(|f| std::cmp::Reverse(f.churn.commits));

    if files_by_churn.is_empty() {
        out.push_str("  (no files)\n");
    } else {
        for fm in files_by_churn.iter().take(top_n) {
            let rate_pct = fm.churn.rate * 100.0;
            let stability = fm.stability_score;
            let churn_str = format!("{:4}", fm.churn.commits).yellow().to_string();
            let rate_str = format!("{rate_pct:5.1}%");
            let stab_str = if stability < 40 {
                format!("{stability:3}").red().to_string()
            } else if stability < 70 {
                format!("{stability:3}").yellow().to_string()
            } else {
                format!("{stability:3}").green().to_string()
            };
            out.push_str(&format!(
                "  {churn_str} commits  {rate_str} rate  stability {stab_str}  {}\n",
                fm.path
            ));
        }
    }
    out.push('\n');
}

fn render_blast_radius(out: &mut String, result: &HeatmapResult, top_n: usize) {
    section_header(out, "Blast Radius (coupling above threshold):");
    let mut files_with_coupling: Vec<_> = result
        .files
        .iter()
        .filter(|f| !f.blast_radius.is_empty())
        .collect();
    files_with_coupling.sort_by(|a, b| {
        let a_conf = a.blast_radius.first().map(|e| e.confidence).unwrap_or(0.0);
        let b_conf = b.blast_radius.first().map(|e| e.confidence).unwrap_or(0.0);
        b_conf.total_cmp(&a_conf)
    });

    if files_with_coupling.is_empty() {
        out.push_str("  (no coupling above threshold)\n");
    } else {
        for fm in files_with_coupling.iter().take(top_n) {
            out.push_str(&format!("  {}\n", fm.path));
            for entry in fm.blast_radius.iter().take(5) {
                let conf_pct = entry.confidence * 100.0;
                let conf_str = format!("{conf_pct:5.1}%").cyan().to_string();
                out.push_str(&format!(
                    "    → {} ({conf_str} confidence, {} support)\n",
                    entry.path, entry.support
                ));
            }
        }
    }
    out.push('\n');
}

fn render_fix_risk(out: &mut String, result: &HeatmapResult, top_n: usize) {
    section_header(out, "Fix Risk (> 20%):");
    let mut fix_risk_files: Vec<_> = result
        .files
        .iter()
        .filter(|f| f.fix_risk.combined_pct > 20.0 && !f.fix_risk.insufficient_data)
        .collect();
    fix_risk_files.sort_by(|a, b| b.fix_risk.combined_pct.total_cmp(&a.fix_risk.combined_pct));

    if fix_risk_files.is_empty() {
        out.push_str("  (no files above threshold)\n");
    } else {
        for fm in fix_risk_files.iter().take(top_n) {
            let combined = fm.fix_risk.combined_pct;
            let kw = fm.fix_risk.keyword_pct;
            let prox = fm.fix_risk.proximity_pct;
            let combined_str = format!("{combined:5.1}%").red().to_string();
            out.push_str(&format!(
                "  {} combined={combined_str} keyword={kw:5.1}% proximity={prox:5.1}%\n",
                fm.path
            ));
        }
    }
    out.push('\n');
}

fn render_module_health(out: &mut String, result: &HeatmapResult, top_n: usize) {
    section_header(out, "Module Health:");
    if result.modules.is_empty() {
        out.push_str("  (no modules with enough data)\n");
    } else {
        for module in result.modules.iter().take(top_n) {
            let pct = module.encapsulation_pct;
            let pct_str = if pct < 50.0 {
                format!("{pct:5.1}%").red().to_string()
            } else if pct < 75.0 {
                format!("{pct:5.1}%").yellow().to_string()
            } else {
                format!("{pct:5.1}%").green().to_string()
            };
            out.push_str(&format!(
                "  {} encapsulation={pct_str} files={} commits={} cross={}\n",
                module.path,
                module.files_count,
                module.total_commits,
                module.cross_boundary_commits
            ));
        }
    }
    out.push('\n');
}

fn render_bus_factor(out: &mut String, result: &HeatmapResult, top_n: usize) {
    section_header(out, "Bus Factor Risk:");
    let bus_factor_files: Vec<_> = result
        .files
        .iter()
        .filter(|f| f.authors.single_owner_risk)
        .collect();

    if bus_factor_files.is_empty() {
        out.push_str("  (no single-owner risk detected)\n");
    } else {
        for fm in bus_factor_files.iter().take(top_n) {
            let pct = fm.authors.top_author_pct;
            let pct_str = format!("{pct:5.1}%").red().to_string();
            out.push_str(&format!(
                "  {} top-author={pct_str} authors={}\n",
                fm.path, fm.authors.count
            ));
        }
    }
    out.push('\n');
}

fn section_header(out: &mut String, title: &str) {
    out.push_str(&title.bold().underline().to_string());
    out.push('\n');
}

// ============================================================================
// Insights rendering
// ============================================================================

/// Total width of the insights header separator line.
const HEADER_WIDTH: usize = 76;

/// Render `--insights` text output: a filtered list of one-liner findings.
///
/// Color application is unconditional — the `colored` crate strips ANSI codes
/// when `NO_COLOR` is set or stdout is not a TTY.
pub(crate) fn render_insights_text(insights: &[Insight]) -> String {
    let mut out = String::new();

    // Header line: "── Insights ─────────────────────────────────────────────────────────────"
    let label = " Insights ";
    let prefix = "──";
    let suffix_len = HEADER_WIDTH.saturating_sub(prefix.len() + label.len());
    let suffix = "─".repeat(suffix_len);
    let header = format!("{prefix}{label}{suffix}");
    out.push_str(&header);
    out.push('\n');
    out.push('\n'); // blank line after header

    if insights.is_empty() {
        out.push_str("  (no notable findings)\n");
        return out;
    }

    for insight in insights {
        let severity_str = match insight.severity {
            Severity::Critical => "CRITICAL ".red().bold().to_string(),
            Severity::Warning => "WARNING  ".yellow().bold().to_string(),
        };
        out.push_str(&format!("  {severity_str}  {}\n", insight.message));
    }

    out
}

/// Render `--insights --json` output as pretty-printed JSON.
pub(crate) fn render_insights_json(insights_result: &InsightsResult) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(insights_result)?)
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

    fn make_result() -> HeatmapResult {
        HeatmapResult {
            version: 1,
            generated_at: "2025-01-01T00:00:00Z".to_string(),
            repository: "test-repo".to_string(),
            window: WindowInfo {
                mode: "90d".to_string(),
                since: "2024-10-01".to_string(),
                until: "2025-01-01".to_string(),
                commits_analyzed: 10,
                effective_strategy: None,
            },
            files: vec![FileMetrics {
                path: "src/main.rs".to_string(),
                churn: ChurnMetrics {
                    commits: 5,
                    rate: 0.5,
                },
                stability_score: 42,
                authors: AuthorMetrics {
                    count: 2,
                    top_author_pct: 85.0,
                    single_owner_risk: true,
                },
                fix_risk: FixRiskMetrics {
                    keyword_pct: 40.0,
                    proximity_pct: 20.0,
                    combined_pct: 50.0,
                    insufficient_data: false,
                },
                blast_radius: vec![CouplingEntry {
                    path: "src/lib.rs".to_string(),
                    confidence: 0.75,
                    support: 4,
                }],
            }],
            modules: vec![ModuleHealth {
                path: "src".to_string(),
                encapsulation_pct: 60.0,
                files_count: 5,
                total_commits: 10,
                cross_boundary_commits: 4,
            }],
            coupling_graph: vec![CouplingEdge {
                a: "src/main.rs".to_string(),
                b: "src/lib.rs".to_string(),
                confidence: 0.75,
                support: 4,
            }],
            excluded_patterns: vec!["Cargo.lock".to_string()],
            warnings: vec![],
            file_targets: None,
        }
    }

    #[test]
    fn test_json_roundtrip() {
        let result = make_result();
        let json = render_json(&result).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["version"], 1);
        assert_eq!(parsed["repository"], "test-repo");
        assert!(parsed["files"].is_array());
        assert!(parsed["modules"].is_array());
        assert!(parsed["coupling_graph"].is_array());
    }

    #[test]
    fn test_json_has_all_metric_keys() {
        let result = make_result();
        let json = render_json(&result).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let file = &parsed["files"][0];
        assert!(file["churn"].is_object());
        assert!(file["stability_score"].is_number());
        assert!(file["authors"].is_object());
        assert!(file["fix_risk"].is_object());
        assert!(file["blast_radius"].is_array());
    }

    #[test]
    fn test_text_contains_top_churn_section() {
        let result = make_result();
        let text = render_text(&result, 20);
        assert!(text.contains("Top Churn"), "expected Top Churn section");
    }

    #[test]
    fn test_text_contains_blast_radius_section() {
        let result = make_result();
        let text = render_text(&result, 20);
        assert!(
            text.contains("Blast Radius"),
            "expected Blast Radius section"
        );
    }

    #[test]
    fn test_text_contains_module_health_section() {
        let result = make_result();
        let text = render_text(&result, 20);
        assert!(
            text.contains("Module Health"),
            "expected Module Health section"
        );
    }

    #[test]
    fn test_text_contains_bus_factor_section() {
        let result = make_result();
        let text = render_text(&result, 20);
        assert!(text.contains("Bus Factor"), "expected Bus Factor section");
    }

    #[test]
    fn test_text_contains_fix_risk_section() {
        let result = make_result();
        let text = render_text(&result, 20);
        assert!(text.contains("Fix Risk"), "expected Fix Risk section");
    }

    #[test]
    fn test_text_contains_repo_name() {
        let result = make_result();
        let text = render_text(&result, 20);
        assert!(text.contains("test-repo"));
    }

    #[test]
    fn test_text_respects_top_n() {
        let mut result = make_result();
        // Add 30 files with commits 1..=30 so file29.rs (30 commits) is highest-churn,
        // file28.rs (29) second, file27.rs (28) third, and file0.rs (1 commit) is lowest.
        for i in 0..30 {
            result.files.push(FileMetrics {
                path: format!("file{i}.rs"),
                churn: ChurnMetrics {
                    commits: i + 1,
                    rate: 0.1,
                },
                stability_score: 50,
                authors: AuthorMetrics {
                    count: 1,
                    top_author_pct: 100.0,
                    single_owner_risk: false,
                },
                fix_risk: FixRiskMetrics {
                    keyword_pct: 0.0,
                    proximity_pct: 0.0,
                    combined_pct: 0.0,
                    insufficient_data: true,
                },
                blast_radius: vec![],
            });
        }
        let text = render_text(&result, 3);

        // Count guard: at most 3 entries rendered in Top Churn section.
        let churn_count = text.matches("commits  ").count();
        assert!(
            churn_count <= 3,
            "expected at most 3 churn entries, got {churn_count}"
        );

        // Sort-order guard: the three highest-churn files must appear; the lowest must not.
        // This catches an ascending-vs-descending sort bug that the count check cannot.
        assert!(
            text.contains("file29.rs"),
            "expected highest-churn file (file29.rs, 30 commits) to appear in top-3"
        );
        assert!(
            text.contains("file28.rs"),
            "expected second-highest-churn file (file28.rs, 29 commits) to appear in top-3"
        );
        assert!(
            text.contains("file27.rs"),
            "expected third-highest-churn file (file27.rs, 28 commits) to appear in top-3"
        );
        assert!(
            !text.contains("file0.rs"),
            "expected lowest-churn file (file0.rs, 1 commit) to be excluded from top-3"
        );
    }

    #[test]
    fn test_text_empty_files_shows_placeholder() {
        let mut result = make_result();
        result.files.clear();
        let text = render_text(&result, 20);
        assert!(
            text.contains("(no files)"),
            "expected '(no files)' placeholder in Top Churn section"
        );
        assert!(
            text.contains("(no coupling above threshold)"),
            "expected '(no coupling above threshold)' placeholder in Blast Radius section"
        );
        assert!(
            text.contains("(no files above threshold)"),
            "expected '(no files above threshold)' placeholder in Fix Risk section"
        );
        assert!(
            text.contains("(no single-owner risk detected)"),
            "expected '(no single-owner risk detected)' placeholder in Bus Factor section"
        );
    }

    #[test]
    fn test_text_empty_modules_shows_placeholder() {
        let mut result = make_result();
        result.modules.clear();
        let text = render_text(&result, 20);
        assert!(
            text.contains("(no modules with enough data)"),
            "expected '(no modules with enough data)' placeholder in Module Health section"
        );
    }

    // -----------------------------------------------------------------------
    // Insights rendering tests
    // -----------------------------------------------------------------------

    fn make_insight(severity: Severity, category: &str, file: &str, msg: &str) -> Insight {
        Insight {
            severity,
            category: category.to_string(),
            file: file.to_string(),
            message: msg.to_string(),
            metric_value: 42.0,
        }
    }

    #[test]
    fn test_insights_text_contains_findings() {
        let insights = vec![
            make_insight(
                Severity::Critical,
                "stability",
                "a.rs",
                "a.rs: critically unstable (score 22/100)",
            ),
            make_insight(
                Severity::Warning,
                "fix-risk",
                "b.rs",
                "b.rs: elevated fix-risk (40.0% combined)",
            ),
        ];
        let text = render_insights_text(&insights);
        assert!(
            text.contains("Insights"),
            "expected Insights header in output"
        );
        assert!(
            text.contains("critically unstable"),
            "expected CRITICAL message in output"
        );
        assert!(
            text.contains("elevated fix-risk"),
            "expected WARNING message in output"
        );
    }

    #[test]
    fn test_insights_text_empty_state() {
        let insights: Vec<Insight> = vec![];
        let text = render_insights_text(&insights);
        assert!(
            text.contains("(no notable findings)"),
            "expected empty-state message, got: {text}"
        );
    }

    #[test]
    fn test_insights_json_valid() {
        use crate::cmd::heatmap::types::{CompactFileEntry, CompactModuleEntry, InsightsResult};

        let ir = InsightsResult {
            version: 1,
            repository: "test-repo".to_string(),
            window: WindowInfo {
                mode: "90d".to_string(),
                since: "2024-10-01".to_string(),
                until: "2025-01-01".to_string(),
                commits_analyzed: 5,
                effective_strategy: None,
            },
            insights: vec![make_insight(
                Severity::Critical,
                "stability",
                "a.rs",
                "a.rs: critically unstable (score 22/100)",
            )],
            top_files: vec![CompactFileEntry {
                path: "a.rs".to_string(),
                stability: 22,
                churn_commits: 10,
                fix_risk_pct: 0.0,
                bus_factor_risk: false,
            }],
            flagged_modules: vec![CompactModuleEntry {
                path: "src/".to_string(),
                encapsulation_pct: 30.0,
                cross_boundary: 3,
            }],
        };

        let json = render_insights_json(&ir).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["version"], 1);
        assert!(parsed["insights"].is_array());
    }

    #[test]
    fn test_insights_json_schema() {
        use crate::cmd::heatmap::types::{CompactFileEntry, CompactModuleEntry, InsightsResult};

        let ir = InsightsResult {
            version: 1,
            repository: "repo".to_string(),
            window: WindowInfo {
                mode: "90d".to_string(),
                since: "2024-10-01".to_string(),
                until: "2025-01-01".to_string(),
                commits_analyzed: 3,
                effective_strategy: None,
            },
            insights: vec![],
            top_files: vec![CompactFileEntry {
                path: "x.rs".to_string(),
                stability: 80,
                churn_commits: 2,
                fix_risk_pct: 10.0,
                bus_factor_risk: false,
            }],
            flagged_modules: vec![CompactModuleEntry {
                path: "lib/".to_string(),
                encapsulation_pct: 45.0,
                cross_boundary: 2,
            }],
        };

        let json = render_insights_json(&ir).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

        // Required top-level keys
        assert!(parsed["version"].is_number(), "version must be number");
        assert!(
            parsed["repository"].is_string(),
            "repository must be string"
        );
        assert!(parsed["window"].is_object(), "window must be object");
        assert!(parsed["insights"].is_array(), "insights must be array");
        assert!(parsed["top_files"].is_array(), "top_files must be array");
        assert!(
            parsed["flagged_modules"].is_array(),
            "flagged_modules must be array"
        );

        // top_files entry keys
        let tf = &parsed["top_files"][0];
        assert!(tf["path"].is_string());
        assert!(tf["stability"].is_number());
        assert!(tf["churn_commits"].is_number());
        assert!(tf["fix_risk_pct"].is_number());
        assert!(tf["bus_factor_risk"].is_boolean());

        // flagged_modules entry keys
        let fm = &parsed["flagged_modules"][0];
        assert!(fm["path"].is_string());
        assert!(fm["encapsulation_pct"].is_number());
        assert!(fm["cross_boundary"].is_number());
    }
}
