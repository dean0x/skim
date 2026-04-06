//! [`IndexEntry`] — 20-byte on-disk lookup table entry.

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
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_entry_roundtrip() {
        let entry = IndexEntry {
            ngram_hash: 0xDEAD_BEEF_CAFE_1234,
            posting_offset: 42,
            posting_length: 7,
        };
        let bytes = entry.to_bytes();
        let decoded = IndexEntry::from_bytes(&bytes).expect("roundtrip must succeed");
        assert_eq!(decoded.ngram_hash, entry.ngram_hash);
        assert_eq!(decoded.posting_offset, entry.posting_offset);
        assert_eq!(decoded.posting_length, entry.posting_length);
    }

    #[test]
    fn index_entry_from_short_slice_returns_none() {
        assert!(IndexEntry::from_bytes(&[0u8; 19]).is_none());
    }
}
