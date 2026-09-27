//! Scoring skim's standalone `--ast` answers against the structural oracle
//! (#541): the oracle's answers over one corpus's AST universe
//! ([`OracleAnswers`]), skim's rows split by language, the three HARD
//! structural checks, the per-entry measurements behind the structural
//! RATCHET values ([`StructuralSample`]), and the `uncovered_patterns` list.
//!
//! On a corpus with at least one `[[ast]]` entry, skim is called once per
//! catalog pattern ([`called_patterns`]) with `--ast <pattern>` — the
//! patterns the oracle cannot encode, and those with no entry in the corpus,
//! included. Its rows are split by file extension (the oracle's own table,
//! [`structural::classify`]) into that pattern's `[[ast]]` entries. A row no
//! entry scores — a language the oracle has no grammar for (Java, C,
//! Markdown, …), an oracle language with no entry for the pattern, or a
//! pattern with no entry at all — is counted in
//! `structural.unscored_rows.<pattern>`, never dropped. The count is emitted
//! for every called pattern, 0 included, so a pattern's first unscored row is
//! a visible RATCHET move.
//!
//! Like the structural oracle (`rskim_oracle::structural`), nothing here
//! imports skim's AST search code: the expectations come from the oracle's
//! tree-sitter queries only. skim's pattern catalog arrives as a parameter
//! ([`crate::scoreboard::catalog::skim_catalog`]).

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use rayon::prelude::*;
use rskim_oracle::structural::{
    self, AST_SIZE_CAP_BYTES, FileMatches, INTENTS, LangClass, OracleLang, OracleScratch,
    PatternCoverage, StructuralOracle,
};
use serde::{Deserialize, Serialize};

use crate::scoreboard::catalog::{CatalogPattern, catalog_coverage};
use crate::scoreboard::fmt::{round4, sample};
use crate::scoreboard::golden::{GoldenFile, PrecisionClass};
use crate::scoreboard::types::{AstCoverage, AstPage, CheckOutcome, ResultPage, ResultRow};

// ============================================================================
// Targets and oracle answers
// ============================================================================

/// What an `[[ast]]` entry scores: one catalog pattern in one oracle
/// language, with its declared precision class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructuralTarget {
    pub pattern: String,
    pub lang: OracleLang,
    pub precision: PrecisionClass,
    /// The golden entry's `expect_oracle_empty`: a false-positive guard (see
    /// [`is_vacuous`] and [`unexpected_oracle_matches`]).
    pub expect_oracle_empty: bool,
}

/// The files one oracle query matched: path → the 1-based first line of
/// each match (sorted, de-duplicated). Files without a match are absent.
pub type MatchFiles = BTreeMap<String, Vec<u32>>;

/// Oracle answers keyed by `(pattern, lang)`: the pattern name from the
/// oracle's own registry (`&'static str`), then the language its query runs
/// on. Two levels rather than one tuple key, because a `(&'static str, _)`
/// key cannot be looked up with a golden entry's shorter-lived `&str`
/// without allocating, while a `&'static str` key can (`Borrow<str>`).
type AnswerMap = BTreeMap<&'static str, BTreeMap<OracleLang, MatchFiles>>;

/// The structural oracle's answers over one corpus's AST universe.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OracleAnswers {
    /// Every registered `(pattern, lang)` query, matched or not.
    definition: AnswerMap,
    /// Every intent oracle's `(pattern, lang)`, matched or not.
    intent: AnswerMap,
    /// Scored files (inside the AST universe) per oracle language.
    scored_files: BTreeMap<OracleLang, u64>,
    /// Universe files skim's size-cap accounting excludes
    /// ([`structural::over_cap_count`]).
    over_cap: u64,
}

