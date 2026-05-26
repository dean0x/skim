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
use std::path::{Path, PathBuf};

// Search types derive Serialize/Deserialize because search results are serialized
// to JSON for `--json` CLI output. rskim-core types do not need serde — they are
// internal transformation types that never cross a serialization boundary.
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
///
/// # Binary format
///
/// The `#[repr(u8)]` attribute with explicit discriminants 0–7 allows the index
/// format to store field IDs as a single byte and recover the variant via
/// [`SearchField::from_discriminant`]. The mapping is part of the stable on-disk
/// format — **do not change discriminant values without a format migration**.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchField {
    /// Type definitions: structs, enums, interfaces, type aliases
    TypeDefinition = 0,
    /// Function and method signatures (declaration lines only, not body)
    FunctionSignature = 1,
    /// Symbol names: variable names, identifiers, labels
    SymbolName = 2,
    /// Import and export declarations
    ImportExport = 3,
    /// Function and method bodies (implementation, excluding signature)
    FunctionBody = 4,
    /// Comments (line and block)
    Comment = 5,
    /// String literals
    StringLiteral = 6,
    /// Unclassified content not matching any of the above
    Other = 7,
}

impl SearchField {
    /// All eight field variants in discriminant order (0–7).
    ///
    /// Used wherever code needs to iterate over every field (e.g., BM25F scoring
    /// loops, field-length accumulation in the builder).
    pub const ALL: [Self; 8] = [
        Self::TypeDefinition,
        Self::FunctionSignature,
        Self::SymbolName,
        Self::ImportExport,
        Self::FunctionBody,
        Self::Comment,
        Self::StringLiteral,
        Self::Other,
    ];

    /// The total number of [`SearchField`] variants.
    ///
    /// Derived from `ALL.len()` so it stays in sync automatically when variants
    /// are added or removed. `FIELD_COUNT` in the lexical module is defined as
    /// `SearchField::count()` to create a single authoritative source.
    #[must_use]
    #[inline]
    pub const fn count() -> usize {
        Self::ALL.len()
    }

    /// Returns the numeric discriminant of this variant (0–7).
    ///
    /// The discriminant is stable across compilations and forms part of the
    /// on-disk index format. Changing a variant's discriminant is a **breaking
    /// format change** requiring a version bump in `FORMAT_VERSION`.
    #[must_use]
    #[inline]
    pub fn discriminant(self) -> u8 {
        self as u8
    }

    /// Recover a [`SearchField`] from its numeric discriminant.
    ///
    /// Returns `None` for any byte that does not correspond to a known variant,
    /// so corrupt index bytes produce a recoverable error rather than undefined
    /// behaviour.
    #[must_use]
    pub fn from_discriminant(d: u8) -> Option<Self> {
        match d {
            0 => Some(Self::TypeDefinition),
            1 => Some(Self::FunctionSignature),
            2 => Some(Self::SymbolName),
            3 => Some(Self::ImportExport),
            4 => Some(Self::FunctionBody),
            5 => Some(Self::Comment),
            6 => Some(Self::StringLiteral),
            7 => Some(Self::Other),
            _ => None,
        }
    }
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
// Co-change Matrix Statistics
// ============================================================================

/// Statistics returned by [`crate::cochange::CochangeMatrixBuilder::build`].
///
/// Provides observability into the build process: how many pairs were
/// accumulated, how many commits were processed, and how many were skipped
/// due to the `COUPLING_MAX_FILES` cap or unknown path resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CochangeStats {
    /// Number of distinct co-change pairs stored in the matrix.
    pub pair_count: u32,
    /// Number of distinct files referenced across all processed commits.
    pub file_count: u32,
    /// Number of commits iterated (regardless of path resolution success).
    pub commits_processed: u32,
    /// Number of commits skipped because they touched more than
    /// `COUPLING_MAX_FILES` files.
    pub commits_skipped_too_large: u32,
    /// Number of file paths silently skipped because they were absent from the
    /// path-to-[`FileId`] map supplied by the caller.
    pub unknown_paths_skipped: u32,
}

// ============================================================================
// Temporal Flags
// ============================================================================

/// Time-based filter flags for scoping search results.
///
/// All fields are optional — absent means no temporal constraint.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemporalFlags {
    /// Restrict results to files modified within the given number of days.
    pub modified_within_days: Option<u32>,
}

