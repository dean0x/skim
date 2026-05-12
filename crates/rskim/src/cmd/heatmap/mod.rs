//! `skim heatmap` subcommand — git history risk/coupling analysis.
//!
//! Computes 6 metrics from git log history:
//! 1. Churn: commit frequency per file
//! 2. Coupling: files that change together (blast radius)
//! 3. Stability: composite score (churn + recency + volatility)
//! 4. Author diversity: bus-factor risk detection
//! 5. Fix-after-touch: proximity-based bug-introduction risk
//! 6. Module encapsulation: cross-boundary coupling health

mod excludes;
mod git_source;
mod insights;
mod metrics;
mod output;
mod types;
mod window;

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::analytics::{CommandType, RecordingContext};

use excludes::{build_exclude_set, should_exclude};
use git_source::{CliGitSource, GitDataSource};
use insights::{build_insights_result, compute_insights};
use metrics::{
    build_fix_regex, compute_authors, compute_churn, compute_coupling, compute_encapsulation,
    compute_fix_after_touch, compute_stability,
};
use output::{render_insights_json, render_insights_text, render_json, render_text};
use types::{CommitRecord, FileMetrics, HeatmapConfig, HeatmapResult, ResolvedWindow};
use window::{build_window_info, format_epoch, resolve_effective_config};

// ============================================================================
// Entry point
// ============================================================================

/// Run the `skim heatmap` subcommand.
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let mut config = match parse_args(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skim heatmap: {e}");
            eprintln!("Run `skim heatmap --help` for usage.");
            return Ok(ExitCode::FAILURE);
        }
    };

    let git_source = CliGitSource::new();

    // Resolve --diff to concrete file list
    if let Some(exit) = resolve_diff_files(&git_source, &mut config)? {
        return Ok(exit);
    }

    run_with_source(&git_source, &config, analytics)
}

/// Resolve `--diff` to a concrete file list, mutating `config.files`.
///
/// Returns `Some(ExitCode::FAILURE)` for any early-exit condition so that `run()`
/// can propagate it directly. Returns `Ok(None)` when resolution succeeded and the
/// caller should continue.
///
/// Path correctness: git diff output is repo-root-relative, so deleted-file
/// detection uses [`CliGitSource::get_repo_root`] to build absolute paths rather
/// than relying on cwd. The `is_git_repo()` guard is intentionally omitted — if
/// the cwd is not inside a repository, `fetch_diff_files` will return an error
/// with a clear message, and `prepare_commits` already handles the non-repo case.
fn resolve_diff_files(
    git_source: &CliGitSource,
    config: &mut HeatmapConfig,
) -> anyhow::Result<Option<ExitCode>> {
    let Some(base) = config.diff_base.take() else {
        return Ok(None);
    };

    match git_source.fetch_diff_files(&base) {
        Ok(files) if files.is_empty() => {
            eprintln!("skim heatmap: no files changed vs '{base}'");
            Ok(Some(ExitCode::FAILURE))
        }
        Ok(files) => {
            // Detect deleted files for annotation. git diff output is repo-root-relative,
            // so resolve paths against the repo root to avoid cwd-dependent failures.
            let root = git_source.get_repo_root().unwrap_or_default();
            for f in &files {
                let abs = std::path::Path::new(&root).join(f);
                if !abs.exists() {
                    eprintln!(
                        "skim heatmap: warning: file '{}' deleted on current branch",
                        f
                    );
                }
            }
            config.files = files;
            Ok(None)
        }
        Err(e) => {
            eprintln!("skim heatmap: {e}");
            Ok(Some(ExitCode::FAILURE))
        }
    }
}

/// Bundled output of [`prepare_commits`] — everything needed to call [`compute_heatmap`].
struct PreparedCommits {
    commits: Vec<CommitRecord>,
    window: ResolvedWindow,
    now_epoch: u64,
    repo_root: String,
    warnings: Vec<String>,
}

