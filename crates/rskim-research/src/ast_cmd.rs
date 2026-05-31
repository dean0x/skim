//! AST subcommand handlers: `ast-run`, `ast-codegen`, `ast-validate`.
//!
//! Extracted from `main.rs` to keep it below the 500-line threshold.
//! Shared helpers are imported from `super::` (see `use super::` below).

use std::path::PathBuf;

use anyhow::Context;
use rskim_research::{ast_codegen, ast_pipeline, ast_types, ast_validate, clone, codegen, config};

use super::{chrono_now, fetch_files_parallel, resolve_corpus_dir, types, write_json_table};

pub(super) fn cmd_ast_run(
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

    let table = ast_pipeline::build_ast_weight_table(
        &all_files,
        threshold,
        collect_trigrams,
        &chrono_now(),
    );

    eprintln!("Vocabulary: {} node kinds.", table.vocabulary.len());
    for (lang, weights) in &table.bigram_weights {
        eprintln!(
            "  {lang}: {} bigrams (threshold={threshold})",
            weights.len()
        );
    }
    for (lang, weights) in &table.trigram_weights {
        if !weights.is_empty() {
            eprintln!(
                "  {lang}: {} trigrams (threshold={threshold})",
                weights.len()
            );
        }
    }

    write_ast_weight_table(&table, output)?;
    log_ast_summary(&table);

    Ok(())
}

/// Print a compact per-language summary to stderr so the user can assess quality
/// without running `ast-validate` separately. Mirrors the feedback provided by
/// `cmd_run`'s `log_validation_summary` for the lexical pipeline.
///
/// Reuses `run_ast_validation` for error-rate computation to avoid duplicating
/// the formula that lives in `ast_validate`.
fn log_ast_summary(table: &ast_types::AstWeightTable) {
    let report = ast_validate::run_ast_validation(table);
    eprintln!("=== AST weight table summary ===");
    eprintln!("Vocabulary: {} node kinds", report.vocabulary_size);
    for lang_report in &report.per_language {
        let lang = &lang_report.bigram_distribution.language;
        let bigrams = lang_report.bigram_distribution.count;
        let trigrams = lang_report.trigram_distribution.count;
        let error_rate = lang_report.error_node_rate * 100.0;
        eprintln!("  {lang}: {bigrams} bigrams, {trigrams} trigrams, error_rate={error_rate:.2}%");
    }
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
    write_json_table(table, output_path, "AST weight table")
}

pub(super) fn cmd_ast_codegen(
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

pub(super) fn cmd_ast_validate(json_path: Option<PathBuf>) -> anyhow::Result<()> {
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
