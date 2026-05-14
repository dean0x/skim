//! Tests for the format codec (format.rs).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

// -----------------------------------------------------------------------
// Header roundtrip
// -----------------------------------------------------------------------

#[test]
fn test_header_roundtrip() {
    let h = SkidxHeader {
        magic: *SKIDX_MAGIC,
        version: FORMAT_VERSION,
        ngram_count: 1024,
        file_count: 42,
        postings_file_size: 65536,
        avg_doc_length: 512.5,
        checksum: 0xDEAD_BEEF,
    };
    let encoded = encode_header(&h);
    assert_eq!(encoded.len(), SKIDX_HEADER_SIZE);
    let decoded = decode_header(&encoded).unwrap();
    assert_eq!(decoded.magic, h.magic);
    assert_eq!(decoded.version, h.version);
    assert_eq!(decoded.ngram_count, h.ngram_count);
    assert_eq!(decoded.file_count, h.file_count);
    assert_eq!(decoded.postings_file_size, h.postings_file_size);
    assert!((decoded.avg_doc_length - h.avg_doc_length).abs() < f32::EPSILON);
    assert_eq!(decoded.checksum, h.checksum);
}

#[test]
fn test_header_bad_magic() {
    let h = SkidxHeader {
        magic: *b"ABCD",
        version: FORMAT_VERSION,
        ngram_count: 0,
        file_count: 0,
        postings_file_size: 0,
        avg_doc_length: 0.0,
        checksum: 0,
    };
    let encoded = encode_header(&h);
    let result = decode_header(&encoded);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("bad magic"), "unexpected error: {err}");
}

#[test]
fn test_header_bad_version() {
    let h = SkidxHeader {
        magic: *SKIDX_MAGIC,
        version: 999,
        ngram_count: 0,
        file_count: 0,
        postings_file_size: 0,
        avg_doc_length: 0.0,
        checksum: 0,
    };
    let encoded = encode_header(&h);
    let result = decode_header(&encoded);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("unsupported format version"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_header_truncated() {
    let result = decode_header(&[0u8; 10]);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("truncated"), "unexpected error: {err}");
}

// -----------------------------------------------------------------------
// Entry roundtrip
// -----------------------------------------------------------------------

#[test]
fn test_entry_roundtrip() {
    let e = SkidxEntry {
        ngram_key: 0x6865, // "he"
        posting_offset: 4096,
        posting_length: 27,
    };
    let encoded = encode_entry(&e);
    assert_eq!(encoded.len(), SKIDX_ENTRY_SIZE);
    let decoded = decode_entry(&encoded).unwrap();
    assert_eq!(decoded, e);
}

#[test]
fn test_entry_truncated() {
    let result = decode_entry(&[0u8; 5]);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("truncated"), "unexpected error: {err}");
}

// -----------------------------------------------------------------------
// Posting roundtrip
// -----------------------------------------------------------------------

#[test]
fn test_posting_roundtrip() {
    let p = PostingEntry {
        doc_id: 7,
        field_id: crate::SearchField::FunctionSignature.discriminant(),
        position: 1024,
    };
    let encoded = encode_posting(&p);
    assert_eq!(encoded.len(), POSTING_ENTRY_SIZE);
    let decoded = decode_posting(&encoded).unwrap();
    assert_eq!(decoded, p);
}

#[test]
fn test_posting_bad_field_id() {
    let p = PostingEntry {
        doc_id: 0,
        field_id: 200, // invalid
        position: 0,
    };
    let encoded = encode_posting(&p);
    let result = decode_posting(&encoded);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("invalid field_id"), "unexpected error: {err}");
}

#[test]
fn test_posting_truncated() {
    let result = decode_posting(&[0u8; 3]);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("truncated"), "unexpected error: {err}");
}

// -----------------------------------------------------------------------
// File meta roundtrip
// -----------------------------------------------------------------------

#[test]
fn test_file_meta_roundtrip() {
    let m = FileMetaEntry {
        lang_id: lang_to_id(rskim_core::Language::Rust),
        doc_length: 8192,
    };
    let encoded = encode_file_meta(&m);
    assert_eq!(encoded.len(), FILE_META_SIZE);
    let decoded = decode_file_meta(&encoded).unwrap();
    assert_eq!(decoded, m);
}

#[test]
fn test_file_meta_truncated() {
    let result = decode_file_meta(&[0u8; 2]);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("truncated"), "unexpected error: {err}");
}

// -----------------------------------------------------------------------
// Binary search
// -----------------------------------------------------------------------

fn make_entries(keys: &[u16]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(keys.len() * SKIDX_ENTRY_SIZE);
    for (i, &key) in keys.iter().enumerate() {
        let e = SkidxEntry {
            ngram_key: key,
            posting_offset: (i as u64) * 100,
            posting_length: 9,
        };
        buf.extend_from_slice(&encode_entry(&e));
    }
    buf
}

#[test]
fn test_lookup_ngram_found() {
    let keys = &[0x0100u16, 0x0200, 0x0300, 0x0400];
    let data = make_entries(keys);
    let result = lookup_ngram(&data, 0x0200).unwrap();
    assert!(result.is_some());
    let entry = result.unwrap();
    assert_eq!(entry.ngram_key, 0x0200);
    assert_eq!(entry.posting_offset, 100);
}

#[test]
fn test_lookup_ngram_not_found() {
    let keys = &[0x0100u16, 0x0300];
    let data = make_entries(keys);
    let result = lookup_ngram(&data, 0x0200).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_lookup_ngram_empty() {
    let result = lookup_ngram(&[], 0x1234).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_lookup_ngram_single_match() {
    let data = make_entries(&[0xABCDu16]);
    let result = lookup_ngram(&data, 0xABCD).unwrap();
    assert_eq!(result.unwrap().ngram_key, 0xABCD);
}

#[test]
fn test_lookup_ngram_bad_slice_length() {
    let result = lookup_ngram(&[0u8; 7], 0x0100);
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// BM25
// -----------------------------------------------------------------------

#[test]
fn test_bm25_positive_score() {
    let score = bm25_score(3.0, 5.0, 200, 300.0);
    assert!(score > 0.0, "BM25 should be positive for positive inputs");
}

#[test]
fn test_bm25_higher_tf_increases_score() {
    let low = bm25_score(1.0, 5.0, 200, 300.0);
    let high = bm25_score(10.0, 5.0, 200, 300.0);
    assert!(high > low, "higher tf should increase BM25 score");
}

#[test]
fn test_bm25_saturation() {
    // BM25 TF saturates — doubling tf from 100 to 200 should give < 2x score
    let s100 = bm25_score(100.0, 5.0, 200, 300.0);
    let s200 = bm25_score(200.0, 5.0, 200, 300.0);
    assert!(s200 < s100 * 2.0, "BM25 TF should saturate");
}

#[test]
fn test_bm25_shorter_doc_ranks_higher() {
    // Same tf but shorter document → higher BM25 score
    let short_doc = bm25_score(5.0, 4.0, 50, 300.0);
    let long_doc = bm25_score(5.0, 4.0, 1000, 300.0);
    assert!(
        short_doc > long_doc,
        "shorter doc should rank higher: {short_doc} vs {long_doc}"
    );
}

#[test]
fn test_bm25_zero_avg_doc_len_no_panic() {
    // avg_doc_len = 0 should not panic — treated as 1.0 internally
    let score = bm25_score(1.0, 2.0, 100, 0.0);
    assert!(score.is_finite(), "BM25 should return finite value");
}

