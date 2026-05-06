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
mod metrics;
mod output;
mod types;

use std::collections::HashSet;
use std::io::{self, Write};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::analytics::{CommandType, RecordingContext};

use excludes::{build_exclude_set, should_exclude};
use git_source::{CliGitSource, GitDataSource};
use metrics::{
    build_fix_regex, compute_authors, compute_churn, compute_coupling, compute_encapsulation,
    compute_fix_after_touch, compute_stability,
};
use output::{render_json, render_text};
use types::{
    AuthorMetrics, ChurnMetrics, FileMetrics, FixRiskMetrics, HeatmapConfig, HeatmapResult,
    WindowInfo,
};

// ============================================================================
// Window presets
// ============================================================================

/// Map a named preset to `--since` epoch seconds offset.
fn preset_to_since_secs(preset: &str) -> Option<u64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    match preset {
        "sprint" => Some(now.saturating_sub(14 * 86400)),
        "month" => Some(now.saturating_sub(30 * 86400)),
        "quarter" => Some(now.saturating_sub(90 * 86400)),
        "half" => Some(now.saturating_sub(180 * 86400)),
        "year" => Some(now.saturating_sub(365 * 86400)),
        "all" => Some(0),
        _ => None,
    }
}

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

    let config = match parse_args(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skim heatmap: {e}");
            eprintln!("Run `skim heatmap --help` for usage.");
            return Ok(ExitCode::FAILURE);
        }
    };

    let git_source = CliGitSource::new();
    run_with_source(&git_source, &git_source, &config, analytics)
}

