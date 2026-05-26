//! Store, load, and sync implementations for [`TemporalDb`].
//!
//! This file holds the data-manipulation `impl` block for [`TemporalDb`].
//! Schema migrations, connection setup, and the `TemporalDb` struct definition
//! live in [`super::storage`].

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::params;

use crate::types::{Result, SearchError};

use super::storage_types::{CochangeRow, HotspotRow, RiskRow};
use super::{META_GIT_HEAD, META_LAST_UPDATED, TemporalDb, db_err};

/// Maximum rows accepted per table in a single store or sync call.
///
/// Prevents unbounded memory pressure and runaway INSERT loops on unexpectedly
/// large datasets. Matches the co-change module's `MAX_ROWS_PER_TABLE` limit.
const MAX_ROWS_PER_TABLE: usize = 500_000;

// ============================================================================
// Private insert helpers — accept an open Transaction
// ============================================================================

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
        stmt.execute(params![row.file_a, row.file_b, row.count, row.jaccard])
            .map_err(db_err)?;
    }
    Ok(())
}

impl TemporalDb {
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
    /// # Errors
    ///
    /// Returns [`SearchError::Database`] on any SQLite failure.
    /// Returns [`SearchError::CapacityExceeded`] if `rows.len() > 500_000`.
    pub fn store_cochanges(&self, rows: &[CochangeRow]) -> Result<()> {
        if rows.len() > MAX_ROWS_PER_TABLE {
            return Err(SearchError::CapacityExceeded(format!(
                "store_cochanges: {} rows exceeds limit of {MAX_ROWS_PER_TABLE}",
                rows.len()
            )));
        }
        // SAFETY: See store_hotspots.
        let tx = self.conn.unchecked_transaction().map_err(db_err)?;
        insert_cochanges_in_tx(&tx, rows)?;
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
            .collect::<std::result::Result<Vec<_>, _>>()
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
            .collect::<std::result::Result<Vec<_>, _>>()
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
            .collect::<std::result::Result<Vec<_>, _>>()
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
    /// Returns [`SearchError::CapacityExceeded`] if any slice exceeds 500_000 rows.
    pub fn sync(
        &self,
        hotspots: &[HotspotRow],
        risks: &[RiskRow],
        cochanges: &[CochangeRow],
        git_head: &str,
    ) -> Result<()> {
        for (name, len) in [
            ("hotspots", hotspots.len()),
            ("risks", risks.len()),
            ("cochanges", cochanges.len()),
        ] {
            if len > MAX_ROWS_PER_TABLE {
                return Err(SearchError::CapacityExceeded(format!(
                    "sync: {name} has {len} rows, exceeds limit of {MAX_ROWS_PER_TABLE}"
                )));
            }
        }

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
        let mut meta_stmt = tx
            .prepare_cached("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)")
            .map_err(db_err)?;
        meta_stmt
            .execute(params![META_GIT_HEAD, git_head])
            .map_err(db_err)?;
        meta_stmt
            .execute(params![META_LAST_UPDATED, now_secs])
            .map_err(db_err)?;
        drop(meta_stmt);

        tx.commit().map_err(db_err)
    }
}
