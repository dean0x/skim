//! `report.json` and `report.md` for the search scoreboard (#203).
//!
//! `report.json` is byte-deterministic across runs of the same binary on the
//! same corpora, apart from its last top-level section, `latency`:
//! - every map is a [`BTreeMap`] or a struct (the workspace `serde_json` has
//!   `preserve_order`, so insertion order would otherwise leak into output);
//! - every list is sorted before it is stored;
//! - floats are rounded to 4 decimal places
//!   ([`crate::scoreboard::fmt::round4`]);
//! - paths are repo-relative, and nothing records a timestamp, a binary
//!   version, or a machine path.
//!
//! `report.md` is the step summary (`$GITHUB_STEP_SUMMARY`): the vision's
//! scoreboard table (metric | bar | current | baseline | status), the gate
//! failures, the HARD tallies, each corpus's structural (`--ast`) table, and
//! the catalog patterns no structural entry scores.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::scoreboard::baseline::Baseline;
use crate::scoreboard::metrics::{self, Beat, ConceptSample, IdentSample, RatchetChange};
use crate::scoreboard::structural_metrics::{
    CoverageComparison, StructuralSample, UncoveredCause, UncoveredPattern,
};
use crate::scoreboard::types::CheckId;

/// `report.json` schema version; `bless` refuses any other.
pub const REPORT_SCHEMA: u32 = 1;

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
    /// Catalog patterns no `[[ast]]` entry of this run scores, sorted by
    /// name (they stay under ADR-007 manual dog-food).
    #[serde(default)]
    pub uncovered_patterns: Vec<UncoveredPattern>,
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
    /// The `[[ast]]` entries' measurements (their HARD outcomes are in
    /// `checks`, their RATCHET values in `ratchet`).
    #[serde(default)]
    pub structural: StructuralReport,
    /// INFO: never gated.
    pub info: CorpusInfo,
}

/// One corpus's structural (`--ast`) section (#541).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuralReport {
    /// skim's `ast_coverage` next to the oracle's over-cap count; absent
    /// when the corpus has no `[[ast]]` entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<CoverageComparison>,
    /// Per `[[ast]]` entry (corpus, pattern, lang), in golden order.
    pub entries: Vec<StructuralSample>,
    /// skim `--ast` rows in a language no entry scores, per pattern called.
    pub unscored_rows: BTreeMap<String, u64>,
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

/// The report file names `run` / `check` write into their output dir.
pub const REPORT_JSON: &str = "report.json";
pub const REPORT_MD: &str = "report.md";

/// Remove the [`REPORT_JSON`] / [`REPORT_MD`] a previous run left in
/// `out_dir`, before a run does any work: a run that stops on a harness error
/// then leaves no report behind for `bless --from` to take as its own. A
/// missing directory or file is fine.
///
/// # Errors
///
/// Returns an error if an existing report file cannot be removed.
pub fn clear_outputs(out_dir: &Path) -> anyhow::Result<()> {
    for name in [REPORT_JSON, REPORT_MD] {
        let path = out_dir.join(name);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("removing the previous {}", path.display()));
            }
        }
    }
    Ok(())
}

/// Write [`REPORT_JSON`] and [`REPORT_MD`] into `out_dir` (created if
/// missing).
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
    let json_path = out_dir.join(REPORT_JSON);
    std::fs::write(&json_path, report.to_json()?)
        .with_context(|| format!("writing {}", json_path.display()))?;
    let md_path = out_dir.join(REPORT_MD);
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
        structural_table(&mut md, &c.structural);
    }
    uncovered_section(&mut md, &report.uncovered_patterns);

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

