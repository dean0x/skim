//! Core type definitions for rskim-search
//!
//! ARCHITECTURE: This module defines ALL types used across the search layer.
//! Design principle: I/O-free types with explicit error handling.
//! All types follow the rskim-core pattern: thiserror for errors, explicit derives.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ============================================================================
// File Identity
// ============================================================================

/// Opaque identifier for a file in the search index.
///
/// All search layers reference files by `FileId`, resolved to paths via [`FileTable`].
/// This indirection allows layers to store compact integer keys instead of heap-allocated paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileId(u64);

impl FileId {
    /// Create a new `FileId` from a raw integer.
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    /// Return the raw integer value.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

// ============================================================================
// Search Fields
// ============================================================================

/// Semantic field within a source file used for field-boosted search scoring.
///
/// Each variant corresponds to a distinct syntactic region of a file.
/// Field weights are defined by [`SearchField::default_boost`] and are applied
/// during BM25F scoring in the lexical search layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SearchField {
    /// Top-level type, interface, struct, class, or enum definition.
    TypeDefinition,
    /// Function or method signature (name + parameters + return type).
    FunctionSignature,
    /// Bare symbol name (identifier without surrounding context).
    SymbolName,
    /// Import or export statement.
    ImportExport,
    /// Function or method body (implementation detail).
    FunctionBody,
    /// Doc comment or regular comment.
    Comment,
    /// String literal value.
    StringLiteral,
}

impl SearchField {
    /// Return the default BM25F boost factor for this field.
    ///
    /// Higher values cause matches in this field to score more strongly.
    pub fn default_boost(self) -> f32 {
        match self {
            Self::TypeDefinition => 5.0,
            Self::FunctionSignature => 4.0,
            Self::SymbolName => 3.5,
            Self::ImportExport => 3.0,
            Self::FunctionBody => 1.0,
            Self::Comment => 0.8,
            Self::StringLiteral => 0.5,
        }
    }
}

// ============================================================================
// Span Types
// ============================================================================

/// Byte-offset span within a source file.
///
/// Both `start` and `end` are byte offsets into the original UTF-8 source.
/// The span is half-open: `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchSpan {
    /// Start byte offset (inclusive).
    pub start: u32,
    /// End byte offset (exclusive).
    pub end: u32,
}

impl MatchSpan {
    /// Create a new span from start and end byte offsets.
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Return the length of the span in bytes.
    pub fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// Return `true` if the span has zero length.
    pub fn is_empty(self) -> bool {
        self.len() == 0
    }
}

/// 1-indexed, inclusive line range within a source file.
///
/// Both `start` and `end` are 1-based line numbers.
/// A single-line range has `start == end`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    /// First line (1-indexed, inclusive).
    pub start: u32,
    /// Last line (1-indexed, inclusive).
    pub end: u32,
}

impl LineRange {
    /// Create a new line range from start and end line numbers.
    pub fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }
}

// ============================================================================
// Temporal Flags
// ============================================================================

/// Temporal filter flags for query-time filtering by git activity signals.
///
/// All flags default to `false` (disabled). Any combination is valid.
#[derive(Debug, Clone, Default)]
pub struct TemporalFlags {
    /// Include only files with high blast radius (many dependents).
    pub blast_radius: bool,
    /// Include only files with recent commit activity ("hot" files).
    pub hot: bool,
    /// Include only files with no recent changes ("cold" files).
    pub cold: bool,
    /// Include only files with high churn or complexity.
    pub risky: bool,
}

// ============================================================================
// Search Query
// ============================================================================

/// Query to execute against the search index.
///
/// Constructed via [`SearchQuery::new`] or the convenience [`SearchQuery::text`],
/// then customised with builder methods.
///
/// # Examples
///
/// ```
/// use rskim_search::SearchQuery;
///
/// let q = SearchQuery::text("parse_file").with_limit(20);
/// ```
#[derive(Debug, Clone)]
pub struct SearchQuery {
    /// Free-text query string for lexical matching.
    pub text_query: Option<String>,
    /// AST pattern string for structural matching.
    pub ast_pattern: Option<String>,
    /// Temporal filter flags.
    pub temporal_flags: TemporalFlags,
    /// Maximum number of results to return.
    pub limit: usize,
    /// Number of results to skip (pagination offset).
    pub offset: usize,
}

impl SearchQuery {
    /// Create a query with default settings (no text, limit 50, offset 0).
    pub fn new() -> Self {
        Self {
            text_query: None,
            ast_pattern: None,
            temporal_flags: TemporalFlags::default(),
            limit: 50,
            offset: 0,
        }
    }

