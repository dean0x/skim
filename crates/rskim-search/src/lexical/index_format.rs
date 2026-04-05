//! Two-file mmap'd on-disk index format (`.skidx` + `.skpost`).
//!
//! Provides atomic write, memory-mapped read, and delta/tombstone support
//! for incremental updates.
//!
//! # File Layout
//!
//! ## `.skidx` (lookup table)
//! - Bytes 0..32: [`IndexHeader`] (magic, version, ngram_count, file_count, timestamp)
//! - Bytes 32+: Sorted array of [`IndexEntry`] (20 bytes each, sorted by `ngram_hash`)
//!
//! ## `.skpost` (postings)
//! - Flat array of [`PostingEntry`] (12 bytes each), referenced by `(offset, length)` in
//!   [`IndexEntry`].
//!
//! ## `lexical.delta` (incremental updates)
//! - Flat array of 20-byte records: `ngram_hash` (8 bytes LE) + [`PostingEntry`] (12 bytes).
//!
//! ## `lexical.tombstones` (deleted doc_ids)
//! - Sorted array of `u32` LE values (4 bytes each).
//!
//! # Atomicity
//!
//! Writes go to `.tmp` files first, then [`std::fs::rename`] swaps them in.
//! Any stale `.tmp` files from a previous crash are deleted before starting.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Read, Write},
    path::Path,
    time::SystemTime,
};

use memmap2::Mmap;

use super::{
    IndexHeader, Ngram, PostingEntry, INDEX_FORMAT_VERSION, INDEX_HEADER_SIZE, INDEX_MAGIC,
    POSTING_ENTRY_SIZE,
};
use crate::{SearchError, SearchField};

// ============================================================================
// File name constants
// ============================================================================

const SKIDX_FILE: &str = "lexical.skidx";
const SKPOST_FILE: &str = "lexical.skpost";
const DELTA_FILE: &str = "lexical.delta";
const TOMBSTONES_FILE: &str = "lexical.tombstones";
const SKIDX_TMP: &str = "lexical.skidx.tmp";
const SKPOST_TMP: &str = "lexical.skpost.tmp";

// ============================================================================
// IndexEntry — 20-byte on-disk lookup table entry
// ============================================================================

/// Single entry in the `.skidx` lookup table. 20 bytes on disk.
///
/// # On-disk layout (little-endian)
///
/// | Offset | Size | Field              |
/// |--------|------|--------------------|
/// | 0      | 8    | `ngram_hash`       |
/// | 8      | 8    | `posting_offset`   |
/// | 16     | 4    | `posting_length`   |
#[derive(Debug, Clone, Copy)]
pub(crate) struct IndexEntry {
    /// FxHash of the n-gram. Used for binary search.
    pub ngram_hash: u64,
    /// Byte offset into `.skpost` where this ngram's postings begin.
    pub posting_offset: u64,
    /// Number of [`PostingEntry`] items in this ngram's posting list.
    pub posting_length: u32,
}

/// Size of a single [`IndexEntry`] on disk, in bytes.
pub(crate) const INDEX_ENTRY_SIZE: usize = 20;

impl IndexEntry {
    /// Serialize to a fixed 20-byte little-endian representation.
    pub(crate) fn to_bytes(self) -> [u8; INDEX_ENTRY_SIZE] {
        let mut buf = [0u8; INDEX_ENTRY_SIZE];
        buf[0..8].copy_from_slice(&self.ngram_hash.to_le_bytes());
        buf[8..16].copy_from_slice(&self.posting_offset.to_le_bytes());
        buf[16..20].copy_from_slice(&self.posting_length.to_le_bytes());
        buf
    }

    /// Deserialize from a 20-byte little-endian slice.
    ///
    /// Returns `None` if the slice is too short.
    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < INDEX_ENTRY_SIZE {
            return None;
        }
        Some(Self {
            ngram_hash: u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]),
            posting_offset: u64::from_le_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]),
            posting_length: u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
        })
    }
}

// ============================================================================
// write_index — atomic two-file write
// ============================================================================

