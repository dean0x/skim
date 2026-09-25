//! Per-query HARD checks, RATCHET measurements and aggregates for the search
//! scoreboard (#203).
//!
//! - [`plan`] turns a golden file into [`PlannedQuery`]s and fixes, per
//!   entry, which HARD checks run ([`PlannedQuery::checks`]). The ledger is
//!   validated against the same plan, so a ledger entry can never name a
//!   check that does not run.
//! - The `check_*` functions are pure: skim's pages in, a
//!   [`CheckOutcome`] with a one-line detail out.
//! - [`evaluate`] scores one corpus: outcomes, per-query samples, and
//!   `unindexed_hits` (INFO). [`ratchet_values`] turns samples (one corpus,
//!   or every corpus for the aggregate) into RATCHET values.
//! - [`RATCHET_METRICS`] defines each RATCHET metric's good direction and
//!   tolerance; [`compare_ratchet`] applies them for the gate and `bless`.
//!
//! No function here imports skim's own search code: every expectation comes
//! from the oracle (`oracle.rs`) over the oracle's universe.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::scoreboard::MAX_PAGES;
use crate::scoreboard::golden::{DefSite, GoldenFile, QueryFlags};
use crate::scoreboard::oracle::{
    LexicalQuery, MatchMode, baseline_alphabetical, baseline_occurrence_count, ground_truth,
    simulate_rg_fixed,
};
use crate::scoreboard::report::round4;
use crate::scoreboard::runner::{EntryObservation, Sweep};
use crate::scoreboard::types::{
    Arm, CheckId, CheckOutcome, EntryKind, ResultPage, ResultRow, StatsSnapshot, VerifyMode,
};
use crate::scoreboard::universe::Universe;

/// Paths quoted in a failure detail before "+N more".
const SAMPLE_PATHS: usize = 5;

// ============================================================================
// Plan
// ============================================================================

/// What an entry ranks against.
#[derive(Debug, Clone)]
pub enum Target {
    /// `[[ident]]`: the defining file and line.
    Definition(DefSite),
    /// `[[concept]]`: a file is relevant iff this regex matches its text.
    Relevance(Regex),
    /// `[[lexical]]` / `[[pagination]]` / `[[prefix]]`.
    None,
}

/// One golden entry, resolved into what the runner calls and what the
/// metrics check.
#[derive(Debug, Clone)]
pub struct PlannedQuery {
    pub id: String,
    pub kind: EntryKind,
    /// The text query (`None` for a standalone `--ast` / temporal prefix).
    pub query: Option<String>,
    pub flags: QueryFlags,
    /// The JSON envelope the flags produce.
    pub arm: Arm,
    /// The oracle's query for the full result set; `None` when that set is
    /// not a lexical predicate (`--ast`, `--blast-radius`, no text query).
    pub oracle: Option<LexicalQuery>,
    /// `[[pagination]]` sweep limits / `[[prefix]]` limits.
    pub limits: Vec<u32>,
    pub target: Target,
}

impl PlannedQuery {
    /// The HARD checks that run on this entry, in [`CheckId::ALL`] order.
    pub fn checks(&self) -> Vec<CheckId> {
        CheckId::ALL
            .iter()
            .copied()
            .filter(|c| self.runs(*c))
            .collect()
    }

    /// Whether `check` runs on this entry:
    /// - `lexical.recall` / `precision` / `silent_fn`: the full list has an
    ///   oracle ground truth;
    /// - `lexical.verify_mode`: there is a text query (the lexical envelope);
    /// - `pagination.*`: `[[pagination]]` entries;
    /// - `order.prefix_consistent`: `[[prefix]]` entries;
    /// - `order.score_monotone`: the list is ranked by `score` (no temporal
    ///   sort, no `--blast-radius`).
    pub fn runs(&self, check: CheckId) -> bool {
        match check {
            CheckId::LexicalRecall | CheckId::LexicalPrecision | CheckId::LexicalSilentFn => {
                self.oracle.is_some()
            }
            CheckId::LexicalVerifyMode => self.arm == Arm::Lexical,
            CheckId::PaginationComplete
            | CheckId::PaginationDisjoint
            | CheckId::PaginationOrdered
            | CheckId::PaginationHasMoreHonest => self.kind == EntryKind::Pagination,
            CheckId::OrderPrefixConsistent => self.kind == EntryKind::Prefix,
            CheckId::OrderScoreMonotone => !self.flags.has_rank_override(),
        }
    }

    /// Whether this entry gets a text-mode run for the byte metrics
    /// (`[[ident]]` and `[[concept]]` only; they never carry temporal flags).
    pub fn measures_text(&self) -> bool {
        !matches!(self.target, Target::None)
    }
}

