//! Read-only query methods on [`TemporalDb`].
//!
//! All methods in this module are pure reads — they never modify the database.
//! Write operations live in [`super::build_ops`].

use std::path::{Path, PathBuf};

use rusqlite::params;

use crate::Result;

use super::{path_to_string, sql_err, ScoreKind, TemporalDb};

impl TemporalDb {
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
    /// Uses `prepare_cached` to amortize statement compilation cost across the
    /// N iterations inside `TemporalIndex::rerank` (previously N×M fresh
    /// `prepare` calls, now one cached statement per signal kind).
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
                "SELECT h.score FROM hotspot h \
                 JOIN file_paths fp ON fp.temporal_file_id = h.temporal_file_id \
                 WHERE fp.path = ?1"
            }
            ScoreKind::Risk => {
                "SELECT r.score FROM risk r \
                 JOIN file_paths fp ON fp.temporal_file_id = r.temporal_file_id \
                 WHERE fp.path = ?1"
            }
        };
        // prepare_cached reuses the same compiled statement across calls,
        // amortizing preparation cost over N rerank iterations.
        let mut stmt = self.conn.prepare_cached(sql).map_err(sql_err)?;
        let score: Option<f64> = stmt
            .query_row(rusqlite::params![path_str], |row| row.get(0))
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
}

// ============================================================================
// Private query helpers
// ============================================================================

/// Run a `SELECT path, score … LIMIT ?1` query, optionally inverting the score.
///
/// `invert = true` returns `(path, 1.0 - score)` for coldspot queries.
fn load_scored(
    conn: &rusqlite::Connection,
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
