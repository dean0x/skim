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

mod types;

pub use types::{
    FieldClassifier, FileId, IndexStats, LayerBuilder, NodeInfo, Result, SearchError, SearchField,
    SearchLayer, SearchQuery, SearchResult, TemporalFlags,
};
