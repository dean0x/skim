//! Lexical search layer: BM25F scoring over character n-gram inverted index.
//!
//! # Module Layout
//!
//! - [`ngram`] — N-gram extraction from source text
//! - [`index_format`] — Two-file mmap'd on-disk index (`.skidx` + `.skpost`)
//! - [`scoring`] — BM25F scoring formula
//! - [`builder`] — Index construction (`LayerBuilder` implementation)
//! - [`query`] — Query engine (`SearchLayer` + `SearchIndex` implementation)
//!
//! # Shared Types
//!
//! Types used across multiple sub-modules are defined here to avoid circular
//! dependencies and ensure a single source of truth.

pub mod builder;
pub mod index_format;
pub mod ngram;
pub mod query;
pub mod scoring;
pub(crate) mod walker;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{FileTable, IndexStats};

// ============================================================================
// Shared types used across lexical sub-modules
// ============================================================================

/// Fixed-size character n-gram hash. Wraps `u64` for type safety.
///
/// Produced by hashing byte windows (bigrams by default) via FxHash.
/// Two n-grams are equal iff their hashes are equal (hash collision is
/// accepted as a minor scoring inaccuracy, not a correctness bug).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ngram(u64);

impl Ngram {
    /// Hash a byte slice into an `Ngram` using FxHash.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(fxhash_bytes(bytes))
    }

    /// Return the raw `u64` hash value.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// Single entry in a posting list. Fixed 12 bytes on disk.
///
/// # On-disk layout (little-endian)
///
/// | Offset | Size | Field      |
/// |--------|------|------------|
/// | 0      | 4    | `doc_id`   |
/// | 4      | 1    | `field_id` |
/// | 5      | 4    | `position` |
/// | 9      | 2    | `tf`       |
/// | 11     | 1    | padding    |
///
/// `doc_id` is `u32` (not `u64`) — no real-world repo has >4B files.
/// The builder validates `file_count <= u32::MAX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostingEntry {
    /// FileId truncated to `u32` (supports 4 billion files).
    pub doc_id: u32,
    /// `SearchField` as `u8` (7 variants, stable mapping).
    pub field_id: u8,
    /// Byte offset of this n-gram occurrence in the source file.
    pub position: u32,
    /// Term frequency of this n-gram in this field of this document.
    pub tf: u16,
}

/// Size of a single [`PostingEntry`] on disk, in bytes.
pub const POSTING_ENTRY_SIZE: usize = 12;

impl PostingEntry {
    /// Serialize to a fixed 12-byte little-endian representation.
    pub fn to_bytes(self) -> [u8; POSTING_ENTRY_SIZE] {
        let mut buf = [0u8; POSTING_ENTRY_SIZE];
        buf[0..4].copy_from_slice(&self.doc_id.to_le_bytes());
        buf[4] = self.field_id;
        buf[5..9].copy_from_slice(&self.position.to_le_bytes());
        buf[9..11].copy_from_slice(&self.tf.to_le_bytes());
        // buf[11] = 0 (padding, already zeroed)
        buf
    }

    /// Deserialize from a 12-byte little-endian slice.
    ///
    /// Returns `None` if the slice is too short.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < POSTING_ENTRY_SIZE {
            return None;
        }
        Some(Self {
            doc_id: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            field_id: bytes[4],
            position: u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]),
            tf: u16::from_le_bytes([bytes[9], bytes[10]]),
        })
    }
}

/// On-disk index header. Fixed size, versioned.
///
/// Stored at the beginning of `.skidx` files. The magic bytes and version
/// are validated on open to detect corruption or format mismatches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexHeader {
    /// Magic bytes: `b"SKIM"`.
    pub magic: [u8; 4],
    /// Format version (currently 1).
    pub version: u32,
    /// Number of unique n-grams in the index.
    pub ngram_count: u64,
    /// Number of indexed files.
    pub file_count: u64,
    /// Unix timestamp (seconds since epoch) when the index was created.
    pub created_at: u64,
}

/// Size of the [`IndexHeader`] on disk, in bytes.
pub const INDEX_HEADER_SIZE: usize = 32;

/// Current index format version.
pub const INDEX_FORMAT_VERSION: u32 = 1;

/// Magic bytes for the index file header.
pub const INDEX_MAGIC: [u8; 4] = *b"SKIM";

impl IndexHeader {
    /// Serialize to a fixed 32-byte little-endian representation.
    pub fn to_bytes(self) -> [u8; INDEX_HEADER_SIZE] {
        let mut buf = [0u8; INDEX_HEADER_SIZE];
        buf[0..4].copy_from_slice(&self.magic);
        buf[4..8].copy_from_slice(&self.version.to_le_bytes());
        buf[8..16].copy_from_slice(&self.ngram_count.to_le_bytes());
        buf[16..24].copy_from_slice(&self.file_count.to_le_bytes());
        buf[24..32].copy_from_slice(&self.created_at.to_le_bytes());
        buf
    }

