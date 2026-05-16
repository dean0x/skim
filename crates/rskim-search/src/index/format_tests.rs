//! Tests for the format codec (format.rs).

#![allow(clippy::unwrap_used)]

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
        avg_field_lengths: [10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0],
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
    for (i, (&a, &b)) in decoded
        .avg_field_lengths
        .iter()
        .zip(h.avg_field_lengths.iter())
        .enumerate()
    {
        assert!(
            (a - b).abs() < f32::EPSILON,
            "avg_field_lengths[{i}] mismatch: {a} vs {b}"
        );
    }
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
        avg_field_lengths: [0.0; 8],
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
        avg_field_lengths: [0.0; 8],
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

/// Format v1 indexes (old header size 30 bytes) must be rejected with a clear error.
#[test]
fn test_v1_header_rejected_with_format_version_message() {
    // Simulate a v1-style header (30 bytes) — will fail both size check and version check.
    // We write the magic correctly to get past magic check and hit the version rejection.
    let mut buf = vec![0u8; 30]; // v1 header size
    buf[0..4].copy_from_slice(b"SKIX"); // correct magic
    buf[4..6].copy_from_slice(&1u16.to_le_bytes()); // version = 1
    // Truncated header — size check fires first.
    let result = decode_header(&buf);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    // Either truncated or version error — both are acceptable rejections.
    assert!(
        err.contains("truncated") || err.contains("format version"),
        "v1 index should be rejected with truncated or format version error: {err}"
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
        field_lengths: [100, 200, 300, 400, 500, 600, 700, 5392],
    };
    let encoded = encode_file_meta(&m);
    assert_eq!(encoded.len(), FILE_META_SIZE);
    let decoded = decode_file_meta(&encoded).unwrap();
    assert_eq!(decoded, m);
}

/// Verify that the field_lengths invariant is encoded correctly.
#[test]
fn test_file_meta_field_lengths_encode_decode() {
    let field_lengths = [10u32, 20, 30, 40, 50, 60, 70, 80];
    let total: u32 = field_lengths.iter().sum();
    let m = FileMetaEntry {
        lang_id: lang_to_id(rskim_core::Language::TypeScript),
        doc_length: total,
        field_lengths,
    };
    let encoded = encode_file_meta(&m);
    let decoded = decode_file_meta(&encoded).unwrap();
    assert_eq!(decoded.field_lengths, field_lengths);
    assert_eq!(decoded.doc_length, total);
}

/// Verify the v2 header size constant matches actual encoded size.
#[test]
fn test_header_size_is_62_bytes() {
    assert_eq!(SKIDX_HEADER_SIZE, 62, "v2 header must be 62 bytes");
    let h = SkidxHeader {
        magic: *SKIDX_MAGIC,
        version: FORMAT_VERSION,
        ngram_count: 0,
        file_count: 0,
        postings_file_size: 0,
        avg_doc_length: 0.0,
        avg_field_lengths: [0.0; 8],
        checksum: 0,
    };
    let encoded = encode_header(&h);
    assert_eq!(encoded.len(), 62);
}

/// Verify the v2 FileMetaEntry size constant matches actual encoded size.
#[test]
fn test_file_meta_size_is_37_bytes() {
    assert_eq!(FILE_META_SIZE, 37, "v2 FileMetaEntry must be 37 bytes");
    let m = FileMetaEntry {
        lang_id: 0,
        doc_length: 0,
        field_lengths: [0; 8],
    };
    let encoded = encode_file_meta(&m);
    assert_eq!(encoded.len(), 37);
}

#[test]
fn test_file_meta_truncated() {
    let result = decode_file_meta(&[0u8; 2]);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(err.contains("truncated"), "unexpected error: {err}");
}

/// decode_file_meta must reject entries where field_lengths sum != doc_length.
#[test]
fn test_file_meta_field_lengths_sum_mismatch_rejected() {
    let m = FileMetaEntry {
        lang_id: 0,
        doc_length: 1000,
        // Sum is 100+200 = 300, not 1000 — deliberate mismatch.
        field_lengths: [100, 200, 0, 0, 0, 0, 0, 0],
    };
    let encoded = encode_file_meta(&m);
    let result = decode_file_meta(&encoded);
    assert!(result.is_err(), "expected Err for field_lengths sum mismatch");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("field_lengths sum"),
        "unexpected error message: {err}"
    );
}

