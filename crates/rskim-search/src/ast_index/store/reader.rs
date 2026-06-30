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
//! [AstFileMetaEntry × file_count: 15 bytes each]
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
    AstFileMetaEntry, AstSkidxHeader, BIGRAM_ENTRY_SIZE, FILE_META_SIZE, HEADER_SIZE,
    POSTING_ENTRY_SIZE, SKAX_MAGIC, TRIGRAM_ENTRY_SIZE, compute_checksum, decode_file_meta,
    decode_header, decode_lang_and_node_count, decode_posting, lookup_bigram, lookup_trigram,
};
use crate::{
    Result, SearchError,
    ast_index::{AstBigram, AstTrigram, StructuralMetrics},
};

// ============================================================================
// Public types
// ============================================================================

/// One element of a decoded posting list.
///
/// `doc_id` — the file index (0-based sequential FileId).
/// `count`  — per-file structural term-frequency (always >= 1, per C4/C5).
///
/// Reader API contracts (C1–C7):
/// - C1: postings returned by `lookup_bigram`/`lookup_trigram` are sorted
///   ascending by `doc_id`, at most one per `doc_id` (validated on decode;
///   see `lookup_postings_generic`).
/// - C2: absent key → `Ok(vec![])` (no error, no panic).
/// - C3: malformed entry (bad offset/len, OOB, `len % 8 != 0`) →
///   `Err(IndexCorrupted)`.
/// - C4: `count >= 1` (validated by `decode_posting`).
/// - C5: `count` is the structural term-frequency from `extract_ast_ngrams`;
///   use it for BM25-style scoring at query time (Wave 3f).
/// - C6: `file_meta(i).language()` recovers the [`rskim_core::Language`] for
///   file `i`; returns `None` for unrecognised IDs (future-compat).
/// - C7: `AstIndexReader` is `Send + Sync` (verified by test A6 via generic
///   bound).
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

// C7: AstIndexReader is Send + Sync.
// - AstSkidxHeader: Copy (no heap)
// - Mmap: Send + Sync (memmap2 guarantees)
// - Option<Mmap>: inherits Send + Sync from Mmap
// - All fields read-only after construction; no interior mutability.
// The A6 test in reader_tests.rs verifies Send + Sync at compile time via
// a generic bound: `fn assert_send_sync<T: Send + Sync>() {}`.

impl AstIndexReader {
    // -----------------------------------------------------------------------
    // Private layout helpers — single source of truth for section offsets.
    // -----------------------------------------------------------------------

    /// Byte range of the bigram table within `idx_mmap`: `[48 .. 48 + bigram_bytes)`.
    ///
    /// Safety: `open()` validates `idx_mmap.len() == expected_idx_size` using
    /// checked arithmetic before constructing a reader, so
    /// `bigram_count * BIGRAM_ENTRY_SIZE` is guaranteed not to overflow.
    /// The `saturating_mul` / `saturating_add` here are belt-and-suspenders
    /// guards that produce a coherent (shrunk) range rather than panicking
    /// if the struct were somehow constructed with out-of-range counts.
    fn bigram_table_range(&self) -> std::ops::Range<usize> {
        let bigram_bytes = (self.header.bigram_count as usize).saturating_mul(BIGRAM_ENTRY_SIZE);
        HEADER_SIZE..HEADER_SIZE.saturating_add(bigram_bytes)
    }

    /// Byte range of the trigram table: `[bigram_end .. bigram_end + trigram_bytes)`.
    fn trigram_table_range(&self) -> std::ops::Range<usize> {
        let bigram_end = self.bigram_table_range().end;
        let trigram_bytes = (self.header.trigram_count as usize).saturating_mul(TRIGRAM_ENTRY_SIZE);
        bigram_end..bigram_end.saturating_add(trigram_bytes)
    }

    /// Byte offset where the file-meta section starts (= trigram_table end).
    fn meta_start(&self) -> usize {
        self.trigram_table_range().end
    }

    // -----------------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------------

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

        let header = decode_header(&idx_mmap)?;

        // ── Size validation (checked arithmetic) ────────────────────────────
        let bigram_bytes = (header.bigram_count as usize)
            .checked_mul(BIGRAM_ENTRY_SIZE)
            .ok_or_else(|| {
                SearchError::IndexCorrupted("bigram_count * BIGRAM_ENTRY_SIZE overflow".into())
            })?;
        let trigram_bytes = (header.trigram_count as usize)
            .checked_mul(TRIGRAM_ENTRY_SIZE)
            .ok_or_else(|| {
                SearchError::IndexCorrupted("trigram_count * TRIGRAM_ENTRY_SIZE overflow".into())
            })?;
        let meta_bytes = (header.file_count as usize)
            .checked_mul(FILE_META_SIZE)
            .ok_or_else(|| {
                SearchError::IndexCorrupted("file_count * FILE_META_SIZE overflow".into())
            })?;
        let expected_idx_size = HEADER_SIZE
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
        // The checksum covers idx_mmap[HEADER_SIZE..expected_idx_size],
        // the contiguous post-header payload (bigrams + trigrams + file_meta).
        //
        // The dual lexical+AST index is one coherent unit (ADR-006), so this
        // reader carries the SAME validity-marker fast path as the lexical
        // reader (#376, AD-376-5): a marker proving byte-identity to a prior
        // verified open moves the full CRC32 off the --ast per-query hot path.
        // On any marker miss the full CRC32 still runs (corruption guard).
        let marker_path = dir.join("ast_index.skverify");
        let current_sig =
            crate::validity::current_signature(&idx_path, &post_path, header.checksum);