/// Execute Steps 1–4 of the heatmap pipeline (git I/O + exclusions).
///
/// Returns `Ok(None)` for all early-exit conditions (not a git repo, no commits,
/// all commits excluded). Callers treat `None` as a `ExitCode::FAILURE` signal.
/// Returns `Ok(Some(PreparedCommits))` when the pipeline should proceed to
/// metric computation.
fn prepare_commits(
    source: &dyn GitDataSource,
    config: &HeatmapConfig,
) -> anyhow::Result<Option<PreparedCommits>> {
    // Step 1: Validate git environment
    if !source.is_git_repo() {
        eprintln!("skim heatmap: Not a git repository");
        return Ok(None);
    }

    let repo_root = match source.get_repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("skim heatmap: {e}");
            return Ok(None);
        }
    };

    let mut warnings: Vec<String> = Vec::new();

    if source.detect_shallow_clone() {
        warnings.push(
            "Shallow clone detected — history may be incomplete, metrics may be skewed."
                .to_string(),
        );
    }

    if config.debug {
        eprintln!("[skim:heatmap] repo root: {repo_root}");
    }

    // Capture a single clock snapshot for all window-resolution helpers so that
    // preset_to_since_secs, resolve_effective_config, and build_window_info all
    // use the same value (Issue 1: temporal consistency).
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Step 2: Resolve effective config (presets and --last)
    let (effective_config, window) =
        resolve_effective_config(config, source, &mut warnings, now_epoch)?;

    if config.debug {
        let mode = if window.dual_mode {
            "dual"
        } else if config.last_n.is_some() {
            "count"
        } else {
            "time"
        };
        eprintln!("[skim:heatmap] window mode: {mode}");
        if let Some(since) = window.since {
            eprintln!(
                "[skim:heatmap] since epoch: {since} ({})",
                format_epoch(since)
            );
        }
    }

    // Step 3: Fetch commits
    let raw_commits = match source.fetch_commits(&effective_config) {
        Ok(c) => c,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not installed") || msg.contains("not in PATH") {
                eprintln!("skim heatmap: {msg}");
            } else {
                eprintln!("skim heatmap: failed to fetch git log: {msg}");
            }
            return Ok(None);
        }
    };

    if config.debug {
        eprintln!("[skim:heatmap] raw commits fetched: {}", raw_commits.len());
    }

    if raw_commits.is_empty() {
        eprintln!("skim heatmap: No commits found in repository");
        return Ok(None);
    }

    // Step 4: Apply exclusions
    let exclude_set = build_exclude_set(config.no_exclude, &config.extra_excludes);
    let raw_commit_count = raw_commits.len();
    let mut commits = raw_commits;
    for commit in &mut commits {
        commit
            .files
            .retain(|f| !should_exclude(&f.path, &exclude_set));
    }
    // Remove commits that are now file-less after exclusion
    commits.retain(|c| !c.files.is_empty());

    if config.debug {
        eprintln!(
            "[skim:heatmap] commits after exclusion: {} ({} excluded)",
            commits.len(),
            raw_commit_count - commits.len()
        );
    }

    if commits.is_empty() {
        eprintln!("skim heatmap: No analyzable files after exclusions");
        return Ok(None);
    }

    Ok(Some(PreparedCommits {
        commits,
        window,
        now_epoch,
        repo_root,
        warnings,
    }))
}

/// Orchestration with injected data source (enables testing).
///
/// All git I/O is routed through `source` — infra checks (repo detection, root,
/// shallow clone, commit count) and the commit fetch all use the same trait object.
fn run_with_source(
    source: &dyn GitDataSource,
    config: &HeatmapConfig,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let PreparedCommits {
        commits,
        window,
        now_epoch,
        repo_root,
        warnings,
    } = match prepare_commits(source, config)? {
        Some(p) => p,
        None => return Ok(ExitCode::FAILURE),
    };

    // Steps 5-9: Compute metrics and assemble result
    let start_time = std::time::Instant::now();
    let mut result = compute_heatmap(commits, config, &window, now_epoch, repo_root, warnings);

    // Apply file-targeting display filter (after full metric computation)
    if !config.files.is_empty() {
        apply_file_scope(&mut result, &config.files);
    }

    // Compute display top_n: when files are explicitly targeted and --top
    // was not given, show all targeted files rather than the global default.
    let display_top_n = if !config.files.is_empty() && !config.top_explicit {
        config.files.len()
    } else {
        config.top_n
    };

    // Step 10: Render
    let elapsed = start_time.elapsed();
    let mut stdout = io::stdout().lock();

    // Insights early-return (before truncation — insights use full dataset)
    if config.insights {
        let insights = compute_insights(&result);
        if config.format_json {
            let insights_result = build_insights_result(&result, insights);
            let json = render_insights_json(&insights_result)?;
            writeln!(stdout, "{json}")?;
        } else {
            let text = render_insights_text(&insights);
            write!(stdout, "{text}")?;
        }
        record_heatmap_analytics(analytics, "skim heatmap --insights", elapsed);
        return Ok(ExitCode::SUCCESS);
    }

    // Apply --top N limit to files (not needed for insights)
    result.files.truncate(display_top_n);

    if config.format_json {
        let json = render_json(&result)?;
        writeln!(stdout, "{json}")?;
    } else {
        let text = render_text(&result, display_top_n);
        write!(stdout, "{text}")?;
    }

    // Step 11: Fire-and-forget analytics
    record_heatmap_analytics(analytics, "skim heatmap", elapsed);

    Ok(ExitCode::SUCCESS)
}

