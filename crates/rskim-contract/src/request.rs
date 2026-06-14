//! Byte-preserving structural view of LLM request bodies.
//!
//! This module provides a **minimal** parse of Anthropic `/v1/messages` and
//! OpenAI `/v1/chat/completions` request bodies sufficient to:
//! - Locate the envelope fields (`model`, `metadata`, transport map)
//! - Identify the turn boundary (messages array)
//! - Find the live zone (trailing run after last assistant message)
//! - Identify hot-zone and sacrosanct content
//!
//! # Byte preservation
//!
//! The structural view stores parsed `serde_json::Value` clones of each message
//! object, not raw byte offsets into the original buffer. Byte-offset derivation
//! for hot-zone splicing is a per-consumer responsibility (#302); this layer
//! provides the structural parse and zone-boundary classification only.
//! Hot-zone content must be re-emitted from the original buffer by
//! [`crate::zone::splice_hot_zone`] — re-serialization via `serde_json::to_vec`
//! is forbidden because it can change key order, number tokens, or whitespace,
//! busting the prompt cache. See `zone.rs` for the splice mechanism.
//!
//! # Bounded recursion (AC17)
//!
//! serde_json imposes a default recursion limit of 128 nested containers.
//! This module sets an explicit depth bound of 64 for the structural analysis
//! functions, tighter than serde_json's ceiling, to account for Windows's
//! smaller default stack size. Inputs exceeding the depth bound resolve to
//! fail-open passthrough without panic or timeout.
//!
//! # Provider schemas
//!
//! The structural view covers both provider schemas. Key differences:
//! - Anthropic: role `"assistant"`, thinking blocks in `content[]`
//! - OpenAI: role `"assistant"`, `"tool"` role for tool results, `tool_calls[]`
//!
//! # Sacrosanct fields
//!
//! The following fields pass through byte-identical (invariant 7):
//! - `model` — provider routing; any rewrite causes wrong-model requests
//! - `metadata` — pass-through to provider; semantics unknown
//! - Transport headers — opaque bytes; the contract holds an `(name, bytes)` map
//! - OpenAI-specific opaque fields: `tool_calls[].function.arguments`,
//!   `tool_call_id`, `reasoning` / reasoning-token content

use serde_json::Value;

/// Maximum nesting depth for structural analysis (invariant / AC17).
///
/// serde_json's documented ceiling is 128. We use 64 to leave headroom
/// on Windows (smaller default thread stack). Inputs deeper than this
/// resolve to fail-open passthrough.
pub const MAX_ANALYSIS_DEPTH: usize = 64;

/// Provider schema variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Provider {
    /// Anthropic `/v1/messages` schema.
    Anthropic,
    /// OpenAI `/v1/chat/completions` schema.
    OpenAI,
}

/// The role of a message turn in the conversation.
///
/// All roles — including unrecognized ones — are retained as turn slots so
/// that `Turn.index` and `ZoneBoundary.turn_count` share the same index space.
/// Silently dropping unknown-role turns would misalign `last_assistant_index`
/// (original-space) with `turn_count` (filtered-space), producing incorrect
/// zone boundaries. Use `TurnRole::Other` for any role not explicitly listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TurnRole {
    /// A user or human turn.
    User,
    /// An assistant / model turn.
    Assistant,
    /// A system message (OpenAI) or top-level `system` field (Anthropic).
    System,
    /// A tool result turn (OpenAI `"tool"` role, Anthropic `tool_result` block).
    Tool,
    /// An unrecognized or future role (e.g., OpenAI `"developer"`, custom roles).
    ///
    /// Retained as a turn slot to preserve index-space alignment with the original
    /// messages array. A dropped unknown-role turn would shift `turn_count` relative
    /// to `last_assistant_index`, making `live_zone_range()` inaccurate.
    Other,
}

