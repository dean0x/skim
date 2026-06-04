//! Pure binary codec for the two-file mmap'd AST n-gram index format.
//!
//! # File layout
//!
//! ## `ast_index.skidx`
//!
//! ```text
//! [AstSkidxHeader: 48 bytes]
//! [AstBigramEntry × bigram_count: 16 bytes each, sorted by key asc]
//! [AstTrigramEntry × trigram_count: 20 bytes each, sorted by key asc]
//! [AstFileMetaEntry × file_count: 5 bytes each]
//! ```
//!
//! ## `ast_index.skpost`
//!
//! ```text
//! [AstPostingEntry ... concatenated posting lists]
//! (all bigram lists, then all trigram lists; offsets in entry tables
//! disambiguate)
//! ```
//!
//! # Encoding
//!
//! All multi-byte integers are little-endian. The CRC32 checksum covers
//! the single contiguous post-header payload slice in `.skidx` (bigram
//! entries + trigram entries + file-meta entries, in that serialization
//! order).
//!
//! # Invariants upheld by this module
//!
//! - **No `std::fs` or `std::io::Write`** — every function operates on `&[u8]`
//!   or returns owned byte arrays. All I/O happens in `builder.rs` / `reader.rs`.
//! - **No `unwrap()` / `expect()` / `panic!()`** outside `#[cfg(test)]`.

pub(crate) use crate::index::lang_map::lang_to_id;
// lang_from_id is used in tests and by readers to recover Language from lang_id
#[allow(unused_imports)]
pub(crate) use crate::index::lang_map::lang_from_id;
use crate::{Result, SearchError};

// ============================================================================
// Format constants
// ============================================================================

/// Magic bytes at the start of every `ast_index.skidx` file.
///
/// Distinct from the lexical index magic `b"SKIX"` so that opening an
/// AST index dir containing only lexical files fails cleanly with Io/NotFound.
pub(crate) const AST_SKIDX_MAGIC: &[u8; 4] = b"SKAX";

/// Current on-disk format version.  Increment on ANY layout change.
///
/// Version-bump policy: any change to field order, field width, or interpretation
/// of any byte in any struct increments this number.  Old versions are rejected
/// with an error message containing "format version".
pub(crate) const AST_FORMAT_VERSION: u16 = 1;

/// Size in bytes of [`AstSkidxHeader`] on disk.
pub(crate) const AST_HEADER_SIZE: usize = 48;

/// Size in bytes of a single [`AstBigramEntry`] on disk.
pub(crate) const AST_BIGRAM_ENTRY_SIZE: usize = 16;

/// Size in bytes of a single [`AstTrigramEntry`] on disk.
pub(crate) const AST_TRIGRAM_ENTRY_SIZE: usize = 20;

/// Size in bytes of a single [`AstPostingEntry`] on disk.
pub(crate) const AST_POSTING_ENTRY_SIZE: usize = 8;

/// Size in bytes of a single [`AstFileMetaEntry`] on disk.
pub(crate) const AST_FILE_META_SIZE: usize = 5;

// ============================================================================
// On-disk structs
// ============================================================================

/// Fixed-size header at the start of every `ast_index.skidx` file.
///
/// Layout (48 bytes, all integers little-endian):
/// ```text
/// [0..4]   magic b"SKAX"         4 bytes
/// [4..6]   version = 1           2 bytes
/// [6..10]  bigram_count          4 bytes (u32)
/// [10..14] trigram_count         4 bytes (u32)
/// [14..18] file_count            4 bytes (u32)
/// [18..26] postings_file_size    8 bytes (u64)
/// [26..30] avg_bigram_count      4 bytes (f32 LE)
/// [30..34] avg_trigram_count     4 bytes (f32 LE)
/// [34..38] avg_node_count        4 bytes (f32 LE)
/// [38..44] reserved (= 0)        6 bytes
/// [44..48] checksum (CRC32)      4 bytes (u32)
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AstSkidxHeader {
    /// Must equal [`AST_SKIDX_MAGIC`].
    pub magic: [u8; 4],
    /// Must equal [`AST_FORMAT_VERSION`].
    pub version: u16,
    /// Number of distinct AST bigram entries in the lookup table.
    pub bigram_count: u32,
    /// Number of distinct AST trigram entries in the lookup table.
    pub trigram_count: u32,
    /// Number of files in the index.
    pub file_count: u32,
    /// Total byte size of the companion `ast_index.skpost` file.
    pub postings_file_size: u64,
    /// Average per-file distinct bigram count across all indexed files.
    ///
    /// Useful for IDF normalisation at query time (Wave 3f).
    pub avg_bigram_count: f32,
    /// Average per-file distinct trigram count across all indexed files.
    pub avg_trigram_count: f32,
    /// Average emitted-node count per file (nodes.len(), excludes ERROR/MISSING).
    pub avg_node_count: f32,
    /// CRC32 of the post-header payload (bigram entries + trigram entries +
    /// file-meta entries, in that order).
    pub checksum: u32,
}

