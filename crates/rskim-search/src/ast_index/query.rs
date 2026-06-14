//! AST Structural Pattern Query Engine — Wave 3f (#197), Wave 4 perf (#286).
//!
//! Answers named-pattern and containment queries with OR-union additive BM25
//! ranking. Exposes a Wave-4 intersection hook (`search_ast`) and a
//! Wave-3g [`SearchLayer`] adapter.
//!
//! # Wave 4 performance changes (#286)
//!
//! - **P1 (partial decode)**: `score_postings` calls `file_lang_and_node_count`
//!   instead of `file_meta`, decoding only `lang_id` + `node_count` (5 bytes)
//!   rather than the full 15-byte record.
//! - **P2 (scalar IDF cache)**: The `last_lang`/`last_idf` scalar cache
//!   introduced post-#284 already collapses O(postings) IDF lookups to
//!   O(distinct-langs-in-run).  The mixed-language bench confirms no thrash.
//!   Closed-by-#284-refactor; no `LANG_COUNT` constant introduced (ADR-003).
//! - **P3 (capacity sizing)**: `run_ngram_set` collects ALL posting lists first,
//!   measures the maximum list length, and sizes `scores`/`meta_cache` from
//!   that instead of `file_count()`.  Solves both the over-allocation (broad
//!   queries) and the empty-first-list under-sizing (AC7) cases.
//! - **P4 (lang filter fold-in)**: `run_ngram_set` accepts an optional
//!   `lang_filter`; when set, each posting is skipped before insertion if its
//!   `lang_id` does not match, eliminating the second per-file `file_meta`
//!   decode loop that previously ran in `SearchLayer::search`.

use std::cmp::Ordering;
use std::path::Path;

use rustc_hash::FxHashMap;

use rskim_core::Language;

use super::patterns::Pattern;
use super::{
    AstBigram, AstBigramEntry, AstFileMetaEntry, AstIndexReader, AstNgramSet, AstPosting,
    AstTrigram, AstTrigramEntry, DEFAULT_AST_WEIGHT, NodeKindId, ast_bigram_idf, ast_trigram_idf,
    lookup_pattern, vocab_lookup,
};
use crate::{
    FileId, Result, SearchError,
    types::{SearchField, SearchLayer, SearchQuery, SearchResult},
};

// BM25 constants
/// BM25 saturation parameter k1 for AST structural scoring.
pub const AST_BM25_K1: f64 = 1.2;
/// BM25 length-normalisation parameter b for AST structural scoring.
pub const AST_BM25_B: f64 = 0.75;
/// Maximum allowed byte length for a raw query string (reliability bound).
/// Aliased from [`crate::lexical::MAX_QUERY_BYTES`] so both layers share one source of truth.
const MAX_AST_QUERY_BYTES: usize = crate::lexical::MAX_QUERY_BYTES;

/// Shared error message for empty query strings — used in both `SearchLayer::search`
/// and `parse_ast_query` so the two sites cannot silently drift.
const EMPTY_QUERY_MSG: &str = "empty AST query";

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
const CAPACITY_FLOOR: usize = 16;

// Query enum
/// A parsed, validated AST structural query.
///
/// Created exclusively via [`parse_ast_query`] — the only `String → AstQuery`
/// boundary.
#[derive(Debug, Clone)]
pub enum AstQuery {
    /// Named catalog pattern (e.g. `"try-catch"`). Resolved at execution time.
    Pattern(&'static Pattern),
    /// Depth-1 bigram (`A > B`) or depth-2 trigram (`A > B > C`); deduped.
    Containment(AstNgramSet),
    /// Validated single node kind. Execution deferred to #283 (unigram index).
    SingleNode(NodeKindId),
}

impl PartialEq for AstQuery {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Pattern(a), Self::Pattern(b)) => std::ptr::eq(*a, *b),
            (Self::Containment(a), Self::Containment(b)) => a == b,
            (Self::SingleNode(a), Self::SingleNode(b)) => a == b,
            _ => false,
        }
    }
}

