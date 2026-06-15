//! JSON parsing for LLM request bodies.
//!
//! Entry point: [`parse`] and [`parse_with_provider`].
//!
//! # Depth-checking
//!
//! Before structural parsing, the raw JSON is scanned for nesting depth to prevent
//! stack overflow from adversarial input. Any body exceeding [`crate::MAX_DEPTH`]
//! returns [`crate::LlmError::DepthExceeded`].

use crate::model::anthropic::AnthropicBody;
use crate::model::openai::OpenAiBody;
use crate::provider::Provider;
use crate::{LlmError, MAX_DEPTH, Result, provider};

/// Validate the UTF-8 text as a top-level JSON object with a `messages` array,
/// and return the parsed `Value` so the caller can reuse it for provider detection
/// without a second `serde_json::from_str` pass.
///
/// Returns the top-level `serde_json::Value::Object` by value (not cloned — the
/// intermediate `Value` is moved out).  On the `parse_with_provider` path the
/// returned value is dropped immediately; on the `parse` path it is used once
/// by `provider::detect` (which takes a borrow) and then dropped.
///
/// # Complexity
///
/// One O(n) depth scan + one serde_json parse.  Provider detection and the
/// typed-model parse in `parse_as` then re-parse the same bytes independently
/// (one extra pass for the typed model), but the intermediate `Value` used here
/// is not kept alive — it is dropped before `parse_as` is called.
fn validate(text: &str) -> Result<serde_json::Map<String, serde_json::Value>> {
    check_depth(text)?;
    let top: serde_json::Value = serde_json::from_str(text)?;
    // Move the map out of the Value rather than cloning — avoids a full O(body)
    // deep copy.  We only need to check the shape and return the map for
    // provider::detect to borrow briefly.
    let obj = match top {
        serde_json::Value::Object(m) => m,
        other => return Err(LlmError::NotAnObject(describe_value(&other))),
    };
    match obj.get("messages") {
        None => return Err(LlmError::MissingMessages),
        Some(serde_json::Value::Array(_)) => {}
        Some(other) => return Err(LlmError::MessagesNotArray(describe_value(other))),
    }
    Ok(obj)
}

/// A parsed LLM request body.
///
/// Holds either an Anthropic or OpenAI body, preserving all unknown fields as raw
/// byte spans for byte-identical round-trips.
///
/// This enum is `#[non_exhaustive]` — future providers can be added without a
/// breaking change (additive-only insurance per Resolved Decision 7).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ParsedBody {
    /// An Anthropic `/v1/messages` request body.
    Anthropic(AnthropicBody),
    /// An OpenAI `/v1/chat/completions` request body.
    OpenAi(OpenAiBody),
}

/// Parse raw JSON bytes into a typed LLM request body.
///
/// Provider is auto-detected from the body structure. Use [`parse_with_provider`]
/// to force a specific provider.
///
/// # Errors
///
/// - [`LlmError::InvalidUtf8`] — bytes are not valid UTF-8
/// - [`LlmError::Json`] — bytes are not valid JSON
/// - [`LlmError::NotAnObject`] — top-level JSON value is not an object
/// - [`LlmError::MissingMessages`] — the `messages` field is absent
/// - [`LlmError::MessagesNotArray`] — the `messages` field is not an array
/// - [`LlmError::DepthExceeded`] — nesting depth exceeds [`crate::MAX_DEPTH`]
///
/// # Examples
///
/// ```
/// use rskim_llm::{parse, ParsedBody};
///
/// let json = r#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"Hello"}],"max_tokens":1024}"#;
/// let body = parse(json.as_bytes())?;
/// match body {
///     ParsedBody::Anthropic(b) => assert_eq!(b.model(), "claude-3-5-sonnet-20241022"),
///     ParsedBody::OpenAi(_) => panic!("unexpected"),
///     // ParsedBody is #[non_exhaustive] — wildcard required for future variants
///     _ => panic!("unexpected variant"),
/// }
/// # Ok::<(), rskim_llm::LlmError>(())
/// ```
pub fn parse(bytes: &[u8]) -> Result<ParsedBody> {
    let text = std::str::from_utf8(bytes).map_err(|e| LlmError::InvalidUtf8(e.to_string()))?;
    let obj = validate(text)?;
    let provider = provider::detect(&obj);
    parse_as(text, provider)
}

/// Parse raw JSON bytes with an explicit provider hint.
///
/// Skips provider auto-detection. Useful when the provider is known from context
/// (e.g., the API endpoint URL).
///
/// # Errors
///
/// Same as [`parse`].
pub fn parse_with_provider(bytes: &[u8], p: Provider) -> Result<ParsedBody> {
    let text = std::str::from_utf8(bytes).map_err(|e| LlmError::InvalidUtf8(e.to_string()))?;
    validate(text)?;
    parse_as(text, p)
}

/// Parse text as a specific provider's schema, storing raw bytes for byte-identical serialize.
fn parse_as(text: &str, provider: Provider) -> Result<ParsedBody> {
    let raw = text.as_bytes().to_vec();
    match provider {
        Provider::Anthropic => {
            let mut body: AnthropicBody = serde_json::from_str(text)?;
            body.raw_bytes = raw;
            Ok(ParsedBody::Anthropic(body))
        }
        Provider::OpenAi => {
            let mut body: OpenAiBody = serde_json::from_str(text)?;
            body.raw_bytes = raw;
            Ok(ParsedBody::OpenAi(body))
        }
    }
}

/// Check that the JSON text does not exceed [`MAX_DEPTH`] nesting levels.
///
/// Uses PF-004-safe arithmetic: depth is accumulated in `u32` with saturating
/// addition to prevent overflow before the comparison.
fn check_depth(text: &str) -> Result<()> {
    let mut depth: u32 = 0;
    let mut in_string = false;
    let mut escape_next = false;

    for byte in text.bytes() {
        if escape_next {
            escape_next = false;
            continue;
        }
        if in_string {
            match byte {
                b'\\' => escape_next = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => {
                // PF-004: saturating_add before comparison to prevent overflow
                depth = depth.saturating_add(1);
                if depth > MAX_DEPTH {
                    return Err(LlmError::DepthExceeded(depth));
                }
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }

    Ok(())
}

/// Describe a JSON value type for error messages.
fn describe_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => format!("bool({b})"),
        serde_json::Value::Number(n) => format!("number({n})"),
        serde_json::Value::String(s) => {
            if s.len() > 32 {
                // Truncate at a UTF-8 char boundary at or before byte 32 — direct
                // byte indexing can fall mid-codepoint and panic on multi-byte input.
                let end = s.char_indices().map(|(i, _)| i).filter(|&i| i <= 32).last().unwrap_or(0);
                format!("string(\"{}...\")", &s[..end])
            } else {
                format!("string(\"{s}\")")
            }
        }
        serde_json::Value::Array(_) => "array".to_string(),
        serde_json::Value::Object(_) => "object".to_string(),
    }
}