/// One entry in the sorted bigram lookup table.
///
/// Layout (16 bytes, all integers little-endian):
/// ```text
/// [0..4]   key (u32)             4 bytes
/// [4..12]  posting_offset (u64)  8 bytes
/// [12..16] posting_length (u32)  4 bytes
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AstBigramEntry {
    /// Encoded bigram key (`AstBigram::key()`).
    pub key: u32,
    /// Byte offset into `ast_index.skpost` where this bigram's posting list begins.
    pub posting_offset: u64,
    /// Byte length of this bigram's posting list in `ast_index.skpost`.
    pub posting_length: u32,
}

/// One entry in the sorted trigram lookup table.
///
/// Layout (20 bytes, all integers little-endian):
/// ```text
/// [0..8]   key (u64)             8 bytes
/// [8..16]  posting_offset (u64)  8 bytes
/// [16..20] posting_length (u32)  4 bytes
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AstTrigramEntry {
    /// Encoded trigram key (`AstTrigram::key()`).
    pub key: u64,
    /// Byte offset into `ast_index.skpost` where this trigram's posting list begins.
    pub posting_offset: u64,
    /// Byte length of this trigram's posting list in `ast_index.skpost`.
    pub posting_length: u32,
}

/// One element in a posting list inside `ast_index.skpost`.
///
/// Layout (8 bytes, all integers little-endian):
/// ```text
/// [0..4] doc_id  4 bytes (u32)
/// [4..8] count   4 bytes (u32, per-file structural term-frequency, >= 1)
/// ```
///
/// `count` is taken directly from `AstBigramEntry.count` / `AstTrigramEntry.count`
/// (the structural term-frequency produced by `extract_ast_ngrams`).
/// IDF weight is discarded at build time and recomputed at query time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AstPostingEntry {
    /// The document (file) this posting belongs to.
    pub doc_id: u32,
    /// Per-file structural term-frequency (>= 1).
    pub count: u32,
}

/// Per-file metadata stored in the tail of `ast_index.skidx`.
///
/// Layout (5 bytes, all integers little-endian):
/// ```text
/// [0]    lang_id    1 byte (u8, from lang_to_id)
/// [1..5] node_count 4 bytes (u32, emitted-node count from linearize_source)
/// ```
///
/// `node_count` equals `lin.nodes.len()` (emitted nodes, excludes ERROR/MISSING).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AstFileMetaEntry {
    /// Language ID from [`lang_to_id`].
    pub lang_id: u8,
    /// Number of emitted AST nodes from `linearize_source` (excludes ERROR/MISSING).
    pub node_count: u32,
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

/// Encode an [`AstSkidxHeader`] into its 48-byte on-disk representation.
pub(crate) fn encode_header(h: &AstSkidxHeader) -> [u8; AST_HEADER_SIZE] {
    let mut buf = [0u8; AST_HEADER_SIZE];
    buf[0..4].copy_from_slice(&h.magic);
    buf[4..6].copy_from_slice(&h.version.to_le_bytes());
    buf[6..10].copy_from_slice(&h.bigram_count.to_le_bytes());
    buf[10..14].copy_from_slice(&h.trigram_count.to_le_bytes());
    buf[14..18].copy_from_slice(&h.file_count.to_le_bytes());
    buf[18..26].copy_from_slice(&h.postings_file_size.to_le_bytes());
    buf[26..30].copy_from_slice(&h.avg_bigram_count.to_le_bytes());
    buf[30..34].copy_from_slice(&h.avg_trigram_count.to_le_bytes());
    buf[34..38].copy_from_slice(&h.avg_node_count.to_le_bytes());
    // [38..44] reserved — already zeroed by `[0u8; AST_HEADER_SIZE]`
    buf[44..48].copy_from_slice(&h.checksum.to_le_bytes());
    buf
}