impl TurnRole {
    /// Parse a role string from either provider schema.
    ///
    /// Always returns `Some` — unknown roles map to [`TurnRole::Other`] rather
    /// than `None`, preserving the turn slot and keeping index spaces aligned.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" | "human" => Some(TurnRole::User),
            "assistant" | "model" => Some(TurnRole::Assistant),
            "system" => Some(TurnRole::System),
            "tool" => Some(TurnRole::Tool),
            // OpenAI "developer" role (added 2024) and any other unknown role.
            _ => Some(TurnRole::Other),
        }
    }
}

/// A single turn (message) in the conversation array.
#[derive(Debug, Clone)]
pub struct Turn {
    /// Zero-based index in the messages array.
    pub index: usize,
    /// The role of this turn.
    pub role: TurnRole,
    /// The parsed JSON value of this message object (cloned from the parsed body).
    ///
    /// This is an owned `serde_json::Value` clone, NOT byte offsets into the original
    /// buffer. Re-emitting via `serde_json::to_vec` is explicitly **forbidden** for
    /// hot-zone turns because it may change key order, number tokens, or whitespace,
    /// busting the prompt cache (invariant 3). For hot-zone turns, callers must splice
    /// directly from the original buffer via [`crate::zone::splice_hot_zone`] using
    /// byte offsets derived by the #302 consumer.
    pub value: Value,
}

/// Zone boundary for the messages array.
///
/// The live zone is the trailing run of turns after the last assistant turn.
/// The hot zone is everything up to and including the last assistant turn.
///
/// # Zone boundary edge cases (AC6)
///
/// 1. **Zero assistant messages** — entire array is live zone; hot zone is empty.
/// 2. **Trailing assistant message** — live zone is empty; the whole array is hot.
///    Any transform MUST emit passthrough (no live content to modify).
/// 3. **Consecutive trailing user/tool messages** — all consecutive trailing
///    non-assistant messages form the live zone.
#[derive(Debug, Clone)]
pub struct ZoneBoundary {
    /// Index of the last assistant turn in the messages array.
    /// `None` when there are no assistant messages (whole array is live).
    pub last_assistant_index: Option<usize>,
    /// Total number of turns.
    pub turn_count: usize,
}

impl ZoneBoundary {
    /// Returns `true` if the live zone is empty (trailing assistant message).
    ///
    /// When live zone is empty, the correct action is passthrough.
    pub fn is_live_zone_empty(&self) -> bool {
        match self.last_assistant_index {
            // Last assistant is the final turn → no trailing user/tool → empty live zone.
            Some(idx) => idx + 1 >= self.turn_count,
            // No assistant at all → whole array is live.
            None => false,
        }
    }

    /// Returns the index range of the live zone (exclusive start, exclusive end).
    ///
    /// The live zone starts at `last_assistant_index + 1` (or 0 if no assistant)
    /// and ends at `turn_count`.
    pub fn live_zone_range(&self) -> std::ops::Range<usize> {
        let start = match self.last_assistant_index {
            Some(idx) => idx + 1,
            None => 0,
        };
        start..self.turn_count
    }
}

/// The minimal structural view of an LLM request body.
///
/// Produced by [`parse_request`]. Contains only what the contract needs to:
/// - Identify the provider
/// - Locate zone boundaries
/// - Enumerate turns for live-zone editing
/// - Verify sacrosanct-field passthrough
///
/// The view does NOT own the original bytes. The caller retains the original
/// buffer for hot-zone splicing (invariant 3).
#[derive(Debug)]
pub struct StructuralView {
    /// Detected provider schema.
    pub provider: Provider,
    /// The `model` field value (sacrosanct — must pass through unchanged).
    pub model: Option<String>,
    /// Zone boundary derived from the messages array.
    pub zone: ZoneBoundary,
    /// All turns in the messages array.
    pub turns: Vec<Turn>,
    /// Whether the request contains thinking/reasoning blocks.
    /// Thinking blocks are sacrosanct in both zones (invariant 7).
    pub has_thinking_blocks: bool,
}

