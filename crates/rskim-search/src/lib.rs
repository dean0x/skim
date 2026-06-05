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
pub(crate) mod fields;
pub mod index;
pub(crate) mod io_util;
pub mod lexical;
pub mod ngram;
pub mod temporal;
mod types;
pub mod weights;

pub use ast_index::{
    AstBigram, AstBigramEntry, AstFileMetaEntry, AstIndexBuilder, AstIndexReader, AstNgramSet,
    AstPosting, AstTrigram, AstTrigramEntry, DEFAULT_AST_WEIGHT, LinearNode, LinearizeResult,
    NodeKindId, ast_bigram_idf, ast_trigram_idf, extract_ast_ngrams,
    extract_ast_ngrams_with_weights, linearize_source, vocab_len, vocab_lookup, vocab_resolve,
};
pub use cochange::{CochangeMatrixBuilder, CochangeMatrixReader};
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
    CochangeRow, HotspotRow, META_GIT_HEAD, META_LAST_UPDATED, RiskRow, TemporalDb,
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