/// Resolve every entry of `golden`, in [`GoldenFile::ids`] order.
///
/// # Errors
///
/// An entry whose flags, mode or regex do not resolve (golden integrity
/// reports the same problems first).
pub fn plan(golden: &GoldenFile) -> anyhow::Result<Vec<PlannedQuery>> {
    let mut out = Vec::new();
    for e in &golden.idents {
        out.push(PlannedQuery {
            id: e.id.clone(),
            kind: EntryKind::Ident,
            query: Some(e.query.clone()),
            flags: QueryFlags::default(),
            arm: Arm::Lexical,
            oracle: Some(
                LexicalQuery::new(&e.query, MatchMode::And, None).with_context(|| e.id.clone())?,
            ),
            limits: Vec::new(),
            target: Target::Definition(e.def.clone()),
        });
    }
    for e in &golden.concepts {
        out.push(PlannedQuery {
            id: e.id.clone(),
            kind: EntryKind::Concept,
            query: Some(e.query.clone()),
            flags: QueryFlags::default(),
            arm: Arm::Lexical,
            oracle: Some(
                LexicalQuery::new(&e.query, MatchMode::And, None).with_context(|| e.id.clone())?,
            ),
            limits: Vec::new(),
            target: Target::Relevance(e.relevance()?),
        });
    }
    for e in &golden.lexicals {
        out.push(PlannedQuery {
            id: e.id.clone(),
            kind: EntryKind::Lexical,
            query: Some(e.query.clone()),
            flags: e.flags().with_context(|| e.id.clone())?,
            arm: Arm::Lexical,
            oracle: Some(e.oracle_query().with_context(|| e.id.clone())?),
            limits: Vec::new(),
            target: Target::None,
        });
    }
    for e in &golden.paginations {
        let flags = QueryFlags::parse(&e.flags).with_context(|| e.id.clone())?;
        out.push(PlannedQuery {
            id: e.id.clone(),
            kind: EntryKind::Pagination,
            query: Some(e.query.clone()),
            arm: flags.arm(true)?,
            oracle: flags.oracle_query(&e.query).with_context(|| e.id.clone())?,
            flags,
            limits: e.limits.clone(),
            target: Target::None,
        });
    }
    for e in &golden.prefixes {
        let flags = QueryFlags::parse(&e.flags).with_context(|| e.id.clone())?;
        let oracle = match &e.query {
            Some(q) => flags.oracle_query(q).with_context(|| e.id.clone())?,
            None => None,
        };
        out.push(PlannedQuery {
            id: e.id.clone(),
            kind: EntryKind::Prefix,
            query: e.query.clone(),
            arm: flags.arm(e.query.is_some()).with_context(|| e.id.clone())?,
            oracle,
            flags,
            limits: e.limits.clone(),
            target: Target::None,
        });
    }
    Ok(out)
}

// ============================================================================
// HARD checks (pure)
// ============================================================================

fn paths(rows: &[ResultRow]) -> Vec<&str> {
    rows.iter().map(|r| r.path.as_str()).collect()
}

/// Up to [`SAMPLE_PATHS`] paths, then `(+N more)`.
fn sample<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    let items: Vec<&str> = items.into_iter().collect();
    let shown = items
        .iter()
        .take(SAMPLE_PATHS)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    match items.len().saturating_sub(SAMPLE_PATHS) {
        0 => shown,
        more => format!("{shown} (+{more} more)"),
    }
}

/// Ground-truth files absent from `rows`, in ground-truth order.
fn missing<'a>(rows: &[ResultRow], gt: &'a [String]) -> Vec<&'a str> {
    let returned: BTreeSet<&str> = rows.iter().map(|r| r.path.as_str()).collect();
    gt.iter()
        .map(String::as_str)
        .filter(|p| !returned.contains(p))
        .collect()
}

/// `lexical.recall`: every ground-truth file is returned.
pub fn check_recall(rows: &[ResultRow], gt: &[String]) -> CheckOutcome {
    let missing = missing(rows, gt);
    if missing.is_empty() {
        return CheckOutcome::Pass;
    }
    CheckOutcome::fail(format!(
        "missing {} of {} ground-truth file(s): {}",
        missing.len(),
        gt.len(),
        sample(missing)
    ))
}

/// `lexical.precision`: every returned file is in the ground truth.
pub fn check_precision(rows: &[ResultRow], gt: &[String]) -> CheckOutcome {
    let truth: BTreeSet<&str> = gt.iter().map(String::as_str).collect();
    let extra: BTreeSet<&str> = rows
        .iter()
        .map(|r| r.path.as_str())
        .filter(|p| !truth.contains(p))
        .collect();
    if extra.is_empty() {
        return CheckOutcome::Pass;
    }
    CheckOutcome::fail(format!(
        "{} returned file(s) outside the ground truth: {}",
        extra.len(),
        sample(extra)
    ))
}

/// `lexical.silent_fn`: no ground-truth file is missing while `degraded[]`
/// is empty (a disclosed miss is a recall failure, not a silent one).
pub fn check_silent_fn(page: &ResultPage, gt: &[String]) -> CheckOutcome {
    let missing = missing(&page.rows, gt);
    if missing.is_empty() || !page.degraded.is_empty() {
        return CheckOutcome::Pass;
    }
    CheckOutcome::fail(format!(
        "{} ground-truth file(s) missing with an empty degraded[]: {}",
        missing.len(),
        sample(missing)
    ))
}

/// The JSON name of a verify mode (`substring` when absent).
pub fn verify_mode_name(mode: &VerifyMode) -> &str {
    match mode {
        VerifyMode::Substring => "substring",
        VerifyMode::Phrase => "phrase",
        VerifyMode::Near => "near",
        VerifyMode::PhraseNear => "phrase_near",
        VerifyMode::Unknown(name) => name,
    }
}

/// `lexical.verify_mode`: the JSON `verify_mode` equals the declared mode.
pub fn check_verify_mode(page: &ResultPage, expected: &VerifyMode) -> CheckOutcome {
    if &page.verify_mode == expected {
        return CheckOutcome::Pass;
    }
    CheckOutcome::fail(format!(
        "verify_mode is {}, expected {}",
        verify_mode_name(&page.verify_mode),
        verify_mode_name(expected)
    ))
}

/// `order.score_monotone`: `score` never increases down the list.
pub fn check_score_monotone(rows: &[ResultRow]) -> CheckOutcome {
    let rises: Vec<usize> = (1..rows.len())
        .filter(|&i| rows[i].score > rows[i - 1].score)
        .collect();
    let Some(&first) = rises.first() else {
        return CheckOutcome::Pass;
    };
    CheckOutcome::fail(format!(
        "score rises at rank {} ({} {} > {} at rank {}); {} rise(s) over {} rows",
        first + 1,
        rows[first].path,
        rows[first].score,
        rows[first - 1].score,
        first,
        rises.len(),
        rows.len()
    ))
}

