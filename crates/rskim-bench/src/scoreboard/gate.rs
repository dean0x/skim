//! The ledger (`known_failures.toml`) and the gate verdict (#203).
//!
//! # Ledger
//!
//! ```toml
//! [[xfail]]
//! issue = "#544"                        # a filed ticket: "#" + number, never a placeholder
//! check = "pagination.has_more_honest"  # a HARD check's dotted name
//! ids = ["skim-G001", "skim-G002"]      # exact failing golden ids
//! note = "query.rs:676-693 pool_was_capped"   # optional
//! ```
//!
//! A `(check, id)` pair appears at most once in the file. Applied to a HARD
//! outcome ([`classify`]): a ledgered failure is XFAIL (fine), an unledgered
//! failure is FAIL, and a ledgered pass is XPASS — a gate failure asking to
//! promote (remove) the entry, so the ledger can only shrink silently in
//! the right direction. Golden integrity and [`unplanned_ledger_refs`]
//! reject entries that could never apply.
//!
//! # Gate
//!
//! [`evaluate`] fails on: unledgered HARD failures; XPASSes; a missing
//! baseline; a corpus, commit, golden file or HARD state that differs from
//! the baseline; and any RATCHET value outside tolerance in either
//! direction ("bless required"). Every failure names its check (or metric)
//! and query ids.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Context;
use serde::Deserialize;

use crate::scoreboard::baseline::{Baseline, BaselineCorpus, HardState};
use crate::scoreboard::golden::LedgerRef;
use crate::scoreboard::metrics::{PlannedQuery, RatchetChange, compare_ratchet};
use crate::scoreboard::report::{
    CheckRecord, CorpusReport, FailureKind, GateFailure, GateReport, GateStatus, Outcome,
};
use crate::scoreboard::types::{CheckId, CheckOutcome};

/// Failing ids whose detail is quoted in one gate failure message.
const DETAILED_IDS: usize = 3;

// ============================================================================
// Ledger
// ============================================================================

/// One `[[xfail]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerEntry {
    /// The filed ticket, `#<number>`.
    pub issue: String,
    pub check: CheckId,
    /// Golden ids expected to fail `check`.
    pub ids: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerFile {
    #[serde(default)]
    xfail: Vec<LedgerEntry>,
}

/// The parsed, validated ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ledger {
    entries: Vec<LedgerEntry>,
    /// `(check, id)` → issue.
    index: BTreeMap<(CheckId, String), String>,
}

impl Ledger {
    /// No expected failures.
    pub fn empty() -> Self {
        Ledger::default()
    }

