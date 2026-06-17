//! Provider detection from raw JSON request bodies.
//!
//! Determines whether a request body is an Anthropic or OpenAI format based on
//! structural heuristics applied to the raw JSON text.
//!
//! # Single-pass detection
//!
//! `detect_str` is the primary entry point.  It performs a single shallow
//! deserialization that captures only the fields needed for provider detection
//! (`model`, `response_format`, `max_tokens`, `messages[*].{role,tool_calls,
//! tool_call_id,content[*].type}`), discarding all other fields via
//! `serde::de::IgnoredAny`.  This avoids materialising the full body as a
//! `serde_json::Value` tree — the typed parse in `parse_as` is the only deep
//! parse of the body.
//!
//! `detect_str` is the sole provider-detection entry point for the parse path.

use serde::Deserialize;

/// The detected provider for a request body.
///
/// Provider detection is structural — it inspects field names, not field values.
/// Detection is deterministic and does not depend on runtime state.
///
/// This enum is `#[non_exhaustive]` — future providers can be added without a
/// breaking change (additive-only insurance per Resolved Decision 7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Provider {
    /// An Anthropic `/v1/messages` request body.
    ///
    /// Detected by the presence of a `model` field AND a `messages` field where
    /// messages have an Anthropic-style `content` that is either a string or an array
    /// of objects with Anthropic block `type` values (e.g., `"text"`, `"tool_use"`,
    /// `"tool_result"`, `"thinking"`).
    ///
    /// The most reliable discriminant is the presence of `tool_use`/`tool_result`
    /// block types, or the absence of `role: "developer"` / `role: "tool"` with
    /// `tool_call_id`.
    Anthropic,

    /// An OpenAI `/v1/chat/completions` request body.
    ///
    /// Detected by the presence of a `model` field AND a `messages` field. OpenAI
    /// bodies may have `tool_calls`, `tool_call_id`, `response_format`, or
    /// `function_call` fields at the message level.
    OpenAi,
}

/// Known OpenAI model name prefixes for positive OpenAI detection.
///
/// A `model` field starting with any of these strings is a strong signal that
/// the body is an OpenAI request even when no other discriminating field is
/// present (e.g. a plain `{"model":"gpt-4o","messages":[...]}` with no
/// `tool_calls`/`response_format`).  The list covers documented OpenAI model
/// series as of mid-2025; it is intentionally conservative so false-positives
/// against future Anthropic model names are avoided.
const OPENAI_MODEL_PREFIXES: &[&str] = &["gpt-", "o1-", "o3-", "o4-", "text-davinci", "davinci"];

// ---------------------------------------------------------------------------
// Shallow deserialization structs (single-pass provider detection)
// ---------------------------------------------------------------------------
//
// These structs capture only the fields needed to run the detection heuristics.
// All other fields are silently discarded by serde's default unknown-field
// behaviour (no `deny_unknown_fields`).  Values for presence-only fields
// (response_format, max_tokens, tool_calls, tool_call_id) are consumed via
// `IgnoredAny` so serde does not allocate a Value tree for them.

/// Top-level body — only discriminating fields are captured.
#[derive(Deserialize)]
struct ShallowBody {
    #[serde(default)]
    model: Option<String>,
    /// Presence only — value is discarded.  OpenAI-only field.
    #[serde(default)]
    response_format: Option<serde::de::IgnoredAny>,
    /// Presence only — value is discarded.  Anthropic-only field.
    #[serde(default)]
    max_tokens: Option<serde::de::IgnoredAny>,
    #[serde(default)]
    messages: Option<ShallowMessages>,
}

/// `messages` can be an array (valid) or any other value (shape error surfaced
/// by `detect_str`).
#[derive(Deserialize)]
#[serde(untagged)]
enum ShallowMessages {
    /// The only valid shape — an array of message objects.
    Array(Vec<ShallowMessage>),
    /// Any other JSON value (string, number, object…) — shape error.
    Other(serde::de::IgnoredAny),
}

/// Per-message fields relevant to provider detection.
#[derive(Deserialize)]
struct ShallowMessage {
    #[serde(default)]
    role: Option<String>,
    /// Presence only — value is discarded.  OpenAI-only field.
    #[serde(default)]
    tool_calls: Option<serde::de::IgnoredAny>,
    /// Presence only — value is discarded.  OpenAI-only field.
    #[serde(default)]
    tool_call_id: Option<serde::de::IgnoredAny>,
    /// Content can be a string (both providers) or an array of blocks.
    #[serde(default)]
    content: Option<ShallowContent>,
}

