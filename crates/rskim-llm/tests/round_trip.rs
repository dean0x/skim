//! Round-trip byte-identity tests (AC3, AC4).
//!
//! For every corpus body, `serialize(parse(bytes)) == bytes`.
//! This covers adversarial cases: numbers, escapes, duplicate keys, whitespace,
//! unknown fields in non-alphabetical order, and bodies >64KB (PF-004).

// Test code legitimately uses panic/expect/unwrap for test failure reporting.
#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::unwrap_in_result
)]

use rskim_llm::{Provider, parse, parse_with_provider, serialize};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn assert_round_trip(bytes: &[u8], label: &str) {
    let body = parse(bytes).unwrap_or_else(|e| panic!("parse failed for {label}: {e}"));
    let out = serialize(&body).unwrap_or_else(|e| panic!("serialize failed for {label}: {e}"));
    assert_eq!(
        out,
        bytes,
        "round-trip mismatch for {label}:\nexpected: {}\ngot:      {}",
        String::from_utf8_lossy(bytes),
        String::from_utf8_lossy(&out)
    );
}

fn assert_round_trip_provider(bytes: &[u8], provider: Provider, label: &str) {
    let body = parse_with_provider(bytes, provider)
        .unwrap_or_else(|e| panic!("parse failed for {label}: {e}"));
    let out = serialize(&body).unwrap_or_else(|e| panic!("serialize failed for {label}: {e}"));
    assert_eq!(
        out,
        bytes,
        "round-trip mismatch for {label}:\nexpected: {}\ngot:      {}",
        String::from_utf8_lossy(bytes),
        String::from_utf8_lossy(&out)
    );
}

#[test]
fn ac3_anthropic_fixtures_round_trip() {
    let dir = fixtures_dir().join("anthropic");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    paths.sort();

    for path in paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(&path).unwrap();
        // Strip trailing newline added by editors
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
        assert_round_trip(bytes, &name);
    }
}

#[test]
fn ac3_openai_fixtures_round_trip() {
    let dir = fixtures_dir().join("openai");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    paths.sort();

    for path in paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(&path).unwrap();
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
        assert_round_trip_provider(bytes, Provider::OpenAi, &name);
    }
}

#[test]
fn ac3_adversarial_number_tokens() {
    // Non-canonical number tokens must survive round-trip unchanged.
    // serde_json::Value would reformat 1e3 -> 1000.0, 1.0 -> 1.0 (ok), -0.5e2 -> -50.0
    let cases = &[
        r#"{"model":"m","messages":[{"role":"user","content":"t"}],"max_tokens":1.0e3}"#,
        r#"{"model":"m","messages":[{"role":"user","content":"t"}],"max_tokens":1e3}"#,
        r#"{"model":"m","messages":[{"role":"user","content":"t"}],"max_tokens":1024.0}"#,
        r#"{"model":"m","messages":[{"role":"user","content":"t"}],"temperature":-0.5e-1}"#,
    ];
    for input in cases {
        assert_round_trip(input.as_bytes(), input);
    }
}

#[test]
fn ac3_unicode_escape_preservation() {
    // \uXXXX escapes vs literal characters must survive.
    // serde_json::Value would decode Hello to "Hello".
    let input = r#"{"model":"m","messages":[{"role":"user","content":"Hello"}],"max_tokens":100}"#;
    assert_round_trip(input.as_bytes(), "unicode_escapes");
}

#[test]
fn ac3_insignificant_whitespace_preserved() {
    // Internal whitespace (spaces around colons, inside objects) must survive.
    let input = r#"{ "model" : "m" , "messages" : [ { "role" : "user" , "content" : "hi" } ] , "max_tokens" : 100 }"#;
    assert_round_trip(input.as_bytes(), "whitespace");
}

#[test]
fn ac3_unknown_fields_order_preserved() {
    // Unknown fields in non-alphabetical order must preserve their order.
    // (z_field comes before a_field alphabetically reversed)
    let input = r#"{"model":"m","messages":[{"role":"user","content":"hi"}],"max_tokens":100,"z_unknown_field":"last","a_unknown_field":"first"}"#;
    assert_round_trip(input.as_bytes(), "field_order");
}

#[test]
fn ac3_large_body_round_trip() {
    // Body >64KB to catch offset-truncation bugs (PF-004).
    // Build a body with many tool_result messages totaling >64KB.
    let large_payload = "X".repeat(1000);
    let mut messages = Vec::new();
    for i in 0..80 {
        let msg = format!(
            r#"{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"call_{i}","content":"{large_payload}"}}]}}"#,
        );
        messages.push(msg);
    }
    let body = format!(
        r#"{{"model":"claude-3-5-sonnet-20241022","messages":[{}],"max_tokens":4096}}"#,
        messages.join(",")
    );
    let bytes = body.as_bytes();
    assert!(bytes.len() > 64 * 1024, "test body should be >64KB");
    assert_round_trip(bytes, "large_body_>64KB");
}

#[test]
fn ac4_byte_stability_double_run() {
    // Serializing the same model twice must produce identical bytes.
    // This covers: no hash-map iteration leaks, no RNG, no reformatting.
    //
    // Cross-process / cross-OS stability: the serialize() hot path is
    // `raw_bytes.clone()` — a verbatim Vec<u8> copy with no HashMap iteration,
    // RNG, or clock dependence — so output cannot differ between runs or OSes.
    // Cross-OS matrix coverage is delegated to #323's workspace CI matrix.
    let input = br#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":[{"type":"text","text":"Hello"},{"type":"tool_use","id":"t1","name":"search","input":{"q":"test"}}]}],"max_tokens":1024}"#;
    let body = parse(input).expect("parse failed");
    let out1 = serialize(&body).expect("serialize 1 failed");
    let out2 = serialize(&body).expect("serialize 2 failed");
    assert_eq!(out1, out2, "double-run serialization must be identical");
    // Also verify it equals the input (round-trip)
    assert_eq!(out1, input.as_ref(), "output must equal input");
}

#[test]
fn ac3_polymorphic_system_both_shapes() {
    // String system
    let string_system = r#"{"model":"m","system":"You are helpful.","messages":[{"role":"user","content":"Hi"}],"max_tokens":100}"#;
    assert_round_trip(string_system.as_bytes(), "system_string");

    // Array system
    let array_system = r#"{"model":"m","system":[{"type":"text","text":"You are helpful.","cache_control":{"type":"ephemeral"}}],"messages":[{"role":"user","content":"Hi"}],"max_tokens":100}"#;
    assert_round_trip(array_system.as_bytes(), "system_array");
}

#[test]
fn ac3_polymorphic_content_both_shapes() {
    // String content
    let string_content =
        r#"{"model":"m","messages":[{"role":"user","content":"Hello"}],"max_tokens":100}"#;
    assert_round_trip(string_content.as_bytes(), "content_string");

    // Array content
    let array_content = r#"{"model":"m","messages":[{"role":"user","content":[{"type":"text","text":"Hello"}]}],"max_tokens":100}"#;
    assert_round_trip(array_content.as_bytes(), "content_array");
}
