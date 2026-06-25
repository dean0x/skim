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
//! - `trigram-run`: scan a local directory, extract trigrams, compute IDF, write JSON
//! - `trigram-codegen`: read trigram_weights.json, generate weights.rs for rskim-search

use rskim_research::{clone, codegen, config, extract, idf, trigram_codegen, types, validate};

use serde::Serialize;

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

    /// Scan a local directory tree, extract character trigrams, compute IDF weights, and write JSON.
    ///
    /// Unlike `run` (which clones external repos), `trigram-run` works on an already-present
    /// source directory — suitable for generating weights from the workspace itself.
    TrigramRun {
        /// Root directory to scan for source files (defaults to cwd).
        #[arg(long)]
        source_dir: Option<PathBuf>,

        /// Minimum IDF threshold — trigrams below this are excluded from the table.
        #[arg(long, default_value = "1.5")]
        threshold: f32,

        /// Output path for trigram_weights.json.
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Read trigram_weights.json and generate weights.rs for rskim-search.
    TrigramCodegen {
        /// Path to trigram_weights.json (defaults to crates/rskim-search/data/trigram_weights.json).
        #[arg(long)]
        json_path: Option<PathBuf>,

        /// Override workspace root detection (auto-detected if omitted).
        #[arg(long)]
        workspace_root: Option<PathBuf>,
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

        Commands::TrigramRun {
            source_dir,
            threshold,
            output,
        } => cmd_trigram_run(source_dir, threshold, output),

        Commands::TrigramCodegen {
            json_path,
            workspace_root,
        } => cmd_trigram_codegen(json_path, workspace_root),

        Commands::AstRun {
            corpus_dir,
            threshold,
            corpus_config,
            output,
            trigrams,
        } => ast_cmd::cmd_ast_run(corpus_dir, threshold, &corpus_config, output, trigrams),

        Commands::AstCodegen {
            json_path,
            workspace_root,
        } => ast_cmd::cmd_ast_codegen(json_path, workspace_root),

        Commands::AstValidate { json_path } => ast_cmd::cmd_ast_validate(json_path),
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

/// Serialize a JSON-serializable value to the given output path (creating parent
/// directories if needed). `label` is used only in the error/log messages so the
/// caller can distinguish between table types in stderr output.
fn write_json_table<T: Serialize>(
    table: &T,
    output_path: PathBuf,
    label: &str,
) -> anyhow::Result<()> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating output directory {}", parent.display()))?;
    }

    let json =
        serde_json::to_string_pretty(table).with_context(|| format!("serializing {label}"))?;
    std::fs::write(&output_path, json)
        .with_context(|| format!("writing {}", output_path.display()))?;

    eprintln!("Written: {}", output_path.display());
    Ok(())
}

/// Serialize the weight table to JSON and write it to the output path.
fn write_weight_table(table: &types::WeightTable, output: Option<PathBuf>) -> anyhow::Result<()> {
    let output_path = output.unwrap_or_else(default_json_path);
    write_json_table(table, output_path, "weight table")
}

