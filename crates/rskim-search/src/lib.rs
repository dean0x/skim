//! Skim Search - Code search foundation library
//!
//! # Architecture
//!
//! - Core types (`types` module) are **pure**: no I/O, no side effects.
//! - The `index` module provides on-disk persistence via memory-mapped files.
//! - The `ngram` module handles bigram extraction (pure, no I/O).
//! - The `temporal` module parses git history via gix for temporal scoring.
//! - Returns Result types throughout — no panics in non-test code.
//!
//! CLI/binary code in `crates/rskim/src/cmd/search/mod.rs` handles user-facing I/O.

pub mod cochange;
pub(crate) mod fields;
pub mod index;
pub mod lexical;
pub mod ngram;
pub mod temporal;
mod types;
pub mod weights;

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
pub use temporal::{GixSource, is_fix_commit};
pub use types::{
    CochangeStats, CommitInfo, FieldClassifier, FileChangeInfo, FileId, HistoryResult, IndexStats,
    LayerBuilder, NodeInfo, Result, SearchError, SearchField, SearchLayer, SearchQuery,
    SearchResult, TemporalFlags, TemporalMetadata, TemporalSource, byte_offset_to_line,
    compute_line_range,
};
pub use weights::{BIGRAM_WEIGHTS, DEFAULT_WEIGHT, bigram_weight};
