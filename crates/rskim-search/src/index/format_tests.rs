//! Tests for the format codec (format.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

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

/// Format v2 indexes must be rejected with an actionable 'please rebuild' message.
///
/// This validates the ADR-006 invariant: old-format indexes are rejected cleanly
/// so the staleness check triggers a full rebuild, not corruption.
#[test]
fn test_v2_header_rejected_with_please_rebuild_message() {
    // Construct a well-formed 62-byte header but with version = 2 (old format).
    let h = SkidxHeader {
        magic: *SKIDX_MAGIC,
        version: 2, // old v2 format
        ngram_count: 0,
        file_count: 0,
        postings_file_size: 0,
        avg_doc_length: 0.0,
        avg_field_lengths: [0.0; 8],
        checksum: 0,
    };
    let encoded = encode_header(&h);
    // decode_header must NOT accept version 2 — FORMAT_VERSION is now 4.
    let result = decode_header(&encoded);
    assert!(result.is_err(), "v2 header must be rejected by v4 reader");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("please rebuild"),
        "v2 rejection must include 'please rebuild': {err}"
    );
    assert!(
        err.contains("format version") || err.contains("unsupported"),
        "v2 rejection must mention format version: {err}"
    );
}

/// Format v3 indexes must be rejected with an actionable 'please rebuild' message.
///
/// AC3 / ADR-006: After the v3→v4 format bump (#358 Item 2), the v4 reader
/// must reject v3 indexes cleanly so the staleness check triggers a full
/// rebuild — the old index is NOT corrupted, just incompatible.
///
/// PF-007 compliance: asserts BOTH discriminating substrings
/// ("unsupported format version" AND "please rebuild") so the test fails if
/// either message is missing, not just if decode_header() returns Ok(()).
#[test]
fn test_v3_header_rejected_with_please_rebuild_message() {
    // Construct a well-formed 62-byte header but with version = 3 (pre-v4 format).
    let h = SkidxHeader {
        magic: *SKIDX_MAGIC,
        version: 3, // old v3 format (pre-varint posting compression)
        ngram_count: 0,
        file_count: 0,
        postings_file_size: 0,
        avg_doc_length: 0.0,
        avg_field_lengths: [0.0; 8],
        checksum: 0,
    };
    let encoded = encode_header(&h);
    // decode_header must NOT accept version 3 — FORMAT_VERSION is now 4.
    let result = decode_header(&encoded);
    assert!(result.is_err(), "v3 header must be rejected by v4 reader");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("please rebuild"),
        "v3 rejection must include 'please rebuild' (actionable per ADR-006): {err}"
    );
    assert!(
        err.contains("format version") || err.contains("unsupported"),
        "v3 rejection must mention 'format version' or 'unsupported': {err}"
    );
}

