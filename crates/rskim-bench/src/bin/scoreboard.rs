//! `scoreboard` — the end-to-end search quality gate (#203).
//!
//! Drives a `skim` binary against pinned corpora and checks it against
//! independent oracles. See `rskim_bench::scoreboard` for the design.
//!
//! # Usage
//!
//! ```text
//! scoreboard run   [--skim-bin P] [--corpus-dir D] [--data-dir D] [--only NAME] [--out DIR]
//! scoreboard check [same flags]           # run + gate against baseline.json / known_failures.toml
//! scoreboard bless --from report.json [--data-dir D] [--accept-regression "<reason>"]
//! scoreboard golden-gen --corpus NAME [--corpus-dir D] [--data-dir D]   # TOML proposal on stdout
//! ```
//!
//! # Exit codes
//!
//! - `0` — pass (`run` always exits 0 unless a harness error occurs;
//!   `bless` exits 0 when it wrote the baseline).
//! - `1` — gate failure (`check`), or `bless` refused.
//! - `2` — harness error: clone verification, golden integrity, an invalid
//!   data file, a skim crash / timeout / unparsable output, temporal data
//!   skim reports unusable for entries that rank by it, a corpus changed by
//!   the run. A harness error is never reported as a regression.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::{Parser, Subcommand};

use rskim_bench::scoreboard::baseline::{Baseline, BlessDecision, BlessInputs, bless};
use rskim_bench::scoreboard::corpus::{
    DEFAULT_CORPUS_DIR, GitCorpusSource, find_corpus, load_corpora, materialize_verified,
};
use rskim_bench::scoreboard::golden_gen;
use rskim_bench::scoreboard::pipeline::{self, DataDir, Inputs};
use rskim_bench::scoreboard::report::{GateStatus, Report, total, write_outputs};
use rskim_bench::scoreboard::runner::{SkimRunner, SkimSandbox};
use rskim_bench::scoreboard::universe::{GitIsolation, Universe};

const EXIT_PASS: u8 = 0;
const EXIT_GATE_FAIL: u8 = 1;
const EXIT_HARNESS_ERROR: u8 = 2;

const DEFAULT_SKIM_BIN: &str = "target/release/skim";
const DEFAULT_DATA_DIR: &str = "crates/rskim-bench/scoreboard";
const DEFAULT_OUT_DIR: &str = "target/scoreboard";

/// End-to-end retrieval-quality gate for `skim search`.
#[derive(Debug, Parser)]
#[command(name = "scoreboard", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run every corpus and write report.json + report.md (never gates).
    Run(EngineArgs),
    /// Run, then gate against baseline.json and known_failures.toml.
    Check(EngineArgs),
    /// Rewrite baseline.json from a report.json (e.g. the CI artifact).
    Bless(BlessArgs),
    /// Print candidate `[[ident]]` entries for one corpus (a proposal to review
    /// and freeze in `golden/<corpus>.toml`; never run in CI).
    GoldenGen(GoldenGenArgs),
}

#[derive(Debug, Parser)]
struct EngineArgs {
    /// The skim binary under test.
    #[arg(long, default_value = DEFAULT_SKIM_BIN)]
    skim_bin: PathBuf,

    /// Where the pinned corpus clones live (`<dir>/<corpus>`).
    #[arg(long, default_value = DEFAULT_CORPUS_DIR)]
    corpus_dir: PathBuf,

    /// corpora.toml, golden/, known_failures.toml and baseline.json.
    #[arg(long, default_value = DEFAULT_DATA_DIR)]
    data_dir: PathBuf,

    /// Run one corpus only (a partial run; `bless` refuses its report).
    #[arg(long, value_name = "CORPUS")]
    only: Option<String>,

    /// Output directory for report.json and report.md.
    #[arg(long, default_value = DEFAULT_OUT_DIR)]
    out: PathBuf,
}

#[derive(Debug, Parser)]
struct BlessArgs {
    /// The report.json to bless.
    #[arg(long)]
    from: PathBuf,

    /// The data dir whose baseline.json is rewritten.
    #[arg(long, default_value = DEFAULT_DATA_DIR)]
    data_dir: PathBuf,

    /// Accept RATCHET regressions and HARD downgrades (pass -> xfail, a
    /// blessed check that no longer runs), recording this reason in the
    /// baseline.
    #[arg(long, value_name = "REASON")]
    accept_regression: Option<String>,
}

#[derive(Debug, Parser)]
struct GoldenGenArgs {
    /// Corpus to generate candidates for.
    #[arg(long)]
    corpus: String,

    /// Where the pinned corpus clones live.
    #[arg(long, default_value = DEFAULT_CORPUS_DIR)]
    corpus_dir: PathBuf,

    /// The data dir holding corpora.toml.
    #[arg(long, default_value = DEFAULT_DATA_DIR)]
    data_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Run,
    Check,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match &cli.command {
        Command::Run(args) => engine(args, Mode::Run),
        Command::Check(args) => engine(args, Mode::Check),
        Command::Bless(args) => bless_command(args),
        Command::GoldenGen(args) => golden_gen(args),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("scoreboard: harness error: {e:#}");
            ExitCode::from(EXIT_HARNESS_ERROR)
        }
    }
}

