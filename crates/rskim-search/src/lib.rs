//! Search and indexing layer for skim's code intelligence
//!
//! # Architecture
//!
//! This crate provides the 3-layer search architecture:
//! - **Layer 1**: Lexical indexing (BM25F with field boosting)
//! - **Layer 2**: AST n-gram indexing (structural code patterns)
//! - **Layer 3**: Temporal signals (git-aware recency and change patterns)
//!
//! All types and traits are defined here. Layer implementations
//! are added in subsequent waves.
//!
//! # Design Principles
//!
//! 1. **I/O-free types** - Core types have no filesystem dependencies
//! 2. **Trait-first** - Layers implement `SearchLayer`, built via `LayerBuilder`
//! 3. **FileId indirection** - All layers reference files by `FileId`, resolved via `FileTable`

mod traits;
mod types;

pub use traits::{FieldClassifier, LayerBuilder, SearchLayer};
pub use types::{
    FileId, FileTable, IndexStats, LineRange, MatchSpan, Result, SearchError, SearchField,
    SearchQuery, SearchResult, TemporalFlags,
};
