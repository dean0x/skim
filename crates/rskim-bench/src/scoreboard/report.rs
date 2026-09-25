//! `report.json` and `report.md` for the search scoreboard (#203).
//!
//! `report.json` is byte-deterministic across runs of the same binary on the
//! same corpora, apart from its last top-level section, `latency`:
//! - every map is a [`BTreeMap`] or a struct (the workspace `serde_json` has
//!   `preserve_order`, so insertion order would otherwise leak into output);
//! - every list is sorted before it is stored;
//! - floats are rounded to 4 decimal places ([`round4`]);
//! - paths are repo-relative, and nothing records a timestamp, a binary
//!   version, or a machine path.
//!
//! `report.md` is the step summary (`$GITHUB_STEP_SUMMARY`): the vision's
//! scoreboard table (metric | bar | current | baseline | status), the gate
//! failures, and the HARD tallies.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::scoreboard::baseline::Baseline;
use crate::scoreboard::metrics::{self, Beat, ConceptSample, IdentSample, RatchetChange};
use crate::scoreboard::types::CheckId;

/// `report.json` schema version; `bless` refuses any other.
pub const REPORT_SCHEMA: u32 = 1;

/// Round to 4 decimal places (report and baseline floats).
pub fn round4(x: f64) -> f64 {
    (x * 10_000.0).round() / 10_000.0
}

// ============================================================================
// report.json
// ============================================================================

/// The whole `report.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema: u32,
    /// [`crate::scoreboard::golden::golden_set_sha256`] over the golden files
    /// of the corpora this run covered.
    pub golden_sha256: String,
    /// `false` when `--only` restricted the run to a subset of the corpora;
    /// `bless` refuses such a report.
    pub complete: bool,
    /// One entry per corpus, in `corpora.toml` order.
    pub corpora: Vec<CorpusReport>,
    pub aggregate: AggregateReport,
    pub gate: GateReport,
    /// INFO only — the one section allowed to differ between runs. Always
    /// the last key.
    pub latency: LatencyReport,
}

/// One corpus's results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusReport {
    pub name: String,
    pub commit: String,
    /// SHA-256 of this corpus's golden file bytes.
    pub golden_sha256: String,
    pub universe: UniverseReport,
    pub coverage: CoverageReport,
    /// Every HARD check that ran, sorted by `(id, check)`.
    pub checks: Vec<CheckRecord>,
    /// RATCHET values by metric name.
    pub ratchet: BTreeMap<String, f64>,
    /// INFO: never gated.
    pub info: CorpusInfo,
}

/// Oracle universe vs skim's index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UniverseReport {
    /// Files in the oracle's indexed universe.
    pub oracle: u64,
    /// `--stats --json` `file_count`.
    pub skim_file_count: u64,
    /// `oracle − skim_file_count`.
    pub delta: i64,
    pub skipped_by_reason: SkippedByReason,
}

/// Producer-phase (persisted) skips, oracle vs skim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkippedByReason {
    pub oracle: BTreeMap<String, u64>,
    pub skim: BTreeMap<String, u64>,
}

/// Coverage of the tracked text files by the indexed universe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageReport {
    pub indexed_tracked: u64,
    pub tracked_text: u64,
    pub ratio: f64,
}

/// A HARD check's outcome after the ledger is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Passed and not ledgered.
    Pass,
    /// Failed and not ledgered: a gate failure.
    Fail,
    /// Failed and ledgered: expected, not a gate failure.
    Xfail,
    /// Passed although ledgered: a gate failure ("promote").
    Xpass,
}

impl Outcome {
    /// The lowercase name used in reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Pass => "pass",
            Outcome::Fail => "fail",
            Outcome::Xfail => "xfail",
            Outcome::Xpass => "xpass",
        }
    }
}

/// One `(id, check)` outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckRecord {
    pub id: String,
    pub check: CheckId,
    pub outcome: Outcome,
    /// The ledger issue (`xfail` / `xpass` only).
    pub issue: Option<String>,
    /// What failed (`fail` / `xfail` only).
    pub detail: Option<String>,
}

/// INFO about one corpus (deterministic, never gated).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusInfo {
    /// The oracle's per-reason skip breakdown, walk-phase reasons included.
    pub oracle_skipped_by_reason: BTreeMap<String, u64>,
    /// Ground-truth hits among tracked text files outside the indexed
    /// universe, by query id (non-zero counts only).
    pub unindexed_hits: BTreeMap<String, u64>,
    /// Per-query ranking measurements behind the `ident.*` / `bytes.*`
    /// ratchets, in golden order.
    pub idents: Vec<IdentSample>,
    /// Per-query measurements behind the `concept.*` / `bytes.*` ratchets.
    pub concepts: Vec<ConceptSample>,
}

