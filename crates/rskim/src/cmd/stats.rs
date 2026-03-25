//! Stats subcommand — token analytics dashboard (#56)
//!
//! Queries the analytics SQLite database and displays a summary of token
//! savings across all skim invocations. Supports time filtering (`--since`),
//! JSON output (`--format json`), cost estimates (`--cost`), and data clearing
//! (`--clear`).

use std::process::ExitCode;
use std::time::UNIX_EPOCH;

use colored::Colorize;

use crate::analytics::{AnalyticsDb, AnalyticsStore, PricingModel};
use crate::cmd::session::types::parse_duration_ago;
use crate::tokens;

// ============================================================================
// Public entry point
// ============================================================================

/// Run the `skim stats` subcommand.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Parse flags
    let clear = args.iter().any(|a| a == "--clear");
    let show_cost = args.iter().any(|a| a == "--cost");
    let format = parse_value_flag(args, "--format");
    let since_str = parse_value_flag(args, "--since");

    let db = AnalyticsDb::open_default()?;

    if clear {
        return run_clear(&db);
    }

    let since_ts = if let Some(s) = &since_str {
        let time = parse_duration_ago(s)?;
        let ts = time.duration_since(UNIX_EPOCH)?.as_secs() as i64;
        Some(ts)
    } else {
        None
    };

    if format.as_deref() == Some("json") {
        return run_json(&db, since_ts, show_cost);
    }

    run_dashboard(&db, since_ts, show_cost, since_str.as_deref())
}

// ============================================================================
// Flag parsing
// ============================================================================

/// Parse a `--flag value` or `--flag=value` pair from args.
fn parse_value_flag(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().cloned();
        }
        if let Some(val) = arg.strip_prefix(&format!("{flag}=")) {
            return Some(val.to_string());
        }
    }
    None
}

// ============================================================================
// Help
// ============================================================================

fn print_help() {
    println!("skim stats");
    println!();
    println!("  Show token analytics dashboard.");
    println!();
    println!("FLAGS:");
    println!("  --since <DURATION>    Filter to recent data (e.g., 7d, 24h, 4w)");
    println!("  --format json         Output as JSON");
    println!("  --cost                Show cost savings estimates");
    println!("  --clear               Delete all analytics data");
    println!();
    println!("EXAMPLES:");
    println!("  skim stats                   Show all-time summary");
    println!("  skim stats --since 7d        Last 7 days");
    println!("  skim stats --format json     Machine-readable output");
    println!("  skim stats --cost            Include cost estimates");
    println!("  skim stats --clear           Reset analytics data");
    println!();
    println!("ENVIRONMENT:");
    println!("  SKIM_INPUT_COST_PER_MTOK     Override $/MTok for cost estimates (default: 3.0)");
    println!("  SKIM_ANALYTICS_DB            Override analytics database path");
    println!("  SKIM_DISABLE_ANALYTICS       Set to disable analytics recording");
}

// ============================================================================
// Clear
// ============================================================================

