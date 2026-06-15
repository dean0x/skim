//! JSON parsing for LLM request bodies.
//!
//! Entry point: [`parse`] and [`parse_with_provider`].
//!
//! # Depth-checking
//!
//! Before structural parsing, the raw JSON is scanned for nesting depth to prevent
//! stack overflow from adversarial input. Any body exceeding [`crate::MAX_DEPTH`]
//! returns [`crate::LlmError::DepthExceeded`].
//!
//! # Single-pass provider detection
//!
//! The [`parse`] function performs provider detection via [`crate::provider::detect_str`],
//! which runs a single shallow deserialization that captures only the fields needed
//! for provider selection (`model`, `response_format`, `max_tokens`,
//! `messages[*].{role,tool_calls,tool_call_id,content[*].type}`), discarding all
//! other field values via `serde::de::IgnoredAny`.  The typed parse in [`parse_as`]
//! is the only deep parse of the body.
//!
//! # Memory profile
//!
//! For a body of size B, peak allocation is approximately:
//!
//! - Input buffer: 1× B (owned `Vec<u8>` in the caller or `ChunkIngestionBuilder`)
//! - Shallow detection: sub-linear in B (only discriminating fields materialised)
//! - Typed model (`AnthropicBody`/`OpenAiBody`): ≤1× B
//!
//! This gives k ≈ 2× B for the parse path — a significant improvement over the
//! previous k ≈ 3.5× that included a full `serde_json::Value` intermediate.

use crate::model::anthropic::AnthropicBody;
use crate::model::openai::OpenAiBody;
use crate::provider::Provider;
use crate::{LlmError, MAX_DEPTH, Result, provider};

/// Convert raw bytes to UTF-8, returning [`LlmError::InvalidUtf8`] on failure.
pub(crate) fn to_utf8(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|e| LlmError::InvalidUtf8(e.to_string()))
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
    let text = to_utf8(bytes)?;
    check_depth(text)?;
    // Single shallow parse: provider detection + shape validation.
    let det = detect_or_shape_err(text)?;
    parse_as(text, det.provider)
}

/// Detect provider or return the correct shape error.
///
/// Combines the top-level type check (NotAnObject) with the shallow detection
/// (MissingMessages, MessagesNotArray).  Separated from [`parse`] so
/// [`parse_with_provider`] can reuse the shape-validation half.
fn detect_or_shape_err(text: &str) -> Result<provider::Detection> {
    let text_trimmed = text.trim();
    let first = text_trimmed.as_bytes().first();
    match first {
        Some(b'[') => return Err(LlmError::NotAnObject("array".to_string())),
        Some(b'"') => return Err(LlmError::NotAnObject("string".to_string())),
        Some(b't') => return Err(LlmError::NotAnObject("bool".to_string())),
        Some(b'f') => return Err(LlmError::NotAnObject("bool".to_string())),
        Some(b'n') => return Err(LlmError::NotAnObject("null".to_string())),
        Some(b'0'..=b'9') | Some(b'-') => return Err(LlmError::NotAnObject("number".to_string())),
        _ => {}
    }

    let det = provider::detect_str(text)?;
    if !det.messages_present {
        return Err(LlmError::MissingMessages);
    }
    if !det.messages_is_array {
        return Err(LlmError::MessagesNotArray("non-array".to_string()));
    }
    Ok(det)
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
    let text = to_utf8(bytes)?;
    check_depth(text)?;
    // Shape validation only (provider hint overrides detection).
    detect_or_shape_err(text)?;
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
///
/// # Structural validation dependency
///
/// This function does NOT validate JSON structure — it only counts nesting tokens.
/// Unbalanced closers (e.g. a leading `}`) are absorbed by `saturating_sub` without
/// error.  Structural validity is enforced by the `serde_json::from_str` calls that
/// follow in `detect_str` and `parse_as`.  If those calls were ever removed or
/// reordered, structurally malformed bodies could pass the depth check silently.
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
