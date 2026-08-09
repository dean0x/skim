//! Stats subcommand — token analytics dashboard (#56)
//!
//! Queries the analytics SQLite database and displays a summary of token
//! savings across all skim invocations. Supports time filtering (`--since`),
//! JSON output (`--format json`), verbose parse-quality output (`--verbose`),
//! and data clearing (`--clear`). Cost estimates are always shown.

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::UNIX_EPOCH;

// Uses `Colorize` directly for value/header formatting (green numbers,
// bold labels). The `ux` module wraps mark primitives (+/-) only;
// arbitrary value coloring is intentionally not centralised.
use colored::{ColoredString, Colorize};

use crate::analytics::{
    AnalyticsDb, AnalyticsStore, OriginalCommandStats, PricingModel, ProxyModelStats,
    ProxyProviderStats, SessionStats,
};
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
    println!("  --verbose, -v         Show per-session and parse quality sections");
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
    let session_stats = db.query_session_stats(since)?;

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

    // AC10 / AD-AN-9: proxy section is present only when proxy rows exist.
    // AD-AN-9: ordering is guaranteed by query_by_model (NULL-last SQL ORDER BY),
    // so identical row sets produce byte-identical JSON (AC13).
    //
    // AC17 / AD-AN-8: a non-zero drop counter also materialises the section even
    // when no proxy row survived — that is precisely the case the disclosure
    // exists for.  With no proxy activity at all the section stays `null`, so
    // pre-#305 output is unchanged (AC10).
    let by_model = db.query_by_model(since)?;
    let upstream_errors = db.query_by_upstream_error(since)?;
    let dropped_records = db.query_proxy_dropped_records()?;
    let proxy_section = if !by_model.is_empty() || upstream_errors > 0 || dropped_records > 0 {
        let by_provider = db.query_by_provider(since)?;

        // Serialise by_model: per-row basis disclosure + uncounted_rows (AC12).
        // AD-AN-9: mixed-basis provider rows carry null token sums (already
        // encoded as Option<u64> None in the struct).
        let model_json: Vec<serde_json::Value> = by_model
            .iter()
            .map(|r| {
                serde_json::json!({
                    "provider": r.provider,
                    "model": r.model,
                    "requests": r.requests,
                    "upstream_errors": r.upstream_errors,
                    "raw_tokens": r.raw_tokens,
                    "compressed_tokens": r.compressed_tokens,
                    "avg_savings_pct": r.avg_savings_pct,
                    "tier_full_pct": r.tier_full_pct,
                    "tier_degraded_pct": r.tier_degraded_pct,
                    "tier_passthrough_pct": r.tier_passthrough_pct,
                    "basis": r.basis,
                    "counted_rows": r.counted_rows,
                    "uncounted_rows": r.uncounted_rows,
                })
            })
            .collect();

        // AD-AN-9: provider rollup spans multiple bases → token sums are null
        // (mixed-basis; combining would be meaningless).
        let provider_json: Vec<serde_json::Value> = by_provider
            .iter()
            .map(|r| {
                serde_json::json!({
                    "provider": r.provider,
                    "requests": r.requests,
                    "upstream_errors": r.upstream_errors,
                    "raw_tokens": r.raw_tokens,
                    "compressed_tokens": r.compressed_tokens,
                    "avg_savings_pct": r.avg_savings_pct,
                    "tier_full_pct": r.tier_full_pct,
                    "tier_degraded_pct": r.tier_degraded_pct,
                    "tier_passthrough_pct": r.tier_passthrough_pct,
                    "basis": r.basis,
                    "counted_rows": r.counted_rows,
                    "uncounted_rows": r.uncounted_rows,
                })
            })
            .collect();

        serde_json::json!({
            "by_provider": provider_json,
            "by_model": model_json,
            // AD-PXY-25: upstream-errored rows excluded from savings/tier aggregates;
            // count is surfaced separately so consumers can distinguish relay success
            // from total requests.
            "upstream_errors": upstream_errors,
            "dropped_records": dropped_records,
        })
    } else {
        serde_json::Value::Null
    };

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
        "session_stats": {
            "distinct_sessions": session_stats.distinct_sessions,
            "total_tokens_saved": session_stats.total_tokens_saved,
            "avg_tokens_per_session": session_stats.avg_tokens_per_session,
            "untagged_invocations": session_stats.untagged_invocations,
        },
        "cost_estimate": cost_estimate,
        // AC10: null when no proxy rows; omitting entirely vs null is a protocol
        // choice — using null keeps the key stable for downstream consumers.
        "proxy": proxy_section,
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
    session_stats: &SessionStats,
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
    if session_stats.distinct_sessions > 0 {
        writeln!(
            w,
            "  Avg/session:  {}",
            tokens::format_number(session_stats.avg_tokens_per_session.round() as usize).green()
        )?;
    }
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

