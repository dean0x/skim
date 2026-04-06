//! Error types for search and indexing operations.

use thiserror::Error;

/// Errors that can occur during search and indexing operations.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum SearchError {
    /// An I/O error occurred while reading or writing index files.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// An error occurred while building (writing) the index.
    #[error("Index error: {0}")]
    IndexBuildError(String),

    /// The query is malformed or contains unsupported constructs.
    #[error("Invalid query: {0}")]
    InvalidQuery(String),

    /// An error propagated from `rskim-core`.
    #[error("Core error: {0}")]
    CoreError(#[from] rskim_core::SkimError),

    /// A serialization or deserialization error occurred.
    #[error("Serialization error: {0}")]
    SerializationError(String),

    /// The index file is corrupted or has an incompatible format version.
    #[error("Corrupted index at {path}: {reason}")]
    CorruptedIndex {
        /// Path to the corrupted index file.
        path: String,
        /// Human-readable description of the corruption.
        reason: String,
    },
}

/// Result type alias for search operations.
pub type Result<T> = std::result::Result<T, SearchError>;
