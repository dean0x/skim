//! Token analytics persistence layer.
//!
//! Records token savings from every skim invocation into a local SQLite
//! database (`{cache_dir}/skim/analytics.db` — `~/.cache/skim` on Linux,
//! `~/Library/Caches/skim` on macOS, per `dirs::cache_dir`) and provides query
//! functions for the `skim stats` dashboard.
//!
//! ## Design
//!
//! - **SQLite + WAL mode** for concurrent read/write safety.
//! - **Fire-and-forget background threads** -- recording never blocks the
//!   main processing pipeline. Token counting for analytics is deferred to
//!   the background thread so the main thread pays zero BPE cost.
//! - **90-day auto-pruning** via [`AnalyticsDb::maybe_prune`], tracked in
//!   the `analytics_meta` table (schema migration v2).
//! - **[`AnalyticsStore`] trait** abstracts query operations for testability;
//!   test code can provide a mock without a real SQLite database.
//! - **TWO schema regimes in [`schema`], and they are not interchangeable.**
//!   The numbered migrations (v1-v3) are idempotent and each is guarded by a
//!   `user_version` PRAGMA check. The delivered-cost columns
//!   (`notice_tokens`, `notice_bytes`, `served`) are NOT: they are
//!   **presence**-gated -- added iff absent -- and deliberately stamp NO
//!   version at all, because three lineages each want the next number against
//!   the same file (ADR-020). Nothing consults `user_version` to know where
//!   that series opens; the boundary is row-level NULL-ness, which is why
//!   `query_summary` opens the delivered series at `notice_tokens IS NOT NULL`
//!   (alongside operand guards that are not part of the boundary -- see
//!   `AnalyticsDb::DELIVERED_ROW`).
//!
//!   If you are reconciling this with another lineage, read the HAZARD banner
//!   in [`schema`] FIRST. Presence-gating survives a foreign `ALTER` and does
//!   not survive a foreign table REBUILD, in either direction, and both
//!   directions fail silently.

mod schema;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rayon::prelude::*;
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use crate::output::Served;
use crate::tokens;

// ============================================================================
// Shared time constant
// ============================================================================

/// Seconds per day — replaces bare `86400` literals in prune logic and
/// timestamp calculations (used here and in `cmd::hook_log`).
pub(crate) const SECONDS_PER_DAY: u64 = 86_400;

// ============================================================================
// Tokenization caps (applies ADR-001)
// ============================================================================

/// Maximum byte length for background BPE tokenization.
///
/// Inputs above this threshold skip tokenization and use byte length as a proxy.
/// Mirrors TOKEN_SIZE_CAP in `cmd::execution::savings_decision` (ADR-001).
const TOKEN_SIZE_CAP: usize = 256 * 1024;

/// Maximum non-whitespace run length before falling back to byte comparison.
///
/// cl100k BPE merge is O(n²) in run length; this bounds the per-word cost.
/// Mirrors TOKEN_RUN_CAP in `cmd::execution::savings_decision` (ADR-001).
const TOKEN_RUN_CAP: usize = 4 * 1024;

// ============================================================================
// Types
// ============================================================================

/// Type of skim command that produced the savings.
#[derive(Debug, Clone, Copy)]
pub(crate) enum CommandType {
    File,
    Test,
    Build,
    Git,
    Lint,
    Pkg,
    Infra,
    FileOps,
    Log,
    Heatmap,
    Db,
}

impl CommandType {
    fn as_str(&self) -> &'static str {
        match self {
            CommandType::File => "file",
            CommandType::Test => "test",
            CommandType::Build => "build",
            CommandType::Git => "git",
            CommandType::Lint => "lint",
            CommandType::Pkg => "pkg",
            CommandType::Infra => "infra",
            CommandType::FileOps => "fileops",
            CommandType::Log => "log",
            CommandType::Heatmap => "heatmap",
            CommandType::Db => "db",
        }
    }
}

/// What an invocation delivered to the reader beyond its stdout body.
///
/// `Default` is all-`None`, which is the correct value for every path that
/// cannot measure these: the subcommand recording path, cache hits, and
/// `Mode::Full` runs where the guard never ran. `None` persists as SQL NULL and
/// stays distinguishable from a measured `0` — a run that emitted no disclosure
/// is a different fact from a run whose disclosure was never counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Delivery {
    /// cl100k cost of the stderr disclosure this invocation actually emitted,
    /// measured on the emitted bytes by [`crate::output::EmittedNotice`].
    ///
    /// `Some(0)` means "measured: this run emitted no disclosure, or emitted it
    /// against a sibling row". `None` means the cost was never established —
    /// the subcommand path, which charges a different marker family, and the
    /// tokeniser-unavailable case. The delivered series selects on this being
    /// non-NULL, so the distinction decides which rows it covers.
    pub(crate) notice_tokens: Option<usize>,
    /// Byte cost of that same disclosure, terminator included.
    pub(crate) notice_bytes: Option<usize>,
    /// Which view the ADR-001 guard served. `None` where no guard decision was
    /// taken, never a default guess.
    pub(crate) served: Option<Served>,
}

/// A single token savings measurement.
pub(crate) struct TokenSavingsRecord {
    pub(crate) timestamp: i64,
    pub(crate) command_type: CommandType,
    pub(crate) original_cmd: String,
    pub(crate) raw_tokens: usize,
    pub(crate) compressed_tokens: usize,
    pub(crate) savings_pct: f32,
    pub(crate) duration_ms: u64,
    pub(crate) project_path: String,
    pub(crate) mode: Option<String>,
    pub(crate) language: Option<String>,
    pub(crate) parse_tier: Option<String>,
    /// AD-AN-4: session_id is nullable for backward compatibility — rows
    /// recorded before schema v3 have NULL and are excluded from per-session
    /// average calculations.
    pub(crate) session_id: Option<String>,
    /// `Delivery::default()` on every path that takes no such measurement.
    pub(crate) delivery: Delivery,
}

// ============================================================================
// Query result types
// ============================================================================

/// The delivered-savings series, inseparable from the window it covers.
///
/// `first_day`/`last_day`/`rows` are fields of this struct rather than
/// something the dashboard may look up separately, because the one way this
/// number
/// misleads is being read as if it spanned the same history as
/// [`AnalyticsSummary::tokens_saved`]. It cannot: only rows carrying a
/// disclosure measurement have `notice_tokens`, so the window opens where that
/// measurement began and the series is structurally incomparable with anything
/// older. Bundling the window with the value makes printing one without the
/// other require deleting code.
///
/// The boundary is row-level NULL-ness, NOT a schema version — this build
/// stamps no `user_version` for those columns (see the delivered-cost block in
/// [`super::schema`]), so nothing anywhere needs to consult one to know where
/// this series opens.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct DeliveredSavings {
    /// Rows carrying a disclosure measurement. Zero means "not yet measured",
    /// which is not the same as "measured zero saving".
    pub(crate) rows: u64,
    /// `raw - compressed - notice`, summed and **unclamped** — signed, because
    /// a disclosure can cost more than the transform saved, and on 9.3%–10.7%
    /// of saving file rows it does.
    pub(crate) tokens: i64,
    /// Disclosure cost alone, so the correction is legible next to the total.
    ///
    /// Summed over exactly [`Self::rows`], not over every row carrying a
    /// `notice_tokens` value — otherwise the cost and the population it is
    /// charged against could cover different sets of rows.
    pub(crate) notice_tokens: u64,
    /// Rows that DID emit a disclosure whose token cost could not be measured,
    /// and therefore left this series carrying a cost it no longer charges.
    ///
    /// `notice_bytes IS NOT NULL AND notice_tokens IS NULL` is reachable only
    /// through `file_op_notice_cost`'s `Some(n)` arm — a notice was emitted,
    /// its bytes were counted, and
    /// [`crate::output::EmittedNotice::tokens`] returned `None` because the
    /// tokeniser was unavailable. The row is still IN the database; what it
    /// leaves is this SERIES, because [`Self::rows`] selects on
    /// `notice_tokens IS NOT NULL`.
    ///
    /// The bias has a sign, and it is the favourable one: every such row was
    /// going to contribute a COST, so dropping it moves [`Self::tokens`] UP.
    /// Keeping the NULL is still right — a manufactured zero would assert a
    /// measurement that was never taken — so the drop is made countable
    /// instead. Zero here means the series is whole.
    pub(crate) unmeasured_notice_rows: u64,
    /// Earliest contributing row, as a UTC day. `None` when `rows == 0`.
    ///
    /// Formatted by SQLite's `date(timestamp,'unixepoch')`, the same bucketing
    /// `query_daily` uses — and, like it, UTC rather than local time (PF-036).
    /// Consistency with the series next to it matters more here than agreeing
    /// with the wall clock.
    pub(crate) first_day: Option<String>,
    /// Latest contributing row, as a UTC day. `None` when `rows == 0`.
    pub(crate) last_day: Option<String>,
    /// Delivered rows the guard served RAW — the source's own bytes.
    ///
    /// The first reader `served` has ever had. It was written on every measured
    /// row and consumed by no production SELECT, which is what PF-036's
    /// amendment means by "recording a column is not measuring it": under the
    /// 90-day prune it expired unread.
    ///
    /// This is the cut the split is worth taking: the delivered total is a
    /// blend of runs where skim transformed and runs where the guard handed
    /// back the original, and those two populations have opposite cost
    /// profiles. A raw-served run saves nothing and still pays for the
    /// disclosure that says so, so it can only push the total DOWN; a
    /// transformed run is the one the headline is about. One number over both
    /// cannot be read as either.
    pub(crate) served_raw: u64,
    /// Delivered rows the guard served TRANSFORMED.
    pub(crate) served_transformed: u64,
    /// Delivered rows carrying no RECOGNISED serving decision.
    ///
    /// Published so the three ADD UP to [`Self::rows`]. Without it the two
    /// above silently fail to reconcile with the population they are taken
    /// over, and a reader has no way to tell a missing decision from a
    /// miscount — the same defect, in a smaller frame, as a mean that excludes
    /// rows without saying how many.
    ///
    /// It is NOT a fault state. A cache hit records no `served` because the
    /// guard did not run on that invocation, and `Mode::Full` records none
    /// because the guard is skipped for it entirely. Both are legitimate
    /// inside a fully measured row.
    ///
    /// # Why the COMPLEMENT and not `served IS NULL`
    ///
    /// The sum invariant is the whole reason this field exists, and
    /// `served IS NULL` does not deliver it: `served` is a bare `TEXT` column
    /// on a table three lineages write to, with no `CHECK` and no enforcement
    /// anywhere, so a value this build does not know belongs to none of three
    /// buckets keyed on equality — and the counts quietly stop reconciling
    /// with `rows`, which is precisely the failure the third bucket was added
    /// to make impossible. Measured on a copy of the live database
    /// (2026-09-26) over `('raw','transformed',NULL,'proxied')`: the `IS NULL`
    /// form yields `1 + 1 + 1 = 3` against 4 rows, while the complement yields
    /// `1 + 1 + 2 = 4`.
    ///
    /// SQLite's `IS NOT` is the NULL-SAFE comparison — `served IS NOT 'raw'`
    /// is true for a NULL, where `served <> 'raw'` evaluates to NULL and the
    /// row falls out of every bucket. That is what makes this arm total rather
    /// than merely wider.
    pub(crate) served_other: u64,
    /// Seconds since the epoch of the most recent delivered-series RESET, when
    /// one has happened.
    ///
    /// This is what makes `rows == 0` more than one state. Zero rows is the
    /// normal, unremarkable condition of a fresh database, of an upgraded one
    /// before its first measured invocation, of `stats --clear` and of a
    /// 90-day prune — and it is also what a reader sees after a foreign
    /// `token_savings` rebuild dropped the delivered-cost columns and the
    /// presence reconcile re-added them EMPTY. The two are byte-identical in
    /// `token_savings`; the only thing that tells them apart lives in
    /// `analytics_meta`, which a rebuild of `token_savings` cannot reach.
    ///
    /// Read from [`schema::DELIVERED_SERIES_RESET_AT`], NOT from the sibling
    /// `delivered_series_opened_at` — see that const for why the distinction is
    /// load-bearing rather than pedantic.
    ///
    /// `skip_serializing_if` so an unreset series publishes no key at all. A
    /// `"reset_at": null` would hand every JSON consumer a field to test, in a
    /// renderer whose other degenerate-case guard exists precisely because
    /// publishing the absent state as a concrete value is indistinguishable
    /// from publishing a measured one (PF-037).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reset_at: Option<i64>,
}

