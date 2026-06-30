//! [`NgramIndexBuilder`] — constructs the two-file mmap'd n-gram index.
//!
//! # Atomicity contract
//!
//! `.skpost` is written first, then `.skidx`.  A reader that finds `.skidx`
//! present can assume `.skpost` is coherent.  A partial write (power loss
//! between the two) leaves no `.skidx`, so the next open attempt fails
//! cleanly with a "file not found" rather than a corrupt read.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;

use super::format::{
    FILE_META_SIZE, FORMAT_VERSION, FileMetaEntry, PostingEntry, SKIDX_ENTRY_SIZE,
    SKIDX_HEADER_SIZE, SKIDX_MAGIC, SkidxEntry, SkidxHeader, encode_entry, encode_file_meta,
    encode_header, encode_postings_varint, lang_to_id,
};
use super::reader::NgramIndexReader;
use crate::{
    FIELD_COUNT, FileId, LayerBuilder, Result, SearchError, SearchField, SearchLayer,
    io_util::atomic_write,
};

/// Capacity-hint upper bound (bytes per posting entry) for the postings buffer
/// in [`NgramIndexBuilder::serialize_index`].
///
/// A v4 entry is `[varint delta_doc_id][u8 field_id][varint delta_position]`.
/// The maximum varint width is 5 bytes each (35-bit span for a u32), giving
/// 5 + 1 + 5 = 11 bytes as the strict upper bound.  We use 9 — the v3 fixed
/// entry size — as a deliberate over-estimate (~2.5x the measured v4 average of
/// ~3.5 bytes/entry on a diverse 1000-file corpus) to avoid reallocation during
/// index build.  After encoding, `postings_buf.shrink_to_fit()` releases the
/// unused capacity before CRC computation and `atomic_write`, so peak RSS
/// reflects the actual encoded size (~3.5 bytes/entry) rather than the
/// upper-bound estimate (9 bytes/entry).  The buffer is build-time only.
///
/// Framing: this is a zero-realloc-during-encode / peak-RSS trade-off.
/// The excess capacity (~2.5x average) is held only for the duration of the
/// encode loop; `shrink_to_fit` reclaims it immediately after.
const VARINT_UPPER_BOUND_PER_ENTRY: usize = 9;

// ============================================================================
// Public builder struct
// ============================================================================

/// Constructs the two-file mmap'd n-gram index from raw file content.
///
/// Call [`LayerBuilder::add_file`] for each file you want to index, then
/// call [`LayerBuilder::build`] to flush the index to disk and obtain a
/// queryable [`NgramIndexReader`].
///
/// Each [`FileId`] must be unique across all `add_file` calls; duplicate IDs
/// are rejected with [`SearchError::InvalidQuery`].
pub struct NgramIndexBuilder {
    /// Directory where `index.skidx` and `index.skpost` will be written.
    output_dir: PathBuf,
    /// Accumulated postings: trigram key → list of (doc_id, field_id, position).
    postings: HashMap<u32, Vec<PostingEntry>>,
    /// Per-file metadata in insertion order (indexed by sequential file_index).
    file_meta: Vec<FileMetaEntry>,
    /// Guard against duplicate FileIds.
    seen_file_ids: HashSet<u32>,
    /// Number of files added.
    file_count: u32,
    /// Sum of all document byte lengths (for avg_doc_length computation).
    total_doc_length: u64,
    /// Sum of per-field byte lengths across all documents (for avg_field_lengths).
    total_field_lengths: [u64; FIELD_COUNT],
}

impl NgramIndexBuilder {
    /// Create a new builder that will write index files to `output_dir`.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Io`] if `output_dir` does not exist.
    pub fn new(output_dir: PathBuf) -> Result<Self> {
        if !output_dir.exists() {
            return Err(SearchError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("output_dir does not exist: {}", output_dir.display()),
            )));
        }
        Ok(Self {
            output_dir,
            postings: HashMap::new(),
            file_meta: Vec::new(),
            seen_file_ids: HashSet::new(),
            file_count: 0,
            total_doc_length: 0,
            total_field_lengths: [0u64; FIELD_COUNT],
        })
    }
}

// ============================================================================
// Classified indexing
// ============================================================================

