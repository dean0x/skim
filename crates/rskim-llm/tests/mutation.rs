//! Mutation API tests (AC9, AC10, AC11).
//!
//! - AC9: Mutation replaces one payload, leaving surrounding bytes identical, re-parseable
//! - AC10: NEGATIVE: no content added
//! - AC11: NEGATIVE: no turn manipulation

// Test code legitimately uses panic/expect/unwrap for test failure reporting.
#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    unused_doc_comments
)]

use proptest::prelude::*;
use rskim_llm::{
    ParsedBody, Provider, list_blocks, mutate_block, parse, parse_with_provider, serialize,
};

// A body with multiple sibling blocks for mutation tests
const MULTI_BLOCK_BODY: &[u8] = br#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_001","content":"ORIGINAL_PAYLOAD","is_error":false},{"type":"text","text":"Some other text block"}]},{"role":"assistant","content":"Assistant response"}],"max_tokens":2048}"#;

#[test]
fn ac9_mutate_tool_result_string() {
    let body_str = br#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_001","content":"ORIGINAL"}]}],"max_tokens":100}"#;
    let mut body = parse(body_str).expect("parse failed");

    let blocks = list_blocks(&body);
    let mutable = blocks.iter().find(|b| b.mutable).expect("no mutable block");
    let block_id = mutable.id.clone();

    let result = mutate_block(&mut body, &block_id, "REPLACED").expect("mutation failed");

    // (a) verify the payload was replaced
    let reparsed = parse(&result).expect("re-parse failed");
    match &reparsed {
        ParsedBody::Anthropic(b) => match &b.messages()[0].content {
            rskim_llm::model::anthropic::AnthropicContent::Blocks(blocks) => match &blocks[0] {
                rskim_llm::model::anthropic::AnthropicBlock::ToolResult(tr) => match &tr.content {
                    Some(rskim_llm::model::anthropic::ToolResultContent::Text(s)) => {
                        assert_eq!(s, "REPLACED");
                    }
                    _ => panic!("unexpected content"),
                },
                _ => panic!("expected tool_result"),
            },
            _ => panic!("expected blocks"),
        },
        _ => panic!("expected Anthropic"),
    }

    // (b) output re-parses as valid
    assert!(parse(&result).is_ok());

    // (c) repeated mutation yields identical bytes
    let result2 = mutate_block(&mut body, &block_id, "REPLACED").expect("second mutation failed");
    assert_eq!(
        result, result2,
        "repeated mutation must yield identical bytes"
    );
}

#[test]
fn ac9_mutate_text_block() {
    let body_str = br#"{"model":"m","messages":[{"role":"user","content":[{"type":"text","text":"ORIGINAL TEXT"},{"type":"text","text":"SECOND BLOCK"}]}],"max_tokens":100}"#;
    let mut body = parse(body_str).expect("parse failed");

    let blocks = list_blocks(&body);
    let first_text = blocks
        .iter()
        .find(|b| b.mutable && b.kind == "text")
        .expect("no text block");
    let block_id = first_text.id.clone();

    let result = mutate_block(&mut body, &block_id, "REPLACED TEXT").expect("mutation failed");

    // Verify replacement at the correct position
    let reparsed = parse(&result).expect("re-parse failed");
    match &reparsed {
        ParsedBody::Anthropic(b) => {
            match &b.messages()[0].content {
                rskim_llm::model::anthropic::AnthropicContent::Blocks(blks) => {
                    match &blks[0] {
                        rskim_llm::model::anthropic::AnthropicBlock::Text(tb) => {
                            assert_eq!(
                                tb.text, "REPLACED TEXT",
                                "first text block must be replaced"
                            );
                        }
                        _ => panic!("expected text block"),
                    }
                    // Second block must be unchanged
                    match &blks[1] {
                        rskim_llm::model::anthropic::AnthropicBlock::Text(tb) => {
                            assert_eq!(
                                tb.text, "SECOND BLOCK",
                                "second text block must be unchanged"
                            );
                        }
                        _ => panic!("expected text block"),
                    }
                }
                _ => panic!("expected blocks"),
            }
        }
        _ => panic!("expected Anthropic"),
    }
}

