//! CLI argument parsing for `skim heatmap`.
//!
//! Provides [`parse_args`] (table-driven flag dispatch) and [`print_help`].

use super::types::{DetailSection, HeatmapArgs};
use std::time::UNIX_EPOCH;

// ============================================================================
// Table-driven value flag dispatch
// ============================================================================

/// Identifies which args field a value-taking flag maps to.
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
    Detail,
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
    ValueFlagSpec {
        name: "--detail",
        action: ValueFlagAction::Detail,
    },
];

/// Apply a value-taking flag to `args`. Each arm preserves exact validation
/// logic and error messages from the original flag-by-flag implementation.
fn apply_value_flag(
    args: &mut HeatmapArgs,
    action: &ValueFlagAction,
    val: String,
) -> anyhow::Result<()> {
    match action {
        ValueFlagAction::Since => {
            let ts = parse_since_value(&val)?;
            args.since = Some(ts);
        }
        ValueFlagAction::Path => {
            args.path = Some(val);
        }
        ValueFlagAction::TopN => {
            let n: usize = val
                .parse()
                .map_err(|_| anyhow::anyhow!("--top requires a positive integer"))?;
            if n == 0 {
                anyhow::bail!("--top must be at least 1");
            }
            args.top_n = n;
            args.top_explicit = true;
        }
        ValueFlagAction::Window => {
            args.window_preset = Some(val);
        }
        ValueFlagAction::LastN => {
            let n: usize = val
                .parse()
                .map_err(|_| anyhow::anyhow!("--last requires a positive integer"))?;
            if n == 0 {
                anyhow::bail!("--last must be at least 1");
            }
            args.last_n = Some(n);
        }
        ValueFlagAction::Exclude => {
            args.extra_excludes.push(val);
        }
        ValueFlagAction::CouplingThreshold => {
            args.coupling_threshold = val
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
            args.fix_window = n;
        }
        ValueFlagAction::Format => {
            if val == "json" {
                args.format_json = true;
            } else {
                anyhow::bail!("--format only supports 'json', got: {val}");
            }
        }
        ValueFlagAction::Diff => {
            // Heatmap flag values reach git as values only: a value that
            // starts with `-` is not a branch or commit name.
            if val.starts_with('-') {
                anyhow::bail!("--diff value must not start with '-': {val}");
            }
            args.diff_base = Some(val);
        }
        ValueFlagAction::Detail => {
            if args.detail.is_some() {
                anyhow::bail!("--detail can be given only once");
            }
            let Some(section) = DetailSection::from_name(&val) else {
                let names = DetailSection::ALL.map(DetailSection::name).join(", ");
                anyhow::bail!("--detail must be one of {names}; got: {val}");
            };
            args.detail = Some(section);
        }
    }
    Ok(())
}

// ============================================================================
// Public API
// ============================================================================

/// Parse CLI args into `HeatmapArgs`.
///
/// Follows the manual flag-parsing pattern used by `stats.rs` and `discover.rs`.
/// Initialises `args.debug` from the process-wide debug flag so that
/// `SKIM_DEBUG=1` (initialised by `main()` before dispatch) is honoured automatically.
pub(super) fn parse_args(raw: &[String]) -> anyhow::Result<HeatmapArgs> {
    let mut args = HeatmapArgs {
        // Inherit SKIM_DEBUG / --debug flag set by main() before subcommand dispatch.
        debug: crate::debug::is_debug_enabled(),
        ..HeatmapArgs::default()
    };
    let mut i = 0;

    while i < raw.len() {
        let arg = raw[i].as_str();

        // Missing value pre-check: if this arg is a value-taking flag but there's
        // no next argument, bail with an actionable error before falling through to
        // "unknown flag".
        if VALUE_FLAG_TABLE.iter().any(|s| s.name == arg) && i + 1 >= raw.len() {
            anyhow::bail!("{arg} requires a value");
        }

        // Table-driven value flag dispatch
        let mut matched = false;
        for spec in VALUE_FLAG_TABLE {
            if let Some(val) = extract_value(raw, &mut i, spec.name) {
                apply_value_flag(&mut args, &spec.action, val)?;
                matched = true;
                break;
            }
        }
        if matched {
            continue;
        }

        // Boolean flags
        if apply_boolean_flag(&mut args, arg)? {
            i += 1;
            continue;
        }

        if arg.starts_with('-') {
            anyhow::bail!("unknown flag: {arg}");
        }
        // Positional (non-flag) argument — file target.
        args.files.push(arg.to_string());
        i += 1;
    }

    // Post-parse validation
    if args.diff_base.is_some() && !args.files.is_empty() {
        anyhow::bail!("cannot combine --diff with explicit file arguments");
    }
    // `--detail` with `--diff` is allowed: the diff only scopes the files.
    if args.detail.is_some() {
        if args.insights {
            anyhow::bail!("cannot combine --detail with --insights");
        }
        if !args.files.is_empty() {
            anyhow::bail!("cannot combine --detail with explicit file arguments");
        }
    }

    // File arguments stay as typed: `prepare_commits` normalises them
    // against the repository root.
    Ok(args)
}