/// Decode an [`AstSkidxHeader`] from a byte slice.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if:
/// - The slice is shorter than [`AST_HEADER_SIZE`].
/// - Magic bytes do not match [`AST_SKIDX_MAGIC`] (message contains "bad magic").
/// - Version does not equal [`AST_FORMAT_VERSION`] (message contains "format version").
/// - Any of the three `avg_*` f32 fields is non-finite or < 0.0.
pub(crate) fn decode_header(data: &[u8]) -> Result<AstSkidxHeader> {
    if data.len() < AST_HEADER_SIZE {
        return Err(SearchError::IndexCorrupted(format!(
            "header truncated: need {AST_HEADER_SIZE} bytes, got {}",
            data.len()
        )));
    }
    let magic: [u8; 4] = read_array(data, 0, "header: magic")?;
    if &magic != AST_SKIDX_MAGIC {
        return Err(SearchError::IndexCorrupted(format!(
            "bad magic: expected {:?}, got {:?}",
            AST_SKIDX_MAGIC, magic
        )));
    }
    let version = u16::from_le_bytes(read_array(data, 4, "header: version")?);
    if version != AST_FORMAT_VERSION {
        return Err(SearchError::IndexCorrupted(format!(
            "unsupported format version: {version} (expected {AST_FORMAT_VERSION}); \
             please rebuild the AST index"
        )));
    }

    let avg_bigram_count = f32::from_le_bytes(read_array(data, 26, "header: avg_bigram_count")?);
    if !avg_bigram_count.is_finite() || avg_bigram_count < 0.0 {
        return Err(SearchError::IndexCorrupted(format!(
            "header: avg_bigram_count must be finite and >= 0.0, got {avg_bigram_count}"
        )));
    }

    let avg_trigram_count = f32::from_le_bytes(read_array(data, 30, "header: avg_trigram_count")?);
    if !avg_trigram_count.is_finite() || avg_trigram_count < 0.0 {
        return Err(SearchError::IndexCorrupted(format!(
            "header: avg_trigram_count must be finite and >= 0.0, got {avg_trigram_count}"
        )));
    }

    let avg_node_count = f32::from_le_bytes(read_array(data, 34, "header: avg_node_count")?);
    if !avg_node_count.is_finite() || avg_node_count < 0.0 {
        return Err(SearchError::IndexCorrupted(format!(
            "header: avg_node_count must be finite and >= 0.0, got {avg_node_count}"
        )));
    }

    Ok(AstSkidxHeader {
        magic,
        version,
        bigram_count: u32::from_le_bytes(read_array(data, 6, "header: bigram_count")?),
        trigram_count: u32::from_le_bytes(read_array(data, 10, "header: trigram_count")?),
        file_count: u32::from_le_bytes(read_array(data, 14, "header: file_count")?),
        postings_file_size: u64::from_le_bytes(read_array(data, 18, "header: postings_file_size")?),
        avg_bigram_count,
        avg_trigram_count,
        avg_node_count,
        checksum: u32::from_le_bytes(read_array(data, 44, "header: checksum")?),
    })
}

// ============================================================================
// AstBigramEntry encode / decode
// ============================================================================

/// Encode an [`AstBigramEntry`] into its 16-byte on-disk representation.
pub(crate) fn encode_bigram_entry(e: &AstBigramEntry) -> [u8; AST_BIGRAM_ENTRY_SIZE] {
    let mut buf = [0u8; AST_BIGRAM_ENTRY_SIZE];
    buf[0..4].copy_from_slice(&e.key.to_le_bytes());
    buf[4..12].copy_from_slice(&e.posting_offset.to_le_bytes());
    buf[12..16].copy_from_slice(&e.posting_length.to_le_bytes());
    buf
}

