//! `cochange-validate` — co-change blast-radius prediction benchmark.
//!
//! Clones a corpus of OSS repositories (full history), builds co-change matrices
//! from the training split, and evaluates blast-radius prediction accuracy on
//! the test split at multiple Jaccard thresholds.
//!
//! # Usage
//!
//! ```text
//! cochange-validate [OPTIONS]
//!
//! Options:
//!   --corpus-dir <PATH>     Directory for cloned repos [default: .bench-corpus]
//!   --corpus-config <PATH>  Path to cochange-corpus.toml
//!   --format <FORMAT>       Output format: markdown | json [default: markdown]
//!   --save-report           Write report to .devflow/docs/
//!   --thresholds <LIST>     Comma-separated Jaccard thresholds [default: 0.01,0.05,0.1,0.2,0.3,0.5]
//!   --train-fraction <F>    Fraction of commits for training [default: 0.8]
//! ```

use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::{Parser, ValueEnum};
use rayon::prelude::*;

use rskim_bench::cochange::{
    report,
    types::{CochangeValidationResult, RepoCochangeResult, RepoManifest, RunMetadata},
    validate::{aggregate_metrics, validate_repo},
};
use rskim_research::config::load_corpus_config;

// ============================================================================
// CLI
// ============================================================================

/// Output format for the validation report.
#[derive(Debug, Clone, Default, ValueEnum)]
enum OutputFormat {
    /// Human-readable Markdown (default).
    #[default]
    Markdown,
    /// Machine-readable JSON.
    Json,
}

/// Co-change blast-radius prediction accuracy benchmark.
///
/// Clones OSS repos with full git history, builds co-change matrices from the
/// training split, and measures precision/recall of blast-radius predictions
/// on the test split at multiple Jaccard thresholds.
#[derive(Debug, Parser)]
#[command(name = "cochange-validate", version)]
struct Cli {
    /// Directory for cloned repositories.
    #[arg(long, default_value = ".bench-corpus")]
    corpus_dir: PathBuf,

    /// Path to cochange-corpus.toml.
    ///
    /// Defaults to `crates/rskim-research/cochange-corpus.toml` relative to the
    /// workspace root (resolved via `CARGO_MANIFEST_DIR` at compile time).
    #[arg(long)]
    corpus_config: Option<PathBuf>,

    /// Output format.
    #[arg(long, default_value_t = OutputFormat::Markdown)]
    format: OutputFormat,

    /// Write the report to `.devflow/docs/cochange-validate-<timestamp>.md|json`.
    #[arg(long, default_value_t = false)]
    save_report: bool,

    /// Comma-separated Jaccard thresholds to evaluate.
    #[arg(long, default_value = "0.01,0.05,0.1,0.2,0.3,0.5")]
    thresholds: String,

    /// Fraction of commits (chronologically oldest) used for training.
    ///
    /// Must be in (0, 1). Clamped to [0.01, 0.99] internally.
    #[arg(long, default_value_t = 0.8)]
    train_fraction: f64,
}

// ============================================================================
// Entry point
// ============================================================================

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Resolve corpus config path.
    let config_path = cli.corpus_config.unwrap_or_else(default_corpus_config);

    // Parse thresholds from comma-separated string.
    let thresholds = parse_thresholds(&cli.thresholds)?;

    // Load corpus config.
    let corpus = load_corpus_config(&config_path)
        .with_context(|| format!("loading corpus config from {}", config_path.display()))?;

    // Create corpus dir if absent.
    std::fs::create_dir_all(&cli.corpus_dir)
        .with_context(|| format!("creating corpus dir {}", cli.corpus_dir.display()))?;

    let timestamp = chrono_now();

    // Process repos in parallel (cap at 3 concurrent — DD-3).
    // rayon's ThreadPoolBuilder lets us bound concurrency independently of the
    // global pool so other crates' rayon usage is unaffected.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(3)
        .build()
        .context("building rayon thread pool")?;

    let repo_results = pool.install(|| {
        corpus
            .repos
            .par_iter()
            .map(|entry| {
                let repo_name = entry.url.rsplit('/').next().unwrap_or("unknown");
                eprintln!("[cochange-validate] processing: {repo_name}");
                validate_repo(entry, &cli.corpus_dir, &thresholds, cli.train_fraction)
            })
            .collect::<anyhow::Result<Vec<_>>>()
    })?;

    // Aggregate across repos that passed quality gates.
    let agg = aggregate_metrics(&repo_results, &thresholds);

    // Build manifests for reproducibility.
    let repo_manifests = build_manifests(&repo_results);

    // Deny-list patterns for the report.
    let deny_list_patterns = deny_list_pattern_names();

    let result = CochangeValidationResult {
        repos: repo_results,
        aggregate_metrics: agg,
        thresholds: thresholds.clone(),
        deny_list_patterns,
        run_metadata: RunMetadata {
            timestamp,
            corpus_config_path: config_path.display().to_string(),
            repo_manifests,
        },
    };

    // Render.
    let output = match cli.format {
        OutputFormat::Json => report::to_json(&result)?,
        OutputFormat::Markdown => report::to_markdown(&result),
    };

    if cli.save_report {
        save_to_devflow(&result.run_metadata.timestamp, &cli.format, &output)?;
    }

    println!("{output}");
    Ok(())
}

