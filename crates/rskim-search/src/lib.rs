//! Skim Search - Code search foundation library
//!
//! # Architecture
//!
//! - Core types (`types` module) are **pure**: no I/O, no side effects.
//! - The `index` module provides on-disk persistence via memory-mapped files.
//! - The `ngram` module handles bigram extraction (pure, no I/O).
//! - Returns Result types throughout — no panics in non-test code.
//!
//! CLI/binary code in `crates/rskim/src/cmd/search.rs` handles user-facing I/O.

pub mod index;
pub mod ngram;
mod types;
pub mod weights;

pub use index::{NgramIndexBuilder, NgramIndexReader};
pub use ngram::{
    BORDER_MULTIPLIER, Ngram, extract_ngrams, extract_ngrams_with_weights, extract_query_ngrams,
    extract_query_ngrams_with_weights,
};
pub use types::{
    FieldClassifier, FileId, IndexStats, LayerBuilder, NodeInfo, Result, SearchError, SearchField,
    SearchLayer, SearchQuery, SearchResult, TemporalFlags,
};
pub use weights::{BIGRAM_WEIGHTS, DEFAULT_WEIGHT, bigram_weight};