/// Render per-session analytics summary.
///
/// AD-AN-2: Displays distinct session count, total tokens saved across sessions,
/// average tokens saved per session, and untagged invocation count.
/// Skipped when no session data is present (distinct_sessions == 0 and
/// untagged_invocations == 0) to avoid cluttering the dashboard for users
/// who have not installed the hook or enabled session tracking.
fn render_session_stats(w: &mut dyn Write, stats: &SessionStats) -> anyhow::Result<()> {
    if stats.distinct_sessions == 0 && stats.untagged_invocations == 0 {
        return Ok(());
    }
    writeln!(w, "{}", section_header("Per Session"))?;
    writeln!(w)?;
    if stats.distinct_sessions > 0 {
        writeln!(
            w,
            "  Sessions tracked:   {}",
            tokens::format_number(stats.distinct_sessions as usize)
        )?;
        writeln!(
            w,
            "  Total tokens saved: {}",
            tokens::format_number(stats.total_tokens_saved as usize).green()
        )?;
    }
    if stats.untagged_invocations > 0 {
        writeln!(
            w,
            "  Untagged calls:     {}",
            tokens::format_number(stats.untagged_invocations as usize)
        )?;
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
// Proxy dashboard — section renderers
// ============================================================================

/// Format an optional token count; None renders as a dash.
fn fmt_opt_tokens(v: Option<u64>) -> String {
    match v {
        Some(n) => format_tokens(n),
        None => "-".to_string(),
    }
}

/// Render the per-provider proxy breakdown.
///
/// AD-AN-9: per-(provider,model) is the authoritative token-sum unit; the
/// provider row omits the combined token figure when `basis == "mixed"` (models
/// span different tokenizers and the sum would be meaningless).
/// AD-PXY-25: upstream_errors is shown separately — those rows are excluded
/// from savings/tier aggregates.
/// Render an untrusted string into a fixed-width table cell.
///
/// The `model` column carries **verbatim client-supplied text** (AD-PXY-22):
/// the proxy stores whatever a caller put in the request body's `"model"` key.
/// It therefore reaches this renderer unvalidated, and two hazards must close
/// here:
///
/// - **Panic.** `&s[..n]` slices by *byte* index and panics when byte `n` is not
///   a UTF-8 character boundary, so any multi-byte model string long enough to
///   be truncated would crash `skim stats`. Taking `max` *chars* is boundary-safe
///   by construction.
/// - **Terminal injection.** Control characters — ANSI escape introducers,
///   `\r`, `\n` — would be interpreted by the operator's terminal and could
///   rewrite the rendered table. Each is replaced with U+FFFD (A03: escape
///   output for its context).
fn display_cell(s: &str, max: usize) -> String {
    s.chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .take(max)
        .collect()
}

fn render_proxy_by_provider(
    w: &mut dyn Write,
    rows: &[ProxyProviderStats],
) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    writeln!(w, "{}", section_header("Proxy — By Provider"))?;
    writeln!(w)?;
    // Column widths: provider(16) | reqs(6) | errors(6) | tokens_raw(10) |
    //                tokens_cmp(10) | avg_savings(9) | basis(14) | uncounted(9)
    writeln!(
        w,
        "  {:<16}  {:>6}  {:>6}  {:>10}  {:>10}  {:<9}  {:<14}  {:>9}",
        "PROVIDER", "REQS", "ERRORS", "RAW", "COMPRESSED", "REDUCTION", "BASIS", "UNCOUNTED"
    )?;
    for row in rows {
        let provider = row.provider.as_deref().unwrap_or("(unknown)");
        let avg_pct = row.avg_savings_pct.unwrap_or(0.0);
        writeln!(
            w,
            "  {:<16}  {:>6}  {:>6}  {:>10}  {:>10}  {}  {:<14}  {:>9}",
            display_cell(provider, 16),
            tokens::format_number(row.requests as usize),
            tokens::format_number(row.upstream_errors as usize),
            fmt_opt_tokens(row.raw_tokens),
            fmt_opt_tokens(row.compressed_tokens),
            color_pct(avg_pct),
            row.basis,
            tokens::format_number(row.uncounted_rows as usize),
        )?;
    }
    writeln!(w)?;
    Ok(())
}

/// Render the per-model proxy breakdown.
///
/// AD-AN-9 / AC10: rendered only when at least one proxy row exists.
/// AD-PXY-25: upstream_errors shown separately from success-scope metrics.
/// AC12: uncounted_rows disclosed alongside basis label.
fn render_proxy_by_model(
    w: &mut dyn Write,
    rows: &[ProxyModelStats],
) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    writeln!(w, "{}", section_header("Proxy — By Model"))?;
    writeln!(w)?;
    writeln!(
        w,
        "  {:<16}  {:<26}  {:>6}  {:>6}  {:>10}  {:>10}  {:<9}  {:<14}  {:>9}",
        "PROVIDER", "MODEL", "REQS", "ERRORS", "RAW", "COMPRESSED", "REDUCTION", "BASIS", "UNCOUNTED"
    )?;
    for row in rows {
        let provider = row.provider.as_deref().unwrap_or("(unknown)");
        let model = row.model.as_deref().unwrap_or("(unknown)");
        let avg_pct = row.avg_savings_pct.unwrap_or(0.0);
        writeln!(
            w,
            "  {:<16}  {:<26}  {:>6}  {:>6}  {:>10}  {:>10}  {}  {:<14}  {:>9}",
            display_cell(provider, 16),
            display_cell(model, 26),
            tokens::format_number(row.requests as usize),
            tokens::format_number(row.upstream_errors as usize),
            fmt_opt_tokens(row.raw_tokens),
            fmt_opt_tokens(row.compressed_tokens),
            color_pct(avg_pct),
            row.basis,
            tokens::format_number(row.uncounted_rows as usize),
        )?;
    }
    writeln!(w)?;
    Ok(())
}

