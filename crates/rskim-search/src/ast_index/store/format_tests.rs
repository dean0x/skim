//! Tests for the AST index pure codec (`format.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

// ============================================================================
// Header roundtrip
// ============================================================================

fn make_valid_header() -> AstSkidxHeader {
    AstSkidxHeader {
        magic: *SKAX_MAGIC,
        version: FORMAT_VERSION,
        bigram_count: 10,
        trigram_count: 5,
        file_count: 3,
        postings_file_size: 1024,
        avg_bigram_count: 3.5_f32,
        avg_trigram_count: 1.2_f32,
        avg_node_count: 42.0_f32,
        avg_max_depth: 5.2_f32,
        checksum: 0xDEAD_BEEF,
    }
}

#[test]
fn header_roundtrip() {
    let h = make_valid_header();
    let encoded = encode_header(&h);
    assert_eq!(encoded.len(), HEADER_SIZE);
    let decoded = decode_header(&encoded).unwrap();
    assert_eq!(decoded, h);
}

#[test]
fn header_roundtrip_zero_avgs() {
    let h = AstSkidxHeader {
        magic: *SKAX_MAGIC,
        version: FORMAT_VERSION,
        bigram_count: 0,
        trigram_count: 0,
        file_count: 0,
        postings_file_size: 0,
        avg_bigram_count: 0.0,
        avg_trigram_count: 0.0,
        avg_node_count: 0.0,
        avg_max_depth: 0.0,
        checksum: 0,
    };
    let encoded = encode_header(&h);
    let decoded = decode_header(&encoded).unwrap();
    assert_eq!(decoded, h);
}

// ============================================================================
// Header rejection cases
// ============================================================================

#[test]
fn header_rejects_truncation() {
    let encoded = encode_header(&make_valid_header());
    // Truncate to one byte short
    let err = decode_header(&encoded[..HEADER_SIZE - 1]).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("truncated"), "expected 'truncated' in: {msg}");
}

#[test]
fn header_rejects_empty() {
    let err = decode_header(&[]).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("truncated"), "expected 'truncated' in: {msg}");
}

#[test]
fn header_rejects_bad_magic() {
    let mut encoded = encode_header(&make_valid_header());
    // Replace magic with b"SKIX" (the lexical index magic)
    encoded[0..4].copy_from_slice(b"SKIX");
    let err = decode_header(&encoded).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("bad magic"), "expected 'bad magic' in: {msg}");
}

#[test]
fn header_rejects_garbage_magic() {
    let mut encoded = encode_header(&make_valid_header());
    encoded[0..4].copy_from_slice(&[0xFF, 0x00, 0xAB, 0xCD]);
    let err = decode_header(&encoded).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("bad magic"), "expected 'bad magic' in: {msg}");
}

#[test]
fn header_rejects_wrong_version_zero() {
    let mut encoded = encode_header(&make_valid_header());
    encoded[4..6].copy_from_slice(&0u16.to_le_bytes());
    let err = decode_header(&encoded).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("format version"),
        "expected 'format version' in: {msg}"
    );
}

#[test]
fn header_rejects_wrong_version_one() {
    // v1 is no longer valid — it lacks v2 structural fields.
    // The reader must reject it with "please rebuild the AST index".
    let mut encoded = encode_header(&make_valid_header());
    encoded[4..6].copy_from_slice(&1u16.to_le_bytes());
    let err = decode_header(&encoded).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("format version"),
        "expected 'format version' in: {msg}"
    );
    assert!(
        msg.contains("please rebuild the AST index"),
        "expected 'please rebuild' in: {msg}"
    );
}

#[test]
fn header_rejects_wrong_version_three() {
    // A future version (3) must also be rejected by this binary.
    let mut encoded = encode_header(&make_valid_header());
    encoded[4..6].copy_from_slice(&3u16.to_le_bytes());
    let err = decode_header(&encoded).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("format version"),
        "expected 'format version' in: {msg}"
    );
}

