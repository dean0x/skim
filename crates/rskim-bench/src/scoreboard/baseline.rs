//! `baseline.json` and `bless` for the search scoreboard (#203).
//!
//! The baseline records what `check` compares a run against:
//! - every HARD outcome (`pass` / `xfail`) by query id and check;
//! - every RATCHET value, per corpus and aggregated;
//! - each corpus's commit and golden-file SHA-256, and the golden-set hash;
//! - `accepted_regressions[]`: every RATCHET regression a bless accepted,
//!   with its reason.
//!
//! It holds no latency, binary version, paths or timestamps, so it changes
//! only when a bless changes what is expected.
//!
//! [`bless`] is pure: it decides from a `report.json` (usually the CI
//! artifact, so blessing needs no local run), the golden files on disk, and
//! the existing baseline. It refuses a partial (`--only`) run, a report made
//! from other golden files, any unledgered HARD failure, any XPASS, and any
//! RATCHET regression that was not accepted with a reason.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::scoreboard::metrics::{RatchetChange, compare_ratchet};
use crate::scoreboard::report::{Outcome, Report};

/// `baseline.json` schema version.
pub const BASELINE_SCHEMA: u32 = 1;

/// A blessed HARD outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HardState {
    Pass,
    Xfail,
}

impl HardState {
    /// The report outcome this state blesses, if blessable.
    pub fn from_outcome(outcome: Outcome) -> Option<Self> {
        match outcome {
            Outcome::Pass => Some(HardState::Pass),
            Outcome::Xfail => Some(HardState::Xfail),
            Outcome::Fail | Outcome::Xpass => None,
        }
    }

    /// The lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            HardState::Pass => "pass",
            HardState::Xfail => "xfail",
        }
    }
}

/// One corpus in the baseline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineCorpus {
    pub commit: String,
    pub golden_sha256: String,
    /// HARD outcomes by query id, then dotted check name.
    pub hard: BTreeMap<String, BTreeMap<String, HardState>>,
    pub ratchet: BTreeMap<String, f64>,
}

/// One accepted batch of RATCHET regressions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedRegression {
    pub reason: String,
    /// `"<scope>/<metric>: <baseline> -> <current>"`, sorted.
    pub regressions: Vec<String>,
}

/// The whole `baseline.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub schema: u32,
    pub golden_sha256: String,
    pub corpora: BTreeMap<String, BaselineCorpus>,
    pub aggregate: BTreeMap<String, f64>,
    pub accepted_regressions: Vec<AcceptedRegression>,
}

impl Baseline {
    /// Parse `baseline.json`.
    ///
    /// # Errors
    ///
    /// Invalid JSON, unknown fields, or a schema other than
    /// [`BASELINE_SCHEMA`].
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        let baseline: Baseline = serde_json::from_str(raw).context("parsing baseline.json")?;
        anyhow::ensure!(
            baseline.schema == BASELINE_SCHEMA,
            "baseline.json schema {} is not the supported schema {BASELINE_SCHEMA}",
            baseline.schema
        );
        Ok(baseline)
    }

    /// Load `path`, or `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// Unreadable or unparsable file.
    pub fn load(path: &Path) -> anyhow::Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(raw) => Self::parse(&raw)
                .with_context(|| format!("in {}", path.display()))
                .map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(anyhow::anyhow!(e).context(format!("reading {}", path.display()))),
        }
    }

    /// Pretty JSON plus a trailing newline.
    ///
    /// # Errors
    ///
    /// Serialization failure.
    pub fn to_json(&self) -> anyhow::Result<String> {
        let mut s = serde_json::to_string_pretty(self).context("serializing baseline.json")?;
        s.push('\n');
        Ok(s)
    }

    /// Write to `path` atomically (temporary file in the same directory,
    /// then rename).
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        let json = self.to_json()?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))
    }

    /// The blessed state of `(id, check)` in `corpus`.
    pub fn hard_state(&self, corpus: &str, id: &str, check: &str) -> Option<HardState> {
        self.corpora.get(corpus)?.hard.get(id)?.get(check).copied()
    }
}

/// What [`bless`] decided.
#[derive(Debug, Clone, PartialEq)]
pub enum BlessDecision {
    /// The new baseline, plus notes for the operator.
    Blessed {
        baseline: Baseline,
        notes: Vec<String>,
    },
    /// Why nothing was blessed (one line each).
    Refused(Vec<String>),
}

/// Everything [`bless`] decides from.
#[derive(Debug, Clone, Copy)]
pub struct BlessInputs<'a> {
    pub report: &'a Report,
    /// SHA-256 of each corpus's golden file in the data dir, by corpus.
    pub golden_on_disk: &'a BTreeMap<String, String>,
    pub existing: Option<&'a Baseline>,
    /// `--accept-regression "<reason>"`.
    pub accept_regression: Option<&'a str>,
}

