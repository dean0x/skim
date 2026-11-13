//! Per-query HARD checks, RATCHET measurements and aggregates for the search
//! scoreboard (#203).
//!
//! - [`plan`] turns a golden file into [`PlannedQuery`]s and fixes, per
//!   entry, which HARD checks run ([`PlannedQuery::checks`]). The ledger is
//!   validated against the same plan, so a ledger entry can never name a
//!   check that does not run.
//! - The `check_*` functions are pure: skim's pages in, a
//!   [`CheckOutcome`] with a one-line detail out. Most compare a page with
//!   an oracle; the `refs.*`, `clash.*`, `stakes.*` and
//!   `concept.first_right_top5` checks compare it with an answer frozen in
//!   the golden file. A check whose data the page does not state fails: it
//!   never passes by default.
//! - [`evaluate`] scores one corpus: outcomes and per-query samples. It reads
//!   the text universe of every tracked text file, the one universe the
//!   engine and the oracle share. [`ratchet_values`] turns samples (one
//!   corpus, or every corpus for the aggregate) into RATCHET values, and
//!   [`reference_values`] into the INFO reference rows (the `rg -nw`
//!   columns and the first relevant file of a concept), which no baseline
//!   holds and no gate reads.
//! - [`RATCHET_METRICS`] defines each RATCHET metric's good direction and
//!   tolerance; [`compare_ratchet`] applies them for the gate and `bless`. A
//!   row whose entries state no value (for example `roles.precision.go` when no
//!   page labels a Go file) is absent from the values, and an
//!   absent row never fails the gate.
//!
//! No function here imports skim's own search code: every expectation comes
//! from the oracles (`oracle.rs`) over the oracle's universe, or from the
//! golden file.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;
use regex::Regex;
use rskim_oracle::roles::role_sites;
use rskim_oracle::structural::{LangClass, OracleLang, classify};
use serde::{Deserialize, Serialize};

use crate::scoreboard::MAX_PAGES;
use crate::scoreboard::fmt::{round4, sample};
use crate::scoreboard::golden::{
    ClashEntry, DefSite, GoldenFile, RefsEntry, SiteRole, StakesEntry, StakesExpect, StakesRun,
};
use crate::scoreboard::oracle::{
    LexicalQuery, baseline_alphabetical, baseline_occurrence_count, ground_truth, line_truth,
    simulate_rg_fixed, simulate_rg_word,
};
use crate::scoreboard::runner::{EntryObservation, Sweep};
use crate::scoreboard::types::{
    CheckId, CheckOutcome, Cost, EntryKind, ResultPage, ResultRow, Role,
};
use crate::scoreboard::universe::{LEFT_OUT_REASONS, TextUniverse};

// ============================================================================
// Plan
// ============================================================================

/// What an entry ranks against.
#[derive(Debug, Clone)]
pub enum Target {
    /// `[[ident]]`: the defining file and line.
    Definition(DefSite),
    /// `[[concept]]`: a file is relevant iff `relevant` matches its text;
    /// `first` lists the acceptable first files, when the entry freezes them.
    Relevance {
        relevant: Regex,
        first: Option<Vec<String>>,
    },
    /// `[[lexical]]` / `[[pagination]]` / `[[prefix]]`, and the entries
    /// judged against frozen answers (`[[refs]]`, `[[clash]]`, `[[stakes]]`).
    None,
}

/// What judges an entry's full list.
#[derive(Debug, Clone)]
pub enum Oracle {
    /// A lexical predicate over the oracle universe (`oracle.rs`).
    Lexical(LexicalQuery),
    /// The frozen sites of one name (`[[refs]]`).
    Refs(RefsEntry),
    /// The frozen definitions and calls of a name with two or more
    /// definitions (`[[clash]]`).
    Clash(ClashEntry),
    /// The regex oracle's reading of a plain-text query (`[[stakes]]`).
    Stakes(StakesOracle),
}

/// A `[[stakes]]` entry with its regex-oracle reading, resolved once by
/// [`plan`].
#[derive(Debug, Clone)]
pub struct StakesOracle {
    pub entry: StakesEntry,
    /// The lines the entry's flags and query ask for, or the usage error the
    /// search must report.
    pub run: StakesRun,
}

/// One golden entry, resolved into what the runner calls and what the
/// metrics check.
#[derive(Debug, Clone)]
pub struct PlannedQuery {
    pub id: String,
    pub kind: EntryKind,
    /// The text query.
    pub query: String,
    /// The flags the runner passes before `--`: a golden `lang` as `-t`, or a
    /// `[[stakes]]` entry's own flags.
    pub flags: Vec<String>,
    /// What judges the full result set.
    pub oracle: Oracle,
    /// `[[pagination]]` sweep limits / `[[prefix]]` limits.
    pub limits: Vec<u32>,
    pub target: Target,
}

impl PlannedQuery {
    /// The lexical oracle query, if the entry has one.
    pub fn lexical_oracle(&self) -> Option<&LexicalQuery> {
        match &self.oracle {
            Oracle::Lexical(query) => Some(query),
            _ => None,
        }
    }

    /// The frozen sites of a `[[refs]]` entry.
    pub fn refs_entry(&self) -> Option<&RefsEntry> {
        match &self.oracle {
            Oracle::Refs(entry) => Some(entry),
            _ => None,
        }
    }

    /// The frozen definitions and calls of a `[[clash]]` entry.
    pub fn clash_entry(&self) -> Option<&ClashEntry> {
        match &self.oracle {
            Oracle::Clash(entry) => Some(entry),
            _ => None,
        }
    }

    /// The regex oracle reading of a `[[stakes]]` entry.
    pub fn stakes_oracle(&self) -> Option<&StakesOracle> {
        match &self.oracle {
            Oracle::Stakes(stakes) => Some(stakes),
            _ => None,
        }
    }

    /// The acceptable first files of a `[[concept]]` entry that freezes them.
    pub fn first_files(&self) -> Option<&[String]> {
        match &self.target {
            Target::Relevance {
                first: Some(first), ..
            } => Some(first),
            _ => None,
        }
    }

    /// The HARD checks that run on this entry, in [`CheckId::ALL`] order.
    pub fn checks(&self) -> Vec<CheckId> {
        CheckId::ALL
            .iter()
            .copied()
            .filter(|c| self.runs(*c))
            .collect()
    }

