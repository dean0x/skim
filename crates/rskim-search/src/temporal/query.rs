//! `TemporalIndex` — the top-level type implementing [`TemporalQuery`].
//!
//! Wraps a [`TemporalDb`] connection and provides composite rerank logic
//! that combines temporal signals (hot, cold, risky) via normalized rank
//! averaging.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::temporal::{ScoreKind, TemporalDb};
use crate::{Result, SearchError, TemporalFlags, TemporalQuery};

/// Default blend weight for temporal signals in composite queries.
///
/// `final = (1 - alpha) * lexical_rank + alpha * temporal_rank`.
/// Higher alpha means temporal signals dominate more.
const DEFAULT_TEMPORAL_ALPHA: f32 = 0.3;

/// Temporal signals active for a given rerank call.
///
/// `blast_radius` is handled separately and is not represented here.
#[derive(Clone, Copy)]
enum Signal {
    Hot,
    Cold,
    Risky,
}

/// Temporal layer query interface.
///
/// Wraps [`TemporalDb`] and implements [`TemporalQuery`], providing rerank
/// logic for composite (lexical + temporal) queries.
///
/// # Thread safety
///
/// `rusqlite::Connection` is `Send` but not `Sync`. The `Mutex` wrapper here
/// satisfies the `Sync` bound required by `TemporalQuery: Send + Sync`.
/// All query methods acquire the lock for the duration of the database call.
pub struct TemporalIndex {
    db: Mutex<TemporalDb>,
    alpha: f32,
}

// Compile-time assertion: TemporalDb must be Send so Mutex<TemporalDb> is Sync.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<TemporalDb>();
};

impl TemporalIndex {
    /// Open an existing temporal index from `db_path`.
    ///
    /// Returns [`SearchError::TemporalNotFound`] if the file does not exist.
    pub fn open(db_path: &Path) -> Result<Self> {
        if !db_path.exists() {
            return Err(SearchError::TemporalNotFound(db_path.display().to_string()));
        }
        let db = TemporalDb::open(db_path)?;
        Ok(Self {
            db: Mutex::new(db),
            alpha: DEFAULT_TEMPORAL_ALPHA,
        })
    }

    /// Execute a closure with access to the underlying [`TemporalDb`].
    ///
    /// Used for meta queries in stats output. The closure receives a shared
    /// reference to the db for the duration of the lock.
    ///
    /// # Errors
    ///
    /// Returns the error from the closure, or a [`SearchError::IndexBuildError`]
    /// if the mutex is poisoned (which should not happen in normal operation).
    pub fn with_db<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&TemporalDb) -> Result<T>,
    {
        let db = self
            .db
            .lock()
            .map_err(|_| SearchError::IndexBuildError("temporal db mutex poisoned".to_string()))?;
        f(&db)
    }

    /// Acquire the DB lock once and issue batch score queries for all active signals.
    ///
    /// Returns `(hotspot_map, risk_map)` keyed by POSIX-normalised path strings.
    /// Maps are empty when the corresponding signal kind is not needed.
    fn fetch_score_maps(
        &self,
        paths: &[&Path],
        active_signals: &[Signal],
    ) -> Result<(HashMap<String, f32>, HashMap<String, f32>)> {
        let needs_hotspot = active_signals
            .iter()
            .any(|s| matches!(s, Signal::Hot | Signal::Cold));
        let needs_risk = active_signals.iter().any(|s| matches!(s, Signal::Risky));

        let db = self
            .db
            .lock()
            .map_err(|_| SearchError::IndexBuildError("temporal db mutex poisoned".to_string()))?;

        let hotspot_map = if needs_hotspot {
            db.load_scores_batch(paths, ScoreKind::Hotspot)?
        } else {
            HashMap::new()
        };
        let risk_map = if needs_risk {
            db.load_scores_batch(paths, ScoreKind::Risk)?
        } else {
            HashMap::new()
        };

        Ok((hotspot_map, risk_map))
    }

    /// Pure: compute per-file temporal composite scores from pre-fetched maps.
    ///
    /// For each file the score is the average of the active signal contributions:
    /// - `Hot`   → hotspot score
    /// - `Cold`  → `1.0 - hotspot score`
    /// - `Risky` → risk score
    ///
    /// Returns scores paired with their paths, in the same order as `lexical_results`.
    fn compute_temporal_composite(
        lexical_results: &[(PathBuf, f32)],
        active_signals: &[Signal],
        hotspot_map: &HashMap<String, f32>,
        risk_map: &HashMap<String, f32>,
    ) -> Vec<(PathBuf, f32)> {
        let mut out = Vec::with_capacity(lexical_results.len());
        for (path, _) in lexical_results {
            let path_str = path.to_string_lossy().replace('\\', "/");
            let mut sum = 0.0_f32;
            for &signal in active_signals {
                let score = match signal {
                    Signal::Hot => hotspot_map.get(&path_str).copied().unwrap_or(0.0),
                    Signal::Cold => 1.0 - hotspot_map.get(&path_str).copied().unwrap_or(0.0),
                    Signal::Risky => risk_map.get(&path_str).copied().unwrap_or(0.0),
                };
                sum += score;
            }
            #[allow(clippy::cast_precision_loss)]
            let avg = sum / active_signals.len() as f32;
            out.push((path.clone(), avg));
        }
        out
    }
}

impl TemporalQuery for TemporalIndex {
    fn blast_radius(&self, target: &Path, limit: usize) -> Result<Vec<(PathBuf, f32)>> {
        self.with_db(|db| db.load_blast_radius(target, limit))
    }

