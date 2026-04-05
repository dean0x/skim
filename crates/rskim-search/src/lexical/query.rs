//! Query engine: `SearchLayer` + `SearchIndex` implementation for the lexical layer.
//!
//! Opens a persistent index, executes n-gram lookups, intersects posting lists,
//! scores documents via BM25F, and returns ranked results.
//!
//! # Phase status
//!
//! This module contains the minimal `LexicalSearchLayer` struct used by the builder.
//! Phase 3 will implement the full query logic in [`SearchLayer::search`].

use std::path::{Path, PathBuf};

use crate::{FileId, FileTable, IndexStats, SearchIndex, SearchLayer, SearchQuery};

use super::{index_format::IndexReader, scoring::Bm25Scorer, Bm25Params, IndexMetadata};

// ============================================================================
// LexicalSearchLayer
// ============================================================================

/// Persistent lexical search layer backed by a mmap'd on-disk index.
///
/// Constructed by [`super::builder::LexicalLayerBuilder::build`] or opened
/// directly via [`LexicalSearchLayer::open`].
///
/// # Phase 3 Note
///
/// The [`SearchLayer::search`] implementation is a stub that returns `Ok(vec![])`.
/// Full BM25F query logic will be added in Phase 3.
// `reader` and `scorer` are unused until Phase 3 implements the full query pipeline.
#[allow(dead_code)]
pub struct LexicalSearchLayer {
    pub(crate) reader: IndexReader,
    pub(crate) scorer: Bm25Scorer,
    pub(crate) file_table: FileTable,
    pub(crate) metadata: IndexMetadata,
}

impl LexicalSearchLayer {
    /// Open a previously built index from `dir`.
    ///
    /// Reads `metadata.json`, constructs a [`Bm25Scorer`], and opens the
    /// mmap'd index files.
    ///
    /// # Errors
    ///
    /// - [`SearchError::Io`] if any file cannot be read.
    /// - [`SearchError::SerializationError`] if `metadata.json` is malformed.
    /// - [`SearchError::CorruptedIndex`] if the binary index files are invalid.
    pub fn open(dir: &Path) -> crate::Result<Self> {
        // Read and deserialize metadata.json.
        let meta_path = dir.join("metadata.json");
        let meta_str =
            std::fs::read_to_string(&meta_path).map_err(crate::SearchError::Io)?;
        let metadata: IndexMetadata = serde_json::from_str(&meta_str)
            .map_err(|e| crate::SearchError::SerializationError(e.to_string()))?;

        let reader = IndexReader::open(dir)?;

        let scorer = Bm25Scorer::new(
            metadata.bm25_params.clone(),
            metadata.stats.file_count,
        );

        Ok(Self {
            reader,
            scorer,
            file_table: metadata.file_table.clone(),
            metadata,
        })
    }

    /// Return a reference to the raw index params for inspection or testing.
    pub fn bm25_params(&self) -> &Bm25Params {
        &self.metadata.bm25_params
    }
}

// ============================================================================
// Trait implementations
// ============================================================================

impl SearchLayer for LexicalSearchLayer {
    fn search(&self, _query: &SearchQuery) -> crate::Result<Vec<(FileId, f32)>> {
        // TODO(phase-3): Implement full BM25F query pipeline:
        //   1. Extract query n-grams via extract_query_ngrams()
        //   2. For each n-gram, call self.reader.lookup(ngram)
        //   3. Accumulate per-doc term frequencies by field
        //   4. Score each doc with self.scorer.score_term()
        //   5. Sort by score descending and return
        Ok(vec![])
    }
}

impl SearchIndex for LexicalSearchLayer {
    fn file_table(&self) -> &FileTable {
        &self.file_table
    }

    fn stats(&self) -> IndexStats {
        self.metadata.stats.clone()
    }

    fn stored_file_mtimes(&self) -> &[(PathBuf, u64)] {
        &self.metadata.file_mtimes
    }
}