    /// Whether `check` runs on this entry:
    /// - `lexical.recall` / `precision` / `silent_fn`: the full list has a
    ///   lexical oracle ground truth;
    /// - `pagination.*`: `[[pagination]]` entries;
    /// - `order.prefix_consistent`: `[[prefix]]` entries;
    /// - `order.score_monotone`: every entry that ranks by score;
    /// - `results.unique_paths`: every entry (every list it fetches);
    /// - `refs.*` and `universe.accounted`: `[[refs]]` entries; `clash.*`:
    ///   `[[clash]]` entries;
    /// - `lexical.recall_tracked`: `[[lexical]]` entries;
    /// - `ident.def_first`: `[[ident]]` entries;
    /// - `concept.first_right_top5`: `[[concept]]` entries with `first`;
    /// - `stakes.lines` / `files` / `counts` / `exit`: the `[[stakes]]`
    ///   entries whose `expect` names it.
    ///
    /// A check runs only on the entry kinds it [applies to](CheckId::applies_to),
    /// so the ledger and the plan never disagree.
    pub fn runs(&self, check: CheckId) -> bool {
        if !check.applies_to(self.kind) {
            return false;
        }
        match check {
            CheckId::LexicalRecall | CheckId::LexicalPrecision | CheckId::LexicalSilentFn => {
                self.lexical_oracle().is_some()
            }
            CheckId::PaginationComplete
            | CheckId::PaginationDisjoint
            | CheckId::PaginationOrdered
            | CheckId::PaginationHasMoreHonest => self.kind == EntryKind::Pagination,
            CheckId::OrderPrefixConsistent => self.kind == EntryKind::Prefix,
            CheckId::OrderScoreMonotone | CheckId::ResultsUniquePaths => true,
            CheckId::RefsRecall | CheckId::RefsCallsRecall | CheckId::UniverseAccounted => {
                self.refs_entry().is_some()
            }
            CheckId::ClashDefsComplete | CheckId::ClashDefCount | CheckId::ClashCallsRecall => {
                self.clash_entry().is_some()
            }
            CheckId::LexicalRecallTracked => self.lexical_oracle().is_some(),
            CheckId::IdentDefFirst => matches!(self.target, Target::Definition(_)),
            CheckId::ConceptFirstRightTop5 => self.first_files().is_some(),
            CheckId::StakesLines => self.stakes_expects(StakesExpect::Lines),
            CheckId::StakesFiles => self.stakes_expects(StakesExpect::Files),
            CheckId::StakesCounts => self.stakes_expects(StakesExpect::Counts),
            CheckId::StakesExit => self.stakes_expects(StakesExpect::Exit),
        }
    }

    /// Whether this is a `[[stakes]]` entry with `expect = expect`.
    fn stakes_expects(&self, expect: StakesExpect) -> bool {
        self.stakes_oracle()
            .is_some_and(|s| s.entry.expect == expect)
    }

    /// Whether this entry gets a text-mode run for the byte metrics
    /// (`[[ident]]` and `[[concept]]` only).
    pub fn measures_text(&self) -> bool {
        !matches!(self.target, Target::None)
    }
}

/// Resolve every entry of `golden`, in [`GoldenFile::ids`] order.
///
/// # Errors
///
/// An entry whose flags or regex do not resolve (golden integrity reports the
/// same problems first).
pub fn plan(golden: &GoldenFile) -> anyhow::Result<Vec<PlannedQuery>> {
    let mut out = Vec::new();
    for e in &golden.idents {
        out.push(PlannedQuery {
            id: e.id.clone(),
            kind: EntryKind::Ident,
            query: e.query.clone(),
            flags: Vec::new(),
            oracle: Oracle::Lexical(
                LexicalQuery::new(&e.query, None).with_context(|| e.id.clone())?,
            ),
            limits: Vec::new(),
            target: Target::Definition(e.def.clone()),
        });
    }
    for e in &golden.concepts {
        out.push(PlannedQuery {
            id: e.id.clone(),
            kind: EntryKind::Concept,
            query: e.query.clone(),
            flags: Vec::new(),
            oracle: Oracle::Lexical(
                LexicalQuery::new(&e.query, None).with_context(|| e.id.clone())?,
            ),
            limits: Vec::new(),
            target: Target::Relevance {
                relevant: e.relevance()?,
                first: e.first.clone(),
            },
        });
    }
    for e in &golden.lexicals {
        let flags = e.flags().with_context(|| e.id.clone())?;
        out.push(PlannedQuery {
            id: e.id.clone(),
            kind: EntryKind::Lexical,
            query: e.query.clone(),
            flags: flags.to_args(),
            oracle: Oracle::Lexical(e.oracle_query().with_context(|| e.id.clone())?),
            limits: Vec::new(),
            target: Target::None,
        });
    }
    for e in &golden.paginations {
        out.push(PlannedQuery {
            id: e.id.clone(),
            kind: EntryKind::Pagination,
            query: e.query.clone(),
            flags: Vec::new(),
            oracle: Oracle::Lexical(
                LexicalQuery::new(&e.query, None).with_context(|| e.id.clone())?,
            ),
            limits: e.limits.clone(),
            target: Target::None,
        });
    }
    for e in &golden.prefixes {
        out.push(PlannedQuery {
            id: e.id.clone(),
            kind: EntryKind::Prefix,
            query: e.query.clone(),
            flags: Vec::new(),
            oracle: Oracle::Lexical(
                LexicalQuery::new(&e.query, None).with_context(|| e.id.clone())?,
            ),
            limits: e.limits.clone(),
            target: Target::None,
        });
    }
    for e in &golden.refs {
        out.push(frozen_entry(
            &e.id,
            EntryKind::Refs,
            &e.query,
            Vec::new(),
            Oracle::Refs(e.clone()),
        ));
    }
    for e in &golden.clashes {
        out.push(frozen_entry(
            &e.id,
            EntryKind::Clash,
            &e.query,
            Vec::new(),
            Oracle::Clash(e.clone()),
        ));
    }
    for e in &golden.stakes {
        let stakes = StakesOracle {
            run: e.run().with_context(|| e.id.clone())?,
            entry: e.clone(),
        };
        out.push(frozen_entry(
            &e.id,
            EntryKind::Stakes,
            &e.query,
            e.flags.clone(),
            Oracle::Stakes(stakes),
        ));
    }
    Ok(out)
}

/// An entry judged against a frozen answer: one text query, no ranking
/// target, no pagination or prefix limits.
fn frozen_entry(
    id: &str,
    kind: EntryKind,
    query: &str,
    flags: Vec<String>,
    oracle: Oracle,
) -> PlannedQuery {
    PlannedQuery {
        id: id.to_string(),
        kind,
        query: query.to_string(),
        flags,
        oracle,
        limits: Vec::new(),
        target: Target::None,
    }
}

// ============================================================================
// HARD checks (pure)
// ============================================================================