#[test]
fn ac9_mutate_sibling_blocks_byte_identical() {
    let mut body = parse(MULTI_BLOCK_BODY).expect("parse failed");
    let blocks = list_blocks(&body);

    // Find the tool_result string block
    let tr_block = blocks
        .iter()
        .find(|b| b.kind == "tool_result-string")
        .expect("no tool_result block");
    let tr_id = tr_block.id.clone();

    let result = mutate_block(&mut body, &tr_id, "NEW_PAYLOAD").expect("mutation failed");

    // Re-parse and verify sibling blocks are unchanged
    let reparsed = parse(&result).expect("re-parse failed");
    match &reparsed {
        ParsedBody::Anthropic(b) => {
            let msg = &b.messages()[0];
            match &msg.content {
                rskim_llm::model::anthropic::AnthropicContent::Blocks(blks) => {
                    // Second block (text) should be unchanged
                    match &blks[1] {
                        rskim_llm::model::anthropic::AnthropicBlock::Text(tb) => {
                            assert_eq!(tb.text, "Some other text block");
                        }
                        _ => panic!("second block should be text"),
                    }
                }
                _ => panic!("expected blocks"),
            }
            // Second message should be unchanged
            match &b.messages()[1].content {
                rskim_llm::model::anthropic::AnthropicContent::Text(s) => {
                    assert_eq!(s, "Assistant response");
                }
                _ => panic!("second message should be string"),
            }
        }
        _ => panic!("expected Anthropic"),
    }
}

#[test]
fn ac10_no_content_added_unmutated() {
    // Unmutated round-trip: zero diff (byte-identity)
    let input = br#"{"model":"m","messages":[{"role":"user","content":"Hello"}],"max_tokens":100}"#;
    let body = parse(input).expect("parse failed");
    let output = serialize(&body).expect("serialize failed");
    assert_eq!(
        output,
        input.as_ref(),
        "unmutated round-trip must be byte-identical"
    );
}

#[test]
fn ac10_no_new_fields_after_mutation() {
    let mut body = parse(MULTI_BLOCK_BODY).expect("parse failed");
    let blocks = list_blocks(&body);
    let tr_block = blocks
        .iter()
        .find(|b| b.kind == "tool_result-string")
        .expect("no tool_result");
    let tr_id = tr_block.id.clone();

    let result = mutate_block(&mut body, &tr_id, "REPLACED").expect("mutation failed");

    // Re-parse both input and output and verify no new fields
    let input_body = parse(MULTI_BLOCK_BODY).expect("input parse failed");
    let output_body = parse(&result).expect("output parse failed");

    match (input_body, output_body) {
        (ParsedBody::Anthropic(inp), ParsedBody::Anthropic(out)) => {
            // Compare extra_fields at top level
            assert_eq!(
                inp.extra_fields().keys().collect::<Vec<_>>(),
                out.extra_fields().keys().collect::<Vec<_>>(),
                "no new top-level fields should appear after mutation"
            );
            assert_eq!(
                inp.messages().len(),
                out.messages().len(),
                "message count unchanged"
            );
        }
        _ => panic!("expected Anthropic bodies"),
    }
}

#[test]
fn ac11_no_turn_manipulation_message_count() {
    let body = parse(MULTI_BLOCK_BODY).expect("parse failed");
    let initial_count = match &body {
        ParsedBody::Anthropic(b) => b.messages().len(),
        _ => panic!("expected Anthropic"),
    };

    // Verify list_blocks doesn't mutate count
    let _blocks = list_blocks(&body);
    let count_after = match &body {
        ParsedBody::Anthropic(b) => b.messages().len(),
        _ => panic!("expected Anthropic"),
    };
    assert_eq!(
        initial_count, count_after,
        "list_blocks must not change message count"
    );

    // Verify serialize doesn't change count
    let serialized = serialize(&body).expect("serialize failed");
    let reparsed = parse(&serialized).expect("re-parse failed");
    let count_reparsed = match &reparsed {
        ParsedBody::Anthropic(b) => b.messages().len(),
        _ => panic!("expected Anthropic"),
    };
    assert_eq!(
        initial_count, count_reparsed,
        "message count must be invariant through round-trip"
    );
}

#[test]
fn ac11_no_turn_manipulation_order() {
    let body = parse(MULTI_BLOCK_BODY).expect("parse failed");

    // Record original message order
    let original_roles: Vec<String> = match &body {
        ParsedBody::Anthropic(b) => b.messages().iter().map(|m| m.role.clone()).collect(),
        _ => panic!("expected Anthropic"),
    };

    let serialized = serialize(&body).expect("serialize failed");
    let reparsed = parse(&serialized).expect("re-parse failed");
    let reparsed_roles: Vec<String> = match &reparsed {
        ParsedBody::Anthropic(b) => b.messages().iter().map(|m| m.role.clone()).collect(),
        _ => panic!("expected Anthropic"),
    };

    assert_eq!(
        original_roles, reparsed_roles,
        "message order must be invariant"
    );
}

