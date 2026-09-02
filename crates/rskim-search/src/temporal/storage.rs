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

/// Minimum Jaccard similarity for co-change read-query results.
///
/// This constant is the single source of truth for the Jaccard filter that the
/// co-change read query applies (`cochanges_for_file`). Write-side code that
/// pre-filters emitted rows MUST use the same threshold so stored and queried
/// values stay consistent (Decision O-D).
///
/// Value: 0.10 — empirically determined via co-change validation benchmark
/// (#191); see `storage_ops::MIN_JACCARD_THRESHOLD` doc for rationale.
pub const MIN_COCHANGE_JACCARD: f64 = storage_ops::MIN_JACCARD_THRESHOLD;

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::types::{Result, SearchError};

// ============================================================================
// Schema version
// ============================================================================

/// Current schema version. Must be bumped whenever the DDL changes.
const CURRENT_VERSION: i64 = 2;

// ============================================================================
// Meta key constants
// ============================================================================

/// Key storing the Unix epoch timestamp (seconds) of the last successful [`TemporalDb::sync`].
pub const META_LAST_UPDATED: &str = "last_updated";

/// Key storing the git HEAD SHA at the time of the last [`TemporalDb::sync`].
pub const META_GIT_HEAD: &str = "git_head";

/// Key storing the canonical git repository toplevel path at the time of the
/// last [`TemporalDb::sync`] for an adopted subdirectory root (AD-413-16).
///
/// Written after `sync` completes (a second transaction on purpose — process
/// death between the two leaves the anchor absent, which is the adopt-and-record
/// case, never a false refusal). An absent row means "built before #413" and is
/// adopted rather than refused.
///
/// No schema bump required: `meta` is a key/value table and `CURRENT_VERSION`
/// and `TEMPORAL_DATA_VERSION` remain unchanged (AC26).
pub const META_GIT_TOPLEVEL: &str = "git_toplevel";

/// Version number attesting that the temporal data was written by a binary
/// whose `rebuild_temporal` applies the ghost filter.
///
/// AD-408-3: This const is the single source of truth for the self-heal
/// contract: the data-version attests "written by a binary whose
/// `rebuild_temporal` applies the ghost filter." Written **unconditionally**
/// in [`TemporalDb::sync`] — the only version-attesting write path (same
/// choke point as `META_GIT_HEAD`), so the empty-history DB also carries it
/// and the no-rebuild-loop invariant is preserved. Bump this const to force a
/// future global rebuild of all `temporal.db` files on the next query.
///
/// Note: a future caller using `store_*` / `set_meta(git_head)` directly
/// would produce a DB with a HEAD but no `data_version` row — perpetually
/// flagged stale. [`TemporalDb::sync`] is the only version-attesting write
/// path; do not bypass it.
pub const TEMPORAL_DATA_VERSION: u16 = 1;

/// Meta table key storing the [`TEMPORAL_DATA_VERSION`] value.
///
/// Written unconditionally inside [`TemporalDb::sync`] alongside
/// [`META_GIT_HEAD`] and [`META_LAST_UPDATED`] (AD-408-3).
pub const META_DATA_VERSION: &str = "data_version";

/// Meta table key storing whether the repository was shallow at build time.
///
/// AD-414-14: written as `"1"` when the `.git/shallow` file exists at
/// `TemporalDb::sync` time (indicating a `git clone --depth N`).  When the
/// stored value is `"1"` but `.git/shallow` is subsequently absent (because
/// `git fetch --unshallow` ran), the shallow→full transition triggers a
/// staleness rebuild so the now-reachable history is ingested.
///
/// Absent row (DBs written before AD-414-14 was implemented) means the check
/// is skipped — no spurious rebuilds on upgrade.
pub const META_IS_SHALLOW: &str = "is_shallow";

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