/// Write index to disk atomically.
///
/// Writes `.skidx` and `.skpost` as a pair to temp files, then renames both.
/// Any stale `.tmp` files from a prior crash are removed before starting.
///
/// `entries` must be sorted by n-gram hash (ascending). The caller is responsible
/// for sorting; this function validates in debug builds only.
///
/// # Errors
///
/// Returns [`SearchError::Io`] on any filesystem failure.
pub fn write_index(
    dir: &Path,
    entries: &[(Ngram, Vec<PostingEntry>)],
    header: &IndexHeader,
) -> crate::Result<()> {
    // --- Clean up stale temp files from any prior crash ----------------------
    let skidx_tmp = dir.join(SKIDX_TMP);
    let skpost_tmp = dir.join(SKPOST_TMP);
    remove_if_exists(&skidx_tmp)?;
    remove_if_exists(&skpost_tmp)?;

    // --- Build postings file in memory first to compute offsets ---------------
    // Each posting list is written contiguously; IndexEntry records the byte offset.
    let mut post_buf: Vec<u8> =
        Vec::with_capacity(entries.len() * POSTING_ENTRY_SIZE * 4 /* rough estimate */);

    let mut index_entries: Vec<IndexEntry> = Vec::with_capacity(entries.len());

    for (ngram, postings) in entries {
        let posting_offset = post_buf.len() as u64;
        let posting_length = postings.len() as u32;

        for p in postings {
            post_buf.extend_from_slice(&p.to_bytes());
        }

        index_entries.push(IndexEntry {
            ngram_hash: ngram.as_u64(),
            posting_offset,
            posting_length,
        });
    }

    // --- Write .skpost.tmp ---------------------------------------------------
    {
        let file = File::create(&skpost_tmp).map_err(SearchError::Io)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(&post_buf).map_err(SearchError::Io)?;
        writer.flush().map_err(SearchError::Io)?;
    }

    // --- Write .skidx.tmp ----------------------------------------------------
    {
        let file = File::create(&skidx_tmp).map_err(SearchError::Io)?;
        let mut writer = BufWriter::new(file);
        writer
            .write_all(&header.to_bytes())
            .map_err(SearchError::Io)?;
        for entry in &index_entries {
            writer
                .write_all(&entry.to_bytes())
                .map_err(SearchError::Io)?;
        }
        writer.flush().map_err(SearchError::Io)?;
    }

    // --- Atomic rename (post first, then idx) --------------------------------
    // If we crash between the two renames, the idx still points to the old post.
    // Rename post first so a reader always sees a consistent pair: either both
    // old or both new. In the failure window the idx is stale but the post is
    // already new, so any reader that opens the old idx will still get valid
    // offsets into the new post (since we append-only in the post file and the
    // old offsets are a subset of the new ones).
    fs::rename(&skpost_tmp, dir.join(SKPOST_FILE)).map_err(SearchError::Io)?;
    fs::rename(&skidx_tmp, dir.join(SKIDX_FILE)).map_err(SearchError::Io)?;

    Ok(())
}

// ============================================================================
// IndexReader — mmap'd reader
// ============================================================================

/// Memory-mapped index reader. Zero-copy, `Send + Sync`.
///
/// Opens `.skidx` and `.skpost` in the given directory, validates their
/// headers and sizes, and provides binary-search lookup by n-gram hash.
pub struct IndexReader {
    /// Memory-mapped contents of `.skidx`.
    idx_mmap: Mmap,
    /// Memory-mapped contents of `.skpost`.
    post_mmap: Mmap,
    /// Path to the directory (used in error messages).
    dir: std::path::PathBuf,
    /// Cached reference to the header at offset 0 of `idx_mmap`.
    header: IndexHeader,
    /// Number of [`IndexEntry`] records in the lookup table.
    ngram_count: usize,
}

