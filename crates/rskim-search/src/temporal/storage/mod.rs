//! Temporal database: SQLite-backed persistence for co-change, hotspot, and risk data.
//!
//! Schema defined in [`schema`]. Build via [`TemporalDb::build`]. Query via the
//! `load_*` methods, which return `(PathBuf, f32)` pairs compatible with the
//! `TemporalQuery` trait's API (wired up in Phase 4).
//!
//! # Crash safety
//!
//! [`TemporalDb::build`] writes to a temporary file (`{db_path}.tmp`) and
//! atomically replaces the live database via `fs::rename` on success. A process
//! crash during build leaves the original database at `db_path` intact. WAL
//! sidecars are checkpointed before rename so the result is a clean single-file DB.
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
#[non_exhaustive]
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
///
/// # Thread safety
///
/// `TemporalDb` is `Send` (owns a `rusqlite::Connection` which is `Send`).
/// It is NOT `Sync` — use a `Mutex<TemporalDb>` for shared access across threads
/// (see `TemporalIndex`).
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
        let mut conn = Connection::open(path).map_err(sql_err)?;
        Self::configure_connection(&conn)?;
        Self::migrate(&mut conn)?;
        Ok(Self { conn })
    }

    /// Build a temporal database from scratch for the given git repo.
    ///
    /// Writes the new database to `{db_path}.tmp` first, then atomically
    /// replaces the live file via `fs::rename` on success. If the build fails
    /// at any point the original database at `db_path` is preserved intact.
    ///
    /// # Errors
    ///
    /// - [`SearchError::GitError`] if git history cannot be parsed
    /// - [`SearchError::IndexBuildError`] for SQLite or I/O failures
    pub fn build(repo_path: &Path, db_path: &Path, lookback_days: u32) -> Result<Self> {
        use crate::temporal::{build_cochange_matrix, hotspot_scores, parse_history, risk_scores};

        // Build into a temp path so the live DB is untouched until success.
        // Append ".tmp" to the full filename (e.g. "temporal.db" → "temporal.db.tmp").
        let tmp_path = {
            let mut p = db_path.as_os_str().to_owned();
            p.push(".tmp");
            std::path::PathBuf::from(p)
        };

        // Clean up any leftover temp file and its WAL/SHM sidecars from a
        // previous crashed build.  SQLite names WAL/SHM by appending "-wal"/"-shm"
        // to the database filename.
        remove_db_and_sidecars(&tmp_path);

        let mut db = Self::open(&tmp_path)?;

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
        let tx = db.conn.transaction().map_err(sql_err)?;
        // Note: `transaction()` (vs `unchecked_transaction()`) enforces that
        // no nested transactions accidentally nest, catching bugs at runtime.

        build_ops::insert_file_paths(&tx, &all_paths)?;
        let path_to_id = build_ops::load_path_id_map(&tx)?;

        build_ops::insert_cochange(&tx, &cochange, &path_to_id)?;
        build_ops::insert_hotspots(&tx, &hotspots, &path_to_id)?;
        build_ops::insert_risks(&tx, &risks, &path_to_id)?;
        build_ops::insert_meta(&tx, &commits, now_secs, lookback_days, repo_path)?;

        tx.commit().map_err(sql_err)?;

        // Flush WAL back into the main file before rename so the destination
        // is a clean single-file database without dangling WAL sidecars.
        db.conn
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .map_err(sql_err)?;

        // Close the connection before rename so SQLite releases its locks on
        // the temp file.  The `db` value is dropped here; `open` re-opens the
        // renamed path immediately after.
        drop(db);

        // Remove live WAL/SHM sidecars before atomically swapping in the new DB.
        remove_wal_sidecars(db_path);

        std::fs::rename(&tmp_path, db_path).map_err(|e| {
            SearchError::IndexBuildError(format!(
                "rename {} → {}: {e}",
                tmp_path.display(),
                db_path.display()
            ))
        })?;

        // Re-open the final (renamed) database file.
        Self::open(db_path)
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

    fn migrate(conn: &mut Connection) -> Result<()> {
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(sql_err)?;

        if version == 0 {
            let tx = conn.transaction().map_err(sql_err)?;
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
///
/// # Panics
///
/// Does not panic, but returns a lossy replacement for non-UTF-8 paths with a
/// warning logged to stderr. In practice all repo paths from gix are UTF-8.
pub(super) fn path_to_string(path: &std::path::Path) -> String {
    let s = path.to_str().unwrap_or_else(|| {
        eprintln!(
            "warning: non-UTF-8 path will be stored with replacement characters: {}",
            path.display()
        );
        // Fallback: lossy conversion is better than silent corruption.
        // This branch is unreachable in practice (gix emits UTF-8 paths).
        ""
    });
    if s.is_empty() {
        // Fallback for the non-UTF-8 branch above.
        return path.to_string_lossy().replace('\\', "/");
    }
    s.replace('\\', "/")
}

/// Current time as Unix epoch seconds. Falls back to 0 on platform error.
pub(super) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Convert a `rusqlite::Error` into a [`SearchError::IndexBuildError`].
///
/// Use this for write-path (build) errors. For read-path errors, use [`sql_query_err`].
pub(super) fn sql_err(e: rusqlite::Error) -> SearchError {
    SearchError::IndexBuildError(format!("sqlite: {e}"))
}

/// Convert a `rusqlite::Error` into a [`SearchError::TemporalQueryError`].
///
/// Use this for read-path (query) errors. For write-path errors, use [`sql_err`].
pub(super) fn sql_query_err(e: rusqlite::Error) -> SearchError {
    SearchError::TemporalQueryError(format!("sqlite: {e}"))
}

/// Remove `path`, `path-wal`, and `path-shm`, ignoring missing files.
///
/// SQLite WAL sidecars are named by appending `-wal`/`-shm` to the database
/// filename. Missing files are silently ignored. Other errors (e.g. permission
/// denied) are logged to stderr but do not fail the build.
fn remove_db_and_sidecars(path: &std::path::Path) {
    remove_file_warn(path);
    remove_wal_sidecars(path);
}

/// Remove `path-wal` and `path-shm` sidecars, ignoring missing files.
fn remove_wal_sidecars(path: &std::path::Path) {
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        remove_file_warn(&std::path::PathBuf::from(sidecar));
    }
}

/// Remove a file, ignoring `NotFound` but warning on other errors.
fn remove_file_warn(path: &std::path::Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            eprintln!("warning: could not remove {}: {e}", path.display());
        }
    }
}