        // Fast path (AC1 analogue): marker (len, mtime, header.checksum) match
        // licenses skipping the CRC32.  TRUST BOUNDARY (AD-376-2, accepted): a
        // byte-flip preserving len+mtime+header.checksum is served unverified.
        let marker_hit = match (&current_sig, crate::validity::read_marker(&marker_path)) {
            (Some(cur), Some(disk)) => disk == *cur,
            _ => false,
        };

        if !marker_hit {
            let payload = &idx_mmap[HEADER_SIZE..expected_idx_size];
            let actual_checksum = compute_checksum(payload);
            if actual_checksum != header.checksum {
                return Err(SearchError::IndexCorrupted(format!(
                    "checksum mismatch: expected {:#010x}, got {:#010x}",
                    header.checksum, actual_checksum
                )));
            }
            // Full verify succeeded: stamp a fresh marker for the next open
            // (AC6: a failed write must not fail open()).
            if let Some(sig) = current_sig {
                crate::validity::write_marker_best_effort(dir, &marker_path, &sig);
            }
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

    /// Probe the format version of an on-disk AST index without fully opening it.
    ///
    /// Reads only the first 6 bytes of `ast_index.skidx` (magic + version) to
    /// cheaply determine the format version. Returns `Err(IndexCorrupted)` if
    /// the file is too short, has bad magic bytes, or cannot be read.
    ///
    /// Intended for staleness checks (Wave 3f/3g): callers can probe the version
    /// before attempting a full `open`, which would fail with a version error.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Io`] if the file cannot be opened.
    /// Returns [`SearchError::IndexCorrupted`] if the file is too short or has
    /// bad magic bytes.
    pub fn index_version(dir: &Path) -> Result<u16> {
        use std::io::Read;
        let idx_path = dir.join("ast_index.skidx");
        let mut file = std::fs::File::open(&idx_path)?;
        let mut buf = [0u8; 6];
        file.read_exact(&mut buf).map_err(|_| {
            SearchError::IndexCorrupted(
                "index_version: ast_index.skidx too short (need 6 bytes)".into(),
            )
        })?;
        let magic = &buf[0..4];
        if magic != SKAX_MAGIC {
            return Err(SearchError::IndexCorrupted(format!(
                "index_version: bad magic: expected {:?}, got {:?}",
                SKAX_MAGIC, magic
            )));
        }
        let version = u16::from_le_bytes([buf[4], buf[5]]);
        Ok(version)
    }

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
    /// # C6 — language recovery
    ///
    /// Call [`AstFileMetaEntry::language`] on the returned entry to recover the
    /// [`rskim_core::Language`] from `lang_id`.  Returns `None` for IDs not
    /// recognised by the current binary (future-compat path).
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::IndexCorrupted`] if `file_index` is out of bounds.
    pub fn file_meta(&self, file_index: u32) -> Result<AstFileMetaEntry> {
        let meta_start = self.meta_start();
        // avoids PF-004: use checked arithmetic throughout to prevent silent
        // overflow on 32-bit targets where usize is 32 bits.
        let offset = (file_index as usize)
            .checked_mul(FILE_META_SIZE)
            .and_then(|o| meta_start.checked_add(o))
            .ok_or_else(|| {
                SearchError::IndexCorrupted(format!("file_meta({file_index}): offset overflow"))
            })?;
        let end = offset
            .checked_add(FILE_META_SIZE)
            .filter(|&e| e <= self.idx_mmap.len())
            .ok_or_else(|| {
                SearchError::IndexCorrupted(format!(
                    "file_meta({file_index}): offset {offset} out of bounds \
                     (idx_mmap len={})",
                    self.idx_mmap.len()
                ))
            })?;
        decode_file_meta(&self.idx_mmap[offset..end])
    }

    /// Partial decode — returns only `(lang_id, node_count)` for `file_index`,
    /// reading bytes `[0..5]` of the 15-byte on-disk record.
    ///
    /// This is the hot-path accessor used by `score_postings` (P1, #286).
    /// Skipping the remaining 10 bytes (`max_depth`, `max_block_stmts`,
    /// `max_params`, `branch_count`) reduces the decode cost on the BM25
    /// scoring path while remaining byte-for-byte identical to
    /// `file_meta(file_index)?.lang_id` / `?.node_count`.
    ///
    /// The byte offsets are read through [`decode_lang_and_node_count`] — the
    /// single source of truth shared with [`decode_file_meta`] — so the two
    /// paths cannot drift.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::IndexCorrupted`] if `file_index` is out of bounds
    /// (same error variant as [`file_meta`][Self::file_meta]).
    pub fn file_lang_and_node_count(&self, file_index: u32) -> Result<(u8, u32)> {
        let meta_start = self.meta_start();
        // avoids PF-004: checked arithmetic throughout.
        let offset = (file_index as usize)
            .checked_mul(FILE_META_SIZE)
            .and_then(|o| meta_start.checked_add(o))
            .ok_or_else(|| {
                SearchError::IndexCorrupted(format!(
                    "file_lang_and_node_count({file_index}): offset overflow"
                ))
            })?;
        // We need at least 5 bytes (lang_id + node_count); require the full
        // FILE_META_SIZE slice so the bounds check is identical to file_meta.
        let end = offset
            .checked_add(FILE_META_SIZE)
            .filter(|&e| e <= self.idx_mmap.len())
            .ok_or_else(|| {
                SearchError::IndexCorrupted(format!(
                    "file_lang_and_node_count({file_index}): offset {offset} out of bounds \
                     (idx_mmap len={})",
                    self.idx_mmap.len()
                ))
            })?;
        // decode_lang_and_node_count reads data[0] and data[1..5].
        decode_lang_and_node_count(&self.idx_mmap[offset..end])
    }

