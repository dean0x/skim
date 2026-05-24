//! Pure binary codec for the co-change matrix format (`.skcc`).
//!
//! # File layout
//!
//! ```text
//! [SkccHeader: 18 bytes]
//! [FileCommitEntry × file_count:  8 bytes each, sorted by file_id]
//! [PairEntry       × pair_count: 12 bytes each, sorted by (file_a, file_b)]
//! ```
//!
//! # Encoding
//!
//! All multi-byte integers are little-endian. The header checksum covers the
//! `FileCommitEntry` array bytes concatenated with the `PairEntry` array bytes.
//!
//! # Invariants upheld by this module
//!
//! - **No `std::fs` or `std::io::Write`** — every function operates on `&[u8]`
//!   or returns owned byte arrays. All I/O happens in `builder.rs` / `reader.rs`.
//! - **No `unwrap()` / `expect()` / `panic!()`** outside `#[cfg(test)]`.

use crate::{Result, SearchError};

/// Magic bytes at the start of every `.skcc` file.
pub(crate) const SKCC_MAGIC: &[u8; 4] = b"SKCC";

/// Current on-disk format version. Increment on any breaking change.
pub(crate) const FORMAT_VERSION: u16 = 1;

/// Size in bytes of [`SkccHeader`] on disk.
///
/// Layout: magic (4) + version (2) + pair_count (4) + file_count (4) + checksum (4) = 18 bytes.
pub(crate) const HEADER_SIZE: usize = 18;

/// Size in bytes of a single [`FileCommitEntry`] on disk.
///
/// Layout: file_id (4) + commit_count (4) = 8 bytes.
pub(crate) const FILE_COMMIT_ENTRY_SIZE: usize = 8;

/// Size in bytes of a single [`PairEntry`] on disk.
///
/// Layout: file_a (4) + file_b (4) + count (4) = 12 bytes.
pub(crate) const PAIR_ENTRY_SIZE: usize = 12;

// ============================================================================
// On-disk structs
// ============================================================================

/// Fixed-size header at the start of every `.skcc` file.
///
/// Layout (18 bytes, all integers little-endian):
/// ```text
/// [0..4]   magic       4 bytes
/// [4..6]   version     2 bytes
/// [6..10]  pair_count  4 bytes
/// [10..14] file_count  4 bytes
/// [14..18] checksum    4 bytes (CRC32 of file_commit + pair bytes)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SkccHeader {
    /// Must equal [`SKCC_MAGIC`].
    pub magic: [u8; 4],
    /// Must equal [`FORMAT_VERSION`].
    pub version: u16,
    /// Number of co-change pair entries.
    pub pair_count: u32,
    /// Number of file-commit-count entries.
    pub file_count: u32,
    /// CRC32 of the `FileCommitEntry` array bytes ++ `PairEntry` array bytes.
    pub checksum: u32,
}

/// Per-file commit count entry, sorted by `file_id` ascending.
///
/// Layout (8 bytes, all integers little-endian):
/// ```text
/// [0..4] file_id      4 bytes
/// [4..8] commit_count 4 bytes
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileCommitEntry {
    /// The [`crate::FileId`] inner value.
    pub file_id: u32,
    /// Number of commits in which this file appeared.
    pub commit_count: u32,
}

/// A single co-change pair entry, sorted by `(file_a, file_b)` ascending.
///
/// Invariant: `file_a < file_b` (canonical ordering enforced by builder).
///
/// Layout (12 bytes, all integers little-endian):
/// ```text
/// [0..4]  file_a 4 bytes
/// [4..8]  file_b 4 bytes
/// [8..12] count  4 bytes
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PairEntry {
    /// Lower file ID in the pair.
    pub file_a: u32,
    /// Higher file ID in the pair.
    pub file_b: u32,
    /// Number of commits in which both files appeared together.
    pub count: u32,
}

// ============================================================================
// Private helpers
// ============================================================================

/// Extract a fixed-size byte array from `data[start..start+N]`.
///
/// Returns [`SearchError::IndexCorrupted`] if the range would overflow `usize`
/// or exceeds `data.len()`, rather than panicking.
fn read_array<const N: usize>(data: &[u8], start: usize, context: &'static str) -> Result<[u8; N]> {
    let end = start
        .checked_add(N)
        .ok_or_else(|| SearchError::IndexCorrupted(format!("{context}: offset overflow")))?;
    data.get(start..end)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| {
            SearchError::IndexCorrupted(format!(
                "{context}: need {N} bytes at offset {start}, got {}",
                data.len()
            ))
        })
}

// ============================================================================
// Header encode / decode
// ============================================================================

/// Encode a [`SkccHeader`] into its 18-byte on-disk representation.
pub(crate) fn encode_header(h: &SkccHeader) -> [u8; HEADER_SIZE] {
    let mut buf = [0u8; HEADER_SIZE];
    buf[0..4].copy_from_slice(&h.magic);
    buf[4..6].copy_from_slice(&h.version.to_le_bytes());
    buf[6..10].copy_from_slice(&h.pair_count.to_le_bytes());
    buf[10..14].copy_from_slice(&h.file_count.to_le_bytes());
    buf[14..18].copy_from_slice(&h.checksum.to_le_bytes());
    buf
}

