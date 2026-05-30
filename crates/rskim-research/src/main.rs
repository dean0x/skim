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
//! - `ast-run`: clone corpus repos, extract AST n-grams, compute IDF, write JSON
//! - `ast-codegen`: read ast_weights.json, generate ast_weights.rs for rskim-search
//! - `ast-validate`: read ast_weights.json, run AST validation report

use rskim_research::{
    ast_codegen, ast_extract, ast_idf, ast_types, ast_validate, clone, codegen, config, extract,
    idf, types, validate,
};

use std::collections::HashMap;
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

    /// Clone corpus repos, extract AST bigrams/trigrams, compute IDF weights, and write JSON.
    AstRun {
        /// Directory to clone repos into (defaults to a temporary directory).
        #[arg(long)]
        corpus_dir: Option<PathBuf>,

        /// Minimum IDF threshold — bigrams/trigrams below this are excluded.
        #[arg(long, default_value = "1.5")]
        threshold: f32,

        /// Path to ast-corpus.toml configuration file.
        #[arg(long, default_value = "ast-corpus.toml")]
        corpus_config: PathBuf,

        /// Output path for ast_weights.json.
        #[arg(long)]
        output: Option<PathBuf>,

        /// Collect AST trigrams in addition to bigrams.
        #[arg(long, default_value = "true")]
        trigrams: bool,
    },

    /// Read ast_weights.json and generate ast_weights.rs for rskim-search.
    AstCodegen {
        /// Path to ast_weights.json (defaults to crates/rskim-search/data/ast_weights.json).
        #[arg(long)]
        json_path: Option<PathBuf>,

        /// Override workspace root detection (auto-detected if omitted).
        #[arg(long)]
        workspace_root: Option<PathBuf>,
    },

    /// Read ast_weights.json and run AST validation report.
    AstValidate {
        /// Path to ast_weights.json (defaults to crates/rskim-search/data/ast_weights.json).
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

        Commands::AstRun {
            corpus_dir,
            threshold,
            corpus_config,
            output,
            trigrams,
        } => cmd_ast_run(corpus_dir, threshold, &corpus_config, output, trigrams),

        Commands::AstCodegen {
            json_path,
            workspace_root,
        } => cmd_ast_codegen(json_path, workspace_root),

        Commands::AstValidate { json_path } => cmd_ast_validate(json_path),
    }
}

fn cmd_run(
    corpus_dir: Option<PathBuf>,
    threshold: f32,
    corpus_config: &std::path::Path,
    output: Option<PathBuf>,
) -> anyhow::Result<()> {
    let config = config::load_corpus_config(corpus_config)
        .with_context(|| format!("loading corpus config from {}", corpus_config.display()))?;

    let (corpus_dir, _temp_dir_guard) = resolve_corpus_dir(corpus_dir)?;

    eprintln!(
        "Cloning {} repos into {} ...",
        config.repos.len(),
        corpus_dir.display()
    );

    let all_files = fetch_all_files(&config, &corpus_dir)?;

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

    log_validation_summary(&weights);

    let table = types::WeightTable {
        version: 1,
        generated_at: chrono_now(),
        corpus_stats,
        weights,
    };

    write_weight_table(&table, output)?;

    Ok(())
}

/// Resolve the corpus directory, creating a temporary one if none was provided.
///
/// Returns `(path, guard)` — the guard keeps the `TempDir` alive for the caller's
/// scope; it is `None` when the caller supplied an explicit path.
fn resolve_corpus_dir(
    corpus_dir: Option<PathBuf>,
) -> anyhow::Result<(PathBuf, Option<tempfile::TempDir>)> {
    let (path, guard) = match corpus_dir {
        Some(p) => (p, None),
        None => {
            let td = tempfile::tempdir().context("creating temporary corpus directory")?;
            let path = td.path().to_path_buf();
            (path, Some(td))
        }
    };
    std::fs::create_dir_all(&path)
        .with_context(|| format!("creating corpus dir {}", path.display()))?;
    Ok((path, guard))
}

