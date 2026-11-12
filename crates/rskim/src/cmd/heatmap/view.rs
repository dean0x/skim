//! Ranked view of a heatmap result: the sections that the text and JSON
//! outputs show, each one sorted, gated and capped.
//!
//! Pure functions over a [`HeatmapResult`], no I/O. [`build_view`] owns every
//! row order, the code-to-code coupling filter, the fix-risk lift gate, the
//! bus-factor gate and the row caps, so a renderer sorts, filters and caps
//! nothing. The gates themselves live in `noise.rs`.

use std::cmp::Ordering;
use std::path::Path;

use serde::Serialize;

use super::card::Card;
use super::metrics::edge_order;
use super::noise;
use super::types::{
    AuthorMetrics, ChurnMetrics, Count, CouplingEdge, CouplingEntry, DetailSection, FileMetrics,
    FixRiskMetrics, HeatmapConfig, HeatmapResult, ModuleHealth, WindowInfo,
};

// ============================================================================
// Caps
// ============================================================================

/// Hotspot rows in the default view.
const HOTSPOTS_DEFAULT: usize = 10;
/// Coupling rows in the default view.
const COUPLING_DEFAULT: usize = 5;
/// Fix-risk rows in the default view.
const FIX_RISK_DEFAULT: usize = 5;
/// Rows of the section that `--detail` names, when `--top` is not given.
const DETAIL_DEFAULT: usize = 25;
/// Insight rows.
const INSIGHTS_DEFAULT: usize = 20;
/// Cards, one for each file argument.
const CARDS_DEFAULT: usize = 5;
/// Coupling partners on one card.
const PARTNERS_DEFAULT: usize = 4;
/// Fix commits on one card.
const FIX_COMMITS_DEFAULT: usize = 3;
/// Fix commits on one card at any `--top`, so that a card stays short.
const FIX_COMMITS_MAX: usize = 5;
/// Owners on one card.
const OWNERS_DEFAULT: usize = 3;

/// Warning lines in a text view. With more warnings, the last of these
/// lines counts the warnings that it does not show.
const WARNING_LINES: usize = 2;

/// Coupling partners of one `files[]` entry in the heatmap JSON.
const BLAST_RADIUS_JSON_MAX: usize = 5;

/// Entries of `file_targets[]` in the heatmap JSON, in argument order.
/// `counts.file_targets` gives the full count.
const FILE_TARGETS_JSON_MAX: usize = 20;

/// Version of both heatmap JSON shapes: [`HeatmapJson`] and the insights
/// JSON.
pub(crate) const JSON_VERSION: u8 = 2;

/// Rows of `section` in the default view. Zero means that the section is not
/// in the default view: module health and bus factor show only with
/// `--detail`.
fn default_rows(section: DetailSection) -> usize {
    match section {
        DetailSection::Hotspots => HOTSPOTS_DEFAULT,
        DetailSection::Coupling => COUPLING_DEFAULT,
        DetailSection::FixRisk => FIX_RISK_DEFAULT,
        DetailSection::Modules | DetailSection::BusFactor => 0,
    }
}

// ============================================================================
// View spec
// ============================================================================

/// What a view shows: the section that `--detail` names, the `--top` value
/// when it was given, and whether `--include-docs` keeps the non-code paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ViewSpec {
    pub(crate) detail: Option<DetailSection>,
    /// `Some` only when `--top` was given. Without it, each list has its own
    /// default cap.
    pub(crate) top: Option<usize>,
    /// True with `--include-docs`: the hotspots, the modules and the insights
    /// keep the paths that [`noise::is_non_code_path`] names.
    pub(crate) include_docs: bool,
}

/// Caps of the lists outside the five sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ListCaps {
    pub(crate) insights: usize,
    pub(crate) cards: usize,
    pub(crate) partners: usize,
    /// Never above [`FIX_COMMITS_MAX`], also with a larger `--top`.
    pub(crate) fix_commits: usize,
    pub(crate) owners: usize,
}

impl ListCaps {
    /// True when the fix-commit cap is at its maximum, so a larger `--top`
    /// shows no more fix commits.
    #[must_use]
    pub(crate) fn fix_commits_at_max(self) -> bool {
        self.fix_commits >= FIX_COMMITS_MAX
    }
}

impl ViewSpec {
    /// The spec of `config`. The `--top` value counts only when it was given.
    #[must_use]
    pub(crate) fn from_config(config: &HeatmapConfig) -> Self {
        Self {
            detail: config.detail,
            top: config.top_explicit.then_some(config.top_n),
            include_docs: config.include_docs,
        }
    }

    /// True when `path` stays out of the hotspots, the modules and the
    /// insights of this view.
    #[must_use]
    pub(crate) fn leaves_out(self, path: &str) -> bool {
        !self.include_docs && noise::is_non_code_path(path)
    }

    /// Rows of `section` in this view.
    ///
    /// The section that `--detail` names shows [`DETAIL_DEFAULT`] rows, or
    /// `--top` rows. The other sections of the default view keep their caps,
    /// so that JSON `files[]` is never empty; a section that is not in the
    /// default view gets no rows unless `--detail` names it.
    #[must_use]
    pub(crate) fn cap(self, section: DetailSection) -> usize {
        if self.detail == Some(section) {
            return self.top.unwrap_or(DETAIL_DEFAULT);
        }
        match default_rows(section) {
            0 => 0,
            rows => self.top.unwrap_or(rows),
        }
    }

    /// True when the text view shows `section`: in a `--detail` view only the
    /// named section, else hotspots, coupling and fix risk.
    #[must_use]
    pub(crate) fn shows(self, section: DetailSection) -> bool {
        match self.detail {
            Some(named) => named == section,
            None => default_rows(section) > 0,
        }
    }

    /// The caps of the insights and the card lists.
    #[must_use]
    pub(crate) fn lists(self) -> ListCaps {
        let cap = |default: usize| self.top.unwrap_or(default);
        ListCaps {
            insights: cap(INSIGHTS_DEFAULT),
            cards: cap(CARDS_DEFAULT),
            partners: cap(PARTNERS_DEFAULT),
            fix_commits: cap(FIX_COMMITS_DEFAULT).min(FIX_COMMITS_MAX),
            owners: cap(OWNERS_DEFAULT),
        }
    }