impl OracleAnswers {
    /// Run the oracle over every `(path, text)` of a corpus universe: each
    /// file is parsed once, and files are processed in parallel (the oracle
    /// is `Sync`), each rayon worker reusing one [`OracleScratch`] (parser
    /// and query cursor) from file to file; the answers do not depend on the
    /// order.
    ///
    /// # Errors
    ///
    /// Any [`StructuralOracle::file_matches`] error (a parser that returns
    /// no tree, a query over the oracle's match limit), naming the file.
    /// When several files fail, the first failing file in path order is
    /// reported ([`first_failure_by_path`]), whatever order the workers
    /// reached them in.
    pub fn compute<'a>(
        oracle: &StructuralOracle,
        files: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> anyhow::Result<Self> {
        let files: Vec<(&str, &str)> = files.into_iter().collect();
        let over_cap = structural::over_cap_count(files.iter().map(|&(p, t)| (p, t.len() as u64)));
        let results: Vec<(&str, anyhow::Result<FileMatches>)> = files
            .par_iter()
            .map_init(OracleScratch::new, |scratch, &(path, text)| {
                (path, oracle.file_matches(scratch, path, text))
            })
            .collect();
        let reports = first_failure_by_path(results)?;

        let mut answers = OracleAnswers {
            over_cap,
            ..OracleAnswers::default()
        };
        // Pre-populated with the REGISTERED keys only: a registered pair with
        // no match reads as empty, and an absent key still means "the oracle
        // has no query for it", which `definition` reports as an error.
        // Never seed any other key.
        for q in structural::query_sources() {
            answers
                .definition
                .entry(q.pattern)
                .or_default()
                .entry(q.lang)
                .or_default();
        }
        for spec in INTENTS {
            for &lang in spec.langs {
                answers
                    .intent
                    .entry(spec.pattern)
                    .or_default()
                    .entry(lang)
                    .or_default();
            }
        }
        for (path, matches) in reports {
            let FileMatches::Scored(report) = matches else {
                continue;
            };
            *answers.scored_files.entry(report.lang).or_default() += 1;
            for (pattern, lines) in report.definition {
                insert_match(&mut answers.definition, pattern, report.lang, path, lines);
            }
            for (pattern, lines) in report.intent {
                insert_match(&mut answers.intent, pattern, report.lang, path, lines);
            }
        }
        Ok(answers)
    }

    /// The definition oracle's matches for `(pattern, lang)`.
    ///
    /// # Errors
    ///
    /// The oracle has no query for `(pattern, lang)` (golden integrity
    /// rejects such an entry first).
    pub fn definition(&self, pattern: &str, lang: OracleLang) -> anyhow::Result<&MatchFiles> {
        lookup(&self.definition, pattern, lang)
            .with_context(|| format!("the structural oracle has no {lang} query for {pattern}"))
    }

    /// The intent oracle's matches for `(pattern, lang)`, if it has one.
    pub fn intent(&self, pattern: &str, lang: OracleLang) -> Option<&MatchFiles> {
        lookup(&self.intent, pattern, lang)
    }

    /// Scored files (inside the AST universe) in `lang`.
    pub fn scored_files(&self, lang: OracleLang) -> u64 {
        self.scored_files.get(&lang).copied().unwrap_or(0)
    }

    /// The oracle's count of universe files over the AST size cap, in every
    /// language skim's `ast_coverage` accounting counts.
    pub fn over_cap(&self) -> u64 {
        self.over_cap
    }
}

/// `map`'s answer for `(pattern, lang)`; `None` when the oracle has no query
/// for the pair.
fn lookup<'m>(map: &'m AnswerMap, pattern: &str, lang: OracleLang) -> Option<&'m MatchFiles> {
    map.get(pattern).and_then(|by_lang| by_lang.get(&lang))
}

fn insert_match(
    map: &mut AnswerMap,
    pattern: &'static str,
    lang: OracleLang,
    path: &str,
    lines: Vec<u32>,
) {
    if !lines.is_empty() {
        map.entry(pattern)
            .or_default()
            .entry(lang)
            .or_default()
            .insert(path.to_string(), lines);
    }
}

/// Every file's oracle result (`(path, result)`, in any order), or — when
/// some file failed — the error of the first failing file in path order, so
/// the error reported never depends on which rayon worker reached a failing
/// file first. With more than one failure the error says how many files
/// failed.
///
/// # Errors
///
/// Some file's result is an error.
fn first_failure_by_path<T>(
    results: Vec<(&str, anyhow::Result<T>)>,
) -> anyhow::Result<Vec<(&str, T)>> {
    let failed = results.iter().filter(|(_, r)| r.is_err()).count();
    let mut reports = Vec::with_capacity(results.len() - failed);
    let mut first: Option<(&str, anyhow::Error)> = None;
    for (path, result) in results {
        match result {
            Ok(report) => reports.push((path, report)),
            Err(e) if first.as_ref().is_none_or(|(p, _)| path < *p) => first = Some((path, e)),
            Err(_) => {}
        }
    }
    match first {
        None => Ok(reports),
        Some((_, e)) if failed == 1 => Err(e),
        Some((_, e)) => Err(e.context(format!(
            "{failed} universe files failed the structural oracle (the first by path is reported)"
        ))),
    }
}

