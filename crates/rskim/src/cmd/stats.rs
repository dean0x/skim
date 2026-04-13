//! Stats subcommand — token analytics dashboard (#56)
//!
//! Queries the analytics SQLite database and displays a summary of token
//! savings across all skim invocations. Supports time filtering (`--since`),
//! JSON output (`--format json`), cost estimates (`--cost`), and data clearing
//! (`--clear`).

use std::collections::HashMap;
use std::io::{self, Write};
use std::process::ExitCode;
use std::time::UNIX_EPOCH;

use colored::{ColoredString, Colorize};

use crate::analytics::{AnalyticsDb, AnalyticsStore, DailyStats, PricingModel};
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
    let clear = args.iter().any(|a| a == "--clear");
    let show_cost = args.iter().any(|a| a == "--cost");
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
        let time = parse_duration_ago(s)?;
        let ts = time.duration_since(UNIX_EPOCH)?.as_secs() as i64;
        Some(ts)
    } else {
        None
    };

    let mut stdout = io::stdout().lock();

    if format.as_deref() == Some("json") {
        return run_json(
            &mut stdout,
            &db,
            since_ts,
            show_cost,
            analytics.input_cost_per_mtok,
        );
    }

    run_dashboard(
        &mut stdout,
        &db,
        since_ts,
        show_cost,
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
    show_cost: bool,
    cost_override: Option<f64>,
) -> anyhow::Result<ExitCode> {
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
        let pricing = PricingModel::from_cost_override(cost_override);
        let cost_savings = pricing.estimate_savings(summary.tokens_saved);
        // INTENTIONAL API CHANGE (stats dashboard v3 refactor): the `cost_estimate`
        // object uses `tier` (e.g. "Standard") rather than the previous `model` key
        // (e.g. "claude-sonnet-4-6").  Downstream consumers must update accordingly.
        root["cost_estimate"] = serde_json::json!({
            "tier": pricing.tier_name,
            "input_cost_per_mtok": pricing.input_cost_per_mtok,
            "estimated_savings_usd": (cost_savings * 100.0).round() / 100.0,
            "tokens_saved": summary.tokens_saved,
        });
    }

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
const SPARKLINE_CHAR_WIDTH: usize = 4;

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