    /// The flag that shows the hidden rows of `section`: `--detail` names the
    /// section in the default view; in a `--detail` view only `--top` is left.
    fn remedy(self, section: DetailSection) -> MoreRemedy {
        match self.detail {
            Some(_) => MoreRemedy::Top,
            None => MoreRemedy::Detail(section),
        }
    }
}

// ============================================================================
// "More" line
// ============================================================================

/// The flag that a "more" line names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MoreRemedy {
    /// `--detail <section>`: the default view.
    Detail(DetailSection),
    /// `--top`: a `--detail` view, the insights and the cards.
    Top,
}

/// The line that tells how many rows are hidden and which flag shows them,
/// or `None` when no row is hidden.
#[must_use]
pub(crate) fn more_line(hidden: usize, remedy: MoreRemedy) -> Option<String> {
    if hidden == 0 {
        return None;
    }
    Some(match remedy {
        MoreRemedy::Detail(section) => format!("{hidden} more (--detail {})", section.name()),
        MoreRemedy::Top => format!("{hidden} more (raise --top)"),
    })
}

/// The "more" line with the count of rows that the non-code rule left out
/// added, or `None` when no row is hidden or left out. `noun` names the
/// left-out rows (for example "doc and non-code files").
#[must_use]
pub(crate) fn more_and_left_out_line(
    hidden: usize,
    remedy: MoreRemedy,
    left_out: usize,
    noun: &str,
) -> Option<String> {
    let left = (left_out > 0).then(|| format!("{left_out} {noun} left out (--include-docs)"));
    match (more_line(hidden, remedy), left) {
        (Some(more), Some(left)) => Some(format!("{more}; {left}")),
        (more, left) => more.or(left),
    }
}

/// The line of the default text view that points to the per-file card.
pub(crate) const CARD_HINT: &str = "Before you edit a file: skim heatmap <file>";

/// The warnings that a text view prints in full, and the line that counts
/// the others, or `None` when every warning fits in [`WARNING_LINES`] lines.
#[must_use]
pub(crate) fn text_warnings(warnings: &[String]) -> (&[String], Option<String>) {
    if warnings.len() <= WARNING_LINES {
        return (warnings, None);
    }
    let (shown, hidden) = warnings.split_at(WARNING_LINES - 1);
    (
        shown,
        Some(format!("{} more warnings (--json)", hidden.len())),
    )
}

/// The repository name that every output gives (the text header and both
/// JSON shapes): the last component of the root path, never the path. A
/// root with no name (`/`) or a name that is not UTF-8 gives `repository`.
#[must_use]
pub(crate) fn repo_name(repository: &str) -> &str {
    Path::new(repository)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repository")
}

// ============================================================================
// Sections and rows
// ============================================================================

/// The [`Count`] of each section, of the cards when the run has file
/// arguments, and of the file targets when the run is scoped to files, for
/// JSON `counts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct SectionCounts {
    pub(crate) hotspots: Count,
    pub(crate) coupling: Count,
    pub(crate) fix_risk: Count,
    pub(crate) modules: Count,
    pub(crate) bus_factor: Count,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cards: Option<Count>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) file_targets: Option<Count>,
}

/// One section: the shown rows in order, the count of rows that passed the
/// gates before the cap, and the count of rows that the non-code rule left
/// out before the gates.
#[derive(Debug)]
pub(crate) struct Section<T> {
    pub(crate) rows: Vec<T>,
    pub(crate) total: usize,
    pub(crate) left_out: usize,
}

impl<T> Section<T> {
    /// Sort `rows` by `order`, then keep the first `cap`.
    fn ranked(mut rows: Vec<T>, order: impl FnMut(&T, &T) -> Ordering, cap: usize) -> Self {
        let total = rows.len();
        rows.sort_by(order);
        rows.truncate(cap);
        Self {
            rows,
            total,
            left_out: 0,
        }
    }

    /// This section with `left_out` rows left out by the non-code rule.
    fn with_left_out(self, left_out: usize) -> Self {
        Self { left_out, ..self }
    }

    #[must_use]
    pub(crate) fn count(&self) -> Count {
        Count {
            total: self.total,
            shown: self.rows.len(),
        }
    }
}

/// A file above the fix-risk base rate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub(crate) struct FixRiskRow<'a> {
    pub(crate) path: &'a str,
    pub(crate) fix_commits: usize,
    /// Commits that touch the file.
    pub(crate) support: usize,
    /// The fix rate of the file divided by the base rate.
    pub(crate) lift: f64,
    /// Wilson lower bound (z = 1.96) of the fix rate of the file.
    pub(crate) lower_bound: f64,
}

/// A file with one dominant author.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub(crate) struct BusFactorRow<'a> {
    pub(crate) path: &'a str,
    pub(crate) top_author_pct: f64,
    /// Commits that touch the file.
    pub(crate) support: usize,
    /// Authors with more than 5% of the commits of the file.
    pub(crate) authors: usize,
}

// ============================================================================
// View
// ============================================================================

/// The ranked sections of one heatmap result.
#[derive(Debug)]
pub(crate) struct HeatmapView<'a> {
    pub(crate) spec: ViewSpec,
    /// Fix commits divided by all commits in the window, `0.0..=1.0`.
    pub(crate) base_rate: f64,
    pub(crate) hotspots: Section<&'a FileMetrics>,
    pub(crate) coupling: Section<&'a CouplingEdge>,
    pub(crate) fix_risk: Section<FixRiskRow<'a>>,
    pub(crate) modules: Section<&'a ModuleHealth>,
    pub(crate) bus_factor: Section<BusFactorRow<'a>>,
}