/// Decode an [`AstBigramEntry`] from a 16-byte slice.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if the slice is too short.
pub(crate) fn decode_bigram_entry(data: &[u8]) -> Result<AstBigramEntry> {
    if data.len() < AST_BIGRAM_ENTRY_SIZE {
        return Err(SearchError::IndexCorrupted(format!(
            "bigram_entry truncated: need {AST_BIGRAM_ENTRY_SIZE} bytes, got {}",
            data.len()
        )));
    }
    Ok(AstBigramEntry {
        key: u32::from_le_bytes(read_array(data, 0, "bigram_entry: key")?),
        posting_offset: u64::from_le_bytes(read_array(data, 4, "bigram_entry: posting_offset")?),
        posting_length: u32::from_le_bytes(read_array(data, 12, "bigram_entry: posting_length")?),
    })
}

// ============================================================================
// AstTrigramEntry encode / decode
// ============================================================================

/// Encode an [`AstTrigramEntry`] into its 20-byte on-disk representation.
pub(crate) fn encode_trigram_entry(e: &AstTrigramEntry) -> [u8; AST_TRIGRAM_ENTRY_SIZE] {
    let mut buf = [0u8; AST_TRIGRAM_ENTRY_SIZE];
    buf[0..8].copy_from_slice(&e.key.to_le_bytes());
    buf[8..16].copy_from_slice(&e.posting_offset.to_le_bytes());
    buf[16..20].copy_from_slice(&e.posting_length.to_le_bytes());
    buf
}

/// Decode an [`AstTrigramEntry`] from a 20-byte slice.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if the slice is too short.
pub(crate) fn decode_trigram_entry(data: &[u8]) -> Result<AstTrigramEntry> {
    if data.len() < AST_TRIGRAM_ENTRY_SIZE {
        return Err(SearchError::IndexCorrupted(format!(
            "trigram_entry truncated: need {AST_TRIGRAM_ENTRY_SIZE} bytes, got {}",
            data.len()
        )));
    }
    Ok(AstTrigramEntry {
        key: u64::from_le_bytes(read_array(data, 0, "trigram_entry: key")?),
        posting_offset: u64::from_le_bytes(read_array(data, 8, "trigram_entry: posting_offset")?),
        posting_length: u32::from_le_bytes(read_array(data, 16, "trigram_entry: posting_length")?),
    })
}

// ============================================================================
// AstPostingEntry encode / decode
// ============================================================================

/// Encode an [`AstPostingEntry`] into its 8-byte on-disk representation.
pub(crate) fn encode_posting(p: &AstPostingEntry) -> [u8; AST_POSTING_ENTRY_SIZE] {
    let mut buf = [0u8; AST_POSTING_ENTRY_SIZE];
    buf[0..4].copy_from_slice(&p.doc_id.to_le_bytes());
    buf[4..8].copy_from_slice(&p.count.to_le_bytes());
    buf
}

/// Decode an [`AstPostingEntry`] from an 8-byte slice.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if the slice is too short or
/// `count == 0` (invariant: every posting has count >= 1).
pub(crate) fn decode_posting(data: &[u8]) -> Result<AstPostingEntry> {
    if data.len() < AST_POSTING_ENTRY_SIZE {
        return Err(SearchError::IndexCorrupted(format!(
            "ast_posting truncated: need {AST_POSTING_ENTRY_SIZE} bytes, got {}",
            data.len()
        )));
    }
    let doc_id = u32::from_le_bytes(read_array(data, 0, "ast_posting: doc_id")?);
    let count = u32::from_le_bytes(read_array(data, 4, "ast_posting: count")?);
    if count == 0 {
        return Err(SearchError::IndexCorrupted(format!(
            "ast_posting: count must be >= 1 for doc_id {doc_id}"
        )));
    }
    Ok(AstPostingEntry { doc_id, count })
}

// ============================================================================
// AstFileMetaEntry encode / decode
// ============================================================================

/// Encode an [`AstFileMetaEntry`] into its 5-byte on-disk representation.
pub(crate) fn encode_file_meta(m: &AstFileMetaEntry) -> [u8; AST_FILE_META_SIZE] {
    let mut buf = [0u8; AST_FILE_META_SIZE];
    buf[0] = m.lang_id;
    buf[1..5].copy_from_slice(&m.node_count.to_le_bytes());
    buf
}

