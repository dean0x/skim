//! One scoreboard run, end to end (#203): load the data dir, then for each
//! corpus materialize and verify the pinned clone, compute the oracle
//! universe, check golden integrity, drive skim, verify the clone again,
//! score, and apply the ledger; finally aggregate, gate, and assemble the
//! report.
//!
//! Every `Err` here is a harness error (exit 2): a missing or invalid data
//! file, clone verification, golden integrity (including a ledger entry that
//! can never apply), a skim crash / timeout / unparsable output, a temporal
//! ranking skim reports it cannot apply (see [`require_temporal_data`]), an
//! empty list for an entry with no oracle (see [`require_oracle_less_rows`]),
//! a vacuous `[[ast]]` entry (see [`require_non_vacuous_structural`]), an
//! `[[ast]]` false-positive guard whose oracle matches a file (see
//! [`require_expected_empty_oracles`]), a structural oracle failure, or a
//! corpus that changed under the run. Gate
//! failures are not errors — they are recorded in the report's `gate`
//! section.
//!
//! `[[ast]]` entries (#541): the structural oracle is compiled once per run
//! and run over each corpus's universe once. On a corpus with an `[[ast]]`
//! entry, skim is called once per catalog pattern with `--ast <pattern>`
//! ([`called_patterns`]: patterns with no entry in the corpus, and patterns
//! the oracle cannot encode, included), and the rows are split among that
//! pattern's entries by language; rows no entry scores are counted in
//! `structural.unscored_rows.<pattern>`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use rskim_oracle::structural::StructuralOracle;

use crate::scoreboard::baseline::Baseline;
use crate::scoreboard::catalog::{CatalogPattern, skim_catalog};
use crate::scoreboard::corpus::{
    CorpusSource, CorpusSpec, find_corpus, load_corpora, materialize_verified,
};
use crate::scoreboard::fmt::{entries_noun, round4};
use crate::scoreboard::gate::{self, GateInputs, Ledger};
use crate::scoreboard::golden::{
    IntegrityContext, LoadedGolden, check_integrity, golden_set_sha256, load_golden,
};
use crate::scoreboard::metrics::{
    self, CorpusEvaluation, CorpusSamples, PlannedQuery, percentile_f64,
};
use crate::scoreboard::report::{
    AggregateReport, CorpusInfo, CorpusReport, CoverageReport, LatencyReport, LatencyStats,
    REPORT_SCHEMA, Report, SkippedByReason, StructuralReport, UniverseReport, tally,
};
use crate::scoreboard::runner::{EntryObservation, SkimRunner, Timing};
use crate::scoreboard::structural_metrics::{
    OracleAnswers, StructuralEvidence, called_patterns, coverage_comparison, is_vacuous,
    uncovered_patterns, unexpected_oracle_matches,
};
use crate::scoreboard::types::{AstPage, StatsSnapshot};
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
    /// skim's pattern catalog ([`skim_catalog`]), read once here and passed
    /// to golden integrity, the `--ast` call set and `uncovered_patterns`.
    pub catalog: Vec<CatalogPattern>,
}

impl Inputs {
    /// Load `corpora.toml`, the ledger, the covered corpora's golden files,
    /// the baseline (if any), and skim's pattern catalog.
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
            catalog: skim_catalog(),
        })
    }
}

/// The golden digest ([`crate::scoreboard::golden::golden_digest`]) of each
/// named corpus's golden file on disk — its bytes, with this scoreboard's
/// structural-oracle fingerprint folded in when it has `[[ast]]` entries —
/// for `bless` (names whose file is missing are left out).
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
    // Compiled once per run, and only when some corpus has [[ast]] entries.
    let oracle = inputs
        .goldens
        .values()
        .any(|g| !g.file.asts.is_empty())
        .then(StructuralOracle::new)
        .transpose()
        .context("compiling the structural oracle")?;
    for spec in &inputs.corpora {
        let run = run_corpus(spec, inputs, source, runner, oracle.as_ref())
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
        uncovered_patterns: uncovered_patterns(
            &inputs.catalog,
            inputs.goldens.values().map(|g| &g.file),
        ),
        gate,
        latency: LatencyReport { corpora: latency },
    })
}