#[test]
fn header_rejects_non_finite_avg_bigram_count() {
    let mut encoded = encode_header(&make_valid_header());
    encoded[26..30].copy_from_slice(&f32::NAN.to_le_bytes());
    let err = decode_header(&encoded).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("avg_bigram_count"),
        "expected 'avg_bigram_count' in: {msg}"
    );
}

#[test]
fn header_rejects_negative_avg_bigram_count() {
    let mut encoded = encode_header(&make_valid_header());
    encoded[26..30].copy_from_slice(&(-1.0_f32).to_le_bytes());
    let err = decode_header(&encoded).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("avg_bigram_count"),
        "expected 'avg_bigram_count' in: {msg}"
    );
}

#[test]
fn header_rejects_inf_avg_bigram_count() {
    let mut encoded = encode_header(&make_valid_header());
    encoded[26..30].copy_from_slice(&f32::INFINITY.to_le_bytes());
    let err = decode_header(&encoded).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("avg_bigram_count"),
        "expected 'avg_bigram_count' in: {msg}"
    );
}

#[test]
fn header_rejects_non_finite_avg_trigram_count() {
    let mut encoded = encode_header(&make_valid_header());
    encoded[30..34].copy_from_slice(&f32::NAN.to_le_bytes());
    let err = decode_header(&encoded).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("avg_trigram_count"),
        "expected 'avg_trigram_count' in: {msg}"
    );
}

#[test]
fn header_rejects_negative_avg_trigram_count() {
    let mut encoded = encode_header(&make_valid_header());
    encoded[30..34].copy_from_slice(&(-0.1_f32).to_le_bytes());
    let err = decode_header(&encoded).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("avg_trigram_count"),
        "expected 'avg_trigram_count' in: {msg}"
    );
}

#[test]
fn header_rejects_non_finite_avg_node_count() {
    let mut encoded = encode_header(&make_valid_header());
    encoded[34..38].copy_from_slice(&f32::NEG_INFINITY.to_le_bytes());
    let err = decode_header(&encoded).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("avg_node_count"),
        "expected 'avg_node_count' in: {msg}"
    );
}

#[test]
fn header_rejects_negative_avg_node_count() {
    let mut encoded = encode_header(&make_valid_header());
    encoded[34..38].copy_from_slice(&(-100.0_f32).to_le_bytes());
    let err = decode_header(&encoded).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("avg_node_count"),
        "expected 'avg_node_count' in: {msg}"
    );
}

// ============================================================================
// AstBigramTableEntry roundtrip
// ============================================================================

#[test]
fn bigram_entry_roundtrip() {
    let e = AstBigramTableEntry {
        key: 0x0001_0002_u32,
        posting_offset: 512,
        posting_length: 32,
    };
    let encoded = encode_bigram_entry(&e);
    assert_eq!(encoded.len(), BIGRAM_ENTRY_SIZE);
    let decoded = decode_bigram_entry(&encoded).unwrap();
    assert_eq!(decoded, e);
}

#[test]
fn bigram_entry_boundary_max_key() {
    let e = AstBigramTableEntry {
        key: u32::MAX,
        posting_offset: u64::MAX,
        posting_length: u32::MAX,
    };
    let encoded = encode_bigram_entry(&e);
    let decoded = decode_bigram_entry(&encoded).unwrap();
    assert_eq!(decoded, e);
}

#[test]
fn bigram_entry_rejects_truncation() {
    let e = AstBigramTableEntry {
        key: 1,
        posting_offset: 0,
        posting_length: 8,
    };
    let encoded = encode_bigram_entry(&e);
    let err = decode_bigram_entry(&encoded[..BIGRAM_ENTRY_SIZE - 1]).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("truncated"), "expected 'truncated' in: {msg}");
}

// ============================================================================
// AstTrigramTableEntry roundtrip
// ============================================================================

#[test]
fn trigram_entry_roundtrip() {
    let e = AstTrigramTableEntry {
        key: 0x0001_0002_0003_u64,
        posting_offset: 0,
        posting_length: 8,
    };
    let encoded = encode_trigram_entry(&e);
    assert_eq!(encoded.len(), TRIGRAM_ENTRY_SIZE);
    let decoded = decode_trigram_entry(&encoded).unwrap();
    assert_eq!(decoded, e);
}