/// One corpus's structural table: per `[[ast]]` entry, the oracle and skim
/// file counts, file-level recall / precision (and intent, for the nested
/// loops), and the rows anchored on an oracle match line. A false-positive
/// guard's class cell says so, and a legend under the table explains it.
fn structural_table(md: &mut String, s: &StructuralReport) {
    if s.entries.is_empty() {
        return;
    }
    let _ = writeln!(md, "### Structural (`--ast`)\n");
    if let Some(c) = &s.coverage {
        let list = |v: &[u64]| v.iter().map(u64::to_string).collect::<Vec<_>>().join(" / ");
        let _ = writeln!(
            md,
            "AST size cap: skim `size_excluded_files` {} · oracle over-cap {} · skim undetermined {}\n",
            list(&c.skim_size_excluded_files),
            c.oracle_over_cap,
            list(&c.skim_undetermined_files)
        );
    }
    let _ = writeln!(
        md,
        "| entry | pattern | lang | class | oracle files | skim files | recall | precision | intent recall | intent precision | line on match |"
    );
    let _ = writeln!(md, "|---|---|---|---|---|---|---|---|---|---|---|");
    for e in &s.entries {
        entry_row(md, e);
    }
    if s.entries.iter().any(|e| e.expect_oracle_empty) {
        let _ = writeln!(
            md,
            "\nFP guard: a false-positive guard (`expect_oracle_empty = true`). Its oracle matches no file by \
             declaration, so it guards precision only; oracle 0 / skim 0 is its fixed state."
        );
    }
    let unscored: Vec<String> = s
        .unscored_rows
        .iter()
        .filter(|(_, n)| **n > 0)
        .map(|(pattern, n)| format!("{pattern} {n}"))
        .collect();
    if !unscored.is_empty() {
        let _ = writeln!(
            md,
            "\nUnscored `--ast` rows (a language no entry scores): {}",
            unscored.join(", ")
        );
    }
    md.push('\n');
}

/// One `[[ast]]` entry's row of [`structural_table`]; a false-positive
/// guard's class cell carries [`FP_GUARD_MARKER`], and a measurement the
/// entry has none of reads `—`.
fn entry_row(md: &mut String, e: &StructuralSample) {
    let guard = if e.expect_oracle_empty {
        FP_GUARD_MARKER
    } else {
        ""
    };
    let opt = |v: Option<f64>| v.map_or_else(|| "—".to_string(), fmt_value);
    let _ = writeln!(
        md,
        "| `{}` | {} | {} | {}{guard} | {} | {} | {} | {} | {} | {} | {} |",
        e.id,
        e.pattern,
        e.lang,
        e.precision_class.as_str(),
        e.oracle_files,
        e.skim_files,
        fmt_value(e.recall),
        fmt_value(e.precision),
        opt(e.intent_recall),
        opt(e.intent_precision),
        e.line_on_match
    );
}

/// The catalog patterns no structural entry scores, each with its cause and
/// reason (nothing when every pattern is scored).
fn uncovered_section(md: &mut String, uncovered: &[UncoveredPattern]) {
    if uncovered.is_empty() {
        return;
    }
    let _ = writeln!(
        md,
        "## Uncovered structural patterns (manual dog-food, ADR-007)\n"
    );
    for p in uncovered {
        let cause = match p.cause {
            UncoveredCause::NoOracle => "no oracle",
            UncoveredCause::NoEntry => "no entry",
        };
        let _ = writeln!(md, "- `{}` ({cause}): {}", p.name, md_escape(&p.reason));
    }
    md.push('\n');
}