/// The four pagination outcomes of one entry (all limits).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaginationOutcomes {
    pub complete: CheckOutcome,
    pub disjoint: CheckOutcome,
    pub ordered: CheckOutcome,
    pub has_more_honest: CheckOutcome,
}

/// Fold per-limit problems into one outcome (`L=<limit>: …; L=…`).
fn outcome_of(problems: &[String]) -> CheckOutcome {
    if problems.is_empty() {
        CheckOutcome::Pass
    } else {
        CheckOutcome::fail(problems.join("; "))
    }
}

/// `pagination.complete` / `disjoint` / `ordered` / `has_more_honest` over
/// every sweep, against the full list:
/// - complete: the union of the pages is the full list's set;
/// - disjoint: no file appears twice across the pages;
/// - ordered: the concatenated pages equal the full list;
/// - has_more_honest: no empty page claims `has_more`; a page that reaches
///   the end of the full list says `has_more: false`; a page that does not
///   reach it says `true`; and the sweep ends within [`MAX_PAGES`].
pub fn check_pagination(full: &[ResultRow], sweeps: &[Sweep]) -> PaginationOutcomes {
    let full_paths = paths(full);
    let full_set: BTreeSet<&str> = full_paths.iter().copied().collect();
    let total = full.len() as u64;
    let (mut complete, mut disjoint, mut ordered, mut honest) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());

    for sweep in sweeps {
        let tag = format!("L={}", sweep.limit);
        let shown: Vec<&str> = sweep
            .pages
            .iter()
            .flat_map(|p| p.page.rows.iter().map(|r| r.path.as_str()))
            .collect();
        if let Some(problem) = completeness_problem(&full_set, &shown) {
            complete.push(format!("{tag}: {problem}"));
        }
        if let Some(problem) = duplicate_problem(&shown) {
            disjoint.push(format!("{tag}: {problem}"));
        }
        if let Some(problem) = order_problem(&full_paths, &shown) {
            ordered.push(format!("{tag}: {problem}"));
        }
        if let Some(problem) = has_more_problem(sweep, total) {
            honest.push(format!("{tag}: {problem}"));
        }
    }

    PaginationOutcomes {
        complete: outcome_of(&complete),
        disjoint: outcome_of(&disjoint),
        ordered: outcome_of(&ordered),
        has_more_honest: outcome_of(&honest),
    }
}

/// `pagination.complete` for one sweep: full-list files the pages never
/// show, and shown files the full list lacks.
fn completeness_problem(full_set: &BTreeSet<&str>, shown: &[&str]) -> Option<String> {
    let shown_set: BTreeSet<&str> = shown.iter().copied().collect();
    let never: Vec<&str> = full_set.difference(&shown_set).copied().collect();
    let foreign: Vec<&str> = shown_set.difference(full_set).copied().collect();
    let mut parts = Vec::new();
    if !never.is_empty() {
        parts.push(format!(
            "{} file(s) never shown: {}",
            never.len(),
            sample(never)
        ));
    }
    if !foreign.is_empty() {
        parts.push(format!(
            "{} file(s) shown but not in the full list: {}",
            foreign.len(),
            sample(foreign)
        ));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// `pagination.disjoint` for one sweep: files shown more than once.
fn duplicate_problem(shown: &[&str]) -> Option<String> {
    let mut seen = BTreeSet::new();
    let dups: BTreeSet<&str> = shown.iter().copied().filter(|p| !seen.insert(*p)).collect();
    (!dups.is_empty()).then(|| {
        format!(
            "{} file(s) shown more than once: {}",
            dups.len(),
            sample(dups)
        )
    })
}

/// `pagination.ordered` for one sweep: the concatenated pages differ from
/// the full list.
fn order_problem(full_paths: &[&str], shown: &[&str]) -> Option<String> {
    if shown == full_paths {
        return None;
    }
    let (rank, got, want) = first_difference(shown, full_paths);
    Some(format!(
        "pages show {} row(s), the full list has {}; first difference at rank {rank} \
         (pages: {got}, full list: {want})",
        shown.len(),
        full_paths.len()
    ))
}

/// `pagination.has_more_honest` for one sweep over a `total`-row full list.
fn has_more_problem(sweep: &Sweep, total: u64) -> Option<String> {
    let mut problems = Vec::new();
    let empty_claims: Vec<u64> = sweep
        .pages
        .iter()
        .filter(|p| p.page.has_more && p.page.rows.is_empty())
        .map(|p| p.offset)
        .collect();
    if let Some(first) = empty_claims.first() {
        problems.push(format!(
            "{} empty page(s) claim has_more (first at offset {first})",
            empty_claims.len()
        ));
    }
    for p in &sweep.pages {
        let rows = p.page.rows.len() as u64;
        let reach = p.offset.saturating_add(rows);
        if p.page.has_more && rows > 0 && reach >= total {
            problems.push(format!(
                "page at offset {} reaches the end of the {total}-row list but claims has_more",
                p.offset
            ));
        }
        if !p.page.has_more && reach < total {
            problems.push(format!(
                "has_more is false at offset {} but the full list has {total} rows",
                p.offset
            ));
        }
    }
    match sweep.pages.last() {
        None => problems.push("no page was fetched".to_string()),
        Some(last) if last.page.has_more => problems.push(format!(
            "sweep did not end within {MAX_PAGES} pages ({} fetched)",
            sweep.pages.len()
        )),
        Some(_) => {}
    }
    (!problems.is_empty()).then(|| problems.join("; "))
}

/// Where two path lists first differ: the 1-based rank (the shorter list's
/// end when one is a prefix of the other) and each list's path there
/// (`<end>` past its end).
fn first_difference<'a>(got: &[&'a str], want: &[&'a str]) -> (usize, &'a str, &'a str) {
    let at = got
        .iter()
        .zip(want)
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| got.len().min(want.len()));
    (
        at + 1,
        got.get(at).copied().unwrap_or("<end>"),
        want.get(at).copied().unwrap_or("<end>"),
    )
}