#[test]
fn trigram_entry_boundary_max_key() {
    let e = AstTrigramTableEntry {
        key: u64::MAX,
        posting_offset: u64::MAX,
        posting_length: u32::MAX,
    };
    let encoded = encode_trigram_entry(&e);
    let decoded = decode_trigram_entry(&encoded).unwrap();
    assert_eq!(decoded, e);
}

#[test]
fn trigram_entry_rejects_truncation() {
    let e = AstTrigramTableEntry {
        key: 1,
        posting_offset: 0,
        posting_length: 0,
    };
    let encoded = encode_trigram_entry(&e);
    let err = decode_trigram_entry(&encoded[..TRIGRAM_ENTRY_SIZE - 1]).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("truncated"), "expected 'truncated' in: {msg}");
}

// ============================================================================
// AstPostingEntry roundtrip
// ============================================================================

#[test]
fn posting_roundtrip() {
    let p = AstPostingEntry {
        doc_id: 42,
        count: 7,
    };
    let encoded = encode_posting(&p);
    assert_eq!(encoded.len(), POSTING_ENTRY_SIZE);
    let decoded = decode_posting(&encoded).unwrap();
    assert_eq!(decoded, p);
}

#[test]
fn posting_boundary_min_count() {
    let p = AstPostingEntry {
        doc_id: 0,
        count: 1,
    };
    let encoded = encode_posting(&p);
    let decoded = decode_posting(&encoded).unwrap();
    assert_eq!(decoded.count, 1);
}

#[test]
fn posting_rejects_count_zero() {
    let p = AstPostingEntry {
        doc_id: 0,
        count: 0,
    };
    let encoded = encode_posting(&p);
    let err = decode_posting(&encoded).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("count must be >= 1"),
        "expected 'count must be >= 1' in: {msg}"
    );
}

#[test]
fn posting_rejects_truncation() {
    let p = AstPostingEntry {
        doc_id: 1,
        count: 1,
    };
    let encoded = encode_posting(&p);
    let err = decode_posting(&encoded[..POSTING_ENTRY_SIZE - 1]).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("truncated"), "expected 'truncated' in: {msg}");
}

// ============================================================================
// AstFileMetaEntry roundtrip
// ============================================================================

// ============================================================================
// A2: AstFileMetaEntry v2 round-trip and size constant
// ============================================================================

fn make_file_meta() -> AstFileMetaEntry {
    AstFileMetaEntry {
        lang_id: 11, // Rust
        node_count: 256,
        max_depth: 12,
        max_block_stmts: 25,
        max_params: 6,
        branch_count: 8,
    }
}

#[test]
fn file_meta_roundtrip() {
    let m = make_file_meta();
    let encoded = encode_file_meta(&m);
    // A2: FILE_META_SIZE == 15 (P3: delta +10 bytes/file from v1's 5 bytes)
    assert_eq!(encoded.len(), FILE_META_SIZE);
    assert_eq!(FILE_META_SIZE, 15, "P3: file meta must be 15 bytes");
    let decoded = decode_file_meta(&encoded).unwrap();
    assert_eq!(decoded, m);
}

#[test]
fn file_meta_roundtrip_all_fields() {
    // Round-trip with boundary/max values to verify all field encodings
    let m = AstFileMetaEntry {
        lang_id: 0xFF,
        node_count: u32::MAX,
        max_depth: u16::MAX,
        max_block_stmts: u16::MAX,
        max_params: u16::MAX,
        branch_count: u32::MAX,
    };
    let encoded = encode_file_meta(&m);
    let decoded = decode_file_meta(&encoded).unwrap();
    assert_eq!(decoded, m);
}

#[test]
fn file_meta_boundary_max_node_count() {
    let m = AstFileMetaEntry {
        lang_id: 0,
        node_count: u32::MAX,
        max_depth: 0,
        max_block_stmts: 0,
        max_params: 0,
        branch_count: 0,
    };
    let encoded = encode_file_meta(&m);
    let decoded = decode_file_meta(&encoded).unwrap();
    assert_eq!(decoded.node_count, u32::MAX);
}

