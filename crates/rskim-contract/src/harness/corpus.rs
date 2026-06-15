//! Adversarial corpus fixtures for conformance testing.
//!
//! Two fixture categories:
//! - [`ADVERSARIAL_CORPUS`] — malformed/edge-case inputs expected to produce
//!   fail-open passthrough (AC3)
//! - [`VALID_CORPUS`] — well-formed inputs for both Anthropic and OpenAI schemas
//!   (AC3, AC4, AC6, AC8, AC9, AC13)
//!
//! Both categories are included in [`ALL_CORPUS`].
//!
//! # Corpus coverage targets (AC3/AC17)
//!
//! - Malformed JSON
//! - Truncated body
//! - Invalid UTF-8 bytes
//! - Empty body
//! - CRLF-containing strings
//! - Nesting exceeding `MAX_ANALYSIS_DEPTH`
//! - >100KB payload (inflation/boundary class)
//! - Multi-MB payload
//! - Zone boundary edge cases: zero assistant, trailing assistant, consecutive tool messages
//! - Both Anthropic and OpenAI schema shapes
//! - Thinking/reasoning blocks
//! - Tool calls with opaque `arguments` field

// ============================================================================
// Adversarial corpus (malformed / edge cases)
// ============================================================================

/// Malformed JSON body.
pub const MALFORMED_JSON: &[u8] = b"{not valid json}";

/// Truncated JSON body (cut mid-string).
pub const TRUNCATED_JSON: &[u8] = b"{\"model\":\"claude-3-5-sonnet-20241022\",\"mes";

/// Empty body.
pub const EMPTY_BODY: &[u8] = b"";

/// Whitespace-only body.
pub const WHITESPACE_ONLY: &[u8] = b"   \n\t  ";

/// Valid JSON but not an object (array at root).
pub const ROOT_ARRAY: &[u8] = b"[1,2,3]";

/// Valid JSON but not an object (null at root).
pub const ROOT_NULL: &[u8] = b"null";

/// JSON with CRLF inside string values.
pub const CRLF_IN_STRING: &[u8] =
    b"{\"model\":\"gpt-4o\",\"messages\":[{\"role\":\"user\",\"content\":\"line1\\r\\nline2\"}]}";

/// Invalid UTF-8 byte sequence (0xFF 0xFE BOM-like bytes).
pub const INVALID_UTF8: &[u8] = &[b'{', 0xFF, 0xFE, b'}'];

/// Nesting beyond MAX_ANALYSIS_DEPTH (adversarial DoS attempt).
///
/// This is generated at test time via `generate_deep_nesting()` because
/// static arrays of depth > 128 would be very large literals.
pub fn generate_deep_nesting(depth: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(depth * 2 + 10);
    buf.extend(std::iter::repeat_n(b'[', depth));
    buf.extend_from_slice(b"null");
    buf.extend(std::iter::repeat_n(b']', depth));
    buf
}

/// Anthropic schema with thinking blocks (sacrosanct content class).
pub const ANTHROPIC_WITH_THINKING: &[u8] = br#"{
  "model": "claude-3-5-sonnet-20241022",
  "max_tokens": 8096,
  "messages": [
    {
      "role": "user",
      "content": "What is 2+2?"
    },
    {
      "role": "assistant",
      "content": [
        {
          "type": "thinking",
          "thinking": "The user is asking a simple arithmetic question.",
          "signature": "encrypted-sig-bytes-here"
        },
        {
          "type": "text",
          "text": "2+2 = 4"
        }
      ]
    }
  ]
}"#;

/// All adversarial corpus inputs.
pub const ADVERSARIAL_CORPUS: &[&[u8]] = &[
    MALFORMED_JSON,
    TRUNCATED_JSON,
    EMPTY_BODY,
    WHITESPACE_ONLY,
    ROOT_ARRAY,
    ROOT_NULL,
    CRLF_IN_STRING,
    INVALID_UTF8,
];

// ============================================================================
// Valid corpus — Anthropic schema
// ============================================================================

/// Minimal Anthropic single-user-turn request.
pub const ANTHROPIC_MINIMAL: &[u8] = br#"{
  "model": "claude-3-5-sonnet-20241022",
  "max_tokens": 1024,
  "messages": [
    {"role": "user", "content": "Hello, Claude!"}
  ]
}"#;

/// Anthropic multi-turn request (AC6 zone boundary: assistant followed by user).
pub const ANTHROPIC_MULTI_TURN: &[u8] = br#"{
  "model": "claude-3-5-sonnet-20241022",
  "max_tokens": 1024,
  "messages": [
    {"role": "user", "content": "What is Rust?"},
    {"role": "assistant", "content": "Rust is a systems programming language."},
    {"role": "user", "content": "Tell me more."}
  ]
}"#;

