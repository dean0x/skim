//! rskim-research — empirical bigram IDF weight table generator.
//!
//! A developer-only binary (publish = false) that analyzes source code corpora
//! to derive character bigram IDF weights for the rskim-search sparse index.
//!
//! # Subcommands
//!
//! - `run`: clone corpus repos, extract bigrams, compute IDF, write JSON
//! - `codegen`: read JSON weight table, generate weights.rs for rskim-search
//! - `validate`: read JSON weight table, run border-weight validation report

use rskim_research::{clone, codegen, config, extract, idf, types, validate};

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use clone::FileSource;
use indicatif::{ProgressBar, ProgressStyle};

#[derive(Parser, Debug)]
#[command(
    name = "rskim-research",
    about = "Empirical bigram IDF weight table generator for rskim-search",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Clone corpus repos, extract bigrams, compute IDF weights, and write JSON.
    Run {
        /// Directory to clone repos into (defaults to a temporary directory).
        #[arg(long)]
        corpus_dir: Option<PathBuf>,

        /// Minimum IDF threshold — bigrams below this are excluded from the table.
        #[arg(long, default_value = "1.5")]
        threshold: f32,

        /// Path to corpus.toml configuration file.
        #[arg(long, default_value = "corpus.toml")]
        corpus_config: PathBuf,

        /// Output path for bigram_weights.json.
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Read bigram_weights.json and generate weights.rs for rskim-search.
    Codegen {
        /// Path to bigram_weights.json (defaults to crates/rskim-search/data/bigram_weights.json).
        #[arg(long)]
        json_path: Option<PathBuf>,

        /// Override workspace root detection (auto-detected if omitted).
        #[arg(long)]
        workspace_root: Option<PathBuf>,
    },

    /// Read bigram_weights.json and run border-weight validation report.
    Validate {
        /// Path to bigram_weights.json (defaults to crates/rskim-search/data/bigram_weights.json).
        #[arg(long)]
        json_path: Option<PathBuf>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            corpus_dir,
            threshold,
            corpus_config,
            output,
        } => cmd_run(corpus_dir, threshold, &corpus_config, output),

        Commands::Codegen {
            json_path,
            workspace_root,
        } => cmd_codegen(json_path, workspace_root),

        Commands::Validate { json_path } => cmd_validate(json_path),
    }
}