/// `order.prefix_consistent`: each `--limit N` list equals the full list's
/// first `N` rows.
pub fn check_prefix(full: &[ResultRow], limited: &[(u32, ResultPage)]) -> CheckOutcome {
    let full_paths = paths(full);
    let mut problems = Vec::new();
    for (limit, page) in limited {
        let n = (*limit as usize).min(full_paths.len());
        let want = &full_paths[..n];
        let got = paths(&page.rows);
        if got == want {
            continue;
        }
        let want_set: BTreeSet<&str> = want.iter().copied().collect();
        let overlap = got
            .iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter(|p| want_set.contains(**p))
            .count();
        let (rank, got_at, want_at) = first_difference(&got, want);
        problems.push(format!(
            "limit {limit}: {overlap}/{n} overlap with the full list's first {n}; rank {rank}: got {got_at}, want {want_at}"
        ));
    }
    outcome_of(&problems)
}

// ============================================================================
// Measurements (pure)
// ============================================================================

/// Replace every `in <digits>ms` with `in 0ms`, so the text-mode footer's
/// duration never changes a byte count.
pub fn normalize_durations(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    // Each iteration advances `i` by at least one byte.
    while i < bytes.len() {
        if bytes[i..].starts_with(b"in ") {
            let digits = i + 3;
            let end = digits
                + bytes[digits..]
                    .iter()
                    .take_while(|b| b.is_ascii_digit())
                    .count();
            if end > digits && bytes[end..].starts_with(b"ms") {
                out.extend_from_slice(b"in 0ms");
                i = end + 2;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Whether `line` is the header of `path`'s result block in skim's text
/// output: `<path>:<line>  [field] …` or `<path>  [field] …`.
fn is_block_header(line: &[u8], path: &str) -> bool {
    let Some(rest) = line.strip_prefix(path.as_bytes()) else {
        return false;
    };
    rest.starts_with(b"  [")
        || (rest.first() == Some(&b':') && rest.get(1).is_some_and(u8::is_ascii_digit))
}

/// Bytes of `stdout` up to and including `path`'s result block (its header,
/// its snippet lines and the blank line closing it), or `None` if `path`
/// has no block.
pub fn bytes_through_block(stdout: &[u8], path: &str) -> Option<u64> {
    let mut offset = 0usize;
    let mut in_block = false;
    for line in stdout.split_inclusive(|&b| b == b'\n') {
        offset += line.len();
        let content = line.strip_suffix(b"\n").unwrap_or(line);
        if in_block {
            if content.is_empty() {
                return u64::try_from(offset).ok();
            }
        } else if is_block_header(content, path) {
            in_block = true;
        }
    }
    if in_block {
        u64::try_from(offset).ok()
    } else {
        None
    }
}

/// Precision over the first `min(k, ranked.len())` entries (0 when empty).
pub fn precision_at_k(ranked: &[&str], k: usize, relevant: impl Fn(&str) -> bool) -> f64 {
    let n = k.min(ranked.len());
    if n == 0 {
        return 0.0;
    }
    let hits = ranked[..n].iter().filter(|p| relevant(p)).count();
    hits as f64 / n as f64
}

/// Nearest-rank percentile (`p` in `[0, 1]`): the value at rank
/// `ceil(p · n)` of the sorted values, so the median of an even count is
/// the lower middle value. `None` for no values.
pub fn percentile(values: &[u64], p: f64) -> Option<f64> {
    let values: Vec<f64> = values.iter().map(|&v| v as f64).collect();
    percentile_f64(&values, p)
}

/// [`percentile`] over `f64` values (e.g. latency milliseconds), in
/// [`f64::total_cmp`] order.
pub fn percentile_f64(values: &[f64], p: f64) -> Option<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    let rank = ((p * n as f64).ceil() as usize).clamp(1, n);
    sorted.get(rank - 1).copied()
}

/// Per-query measurements of one `[[ident]]` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentSample {
    pub id: String,
    /// 1-based rank of the defining file in the full list (`None` = absent).
    pub rank: Option<u64>,
    /// Rank of the defining file in the alphabetical baseline
    /// (`rg -l --sort path` order over the ground truth).
    pub rank_baseline_alpha: Option<u64>,
    /// Rank of the defining file in the occurrence-count baseline.
    pub rank_baseline_count: Option<u64>,
    /// The defining file's row anchors on `def.line`.
    pub anchor_eq_def: bool,
    /// The defining file's snippet shows `def.line`.
    pub def_line_in_snippet: bool,
    /// Text-mode stdout + stderr bytes at the default limit, durations
    /// normalized.
    pub text_bytes: u64,
    /// Text-mode bytes through the defining file's block (`None` = not on
    /// page 1: a miss).
    pub first_correct_bytes: Option<u64>,
    /// Simulated `rg -n -F --sort path` output bytes.
    pub rg_text_bytes: u64,
    /// Simulated rg bytes through the definition line.
    pub rg_first_correct_bytes: Option<u64>,
}

/// Per-query measurements of one `[[concept]]` entry (precision at k with
/// denominator `min(k, rows)`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConceptSample {
    pub id: String,
    pub p5: f64,
    pub p10: f64,
    pub p5_baseline_alpha: f64,
    pub p10_baseline_alpha: f64,
    pub p5_baseline_count: f64,
    pub p10_baseline_count: f64,
    pub text_bytes: u64,
    pub rg_text_bytes: u64,
}

/// Everything one corpus contributes to the RATCHET values.
#[derive(Debug, Clone, PartialEq)]
pub struct CorpusSamples {
    /// Oracle universe size − skim `file_count`.
    pub universe_delta: i64,
    /// Persisted skip reasons whose counts differ between oracle and skim.
    pub skipped_mismatch: u64,
    pub indexed_tracked: u64,
    pub tracked_text: u64,
    pub idents: Vec<IdentSample>,
    pub concepts: Vec<ConceptSample>,
}

// ============================================================================
// RATCHET metrics
// ============================================================================

/// Which way a RATCHET metric improves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    HigherBetter,
    LowerBetter,
    /// Closer to zero is better (a delta or mismatch count).
    ZeroBest,
    /// A reference value (the baselines): a change is neither better nor
    /// worse, but still needs a bless.
    Neutral,
}

