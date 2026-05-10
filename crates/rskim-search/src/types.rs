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

// Search types derive Serialize/Deserialize because search results are serialized
// to JSON for `--json` CLI output. rskim-core types do not need serde — they are
// internal transformation types that never cross a serialization boundary.
use serde::{Deserialize, Serialize};

// ============================================================================
// File Identifier
// ============================================================================

/// Transparent numeric wrapper for a file in the search index, providing type
/// safety to prevent accidental misuse of IDs as raw integers.
///
/// The inner field is `pub` by design: index builders need to construct
/// `FileId` values directly for posting-list efficiency. Implements ordering
/// so indices can use `FileId` as a map key.
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
#[serde(rename_all = "snake_case")]
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
    ///
    /// # Rationale for exhaustive match
    ///
    /// This duplicates the strings that `#[serde(rename_all = "snake_case")]`
    /// would produce, but the duplication is intentional:
    ///
    /// - **Compile-time enforcement**: adding a variant without updating this
    ///   match is a compile error, whereas forgetting to test a new serde
    ///   serialization would be silent.
    /// - **Zero allocation**: returns `&'static str` without going through
    ///   `serde_json`. Useful in hot paths (e.g. BM25F field weighting loops).
    ///
    /// The test `test_search_field_serde_agrees_with_name` verifies that both
    /// sources of truth stay in sync.
    #[must_use]
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
    #[must_use]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    fn add_file(&mut self, id: FileId, content: &str, lang: rskim_core::Language) -> Result<()>;

    /// Finalise the builder and produce a queryable [`SearchLayer`].
    ///
    /// # Errors
    /// Returns [`SearchError`] if the index cannot be constructed.
    fn build(self) -> Result<Box<dyn SearchLayer>>
    where
        Self: Sized;
}

/// Language-neutral representation of an AST node for field classification.
///
/// Captures exactly what [`FieldClassifier`] needs from a parsed node so that
/// `rskim-search` does not expose tree-sitter as part of its public API. Callers
/// (in `rskim-core` or in tree-sitter-specific indexing code) convert their
/// concrete node type to `NodeInfo` before calling [`FieldClassifier::classify`].
///
/// This keeps the Strategy Pattern in `Language::transform_source()` intact:
/// non-tree-sitter languages (JSON, YAML, TOML) can implement `FieldClassifier`
/// without depending on the tree-sitter crate.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    /// The grammar rule name for this node (e.g. `"function_definition"`).
    pub kind: &'static str,
    /// Byte range of this node within the source file.
    pub byte_range: Range<usize>,
    /// Number of named children this node has.
    pub named_child_count: usize,
}