    fn hotspots(&self, limit: usize) -> Result<Vec<(PathBuf, f32)>> {
        self.with_db(|db| db.load_hotspots(limit))
    }

    fn coldspots(&self, limit: usize) -> Result<Vec<(PathBuf, f32)>> {
        self.with_db(|db| db.load_coldspots(limit))
    }

    fn risky(&self, limit: usize) -> Result<Vec<(PathBuf, f32)>> {
        self.with_db(|db| db.load_risk(limit))
    }

    /// Rerank lexical results using temporal signals.
    ///
    /// Algorithm:
    /// 1. Determine active signals from `flags`
    /// 2. Fetch hotspot and risk score maps in a single DB lock acquisition
    /// 3. Compute per-file temporal composite scores (pure, no I/O)
    /// 4. Rank-normalize both lexical and temporal score lists
    /// 5. Blend: `final = (1 - alpha) * lexical_norm + alpha * temporal_norm`
    /// 6. Sort by final score desc, ties broken by path
    fn rerank(
        &self,
        lexical_results: &[(PathBuf, f32)],
        flags: &TemporalFlags,
    ) -> Result<Vec<(PathBuf, f32)>> {
        if lexical_results.is_empty() {
            return Ok(Vec::new());
        }

        // Determine which temporal signals are active (blast_radius is not used in rerank).
        let mut active_signals: Vec<Signal> = Vec::new();
        if flags.hot {
            active_signals.push(Signal::Hot);
        }
        if flags.cold {
            active_signals.push(Signal::Cold);
        }
        if flags.risky {
            active_signals.push(Signal::Risky);
        }

        if active_signals.is_empty() {
            return Ok(lexical_results.to_vec());
        }

        let paths: Vec<&Path> = lexical_results.iter().map(|(p, _)| p.as_path()).collect();
        let (hotspot_map, risk_map) = self.fetch_score_maps(&paths, &active_signals)?;

        let temporal_composite = Self::compute_temporal_composite(
            lexical_results,
            &active_signals,
            &hotspot_map,
            &risk_map,
        );

        // rank_normalize returns scores only (Vec<f32>) in original input order;
        // paths are carried from lexical_results to avoid cloning them here.
        let lexical_norm = rank_normalize(lexical_results);
        let temporal_norm = rank_normalize(&temporal_composite);

        let mut blended: Vec<(PathBuf, f32)> = lexical_results
            .iter()
            .zip(lexical_norm)
            .zip(temporal_norm)
            .map(|(((path, _), lex), temp)| {
                let score = (1.0 - self.alpha) * lex + self.alpha * temp;
                (path.clone(), score)
            })
            .collect();

        blended.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        Ok(blended)
    }
}

/// Rank-normalize a score list to `[0, 1]`.
///
/// Each entry's new score is `(n - rank) / n` where rank is its 0-indexed
/// position after sorting by score desc. Ties are broken by original index
/// (stable sort). Returns scores **in their original input order** so the
/// caller can zip two rank-normalized score lists together alongside the
/// original paths (which it already holds).
///
/// Single-element input: score 1.0.
/// Empty input: empty.
fn rank_normalize(results: &[(PathBuf, f32)]) -> Vec<f32> {
    if results.is_empty() {
        return Vec::new();
    }
    let n = results.len();
    if n == 1 {
        return vec![1.0];
    }

    // Sort indices by score desc (stable relative to original index on ties).
    let mut indexed: Vec<(usize, f32)> = results
        .iter()
        .enumerate()
        .map(|(i, (_, s))| (i, *s))
        .collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // Assign normalized score by rank.
    let mut normalized = vec![0.0_f32; n];
    for (rank, (orig_idx, _)) in indexed.iter().enumerate() {
        // Acceptable precision loss: n is small (user-facing result count).
        #[allow(clippy::cast_precision_loss)]
        let score = (n - rank) as f32 / n as f32;
        normalized[*orig_idx] = score;
    }

    normalized
}

// ============================================================================
// Unit tests for rank_normalize (pure function, no I/O)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_normalize_empty() {
        assert!(rank_normalize(&[]).is_empty());
    }

    #[test]
    fn rank_normalize_single() {
        let input = vec![(PathBuf::from("a"), 5.0)];
        let out = rank_normalize(&input);
        assert_eq!(out.len(), 1);
        assert!((out[0] - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn rank_normalize_preserves_order() {
        // Input: a=3, b=1, c=2. Ranks after sort desc: a=0, c=1, b=2.
        // Normalized (n=3): a=(3-0)/3=1.0, c=(3-1)/3≈0.667, b=(3-2)/3≈0.333
        let input = vec![
            (PathBuf::from("a"), 3.0),
            (PathBuf::from("b"), 1.0),
            (PathBuf::from("c"), 2.0),
        ];
        let out = rank_normalize(&input);
        // Output order matches input order (a, b, c).
        assert!((out[0] - 1.0).abs() < f32::EPSILON);
        assert!((out[1] - (1.0 / 3.0)).abs() < 0.01);
        assert!((out[2] - (2.0 / 3.0)).abs() < 0.01);
    }

    #[test]
    fn rank_normalize_two_elements() {
        // n=2: top gets 2/2=1.0, bottom gets 1/2=0.5
        let input = vec![(PathBuf::from("high"), 10.0), (PathBuf::from("low"), 1.0)];
        let out = rank_normalize(&input);
        assert!((out[0] - 1.0).abs() < f32::EPSILON);
        assert!((out[1] - 0.5).abs() < f32::EPSILON);
    }
}