fn cmd_run(
    corpus_dir: Option<PathBuf>,
    threshold: f32,
    corpus_config: &std::path::Path,
    output: Option<PathBuf>,
) -> anyhow::Result<()> {
    use rayon::prelude::*;

    let config = config::load_corpus_config(corpus_config)
        .with_context(|| format!("loading corpus config from {}", corpus_config.display()))?;

    // Use a provided directory or create a temporary one.
    let _temp_dir_guard;
    let corpus_dir = match corpus_dir {
        Some(p) => p,
        None => {
            let td = tempfile::tempdir().context("creating temporary corpus directory")?;
            let path = td.path().to_path_buf();
            _temp_dir_guard = td;
            path
        }
    };

    std::fs::create_dir_all(&corpus_dir)
        .with_context(|| format!("creating corpus dir {}", corpus_dir.display()))?;

    eprintln!(
        "Cloning {} repos into {} ...",
        config.repos.len(),
        corpus_dir.display()
    );

    let source = clone::GitCloneSource {
        corpus_dir: corpus_dir.clone(),
    };

    let progress = ProgressBar::new(config.repos.len() as u64);
    if let Ok(style) =
        ProgressStyle::with_template("[{elapsed_precise}] [{bar:40}] {pos}/{len} {msg}")
    {
        progress.set_style(style);
    }

    // Collect all source files from each repo in parallel.
    let all_files: Vec<types::SourceFile> = config
        .repos
        .par_iter()
        .flat_map(|repo| {
            progress.set_message(repo.url.clone());
            let result = source.fetch_files(repo);
            progress.inc(1);
            match result {
                Ok(files) => files,
                Err(e) => {
                    eprintln!("Warning: failed to fetch {}: {e:#}", repo.url);
                    vec![]
                }
            }
        })
        .collect();

    progress.finish_with_message("done");

    eprintln!(
        "Loaded {} source files. Extracting bigrams...",
        all_files.len()
    );

    let (df_map, corpus_stats) = extract::extract_bigrams_from_corpus(&all_files);
    let total_docs = corpus_stats.total_files;

    eprintln!(
        "Corpus: {} unique files, {} unique bigrams. Computing IDF...",
        total_docs,
        df_map.len()
    );

    let weights = idf::compute_weight_table(&df_map, total_docs, threshold);

    eprintln!(
        "Weight table: {} entries (threshold={threshold}). Running validation...",
        weights.len()
    );

    let weight_pairs: Vec<(u16, f32)> = weights.iter().map(|w| (w.bigram, w.idf)).collect();
    let test_queries = validate::TEST_QUERIES;
    let validation = validate::run_validation(&weight_pairs, test_queries);

    eprintln!(
        "Validation — uniform: {:.4}, border-weighted: {:.4}, improvement: {:.1}%",
        validation.uniform_selectivity,
        validation.border_weighted_selectivity,
        validation.improvement_pct
    );

    // Determine output path.
    let output_path = output.unwrap_or_else(|| {
        // Default: crates/rskim-search/data/bigram_weights.json relative to workspace root.
        codegen::find_workspace_root()
            .map(|root| root.join("crates/rskim-search/data/bigram_weights.json"))
            .unwrap_or_else(|_| PathBuf::from("bigram_weights.json"))
    });

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating output directory {}", parent.display()))?;
    }

    let table = types::WeightTable {
        version: 1,
        generated_at: chrono_now(),
        corpus_stats,
        weights,
    };

    let json = serde_json::to_string_pretty(&table).context("serializing weight table")?;
    std::fs::write(&output_path, json)
        .with_context(|| format!("writing {}", output_path.display()))?;

    eprintln!("Written: {}", output_path.display());
    Ok(())
}

fn cmd_codegen(json_path: Option<PathBuf>, workspace_root: Option<PathBuf>) -> anyhow::Result<()> {
    let workspace_root = match workspace_root {
        Some(p) => p,
        None => codegen::find_workspace_root().context("auto-detecting workspace root")?,
    };

    let json_path = json_path
        .unwrap_or_else(|| workspace_root.join("crates/rskim-search/data/bigram_weights.json"));

    let output_path = workspace_root.join("crates/rskim-search/src/weights.rs");

    eprintln!(
        "Reading {} -> generating {}",
        json_path.display(),
        output_path.display()
    );

    codegen::generate_weights_rs(&json_path, &output_path)?;

    eprintln!("Generated: {}", output_path.display());
    Ok(())
}

fn cmd_validate(json_path: Option<PathBuf>) -> anyhow::Result<()> {
    let json_path = json_path.unwrap_or_else(|| {
        codegen::find_workspace_root()
            .map(|root| root.join("crates/rskim-search/data/bigram_weights.json"))
            .unwrap_or_else(|_| PathBuf::from("bigram_weights.json"))
    });

    let raw = std::fs::read_to_string(&json_path)
        .with_context(|| format!("reading {}", json_path.display()))?;

    let table: types::WeightTable =
        serde_json::from_str(&raw).context("parsing bigram_weights.json")?;

    let weight_pairs: Vec<(u16, f32)> = table.weights.iter().map(|w| (w.bigram, w.idf)).collect();

    let validation = validate::run_validation(&weight_pairs, validate::TEST_QUERIES);

    println!("=== Validation Report ===");
    println!("Weight table version: {}", table.version);
    println!("Generated at:         {}", table.generated_at);
    println!("Total entries:        {}", table.weights.len());
    println!("Total corpus files:   {}", table.corpus_stats.total_files);
    println!();
    println!(
        "Uniform selectivity:        {:.6}",
        validation.uniform_selectivity
    );
    println!(
        "Border-weighted selectivity:{:.6}",
        validation.border_weighted_selectivity
    );
    println!(
        "Improvement:                {:.2}%",
        validation.improvement_pct
    );

    Ok(())
}

/// Returns a UTC timestamp string for the `generated_at` field.
fn chrono_now() -> String {
    // Use std::time since we don't depend on chrono crate.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}