/// Everything the structural checks of one corpus judge.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StructuralEvidence {
    pub answers: OracleAnswers,
    /// skim's `--ast <pattern>` answer (every row, every language) for each
    /// catalog pattern ([`called_patterns`]; empty when the corpus has no
    /// `[[ast]]` entry).
    pub patterns: BTreeMap<String, AstPage>,
}

/// The patterns skim is called for, once per corpus with an `[[ast]]` entry
/// and by `golden-gen --ast`: every pattern of `catalog` (sorted, each once),
/// whether or not the oracle covers it and whether or not the corpus has an
/// entry for it. A pattern left out would have its rows neither scored nor
/// counted.
pub fn called_patterns(catalog: &[CatalogPattern]) -> Vec<&'static str> {
    catalog
        .iter()
        .map(|p| p.name)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

// ============================================================================
// Splitting skim's rows
// ============================================================================

/// Whether `row`'s file is in `lang` by the oracle's extension table (so
/// `.tsx` is its own language).
fn is_in(row: &ResultRow, lang: OracleLang) -> bool {
    structural::classify(&row.path) == LangClass::Oracle(lang)
}

/// The rows of `page` whose file is in `lang` by the oracle's extension
/// table (so `.tsx` is its own language), borrowed, in skim's order.
pub fn lang_rows(page: &ResultPage, lang: OracleLang) -> impl Iterator<Item = &ResultRow> {
    page.rows.iter().filter(move |r| is_in(r, lang))
}

/// The rows of `page` whose file is in `lang` ([`lang_rows`]), in skim's
/// order: an `[[ast]]` entry's full list.
pub fn rows_in(page: &ResultPage, lang: OracleLang) -> ResultPage {
    ResultPage {
        rows: lang_rows(page, lang).cloned().collect(),
        has_more: page.has_more,
        verify_mode: page.verify_mode.clone(),
        degraded: page.degraded.clone(),
    }
}

/// The distinct files of `page`'s rows in `lang`: the files of
/// [`rows_in`], without copying a row.
pub fn files_in(page: &ResultPage, lang: OracleLang) -> BTreeSet<&str> {
    lang_rows(page, lang).map(|r| r.path.as_str()).collect()
}

/// The `(pattern, language)` pairs `targets` score: a row of skim's
/// `--ast <pattern>` answer in one of these pairs' languages is scored, any
/// other row is unscored ([`unscored_in`]).
pub fn scored_pairs<'a>(
    targets: impl IntoIterator<Item = &'a StructuralTarget>,
) -> BTreeSet<(&'a str, OracleLang)> {
    targets
        .into_iter()
        .map(|t| (t.pattern.as_str(), t.lang))
        .collect()
}

/// The rows of skim's `--ast <pattern>` answer `page` that no entry in
/// `scored` (its `(pattern, language)` pairs) scores, in skim's order: rows in
/// a language the oracle has no grammar for, rows in an extension skim never
/// AST-indexes, and rows in an oracle language with no entry for `pattern`.
pub fn unscored_in<'p>(
    pattern: &str,
    page: &'p ResultPage,
    scored: &BTreeSet<(&str, OracleLang)>,
) -> impl Iterator<Item = &'p ResultRow> {
    page.rows
        .iter()
        .filter(move |r| match structural::classify(&r.path) {
            LangClass::Oracle(lang) => !scored.contains(&(pattern, lang)),
            LangClass::Unscored { .. } | LangClass::NotIndexed { .. } => true,
        })
}

/// skim rows no `[[ast]]` entry scores ([`unscored_in`] over
/// [`scored_pairs`]), per pattern skim was called for (zero counts
/// included).
pub fn unscored_rows<'a>(
    targets: impl IntoIterator<Item = &'a StructuralTarget>,
    patterns: &BTreeMap<String, AstPage>,
) -> BTreeMap<String, u64> {
    let scored = scored_pairs(targets);
    patterns
        .iter()
        .map(|(pattern, call)| {
            let unscored = unscored_in(pattern, &call.page, &scored).count();
            (pattern.clone(), unscored as u64)
        })
        .collect()
}

// ============================================================================
// HARD checks (pure)
// ============================================================================