/// Clone/fetch all source files from each configured repo in parallel using `source`.
fn fetch_files_parallel(
    config: &config::CorpusConfig,
    source: &impl FileSource,
) -> anyhow::Result<Vec<types::SourceFile>> {
    use rayon::prelude::*;

    let progress = ProgressBar::new(config.repos.len() as u64);
    if let Ok(style) =
        ProgressStyle::with_template("[{elapsed_precise}] [{bar:40}] {pos}/{len} {msg}")
    {
        progress.set_style(style);
    }

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
    Ok(all_files)
}

/// Clone/fetch all source files from each configured repo in parallel.
fn fetch_all_files(
    config: &config::CorpusConfig,
    corpus_dir: &std::path::Path,
) -> anyhow::Result<Vec<types::SourceFile>> {
    let source = clone::GitCloneSource {
        corpus_dir: corpus_dir.to_path_buf(),
    };
    fetch_files_parallel(config, &source)
}

/// Print validation summary to stderr.
fn log_validation_summary(weights: &[types::BigramWeight]) {
    let weight_pairs: Vec<(u16, f32)> = weights.iter().map(|w| (w.bigram, w.idf)).collect();
    let validation = validate::run_validation(&weight_pairs, validate::TEST_QUERIES);
    eprintln!(
        "Validation — uniform: {:.4}, border-weighted: {:.4}, improvement: {:.1}%",
        validation.uniform_selectivity,
        validation.border_weighted_selectivity,
        validation.improvement_pct
    );
}

/// Default path for `bigram_weights.json`: `<workspace>/crates/rskim-search/data/bigram_weights.json`,
/// falling back to `bigram_weights.json` in the current directory if the workspace root cannot be found.
fn default_json_path() -> PathBuf {
    codegen::find_workspace_root()
        .map(|root| root.join("crates/rskim-search/data/bigram_weights.json"))
        .unwrap_or_else(|_| PathBuf::from("bigram_weights.json"))
}

/// Serialize the weight table to JSON and write it to the output path.
fn write_weight_table(table: &types::WeightTable, output: Option<PathBuf>) -> anyhow::Result<()> {
    let output_path = output.unwrap_or_else(default_json_path);

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating output directory {}", parent.display()))?;
    }

    let json = serde_json::to_string_pretty(table).context("serializing weight table")?;
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
    let json_path = json_path.unwrap_or_else(default_json_path);

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

// ─────────────────────────────────────────────────────────────────────────────
// AST subcommand handlers
// ─────────────────────────────────────────────────────────────────────────────

fn cmd_ast_run(
    corpus_dir: Option<PathBuf>,
    threshold: f32,
    corpus_config: &std::path::Path,
    output: Option<PathBuf>,
    collect_trigrams: bool,
) -> anyhow::Result<()> {
    let ast_config = config::load_ast_corpus_config(corpus_config)
        .with_context(|| format!("loading AST corpus config from {}", corpus_config.display()))?;

    let (corpus_dir_path, _temp_dir_guard) = resolve_corpus_dir(corpus_dir)?;

    eprintln!(
        "Cloning {} repos into {} ...",
        ast_config.repos.len(),
        corpus_dir_path.display()
    );

    let all_files = fetch_all_ast_files(&ast_config, &corpus_dir_path)?;

    eprintln!(
        "Loaded {} source files. Extracting AST n-grams...",
        all_files.len()
    );

    let mut vocab = ast_types::NodeKindVocabulary::new();

    let (raw_bigram_df_maps, raw_trigram_df_maps, corpus_stats) =
        ast_extract::extract_ast_ngrams_from_corpus(&all_files, &mut vocab, collect_trigrams);

    // Stabilize the vocabulary (sort alphabetically, reassign IDs) and get the
    // old→new ID remap table. All bigram/trigram keys in the DF maps were encoded
    // with pre-stabilize IDs and must be re-keyed before IDF computation.
    let remap = vocab.stabilize();

    eprintln!(
        "Vocabulary: {} node kinds. Re-keying and computing IDF weights...",
        vocab.len()
    );

    let mut bigram_weights_map: HashMap<String, Vec<ast_types::AstBigramWeight>> = HashMap::new();
    let mut trigram_weights_map: HashMap<String, Vec<ast_types::AstTrigramWeight>> = HashMap::new();

    let total_docs = corpus_stats.total_files;

    for (lang, df_map) in &raw_bigram_df_maps {
        let rekeyed = ast_types::rekey_bigram_df_map(df_map, &remap);
        let weights = ast_idf::compute_ast_bigram_weights(&rekeyed, total_docs, threshold, &vocab);
        eprintln!(
            "  {lang}: {} bigrams (threshold={threshold})",
            weights.len()
        );
        bigram_weights_map.insert(lang.clone(), weights);
    }

    for (lang, df_map) in &raw_trigram_df_maps {
        let rekeyed = ast_types::rekey_trigram_df_map(df_map, &remap);
        let weights =
            ast_idf::compute_ast_trigram_weights(&rekeyed, total_docs, threshold, &vocab);
        eprintln!(
            "  {lang}: {} trigrams (threshold={threshold})",
            weights.len()
        );
        trigram_weights_map.insert(lang.clone(), weights);
    }

    let table = ast_types::AstWeightTable {
        version: 1,
        generated_at: chrono_now(),
        vocabulary: vocab.kinds().into_iter().map(str::to_string).collect(),
        corpus_stats,
        bigram_weights: bigram_weights_map,
        trigram_weights: trigram_weights_map,
    };

    write_ast_weight_table(&table, output)?;

    Ok(())
}

