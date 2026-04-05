//! Query engine: `SearchLayer` + `SearchIndex` implementation for the lexical layer.
//!
//! Opens a persistent index, executes n-gram lookups, intersects posting lists,
//! scores documents via BM25F, and returns ranked results.
//!
//! # Pipeline
//!
//! 1. Extract query n-grams via [`ngram::extract_query_ngrams`].
//! 2. For each n-gram, load postings from the main index and (if present) the delta.
//! 3. Filter tombstoned doc_ids from main-index results.
//! 4. Accumulate per-document, per-field term frequencies.
//! 5. Score each document with [`Bm25Scorer::score_term`].
//! 6. Sort descending by score, apply offset and limit, return `Vec<(FileId, f32)>`.

use std::path::{Path, PathBuf};

use rustc_hash::FxHashMap;

use crate::{FileId, FileTable, IndexStats, SearchField, SearchIndex, SearchLayer, SearchQuery};

use super::{
    index_format::{DeltaReader, IndexReader, Tombstones},
    ngram::extract_query_ngrams,
    scoring::Bm25Scorer,
    Bm25Params, IndexMetadata, PostingEntry,
};

// ============================================================================
// LexicalSearchLayer
// ============================================================================

/// Persistent lexical search layer backed by a mmap'd on-disk index.
///
/// Constructed by [`super::builder::LexicalLayerBuilder::build`] or opened
/// directly via [`LexicalSearchLayer::open`].
pub struct LexicalSearchLayer {
    pub(crate) reader: IndexReader,
    pub(crate) scorer: Bm25Scorer,
    pub(crate) file_table: FileTable,
    pub(crate) metadata: IndexMetadata,
    /// Index directory — required to locate delta and tombstone files at query time.
    pub(crate) index_dir: PathBuf,
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
            index_dir: dir.to_path_buf(),
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
    fn search(&self, query: &SearchQuery) -> crate::Result<Vec<(FileId, f32)>> {
        // Step 1: Require a non-empty text query.
        let text = match &query.text_query {
            Some(t) if !t.is_empty() => t.as_str(),
            _ => return Ok(vec![]),
        };

        // Step 2: Extract query n-grams.
        let query_ngrams = extract_query_ngrams(text);
        if query_ngrams.is_empty() {
            return Ok(vec![]);
        }

        // Step 3: Load tombstones (returns empty set if file absent).
        let tombstones = Tombstones::load(&self.index_dir)?;

        // Step 4: Load delta reader (returns None if delta absent or empty).
        let delta = DeltaReader::open(&self.index_dir)?;

        // Step 5: For each query n-gram, accumulate per-document scores.
        //
        // Structure:
        //   doc_scores: doc_id → cumulative BM25F score across all query terms
        //
        // For each ngram we:
        //   a. Gather main-index postings, filter tombstoned doc_ids.
        //   b. Gather delta postings (no tombstone filter — delta only adds new docs).
        //   c. Group by doc_id → per-field TF list.
        //   d. Compute df (unique doc count) and score each doc for this term.
        let mut doc_scores: FxHashMap<u32, f32> = FxHashMap::default();

        for (ngram, _query_weight) in &query_ngrams {
            // Collect per-doc field-TF pairs from main index + delta.
            // ngram_docs: doc_id → Vec<(SearchField, tf)>
            let mut ngram_docs: FxHashMap<u32, Vec<(SearchField, u16)>> = FxHashMap::default();

            // Main index postings.
            if let Some(postings) = self.reader.lookup(*ngram) {
                for entry in postings {
                    if tombstones.contains(entry.doc_id) {
                        continue;
                    }
                    if let Some(field) = SearchField::from_u8(entry.field_id) {
                        ngram_docs
                            .entry(entry.doc_id)
                            .or_default()
                            .push((field, entry.tf));
                    }
                }
            }

            // Delta postings (merged on top of main index — newest wins by union).
            if let Some(ref dr) = delta {
                for entry in dr.scan(*ngram) {
                    if let Some(field) = SearchField::from_u8(entry.field_id) {
                        ngram_docs
                            .entry(entry.doc_id)
                            .or_default()
                            .push((field, entry.tf));
                    }
                }
            }

            if ngram_docs.is_empty() {
                continue;
            }

            let df = ngram_docs.len() as u64;

            // Score each doc for this query term and accumulate.
            for (doc_id, field_tfs) in &ngram_docs {
                // doc_len = 0: length normalization falls back to 1.0 when avg_doc_len = 0.
                // The builder stores avg_doc_len in metadata; for queries we pass 0 here
                // and rely on the scorer's guard (when avg_doc_len = 0, len_norm = 1.0).
                // TODO(phase-4): store per-doc lengths in metadata for accurate normalization.
                let score = self.scorer.score_term(field_tfs, 0, df);
                *doc_scores.entry(*doc_id).or_insert(0.0) += score;
            }
        }

        // Step 6: Sort by score descending.
        let mut results: Vec<(u32, f32)> = doc_scores.into_iter().collect();
        results.sort_by(|a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Step 7: Apply offset + limit and map to FileId.
        let results: Vec<(FileId, f32)> = results
            .into_iter()
            .skip(query.offset)
            .take(query.limit)
            .map(|(doc_id, score)| (FileId::new(doc_id as u64), score))
            .collect();

        Ok(results)
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

// ============================================================================
// Helpers shared across query.rs and tests
// ============================================================================

/// Merge accumulated per-doc field TF entries, deduplicating by (doc_id, field)
/// by summing TF values. This is used when the same n-gram appears in both the
/// main index and the delta for the same doc/field.
///
/// Not currently used by the production pipeline (which simply appends delta
/// entries), but available for callers that need deduplication.
#[allow(dead_code)]
fn merge_field_tfs(entries: &[(SearchField, u16)]) -> Vec<(SearchField, u16)> {
    let mut merged: FxHashMap<SearchField, u16> = FxHashMap::default();
    for (field, tf) in entries {
        *merged.entry(*field).or_insert(0) = merged
            .get(field)
            .copied()
            .unwrap_or(0)
            .saturating_add(*tf);
    }
    merged.into_iter().collect()
}

// Silence unused-import warnings for PostingEntry (used implicitly through index_format).
const _: () = {
    let _ = std::mem::size_of::<PostingEntry>();
};