/// Decide whether `inputs.report` may become the baseline.
pub fn bless(inputs: &BlessInputs<'_>) -> BlessDecision {
    let report = inputs.report;
    let mut reasons = Vec::new();
    let mut notes = Vec::new();

    if !report.complete {
        reasons.push(
            "report.json comes from a partial run (--only); bless needs a run over every corpus"
                .to_string(),
        );
    }
    for c in &report.corpora {
        match inputs.golden_on_disk.get(&c.name) {
            None => reasons.push(format!(
                "corpus {}: no golden file for it in the data dir",
                c.name
            )),
            Some(sha) if *sha != c.golden_sha256 => reasons.push(format!(
                "corpus {}: report.json was produced from a different golden file than the data dir holds; rerun the scoreboard",
                c.name
            )),
            Some(_) => {}
        }
    }
    reasons.extend(unblessable_hard_outcomes(report));

    let regressions = inputs
        .existing
        .map(|b| regressions(b, report))
        .unwrap_or_default();
    let accepted = match (regressions.is_empty(), inputs.accept_regression) {
        (true, None) => None,
        (true, Some(_)) => {
            notes.push("--accept-regression ignored: nothing regressed".to_string());
            None
        }
        (false, Some(reason)) if !reason.trim().is_empty() => Some(AcceptedRegression {
            reason: reason.trim().to_string(),
            regressions: regressions.clone(),
        }),
        (false, Some(_)) => {
            reasons.push("--accept-regression needs a non-blank reason".to_string());
            None
        }
        (false, None) => {
            reasons.push(format!(
                "RATCHET regression(s): {}; rerun bless with --accept-regression \"<reason>\" to accept them",
                regressions.join("; ")
            ));
            None
        }
    };

    if !reasons.is_empty() {
        return BlessDecision::Refused(reasons);
    }

    let mut accepted_regressions = inputs
        .existing
        .map(|b| b.accepted_regressions.clone())
        .unwrap_or_default();
    if let Some(a) = accepted {
        notes.push(format!(
            "accepted {} regression(s): {}",
            a.regressions.len(),
            a.reason
        ));
        accepted_regressions.push(a);
    }
    BlessDecision::Blessed {
        baseline: baseline_from(report, accepted_regressions),
        notes,
    }
}

/// The baseline a report blesses (HARD outcomes must all be pass / xfail;
/// [`bless`] checks that first).
fn baseline_from(report: &Report, accepted_regressions: Vec<AcceptedRegression>) -> Baseline {
    let corpora = report
        .corpora
        .iter()
        .map(|c| {
            let mut hard: BTreeMap<String, BTreeMap<String, HardState>> = BTreeMap::new();
            for r in &c.checks {
                if let Some(state) = HardState::from_outcome(r.outcome) {
                    hard.entry(r.id.clone())
                        .or_default()
                        .insert(r.check.as_str().to_string(), state);
                }
            }
            (
                c.name.clone(),
                BaselineCorpus {
                    commit: c.commit.clone(),
                    golden_sha256: c.golden_sha256.clone(),
                    hard,
                    ratchet: c.ratchet.clone(),
                },
            )
        })
        .collect();
    Baseline {
        schema: BASELINE_SCHEMA,
        golden_sha256: report.golden_sha256.clone(),
        corpora,
        aggregate: report.aggregate.ratchet.clone(),
        accepted_regressions,
    }
}

/// One refusal line per check with unledgered failures or XPASSes.
fn unblessable_hard_outcomes(report: &Report) -> Vec<String> {
    let mut fails: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut xpasses: BTreeMap<&str, (BTreeSet<&str>, BTreeSet<&str>)> = BTreeMap::new();
    for r in report.records() {
        match r.outcome {
            Outcome::Fail => {
                fails.entry(r.check.as_str()).or_default().insert(&r.id);
            }
            Outcome::Xpass => {
                let e = xpasses.entry(r.check.as_str()).or_default();
                e.0.insert(&r.id);
                if let Some(issue) = &r.issue {
                    e.1.insert(issue);
                }
            }
            Outcome::Pass | Outcome::Xfail => {}
        }
    }
    let fails = fails.into_iter().map(|(check, ids)| {
        format!(
            "unledgered HARD failure {check} on {}: fix it, or file a ticket and ledger it in known_failures.toml",
            join(&ids)
        )
    });
    let xpasses = xpasses.into_iter().map(|(check, (ids, issues))| {
        format!(
            "XPASS {check} on {} (ledgered by {}): promote first — remove the ledger entry",
            join(&ids),
            join(&issues)
        )
    });
    fails.chain(xpasses).collect()
}