// DI seam
/// Dependency-injection seam: implemented by [`AstIndexReader`] and test fakes.
///
/// **Value-type coupling is intentional and bounded.** `AstPosting` and
/// `AstFileMetaEntry` are deliberately treated as the stable query-layer value
/// contract — they are `Copy`, gix-free, and mmap-free — mirroring the
/// "free of gix types" note on `CommitInfo` in `types.rs`.
///
/// **Finiteness contract.** `avg_node_count()` MUST return a finite, non-NaN
/// value. `node_count` values in `AstFileMetaEntry` MUST be non-negative.
/// The production reader validates these at header/entry decode time; custom
/// implementations must uphold the same contract.
///
/// **Count contract (C4).** `count` in every returned [`AstPosting`] MUST be
/// `>= 1`. Sources returning `count == 0` break the all-scores-positive
/// invariant (`count >= 1 → tf > 0 → score > 0`) relied on by BM25 and the
/// `debug_assert!` in [`AstQueryEngine::run_ngram_set`].
pub trait AstPostingSource: Send + Sync {
    /// Look up postings for an [`AstBigram`]; `Ok(vec![])` when absent (C2).
    fn lookup_bigram(&self, b: AstBigram) -> Result<Vec<AstPosting>>;
    /// Look up postings for an [`AstTrigram`]; `Ok(vec![])` when absent (C2).
    fn lookup_trigram(&self, t: AstTrigram) -> Result<Vec<AstPosting>>;
    /// Per-file metadata for `doc_id`; `Err(IndexCorrupted)` when out of range.
    fn file_meta(&self, doc_id: u32) -> Result<AstFileMetaEntry>;
    /// Average per-file node count across the corpus. MUST be finite and non-NaN.
    fn avg_node_count(&self) -> f32;
    /// Total number of files in the index.
    fn file_count(&self) -> u32;
    /// Partial decode — returns `(lang_id, node_count)` for `doc_id`.
    ///
    /// This is the hot-path accessor called by `score_postings` (#286 P1).
    /// The default implementation delegates to `file_meta` so test fakes
    /// compiled against the trait before this method existed continue to work.
    /// The production [`AstIndexReader`] overrides with a fast path that
    /// decodes only bytes `[0..5]` of the 15-byte on-disk record.
    ///
    /// **Contract**: for any in-range `doc_id`, the returned `(u8, u32)` equals
    /// `(file_meta(doc_id)?.lang_id, file_meta(doc_id)?.node_count)`.  For an
    /// out-of-range `doc_id` it returns the same `Err(IndexCorrupted)` as
    /// `file_meta`.
    fn file_lang_and_node_count(&self, doc_id: u32) -> Result<(u8, u32)> {
        let m = self.file_meta(doc_id)?;
        Ok((m.lang_id, m.node_count))
    }
}

impl AstPostingSource for AstIndexReader {
    fn lookup_bigram(&self, b: AstBigram) -> Result<Vec<AstPosting>> {
        AstIndexReader::lookup_bigram(self, b)
    }
    fn lookup_trigram(&self, t: AstTrigram) -> Result<Vec<AstPosting>> {
        AstIndexReader::lookup_trigram(self, t)
    }
    fn file_meta(&self, doc_id: u32) -> Result<AstFileMetaEntry> {
        AstIndexReader::file_meta(self, doc_id)
    }
    fn avg_node_count(&self) -> f32 {
        AstIndexReader::avg_node_count(self)
    }
    fn file_count(&self) -> u32 {
        AstIndexReader::file_count(self)
    }
    /// Override with the fast path: decode only `lang_id` + `node_count` (5 bytes).
    fn file_lang_and_node_count(&self, doc_id: u32) -> Result<(u8, u32)> {
        AstIndexReader::file_lang_and_node_count(self, doc_id)
    }
}

