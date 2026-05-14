//! Pure codec for the two-file mmap'd n-gram index format.
//!
//! # File layout
//!
//! ## `index.skidx`
//!
//! ```text
//! [SkidxHeader: 30 bytes]
//! [SkidxEntry × ngram_count: 14 bytes each]
//! [FileMetaEntry × file_count: 5 bytes each]
//! ```
//!
//! ## `index.skpost`
//!
//! ```text
//! [PostingEntry ... concatenated posting lists]
//! ```
//!
//! # Encoding
//!
//! All multi-byte integers are little-endian.  The header checksum covers
//! the entry array and file-metadata array bytes (appended in that order).
//!
//! # Invariants upheld by this module
//!
//! - **No `std::fs` or `std::io::Write`** — every function operates on `&[u8]`
//!   or returns owned byte arrays.  All I/O happens in `builder.rs`/`reader.rs`.
//! - **No `unwrap()` / `expect()` / `panic!()`** outside `#[cfg(test)]`.

use crate::{
    SearchError, SearchField,
    weights::{BIGRAM_WEIGHTS, lookup_weight},
};

// ============================================================================
// Format constants
// ============================================================================

/// Magic bytes at the start of every `.skidx` file.
pub(crate) const SKIDX_MAGIC: &[u8; 4] = b"SKIX";

/// Current on-disk format version.  Increment on any breaking change.
pub(crate) const FORMAT_VERSION: u16 = 1;

/// Size in bytes of [`SkidxHeader`] on disk.
pub(crate) const SKIDX_HEADER_SIZE: usize = 30;

/// Size in bytes of a single [`SkidxEntry`] on disk.
pub(crate) const SKIDX_ENTRY_SIZE: usize = 14;

/// Size in bytes of a single [`PostingEntry`] on disk.
pub(crate) const POSTING_ENTRY_SIZE: usize = 9;

/// Size in bytes of a single [`FileMetaEntry`] on disk.
pub(crate) const FILE_META_SIZE: usize = 5;

// ============================================================================
// BM25 parameters
// ============================================================================

/// BM25 term-frequency saturation parameter.
pub(crate) const BM25_K1: f32 = 1.2;

/// BM25 document-length normalisation parameter.
pub(crate) const BM25_B: f32 = 0.75;

// ============================================================================
// On-disk structs
// ============================================================================

/// Fixed-size header at the start of every `.skidx` file.
///
/// Layout (30 bytes, all integers little-endian):
/// ```text
/// [0..4]   magic         4 bytes
/// [4..6]   version       2 bytes
/// [6..10]  ngram_count   4 bytes
/// [10..14] file_count    4 bytes
/// [14..22] postings_file_size  8 bytes
/// [22..26] avg_doc_length  4 bytes (f32 LE)
/// [26..30] checksum      4 bytes (CRC32)
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SkidxHeader {
    /// Must equal [`SKIDX_MAGIC`].
    pub magic: [u8; 4],
    /// Must equal [`FORMAT_VERSION`].
    pub version: u16,
    /// Number of distinct n-gram entries in the lookup table.
    pub ngram_count: u32,
    /// Number of files in the index.
    pub file_count: u32,
    /// Total byte size of the companion `.skpost` file.
    pub postings_file_size: u64,
    /// Average document byte length, used for BM25 normalisation.
    pub avg_doc_length: f32,
    /// CRC32 of the entry array + file-metadata array bytes.
    pub checksum: u32,
}

/// One entry in the sorted n-gram lookup table.
///
/// Layout (14 bytes, all integers little-endian):
/// ```text
/// [0..2]  ngram_key       2 bytes
/// [2..10] posting_offset  8 bytes
/// [10..14] posting_length 4 bytes (number of bytes, not entries)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SkidxEntry {
    /// The bigram key (`(b1 << 8) | b2`).
    pub ngram_key: u16,
    /// Byte offset into `.skpost` where this n-gram's posting list begins.
    pub posting_offset: u64,
    /// Byte length of this n-gram's posting list in `.skpost`.
    pub posting_length: u32,
}

/// One element in a posting list inside `.skpost`.
///
/// Layout (9 bytes, all integers little-endian):
/// ```text
/// [0..4] doc_id    4 bytes
/// [4]    field_id  1 byte
/// [5..9] position  4 bytes
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PostingEntry {
    /// The document (file) this posting belongs to.
    pub doc_id: u32,
    /// [`SearchField`] discriminant — validated during decode.
    pub field_id: u8,
    /// Byte position within the document.
    pub position: u32,
}

/// Per-file metadata stored in the tail of `.skidx`.
///
/// Layout (5 bytes, all integers little-endian):
/// ```text
/// [0]    lang_id     1 byte
/// [1..5] doc_length  4 bytes
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileMetaEntry {
    /// Language ID from [`lang_to_id`].
    pub lang_id: u8,
    /// Byte length of the original document.
    pub doc_length: u32,
}