/// Build the ranked view of `result` for `spec`.
#[must_use]
pub(crate) fn build_view<'a>(result: &'a HeatmapResult, spec: &ViewSpec) -> HeatmapView<'a> {
    let base_rate = noise::base_rate(result.fix_commits_total, result.window.commits_analyzed);

    let hotspots: Vec<&FileMetrics> = result
        .files
        .iter()
        .filter(|f| !spec.leaves_out(&f.path))
        .collect();
    let hotspots_left_out = result.files.len() - hotspots.len();

    let coupling: Vec<&CouplingEdge> = result
        .coupling_graph
        .iter()
        .filter(|e| !noise::is_doc_path(&e.a) && !noise::is_doc_path(&e.b))
        .collect();

    let fix_risk: Vec<FixRiskRow<'_>> = result
        .files
        .iter()
        .filter(|f| noise::passes_lift(f.fix_risk.fix_commits, f.churn.commits, base_rate))
        .map(|f| FixRiskRow {
            path: &f.path,
            fix_commits: f.fix_risk.fix_commits,
            support: f.churn.commits,
            lift: noise::lift(f.fix_risk.fix_commits, f.churn.commits, base_rate),
            lower_bound: noise::wilson_lower_bound(f.fix_risk.fix_commits, f.churn.commits),
        })
        .collect();

    let modules: Vec<&ModuleHealth> = result
        .modules
        .iter()
        .filter(|m| !spec.leaves_out(&m.path))
        .collect();
    let modules_left_out = result.modules.len() - modules.len();

    let bus_factor: Vec<BusFactorRow<'_>> = result
        .files
        .iter()
        .filter(|f| {
            noise::passes_bus_gate(
                result.window_authors,
                f.churn.commits,
                f.authors.top_author_pct,
            )
        })
        .map(|f| BusFactorRow {
            path: &f.path,
            top_author_pct: f.authors.top_author_pct,
            support: f.churn.commits,
            authors: f.authors.count,
        })
        .collect();

    HeatmapView {
        spec: *spec,
        base_rate,
        hotspots: Section::ranked(hotspots, hotspot_order, spec.cap(DetailSection::Hotspots))
            .with_left_out(hotspots_left_out),
        coupling: Section::ranked(coupling, coupling_order, spec.cap(DetailSection::Coupling)),
        fix_risk: Section::ranked(fix_risk, fix_risk_order, spec.cap(DetailSection::FixRisk)),
        modules: Section::ranked(modules, module_order, spec.cap(DetailSection::Modules))
            .with_left_out(modules_left_out),
        bus_factor: Section::ranked(
            bus_factor,
            bus_factor_order,
            spec.cap(DetailSection::BusFactor),
        ),
    }
}

// Row orders. Each one ends on the path, so the order is total and two runs
// give the same rows.

/// Hotspot order: commits desc, then path asc.
fn hotspot_order(x: &&FileMetrics, y: &&FileMetrics) -> Ordering {
    y.churn
        .commits
        .cmp(&x.churn.commits)
        .then_with(|| x.path.cmp(&y.path))
}

/// Coupling order: the order of the coupling graph (confidence desc, then
/// support desc, then the paths asc).
fn coupling_order(x: &&CouplingEdge, y: &&CouplingEdge) -> Ordering {
    edge_order(x, y)
}

/// Fix-risk order: Wilson lower bound desc, then support desc, then path asc.
fn fix_risk_order(x: &FixRiskRow<'_>, y: &FixRiskRow<'_>) -> Ordering {
    y.lower_bound
        .total_cmp(&x.lower_bound)
        .then_with(|| y.support.cmp(&x.support))
        .then_with(|| x.path.cmp(y.path))
}

/// Module order: encapsulation asc (worst first), then commits desc, then
/// path asc.
fn module_order(x: &&ModuleHealth, y: &&ModuleHealth) -> Ordering {
    x.encapsulation_pct
        .total_cmp(&y.encapsulation_pct)
        .then_with(|| y.total_commits.cmp(&x.total_commits))
        .then_with(|| x.path.cmp(&y.path))
}

/// Bus-factor order: top-author share desc, then support desc, then path asc.
fn bus_factor_order(x: &BusFactorRow<'_>, y: &BusFactorRow<'_>) -> Ordering {
    y.top_author_pct
        .total_cmp(&x.top_author_pct)
        .then_with(|| y.support.cmp(&x.support))
        .then_with(|| x.path.cmp(y.path))
}

impl<'a> HeatmapView<'a> {
    /// The count of `section`.
    #[must_use]
    pub(crate) fn count(&self, section: DetailSection) -> Count {
        match section {
            DetailSection::Hotspots => self.hotspots.count(),
            DetailSection::Coupling => self.coupling.count(),
            DetailSection::FixRisk => self.fix_risk.count(),
            DetailSection::Modules => self.modules.count(),
            DetailSection::BusFactor => self.bus_factor.count(),
        }
    }

    /// The "more" line of `section`, or `None` when no row of it is hidden
    /// or left out. The line also counts the rows that the non-code rule
    /// left out, and names `--include-docs`.
    #[must_use]
    pub(crate) fn more(&self, section: DetailSection) -> Option<String> {
        let (left_out, noun) = match section {
            DetailSection::Hotspots => (self.hotspots.left_out, "doc and non-code files"),
            DetailSection::Modules => (self.modules.left_out, "non-code folders"),
            DetailSection::Coupling | DetailSection::FixRisk | DetailSection::BusFactor => (0, ""),
        };
        more_and_left_out_line(
            self.count(section).hidden(),
            self.spec.remedy(section),
            left_out,
            noun,
        )
    }

    /// The JSON v2 shape of this view, with the cards of a run with file
    /// arguments.
    #[must_use]
    pub(crate) fn to_json<'v>(
        &'v self,
        result: &'v HeatmapResult,
        cards: Option<&'v Section<Card>>,
    ) -> HeatmapJson<'v> {
        let targets = result.file_targets.as_deref();
        let shown_targets = targets.map(|t| t.get(..FILE_TARGETS_JSON_MAX).unwrap_or(t));
        HeatmapJson {
            version: JSON_VERSION,
            generated_at: &result.generated_at,
            repository: repo_name(&result.repository),
            window: &result.window,
            base_rate: self.base_rate,
            counts: SectionCounts {
                hotspots: self.hotspots.count(),
                coupling: self.coupling.count(),
                fix_risk: self.fix_risk.count(),
                modules: self.modules.count(),
                bus_factor: self.bus_factor.count(),
                cards: cards.map(Section::count),
                file_targets: targets.zip(shown_targets).map(|(all, shown)| Count {
                    total: all.len(),
                    shown: shown.len(),
                }),
            },
            files: self.hotspots.rows.iter().map(|f| FileJson::of(f)).collect(),
            coupling_graph: &self.coupling.rows,
            fix_risk: &self.fix_risk.rows,
            modules: &self.modules.rows,
            bus_factor: &self.bus_factor.rows,
            left_out: LeftOutJson::of(self),
            excluded_patterns: &result.excluded_patterns,
            warnings: &result.warnings,
            file_targets: shown_targets,
            cards: cards.map(|c| c.rows.as_slice()),
        }
    }
}

