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
use std::time::Duration;

use rusqlite::Connection;
use serde::Serialize;

use crate::tokens;

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
    pub(crate) const PREMIUM: Self = Self {
        input_cost_per_mtok: 15.0,
        tier_name: "Premium",
    };

    pub(crate) fn all_tiers() -> [Self; 3] {
        [Self::ECONOMY, Self::STANDARD, Self::PREMIUM]
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
#[derive(Debug, Clone, Copy)]
pub(crate) struct AnalyticsConfig {
    pub enabled: bool,
    pub input_cost_per_mtok: Option<f64>,
}

impl AnalyticsConfig {
    /// Read process env once at the system boundary.
    ///
    /// `cli_disable` is the value of `--disable-analytics` from CLI parsing.
    /// Call this in main(), then thread the result down to all callers.
    pub fn from_process(cli_disable: bool) -> Self {
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
    pub(crate) fn open_default() -> anyhow::Result<Self> {
        let path = if let Ok(override_path) = std::env::var("SKIM_ANALYTICS_DB") {
            PathBuf::from(override_path)
        } else {
            crate::cache::get_cache_dir()?.join("analytics.db")
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
            "INSERT INTO token_savings (timestamp, command_type, original_cmd, raw_tokens, compressed_tokens, savings_pct, duration_ms, project_path, mode, language, parse_tier)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
            ],
        )?;
        Ok(())
    }

    /// Query aggregate summary.
    pub(crate) fn query_summary(&self, since: Option<i64>) -> anyhow::Result<AnalyticsSummary> {
        let (where_clause, params) = since_clause(since);
        let sql = format!(
            "SELECT COUNT(*), COALESCE(SUM(raw_tokens), 0), COALESCE(SUM(compressed_tokens), 0), COALESCE(AVG(savings_pct), 0) FROM token_savings {where_clause}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let row = stmt.query_row(rusqlite::params_from_iter(params), |row| {
            let invocations: u64 = row.get(0)?;
            let raw_tokens: i64 = row.get(1)?;
            let compressed_tokens: i64 = row.get(2)?;
            let avg_savings_pct: f64 = row.get(3)?;
            Ok(AnalyticsSummary {
                invocations,
                raw_tokens: raw_tokens as u64,
                compressed_tokens: compressed_tokens as u64,
                tokens_saved: (raw_tokens - compressed_tokens).max(0) as u64,
                avg_savings_pct,
            })
        })?;
        Ok(row)
    }

    /// Query daily breakdown.
    pub(crate) fn query_daily(&self, since: Option<i64>) -> anyhow::Result<Vec<DailyStats>> {
        let (where_clause, params) = since_clause(since);
        let sql = format!(
            "SELECT date(timestamp, 'unixepoch') as day, COUNT(*), COALESCE(SUM(raw_tokens - compressed_tokens), 0), COALESCE(AVG(savings_pct), 0) FROM token_savings {where_clause} GROUP BY day ORDER BY day DESC"
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
            "SELECT command_type, COUNT(*), COALESCE(SUM(raw_tokens - compressed_tokens), 0), COALESCE(AVG(savings_pct), 0), COALESCE(AVG(duration_ms), 0.0) FROM token_savings {where_clause} GROUP BY command_type ORDER BY SUM(raw_tokens - compressed_tokens) DESC"
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
            "SELECT language, COUNT(*), COALESCE(SUM(raw_tokens - compressed_tokens), 0), COALESCE(AVG(savings_pct), 0) FROM token_savings {clause} GROUP BY language ORDER BY SUM(raw_tokens - compressed_tokens) DESC"
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
            "SELECT mode, COUNT(*), COALESCE(SUM(raw_tokens - compressed_tokens), 0), COALESCE(AVG(savings_pct), 0) FROM token_savings {clause} GROUP BY mode ORDER BY SUM(raw_tokens - compressed_tokens) DESC"
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

    /// Prune records older than N days.
    pub(crate) fn prune_older_than(&self, days: u64) -> anyhow::Result<usize> {
        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
            - (days as i64 * 86400);
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

        if now as i64 - last_prune > 86400 && self.prune_older_than(90).is_ok() {
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
// Fire-and-forget recording functions
// ============================================================================

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
fn register_thread(handle: std::thread::JoinHandle<()>) {
    if let Ok(mut handles) = PENDING_THREADS.lock() {
        handles.push(handle);
    }
}

/// Join all pending analytics threads.
///
/// Call from `main()` before returning `ExitCode`. This ensures all
/// background analytics DB writes complete before the process exits —
/// without this, short-lived commands may terminate before the thread
/// finishes writing to SQLite.
pub(crate) fn flush_pending() {
    if let Ok(mut handles) = PENDING_THREADS.lock() {
        for handle in handles.drain(..) {
            let _ = handle.join();
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
/// so removing the redundant parameter here keeps the argument count at 7.
fn record_fire_and_forget(
    raw_text: String,
    compressed_text: String,
    original_cmd: String,
    command_type: CommandType,
    duration: Duration,
    project_path: String,
    parse_tier: Option<String>,
) {
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
            compressed_tokens: comp_tokens.min(raw_tokens),
            savings_pct: savings_percentage(raw_tokens, comp_tokens),
            duration_ms: duration.as_millis() as u64,
            project_path,
            mode: None,
            language: None,
            parse_tier,
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
// Convenience helpers for subcommand call sites
// ============================================================================

/// Record command output analytics with enabled-check and cwd detection.
///
/// Reduces the 12-15 line inline pattern at each subcommand call site to a
/// single function call. Token counting is deferred to a background thread.
pub(crate) fn try_record_command(
    enabled: bool,
    raw_text: String,
    compressed_text: String,
    original_cmd: String,
    command_type: CommandType,
    duration: Duration,
    parse_tier: Option<&str>,
) {
    if !enabled {
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
        command_type,
        duration,
        cwd,
        parse_tier.map(str::to_string),
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
pub(crate) fn try_record_command_with_counts(
    enabled: bool,
    raw_tokens: usize,
    compressed_tokens: usize,
    original_cmd: String,
    command_type: CommandType,
    duration: Duration,
    parse_tier: Option<&str>,
) {
    if !enabled {
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
            command_type,
            original_cmd,
            raw_tokens,
            compressed_tokens: compressed_tokens.min(raw_tokens),
            savings_pct: savings_percentage(raw_tokens, compressed_tokens),
            duration_ms: duration.as_millis() as u64,
            project_path: cwd,
            mode: None,
            language: None,
            parse_tier: parse_tier.map(str::to_string),
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
            stored.is_ascii() || stored.chars().all(|_| true),
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
        assert_eq!(tiers.len(), 3);
        assert_eq!(tiers[0].tier_name, "Economy");
        assert_eq!(tiers[0].input_cost_per_mtok, 1.0);
        assert_eq!(tiers[1].tier_name, "Standard");
        assert_eq!(tiers[1].input_cost_per_mtok, 3.0);
        assert_eq!(tiers[2].tier_name, "Premium");
        assert_eq!(tiers[2].input_cost_per_mtok, 15.0);
    }

    #[test]
    fn test_pricing_default_is_standard() {
        let p = PricingModel::default_pricing();
        assert_eq!(p.tier_name, "Standard");
        assert_eq!(p.input_cost_per_mtok, 3.0);
    }
}
