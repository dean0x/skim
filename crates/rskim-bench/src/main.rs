//! rskim-bench CLI — BM25F parameter tuning harness.
//!
//! Subcommands:
//! - `bench`   — run 4-config comparison on corpus repos
//! - `tune`    — coordinate descent over BM25F parameters
//! - `qrels`   — dump qrel judgments for a repo (debug)
//! - `report`  — render a saved bench result as markdown

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};

use rskim_bench::{
    configs,
    harness::{aggregate_results, run_on_files, BenchConfig},
    report,
    tuning::coordinate_descent,
    types::IndexedFile,
};
use rskim_research::{
    clone::{FileSource, GitCloneSource},
    config::load_corpus_config,
};
use rskim_search::{FileId, LayerBuilder};

// ============================================================================
// CLI definition
// ============================================================================

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
}

// ============================================================================
// Bench subcommand
// ============================================================================

#[derive(Debug, Parser)]
struct BenchArgs {
    /// Path to the corpus directory (repos are cloned here if absent).
    #[arg(long, default_value = ".bench-corpus")]
    corpus_dir: PathBuf,

    /// Path to corpus.toml (default: crates/rskim-research/corpus.toml).
    #[arg(long)]
    corpus_config: Option<PathBuf>,

    /// Output format: json or markdown.
    #[arg(long, default_value = "markdown")]
    output: String,

    /// Restrict to specific repo names (e.g. fd flask gin).
    #[arg(long)]
    repos: Vec<String>,
}

// ============================================================================
// Tune subcommand
// ============================================================================

#[derive(Debug, Parser)]
struct TuneArgs {
    /// Path to the corpus directory.
    #[arg(long, default_value = ".bench-corpus")]
    corpus_dir: PathBuf,

    /// Path to corpus.toml.
    #[arg(long)]
    corpus_config: Option<PathBuf>,

    /// Output format: json or markdown.
    #[arg(long, default_value = "markdown")]
    output: String,
}

// ============================================================================
// Qrels subcommand
// ============================================================================

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

// ============================================================================
// Main entry point
// ============================================================================

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Bench(args) => run_bench(args),
        Command::Tune(args) => run_tune(args),
        Command::Qrels(args) => run_qrels(args),
    }
}

// ============================================================================
// Command implementations
// ============================================================================

fn default_corpus_config() -> PathBuf {
    // Walk up from executable location to find corpus.toml
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.join("rskim-research").join("corpus.toml"))
        .unwrap_or_else(|| PathBuf::from("corpus.toml"))
}

fn run_bench(args: BenchArgs) -> anyhow::Result<()> {
    let config_path = args
        .corpus_config
        .unwrap_or_else(default_corpus_config);

    let corpus = load_corpus_config(&config_path)
        .with_context(|| format!("loading corpus config from {}", config_path.display()))?;

    std::fs::create_dir_all(&args.corpus_dir)
        .with_context(|| format!("creating corpus dir {}", args.corpus_dir.display()))?;

    let source = GitCloneSource {
        corpus_dir: args.corpus_dir.clone(),
    };

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

        let source_files = source
            .fetch_files(repo_entry)
            .with_context(|| format!("fetching files for {repo_name}"))?;

        // Sort files by path for determinism (AC24)
        let mut sorted_files = source_files;
        sorted_files.sort_by(|a, b| a.path.cmp(&b.path));

        let indexed: Vec<IndexedFile> = sorted_files
            .iter()
            .enumerate()
            .map(|(i, f)| IndexedFile {
                file_id: FileId(i as u32),
                path: f.path.clone(),
                language: f.language,
            })
            .collect();

        let mut contents: HashMap<FileId, String> = HashMap::new();
        for (i, f) in sorted_files.iter().enumerate() {
            contents.insert(FileId(i as u32), f.content.clone());
        }

        let index_dir = tempfile::tempdir().context("creating temp index dir")?;

        let mut result = run_on_files(&indexed, &contents, &bench_configs, index_dir.path())
            .with_context(|| format!("running benchmark on {repo_name}"))?;
        result.repo_url = repo_entry.url.clone();
        repo_results.push(result);
    }

    if repo_results.is_empty() {
        anyhow::bail!("No repos matched. Use --repos to filter, or check corpus config.");
    }

    let bench_result = aggregate_results(repo_results);

    match args.output.as_str() {
        "json" => {
            println!("{}", report::to_json(&bench_result, None)?);
        }
        _ => {
            print!("{}", report::to_markdown(&bench_result, None));
        }
    }

    Ok(())
}