    /// Convenience constructor: create a query with the given text.
    ///
    /// Equivalent to `SearchQuery::new().with_text(query)`.
    pub fn text(query: &str) -> Self {
        Self::new().with_text(query)
    }

    /// Set the free-text query string.
    pub fn with_text(mut self, text: &str) -> Self {
        self.text_query = Some(text.to_string());
        self
    }

    /// Set the maximum number of results.
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Set the pagination offset.
    pub fn with_offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    /// Set the AST pattern for structural matching.
    pub fn with_ast_pattern(mut self, pattern: &str) -> Self {
        self.ast_pattern = Some(pattern.to_string());
        self
    }

    /// Set the temporal filter flags.
    pub fn with_temporal_flags(mut self, flags: TemporalFlags) -> Self {
        self.temporal_flags = flags;
        self
    }
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Search Results
// ============================================================================

/// A single result from a search query.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Path to the file containing the match.
    pub file_path: PathBuf,
    /// Line range of the matched region (1-indexed, inclusive).
    pub line_range: LineRange,
    /// Relevance score (higher is better; not normalized across layers).
    pub score: f32,
    /// The semantic field in which the match was found.
    pub matched_field: SearchField,
    /// A short excerpt of the matching source region.
    pub snippet: String,
    /// Byte-offset positions of the matched terms within `snippet`.
    pub match_positions: Vec<MatchSpan>,
}

// ============================================================================
// Index Statistics
// ============================================================================

/// Runtime statistics for a search index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStats {
    /// Total number of files indexed.
    pub file_count: u64,
    /// Total number of n-grams stored in the index.
    pub total_ngrams: u64,
    /// On-disk size of the index in bytes.
    pub index_size_bytes: u64,
    /// Unix timestamp (seconds) of the last index update.
    pub last_updated: u64,
    /// Serialization format version for forward/backward compatibility.
    pub format_version: u32,
}

// ============================================================================
// File Table
// ============================================================================

/// Bidirectional mapping between file paths and compact [`FileId`] values.
///
/// All search layers reference files by `FileId`. Callers resolve IDs back to
/// paths via [`FileTable::lookup`]. The table is I/O-free — it does not touch
/// the filesystem.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTable {
    /// Ordered list of registered paths; index into this vec is the raw FileId.
    paths: Vec<PathBuf>,
    /// Reverse map: normalized path -> FileId.
    ids: HashMap<PathBuf, FileId>,
}

impl FileTable {
    /// Create an empty `FileTable`.
    pub fn new() -> Self {
        Self {
            paths: Vec::new(),
            ids: HashMap::new(),
        }
    }

    /// Register `path` and return its `FileId`.
    ///
    /// Idempotent: re-registering an already-known path returns the same `FileId`.
    /// The path is normalized (leading `./` stripped, `..` components collapsed) before
    /// lookup; two paths that normalize to the same value get the same `FileId`.
    pub fn register(&mut self, path: &Path) -> FileId {
        let normalized = Self::normalize(path);
        if let Some(&id) = self.ids.get(&normalized) {
            return id;
        }
        let id = FileId::new(self.paths.len() as u64);
        self.paths.push(normalized.clone());
        self.ids.insert(normalized, id);
        id
    }

    /// Resolve a `FileId` back to a path, if it was registered.
    ///
    /// Returns `None` for IDs that were never registered with this table.
    pub fn lookup(&self, id: FileId) -> Option<&Path> {
        self.paths.get(id.as_u64() as usize).map(PathBuf::as_path)
    }

    /// Return the number of registered files.
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Return `true` if no files have been registered.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Normalize `path` for consistent lookup.
    ///
    /// Rules (I/O-free — no `fs::canonicalize`):
    /// - Leading `./` is stripped (CurDir component removed).
    /// - `..` components are collapsed by removing the preceding component.
    /// - Absolute paths are kept as-is.
    fn normalize(path: &Path) -> PathBuf {
        let mut components: Vec<Component<'_>> = Vec::new();
        for component in path.components() {
            match component {
                Component::CurDir => {
                    // Strip `.` components (handles leading `./`)
                }
                Component::ParentDir => {
                    // Pop the last normal component to handle `..`
                    if matches!(components.last(), Some(Component::Normal(_))) {
                        components.pop();
                    } else {
                        components.push(component);
                    }
                }
                other => {
                    components.push(other);
                }
            }
        }
        components.iter().collect()
    }
}

