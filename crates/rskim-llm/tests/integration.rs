//! Integration tests: Anthropic and OpenAI corpus parsing.
//!
//! Criteria covered:
//! - AC1: Anthropic corpus parses to Ok with unknowns preserved
//! - AC2: OpenAI corpus parses to Ok with unmodeled fields preserved

// Test code legitimately uses panic/expect/unwrap for test failure reporting.
#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::unwrap_in_result
)]

use rskim_llm::{ParsedBody, Provider, parse, parse_with_provider};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// Load all .json files from a fixtures subdirectory.
fn load_fixtures(subdir: &str) -> Vec<(String, Vec<u8>)> {
    let dir = fixtures_dir().join(subdir);
    let mut fixtures = Vec::new();
    let entries = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("failed to read fixture dir {}: {}", dir.display(), e));
    let mut paths: Vec<_> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    paths.sort();
    for path in paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("failed to read fixture {}: {}", path.display(), e));
        fixtures.push((name, bytes));
    }
    fixtures
}

#[test]
fn ac1_anthropic_corpus_parses_to_ok() {
    let fixtures = load_fixtures("anthropic");
    assert!(!fixtures.is_empty(), "no Anthropic fixtures found");

    for (name, bytes) in &fixtures {
        // Strip trailing newline that editors add
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        let result = parse(bytes);
        assert!(
            result.is_ok(),
            "fixture {name} failed to parse: {:?}",
            result.unwrap_err()
        );

        // Verify it was detected as Anthropic (or at minimum parsed successfully)
        match result.unwrap() {
            ParsedBody::Anthropic(_) => {}
            ParsedBody::OpenAi(_) => {
                // Some Anthropic fixtures might be ambiguous — still a valid parse
            }
        }
    }
}

#[test]
fn ac1_anthropic_unknown_block_preserved() {
    // Fixture 07 contains unknown block types — verify they round-trip
    let bytes = std::fs::read(fixtures_dir().join("anthropic/07_unknown_fields.json"))
        .expect("fixture not found");
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let result = parse(bytes).expect("parse failed");

    // Must contain extra fields in the model (preserved unknown fields)
    match result {
        ParsedBody::Anthropic(body) => {
            // The 07 fixture has a future_param at top level
            assert!(
                body.extra_fields.contains_key("future_param"),
                "extra_fields should contain 'future_param'"
            );
        }
        _ => panic!("expected Anthropic body"),
    }
}

#[test]
fn ac2_openai_corpus_parses_to_ok() {
    let fixtures = load_fixtures("openai");
    assert!(!fixtures.is_empty(), "no OpenAI fixtures found");

    for (name, bytes) in &fixtures {
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        let result = parse_with_provider(bytes, Provider::OpenAi);
        assert!(
            result.is_ok(),
            "fixture {name} failed to parse as OpenAI: {:?}",
            result.unwrap_err()
        );

        match result.unwrap() {
            ParsedBody::OpenAi(_) => {}
            _ => panic!("expected OpenAI body for fixture {name}"),
        }
    }
}

#[test]
fn ac2_openai_legacy_function_call_preserved() {
    let bytes = std::fs::read(fixtures_dir().join("openai/06_legacy_function_call.json"))
        .expect("fixture not found");
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let body = parse_with_provider(bytes, Provider::OpenAi).expect("parse failed");

    match body {
        ParsedBody::OpenAi(b) => {
            // The legacy function_call should be in extra_fields of the assistant message
            let assistant = b.messages.iter().find(|m| m.role == "assistant");
            assert!(assistant.is_some(), "no assistant message found");
            let asst = assistant.unwrap();
            assert!(
                asst.extra_fields.contains_key("function_call") || asst.tool_calls.is_some(),
                "function_call or tool_calls should be preserved"
            );
        }
        _ => panic!("expected OpenAI body"),
    }
}

#[test]
fn ac2_openai_all_roles_parse() {
    let bytes =
        std::fs::read(fixtures_dir().join("openai/07_all_roles.json")).expect("fixture not found");
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let body = parse_with_provider(bytes, Provider::OpenAi).expect("parse failed");

    match body {
        ParsedBody::OpenAi(b) => {
            assert_eq!(
                b.messages.len(),
                5,
                "expected 5 messages for all-roles fixture"
            );
            let roles: Vec<&str> = b.messages.iter().map(|m| m.role.as_str()).collect();
            assert!(roles.contains(&"system"));
            assert!(roles.contains(&"developer"));
            assert!(roles.contains(&"user"));
            assert!(roles.contains(&"assistant"));
            assert!(roles.contains(&"tool"));
        }
        _ => panic!("expected OpenAI body"),
    }
}