#[test]
fn file_meta_rejects_truncation() {
    let m = make_file_meta();
    let encoded = encode_file_meta(&m);
    let err = decode_file_meta(&encoded[..FILE_META_SIZE - 1]).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("truncated"), "expected 'truncated' in: {msg}");
}

// ============================================================================
// A1: FORMAT_VERSION == 2, magic unchanged
// ============================================================================

#[test]
fn a1_format_version_is_2() {
    assert_eq!(FORMAT_VERSION, 2, "A1: FORMAT_VERSION must be 2");
}

#[test]
fn a1_magic_unchanged() {
    assert_eq!(SKAX_MAGIC, b"SKAX", "A1: magic must remain b\"SKAX\"");
}

// ============================================================================
// A3: Reader rejects v1 with "please rebuild the AST index"
// ============================================================================

#[test]
fn a3_reader_rejects_v1_header() {
    // Hand-craft a minimal v1 header (48 bytes, version = 1)
    let mut v1_header = [0u8; 48];
    v1_header[0..4].copy_from_slice(b"SKAX");
    v1_header[4..6].copy_from_slice(&1u16.to_le_bytes()); // version = 1
    // All other fields 0 (empty index)

    let err = decode_header(&v1_header).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("please rebuild the AST index"),
        "A3: v1 rejection must contain 'please rebuild the AST index', got: {msg}"
    );
    assert!(
        msg.contains("format version"),
        "A3: v1 rejection must contain 'format version', got: {msg}"
    );
}

// ============================================================================
// A8: avg_max_depth decodes correctly from header reserved bytes
// ============================================================================

#[test]
fn a8_avg_max_depth_roundtrip() {
    let h = AstSkidxHeader {
        magic: *SKAX_MAGIC,
        version: FORMAT_VERSION,
        bigram_count: 0,
        trigram_count: 0,
        file_count: 0,
        postings_file_size: 0,
        avg_bigram_count: 0.0,
        avg_trigram_count: 0.0,
        avg_node_count: 0.0,
        avg_max_depth: 7.5,
        checksum: 0,
    };
    let encoded = encode_header(&h);
    let decoded = decode_header(&encoded).unwrap();
    assert!(
        (decoded.avg_max_depth - 7.5).abs() < 1e-6,
        "A8: avg_max_depth must round-trip; got {}",
        decoded.avg_max_depth
    );
}

// ============================================================================
// CRC determinism
// ============================================================================

#[test]
fn crc_determinism() {
    let data = b"hello world";
    let crc1 = compute_checksum(data);
    let crc2 = compute_checksum(data);
    assert_eq!(crc1, crc2);
}

#[test]
fn crc_differs_for_different_data() {
    let crc1 = compute_checksum(b"foo");
    let crc2 = compute_checksum(b"bar");
    assert_ne!(crc1, crc2);
}

// ============================================================================
// Binary search: lookup_bigram
// ============================================================================

/// Build a sorted flat byte buffer of bigram entries for lookup tests.
fn build_bigram_entries_data(keys: &[u32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(keys.len() * BIGRAM_ENTRY_SIZE);
    for (i, &key) in keys.iter().enumerate() {
        let e = AstBigramTableEntry {
            key,
            posting_offset: (i * 8) as u64,
            posting_length: 8,
        };
        buf.extend_from_slice(&encode_bigram_entry(&e));
    }
    buf
}

#[test]
fn lookup_bigram_hit() {
    let data = build_bigram_entries_data(&[10, 20, 30, 40, 50]);
    let entry = lookup_bigram(&data, 30).unwrap();
    assert!(entry.is_some());
    assert_eq!(entry.unwrap().key, 30);
}

#[test]
fn lookup_bigram_miss_below() {
    let data = build_bigram_entries_data(&[10, 20, 30]);
    let entry = lookup_bigram(&data, 5).unwrap();
    assert!(entry.is_none());
}

#[test]
fn lookup_bigram_miss_above() {
    let data = build_bigram_entries_data(&[10, 20, 30]);
    let entry = lookup_bigram(&data, 99).unwrap();
    assert!(entry.is_none());
}

#[test]
fn lookup_bigram_miss_between() {
    let data = build_bigram_entries_data(&[10, 30, 50]);
    let entry = lookup_bigram(&data, 20).unwrap();
    assert!(entry.is_none());
}

#[test]
fn lookup_bigram_empty_entries() {
    let entry = lookup_bigram(&[], 42).unwrap();
    assert!(entry.is_none());
}

#[test]
fn lookup_bigram_single_entry_hit() {
    let data = build_bigram_entries_data(&[42]);
    let entry = lookup_bigram(&data, 42).unwrap();
    assert!(entry.is_some());
}

#[test]
fn lookup_bigram_single_entry_miss() {
    let data = build_bigram_entries_data(&[42]);
    let entry = lookup_bigram(&data, 43).unwrap();
    assert!(entry.is_none());
}

#[test]
fn lookup_bigram_rejects_non_multiple_of_stride() {
    let data = vec![0u8; BIGRAM_ENTRY_SIZE + 1];
    let err = lookup_bigram(&data, 0).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not a multiple"),
        "expected 'not a multiple' in: {msg}"
    );
}