// Lite scoring metadata (P1, #286)
/// Minimal file metadata needed for BM25 scoring: `lang_id` and `node_count`.
///
/// Used as the value type in `meta_cache` (replacing `AstFileMetaEntry`) so
/// the cache footprint is 5 bytes per entry instead of 15 bytes.
#[derive(Clone, Copy)]
struct LiteMeta {
    lang_id: u8,
    node_count: u32,
}

// Query engine
/// AST structural pattern query engine. Immutable; `&self`-only; `Send + Sync`.
///
/// Use [`AstQueryEngine::new`] for DI (tests, Wave 4) or
/// [`AstQueryEngine::open`] for CLI convenience.
pub struct AstQueryEngine<R: AstPostingSource = AstIndexReader> {
    reader: R,
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
        match q {
            AstQuery::SingleNode(_) => Err(SearchError::InvalidQuery(
                "single-node structural search requires the unigram index — tracked in #283".into(),
            )),
            AstQuery::Pattern(pattern) => {
                let set = crate::ast_index::pattern_to_query_set(pattern);
                self.run_ngram_set(&set, None)
            }
            AstQuery::Containment(set) => self.run_ngram_set(set, None),
        }
    }

    /// Inner scoring loop for both `search_ast` (no lang filter) and the
    /// `SearchLayer` path (optional lang filter, P4 #286).
    ///
    /// `lang_filter` — when `Some(L)`, postings whose `lang_id` does not
    /// map to `L` are skipped before insertion into `scores`.  The public
    /// `search_ast` always passes `None` (Wave-4 merge-join contract: results
    /// are UNFILTERED, see AC12).
    fn run_ngram_set(
        &self,
        set: &AstNgramSet,
        lang_filter: Option<Language>,
    ) -> Result<Vec<(FileId, f64)>> {
        let avg = f64::from(self.reader.avg_node_count());

        // Gap-fix #6: dedup by key (entries are sorted; O(n); prevents double-scoring dups).
        let mut bigrams: Vec<&AstBigramEntry> = set.bigrams.iter().collect();
        bigrams.dedup_by_key(|e| e.ngram.key());
        debug_assert!({
            bigrams
                .windows(2)
                .all(|w| w[0].ngram.key() != w[1].ngram.key())
        });
        let mut trigrams: Vec<&AstTrigramEntry> = set.trigrams.iter().collect();
        trigrams.dedup_by_key(|e| e.ngram.key());
        debug_assert!({
            trigrams
                .windows(2)
                .all(|w| w[0].ngram.key() != w[1].ngram.key())
        });

        let total_ngrams = bigrams.len() + trigrams.len();

        // P3 (#286): posting-driven, single-pass capacity with reserve().
        //
        // Strategy: start with `CAPACITY_FLOOR`, then call `scores.reserve(n)`
        // before processing each posting list of length `n`.  This means the
        // map grows at most once per n-gram rather than rehashing during insert,
        // while never over-allocating to `file_count()` for selective queries.
        //
        // AC6 (broad): for a posting list of length ≥ file_count the initial
        // reserve brings the capacity to file_count — no further rehash occurs.
        // AC7 (empty-first): an empty first list does not reserve anything;
        // subsequent large lists call reserve just before their insertions.
        // The floor at CAPACITY_FLOOR (see constant docs) prevents pathological
        // grow-from-1 churn on the very first posting.
        let file_count = self.reader.file_count() as usize;

        // Use FxHashMap (integer keys, trusted in-range doc_ids).
        let mut scores: FxHashMap<u32, f64> =
            FxHashMap::with_capacity_and_hasher(CAPACITY_FLOOR, Default::default());

        // Per-call meta cache: skip for single-n-gram queries — by contract C1,
        // each posting list has at most one posting per doc_id, so cross-n-gram
        // cache hits only occur when total_ngrams > 1.
        //
        // P1 (#286): Value type shrunk from `AstFileMetaEntry` (15 bytes) to
        // `LiteMeta` (5 bytes).  BM25 scoring only needs `lang_id` and
        // `node_count`; the other fields are only needed for `file_metrics`.
        let mut meta_cache: Option<FxHashMap<u32, LiteMeta>> = if total_ngrams > 1 {
            Some(FxHashMap::with_capacity_and_hasher(
                CAPACITY_FLOOR,
                Default::default(),
            ))
        } else {
            None
        };

        for entry in &bigrams {
            let postings = self.reader.lookup_bigram(entry.ngram)?;
            // Reserve only new slots: total entries ≤ file_count, so clamp to
            // avoid over-allocation when scores already contains overlapping docs.
            let new_slots = postings.len().min(file_count).saturating_sub(scores.len());
            if new_slots > 0 {
                scores.reserve(new_slots);
                if let Some(ref mut cache) = meta_cache {
                    cache.reserve(new_slots);
                }
            }
            score_postings(
                &postings,
                &mut scores,
                &mut meta_cache,
                &self.reader,
                avg,
                lang_filter,
                |lang| f64::from(ast_bigram_idf(lang, entry.ngram)),
            )?;
        }
        for entry in &trigrams {
            let postings = self.reader.lookup_trigram(entry.ngram)?;
            // DEFERRED (Wave 4): minimal-covering-set to remove trigram/sub-bigram
            // double-counting (#198). For now, contributions are additive.
            let new_slots = postings.len().min(file_count).saturating_sub(scores.len());
            if new_slots > 0 {
                scores.reserve(new_slots);
                if let Some(ref mut cache) = meta_cache {
                    cache.reserve(new_slots);
                }
            }
            score_postings(
                &postings,
                &mut scores,
                &mut meta_cache,
                &self.reader,
                avg,
                lang_filter,
                |lang| f64::from(ast_trigram_idf(lang, entry.ngram)),
            )?;
        }

        let mut out: Vec<(FileId, f64)> =
            scores.into_iter().map(|(id, s)| (FileId(id), s)).collect();

        // B2: unique (FxHashMap), all > 0 (BM25 with C4: count>=1 → tf>0 → score>0),
        // sorted FileId-ASC (Wave-4 contract).
        debug_assert!(out.iter().all(|(_, s)| *s > 0.0), "all scores must be > 0");
        out.sort_unstable_by_key(|(fid, _)| *fid);
        Ok(out)
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

// SearchLayer adapter (Wave 3g)
impl SearchLayer for AstQueryEngine<AstIndexReader> {
    /// `ast_pattern = None` → `Ok(vec![])` (Wave-4 no-op).
    /// `ast_pattern = Some("")` → `Err(InvalidQuery("empty AST query"))`.
    /// `ast_pattern = Some(s)` → parse + execute; apply filters; return score-DESC results.
    /// Filters (in order): `file_filter` allowlist, `lang` (folded into scoring,
    /// P4 #286), `offset`/`limit`.
    /// Defaults: `offset` → 0, `limit` → 20 (results truncated when unset).
    ///
    /// `bm25f_config` is intentionally ignored: the AST layer uses its own BM25
    /// parameterisation ([`AST_BM25_K1`] / [`AST_BM25_B`]) and the lexical BM25F
    /// config has no meaning here.
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
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(SearchError::InvalidQuery(EMPTY_QUERY_MSG.into()));
        }

        // P4 (#286): fold the lang filter into scoring so the second
        // file_meta decode loop is eliminated.  `run_ngram_set` with
        // `lang_filter = Some(lang)` skips mismatched postings before
        // insertion — filter-application order becomes lang-then-file_filter
        // rather than file_filter-then-lang, but the final set is identical
        // (both are pure membership filters; AC11).
        let ast_q = parse_ast_query(trimmed)?;
        let ngram_set = match &ast_q {
            AstQuery::SingleNode(_) => {
                return Err(SearchError::InvalidQuery(
                    "single-node structural search requires the unigram index — tracked in #283"
                        .into(),
                ));
            }
            AstQuery::Pattern(pattern) => crate::ast_index::pattern_to_query_set(pattern),
            AstQuery::Containment(set) => set.clone(),
        };

        let mut hits = self.run_ngram_set(&ngram_set, query.lang)?;
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

// Parser
/// Parse a raw string into an [`AstQuery`].
///
/// **Only** `String → AstQuery` boundary; total (never panics). Rejects
/// strings longer than `MAX_AST_QUERY_BYTES` (4096 bytes).
///
/// | Input form | Result |
/// |---|---|
/// | `"try-catch"` | [`AstQuery::Pattern`] (hyphen → catalog lookup) |
/// | `"A > B"` | [`AstQuery::Containment`] bigram |
/// | `"A > B > C"` | [`AstQuery::Containment`] trigram |
/// | `"try_statement"` | [`AstQuery::SingleNode`] (vocab-validated) |
///
/// Returns [`SearchError::InvalidQuery`] for unknown kinds/patterns, empty
/// segments, `>>`, depth > 2, or inputs > 4096 bytes.
pub fn parse_ast_query(s: &str) -> Result<AstQuery> {
    if s.len() > MAX_AST_QUERY_BYTES {
        return Err(SearchError::InvalidQuery(format!(
            "AST query too long: {} bytes (max {MAX_AST_QUERY_BYTES})",
            s.len()
        )));
    }
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(SearchError::InvalidQuery(EMPTY_QUERY_MSG.into()));
    }
    if trimmed.contains(">>") {
        return Err(SearchError::InvalidQuery(
            "transitive ancestor operator `>>` is not supported; use `>` for direct containment"
                .into(),
        ));
    }

    let segments: Vec<&str> = trimmed.split('>').map(str::trim).collect();
    for seg in &segments {
        if seg.is_empty() {
            return Err(SearchError::InvalidQuery(
                "empty segment in query: check for trailing or doubled `>` operators".into(),
            ));
        }
    }

    match segments.len() {
        1 => parse_single(segments[0]),
        2 => parse_bigram(segments[0], segments[1]),
        3 => parse_trigram(segments[0], segments[1], segments[2]),
        n => Err(SearchError::InvalidQuery(format!(
            "containment depth > 2 is not supported ({n} segments); maximum is `A > B > C`"
        ))),
    }
}

