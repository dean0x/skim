//! Stats subcommand — token analytics dashboard (#56)
//!
//! Queries the analytics SQLite database and displays a summary of token
//! savings across all skim invocations. Supports time filtering (`--since`),
//! JSON output (`--format json`), cost estimates (`--cost`), and data clearing
//! (`--clear`).

use std::process::ExitCode;
use std::time::UNIX_EPOCH;

use colored::Colorize;

use crate::analytics::{AnalyticsDb, PricingModel};
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

    if clear {
        return run_clear();
    }

    let since_ts = if let Some(s) = &since_str {
        let time = parse_duration_ago(s)?;
        let ts = time.duration_since(UNIX_EPOCH)?.as_secs() as i64;
        Some(ts)
    } else {
        None
    };

    let db = AnalyticsDb::open_default()?;

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

fn run_clear() -> anyhow::Result<ExitCode> {
    let db = AnalyticsDb::open_default()?;
    db.clear()?;
    println!("Analytics data cleared.");
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// JSON output
// ============================================================================

fn run_json(db: &AnalyticsDb, since: Option<i64>, show_cost: bool) -> anyhow::Result<ExitCode> {
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
    db: &AnalyticsDb,
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
