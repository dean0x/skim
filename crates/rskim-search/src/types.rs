//! Core types, traits, and errors for skim search.
//!
//! # Architecture
//!
//! **IMPORTANT: This module contains pure types and traits with NO I/O.**
//! - All types are derived with appropriate traits
//! - Traits accept pre-parsed data, not file paths
//! - Error types use thiserror for ergonomic, typed handling
//! - CLI/binary code in `crates/rskim/src/cmd/search.rs` handles all I/O

use std::fmt;
use std::ops::Range;

use serde::{Deserialize, Serialize};

// ============================================================================
// File Identifier
// ============================================================================

/// Opaque numeric identifier for a file in the search index.
///
/// Using a newtype (rather than bare u32) prevents accidental misuse of IDs
/// as raw integers. Implements ordering so indices can use FileId as a map key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileId(pub u32);

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ============================================================================
// Search Field
// ============================================================================

/// AST-aware field classification for search results.
///
/// Determines which structural region of a source file a match appears in.
/// Used to weight search results and filter queries to specific code regions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SearchField {
    /// Type definitions: structs, enums, interfaces, type aliases
    TypeDefinition,
    /// Function and method signatures (declaration lines only, not body)
    FunctionSignature,
    /// Symbol names: variable names, identifiers, labels
    SymbolName,
    /// Import and export declarations
    ImportExport,
    /// Function and method bodies (implementation, excluding signature)
    FunctionBody,
    /// Comments (line and block)
    Comment,
    /// String literals
    StringLiteral,
    /// Unclassified content not matching any of the above
    Other,
}

impl SearchField {
    /// Returns the snake_case name of this field variant.
    pub fn name(self) -> &'static str {
        match self {
            Self::TypeDefinition => "type_definition",
            Self::FunctionSignature => "function_signature",
            Self::SymbolName => "symbol_name",
            Self::ImportExport => "import_export",
            Self::FunctionBody => "function_body",
            Self::Comment => "comment",
            Self::StringLiteral => "string_literal",
            Self::Other => "other",
        }
    }
}

// ============================================================================
// Temporal Flags
// ============================================================================

/// Time-based filter flags for scoping search results.
///
/// All fields are optional — absent means no temporal constraint.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TemporalFlags {
    /// Restrict results to files modified within the given number of days.
    pub modified_within_days: Option<u32>,
}

// ============================================================================
// Search Query
// ============================================================================

/// Structured search query with optional filters.
///
/// Constructed via [`SearchQuery::new`] and then configured by setting fields.
/// This type is the primary input to [`SearchLayer::search`].
#[derive(Debug, Clone)]
pub struct SearchQuery {
    /// The text to search for
    pub text: String,
    /// Optional language filter (restrict to files of this language)
    pub lang: Option<rskim_core::Language>,
    /// Optional AST pattern string (layer-defined syntax)
    pub ast_pattern: Option<String>,
    /// Optional time-based filter
    pub temporal_flags: Option<TemporalFlags>,
    /// Maximum number of results to return
    pub limit: Option<usize>,
    /// Number of results to skip (for pagination)
    pub offset: Option<usize>,
}

impl SearchQuery {
    /// Create a new query with the given search text and no filters.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            lang: None,
            ast_pattern: None,
            temporal_flags: None,
            limit: None,
            offset: None,
        }
    }
}

// ============================================================================
// Search Result
// ============================================================================

/// A single match returned by a [`SearchLayer`].
///
/// NOTE: Does NOT derive `PartialEq` because `score: f64` cannot implement it
/// reliably (NaN != NaN). Use exact field comparisons in tests.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    /// The file containing this match
    pub file_id: FileId,
    /// Relevance score (higher is more relevant); layer-specific scale
    pub score: f64,
    /// Source lines spanned by this match (0-indexed, exclusive end)
    pub line_range: Range<usize>,
    /// Byte-position ranges within the source where query terms appear
    pub match_positions: Vec<Range<usize>>,
    /// AST field classification of the matched region
    pub field: SearchField,
    /// Optional short excerpt surrounding the match for display
    pub snippet: Option<String>,
}

// ============================================================================
// Index Statistics
// ============================================================================

/// Summary statistics for a search index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStats {
    /// Number of files currently indexed
    pub file_count: u32,
    /// Total number of n-grams stored across all files
    pub total_ngrams: u64,
    /// Estimated size of the index in bytes
    pub index_size_bytes: u64,
    /// Unix epoch timestamp of the most recent index update, if any
    pub last_updated: Option<u64>,
}

// ============================================================================
// Traits
// ============================================================================

/// A search layer that can answer queries against an index.
///
/// Implementations are expected to be thread-safe (`Send + Sync`) so they can
/// be shared across worker threads in parallel search pipelines.
pub trait SearchLayer: Send + Sync {
    /// Execute a search query and return ranked results.
    ///
    /// # Errors
    /// Returns [`SearchError`] if the query is invalid or the index is corrupted.
    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>>;

    /// Human-readable name identifying this layer (e.g., `"ngram"`, `"ast"`).
    fn name(&self) -> &str;
}

/// Builder for constructing a [`SearchLayer`] from raw file content.
///
/// Separates the mutating build phase (add files) from the immutable query
/// phase (`SearchLayer`). The `where Self: Sized` bound on `build` ensures
/// the method is callable on concrete types but not on `dyn LayerBuilder`.
pub trait LayerBuilder: Send {
    /// Index the content of a file identified by `id`.
    ///
    /// # Errors
    /// Returns [`SearchError`] if the content cannot be parsed or indexed.
    fn add_file(
        &mut self,
        id: FileId,
        content: &str,
        lang: rskim_core::Language,
    ) -> Result<()>;

