//! Skim Search - Code search foundation library
//!
//! # Architecture
//!
//! **IMPORTANT: This is a LIBRARY with NO I/O.**
//! - Accepts pre-parsed data, not file paths
//! - Returns Result types, not stdout writes
//! - Pure types and traits, no side effects
//!
//! CLI/binary code in `crates/rskim/src/cmd/search.rs` handles all I/O.

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
