//! Search and indexing layer for skim's code intelligence
//!
//! # Architecture
//!
//! This crate provides the 3-layer search architecture:
//! - **Layer 1**: Lexical indexing (BM25F with field boosting)
//! - **Layer 2**: AST n-gram indexing (structural code patterns)
//! - **Layer 3**: Temporal signals (git-aware recency and change patterns)
//!
//! # Design Principles
//!
//! 1. **I/O-free types** - Core types have no filesystem dependencies
//! 2. **Trait-first** - Layers implement `SearchLayer`, built via `LayerBuilder`
//! 3. **FileId indirection** - All layers reference files by `FileId`, resolved via `FileTable`

pub mod fields;
pub mod lexical;
mod traits;
mod types;

pub use lexical::fxhash_bytes;
pub use traits::{FieldClassifier, LayerBuilder, SearchIndex, SearchLayer};
pub use types::{
    FileId, FileTable, IndexStats, LineRange, MatchSpan, Result, SearchError, SearchField,
    SearchQuery, SearchResult, TemporalFlags, MAX_FILE_TABLE_ENTRIES,
};
