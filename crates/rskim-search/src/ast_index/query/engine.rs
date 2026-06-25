//! AST query engine — dispatch, execution, and Wave-3g `SearchLayer` adapter.
//!
//! [`AstQueryEngine`] is the immutable, `Send + Sync` entry point for both
//! direct structural queries (`search_ast`) and the `SearchLayer` search path.
//!
//! [`AstQuery`] is defined in [`super::parse`] (its sole constructor) so
//! that dependencies flow `engine → parse` in one direction only.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::path::Path;

use rskim_core::Language;

use super::adapter::AstPostingSource;
use super::parse::{AstQuery, parse_ast_query};
use super::scoring::ScoringCtx;
use crate::{
    FileId, Result, SearchError,
    ast_index::{
        AstBigramEntry, AstIndexReader, AstNgramSet, AstTrigramEntry, ast_bigram_idf,
        ast_trigram_idf,
    },
    types::{SearchField, SearchLayer, SearchQuery, SearchResult},
};

/// AST structural pattern query engine. Immutable; `&self`-only; `Send + Sync`.
///
/// Use [`AstQueryEngine::new`] for DI (tests, Wave 4) or
/// [`AstQueryEngine::open`] for CLI convenience.
pub struct AstQueryEngine<R: AstPostingSource = AstIndexReader> {
    pub(super) reader: R,
}

impl<R: AstPostingSource> AstQueryEngine<R> {
    /// Wrap any [`AstPostingSource`].
    #[must_use]
    pub fn new(reader: R) -> Self {
        Self { reader }
    }

    /// Execute a structural query; returns `(FileId, score)` sorted **ascending
    /// by FileId**, unique, all scores > 0. Wave-4 merge-join contract.
    ///
    /// OR-union BM25: every file with ≥1 matching n-gram is a candidate.
    /// `score = Σ idf · (tf_norm / (tf_norm + k1))`.
    ///
    /// # Errors
    /// - [`SearchError::InvalidQuery`] for [`AstQuery::SingleNode`] (→ #283).
    /// - [`SearchError::IndexCorrupted`] on corrupt backing index.
    pub fn search_ast(&self, q: &AstQuery) -> Result<Vec<(FileId, f64)>> {
        let set = ast_query_to_ngram_set(q)?;
        self.run_ngram_set(set.as_ref(), None)
    }

    /// Inner scoring loop for both `search_ast` (no lang filter) and the
    /// `SearchLayer` path (optional lang filter, P4 #286).
    ///
    /// `lang_filter` — when `Some(L)`, postings whose `lang_id` does not
    /// map to `L` are skipped before insertion into `scores`.  The public
    /// `search_ast` always passes `None` (Wave-4 merge-join contract: results
    /// are UNFILTERED, see AC12).
    pub(super) fn run_ngram_set(
        &self,
        set: &AstNgramSet,
        lang_filter: Option<Language>,
    ) -> Result<Vec<(FileId, f64)>> {
        let ctx = self.score_ngram_set(set, lang_filter)?;
        let out = ctx.into_sorted_vec();

        // B2: unique (FxHashMap), all > 0 (BM25 with C4: count>=1 → tf>0 → score>0),
        // sorted FileId-ASC (Wave-4 contract).
        debug_assert!(out.iter().all(|(_, s)| *s > 0.0), "all scores must be > 0");
        Ok(out)
    }

