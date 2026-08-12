//! Token analytics persistence layer.
//!
//! Records token savings from every skim invocation into a local SQLite
//! database (`~/.cache/skim/analytics.db`) and provides query functions
//! for the `skim stats` dashboard.
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
//! - **Versioned schema migrations** in [`schema`] -- each migration is
//!   idempotent and guarded by a `user_version` PRAGMA check.

mod schema;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
// Arc, AtomicUsize, and Ordering are only used in the proxy-gated
// ChannelAlignmentRecorder and its test helpers (AD-CA-9).
#[cfg(feature = "proxy")]
use std::sync::Arc;
#[cfg(feature = "proxy")]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rayon::prelude::*;
use rusqlite::Connection;
use serde::Serialize;

use crate::tokens;
use rskim_tokens::{Encoding, encoding_for_model};

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
    /// Proxy request (L3 compression proxy — recorded by `record_proxy`, not `record`).
    /// AD-AN-6: CLI-scope query methods exclude rows with `command_type = 'proxy'`
    /// so proxy analytics never inflate CLI figures.
    // Used only by `record_proxy` (proxy feature build); dead_code in default build.
    #[allow(dead_code)]
    Proxy,
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
            CommandType::Proxy => "proxy",
        }
    }
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
}

// ============================================================================
// Query result types
// ============================================================================

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct AnalyticsSummary {
    pub(crate) invocations: u64,
    pub(crate) raw_tokens: u64,
    pub(crate) compressed_tokens: u64,
    pub(crate) tokens_saved: u64,
    pub(crate) avg_savings_pct: f64,
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
// Proxy query result types
// ============================================================================

/// Per-(provider, model) proxy breakdown row — the authoritative token-sum unit.
///
/// **AD-AN-9:** per-(provider,model) is the single-encoding unit.  A per-provider
/// rollup that spans multiple bases (e.g. openai = cl100k + o200k) carries
/// `basis = "mixed"` and omits the combined token figure (JSON null).
///
/// Token aggregates exclude rows where `upstream_error_status IS NOT NULL`
/// (AD-PXY-25), so only successfully-relayed requests contribute.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ProxyModelStats {
    /// Provider name — `None` for unknown / undetected provider (NULL column).
    pub provider: Option<String>,
    /// Model identifier — verbatim as stored (AD-PXY-22), `None` when undetected.
    pub model: Option<String>,
    /// Number of successfully forwarded+relayed requests
    /// (rows with `upstream_error_status IS NULL`).
    pub requests: u64,
    /// Number of transformed-but-upstream-errored requests
    /// (rows with `upstream_error_status IS NOT NULL`).
    pub upstream_errors: u64,
    /// Combined raw token count.  `None` when no counted rows exist
    /// (all rows have NULL token columns).
    pub raw_tokens: Option<u64>,
    /// Combined compressed token count.  `None` when no counted rows exist.
    pub compressed_tokens: Option<u64>,
    /// Average savings percentage across success-scope counted rows.
    /// `None` when no counted rows exist.
    pub avg_savings_pct: Option<f64>,
    /// Fraction of success-scope requests with tier=full (0.0 – 100.0).
    pub tier_full_pct: f64,
    /// Fraction of success-scope requests with tier=degraded (0.0 – 100.0).
    pub tier_degraded_pct: f64,
    /// Fraction of success-scope requests with tier=passthrough (0.0 – 100.0).
    pub tier_passthrough_pct: f64,
    /// Counting basis: one of `"exact"`, `"approximation"`, or `"heuristic"`.
    /// Derived from `select_encoding(provider, model)` (AD-AN-9).
    pub basis: String,
    /// Success-scope rows with non-NULL token columns (countable).
    pub counted_rows: u64,
    /// Success-scope rows with NULL token columns (uncountable).
    pub uncounted_rows: u64,
}

/// Per-provider proxy rollup.
///
/// **AD-AN-9:** when a provider spans multiple counting bases (e.g. openai with
/// both cl100k models and o200k models), `basis = "mixed"` and `raw_tokens` /
/// `compressed_tokens` are `None` — the per-model rows carry the authoritative
/// token figures.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ProxyProviderStats {
    /// Provider name — `None` for unknown / undetected.
    pub provider: Option<String>,
    /// Number of successfully forwarded+relayed requests.
    pub requests: u64,
    /// Number of transformed-but-upstream-errored requests.
    pub upstream_errors: u64,
    /// Combined raw token count.
    /// `None` when `basis = "mixed"` or no countable rows exist.
    pub raw_tokens: Option<u64>,
    /// Combined compressed token count.
    /// `None` when `basis = "mixed"` or no countable rows exist.
    pub compressed_tokens: Option<u64>,
    /// Average savings percentage.
    /// `None` when `basis = "mixed"` or no countable rows.
    pub avg_savings_pct: Option<f64>,
    /// Fraction of success-scope requests with tier=full (0.0 – 100.0).
    pub tier_full_pct: f64,
    /// Fraction of success-scope requests with tier=degraded (0.0 – 100.0).
    pub tier_degraded_pct: f64,
    /// Fraction of success-scope requests with tier=passthrough (0.0 – 100.0).
    pub tier_passthrough_pct: f64,
    /// Counting basis: `"exact"`, `"approximation"`, `"heuristic"`, or `"mixed"`.
    pub basis: String,
    /// Success-scope rows with non-NULL token columns (countable).
    pub counted_rows: u64,
    /// Success-scope rows with NULL token columns (uncountable).
    pub uncounted_rows: u64,
}

/// Row returned by `query_block_decisions` (via `AnalyticsStore` trait).
///
/// Only constructed in the trait impl and consumed in tests / the deferred
/// `--audit` CLI path (#469). Suppress dead_code in non-test builds.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[allow(dead_code)]
pub(crate) struct ProxyBlockDecisionRow {
    pub id: i64,
    pub savings_id: i64,
    pub block_index: u64,
    pub component: String,
    pub outcome: String,
    pub bytes_in: u64,
    pub bytes_out: u64,
}

// ============================================================================
// Proxy recording types (always compiled — no rskim-proxy dependency)
// ============================================================================

/// Provider classification for proxy analytics recording.
///
/// AD-PXY-21: Local enum (no rskim-proxy dependency) so the recording core
/// compiles in default builds without the HTTP/TLS stack (ADR-008).  The
/// feature-gated bridge in `proxy_analytics.rs` converts from
/// `rskim_proxy::detect::ProxyProvider` to this type.
///
/// Column mapping in `token_savings.provider`:
/// - `Unknown`   → `NULL`
/// - `Anthropic` → `"anthropic"`
/// - `OpenAI`    → `"openai"`
// Used by proxy_analytics.rs bridge (proxy feature build); dead_code in default build.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordingProvider {
    /// Provider could not be determined from the request.
    Unknown,
    /// Anthropic API (Claude family).
    Anthropic,
    /// OpenAI API (GPT / o-series family).
    OpenAI,
}

/// Per-block decision record from the proxy's BlockRouter.
///
/// AD-AN-13: the unit is bytes (not tokens); `bytes_in`/`bytes_out` are exact
/// and always present (verified log.rs:390-393).  One row per
/// `rskim_contract::log::DecisionRecord` projected from the collecting sink in
/// `server.rs` (AD-PXY-21).
///
/// **Byte-reconciliation caveat:** the `Σ bytes_in` over all `ProxyBlockDecision`
/// rows for a parent equals the sum of the *live-zone candidate* block lengths,
/// NOT the full raw body length.  `BlockRouter` partitions only live-zone
/// mutable classified blocks (zone.rs AC-27); stale/orientation/immutable
/// blocks are forwarded byte-identical and do NOT emit `DecisionRecord`s.
/// This was verified against the `#304` BlockRouter implementation.
// Used by proxy_analytics.rs bridge (proxy feature build); dead_code in default build.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct ProxyBlockDecision {
    /// Zero-indexed position of this block in the decision sequence.
    pub block_index: usize,
    /// Component/engine identifier (e.g. `"block-router"`, `"json"`, `"log"`).
    pub component: String,
    /// Decision outcome string (e.g. `"passthrough"`, `"modified"`, `"degraded"`).
    pub outcome: String,
    /// Input bytes fed into this block.
    pub bytes_in: u64,
    /// Output bytes produced by this block.
    pub bytes_out: u64,
}

/// Input to [`AnalyticsDb::record_proxy`] — bundles all fields for one proxy
/// request recording.
///
/// **Pair-jointly-NULL semantics (AD-AN-7):** `raw_tokens`, `compressed_tokens`,
/// and `savings_pct` are **all** `None` when token counting was unavailable
/// (counter construction failed, or either body was not valid UTF-8).  They are
/// **all** `Some` otherwise — no mixed state, never a fabricated or estimated
/// value.
///
/// The consumer thread in `proxy_analytics.rs` populates the three token fields
/// after calling [`select_encoding`] and constructing a
/// [`rskim_tokens::Counter`] for the resolved encoding.
// Used by proxy_analytics.rs bridge (proxy feature build); dead_code in default build.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct ProxyRecordInput {
    /// Unix timestamp of the request (seconds since epoch).
    pub timestamp: i64,
    /// Provider classification.
    pub provider: RecordingProvider,
    /// Model identifier — verbatim as extracted from the request body, no
    /// casing/alias folding (AD-PXY-22).  `None` when the model could not be
    /// detected.
    pub model: Option<String>,
    /// Turn-level attribution.  `None` until `#344` emits it.
    pub turn_id: Option<String>,
    /// Request tier: `"passthrough"`, `"full"`, or `"degraded"`.
    pub tier: String,
    /// Duration from first request byte to last relayed response byte (AC4).
    pub duration_ms: u64,
    /// Project path for grouping (current working directory at recording time).
    pub project_path: String,
    /// Session ID for per-session attribution (AD-AN-4).
    pub session_id: Option<String>,
    /// Original command string.  The bridge stores the constant `"proxy"` —
    /// the request URL path is deliberately NOT stored, because
    /// `query_by_original_cmd` is CLI-scoped (AD-AN-6) and a path would put
    /// caller-controlled, unbounded text into the column for no consumer.
    pub original_cmd: String,
    /// Pre-counted raw (pre-transform) tokens.  `None` per AD-AN-7 when
    /// counting is unavailable; paired with `compressed_tokens` and
    /// `savings_pct`.
    pub raw_tokens: Option<i64>,
    /// Pre-counted compressed (post-transform) tokens.
    pub compressed_tokens: Option<i64>,
    /// Pre-computed savings percentage.
    pub savings_pct: Option<f64>,
    /// HTTP status code generated by the proxy when the upstream errored
    /// (502 or 504).  `None` for normal relayed rows (AD-PXY-25).
    pub upstream_error_status: Option<i64>,
    /// Per-block decision log (AD-AN-13).
    pub blocks: Vec<ProxyBlockDecision>,
}

// ============================================================================
// select_encoding — provider + model → token counting basis (AD-AN-11)
// ============================================================================