/// Anthropic request where the last message is assistant (empty live zone / AC6 edge case b).
pub const ANTHROPIC_TRAILING_ASSISTANT: &[u8] = br#"{
  "model": "claude-3-5-sonnet-20241022",
  "max_tokens": 1024,
  "messages": [
    {"role": "user", "content": "Hello"},
    {"role": "assistant", "content": "Hi there!"}
  ]
}"#;

/// Anthropic request with no assistant messages (whole array live / AC6 edge case a).
pub const ANTHROPIC_NO_ASSISTANT: &[u8] = br#"{
  "model": "claude-3-5-sonnet-20241022",
  "max_tokens": 1024,
  "messages": [
    {"role": "user", "content": "First question"},
    {"role": "user", "content": "Second question"}
  ]
}"#;

/// Anthropic request with tool_result blocks (AC6 edge case c).
pub const ANTHROPIC_TOOL_RESULT: &[u8] = br#"{
  "model": "claude-3-5-sonnet-20241022",
  "max_tokens": 1024,
  "tools": [{"name": "search", "description": "Search the web", "input_schema": {"type": "object", "properties": {}}}],
  "messages": [
    {"role": "user", "content": "Search for Rust"},
    {"role": "assistant", "content": [{"type": "tool_use", "id": "toolu_01", "name": "search", "input": {}}]},
    {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_01", "content": "Rust results..."}]}
  ]
}"#;

/// Anthropic request with sacrosanct opaque fields.
pub const ANTHROPIC_SACROSANCT: &[u8] = br#"{
  "model": "claude-3-5-sonnet-20241022",
  "max_tokens": 1024,
  "metadata": {"user_id": "user-123"},
  "messages": [
    {
      "role": "assistant",
      "content": [
        {
          "type": "thinking",
          "thinking": "secret thoughts",
          "signature": "sig-bytes-must-pass-through-unchanged",
          "encrypted_content": "encrypted-bytes-opaque"
        }
      ]
    },
    {"role": "user", "content": "Continue"}
  ]
}"#;

// ============================================================================
// Valid corpus — OpenAI schema
// ============================================================================

/// Minimal OpenAI single-user-turn request.
pub const OPENAI_MINIMAL: &[u8] = br#"{
  "model": "gpt-4o",
  "messages": [
    {"role": "user", "content": "Hello!"}
  ]
}"#;

/// OpenAI multi-turn request.
pub const OPENAI_MULTI_TURN: &[u8] = br#"{
  "model": "gpt-4o",
  "messages": [
    {"role": "user", "content": "What is 2+2?"},
    {"role": "assistant", "content": "4"},
    {"role": "user", "content": "And 3+3?"}
  ]
}"#;

/// OpenAI request with tool calls (sacrosanct `arguments` field).
pub const OPENAI_TOOL_CALLS: &[u8] = br#"{
  "model": "gpt-4o",
  "messages": [
    {"role": "user", "content": "Search for Rust"},
    {
      "role": "assistant",
      "content": null,
      "tool_calls": [
        {
          "id": "call_abc123",
          "type": "function",
          "function": {
            "name": "search",
            "arguments": "{\"query\":\"Rust programming language\"}"
          }
        }
      ]
    },
    {
      "role": "tool",
      "tool_call_id": "call_abc123",
      "content": "Search results here"
    },
    {"role": "user", "content": "Tell me more"}
  ],
  "tools": [
    {
      "type": "function",
      "function": {
        "name": "search",
        "description": "Search the web",
        "parameters": {"type": "object", "properties": {"query": {"type": "string"}}}
      }
    }
  ]
}"#;

/// OpenAI request trailing with tool role (AC6 edge case c: consecutive trailing tool messages).
pub const OPENAI_TRAILING_TOOL: &[u8] = br#"{
  "model": "gpt-4o",
  "messages": [
    {"role": "user", "content": "Call the tool"},
    {"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "f", "arguments": "{}"}}]},
    {"role": "tool", "tool_call_id": "c1", "content": "result"},
    {"role": "tool", "tool_call_id": "c2", "content": "result2"}
  ]
}"#;

/// OpenAI request with reasoning content (sacrosanct field).
pub const OPENAI_WITH_REASONING: &[u8] = br#"{
  "model": "o1-mini",
  "messages": [
    {"role": "user", "content": "Solve this"},
    {"role": "assistant", "reasoning": "Let me think step by step...", "content": "Answer: 42"}
  ]
}"#;

// ============================================================================
// Large payload corpus (>100KB class, AC17 / PRISM #718/#600 analogue)
// ============================================================================

