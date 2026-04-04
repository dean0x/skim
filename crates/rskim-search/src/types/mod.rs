//! Core type definitions for rskim-search.
//!
//! ARCHITECTURE: This module defines ALL types used across the search layer.
//! Design principle: I/O-free types with explicit error handling.
//! All types follow the rskim-core pattern: thiserror for errors, explicit derives.

mod error;
mod file_table;
mod query;
mod result;

pub use error::{Result, SearchError};
pub use file_table::{FileId, FileTable, MAX_FILE_TABLE_ENTRIES};
pub use query::{SearchField, SearchQuery, TemporalFlags};
pub use result::{IndexStats, LineRange, MatchSpan, SearchResult};