    /// Deserialize from a 32-byte little-endian slice.
    ///
    /// Returns `None` if the slice is too short.
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < INDEX_HEADER_SIZE {
            return None;
        }
        let mut magic = [0u8; 4];
        magic.copy_from_slice(&bytes[0..4]);
        Some(Self {
            magic,
            version: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            ngram_count: u64::from_le_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]),
            file_count: u64::from_le_bytes([
                bytes[16], bytes[17], bytes[18], bytes[19], bytes[20], bytes[21], bytes[22],
                bytes[23],
            ]),
            created_at: u64::from_le_bytes([
                bytes[24], bytes[25], bytes[26], bytes[27], bytes[28], bytes[29], bytes[30],
                bytes[31],
            ]),
        })
    }
}

/// BM25F tuning parameters. Stored in index metadata for reproducibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bm25Params {
    /// Term saturation parameter (default 1.2).
    pub k1: f32,
    /// Length normalization parameter (default 0.75).
    pub b: f32,
    /// Average document length in tokens (computed at build time).
    pub avg_doc_len: f32,
}

impl Default for Bm25Params {
    fn default() -> Self {
        Self {
            k1: 1.2,
            b: 0.75,
            avg_doc_len: 0.0,
        }
    }
}

/// Persistent index metadata, serialized as `metadata.json`.
///
/// Contains everything needed to open and query the index without
/// re-scanning the repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexMetadata {
    /// Bidirectional path ↔ FileId mapping.
    pub file_table: FileTable,
    /// BM25F scoring parameters (including computed avg_doc_len).
    pub bm25_params: Bm25Params,
    /// Index statistics.
    pub stats: IndexStats,
    /// Per-file mtimes for staleness detection.
    /// Stored as `(relative_path, unix_timestamp_seconds)`.
    pub file_mtimes: Vec<(PathBuf, u64)>,
    /// Canonical repo root path (for collision detection across repos
    /// that hash to the same directory name).
    pub repo_root: PathBuf,
    /// Per-document token counts, indexed by `doc_id`.
    ///
    /// Used for BM25F length normalization at query time. Absent in
    /// indexes built before this field was added; defaults to empty
    /// (which causes the scorer to fall back to `avg_doc_len`-only
    /// normalization, the same behavior as the previous implementation).
    #[serde(default)]
    pub doc_lengths: Vec<u32>,
}

// ============================================================================
// Internal helpers
// ============================================================================

/// FxHash for byte slices. Deterministic, fast, non-cryptographic.
///
/// Used for n-gram hashing and cache key derivation. Not suitable for
/// security-sensitive contexts.
///
/// Public so that downstream crates (e.g. the CLI) can compute the same
/// hash without duplicating the algorithm. Changing this function would
/// invalidate all existing on-disk indexes.
pub fn fxhash_bytes(bytes: &[u8]) -> u64 {
    // FxHash algorithm: multiply-rotate on each byte.
    const SEED: u64 = 0x517c_c1b7_2722_0a95;
    let mut hash: u64 = 0;
    for &b in bytes {
        hash = (hash.rotate_left(5) ^ u64::from(b)).wrapping_mul(SEED);
    }
    hash
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posting_entry_roundtrip() {
        let entry = PostingEntry {
            doc_id: 42,
            field_id: 2,
            position: 1024,
            tf: 7,
        };
        let bytes = entry.to_bytes();
        let decoded = PostingEntry::from_bytes(&bytes);
        assert_eq!(decoded, Some(entry));
    }

    #[test]
    fn posting_entry_from_short_slice() {
        assert_eq!(PostingEntry::from_bytes(&[0; 11]), None);
    }

    #[test]
    fn index_header_roundtrip() {
        let header = IndexHeader {
            magic: INDEX_MAGIC,
            version: INDEX_FORMAT_VERSION,
            ngram_count: 50_000,
            file_count: 1_234,
            created_at: 1_700_000_000,
        };
        let bytes = header.to_bytes();
        let decoded = IndexHeader::from_bytes(&bytes);
        assert_eq!(decoded, Some(header));
    }

    #[test]
    fn index_header_from_short_slice() {
        assert_eq!(IndexHeader::from_bytes(&[0; 31]), None);
    }

    #[test]
    fn ngram_deterministic() {
        let a = Ngram::from_bytes(b"fn");
        let b = Ngram::from_bytes(b"fn");
        assert_eq!(a, b);
    }

    #[test]
    fn ngram_different_inputs_differ() {
        let a = Ngram::from_bytes(b"fn");
        let b = Ngram::from_bytes(b"if");
        assert_ne!(a, b);
    }

    #[test]
    fn bm25_params_defaults() {
        let p = Bm25Params::default();
        assert!((p.k1 - 1.2).abs() < f32::EPSILON);
        assert!((p.b - 0.75).abs() < f32::EPSILON);
        assert!((p.avg_doc_len).abs() < f32::EPSILON);
    }
}
