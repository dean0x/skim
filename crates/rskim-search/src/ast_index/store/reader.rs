//! [`AstIndexReader`] — mmap'd read-only layer for the two-file AST n-gram index.
//!
//! # Memory layout
//!
//! The `ast_index.skidx` file is memory-mapped in its entirety:
//!
//! ```text
//! [AstSkidxHeader: 48 bytes]
//! [AstBigramEntry × bigram_count: 16 bytes each]
//! [AstTrigramEntry × trigram_count: 20 bytes each]
//! [AstFileMetaEntry × file_count: 5 bytes each]
//! ```
//!
//! The `ast_index.skpost` file is memory-mapped when `postings_file_size > 0`.
//! Entry offsets/lengths in the lookup tables index directly into it.
//!
//! # Send + Sync
//!
//! `AstIndexReader` is `Send + Sync` because:
//! - `AstSkidxHeader` is `Copy`.
//! - `Mmap` is `Send + Sync` on all platforms supported by `memmap2`.
//! - All fields are read-only after construction.
//! - No interior mutability.

use std::path::Path;

use memmap2::Mmap;

use super::format::{
    AST_BIGRAM_ENTRY_SIZE, AST_FILE_META_SIZE, AST_HEADER_SIZE, AST_POSTING_ENTRY_SIZE,
    AST_TRIGRAM_ENTRY_SIZE, AstFileMetaEntry, AstSkidxHeader, compute_checksum, decode_file_meta,
    decode_posting, lookup_bigram, lookup_trigram,
};
use crate::{
    Result, SearchError,
    ast_index::{AstBigram, AstTrigram},
};

// ============================================================================
// Public types
// ============================================================================

/// One element of a decoded posting list.
///
/// `doc_id` — the file index (0-based sequential FileId).
/// `count`  — per-file structural term-frequency (always >= 1, per C4).
///
/// Guarantees upheld by the reader (C1–C5):
/// - C1: returned slices are sorted ascending by `doc_id`.
/// - C2: at most one posting per `doc_id` (builder invariant).
/// - C4: `count >= 1` (validated by `decode_posting`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AstPosting {
    /// Zero-based sequential file index.
    pub doc_id: u32,
    /// Per-file structural term-frequency.
    pub count: u32,
}

// ============================================================================
// Reader struct
// ============================================================================

/// Memory-mapped, read-only query layer for the two-file AST n-gram index.
///
/// Constructed via [`AstIndexReader::open`] after an [`super::builder::AstIndexBuilder`]
/// has written `ast_index.skidx` and `ast_index.skpost` to a directory.
pub struct AstIndexReader {
    /// Decoded header (copied out of mmap for cheap repeated access).
    header: AstSkidxHeader,
    /// Memory-mapped `ast_index.skidx` file.
    idx_mmap: Mmap,
    /// Memory-mapped `ast_index.skpost` file.
    /// `None` when `postings_file_size == 0` (empty corpus or all-empty files).
    post_mmap: Option<Mmap>,
}

impl std::fmt::Debug for AstIndexReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AstIndexReader")
            .field("header", &self.header)
            .field("idx_mmap_len", &self.idx_mmap.len())
            .field("post_mmap_len", &self.post_mmap.as_ref().map(|m| m.len()))
            .finish()
    }
}

// AstIndexReader is Send + Sync:
// - AstSkidxHeader: Copy (no heap)
// - Mmap: Send + Sync (memmap2 guarantees)
// - Option<Mmap>: inherits Send + Sync from Mmap
// - All fields read-only after construction
// The A6 test below verifies Send + Sync at compile time via generic bounds.

