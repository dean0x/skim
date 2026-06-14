//! OpenAI `/v1/chat/completions` request body model.
//!
//! This module models the OpenAI Chat Completions API request body. Unknown fields
//! at any level are retained as raw byte blobs to guarantee byte-identical round-trips.
//!
//! # OpenAI opaque/sacrosanct fields
//!
//! Per Resolved Decision 6 (DECISIONS-RESOLVED.md 2026-06-13), these fields are
//! explicitly named as classifier-exempt and sacrosanct:
//!
//! - `tool_calls[].function.arguments` — model-emitted JSON string, never re-parsed
//!   or reformatted
//! - `tool_call_id` — correlation identifier, never inspected
//! - `reasoning` / reasoning-token content on reasoning models — analogous to
//!   Anthropic `thinking`, never compressible
//!
//! Default-deny is the catch-all: any field not listed above is also treated as
//! opaque if it is not explicitly modeled. This ensures an evolved OpenAI schema
//! never loses byte-faithfulness.
//!
//! # Non-exhaustive types
//!
//! Public enums are `#[non_exhaustive]` where applicable. The `OpenAiRole` enum
//! is exhaustive over the five documented roles but new roles land in
//! `extra_fields` via the `flatten` mechanism on `OpenAiMessage`.

use serde::{Deserialize, Serialize};

use super::RawBlob;

/// A complete OpenAI `/v1/chat/completions` request body.
///
/// # Byte-identical round-trips
///
/// See [`AnthropicBody`] for the raw-bytes cache rationale. The same mechanism
/// applies here: `raw_bytes` is stored on parse and returned verbatim on
/// unmutated serialize.
///
/// # No-envelope-mutation invariant (AC11)
///
/// The structural fields `model`, `messages`, and `extra_fields` are intentionally
/// not `pub` to prevent callers from dropping, reordering, duplicating, or adding
/// messages, or mutating envelope fields through this crate's public API.
/// Read-only access is provided via [`OpenAiBody::model`], [`OpenAiBody::messages`],
/// and [`OpenAiBody::extra_fields`]. Envelope mutation lives in a separate layer
/// above this crate (Resolved Decision 7; AC11).
///
/// [`AnthropicBody`]: crate::model::anthropic::AnthropicBody
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiBody {
    /// The model identifier (e.g., `"gpt-4o"`).
    ///
    /// Not `pub` — use [`OpenAiBody::model`] for read-only access.
    pub(crate) model: String,

    /// The conversation messages.
    ///
    /// Not `pub` — use [`OpenAiBody::messages`] for read-only access.
    pub(crate) messages: Vec<OpenAiMessage>,

    /// Unknown top-level fields retained for the fall-back rebuild path.
    ///
    /// Not `pub` — use [`OpenAiBody::extra_fields`] for read-only access.
    #[serde(flatten)]
    pub(crate) extra_fields: serde_json::Map<String, serde_json::Value>,

    /// Original JSON bytes for byte-identical unmutated serialize.
    ///
    /// Set by [`crate::parse`] from the input bytes. Not serialized.
    #[serde(skip)]
    pub(crate) raw_bytes: Vec<u8>,
}

impl OpenAiBody {
    /// The model identifier (e.g., `"gpt-4o"`).
    ///
    /// Read-only — envelope mutation is not supported in this crate (AC11).
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The conversation messages, in order.
    ///
    /// Returns an immutable slice — structural manipulation (push/remove/reorder)
    /// is not supported through this crate's public API (AC11 no-turn-manipulation
    /// invariant).
    pub fn messages(&self) -> &[OpenAiMessage] {
        &self.messages
    }

    /// Unknown top-level fields retained for byte-identical round-trips.
    ///
    /// Read-only — inserting or removing fields is envelope mutation and is
    /// not supported in this crate (Resolved Decision 7; AC11).
    pub fn extra_fields(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.extra_fields
    }
}

/// A single message in an OpenAI chat conversation.
///
/// The five documented roles are `system`, `developer`, `user`, `assistant`, and `tool`.
/// The `role` field is preserved as a raw string to handle future roles gracefully.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiMessage {
    /// The message role.
    pub role: String,

    /// The message content — either a string, an array of content parts, or absent
    /// (for assistant messages with only tool_calls).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<OpenAiContent>,

    /// Tool calls made by the assistant (assistant messages only).
    ///
    /// This field is present only on assistant messages that invoke tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<OpenAiToolCall>>,

    /// Tool call correlation identifier (tool messages only).
    ///
    /// **Sacrosanct / classifier-exempt per Resolved Decision 6.** This is an opaque
    /// correlator and must never be inspected, classified, or mutated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,

    /// Reasoning-model content (o1/o3 series).
    ///
    /// **Sacrosanct / classifier-exempt per Resolved Decision 6.** Analogous to
    /// Anthropic `thinking` — opaque reasoning tokens, never compressible.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<RawBlob>,

    /// Unknown fields retained verbatim (covers `name`, `refusal`, legacy `function_call`, etc.).
    #[serde(flatten)]
    pub extra_fields: serde_json::Map<String, serde_json::Value>,
}

/// OpenAI message content — string or array of content parts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpenAiContent {
    /// Plain string content.
    Text(String),
    /// Array of typed content parts.
    Parts(Vec<OpenAiContentPart>),
}

/// A single content part in an OpenAI message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiContentPart {
    /// The part type (e.g., `"text"`, `"image_url"`).
    #[serde(rename = "type")]
    pub part_type: String,

    /// Text content (present for `"text"` parts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    /// Unknown fields retained verbatim (covers image_url, etc.).
    #[serde(flatten)]
    pub extra_fields: serde_json::Map<String, serde_json::Value>,
}

/// A tool call made by the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiToolCall {
    /// The tool call identifier.
    pub id: String,

    /// Always `"function"` in current API.
    #[serde(rename = "type")]
    pub call_type: String,

    /// The function being called.
    pub function: OpenAiFunctionCall,

    /// Unknown fields retained verbatim.
    #[serde(flatten)]
    pub extra_fields: serde_json::Map<String, serde_json::Value>,
}

/// The function name and arguments in a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiFunctionCall {
    /// The function name.
    pub name: String,

    /// The function arguments — model-emitted JSON string.
    ///
    /// **Sacrosanct / classifier-exempt per Resolved Decision 6.** This is an opaque
    /// JSON string produced by the model and must be preserved byte-for-byte. It is
    /// exempt from classification (returns `unknown` if a class is requested) and
    /// exempt from mutation.
    pub arguments: String,

    /// Unknown fields retained verbatim.
    #[serde(flatten)]
    pub extra_fields: serde_json::Map<String, serde_json::Value>,
}
