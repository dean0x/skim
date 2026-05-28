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
    deny_list,
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

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OutputFormat::Markdown => write!(f, "markdown"),
            OutputFormat::Json => write!(f, "json"),
        }
    }
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

    // Deny-list patterns for the report (single source of truth: deny_list::pattern_names).
    let deny_list_patterns = deny_list::pattern_names();

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

/// Return the current UTC time as an ISO-8601 string (`YYYY-MM-DDTHH:MM:SSZ`).
///
/// Implemented with only `std::time` to avoid adding a `chrono`/`time`
/// dependency to this benchmark binary.  The Gregorian calendar arithmetic
/// accounts for leap years.  Falls back to `"unknown"` if the system clock
/// is before the Unix epoch (should never happen in practice).
fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(_) => return "unknown".to_string(),
    };

    // --- time of day ---
    let hour = (secs % 86400) / 3600;
    let minute = (secs % 3600) / 60;
    let second = secs % 60;

    // --- Gregorian calendar: days since epoch → year/month/day ---
    // Algorithm: "civil_from_days" (Howard Hinnant, https://howardhinnant.github.io/date_algorithms.html)
    let z = (secs / 86400) as i64 + 719_468; // shift to 0000-03-01 epoch
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // year of era [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month in [0, 11] (March=0)
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}Z")
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

fn save_to_devflow(timestamp: &str, format: &OutputFormat, content: &str) -> anyhow::Result<()> {
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    // --- parse_thresholds ---

    #[test]
    fn parse_thresholds_rejects_zero() {
        assert!(parse_thresholds("0.0").is_err());
    }

    #[test]
    fn parse_thresholds_accepts_one() {
        let result = parse_thresholds("1.0").unwrap();
        assert_eq!(result, vec![1.0]);
    }

    #[test]
    fn parse_thresholds_rejects_above_one() {
        assert!(parse_thresholds("1.1").is_err());
    }

    #[test]
    fn parse_thresholds_rejects_empty_input() {
        assert!(parse_thresholds("").is_err());
        assert!(parse_thresholds("  ").is_err());
    }

    #[test]
    fn parse_thresholds_deduplicates() {
        let result = parse_thresholds("0.1,0.1,0.2").unwrap();
        assert_eq!(result, vec![0.1, 0.2]);
    }

    #[test]
    fn parse_thresholds_rejects_nan() {
        assert!(parse_thresholds("NaN").is_err());
    }

    #[test]
    fn parse_thresholds_sorts_ascending() {
        let result = parse_thresholds("0.5,0.1,0.3").unwrap();
        assert_eq!(result, vec![0.1, 0.3, 0.5]);
    }

    #[test]
    fn parse_thresholds_trims_whitespace() {
        let result = parse_thresholds(" 0.1 , 0.2 ").unwrap();
        assert_eq!(result, vec![0.1, 0.2]);
    }

    // --- chrono_now ---

    #[test]
    fn chrono_now_produces_iso8601_format() {
        let ts = chrono_now();
        // Must match YYYY-MM-DDTHH:MM:SSZ  (20 chars)
        assert_eq!(ts.len(), 20, "unexpected length: {ts}");
        assert!(ts.ends_with('Z'), "missing Z suffix: {ts}");
        assert_eq!(&ts[4..5], "-", "missing year-month separator: {ts}");
        assert_eq!(&ts[7..8], "-", "missing month-day separator: {ts}");
        assert_eq!(&ts[10..11], "T", "missing T: {ts}");
        assert_eq!(&ts[13..14], ":", "missing hour-minute separator: {ts}");
        assert_eq!(&ts[16..17], ":", "missing minute-second separator: {ts}");
        // All numeric positions must be digits.
        for i in [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 14, 15, 17, 18] {
            assert!(
                ts.as_bytes()[i].is_ascii_digit(),
                "position {i} is not a digit in {ts}"
            );
        }
    }
}
