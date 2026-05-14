//! [`NgramIndexReader`] — mmap'd BM25 query layer for the two-file n-gram index.
//!
//! # Memory layout
//!
//! The `.skidx` file is memory-mapped in its entirety.  The layout is:
//!
//! ```text
//! [SkidxHeader: 30 bytes]
//! [SkidxEntry × ngram_count: 14 bytes each]
//! [FileMetaEntry × file_count: 5 bytes each]
//! ```
//!
//! The `.skpost` file is also memory-mapped.  Entry offsets/lengths in the
//! `.skidx` lookup table directly index into it.
//!
//! # Send + Sync
//!
//! `NgramIndexReader` is `Send + Sync` because:
//! - Both `Mmap` fields are `Send + Sync` on all platforms supported by
//!   `memmap2`.
//! - All fields are read-only after construction.
//! - The `SearchLayer` trait bound requires `Send + Sync`.

use std::collections::HashMap;
use std::path::Path;

use memmap2::Mmap;

use super::format::{
    FILE_META_SIZE, FileMetaEntry, SKIDX_ENTRY_SIZE, SKIDX_HEADER_SIZE, SkidxHeader, bm25_score,
    compute_checksum, decode_file_meta, decode_header, decode_posting, idf_for_key, lookup_ngram,
};
use crate::{
    FileId, IndexStats, Result, SearchError, SearchField, SearchLayer, SearchQuery, SearchResult,
    ngram::extract_query_ngrams,
};

// ============================================================================
// Reader struct
// ============================================================================

/// Memory-mapped, read-only query layer for the two-file n-gram index.
///
/// Constructed via [`NgramIndexReader::open`] after an
/// [`super::builder::NgramIndexBuilder`] has written `index.skidx` and
/// `index.skpost` to a directory.
pub struct NgramIndexReader {
    /// Decoded header (copied out of mmap for cheap access).
    header: SkidxHeader,
    /// Memory-mapped `.skidx` file (header + entries + file metadata).
    idx_mmap: Mmap,
    /// Memory-mapped `.skpost` file (concatenated posting lists).
    post_mmap: Mmap,
}

// SAFETY: Both Mmap fields are read-only after construction and `Mmap` itself
// is Send + Sync on all supported platforms (see memmap2 docs).
unsafe impl Send for NgramIndexReader {}
unsafe impl Sync for NgramIndexReader {}

impl NgramIndexReader {
    /// Open an existing index from `dir`.
    ///
    /// Validates magic bytes, format version, file sizes, and the CRC32
    /// checksum before returning.
    ///
    /// # Errors
    ///
    /// - [`SearchError::Io`] if the index files cannot be opened.
    /// - [`SearchError::IndexCorrupted`] if validation fails.
    pub fn open(dir: &Path) -> Result<Self> {
        let idx_path = dir.join("index.skidx");
        let post_path = dir.join("index.skpost");

        let idx_file = std::fs::File::open(&idx_path)?;
        let post_file = std::fs::File::open(&post_path)?;

        // SAFETY: The files are not modified after mapping.  If another
        // process truncates or overwrites them concurrently, behaviour is
        // undefined but this is an inherent constraint of mmap-based indexes.
        let idx_mmap = unsafe { Mmap::map(&idx_file) }?;
        let post_mmap = unsafe { Mmap::map(&post_file) }?;

        let header = decode_header(&idx_mmap)?;

        // Validate sizes are internally consistent.
        let expected_idx_size = SKIDX_HEADER_SIZE
            + (header.ngram_count as usize) * SKIDX_ENTRY_SIZE
            + (header.file_count as usize) * FILE_META_SIZE;
        if idx_mmap.len() != expected_idx_size {
            return Err(SearchError::IndexCorrupted(format!(
                "skidx size mismatch: expected {expected_idx_size}, got {}",
                idx_mmap.len()
            )));
        }
        if post_mmap.len() != header.postings_file_size as usize {
            return Err(SearchError::IndexCorrupted(format!(
                "skpost size mismatch: expected {}, got {}",
                header.postings_file_size,
                post_mmap.len()
            )));
        }

        // Verify CRC32 checksum over entries + file metadata.
        let entries_start = SKIDX_HEADER_SIZE;
        let entries_end = entries_start + (header.ngram_count as usize) * SKIDX_ENTRY_SIZE;
        let meta_end = entries_end + (header.file_count as usize) * FILE_META_SIZE;

        let mut checksum_data = Vec::with_capacity(meta_end - entries_start);
        checksum_data.extend_from_slice(&idx_mmap[entries_start..meta_end]);
        let actual_checksum = compute_checksum(&checksum_data);
        if actual_checksum != header.checksum {
            return Err(SearchError::IndexCorrupted(format!(
                "checksum mismatch: expected {:#010x}, got {:#010x}",
                header.checksum, actual_checksum
            )));
        }

        Ok(Self {
            header,
            idx_mmap,
            post_mmap,
        })
    }