/// How far a RATCHET value may move before the gate asks for a bless.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Tolerance {
    /// Equal after rounding to 4 decimal places.
    Exact,
    /// Within this fraction of the baseline value (bytes: ±3%).
    Relative(f64),
}

/// One RATCHET metric.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricDef {
    pub name: &'static str,
    pub direction: Direction,
    pub tolerance: Tolerance,
    /// The vision's bar, for `report.md`.
    pub bar: &'static str,
    /// The baseline metric this one should beat (the "beats baseline"
    /// column), if any.
    pub beats: Option<&'static str>,
}

const BYTES: Tolerance = Tolerance::Relative(0.03);

const fn def(
    name: &'static str,
    direction: Direction,
    tolerance: Tolerance,
    bar: &'static str,
) -> MetricDef {
    MetricDef {
        name,
        direction,
        tolerance,
        bar,
        beats: None,
    }
}

/// A metric whose bar is beating the baseline metric `versus`.
const fn versus(
    name: &'static str,
    direction: Direction,
    tolerance: Tolerance,
    bar: &'static str,
    versus: &'static str,
) -> MetricDef {
    MetricDef {
        name,
        direction,
        tolerance,
        bar,
        beats: Some(versus),
    }
}

/// A neutral reference value (a baseline's score).
const fn reference(name: &'static str, tolerance: Tolerance) -> MetricDef {
    def(name, Direction::Neutral, tolerance, "baseline")
}

/// Every RATCHET metric (tolerance 0 except bytes at ±3%).
pub const RATCHET_METRICS: &[MetricDef] = &[
    def(
        "universe.delta",
        Direction::ZeroBest,
        Tolerance::Exact,
        "= 0",
    ),
    def(
        "universe.skipped_by_reason_mismatch",
        Direction::ZeroBest,
        Tolerance::Exact,
        "= 0",
    ),
    def(
        "coverage.tracked_text",
        Direction::HigherBetter,
        Tolerance::Exact,
        "ratchet",
    ),
    versus(
        "ident.def_top1",
        Direction::HigherBetter,
        Tolerance::Exact,
        "> path-ordered grep",
        "ident.def_top1.baseline_alpha",
    ),
    versus(
        "ident.mrr",
        Direction::HigherBetter,
        Tolerance::Exact,
        "> path-ordered grep",
        "ident.mrr.baseline_alpha",
    ),
    reference("ident.def_top1.baseline_alpha", Tolerance::Exact),
    reference("ident.mrr.baseline_alpha", Tolerance::Exact),
    reference("ident.def_top1.baseline_count", Tolerance::Exact),
    reference("ident.mrr.baseline_count", Tolerance::Exact),
    def(
        "ident.anchor_eq_def",
        Direction::HigherBetter,
        Tolerance::Exact,
        "ratchet (#555)",
    ),
    def(
        "ident.def_line_in_snippet",
        Direction::HigherBetter,
        Tolerance::Exact,
        "ratchet",
    ),
    versus(
        "concept.p5",
        Direction::HigherBetter,
        Tolerance::Exact,
        "> occurrence-count sort",
        "concept.p5.baseline_count",
    ),
    versus(
        "concept.p10",
        Direction::HigherBetter,
        Tolerance::Exact,
        "> occurrence-count sort (#556)",
        "concept.p10.baseline_count",
    ),
    reference("concept.p5.baseline_alpha", Tolerance::Exact),
    reference("concept.p10.baseline_alpha", Tolerance::Exact),
    reference("concept.p5.baseline_count", Tolerance::Exact),
    reference("concept.p10.baseline_count", Tolerance::Exact),
    versus(
        "bytes.text_median",
        Direction::LowerBetter,
        BYTES,
        "< simulated rg",
        "bytes.rg_text_median",
    ),
    versus(
        "bytes.text_p90",
        Direction::LowerBetter,
        BYTES,
        "< simulated rg",
        "bytes.rg_text_p90",
    ),
    reference("bytes.rg_text_median", BYTES),
    reference("bytes.rg_text_p90", BYTES),
    versus(
        "bytes.first_correct_median",
        Direction::LowerBetter,
        BYTES,
        "< simulated rg",
        "bytes.rg_first_correct_median",
    ),
    reference("bytes.rg_first_correct_median", BYTES),
    def(
        "bytes.first_correct_misses",
        Direction::LowerBetter,
        Tolerance::Exact,
        "= 0",
    ),
];

/// The definition of the RATCHET metric `name`.
pub fn metric_def(name: &str) -> Option<&'static MetricDef> {
    RATCHET_METRICS.iter().find(|d| d.name == name)
}

/// How a RATCHET value moved against its baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RatchetChange {
    /// Within tolerance.
    Unchanged,
    Improved,
    Regressed,
    /// Moved, but not in a direction that is better or worse (a neutral
    /// metric, an unknown metric, or a zero-best value that flipped sign).
    Changed,
}

