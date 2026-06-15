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

use rskim_llm::{LlmError, MAX_DEPTH, ParsedBody, Provider, parse, parse_with_provider};
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
            // non_exhaustive: wildcard required for future provider variants
            _ => {}
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
                body.extra_fields().contains_key("future_param"),
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
            let assistant = b.messages().iter().find(|m| m.role == "assistant");
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
                b.messages().len(),
                5,
                "expected 5 messages for all-roles fixture"
            );
            let roles: Vec<&str> = b.messages().iter().map(|m| m.role.as_str()).collect();
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
            let tool_msg = b.messages().iter().find(|m| m.role == "tool");
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

    // Over-depth input — must return LlmError::DepthExceeded (not a generic Err)
    let depth = 66;
    let deep = "[".repeat(depth) + &"]".repeat(depth);
    let body =
        format!("{{\"model\":\"m\",\"messages\":[{{\"role\":\"user\",\"content\":{deep}}}]}}");
    let result = parse(body.as_bytes());
    match result {
        Err(LlmError::DepthExceeded(d)) => {
            assert!(
                d > MAX_DEPTH,
                "DepthExceeded depth {d} must exceed MAX_DEPTH {MAX_DEPTH}"
            );
        }
        Ok(_) => panic!("over-depth input should fail"),
        Err(other) => panic!("expected LlmError::DepthExceeded, got: {other}"),
    }

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

/// Asserts that `LlmError::DepthExceeded` error message contains the literal
/// `MAX_DEPTH` value as a string, converting the manual-sync comment in `error.rs`
/// into an enforced invariant.  A change to `MAX_DEPTH` without updating the
/// `#[error("...maximum 64...")]` format string will fail this test.
#[test]
fn error_depth_exceeded_message_embeds_max_depth() {
    let err = LlmError::DepthExceeded(MAX_DEPTH + 1);
    let msg = err.to_string();
    let max_depth_str = MAX_DEPTH.to_string();
    assert!(
        msg.contains(&max_depth_str),
        "DepthExceeded error message must contain the MAX_DEPTH value ({MAX_DEPTH}) \
         as a string; got: {msg:?}\n\
         If MAX_DEPTH was changed, update error.rs #[error(\"...maximum N\")] to match."
    );
}

/// AC3 — Provider auto-detection via `parse()` (no explicit provider hint).
///
/// Tests that `parse()` correctly identifies OpenAI bodies even when they have
/// no discriminating message-level fields (no tool_calls, no tool_call_id, no
/// role:"developer", no response_format).  Previously, such bodies would be
/// silently misclassified as Anthropic.
#[test]
fn ac3_auto_detect_openai_from_model_prefix() {
    // A minimal plain-chat OpenAI body: no tool signals, just a model name.
    let cases: &[(&[u8], &str)] = &[
        (
            br#"{"model":"gpt-4o","messages":[{"role":"user","content":"Hello"}]}"#,
            "gpt-4o",
        ),
        (
            br#"{"model":"gpt-3.5-turbo","messages":[{"role":"system","content":"Be helpful"},{"role":"user","content":"Hi"}]}"#,
            "gpt-3.5-turbo",
        ),
        (
            br#"{"model":"o1-mini","messages":[{"role":"user","content":"Solve this"}]}"#,
            "o1-mini",
        ),
        (
            br#"{"model":"o3-mini","messages":[{"role":"user","content":"Test"}]}"#,
            "o3-mini",
        ),
    ];

    for (bytes, model) in cases {
        let result = parse(bytes);
        assert!(
            result.is_ok(),
            "parse failed for model {model}: {:?}",
            result.unwrap_err()
        );
        match result.unwrap() {
            ParsedBody::OpenAi(b) => {
                assert_eq!(b.model(), *model, "model field must be preserved");
            }
            ParsedBody::Anthropic(_) => {
                panic!("model {model} was misdetected as Anthropic — should be OpenAI");
            }
            // non_exhaustive: wildcard required for future provider variants
            _ => panic!("unexpected ParsedBody variant for model {model}"),
        }
    }
}

/// A plain Anthropic body (no max_tokens discriminant) must still detect as Anthropic
/// when its message content contains an Anthropic-specific block type.
#[test]
fn ac3_auto_detect_anthropic_from_block_type() {
    let json = br#"{"model":"claude-opus-4","messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"Let me reason..."}]}]}"#;
    let body = parse(json).expect("parse failed");
    match body {
        ParsedBody::Anthropic(_) => {}
        ParsedBody::OpenAi(_) => {
            panic!("body with thinking block must detect as Anthropic")
        }
        // non_exhaustive: wildcard required for future provider variants
        _ => panic!("unexpected ParsedBody variant"),
    }
}

#[test]
fn ac12_no_io_double_run_equality() {
    // AC12: two INDEPENDENT full pipelines (parse→classify_body→mutate→serialize)
    // on the same input bytes must produce identical output.  Serializing the same
    // model twice only tests Vec::clone determinism; running two separate parse
    // calls exercises any HashMap-iteration or RNG dependence in the pipeline.
    //
    // We use a body with a mutation (forcing the typed-field serialize path to be
    // exercised) and extra_fields (to exercise map-iteration order) so that a
    // future nondeterminism regression in those paths would be caught here.
    use rskim_llm::{list_blocks, mutate_block, serialize};

    let input = br#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"Hello"},{"role":"assistant","content":[{"type":"text","text":"Hi there"},{"type":"tool_use","id":"t1","name":"search","input":{"q":"test"}}]}],"max_tokens":1024,"unknown_extra":"preserved"}"#;

    // Pipeline A: parse → classify → mutate first mutable block → serialize
    let mut body_a = parse(input).expect("pipeline A parse failed");
    let _cls_a = rskim_llm::classify_body(&body_a);
    let blocks_a = list_blocks(&body_a);
    let out_a = if let Some(b) = blocks_a.iter().find(|b| b.mutable) {
        mutate_block(&mut body_a, &b.id, "REPLACED").expect("pipeline A mutate failed")
    } else {
        serialize(&body_a).expect("pipeline A serialize failed")
    };

    // Pipeline B: independent parse → classify → mutate → serialize
    let mut body_b = parse(input).expect("pipeline B parse failed");
    let _cls_b = rskim_llm::classify_body(&body_b);
    let blocks_b = list_blocks(&body_b);
    let out_b = if let Some(b) = blocks_b.iter().find(|b| b.mutable) {
        mutate_block(&mut body_b, &b.id, "REPLACED").expect("pipeline B mutate failed")
    } else {
        serialize(&body_b).expect("pipeline B serialize failed")
    };

    assert_eq!(
        out_a, out_b,
        "two independent parse→classify→mutate→serialize pipelines must produce \
         identical bytes (AC12 determinism)"
    );
}