// ============================================================================
// Header encode / decode
// ============================================================================

/// Encode a [`SkidxHeader`] into its 30-byte on-disk representation.
pub(crate) fn encode_header(h: &SkidxHeader) -> [u8; SKIDX_HEADER_SIZE] {
    let mut buf = [0u8; SKIDX_HEADER_SIZE];
    buf[0..4].copy_from_slice(&h.magic);
    buf[4..6].copy_from_slice(&h.version.to_le_bytes());
    buf[6..10].copy_from_slice(&h.ngram_count.to_le_bytes());
    buf[10..14].copy_from_slice(&h.file_count.to_le_bytes());
    buf[14..22].copy_from_slice(&h.postings_file_size.to_le_bytes());
    buf[22..26].copy_from_slice(&h.avg_doc_length.to_le_bytes());
    buf[26..30].copy_from_slice(&h.checksum.to_le_bytes());
    buf
}

/// Decode a [`SkidxHeader`] from a byte slice.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if the slice is too short,
/// the magic bytes do not match, or the version is unsupported.
pub(crate) fn decode_header(data: &[u8]) -> crate::Result<SkidxHeader> {
    if data.len() < SKIDX_HEADER_SIZE {
        return Err(SearchError::IndexCorrupted(format!(
            "header truncated: need {SKIDX_HEADER_SIZE} bytes, got {}",
            data.len()
        )));
    }
    let magic: [u8; 4] = data[0..4].try_into().map_err(|_| {
        SearchError::IndexCorrupted("header: magic slice conversion failed".to_string())
    })?;
    if &magic != SKIDX_MAGIC {
        return Err(SearchError::IndexCorrupted(format!(
            "bad magic: expected {:?}, got {:?}",
            SKIDX_MAGIC, magic
        )));
    }
    let version = u16::from_le_bytes(data[4..6].try_into().map_err(|_| {
        SearchError::IndexCorrupted("header: version slice conversion failed".to_string())
    })?);
    if version != FORMAT_VERSION {
        return Err(SearchError::IndexCorrupted(format!(
            "unsupported format version: {version} (expected {FORMAT_VERSION})"
        )));
    }
    let ngram_count = u32::from_le_bytes(data[6..10].try_into().map_err(|_| {
        SearchError::IndexCorrupted("header: ngram_count slice conversion failed".to_string())
    })?);
    let file_count = u32::from_le_bytes(data[10..14].try_into().map_err(|_| {
        SearchError::IndexCorrupted("header: file_count slice conversion failed".to_string())
    })?);
    let postings_file_size = u64::from_le_bytes(data[14..22].try_into().map_err(|_| {
        SearchError::IndexCorrupted(
            "header: postings_file_size slice conversion failed".to_string(),
        )
    })?);
    let avg_doc_length = f32::from_le_bytes(data[22..26].try_into().map_err(|_| {
        SearchError::IndexCorrupted("header: avg_doc_length slice conversion failed".to_string())
    })?);
    let checksum = u32::from_le_bytes(data[26..30].try_into().map_err(|_| {
        SearchError::IndexCorrupted("header: checksum slice conversion failed".to_string())
    })?);
    Ok(SkidxHeader {
        magic,
        version,
        ngram_count,
        file_count,
        postings_file_size,
        avg_doc_length,
        checksum,
    })
}

// ============================================================================
// Entry encode / decode
// ============================================================================

/// Encode a [`SkidxEntry`] into its 14-byte on-disk representation.
pub(crate) fn encode_entry(e: &SkidxEntry) -> [u8; SKIDX_ENTRY_SIZE] {
    let mut buf = [0u8; SKIDX_ENTRY_SIZE];
    buf[0..2].copy_from_slice(&e.ngram_key.to_le_bytes());
    buf[2..10].copy_from_slice(&e.posting_offset.to_le_bytes());
    buf[10..14].copy_from_slice(&e.posting_length.to_le_bytes());
    buf
}

/// Decode a [`SkidxEntry`] from a 14-byte slice.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if the slice is too short.
pub(crate) fn decode_entry(data: &[u8]) -> crate::Result<SkidxEntry> {
    if data.len() < SKIDX_ENTRY_SIZE {
        return Err(SearchError::IndexCorrupted(format!(
            "entry truncated: need {SKIDX_ENTRY_SIZE} bytes, got {}",
            data.len()
        )));
    }
    let ngram_key = u16::from_le_bytes(data[0..2].try_into().map_err(|_| {
        SearchError::IndexCorrupted("entry: ngram_key slice conversion failed".to_string())
    })?);
    let posting_offset = u64::from_le_bytes(data[2..10].try_into().map_err(|_| {
        SearchError::IndexCorrupted("entry: posting_offset slice conversion failed".to_string())
    })?);
    let posting_length = u32::from_le_bytes(data[10..14].try_into().map_err(|_| {
        SearchError::IndexCorrupted("entry: posting_length slice conversion failed".to_string())
    })?);
    Ok(SkidxEntry {
        ngram_key,
        posting_offset,
        posting_length,
    })
}