/// Classifier that maps an AST node to a [`SearchField`].
///
/// Accepts [`NodeInfo`] rather than a concrete tree-sitter node so that
/// non-tree-sitter languages (JSON, YAML, TOML) can implement this trait
/// without depending on the tree-sitter crate.
///
/// Implementations should be thread-safe so they can be shared across indexing
/// workers.
pub trait FieldClassifier: Send + Sync {
    /// Classify the given `node` within its `source` file.
    fn classify(&self, node: &NodeInfo, source: &str) -> SearchField;
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
            snippet: Some("fn foo()".to_string()),
        };
        let json = serde_json::to_string(&result).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["file_id"], serde_json::json!(1));
        assert!((v["score"].as_f64().unwrap() - 0.95).abs() < f64::EPSILON);
        // Range<usize> serializes as {start: N, end: N}
        assert_eq!(v["line_range"]["start"], serde_json::json!(10));
        assert_eq!(v["line_range"]["end"], serde_json::json!(20));
        assert_eq!(v["match_positions"][0]["start"], serde_json::json!(5));
        assert_eq!(v["match_positions"][0]["end"], serde_json::json!(10));
        assert_eq!(v["field"], serde_json::json!("function_signature"));
        assert_eq!(v["snippet"], serde_json::json!("fn foo()"));
    }

    #[test]
    fn test_search_error_from_core() {
        let core_err = rskim_core::SkimError::ParseError("x".into());
        let search_err = SearchError::from(core_err);
        let display = format!("{search_err}");
        assert!(
            display.contains("x"),
            "Display should propagate core message, got: {display}"
        );
    }

    #[test]
    fn test_search_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let search_err = SearchError::from(io_err);
        let display = format!("{search_err}");
        assert!(
            display.starts_with("IO error:"),
            "Display should start with 'IO error:', got: {display}"
        );
        assert!(
            display.contains("file missing"),
            "Display should contain the IO message, got: {display}"
        );
    }

    #[test]
    fn test_search_error_display_variants() {
        let corrupted = SearchError::IndexCorrupted("bad checksum".to_string());
        let display = format!("{corrupted}");
        assert_eq!(display, "Index corrupted: bad checksum");

        let invalid = SearchError::InvalidQuery("empty query".to_string());
        let display = format!("{invalid}");
        assert_eq!(display, "Invalid query: empty query");

        let not_found = SearchError::FileNotFound(FileId(42));
        let display = format!("{not_found}");
        assert_eq!(display, "File not found in index: 42");
    }

    #[test]
    fn test_search_query_with_filters() {
        let q = SearchQuery {
            text: "find_me".to_string(),
            lang: Some(rskim_core::Language::Rust),
            ast_pattern: Some("fn_def".to_string()),
            temporal_flags: Some(TemporalFlags {
                modified_within_days: Some(7),
            }),
            limit: Some(10),
            offset: Some(5),
        };
        assert_eq!(q.text, "find_me");
        assert_eq!(q.lang, Some(rskim_core::Language::Rust));
        assert_eq!(q.ast_pattern.as_deref(), Some("fn_def"));
        let tf = q.temporal_flags.unwrap();
        assert_eq!(tf.modified_within_days, Some(7));
        assert_eq!(q.limit, Some(10));
        assert_eq!(q.offset, Some(5));
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
        // serde uses snake_case (rename_all) to align with the name() method output
        assert_eq!(
            serde_json::to_string(&SearchField::TypeDefinition).unwrap(),
            "\"type_definition\""
        );
        assert_eq!(
            serde_json::to_string(&SearchField::FunctionSignature).unwrap(),
            "\"function_signature\""
        );
        assert_eq!(
            serde_json::to_string(&SearchField::Other).unwrap(),
            "\"other\""
        );
    }

    /// Verifies that deserialization from snake_case strings produces the correct
    /// variant — guards against regressions when adding new variants.
    #[test]
    fn test_search_field_deserialization() {
        let cases: &[(&str, SearchField)] = &[
            ("\"type_definition\"", SearchField::TypeDefinition),
            ("\"function_signature\"", SearchField::FunctionSignature),
            ("\"symbol_name\"", SearchField::SymbolName),
            ("\"import_export\"", SearchField::ImportExport),
            ("\"function_body\"", SearchField::FunctionBody),
            ("\"comment\"", SearchField::Comment),
            ("\"string_literal\"", SearchField::StringLiteral),
            ("\"other\"", SearchField::Other),
        ];
        for (json, expected) in cases {
            let got: SearchField = serde_json::from_str(json).unwrap();
            assert_eq!(got, *expected, "failed for input {json}");
        }
    }

    /// Verifies that `name()` and serde both produce the same string for every
    /// variant, so the two sources of truth cannot drift apart silently.
    #[test]
    fn test_search_field_serde_agrees_with_name() {
        let variants = [
            SearchField::TypeDefinition,
            SearchField::FunctionSignature,
            SearchField::SymbolName,
            SearchField::ImportExport,
            SearchField::FunctionBody,
            SearchField::Comment,
            SearchField::StringLiteral,
            SearchField::Other,
        ];
        for v in variants {
            let serde_str = serde_json::to_string(&v).unwrap();
            // serde wraps in quotes; name() does not
            let serde_inner = serde_str.trim_matches('"');
            assert_eq!(
                serde_inner,
                v.name(),
                "serde and name() disagree for {v:?}"
            );
        }
    }

    /// Roundtrip test for SearchResult: serialize to JSON then deserialize back
    /// into SearchResult, verifying the Deserialize impl matches the Serialize
    /// impl field-by-field.
    #[test]
    fn test_search_result_roundtrip() {
        let original = SearchResult {
            file_id: FileId(7),
            score: 0.42,
            line_range: 3..15,
            match_positions: vec![0..4, 8..12],
            field: SearchField::SymbolName,
            snippet: Some("let foo = bar".to_string()),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: SearchResult = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.file_id, original.file_id);
        assert!((restored.score - original.score).abs() < f64::EPSILON);
        assert_eq!(restored.line_range, original.line_range);
        assert_eq!(restored.match_positions, original.match_positions);
        assert_eq!(restored.field, original.field);
        assert_eq!(restored.snippet, original.snippet);
    }

    /// Roundtrip test for SearchResult with a null snippet.
    #[test]
    fn test_search_result_roundtrip_null_snippet() {
        let original = SearchResult {
            file_id: FileId(0),
            score: 1.0,
            line_range: 0..1,
            match_positions: vec![],
            field: SearchField::Other,
            snippet: None,
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: SearchResult = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.file_id, original.file_id);
        assert!((restored.score - original.score).abs() < f64::EPSILON);
        assert_eq!(restored.line_range, original.line_range);
        assert_eq!(restored.match_positions, original.match_positions);
        assert_eq!(restored.field, original.field);
        assert_eq!(restored.snippet, None);
    }

    /// Basic serialization test for IndexStats — ensures all fields are present
    /// and correctly named in the JSON output.
    #[test]
    fn test_index_stats_serialization() {
        let stats = IndexStats {
            file_count: 42,
            total_ngrams: 1_000_000,
            index_size_bytes: 512 * 1024,
            last_updated: Some(1_700_000_000),
        };
        let json = serde_json::to_string(&stats).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["file_count"], serde_json::json!(42));
        assert_eq!(v["total_ngrams"], serde_json::json!(1_000_000u64));
        assert_eq!(v["index_size_bytes"], serde_json::json!(512u64 * 1024));
        assert_eq!(v["last_updated"], serde_json::json!(1_700_000_000u64));
    }

    /// IndexStats with no last_updated should serialize last_updated as null.
    #[test]
    fn test_index_stats_serialization_no_last_updated() {
        let stats = IndexStats {
            file_count: 0,
            total_ngrams: 0,
            index_size_bytes: 0,
            last_updated: None,
        };
        let json = serde_json::to_string(&stats).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["last_updated"], serde_json::Value::Null);
        assert_eq!(v["file_count"], serde_json::json!(0));
    }

    /// Roundtrip test for IndexStats: serialize to JSON then deserialize back,
    /// verifying the Deserialize impl matches the Serialize impl field-by-field.
    /// IndexStats will be persisted/loaded from index files, so roundtrip
    /// correctness matters independently of serialization correctness.
    #[test]
    fn test_index_stats_roundtrip() {
        let original = IndexStats {
            file_count: 42,
            total_ngrams: 1_000_000,
            index_size_bytes: 512 * 1024,
            last_updated: Some(1_700_000_000),
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: IndexStats = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.file_count, original.file_count);
        assert_eq!(restored.total_ngrams, original.total_ngrams);
        assert_eq!(restored.index_size_bytes, original.index_size_bytes);
        assert_eq!(restored.last_updated, original.last_updated);
    }

    /// Roundtrip test for IndexStats with no last_updated.
    #[test]
    fn test_index_stats_roundtrip_no_last_updated() {
        let original = IndexStats {
            file_count: 0,
            total_ngrams: 0,
            index_size_bytes: 0,
            last_updated: None,
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: IndexStats = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.file_count, original.file_count);
        assert_eq!(restored.total_ngrams, original.total_ngrams);
        assert_eq!(restored.index_size_bytes, original.index_size_bytes);
        assert_eq!(restored.last_updated, None);
    }

    /// Verifies that a concrete FieldClassifier implementation can be written
    /// and used with NodeInfo. Guards the trait's API contract: classifiers must
    /// be constructable without tree-sitter, using only NodeInfo fields.
    #[test]
    fn test_field_classifier_concrete_impl() {
        struct KindClassifier;

        impl FieldClassifier for KindClassifier {
            fn classify(&self, node: &NodeInfo, _source: &str) -> SearchField {
                match node.kind {
                    "function_definition" | "function_item" => SearchField::FunctionSignature,
                    "struct_item" | "class_definition" | "type_alias" => {
                        SearchField::TypeDefinition
                    }
                    "import_declaration" | "use_declaration" => SearchField::ImportExport,
                    "line_comment" | "block_comment" => SearchField::Comment,
                    "string_literal" => SearchField::StringLiteral,
                    _ => SearchField::Other,
                }
            }
        }

        let classifier = KindClassifier;

        let fn_node = NodeInfo {
            kind: "function_item",
            byte_range: 0..50,
            named_child_count: 3,
        };
        assert_eq!(
            classifier.classify(&fn_node, "fn foo() {}"),
            SearchField::FunctionSignature
        );

        let struct_node = NodeInfo {
            kind: "struct_item",
            byte_range: 0..30,
            named_child_count: 2,
        };
        assert_eq!(
            classifier.classify(&struct_node, "struct Foo { x: u32 }"),
            SearchField::TypeDefinition
        );

        let unknown_node = NodeInfo {
            kind: "attribute_item",
            byte_range: 0..10,
            named_child_count: 1,
        };
        assert_eq!(
            classifier.classify(&unknown_node, "#[derive(Debug)]"),
            SearchField::Other
        );
    }
}