fn run_corpus(
    spec: &CorpusSpec,
    inputs: &Inputs,
    source: &dyn CorpusSource,
    runner: &SkimRunner,
    oracle: Option<&StructuralOracle>,
) -> anyhow::Result<CorpusRun> {
    let name = spec.name.as_str();
    let golden = inputs
        .goldens
        .get(name)
        .with_context(|| format!("no golden file loaded for corpus {name}"))?;
    progress(name, "verifying the pinned clone");
    let root = materialize_verified(source, spec)?;

    progress(name, "computing the oracle universe");
    let universe = Universe::compute(&root, &runner.sandbox().git())?;
    universe.check_file_cap()?;
    let plan = checked_plan(spec, golden, inputs, &universe)?;
    let (answers, oracle_wall_ms) = structural_answers(name, &plan, &universe, oracle)?;
    require_expected_empty_oracles(&plan, &answers)?;

    progress(name, "skim search --build");
    runner.build(&root)?;
    let stats = runner.stats(&root)?;
    require_temporal_data(&plan, &stats)?;

    progress(name, &format!("running {} golden entries", plan.len()));
    let observed = observe_plan(runner, &root, &plan, &inputs.catalog)?;
    require_oracle_less_rows(&plan, &observed.observations)?;
    let evidence = StructuralEvidence {
        answers,
        patterns: observed.patterns,
    };
    require_non_vacuous_structural(&plan, &observed.observations, &evidence.answers)?;

    let after = source.verify_untouched(spec, &root)?;
    anyhow::ensure!(
        after.is_reusable(),
        "the corpus changed during the run (skim must not write into the corpus root): {after}"
    );

    let eval = metrics::evaluate(&universe, &stats, &plan, &observed.observations, &evidence)?;
    let report = corpus_report(
        spec,
        golden,
        &CorpusEvidence {
            universe: &universe,
            stats: &stats,
            eval: &eval,
            structural: &evidence,
        },
        &inputs.ledger,
    )?;
    Ok(CorpusRun {
        report,
        samples: eval.samples,
        latency: latency_stats(&observed.timings, &observed.pattern_timings, oracle_wall_ms),
    })
}

/// The structural oracle's answers over `universe`, and the pass's
/// wall-clock milliseconds (INFO), when `plan` has `[[ast]]` entries; empty
/// answers and no time otherwise.
///
/// # Errors
///
/// `[[ast]]` entries but no compiled oracle, or an oracle failure on a file.
fn structural_answers(
    corpus: &str,
    plan: &[PlannedQuery],
    universe: &Universe,
    oracle: Option<&StructuralOracle>,
) -> anyhow::Result<(OracleAnswers, Option<f64>)> {
    if !has_ast_entries(plan) {
        return Ok((OracleAnswers::default(), None));
    }
    let oracle = oracle.context("[[ast]] entries but no structural oracle was compiled")?;
    progress(corpus, "running the structural oracle");
    let started = Instant::now();
    let answers = OracleAnswers::compute(oracle, universe.files()).context("structural oracle")?;
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    progress(
        corpus,
        &format!(
            "structural oracle: {wall_ms:.1} ms over {} universe file(s)",
            universe.len()
        ),
    );
    Ok((answers, Some(wall_ms)))
}

/// Every observation of one corpus's plan.
struct PlanObservations {
    /// One per planned entry, in plan order.
    observations: Vec<EntryObservation>,
    /// The golden entries' own call timings, by entry id (an `[[ast]]`
    /// entry makes no call of its own).
    timings: Vec<(String, Vec<Timing>)>,
    /// Each `--ast <pattern>` call's timing, by pattern ([`ast_calls`]).
    pattern_timings: BTreeMap<String, Timing>,
    /// skim's `--ast <pattern>` answer per called pattern ([`ast_calls`]).
    patterns: BTreeMap<String, AstPage>,
}

/// The `--ast <pattern>` calls a corpus's plan makes: every pattern of `catalog`
/// ([`called_patterns`]) when the plan has an `[[ast]]` entry, so no skim row
/// escapes both the entries and `structural.unscored_rows.<pattern>`; none
/// otherwise.
fn ast_calls(plan: &[PlannedQuery], catalog: &[CatalogPattern]) -> Vec<&'static str> {
    if has_ast_entries(plan) {
        called_patterns(catalog)
    } else {
        Vec::new()
    }
}

/// Whether `plan` has an `[[ast]]` entry ([`PlannedQuery::is_structural`]):
/// only then does the corpus run the structural oracle and call skim once
/// per catalog pattern.
fn has_ast_entries(plan: &[PlannedQuery]) -> bool {
    plan.iter().any(PlannedQuery::is_structural)
}

/// skim's `--ast <pattern>` full list and the call's timing for each of
/// `patterns`, called once each, in order, and keyed by pattern: the loop
/// behind both the gate's pattern calls and `golden-gen --ast`'s (every
/// pattern of [`called_patterns`] in each), so the proposal is made from the
/// same answers the gate scores.
///
/// # Errors
///
/// Any skim call error, naming the pattern.
pub fn call_patterns(
    runner: &SkimRunner,
    root: &Path,
    patterns: impl IntoIterator<Item = &'static str>,
) -> anyhow::Result<BTreeMap<String, (AstPage, Timing)>> {
    patterns
        .into_iter()
        .map(|pattern| {
            let call = runner
                .ast_list(root, pattern)
                .with_context(|| format!("--ast {pattern}"))?;
            Ok((pattern.to_string(), call))
        })
        .collect()
}