impl Default for FileTable {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Error Types
// ============================================================================

/// Errors that can occur during search and indexing operations.
#[non_exhaustive]
#[derive(Debug, Error)]
pub enum SearchError {
    /// An I/O error occurred while reading or writing index files.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// The index is corrupt, missing, or in an incompatible format.
    #[error("Index error: {0}")]
    IndexError(String),

    /// The query is malformed or contains unsupported constructs.
    #[error("Invalid query: {0}")]
    InvalidQuery(String),

    /// An error propagated from `rskim-core`.
    #[error("Core error: {0}")]
    CoreError(#[from] rskim_core::SkimError),

    /// A serialization or deserialization error occurred.
    #[error("Serialization error: {0}")]
    SerializationError(String),
}

/// Result type alias for search operations.
pub type Result<T> = std::result::Result<T, SearchError>;

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_id_accessors() {
        let id = FileId::new(42);
        assert_eq!(id.as_u64(), 42);
        // Copy semantics: both bindings are independent copies
        let a = id;
        let b = id;
        assert_eq!(a, b);
        assert_eq!(a.as_u64(), 42);
    }

    #[test]
    fn test_match_span_len_and_empty() {
        let span = MatchSpan::new(10, 20);
        assert_eq!(span.len(), 10);
        assert!(!span.is_empty());

        let zero = MatchSpan::new(5, 5);
        assert_eq!(zero.len(), 0);
        assert!(zero.is_empty());

        // Saturating subtraction: start > end should not panic
        let inverted = MatchSpan::new(20, 10);
        assert_eq!(inverted.len(), 0);
        assert!(inverted.is_empty());
    }

    #[test]
    fn test_line_range_construction() {
        let r = LineRange::new(1, 5);
        assert_eq!(r.start, 1);
        assert_eq!(r.end, 5);
        assert_eq!(r, LineRange { start: 1, end: 5 });
    }

    #[test]
    fn test_search_query_new_defaults() {
        let q = SearchQuery::new();
        assert!(q.text_query.is_none());
        assert_eq!(q.limit, 50);
        assert_eq!(q.offset, 0);
    }

    #[test]
    fn test_search_query_text_convenience() {
        let q = SearchQuery::text("foo");
        assert_eq!(q.text_query, Some("foo".to_string()));
    }

    #[test]
    fn test_search_query_builder_chain() {
        let flags = TemporalFlags {
            hot: true,
            ..Default::default()
        };
        let q = SearchQuery::new()
            .with_text("bar")
            .with_limit(10)
            .with_offset(5)
            .with_ast_pattern("fn _()")
            .with_temporal_flags(flags);

        assert_eq!(q.text_query, Some("bar".to_string()));
        assert_eq!(q.limit, 10);
        assert_eq!(q.offset, 5);
        assert_eq!(q.ast_pattern, Some("fn _()".to_string()));
        assert!(q.temporal_flags.hot);
        assert!(!q.temporal_flags.blast_radius);
    }

    #[test]
    fn test_search_field_boost_values() {
        assert_eq!(SearchField::TypeDefinition.default_boost(), 5.0);
        assert_eq!(SearchField::FunctionSignature.default_boost(), 4.0);
        assert_eq!(SearchField::SymbolName.default_boost(), 3.5);
        assert_eq!(SearchField::ImportExport.default_boost(), 3.0);
        assert_eq!(SearchField::FunctionBody.default_boost(), 1.0);
        assert_eq!(SearchField::Comment.default_boost(), 0.8);
        assert_eq!(SearchField::StringLiteral.default_boost(), 0.5);
    }

    #[test]
    fn test_temporal_flags_default() {
        let flags = TemporalFlags::default();
        assert!(!flags.blast_radius);
        assert!(!flags.hot);
        assert!(!flags.cold);
        assert!(!flags.risky);
    }

    #[test]
    fn test_file_table_register_and_lookup() {
        let mut table = FileTable::new();
        assert!(table.is_empty());

        let id = table.register(Path::new("src/main.rs"));
        assert_eq!(table.len(), 1);
        assert!(!table.is_empty());

        let path = table.lookup(id);
        assert_eq!(path, Some(Path::new("src/main.rs")));

        // Idempotent: re-registering returns the same FileId
        let id2 = table.register(Path::new("src/main.rs"));
        assert_eq!(id, id2);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn test_file_table_normalizes_paths() {
        let mut table = FileTable::new();

        let id1 = table.register(Path::new("./src/main.rs"));
        let id2 = table.register(Path::new("src/main.rs"));

        // Both paths normalize to "src/main.rs" — same FileId, single entry
        assert_eq!(id1, id2);
        assert_eq!(table.len(), 1);
    }
}
