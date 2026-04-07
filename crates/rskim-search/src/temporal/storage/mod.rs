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

mod schema;

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

use crate::temporal::{
    build_cochange_matrix, hotspot_scores, parse_history, risk_scores, CochangeEntry, CommitInfo,
    HotspotScore, RiskScore,
};
use crate::{Result, SearchError};

pub use schema::SCHEMA_VERSION;

/// Default temporal lookback window in days.
pub const DEFAULT_LOOKBACK_DAYS: u32 = 365;

// ============================================================================
// Meta-table key constants
// ============================================================================

mod meta_keys {
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
    conn: Connection,
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
        let all_paths = collect_all_paths(&cochange, &hotspots, &risks);

        // ── 4. Write everything in one transaction ───────────────────────────
        let tx = db.conn.unchecked_transaction().map_err(sql_err)?;

        insert_file_paths(&tx, &all_paths)?;
        let path_to_id = load_path_id_map(&tx)?;

        insert_cochange(&tx, &cochange, &path_to_id)?;
        insert_hotspots(&tx, &hotspots, &path_to_id)?;
        insert_risks(&tx, &risks, &path_to_id)?;
        insert_meta(&tx, &commits, now_secs, lookback_days, repo_path)?;

        tx.commit().map_err(sql_err)?;

        Ok(db)
    }

    // -----------------------------------------------------------------------
    // Query methods
    // -----------------------------------------------------------------------

    /// Return co-change partners for `target`, sorted by Jaccard desc.
    ///
    /// Returns an empty vec if `target` has no history in the database.
    ///
    /// # Errors
    ///
    /// [`SearchError::IndexBuildError`] on SQLite failures.
    pub fn load_blast_radius(&self, target: &Path, limit: usize) -> Result<Vec<(PathBuf, f32)>> {
        let target_str = path_to_string(target);
        let target_id: Option<i64> = self
            .conn
            .query_row(
                "SELECT temporal_file_id FROM file_paths WHERE path = ?1",
                params![target_str],
                |row| row.get(0),
            )
            .ok();
        let Some(tid) = target_id else {
            return Ok(Vec::new());
        };

        let mut stmt = self
            .conn
            .prepare(
                "SELECT fp.path, c.jaccard
                 FROM cochange c
                 JOIN file_paths fp
                   ON fp.temporal_file_id =
                      (CASE WHEN c.file_a = ?1 THEN c.file_b ELSE c.file_a END)
                 WHERE c.file_a = ?1 OR c.file_b = ?1
                 ORDER BY c.jaccard DESC
                 LIMIT ?2",
            )
            .map_err(sql_err)?;

        let rows = stmt
            .query_map(
                params![tid, i64::try_from(limit).unwrap_or(i64::MAX)],
                |row: &rusqlite::Row<'_>| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
            )
            .map_err(sql_err)?;

        let mut out = Vec::new();
        for row in rows {
            let (path_str, jaccard): (String, f64) = row.map_err(sql_err)?;
            #[allow(clippy::cast_possible_truncation)]
            out.push((PathBuf::from(path_str), jaccard as f32));
        }
        Ok(out)
    }

    /// Return files ranked by hotspot score descending.
    ///
    /// # Errors
    ///
    /// [`SearchError::IndexBuildError`] on SQLite failures.
    pub fn load_hotspots(&self, limit: usize) -> Result<Vec<(PathBuf, f32)>> {
        load_scored(
            &self.conn,
            "SELECT fp.path, h.score
             FROM hotspot h
             JOIN file_paths fp ON fp.temporal_file_id = h.temporal_file_id
             ORDER BY h.score DESC, fp.path ASC
             LIMIT ?1",
            limit,
            false,
        )
    }

    /// Return files ranked by coldspot (lowest hotspot score first).
    ///
    /// The returned score is `(1.0 - hotspot_score)` so that higher values
    /// indicate colder files, consistent with the convention that larger scores
    /// mean "more relevant."
    ///
    /// # Errors
    ///
    /// [`SearchError::IndexBuildError`] on SQLite failures.
    pub fn load_coldspots(&self, limit: usize) -> Result<Vec<(PathBuf, f32)>> {
        load_scored(
            &self.conn,
            "SELECT fp.path, h.score
             FROM hotspot h
             JOIN file_paths fp ON fp.temporal_file_id = h.temporal_file_id
             ORDER BY h.score ASC, fp.path ASC
             LIMIT ?1",
            limit,
            true, // invert: 1.0 - score
        )
    }

    /// Return files ranked by risk score descending.
    ///
    /// # Errors
    ///
    /// [`SearchError::IndexBuildError`] on SQLite failures.
    pub fn load_risk(&self, limit: usize) -> Result<Vec<(PathBuf, f32)>> {
        load_scored(
            &self.conn,
            "SELECT fp.path, r.score
             FROM risk r
             JOIN file_paths fp ON fp.temporal_file_id = r.temporal_file_id
             ORDER BY r.score DESC, fp.path ASC
             LIMIT ?1",
            limit,
            false,
        )
    }

    /// Look up a single file's score for re-rank operations.
    ///
    /// Returns `Ok(None)` if the file is not in the database.
    ///
    /// # Errors
    ///
    /// [`SearchError::IndexBuildError`] on SQLite failures.
    pub fn load_score_for(&self, path: &Path, kind: ScoreKind) -> Result<Option<f32>> {
        let path_str = path_to_string(path);
        let sql = match kind {
            ScoreKind::Hotspot => {
                "SELECT h.score FROM hotspot h
                 JOIN file_paths fp ON fp.temporal_file_id = h.temporal_file_id
                 WHERE fp.path = ?1"
            }
            ScoreKind::Risk => {
                "SELECT r.score FROM risk r
                 JOIN file_paths fp ON fp.temporal_file_id = r.temporal_file_id
                 WHERE fp.path = ?1"
            }
        };
        let score: Option<f64> = self
            .conn
            .query_row(sql, params![path_str], |row| row.get(0))
            .ok();
        #[allow(clippy::cast_possible_truncation)]
        Ok(score.map(|s| s as f32))
    }

    /// Return the value for a meta key, if present.
    ///
    /// # Errors
    ///
    /// [`SearchError::IndexBuildError`] on SQLite failures.
    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .ok())
    }

    /// Return the total number of file paths tracked in this database.
    ///
    /// # Errors
    ///
    /// [`SearchError::IndexBuildError`] on SQLite failures.
    pub fn file_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM file_paths", [], |row| row.get(0))
            .map_err(sql_err)?;
        Ok(n as u64)
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
// Private free functions — build helpers
// ============================================================================

