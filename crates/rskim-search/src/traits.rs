//! Core traits for the 3-layer search architecture.
//!
//! These traits define the contracts that layer implementations must satisfy.
//! Layers are built via [`LayerBuilder`] and become immutable [`SearchLayer`]s
//! after construction. [`FieldClassifier`] is used during indexing to map
//! AST nodes to semantic [`SearchField`] values.
//!
//! [`SearchIndex`] extends [`SearchLayer`] with metadata access (file table,
//! stats, staleness data). All persistent layer implementations implement
//! `SearchIndex`.

use std::path::{Path, PathBuf};

use rskim_core::Language;

use crate::{FileId, FileTable, IndexStats, Result, SearchField, SearchQuery};

/// Immutable search index that scores files against a query.
///
/// Implementations are built via [`LayerBuilder`] and are immutable after construction.
/// Callers resolve [`FileId`] values to paths via [`FileTable`].
pub trait SearchLayer: Send + Sync {
    /// Score files against the given query.
    ///
    /// Returns a list of `(FileId, score)` pairs, ordered by descending score.
    /// Higher scores indicate stronger matches. Scores are not normalized across layers.
    fn search(&self, query: &SearchQuery) -> Result<Vec<(FileId, f32)>>;
}

/// Extended trait for persistent search index layers.
///
/// All three layers (lexical, AST, temporal) implement this in their respective
/// waves. Provides access to shared metadata that the CLI and compound engine need.
///
/// `SearchIndex: SearchLayer` means all `SearchLayer` functionality is available.
/// Trait upcasting (`Box<dyn SearchIndex>` usable where `Box<dyn SearchLayer>` is
/// needed) is stable since Rust 1.76.
pub trait SearchIndex: SearchLayer {
    /// Access the file table for `FileId` → path resolution.
    fn file_table(&self) -> &FileTable;

    /// Get index statistics (file count, ngram count, size, freshness).
    fn stats(&self) -> IndexStats;

    /// Per-file metadata for staleness checking.
    ///
    /// Returns stored mtimes as `(path, unix_timestamp)` pairs. The CLI compares
    /// these against the filesystem to detect stale files. Stored as `u64` unix
    /// timestamps (not `SystemTime`) for cross-platform serialization.
    fn stored_file_mtimes(&self) -> &[(PathBuf, u64)];
}

/// Builder for constructing a [`SearchIndex`].
///
/// Accepts files one at a time, then produces an immutable index via [`build`].
/// Consumed by `build` — single-use pattern.
pub trait LayerBuilder: Send {
    /// Add a file's content to the index being built.
    fn add_file(&mut self, path: &Path, content: &str, language: Language) -> Result<()>;

    /// Consume this builder and produce an immutable [`SearchIndex`].
    ///
    /// Uses `Box<Self>` for object safety with `Box<dyn LayerBuilder>`.
    fn build(self: Box<Self>) -> Result<Box<dyn SearchIndex>>;
}

/// Classifies tree-sitter AST nodes into semantic search fields.
///
/// Returns `None` for nodes that are not interesting for search indexing
/// (e.g., punctuation, whitespace). `None` means "skip this node."
pub trait FieldClassifier: Send + Sync {
    /// Classify a tree-sitter node into a search field.
    ///
    /// Returns `None` if the node is not relevant for indexing.
    fn classify_node(&self, node: &tree_sitter::Node<'_>, source: &str) -> Option<SearchField>;
}

/// Temporal filter flags re-export for the `TemporalQuery::rerank` API.
///
/// Defined here to avoid a circular import with `types::query::TemporalFlags`.
/// `TemporalQuery` lives in `traits.rs` to stay alongside `SearchLayer`, but it
/// takes `&TemporalFlags` which is defined in `types/query.rs`.
use crate::TemporalFlags;

/// Query trait for the temporal analysis layer.
///
/// Temporal queries are file-property queries (not text queries), so they
/// cannot fit into [`SearchLayer::search`] which takes a [`SearchQuery`]. The
/// trait returns owned [`PathBuf`] values because the temporal layer is
/// decoupled from the lexical [`FileTable`]: it maintains its own internal
/// path mapping so that deleted files and files-in-git-history-but-not-indexed
/// are still tracked.
///
/// Scores are in `[0, 1]` (normalized) for all methods.
pub trait TemporalQuery: Send + Sync {
    /// Return files that frequently co-change with `target`.
    ///
    /// Scores are Jaccard similarities (higher = more coupled).
    fn blast_radius(&self, target: &Path, limit: usize) -> Result<Vec<(PathBuf, f32)>>;

    /// Return files ranked by recent commit activity (hotspots).
    fn hotspots(&self, limit: usize) -> Result<Vec<(PathBuf, f32)>>;

    /// Return files ranked by _lowest_ recent activity (coldspots).
    ///
    /// Only files with at least one commit in the lookback window are
    /// considered; files never touched do NOT appear.
    fn coldspots(&self, limit: usize) -> Result<Vec<(PathBuf, f32)>>;

    /// Return files ranked by risk (fix-commit density).
    fn risky(&self, limit: usize) -> Result<Vec<(PathBuf, f32)>>;

    /// Rerank lexical results by applying temporal signals.
    ///
    /// Lexical and temporal ranks are both percentile-normalized internally
    /// before blending. The `flags` parameter selects which temporal signals
    /// to apply (`hot`, `cold`, `risky`). Blast-radius queries are a separate
    /// operation — see [`TemporalQuery::blast_radius`].
    fn rerank(
        &self,
        lexical_results: &[(PathBuf, f32)],
        flags: &TemporalFlags,
    ) -> Result<Vec<(PathBuf, f32)>>;
}