/// Cross-corpus aggregates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregateReport {
    /// Outcome tallies per HARD check (dotted name).
    pub hard: BTreeMap<String, HardTally>,
    /// RATCHET values pooled over every corpus in the run.
    pub ratchet: BTreeMap<String, f64>,
}

/// Outcome counts for one HARD check.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HardTally {
    pub pass: u64,
    pub fail: u64,
    pub xfail: u64,
    pub xpass: u64,
}

impl HardTally {
    fn add(&mut self, outcome: Outcome) {
        match outcome {
            Outcome::Pass => self.pass += 1,
            Outcome::Fail => self.fail += 1,
            Outcome::Xfail => self.xfail += 1,
            Outcome::Xpass => self.xpass += 1,
        }
    }
}

/// Tally every record, all checks together.
pub fn total<'a>(records: impl IntoIterator<Item = &'a CheckRecord>) -> HardTally {
    records.into_iter().fold(HardTally::default(), |mut t, r| {
        t.add(r.outcome);
        t
    })
}

/// Tally every record by check.
pub fn tally<'a>(
    records: impl IntoIterator<Item = &'a CheckRecord>,
) -> BTreeMap<String, HardTally> {
    let mut out: BTreeMap<String, HardTally> = BTreeMap::new();
    for r in records {
        out.entry(r.check.as_str().to_string())
            .or_default()
            .add(r.outcome);
    }
    out
}

/// The gate verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GateStatus {
    Pass,
    Fail,
}

/// Why the gate failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// A HARD check failed and no ledger entry covers it.
    Unledgered,
    /// A ledgered HARD check passed: remove its ledger entry.
    Xpass,
    /// A RATCHET value moved beyond tolerance (either direction).
    Ratchet,
    /// The run differs from `baseline.json` in something other than a
    /// ratchet value (no baseline, corpus set, commit, golden set, HARD
    /// outcome states).
    Baseline,
}

/// One gate failure. `message` is a single line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateFailure {
    pub kind: FailureKind,
    /// The HARD check or RATCHET metric concerned, if any.
    pub check: Option<String>,
    /// The golden query ids concerned (sorted), if any.
    pub ids: Vec<String>,
    pub message: String,
}

impl GateFailure {
    /// `FAIL <check>: <ids> — <message>`, the line printed on stderr.
    pub fn line(&self) -> String {
        let mut line = String::from("FAIL");
        if let Some(check) = &self.check {
            let _ = write!(line, " {check}");
        }
        if !self.ids.is_empty() {
            let _ = write!(line, " [{}]", self.ids.join(", "));
        }
        let _ = write!(line, ": {}", self.message);
        line
    }
}

/// The gate section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateReport {
    pub status: GateStatus,
    pub failures: Vec<GateFailure>,
}

/// Per-corpus latency (INFO; the only non-deterministic section).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatencyReport {
    pub corpora: BTreeMap<String, LatencyStats>,
}

/// Latency percentiles over one corpus's skim query calls.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatencyStats {
    pub calls: u64,
    pub wall_ms_p50: f64,
    pub wall_ms_p95: f64,
    /// From the JSON `duration_ms` field, when the envelope carries it.
    pub duration_ms_p50: Option<f64>,
    pub duration_ms_p95: Option<f64>,
    /// Total wall-clock milliseconds of every call behind each golden entry.
    pub entries_wall_ms: BTreeMap<String, f64>,
}

impl Report {
    /// Every check record across corpora.
    pub fn records(&self) -> impl Iterator<Item = &CheckRecord> {
        self.corpora.iter().flat_map(|c| c.checks.iter())
    }

    /// `report.json` bytes: pretty JSON plus a trailing newline.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub fn to_json(&self) -> anyhow::Result<String> {
        let mut s = serde_json::to_string_pretty(self).context("serializing report.json")?;
        s.push('\n');
        Ok(s)
    }

    /// Parse `report.json`.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid JSON, unknown fields, or a schema other
    /// than [`REPORT_SCHEMA`].
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        let report: Report = serde_json::from_str(raw).context("parsing report.json")?;
        anyhow::ensure!(
            report.schema == REPORT_SCHEMA,
            "report.json schema {} is not the supported schema {REPORT_SCHEMA}",
            report.schema
        );
        Ok(report)
    }
}