impl NgramIndexBuilder {
    /// Index the byte-content of a file with pre-computed field classification.
    ///
    /// `field_map` is a sorted, non-overlapping, contiguous list of byte ranges
    /// mapping each byte position to a [`SearchField`], as produced by
    /// [`crate::lexical::classify_source`].
    ///
    /// When `field_map` is empty, all bytes default to [`SearchField::Other`].
    ///
    /// # Errors
    ///
    /// - [`SearchError::InvalidQuery`] if `id` was already added or is not sequential.
    /// - [`SearchError::IndexCorrupted`] if `content` exceeds `u32::MAX` bytes.
    pub fn add_file_classified(
        &mut self,
        id: FileId,
        content: &str,
        lang: rskim_core::Language,
        field_map: &[(Range<usize>, SearchField)],
    ) -> Result<()> {
        if self.seen_file_ids.contains(&id.0) {
            return Err(SearchError::InvalidQuery(format!(
                "duplicate FileId: {}",
                id.0
            )));
        }
        if id.0 != self.file_count {
            return Err(SearchError::InvalidQuery(format!(
                "FileId must equal sequential insertion index: expected {}, got {}",
                self.file_count, id.0
            )));
        }

        let doc_length: u32 = u32::try_from(content.len()).map_err(|_| {
            SearchError::IndexCorrupted(format!(
                "file {} too large: {} bytes exceeds u32::MAX",
                id.0,
                content.len()
            ))
        })?;

        self.seen_file_ids.insert(id.0);

        // Compute per-field byte lengths from the field_map.
        let field_lengths = compute_field_lengths(content.len(), field_map);

        // Accumulate total field lengths for header avg_field_lengths.
        for (i, &fl) in field_lengths.iter().enumerate() {
            self.total_field_lengths[i] += u64::from(fl);
        }

        // Record file metadata.
        self.file_meta.push(FileMetaEntry {
            lang_id: lang_to_id(lang),
            doc_length,
            field_lengths,
        });

        // Scan every 3-byte window (trigram), resolving the field via a linearly
        // advancing pointer through field_map.  Because positions increase
        // monotonically and field_map is sorted ascending, a single forward scan
        // is O(n + m) instead of O(n log m).
        //
        // AD-355-5 / PF-004: widen each byte to u32 before shift arithmetic to
        // prevent u8 overflow: `u32::from(b) << k`, never `b << k`.
        let bytes = content.as_bytes();
        let mut range_idx = 0usize;
        for (pos, window) in bytes.windows(3).enumerate() {
            // Advance past any ranges that have ended before `pos`.
            while range_idx < field_map.len() && field_map[range_idx].0.end <= pos {
                range_idx += 1;
            }
            let field_id = if range_idx < field_map.len() && field_map[range_idx].0.contains(&pos) {
                field_map[range_idx].1.discriminant()
            } else {
                SearchField::Other.discriminant()
            };
            // PF-004: widen to u32 before shifting — never shift on a bare u8.
            let key =
                (u32::from(window[0]) << 16) | (u32::from(window[1]) << 8) | u32::from(window[2]);
            self.postings.entry(key).or_default().push(PostingEntry {
                doc_id: id.0,
                field_id,
                position: pos as u32,
            });
        }

        self.file_count = self.file_count.checked_add(1).ok_or_else(|| {
            SearchError::IndexCorrupted("file_count overflow: too many files".into())
        })?;
        self.total_doc_length += u64::from(doc_length);
        Ok(())
    }
}

// ============================================================================
// Private helpers
// ============================================================================

/// Compute per-field byte lengths from a field_map covering `source_len` bytes.
///
/// Returns an array of `FIELD_COUNT` `u32` values — one per [`SearchField`] discriminant.
/// The sum of the returned values equals `source_len` (enforced by the
/// contiguous invariant of `field_map`).
fn compute_field_lengths(
    source_len: usize,
    field_map: &[(Range<usize>, SearchField)],
) -> [u32; FIELD_COUNT] {
    // Precondition: source_len fits in u32. This is guaranteed by the caller
    // (add_file_classified converts content.len() to u32 before reaching here),
    // and enforced transitively by MAX_SOURCE_BYTES < u32::MAX. The assert
    // documents the invariant so future callers cannot silently violate it.
    debug_assert!(
        source_len <= u32::MAX as usize,
        "source_len {source_len} exceeds u32::MAX — caller must enforce this"
    );
    let mut lengths = [0u32; FIELD_COUNT];
    if field_map.is_empty() {
        // All bytes are Other (discriminant 7).
        lengths[SearchField::Other.discriminant() as usize] =
            u32::try_from(source_len).unwrap_or(u32::MAX);
        return lengths;
    }
    for (range, field) in field_map {
        let range_len = u32::try_from(range.end.saturating_sub(range.start)).unwrap_or(u32::MAX);
        let idx = field.discriminant() as usize;
        lengths[idx] = lengths[idx].saturating_add(range_len);
    }
    lengths
}

