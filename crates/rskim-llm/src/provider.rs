//! Provider detection from raw JSON request bodies.
//!
//! Determines whether a request body is an Anthropic or OpenAI format based on
//! structural heuristics applied to the parsed JSON object.

use serde_json::Map;

/// The detected provider for a request body.
///
/// Provider detection is structural — it inspects field names, not field values.
/// Detection is deterministic and does not depend on runtime state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Detect the provider from a top-level JSON object.
///
/// Returns `Some(Provider)` if the body matches a known provider's structure,
/// or `None` if detection is ambiguous.
///
/// # Detection heuristics
///
/// 1. If any message has `tool_call_id` → OpenAI (Anthropic uses `tool_use_id` inside blocks)
/// 2. If any message has `tool_calls` → OpenAI
/// 3. If any message content array has `type: "tool_use"` or `type: "tool_result"` → Anthropic
/// 4. If any message content array has `type: "thinking"` → Anthropic
/// 5. If top-level has `max_tokens` AND no OpenAI-only fields → Anthropic
/// 6. If top-level has `response_format` → OpenAI
/// 7. Default: Anthropic (most common usage context for this crate)
pub fn detect(obj: &Map<String, serde_json::Value>) -> Option<Provider> {
    // Check top-level OpenAI-only fields
    if obj.contains_key("response_format") {
        return Some(Provider::OpenAi);
    }

    // Check messages array for provider-specific fields
    if let Some(serde_json::Value::Array(messages)) = obj.get("messages") {
        for msg in messages {
            let Some(msg_obj) = msg.as_object() else {
                continue;
            };

            // OpenAI-specific message fields
            if msg_obj.contains_key("tool_calls") || msg_obj.contains_key("tool_call_id") {
                return Some(Provider::OpenAi);
            }

            // OpenAI-specific role
            if let Some(serde_json::Value::String(role)) = msg_obj.get("role")
                && role == "developer"
            {
                return Some(Provider::OpenAi);
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
                                return Some(Provider::Anthropic);
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
        return Some(Provider::Anthropic);
    }

    // Default: Anthropic
    Some(Provider::Anthropic)
}