// ============================================================================
// Posting encode / decode
// ============================================================================

/// Encode a [`PostingEntry`] into its 9-byte on-disk representation.
pub(crate) fn encode_posting(p: &PostingEntry) -> [u8; POSTING_ENTRY_SIZE] {
    let mut buf = [0u8; POSTING_ENTRY_SIZE];
    buf[0..4].copy_from_slice(&p.doc_id.to_le_bytes());
    buf[4] = p.field_id;
    buf[5..9].copy_from_slice(&p.position.to_le_bytes());
    buf
}

/// Decode a [`PostingEntry`] from a 9-byte slice.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if the slice is too short or
/// `field_id` is not a valid [`SearchField`] discriminant.
pub(crate) fn decode_posting(data: &[u8]) -> crate::Result<PostingEntry> {
    if data.len() < POSTING_ENTRY_SIZE {
        return Err(SearchError::IndexCorrupted(format!(
            "posting truncated: need {POSTING_ENTRY_SIZE} bytes, got {}",
            data.len()
        )));
    }
    let doc_id = u32::from_le_bytes(data[0..4].try_into().map_err(|_| {
        SearchError::IndexCorrupted("posting: doc_id slice conversion failed".to_string())
    })?);
    let field_id = data[4];
    // Validate the field_id byte — bad data produces a recoverable error.
    if SearchField::from_discriminant(field_id).is_none() {
        return Err(SearchError::IndexCorrupted(format!(
            "posting: invalid field_id {field_id}"
        )));
    }
    let position = u32::from_le_bytes(data[5..9].try_into().map_err(|_| {
        SearchError::IndexCorrupted("posting: position slice conversion failed".to_string())
    })?);
    Ok(PostingEntry {
        doc_id,
        field_id,
        position,
    })
}

// ============================================================================
// File metadata encode / decode
// ============================================================================

/// Encode a [`FileMetaEntry`] into its 5-byte on-disk representation.
pub(crate) fn encode_file_meta(m: &FileMetaEntry) -> [u8; FILE_META_SIZE] {
    let mut buf = [0u8; FILE_META_SIZE];
    buf[0] = m.lang_id;
    buf[1..5].copy_from_slice(&m.doc_length.to_le_bytes());
    buf
}

/// Decode a [`FileMetaEntry`] from a 5-byte slice.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if the slice is too short.
pub(crate) fn decode_file_meta(data: &[u8]) -> crate::Result<FileMetaEntry> {
    if data.len() < FILE_META_SIZE {
        return Err(SearchError::IndexCorrupted(format!(
            "file_meta truncated: need {FILE_META_SIZE} bytes, got {}",
            data.len()
        )));
    }
    let lang_id = data[0];
    let doc_length = u32::from_le_bytes(data[1..5].try_into().map_err(|_| {
        SearchError::IndexCorrupted("file_meta: doc_length slice conversion failed".to_string())
    })?);
    Ok(FileMetaEntry {
        lang_id,
        doc_length,
    })
}

// ============================================================================
// Binary search
// ============================================================================

/// Binary-search `entries_data` for the entry with `ngram_key`.
///
/// `entries_data` must be a byte slice whose length is a multiple of
/// [`SKIDX_ENTRY_SIZE`] and whose entries are sorted ascending by `ngram_key`.
///
/// Returns `Ok(Some(entry))` if found, `Ok(None)` if absent, or
/// [`SearchError::IndexCorrupted`] if the slice is malformed.
pub(crate) fn lookup_ngram(
    entries_data: &[u8],
    ngram_key: u16,
) -> crate::Result<Option<SkidxEntry>> {
    if !entries_data.len().is_multiple_of(SKIDX_ENTRY_SIZE) {
        return Err(SearchError::IndexCorrupted(format!(
            "entries_data length {} is not a multiple of SKIDX_ENTRY_SIZE {}",
            entries_data.len(),
            SKIDX_ENTRY_SIZE
        )));
    }
    let n = entries_data.len() / SKIDX_ENTRY_SIZE;
    let mut lo = 0usize;
    let mut hi = n;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let offset = mid * SKIDX_ENTRY_SIZE;
        let key =
            u16::from_le_bytes(entries_data[offset..offset + 2].try_into().map_err(|_| {
                SearchError::IndexCorrupted("entries: key read failed".to_string())
            })?);
        match key.cmp(&ngram_key) {
            std::cmp::Ordering::Equal => {
                return decode_entry(&entries_data[offset..offset + SKIDX_ENTRY_SIZE]).map(Some);
            }
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
        }
    }
    Ok(None)
}

