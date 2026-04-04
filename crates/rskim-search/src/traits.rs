//! Core traits for the 3-layer search architecture.
//!
//! These traits define the contracts that layer implementations must satisfy.
//! Layers are built via [`LayerBuilder`] and become immutable [`SearchLayer`]s
//! after construction. [`FieldClassifier`] is used during indexing to map
//! AST nodes to semantic [`SearchField`] values.

use std::path::Path;

use rskim_core::Language;

use crate::{FileId, Result, SearchField, SearchQuery};

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

/// Builder for constructing a [`SearchLayer`].
///
/// Accepts files one at a time, then produces an immutable layer via [`build`].
/// Consumed by `build` — single-use pattern.
pub trait LayerBuilder: Send {
    /// Add a file's content to the index being built.
    fn add_file(&mut self, path: &Path, content: &str, language: Language) -> Result<()>;

    /// Consume this builder and produce an immutable [`SearchLayer`].
    ///
    /// Uses `Box<Self>` for object safety with `Box<dyn LayerBuilder>`.
    fn build(self: Box<Self>) -> Result<Box<dyn SearchLayer>>;
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
