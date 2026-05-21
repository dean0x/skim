//! rskim-bench CLI — BM25F parameter tuning harness.
//!
//! Subcommands:
//! - `bench`   — run 4-config comparison on corpus repos
//! - `tune`    — coordinate descent over BM25F parameters
//! - `qrels`   — dump qrel judgments for a repo (debug)
//! - `report`  — render a saved bench result as markdown

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Context;
use clap::{Parser, Subcommand, ValueEnum};

use rskim_bench::{
    configs,
    harness::{BenchConfig, aggregate_results, run_on_files},
    report,
    tuning::coordinate_descent,
    types::IndexedFile,
};
use rskim_research::{
    clone::{FileSource, GitCloneSource},
    config::{CorpusConfig, load_corpus_config},
};
use rskim_search::{FileId, LayerBuilder};

/// Output format for bench, tune, and report subcommands.
#[derive(Debug, Clone, Default, ValueEnum)]
enum OutputFormat {
    /// Human-readable markdown table (default).
    #[default]
    Markdown,
    /// Machine-readable JSON.
    Json,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputFormat::Markdown => write!(f, "markdown"),
            OutputFormat::Json => write!(f, "json"),
        }
    }
}

/// BM25F parameter tuning benchmark harness.
#[derive(Debug, Parser)]
#[command(name = "rskim-bench", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run 4-config comparison on corpus repos.
    Bench(BenchArgs),
    /// Tune BM25F parameters via coordinate descent.
    Tune(TuneArgs),
    /// Print qrel judgments for a repo (debug/inspection).
    Qrels(QrelsArgs),
    /// Render a saved bench result JSON file as markdown or JSON.
    Report(ReportArgs),
}

#[derive(Debug, Parser)]
struct BenchArgs {
    /// Path to the corpus directory (repos are cloned here if absent).
    #[arg(long, default_value = ".bench-corpus")]
    corpus_dir: PathBuf,

    /// Path to corpus.toml (default: crates/rskim-research/corpus.toml).
    #[arg(long)]
    corpus_config: Option<PathBuf>,

    /// Output format.
    #[arg(long, default_value_t = OutputFormat::Markdown)]
    format: OutputFormat,

    /// Restrict to specific repo names (e.g. fd flask gin).
    #[arg(long)]
    repos: Vec<String>,
}

#[derive(Debug, Parser)]
struct TuneArgs {
    /// Path to the corpus directory.
    #[arg(long, default_value = ".bench-corpus")]
    corpus_dir: PathBuf,

    /// Path to corpus.toml.
    #[arg(long)]
    corpus_config: Option<PathBuf>,

    /// Output format.
    #[arg(long, default_value_t = OutputFormat::Markdown)]
    format: OutputFormat,
}

#[derive(Debug, Parser)]
struct QrelsArgs {
    /// Path to the corpus directory.
    #[arg(long, default_value = ".bench-corpus")]
    corpus_dir: PathBuf,

    /// Path to corpus.toml.
    #[arg(long)]
    corpus_config: Option<PathBuf>,

    /// Restrict to specific repo name.
    #[arg(long)]
    repo: Option<String>,
}

#[derive(Debug, Parser)]
struct ReportArgs {
    /// Path to a saved JSON bench result file (produced by `bench --format json`).
    #[arg(long)]
    input: PathBuf,

    /// Output format.
    #[arg(long, default_value_t = OutputFormat::Markdown)]
    format: OutputFormat,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Bench(args) => run_bench(args),
        Command::Tune(args) => run_tune(args),
        Command::Qrels(args) => run_qrels(args),
        Command::Report(args) => run_report(args),
    }
}

fn default_corpus_config() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.join("rskim-research").join("corpus.toml"))
        .unwrap_or_else(|| PathBuf::from("corpus.toml"))
}

/// Load corpus config and prepare the file source, creating the corpus dir if absent.
fn open_corpus(
    corpus_config: Option<PathBuf>,
    corpus_dir: &Path,
) -> anyhow::Result<(CorpusConfig, Box<dyn FileSource>)> {
    let config_path = corpus_config.unwrap_or_else(default_corpus_config);
    let corpus = load_corpus_config(&config_path)
        .with_context(|| format!("loading corpus config from {}", config_path.display()))?;
    std::fs::create_dir_all(corpus_dir)
        .with_context(|| format!("creating corpus dir {}", corpus_dir.display()))?;
    let source = GitCloneSource {
        corpus_dir: corpus_dir.to_path_buf(),
    };
    Ok((corpus, Box::new(source)))
}

/// Pre-loaded files from a single repository.
struct LoadedRepo {
    indexed: Vec<IndexedFile>,
    contents: HashMap<FileId, String>,
    repo_url: String,
}

