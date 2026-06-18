//! Skim Search - Code search foundation library
//!
//! # Architecture
//!
//! - Core types (`types` module) are **pure**: no I/O, no side effects.
//! - The `ast_index` module linearises source files into pre-order AST node
//!   sequences and exposes the `linearize_source` entry point used by indexing.
//! - The `ast_weights` module maps AST node kinds to IDF-style weights derived
//!   from the node-frequency research corpus.
//! - The `index` module provides on-disk persistence via memory-mapped files.
//! - The `lexical` module implements BM25F scoring over per-field n-gram indexes.
//! - The `ngram` module handles bigram extraction (pure, no I/O).
//! - The `temporal` module parses git history via gix and computes risk scoring
//!   (hotspot, bug-fix density) with exponential decay. The `temporal::storage`
//!   sub-module persists temporal data to SQLite with WAL mode.
//! - The `cochange` module builds and queries a binary co-change matrix with
//!   Jaccard similarity from git history.
//! - The `weights` module provides composite n-gram weight tables combining
//!   AST and lexical signals.
//! - Returns Result types throughout — no panics in non-test code.
//!
//! CLI/binary code in `crates/rskim/src/cmd/search/mod.rs` handles user-facing I/O.

pub mod ast_index;
pub mod ast_weights;
pub mod cochange;
pub mod compound;
pub(crate) mod fields;
pub mod index;
pub(crate) mod io_util;
pub mod lexical;
pub mod ngram;
pub mod temporal;
mod types;
pub mod weights;

pub use ast_index::{
    AST_BM25_B, AST_BM25_K1, AST_CACHE_FILENAME, AST_CACHE_FORMAT_VERSION, AstBigram,
    AstBigramEntry, AstFileMetaEntry, AstIndexBuilder, AstIndexReader, AstNgramCache, AstNgramSet,
    AstPosting, AstPostingSource, AstQuery, AstQueryEngine, AstTrigram, AstTrigramEntry,
    CachedAstEntry, DEFAULT_AST_WEIGHT, LinearNode, LinearizeResult, NodeKindId, Pattern,
    PatternCategory, StructuralMetrics, all_patterns, ast_bigram_idf, ast_trigram_idf,
    extract_ast_ngrams, extract_ast_ngrams_with_metrics, extract_ast_ngrams_with_weights,
    linearize_source, lookup_pattern, parse_ast_query, vocab_len, vocab_lookup, vocab_resolve,
};

/// Current on-disk format version for the AST index (`ast_index.skidx`).
///
/// Exposed at the crate level so CLI code (e.g. staleness self-heal in Wave 3g)
/// can compare against the value returned by [`AstIndexReader::index_version`]
/// without accessing crate-internal format constants.
///
/// This constant derives from the single source of truth in
/// `ast_index::store::format::FORMAT_VERSION`. A compile-time assertion below
/// keeps them in sync — bumping only one will fail the build immediately.
pub const AST_INDEX_FORMAT_VERSION: u16 = ast_index::store::format::FORMAT_VERSION;

// Compile-time guard: `AST_INDEX_FORMAT_VERSION` is currently defined as an alias
// for `FORMAT_VERSION`, so this assert is trivially true. It is kept as an explicit
// statement of the invariant so that any future refactoring (e.g. making it a
// separate literal) will fail the build if the two values diverge.
const _: () = assert!(
    AST_INDEX_FORMAT_VERSION == ast_index::store::format::FORMAT_VERSION,
    "AST_INDEX_FORMAT_VERSION must equal ast_index::store::format::FORMAT_VERSION"
);

pub use cochange::{COUPLING_MAX_FILES, CochangeMatrixBuilder, CochangeMatrixReader};
pub use compound::{
    CompositeWeights, RRF_K, WEIGHT_AST, WEIGHT_LEXICAL, intersect_and_rank, recompose_with_lexical,
};
// #200: N-signal composite ranking (UNION mode, 6-signal weights).
pub use compound::{
    CompositeWeights6, ImportGraph, ImportLanguage, WEIGHT6_AST, WEIGHT6_DIR_PROXIMITY,
    WEIGHT6_IMPORT_GRAPH, WEIGHT6_LEXICAL, WEIGHT6_STRUCTURAL_COUPLING, WEIGHT6_TEMPORAL,
    dir_proximity_score, merge_composite, merge_layer_scores, structural_coupling_score,
};
pub use index::{NgramIndexBuilder, NgramIndexReader};
pub use lexical::{
    BM25FConfig, FIELD_COUNT, MAX_QUERY_BYTES, QueryEngine, bm25f_score, classify_source,
    dominant_field,
};
pub use ngram::{
    BORDER_MULTIPLIER, Ngram, extract_ngrams, extract_ngrams_with_weights, extract_query_ngrams,
    extract_query_ngrams_with_weights,
};
pub use temporal::storage::{
    CochangeRow, HotspotRow, META_GIT_HEAD, META_LAST_UPDATED, MIN_COCHANGE_JACCARD, RiskRow,
    TemporalDb,
};
pub use temporal::{
    DEFAULT_HALF_LIFE_DAYS, GixSource, compute_file_risk_scores, compute_file_temporal_stats,
    decay_weight, is_fix_commit,
};
pub use types::{
    CochangeStats, CommitInfo, FieldClassifier, FileChangeInfo, FileId, FileRiskScores,
    FileTemporalStats, HistoryResult, IndexStats, LayerBuilder, NodeInfo, Result, SearchError,
    SearchField, SearchLayer, SearchQuery, SearchResult, TemporalFlags, TemporalMetadata,
    TemporalSource, byte_offset_to_line, compute_line_range,
};
pub use weights::{BIGRAM_WEIGHTS, DEFAULT_WEIGHT, bigram_weight};