impl AstIndexReader {
    /// Open an existing AST index from `dir`.
    ///
    /// Validates magic bytes, format version, file sizes, and the CRC32
    /// checksum before returning.
    ///
    /// # Errors
    ///
    /// - [`SearchError::Io`] if `ast_index.skidx` or `ast_index.skpost` cannot
    ///   be opened (e.g. missing files from a different index type).
    /// - [`SearchError::IndexCorrupted`] if any validation fails.
    pub fn open(dir: &Path) -> Result<Self> {
        let idx_path = dir.join("ast_index.skidx");
        let post_path = dir.join("ast_index.skpost");

        let idx_file = std::fs::File::open(&idx_path)?;

        // SAFETY: The file is not modified after mapping. If another process
        // truncates or overwrites it concurrently, behaviour is undefined but
        // this is an inherent constraint of mmap-based indexes (same as lexical).
        let idx_mmap = unsafe { Mmap::map(&idx_file) }?;

        let header = decode_header_and_validate(&idx_mmap)?;

        // ── Size validation (checked arithmetic) ────────────────────────────
        let bigram_bytes = (header.bigram_count as usize)
            .checked_mul(AST_BIGRAM_ENTRY_SIZE)
            .ok_or_else(|| {
                SearchError::IndexCorrupted("bigram_count * AST_BIGRAM_ENTRY_SIZE overflow".into())
            })?;
        let trigram_bytes = (header.trigram_count as usize)
            .checked_mul(AST_TRIGRAM_ENTRY_SIZE)
            .ok_or_else(|| {
                SearchError::IndexCorrupted(
                    "trigram_count * AST_TRIGRAM_ENTRY_SIZE overflow".into(),
                )
            })?;
        let meta_bytes = (header.file_count as usize)
            .checked_mul(AST_FILE_META_SIZE)
            .ok_or_else(|| {
                SearchError::IndexCorrupted("file_count * AST_FILE_META_SIZE overflow".into())
            })?;
        let expected_idx_size = AST_HEADER_SIZE
            .checked_add(bigram_bytes)
            .and_then(|s| s.checked_add(trigram_bytes))
            .and_then(|s| s.checked_add(meta_bytes))
            .ok_or_else(|| SearchError::IndexCorrupted("expected_idx_size overflow".into()))?;

        if idx_mmap.len() != expected_idx_size {
            return Err(SearchError::IndexCorrupted(format!(
                "skidx size mismatch: expected {expected_idx_size}, got {}",
                idx_mmap.len()
            )));
        }

        // ── CRC32 validation ─────────────────────────────────────────────────
        // The checksum covers idx_mmap[AST_HEADER_SIZE..expected_idx_size],
        // the contiguous post-header payload (bigrams + trigrams + file_meta).
        let payload = &idx_mmap[AST_HEADER_SIZE..expected_idx_size];
        let actual_checksum = compute_checksum(payload);
        if actual_checksum != header.checksum {
            return Err(SearchError::IndexCorrupted(format!(
                "checksum mismatch: expected {:#010x}, got {:#010x}",
                header.checksum, actual_checksum
            )));
        }

        // ── Postings mmap ────────────────────────────────────────────────────
        // Do NOT mmap a zero-length file: memmap2 returns Err on some platforms.
        let post_mmap = if header.postings_file_size == 0 {
            None
        } else {
            let post_file = std::fs::File::open(&post_path)?;
            let expected_post_size = usize::try_from(header.postings_file_size).map_err(|_| {
                SearchError::IndexCorrupted(format!(
                    "postings_file_size {} exceeds platform usize",
                    header.postings_file_size
                ))
            })?;
            // SAFETY: same as idx_mmap above.
            let mmap = unsafe { Mmap::map(&post_file) }?;
            if mmap.len() != expected_post_size {
                return Err(SearchError::IndexCorrupted(format!(
                    "skpost size mismatch: expected {}, got {}",
                    header.postings_file_size,
                    mmap.len()
                )));
            }
            Some(mmap)
        };

        Ok(Self {
            header,
            idx_mmap,
            post_mmap,
        })
    }

    // -----------------------------------------------------------------------
    // Public accessors
    // -----------------------------------------------------------------------

    /// Return the number of files in the index.
    #[must_use]
    pub fn file_count(&self) -> u32 {
        self.header.file_count
    }

    /// Return the average emitted-node count per file.
    #[must_use]
    pub fn avg_node_count(&self) -> f32 {
        self.header.avg_node_count
    }

    /// Return the average distinct bigram count per file.
    #[must_use]
    pub fn avg_bigram_count(&self) -> f32 {
        self.header.avg_bigram_count
    }

    /// Return the average distinct trigram count per file.
    #[must_use]
    pub fn avg_trigram_count(&self) -> f32 {
        self.header.avg_trigram_count
    }

