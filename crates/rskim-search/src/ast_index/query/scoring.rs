//! BM25 scoring helpers for AST structural query execution.
//!
//! Contains the per-posting BM25 score computation, IDF memoization, and
//! the [`ScoringCtx`] accumulator used by `engine::score_ngram_set`.

use rskim_core::Language;
use rustc_hash::FxHashMap;

use super::adapter::AstPostingSource;
use crate::{Result, ast_index::AstPosting};

// BM25 constants
/// BM25 saturation parameter k1 for AST structural scoring.
pub const AST_BM25_K1: f64 = 1.2;
/// BM25 length-normalisation parameter b for AST structural scoring.
pub const AST_BM25_B: f64 = 0.75;

/// IDF fallback for files whose language byte is unrecognised (neutral weight —
/// does not amplify or suppress the BM25 term-frequency contribution).
pub(super) const UNKNOWN_LANG_IDF: f64 = 1.0;

// P3: capacity sizing constants (#286).
//
// `CAPACITY_FLOOR` is the minimum initial capacity for `scores` and
// `meta_cache` regardless of how small the posting lists are.  This prevents
// pathological grow-from-1 churn on tiny queries that suddenly fan out.
//
// Basis (measured, not magic): a FxHashMap rehash doubles the table; the
// floor at 16 means a selective query matching ≤16 files never rehashes.
// For the 10k-file hot-bigram bench the capacity estimate from `max_list_len`
// equals `file_count` anyway, so the floor only applies on narrow queries.
pub(super) const CAPACITY_FLOOR: usize = 16;

// Lite scoring metadata (P1, #286)
/// Minimal file metadata needed for BM25 scoring: `lang_id` and `node_count`.
///
/// Used as the value type in `meta_cache` (replacing `AstFileMetaEntry`) so
/// the cache footprint is 5 bytes per entry instead of 15 bytes.
#[derive(Clone, Copy)]
pub(super) struct LiteMeta {
    pub(super) lang_id: u8,
    pub(super) node_count: u32,
}

impl From<(u8, u32)> for LiteMeta {
    fn from((lang_id, node_count): (u8, u32)) -> Self {
        Self {
            lang_id,
            node_count,
        }
    }
}

impl LiteMeta {
    /// Recover the [`Language`] from this entry's `lang_id`.
    ///
    /// Mirrors [`crate::ast_index::store::format::AstFileMetaEntry::language`]
    /// so that lang-id → Language recovery lives in one conceptual place per
    /// struct rather than being inlined at every call site (#286).
    #[inline]
    pub(super) fn language(self) -> Option<Language> {
        crate::index::lang_map::lang_from_id(self.lang_id)
    }
}

// Scoring context

/// Accumulated scoring state for one `run_ngram_set` call.
///
/// Bundles `scores`, `meta_cache`, and the corpus `file_count` so that
/// capacity reservation and score accumulation share the same mutable state
/// without a 7-parameter function signature (#286).
pub(super) struct ScoringCtx {
    pub(super) scores: FxHashMap<u32, f64>,
    /// `None` for single-n-gram queries (no cross-list cache benefit, C1).
    /// Capacity note: both `scores` and `meta_cache` reserve via the same
    /// `new_slots` value, computed as `postings.len().min(file_count)
    /// .saturating_sub(scores.len())`.  On lang-filtered runs `scores.len()`
    /// can be smaller than `cache.len()` (decoded-but-skipped postings populate
    /// the cache without entering `scores`), so `new_slots` is a lower bound
    /// for the cache — but `reserve(additional)` is additive, so the cache is
    /// never under-sized (#286).
    pub(super) meta_cache: Option<FxHashMap<u32, LiteMeta>>,
    pub(super) file_count: usize,
}

