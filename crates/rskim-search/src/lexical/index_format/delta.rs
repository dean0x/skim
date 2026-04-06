//! Append-only delta segment for incremental index updates.

use std::{
    fs::{File, OpenOptions},
    io::{self, BufWriter, Write},
    path::Path,
};

use memmap2::Mmap;
use rustc_hash::FxHashMap;

use crate::SearchError;

use super::super::{Ngram, PostingEntry, POSTING_ENTRY_SIZE};

// ============================================================================
// DeltaWriter / DeltaReader — append-only incremental segment
// ============================================================================

/// Record size for delta file: 8 bytes ngram_hash + 12 bytes PostingEntry.
const DELTA_RECORD_SIZE: usize = 8 + POSTING_ENTRY_SIZE;

const DELTA_FILE: &str = "lexical.delta";

/// Maximum delta file size in bytes (100 MB).
///
/// At 20 bytes per record this allows up to 5 million incremental postings.
/// A legitimate delta file beyond this size indicates either corruption or a
/// runaway writer that should have triggered a full rebuild instead.
const MAX_DELTA_BYTES: u64 = 100_000_000;

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
    #[must_use = "the opened DeltaWriter must be used to append entries"]
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

/// Memory-mapped reader for the delta file with an in-memory hash index.
///
/// The `index` field maps each `ngram_hash` to the byte offsets of its matching
/// records, built once at open time. This makes [`DeltaReader::scan`] O(1) per
/// query n-gram instead of O(n) over the full mmap.
pub struct DeltaReader {
    mmap: Mmap,
    /// ngram_hash → list of record start byte offsets into `mmap`.
    index: FxHashMap<u64, Vec<usize>>,
}

impl DeltaReader {
    /// Open the delta file. Returns `Ok(None)` if no delta file exists.
    ///
    /// Builds an in-memory hash index over all complete records so that
    /// [`scan`] is O(1) rather than O(n).
    ///
    /// # Errors
    ///
    /// - [`SearchError::Io`] if the file exists but cannot be opened or mapped.
    /// - [`SearchError::CorruptedIndex`] if the file exceeds [`MAX_DELTA_BYTES`].
    #[must_use = "the opened DeltaReader must be used to scan entries"]
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

        if metadata.len() > MAX_DELTA_BYTES {
            return Err(SearchError::CorruptedIndex {
                path: path.display().to_string(),
                reason: format!(
                    "delta file is {} bytes, exceeds maximum of {} bytes",
                    metadata.len(),
                    MAX_DELTA_BYTES
                ),
            });
        }

        // SAFETY: The delta file is opened read-only and is only written via
        // `DeltaWriter::append`, which uses `BufWriter` and never truncates.
        // Concurrent rebuilds use atomic rename on the main index files; the
        // delta file itself is append-only, so any data visible at `mmap` time
        // remains valid for the lifetime of this mapping. Incomplete records
        // at the tail are handled below via integer division on record_count.
        let mmap = unsafe { Mmap::map(&file) }.map_err(SearchError::Io)?;

        // Build the hash index in a single pass over all complete records.
        let data = &mmap[..];
        let record_count = data.len() / DELTA_RECORD_SIZE;
        let mut index: FxHashMap<u64, Vec<usize>> =
            FxHashMap::with_capacity_and_hasher(record_count, Default::default());

        for i in 0..record_count {
            let base = i * DELTA_RECORD_SIZE;
            let hash = u64::from_le_bytes([
                data[base],
                data[base + 1],
                data[base + 2],
                data[base + 3],
                data[base + 4],
                data[base + 5],
                data[base + 6],
                data[base + 7],
            ]);
            index.entry(hash).or_default().push(base);
        }

        Ok(Some(Self { mmap, index }))
    }

    /// Look up all postings matching the given n-gram.
    ///
    /// Uses the in-memory hash index built at open time for O(1) dispatch.
    /// Returns all [`PostingEntry`] items whose `ngram_hash` matches.
    pub fn scan(&self, ngram: Ngram) -> Vec<PostingEntry> {
        let mut buf = Vec::new();
        self.scan_into(ngram, &mut buf);
        buf
    }

    /// Look up all postings matching the given n-gram into a caller-owned buffer.
    ///
    /// Clears `buf` before use and fills it with decoded posting entries. The
    /// caller can reuse the same buffer across multiple scans to avoid
    /// per-call allocation.
    pub fn scan_into(&self, ngram: Ngram, buf: &mut Vec<PostingEntry>) {
        buf.clear();

        let Some(offsets) = self.index.get(&ngram.as_u64()) else {
            return;
        };
        let data = &self.mmap[..];
        buf.reserve(offsets.len());
        for &base in offsets {
            if let Some(posting) = PostingEntry::from_bytes(&data[base + 8..]) {
                buf.push(posting);
            }
        }
    }
}
