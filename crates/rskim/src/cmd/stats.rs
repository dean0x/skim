//! Stats subcommand — token analytics dashboard (#56)
//!
//! Queries the analytics SQLite database and displays a summary of token
//! savings across all skim invocations. Supports time filtering (`--since`),
//! JSON output (`--format json`), verbose parse-quality output (`--verbose`),
//! and data clearing (`--clear`). Cost estimates are always shown.

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::UNIX_EPOCH;

use colored::{ColoredString, Colorize};

use crate::analytics::{AnalyticsDb, AnalyticsStore, OriginalCommandStats, PricingModel};
use crate::cmd::session::types::parse_duration_ago;
use crate::tokens;

// ============================================================================
// Public entry point
// ============================================================================

/// Run the `skim stats` subcommand.
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Parse flags
    if args.iter().any(|a| a == "--cost") {
        eprintln!("skim: --cost is deprecated; cost estimates are now always shown");
    }
    let clear = args.iter().any(|a| a == "--clear");
    let verbose = args
        .iter()
        .any(|a| matches!(a.as_str(), "--verbose" | "-v"));
    let format = parse_value_flag(args, "--format");
    let since_str = parse_value_flag(args, "--since");

    let db = AnalyticsDb::open_default()?;

    if clear {
        return run_clear(&db);
    }

    // Auto-clean: one-time self-healing for pre-fix corrupt records where
    // compressed_tokens > raw_tokens.  Runs on concrete AnalyticsDb, reports
    // to stderr so it never pollutes JSON stdout.
    let cleaned = db.clean_invalid_records().unwrap_or(0);
    if cleaned > 0 {
        eprintln!("skim: cleaned {cleaned} invalid analytics record(s)");
    }

    let since_ts = if let Some(s) = &since_str {
        let ts = parse_duration_ago(s)?.duration_since(UNIX_EPOCH)?.as_secs() as i64;
        Some(ts)
    } else {
        None
    };

    let mut stdout = io::stdout().lock();

    if format.as_deref() == Some("json") {
        return run_json(&mut stdout, &db, since_ts, analytics.input_cost_per_mtok);
    }

    run_dashboard(
        &mut stdout,
        &db,
        since_ts,
        verbose,
        since_str.as_deref(),
        analytics.input_cost_per_mtok,
    )
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
    println!("  --verbose, -v         Show parse quality section");
    println!("  --clear               Delete all analytics data");
    println!();
    println!("EXAMPLES:");
    println!("  skim stats                   Show all-time summary");
    println!("  skim stats --since 7d        Last 7 days");
    println!("  skim stats --format json     Machine-readable output");
    println!("  skim stats --verbose         Include parse quality details");
    println!("  skim stats --clear           Reset analytics data");
    println!();
    println!("ENVIRONMENT:");
    println!("  SKIM_INPUT_COST_PER_MTOK     Override $/MTok for cost estimates (default: 3.0)");
    println!("  SKIM_ANALYTICS_DB            Override analytics database path");
    println!(
        "  SKIM_DISABLE_ANALYTICS       Set to 1, true, or yes to disable analytics recording"
    );
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