fn parse_single(token: &str) -> Result<AstQuery> {
    if token.contains('-') {
        return Ok(AstQuery::Pattern(lookup_pattern(token)?));
    }
    vocab_lookup(token)
        .map(AstQuery::SingleNode)
        .ok_or_else(|| {
            SearchError::InvalidQuery(format!(
                "unknown node kind '{token}'; \
             use a valid tree-sitter node kind or a hyphenated pattern name"
            ))
        })
}

fn parse_bigram(a: &str, b: &str) -> Result<AstQuery> {
    let bigram = AstBigram::encode(kind(a)?, kind(b)?);
    Ok(AstQuery::Containment(AstNgramSet {
        bigrams: vec![AstBigramEntry {
            ngram: bigram,
            // weight/count unused on query path; meaningful only at index build.
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        }],
        trigrams: vec![],
    }))
}

fn parse_trigram(a: &str, b: &str, c: &str) -> Result<AstQuery> {
    let trigram = AstTrigram::encode(kind(a)?, kind(b)?, kind(c)?);
    Ok(AstQuery::Containment(AstNgramSet {
        bigrams: vec![],
        trigrams: vec![AstTrigramEntry {
            ngram: trigram,
            // weight/count unused on query path; meaningful only at index build.
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        }],
    }))
}

