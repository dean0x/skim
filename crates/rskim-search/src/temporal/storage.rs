//! SQLite persistence layer for temporal risk data.
//!
//! # Architecture
//!
//! [`TemporalDb`] wraps a single SQLite connection (WAL mode) and owns four
//! tables: `hotspot`, `risk`, `cochange`, and `meta`. All mutations go through
//! [`TemporalDb::sync`], which atomically replaces all four tables in a single
//! transaction so readers never see a partially-refreshed state.
//!
//! Schema migrations are version-gated by SQLite's `PRAGMA user_version`. A
//! forward-compat guard rejects databases created by a future schema version
//! to prevent silent data corruption.
//!
//! # Error handling
//!
//! All rusqlite errors are converted to [`SearchError::Database`] via the
//! private `db_err` helper so no rusqlite types leak into the public API.
//!
//! # Module layout
//!
//! - `storage_types` — row types ([`HotspotRow`], [`RiskRow`], [`CochangeRow`])
//! - `storage_ops`   — store / load / sync `impl` block for [`TemporalDb`]

#[path = "storage_types.rs"]
mod storage_types;

// Re-export row types so callers can import them from `storage::*`.
pub use storage_types::{CochangeRow, HotspotRow, RiskRow};

// storage_ops provides additional `impl TemporalDb` methods (store/load/sync).
#[path = "storage_ops.rs"]
mod storage_ops;

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::types::{Result, SearchError};

// ============================================================================
// Schema version
// ============================================================================

/// Current schema version. Must be bumped whenever the DDL changes.
const CURRENT_VERSION: i64 = 1;

// ============================================================================
// Meta key constants
// ============================================================================

/// Key storing the Unix epoch timestamp (seconds) of the last successful [`TemporalDb::sync`].
pub const META_LAST_UPDATED: &str = "last_updated";

/// Key storing the git HEAD SHA at the time of the last [`TemporalDb::sync`].
pub const META_GIT_HEAD: &str = "git_head";

// ============================================================================
// Error helper
// ============================================================================

/// Convert a rusqlite error into [`SearchError::Database`].
///
/// Visible to the storage sub-modules — not part of the public API.
#[inline]
pub(super) fn db_err(e: impl std::fmt::Display) -> SearchError {
    SearchError::Database(e.to_string())
}

// ============================================================================
// Migrations
// ============================================================================

/// Create all tables and bump `user_version` to [`CURRENT_VERSION`].
///
/// Each version block is guarded by `version < N` so migrations are idempotent
/// when the database is re-opened after an earlier run.
///
/// # Forward-compat guard
///
/// If the database was created by a **future** version of this code
/// (`version > CURRENT_VERSION`), the function returns an error rather than
/// silently corrupting the newer schema.
fn run_migrations(conn: &Connection) -> Result<()> {
    let version: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(db_err)?;

    if version > CURRENT_VERSION {
        return Err(SearchError::Database(format!(
            "database schema version {version} is newer than supported version \
             {CURRENT_VERSION}; upgrade rskim-search to open this database"
        )));
    }

    if version < 1 {
        conn.execute_batch(
            "BEGIN;

            CREATE TABLE IF NOT EXISTS hotspot (
                file_path  TEXT    PRIMARY KEY,
                score      REAL    NOT NULL,
                changes_30d INTEGER NOT NULL,
                changes_90d INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS risk (
                file_path    TEXT    PRIMARY KEY,
                risk_score   REAL    NOT NULL,
                total_commits INTEGER NOT NULL,
                fix_commits   INTEGER NOT NULL,
                fix_density  REAL    NOT NULL
            );

            CREATE TABLE IF NOT EXISTS cochange (
                file_a  TEXT NOT NULL,
                file_b  TEXT NOT NULL,
                count   INTEGER NOT NULL,
                jaccard REAL    NOT NULL,
                PRIMARY KEY (file_a, file_b)
            );

            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            PRAGMA user_version = 1;

            COMMIT;",
        )
        .map_err(db_err)?;
    }

    Ok(())
}

// ============================================================================
// TemporalDb
// ============================================================================

/// SQLite persistence layer for temporal risk scores, co-change pairs, and
/// associated metadata.
///
/// # Thread safety
///
/// `TemporalDb` is not `Sync` — each thread should open its own connection.
/// For concurrent read access, open multiple `TemporalDb` instances pointing at
/// the same WAL-mode database file.
pub struct TemporalDb {
    pub(super) conn: Connection,
}

impl std::fmt::Debug for TemporalDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TemporalDb")
            .field("path", &"<sqlite connection>")
            .finish()
    }
}

impl TemporalDb {
    /// Open (or create) a temporal database at `db_path`.
    ///
    /// 1. Opens the SQLite file (creating it if absent).
    /// 2. Sets Unix file permissions to `0o600` on Unix targets.
    /// 3. Configures a 5-second busy timeout.
    /// 4. Enables WAL journal mode.
    /// 5. Runs schema migrations.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] if the file cannot be opened, the
    /// WAL pragma fails, or the migrations fail (including forward-compat guard).
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path).map_err(db_err)?;

        // Restrict file permissions to owner-only on Unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(db_path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                if let Err(e) = std::fs::set_permissions(db_path, perms) {
                    eprintln!(
                        "[skim-search] warning: could not restrict database permissions to 0o600: {e}"
                    );
                }
            }
        }

        conn.busy_timeout(Duration::from_millis(5_000))
            .map_err(db_err)?;

        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .map_err(db_err)?;
        if journal_mode.to_lowercase() != "wal" {
            return Err(SearchError::Database(format!(
                "failed to enable WAL mode; journal_mode is '{journal_mode}'"
            )));
        }
        conn.execute_batch("PRAGMA synchronous=NORMAL;")
            .map_err(db_err)?;

        run_migrations(&conn)?;

        Ok(Self { conn })
    }

    // ========================================================================
    // Schema introspection
    // ========================================================================

    /// Return the current `PRAGMA user_version` of the open database.
    ///
    /// Primarily used in tests to verify that migrations ran correctly.
    #[must_use = "schema_version returns a Result; check the version or propagate the error"]
    pub fn schema_version(&self) -> Result<i64> {
        self.conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(db_err)
    }
}

// ============================================================================
// Co-located tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[path = "storage_tests.rs"]
mod tests;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[path = "storage_perf_tests.rs"]
mod perf_tests;
