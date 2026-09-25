//! One scoreboard run, end to end (#203): load the data dir, then for each
//! corpus materialize and verify the pinned clone, compute the oracle
//! universe, check golden integrity, drive skim, verify the clone again,
//! score, and apply the ledger; finally aggregate, gate, and assemble the
//! report.
//!
//! Every `Err` here is a harness error (exit 2): a missing or invalid data
//! file, clone verification, golden integrity (including a ledger entry that
//! can never apply), a skim crash / timeout / unparsable output, or a corpus
//! that changed under the run. Gate failures are not errors — they are
//! recorded in the report's `gate` section.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context;

use crate::scoreboard::baseline::Baseline;
use crate::scoreboard::corpus::{
    CorpusSource, CorpusSpec, find_corpus, load_corpora, materialize_verified,
};
use crate::scoreboard::gate::{self, GateInputs, Ledger};
use crate::scoreboard::golden::{
    IntegrityContext, LoadedGolden, check_integrity, golden_set_sha256, load_golden,
};
use crate::scoreboard::metrics::{
    self, CorpusEvaluation, CorpusSamples, PlannedQuery, percentile_f64,
};
use crate::scoreboard::report::{
    AggregateReport, CorpusInfo, CorpusReport, CoverageReport, LatencyReport, LatencyStats,
    REPORT_SCHEMA, Report, SkippedByReason, UniverseReport, round4, tally,
};
use crate::scoreboard::runner::{SkimRunner, Timing};
use crate::scoreboard::types::StatsSnapshot;
use crate::scoreboard::universe::Universe;

// ============================================================================
// Data dir
// ============================================================================

/// The scoreboard's data dir (`crates/rskim-bench/scoreboard` by default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDir {
    root: PathBuf,
}

impl DataDir {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        DataDir { root: root.into() }
    }

    /// `corpora.toml`.
    pub fn corpora(&self) -> PathBuf {
        self.root.join("corpora.toml")
    }

    /// `golden/<corpus>.toml`.
    pub fn golden(&self, corpus: &str) -> PathBuf {
        self.root.join("golden").join(format!("{corpus}.toml"))
    }

    /// `known_failures.toml` (optional; absent = empty ledger).
    pub fn ledger(&self) -> PathBuf {
        self.root.join("known_failures.toml")
    }

    /// `baseline.json` (optional for `run`; absent = "bless required").
    pub fn baseline(&self) -> PathBuf {
        self.root.join("baseline.json")
    }
}

/// Everything loaded from the data dir before any corpus is touched.
#[derive(Debug, Clone)]
pub struct Inputs {
    /// The corpora this run covers, in `corpora.toml` order.
    pub corpora: Vec<CorpusSpec>,
    /// Every corpus name in `corpora.toml` (ledger ids are assigned to
    /// these, even under `--only`).
    pub all_names: Vec<String>,
    /// Golden files of the covered corpora, by name.
    pub goldens: BTreeMap<String, LoadedGolden>,
    pub ledger: Ledger,
    pub baseline: Option<Baseline>,
    /// Whether every corpus in `corpora.toml` is covered.
    pub complete: bool,
}

impl Inputs {
    /// Load `corpora.toml`, the ledger, the covered corpora's golden files,
    /// and the baseline (if any).
    ///
    /// # Errors
    ///
    /// An unknown `--only` corpus, an invalid data file, or a ledger id that
    /// belongs to no corpus in `corpora.toml`.
    pub fn load(data: &DataDir, only: Option<&str>) -> anyhow::Result<Self> {
        let specs = load_corpora(&data.corpora())?;
        let all_names: Vec<String> = specs.iter().map(|s| s.name.clone()).collect();
        let corpora: Vec<CorpusSpec> = match only {
            None => specs,
            Some(name) => vec![find_corpus(&specs, name, "--only", &data.corpora())?.clone()],
        };
        let complete = corpora.len() == all_names.len();

        let ledger_path = data.ledger();
        let ledger = Ledger::load(&ledger_path)?;
        let names: Vec<&str> = all_names.iter().map(String::as_str).collect();
        let unassigned = ledger.unassigned(&names);
        anyhow::ensure!(
            unassigned.is_empty(),
            "{} names id(s) that belong to no corpus in corpora.toml: {}",
            ledger_path.display(),
            unassigned.join(", ")
        );

        let goldens = corpora
            .iter()
            .map(|s| Ok((s.name.clone(), load_golden(&data.golden(&s.name))?)))
            .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
        let baseline = Baseline::load(&data.baseline())?;

        Ok(Inputs {
            corpora,
            all_names,
            goldens,
            ledger,
            baseline,
            complete,
        })
    }
}