/// Generate a large Anthropic request body exceeding `target_bytes`.
///
/// The content is a valid Anthropic chat request with a single large user turn.
/// Used to verify that the structural analysis handles large payloads without
/// timeout or stack overflow.
pub fn generate_large_anthropic(target_bytes: usize) -> Vec<u8> {
    // Compute the envelope length without content to derive the required content size.
    let envelope_prefix = r#"{"model":"claude-3-5-sonnet-20241022","max_tokens":1024,"messages":[{"role":"user","content":""#;
    let envelope_suffix = r#""}]}"#;
    let envelope_len = envelope_prefix.len() + envelope_suffix.len();
    let content_len = target_bytes.saturating_sub(envelope_len);
    let content: String = "x".repeat(content_len);
    let body = format!("{envelope_prefix}{content}{envelope_suffix}");
    body.into_bytes()
}

/// Generate a large OpenAI request body exceeding `target_bytes`.
pub fn generate_large_openai(target_bytes: usize) -> Vec<u8> {
    let envelope_prefix = r#"{"model":"gpt-4o","messages":[{"role":"user","content":""#;
    let envelope_suffix = r#""}]}"#;
    let envelope_len = envelope_prefix.len() + envelope_suffix.len();
    let content_len = target_bytes.saturating_sub(envelope_len);
    let content: String = "y".repeat(content_len);
    let body = format!("{envelope_prefix}{content}{envelope_suffix}");
    body.into_bytes()
}

/// All valid corpus inputs (static, both schemas).
pub const VALID_CORPUS: &[&[u8]] = &[
    ANTHROPIC_MINIMAL,
    ANTHROPIC_MULTI_TURN,
    ANTHROPIC_TRAILING_ASSISTANT,
    ANTHROPIC_NO_ASSISTANT,
    ANTHROPIC_TOOL_RESULT,
    ANTHROPIC_SACROSANCT,
    ANTHROPIC_WITH_THINKING,
    OPENAI_MINIMAL,
    OPENAI_MULTI_TURN,
    OPENAI_TOOL_CALLS,
    OPENAI_TRAILING_TOOL,
    OPENAI_WITH_REASONING,
    CRLF_IN_STRING, // also valid JSON with CRLF in string values
];

/// All corpus inputs (adversarial + valid).
pub const ALL_CORPUS: &[&[u8]] = &[
    // Adversarial
    MALFORMED_JSON,
    TRUNCATED_JSON,
    EMPTY_BODY,
    WHITESPACE_ONLY,
    ROOT_ARRAY,
    ROOT_NULL,
    CRLF_IN_STRING,
    INVALID_UTF8,
    // Valid — Anthropic
    ANTHROPIC_MINIMAL,
    ANTHROPIC_MULTI_TURN,
    ANTHROPIC_TRAILING_ASSISTANT,
    ANTHROPIC_NO_ASSISTANT,
    ANTHROPIC_TOOL_RESULT,
    ANTHROPIC_SACROSANCT,
    ANTHROPIC_WITH_THINKING,
    // Valid — OpenAI
    OPENAI_MINIMAL,
    OPENAI_MULTI_TURN,
    OPENAI_TOOL_CALLS,
    OPENAI_TRAILING_TOOL,
    OPENAI_WITH_REASONING,
];

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, invalid_from_utf8)]
mod tests {
    use super::*;

    #[test]
    fn adversarial_corpus_non_empty() {
        assert!(!ADVERSARIAL_CORPUS.is_empty());
        assert!(!VALID_CORPUS.is_empty());
        assert!(!ALL_CORPUS.is_empty());
    }

    #[test]
    fn generate_deep_nesting_produces_correct_depth() {
        let nested = generate_deep_nesting(10);
        let expected = b"[[[[[[[[[[null]]]]]]]]]]";
        assert_eq!(nested, expected);
    }

    #[test]
    fn generate_large_anthropic_exceeds_target() {
        let body = generate_large_anthropic(100_000);
        assert!(
            body.len() >= 100_000,
            "expected ≥100KB, got {} bytes",
            body.len()
        );
        // Verify it's valid JSON.
        let s = std::str::from_utf8(&body).expect("must be valid UTF-8");
        serde_json::from_str::<serde_json::Value>(s).expect("must be valid JSON");
    }

    #[test]
    fn generate_large_openai_exceeds_target() {
        let body = generate_large_openai(100_000);
        assert!(body.len() >= 100_000);
        let s = std::str::from_utf8(&body).expect("valid UTF-8");
        serde_json::from_str::<serde_json::Value>(s).expect("valid JSON");
    }

    #[test]
    fn crlf_in_string_is_valid_json() {
        // CRLF inside string values must be valid JSON (escaped as \\r\\n).
        let s = std::str::from_utf8(CRLF_IN_STRING).expect("valid UTF-8");
        serde_json::from_str::<serde_json::Value>(s).expect("valid JSON");
    }

    #[test]
    fn invalid_utf8_is_not_valid_utf8() {
        assert!(std::str::from_utf8(INVALID_UTF8).is_err());
    }
}