    /// Return the [`AstFileMetaEntry`] for the file at sequential index `file_index`.
    ///
    /// `file_index` is the 0-based insertion order (equals the `FileId.0` value).
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::IndexCorrupted`] if `file_index` is out of bounds.
    pub fn file_meta(&self, file_index: u32) -> Result<AstFileMetaEntry> {
        self.file_meta_at(file_index)
    }

    // -----------------------------------------------------------------------
    // Lookup API (C1–C5)
    // -----------------------------------------------------------------------

    /// Look up all postings for an [`AstBigram`].
    ///
    /// Returns `Ok(vec![])` when the key is absent (C2).
    /// Returns `Err(IndexCorrupted)` when bytes are malformed (C3).
    /// The returned slice is sorted ascending by `doc_id` (C1).
    /// Every `count` is >= 1 (C4).
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::IndexCorrupted`] if the posting data is malformed.
    pub fn lookup_bigram(&self, bigram: AstBigram) -> Result<Vec<AstPosting>> {
        let bigram_start = AST_HEADER_SIZE;
        let bigram_end = bigram_start + (self.header.bigram_count as usize) * AST_BIGRAM_ENTRY_SIZE;
        let entries_data = &self.idx_mmap[bigram_start..bigram_end];

        let entry = match lookup_bigram(entries_data, bigram.key())? {
            Some(e) => e,
            None => return Ok(Vec::new()),
        };

        self.lookup_postings_generic(entry.posting_offset, entry.posting_length)
    }

    /// Look up all postings for an [`AstTrigram`].
    ///
    /// Returns `Ok(vec![])` when the key is absent (C2).
    /// Returns `Err(IndexCorrupted)` when bytes are malformed (C3).
    /// The returned slice is sorted ascending by `doc_id` (C1).
    /// Every `count` is >= 1 (C4).
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::IndexCorrupted`] if the posting data is malformed.
    pub fn lookup_trigram(&self, trigram: AstTrigram) -> Result<Vec<AstPosting>> {
        let trigram_start =
            AST_HEADER_SIZE + (self.header.bigram_count as usize) * AST_BIGRAM_ENTRY_SIZE;
        let trigram_end =
            trigram_start + (self.header.trigram_count as usize) * AST_TRIGRAM_ENTRY_SIZE;
        let entries_data = &self.idx_mmap[trigram_start..trigram_end];

        let entry = match lookup_trigram(entries_data, trigram.key())? {
            Some(e) => e,
            None => return Ok(Vec::new()),
        };

        self.lookup_postings_generic(entry.posting_offset, entry.posting_length)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Shared posting-list decode logic for bigram and trigram lookups.
    ///
    /// Validates offset/length bounds, alignment to `AST_POSTING_ENTRY_SIZE`,
    /// and decodes each entry via `decode_posting` (which re-validates count >= 1).
    fn lookup_postings_generic(
        &self,
        posting_offset: u64,
        posting_length: u32,
    ) -> Result<Vec<AstPosting>> {
        // Empty posting list (e.g. zero-length posting_length from a corrupt entry)
        if posting_length == 0 {
            return Ok(Vec::new());
        }

        // No postings file → empty corpus
        let post_mmap = match &self.post_mmap {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };

        let start = usize::try_from(posting_offset).map_err(|_| {
            SearchError::IndexCorrupted(format!("posting_offset {posting_offset} exceeds usize"))
        })?;
        let length = posting_length as usize;
        let end = start.checked_add(length).ok_or_else(|| {
            SearchError::IndexCorrupted(format!("posting slice overflow: {start} + {length}"))
        })?;
        if end > post_mmap.len() {
            return Err(SearchError::IndexCorrupted(format!(
                "posting slice [{start}..{end}] out of bounds (skpost len={})",
                post_mmap.len()
            )));
        }
        if !length.is_multiple_of(AST_POSTING_ENTRY_SIZE) {
            return Err(SearchError::IndexCorrupted(format!(
                "posting_length {length} not aligned to AST_POSTING_ENTRY_SIZE \
                 {AST_POSTING_ENTRY_SIZE}"
            )));
        }

        let data = &post_mmap[start..end];
        let n = length / AST_POSTING_ENTRY_SIZE;
        let mut postings = Vec::with_capacity(n);
        for i in 0..n {
            let off = i * AST_POSTING_ENTRY_SIZE;
            let raw = decode_posting(&data[off..off + AST_POSTING_ENTRY_SIZE])?;
            postings.push(AstPosting {
                doc_id: raw.doc_id,
                count: raw.count,
            });
        }
        Ok(postings)
    }

    /// Read the [`AstFileMetaEntry`] for the file at sequential index `file_index`.
    fn file_meta_at(&self, file_index: u32) -> Result<AstFileMetaEntry> {
        let bigram_bytes = (self.header.bigram_count as usize) * AST_BIGRAM_ENTRY_SIZE;
        let trigram_bytes = (self.header.trigram_count as usize) * AST_TRIGRAM_ENTRY_SIZE;
        let meta_start = AST_HEADER_SIZE + bigram_bytes + trigram_bytes;
        let offset = meta_start + (file_index as usize) * AST_FILE_META_SIZE;
        let end = offset
            .checked_add(AST_FILE_META_SIZE)
            .filter(|&e| e <= self.idx_mmap.len())
            .ok_or_else(|| {
                SearchError::IndexCorrupted(format!(
                    "file_meta_at({file_index}): offset {offset} out of bounds \
                     (idx_mmap len={})",
                    self.idx_mmap.len()
                ))
            })?;
        decode_file_meta(&self.idx_mmap[offset..end])
    }
}

// ============================================================================
// Header decode + validate (private free function)
// ============================================================================

/// Decode and validate the header from the start of the idx mmap.
///
/// Called exclusively by `AstIndexReader::open` so the validate logic is
/// not duplicated between open and any future re-validate path.
fn decode_header_and_validate(idx_mmap: &[u8]) -> Result<AstSkidxHeader> {
    use super::format::decode_header;
    decode_header(idx_mmap)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "reader_tests.rs"]
mod tests;
