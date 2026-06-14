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
use rskim_llm::{ChunkIngestionBuilder, parse, serialize};

fn whole_body_parse_serialize(bytes: &[u8]) -> Vec<u8> {
    let body = parse(bytes).expect("whole-body parse failed");
    serialize(&body).expect("whole-body serialize failed")
}

fn chunk_parse_serialize(bytes: &[u8], chunk_size: usize) -> Vec<u8> {
    let mut builder = ChunkIngestionBuilder::new();
    for chunk in bytes.chunks(chunk_size.max(1)) {
        builder.push(chunk);
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
    builder.push(b"hello");
    assert_eq!(builder.len(), 5);
    assert!(!builder.is_empty());
    builder.push(b" world");
    assert_eq!(builder.len(), 11);
}

// Property test: any split point produces the same output.
// Uses a deterministic seed so the test is reproducible.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    #[test]
    fn ac7_proptest_random_splits(
        // Generate a split offset into the body
        split in 0usize..SIMPLE_BODY.len()
    ) {
        let expected = whole_body_parse_serialize(SIMPLE_BODY);

        // Two-chunk split at `split`
        let (first, second) = SIMPLE_BODY.split_at(split);
        let mut builder = ChunkIngestionBuilder::new();
        builder.push(first);
        builder.push(second);
        let body = builder.finish().expect("two-chunk parse failed");
        let actual = serialize(&body).expect("serialize failed");

        prop_assert_eq!(actual, expected);
    }
}