/// Clone/fetch all source files for the AST corpus using the AST extension walker.
fn fetch_all_ast_files(
    config: &config::CorpusConfig,
    corpus_dir: &std::path::Path,
) -> anyhow::Result<Vec<types::SourceFile>> {
    let source = clone::AstGitCloneSource {
        corpus_dir: corpus_dir.to_path_buf(),
    };
    fetch_files_parallel(config, &source)
}

/// Serialize the AST weight table to JSON and write to output path.
fn write_ast_weight_table(
    table: &ast_types::AstWeightTable,
    output: Option<PathBuf>,
) -> anyhow::Result<()> {
    let output_path = output.unwrap_or_else(|| {
        ast_codegen::default_ast_weights_json_path()
            .unwrap_or_else(|_| PathBuf::from("ast_weights.json"))
    });

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating output directory {}", parent.display()))?;
    }

    let json = serde_json::to_string_pretty(table).context("serializing AST weight table")?;
    std::fs::write(&output_path, json)
        .with_context(|| format!("writing {}", output_path.display()))?;

    eprintln!("Written: {}", output_path.display());
    Ok(())
}

fn cmd_ast_codegen(
    json_path: Option<PathBuf>,
    workspace_root: Option<PathBuf>,
) -> anyhow::Result<()> {
    let workspace_root = match workspace_root {
        Some(p) => p,
        None => codegen::find_workspace_root().context("auto-detecting workspace root")?,
    };

    let json_path = json_path
        .unwrap_or_else(|| workspace_root.join("crates/rskim-search/data/ast_weights.json"));

    let output_path = workspace_root.join("crates/rskim-search/src/ast_weights.rs");

    eprintln!(
        "Reading {} -> generating {}",
        json_path.display(),
        output_path.display()
    );

    ast_codegen::generate_ast_weights_rs(&json_path, &output_path)?;

    eprintln!("Generated: {}", output_path.display());
    Ok(())
}

fn cmd_ast_validate(json_path: Option<PathBuf>) -> anyhow::Result<()> {
    let json_path = json_path.unwrap_or_else(|| {
        ast_codegen::default_ast_weights_json_path()
            .unwrap_or_else(|_| PathBuf::from("ast_weights.json"))
    });

    let raw = std::fs::read_to_string(&json_path)
        .with_context(|| format!("reading {}", json_path.display()))?;

    let table: ast_types::AstWeightTable =
        serde_json::from_str(&raw).context("parsing ast_weights.json")?;

    let report = ast_validate::run_ast_validation(&table);
    ast_validate::print_ast_validation_report(&report);

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
