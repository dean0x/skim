//! Error types for `rskim-llm`.

use thiserror::Error;

/// Errors that can occur when parsing, serializing, or mutating LLM request bodies.
///
/// All public operations on this crate return `Result<T, LlmError>` — no panics,
/// no partial builds, no silent failures.
///
/// This enum is `#[non_exhaustive]` — new error variants may be added without
/// a breaking change as the crate gains capabilities (additive-only insurance per
/// Resolved Decision 7).
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LlmError {
    /// The input bytes could not be parsed as valid JSON.
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),

    /// The JSON is structurally valid but the top-level value is not an object.
    #[error("expected a JSON object at the top level, got: {0}")]
    NotAnObject(String),

    /// The request body is missing the required `messages` field.
    #[error("request body missing required field `messages`")]
    MissingMessages,

    /// The `messages` field is not a JSON array.
    #[error("`messages` field must be an array, got: {0}")]
    MessagesNotArray(String),

    /// The JSON object nesting depth exceeds the documented maximum.
    ///
    /// See [`crate::MAX_DEPTH`] for the documented bound.
    ///
    /// The error message value (64) must equal [`crate::MAX_DEPTH`]. If MAX_DEPTH
    /// is ever changed, update the string here too (or use a const format string
    /// when Rust stable stabilizes const string formatting).
    #[error("JSON nesting depth {0} exceeds maximum 64")]
    DepthExceeded(u32),

    /// A mutation was requested on a block that does not exist.
    #[error("block with id {0:?} not found")]
    BlockNotFound(String),

    /// A mutation was requested on a block that is not mutable (exempt from mutation).
    ///
    /// Exempt blocks include: `tool_use` inputs, `thinking` blocks, OpenAI opaque fields
    /// (`tool_calls[].function.arguments`, `tool_call_id`, `reasoning`), and unrecognized
    /// block types.
    #[error("block with id {0:?} is not mutable (exempt block type: {1})")]
    BlockNotMutable(String, String),

    /// A mutation was requested on a block that has no text payload.
    #[error("block with id {0:?} has no text payload to replace")]
    NoTextPayload(String),

    /// The input contained invalid UTF-8 bytes.
    #[error("input contains invalid UTF-8: {0}")]
    InvalidUtf8(String),
}