    /// Parse and validate `known_failures.toml`.
    ///
    /// # Errors
    ///
    /// Invalid TOML, an unknown field or check, an issue that is not
    /// `#<number>`, an entry without ids, a blank id, or a `(check, id)`
    /// pair listed twice.
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        let file: LedgerFile = toml::from_str(raw).context("parsing known_failures.toml")?;
        let mut index = BTreeMap::new();
        for (n, e) in file.xfail.iter().enumerate() {
            let at = || format!("[[xfail]] #{} ({} {})", n + 1, e.issue, e.check);
            anyhow::ensure!(
                is_issue_ref(&e.issue),
                "{}: issue must be a filed ticket like \"#544\", got {:?}",
                at(),
                e.issue
            );
            anyhow::ensure!(!e.ids.is_empty(), "{}: ids is empty", at());
            for id in &e.ids {
                anyhow::ensure!(!id.trim().is_empty(), "{}: blank id", at());
                let previous = index.insert((e.check, id.clone()), e.issue.clone());
                anyhow::ensure!(
                    previous.is_none(),
                    "{}: ({}, {id}) is ledgered more than once",
                    at(),
                    e.check
                );
            }
        }
        Ok(Ledger {
            entries: file.xfail,
            index,
        })
    }

    /// Load `path`; a missing file is an empty ledger.
    ///
    /// # Errors
    ///
    /// Unreadable or invalid file.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(raw) => Self::parse(&raw).with_context(|| format!("in {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::empty()),
            Err(e) => Err(anyhow::anyhow!(e).context(format!("reading {}", path.display()))),
        }
    }

    /// Every entry, in file order.
    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    /// The issue ledgering `(check, id)`, if any.
    pub fn issue(&self, check: CheckId, id: &str) -> Option<&str> {
        self.index.get(&(check, id.to_string())).map(String::as_str)
    }

    /// Ledger refs whose id belongs to `corpus` (see [`corpus_of`]).
    pub fn refs_for_corpus(&self, corpus: &str, corpora: &[&str]) -> Vec<LedgerRef<'_>> {
        self.index
            .keys()
            .filter(|(_, id)| corpus_of(id, corpora) == Some(corpus))
            .map(|(check, id)| LedgerRef {
                check: *check,
                id: id.as_str(),
            })
            .collect()
    }

    /// Ledgered ids that belong to no corpus in `corpora` (sorted, unique).
    pub fn unassigned(&self, corpora: &[&str]) -> Vec<&str> {
        self.index
            .keys()
            .map(|(_, id)| id.as_str())
            .filter(|id| corpus_of(id, corpora).is_none())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// `#` followed by a positive decimal number without leading zeros.
fn is_issue_ref(issue: &str) -> bool {
    issue.strip_prefix('#').is_some_and(|n| {
        !n.is_empty()
            && !n.starts_with('0')
            && n.len() <= 9
            && n.bytes().all(|b| b.is_ascii_digit())
    })
}

/// The corpus a golden id belongs to: the longest corpus name `c` such that
/// the id is `<c>-<something>`.
pub fn corpus_of<'a>(id: &str, corpora: &[&'a str]) -> Option<&'a str> {
    corpora
        .iter()
        .copied()
        .filter(|c| {
            id.strip_prefix(c)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|rest| !rest.is_empty())
        })
        .max_by_key(|c| c.len())
}

/// Apply the ledger to one raw outcome.
pub fn classify(outcome: &CheckOutcome, issue: Option<&str>) -> Outcome {
    match (outcome.is_pass(), issue.is_some()) {
        (true, false) => Outcome::Pass,
        (false, false) => Outcome::Fail,
        (false, true) => Outcome::Xfail,
        (true, true) => Outcome::Xpass,
    }
}

/// Report records for raw outcomes (order preserved).
pub fn apply_ledger(
    outcomes: &[(String, CheckId, CheckOutcome)],
    ledger: &Ledger,
) -> Vec<CheckRecord> {
    outcomes
        .iter()
        .map(|(id, check, outcome)| {
            let issue = ledger.issue(*check, id);
            CheckRecord {
                id: id.clone(),
                check: *check,
                outcome: classify(outcome, issue),
                issue: issue.map(str::to_string),
                detail: outcome.detail().map(one_line),
            }
        })
        .collect()
}

