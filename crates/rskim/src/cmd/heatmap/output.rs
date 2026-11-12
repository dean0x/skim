//! Rendering layer for `skim heatmap` output.
//!
//! The default text view is plain ASCII with no color: one header line, at
//! most two warning lines, then for each section one heading line and one
//! line per row, with no blank line. The renderers take the rows from the
//! ranked view (`view.rs`), already sorted, gated and capped, so this module
//! sorts, filters and caps nothing.

use std::path::Path;

use colored::Colorize;

use super::noise;
use super::types::{DetailSection, HeatmapResult, Insight, InsightsResult, Severity};
use super::view::{HeatmapView, text_warnings};

// ============================================================================
// JSON output
// ============================================================================

/// Render the view as compact JSON version 2 (one line).
pub(crate) fn render_json(
    result: &HeatmapResult,
    view: &HeatmapView<'_>,
) -> anyhow::Result<String> {
    Ok(serde_json::to_string(&view.to_json(result))?)
}

// ============================================================================
// Text output
// ============================================================================

/// Render the default text view, or the one section that `--detail` names.
pub(crate) fn render_text(result: &HeatmapResult, view: &HeatmapView<'_>) -> String {
    let mut out = String::new();
    push_line(&mut out, &header_line("Heatmap", result));
    let (warnings, more_warnings) = text_warnings(&result.warnings);
    for warning in warnings {
        push_line(&mut out, &format!("warning: {warning}"));
    }
    if let Some(line) = more_warnings {
        push_line(&mut out, &line);
    }
    for section in DetailSection::ALL {
        if view.spec.shows(section) {
            push_section(&mut out, view, section);
        }
    }
    out
}

fn push_line(out: &mut String, line: &str) {
    out.push_str(line);
    out.push('\n');
}

/// The header line: the repo name (never its absolute path), the window
/// dates, the commit count, the window strategy and, for file targets, a
/// scope note.
fn header_line(label: &str, result: &HeatmapResult) -> String {
    let window = &result.window;
    let strategy = window
        .effective_strategy
        .as_deref()
        .map(|s| format!(" ({s})"))
        .unwrap_or_default();
    let scope = result
        .file_targets
        .as_ref()
        .map(|targets| format!(", scoped to {} files", targets.len()))
        .unwrap_or_default();
    format!(
        "{label}: {} {}..{}, {} commits, {} window{strategy}{scope}",
        repo_name(&result.repository),
        window.since,
        window.until,
        window.commits_analyzed,
        window.mode
    )
}

/// The last component of the repository path. A path with no name (`/`)
/// or a name that is not UTF-8 gives `repository`.
fn repo_name(repository: &str) -> &str {
    Path::new(repository)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repository")
}

/// One section: its heading, its rows (or one line that tells why there is
/// none), and the "more" line when the cap hides rows.
fn push_section(out: &mut String, view: &HeatmapView<'_>, section: DetailSection) {
    push_line(out, section_heading(section));
    let rows = section_rows(view, section);
    if rows.is_empty() {
        push_line(out, &empty_section_line(view, section));
    }
    for row in &rows {
        push_line(out, row);
    }
    if let Some(more) = view.more(section) {
        push_line(out, &format!("  {more}"));
    }
}

/// The heading of `section`. The words in parentheses name the numbers of
/// each row, in order.
fn section_heading(section: DetailSection) -> &'static str {
    match section {
        DetailSection::Hotspots => "Hotspots (commits):",
        DetailSection::Coupling => "Coupling (confidence, shared commits):",
        DetailSection::FixRisk => "Fix risk (fix commits/commits, lift, base rate):",
        DetailSection::Modules => "Modules (encapsulation, commits):",
        DetailSection::BusFactor => "Bus factor (top author share, commits):",
    }
}