/// Appended to a false-positive guard's class cell in the structural table.
const FP_GUARD_MARKER: &str = ", FP guard";

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
            structural: StructuralReport::default(),
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
            uncovered_patterns: Vec::new(),
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

    fn structural_report() -> Report {
        use crate::scoreboard::golden::PrecisionClass;
        use rskim_oracle::structural::OracleLang;
        let mut r = minimal_report();
        let entry =
            |id: &str, pattern: &str, lang, intent: Option<(u64, f64, f64)>| StructuralSample {
                id: id.to_string(),
                pattern: pattern.to_string(),
                lang,
                precision_class: PrecisionClass::Hard,
                expect_oracle_empty: false,
                oracle_files: 3,
                skim_files: 4,
                recall: 1.0,
                precision: 0.75,
                intent_files: intent.map(|i| i.0),
                intent_recall: intent.map(|i| i.1),
                intent_precision: intent.map(|i| i.2),
                line_on_match: 2,
            };
        r.corpora[0].structural = StructuralReport {
            coverage: Some(CoverageComparison {
                oracle_over_cap: 2,
                skim_size_excluded_files: vec![2],
                skim_undetermined_files: vec![0],
            }),
            entries: vec![
                entry(
                    "skim-ast-rust-nested-loop-rust",
                    "rust-nested-loop",
                    OracleLang::Rust,
                    Some((110, 0.9, 0.3)),
                ),
                entry(
                    "skim-ast-try-catch-typescript",
                    "try-catch",
                    OracleLang::TypeScript,
                    None,
                ),
                StructuralSample {
                    expect_oracle_empty: true,
                    oracle_files: 0,
                    skim_files: 1,
                    precision: 0.0,
                    line_on_match: 0,
                    ..entry(
                        "skim-ast-try-catch-finally-javascript",
                        "try-catch-finally",
                        OracleLang::JavaScript,
                        None,
                    )
                },
            ],
            unscored_rows: BTreeMap::from([
                ("try-catch".to_string(), 0),
                ("rust-nested-loop".to_string(), 7),
            ]),
        };
        r.uncovered_patterns = vec![UncoveredPattern {
            name: "deep-nesting".to_string(),
            cause: UncoveredCause::NoOracle,
            reason: "threshold | not stated".to_string(),
        }];
        r
    }

    #[test]
    fn the_structural_section_carries_the_ac4_fields_and_omits_absent_intent() {
        let r = structural_report();
        let json: serde_json::Value = serde_json::from_str(&r.to_json().unwrap()).unwrap();
        let entries = &json["corpora"][0]["structural"]["entries"];
        let nested = entries[0].as_object().unwrap();
        for key in [
            "oracle_files",
            "skim_files",
            "recall",
            "precision",
            "intent_recall",
            "intent_precision",
            "line_on_match",
        ] {
            assert!(nested.contains_key(key), "{key}");
        }
        let plain = entries[1].as_object().unwrap();
        for key in ["intent_files", "intent_recall", "intent_precision"] {
            assert!(!plain.contains_key(key), "{key} is omitted when None");
        }
        // A false-positive guard says so; an ordinary entry has no such key.
        assert_eq!(entries[2]["expect_oracle_empty"], true);
        for ordinary in [nested, plain] {
            assert!(!ordinary.contains_key("expect_oracle_empty"));
        }
        assert_eq!(
            json["corpora"][0]["structural"]["coverage"]["oracle_over_cap"],
            2
        );
        assert_eq!(
            json["uncovered_patterns"][0],
            serde_json::json!({"name": "deep-nesting", "cause": "no_oracle", "reason": "threshold | not stated"})
        );
        // Unscored rows serialize in key order, whatever the insertion order.
        let unscored: Vec<&String> = json["corpora"][0]["structural"]["unscored_rows"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        assert_eq!(unscored, ["rust-nested-loop", "try-catch"]);
        // latency stays the last key.
        let top: Vec<&String> = json.as_object().unwrap().keys().collect();
        assert_eq!(top.last().map(|k| k.as_str()), Some("latency"));
    }

    #[test]
    fn a_structural_report_round_trips_byte_for_byte() {
        let r = structural_report();
        let first = r.to_json().unwrap();
        assert_eq!(Report::parse(&first).unwrap(), r);
        assert_eq!(Report::parse(&first).unwrap().to_json().unwrap(), first);
        assert_eq!(structural_report().to_json().unwrap(), first);
        // A corpus with no [[ast]] entry has no coverage key.
        let plain = minimal_report().to_json().unwrap();
        assert!(!plain.contains("oracle_over_cap"), "{plain}");
    }

    #[test]
    fn a_report_from_before_the_structural_section_still_parses() {
        let mut json: serde_json::Value =
            serde_json::from_str(&minimal_report().to_json().unwrap()).unwrap();
        json.as_object_mut().unwrap().remove("uncovered_patterns");
        json["corpora"][0]
            .as_object_mut()
            .unwrap()
            .remove("structural");
        let parsed = Report::parse(&json.to_string()).unwrap();
        assert_eq!(parsed, minimal_report());
    }

    #[test]
    fn markdown_shows_each_corpus_structural_table_and_the_uncovered_patterns() {
        let md = render_markdown(&structural_report(), None);
        assert!(md.contains("### Structural (`--ast`)"), "{md}");
        assert!(
            md.contains("AST size cap: skim `size_excluded_files` 2 · oracle over-cap 2"),
            "{md}"
        );
        let row = md
            .lines()
            .find(|l| l.starts_with("| `skim-ast-rust-nested-loop-rust` "))
            .unwrap();
        assert_eq!(
            row,
            "| `skim-ast-rust-nested-loop-rust` | rust-nested-loop | rust | hard | 3 | 4 | 1 | 0.7500 | 0.9000 | 0.3000 | 2 |"
        );
        let plain = md
            .lines()
            .find(|l| l.starts_with("| `skim-ast-try-catch-typescript` "))
            .unwrap();
        assert!(plain.ends_with("| — | — | 2 |"), "{plain}");
        // A false-positive guard is marked in its class cell and explained
        // once under the table.
        let guard = md
            .lines()
            .find(|l| l.starts_with("| `skim-ast-try-catch-finally-javascript` "))
            .unwrap();
        assert_eq!(
            guard,
            "| `skim-ast-try-catch-finally-javascript` | try-catch-finally | javascript | hard, FP guard | 0 | 1 | 1 | 0 | — | — | 0 |"
        );
        assert_eq!(md.matches("\nFP guard: ").count(), 1, "{md}");
        assert!(
            md.contains("Unscored `--ast` rows (a language no entry scores): rust-nested-loop 7")
        );
        assert!(md.contains("## Uncovered structural patterns"), "{md}");
        assert!(
            md.contains("- `deep-nesting` (no oracle): threshold \\| not stated"),
            "{md}"
        );
        // No structural section without [[ast]] entries.
        let plain_md = render_markdown(&minimal_report(), None);
        assert!(!plain_md.contains("Structural"), "{plain_md}");
    }

    #[test]
    fn markdown_explains_fp_guards_only_when_an_entry_is_one() {
        let mut r = structural_report();
        r.corpora[0]
            .structural
            .entries
            .retain(|e| !e.expect_oracle_empty);
        let md = render_markdown(&r, None);
        assert!(md.contains("### Structural (`--ast`)"), "{md}");
        assert!(!md.contains("FP guard"), "{md}");
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

    #[test]
    fn clearing_outputs_removes_only_the_report_files() {
        let dir = tempfile::tempdir().unwrap();
        for name in [REPORT_JSON, REPORT_MD, "keep.txt"] {
            std::fs::write(dir.path().join(name), "stale").unwrap();
        }

        clear_outputs(dir.path()).unwrap();

        assert!(!dir.path().join(REPORT_JSON).exists());
        assert!(!dir.path().join(REPORT_MD).exists());
        assert!(dir.path().join("keep.txt").exists());
        // Nothing left to clear, or no directory at all, is fine.
        clear_outputs(dir.path()).unwrap();
        clear_outputs(&dir.path().join("absent")).unwrap();
    }

    #[test]
    fn a_report_path_that_cannot_be_removed_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(REPORT_JSON)).unwrap();

        let err = clear_outputs(dir.path()).expect_err("a directory is not a report file");

        assert!(format!("{err:#}").contains(REPORT_JSON), "{err:#}");
    }
}