/// Message content — can be a plain string or an array of typed blocks.
#[derive(Deserialize)]
#[serde(untagged)]
enum ShallowContent {
    /// Array of typed blocks — walk for Anthropic-specific `type` values.
    Array(Vec<ShallowBlock>),
    /// Plain string content (both providers) — no block types to check.
    Other(serde::de::IgnoredAny),
}

/// A single content block — only the `type` discriminant is captured.
#[derive(Deserialize)]
struct ShallowBlock {
    #[serde(rename = "type", default)]
    block_type: Option<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Outcome of [`detect_str`] — the detected provider plus a shape flag.
pub(crate) struct Detection {
    /// The detected provider (Anthropic or OpenAI).
    pub provider: Provider,
    /// True iff the `messages` field is present and is a JSON array.
    /// `parse()` returns `LlmError::MessagesNotArray` when this is false.
    pub messages_is_array: bool,
    /// True iff the `messages` field is present at all.
    /// `parse()` returns `LlmError::MissingMessages` when this is false.
    pub messages_present: bool,
}

/// Detect the provider from raw JSON text in a single shallow parse.
///
/// Performs one `serde_json::from_str` into a minimal struct that captures
/// only the fields needed for provider detection, discarding all other
/// field values via `serde::de::IgnoredAny`.  This avoids allocating a full
/// `serde_json::Value` tree for the body; the typed parse in `parse_as` is
/// the only deep parse.
///
/// # Errors
///
/// Returns `Err` when `text` is not valid JSON or is not a top-level JSON
/// object.  Shape errors (`messages` absent or not an array) are reported
/// via the [`Detection`] flags rather than as errors so that the caller can
/// produce the appropriate [`crate::LlmError`] variant.
pub(crate) fn detect_str(text: &str) -> crate::Result<Detection> {
    // A single shallow parse — O(n) scan, minimal allocation.
    let body: ShallowBody = serde_json::from_str(text).map_err(crate::LlmError::Json)?;

    let (messages_present, messages_is_array, msgs) = match body.messages {
        None => (false, false, None),
        Some(ShallowMessages::Other(_)) => (true, false, None),
        Some(ShallowMessages::Array(v)) => (true, true, Some(v)),
    };

    let provider = detect_from_parts(
        body.response_format.is_some(),
        body.max_tokens.is_some(),
        body.model.as_deref(),
        msgs.as_deref(),
    );

    Ok(Detection {
        provider,
        messages_is_array,
        messages_present,
    })
}

/// Core detection logic — pure function over already-extracted fields.
///
/// Accepts the same discriminating signals as the original [`detect`] but
/// operates on already-extracted data rather than a `serde_json::Value` map.
/// This keeps the heuristic logic in one place, testable independently of
/// the deserialization path.
///
/// # Detection heuristics (in order)
///
/// 1. If top-level has `response_format` → OpenAI
/// 2. If any message has `tool_call_id` or `tool_calls` → OpenAI
/// 3. If any message has `role: "developer"` → OpenAI
/// 4. If any message content array has `type: "tool_use"`, `"tool_result"`, or `"thinking"` → Anthropic
/// 5. If top-level has `max_tokens` → Anthropic
/// 6. If `model` starts with a known OpenAI prefix (e.g. `"gpt-"`, `"o1-"`) → OpenAI
/// 7. Default: Anthropic
fn detect_from_parts(
    has_response_format: bool,
    has_max_tokens: bool,
    model: Option<&str>,
    messages: Option<&[ShallowMessage]>,
) -> Provider {
    if has_response_format {
        return Provider::OpenAi;
    }

    if let Some(msgs) = messages {
        for msg in msgs {
            if msg.tool_calls.is_some() || msg.tool_call_id.is_some() {
                return Provider::OpenAi;
            }
            if msg.role.as_deref() == Some("developer") {
                return Provider::OpenAi;
            }
            if let Some(ShallowContent::Array(blocks)) = &msg.content {
                for block in blocks {
                    match block.block_type.as_deref() {
                        Some("tool_use") | Some("tool_result") | Some("thinking") => {
                            return Provider::Anthropic;
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    if has_max_tokens {
        return Provider::Anthropic;
    }

    if let Some(m) = model
        && OPENAI_MODEL_PREFIXES.iter().any(|p| m.starts_with(p))
    {
        return Provider::OpenAi;
    }

    Provider::Anthropic
}