// ============================================================================
// Checksum
// ============================================================================

/// Compute the CRC32 checksum of `data`.
///
/// Used to verify index integrity on load.  The checksum in the header
/// covers the entry array and file-metadata bytes appended together.
pub(crate) fn compute_checksum(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

// ============================================================================
// BM25 scoring
// ============================================================================

/// Compute the BM25 contribution for a single term occurrence.
///
/// `tf` — observed term frequency in this document.
/// `idf` — inverse document frequency weight from the bigram table.
/// `doc_len` — byte length of the document.
/// `avg_doc_len` — average byte length across the corpus.
///
/// Returns the BM25 score contribution as `f64` (accumulated across terms
/// to avoid precision loss).
#[must_use]
pub(crate) fn bm25_score(tf: f32, idf: f32, doc_len: u32, avg_doc_len: f32) -> f64 {
    let k1 = f64::from(BM25_K1);
    let b = f64::from(BM25_B);
    let tf = f64::from(tf);
    let idf = f64::from(idf);
    let dl = f64::from(doc_len);
    let adl = if avg_doc_len > 0.0 {
        f64::from(avg_doc_len)
    } else {
        1.0
    };
    let norm = 1.0 - b + b * (dl / adl);
    let tf_norm = tf * (k1 + 1.0) / (tf + k1 * norm);
    idf * tf_norm
}

// ============================================================================
// Language mapping
// ============================================================================

/// Map a [`rskim_core::Language`] variant to a stable 1-byte ID.
///
/// IDs are assigned in alphabetical order of the enum variant names and are
/// part of the stable on-disk format.  Adding a new language variant without
/// a format version bump is acceptable because [`lang_from_id`] returns
/// `None` for unknown IDs (graceful degradation).
#[must_use]
pub(crate) fn lang_to_id(lang: rskim_core::Language) -> u8 {
    match lang {
        rskim_core::Language::C => 0,
        rskim_core::Language::Cpp => 1,
        rskim_core::Language::CSharp => 2,
        rskim_core::Language::Go => 3,
        rskim_core::Language::Java => 4,
        rskim_core::Language::JavaScript => 5,
        rskim_core::Language::Json => 6,
        rskim_core::Language::Kotlin => 7,
        rskim_core::Language::Markdown => 8,
        rskim_core::Language::Python => 9,
        rskim_core::Language::Ruby => 10,
        rskim_core::Language::Rust => 11,
        rskim_core::Language::Sql => 12,
        rskim_core::Language::Swift => 13,
        rskim_core::Language::Toml => 14,
        rskim_core::Language::TypeScript => 15,
        rskim_core::Language::Yaml => 16,
    }
}

/// Recover a [`rskim_core::Language`] from its 1-byte index ID.
///
/// Returns `None` for IDs that don't correspond to any known language,
/// allowing the reader to degrade gracefully when opening indices written
/// by a newer version that supports additional languages.
///
/// Currently used only in tests and reserved for future reader use
/// (e.g. language-aware snippet extraction).
#[must_use]
#[allow(dead_code)]
pub(crate) fn lang_from_id(id: u8) -> Option<rskim_core::Language> {
    match id {
        0 => Some(rskim_core::Language::C),
        1 => Some(rskim_core::Language::Cpp),
        2 => Some(rskim_core::Language::CSharp),
        3 => Some(rskim_core::Language::Go),
        4 => Some(rskim_core::Language::Java),
        5 => Some(rskim_core::Language::JavaScript),
        6 => Some(rskim_core::Language::Json),
        7 => Some(rskim_core::Language::Kotlin),
        8 => Some(rskim_core::Language::Markdown),
        9 => Some(rskim_core::Language::Python),
        10 => Some(rskim_core::Language::Ruby),
        11 => Some(rskim_core::Language::Rust),
        12 => Some(rskim_core::Language::Sql),
        13 => Some(rskim_core::Language::Swift),
        14 => Some(rskim_core::Language::Toml),
        15 => Some(rskim_core::Language::TypeScript),
        16 => Some(rskim_core::Language::Yaml),
        _ => None,
    }
}

/// Compute IDF weight for a bigram key using the empirical weight table.
///
/// Falls back to the default weight for bigrams not present in the table.
#[must_use]
#[inline]
pub(crate) fn idf_for_key(key: u16) -> f32 {
    lookup_weight(key, BIGRAM_WEIGHTS)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
