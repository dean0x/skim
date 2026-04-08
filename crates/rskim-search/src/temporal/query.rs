//! `TemporalIndex` — the top-level type implementing [`TemporalQuery`].
//!
//! Wraps a [`TemporalDb`] connection and provides composite rerank logic
//! that combines temporal signals (hot, cold, risky) via normalized rank
//! averaging.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::temporal::{ScoreKind, TemporalDb};
use crate::{Result, SearchError, TemporalFlags, TemporalQuery};

/// Default blend weight for temporal signals in composite queries.
///
/// `final = (1 - alpha) * lexical_rank + alpha * temporal_rank`.
/// Higher alpha means temporal signals dominate more.
const DEFAULT_TEMPORAL_ALPHA: f32 = 0.3;

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
    /// 1. Compute temporal composite score per file (average of normalized
    ///    hotspot/cold/risk scores for enabled flags)
    /// 2. Normalize both lexical and temporal scores via rank position:
    ///    `(n - rank) / n` where rank is 0-indexed
    /// 3. Blend: `final = (1 - alpha) * lexical_norm + alpha * temporal_norm`
    /// 4. Sort by final score desc, ties broken by path
    fn rerank(
        &self,
        lexical_results: &[(PathBuf, f32)],
        flags: &TemporalFlags,
    ) -> Result<Vec<(PathBuf, f32)>> {
        if lexical_results.is_empty() {
            return Ok(Vec::new());
        }

        // Collect which temporal signals are active (blast_radius is ignored in rerank).
        #[derive(Clone, Copy)]
        enum Signal {
            Hot,
            Cold,
            Risky,
        }

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

        // If no temporal flags active, return lexical unchanged.
        if active_signals.is_empty() {
            return Ok(lexical_results.to_vec());
        }

        // Compute composite temporal score per file.
        // Acquire the lock once for the entire loop to avoid repeated locking overhead.
        let db = self
            .db
            .lock()
            .map_err(|_| SearchError::IndexBuildError("temporal db mutex poisoned".to_string()))?;

        let mut temporal_composite: Vec<(PathBuf, f32)> = Vec::with_capacity(lexical_results.len());
        for (path, _) in lexical_results {
            let mut sum = 0.0_f32;
            for &signal in &active_signals {
                let score = match signal {
                    Signal::Hot => db.load_score_for(path, ScoreKind::Hotspot)?.unwrap_or(0.0),
                    Signal::Cold => {
                        1.0 - db.load_score_for(path, ScoreKind::Hotspot)?.unwrap_or(0.0)
                    }
                    Signal::Risky => db.load_score_for(path, ScoreKind::Risk)?.unwrap_or(0.0),
                };
                sum += score;
            }
            #[allow(clippy::cast_precision_loss)]
            let avg = sum / active_signals.len() as f32;
            temporal_composite.push((path.clone(), avg));
        }
        // Release the lock before doing pure computation.
        drop(db);

        // Percentile-normalize both lexical and temporal by rank.
        let lexical_norm = rank_normalize(lexical_results);
        let temporal_norm = rank_normalize(&temporal_composite);

        // Blend via alpha.
        let mut blended: Vec<(PathBuf, f32)> = lexical_norm
            .into_iter()
            .zip(temporal_norm)
            .map(|((path, lex), (_, temp))| {
                let score = (1.0 - self.alpha) * lex + self.alpha * temp;
                (path, score)
            })
            .collect();

        // Sort: score desc, path asc tie-break.
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
/// (stable sort). Returns entries **in their original input order** so the
/// caller can zip two rank-normalized lists together.
///
/// Single-element input: score 1.0.
/// Empty input: empty.
fn rank_normalize(results: &[(PathBuf, f32)]) -> Vec<(PathBuf, f32)> {
    if results.is_empty() {
        return Vec::new();
    }
    let n = results.len();
    if n == 1 {
        return vec![(results[0].0.clone(), 1.0)];
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

    results
        .iter()
        .enumerate()
        .map(|(i, (p, _))| (p.clone(), normalized[i]))
        .collect()
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
        assert!((out[0].1 - 1.0).abs() < f32::EPSILON);
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
        // Output order matches input order.
        assert_eq!(out[0].0, PathBuf::from("a"));
        assert_eq!(out[1].0, PathBuf::from("b"));
        assert_eq!(out[2].0, PathBuf::from("c"));
        assert!((out[0].1 - 1.0).abs() < f32::EPSILON);
        assert!((out[1].1 - (1.0 / 3.0)).abs() < 0.01);
        assert!((out[2].1 - (2.0 / 3.0)).abs() < 0.01);
    }

    #[test]
    fn rank_normalize_two_elements() {
        // n=2: top gets 2/2=1.0, bottom gets 1/2=0.5
        let input = vec![(PathBuf::from("high"), 10.0), (PathBuf::from("low"), 1.0)];
        let out = rank_normalize(&input);
        assert!((out[0].1 - 1.0).abs() < f32::EPSILON);
        assert!((out[1].1 - 0.5).abs() < f32::EPSILON);
    }
}