/// Whether a metric beats the baseline it is compared with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Beat {
    Yes,
    Tie,
    No,
}

impl Beat {
    /// `yes` / `tie` / `no`.
    pub fn as_str(self) -> &'static str {
        match self {
            Beat::Yes => "yes",
            Beat::Tie => "tie",
            Beat::No => "no",
        }
    }
}

/// Whether `name` beats its baseline metric ([`MetricDef::beats`]) within
/// one set of RATCHET values (INFO: the "beats baseline" column). `None`
/// when the metric has no baseline or either value is missing.
pub fn beats_baseline(name: &str, ratchet: &BTreeMap<String, f64>) -> Option<Beat> {
    let d = metric_def(name)?;
    let other = d.beats?;
    let (current, base) = (*ratchet.get(name)?, *ratchet.get(other)?);
    let ord = match d.direction {
        Direction::HigherBetter => current.partial_cmp(&base)?,
        Direction::LowerBetter => base.partial_cmp(&current)?,
        Direction::ZeroBest | Direction::Neutral => return None,
    };
    Some(match ord {
        std::cmp::Ordering::Greater => Beat::Yes,
        std::cmp::Ordering::Equal => Beat::Tie,
        std::cmp::Ordering::Less => Beat::No,
    })
}

/// Compare `current` with `baseline` for metric `name` (an unknown name is
/// exact and neutral).
pub fn compare_ratchet(name: &str, baseline: f64, current: f64) -> RatchetChange {
    let (direction, tolerance) = metric_def(name)
        .map_or((Direction::Neutral, Tolerance::Exact), |d| {
            (d.direction, d.tolerance)
        });
    let within = match tolerance {
        Tolerance::Exact => round4(baseline) == round4(current),
        Tolerance::Relative(r) => (current - baseline).abs() <= r * baseline.abs(),
    };
    if within {
        return RatchetChange::Unchanged;
    }
    let better = match direction {
        Direction::HigherBetter => Some(current > baseline),
        Direction::LowerBetter => Some(current < baseline),
        Direction::ZeroBest if current.abs() != baseline.abs() => {
            Some(current.abs() < baseline.abs())
        }
        Direction::ZeroBest | Direction::Neutral => None,
    };
    match better {
        Some(true) => RatchetChange::Improved,
        Some(false) => RatchetChange::Regressed,
        None => RatchetChange::Changed,
    }
}

fn mean(values: impl IntoIterator<Item = f64>) -> Option<f64> {
    let (sum, n) = values
        .into_iter()
        .fold((0.0, 0usize), |(s, n), v| (s + v, n + 1));
    (n > 0).then(|| sum / n as f64)
}

fn fraction(flags: impl IntoIterator<Item = bool>) -> Option<f64> {
    mean(flags.into_iter().map(|b| if b { 1.0 } else { 0.0 }))
}

/// Which rank of an [`IdentSample`] a ranking metric reads.
type RankOf = fn(&IdentSample) -> Option<u64>;

/// RATCHET values pooled over `samples` (one corpus, or every corpus for
/// the aggregate), rounded to 4 decimal places. `ident.*` / `concept.*` /
/// `bytes.*` are present only when the pool has entries of that kind.
pub fn ratchet_values(samples: &[&CorpusSamples]) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    put(
        &mut out,
        "universe.delta",
        Some(samples.iter().map(|s| s.universe_delta).sum::<i64>() as f64),
    );
    put(
        &mut out,
        "universe.skipped_by_reason_mismatch",
        Some(samples.iter().map(|s| s.skipped_mismatch).sum::<u64>() as f64),
    );
    let indexed: u64 = samples.iter().map(|s| s.indexed_tracked).sum();
    let text: u64 = samples.iter().map(|s| s.tracked_text).sum();
    put(
        &mut out,
        "coverage.tracked_text",
        Some(if text == 0 {
            1.0
        } else {
            indexed as f64 / text as f64
        }),
    );

    let idents: Vec<&IdentSample> = samples.iter().flat_map(|s| s.idents.iter()).collect();
    if !idents.is_empty() {
        put_ident_values(&mut out, &idents);
    }
    let concepts: Vec<&ConceptSample> = samples.iter().flat_map(|s| s.concepts.iter()).collect();
    if !concepts.is_empty() {
        put_concept_values(&mut out, &concepts);
    }
    put_text_byte_values(&mut out, &idents, &concepts);
    out
}

/// Record `value`, rounded to 4 decimal places, under `name` (nothing for
/// `None`).
fn put(out: &mut BTreeMap<String, f64>, name: &str, value: Option<f64>) {
    if let Some(v) = value {
        out.insert(name.to_string(), round4(v));
    }
}

/// `ident.*` and the definition-bound `bytes.*first_correct*` values.
fn put_ident_values(out: &mut BTreeMap<String, f64>, idents: &[&IdentSample]) {
    // (top-1 metric, MRR metric, which ranking): skim, then both baselines.
    let rankings: [(&str, &str, RankOf); 3] = [
        ("ident.def_top1", "ident.mrr", |i| i.rank),
        (
            "ident.def_top1.baseline_alpha",
            "ident.mrr.baseline_alpha",
            |i| i.rank_baseline_alpha,
        ),
        (
            "ident.def_top1.baseline_count",
            "ident.mrr.baseline_count",
            |i| i.rank_baseline_count,
        ),
    ];
    for (top1, mrr, rank_of) in rankings {
        put(
            out,
            top1,
            fraction(idents.iter().map(|i| rank_of(i) == Some(1))),
        );
        let rrs: Vec<f64> = idents
            .iter()
            .map(|i| rank_of(i).map_or(0.0, |r| 1.0 / r as f64))
            .collect();
        put(out, mrr, Some(crate::metrics::mrr(&rrs)));
    }
    put(
        out,
        "ident.anchor_eq_def",
        fraction(idents.iter().map(|i| i.anchor_eq_def)),
    );
    put(
        out,
        "ident.def_line_in_snippet",
        fraction(idents.iter().map(|i| i.def_line_in_snippet)),
    );
    let hits: Vec<u64> = idents
        .iter()
        .filter_map(|i| i.first_correct_bytes)
        .collect();
    put(out, "bytes.first_correct_median", percentile(&hits, 0.5));
    put(
        out,
        "bytes.first_correct_misses",
        Some(
            idents
                .iter()
                .filter(|i| i.first_correct_bytes.is_none())
                .count() as f64,
        ),
    );
    let rg: Vec<u64> = idents
        .iter()
        .filter_map(|i| i.rg_first_correct_bytes)
        .collect();
    put(out, "bytes.rg_first_correct_median", percentile(&rg, 0.5));
}