// ============================================================================
// Git history types (Wave 2a)
// ============================================================================

/// A single file touched in a commit, with line-change counts.
///
/// Shared between the `temporal` module (git history parsing) and any consumer
/// that needs file-level change metadata. Both `additions` and `deletions` use
/// `u64` to accommodate large generated files without overflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChangeInfo {
    /// Repo-root-relative path of the file.
    pub path: PathBuf,
    /// Number of lines added.
    pub additions: u64,
    /// Number of lines deleted.
    pub deletions: u64,
}

impl FileChangeInfo {
    /// Returns the path as a string slice, using lossy UTF-8 conversion.
    ///
    /// Git paths are always valid UTF-8 in practice, so lossy conversion is
    /// safe and consistent with the rest of the codebase. Returns a `Cow<str>`
    /// to avoid an unnecessary allocation when the path is already valid UTF-8.
    #[must_use]
    #[inline]
    pub fn path_str(&self) -> std::borrow::Cow<'_, str> {
        self.path.to_string_lossy()
    }
}

/// Metadata extracted from a single git commit.
///
/// Intentionally free of gix types — all gix values are converted at the
/// parser boundary so no gix dependency leaks into the public API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitInfo {
    /// Full 40-character hex SHA of the commit.
    pub hash: String,
    /// Unix timestamp (seconds since epoch, UTC).
    pub timestamp: i64,
    /// Author name (from git `author.name`).
    pub author: String,
    /// First line of the commit message.
    pub message: String,
    /// Files touched by this commit.
    pub changed_files: Vec<FileChangeInfo>,
}

// ============================================================================
// Risk scoring types (Issue #183)
// ============================================================================

/// Per-file hotspot and bug-fix density scores computed from git history.
///
/// Both fields are in the range `[0.0, 1.0]`.
///
/// - `hotspot`: decay-weighted commit frequency, max-normalized so the most active
///   file scores `1.0`. Higher values indicate files that change frequently and
///   recently — strong candidates for code-smell review.
/// - `fix_density`: fraction of weighted commits that are classified as bug fixes.
///   A value of `1.0` means every weighted touch was a fix commit; `0.0` means none.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FileRiskScores {
    /// Decay-weighted commit frequency, max-normalized to `[0.0, 1.0]`.
    pub hotspot: f64,
    /// Decay-weighted ratio of fix commits to total commits, in `[0.0, 1.0]`.
    pub fix_density: f64,
}

/// Per-file raw commit counts within two time windows.
///
/// Computed by [`crate::temporal::compute_file_temporal_stats`] from a slice of
/// [`CommitInfo`] values. Unlike [`FileRiskScores`], these are raw counts (not
/// decay-weighted), suitable for persistence and incremental refresh.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileTemporalStats {
    /// Number of commits touching this file within the last 30 days.
    pub changes_30d: u32,
    /// Number of commits touching this file within the last 90 days.
    pub changes_90d: u32,
    /// Total number of commits touching this file in the input slice.
    pub total_commits: u32,
    /// Number of commits classified as fix commits by [`crate::is_fix_commit`].
    pub fix_commits: u32,
}

/// Summary metadata about a parsed history result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemporalMetadata {
    /// True when the repository is a shallow clone (history may be incomplete).
    pub is_shallow: bool,
    /// Number of commits included in this result (equals `commits.len()`).
    pub commit_count: usize,
}

/// Output of [`TemporalSource::parse_history`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryResult {
    /// Commits ordered from newest to oldest.
    pub commits: Vec<CommitInfo>,
    /// Summary metadata about the history traversal.
    pub metadata: TemporalMetadata,
}

/// Trait for git history parsers.
///
/// Implementations must be `Send + Sync` so they can be used from worker
/// threads. Object safety is preserved — no associated types or generics.
pub trait TemporalSource: Send + Sync {
    /// Parse git history for the repository at `repo_path`.
    ///
    /// Returns commits ordered from newest to oldest, filtered to those whose
    /// author timestamp falls within the last `lookback_days` days.
    /// When `lookback_days` is `0`, all history is returned (no time filter).
    ///
    /// # Errors
    /// Returns [`SearchError::Git`] on any git-level failure (not a repo,
    /// unreadable objects, etc.).
    fn parse_history(&self, repo_path: &Path, lookback_days: u32) -> Result<HistoryResult>;
}

// ============================================================================
// Search Query
// ============================================================================

