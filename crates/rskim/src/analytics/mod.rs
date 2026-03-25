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
use std::sync::atomic::{AtomicBool, Ordering};
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
}

impl CommandType {
    fn as_str(&self) -> &'static str {
        match self {
            CommandType::File => "file",
            CommandType::Test => "test",
            CommandType::Build => "build",
            CommandType::Git => "git",
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

#[derive(Debug)]
pub(crate) struct PricingModel {
    pub(crate) input_cost_per_mtok: f64,
    pub(crate) model_name: &'static str,
}

impl PricingModel {
    pub(crate) fn default_pricing() -> Self {
        Self {
            input_cost_per_mtok: 3.0,
            model_name: "claude-sonnet-4-6",
        }
    }

    pub(crate) fn from_env_or_default() -> Self {
        if let Ok(val) = std::env::var("SKIM_INPUT_COST_PER_MTOK") {
            if let Ok(cost) = val.parse::<f64>() {
                if cost.is_finite() && cost >= 0.0 {
                    return Self {
                        input_cost_per_mtok: cost,
                        model_name: "custom",
                    };
                }
            }
        }
        Self::default_pricing()
    }

    pub(crate) fn estimate_savings(&self, tokens_saved: u64) -> f64 {
        tokens_saved as f64 / 1_000_000.0 * self.input_cost_per_mtok
    }
}

// ============================================================================
// Analytics enabled check
// ============================================================================

/// Process-wide flag to disable analytics without mutating environment
/// variables. Set via [`force_disable_analytics`] at startup, before any
/// background threads are spawned. Checked by [`is_analytics_enabled`].
static ANALYTICS_FORCE_DISABLED: AtomicBool = AtomicBool::new(false);

/// Disable analytics for the lifetime of this process.
///
/// Thread-safe alternative to `std::env::set_var("SKIM_DISABLE_ANALYTICS", "1")`.
/// Call this early in `main()` when `--disable-analytics` is detected.
pub(crate) fn force_disable_analytics() {
    ANALYTICS_FORCE_DISABLED.store(true, Ordering::Relaxed);
}

/// Check if analytics recording is enabled.
///
/// Returns `false` when:
/// - [`force_disable_analytics`] has been called (e.g., `--disable-analytics` flag), OR
/// - `SKIM_DISABLE_ANALYTICS` env var is set to a truthy value
///   (`1`, `true`, or `yes`, case-insensitive).
///
/// Any other value (including `0`, `false`, `no`) keeps analytics enabled.
/// Unsetting the variable also keeps analytics enabled (the default).
pub(crate) fn is_analytics_enabled() -> bool {
    if ANALYTICS_FORCE_DISABLED.load(Ordering::Relaxed) {
        return false;
    }
    match std::env::var("SKIM_DISABLE_ANALYTICS") {
        Ok(val) => !matches!(val.to_lowercase().as_str(), "1" | "true" | "yes"),
        Err(_) => true,
    }
}

// ============================================================================
// AnalyticsStore trait
// ============================================================================

/// Trait abstracting analytics query operations for testability.
///
/// `AnalyticsDb` implements this trait directly. Test code can provide a
/// `MockStore` without requiring a real SQLite database.
pub(crate) trait AnalyticsStore {
    fn query_summary(&self, since: Option<i64>) -> anyhow::Result<AnalyticsSummary>;
    fn query_daily(&self, since: Option<i64>) -> anyhow::Result<Vec<DailyStats>>;
    fn query_by_command(&self, since: Option<i64>) -> anyhow::Result<Vec<CommandStats>>;
    fn query_by_language(&self, since: Option<i64>) -> anyhow::Result<Vec<LanguageStats>>;
    fn query_by_mode(&self, since: Option<i64>) -> anyhow::Result<Vec<ModeStats>>;
    fn query_tier_distribution(&self, since: Option<i64>) -> anyhow::Result<TierDistribution>;
    fn clear(&self) -> anyhow::Result<()>;
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
            &r.original_cmd[..Self::MAX_CMD_LEN]
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
                tokens_saved: row.get::<_, i64>(2)? as u64,
                avg_savings_pct: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Query breakdown by command type.
    pub(crate) fn query_by_command(&self, since: Option<i64>) -> anyhow::Result<Vec<CommandStats>> {
        let (where_clause, params) = since_clause(since);
        let sql = format!(
            "SELECT command_type, COUNT(*), COALESCE(SUM(raw_tokens - compressed_tokens), 0), COALESCE(AVG(savings_pct), 0) FROM token_savings {where_clause} GROUP BY command_type ORDER BY SUM(raw_tokens - compressed_tokens) DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok(CommandStats {
                command_type: row.get(0)?,
                invocations: row.get(1)?,
                tokens_saved: row.get::<_, i64>(2)? as u64,
                avg_savings_pct: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Query breakdown by language (file operations only).
    pub(crate) fn query_by_language(&self, since: Option<i64>) -> anyhow::Result<Vec<LanguageStats>> {
        let (clause, params) = since_clause_with_extra(since, "language IS NOT NULL");
        let sql = format!(
            "SELECT language, COUNT(*), COALESCE(SUM(raw_tokens - compressed_tokens), 0), COALESCE(AVG(savings_pct), 0) FROM token_savings {clause} GROUP BY language ORDER BY SUM(raw_tokens - compressed_tokens) DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            Ok(LanguageStats {
                language: row.get(0)?,
                files: row.get(1)?,
                tokens_saved: row.get::<_, i64>(2)? as u64,
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
                tokens_saved: row.get::<_, i64>(2)? as u64,
                avg_savings_pct: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Query parse tier distribution (command operations only).
    pub(crate) fn query_tier_distribution(&self, since: Option<i64>) -> anyhow::Result<TierDistribution> {
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

    /// Delete all analytics data.
    fn clear_data(&self) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM token_savings", [])?;
        Ok(())
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
        self.clear_data()
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

/// Compute token savings as a percentage (0.0 when raw_tokens is zero).
pub(crate) fn savings_percentage(raw_tokens: usize, compressed_tokens: usize) -> f32 {
    if raw_tokens == 0 {
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

/// Persist a record to the default database, with auto-pruning.
fn persist_record(record: &TokenSavingsRecord) {
    if let Ok(db) = AnalyticsDb::open_default() {
        let _ = db.record(record);
        db.maybe_prune();
    }
}

/// Record command output token savings. Defers token counting to background thread.
/// Check is_analytics_enabled() BEFORE cloning strings.
pub(crate) fn record_fire_and_forget(
    raw_text: String,
    compressed_text: String,
    original_cmd: String,
    command_type: CommandType,
    duration: Duration,
    project_path: String,
    parse_tier: Option<String>,
) {
    if !is_analytics_enabled() {
        return;
    }
    std::thread::spawn(move || {
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
        };
        persist_record(&record);
    });
}

/// Record file operation token savings where counts are already known.
///
/// Accepts a fully-constructed [`TokenSavingsRecord`] and persists it on
/// a background thread. The `timestamp` and `savings_pct` fields should
/// be populated by the caller (use [`now_unix_secs`] and
/// [`savings_percentage`] helpers).
pub(crate) fn record_with_counts(record: TokenSavingsRecord) {
    if !is_analytics_enabled() {
        return;
    }
    std::thread::spawn(move || {
        persist_record(&record);
    });
}

// ============================================================================
// Convenience helpers for subcommand call sites
// ============================================================================

/// Record command output analytics with automatic enabled-check and cwd detection.
///
/// Reduces the 12-15 line inline pattern at each subcommand call site to a
/// single function call. Token counting is deferred to a background thread.
pub(crate) fn try_record_command(
    raw_text: String,
    compressed_text: String,
    original_cmd: String,
    command_type: CommandType,
    duration: Duration,
    parse_tier: Option<&str>,
) {
    if !is_analytics_enabled() {
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
        parse_tier.map(|s| s.to_string()),
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
#[allow(dead_code)]
pub(crate) fn try_record_command_with_counts(
    raw_tokens: usize,
    compressed_tokens: usize,
    original_cmd: String,
    command_type: CommandType,
    duration: Duration,
    parse_tier: Option<&str>,
) {
    if !is_analytics_enabled() {
        return;
    }
    let cwd = std::env::current_dir()
        .unwrap_or_default()
        .display()
        .to_string();
    record_with_counts(TokenSavingsRecord {
        timestamp: now_unix_secs(),
        command_type,
        original_cmd,
        raw_tokens,
        compressed_tokens,
        savings_pct: savings_percentage(raw_tokens, compressed_tokens),
        duration_ms: duration.as_millis() as u64,
        project_path: cwd,
        mode: None,
        language: None,
        parse_tier: parse_tier.map(|s| s.to_string()),
    });
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
        assert_eq!(p.model_name, "claude-sonnet-4-6");
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
    // is_analytics_enabled() tests
    // ========================================================================

    /// Run a closure with `SKIM_DISABLE_ANALYTICS` set to the given value,
    /// then restore the original environment. Uses a mutex to prevent
    /// concurrent env-var mutations from interfering between tests.
    fn with_env_var(value: Option<&str>, f: impl FnOnce()) {
        use std::sync::Mutex;
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap();

        let prev = std::env::var("SKIM_DISABLE_ANALYTICS").ok();
        match value {
            Some(v) => std::env::set_var("SKIM_DISABLE_ANALYTICS", v),
            None => std::env::remove_var("SKIM_DISABLE_ANALYTICS"),
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        match prev {
            Some(v) => std::env::set_var("SKIM_DISABLE_ANALYTICS", v),
            None => std::env::remove_var("SKIM_DISABLE_ANALYTICS"),
        }
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    fn test_analytics_enabled_when_env_unset() {
        with_env_var(None, || {
            assert!(
                is_analytics_enabled(),
                "analytics should be enabled when SKIM_DISABLE_ANALYTICS is unset"
            );
        });
    }

    #[test]
    fn test_analytics_disabled_with_value_1() {
        with_env_var(Some("1"), || {
            assert!(
                !is_analytics_enabled(),
                "analytics should be disabled when SKIM_DISABLE_ANALYTICS=1"
            );
        });
    }

    #[test]
    fn test_analytics_disabled_with_value_true() {
        with_env_var(Some("true"), || {
            assert!(
                !is_analytics_enabled(),
                "analytics should be disabled when SKIM_DISABLE_ANALYTICS=true"
            );
        });
    }

    #[test]
    fn test_analytics_disabled_with_value_yes() {
        with_env_var(Some("yes"), || {
            assert!(
                !is_analytics_enabled(),
                "analytics should be disabled when SKIM_DISABLE_ANALYTICS=yes"
            );
        });
    }

    #[test]
    fn test_analytics_disabled_case_insensitive() {
        with_env_var(Some("TRUE"), || {
            assert!(
                !is_analytics_enabled(),
                "analytics should be disabled when SKIM_DISABLE_ANALYTICS=TRUE (case-insensitive)"
            );
        });
    }

    #[test]
    fn test_analytics_enabled_with_value_0() {
        with_env_var(Some("0"), || {
            assert!(
                is_analytics_enabled(),
                "analytics should remain enabled when SKIM_DISABLE_ANALYTICS=0"
            );
        });
    }

    #[test]
    fn test_analytics_enabled_with_value_false() {
        with_env_var(Some("false"), || {
            assert!(
                is_analytics_enabled(),
                "analytics should remain enabled when SKIM_DISABLE_ANALYTICS=false"
            );
        });
    }

    #[test]
    fn test_analytics_enabled_with_value_no() {
        with_env_var(Some("no"), || {
            assert!(
                is_analytics_enabled(),
                "analytics should remain enabled when SKIM_DISABLE_ANALYTICS=no"
            );
        });
    }

    #[test]
    fn test_analytics_enabled_with_empty_string() {
        with_env_var(Some(""), || {
            assert!(
                is_analytics_enabled(),
                "analytics should remain enabled when SKIM_DISABLE_ANALYTICS is empty"
            );
        });
    }

    // ========================================================================
    // Pricing validation tests
    // ========================================================================

    /// Run a closure with `SKIM_INPUT_COST_PER_MTOK` set to the given value,
    /// then restore the original environment. Uses the same mutex as
    /// `with_env_var` to prevent concurrent env-var mutations.
    fn with_cost_env_var(value: Option<&str>, f: impl FnOnce()) {
        use std::sync::Mutex;
        static COST_ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = COST_ENV_LOCK.lock().unwrap();

        let prev = std::env::var("SKIM_INPUT_COST_PER_MTOK").ok();
        match value {
            Some(v) => std::env::set_var("SKIM_INPUT_COST_PER_MTOK", v),
            None => std::env::remove_var("SKIM_INPUT_COST_PER_MTOK"),
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        match prev {
            Some(v) => std::env::set_var("SKIM_INPUT_COST_PER_MTOK", v),
            None => std::env::remove_var("SKIM_INPUT_COST_PER_MTOK"),
        }
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    fn test_pricing_negative_falls_back_to_default() {
        with_cost_env_var(Some("-5"), || {
            let p = PricingModel::from_env_or_default();
            assert_eq!(
                p.input_cost_per_mtok, 3.0,
                "negative cost should fall back to default"
            );
            assert_eq!(p.model_name, "claude-sonnet-4-6");
        });
    }

    #[test]
    fn test_pricing_zero_is_valid() {
        with_cost_env_var(Some("0"), || {
            let p = PricingModel::from_env_or_default();
            assert_eq!(
                p.input_cost_per_mtok, 0.0,
                "zero cost should be accepted"
            );
            assert_eq!(p.model_name, "custom");
        });
    }

    #[test]
    fn test_pricing_infinity_falls_back_to_default() {
        with_cost_env_var(Some("inf"), || {
            let p = PricingModel::from_env_or_default();
            assert_eq!(
                p.input_cost_per_mtok, 3.0,
                "infinite cost should fall back to default"
            );
            assert_eq!(p.model_name, "claude-sonnet-4-6");
        });
    }

    #[test]
    fn test_pricing_nan_falls_back_to_default() {
        with_cost_env_var(Some("NaN"), || {
            let p = PricingModel::from_env_or_default();
            assert_eq!(
                p.input_cost_per_mtok, 3.0,
                "NaN cost should fall back to default"
            );
            assert_eq!(p.model_name, "claude-sonnet-4-6");
        });
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
        assert_eq!(count, 1, "analytics_meta table should be created by migration");
    }

    // ========================================================================
    // force_disable_analytics / AtomicBool tests
    // ========================================================================

    #[test]
    fn test_force_disable_analytics_disables() {
        // Reset state (tests share the process-wide atomic)
        ANALYTICS_FORCE_DISABLED.store(false, Ordering::Relaxed);

        // Should be enabled by default (assuming env var not set by another test)
        with_env_var(None, || {
            assert!(is_analytics_enabled(), "should be enabled before force_disable");
            force_disable_analytics();
            assert!(!is_analytics_enabled(), "should be disabled after force_disable");
        });

        // Reset to not pollute other tests
        ANALYTICS_FORCE_DISABLED.store(false, Ordering::Relaxed);
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
}