fn paths(rows: &[ResultRow]) -> Vec<&str> {
    rows.iter().map(|r| r.path.as_str()).collect()
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

/// `lexical.silent_fn`: no ground-truth file is missing. The envelope has no
/// channel that discloses a missed file (every file of the universe is
/// read), so a miss is always silent.
pub fn check_silent_fn(page: &ResultPage, gt: &[String]) -> CheckOutcome {
    let missing = missing(&page.rows, gt);
    if missing.is_empty() {
        return CheckOutcome::Pass;
    }
    CheckOutcome::fail(format!(
        "{} ground-truth file(s) missing and nothing in the output says so: {}",
        missing.len(),
        sample(missing)
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

/// `results.unique_paths`: no path appears twice within one list — the full
/// list, each `--limit` list, and each page of each sweep (a path on two
/// different pages is `pagination.disjoint`). A repeated row is invisible to
/// the set-based checks (recall, precision), and `order.score_monotone`
/// passes a repeat that ties.
pub fn check_unique_paths(
    full: &[ResultRow],
    sweeps: &[Sweep],
    limited: &[(u32, ResultPage)],
) -> CheckOutcome {
    let limited_lists = limited
        .iter()
        .map(|(limit, page)| (format!("limit {limit}"), page.rows.as_slice()));
    let sweep_pages = sweeps.iter().flat_map(|s| {
        s.pages.iter().map(move |p| {
            (
                format!("L={} offset {}", s.limit, p.offset),
                p.page.rows.as_slice(),
            )
        })
    });
    let problems: Vec<String> = std::iter::once(("full list".to_string(), full))
        .chain(limited_lists)
        .chain(sweep_pages)
        .filter_map(|(list, rows)| {
            duplicate_problem(&paths(rows)).map(|problem| format!("{list}: {problem}"))
        })
        .collect();
    outcome_of(&problems)
}

// ----------------------------------------------------------------------------
// HARD checks of the frozen answers (#540)
//
// Each check reads the page's line-level answers (`occurrences`,
// `definitions`, `searched_files`, ...). An answer the output does not state
// fails the check: nothing here treats a missing value as a pass.
// ----------------------------------------------------------------------------

/// `path:line`, for a failure detail.
fn site_label(path: &str, line: u32) -> String {
    format!("{path}:{line}")
}

/// A `(path, line)` list as a short sample.
fn sample_sites(sites: &[String]) -> String {
    sample(sites.iter().map(String::as_str))
}

/// `refs.recall`: every frozen site of the name is among the page's
/// occurrences (the role is `refs.calls_recall`'s business).
pub fn check_refs_recall(page: &ResultPage, entry: &RefsEntry) -> CheckOutcome {
    let present: BTreeSet<(&str, u32)> = page
        .occurrences
        .iter()
        .map(|o| (o.path.as_str(), o.line))
        .collect();
    let missing: Vec<String> = entry
        .sites
        .iter()
        .filter(|s| !present.contains(&(s.path.as_str(), s.line)))
        .map(|s| site_label(&s.path, s.line))
        .collect();
    if missing.is_empty() {
        return CheckOutcome::Pass;
    }
    CheckOutcome::fail(format!(
        "missing {} of {} frozen site(s) of {}: {}",
        missing.len(),
        entry.sites.len(),
        entry.query,
        sample_sites(&missing)
    ))
}

/// The frozen call sites that are not an occurrence with the role `call`,
/// each with what the page says about it.
fn calls_not_labelled<'a>(
    page: &ResultPage,
    calls: impl Iterator<Item = (&'a str, u32)>,
) -> (usize, Vec<String>) {
    let mut roles: BTreeMap<(&str, u32), Vec<Option<Role>>> = BTreeMap::new();
    for o in &page.occurrences {
        roles
            .entry((o.path.as_str(), o.line))
            .or_default()
            .push(o.role);
    }
    let mut total = 0;
    let mut problems = Vec::new();
    for (path, line) in calls {
        total += 1;
        let site = site_label(path, line);
        match roles.get(&(path, line)) {
            None => problems.push(format!("{site} (absent)")),
            Some(found) => {
                if let Some(role) = found.iter().find(|r| **r != Some(Role::Call)) {
                    problems.push(format!(
                        "{site} (role {})",
                        role.map_or("none", Role::as_str)
                    ));
                }
            }
        }
    }
    (total, problems)
}

/// The failure of a call-recall check, or a pass.
fn calls_outcome(total: usize, problems: &[String], what: &str) -> CheckOutcome {
    if problems.is_empty() {
        return CheckOutcome::Pass;
    }
    CheckOutcome::fail(format!(
        "{} of {total} frozen call site(s){what} are not labelled call: {}",
        problems.len(),
        sample_sites(problems)
    ))
}

/// `refs.calls_recall`: every frozen call site of the name is an occurrence
/// with the role `call`. A call that is absent, or labelled anything else,
/// fails.
pub fn check_refs_calls_recall(page: &ResultPage, entry: &RefsEntry) -> CheckOutcome {
    let calls = entry
        .sites
        .iter()
        .filter(|s| s.role == SiteRole::Call)
        .map(|s| (s.path.as_str(), s.line));
    let (total, problems) = calls_not_labelled(page, calls);
    calls_outcome(total, &problems, "")
}

/// `refs.call_precision` for one entry: of the occurrences labelled `call`
/// (the call group), the fraction that are frozen call sites. `None` when the
/// page labels no call, so the entry has no value to record.
pub fn refs_call_precision(page: &ResultPage, entry: &RefsEntry) -> Option<f64> {
    let frozen: BTreeSet<(&str, u32)> = entry
        .sites
        .iter()
        .filter(|s| s.role == SiteRole::Call)
        .map(|s| (s.path.as_str(), s.line))
        .collect();
    let group: BTreeSet<(&str, u32)> = page
        .occurrences
        .iter()
        .filter(|o| o.role == Some(Role::Call))
        .map(|o| (o.path.as_str(), o.line))
        .collect();
    if group.is_empty() {
        return None;
    }
    Some(group.intersection(&frozen).count() as f64 / group.len() as f64)
}

/// How many role labels one language has, and how many of them the role
/// oracle confirms.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoleTally {
    /// The `def`, `call` and `import` labels the page puts on files of the
    /// language.
    pub labelled: u64,
    /// The labels whose line the role oracle lists for the same role.
    pub confirmed: u64,
}

impl RoleTally {
    fn add(&mut self, other: RoleTally) {
        self.labelled += other.labelled;
        self.confirmed += other.confirmed;
    }
}

/// The `roles.precision.<lang>` input of one page: for each oracle language,
/// the `def`, `call` and `import` labels of `page.occurrences` and how many of
/// them the role oracle (`rskim_oracle::roles::role_sites`) confirms.
///
/// `name` is the bare identifier the page was searched for. A label is
/// confirmed when the oracle lists the occurrence's line under the same role
/// for `name` in that file. `source_of` gives the text of a file of the oracle
/// universe. A labelled file that is not there has no oracle reading, so its
/// labels count as not confirmed. Other roles (`ref`, `comment`, `string`,
/// `text`, `partial`) and files of a language without a role oracle are not
/// counted. A line counts once per role.
///
/// # Errors
///
/// The role oracle fails on a file (harness error, never a silent miss).
pub fn page_role_tallies<'a>(
    page: &ResultPage,
    name: &str,
    source_of: impl Fn(&str) -> Option<&'a str>,
) -> anyhow::Result<BTreeMap<OracleLang, RoleTally>> {
    let mut labels: BTreeMap<&str, BTreeSet<(u32, Role)>> = BTreeMap::new();
    for o in &page.occurrences {
        if let Some(role @ (Role::Def | Role::Call | Role::Import)) = o.role {
            labels
                .entry(o.path.as_str())
                .or_default()
                .insert((o.line, role));
        }
    }
    let mut tallies: BTreeMap<OracleLang, RoleTally> = BTreeMap::new();
    for (path, lines) in labels {
        let LangClass::Oracle(lang) = classify(path) else {
            continue;
        };
        let sites = match source_of(path) {
            Some(source) => Some(
                role_sites(lang, source, name)
                    .with_context(|| format!("role oracle on {path} for {name:?}"))?,
            ),
            None => None,
        };
        let tally = tallies.entry(lang).or_default();
        for (line, role) in lines {
            tally.labelled += 1;
            let confirmed = sites.as_ref().is_some_and(|s| match role {
                Role::Def => s.defs.contains(&line),
                Role::Call => s.calls.contains(&line),
                _ => s.imports.contains(&line),
            });
            tally.confirmed += u64::from(confirmed);
        }
    }
    Ok(tallies)
}

/// `clash.defs_complete`: every frozen definition is among the definition
/// sites the page reports.
pub fn check_clash_defs_complete(page: &ResultPage, entry: &ClashEntry) -> CheckOutcome {
    let Some(definitions) = &page.definitions else {
        return CheckOutcome::fail("the output reports no definitions");
    };
    let reported: BTreeSet<(&str, u32)> = definitions
        .sites
        .iter()
        .map(|s| (s.path.as_str(), s.line))
        .collect();
    let missing: Vec<String> = entry
        .defs
        .iter()
        .filter(|d| !reported.contains(&(d.path.as_str(), d.line)))
        .map(|d| site_label(&d.path, d.line))
        .collect();
    if missing.is_empty() {
        return CheckOutcome::Pass;
    }
    CheckOutcome::fail(format!(
        "missing {} of {} frozen definition(s) of {}: {}",
        missing.len(),
        entry.defs.len(),
        entry.query,
        sample_sites(&missing)
    ))
}

/// `clash.def_count`: the page states a definition count, and it equals the
/// frozen number of definitions. A count marked `at_least` is also accepted
/// when it is a valid lower bound: it is not above the frozen number, and the
/// frozen number is within the files the parse budget left unlabelled
/// (`count <= frozen <= count + unlabelled_files`).
pub fn check_clash_def_count(page: &ResultPage, entry: &ClashEntry) -> CheckOutcome {
    let frozen = entry.defs.len() as u64;
    let Some(definitions) = &page.definitions else {
        return CheckOutcome::fail("the output reports no definitions");
    };
    let Some(count) = definitions.count else {
        return CheckOutcome::fail("the output states no definitions.count");
    };
    if count == frozen {
        return CheckOutcome::Pass;
    }
    let unlabelled = page.unlabelled_files;
    if definitions.at_least && count <= frozen && frozen <= count.saturating_add(unlabelled) {
        return CheckOutcome::Pass;
    }
    let stated = if definitions.at_least {
        format!("at least {count} (with {unlabelled} unlabelled file(s))")
    } else {
        count.to_string()
    };
    CheckOutcome::fail(format!(
        "definitions.count is {stated}, expected {frozen} definition(s) of {}",
        entry.query
    ))
}

/// `clash.calls_recall`: every frozen call site of either definition is an
/// occurrence with the role `call` (calls are not split by definition).
pub fn check_clash_calls_recall(page: &ResultPage, entry: &ClashEntry) -> CheckOutcome {
    let calls = entry.calls.iter().map(|c| (c.path.as_str(), c.line));
    let (total, problems) = calls_not_labelled(page, calls);
    calls_outcome(total, &problems, " of either definition")
}

/// `lexical.recall_tracked`: every file of the text universe (`gt`, with no
/// language allow-list) that holds the query is returned.
pub fn check_recall_tracked(rows: &[ResultRow], gt: &[String]) -> CheckOutcome {
    let missing = missing(rows, gt);
    if missing.is_empty() {
        return CheckOutcome::Pass;
    }
    CheckOutcome::fail(format!(
        "missing {} of {} text-universe file(s): {}",
        missing.len(),
        gt.len(),
        sample(missing)
    ))
}

/// `universe.accounted`: the searched files plus each left-out reason count
/// equal the oracle text universe's: the file count and every reason count
/// (a reason the page omits counts 0).
pub fn check_universe_accounted(page: &ResultPage, text: &TextUniverse) -> CheckOutcome {
    let Some(searched) = page.searched_files else {
        return CheckOutcome::fail("the output states no searched_files");
    };
    let Some(left_out) = &page.left_out else {
        return CheckOutcome::fail("the output states no left_out");
    };
    let oracle_files = text.len() as u64;
    let mut problems = Vec::new();
    if searched != oracle_files {
        problems.push(format!(
            "searched_files is {searched}, the oracle text universe has {oracle_files}"
        ));
    }
    let reasons: BTreeSet<&str> = LEFT_OUT_REASONS
        .iter()
        .copied()
        .chain(left_out.keys().map(String::as_str))
        .collect();
    for reason in reasons {
        let got = left_out.get(reason).copied().unwrap_or(0);
        let want = text.left_out_count(reason);
        if got != want {
            problems.push(format!("left_out.{reason} is {got}, the oracle has {want}"));
        }
    }
    if problems.is_empty() {
        return CheckOutcome::Pass;
    }
    let reported = searched.saturating_add(left_out.values().sum::<u64>());
    let oracle = oracle_files.saturating_add(text.left_out().values().sum::<u64>());
    CheckOutcome::fail(format!(
        "{} (accounted {reported} files, the oracle {oracle})",
        problems.join("; ")
    ))
}

/// `ident.def_first`: the first definition site is the frozen definition
/// line (the path and the line that holds the name).
pub fn check_def_first(page: &ResultPage, def: &DefSite) -> CheckOutcome {
    let Some(definitions) = &page.definitions else {
        return CheckOutcome::fail("the output reports no definitions");
    };
    let Some(first) = definitions.sites.first() else {
        return CheckOutcome::fail("definitions.sites is empty");
    };
    if first.path == def.path && first.line == def.line {
        return CheckOutcome::Pass;
    }
    CheckOutcome::fail(format!(
        "the first definition is {}, expected {}",
        site_label(&first.path, first.line),
        site_label(&def.path, def.line)
    ))
}

/// How many files `concept.first_right_top5` looks at.
const FIRST_FILES_RANK: usize = 5;

/// `concept.first_right_top5`: a frozen first file is among the first five
/// files of the list.
pub fn check_first_right_top5(rows: &[ResultRow], first: &[String]) -> CheckOutcome {
    let top: Vec<&str> = rows
        .iter()
        .take(FIRST_FILES_RANK)
        .map(|r| r.path.as_str())
        .collect();
    if top.iter().any(|p| first.iter().any(|f| f == p)) {
        return CheckOutcome::Pass;
    }
    CheckOutcome::fail(format!(
        "none of the first files ({}) is among the first {FIRST_FILES_RANK} files ({})",
        sample(first.iter().map(String::as_str)),
        sample(top)
    ))
}

/// The failure of a set comparison, or a pass.
fn set_outcome(what: &str, missing: &[String], extra: &[String]) -> CheckOutcome {
    if missing.is_empty() && extra.is_empty() {
        return CheckOutcome::Pass;
    }
    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!(
            "{} {what} missing: {}",
            missing.len(),
            sample_sites(missing)
        ));
    }
    if !extra.is_empty() {
        parts.push(format!(
            "{} {what} not in the oracle: {}",
            extra.len(),
            sample_sites(extra)
        ));
    }
    CheckOutcome::fail(parts.join("; "))
}