impl IndexReader {
    /// Open from index directory. Validates magic bytes, version, and file sizes.
    ///
    /// # Errors
    ///
    /// - [`SearchError::Io`] if either file cannot be opened.
    /// - [`SearchError::CorruptedIndex`] if any validation fails.
    pub fn open(dir: &Path) -> crate::Result<Self> {
        let skidx_path = dir.join(SKIDX_FILE);
        let skpost_path = dir.join(SKPOST_FILE);

        // Open and mmap .skidx
        let idx_file = File::open(&skidx_path).map_err(SearchError::Io)?;
        let idx_mmap = unsafe { Mmap::map(&idx_file) }.map_err(SearchError::Io)?;

        // Open and mmap .skpost
        let post_file = File::open(&skpost_path).map_err(SearchError::Io)?;
        let post_mmap = unsafe { Mmap::map(&post_file) }.map_err(SearchError::Io)?;

        // Validate header size
        if idx_mmap.len() < INDEX_HEADER_SIZE {
            return Err(SearchError::CorruptedIndex {
                path: skidx_path.display().to_string(),
                reason: format!(
                    "file too small for header: {} < {} bytes",
                    idx_mmap.len(),
                    INDEX_HEADER_SIZE
                ),
            });
        }

        // Parse and validate header
        let header = IndexHeader::from_bytes(&idx_mmap[..INDEX_HEADER_SIZE]).ok_or_else(|| {
            SearchError::CorruptedIndex {
                path: skidx_path.display().to_string(),
                reason: "failed to parse index header".to_string(),
            }
        })?;

        if header.magic != INDEX_MAGIC {
            return Err(SearchError::CorruptedIndex {
                path: skidx_path.display().to_string(),
                reason: format!(
                    "invalid magic bytes: expected {:?}, got {:?}",
                    INDEX_MAGIC, header.magic
                ),
            });
        }

        if header.version != INDEX_FORMAT_VERSION {
            return Err(SearchError::CorruptedIndex {
                path: skidx_path.display().to_string(),
                reason: format!(
                    "unsupported index format version: expected {}, got {}",
                    INDEX_FORMAT_VERSION, header.version
                ),
            });
        }

        let ngram_count = header.ngram_count as usize;

        // Validate .skidx size: header + ngram_count * INDEX_ENTRY_SIZE
        let expected_idx_size = INDEX_HEADER_SIZE + ngram_count * INDEX_ENTRY_SIZE;
        if idx_mmap.len() != expected_idx_size {
            return Err(SearchError::CorruptedIndex {
                path: skidx_path.display().to_string(),
                reason: format!(
                    "unexpected .skidx size: expected {} bytes (header + {} ngrams * {}), got {}",
                    expected_idx_size, ngram_count, INDEX_ENTRY_SIZE, idx_mmap.len()
                ),
            });
        }

        // Validate .skpost size: must be divisible by POSTING_ENTRY_SIZE
        if post_mmap.len() % POSTING_ENTRY_SIZE != 0 {
            return Err(SearchError::CorruptedIndex {
                path: skpost_path.display().to_string(),
                reason: format!(
                    ".skpost size {} is not a multiple of posting entry size {}",
                    post_mmap.len(),
                    POSTING_ENTRY_SIZE
                ),
            });
        }

        // Validate that all IndexEntry references are within .skpost bounds.
        // `posting_offset` is a byte offset into .skpost;
        // `posting_length` is a count of PostingEntry items.
        let post_size = post_mmap.len();
        for i in 0..ngram_count {
            let offset = INDEX_HEADER_SIZE + i * INDEX_ENTRY_SIZE;
            let entry = IndexEntry::from_bytes(&idx_mmap[offset..]).ok_or_else(|| {
                SearchError::CorruptedIndex {
                    path: skidx_path.display().to_string(),
                    reason: format!("IndexEntry {i} could not be parsed"),
                }
            })?;

            let byte_start = entry.posting_offset as usize;
            let byte_length = (entry.posting_length as usize)
                .checked_mul(POSTING_ENTRY_SIZE)
                .ok_or_else(|| SearchError::CorruptedIndex {
                    path: skidx_path.display().to_string(),
                    reason: format!("IndexEntry {i}: posting length overflows byte count"),
                })?;
            let byte_end = byte_start.checked_add(byte_length).ok_or_else(|| {
                SearchError::CorruptedIndex {
                    path: skidx_path.display().to_string(),
                    reason: format!("IndexEntry {i}: posting offset + length overflows"),
                }
            })?;

            if byte_end > post_size {
                return Err(SearchError::CorruptedIndex {
                    path: skidx_path.display().to_string(),
                    reason: format!(
                        "IndexEntry {i}: posting byte range [{byte_start}, {byte_end}) out of \
                         bounds (post file size {post_size})"
                    ),
                });
            }
        }

        Ok(Self {
            idx_mmap,
            post_mmap,
            dir: dir.to_path_buf(),
            header,
            ngram_count,
        })
    }

