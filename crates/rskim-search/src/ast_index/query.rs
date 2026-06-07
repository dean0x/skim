//! AST Structural Pattern Query Engine — Wave 3f (#197).
//!
//! Answers named-pattern and containment queries with OR-union additive BM25
//! ranking. Exposes a Wave-4 intersection hook (`search_ast`) and a
//! Wave-3g [`SearchLayer`] adapter.

use std::cmp::Ordering;
use std::hash::BuildHasherDefault;
use std::path::Path;

use rustc_hash::{FxHashMap, FxHasher};

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
const MAX_AST_QUERY_BYTES: usize = 4096;

/// Shared error message for empty query strings — used in both `SearchLayer::search`
/// and `parse_ast_query` so the two sites cannot silently drift.
const EMPTY_QUERY_MSG: &str = "empty AST query";

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
/// contract — they are `Copy`, gix-free, and mmap-free — so coupling to them
/// is bounded and intentional, mirroring the "free of gix types" note on
/// `CommitInfo` in `types.rs`.
///
/// **Finiteness contract.** `avg_node_count()` MUST return a finite, non-NaN
/// value. `node_count` values in `AstFileMetaEntry` MUST be non-negative.
/// The production reader validates these at header/entry decode time; custom
/// implementations must uphold the same contract.
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
                self.run_ngram_set(&set)
            }
            AstQuery::Containment(set) => self.run_ngram_set(set),
        }
    }

    fn run_ngram_set(&self, set: &AstNgramSet) -> Result<Vec<(FileId, f64)>> {
        let avg = f64::from(self.reader.avg_node_count());
        let capacity = self.reader.file_count() as usize;

        // Use FxHashMap (integer keys, trusted in-range doc_ids) with a capacity
        // hint to avoid rehashing on the hot insert path.
        let mut scores: FxHashMap<u32, f64> = FxHashMap::with_capacity_and_hasher(
            capacity,
            BuildHasherDefault::<FxHasher>::default(),
        );

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

        // Per-call meta cache: skip for single-n-gram queries — by contract C1,
        // each posting list has at most one posting per doc_id, so cross-n-gram
        // cache hits only occur when total_ngrams > 1.
        let mut meta_cache: Option<FxHashMap<u32, AstFileMetaEntry>> = if total_ngrams > 1 {
            Some(FxHashMap::with_capacity_and_hasher(
                capacity,
                BuildHasherDefault::<FxHasher>::default(),
            ))
        } else {
            None
        };

        for entry in &bigrams {
            let postings = self.reader.lookup_bigram(entry.ngram)?;
            score_postings(
                &postings,
                &mut scores,
                &mut meta_cache,
                &self.reader,
                avg,
                |lang| f64::from(ast_bigram_idf(lang, entry.ngram)),
            )?;
        }
        for entry in &trigrams {
            let postings = self.reader.lookup_trigram(entry.ngram)?;
            // DEFERRED (Wave 4): minimal-covering-set to remove trigram/sub-bigram
            // double-counting (#198). For now, contributions are additive.
            score_postings(
                &postings,
                &mut scores,
                &mut meta_cache,
                &self.reader,
                avg,
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
    /// Filters (in order): `file_filter` allowlist, `lang`, `offset`/`limit`.
    /// Defaults: `offset` → 0, `limit` → 20 (results truncated when unset).
    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        let raw = match &query.ast_pattern {
            None => return Ok(vec![]),
            Some(s) => s,
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(SearchError::InvalidQuery(EMPTY_QUERY_MSG.into()));
        }

        let mut hits = self.search_ast(&parse_ast_query(trimmed)?)?;
        // hits is FileId-ASC from search_ast.

        // Apply file_filter allowlist (no I/O).
        if let Some(ref filter) = query.file_filter {
            hits.retain(|(fid, _)| filter.contains(fid));
        }

        // Apply lang filter (one file_meta per surviving candidate).
        let mut filtered: Vec<(FileId, f64)> = if let Some(lang) = query.lang {
            let mut out = Vec::with_capacity(hits.len());
            for (fid, score) in hits {
                if self.reader.file_meta(fid.0)?.language() == Some(lang) {
                    out.push((fid, score));
                }
            }
            out
        } else {
            hits
        };

        // Sort score-DESC, FileId-ASC tie-break (NaN-safe; mirrors index/reader.rs sort).
        filtered.sort_unstable_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        Ok(filtered
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

// Scoring helpers

/// Accumulate BM25 scores for one set of postings into `scores`.
///
/// IDF is computed once per distinct language via a tiny last-value cache,
/// reducing O(postings) binary-search calls to O(distinct_languages).
///
/// When `meta_cache` is `None` (single-n-gram query), `file_meta` is called
/// directly without caching — by contract C1 each posting list has at most one
/// posting per doc_id, so cross-list cache hits never occur for a single n-gram.
fn score_postings<R: AstPostingSource>(
    postings: &[AstPosting],
    scores: &mut FxHashMap<u32, f64>,
    meta_cache: &mut Option<FxHashMap<u32, AstFileMetaEntry>>,
    reader: &R,
    avg: f64,
    idf_fn: impl Fn(Language) -> f64,
) -> Result<()> {
    // Per-n-gram IDF memoization: at most one distinct value per language.
    // Avoid HashMap overhead — track only the last seen (lang, idf) pair.
    let mut last_lang: Option<Language> = None;
    let mut last_idf: f64 = 1.0;

    for posting in postings {
        let meta = match meta_cache {
            Some(cache) => cached_meta_fx(reader, cache, posting.doc_id)?,
            None => reader.file_meta(posting.doc_id)?,
        };
        let idf = match meta.language() {
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
            None => 1.0,
        };
        *scores.entry(posting.doc_id).or_insert(0.0) += bm25_with_idf(posting, &meta, avg, idf);
    }
    Ok(())
}

/// BM25 score contribution for one n-gram posting, given a pre-computed IDF.
///
/// `tf_norm = tf / length_norm`; avdl==0 → length_norm=1.0; norm<=0 → 1.0
/// (defensive-only: with b=0.75 and nc>=0, n is always >= 0.25, so n<=0.0 is
/// unreachable in practice).
fn bm25_with_idf(posting: &AstPosting, meta: &AstFileMetaEntry, avg: f64, idf: f64) -> f64 {
    debug_assert!(
        avg.is_finite(),
        "avg_node_count must be finite (AstPostingSource contract)"
    );
    // Release-safe fallback: treat non-finite avg as 0 → length_norm=1.0.
    let avg = if avg.is_finite() { avg } else { 0.0 };
    let tf = f64::from(posting.count);
    let nc = f64::from(meta.node_count);
    let ln = if avg <= 0.0 {
        1.0
    } else {
        let n = 1.0 - AST_BM25_B + AST_BM25_B * (nc / avg);
        if n <= 0.0 { 1.0 } else { n }
    };
    let tf_norm = tf / ln;
    idf * (tf_norm / (tf_norm + AST_BM25_K1))
}

/// Fetch meta from FxHashMap cache; insert on miss.
///
/// Manual check-then-insert because `or_insert_with` cannot propagate `Result`.
fn cached_meta_fx<R: AstPostingSource>(
    reader: &R,
    cache: &mut FxHashMap<u32, AstFileMetaEntry>,
    doc_id: u32,
) -> Result<AstFileMetaEntry> {
    if let Some(e) = cache.get(&doc_id) {
        return Ok(*e);
    }
    let e = reader.file_meta(doc_id)?;
    cache.insert(doc_id, e);
    Ok(e)
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