// ============================================================================
// JSON v2
// ============================================================================

/// The heatmap JSON, version 2: the sections of the view and their counts.
///
/// `files` holds the hotspot rows, with the nested `churn` and `fix_risk`
/// objects. Any file can be a hotspot: `files[]` is never filtered by
/// support. Every array of rows has the cap of its section, and
/// `file_targets` has [`FILE_TARGETS_JSON_MAX`]. `warnings` holds at most one
/// line for each kind of warning. `excluded_patterns` is the fixed pattern
/// list: the default list and the `--exclude` values.
#[derive(Debug, Serialize)]
pub(crate) struct HeatmapJson<'a> {
    version: u8,
    generated_at: &'a str,
    /// The repository name ([`repo_name`]), the same as the text header.
    repository: &'a str,
    window: &'a WindowInfo,
    base_rate: f64,
    counts: SectionCounts,
    files: Vec<FileJson<'a>>,
    coupling_graph: &'a [&'a CouplingEdge],
    fix_risk: &'a [FixRiskRow<'a>],
    modules: &'a [&'a ModuleHealth],
    bus_factor: &'a [BusFactorRow<'a>],
    /// Rows that the non-code rule left out, when any (see `--include-docs`).
    #[serde(skip_serializing_if = "Option::is_none")]
    left_out: Option<LeftOutJson>,
    excluded_patterns: &'a [String],
    warnings: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    file_targets: Option<&'a [String]>,
    /// One card per shown file argument, in argument order.
    #[serde(skip_serializing_if = "Option::is_none")]
    cards: Option<&'a [Card]>,
}

/// The rows that the non-code rule left out of the hotspots (`files[]`) and
/// the modules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct LeftOutJson {
    hotspots: usize,
    modules: usize,
}

impl LeftOutJson {
    /// The counts of `view`, or `None` when the rule left out no row.
    fn of(view: &HeatmapView<'_>) -> Option<Self> {
        let counts = Self {
            hotspots: view.hotspots.left_out,
            modules: view.modules.left_out,
        };
        (counts.hotspots + counts.modules > 0).then_some(counts)
    }
}

/// One `files[]` entry: the keys of [`FileMetrics`], with the blast radius
/// cut to its [`BLAST_RADIUS_JSON_MAX`] strongest partners.
#[derive(Debug, Serialize)]
pub(crate) struct FileJson<'a> {
    path: &'a str,
    churn: &'a ChurnMetrics,
    stability_score: u8,
    authors: &'a AuthorMetrics,
    fix_risk: &'a FixRiskMetrics,
    blast_radius: &'a [CouplingEntry],
}

