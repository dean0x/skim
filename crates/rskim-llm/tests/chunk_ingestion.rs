//! Chunked ingestion tests (AC7).
//!
//! Feeding any corpus body via the chunk-ingestion API as 1-byte chunks, 4KB chunks,
//! and randomly-sized chunks MUST produce serialized output byte-identical to
//! whole-body parsing.

// Test code legitimately uses panic/expect/unwrap for test failure reporting.
#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    unused_doc_comments
)]

use proptest::prelude::*;
use rskim_llm::{ChunkIngestionBuilder, LlmError, parse, serialize};

fn whole_body_parse_serialize(bytes: &[u8]) -> Vec<u8> {
    let body = parse(bytes).expect("whole-body parse failed");
    serialize(&body).expect("whole-body serialize failed")
}

fn chunk_parse_serialize(bytes: &[u8], chunk_size: usize) -> Vec<u8> {
    let mut builder = ChunkIngestionBuilder::new();
    for chunk in bytes.chunks(chunk_size.max(1)) {
        builder.push(chunk).expect("push failed");
    }
    let body = builder.finish().expect("chunk parse failed");
    serialize(&body).expect("chunk serialize failed")
}

const SIMPLE_BODY: &[u8] = br#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"Hello, world!"},{"role":"assistant","content":"Hi there!"}],"max_tokens":1024}"#;

#[test]
fn ac7_chunk_1_byte() {
    let expected = whole_body_parse_serialize(SIMPLE_BODY);
    let actual = chunk_parse_serialize(SIMPLE_BODY, 1);
    assert_eq!(
        actual, expected,
        "1-byte chunks must equal whole-body parse"
    );
}

#[test]
fn ac7_chunk_4kb() {
    let expected = whole_body_parse_serialize(SIMPLE_BODY);
    let actual = chunk_parse_serialize(SIMPLE_BODY, 4096);
    assert_eq!(actual, expected, "4KB chunks must equal whole-body parse");
}

#[test]
fn ac7_chunk_13_bytes() {
    // Odd chunk size to catch boundary issues
    let expected = whole_body_parse_serialize(SIMPLE_BODY);
    let actual = chunk_parse_serialize(SIMPLE_BODY, 13);
    assert_eq!(
        actual, expected,
        "13-byte chunks must equal whole-body parse"
    );
}

#[test]
fn ac7_large_body_chunked() {
    // Build a ~10KB body and chunk it at various sizes
    let payload = "Z".repeat(500);
    let mut messages = Vec::new();
    for i in 0..10 {
        messages.push(format!(
            r#"{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"c{i}","content":"{payload}"}}]}}"#,
        ));
    }
    let body_str = format!(
        r#"{{"model":"claude-3-5-sonnet-20241022","messages":[{}],"max_tokens":2048}}"#,
        messages.join(",")
    );
    let body_bytes = body_str.as_bytes();

    let expected = whole_body_parse_serialize(body_bytes);

    for chunk_size in &[1, 7, 64, 512, 4096] {
        let actual = chunk_parse_serialize(body_bytes, *chunk_size);
        assert_eq!(
            actual, expected,
            "chunk_size={chunk_size} must equal whole-body parse"
        );
    }
}

#[test]
fn ac7_builder_len_tracking() {
    let mut builder = ChunkIngestionBuilder::new();
    assert!(builder.is_empty());
    assert_eq!(builder.len(), 0);
    builder.push(b"hello").expect("push failed");
    assert_eq!(builder.len(), 5);
    assert!(!builder.is_empty());
    builder.push(b" world").expect("push failed");
    assert_eq!(builder.len(), 11);
}

/// AC7 / OWASP A04 — body-too-large guard: push() returns BodyTooLarge before the
/// buffer exceeds the configured limit.  No bytes are accumulated after the error.
#[test]
fn ac7_body_too_large_returns_error() {
    // Set a tiny limit of 10 bytes so we can test with small inputs.
    let mut builder = ChunkIngestionBuilder::new().with_max_bytes(10);
    // Push 8 bytes — still within limit.
    builder
        .push(b"12345678")
        .expect("first push should succeed");
    assert_eq!(builder.len(), 8);
    // Push 3 more bytes — total 11, exceeds limit of 10.
    let err = builder
        .push(b"abc")
        .expect_err("push should fail when limit is exceeded");
    match err {
        LlmError::BodyTooLarge(n) => {
            assert!(
                n > 10,
                "BodyTooLarge should report the attempted total size; got {n}"
            );
        }
        other => panic!("expected BodyTooLarge, got: {other}"),
    }
    // Buffer must NOT have grown — the failed push is atomic.
    assert_eq!(
        builder.len(),
        8,
        "buffer must not grow after a BodyTooLarge error"
    );
}