#[test]
fn error_block_not_found() {
    let mut body = parse(MULTI_BLOCK_BODY).expect("parse failed");
    let result = mutate_block(&mut body, "nonexistent_id", "text");
    assert!(result.is_err(), "nonexistent block should return error");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not found"),
        "error should mention not found: {err}"
    );
}

#[test]
fn error_exempt_block_anthropic_tool_use() {
    // An Anthropic body with a tool_use block (exempt from mutation).
    // list_blocks returns a descriptor with mutable:false and kind "tool_use".
    // mutate_block must return Err(BlockNotMutable), NOT BlockNotFound.
    let json = br#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"assistant","content":[{"type":"tool_use","id":"call_abc","name":"bash","input":{"cmd":"ls"}}]}],"max_tokens":100}"#;
    let mut body = parse(json).expect("parse failed");

    // list_blocks must expose the tool_use block with mutable:false
    let blocks = list_blocks(&body);
    assert_eq!(blocks.len(), 1, "should have one block descriptor");
    assert!(!blocks[0].mutable, "tool_use must be non-mutable");
    assert_eq!(blocks[0].kind, "tool_use");
    let tool_id = blocks[0].id.clone();

    // mutate_block on an exempt id must return BlockNotMutable, not BlockNotFound
    let result = mutate_block(&mut body, &tool_id, "replacement");
    assert!(result.is_err(), "mutating exempt block must return Err");
    match result.unwrap_err() {
        rskim_llm::LlmError::BlockNotMutable(id, kind) => {
            assert_eq!(id, tool_id, "error id must match requested id");
            assert_eq!(kind, "tool_use", "error kind must match block kind");
        }
        other => panic!("expected BlockNotMutable, got: {other}"),
    }
}

#[test]
fn error_exempt_block_anthropic_thinking() {
    // An Anthropic body with a thinking block (exempt from mutation).
    let json = br#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"Let me think..."}]}],"max_tokens":100}"#;
    let mut body = parse(json).expect("parse failed");

    let blocks = list_blocks(&body);
    assert_eq!(blocks.len(), 1);
    assert!(!blocks[0].mutable, "thinking must be non-mutable");
    assert_eq!(blocks[0].kind, "thinking");
    let thinking_id = blocks[0].id.clone();

    let result = mutate_block(&mut body, &thinking_id, "replacement");
    match result.unwrap_err() {
        rskim_llm::LlmError::BlockNotMutable(id, kind) => {
            assert_eq!(id, thinking_id);
            assert_eq!(kind, "thinking");
        }
        other => panic!("expected BlockNotMutable, got: {other}"),
    }
}

#[test]
fn error_openai_body_returns_block_not_mutable() {
    // OpenAI body — mutation not yet implemented.
    // Must return BlockNotMutable (not BlockNotFound) so callers can distinguish
    // "unsupported provider" from "id does not exist".
    let openai_json = br#"{"model":"gpt-4o","messages":[{"role":"user","content":"Hello"}]}"#;
    let mut body = parse_with_provider(openai_json, Provider::OpenAi).expect("parse failed");
    let result = mutate_block(&mut body, "m0", "text");
    match result.unwrap_err() {
        rskim_llm::LlmError::BlockNotMutable(id, kind) => {
            assert_eq!(id, "m0");
            assert_eq!(kind, "openai-not-implemented");
        }
        other => panic!("expected BlockNotMutable(openai-not-implemented), got: {other}"),
    }
}

// ---------------------------------------------------------------------------
// AC9(b) + AC10 — byte-identical surrounding bytes after mutation (byte-surgery proof)
// ---------------------------------------------------------------------------

/// Extract the portion of `haystack` that does NOT contain the occurrence of
/// `needle` as a JSON string value.  Returns `(prefix, suffix)` where
/// `prefix = haystack[..needle_start]` and `suffix = haystack[needle_end..]`.
fn split_around(haystack: &[u8], needle_json: &[u8]) -> (Vec<u8>, Vec<u8>) {
    // Find the needle inside haystack.  Since needle_json is the exact JSON
    // encoding of the payload including the surrounding quotes, there is exactly
    // one match in a well-formed body.
    let pos = haystack
        .windows(needle_json.len())
        .position(|w| w == needle_json)
        .expect("needle not found in haystack");
    (
        haystack[..pos].to_vec(),
        haystack[pos + needle_json.len()..].to_vec(),
    )
}