// ============================================================================
// LayerBuilder implementation
// ============================================================================

impl LayerBuilder for NgramIndexBuilder {
    /// Index the byte-content of a file.
    ///
    /// Delegates to [`NgramIndexBuilder::add_file_classified`] with an empty
    /// `field_map` so all bytes are classified as [`SearchField::Other`].
    ///
    /// # Errors
    ///
    /// - [`SearchError::InvalidQuery`] if `id` was already added.
    /// - [`SearchError::IndexCorrupted`] if `content` is so large that a byte
    ///   position would overflow `u32`.
    fn add_file(&mut self, id: FileId, content: &str, lang: rskim_core::Language) -> Result<()> {
        self.add_file_classified(id, content, lang, &[])
    }

    /// Finalise the builder: serialise the index to disk and return a reader.
    ///
    /// Write order: `.skpost` first, then `.skidx` (commit point).  Both files
    /// are written atomically via [`crate::io_util::atomic_write`].
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Io`] if writing fails, or
    /// [`SearchError::IndexCorrupted`] if the reader cannot open the result.
    fn build(mut self) -> Result<Box<dyn SearchLayer>>
    where
        Self: Sized,
    {
        // Compute corpus averages.
        let (avg_doc_length, avg_field_lengths) = if self.file_count == 0 {
            (0.0f32, [0.0f32; FIELD_COUNT])
        } else {
            let n = f64::from(self.file_count);
            let avg_doc = (self.total_doc_length as f64 / n) as f32;
            let mut avgs = [0.0f32; FIELD_COUNT];
            for (avg, &total) in avgs.iter_mut().zip(self.total_field_lengths.iter()) {
                *avg = (total as f64 / n) as f32;
            }
            (avg_doc, avgs)
        };

        // Sort posting lists and ngram keys.
        for list in self.postings.values_mut() {
            list.sort_unstable();
        }
        let mut sorted_keys: Vec<u32> = self.postings.keys().copied().collect();
        sorted_keys.sort_unstable();

        // Serialise everything into the two on-disk buffers.
        let (postings_buf, skidx_buf) =
            self.serialize_index(&sorted_keys, avg_doc_length, avg_field_lengths)?;

        let post_path = self.output_dir.join("index.skpost");
        let idx_path = self.output_dir.join("index.skidx");

        // Invalidate any prior validity marker BEFORE writing fresh files
        // (#376, AD-376-4).  The (len, mtime, checksum) signature already
        // self-invalidates on rewrite, but unlinking defensively means a
        // partial or aborted rebuild can never leave a stale marker that would
        // validate the wrong bytes on the next open.
        crate::validity::unlink_marker_best_effort(&self.output_dir.join("index.skverify"));

        // Atomic writes: .skpost first, .skidx second (commit point).
        atomic_write(&self.output_dir, &post_path, &postings_buf)?;
        atomic_write(&self.output_dir, &idx_path, &skidx_buf)?;

        // Verify-back open re-validates the freshly-written bytes and stamps a
        // new index.skverify (AD-376-3 / AC8) so the first post-build query
        // skips the redundant full CRC32.
        let reader = NgramIndexReader::open(&self.output_dir)?;
        Ok(Box::new(reader))
    }
}

impl NgramIndexBuilder {
    /// Serialise postings, entries, file metadata, and header into the two
    /// on-disk byte buffers: `(postings_buf, skidx_buf)`.
    ///
    /// # AD-LXPOST-1
    ///
    /// Postings are encoded using v4 delta+varint compression (see
    /// [`encode_postings_varint`]).  Each posting list is sorted ascending by
    /// `(doc_id, field_id, position)` before encoding so that each
    /// `delta_doc_id` and `delta_position` is a forward, non-wrapping step
    /// within its `(doc_id, field_id)` run.
    fn serialize_index(
        &self,
        sorted_keys: &[u32],
        avg_doc_length: f32,
        avg_field_lengths: [f32; FIELD_COUNT],
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        // Serialise posting lists using v4 variable-length (delta+varint) codec.
        // Pre-size at VARINT_UPPER_BOUND_PER_ENTRY (= 9, the v3 fixed-entry size)
        // per entry.  This is ~2.5x the measured v4 average of ~3.5 bytes/entry —
        // a deliberate peak-memory/zero-realloc trade-off (see constant comment
        // above).  The strict worst-case is 11 bytes/entry; 9 avoids that extra
        // ~22% while still guaranteeing zero reallocations in practice.
        let estimated_capacity: usize = self
            .postings
            .values()
            .map(|v| v.len() * VARINT_UPPER_BOUND_PER_ENTRY)
            .fold(0usize, usize::saturating_add);
        let mut postings_buf: Vec<u8> = Vec::with_capacity(estimated_capacity);
        let mut entries: Vec<SkidxEntry> = Vec::with_capacity(sorted_keys.len());

        for key in sorted_keys {
            let list = &self.postings[key];
            let offset = postings_buf.len() as u64;
            // Encode this posting list with delta+varint (AD-LXPOST-1, FORMAT_VERSION v4).
            // The list is already sorted by (doc_id, field_id, position) — the caller
            // (build()) calls list.sort_unstable() before reaching here.
            encode_postings_varint(list, &mut postings_buf);
            let byte_len = postings_buf.len() as u64 - offset;
            let length = u32::try_from(byte_len).map_err(|_| {
                SearchError::IndexCorrupted(format!(
                    "posting list for key {key:#010x} exceeds u32::MAX bytes ({byte_len})"
                ))
            })?;
            entries.push(SkidxEntry {
                ngram_key: *key,
                posting_offset: offset,
                posting_length: length,
            });
        }

        // Release the over-allocated capacity before CRC and write.
        // The initial reservation uses VARINT_UPPER_BOUND_PER_ENTRY = 9 bytes/entry
        // (~2.5x the ~3.5 byte v4 average) to guarantee zero reallocations during
        // encoding.  shrink_to_fit reclaims the unused portion so peak RSS during
        // CRC computation and atomic_write reflects the actual encoded size, not
        // the upper-bound estimate.  Build-time only — the buffer is dropped after
        // atomic_write returns.
        postings_buf.shrink_to_fit();

        // Serialise file metadata.
        let mut meta_buf: Vec<u8> = Vec::with_capacity(self.file_meta.len() * FILE_META_SIZE);
        for m in &self.file_meta {
            meta_buf.extend_from_slice(&encode_file_meta(m));
        }

        // Serialise entry array.
        let mut entries_buf: Vec<u8> = Vec::with_capacity(entries.len() * SKIDX_ENTRY_SIZE);
        for e in &entries {
            entries_buf.extend_from_slice(&encode_entry(e));
        }

        // CRC32 over postings + entries + file metadata (#364: integrity guard).
        //
        // v4 posting integrity: the old fixed-stride guard
        // (is_multiple_of(POSTING_ENTRY_SIZE)) was removed because varint byte
        // counts are not a multiple of 9.  Folding postings_buf into the CRC
        // replaces that structural guard with a value-integrity check: a
        // bit-flip inside a posting blob that would otherwise produce wrong-but-
        // bounded (doc_id, position) values and silently mis-rank results is now
        // detected in NgramIndexReader::open before any query can run.
        //
        // Ordering: postings first, then entries, then meta — must match the
        // verification order in NgramIndexReader::open (reader.rs).
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&postings_buf);
        hasher.update(&entries_buf);
        hasher.update(&meta_buf);
        let checksum = hasher.finalize();

        // Build header.
        let ngram_count = u32::try_from(entries.len()).map_err(|_| {
            SearchError::IndexCorrupted(format!("ngram_count {} exceeds u32::MAX", entries.len()))
        })?;
        let header = SkidxHeader {
            magic: *SKIDX_MAGIC,
            version: FORMAT_VERSION,
            ngram_count,
            file_count: self.file_count,
            postings_file_size: postings_buf.len() as u64,
            avg_doc_length,
            avg_field_lengths,
            checksum,
        };

        // Assemble .skidx: header + entries + file_meta.
        let mut skidx_buf =
            Vec::with_capacity(SKIDX_HEADER_SIZE + entries_buf.len() + meta_buf.len());
        skidx_buf.extend_from_slice(&encode_header(&header));
        skidx_buf.extend_from_slice(&entries_buf);
        skidx_buf.extend_from_slice(&meta_buf);

        Ok((postings_buf, skidx_buf))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "builder_tests.rs"]
mod tests;