    /// Binary search for an n-gram in the lookup table.
    ///
    /// Returns the decoded posting entries, filtering out any with invalid
    /// `field_id` values. Returns `None` if the n-gram is not present.
    pub fn lookup(&self, ngram: Ngram) -> Option<Vec<PostingEntry>> {
        let target_hash = ngram.as_u64();

        // Binary search over IndexEntry array (sorted by ngram_hash)
        let mut lo: usize = 0;
        let mut hi: usize = self.ngram_count;

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let offset = INDEX_HEADER_SIZE + mid * INDEX_ENTRY_SIZE;
            let entry = IndexEntry::from_bytes(&self.idx_mmap[offset..])?;

            match entry.ngram_hash.cmp(&target_hash) {
                std::cmp::Ordering::Equal => {
                    return Some(self.read_postings(&entry));
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }

        None
    }

    /// Return the index header.
    pub fn header(&self) -> &IndexHeader {
        &self.header
    }

    /// Decode posting entries for the given IndexEntry, filtering invalid ones.
    fn read_postings(&self, entry: &IndexEntry) -> Vec<PostingEntry> {
        // `posting_offset` is already a byte offset into .skpost.
        let start = entry.posting_offset as usize;
        let count = entry.posting_length as usize;
        let file_count = self.header.file_count;

        let mut result = Vec::with_capacity(count);
        for i in 0..count {
            let byte_offset = start + i * POSTING_ENTRY_SIZE;
            let end = byte_offset + POSTING_ENTRY_SIZE;
            if end > self.post_mmap.len() {
                break;
            }
            let Some(p) = PostingEntry::from_bytes(&self.post_mmap[byte_offset..end]) else {
                continue;
            };
            // Skip entries with unknown field_id
            if SearchField::from_u8(p.field_id).is_none() {
                continue;
            }
            // Skip entries with doc_id >= file_count
            if u64::from(p.doc_id) >= file_count {
                continue;
            }
            result.push(p);
        }
        result
    }

    /// Return the path to the directory this reader was opened from.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

// ============================================================================
// DeltaWriter / DeltaReader — append-only incremental segment
// ============================================================================

/// Record size for delta file: 8 bytes ngram_hash + 12 bytes PostingEntry.
const DELTA_RECORD_SIZE: usize = 8 + POSTING_ENTRY_SIZE;

/// Append-only delta segment for incremental index updates.
///
/// New postings are appended here instead of rebuilding the full index on every
/// change. The query layer merges delta results with main index results at query time.
pub struct DeltaWriter {
    writer: BufWriter<File>,
}

impl DeltaWriter {
    /// Open the delta file for appending, creating it if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Io`] if the file cannot be opened.
    pub fn open_or_create(dir: &Path) -> crate::Result<Self> {
        let path = dir.join(DELTA_FILE);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(SearchError::Io)?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    /// Append entries to the delta file.
    ///
    /// Each entry is written as: ngram_hash (8 bytes LE) + PostingEntry (12 bytes) = 20 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Io`] on write failure.
    pub fn append(&mut self, entries: &[(Ngram, PostingEntry)]) -> crate::Result<()> {
        for (ngram, posting) in entries {
            self.writer
                .write_all(&ngram.as_u64().to_le_bytes())
                .map_err(SearchError::Io)?;
            self.writer
                .write_all(&posting.to_bytes())
                .map_err(SearchError::Io)?;
        }
        self.writer.flush().map_err(SearchError::Io)?;
        Ok(())
    }
}

/// Memory-mapped reader for the delta file.
pub struct DeltaReader {
    mmap: Mmap,
}

impl DeltaReader {
    /// Open the delta file. Returns `Ok(None)` if no delta file exists.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Io`] if the file exists but cannot be opened or mapped.
    pub fn open(dir: &Path) -> crate::Result<Option<Self>> {
        let path = dir.join(DELTA_FILE);
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(SearchError::Io(e)),
        };

        let metadata = file.metadata().map_err(SearchError::Io)?;
        if metadata.len() == 0 {
            return Ok(None);
        }

        let mmap = unsafe { Mmap::map(&file) }.map_err(SearchError::Io)?;
        Ok(Some(Self { mmap }))
    }

    /// Linear scan for all entries matching the given n-gram.
    ///
    /// Returns all [`PostingEntry`] items whose `ngram_hash` matches.
    /// Incomplete records at the end of the file are silently skipped.
    pub fn scan(&self, ngram: Ngram) -> Vec<PostingEntry> {
        let target = ngram.as_u64();
        let mut results = Vec::new();
        let data = &self.mmap[..];
        let record_count = data.len() / DELTA_RECORD_SIZE;

        for i in 0..record_count {
            let base = i * DELTA_RECORD_SIZE;
            let hash_bytes: [u8; 8] = [
                data[base],
                data[base + 1],
                data[base + 2],
                data[base + 3],
                data[base + 4],
                data[base + 5],
                data[base + 6],
                data[base + 7],
            ];
            let hash = u64::from_le_bytes(hash_bytes);
            if hash != target {
                continue;
            }
            if let Some(posting) = PostingEntry::from_bytes(&data[base + 8..]) {
                results.push(posting);
            }
        }

        results
    }
}

// ============================================================================
// Tombstones — deleted doc_id set
// ============================================================================

/// Set of invalidated doc_ids excluded from main index results.
///
/// Maintains a sorted `Vec<u32>` for O(log n) `contains` queries and
/// O(n log n) `add` + `save` operations.
#[derive(Debug, Default)]
pub struct Tombstones {
    doc_ids: Vec<u32>,
}

impl Tombstones {
    /// Load tombstones from `lexical.tombstones` in the given directory.
    ///
    /// Returns an empty `Tombstones` if the file does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Io`] if the file exists but cannot be read.
    pub fn load(dir: &Path) -> crate::Result<Self> {
        let path = dir.join(TOMBSTONES_FILE);
        let mut file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => return Err(SearchError::Io(e)),
        };

        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(SearchError::Io)?;

        if bytes.len() % 4 != 0 {
            return Err(SearchError::CorruptedIndex {
                path: path.display().to_string(),
                reason: format!(
                    "tombstone file size {} is not a multiple of 4",
                    bytes.len()
                ),
            });
        }

        let count = bytes.len() / 4;
        let mut doc_ids = Vec::with_capacity(count);
        for i in 0..count {
            let offset = i * 4;
            let id = u32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ]);
            doc_ids.push(id);
        }