/// Structured search query with optional filters.
///
/// Constructed via [`SearchQuery::new`] and then configured by setting fields.
/// This type is the primary input to [`SearchLayer::search`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    /// The text to search for
    pub text: String,
    /// Optional language filter (restrict to files of this language).
    ///
    /// NOTE: `rskim_core::Language` does not implement `Serialize`/`Deserialize`
    /// so this field is skipped during serialization. Language filters are applied
    /// at query construction time and are not round-tripped through JSON.
    #[serde(skip)]
    pub lang: Option<rskim_core::Language>,
    /// Optional AST pattern string (layer-defined syntax)
    pub ast_pattern: Option<String>,
    /// Optional time-based filter
    pub temporal_flags: Option<TemporalFlags>,
    /// Maximum number of results to return
    pub limit: Option<usize>,
    /// Number of results to skip (for pagination)
    pub offset: Option<usize>,
    /// Per-query BM25F scoring configuration override.
    ///
    /// When `Some`, this configuration replaces the reader's default
    /// [`crate::lexical::BM25FConfig`] for this query only. When `None`,
    /// the reader's default is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bm25f_config: Option<crate::lexical::BM25FConfig>,
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
            bm25f_config: None,
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
    /// Source lines spanned by this match (1-indexed, exclusive end; 0..0 when not yet computed)
    pub line_range: Range<usize>,
    /// Byte-position ranges within the source where query terms appear
    pub match_positions: Vec<Range<usize>>,
    /// AST field classification of the matched region
    pub field: SearchField,
    /// Optional short excerpt surrounding the match for display
    pub snippet: Option<String>,
}

// ============================================================================
// Line-range utilities
// ============================================================================

/// Map a byte offset within `content` to a **1-indexed** line number.
///
/// Counts newlines in `content[..offset]`. The offset is clamped to
/// `content.len()` so out-of-bounds values never panic. Returns `1` for
/// offset `0` or any offset in empty content.
#[must_use]
pub fn byte_offset_to_line(content: &[u8], offset: usize) -> usize {
    let safe_offset = offset.min(content.len());
    let newlines = content[..safe_offset]
        .iter()
        .filter(|&&b| b == b'\n')
        .count();
    newlines + 1
}

/// Compute the line range spanned by a set of byte-position `match_positions`.
///
/// For each position the start byte is converted to a 1-indexed line number via
/// [`byte_offset_to_line`]. Returns `min_line..(max_line + 1)` (exclusive end,
/// 1-indexed), matching the convention used by [`SearchResult::line_range`].
///
/// Returns `0..0` when `match_positions` is empty.
#[must_use]
pub fn compute_line_range(content: &[u8], match_positions: &[Range<usize>]) -> Range<usize> {
    if match_positions.is_empty() {
        return 0..0;
    }

    let (min_line, max_line) = match_positions
        .iter()
        .map(|pos| byte_offset_to_line(content, pos.start))
        .fold((usize::MAX, 0usize), |(mn, mx), line| {
            (mn.min(line), mx.max(line))
        });

    min_line..(max_line + 1)
}

// ============================================================================
// Index Statistics
// ============================================================================

/// Summary statistics for a search index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Consuming `self` is intentional: the build phase is separate from the
    /// query phase to keep `SearchLayer` implementations immutable and
    /// thread-safe. Incremental indexing (updating an existing layer after
    /// individual file changes) is intentionally deferred to a separate
    /// `IncrementalBuilder` trait in a future Wave. For now, re-index by
    /// constructing a new builder and calling `build` again.
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
    ///
    /// **Constraint**: must be a compile-time constant (`&'static str`). Tree-sitter
    /// grammars expose node kinds as `&'static str` naturally. Non-tree-sitter
    /// parsers (JSON, YAML, TOML) should use fixed string literals (e.g.
    /// `"json_object"`) rather than dynamically allocated strings.
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
///
/// # Relationship to `classify_source`
///
/// [`crate::lexical::classify_source`] is the built-in, byte-range implementation
/// used by the BM25F indexing pipeline. `FieldClassifier` is a **future extension
/// point**: it allows downstream consumers (custom indexers, non-tree-sitter
/// language plugins) to plug in alternative classification logic without depending
/// on tree-sitter internals. The two are parallel, not competing, APIs.
pub trait FieldClassifier: Send + Sync {
    /// Classify the given `node` within its `source` file.
    fn classify(&self, node: &NodeInfo, source: &str) -> SearchField;
}