fn collect_all_paths(
    cochange: &[CochangeEntry],
    hotspots: &[HotspotScore],
    risks: &[RiskScore],
) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();
    for e in cochange {
        paths.insert(e.path_a.clone());
        paths.insert(e.path_b.clone());
    }
    for h in hotspots {
        paths.insert(h.path.clone());
    }
    for r in risks {
        paths.insert(r.path.clone());
    }
    paths
}

fn insert_file_paths(tx: &rusqlite::Transaction<'_>, paths: &BTreeSet<PathBuf>) -> Result<()> {
    for path in paths {
        tx.execute(
            "INSERT INTO file_paths (path) VALUES (?1)",
            params![path_to_string(path)],
        )
        .map_err(sql_err)?;
    }
    Ok(())
}

fn load_path_id_map(tx: &rusqlite::Transaction<'_>) -> Result<HashMap<String, i64>> {
    let mut stmt = tx
        .prepare("SELECT temporal_file_id, path FROM file_paths")
        .map_err(sql_err)?;
    let rows = stmt
        .query_map([], |row: &rusqlite::Row<'_>| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sql_err)?;
    let mut map = HashMap::new();
    for row in rows {
        let (id, path): (i64, String) = row.map_err(sql_err)?;
        map.insert(path, id);
    }
    Ok(map)
}

fn insert_cochange(
    tx: &rusqlite::Transaction<'_>,
    cochange: &[CochangeEntry],
    path_to_id: &HashMap<String, i64>,
) -> Result<()> {
    for entry in cochange {
        let id_a = lookup_id(path_to_id, &entry.path_a)?;
        let id_b = lookup_id(path_to_id, &entry.path_b)?;
        // Enforce id_a < id_b (CHECK constraint requires it).
        let (a, b) = if id_a < id_b {
            (id_a, id_b)
        } else {
            (id_b, id_a)
        };
        tx.execute(
            "INSERT INTO cochange (file_a, file_b, co_occurrences, jaccard)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                a,
                b,
                i64::from(entry.co_occurrences),
                f64::from(entry.jaccard)
            ],
        )
        .map_err(sql_err)?;
    }
    Ok(())
}