/// Run every call the plan needs. Each pattern of [`ast_calls`] is called
/// once (`--ast <pattern>`, full list), in pattern order
/// ([`call_patterns`]); an `[[ast]]` entry's observation is its pattern
/// call's rows in its language ([`EntryObservation::for_ast`]).
///
/// # Errors
///
/// Any skim call error, naming the entry or pattern.
fn observe_plan(
    runner: &SkimRunner,
    root: &Path,
    plan: &[PlannedQuery],
    catalog: &[CatalogPattern],
) -> anyhow::Result<PlanObservations> {
    let calls = call_patterns(runner, root, ast_calls(plan, catalog))?;
    let mut pattern_timings = BTreeMap::new();
    let mut patterns: BTreeMap<String, AstPage> = BTreeMap::new();
    for (pattern, (page, timing)) in calls {
        pattern_timings.insert(pattern.clone(), timing);
        patterns.insert(pattern, page);
    }

    let mut timings = Vec::with_capacity(plan.len());
    let mut observations = Vec::with_capacity(plan.len());
    for q in plan {
        if let Some(target) = q.structural_target() {
            let call = patterns
                .get(&target.pattern)
                .with_context(|| format!("entry {}: no --ast {} call", q.id, target.pattern))?;
            observations.push(EntryObservation::for_ast(&q.id, call, target.lang));
            continue;
        }
        let observed = runner
            .observe(root, q)
            .with_context(|| format!("entry {}", q.id))?;
        observations.push(observed.observation);
        timings.push((q.id.clone(), observed.timings));
    }
    Ok(PlanObservations {
        observations,
        timings,
        pattern_timings,
        patterns,
    })
}

/// `--stats --json` `temporal_state` when skim's temporal data is usable.
const TEMPORAL_READY: &str = "ready";

/// Require usable temporal data when some planned entry ranks by it
/// ([`crate::scoreboard::golden::QueryFlags::uses_temporal_data`]).
///
/// Without it skim serves a fallback order and says so only in `--stats`
/// and, on the text arms, in `degraded[]` (the standalone `--ast` arm cannot
/// carry `degraded[]`, #483). Scoring that order would misreport the broken
/// temporal layer: a ledgered `--hot` check can "XPASS" and ask for a
/// promotion that bakes the breakage into the ledger and the baseline. So it
/// is a harness error, never a gate result.
///
/// # Errors
///
/// Some entry ranks by temporal data and `temporal_state` is absent or not
/// `"ready"`; the message names the state and those entries.
pub fn require_temporal_data(plan: &[PlannedQuery], stats: &StatsSnapshot) -> anyhow::Result<()> {
    let ranked: Vec<&str> = plan
        .iter()
        .filter(|q| q.flags.uses_temporal_data())
        .map(|q| q.id.as_str())
        .collect();
    if ranked.is_empty() || stats.temporal_state.as_deref() == Some(TEMPORAL_READY) {
        return Ok(());
    }
    let state = match stats.temporal_state.as_deref() {
        Some(state) => format!("temporal_state {state:?}"),
        None => "no temporal_state".to_string(),
    };
    anyhow::bail!(
        "skim search --stats --json reports {state} after --build (needs {TEMPORAL_READY:?}): \
         skim cannot apply the temporal ranking that {} golden {} ask for ({}), so their \
         ordering checks would judge a fallback order",
        ranked.len(),
        entries_noun(ranked.len()),
        ranked.join(", ")
    )
}

/// Refuse to score an entry with no oracle whose full list is empty.
///
/// An entry with no oracle (`--ast`, `--blast-radius`, a standalone `--hot` /
/// `--cold` / `--risky` run) has no ground truth: its checks compare skim's
/// answers only with each other, and an empty list satisfies every one of
/// them. If the structural (or temporal) layer broke and returned nothing,
/// the ledgered `order.score_monotone` failures (#547) would "XPASS" and ask
/// for a promotion that bakes the breakage into the ledger and the baseline.
/// So it is a harness error, never a gate result. A list that shrinks
/// without emptying moves its `oracle_less.full_rows.<id>` RATCHET value.
///
/// # Errors
///
/// Some entry with no oracle has an empty full list; the message names those
/// entries.
pub fn require_oracle_less_rows(
    plan: &[PlannedQuery],
    observations: &[EntryObservation],
) -> anyhow::Result<()> {
    let empty: BTreeSet<&str> = observations
        .iter()
        .filter(|o| o.full.rows.is_empty())
        .map(|o| o.id.as_str())
        .collect();
    let vacuous: Vec<&str> = plan
        .iter()
        .filter(|q| q.oracle.is_none() && empty.contains(q.id.as_str()))
        .map(|q| q.id.as_str())
        .collect();
    if vacuous.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "skim returned an empty full list for {} golden {} with no oracle ({}): with no \
         ground truth, an empty list passes every check that runs on it, so a broken structural \
         or temporal layer would read as a pass, and a ledgered failure as fixed; fix skim, or \
         replace the entry with one that matches on this corpus",
        vacuous.len(),
        entries_noun(vacuous.len()),
        vacuous.join(", ")
    )
}