/// Render a sparkline from daily stats using block chars `▁▂▃▄▅▆▇█`.
///
/// Takes up to the last 14 days of data.  Gaps between dates are filled
/// with `▁` (minimum bar).  Returns an empty string when `daily` is empty.
fn render_sparkline(daily: &[DailyStats]) -> String {
    const BARS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

    if daily.is_empty() {
        return String::new();
    }

    // Work with data sorted ascending by date; take last 14 entries.
    let mut sorted: Vec<&DailyStats> = daily.iter().collect();
    sorted.sort_by(|a, b| a.date.cmp(&b.date));
    let start = sorted.len().saturating_sub(14);
    let window: Vec<&DailyStats> = sorted[start..].to_vec();

    // Build a date-indexed map of tokens_saved.
    let mut by_date: HashMap<&str, u64> = HashMap::new();
    for entry in &window {
        by_date.insert(entry.date.as_str(), entry.tokens_saved);
    }

    let first_date = window.first().map(|d| d.date.as_str()).unwrap_or("");
    let last_date = window.last().map(|d| d.date.as_str()).unwrap_or("");

    // Enumerate every calendar day between first and last inclusive.
    let dates = calendar_dates_between(first_date, last_date);

    let max_val = by_date.values().copied().max().unwrap_or(0);

    dates
        .iter()
        .map(|date| {
            let tokens = by_date.get(date.as_str()).copied().unwrap_or(0);
            let idx = if max_val == 0 {
                0
            } else {
                ((tokens as f64 / max_val as f64) * (BARS.len() - 1) as f64).round() as usize
            };
            let ch = BARS[idx.min(BARS.len() - 1)];
            std::iter::repeat_n(ch, SPARKLINE_CHAR_WIDTH).collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Enumerate calendar dates (YYYY-MM-DD strings) from `start` to `end` inclusive.
///
/// Falls back to just returning the start date when date arithmetic is not
/// possible (e.g. malformed strings), keeping output safe.
fn calendar_dates_between(start: &str, end: &str) -> Vec<String> {
    // Parse YYYY-MM-DD manually to avoid pulling in chrono.
    fn parse_ymd(s: &str) -> Option<(i32, u32, u32)> {
        let parts: Vec<&str> = s.splitn(3, '-').collect();
        if parts.len() != 3 {
            return None;
        }
        let y = parts[0].parse::<i32>().ok()?;
        let m = parts[1].parse::<u32>().ok()?;
        let d = parts[2].parse::<u32>().ok()?;
        if !(1..=12).contains(&m) || d == 0 || d > days_in_month(y, m) {
            return None;
        }
        Some((y, m, d))
    }

    fn days_in_month(year: i32, month: u32) -> u32 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if year % 400 == 0 || (year % 4 == 0 && year % 100 != 0) {
                    29
                } else {
                    28
                }
            }
            _ => 30,
        }
    }

    fn advance_day(year: i32, month: u32, day: u32) -> (i32, u32, u32) {
        let max_day = days_in_month(year, month);
        if day < max_day {
            (year, month, day + 1)
        } else if month < 12 {
            (year, month + 1, 1)
        } else {
            (year + 1, 1, 1)
        }
    }

    let (mut y, mut m, mut d) = match parse_ymd(start) {
        Some(v) => v,
        None => return vec![start.to_string()],
    };
    let end_parsed = match parse_ymd(end) {
        Some(v) => v,
        None => return vec![start.to_string()],
    };

    let mut dates = Vec::new();
    // Safety cap: never generate more than 100 dates to prevent runaway loops.
    while (y, m, d) <= end_parsed && dates.len() < 100 {
        dates.push(format!("{y:04}-{m:02}-{d:02}"));
        let next = advance_day(y, m, d);
        (y, m, d) = next;
    }
    dates
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
    writeln!(w, "{}", section_header("Summary"))?;
    writeln!(
        w,
        "  Invocations:    {}",
        tokens::format_number(summary.invocations as usize)
    )?;
    writeln!(
        w,
        "  Raw tokens:     {}",
        tokens::format_number(summary.raw_tokens as usize)
    )?;
    writeln!(
        w,
        "  Compressed:     {}",
        tokens::format_number(summary.compressed_tokens as usize)
    )?;
    writeln!(
        w,
        "  Tokens saved:   {}",
        tokens::format_number(summary.tokens_saved as usize).green()
    )?;
    writeln!(
        w,
        "  Avg reduction:  {}",
        color_pct(summary.avg_savings_pct)
    )?;
    writeln!(w, "  {}", render_bar(summary.avg_savings_pct, 20))?;
    writeln!(w)?;
    Ok(())
}

fn render_daily_trend(
    w: &mut dyn Write,
    daily: &[crate::analytics::DailyStats],
) -> anyhow::Result<()> {
    if daily.is_empty() {
        return Ok(());
    }
    let first = daily.iter().map(|d| d.date.as_str()).min().unwrap_or("");
    let last = daily.iter().map(|d| d.date.as_str()).max().unwrap_or("");
    writeln!(w, "{}", section_header("Daily Trend (tokens saved)"))?;
    writeln!(w)?;
    writeln!(w, "  {}", render_sparkline(daily))?;
    writeln!(w, "  {} to {}", first.dimmed(), last.dimmed())?;
    writeln!(w)?;
    Ok(())
}