// ============================================================================
// Binary search: lookup_trigram
// ============================================================================

/// Build a sorted flat byte buffer of trigram entries for lookup tests.
fn build_trigram_entries_data(keys: &[u64]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(keys.len() * TRIGRAM_ENTRY_SIZE);
    for (i, &key) in keys.iter().enumerate() {
        let e = AstTrigramTableEntry {
            key,
            posting_offset: (i * 8) as u64,
            posting_length: 8,
        };
        buf.extend_from_slice(&encode_trigram_entry(&e));
    }
    buf
}

#[test]
fn lookup_trigram_hit() {
    let data = build_trigram_entries_data(&[100, 200, 300]);
    let entry = lookup_trigram(&data, 200).unwrap();
    assert!(entry.is_some());
    assert_eq!(entry.unwrap().key, 200);
}

#[test]
fn lookup_trigram_miss_below() {
    let data = build_trigram_entries_data(&[100, 200, 300]);
    let entry = lookup_trigram(&data, 50).unwrap();
    assert!(entry.is_none());
}

#[test]
fn lookup_trigram_miss_above() {
    let data = build_trigram_entries_data(&[100, 200, 300]);
    let entry = lookup_trigram(&data, 400).unwrap();
    assert!(entry.is_none());
}

#[test]
fn lookup_trigram_empty_entries() {
    let entry = lookup_trigram(&[], 100).unwrap();
    assert!(entry.is_none());
}

#[test]
fn lookup_trigram_rejects_non_multiple_of_stride() {
    let data = vec![0u8; TRIGRAM_ENTRY_SIZE + 3];
    let err = lookup_trigram(&data, 0).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("not a multiple"),
        "expected 'not a multiple' in: {msg}"
    );
}

// ============================================================================
// Disjointness: trigram key space vs bigram key space
// ============================================================================

/// Spot-check that AstTrigram::encode(1,1,1) produces a key >= 2^32,
/// while any AstBigram key is < 2^32 (since bigram keys are u32).
///
/// The layout does NOT rely on this disjointness for correctness — bigrams
/// are stored in a separate section with separate offsets/lengths.  This test
/// documents the encoding invariant for readers.
#[test]
fn trigram_key_space_exceeds_bigram_key_space() {
    use crate::ast_index::{AstBigram, AstTrigram};
    // Trigram encode(1,1,1) = (1<<32)|(1<<16)|1 = 0x0000_0001_0001_0001
    let trigram_key = AstTrigram::encode(1, 1, 1).key();
    assert!(
        trigram_key >= (1u64 << 32),
        "encode(1,1,1).key() {trigram_key:#018x} should be >= 2^32"
    );

    // Any bigram key fits in u32 (i.e. < 2^32)
    let bigram_key = AstBigram::encode(u16::MAX, u16::MAX).key();
    assert!(
        u64::from(bigram_key) < (1u64 << 32),
        "bigram key {bigram_key:#010x} should be < 2^32"
    );
}