fn run_tune(args: TuneArgs) -> anyhow::Result<()> {
    let config_path = args
        .corpus_config
        .unwrap_or_else(default_corpus_config);

    let corpus = load_corpus_config(&config_path)
        .with_context(|| format!("loading corpus config from {}", config_path.display()))?;

    std::fs::create_dir_all(&args.corpus_dir)
        .with_context(|| format!("creating corpus dir {}", args.corpus_dir.display()))?;

    let source = GitCloneSource {
        corpus_dir: args.corpus_dir.clone(),
    };

    // Load all files from all repos for tuning
    let mut all_indexed: Vec<IndexedFile> = Vec::new();
    let mut all_contents: HashMap<FileId, String> = HashMap::new();
    let mut file_id_counter = 0u32;

    for repo_entry in &corpus.repos {
        let repo_name = repo_entry.url.rsplit('/').next().unwrap_or("unknown");
        eprintln!("Loading repo: {repo_name}");

        let mut source_files = source
            .fetch_files(repo_entry)
            .with_context(|| format!("fetching files for {repo_name}"))?;

        source_files.sort_by(|a, b| a.path.cmp(&b.path));

        for f in source_files {
            let fid = FileId(file_id_counter);
            file_id_counter += 1;
            all_indexed.push(IndexedFile {
                file_id: fid,
                path: f.path.clone(),
                language: f.language,
            });
            all_contents.insert(fid, f.content);
        }
    }

    // Build qrel inputs
    let qrel_inputs: Vec<rskim_bench::qrel::QrelInput> = all_indexed
        .iter()
        .map(|f| rskim_bench::qrel::QrelInput {
            file_id: f.file_id,
            path: f.path.clone(),
            language: f.language,
            content: all_contents.get(&f.file_id).cloned().unwrap_or_default(),
        })
        .collect();

    let qrels = rskim_bench::qrel::generate_qrels(&qrel_inputs)
        .context("generating qrels for tuning")?;

    // Build index once for all tuning evaluations
    let index_dir = tempfile::tempdir().context("creating temp index dir")?;
    let mut builder =
        rskim_search::NgramIndexBuilder::new(index_dir.path().to_path_buf())
            .context("creating index builder")?;

    for f in &all_indexed {
        let content = all_contents.get(&f.file_id).map(|s| s.as_str()).unwrap_or("");
        builder
            .add_file(f.file_id, content, f.language)
            .context("indexing file")?;
    }
    let _base = builder.build().context("building index")?;

    // Filter to train split
    let train_qrels: Vec<_> = qrels
        .iter()
        .filter(|q| {
            rskim_bench::split::assign_split(&q.query) == rskim_bench::split::Split::Train
        })
        .cloned()
        .collect();

    eprintln!(
        "Tuning on {} train qrels",
        train_qrels.len()
    );

    let idx_path = index_dir.path().to_path_buf();

    let tuning_result = coordinate_descent(None, move |cfg: rskim_search::BM25FConfig| {
        let reader = match rskim_search::NgramIndexReader::open_with_config(&idx_path, cfg) {
            Ok(r) => r,
            Err(_) => return 0.0,
        };
        let metrics = rskim_bench::harness::evaluate_split(&reader, &train_qrels, "tuning")
            .unwrap_or_else(|_| rskim_bench::types::ConfigMetrics {
                config_name: "tuning".to_string(),
                mrr: 0.0,
                precision_at_5: 0.0,
                precision_at_10: 0.0,
                query_count: 0,
                found_at_rank_1: 0,
            });
        metrics.mrr
    });

    eprintln!(
        "Tuning complete. Best MRR: {:.4}, passes: {}",
        tuning_result.best_train_mrr,
        tuning_result.passes_needed
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
    let mut final_result =
        run_on_files(&all_indexed, &all_contents, &bench_configs, tune_index_dir.path())
            .context("running final evaluation with tuned config")?;
    final_result.repo_url = "aggregate".to_string();

    let bench_result = aggregate_results(vec![final_result]);

    match args.output.as_str() {
        "json" => {
            println!(
                "{}",
                report::to_json(&bench_result, Some(&tuning_result))?
            );
        }
        _ => {
            print!(
                "{}",
                report::to_markdown(&bench_result, Some(&tuning_result))
            );
        }
    }

    Ok(())
}

fn run_qrels(args: QrelsArgs) -> anyhow::Result<()> {
    let config_path = args
        .corpus_config
        .unwrap_or_else(default_corpus_config);

    let corpus = load_corpus_config(&config_path)
        .with_context(|| format!("loading corpus config from {}", config_path.display()))?;

    std::fs::create_dir_all(&args.corpus_dir)
        .with_context(|| format!("creating corpus dir {}", args.corpus_dir.display()))?;

    let source = GitCloneSource {
        corpus_dir: args.corpus_dir,
    };

    for repo_entry in &corpus.repos {
        let repo_name = repo_entry.url.rsplit('/').next().unwrap_or("unknown");

        if args.repo.as_ref().is_some_and(|f| !repo_name.contains(f.as_str())) {
            continue;
        }

        eprintln!("Generating qrels for: {repo_name}");

        let mut source_files = source
            .fetch_files(repo_entry)
            .with_context(|| format!("fetching files for {repo_name}"))?;

        source_files.sort_by(|a, b| a.path.cmp(&b.path));

        let qrel_inputs: Vec<rskim_bench::qrel::QrelInput> = source_files
            .iter()
            .enumerate()
            .map(|(i, f)| rskim_bench::qrel::QrelInput {
                file_id: FileId(i as u32),
                path: f.path.clone(),
                language: f.language,
                content: f.content.clone(),
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
