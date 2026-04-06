//! Memory-mapped index reader with binary-search lookup.

use std::{fs::File, path::Path};

use memmap2::Mmap;

use crate::{SearchError, SearchField};

use super::{
    super::{
        IndexHeader, Ngram, PostingEntry, INDEX_FORMAT_VERSION, INDEX_HEADER_SIZE, INDEX_MAGIC,
        POSTING_ENTRY_SIZE,
    },
    entry::{IndexEntry, INDEX_ENTRY_SIZE},
    writer::{SKIDX_FILE, SKPOST_FILE},
};

// ============================================================================
// Constants
// ============================================================================

/// Maximum `.skpost` file size in bytes (4 GiB).
///
/// At 12 bytes per `PostingEntry` this allows ~357 million records, well
/// beyond any real-world repository. Exceeding this limit indicates either
/// corruption or a crafted index designed to trigger a large allocation.
const MAX_SKPOST_BYTES: usize = 4_000_000_000;

// ============================================================================
// IndexReader — mmap'd reader
// ============================================================================

/// Memory-mapped index reader. Zero-copy, `Send + Sync`.
///
/// Opens `.skidx` and `.skpost` in the given directory, validates their
/// headers and sizes, and provides binary-search lookup by n-gram hash.
///
/// Per-entry bounds validation is deferred to [`IndexReader::validate`] so
/// that opening the reader at query time does not pay O(ngram_count) startup
/// cost. Header-level size checks in [`IndexReader::open`] already guarantee
/// structural integrity; `read_postings` enforces bounds on every access.
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
    /// Per-entry bounds validation is not performed here — call
    /// [`IndexReader::validate`] explicitly if you need a full integrity check
    /// (e.g., during `--rebuild`). Bounds checking is still enforced per-access
    /// in [`IndexReader::lookup`] via `read_postings`.
    ///
    /// # Errors
    ///
    /// - [`SearchError::Io`] if either file cannot be opened.
    /// - [`SearchError::CorruptedIndex`] if any validation fails.
    #[must_use = "the opened IndexReader must be used; dropping it immediately is a bug"]
    pub fn open(dir: &Path) -> crate::Result<Self> {
        let skidx_path = dir.join(SKIDX_FILE);
        let skpost_path = dir.join(SKPOST_FILE);

        // Local helpers to build CorruptedIndex errors without repeating the path conversion.
        let idx_corrupted = |reason: String| SearchError::CorruptedIndex {
            path: skidx_path.display().to_string(),
            reason,
        };
        let post_corrupted = |reason: String| SearchError::CorruptedIndex {
            path: skpost_path.display().to_string(),
            reason,
        };

        // Open and mmap .skidx.
        //
        // SAFETY: The file is opened read-only. Writes always go to a `.tmp`
        // sibling and are committed via atomic `rename(2)`, so the mapped region
        // always corresponds to a complete, consistent file. Concurrent
        // `--rebuild` operations use the same rename strategy, meaning any
        // reader holding this mmap sees the old (valid) file until it is
        // dropped; the kernel keeps the inode alive until the last mapping is
        // released.
        let idx_file = File::open(&skidx_path).map_err(SearchError::Io)?;
        let idx_mmap = unsafe { Mmap::map(&idx_file) }.map_err(SearchError::Io)?;

        // Open and mmap .skpost.
        //
        // SAFETY: Same invariants as idx_mmap above — read-only, atomically
        // written via rename, valid for the lifetime of this mapping.
        let post_file = File::open(&skpost_path).map_err(SearchError::Io)?;
        let post_mmap = unsafe { Mmap::map(&post_file) }.map_err(SearchError::Io)?;

        // Validate header size
        if idx_mmap.len() < INDEX_HEADER_SIZE {
            return Err(idx_corrupted(format!(
                "file too small for header: {} < {} bytes",
                idx_mmap.len(),
                INDEX_HEADER_SIZE
            )));
        }

        // Parse and validate header
        let header = IndexHeader::from_bytes(&idx_mmap[..INDEX_HEADER_SIZE])
            .ok_or_else(|| idx_corrupted("failed to parse index header".to_string()))?;

        if header.magic != INDEX_MAGIC {
            return Err(idx_corrupted(format!(
                "invalid magic bytes: expected {:?}, got {:?}",
                INDEX_MAGIC, header.magic
            )));
        }

        if header.version != INDEX_FORMAT_VERSION {
            return Err(idx_corrupted(format!(
                "unsupported index format version: expected {}, got {}",
                INDEX_FORMAT_VERSION, header.version
            )));
        }

        // Reject ngram_count values that cannot be represented as usize (e.g. on
        // 32-bit targets) or that would cause arithmetic overflow below.
        let ngram_count = usize::try_from(header.ngram_count).map_err(|_| {
            idx_corrupted(format!(
                "ngram_count {} exceeds platform usize::MAX",
                header.ngram_count
            ))
        })?;

        // Validate .skidx size: header + ngram_count * INDEX_ENTRY_SIZE
        let expected_idx_size = ngram_count
            .checked_mul(INDEX_ENTRY_SIZE)
            .and_then(|n| n.checked_add(INDEX_HEADER_SIZE))
            .ok_or_else(|| {
                idx_corrupted(format!("ngram_count {} causes size overflow", ngram_count))
            })?;
        if idx_mmap.len() != expected_idx_size {
            return Err(idx_corrupted(format!(
                "unexpected .skidx size: expected {} bytes (header + {} ngrams * {}), got {}",
                expected_idx_size,
                ngram_count,
                INDEX_ENTRY_SIZE,
                idx_mmap.len()
            )));
        }

        // Validate .skpost size: must be below the hard cap and divisible by POSTING_ENTRY_SIZE.
        if post_mmap.len() > MAX_SKPOST_BYTES {
            return Err(post_corrupted(format!(
                ".skpost size {} exceeds maximum allowed size {} bytes",
                post_mmap.len(),
                MAX_SKPOST_BYTES
            )));
        }
        if post_mmap.len() % POSTING_ENTRY_SIZE != 0 {
            return Err(post_corrupted(format!(
                ".skpost size {} is not a multiple of posting entry size {}",
                post_mmap.len(),
                POSTING_ENTRY_SIZE
            )));
        }

        Ok(Self {
            idx_mmap,
            post_mmap,
            dir: dir.to_path_buf(),
            header,
            ngram_count,
        })
    }

    /// Perform a full per-entry integrity check of all [`IndexEntry`] bounds.
    ///
    /// This is O(ngram_count) and intended for use during `--rebuild` or
    /// explicit index validation, not normal query startup. Call after
    /// [`IndexReader::open`] when you need a complete consistency guarantee.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::CorruptedIndex`] if any entry references an
    /// out-of-bounds range in the `.skpost` file.
    pub fn validate(&self) -> crate::Result<()> {
        let skidx_path = self.dir.join(SKIDX_FILE);
        let idx_corrupted = |reason: String| SearchError::CorruptedIndex {
            path: skidx_path.display().to_string(),
            reason,
        };

        let post_size = self.post_mmap.len();
        for i in 0..self.ngram_count {
            let offset = INDEX_HEADER_SIZE + i * INDEX_ENTRY_SIZE;
            let entry = IndexEntry::from_bytes(&self.idx_mmap[offset..])
                .ok_or_else(|| idx_corrupted(format!("IndexEntry {i} could not be parsed")))?;

            let byte_start = entry.posting_offset as usize;
            let byte_length = (entry.posting_length as usize)
                .checked_mul(POSTING_ENTRY_SIZE)
                .ok_or_else(|| {
                    idx_corrupted(format!(
                        "IndexEntry {i}: posting length overflows byte count"
                    ))
                })?;
            let byte_end = byte_start.checked_add(byte_length).ok_or_else(|| {
                idx_corrupted(format!("IndexEntry {i}: posting offset + length overflows"))
            })?;

            if byte_end > post_size {
                return Err(idx_corrupted(format!(
                    "IndexEntry {i}: posting byte range [{byte_start}, {byte_end}) out of \
                     bounds (post file size {post_size})"
                )));
            }
        }

        Ok(())
    }

    /// Binary search for an n-gram in the lookup table.
    ///
    /// Returns the decoded posting entries, filtering out any with invalid
    /// `field_id` values. Returns `None` if the n-gram is not present.
    #[must_use = "returns the posting list for scoring; ignoring it silently skips matching documents"]
    pub fn lookup(&self, ngram: Ngram) -> Option<Vec<PostingEntry>> {
        let mut buf = Vec::new();
        if self.lookup_into(ngram, &mut buf) {
            Some(buf)
        } else {
            None
        }
    }

    /// Binary search for an n-gram, writing results into a caller-owned buffer.
    ///
    /// Clears `buf` before use and fills it with decoded posting entries. The
    /// caller can reuse the same buffer across multiple lookups to avoid
    /// per-call allocation. Returns `true` if the n-gram was found.
    #[must_use = "ignoring the found/not-found signal silently skips scoring for this n-gram"]
    pub fn lookup_into(&self, ngram: Ngram, buf: &mut Vec<PostingEntry>) -> bool {
        buf.clear();

        let target_hash = ngram.as_u64();

        // Binary search over IndexEntry array (sorted by ngram_hash)
        let mut lo: usize = 0;
        let mut hi: usize = self.ngram_count;

        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let offset = INDEX_HEADER_SIZE + mid * INDEX_ENTRY_SIZE;
            let Some(entry) = IndexEntry::from_bytes(&self.idx_mmap[offset..]) else {
                buf.clear();
                return false;
            };

            match entry.ngram_hash.cmp(&target_hash) {
                std::cmp::Ordering::Equal => {
                    self.read_postings_into(&entry, buf);
                    return true;
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }

        false
    }

    /// Return the index header.
    pub fn header(&self) -> &IndexHeader {
        &self.header
    }

    /// Decode posting entries into a caller-owned buffer, filtering invalid ones.
    fn read_postings_into(&self, entry: &IndexEntry, buf: &mut Vec<PostingEntry>) {
        buf.clear();

        // `posting_offset` is already a byte offset into .skpost.
        let start = entry.posting_offset as usize;
        // Cap the capacity hint against the maximum number of entries that could
        // physically exist in the mapped file. An untrusted index could claim
        // posting_length = u32::MAX, which would attempt a ~51 GiB allocation
        // before any bounds check fires. Capping here prevents that.
        let max_possible = self.post_mmap.len() / POSTING_ENTRY_SIZE;
        let count = (entry.posting_length as usize).min(max_possible);
        let file_count = self.header.file_count;

        buf.reserve(count);
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
            buf.push(p);
        }
    }

    /// Return the path to the directory this reader was opened from.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}