    /// Finalise the builder and produce a queryable [`SearchLayer`].
    ///
    /// # Errors
    /// Returns [`SearchError`] if the index cannot be constructed.
    fn build(self) -> Result<Box<dyn SearchLayer>>
    where
        Self: Sized;
}

/// Classifier that maps a tree-sitter AST node to a [`SearchField`].
///
/// Implementations should be thread-safe so they can be shared across indexing
/// workers.
pub trait FieldClassifier: Send + Sync {
    /// Classify the given `node` within its `source` file.
    fn classify(&self, node: &tree_sitter::Node<'_>, source: &str) -> SearchField;
}

// ============================================================================
// Error Types
// ============================================================================

/// Errors that can occur during search index construction or querying.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// Propagated error from the rskim-core library
    #[error("Core error: {0}")]
    Core(#[from] rskim_core::SkimError),

    /// The index data is in an inconsistent or unreadable state
    #[error("Index corrupted: {0}")]
    IndexCorrupted(String),

    /// The search query contains invalid or unsupported syntax
    #[error("Invalid query: {0}")]
    InvalidQuery(String),

    /// A FileId was referenced that does not exist in the index
    #[error("File not found in index: {0}")]
    FileNotFound(FileId),

    /// I/O error (primarily for future persistence layers)
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type alias for all rskim-search operations.
pub type Result<T> = std::result::Result<T, SearchError>;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_file_id_equality() {
        assert_eq!(FileId(0), FileId(0));
        assert_ne!(FileId(0), FileId(1));
    }

    #[test]
    fn test_file_id_ordering() {
        assert!(FileId(0) < FileId(1));
        assert!(FileId(1) > FileId(0));
        assert!(!(FileId(0) > FileId(0)));
    }

    #[test]
    fn test_file_id_display() {
        assert_eq!(format!("{}", FileId(42)), "42");
        assert_eq!(format!("{}", FileId(0)), "0");
    }

    #[test]
    fn test_search_query_new() {
        let q = SearchQuery::new("test");
        assert_eq!(q.text, "test");
        assert!(q.lang.is_none());
        assert!(q.ast_pattern.is_none());
        assert!(q.temporal_flags.is_none());
        assert!(q.limit.is_none());
        assert!(q.offset.is_none());
    }

    #[test]
    fn test_search_result_serialization() {
        let result = SearchResult {
            file_id: FileId(1),
            score: 0.95,
            line_range: 10..20,
            match_positions: vec![5..10],
            field: SearchField::FunctionSignature,
            snippet: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        // snippet serializes as null when None
        assert!(json.contains("\"snippet\":null"));
        assert!(json.contains("\"score\":0.95"));
    }

    #[test]
    fn test_search_error_from_core() {
        let core_err = rskim_core::SkimError::ParseError("x".into());
        let search_err = SearchError::from(core_err);
        let display = format!("{search_err}");
        assert!(display.contains("x"), "Display should propagate core message, got: {display}");
    }

    #[test]
    fn test_temporal_flags_default() {
        let flags = TemporalFlags::default();
        assert!(flags.modified_within_days.is_none());
    }

    #[test]
    fn test_search_field_name() {
        assert_eq!(SearchField::TypeDefinition.name(), "type_definition");
        assert_eq!(SearchField::FunctionSignature.name(), "function_signature");
        assert_eq!(SearchField::SymbolName.name(), "symbol_name");
        assert_eq!(SearchField::ImportExport.name(), "import_export");
        assert_eq!(SearchField::FunctionBody.name(), "function_body");
        assert_eq!(SearchField::Comment.name(), "comment");
        assert_eq!(SearchField::StringLiteral.name(), "string_literal");
        assert_eq!(SearchField::Other.name(), "other");
    }

    #[test]
    fn test_search_field_serialization() {
        assert_eq!(
            serde_json::to_string(&SearchField::TypeDefinition).unwrap(),
            "\"TypeDefinition\""
        );
        assert_eq!(
            serde_json::to_string(&SearchField::FunctionSignature).unwrap(),
            "\"FunctionSignature\""
        );
        assert_eq!(
            serde_json::to_string(&SearchField::Other).unwrap(),
            "\"Other\""
        );
        // Verify all 8 variants serialize without panicking
        for field in [
            SearchField::TypeDefinition,
            SearchField::FunctionSignature,
            SearchField::SymbolName,
            SearchField::ImportExport,
            SearchField::FunctionBody,
            SearchField::Comment,
            SearchField::StringLiteral,
            SearchField::Other,
        ] {
            let s = serde_json::to_string(&field).unwrap();
            assert!(!s.is_empty(), "Serialization should produce non-empty string for {field:?}");
        }
    }

    #[test]
    fn test_index_stats_construction() {
        let stats = IndexStats {
            file_count: 42,
            total_ngrams: 100_000,
            index_size_bytes: 512 * 1024,
            last_updated: Some(1_700_000_000),
        };
        assert_eq!(stats.file_count, 42);
        assert_eq!(stats.total_ngrams, 100_000);
        assert_eq!(stats.index_size_bytes, 524_288);
        assert_eq!(stats.last_updated, Some(1_700_000_000));
    }
}