/// SHA-256 of each named corpus's golden file on disk (names whose file is
/// missing are left out), for `bless`.
///
/// # Errors
///
/// A golden file that exists but cannot be read or parsed.
pub fn golden_hashes_on_disk(
    data: &DataDir,
    names: &[&str],
) -> anyhow::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for name in names {
        let path = data.golden(name);
        if path.exists() {
            out.insert((*name).to_string(), load_golden(&path)?.sha256);
        }
    }
    Ok(out)
}

// ============================================================================
// Run
// ============================================================================

/// One corpus's contribution to the report.
struct CorpusRun {
    report: CorpusReport,
    samples: CorpusSamples,
    latency: LatencyStats,
}

/// Run every covered corpus and assemble the report (gate included).
///
/// # Errors
///
/// Any harness error (see the module docs), naming the corpus and, for skim
/// calls, the golden entry.
pub fn run(
    inputs: &Inputs,
    source: &dyn CorpusSource,
    runner: &SkimRunner,
) -> anyhow::Result<Report> {
    let mut corpora = Vec::with_capacity(inputs.corpora.len());
    let mut samples = Vec::with_capacity(inputs.corpora.len());
    let mut latency = BTreeMap::new();
    for spec in &inputs.corpora {
        let golden = inputs
            .goldens
            .get(&spec.name)
            .with_context(|| format!("no golden file loaded for corpus {}", spec.name))?;
        let run = run_corpus(spec, golden, inputs, source, runner)
            .with_context(|| format!("corpus {}", spec.name))?;
        latency.insert(spec.name.clone(), run.latency);
        corpora.push(run.report);
        samples.push(run.samples);
    }

    let aggregate_ratchet = metrics::ratchet_values(&samples.iter().collect::<Vec<_>>());
    let gate = gate::evaluate(&GateInputs {
        corpora: &corpora,
        aggregate: &aggregate_ratchet,
        complete: inputs.complete,
        baseline: inputs.baseline.as_ref(),
    });
    Ok(Report {
        schema: REPORT_SCHEMA,
        golden_sha256: golden_set_sha256(inputs.goldens.values()),
        complete: inputs.complete,
        aggregate: AggregateReport {
            hard: tally(corpora.iter().flat_map(|c| c.checks.iter())),
            ratchet: aggregate_ratchet,
        },
        corpora,
        gate,
        latency: LatencyReport { corpora: latency },
    })
}

fn run_corpus(
    spec: &CorpusSpec,
    golden: &LoadedGolden,
    inputs: &Inputs,
    source: &dyn CorpusSource,
    runner: &SkimRunner,
) -> anyhow::Result<CorpusRun> {
    let name = spec.name.as_str();
    progress(name, "verifying the pinned clone");
    let root = materialize_verified(source, spec)?;

    progress(name, "computing the oracle universe");
    let universe = Universe::compute(&root, &runner.sandbox().git())?;
    universe.check_file_cap()?;
    let plan = checked_plan(spec, golden, inputs, &universe)?;

    progress(name, "skim search --build");
    runner.build(&root)?;
    let stats = runner.stats(&root)?;

    progress(name, &format!("running {} golden entries", plan.len()));
    let mut observations = Vec::with_capacity(plan.len());
    let mut timings = Vec::with_capacity(plan.len());
    for q in &plan {
        let observed = runner
            .observe(&root, q)
            .with_context(|| format!("entry {}", q.id))?;
        observations.push(observed.observation);
        timings.push((q.id.clone(), observed.timings));
    }

    let after = source.verify_untouched(spec, &root)?;
    anyhow::ensure!(
        after.is_reusable(),
        "the corpus changed during the run (skim must not write into the corpus root): {after}"
    );

    let eval = metrics::evaluate(&universe, &stats, &plan, &observations)?;
    let report = corpus_report(spec, golden, &universe, &stats, &eval, &inputs.ledger)?;
    Ok(CorpusRun {
        report,
        samples: eval.samples,
        latency: latency_stats(&timings),
    })
}

/// Check golden integrity against the verified universe (ledger refs
/// included), then plan the entries; a ledger ref naming a check the plan
/// never runs is an integrity problem too.
fn checked_plan(
    spec: &CorpusSpec,
    golden: &LoadedGolden,
    inputs: &Inputs,
    universe: &Universe,
) -> anyhow::Result<Vec<PlannedQuery>> {
    let names: Vec<&str> = inputs.all_names.iter().map(String::as_str).collect();
    let refs = inputs.ledger.refs_for_corpus(&spec.name, &names);
    let mut problems: Vec<String> = check_integrity(
        &golden.file,
        &IntegrityContext {
            corpus: &spec.name,
            commit: &spec.commit,
            universe: Some(universe),
            ledger: &refs,
        },
    )
    .iter()
    .map(ToString::to_string)
    .collect();
    let plan = if problems.is_empty() {
        let plan = metrics::plan(&golden.file)?;
        problems.extend(gate::unplanned_ledger_refs(&refs, &plan));
        plan
    } else {
        Vec::new()
    };
    anyhow::ensure!(
        problems.is_empty(),
        "golden integrity failed ({} problem(s)):\n  {}",
        problems.len(),
        problems.join("\n  ")
    );
    Ok(plan)
}