/// Resolve a containment segment to a [`NodeKindId`] or return `InvalidQuery`.
fn kind(seg: &str) -> Result<NodeKindId> {
    vocab_lookup(seg).ok_or_else(|| {
        SearchError::InvalidQuery(format!(
            "unknown node kind '{seg}' in containment query; \
             use a valid tree-sitter node kind (e.g. `function_item`, `block`)"
        ))
    })
}

/// IDF fallback for files whose language byte is unrecognised (neutral weight —
/// does not amplify or suppress the BM25 term-frequency contribution).
const UNKNOWN_LANG_IDF: f64 = 1.0;

// Scoring helpers

/// Accumulate BM25 scores for one set of postings into `scores`.
///
/// IDF is computed once per distinct language via a tiny last-value cache,
/// reducing O(postings) binary-search calls to O(distinct_languages).
///
/// P1 (#286): Uses `file_lang_and_node_count` instead of `file_meta` to
/// decode only the 5 bytes needed for BM25 scoring.  `meta_cache` now holds
/// `LiteMeta` (5 bytes) instead of `AstFileMetaEntry` (15 bytes).
///
/// P4 (#286): `lang_filter` — when `Some(L)`, a posting whose `lang_id` does
/// not resolve to `L` is skipped before insertion into `scores`.  When `None`,
/// all postings are scored (unfiltered; `search_ast` always passes `None`).
///
/// When `meta_cache` is `None` (single-n-gram query), the lite meta is fetched
/// directly without caching — by contract C1 each posting list has at most one
/// posting per doc_id, so cross-list cache hits never occur for a single n-gram.
fn score_postings<R: AstPostingSource>(
    postings: &[AstPosting],
    scores: &mut FxHashMap<u32, f64>,
    meta_cache: &mut Option<FxHashMap<u32, LiteMeta>>,
    reader: &R,
    avg: f64,
    lang_filter: Option<Language>,
    idf_fn: impl Fn(Language) -> f64,
) -> Result<()> {
    // Per-n-gram IDF memoization: at most one distinct value per language.
    // Avoid HashMap overhead — track only the last seen (lang, idf) pair.
    // P2 (#286): scalar cache already collapses O(postings) IDF lookups to
    // O(distinct-langs-in-run); no array needed (ADR-003, closed-by-#284).
    let mut last_lang: Option<Language> = None;
    let mut last_idf: f64 = UNKNOWN_LANG_IDF;

    for posting in postings {
        let lite = match meta_cache {
            Some(cache) => cached_lite_meta(reader, cache, posting.doc_id)?,
            None => {
                let (lang_id, node_count) = reader.file_lang_and_node_count(posting.doc_id)?;
                LiteMeta {
                    lang_id,
                    node_count,
                }
            }
        };

        // Recover Language from the stored lang_id.
        let language = crate::index::lang_map::lang_from_id(lite.lang_id);

        // P4 (#286): skip this posting before insertion if the lang filter is
        // active and this file's language doesn't match (avoids PF-006: filter
        // is purely additive narrowing; lang=None path is byte-identical).
        // AC3 (#286): unknown lang_id (lang_from_id returns None) is skipped
        // when a lang filter is active — consistent with pre-P4 behaviour
        // where an unknown lang never matches Some(lang).
        if let Some(required_lang) = lang_filter {
            match language {
                Some(l) if l == required_lang => {}
                _ => continue,
            }
        }

        let idf = match language {
            Some(lang) => {
                if last_lang == Some(lang) {
                    last_idf
                } else {
                    let v = idf_fn(lang);
                    last_lang = Some(lang);
                    last_idf = v;
                    v
                }
            }
            None => UNKNOWN_LANG_IDF,
        };
        *scores.entry(posting.doc_id).or_insert(0.0) +=
            bm25_with_lite(posting, lite.node_count, avg, idf);
    }
    Ok(())
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
fn bm25_with_lite(posting: &AstPosting, node_count: u32, avg: f64, idf: f64) -> f64 {
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
fn cached_lite_meta<R: AstPostingSource>(
    reader: &R,
    cache: &mut FxHashMap<u32, LiteMeta>,
    doc_id: u32,
) -> Result<LiteMeta> {
    if let Some(e) = cache.get(&doc_id) {
        return Ok(*e);
    }
    let (lang_id, node_count) = reader.file_lang_and_node_count(doc_id)?;
    let lite = LiteMeta {
        lang_id,
        node_count,
    };
    cache.insert(doc_id, lite);
    Ok(lite)
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