/// `concept.*` values: mean precision at 5 / 10, skim and both baselines.
fn put_concept_values(out: &mut BTreeMap<String, f64>, concepts: &[&ConceptSample]) {
    put(out, "concept.p5", mean(concepts.iter().map(|c| c.p5)));
    put(out, "concept.p10", mean(concepts.iter().map(|c| c.p10)));
    put(
        out,
        "concept.p5.baseline_alpha",
        mean(concepts.iter().map(|c| c.p5_baseline_alpha)),
    );
    put(
        out,
        "concept.p10.baseline_alpha",
        mean(concepts.iter().map(|c| c.p10_baseline_alpha)),
    );
    put(
        out,
        "concept.p5.baseline_count",
        mean(concepts.iter().map(|c| c.p5_baseline_count)),
    );
    put(
        out,
        "concept.p10.baseline_count",
        mean(concepts.iter().map(|c| c.p10_baseline_count)),
    );
}

/// `bytes.text_*` / `bytes.rg_text_*`: text-mode output size over every
/// ranking entry (idents and concepts together), skim vs simulated rg.
fn put_text_byte_values(
    out: &mut BTreeMap<String, f64>,
    idents: &[&IdentSample],
    concepts: &[&ConceptSample],
) {
    let text_bytes: Vec<u64> = idents
        .iter()
        .map(|i| i.text_bytes)
        .chain(concepts.iter().map(|c| c.text_bytes))
        .collect();
    let rg_bytes: Vec<u64> = idents
        .iter()
        .map(|i| i.rg_text_bytes)
        .chain(concepts.iter().map(|c| c.rg_text_bytes))
        .collect();
    put(out, "bytes.text_median", percentile(&text_bytes, 0.5));
    put(out, "bytes.text_p90", percentile(&text_bytes, 0.9));
    put(out, "bytes.rg_text_median", percentile(&rg_bytes, 0.5));
    put(out, "bytes.rg_text_p90", percentile(&rg_bytes, 0.9));
}

// ============================================================================
// Evaluation
// ============================================================================

/// One corpus's raw results: HARD outcomes (before the ledger), samples,
/// and INFO.
#[derive(Debug, Clone, PartialEq)]
pub struct CorpusEvaluation {
    /// `(id, check, outcome)`, sorted by `(id, check)`.
    pub outcomes: Vec<(String, CheckId, CheckOutcome)>,
    pub samples: CorpusSamples,
    /// Ground-truth hits among tracked text files outside the indexed
    /// universe, by id (non-zero only).
    pub unindexed_hits: BTreeMap<String, u64>,
}

/// Score one corpus.
///
/// # Errors
///
/// `observations` does not follow `plan` one-for-one (same length, same
/// ids), or an entry lacks an observation its checks need.
pub fn evaluate(
    universe: &Universe,
    stats: &StatsSnapshot,
    plan: &[PlannedQuery],
    observations: &[EntryObservation],
) -> anyhow::Result<CorpusEvaluation> {
    anyhow::ensure!(
        plan.len() == observations.len(),
        "{} planned entries but {} observations",
        plan.len(),
        observations.len()
    );

    let mut outcomes = Vec::new();
    let mut idents = Vec::new();
    let mut concepts = Vec::new();
    let mut unindexed_hits = BTreeMap::new();

    for (q, obs) in plan.iter().zip(observations) {
        anyhow::ensure!(
            q.id == obs.id,
            "observation {} does not match planned entry {}",
            obs.id,
            q.id
        );
        let gt = q.oracle.as_ref().map(|o| ground_truth(universe.files(), o));
        if let Some(o) = &q.oracle {
            let hits = ground_truth(universe.unindexed_text_files(), o).len();
            if hits > 0 {
                unindexed_hits.insert(q.id.clone(), hits as u64);
            }
        }

        let pagination = (q.kind == EntryKind::Pagination)
            .then(|| check_pagination(&obs.full.rows, &obs.sweeps));
        for check in q.checks() {
            let outcome = run_check(check, q, obs, gt.as_deref(), pagination.as_ref())
                .with_context(|| format!("{}: {check}", q.id))?;
            outcomes.push((q.id.clone(), check, outcome));
        }

        match &q.target {
            Target::Definition(def) => {
                idents.push(measure_ident(q, def, obs, universe, gt.as_deref())?);
            }
            Target::Relevance(re) => {
                concepts.push(measure_concept(q, re, obs, universe, gt.as_deref())?);
            }
            Target::None => {}
        }
    }
    outcomes.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));

    let coverage = universe.coverage();
    Ok(CorpusEvaluation {
        outcomes,
        samples: CorpusSamples {
            universe_delta: i64::try_from(universe.len())? - i64::try_from(stats.file_count)?,
            skipped_mismatch: skip_mismatches(
                &universe.persisted_skipped_by_reason(),
                &stats.skipped_by_reason,
            ),
            indexed_tracked: u64::try_from(coverage.indexed_tracked)?,
            tracked_text: u64::try_from(coverage.tracked_text)?,
            idents,
            concepts,
        },
        unindexed_hits,
    })
}