    /// Return summary statistics for this index.
    #[must_use]
    pub fn stats(&self) -> IndexStats {
        IndexStats {
            file_count: self.header.file_count,
            total_ngrams: self.header.ngram_count as u64,
            index_size_bytes: (self.idx_mmap.len() + self.post_mmap.len()) as u64,
            last_updated: None,
        }
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Read the [`FileMetaEntry`] for the file at sequential index `file_index`.
    ///
    /// `file_index` is the zero-based insertion order, not a [`FileId`].
    fn file_meta_at(&self, file_index: u32) -> Result<FileMetaEntry> {
        let entries_end = SKIDX_HEADER_SIZE + (self.header.ngram_count as usize) * SKIDX_ENTRY_SIZE;
        let offset = entries_end + (file_index as usize) * FILE_META_SIZE;
        if offset + FILE_META_SIZE > self.idx_mmap.len() {
            return Err(SearchError::IndexCorrupted(format!(
                "file_meta_at({file_index}): offset {offset} out of bounds"
            )));
        }
        decode_file_meta(&self.idx_mmap[offset..offset + FILE_META_SIZE])
    }

    /// Retrieve all posting entries for `ngram_key` from the mmap'd posting file.
    fn lookup_postings(&self, ngram_key: u16) -> Result<Vec<super::format::PostingEntry>> {
        let entries_start = SKIDX_HEADER_SIZE;
        let entries_end = entries_start + (self.header.ngram_count as usize) * SKIDX_ENTRY_SIZE;
        let entries_data = &self.idx_mmap[entries_start..entries_end];

        let entry = match lookup_ngram(entries_data, ngram_key)? {
            Some(e) => e,
            None => return Ok(Vec::new()),
        };

        let start = entry.posting_offset as usize;
        let end = start + entry.posting_length as usize;
        if end > self.post_mmap.len() {
            return Err(SearchError::IndexCorrupted(format!(
                "posting slice [{start}..{end}] out of bounds (skpost len={})",
                self.post_mmap.len()
            )));
        }

        let data = &self.post_mmap[start..end];
        let n = entry.posting_length as usize / super::format::POSTING_ENTRY_SIZE;
        let mut result = Vec::with_capacity(n);
        for i in 0..n {
            let off = i * super::format::POSTING_ENTRY_SIZE;
            result.push(decode_posting(
                &data[off..off + super::format::POSTING_ENTRY_SIZE],
            )?);
        }
        Ok(result)
    }
}

// ============================================================================
// SearchLayer implementation
// ============================================================================

impl SearchLayer for NgramIndexReader {
    /// Execute a BM25-scored n-gram search.
    ///
    /// # Algorithm
    ///
    /// 1. Extract query bigrams via [`extract_query_ngrams`] (sorted by weight desc).
    /// 2. For each bigram, retrieve its posting list.
    /// 3. Accumulate per-document: term frequency and all match positions.
    /// 4. Apply language filter if `query.lang` is set.
    /// 5. Score each document with BM25, using per-bigram IDF from the weight table.
    /// 6. Sort descending by score, apply offset/limit (default: 0/20).
    /// 7. Return [`SearchResult`] values with `line_range: 0..0` and `snippet: None`
    ///    (v1 — full line/snippet extraction is deferred to a later wave).
    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        if query.text.is_empty() {
            return Ok(Vec::new());
        }

        let ngrams = extract_query_ngrams(&query.text);
        if ngrams.is_empty() {
            return Ok(Vec::new());
        }

        // doc_id → (accumulated BM25 score, positions)
        let mut doc_tf: HashMap<u32, f64> = HashMap::new();
        let mut doc_positions: HashMap<u32, Vec<std::ops::Range<usize>>> = HashMap::new();

        for (ngram, _weight) in &ngrams {
            let postings = self.lookup_postings(ngram.key())?;
            let idf = idf_for_key(ngram.key());
            let tf_per_doc = {
                let mut counts: HashMap<u32, u32> = HashMap::new();
                for p in &postings {
                    *counts.entry(p.doc_id).or_default() += 1;
                }
                counts
            };
            for (doc_id, tf) in tf_per_doc {
                let doc_len = if doc_id < self.header.file_count {
                    self.file_meta_at(doc_id)?.doc_length
                } else {
                    0
                };
                let contribution = bm25_score(tf as f32, idf, doc_len, self.header.avg_doc_length);
                *doc_tf.entry(doc_id).or_default() += contribution;
            }
            for p in &postings {
                let pos = p.position as usize;
                doc_positions
                    .entry(p.doc_id)
                    .or_default()
                    .push(pos..pos + 2);
            }
        }

        // Language filter.
        let lang_filter: Option<u8> = query.lang.map(super::format::lang_to_id);

        let mut scored: Vec<(u32, f64)> = doc_tf.into_iter().collect();
        scored.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let offset = query.offset.unwrap_or(0);
        let limit = query.limit.unwrap_or(20);

        let mut results: Vec<SearchResult> = Vec::new();
        let mut count = 0usize;
        for (doc_id, score) in scored {
            // Apply language filter.
            if let Some(required_lang) = lang_filter
                && doc_id < self.header.file_count
            {
                let meta = self.file_meta_at(doc_id)?;
                if meta.lang_id != required_lang {
                    continue;
                }
            }

            if count < offset {
                count += 1;
                continue;
            }
            if results.len() >= limit {
                break;
            }

            let positions = doc_positions.remove(&doc_id).unwrap_or_default();
            results.push(SearchResult {
                file_id: FileId(doc_id),
                score,
                line_range: 0..0,
                match_positions: positions,
                field: SearchField::Other,
                snippet: None,
            });
            count += 1;
        }

        Ok(results)
    }

    fn name(&self) -> &str {
        "ngram-index"
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "reader_tests.rs"]
mod tests;