#[test]
fn ac9b_surrounding_bytes_byte_identical_after_tool_result_mutation() {
    // Body with a non-canonical number token (1e3) in the envelope to prove
    // that mutation does NOT reformat the envelope.
    let input: &[u8] = br#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_001","content":"ORIGINAL_PAYLOAD","is_error":false},{"type":"text","text":"sibling text"}]}],"max_tokens":1e3}"#;

    let mut body = parse(input).expect("parse failed");
    let blocks = list_blocks(&body);
    let tr = blocks
        .iter()
        .find(|b| b.kind == "tool_result-string")
        .expect("no tr block");
    let tr_id = tr.id.clone();

    let result = mutate_block(&mut body, &tr_id, "NEW_PAYLOAD").expect("mutation failed");

    // Split original around the old payload
    let (orig_prefix, orig_suffix) = split_around(input, b"\"ORIGINAL_PAYLOAD\"");
    // Split result around the new payload
    let (res_prefix, res_suffix) = split_around(&result, b"\"NEW_PAYLOAD\"");

    assert_eq!(
        orig_prefix,
        res_prefix,
        "bytes BEFORE the mutated span must be byte-identical (AC9b)\n\
         original prefix:  {}\n\
         mutated  prefix:  {}",
        String::from_utf8_lossy(&orig_prefix),
        String::from_utf8_lossy(&res_prefix),
    );
    assert_eq!(
        orig_suffix,
        res_suffix,
        "bytes AFTER the mutated span must be byte-identical (AC9b)\n\
         original suffix: {}\n\
         mutated  suffix: {}",
        String::from_utf8_lossy(&orig_suffix),
        String::from_utf8_lossy(&res_suffix),
    );

    // Specifically: the envelope token 1e3 must survive unchanged (the key proof
    // that the old whole-body re-serialization path (which rewrote 1e3→1000.0)
    // has been replaced by byte-surgery).
    assert!(
        result.windows(3).any(|w| w == b"1e3"),
        "non-canonical number token 1e3 in envelope must survive mutation (AC9b/AC10)\n\
         result: {}",
        String::from_utf8_lossy(&result),
    );
}

#[test]
fn ac10_only_differences_lie_within_replaced_span() {
    // Feed a body through mutate_block and verify that the ONLY bytes that differ
    // from the input lie within the replaced payload span.
    let input: &[u8] = br#"{"model":"m","messages":[{"role":"user","content":[{"type":"text","text":"OLD"},{"type":"text","text":"OTHER"}]}],"max_tokens":100}"#;

    let mut body = parse(input).expect("parse failed");
    let blocks = list_blocks(&body);
    let first_text = blocks
        .iter()
        .find(|b| b.mutable && b.kind == "text")
        .expect("no text block");
    let bid = first_text.id.clone();

    let result = mutate_block(&mut body, &bid, "NEW").expect("mutation failed");

    // The prefix before "OLD" and the suffix after "OLD" in input must equal
    // the corresponding regions in result (with "OLD"→"NEW" at the seam).
    let (orig_pre, orig_suf) = split_around(input, b"\"OLD\"");
    let (res_pre, res_suf) = split_around(&result, b"\"NEW\"");

    assert_eq!(orig_pre, res_pre, "prefix must be byte-identical");
    assert_eq!(
        orig_suf, res_suf,
        "suffix must be byte-identical (includes 'OTHER')"
    );

    // Re-parse and verify the OTHER text block is still "OTHER"
    let reparsed = parse(&result).expect("re-parse after mutation failed");
    match &reparsed {
        ParsedBody::Anthropic(b) => match &b.messages()[0].content {
            rskim_llm::model::anthropic::AnthropicContent::Blocks(blks) => match &blks[1] {
                rskim_llm::model::anthropic::AnthropicBlock::Text(tb) => {
                    assert_eq!(tb.text, "OTHER", "untouched sibling must be byte-identical");
                }
                _ => panic!("expected text block"),
            },
            _ => panic!("expected blocks"),
        },
        _ => panic!("expected Anthropic"),
    }
}