fn engine(args: &EngineArgs, mode: Mode) -> anyhow::Result<u8> {
    let skim_bin = std::fs::canonicalize(&args.skim_bin).with_context(|| {
        format!(
            "skim binary {} not found (build it with `cargo build --release -p rskim`, or pass --skim-bin)",
            args.skim_bin.display()
        )
    })?;
    let data = DataDir::new(&args.data_dir);
    let inputs = Inputs::load(&data, args.only.as_deref())?;

    // One sandbox HOME per run: skim's cache (and so every index) and the
    // agent config dirs live here, and the oracle's git calls share it.
    let sandbox = tempfile::Builder::new()
        .prefix("skim-scoreboard-")
        .tempdir()
        .context("creating the sandbox HOME")?;
    let runner = SkimRunner::new(skim_bin, SkimSandbox::new(sandbox.path()));
    let source = GitCorpusSource::new(&args.corpus_dir);

    let report = pipeline::run(&inputs, &source, &runner)?;
    write_outputs(&report, inputs.baseline.as_ref(), &args.out)?;
    print_summary(&report, mode, &args.out);

    Ok(match (mode, report.gate.status) {
        (Mode::Check, GateStatus::Fail) => EXIT_GATE_FAIL,
        (Mode::Check | Mode::Run, _) => EXIT_PASS,
    })
}

fn print_summary(report: &Report, mode: Mode, out: &std::path::Path) {
    for c in &report.corpora {
        let t = total(&c.checks);
        eprintln!(
            "[scoreboard] {}: HARD {} pass, {} xfail, {} fail, {} xpass; universe delta {}",
            c.name, t.pass, t.xfail, t.fail, t.xpass, c.universe.delta
        );
    }
    for f in &report.gate.failures {
        eprintln!("{}", f.line());
    }
    let verdict = match (report.gate.status, mode) {
        (GateStatus::Pass, _) => "PASS",
        (GateStatus::Fail, Mode::Check) => "FAIL",
        (GateStatus::Fail, Mode::Run) => "would FAIL (`run` does not gate)",
    };
    eprintln!(
        "scoreboard: gate {verdict} ({} failure(s)); wrote {} and report.md",
        report.gate.failures.len(),
        out.join("report.json").display()
    );
}

fn bless_command(args: &BlessArgs) -> anyhow::Result<u8> {
    let raw = std::fs::read_to_string(&args.from)
        .with_context(|| format!("reading {}", args.from.display()))?;
    let report = Report::parse(&raw).with_context(|| format!("in {}", args.from.display()))?;
    let data = DataDir::new(&args.data_dir);
    let names: Vec<&str> = report.corpora.iter().map(|c| c.name.as_str()).collect();
    let on_disk = pipeline::golden_hashes_on_disk(&data, &names)?;
    let path = data.baseline();
    let existing = Baseline::load(&path)?;

    match bless(&BlessInputs {
        report: &report,
        golden_on_disk: &on_disk,
        existing: existing.as_ref(),
        accept_regression: args.accept_regression.as_deref(),
    }) {
        BlessDecision::Blessed { baseline, notes } => {
            baseline.write(&path)?;
            for note in notes {
                eprintln!("scoreboard: {note}");
            }
            eprintln!("scoreboard: blessed {}", path.display());
            Ok(EXIT_PASS)
        }
        BlessDecision::Refused(reasons) => {
            eprintln!("scoreboard: bless refused:");
            for reason in reasons {
                eprintln!("  - {reason}");
            }
            Ok(EXIT_GATE_FAIL)
        }
    }
}

fn golden_gen(args: &GoldenGenArgs) -> anyhow::Result<u8> {
    let data = DataDir::new(&args.data_dir);
    let specs = load_corpora(&data.corpora())?;
    let spec = find_corpus(&specs, &args.corpus, "--corpus", &data.corpora())?;
    let root = materialize_verified(&GitCorpusSource::new(&args.corpus_dir), spec)?;

    // The oracle's git calls run under an empty HOME, as in `run`.
    let home = tempfile::Builder::new()
        .prefix("skim-scoreboard-golden-gen-")
        .tempdir()
        .context("creating the isolated HOME")?;
    let universe = Universe::compute(&root, &GitIsolation::new(home.path()))?;
    let candidates = golden_gen::generate(&spec.name, &universe, golden_gen::GENERATED_PER_CORPUS)?;

    println!(
        "# golden-gen proposal for corpus {} at {} ({} of {} requested; review, then freeze in golden/{}.toml)",
        spec.name,
        spec.commit,
        candidates.len(),
        golden_gen::GENERATED_PER_CORPUS,
        spec.name
    );
    print!("{}", golden_gen::render_toml(&spec.name, &candidates));
    eprintln!(
        "scoreboard: golden-gen {}: {} candidate(s) over {} universe file(s)",
        spec.name,
        candidates.len(),
        universe.len()
    );
    Ok(EXIT_PASS)
}