/// Refuse to score a vacuous `[[ast]]` entry ([`is_vacuous`]): the
/// structural oracle matches no file of its language and skim returns no row
/// in it, or, for a false-positive guard (`expect_oracle_empty = true`), the
/// corpus has no scored file in its language.
///
/// Every structural check passes on such an entry, so it measures nothing,
/// and a regression that made skim return nothing there could never show.
/// It is a golden error (`golden-gen` proposes only non-vacuous entries):
/// remove the entry, or pick a corpus where the pattern occurs. A guard with
/// scored files in its language is exempt: skim returning nothing is the
/// fixed state it guards, and [`require_expected_empty_oracles`] keeps its
/// oracle empty.
///
/// # Errors
///
/// Some `[[ast]]` entry is vacuous: a golden-integrity error
/// (`integrity_failure`) with one `<id>: <reason>` line per entry, then
/// the remediation. Also `observations` that do not follow `plan`
/// ([`metrics::paired`]), or an entry the oracle has no query for
/// (integrity rejects that first).
pub fn require_non_vacuous_structural(
    plan: &[PlannedQuery],
    observations: &[EntryObservation],
    answers: &OracleAnswers,
) -> anyhow::Result<()> {
    let mut vacuous = Vec::new();
    for (q, obs) in metrics::paired(plan, observations)? {
        let Some(target) = q.structural_target() else {
            continue;
        };
        if !is_vacuous(target, &obs.full.rows, answers)? {
            continue;
        }
        vacuous.push(if target.expect_oracle_empty {
            format!(
                "{}: vacuous false-positive guard: no scored {} file, so a false positive has \
                 nowhere to land",
                q.id, target.lang
            )
        } else {
            format!(
                "{}: vacuous [[ast]] entry: neither the structural oracle nor skim finds {} in a \
                 {} file, so every check would pass on nothing",
                q.id, target.pattern, target.lang
            )
        });
    }
    if vacuous.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "{}\nremove the entry (golden-gen proposes only entries where the oracle or skim finds a \
         file); an entry that guards a skim false positive the oracle rejects declares \
         `expect_oracle_empty = true` and is exempt while the corpus has a scored file in its \
         language",
        integrity_failure(&vacuous)
    )
}

/// Refuse a stale false-positive guard: an `[[ast]]` entry declaring
/// `expect_oracle_empty = true` whose structural oracle matches a file.
///
/// The flag exempts the entry from the vacuity guard because its oracle is
/// empty by declaration. Once the oracle matches (a pin bump, an oracle
/// edit), the entry measures recall too, and a flag left in place would keep
/// excusing the both-empty state that hides a recall loss. It is a golden
/// error, checked before skim runs (it needs only the oracle): remove the
/// flag in a reviewed golden edit, then bless.
///
/// # Errors
///
/// Some flagged entry's oracle matches a file: a golden-integrity error
/// (`integrity_failure`) with one `<id>: <reason>` line per entry (naming
/// a matched file), then the remediation. Also an entry the oracle has no
/// query for (integrity rejects that first).
pub fn require_expected_empty_oracles(
    plan: &[PlannedQuery],
    answers: &OracleAnswers,
) -> anyhow::Result<()> {
    let mut stale = Vec::new();
    for q in plan {
        let Some(target) = q.structural_target() else {
            continue;
        };
        if let Some(matches) = unexpected_oracle_matches(target, answers)? {
            let first = matches.keys().next().map_or("", String::as_str);
            stale.push(format!(
                "{}: declares `expect_oracle_empty = true` but the structural oracle matches {} \
                 file(s), e.g. {first}, so the entry now measures recall",
                q.id,
                matches.len()
            ));
        }
    }
    if stale.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "{}\nremove the flag in a reviewed golden edit, then bless",
        integrity_failure(&stale)
    )
}

/// The golden-integrity error text: `golden integrity failed (N
/// problem(s)):`, then one indented line per problem (`<id>: <reason>` for
/// an entry's). [`checked_plan`] reports [`check_integrity`]'s violations
/// with it, and the two `[[ast]]` guards that need the oracle's answers
/// ([`require_expected_empty_oracles`], [`require_non_vacuous_structural`])
/// add their remediation on the line after.
fn integrity_failure(problems: &[String]) -> String {
    format!(
        "golden integrity failed ({} problem(s)):\n  {}",
        problems.len(),
        problems.join("\n  ")
    )
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
            catalog: &inputs.catalog,
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
    anyhow::ensure!(problems.is_empty(), "{}", integrity_failure(&problems));
    Ok(plan)
}

/// What one corpus's run observed and scored, for its report section: the
/// oracle universe, skim's `--stats`, the evaluation and the structural
/// evidence. They always travel together, so the next per-corpus input
/// (#542's temporal oracle) is one more field here.
struct CorpusEvidence<'a> {
    universe: &'a Universe,
    stats: &'a StatsSnapshot,
    eval: &'a CorpusEvaluation,
    structural: &'a StructuralEvidence,
}