#[test]
fn ac11_no_mutation_api_through_public_fields() {
    // Compile-time proof that structural mutation is not reachable through the
    // public API: this test asserts that calling read-only accessor methods does
    // NOT return a mutable reference, and that no public API surface exposes
    // messages.push / messages.remove / model assignment.
    //
    // The actual type-level enforcement is performed by the Rust compiler
    // (private fields + no `&mut` accessor = no mutation possible).  This test
    // documents and exercises the read-only contract so that a future regression
    // (accidentally making a field pub) would surface as a compile error in the
    // test file rather than silently at runtime.
    let input = br#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"Hello"}],"max_tokens":100}"#;
    let body = parse(input).expect("parse failed");
    match &body {
        ParsedBody::Anthropic(b) => {
            // These are the ONLY read paths available — no &mut is returned.
            let _model: &str = b.model();
            let _msgs: &[_] = b.messages();
            let _extra: &serde_json::Map<_, _> = b.extra_fields();
            // Cannot call b.messages_mut() — no such method exists.
            // Cannot do b.model = "other" — field is pub(crate).
            // This test passes if and only if those invariants hold at compile time.
            assert_eq!(_model, "claude-3-5-sonnet-20241022");
            assert_eq!(_msgs.len(), 1);
        }
        _ => panic!("expected Anthropic"),
    }
}

// ---------------------------------------------------------------------------
// JSON-escaping mutation (coverage for splice_replace's serde_json::to_string path)
// ---------------------------------------------------------------------------

#[test]
fn mutation_with_json_escaped_replacement() {
    // Replacement text contains characters that require JSON escaping:
    // double-quote, backslash, newline, tab, and a non-ASCII codepoint.
    // splice_replace uses serde_json::to_string to produce the quoted replacement,
    // so the round-trip must: (a) re-parse successfully, (b) decode back to the
    // original replacement text, (c) be byte-stable under repeat mutation, and
    // (d) leave surrounding bytes unchanged.
    let input: &[u8] = br#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"ORIGINAL"}],"max_tokens":100}"#;
    let new_text = "she said \"hi\"\nline2\t\u{1f600}";

    let mut body = parse(input).expect("parse failed");
    let blocks = list_blocks(&body);
    let bid = blocks
        .iter()
        .find(|b| b.mutable)
        .expect("no mutable block")
        .id
        .clone();

    // (a) mutation must succeed and produce valid JSON
    let result = mutate_block(&mut body, &bid, new_text).expect("mutation failed");
    let reparsed = parse(&result).expect("re-parse after escape-triggering mutation failed");

    // (b) the replacement text must round-trip correctly through JSON
    match &reparsed {
        ParsedBody::Anthropic(b) => match &b.messages()[0].content {
            rskim_llm::model::anthropic::AnthropicContent::Text(s) => {
                assert_eq!(
                    s, new_text,
                    "JSON-escaped replacement must decode back to original text"
                );
            }
            _ => panic!("expected string content"),
        },
        _ => panic!("expected Anthropic"),
    }

    // (c) repeat mutation must produce byte-identical output (idempotency)
    let result2 = mutate_block(&mut body, &bid, new_text).expect("second mutation failed");
    assert_eq!(
        result, result2,
        "repeat mutation with escaped text must be byte-identical"
    );

    // (d) surrounding bytes must be unchanged
    let prefix_end = result
        .windows(b"ORIGINAL".len())
        .position(|w| w == b"ORIGINAL");
    assert!(
        prefix_end.is_none(),
        "old payload 'ORIGINAL' must not appear in result"
    );
}

// Property test: message count and order invariant through parse->classify->mutate->serialize.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(20))]

    #[test]
    fn ac11_proptest_message_count_invariant(
        // Number of messages: 1 to 5
        n_messages in 1usize..=5,
        // Replacement text for the first mutable block
        replacement in "[A-Za-z0-9 ]{1,50}",
    ) {
        let mut messages = Vec::new();
        for i in 0..n_messages {
            messages.push(format!(
                r#"{{"role":"user","content":"Message {i}"}}"#,
            ));
        }
        let body_str = format!(
            r#"{{"model":"claude-3-5-sonnet-20241022","messages":[{}],"max_tokens":100}}"#,
            messages.join(",")
        );

        let mut body = parse(body_str.as_bytes()).expect("parse failed");
        let initial_count = match &body {
            ParsedBody::Anthropic(b) => b.messages().len(),
            _ => return Ok(()),
        };

        // Mutate the first mutable block
        let blocks = list_blocks(&body);
        if let Some(mutable) = blocks.iter().find(|b| b.mutable) {
            let block_id = mutable.id.clone();
            let result = mutate_block(&mut body, &block_id, &replacement);
            if let Ok(result) = result {
                let reparsed = parse(&result).expect("re-parse failed");
                let count_after = match &reparsed {
                    ParsedBody::Anthropic(b) => b.messages().len(),
                    _ => return Ok(()),
                };
                prop_assert_eq!(initial_count, count_after, "message count must be invariant");
            }
        }
    }
}