/// The distinct files of `rows`.
pub(crate) fn distinct_files(rows: &[ResultRow]) -> BTreeSet<&str> {
    rows.iter().map(|r| r.path.as_str()).collect()
}

/// How many of `skim`'s files `oracle` matches.
fn overlap(skim: &BTreeSet<&str>, oracle: &MatchFiles) -> usize {
    skim.iter().filter(|p| oracle.contains_key(**p)).count()
}

/// `structural.recall`: every file the oracle matches is returned.
pub fn check_recall(rows: &[ResultRow], oracle: &MatchFiles) -> CheckOutcome {
    let returned = distinct_files(rows);
    let missing: Vec<&str> = oracle
        .keys()
        .map(String::as_str)
        .filter(|p| !returned.contains(p))
        .collect();
    if missing.is_empty() {
        return CheckOutcome::Pass;
    }
    CheckOutcome::fail(format!(
        "missing {} of {} oracle file(s): {}",
        missing.len(),
        oracle.len(),
        sample(missing)
    ))
}

/// `structural.precision`: every returned file is an oracle match.
pub fn check_precision(rows: &[ResultRow], oracle: &MatchFiles) -> CheckOutcome {
    let extra: Vec<&str> = distinct_files(rows)
        .into_iter()
        .filter(|p| !oracle.contains_key(*p))
        .collect();
    if extra.is_empty() {
        return CheckOutcome::Pass;
    }
    CheckOutcome::fail(format!(
        "{} returned file(s) with no oracle match: {}",
        extra.len(),
        sample(extra)
    ))
}

/// `structural.coverage`: skim's `ast_coverage.size_excluded_files` equals
/// the oracle's over-cap count, and no file is undetermined (the oracle knows
/// every universe file's size and language, so an undetermined file is an
/// accounting gap).
pub fn check_coverage(coverage: &AstCoverage, oracle_over_cap: u64) -> CheckOutcome {
    let mut problems = Vec::new();
    if coverage.size_excluded_files != oracle_over_cap {
        let by_lang: Vec<String> = coverage
            .excluded_by_lang
            .iter()
            .map(|(lang, n)| format!("{lang} {n}"))
            .collect();
        problems.push(format!(
            "ast_coverage.size_excluded_files is {} (by language: {}), the oracle counts {oracle_over_cap} \
             universe file(s) over the {} AST cap",
            coverage.size_excluded_files,
            if by_lang.is_empty() {
                "none".to_string()
            } else {
                by_lang.join(", ")
            },
            byte_size(AST_SIZE_CAP_BYTES)
        ));
    }
    if coverage.undetermined_files > 0 {
        problems.push(format!(
            "ast_coverage.undetermined_files is {}, expected 0",
            coverage.undetermined_files
        ));
    }
    if problems.is_empty() {
        CheckOutcome::Pass
    } else {
        CheckOutcome::fail(problems.join("; "))
    }
}

/// A byte count for a message: whole mebibytes as `N MiB` (the AST size
/// cap, [`AST_SIZE_CAP_BYTES`], reads `1 MiB`), anything else as `N bytes`.
fn byte_size(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes > 0 && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes} bytes")
    }
}

// ============================================================================
// Measurements (pure)
// ============================================================================

/// Per-entry measurements of one `[[ast]]` entry (report.json and the
/// structural RATCHET values). File-level fractions are rounded to 4
/// decimals; an empty denominator reads as 1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuralSample {
    pub id: String,
    pub pattern: String,
    pub lang: OracleLang,
    /// The class the golden entry declares.
    pub precision_class: PrecisionClass,
    /// The golden entry is a false-positive guard (`expect_oracle_empty =
    /// true`): its oracle is empty by declaration, so `oracle_files` 0 is
    /// expected and the entry guards precision only. Serialized only when
    /// true, so an ordinary entry carries no such key.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expect_oracle_empty: bool,
    /// Files with at least one definition-oracle match.
    pub oracle_files: u64,
    /// Distinct files skim returned in this language.
    pub skim_files: u64,
    /// `|skim ∩ oracle| / |oracle|`.
    pub recall: f64,
    /// `|skim ∩ oracle| / |skim|`.
    pub precision: f64,
    /// Files with an intent-oracle match (the nested-loop patterns only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_files: Option<u64>,
    /// `|skim ∩ intent| / |intent|`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_recall: Option<f64>,
    /// `|skim ∩ intent| / |skim|`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_precision: Option<f64>,
    /// skim rows whose `line` is the first line of an oracle match in that
    /// row's file (a count; a row without `line` never counts). skim anchors
    /// on the child node that completes a declared n-gram, the oracle on the
    /// construct the description names, so multi-line constructs read low by
    /// design (see the #541 handoff).
    pub line_on_match: u64,
}