/// Fetch, sort, and assign FileIds to all files from a repository.
///
/// IDs are assigned sequentially starting at `id_offset`.
/// Returns the loaded repo and the next available ID.
///
/// # Errors
///
/// Returns an error if fetching files fails or if the number of files
/// exceeds `u32::MAX`.
fn load_repo_files(
    source: &dyn FileSource,
    repo_entry: &rskim_research::config::RepoEntry,
    id_offset: u32,
) -> anyhow::Result<(LoadedRepo, u32)> {
    let repo_name = repo_entry.url.rsplit('/').next().unwrap_or("unknown");

    let mut source_files = source
        .fetch_files(repo_entry)
        .with_context(|| format!("fetching files for {repo_name}"))?;

    // Sort by path for determinism (AC24)
    source_files.sort_by(|a, b| a.path.cmp(&b.path));

    let mut indexed = Vec::with_capacity(source_files.len());
    let mut contents = HashMap::with_capacity(source_files.len());
    let mut next_id = id_offset;

    for f in source_files {
        let fid = FileId(next_id);
        next_id = next_id
            .checked_add(1)
            .context("FileId overflow: too many files")?;
        indexed.push(IndexedFile {
            file_id: fid,
            path: f.path,
            language: f.language,
        });
        contents.insert(fid, f.content);
    }

    Ok((
        LoadedRepo {
            indexed,
            contents,
            repo_url: repo_entry.url.clone(),
        },
        next_id,
    ))
}

fn run_bench(args: BenchArgs) -> anyhow::Result<()> {
    let (corpus, source) = open_corpus(args.corpus_config, &args.corpus_dir)?;

    let bench_configs = vec![
        BenchConfig {
            name: "uniform".to_string(),
            bm25f: configs::uniform(),
        },
        BenchConfig {
            name: "sourcegraph_style".to_string(),
            bm25f: configs::sourcegraph_style(),
        },
        BenchConfig {
            name: "default_8field".to_string(),
            bm25f: configs::default_8field(),
        },
    ];

    let mut repo_results = Vec::new();

    for repo_entry in &corpus.repos {
        let repo_name = repo_entry.url.rsplit('/').next().unwrap_or("unknown");

        // Apply repo filter
        if !args.repos.is_empty() && !args.repos.iter().any(|r| repo_name.contains(r.as_str())) {
            continue;
        }

        eprintln!("Benchmarking repo: {repo_name}");

        let (loaded, _) = load_repo_files(&*source, repo_entry, 0)?;
        let index_dir = tempfile::tempdir().context("creating temp index dir")?;

        let result = run_on_files(
            &loaded.indexed,
            &loaded.contents,
            &bench_configs,
            index_dir.path(),
            &loaded.repo_url,
        )
        .with_context(|| format!("running benchmark on {repo_name}"))?;
        repo_results.push(result);
    }

    if repo_results.is_empty() {
        anyhow::bail!("No repos matched. Use --repos to filter, or check corpus config.");
    }

    let bench_result = aggregate_results(repo_results)?;

    match args.format {
        OutputFormat::Json => {
            println!("{}", report::to_json(&bench_result, None)?);
        }
        OutputFormat::Markdown => {
            print!("{}", report::to_markdown(&bench_result, None));
        }
    }

    Ok(())
}

/// Build a search index from pre-loaded files into `index_dir`.
///
/// # Errors
///
/// Returns an error if the index builder cannot be created, a file cannot be
/// added, or the build step fails.
fn build_index(
    files: &[IndexedFile],
    contents: &HashMap<FileId, String>,
    index_dir: &Path,
) -> anyhow::Result<()> {
    let mut builder = rskim_search::NgramIndexBuilder::new(index_dir.to_path_buf())
        .context("creating index builder")?;

    for f in files {
        let content = contents.get(&f.file_id).map(|s| s.as_str()).unwrap_or("");
        builder
            .add_file(f.file_id, content, f.language)
            .with_context(|| format!("indexing file {:?}", f.path))?;
    }

    let _layer = builder.build().context("building index")?;
    Ok(())
}

/// Generate qrels for `files` and filter to the train split.
///
/// # Errors
///
/// Returns an error if qrel generation fails (too few symbols).
fn make_train_qrels(
    files: &[IndexedFile],
    contents: &HashMap<FileId, String>,
) -> anyhow::Result<Vec<rskim_bench::types::Qrel>> {
    let qrel_inputs: Vec<rskim_bench::qrel::QrelInput<'_>> = files
        .iter()
        .map(|f| rskim_bench::qrel::QrelInput {
            file_id: f.file_id,
            path: f.path.clone(),
            language: f.language,
            content: contents.get(&f.file_id).map(|s| s.as_str()).unwrap_or(""),
        })
        .collect();

    let all_qrels =
        rskim_bench::qrel::generate_qrels(&qrel_inputs).context("generating qrels")?;

    let train_qrels = all_qrels
        .into_iter()
        .filter(|q| rskim_bench::split::assign_split(&q.query) == rskim_bench::split::Split::Train)
        .collect();

    Ok(train_qrels)
}