/// Render upstream-error count (AD-PXY-25).
///
/// AD-PXY-25: upstream-errored rows are excluded from savings/tier aggregates
/// and reported separately so the operator can distinguish "proxy saw the
/// request" from "savings were computed for the request".
fn render_proxy_upstream_errors(w: &mut dyn Write, count: u64) -> anyhow::Result<()> {
    if count > 0 {
        writeln!(
            w,
            "  Upstream errors:     {} (excluded from savings aggregates)",
            tokens::format_number(count as usize)
        )?;
    }
    Ok(())
}

/// Render dropped-record count (AD-AN-8).
///
/// Dropped records are rows that were queued but never persisted (queue
/// overflow). The count is monotonically accumulated via
/// `analytics_meta_add_drop_count` and surfaced here when nonzero.
fn render_proxy_dropped_records(w: &mut dyn Write, count: u64) -> anyhow::Result<()> {
    if count > 0 {
        writeln!(
            w,
            "  Dropped records:     {} (recording queue overflowed)",
            tokens::format_number(count as usize)
        )?;
    }
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

    // AD-AN-6: `summary` is CLI-scope — it excludes `command_type = 'proxy'`
    // rows.  The empty-dashboard shortcut must therefore also consult the proxy
    // scope, otherwise a proxy-only database (AC9) would print "No analytics
    // data found." and the per-provider/per-model breakdowns would be
    // unreachable in text output.  `dropped_records` is included so a run whose
    // events were all dropped still discloses the counter (AC17 / AD-AN-8).
    let by_model = db.query_by_model(since)?;
    let dropped_records = db.query_proxy_dropped_records()?;

    if summary.invocations == 0 && by_model.is_empty() && dropped_records == 0 {
        writeln!(w, "{}", "No analytics data found.".dimmed())?;
        writeln!(w)?;
        writeln!(
            w,
            "Run skim commands to start collecting token savings data."
        )?;
        writeln!(w, "Example: skim src/main.rs")?;
        return Ok(ExitCode::SUCCESS);
    }

    let session_stats = db.query_session_stats(since)?;

    let period = since_str.map_or("all time".to_string(), |s| format!("last {s}"));
    render_header(w, &period)?;
    render_summary(w, &summary, &session_stats)?;
    render_by_category(w, &db.query_by_command(since)?)?;
    render_by_language(w, &db.query_by_language(since)?)?;
    render_by_mode(w, &db.query_by_mode(since)?)?;
    render_by_original_cmd(w, &db.query_by_original_cmd(since)?)?;
    if verbose {
        render_session_stats(w, &session_stats)?;
        render_parse_quality(w, &db.query_tier_distribution(since)?)?;
    }

    // AC10: proxy sections only when proxy rows exist (render-when-present).
    // AD-PXY-25 / AD-AN-9: upstream-error and dropped-record counts are surfaced
    // alongside the breakdown tables.
    if !by_model.is_empty() {
        let by_provider = db.query_by_provider(since)?;
        render_proxy_by_provider(w, &by_provider)?;
        render_proxy_by_model(w, &by_model)?;
    }

    // Inline advisory lines: upstream errors + dropped records.
    //
    // AC17 / AD-AN-8: the drop counter is the disclosure that justifies the
    // bounded-default queue constants, so it is rendered independently of
    // whether any proxy row survived — a run whose every event was dropped has
    // an empty `by_model` and is exactly the case that must not stay silent.
    // Both lines render only when non-zero, so a database with no proxy
    // activity at all still produces byte-identical pre-#305 output (AC10).
    let upstream_errors = db.query_by_upstream_error(since)?;
    if upstream_errors > 0 || dropped_records > 0 {
        writeln!(w, "{}", section_header("Proxy — Notices"))?;
        writeln!(w)?;
        render_proxy_upstream_errors(w, upstream_errors)?;
        render_proxy_dropped_records(w, dropped_records)?;
        writeln!(w)?;
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
        session_stats: SessionStats,
        // Phase 3: proxy query fields (default empty / zero)
        by_model: Vec<ProxyModelStats>,
        by_provider: Vec<ProxyProviderStats>,
        upstream_errors: u64,
        dropped_records: u64,
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
                session_stats: SessionStats {
                    distinct_sessions: 0,
                    total_tokens_saved: 0,
                    avg_tokens_per_session: 0.0,
                    untagged_invocations: 0,
                },
                by_model: vec![],
                by_provider: vec![],
                upstream_errors: 0,
                dropped_records: 0,
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
                session_stats: SessionStats {
                    distinct_sessions: 0,
                    total_tokens_saved: 0,
                    avg_tokens_per_session: 0.0,
                    untagged_invocations: 0,
                },
                by_model: vec![],
                by_provider: vec![],
                upstream_errors: 0,
                dropped_records: 0,
            }
        }

        /// Construct a MockStore variant that has session data for testing the Per Session section.
        fn with_sessions() -> Self {
            let mut s = Self::with_data();
            s.session_stats = SessionStats {
                distinct_sessions: 5,
                total_tokens_saved: 50_000,
                avg_tokens_per_session: 10_000.0,
                untagged_invocations: 12,
            };
            s
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
        fn query_session_stats(&self, _since: Option<i64>) -> anyhow::Result<SessionStats> {
            Ok(self.session_stats.clone())
        }
        fn clear(&self) -> anyhow::Result<()> {
            Ok(())
        }
        fn query_by_model(
            &self,
            _since: Option<i64>,
        ) -> anyhow::Result<Vec<ProxyModelStats>> {
            Ok(self.by_model.clone())
        }
        fn query_by_provider(
            &self,
            _since: Option<i64>,
        ) -> anyhow::Result<Vec<ProxyProviderStats>> {
            Ok(self.by_provider.clone())
        }
        fn query_by_upstream_error(&self, _since: Option<i64>) -> anyhow::Result<u64> {
            Ok(self.upstream_errors)
        }
        fn query_proxy_dropped_records(&self) -> anyhow::Result<u64> {
            Ok(self.dropped_records)
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

    /// AC9 (PF-007 discriminating): a proxy-only database renders the proxy
    /// breakdowns with a zero CLI headline — it must NOT take the
    /// "No analytics data found." shortcut.
    ///
    /// `query_summary` is CLI-scope (AD-AN-6), so a proxy-only database has
    /// `invocations == 0`.  Restoring the bare `summary.invocations == 0`
    /// early-return blanks the entire proxy scope in text output and fails this
    /// test.
    #[test]
    fn test_run_dashboard_proxy_only_store_renders_proxy_scope() {
        let mut store = MockStore::empty();
        store.by_model = vec![proxy_model_row(
            Some("anthropic"),
            Some("claude-3-5-sonnet-20241022"),
            5,
            Some(10_000),
            Some(4_000),
            Some(60.0),
            "approximation",
            0,
            0,
        )];
        store.by_provider = vec![proxy_provider_row(
            Some("anthropic"),
            5,
            Some(10_000),
            Some(4_000),
            Some(60.0),
            "approximation",
            0,
        )];

        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            !output.contains("No analytics data found"),
            "a proxy-only database must not report an empty dashboard; got:\n{output}"
        );
        assert!(
            output.contains("claude-3-5-sonnet-20241022"),
            "proxy per-model breakdown must render for a proxy-only database; got:\n{output}"
        );
        assert!(
            output.contains("Invocations:  0"),
            "the CLI headline must stay zero for a proxy-only database (AC9); got:\n{output}"
        );
    }

    /// PF-007 discriminating: a multi-byte `model` string must not panic the
    /// renderer, and control characters must not reach the terminal.
    ///
    /// `model` is verbatim client-supplied text (AD-PXY-22), so any proxy caller
    /// controls it. Restoring the byte slice `&model[..model.len().min(26)]`
    /// panics on the CJK row ("byte index 26 is not a char boundary"); dropping
    /// the control-character replacement lets the ESC byte through and fails the
    /// second assertion.
    #[test]
    fn test_proxy_render_survives_hostile_model_strings() {
        let mut store = MockStore::empty();
        store.by_model = vec![
            // 30 CJK chars — byte 26 falls mid-character.
            proxy_model_row(
                Some("openai"),
                Some(&"日".repeat(30)),
                1,
                Some(10),
                Some(5),
                Some(50.0),
                "exact",
                0,
                0,
            ),
            // ANSI escape + newline injection attempt.
            proxy_model_row(
                Some("openai"),
                Some("gpt\u{1b}[31m-evil\nINJECTED"),
                1,
                Some(10),
                Some(5),
                Some(50.0),
                "exact",
                0,
                0,
            ),
        ];
        store.by_provider = vec![proxy_provider_row(
            Some("openai"),
            2,
            Some(20),
            Some(10),
            Some(50.0),
            "exact",
            0,
        )];

        // Must not panic.
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));

        assert!(
            !output.contains('\u{1b}'),
            "an ESC byte from a client-supplied model string must never reach the terminal"
        );
        assert!(
            !output.contains("\nINJECTED"),
            "a newline inside a model cell must not start a forged output line"
        );
    }

    /// AC17 (PF-007 discriminating): a non-zero drop counter is disclosed even
    /// when no proxy row survived to be recorded.
    ///
    /// The drop counter is the disclosure that justifies the bounded-default
    /// queue constants (ADR-003), so gating it behind `!by_model.is_empty()`
    /// silences it in exactly the case it exists for.  Restoring that gate
    /// fails this test.
    #[test]
    fn test_run_dashboard_surfaces_drops_with_no_surviving_proxy_rows() {
        let mut store = MockStore::empty();
        store.dropped_records = 17;

        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            output.contains("Dropped records"),
            "a non-zero drop total must be surfaced even with zero proxy rows; got:\n{output}"
        );
        assert!(
            output.contains("17"),
            "the drop total itself must be shown; got:\n{output}"
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
            output.contains("Advanced"),
            "cost section should show Advanced tier"
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
        let empty_sessions = SessionStats {
            distinct_sessions: 0,
            total_tokens_saved: 0,
            avg_tokens_per_session: 0.0,
            untagged_invocations: 0,
        };
        let mut buf = Vec::new();
        render_summary(&mut buf, &summary, &empty_sessions).expect("render should not fail");
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

    // ========================================================================
    // B8: AD-AN-2 — render_session_stats and JSON session_stats field
    // ========================================================================

    /// AD-AN-2: "Per Session" section is hidden when both counts are zero.
    #[test]
    fn test_render_session_stats_hidden_when_empty() {
        let stats = SessionStats {
            distinct_sessions: 0,
            total_tokens_saved: 0,
            avg_tokens_per_session: 0.0,
            untagged_invocations: 0,
        };
        let mut buf = Vec::new();
        render_session_stats(&mut buf, &stats).expect("should not fail");
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.is_empty(),
            "Per Session section should produce no output when all counts are zero"
        );
    }

    /// AD-AN-2: "Per Session" section is shown when distinct_sessions > 0.
    #[test]
    fn test_render_session_stats_shown_with_sessions() {
        let stats = SessionStats {
            distinct_sessions: 5,
            total_tokens_saved: 50_000,
            avg_tokens_per_session: 10_000.0,
            untagged_invocations: 0,
        };
        let mut buf = Vec::new();
        render_session_stats(&mut buf, &stats).expect("should not fail");
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("Per Session"),
            "Per Session header should appear when distinct_sessions > 0"
        );
        assert!(
            output.contains("Sessions tracked"),
            "should show 'Sessions tracked' label"
        );
        assert!(
            output.contains("50,000") || output.contains("50K"),
            "should show total tokens saved"
        );
    }

    /// AD-AN-2: "Per Session" section is shown when only untagged_invocations > 0.
    #[test]
    fn test_render_session_stats_shown_with_untagged_only() {
        let stats = SessionStats {
            distinct_sessions: 0,
            total_tokens_saved: 0,
            avg_tokens_per_session: 0.0,
            untagged_invocations: 7,
        };
        let mut buf = Vec::new();
        render_session_stats(&mut buf, &stats).expect("should not fail");
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("Per Session"),
            "Per Session header should appear when untagged_invocations > 0"
        );
        assert!(
            output.contains("Untagged calls"),
            "should show 'Untagged calls' label"
        );
    }

    /// AD-AN-2: "Untagged calls" line is hidden when untagged_invocations == 0.
    #[test]
    fn test_render_session_stats_untagged_hidden_when_zero() {
        let stats = SessionStats {
            distinct_sessions: 3,
            total_tokens_saved: 1000,
            avg_tokens_per_session: 333.0,
            untagged_invocations: 0,
        };
        let mut buf = Vec::new();
        render_session_stats(&mut buf, &stats).expect("should not fail");
        let output = String::from_utf8(buf).unwrap();
        assert!(
            !output.contains("Untagged calls"),
            "Untagged calls line should be hidden when untagged_invocations == 0"
        );
    }

    /// Per Session section is hidden in default (non-verbose) mode even with data.
    #[test]
    fn test_dashboard_hides_per_session_in_default_mode() {
        let store = MockStore::with_sessions();
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            !output.contains("Per Session"),
            "Per Session section should be hidden in default mode"
        );
    }

    /// Per Session section appears in verbose mode when session data is present.
    #[test]
    fn test_dashboard_shows_per_session_in_verbose_mode() {
        let store = MockStore::with_sessions();
        let output = capture(|w| run_dashboard(w, &store, None, true, None, None));
        assert!(
            output.contains("Per Session"),
            "verbose mode should show Per Session section when session data is present"
        );
        assert!(
            output.contains("Sessions tracked"),
            "verbose Per Session section should show tracked sessions count"
        );
        assert!(
            output.contains("Untagged calls"),
            "verbose Per Session section should show untagged calls"
        );
    }

    /// Per Session section is hidden in verbose mode when no session data.
    #[test]
    fn test_dashboard_hides_per_session_in_verbose_when_empty() {
        let store = MockStore::with_data(); // session_stats all zeros
        let output = capture(|w| run_dashboard(w, &store, None, true, None, None));
        assert!(
            !output.contains("Per Session"),
            "verbose mode should NOT show Per Session section when session data is all zeros"
        );
    }

    /// Avg/session appears in Summary section when session data exists.
    #[test]
    fn test_summary_shows_avg_per_session() {
        let store = MockStore::with_sessions();
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            output.contains("Avg/session"),
            "Summary should show Avg/session when session data is present"
        );
        assert!(
            output.contains("10,000"),
            "Summary Avg/session should show 10,000"
        );
    }

    /// Avg/session is omitted from Summary when no sessions tracked.
    #[test]
    fn test_summary_hides_avg_per_session_when_no_sessions() {
        let store = MockStore::with_data(); // session_stats all zeros
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            !output.contains("Avg/session"),
            "Summary should NOT show Avg/session when no sessions tracked"
        );
    }

    /// Verbose Per Session section no longer shows "Avg per session" (promoted to Summary).
    #[test]
    fn test_verbose_per_session_excludes_avg() {
        let store = MockStore::with_sessions();
        let output = capture(|w| run_dashboard(w, &store, None, true, None, None));
        // Find the Per Session section and check it doesn't contain "Avg per session"
        let per_session_start = output.find("Per Session").expect("should have Per Session");
        let after_per_session = &output[per_session_start..];
        assert!(
            !after_per_session.contains("Avg per session"),
            "Per Session section should NOT contain 'Avg per session' (promoted to Summary)"
        );
    }

    /// AD-AN-2: JSON output includes session_stats object with correct fields.
    #[test]
    fn test_run_json_includes_session_stats_field() {
        let store = MockStore::with_sessions();
        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        let ss = &parsed["session_stats"];
        assert!(
            ss.is_object(),
            "JSON output must include 'session_stats' object"
        );
        assert_eq!(
            ss["distinct_sessions"].as_u64().unwrap(),
            5,
            "distinct_sessions should be 5"
        );
        assert_eq!(
            ss["total_tokens_saved"].as_u64().unwrap(),
            50_000,
            "total_tokens_saved should be 50000"
        );
        assert!(
            (ss["avg_tokens_per_session"].as_f64().unwrap() - 10_000.0).abs() < 1.0,
            "avg_tokens_per_session should be ~10000"
        );
        assert_eq!(
            ss["untagged_invocations"].as_u64().unwrap(),
            12,
            "untagged_invocations should be 12"
        );
    }

    /// AD-AN-2: JSON session_stats field is always present (even when zero).
    #[test]
    fn test_run_json_session_stats_present_when_empty() {
        let store = MockStore::with_data(); // session_stats all zeros
        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        let ss = &parsed["session_stats"];
        assert!(
            ss.is_object(),
            "session_stats must always be present in JSON output, even when zero"
        );
        assert_eq!(ss["distinct_sessions"].as_u64().unwrap(), 0);
        assert_eq!(ss["untagged_invocations"].as_u64().unwrap(), 0);
    }

    // ========================================================================
    // Phase 3: proxy dashboard rendering tests (AC9–AC13, AC25)
    // ========================================================================

    /// Build a minimal ProxyModelStats row for MockStore tests.
    // Nine positional args mirror all fields of ProxyModelStats — grouping them
    // into a sub-struct would move the same fields to the call sites unchanged.
    #[allow(clippy::too_many_arguments)]
    fn proxy_model_row(
        provider: Option<&str>,
        model: Option<&str>,
        requests: u64,
        raw_tokens: Option<u64>,
        compressed_tokens: Option<u64>,
        avg_savings_pct: Option<f64>,
        basis: &str,
        upstream_errors: u64,
        uncounted_rows: u64,
    ) -> ProxyModelStats {
        ProxyModelStats {
            provider: provider.map(str::to_string),
            model: model.map(str::to_string),
            requests,
            upstream_errors,
            raw_tokens,
            compressed_tokens,
            avg_savings_pct,
            tier_full_pct: 100.0,
            tier_degraded_pct: 0.0,
            tier_passthrough_pct: 0.0,
            basis: basis.to_string(),
            counted_rows: requests.saturating_sub(uncounted_rows),
            uncounted_rows,
        }
    }

    fn proxy_provider_row(
        provider: Option<&str>,
        requests: u64,
        raw_tokens: Option<u64>,
        compressed_tokens: Option<u64>,
        avg_savings_pct: Option<f64>,
        basis: &str,
        upstream_errors: u64,
    ) -> ProxyProviderStats {
        ProxyProviderStats {
            provider: provider.map(str::to_string),
            requests,
            upstream_errors,
            raw_tokens,
            compressed_tokens,
            avg_savings_pct,
            tier_full_pct: 100.0,
            tier_degraded_pct: 0.0,
            tier_passthrough_pct: 0.0,
            basis: basis.to_string(),
            counted_rows: requests,
            uncounted_rows: 0,
        }
    }

    /// AC9 (POSITIVE) — proxy section absent from JSON when no proxy rows exist.
    ///
    /// The `"proxy"` key must be JSON null when query_by_model returns empty —
    /// a non-null proxy key with no data would mislead consumers.
    #[test]
    fn test_run_json_proxy_null_when_no_proxy_rows() {
        let store = MockStore::empty(); // by_model is empty by default
        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        // The "proxy" key must be present and null.
        assert!(
            parsed.get("proxy").is_some(),
            "AC9: JSON output must contain the 'proxy' key"
        );
        assert!(
            parsed["proxy"].is_null(),
            "AC9: 'proxy' key must be null when no proxy rows exist; got: {:?}",
            parsed["proxy"]
        );
    }

    /// AC10 (POSITIVE) — proxy section present and non-null when proxy rows exist.
    ///
    /// A MockStore with by_model populated must yield a non-null "proxy" key
    /// with "by_model" and "by_provider" sub-arrays.
    #[test]
    fn test_run_json_proxy_present_when_proxy_rows_exist() {
        let mut store = MockStore::empty();
        store.by_model = vec![proxy_model_row(
            Some("anthropic"),
            Some("claude-3-5-sonnet-20241022"),
            10,
            Some(5_000),
            Some(500),
            Some(90.0),
            "approximation",
            0,
            0,
        )];
        store.by_provider = vec![proxy_provider_row(
            Some("anthropic"),
            10,
            Some(5_000),
            Some(500),
            Some(90.0),
            "approximation",
            0,
        )];

        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");

        assert!(
            !parsed["proxy"].is_null(),
            "AC10: 'proxy' key must be non-null when by_model is non-empty"
        );
        let proxy = &parsed["proxy"];
        assert!(
            proxy["by_model"].is_array(),
            "AC10: proxy.by_model must be a JSON array"
        );
        assert_eq!(
            proxy["by_model"].as_array().unwrap().len(),
            1,
            "AC10: proxy.by_model must have 1 row"
        );
        assert!(
            proxy["by_provider"].is_array(),
            "AC10: proxy.by_provider must be present"
        );
    }

    /// AC11 — mixed-basis provider: combined token figure is null in JSON.
    ///
    /// AD-AN-9: when a provider's models span different counting bases,
    /// the provider-level token sum is meaningless and must be null (not 0).
    #[test]
    fn test_run_json_proxy_mixed_basis_tokens_are_null() {
        let mut store = MockStore::empty();
        // Two model rows for "openai" with different bases.
        store.by_model = vec![
            proxy_model_row(
                Some("openai"),
                Some("gpt-4"),
                5,
                Some(2_000),
                Some(400),
                Some(80.0),
                "exact",
                0,
                0,
            ),
            proxy_model_row(
                Some("openai"),
                Some("gpt-4o"),
                5,
                Some(1_000),
                Some(200),
                Some(80.0),
                "exact",
                0,
                0,
            ),
        ];
        // Provider row with mixed basis (tokens null).
        store.by_provider = vec![ProxyProviderStats {
            provider: Some("openai".to_string()),
            requests: 10,
            upstream_errors: 0,
            raw_tokens: None,   // mixed-basis → null
            compressed_tokens: None,
            avg_savings_pct: Some(80.0),
            tier_full_pct: 100.0,
            tier_degraded_pct: 0.0,
            tier_passthrough_pct: 0.0,
            basis: "mixed".to_string(),
            counted_rows: 10,
            uncounted_rows: 0,
        }];

        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        let provider_row = &parsed["proxy"]["by_provider"][0];

        assert_eq!(
            provider_row["basis"].as_str().unwrap(),
            "mixed",
            "AC11: mixed-basis provider must have basis='mixed'"
        );
        assert!(
            provider_row["raw_tokens"].is_null(),
            "AC11: mixed-basis provider must have null raw_tokens in JSON; \
             got: {:?}",
            provider_row["raw_tokens"]
        );
        assert!(
            provider_row["compressed_tokens"].is_null(),
            "AC11: mixed-basis provider must have null compressed_tokens in JSON"
        );
    }

    /// AC12 — basis disclosure: counted_rows + uncounted_rows are both present in JSON.
    ///
    /// Uncounted rows (NULL token pairs) must be disclosed alongside the basis
    /// label so consumers can assess confidence in the token aggregate.
    #[test]
    fn test_run_json_proxy_basis_and_uncounted_rows_disclosed() {
        let mut store = MockStore::empty();
        store.by_model = vec![proxy_model_row(
            Some("anthropic"),
            Some("claude-3-5-sonnet-20241022"),
            10,
            None,   // all NULL token pairs → 0 counted
            None,
            None,
            "approximation",
            0,
            10,     // all 10 are uncounted
        )];
        store.by_provider = vec![proxy_provider_row(
            Some("anthropic"),
            10,
            None,
            None,
            None,
            "approximation",
            0,
        )];

        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        let model_row = &parsed["proxy"]["by_model"][0];

        assert_eq!(
            model_row["counted_rows"].as_u64().unwrap(),
            0,
            "AC12: counted_rows must be 0 when all tokens are NULL"
        );
        assert_eq!(
            model_row["uncounted_rows"].as_u64().unwrap(),
            10,
            "AC12: uncounted_rows must be 10"
        );
        assert_eq!(
            model_row["basis"].as_str().unwrap(),
            "approximation",
            "AC12: basis must be disclosed per model row"
        );
    }

    /// AC13 — JSON determinism: identical MockStore data produces the same output
    /// on two calls.
    ///
    /// AD-AN-9: ordering is NULL-last by SQL ORDER BY — identical row sets must
    /// produce byte-identical JSON.
    #[test]
    fn test_run_json_proxy_deterministic() {
        let mut store = MockStore::empty();
        store.by_model = vec![
            proxy_model_row(
                Some("anthropic"),
                Some("claude-3-5-sonnet-20241022"),
                5,
                Some(1_000),
                Some(200),
                Some(80.0),
                "approximation",
                0,
                0,
            ),
            proxy_model_row(
                Some("openai"),
                Some("gpt-4o"),
                3,
                Some(600),
                Some(120),
                Some(80.0),
                "exact",
                0,
                0,
            ),
        ];
        store.by_provider = vec![
            proxy_provider_row(Some("anthropic"), 5, Some(1_000), Some(200), Some(80.0), "approximation", 0),
            proxy_provider_row(Some("openai"), 3, Some(600), Some(120), Some(80.0), "exact", 0),
        ];

        let out1 = capture(|w| run_json(w, &store, None, None));
        let out2 = capture(|w| run_json(w, &store, None, None));
        assert_eq!(
            out1, out2,
            "AC13: identical MockStore state must produce byte-identical JSON"
        );
    }

    /// AC25 — upstream errors count: surfaced separately in JSON (not conflated
    /// with success-scope request counts).
    ///
    /// AD-PXY-25: a nonzero upstream_errors must appear in proxy.upstream_errors;
    /// the by_model rows' own upstream_errors field is an error count too, but
    /// the top-level proxy.upstream_errors is the sum across all models (total
    /// for the time window).
    #[test]
    fn test_run_json_proxy_upstream_errors_surfaced() {
        let mut store = MockStore::empty();
        store.by_model = vec![proxy_model_row(
            Some("anthropic"),
            Some("claude-3-5-sonnet-20241022"),
            2,
            Some(1_000),
            Some(200),
            Some(80.0),
            "approximation",
            3,  // 3 upstream errors for this model
            0,
        )];
        store.by_provider = vec![proxy_provider_row(
            Some("anthropic"),
            2,
            Some(1_000),
            Some(200),
            Some(80.0),
            "approximation",
            3,
        )];
        store.upstream_errors = 3;

        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        assert_eq!(
            parsed["proxy"]["upstream_errors"].as_u64().unwrap(),
            3,
            "AC25: upstream_errors must be 3 in proxy section"
        );
    }

    /// AC10 — dashboard render-when-present: proxy sections appear only when
    /// by_model is non-empty.
    #[test]
    fn test_dashboard_proxy_sections_absent_when_no_proxy_rows() {
        let store = MockStore::with_data(); // no proxy rows
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            !output.contains("Proxy — By Provider"),
            "AC10: 'Proxy — By Provider' section must not appear without proxy rows"
        );
        assert!(
            !output.contains("Proxy — By Model"),
            "AC10: 'Proxy — By Model' section must not appear without proxy rows"
        );
    }

    /// AC10 — dashboard render-when-present: proxy sections appear when by_model
    /// is non-empty.
    #[test]
    fn test_dashboard_proxy_sections_present_when_proxy_rows_exist() {
        let mut store = MockStore::with_data();
        store.by_model = vec![proxy_model_row(
            Some("anthropic"),
            Some("claude-3-5-sonnet-20241022"),
            10,
            Some(5_000),
            Some(500),
            Some(90.0),
            "approximation",
            0,
            0,
        )];
        store.by_provider = vec![proxy_provider_row(
            Some("anthropic"),
            10,
            Some(5_000),
            Some(500),
            Some(90.0),
            "approximation",
            0,
        )];
        // with_data() has non-zero invocations so dashboard proceeds past the empty-check.
        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            output.contains("Proxy"),
            "AC10: dashboard must show 'Proxy' sections when proxy rows exist"
        );
        assert!(
            output.contains("anthropic"),
            "AC10: dashboard must show provider name in proxy section"
        );
    }

    /// AC25 — dashboard notices section: upstream errors shown separately.
    #[test]
    fn test_dashboard_proxy_notices_shows_upstream_errors() {
        let mut store = MockStore::with_data();
        store.by_model = vec![proxy_model_row(
            Some("anthropic"),
            Some("claude-3-5-sonnet-20241022"),
            5,
            Some(2_000),
            Some(400),
            Some(80.0),
            "approximation",
            7,
            0,
        )];
        store.by_provider = vec![proxy_provider_row(
            Some("anthropic"),
            5,
            Some(2_000),
            Some(400),
            Some(80.0),
            "approximation",
            7,
        )];
        store.upstream_errors = 7;

        let output = capture(|w| run_dashboard(w, &store, None, false, None, None));
        assert!(
            output.contains("Upstream errors"),
            "AC25: dashboard must show 'Upstream errors' notice when count > 0"
        );
        assert!(
            output.contains("excluded from savings aggregates"),
            "AC25: upstream-errors notice must clarify exclusion from savings"
        );
    }
}
