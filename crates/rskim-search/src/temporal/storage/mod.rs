//! Temporal database: SQLite-backed persistence for co-change, hotspot, and risk data.
//!
//! Schema defined in [`schema`]. Build via [`TemporalDb::build`]. Query via the
//! `load_*` methods, which return `(PathBuf, f32)` pairs compatible with the
//! `TemporalQuery` trait's API (wired up in Phase 4).
//!
//! # Crash safety
//!
//! [`TemporalDb::build`] runs the entire write phase inside a single SQLite
//! transaction. A process crash during build leaves the database file unchanged
//! (the previous version remains intact).
//!
//! # Reproducibility
//!
//! Paths are inserted in sorted (BTreeSet) order so that `temporal_file_id`
//! values are identical across two builds of the same repo state.
//!
//! # Module layout
//!
//! - [`build_ops`] — build-time helpers (collect paths, insert tables, meta)
//! - [`query`] — read-only load methods on [`TemporalDb`]

mod build_ops;
mod query;
mod schema;

use std::path::Path;

use rusqlite::Connection;

use crate::{Result, SearchError};

pub use schema::SCHEMA_VERSION;

/// Default temporal lookback window in days.
pub const DEFAULT_LOOKBACK_DAYS: u32 = 365;

// ============================================================================
// Meta-table key constants
// ============================================================================

pub(super) mod meta_keys {
    pub const SCHEMA_VERSION: &str = "schema_version";
    pub const LAST_COMMIT_HASH: &str = "last_commit_hash";
    pub const LAST_BUILD_TIMESTAMP: &str = "last_build_timestamp";
    pub const LOOKBACK_DAYS: &str = "lookback_days";
    pub const REPO_ROOT: &str = "repo_root";
    pub const GIX_VERSION: &str = "gix_version";
    pub const COMMITS_ANALYZED: &str = "commits_analyzed";
}

// ============================================================================
// Score kind
// ============================================================================

/// Which score table to query in [`TemporalDb::load_score_for`].
#[derive(Debug, Clone, Copy)]
pub enum ScoreKind {
    Hotspot,
    Risk,
}

// ============================================================================
// TemporalDb
// ============================================================================

/// SQLite-backed store for temporal search signals.
///
/// Opened via [`TemporalDb::open`] (reads existing DB) or [`TemporalDb::build`]
/// (rebuilds from scratch). Query via the `load_*` family of methods.
pub struct TemporalDb {
    pub(super) conn: Connection,
}

impl std::fmt::Debug for TemporalDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TemporalDb").finish_non_exhaustive()
    }
}

impl TemporalDb {
    // -----------------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------------

    /// Open an existing or create a new temporal database at `path`.
    ///
    /// Applies PRAGMAs (WAL, foreign keys, synchronous=NORMAL) and runs schema
    /// migrations. The parent directory of `path` must already exist.
    ///
    /// # Errors
    ///
    /// - [`SearchError::IndexBuildError`] for SQLite failures
    /// - [`SearchError::CorruptedIndex`] if `user_version > SCHEMA_VERSION`
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).map_err(sql_err)?;
        Self::configure_connection(&conn)?;
        Self::migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Build a temporal database from scratch for the given git repo.
    ///
    /// Removes any existing database at `db_path` (including WAL/SHM sidecars)
    /// before starting. The entire write is wrapped in a single transaction for
    /// crash safety: a crash during build leaves the file unmodified.
    ///
    /// # Errors
    ///
    /// - [`SearchError::GitError`] if git history cannot be parsed
    /// - [`SearchError::IndexBuildError`] for SQLite or I/O failures
    pub fn build(repo_path: &Path, db_path: &Path, lookback_days: u32) -> Result<Self> {
        use crate::temporal::{build_cochange_matrix, hotspot_scores, parse_history, risk_scores};

        // Remove existing DB and sidecars so we start clean.
        let _ = std::fs::remove_file(db_path);
        let _ = std::fs::remove_file(db_path.with_extension("db-wal"));
        let _ = std::fs::remove_file(db_path.with_extension("db-shm"));

        let db = Self::open(db_path)?;

        // ── 1. Parse git history ────────────────────────────────────────────
        let commits = parse_history(repo_path, lookback_days)?;

        // ── 2. Compute signals ──────────────────────────────────────────────
        let cochange = build_cochange_matrix(&commits);
        let now_secs = unix_now();
        let hotspots = hotspot_scores(&commits, now_secs);
        let risks = risk_scores(&commits);

        // ── 3. Collect all unique paths in sorted order ─────────────────────
        let all_paths = build_ops::collect_all_paths(&cochange, &hotspots, &risks);

        // ── 4. Write everything in one transaction ───────────────────────────
        let tx = db.conn.unchecked_transaction().map_err(sql_err)?;

        build_ops::insert_file_paths(&tx, &all_paths)?;
        let path_to_id = build_ops::load_path_id_map(&tx)?;

        build_ops::insert_cochange(&tx, &cochange, &path_to_id)?;
        build_ops::insert_hotspots(&tx, &hotspots, &path_to_id)?;
        build_ops::insert_risks(&tx, &risks, &path_to_id)?;
        build_ops::insert_meta(&tx, &commits, now_secs, lookback_days, repo_path)?;

        tx.commit().map_err(sql_err)?;

        Ok(db)
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn configure_connection(conn: &Connection) -> Result<()> {
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(sql_err)?;
        conn.pragma_update(None, "foreign_keys", "ON")
            .map_err(sql_err)?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(sql_err)?;
        Ok(())
    }

    fn migrate(conn: &Connection) -> Result<()> {
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(sql_err)?;

        if version == 0 {
            let tx = conn.unchecked_transaction().map_err(sql_err)?;
            schema::apply_migration_v1(&tx).map_err(sql_err)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(sql_err)?;
            tx.commit().map_err(sql_err)?;
        } else if version > SCHEMA_VERSION {
            return Err(SearchError::CorruptedIndex {
                path: "<temporal.db>".into(),
                reason: format!(
                    "schema version {version} is newer than supported {SCHEMA_VERSION}"
                ),
            });
        }
        Ok(())
    }
}

// ============================================================================
// Shared utilities (used by both build_ops and query submodules)
// ============================================================================

/// Convert a repo-relative path to a forward-slash string for storage.
///
/// All platforms store paths with forward slashes for portability.
pub(super) fn path_to_string(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Current time as Unix epoch seconds. Falls back to 0 on platform error.
pub(super) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Convert a `rusqlite::Error` into a [`SearchError::IndexBuildError`].
pub(super) fn sql_err(e: rusqlite::Error) -> SearchError {
    SearchError::IndexBuildError(format!("sqlite: {e}"))
}