fn join(set: &BTreeSet<&str>) -> String {
    set.iter().copied().collect::<Vec<_>>().join(", ")
}

/// Every RATCHET value that moved in its bad direction beyond tolerance,
/// as `"<scope>/<metric>: <baseline> -> <current>"`. Aggregate regressions
/// are listed only for metrics with no per-corpus regression.
pub fn regressions(existing: &Baseline, report: &Report) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for c in &report.corpora {
        let Some(b) = existing.corpora.get(&c.name) else {
            continue;
        };
        for (name, &current) in &c.ratchet {
            if let Some(&base) = b.ratchet.get(name)
                && compare_ratchet(name, base, current) == RatchetChange::Regressed
            {
                seen.insert(name);
                out.push(format!("{}/{name}: {base} -> {current}", c.name));
            }
        }
    }
    for (name, &current) in &report.aggregate.ratchet {
        if seen.contains(name.as_str()) {
            continue;
        }
        if let Some(&base) = existing.aggregate.get(name)
            && compare_ratchet(name, base, current) == RatchetChange::Regressed
        {
            out.push(format!("aggregate/{name}: {base} -> {current}"));
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code — fail loudly
mod tests {
    use super::*;
    use crate::scoreboard::report::{
        AggregateReport, CheckRecord, CorpusInfo, CorpusReport, CoverageReport, GateReport,
        GateStatus, LatencyReport, REPORT_SCHEMA, SkippedByReason, UniverseReport,
    };
    use crate::scoreboard::types::CheckId;

    fn record(id: &str, check: CheckId, outcome: Outcome) -> CheckRecord {
        CheckRecord {
            id: id.to_string(),
            check,
            outcome,
            issue: matches!(outcome, Outcome::Xfail | Outcome::Xpass).then(|| "#544".to_string()),
            detail: None,
        }
    }

    fn corpus(name: &str, checks: Vec<CheckRecord>, mrr: f64) -> CorpusReport {
        CorpusReport {
            name: name.to_string(),
            commit: "c".repeat(40),
            golden_sha256: format!("g-{name}"),
            universe: UniverseReport {
                oracle: 1,
                skim_file_count: 1,
                delta: 0,
                skipped_by_reason: SkippedByReason {
                    oracle: BTreeMap::new(),
                    skim: BTreeMap::new(),
                },
            },
            coverage: CoverageReport {
                indexed_tracked: 1,
                tracked_text: 1,
                ratio: 1.0,
            },
            checks,
            ratchet: BTreeMap::from([("ident.mrr".to_string(), mrr)]),
            info: CorpusInfo {
                oracle_skipped_by_reason: BTreeMap::new(),
                unindexed_hits: BTreeMap::new(),
                idents: Vec::new(),
                concepts: Vec::new(),
            },
        }
    }

    fn report(corpora: Vec<CorpusReport>, mrr: f64) -> Report {
        Report {
            schema: REPORT_SCHEMA,
            golden_sha256: "set".to_string(),
            complete: true,
            corpora,
            aggregate: AggregateReport {
                hard: BTreeMap::new(),
                ratchet: BTreeMap::from([("ident.mrr".to_string(), mrr)]),
            },
            gate: GateReport {
                status: GateStatus::Pass,
                failures: Vec::new(),
            },
            latency: LatencyReport::default(),
        }
    }

    fn on_disk() -> BTreeMap<String, String> {
        BTreeMap::from([("skim".to_string(), "g-skim".to_string())])
    }

    fn decide(report: &Report, existing: Option<&Baseline>, accept: Option<&str>) -> BlessDecision {
        let disk = on_disk();
        bless(&BlessInputs {
            report,
            golden_on_disk: &disk,
            existing,
            accept_regression: accept,
        })
    }

    fn blessed(d: BlessDecision) -> Baseline {
        match d {
            BlessDecision::Blessed { baseline, .. } => baseline,
            BlessDecision::Refused(r) => panic!("refused: {r:?}"),
        }
    }

    fn refused(d: BlessDecision) -> String {
        match d {
            BlessDecision::Refused(r) => r.join("\n"),
            BlessDecision::Blessed { .. } => panic!("blessed"),
        }
    }

    #[test]
    fn a_clean_report_blesses_hard_states_ratchets_and_hashes() {
        let r = report(
            vec![corpus(
                "skim",
                vec![
                    record("skim-X01", CheckId::LexicalRecall, Outcome::Pass),
                    record("skim-G001", CheckId::PaginationComplete, Outcome::Xfail),
                ],
                0.9,
            )],
            0.9,
        );
        let b = blessed(decide(&r, None, None));
        assert_eq!(
            b.hard_state("skim", "skim-X01", "lexical.recall"),
            Some(HardState::Pass)
        );
        assert_eq!(
            b.hard_state("skim", "skim-G001", "pagination.complete"),
            Some(HardState::Xfail)
        );
        assert_eq!(b.corpora["skim"].ratchet["ident.mrr"], 0.9);
        assert_eq!(b.corpora["skim"].golden_sha256, "g-skim");
        assert_eq!(b.aggregate["ident.mrr"], 0.9);
        assert_eq!(b.golden_sha256, "set");
        assert_eq!(Baseline::parse(&b.to_json().unwrap()).unwrap(), b);
    }

    #[test]
    fn unledgered_failures_and_xpasses_are_refused_by_check_and_id() {
        let r = report(
            vec![corpus(
                "skim",
                vec![
                    record("skim-X02", CheckId::LexicalSilentFn, Outcome::Fail),
                    record("skim-X01", CheckId::LexicalSilentFn, Outcome::Fail),
                    record("skim-G001", CheckId::PaginationComplete, Outcome::Xpass),
                ],
                0.9,
            )],
            0.9,
        );
        let why = refused(decide(&r, None, None));
        assert!(
            why.contains("lexical.silent_fn on skim-X01, skim-X02"),
            "{why}"
        );
        assert!(
            why.contains("XPASS pagination.complete on skim-G001 (ledgered by #544)"),
            "{why}"
        );
    }

    #[test]
    fn partial_runs_and_foreign_golden_files_are_refused() {
        let mut r = report(vec![corpus("skim", Vec::new(), 0.9)], 0.9);
        r.complete = false;
        assert!(refused(decide(&r, None, None)).contains("partial run"));

        let mut r = report(vec![corpus("skim", Vec::new(), 0.9)], 0.9);
        r.corpora[0].golden_sha256 = "other".to_string();
        assert!(refused(decide(&r, None, None)).contains("different golden file"));
    }

    #[test]
    fn a_regression_needs_a_reason_and_the_reason_is_recorded() {
        let old = blessed(decide(
            &report(vec![corpus("skim", Vec::new(), 0.9)], 0.9),
            None,
            None,
        ));
        let worse = report(vec![corpus("skim", Vec::new(), 0.8)], 0.8);

        let why = refused(decide(&worse, Some(&old), None));
        assert!(why.contains("skim/ident.mrr: 0.9 -> 0.8"), "{why}");
        assert!(why.contains("--accept-regression"), "{why}");
        assert!(
            !why.contains("aggregate/ident.mrr"),
            "aggregate duplicates are suppressed: {why}"
        );
        assert!(refused(decide(&worse, Some(&old), Some("  "))).contains("non-blank"));

        let b = blessed(decide(&worse, Some(&old), Some("golden set grew")));
        assert_eq!(
            b.accepted_regressions,
            vec![AcceptedRegression {
                reason: "golden set grew".to_string(),
                regressions: vec!["skim/ident.mrr: 0.9 -> 0.8".to_string()],
            }]
        );
        assert_eq!(b.corpora["skim"].ratchet["ident.mrr"], 0.8);

        // Accepted regressions are kept by later blesses.
        let again = blessed(decide(&worse, Some(&b), None));
        assert_eq!(again.accepted_regressions.len(), 1);
    }

    #[test]
    fn improvements_bless_without_a_reason() {
        let old = blessed(decide(
            &report(vec![corpus("skim", Vec::new(), 0.5)], 0.5),
            None,
            None,
        ));
        let better = report(vec![corpus("skim", Vec::new(), 0.9)], 0.9);
        match decide(&better, Some(&old), Some("unused")) {
            BlessDecision::Blessed { baseline, notes } => {
                assert_eq!(baseline.corpora["skim"].ratchet["ident.mrr"], 0.9);
                assert!(baseline.accepted_regressions.is_empty());
                assert!(notes.iter().any(|n| n.contains("ignored")), "{notes:?}");
            }
            BlessDecision::Refused(r) => panic!("{r:?}"),
        }
    }

    #[test]
    fn load_returns_none_for_a_missing_file_and_rejects_other_schemas() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("baseline.json");
        assert_eq!(Baseline::load(&path).unwrap(), None);

        let b = blessed(decide(
            &report(vec![corpus("skim", Vec::new(), 0.5)], 0.5),
            None,
            None,
        ));
        b.write(&path).unwrap();
        assert_eq!(Baseline::load(&path).unwrap(), Some(b.clone()));

        let raw = b
            .to_json()
            .unwrap()
            .replacen("\"schema\": 1", "\"schema\": 2", 1);
        assert!(Baseline::parse(&raw).is_err());
    }
}