// -----------------------------------------------------------------------
// NaN / Infinity rejection in decode_header
// -----------------------------------------------------------------------

/// Helper: build a valid 62-byte header buffer then patch 4 bytes at `offset`
/// with the given f32 bit pattern.
fn make_header_with_float_at(offset: usize, value: f32) -> Vec<u8> {
    let h = SkidxHeader {
        magic: *SKIDX_MAGIC,
        version: FORMAT_VERSION,
        ngram_count: 0,
        file_count: 0,
        postings_file_size: 0,
        avg_doc_length: 0.0,
        avg_field_lengths: [0.0; 8],
        checksum: 0,
    };
    let mut buf = encode_header(&h).to_vec();
    buf[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    buf
}

#[test]
fn test_decode_header_rejects_nan_avg_doc_length() {
    // avg_doc_length is at bytes [22..26]
    let buf = make_header_with_float_at(22, f32::NAN);
    let result = decode_header(&buf);
    assert!(result.is_err(), "NaN avg_doc_length should be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("avg_doc_length"),
        "error should mention avg_doc_length: {err}"
    );
}

#[test]
fn test_decode_header_rejects_infinity_avg_doc_length() {
    let buf = make_header_with_float_at(22, f32::INFINITY);
    let result = decode_header(&buf);
    assert!(result.is_err(), "INFINITY avg_doc_length should be rejected");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("avg_doc_length"),
        "error should mention avg_doc_length: {err}"
    );
}

#[test]
fn test_decode_header_rejects_negative_avg_doc_length() {
    let buf = make_header_with_float_at(22, -1.0f32);
    let result = decode_header(&buf);
    assert!(
        result.is_err(),
        "negative avg_doc_length should be rejected"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("avg_doc_length"),
        "error should mention avg_doc_length: {err}"
    );
}

#[test]
fn test_decode_header_rejects_nan_avg_field_length() {
    // avg_field_lengths start at byte 26; field 0 is at bytes [26..30]
    let buf = make_header_with_float_at(26, f32::NAN);
    let result = decode_header(&buf);
    assert!(
        result.is_err(),
        "NaN avg_field_lengths[0] should be rejected"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("avg_field_lengths"),
        "error should mention avg_field_lengths: {err}"
    );
}

#[test]
fn test_decode_header_rejects_infinity_avg_field_length() {
    // avg_field_lengths[3] is at bytes [26 + 3*4 .. 26 + 3*4 + 4] = [38..42]
    let buf = make_header_with_float_at(38, f32::INFINITY);
    let result = decode_header(&buf);
    assert!(
        result.is_err(),
        "INFINITY avg_field_lengths[3] should be rejected"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("avg_field_lengths"),
        "error should mention avg_field_lengths: {err}"
    );
}

#[test]
fn test_decode_header_rejects_negative_avg_field_length() {
    // avg_field_lengths[7] is at bytes [26 + 7*4 .. 26 + 7*4 + 4] = [54..58]
    let buf = make_header_with_float_at(54, -0.5f32);
    let result = decode_header(&buf);
    assert!(
        result.is_err(),
        "negative avg_field_lengths[7] should be rejected"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("avg_field_lengths"),
        "error should mention avg_field_lengths: {err}"
    );
}

// -----------------------------------------------------------------------
// read_array — defensive bounds checking
// -----------------------------------------------------------------------

/// Verify read_array returns Err (not panic) when start + N exceeds data.len().
#[test]
fn test_read_array_out_of_bounds_returns_err() {
    // 3 bytes of data; asking for 4 bytes at offset 0 must fail gracefully.
    let data = [0u8; 3];
    let result: crate::Result<[u8; 4]> = read_array(&data, 0, "test");
    assert!(result.is_err(), "expected Err on out-of-bounds read_array");
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("need 4 bytes at offset 0"),
        "unexpected error message: {msg}"
    );
}

/// Verify read_array returns Err (not panic) when start itself is beyond data.
#[test]
fn test_read_array_start_beyond_data_returns_err() {
    let data = [0u8; 2];
    let result: crate::Result<[u8; 4]> = read_array(&data, 10, "test");
    assert!(
        result.is_err(),
        "expected Err on start-beyond-data read_array"
    );
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