// ============================================================================
// Error Types
// ============================================================================

/// Errors that can occur during search index construction or querying.
#[non_exhaustive]
#[derive(Debug, Error)]
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

    /// Git operation error — all gix/libgit2 errors are converted to strings
    /// at the parser boundary so no git library types leak into this enum.
    #[error("Git error: {0}")]
    Git(String),

    /// The source file exceeds the maximum size that can be safely classified.
    /// The classifier allocates a per-byte array, so unbounded input would
    /// cause proportional memory growth.
    #[error("File too large to classify: {size} bytes exceeds the {limit}-byte limit")]
    FileTooLarge { size: usize, limit: usize },

    /// A build-time safety limit was exceeded (e.g. the co-change `MAX_PAIRS` cap).
    ///
    /// Distinct from [`SearchError::IndexCorrupted`], which signals corrupt
    /// data on disk. This variant indicates that the input data is valid but
    /// exceeds a pre-configured capacity bound.
    #[error("Capacity exceeded: {0}")]
    CapacityExceeded(String),

    /// SQLite database error from the temporal persistence layer.
    ///
    /// All rusqlite errors are converted to strings at the storage boundary so
    /// no rusqlite types leak into this enum.
    #[error("Database error: {0}")]
    Database(String),
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
            match_positions: vec![5..8, 12..15],
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
        assert_eq!(v["match_positions"][0]["end"], serde_json::json!(8));
        assert_eq!(v["match_positions"][1]["start"], serde_json::json!(12));
        assert_eq!(v["match_positions"][1]["end"], serde_json::json!(15));
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
            bm25f_config: None,
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

    /// Verifies that discriminant() returns values 0-7 matching the #[repr(u8)]
    /// discriminants, and that from_discriminant() is the exact inverse.
    #[test]
    fn test_search_field_discriminant_roundtrip() {
        let variants = [
            (SearchField::TypeDefinition, 0u8),
            (SearchField::FunctionSignature, 1u8),
            (SearchField::SymbolName, 2u8),
            (SearchField::ImportExport, 3u8),
            (SearchField::FunctionBody, 4u8),
            (SearchField::Comment, 5u8),
            (SearchField::StringLiteral, 6u8),
            (SearchField::Other, 7u8),
        ];
        for (variant, expected_disc) in variants {
            assert_eq!(
                variant.discriminant(),
                expected_disc,
                "discriminant mismatch for {variant:?}"
            );
            let recovered = SearchField::from_discriminant(expected_disc);
            assert_eq!(
                recovered,
                Some(variant),
                "from_discriminant({expected_disc}) did not recover {variant:?}"
            );
        }
    }

    /// Verifies that from_discriminant returns None for unknown byte values.
    #[test]
    fn test_search_field_unknown_discriminant_returns_none() {
        for bad in [8u8, 9, 100, 200, 255] {
            assert_eq!(
                SearchField::from_discriminant(bad),
                None,
                "from_discriminant({bad}) should be None"
            );
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
            assert_eq!(serde_inner, v.name(), "serde and name() disagree for {v:?}");
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

    /// Verifies that a concrete SearchLayer implementation can be written and
    /// used with SearchQuery/SearchResult. Guards the trait's API contract:
    /// layers must be constructable without I/O and must return typed results.
    #[test]
    fn test_search_layer_contract() {
        struct EmptyLayer;

        impl SearchLayer for EmptyLayer {
            fn search(&self, _query: &SearchQuery) -> Result<Vec<SearchResult>> {
                Ok(vec![])
            }

            fn name(&self) -> &str {
                "empty"
            }
        }

        let layer = EmptyLayer;
        let query = SearchQuery::new("anything");
        let results = layer.search(&query).unwrap();
        assert!(results.is_empty());
        assert_eq!(layer.name(), "empty");
    }

    /// Verifies that a concrete LayerBuilder implementation can be written and
    /// used to index files and produce a SearchLayer. Guards the trait's API
    /// contract: builders must accept add_file and build into a queryable layer.
    #[test]
    fn test_layer_builder_contract() {
        struct NoopBuilder {
            file_count: u32,
        }

        impl LayerBuilder for NoopBuilder {
            fn add_file(
                &mut self,
                _id: FileId,
                _content: &str,
                _lang: rskim_core::Language,
            ) -> Result<()> {
                self.file_count += 1;
                Ok(())
            }

            fn build(self) -> Result<Box<dyn SearchLayer>> {
                struct BuiltLayer;

                impl SearchLayer for BuiltLayer {
                    fn search(&self, _query: &SearchQuery) -> Result<Vec<SearchResult>> {
                        Ok(vec![])
                    }

                    fn name(&self) -> &str {
                        "noop"
                    }
                }

                Ok(Box::new(BuiltLayer))
            }
        }

        let mut builder = NoopBuilder { file_count: 0 };
        builder
            .add_file(FileId(0), "fn main() {}", rskim_core::Language::Rust)
            .unwrap();
        builder
            .add_file(FileId(1), "def hello(): pass", rskim_core::Language::Python)
            .unwrap();

        let layer = builder.build().unwrap();
        assert_eq!(layer.name(), "noop");
        let results = layer.search(&SearchQuery::new("hello")).unwrap();
        assert!(results.is_empty());
    }

    /// SearchField::ALL must contain all 8 variants in discriminant order.
    #[test]
    fn test_search_field_all_contains_all_variants() {
        assert_eq!(SearchField::ALL.len(), 8, "ALL must have 8 elements");
        for (i, &field) in SearchField::ALL.iter().enumerate() {
            assert_eq!(
                field.discriminant(),
                i as u8,
                "ALL[{i}] should have discriminant {i}, got {}",
                field.discriminant()
            );
        }
    }

    /// SearchField::count() must equal 8.
    #[test]
    fn test_search_field_count() {
        assert_eq!(SearchField::count(), 8, "count() must return 8");
    }

    /// SearchQuery::new() should have bm25f_config set to None.
    #[test]
    fn test_search_query_new_bm25f_config_none() {
        let q = SearchQuery::new("hello");
        assert!(
            q.bm25f_config.is_none(),
            "new() should initialise bm25f_config to None"
        );
    }

    // ========================================================================
    // byte_offset_to_line (library version, returns usize, 1-indexed)
    // ========================================================================

    #[test]
    fn test_lib_byte_offset_start_of_file() {
        assert_eq!(byte_offset_to_line(b"hello\nworld", 0), 1);
    }

    #[test]
    fn test_lib_byte_offset_second_line() {
        assert_eq!(byte_offset_to_line(b"hello\nworld", 6), 2);
    }

    #[test]
    fn test_lib_byte_offset_at_newline() {
        // newline byte itself is end of line 1
        assert_eq!(byte_offset_to_line(b"hello\nworld", 5), 1);
    }

    #[test]
    fn test_lib_byte_offset_middle_of_line() {
        assert_eq!(byte_offset_to_line(b"hello\nworld", 8), 2);
    }

    #[test]
    fn test_lib_byte_offset_empty_content() {
        assert_eq!(byte_offset_to_line(b"", 0), 1);
    }

    #[test]
    fn test_lib_byte_offset_clamped() {
        assert_eq!(byte_offset_to_line(b"hello", 999), 1);
    }

    #[test]
    fn test_lib_byte_offset_last_byte() {
        assert_eq!(byte_offset_to_line(b"a\nb\nc", 4), 3);
    }

    // ========================================================================
    // compute_line_range
    // ========================================================================

    #[test]
    fn test_line_range_empty_positions() {
        assert_eq!(compute_line_range(b"hello\nworld", &[]), 0..0);
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn test_line_range_single_position() {
        // offset 2 on "a\nb\nc" -> line 2 (byte 2 = 'b')
        assert_eq!(compute_line_range(b"a\nb\nc", &[2..3]), 2..3);
    }

    #[test]
    fn test_line_range_multi_line_span() {
        // offsets 0 (line 1) and 6 (line 4) on "a\nb\nc\nd\ne"
        assert_eq!(compute_line_range(b"a\nb\nc\nd\ne", &[0..1, 6..7]), 1..5);
    }

    #[test]
    fn test_line_range_same_line() {
        assert_eq!(compute_line_range(b"hello world", &[0..3, 6..9]), 1..2);
    }

    #[test]
    fn test_line_range_adjacent_lines() {
        // offsets 0 (line 1) and 2 (line 2) on "a\nb\nc"
        assert_eq!(compute_line_range(b"a\nb\nc", &[0..1, 2..3]), 1..3);
    }
}
