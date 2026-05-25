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

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};

use crate::types::{Result, SearchError};

// ============================================================================
// Schema version
// ============================================================================

/// Current schema version. Must be bumped whenever the DDL changes.
const CURRENT_VERSION: i64 = 1;

// ============================================================================
// Meta key constants
// ============================================================================

/// Key storing the ISO-8601 UTC timestamp of the last successful [`TemporalDb::sync`].
pub const META_LAST_UPDATED: &str = "last_updated";

/// Key storing the git HEAD SHA at the time of the last [`TemporalDb::sync`].
pub const META_GIT_HEAD: &str = "git_head";

// ============================================================================
// Row types
// ============================================================================

/// A row from the `hotspot` table.
#[derive(Debug, Clone, PartialEq)]
pub struct HotspotRow {
    /// Repository-root-relative file path.
    pub file_path: String,
    /// Decay-weighted commit frequency, max-normalized to `[0.0, 1.0]`.
    pub score: f64,
    /// Raw commit count within the last 30 days.
    pub changes_30d: i64,
    /// Raw commit count within the last 90 days.
    pub changes_90d: i64,
}

/// A row from the `risk` table.
#[derive(Debug, Clone, PartialEq)]
pub struct RiskRow {
    /// Repository-root-relative file path.
    pub file_path: String,
    /// Bug-fix density score in `[0.0, 1.0]`.
    pub risk_score: f64,
    /// Total number of commits touching this file.
    pub total_commits: i64,
    /// Number of commits classified as fix commits.
    pub fix_commits: i64,
    /// Ratio of fix commits to total commits, in `[0.0, 1.0]`.
    pub fix_density: f64,
}

/// A row from the `cochange` table.
#[derive(Debug, Clone, PartialEq)]
pub struct CochangeRow {
    /// Repository-root-relative path of the first file in the pair (lexically smaller).
    pub file_a: String,
    /// Repository-root-relative path of the second file in the pair.
    pub file_b: String,
    /// Number of commits that touched both files.
    pub count: i64,
    /// Jaccard similarity of the two files' commit sets.
    pub jaccard: f64,
}

// ============================================================================
// Error helper
// ============================================================================