/// Verify the v3 SkidxEntry size constant is 16 bytes.
#[test]
fn test_entry_size_is_16_bytes() {
    assert_eq!(
        SKIDX_ENTRY_SIZE, 16,
        "v3 SkidxEntry must be 16 bytes (ngram_key widened to u32)"
    );
    let e = SkidxEntry {
        ngram_key: 0x0066_6E20u32, // "fn " trigram
        posting_offset: 0,
        posting_length: 0,
    };
    let encoded = encode_entry(&e);
    assert_eq!(encoded.len(), 16, "encoded entry must be exactly 16 bytes");
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
    // v3: ngram_key is u32 (trigram: (b1<<16)|(b2<<8)|b3 = 0x68_65_6C = "hel")
    let e = SkidxEntry {
        ngram_key: 0x0068656C, // "hel" trigram
        posting_offset: 4096,
        posting_length: 27,
    };
    let encoded = encode_entry(&e);
    assert_eq!(encoded.len(), SKIDX_ENTRY_SIZE, "v3 entry must be 16 bytes");
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

/// Verify the v3 header size constant matches actual encoded size (unchanged from v2).
#[test]
fn test_header_size_is_62_bytes() {
    assert_eq!(
        SKIDX_HEADER_SIZE, 62,
        "v3 header must be 62 bytes (unchanged from v2)"
    );
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

/// Verify the FileMetaEntry size constant matches actual encoded size (unchanged from v2 → v3).
#[test]
fn test_file_meta_size_is_37_bytes() {
    assert_eq!(
        FILE_META_SIZE, 37,
        "v2/v3 FileMetaEntry must be 37 bytes (unchanged by #355)"
    );
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
    assert!(
        result.is_err(),
        "expected Err for field_lengths sum mismatch"
    );
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
    assert!(
        result.is_err(),
        "infinity avg_doc_length should be rejected"
    );
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
        "infinity avg_field_lengths[3] should be rejected"
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

fn make_entries(keys: &[u32]) -> Vec<u8> {
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
    // v3: u32 trigram keys.
    let keys = &[0x0001_0000u32, 0x0002_0000, 0x0003_0000, 0x0004_0000];
    let data = make_entries(keys);
    let result = lookup_ngram(&data, 0x0002_0000).unwrap();
    assert!(result.is_some());
    let entry = result.unwrap();
    assert_eq!(entry.ngram_key, 0x0002_0000);
    assert_eq!(entry.posting_offset, 100);
}

#[test]
fn test_lookup_ngram_not_found() {
    let keys = &[0x0001_0000u32, 0x0003_0000];
    let data = make_entries(keys);
    let result = lookup_ngram(&data, 0x0002_0000).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_lookup_ngram_empty() {
    let result = lookup_ngram(&[], 0x1234_5678u32).unwrap();
    assert!(result.is_none());
}

#[test]
fn test_lookup_ngram_single_match() {
    let data = make_entries(&[0x00AB_CDEFu32]);
    let result = lookup_ngram(&data, 0x00AB_CDEF).unwrap();
    assert_eq!(result.unwrap().ngram_key, 0x00AB_CDEF);
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

// -----------------------------------------------------------------------
// v4 varint codec (encode_varint / decode_varint)
// -----------------------------------------------------------------------

/// Varint round-trip for a range of representative u32 values.
///
/// AC4 precursor: verifies that the varint primitive round-trips correctly
/// before testing the full posting codec built on top of it.
#[test]
fn test_varint_roundtrip_representative_values() {
    let values: &[u32] = &[
        0,
        1,
        127,
        128, // first 2-byte value
        255,
        16383,
        16384, // first 3-byte value
        2097151,
        2097152, // first 4-byte value
        268435455,
        268435456, // first 5-byte value
        u32::MAX,
    ];
    for &v in values {
        let mut buf = Vec::new();
        let written = encode_varint(v, &mut buf);
        assert!(
            (1..=5).contains(&written),
            "varint for {v} must be 1-5 bytes, got {written}"
        );
        let (decoded, consumed) = decode_varint(&buf, 0).unwrap();
        assert_eq!(decoded, v, "varint round-trip failed for {v}");
        assert_eq!(consumed, written, "consumed bytes mismatch for {v}");
    }
}

/// decode_varint must return IndexCorrupted for a truncated input.
#[test]
fn test_varint_decode_truncated_returns_err() {
    // A continuation byte (MSB set) with no following byte — truncated.
    let buf = [0x80u8]; // says "more bytes follow" but none are present
    let result = decode_varint(&buf, 0);
    assert!(result.is_err(), "truncated varint must return Err");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("truncated"),
        "error should mention truncated: {err}"
    );
}

/// decode_varint must return IndexCorrupted for a 6-byte (overflow) varint.
#[test]
fn test_varint_decode_overflow_returns_err() {
    // Six consecutive continuation bytes — exceeds the 5-byte u32 maximum.
    let buf = [0x80u8, 0x80, 0x80, 0x80, 0x80, 0x00];
    let result = decode_varint(&buf, 0);
    assert!(result.is_err(), "6-byte varint must return Err (overflow)");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("overflow"),
        "error should mention overflow: {err}"
    );
}

/// encode_varint / decode_varint at a non-zero offset.
#[test]
fn test_varint_decode_at_offset() {
    let mut buf = vec![0xFFu8, 0xFFu8]; // 2 bytes of prefix garbage
    encode_varint(300, &mut buf); // 300 = 0x12C → 2-byte varint: [0xAC, 0x02]
    let (val, n) = decode_varint(&buf, 2).unwrap();
    assert_eq!(val, 300, "varint at offset 2 should decode to 300");
    assert_eq!(n, 2, "300 encodes to 2 bytes");
}

// -----------------------------------------------------------------------
// v4 posting codec (encode_postings_varint / decode_postings_varint)
// AC4: byte-faithful round-trip across empty / single / multi-doc / max-gap
// -----------------------------------------------------------------------

/// Helper: encode a list of PostingEntry values and decode them back.
fn posting_roundtrip(postings: &[PostingEntry]) -> Vec<PostingEntry> {
    let mut buf = Vec::new();
    encode_postings_varint(postings, &mut buf);
    decode_postings_varint(&buf).unwrap()
}

/// AC4 — empty posting list round-trips cleanly to an empty Vec.
#[test]
fn test_posting_codec_empty_list() {
    let result = posting_roundtrip(&[]);
    assert!(
        result.is_empty(),
        "empty posting list should round-trip to empty"
    );
}

/// AC4 — single-entry posting list round-trips exactly.
#[test]
fn test_posting_codec_single_entry() {
    let p = PostingEntry {
        doc_id: 42,
        field_id: crate::SearchField::FunctionSignature.discriminant(),
        position: 1024,
    };
    let decoded = posting_roundtrip(&[p]);
    assert_eq!(
        decoded.len(),
        1,
        "single-entry round-trip must return 1 entry"
    );
    assert_eq!(decoded[0], p, "single-entry must decode to exact input");
}

/// AC4 — multi-doc posting list with 3+ entries spanning multiple doc_ids.
///
/// This is the primary AC4 discriminating assertion: encodes a posting list
/// with multiple distinct doc_ids, decodes it, and asserts exact
/// (doc_id, field_id, position) tuple match for every entry.
#[test]
fn test_posting_codec_multi_doc_roundtrip() {
    let postings = vec![
        PostingEntry {
            doc_id: 0,
            field_id: crate::SearchField::TypeDefinition.discriminant(),
            position: 0,
        },
        PostingEntry {
            doc_id: 0,
            field_id: crate::SearchField::FunctionSignature.discriminant(),
            position: 5,
        },
        PostingEntry {
            doc_id: 1,
            field_id: crate::SearchField::Other.discriminant(),
            position: 10,
        },
        PostingEntry {
            doc_id: 3,
            field_id: crate::SearchField::TypeDefinition.discriminant(),
            position: 100,
        },
        PostingEntry {
            doc_id: 3,
            field_id: crate::SearchField::Other.discriminant(),
            position: 200,
        },
    ];
    let decoded = posting_roundtrip(&postings);
    assert_eq!(
        decoded.len(),
        postings.len(),
        "multi-doc round-trip must preserve entry count"
    );
    for (i, (got, want)) in decoded.iter().zip(postings.iter()).enumerate() {
        assert_eq!(
            got, want,
            "entry[{i}] mismatch: got {:?}, want {:?}",
            got, want
        );
    }
}

/// AC4 — posting list with maximum doc_id gap (u32::MAX - 0 = u32::MAX).
///
/// Verifies that wrapping_add arithmetic in the decoder handles the largest
/// possible delta without panicking or silently truncating.
#[test]
fn test_posting_codec_max_gap_docid() {
    let postings = vec![
        PostingEntry {
            doc_id: 0,
            field_id: 0,
            position: 0,
        },
        PostingEntry {
            doc_id: u32::MAX,
            field_id: 0,
            position: 0,
        },
    ];
    let decoded = posting_roundtrip(&postings);
    assert_eq!(
        decoded.len(),
        2,
        "max-gap posting list must round-trip to 2 entries"
    );
    assert_eq!(decoded[0].doc_id, 0, "first entry doc_id must be 0");
    assert_eq!(
        decoded[1].doc_id,
        u32::MAX,
        "second entry doc_id must be u32::MAX"
    );
}

/// AC4 — posting list with large position values.
#[test]
fn test_posting_codec_large_positions() {
    let postings = vec![
        PostingEntry {
            doc_id: 5,
            field_id: 0,
            position: 0,
        },
        PostingEntry {
            doc_id: 5,
            field_id: 0,
            position: 1_000_000,
        },
        PostingEntry {
            doc_id: 5,
            field_id: 0,
            position: u32::MAX,
        },
    ];
    let decoded = posting_roundtrip(&postings);
    assert_eq!(decoded.len(), 3);
    assert_eq!(decoded[0].position, 0);
    assert_eq!(decoded[1].position, 1_000_000);
    assert_eq!(decoded[2].position, u32::MAX);
}

/// AC4 / Finding 1+3 — cross-field position-decrease round-trip within the same doc.
///
/// Verifies that the encoder resets `prev_position` when `field_id` changes
/// (even if `doc_id` is unchanged) so that the first position in field N+1
/// does NOT produce a near-u32::MAX delta when it is lower than the last
/// position of field N.
///
/// Example scenario: within doc 0, field TypeDefinition (=0) covers bytes
/// 200..300 (position 250), then field Other (=7) covers bytes 0..200
/// (position 10).  Sorted by (doc_id, field_id, position) the TypeDefinition
/// entry (pos 250) comes before the Other entry (pos 10).  Without the
/// field-boundary reset, the encoder would compute `10.wrapping_sub(250)` =
/// 4294967056 and emit a 5-byte varint — worst case, defeating compression.
/// With the fix, `prev_position` is reset to 0 on the field boundary so
/// `delta_position = 10 - 0 = 10`, which encodes as a 1-byte varint.
///
/// PF-007 compliance: the assertion checks the exact decoded (doc_id, field_id,
/// position) tuples, not just that decode returns Ok.  A round-trip that silently
/// wraps-and-recovers would still pass without this exact-value check.
#[test]
fn test_posting_codec_cross_field_position_decrease_roundtrip() {
    // doc_id = 0, field TypeDefinition (discriminant 0), high position (250)
    // doc_id = 0, field Other (discriminant 7), lower position (10)
    // Sorted by (doc_id, field_id, position):
    //   (0, 0=TypeDefinition, 250) < (0, 7=Other, 10)
    // field_id 0 < field_id 7, so TypeDefinition entry comes first despite
    // having a higher byte position than the Other entry.
    let td = crate::SearchField::TypeDefinition.discriminant(); // 0
    let other = crate::SearchField::Other.discriminant(); // 7
    let postings = vec![
        PostingEntry {
            doc_id: 0,
            field_id: td,
            position: 250,
        },
        PostingEntry {
            doc_id: 0,
            field_id: other,
            position: 10,
        },
    ];
    // Verify the input is sorted (as the builder would produce it).
    assert!(
        postings[0] < postings[1],
        "test invariant: postings must be sorted ascending by (doc_id, field_id, position)"
    );

    let decoded = posting_roundtrip(&postings);
    assert_eq!(
        decoded.len(),
        2,
        "cross-field position-decrease must round-trip to 2 entries"
    );
    assert_eq!(
        decoded[0],
        postings[0],
        "first entry (TypeDefinition, pos=250) must decode exactly"
    );
    assert_eq!(
        decoded[1],
        postings[1],
        "second entry (Other, pos=10) must decode exactly after cross-field reset"
    );
}

/// decode_postings_varint must return IndexCorrupted for an invalid field_id.
#[test]
fn test_posting_codec_invalid_field_id_returns_err() {
    let p = PostingEntry {
        doc_id: 0,
        field_id: 200, // invalid — not a valid SearchField discriminant
        position: 0,
    };
    let mut buf = Vec::new();
    encode_postings_varint(&[p], &mut buf);
    // field_id=200 is stored verbatim; decoder must reject it.
    let result = decode_postings_varint(&buf);
    assert!(result.is_err(), "invalid field_id must cause decode error");
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("field_id"),
        "error should mention field_id: {err}"
    );
}

/// Finding 4 (AC4 error-path gap): decode_postings_varint returns IndexCorrupted
/// when a valid doc_id varint is decoded but no field_id byte follows
/// ("truncated before field_id" branch in decode_postings_varint).
///
/// This is the posting-walker's distinct truncation branch that was not covered
/// by the existing varint-primitive test (`test_varint_decode_truncated_returns_err`,
/// which truncates INSIDE the varint, not AFTER it).  The production decode path
/// used by `reader.rs::lookup_postings` calls `decode_postings_varint` directly;
/// a corrupt or truncated `.skpost` slice that ends exactly after a doc_id varint
/// hits this branch.
///
/// PF-007 compliance: asserts the discriminating error substring "field_id" OR
/// "truncated" so the test fails the moment the branch is removed or the error
/// message changes semantically.
#[test]
fn test_posting_codec_truncated_before_field_id_returns_err() {
    // Manually construct a buffer that contains a valid 1-byte doc_id varint
    // (value 0x01 = delta 1, MSB clear = terminal byte) followed by NO field_id
    // byte.  This simulates a .skpost slice truncated mid-entry after the
    // doc_id varint but before the field_id byte.
    //
    // Layout per entry: [varint delta_doc_id][u8 field_id][varint delta_position]
    // We emit [0x01] and stop — the doc_id varint is complete (MSB=0, value=1)
    // but field_id is absent, hitting the "truncated before field_id" branch.
    let buf = vec![0x01u8]; // valid 1-byte varint (doc_id delta=1), no field_id follows
    let result = decode_postings_varint(&buf);
    assert!(
        result.is_err(),
        "truncated-before-field_id slice must return Err, got Ok"
    );
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("field_id") || err.contains("truncated"),
        "error must mention 'field_id' or 'truncated': {err}"
    );
}