    /// Build and populate a [`ScoringCtx`] for `set` + `lang_filter`.
    ///
    /// Shared by `run_ngram_set` (production) and `run_ngram_set_with_capacity`
    /// (test-only capacity hook) to eliminate duplicated dedup + scoring loop code.
    pub(super) fn score_ngram_set(
        &self,
        set: &AstNgramSet,
        lang_filter: Option<Language>,
    ) -> Result<ScoringCtx> {
        let avg = f64::from(self.reader.avg_node_count());

        // Gap-fix #6: dedup by key (entries are sorted; O(n); prevents double-scoring dups).
        let mut bigrams: Vec<&AstBigramEntry> = set.bigrams.iter().collect();
        bigrams.dedup_by_key(|e| e.ngram.key());
        debug_assert!(
            bigrams
                .windows(2)
                .all(|w| w[0].ngram.key() != w[1].ngram.key())
        );
        let mut trigrams: Vec<&AstTrigramEntry> = set.trigrams.iter().collect();
        trigrams.dedup_by_key(|e| e.ngram.key());
        debug_assert!(
            trigrams
                .windows(2)
                .all(|w| w[0].ngram.key() != w[1].ngram.key())
        );

        let total_ngrams = bigrams.len() + trigrams.len();

        // P3 (#286): posting-driven capacity — start at CAPACITY_FLOOR, reserve(n) per
        // posting list. Avoids over-allocating file_count() for selective queries (AC6)
        // and correctly handles an empty first list followed by a large second (AC7).
        let file_count = self.reader.file_count() as usize;

        // Per-call meta cache: skip for single-n-gram queries (C1: at most one posting
        // per doc_id per list, so cross-list cache hits only occur when total_ngrams > 1).
        // P1 (#286): value type is LiteMeta (5 bytes) not AstFileMetaEntry (15 bytes).
        let mut ctx = ScoringCtx::new(file_count, total_ngrams > 1);

        for entry in &bigrams {
            let postings = self.reader.lookup_bigram(entry.ngram)?;
            ctx.score_postings(&postings, &self.reader, avg, lang_filter, |lang| {
                f64::from(ast_bigram_idf(lang, entry.ngram))
            })?;
        }
        for entry in &trigrams {
            let postings = self.reader.lookup_trigram(entry.ngram)?;
            // DEFERRED (Wave 4): minimal-covering-set to remove trigram/sub-bigram
            // double-counting (#198). For now, contributions are additive.
            ctx.score_postings(&postings, &self.reader, avg, lang_filter, |lang| {
                f64::from(ast_trigram_idf(lang, entry.ngram))
            })?;
        }

        Ok(ctx)
    }
}

impl AstQueryEngine<AstIndexReader> {
    /// Open the index at `dir`. Surfaces all [`AstIndexReader::open`] errors.
    pub fn open(dir: &Path) -> Result<Self> {
        Ok(Self {
            reader: AstIndexReader::open(dir)?,
        })
    }
}

impl SearchLayer for AstQueryEngine<AstIndexReader> {
    /// `ast_pattern = None` → `Ok(vec![])` (Wave-4 no-op).
    /// `ast_pattern = Some("")` → `Err(InvalidQuery("empty AST query"))`.
    /// `ast_pattern = Some(s)` → parse + execute; apply filters; return score-DESC results.
    /// Filters (in order): `file_filter` allowlist, `lang` (folded into scoring,
    /// P4 #286), `offset`/`limit`.
    /// Defaults: `offset` → 0, `limit` → 20 (results truncated when unset).
    ///
    /// `bm25f_config` is intentionally ignored: the AST layer uses its own BM25
    /// parameterisation ([`super::scoring::AST_BM25_K1`] / [`super::scoring::AST_BM25_B`])
    /// and the lexical BM25F config has no meaning here.
    ///
    /// # Errors
    /// Returns [`SearchError::InvalidQuery`] when `temporal_flags` is set —
    /// temporal sorting is not yet supported on the AST layer (deferred to Wave 4).
    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        if query.temporal_flags.is_some() {
            return Err(SearchError::InvalidQuery(
                "temporal sorting (--hot / --cold / --risky) is not yet supported on the AST \
                 layer; omit temporal flags or use the lexical search layer"
                    .into(),
            ));
        }

        let raw = match &query.ast_pattern {
            None => return Ok(vec![]),
            Some(s) => s,
        };