fn cmd_codegen(json_path: Option<PathBuf>, workspace_root: Option<PathBuf>) -> anyhow::Result<()> {
    let workspace_root = match workspace_root {
        Some(p) => p,
        None => codegen::find_workspace_root().context("auto-detecting workspace root")?,
    };

    let json_path = json_path
        .unwrap_or_else(|| workspace_root.join("crates/rskim-search/data/bigram_weights.json"));

    // #355 (Finding 8): After the bigram→trigram migration, rskim-search consumes only
    // TRIGRAM_WEIGHTS / lookup_weight / trigram_weight from weights.rs (generated by
    // `trigram-codegen`).  Writing BIGRAM_WEIGHTS / bigram_weight to the same
    // `weights.rs` would silently overwrite the trigram table with incompatible symbols
    // that no longer compile against the rest of the workspace.
    //
    // Fix: redirect the legacy bigram codegen output to `bigram_weights_legacy.rs`, a
    // clearly distinct artifact that cannot clobber the live trigram weights.rs.
    // The `run`/`validate` subcommands are preserved for historical reference; only
    // the output file is renamed so the two generators are disjoint.
    //
    // To regenerate the LEXICAL (trigram) weights table, use `trigram-codegen` instead.
    let output_path = workspace_root.join("crates/rskim-search/src/bigram_weights_legacy.rs");

    eprintln!(
        "Reading {} -> generating {} (legacy bigram table; for trigram weights use trigram-codegen)",
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
// Trigram subcommand handlers
// ─────────────────────────────────────────────────────────────────────────────

/// Extensions accepted by the trigram corpus walk.
///
/// Wider than the lexical corpus (`clone::TARGET_EXTENSIONS`) because the
/// trigram weight table benefits from broader coverage (docs, shell, data files).
/// build/vendor directories (target/, node_modules/, vendor/) are always skipped.
/// The size cap (`TRIGRAM_MAX_FILE_BYTES`) is 1 MB — larger than `clone.rs`
/// `MAX_FILE_SIZE` (100 KB) to retain larger data files that are representative
/// of real-world corpora but would dominate IDF if they were much larger.
const TRIGRAM_EXTENSIONS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "c", "cpp", "h",
    "hpp", "cs", "rb", "kt", "swift", "sql", "md",
];
const TRIGRAM_MAX_FILE_BYTES: u64 = 1_048_576; // 1 MB

/// Walk `root` for trigram corpus collection, skipping build dirs and large files.
///
/// Isolated from `clone::walk_and_load` because the trigram walk uses a wider
/// extension list (see `TRIGRAM_EXTENSIONS`) and a larger file-size cap than the
/// lexical corpus default (`clone::MAX_FILE_SIZE` = 100 KB).  Extraction of this
/// helper from the previous inline loop brings `cmd_trigram_run` to <30 lines.
fn walk_trigram_corpus(root: &std::path::Path) -> anyhow::Result<Vec<types::SourceFile>> {
    let ext_set: std::collections::HashSet<&str> = TRIGRAM_EXTENSIONS.iter().copied().collect();
    let mut files: Vec<types::SourceFile> = Vec::new();

    for result in ignore::WalkBuilder::new(root).follow_links(false).hidden(false).build() {
        let entry = match result {
            Ok(e) => e,
            Err(e) => { eprintln!("Warning: walk error: {e}"); continue; }
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) { continue; }
        let path = entry.path();
        // Skip common build/vendor directories.
        if path.components().any(|c| {
            matches!(c.as_os_str().to_str(), Some("target") | Some("node_modules") | Some("vendor"))
        }) { continue; }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !ext_set.contains(ext) { continue; }
        if path.metadata().map(|m| m.len() > TRIGRAM_MAX_FILE_BYTES).unwrap_or(false) { continue; }
        match std::fs::read_to_string(path) {
            Ok(content) => {
                let language = path.extension().and_then(|e| e.to_str())
                    .and_then(rskim_core::Language::from_extension)
                    .unwrap_or(rskim_core::Language::Rust);
                files.push(types::SourceFile { path: path.to_path_buf(), language, content });
            }
            Err(e) => eprintln!("Warning: could not read {}: {e}", path.display()),
        }
    }
    Ok(files)
}

/// Walk `source_dir` recursively and collect all source files rskim-search
/// can index, then extract trigrams and compute IDF weights.
///
/// Unlike `run` (which clones external repos over the network), this command
/// works on a directory that already exists — suitable for generating a
/// corpus-derived weight table from the workspace's own source files.
fn cmd_trigram_run(
    source_dir: Option<PathBuf>,
    threshold: f32,
    output: Option<PathBuf>,
) -> anyhow::Result<()> {
    let root = source_dir
        .map(Ok)
        .unwrap_or_else(|| std::env::current_dir().with_context(|| "getting current directory"))?;

    eprintln!("Scanning source files under {} ...", root.display());

    let files = walk_trigram_corpus(&root)?;

    eprintln!("Loaded {} source files. Extracting trigrams...", files.len());

    if files.is_empty() {
        anyhow::bail!("No source files found under {}", root.display());
    }

    let (df_map, corpus_stats) = extract::extract_trigrams_from_corpus(&files);
    let total_docs = corpus_stats.total_files;

    eprintln!(
        "Corpus: {} unique files, {} unique trigrams. Computing IDF...",
        total_docs, df_map.len()
    );

    let weights = idf::compute_trigram_weight_table(&df_map, total_docs, threshold);
    eprintln!("Trigram weight table: {} entries (threshold={threshold}).", weights.len());

    let table = types::TrigramWeightTable {
        version: 1,
        generated_at: chrono_now(),
        corpus_stats,
        weights,
    };

    let output_path = output.unwrap_or_else(|| {
        trigram_codegen::default_trigram_weights_json_path()
            .unwrap_or_else(|_| PathBuf::from("trigram_weights.json"))
    });

    write_json_table(&table, output_path, "trigram weight table")
}

fn cmd_trigram_codegen(
    json_path: Option<PathBuf>,
    workspace_root: Option<PathBuf>,
) -> anyhow::Result<()> {
    let workspace_root = match workspace_root {
        Some(p) => p,
        None => codegen::find_workspace_root().context("auto-detecting workspace root")?,
    };

    let json_path = json_path.unwrap_or_else(|| {
        workspace_root.join("crates/rskim-search/data/trigram_weights.json")
    });

    let output_path = workspace_root.join("crates/rskim-search/src/weights.rs");

    eprintln!(
        "Reading {} -> generating {}",
        json_path.display(),
        output_path.display()
    );

    trigram_codegen::generate_weights_rs(&json_path, &output_path)?;

    eprintln!("Generated: {}", output_path.display());
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// AST subcommand handlers (separate module)
// ─────────────────────────────────────────────────────────────────────────────

mod ast_cmd;

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