/// Apply a recognised boolean flag to `args`.
///
/// Returns `Ok(true)` if the flag was recognised and applied, `Ok(false)` if
/// the flag is unknown (caller falls through to the unknown-flag error).
fn apply_boolean_flag(args: &mut HeatmapArgs, flag: &str) -> anyhow::Result<bool> {
    match flag {
        "--json" => args.format_json = true,
        "--no-exclude" => args.no_exclude = true,
        "--insights" => args.insights = true,
        "--debug" => {
            args.debug = true;
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

pub(super) fn print_help() {
    print!(
        "\
skim heatmap — git history risk and coupling analysis

USAGE:
    skim heatmap [OPTIONS]                Short ranked view (30 lines or fewer)
    skim heatmap --detail <SECTION>       One full section
    skim heatmap --insights               Ranked CRITICAL/WARNING findings
    skim heatmap [OPTIONS] <FILE>...      One \"before you edit\" card per file

OPTIONS:
    --since <VALUE>               Analyze commits since epoch (seconds) or duration (30d, 2w, 24h)
    --last <N>                    Analyze last N commits
    --window <PRESET>             Named window: sprint|month|quarter|half|year|all
    --path <DIR>                  Scope analysis to files under this path
    --diff <BASE>                 Default view for the files changed vs BASE (three-dot diff)
    --detail <SECTION>            One full section: hotspots|coupling|fix-risk|modules|bus-factor
    --top <N>                     Rows of each list in every view (replaces the caps below)
    --json, --format json         Output JSON (version 2: one compact line, capped, with counts)
    --no-exclude                  Keep the files of the default excludes (see DEFAULT EXCLUDES)
    --exclude <PATTERN>           Add extra glob pattern to exclude (repeatable)
    --coupling-threshold <FLOAT>  Coupling confidence threshold (default: 0.5)
    --fix-window <N>              Proximity window for fix-after-touch detection (default: 5)
    --insights                    Show ranked findings only
    --debug                       Enable debug output
    -h, --help                    Show this help message

DEFAULT VIEW:
    A header line (repository name, since..until dates, commits, window), then:
    Hotspots     the 10 files with the most commits
    Coupling     the 5 code-to-code pairs with the highest confidence
    Fix risk     5 files whose fix rate is above the repository base rate
    A capped section ends with one \"N more (--detail <section>)\" line.
    Module health and bus factor show only with --detail.

SECTIONS (--detail names):
    hotspots     Files changed most often (commits)
    coupling     Code-to-code file pairs that change together (no doc files)
    fix-risk     Fix commits/commits, lift and base rate, for files above the base rate
    modules      Encapsulation of each crate, package (Cargo.toml, package.json) or top folder
    bus-factor   Files with one dominant author (more than 80% of commits)

CAPS (--top N replaces each of them):
    Default view     hotspots 10, coupling 5, fix risk 5
    --detail X       all rows of X
    --insights       20 findings, then one \"N more (raise --top)\" line
    File cards       5 cards; per card 3 owners, 4 partners, 3 fix commits
                     (fix commits stay at 5 or fewer at any --top)

NOISE RULES:
    Fix risk and bus factor need 5 commits of the file. Fix risk also needs
    the 95% lower bound of the file's fix rate to be above the base rate.
    Coupling needs 3 shared commits. Modules need 5 commits. Bus factor shows
    only when the window has 2 or more authors. Commits with more than 50
    files are not paired; a warning line gives their count. Insights follow
    the same minimums and rank by severity, score, support, then path.

FILE CARDS:
    skim heatmap <FILE>... prints one card per file: commits, owners (only
    with 2 or more authors), co-change partners with 3 or more shared commits
    (tests first), and fix commits from history, newest first. A file outside
    the window prints \"not in history window\". --insights <FILE> prints the
    same card. ./x, an absolute path under the repository root and a path
    relative to the current folder give the same card; a path outside the
    repository exits 1. Metrics come from the full window; a card only
    narrows the display.

FILE TARGETING:
    --path scopes the git log itself (commit-level filter); it composes with
    file arguments. --diff and file arguments are mutually exclusive.
    --detail takes no file arguments and does not combine with --insights.

DEFAULT EXCLUDES (--no-exclude keeps them):
    Lock files, build and dist folders, minified bundles, logs, vendor and
    node_modules, coverage output, and at any depth: CHANGELOG.md, *.snap,
    snapshots/, *.pb.go, *_pb2.py and generated/ folders. A file marked
    \"generated\" only in a header line stays in.

JSON (version 2):
    One compact line. Each array of rows has the cap of its section, and
    counts.<section>.{{total,shown}} gives the full and shown counts. files[]
    holds the hotspot rows; no support minimum applies to it. With file
    arguments, cards[] holds the cards. file_targets[] holds at most 20
    targets, and counts.file_targets gives their full count. warnings[] has
    at most one line for each kind of warning. excluded_patterns[] is the
    fixed pattern list: the default excludes and the --exclude values.
    --insights --json gives insights[],
    top_files[] and flagged_modules[] for the shown findings only, and the
    same warnings[] as --json.

WINDOW PRESETS:
    sprint     14 days
    month      30 days
    quarter    90 days
    half       180 days
    year       365 days
    all        No time limit (analyze entire history)
    (default)  The wider of the last 90 days and the last 200 commits

EXAMPLES:
    skim heatmap                              # Short ranked view
    skim heatmap --detail coupling            # All coupling pairs
    skim heatmap --detail fix-risk --top 20   # 20 fix-risk rows
    skim heatmap --top 3                      # 3 rows per section
    skim heatmap src/main.rs                  # Card for one file before you edit it
    skim heatmap --diff main                  # Default view for files changed vs main
    skim heatmap --insights                   # Top 20 ranked findings
    skim heatmap --insights --top 50 --json   # Top 50 findings as JSON
    skim heatmap --last 200                   # Analyze last 200 commits
    skim heatmap --window sprint              # Analyze last sprint
    skim heatmap --path src/                  # Scope to src/ directory
    skim heatmap --no-exclude                 # Keep lock files, changelogs and snapshots
"
    );
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn test_parse_args_defaults() {
        let args = parse_args(&[]).unwrap();
        assert_eq!(args.top_n, 20);
        assert!((args.coupling_threshold - 0.5).abs() < 1e-9);
        assert_eq!(args.fix_window, 5);
        assert!(!args.format_json);
        assert!(!args.no_exclude);
    }

    #[test]
    fn test_parse_args_json_flag() {
        let args = parse_args(&["--json".to_string()]).unwrap();
        assert!(args.format_json);
    }

    #[test]
    fn test_parse_args_top_n() {
        let args = parse_args(&["--top".to_string(), "5".to_string()]).unwrap();
        assert_eq!(args.top_n, 5);
    }

    #[test]
    fn test_parse_args_top_n_equals() {
        let args = parse_args(&["--top=10".to_string()]).unwrap();
        assert_eq!(args.top_n, 10);
    }

    #[test]
    fn test_parse_args_window_preset() {
        let args = parse_args(&["--window=sprint".to_string()]).unwrap();
        assert_eq!(args.window_preset.as_deref(), Some("sprint"));
    }

    #[test]
    fn test_parse_args_last_n() {
        let args = parse_args(&["--last=100".to_string()]).unwrap();
        assert_eq!(args.last_n, Some(100));
    }

    #[test]
    fn test_parse_args_no_exclude() {
        let args = parse_args(&["--no-exclude".to_string()]).unwrap();
        assert!(args.no_exclude);
    }

    #[test]
    fn test_parse_args_coupling_threshold() {
        let args = parse_args(&["--coupling-threshold=0.3".to_string()]).unwrap();
        assert!((args.coupling_threshold - 0.3).abs() < 1e-9);
    }

    #[test]
    fn test_parse_args_since_epoch() {
        let args = parse_args(&["--since=1700000000".to_string()]).unwrap();
        assert_eq!(args.since, Some(1_700_000_000));
    }

    #[test]
    fn test_parse_args_since_duration() {
        let args = parse_args(&["--since=30d".to_string()]).unwrap();
        // Should be set to some epoch in the past
        assert!(args.since.is_some());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let since = args.since.unwrap();
        let diff = now - since;
        assert!((29 * 86400..=31 * 86400).contains(&diff));
    }

    #[test]
    fn test_parse_args_path() {
        let args = parse_args(&["--path=src/".to_string()]).unwrap();
        assert_eq!(args.path.as_deref(), Some("src/"));
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
        let args = parse_args(&["src/main.rs".to_string(), "src/lib.rs".to_string()]).unwrap();
        assert_eq!(args.files, vec!["src/main.rs", "src/lib.rs"]);
    }

    #[test]
    fn test_parse_args_diff_flag() {
        let args = parse_args(&["--diff".to_string(), "main".to_string()]).unwrap();
        assert_eq!(args.diff_base, Some("main".to_string()));
    }

    #[test]
    fn test_parse_args_diff_equals() {
        let args = parse_args(&["--diff=develop".to_string()]).unwrap();
        assert_eq!(args.diff_base, Some("develop".to_string()));
    }

    #[test]
    fn test_parse_args_diff_dash_value_space_form_errors() {
        let result = parse_args(&["--diff".to_string(), "--stat".to_string()]);
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("--diff value must not start with '-'"),
            "got: {msg}"
        );
    }

    #[test]
    fn test_parse_args_diff_dash_value_equals_form_errors() {
        let result = parse_args(&["--diff=-x".to_string()]);
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("--diff value must not start with '-'"),
            "got: {msg}"
        );
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
    fn test_parse_args_keeps_file_arguments_as_typed() {
        // The card normalises paths against the repository root later.
        let args = parse_args(&["./src/main.rs".to_string()]).unwrap();
        assert_eq!(args.files, vec!["./src/main.rs"]);
    }

    #[test]
    fn test_parse_args_top_explicit_flag() {
        let args = parse_args(&["--top".to_string(), "5".to_string()]).unwrap();
        assert!(args.top_explicit);
    }

    #[test]
    fn test_parse_args_top_implicit_by_default() {
        let args = parse_args(&[]).unwrap();
        assert!(!args.top_explicit);
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
        let args = parse_args(&[
            "--since".to_string(),
            "30d".to_string(),
            "src/main.rs".to_string(),
            "--json".to_string(),
        ])
        .unwrap();
        assert_eq!(args.files, vec!["src/main.rs"]);
        assert!(args.format_json);
        assert!(args.since.is_some());
    }

    #[test]
    fn test_parse_args_extra_exclude() {
        let args = parse_args(&["--exclude=*.generated.ts".to_string()]).unwrap();
        assert_eq!(args.extra_excludes, vec!["*.generated.ts"]);
    }

    #[test]
    fn test_parse_args_fix_window() {
        let args = parse_args(&["--fix-window=10".to_string()]).unwrap();
        assert_eq!(args.fix_window, 10);
    }

    // -----------------------------------------------------------------------
    // Insights flag parsing tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_parse_args_insights_flag() {
        let args = parse_args(&["--insights".to_string()]).unwrap();
        assert!(args.insights, "--insights should set args.insights=true");
    }

    #[test]
    fn test_parse_args_insights_with_json() {
        let args = parse_args(&["--insights".to_string(), "--json".to_string()]).unwrap();
        assert!(args.insights, "--insights should be set");
        assert!(args.format_json, "--json should be set");
    }

    #[test]
    fn test_parse_args_insights_default_false() {
        let args = parse_args(&[]).unwrap();
        assert!(!args.insights, "insights should default to false");
    }

    // -----------------------------------------------------------------------
    // --detail
    // -----------------------------------------------------------------------

    fn strings(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| s.to_string()).collect()
    }

    fn parse_error(raw: &[&str]) -> String {
        parse_args(&strings(raw)).unwrap_err().to_string()
    }

    #[test]
    fn test_parse_args_detail_accepts_every_section_name() {
        let expected = [
            ("hotspots", DetailSection::Hotspots),
            ("coupling", DetailSection::Coupling),
            ("fix-risk", DetailSection::FixRisk),
            ("modules", DetailSection::Modules),
            ("bus-factor", DetailSection::BusFactor),
        ];
        for (name, section) in expected {
            let space = parse_args(&strings(&["--detail", name])).unwrap();
            assert_eq!(space.detail, Some(section), "--detail {name}");
            let equals = parse_args(&strings(&[&format!("--detail={name}")])).unwrap();
            assert_eq!(equals.detail, Some(section), "--detail={name}");
        }
    }

    #[test]
    fn test_parse_args_detail_absent_by_default() {
        assert_eq!(parse_args(&[]).unwrap().detail, None);
    }

    #[test]
    fn test_parse_args_detail_unknown_name_lists_valid_names() {
        assert_eq!(
            parse_error(&["--detail", "churn"]),
            "--detail must be one of hotspots, coupling, fix-risk, modules, bus-factor; got: churn"
        );
    }

    #[test]
    fn test_parse_args_detail_repeated_errors() {
        assert_eq!(
            parse_error(&["--detail", "coupling", "--detail=modules"]),
            "--detail can be given only once"
        );
        assert_eq!(
            parse_error(&["--detail", "coupling", "--detail", "coupling"]),
            "--detail can be given only once"
        );
    }

    #[test]
    fn test_parse_args_detail_with_insights_errors() {
        assert_eq!(
            parse_error(&["--detail", "coupling", "--insights"]),
            "cannot combine --detail with --insights"
        );
    }

    #[test]
    fn test_parse_args_detail_with_file_arguments_errors() {
        assert_eq!(
            parse_error(&["src/main.rs", "--detail", "hotspots"]),
            "cannot combine --detail with explicit file arguments"
        );
    }

    #[test]
    fn test_parse_args_detail_with_diff_is_allowed() {
        let args = parse_args(&strings(&["--diff", "main", "--detail", "coupling"])).unwrap();
        assert_eq!(args.detail, Some(DetailSection::Coupling));
        assert_eq!(args.diff_base.as_deref(), Some("main"));
    }

    #[test]
    fn test_parse_args_detail_without_value_errors() {
        assert_eq!(parse_error(&["--detail"]), "--detail requires a value");
    }

    #[test]
    fn test_parse_args_detail_keeps_top_explicit() {
        let implicit = parse_args(&strings(&["--detail", "modules"])).unwrap();
        assert!(!implicit.top_explicit);
        let explicit = parse_args(&strings(&["--detail", "modules", "--top", "4"])).unwrap();
        assert!(explicit.top_explicit);
        assert_eq!(explicit.top_n, 4);
    }
}