/// Decode an [`AstFileMetaEntry`] from a 5-byte slice.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if the slice is too short.
pub(crate) fn decode_file_meta(data: &[u8]) -> Result<AstFileMetaEntry> {
    if data.len() < AST_FILE_META_SIZE {
        return Err(SearchError::IndexCorrupted(format!(
            "ast_file_meta truncated: need {AST_FILE_META_SIZE} bytes, got {}",
            data.len()
        )));
    }
    Ok(AstFileMetaEntry {
        lang_id: data[0],
        node_count: u32::from_le_bytes(read_array(data, 1, "ast_file_meta: node_count")?),
    })
}

// ============================================================================
// CRC32 checksum
// ============================================================================

/// Compute the CRC32 checksum of `data`.
///
/// The header checksum covers the single contiguous post-header payload:
/// `idx_mmap[AST_HEADER_SIZE..expected_idx_size]`, which includes bigram
/// entries, trigram entries, and file-meta entries in serialization order.
pub(crate) fn compute_checksum(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

// ============================================================================
// Binary search helpers
// ============================================================================

/// Binary-search `entries_data` for an [`AstBigramEntry`] with the given `key`.
///
/// `entries_data` must be a byte slice whose length is a multiple of
/// [`AST_BIGRAM_ENTRY_SIZE`] and whose entries are sorted ascending by `key`.
///
/// Returns `Ok(Some(entry))` if found, `Ok(None)` if absent, or
/// [`SearchError::IndexCorrupted`] if the slice length is not a multiple of
/// the entry size.
pub(crate) fn lookup_bigram(entries_data: &[u8], key: u32) -> Result<Option<AstBigramEntry>> {
    binary_search_entries(
        entries_data,
        AST_BIGRAM_ENTRY_SIZE,
        |data, off| {
            let raw: [u8; 4] = data[off..off + 4]
                .try_into()
                .map_err(|_| SearchError::IndexCorrupted("bigram key read error".into()))?;
            Ok(u64::from(u32::from_le_bytes(raw)))
        },
        u64::from(key),
        |data, off| decode_bigram_entry(&data[off..off + AST_BIGRAM_ENTRY_SIZE]).map(Some),
    )
}

/// Binary-search `entries_data` for an [`AstTrigramEntry`] with the given `key`.
///
/// `entries_data` must be a byte slice whose length is a multiple of
/// [`AST_TRIGRAM_ENTRY_SIZE`] and whose entries are sorted ascending by `key`.
///
/// Returns `Ok(Some(entry))` if found, `Ok(None)` if absent, or
/// [`SearchError::IndexCorrupted`] if the slice length is not a multiple of
/// the entry size.
pub(crate) fn lookup_trigram(entries_data: &[u8], key: u64) -> Result<Option<AstTrigramEntry>> {
    binary_search_entries(
        entries_data,
        AST_TRIGRAM_ENTRY_SIZE,
        |data, off| {
            let raw: [u8; 8] = data[off..off + 8]
                .try_into()
                .map_err(|_| SearchError::IndexCorrupted("trigram key read error".into()))?;
            Ok(u64::from_le_bytes(raw))
        },
        key,
        |data, off| decode_trigram_entry(&data[off..off + AST_TRIGRAM_ENTRY_SIZE]).map(Some),
    )
}

/// Generic binary search over a sorted flat byte array of fixed-size entries.
///
/// Parameters:
/// - `data` — the byte slice to search (length must be a multiple of `stride`).
/// - `stride` — byte size of one entry.
/// - `read_key` — pure function: `(data, entry_offset) -> Result<u64>`.
///   Keys are widened to `u64` for comparison; u32 keys are zero-extended.
/// - `target` — the key to search for (as u64).
/// - `decode_found` — called when an equal key is found; returns `Ok(Some(T))`.
fn binary_search_entries<T>(
    data: &[u8],
    stride: usize,
    read_key: impl Fn(&[u8], usize) -> Result<u64>,
    target: u64,
    decode_found: impl Fn(&[u8], usize) -> Result<Option<T>>,
) -> Result<Option<T>> {
    if !data.len().is_multiple_of(stride) {
        return Err(SearchError::IndexCorrupted(format!(
            "entries_data length {} is not a multiple of stride {}",
            data.len(),
            stride
        )));
    }
    let n = data.len() / stride;
    let mut lo = 0usize;
    let mut hi = n;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let offset = mid * stride;
        let key = read_key(data, offset)?;
        match key.cmp(&target) {
            std::cmp::Ordering::Equal => return decode_found(data, offset),
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
        }
    }
    Ok(None)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
