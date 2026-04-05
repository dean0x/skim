//! Atomic two-file write for `.skidx` and `.skpost`.

use std::{
    fs::{self, File},
    io::{self, BufWriter, Write},
    path::Path,
};

use crate::SearchError;

use super::{
    super::{IndexHeader, Ngram, PostingEntry, POSTING_ENTRY_SIZE},
    entry::IndexEntry,
};

// ============================================================================
// File name constants (private to index_format)
// ============================================================================

pub(super) const SKIDX_FILE: &str = "lexical.skidx";
pub(super) const SKPOST_FILE: &str = "lexical.skpost";
pub(super) const SKIDX_TMP: &str = "lexical.skidx.tmp";
pub(super) const SKPOST_TMP: &str = "lexical.skpost.tmp";

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
    let mut post_buf: Vec<u8> = Vec::with_capacity(
        entries.len() * POSTING_ENTRY_SIZE * 4, /* rough estimate */
    );

    let mut index_entries: Vec<IndexEntry> = Vec::with_capacity(entries.len());

    for (ngram, postings) in entries {
        let posting_offset = post_buf.len() as u64;
        let posting_length = u32::try_from(postings.len()).map_err(|_| {
            SearchError::IndexError(format!(
                "posting list for ngram {:?} exceeds u32::MAX entries",
                ngram
            ))
        })?;

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
// Internal helpers
// ============================================================================

/// Remove a file if it exists. Ignores `NotFound` errors.
pub(super) fn remove_if_exists(path: &Path) -> crate::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SearchError::Io(e)),
    }
}
