//! Append-only delta segment for incremental index updates.

use std::{
    fs::{File, OpenOptions},
    io::{self, BufWriter, Write},
    path::Path,
};

use memmap2::Mmap;

use crate::SearchError;

use super::super::{Ngram, PostingEntry, POSTING_ENTRY_SIZE};

// ============================================================================
// DeltaWriter / DeltaReader — append-only incremental segment
// ============================================================================

/// Record size for delta file: 8 bytes ngram_hash + 12 bytes PostingEntry.
const DELTA_RECORD_SIZE: usize = 8 + POSTING_ENTRY_SIZE;

const DELTA_FILE: &str = "lexical.delta";

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

        // SAFETY: The delta file is opened read-only and is only written via
        // `DeltaWriter::append`, which uses `BufWriter` and never truncates.
        // Concurrent rebuilds use atomic rename on the main index files; the
        // delta file itself is append-only, so any data visible at `mmap` time
        // remains valid for the lifetime of this mapping. Incomplete records
        // at the tail are handled by `scan` via integer division on record_count.
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