/// `stakes.lines`: the page's occurrences are exactly the lines the regex
/// oracle finds (`truth`).
pub fn check_stakes_lines(page: &ResultPage, truth: &BTreeSet<(String, u32)>) -> CheckOutcome {
    let got: BTreeSet<(String, u32)> = page
        .occurrences
        .iter()
        .map(|o| (o.path.clone(), o.line))
        .collect();
    let label = |(path, line): &(String, u32)| site_label(path, *line);
    let missing: Vec<String> = truth.difference(&got).map(label).collect();
    let extra: Vec<String> = got.difference(truth).map(label).collect();
    set_outcome("matching line(s)", &missing, &extra)
}

/// `stakes.files`: the page's files are exactly the files that hold a line
/// the regex oracle finds.
pub fn check_stakes_files(page: &ResultPage, truth: &BTreeSet<(String, u32)>) -> CheckOutcome {
    let want: BTreeSet<&str> = truth.iter().map(|(p, _)| p.as_str()).collect();
    let got: BTreeSet<&str> = page.rows.iter().map(|r| r.path.as_str()).collect();
    let missing: Vec<String> = want.difference(&got).map(|p| p.to_string()).collect();
    let extra: Vec<String> = got.difference(&want).map(|p| p.to_string()).collect();
    set_outcome("file(s)", &missing, &extra)
}