fn run_check(
    check: CheckId,
    q: &PlannedQuery,
    obs: &EntryObservation,
    gt: Option<&[String]>,
    pagination: Option<&PaginationOutcomes>,
) -> anyhow::Result<CheckOutcome> {
    let gt = || gt.context("no oracle ground truth");
    let pagination = || pagination.context("no pagination sweep");
    Ok(match check {
        CheckId::LexicalRecall => check_recall(&obs.full.rows, gt()?),
        CheckId::LexicalPrecision => check_precision(&obs.full.rows, gt()?),
        CheckId::LexicalSilentFn => check_silent_fn(&obs.full, gt()?),
        CheckId::LexicalVerifyMode => check_verify_mode(&obs.full, &q.flags.verify_mode()),
        CheckId::PaginationComplete => pagination()?.complete.clone(),
        CheckId::PaginationDisjoint => pagination()?.disjoint.clone(),
        CheckId::PaginationOrdered => pagination()?.ordered.clone(),
        CheckId::PaginationHasMoreHonest => pagination()?.has_more_honest.clone(),
        CheckId::OrderPrefixConsistent => check_prefix(&obs.full.rows, &obs.limited),
        CheckId::OrderScoreMonotone => check_score_monotone(&obs.full.rows),
    })
}

/// Number of skip reasons whose counts differ (a missing reason counts 0).
fn skip_mismatches(oracle: &BTreeMap<String, u64>, skim: &BTreeMap<String, u64>) -> u64 {
    let reasons: BTreeSet<&String> = oracle.keys().chain(skim.keys()).collect();
    reasons
        .into_iter()
        .filter(|r| oracle.get(*r).copied().unwrap_or(0) != skim.get(*r).copied().unwrap_or(0))
        .count() as u64
}

fn text_bytes(obs: &EntryObservation) -> anyhow::Result<(Vec<u8>, u64)> {
    let text = obs
        .text
        .as_ref()
        .context("no text-mode output for a ranking entry")?;
    let stdout = normalize_durations(&text.stdout);
    let total = stdout.len() + normalize_durations(&text.stderr).len();
    Ok((stdout, u64::try_from(total)?))
}

/// 1-based rank of `path` in `ranked`.
fn rank_in(ranked: &[String], path: &str) -> Option<u64> {
    ranked.iter().position(|p| p == path).map(|i| i as u64 + 1)
}

fn measure_ident(
    q: &PlannedQuery,
    def: &DefSite,
    obs: &EntryObservation,
    universe: &Universe,
    gt: Option<&[String]>,
) -> anyhow::Result<IdentSample> {
    let query = q.query.as_deref().context("an ident entry has no query")?;
    let gt = gt.context("an ident entry has no ground truth")?;
    let rows = &obs.full.rows;
    let position = rows.iter().position(|r| r.path == def.path);
    let def_row = position.map(|i| &rows[i]);
    let (stdout, text_bytes) = text_bytes(obs)?;
    let rg = simulate_rg_fixed(universe.files(), query);
    Ok(IdentSample {
        id: q.id.clone(),
        rank: position.map(|i| i as u64 + 1),
        rank_baseline_alpha: rank_in(&baseline_alphabetical(gt), &def.path),
        rank_baseline_count: rank_in(
            &baseline_occurrence_count(gt, query, |p| universe.text(p)),
            &def.path,
        ),
        anchor_eq_def: def_row.is_some_and(|r| r.line == Some(def.line)),
        def_line_in_snippet: def_row
            .is_some_and(|r| r.snippet.iter().any(|l| l.line_number == def.line)),
        text_bytes,
        first_correct_bytes: bytes_through_block(&stdout, &def.path),
        rg_text_bytes: u64::try_from(rg.total_bytes())?,
        rg_first_correct_bytes: rg
            .bytes_through(&def.path, def.line)
            .map(u64::try_from)
            .transpose()?,
    })
}

fn measure_concept(
    q: &PlannedQuery,
    relevant: &Regex,
    obs: &EntryObservation,
    universe: &Universe,
    gt: Option<&[String]>,
) -> anyhow::Result<ConceptSample> {
    let query = q.query.as_deref().context("a concept entry has no query")?;
    let gt = gt.context("a concept entry has no ground truth")?;
    let is_relevant = |path: &str| universe.text(path).is_some_and(|t| relevant.is_match(t));
    let ranked = paths(&obs.full.rows);
    let alpha = baseline_alphabetical(gt);
    let alpha: Vec<&str> = alpha.iter().map(String::as_str).collect();
    let count = baseline_occurrence_count(gt, query, |p| universe.text(p));
    let count: Vec<&str> = count.iter().map(String::as_str).collect();
    let (_, text_bytes) = text_bytes(obs)?;
    let rg = simulate_rg_fixed(universe.files(), query);
    Ok(ConceptSample {
        id: q.id.clone(),
        p5: round4(precision_at_k(&ranked, 5, is_relevant)),
        p10: round4(precision_at_k(&ranked, 10, is_relevant)),
        p5_baseline_alpha: round4(precision_at_k(&alpha, 5, is_relevant)),
        p10_baseline_alpha: round4(precision_at_k(&alpha, 10, is_relevant)),
        p5_baseline_count: round4(precision_at_k(&count, 5, is_relevant)),
        p10_baseline_count: round4(precision_at_k(&count, 10, is_relevant)),
        text_bytes,
        rg_text_bytes: u64::try_from(rg.total_bytes())?,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
#[path = "metrics_tests.rs"]
mod tests;
