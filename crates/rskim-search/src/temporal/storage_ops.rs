//! Store, load, and sync implementations for [`TemporalDb`].
//!
//! This file holds the data-manipulation `impl` block for [`TemporalDb`].
//! Schema migrations, connection setup, and the `TemporalDb` struct definition
//! live in [`super::storage`].

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::params;

use crate::types::{Result, SearchError};

use super::storage_types::{CochangeRow, HotspotRow, RiskRow};
use super::{
    META_DATA_VERSION, META_GIT_HEAD, META_IS_SHALLOW, META_LAST_UPDATED, TEMPORAL_DATA_VERSION,
    TemporalDb, db_err,
};

/// Maximum rows accepted per table in a single store or sync call.
///
/// Prevents unbounded memory pressure and runaway INSERT loops on unexpectedly
/// large datasets. Matches the co-change module's `MAX_ROWS_PER_TABLE` limit.
const MAX_ROWS_PER_TABLE: usize = 500_000;

/// Minimum Jaccard similarity for co-change query results.
///
/// Empirically determined via the co-change validation benchmark (#191):
/// threshold 0.10 yields the best macro F1 (0.28) across 6 OSS repos,
/// nearly doubling precision vs. unfiltered (10.9% → 21.5%) while retaining
/// 41% recall. Pairs below this threshold are noise — they co-occurred in
/// commits but the coupling signal is too weak to be predictive.
///
/// Exposed via `rskim_search::MIN_COCHANGE_JACCARD` so that the temporal
/// write-path (`rskim`'s `temporal_build.rs`) uses the same threshold as this
/// read-path query — preventing silent drift if the threshold ever changes
/// (Decision O-D).
pub(super) const MIN_JACCARD_THRESHOLD: f64 = 0.10;

// ============================================================================
// Private insert helpers — accept an open Transaction
// ============================================================================

/// Write the version-attestation pair atomically within `tx`.
///
/// `META_GIT_HEAD` and `META_DATA_VERSION` form a co-required pair (AD-408-3,
/// ADR-006): any `temporal.db` that carries `git_head` must also carry
/// `data_version`, or `temporal_db_is_stale` flags it stale indefinitely.
/// Co-locating both writes in this single primitive makes the invariant
/// un-bypassable in the production write path — callers that need to record a
/// new HEAD must go through here rather than calling [`TemporalDb::set_meta`]
/// for each key independently. (`set_meta` has a `debug_assert!` guard that
/// fires in debug builds if those keys are written through it directly.)
fn write_version_meta_in_tx(tx: &rusqlite::Transaction<'_>, git_head: &str) -> Result<()> {
    let sql = "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)";
    let mut stmt = tx.prepare_cached(sql).map_err(db_err)?;
    stmt.execute(params![META_GIT_HEAD, git_head])
        .map_err(db_err)?;
    stmt.execute(params![
        META_DATA_VERSION,
        TEMPORAL_DATA_VERSION.to_string()
    ])
    .map_err(db_err)?;
    Ok(())
}

/// Remove the version-attestation pair atomically within `tx` (AD-414-22).
///
/// The inverse of [`write_version_meta_in_tx`], and the only sanctioned way to
/// leave a `temporal.db` with **no** recorded HEAD.  Both keys are removed
/// together so the AD-408-3 co-requirement ("a DB that carries `git_head` must
/// also carry `data_version`") holds in the negative direction too — a DB that
/// attests to no snapshot attests to nothing at all.
///
/// Used by [`TemporalDb::sync_empty_unborn`] for a repository whose HEAD is
/// unborn: there is no commit to record, and writing a placeholder would be a
/// fabricated attestation (avoids PF-016).  Downstream, an absent `META_GIT_HEAD`
/// makes `temporal_db_is_stale`'s Check 1 report stale the moment a real HEAD
/// appears, and suppresses the "served from recorded commit …" advisory in
/// `warn_if_temporal_unverifiable`.
fn clear_version_meta_in_tx(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let mut stmt = tx
        .prepare_cached("DELETE FROM meta WHERE key = ?1")
        .map_err(db_err)?;
    stmt.execute(params![META_GIT_HEAD]).map_err(db_err)?;
    stmt.execute(params![META_DATA_VERSION]).map_err(db_err)?;
    Ok(())
}