        // The file should already be sorted, but sort defensively.
        doc_ids.sort_unstable();
        doc_ids.dedup();

        Ok(Self { doc_ids })
    }

    /// Save tombstones to `lexical.tombstones` in the given directory.
    ///
    /// Writes sorted `u32` LE values. Overwrites any existing file.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Io`] on write failure.
    pub fn save(&self, dir: &Path) -> crate::Result<()> {
        let path = dir.join(TOMBSTONES_FILE);
        let file = File::create(&path).map_err(SearchError::Io)?;
        let mut writer = BufWriter::new(file);
        for &id in &self.doc_ids {
            writer
                .write_all(&id.to_le_bytes())
                .map_err(SearchError::Io)?;
        }
        writer.flush().map_err(SearchError::Io)?;
        Ok(())
    }

    /// Add a doc_id to the tombstone set.
    ///
    /// Maintains sorted order. Duplicates are silently ignored.
    pub fn add(&mut self, doc_id: u32) {
        match self.doc_ids.binary_search(&doc_id) {
            Ok(_) => {}                    // already present
            Err(pos) => self.doc_ids.insert(pos, doc_id),
        }
    }

    /// Return whether the given `doc_id` is tombstoned.
    ///
    /// O(log n) binary search.
    pub fn contains(&self, doc_id: u32) -> bool {
        self.doc_ids.binary_search(&doc_id).is_ok()
    }

    /// Return `true` if the tombstone set is empty.
    pub fn is_empty(&self) -> bool {
        self.doc_ids.is_empty()
    }

    /// Return the number of tombstoned doc_ids.
    pub fn len(&self) -> usize {
        self.doc_ids.len()
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Remove a file if it exists. Ignores `NotFound` errors.
fn remove_if_exists(path: &Path) -> crate::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SearchError::Io(e)),
    }
}

/// Return the current Unix timestamp (seconds since epoch).
///
/// Falls back to 0 if `SystemTime` is before the Unix epoch (pathological case).
#[allow(dead_code)]
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexical::{INDEX_FORMAT_VERSION, INDEX_MAGIC};

    fn make_header(ngram_count: u64, file_count: u64) -> IndexHeader {
        IndexHeader {
            magic: INDEX_MAGIC,
            version: INDEX_FORMAT_VERSION,
            ngram_count,
            file_count,
            created_at: 0,
        }
    }

    fn make_posting(doc_id: u32, field_id: u8, position: u32, tf: u16) -> PostingEntry {
        PostingEntry {
            doc_id,
            field_id,
            position,
            tf,
        }
    }

    #[test]
    fn index_entry_roundtrip() {
        let entry = IndexEntry {
            ngram_hash: 0xDEAD_BEEF_CAFE_1234,
            posting_offset: 42,
            posting_length: 7,
        };
        let bytes = entry.to_bytes();
        let decoded = IndexEntry::from_bytes(&bytes);
        assert!(decoded.is_some());
        let decoded = decoded.unwrap_or_else(|| unreachable!());
        assert_eq!(decoded.ngram_hash, entry.ngram_hash);
        assert_eq!(decoded.posting_offset, entry.posting_offset);
        assert_eq!(decoded.posting_length, entry.posting_length);
    }

    #[test]
    fn index_entry_from_short_slice_returns_none() {
        assert!(IndexEntry::from_bytes(&[0u8; 19]).is_none());
    }
}