impl ScoringCtx {
    /// Accumulate BM25 scores for one set of postings.
    ///
    /// Reserves capacity before inserting (P3 invariant: reservation and
    /// insert are always co-located; #286).
    ///
    /// IDF is computed once per distinct language via a tiny last-value cache,
    /// reducing O(postings) binary-search calls to O(distinct_languages).
    ///
    /// P1 (#286): Uses `file_lang_and_node_count` instead of `file_meta` to
    /// decode only the 5 bytes needed for BM25 scoring.
    ///
    /// P4 (#286): `lang_filter` — when `Some(L)`, a posting whose `lang_id`
    /// does not resolve to `L` is skipped before insertion into `scores`.
    /// When `None`, all postings are scored (unfiltered; `search_ast` always
    /// passes `None`).
    pub(super) fn score_postings<R: AstPostingSource>(
        &mut self,
        postings: &[AstPosting],
        reader: &R,
        avg: f64,
        lang_filter: Option<Language>,
        idf_fn: impl Fn(Language) -> f64,
    ) -> Result<()> {
        // Reserve only new slots: clamped to file_count to avoid over-allocation
        // when scores already contains overlapping docs (AC6 broad, AC7 empty-first,
        // P3 #286).  P3 invariant: reservation and insert are co-located here so
        // callers cannot omit or reorder the reserve step (#286).
        let new_slots = postings
            .len()
            .min(self.file_count)
            .saturating_sub(self.scores.len());
        if new_slots > 0 {
            self.scores.reserve(new_slots);
            if let Some(cache) = self.meta_cache.as_mut() {
                // Additive reserve; on lang-filtered runs scores.len() < cache.len()
                // is possible (filtered postings enter the cache but not scores), so
                // `new_slots` is a lower bound — never under-sizes the cache (#286).
                cache.reserve(new_slots);
            }
        }

        // Per-n-gram IDF memoization: at most one distinct value per language.
        // Avoid HashMap overhead — track only the last seen (lang, idf) pair.
        // P2 (#286): scalar cache already collapses O(postings) IDF lookups to
        // O(distinct-langs-in-run); no array needed (ADR-003, closed-by-#284).
        //
        // AC8 (#286): the scalar `last_lang`/`last_idf` cache is reset each
        // `score_postings` call (per-n-gram scope).  Score-equivalence vs. a
        // naive no-cache reference is verified in test `ac8_scalar_idf_cache_score_equivalence`.
        let mut last_lang: Option<Language> = None;
        let mut last_idf: f64 = UNKNOWN_LANG_IDF;

        for posting in postings {
            let lite = match self.meta_cache.as_mut() {
                Some(cache) => cached_lite_meta(reader, cache, posting.doc_id)?,
                None => reader.file_lang_and_node_count(posting.doc_id)?.into(),
            };

            // Recover Language from the stored lang_id via LiteMeta::language(),
            // keeping lang-id → Language recovery in one place per struct (#286).
            let language = lite.language();

            // P4 (#286): skip this posting before insertion if the lang filter is
            // active and this file's language doesn't match (avoids PF-006: filter
            // is purely additive narrowing; lang=None path is byte-identical).
            // AC3 (#286): unknown lang_id (lang_from_id returns None) is skipped
            // when a lang filter is active — consistent with pre-P4 behaviour
            // where an unknown lang never matches Some(lang).
            if lang_filter.is_some_and(|req| language != Some(req)) {
                continue;
            }

            // Per-posting lang→idf lookup, memoized by last-value scalar cache.
            // `lang_from_id` (called inside `lite.language()` above) is a cheap
            // match/jump table with no I/O; calling it once per posting is
            // performance-neutral (#286).
            let idf = idf_for_language(language, &mut last_lang, &mut last_idf, &idf_fn);

            *self.scores.entry(posting.doc_id).or_insert(0.0) +=
                bm25_with_lite(posting, lite.node_count, avg, idf);
        }
        Ok(())
    }

    /// Return the `scores` map capacity after all postings have been processed.
    /// Used by tests to verify P3 posting-driven sizing (AC6/AC7, #286).
    #[cfg(test)]
    pub(super) fn scores_capacity(&self) -> usize {
        self.scores.capacity()
    }
}

/// Return the IDF for `language`, consulting and updating the per-n-gram scalar
/// cache (`last_lang`, `last_idf`) for P2 memoization (#286).
///
/// Extracted from `ScoringCtx::score_postings` to flatten the nesting depth of
/// the hot posting loop (was: `match` inside `if last_lang == Some(lang)` inside
/// `match language`).  Unknown / `None` languages use [`UNKNOWN_LANG_IDF`].
#[inline]
pub(super) fn idf_for_language(
    language: Option<Language>,
    last_lang: &mut Option<Language>,
    last_idf: &mut f64,
    idf_fn: &impl Fn(Language) -> f64,
) -> f64 {
    match language {
        Some(lang) if *last_lang == Some(lang) => *last_idf,
        Some(lang) => {
            let v = idf_fn(lang);
            *last_lang = Some(lang);
            *last_idf = v;
            v
        }
        None => UNKNOWN_LANG_IDF,
    }
}

/// BM25 score contribution for one n-gram posting, given a pre-computed IDF
/// and the file's `node_count`.
///
/// P1 (#286): Takes `node_count: u32` directly (from `LiteMeta`) instead of
/// a full `&AstFileMetaEntry`.
///
/// `tf_norm = tf / length_norm`; avdl==0 → length_norm=1.0; norm<=0 → 1.0
/// (defensive-only: with b=0.75 and nc>=0, n is always >= 0.25, so n<=0.0 is
/// unreachable in practice).
pub(super) fn bm25_with_lite(posting: &AstPosting, node_count: u32, avg: f64, idf: f64) -> f64 {
    debug_assert!(
        avg.is_finite(),
        "avg_node_count must be finite (AstPostingSource contract)"
    );
    debug_assert!(
        idf.is_finite() && idf > 0.0,
        "idf must be finite and positive (language IDF contract)"
    );
    // Release-safe fallback: treat non-finite avg as 0 → length_norm=1.0.
    let avg = if avg.is_finite() { avg } else { 0.0 };
    let tf = f64::from(posting.count);
    let nc = f64::from(node_count);
    let ln = if avg <= 0.0 {
        1.0
    } else {
        let n = 1.0 - AST_BM25_B + AST_BM25_B * (nc / avg);
        if n <= 0.0 { 1.0 } else { n }
    };
    let tf_norm = tf / ln;
    idf * (tf_norm / (tf_norm + AST_BM25_K1))
}

/// Fetch `LiteMeta` from FxHashMap cache; insert on miss (P1, #286).
///
/// Manual check-then-insert because `or_insert_with` cannot propagate `Result`.
pub(super) fn cached_lite_meta<R: AstPostingSource>(
    reader: &R,
    cache: &mut FxHashMap<u32, LiteMeta>,
    doc_id: u32,
) -> Result<LiteMeta> {
    if let Some(e) = cache.get(&doc_id) {
        return Ok(*e);
    }
    let lite = reader.file_lang_and_node_count(doc_id)?.into();
    cache.insert(doc_id, lite);
    Ok(lite)
}
