//! Repository map for `skim heatmap --map`.

use std::collections::BTreeMap;

use super::card::{CARD_TEXT_MAX_CHARS, sanitize};
use super::output::{header_line, push_warnings};
use super::types::{FileMetrics, HeatmapResult};
use super::view::{ViewSpec, ranked_files};
use crate::output::RunBudget;
use crate::run_budget::{Body, Piece, RUN_SUM_STOP_FACTOR, plan_cut_with_trailer};

/// Render whole blocks in hotspot order. Only the shared planner can accept
/// content for stdout. If any required count fails, return only the trailer.
#[must_use]
pub(super) fn render(
    result: &HeatmapResult,
    spec: &ViewSpec,
    budget: usize,
    count: &(dyn Fn(&str) -> Option<usize> + Sync),
) -> String {
    let candidates = ranked_files(result, spec);
    let total = candidates.len();
    let mut header = header_line("Heatmap map", result);
    header.push('\n');
    push_warnings(&mut header, &result.warnings);
    if total == 0 {
        header.push_str("(none)\n");
    }
    let Some(mut sum) = count(&header).filter(|&cost| cost <= budget) else {
        return format_trailer(0, total, &[]);
    };
    let mut pieces = vec![fixed_piece(header, sum)];
    let mut reasons = Vec::new();
    let stop = budget.saturating_mul(RUN_SUM_STOP_FACTOR);
    for file in candidates {
        let (text, reason) = file_block(file);
        let Some(cost) = count(&text) else {
            return format_trailer(0, total, &[]);
        };
        sum = sum.saturating_add(cost);
        if sum > stop {
            break;
        }
        pieces.push(fixed_piece(text, cost));
        reasons.push(reason);
    }

    plan_cut_with_trailer(
        &pieces,
        RunBudget::Tokens(budget),
        count,
        |kept, _| trailer_line(kept, total, &reasons),
        || count(&format_trailer(total, total, &reasons)).unwrap_or(usize::MAX),
    )
    .output
}

fn fixed_piece(text: String, cost: usize) -> Piece<'static> {
    Piece {
        prefix: String::new(),
        body: Body::Fixed(text.into()),
        cut_language: None,
        count: cost,
        floor: cost,
    }
}

/// One block is atomic: the planner keeps or drops its stats and all subrows.
/// A reason, if present, belongs to this block's rank, not to the path.
fn file_block(file: &FileMetrics) -> (String, Option<&'static str>) {
    let path = sanitize(&file.path, CARD_TEXT_MAX_CHARS);
    let commits = file.churn.commits;
    let commit_noun = if commits == 1 { "commit" } else { "commits" };
    let lines = file.lines;
    let line_noun = if lines == 1 { "line" } else { "lines" };
    let mut text = format!(
        "{path}: {commits} {commit_noun}, {lines} {line_noun}, score {}\n",
        file.score()
    );
    if let Some(partner) = file.blast_radius.first() {
        let path = sanitize(&partner.path, CARD_TEXT_MAX_CHARS);
        text.push_str(&format!("  changes with: {path}\n"));
    }
    (text, None)
}

/// The header is one piece but not a file. The real total includes ranks the
/// construction bound did not reach. Reasons count only the retained ranks.
fn trailer_line(kept: usize, total: usize, reasons: &[Option<&str>]) -> Option<String> {
    let shown = kept.saturating_sub(1);
    debug_assert!(shown <= reasons.len() && reasons.len() <= total);
    let reasons = &reasons[..shown];
    (kept == 0 || shown < total || reasons.iter().any(Option::is_some))
        .then(|| format_trailer(shown, total, reasons))
}