impl<'a> FileJson<'a> {
    /// The entry of `file`. The blast radius is already in rank order
    /// (confidence desc, then support desc, then path asc).
    fn of(file: &'a FileMetrics) -> Self {
        let partners = &file.blast_radius;
        Self {
            path: &file.path,
            churn: &file.churn,
            stability_score: file.stability_score,
            authors: &file.authors,
            fix_risk: &file.fix_risk,
            blast_radius: partners.get(..BLAST_RADIUS_JSON_MAX).unwrap_or(partners),
        }
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    const DEFAULT_VIEW: ViewSpec = ViewSpec {
        detail: None,
        top: None,
        include_docs: false,
    };

    // -----------------------------------------------------------------------
    // Builders
    // -----------------------------------------------------------------------

    /// A result with no rows: 20 commits in the window, 4 of them fix
    /// commits (base rate 20%), 2 distinct authors.
    fn empty_result() -> HeatmapResult {
        HeatmapResult {
            generated_at: "2025-01-01T00:00:00Z".to_string(),
            repository: "repo".to_string(),
            window: WindowInfo {
                mode: "dual".to_string(),
                since: "2024-10-01".to_string(),
                until: "2025-01-01".to_string(),
                commits_analyzed: 20,
                effective_strategy: None,
            },
            files: vec![],
            modules: vec![],
            coupling_graph: vec![],
            excluded_patterns: vec![],
            warnings: vec![],
            file_targets: None,
            fix_commits_total: 4,
            window_authors: 2,
        }
    }

    /// A file with `commits` commits, no fix commit, one author with 50%.
    fn file(path: &str, commits: usize) -> FileMetrics {
        FileMetrics {
            path: path.to_string(),
            churn: ChurnMetrics { commits, rate: 0.0 },
            stability_score: 50,
            authors: AuthorMetrics {
                count: 2,
                top_author_pct: 50.0,
                single_owner_risk: false,
            },
            fix_risk: FixRiskMetrics {
                insufficient_data: commits < noise::MIN_FILE_SUPPORT,
                ..FixRiskMetrics::default()
            },
            blast_radius: vec![],
        }
    }

    fn with_fixes(mut f: FileMetrics, fix_commits: usize) -> FileMetrics {
        f.fix_risk.fix_commits = fix_commits;
        f
    }

    fn owned(mut f: FileMetrics, top_author_pct: f64) -> FileMetrics {
        f.authors.top_author_pct = top_author_pct;
        f
    }

    fn edge(a: &str, b: &str, confidence: f64, support: usize) -> CouplingEdge {
        CouplingEdge {
            a: a.to_string(),
            b: b.to_string(),
            confidence,
            support,
        }
    }

    fn module(path: &str, encapsulation_pct: f64, total_commits: usize) -> ModuleHealth {
        ModuleHealth {
            path: path.to_string(),
            encapsulation_pct,
            files_count: 2,
            total_commits,
            cross_boundary_commits: 1,
        }
    }

    /// 12 files `src/fNN.rs` with 5 to 16 commits; the 6 even ones have only
    /// fix commits (above the base rate); every file has a 100% top author.
    /// 6 code-to-code pairs and 4 modules.
    fn busy_result() -> HeatmapResult {
        let mut result = empty_result();
        result.window.commits_analyzed = 100;
        result.fix_commits_total = 20;
        result.files = (1..=12)
            .map(|i| {
                let commits = i + 4;
                let fixes = if i % 2 == 0 { commits } else { 0 };
                owned(
                    with_fixes(file(&format!("src/f{i:02}.rs"), commits), fixes),
                    100.0,
                )
            })
            .collect();
        result.coupling_graph = (1..=6)
            .map(|i| edge(&format!("src/a{i}.rs"), &format!("src/b{i}.rs"), 0.9, 3))
            .collect();
        result.modules = (1..=4)
            .map(|i| module(&format!("crates/m{i}"), 50.0, 3 + i))
            .collect();
        result
    }

    fn detail_view(section: DetailSection, top: Option<usize>) -> ViewSpec {
        ViewSpec {
            detail: Some(section),
            top,
            include_docs: false,
        }
    }

    fn top_view(top: usize) -> ViewSpec {
        ViewSpec {
            detail: None,
            top: Some(top),
            include_docs: false,
        }
    }

    /// `(shown, total)` of each section, in `DetailSection::ALL` order.
    fn shown_and_total(view: &HeatmapView<'_>) -> [(usize, usize); 5] {
        DetailSection::ALL.map(|s| {
            let c = view.count(s);
            (c.shown, c.total)
        })
    }

    fn hotspot_paths<'a>(view: &HeatmapView<'a>) -> Vec<&'a str> {
        view.hotspots.rows.iter().map(|f| f.path.as_str()).collect()
    }

    // -----------------------------------------------------------------------
    // Hotspots
    // -----------------------------------------------------------------------

    #[test]
    fn test_hotspots_are_the_ten_highest_churn_files_in_order() {
        let mut result = empty_result();
        // Lowest churn first, so that the first ten in input order are wrong.
        result.files = (1..=12)
            .map(|i| file(&format!("src/f{i:02}.rs"), i))
            .collect();
        let view = build_view(&result, &DEFAULT_VIEW);
        let expected: Vec<String> = (3..=12).rev().map(|i| format!("src/f{i:02}.rs")).collect();
        assert_eq!(hotspot_paths(&view), expected);
        assert_eq!(view.hotspots.total, 12);
    }

    #[test]
    fn test_hotspots_tie_on_commits_sorts_path_ascending() {
        let mut result = empty_result();
        result.files = vec![
            file("src/b.rs", 3),
            file("src/a.rs", 3),
            file("src/c.rs", 5),
        ];
        let view = build_view(&result, &DEFAULT_VIEW);
        assert_eq!(hotspot_paths(&view), ["src/c.rs", "src/a.rs", "src/b.rs"]);
    }

    #[test]
    fn test_hotspots_keep_high_churn_file_with_high_stability_score() {
        let mut result = empty_result();
        result.files = (1..=11)
            .map(|i| {
                let mut f = file(&format!("src/hot{i:02}.rs"), i);
                f.stability_score = 10;
                f
            })
            .collect();
        // Old commits only: a high stability score, but the most churn.
        let mut old_busy = file("src/old_busy.rs", 40);
        old_busy.stability_score = 95;
        result.files.push(old_busy);
        let view = build_view(&result, &DEFAULT_VIEW);
        assert_eq!(hotspot_paths(&view).first(), Some(&"src/old_busy.rs"));
    }

    // -----------------------------------------------------------------------
    // Caps, --top and --detail
    // -----------------------------------------------------------------------

    #[test]
    fn test_default_view_caps_hotspots_coupling_and_fix_risk() {
        let result = busy_result();
        let view = build_view(&result, &DEFAULT_VIEW);
        // hotspots, coupling, fix-risk, modules, bus-factor
        assert_eq!(
            shown_and_total(&view),
            [(10, 12), (5, 6), (5, 6), (0, 4), (0, 12)]
        );
    }

    #[test]
    fn test_top_gives_exact_rows_for_each_section() {
        let result = busy_result();
        let view = build_view(&result, &top_view(3));
        assert_eq!(
            shown_and_total(&view),
            [(3, 12), (3, 6), (3, 6), (0, 4), (0, 12)]
        );
        let all = build_view(&result, &top_view(50));
        assert_eq!(
            shown_and_total(&all),
            [(12, 12), (6, 6), (6, 6), (0, 4), (0, 12)]
        );
    }

    #[test]
    fn test_detail_section_shows_all_its_rows_unless_top_is_given() {
        let result = busy_result();
        let coupling = build_view(&result, &detail_view(DetailSection::Coupling, None));
        assert_eq!(
            shown_and_total(&coupling),
            [(10, 12), (6, 6), (5, 6), (0, 4), (0, 12)]
        );
        let modules = build_view(&result, &detail_view(DetailSection::Modules, None));
        assert_eq!(modules.count(DetailSection::Modules).shown, 4);
        let bus = build_view(&result, &detail_view(DetailSection::BusFactor, None));
        assert_eq!(bus.count(DetailSection::BusFactor).shown, 12);
        let capped = build_view(&result, &detail_view(DetailSection::BusFactor, Some(2)));
        assert_eq!(capped.count(DetailSection::BusFactor).shown, 2);
    }

    #[test]
    fn test_detail_section_caps_at_25_rows_and_top_raises_the_cap() {
        let mut result = empty_result();
        result.files = (1..=40)
            .map(|i| file(&format!("src/f{i:02}.rs"), i))
            .collect();
        let detail = build_view(&result, &detail_view(DetailSection::Hotspots, None));
        assert_eq!(detail.count(DetailSection::Hotspots).shown, 25);
        assert_eq!(detail.count(DetailSection::Hotspots).total, 40);
        assert_eq!(
            detail.more(DetailSection::Hotspots).as_deref(),
            Some("15 more (raise --top)")
        );
        let raised = build_view(&result, &detail_view(DetailSection::Hotspots, Some(40)));
        assert_eq!(raised.count(DetailSection::Hotspots).shown, 40);
        assert_eq!(raised.more(DetailSection::Hotspots), None);
    }

    #[test]
    fn test_hotspots_leave_out_doc_and_non_code_files_and_count_them() {
        let mut result = empty_result();
        result.files = vec![
            file("CLAUDE.md", 60),
            file("README.md", 50),
            file(".github/workflows/ci.yml", 40),
            file("src/main.rs", 30),
            file("crates/x/tests/cli.rs", 20),
        ];
        let view = build_view(&result, &DEFAULT_VIEW);
        assert_eq!(
            hotspot_paths(&view),
            ["src/main.rs", "crates/x/tests/cli.rs"]
        );
        assert_eq!(view.hotspots.left_out, 3);
        assert_eq!(
            view.more(DetailSection::Hotspots).as_deref(),
            Some("3 doc and non-code files left out (--include-docs)")
        );

        let all = ViewSpec {
            include_docs: true,
            ..DEFAULT_VIEW
        };
        let kept = build_view(&result, &all);
        assert_eq!(hotspot_paths(&kept)[..2], ["CLAUDE.md", "README.md"]);
        assert_eq!(kept.hotspots.left_out, 0);
        assert_eq!(kept.more(DetailSection::Hotspots), None);
    }

    #[test]
    fn test_more_line_joins_hidden_and_left_out_counts() {
        let mut result = empty_result();
        result.files = (1..=12)
            .map(|i| file(&format!("src/f{i:02}.rs"), i))
            .chain([file("docs/guide.md", 99)])
            .collect();
        let view = build_view(&result, &DEFAULT_VIEW);
        assert_eq!(
            view.more(DetailSection::Hotspots).as_deref(),
            Some("2 more (--detail hotspots); 1 doc and non-code files left out (--include-docs)")
        );
    }

    #[test]
    fn test_modules_leave_out_non_code_folders_unless_include_docs() {
        let mut result = empty_result();
        result.modules = vec![
            module("docs", 5.0, 15),
            module(".devflow", 8.0, 36),
            module("tests", 0.0, 9),
            module("crates/core", 10.0, 57),
            module("plugins", 57.0, 7),
        ];
        let spec = detail_view(DetailSection::Modules, None);
        let view = build_view(&result, &spec);
        let paths: Vec<&str> = view.modules.rows.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["crates/core", "plugins"]);
        assert_eq!(
            view.more(DetailSection::Modules).as_deref(),
            Some("3 non-code folders left out (--include-docs)")
        );
        let all = ViewSpec {
            include_docs: true,
            ..spec
        };
        assert_eq!(build_view(&result, &all).modules.rows.len(), 5);
    }

    #[test]
    fn test_json_left_out_counts_only_when_rows_are_left_out() {
        let mut result = empty_result();
        result.files = vec![file("README.md", 9), file("src/a.rs", 5)];
        result.modules = vec![module("docs", 5.0, 15)];
        let view = build_view(&result, &DEFAULT_VIEW);
        let json = serde_json::to_value(view.to_json(&result, None)).unwrap();
        assert_eq!(json["left_out"]["hotspots"], 1);
        assert_eq!(json["left_out"]["modules"], 1);
        assert_eq!(json["counts"]["hotspots"]["total"], 1);

        let all = ViewSpec {
            include_docs: true,
            ..DEFAULT_VIEW
        };
        let kept = build_view(&result, &all);
        let json = serde_json::to_value(kept.to_json(&result, None)).unwrap();
        assert!(json.get("left_out").is_none(), "{json}");
    }

    #[test]
    fn test_text_shows_default_sections_or_only_the_detail_section() {
        let shown_in = |spec: ViewSpec| DetailSection::ALL.map(|s| spec.shows(s));
        assert_eq!(shown_in(DEFAULT_VIEW), [true, true, true, false, false]);
        assert_eq!(shown_in(top_view(3)), [true, true, true, false, false]);
        assert_eq!(
            shown_in(detail_view(DetailSection::Modules, None)),
            [false, false, false, true, false]
        );
    }

    #[test]
    fn test_list_caps_defaults_and_top() {
        assert_eq!(
            DEFAULT_VIEW.lists(),
            ListCaps {
                insights: 20,
                cards: 5,
                partners: 4,
                fix_commits: 3,
                owners: 3,
            }
        );
        assert_eq!(
            top_view(9).lists(),
            ListCaps {
                insights: 9,
                cards: 9,
                partners: 9,
                fix_commits: 5,
                owners: 9,
            }
        );
        assert_eq!(top_view(2).lists().fix_commits, 2);
    }

    // -----------------------------------------------------------------------
    // Coupling
    // -----------------------------------------------------------------------

    #[test]
    fn test_coupling_drops_pairs_with_a_doc_endpoint() {
        let mut result = empty_result();
        result.coupling_graph = vec![
            edge("README.md", "src/a.rs", 0.95, 9),
            edge("src/a.rs", "src/b.rs", 0.7, 4),
            edge("docs/guide.rst", "docs/intro.MD", 0.9, 5),
            // git quotes a non-ASCII name; the file is still a doc file.
            edge(r#""docs/caf\303\251.md""#, "src/a.rs", 0.9, 6),
        ];
        let view = build_view(&result, &DEFAULT_VIEW);
        let pairs: Vec<(&str, &str)> = view
            .coupling
            .rows
            .iter()
            .map(|e| (e.a.as_str(), e.b.as_str()))
            .collect();
        assert_eq!(pairs, [("src/a.rs", "src/b.rs")]);
        assert_eq!(view.coupling.total, 1);
    }

    #[test]
    fn test_coupling_ranks_confidence_then_support_then_path() {
        let mut result = empty_result();
        result.coupling_graph = vec![
            edge("src/c.rs", "src/d.rs", 0.6, 3),
            edge("src/b.rs", "src/c.rs", 0.9, 3),
            edge("src/a.rs", "src/b.rs", 0.9, 3),
            edge("src/x.rs", "src/y.rs", 0.9, 7),
        ];
        let view = build_view(&result, &DEFAULT_VIEW);
        let firsts: Vec<&str> = view.coupling.rows.iter().map(|e| e.a.as_str()).collect();
        assert_eq!(firsts, ["src/x.rs", "src/a.rs", "src/b.rs", "src/c.rs"]);
    }

    // -----------------------------------------------------------------------
    // Fix risk
    // -----------------------------------------------------------------------

    #[test]
    fn test_fix_risk_needs_five_commits() {
        // Base rate 20%. 4 of 4 has a Wilson bound of 0.51, above the base
        // rate, but support 4 is below the minimum.
        let mut result = empty_result();
        result.files = vec![
            with_fixes(file("src/four.rs", 4), 4),
            with_fixes(file("src/five.rs", 5), 5),
        ];
        let view = build_view(&result, &DEFAULT_VIEW);
        let paths: Vec<&str> = view.fix_risk.rows.iter().map(|r| r.path).collect();
        assert_eq!(paths, ["src/five.rs"]);
    }

    #[test]
    fn test_fix_risk_row_values_match_hand_computed() {
        // 20 commits, 4 fix commits: base rate 0.2. The file: 5 commits, 3
        // of them fix commits. Wilson bound 0.230720 > 0.2; lift (3/5)/0.2 = 3.
        let mut result = empty_result();
        result.files = vec![with_fixes(file("src/a.rs", 5), 3)];
        let view = build_view(&result, &DEFAULT_VIEW);
        assert!((view.base_rate - 0.2).abs() < EPS, "{}", view.base_rate);
        let row = view.fix_risk.rows.first().expect("one fix-risk row");
        assert_eq!(row.path, "src/a.rs");
        assert_eq!(row.fix_commits, 3);
        assert_eq!(row.support, 5);
        assert!((row.lift - 3.0).abs() < EPS, "lift {}", row.lift);
        assert!(
            (row.lower_bound - 0.230_719_932_208_837).abs() < EPS,
            "bound {}",
            row.lower_bound
        );
    }

    #[test]
    fn test_fix_risk_has_no_rows_at_zero_or_full_base_rate() {
        for fix_commits_total in [0, 20] {
            let mut result = empty_result();
            result.fix_commits_total = fix_commits_total;
            result.files = vec![with_fixes(file("src/a.rs", 10), 10)];
            let view = build_view(&result, &DEFAULT_VIEW);
            assert_eq!(view.fix_risk.total, 0, "{fix_commits_total} fix commits");
        }
    }

    #[test]
    fn test_fix_risk_ranks_lower_bound_then_support_then_path() {
        let mut result = empty_result();
        result.files = vec![
            with_fixes(file("src/b.rs", 5), 5),
            with_fixes(file("src/a.rs", 5), 5),
            with_fixes(file("src/big.rs", 10), 10),
        ];
        let view = build_view(&result, &DEFAULT_VIEW);
        let paths: Vec<&str> = view.fix_risk.rows.iter().map(|r| r.path).collect();
        assert_eq!(paths, ["src/big.rs", "src/a.rs", "src/b.rs"]);
    }

    // -----------------------------------------------------------------------
    // Bus factor
    // -----------------------------------------------------------------------

    #[test]
    fn test_bus_factor_has_no_rows_in_a_one_author_window() {
        let mut result = empty_result();
        result.window_authors = 1;
        result.files = vec![owned(file("src/a.rs", 10), 100.0)];
        let view = build_view(&result, &detail_view(DetailSection::BusFactor, None));
        assert_eq!(view.bus_factor.total, 0);
    }

    #[test]
    fn test_bus_factor_needs_five_commits_and_a_top_share_above_80() {
        let mut result = empty_result();
        result.files = vec![
            owned(file("src/four.rs", 4), 100.0),
            owned(file("src/even.rs", 5), 80.0),
            owned(file("src/owned.rs", 5), 90.0),
        ];
        let view = build_view(&result, &detail_view(DetailSection::BusFactor, None));
        let paths: Vec<&str> = view.bus_factor.rows.iter().map(|r| r.path).collect();
        assert_eq!(paths, ["src/owned.rs"]);
        let row = view.bus_factor.rows[0];
        assert_eq!((row.support, row.authors), (5, 2));
        assert!((row.top_author_pct - 90.0).abs() < EPS);
    }

    #[test]
    fn test_bus_factor_is_not_in_the_default_view() {
        let mut result = empty_result();
        result.files = vec![owned(file("src/owned.rs", 5), 90.0)];
        let view = build_view(&result, &DEFAULT_VIEW);
        assert_eq!(
            view.count(DetailSection::BusFactor),
            Count { total: 1, shown: 0 }
        );
        assert!(!DEFAULT_VIEW.shows(DetailSection::BusFactor));
    }

    #[test]
    fn test_bus_factor_ranks_top_share_then_support_then_path() {
        let mut result = empty_result();
        result.files = vec![
            owned(file("src/b.rs", 6), 90.0),
            owned(file("src/a.rs", 6), 90.0),
            owned(file("src/c.rs", 9), 90.0),
            owned(file("src/d.rs", 5), 100.0),
        ];
        let view = build_view(&result, &detail_view(DetailSection::BusFactor, None));
        let paths: Vec<&str> = view.bus_factor.rows.iter().map(|r| r.path).collect();
        assert_eq!(paths, ["src/d.rs", "src/c.rs", "src/a.rs", "src/b.rs"]);
    }

    // -----------------------------------------------------------------------
    // Modules
    // -----------------------------------------------------------------------

    #[test]
    fn test_modules_rank_worst_encapsulation_then_support_then_path() {
        let mut result = empty_result();
        result.modules = vec![
            module("crates/b", 40.0, 5),
            module("crates/a", 40.0, 5),
            module("crates/c", 40.0, 9),
            module("crates/d", 20.0, 3),
            module("crates/e", 90.0, 30),
        ];
        let view = build_view(&result, &detail_view(DetailSection::Modules, None));
        let paths: Vec<&str> = view.modules.rows.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(
            paths,
            ["crates/d", "crates/c", "crates/a", "crates/b", "crates/e"]
        );
    }

    // -----------------------------------------------------------------------
    // "More" line
    // -----------------------------------------------------------------------

    #[test]
    fn test_more_line_names_detail_section_or_top() {
        assert_eq!(
            more_line(7, MoreRemedy::Detail(DetailSection::Coupling)).as_deref(),
            Some("7 more (--detail coupling)")
        );
        assert_eq!(
            more_line(1, MoreRemedy::Detail(DetailSection::FixRisk)).as_deref(),
            Some("1 more (--detail fix-risk)")
        );
        assert_eq!(
            more_line(12, MoreRemedy::Top).as_deref(),
            Some("12 more (raise --top)")
        );
        assert_eq!(more_line(0, MoreRemedy::Top), None);
    }

    #[test]
    fn test_view_more_names_detail_in_default_view_and_top_in_detail_view() {
        let result = busy_result();
        let default = build_view(&result, &DEFAULT_VIEW);
        assert_eq!(
            default.more(DetailSection::Hotspots).as_deref(),
            Some("2 more (--detail hotspots)")
        );
        assert_eq!(
            default.more(DetailSection::Coupling).as_deref(),
            Some("1 more (--detail coupling)")
        );
        let detail = build_view(&result, &detail_view(DetailSection::Coupling, Some(4)));
        assert_eq!(
            detail.more(DetailSection::Coupling).as_deref(),
            Some("2 more (raise --top)")
        );
        let all = build_view(&result, &detail_view(DetailSection::Coupling, None));
        assert_eq!(all.more(DetailSection::Coupling), None);
    }

    // -----------------------------------------------------------------------
    // JSON v2
    // -----------------------------------------------------------------------

    #[test]
    fn test_json_v2_has_the_kept_and_the_new_keys() {
        let result = busy_result();
        let view = build_view(&result, &DEFAULT_VIEW);
        let json = serde_json::to_value(view.to_json(&result, None)).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "base_rate",
                "bus_factor",
                "counts",
                "coupling_graph",
                "excluded_patterns",
                "files",
                "fix_risk",
                "generated_at",
                "modules",
                "repository",
                "version",
                "warnings",
                "window",
            ]
        );
        assert_eq!(json["version"], 2);
        assert!((json["base_rate"].as_f64().unwrap() - 0.2).abs() < EPS);
    }

    #[test]
    fn test_json_v2_counts_match_the_array_lengths() {
        let result = busy_result();
        let view = build_view(&result, &top_view(4));
        let json = serde_json::to_value(view.to_json(&result, None)).unwrap();
        let sections = [
            ("hotspots", "files", 4, 12),
            ("coupling", "coupling_graph", 4, 6),
            ("fix_risk", "fix_risk", 4, 6),
            ("modules", "modules", 0, 4),
            ("bus_factor", "bus_factor", 0, 12),
        ];
        for (count_key, array_key, shown, total) in sections {
            let count = &json["counts"][count_key];
            assert_eq!(count["shown"], shown, "{count_key}");
            assert_eq!(count["total"], total, "{count_key}");
            assert_eq!(
                json[array_key].as_array().unwrap().len(),
                shown,
                "{array_key}"
            );
        }
    }

    #[test]
    fn test_json_v2_caps_file_targets_and_counts_them() {
        let mut result = busy_result();
        let targets: Vec<String> = (0..FILE_TARGETS_JSON_MAX + 7)
            .map(|i| format!("src/t{i:02}.rs"))
            .collect();
        result.file_targets = Some(targets.clone());
        let view = build_view(&result, &DEFAULT_VIEW);
        let json = serde_json::to_value(view.to_json(&result, None)).unwrap();
        assert_eq!(
            json["file_targets"],
            serde_json::json!(targets[..FILE_TARGETS_JSON_MAX])
        );
        assert_eq!(
            json["counts"]["file_targets"],
            serde_json::json!({"total": FILE_TARGETS_JSON_MAX + 7, "shown": FILE_TARGETS_JSON_MAX})
        );

        result.file_targets = None;
        let view = build_view(&result, &DEFAULT_VIEW);
        let json = serde_json::to_value(view.to_json(&result, None)).unwrap();
        assert!(json.get("file_targets").is_none());
        assert!(json["counts"].get("file_targets").is_none());
    }

    #[test]
    fn test_json_v2_files_keep_raw_rates_below_support() {
        let mut result = empty_result();
        let mut single = file("src/one.rs", 1);
        single.fix_risk.keyword_pct = 100.0;
        single.fix_risk.fix_commits = 1;
        result.files = vec![single];
        let view = build_view(&result, &DEFAULT_VIEW);
        let json = serde_json::to_value(view.to_json(&result, None)).unwrap();
        let entry = &json["files"][0];
        assert_eq!(entry["path"], "src/one.rs");
        assert_eq!(entry["churn"]["commits"], 1);
        assert_eq!(entry["fix_risk"]["keyword_pct"], 100.0);
        assert_eq!(entry["fix_risk"]["insufficient_data"], true);
    }

    #[test]
    fn test_json_v2_files_keep_the_five_strongest_blast_radius_partners() {
        let mut result = empty_result();
        let mut hub = file("src/hub.rs", 9);
        // In rank order, as the coupling metric gives them.
        hub.blast_radius = (0..7)
            .map(|i| CouplingEntry {
                path: format!("src/p{i}.rs"),
                confidence: 0.9 - f64::from(i) * 0.05,
                support: 3,
            })
            .collect();
        result.files = vec![hub];
        let view = build_view(&result, &DEFAULT_VIEW);
        let json = serde_json::to_value(view.to_json(&result, None)).unwrap();
        let partners: Vec<&str> = json["files"][0]["blast_radius"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["path"].as_str().unwrap())
            .collect();
        assert_eq!(
            partners,
            [
                "src/p0.rs",
                "src/p1.rs",
                "src/p2.rs",
                "src/p3.rs",
                "src/p4.rs"
            ]
        );
    }
}