/// Fire-and-forget analytics recording for heatmap commands.
fn record_heatmap_analytics(
    analytics: &crate::analytics::AnalyticsConfig,
    command: &str,
    elapsed: std::time::Duration,
) {
    let rec = RecordingContext {
        enabled: analytics.enabled,
        command_type: CommandType::Heatmap,
        parse_tier: None,
        session_id: analytics.session_id.as_deref(),
    };
    crate::analytics::try_record_command(
        rec,
        String::new(),
        String::new(),
        command.to_string(),
        elapsed,
    );
}

// ============================================================================
// File-scope display filter (Step 5-alt)
// ============================================================================

/// Filter heatmap results to only include targeted files.
///
/// Filtering happens AFTER metric computation to preserve coupling accuracy.
/// Coupling graph retains edges where at least one endpoint is targeted
/// (blast radius view).
fn apply_file_scope(result: &mut HeatmapResult, files: &[String]) {
    use std::collections::HashSet;

    let target_set: HashSet<&str> = files.iter().map(|s| s.as_str()).collect();

    // Warn about targets not found in results
    let result_paths: HashSet<&str> = result.files.iter().map(|f| f.path.as_str()).collect();
    for target in &target_set {
        if !result_paths.contains(target) {
            result
                .warnings
                .push(format!("targeted file '{target}' not found in git history"));
        }
    }

    // Filter files
    result
        .files
        .retain(|f| target_set.contains(f.path.as_str()));

    // Filter coupling graph: keep edges where at least one endpoint is targeted
    result
        .coupling_graph
        .retain(|e| target_set.contains(e.a.as_str()) || target_set.contains(e.b.as_str()));

    // Filter modules: keep only modules whose top-level directory contains a
    // targeted file.  Modules use top-level directory names (e.g. "src",
    // "tests") produced by `extract_top_dir`, so we extract the first path
    // component to align with those names regardless of nesting depth.
    let target_dirs: HashSet<&str> = files
        .iter()
        .filter_map(|f| f.split_once('/').map(|(top, _)| top))
        .collect();
    result
        .modules
        .retain(|m| target_dirs.contains(m.path.as_str()));

    result.file_targets = Some(files.to_vec());
}

// ============================================================================
// Pure metric computation (Steps 5-9)
// ============================================================================

/// Minimum number of co-occurrences required before a coupling pair or module
/// is included in results. Prevents noise from one-off coincidences.
const MIN_SUPPORT_THRESHOLD: usize = 3;

/// Compute all six risk metrics from commits and assemble a `HeatmapResult`.
///
/// No git I/O — all repository data is pre-fetched by callers (Steps 1-4 in
/// `run_with_source`). Accepting `now_epoch` as a parameter (instead of calling
/// `SystemTime::now()` here) keeps metric computation deterministic and testable.
///
/// Debug timing is emitted to stderr when `config.debug` is enabled.
fn compute_heatmap(
    commits: Vec<CommitRecord>,
    config: &HeatmapConfig,
    window: &ResolvedWindow,
    now_epoch: u64,
    repository: String,
    warnings: Vec<String>,
) -> HeatmapResult {
    use std::time::Instant;

    let t0 = Instant::now();
    let fix_regex = build_fix_regex();

    // Step 5: Compute metrics
    // Phase 1: churn must run first — max_churn feeds into stability.
    let churn_map = compute_churn(&commits);
    let max_churn = churn_map.values().map(|m| m.commits).max().unwrap_or(1);

    // Phase 2: remaining 5 metrics are independent of each other — run in parallel.
    let fix_window = config.fix_window;
    let coupling_threshold = config.coupling_threshold;
    let (
        (stability_map, author_map),
        ((fix_risk_map, (blast_radius_map, coupling_graph)), modules),
    ) = rayon::join(
        || {
            rayon::join(
                || compute_stability(&commits, &fix_regex, max_churn, now_epoch),
                || compute_authors(&commits),
            )
        },
        || {
            rayon::join(
                || {
                    rayon::join(
                        || compute_fix_after_touch(&commits, &fix_regex, fix_window),
                        || compute_coupling(&commits, coupling_threshold, MIN_SUPPORT_THRESHOLD),
                    )
                },
                || compute_encapsulation(&commits, MIN_SUPPORT_THRESHOLD),
            )
        },
    );

    if config.debug {
        let elapsed = t0.elapsed();
        eprintln!(
            "[skim:heatmap] metrics computed in {:.1}ms — {} files, {} coupling edges, {} modules",
            elapsed.as_secs_f64() * 1000.0,
            churn_map.len(),
            coupling_graph.len(),
            modules.len(),
        );
    }

    // Step 6: Assemble FileMetrics
    // `churn_map` already contains every path seen across all commits; no need
    // to rebuild a separate HashSet from the commit list.
    let mut file_metrics: Vec<FileMetrics> = churn_map
        .into_iter()
        .map(|(path, churn)| {
            let stability_score = stability_map.get(&path).copied().unwrap_or(100);
            let authors = author_map.get(&path).cloned().unwrap_or_default();
            let fix_risk = fix_risk_map.get(&path).cloned().unwrap_or_default();
            let blast_radius = blast_radius_map.get(&path).cloned().unwrap_or_default();

            FileMetrics {
                path,
                churn,
                stability_score,
                authors,
                fix_risk,
                blast_radius,
            }
        })
        .collect();

    // Sort by stability_score ascending (riskiest first)
    file_metrics.sort_by_key(|f| f.stability_score);

    // Step 7: Build window info
    let window_info = build_window_info(window, commits.len(), now_epoch);

    // Step 8: Get excluded patterns for output
    let excluded_patterns: Vec<String> = if config.no_exclude {
        Vec::new()
    } else {
        let capacity = excludes::DEFAULT_EXCLUDES.len() + config.extra_excludes.len();
        let mut patterns = Vec::with_capacity(capacity);
        patterns.extend(excludes::DEFAULT_EXCLUDES.iter().map(|s| s.to_string()));
        patterns.extend(config.extra_excludes.iter().cloned());
        patterns
    };

    // Step 9: Build result
    HeatmapResult {
        version: 1,
        generated_at: format_epoch(now_epoch),
        repository,
        window: window_info,
        files: file_metrics,
        modules,
        coupling_graph,
        excluded_patterns,
        warnings,
        file_targets: None,
    }
}