/// The rows of `section`, in view order. Each row gives its number and its
/// support before the path.
fn section_rows(view: &HeatmapView<'_>, section: DetailSection) -> Vec<String> {
    match section {
        DetailSection::Hotspots => view
            .hotspots
            .rows
            .iter()
            .map(|f| format!("  {:>3}  {}", f.churn.commits, f.path))
            .collect(),
        DetailSection::Coupling => view
            .coupling
            .rows
            .iter()
            .map(|e| {
                format!(
                    "  {:>3.0}% {:>3}  {} <-> {}",
                    e.confidence * 100.0,
                    e.support,
                    e.a,
                    e.b
                )
            })
            .collect(),
        DetailSection::FixRisk => view
            .fix_risk
            .rows
            .iter()
            .map(|r| {
                format!(
                    "  {}/{}  lift {:.1}  base {:.0}%  {}",
                    r.fix_commits,
                    r.support,
                    r.lift,
                    view.base_rate * 100.0,
                    r.path
                )
            })
            .collect(),
        DetailSection::Modules => view
            .modules
            .rows
            .iter()
            .map(|m| {
                format!(
                    "  {:>3.0}% {:>3}  {}",
                    m.encapsulation_pct, m.total_commits, m.path
                )
            })
            .collect(),
        DetailSection::BusFactor => view
            .bus_factor
            .rows
            .iter()
            .map(|r| format!("  {:>3.0}% {:>3}  {}", r.top_author_pct, r.support, r.path))
            .collect(),
    }
}

