//! Tests for the co-change binary format codec (format.rs).

#![allow(clippy::unwrap_used)]

use super::*;

// -----------------------------------------------------------------------
// Header roundtrip
// -----------------------------------------------------------------------

#[test]
fn test_header_roundtrip() {
    let h = SkccHeader {
        magic: *SKCC_MAGIC,
        version: FORMAT_VERSION,
        pair_count: 42,
        file_count: 10,
        checksum: 0xDEAD_BEEF,
    };
    let encoded = encode_header(&h);
    assert_eq!(encoded.len(), HEADER_SIZE);
    let decoded = decode_header(&encoded).unwrap();
    assert_eq!(decoded.magic, h.magic);
    assert_eq!(decoded.version, h.version);
    assert_eq!(decoded.pair_count, h.pair_count);
    assert_eq!(decoded.file_count, h.file_count);
    assert_eq!(decoded.checksum, h.checksum);
}

#[test]
fn test_header_bad_magic() {
    let h = SkccHeader {
        magic: *b"ABCD",
        version: FORMAT_VERSION,
        pair_count: 0,
        file_count: 0,
        checksum: 0,
    };
    let encoded = encode_header(&h);
    let result = decode_header(&encoded);
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("magic"), "error should mention 'magic': {msg}");
}

#[test]
fn test_header_bad_version() {
    let h = SkccHeader {
        magic: *SKCC_MAGIC,
        version: FORMAT_VERSION + 1,
        pair_count: 0,
        file_count: 0,
        checksum: 0,
    };
    let encoded = encode_header(&h);
    let result = decode_header(&encoded);
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("version"),
        "error should mention 'version': {msg}"
    );
}

#[test]
fn test_header_truncated() {
    let h = SkccHeader {
        magic: *SKCC_MAGIC,
        version: FORMAT_VERSION,
        pair_count: 1,
        file_count: 1,
        checksum: 0,
    };
    let encoded = encode_header(&h);
    // Truncate to 10 bytes (less than HEADER_SIZE = 18)
    let truncated = &encoded[..10];
    let result = decode_header(truncated);
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("truncated") || msg.contains("bytes"),
        "error should describe truncation: {msg}"
    );
}

// -----------------------------------------------------------------------
// FileCommitEntry roundtrip
// -----------------------------------------------------------------------

#[test]
fn test_file_commit_entry_roundtrip() {
    let e = FileCommitEntry {
        file_id: 7,
        commit_count: 42,
    };
    let encoded = encode_file_commit(&e);
    assert_eq!(encoded.len(), FILE_COMMIT_ENTRY_SIZE);
    let decoded = decode_file_commit(&encoded).unwrap();
    assert_eq!(decoded.file_id, e.file_id);
    assert_eq!(decoded.commit_count, e.commit_count);
}

// -----------------------------------------------------------------------
// PairEntry roundtrip
// -----------------------------------------------------------------------

#[test]
fn test_pair_entry_roundtrip() {
    let p = PairEntry {
        file_a: 3,
        file_b: 5,
        count: 17,
    };
    let encoded = encode_pair(&p);
    assert_eq!(encoded.len(), PAIR_ENTRY_SIZE);
    let decoded = decode_pair(&encoded).unwrap();
    assert_eq!(decoded.file_a, p.file_a);
    assert_eq!(decoded.file_b, p.file_b);
    assert_eq!(decoded.count, p.count);
}

// -----------------------------------------------------------------------
// lookup_pair
// -----------------------------------------------------------------------

#[test]
fn test_lookup_pair_found() {
    // Build a sorted pairs slice: (1,2,10), (1,3,5), (2,4,3)
    let pairs = [
        PairEntry {
            file_a: 1,
            file_b: 2,
            count: 10,
        },
        PairEntry {
            file_a: 1,
            file_b: 3,
            count: 5,
        },
        PairEntry {
            file_a: 2,
            file_b: 4,
            count: 3,
        },
    ];
    let mut data = Vec::new();
    for p in &pairs {
        data.extend_from_slice(&encode_pair(p));
    }
    let result = lookup_pair(&data, 1, 3).unwrap();
    assert_eq!(result, Some(5));
}

#[test]
fn test_lookup_pair_not_found() {
    let pairs = [
        PairEntry {
            file_a: 1,
            file_b: 2,
            count: 10,
        },
        PairEntry {
            file_a: 2,
            file_b: 4,
            count: 3,
        },
    ];
    let mut data = Vec::new();
    for p in &pairs {
        data.extend_from_slice(&encode_pair(p));
    }
    let result = lookup_pair(&data, 1, 3).unwrap();
    assert_eq!(result, None);
}

#[test]
fn test_lookup_pair_empty_slice() {
    let result = lookup_pair(&[], 0, 1).unwrap();
    assert_eq!(result, None);
}

#[test]
fn test_lookup_pair_misaligned_slice() {
    // 5 bytes is not a multiple of PAIR_ENTRY_SIZE (12)
    let data = vec![0u8; 5];
    let result = lookup_pair(&data, 0, 1);
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("multiple") || msg.contains("aligned") || msg.contains("corrupt"),
        "error should mention alignment: {msg}"
    );
}

// -----------------------------------------------------------------------
// FileCommitEntry truncation
// -----------------------------------------------------------------------

#[test]
fn test_file_commit_entry_truncated() {
    let data = [0u8; 4]; // less than FILE_COMMIT_ENTRY_SIZE (8)
    let result = decode_file_commit(&data);
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("truncated") || msg.contains("bytes"),
        "error should describe truncation: {msg}"
    );
}

// -----------------------------------------------------------------------
// PairEntry truncation
// -----------------------------------------------------------------------

#[test]
fn test_pair_entry_truncated() {
    let data = [0u8; 8]; // less than PAIR_ENTRY_SIZE (12)
    let result = decode_pair(&data);
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("truncated") || msg.contains("bytes"),
        "error should describe truncation: {msg}"
    );
}

// -----------------------------------------------------------------------
// Checksum determinism
// -----------------------------------------------------------------------

#[test]
fn test_checksum_determinism() {
    let data = b"hello world";
    let c1 = compute_checksum(data);
    let c2 = compute_checksum(data);
    assert_eq!(c1, c2);
}

#[test]
fn test_checksum_different_data() {
    let c1 = compute_checksum(b"hello");
    let c2 = compute_checksum(b"world");
    assert_ne!(c1, c2);
}

#[test]
fn test_checksum_empty() {
    // Empty data should not panic and should return a deterministic value.
    let c1 = compute_checksum(b"");
    let c2 = compute_checksum(b"");
    assert_eq!(c1, c2);
}