fn run_tune(args: TuneArgs) -> anyhow::Result<()> {
    let (corpus, source) = open_corpus(args.corpus_config, &args.corpus_dir)?;

    // Load all files from all repos for tuning, assigning globally unique IDs
    let mut all_indexed: Vec<IndexedFile> = Vec::new();
    let mut all_contents: HashMap<FileId, String> = HashMap::new();
    let mut file_id_counter = 0u32;

    for repo_entry in &corpus.repos {
        let repo_name = repo_entry.url.rsplit('/').next().unwrap_or("unknown");
        eprintln!("Loading repo: {repo_name}");

        let (loaded, next_id) = load_repo_files(&*source, repo_entry, file_id_counter)?;
        file_id_counter = next_id;
        all_indexed.extend(loaded.indexed);
        all_contents.extend(loaded.contents);
    }

    // Build index and generate train qrels
    let index_dir = tempfile::tempdir().context("creating temp index dir")?;
    build_index(&all_indexed, &all_contents, index_dir.path())?;

    let train_qrels = make_train_qrels(&all_indexed, &all_contents)?;
    eprintln!("Tuning on {} train qrels", train_qrels.len());

    // Open reader once; BM25F config is overridden per-evaluation via
    // SearchQuery::bm25f_config (single-reader pattern, item 8).
    let reader = rskim_search::NgramIndexReader::open(index_dir.path())
        .context("opening index for tuning")?;

    // Error counter shared across closure invocations. coordinate_descent requires an f64
    // return value (0.0 signals a failed evaluation), so errors are visible on stderr rather
    // than propagated. We cap logging at the first 5 errors to avoid flooding output.
    let eval_error_count = Arc::new(AtomicU32::new(0));
    let counter = eval_error_count.clone();

    let tuning_result = coordinate_descent(None, move |cfg: rskim_search::BM25FConfig| {
        match rskim_bench::harness::evaluate_split(&reader, &train_qrels, "tuning", Some(cfg)) {
            Ok(metrics) => metrics.mrr,
            Err(e) => {
                let n = counter.fetch_add(1, Ordering::Relaxed);
                if n < 5 {
                    eprintln!("[tune] evaluate_split failed (error #{n}): {e:#}");
                }
                0.0
            }
        }
    });

    let total_errors = eval_error_count.load(Ordering::Relaxed);
    if total_errors > 0 {
        eprintln!(
            "[tune] {total_errors} evaluation(s) failed and returned 0.0 MRR — results may be unreliable."
        );
    }

    eprintln!(
        "Tuning complete. Best MRR: {:.4}, passes: {}",
        tuning_result.best_train_mrr, tuning_result.passes_needed
    );

    // Build final bench result with tuned config
    let tuned_cfg = rskim_bench::tuning::result_to_config(&tuning_result)
        .context("converting tuning result to config")?;

    let bench_configs = vec![
        BenchConfig {
            name: "default_8field".to_string(),
            bm25f: configs::default_8field(),
        },
        BenchConfig {
            name: "tuned".to_string(),
            bm25f: tuned_cfg,
        },
    ];

    let tune_index_dir = tempfile::tempdir().context("temp dir for final evaluation")?;
    let final_result = run_on_files(
        &all_indexed,
        &all_contents,
        &bench_configs,
        tune_index_dir.path(),
        "aggregate",
    )
    .context("running final evaluation with tuned config")?;

    let bench_result = aggregate_results(vec![final_result])?;

    match args.format {
        OutputFormat::Json => {
            println!("{}", report::to_json(&bench_result, Some(&tuning_result))?);
        }
        OutputFormat::Markdown => {
            print!(
                "{}",
                report::to_markdown(&bench_result, Some(&tuning_result))
            );
        }
    }

    Ok(())
}

fn run_report(args: ReportArgs) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(&args.input)
        .with_context(|| format!("reading bench result from {}", args.input.display()))?;
    let bench_result: rskim_bench::types::BenchResult = serde_json::from_str(&raw)
        .with_context(|| format!("deserialising bench result from {}", args.input.display()))?;

    match args.format {
        OutputFormat::Json => {
            println!("{}", report::to_json(&bench_result, None)?);
        }
        OutputFormat::Markdown => {
            print!("{}", report::to_markdown(&bench_result, None));
        }
    }

    Ok(())
}

fn run_qrels(args: QrelsArgs) -> anyhow::Result<()> {
    let (corpus, source) = open_corpus(args.corpus_config, &args.corpus_dir)?;

    for repo_entry in &corpus.repos {
        let repo_name = repo_entry.url.rsplit('/').next().unwrap_or("unknown");

        if args
            .repo
            .as_ref()
            .is_some_and(|f| !repo_name.contains(f.as_str()))
        {
            continue;
        }

        eprintln!("Generating qrels for: {repo_name}");

        let (loaded, _) = load_repo_files(&*source, repo_entry, 0)?;

        let qrel_inputs: Vec<rskim_bench::qrel::QrelInput<'_>> = loaded
            .indexed
            .iter()
            .map(|f| rskim_bench::qrel::QrelInput {
                file_id: f.file_id,
                path: f.path.clone(),
                language: f.language,
                content: loaded.contents.get(&f.file_id).map(|s| s.as_str()).unwrap_or(""),
            })
            .collect();

        let qrels = rskim_bench::qrel::generate_qrels(&qrel_inputs)
            .with_context(|| format!("generating qrels for {repo_name}"))?;

        println!(
            "{}",
            serde_json::to_string_pretty(&qrels).context("serialising qrels")?
        );
    }

    Ok(())
}