/// `|a ∩ b| / |denominator|` rounded, 1 for an empty denominator.
fn ratio(hits: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        round4(hits as f64 / denominator as f64)
    }
}

/// Measure one `[[ast]]` entry: `rows` are skim's rows in `target.lang`.
///
/// # Errors
///
/// The oracle has no query for the target.
pub fn measure(
    id: &str,
    target: &StructuralTarget,
    rows: &[ResultRow],
    answers: &OracleAnswers,
) -> anyhow::Result<StructuralSample> {
    let oracle = answers.definition(&target.pattern, target.lang)?;
    let skim = distinct_files(rows);
    let hits = overlap(&skim, oracle);
    let intent = answers.intent(&target.pattern, target.lang);
    let line_on_match = rows
        .iter()
        .filter(|r| {
            r.line
                .is_some_and(|line| oracle.get(&r.path).is_some_and(|l| l.contains(&line)))
        })
        .count();
    Ok(StructuralSample {
        id: id.to_string(),
        pattern: target.pattern.clone(),
        lang: target.lang,
        precision_class: target.precision,
        expect_oracle_empty: target.expect_oracle_empty,
        oracle_files: oracle.len() as u64,
        skim_files: skim.len() as u64,
        recall: ratio(hits, oracle.len()),
        precision: ratio(hits, skim.len()),
        intent_files: intent.map(|intent| intent.len() as u64),
        intent_recall: intent.map(|intent| ratio(overlap(&skim, intent), intent.len())),
        intent_precision: intent.map(|intent| ratio(overlap(&skim, intent), skim.len())),
        line_on_match: line_on_match as u64,
    })
}

/// The three HARD outcomes and the measurements of one `[[ast]]` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryScore {
    pub recall: CheckOutcome,
    pub precision: CheckOutcome,
    pub coverage: CheckOutcome,
    pub sample: StructuralSample,
}

/// Score one `[[ast]]` entry whose rows (skim's rows in `target.lang`) are
/// `rows`.
///
/// # Errors
///
/// The oracle has no query for the target, or skim was not called for its
/// pattern.
pub fn score_entry(
    id: &str,
    target: &StructuralTarget,
    rows: &[ResultRow],
    evidence: &StructuralEvidence,
) -> anyhow::Result<EntryScore> {
    let oracle = evidence.answers.definition(&target.pattern, target.lang)?;
    let call = evidence
        .patterns
        .get(&target.pattern)
        .with_context(|| format!("skim was not called with --ast {}", target.pattern))?;
    Ok(EntryScore {
        recall: check_recall(rows, oracle),
        precision: check_precision(rows, oracle),
        coverage: check_coverage(&call.coverage, evidence.answers.over_cap()),
        sample: measure(id, target, rows, &evidence.answers)?,
    })
}

/// Whether an `[[ast]]` entry is vacuous: the oracle matches no file and skim
/// returns no row in its language. Every check passes on such an entry, so it
/// is a golden error, never a pass.
///
/// A false-positive guard (`expect_oracle_empty`) is judged differently: its
/// oracle is empty by declaration, and skim returning nothing is the state it
/// guards (recall and precision both read 1 over an empty denominator; a row
/// fails `structural.precision`), so it is vacuous only when the corpus has
/// no scored file in its language (none at all, or every one over the AST
/// size cap): then there is no file a false positive could land on, and the
/// guard would pass forever while measuring nothing.
/// [`unexpected_oracle_matches`] keeps the flag honest.
///
/// `rows` are skim's rows in the target's language. The gate applies this
/// rule to every `[[ast]]` entry, and `golden-gen --ast` to every entry it
/// could propose, so it never proposes one the gate would refuse.
///
/// # Errors
///
/// The oracle has no query for the target.
pub fn is_vacuous<'r>(
    target: &StructuralTarget,
    rows: impl IntoIterator<Item = &'r ResultRow>,
    answers: &OracleAnswers,
) -> anyhow::Result<bool> {
    let oracle = answers.definition(&target.pattern, target.lang)?;
    if target.expect_oracle_empty {
        return Ok(answers.scored_files(target.lang) == 0);
    }
    Ok(rows.into_iter().next().is_none() && oracle.is_empty())
}

