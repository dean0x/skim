//! Build-time helpers for populating the temporal database.
//!
//! All functions in this module are called exclusively from [`super::TemporalDb::build`]
//! inside a single SQLite transaction. They are pure write operations (except for
//! [`load_path_id_map`], which reads back the path IDs immediately after insertion
//! to resolve foreign keys for subsequent inserts).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use rusqlite::params;

use crate::temporal::{CochangeEntry, CommitInfo, HotspotScore, RiskScore};
use crate::Result;

use super::{meta_keys, path_to_string, sql_err, SCHEMA_VERSION};

// ============================================================================
// Path collection
// ============================================================================

/// Collect all unique file paths from the three signal slices into sorted order.
///
/// Sorted order (BTreeSet) ensures deterministic `temporal_file_id` assignment
/// across two builds of the same repo state.
pub(super) fn collect_all_paths(
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

// ============================================================================
// Insert helpers
// ============================================================================

/// Insert every path from `paths` into the `file_paths` table.
pub(super) fn insert_file_paths(
    tx: &rusqlite::Transaction<'_>,
    paths: &BTreeSet<PathBuf>,
) -> Result<()> {
    for path in paths {
        tx.execute(
            "INSERT INTO file_paths (path) VALUES (?1)",
            params![path_to_string(path)],
        )
        .map_err(sql_err)?;
    }
    Ok(())
}

/// Load the `(path → temporal_file_id)` map after inserting all paths.
///
/// Called once per build so that subsequent insert helpers can resolve paths
/// to their assigned IDs without further round-trips.
pub(super) fn load_path_id_map(tx: &rusqlite::Transaction<'_>) -> Result<HashMap<String, i64>> {
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

/// Insert co-change entries into the `cochange` table.
///
/// Enforces `file_a < file_b` ordering required by the schema CHECK constraint.
pub(super) fn insert_cochange(
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

/// Insert hotspot scores into the `hotspot` table.
pub(super) fn insert_hotspots(
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

/// Insert risk scores into the `risk` table.
pub(super) fn insert_risks(
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

/// Insert build metadata key-value pairs into the `meta` table.
pub(super) fn insert_meta(
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
// Internal utility
// ============================================================================

/// Look up a path's `temporal_file_id`, returning an error if missing.
fn lookup_id(map: &HashMap<String, i64>, path: &Path) -> Result<i64> {
    use crate::SearchError;
    let key = path_to_string(path);
    map.get(&key)
        .copied()
        .ok_or_else(|| SearchError::IndexBuildError(format!("path missing from id map: {key}")))
}