/// `stakes.counts`: the page's matching-line count of each file equals the
/// regex oracle's, and the page counts no other file.
pub fn check_stakes_counts(page: &ResultPage, truth: &BTreeSet<(String, u32)>) -> CheckOutcome {
    let mut want: BTreeMap<&str, u64> = BTreeMap::new();
    for (path, _) in truth {
        *want.entry(path.as_str()).or_insert(0) += 1;
    }
    let paths: BTreeSet<&str> = want
        .keys()
        .copied()
        .chain(page.line_counts.keys().map(String::as_str))
        .collect();
    let problems: Vec<String> = paths
        .into_iter()
        .filter_map(|path| {
            let got = page.line_counts.get(path).copied().unwrap_or(0);
            let expected = want.get(path).copied().unwrap_or(0);
            (got != expected).then(|| format!("{path} has {got} line(s), the oracle {expected}"))
        })
        .collect();
    if problems.is_empty() {
        return CheckOutcome::Pass;
    }
    CheckOutcome::fail(format!(
        "{} file count(s) differ: {}",
        problems.len(),
        sample_sites(&problems)
    ))
}

/// `stakes.exit`: the run's exit code equals the frozen one.
pub fn check_stakes_exit(page: &ResultPage, expected: u8) -> CheckOutcome {
    if page.exit_code == i32::from(expected) {
        return CheckOutcome::Pass;
    }
    CheckOutcome::fail(format!(
        "the exit code is {}, expected {expected}",
        page.exit_code
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

/// Files shown more than once in `shown` (`pagination.disjoint` over one
/// sweep's pages, `results.unique_paths` over one list).
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

/// Bytes of `stdout` up to and including the line that starts `def <path>:`,
/// the line of the labelled view that shows a definition of `path`, or
/// `None` if no line does.
pub fn bytes_through_def_line(stdout: &[u8], path: &str) -> Option<u64> {
    let prefix = format!("def {path}:");
    let mut offset = 0usize;
    for line in stdout.split_inclusive(|&b| b == b'\n') {
        offset += line.len();
        if line.starts_with(prefix.as_bytes()) {
            return u64::try_from(offset).ok();
        }
    }
    None
}

/// Bytes of `stdout` up to and including the end of `path`'s block in the
/// word view, or `None` if `path` has no block.
///
/// The block starts at the header line `<path>  <word counts>`. Its matching
/// lines are `<path>:<line>: …`. It ends at the first line that does not
/// start with `path` (followed by `:` or two spaces, so `a.rs` does not
/// claim the lines of `a.rs.orig`).
pub fn bytes_through_file_block(stdout: &[u8], path: &str) -> Option<u64> {
    let starts_block = |line: &[u8]| line.starts_with(format!("{path}  ").as_bytes());
    let continues_block = |line: &[u8]| {
        line.strip_prefix(path.as_bytes())
            .is_some_and(|rest| rest.starts_with(b":") || rest.starts_with(b"  "))
    };
    let mut offset = 0usize;
    let mut in_block = false;
    for line in stdout.split_inclusive(|&b| b == b'\n') {
        if in_block && !continues_block(line) {
            break;
        }
        in_block = in_block || starts_block(line);
        offset += line.len();
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
    /// Text-mode stdout + stderr bytes at the default limit.
    pub text_bytes: u64,
    /// Text-mode bytes through the line that shows the definition (`None` =
    /// not on page 1: a miss).
    pub first_correct_bytes: Option<u64>,
    /// Simulated `rg -n -F --sort path` output bytes.
    pub rg_text_bytes: u64,
    /// Simulated rg bytes through the definition line.
    pub rg_first_correct_bytes: Option<u64>,
    /// Simulated `rg -n -w --sort path` output bytes.
    pub rgw_text_bytes: u64,
    /// Simulated `rg -nw` bytes through the definition line.
    pub rgw_first_correct_bytes: Option<u64>,
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
    /// Text-mode bytes through the end of the first relevant file's block in
    /// the word view (`None` = no relevant file shown on page 1).
    #[serde(default)]
    pub first_relevant_bytes: Option<u64>,
    pub rg_text_bytes: u64,
    pub rgw_text_bytes: u64,
}

/// Per-query measurements of one `[[refs]]` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefsSample {
    pub id: String,
    /// Of the occurrences the page labels `call`, the fraction that are
    /// frozen call sites ([`refs_call_precision`]). `None` when the page
    /// labels no call.
    pub call_precision: Option<f64>,
}

/// Everything one corpus contributes to the RATCHET values.
#[derive(Debug, Clone, PartialEq)]
pub struct CorpusSamples {
    /// Oracle universe size − the envelope's `searched_files`.
    pub universe_delta: i64,
    /// Left-out reasons whose counts differ between oracle and envelope.
    pub skipped_mismatch: u64,
    /// Files of the oracle universe the engine searched: the smaller of the
    /// two counts.
    pub indexed_tracked: u64,
    /// Files of the oracle universe.
    pub tracked_text: u64,
    pub idents: Vec<IdentSample>,
    pub concepts: Vec<ConceptSample>,
    /// Per-`[[refs]]`-entry measurements, in plan order.
    pub refs: Vec<RefsSample>,
    /// Per language, the role labels of the `[[refs]]` and `[[clash]]` pages and
    /// the ones the role oracle confirms ([`page_role_tallies`]).
    pub roles: BTreeMap<OracleLang, RoleTally>,
    /// The work counters of every call that states them, in plan order.
    pub costs: Vec<Cost>,
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

/// Every fixed RATCHET metric (tolerance 0 except bytes at ±3%).
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
    // The rows below have a value only when the search output states the
    // data they read; a row with no value is absent from the values, and an
    // absent row is neither a failure nor a baseline entry.
    def(
        "refs.call_precision",
        Direction::HigherBetter,
        Tolerance::Exact,
        "ratchet (frozen calls in the call group)",
    ),
    def(
        "cost.files_parsed_median",
        Direction::LowerBetter,
        Tolerance::Exact,
        "ratchet (exact counter)",
    ),
    def(
        "cost.bytes_parsed_median",
        Direction::LowerBetter,
        Tolerance::Exact,
        "ratchet (exact counter)",
    ),
    def(
        "cost.bytes_read_median",
        Direction::LowerBetter,
        Tolerance::Exact,
        "ratchet (exact counter)",
    ),
    // Precision of the `def`, `call` and `import` labels of the `[[refs]]` and
    // `[[clash]]` pages, judged by the role oracle (`page_role_tallies`). A
    // language with no label in any corpus has no value.
    def(
        "roles.precision.rust",
        Direction::HigherBetter,
        Tolerance::Exact,
        "ratchet (role oracle)",
    ),
    def(
        "roles.precision.python",
        Direction::HigherBetter,
        Tolerance::Exact,
        "ratchet (role oracle)",
    ),
    def(
        "roles.precision.typescript",
        Direction::HigherBetter,
        Tolerance::Exact,
        "ratchet (role oracle)",
    ),
    def(
        "roles.precision.tsx",
        Direction::HigherBetter,
        Tolerance::Exact,
        "ratchet (role oracle)",
    ),
    def(
        "roles.precision.javascript",
        Direction::HigherBetter,
        Tolerance::Exact,
        "ratchet (role oracle)",
    ),
    def(
        "roles.precision.go",
        Direction::HigherBetter,
        Tolerance::Exact,
        "ratchet (role oracle)",
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
/// `refs.call_precision` and `cost.*` are present only when at least one
/// entry states a value; a row with no value is left out, not set to zero.
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
    let searched: u64 = samples.iter().map(|s| s.indexed_tracked).sum();
    let text: u64 = samples.iter().map(|s| s.tracked_text).sum();
    put(
        &mut out,
        "coverage.tracked_text",
        Some(if text == 0 {
            1.0
        } else {
            searched as f64 / text as f64
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
    put_refs_values(&mut out, samples);
    put_role_values(&mut out, samples);
    put_cost_values(&mut out, samples);
    out
}

/// `refs.call_precision`: the mean over the `[[refs]]` entries whose page
/// labels a call. No such entry, no value.
fn put_refs_values(out: &mut BTreeMap<String, f64>, samples: &[&CorpusSamples]) {
    put(
        out,
        "refs.call_precision",
        mean(
            samples
                .iter()
                .flat_map(|s| s.refs.iter())
                .filter_map(|r| r.call_precision),
        ),
    );
}

/// `roles.precision.<lang>`: the labels the role oracle confirms over the labels
/// the pages put on files of the language, pooled over every corpus. A language
/// with no label has no value.
fn put_role_values(out: &mut BTreeMap<String, f64>, samples: &[&CorpusSamples]) {
    for lang in OracleLang::ALL {
        let mut pooled = RoleTally::default();
        for tally in samples.iter().filter_map(|s| s.roles.get(&lang)) {
            pooled.add(*tally);
        }
        put(
            out,
            &format!("roles.precision.{lang}"),
            (pooled.labelled > 0).then(|| pooled.confirmed as f64 / pooled.labelled as f64),
        );
    }
}

/// `cost.*_median`: the median of each work counter over every call that
/// states its cost. No such call, no value.
fn put_cost_values(out: &mut BTreeMap<String, f64>, samples: &[&CorpusSamples]) {
    let costs: Vec<&Cost> = samples.iter().flat_map(|s| s.costs.iter()).collect();
    let median = |counter: fn(&Cost) -> u64| {
        let values: Vec<u64> = costs.iter().map(|c| counter(c)).collect();
        percentile(&values, 0.5)
    };
    put(out, "cost.files_parsed_median", median(|c| c.files_parsed));
    put(out, "cost.bytes_parsed_median", median(|c| c.bytes_parsed));
    put(out, "cost.bytes_read_median", median(|c| c.bytes_read));
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

/// The INFO reference rows (#540): the byte columns of the `rg -nw`
/// simulation, the search an agent types for a bare identifier, and the bytes
/// to the first relevant file of a concept. They are not RATCHET metrics: the
/// report records them under `info`, outside the baseline, so no move of
/// theirs ever gates. A row with no value is absent.
///
/// - `bytes.rgw_text_median`: output bytes over every ranking entry (idents
///   and concepts);
/// - `bytes.rgw_first_correct_median`: output bytes through the definition
///   line, over the idents whose definition `rg -nw` shows;
/// - `bytes.first_relevant_median`: output bytes through the first relevant
///   file's block, over the concepts that show a relevant file.
///
/// Pooled over `samples` the way [`ratchet_values`] pools them, rounded to 4
/// decimal places.
pub fn reference_values(samples: &[&CorpusSamples]) -> BTreeMap<String, f64> {
    let idents = || samples.iter().flat_map(|s| s.idents.iter());
    let concepts = || samples.iter().flat_map(|s| s.concepts.iter());
    let text: Vec<u64> = idents()
        .map(|i| i.rgw_text_bytes)
        .chain(concepts().map(|c| c.rgw_text_bytes))
        .collect();
    let first_correct: Vec<u64> = idents().filter_map(|i| i.rgw_first_correct_bytes).collect();
    let first_relevant: Vec<u64> = concepts().filter_map(|c| c.first_relevant_bytes).collect();
    let mut out = BTreeMap::new();
    put(&mut out, "bytes.rgw_text_median", percentile(&text, 0.5));
    put(
        &mut out,
        "bytes.rgw_first_correct_median",
        percentile(&first_correct, 0.5),
    );
    put(
        &mut out,
        "bytes.first_relevant_median",
        percentile(&first_relevant, 0.5),
    );
    out
}

// ============================================================================
// Evaluation
// ============================================================================

/// One corpus's raw results: HARD outcomes (before the ledger) and samples.
#[derive(Debug, Clone, PartialEq)]
pub struct CorpusEvaluation {
    /// `(id, check, outcome)`, sorted by `(id, check)`.
    pub outcomes: Vec<(String, CheckId, CheckOutcome)>,
    pub samples: CorpusSamples,
    /// The files the engine searched (`searched_files`).
    pub searched_files: u64,
    /// The files the engine left out, by reason (`left_out`).
    pub left_out: BTreeMap<String, u64>,
}

/// `plan` and `observations` paired entry by entry: observations follow the
/// plan, one per entry, in order. Everything that walks both checks this
/// first, so an entry never goes unjudged or is judged by another entry's
/// rows.
///
/// # Errors
///
/// The two differ in length, or an observation's id is not its planned
/// entry's.
pub fn paired<'a>(
    plan: &'a [PlannedQuery],
    observations: &'a [EntryObservation],
) -> anyhow::Result<impl Iterator<Item = (&'a PlannedQuery, &'a EntryObservation)>> {
    anyhow::ensure!(
        plan.len() == observations.len(),
        "{} planned entries but {} observations",
        plan.len(),
        observations.len()
    );
    if let Some((q, obs)) = plan
        .iter()
        .zip(observations)
        .find(|(q, obs)| q.id != obs.id)
    {
        anyhow::bail!(
            "observation {} does not match planned entry {}",
            obs.id,
            q.id
        );
    }
    Ok(plan.iter().zip(observations))
}

/// Score one corpus against its oracle universe `text`.
///
/// # Errors
///
/// `observations` does not follow `plan` one-for-one (same length, same
/// ids), no observation states the engine's file accounting, or an entry
/// lacks an observation its checks need.
pub fn evaluate(
    text: &TextUniverse,
    plan: &[PlannedQuery],
    observations: &[EntryObservation],
) -> anyhow::Result<CorpusEvaluation> {
    let mut outcomes = Vec::new();
    let mut idents = Vec::new();
    let mut concepts = Vec::new();
    let mut refs = Vec::new();
    let mut costs = Vec::new();
    let mut roles: BTreeMap<OracleLang, RoleTally> = BTreeMap::new();

    for (q, obs) in paired(plan, observations)? {
        let oracle = eval_oracle(q, text);
        let pagination = (q.kind == EntryKind::Pagination)
            .then(|| check_pagination(&obs.full.rows, &obs.sweeps));
        let inputs = CheckInputs {
            oracle: &oracle,
            pagination: pagination.as_ref(),
            text,
        };
        for check in q.checks() {
            let outcome =
                run_check(check, q, obs, &inputs).with_context(|| format!("{}: {check}", q.id))?;
            outcomes.push((q.id.clone(), check, outcome));
        }

        let gt = oracle.ground_truth();
        match &q.target {
            Target::Definition(def) => {
                idents.push(measure_ident(q, def, obs, text, gt)?);
            }
            Target::Relevance { relevant, .. } => {
                concepts.push(measure_concept(q, relevant, obs, text, gt)?);
            }
            Target::None => {}
        }
        let role_name = q
            .refs_entry()
            .map(|e| e.query.as_str())
            .or_else(|| q.clash_entry().map(|e| e.query.as_str()));
        if let Some(name) = role_name {
            for (lang, tally) in page_role_tallies(&obs.full, name, |p| text.text(p))
                .with_context(|| format!("{}: roles.precision", q.id))?
            {
                roles.entry(lang).or_default().add(tally);
            }
        }
        if let Some(entry) = q.refs_entry() {
            refs.push(RefsSample {
                id: q.id.clone(),
                call_precision: refs_call_precision(&obs.full, entry).map(round4),
            });
        }
        costs.extend(obs.full.cost);
    }
    outcomes.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));

    let (searched, left_out) = accounting(observations)?;
    let oracle_files = u64::try_from(text.len())?;
    Ok(CorpusEvaluation {
        outcomes,
        samples: CorpusSamples {
            universe_delta: i64::try_from(oracle_files)? - i64::try_from(searched)?,
            skipped_mismatch: left_out_mismatches(text, left_out),
            indexed_tracked: oracle_files.min(searched),
            tracked_text: oracle_files,
            idents,
            concepts,
            refs,
            roles,
            costs,
        },
        searched_files: searched,
        left_out: left_out.clone(),
    })
}

/// The engine's file accounting (`searched_files` and `left_out`), from the
/// first observation that states both. Every call of a corpus searches the
/// same files, so one observation speaks for the corpus.
///
/// # Errors
///
/// No observation states both.
fn accounting(observations: &[EntryObservation]) -> anyhow::Result<(u64, &BTreeMap<String, u64>)> {
    observations
        .iter()
        .find_map(|o| Some((o.full.searched_files?, o.full.left_out.as_ref()?)))
        .context("no observation states searched_files and left_out")
}

/// Left-out reasons whose count differs between the engine (`skim`) and the
/// oracle (a reason one side omits counts 0).
fn left_out_mismatches(text: &TextUniverse, skim: &BTreeMap<String, u64>) -> u64 {
    let reasons: BTreeSet<&str> = LEFT_OUT_REASONS
        .iter()
        .copied()
        .chain(skim.keys().map(String::as_str))
        .collect();
    reasons
        .into_iter()
        .filter(|r| text.left_out_count(r) != skim.get(*r).copied().unwrap_or(0))
        .count() as u64
}

/// What an entry's oracle says about its full list, resolved once per entry
/// by [`eval_oracle`]. The checks ([`run_check`]) and the ranking
/// measurements read it.
#[derive(Debug)]
enum OracleEval {
    /// A lexical oracle (`oracle.rs`).
    Lexical {
        /// Its ground truth over the universe.
        gt: Vec<String>,
    },
    /// A frozen answer or the regex oracle (`[[refs]]`, `[[clash]]`,
    /// `[[stakes]]`): the checks read the entry itself, and nothing ratchets
    /// per entry.
    Frozen,
}

impl OracleEval {
    /// The lexical ground truth, if the entry has a lexical oracle.
    fn ground_truth(&self) -> Option<&[String]> {
        match self {
            OracleEval::Lexical { gt } => Some(gt),
            OracleEval::Frozen => None,
        }
    }
}

/// Resolve `q`'s oracle for [`evaluate`]: a lexical oracle's ground truth
/// over `text`, or the entry itself for a frozen answer.
fn eval_oracle(q: &PlannedQuery, text: &TextUniverse) -> OracleEval {
    match &q.oracle {
        Oracle::Lexical(query) => OracleEval::Lexical {
            gt: ground_truth(text.files(), query),
        },
        Oracle::Refs(_) | Oracle::Clash(_) | Oracle::Stakes(_) => OracleEval::Frozen,
    }
}

/// What an entry's checks read besides its observation.
struct CheckInputs<'a> {
    /// The entry's oracle, resolved once ([`eval_oracle`]).
    oracle: &'a OracleEval,
    /// The pagination outcomes of a `[[pagination]]` entry.
    pagination: Option<&'a PaginationOutcomes>,
    /// The text universe, for the checks that compare with it.
    text: &'a TextUniverse,
}

fn run_check(
    check: CheckId,
    q: &PlannedQuery,
    obs: &EntryObservation,
    inputs: &CheckInputs<'_>,
) -> anyhow::Result<CheckOutcome> {
    let gt = || {
        inputs
            .oracle
            .ground_truth()
            .context("no oracle ground truth")
    };
    let pagination = || inputs.pagination.context("no pagination sweep");
    let refs = || q.refs_entry().context("not a [[refs]] entry");
    let clash = || q.clash_entry().context("not a [[clash]] entry");
    let stakes = || q.stakes_oracle().context("not a [[stakes]] entry");
    let stakes_truth = || -> anyhow::Result<BTreeSet<(String, u32)>> {
        match &stakes()?.run {
            StakesRun::Search { query, filter } => Ok(line_truth(
                inputs.text.files().filter(|(path, _)| filter.keeps(path)),
                query,
            )),
            StakesRun::Usage(reason) => {
                anyhow::bail!("the oracle reads this query as a usage error ({reason})")
            }
        }
    };
    Ok(match check {
        CheckId::LexicalRecall => check_recall(&obs.full.rows, gt()?),
        CheckId::LexicalPrecision => check_precision(&obs.full.rows, gt()?),
        CheckId::LexicalSilentFn => check_silent_fn(&obs.full, gt()?),
        CheckId::PaginationComplete => pagination()?.complete.clone(),
        CheckId::PaginationDisjoint => pagination()?.disjoint.clone(),
        CheckId::PaginationOrdered => pagination()?.ordered.clone(),
        CheckId::PaginationHasMoreHonest => pagination()?.has_more_honest.clone(),
        CheckId::OrderPrefixConsistent => check_prefix(&obs.full.rows, &obs.limited),
        CheckId::OrderScoreMonotone => check_score_monotone(&obs.full.rows),
        CheckId::ResultsUniquePaths => {
            check_unique_paths(&obs.full.rows, &obs.sweeps, &obs.limited)
        }
        CheckId::RefsRecall => check_refs_recall(&obs.full, refs()?),
        CheckId::RefsCallsRecall => check_refs_calls_recall(&obs.full, refs()?),
        CheckId::ClashDefsComplete => check_clash_defs_complete(&obs.full, clash()?),
        CheckId::ClashDefCount => check_clash_def_count(&obs.full, clash()?),
        CheckId::ClashCallsRecall => check_clash_calls_recall(&obs.full, clash()?),
        CheckId::LexicalRecallTracked => check_recall_tracked(&obs.full.rows, gt()?),
        CheckId::UniverseAccounted => check_universe_accounted(&obs.full, inputs.text),
        CheckId::IdentDefFirst => match &q.target {
            Target::Definition(def) => check_def_first(&obs.full, def),
            Target::Relevance { .. } | Target::None => anyhow::bail!("not an [[ident]] entry"),
        },
        CheckId::ConceptFirstRightTop5 => check_first_right_top5(
            &obs.full.rows,
            q.first_files()
                .context("a [[concept]] entry with no `first`")?,
        ),
        CheckId::StakesLines => check_stakes_lines(&obs.full, &stakes_truth()?),
        CheckId::StakesFiles => check_stakes_files(&obs.full, &stakes_truth()?),
        CheckId::StakesCounts => check_stakes_counts(&obs.full, &stakes_truth()?),
        CheckId::StakesExit => check_stakes_exit(
            &obs.full,
            stakes()?
                .entry
                .exit
                .context("a [[stakes]] entry with expect = \"exit\" has no `exit`")?,
        ),
    })
}

fn text_bytes(obs: &EntryObservation) -> anyhow::Result<(Vec<u8>, u64)> {
    let text = obs
        .text
        .as_ref()
        .context("no text-mode output for a ranking entry")?;
    let total = text.stdout.len() + text.stderr.len();
    Ok((text.stdout.clone(), u64::try_from(total)?))
}

/// 1-based rank of `path` in `ranked`.
fn rank_in(ranked: &[String], path: &str) -> Option<u64> {
    ranked.iter().position(|p| p == path).map(|i| i as u64 + 1)
}

fn measure_ident(
    q: &PlannedQuery,
    def: &DefSite,
    obs: &EntryObservation,
    text: &TextUniverse,
    gt: Option<&[String]>,
) -> anyhow::Result<IdentSample> {
    let query = q.query.as_str();
    let gt = gt.context("an ident entry has no ground truth")?;
    let rows = &obs.full.rows;
    let position = rows.iter().position(|r| r.path == def.path);
    let def_row = position.map(|i| &rows[i]);
    let (stdout, text_bytes) = text_bytes(obs)?;
    let rg = simulate_rg_fixed(text.files(), query);
    let rgw = simulate_rg_word(text.files(), query)?;
    Ok(IdentSample {
        id: q.id.clone(),
        rank: position.map(|i| i as u64 + 1),
        rank_baseline_alpha: rank_in(&baseline_alphabetical(gt), &def.path),
        rank_baseline_count: rank_in(
            &baseline_occurrence_count(gt, query, |p| text.text(p)),
            &def.path,
        ),
        anchor_eq_def: def_row.is_some_and(|r| r.line == Some(def.line)),
        def_line_in_snippet: def_row
            .is_some_and(|r| r.snippet.iter().any(|l| l.line_number == def.line)),
        text_bytes,
        first_correct_bytes: bytes_through_def_line(&stdout, &def.path),
        rg_text_bytes: u64::try_from(rg.total_bytes())?,
        rg_first_correct_bytes: rg
            .bytes_through(&def.path, def.line)
            .map(u64::try_from)
            .transpose()?,
        rgw_text_bytes: u64::try_from(rgw.total_bytes())?,
        rgw_first_correct_bytes: rgw
            .bytes_through(&def.path, def.line)
            .map(u64::try_from)
            .transpose()?,
    })
}

fn measure_concept(
    q: &PlannedQuery,
    relevant: &Regex,
    obs: &EntryObservation,
    text: &TextUniverse,
    gt: Option<&[String]>,
) -> anyhow::Result<ConceptSample> {
    let query = q.query.as_str();
    let gt = gt.context("a concept entry has no ground truth")?;
    let is_relevant = |path: &str| text.text(path).is_some_and(|t| relevant.is_match(t));
    let ranked = paths(&obs.full.rows);
    let alpha = baseline_alphabetical(gt);
    let alpha: Vec<&str> = alpha.iter().map(String::as_str).collect();
    let count = baseline_occurrence_count(gt, query, |p| text.text(p));
    let count: Vec<&str> = count.iter().map(String::as_str).collect();
    let (stdout, text_bytes) = text_bytes(obs)?;
    let first_relevant = ranked.iter().copied().find(|p| is_relevant(p));
    let rg = simulate_rg_fixed(text.files(), query);
    let rgw = simulate_rg_word(text.files(), query)?;
    Ok(ConceptSample {
        id: q.id.clone(),
        p5: round4(precision_at_k(&ranked, 5, is_relevant)),
        p10: round4(precision_at_k(&ranked, 10, is_relevant)),
        p5_baseline_alpha: round4(precision_at_k(&alpha, 5, is_relevant)),
        p10_baseline_alpha: round4(precision_at_k(&alpha, 10, is_relevant)),
        p5_baseline_count: round4(precision_at_k(&count, 5, is_relevant)),
        p10_baseline_count: round4(precision_at_k(&count, 10, is_relevant)),
        text_bytes,
        first_relevant_bytes: first_relevant.and_then(|p| bytes_through_file_block(&stdout, p)),
        rg_text_bytes: u64::try_from(rg.total_bytes())?,
        rgw_text_bytes: u64::try_from(rgw.total_bytes())?,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
#[path = "metrics_tests.rs"]
mod tests;