fn format_trailer(shown: usize, total: usize, reasons: &[Option<&str>]) -> String {
    let mut counts = BTreeMap::new();
    for reason in reasons.iter().flatten() {
        let count = counts.entry(reason).or_insert(0usize);
        *count = count.saturating_add(1);
    }
    let mut line = format!("[skim] {shown} of {total} files");
    for (reason, count) in counts {
        line.push_str(&format!("; {reason}: {count}"));
    }
    line.push_str(" — adjust the map budget with --tokens\n");
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::heatmap::types::{ChurnMetrics, CouplingEntry, FileMetrics, WindowInfo};

    const SPEC: ViewSpec = ViewSpec {
        detail: None,
        top: None,
        include_docs: false,
    };

    fn file(path: &str, commits: usize, lines: u64) -> FileMetrics {
        FileMetrics {
            path: path.into(),
            churn: ChurnMetrics { commits, rate: 0.0 },
            lines,
            stability_score: 0,
            authors: Default::default(),
            fix_risk: Default::default(),
            blast_radius: Vec::new(),
        }
    }

    fn fixture() -> HeatmapResult {
        HeatmapResult {
            generated_at: String::new(),
            repository: "/repo/demo".into(),
            window: WindowInfo {
                mode: "time".into(),
                since: "2025-01-01".into(),
                until: "2025-01-02".into(),
                commits_analyzed: 5,
                effective_strategy: None,
            },
            files: vec![
                file("src/small.rs", 4, 2),
                file("src/large.rs", 1, 50),
                file("src/tiny.rs", 1, 1),
                file("docs/guide.md", 5, 500),
            ],
            not_at_head: 0,
            modules: Vec::new(),
            coupling_graph: Vec::new(),
            excluded_patterns: Vec::new(),
            warnings: Vec::new(),
            file_targets: None,
            fix_commits_total: 0,
            window_authors: 1,
        }
    }

    #[test]
    fn test_map_trailer_counts_reasons_in_the_shown_prefix_only() {
        let reasons = [Some("language"), None, Some("language"), Some("read error")];
        assert_eq!(
            trailer_line(4, 4, &reasons).as_deref(),
            Some("[skim] 3 of 4 files; language: 2 — adjust the map budget with --tokens\n")
        );
        assert_eq!(
            trailer_line(5, 4, &reasons).as_deref(),
            Some(
                "[skim] 4 of 4 files; language: 2; read error: 1 — adjust the map budget with --tokens\n"
            )
        );
    }

    #[test]
    fn test_full_map_has_no_trailer() {
        let text = render(&fixture(), &SPEC, 1_000, &|s| Some(s.len()));
        assert_eq!(
            text,
            concat!(
                "Heatmap map: demo 2025-01-01..2025-01-02, 5 commits, time window\n",
                "src/large.rs: 1 commit, 50 lines, score 50\n",
                "src/small.rs: 4 commits, 2 lines, score 8\n",
                "src/tiny.rs: 1 commit, 1 line, score 1\n",
            )
        );
    }

    #[test]
    fn test_empty_map_prints_none_below_the_warning() {
        let mut result = fixture();
        result.files.clear();
        result.warnings.push("Shallow clone detected.".into());
        let text = render(&result, &SPEC, 1_000, &|s| Some(s.len()));
        assert!(text.ends_with("warning: Shallow clone detected.\n(none)\n"));
    }

    #[test]
    fn test_map_shows_only_the_first_partner_and_filters_control_chars() {
        let mut result = fixture();
        result.files[1].path = "src/large\nfile.rs".into();
        result.files[1].blast_radius = ["src/partner\u{1b}.rs", "src/second.rs"]
            .map(|path| CouplingEntry {
                path: path.into(),
                confidence: 0.9,
                support: 4,
            })
            .to_vec();
        let text = render(&result, &SPEC, 1_000, &|s| Some(s.len()));
        assert!(text.contains(
            "src/large�file.rs: 1 commit, 50 lines, score 50\n  changes with: src/partner�.rs\n"
        ));
        assert!(!text.contains("src/second.rs"));
    }

    #[test]
    fn test_cut_map_trailer_counts_candidates_and_is_the_last_line() {
        let result = fixture();
        let budget = 174;
        let text = render(&result, &SPEC, budget, &|s| Some(s.len()));
        assert!(text.len() <= budget);
        assert!(text.contains("src/large.rs:"));
        assert!(!text.contains("src/small.rs:"));
        assert_eq!(
            text.lines().last(),
            Some("[skim] 1 of 3 files — adjust the map budget with --tokens")
        );
    }

    #[test]
    fn test_piece_count_failure_discards_all_map_content() {
        let count = |s: &str| (!s.starts_with("src/small.rs:")).then_some(s.len());
        let text = render(&fixture(), &SPEC, 1_000, &count);
        assert_eq!(
            text,
            "[skim] 0 of 3 files — adjust the map budget with --tokens\n"
        );
    }

    #[test]
    fn test_exact_join_count_failure_discards_all_map_content() {
        let count = |s: &str| {
            (!s.starts_with("Heatmap map:") || !s.contains("src/large.rs:"))
                .then_some(s.len().div_ceil(4))
        };
        let text = render(&fixture(), &SPEC, 100, &count);
        assert_eq!(
            text,
            "[skim] 0 of 3 files — adjust the map budget with --tokens\n"
        );
    }

    #[test]
    fn test_map_stops_counting_at_twice_the_budget_but_counts_all_omissions() {
        let mut result = fixture();
        result
            .files
            .extend((0..10).map(|n| file(&format!("src/{n}.rs"), 1, 1)));
        result.files.push(file("src/sentinel.rs", 1, 0));
        let count = |s: &str| {
            assert!(!s.contains("src/sentinel.rs"));
            Some(s.len())
        };
        let text = render(&result, &SPEC, 174, &count);
        assert_eq!(
            text.lines().last(),
            Some("[skim] 1 of 14 files — adjust the map budget with --tokens")
        );
    }
}