/// The line of a section with no rows. At a fix base rate of 0% or 100% no
/// file can rise above it, and the line says so.
fn empty_section_line(view: &HeatmapView<'_>, section: DetailSection) -> String {
    if section == DetailSection::FixRisk && noise::base_rate_is_degenerate(view.base_rate) {
        return format!(
            "  (none: the fix base rate is {:.0}%)",
            view.base_rate * 100.0
        );
    }
    "  (none)".to_string()
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

    // Header line: "─── Insights ───────────────────────────────────────────────────────────"
    let label = " Insights ";
    let prefix = "───";
    let suffix_len = HEADER_WIDTH.saturating_sub(prefix.len() + label.len());
    let suffix = "─".repeat(suffix_len);
    let header = format!("{prefix}{label}{suffix}");
    out.push_str(&header.bold().to_string());
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
        out.push_str(&format!(
            "  {severity_str}  {}: {}\n",
            insight.file, insight.message
        ));
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::cmd::heatmap::types::{
        AuthorMetrics, ChurnMetrics, CouplingEdge, CouplingEntry, FileMetrics, FixRiskMetrics,
        InsightCategory, ModuleHealth, WindowInfo,
    };
    use crate::cmd::heatmap::view::{ViewSpec, build_view};

    const DEFAULT_VIEW: ViewSpec = ViewSpec {
        detail: None,
        top: None,
    };

    fn make_result() -> HeatmapResult {
        HeatmapResult {
            generated_at: "2025-01-01T00:00:00Z".to_string(),
            repository: "/home/dev/work/test-repo".to_string(),
            window: WindowInfo {
                mode: "dual".to_string(),
                since: "2024-10-01".to_string(),
                until: "2025-01-01".to_string(),
                commits_analyzed: 10,
                effective_strategy: Some("count".to_string()),
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
                    fix_commits: 2,
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
            fix_commits_total: 3,
            window_authors: 2,
        }
    }

    /// A file with `commits` commits, `fix_commits` of them fix commits.
    fn file(path: &str, commits: usize, fix_commits: usize) -> FileMetrics {
        FileMetrics {
            path: path.to_string(),
            churn: ChurnMetrics { commits, rate: 0.1 },
            stability_score: 50,
            authors: AuthorMetrics {
                count: 2,
                top_author_pct: 50.0,
                single_owner_risk: false,
            },
            fix_risk: FixRiskMetrics {
                fix_commits,
                insufficient_data: commits < noise::MIN_FILE_SUPPORT,
                ..FixRiskMetrics::default()
            },
            blast_radius: vec![],
        }
    }

    fn edge(a: &str, b: &str, confidence: f64, support: usize) -> CouplingEdge {
        CouplingEdge {
            a: a.to_string(),
            b: b.to_string(),
            confidence,
            support,
        }
    }

    /// A path of exactly 100 chars that ends in `{tag}{i:03}.rs`.
    fn long_path(tag: char, i: usize) -> String {
        let tail = format!("{tag}{i:03}.rs");
        format!("{}{tail}", "d/".repeat((100 - tail.len()) / 2))
    }

    fn text_of(result: &HeatmapResult, spec: ViewSpec) -> String {
        render_text(result, &build_view(result, &spec))
    }

    /// The lines under the heading that starts with `heading`, up to the next
    /// heading.
    fn section_lines<'a>(text: &'a str, heading: &str) -> Vec<&'a str> {
        text.lines()
            .skip_while(|l| !l.starts_with(heading))
            .skip(1)
            .take_while(|l| l.starts_with("  "))
            .collect()
    }

    // -----------------------------------------------------------------------
    // Default text view
    // -----------------------------------------------------------------------

    #[test]
    fn test_text_default_view_of_a_large_result_fits_in_thirty_lines() {
        let mut result = make_result();
        result.window.commits_analyzed = 1000;
        result.fix_commits_total = 100;
        // 100 files with 100-char paths; the first 20 have only fix commits,
        // far above the 10% base rate.
        result.files = (0..100)
            .map(|i| {
                let commits = 10 + i;
                let fixes = if i < 20 { commits } else { 0 };
                file(&long_path('f', i), commits, fixes)
            })
            .collect();
        result.coupling_graph = (0..60)
            .map(|i| edge(&long_path('a', i), &long_path('b', i), 0.9, 3))
            .collect();
        result.warnings = (1..=4).map(|i| format!("warning number {i}")).collect();

        let text = text_of(&result, DEFAULT_VIEW);
        let lines: Vec<&str> = text.lines().collect();
        // header 1, warnings 2, then heading + rows + more line per section:
        // hotspots 1 + 10 + 1, coupling 1 + 5 + 1, fix risk 1 + 5 + 1.
        assert_eq!(lines.len(), 29, "{text}");
        assert!(lines.len() <= 30);
        assert!(lines.iter().all(|l| !l.is_empty()), "no blank line: {text}");
        assert_eq!(lines[2], "3 more warnings (--json)");
        assert!(lines.contains(&"  90 more (--detail hotspots)"), "{text}");
        assert!(lines.contains(&"  55 more (--detail coupling)"), "{text}");
        assert!(lines.contains(&"  15 more (--detail fix-risk)"), "{text}");
    }

    #[test]
    fn test_text_header_has_repo_name_dates_count_and_strategy() {
        let result = make_result();
        let text = text_of(&result, DEFAULT_VIEW);
        assert_eq!(
            text.lines().next(),
            Some("Heatmap: test-repo 2024-10-01..2025-01-01, 10 commits, dual window (count)")
        );
        assert!(!text.contains("/home/dev"), "no absolute path: {text}");
    }

    #[test]
    fn test_text_header_notes_the_file_scope() {
        let mut result = make_result();
        result.window.mode = "sprint".to_string();
        result.window.effective_strategy = None;
        result.file_targets = Some(vec!["src/main.rs".to_string(), "src/lib.rs".to_string()]);
        let text = text_of(&result, DEFAULT_VIEW);
        assert_eq!(
            text.lines().next(),
            Some(
                "Heatmap: test-repo 2024-10-01..2025-01-01, 10 commits, sprint window, scoped to 2 files"
            )
        );
    }

    #[test]
    fn test_text_shows_two_warnings_in_full() {
        let mut result = make_result();
        result.warnings = vec!["first".to_string(), "second".to_string()];
        let text = text_of(&result, DEFAULT_VIEW);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[1..3], ["warning: first", "warning: second"]);
        assert_eq!(lines[3], "Hotspots (commits):");
    }

    #[test]
    fn test_text_default_view_has_three_sections_in_order() {
        let result = make_result();
        let text = text_of(&result, DEFAULT_VIEW);
        let headings: Vec<&str> = text
            .lines()
            .skip(1)
            .filter(|l| !l.starts_with(' '))
            .collect();
        assert_eq!(
            headings,
            [
                "Hotspots (commits):",
                "Coupling (confidence, shared commits):",
                "Fix risk (fix commits/commits, lift, base rate):",
            ]
        );
        assert!(text.is_ascii(), "{text}");
    }

    #[test]
    fn test_text_rows_give_their_number_and_support() {
        let mut result = make_result();
        result.files = vec![file("src/a.rs", 12, 0)];
        result.coupling_graph = vec![edge("src/a.rs", "src/b.rs", 0.87, 7)];
        let text = text_of(&result, DEFAULT_VIEW);
        assert_eq!(section_lines(&text, "Hotspots"), ["   12  src/a.rs"]);
        assert_eq!(
            section_lines(&text, "Coupling"),
            ["   87%   7  src/a.rs <-> src/b.rs"]
        );
    }

    #[test]
    fn test_text_fix_risk_row_prints_fix_count_support_lift_and_base_rate() {
        let mut result = make_result();
        // Base rate 20 of 100 commits; 5 fix commits of 5: lift 1.0 / 0.2 = 5.
        result.window.commits_analyzed = 100;
        result.fix_commits_total = 20;
        result.files = vec![file("src/f.rs", 5, 5), file("src/g.rs", 5, 0)];
        let text = text_of(&result, DEFAULT_VIEW);
        assert_eq!(
            section_lines(&text, "Fix risk"),
            ["  5/5  lift 5.0  base 20%  src/f.rs"]
        );
    }

    #[test]
    fn test_text_fix_risk_gives_one_reason_line_at_zero_and_full_base_rate() {
        for (fixes, rate) in [(0, "0%"), (10, "100%")] {
            let mut result = make_result();
            result.window.commits_analyzed = 10;
            result.fix_commits_total = fixes;
            result.files = vec![file("src/f.rs", 10, fixes)];
            let text = text_of(&result, DEFAULT_VIEW);
            assert_eq!(
                section_lines(&text, "Fix risk"),
                [format!("  (none: the fix base rate is {rate})")],
                "{text}"
            );
        }
    }

    #[test]
    fn test_text_empty_section_shows_none() {
        let mut result = make_result();
        result.coupling_graph.clear();
        let text = text_of(&result, DEFAULT_VIEW);
        assert_eq!(section_lines(&text, "Coupling"), ["  (none)"]);
    }

    // -----------------------------------------------------------------------
    // --detail text view
    // -----------------------------------------------------------------------

    #[test]
    fn test_text_detail_view_shows_only_the_named_section_with_all_rows() {
        let mut result = make_result();
        result.modules = (1..=12)
            .map(|i| ModuleHealth {
                path: format!("crates/m{i:02}"),
                encapsulation_pct: 50.0,
                files_count: 2,
                total_commits: 3 + i,
                cross_boundary_commits: 1,
            })
            .collect();
        let spec = ViewSpec {
            detail: Some(DetailSection::Modules),
            top: None,
        };
        let text = text_of(&result, spec);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[1], "Modules (encapsulation, commits):");
        assert_eq!(lines.len(), 2 + 12, "{text}");
        assert_eq!(lines[2], "   50%  15  crates/m12");
    }

    #[test]
    fn test_text_detail_view_with_top_names_top_in_the_more_line() {
        let mut result = make_result();
        result.files = (1..=4)
            .map(|i| file(&format!("src/f{i}.rs"), i, 0))
            .collect();
        let spec = ViewSpec {
            detail: Some(DetailSection::Hotspots),
            top: Some(1),
        };
        let text = text_of(&result, spec);
        assert_eq!(
            text.lines().skip(1).collect::<Vec<_>>(),
            [
                "Hotspots (commits):",
                "    4  src/f4.rs",
                "  3 more (raise --top)"
            ]
        );
    }

    // -----------------------------------------------------------------------
    // JSON
    // -----------------------------------------------------------------------

    #[test]
    fn test_json_is_compact_version_two() {
        let result = make_result();
        let json = render_json(&result, &build_view(&result, &DEFAULT_VIEW)).unwrap();
        assert_eq!(json.lines().count(), 1, "one line: {json}");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["version"], 2);
        assert_eq!(parsed["repository"], "/home/dev/work/test-repo");
        assert_eq!(parsed["counts"]["hotspots"]["shown"], 1);
    }

    #[test]
    fn test_json_has_all_metric_keys() {
        let result = make_result();
        let json = render_json(&result, &build_view(&result, &DEFAULT_VIEW)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let file = &parsed["files"][0];
        assert_eq!(file["churn"]["commits"], 5);
        assert_eq!(file["stability_score"], 42);
        assert!(file["authors"].is_object());
        assert_eq!(file["fix_risk"]["keyword_pct"], 40.0);
        assert_eq!(file["fix_risk"]["insufficient_data"], false);
        assert_eq!(file["blast_radius"][0]["path"], "src/lib.rs");
    }

    // -----------------------------------------------------------------------
    // Insights rendering tests
    // -----------------------------------------------------------------------

    fn make_insight(
        severity: Severity,
        category: InsightCategory,
        file: &str,
        msg: &str,
    ) -> Insight {
        Insight {
            severity,
            category,
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
                InsightCategory::Stability,
                "a.rs",
                "critically unstable (score 22/100)",
            ),
            make_insight(
                Severity::Warning,
                InsightCategory::FixRisk,
                "b.rs",
                "elevated fix-risk (40.0% combined)",
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
                InsightCategory::Stability,
                "a.rs",
                "critically unstable (score 22/100)",
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