/// Aggregate summary.
///
/// # Three series, never one blended number
///
/// A single figure spanning a schema change is how a 90-day series stops
/// meaning anything, so the summary carries three and keeps them apart:
///
/// - [`Self::tokens_saved`] — the CONTINUITY series. Clamped per row
///   (`ELSE 0`), definition untouched, comparable across all retained history.
/// - [`Self::tokens_lost`] — retro-computable from `raw_tokens` and
///   `compressed_tokens` alone, so it covers the same full history at no cost
///   in comparability. It is the exact quantity the clamp above discards.
/// - [`Self::delivered`] — disclosure-measured rows ONLY, and carries its own
///   window so it cannot be silently compared against older history.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub(crate) struct AnalyticsSummary {
    pub(crate) invocations: u64,
    pub(crate) raw_tokens: u64,
    pub(crate) compressed_tokens: u64,
    /// CONTINUITY SERIES — deliberately unchanged, clamp included.
    pub(crate) tokens_saved: u64,
    /// CONTINUITY SERIES — deliberately unchanged, no-op rows included.
    pub(crate) avg_savings_pct: f64,
    /// Token expansion the `tokens_saved` clamp discards, summed over the same
    /// rows. Reported beside it so the clamp stops hiding and starts
    /// disclosing.
    ///
    /// Measured on the author's 90-day corpus (68,326 rows): `tokens_saved`
    /// reads 80,238,562 while 11,983,387 tokens of real expansion sit under the
    /// clamp across 6,983 rows — the headline is **+17.6% over the true net**
    /// of 68,255,175.
    pub(crate) tokens_lost: u64,
    /// Rows the [`Self::tokens_lost`] magnitude is spread over — the count of
    /// invocations that EXPANDED (`compressed_tokens > raw_tokens`).
    ///
    /// Reported because `savings_pct` is stored already floored at zero
    /// (see [`savings_percentage`]), so in percentage space an expansion is
    /// indistinguishable from a no-op and no field could carry its magnitude.
    /// The count is the only thing recoverable, so the count is what is
    /// reported: it is what lets a reader see that
    /// [`Self::avg_savings_pct_compressed`] excludes a population rather than
    /// silently averaging it in at 0%.
    pub(crate) expansion_invocations: u64,
    /// Rows where compression actually ran and paid
    /// (`raw_tokens > compressed_tokens AND raw_tokens > 0`) — the population
    /// [`Self::avg_savings_pct_compressed`] is taken over, and the only
    /// population a stored `savings_pct` can honestly be averaged across.
    pub(crate) compressed_invocations: u64,
    /// [`Self::avg_savings_pct`] over rows that compressed.
    ///
    /// # Why not "rows that changed"
    ///
    /// The all-rows mean is diluted by no-ops — invocations where skim served
    /// exactly what it was given, each contributing a 0% sample to an average
    /// about compression. Narrowing to `raw_tokens <> compressed_tokens` removed
    /// the no-ops and kept a second, worse dilution, because `savings_pct` is
    /// FLOORED AT WRITE TIME: an expansion is stored as 0.0, so every expanding
    /// row entered a mean labelled "over the rows that changed" as a 0% SAVING
    /// rather than as the loss it was. Measured on the author's corpus
    /// (69,258 rows, 2026-09-25): that mean printed **35.96%** over 25,682
    /// changed rows, of which **7,037 were expansions** contributing a floored
    /// 0% each; over the 18,645 rows that actually compressed it is **49.54%**,
    /// and the true signed mean over all changed rows — recomputed from
    /// `raw_tokens`/`compressed_tokens`, the only place the sign survives — is
    /// **22.67%**. The printed figure was none of the three.
    ///
    /// So the population is now the one the statistic can describe, and the
    /// rows it excludes are counted in [`Self::expansion_invocations`] rather
    /// than folded in as zeros. This is the same disclosure the `tokens_lost`
    /// line makes in token space, one statistic later; the clamp used to be
    /// exposed two lines above and silent one line below.
    ///
    /// `raw_tokens > 0` is redundant under the sign invariant — `raw > comp`
    /// with a non-negative `comp` already implies it, and 0 rows in the corpus
    /// violate that. It is kept because NOTHING ENFORCES the invariant: the
    /// live table is a foreign rebuild that dropped `NOT NULL` from
    /// `raw_tokens`, `compressed_tokens` and `savings_pct` (ADR-020) and
    /// declares no `CHECK`, so the clause is what makes PF-036's zero-raw
    /// exclusion hold by construction rather than by luck.
    pub(crate) avg_savings_pct_compressed: f64,
    /// The delivered series over disclosure-measured rows, window attached.
    pub(crate) delivered: DeliveredSavings,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct DailyStats {
    pub(crate) date: String,
    pub(crate) invocations: u64,
    pub(crate) tokens_saved: u64,
    pub(crate) avg_savings_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct CommandStats {
    #[serde(rename = "type")]
    pub(crate) command_type: String,
    pub(crate) invocations: u64,
    pub(crate) tokens_saved: u64,
    pub(crate) avg_savings_pct: f64,
    /// Average command duration in milliseconds across all invocations.
    pub(crate) avg_duration_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct LanguageStats {
    pub(crate) language: String,
    pub(crate) files: u64,
    pub(crate) tokens_saved: u64,
    pub(crate) avg_savings_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ModeStats {
    pub(crate) mode: String,
    pub(crate) files: u64,
    pub(crate) tokens_saved: u64,
    pub(crate) avg_savings_pct: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct TierDistribution {
    pub(crate) full_pct: f64,
    pub(crate) degraded_pct: f64,
    pub(crate) passthrough_pct: f64,
}

/// Per-original-command breakdown, grouping by the raw command string stored
/// at recording time (e.g., `"cargo build 2>&1"`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct OriginalCommandStats {
    pub(crate) original_cmd: String,
    pub(crate) invocations: u64,
    pub(crate) tokens_saved: u64,
    pub(crate) avg_savings_pct: f64,
    pub(crate) avg_duration_ms: f64,
}

/// Per-session aggregate statistics derived from the `session_id` column.
///
/// AD-AN-2: Only invocations with a non-NULL `session_id` are counted.
/// Pre-v3 rows (NULL session_id) are excluded so the average reflects
/// actual observed session throughput rather than inflated all-time totals.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct SessionStats {
    /// Number of distinct session IDs observed.
    pub(crate) distinct_sessions: u64,
    /// Tokens saved by session-tagged invocations only (NULL rows excluded).
    /// See `untagged_invocations` for the excluded count.
    pub(crate) total_tokens_saved: u64,
    /// Average tokens saved per session (zero-safe, returns 0.0 when no sessions).
    pub(crate) avg_tokens_per_session: f64,
    /// Invocations without a session_id (NULL rows, excluded from averages).
    pub(crate) untagged_invocations: u64,
}

// ============================================================================
// Pricing
// ============================================================================

#[derive(Debug, Clone, Copy)]
pub(crate) struct PricingModel {
    pub(crate) input_cost_per_mtok: f64,
    pub(crate) tier_name: &'static str,
}

impl PricingModel {
    pub(crate) const ECONOMY: Self = Self {
        input_cost_per_mtok: 1.0,
        tier_name: "Economy",
    };
    pub(crate) const STANDARD: Self = Self {
        input_cost_per_mtok: 3.0,
        tier_name: "Standard",
    };
    /// Advanced tier at $5/MTok.
    ///
    /// AD-AN-3: No model hints are attached — pricing tiers shift frequently
    /// and model names become stale within months. The $5 rate represents a
    /// mid-range tier between Standard ($3) and Premium ($15) that covers
    /// several recently-published model price points.
    pub(crate) const ADVANCED: Self = Self {
        input_cost_per_mtok: 5.0,
        tier_name: "Advanced",
    };
    pub(crate) const PREMIUM: Self = Self {
        input_cost_per_mtok: 15.0,
        tier_name: "Premium",
    };

    pub(crate) fn all_tiers() -> [Self; 4] {
        [Self::ECONOMY, Self::STANDARD, Self::ADVANCED, Self::PREMIUM]
    }

    pub(crate) fn default_pricing() -> Self {
        Self::STANDARD
    }

    /// Build a pricing model from an optional cost override.
    ///
    /// If `cost` is `Some(value)`, returns a Custom tier with that rate.
    /// Otherwise returns the default Standard pricing.
    /// Pure function: no env reads.
    pub(crate) fn from_cost_override(cost: Option<f64>) -> Self {
        match cost {
            Some(c) if c.is_finite() && c >= 0.0 => Self {
                input_cost_per_mtok: c,
                tier_name: "Custom",
            },
            _ => Self::default_pricing(),
        }
    }

    pub(crate) fn estimate_savings(&self, tokens_saved: u64) -> f64 {
        tokens_saved as f64 / 1_000_000.0 * self.input_cost_per_mtok
    }
}

// ============================================================================
// AnalyticsConfig — injected analytics configuration
// ============================================================================

/// Injected analytics configuration created once at the system boundary.
///
/// ARCHITECTURE: Replaces the process-global `ANALYTICS_FORCE_DISABLED` AtomicBool
/// and per-call `SKIM_DISABLE_ANALYTICS` / `SKIM_INPUT_COST_PER_MTOK` env reads.
/// Created in `main()` after CLI parsing and threaded to all callers.
/// Tests construct this struct directly with controlled values — no env mutation.
///
/// AD-AN-1: `Copy` is intentionally not derived because `session_id: Option<String>`
/// contains a heap-allocated `String`. `Clone` is derived for explicit duplication
/// where needed.
#[derive(Debug, Clone)]
pub(crate) struct AnalyticsConfig {
    pub enabled: bool,
    pub input_cost_per_mtok: Option<f64>,
    /// AD-AN-4: Optional session ID injected by the hook rewrite pipeline
    /// (`--session-id=VALUE`). Propagated to every `TokenSavingsRecord` so
    /// the per-session dashboard section can group invocations by session.
    pub session_id: Option<String>,
}

impl AnalyticsConfig {
    /// Read process env once at the system boundary.
    ///
    /// `cli_disable` is the value of `--disable-analytics` from CLI parsing.
    /// `session_id` is extracted from `--session-id=VALUE` in `main()` before
    /// this call. Call this in main(), then thread the result down to all callers.
    pub fn from_process(cli_disable: bool, session_id: Option<String>) -> Self {
        let env_disabled = std::env::var("SKIM_DISABLE_ANALYTICS")
            .ok()
            .map(|v| Self::parse_disable_value(&v))
            .unwrap_or(false);
        let cost = std::env::var("SKIM_INPUT_COST_PER_MTOK")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|c| c.is_finite() && *c >= 0.0);
        Self {
            enabled: !cli_disable && !env_disabled,
            input_cost_per_mtok: cost,
            session_id,
        }
    }

    /// Parse a `SKIM_DISABLE_ANALYTICS` env value string.
    ///
    /// Returns `true` when the value is `"1"`, `"true"`, or `"yes"` (case-insensitive).
    /// Extracted as a pure function so tests can exercise the parsing logic directly.
    pub(crate) fn parse_disable_value(val: &str) -> bool {
        matches!(val.to_lowercase().as_str(), "1" | "true" | "yes")
    }
}

// ============================================================================
// AnalyticsStore trait
// ============================================================================

/// Trait abstracting analytics query operations for testability.
///
/// `AnalyticsDb` implements this trait directly. Test code can provide a
/// `MockStore` without requiring a real SQLite database.
///
/// All query methods have default implementations returning empty/zero values
/// so test mocks only need to override the methods relevant to the behaviour
/// under test.
pub(crate) trait AnalyticsStore {
    fn query_summary(&self, _since: Option<i64>) -> anyhow::Result<AnalyticsSummary> {
        // All-zero with an empty delivered window — the honest shape for
        // "no rows", which is what a mock that does not override this has.
        Ok(AnalyticsSummary::default())
    }
    fn query_daily(&self, _since: Option<i64>) -> anyhow::Result<Vec<DailyStats>> {
        Ok(vec![])
    }
    fn query_by_command(&self, _since: Option<i64>) -> anyhow::Result<Vec<CommandStats>> {
        Ok(vec![])
    }
    fn query_by_language(&self, _since: Option<i64>) -> anyhow::Result<Vec<LanguageStats>> {
        Ok(vec![])
    }
    fn query_by_mode(&self, _since: Option<i64>) -> anyhow::Result<Vec<ModeStats>> {
        Ok(vec![])
    }
    fn query_tier_distribution(&self, _since: Option<i64>) -> anyhow::Result<TierDistribution> {
        Ok(TierDistribution {
            full_pct: 0.0,
            degraded_pct: 0.0,
            passthrough_pct: 0.0,
        })
    }
    fn query_by_original_cmd(
        &self,
        _since: Option<i64>,
    ) -> anyhow::Result<Vec<OriginalCommandStats>> {
        Ok(vec![])
    }
    fn query_session_stats(&self, _since: Option<i64>) -> anyhow::Result<SessionStats> {
        Ok(SessionStats {
            distinct_sessions: 0,
            total_tokens_saved: 0,
            avg_tokens_per_session: 0.0,
            untagged_invocations: 0,
        })
    }
    fn clear(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

// ============================================================================
// AnalyticsDb
// ============================================================================

pub(crate) struct AnalyticsDb {
    conn: Connection,
}

impl AnalyticsDb {
    /// Open database at the given path, run migrations, enable WAL mode.
    ///
    /// On Unix, restricts file permissions to owner-only (0600) after
    /// creation to prevent world-readable analytics data when the DB path
    /// is outside the default 0700 cache directory.
    pub(crate) fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;

        // Restrict DB file permissions to owner-only on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(path) {
                let mut perms = metadata.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(path, perms);
            }
        }

        conn.busy_timeout(Duration::from_millis(5000))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        schema::run_migrations(&conn)?;
        Ok(Self { conn })
    }

    /// Open database at default location, or override with SKIM_ANALYTICS_DB env var.
    ///
    /// Empty or whitespace-only `SKIM_ANALYTICS_DB` is treated as unset and falls
    /// back to the default path (mirrors the `SKIM_CACHE_DIR` hardening in
    /// [`crate::cache::cache_root_from`]).  `SKIM_ANALYTICS_DB` takes precedence
    /// over `SKIM_CACHE_DIR` when both are set.
    pub(crate) fn open_default() -> anyhow::Result<Self> {
        let path = match std::env::var("SKIM_ANALYTICS_DB") {
            Ok(s) if !s.trim().is_empty() => PathBuf::from(s),
            _ => crate::cache::get_cache_dir()?.join("analytics.db"),
        };
        Self::open(&path)
    }

    /// Maximum length for the `original_cmd` column to prevent unbounded
    /// DB growth from extremely long command strings.
    const MAX_CMD_LEN: usize = 500;

    /// Rows a stored `savings_pct` can honestly be averaged over: compression
    /// ran and paid.
    ///
    /// `savings_pct` is floored at zero at WRITE time (see
    /// [`savings_percentage`]), so an expanding row is stored as `0.0` and is
    /// indistinguishable in this column from a no-op. Averaging over any wider
    /// population therefore reports expansions as zero SAVINGS rather than as
    /// losses. `raw_tokens > 0` additionally holds PF-036's zero-raw exclusion
    /// by construction — see
    /// [`AnalyticsSummary::avg_savings_pct_compressed`] for why it is kept even
    /// though the sign invariant makes it redundant.
    const COMPRESSED_ROW: &str = "raw_tokens > compressed_tokens AND raw_tokens > 0";

    /// Rows the delivered series covers — written once, used by every column of
    /// that series so they cannot cover different populations.
    ///
    /// `notice_tokens IS NOT NULL` is the measurement boundary (ADR-020: row-level
    /// NULL-ness, not a schema version). The two operand clauses are NOT
    /// belt-and-braces: `raw_tokens - compressed_tokens - notice_tokens`
    /// evaluates to NULL if either operand is NULL, `SUM` skips NULLs, and the
    /// row would then be counted by a plainer predicate while contributing
    /// nothing to the total it was counted for. The live table is a foreign
    /// rebuild that dropped `NOT NULL` from both operands and `ALTER TABLE`
    /// cannot restore it (ADR-020), so this is the first query on this path whose
    /// correctness cannot lean on that constraint. Latent today
    /// (`SUM(raw_tokens IS NULL) = 0`), unenforceable tomorrow.
    const DELIVERED_ROW: &str =
        "notice_tokens IS NOT NULL AND raw_tokens IS NOT NULL AND compressed_tokens IS NOT NULL";

    /// Record a token savings measurement.
    ///
    /// The `original_cmd` field is truncated to [`Self::MAX_CMD_LEN`] characters
    /// before storage to bound database row size.
    ///
    /// # This INSERT names a FIXED 15 columns, and that is a dependency
    ///
    /// It omits every column it does not know about, which is safe only while
    /// each of those is nullable or carries a `DEFAULT`. Nothing in this build
    /// enforces that — the columns come from lineages this one does not control
    /// (`provider`, `model`, `turn_id`, `upstream_error_status` on the live
    /// database) and it is their choice of nullable that averts the failure, not
    /// an invariant. A foreign rebuild that declares one `NOT NULL` with no
    /// `DEFAULT` makes this statement violate a constraint on EVERY row,
    /// forever, and nothing self-heals it. The full analysis and the merge-time
    /// obligation are in the delivered-cost block in [`super::schema`]; the
    /// failure itself is now visible at `SKIM_DEBUG=1` from `persist_record`
    /// and from [`record_file_ops`]'s batch write, which is the only place it
    /// can be seen at all.
    pub(crate) fn record(&self, r: &TokenSavingsRecord) -> anyhow::Result<()> {
        let cmd = if r.original_cmd.len() > Self::MAX_CMD_LEN {
            // Walk back from MAX_CMD_LEN to the nearest valid UTF-8 character
            // boundary so we never slice through a multi-byte character.
            let mut end = Self::MAX_CMD_LEN;
            while !r.original_cmd.is_char_boundary(end) && end > 0 {
                end -= 1;
            }
            &r.original_cmd[..end]
        } else {
            &r.original_cmd
        };
        self.conn.execute(
            "INSERT INTO token_savings (timestamp, command_type, original_cmd, raw_tokens, compressed_tokens, savings_pct, duration_ms, project_path, mode, language, parse_tier, session_id, notice_tokens, notice_bytes, served)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            rusqlite::params![
                r.timestamp,
                r.command_type.as_str(),
                cmd,
                r.raw_tokens as i64,
                r.compressed_tokens as i64,
                r.savings_pct as f64,
                r.duration_ms as i64,
                r.project_path,
                r.mode,
                r.language,
                r.parse_tier,
                r.session_id,
                // `Option` maps to SQL NULL, preserving "not measured in this
                // regime" as its own value rather than collapsing it onto 0.
                r.delivery.notice_tokens.map(|n| n as i64),
                r.delivery.notice_bytes.map(|n| n as i64),
                r.delivery.served.map(Served::as_str),
            ],
        )?;
        Ok(())
    }

    /// Query aggregate summary — all three series in one pass.
    ///
    /// # Read by name, never by position
    ///
    /// Every expression below is aliased and read back with `row.get("alias")`.
    /// The earlier positional form made adding a column in the middle silently
    /// re-point every index below it, and the failure mode was PLAUSIBLE NUMBERS
    /// rather than an error — one series' value arriving in another's field, in
    /// the subsystem whose whole purpose is to be trustworthy. Adding a column
    /// now costs one alias and cannot disturb its neighbours. Keep it that way:
    /// no positional `row.get(n)` on this query.
    ///
    /// # One predicate per population
    ///
    /// `COMPRESSED_ROW` and `DELIVERED_ROW` are each written ONCE
    /// and interpolated into every column that must cover the same rows. Two
    /// hand-maintained copies of a predicate are two predicates: the delivered
    /// row count and its token sum previously carried different ones
    /// (`notice_tokens IS NOT NULL` against an arithmetic expression that
    /// evaluates NULL if `raw_tokens` or `compressed_tokens` is NULL), so a row
    /// with a NULL operand counted in `rows` and contributed nothing to `tokens`
    /// — a total stated over fewer rows than it named, with no indicator.
    /// Sharing one string makes that divergence unrepresentable.
    ///
    /// `COALESCE(raw_tokens, 0)` would have been the wrong fix: it manufactures
    /// a measurement, which is the one thing this design refuses to do.
    ///
    /// # The `ELSE 0` clamp stays, and is now disclosed
    ///
    /// `tokens_saved` floors per row, consistent with
    /// `query_by_command`/`query_by_language`/`query_by_mode`. It is NOT
    /// removed: it is the definition the existing 90-day series was built on,
    /// and changing it would re-base every historical comparison rather than
    /// correct one. What was wrong was not the clamp but its silence — the
    /// discarded expansion was unrecoverable from the summary. `tokens_lost` is
    /// that same quantity, computed from the same two columns over the same
    /// rows, so it covers the full retained history and the reader can see both
    /// the clamped headline and what the clamp cost.
    /// `tokens_saved - tokens_lost` is the true net.
    ///
    /// `avg_savings_pct_compressed` and its two population counts do the same
    /// job in PERCENTAGE space, where the clamp is upstream and irreversible —
    /// `savings_pct` is stored already floored, so no query can recover an
    /// expansion's magnitude here and only its COUNT is reportable. See
    /// [`AnalyticsSummary::avg_savings_pct_compressed`].
    ///
    /// # The delivered series
    ///
    /// The delivered columns and the window that labels them all select on
    /// `DELIVERED_ROW`, so rows predating disclosure measurement — which
    /// have no delivered measurement rather than a zero one — are excluded from
    /// the value, from the disclosure cost charged against it, AND from the
    /// window. That row predicate is the only boundary; no schema version is
    /// read here or anywhere else on this path.
    ///
    /// `delivered_unmeasured_rows` deliberately does NOT share that predicate:
    /// it counts the rows the predicate REJECTS while still carrying a cost
    /// (see [`DeliveredSavings::unmeasured_notice_rows`]).
    ///
    /// The three `served` counts DO share it, and they PARTITION it: `'raw'`,
    /// `'transformed'`, and a NULL-safe complement that catches everything
    /// else, so they always sum back to `delivered_rows`. Three buckets keyed
    /// on equality would not — see [`DeliveredSavings::served_other`].
    ///
    /// # The reset mark is read OUTSIDE this query, and has to be
    ///
    /// [`DeliveredSavings::reset_at`] comes from `analytics_meta`, not from
    /// `token_savings`, and cannot be folded into the pass above: the whole
    /// reason the mark is recoverable is that it lives in a table a
    /// `token_savings` rebuild does not touch. It is also deliberately NOT
    /// windowed by `since` — a reset is a fact about the database, not a row
    /// in a date range, and suppressing it for a narrow `--since` would hide
    /// the one thing that explains why that window is empty.
    pub(crate) fn query_summary(&self, since: Option<i64>) -> anyhow::Result<AnalyticsSummary> {
        let (where_clause, params) = since_clause(since);
        let compressed_row = Self::COMPRESSED_ROW;
        let delivered_row = Self::DELIVERED_ROW;
        let sql = format!(
            "SELECT COUNT(*) AS invocations, \
             COALESCE(SUM(raw_tokens), 0) AS raw_tokens, \
             COALESCE(SUM(compressed_tokens), 0) AS compressed_tokens, \
             COALESCE(AVG(savings_pct), 0) AS avg_savings_pct, \
             COALESCE(SUM(CASE WHEN raw_tokens > compressed_tokens THEN raw_tokens - compressed_tokens ELSE 0 END), 0) AS tokens_saved, \
             COALESCE(SUM(CASE WHEN compressed_tokens > raw_tokens THEN compressed_tokens - raw_tokens ELSE 0 END), 0) AS tokens_lost, \
             COALESCE(SUM(CASE WHEN compressed_tokens > raw_tokens THEN 1 ELSE 0 END), 0) AS expansion_invocations, \
             COALESCE(SUM(CASE WHEN {compressed_row} THEN 1 ELSE 0 END), 0) AS compressed_invocations, \
             COALESCE(AVG(CASE WHEN {compressed_row} THEN savings_pct END), 0) AS avg_savings_pct_compressed, \
             COALESCE(SUM(CASE WHEN {delivered_row} THEN 1 ELSE 0 END), 0) AS delivered_rows, \
             COALESCE(SUM(CASE WHEN {delivered_row} THEN raw_tokens - compressed_tokens - notice_tokens ELSE 0 END), 0) AS delivered_tokens, \
             COALESCE(SUM(CASE WHEN {delivered_row} THEN notice_tokens ELSE 0 END), 0) AS delivered_notice_tokens, \
             COALESCE(SUM(CASE WHEN notice_bytes IS NOT NULL AND notice_tokens IS NULL THEN 1 ELSE 0 END), 0) AS delivered_unmeasured_rows, \
             COALESCE(SUM(CASE WHEN {delivered_row} AND served = 'raw' THEN 1 ELSE 0 END), 0) AS delivered_served_raw, \
             COALESCE(SUM(CASE WHEN {delivered_row} AND served = 'transformed' THEN 1 ELSE 0 END), 0) AS delivered_served_transformed, \
             COALESCE(SUM(CASE WHEN {delivered_row} AND served IS NOT 'raw' AND served IS NOT 'transformed' THEN 1 ELSE 0 END), 0) AS delivered_served_other, \
             date(MIN(CASE WHEN {delivered_row} THEN timestamp END), 'unixepoch') AS delivered_first_day, \
             date(MAX(CASE WHEN {delivered_row} THEN timestamp END), 'unixepoch') AS delivered_last_day \
             FROM token_savings {where_clause}"
        );
        let reset_at = self.delivered_series_reset_at();
        let mut stmt = self.conn.prepare(&sql)?;
        let row = stmt.query_row(rusqlite::params_from_iter(params), |row| {
            let raw_tokens: i64 = row.get("raw_tokens")?;
            let compressed_tokens: i64 = row.get("compressed_tokens")?;
            let tokens_saved: i64 = row.get("tokens_saved")?;
            let tokens_lost: i64 = row.get("tokens_lost")?;
            let delivered_notice_tokens: i64 = row.get("delivered_notice_tokens")?;
            Ok(AnalyticsSummary {
                invocations: row.get("invocations")?,
                raw_tokens: raw_tokens as u64,
                compressed_tokens: compressed_tokens as u64,
                // .max(0) is defensive; CASE WHEN already floors per-row.
                tokens_saved: tokens_saved.max(0) as u64,
                avg_savings_pct: row.get("avg_savings_pct")?,
                tokens_lost: tokens_lost.max(0) as u64,
                expansion_invocations: row.get("expansion_invocations")?,
                compressed_invocations: row.get("compressed_invocations")?,
                avg_savings_pct_compressed: row.get("avg_savings_pct_compressed")?,
                delivered: DeliveredSavings {
                    rows: row.get("delivered_rows")?,
                    // NOT clamped: the sign is the finding.
                    tokens: row.get("delivered_tokens")?,
                    notice_tokens: delivered_notice_tokens.max(0) as u64,
                    unmeasured_notice_rows: row.get("delivered_unmeasured_rows")?,
                    first_day: row.get("delivered_first_day")?,
                    last_day: row.get("delivered_last_day")?,
                    served_raw: row.get("delivered_served_raw")?,
                    served_transformed: row.get("delivered_served_transformed")?,
                    served_other: row.get("delivered_served_other")?,
                    reset_at,
                },
            })
        })?;
        Ok(row)
    }

    /// When the delivered series was last RESET, if it ever was.
    ///
    /// # Absence is the answer here, never an error
    ///
    /// Three distinct shapes have to read as "no reset", and only one of them
    /// is the key simply not being there:
    ///
    /// - no `delivered_series_reset_at` row — the overwhelmingly common case,
    ///   and what `.optional()` folds to `None`;
    /// - no `analytics_meta` TABLE at all — `note_delivered_columns_reconciled`
    ///   creates it lazily, and a foreign-lineage database that arrives at a
    ///   `user_version` above skim's ladder skips the rung that would have.
    ///   SQLite answers that with a plain error, not with zero rows;
    /// - a value another lineage wrote as something other than an integer.
    ///
    /// None of the three is a reason to fail `query_summary` and take the whole
    /// 90-day dashboard down with it, so the tolerance is deliberate and
    /// total — `unwrap_or(None)` is the tolerance, and it is narrow because the
    /// only thing being given up is one disclosure LINE. Erring the other way
    /// would trade a dashboard for it.
    fn delivered_series_reset_at(&self) -> Option<i64> {
        self.conn
            .query_row(
                "SELECT value FROM analytics_meta WHERE key = ?1",
                [schema::DELIVERED_SERIES_RESET_AT],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .unwrap_or(None)
    }

    /// Query daily breakdown.
    pub(crate) fn query_daily(&self, since: Option<i64>) -> anyhow::Result<Vec<DailyStats>> {
        let (where_clause, params) = since_clause(since);
        // CASE WHEN flooring is consistent with query_by_command/lang/mode/session.
        // Expansion rows (compressed_tokens > raw_tokens) contribute 0 to tokens_saved.
        let sql = format!(
            "SELECT date(timestamp, 'unixepoch') as day, COUNT(*), \
             COALESCE(SUM(CASE WHEN raw_tokens > compressed_tokens THEN raw_tokens - compressed_tokens ELSE 0 END), 0), \
             COALESCE(AVG(savings_pct), 0) \
             FROM token_savings {where_clause} GROUP BY day ORDER BY day DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok(DailyStats {
                date: row.get(0)?,
                invocations: row.get(1)?,
                tokens_saved: row.get::<_, i64>(2)?.max(0) as u64,
                avg_savings_pct: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Query breakdown by command type.
    pub(crate) fn query_by_command(&self, since: Option<i64>) -> anyhow::Result<Vec<CommandStats>> {
        let (where_clause, params) = since_clause(since);
        let sql = format!(
            "SELECT command_type, COUNT(*), \
             COALESCE(SUM(CASE WHEN raw_tokens > compressed_tokens THEN raw_tokens - compressed_tokens ELSE 0 END), 0), \
             COALESCE(AVG(savings_pct), 0), COALESCE(AVG(duration_ms), 0.0) \
             FROM token_savings {where_clause} \
             GROUP BY command_type \
             ORDER BY SUM(CASE WHEN raw_tokens > compressed_tokens THEN raw_tokens - compressed_tokens ELSE 0 END) DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok(CommandStats {
                command_type: row.get(0)?,
                invocations: row.get(1)?,
                tokens_saved: row.get::<_, i64>(2)?.max(0) as u64,
                avg_savings_pct: row.get(3)?,
                avg_duration_ms: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Query breakdown by language (file operations only).
    pub(crate) fn query_by_language(
        &self,
        since: Option<i64>,
    ) -> anyhow::Result<Vec<LanguageStats>> {
        let (clause, params) = since_clause_with_extra(since, "language IS NOT NULL");
        let sql = format!(
            "SELECT language, COUNT(*), \
             COALESCE(SUM(CASE WHEN raw_tokens > compressed_tokens THEN raw_tokens - compressed_tokens ELSE 0 END), 0), \
             COALESCE(AVG(savings_pct), 0) \
             FROM token_savings {clause} \
             GROUP BY language \
             ORDER BY SUM(CASE WHEN raw_tokens > compressed_tokens THEN raw_tokens - compressed_tokens ELSE 0 END) DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok(LanguageStats {
                language: row.get(0)?,
                files: row.get(1)?,
                tokens_saved: row.get::<_, i64>(2)?.max(0) as u64,
                avg_savings_pct: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Query breakdown by mode (file operations only).
    pub(crate) fn query_by_mode(&self, since: Option<i64>) -> anyhow::Result<Vec<ModeStats>> {
        let (clause, params) = since_clause_with_extra(since, "mode IS NOT NULL");
        let sql = format!(
            "SELECT mode, COUNT(*), \
             COALESCE(SUM(CASE WHEN raw_tokens > compressed_tokens THEN raw_tokens - compressed_tokens ELSE 0 END), 0), \
             COALESCE(AVG(savings_pct), 0) \
             FROM token_savings {clause} \
             GROUP BY mode \
             ORDER BY SUM(CASE WHEN raw_tokens > compressed_tokens THEN raw_tokens - compressed_tokens ELSE 0 END) DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok(ModeStats {
                mode: row.get(0)?,
                files: row.get(1)?,
                tokens_saved: row.get::<_, i64>(2)?.max(0) as u64,
                avg_savings_pct: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Query parse tier distribution (command operations only).
    pub(crate) fn query_tier_distribution(
        &self,
        since: Option<i64>,
    ) -> anyhow::Result<TierDistribution> {
        let (clause, params) = since_clause_with_extra(since, "parse_tier IS NOT NULL");
        let sql = format!(
            "SELECT COALESCE(SUM(CASE WHEN parse_tier = 'full' THEN 1 ELSE 0 END), 0), \
             COALESCE(SUM(CASE WHEN parse_tier = 'degraded' THEN 1 ELSE 0 END), 0), \
             COALESCE(SUM(CASE WHEN parse_tier = 'passthrough' THEN 1 ELSE 0 END), 0), \
             COUNT(*) FROM token_savings {clause}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let row = stmt.query_row(rusqlite::params_from_iter(params), |row| {
            let full: i64 = row.get(0)?;
            let degraded: i64 = row.get(1)?;
            let passthrough: i64 = row.get(2)?;
            let total: i64 = row.get(3)?;
            let t = if total > 0 { total as f64 } else { 1.0 };
            Ok(TierDistribution {
                full_pct: full as f64 / t * 100.0,
                degraded_pct: degraded as f64 / t * 100.0,
                passthrough_pct: passthrough as f64 / t * 100.0,
            })
        })?;
        Ok(row)
    }

    /// Query breakdown by original command string (top 15 by tokens saved).
    pub(crate) fn query_by_original_cmd(
        &self,
        since: Option<i64>,
    ) -> anyhow::Result<Vec<OriginalCommandStats>> {
        let (where_clause, params) = since_clause(since);
        let sql = format!(
            "SELECT original_cmd, COUNT(*) as cnt, \
             SUM(CASE WHEN raw_tokens > compressed_tokens THEN raw_tokens - compressed_tokens ELSE 0 END) as saved, \
             AVG(savings_pct) as avg_pct, AVG(duration_ms) as avg_dur \
             FROM token_savings {where_clause} \
             GROUP BY original_cmd \
             ORDER BY saved DESC \
             LIMIT 15"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok(OriginalCommandStats {
                original_cmd: row.get(0)?,
                invocations: row.get::<_, i64>(1)? as u64,
                tokens_saved: row.get::<_, i64>(2)?.max(0) as u64,
                avg_savings_pct: row.get(3)?,
                avg_duration_ms: row.get(4)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Prune records older than N days.
    pub(crate) fn prune_older_than(&self, days: u64) -> anyhow::Result<usize> {
        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
            - (days as i64 * SECONDS_PER_DAY as i64);
        let count = self
            .conn
            .execute("DELETE FROM token_savings WHERE timestamp < ?1", [cutoff])?;
        Ok(count)
    }

    /// Prune if last prune was >24h ago. Uses the `analytics_meta` table
    /// (created by schema migration v2) for tracking.
    pub(crate) fn maybe_prune(&self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let last_prune: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE((SELECT value FROM analytics_meta WHERE key = 'last_prune'), 0)",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if now as i64 - last_prune > SECONDS_PER_DAY as i64 && self.prune_older_than(90).is_ok() {
            let _ = self.conn.execute(
                "INSERT OR REPLACE INTO analytics_meta (key, value) VALUES ('last_prune', ?1)",
                [now as i64],
            );
        }
    }

    /// Delete records where compressed_tokens > raw_tokens (invalid data from
    /// pre-fix versions that did not clamp at recording time).
    ///
    /// Gated behind an `analytics_meta` sentinel key `invalid_records_cleaned`
    /// so the DELETE only runs once, not on every `skim stats` invocation.
    /// After cleaning, the sentinel is written so subsequent calls are no-ops.
    pub(crate) fn clean_invalid_records(&self) -> anyhow::Result<usize> {
        // Check sentinel — if already cleaned, skip the full table scan.
        let already_cleaned: bool = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM analytics_meta WHERE key = 'invalid_records_cleaned'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;

        if already_cleaned {
            return Ok(0);
        }

        let count = self.conn.execute(
            "DELETE FROM token_savings WHERE compressed_tokens > raw_tokens",
            [],
        )?;

        // Write sentinel so this never runs again.
        let _ = self.conn.execute(
            "INSERT OR REPLACE INTO analytics_meta (key, value) VALUES ('invalid_records_cleaned', 1)",
            [],
        );

        Ok(count)
    }

    /// Query per-session statistics grouped by session_id.
    ///
    /// Uses a single conditional-aggregation query instead of two separate queries
    /// to avoid a TOCTOU race between the two reads and to reduce round-trips.
    /// `COUNT(DISTINCT session_id)` naturally ignores NULL rows, so no extra WHERE
    /// clause is needed for the session count.
    ///
    /// Division by zero is guarded: when `distinct_sessions == 0`,
    /// `avg_tokens_per_session` is 0.0 (computed in Rust after the query).
    pub(crate) fn query_session_stats(&self, since: Option<i64>) -> anyhow::Result<SessionStats> {
        // F2: Single query using conditional aggregation — eliminates the two-query pattern.
        let (where_clause, params) = since_clause(since);
        let sql = format!(
            "SELECT \
             COUNT(DISTINCT session_id), \
             COALESCE(SUM(CASE WHEN raw_tokens > compressed_tokens AND session_id IS NOT NULL \
                              THEN raw_tokens - compressed_tokens ELSE 0 END), 0), \
             COALESCE(SUM(CASE WHEN session_id IS NULL THEN 1 ELSE 0 END), 0) \
             FROM token_savings {where_clause}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let (distinct_sessions, total_tokens_saved, untagged_invocations): (u64, i64, u64) =
            stmt.query_row(rusqlite::params_from_iter(params), |row| {
                Ok((
                    row.get::<_, u64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, u64>(2)?,
                ))
            })?;

        let total_tokens_saved = total_tokens_saved.max(0) as u64;
        let avg_tokens_per_session = if distinct_sessions > 0 {
            total_tokens_saved as f64 / distinct_sessions as f64
        } else {
            0.0
        };

        Ok(SessionStats {
            distinct_sessions,
            total_tokens_saved,
            avg_tokens_per_session,
            untagged_invocations,
        })
    }
}

impl AnalyticsStore for AnalyticsDb {
    fn query_summary(&self, since: Option<i64>) -> anyhow::Result<AnalyticsSummary> {
        self.query_summary(since)
    }
    fn query_daily(&self, since: Option<i64>) -> anyhow::Result<Vec<DailyStats>> {
        self.query_daily(since)
    }
    fn query_by_command(&self, since: Option<i64>) -> anyhow::Result<Vec<CommandStats>> {
        self.query_by_command(since)
    }
    fn query_by_language(&self, since: Option<i64>) -> anyhow::Result<Vec<LanguageStats>> {
        self.query_by_language(since)
    }
    fn query_by_mode(&self, since: Option<i64>) -> anyhow::Result<Vec<ModeStats>> {
        self.query_by_mode(since)
    }
    fn query_tier_distribution(&self, since: Option<i64>) -> anyhow::Result<TierDistribution> {
        self.query_tier_distribution(since)
    }
    fn query_by_original_cmd(
        &self,
        since: Option<i64>,
    ) -> anyhow::Result<Vec<OriginalCommandStats>> {
        self.query_by_original_cmd(since)
    }
    fn query_session_stats(&self, since: Option<i64>) -> anyhow::Result<SessionStats> {
        self.query_session_stats(since)
    }
    fn clear(&self) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM token_savings", [])?;
        Ok(())
    }
}

/// Build WHERE clause for optional since filter.
fn since_clause(since: Option<i64>) -> (String, Vec<i64>) {
    match since {
        Some(ts) => ("WHERE timestamp >= ?1".to_string(), vec![ts]),
        None => (String::new(), vec![]),
    }
}

/// Build WHERE clause with an optional extra condition appended.
///
/// Composes the `since` filter with an additional SQL predicate (e.g.
/// `"language IS NOT NULL"`). The extra condition is AND-ed to the since
/// clause when present, or becomes its own WHERE clause when since is None.
fn since_clause_with_extra(since: Option<i64>, extra_condition: &str) -> (String, Vec<i64>) {
    let (base, params) = since_clause(since);
    let clause = if base.is_empty() {
        format!("WHERE {extra_condition}")
    } else {
        format!("{base} AND {extra_condition}")
    };
    (clause, params)
}

// ============================================================================
// RecordingContext — bundles analytics metadata for subcommand handlers
// ============================================================================

/// Bundles analytics recording parameters threaded through subcommand handlers.
///
/// `Copy` keeps call sites clean (no `&rec` or `.clone()`).  See
/// [`crate::cmd::RunContext`] for the broader dispatch-layer struct that also
/// carries UI concerns (`show_stats`, `json_output`) irrelevant to recording.
/// `RunContext` owns its strings; `RecordingContext` borrows them — the lifetime
/// boundary is intentional.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RecordingContext<'a> {
    /// Whether analytics recording is enabled for this invocation.
    pub enabled: bool,
    /// The command family tag stored in the analytics DB.
    pub command_type: CommandType,
    /// Optional tier label set by the parser (e.g. `"full"`, `"degraded"`).
    pub parse_tier: Option<&'a str>,
    /// Hook-injected session identifier (AD-AN-4).
    pub session_id: Option<&'a str>,
}

impl<'a> RecordingContext<'a> {
    /// Return a copy of `self` with `parse_tier` set to `Some(tier)`.
    pub(crate) fn with_tier(self, tier: &'a str) -> Self {
        Self {
            parse_tier: Some(tier),
            ..self
        }
    }

    /// Return a copy of `self` with `parse_tier` set to `tier`.
    ///
    /// Use when the tier is already `Option<&'a str>` (e.g. from a parser result).
    pub(crate) fn with_tier_opt(self, tier: Option<&'a str>) -> Self {
        Self {
            parse_tier: tier,
            ..self
        }
    }
}

/// Owned recording parameters for the background recording thread.
///
/// Bundles the fields that `record_fire_and_forget` previously accepted
/// individually so the thread spawning function stays under the
/// `clippy::too_many_arguments` threshold.
struct FireAndForgetParams {
    command_type: CommandType,
    project_path: String,
    parse_tier: Option<String>,
    session_id: Option<String>,
}

// ============================================================================
// Fire-and-forget recording functions
// ============================================================================

/// Returns `true` if `sid` is safe for shell command interpolation.
///
/// Allows `[a-zA-Z0-9_\-.]`, max 128 chars. Rejects empty, oversized,
/// and metacharacter-bearing values to prevent command injection.
///
/// ## Why 128 chars?
///
/// Session IDs are agent-generated opaque identifiers (typically UUIDs or
/// short descriptive strings). 128 characters is generous for any plausible
/// legitimate value while bounding the injected flag length in command strings.
///
/// ## Rationale for allowed characters
///
/// `[a-zA-Z0-9_-.]` covers UUIDs, ISO 8601 timestamps, dot-separated
/// identifiers, and human-readable session names. All other characters —
/// including shell metacharacters (`;`, `|`, `$`, spaces, backticks, etc.)
/// — are rejected.
pub(crate) fn is_safe_session_id(sid: &str) -> bool {
    !sid.is_empty()
        && sid.len() <= 128
        && sid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// Compute token savings as a percentage.
///
/// Returns 0.0 when:
/// - `raw_tokens` is zero (nothing to compress), or
/// - `compressed_tokens >= raw_tokens` (0% savings, e.g. passthrough mode or
///   very small files — this is valid, not an error condition).
pub(crate) fn savings_percentage(raw_tokens: usize, compressed_tokens: usize) -> f32 {
    if raw_tokens == 0 || compressed_tokens >= raw_tokens {
        0.0
    } else {
        (raw_tokens as f32 - compressed_tokens as f32) / raw_tokens as f32 * 100.0
    }
}

/// Current Unix timestamp in seconds.
pub(crate) fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ============================================================================
// Background thread registry for join-before-exit
// ============================================================================

/// Registry of analytics thread handles for join-before-exit.
///
/// Each spawned analytics thread is registered here so that `flush_pending()`
/// can join them before the process exits, ensuring DB writes complete.
static PENDING_THREADS: Mutex<Vec<std::thread::JoinHandle<()>>> = Mutex::new(Vec::new());

/// Register a spawned analytics thread handle.
///
/// Recovers from a poisoned mutex (via `into_inner()`) so that a prior
/// analytics thread panic does not silently drop all subsequent handles.
/// Emits a debug warning when `SKIM_DEBUG=1` if the mutex was poisoned.
fn register_thread(handle: std::thread::JoinHandle<()>) {
    let mut handles = match PENDING_THREADS.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            if crate::debug::is_debug_enabled() {
                eprintln!("[skim:debug] analytics: PENDING_THREADS mutex was poisoned; recovering");
            }
            poisoned.into_inner()
        }
    };
    handles.push(handle);
}

/// Join all pending analytics threads before process exit.
///
/// Call from `main()` before returning `ExitCode`. This ensures all
/// background analytics DB writes complete before the process exits —
/// without this, short-lived commands may terminate before the thread
/// finishes writing to SQLite.
///
/// ## Blocking trade-off
///
/// This function joins every pending thread synchronously, which means it
/// blocks until all analytics DB writes complete. On a fast local disk
/// writes finish in <50 ms; on a slow network filesystem they could take
/// 200 ms or more. The alternative — fire-and-forget without joining —
/// would silently discard analytics data for short-lived commands, which
/// defeats the purpose of the feature. The blocking exit latency is
/// accepted as the lesser trade-off because:
///
/// 1. Analytics writes happen at most once per CLI invocation.
/// 2. Exit latency is far less visible to users than startup latency.
/// 3. `std::thread::JoinHandle` has no native timeout in Rust, so a
///    bounded-timeout approach would require additional unsafe plumbing.
///
/// Recovers from a poisoned mutex (via `into_inner()`) so that a prior
/// analytics thread panic does not cause the pending handles to leak.
/// Emits a debug warning when `SKIM_DEBUG=1` if the mutex was poisoned.
pub(crate) fn flush_pending() {
    let mut handles = match PENDING_THREADS.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "[skim:debug] analytics: PENDING_THREADS mutex was poisoned during flush; recovering"
                );
            }
            poisoned.into_inner()
        }
    };
    for handle in handles.drain(..) {
        let _ = handle.join();
    }
}

/// Persist a record to the default database, with auto-pruning.
///
/// # The discard stays; the silence does not
///
/// Both failures are still swallowed — analytics must never crash the main
/// path, and that part is correct by design. What was wrong was that they were
/// swallowed *without trace*: a migration failure, a permissions failure or an
/// INSERT that fails on every row forever all looked exactly like a machine
/// where nothing had been recorded yet, with no diagnostic anywhere.
///
/// That silence is load-bearing for the hazards the delivered-cost block in
/// [`super::schema`] documents. Each of them — a foreign table rebuild dropping
/// our columns, a rebuild adding a `NOT NULL` column with no `DEFAULT` so this
/// build's fixed 15-column INSERT violates a constraint on every row, a
/// same-named foreign column adopted with different semantics, a concurrent
/// unreconciled open losing the presence-gate race with `duplicate column name`
/// — surfaces HERE and nowhere else. `SKIM_DEBUG=1` is now the whole diagnosis
/// path for all of them.
fn persist_record(record: &TokenSavingsRecord) {
    match AnalyticsDb::open_default() {
        Ok(db) => {
            if let Err(e) = db.record(record) {
                crate::debug_log!("[skim:debug] analytics: record discarded: {e}");
            }
            db.maybe_prune();
        }
        Err(e) => crate::debug_log!("[skim:debug] analytics: open_default failed: {e}"),
    }
}

/// Record command output token savings. Defers token counting to background thread.
///
/// Callers must check `enabled` before calling; this function always records.
/// The single external caller (`try_record_command`) already guards on `enabled`,
/// so removing the redundant parameter here keeps the argument count low.
fn record_fire_and_forget(
    raw_text: String,
    compressed_text: String,
    original_cmd: String,
    duration: Duration,
    params: FireAndForgetParams,
) {
    let FireAndForgetParams {
        command_type,
        project_path,
        parse_tier,
        session_id,
    } = params;
    register_thread(std::thread::spawn(move || {
        let Ok(raw_tokens) = tokens::count_tokens(&raw_text) else {
            return;
        };
        // Short-circuit: when raw and compressed are byte-identical (e.g. a grep/rg
        // RawPassthrough where both contain the same original stdout content), reuse
        // the raw token count rather than running a second BPE pass. An O(n) memcmp
        // beats a second BPE pass; 0% savings — no point tokenising twice.
        let comp_tokens = if compressed_text == raw_text {
            raw_tokens
        } else {
            let Ok(ct) = tokens::count_tokens(&compressed_text) else {
                return;
            };
            ct
        };
        let record = TokenSavingsRecord {
            timestamp: now_unix_secs(),
            command_type,
            original_cmd,
            raw_tokens,
            compressed_tokens: comp_tokens,
            savings_pct: savings_percentage(raw_tokens, comp_tokens),
            duration_ms: duration.as_millis() as u64,
            project_path,
            mode: None,
            language: None,
            parse_tier,
            session_id,
            // Subcommand path: the file-read lossy-view disclosure does not
            // exist here, and commit 14's build-family marker is deliberately
            // NOT charged. Nothing was measured, so nothing is claimed.
            delivery: Delivery::default(),
        };
        persist_record(&record);
    }));
}

/// Record file operation token savings where counts are already known.
///
/// Accepts a fully-constructed [`TokenSavingsRecord`] and persists it on
/// a background thread. The `timestamp` and `savings_pct` fields should
/// be populated by the caller (use [`now_unix_secs`] and
/// [`savings_percentage`] helpers).
pub(crate) fn record_with_counts(enabled: bool, record: TokenSavingsRecord) {
    if !enabled {
        return;
    }
    register_thread(std::thread::spawn(move || {
        persist_record(&record);
    }));
}

// ============================================================================
// File-op unified recorder (Phase A1 — fixes PF-001)
// ============================================================================

/// Count tokens in `text` with ADR-001 size and run-length caps.
///
/// - If `text.len() > TOKEN_SIZE_CAP`: falls back to byte-length heuristic (`len / 4`).
/// - If the longest non-whitespace run exceeds `TOKEN_RUN_CAP` (only checked when
///   below the size cap): falls back to byte-length heuristic to avoid O(n²) BPE cost.
/// - Otherwise: delegates to [`tokens::count_tokens`].
///
/// Using the byte heuristic for oversized inputs is consistent with the approach
/// in `cmd::execution::savings_decision` (applies ADR-001): token accuracy matters
/// most for small, typical source files which are always below the cap.
fn count_tokens_bounded(text: &str) -> usize {
    if text.len() > TOKEN_SIZE_CAP {
        return text.len() / 4;
    }
    // Longest non-whitespace run check (O(n), bounded to ≤TOKEN_SIZE_CAP bytes).
    let longest_run = text
        .as_bytes()
        .split(|b| b.is_ascii_whitespace())
        .map(|run| run.len())
        .max()
        .unwrap_or(0);
    if longest_run > TOKEN_RUN_CAP {
        return text.len() / 4;
    }
    // Safe to tokenize: input is small and well-formed.
    tokens::count_tokens(text).unwrap_or(text.len() / 4)
}

/// Source of the raw text for background tokenization.
///
/// - `Reread(path)`: single file — re-read from disk on the background thread.
/// - `Inline(text)`: stdin — retain the buffer in-memory (stdin cannot be re-read).
pub(crate) enum RawSource {
    Reread(PathBuf),
    Inline(String),
}

/// How token counts are obtained for a file-op row.
pub(crate) enum FileCounts {
    /// `--show-stats` or count-carrying cache hit: counts already computed, no re-work.
    Known { raw: usize, compressed: usize },
    /// Plain run / cold cache: tokenize raw + compressed off the main thread.
    Tokenize { raw: RawSource, compressed: String },
}

/// Per-file data for a single analytics row.
pub(crate) struct FileOpRow {
    pub(crate) counts: FileCounts,
    pub(crate) original_cmd: String,
    pub(crate) language: Option<String>,
    pub(crate) parse_tier: Option<String>,
    /// The disclosure this row is charged, carried as the emitted bytes
    /// themselves rather than as a number derived from them.
    ///
    /// Meaningful only while [`Self::notice_measured`] is `true`. On a
    /// single-file invocation the marker is per-file, so `Some` is that file's
    /// cost and `None` is a measured zero. On a batch this field is `None` and
    /// [`Self::notice_measured`] is `false`, which is a different statement
    /// entirely — see that field.
    ///
    /// Tokenised on the background thread, never here: see
    /// [`crate::output::EmittedNotice::tokens`].
    pub(crate) notice: Option<crate::output::EmittedNotice>,
    /// Whether a per-file disclosure cost was measurable for this row at all.
    ///
    /// `false` on batch rows: the disclosure is RUN-scoped and the schema has
    /// no invocation dimension, so both columns record SQL NULL ("not measured
    /// in this regime") rather than a per-file number that was never emitted.
    ///
    /// # Why this is a separate field and not `notice: None`
    ///
    /// A multi-file run emits ONE aggregate marker for the whole run. The two
    /// obvious encodings are both wrong, and wrong invisibly:
    ///
    /// - charge every differing row — the run is billed N times for one line
    ///   of stderr;
    /// - charge the FIRST differing row and give the rest `notice: None` —
    ///   which is what this did. `file_op_notice_cost` maps `None` to
    ///   `(Some(0), Some(0))`, so after a 40-file run one arbitrary file
    ///   carried the run's entire disclosure cost and 39 carried a MEASURED
    ///   ZERO, in per-file columns, with nothing in any row marking the
    ///   regime. Every per-file query over those columns is mis-attributed.
    ///
    /// Passing `notice: None` on every batch row does not fix it: that records
    /// a measured zero for the whole run and deletes the run's real disclosure
    /// cost from the delivered series, biasing the headline UPWARD — PF-036's
    /// favourable direction, and worse than the mis-attribution. Only a value
    /// `notice: Option<..>` cannot express will do, which is why the channel is
    /// widened here rather than overloaded.
    ///
    /// BOTH columns must go NULL together, and that is a query-level
    /// requirement, not tidiness: `AnalyticsDb::DELIVERED_ROW` selects on
    /// `notice_tokens IS NOT NULL`, while `delivered_unmeasured_rows` counts
    /// `notice_bytes IS NOT NULL AND notice_tokens IS NULL`. NULLing only
    /// `notice_tokens` would move every batch row out of the series and into
    /// the counter that exists to expose untokenisable DISCLOSURES —
    /// corrupting the statistic instead of staying out of both.
    pub(crate) notice_measured: bool,
    /// Which view the guard served for this file, when a guard decision was
    /// taken at all.
    pub(crate) served: Option<Served>,
}

/// Shared metadata common to all rows in a single file-op invocation.
pub(crate) struct FileOpCommon {
    pub(crate) mode: Option<String>,
    pub(crate) project_path: String,
    pub(crate) session_id: Option<String>,
}

/// `(notice_tokens, notice_bytes)` for one file-op row.
///
/// # `measured` is the regime, `notice` is the value
///
/// `measured == false` short-circuits to `(None, None)` — SQL NULL in both
/// columns, "not measured in this regime". It is the batch case: the
/// disclosure is RUN-scoped, `token_savings` has no invocation dimension to
/// hang it on, and every per-file encoding of it is a number that was never
/// emitted. Both columns go NULL together so the row lands in neither
/// `AnalyticsDb::DELIVERED_ROW` nor `delivered_unmeasured_rows` — see
/// [`FileOpRow::notice_measured`].
///
/// Within a measured regime, "no notice" is a measured **zero**, not an
/// unknown: a single-file path knows whether its invocation emitted a
/// disclosure, and a raw-served file genuinely bore no cost. `None` is
/// reserved for the one case that really is unmeasured — the tokeniser failing
/// — so `notice_tokens IS NOT NULL` selects every measured file-op row rather
/// than only the lossy ones. That narrower population would bias the delivered
/// series upward by dropping exactly the raw-served rows that saved nothing.
///
/// Byte cost never fails, so it is `Some` on both arms; tokenisation can, and
/// when it does the row records "not measured" instead of a zero it did not
/// measure.
///
/// # The `Some(n)` arm can drop a COST-bearing row from the series
///
/// The reasoning above covers one direction only. The other has the SAME sign.
/// `EmittedNotice::tokens` is `count_tokens(..).ok()`, so on tokeniser failure
/// this arm yields `(None, Some(bytes))` — and that shape is reachable ONLY on a
/// row that did emit a disclosure. `query_summary` selects the delivered series
/// on `notice_tokens IS NOT NULL`, so precisely those rows leave it. What leaves
/// is a COST, never a saving, so the total it leaves behind is biased UPWARD —
/// the favourable direction, which is the direction that argues for shipping
/// (PF-036 resolution C).
///
/// The row is not lost: it is written to the database in full, `notice_bytes`
/// included. It is the SERIES it leaves. Manufacturing a zero to keep it would
/// be worse — that is a measurement nobody took — so the NULL stays and the
/// departure is counted instead, in
/// [`DeliveredSavings::unmeasured_notice_rows`].
fn file_op_notice_cost(
    notice: Option<&crate::output::EmittedNotice>,
    measured: bool,
) -> (Option<usize>, Option<usize>) {
    if !measured {
        return (None, None);
    }
    match notice {
        None => (Some(0), Some(0)),
        Some(n) => (n.tokens(), Some(n.bytes())),
    }
}

/// Record file-op analytics for one or more files, off the main thread.
///
/// - Captures `project_path` from the main thread before the spawn.
/// - Tokenizes in PARALLEL (rayon) when counts are not yet known.
/// - Persists SERIALLY to avoid SQLite write contention.
/// - Uses `register_thread` so `flush_pending()` joins before exit (fixes PF-001).
///
/// Returns immediately (non-blocking).  When `enabled` is false or `rows` is
/// empty, returns without spawning any thread.
pub(crate) fn record_file_ops(enabled: bool, rows: Vec<FileOpRow>, common: FileOpCommon) {
    if !enabled || rows.is_empty() {
        return;
    }
    register_thread(std::thread::spawn(move || {
        let ts = now_unix_secs();
        // Resolve counts in parallel; filter out rows where tokenization fails.
        let records: Vec<TokenSavingsRecord> = rows
            .into_par_iter()
            .filter_map(|r| {
                let (raw, comp) = match r.counts {
                    FileCounts::Known { raw, compressed } => (raw, compressed),
                    FileCounts::Tokenize { raw, compressed } => {
                        let text = match raw {
                            // Best-effort: skip row on read or UTF-8 error.
                            // This also naturally rejects TOCTOU-grown files (size guard
                            // in read_source rejects anything over the 50 MB limit).
                            RawSource::Reread(p) => crate::process::read_source(&p).ok()?,
                            RawSource::Inline(s) => s,
                        };
                        let raw_tok = count_tokens_bounded(&text);
                        let comp_tok = count_tokens_bounded(&compressed);
                        (raw_tok, comp_tok)
                    }
                };
                // Where a cost is measured at all, it is measured off the very
                // String the emitter wrote to stderr, so it is the emitted cost
                // rather than a second estimate of it — and it is tokenised
                // HERE, on the background thread, keeping BPE off the main
                // path. A batch row is not measured and yields two NULLs; see
                // `FileOpRow::notice_measured`.
                let (notice_tokens, notice_bytes) =
                    file_op_notice_cost(r.notice.as_ref(), r.notice_measured);
                let delivery = Delivery {
                    notice_tokens,
                    notice_bytes,
                    served: r.served,
                };
                Some(TokenSavingsRecord {
                    timestamp: ts,
                    command_type: CommandType::File,
                    original_cmd: r.original_cmd,
                    raw_tokens: raw,
                    compressed_tokens: comp,
                    // Unchanged: the continuity series keeps its definition.
                    // The disclosure is recorded in `delivery`, never folded
                    // into `compressed_tokens`.
                    savings_pct: savings_percentage(raw, comp),
                    duration_ms: 0,
                    project_path: common.project_path.clone(),
                    mode: common.mode.clone(),
                    language: r.language,
                    parse_tier: r.parse_tier,
                    session_id: common.session_id.clone(),
                    delivery,
                })
            })
            .collect();
        // Open DB once for all rows. The discard mirrors `persist_record` — and
        // so does its diagnostic: this is the path the `file` cohort's rows come
        // through, so leaving it silent would leave the schema hazards in
        // `super::schema` unobservable on the only path most machines exercise.
        match AnalyticsDb::open_default() {
            Ok(db) => {
                let total = records.len();
                let mut failed = 0usize;
                let mut first_err = None;
                for rec in &records {
                    if let Err(e) = db.record(rec) {
                        failed += 1;
                        if first_err.is_none() {
                            first_err = Some(e);
                        }
                    }
                }
                // ONE line for the whole batch, not one per row: the failures
                // this exists to surface are schema-level, so they fail every
                // row of every batch, and N identical lines would bury whatever
                // the reader was actually debugging.
                if let Some(e) = first_err {
                    crate::debug_log!(
                        "[skim:debug] analytics: {failed} of {total} file-op records discarded; first error: {e}"
                    );
                }
                if total > 0 {
                    db.maybe_prune();
                }
            }
            Err(e) => crate::debug_log!("[skim:debug] analytics: open_default failed: {e}"),
        }
    }));
}

// ============================================================================
// Convenience helpers for subcommand call sites
// ============================================================================

/// Record command output analytics with enabled-check and cwd detection.
///
/// Reduces the 12-15 line inline pattern at each subcommand call site to a
/// single function call. Token counting is deferred to a background thread.
///
/// `rec` bundles the recording control fields (enabled, command_type,
/// parse_tier, session_id) so this function stays under the
/// `clippy::too_many_arguments` threshold.
pub(crate) fn try_record_command(
    rec: RecordingContext<'_>,
    raw_text: String,
    compressed_text: String,
    original_cmd: String,
    duration: Duration,
) {
    if !rec.enabled {
        return;
    }
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .display()
        .to_string();
    record_fire_and_forget(
        raw_text,
        compressed_text,
        original_cmd,
        duration,
        FireAndForgetParams {
            command_type: rec.command_type,
            project_path: cwd,
            parse_tier: rec.parse_tier.map(str::to_string),
            session_id: rec.session_id.map(str::to_string),
        },
    );
}

/// Record command output analytics when token counts are already known.
///
/// Use this instead of [`try_record_command`] when the caller has already
/// computed token counts (e.g., via `--show-stats`), avoiding redundant
/// re-tokenization in the background thread.
///
/// Delegates to [`record_with_counts`] after resolving cwd and building
/// the record.
///
/// `rec` bundles the recording control fields (enabled, command_type,
/// parse_tier, session_id) so this function stays under the
/// `clippy::too_many_arguments` threshold.
pub(crate) fn try_record_command_with_counts(
    rec: RecordingContext<'_>,
    raw_tokens: usize,
    compressed_tokens: usize,
    original_cmd: String,
    duration: Duration,
) {
    if !rec.enabled {
        return;
    }
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .display()
        .to_string();
    record_with_counts(
        true,
        TokenSavingsRecord {
            timestamp: now_unix_secs(),
            command_type: rec.command_type,
            original_cmd,
            raw_tokens,
            compressed_tokens,
            savings_pct: savings_percentage(raw_tokens, compressed_tokens),
            duration_ms: duration.as_millis() as u64,
            project_path: cwd,
            mode: None,
            language: None,
            parse_tier: rec.parse_tier.map(str::to_string),
            session_id: rec.session_id.map(str::to_string),
            delivery: Delivery::default(),
        },
    );
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    /// Create a test database backed by a temporary file.
    ///
    /// Returns both the `AnalyticsDb` and the `NamedTempFile` handle. The
    /// caller must keep the `NamedTempFile` alive for the duration of the
    /// test -- dropping it deletes the underlying file, which would
    /// invalidate the database connection.
    fn test_db() -> (AnalyticsDb, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        let db = AnalyticsDb::open(tmp.path()).unwrap();
        (db, tmp)
    }

    fn sample_record() -> TokenSavingsRecord {
        TokenSavingsRecord {
            timestamp: 1711300000,
            command_type: CommandType::File,
            original_cmd: "skim src/main.rs".to_string(),
            raw_tokens: 1000,
            compressed_tokens: 200,
            savings_pct: 80.0,
            duration_ms: 15,
            project_path: "/tmp/test".to_string(),
            mode: Some("structure".to_string()),
            language: Some("rust".to_string()),
            parse_tier: None,
            session_id: None,
            delivery: Delivery::default(),
        }
    }

    #[test]
    fn test_open_creates_tables() {
        let (db, _tmp) = test_db();
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM token_savings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_record_and_query_summary() {
        let (db, _tmp) = test_db();
        db.record(&sample_record()).unwrap();

        let summary = db.query_summary(None).unwrap();
        assert_eq!(summary.invocations, 1);
        assert_eq!(summary.raw_tokens, 1000);
        assert_eq!(summary.compressed_tokens, 200);
        assert_eq!(summary.tokens_saved, 800);
    }

    #[test]
    fn test_daily_breakdown_groups_correctly() {
        let (db, _tmp) = test_db();
        // Two records on same day
        let mut r1 = sample_record();
        r1.timestamp = 1711300000;
        db.record(&r1).unwrap();

        let mut r2 = sample_record();
        r2.timestamp = 1711300100;
        db.record(&r2).unwrap();

        // One record on different day
        let mut r3 = sample_record();
        r3.timestamp = 1711300000 + 86400;
        db.record(&r3).unwrap();

        let daily = db.query_daily(None).unwrap();
        assert_eq!(daily.len(), 2);
    }

    #[test]
    fn test_command_breakdown() {
        let (db, _tmp) = test_db();
        let mut r1 = sample_record();
        r1.command_type = CommandType::File;
        db.record(&r1).unwrap();

        let mut r2 = sample_record();
        r2.command_type = CommandType::Test;
        db.record(&r2).unwrap();

        let by_cmd = db.query_by_command(None).unwrap();
        assert_eq!(by_cmd.len(), 2);
    }

    #[test]
    fn test_prune_removes_old_records() {
        let (db, _tmp) = test_db();
        // Record from 100 days ago
        let mut r = sample_record();
        r.timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - (100 * 86400);
        db.record(&r).unwrap();

        // Record from today
        let mut r2 = sample_record();
        r2.timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        db.record(&r2).unwrap();

        let pruned = db.prune_older_than(90).unwrap();
        assert_eq!(pruned, 1);

        let summary = db.query_summary(None).unwrap();
        assert_eq!(summary.invocations, 1);
    }

    #[test]
    fn test_wal_mode_enabled() {
        let (db, _tmp) = test_db();
        let mode: String = db
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn test_clear_deletes_all() {
        let (db, _tmp) = test_db();
        db.record(&sample_record()).unwrap();
        db.record(&sample_record()).unwrap();
        db.clear().unwrap();
        let summary = db.query_summary(None).unwrap();
        assert_eq!(summary.invocations, 0);
    }

    #[test]
    fn test_language_breakdown() {
        let (db, _tmp) = test_db();
        let mut r1 = sample_record();
        r1.language = Some("rust".to_string());
        db.record(&r1).unwrap();

        let mut r2 = sample_record();
        r2.language = Some("typescript".to_string());
        db.record(&r2).unwrap();

        let by_lang = db.query_by_language(None).unwrap();
        assert_eq!(by_lang.len(), 2);
    }

    #[test]
    fn test_mode_breakdown() {
        let (db, _tmp) = test_db();
        let mut r1 = sample_record();
        r1.mode = Some("structure".to_string());
        db.record(&r1).unwrap();

        let mut r2 = sample_record();
        r2.mode = Some("signatures".to_string());
        db.record(&r2).unwrap();

        let by_mode = db.query_by_mode(None).unwrap();
        assert_eq!(by_mode.len(), 2);
    }

    #[test]
    fn test_tier_distribution() {
        let (db, _tmp) = test_db();
        for tier in &["full", "full", "full", "degraded", "passthrough"] {
            let mut r = sample_record();
            r.parse_tier = Some(tier.to_string());
            r.mode = None;
            r.language = None;
            db.record(&r).unwrap();
        }
        let dist = db.query_tier_distribution(None).unwrap();
        assert!((dist.full_pct - 60.0).abs() < 0.1);
        assert!((dist.degraded_pct - 20.0).abs() < 0.1);
        assert!((dist.passthrough_pct - 20.0).abs() < 0.1);
    }

    #[test]
    fn test_pricing_default() {
        let p = PricingModel::default_pricing();
        assert_eq!(p.input_cost_per_mtok, 3.0);
        assert_eq!(p.tier_name, "Standard");
    }

    #[test]
    fn test_estimate_calculation() {
        let p = PricingModel::default_pricing();
        let savings = p.estimate_savings(1_000_000);
        assert!((savings - 3.0).abs() < 0.001);
    }

    #[test]
    fn test_since_filter() {
        let (db, _tmp) = test_db();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let mut old = sample_record();
        old.timestamp = now - 86400 * 10; // 10 days ago
        db.record(&old).unwrap();

        let mut recent = sample_record();
        recent.timestamp = now - 3600; // 1 hour ago
        db.record(&recent).unwrap();

        let summary = db.query_summary(Some(now - 86400)).unwrap();
        assert_eq!(summary.invocations, 1);
    }

    // ========================================================================
    // AnalyticsConfig::parse_disable_value tests
    //
    // Tests call the production parse_disable_value function directly.
    // No env mutation, no struct construction boilerplate.
    // ========================================================================

    #[test]
    fn test_analytics_enabled_with_empty_string() {
        assert!(!AnalyticsConfig::parse_disable_value(""));
    }

    #[test]
    fn test_analytics_disabled_with_value_1() {
        assert!(AnalyticsConfig::parse_disable_value("1"));
    }

    #[test]
    fn test_analytics_disabled_with_value_true() {
        assert!(AnalyticsConfig::parse_disable_value("true"));
    }

    #[test]
    fn test_analytics_disabled_with_value_yes() {
        assert!(AnalyticsConfig::parse_disable_value("yes"));
    }

    #[test]
    fn test_analytics_disabled_case_insensitive() {
        assert!(AnalyticsConfig::parse_disable_value("TRUE"));
    }

    #[test]
    fn test_analytics_enabled_with_value_0() {
        assert!(!AnalyticsConfig::parse_disable_value("0"));
    }

    #[test]
    fn test_analytics_enabled_with_value_false() {
        assert!(!AnalyticsConfig::parse_disable_value("false"));
    }

    #[test]
    fn test_analytics_enabled_with_value_no() {
        assert!(!AnalyticsConfig::parse_disable_value("no"));
    }

    // ========================================================================
    // Pricing validation tests — use from_cost_override (pure fn, no env reads)
    // ========================================================================

    #[test]
    fn test_pricing_negative_falls_back_to_default() {
        // negative cost: parse would yield -5.0, which fails the >= 0.0 guard
        let p = PricingModel::from_cost_override(Some(-5.0));
        assert_eq!(
            p.input_cost_per_mtok, 3.0,
            "negative cost should fall back to default"
        );
        assert_eq!(p.tier_name, "Standard");
    }

    #[test]
    fn test_pricing_zero_is_valid() {
        let p = PricingModel::from_cost_override(Some(0.0));
        assert_eq!(p.input_cost_per_mtok, 0.0, "zero cost should be accepted");
        assert_eq!(p.tier_name, "Custom");
    }

    #[test]
    fn test_pricing_infinity_falls_back_to_default() {
        let p = PricingModel::from_cost_override(Some(f64::INFINITY));
        assert_eq!(
            p.input_cost_per_mtok, 3.0,
            "infinite cost should fall back to default"
        );
        assert_eq!(p.tier_name, "Standard");
    }

    #[test]
    fn test_pricing_nan_falls_back_to_default() {
        let p = PricingModel::from_cost_override(Some(f64::NAN));
        assert_eq!(
            p.input_cost_per_mtok, 3.0,
            "NaN cost should fall back to default"
        );
        assert_eq!(p.tier_name, "Standard");
    }

    // ========================================================================
    // Schema migration v2 test
    // ========================================================================

    #[test]
    fn test_analytics_meta_table_created_by_migration() {
        let (db, _tmp) = test_db();
        // analytics_meta should exist from the v2 migration
        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='analytics_meta'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "analytics_meta table should be created by migration"
        );
    }

    // ========================================================================
    // original_cmd truncation test
    // ========================================================================

    #[test]
    fn test_record_truncates_long_original_cmd() {
        let (db, _tmp) = test_db();
        let mut r = sample_record();
        r.original_cmd = "x".repeat(1000);
        db.record(&r).unwrap();

        let stored: String = db
            .conn
            .query_row("SELECT original_cmd FROM token_savings", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            stored.len(),
            AnalyticsDb::MAX_CMD_LEN,
            "original_cmd should be truncated to {} chars",
            AnalyticsDb::MAX_CMD_LEN
        );
    }

    #[test]
    fn test_record_truncates_multibyte_utf8_original_cmd_at_char_boundary() {
        // Build a string whose byte length exceeds MAX_CMD_LEN but where the
        // byte at MAX_CMD_LEN falls in the middle of a multi-byte character.
        // "é" is U+00E9, encoded as two bytes [0xC3, 0xA9] in UTF-8.
        // Fill up to just before MAX_CMD_LEN with ASCII, then append "é"s so
        // that a byte-index truncation at MAX_CMD_LEN would land inside one.
        let ascii_prefix = "a".repeat(AnalyticsDb::MAX_CMD_LEN - 1);
        // The next 'é' straddles the boundary: byte 499 is 0xC3 (first byte of
        // the two-byte sequence), byte 500 would be 0xA9.  Slicing at 500
        // would previously panic; the fix must walk back to 499.
        let cmd = format!("{ascii_prefix}{}", "é".repeat(10));
        assert!(
            cmd.len() > AnalyticsDb::MAX_CMD_LEN,
            "test input must exceed MAX_CMD_LEN bytes"
        );
        assert!(
            !cmd.is_char_boundary(AnalyticsDb::MAX_CMD_LEN),
            "test input must have a char boundary violation at MAX_CMD_LEN"
        );

        let (db, _tmp) = test_db();
        let mut r = sample_record();
        r.original_cmd = cmd;
        // Must not panic (previously would panic with byte-index slice).
        db.record(&r).unwrap();

        let stored: String = db
            .conn
            .query_row("SELECT original_cmd FROM token_savings", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(
            stored.len() < AnalyticsDb::MAX_CMD_LEN,
            "truncation walked back to char boundary: stored {} bytes",
            stored.len()
        );
        assert!(
            std::str::from_utf8(stored.as_bytes()).is_ok(),
            "stored value must be valid UTF-8"
        );
    }

    // ========================================================================
    // DB file permissions test (Unix only)
    // ========================================================================

    #[cfg(unix)]
    #[test]
    fn test_db_file_permissions_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = NamedTempFile::new().unwrap();
        let _db = AnalyticsDb::open(tmp.path()).unwrap();

        let perms = std::fs::metadata(tmp.path()).unwrap().permissions();
        let mode = perms.mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "DB file should have 0600 permissions, got {:o}",
            mode
        );
    }

    // ========================================================================
    // savings_percentage underflow guard tests
    // ========================================================================

    #[test]
    fn test_clean_invalid_records() {
        let (db, _tmp) = test_db();
        // Insert a valid record
        db.record(&sample_record()).unwrap();
        // Insert an invalid record directly (compressed > raw)
        db.conn
            .execute(
                "INSERT INTO token_savings (timestamp, command_type, original_cmd, raw_tokens, compressed_tokens, savings_pct, duration_ms, project_path)
                 VALUES (1711300000, 'file', 'test', 10, 20, -100.0, 5, '/tmp')",
                [],
            )
            .unwrap();
        let cleaned = db.clean_invalid_records().unwrap();
        assert_eq!(cleaned, 1, "should remove exactly the 1 invalid record");
        let summary = db.query_summary(None).unwrap();
        assert_eq!(
            summary.invocations, 1,
            "only the valid record should remain"
        );
    }

    #[test]
    fn test_query_negative_savings_returns_zero() {
        let (db, _tmp) = test_db();
        // Insert a record where compressed > raw directly (simulates pre-fix corrupt data)
        db.conn
            .execute(
                "INSERT INTO token_savings (timestamp, command_type, original_cmd, raw_tokens, compressed_tokens, savings_pct, duration_ms, project_path)
                 VALUES (1711300000, 'file', 'test', 10, 20, -100.0, 5, '/tmp')",
                [],
            )
            .unwrap();
        let daily = db.query_daily(None).unwrap();
        assert_eq!(
            daily[0].tokens_saved, 0,
            "negative savings from corrupt DB should be clamped to 0"
        );
        let by_cmd = db.query_by_command(None).unwrap();
        assert_eq!(
            by_cmd[0].tokens_saved, 0,
            "negative savings in query_by_command should clamp to 0"
        );
    }

    #[test]
    fn test_negative_savings_clamped_at_recording() {
        // savings_percentage should return 0.0 when compressed >= raw
        assert_eq!(savings_percentage(10, 20), 0.0);
        assert_eq!(savings_percentage(0, 5), 0.0);
        assert_eq!(savings_percentage(10, 10), 0.0);
        // Normal case still works
        assert!(
            (savings_percentage(100, 20) - 80.0).abs() < 0.01,
            "expected ~80.0%"
        );
    }

    // ========================================================================
    // is_safe_session_id tests (F1, F6, F10)
    // ========================================================================

    /// F1: 128-char string is accepted; 129-char string is rejected.
    #[test]
    fn test_is_safe_session_id_max_length() {
        let at_limit = "a".repeat(128);
        assert!(
            is_safe_session_id(&at_limit),
            "128-char session_id should be accepted"
        );
        let over_limit = "a".repeat(129);
        assert!(
            !is_safe_session_id(&over_limit),
            "129-char session_id should be rejected"
        );
    }

    /// F1: empty string is rejected.
    #[test]
    fn test_is_safe_session_id_empty() {
        assert!(
            !is_safe_session_id(""),
            "empty session_id should be rejected"
        );
    }

    /// F1: shell metacharacters are rejected.
    #[test]
    fn test_is_safe_session_id_with_metacharacters() {
        assert!(
            !is_safe_session_id("foo;bar"),
            "semicolon should be rejected"
        );
        assert!(!is_safe_session_id("foo|bar"), "pipe should be rejected");
        assert!(!is_safe_session_id("foo bar"), "space should be rejected");
        assert!(
            !is_safe_session_id("$HOME"),
            "dollar sign should be rejected"
        );
    }

    /// F1: alphanumeric, hyphens, underscores, dots are accepted.
    #[test]
    fn test_is_safe_session_id_valid() {
        assert!(
            is_safe_session_id("abc-123_test.v2"),
            "alphanumeric, hyphen, underscore, dot should be accepted"
        );
        assert!(
            is_safe_session_id("session-2024-01-15_abc123"),
            "typical session ID format should be accepted"
        );
    }

    // ========================================================================
    // since_clause_with_extra helper test
    // ========================================================================

    #[test]
    fn test_since_clause_with_extra_no_since() {
        let (clause, params) = since_clause_with_extra(None, "language IS NOT NULL");
        assert_eq!(clause, "WHERE language IS NOT NULL");
        assert!(params.is_empty());
    }

    #[test]
    fn test_since_clause_with_extra_with_since() {
        let (clause, params) = since_clause_with_extra(Some(12345), "mode IS NOT NULL");
        assert_eq!(clause, "WHERE timestamp >= ?1 AND mode IS NOT NULL");
        assert_eq!(params, vec![12345]);
    }

    // ========================================================================
    // PricingModel tier tests
    // ========================================================================

    #[test]
    fn test_pricing_tiers() {
        let tiers = PricingModel::all_tiers();
        assert_eq!(tiers.len(), 4);
        assert_eq!(tiers[0].tier_name, "Economy");
        assert_eq!(tiers[0].input_cost_per_mtok, 1.0);
        assert_eq!(tiers[1].tier_name, "Standard");
        assert_eq!(tiers[1].input_cost_per_mtok, 3.0);
        assert_eq!(tiers[2].tier_name, "Advanced");
        assert_eq!(tiers[2].input_cost_per_mtok, 5.0);
        assert_eq!(tiers[3].tier_name, "Premium");
        assert_eq!(tiers[3].input_cost_per_mtok, 15.0);
    }

    #[test]
    fn test_pricing_default_is_standard() {
        let p = PricingModel::default_pricing();
        assert_eq!(p.tier_name, "Standard");
        assert_eq!(p.input_cost_per_mtok, 3.0);
    }

    // ========================================================================
    // Thread registry (PENDING_THREADS) tests
    //
    // PENDING_THREADS is a process-global static. Tests that touch it must run
    // serially to avoid interfering with each other. `REGISTRY_TEST_LOCK`
    // provides that serialization. Each test drains the registry on entry via
    // `drain_registry()` so leftover handles from a prior test cannot affect
    // the assertion counts.
    // ========================================================================

    /// Serializes tests that interact with the process-global PENDING_THREADS
    /// registry. Acquire this lock at the start of every registry test.
    static REGISTRY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Drain PENDING_THREADS and join all handles. Returns the number drained.
    ///
    /// Called at the start of each registry test to ensure a clean slate,
    /// and directly verified in the flush round-trip test.
    fn drain_registry() -> usize {
        let mut handles = PENDING_THREADS.lock().unwrap_or_else(|e| e.into_inner());
        let count = handles.len();
        for h in handles.drain(..) {
            let _ = h.join();
        }
        count
    }

    /// `flush_pending()` on an empty registry is a no-op and does not panic.
    #[test]
    fn test_flush_pending_empty_is_noop() {
        let _lock = REGISTRY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        drain_registry(); // ensure clean state

        flush_pending(); // must not panic
        // registry must still be empty afterwards
        let len = PENDING_THREADS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        assert_eq!(
            len, 0,
            "registry should be empty after flush on empty input"
        );
    }

    /// `register_thread` + `flush_pending` round-trip: registered handle is
    /// joined and the registry is empty afterwards.
    #[test]
    fn test_register_and_flush_round_trip() {
        let _lock = REGISTRY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        drain_registry();

        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let handle = std::thread::spawn(move || {
            // block until the main thread signals, so we can verify the handle
            // is actually registered before flush is called
            let _ = rx.recv();
        });
        register_thread(handle);

        // verify one handle is registered
        let len = PENDING_THREADS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        assert_eq!(
            len, 1,
            "one handle should be in registry after register_thread"
        );

        // unblock the spawned thread, then flush
        drop(tx);
        flush_pending();

        // registry must be empty after flush
        let len_after = PENDING_THREADS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        assert_eq!(len_after, 0, "registry should be empty after flush");
    }

    /// `flush_pending()` is idempotent: calling it twice in a row does not
    /// panic and leaves the registry empty both times.
    #[test]
    fn test_flush_pending_idempotent() {
        let _lock = REGISTRY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        drain_registry();

        // register a thread that completes immediately
        register_thread(std::thread::spawn(|| {}));
        flush_pending(); // first flush joins and drains
        flush_pending(); // second flush is a no-op on an already-empty registry

        let len = PENDING_THREADS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        assert_eq!(len, 0, "registry should remain empty after double flush");
    }

    /// `register_thread` recovers from a poisoned mutex without panicking.
    ///
    /// We cannot actually poison PENDING_THREADS in this process (it would
    /// affect every subsequent test), so we verify the recovery code path
    /// compiles and the normal path continues to work correctly.
    #[test]
    fn test_register_thread_normal_path_succeeds() {
        let _lock = REGISTRY_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        drain_registry();

        for _ in 0..3 {
            register_thread(std::thread::spawn(|| {}));
        }
        let len = PENDING_THREADS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        assert_eq!(len, 3, "all three handles should be registered");
        flush_pending(); // clean up
    }

    // ========================================================================
    // query_by_original_cmd tests
    // ========================================================================

    #[test]
    fn test_query_by_original_cmd_grouping() {
        let (db, _tmp) = test_db();

        // Record 3 invocations of "cargo build", 1 of "go test"
        for _ in 0..3 {
            let mut r = sample_record();
            r.original_cmd = "cargo build".to_string();
            r.raw_tokens = 1000;
            r.compressed_tokens = 100;
            db.record(&r).unwrap();
        }
        let mut r2 = sample_record();
        r2.original_cmd = "go test".to_string();
        r2.raw_tokens = 500;
        r2.compressed_tokens = 50;
        db.record(&r2).unwrap();

        let results = db.query_by_original_cmd(None).unwrap();
        assert_eq!(results.len(), 2, "should have 2 distinct commands");
        // "cargo build" has more tokens saved, should be first
        assert_eq!(results[0].original_cmd, "cargo build");
        assert_eq!(results[0].invocations, 3);
        assert_eq!(results[0].tokens_saved, 2700); // 3 * (1000 - 100)
        assert_eq!(results[1].original_cmd, "go test");
        assert_eq!(results[1].invocations, 1);
        assert_eq!(results[1].tokens_saved, 450); // 500 - 50
    }

    #[test]
    fn test_query_by_original_cmd_limited_to_15() {
        let (db, _tmp) = test_db();

        // Insert 20 distinct commands
        for i in 0..20_u64 {
            let mut r = sample_record();
            r.original_cmd = format!("cmd_{i:02}");
            r.raw_tokens = 1000;
            r.compressed_tokens = 100;
            db.record(&r).unwrap();
        }

        let results = db.query_by_original_cmd(None).unwrap();
        assert_eq!(results.len(), 15, "should be limited to top 15 results");
    }

    // ========================================================================
    // B8: Schema v3 — session_id column
    // ========================================================================

    /// AD-AN-4: verify the session_id column exists after migration.
    #[test]
    fn test_schema_v3_session_id_column_exists() {
        let (db, _tmp) = test_db();
        // PRAGMA table_info lists all columns; confirm session_id is present.
        let mut stmt = db.conn.prepare("PRAGMA table_info(token_savings)").unwrap();
        let col_names: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            col_names.contains(&"session_id".to_string()),
            "token_savings must have session_id column after v3 migration; found: {col_names:?}"
        );
    }

    /// AD-AN-4: verify the idx_ts_session_id index exists after migration.
    #[test]
    fn test_schema_v3_session_id_index_exists() {
        let (db, _tmp) = test_db();
        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='idx_ts_session_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "idx_ts_session_id index should be created by v3 migration"
        );
    }

    /// A freshly created database can store a delivered measurement.
    ///
    /// The subject is the property, not an integer: `test_db` goes through the
    /// real [`AnalyticsDb::open`] entry point — WAL, busy timeout, permissions,
    /// `run_migrations` — so this pins that a brand-new DB opened the way
    /// production opens one ends up with the delivered-cost columns actually
    /// present. `schema::tests` covers the same landing point against a bare
    /// in-memory connection; this covers it through the constructor callers
    /// actually use.
    ///
    /// It deliberately asserts NO `user_version`. This build claims no schema
    /// number for these columns — see the delivered-cost block in `schema.rs`
    /// for the collision with `ticket/305` and `ticket/306` that makes claiming
    /// one unsafe — and nothing downstream reads one: `query_summary` selects
    /// the delivered series on `notice_tokens IS NOT NULL`, row by row, and
    /// derives its window from MIN/MAX over that same predicate. Column
    /// presence is the property the series depends on, so column presence is
    /// what this pins.
    ///
    /// The `5` in `schema::tests::foreign_lineage_v5_db_gains_columns_and_keeps_its_version`
    /// is a different subject entirely — another lineage's rung, asserted to
    /// survive us untouched — and is unaffected by this.
    #[test]
    fn test_fresh_db_has_delivered_columns_through_constructor() {
        let (db, _tmp) = test_db();
        let mut stmt = db.conn.prepare("PRAGMA table_info(token_savings)").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for (name, _) in schema::DELIVERED_COST_COLUMNS {
            assert!(
                cols.iter().any(|c| c == name),
                "a freshly opened analytics DB must carry the delivered-cost \
                 column {name} after run_migrations; got {cols:?}"
            );
        }
    }

    // ========================================================================
    // B8: query_session_stats — tagged and untagged invocations
    // ========================================================================

    /// AD-AN-2: session_id is stored and retrievable.
    #[test]
    fn test_record_with_session_id_stored() {
        let (db, _tmp) = test_db();
        let mut r = sample_record();
        r.session_id = Some("session-abc-123".to_string());
        db.record(&r).unwrap();

        let stored: Option<String> = db
            .conn
            .query_row("SELECT session_id FROM token_savings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            stored,
            Some("session-abc-123".to_string()),
            "session_id should be stored and retrievable"
        );
    }

    /// AD-AN-2: NULL session_id stored for untagged records.
    #[test]
    fn test_record_without_session_id_stores_null() {
        let (db, _tmp) = test_db();
        let r = sample_record(); // session_id: None
        db.record(&r).unwrap();

        let stored: Option<String> = db
            .conn
            .query_row("SELECT session_id FROM token_savings", [], |row| row.get(0))
            .unwrap();
        assert!(
            stored.is_none(),
            "session_id should be NULL when not provided"
        );
    }

    /// AD-AN-2: query_session_stats returns correct counts for tagged sessions.
    #[test]
    fn test_query_session_stats_basic() {
        let (db, _tmp) = test_db();

        // 3 records tagged to "sess-1", 2 to "sess-2"
        for _ in 0..3 {
            let mut r = sample_record();
            r.session_id = Some("sess-1".to_string());
            r.raw_tokens = 1000;
            r.compressed_tokens = 200;
            db.record(&r).unwrap();
        }
        for _ in 0..2 {
            let mut r = sample_record();
            r.session_id = Some("sess-2".to_string());
            r.raw_tokens = 500;
            r.compressed_tokens = 100;
            db.record(&r).unwrap();
        }

        let stats = db.query_session_stats(None).unwrap();
        assert_eq!(
            stats.distinct_sessions, 2,
            "should count 2 distinct sessions"
        );
        // sess-1: 3 * (1000 - 200) = 2400; sess-2: 2 * (500 - 100) = 800; total = 3200
        assert_eq!(
            stats.total_tokens_saved, 3200,
            "total tokens saved should be 3200"
        );
        // avg per session: 3200 / 2 = 1600
        assert!(
            (stats.avg_tokens_per_session - 1600.0).abs() < 1.0,
            "avg_tokens_per_session should be ~1600.0, got {}",
            stats.avg_tokens_per_session
        );
        assert_eq!(
            stats.untagged_invocations, 0,
            "no untagged invocations expected"
        );
    }

    /// AD-AN-2: untagged invocations (NULL session_id) are counted separately
    /// and do NOT contribute to `total_tokens_saved`.
    #[test]
    fn test_query_session_stats_untagged() {
        let (db, _tmp) = test_db();

        // 1 tagged record: raw=1000, compressed=200 → savings=800
        let mut tagged = sample_record();
        tagged.session_id = Some("sess-x".to_string());
        tagged.raw_tokens = 1000;
        tagged.compressed_tokens = 200;
        db.record(&tagged).unwrap();

        // 3 untagged records (session_id: None) — must NOT inflate total_tokens_saved
        for _ in 0..3 {
            let r = sample_record(); // session_id: None, raw=1000, compressed=200
            db.record(&r).unwrap();
        }

        let stats = db.query_session_stats(None).unwrap();
        assert_eq!(
            stats.distinct_sessions, 1,
            "should count 1 distinct tagged session"
        );
        assert_eq!(
            stats.untagged_invocations, 3,
            "should count 3 untagged invocations"
        );
        // Untagged rows must be excluded from total_tokens_saved.
        // Only the single tagged record (savings = 800) should count.
        assert_eq!(
            stats.total_tokens_saved, 800,
            "total_tokens_saved must only count tagged rows, not untagged"
        );
    }

    /// AD-AN-2: empty DB returns zero-valued SessionStats (no panic).
    #[test]
    fn test_query_session_stats_empty_db() {
        let (db, _tmp) = test_db();
        let stats = db.query_session_stats(None).unwrap();
        assert_eq!(stats.distinct_sessions, 0);
        assert_eq!(stats.total_tokens_saved, 0);
        assert_eq!(stats.avg_tokens_per_session, 0.0);
        assert_eq!(stats.untagged_invocations, 0);
    }

    /// AD-AN-2: since filter applies to session_stats queries.
    #[test]
    fn test_query_session_stats_since_filter() {
        let (db, _tmp) = test_db();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Old tagged record (10 days ago)
        let mut old = sample_record();
        old.timestamp = now - 86400 * 10;
        old.session_id = Some("old-session".to_string());
        db.record(&old).unwrap();

        // Recent tagged record (1 hour ago)
        let mut recent = sample_record();
        recent.timestamp = now - 3600;
        recent.session_id = Some("new-session".to_string());
        db.record(&recent).unwrap();

        // Filter to last 24h: should see only new-session
        let stats = db.query_session_stats(Some(now - 86400)).unwrap();
        assert_eq!(
            stats.distinct_sessions, 1,
            "since filter should exclude old session"
        );
    }

    // ========================================================================
    // AnalyticsConfig::from_process tests (F5 step 3)
    // ========================================================================

    /// F5: from_process carries session_id when provided.
    #[test]
    fn test_from_process_passes_session_id() {
        let config = AnalyticsConfig::from_process(false, Some("my-session".to_string()));
        assert_eq!(
            config.session_id.as_deref(),
            Some("my-session"),
            "session_id should propagate through from_process"
        );
    }

    /// F5: from_process yields None session_id when not provided.
    #[test]
    fn test_from_process_none_session_id() {
        let config = AnalyticsConfig::from_process(false, None);
        assert!(
            config.session_id.is_none(),
            "session_id should be None when not provided"
        );
    }

    // ========================================================================
    // Gross/faithful expansion accounting tests (AC-N1..N3, AC-A2)
    // Part 3: compressed_tokens stored as TRUE count (no clamp); aggregates
    // floor expansion rows' contribution to tokens_saved=0.
    // ========================================================================

    /// AC-N1 (T6) — record via record_with_counts with comp_tokens > raw_tokens;
    /// read back and assert stored compressed_tokens equals the TRUE value (> raw),
    /// NOT the old clamped value.
    #[test]
    fn test_expansion_stored_as_true_count_not_clamped() {
        let (db, _tmp) = test_db();

        let raw = 100usize;
        let comp_expanded = 150usize; // expansion: comp > raw
        let record = TokenSavingsRecord {
            timestamp: 1711300000,
            command_type: CommandType::Build,
            original_cmd: "skim heatmap".to_string(),
            raw_tokens: raw,
            compressed_tokens: comp_expanded,
            savings_pct: savings_percentage(raw, comp_expanded),
            duration_ms: 20,
            project_path: "/tmp/test".to_string(),
            mode: None,
            language: None,
            parse_tier: None,
            session_id: None,
            delivery: Delivery::default(),
        };
        db.record(&record).unwrap();

        // Read back compressed_tokens directly from the DB.
        let stored: i64 = db
            .conn
            .query_row("SELECT compressed_tokens FROM token_savings", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            stored as usize, comp_expanded,
            "compressed_tokens must be stored as true count ({}), not clamped to raw ({})",
            comp_expanded, raw
        );
    }

    /// AC-N2/N3 (T7) — seed: compressing row + tie row + one expansion row.
    /// Assert query_summary, query_daily, and query_by_command all:
    ///   • floor the expansion row's contribution to 0 in tokens_saved
    ///   • agree on tokens_saved
    ///   • non-expanding subset's tokens_saved matches pre-change formula
    #[test]
    fn test_aggregates_floor_expansion_row_to_zero() {
        let (db, _tmp) = test_db();

        // Row 1: compressing — 500 raw, 200 compressed → 300 saved
        let mut r1 = sample_record();
        r1.timestamp = 1711300000;
        r1.raw_tokens = 500;
        r1.compressed_tokens = 200;
        r1.savings_pct = savings_percentage(500, 200);
        r1.command_type = CommandType::Build;
        db.record(&r1).unwrap();

        // Row 2: tie — 300 raw, 300 compressed → 0 saved
        let mut r2 = sample_record();
        r2.timestamp = 1711300100;
        r2.raw_tokens = 300;
        r2.compressed_tokens = 300;
        r2.savings_pct = savings_percentage(300, 300);
        r2.command_type = CommandType::Build;
        db.record(&r2).unwrap();

        // Row 3: expansion — 100 raw, 180 compressed → 0 saved (floored)
        let mut r3 = sample_record();
        r3.timestamp = 1711300200;
        r3.raw_tokens = 100;
        r3.compressed_tokens = 180;
        r3.savings_pct = savings_percentage(100, 180);
        r3.command_type = CommandType::Build;
        db.record(&r3).unwrap();

        // Expected: only row 1 contributes to tokens_saved = 300.
        let expected_tokens_saved = 300u64;

        let summary = db.query_summary(None).unwrap();
        assert_eq!(
            summary.tokens_saved, expected_tokens_saved,
            "query_summary: expansion row must contribute 0 to tokens_saved"
        );
        // True counts are preserved in raw_tokens/compressed_tokens.
        assert_eq!(summary.raw_tokens, 500 + 300 + 100);
        assert_eq!(summary.compressed_tokens, 200 + 300 + 180);

        let daily = db.query_daily(None).unwrap();
        assert_eq!(daily.len(), 1);
        assert_eq!(
            daily[0].tokens_saved, expected_tokens_saved,
            "query_daily: expansion row must contribute 0 to tokens_saved"
        );

        let by_cmd = db.query_by_command(None).unwrap();
        assert_eq!(by_cmd.len(), 1);
        assert_eq!(
            by_cmd[0].tokens_saved, expected_tokens_saved,
            "query_by_command: expansion row must contribute 0 to tokens_saved"
        );
    }

    /// AC-A2 (T8) — expansion-heavy dataset: every aggregate query returns
    /// tokens_saved >= 0 and does not panic.
    #[test]
    fn test_expansion_heavy_dataset_nonnegative_tokens_saved() {
        let (db, _tmp) = test_db();

        // Seed 10 expansion rows and 2 compressing rows.
        for i in 0..10u64 {
            let r = TokenSavingsRecord {
                timestamp: 1711300000 + i as i64,
                command_type: CommandType::Lint,
                original_cmd: format!("skim heatmap {i}"),
                raw_tokens: 50,
                compressed_tokens: 100, // expansion
                savings_pct: 0.0,
                duration_ms: 5,
                project_path: "/tmp/test".to_string(),
                mode: None,
                language: None,
                parse_tier: None,
                session_id: None,
                delivery: Delivery::default(),
            };
            db.record(&r).unwrap();
        }
        // Two normal compressing rows so we can verify non-zero tokens_saved for those.
        let mut r_good = sample_record();
        r_good.command_type = CommandType::Lint;
        r_good.raw_tokens = 1000;
        r_good.compressed_tokens = 100;
        r_good.savings_pct = savings_percentage(1000, 100);
        r_good.timestamp = 1711310000;
        db.record(&r_good).unwrap();
        db.record(&r_good).unwrap();

        let summary = db.query_summary(None).unwrap();
        // tokens_saved is u64 — always non-negative by type; assert exact value.
        // Only the two compressing rows contribute: 2 × (1000 - 100) = 1800.
        assert_eq!(summary.tokens_saved, 1800);

        // tokens_saved is u64 — non-negativity is guaranteed by the type.
        // Call both queries to ensure they do not panic with expansion-heavy data.
        db.query_daily(None).unwrap();
        db.query_by_command(None).unwrap();
    }

    // ========================================================================
    // Delivered cost, and the three series
    // ========================================================================

    /// The recorded cost is the EMITTED cost: both come from the same
    /// `EmittedNotice`, so this asserts the stored bytes against the very line
    /// `write_result_and_stats` would have written.
    #[test]
    fn delivered_cost_round_trips_from_the_emitted_line() {
        let (db, _tmp) = test_db();

        let notice = crate::output::emitted_notice_cost(Some("cat"), "structure", 1, 1)
            .expect("a differing view owes a marker");
        let mut r = sample_record();
        r.delivery = Delivery {
            notice_tokens: notice.tokens(),
            notice_bytes: Some(notice.bytes()),
            served: Some(Served::Transformed),
        };
        db.record(&r).unwrap();

        let (bytes, toks, served): (i64, i64, String) = db
            .conn
            .query_row(
                "SELECT notice_bytes, notice_tokens, served FROM token_savings",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();

        assert_eq!(bytes as usize, notice.line().len());
        assert_eq!(toks as usize, notice.tokens().unwrap());
        assert_eq!(served, "transformed");
    }

    /// NULL survives the round trip as NULL. A path that took no measurement
    /// must not become a row claiming it measured zero.
    #[test]
    fn unmeasured_delivery_stays_null() {
        let (db, _tmp) = test_db();
        db.record(&sample_record()).unwrap();

        let (bytes, toks, served): (Option<i64>, Option<i64>, Option<String>) = db
            .conn
            .query_row(
                "SELECT notice_bytes, notice_tokens, served FROM token_savings",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((bytes, toks, served), (None, None, None));
    }

    /// The clamp stays and the expansion it drops is now reported beside it.
    ///
    /// DISCRIMINATING: delete the `tokens_lost` column from `query_summary` and
    /// the 400 tokens of expansion become unrecoverable from the summary again
    /// — which is the whole defect, since `tokens_saved` alone reads 900 for a
    /// corpus whose true net is 500.
    #[test]
    fn expansion_is_reported_beside_the_clamped_headline() {
        let (db, _tmp) = test_db();

        let mut saving = sample_record();
        saving.raw_tokens = 1000;
        saving.compressed_tokens = 100;
        db.record(&saving).unwrap();

        let mut expanding = sample_record();
        expanding.timestamp += 1;
        expanding.raw_tokens = 100;
        expanding.compressed_tokens = 500;
        db.record(&expanding).unwrap();

        let s = db.query_summary(None).unwrap();
        assert_eq!(s.tokens_saved, 900, "continuity series keeps its clamp");
        assert_eq!(s.tokens_lost, 400, "and the clamp no longer hides");
        assert_eq!(
            s.tokens_saved - s.tokens_lost,
            500,
            "the true net must be recoverable from the summary alone"
        );
    }

    /// Both means are reported, each with the population it was taken over —
    /// and the narrow one covers only rows that COMPRESSED, not rows that
    /// merely changed.
    ///
    /// DISCRIMINATING, twice over. Widen the narrow mean's predicate back to
    /// `raw_tokens <> compressed_tokens` and it reads **32%** over 5 rows on
    /// this fixture, because `savings_pct` is stored already floored
    /// ([`savings_percentage`]) so each of the three expansions enters as a 0%
    /// SAVING rather than as the loss it was. Compression that actually ran
    /// averaged 80% over 2 rows. Separately, drop `expansion_invocations` and
    /// the excluded population becomes invisible: percentage space has no
    /// `tokens_lost` to fall back on, because the magnitude was destroyed at
    /// write time and only the COUNT survives.
    #[test]
    fn the_narrow_mean_excludes_expansions_and_counts_them() {
        let (db, _tmp) = test_db();

        for i in 0..2i64 {
            let mut noop = sample_record();
            noop.timestamp += i;
            noop.raw_tokens = 500;
            noop.compressed_tokens = 500; // served exactly what it was given
            noop.savings_pct = 0.0;
            db.record(&noop).unwrap();
        }
        for i in 2..4i64 {
            let mut compressed = sample_record();
            compressed.timestamp += i;
            compressed.raw_tokens = 1000;
            compressed.compressed_tokens = 200;
            compressed.savings_pct = 80.0;
            db.record(&compressed).unwrap();
        }
        for i in 4..6i64 {
            // An expansion: the view grew. `savings_percentage` floors this to
            // 0.0, which is exactly why it must not enter a mean about saving.
            let mut expansion = sample_record();
            expansion.timestamp += i;
            expansion.raw_tokens = 100;
            expansion.compressed_tokens = 300;
            expansion.savings_pct = savings_percentage(100, 300);
            db.record(&expansion).unwrap();
        }
        // A zero-raw expansion — nothing to compress, so no rate is definable.
        // PF-036 excludes these from rate math; the `raw_tokens > 0` clause in
        // `COMPRESSED_ROW` is what makes that hold here.
        let mut zero_raw = sample_record();
        zero_raw.timestamp += 6;
        zero_raw.raw_tokens = 0;
        zero_raw.compressed_tokens = 50;
        zero_raw.savings_pct = savings_percentage(0, 50);
        db.record(&zero_raw).unwrap();

        let s = db.query_summary(None).unwrap();
        assert_eq!(s.invocations, 7);
        // 160/7: the all-rows mean, deliberately unchanged (continuity series).
        assert!(
            (s.avg_savings_pct - 160.0 / 7.0).abs() < 1e-6,
            "the diluted mean keeps its definition; got {}",
            s.avg_savings_pct
        );
        assert_eq!(
            s.compressed_invocations, 2,
            "only the two rows where compression ran and paid"
        );
        assert!(
            (s.avg_savings_pct_compressed - 80.0).abs() < 1e-6,
            "the mean over rows that compressed; got {}",
            s.avg_savings_pct_compressed
        );
        assert_eq!(
            s.expansion_invocations, 3,
            "two expansions plus the zero-raw one; the mean excludes them, so \
             the renderer must be able to name them"
        );
        // Token space keeps its magnitude; percentage space keeps only the count.
        assert_eq!(s.tokens_saved, 1600);
        assert_eq!(s.tokens_lost, 450, "200 + 200 + 50");
    }

    /// The delivered series covers only disclosure-measured rows, and says so
    /// by carrying their window.
    ///
    /// DISCRIMINATING: widen the selection from `notice_tokens IS NOT NULL` to
    /// all rows and the unmeasured row both changes the value and back-dates
    /// the window onto history that was never measured this way.
    #[test]
    fn delivered_series_excludes_unmeasured_rows_and_carries_its_window() {
        let (db, _tmp) = test_db();

        // An unmeasured row: real savings, no delivered measurement.
        let mut old = sample_record();
        old.timestamp = 1_700_000_000; // 2023-11-14 UTC
        old.raw_tokens = 1000;
        old.compressed_tokens = 100;
        db.record(&old).unwrap();

        // A measured row whose disclosure eats most of the saving.
        let mut new = sample_record();
        new.timestamp = 1_711_300_000; // 2024-03-24 UTC
        new.raw_tokens = 130;
        new.compressed_tokens = 100;
        new.delivery = Delivery {
            notice_tokens: Some(25),
            notice_bytes: Some(90),
            served: Some(Served::Transformed),
        };
        db.record(&new).unwrap();

        let s = db.query_summary(None).unwrap();
        assert_eq!(s.tokens_saved, 930, "both rows feed the continuity series");
        assert_eq!(s.delivered.rows, 1, "only the measured row feeds delivered");
        assert_eq!(s.delivered.tokens, 5, "130 - 100 - 25");
        assert_eq!(s.delivered.notice_tokens, 25);
        assert_eq!(s.delivered.first_day.as_deref(), Some("2024-03-24"));
        assert_eq!(s.delivered.last_day.as_deref(), Some("2024-03-24"));
    }

    /// The delivered series is signed, because the disclosure can outweigh the
    /// transform — measured at 9.3%–10.7% of saving file rows on the author's
    /// corpus. Clamping it here would recreate, in the new series, exactly the
    /// blindness the old one had.
    #[test]
    fn delivered_series_keeps_a_negative_sign() {
        let (db, _tmp) = test_db();

        let mut r = sample_record();
        r.raw_tokens = 110;
        r.compressed_tokens = 100; // a 10-token saving
        r.delivery = Delivery {
            notice_tokens: Some(25), // bought with a 25-token disclosure
            notice_bytes: Some(90),
            served: Some(Served::Transformed),
        };
        db.record(&r).unwrap();

        let s = db.query_summary(None).unwrap();
        assert_eq!(s.tokens_saved, 10, "the old series still calls this a win");
        assert_eq!(
            s.delivered.tokens, -15,
            "delivered context grew; the sign must survive"
        );
    }

    /// Put `token_savings` into the shape the author's live database is
    /// actually in, by performing the rebuild that put it there.
    ///
    /// This is the PRODUCTION table shape, and it is deliberately NOT reachable
    /// through `run_migrations`: the v1 `CREATE TABLE` in [`schema`] declares
    /// `raw_tokens`/`compressed_tokens`/`savings_pct` `NOT NULL` and a fresh
    /// database still does — correctly, and this fixture must not be read as an
    /// argument for relaxing it. A foreign lineage's migration rebuilds the
    /// table (`CREATE` a replacement, copy by explicit column list, `DROP`,
    /// `RENAME` into place) and its `CREATE` omits those three constraints.
    /// `ALTER TABLE` has no form that restores a `NOT NULL`, so the next open
    /// re-adds the delivered-cost columns and leaves the operands nullable
    /// permanently. The rebuild, and why presence-gating does not survive it,
    /// are in the HAZARD banner in `schema::reconcile_additive_columns`; this
    /// reproduces its effect rather than restating its reasoning.
    ///
    /// Measured on `~/Library/Caches/skim/analytics.db` (2026-09-26, 69,879
    /// rows, `user_version` 5): `sqlite_master` holds
    /// `CREATE TABLE "token_savings"` — the quoted identifier SQLite writes
    /// when a table is renamed INTO place, i.e. the post-rebuild signature —
    /// carrying `provider`/`model`/`turn_id`/`upstream_error_status` from the
    /// foreign lineage with the three delivered-cost columns appended by a
    /// later `ALTER`, and `PRAGMA table_info` reporting `notnull = 0` for
    /// `raw_tokens`, `compressed_tokens` and `savings_pct`. The statements
    /// below reproduce that DDL.
    fn rebuild_token_savings_as_a_foreign_lineage_does(conn: &Connection) {
        // The copy names its columns explicitly instead of `SELECT *`, which is
        // exactly why losing the delivered-cost columns raises nothing here and
        // the rebuild commits clean.
        conn.execute_batch(
            "CREATE TABLE token_savings_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp INTEGER NOT NULL,
                command_type TEXT NOT NULL,
                original_cmd TEXT NOT NULL,
                raw_tokens INTEGER,
                compressed_tokens INTEGER,
                savings_pct REAL,
                duration_ms INTEGER NOT NULL,
                project_path TEXT NOT NULL,
                mode TEXT,
                language TEXT,
                parse_tier TEXT,
                session_id TEXT,
                provider TEXT,
                model TEXT,
                turn_id TEXT,
                upstream_error_status INTEGER
            );
            INSERT INTO token_savings_new
                (id, timestamp, command_type, original_cmd, raw_tokens,
                 compressed_tokens, savings_pct, duration_ms, project_path,
                 mode, language, parse_tier, session_id)
            SELECT id, timestamp, command_type, original_cmd, raw_tokens,
                   compressed_tokens, savings_pct, duration_ms, project_path,
                   mode, language, parse_tier, session_id
            FROM token_savings;
            DROP TABLE token_savings;
            ALTER TABLE token_savings_new RENAME TO token_savings;
            PRAGMA user_version = 5;",
        )
        .unwrap();

        // The next open. Presence-gating re-adds the three delivered-cost
        // columns the rebuild took with it; nothing re-adds a `NOT NULL`. All
        // three reappearing on an already-reconciled database is also what
        // `note_delivered_columns_reconciled` calls a RESET, so this fixture
        // leaves `delivered.reset_at` set — by design, and pinned by
        // `schema::tests::a_rebuild_that_drops_the_columns_leaves_a_reset_mark`
        // rather than re-asserted here.
        schema::run_migrations(conn).unwrap();

        // Column 1 is `name`, column 3 is `notnull` — the same positional
        // `PRAGMA table_info` idiom the rest of this module's tests use.
        let mut stmt = conn.prepare("PRAGMA table_info(token_savings)").unwrap();
        let raw_tokens_notnull: Option<i64> = stmt
            .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(3)?)))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .find(|(name, _)| name == "raw_tokens")
            .map(|(_, notnull)| notnull);
        assert_eq!(
            raw_tokens_notnull,
            Some(0),
            "the fixture must reproduce the live nullable operand — a NOT NULL \
             here (or an absent column) means the rebuild above stopped \
             matching the live DDL, and any test standing on it is exercising \
             the wrong table"
        );
    }

    /// Every column of the delivered series covers the same rows, including
    /// when an operand is NULL.
    ///
    /// The precondition is not hypothetical, and it is not reachable through
    /// the normal schema path either: a fresh `run_migrations` database still
    /// declares `raw_tokens` `NOT NULL` and would reject the row below, while
    /// the live table is a foreign rebuild that dropped that constraint and
    /// `ALTER TABLE` cannot restore it (ADR-020). That asymmetry is the whole
    /// finding, so the fixture builds the LIVE table shape — see
    /// [`rebuild_token_savings_as_a_foreign_lineage_does`] — rather than the
    /// fresh one. `TokenSavingsRecord` cannot express a NULL operand, so the
    /// row is written through the connection directly, which is precisely how
    /// a foreign lineage would write it.
    ///
    /// DISCRIMINATING: give the row count a plainer predicate than the token sum
    /// (`notice_tokens IS NOT NULL` alone, as it had) and this fixture reports
    /// `rows = 2` against a total computed from ONE row, `notice_tokens = 50`
    /// charged for a saving only one row contributed, and a window running
    /// 03-24..03-25 for a value covering only 03-24. Four statements over four
    /// different populations, no indicator on any of them.
    #[test]
    fn delivered_columns_cannot_cover_different_populations() {
        let (db, _tmp) = test_db();
        rebuild_token_savings_as_a_foreign_lineage_does(&db.conn);

        // Complete: 130 - 100 - 25 = 5.
        let mut whole = sample_record();
        whole.timestamp = 1_711_300_000; // 2024-03-24 UTC
        whole.raw_tokens = 130;
        whole.compressed_tokens = 100;
        whole.delivery = Delivery {
            notice_tokens: Some(25),
            notice_bytes: Some(90),
            served: Some(Served::Transformed),
        };
        db.record(&whole).unwrap();

        // A disclosure measured against a NULL operand: the arithmetic cannot
        // be evaluated, so the row belongs to NO delivered column.
        db.conn
            .execute(
                "INSERT INTO token_savings (timestamp, command_type, original_cmd, raw_tokens, compressed_tokens, savings_pct, duration_ms, project_path, notice_tokens, notice_bytes, served)
                 VALUES (1711400000, 'file', 'skim x.rs', NULL, 100, NULL, 0, '/tmp/test', 25, 90, 'transformed')",
                [],
            )
            .unwrap();

        let s = db.query_summary(None).unwrap();
        assert_eq!(
            s.delivered.rows, 1,
            "a row the total cannot include must not be counted for it"
        );
        assert_eq!(s.delivered.tokens, 5, "130 - 100 - 25, from the whole row");
        assert_eq!(
            s.delivered.notice_tokens, 25,
            "the disclosure cost is charged over exactly `rows`"
        );
        assert_eq!(s.delivered.first_day.as_deref(), Some("2024-03-24"));
        assert_eq!(
            s.delivered.last_day.as_deref(),
            Some("2024-03-24"),
            "the window must not extend past the rows in the value"
        );
    }

    /// A disclosure whose token cost could not be measured leaves the series,
    /// and the departure is counted rather than silent.
    ///
    /// `notice_bytes IS NOT NULL AND notice_tokens IS NULL` means a notice WAS
    /// emitted and `EmittedNotice::tokens` returned `None`. The row keeps its
    /// place in the database and loses its place in the series, taking a COST
    /// with it — so the total left behind is biased in the favourable direction
    /// (PF-036 resolution C).
    ///
    /// DISCRIMINATING: drop the counter and this fixture is byte-identical to a
    /// run where no such row existed. The bias becomes unobservable, and it is
    /// the kind that argues for shipping.
    #[test]
    fn an_untokenisable_disclosure_leaves_the_series_and_says_so() {
        let (db, _tmp) = test_db();

        let mut measured = sample_record();
        measured.timestamp = 1_711_300_000;
        measured.raw_tokens = 130;
        measured.compressed_tokens = 100;
        measured.delivery = Delivery {
            notice_tokens: Some(25),
            notice_bytes: Some(90),
            served: Some(Served::Transformed),
        };
        db.record(&measured).unwrap();

        // Emitted, counted in bytes, never tokenised — the one shape
        // `file_op_notice_cost` leaves `None`.
        let mut untokenisable = sample_record();
        untokenisable.timestamp = 1_711_300_001;
        untokenisable.raw_tokens = 200;
        untokenisable.compressed_tokens = 100;
        untokenisable.delivery = Delivery {
            notice_tokens: None,
            notice_bytes: Some(90),
            served: Some(Served::Transformed),
        };
        db.record(&untokenisable).unwrap();

        let s = db.query_summary(None).unwrap();
        assert_eq!(
            s.delivered.rows, 1,
            "an unmeasured cost must not be charged as a measured one"
        );
        assert_eq!(s.delivered.tokens, 5, "only the measured row");
        assert_eq!(
            s.delivered.unmeasured_notice_rows, 1,
            "the row that left the series carrying a cost must be countable"
        );
        // Both rows still feed the continuity series: 30 + 100.
        assert_eq!(s.tokens_saved, 130, "the row is in the DB, not the series");
    }

    /// Within a MEASURED regime, a row with no marker is charged a measured
    /// zero, not a NULL.
    ///
    /// DISCRIMINATING: return `(None, None)` on the `None` arm and every
    /// raw-served row drops out of `notice_tokens IS NOT NULL`, leaving the
    /// delivered series computed over lossy rows only — biased upward by
    /// exactly the rows that saved nothing.
    #[test]
    fn absent_marker_is_a_measured_zero_not_an_unknown() {
        assert_eq!(file_op_notice_cost(None, true), (Some(0), Some(0)));
    }

    /// A batch row records NULL in BOTH delivered-cost columns, and the second
    /// NULL is as load-bearing as the first.
    ///
    /// A multi-file run emits ONE run-scoped marker and `token_savings` has no
    /// invocation dimension to charge it to, so there is no per-file number to
    /// record and the row says so rather than inventing one. The previous
    /// encoding charged the whole run to the first differing row and gave the
    /// other N-1 a MEASURED ZERO.
    ///
    /// DISCRIMINATING, and in two independent directions:
    ///
    /// - return `(Some(0), Some(0))` instead and the batch's real disclosure
    ///   cost is deleted from the delivered series, biasing the headline UP
    ///   (PF-036's favourable direction);
    /// - return `(None, Some(0))` — NULL the tokens only — and every batch row
    ///   satisfies `notice_bytes IS NOT NULL AND notice_tokens IS NULL`, which
    ///   is the `delivered_unmeasured_rows` counter. A statistic that exists to
    ///   count untokenisable DISCLOSURES would silently fill with rows that
    ///   emitted nothing per-file at all.
    #[test]
    fn an_unmeasured_regime_records_null_in_both_columns() {
        assert_eq!(file_op_notice_cost(None, false), (None, None));

        // And a carried marker cannot override the regime: `measured` is the
        // outer decision, so a row that somehow still held an `EmittedNotice`
        // would not be charged for it either.
        let notice =
            crate::output::emitted_notice_cost(Some("cat"), "structure", 3, 3).expect("marker");
        assert_eq!(file_op_notice_cost(Some(&notice), false), (None, None));
    }

    /// A batch charges its run-scoped marker to NO row, rather than to one
    /// arbitrary row.
    ///
    /// Costs three rows the way `multi.rs` now does — every row unmeasured —
    /// and asserts the batch contributes nothing to either delivered bucket.
    /// The old shape summed to exactly ONE emission, which was arithmetically
    /// right and per-file wrong: the run total was correct while one arbitrary
    /// file carried all of it and the rest carried zeros they never earned.
    ///
    /// No database: `record_file_ops` writes through `AnalyticsDb::open_default`,
    /// which targets the developer's real analytics DB unless the environment
    /// is redirected, so this is verified on the pure costing function instead.
    ///
    /// DISCRIMINATING: restore the `Option::take` attribution and the first
    /// element stops being `(None, None)`.
    #[test]
    fn a_batch_charges_its_marker_to_no_row() {
        let charged: Vec<(Option<usize>, Option<usize>)> =
            (0..3).map(|_| file_op_notice_cost(None, false)).collect();

        assert_eq!(charged.len(), 3);
        assert!(
            charged.iter().all(|c| *c == (None, None)),
            "no row of a batch may carry a per-file cost the run never emitted \
             per file; got: {charged:?}"
        );
    }

    /// The three `served` buckets partition the delivered population, and the
    /// partition survives a spelling this build does not know.
    ///
    /// `served` is a bare `TEXT` column on a table three lineages write to,
    /// with no `CHECK` anywhere, so an unrecognised value is not hypothetical
    /// in the way an impossible one would be.
    ///
    /// DISCRIMINATING: key the third bucket on `served IS NULL` instead of the
    /// NULL-safe complement and the `'proxied'` row falls into no bucket at
    /// all — the counts read `1 + 1 + 1` against 4 delivered rows, and a reader
    /// comparing them to the total sees a shortfall with no cause attached.
    #[test]
    fn the_served_buckets_partition_the_delivered_population() {
        let (db, _tmp) = test_db();

        for (i, served) in ["raw", "transformed", "proxied"].into_iter().enumerate() {
            let mut r = sample_record();
            r.timestamp = 1_711_300_000 + i as i64;
            r.raw_tokens = 100;
            r.compressed_tokens = 60;
            r.delivery = Delivery {
                notice_tokens: Some(5),
                notice_bytes: Some(20),
                served: None,
            };
            db.record(&r).unwrap();
            // `Served` cannot spell `proxied`, which is the point: the column
            // can hold it and only SQL decides what happens next.
            db.conn
                .execute(
                    "UPDATE token_savings SET served = ?1 WHERE timestamp = ?2",
                    rusqlite::params![served, r.timestamp],
                )
                .unwrap();
        }

        // A fourth delivered row that legitimately recorded no decision — the
        // cache-hit / `Mode::Full` case.
        let mut undecided = sample_record();
        undecided.timestamp = 1_711_300_010;
        undecided.raw_tokens = 100;
        undecided.compressed_tokens = 60;
        undecided.delivery = Delivery {
            notice_tokens: Some(5),
            notice_bytes: Some(20),
            served: None,
        };
        db.record(&undecided).unwrap();

        let d = db.query_summary(None).unwrap().delivered;
        assert_eq!(d.rows, 4);
        assert_eq!(d.served_raw, 1);
        assert_eq!(d.served_transformed, 1);
        assert_eq!(
            d.served_other, 2,
            "NULL and an unrecognised spelling both land here — neither may \
             fall out of the partition"
        );
        assert_eq!(
            d.served_raw + d.served_transformed + d.served_other,
            d.rows,
            "the three buckets must always sum back to the population they are \
             taken over"
        );
    }

    /// An unreset series reports no reset, on a database that has one and on a
    /// database that has no `analytics_meta` at all.
    ///
    /// The second half is the one that matters: `analytics_meta` is created
    /// lazily, and a foreign-lineage database sitting above skim's ladder skips
    /// the rung that would create it. SQLite answers a SELECT against a missing
    /// table with an error, not with zero rows.
    ///
    /// DISCRIMINATING: drop the `unwrap_or(None)` tolerance and the second half
    /// panics instead of reporting `None` — in production it would take the
    /// whole 90-day dashboard down to withhold one disclosure line.
    #[test]
    fn an_unreset_series_reports_no_reset_even_without_the_meta_table() {
        let (db, _tmp) = test_db();
        assert_eq!(db.query_summary(None).unwrap().delivered.reset_at, None);

        db.conn.execute_batch("DROP TABLE analytics_meta").unwrap();
        assert_eq!(
            db.delivered_series_reset_at(),
            None,
            "a missing `analytics_meta` is an absent mark, never an error"
        );
        assert_eq!(
            db.query_summary(None).unwrap().delivered.reset_at,
            None,
            "and the summary still renders"
        );
    }

    /// A reset recorded in `analytics_meta` reaches `DeliveredSavings`.
    ///
    /// Written through the same key const the writer uses, so the two halves
    /// cannot drift to different spellings.
    ///
    /// DISCRIMINATING, and this is the whole point of the key choice: the
    /// sibling `delivered_series_opened_at` is written on the FIRST reconcile
    /// of ANY database, so a reader keying on it would report a reset here —
    /// where none happened — and on every fresh install. Measured on the
    /// author's live database (69,742 rows, zero delivered), `opened_at` is
    /// exactly what the next open writes and `reset_at` is exactly what it does
    /// not.
    #[test]
    fn a_recorded_reset_reaches_the_summary_and_opened_at_does_not() {
        let (db, _tmp) = test_db();

        db.conn
            .execute(
                "INSERT OR REPLACE INTO analytics_meta (key, value) VALUES ('delivered_series_opened_at', ?1)",
                [1_700_000_000_i64],
            )
            .unwrap();
        assert_eq!(
            db.query_summary(None).unwrap().delivered.reset_at,
            None,
            "an opened series is not a reset one — this is the normal state of \
             every fresh install and of the author's live database"
        );

        db.conn
            .execute(
                "INSERT OR REPLACE INTO analytics_meta (key, value) VALUES (?1, ?2)",
                rusqlite::params![schema::DELIVERED_SERIES_RESET_AT, 1_790_000_000_i64],
            )
            .unwrap();
        assert_eq!(
            db.query_summary(None).unwrap().delivered.reset_at,
            Some(1_790_000_000),
        );
    }
}
