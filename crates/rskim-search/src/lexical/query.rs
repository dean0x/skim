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

/// Maximum `metadata.json` file size in bytes (100 MB).
///
/// Guards against crafted metadata with millions of `file_mtimes` or
/// `doc_lengths` entries that would cause OOM during deserialization.
const MAX_METADATA_BYTES: u64 = 100_000_000;

// ============================================================================
// LexicalSearchLayer
// ============================================================================

/// Persistent lexical search layer backed by a mmap'd on-disk index.
///
/// Constructed by [`super::builder::LexicalLayerBuilder::build`] or opened
/// directly via [`LexicalSearchLayer::open`].
pub struct LexicalSearchLayer {
    reader: IndexReader,
    scorer: Bm25Scorer,
    metadata: IndexMetadata,
    /// Tombstoned doc_ids, loaded once at open time.
    tombstones: Tombstones,
    /// Delta reader (if a delta file is present), loaded once at open time.
    delta: Option<DeltaReader>,
}

impl LexicalSearchLayer {
    /// Open a previously built index from `dir`.
    ///
    /// Reads `metadata.json`, constructs a [`Bm25Scorer`], and opens the
    /// mmap'd index files. Tombstones and the delta reader are loaded once
    /// here and reused across all queries.
    ///
    /// # Errors
    ///
    /// - [`SearchError::Io`] if any file cannot be read.
    /// - [`SearchError::SerializationError`] if `metadata.json` is malformed.
    /// - [`SearchError::CorruptedIndex`] if the binary index files are invalid
    ///   or the metadata file exceeds `MAX_METADATA_BYTES`.
    #[must_use = "the opened LexicalSearchLayer must be used for queries"]
    pub fn open(dir: &Path) -> crate::Result<Self> {
        let meta_path = dir.join("metadata.json");

        // Guard against OOM from crafted oversized metadata files.
        let meta_len = std::fs::metadata(&meta_path)
            .map_err(crate::SearchError::Io)?
            .len();
        if meta_len > MAX_METADATA_BYTES {
            return Err(crate::SearchError::CorruptedIndex {
                path: meta_path.display().to_string(),
                reason: format!(
                    "metadata.json is {meta_len} bytes, exceeds maximum of {MAX_METADATA_BYTES}"
                ),
            });
        }

        let meta_str = std::fs::read_to_string(&meta_path).map_err(crate::SearchError::Io)?;
        let metadata: IndexMetadata = serde_json::from_str(&meta_str)
            .map_err(|e| crate::SearchError::SerializationError(e.to_string()))?;

        let reader = IndexReader::open(dir)?;

        let scorer = Bm25Scorer::new(metadata.bm25_params.clone(), metadata.stats.file_count);

        // Load tombstones and delta once so every search() call reuses them.
        let tombstones = Tombstones::load(dir)?;
        let delta = DeltaReader::open(dir)?;

        Ok(Self {
            reader,
            scorer,
            metadata,
            tombstones,
            delta,
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

        // Steps 2–5: For each query n-gram, load postings, filter tombstones,
        // accumulate per-field TFs, and score each document.
        //
        // doc_scores accumulates the cumulative BM25F score across all query terms.
        // For each ngram:
        //   a. Load main-index postings (step 2), filter tombstoned doc_ids (step 3).
        //   b. Load delta postings — no tombstone filter; delta only adds new docs.
        //   c. Group by doc_id → per-field TF list (step 4).
        //   d. Compute df (unique doc count) and score each doc for this term (step 5).
        let mut doc_scores: FxHashMap<u32, f32> = FxHashMap::default();
        let mut postings_buf: Vec<PostingEntry> = Vec::new();
        let mut ngram_docs: FxHashMap<u32, Vec<(SearchField, u16)>> = FxHashMap::default();

        for (ngram, _query_weight) in &query_ngrams {
            // Collect per-doc field-TF pairs from main index + delta.
            ngram_docs.clear();

            // Main index postings.
            if self.reader.lookup_into(*ngram, &mut postings_buf) {
                for entry in &postings_buf {
                    if self.tombstones.contains(entry.doc_id) {
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
            if let Some(ref dr) = self.delta {
                dr.scan_into(*ngram, &mut postings_buf);
                for entry in &postings_buf {
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
            // Look up the per-document token count for accurate BM25F length
            // normalization. Falls back to 0 for indexes built without doc_lengths
            // (the scorer treats doc_len=0 the same as avg_doc_len when b > 0).
            for (doc_id, field_tfs) in &ngram_docs {
                let doc_len = self
                    .metadata
                    .doc_lengths
                    .get(*doc_id as usize)
                    .copied()
                    .unwrap_or(0);
                let score = self.scorer.score_term(field_tfs, doc_len, df);
                *doc_scores.entry(*doc_id).or_insert(0.0) += score;
            }
        }

        // Step 6: Sort by score descending, apply offset + limit, map to FileId.
        let mut results: Vec<(u32, f32)> = doc_scores.into_iter().collect();
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
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
        &self.metadata.file_table
    }

    fn stats(&self) -> IndexStats {
        self.metadata.stats.clone()
    }

    fn stored_file_mtimes(&self) -> &[(PathBuf, u64)] {
        &self.metadata.file_mtimes
    }
}