/// Write `report.json` and `report.md` into `out_dir` (created if missing).
///
/// # Errors
///
/// Returns an error if the directory or either file cannot be written.
pub fn write_outputs(
    report: &Report,
    baseline: Option<&Baseline>,
    out_dir: &Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("creating output dir {}", out_dir.display()))?;
    let json_path = out_dir.join("report.json");
    std::fs::write(&json_path, report.to_json()?)
        .with_context(|| format!("writing {}", json_path.display()))?;
    let md_path = out_dir.join("report.md");
    std::fs::write(&md_path, render_markdown(report, baseline))
        .with_context(|| format!("writing {}", md_path.display()))?;
    Ok(())
}

// ============================================================================
// report.md
// ============================================================================

/// Render the step summary.
pub fn render_markdown(report: &Report, baseline: Option<&Baseline>) -> String {
    let mut md = String::new();
    let status = match report.gate.status {
        GateStatus::Pass => "PASS",
        GateStatus::Fail => "FAIL",
    };
    let _ = writeln!(md, "# Search scoreboard — {status}\n");
    let names: Vec<&str> = report.corpora.iter().map(|c| c.name.as_str()).collect();
    let hard = total(report.records());
    let _ = writeln!(
        md,
        "Corpora: {} · golden `{}` · HARD: {} pass, {} xfail, {} fail, {} xpass{}\n",
        names.join(", "),
        short_sha(&report.golden_sha256),
        hard.pass,
        hard.xfail,
        hard.fail,
        hard.xpass,
        if report.complete {
            ""
        } else {
            " · partial run (`--only`)"
        }
    );

    if !report.gate.failures.is_empty() {
        let _ = writeln!(md, "## Gate failures\n");
        for f in &report.gate.failures {
            let _ = writeln!(md, "- {}", md_escape(&f.line()));
        }
        md.push('\n');
    }

    let _ = writeln!(md, "## HARD checks (tolerance 0)\n");
    let _ = writeln!(md, "| check | pass | xfail | fail | xpass |");
    let _ = writeln!(md, "|---|---|---|---|---|");
    for (check, t) in &report.aggregate.hard {
        let _ = writeln!(
            md,
            "| `{check}` | {} | {} | {} | {} |",
            t.pass, t.xfail, t.fail, t.xpass
        );
    }
    md.push('\n');

    let _ = writeln!(md, "## Aggregate\n");
    ratchet_table(
        &mut md,
        &report.aggregate.ratchet,
        baseline.map(|b| &b.aggregate),
    );
    for c in &report.corpora {
        let _ = writeln!(md, "## {} @ {}\n", c.name, short_sha(&c.commit));
        let _ = writeln!(
            md,
            "Universe: oracle {} · skim {} · delta {} · coverage {:.4}\n",
            c.universe.oracle, c.universe.skim_file_count, c.universe.delta, c.coverage.ratio
        );
        ratchet_table(
            &mut md,
            &c.ratchet,
            baseline
                .and_then(|b| b.corpora.get(&c.name))
                .map(|b| &b.ratchet),
        );
    }

    let _ = writeln!(md, "## Latency (INFO, never gated)\n");
    let _ = writeln!(md, "| corpus | calls | wall p50 ms | wall p95 ms |");
    let _ = writeln!(md, "|---|---|---|---|");
    for (name, l) in &report.latency.corpora {
        let _ = writeln!(
            md,
            "| {name} | {} | {:.1} | {:.1} |",
            l.calls, l.wall_ms_p50, l.wall_ms_p95
        );
    }
    md
}

fn ratchet_table(
    md: &mut String,
    current: &BTreeMap<String, f64>,
    baseline: Option<&BTreeMap<String, f64>>,
) {
    let _ = writeln!(
        md,
        "| metric | bar | current | baseline | status | beats baseline |"
    );
    let _ = writeln!(md, "|---|---|---|---|---|---|");
    for (name, value) in current {
        let base = baseline.and_then(|b| b.get(name)).copied();
        let status = match (baseline, base) {
            (None, _) => "no baseline",
            (Some(_), None) => "new — bless required",
            (Some(_), Some(b)) => match metrics::compare_ratchet(name, b, *value) {
                RatchetChange::Unchanged => "=",
                RatchetChange::Improved => "improved — bless required",
                RatchetChange::Regressed => "regressed — bless required",
                RatchetChange::Changed => "changed — bless required",
            },
        };
        let _ = writeln!(
            md,
            "| `{name}` | {} | {} | {} | {status} | {} |",
            metrics::metric_def(name).map_or("—", |d| d.bar),
            fmt_value(*value),
            base.map_or_else(|| "—".to_string(), fmt_value),
            metrics::beats_baseline(name, current).map_or("—", Beat::as_str)
        );
    }
    md.push('\n');
}