// ============================================================================
// Argument parsing (see args.rs)
// ============================================================================

/// Parse CLI args into `HeatmapConfig`.
///
/// Follows the manual flag-parsing pattern used by `stats.rs` and `discover.rs`.
/// Initialises `config.debug` from the process-wide debug flag so that
/// `SKIM_DEBUG=1` (initialised by `main()` before dispatch) is honoured automatically.
fn parse_args(args: &[String]) -> anyhow::Result<HeatmapConfig> {
    let mut config = HeatmapConfig {
        // Inherit SKIM_DEBUG / --debug flag set by main() before subcommand dispatch.
        debug: crate::debug::is_debug_enabled(),
        ..HeatmapConfig::default()
    };
    let mut i = 0;

    // Value-taking flags — used to detect when a flag is passed without a value
    // (e.g. `skim heatmap --since`) before extract_value calls so the error is
    // actionable rather than falling through to "unknown flag: --since".
    const VALUE_FLAGS: &[&str] = &[
        "--since",
        "--path",
        "--top",
        "--window",
        "--last",
        "--exclude",
        "--coupling-threshold",
        "--fix-window",
        "--format",
        "--diff",
    ];

    while i < args.len() {
        let arg = args[i].as_str();

        if VALUE_FLAGS.contains(&arg) && i + 1 >= args.len() {
            anyhow::bail!("{arg} requires a value");
        }

        // --since=VALUE or --since VALUE
        if let Some(val) = extract_value(args, &mut i, "--since") {
            let ts = parse_since_value(&val)?;
            config.since = Some(ts);
            continue;
        }

        // --path
        if let Some(val) = extract_value(args, &mut i, "--path") {
            config.path = Some(val);
            continue;
        }

        // --top
        if let Some(val) = extract_value(args, &mut i, "--top") {
            let n: usize = val
                .parse()
                .map_err(|_| anyhow::anyhow!("--top requires a positive integer"))?;
            if n == 0 {
                anyhow::bail!("--top must be at least 1");
            }
            config.top_n = n;
            config.top_explicit = true;
            continue;
        }

        // --window
        if let Some(val) = extract_value(args, &mut i, "--window") {
            config.window_preset = Some(val);
            continue;
        }

        // --last
        if let Some(val) = extract_value(args, &mut i, "--last") {
            let n: usize = val
                .parse()
                .map_err(|_| anyhow::anyhow!("--last requires a positive integer"))?;
            if n == 0 {
                anyhow::bail!("--last must be at least 1");
            }
            config.last_n = Some(n);
            continue;
        }

        // --exclude
        if let Some(val) = extract_value(args, &mut i, "--exclude") {
            config.extra_excludes.push(val);
            continue;
        }

        // --coupling-threshold
        if let Some(val) = extract_value(args, &mut i, "--coupling-threshold") {
            config.coupling_threshold = val
                .parse::<f64>()
                .map_err(|_| {
                    anyhow::anyhow!("--coupling-threshold requires a float between 0 and 1")
                })?
                .clamp(0.0, 1.0);
            continue;
        }

        // --fix-window
        if let Some(val) = extract_value(args, &mut i, "--fix-window") {
            let n: usize = val
                .parse()
                .map_err(|_| anyhow::anyhow!("--fix-window requires a positive integer"))?;
            if n == 0 {
                anyhow::bail!("--fix-window must be at least 1");
            }
            config.fix_window = n;
            continue;
        }

        // --format VALUE
        if let Some(val) = extract_value(args, &mut i, "--format") {
            if val == "json" {
                config.format_json = true;
            } else {
                anyhow::bail!("--format only supports 'json', got: {val}");
            }
            continue;
        }

        // --diff VALUE
        if let Some(val) = extract_value(args, &mut i, "--diff") {
            config.diff_base = Some(val);
            continue;
        }

        // Boolean flags
        if apply_boolean_flag(&mut config, arg)? {
            i += 1;
            continue;
        }

        if arg.starts_with('-') {
            anyhow::bail!("unknown flag: {arg}");
        }
        // Positional (non-flag) argument — file target.
        config.files.push(arg.to_string());
        i += 1;
        continue;
    }

    // Post-parse validation
    if config.diff_base.is_some() && !config.files.is_empty() {
        anyhow::bail!("cannot combine --diff with explicit file arguments");
    }

    // Normalize file paths: strip leading ./
    for f in &mut config.files {
        if let Some(stripped) = f.strip_prefix("./") {
            *f = stripped.to_string();
        }
    }

    Ok(config)
}