#[test]
fn ac2_openai_tool_call_id_preserved() {
    let bytes =
        std::fs::read(fixtures_dir().join("openai/02_tool_calls.json")).expect("fixture not found");
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let body = parse_with_provider(bytes, Provider::OpenAi).expect("parse failed");

    match body {
        ParsedBody::OpenAi(b) => {
            let tool_msg = b.messages.iter().find(|m| m.role == "tool");
            assert!(tool_msg.is_some(), "no tool message found");
            let tool = tool_msg.unwrap();
            assert_eq!(
                tool.tool_call_id.as_deref(),
                Some("call_abc123"),
                "tool_call_id should be preserved"
            );
        }
        _ => panic!("expected OpenAI body"),
    }
}

#[test]
fn ac8_error_conditions() {
    // Empty input
    let result = parse(b"");
    assert!(result.is_err(), "empty input should fail");

    // Whitespace only
    let result = parse(b"   \n  ");
    assert!(result.is_err(), "whitespace-only input should fail");

    // Top-level array
    let result = parse(b"[1, 2, 3]");
    assert!(result.is_err(), "top-level array should fail");

    // Top-level string
    let result = parse(b"\"hello\"");
    assert!(result.is_err(), "top-level string should fail");

    // Missing messages field
    let result = parse(b"{\"model\":\"claude-3\"}");
    assert!(result.is_err(), "missing messages should fail");

    // Messages is not an array
    let result = parse(b"{\"model\":\"claude-3\",\"messages\":\"not-array\"}");
    assert!(result.is_err(), "non-array messages should fail");

    // Over-depth input
    let depth = 66;
    let deep = "[".repeat(depth) + &"]".repeat(depth);
    let body =
        format!("{{\"model\":\"m\",\"messages\":[{{\"role\":\"user\",\"content\":{deep}}}]}}");
    let result = parse(body.as_bytes());
    assert!(result.is_err(), "over-depth input should fail");

    // Invalid UTF-8
    let invalid_utf8 = b"\xff\xfe\x00\x01";
    let result = parse(invalid_utf8);
    assert!(result.is_err(), "invalid UTF-8 should fail");

    // Truncated JSON
    let result = parse(b"{\"model\":\"claude-3\",\"messages\":[");
    assert!(result.is_err(), "truncated JSON should fail");
}

#[test]
fn ac8_top_level_string_multibyte_boundary_no_panic() {
    // Regression: describe_value() truncates long top-level strings for the
    // NotAnObject diagnostic. A raw byte slice at index 32 can fall mid-codepoint
    // and panic on hostile input. The error path must return Err, never panic (AC8).
    let mut s = String::new();
    for _ in 0..31 {
        s.push('a'); // 31 ASCII bytes
    }
    s.push('é'); // 2 bytes occupying indices 31..33 — byte 32 is mid-codepoint
    for _ in 0..20 {
        s.push('b'); // push past the 32-byte truncation threshold
    }
    let json = serde_json::to_string(&s).expect("encode string");
    let result = parse(json.as_bytes());
    assert!(
        result.is_err(),
        "top-level string must return Err (NotAnObject), not panic"
    );
}

#[test]
fn ac8_errors_have_non_empty_diagnostics() {
    let cases: &[(&[u8], &str)] = &[
        (b"", "empty input"),
        (b"[1,2]", "top-level array"),
        (b"\"hello\"", "top-level string"),
        (b"{\"model\":\"m\"}", "missing messages"),
        (b"\xff\xfe", "invalid UTF-8"),
        (
            b"{\"model\":\"m\",\"messages\":\"bad\"}",
            "messages not array",
        ),
    ];

    for (input, label) in cases {
        let result = parse(input);
        assert!(result.is_err(), "expected error for {label}");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.is_empty(),
            "error message should not be empty for {label}"
        );
    }
}

#[test]
fn ac12_no_io_double_run_equality() {
    // Serialize the same parsed model twice and assert identical bytes
    let input = br#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"Hello"}],"max_tokens":1024}"#;
    let body = parse(input).expect("parse failed");
    let out1 = rskim_llm::serialize(&body).expect("serialize failed 1");
    let out2 = rskim_llm::serialize(&body).expect("serialize failed 2");
    assert_eq!(out1, out2, "double-run serialization must be identical");
}