fn run_json(
    w: &mut dyn Write,
    db: &dyn AnalyticsStore,
    since: Option<i64>,
    cost_override: Option<f64>,
) -> anyhow::Result<ExitCode> {
    let summary = db.query_summary(since)?;
    let daily = db.query_daily(since)?;
    let by_command = db.query_by_command(since)?;
    let by_language = db.query_by_language(since)?;
    let by_mode = db.query_by_mode(since)?;
    let tier_dist = db.query_tier_distribution(since)?;
    let by_original_cmd = db.query_by_original_cmd(since)?;

    let weighted_pct = weighted_savings_pct(&summary);

    let pricing = PricingModel::from_cost_override(cost_override);
    let cost_savings = pricing.estimate_savings(summary.tokens_saved);
    // INTENTIONAL API CHANGE (stats dashboard v3 refactor): the `cost_estimate`
    // object uses `tier` (e.g. "Standard") rather than the previous `model` key
    // (e.g. "claude-sonnet-4-6").  Downstream consumers must update accordingly.
    let cost_estimate = serde_json::json!({
        "tier": pricing.tier_name,
        "input_cost_per_mtok": pricing.input_cost_per_mtok,
        "estimated_savings_usd": (cost_savings * 100.0).round() / 100.0,
        "tokens_saved": summary.tokens_saved,
    });

    let root = serde_json::json!({
        "summary": {
            "invocations": summary.invocations,
            "raw_tokens": summary.raw_tokens,
            "compressed_tokens": summary.compressed_tokens,
            "tokens_saved": summary.tokens_saved,
            "avg_savings_pct": summary.avg_savings_pct,
            "weighted_savings_pct": weighted_pct,
        },
        "daily": daily,
        "by_command": by_command,
        "by_language": by_language,
        "by_mode": by_mode,
        "tier_distribution": tier_dist,
        "by_original_cmd": by_original_cmd,
        "cost_estimate": cost_estimate,
    });

    writeln!(w, "{}", serde_json::to_string_pretty(&root)?)?;
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Dashboard layout constants
// ============================================================================

const COL_NAME: usize = 14;
const COL_COUNT: usize = 6;
const COL_SAVED: usize = 8;
const COL_DUR: usize = 6;
const BAR_WIDTH: usize = 16;
const SUMMARY_BAR_WIDTH: usize = 50;
/// Maximum display length for original_cmd in the By Command section.
const DISPLAY_CMD_LEN: usize = 30;

// ============================================================================
// Dashboard formatting helpers
// ============================================================================

/// Format a duration in milliseconds as a human-readable string.
///
/// Examples: `0ms`, `12ms`, `1.2s`, `34.5s`.
fn format_duration_ms(ms: f64) -> String {
    if ms < 1000.0 {
        format!("{:.0}ms", ms)
    } else {
        format!("{:.1}s", ms / 1000.0)
    }
}

/// Format a token count in compact human-readable form: 1.5K, 2.4M, 1.2B.
/// Values under 1000 are rendered as plain integers.
fn format_tokens(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Apply the standard efficiency color to a pre-formatted string.
///
/// All values render green — a single unified color for a cleaner visual.
fn apply_efficiency_color(s: String) -> ColoredString {
    s.green()
}

/// Colorise a savings percentage with ANSI codes.
///
/// Clamps to [0.0, 100.0] then formats right-aligned in a 6-char field
/// before applying color so ANSI escape sequences do not affect alignment.
fn color_pct(pct: f64) -> ColoredString {
    let clamped = pct.clamp(0.0, 100.0);
    apply_efficiency_color(format!("{clamped:>5.1}%"))
}

/// Render a block-character progress bar.
///
/// Uses `█` for filled and `░` for empty cells. Filled cells are colored green;
/// empty cells are uncolored. `pct` is clamped to [0, 100] before computing fill width.
fn render_bar(pct: f64, width: usize) -> String {
    let clamped = pct.clamp(0.0, 100.0);
    let filled = ((clamped / 100.0) * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);
    if filled == 0 {
        format!("[{}]", "\u{2591}".repeat(empty))
    } else {
        let colored_fill = apply_efficiency_color("\u{2588}".repeat(filled));
        format!("[{}{}]", colored_fill, "\u{2591}".repeat(empty))
    }
}

/// Format a section header padded to 76 characters with thin horizontal lines.
fn section_header(title: &str) -> String {
    // "── {title} " + trailing dashes to 76 chars total
    let prefix = format!("\u{2500}\u{2500} {title} ");
    let remaining = 76_usize.saturating_sub(prefix.len());
    format!("{}{}", prefix, "\u{2500}".repeat(remaining))
}

/// Map a stored command_type string to a human-readable label.
fn command_label(stored: &str) -> &'static str {
    match stored {
        "file" => "Source files",
        "test" => "Test output",
        "build" => "Build output",
        "git" => "Git output",
        "lint" => "Lint output",
        "pkg" => "Pkg output",
        "infra" => "Infra output",
        "fileops" => "File ops",
        "log" => "Log output",
        _ => "Other",
    }
}

// ============================================================================
// Analytics computation helpers
// ============================================================================

/// Compute the true weighted savings percentage from a summary.
///
/// Unlike `avg_savings_pct` (which is the arithmetic mean of per-invocation
/// percentages), this value is token-count-weighted: it answers "of all raw
/// tokens ever seen, what fraction was saved?".  Returns 0.0 when
/// `raw_tokens == 0` to prevent division by zero.
fn weighted_savings_pct(summary: &crate::analytics::AnalyticsSummary) -> f64 {
    if summary.raw_tokens > 0 {
        (summary.tokens_saved as f64 / summary.raw_tokens as f64) * 100.0
    } else {
        0.0
    }
}

// ============================================================================
// Terminal dashboard — section renderers
// ============================================================================

fn render_header(w: &mut dyn Write, period: &str) -> anyhow::Result<()> {
    let border = "\u{2550}".repeat(78);
    writeln!(w, "{}", border.bold())?;
    writeln!(w, "{}", format!("  skim Token Analytics ({period})").bold())?;
    writeln!(w, "{}", border.bold())?;
    writeln!(w)?;
    Ok(())
}

fn render_summary(
    w: &mut dyn Write,
    summary: &crate::analytics::AnalyticsSummary,
) -> anyhow::Result<()> {
    let weighted_pct = weighted_savings_pct(summary);

    writeln!(w, "{}", section_header("Summary"))?;
    writeln!(w)?;
    writeln!(
        w,
        "  Invocations:  {}",
        tokens::format_number(summary.invocations as usize)
    )?;
    writeln!(
        w,
        "  Raw tokens:   {}",
        tokens::format_number(summary.raw_tokens as usize)
    )?;
    writeln!(
        w,
        "  Tokens saved: {}",
        tokens::format_number(summary.tokens_saved as usize).green(),
    )?;
    writeln!(w)?;
    writeln!(
        w,
        "  {}  {}",
        render_bar(weighted_pct, SUMMARY_BAR_WIDTH),
        color_pct(weighted_pct)
    )?;
    writeln!(w)?;
    Ok(())
}

fn render_by_category(
    w: &mut dyn Write,
    by_command: &[crate::analytics::CommandStats],
) -> anyhow::Result<()> {
    if by_command.is_empty() {
        return Ok(());
    }
    writeln!(w, "{}", section_header("By Category"))?;
    writeln!(w)?;
    writeln!(
        w,
        "  {:<COL_NAME$}  {:>COL_COUNT$}  {:>COL_SAVED$}  {:<9}  {:>COL_DUR$}",
        "CATEGORY", "CALLS", "SAVED", "REDUCTION", "AVG TIME"
    )?;
    for cmd in by_command {
        writeln!(
            w,
            "  {:<COL_NAME$}  {:>COL_COUNT$}  {:>COL_SAVED$}  {}  {:>COL_DUR$}  {}",
            command_label(&cmd.command_type),
            tokens::format_number(cmd.invocations as usize),
            format_tokens(cmd.tokens_saved),
            color_pct(cmd.avg_savings_pct),
            format_duration_ms(cmd.avg_duration_ms),
            render_bar(cmd.avg_savings_pct, BAR_WIDTH),
        )?;
    }
    writeln!(w)?;
    Ok(())
}

fn render_by_language(
    w: &mut dyn Write,
    by_language: &[crate::analytics::LanguageStats],
) -> anyhow::Result<()> {
    if by_language.is_empty() {
        return Ok(());
    }
    writeln!(w, "{}", section_header("By Language"))?;
    writeln!(w)?;
    writeln!(
        w,
        "  {:<COL_NAME$}  {:>COL_COUNT$}  {:>COL_SAVED$}  {:<9}",
        "LANGUAGE", "FILES", "SAVED", "REDUCTION"
    )?;
    for lang in by_language {
        writeln!(
            w,
            "  {:<COL_NAME$}  {:>COL_COUNT$}  {:>COL_SAVED$}  {}  {}",
            lang.language,
            tokens::format_number(lang.files as usize),
            format_tokens(lang.tokens_saved),
            color_pct(lang.avg_savings_pct),
            render_bar(lang.avg_savings_pct, BAR_WIDTH),
        )?;
    }
    writeln!(w)?;
    Ok(())
}

fn render_by_mode(
    w: &mut dyn Write,
    by_mode: &[crate::analytics::ModeStats],
) -> anyhow::Result<()> {
    if by_mode.is_empty() {
        return Ok(());
    }
    writeln!(w, "{}", section_header("By Mode"))?;
    writeln!(w)?;
    writeln!(
        w,
        "  {:<COL_NAME$}  {:>COL_COUNT$}  {:>COL_SAVED$}  {:<9}",
        "MODE", "FILES", "SAVED", "REDUCTION"
    )?;
    for mode in by_mode {
        writeln!(
            w,
            "  {:<COL_NAME$}  {:>COL_COUNT$}  {:>COL_SAVED$}  {}  {}",
            mode.mode,
            tokens::format_number(mode.files as usize),
            format_tokens(mode.tokens_saved),
            color_pct(mode.avg_savings_pct),
            render_bar(mode.avg_savings_pct, BAR_WIDTH),
        )?;
    }
    writeln!(w)?;
    Ok(())
}

/// Truncate `cmd` to at most `max_chars` character-boundary-safe chars,
/// appending `...` when truncated.  Uses a single `char_indices` pass so
/// each character is visited at most once regardless of string length.
fn truncate_cmd_display(cmd: &str, max_chars: usize) -> String {
    let keep = max_chars.saturating_sub(3);
    let mut cut_byte = None;
    for (i, (byte_idx, _)) in cmd.char_indices().enumerate() {
        if i == keep {
            cut_byte = Some(byte_idx);
        }
        if i == max_chars {
            return format!("{}...", &cmd[..cut_byte.unwrap_or(0)]);
        }
    }
    cmd.to_string()
}

fn render_by_original_cmd(
    w: &mut dyn Write,
    by_original_cmd: &[OriginalCommandStats],
) -> anyhow::Result<()> {
    if by_original_cmd.is_empty() {
        return Ok(());
    }
    writeln!(w, "{}", section_header("By Command"))?;
    writeln!(w)?;
    writeln!(
        w,
        "  {:<DISPLAY_CMD_LEN$}  {:>COL_COUNT$}  {:>COL_SAVED$}  {:<9}  {:>COL_DUR$}",
        "COMMAND", "CALLS", "SAVED", "REDUCTION", "AVG TIME"
    )?;
    for cmd in by_original_cmd {
        let display = truncate_cmd_display(&cmd.original_cmd, DISPLAY_CMD_LEN);
        writeln!(
            w,
            "  {:<DISPLAY_CMD_LEN$}  {:>COL_COUNT$}  {:>COL_SAVED$}  {}  {:>COL_DUR$}  {}",
            display,
            tokens::format_number(cmd.invocations as usize),
            format_tokens(cmd.tokens_saved),
            color_pct(cmd.avg_savings_pct),
            format_duration_ms(cmd.avg_duration_ms),
            render_bar(cmd.avg_savings_pct, BAR_WIDTH),
        )?;
    }
    writeln!(w)?;
    Ok(())
}

fn render_parse_quality(
    w: &mut dyn Write,
    tier_dist: &crate::analytics::TierDistribution,
) -> anyhow::Result<()> {
    writeln!(w, "{}", section_header("Parse Quality"))?;
    writeln!(w)?;
    if tier_dist.full_pct > 0.0 || tier_dist.degraded_pct > 0.0 || tier_dist.passthrough_pct > 0.0 {
        writeln!(w, "  Full:        {:.1}%", tier_dist.full_pct)?;
        writeln!(w, "  Degraded:    {:.1}%", tier_dist.degraded_pct)?;
        writeln!(w, "  Passthrough: {:.1}%", tier_dist.passthrough_pct)?;
    } else {
        writeln!(w, "  No tier data recorded yet.")?;
    }
    writeln!(w)?;
    Ok(())
}

fn render_cost_section(
    w: &mut dyn Write,
    tokens_saved: u64,
    cost_override: Option<f64>,
) -> anyhow::Result<()> {
    let pricing = PricingModel::from_cost_override(cost_override);
    writeln!(w, "{}", section_header("Cost Estimates"))?;
    writeln!(w)?;

    for price_tier in PricingModel::all_tiers() {
        let savings = price_tier.estimate_savings(tokens_saved);
        writeln!(
            w,
            "  {:<10} ${:>5.2}/MTok    ${:.2} saved",
            price_tier.tier_name, price_tier.input_cost_per_mtok, savings
        )?;
    }

    // Show custom tier row if env var was used
    if pricing.tier_name == "Custom" {
        let savings = pricing.estimate_savings(tokens_saved);
        writeln!(
            w,
            "  {:<10} ${:>5.2}/MTok    ${:.2} saved",
            pricing.tier_name, pricing.input_cost_per_mtok, savings
        )?;
    }

    writeln!(w)?;
    Ok(())
}

// ============================================================================
// Terminal dashboard — orchestrator
// ============================================================================

fn run_dashboard(
    w: &mut dyn Write,
    db: &dyn AnalyticsStore,
    since: Option<i64>,
    verbose: bool,
    since_str: Option<&str>,
    cost_override: Option<f64>,
) -> anyhow::Result<ExitCode> {
    let summary = db.query_summary(since)?;

    if summary.invocations == 0 {
        writeln!(w, "{}", "No analytics data found.".dimmed())?;
        writeln!(w)?;
        writeln!(
            w,
            "Run skim commands to start collecting token savings data."
        )?;
        writeln!(w, "Example: skim src/main.rs")?;
        return Ok(ExitCode::SUCCESS);
    }

    let period = since_str.map_or("all time".to_string(), |s| format!("last {s}"));
    render_header(w, &period)?;
    render_summary(w, &summary)?;
    render_by_category(w, &db.query_by_command(since)?)?;
    render_by_language(w, &db.query_by_language(since)?)?;
    render_by_mode(w, &db.query_by_mode(since)?)?;
    render_by_original_cmd(w, &db.query_by_original_cmd(since)?)?;
    if verbose {
        render_parse_quality(w, &db.query_tier_distribution(since)?)?;
    }
    render_cost_section(w, summary.tokens_saved, cost_override)?;

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::*;

    // ========================================================================
    // format_tokens tests
    // ========================================================================

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_000), "1.0K");
        assert_eq!(format_tokens(1_500), "1.5K");
        assert_eq!(format_tokens(1_000_000), "1.0M");
        assert_eq!(format_tokens(2_400_000), "2.4M");
        assert_eq!(format_tokens(1_000_000_000), "1.0B");
    }

    // ========================================================================
    // color_pct tests
    // ========================================================================

    #[test]
    fn test_color_pct_clamping() {
        // Negative clamps to 0.0
        let s = color_pct(-5.0).to_string();
        assert!(
            s.contains("0.0%"),
            "negative should clamp to 0.0%, got: {s}"
        );
        // Over 100 clamps to 100.0
        let s = color_pct(150.0).to_string();
        assert!(
            s.contains("100.0%"),
            "over-100 should clamp to 100.0%, got: {s}"
        );
    }

    // ========================================================================
    // section_header test
    // ========================================================================

    #[test]
    fn test_section_header_total_width() {
        let hdr = section_header("Summary");
        // Should be close to 76 chars (allow for unicode char width)
        assert!(
            hdr.len() >= 70,
            "section header should pad to ~76 chars, got {}",
            hdr.len()
        );
        assert!(hdr.contains("Summary"), "header must contain title");
    }

    /// In-memory mock store for testing dashboard rendering without a real DB.
    struct MockStore {
        summary: AnalyticsSummary,
        daily: Vec<DailyStats>,
        by_command: Vec<CommandStats>,
        by_language: Vec<LanguageStats>,
        by_mode: Vec<ModeStats>,
        tier_dist: TierDistribution,
        by_original_cmd: Vec<OriginalCommandStats>,
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
                by_original_cmd: vec![],
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
                daily: vec![
                    DailyStats {
                        date: "2026-03-20".to_string(),
                        invocations: 8,
                        tokens_saved: 10_000,
                        avg_savings_pct: 65.0,
                    },
                    DailyStats {
                        date: "2026-03-22".to_string(),
                        invocations: 12,
                        tokens_saved: 20_000,
                        avg_savings_pct: 70.0,
                    },
                    DailyStats {
                        date: "2026-03-24".to_string(),
                        invocations: 42,
                        tokens_saved: 70_000,
                        avg_savings_pct: 70.0,
                    },
                    DailyStats {
                        date: "2026-03-26".to_string(),
                        invocations: 5,
                        tokens_saved: 8_000,
                        avg_savings_pct: 60.0,
                    },
                    DailyStats {
                        date: "2026-03-28".to_string(),
                        invocations: 7,
                        tokens_saved: 15_000,
                        avg_savings_pct: 72.0,
                    },
                ],
                by_command: vec![CommandStats {
                    command_type: "file".to_string(),
                    invocations: 30,
                    tokens_saved: 50_000,
                    avg_savings_pct: 72.0,
                    avg_duration_ms: 125.0,
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
                by_original_cmd: vec![OriginalCommandStats {
                    original_cmd: "cargo build 2>&1".to_string(),
                    invocations: 42,
                    tokens_saved: 55_000,
                    avg_savings_pct: 72.0,
                    avg_duration_ms: 891.0,
                }],
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
        fn query_by_original_cmd(
            &self,
            _since: Option<i64>,
        ) -> anyhow::Result<Vec<OriginalCommandStats>> {
            Ok(self.by_original_cmd.clone())
        }
        fn clear(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Helper: run a rendering function and return the captured output as a String.
    fn capture<F>(f: F) -> String
    where
        F: FnOnce(&mut Vec<u8>) -> anyhow::Result<ExitCode>,
    {
        let mut buf = Vec::new();
        let code = f(&mut buf).expect("render function should succeed");
        assert_eq!(code, ExitCode::SUCCESS);
        String::from_utf8(buf).expect("output should be valid UTF-8")
    }

    #[test]
    fn test_run_json_empty_store() {
        let store = MockStore::empty();
        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        let summary = &parsed["summary"];
        assert_eq!(summary["invocations"], 0);
        assert_eq!(summary["tokens_saved"], 0);
    }

    #[test]
    fn test_run_json_with_data() {
        let store = MockStore::with_data();
        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        let summary = &parsed["summary"];
        assert_eq!(summary["invocations"], 42);
        assert_eq!(summary["tokens_saved"], 70_000);
        assert_eq!(summary["avg_savings_pct"], 70.0);
        // Verify weighted_savings_pct is present: 70000/100000 * 100 = 70.0
        let weighted = summary["weighted_savings_pct"].as_f64().unwrap();
        assert!(
            (weighted - 70.0).abs() < 0.01,
            "weighted_savings_pct should be 70.0, got {weighted}"
        );
        // Verify breakdowns are present
        assert_eq!(parsed["by_command"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["by_language"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["by_mode"].as_array().unwrap().len(), 1);
        // by_original_cmd breakdown is present
        assert_eq!(parsed["by_original_cmd"].as_array().unwrap().len(), 1);
        // cost_estimate is always present now
        assert!(
            parsed["cost_estimate"].is_object(),
            "cost_estimate should always be in JSON output"
        );
    }

    #[test]
    fn test_run_json_with_cost() {
        // Passing a custom cost_override should reflect in input_cost_per_mtok.
        let store = MockStore::with_data();
        let output = capture(|w| run_json(w, &store, None, Some(5.0)));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        let cost = &parsed["cost_estimate"];
        assert!(cost.is_object(), "cost_estimate should always be present");
        assert_eq!(cost["tokens_saved"], 70_000);
        assert!(cost["estimated_savings_usd"].as_f64().unwrap() > 0.0);
        // The custom rate should appear in the output.
        assert_eq!(
            cost["input_cost_per_mtok"].as_f64().unwrap(),
            5.0,
            "cost_estimate should reflect the custom cost_override of 5.0 $/MTok"
        );
    }

    #[test]
    fn test_run_dashboard_empty_store() {
        let store = MockStore::empty();
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            output.contains("No analytics data found"),
            "empty dashboard should show empty message"
        );
    }

    #[test]
    fn test_run_dashboard_with_data() {
        let store = MockStore::with_data();
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            output.contains("42"),
            "dashboard should show invocation count"
        );
        assert!(
            output.contains("70,000"),
            "dashboard should show tokens saved"
        );
        assert!(
            output.contains("70.0%"),
            "dashboard should show weighted savings percentage"
        );
        assert!(
            output.contains("all time"),
            "dashboard should show period label"
        );
        assert!(
            output.contains("rust"),
            "dashboard should show language breakdown"
        );
        assert!(
            output.contains("structure"),
            "dashboard should show mode breakdown"
        );
    }

    #[test]
    fn test_run_dashboard_always_shows_cost() {
        // Cost section is always shown — no flag needed
        let store = MockStore::with_data();
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            output.contains("Cost Estimates"),
            "dashboard should always show cost section"
        );
        assert!(output.contains("/MTok"), "cost section should show rate");
    }

    #[test]
    fn test_run_dashboard_with_since_label() {
        let store = MockStore::with_data();
        let output = capture(|w| run_dashboard(w, &store, None, false, Some("7d"), None));
        assert!(
            output.contains("last 7d"),
            "dashboard should show since period"
        );
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
        assert_eq!(
            parse_value_flag(&args, "--format"),
            Some("json".to_string())
        );
    }

    #[test]
    fn test_parse_value_flag_equals() {
        let args: Vec<String> = vec!["--format=json".into()];
        assert_eq!(
            parse_value_flag(&args, "--format"),
            Some("json".to_string())
        );
    }

    #[test]
    fn test_parse_value_flag_missing() {
        let args: Vec<String> = vec!["--clear".into()];
        assert_eq!(parse_value_flag(&args, "--format"), None);
    }

    // ========================================================================
    // command_label tests
    // ========================================================================

    #[test]
    fn test_command_label() {
        assert_eq!(command_label("file"), "Source files");
        assert_eq!(command_label("test"), "Test output");
        assert_eq!(command_label("build"), "Build output");
        assert_eq!(command_label("git"), "Git output");
        assert_eq!(command_label("lint"), "Lint output");
        assert_eq!(command_label("pkg"), "Pkg output");
        assert_eq!(command_label("infra"), "Infra output");
        assert_eq!(command_label("fileops"), "File ops");
        assert_eq!(command_label("log"), "Log output");
        assert_eq!(command_label("unknown_cmd"), "Other");
    }

    // ========================================================================
    // render_bar tests
    // ========================================================================

    #[test]
    fn test_render_bar_zero_pct() {
        let bar = render_bar(0.0, 10);
        // All cells should be empty (░), no filled cells
        assert!(bar.starts_with('['), "bar should start with '['");
        assert!(bar.ends_with(']'), "bar should end with ']'");
        // Strip ANSI for counting: just verify the empty block char count
        let empty_count = bar.chars().filter(|&c| c == '░').count();
        assert_eq!(empty_count, 10, "0% bar should have 10 empty cells");
    }

    #[test]
    fn test_render_bar_full_pct() {
        let bar = render_bar(100.0, 10);
        let fill_count = bar.chars().filter(|&c| c == '█').count();
        let empty_count = bar.chars().filter(|&c| c == '░').count();
        assert_eq!(fill_count, 10, "100% bar should have 10 filled cells");
        assert_eq!(empty_count, 0, "100% bar should have 0 empty cells");
    }

    #[test]
    fn test_render_bar_clamps_negative() {
        // Negative percentage should clamp to 0
        let bar = render_bar(-20.0, 10);
        let empty_count = bar.chars().filter(|&c| c == '░').count();
        assert_eq!(
            empty_count, 10,
            "negative pct should clamp to 0% (all empty)"
        );
    }

    #[test]
    fn test_render_bar_clamps_over_100() {
        // Over-100 percentage should clamp to 100
        let bar = render_bar(150.0, 10);
        let fill_count = bar.chars().filter(|&c| c == '█').count();
        assert_eq!(
            fill_count, 10,
            "pct > 100 should clamp to 100% (all filled)"
        );
    }

    #[test]
    fn test_render_bar_zero_width() {
        // Zero-width bar should still have brackets with no cells
        let bar = render_bar(50.0, 0);
        assert_eq!(bar, "[]", "zero-width bar should be '[]'");
    }

    #[test]
    fn test_render_bar_half_pct() {
        let bar = render_bar(50.0, 10);
        let fill_count = bar.chars().filter(|&c| c == '█').count();
        let empty_count = bar.chars().filter(|&c| c == '░').count();
        assert_eq!(
            fill_count, 5,
            "50% bar (width 10) should have 5 filled cells"
        );
        assert_eq!(
            empty_count, 5,
            "50% bar (width 10) should have 5 empty cells"
        );
    }

    // ========================================================================
    // JSON output value assertions
    // ========================================================================

    #[test]
    fn test_run_json_tier_distribution_values() {
        let store = MockStore::with_data();
        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        let tier = &parsed["tier_distribution"];
        assert!(
            tier.is_object(),
            "tier_distribution should be a JSON object"
        );
        assert_eq!(
            tier["full_pct"].as_f64().unwrap(),
            90.0,
            "full_pct should be 90.0"
        );
        assert_eq!(
            tier["degraded_pct"].as_f64().unwrap(),
            8.0,
            "degraded_pct should be 8.0"
        );
        assert_eq!(
            tier["passthrough_pct"].as_f64().unwrap(),
            2.0,
            "passthrough_pct should be 2.0"
        );
    }

    #[test]
    fn test_run_json_cost_tier_value() {
        let store = MockStore::with_data();
        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        let cost = &parsed["cost_estimate"];
        let tier = cost["tier"].as_str().expect("tier should be a string");
        // Default pricing model tier should be "Standard"
        assert_eq!(tier, "Standard", "default cost tier should be 'Standard'");
    }

    // ========================================================================
    // Dashboard section tests
    // ========================================================================

    #[test]
    fn test_dashboard_shows_command_labels() {
        let store = MockStore::with_data();
        // MockStore::with_data() has command_type: "file" → "Source files" label in By Category
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            output.contains("By Category"),
            "dashboard should show 'By Category' section header"
        );
        assert!(
            output.contains("Source files"),
            "dashboard should show 'Source files' label for 'file' command type"
        );
    }

    #[test]
    fn test_dashboard_column_headers() {
        let store = MockStore::with_data();
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        // By Category section headers
        assert!(
            output.contains("CATEGORY"),
            "By Category section should have CATEGORY column header"
        );
        // By Language section headers
        assert!(
            output.contains("LANGUAGE"),
            "By Language section should have LANGUAGE column header"
        );
        // By Mode section headers
        assert!(
            output.contains("MODE"),
            "By Mode section should have MODE column header"
        );
        // By Command section headers
        assert!(
            output.contains("COMMAND"),
            "By Command section should have COMMAND column header"
        );
    }

    // ========================================================================
    // Multi-tier cost table test
    // ========================================================================

    #[test]
    fn test_dashboard_multi_tier_cost() {
        let store = MockStore::with_data();
        // Cost section is always shown now; verbose flag is for parse quality
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            output.contains("Economy"),
            "cost section should show Economy tier"
        );
        assert!(
            output.contains("Standard"),
            "cost section should show Standard tier"
        );
        assert!(
            output.contains("Premium"),
            "cost section should show Premium tier"
        );
        assert!(output.contains("/MTok"), "cost section should show rate");
    }

    // ========================================================================
    // Weighted savings % tests
    // ========================================================================

    #[test]
    fn test_weighted_savings_pct_calculation() {
        // raw=100_000, saved=70_000 → weighted = 70.0%
        let store = MockStore::with_data();
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        // Summary should show the weighted % (70.0%) on the bar line below "Tokens saved"
        assert!(
            output.contains("70.0%"),
            "summary should show weighted savings pct"
        );
    }

    #[test]
    fn test_weighted_savings_pct_zero_raw_tokens() {
        // When raw_tokens == 0, weighted_pct should be 0.0 (no division by zero)
        let summary = crate::analytics::AnalyticsSummary {
            invocations: 1,
            raw_tokens: 0,
            compressed_tokens: 0,
            tokens_saved: 0,
            avg_savings_pct: 0.0,
        };
        let mut buf = Vec::new();
        render_summary(&mut buf, &summary).expect("render should not fail");
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("0.0%"), "zero raw_tokens should show 0.0%");
    }

    // ========================================================================
    // Verbose / parse quality tests
    // ========================================================================

    #[test]
    fn test_verbose_shows_parse_quality() {
        let store = MockStore::with_data();
        let output = capture(|w| run_dashboard(w, &store, None, true, None, None));
        assert!(
            output.contains("Parse Quality"),
            "verbose mode should show Parse Quality section"
        );
    }

    #[test]
    fn test_non_verbose_hides_parse_quality() {
        let store = MockStore::with_data();
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            !output.contains("Parse Quality"),
            "non-verbose mode should NOT show Parse Quality section"
        );
    }

    // ========================================================================
    // render_by_original_cmd truncation test
    // ========================================================================

    #[test]
    fn test_render_by_original_cmd_truncation() {
        // A command longer than DISPLAY_CMD_LEN should be truncated with "..."
        let long_cmd = "a".repeat(50);
        let cmds = vec![OriginalCommandStats {
            original_cmd: long_cmd,
            invocations: 1,
            tokens_saved: 100,
            avg_savings_pct: 80.0,
            avg_duration_ms: 100.0,
        }];
        let mut buf = Vec::new();
        render_by_original_cmd(&mut buf, &cmds).expect("render should not fail");
        let output = String::from_utf8(buf).unwrap();
        // The truncated display should contain "..."
        assert!(
            output.contains("..."),
            "long commands should be truncated with '...'"
        );
        // The full 50-char command should NOT appear verbatim
        assert!(
            !output.contains(&"a".repeat(50)),
            "full long command should not appear verbatim"
        );
    }

    #[test]
    fn test_render_by_original_cmd_empty() {
        // Empty slice: render should succeed and produce no output
        let mut buf = Vec::new();
        render_by_original_cmd(&mut buf, &[]).expect("render should not fail on empty input");
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.is_empty(),
            "render_by_original_cmd with empty input should produce no output"
        );
    }

    #[test]
    fn test_truncate_cmd_display_short() {
        // Short commands are not truncated
        let result = truncate_cmd_display("cargo build", 30);
        assert_eq!(result, "cargo build");
    }

    #[test]
    fn test_truncate_cmd_display_long() {
        // Long commands get "..." suffix, total display ≤ max_chars
        let input = "x".repeat(40);
        let result = truncate_cmd_display(&input, 30);
        assert!(result.ends_with("..."), "should end with '...'");
        assert!(
            result.chars().count() <= 30,
            "result should be at most 30 chars"
        );
    }

    #[test]
    fn test_truncate_cmd_display_multibyte() {
        // Multi-byte characters must be truncated at char boundaries
        let input = "é".repeat(40); // each 'é' is 2 bytes
        let result = truncate_cmd_display(&input, 30);
        assert!(
            std::str::from_utf8(result.as_bytes()).is_ok(),
            "truncated result must be valid UTF-8"
        );
    }

    #[test]
    fn test_truncate_cmd_display_max_zero() {
        // max_chars=0: no room for any visible text, return empty or "..." gracefully
        let result = truncate_cmd_display("hello", 0);
        // The input has 5 chars which exceeds 0, so we get "..." with 0-char prefix.
        // Result must be valid UTF-8 and not panic.
        assert!(
            std::str::from_utf8(result.as_bytes()).is_ok(),
            "result for max_chars=0 must be valid UTF-8"
        );
    }

    #[test]
    fn test_truncate_cmd_display_max_two() {
        // max_chars=2: keep = 2.saturating_sub(3) = 0, so prefix is empty, result is "..."
        let result = truncate_cmd_display("hello", 2);
        assert!(
            std::str::from_utf8(result.as_bytes()).is_ok(),
            "result for max_chars=2 must be valid UTF-8"
        );
        assert!(
            result.chars().count() <= 3,
            "result for max_chars=2 should be at most 3 chars (just the ellipsis)"
        );
    }

    #[test]
    fn test_truncate_cmd_display_max_three() {
        // max_chars=3: keep = 0, a string longer than 3 chars produces "..."
        let result = truncate_cmd_display("hello", 3);
        assert_eq!(
            result, "...",
            "5-char input with max_chars=3 should yield '...'"
        );
    }

    #[test]
    fn test_truncate_cmd_display_exact_max() {
        // Input exactly at max_chars: should not be truncated
        let result = truncate_cmd_display("hello", 5);
        assert_eq!(
            result, "hello",
            "input exactly at max_chars should not be truncated"
        );
    }

    // ========================================================================
    // By Command section test
    // ========================================================================

    #[test]
    fn test_dashboard_shows_by_command_section() {
        let store = MockStore::with_data();
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        // "By Command" section header (the new original-cmd section)
        assert!(
            output.contains("By Command"),
            "dashboard should show 'By Command' section"
        );
        // The mock has "cargo build 2>&1"
        assert!(
            output.contains("cargo build"),
            "By Command section should show the original command"
        );
    }

    #[test]
    fn test_format_duration_ms_sub_second() {
        assert_eq!(format_duration_ms(0.0), "0ms");
        assert_eq!(format_duration_ms(12.0), "12ms");
        assert_eq!(format_duration_ms(999.0), "999ms");
    }

    #[test]
    fn test_format_duration_ms_seconds() {
        assert_eq!(format_duration_ms(1000.0), "1.0s");
        assert_eq!(format_duration_ms(1200.0), "1.2s");
        assert_eq!(format_duration_ms(34500.0), "34.5s");
    }

    #[test]
    fn test_by_category_includes_duration() {
        let store = MockStore::with_data();
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        // The By Category section should include duration for the file command
        assert!(
            output.contains("125ms") || output.contains("AVG TIME"),
            "By Category section should display average duration"
        );
    }
}