fn insert_hotspots_in_tx(tx: &rusqlite::Transaction<'_>, rows: &[HotspotRow]) -> Result<()> {
    tx.execute("DELETE FROM hotspot", []).map_err(db_err)?;
    let mut stmt = tx
        .prepare_cached(
            "INSERT INTO hotspot (file_path, score, changes_30d, changes_90d)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .map_err(db_err)?;
    for row in rows {
        stmt.execute(params![
            row.file_path,
            row.score,
            row.changes_30d,
            row.changes_90d
        ])
        .map_err(db_err)?;
    }
    Ok(())
}

fn insert_risks_in_tx(tx: &rusqlite::Transaction<'_>, rows: &[RiskRow]) -> Result<()> {
    tx.execute("DELETE FROM risk", []).map_err(db_err)?;
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
    Ok(())
}

fn insert_cochanges_in_tx(tx: &rusqlite::Transaction<'_>, rows: &[CochangeRow]) -> Result<()> {
    tx.execute("DELETE FROM cochange", []).map_err(db_err)?;
    let mut stmt = tx
        .prepare_cached(
            "INSERT INTO cochange (file_a, file_b, count, jaccard)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .map_err(db_err)?;
    for row in rows {
        // Canonical ordering invariant: file_a < file_b must always hold.
        // The UNION ALL query in cochanges_for_file relies on this to avoid
        // returning duplicate rows (a row cannot satisfy both arms if the
        // ordering is strict).
        debug_assert!(
            row.file_a < row.file_b,
            "cochange row violates file_a < file_b invariant: {:?} >= {:?}",
            row.file_a,
            row.file_b
        );
        stmt.execute(params![row.file_a, row.file_b, row.count, row.jaccard])
            .map_err(db_err)?;
    }
    Ok(())
}

impl TemporalDb {
    // ========================================================================
    // Per-file lookup methods
    // ========================================================================

    /// Look up a single file's hotspot data.
    ///
    /// Returns `Ok(None)` when the file has no hotspot entry.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure other than
    /// `QueryReturnedNoRows`.
    pub fn hotspot_for_file(&self, path: &str) -> Result<Option<HotspotRow>> {
        match self.conn.query_row(
            "SELECT file_path, score, changes_30d, changes_90d FROM hotspot WHERE file_path = ?1",
            rusqlite::params![path],
            |row| {
                Ok(HotspotRow {
                    file_path: row.get(0)?,
                    score: row.get(1)?,
                    changes_30d: row.get(2)?,
                    changes_90d: row.get(3)?,
                })
            },
        ) {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }

    /// Look up a single file's risk data.
    ///
    /// Returns `Ok(None)` when the file has no risk entry.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure other than
    /// `QueryReturnedNoRows`.
    pub fn risk_for_file(&self, path: &str) -> Result<Option<RiskRow>> {
        match self.conn.query_row(
            "SELECT file_path, risk_score, total_commits, fix_commits, fix_density \
             FROM risk WHERE file_path = ?1",
            rusqlite::params![path],
            |row| {
                Ok(RiskRow {
                    file_path: row.get(0)?,
                    risk_score: row.get(1)?,
                    total_commits: row.get(2)?,
                    fix_commits: row.get(3)?,
                    fix_density: row.get(4)?,
                })
            },
        ) {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(db_err(e)),
        }
    }

    /// Find all co-change partners for a file (bidirectional).
    ///
    /// Searches both `file_a` and `file_b` columns so the canonical ordering
    /// (lexically smaller path in `file_a`) is transparent to callers.
    /// Results are sorted by Jaccard similarity descending; ties are broken by
    /// `file_a ASC, file_b ASC` for deterministic page boundaries under
    /// `--blast-radius --offset` pagination (PF-012 spirit).
    ///
    /// Uses `UNION ALL` of two indexed queries instead of `OR` to allow SQLite
    /// to use both the primary key index on `file_a` and the secondary index on
    /// `file_b`. With `OR`, SQLite degrades to a partial or full table scan at
    /// large row counts. `UNION ALL` (not `UNION`) is safe because the canonical
    /// ordering guarantee (`file_a < file_b`) makes self-pairs impossible, so no
    /// row can satisfy both arms simultaneously.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    pub fn cochanges_for_file(&self, path: &str) -> Result<Vec<CochangeRow>> {
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT file_a, file_b, count, jaccard FROM cochange \
                 WHERE file_a = ?1 AND jaccard >= ?2 \
                 UNION ALL \
                 SELECT file_a, file_b, count, jaccard FROM cochange \
                 WHERE file_b = ?1 AND jaccard >= ?2 \
                 ORDER BY jaccard DESC, file_a ASC, file_b ASC LIMIT 10000",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(rusqlite::params![path, MIN_JACCARD_THRESHOLD], |row| {
                Ok(CochangeRow {
                    file_a: row.get(0)?,
                    file_b: row.get(1)?,
                    count: row.get(2)?,
                    jaccard: row.get(3)?,
                })
            })
            .map_err(db_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    // ========================================================================
    // Top-N query methods
    // ========================================================================

    /// Return the top `limit` hotspot rows ordered by score descending.
    ///
    /// `limit` is silently clamped to [`MAX_ROWS_PER_TABLE`] to prevent
    /// `usize::MAX as i64` integer overflow when binding to SQLite.
    ///
    /// Returns an empty `Vec` when the table is empty.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    pub fn top_hotspots(&self, limit: usize) -> Result<Vec<HotspotRow>> {
        let limit = limit.min(MAX_ROWS_PER_TABLE);
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT file_path, score, changes_30d, changes_90d FROM hotspot \
                 ORDER BY score DESC, file_path ASC LIMIT ?1",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(rusqlite::params![limit as i64], |row| {
                Ok(HotspotRow {
                    file_path: row.get(0)?,
                    score: row.get(1)?,
                    changes_30d: row.get(2)?,
                    changes_90d: row.get(3)?,
                })
            })
            .map_err(db_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    /// Return the top `limit` risk rows ordered by risk_score descending.
    ///
    /// `limit` is silently clamped to [`MAX_ROWS_PER_TABLE`] to prevent
    /// `usize::MAX as i64` integer overflow when binding to SQLite.
    ///
    /// Returns an empty `Vec` when the table is empty.
    ///
    /// # Deterministic tie-break (AD-378-4)
    ///
    /// Volume-weighting (Wilson lower bound, #378) reduces but does not eliminate
    /// exact ties — e.g. all `1-fix/1-commit` files share the same Wilson score.
    /// Rows with equal `risk_score` are therefore ordered by `total_commits`
    /// **descending** (a higher-evidence file wins) and then `file_path`
    /// **ascending** (lexicographic) as a final stable key. This makes standalone
    /// `--risky` ordering deterministic across rebuilds and consistent with the
    /// combined-query paths that already break ties by path. This is a query-time
    /// ORDER BY change only — NOT a schema change (AC7).
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    pub fn top_risks(&self, limit: usize) -> Result<Vec<RiskRow>> {
        let limit = limit.min(MAX_ROWS_PER_TABLE);
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT file_path, risk_score, total_commits, fix_commits, fix_density \
                 FROM risk ORDER BY risk_score DESC, total_commits DESC, file_path ASC LIMIT ?1",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(rusqlite::params![limit as i64], |row| {
                Ok(RiskRow {
                    file_path: row.get(0)?,
                    risk_score: row.get(1)?,
                    total_commits: row.get(2)?,
                    fix_commits: row.get(3)?,
                    fix_density: row.get(4)?,
                })
            })
            .map_err(db_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_err)?;
        Ok(rows)
    }

    /// Return the bottom `limit` hotspot rows ordered by score ascending (coldspots).
    ///
    /// `limit` is silently clamped to [`MAX_ROWS_PER_TABLE`] to prevent
    /// `usize::MAX as i64` integer overflow when binding to SQLite.
    ///
    /// Returns an empty `Vec` when the table is empty.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    pub fn top_coldspots(&self, limit: usize) -> Result<Vec<HotspotRow>> {
        let limit = limit.min(MAX_ROWS_PER_TABLE);
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT file_path, score, changes_30d, changes_90d FROM hotspot \
                 ORDER BY score ASC, file_path ASC LIMIT ?1",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map(rusqlite::params![limit as i64], |row| {
                Ok(HotspotRow {
                    file_path: row.get(0)?,
                    score: row.get(1)?,
                    changes_30d: row.get(2)?,
                    changes_90d: row.get(3)?,
                })
            })
            .map_err(db_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_err)?;
        Ok(rows)
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
    /// Returns [`SearchError::CapacityExceeded`] if `rows.len() > 500_000`.
    pub fn store_hotspots(&self, rows: &[HotspotRow]) -> Result<()> {
        if rows.len() > MAX_ROWS_PER_TABLE {
            return Err(SearchError::CapacityExceeded(format!(
                "store_hotspots: {} rows exceeds limit of {MAX_ROWS_PER_TABLE}",
                rows.len()
            )));
        }
        // SAFETY: `TemporalDb` is `Send` but not `Sync` — it can be moved to
        // another thread but cannot be shared. Since `&self` methods cannot be
        // called concurrently, no nested transaction can be active.
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        insert_hotspots_in_tx(&tx, rows)?;
        tx.commit().map_err(db_err)
    }

    /// Replace all rows in the `risk` table with `rows`.
    ///
    /// Runs DELETE + batch INSERT in a single transaction.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    /// Returns [`SearchError::CapacityExceeded`] if `rows.len() > 500_000`.
    pub fn store_risks(&self, rows: &[RiskRow]) -> Result<()> {
        if rows.len() > MAX_ROWS_PER_TABLE {
            return Err(SearchError::CapacityExceeded(format!(
                "store_risks: {} rows exceeds limit of {MAX_ROWS_PER_TABLE}",
                rows.len()
            )));
        }
        // SAFETY: See store_hotspots.
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        insert_risks_in_tx(&tx, rows)?;
        tx.commit().map_err(db_err)
    }

    /// Replace all rows in the `cochange` table with `rows`.
    ///
    /// Runs DELETE + batch INSERT in a single transaction.
    ///
    /// When `rows.len() > MAX_ROWS_PER_TABLE`, the call degrades gracefully:
    /// rows are sorted by Jaccard score descending and truncated to
    /// `MAX_ROWS_PER_TABLE` (highest-signal pairs kept; lowest-signal dropped),
    /// and a notice is printed to stderr. This avoids the caller losing the
    /// entire write due to an oversized co-change set. Follow-up #522 tracks
    /// re-derivation of the Jaccard threshold for the post-#407 commit
    /// population.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    pub fn store_cochanges(&self, rows: &[CochangeRow]) -> Result<()> {
        // AD-407-9 (capacity ripple): the full-DAG walk can inflate the
        // co-change pair set ~2.5-3x.  Degrade gracefully — sort by Jaccard
        // descending and keep the top MAX_ROWS_PER_TABLE pairs — rather than
        // returning CapacityExceeded and losing the caller's write entirely.
        let rows_buf: Option<Vec<CochangeRow>> = if rows.len() > MAX_ROWS_PER_TABLE {
            eprintln!(
                "skim: store_cochanges: {} rows exceeds {MAX_ROWS_PER_TABLE}-row \
                 capacity; retaining top {MAX_ROWS_PER_TABLE} by Jaccard score \
                 (lowest-signal pairs dropped). See #522 for threshold re-derivation.",
                rows.len()
            );
            let mut v = rows.to_vec();
            v.sort_unstable_by(|a, b| {
                b.jaccard
                    .partial_cmp(&a.jaccard)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            v.truncate(MAX_ROWS_PER_TABLE);
            Some(v)
        } else {
            None
        };
        let rows: &[CochangeRow] = rows_buf.as_deref().unwrap_or(rows);
        // SAFETY: See store_hotspots.
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        insert_cochanges_in_tx(&tx, rows)?;
        tx.commit().map_err(db_err)
    }

    /// Insert or replace a single key-value pair in the `meta` table.
    ///
    /// # Warning: version-attestation keys
    ///
    /// Do **not** write [`META_GIT_HEAD`] or [`META_DATA_VERSION`] through
    /// this method directly. Those two keys form a co-required pair (AD-408-3):
    /// writing one without the other yields a database that
    /// `temporal_db_is_stale` flags stale forever (absent `data_version` is
    /// treated as stale). Use [`TemporalDb::sync`] instead — it writes both
    /// atomically via the `write_version_meta_in_tx` primitive. A
    /// `debug_assert!` below fires in debug builds if this contract is
    /// violated.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        debug_assert!(
            key != META_GIT_HEAD && key != META_DATA_VERSION,
            "set_meta must not write version-attestation keys directly; \
             use TemporalDb::sync (AD-408-3)"
        );
        self.conn
            .execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                params![key, value],
            )
            .map(|_| ())
            .map_err(db_err)
    }

    /// Delete a metadata entry by key from the `meta` table.
    ///
    /// Returns `Ok(())` when the key is absent (idempotent delete) — the caller
    /// does not need to check whether a row existed.
    ///
    /// The version-attestation keys `META_GIT_HEAD` and `META_DATA_VERSION` must
    /// not be deleted through this API — use [`TemporalDb::sync`] for those
    /// (AD-408-3).  The `debug_assert!` below fires in debug builds if this
    /// contract is violated.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    pub fn delete_meta(&self, key: &str) -> Result<()> {
        debug_assert!(
            key != META_GIT_HEAD && key != META_DATA_VERSION,
            "delete_meta must not delete version-attestation keys; \
             use TemporalDb::sync (AD-408-3)"
        );
        self.conn
            .execute("DELETE FROM meta WHERE key = ?1", params![key])
            .map(|_| ())
            .map_err(db_err)
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
    pub fn load_hotspots(&self) -> Result<Vec<HotspotRow>> {
        let mut stmt = self
            .conn
            .prepare(
                // LIMIT is MAX_ROWS_PER_TABLE + 1 so the post-query check below
                // can distinguish "exactly at limit" from "over limit".
                "SELECT file_path, score, changes_30d, changes_90d FROM hotspot
                 LIMIT 500001",
            )
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
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_err)?;
        if rows.len() > MAX_ROWS_PER_TABLE {
            return Err(SearchError::CapacityExceeded(format!(
                "load_hotspots: table contains more than {MAX_ROWS_PER_TABLE} rows"
            )));
        }
        Ok(rows)
    }

    /// Load all rows from the `risk` table.
    ///
    /// Returns an empty `Vec` when the table is empty.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    pub fn load_risks(&self) -> Result<Vec<RiskRow>> {
        let mut stmt = self
            .conn
            .prepare(
                // LIMIT is MAX_ROWS_PER_TABLE + 1 so the post-query check can
                // distinguish "exactly at limit" from "over limit".
                "SELECT file_path, risk_score, total_commits, fix_commits, fix_density FROM risk
                 LIMIT 500001",
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
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_err)?;
        if rows.len() > MAX_ROWS_PER_TABLE {
            return Err(SearchError::CapacityExceeded(format!(
                "load_risks: table contains more than {MAX_ROWS_PER_TABLE} rows"
            )));
        }
        Ok(rows)
    }

    /// Load all rows from the `cochange` table.
    ///
    /// Returns an empty `Vec` when the table is empty.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    pub fn load_cochanges(&self) -> Result<Vec<CochangeRow>> {
        let mut stmt = self
            .conn
            .prepare(
                // LIMIT is MAX_ROWS_PER_TABLE + 1 so the post-query check can
                // distinguish "exactly at limit" from "over limit".
                "SELECT file_a, file_b, count, jaccard FROM cochange
                 LIMIT 500001",
            )
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
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_err)?;
        if rows.len() > MAX_ROWS_PER_TABLE {
            return Err(SearchError::CapacityExceeded(format!(
                "load_cochanges: table contains more than {MAX_ROWS_PER_TABLE} rows"
            )));
        }
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
    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        match self.conn.query_row(
            "SELECT value FROM meta WHERE key = ?1",
            params![key],
            |row| row.get(0),
        ) {
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
    /// updates the `meta` table with four keys: `git_head` under
    /// [`META_GIT_HEAD`], the current UTC timestamp under [`META_LAST_UPDATED`],
    /// the current [`TEMPORAL_DATA_VERSION`] under [`META_DATA_VERSION`]
    /// (AD-408-3), and the shallow-clone flag under [`META_IS_SHALLOW`]
    /// (AD-414-14). All operations are wrapped in one transaction: either all
    /// succeed or none are committed.
    ///
    /// **AD-414-14**: `is_shallow` is written as the [`META_IS_SHALLOW`] key/value
    /// meta row inside this transaction (no schema bump; `CURRENT_VERSION` stays 2).
    /// When `is_shallow` is `true`, `check_staleness` Check 3 treats a subsequently
    /// absent `.git/shallow` file as a staleness trigger so a `git fetch --unshallow`
    /// is detected on the next query without manual `--rebuild`.
    ///
    /// # Parameters
    ///
    /// - `hotspots`: Rows to store in the `hotspot` table.
    /// - `risks`: Rows to store in the `risk` table.
    /// - `cochanges`: Rows to store in the `cochange` table.
    /// - `git_head`: The git HEAD SHA (or any string identifier) to record in
    ///   the `meta` table under [`META_GIT_HEAD`].
    /// - `is_shallow`: Whether `.git/shallow` existed when these rows were
    ///   computed; recorded under [`META_IS_SHALLOW`] as `"1"` / `"0"`.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure. On error the
    /// transaction is rolled back and the database is left unchanged.
    /// Returns [`SearchError::CapacityExceeded`] if the `hotspots` or `risks`
    /// slice exceeds 500_000 rows. Co-change rows exceeding this limit are
    /// degraded (lowest-Jaccard pairs dropped, notice to stderr) rather than
    /// causing an error — see [`Self::store_cochanges`] for the rationale.
    pub fn sync(
        &self,
        hotspots: &[HotspotRow],
        risks: &[RiskRow],
        cochanges: &[CochangeRow],
        git_head: &str,
        is_shallow: bool,
    ) -> Result<()> {
        self.sync_inner(hotspots, risks, cochanges, Some(git_head), is_shallow)
    }

    /// AD-414-22: atomically replace all temporal data with an **empty** result
    /// set for a repository whose HEAD is unborn (`git init` with no commits).
    ///
    /// Identical to [`Self::sync`] except that no HEAD is recorded: the
    /// `git_head` / `data_version` attestation pair is *removed* rather than
    /// written (see `clear_version_meta_in_tx`).  An unborn branch has no commit
    /// to name, and recording a placeholder would be a fabricated attestation
    /// that `warn_if_temporal_unverifiable` would then report as "served from
    /// recorded commit …" (avoids PF-016).
    ///
    /// The resulting file is a schema-current, zero-row `temporal.db` whose
    /// `meta` table carries only [`META_LAST_UPDATED`] and [`META_IS_SHALLOW`].
    /// `--stats` reports it as `temporal_state: "empty"`, and the first query
    /// after the repository's first commit sees an absent [`META_GIT_HEAD`] and
    /// rebuilds (`temporal_db_is_stale` Check 1).
    ///
    /// # Parameters
    ///
    /// - `is_shallow`: whether `.git/shallow` existed when the (empty) history
    ///   was read; recorded under [`META_IS_SHALLOW`] as `"1"` / `"0"`.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.  On error the
    /// transaction is rolled back and the database is left unchanged.
    pub fn sync_empty_unborn(&self, is_shallow: bool) -> Result<()> {
        self.sync_inner(&[], &[], &[], None, is_shallow)
    }

    /// Shared transaction body for [`Self::sync`] and [`Self::sync_empty_unborn`].
    ///
    /// `git_head` is `Some(sha)` when the caller has a resolved HEAD to attest to
    /// and `None` for the unborn-HEAD case; that is the *only* difference between
    /// the two public entry points, so both go through one transaction and cannot
    /// drift apart.
    fn sync_inner(
        &self,
        hotspots: &[HotspotRow],
        risks: &[RiskRow],
        cochanges: &[CochangeRow],
        git_head: Option<&str>,
        is_shallow: bool,
    ) -> Result<()> {
        // Hotspot and risk tables: hard cap (error if exceeded).
        // These are per-file tables bounded by the walked file count and
        // should never approach MAX_ROWS_PER_TABLE in practice.
        for (name, len) in [("hotspots", hotspots.len()), ("risks", risks.len())] {
            if len > MAX_ROWS_PER_TABLE {
                return Err(SearchError::CapacityExceeded(format!(
                    "sync: {name} has {len} rows, exceeds limit of {MAX_ROWS_PER_TABLE}"
                )));
            }
        }

        // Co-change table: degrade gracefully rather than failing the whole sync.
        //
        // AD-407-9 (capacity ripple): the full-DAG walk (#407) can inflate the
        // co-change pair set ~2.5-3x relative to the pre-#407 first-parent walk,
        // pushing large repos past MAX_ROWS_PER_TABLE.  Hard-failing here would
        // abort the entire transaction and lose the hotspot and risk writes too.
        // Instead, sort by Jaccard descending and keep only the top
        // MAX_ROWS_PER_TABLE pairs (highest-signal retained; lowest-signal
        // dropped).  A notice is printed to stderr.  Follow-up #522 tracks
        // re-derivation of MIN_COCHANGE_JACCARD for the new commit population.
        let cochanges_buf: Option<Vec<CochangeRow>> = if cochanges.len() > MAX_ROWS_PER_TABLE {
            eprintln!(
                "skim: co-change table has {} pairs, exceeds {MAX_ROWS_PER_TABLE}-pair \
                 capacity; retaining top {MAX_ROWS_PER_TABLE} by Jaccard score \
                 (lowest-signal pairs dropped). See #522 for threshold re-derivation.",
                cochanges.len()
            );
            let mut v = cochanges.to_vec();
            v.sort_unstable_by(|a, b| {
                b.jaccard
                    .partial_cmp(&a.jaccard)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            v.truncate(MAX_ROWS_PER_TABLE);
            Some(v)
        } else {
            None
        };
        let cochanges: &[CochangeRow] = cochanges_buf.as_deref().unwrap_or(cochanges);

        // SAFETY: `TemporalDb` is `Send` but not `Sync` — it can be moved to
        // another thread but cannot be shared. Since `&self` methods cannot be
        // called concurrently, no nested transaction can be active.
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;

        insert_hotspots_in_tx(&tx, hotspots)?;
        insert_risks_in_tx(&tx, risks)?;
        insert_cochanges_in_tx(&tx, cochanges)?;

        // ---- meta ----
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_secs()
            .to_string();
        // Write git_head + data_version as a co-required pair through the
        // dedicated primitive so the invariant cannot be bypassed (AD-408-3).
        // AD-414-22: `None` (unborn HEAD) removes the pair instead of writing a
        // placeholder, so the DB never attests to a snapshot it does not have.
        match git_head {
            Some(head) => write_version_meta_in_tx(&tx, head)?,
            None => clear_version_meta_in_tx(&tx)?,
        }
        tx.prepare_cached("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)")
            .map_err(db_err)?
            .execute(params![META_LAST_UPDATED, now_secs])
            .map_err(db_err)?;

        // AD-414-14: record is_shallow so Check 3 in `temporal_db_is_stale` can
        // detect a shallow→full transition (git fetch --unshallow) on the next query.
        // "1" = shallow clone; "0" = full history.  No schema bump: key/value meta.
        let is_shallow_val = if is_shallow { "1" } else { "0" };
        tx.prepare_cached("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)")
            .map_err(db_err)?
            .execute(params![META_IS_SHALLOW, is_shallow_val])
            .map_err(db_err)?;

        tx.commit().map_err(db_err)
    }
}