/// Orchestration with injected data source (enables testing).
///
/// `git` handles infrastructure checks (repo detection, root, shallow clone,
/// commit count). `source` handles the actual commit fetch and may be a test double.
fn run_with_source(
    git: &CliGitSource,
    source: &dyn GitDataSource,
    config: &HeatmapConfig,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    // Step 1: Validate git environment
    if !git.is_git_repo() {
        eprintln!("skim heatmap: Not a git repository");
        return Ok(ExitCode::FAILURE);
    }

    let repo_root = match git.get_repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("skim heatmap: {e}");
            return Ok(ExitCode::FAILURE);
        }
    };

    let mut warnings: Vec<String> = Vec::new();

    if git.detect_shallow_clone() {
        warnings.push(
            "Shallow clone detected — history may be incomplete, metrics may be skewed."
                .to_string(),
        );
    }

    if config.debug {
        eprintln!("[heatmap] repo root: {repo_root}");
    }

    // Step 2: Resolve effective config (presets and --last)
    let effective_config = resolve_effective_config(config, git, &mut warnings)?;

    if config.debug {
        let mode = if effective_config.dual_mode {
            "dual"
        } else if config.last_n.is_some() {
            "count"
        } else {
            "time"
        };
        eprintln!("[heatmap] window mode: {mode}");
        if let Some(since) = effective_config.since {
            eprintln!("[heatmap] since epoch: {since} ({})", format_epoch(since));
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
            return Ok(ExitCode::FAILURE);
        }
    };

    if config.debug {
        eprintln!("[heatmap] raw commits fetched: {}", raw_commits.len());
    }

    if raw_commits.is_empty() {
        eprintln!("skim heatmap: No commits found in repository");
        return Ok(ExitCode::FAILURE);
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
            "[heatmap] commits after exclusion: {} ({} excluded)",
            commits.len(),
            raw_commit_count - commits.len()
        );
    }

    if commits.is_empty() {
        eprintln!("skim heatmap: No analyzable files after exclusions");
        return Ok(ExitCode::FAILURE);
    }

    // Step 5: Compute metrics
    let start_time = std::time::Instant::now();
    let fix_regex = build_fix_regex();
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let churn_map = compute_churn(&commits);
    let max_churn = churn_map.values().map(|m| m.commits).max().unwrap_or(1);
    let stability_map = compute_stability(&commits, &fix_regex, max_churn, now_epoch);
    let author_map = compute_authors(&commits);
    let fix_risk_map = compute_fix_after_touch(&commits, &fix_regex, config.fix_window);
    let (blast_radius_map, coupling_graph) =
        compute_coupling(&commits, config.coupling_threshold, 3);
    let modules = compute_encapsulation(&commits, 3);

    if config.debug {
        let compute_elapsed = start_time.elapsed();
        eprintln!(
            "[heatmap] metrics computed in {:.1}ms — {} files, {} coupling edges, {} modules",
            compute_elapsed.as_secs_f64() * 1000.0,
            churn_map.len(),
            coupling_graph.len(),
            modules.len(),
        );
    }

    // Step 6: Assemble FileMetrics
    let mut all_paths: HashSet<String> = HashSet::new();
    for commit in &commits {
        for f in &commit.files {
            all_paths.insert(f.path.clone());
        }
    }

    let mut file_metrics: Vec<FileMetrics> = all_paths
        .into_iter()
        .map(|path| {
            let churn = churn_map.get(&path).cloned().unwrap_or(ChurnMetrics {
                commits: 0,
                rate: 0.0,
            });
            let stability_score = stability_map.get(&path).copied().unwrap_or(100);
            let authors = author_map.get(&path).cloned().unwrap_or(AuthorMetrics {
                count: 0,
                top_author_pct: 0.0,
                single_owner_risk: false,
            });
            let fix_risk = fix_risk_map.get(&path).cloned().unwrap_or(FixRiskMetrics {
                keyword_pct: 0.0,
                proximity_pct: 0.0,
                combined_pct: 0.0,
                insufficient_data: true,
            });
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
    file_metrics.sort_by(|a, b| a.stability_score.cmp(&b.stability_score));

    // Step 7: Build window info
    let window_info = build_window_info(&effective_config, commits.len());

    // Step 8: Get excluded patterns for output
    let excluded_patterns: Vec<String> = if config.no_exclude {
        Vec::new()
    } else {
        excludes::DEFAULT_EXCLUDES
            .iter()
            .map(|s| s.to_string())
            .chain(config.extra_excludes.iter().cloned())
            .collect()
    };

    // Step 9: Build result
    let result = HeatmapResult {
        version: 1,
        generated_at: format_epoch(now_epoch),
        repository: repo_root,
        window: window_info,
        files: file_metrics,
        modules,
        coupling_graph,
        excluded_patterns,
        warnings,
    };

    // Step 10: Render
    let elapsed = start_time.elapsed();
    let mut stdout = io::stdout().lock();

    if config.format_json {
        let json = render_json(&result)?;
        writeln!(stdout, "{json}")?;
    } else {
        let text = render_text(&result, config.top_n);
        write!(stdout, "{text}")?;
    }

    // Step 11: Fire-and-forget analytics
    let rec = RecordingContext {
        enabled: analytics.enabled,
        command_type: CommandType::Heatmap,
        parse_tier: None,
        session_id: analytics.session_id.as_deref(),
    };
    crate::analytics::try_record_command(
        rec,
        String::new(), // no raw text for heatmap
        String::new(), // no compressed text
        "skim heatmap".to_string(),
        elapsed,
    );

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Config resolution
// ============================================================================

/// Resolve the effective `HeatmapConfig` by applying presets and `--last`.
///
/// Precedence: `--since` > `--last` > `--window` preset > dual default.
fn resolve_effective_config(
    config: &HeatmapConfig,
    source: &CliGitSource,
    warnings: &mut Vec<String>,
) -> anyhow::Result<HeatmapConfig> {
    let mut effective = config.clone();

    // Count explicit time-selection flags
    let explicit_count = [
        config.since.is_some(),
        config.last_n.is_some(),
        config.window_preset.is_some(),
    ]
    .iter()
    .filter(|&&b| b)
    .count();

    if explicit_count > 1 {
        warnings.push(
            "Multiple window flags specified — using first (--since > --last > --window)."
                .to_string(),
        );
    }

    if let Some(since) = config.since {
        // Already set — highest precedence
        effective.since = Some(since);
    } else if let Some(n) = config.last_n {
        // --last N: find the timestamp of the Nth commit
        match source.fetch_commit_count_since(n) {
            Ok(Some(ts)) => {
                effective.since = Some(ts);
            }
            Ok(None) => {
                warnings.push(format!(
                    "Repository has fewer than {n} commits — analyzing all history."
                ));
            }
            Err(e) => {
                warnings.push(format!("Could not resolve --last {n}: {e}"));
            }
        }
    } else if let Some(ref preset) = config.window_preset {
        if let Some(since) = preset_to_since_secs(preset) {
            effective.since = Some(since);
        } else {
            warnings.push(format!(
                "Unknown window preset '{preset}' — valid: sprint, month, quarter, half, year, all. Analyzing all history."
            ));
        }
    } else {
        // Dual default: max(last 90 days, last 200 commits)
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let time_since = now.saturating_sub(90 * 86400);

        let count_since = match source.fetch_commit_count_since(200) {
            Ok(Some(ts)) => ts,
            _ => time_since, // fallback to time-based if lookup fails
        };

        // Use whichever captures more history (lower epoch = more history)
        effective.since = Some(time_since.min(count_since));
        effective.dual_mode = true;
        effective.dual_time_since = Some(time_since);
        effective.dual_count_since = Some(count_since);
    }

    Ok(effective)
}

// ============================================================================
// Window info construction
// ============================================================================

fn build_window_info(config: &HeatmapConfig, commits_analyzed: usize) -> WindowInfo {
    let mode = if config.dual_mode {
        "dual".to_string()
    } else if let Some(ref preset) = config.window_preset {
        preset.clone()
    } else if config.last_n.is_some() {
        "count".to_string()
    } else if config.since.is_some() {
        "time".to_string()
    } else {
        "dual".to_string()
    };

    let since_str = config
        .since
        .map(format_epoch)
        .unwrap_or_else(|| "all".to_string());

    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let (effective_strategy, time_commits, count_commits) = if config.dual_mode {
        let time_since = config.dual_time_since.unwrap_or(0);
        let count_since = config.dual_count_since.unwrap_or(0);
        let strategy = if time_since <= count_since {
            "time"
        } else {
            "count"
        };
        (Some(strategy.to_string()), None, None)
    } else {
        (None, None, None)
    };

    WindowInfo {
        mode,
        since: since_str,
        until: format_epoch(now_epoch),
        commits_analyzed,
        time_commits,
        count_commits,
        effective_strategy,
    }
}

// ============================================================================
// Formatting helpers
// ============================================================================

/// Format a Unix epoch as a simple date string (YYYY-MM-DD).
fn format_epoch(epoch: u64) -> String {
    // Manual calculation — no chrono dependency
    // Days since 1970-01-01
    let days = epoch / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Convert days since 1970-01-01 to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // year of era
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // day of year [0, 365]
    let mp = (5 * doy + 2) / 153; // month prime
    let d = doy - (153 * mp + 2) / 5 + 1; // day [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // month [1, 12]
    let y_adj = if m <= 2 { y + 1 } else { y };
    (y_adj, m, d)
}

// ============================================================================
// Argument parsing
// ============================================================================

/// Parse CLI args into `HeatmapConfig`.
///
/// Follows the manual flag-parsing pattern used by `stats.rs`.
fn parse_args(args: &[String]) -> anyhow::Result<HeatmapConfig> {
    let mut config = HeatmapConfig::default();
    let mut i = 0;

    while i < args.len() {
        let arg = args[i].as_str();

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
            config.top_n = val
                .parse()
                .map_err(|_| anyhow::anyhow!("--top requires a positive integer"))?;
            continue;
        }

        // --window
        if let Some(val) = extract_value(args, &mut i, "--window") {
            config.window_preset = Some(val);
            continue;
        }

        // --last
        if let Some(val) = extract_value(args, &mut i, "--last") {
            config.last_n = Some(
                val.parse()
                    .map_err(|_| anyhow::anyhow!("--last requires a positive integer"))?,
            );
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
            config.fix_window = val
                .parse()
                .map_err(|_| anyhow::anyhow!("--fix-window requires a positive integer"))?;
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

        // Boolean flags
        match arg {
            "--json" => config.format_json = true,
            "--no-exclude" => config.no_exclude = true,
            "--debug" => config.debug = true,
            other => {
                if other.starts_with('-') {
                    anyhow::bail!("unknown flag: {other}");
                }
            }
        }

        i += 1;
    }

    Ok(config)
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
    skim heatmap [OPTIONS]

OPTIONS:
    --since <VALUE>               Analyze commits since epoch (seconds) or duration (30d, 2w, 24h)
    --last <N>                    Analyze last N commits
    --window <PRESET>             Named window: sprint|month|quarter|half|year|all
    --path <DIR>                  Scope analysis to files under this path
    --json                        Output JSON instead of human-readable text
    --top <N>                     Maximum files to display (default: 20)
    --no-exclude                  Disable default exclusion patterns (lock files, build dirs, etc.)
    --exclude <PATTERN>           Add extra glob pattern to exclude (repeatable)
    --coupling-threshold <FLOAT>  Coupling confidence threshold (default: 0.5)
    --fix-window <N>              Proximity window for fix-after-touch detection (default: 5)
    --debug                       Enable debug output
    -h, --help                    Show this help message

WINDOW PRESETS:
    sprint     14 days
    month      30 days
    quarter    90 days
    half       180 days
    year       365 days
    all        No time limit (analyze entire history)

EXAMPLES:
    skim heatmap                           # Analyze last 90 days
    skim heatmap --last 200                # Analyze last 200 commits
    skim heatmap --window sprint           # Analyze last sprint
    skim heatmap --since 30d               # Analyze last 30 days
    skim heatmap --json                    # JSON output
    skim heatmap --path src/               # Scope to src/ directory
    skim heatmap --no-exclude              # Include lock files and build artifacts
    skim heatmap --coupling-threshold 0.3  # Lower coupling threshold

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
    fn test_format_epoch_known_date() {
        // 2024-01-01 = 1704067200
        assert_eq!(format_epoch(1_704_067_200), "2024-01-01");
    }

    #[test]
    fn test_format_epoch_unix_epoch() {
        assert_eq!(format_epoch(0), "1970-01-01");
    }

    #[test]
    fn test_preset_to_since_secs_sprint() {
        let since = preset_to_since_secs("sprint").unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let diff = now - since;
        assert!(diff >= 13 * 86400 && diff <= 15 * 86400);
    }

    #[test]
    fn test_preset_to_since_secs_unknown() {
        assert!(preset_to_since_secs("unknown-preset").is_none());
    }

    #[test]
    fn test_days_to_ymd_epoch() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn test_days_to_ymd_known_date() {
        // 2024-01-01 = 19723 days since epoch
        assert_eq!(days_to_ymd(19723), (2024, 1, 1));
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
}
