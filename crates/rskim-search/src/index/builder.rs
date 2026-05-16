//! [`NgramIndexBuilder`] — constructs the two-file mmap'd n-gram index.
//!
//! # Atomicity contract
//!
//! `.skpost` is written first, then `.skidx`.  A reader that finds `.skidx`
//! present can assume `.skpost` is coherent.  A partial write (power loss
//! between the two) leaves no `.skidx`, so the next open attempt fails
//! cleanly with a "file not found" rather than a corrupt read.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use std::ops::Range;

use super::format::{
    FILE_META_SIZE, FORMAT_VERSION, FileMetaEntry, POSTING_ENTRY_SIZE, PostingEntry,
    SKIDX_ENTRY_SIZE, SKIDX_HEADER_SIZE, SKIDX_MAGIC, SkidxEntry, SkidxHeader, encode_entry,
    encode_file_meta, encode_header, encode_posting, lang_to_id,
};
use super::reader::NgramIndexReader;
use crate::{FIELD_COUNT, FileId, LayerBuilder, Result, SearchError, SearchField, SearchLayer};

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
    /// Accumulated postings: bigram key → list of (doc_id, field_id, position).
    postings: HashMap<u16, Vec<PostingEntry>>,
    /// Per-file metadata in insertion order (indexed by sequential file_index).
    file_meta: Vec<FileMetaEntry>,
    /// Guard against duplicate FileIds.
    seen_file_ids: HashSet<u32>,
    /// Number of files added.
    pub(crate) file_count: u32,
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

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Atomically write `data` to `path` using a temp file in the same directory.
    ///
    /// Creates a named temp file in `dir`, writes all data, then persists (renames)
    /// it to `path`.  On most platforms the rename is atomic, preventing readers
    /// from observing a partial write.
    fn atomic_write(dir: &Path, path: &Path, data: &[u8]) -> Result<()> {
        let mut tmp = NamedTempFile::new_in(dir)?;
        use std::io::Write as _;
        tmp.write_all(data)?;
        tmp.persist(path).map_err(|e| e.error)?;
        Ok(())
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

        // Scan every 2-byte window, resolving the field via a linearly advancing
        // pointer through field_map.  Because positions increase monotonically and
        // field_map is sorted ascending, a single forward scan is O(n + m) instead
        // of the O(n log m) cost of calling binary search once per window.
        let bytes = content.as_bytes();
        let mut range_idx = 0usize;
        for (pos, window) in bytes.windows(2).enumerate() {
            // Advance past any ranges that have ended before `pos`.
            while range_idx < field_map.len() && field_map[range_idx].0.end <= pos {
                range_idx += 1;
            }
            let field_id = if range_idx < field_map.len() && field_map[range_idx].0.contains(&pos)
            {
                field_map[range_idx].1.discriminant()
            } else {
                SearchField::Other.discriminant()
            };
            let key = (u16::from(window[0]) << 8) | u16::from(window[1]);
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
    /// are written atomically via [`Self::atomic_write`].
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Io`] if writing fails, or
    /// [`SearchError::IndexCorrupted`] if the reader cannot open the result.
    fn build(mut self) -> Result<Box<dyn SearchLayer>>
    where
        Self: Sized,
    {
        // Compute corpus averages. Both avg_doc_length and avg_field_lengths share
        // the same divisor, so evaluate it once and default to 0.0 for empty indexes.
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

        // Sort each posting list by (doc_id, field_id, position).
        for list in self.postings.values_mut() {
            list.sort_unstable();
        }

        // Sort ngram keys for the lookup table.
        let mut sorted_keys: Vec<u16> = self.postings.keys().copied().collect();
        sorted_keys.sort_unstable();

        // Serialise posting lists and build the entry table.
        let mut postings_buf: Vec<u8> = Vec::new();
        let mut entries: Vec<SkidxEntry> = Vec::with_capacity(sorted_keys.len());

        for key in &sorted_keys {
            let list = &self.postings[key];
            let offset = postings_buf.len() as u64;
            let byte_len = list.len().checked_mul(POSTING_ENTRY_SIZE).ok_or_else(|| {
                SearchError::IndexCorrupted(format!(
                    "posting list for key {key:#06x} overflows usize"
                ))
            })?;
            let length = u32::try_from(byte_len).map_err(|_| {
                SearchError::IndexCorrupted(format!(
                    "posting list for key {key:#06x} exceeds u32::MAX bytes ({byte_len})"
                ))
            })?;
            for p in list {
                postings_buf.extend_from_slice(&encode_posting(p));
            }
            entries.push(SkidxEntry {
                ngram_key: *key,
                posting_offset: offset,
                posting_length: length,
            });
        }

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

        // CRC32 over entry array + file metadata (contiguous).
        let mut hasher = crc32fast::Hasher::new();
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

        // Assemble .skidx bytes: header + entries + file_meta.
        let mut skidx_buf =
            Vec::with_capacity(SKIDX_HEADER_SIZE + entries_buf.len() + meta_buf.len());
        skidx_buf.extend_from_slice(&encode_header(&header));
        skidx_buf.extend_from_slice(&entries_buf);
        skidx_buf.extend_from_slice(&meta_buf);

        let post_path = self.output_dir.join("index.skpost");
        let idx_path = self.output_dir.join("index.skidx");

        // Atomic writes: .skpost first, .skidx second (commit point).
        Self::atomic_write(&self.output_dir, &post_path, &postings_buf)?;
        Self::atomic_write(&self.output_dir, &idx_path, &skidx_buf)?;

        let reader = NgramIndexReader::open(&self.output_dir)?;
        Ok(Box::new(reader))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "builder_tests.rs"]
mod tests;