/// Convert a rusqlite error into [`SearchError::Database`].
///
/// Private to this module — rusqlite types must not leak into the public API.
fn db_err(e: rusqlite::Error) -> SearchError {
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
            "CREATE TABLE IF NOT EXISTS hotspot (
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

            PRAGMA user_version = 1;",
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
    conn: Connection,
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
                let _ = std::fs::set_permissions(db_path, perms);
            }
        }

        conn.busy_timeout(Duration::from_millis(5_000))
            .map_err(db_err)?;

        conn.execute_batch("PRAGMA journal_mode=WAL;")
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

    // ========================================================================
    // Individual store methods
    // ========================================================================

    /// Replace all rows in the `hotspot` table with `rows`.
    ///
    /// Runs DELETE + batch INSERT in a single transaction. An empty `rows`
    /// slice leaves the table empty after the call.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    pub fn store_hotspots(&self, rows: &[HotspotRow]) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        tx.execute("DELETE FROM hotspot", []).map_err(db_err)?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT INTO hotspot (file_path, score, changes_30d, changes_90d)
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(db_err)?;
            for row in rows {
                stmt.execute(params![row.file_path, row.score, row.changes_30d, row.changes_90d])
                    .map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)
    }

    /// Replace all rows in the `risk` table with `rows`.
    ///
    /// Runs DELETE + batch INSERT in a single transaction.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    pub fn store_risks(&self, rows: &[RiskRow]) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        tx.execute("DELETE FROM risk", []).map_err(db_err)?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT INTO risk (file_path, risk_score, total_commits, fix_commits, fix_density)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(db_err)?;
            for row in rows {
                stmt.execute(params![
                    row.file_path,
                    row.risk_score,
                    row.total_commits,
                    row.fix_commits,
                    row.fix_density
                ])
                .map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)
    }

    /// Replace all rows in the `cochange` table with `rows`.
    ///
    /// Runs DELETE + batch INSERT in a single transaction.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    pub fn store_cochanges(&self, rows: &[CochangeRow]) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        tx.execute("DELETE FROM cochange", []).map_err(db_err)?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT INTO cochange (file_a, file_b, count, jaccard)
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(db_err)?;
            for row in rows {
                stmt.execute(params![row.file_a, row.file_b, row.count, row.jaccard])
                    .map_err(db_err)?;
            }
        }
        tx.commit().map_err(db_err)
    }

    /// Insert or replace a single key-value pair in the `meta` table.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .map_err(db_err)?;
        Ok(())
    }

    // ========================================================================
    // Load methods
    // ========================================================================

    /// Load all rows from the `hotspot` table.
    ///
    /// Returns an empty `Vec` when the table is empty.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    #[must_use = "load_hotspots returns a Result; use or propagate the rows"]
    pub fn load_hotspots(&self) -> Result<Vec<HotspotRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT file_path, score, changes_30d, changes_90d FROM hotspot")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(HotspotRow {
                    file_path: row.get(0)?,
                    score: row.get(1)?,
                    changes_30d: row.get(2)?,
                    changes_90d: row.get(3)?,
                })
            })
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    /// Load all rows from the `risk` table.
    ///
    /// Returns an empty `Vec` when the table is empty.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    #[must_use = "load_risks returns a Result; use or propagate the rows"]
    pub fn load_risks(&self) -> Result<Vec<RiskRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT file_path, risk_score, total_commits, fix_commits, fix_density FROM risk",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(RiskRow {
                    file_path: row.get(0)?,
                    risk_score: row.get(1)?,
                    total_commits: row.get(2)?,
                    fix_commits: row.get(3)?,
                    fix_density: row.get(4)?,
                })
            })
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    /// Load all rows from the `cochange` table.
    ///
    /// Returns an empty `Vec` when the table is empty.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    #[must_use = "load_cochanges returns a Result; use or propagate the rows"]
    pub fn load_cochanges(&self) -> Result<Vec<CochangeRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT file_a, file_b, count, jaccard FROM cochange")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(CochangeRow {
                    file_a: row.get(0)?,
                    file_b: row.get(1)?,
                    count: row.get(2)?,
                    jaccard: row.get(3)?,
                })
            })
            .map_err(db_err)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    /// Retrieve a single value from the `meta` table by key.
    ///
    /// Returns `Ok(None)` when the key is absent.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure other than
    /// `QueryReturnedNoRows`.
    #[must_use = "get_meta returns a Result; check the value or propagate the error"]
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        match self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", params![key], |row| {
                row.get(0)
            }) {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }

    // ========================================================================
    // Atomic multi-table sync
    // ========================================================================

    /// Atomically replace all temporal data in a single transaction.
    ///
    /// Writes `hotspots`, `risks`, and `cochanges` via DELETE + INSERT and
    /// updates the `meta` table with `git_head` and the current UTC timestamp
    /// under [`META_LAST_UPDATED`]. All four operations are wrapped in one
    /// transaction: either all succeed or none are committed.
    ///
    /// # Parameters
    ///
    /// - `hotspots`: Rows to store in the `hotspot` table.
    /// - `risks`: Rows to store in the `risk` table.
    /// - `cochanges`: Rows to store in the `cochange` table.
    /// - `git_head`: The git HEAD SHA (or any string identifier) to record in
    ///   the `meta` table under [`META_GIT_HEAD`].
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure. On error the
    /// transaction is rolled back and the database is left unchanged.
    pub fn sync(
        &self,
        hotspots: &[HotspotRow],
        risks: &[RiskRow],
        cochanges: &[CochangeRow],
        git_head: &str,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;

        // ---- hotspot ----
        tx.execute("DELETE FROM hotspot", []).map_err(db_err)?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT INTO hotspot (file_path, score, changes_30d, changes_90d)
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(db_err)?;
            for row in hotspots {
                stmt.execute(params![row.file_path, row.score, row.changes_30d, row.changes_90d])
                    .map_err(db_err)?;
            }
        }

        // ---- risk ----
        tx.execute("DELETE FROM risk", []).map_err(db_err)?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT INTO risk (file_path, risk_score, total_commits, fix_commits, fix_density)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(db_err)?;
            for row in risks {
                stmt.execute(params![
                    row.file_path,
                    row.risk_score,
                    row.total_commits,
                    row.fix_commits,
                    row.fix_density
                ])
                .map_err(db_err)?;
            }
        }

        // ---- cochange ----
        tx.execute("DELETE FROM cochange", []).map_err(db_err)?;
        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT INTO cochange (file_a, file_b, count, jaccard)
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .map_err(db_err)?;
            for row in cochanges {
                stmt.execute(params![row.file_a, row.file_b, row.count, row.jaccard])
                    .map_err(db_err)?;
            }
        }

        // ---- meta ----
        {
            let now_secs = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_secs();
            let timestamp_str = now_secs.to_string();

            let mut meta_stmt = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                )
                .map_err(db_err)?;
            meta_stmt
                .execute(params![META_GIT_HEAD, git_head])
                .map_err(db_err)?;
            meta_stmt
                .execute(params![META_LAST_UPDATED, timestamp_str])
                .map_err(db_err)?;
        }

        tx.commit().map_err(db_err)
    }
}

// ============================================================================
// Co-located tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[path = "storage_tests.rs"]
mod tests;