/// The oracle's matches for an `[[ast]]` entry flagged `expect_oracle_empty`
/// when there are any: the flag is stale (the entry would measure recall while
/// declaring it has none to measure), which is a golden error. `None` for an
/// unflagged entry or an empty oracle.
///
/// # Errors
///
/// The oracle has no query for the target.
pub fn unexpected_oracle_matches<'a>(
    target: &StructuralTarget,
    answers: &'a OracleAnswers,
) -> anyhow::Result<Option<&'a MatchFiles>> {
    let oracle = answers.definition(&target.pattern, target.lang)?;
    Ok((target.expect_oracle_empty && !oracle.is_empty()).then_some(oracle))
}

// ============================================================================
// Report sections
// ============================================================================

/// skim's `ast_coverage` next to the oracle's over-cap count (one corpus).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageComparison {
    /// Universe files over the AST size cap
    /// ([`structural::AST_SIZE_CAP_BYTES`]) in a language skim's accounting
    /// counts ([`structural::over_cap_count`]).
    pub oracle_over_cap: u64,
    /// Every distinct `ast_coverage.size_excluded_files` over this corpus's
    /// `--ast` calls, sorted (one value: every call reads the same manifest).
    pub skim_size_excluded_files: Vec<u64>,
    /// Every distinct `ast_coverage.undetermined_files`, sorted.
    pub skim_undetermined_files: Vec<u64>,
}

/// The coverage comparison over `evidence`'s calls, or `None` when skim was
/// not called with `--ast` (no `[[ast]]` entry).
pub fn coverage_comparison(evidence: &StructuralEvidence) -> Option<CoverageComparison> {
    if evidence.patterns.is_empty() {
        return None;
    }
    let distinct = |get: fn(&AstCoverage) -> u64| -> Vec<u64> {
        evidence
            .patterns
            .values()
            .map(|call| get(&call.coverage))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    Some(CoverageComparison {
        oracle_over_cap: evidence.answers.over_cap(),
        skim_size_excluded_files: distinct(|c| c.size_excluded_files),
        skim_undetermined_files: distinct(|c| c.undetermined_files),
    })
}

/// Why a catalog pattern has no structural score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UncoveredCause {
    /// The oracle has no query for it
    /// ([`crate::scoreboard::catalog::catalog_coverage`]).
    NoOracle,
    /// The oracle has a query, but no corpus in the run has an `[[ast]]`
    /// entry for it.
    NoEntry,
}

/// A catalog pattern no `[[ast]]` entry scores; it stays under ADR-007
/// manual dog-food.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UncoveredPattern {
    pub name: String,
    pub cause: UncoveredCause,
    pub reason: String,
}

/// Every pattern of `catalog` with no `[[ast]]` entry in `goldens` (the
/// corpora of one run), sorted by name: those the oracle cannot encode, with
/// the oracle's reason, and those it covers but no corpus scores.
pub fn uncovered_patterns<'a>(
    catalog: &[CatalogPattern],
    goldens: impl IntoIterator<Item = &'a GoldenFile>,
) -> Vec<UncoveredPattern> {
    let entered: BTreeSet<&str> = goldens
        .into_iter()
        .flat_map(|g| g.asts.iter().map(|e| e.pattern.as_str()))
        .collect();
    catalog_coverage(catalog)
        .into_iter()
        .filter_map(|(name, coverage)| match coverage {
            PatternCoverage::Uncovered { reason } => Some(UncoveredPattern {
                name: name.to_string(),
                cause: UncoveredCause::NoOracle,
                reason: reason.to_string(),
            }),
            PatternCoverage::Covered { langs } if !entered.contains(name) => {
                let langs: Vec<&str> = langs.iter().map(|l| l.as_str()).collect();
                Some(UncoveredPattern {
                    name: name.to_string(),
                    cause: UncoveredCause::NoEntry,
                    reason: format!(
                        "no [[ast]] entry on any corpus in this run (the oracle covers it in: {}); \
                         golden-gen proposes an entry only where the oracle or skim finds a file",
                        langs.join(", ")
                    ),
                })
            }
            PatternCoverage::Covered { .. } => None,
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code — fail loudly
#[path = "structural_metrics_tests.rs"]
mod tests;