fn fmt_value(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.4}")
    }
}

fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// Keep a gate line from breaking the markdown list or table layout.
fn md_escape(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
mod tests {
    use super::*;

    #[test]
    fn round4_keeps_four_decimal_places() {
        assert_eq!(round4(1.0 / 3.0), 0.3333);
        assert_eq!(round4(2.0 / 3.0), 0.6667);
        assert_eq!(round4(0.5), 0.5);
    }

    #[test]
    fn gate_failure_line_names_check_and_ids() {
        let f = GateFailure {
            kind: FailureKind::Unledgered,
            check: Some("lexical.silent_fn".to_string()),
            ids: vec!["skim-X01".to_string(), "skim-X02".to_string()],
            message: "unledgered HARD failure".to_string(),
        };
        assert_eq!(
            f.line(),
            "FAIL lexical.silent_fn [skim-X01, skim-X02]: unledgered HARD failure"
        );
    }

    #[test]
    fn outcomes_serialize_lowercase() {
        assert_eq!(serde_json::to_string(&Outcome::Xpass).unwrap(), "\"xpass\"");
        assert_eq!(
            serde_json::to_string(&FailureKind::Unledgered).unwrap(),
            "\"unledgered\""
        );
    }

    fn minimal_report() -> Report {
        let corpus = CorpusReport {
            name: "skim".to_string(),
            commit: "b8a0a79463382347820f1c2572bde37b68e87c76".to_string(),
            golden_sha256: "g".to_string(),
            universe: UniverseReport {
                oracle: 3,
                skim_file_count: 3,
                delta: 0,
                skipped_by_reason: SkippedByReason {
                    oracle: BTreeMap::new(),
                    skim: BTreeMap::new(),
                },
            },
            coverage: CoverageReport {
                indexed_tracked: 3,
                tracked_text: 3,
                ratio: 1.0,
            },
            checks: Vec::new(),
            ratchet: BTreeMap::from([
                ("ident.mrr".to_string(), 0.9),
                ("ident.mrr.baseline_alpha".to_string(), 0.5),
            ]),
            info: CorpusInfo {
                oracle_skipped_by_reason: BTreeMap::new(),
                unindexed_hits: BTreeMap::new(),
                idents: Vec::new(),
                concepts: Vec::new(),
            },
        };
        Report {
            schema: REPORT_SCHEMA,
            golden_sha256: "set".to_string(),
            complete: true,
            aggregate: AggregateReport {
                hard: BTreeMap::new(),
                ratchet: corpus.ratchet.clone(),
            },
            corpora: vec![corpus],
            gate: GateReport {
                status: GateStatus::Pass,
                failures: Vec::new(),
            },
            latency: LatencyReport::default(),
        }
    }

    #[test]
    fn markdown_puts_each_corpus_heading_before_its_universe_line() {
        let md = render_markdown(&minimal_report(), None);
        let heading = md.find("## skim @ b8a0a7946338").unwrap();
        let universe = md.find("Universe: oracle 3").unwrap();
        assert!(heading < universe, "{md}");
    }

    #[test]
    fn markdown_shows_whether_each_metric_beats_its_baseline() {
        let md = render_markdown(&minimal_report(), None);
        assert!(
            md.contains("| metric | bar | current | baseline | status | beats baseline |"),
            "{md}"
        );
        let row = md
            .lines()
            .find(|l| l.starts_with("| `ident.mrr` "))
            .unwrap();
        assert!(row.ends_with("| yes |"), "{row}");
    }

    #[test]
    fn a_report_round_trips_through_json() {
        let r = minimal_report();
        assert_eq!(Report::parse(&r.to_json().unwrap()).unwrap(), r);
        let other = r
            .to_json()
            .unwrap()
            .replacen("\"schema\": 1", "\"schema\": 9", 1);
        assert!(Report::parse(&other).is_err());
    }

    #[test]
    fn tally_counts_outcomes_per_check() {
        let rec = |outcome| CheckRecord {
            id: "a-1".to_string(),
            check: CheckId::LexicalRecall,
            outcome,
            issue: None,
            detail: None,
        };
        let t = tally(&[rec(Outcome::Pass), rec(Outcome::Xfail), rec(Outcome::Pass)]);
        assert_eq!(
            t["lexical.recall"],
            HardTally {
                pass: 2,
                fail: 0,
                xfail: 1,
                xpass: 0
            }
        );
    }
}
