//! Serialization of parsed LLM request bodies back to JSON bytes.
//!
//! The serialize function guarantees:
//!
//! - **Byte-identity (unmutated path):** `serialize(parse(bytes)) == bytes` for all
//!   valid inputs. This holds because every uninterpreted region is stored as a
//!   [`serde_json::value::RawValue`] (raw byte span) and the `preserve_order` feature
//!   keeps key order intact. Resolved Decision 1 confirms this mechanism without
//!   `arbitrary_precision`.
//!
//! - **Determinism:** Serializing the same parsed model twice produces identical bytes.
//!   There is no hash-map ordering, no clock dependence, no RNG.
//!
//! - **Per-version stability:** Bytes produced by one version of this crate remain
//!   stable within that version across runs and OSes.

use crate::{ParsedBody, Result};

/// Serialize a parsed LLM request body back to JSON bytes.
///
/// The returned bytes are byte-identical to the original input when no mutation has
/// been applied (Invariant 5). After mutation, only the replaced payload spans differ
/// from the input (Invariant 8).
///
/// # Errors
///
/// Returns `Err` if serde_json fails to serialize the body. In practice, this is
/// unreachable for a well-formed `ParsedBody` — all fields are either typed values
/// (always serializable) or `RawValue` blobs (already valid JSON). The `Result`
/// return type satisfies the no-panic contract (AC8).
///
/// # Examples
///
/// ```
/// use rskim_llm::{parse, serialize};
///
/// let input = r#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"Hello"}],"max_tokens":1024}"#;
/// let body = parse(input.as_bytes())?;
/// let output = serialize(&body)?;
/// assert_eq!(output, input.as_bytes());
/// # Ok::<(), rskim_llm::LlmError>(())
/// ```
pub fn serialize(body: &ParsedBody) -> Result<Vec<u8>> {
    // Raw-bytes path (normal): return the cached raw bytes verbatim.
    // On parse, raw_bytes holds the original input; after `mutate_block`, it holds
    // the byte-surgery result (original bytes with only the replaced payload span
    // substituted). Either way, returning it verbatim preserves insignificant
    // whitespace, non-canonical number tokens, \uXXXX escape sequences, and
    // arbitrary field ordering outside the mutated span (Invariants 5 & 8).
    let raw = match body {
        ParsedBody::Anthropic(b) => &b.raw_bytes,
        ParsedBody::OpenAi(b) => &b.raw_bytes,
    };
    if !raw.is_empty() {
        return Ok(raw.clone());
    }

    // Typed-fields fallback: only reachable if raw_bytes is empty, which never
    // happens for a body produced by `parse`/`mutate_block` (both always set it).
    // Retained as a defensive path so serialize() is total for any constructible
    // ParsedBody. Field order follows struct declaration order; number formatting
    // is canonical here (no byte-identity guarantee on this path).
    let bytes = serde_json::to_vec(body)?;
    Ok(bytes)
}

/// Serialize a parsed body to a UTF-8 string.
///
/// Convenience wrapper around [`serialize`] for callers that need a `String`.
///
/// # Errors
///
/// Same as [`serialize`]. Additionally returns `Err(LlmError::InvalidUtf8)` if the
/// serialized bytes are not valid UTF-8 — which cannot happen for JSON output from
/// serde_json, so this branch is unreachable in practice.
pub fn serialize_to_string(body: &ParsedBody) -> Result<String> {
    let bytes = serialize(body)?;
    // serde_json always emits valid UTF-8; this is structurally unreachable
    String::from_utf8(bytes).map_err(|e| crate::LlmError::InvalidUtf8(e.to_string()))
}

// Implement serde::Serialize for ParsedBody so the top-level serialize() can
// dispatch to the appropriate body type.
use serde::Serialize;

impl Serialize for ParsedBody {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            ParsedBody::Anthropic(body) => body.serialize(s),
            ParsedBody::OpenAi(body) => body.serialize(s),
        }
    }
}
