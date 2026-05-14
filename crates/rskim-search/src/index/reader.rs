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

// NgramIndexReader is automatically Send + Sync because all fields
// (SkidxHeader: Copy, Mmap: Send+Sync) satisfy the auto-trait bounds.

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
        let entries_bytes = (header.ngram_count as usize)
            .checked_mul(SKIDX_ENTRY_SIZE)
            .ok_or_else(|| {
                SearchError::IndexCorrupted(
                    "ngram_count * SKIDX_ENTRY_SIZE overflow".into(),
                )
            })?;
        let meta_bytes = (header.file_count as usize)
            .checked_mul(FILE_META_SIZE)
            .ok_or_else(|| {
                SearchError::IndexCorrupted("file_count * FILE_META_SIZE overflow".into())
            })?;
        let expected_idx_size = SKIDX_HEADER_SIZE
            .checked_add(entries_bytes)
            .and_then(|s| s.checked_add(meta_bytes))
            .ok_or_else(|| {
                SearchError::IndexCorrupted("expected_idx_size overflow".into())
            })?;
        if idx_mmap.len() != expected_idx_size {
            return Err(SearchError::IndexCorrupted(format!(
                "skidx size mismatch: expected {expected_idx_size}, got {}",
                idx_mmap.len()
            )));
        }
        let expected_post_size = usize::try_from(header.postings_file_size).map_err(|_| {
            SearchError::IndexCorrupted(format!(
                "postings_file_size {} exceeds platform usize",
                header.postings_file_size
            ))
        })?;
        if post_mmap.len() != expected_post_size {
            return Err(SearchError::IndexCorrupted(format!(
                "skpost size mismatch: expected {}, got {}",
                header.postings_file_size,
                post_mmap.len()
            )));
        }

        // Verify CRC32 checksum over entries + file metadata.  The slice is
        // contiguous in the mmap so no copy is needed.
        let payload = &idx_mmap[SKIDX_HEADER_SIZE..expected_idx_size];
        let actual_checksum = compute_checksum(payload);
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

        let start = usize::try_from(entry.posting_offset).map_err(|_| {
            SearchError::IndexCorrupted(format!(
                "posting_offset {} exceeds usize",
                entry.posting_offset
            ))
        })?;
        let length = entry.posting_length as usize;
        let end = start.checked_add(length).ok_or_else(|| {
            SearchError::IndexCorrupted(format!(
                "posting slice overflow: {start} + {length}"
            ))
        })?;
        if end > self.post_mmap.len() {
            return Err(SearchError::IndexCorrupted(format!(
                "posting slice [{start}..{end}] out of bounds (skpost len={})",
                self.post_mmap.len()
            )));
        }

        let posting_len = length;
        if posting_len % super::format::POSTING_ENTRY_SIZE != 0 {
            return Err(SearchError::IndexCorrupted(format!(
                "posting_length {posting_len} not aligned to POSTING_ENTRY_SIZE {}",
                super::format::POSTING_ENTRY_SIZE
            )));
        }
        let data = &self.post_mmap[start..end];
        let n = posting_len / super::format::POSTING_ENTRY_SIZE;
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

        // Language filter resolved up-front so we can skip scoring documents that
        // won't pass the filter.
        let lang_filter: Option<u8> = query.lang.map(super::format::lang_to_id);

        // doc_id → accumulated BM25 score.
        let mut doc_scores: HashMap<u32, f64> = HashMap::new();
        // doc_id → match positions (collected during the single posting pass).
        let mut doc_positions: HashMap<u32, Vec<std::ops::Range<usize>>> = HashMap::new();
        // Cache of doc_length per doc_id to avoid re-decoding file metadata for
        // the same document across multiple bigram iterations.
        let mut doc_len_cache: HashMap<u32, u32> = HashMap::new();

        for (ngram, _weight) in &ngrams {
            let postings = self.lookup_postings(ngram.key())?;
            let idf = idf_for_key(ngram.key());

            // Single pass: accumulate tf counts and positions simultaneously.
            let mut tf_per_doc: HashMap<u32, u32> = HashMap::new();
            for p in &postings {
                *tf_per_doc.entry(p.doc_id).or_default() += 1;
                let pos = p.position as usize;
                doc_positions
                    .entry(p.doc_id)
                    .or_default()
                    .push(pos..pos + 2);
            }

            for (doc_id, tf) in tf_per_doc {
                // Apply language filter before scoring to avoid decoding metadata
                // for documents that won't appear in results.
                if let Some(required_lang) = lang_filter {
                    if doc_id < self.header.file_count {
                        let meta = self.file_meta_at(doc_id)?;
                        // Cache doc_length at the same time we read meta.
                        doc_len_cache.entry(doc_id).or_insert(meta.doc_length);
                        if meta.lang_id != required_lang {
                            continue;
                        }
                    }
                }

                // Resolve doc_length from cache (populated above when lang filter
                // reads the meta, or populated here on first encounter without filter).
                let doc_len = if doc_id < self.header.file_count {
                    if let Some(&cached) = doc_len_cache.get(&doc_id) {
                        cached
                    } else {
                        let len = self.file_meta_at(doc_id)?.doc_length;
                        doc_len_cache.insert(doc_id, len);
                        len
                    }
                } else {
                    0
                };

                let contribution = bm25_score(tf as f32, idf, doc_len, self.header.avg_doc_length);
                *doc_scores.entry(doc_id).or_default() += contribution;
            }
        }

        let mut scored: Vec<(u32, f64)> = doc_scores.into_iter().collect();
        scored.sort_unstable_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let offset = query.offset.unwrap_or(0);
        let limit = query.limit.unwrap_or(20);

        let mut results: Vec<SearchResult> = Vec::new();
        let mut count = 0usize;
        for (doc_id, score) in scored {
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