/// AC7 — default max_bytes (64 MiB) allows normal usage.
/// A body larger than a typical single message (~1 MB of text payload) must succeed.
#[test]
fn ac7_default_max_bytes_allows_large_body() {
    // Build a body with ~1 MB of text payload so the total JSON is well above the
    // range of real-world bodies but far below the 64 MiB default limit.
    let payload = "X".repeat(1_000_000); // 1 MB payload string
    let body_str = format!(
        r#"{{"model":"claude-3-5-sonnet-20241022","messages":[{{"role":"user","content":"{payload}"}}],"max_tokens":1}}"#
    );
    let bytes = body_str.as_bytes();
    assert!(
        bytes.len() > 1_000_000,
        "test body should be >1MB; got {} bytes",
        bytes.len()
    );
    assert!(
        bytes.len() < 64 * 1024 * 1024,
        "test body must be within the default limit; got {} bytes",
        bytes.len()
    );

    let mut builder = ChunkIngestionBuilder::new();
    for chunk in bytes.chunks(4096) {
        builder
            .push(chunk)
            .expect("push should not fail for a normal body within the default 64 MiB limit");
    }
    let body = builder.finish().expect("finish should succeed");
    let out = rskim_llm::serialize(&body).expect("serialize failed");
    assert_eq!(out, bytes, "round-trip must be byte-identical");
}

// Property tests: any split strategy produces the same output as whole-body parsing.
// Two-chunk (single split point) and multi-chunk (random sequence of chunk sizes).
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// AC7: single split point — body split into exactly two chunks at a random offset.
    #[test]
    fn ac7_proptest_two_chunk_split(
        // Generate a split offset into the body
        split in 0usize..SIMPLE_BODY.len()
    ) {
        let expected = whole_body_parse_serialize(SIMPLE_BODY);

        let (first, second) = SIMPLE_BODY.split_at(split);
        let mut builder = ChunkIngestionBuilder::new();
        builder.push(first).expect("push failed");
        builder.push(second).expect("push failed");
        let body = builder.finish().expect("two-chunk parse failed");
        let actual = serialize(&body).expect("serialize failed");

        prop_assert_eq!(actual, expected);
    }

    /// AC7: multi-chunk split — body chopped into N chunks of randomly-varying sizes.
    ///
    /// This exercises the boundary behavior the AC7 criterion describes: "randomly-sized
    /// chunks … random split strategies" (AC7/Criterion-7).  The two-chunk test above
    /// only probes one split point per run; this test generates a *sequence* of chunk
    /// sizes so that every byte boundary within the body may become a chunk boundary
    /// in some run.
    #[test]
    fn ac7_proptest_multi_chunk_splits(
        // Between 1 and 20 chunk sizes, each 1..64 bytes.
        chunk_sizes in proptest::collection::vec(1usize..=64, 1..=20),
    ) {
        let expected = whole_body_parse_serialize(SIMPLE_BODY);

        let mut builder = ChunkIngestionBuilder::new();
        let mut offset = 0;
        for size in &chunk_sizes {
            let end = (offset + size).min(SIMPLE_BODY.len());
            if offset >= SIMPLE_BODY.len() {
                break;
            }
            builder.push(&SIMPLE_BODY[offset..end]).expect("push failed");
            offset = end;
        }
        // Push any remaining bytes not covered by the chunk_sizes sequence
        if offset < SIMPLE_BODY.len() {
            builder.push(&SIMPLE_BODY[offset..]).expect("push tail failed");
        }
        let body = builder.finish().expect("multi-chunk parse failed");
        let actual = serialize(&body).expect("serialize failed");

        prop_assert_eq!(actual, expected);
    }
}
