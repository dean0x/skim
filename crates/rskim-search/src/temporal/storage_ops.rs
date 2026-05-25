//! Store, load, and sync implementations for [`TemporalDb`].
//!
//! This file holds the data-manipulation `impl` block for [`TemporalDb`].
//! Schema migrations, connection setup, and the `TemporalDb` struct definition
//! live in [`super::storage`].

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::params;

use crate::types::Result;

use super::{META_GIT_HEAD, META_LAST_UPDATED, TemporalDb, db_err};
use super::storage_types::{CochangeRow, HotspotRow, RiskRow};

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