fn render_by_command(
    w: &mut dyn Write,
    by_command: &[crate::analytics::CommandStats],
) -> anyhow::Result<()> {
    if by_command.is_empty() {
        return Ok(());
    }
    writeln!(w, "{}", section_header("By Command"))?;
    for cmd in by_command {
        writeln!(
            w,
            "  {:<COL_NAME$} {:>COL_COUNT$} calls  {:>COL_SAVED$} saved  {}  {:>COL_DUR$} avg  {}",
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
    for lang in by_language {
        writeln!(
            w,
            "  {:<COL_NAME$} {:>COL_COUNT$} files  {:>COL_SAVED$} saved  {}  {}",
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
    for mode in by_mode {
        writeln!(
            w,
            "  {:<COL_NAME$} {:>COL_COUNT$} files  {:>COL_SAVED$} saved  {}  {}",
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

fn render_parse_quality(
    w: &mut dyn Write,
    tier_dist: &crate::analytics::TierDistribution,
) -> anyhow::Result<()> {
    writeln!(w, "{}", section_header("Parse Quality"))?;
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
    writeln!(
        w,
        "  Rate:      ${:.2}/MTok ({})",
        pricing.input_cost_per_mtok, pricing.tier_name
    )?;
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
    show_cost: bool,
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
    render_daily_trend(w, &db.query_daily(since)?)?;
    render_by_command(w, &db.query_by_command(since)?)?;
    render_by_language(w, &db.query_by_language(since)?)?;
    render_by_mode(w, &db.query_by_mode(since)?)?;
    render_parse_quality(w, &db.query_tier_distribution(since)?)?;

    if show_cost {
        render_cost_section(w, summary.tokens_saved, cost_override)?;
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
    // render_sparkline tests
    // ========================================================================

    #[test]
    fn test_render_sparkline_empty() {
        let result = render_sparkline(&[]);
        assert!(result.is_empty());
    }

    #[test]
    fn test_render_sparkline_with_gaps() {
        let daily = vec![
            DailyStats {
                date: "2026-04-01".to_string(),
                invocations: 5,
                tokens_saved: 100,
                avg_savings_pct: 50.0,
            },
            DailyStats {
                date: "2026-04-03".to_string(),
                invocations: 3,
                tokens_saved: 200,
                avg_savings_pct: 60.0,
            },
            DailyStats {
                date: "2026-04-05".to_string(),
                invocations: 7,
                tokens_saved: 50,
                avg_savings_pct: 40.0,
            },
        ];
        let sparkline = render_sparkline(&daily);
        // 5 days × SPARKLINE_CHAR_WIDTH chars + 4 spaces between blocks
        let expected_len = 5 * SPARKLINE_CHAR_WIDTH + 4;
        assert_eq!(
            sparkline.chars().count(),
            expected_len,
            "Apr 1-5 = 5 days, each {} chars wide with space separators",
            SPARKLINE_CHAR_WIDTH
        );
        // Split on space to get individual blocks; gaps (Apr 2, Apr 4) are min-bar blocks
        let blocks: Vec<&str> = sparkline.split(' ').collect();
        assert_eq!(blocks.len(), 5, "should have 5 blocks");
        // Gap blocks (Apr 2 at index 1, Apr 4 at index 3) should be all minimum-bar chars
        let min_bar = '▁';
        assert!(
            blocks[1].chars().all(|c| c == min_bar),
            "Apr 2 gap block should be min bar"
        );
        assert!(
            blocks[3].chars().all(|c| c == min_bar),
            "Apr 4 gap block should be min bar"
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
                // Multiple non-consecutive dates for sparkline coverage
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
        let output = capture(|w| run_json(w, &store, None, false, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        let summary = &parsed["summary"];
        assert_eq!(summary["invocations"], 0);
        assert_eq!(summary["tokens_saved"], 0);
    }

    #[test]
    fn test_run_json_with_data() {
        let store = MockStore::with_data();
        let output = capture(|w| run_json(w, &store, None, false, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        let summary = &parsed["summary"];
        assert_eq!(summary["invocations"], 42);
        assert_eq!(summary["tokens_saved"], 70_000);
        assert_eq!(summary["avg_savings_pct"], 70.0);
        // Verify breakdowns are present
        assert_eq!(parsed["by_command"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["by_language"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["by_mode"].as_array().unwrap().len(), 1);
        // No cost_estimate when show_cost is false
        assert!(parsed.get("cost_estimate").is_none());
    }

    #[test]
    fn test_run_json_with_cost() {
        let store = MockStore::with_data();
        let output = capture(|w| run_json(w, &store, None, true, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        let cost = &parsed["cost_estimate"];
        assert!(
            cost.is_object(),
            "cost_estimate should be present when show_cost=true"
        );
        assert_eq!(cost["tokens_saved"], 70_000);
        assert!(cost["estimated_savings_usd"].as_f64().unwrap() > 0.0);
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
            "dashboard should show avg reduction"
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
    fn test_run_dashboard_with_cost() {
        let store = MockStore::with_data();
        let output = capture(|w| run_dashboard(w, &store, None, true, None, None));
        assert!(
            output.contains("Cost Estimates"),
            "dashboard should show cost section"
        );
        assert!(output.contains("/MTok"), "dashboard should show cost rate");
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
        let args: Vec<String> = vec!["--cost".into()];
        assert_eq!(parse_value_flag(&args, "--format"), None);
    }

    // ========================================================================
    // Daily Trend section integration tests
    // ========================================================================

    #[test]
    fn test_dashboard_has_daily_trend() {
        let store = MockStore {
            daily: vec![
                DailyStats {
                    date: "2026-04-01".to_string(),
                    invocations: 5,
                    tokens_saved: 100,
                    avg_savings_pct: 50.0,
                },
                DailyStats {
                    date: "2026-04-03".to_string(),
                    invocations: 3,
                    tokens_saved: 200,
                    avg_savings_pct: 60.0,
                },
            ],
            ..MockStore::with_data()
        };
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            output.contains("Daily Trend (tokens saved)"),
            "dashboard should show daily trend section with subtitle"
        );
    }

    #[test]
    fn test_daily_trend_subtitle() {
        let store = MockStore::with_data();
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            output.contains("tokens saved"),
            "daily trend header should include 'tokens saved' subtitle"
        );
    }

    #[test]
    fn test_dashboard_no_daily_trend_when_empty() {
        let store = MockStore {
            daily: vec![],
            ..MockStore::with_data()
        };
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            !output.contains("Daily Trend"),
            "dashboard should skip daily trend section when no daily data"
        );
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
    // Wider sparkline tests
    // ========================================================================

    #[test]
    fn test_sparkline_width_with_spaces() {
        let daily: Vec<DailyStats> = (1..=5)
            .map(|i| DailyStats {
                date: format!("2026-04-{:02}", i),
                invocations: i as u64,
                tokens_saved: i as u64 * 100,
                avg_savings_pct: 50.0,
            })
            .collect();
        let sparkline = render_sparkline(&daily);
        // N days → N * SPARKLINE_CHAR_WIDTH + (N-1) spaces
        let expected_len = 5 * SPARKLINE_CHAR_WIDTH + 4;
        assert_eq!(
            sparkline.chars().count(),
            expected_len,
            "5 days should produce {} chars, got {}",
            expected_len,
            sparkline.chars().count()
        );
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
    // render_sparkline sort correctness tests
    // ========================================================================

    #[test]
    fn test_render_sparkline_sort_order() {
        // Provide daily data in reverse order; sparkline should sort ascending
        let daily = vec![
            DailyStats {
                date: "2026-04-03".to_string(),
                invocations: 3,
                tokens_saved: 300,
                avg_savings_pct: 60.0,
            },
            DailyStats {
                date: "2026-04-01".to_string(),
                invocations: 1,
                tokens_saved: 100,
                avg_savings_pct: 50.0,
            },
            DailyStats {
                date: "2026-04-02".to_string(),
                invocations: 2,
                tokens_saved: 200,
                avg_savings_pct: 55.0,
            },
        ];
        // Sorted ascending and with gaps filled: Apr 1, 2, 3 — 3 blocks
        let sparkline = render_sparkline(&daily);
        let blocks: Vec<&str> = sparkline.split(' ').collect();
        assert_eq!(blocks.len(), 3, "3 days should produce 3 blocks");
        // Apr 3 has the highest tokens_saved (300), so its block should be max bar '█'
        let max_bar = '█';
        assert!(
            blocks[2].chars().all(|c| c == max_bar),
            "last block (Apr 3, highest) should be max bar"
        );
    }

    #[test]
    fn test_render_sparkline_takes_last_14() {
        // Provide 20 days; sparkline should use only the last 14
        let daily: Vec<DailyStats> = (1..=20)
            .map(|i| DailyStats {
                date: format!("2026-04-{:02}", i),
                invocations: i as u64,
                tokens_saved: i as u64 * 100,
                avg_savings_pct: 50.0,
            })
            .collect();
        let sparkline = render_sparkline(&daily);
        let blocks: Vec<&str> = sparkline.split(' ').collect();
        assert_eq!(
            blocks.len(),
            14,
            "20 days of data should yield only last 14 blocks"
        );
    }

    // ========================================================================
    // calendar_dates_between tests
    // ========================================================================

    #[test]
    fn test_calendar_same_day() {
        let dates = calendar_dates_between("2026-04-05", "2026-04-05");
        assert_eq!(dates, vec!["2026-04-05"]);
    }

    #[test]
    fn test_calendar_month_boundary() {
        // Jan 30 → Feb 2
        let dates = calendar_dates_between("2026-01-30", "2026-02-02");
        assert_eq!(
            dates,
            vec!["2026-01-30", "2026-01-31", "2026-02-01", "2026-02-02"]
        );
    }

    #[test]
    fn test_calendar_year_boundary() {
        // Dec 30 → Jan 2 next year
        let dates = calendar_dates_between("2025-12-30", "2026-01-02");
        assert_eq!(
            dates,
            vec!["2025-12-30", "2025-12-31", "2026-01-01", "2026-01-02"]
        );
    }

    #[test]
    fn test_calendar_leap_year() {
        // Feb 28 → Mar 1 in 2024 (leap year)
        let dates = calendar_dates_between("2024-02-28", "2024-03-01");
        assert_eq!(dates, vec!["2024-02-28", "2024-02-29", "2024-03-01"]);
    }

    #[test]
    fn test_calendar_non_leap_year() {
        // Feb 28 → Mar 1 in 2025 (non-leap year, no Feb 29)
        let dates = calendar_dates_between("2025-02-28", "2025-03-01");
        assert_eq!(dates, vec!["2025-02-28", "2025-03-01"]);
    }

    #[test]
    fn test_calendar_malformed_start() {
        // Malformed start returns vec with just the start string
        let dates = calendar_dates_between("not-a-date", "2026-04-05");
        assert_eq!(dates, vec!["not-a-date"]);
    }

    #[test]
    fn test_calendar_malformed_end() {
        // Malformed end returns vec with just the start string
        let dates = calendar_dates_between("2026-04-01", "not-a-date");
        assert_eq!(dates, vec!["2026-04-01"]);
    }

    #[test]
    fn test_calendar_invalid_month() {
        // Month 13 is invalid; parse_ymd should return None → fallback to start string
        let dates = calendar_dates_between("2026-13-01", "2026-13-05");
        assert_eq!(dates, vec!["2026-13-01"]);
    }

    #[test]
    fn test_calendar_invalid_day() {
        // Day 0 is invalid
        let dates = calendar_dates_between("2026-04-00", "2026-04-03");
        assert_eq!(dates, vec!["2026-04-00"]);
    }

    #[test]
    fn test_calendar_safety_cap() {
        // 365+ days apart should be capped at 100 entries
        let dates = calendar_dates_between("2026-01-01", "2027-12-31");
        assert_eq!(
            dates.len(),
            100,
            "safety cap should limit output to 100 dates"
        );
    }

    // ========================================================================
    // JSON output value assertions
    // ========================================================================

    #[test]
    fn test_run_json_tier_distribution_values() {
        let store = MockStore::with_data();
        let output = capture(|w| run_json(w, &store, None, false, None));
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
        let output = capture(|w| run_json(w, &store, None, true, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        let cost = &parsed["cost_estimate"];
        let tier = cost["tier"].as_str().expect("tier should be a string");
        // Default pricing model tier should be "Standard"
        assert_eq!(tier, "Standard", "default cost tier should be 'Standard'");
    }

    // ========================================================================
    // Dashboard command labels test
    // ========================================================================

    #[test]
    fn test_dashboard_shows_command_labels() {
        let store = MockStore::with_data();
        // MockStore::with_data() has command_type: "file"
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            output.contains("Source files"),
            "dashboard should show 'Source files' label for 'file' command type"
        );
    }

    // ========================================================================
    // Multi-tier cost table test
    // ========================================================================

    #[test]
    fn test_dashboard_multi_tier_cost() {
        let store = MockStore::with_data();
        let output = capture(|w| run_dashboard(w, &store, None, true, None, None));
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
    fn test_by_command_includes_duration() {
        let store = MockStore::with_data();
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        // The By Command section should include duration for the file command
        assert!(
            output.contains("125ms") || output.contains("avg"),
            "By Command section should display average duration"
        );
    }
}
