//! Rendering layer for `skim heatmap` output.
//!
//! The default text view is plain ASCII with no color: one header line, at
//! most two warning lines, then for each section one heading line and one
//! line per row, with no blank line. The insights text has the same header
//! line and warning lines, then one line per finding. Both JSON shapes are
//! compact (one line).
//! The renderers take the rows from the ranked view (`view.rs`) and the
//! ranked insights, already sorted, gated and capped, so this module sorts,
//! filters and caps nothing.

use std::path::Path;

use super::card::{Card, CardCounts};
use super::noise;
use super::types::{DetailSection, HeatmapResult, InsightsResult, Severity, WindowInfo};
use super::view::{HeatmapView, ListCaps, MoreRemedy, Section, more_line, text_warnings};

// ============================================================================
// JSON output
// ============================================================================

/// Render the view as compact JSON version 2 (one line), with the cards of
/// a run with file arguments.
pub(crate) fn render_json(
    result: &HeatmapResult,
    view: &HeatmapView<'_>,
    cards: Option<&Section<Card>>,
) -> anyhow::Result<String> {
    Ok(serde_json::to_string(&view.to_json(result, cards))?)
}

// ============================================================================
// Text output
// ============================================================================

/// Render the default text view, or the one section that `--detail` names.
pub(crate) fn render_text(result: &HeatmapResult, view: &HeatmapView<'_>) -> String {
    let mut out = String::new();
    push_line(&mut out, &header_line("Heatmap", result));
    push_warnings(&mut out, &result.warnings);
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

/// The warning lines of a text view: at most two (see `text_warnings`).
fn push_warnings(out: &mut String, warnings: &[String]) {
    let (shown, more_warnings) = text_warnings(warnings);
    for warning in shown {
        push_line(out, &format!("warning: {warning}"));
    }
    if let Some(line) = more_warnings {
        push_line(out, &line);
    }
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
// Card rendering
// ============================================================================

/// Render the cards of a run with file arguments: the header line, the
/// warning lines, each card, and one line with the count of file arguments
/// that the cap leaves without a card. The plain and the `--insights` run
/// give these same bytes.
///
/// At the default caps a card has 12 lines or fewer: the title, an owners
/// line, the partners heading with 4 rows, the fix-commit heading with 3
/// rows, and one line with the hidden counts.
pub(crate) fn render_cards_text(
    result: &HeatmapResult,
    cards: &Section<Card>,
    caps: ListCaps,
) -> String {
    let mut out = String::new();
    push_line(&mut out, &header_line("Heatmap", result));
    push_warnings(&mut out, &result.warnings);
    for card in &cards.rows {
        push_card(&mut out, card, &result.window, caps);
    }
    let hidden = cards.count().hidden();
    if hidden > 0 {
        push_line(
            &mut out,
            &format!("{} (raise --top)", more_items(hidden, "file", "files")),
        );
    }
    out
}

/// One card. The commit subjects are in quotes under a "from history"
/// heading: they are text from the repository, not instructions.
fn push_card(out: &mut String, card: &Card, window: &WindowInfo, caps: ListCaps) {
    if !card.in_window {
        push_line(
            out,
            &format!(
                "{}: not in history window ({} commits since {})",
                card.path, window.commits_analyzed, window.since
            ),
        );
        return;
    }
    push_line(out, &format!("{}: {} commits", card.path, card.commits));
    if !card.owners.is_empty() {
        let owners: Vec<String> = card
            .owners
            .iter()
            .map(|o| format!("{} {:.0}%", o.name, o.share_pct))
            .collect();
        push_line(out, &format!("  owners: {}", owners.join(", ")));
    }
    push_line(out, "  partners (confidence, shared commits):");
    if card.partners.is_empty() {
        push_line(out, "    (none)");
    }
    for p in &card.partners {
        push_line(
            out,
            &format!(
                "    {:>3.0}% {:>3}  {}",
                p.confidence * 100.0,
                p.support,
                p.path
            ),
        );
    }
    push_line(out, "  fix commits from history (newest first):");
    if card.fix_commits.is_empty() {
        push_line(out, "    (none)");
    }
    for f in &card.fix_commits {
        push_line(out, &format!("    {} {} \"{}\"", f.sha, f.date, f.subject));
    }
    if let Some(line) = card_more_line(card.counts, caps) {
        push_line(out, &format!("  {line}"));
    }
}

/// The line with the hidden counts of a card, or `None` when the caps hide
/// nothing. `--top` shows more owners and partners, and more fix commits up
/// to their maximum; past it, only `git log` shows the others.
fn card_more_line(counts: CardCounts, caps: ListCaps) -> Option<String> {
    let mut by_top: Vec<String> = Vec::new();
    for (count, one, many) in [
        (counts.owners, "owner", "owners"),
        (counts.partners, "partner", "partners"),
    ] {
        if count.hidden() > 0 {
            by_top.push(more_items(count.hidden(), one, many));
        }
    }
    let fix_hidden = counts.fix_commits.hidden();
    let mut by_log = None;
    if fix_hidden > 0 {
        let fix = more_items(fix_hidden, "fix commit", "fix commits");
        if caps.fix_commits_at_max() {
            by_log = Some(format!("{fix} (git log)"));
        } else {
            by_top.push(fix);
        }
    }
    let by_top = (!by_top.is_empty()).then(|| format!("{} (raise --top)", by_top.join(", ")));
    let parts: Vec<String> = by_top.into_iter().chain(by_log).collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// `"N more <noun>"`, with the singular noun for 1.
fn more_items(n: usize, one: &str, many: &str) -> String {
    format!("{n} more {}", if n == 1 { one } else { many })
}

// ============================================================================
// Insights rendering
// ============================================================================

/// Render `--insights` text: the header line, the warning lines, one line
/// per shown finding in rank order, and a "more" line when the cap hides
/// findings.
pub(crate) fn render_insights_text(result: &HeatmapResult, insights: &InsightsResult) -> String {
    let mut out = String::new();
    push_line(&mut out, &header_line("Insights", result));
    push_warnings(&mut out, &insights.warnings);
    if insights.insights.is_empty() {
        push_line(&mut out, "  (no notable findings)");
    }
    for insight in &insights.insights {
        push_line(
            &mut out,
            &format!(
                "  {} {}: {}",
                severity_label(&insight.severity),
                insight.file,
                insight.message
            ),
        );
    }
    if let Some(more) = more_line(insights.counts.insights.hidden(), MoreRemedy::Top) {
        push_line(&mut out, &format!("  {more}"));
    }
    out
}

/// The severity column of an insight line, padded to one width.
fn severity_label(severity: &Severity) -> &'static str {
    match severity {
        Severity::Critical => "CRITICAL",
        Severity::Warning => "WARNING ",
    }
}

/// Render `--insights --json` as compact JSON version 2 (one line).
pub(crate) fn render_insights_json(insights_result: &InsightsResult) -> anyhow::Result<String> {
    Ok(serde_json::to_string(insights_result)?)
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::cmd::heatmap::types::{
        AuthorMetrics, ChurnMetrics, CompactFileEntry, CompactModuleEntry, Count, CouplingEdge,
        CouplingEntry, FileMetrics, FixRiskMetrics, Insight, InsightCategory, InsightCounts,
        ModuleHealth, WindowInfo,
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
        let json = render_json(&result, &build_view(&result, &DEFAULT_VIEW), None).unwrap();
        assert_eq!(json.lines().count(), 1, "one line: {json}");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["version"], 2);
        assert_eq!(parsed["repository"], "/home/dev/work/test-repo");
        assert_eq!(parsed["counts"]["hotspots"]["shown"], 1);
    }

    #[test]
    fn test_json_has_all_metric_keys() {
        let result = make_result();
        let json = render_json(&result, &build_view(&result, &DEFAULT_VIEW), None).unwrap();
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

    fn make_insight(severity: Severity, file: &str, msg: &str) -> Insight {
        Insight {
            severity,
            category: InsightCategory::Stability,
            file: file.to_string(),
            message: msg.to_string(),
            metric_value: 42.0,
            support: 5,
            score: 0.58,
        }
    }

    fn insights_of(insights: Vec<Insight>, total: usize) -> InsightsResult {
        InsightsResult {
            version: 2,
            repository: "/home/dev/work/test-repo".to_string(),
            window: make_result().window,
            counts: InsightCounts {
                insights: Count {
                    total,
                    shown: insights.len(),
                },
            },
            insights,
            top_files: vec![CompactFileEntry {
                path: "a.rs".to_string(),
                stability: 22,
                churn_commits: 10,
                fix_risk_pct: 0.0,
                bus_factor_risk: false,
            }],
            flagged_modules: vec![CompactModuleEntry {
                path: "src".to_string(),
                encapsulation_pct: 30.0,
                cross_boundary: 3,
            }],
            warnings: vec![],
        }
    }

    #[test]
    fn test_insights_text_gives_header_rows_and_more_line() {
        let result = make_result();
        let shown = vec![
            make_insight(
                Severity::Critical,
                "a.rs",
                "critically unstable (score 22/100, 9 commits)",
            ),
            make_insight(
                Severity::Warning,
                "b.rs",
                "moderate instability (score 55/100, 6 commits)",
            ),
        ];
        let text = render_insights_text(&result, &insights_of(shown, 5));
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            [
                "Insights: test-repo 2024-10-01..2025-01-01, 10 commits, dual window (count)",
                "  CRITICAL a.rs: critically unstable (score 22/100, 9 commits)",
                "  WARNING  b.rs: moderate instability (score 55/100, 6 commits)",
                "  3 more (raise --top)",
            ]
        );
    }

    #[test]
    fn test_insights_text_shows_the_run_warnings_after_the_header() {
        let result = make_result();
        let mut insights = insights_of(vec![], 0);
        insights.warnings = vec!["first".to_string(), "second".to_string()];
        let text = render_insights_text(&result, &insights);
        assert_eq!(
            text.lines().skip(1).collect::<Vec<_>>(),
            [
                "warning: first",
                "warning: second",
                "  (no notable findings)"
            ]
        );
    }

    #[test]
    fn test_insights_text_counts_the_warnings_past_the_text_cap() {
        let result = make_result();
        let mut insights = insights_of(vec![], 0);
        insights.warnings = (1..=4).map(|i| format!("warning number {i}")).collect();
        let text = render_insights_text(&result, &insights);
        assert_eq!(
            text.lines().skip(1).take(2).collect::<Vec<_>>(),
            ["warning: warning number 1", "3 more warnings (--json)"]
        );
    }

    #[test]
    fn test_insights_json_carries_every_run_warning() {
        let mut insights = insights_of(vec![], 0);
        insights.warnings = (1..=4).map(|i| format!("warning number {i}")).collect();
        let json = render_insights_json(&insights).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(
            parsed["warnings"],
            serde_json::json!([
                "warning number 1",
                "warning number 2",
                "warning number 3",
                "warning number 4"
            ])
        );
    }

    #[test]
    fn test_insights_text_empty_state() {
        let result = make_result();
        let text = render_insights_text(&result, &insights_of(vec![], 0));
        assert_eq!(text.lines().nth(1), Some("  (no notable findings)"));
        assert_eq!(text.lines().count(), 2, "{text}");
    }

    #[test]
    fn test_insights_json_is_compact_with_counts_and_ranked_fields() {
        let shown = vec![make_insight(
            Severity::Critical,
            "a.rs",
            "critically unstable",
        )];
        let json = render_insights_json(&insights_of(shown, 4)).unwrap();
        assert_eq!(json.lines().count(), 1, "one line: {json}");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["version"], 2);
        assert_eq!(
            parsed["counts"]["insights"],
            serde_json::json!({"total": 4, "shown": 1})
        );
        let insight = &parsed["insights"][0];
        assert_eq!(insight["severity"], "Critical");
        assert_eq!(insight["category"], "stability");
        assert_eq!(insight["support"], 5);
        assert_eq!(insight["score"], 0.58);

        // top_files and flagged_modules entry keys
        let tf = &parsed["top_files"][0];
        assert!(tf["path"].is_string());
        assert!(tf["stability"].is_number());
        assert!(tf["churn_commits"].is_number());
        assert!(tf["fix_risk_pct"].is_number());
        assert!(tf["bus_factor_risk"].is_boolean());
        let fm = &parsed["flagged_modules"][0];
        assert!(fm["path"].is_string());
        assert!(fm["encapsulation_pct"].is_number());
        assert!(fm["cross_boundary"].is_number());
    }
}