fn insert_hotspots(
    tx: &rusqlite::Transaction<'_>,
    hotspots: &[HotspotScore],
    path_to_id: &HashMap<String, i64>,
) -> Result<()> {
    for h in hotspots {
        let id = lookup_id(path_to_id, &h.path)?;
        tx.execute(
            "INSERT INTO hotspot (temporal_file_id, commit_count_30d, commit_count_90d, score)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                id,
                i64::from(h.commit_count_30d),
                i64::from(h.commit_count_90d),
                f64::from(h.score)
            ],
        )
        .map_err(sql_err)?;
    }
    Ok(())
}

fn insert_risks(
    tx: &rusqlite::Transaction<'_>,
    risks: &[RiskScore],
    path_to_id: &HashMap<String, i64>,
) -> Result<()> {
    for r in risks {
        let id = lookup_id(path_to_id, &r.path)?;
        tx.execute(
            "INSERT INTO risk (temporal_file_id, total_commits, fix_commits, fix_density, score)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id,
                i64::from(r.total_commits),
                i64::from(r.fix_commits),
                f64::from(r.fix_density),
                f64::from(r.score)
            ],
        )
        .map_err(sql_err)?;
    }
    Ok(())
}

fn insert_meta(
    tx: &rusqlite::Transaction<'_>,
    commits: &[CommitInfo],
    now_secs: u64,
    lookback_days: u32,
    repo_path: &Path,
) -> Result<()> {
    let last_commit_hash = commits.first().map(|c| c.hash.clone()).unwrap_or_default();
    let pairs: &[(&str, String)] = &[
        (meta_keys::SCHEMA_VERSION, SCHEMA_VERSION.to_string()),
        (meta_keys::LAST_COMMIT_HASH, last_commit_hash),
        (meta_keys::LAST_BUILD_TIMESTAMP, now_secs.to_string()),
        (meta_keys::LOOKBACK_DAYS, lookback_days.to_string()),
        (meta_keys::REPO_ROOT, repo_path.display().to_string()),
        (meta_keys::GIX_VERSION, "0.68".to_string()),
        (meta_keys::COMMITS_ANALYZED, commits.len().to_string()),
    ];
    for (k, v) in pairs {
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![k, v],
        )
        .map_err(sql_err)?;
    }
    Ok(())
}

// ============================================================================
// Private free functions — query helpers
// ============================================================================

/// Run a `SELECT path, score … LIMIT ?1` query, optionally inverting the score.
///
/// `invert = true` returns `(path, 1.0 - score)` for coldspot queries.
fn load_scored(
    conn: &Connection,
    sql: &str,
    limit: usize,
    invert: bool,
) -> Result<Vec<(PathBuf, f32)>> {
    let mut stmt = conn.prepare(sql).map_err(sql_err)?;
    let rows = stmt
        .query_map(
            params![i64::try_from(limit).unwrap_or(i64::MAX)],
            |row: &rusqlite::Row<'_>| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
        )
        .map_err(sql_err)?;

    let mut out = Vec::new();
    for row in rows {
        let (path_str, score): (String, f64) = row.map_err(sql_err)?;
        #[allow(clippy::cast_possible_truncation)]
        let score_f32 = score as f32;
        let final_score = if invert { 1.0 - score_f32 } else { score_f32 };
        out.push((PathBuf::from(path_str), final_score));
    }
    Ok(out)
}

// ============================================================================
// Utility
// ============================================================================

/// Convert a repo-relative path to a forward-slash string for storage.
///
/// All platforms store paths with forward slashes for portability.
fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Look up a path's `temporal_file_id`, returning an error if missing.
fn lookup_id(map: &HashMap<String, i64>, path: &Path) -> Result<i64> {
    let key = path_to_string(path);
    map.get(&key)
        .copied()
        .ok_or_else(|| SearchError::IndexBuildError(format!("path missing from id map: {key}")))
}

/// Current time as Unix epoch seconds. Falls back to 0 on platform error.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Convert a `rusqlite::Error` into a [`SearchError::IndexBuildError`].
fn sql_err(e: rusqlite::Error) -> SearchError {
    SearchError::IndexBuildError(format!("sqlite: {e}"))
}