/// Parse an LLM request body into a structural view.
///
/// Returns `None` when:
/// - The input is not valid UTF-8
/// - The input is not valid JSON
/// - The top-level value is not a JSON object
/// - Nesting depth exceeds [`MAX_ANALYSIS_DEPTH`]
///
/// In all `None` cases, the caller MUST emit passthrough (fail-open rule).
///
/// # Examples
///
/// ```rust
/// use rskim_contract::request::{parse_request, Provider};
///
/// let body = br#"{"model":"claude-3-5-sonnet-20241022","max_tokens":1024,"messages":[{"role":"user","content":"hi"}]}"#;
/// let view = parse_request(body).expect("must parse valid JSON");
/// assert_eq!(view.provider, Provider::Anthropic);
/// assert_eq!(view.turns.len(), 1);
/// ```
pub fn parse_request(input: &[u8]) -> Option<StructuralView> {
    // Check depth before full parse to detect pathological nesting.
    // serde_json will enforce its own 128-level ceiling during Value parsing,
    // but we check at our tighter bound first.
    if estimated_depth_exceeds(input, MAX_ANALYSIS_DEPTH) {
        return None;
    }

    let s = std::str::from_utf8(input).ok()?;
    let value: Value = serde_json::from_str(s).ok()?;
    let obj = value.as_object()?;

    // Detect provider from schema shape.
    let provider = detect_provider(obj);

    // Extract model (sacrosanct field).
    let model = obj.get("model").and_then(|v| v.as_str()).map(String::from);

    // Parse messages array.
    //
    // All slots are retained — including unknown roles — so that `Turn.index` and
    // `ZoneBoundary.turn_count` share the same index space as the original array.
    // Silently dropping unknown-role turns would misalign `last_assistant_index`
    // (original-space) with `turn_count` (filtered-space), producing inverted or
    // empty live-zone ranges for requests that mix in unknown-role messages
    // (e.g., OpenAI `"developer"` turns). Unknown roles map to `TurnRole::Other`.
    let messages = obj.get("messages").and_then(|v| v.as_array());
    let turns: Vec<Turn> = messages
        .map(|arr| {
            arr.iter()
                .enumerate()
                .map(|(i, msg)| {
                    // `TurnRole::parse` always returns Some (unknown → Other).
                    // If role is missing or not a string, treat as Other.
                    let role = msg
                        .get("role")
                        .and_then(|v| v.as_str())
                        .and_then(TurnRole::parse)
                        .unwrap_or(TurnRole::Other);
                    Turn {
                        index: i,
                        role,
                        value: msg.clone(),
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // Locate last assistant turn for zone boundary.
    // `Turn.index` is the original array index; `turn_count` is the original array
    // length — both index spaces are aligned because no turns were dropped.
    let last_assistant_index = turns
        .iter()
        .rev()
        .find(|t| t.role == TurnRole::Assistant)
        .map(|t| t.index);

    // Use the messages array length (not turns.len()) so that turn_count is always
    // the original-space count even if future code changes the collection logic.
    let turn_count = messages.map(|arr| arr.len()).unwrap_or(0);
    let zone = ZoneBoundary {
        last_assistant_index,
        turn_count,
    };

    // Detect thinking/reasoning blocks (sacrosanct in both zones).
    let has_thinking_blocks = detect_thinking_blocks(obj);

    Some(StructuralView {
        provider,
        model,
        zone,
        turns,
        has_thinking_blocks,
    })
}

/// Detect the provider schema from the top-level object keys.
///
/// Anthropic `/v1/messages` uses `"max_tokens"` as a required field.
/// OpenAI `/v1/chat/completions` uses `"messages"` + optional `"temperature"`.
/// The presence of `"max_tokens"` is the strongest Anthropic signal; otherwise
/// default to OpenAI.
fn detect_provider(obj: &serde_json::Map<String, Value>) -> Provider {
    // Anthropic requires `max_tokens` in the top-level object.
    // OpenAI has `max_tokens` as optional (deprecated) but uses `max_completion_tokens`.
    // The Anthropic `stream` + `max_tokens` combination is the reliable discriminant.
    if obj.contains_key("max_tokens") && !obj.contains_key("max_completion_tokens") {
        Provider::Anthropic
    } else {
        Provider::OpenAI
    }
}

/// Returns `true` if the input contains thinking/reasoning blocks.
///
/// Anthropic thinking blocks have `"type": "thinking"` inside message content.
/// OpenAI reasoning content uses `"reasoning"` keys in message objects.
/// Both are sacrosanct and must pass through byte-identical.
fn detect_thinking_blocks(obj: &serde_json::Map<String, Value>) -> bool {
    // Check messages array for thinking-type content blocks.
    if let Some(messages) = obj.get("messages").and_then(|v| v.as_array()) {
        for msg in messages {
            if let Some(content) = msg.get("content").and_then(|v| v.as_array()) {
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) == Some("thinking") {
                        return true;
                    }
                }
            }
            // OpenAI reasoning content.
            if msg.get("reasoning").is_some() {
                return true;
            }
        }
    }
    false
}

/// Estimate whether input JSON nesting exceeds the given depth limit.
///
/// Uses a fast byte-scan heuristic: count consecutive `[` and `{` depth.
/// This is a conservative over-estimate (string content containing brackets
/// will inflate the count), which is acceptable — false positives fall back
/// to passthrough rather than crash.
///
/// The actual serde_json parser will enforce its own 128-level ceiling, but
/// this check catches the pathological case before allocating.
///
/// # Bounds
///
/// - `max_depth` must be ≤ 128 (serde_json's ceiling).
/// - Uses `u32` for depth counter to avoid overflow on adversarial input
///   (a 64GB file of `[` characters would overflow `usize` on 32-bit targets;
///   `u32` overflows at 4G characters, but we bail early on exceed anyway).
fn estimated_depth_exceeds(input: &[u8], max_depth: usize) -> bool {
    // Widening to u32 before comparison with max_depth (PF-004).
    let max_u32 = max_depth.min(128) as u32;
    let mut depth: u32 = 0;
    let mut in_string = false;
    let mut prev_backslash = false;

    for &b in input {
        if prev_backslash {
            prev_backslash = false;
            continue;
        }
        if b == b'\\' && in_string {
            prev_backslash = true;
            continue;
        }
        if b == b'"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match b {
            b'[' | b'{' => {
                // Saturating add: on overflow (impossible for sane inputs but safe), bail.
                depth = depth.saturating_add(1);
                if depth > max_u32 {
                    return true;
                }
            }
            b']' | b'}' => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    false
}

/// Extract the names of OpenAI sacrosanct / provider-opaque fields.
///
/// These fields must pass through byte-identical (invariant 7 / AC12):
/// - `tool_calls[].function.arguments` — opaque model-emitted JSON string
/// - `tool_call_id` — correlator
/// - `reasoning` — reasoning-token content on reasoning models
///
/// Default-deny protects anything not listed here; this list names the
/// *known* opaque fields for clarity, per Decision 6 / DECISIONS-RESOLVED.md.
///
/// The list is returned as a static slice for use in harness assertions.
pub fn openai_sacrosanct_field_names() -> &'static [&'static str] {
    &[
        "tool_calls[].function.arguments",
        "tool_call_id",
        "reasoning",
    ]
}

/// Extract the names of Anthropic sacrosanct / provider-opaque fields.
///
/// These fields must pass through byte-identical (invariant 7 / AC12):
/// - `signature` — Anthropic thinking block signature
/// - `encrypted_content` — encrypted thinking content
/// - `redacted_thinking.data` — redacted thinking block data
/// - `compaction.encrypted_content` — compaction envelope
pub fn anthropic_sacrosanct_field_names() -> &'static [&'static str] {
    &[
        "signature",
        "encrypted_content",
        "redacted_thinking.data",
        "compaction.encrypted_content",
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // ========================================================================
    // estimated_depth_exceeds tests
    // ========================================================================

    #[test]
    fn depth_check_flat_object_passes() {
        let input = br#"{"key": "value"}"#;
        assert!(!estimated_depth_exceeds(input, MAX_ANALYSIS_DEPTH));
    }

    #[test]
    fn depth_check_shallow_nesting_passes() {
        let input = br#"{"messages": [{"role": "user"}]}"#;
        assert!(!estimated_depth_exceeds(input, MAX_ANALYSIS_DEPTH));
    }

    #[test]
    fn depth_check_exceeds_limit() {
        // Create nesting deeper than MAX_ANALYSIS_DEPTH.
        let open: String = "[".repeat(MAX_ANALYSIS_DEPTH + 5);
        let close: String = "]".repeat(MAX_ANALYSIS_DEPTH + 5);
        let input = format!("{open}{close}");
        assert!(estimated_depth_exceeds(
            input.as_bytes(),
            MAX_ANALYSIS_DEPTH
        ));
    }

    #[test]
    fn depth_check_brackets_inside_string_not_counted() {
        // Brackets inside a JSON string must not inflate the depth counter.
        let input = br#"{"key": "[[[[[[[[[[[[[[[[[[[[not nested]]]]]]]]]]]]]]]]]]]]"}"#;
        assert!(!estimated_depth_exceeds(input, 5));
    }

    #[test]
    fn depth_check_pathological_nesting_no_panic() {
        // 200 levels deep — should return true without panicking.
        let open: String = "[".repeat(200);
        let close: String = "]".repeat(200);
        let input = format!("{open}{close}");
        let result = estimated_depth_exceeds(input.as_bytes(), MAX_ANALYSIS_DEPTH);
        assert!(result); // 200 > 64
    }

    // ========================================================================
    // parse_request tests
    // ========================================================================

    #[test]
    fn parse_anthropic_simple_request() {
        let body = br#"{"model":"claude-3-5-sonnet-20241022","max_tokens":1024,"messages":[{"role":"user","content":"Hello"}]}"#;
        let view = parse_request(body).expect("must parse");
        assert_eq!(view.provider, Provider::Anthropic);
        assert_eq!(view.model.as_deref(), Some("claude-3-5-sonnet-20241022"));
        assert_eq!(view.turns.len(), 1);
        assert_eq!(view.turns[0].role, TurnRole::User);
    }

    #[test]
    fn parse_openai_simple_request() {
        let body = br#"{"model":"gpt-4o","messages":[{"role":"user","content":"Hello"},{"role":"assistant","content":"Hi!"}]}"#;
        let view = parse_request(body).expect("must parse");
        assert_eq!(view.provider, Provider::OpenAI);
        assert_eq!(view.turns.len(), 2);
        assert_eq!(view.turns[0].role, TurnRole::User);
        assert_eq!(view.turns[1].role, TurnRole::Assistant);
    }

    #[test]
    fn parse_invalid_utf8_returns_none() {
        let bad_bytes = &[0xff, 0xfe, 0x00];
        assert!(parse_request(bad_bytes).is_none());
    }

    #[test]
    fn parse_invalid_json_returns_none() {
        assert!(parse_request(b"{not json}").is_none());
    }

    #[test]
    fn parse_empty_returns_none() {
        assert!(parse_request(b"").is_none());
    }

    #[test]
    fn parse_over_depth_returns_none() {
        let open: String = "[".repeat(MAX_ANALYSIS_DEPTH + 10);
        let close: String = "]".repeat(MAX_ANALYSIS_DEPTH + 10);
        let input = format!("{open}{close}");
        // This returns None because estimated_depth_exceeds bails early.
        assert!(parse_request(input.as_bytes()).is_none());
    }

    // ========================================================================
    // ZoneBoundary tests (AC6 edge cases)
    // ========================================================================

    #[test]
    fn zone_zero_assistant_messages_whole_array_live() {
        let zone = ZoneBoundary {
            last_assistant_index: None,
            turn_count: 3,
        };
        assert!(!zone.is_live_zone_empty());
        assert_eq!(zone.live_zone_range(), 0..3);
    }

    #[test]
    fn zone_trailing_assistant_empty_live_zone() {
        // Last message is assistant → live zone empty → should passthrough.
        let zone = ZoneBoundary {
            last_assistant_index: Some(2),
            turn_count: 3,
        };
        assert!(zone.is_live_zone_empty());
        assert_eq!(zone.live_zone_range(), 3..3); // empty range
    }

    #[test]
    fn zone_assistant_followed_by_user_tool() {
        let zone = ZoneBoundary {
            last_assistant_index: Some(1),
            turn_count: 4,
        };
        assert!(!zone.is_live_zone_empty());
        assert_eq!(zone.live_zone_range(), 2..4); // turns 2 and 3 are live
    }

    #[test]
    fn zone_boundary_parse_trailing_user() {
        let body = br#"{"model":"claude-3-5-sonnet-20241022","max_tokens":1024,"messages":[
            {"role":"user","content":"first"},
            {"role":"assistant","content":"response"},
            {"role":"user","content":"follow up"}
        ]}"#;
        let view = parse_request(body).expect("must parse");
        assert!(!view.zone.is_live_zone_empty());
        assert_eq!(view.zone.live_zone_range(), 2..3); // only the last user turn
    }

    #[test]
    fn zone_boundary_all_assistant_live_empty() {
        let body = br#"{"model":"claude-3-5-sonnet-20241022","max_tokens":1024,"messages":[
            {"role":"user","content":"first"},
            {"role":"assistant","content":"response"}
        ]}"#;
        let view = parse_request(body).expect("must parse");
        assert!(view.zone.is_live_zone_empty());
    }

    // ========================================================================
    // Sacrosanct field lists
    // ========================================================================

    #[test]
    fn openai_sacrosanct_includes_tool_calls_arguments() {
        let fields = openai_sacrosanct_field_names();
        assert!(fields.contains(&"tool_calls[].function.arguments"));
        assert!(fields.contains(&"tool_call_id"));
        assert!(fields.contains(&"reasoning"));
    }

    #[test]
    fn anthropic_sacrosanct_includes_signature() {
        let fields = anthropic_sacrosanct_field_names();
        assert!(fields.contains(&"signature"));
        assert!(fields.contains(&"encrypted_content"));
    }

    // ========================================================================
    // Thinking block detection
    // ========================================================================

    #[test]
    fn detect_thinking_blocks_anthropic() {
        let body = br#"{"model":"claude-3-5-sonnet-20241022","max_tokens":8096,"messages":[
            {"role":"assistant","content":[
                {"type":"thinking","thinking":"let me think..."},
                {"type":"text","text":"hello"}
            ]}
        ]}"#;
        let view = parse_request(body).expect("must parse");
        assert!(view.has_thinking_blocks);
    }

    #[test]
    fn detect_no_thinking_blocks() {
        let body = br#"{"model":"gpt-4o","messages":[
            {"role":"user","content":"hi"}
        ]}"#;
        let view = parse_request(body).expect("must parse");
        assert!(!view.has_thinking_blocks);
    }

    // ========================================================================
    // TurnRole
    // ========================================================================

    #[test]
    fn turn_role_parse_known_roles() {
        assert_eq!(TurnRole::parse("user"), Some(TurnRole::User));
        assert_eq!(TurnRole::parse("human"), Some(TurnRole::User));
        assert_eq!(TurnRole::parse("assistant"), Some(TurnRole::Assistant));
        assert_eq!(TurnRole::parse("model"), Some(TurnRole::Assistant));
        assert_eq!(TurnRole::parse("system"), Some(TurnRole::System));
        assert_eq!(TurnRole::parse("tool"), Some(TurnRole::Tool));
    }

    #[test]
    fn turn_role_parse_unknown_returns_other() {
        // Unknown roles map to TurnRole::Other (not None) to preserve index-space
        // alignment: no turn slot is dropped, so Turn.index and ZoneBoundary.turn_count
        // remain consistent with the original messages array positions.
        assert_eq!(TurnRole::parse("unknown_role"), Some(TurnRole::Other));
        assert_eq!(TurnRole::parse(""), Some(TurnRole::Other));
        // OpenAI "developer" role (added 2024) maps to Other until explicitly added.
        assert_eq!(TurnRole::parse("developer"), Some(TurnRole::Other));
    }
}