/// Select the token encoding for a proxy request based on provider and model.
///
/// **AD-AN-11:** reconciles `#300`'s model-string API with the provider-enum
/// fallback.  `encoding_for_model` (rskim-tokens) is model-string-only and
/// returns `Heuristic` for unknown strings (verified encoding.rs:138), so this
/// function adds provider-level family defaults so an unrecognized model within
/// a known family uses the family encoding rather than the conservative
/// `Heuristic`.
///
/// ## Fallback table (AC21)
///
/// | Provider  | `model = None`     | recognized model   | unrecognized model |
/// |-----------|--------------------|--------------------|---------------------|
/// | `Unknown` | `Heuristic`        | `Heuristic`        | `Heuristic`         |
/// | `Anthropic` | `AnthropicOffline` | family encoding | `AnthropicOffline`  |
/// | `OpenAI`  | `O200k`            | family encoding    | `O200k`             |
///
/// All 9 cases are pinned by a table-driven unit test (AC21).
// Called by proxy_analytics.rs bridge (proxy feature build); dead_code in default build.
#[allow(dead_code)]
pub(crate) fn select_encoding(provider: &RecordingProvider, model: Option<&str>) -> Encoding {
    match provider {
        // AD-AN-11 challenge #5b: unknown provider → Heuristic even when a model
        // string is present (the model may belong to an unclassifiable provider).
        RecordingProvider::Unknown => Encoding::Heuristic,
        RecordingProvider::Anthropic => match model {
            // AD-AN-11 challenge #5a: family default when model absent.
            None => Encoding::AnthropicOffline,
            // AD-AN-11: unrecognized-within-family → family default;
            // recognized model keeps its own encoding.
            Some(m) => match encoding_for_model(m) {
                Encoding::Heuristic => Encoding::AnthropicOffline,
                e => e,
            },
        },
        RecordingProvider::OpenAI => match model {
            None => Encoding::O200k,
            Some(m) => match encoding_for_model(m) {
                Encoding::Heuristic => Encoding::O200k,
                e => e,
            },
        },
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
        Ok(AnalyticsSummary {
            invocations: 0,
            raw_tokens: 0,
            compressed_tokens: 0,
            tokens_saved: 0,
            avg_savings_pct: 0.0,
        })
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

    // ---- Proxy-scope methods (default impls return empty/zero) ----
    // Default impls allow existing MockStore implementations to compile without
    // change; proxy-aware mocks override only the methods they need.

    /// Per-(provider, model) proxy breakdown.  Default returns empty.
    /// **AD-AN-9:** see [`AnalyticsDb::query_by_model`] for the full contract.
    fn query_by_model(&self, _since: Option<i64>) -> anyhow::Result<Vec<ProxyModelStats>> {
        Ok(vec![])
    }

    /// Per-provider proxy rollup.  Default returns empty.
    /// **AD-AN-9:** see [`AnalyticsDb::query_by_provider`] for the full contract.
    fn query_by_provider(&self, _since: Option<i64>) -> anyhow::Result<Vec<ProxyProviderStats>> {
        Ok(vec![])
    }

    /// Count of transformed-but-upstream-errored requests.  Default returns 0.
    /// **AD-PXY-25:** see [`AnalyticsDb::query_by_upstream_error`].
    fn query_by_upstream_error(&self, _since: Option<i64>) -> anyhow::Result<u64> {
        Ok(0)
    }

    /// Accumulated proxy event drop count from analytics_meta.  Default returns 0.
    /// **AD-AN-8:** see [`AnalyticsDb::query_proxy_dropped_records`].
    fn query_proxy_dropped_records(&self) -> anyhow::Result<u64> {
        Ok(0)
    }

    /// Per-block decision rows for a parent savings_id.  Default returns empty.
    /// **AD-AN-13:** implementation inlined in `impl AnalyticsStore for AnalyticsDb`.
    ///
    /// Called only from tests (and the deferred `--audit` CLI path — #469). The
    /// `#[allow(dead_code)]` suppresses the warning in non-test builds.
    #[allow(dead_code)]
    fn query_block_decisions(
        &self,
        _savings_id: i64,
    ) -> anyhow::Result<Vec<ProxyBlockDecisionRow>> {
        Ok(vec![])
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

        // AD-AN-5: future-version rejection — the FIRST step after Connection::open,
        // before chmod, busy_timeout, WAL, and run_migrations (AC3).
        //
        // Opening the connection is unavoidable (needed to read PRAGMA user_version),
        // but `Connection::open` does not mutate the DB file on its own.  Checking
        // the version here ensures a future-version DB receives no schema write,
        // no WAL journal_mode flip, and no permission chmod from this skim build.
        {
            let db_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            if db_version > schema::CURRENT_SCHEMA_VERSION {
                anyhow::bail!(
                    "analytics database schema version {} is newer than this skim \
                     build (supports up to v{}); upgrade skim or delete {:?} to reset",
                    db_version,
                    schema::CURRENT_SCHEMA_VERSION,
                    path
                );
            }
        }

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

    /// Record a token savings measurement.
    ///
    /// The `original_cmd` field is truncated to [`Self::MAX_CMD_LEN`] characters
    /// before storage to bound database row size.
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
            "INSERT INTO token_savings (timestamp, command_type, original_cmd, raw_tokens, compressed_tokens, savings_pct, duration_ms, project_path, mode, language, parse_tier, session_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
            ],
        )?;
        Ok(())
    }

    /// Query aggregate summary.
    pub(crate) fn query_summary(&self, since: Option<i64>) -> anyhow::Result<AnalyticsSummary> {
        let (where_clause, params) = cli_scope_clause(since);
        // Fifth column: per-row floored tokens_saved — consistent with
        // query_by_command/query_by_lang/query_by_mode.  Rows where
        // compressed_tokens > raw_tokens (expansion) contribute 0 to tokens_saved
        // while their true counts remain in raw_tokens/compressed_tokens columns.
        let sql = format!(
            "SELECT COUNT(*), \
             COALESCE(SUM(raw_tokens), 0), \
             COALESCE(SUM(compressed_tokens), 0), \
             COALESCE(AVG(savings_pct), 0), \
             COALESCE(SUM(CASE WHEN raw_tokens > compressed_tokens THEN raw_tokens - compressed_tokens ELSE 0 END), 0) \
             FROM token_savings {where_clause}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let row = stmt.query_row(rusqlite::params_from_iter(params), |row| {
            let invocations: u64 = row.get(0)?;
            let raw_tokens: i64 = row.get(1)?;
            let compressed_tokens: i64 = row.get(2)?;
            let avg_savings_pct: f64 = row.get(3)?;
            let tokens_saved: i64 = row.get(4)?;
            Ok(AnalyticsSummary {
                invocations,
                raw_tokens: raw_tokens as u64,
                compressed_tokens: compressed_tokens as u64,
                // .max(0) is defensive; CASE WHEN already floors per-row.
                tokens_saved: tokens_saved.max(0) as u64,
                avg_savings_pct,
            })
        })?;
        Ok(row)
    }

    /// Query daily breakdown.
    pub(crate) fn query_daily(&self, since: Option<i64>) -> anyhow::Result<Vec<DailyStats>> {
        let (where_clause, params) = cli_scope_clause(since);
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
        let (where_clause, params) = cli_scope_clause(since);
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
        let (clause, params) = cli_scope_clause_with_extra(since, "language IS NOT NULL");
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
        let (clause, params) = cli_scope_clause_with_extra(since, "mode IS NOT NULL");
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
        let (clause, params) = cli_scope_clause_with_extra(since, "parse_tier IS NOT NULL");
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
        let (where_clause, params) = cli_scope_clause(since);
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
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM proxy_block_decisions WHERE savings_id IN \
             (SELECT id FROM token_savings WHERE timestamp < ?1)",
            [cutoff],
        )?;
        let count = tx.execute("DELETE FROM token_savings WHERE timestamp < ?1", [cutoff])?;
        tx.commit()?;
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

        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM proxy_block_decisions WHERE savings_id IN \
             (SELECT id FROM token_savings \
              WHERE command_type != 'proxy' AND compressed_tokens > raw_tokens)",
            [],
        )?;
        let count = tx.execute(
            "DELETE FROM token_savings \
             WHERE command_type != 'proxy' AND compressed_tokens > raw_tokens",
            [],
        )?;
        tx.commit()?;

        // Write sentinel so this never runs again.
        let _ = self.conn.execute(
            "INSERT OR REPLACE INTO analytics_meta (key, value) VALUES ('invalid_records_cleaned', 1)",
            [],
        );

        Ok(count)
    }

    /// Record an alignment decision into the `alignment_decisions` table.
    ///
    /// # AD-CA-9 / AD-AN-5
    ///
    /// Called by the `ChannelAlignmentRecorder` consumer thread. Silently
    /// ignores errors (fire-and-forget analytics path). Gated on `proxy`
    /// feature because only the proxy-gated consumer thread calls this.
    #[cfg(feature = "proxy")]
    pub(crate) fn record_alignment(&self, r: &AlignmentDecisionRecord) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO alignment_decisions (
                timestamp, request_id, provider,
                tools_key_sorted, spans_compacted,
                skim_breakpoints_injected, client_breakpoint_count,
                volatile_warn_count, fail_open,
                input_len, output_len,
                input_sha256, output_sha256
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13
            )",
            rusqlite::params![
                r.timestamp,
                r.request_id,
                r.provider,
                r.tools_key_sorted as i64,
                r.spans_compacted as i64,
                r.skim_breakpoints_injected as i64,
                r.client_breakpoint_count as i64,
                r.volatile_warn_count as i64,
                r.fail_open as i64,
                r.input_len as i64,
                r.output_len as i64,
                r.input_sha256.as_ref(),
                r.output_sha256.as_ref(),
            ],
        )?;
        Ok(())
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
        let (where_clause, params) = cli_scope_clause(since);
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

    /// Record one proxy request in `token_savings` + per-block rows in
    /// `proxy_block_decisions`.
    ///
    /// **AD-AN-7:** `raw_tokens`, `compressed_tokens`, `savings_pct` are stored
    /// as a jointly-NULL triple — never partial; the caller must ensure all three
    /// are `Some` or all three are `None`.  A partial `Some` is an invariant
    /// violation detectable only at the caller (BPE counter construction).
    ///
    /// **AD-PXY-25:** `upstream_error_status` is non-`None` for
    /// transformed-but-upstream-errored rows (502/504).  These rows are excluded
    /// from savings and tier aggregates by the query layer.
    // Called by proxy_analytics.rs bridge (proxy feature build); dead_code in default build.
    #[allow(dead_code)]
    pub(crate) fn record_proxy(&mut self, r: &ProxyRecordInput) -> anyhow::Result<()> {
        // AD-PXY-21: map provider enum to the nullable TEXT column value.
        let provider_str: Option<&str> = match &r.provider {
            RecordingProvider::Unknown => None,
            RecordingProvider::Anthropic => Some("anthropic"),
            RecordingProvider::OpenAI => Some("openai"),
        };

        // AD-AN-13: single transaction — parent row and per-block detail rows
        // are committed together.  `Transaction::commit` is required; if it is
        // not called (e.g. on `?` early return), rusqlite rolls back on Drop
        // (ADR-006: abort before persisting, old state survives).
        let tx = self.conn.transaction()?;

        tx.execute(
            "INSERT INTO token_savings \
             (timestamp, command_type, original_cmd, \
              raw_tokens, compressed_tokens, savings_pct, \
              duration_ms, project_path, parse_tier, session_id, \
              provider, model, turn_id, upstream_error_status) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            rusqlite::params![
                r.timestamp,
                CommandType::Proxy.as_str(),
                r.original_cmd,
                r.raw_tokens,
                r.compressed_tokens,
                r.savings_pct,
                r.duration_ms as i64,
                r.project_path,
                r.tier,
                r.session_id,
                provider_str,
                r.model,
                r.turn_id,
                r.upstream_error_status,
            ],
        )?;

        let parent_id = tx.last_insert_rowid();

        // AD-AN-13: per-block detail rows — same transaction.
        for block in &r.blocks {
            tx.execute(
                "INSERT INTO proxy_block_decisions \
                 (savings_id, block_index, component, outcome, bytes_in, bytes_out) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    parent_id,
                    block.block_index as i64,
                    block.component,
                    block.outcome,
                    block.bytes_in as i64,
                    block.bytes_out as i64,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Persist accumulated proxy event drop count to `analytics_meta`.
    ///
    /// **AD-AN-8:** uses an upsert so the counter is monotonically additive
    /// across process restarts.  The key `proxy_dropped_records` accumulates
    /// the total drops ever observed.
    ///
    /// The `count` parameter is the number of drops that occurred since the
    /// last persist call (from the process-local `AtomicU64` in the bridge).
    ///
    /// Persist failure is fail-open per AD-AN-8 — the **caller** is responsible
    /// for deciding whether to log or discard the error.
    // Called by proxy_analytics.rs bridge (proxy feature build); dead_code in default build.
    #[allow(dead_code)]
    pub(crate) fn analytics_meta_add_drop_count(&self, count: u64) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO analytics_meta (key, value) VALUES ('proxy_dropped_records', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = value + ?1",
            [count as i64],
        )?;
        Ok(())
    }

    /// Query per-(provider, model) proxy breakdown rows.
    ///
    /// **AD-AN-9:** per-(provider,model) is the authoritative single-encoding
    /// token-sum unit.  Token and tier aggregates exclude rows where
    /// `upstream_error_status IS NOT NULL` (AD-PXY-25).  Rows are ordered
    /// `(provider IS NULL, provider, model IS NULL, model)` — NULL-last — so
    /// identical input produces byte-identical JSON (AC13).
    #[allow(dead_code)]
    pub(crate) fn query_by_model(
        &self,
        since: Option<i64>,
    ) -> anyhow::Result<Vec<ProxyModelStats>> {
        let (proxy_clause, params) = proxy_scope_clause(since);
        let sql = format!(
            "SELECT \
             provider, model, \
             COUNT(CASE WHEN upstream_error_status IS NULL THEN 1 END), \
             COUNT(CASE WHEN upstream_error_status IS NOT NULL THEN 1 END), \
             SUM(CASE WHEN upstream_error_status IS NULL THEN raw_tokens END), \
             SUM(CASE WHEN upstream_error_status IS NULL THEN compressed_tokens END), \
             AVG(CASE WHEN upstream_error_status IS NULL THEN savings_pct END), \
             COUNT(CASE WHEN upstream_error_status IS NULL AND raw_tokens IS NOT NULL THEN 1 END), \
             COUNT(CASE WHEN upstream_error_status IS NULL AND raw_tokens IS NULL THEN 1 END), \
             COALESCE(SUM(CASE WHEN upstream_error_status IS NULL AND parse_tier = 'full' THEN 1 ELSE 0 END), 0), \
             COALESCE(SUM(CASE WHEN upstream_error_status IS NULL AND parse_tier = 'degraded' THEN 1 ELSE 0 END), 0), \
             COALESCE(SUM(CASE WHEN upstream_error_status IS NULL AND parse_tier = 'passthrough' THEN 1 ELSE 0 END), 0) \
             FROM token_savings {proxy_clause} \
             GROUP BY provider, model \
             ORDER BY (provider IS NULL), provider, (model IS NULL), model"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            let provider: Option<String> = row.get(0)?;
            let model: Option<String> = row.get(1)?;
            let requests: u64 = row.get(2)?;
            let upstream_errors: u64 = row.get(3)?;
            let raw_tokens: Option<i64> = row.get(4)?;
            let compressed_tokens: Option<i64> = row.get(5)?;
            let avg_savings_pct: Option<f64> = row.get(6)?;
            let counted_rows: u64 = row.get(7)?;
            let uncounted_rows: u64 = row.get(8)?;
            let tier_full: i64 = row.get(9)?;
            let tier_degraded: i64 = row.get(10)?;
            let tier_passthrough: i64 = row.get(11)?;
            let t = if requests > 0 { requests as f64 } else { 1.0 };
            Ok((
                provider,
                model,
                requests,
                upstream_errors,
                raw_tokens,
                compressed_tokens,
                avg_savings_pct,
                counted_rows,
                uncounted_rows,
                tier_full,
                tier_degraded,
                tier_passthrough,
                t,
            ))
        })?;

        let mut result = Vec::new();
        for row_res in rows {
            let (
                provider,
                model,
                requests,
                upstream_errors,
                raw_tokens,
                compressed_tokens,
                avg_savings_pct,
                counted_rows,
                uncounted_rows,
                tier_full,
                tier_degraded,
                tier_passthrough,
                t,
            ) = row_res?;

            let basis = counting_basis_label(provider.as_deref(), model.as_deref()).to_string();

            result.push(ProxyModelStats {
                provider,
                model,
                requests,
                upstream_errors,
                raw_tokens: raw_tokens.map(|v| v.max(0) as u64),
                compressed_tokens: compressed_tokens.map(|v| v.max(0) as u64),
                avg_savings_pct,
                tier_full_pct: tier_full as f64 / t * 100.0,
                tier_degraded_pct: tier_degraded as f64 / t * 100.0,
                tier_passthrough_pct: tier_passthrough as f64 / t * 100.0,
                basis,
                counted_rows,
                uncounted_rows,
            });
        }
        Ok(result)
    }

    /// Query per-provider proxy rollup.
    ///
    /// **AD-AN-9:** Derives provider-level stats from [`query_by_model`] by
    /// grouping model rows in Rust.  When a provider spans multiple encoding
    /// bases, `basis = "mixed"` is set and token fields are `None` (AC11).
    #[allow(dead_code)]
    pub(crate) fn query_by_provider(
        &self,
        since: Option<i64>,
    ) -> anyhow::Result<Vec<ProxyProviderStats>> {
        let models = self.query_by_model(since)?;

        let mut provider_groups: Vec<(Option<String>, Vec<&ProxyModelStats>)> = Vec::new();
        for m in &models {
            if let Some(entry) = provider_groups.iter_mut().find(|(p, _)| p == &m.provider) {
                entry.1.push(m);
            } else {
                provider_groups.push((m.provider.clone(), vec![m]));
            }
        }

        let mut result = Vec::with_capacity(provider_groups.len());
        for (provider, rows) in provider_groups {
            let requests: u64 = rows.iter().map(|r| r.requests).sum();
            let upstream_errors: u64 = rows.iter().map(|r| r.upstream_errors).sum();
            let counted_rows: u64 = rows.iter().map(|r| r.counted_rows).sum();
            let uncounted_rows: u64 = rows.iter().map(|r| r.uncounted_rows).sum();

            let t = if requests > 0 { requests as f64 } else { 1.0 };
            let full_count: f64 = rows
                .iter()
                .map(|r| r.tier_full_pct / 100.0 * r.requests as f64)
                .sum();
            let deg_count: f64 = rows
                .iter()
                .map(|r| r.tier_degraded_pct / 100.0 * r.requests as f64)
                .sum();
            let pass_count: f64 = rows
                .iter()
                .map(|r| r.tier_passthrough_pct / 100.0 * r.requests as f64)
                .sum();

            let encodings: Vec<Encoding> = rows
                .iter()
                .map(|r| {
                    let prov = recording_provider_from_str(r.provider.as_deref());
                    select_encoding(&prov, r.model.as_deref())
                })
                .collect();
            let first_encoding = encodings.first().copied().unwrap_or(Encoding::Heuristic);
            let is_mixed = encodings.iter().any(|&e| e != first_encoding);
            let first_basis = rows
                .first()
                .map(|r| r.basis.as_str())
                .unwrap_or("heuristic");

            let (basis, raw_tokens, compressed_tokens, avg_savings_pct) = if is_mixed {
                ("mixed".to_string(), None, None, None)
            } else if counted_rows > 0 {
                let raw: u64 = rows.iter().filter_map(|r| r.raw_tokens).sum();
                let comp: u64 = rows.iter().filter_map(|r| r.compressed_tokens).sum();
                let weight_total: u64 = rows
                    .iter()
                    .filter(|r| r.avg_savings_pct.is_some())
                    .map(|r| r.counted_rows)
                    .sum();
                let avg_pct: Option<f64> = if weight_total == 0 {
                    None
                } else {
                    let weighted: f64 = rows
                        .iter()
                        .filter_map(|r| r.avg_savings_pct.map(|p| p * r.counted_rows as f64))
                        .sum();
                    Some(weighted / weight_total as f64)
                };
                (first_basis.to_string(), Some(raw), Some(comp), avg_pct)
            } else {
                (first_basis.to_string(), None, None, None)
            };

            result.push(ProxyProviderStats {
                provider,
                requests,
                upstream_errors,
                raw_tokens,
                compressed_tokens,
                avg_savings_pct,
                tier_full_pct: full_count / t * 100.0,
                tier_degraded_pct: deg_count / t * 100.0,
                tier_passthrough_pct: pass_count / t * 100.0,
                basis,
                counted_rows,
                uncounted_rows,
            });
        }
        Ok(result)
    }

    /// Count transformed-but-upstream-errored proxy requests.
    ///
    /// **AD-PXY-25:** these rows have `upstream_error_status IS NOT NULL` and
    /// are excluded from savings/tier aggregates.
    #[allow(dead_code)]
    pub(crate) fn query_by_upstream_error(&self, since: Option<i64>) -> anyhow::Result<u64> {
        let (base_clause, params) = proxy_scope_clause(since);
        let sql = format!(
            "SELECT COUNT(*) FROM token_savings {base_clause} \
             AND upstream_error_status IS NOT NULL"
        );
        let count: i64 = self
            .conn
            .query_row(&sql, rusqlite::params_from_iter(params), |r| r.get(0))?;
        Ok(count.max(0) as u64)
    }

    /// Read the accumulated proxy event drop count from `analytics_meta`.
    ///
    /// **AD-AN-8:** the `proxy_dropped_records` key is written by
    /// [`analytics_meta_add_drop_count`] at proxy shutdown.  Returns 0 when no
    /// drops have been recorded (key absent).
    #[allow(dead_code)]
    pub(crate) fn query_proxy_dropped_records(&self) -> anyhow::Result<u64> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE((SELECT value FROM analytics_meta \
                 WHERE key = 'proxy_dropped_records'), 0)",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(count.max(0) as u64)
    }

    /// Adjust the SQLite busy-wait timeout on the underlying connection.
    ///
    /// Used by [`spawn_consumer`] to cap per-call SQLITE_BUSY waits at a short
    /// bound so a locked DB does not stall the consumer drain loop.
    #[cfg(feature = "proxy")]
    pub(crate) fn set_busy_timeout(&self, d: Duration) -> anyhow::Result<()> {
        self.conn.busy_timeout(d).map_err(Into::into)
    }

    /// Return ALL rows from `proxy_block_decisions` (test helper for byte-reconciliation checks).
    ///
    /// **AD-AN-13:** exercises the full block table — used by proxy_analytics.rs tests to
    /// assert the byte-reconciliation invariant without needing the parent `savings_id` in
    /// advance.
    #[cfg(all(test, feature = "proxy"))]
    pub(crate) fn all_block_decisions(&self) -> anyhow::Result<Vec<ProxyBlockDecisionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, savings_id, block_index, component, outcome, bytes_in, bytes_out \
             FROM proxy_block_decisions ORDER BY savings_id, block_index",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ProxyBlockDecisionRow {
                id: row.get(0)?,
                savings_id: row.get(1)?,
                block_index: row.get::<_, i64>(2)? as u64,
                component: row.get(3)?,
                outcome: row.get(4)?,
                bytes_in: row.get::<_, i64>(5)?.max(0) as u64,
                bytes_out: row.get::<_, i64>(6)?.max(0) as u64,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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
    /// Clear all analytics rows.
    ///
    /// **AD-AN-13 / AC22:** the `proxy_block_decisions` detail rows are cleared
    /// with their parents in one transaction.  `PRAGMA foreign_keys` is off
    /// (SQLite default) so the declared FK does not cascade; deleting only the
    /// parents would strand the detail rows with no way to ever remove them.
    fn clear(&self) -> anyhow::Result<()> {
        self.conn.execute_batch(
            "BEGIN;
             DELETE FROM proxy_block_decisions;
             DELETE FROM token_savings;
             COMMIT;",
        )?;
        Ok(())
    }

    fn query_by_model(&self, since: Option<i64>) -> anyhow::Result<Vec<ProxyModelStats>> {
        self.query_by_model(since)
    }

    fn query_by_provider(&self, since: Option<i64>) -> anyhow::Result<Vec<ProxyProviderStats>> {
        self.query_by_provider(since)
    }

    fn query_by_upstream_error(&self, since: Option<i64>) -> anyhow::Result<u64> {
        self.query_by_upstream_error(since)
    }

    fn query_proxy_dropped_records(&self) -> anyhow::Result<u64> {
        self.query_proxy_dropped_records()
    }

    /// Query per-block decision rows for a specific parent `token_savings` row.
    ///
    /// **AD-AN-13:** used for testing the byte-reconciliation invariant and for
    /// audit queries (the deferred `--audit` flag, Ticket #469).
    fn query_block_decisions(&self, savings_id: i64) -> anyhow::Result<Vec<ProxyBlockDecisionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, savings_id, block_index, component, outcome, bytes_in, bytes_out \
             FROM proxy_block_decisions WHERE savings_id = ?1 \
             ORDER BY block_index",
        )?;
        let rows = stmt.query_map([savings_id], |row| {
            Ok(ProxyBlockDecisionRow {
                id: row.get(0)?,
                savings_id: row.get(1)?,
                block_index: row.get::<_, i64>(2)? as u64,
                component: row.get(3)?,
                outcome: row.get(4)?,
                bytes_in: row.get::<_, i64>(5)?.max(0) as u64,
                bytes_out: row.get::<_, i64>(6)?.max(0) as u64,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

/// Build WHERE clause for optional since filter (unconstrained by command_type).
///
/// Used only in tests (to verify `since_clause_with_extra` in isolation).
/// All production query methods use [`cli_scope_clause`] or [`cli_scope_clause_with_extra`]
/// instead to exclude proxy rows (AD-AN-6).
#[cfg(test)]
fn since_clause(since: Option<i64>) -> (String, Vec<i64>) {
    match since {
        Some(ts) => ("WHERE timestamp >= ?1".to_string(), vec![ts]),
        None => (String::new(), vec![]),
    }
}

/// Build WHERE clause for CLI-scope queries — excludes proxy rows.
///
/// **AD-AN-6:** all eight CLI-scope aggregates exclude `command_type = 'proxy'`
/// rows so proxy analytics never inflate CLI figures.
fn cli_scope_clause(since: Option<i64>) -> (String, Vec<i64>) {
    match since {
        Some(ts) => (
            "WHERE command_type != 'proxy' AND timestamp >= ?1".to_string(),
            vec![ts],
        ),
        None => ("WHERE command_type != 'proxy'".to_string(), vec![]),
    }
}

/// Build WHERE clause for CLI-scope queries with an extra condition (AD-AN-6).
fn cli_scope_clause_with_extra(since: Option<i64>, extra_condition: &str) -> (String, Vec<i64>) {
    let (base, params) = cli_scope_clause(since);
    (format!("{base} AND {extra_condition}"), params)
}

/// Build WHERE clause with an optional extra condition appended.
///
/// Composes the `since` filter with an additional SQL predicate (e.g.
/// `"language IS NOT NULL"`). The extra condition is AND-ed to the since
/// clause when present, or becomes its own WHERE clause when since is None.
/// Used only in tests; production code uses [`cli_scope_clause_with_extra`].
#[cfg(test)]
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
// Proxy query helpers
// ============================================================================

/// Build WHERE clause scoped to proxy rows only.
fn proxy_scope_clause(since: Option<i64>) -> (String, Vec<i64>) {
    match since {
        Some(ts) => (
            "WHERE command_type = 'proxy' AND timestamp >= ?1".to_string(),
            vec![ts],
        ),
        None => ("WHERE command_type = 'proxy'".to_string(), vec![]),
    }
}

/// Map a stored provider column value back to a [`RecordingProvider`].
fn recording_provider_from_str(s: Option<&str>) -> RecordingProvider {
    match s {
        Some("anthropic") => RecordingProvider::Anthropic,
        Some("openai") => RecordingProvider::OpenAI,
        _ => RecordingProvider::Unknown,
    }
}

/// Derive the coarse 3-way counting-basis label from the stored provider+model.
///
/// **AD-AN-9:** the label is stable across additive `encoding.rs` changes that
/// stay within the same bucket.
///
/// Returns `"exact"` (Cl100k/O200k), `"approximation"` (AnthropicOffline),
/// or `"heuristic"` (Heuristic).
fn counting_basis_label(provider: Option<&str>, model: Option<&str>) -> &'static str {
    let rp = recording_provider_from_str(provider);
    match select_encoding(&rp, model) {
        Encoding::Cl100k | Encoding::O200k => "exact",
        Encoding::AnthropicOffline => "approximation",
        Encoding::Heuristic => "heuristic",
    }
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

// ============================================================================
// AlignmentDecisionRecord — per-request cache-alignment record (AD-CA-9)
// ============================================================================

/// A single per-request cache-alignment decision record.
///
/// # AD-CA-9 / AD-AN-5
///
/// The record type is **unconditional** (no proxy feature gate) so the schema
/// migration and this type are always present regardless of build variant.
/// Only the writer wiring (`ChannelAlignmentRecorder`) is proxy-gated.
///
/// Fields mirror [`crate::analytics::AlignStats`] from rskim-align, plus
/// `timestamp`, `request_id`, and `provider` for the DB row context.
///
/// # Note on dead_code in non-proxy builds
///
/// AD-CA-9 keeps this struct unconditional so the schema migration always
/// runs regardless of build variant. Non-proxy builds define but never
/// construct it — `#[allow(dead_code)]` is the correct suppression here.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct AlignmentDecisionRecord {
    /// Unix timestamp (seconds) recorded at the point of the alignment call.
    pub timestamp: i64,
    /// Caller-assigned request identifier (forwarded from the proxy seam).
    pub request_id: String,
    /// Detected provider — `"Anthropic"` or `"OpenAi"`.
    pub provider: String,
    /// True when at least one tool/schema span had its keys sorted.
    pub tools_key_sorted: bool,
    /// True when at least one span was compacted (whitespace removed).
    pub spans_compacted: bool,
    /// Number of `cache_control` markers injected by skim (0–2 in v1).
    pub skim_breakpoints_injected: usize,
    /// Number of client-supplied `cache_control` markers found in the body.
    pub client_breakpoint_count: usize,
    /// Number of volatile-pattern detections (warn/stats only — no effect on placement).
    pub volatile_warn_count: usize,
    /// True when the stage returned the input unchanged (fail-open passthrough).
    pub fail_open: bool,
    /// Input byte length.
    pub input_len: usize,
    /// Output byte length.
    pub output_len: usize,
    /// SHA-256 of the stage input.
    pub input_sha256: [u8; 32],
    /// SHA-256 of the stage output.
    pub output_sha256: [u8; 32],
}

// ============================================================================
// AlignmentRecorder trait — proxy-gated (AD-CA-9: writer proxy-gated)
// ============================================================================

/// Writer trait for alignment decision records.
///
/// # AD-CA-9 / AD-AN-5
///
/// A focused writer trait — NOT an extension of the query-only
/// [`AnalyticsStore`] trait (verified `AnalyticsStore` exposes only `query_*`
/// methods and `clear`, mod.rs:326-372). Implementations are fire-and-forget:
/// a blocked, slow, or panicking recorder MUST NEVER delay or alter request
/// forwarding. The production implementation (`ChannelAlignmentRecorder`) uses
/// `try_send` with drop-on-overflow to enforce this.
#[cfg(feature = "proxy")]
pub(crate) trait AlignmentRecorder: Send + Sync {
    /// Record one alignment decision.
    ///
    /// Must return immediately without blocking. Drop-on-overflow is the
    /// correct failure mode (see `ChannelAlignmentRecorder`).
    fn record(&self, rec: AlignmentDecisionRecord);
}

// ============================================================================
// NoopRecorder — no-op implementation for disabled analytics (proxy-gated)
// ============================================================================

/// No-op recorder used when `analytics.enabled` is false.
///
/// # AD-CA-9
///
/// Returned by [`ChannelAlignmentRecorder::new_boxed`] when analytics are
/// disabled so call sites need not check `enabled` on every invocation.
#[cfg(feature = "proxy")]
pub(crate) struct NoopRecorder;

#[cfg(feature = "proxy")]
impl AlignmentRecorder for NoopRecorder {
    fn record(&self, _rec: AlignmentDecisionRecord) {}
}

// ============================================================================
// ChannelAlignmentRecorder — production implementation (proxy-gated)
// ============================================================================

/// Bounded channel capacity for alignment decision records.
///
/// 256 slots is generous for typical proxy throughput (a few requests/second).
/// When the channel is full, records are dropped and counted — the proxy path
/// is never stalled.
#[cfg(feature = "proxy")]
const ALIGN_CHANNEL_CAP: usize = 256;

/// Production [`AlignmentRecorder`] backed by a bounded crossbeam channel.
///
/// # Design (AD-CA-9 / AD-AN-5)
///
/// - **Bounded channel + `try_send`**: a blocked/full channel causes drop-on-overflow,
///   never a stall. The drop counter is observable for diagnostics.
/// - **One registered consumer thread**: opens `AnalyticsDb::open_default()` once
///   (WAL + `busy_timeout(5000)`, honouring `SKIM_ANALYTICS_DB` > `SKIM_CACHE_DIR`)
///   and drains the channel until the sender is dropped.
/// - **`register_thread` registration**: the consumer handle is registered so
///   `flush_pending()` joins it before process exit, ensuring all queued records
///   reach the DB when the proxy shuts down.
///
/// # Drain sequence (proxy shutdown)
///
/// When `serve_with_stage()` returns (SIGINT / SIGTERM), the
/// `TransformPipeline` drops, which drops `CacheAlignStage`, which drops this
/// recorder, which drops the `Sender`. The channel is then closed; the consumer
/// thread drains remaining records and exits. `flush_pending()` in `main()` joins
/// it before the process exits.
#[cfg(feature = "proxy")]
pub(crate) struct ChannelAlignmentRecorder {
    sender: crossbeam_channel::Sender<AlignmentDecisionRecord>,
    /// Observable overflow count — incremented on every `try_send` failure.
    /// Never fatal: a dropped record is less harmful than a stalled request.
    drop_count: Arc<AtomicUsize>,
}

#[cfg(feature = "proxy")]
impl ChannelAlignmentRecorder {
    /// Construct a recorder and spawn its consumer thread.
    ///
    /// Returns a [`NoopRecorder`] (boxed) when `analytics.enabled` is false.
    /// Otherwise constructs a bounded channel, registers the consumer thread
    /// via [`register_thread`], and returns the recorder (boxed).
    pub(crate) fn new_boxed(analytics: &AnalyticsConfig) -> Box<dyn AlignmentRecorder> {
        if !analytics.enabled {
            return Box::new(NoopRecorder);
        }
        let (tx, rx) = crossbeam_channel::bounded::<AlignmentDecisionRecord>(ALIGN_CHANNEL_CAP);
        let drop_count = Arc::new(AtomicUsize::new(0));
        let handle = std::thread::spawn(move || {
            // Open once — WAL + busy_timeout(5000) inherited from open_default.
            // Honouring SKIM_ANALYTICS_DB > SKIM_CACHE_DIR (mod.rs:414).
            let db = match AnalyticsDb::open_default() {
                Ok(d) => d,
                Err(_) => return, // Fail-open: silently skip all records.
            };
            while let Ok(rec) = rx.recv() {
                let _ = db.record_alignment(&rec);
            }
            // Channel closed (sender dropped) → drain complete.
        });
        register_thread(handle);
        Box::new(Self {
            sender: tx,
            drop_count,
        })
    }

    /// Number of records dropped because the bounded channel was full or disconnected.
    ///
    /// # AC18 — Observable drop counter
    ///
    /// Drop-on-overflow is the correct failure mode for a fire-and-forget analytics
    /// path, but a silent drop is not observable. Observability has two halves, and
    /// this accessor is only the second one:
    /// - **In production builds**, each drop emits a `SKIM_DEBUG`-gated stderr notice
    ///   from [`AlignmentRecorder::record`] carrying the running total. That notice —
    ///   not this accessor — is what an operator reads.
    /// - **In test builds**, this accessor exposes the same counter so the overflow
    ///   path is assertable. It is `#[cfg(test)]` because no production call site
    ///   reads it, and an unused `pub(crate)` method would trip the zero-warnings gate.
    #[cfg(test)]
    pub(crate) fn drop_count(&self) -> usize {
        self.drop_count.load(Ordering::Relaxed)
    }
}

#[cfg(feature = "proxy")]
impl AlignmentRecorder for ChannelAlignmentRecorder {
    /// Send a record via `try_send`.
    ///
    /// On overflow (`Err(Full)`) or disconnected (`Err(Disconnected)`), increments
    /// `drop_count`, emits a `SKIM_DEBUG` notice, and returns immediately. Never blocks.
    ///
    /// # AC18 — Observable drop counter
    ///
    /// The counter is readable via [`ChannelAlignmentRecorder::drop_count`] and each
    /// drop emits a debug-gated stderr notice, so overflow is diagnosable rather than
    /// silent. Dropping is still the correct failure mode: a stalled request is worse
    /// than a missing analytics row.
    fn record(&self, rec: AlignmentDecisionRecord) {
        if self.sender.try_send(rec).is_err() {
            let dropped = self.drop_count.fetch_add(1, Ordering::Relaxed) + 1;
            crate::debug_log!(
                "skim proxy: alignment analytics channel full — record dropped (total dropped: {dropped})"
            );
        }
    }
}

/// Persist a record to the default database, with auto-pruning.
fn persist_record(record: &TokenSavingsRecord) {
    if let Ok(db) = AnalyticsDb::open_default() {
        let _ = db.record(record);
        db.maybe_prune();
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
        let Ok(comp_tokens) = tokens::count_tokens(&compressed_text) else {
            return;
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
}

/// Shared metadata common to all rows in a single file-op invocation.
pub(crate) struct FileOpCommon {
    pub(crate) mode: Option<String>,
    pub(crate) project_path: String,
    pub(crate) session_id: Option<String>,
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
                Some(TokenSavingsRecord {
                    timestamp: ts,
                    command_type: CommandType::File,
                    original_cmd: r.original_cmd,
                    raw_tokens: raw,
                    compressed_tokens: comp,
                    savings_pct: savings_percentage(raw, comp),
                    duration_ms: 0,
                    project_path: common.project_path.clone(),
                    mode: common.mode.clone(),
                    language: r.language,
                    parse_tier: r.parse_tier,
                    session_id: common.session_id.clone(),
                })
            })
            .collect();
        // Open DB once for all rows; skip silently on open failure (mirrors persist_record).
        if let Ok(db) = AnalyticsDb::open_default() {
            for rec in &records {
                let _ = db.record(rec);
            }
            if !records.is_empty() {
                db.maybe_prune();
            }
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
        },
    );
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
pub(crate) mod tests {
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

    /// Verify schema version is at the current head after all migrations.
    ///
    /// Updated from v3 to v4 when the v4 migration was added (#305).
    #[test]
    fn test_schema_version_is_current() {
        let (db, _tmp) = test_db();
        let version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            version,
            schema::CURRENT_SCHEMA_VERSION,
            "schema version should equal CURRENT_SCHEMA_VERSION ({}) after all migrations",
            schema::CURRENT_SCHEMA_VERSION
        );
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
    // Schema migration v5 tests — alignment_decisions table (AD-CA-9)
    // ========================================================================

    /// POSITIVE: v5 migration creates the alignment_decisions table.
    /// DISCRIMINATING (PF-007): if the migration were removed, this test fails.
    #[test]
    fn test_alignment_decisions_table_created_by_migration() {
        let (db, _tmp) = test_db();
        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='alignment_decisions'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 1,
            "alignment_decisions table must be created by migration v5"
        );
    }

    /// POSITIVE: schema version is 5 after migration.
    /// DISCRIMINATING: if PRAGMA user_version = 5 were not the last statement, version would be wrong.
    #[test]
    fn test_schema_version_is_5_after_migration() {
        let (db, _tmp) = test_db();
        let version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 5, "schema version must be 5 after all migrations");
    }

    /// POSITIVE: record_alignment inserts a row into alignment_decisions.
    /// DISCRIMINATING: deleting record_alignment would cause this to return 0.
    #[cfg(feature = "proxy")]
    #[test]
    fn test_record_alignment_inserts_row() {
        let (db, _tmp) = test_db();
        let rec = AlignmentDecisionRecord {
            timestamp: 1711300000,
            request_id: "req-test-001".to_string(),
            provider: "Anthropic".to_string(),
            tools_key_sorted: true,
            spans_compacted: true,
            skim_breakpoints_injected: 1,
            client_breakpoint_count: 0,
            volatile_warn_count: 0,
            fail_open: false,
            input_len: 512,
            output_len: 486,
            input_sha256: [0u8; 32],
            output_sha256: [1u8; 32],
        };
        db.record_alignment(&rec).unwrap();

        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM alignment_decisions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "one alignment decision row must be inserted");

        // Verify round-trip of key fields.
        let (stored_req, stored_provider, stored_injected): (String, String, i64) = db
            .conn
            .query_row(
                "SELECT request_id, provider, skim_breakpoints_injected FROM alignment_decisions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(stored_req, "req-test-001");
        assert_eq!(stored_provider, "Anthropic");
        assert_eq!(stored_injected, 1);
    }

    /// POSITIVE: fail_open=true record round-trips correctly (SHA-256 pair equal).
    #[cfg(feature = "proxy")]
    #[test]
    fn test_record_alignment_fail_open_row() {
        let (db, _tmp) = test_db();
        let sha = [0xAB_u8; 32];
        let rec = AlignmentDecisionRecord {
            timestamp: 1711300001,
            request_id: "req-failopen".to_string(),
            provider: "OpenAi".to_string(),
            tools_key_sorted: false,
            spans_compacted: false,
            skim_breakpoints_injected: 0,
            client_breakpoint_count: 0,
            volatile_warn_count: 0,
            fail_open: true,
            input_len: 100,
            output_len: 100,
            input_sha256: sha,
            output_sha256: sha,
        };
        db.record_alignment(&rec).unwrap();

        let (stored_fail_open, stored_in_len, stored_out_len): (i64, i64, i64) = db
            .conn
            .query_row(
                "SELECT fail_open, input_len, output_len FROM alignment_decisions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(stored_fail_open, 1, "fail_open must be stored as 1");
        assert_eq!(stored_in_len, 100);
        assert_eq!(stored_out_len, 100);
    }

    // ========================================================================
    // AlignmentRecorder test helper types (AC18 / AD-CA-9) — proxy-gated
    // ========================================================================

    /// Synchronous mock recorder — stores all records for test assertion.
    ///
    /// Shares the internal list via `Arc<Mutex<...>>` so callers can inspect
    /// records after moving the recorder into a stage.
    ///
    /// Used in proxy.rs tests (AC18): prove that a call to `CacheAlignStage::apply`
    /// records exactly one `AlignmentDecisionRecord` per request.
    #[cfg(feature = "proxy")]
    pub(crate) struct BlockingMockRecorder {
        records: Arc<Mutex<Vec<AlignmentDecisionRecord>>>,
    }

    #[cfg(feature = "proxy")]
    impl BlockingMockRecorder {
        pub(crate) fn new() -> Self {
            Self {
                records: Arc::new(Mutex::new(Vec::new())),
            }
        }

        /// Return a cloned handle to the shared record list.
        pub(crate) fn handle(&self) -> Arc<Mutex<Vec<AlignmentDecisionRecord>>> {
            Arc::clone(&self.records)
        }
    }

    #[cfg(feature = "proxy")]
    impl AlignmentRecorder for BlockingMockRecorder {
        fn record(&self, rec: AlignmentDecisionRecord) {
            let mut g = self.records.lock().unwrap_or_else(|p| p.into_inner());
            g.push(rec);
        }
    }

    /// Counting mock recorder — counts `record` calls.
    ///
    /// Used in proxy.rs tests (AC18) to prove `CacheAlignStage::apply` emits exactly
    /// one record per request. Drop-on-overflow is covered separately by
    /// `test_channel_recorder_overflow_increments_observable_drop_count`, which
    /// exercises the real `ChannelAlignmentRecorder`.
    #[cfg(feature = "proxy")]
    pub(crate) struct CountingMockRecorder {
        pub(crate) count: Arc<AtomicUsize>,
    }

    #[cfg(feature = "proxy")]
    impl CountingMockRecorder {
        pub(crate) fn new() -> Self {
            Self {
                count: Arc::new(AtomicUsize::new(0)),
            }
        }

        pub(crate) fn handle(&self) -> Arc<AtomicUsize> {
            Arc::clone(&self.count)
        }
    }

    #[cfg(feature = "proxy")]
    impl AlignmentRecorder for CountingMockRecorder {
        fn record(&self, _rec: AlignmentDecisionRecord) {
            self.count.fetch_add(1, Ordering::Relaxed);
        }
    }

    // AC18 / AD-CA-9: NoopRecorder never panics.
    #[cfg(feature = "proxy")]
    #[test]
    fn test_noop_recorder_does_not_panic() {
        let rec = NoopRecorder;
        rec.record(AlignmentDecisionRecord {
            timestamp: 0,
            request_id: String::new(),
            provider: "Anthropic".to_string(),
            tools_key_sorted: false,
            spans_compacted: false,
            skim_breakpoints_injected: 0,
            client_breakpoint_count: 0,
            volatile_warn_count: 0,
            fail_open: true,
            input_len: 0,
            output_len: 0,
            input_sha256: [0u8; 32],
            output_sha256: [0u8; 32],
        });
    }

    /// Build an `AlignmentDecisionRecord` for tests (content-free by construction).
    #[cfg(feature = "proxy")]
    fn sample_alignment_record(request_id: &str) -> AlignmentDecisionRecord {
        AlignmentDecisionRecord {
            timestamp: 0,
            request_id: request_id.to_string(),
            provider: "Anthropic".to_string(),
            tools_key_sorted: true,
            spans_compacted: true,
            skim_breakpoints_injected: 1,
            client_breakpoint_count: 0,
            volatile_warn_count: 0,
            fail_open: false,
            input_len: 10,
            output_len: 10,
            input_sha256: [0u8; 32],
            output_sha256: [0u8; 32],
        }
    }

    // AC18 / AD-CA-9: bounded-channel overflow drops the record AND increments an
    // OBSERVABLE counter. DISCRIMINATING (PF-007): if `record` blocked instead of
    // using try_send, this test would hang; if the counter were not incremented,
    // the final assertion fails.
    #[cfg(feature = "proxy")]
    #[test]
    fn test_channel_recorder_overflow_increments_observable_drop_count() {
        // Capacity 1, and no consumer draining it: the second send must overflow.
        let (tx, rx) = crossbeam_channel::bounded::<AlignmentDecisionRecord>(1);
        let recorder = ChannelAlignmentRecorder {
            sender: tx,
            drop_count: Arc::new(AtomicUsize::new(0)),
        };

        recorder.record(sample_alignment_record("fits"));
        assert_eq!(recorder.drop_count(), 0, "first record fits in the channel");

        recorder.record(sample_alignment_record("overflows"));
        recorder.record(sample_alignment_record("overflows-again"));
        assert_eq!(
            recorder.drop_count(),
            2,
            "AC18: each overflow must increment the observable drop counter"
        );

        // The one record that fit is still intact and content-free.
        let received = rx.try_recv().expect("first record must be queued");
        assert_eq!(received.request_id, "fits");
        drop(rx);

        // A disconnected channel also counts as a drop, never a block.
        recorder.record(sample_alignment_record("disconnected"));
        assert_eq!(recorder.drop_count(), 3);
    }

    // AC18 / AD-CA-9: ChannelAlignmentRecorder returns NoopRecorder when disabled.
    #[cfg(feature = "proxy")]
    #[test]
    fn test_channel_recorder_noop_when_disabled() {
        let cfg = AnalyticsConfig {
            enabled: false,
            input_cost_per_mtok: None,
            session_id: None,
        };
        let recorder = ChannelAlignmentRecorder::new_boxed(&cfg);
        // Must not panic — NoopRecorder::record is a no-op.
        recorder.record(AlignmentDecisionRecord {
            timestamp: 0,
            request_id: "r".to_string(),
            provider: "Anthropic".to_string(),
            tools_key_sorted: false,
            spans_compacted: false,
            skim_breakpoints_injected: 0,
            client_breakpoint_count: 0,
            volatile_warn_count: 0,
            fail_open: true,
            input_len: 0,
            output_len: 0,
            input_sha256: [0u8; 32],
            output_sha256: [0u8; 32],
        });
    }

    // Schema v4 migration tests (AC1–AC4, #305)
    //
    // AD-AN-5: covers the single-transaction table-rebuild migration that adds
    // nullable provider/model/turn_id/upstream_error_status columns, the
    // breakdown index idx_ts_provider_model, the proxy_block_decisions detail
    // table with its FK + idx_pbd_savings_id, and PRAGMA user_version = 4 as
    // the last statement (ADR-006 fail-safe ordering).
    // ========================================================================

    /// Helper: create a raw v3 DB at `path` (bypasses AnalyticsDb::open so we can
    /// test the migration rather than always starting at the current head).
    ///
    /// Inserts two representative CLI rows so aggregate assertions are non-trivial.
    fn setup_v3_db_raw(path: &std::path::Path) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE token_savings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp INTEGER NOT NULL,
                command_type TEXT NOT NULL,
                original_cmd TEXT NOT NULL,
                raw_tokens INTEGER NOT NULL,
                compressed_tokens INTEGER NOT NULL,
                savings_pct REAL NOT NULL,
                duration_ms INTEGER NOT NULL,
                project_path TEXT NOT NULL,
                mode TEXT,
                language TEXT,
                parse_tier TEXT,
                session_id TEXT
            );
            CREATE TABLE analytics_meta (key TEXT PRIMARY KEY, value INTEGER);
            CREATE INDEX idx_ts_timestamp ON token_savings(timestamp);
            CREATE INDEX idx_ts_command_type ON token_savings(command_type);
            CREATE INDEX idx_ts_session_id ON token_savings(session_id);
            INSERT INTO token_savings
                (timestamp, command_type, original_cmd, raw_tokens,
                 compressed_tokens, savings_pct, duration_ms, project_path,
                 mode, language, parse_tier, session_id)
                VALUES
                (1711300000, 'file', 'skim test.rs', 1000, 200, 80.0, 15,
                 '/tmp/test', 'structure', 'rust', NULL, 'sess-abc'),
                (1711300100, 'test', 'cargo test', 500, 100, 80.0, 200,
                 '/tmp/test', NULL, NULL, 'full', NULL);
            PRAGMA user_version = 3;",
        )
        .unwrap();
    }

    /// AC1 — fresh DB is created at schema version 4 with all expected columns,
    /// the proxy_block_decisions detail table, the FK from savings_id to
    /// token_savings(id), and both new indexes; breakdown-index shape is
    /// validated via EXPLAIN QUERY PLAN (SEARCH, not SCAN).
    #[test]
    fn test_schema_v4_fresh_db_creates_v4_schema() {
        use std::collections::HashSet;
        let (db, _tmp) = test_db();

        // 1. Schema version.
        let version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            version, 5,
            "fresh DB must be at schema version 5 after all migrations"
        );

        // 2. token_savings columns include all v4 additions (nullable).
        let mut stmt = db.conn.prepare("PRAGMA table_info(token_savings)").unwrap();
        let cols: HashSet<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        for expected in &[
            "id",
            "timestamp",
            "command_type",
            "original_cmd",
            "raw_tokens",
            "compressed_tokens",
            "savings_pct",
            "duration_ms",
            "project_path",
            "mode",
            "language",
            "parse_tier",
            "session_id",
            "provider",
            "model",
            "turn_id",
            "upstream_error_status",
        ] {
            assert!(
                cols.contains(*expected),
                "AC1: token_savings must have column '{expected}' after v4 migration; \
                 found: {cols:?}"
            );
        }

        // 3. provider, model, turn_id, upstream_error_status are nullable
        //    (notnull flag == 0 in PRAGMA table_info column 3).
        let mut stmt2 = db.conn.prepare("PRAGMA table_info(token_savings)").unwrap();
        let nullable_check: Vec<(String, i64)> = stmt2
            .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(3)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .filter(|(name, _)| {
                matches!(
                    name.as_str(),
                    "provider" | "model" | "turn_id" | "upstream_error_status"
                )
            })
            .collect();
        assert_eq!(
            nullable_check.len(),
            4,
            "AC1: all four new v4 columns must be present"
        );
        for (col, notnull) in &nullable_check {
            assert_eq!(
                *notnull, 0,
                "AC1: column '{col}' must be nullable (notnull=0), got notnull={notnull}"
            );
        }

        // 4. proxy_block_decisions table exists with expected columns.
        let mut pbd_stmt = db
            .conn
            .prepare("PRAGMA table_info(proxy_block_decisions)")
            .unwrap();
        let pbd_cols: Vec<String> = pbd_stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for expected in &[
            "id",
            "savings_id",
            "block_index",
            "component",
            "outcome",
            "bytes_in",
            "bytes_out",
        ] {
            assert!(
                pbd_cols.contains(&(*expected).to_string()),
                "AC1: proxy_block_decisions must have column '{expected}'; found: {pbd_cols:?}"
            );
        }

        // 5. FK from proxy_block_decisions.savings_id to token_savings(id).
        let mut fk_stmt = db
            .conn
            .prepare("PRAGMA foreign_key_list(proxy_block_decisions)")
            .unwrap();
        // Columns: id, seq, table, from, to, ...
        let fks: Vec<(String, String, String)> = fk_stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(2)?, // table
                    r.get::<_, String>(3)?, // from
                    r.get::<_, String>(4)?, // to
                ))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            !fks.is_empty(),
            "AC1: proxy_block_decisions must have a foreign key"
        );
        let (ref_table, from_col, to_col) = &fks[0];
        assert_eq!(
            ref_table, "token_savings",
            "AC1: FK must reference token_savings"
        );
        assert_eq!(
            from_col, "savings_id",
            "AC1: FK from-column must be savings_id"
        );
        assert_eq!(to_col, "id", "AC1: FK to-column must be id");

        // 6. idx_ts_provider_model index exists.
        let idx_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
                 AND name='idx_ts_provider_model'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            idx_count, 1,
            "AC1: idx_ts_provider_model must exist after v4 migration"
        );

        // 7. idx_pbd_savings_id index exists on proxy_block_decisions.
        let pbd_idx_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' \
                 AND name='idx_pbd_savings_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            pbd_idx_count, 1,
            "AC1: idx_pbd_savings_id must exist after v4 migration"
        );

        // 8. Breakdown-index column shape via PRAGMA index_info — deterministic
        //    regardless of table size/stats. Verifies the three-column index is
        //    ordered (command_type, provider, model), which enables the optimizer
        //    to use it for both by-provider (WHERE command_type=? GROUP BY provider)
        //    and by-model (WHERE command_type=? GROUP BY provider, model) queries.
        let mut iinfo_stmt = db
            .conn
            .prepare("PRAGMA index_info(idx_ts_provider_model)")
            .unwrap();
        // Columns: seqno, cid, name
        let idx_cols: Vec<(i64, String)> = iinfo_stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(2)?)))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(
            idx_cols.len(),
            3,
            "AC1: idx_ts_provider_model must have exactly 3 columns; got: {idx_cols:?}"
        );
        assert_eq!(
            idx_cols[0].1, "command_type",
            "AC1: idx_ts_provider_model leading column must be 'command_type'"
        );
        assert_eq!(
            idx_cols[1].1, "provider",
            "AC1: idx_ts_provider_model second column must be 'provider'"
        );
        assert_eq!(
            idx_cols[2].1, "model",
            "AC1: idx_ts_provider_model third column must be 'model'"
        );

        // 9. EXPLAIN QUERY PLAN best-effort: insert enough rows that the optimizer
        //    will choose the index over a full scan, then verify it appears in
        //    the plan detail for the representative by-provider and by-model queries.
        for i in 0..120i64 {
            db.conn
                .execute(
                    "INSERT INTO token_savings \
                     (timestamp, command_type, original_cmd, raw_tokens, \
                      compressed_tokens, savings_pct, duration_ms, project_path, \
                      provider, model) \
                     VALUES (?1, 'proxy', 'proxy', 1000, 200, 80.0, 15, '/tmp', \
                     'anthropic', 'claude-3-5-sonnet-20241022')",
                    rusqlite::params![1711300000i64 + i],
                )
                .unwrap();
        }
        for i in 0..120i64 {
            db.conn
                .execute(
                    "INSERT INTO token_savings \
                     (timestamp, command_type, original_cmd, raw_tokens, \
                      compressed_tokens, savings_pct, duration_ms, project_path) \
                     VALUES (?1, 'file', 'skim a.rs', 800, 160, 80.0, 5, '/tmp')",
                    rusqlite::params![1711500000i64 + i],
                )
                .unwrap();
        }
        db.conn.execute_batch("ANALYZE;").unwrap();

        // by-provider representative query
        let mut eqp1 = db
            .conn
            .prepare(
                "EXPLAIN QUERY PLAN \
                 SELECT provider, COUNT(*) \
                 FROM token_savings WHERE command_type = 'proxy' \
                 GROUP BY provider ORDER BY provider IS NULL, provider",
            )
            .unwrap();
        let plan1: Vec<String> = eqp1
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        let plan1_str = plan1.join(" | ");
        assert!(
            plan1_str.contains("idx_ts_provider_model") || plan1_str.contains("USING INDEX"),
            "AC1: by-provider query plan must reference idx_ts_provider_model; \
             got: {plan1_str}"
        );

        // by-model representative query
        let mut eqp2 = db
            .conn
            .prepare(
                "EXPLAIN QUERY PLAN \
                 SELECT provider, model, COUNT(*) \
                 FROM token_savings WHERE command_type = 'proxy' \
                 GROUP BY provider, model \
                 ORDER BY provider IS NULL, provider, model IS NULL, model",
            )
            .unwrap();
        let plan2: Vec<String> = eqp2
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        let plan2_str = plan2.join(" | ");
        assert!(
            plan2_str.contains("idx_ts_provider_model") || plan2_str.contains("USING INDEX"),
            "AC1: by-model query plan must reference idx_ts_provider_model; \
             got: {plan2_str}"
        );
    }

    /// AC2 — opening an existing v3 DB with CLI rows upgrades it to v4 inside a
    /// single transaction, preserves every row with NULL for the four new columns,
    /// and leaves every pre-existing CLI aggregate numerically identical.
    #[test]
    fn test_schema_v4_v3_to_v4_migration_preserves_data() {
        let tmp = NamedTempFile::new().unwrap();
        setup_v3_db_raw(tmp.path());

        // Snapshot the pre-migration state with raw rusqlite (v3 schema).
        let pre_invocations: i64 = {
            let conn = rusqlite::Connection::open(tmp.path()).unwrap();
            conn.query_row("SELECT COUNT(*) FROM token_savings", [], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(
            pre_invocations, 2,
            "test setup must have 2 rows before migration"
        );

        // Trigger the v4 migration by opening with AnalyticsDb.
        let db = AnalyticsDb::open(tmp.path()).unwrap();

        // Version is now 4.
        let version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 5, "AC2: version must be 5 after full migration (v4+v5)");

        // All 2 original rows are still there.
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM token_savings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2, "AC2: all v3 rows must survive migration");

        // The four new v4 columns default to NULL for existing rows.
        let mut stmt = db
            .conn
            .prepare("SELECT provider, model, turn_id, upstream_error_status FROM token_savings")
            .unwrap();
        let new_cols: Vec<_> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        for (provider, model, turn_id, upstream_error_status) in &new_cols {
            assert!(
                provider.is_none(),
                "AC2: pre-migration rows must have NULL provider"
            );
            assert!(
                model.is_none(),
                "AC2: pre-migration rows must have NULL model"
            );
            assert!(
                turn_id.is_none(),
                "AC2: pre-migration rows must have NULL turn_id"
            );
            assert!(
                upstream_error_status.is_none(),
                "AC2: pre-migration rows must have NULL upstream_error_status"
            );
        }

        // Pre-existing CLI aggregates are numerically identical after migration.
        // The v4 migration adds command_type != 'proxy' scope predicates, which are
        // no-ops on v3 data (no proxy rows), so all eight aggregates survive verbatim.
        // Snapshot all EIGHT (AC2 — plan criterion explicitly lists every aggregate).

        // (1) query_summary
        // Row 1: raw=1000, compressed=200 → saved=800
        // Row 2: raw=500, compressed=100 → saved=400
        let summary = db.query_summary(None).unwrap();
        assert_eq!(
            summary.invocations, 2,
            "AC2: summary.invocations must equal pre-migration value"
        );
        assert_eq!(
            summary.raw_tokens, 1500,
            "AC2: summary.raw_tokens aggregate must be unchanged after migration"
        );
        assert_eq!(
            summary.compressed_tokens, 300,
            "AC2: summary.compressed_tokens aggregate must be unchanged after migration"
        );
        assert_eq!(
            summary.tokens_saved, 1200,
            "AC2: summary.tokens_saved aggregate must be unchanged after migration"
        );

        // (2) query_daily — both timestamps are on the same UTC day (2024-03-24).
        let daily = db.query_daily(None).unwrap();
        assert_eq!(
            daily.len(),
            1,
            "AC2: query_daily must return exactly 1 day entry"
        );
        assert_eq!(
            daily[0].invocations, 2,
            "AC2: daily.invocations must equal pre-migration value"
        );
        assert_eq!(
            daily[0].tokens_saved, 1200,
            "AC2: daily.tokens_saved must equal pre-migration value"
        );

        // (3) query_by_command — ordered by tokens_saved DESC: file(800) then test(400).
        let by_cmd = db.query_by_command(None).unwrap();
        assert_eq!(by_cmd.len(), 2, "AC2: query_by_command must return 2 rows");
        assert_eq!(
            by_cmd[0].command_type, "file",
            "AC2: first by_command row must be 'file'"
        );
        assert_eq!(by_cmd[0].invocations, 1, "AC2: file invocations must be 1");
        assert_eq!(
            by_cmd[0].tokens_saved, 800,
            "AC2: file tokens_saved must be 800"
        );
        assert_eq!(
            by_cmd[1].command_type, "test",
            "AC2: second by_command row must be 'test'"
        );
        assert_eq!(by_cmd[1].invocations, 1, "AC2: test invocations must be 1");
        assert_eq!(
            by_cmd[1].tokens_saved, 400,
            "AC2: test tokens_saved must be 400"
        );

        // (4) query_by_language — only Row 1 has language IS NOT NULL (rust).
        let by_lang = db.query_by_language(None).unwrap();
        assert_eq!(by_lang.len(), 1, "AC2: query_by_language must return 1 row");
        assert_eq!(by_lang[0].language, "rust", "AC2: language must be 'rust'");
        assert_eq!(by_lang[0].files, 1, "AC2: rust files must be 1");
        assert_eq!(
            by_lang[0].tokens_saved, 800,
            "AC2: rust tokens_saved must be 800"
        );

        // (5) query_by_mode — only Row 1 has mode IS NOT NULL (structure).
        let by_mode = db.query_by_mode(None).unwrap();
        assert_eq!(by_mode.len(), 1, "AC2: query_by_mode must return 1 row");
        assert_eq!(
            by_mode[0].mode, "structure",
            "AC2: mode must be 'structure'"
        );
        assert_eq!(by_mode[0].files, 1, "AC2: structure files must be 1");
        assert_eq!(
            by_mode[0].tokens_saved, 800,
            "AC2: structure tokens_saved must be 800"
        );

        // (6) query_by_original_cmd — ordered by tokens_saved DESC.
        let by_orig = db.query_by_original_cmd(None).unwrap();
        assert_eq!(
            by_orig.len(),
            2,
            "AC2: query_by_original_cmd must return 2 rows"
        );
        assert_eq!(
            by_orig[0].original_cmd, "skim test.rs",
            "AC2: first by_original_cmd must be 'skim test.rs'"
        );
        assert_eq!(
            by_orig[0].tokens_saved, 800,
            "AC2: skim test.rs tokens_saved must be 800"
        );
        assert_eq!(
            by_orig[1].original_cmd, "cargo test",
            "AC2: second by_original_cmd must be 'cargo test'"
        );
        assert_eq!(
            by_orig[1].tokens_saved, 400,
            "AC2: cargo test tokens_saved must be 400"
        );

        // (7) query_session_stats — Row 1 has session_id='sess-abc' (saved=800).
        //                           Row 2 has session_id=NULL (untagged).
        let sess = db.query_session_stats(None).unwrap();
        assert_eq!(
            sess.distinct_sessions, 1,
            "AC2: distinct_sessions must be 1"
        );
        assert_eq!(
            sess.total_tokens_saved, 800,
            "AC2: session total_tokens_saved must be 800 (only sessioned row)"
        );
        assert_eq!(
            sess.untagged_invocations, 1,
            "AC2: untagged_invocations must be 1"
        );

        // (8) query_tier_distribution — only Row 2 has parse_tier IS NOT NULL (full).
        let tier = db.query_tier_distribution(None).unwrap();
        assert!(
            (tier.full_pct - 100.0).abs() < 1e-9,
            "AC2: full_pct must be 100.0 (only row with parse_tier IS NOT NULL is 'full')"
        );
        assert!(
            tier.degraded_pct.abs() < 1e-9,
            "AC2: degraded_pct must be 0.0"
        );
        assert!(
            tier.passthrough_pct.abs() < 1e-9,
            "AC2: passthrough_pct must be 0.0"
        );

        // proxy_block_decisions table exists (but is empty — no proxy rows yet).
        let pbd_count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM proxy_block_decisions", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            pbd_count, 0,
            "AC2: proxy_block_decisions must exist and be empty after migration"
        );
    }

    /// AC3 — opening a database whose user_version > CURRENT_SCHEMA_VERSION returns
    /// an error that names the version mismatch, performs no schema write, does not
    /// create proxy_block_decisions, and leaves user_version unchanged.
    ///
    /// Deleting the future-version check in open() makes this test fail (an
    /// unexpected migration or no error is returned) — PF-007 discriminating guard.
    ///
    /// The WAL and chmod assertions are the AC3 discriminators that prevent
    /// reordering the version check to AFTER those operations:
    /// - journal_mode must NOT be 'wal' (WAL flip did not run)
    /// - On Unix, file permissions must NOT be 0600 if they started at 0644
    ///   (chmod did not run)
    #[test]
    fn test_schema_v4_future_version_rejected_without_write() {
        let tmp = NamedTempFile::new().unwrap();

        // Set up a "future version" DB manually (user_version = 99).
        {
            let conn = rusqlite::Connection::open(tmp.path()).unwrap();
            conn.execute_batch(
                "PRAGMA user_version = 99;
                 CREATE TABLE token_savings (id INTEGER PRIMARY KEY, future_col TEXT);",
            )
            .unwrap();
        }

        // Set the file to 0644 (owner-readable + world-readable) before the
        // rejected open. If AnalyticsDb::open were to chmod it to 0600, the
        // permission check below would catch it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::metadata(tmp.path()).unwrap();
            let mut perms = metadata.permissions();
            perms.set_mode(0o644);
            std::fs::set_permissions(tmp.path(), perms).unwrap();
        }

        // Attempt to open — must fail.
        let result = AnalyticsDb::open(tmp.path());
        assert!(
            result.is_err(),
            "AC3: opening a future-version DB must return Err"
        );
        let err_msg = result.err().unwrap().to_string();
        assert!(
            err_msg.contains("99"),
            "AC3: error message must name version 99; got: {err_msg}"
        );
        assert!(
            err_msg.contains("4") || err_msg.contains(&schema::CURRENT_SCHEMA_VERSION.to_string()),
            "AC3: error message must name CURRENT_SCHEMA_VERSION; got: {err_msg}"
        );

        // No migration ran: user_version is still 99.
        let conn = rusqlite::Connection::open(tmp.path()).unwrap();
        let version_after: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            version_after, 99,
            "AC3: user_version must remain 99 after rejected open"
        );

        // AC3 / PF-007: WAL must NOT have been set after a future-version rejection.
        // Moving the version check AFTER `conn.execute_batch("PRAGMA journal_mode=WAL")`
        // would flip the journal mode and make this assertion fail.
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_ne!(
            journal_mode, "wal",
            "AC3 / PF-007: journal_mode must NOT be 'wal' after future-version rejection \
             (the WAL flip must not run before the version check); got: '{journal_mode}'"
        );

        // AC3 / PF-007 (Unix only): file permissions must NOT have been changed to
        // 0600 after a future-version rejection. Moving the version check AFTER the
        // chmod would modify the file mode and make this assertion fail.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::metadata(tmp.path()).unwrap();
            let mode = metadata.permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o644,
                "AC3 / PF-007: file permissions must NOT be changed after future-version \
                 rejection (chmod must not run before the version check); \
                 expected 0644, got {:#o}",
                mode
            );
        }

        // proxy_block_decisions was NOT created (migration did not run).
        let pbd_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='proxy_block_decisions'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            pbd_exists, 0,
            "AC3: proxy_block_decisions must NOT be created after future-version rejection"
        );

        // The token_savings table retains only the future schema (no new v4 columns
        // were injected by migration).
        let mut stmt = conn.prepare("PRAGMA table_info(token_savings)").unwrap();
        let col_names: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(
            !col_names.contains(&"provider".to_string()),
            "AC3: v4 'provider' column must NOT be added after future-version rejection; \
             found cols: {col_names:?}"
        );
    }

    /// AC4 — an injected failure EARLY in the v3→v4 rebuild (pre-creating
    /// token_savings_new so the first CREATE TABLE fails) leaves user_version = 3
    /// with all v3 rows intact; a subsequent clean open migrates to v4 successfully.
    ///
    /// PF-007 discriminating guard: removing the BEGIN..COMMIT wrapping would allow
    /// a partially-rebuilt table to persist, breaking this assertion.
    #[test]
    fn test_schema_v4_injected_early_migration_failure_stays_v3() {
        let tmp = NamedTempFile::new().unwrap();
        setup_v3_db_raw(tmp.path());

        // Pre-create token_savings_new to make the migration's first CREATE TABLE fail.
        {
            let conn = rusqlite::Connection::open(tmp.path()).unwrap();
            conn.execute_batch("CREATE TABLE token_savings_new (poison_col TEXT);")
                .unwrap();
        }

        // Open attempt must fail (CREATE TABLE token_savings_new errors).
        let result = AnalyticsDb::open(tmp.path());
        assert!(
            result.is_err(),
            "AC4 (early): AnalyticsDb::open must return Err when migration fails at first DDL"
        );

        // After failure the DB is at v3 with all original rows intact.
        {
            let conn = rusqlite::Connection::open(tmp.path()).unwrap();
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                version, 3,
                "AC4 (early): user_version must still be 3 after failed migration"
            );

            let row_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM token_savings", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                row_count, 2,
                "AC4 (early): all v3 rows must survive an interrupted migration"
            );

            // The original v3 columns are present; v4 columns are absent.
            let mut stmt = conn.prepare("PRAGMA table_info(token_savings)").unwrap();
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            assert!(
                !cols.contains(&"provider".to_string()),
                "AC4 (early): v4 'provider' column must not exist after failed migration"
            );
        }

        // Drop the blocking table and re-open — must now migrate to v4 cleanly.
        {
            let conn = rusqlite::Connection::open(tmp.path()).unwrap();
            conn.execute_batch("DROP TABLE token_savings_new;").unwrap();
        }

        let db = AnalyticsDb::open(tmp.path()).expect("AC4 (early): clean re-open must succeed");
        let version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 5, "AC4 (early): clean re-open must migrate to v5");
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM token_savings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 2,
            "AC4 (early): all rows must survive the clean re-migration to v4"
        );
    }

    /// AC4 — an injected failure at the DETAIL-TABLE DDL step
    /// (pre-creating proxy_block_decisions so that statement fails) leaves the DB
    /// at user_version = 3 with all v3 rows intact.
    ///
    /// This specifically tests the ADR-006 property that PRAGMA user_version = 4
    /// being the LAST statement ensures the whole transaction rolls back — including
    /// the earlier DROP + RENAME of token_savings — leaving v3 untouched.
    ///
    /// PF-007 discriminating guard: without the single-transaction BEGIN..COMMIT,
    /// the DROP+RENAME would have committed before the detail-table DDL failed,
    /// leaving a partially-upgraded (and corrupt) token_savings table.
    #[test]
    fn test_schema_v4_injected_detail_table_failure_stays_v3() {
        let tmp = NamedTempFile::new().unwrap();
        setup_v3_db_raw(tmp.path());

        // Pre-create proxy_block_decisions to make the migration fail at the
        // detail-table DDL step (after DROP+RENAME of token_savings already ran
        // within the transaction).
        {
            let conn = rusqlite::Connection::open(tmp.path()).unwrap();
            conn.execute_batch("CREATE TABLE proxy_block_decisions (poison_col TEXT);")
                .unwrap();
        }

        // Open attempt must fail.
        let result = AnalyticsDb::open(tmp.path());
        assert!(
            result.is_err(),
            "AC4 (detail-table): AnalyticsDb::open must return Err when migration \
             fails at proxy_block_decisions DDL"
        );

        // After failure the DB is at v3 with all original rows intact.
        // Crucially: token_savings still has the v3 schema (DROP+RENAME rolled back).
        {
            let conn = rusqlite::Connection::open(tmp.path()).unwrap();
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                version, 3,
                "AC4 (detail-table): user_version must still be 3 after failed detail-table DDL"
            );

            let row_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM token_savings", [], |r| r.get(0))
                .unwrap();
            assert_eq!(
                row_count, 2,
                "AC4 (detail-table): all v3 rows must survive an interrupted migration \
                 at the detail-table DDL step — verifies the whole transaction rolled back"
            );

            // v4 columns were NOT added (the rename rolled back).
            let mut stmt = conn.prepare("PRAGMA table_info(token_savings)").unwrap();
            let cols: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            assert!(
                !cols.contains(&"provider".to_string()),
                "AC4 (detail-table): v4 'provider' column must not exist after \
                 failed detail-table migration; found: {cols:?}"
            );

            // The pre-existing proxy_block_decisions table (created before the migration
            // BEGIN) survives the rollback — the transaction rolled back the DDL inside
            // BEGIN..COMMIT, but not the conflict table created before the transaction.
            // Drop it so a clean re-open can migrate to v4.
            conn.execute_batch("DROP TABLE proxy_block_decisions;")
                .unwrap();
        }

        // Clean re-open must migrate to v4 with data preserved.
        let db = AnalyticsDb::open(tmp.path())
            .expect("AC4 (detail-table): clean re-open must succeed after removing conflict");
        let version: i64 = db
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            version, 5,
            "AC4 (detail-table): clean re-open must reach v5"
        );
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM token_savings", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 2,
            "AC4 (detail-table): all rows must survive the clean re-migration"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 2: proxy recording-core tests
    // -----------------------------------------------------------------------

    /// Helper: build a minimal [`ProxyRecordInput`] with full token counts and
    /// two blocks.  Callers override individual fields for targeted cases.
    fn sample_proxy_input() -> ProxyRecordInput {
        ProxyRecordInput {
            timestamp: 1_700_000_000,
            provider: RecordingProvider::Anthropic,
            model: Some("claude-3-opus-20240229".to_string()),
            turn_id: Some("turn-abc".to_string()),
            tier: "premium".to_string(),
            duration_ms: 42,
            project_path: "/tmp/proj".to_string(),
            session_id: Some("sess-1".to_string()),
            original_cmd: "proxy-request".to_string(),
            raw_tokens: Some(1000),
            compressed_tokens: Some(400),
            savings_pct: Some(60.0),
            upstream_error_status: None,
            blocks: vec![
                ProxyBlockDecision {
                    block_index: 0,
                    component: "assistant".to_string(),
                    outcome: "compressed".to_string(),
                    bytes_in: 500,
                    bytes_out: 200,
                },
                ProxyBlockDecision {
                    block_index: 1,
                    component: "tool_result".to_string(),
                    outcome: "passthrough".to_string(),
                    bytes_in: 300,
                    bytes_out: 300,
                },
            ],
        }
    }

    /// Count rows left in the `proxy_block_decisions` detail table.
    fn detail_row_count(db: &AnalyticsDb) -> i64 {
        db.conn
            .query_row("SELECT COUNT(*) FROM proxy_block_decisions", [], |r| {
                r.get(0)
            })
            .unwrap()
    }

    /// AD-AN-13 (PF-007 discriminating): every path that deletes a parent
    /// `token_savings` row also deletes its `proxy_block_decisions` children.
    ///
    /// `PRAGMA foreign_keys` is off (SQLite default), so the declared FK does
    /// NOT cascade.  Dropping the explicit child DELETE from `clear` or
    /// `prune_older_than` leaves detail rows with no deletion path at all — they
    /// would survive both `skim stats --clear` and the 90-day retention prune
    /// forever.  Removing either child DELETE fails this test.
    ///
    /// `clean_invalid_records` is the third arm, and it is CLI-scoped (AD-AN-6):
    /// it must delete *nothing* here, because an expanded proxy row is a valid
    /// measurement rather than corruption.  See
    /// `test_clean_invalid_records_preserves_expanded_proxy_rows` for the
    /// dedicated coverage.
    #[test]
    fn test_deleting_parent_rows_also_deletes_block_decision_rows() {
        // --- clear() ---
        let (mut db, _tmp) = test_db();
        db.record_proxy(&sample_proxy_input()).unwrap();
        assert_eq!(detail_row_count(&db), 2, "fixture inserts two detail rows");
        AnalyticsStore::clear(&db).unwrap();
        assert_eq!(
            detail_row_count(&db),
            0,
            "clear() must not strand orphaned proxy_block_decisions rows"
        );

        // --- prune_older_than() ---
        let (mut db, _tmp) = test_db();
        let mut old = sample_proxy_input();
        old.timestamp = now_unix_secs() - (100 * SECONDS_PER_DAY as i64);
        db.record_proxy(&old).unwrap();
        let mut fresh = sample_proxy_input();
        fresh.timestamp = now_unix_secs();
        db.record_proxy(&fresh).unwrap();
        assert_eq!(detail_row_count(&db), 4);

        let pruned = db.prune_older_than(90).unwrap();
        assert_eq!(pruned, 1, "only the 100-day-old parent is pruned");
        assert_eq!(
            detail_row_count(&db),
            2,
            "prune must delete the pruned parent's detail rows and keep the rest"
        );
        // The survivors must still belong to the surviving parent — an
        // over-broad child DELETE would leave 0 here.
        let surviving_parent: i64 = db
            .conn
            .query_row("SELECT id FROM token_savings LIMIT 1", [], |r| r.get(0))
            .unwrap();
        let attached: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM proxy_block_decisions WHERE savings_id = ?1",
                [surviving_parent],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            attached, 2,
            "surviving detail rows stay attached to their parent"
        );

        // --- clean_invalid_records() ---
        // AD-AN-6: the sweep repairs a pre-#305 CLI recording bug and is scoped
        // to `command_type != 'proxy'`.  `compressed > raw` on a proxy row is a
        // measured expansion, not corruption (AD-AN-7), so nothing is deleted
        // and no detail row can be stranded.
        let (mut db, _tmp) = test_db();
        let mut expanded = sample_proxy_input();
        expanded.raw_tokens = Some(100);
        expanded.compressed_tokens = Some(500); // measured expansion
        db.record_proxy(&expanded).unwrap();
        assert_eq!(detail_row_count(&db), 2);
        let cleaned = db.clean_invalid_records().unwrap();
        assert_eq!(cleaned, 0, "the CLI-scoped sweep must skip proxy rows");
        assert_eq!(
            detail_row_count(&db),
            2,
            "a surviving parent keeps its detail rows — nothing stranded, nothing lost"
        );
    }

    /// AC21 — `select_encoding` covers all 9 cells of the
    /// {Unknown, Anthropic, OpenAI} × {None, recognized, unrecognized} matrix.
    ///
    /// The OpenAI + "gpt-4" cell is the **discriminating case**: gpt-4 maps to
    /// `Cl100k` (not `O200k`), so a regression that always returns the family
    /// default would produce the wrong answer here.
    #[test]
    fn test_select_encoding_table_driven() {
        use Encoding::*;
        use RecordingProvider::*;

        let cases: &[(&str, RecordingProvider, Option<&str>, Encoding)] = &[
            // Unknown provider → always Heuristic regardless of model
            ("Unknown+None", Unknown, None, Heuristic),
            ("Unknown+recognized", Unknown, Some("gpt-4o"), Heuristic),
            (
                "Unknown+unrecognized",
                Unknown,
                Some("mystery-llm"),
                Heuristic,
            ),
            // Anthropic + None → AnthropicOffline family default
            ("Anthropic+None", Anthropic, None, AnthropicOffline),
            // Anthropic + recognized model → keeps its encoding (also AnthropicOffline
            // for claude-3-opus, but the mechanism is the keep-encoding path)
            (
                "Anthropic+recognized",
                Anthropic,
                Some("claude-3-opus-20240229"),
                AnthropicOffline,
            ),
            // Anthropic + unrecognized → falls back to AnthropicOffline family default
            (
                "Anthropic+unrecognized",
                Anthropic,
                Some("mystery-llm"),
                AnthropicOffline,
            ),
            // OpenAI + None → O200k family default
            ("OpenAI+None", OpenAI, None, O200k),
            // OpenAI + "gpt-4" → Cl100k (recognized, DIFFERENT from O200k family default).
            // This is the discriminating case: a function that blindly returns the
            // family default would return O200k here instead of Cl100k.
            ("OpenAI+gpt-4(Cl100k)", OpenAI, Some("gpt-4"), Cl100k),
            // OpenAI + unrecognized → falls back to O200k family default
            ("OpenAI+unrecognized", OpenAI, Some("mystery-llm"), O200k),
        ];

        for &(label, ref provider, model, expected) in cases {
            let got = select_encoding(provider, model);
            assert_eq!(
                got, expected,
                "select_encoding case '{label}': expected {expected:?} got {got:?}"
            );
        }
    }

    /// AD-AN-13 + AD-PXY-21 — record_proxy writes a correctly-populated parent
    /// row and two child block rows inside a single transaction.
    ///
    /// Verifies: provider column mapping (Anthropic→"anthropic"), model, turn_id,
    /// session_id, command_type ("proxy"), token values, block fields.
    #[test]
    fn test_record_proxy_normal_row_stored_correctly() {
        let (mut db, _tmp) = test_db();
        let input = sample_proxy_input();
        db.record_proxy(&input).unwrap();

        // --- parent row ---
        let (cmd_type, provider, model, turn_id, raw_tok, comp_tok, savings, err_status) = db
            .conn
            .query_row(
                // last_insert_rowid() reflects the last proxy_block_decisions row
                // after record_proxy commits; use LIMIT 1 on the fresh DB instead.
                "SELECT command_type, provider, model, turn_id, \
                 raw_tokens, compressed_tokens, savings_pct, upstream_error_status \
                 FROM token_savings LIMIT 1",
                [],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                        r.get::<_, Option<i64>>(5)?,
                        r.get::<_, Option<f64>>(6)?,
                        r.get::<_, Option<i64>>(7)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(cmd_type, "proxy", "command_type must be 'proxy'");
        // AD-PXY-21: Anthropic maps to "anthropic" string
        assert_eq!(
            provider.as_deref(),
            Some("anthropic"),
            "provider must be 'anthropic' for RecordingProvider::Anthropic"
        );
        assert_eq!(model.as_deref(), Some("claude-3-opus-20240229"));
        assert_eq!(turn_id.as_deref(), Some("turn-abc"));
        // AD-AN-7: all three token columns are present (Some)
        assert_eq!(raw_tok, Some(1000));
        assert_eq!(comp_tok, Some(400));
        assert!(
            savings.map(|s| (s - 60.0).abs() < 0.001).unwrap_or(false),
            "savings_pct must be ~60.0"
        );
        assert_eq!(
            err_status, None,
            "upstream_error_status must be NULL for normal rows"
        );

        // --- block rows ---
        let block_count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM proxy_block_decisions", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(block_count, 2, "must have exactly 2 block rows");

        // verify first block
        let (bi, comp, outcome, b_in, b_out): (i64, String, String, i64, i64) = db
            .conn
            .query_row(
                "SELECT block_index, component, outcome, bytes_in, bytes_out \
                 FROM proxy_block_decisions WHERE block_index = 0",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(bi, 0);
        assert_eq!(comp, "assistant");
        assert_eq!(outcome, "compressed");
        assert_eq!(b_in, 500);
        assert_eq!(b_out, 200);
    }

    /// AD-AN-7 (pair-jointly-NULL) — when token counts are not available, all
    /// three token columns must be stored as NULL together, never as fabricated
    /// or partial values.
    ///
    /// A regression that defaults any of the three columns to 0 instead of NULL
    /// would fail this test.
    #[test]
    fn test_record_proxy_pair_jointly_null_when_counting_unavailable() {
        let (mut db, _tmp) = test_db();
        let mut input = sample_proxy_input();
        // AD-AN-7: simulate a request where token counting was not performed
        input.raw_tokens = None;
        input.compressed_tokens = None;
        input.savings_pct = None;
        db.record_proxy(&input).unwrap();

        let (raw, comp, savings): (Option<i64>, Option<i64>, Option<f64>) = db
            .conn
            .query_row(
                "SELECT raw_tokens, compressed_tokens, savings_pct FROM token_savings LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();

        assert!(
            raw.is_none(),
            "AD-AN-7: raw_tokens must be NULL when counting unavailable"
        );
        assert!(
            comp.is_none(),
            "AD-AN-7: compressed_tokens must be NULL when counting unavailable"
        );
        assert!(
            savings.is_none(),
            "AD-AN-7: savings_pct must be NULL when counting unavailable"
        );
    }

    /// AD-PXY-25 — `upstream_error_status` is stored non-NULL for
    /// transformed-but-upstream-errored requests (e.g. the upstream returned 502
    /// after the proxy had already compressed the body).
    ///
    /// Also verifies that `RecordingProvider::OpenAI` maps to the "openai"
    /// column string (AD-PXY-21) and that `RecordingProvider::Unknown` maps
    /// to NULL provider column.
    #[test]
    fn test_record_proxy_upstream_error_status_stored() {
        let (mut db, _tmp) = test_db();

        // AD-PXY-25: `record_proxy` stores exactly what it is handed; NULLing the
        // token columns for an upstream-errored request is the bridge's job
        // (`build_proxy_record_input`), pinned by
        // `proxy_analytics::tests::test_upstream_errored_event_has_null_token_columns`.
        let mut input = sample_proxy_input();
        input.provider = RecordingProvider::OpenAI;
        input.upstream_error_status = Some(502);
        db.record_proxy(&input).unwrap();

        let (provider, err_status): (Option<String>, Option<i64>) = db
            .conn
            .query_row(
                "SELECT provider, upstream_error_status FROM token_savings LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();

        // AD-PXY-21: OpenAI → "openai"
        assert_eq!(
            provider.as_deref(),
            Some("openai"),
            "provider must be 'openai' for RecordingProvider::OpenAI"
        );
        assert_eq!(
            err_status,
            Some(502),
            "AD-PXY-25: upstream_error_status must store the HTTP error code"
        );

        // Also verify Unknown → NULL (second insert)
        let mut input2 = sample_proxy_input();
        input2.provider = RecordingProvider::Unknown;
        db.record_proxy(&input2).unwrap();

        let provider_unknown: Option<String> = db
            .conn
            .query_row(
                // After inserting block rows, last_insert_rowid() no longer
                // points to token_savings; ORDER BY id DESC picks the latest row.
                "SELECT provider FROM token_savings ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            provider_unknown.is_none(),
            "AD-PXY-21: Unknown provider must map to NULL"
        );
    }

    /// PF-007 discriminating — byte-reconciliation invariant test.
    ///
    /// Verifies that Σ bytes_in and Σ bytes_out across all recorded block rows
    /// exactly match the sum of bytes inserted via `ProxyRecordInput.blocks`.
    /// A missing block row, a duplicated block row, or a mis-attributed block
    /// row (wrong savings_id FK) would each cause this assertion to fail.
    ///
    /// **Important caveat documented here:** this invariant holds over the SET
    /// OF RECORDED BLOCKS ONLY — it is NOT a claim that Σ bytes_in equals the
    /// total raw request body length.  BlockRouter (rskim-compress #304) only
    /// emits `DecisionRecord`s for live-zone mutable classified candidates (AC-27
    /// three-way join).  Non-candidate blocks (stale / orientation / immutable)
    /// are not represented in the decision log.  The invariant is self-consistency
    /// of what was recorded, not full-body coverage.
    #[test]
    fn test_record_proxy_byte_reconciliation_invariant() {
        let (mut db, _tmp) = test_db();

        // Build three blocks with known byte counts.
        let blocks = vec![
            ProxyBlockDecision {
                block_index: 0,
                component: "assistant".to_string(),
                outcome: "compressed".to_string(),
                bytes_in: 800,
                bytes_out: 320,
            },
            ProxyBlockDecision {
                block_index: 1,
                component: "tool_result".to_string(),
                outcome: "passthrough".to_string(),
                bytes_in: 600,
                bytes_out: 600,
            },
            ProxyBlockDecision {
                block_index: 2,
                component: "system".to_string(),
                outcome: "dropped".to_string(),
                bytes_in: 200,
                bytes_out: 0,
            },
        ];

        let expected_bytes_in: u64 = blocks.iter().map(|b| b.bytes_in).sum();
        let expected_bytes_out: u64 = blocks.iter().map(|b| b.bytes_out).sum();

        let mut input = sample_proxy_input();
        input.blocks = blocks;
        db.record_proxy(&input).unwrap();

        // Query the parent id so we query only OUR block rows (discriminating
        // against leftover rows from other tests that share the same DB).
        let parent_id: i64 = db
            .conn
            .query_row(
                // last_insert_rowid() reflects the last proxy_block_decisions row;
                // use LIMIT 1 — only one token_savings row exists in this fresh DB.
                "SELECT id FROM token_savings LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();

        let (row_count, actual_bytes_in, actual_bytes_out): (i64, i64, i64) = db
            .conn
            .query_row(
                "SELECT COUNT(*), SUM(bytes_in), SUM(bytes_out) \
                 FROM proxy_block_decisions WHERE savings_id = ?1",
                [parent_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();

        assert_eq!(
            row_count, 3,
            "PF-007: row count must equal the number of blocks inserted \
             (missing, duplicated, or mis-attributed rows detected)"
        );
        assert_eq!(
            actual_bytes_in as u64, expected_bytes_in,
            "PF-007: Σ bytes_in over recorded blocks must match input sum \
             ({} vs {expected_bytes_in})",
            actual_bytes_in
        );
        assert_eq!(
            actual_bytes_out as u64, expected_bytes_out,
            "PF-007: Σ bytes_out over recorded blocks must match input sum \
             ({} vs {expected_bytes_out})",
            actual_bytes_out
        );
    }

    // -----------------------------------------------------------------------
    // Phase 3: scope-separation + new proxy query tests
    // -----------------------------------------------------------------------

    /// Helper: insert a proxy row into the DB (bypasses record_proxy for speed).
    // Nine positional args mirror the INSERT columns exactly — adding a wrapper
    // struct would move the same fields to the call sites without improving clarity.
    #[allow(clippy::too_many_arguments)]
    fn insert_proxy_row(
        db: &AnalyticsDb,
        provider: Option<&str>,
        model: Option<&str>,
        raw_tokens: Option<i64>,
        compressed_tokens: Option<i64>,
        savings_pct: Option<f64>,
        upstream_error_status: Option<i64>,
        tier: &str,
        ts: i64,
    ) {
        db.conn
            .execute(
                "INSERT INTO token_savings \
                 (timestamp, command_type, original_cmd, raw_tokens, compressed_tokens, \
                  savings_pct, duration_ms, project_path, parse_tier, \
                  provider, model, upstream_error_status) \
                 VALUES (?1, 'proxy', 'proxy', ?2, ?3, ?4, 10, '/tmp', ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    ts,
                    raw_tokens,
                    compressed_tokens,
                    savings_pct,
                    tier,
                    provider,
                    model,
                    upstream_error_status
                ],
            )
            .unwrap();
    }

    /// AC8 (NEGATIVE) — scope separation: CLI aggregates must be numerically
    /// identical with proxy rows present vs. with proxy rows removed.
    ///
    /// PF-007: removing `command_type != 'proxy'` from any CLI query would allow
    /// proxy savings to inflate the CLI headline, breaking the equality assertion.
    #[test]
    fn test_scope_separation_cli_aggregates_exclude_proxy_rows() {
        let (db, _tmp) = test_db();

        // Insert two CLI rows.
        let mut r1 = sample_record();
        r1.raw_tokens = 1000;
        r1.compressed_tokens = 200;
        r1.savings_pct = 80.0;
        r1.timestamp = 1_711_300_000;
        db.record(&r1).unwrap();

        let mut r2 = sample_record();
        r2.raw_tokens = 500;
        r2.compressed_tokens = 150;
        r2.savings_pct = 70.0;
        r2.timestamp = 1_711_301_000;
        db.record(&r2).unwrap();

        // Snapshot CLI aggregates before inserting proxy rows.
        let before_summary = db.query_summary(None).unwrap();
        let before_daily = db.query_daily(None).unwrap();
        let before_by_cmd = db.query_by_command(None).unwrap();

        // Now insert proxy rows with larger token counts — these must NOT appear
        // in any CLI aggregate.
        insert_proxy_row(
            &db,
            Some("anthropic"),
            Some("claude-3-opus-20240229"),
            Some(10_000),
            Some(1_000),
            Some(90.0),
            None,
            "full",
            1_711_302_000,
        );
        insert_proxy_row(
            &db,
            Some("openai"),
            Some("gpt-4"),
            Some(8_000),
            Some(800),
            Some(90.0),
            None,
            "full",
            1_711_303_000,
        );

        // CLI aggregates after proxy rows are inserted must be identical.
        let after_summary = db.query_summary(None).unwrap();
        let after_daily = db.query_daily(None).unwrap();
        let after_by_cmd = db.query_by_command(None).unwrap();

        assert_eq!(
            before_summary.invocations, after_summary.invocations,
            "AC8: invocations must exclude proxy rows; \
             expected {}, got {}",
            before_summary.invocations, after_summary.invocations
        );
        assert_eq!(
            before_summary.tokens_saved, after_summary.tokens_saved,
            "AC8: CLI tokens_saved must exclude proxy rows; \
             expected {}, got {}",
            before_summary.tokens_saved, after_summary.tokens_saved
        );
        assert_eq!(
            before_summary.raw_tokens, after_summary.raw_tokens,
            "AC8: CLI raw_tokens must exclude proxy rows"
        );

        assert_eq!(
            before_daily.len(),
            after_daily.len(),
            "AC8: CLI daily rows count must be identical before/after proxy insertion"
        );

        assert_eq!(
            before_by_cmd.len(),
            after_by_cmd.len(),
            "AC8: CLI by_command count must be identical before/after proxy insertion"
        );

        // Verify that none of the by-command rows have command_type 'proxy'.
        for row in &after_by_cmd {
            assert_ne!(
                row.command_type, "proxy",
                "AC8: query_by_command must never return a proxy row"
            );
        }
    }

    /// AC9 (POSITIVE) — scope separation: proxy savings appear ONLY in the
    /// proxy scope (query_by_model / query_by_provider), never in the CLI
    /// headline summary.
    ///
    /// A proxy-only DB shows a zero CLI headline with all savings in the proxy
    /// scope — verifying the predicate is the only thing keeping them apart.
    #[test]
    fn test_scope_separation_proxy_savings_only_in_proxy_scope() {
        let (db, _tmp) = test_db();

        // Insert only proxy rows.
        insert_proxy_row(
            &db,
            Some("anthropic"),
            Some("claude-3-5-sonnet-20241022"),
            Some(5_000),
            Some(500),
            Some(90.0),
            None,
            "full",
            1_711_300_000,
        );
        insert_proxy_row(
            &db,
            Some("openai"),
            Some("gpt-4o"),
            Some(3_000),
            Some(600),
            Some(80.0),
            None,
            "full",
            1_711_301_000,
        );

        // CLI headline must be zero (no CLI rows).
        let summary = db.query_summary(None).unwrap();
        assert_eq!(
            summary.invocations, 0,
            "AC9: CLI invocations must be 0 in a proxy-only DB"
        );
        assert_eq!(
            summary.tokens_saved, 0,
            "AC9: CLI tokens_saved must be 0 in a proxy-only DB"
        );

        // Proxy scope must carry the savings.
        let by_model = db.query_by_model(None).unwrap();
        assert_eq!(
            by_model.len(),
            2,
            "AC9: query_by_model must return 2 rows (one per model)"
        );

        let total_proxy_requests: u64 = by_model.iter().map(|r| r.requests).sum();
        assert_eq!(
            total_proxy_requests, 2,
            "AC9: proxy scope must have 2 request rows"
        );

        // Both proxy rows are countable — raw_tokens must be non-None.
        for row in &by_model {
            assert!(
                row.raw_tokens.is_some(),
                "AC9: proxy model row must have non-NULL raw_tokens for counted rows"
            );
        }

        // Provider rollup must aggregate correctly.
        let by_provider = db.query_by_provider(None).unwrap();
        assert_eq!(
            by_provider.len(),
            2,
            "AC9: query_by_provider must return 2 provider rows"
        );
    }

    /// AC8 — byte-reconciliation invariant for a compressed proxy row: the sum
    /// of block bytes_in and bytes_out must match the recorded values exactly.
    ///
    /// PF-007: this assertion is golden-independent (pure arithmetic, no golden
    /// token value). Dropping or mis-attributing a block row breaks it.
    #[test]
    fn test_query_block_decisions_byte_reconciliation() {
        let (mut db, _tmp) = test_db();
        let mut input = sample_proxy_input();
        input.blocks = vec![
            ProxyBlockDecision {
                block_index: 0,
                component: "block-router".to_string(),
                outcome: "modified".to_string(),
                bytes_in: 600,
                bytes_out: 120,
            },
            ProxyBlockDecision {
                block_index: 1,
                component: "block-router".to_string(),
                outcome: "passthrough".to_string(),
                bytes_in: 400,
                bytes_out: 400,
            },
        ];
        db.record_proxy(&input).unwrap();

        // Get the parent id.
        let parent_id: i64 = db
            .conn
            .query_row("SELECT id FROM token_savings LIMIT 1", [], |r| r.get(0))
            .unwrap();

        let blocks = db.query_block_decisions(parent_id).unwrap();
        assert_eq!(blocks.len(), 2, "must return 2 block decision rows");
        let sum_in: u64 = blocks.iter().map(|b| b.bytes_in).sum();
        let sum_out: u64 = blocks.iter().map(|b| b.bytes_out).sum();
        assert_eq!(sum_in, 1000, "Σ bytes_in must match input (600+400)");
        assert_eq!(sum_out, 520, "Σ bytes_out must match input (120+400)");
    }

    /// query_by_model — ordering is NULL-last (provider IS NULL, provider,
    /// model IS NULL, model).
    ///
    /// AD-AN-9: identical ordering ensures byte-identical JSON across runs (AC13).
    #[test]
    fn test_query_by_model_ordering_null_last() {
        let (db, _tmp) = test_db();

        // Insert rows in a scrambled order: unknown provider, openai, anthropic.
        insert_proxy_row(
            &db,
            None,
            None,
            Some(100),
            Some(50),
            Some(50.0),
            None,
            "passthrough",
            1_711_300_001,
        );
        insert_proxy_row(
            &db,
            Some("openai"),
            Some("gpt-4o"),
            Some(200),
            Some(100),
            Some(50.0),
            None,
            "full",
            1_711_300_002,
        );
        insert_proxy_row(
            &db,
            Some("anthropic"),
            Some("claude-3-5-sonnet-20241022"),
            Some(300),
            Some(150),
            Some(50.0),
            None,
            "full",
            1_711_300_003,
        );

        let rows = db.query_by_model(None).unwrap();

        // Expected order: anthropic (non-NULL, alphabetic first) → openai → NULL.
        assert_eq!(rows.len(), 3, "must return 3 model rows");
        assert_eq!(
            rows[0].provider.as_deref(),
            Some("anthropic"),
            "first row must be anthropic (NULL-last ordering)"
        );
        assert_eq!(
            rows[1].provider.as_deref(),
            Some("openai"),
            "second row must be openai"
        );
        assert_eq!(rows[2].provider, None, "last row must have NULL provider");
    }

    /// query_by_model — upstream_error_status IS NOT NULL rows are excluded from
    /// token aggregates and tier counts (success-scope only).
    ///
    /// AD-PXY-25: removing the exclusion would inflate token sums and tier
    /// counts, breaking the equality assertions.
    #[test]
    fn test_query_by_model_excludes_upstream_error_rows_from_aggregates() {
        let (db, _tmp) = test_db();

        // One normal relayed row.
        insert_proxy_row(
            &db,
            Some("anthropic"),
            Some("claude-3-5-sonnet-20241022"),
            Some(1000),
            Some(100),
            Some(90.0),
            None,
            "full",
            1_711_300_001,
        );
        // One upstream-errored row for the same model (must NOT count in aggregates).
        insert_proxy_row(
            &db,
            Some("anthropic"),
            Some("claude-3-5-sonnet-20241022"),
            None,
            None,
            None,
            Some(502),
            "full",
            1_711_300_002,
        );

        let rows = db.query_by_model(None).unwrap();
        assert_eq!(rows.len(), 1, "must be 1 model group (same provider+model)");

        let row = &rows[0];
        assert_eq!(
            row.requests, 1,
            "AD-PXY-25: success-scope request count must be 1 (errored row excluded)"
        );
        assert_eq!(
            row.upstream_errors, 1,
            "AD-PXY-25: upstream_errors must be 1"
        );
        assert_eq!(
            row.raw_tokens,
            Some(1000),
            "AD-PXY-25: raw_tokens must reflect only the relayed row"
        );
        // Tier counts should reflect only the relayed row.
        assert!(
            row.tier_full_pct > 99.0,
            "AD-PXY-25: tier_full_pct must be ~100% (from the relayed row only)"
        );
    }

    /// query_by_model — all-NULL-token group reports 0 counted / N uncounted.
    ///
    /// AD-AN-9 / AC12: a group with all NULL tokens shows "0 counted, N uncounted"
    /// rather than implying counts exist.
    #[test]
    fn test_query_by_model_all_null_token_group() {
        let (db, _tmp) = test_db();

        // Three rows with NULL tokens (pair-jointly-NULL).
        for i in 0..3i64 {
            insert_proxy_row(
                &db,
                Some("anthropic"),
                Some("claude-3-opus-20240229"),
                None,
                None,
                None,
                None,
                "passthrough",
                1_711_300_000 + i,
            );
        }

        let rows = db.query_by_model(None).unwrap();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.requests, 3);
        assert_eq!(row.counted_rows, 0, "AC12: no countable rows");
        assert_eq!(row.uncounted_rows, 3, "AC12: all 3 rows are uncounted");
        assert!(row.raw_tokens.is_none(), "AC12: raw_tokens must be None");
        assert!(
            row.compressed_tokens.is_none(),
            "AC12: compressed_tokens must be None"
        );
    }

    /// query_by_provider — mixed-basis detection: openai with gpt-4 (Cl100k)
    /// and gpt-4o (O200k) → basis = "mixed", combined token figure is None.
    ///
    /// AD-AN-9 / AC11: cross-tokenizer sum is never emitted at the provider
    /// level when models use different bases.
    #[test]
    fn test_query_by_provider_mixed_basis_omits_combined_tokens() {
        let (db, _tmp) = test_db();

        // gpt-4 → Cl100k encoding
        insert_proxy_row(
            &db,
            Some("openai"),
            Some("gpt-4"),
            Some(2000),
            Some(400),
            Some(80.0),
            None,
            "full",
            1_711_300_001,
        );
        // gpt-4o → O200k encoding
        insert_proxy_row(
            &db,
            Some("openai"),
            Some("gpt-4o"),
            Some(1000),
            Some(200),
            Some(80.0),
            None,
            "full",
            1_711_300_002,
        );

        let providers = db.query_by_provider(None).unwrap();
        assert_eq!(providers.len(), 1, "must be 1 provider group (openai)");
        let prov = &providers[0];

        assert_eq!(prov.provider.as_deref(), Some("openai"));
        assert_eq!(prov.requests, 2);
        assert_eq!(
            prov.basis, "mixed",
            "AC11: openai spanning cl100k+o200k must be 'mixed'"
        );
        assert!(
            prov.raw_tokens.is_none(),
            "AC11: combined raw_tokens must be None for mixed-basis provider"
        );
        assert!(
            prov.compressed_tokens.is_none(),
            "AC11: combined compressed_tokens must be None for mixed-basis provider"
        );

        // Per-model rows must carry the authoritative figures.
        let models = db.query_by_model(None).unwrap();
        assert_eq!(models.len(), 2, "must have 2 per-model rows");
        let gpt4 = models
            .iter()
            .find(|m| m.model.as_deref() == Some("gpt-4"))
            .unwrap();
        assert_eq!(gpt4.basis, "exact", "gpt-4 (Cl100k) must have basis=exact");
        assert_eq!(gpt4.raw_tokens, Some(2000));
    }

    /// AD-AN-9 — the per-provider `avg_savings_pct` rollup is weighted by each
    /// model's counted-row count, not an unweighted mean of model averages.
    ///
    /// PF-007 discriminator: one `claude-3-opus` request at 90% plus nine
    /// `claude-3-haiku` requests at 0% must roll up to 9%, the true per-request
    /// mean. An unweighted mean of the two model averages yields 45% — five
    /// times the real figure — so reverting the weighting fails this test.
    /// Both models resolve to `AnthropicOffline`, keeping the group single-basis
    /// so the token rollup (and therefore `avg_savings_pct`) is emitted at all.
    #[test]
    fn test_query_by_provider_avg_savings_is_row_weighted() {
        let (db, _tmp) = test_db();

        // One high-savings request.
        insert_proxy_row(
            &db,
            Some("anthropic"),
            Some("claude-3-opus"),
            Some(1000),
            Some(100),
            Some(90.0),
            None,
            "full",
            1_711_400_001,
        );
        // Nine zero-savings requests on a different model in the same family.
        for i in 0..9 {
            insert_proxy_row(
                &db,
                Some("anthropic"),
                Some("claude-3-haiku"),
                Some(1000),
                Some(1000),
                Some(0.0),
                None,
                "passthrough",
                1_711_400_010 + i,
            );
        }

        let providers = db.query_by_provider(None).unwrap();
        assert_eq!(providers.len(), 1, "must be 1 provider group (anthropic)");
        let prov = &providers[0];
        assert_eq!(prov.requests, 10);
        assert_eq!(prov.counted_rows, 10);
        assert_ne!(
            prov.basis, "mixed",
            "both models share AnthropicOffline — the rollup must be emitted"
        );

        let avg = prov
            .avg_savings_pct
            .expect("single-basis provider must carry a combined savings figure");
        assert!(
            (avg - 9.0).abs() < 1e-9,
            "AD-AN-9: provider rollup must be row-weighted (expected 9.0, got {avg}); \
             45.0 indicates an unweighted mean of per-model averages"
        );
    }

    /// AD-AN-6 — `clean_invalid_records` is a pre-#305 CLI repair sweep and must
    /// never touch proxy rows.
    ///
    /// PF-007 discriminator: a proxy row whose transform genuinely expanded the
    /// body (`compressed_tokens > raw_tokens`) is a valid measurement that
    /// AD-AN-7 requires be kept. Dropping the `command_type != 'proxy'` predicate
    /// deletes it — and its detail rows — biasing reported savings upward by
    /// removing precisely the worst outcomes. The CLI row in the same table
    /// proves the sweep still does its original job.
    #[test]
    fn test_clean_invalid_records_preserves_expanded_proxy_rows() {
        let (mut db, _tmp) = test_db();

        // A genuinely expanded proxy row (compressed > raw) plus detail rows.
        let mut input = sample_proxy_input();
        input.raw_tokens = Some(100);
        input.compressed_tokens = Some(140);
        input.savings_pct = Some(-40.0);
        input.blocks = vec![ProxyBlockDecision {
            block_index: 0,
            component: "block-router".to_string(),
            outcome: "full".to_string(),
            bytes_in: 400,
            bytes_out: 560,
        }];
        db.record_proxy(&input).unwrap();

        // A CLI row with the pre-#305 corruption the sweep exists to remove.
        db.conn
            .execute(
                "INSERT INTO token_savings \
                 (timestamp, command_type, original_cmd, raw_tokens, compressed_tokens, \
                  savings_pct, duration_ms, project_path) \
                 VALUES (1711500000, 'file', 'cat x.ts', 100, 400, -300.0, 5, '/tmp')",
                [],
            )
            .unwrap();

        let deleted = db.clean_invalid_records().unwrap();
        assert_eq!(deleted, 1, "only the corrupt CLI row may be deleted");

        let proxy_rows: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM token_savings WHERE command_type = 'proxy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            proxy_rows, 1,
            "AD-AN-6: an expanded proxy row is a valid measurement and must survive"
        );

        let detail_rows: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM proxy_block_decisions", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            detail_rows, 1,
            "the surviving parent must keep its detail rows"
        );

        let cli_rows: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM token_savings WHERE command_type != 'proxy'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cli_rows, 0, "the corrupt CLI row must still be swept");
    }

    /// query_by_upstream_error — counts only rows with upstream_error_status IS NOT NULL.
    ///
    /// AD-PXY-25 / AC25: the errored count is separate from success-scope
    /// request counts and never conflated with normal rows.
    #[test]
    fn test_query_by_upstream_error_counts_correctly() {
        let (db, _tmp) = test_db();

        // One normal row.
        insert_proxy_row(
            &db,
            Some("anthropic"),
            None,
            Some(1000),
            Some(100),
            Some(90.0),
            None,
            "full",
            1_711_300_001,
        );
        // Two errored rows.
        insert_proxy_row(
            &db,
            Some("anthropic"),
            None,
            None,
            None,
            None,
            Some(502),
            "passthrough",
            1_711_300_002,
        );
        insert_proxy_row(
            &db,
            Some("openai"),
            None,
            None,
            None,
            None,
            Some(504),
            "passthrough",
            1_711_300_003,
        );

        let errored = db.query_by_upstream_error(None).unwrap();
        assert_eq!(
            errored, 2,
            "AD-PXY-25: must count exactly 2 upstream-errored rows"
        );
    }

    /// query_proxy_dropped_records — returns 0 when no drops recorded, and the
    /// accumulated value after analytics_meta_add_drop_count calls.
    #[test]
    fn test_query_proxy_dropped_records() {
        let (db, _tmp) = test_db();

        // Initially 0.
        assert_eq!(
            db.query_proxy_dropped_records().unwrap(),
            0,
            "must return 0 when no drops recorded"
        );

        // Record some drops.
        db.analytics_meta_add_drop_count(7).unwrap();
        assert_eq!(
            db.query_proxy_dropped_records().unwrap(),
            7,
            "must return the persisted drop count"
        );

        // Accumulates.
        db.analytics_meta_add_drop_count(3).unwrap();
        assert_eq!(
            db.query_proxy_dropped_records().unwrap(),
            10,
            "must accumulate across calls"
        );
    }

    /// AD-AN-8 — `analytics_meta_add_drop_count` is a monotonic upsert: two
    /// calls with counts 10 and 5 must accumulate to 15, not overwrite.
    ///
    /// Also verifies: a key absent from `analytics_meta` is created on first
    /// call (INSERT path), and subsequent calls UPDATE not INSERT again.
    #[test]
    fn test_analytics_meta_add_drop_count_upsert() {
        use rusqlite::OptionalExtension;
        let (db, _tmp) = test_db();

        // Precondition: key must not exist yet.
        let pre: Option<i64> = db
            .conn
            .query_row(
                "SELECT value FROM analytics_meta WHERE key = 'proxy_dropped_records'",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert!(pre.is_none(), "key must be absent before first call");

        // First call (INSERT path)
        db.analytics_meta_add_drop_count(10).unwrap();
        let after_first: i64 = db
            .conn
            .query_row(
                "SELECT value FROM analytics_meta WHERE key = 'proxy_dropped_records'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after_first, 10, "first call must create key with value 10");

        // Second call (UPDATE path — must accumulate, not overwrite)
        db.analytics_meta_add_drop_count(5).unwrap();
        let after_second: i64 = db
            .conn
            .query_row(
                "SELECT value FROM analytics_meta WHERE key = 'proxy_dropped_records'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            after_second, 15,
            "AD-AN-8: second call must accumulate to 15, not overwrite with 5"
        );

        // Third call with zero — must not reset
        db.analytics_meta_add_drop_count(0).unwrap();
        let after_zero: i64 = db
            .conn
            .query_row(
                "SELECT value FROM analytics_meta WHERE key = 'proxy_dropped_records'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            after_zero, 15,
            "AD-AN-8: adding zero must leave value unchanged"
        );

        // Verify no duplicate rows were created (single key row always)
        let row_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM analytics_meta WHERE key = 'proxy_dropped_records'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(row_count, 1, "upsert must never create duplicate key rows");
    }
}