/// Classify a rusqlite error into the most specific [`SearchError`] variant.
///
/// Returns [`SearchError::DatabaseCorrupt`] for `SQLITE_NOTADB`
/// (`ErrorCode::NotADatabase`) and `SQLITE_CORRUPT` (`ErrorCode::DatabaseCorrupt`),
/// which signal that the file is structurally invalid and can be safely discarded
/// and recreated (AD-414-2, bounded self-heal).  All other errors fall through to
/// [`SearchError::Database`] so the distinction is tight and never misclassifies a
/// transient I/O failure or a lock-contention error as corruption.
///
/// Verified against `rusqlite = "0.31"` / `libsqlite3-sys 0.28` where both
/// `ErrorCode::NotADatabase` (SQLITE_NOTADB = 26) and `ErrorCode::DatabaseCorrupt`
/// (SQLITE_CORRUPT = 11) exist as named variants.
pub(super) fn classify_sqlite_err(e: &rusqlite::Error) -> SearchError {
    use rusqlite::ErrorCode;
    match e {
        rusqlite::Error::SqliteFailure(f, _)
            if matches!(f.code, ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt) =>
        {
            SearchError::DatabaseCorrupt(e.to_string())
        }
        _ => SearchError::Database(e.to_string()),
    }
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
        .map_err(|e| classify_sqlite_err(&e))?;

    if version > CURRENT_VERSION {
        // AD-414-11: return the typed variant so callers can distinguish a
        // forward-compat refusal (upgrade skim) from an I/O error (retry later)
        // or structural corruption (discard and recreate).
        return Err(SearchError::UnsupportedSchemaVersion {
            found: version,
            supported: CURRENT_VERSION,
        });
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
        .map_err(|e| classify_sqlite_err(&e))?;
    }

    if version < 2 {
        // Performance indexes for the top-N and per-file lookup queries added in v2.
        // `idx_cochange_file_a` is NOT needed — PK (file_a, file_b) already covers file_a.
        conn.execute_batch(
            "BEGIN;

            CREATE INDEX IF NOT EXISTS idx_hotspot_score ON hotspot(score);
            CREATE INDEX IF NOT EXISTS idx_risk_score ON risk(risk_score);
            CREATE INDEX IF NOT EXISTS idx_cochange_file_b ON cochange(file_b);

            PRAGMA user_version = 2;

            COMMIT;",
        )
        .map_err(|e| classify_sqlite_err(&e))?;
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
    /// - [`SearchError::DatabaseCorrupt`] if the file is not a valid SQLite
    ///   database or its pages are internally inconsistent (`SQLITE_NOTADB` /
    ///   `SQLITE_CORRUPT`); the caller may safely discard and recreate the file.
    /// - [`SearchError::UnsupportedSchemaVersion`] if the stored
    ///   `PRAGMA user_version` is newer than this build supports (forward-compat
    ///   guard, AD-414-11); the file must **not** be overwritten — upgrade skim.
    /// - [`SearchError::Database`] for everything else, including WAL-pragma
    ///   failure, lock-contention, and unexpected migration errors.
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path).map_err(|e| classify_sqlite_err(&e))?;

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
            .map_err(|e| classify_sqlite_err(&e))?;

        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .map_err(|e| classify_sqlite_err(&e))?;
        if journal_mode.to_lowercase() != "wal" {
            return Err(SearchError::Database(format!(
                "failed to enable WAL mode; journal_mode is '{journal_mode}'"
            )));
        }
        conn.execute_batch("PRAGMA synchronous=NORMAL;")
            .map_err(|e| classify_sqlite_err(&e))?;

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
    // Meta access through an already-open connection
    // ========================================================================

    /// Read a single TEXT value from the `meta` table of this open connection.
    ///
    /// Used by callers that need to read meta through an already-open connection
    /// to avoid opening a second SQLite connection for the same data.
    /// Returns `None` when the key is absent or the query fails.
    ///
    /// Currently used by `rskim::cmd::search::temporal_state::anchor_state_on_db`
    /// to read `META_GIT_TOPLEVEL` through the connection the caller already
    /// holds, avoiding a second read-only open of the same file
    /// (Finding 4 / AD-413-16).
    pub fn read_meta(&self, key: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                rusqlite::params![key],
                |row| row.get(0),
            )
            .ok()
    }
}

// ============================================================================
// Co-located tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[path = "storage_tests.rs"]
mod tests;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[path = "storage_perf_tests.rs"]
mod perf_tests;