/// One corpus's report section, with the ledger applied to its outcomes.
fn corpus_report(
    spec: &CorpusSpec,
    golden: &LoadedGolden,
    evidence: &CorpusEvidence<'_>,
    ledger: &Ledger,
) -> anyhow::Result<CorpusReport> {
    let CorpusEvidence {
        universe,
        stats,
        eval,
        structural,
    } = *evidence;
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
        structural: StructuralReport {
            coverage: coverage_comparison(structural),
            entries: eval.samples.structural.clone(),
            unscored_rows: eval.samples.unscored_rows.clone(),
        },
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

/// One corpus's latency (INFO): nearest-rank percentiles over the golden
/// entries' own calls (`entries`, by entry id) and each entry's total
/// wall-clock time; each `--ast <pattern>` call's wall-clock time
/// (`pattern_calls`), kept out of the percentiles and the per-entry totals;
/// and the structural oracle pass. Milliseconds are rounded to 4 decimals.
fn latency_stats(
    entries: &[(String, Vec<Timing>)],
    pattern_calls: &BTreeMap<String, Timing>,
    structural_oracle_wall_ms: Option<f64>,
) -> LatencyStats {
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
        pattern_calls_wall_ms: pattern_calls
            .iter()
            .map(|(pattern, t)| (pattern.clone(), round4(t.wall_ms)))
            .collect(),
        structural_oracle_wall_ms: structural_oracle_wall_ms.map(round4),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
mod tests {
    use super::*;
    use crate::scoreboard::test_support::{catalog, oracle};

    #[test]
    fn latency_uses_nearest_rank_percentiles_and_totals_each_entry() {
        let t = |ms: f64, d: Option<u64>| Timing {
            wall_ms: ms,
            duration_ms: d,
        };
        let entries = [
            ("a-1".to_string(), vec![t(4.0, Some(3)), t(1.0, None)]),
            ("a-2".to_string(), vec![t(3.0, Some(1)), t(2.0, None)]),
        ];
        let s = latency_stats(&entries, &BTreeMap::new(), None);
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
        assert!(s.pattern_calls_wall_ms.is_empty());
        assert_eq!(s.structural_oracle_wall_ms, None);
        assert_eq!(latency_stats(&[], &BTreeMap::new(), None).wall_ms_p50, 0.0);

        // Pattern calls (slow here) and the oracle pass are reported on their
        // own: the entry count, percentiles and per-entry totals are the
        // same as without them.
        let pattern_calls = BTreeMap::from([
            ("go-select".to_string(), t(900.0, Some(899))),
            ("try-catch".to_string(), t(1.234_56, None)),
        ]);
        let with = latency_stats(&entries, &pattern_calls, Some(812.345_67));
        assert_eq!(
            LatencyStats {
                pattern_calls_wall_ms: BTreeMap::new(),
                structural_oracle_wall_ms: None,
                ..with.clone()
            },
            s
        );
        assert_eq!(
            with.pattern_calls_wall_ms,
            BTreeMap::from([
                ("go-select".to_string(), 900.0),
                ("try-catch".to_string(), 1.2346)
            ])
        );
        assert_eq!(with.structural_oracle_wall_ms, Some(812.3457));
    }

    fn plan_of(entries: &str) -> Vec<PlannedQuery> {
        let golden = crate::scoreboard::golden::parse_golden(&format!(
            "corpus = \"skim\"\ncommit = \"b8a0a79463382347820f1c2572bde37b68e87c76\"\n{entries}"
        ))
        .unwrap();
        metrics::plan(&golden).unwrap()
    }

    fn stats_with(temporal_state: Option<&str>) -> StatsSnapshot {
        StatsSnapshot {
            file_count: 1,
            skipped_by_reason: BTreeMap::new(),
            temporal_state: temporal_state.map(str::to_string),
        }
    }

    #[test]
    fn entries_ranked_by_temporal_data_need_a_ready_temporal_layer() {
        let ranked = plan_of(
            "[[prefix]]\nid = \"skim-F001\"\nquery = \"fn\"\nflags = [\"--hot\"]\nlimits = [5]\n\
             [[prefix]]\nid = \"skim-F002\"\nflags = [\"--ast\", \"match-with-arms\", \"--hot\"]\nlimits = [5]\n\
             [[prefix]]\nid = \"skim-F004\"\nflags = [\"--blast-radius\", \"src/a.rs\"]\nlimits = [5]\n\
             [[lexical]]\nid = \"skim-X01\"\nquery = \"fn\"\ncategory = \"short\"\n",
        );
        require_temporal_data(&ranked, &stats_with(Some("ready"))).unwrap();

        // skim's documented not-ready states, and a stats envelope without the key.
        for state in [
            Some("newer-schema"),
            Some("missing"),
            Some("empty"),
            Some("corrupt"),
            None,
        ] {
            let err = require_temporal_data(&ranked, &stats_with(state))
                .expect_err("temporal ordering cannot be judged without temporal data");
            let msg = format!("{err:#}");
            assert!(msg.contains(state.unwrap_or("no temporal_state")), "{msg}");
            for id in ["skim-F001", "skim-F002", "skim-F004"] {
                assert!(msg.contains(id), "{id} ranks by temporal data: {msg}");
            }
            assert!(
                !msg.contains("skim-X01"),
                "a lexical entry is unaffected: {msg}"
            );
        }

        // A corpus with no temporal entry does not depend on the temporal layer.
        let lexical_only =
            plan_of("[[lexical]]\nid = \"skim-X01\"\nquery = \"fn\"\ncategory = \"short\"\n");
        require_temporal_data(&lexical_only, &stats_with(Some("missing"))).unwrap();
        require_temporal_data(&lexical_only, &stats_with(None)).unwrap();
    }

    /// An observation whose full list has `rows` rows.
    fn observed(id: &str, rows: usize) -> EntryObservation {
        use crate::scoreboard::types::{ResultPage, ResultRow, VerifyMode};
        EntryObservation {
            id: id.to_string(),
            full: ResultPage {
                rows: (0..rows)
                    .map(|i| ResultRow {
                        path: format!("src/{i}.rs"),
                        score: 1.0,
                        line: None,
                        snippet: Vec::new(),
                    })
                    .collect(),
                has_more: false,
                verify_mode: VerifyMode::Substring,
                degraded: Vec::new(),
            },
            sweeps: Vec::new(),
            limited: Vec::new(),
            text: None,
        }
    }

    #[test]
    fn an_empty_list_without_an_oracle_is_a_harness_error() {
        // plan order: [[lexical]] first, then the [[prefix]] entries.
        let plan = plan_of(
            "[[prefix]]\nid = \"skim-F002\"\nflags = [\"--ast\", \"match-with-arms\", \"--hot\"]\nlimits = [5]\n\
             [[prefix]]\nid = \"skim-F003\"\nflags = [\"--ast\", \"god-function\"]\nlimits = [5]\n\
             [[prefix]]\nid = \"skim-F004\"\nflags = [\"--blast-radius\", \"src/a.rs\"]\nlimits = [5]\n\
             [[prefix]]\nid = \"skim-F005\"\nquery = \"fn\"\nflags = [\"--ast\", \"god-function\"]\nlimits = [5]\n\
             [[prefix]]\nid = \"skim-F006\"\nflags = [\"--hot\"]\nlimits = [5]\n\
             [[lexical]]\nid = \"skim-Z01\"\nquery = \"qqqq\"\ncategory = \"zero-hit\"\n",
        );
        assert_eq!(plan.iter().filter(|q| q.oracle.is_none()).count(), 5);

        // An empty list is fine where an oracle judges it (a zero-hit entry).
        let healthy = [
            observed("skim-Z01", 0),
            observed("skim-F002", 3),
            observed("skim-F003", 1),
            observed("skim-F004", 2),
            observed("skim-F005", 4),
            observed("skim-F006", 9),
        ];
        require_oracle_less_rows(&plan, &healthy).unwrap();

        let broken = [
            observed("skim-Z01", 0),
            observed("skim-F002", 3),
            observed("skim-F003", 0),
            observed("skim-F004", 0),
            observed("skim-F005", 0),
            observed("skim-F006", 0),
        ];
        let err = require_oracle_less_rows(&plan, &broken)
            .expect_err("an empty list without an oracle passes every check vacuously");
        let msg = format!("{err:#}");
        assert!(msg.contains("empty"), "{msg}");
        for id in ["skim-F003", "skim-F004", "skim-F005", "skim-F006"] {
            assert!(msg.contains(id), "{id} has no oracle and no rows: {msg}");
        }
        for id in ["skim-F002", "skim-Z01"] {
            assert!(!msg.contains(id), "{id} is not vacuous: {msg}");
        }
    }

    #[test]
    fn a_vacuous_ast_entry_is_a_golden_error() {
        let plan = plan_of(
            "[[ast]]\nid = \"skim-ast-go-select-go\"\npattern = \"go-select\"\nlang = \"go\"\nprecision = \"hard\"\n\
             [[ast]]\nid = \"skim-ast-go-defer-go\"\npattern = \"go-defer\"\nlang = \"go\"\nprecision = \"hard\"\n\
             [[ast]]\nid = \"skim-ast-rust-nested-loop-rust\"\npattern = \"rust-nested-loop\"\nlang = \"rust\"\nprecision = \"ratchet\"\n",
        );
        // The oracle finds a Rust loop; there is no Go file at all.
        let answers = OracleAnswers::compute(
            oracle(),
            [("src/a.rs", "fn f() {\n    for i in 0..2 {}\n}\n")],
        )
        .unwrap();

        // go-defer is non-vacuous as long as skim returns a row for it.
        let observations = [
            observed("skim-ast-go-select-go", 0),
            observed("skim-ast-go-defer-go", 1),
            observed("skim-ast-rust-nested-loop-rust", 0),
        ];
        let err = require_non_vacuous_structural(&plan, &observations, &answers)
            .expect_err("neither the oracle nor skim finds a go-select");
        let msg = format!("{err:#}");
        // `checked_plan`'s format: the count, one `<id>: <reason>` line each.
        assert!(
            msg.starts_with(
                "golden integrity failed (1 problem(s)):\n  skim-ast-go-select-go: vacuous [[ast]] \
                 entry: "
            ),
            "{msg}"
        );
        for fine in ["skim-ast-go-defer-go", "skim-ast-rust-nested-loop-rust"] {
            assert!(!msg.contains(fine), "{fine} is not vacuous: {msg}");
        }

        // Two vacuous entries: one line each, the remediation once.
        let both = [
            observed("skim-ast-go-select-go", 0),
            observed("skim-ast-go-defer-go", 0),
            observed("skim-ast-rust-nested-loop-rust", 0),
        ];
        let msg = format!(
            "{:#}",
            require_non_vacuous_structural(&plan, &both, &answers).unwrap_err()
        );
        let lines: Vec<&str> = msg.lines().collect();
        assert_eq!(lines.len(), 4, "{msg}");
        assert_eq!(lines[0], "golden integrity failed (2 problem(s)):", "{msg}");
        assert!(lines[1].starts_with("  skim-ast-go-select-go: "), "{msg}");
        assert!(lines[2].starts_with("  skim-ast-go-defer-go: "), "{msg}");
        assert!(lines[3].starts_with("remove the entry"), "{msg}");
        assert_eq!(msg.matches("remove the entry").count(), 1, "{msg}");

        let fixed = [
            observed("skim-ast-go-select-go", 1),
            observed("skim-ast-go-defer-go", 1),
            observed("skim-ast-rust-nested-loop-rust", 0),
        ];
        require_non_vacuous_structural(&plan, &fixed, &answers).unwrap();
        // Entries without a structural oracle are not this guard's business.
        let lexical =
            plan_of("[[lexical]]\nid = \"skim-Z01\"\nquery = \"qqqq\"\ncategory = \"zero-hit\"\n");
        require_non_vacuous_structural(&lexical, &[observed("skim-Z01", 0)], &answers).unwrap();
    }

    #[test]
    fn the_vacuity_guard_refuses_observations_that_do_not_follow_the_plan() {
        let plan = plan_of(
            "[[ast]]\nid = \"skim-ast-go-select-go\"\npattern = \"go-select\"\nlang = \"go\"\nprecision = \"hard\"\n\
             [[ast]]\nid = \"skim-ast-go-defer-go\"\npattern = \"go-defer\"\nlang = \"go\"\nprecision = \"hard\"\n",
        );
        // No Go file: go-defer is vacuous unless skim returns a row for it.
        let answers = OracleAnswers::compute(oracle(), [("src/a.rs", "fn f() {}\n")]).unwrap();

        // One observation short: an unchecked zip would never judge go-defer.
        let err = require_non_vacuous_structural(
            &plan,
            &[observed("skim-ast-go-select-go", 1)],
            &answers,
        )
        .expect_err("a missing observation");
        assert!(
            format!("{err:#}").contains("2 planned entries but 1 observations"),
            "{err:#}"
        );

        // As many observations, but paired with the wrong entries.
        let swapped = [
            observed("skim-ast-go-defer-go", 1),
            observed("skim-ast-go-select-go", 1),
        ];
        let err = require_non_vacuous_structural(&plan, &swapped, &answers)
            .expect_err("observations out of plan order");
        assert!(
            format!("{err:#}").contains(
                "observation skim-ast-go-defer-go does not match planned entry skim-ast-go-select-go"
            ),
            "{err:#}"
        );
    }

    #[test]
    fn has_ast_entries_is_true_only_with_an_ast_entry() {
        let with_ast = plan_of(
            "[[ast]]\nid = \"skim-ast-rust-nested-loop-rust\"\npattern = \"rust-nested-loop\"\nlang = \"rust\"\nprecision = \"ratchet\"\n",
        );
        assert!(has_ast_entries(&with_ast));
        // A standalone `--ast` [[prefix]] calls skim with --ast but is not
        // scored by the structural oracle.
        let prefix_only = plan_of(
            "[[prefix]]\nid = \"skim-F003\"\nflags = [\"--ast\", \"god-function\"]\nlimits = [5]\n",
        );
        assert!(!has_ast_entries(&prefix_only));
        assert!(!has_ast_entries(&[]));
    }

    #[test]
    fn skim_is_called_for_every_catalog_pattern_once_the_corpus_has_an_ast_entry() {
        let with_ast = plan_of(
            "[[ast]]\nid = \"skim-ast-rust-nested-loop-rust\"\npattern = \"rust-nested-loop\"\nlang = \"rust\"\nprecision = \"ratchet\"\n\
             [[lexical]]\nid = \"skim-Z01\"\nquery = \"qqqq\"\ncategory = \"zero-hit\"\n",
        );
        let calls = ast_calls(&with_ast, catalog());
        assert_eq!(calls, called_patterns(catalog()));
        // Patterns with no entry in this corpus, and patterns no oracle
        // covers, are called too: their rows count as unscored.
        for pattern in [
            "god-function",
            "go-select",
            "deep-nesting",
            "java-synchronized",
        ] {
            assert!(calls.contains(&pattern), "{pattern}");
        }
        // A corpus with no [[ast]] entry makes no pattern call (its
        // standalone `--ast` [[prefix]] entries are observed on their own).
        let without = plan_of(
            "[[prefix]]\nid = \"skim-F003\"\nflags = [\"--ast\", \"god-function\"]\nlimits = [5]\n\
             [[lexical]]\nid = \"skim-Z01\"\nquery = \"qqqq\"\ncategory = \"zero-hit\"\n",
        );
        assert!(ast_calls(&without, catalog()).is_empty());
    }

    #[test]
    fn a_false_positive_guard_is_exempt_from_vacuity_but_its_flag_must_hold() {
        let oracle = oracle();
        let plan = plan_of(
            "[[ast]]\nid = \"skim-ast-go-select-go\"\npattern = \"go-select\"\nlang = \"go\"\nprecision = \"hard\"\nexpect_oracle_empty = true\n\
             [[ast]]\nid = \"skim-ast-go-defer-go\"\npattern = \"go-defer\"\nlang = \"go\"\nprecision = \"hard\"\n",
        );
        // A Go file with no select and no defer: both oracles are empty.
        let no_select = OracleAnswers::compute(
            oracle,
            [
                ("src/a.rs", "fn f() {}\n"),
                ("cmd/main.go", "package main\n\nfunc f() {}\n"),
            ],
        )
        .unwrap();
        require_expected_empty_oracles(&plan, &no_select).unwrap();

        // Both entries find nothing: the flagged one is the fixed state of a
        // false positive and is scored; the unflagged one is still vacuous.
        let empty = [
            observed("skim-ast-go-select-go", 0),
            observed("skim-ast-go-defer-go", 0),
        ];
        let err = require_non_vacuous_structural(&plan, &empty, &no_select)
            .expect_err("the unflagged entry is vacuous");
        let msg = format!("{err:#}");
        assert!(msg.contains("skim-ast-go-defer-go"), "{msg}");
        assert!(
            !msg.contains("skim-ast-go-select-go"),
            "a guard is exempt: {msg}"
        );
        assert!(msg.contains("expect_oracle_empty"), "{msg}");
        let flagged_only = plan_of(
            "[[ast]]\nid = \"skim-ast-go-select-go\"\npattern = \"go-select\"\nlang = \"go\"\nprecision = \"hard\"\nexpect_oracle_empty = true\n",
        );
        require_non_vacuous_structural(&flagged_only, &empty[..1], &no_select).unwrap();

        // With no scored Go file at all, the guard has nothing to guard: it
        // is vacuous too, and the message says why.
        let no_go = OracleAnswers::compute(oracle, [("src/a.rs", "fn f() {}\n")]).unwrap();
        require_expected_empty_oracles(&flagged_only, &no_go).unwrap();
        let err = require_non_vacuous_structural(&flagged_only, &empty[..1], &no_go)
            .expect_err("a guard in a language the corpus has no scored file in measures nothing");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("golden integrity failed (1 problem(s)):"),
            "{msg}"
        );
        assert!(
            msg.contains(
                "\n  skim-ast-go-select-go: vacuous false-positive guard: no scored go file"
            ),
            "{msg}"
        );

        // Once the oracle matches, the flag is stale: a golden error that
        // names the entry and a matched file, before skim is even run.
        let go = OracleAnswers::compute(
            oracle,
            [(
                "cmd/main.go",
                "package main\n\nfunc f() {\n\tselect {}\n}\n",
            )],
        )
        .unwrap();
        let err = require_expected_empty_oracles(&plan, &go)
            .expect_err("the oracle finds a select statement");
        let msg = format!("{err:#}");
        let lines: Vec<&str> = msg.lines().collect();
        assert_eq!(lines.len(), 3, "{msg}");
        assert_eq!(lines[0], "golden integrity failed (1 problem(s)):", "{msg}");
        assert!(
            lines[1].starts_with(
                "  skim-ast-go-select-go: declares `expect_oracle_empty = true` but the \
                 structural oracle matches 1 file(s), e.g. cmd/main.go"
            ),
            "{msg}"
        );
        assert_eq!(
            lines[2], "remove the flag in a reviewed golden edit, then bless",
            "{msg}"
        );
        assert!(!msg.contains("skim-ast-go-defer-go"), "unflagged: {msg}");
        // Entries without a structural oracle are not this guard's business.
        let lexical =
            plan_of("[[lexical]]\nid = \"skim-Z01\"\nquery = \"qqqq\"\ncategory = \"zero-hit\"\n");
        require_expected_empty_oracles(&lexical, &go).unwrap();
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