fn run_clear(db: &dyn AnalyticsStore) -> anyhow::Result<ExitCode> {
    db.clear()?;
    println!("Analytics data cleared.");
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// JSON output
// ============================================================================

fn run_json(db: &dyn AnalyticsStore, since: Option<i64>, show_cost: bool) -> anyhow::Result<ExitCode> {
    let summary = db.query_summary(since)?;
    let daily = db.query_daily(since)?;
    let by_command = db.query_by_command(since)?;
    let by_language = db.query_by_language(since)?;
    let by_mode = db.query_by_mode(since)?;
    let tier_dist = db.query_tier_distribution(since)?;

    let mut root = serde_json::json!({
        "summary": summary,
        "daily": daily,
        "by_command": by_command,
        "by_language": by_language,
        "by_mode": by_mode,
        "tier_distribution": tier_dist,
    });

    if show_cost {
        let pricing = PricingModel::from_env_or_default();
        let cost_savings = pricing.estimate_savings(summary.tokens_saved);
        root.as_object_mut().unwrap().insert(
            "cost_estimate".to_string(),
            serde_json::json!({
                "model": pricing.model_name,
                "input_cost_per_mtok": pricing.input_cost_per_mtok,
                "estimated_savings_usd": (cost_savings * 100.0).round() / 100.0,
                "tokens_saved": summary.tokens_saved,
            }),
        );
    }

    println!("{}", serde_json::to_string_pretty(&root)?);
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Terminal dashboard
// ============================================================================

fn run_dashboard(
    db: &dyn AnalyticsStore,
    since: Option<i64>,
    show_cost: bool,
    since_str: Option<&str>,
) -> anyhow::Result<ExitCode> {
    let summary = db.query_summary(since)?;

    if summary.invocations == 0 {
        println!("{}", "No analytics data found.".dimmed());
        println!();
        println!("Run skim commands to start collecting token savings data.");
        println!("Example: skim src/main.rs");
        return Ok(ExitCode::SUCCESS);
    }

    // Header
    let period = since_str.map_or("all time".to_string(), |s| format!("last {s}"));
    println!(
        "{}",
        format!("Token Analytics ({period})").bold()
    );
    println!();

    // Summary section
    println!("{}", "Summary".bold().underline());
    println!(
        "  Invocations:    {}",
        tokens::format_number(summary.invocations as usize)
    );
    println!(
        "  Raw tokens:     {}",
        tokens::format_number(summary.raw_tokens as usize)
    );
    println!(
        "  Compressed:     {}",
        tokens::format_number(summary.compressed_tokens as usize)
    );
    println!(
        "  Tokens saved:   {}",
        tokens::format_number(summary.tokens_saved as usize).green()
    );
    println!(
        "  Avg reduction:  {:.1}%",
        summary.avg_savings_pct
    );

    // Efficiency meter
    let pct = summary.avg_savings_pct.clamp(0.0, 100.0);
    let filled = (pct / 5.0).round() as usize;
    let empty = 20_usize.saturating_sub(filled);
    let bar = format!(
        "  [{}{}] {:.1}%",
        "\u{2588}".repeat(filled).green(),
        "\u{2591}".repeat(empty),
        pct
    );
    println!("{bar}");
    println!();

    // By command type
    let by_command = db.query_by_command(since)?;
    if !by_command.is_empty() {
        println!("{}", "By Command".bold().underline());
        for cmd in &by_command {
            println!(
                "  {:<8} {:>6} invocations, {} tokens saved ({:.1}%)",
                cmd.command_type,
                tokens::format_number(cmd.invocations as usize),
                tokens::format_number(cmd.tokens_saved as usize),
                cmd.avg_savings_pct,
            );
        }
        println!();
    }

    // By language
    let by_language = db.query_by_language(since)?;
    if !by_language.is_empty() {
        println!("{}", "By Language".bold().underline());
        for lang in &by_language {
            println!(
                "  {:<12} {:>6} files, {} tokens saved ({:.1}%)",
                lang.language,
                tokens::format_number(lang.files as usize),
                tokens::format_number(lang.tokens_saved as usize),
                lang.avg_savings_pct,
            );
        }
        println!();
    }

    // By mode
    let by_mode = db.query_by_mode(since)?;
    if !by_mode.is_empty() {
        println!("{}", "By Mode".bold().underline());
        for mode in &by_mode {
            println!(
                "  {:<12} {:>6} files, {} tokens saved ({:.1}%)",
                mode.mode,
                tokens::format_number(mode.files as usize),
                tokens::format_number(mode.tokens_saved as usize),
                mode.avg_savings_pct,
            );
        }
        println!();
    }

    // Parse tier distribution
    let tier = db.query_tier_distribution(since)?;
    if tier.full_pct > 0.0 || tier.degraded_pct > 0.0 || tier.passthrough_pct > 0.0 {
        println!("{}", "Parse Quality".bold().underline());
        println!(
            "  Full:        {:.1}%",
            tier.full_pct,
        );
        println!(
            "  Degraded:    {:.1}%",
            tier.degraded_pct,
        );
        println!(
            "  Passthrough: {:.1}%",
            tier.passthrough_pct,
        );
        println!();
    }

    // Cost estimates
    if show_cost {
        let pricing = PricingModel::from_env_or_default();
        let cost_savings = pricing.estimate_savings(summary.tokens_saved);
        println!("{}", "Cost Estimates".bold().underline());
        println!(
            "  Model:          {}",
            pricing.model_name
        );
        println!(
            "  Input cost:     ${:.2}/MTok",
            pricing.input_cost_per_mtok
        );
        println!(
            "  Estimated savings: {}",
            format!("${:.2}", cost_savings).green()
        );
        println!();
    }

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::*;

    /// In-memory mock store for testing dashboard rendering without a real DB.
    struct MockStore {
        summary: AnalyticsSummary,
        daily: Vec<DailyStats>,
        by_command: Vec<CommandStats>,
        by_language: Vec<LanguageStats>,
        by_mode: Vec<ModeStats>,
        tier_dist: TierDistribution,
    }

    impl MockStore {
        fn empty() -> Self {
            Self {
                summary: AnalyticsSummary {
                    invocations: 0,
                    raw_tokens: 0,
                    compressed_tokens: 0,
                    tokens_saved: 0,
                    avg_savings_pct: 0.0,
                },
                daily: vec![],
                by_command: vec![],
                by_language: vec![],
                by_mode: vec![],
                tier_dist: TierDistribution {
                    full_pct: 0.0,
                    degraded_pct: 0.0,
                    passthrough_pct: 0.0,
                },
            }
        }

        fn with_data() -> Self {
            Self {
                summary: AnalyticsSummary {
                    invocations: 42,
                    raw_tokens: 100_000,
                    compressed_tokens: 30_000,
                    tokens_saved: 70_000,
                    avg_savings_pct: 70.0,
                },
                daily: vec![DailyStats {
                    date: "2026-03-24".to_string(),
                    invocations: 42,
                    tokens_saved: 70_000,
                    avg_savings_pct: 70.0,
                }],
                by_command: vec![CommandStats {
                    command_type: "file".to_string(),
                    invocations: 30,
                    tokens_saved: 50_000,
                    avg_savings_pct: 72.0,
                }],
                by_language: vec![LanguageStats {
                    language: "rust".to_string(),
                    files: 25,
                    tokens_saved: 40_000,
                    avg_savings_pct: 75.0,
                }],
                by_mode: vec![ModeStats {
                    mode: "structure".to_string(),
                    files: 20,
                    tokens_saved: 35_000,
                    avg_savings_pct: 78.0,
                }],
                tier_dist: TierDistribution {
                    full_pct: 90.0,
                    degraded_pct: 8.0,
                    passthrough_pct: 2.0,
                },
            }
        }
    }

    impl AnalyticsStore for MockStore {
        fn query_summary(&self, _since: Option<i64>) -> anyhow::Result<AnalyticsSummary> {
            Ok(self.summary.clone())
        }
        fn query_daily(&self, _since: Option<i64>) -> anyhow::Result<Vec<DailyStats>> {
            Ok(self.daily.clone())
        }
        fn query_by_command(&self, _since: Option<i64>) -> anyhow::Result<Vec<CommandStats>> {
            Ok(self.by_command.clone())
        }
        fn query_by_language(&self, _since: Option<i64>) -> anyhow::Result<Vec<LanguageStats>> {
            Ok(self.by_language.clone())
        }
        fn query_by_mode(&self, _since: Option<i64>) -> anyhow::Result<Vec<ModeStats>> {
            Ok(self.by_mode.clone())
        }
        fn query_tier_distribution(&self, _since: Option<i64>) -> anyhow::Result<TierDistribution> {
            Ok(self.tier_dist.clone())
        }
        fn clear(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_run_json_empty_store() {
        let store = MockStore::empty();
        let result = run_json(&store, None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_json_with_data() {
        let store = MockStore::with_data();
        let result = run_json(&store, None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_json_with_cost() {
        let store = MockStore::with_data();
        let result = run_json(&store, None, true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_dashboard_empty_store() {
        let store = MockStore::empty();
        let result = run_dashboard(&store, None, false, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_dashboard_with_data() {
        let store = MockStore::with_data();
        let result = run_dashboard(&store, None, false, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_dashboard_with_cost() {
        let store = MockStore::with_data();
        let result = run_dashboard(&store, None, true, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_dashboard_with_since_label() {
        let store = MockStore::with_data();
        let result = run_dashboard(&store, None, false, Some("7d"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_clear_mock() {
        let store = MockStore::empty();
        let result = run_clear(&store);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_value_flag_bare() {
        let args: Vec<String> = vec!["--format".into(), "json".into()];
        assert_eq!(parse_value_flag(&args, "--format"), Some("json".to_string()));
    }

    #[test]
    fn test_parse_value_flag_equals() {
        let args: Vec<String> = vec!["--format=json".into()];
        assert_eq!(parse_value_flag(&args, "--format"), Some("json".to_string()));
    }

    #[test]
    fn test_parse_value_flag_missing() {
        let args: Vec<String> = vec!["--cost".into()];
        assert_eq!(parse_value_flag(&args, "--format"), None);
    }
}
