//! Provider detection from raw JSON request bodies.
//!
//! Determines whether a request body is an Anthropic or OpenAI format based on
//! structural heuristics applied to the parsed JSON object.

use serde_json::Map;

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

/// Detect the provider from a top-level JSON object.
///
/// Always returns a `Provider` — the detection is a heuristic that falls back to
/// Anthropic when no discriminating signal is found (Anthropic is the most common
/// usage context for this crate).
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
///
/// Heuristic 6 prevents plain OpenAI bodies (no discriminating message-level
/// fields) from being misclassified as Anthropic, which was a latent gap when
/// `parse()` (not `parse_with_provider`) was used with bare OpenAI fixtures.
pub fn detect(obj: &Map<String, serde_json::Value>) -> Provider {
    // Check top-level OpenAI-only fields
    if obj.contains_key("response_format") {
        return Provider::OpenAi;
    }

    // Check messages array for provider-specific fields
    if let Some(serde_json::Value::Array(messages)) = obj.get("messages") {
        for msg in messages {
            let Some(msg_obj) = msg.as_object() else {
                continue;
            };

            // OpenAI-specific message fields
            if msg_obj.contains_key("tool_calls") || msg_obj.contains_key("tool_call_id") {
                return Provider::OpenAi;
            }

            // OpenAI-specific role
            if let Some(serde_json::Value::String(role)) = msg_obj.get("role")
                && role == "developer"
            {
                return Provider::OpenAi;
            }

            // Check content array for Anthropic-specific block types
            if let Some(serde_json::Value::Array(content)) = msg_obj.get("content") {
                for block in content {
                    let Some(blk_obj) = block.as_object() else {
                        continue;
                    };
                    if let Some(serde_json::Value::String(ty)) = blk_obj.get("type") {
                        match ty.as_str() {
                            "tool_use" | "tool_result" | "thinking" => {
                                return Provider::Anthropic;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    // Anthropic-specific top-level field
    if obj.contains_key("max_tokens") {
        return Provider::Anthropic;
    }

    // Model-name prefix heuristic: catch plain OpenAI bodies that lack any
    // message-level discriminant (e.g. `{"model":"gpt-4o","messages":[...]}`
    // with no tool_calls/response_format).  This prevents silent misclassification
    // as Anthropic (applies ADR-001: fix noticed issues immediately).
    if let Some(serde_json::Value::String(model)) = obj.get("model")
        && OPENAI_MODEL_PREFIXES.iter().any(|p| model.starts_with(p))
    {
        return Provider::OpenAi;
    }

    // Default: Anthropic
    Provider::Anthropic
}
