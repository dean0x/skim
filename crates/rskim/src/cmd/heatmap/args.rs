//! CLI argument parsing for `skim heatmap`.
//!
//! Provides [`parse_args`] (table-driven flag dispatch) and [`print_help`].

use super::types::HeatmapConfig;
use std::time::UNIX_EPOCH;

// ============================================================================
// Table-driven value flag dispatch
// ============================================================================

/// Identifies which config field a value-taking flag maps to.
enum ValueFlagAction {
    Since,
    Path,
    TopN,
    Window,
    LastN,
    Exclude,
    CouplingThreshold,
    FixWindow,
    Format,
    Diff,
}

struct ValueFlagSpec {
    name: &'static str,
    action: ValueFlagAction,
}

const VALUE_FLAG_TABLE: &[ValueFlagSpec] = &[
    ValueFlagSpec {
        name: "--since",
        action: ValueFlagAction::Since,
    },
    ValueFlagSpec {
        name: "--path",
        action: ValueFlagAction::Path,
    },
    ValueFlagSpec {
        name: "--top",
        action: ValueFlagAction::TopN,
    },
    ValueFlagSpec {
        name: "--window",
        action: ValueFlagAction::Window,
    },
    ValueFlagSpec {
        name: "--last",
        action: ValueFlagAction::LastN,
    },
    ValueFlagSpec {
        name: "--exclude",
        action: ValueFlagAction::Exclude,
    },
    ValueFlagSpec {
        name: "--coupling-threshold",
        action: ValueFlagAction::CouplingThreshold,
    },
    ValueFlagSpec {
        name: "--fix-window",
        action: ValueFlagAction::FixWindow,
    },
    ValueFlagSpec {
        name: "--format",
        action: ValueFlagAction::Format,
    },
    ValueFlagSpec {
        name: "--diff",
        action: ValueFlagAction::Diff,
    },
];

/// Apply a value-taking flag to `config`. Each arm preserves exact validation
/// logic and error messages from the original flag-by-flag implementation.
fn apply_value_flag(
    config: &mut HeatmapConfig,
    action: &ValueFlagAction,
    val: String,
) -> anyhow::Result<()> {
    match action {
        ValueFlagAction::Since => {
            let ts = parse_since_value(&val)?;
            config.since = Some(ts);
        }
        ValueFlagAction::Path => {
            config.path = Some(val);
        }
        ValueFlagAction::TopN => {
            let n: usize = val
                .parse()
                .map_err(|_| anyhow::anyhow!("--top requires a positive integer"))?;
            if n == 0 {
                anyhow::bail!("--top must be at least 1");
            }
            config.top_n = n;
            config.top_explicit = true;
        }
        ValueFlagAction::Window => {
            config.window_preset = Some(val);
        }
        ValueFlagAction::LastN => {
            let n: usize = val
                .parse()
                .map_err(|_| anyhow::anyhow!("--last requires a positive integer"))?;
            if n == 0 {
                anyhow::bail!("--last must be at least 1");
            }
            config.last_n = Some(n);
        }
        ValueFlagAction::Exclude => {
            config.extra_excludes.push(val);
        }
        ValueFlagAction::CouplingThreshold => {
            config.coupling_threshold = val
                .parse::<f64>()
                .map_err(|_| {
                    anyhow::anyhow!("--coupling-threshold requires a float between 0 and 1")
                })?
                .clamp(0.0, 1.0);
        }
        ValueFlagAction::FixWindow => {
            let n: usize = val
                .parse()
                .map_err(|_| anyhow::anyhow!("--fix-window requires a positive integer"))?;
            if n == 0 {
                anyhow::bail!("--fix-window must be at least 1");
            }
            config.fix_window = n;
        }
        ValueFlagAction::Format => {
            if val == "json" {
                config.format_json = true;
            } else {
                anyhow::bail!("--format only supports 'json', got: {val}");
            }
        }
        ValueFlagAction::Diff => {
            config.diff_base = Some(val);
        }
    }
    Ok(())
}

// ============================================================================
// Public API
// ============================================================================

/// Parse CLI args into `HeatmapConfig`.
///
/// Follows the manual flag-parsing pattern used by `stats.rs` and `discover.rs`.
/// Initialises `config.debug` from the process-wide debug flag so that
/// `SKIM_DEBUG=1` (initialised by `main()` before dispatch) is honoured automatically.
pub(super) fn parse_args(args: &[String]) -> anyhow::Result<HeatmapConfig> {
    let mut config = HeatmapConfig {
        // Inherit SKIM_DEBUG / --debug flag set by main() before subcommand dispatch.
        debug: crate::debug::is_debug_enabled(),
        ..HeatmapConfig::default()
    };
    let mut i = 0;

    while i < args.len() {
        let arg = args[i].as_str();

        // Missing value pre-check: if this arg is a value-taking flag but there's
        // no next argument, bail with an actionable error before falling through to
        // "unknown flag".
        if VALUE_FLAG_TABLE.iter().any(|s| s.name == arg) && i + 1 >= args.len() {
            anyhow::bail!("{arg} requires a value");
        }

        // Table-driven value flag dispatch
        let mut matched = false;
        for spec in VALUE_FLAG_TABLE {
            if let Some(val) = extract_value(args, &mut i, spec.name) {
                apply_value_flag(&mut config, &spec.action, val)?;
                matched = true;
                break;
            }
        }
        if matched {
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
pub(super) fn apply_boolean_flag(config: &mut HeatmapConfig, flag: &str) -> anyhow::Result<bool> {
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
pub(super) fn extract_value(args: &[String], i: &mut usize, flag: &str) -> Option<String> {
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
pub(super) fn parse_since_value(val: &str) -> anyhow::Result<u64> {
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

pub(super) fn print_help() {
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
    use std::time::{SystemTime, UNIX_EPOCH};

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