/// Decode a [`SkccHeader`] from a byte slice.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if the slice is too short, the
/// magic bytes do not match, or the version is not [`FORMAT_VERSION`].
pub(crate) fn decode_header(data: &[u8]) -> Result<SkccHeader> {
    if data.len() < HEADER_SIZE {
        return Err(SearchError::IndexCorrupted(format!(
            "header truncated: need {HEADER_SIZE} bytes, got {}",
            data.len()
        )));
    }
    let magic: [u8; 4] = read_array(data, 0, "header: magic")?;
    if &magic != SKCC_MAGIC {
        return Err(SearchError::IndexCorrupted(format!(
            "bad magic: expected {:?}, got {:?}",
            SKCC_MAGIC, magic
        )));
    }
    let version = u16::from_le_bytes(read_array(data, 4, "header: version")?);
    if version != FORMAT_VERSION {
        return Err(SearchError::IndexCorrupted(format!(
            "unsupported format version: {version} (expected {FORMAT_VERSION}); \
             please rebuild the co-change matrix"
        )));
    }
    Ok(SkccHeader {
        magic,
        version,
        pair_count: u32::from_le_bytes(read_array(data, 6, "header: pair_count")?),
        file_count: u32::from_le_bytes(read_array(data, 10, "header: file_count")?),
        checksum: u32::from_le_bytes(read_array(data, 14, "header: checksum")?),
    })
}

// ============================================================================
// FileCommitEntry encode / decode
// ============================================================================

/// Encode a [`FileCommitEntry`] into its 8-byte on-disk representation.
pub(crate) fn encode_file_commit(e: &FileCommitEntry) -> [u8; FILE_COMMIT_ENTRY_SIZE] {
    let mut buf = [0u8; FILE_COMMIT_ENTRY_SIZE];
    buf[0..4].copy_from_slice(&e.file_id.to_le_bytes());
    buf[4..8].copy_from_slice(&e.commit_count.to_le_bytes());
    buf
}

/// Decode a [`FileCommitEntry`] from an 8-byte slice.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if the slice is too short.
pub(crate) fn decode_file_commit(data: &[u8]) -> Result<FileCommitEntry> {
    if data.len() < FILE_COMMIT_ENTRY_SIZE {
        return Err(SearchError::IndexCorrupted(format!(
            "file_commit truncated: need {FILE_COMMIT_ENTRY_SIZE} bytes, got {}",
            data.len()
        )));
    }
    Ok(FileCommitEntry {
        file_id: u32::from_le_bytes(read_array(data, 0, "file_commit: file_id")?),
        commit_count: u32::from_le_bytes(read_array(data, 4, "file_commit: commit_count")?),
    })
}

// ============================================================================
// PairEntry encode / decode
// ============================================================================

/// Encode a [`PairEntry`] into its 12-byte on-disk representation.
pub(crate) fn encode_pair(p: &PairEntry) -> [u8; PAIR_ENTRY_SIZE] {
    let mut buf = [0u8; PAIR_ENTRY_SIZE];
    buf[0..4].copy_from_slice(&p.file_a.to_le_bytes());
    buf[4..8].copy_from_slice(&p.file_b.to_le_bytes());
    buf[8..12].copy_from_slice(&p.count.to_le_bytes());
    buf
}

/// Decode a [`PairEntry`] from a 12-byte slice.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if the slice is too short.
pub(crate) fn decode_pair(data: &[u8]) -> Result<PairEntry> {
    if data.len() < PAIR_ENTRY_SIZE {
        return Err(SearchError::IndexCorrupted(format!(
            "pair_entry truncated: need {PAIR_ENTRY_SIZE} bytes, got {}",
            data.len()
        )));
    }
    Ok(PairEntry {
        file_a: u32::from_le_bytes(read_array(data, 0, "pair_entry: file_a")?),
        file_b: u32::from_le_bytes(read_array(data, 4, "pair_entry: file_b")?),
        count: u32::from_le_bytes(read_array(data, 8, "pair_entry: count")?),
    })
}

// ============================================================================
// Binary search over sorted pair entries
// ============================================================================

/// Binary-search `pairs_data` for the pair `(file_a, file_b)`.
///
/// `pairs_data` must be a byte slice whose length is a multiple of
/// [`PAIR_ENTRY_SIZE`] and whose entries are sorted ascending by
/// `(file_a, file_b)`.
///
/// Returns `Ok(Some(count))` if found, `Ok(None)` if absent, or
/// [`SearchError::IndexCorrupted`] if the slice is malformed.
pub(crate) fn lookup_pair(pairs_data: &[u8], file_a: u32, file_b: u32) -> Result<Option<u32>> {
    if !pairs_data.len().is_multiple_of(PAIR_ENTRY_SIZE) {
        return Err(SearchError::IndexCorrupted(format!(
            "pairs_data length {} is not a multiple of PAIR_ENTRY_SIZE {}; likely corrupt",
            pairs_data.len(),
            PAIR_ENTRY_SIZE
        )));
    }
    let n = pairs_data.len() / PAIR_ENTRY_SIZE;
    let mut lo = 0usize;
    let mut hi = n;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let offset = mid * PAIR_ENTRY_SIZE;
        let entry = decode_pair(&pairs_data[offset..offset + PAIR_ENTRY_SIZE])?;
        match (entry.file_a, entry.file_b).cmp(&(file_a, file_b)) {
            std::cmp::Ordering::Equal => return Ok(Some(entry.count)),
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
        }
    }
    Ok(None)
}

// ============================================================================
// CRC32 checksum
// ============================================================================

/// Compute the CRC32 checksum of `data`.
///
/// CRC32 detects accidental corruption (bit flips, truncation) but is NOT a
/// cryptographic integrity check — it does not protect against intentional
/// tampering. This is acceptable because `.skcc` files are derived caches
/// rebuildable from git history.
///
/// The header checksum covers the `FileCommitEntry` array bytes concatenated
/// with the `PairEntry` array bytes.
pub(crate) fn compute_checksum(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