/// One corpus's report section, with the ledger applied to its outcomes.
fn corpus_report(
    spec: &CorpusSpec,
    golden: &LoadedGolden,
    universe: &Universe,
    stats: &StatsSnapshot,
    eval: &CorpusEvaluation,
    ledger: &Ledger,
) -> anyhow::Result<CorpusReport> {
    let coverage = universe.coverage();
    Ok(CorpusReport {
        name: spec.name.clone(),
        commit: spec.commit.clone(),
        golden_sha256: golden.sha256.clone(),
        universe: UniverseReport {
            oracle: u64::try_from(universe.len())?,
            skim_file_count: stats.file_count,
            delta: eval.samples.universe_delta,
            skipped_by_reason: SkippedByReason {
                oracle: universe.persisted_skipped_by_reason(),
                skim: stats.skipped_by_reason.clone(),
            },
        },
        coverage: CoverageReport {
            indexed_tracked: u64::try_from(coverage.indexed_tracked)?,
            tracked_text: u64::try_from(coverage.tracked_text)?,
            ratio: round4(coverage.ratio()),
        },
        checks: gate::apply_ledger(&eval.outcomes, ledger),
        ratchet: metrics::ratchet_values(&[&eval.samples]),
        info: CorpusInfo {
            oracle_skipped_by_reason: universe.skipped_by_reason(),
            unindexed_hits: eval.unindexed_hits.clone(),
            idents: eval.samples.idents.clone(),
            concepts: eval.samples.concepts.clone(),
        },
    })
}

fn progress(corpus: &str, step: &str) {
    eprintln!("[scoreboard] {corpus}: {step}");
}

/// Nearest-rank percentiles over every call, plus each entry's total
/// wall-clock time (INFO).
fn latency_stats(entries: &[(String, Vec<Timing>)]) -> LatencyStats {
    let timings: Vec<Timing> = entries
        .iter()
        .flat_map(|(_, t)| t.iter().copied())
        .collect();
    let wall: Vec<f64> = timings.iter().map(|t| t.wall_ms).collect();
    let reported: Vec<f64> = timings
        .iter()
        .filter_map(|t| t.duration_ms)
        .map(|d| d as f64)
        .collect();
    LatencyStats {
        calls: timings.len() as u64,
        wall_ms_p50: percentile_f64(&wall, 0.50).map_or(0.0, round4),
        wall_ms_p95: percentile_f64(&wall, 0.95).map_or(0.0, round4),
        duration_ms_p50: percentile_f64(&reported, 0.50),
        duration_ms_p95: percentile_f64(&reported, 0.95),
        entries_wall_ms: entries
            .iter()
            .map(|(id, t)| (id.clone(), round4(t.iter().map(|t| t.wall_ms).sum())))
            .collect(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
mod tests {
    use super::*;

    #[test]
    fn latency_uses_nearest_rank_percentiles_and_totals_each_entry() {
        let t = |ms: f64, d: Option<u64>| Timing {
            wall_ms: ms,
            duration_ms: d,
        };
        let s = latency_stats(&[
            ("a-1".to_string(), vec![t(4.0, Some(3)), t(1.0, None)]),
            ("a-2".to_string(), vec![t(3.0, Some(1)), t(2.0, None)]),
        ]);
        assert_eq!(s.calls, 4);
        assert_eq!((s.wall_ms_p50, s.wall_ms_p95), (2.0, 4.0));
        assert_eq!(
            (s.duration_ms_p50, s.duration_ms_p95),
            (Some(1.0), Some(3.0))
        );
        assert_eq!(
            s.entries_wall_ms,
            BTreeMap::from([("a-1".to_string(), 5.0), ("a-2".to_string(), 5.0)])
        );
        assert_eq!(latency_stats(&[]).wall_ms_p50, 0.0);
    }

    #[test]
    fn data_dir_paths_follow_the_documented_layout() {
        let d = DataDir::new("/data");
        assert_eq!(d.corpora(), PathBuf::from("/data/corpora.toml"));
        assert_eq!(d.golden("skim"), PathBuf::from("/data/golden/skim.toml"));
        assert_eq!(d.ledger(), PathBuf::from("/data/known_failures.toml"));
        assert_eq!(d.baseline(), PathBuf::from("/data/baseline.json"));
    }

    #[test]
    fn an_unknown_only_corpus_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("corpora.toml"),
            "[[repos]]\nurl = \"https://example.invalid/x/skim\"\ncommit = \"b8a0a79463382347820f1c2572bde37b68e87c76\"\nlanguage = \"Rust\"\n",
        )
        .unwrap();
        let err = Inputs::load(&DataDir::new(dir.path()), Some("zod")).unwrap_err();
        assert!(format!("{err:#}").contains("known: skim"), "{err:#}");
    }
}
