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
    AnalyticsDb, AnalyticsStore, OriginalCommandStats, PricingModel, SessionStats,
};
use crate::cmd::session::types::parse_duration_ago;
use crate::tokens;

// ============================================================================
// Public entry point
// ============================================================================

/// Run the `skim stats` subcommand.
#[allow(clippy::disallowed_methods)] // Analytics stats display; locked handle for atomic multi-line dashboard output
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

    // PF-037, second instance and it inverts the usual shape: the DEGENERATE-CASE
    // guard exists and is correct on the text path — `render_delivered` returns
    // early on `rows == 0` because "rendering it as 0 would be a claim" — and was
    // simply absent here. `DeliveredSavings` derives a plain `Serialize` with no
    // `skip_serializing_if`, so `--json` published `{"rows": 0, "tokens": 0,
    // "first_day": null}`: the exact claim the human-readable surface refuses,
    // made to the consumer that parses rather than reads. Measured live on the
    // author's database (zero rows with `notice_tokens IS NOT NULL`), so this was
    // shipping today, not latent.
    //
    // The unmeasured case is made UNREPRESENTABLE by omitting the key, which is
    // the only encoding a consumer cannot mistake for a measured zero. The guard
    // lives here rather than as a `skip_serializing_if` on the struct because
    // PF-037's lesson is that the contract is per-RENDERER: a whole-object guard
    // is a property of what THIS surface will assert, not of the type. (A
    // per-FIELD absence is a different question and does belong on the struct —
    // `DeliveredSavings::reset_at` carries one.)
    let delivered_json = if summary.delivered.rows > 0 {
        Some(serde_json::to_value(&summary.delivered)?)
    } else {
        None
    };

    // PF-037 once more, in the direction the guard above creates. Absence is
    // the right encoding for "not yet measured" — and it makes a RESET series
    // byte-identical to a never-measured one, collapsing two states into one
    // exactly as `{"rows": 0}` did. The reset is the thing that explains why
    // `delivered` is missing, so it has to survive `delivered` being missing.
    //
    // It is published as its OWN key rather than as a stub `delivered` object,
    // because any object carrying `rows`/`tokens` publishes a measured zero —
    // the claim the guard above exists to refuse. The two are mutually
    // exclusive by construction and a consumer reads exactly one of them: with
    // rows, the mark rides inside `delivered.reset_at` via serde on the struct;
    // without rows, it is this key. Neither state emits both, so the mark never
    // appears at two nesting levels.
    let delivered_reset_json = match (summary.delivered.rows, summary.delivered.reset_at) {
        (0, Some(at)) => Some(at),
        _ => None,
    };

    // `delivered` nests INSIDE `summary`, beside `tokens_lost` and
    // `avg_savings_pct_compressed`. The three are one disclosure story, and
    // reading them from two nesting levels is a cost paid by every consumer
    // forever. Moving it is free exactly now, because the series has zero
    // measured rows anywhere, so nothing can yet depend on the old placement.
    let mut summary_json = serde_json::json!({
        "invocations": summary.invocations,
        "raw_tokens": summary.raw_tokens,
        "compressed_tokens": summary.compressed_tokens,
        "tokens_saved": summary.tokens_saved,
        "tokens_lost": summary.tokens_lost,
        "avg_savings_pct": summary.avg_savings_pct,
        "avg_savings_pct_compressed": summary.avg_savings_pct_compressed,
        "compressed_invocations": summary.compressed_invocations,
        "expansion_invocations": summary.expansion_invocations,
        "weighted_savings_pct": weighted_pct,
    });
    // Total by construction: the literal above is an object, and the `Some` arm
    // is the `rows > 0` case. Neither pattern can fail, and neither can panic.
    if let (serde_json::Value::Object(map), Some(value)) = (&mut summary_json, delivered_json) {
        map.insert("delivered".to_string(), value);
    }
    // Named for the `analytics_meta` key it comes from, so a reader chasing the
    // value has the row to look at.
    if let (serde_json::Value::Object(map), Some(at)) = (&mut summary_json, delivered_reset_json) {
        map.insert("delivered_series_reset_at".to_string(), at.into());
    }

    let root = serde_json::json!({
        // Three series, kept apart on purpose — see `AnalyticsSummary`.
        // `tokens_saved` and `avg_savings_pct` keep their exact prior meaning so
        // existing consumers are not silently re-based; `tokens_lost` covers the
        // same full history because it needs only raw/compressed; `delivered`
        // ships its own window because it cannot cover rows recorded before
        // disclosure measurement began — and is ABSENT, not zero, until one is.
        "summary": summary_json,
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
    // Printed directly beneath `Tokens saved`, and only when there is something
    // to disclose. This is the quantity the per-row `ELSE 0` clamp drops; the
    // clamp is kept (it is the definition the whole retained series was built
    // on) but it no longer gets to be silent. On the author's 90-day corpus
    // this line reads 11,983,387 against an 80,238,562 headline — the headline
    // is 17.6% above the true net.
    if summary.tokens_lost > 0 {
        // Widened to i64 and left UNCLAMPED. Computed in u64 with
        // `saturating_sub`, an expansion-dominated `--since` window renders as
        // `net 0` — which reintroduces, one line below, the exact clamp the
        // `Tokens lost:` line two lines above was added to disclose. The reader
        // would be told the headline hides expansion and then handed a net that
        // hides it again, on the same screen. The sibling
        // `DeliveredSavings::tokens` is `i64` and unclamped for this reason:
        // the sign is the finding.
        //
        // `unwrap_or(i64::MAX)` is unreachable on real data (both operands are
        // SUMs over i64 columns, already floored at 0 by `query_summary`) and is
        // the saturating, non-panicking form. Both operands land in
        // `[0, i64::MAX]`, so the subtraction itself cannot overflow.
        let net = i64::try_from(summary.tokens_saved).unwrap_or(i64::MAX)
            - i64::try_from(summary.tokens_lost).unwrap_or(i64::MAX);
        writeln!(
            w,
            "  Tokens lost:  {}  (expansion the line above drops; net {})",
            tokens::format_number(summary.tokens_lost as usize),
            signed_tokens(net),
        )?;
    }
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
    // The bar above is token-weighted (PF-036). The per-invocation mean is a
    // different statistic, and the all-rows form of it is diluted by every
    // invocation where skim served exactly what it was given: a no-op
    // contributes a 0% sample to an average about compression. Both means are
    // shown with the row counts they were taken over, so neither can be quoted
    // without its population.
    //
    // The narrow population is "rows that COMPRESSED", not "rows that changed".
    // The latter was false: `savings_pct` is floored at zero at WRITE time, so
    // every EXPANDING row entered a mean labelled "over the rows that changed"
    // as a 0% SAVING rather than as the loss it was. Measured on the author's
    // corpus (69,258 rows, 2026-09-25): that mean printed 35.96% over 25,682
    // changed rows, of which 7,037 were expansions contributing a floored 0%
    // each; over the 18,645 rows that actually compressed it reads 49.54%. The
    // excluded population is now COUNTED on the next line rather than folded in
    // as zeros — the same disclosure the `tokens_lost` line makes in token
    // space, one statistic later.
    if summary.invocations > 0 && summary.compressed_invocations < summary.invocations {
        writeln!(w)?;
        writeln!(
            w,
            "  Per-invocation mean: {:.1}% over all {} \u{2014} {:.1}% over the {} that compressed",
            summary.avg_savings_pct,
            tokens::format_number(summary.invocations as usize),
            summary.avg_savings_pct_compressed,
            tokens::format_number(summary.compressed_invocations as usize),
        )?;
        // Only when there is a population to name. A mean that excludes rows
        // silently is the defect one line above; a mean that excludes rows and
        // says how many is a statistic.
        if summary.expansion_invocations > 0 {
            writeln!(
                w,
                "    excludes {} that expanded; savings_pct is floored at 0 on write, \
                 so they cannot enter a mean about saving",
                tokens::format_number(summary.expansion_invocations as usize),
            )?;
        }
    }
    render_delivered(w, &summary.delivered)?;
    writeln!(w)?;
    Ok(())
}

/// The delivered series — value and window, never one without the other.
///
/// Printing an unwindowed figure next to `Tokens saved` would invite exactly
/// the comparison it cannot support: `tokens_saved` spans the full 90-day
/// retention, this spans only rows that carry a disclosure measurement
/// (`notice_tokens IS NOT NULL`), which begins where that measurement began.
/// The window is not a footnote, it is what makes the number readable at all,
/// so it shares the line.
///
/// The qualifier is stated as the measurement condition rather than as a schema
/// version, because this build stamps no `user_version` for those columns — see
/// the delivered-cost block in `analytics::schema`. The boundary is row-level
/// NULL-ness, and that is what the line now says.
///
/// # Zero rows is not a single state
///
/// It used to mean exactly one thing — "not yet measured" — and silence was the
/// whole contract, because rendering it as `0` would be a claim. It means one
/// of THREE things, and all three are now distinguishable:
///
/// 1. NEVER MEASURED. The normal state of a fresh database, of an upgraded one
///    before its first measured invocation, of `stats --clear`, and of a 90-day
///    prune. Stays silent, for the original reason.
/// 2. RESET by a foreign rebuild that dropped the delivered-cost columns and
///    had them re-added empty. `analytics::schema` marks this in
///    `analytics_meta` under `delivered_series_reset_at` precisely so the loss
///    stays recoverable, and [`crate::analytics::DeliveredSavings::reset_at`]
///    now carries that mark here. Printed — as a cause, with no number
///    attached, because the measurements it explains are gone.
/// 3. EVERY MEASUREMENT UNTOKENISABLE — [`crate::analytics::DeliveredSavings`]'s
///    `unmeasured_notice_rows` is non-zero while `rows` is zero. Disclosures
///    were emitted and their cost could not be counted, so the series is EMPTY
///    rather than UNOPENED. That is a different claim about a different cause,
///    and it is printed.
///
/// (2) and (3) are independent and can hold together — a reset series can then
/// accumulate only untokenisable disclosures — so both are printed rather than
/// chained on an `else`.
///
/// Per PF-037 this contract is per-RENDERER: `run_json` carries the same
/// disclosure, because a guard that lives on one surface is a guard the other
/// surface does not have.
fn render_delivered(
    w: &mut dyn Write,
    delivered: &crate::analytics::DeliveredSavings,
) -> anyhow::Result<()> {
    if delivered.rows == 0 {
        // Case (2). Named as a CAUSE and nothing more: no figure is printed,
        // because the reset is exactly the event that means there is none. The
        // alternative — staying silent — reports it as case (1), "nothing
        // measured yet", which is the one reading that is wrong here.
        if let Some(reset_at) = delivered.reset_at {
            writeln!(w)?;
            writeln!(
                w,
                "  Delivered series: RESET{} — a `token_savings` rebuild dropped the \
                 delivered-cost columns and every measurement in them",
                age_suffix(reset_at),
            )?;
            writeln!(
                w,
                "    the figure resumes from the next measured invocation; nothing before \
                 the reset is recoverable"
            )?;
        }
        // Case (3). Silence here would report it as case (1), "nothing measured
        // yet", which names a different cause. Deliberately avoids the
        // `Delivered saved:` headline — no total is being asserted, and the
        // headline is what a reader scans for one.
        if delivered.unmeasured_notice_rows > 0 {
            writeln!(w)?;
            writeln!(
                w,
                "  Delivered series: empty, not unopened — {} run(s) emitted a disclosure \
                 whose token cost could not be measured",
                tokens::format_number(delivered.unmeasured_notice_rows as usize),
            )?;
        }
        return Ok(());
    }
    let window = match (&delivered.first_day, &delivered.last_day) {
        (Some(first), Some(last)) if first == last => first.clone(),
        (Some(first), Some(last)) => format!("{first}..{last}"),
        // rows > 0 with no timestamps is not reachable through query_summary;
        // say so rather than printing a bare number with no window.
        _ => "window unknown".to_string(),
    };
    writeln!(w)?;
    // Rendered signed via `signed_tokens`. Clamping a negative total to 0 here
    // would be the same dishonesty the `tokens_lost` line exists to undo, one
    // series later.
    //
    // The qualifier names the COHORT as well as the window. Disclosing only the
    // window leaves the one reading of this number that is wrong: as a
    // whole-corpus delivered total. `notice_tokens IS NOT NULL` is reachable
    // only through `analytics::record_file_ops`, the single recording path that
    // builds a non-default `Delivery`, and it hard-codes `CommandType::File`;
    // `record_fire_and_forget` and `try_record_command_with_counts` pass
    // `Delivery::default()` and write NULL. So the series structurally excludes
    // git, build, test, log, db, infra, pkg and heatmap — roughly 90% of recent
    // invocations, and the highest-savings cohorts among them (PF-036
    // Resolution (C): price a cost against the population that can carry it,
    // never against a denominator containing rows the effect cannot reach).
    writeln!(
        w,
        "  Delivered saved: {} over {} disclosure-measured file reads, {} (not comparable above)",
        signed_tokens(delivered.tokens),
        tokens::format_number(delivered.rows as usize),
        window,
    )?;
    writeln!(
        w,
        "    file cohort only — subcommand output (git/build/test/log/db/infra/pkg/heatmap) \
         is not disclosure-measured"
    )?;
    writeln!(
        w,
        "    after charging {} tokens of stderr disclosure the same runs emitted",
        tokens::format_number(delivered.notice_tokens as usize),
    )?;
    // The total above blends two populations with opposite cost profiles: a
    // raw-served run saved nothing and still paid for the disclosure saying so,
    // and can only push the figure DOWN; a transformed run is the one the
    // headline is about. Printing the split is the first time `served` is read
    // by anything at all (PF-036's amendment: recording a column is not
    // measuring it).
    //
    // All three counts are printed, always, including zeros — they PARTITION
    // the rows above, so they must be seen to add up. `served_other` is not a
    // fault state: a cache hit and `Mode::Full` both record no serving decision
    // inside a perfectly measured row.
    writeln!(
        w,
        "    served {} raw, {} transformed, {} no decision recorded (of {})",
        tokens::format_number(delivered.served_raw as usize),
        tokens::format_number(delivered.served_transformed as usize),
        tokens::format_number(delivered.served_other as usize),
        tokens::format_number(delivered.rows as usize),
    )?;
    // The drop has a sign, and it is the favourable one: every such row was
    // going to contribute a COST, so excluding it moves the total UP. Counted
    // rather than silently absorbed, per the field's own contract.
    if delivered.unmeasured_notice_rows > 0 {
        writeln!(
            w,
            "    {} row(s) emitted a disclosure that could not be tokenised and are \
             excluded; the total above is biased upward",
            tokens::format_number(delivered.unmeasured_notice_rows as usize),
        )?;
    }
    if delivered.tokens < 0 {
        writeln!(
            w,
            "    net NEGATIVE: the disclosures cost more than the transforms saved"
        )?;
    }
    // A reset is not only a zero-rows condition. Once measurement resumes the
    // window above opens at the first NEW row, which looks exactly like a
    // young series — so without this line the window silently understates what
    // the database once held. Said here because `reset_at` reaches `--json`
    // through serde on this same struct, and a disclosure one surface carries
    // and the other drops is PF-037 in the other direction.
    if let Some(reset_at) = delivered.reset_at {
        writeln!(
            w,
            "    the window opens at a RESET{}, not at the start of recording — \
             a `token_savings` rebuild dropped everything before it",
            age_suffix(reset_at),
        )?;
    }
    Ok(())
}

/// Format a signed token count as its magnitude with an explicit leading `-`.
///
/// Shared by the `Tokens lost:` net and the delivered total so the two cannot
/// drift. Both are quantities whose SIGN is the finding, and both are one
/// careless `as usize` — or one `u64::saturating_sub` — away from rendering a
/// loss as a saving. `unsigned_abs` is total, including at `i64::MIN`.
fn signed_tokens(value: i64) -> String {
    let magnitude = tokens::format_number(value.unsigned_abs() as usize);
    if value < 0 {
        format!("-{magnitude}")
    } else {
        magnitude
    }
}

/// How long ago `at` (Unix seconds) was, as a suffix to splice into a sentence,
/// or an empty string when the question has no honest answer.
///
/// # Why an age and not a date
///
/// A calendar date here would mean carrying a `civil_from_days` implementation
/// for one line of output, and the sibling window (`first_day`/`last_day`) gets
/// its day strings from SQLite's `date(…,'unixepoch')` rather than from any
/// Rust date code — so a hand-rolled formatter would also be a SECOND way this
/// file renders time. An age answers the only question the reset line raises
/// ("how much history did I lose?") with integer division and no calendar.
///
/// Returns `""` rather than guessing when the mark is in the FUTURE. That is
/// reachable without anything being wrong with skim — `unix_now` in
/// `analytics::schema` reads the system clock, so a machine whose clock was
/// corrected backwards after a reset has one — and "in -3 days" is worse than
/// a line that simply does not date itself. The reset itself is still reported;
/// only its age is withheld.
fn age_suffix(at: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let Some(elapsed) = now.checked_sub(at).filter(|e| *e >= 0) else {
        return String::new();
    };
    match elapsed / crate::analytics::SECONDS_PER_DAY as i64 {
        0 => " today".to_string(),
        1 => " 1 day ago".to_string(),
        days => format!(" {days} days ago"),
    }
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
    }

    impl MockStore {
        fn empty() -> Self {
            Self {
                summary: AnalyticsSummary::default(),
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
                    // 5,000 tokens of expansion sit under the `tokens_saved`
                    // clamp, spread over 3 rows; 30 of the 42 rows actually
                    // compressed and paid; the remaining 9 were no-ops.
                    //
                    // `expansion_invocations` is NOT 0 here. `tokens_lost` is the
                    // SUM and `expansion_invocations` the COUNT of the same
                    // predicate (`compressed_tokens > raw_tokens`), so a positive
                    // `tokens_lost` over zero rows is an unreachable state, and a
                    // fixture asserting one would leave the exclusion disclosure
                    // this branch adds with no coverage at all.
                    tokens_lost: 5_000,
                    expansion_invocations: 3,
                    compressed_invocations: 30,
                    avg_savings_pct_compressed: 98.0,
                    delivered: crate::analytics::DeliveredSavings {
                        rows: 12,
                        tokens: 64_500,
                        notice_tokens: 500,
                        // Whole series: every measurement was tokenisable.
                        unmeasured_notice_rows: 0,
                        first_day: Some("2026-03-24".to_string()),
                        last_day: Some("2026-03-25".to_string()),
                        // Partitions `rows`: 7 + 4 + 1 = 12. A fixture whose
                        // three buckets did not add up would let a renderer
                        // that drops one of them still pass.
                        served_raw: 7,
                        served_transformed: 4,
                        served_other: 1,
                        // Never reset: the window opens where recording began,
                        // so the general-purpose fixture asserts no reset line
                        // and every test built on it stays about the series
                        // rather than about its history.
                        reset_at: None,
                    },
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
    // Three series — comparability
    // ========================================================================

    fn render_one(summary: &crate::analytics::AnalyticsSummary) -> String {
        let sessions = SessionStats {
            distinct_sessions: 0,
            total_tokens_saved: 0,
            avg_tokens_per_session: 0.0,
            untagged_invocations: 0,
        };
        let mut buf = Vec::new();
        render_summary(&mut buf, summary, &sessions).expect("render should not fail");
        String::from_utf8(buf).unwrap()
    }

    /// The delivered figure never appears without the window it covers.
    ///
    /// DISCRIMINATING: drop the window from `render_delivered` and this fails —
    /// which is the point, because an unwindowed delivered total sitting under
    /// a 90-day `Tokens saved` invites exactly the comparison it cannot support.
    #[test]
    fn delivered_total_is_never_printed_without_its_window() {
        let out = render_one(&MockStore::with_data().summary);
        assert!(out.contains("Delivered saved"), "delivered series missing");
        assert!(
            out.contains("2026-03-24..2026-03-25"),
            "delivered total must carry its window; got:\n{out}"
        );
        assert!(
            out.contains("disclosure-measured file reads"),
            "and must qualify which rows it covers — the series spans only FILE \
             rows carrying a disclosure measurement, not a schema version and not \
             the whole corpus; got:\n{out}"
        );
        assert!(
            out.contains("not comparable above"),
            "and must say it is not comparable with the total above; got:\n{out}"
        );
    }

    /// Nothing measured yet is not the same as zero delivered savings.
    #[test]
    fn delivered_series_is_silent_before_any_measured_row() {
        let summary = crate::analytics::AnalyticsSummary {
            invocations: 10,
            raw_tokens: 1000,
            tokens_saved: 500,
            ..Default::default()
        };
        let out = render_one(&summary);
        assert!(
            !out.contains("Delivered saved"),
            "an unmeasured series must not be rendered as a measured zero; got:\n{out}"
        );
    }

    /// PF-037: the guard above is per-RENDERER, so the JSON path needs its own.
    ///
    /// The mirror of `delivered_series_is_silent_before_any_measured_row`, and
    /// the one that was missing: the text path has refused to print an
    /// unmeasured series since the series was added, while `--json` published
    /// `{"rows": 0, "tokens": 0, "first_day": null}` — the same claim, to the
    /// consumer that parses rather than reads. Measured live on the author's
    /// database (zero rows with `notice_tokens IS NOT NULL`), so this was the
    /// shipping behaviour, not a latent one.
    ///
    /// DISCRIMINATING: serialise `delivered` unconditionally and this fails.
    #[test]
    fn delivered_key_is_absent_from_json_before_any_measured_row() {
        let store = MockStore::empty();
        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        assert!(
            parsed["summary"].get("delivered").is_none(),
            "an unmeasured series must be ABSENT, not a measured zero — a \
             `\"delivered\": {{\"rows\": 0, \"tokens\": 0}}` is indistinguishable \
             from a real zero to every consumer; got:\n{output}"
        );
        assert!(
            parsed.get("delivered").is_none(),
            "and must not reappear at the top level either; got:\n{output}"
        );
    }

    /// Measured, the series IS published — nested with its siblings.
    ///
    /// The guard is a degenerate-case guard, not a suppression: the failure
    /// mode of over-correcting is a series that never publishes at all.
    /// Placement is asserted too, because `tokens_lost`,
    /// `avg_savings_pct_compressed` and `delivered` are one disclosure story
    /// and a consumer should not read them from two nesting levels.
    #[test]
    fn delivered_is_published_inside_summary_once_measured() {
        let store = MockStore::with_data();
        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        let delivered = &parsed["summary"]["delivered"];
        assert!(
            delivered.is_object(),
            "a measured series must be published; got:\n{output}"
        );
        assert_eq!(delivered["rows"], 12);
        assert_eq!(delivered["tokens"], 64_500);
        assert_eq!(delivered["notice_tokens"], 500);
        assert_eq!(delivered["unmeasured_notice_rows"], 0);
        assert_eq!(delivered["first_day"], "2026-03-24");
        assert!(
            parsed.get("delivered").is_none(),
            "and lives ONLY under `summary`, not at both levels; got:\n{output}"
        );
    }

    /// Zero rows AFTER A RESET is a different state from zero rows before any
    /// measurement, and the dashboard names the cause rather than the number.
    ///
    /// `reset_at` is written to `analytics_meta` only when all three
    /// delivered-cost columns REAPPEAR on an already-reconciled database — i.e.
    /// only when something dropped them, taking every measurement with them.
    ///
    /// DISCRIMINATING, twice: restore the bare `if rows == 0 { return }` and a
    /// destroyed series reports as a never-opened one; and print a figure here
    /// and the renderer asserts a total over rows that no longer exist.
    #[test]
    fn a_reset_series_names_the_reset_and_still_asserts_no_total() {
        let summary = crate::analytics::AnalyticsSummary {
            invocations: 10,
            raw_tokens: 1000,
            tokens_saved: 500,
            delivered: crate::analytics::DeliveredSavings {
                rows: 0,
                reset_at: Some(1_790_000_000),
                ..Default::default()
            },
            ..Default::default()
        };
        let out = render_one(&summary);
        assert!(
            out.contains("RESET") && out.contains("resumes from the next measured invocation"),
            "a reset series must name its cause and say where the figure picks \
             up; got:\n{out}"
        );
        assert!(
            !out.contains("Delivered saved"),
            "but must assert no total — every row it would have covered is \
             gone; got:\n{out}"
        );
    }

    /// The two zero-row causes are independent and both are printed.
    ///
    /// A reset series can then accumulate only untokenisable disclosures.
    /// DISCRIMINATING: chain the two branches on an `else` and the reset hides
    /// the emptiness, or the emptiness hides the reset, depending which way the
    /// chain falls.
    #[test]
    fn a_reset_and_an_untokenisable_only_series_are_both_reported() {
        let summary = crate::analytics::AnalyticsSummary {
            delivered: crate::analytics::DeliveredSavings {
                rows: 0,
                unmeasured_notice_rows: 4,
                reset_at: Some(1_790_000_000),
                ..Default::default()
            },
            ..Default::default()
        };
        let out = render_one(&summary);
        assert!(out.contains("RESET"), "the reset is a cause; got:\n{out}");
        assert!(
            out.contains("empty, not unopened") && out.contains("4 run(s)"),
            "and so is the untokenisable population — neither excuses dropping \
             the other; got:\n{out}"
        );
    }

    /// PF-037: the reset disclosure is per-RENDERER, so `--json` carries it too.
    ///
    /// The `delivered` guard makes a zero-row series ABSENT, which is right and
    /// which is also what makes a RESET indistinguishable from "never
    /// measured" to a parsing consumer — the same two-states-into-one collapse
    /// the guard exists to prevent, one level out.
    ///
    /// DISCRIMINATING: drop the standalone key and the JSON surface for a
    /// destroyed series is byte-identical to the JSON for a fresh install.
    #[test]
    fn json_publishes_a_reset_even_though_the_series_itself_is_absent() {
        let mut store = MockStore::empty();
        store.summary.delivered.reset_at = Some(1_790_000_000);
        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        assert!(
            parsed["summary"].get("delivered").is_none(),
            "an unmeasured series stays absent — the reset does not license \
             publishing a measured zero; got:\n{output}"
        );
        assert_eq!(
            parsed["summary"]["delivered_series_reset_at"], 1_790_000_000_i64,
            "but the cause of the absence must be published; got:\n{output}"
        );
    }

    /// A measured series carries its reset INSIDE `delivered`, and never at two
    /// levels at once.
    ///
    /// The struct field and the standalone key are mutually exclusive by
    /// construction; a consumer reads exactly one of them.
    ///
    /// DISCRIMINATING: publish the standalone key unconditionally and the mark
    /// appears twice, which is the nesting-level cost
    /// `delivered_is_published_inside_summary_once_measured` already refuses
    /// for the series itself.
    #[test]
    fn a_measured_reset_series_carries_its_mark_in_exactly_one_place() {
        let mut store = MockStore::with_data();
        store.summary.delivered.reset_at = Some(1_790_000_000);
        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        assert_eq!(
            parsed["summary"]["delivered"]["reset_at"],
            1_790_000_000_i64
        );
        assert!(
            parsed["summary"].get("delivered_series_reset_at").is_none(),
            "one mark, one place; got:\n{output}"
        );
    }

    /// An unreset series publishes no `reset_at` key at all.
    ///
    /// DISCRIMINATING: drop `skip_serializing_if` on the field and every
    /// measured series ships `"reset_at": null` — a field to test, in the
    /// renderer whose sibling guard exists because publishing an absent state
    /// as a concrete value is what consumers misread (PF-037).
    #[test]
    fn an_unreset_series_publishes_no_reset_key() {
        let store = MockStore::with_data();
        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        assert!(
            parsed["summary"]["delivered"].get("reset_at").is_none(),
            "an unreset series must publish no key, not a null; got:\n{output}"
        );
        assert!(
            !output.contains("reset_at"),
            "and the string must not carry it anywhere either; got:\n{output}"
        );
    }

    /// The reset line dates itself when it honestly can, and does not when it
    /// cannot.
    ///
    /// A mark in the FUTURE is reachable without anything being wrong with
    /// skim: `analytics::schema::unix_now` reads the system clock, so a machine
    /// corrected backwards after a reset has one.
    ///
    /// DISCRIMINATING: drop the `filter(|e| *e >= 0)` and the future case
    /// renders as `-N days ago`, or — with unsigned arithmetic — as an
    /// enormous positive age. Both date the reset with a number that is not a
    /// measurement.
    #[test]
    fn age_suffix_reports_only_an_age_it_can_defend() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after the epoch")
            .as_secs() as i64;
        let day = crate::analytics::SECONDS_PER_DAY as i64;

        assert_eq!(age_suffix(now), " today");
        assert_eq!(
            age_suffix(now - day),
            " 1 day ago",
            "singular, not `1 days`"
        );
        assert_eq!(age_suffix(now - 3 * day), " 3 days ago");
        assert_eq!(
            age_suffix(now + 10 * day),
            "",
            "a mark in the future is a clock the renderer cannot vouch for, so \
             it reports the reset and withholds only the age"
        );
    }

    /// The renamed population keys reach JSON, not only the dashboard.
    ///
    /// `avg_savings_pct_changed` was a false label — floored-to-zero expansions
    /// entered it as 0% savings — so the JSON key carrying it had to be renamed
    /// with the statistic, and the excluded population published beside it.
    #[test]
    fn json_publishes_the_compressed_population_and_its_exclusions() {
        let store = MockStore::with_data();
        let output = capture(|w| run_json(w, &store, None, None));
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        let summary = &parsed["summary"];
        assert_eq!(summary["avg_savings_pct_compressed"], 98.0);
        assert_eq!(summary["compressed_invocations"], 30);
        assert_eq!(summary["expansion_invocations"], 3);
        assert_eq!(summary["tokens_lost"], 5_000);
        assert!(
            summary.get("avg_savings_pct_changed").is_none()
                && summary.get("changed_invocations").is_none(),
            "the false label must be gone, not shipped alongside its \
             replacement; got:\n{output}"
        );
    }

    /// Zero rows is three states, and the untokenisable one is not silence.
    ///
    /// DISCRIMINATING: restore the bare `if rows == 0 { return }` and this
    /// fails — an empty-because-untokenisable series would be reported as
    /// never-measured, which names a different cause.
    #[test]
    fn an_untokenisable_only_series_says_it_is_empty_not_unopened() {
        let summary = crate::analytics::AnalyticsSummary {
            invocations: 10,
            raw_tokens: 1000,
            tokens_saved: 500,
            delivered: crate::analytics::DeliveredSavings {
                rows: 0,
                unmeasured_notice_rows: 4,
                ..Default::default()
            },
            ..Default::default()
        };
        let out = render_one(&summary);
        assert!(
            out.contains("empty, not unopened") && out.contains("4 run(s)"),
            "zero rows with untokenisable measurements is a different state from \
             nothing measured, and must say so; got:\n{out}"
        );
        assert!(
            !out.contains("Delivered saved"),
            "but still asserts no total — there is none; got:\n{out}"
        );
    }

    /// A measured series discloses the rows that fell out of it.
    #[test]
    fn a_measured_series_counts_its_untokenisable_dropouts() {
        let summary = crate::analytics::AnalyticsSummary {
            invocations: 10,
            raw_tokens: 1000,
            tokens_saved: 500,
            delivered: crate::analytics::DeliveredSavings {
                rows: 12,
                tokens: 64_500,
                notice_tokens: 500,
                unmeasured_notice_rows: 7,
                first_day: Some("2026-03-24".to_string()),
                last_day: Some("2026-03-25".to_string()),
                served_raw: 7,
                served_transformed: 4,
                served_other: 1,
                reset_at: None,
            },
            ..Default::default()
        };
        let out = render_one(&summary);
        assert!(
            out.contains("could not be tokenised") && out.contains("biased upward"),
            "the drop has a favourable sign, so the total must name it rather \
             than absorb it; got:\n{out}"
        );
    }

    /// The delivered total is split by what the guard actually served, and the
    /// split is shown to add up.
    ///
    /// `served` was written on every measured row and read by no production
    /// query — PF-036's amendment ("recording a column is not measuring it").
    /// This is its first reader, and the split matters because the two
    /// populations have opposite cost profiles: a raw-served run saved nothing
    /// and still paid for the disclosure that said so.
    ///
    /// DISCRIMINATING: drop any one of the three and the printed counts no
    /// longer reconcile with the row count on the line above, which is the
    /// state `served_other` exists to make impossible.
    #[test]
    fn the_delivered_total_is_split_by_what_was_served() {
        let out = render_one(&MockStore::with_data().summary);
        assert!(
            out.contains("7 raw, 4 transformed, 1 no decision recorded (of 12)"),
            "the serving split must be printed, and must be visibly a partition \
             of the rows above; got:\n{out}"
        );
    }

    /// The delivered total names its COHORT, not only its window.
    ///
    /// DISCRIMINATING: drop the cohort line and this fails. The window alone
    /// leaves the one reading of the number that is wrong — as a whole-corpus
    /// delivered total — when `notice_tokens IS NOT NULL` is reachable only
    /// through `record_file_ops`, which hard-codes `CommandType::File`.
    #[test]
    fn delivered_total_names_the_cohort_it_covers() {
        let out = render_one(&MockStore::with_data().summary);
        assert!(
            out.contains("file cohort only"),
            "the series excludes git/build/test/log/db/infra/pkg/heatmap — \
             structurally, not incidentally — and must say so; got:\n{out}"
        );
        assert!(
            out.contains("not disclosure-measured"),
            "and must say WHY those cohorts are absent; got:\n{out}"
        );
    }

    /// The clamped headline is printed with the expansion it drops.
    #[test]
    fn expansion_is_disclosed_next_to_the_clamped_headline() {
        let out = render_one(&MockStore::with_data().summary);
        assert!(out.contains("Tokens saved"), "headline missing");
        assert!(
            out.contains("Tokens lost"),
            "the clamp must disclose what it drops; got:\n{out}"
        );
        // 70,000 clamped headline less 5,000 of hidden expansion.
        assert!(
            out.contains("65,000"),
            "the true net must be on the line; got:\n{out}"
        );
    }

    /// Neither mean can be quoted without the population it was taken over.
    #[test]
    fn both_means_are_printed_with_their_row_counts() {
        let out = render_one(&MockStore::with_data().summary);
        assert!(
            out.contains("Per-invocation mean"),
            "no-op dilution must be visible; got:\n{out}"
        );
        assert!(out.contains("70.0%") && out.contains("98.0%"), "both means");
        assert!(out.contains("42") && out.contains("30"), "both populations");
        assert!(
            out.contains("that compressed"),
            "the narrow population is the rows that COMPRESSED; \"that changed\" \
             folded floored-to-zero expansions in as 0% savings; got:\n{out}"
        );
        assert!(
            !out.contains("that changed"),
            "and the false label must be gone, not merely supplemented; got:\n{out}"
        );
    }

    /// The narrowed mean names the population it drops, not just the one it keeps.
    ///
    /// DISCRIMINATING: delete the `expansion_invocations > 0` line and this
    /// fails — which is the point, because a mean that silently excludes a
    /// quarter of its candidate rows is the defect the rename was made to fix,
    /// relocated rather than removed.
    #[test]
    fn the_narrowed_mean_counts_the_expansions_it_excludes() {
        let out = render_one(&MockStore::with_data().summary);
        assert!(
            out.contains("excludes 3 that expanded"),
            "the excluded population must be counted; got:\n{out}"
        );
        assert!(
            out.contains("floored at 0 on write"),
            "and the reason must be given — an expansion is unrecoverable from \
             savings_pct, which is why it cannot be averaged in; got:\n{out}"
        );
    }

    /// No expansions means no exclusion line — the disclosure is not boilerplate.
    #[test]
    fn a_corpus_with_no_expansions_prints_no_exclusion_line() {
        let summary = crate::analytics::AnalyticsSummary {
            invocations: 10,
            raw_tokens: 1000,
            tokens_saved: 500,
            compressed_invocations: 4,
            avg_savings_pct_compressed: 50.0,
            ..Default::default()
        };
        let out = render_one(&summary);
        assert!(
            out.contains("Per-invocation mean"),
            "the mean itself is still printed; got:\n{out}"
        );
        assert!(
            !out.contains("that expanded"),
            "nothing was excluded, so nothing may be claimed excluded; got:\n{out}"
        );
    }

    /// An expansion-dominated window renders a NEGATIVE net, never `0`.
    ///
    /// DISCRIMINATING: restore `tokens_saved.saturating_sub(tokens_lost)` and
    /// this fails — the u64 clamp printed `net 0`, reintroducing one line below
    /// the very clamp `Tokens lost:` was added two lines above to disclose.
    #[test]
    fn an_expansion_dominated_net_is_rendered_negative_not_zero() {
        let summary = crate::analytics::AnalyticsSummary {
            invocations: 10,
            raw_tokens: 1000,
            compressed_tokens: 4000,
            tokens_saved: 500,
            tokens_lost: 3_500,
            expansion_invocations: 6,
            ..Default::default()
        };
        let out = render_one(&summary);
        assert!(
            out.contains("net -3,000"),
            "the net must carry its sign; a clamped `net 0` hides exactly what \
             the line exists to disclose; got:\n{out}"
        );
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
            ..Default::default()
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
}