/// Apply a recognised boolean flag to `config`.
///
/// Returns `Ok(true)` if the flag was recognised and applied, `Ok(false)` if
/// the flag is unknown (caller falls through to the unknown-flag error).
fn apply_boolean_flag(config: &mut HeatmapConfig, flag: &str) -> anyhow::Result<bool> {
    match flag {
        "--json" => config.format_json = true,
        "--no-exclude" => config.no_exclude = true,
        "--insights" => config.insights = true,
        "--debug" => {
            config.debug = true;
            crate::debug::force_enable_debug();
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// Extract a `--flag VALUE` or `--flag=VALUE` pair, advancing `i`.
///
/// Returns `Some(value_string)` on match, `None` otherwise. Advances `i`
/// past the consumed argument(s).
fn extract_value(args: &[String], i: &mut usize, flag: &str) -> Option<String> {
    let arg = args[*i].as_str();
    let equals_prefix = format!("{flag}=");

    if arg == flag {
        // --flag VALUE form
        if *i + 1 < args.len() {
            *i += 2;
            Some(args[*i - 1].clone())
        } else {
            None
        }
    } else if let Some(val) = arg.strip_prefix(&equals_prefix) {
        // --flag=VALUE form
        *i += 1;
        Some(val.to_string())
    } else {
        None
    }
}

/// Parse a `--since` value: accepts epoch seconds (integer) or duration strings
/// like "30d", "2w", "24h".
fn parse_since_value(val: &str) -> anyhow::Result<u64> {
    // Try plain integer (epoch seconds)
    if let Ok(epoch) = val.parse::<u64>() {
        return Ok(epoch);
    }
    // Try duration string
    let sys_time = crate::cmd::session::types::parse_duration_ago(val)?;
    let epoch = sys_time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| anyhow::anyhow!("--since: time before Unix epoch"))?
        .as_secs();
    Ok(epoch)
}

// ============================================================================
// Help
// ============================================================================

fn print_help() {
    print!(
        "\
skim heatmap — git history risk and coupling analysis

USAGE:
    skim heatmap [OPTIONS] [FILE...]

OPTIONS:
    --since <VALUE>               Analyze commits since epoch (seconds) or duration (30d, 2w, 24h)
    --last <N>                    Analyze last N commits
    --window <PRESET>             Named window: sprint|month|quarter|half|year|all
    --path <DIR>                  Scope analysis to files under this path
    --diff <BASE>                 Show only files changed vs BASE (three-dot diff)
    --json, --format json         Output JSON instead of human-readable text
    --top <N>                     Maximum files to display (default: 20)
    --no-exclude                  Disable default exclusion patterns (lock files, build dirs, etc.)
    --exclude <PATTERN>           Add extra glob pattern to exclude (repeatable)
    --coupling-threshold <FLOAT>  Coupling confidence threshold (default: 0.5)
    --fix-window <N>              Proximity window for fix-after-touch detection (default: 5)
    --insights                    Show only notable findings (threshold-filtered insights)
    --debug                       Enable debug output
    -h, --help                    Show this help message

WINDOW PRESETS:
    sprint     14 days
    month      30 days
    quarter    90 days
    half       180 days
    year       365 days
    all        No time limit (analyze entire history)

FILE TARGETING:
    Positional file arguments and --diff scope the OUTPUT, not the git history.
    Metrics are computed on full commit history for accuracy — coupling and
    fix-risk scores reflect the complete picture, then display is narrowed.

    --path scopes the git log itself (commit-level filter). File targeting and
    --path compose: --path limits which commits are analyzed, file arguments
    limit which results are shown.

    --diff and explicit file arguments are mutually exclusive.

EXAMPLES:
    skim heatmap                           # Analyze last 90 days
    skim heatmap --last 200                # Analyze last 200 commits
    skim heatmap --window sprint           # Analyze last sprint
    skim heatmap --since 30d               # Analyze last 30 days
    skim heatmap --json                    # JSON output
    skim heatmap --path src/               # Scope to src/ directory
    skim heatmap --no-exclude              # Include lock files and build artifacts
    skim heatmap --coupling-threshold 0.3  # Lower coupling threshold
    skim heatmap src/main.rs               # Scope output to one file
    skim heatmap --diff main               # Show files changed vs main
    skim heatmap --path src/ src/main.rs   # Combine path + file scoping
    skim heatmap --insights                # One-liner findings only
    skim heatmap --insights --json         # Insights as JSON (agent-friendly)
    skim heatmap --insights src/main.rs    # Insights for specific file

METRICS:
    Top Churn          Files changed most frequently
    Blast Radius       Files that tend to change together (coupling)
    Fix Risk           Files with high fix-commit density or fix-after-touch
    Module Health      Directory encapsulation (cross-boundary coupling)
    Bus Factor Risk    Files with a single dominant author (>80%% of commits)
"
    );
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_args_defaults() {
        let config = parse_args(&[]).unwrap();
        assert_eq!(config.top_n, 20);
        assert!((config.coupling_threshold - 0.5).abs() < 1e-9);
        assert_eq!(config.fix_window, 5);
        assert!(!config.format_json);
        assert!(!config.no_exclude);
    }

    #[test]
    fn test_parse_args_json_flag() {
        let config = parse_args(&["--json".to_string()]).unwrap();
        assert!(config.format_json);
    }

    #[test]
    fn test_parse_args_top_n() {
        let config = parse_args(&["--top".to_string(), "5".to_string()]).unwrap();
        assert_eq!(config.top_n, 5);
    }

    #[test]
    fn test_parse_args_top_n_equals() {
        let config = parse_args(&["--top=10".to_string()]).unwrap();
        assert_eq!(config.top_n, 10);
    }

    #[test]
    fn test_parse_args_window_preset() {
        let config = parse_args(&["--window=sprint".to_string()]).unwrap();
        assert_eq!(config.window_preset.as_deref(), Some("sprint"));
    }

    #[test]
    fn test_parse_args_last_n() {
        let config = parse_args(&["--last=100".to_string()]).unwrap();
        assert_eq!(config.last_n, Some(100));
    }

    #[test]
    fn test_parse_args_no_exclude() {
        let config = parse_args(&["--no-exclude".to_string()]).unwrap();
        assert!(config.no_exclude);
    }

    #[test]
    fn test_parse_args_coupling_threshold() {
        let config = parse_args(&["--coupling-threshold=0.3".to_string()]).unwrap();
        assert!((config.coupling_threshold - 0.3).abs() < 1e-9);
    }

    #[test]
    fn test_parse_args_since_epoch() {
        let config = parse_args(&["--since=1700000000".to_string()]).unwrap();
        assert_eq!(config.since, Some(1_700_000_000));
    }

    #[test]
    fn test_parse_args_since_duration() {
        let config = parse_args(&["--since=30d".to_string()]).unwrap();
        // Should be set to some epoch in the past
        assert!(config.since.is_some());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let since = config.since.unwrap();
        let diff = now - since;
        assert!(diff >= 29 * 86400 && diff <= 31 * 86400);
    }

    #[test]
    fn test_parse_args_path() {
        let config = parse_args(&["--path=src/".to_string()]).unwrap();
        assert_eq!(config.path.as_deref(), Some("src/"));
    }

    #[test]
    fn test_parse_args_unknown_flag_errors() {
        let result = parse_args(&["--unknown-flag".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_args_top_zero_errors() {
        let result = parse_args(&["--top=0".to_string()]);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("--top must be at least 1"), "got: {msg}");
    }

    #[test]
    fn test_parse_args_fix_window_zero_errors() {
        let result = parse_args(&["--fix-window=0".to_string()]);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("--fix-window must be at least 1"),
            "got: {msg}"
        );
    }

    #[test]
    fn test_parse_args_last_zero_errors() {
        let result = parse_args(&["--last=0".to_string()]);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("--last must be at least 1"), "got: {msg}");
    }

    #[test]
    fn test_parse_args_since_missing_value_errors() {
        let result = parse_args(&["--since".to_string()]);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("--since requires a value"), "got: {msg}");
    }

    #[test]
    fn test_parse_args_positional_files() {
        let config = parse_args(&["src/main.rs".to_string(), "src/lib.rs".to_string()]).unwrap();
        assert_eq!(config.files, vec!["src/main.rs", "src/lib.rs"]);
    }

    #[test]
    fn test_parse_args_diff_flag() {
        let config = parse_args(&["--diff".to_string(), "main".to_string()]).unwrap();
        assert_eq!(config.diff_base, Some("main".to_string()));
    }

    #[test]
    fn test_parse_args_diff_equals() {
        let config = parse_args(&["--diff=develop".to_string()]).unwrap();
        assert_eq!(config.diff_base, Some("develop".to_string()));
    }

    #[test]
    fn test_parse_args_diff_and_files_mutual_exclusion() {
        let result = parse_args(&[
            "--diff".to_string(),
            "main".to_string(),
            "file.rs".to_string(),
        ]);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("cannot combine --diff"), "got: {msg}");
    }

    #[test]
    fn test_parse_args_file_path_normalization() {
        let config = parse_args(&["./src/main.rs".to_string()]).unwrap();
        assert_eq!(config.files, vec!["src/main.rs"]);
    }

    #[test]
    fn test_parse_args_top_explicit_flag() {
        let config = parse_args(&["--top".to_string(), "5".to_string()]).unwrap();
        assert!(config.top_explicit);
    }

    #[test]
    fn test_parse_args_top_implicit_by_default() {
        let config = parse_args(&[]).unwrap();
        assert!(!config.top_explicit);
    }

    #[test]
    fn test_parse_args_diff_without_value() {
        let result = parse_args(&["--diff".to_string()]);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("requires a value"), "got: {msg}");
    }

    #[test]
    fn test_parse_args_files_with_other_flags() {
        let config = parse_args(&[
            "--since".to_string(),
            "30d".to_string(),
            "src/main.rs".to_string(),
            "--json".to_string(),
        ])
        .unwrap();
        assert_eq!(config.files, vec!["src/main.rs"]);
        assert!(config.format_json);
        assert!(config.since.is_some());
    }

    #[test]
    fn test_parse_args_extra_exclude() {
        let config = parse_args(&["--exclude=*.generated.ts".to_string()]).unwrap();
        assert_eq!(config.extra_excludes, vec!["*.generated.ts"]);
    }

    #[test]
    fn test_parse_args_fix_window() {
        let config = parse_args(&["--fix-window=10".to_string()]).unwrap();
        assert_eq!(config.fix_window, 10);
    }

    // -----------------------------------------------------------------------
    // apply_file_scope
    // -----------------------------------------------------------------------

    fn make_test_result() -> HeatmapResult {
        use types::{
            AuthorMetrics, ChurnMetrics, CouplingEdge, FileMetrics, FixRiskMetrics, ModuleHealth,
            WindowInfo,
        };
        HeatmapResult {
            version: 1,
            generated_at: "2025-01-01".to_string(),
            repository: "test".to_string(),
            window: WindowInfo {
                mode: "90d".to_string(),
                since: "2024-10-01".to_string(),
                until: "2025-01-01".to_string(),
                commits_analyzed: 10,
                effective_strategy: None,
            },
            files: vec![
                FileMetrics {
                    path: "src/main.rs".to_string(),
                    churn: ChurnMetrics {
                        commits: 5,
                        rate: 0.5,
                    },
                    stability_score: 42,
                    authors: AuthorMetrics::default(),
                    fix_risk: FixRiskMetrics::default(),
                    blast_radius: vec![],
                },
                FileMetrics {
                    path: "src/lib.rs".to_string(),
                    churn: ChurnMetrics {
                        commits: 3,
                        rate: 0.3,
                    },
                    stability_score: 60,
                    authors: AuthorMetrics::default(),
                    fix_risk: FixRiskMetrics::default(),
                    blast_radius: vec![],
                },
                FileMetrics {
                    path: "tests/test.rs".to_string(),
                    churn: ChurnMetrics {
                        commits: 2,
                        rate: 0.2,
                    },
                    stability_score: 80,
                    authors: AuthorMetrics::default(),
                    fix_risk: FixRiskMetrics::default(),
                    blast_radius: vec![],
                },
            ],
            modules: vec![
                ModuleHealth {
                    path: "src".to_string(),
                    encapsulation_pct: 80.0,
                    files_count: 2,
                    total_commits: 8,
                    cross_boundary_commits: 1,
                },
                ModuleHealth {
                    path: "tests".to_string(),
                    encapsulation_pct: 90.0,
                    files_count: 1,
                    total_commits: 2,
                    cross_boundary_commits: 0,
                },
            ],
            coupling_graph: vec![
                CouplingEdge {
                    a: "src/main.rs".to_string(),
                    b: "src/lib.rs".to_string(),
                    confidence: 0.8,
                    support: 4,
                },
                CouplingEdge {
                    a: "tests/test.rs".to_string(),
                    b: "src/lib.rs".to_string(),
                    confidence: 0.6,
                    support: 3,
                },
            ],
            excluded_patterns: vec![],
            warnings: vec![],
            file_targets: None,
        }
    }

    #[test]
    fn test_apply_file_scope_filters_files() {
        let mut result = make_test_result();
        apply_file_scope(&mut result, &["src/main.rs".to_string()]);
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].path, "src/main.rs");
    }

    #[test]
    fn test_apply_file_scope_filters_coupling() {
        let mut result = make_test_result();
        apply_file_scope(&mut result, &["src/main.rs".to_string()]);
        // Edge with src/main.rs kept, edge without any target dropped
        assert_eq!(result.coupling_graph.len(), 1);
        assert_eq!(result.coupling_graph[0].a, "src/main.rs");
    }

    #[test]
    fn test_apply_file_scope_filters_modules() {
        let mut result = make_test_result();
        apply_file_scope(&mut result, &["src/main.rs".to_string()]);
        assert_eq!(result.modules.len(), 1);
        assert_eq!(result.modules[0].path, "src");
    }

    #[test]
    fn test_apply_file_scope_warns_missing() {
        let mut result = make_test_result();
        apply_file_scope(&mut result, &["nonexistent.rs".to_string()]);
        assert!(result
            .warnings
            .iter()
            .any(|w| { w.contains("nonexistent.rs") && w.contains("not found in git history") }));
    }

    #[test]
    fn test_apply_file_scope_sets_file_targets() {
        let mut result = make_test_result();
        apply_file_scope(&mut result, &["src/main.rs".to_string()]);
        assert_eq!(result.file_targets, Some(vec!["src/main.rs".to_string()]));
    }

    /// Regression test for deeply-nested file targets.
    ///
    /// When a targeted file lives multiple levels deep (e.g.
    /// `src/cmd/heatmap/mod.rs`), the module filter must match it against the
    /// top-level module `"src"` — not the immediate parent `"src/cmd/heatmap"`.
    /// The previous `rsplit_once('/')` implementation extracted the deepest
    /// parent directory and would incorrectly drop the `"src"` module.
    #[test]
    fn test_apply_file_scope_filters_modules_deeply_nested() {
        use types::{AuthorMetrics, ChurnMetrics, FileMetrics, FixRiskMetrics, ModuleHealth};
        let mut result = make_test_result();
        // Replace the file list with a single deeply-nested path
        result.files = vec![FileMetrics {
            path: "src/cmd/heatmap/mod.rs".to_string(),
            churn: ChurnMetrics {
                commits: 1,
                rate: 0.1,
            },
            stability_score: 50,
            authors: AuthorMetrics::default(),
            fix_risk: FixRiskMetrics::default(),
            blast_radius: vec![],
        }];
        // Add a matching deeply-nested module entry to the modules list
        result.modules.push(ModuleHealth {
            path: "src/cmd".to_string(),
            encapsulation_pct: 70.0,
            files_count: 1,
            total_commits: 1,
            cross_boundary_commits: 0,
        });

        apply_file_scope(&mut result, &["src/cmd/heatmap/mod.rs".to_string()]);

        // The top-level "src" module must be retained; "tests" must be dropped;
        // the "src/cmd" entry (not a valid top-level module) must also be dropped.
        let module_paths: Vec<&str> = result.modules.iter().map(|m| m.path.as_str()).collect();
        assert!(
            module_paths.contains(&"src"),
            "expected 'src' module to be retained for deeply-nested target, got: {module_paths:?}"
        );
        assert!(
            !module_paths.contains(&"tests"),
            "expected 'tests' module to be dropped, got: {module_paths:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Insights flag parsing tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_args_insights_flag() {
        let config = parse_args(&["--insights".to_string()]).unwrap();
        assert!(
            config.insights,
            "--insights should set config.insights=true"
        );
    }

    #[test]
    fn test_parse_args_insights_with_json() {
        let config = parse_args(&["--insights".to_string(), "--json".to_string()]).unwrap();
        assert!(config.insights, "--insights should be set");
        assert!(config.format_json, "--json should be set");
    }

    #[test]
    fn test_parse_args_insights_default_false() {
        let config = parse_args(&[]).unwrap();
        assert!(!config.insights, "insights should default to false");
    }
}