// ============================================================================
// Helpers
// ============================================================================

/// Default corpus config path: `crates/rskim-research/cochange-corpus.toml`.
fn default_corpus_config() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|p| p.join("rskim-research").join("cochange-corpus.toml"))
        .unwrap_or_else(|| PathBuf::from("cochange-corpus.toml"))
}

/// Parse a comma-separated threshold list.
///
/// # Errors
///
/// Returns an error if any token is not a valid `f64` in `(0.0, 1.0]`.
fn parse_thresholds(input: &str) -> anyhow::Result<Vec<f64>> {
    let mut thresholds: Vec<f64> = input
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let v = s
                .parse::<f64>()
                .with_context(|| format!("invalid threshold: {s:?}"))?;
            if v <= 0.0 || v > 1.0 || v.is_nan() {
                anyhow::bail!("threshold {s:?} is out of range (0.0, 1.0]");
            }
            Ok(v)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    if thresholds.is_empty() {
        anyhow::bail!("--thresholds must contain at least one value");
    }

    thresholds.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    thresholds.dedup_by(|a, b| (*a - *b).abs() < 1e-9);

    Ok(thresholds)
}

fn chrono_now() -> String {
    // Use std time to avoid a chrono/time dependency.
    // Format as YYYY-XX-XXT HH:MM:SSZ (approximate — year + time of day for human readability).
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let years = 1970 + (secs / 86400) / 365;
    format!("{years}-XX-XXT{:02}:{:02}:{:02}Z", (secs % 86400) / 3600, (secs % 3600) / 60, secs % 60)
}

fn build_manifests(repos: &[RepoCochangeResult]) -> Vec<RepoManifest> {
    repos
        .iter()
        .filter(|r| r.quality_gate_passed && r.error.is_none())
        .map(|r| RepoManifest {
            repo_url: r.repo_url.clone(),
            head_sha: r.head_sha.clone(),
            train_cutoff_timestamp: r.split_timestamp,
            train_commit_count: r.train_commits,
            test_commit_count: r.test_commits,
        })
        .collect()
}

fn deny_list_pattern_names() -> Vec<String> {
    vec![
        "Cargo.lock".to_string(),
        "package-lock.json".to_string(),
        "yarn.lock".to_string(),
        "go.sum".to_string(),
        "poetry.lock".to_string(),
        "pnpm-lock.yaml".to_string(),
        "Pipfile.lock".to_string(),
        "Gemfile.lock".to_string(),
        "composer.lock".to_string(),
        "flake.lock".to_string(),
        "vendor/".to_string(),
        "node_modules/".to_string(),
        "dist/".to_string(),
        "build/".to_string(),
        "target/".to_string(),
        "__pycache__/".to_string(),
        ".tox/".to_string(),
        "*.min.js".to_string(),
        "*.min.css".to_string(),
        "*.pb.go".to_string(),
        "*.generated.go".to_string(),
    ]
}

fn save_to_devflow(
    timestamp: &str,
    format: &OutputFormat,
    content: &str,
) -> anyhow::Result<()> {
    let docs_dir = Path::new(".devflow/docs");
    std::fs::create_dir_all(docs_dir)?;
    let safe_ts = timestamp.replace([':', '/'], "-");
    let ext = match format {
        OutputFormat::Json => "json",
        OutputFormat::Markdown => "md",
    };
    let path = docs_dir.join(format!("cochange-validate-{safe_ts}.{ext}"));
    std::fs::write(&path, content)
        .with_context(|| format!("writing report to {}", path.display()))?;
    eprintln!("[cochange-validate] report saved to {}", path.display());
    Ok(())
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputFormat::Markdown => write!(f, "markdown"),
            OutputFormat::Json => write!(f, "json"),
        }
    }
}