/// Ledger refs naming a check that never runs on that entry (e.g.
/// `order.score_monotone` on a `--hot` list), which would XFAIL nothing.
pub fn unplanned_ledger_refs(refs: &[LedgerRef<'_>], plan: &[PlannedQuery]) -> Vec<String> {
    refs.iter()
        .filter(|r| !plan.iter().any(|q| q.id == r.id && q.runs(r.check)))
        .map(|r| {
            format!(
                "{}: ledger entry ({}, {}) names a check that never runs on this entry",
                r.id, r.check, r.id
            )
        })
        .collect()
}

fn one_line(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

// ============================================================================
// Gate
// ============================================================================

/// What the gate judges.
#[derive(Debug, Clone, Copy)]
pub struct GateInputs<'a> {
    /// Per-corpus results, ledger already applied.
    pub corpora: &'a [CorpusReport],
    /// Aggregate RATCHET values of this run.
    pub aggregate: &'a BTreeMap<String, f64>,
    /// Whether the run covered every corpus (only then are missing corpora
    /// and aggregate values compared with the baseline).
    pub complete: bool,
    pub baseline: Option<&'a Baseline>,
}

/// The gate verdict: every failure, sorted by kind, check, ids, message.
pub fn evaluate(inputs: &GateInputs<'_>) -> GateReport {
    let mut failures = hard_failures(inputs.corpora);
    match inputs.baseline {
        None => failures.push(GateFailure {
            kind: FailureKind::Baseline,
            check: None,
            ids: Vec::new(),
            message: "no baseline.json in the data dir; bless required \
                      (scoreboard bless --from <report.json>)"
                .to_string(),
        }),
        Some(baseline) => failures.extend(baseline_failures(inputs, baseline)),
    }
    failures.sort_by(|a, b| {
        (a.kind, &a.check, &a.ids, &a.message).cmp(&(b.kind, &b.check, &b.ids, &b.message))
    });
    GateReport {
        status: if failures.is_empty() {
            GateStatus::Pass
        } else {
            GateStatus::Fail
        },
        failures,
    }
}

/// Unledgered failures and XPASSes, one failure per check.
fn hard_failures(corpora: &[CorpusReport]) -> Vec<GateFailure> {
    let mut fails: BTreeMap<CheckId, Vec<&CheckRecord>> = BTreeMap::new();
    let mut xpasses: BTreeMap<CheckId, Vec<&CheckRecord>> = BTreeMap::new();
    for r in corpora.iter().flat_map(|c| c.checks.iter()) {
        match r.outcome {
            Outcome::Fail => fails.entry(r.check).or_default().push(r),
            Outcome::Xpass => xpasses.entry(r.check).or_default().push(r),
            Outcome::Pass | Outcome::Xfail => {}
        }
    }

    let mut out = Vec::new();
    for (check, mut records) in fails {
        records.sort_by(|a, b| a.id.cmp(&b.id));
        let mut details: Vec<String> = records
            .iter()
            .take(DETAILED_IDS)
            .map(|r| format!("{}: {}", r.id, r.detail.as_deref().unwrap_or("failed")))
            .collect();
        if records.len() > DETAILED_IDS {
            details.push(format!("+{} more", records.len() - DETAILED_IDS));
        }
        out.push(GateFailure {
            kind: FailureKind::Unledgered,
            check: Some(check.as_str().to_string()),
            ids: records.iter().map(|r| r.id.clone()).collect(),
            message: format!(
                "unledgered HARD failure — {}; fix it, or file a ticket and add an [[xfail]] \
                 entry to known_failures.toml",
                details.join("; ")
            ),
        });
    }
    for (check, mut records) in xpasses {
        records.sort_by(|a, b| a.id.cmp(&b.id));
        let issues: BTreeSet<&str> = records.iter().filter_map(|r| r.issue.as_deref()).collect();
        out.push(GateFailure {
            kind: FailureKind::Xpass,
            check: Some(check.as_str().to_string()),
            ids: records.iter().map(|r| r.id.clone()).collect(),
            message: format!(
                "XPASS: ledgered by {} but passing; promote: remove the ledger entry from \
                 known_failures.toml",
                issues.into_iter().collect::<Vec<_>>().join(", ")
            ),
        });
    }
    out
}

fn bless_failure(check: Option<String>, ids: Vec<String>, message: String) -> GateFailure {
    GateFailure {
        kind: FailureKind::Baseline,
        check,
        ids,
        message,
    }
}

/// Everything that differs from the baseline.
fn baseline_failures(inputs: &GateInputs<'_>, baseline: &Baseline) -> Vec<GateFailure> {
    let mut out = Vec::new();
    let mut flagged: BTreeSet<String> = BTreeSet::new();
    for c in inputs.corpora {
        let Some(b) = baseline.corpora.get(&c.name) else {
            out.push(bless_failure(
                None,
                Vec::new(),
                format!("corpus {} is not in the baseline; bless required", c.name),
            ));
            continue;
        };
        if b.commit != c.commit {
            out.push(bless_failure(
                None,
                Vec::new(),
                format!(
                    "corpus {}: commit {} -> {}; bless required",
                    c.name, b.commit, c.commit
                ),
            ));
        }
        if b.golden_sha256 != c.golden_sha256 {
            out.push(bless_failure(
                None,
                Vec::new(),
                format!("corpus {}: golden file changed; bless required", c.name),
            ));
        }
        out.extend(hard_state_changes(c, b));
        let ratchet = ratchet_failures(&c.name, &c.ratchet, &b.ratchet, &BTreeSet::new());
        flagged.extend(ratchet.iter().filter_map(|f| f.check.clone()));
        out.extend(ratchet);
    }
    if inputs.complete {
        let run: BTreeSet<&str> = inputs.corpora.iter().map(|c| c.name.as_str()).collect();
        for name in baseline.corpora.keys() {
            if !run.contains(name.as_str()) {
                out.push(bless_failure(
                    None,
                    Vec::new(),
                    format!("corpus {name} is in the baseline but was not run; bless required"),
                ));
            }
        }
        out.extend(ratchet_failures(
            "aggregate",
            inputs.aggregate,
            &baseline.aggregate,
            &flagged,
        ));
    }
    out
}

/// Blessable (pass / xfail) HARD states that differ from the baseline, one
/// failure per check. Current fail / xpass records are already gate
/// failures and are not repeated here.
fn hard_state_changes(c: &CorpusReport, b: &BaselineCorpus) -> Vec<GateFailure> {
    let mut current: BTreeMap<(&str, &str), Option<HardState>> = BTreeMap::new();
    for r in &c.checks {
        current.insert(
            (r.id.as_str(), r.check.as_str()),
            HardState::from_outcome(r.outcome),
        );
    }
    let mut changes: BTreeMap<&str, Vec<(String, String)>> = BTreeMap::new();
    for (id, checks) in &b.hard {
        for (check, &was) in checks {
            match current.get(&(id.as_str(), check.as_str())) {
                None => changes
                    .entry(check.as_str())
                    .or_default()
                    .push((id.clone(), format!("{} -> not run", was.as_str()))),
                Some(Some(now)) if *now != was => changes
                    .entry(check.as_str())
                    .or_default()
                    .push((id.clone(), format!("{} -> {}", was.as_str(), now.as_str()))),
                Some(_) => {}
            }
        }
    }
    for ((id, check), state) in &current {
        let known = b.hard.get(*id).is_some_and(|m| m.contains_key(*check));
        if let (false, Some(now)) = (known, state) {
            changes
                .entry(*check)
                .or_default()
                .push(((*id).to_string(), format!("new -> {}", now.as_str())));
        }
    }
    changes
        .into_iter()
        .map(|(check, mut entries)| {
            entries.sort();
            let text: Vec<String> = entries.iter().map(|(id, t)| format!("{id} {t}")).collect();
            bless_failure(
                Some(check.to_string()),
                entries.into_iter().map(|(id, _)| id).collect(),
                format!(
                    "HARD outcome differs from the baseline in {}: {}; bless required",
                    c.name,
                    text.join(", ")
                ),
            )
        })
        .collect()
}

/// RATCHET values outside tolerance (either direction), new, or no longer
/// measured; metrics in `skip` are left out.
fn ratchet_failures(
    scope: &str,
    current: &BTreeMap<String, f64>,
    baseline: &BTreeMap<String, f64>,
    skip: &BTreeSet<String>,
) -> Vec<GateFailure> {
    let names: BTreeSet<&String> = current.keys().chain(baseline.keys()).collect();
    names
        .into_iter()
        .filter(|name| !skip.contains(*name))
        .filter_map(|name| {
            let message = match (baseline.get(name), current.get(name)) {
                (Some(&b), Some(&c)) => {
                    let change = match compare_ratchet(name, b, c) {
                        RatchetChange::Unchanged => return None,
                        RatchetChange::Improved => "improved",
                        RatchetChange::Regressed => "regressed",
                        RatchetChange::Changed => "changed",
                    };
                    format!("[{scope}] baseline {b} -> current {c} ({change}); bless required")
                }
                (None, Some(&c)) => {
                    format!("[{scope}] new metric (current {c}); bless required")
                }
                (Some(&b), None) => {
                    format!("[{scope}] no longer measured (baseline {b}); bless required")
                }
                (None, None) => return None,
            };
            Some(GateFailure {
                kind: FailureKind::Ratchet,
                check: Some(name.clone()),
                ids: Vec::new(),
                message,
            })
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
#[path = "gate_tests.rs"]
mod tests;