    /// Return the per-file structural metrics for the file at sequential index `file_index`.
    ///
    /// Reads the same on-disk entry as [`file_meta`][Self::file_meta] but
    /// extracts the v2-only structural fields as a [`StructuralMetrics`] value.
    ///
    /// `file_index` is the 0-based insertion order.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::IndexCorrupted`] if `file_index` is out of bounds.
    pub fn file_metrics(&self, file_index: u32) -> Result<StructuralMetrics> {
        let entry = self.file_meta(file_index)?;
        Ok(StructuralMetrics {
            max_depth: entry.max_depth,
            max_block_stmts: entry.max_block_stmts,
            max_params: entry.max_params,
            branch_count: entry.branch_count,
        })
    }

    /// Return the average maximum CST depth across all indexed files.
    ///
    /// Stored in header bytes [38..42] (was reserved in v1, now `avg_max_depth`).
    #[must_use]
    pub fn avg_max_depth(&self) -> f32 {
        self.header.avg_max_depth
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
        let bigram_range = self.bigram_table_range();
        let entries_data = &self.idx_mmap[bigram_range];

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
        let trigram_range = self.trigram_table_range();
        let entries_data = &self.idx_mmap[trigram_range];

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
    /// Validates offset/length bounds, alignment to `POSTING_ENTRY_SIZE`,
    /// and decodes each entry via `decode_posting` (which re-validates count >= 1).
    ///
    /// Additionally enforces C1 defensively: each decoded `doc_id` must be
    /// strictly greater than the previous one.  A CRC-valid but unsorted file
    /// (e.g. from a hostile or hand-crafted index) would otherwise produce
    /// silently-wrong query results.
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
        if !length.is_multiple_of(POSTING_ENTRY_SIZE) {
            return Err(SearchError::IndexCorrupted(format!(
                "posting_length {length} not aligned to POSTING_ENTRY_SIZE {POSTING_ENTRY_SIZE}"
            )));
        }

        let data = &post_mmap[start..end];
        let n = length / POSTING_ENTRY_SIZE;
        let mut postings = Vec::with_capacity(n);
        let mut prev_doc_id: Option<u32> = None;
        for i in 0..n {
            let off = i * POSTING_ENTRY_SIZE;
            let raw = decode_posting(&data[off..off + POSTING_ENTRY_SIZE])?;
            // C3: doc_id must refer to a file that actually exists in the index.
            // A CRC-valid hostile index could embed an out-of-range doc_id; any
            // downstream call to file_meta(doc_id) or file_metrics(doc_id) would
            // then access garbage bytes.  Reject here rather than propagate.
            if raw.doc_id >= self.header.file_count {
                return Err(SearchError::IndexCorrupted(format!(
                    "posting doc_id {} out of range (file_count={})",
                    raw.doc_id, self.header.file_count
                )));
            }
            // C1 defensive check: postings must be strictly ascending by doc_id.
            // Collapsed from nested `if let`/`if` to satisfy clippy::collapsible_if.
            if let Some(prev) = prev_doc_id.filter(|&prev| raw.doc_id <= prev) {
                return Err(SearchError::IndexCorrupted(format!(
                    "posting list not sorted: doc_id {prev} followed by {}",
                    raw.doc_id
                )));
            }
            prev_doc_id = Some(raw.doc_id);
            postings.push(AstPosting {
                doc_id: raw.doc_id,
                count: raw.count,
            });
        }
        Ok(postings)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "reader_tests.rs"]
mod tests;