        // Trim before parsing so that the >4096-byte length guard in
        // `parse_ast_query` applies to the actual query content rather than
        // incidental surrounding whitespace (restores pre-split behaviour; the
        // CLI path in ast.rs:109 also trims before calling).
        //
        // P4 (#286): fold the lang filter into scoring so the second
        // file_meta decode loop is eliminated.  `run_ngram_set` with
        // `lang_filter = Some(lang)` skips mismatched postings before
        // insertion — filter-application order becomes lang-then-file_filter
        // rather than file_filter-then-lang, but the final set is identical
        // (both are pure membership filters; AC11).
        //
        // `ast_query_to_ngram_set` is the single dispatch point for
        // AstQuery → AstNgramSet, shared with `search_ast` to eliminate
        // duplicated match arms and error strings (#286).
        let ast_q = parse_ast_query(raw.trim())?;
        let ngram_set = ast_query_to_ngram_set(&ast_q)?;

        let mut hits = self.run_ngram_set(ngram_set.as_ref(), query.lang)?;
        // hits is FileId-ASC from run_ngram_set; lang filter already applied.

        // Apply file_filter allowlist (no I/O).
        if let Some(ref filter) = query.file_filter {
            hits.retain(|(fid, _)| filter.contains(fid));
        }

        // Sort score-DESC, FileId-ASC tie-break (NaN-safe; mirrors index/reader.rs sort).
        hits.sort_unstable_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        Ok(hits
            .into_iter()
            .skip(query.offset.unwrap_or(0))
            .take(query.limit.unwrap_or(20))
            .map(|(fid, score)| SearchResult {
                file_id: fid,
                score,
                line_range: 0..0,
                match_positions: vec![],
                field: SearchField::Other,
                snippet: None,
            })
            .collect())
    }

    fn name(&self) -> &str {
        "ast"
    }
}

// Test-only hook: run_ngram_set and also return the scores capacity after
// scoring, so tests can assert P3 posting-driven sizing without relying on
// internal FxHashMap growth heuristics.
#[cfg(test)]
impl<R: AstPostingSource> AstQueryEngine<R> {
    /// Like `run_ngram_set` but also returns the `scores` map capacity after
    /// all postings have been processed.  Used by AC6/AC7 tests to verify that
    /// P3 reserves proportional to posting-list length rather than `file_count`
    /// (#286).
    pub(super) fn run_ngram_set_with_capacity(
        &self,
        set: &AstNgramSet,
        lang_filter: Option<Language>,
    ) -> Result<(Vec<(FileId, f64)>, usize)> {
        let ctx = self.score_ngram_set(set, lang_filter)?;
        let cap = ctx.scores_capacity();
        Ok((ctx.into_sorted_vec(), cap))
    }
}

/// Resolve an [`AstQuery`] to its [`AstNgramSet`], returning a borrowed or
/// owned value depending on the variant.
///
/// This is the single `AstQuery → AstNgramSet` dispatch point, shared by
/// [`AstQueryEngine::search_ast`] and [`SearchLayer::search`] so the match
/// arms and [`SearchError::InvalidQuery`] message for `SingleNode` cannot
/// silently drift between the two call sites (#286).
///
/// Returns `Err(InvalidQuery)` for [`AstQuery::SingleNode`] (→ #283).
pub(super) fn ast_query_to_ngram_set(q: &AstQuery) -> Result<Cow<'_, AstNgramSet>> {
    match q {
        AstQuery::SingleNode(_) => Err(SearchError::InvalidQuery(
            "single-node structural search requires the unigram index — tracked in #283".into(),
        )),
        AstQuery::Pattern(pattern) => {
            Ok(Cow::Owned(crate::ast_index::pattern_to_query_set(pattern)))
        }
        // Borrow directly — no clone on the hot Containment path (#286).
        AstQuery::Containment(set) => Ok(Cow::Borrowed(set)),
    }
}
