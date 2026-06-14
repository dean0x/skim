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

/// A parsed LLM request body.
///
/// Holds either an Anthropic or OpenAI body, preserving all unknown fields as raw
/// byte spans for byte-identical round-trips.
#[derive(Debug, Clone)]
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
///     ParsedBody::Anthropic(b) => assert_eq!(b.model, "claude-3-5-sonnet-20241022"),
///     ParsedBody::OpenAi(_) => panic!("unexpected"),
/// }
/// # Ok::<(), rskim_llm::LlmError>(())
/// ```
pub fn parse(bytes: &[u8]) -> Result<ParsedBody> {
    let text = std::str::from_utf8(bytes).map_err(|e| LlmError::InvalidUtf8(e.to_string()))?;

    check_depth(text)?;

    // Parse to a generic Map first for provider detection
    let top: serde_json::Value = serde_json::from_str(text)?;

    let obj = top
        .as_object()
        .ok_or_else(|| LlmError::NotAnObject(describe_value(&top)))?;

    // Validate messages field exists and is an array
    match obj.get("messages") {
        None => return Err(LlmError::MissingMessages),
        Some(serde_json::Value::Array(_)) => {}
        Some(other) => return Err(LlmError::MessagesNotArray(describe_value(other))),
    }

    let detected = provider::detect(obj).unwrap_or(Provider::Anthropic);
    parse_as(text, detected)
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

    check_depth(text)?;

    // Validate top-level structure
    let top: serde_json::Value = serde_json::from_str(text)?;
    let obj = top
        .as_object()
        .ok_or_else(|| LlmError::NotAnObject(describe_value(&top)))?;

    match obj.get("messages") {
        None => return Err(LlmError::MissingMessages),
        Some(serde_json::Value::Array(_)) => {}
        Some(other) => return Err(LlmError::MessagesNotArray(describe_value(other))),
    }

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
                format!("string(\"{}...\")", &s[..32])
            } else {
                format!("string(\"{s}\")")
            }
        }
        serde_json::Value::Array(_) => "array".to_string(),
        serde_json::Value::Object(_) => "object".to_string(),
    }
}
