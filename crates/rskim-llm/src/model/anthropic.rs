//! Anthropic `/v1/messages` request body model.
//!
//! This module models the Anthropic Messages API request body. Unknown fields at any
//! level are retained as raw byte blobs to guarantee byte-identical round-trips.
//!
//! # Non-exhaustive types
//!
//! All public enums are `#[non_exhaustive]` — new block types added by Anthropic in
//! future schema versions will fall through to the `Unknown` variant rather than
//! failing to parse, and no callers can match exhaustively (additive-only insurance,
//! per Resolved Decision 7).

use serde::{Deserialize, Serialize};

use super::RawBlob;

/// A complete Anthropic `/v1/messages` request body.
///
/// # Byte-identical round-trips
///
/// Byte identity is achieved via a raw-bytes cache (`raw_bytes`): the original
/// JSON bytes are stored verbatim on parse and returned by [`crate::serialize`]
/// without re-encoding. This preserves insignificant whitespace, non-canonical
/// number tokens (`1.0e3`), `\uXXXX` escapes, and arbitrary field ordering.
///
/// After mutation, `raw_bytes` is cleared and serialization rebuilds from the
/// typed fields, which may reformat the output (field order follows struct
/// declaration order, numbers use canonical representation). Only the mutated
/// path loses verbatim formatting — the unmutated path is always byte-identical.
///
/// `extra_fields` retains all top-level fields not modeled as typed members.
/// It is used only on the mutated-body path; on the unmutated path, `raw_bytes`
/// is used instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicBody {
    /// The model identifier (e.g., `"claude-3-5-sonnet-20241022"`).
    pub model: String,

    /// The conversation turns.
    pub messages: Vec<AnthropicMessage>,

    /// Unknown top-level fields retained for the mutated-rebuild path.
    ///
    /// This includes `system`, `max_tokens`, `tools`, `temperature`, `top_p`,
    /// `stream`, `tool_choice`, `metadata`, and any other top-level fields.
    /// These are serialized using their `serde_json::Value` representation (not
    /// verbatim source bytes) and are only used when `raw_bytes` is empty.
    #[serde(flatten)]
    pub extra_fields: serde_json::Map<String, serde_json::Value>,

    /// Original JSON bytes for byte-identical unmutated serialize.
    ///
    /// Set by [`crate::parse`] from the input bytes. Cleared by
    /// `mutate_block` after any mutation so the typed-field path is used.
    /// Not serialized — this is crate-internal state only.
    #[serde(skip)]
    pub(crate) raw_bytes: Vec<u8>,
}

/// The `system` field — either a plain string or an array of content blocks.
///
/// Both forms are valid in the Anthropic API. The array form supports `cache_control`
/// on individual system entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnthropicSystem {
    /// Plain string system prompt.
    Text(String),
    /// Array of system content blocks (supports cache_control).
    Blocks(Vec<AnthropicSystemBlock>),
}

/// A single block in a system content array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicSystemBlock {
    /// Always `"text"` for system blocks.
    #[serde(rename = "type")]
    pub block_type: String,

    /// The text content.
    pub text: String,

    /// Optional cache control marker.
    ///
    /// Stored as `serde_json::Value` rather than `RawBlob` because `cache_control`
    /// appears in contexts deserialized through `#[serde(untagged)]` ancestors
    /// (`AnthropicSystem`, `AnthropicContent`), which internally buffer to
    /// `serde_json::Value` first — incompatible with `Box<RawValue>`'s
    /// requirement for a raw-bytes deserializer. In practice, `cache_control` is
    /// always `{"type":"ephemeral"}`, which round-trips cleanly through `Value`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<serde_json::Value>,

    /// Unknown fields retained verbatim.
    #[serde(flatten)]
    pub extra_fields: serde_json::Map<String, serde_json::Value>,
}

/// A single message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    /// The message role: `"user"` or `"assistant"`.
    pub role: String,

    /// The message content — either a plain string or an array of content blocks.
    pub content: AnthropicContent,

    /// Unknown fields retained verbatim.
    #[serde(flatten)]
    pub extra_fields: serde_json::Map<String, serde_json::Value>,
}

/// Message content — either a plain string or an array of typed blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnthropicContent {
    /// Plain string content (shorthand for a single text block).
    Text(String),
    /// Array of typed content blocks.
    Blocks(Vec<AnthropicBlock>),
}

/// A single content block in an Anthropic message.
///
/// This enum is `#[non_exhaustive]` — future block types from Anthropic will
/// deserialize into the `Unknown` variant rather than failing to parse.
///
/// # Deserialization
///
/// Uses a custom `Deserialize` implementation to work around serde's limitation
/// that `#[serde(tag = "type")]` with `#[serde(flatten)]` fields in variants is
/// not supported. The custom impl peeks the `type` field from a raw JSON object,
/// dispatches to the appropriate struct, and captures unknown block types as a
/// raw `serde_json::Map`.
///
/// Serialization uses a custom `Serialize` implementation that re-inserts the `type`
/// field so round-trip byte identity is maintained with `preserve_order`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AnthropicBlock {
    /// A plain text block.
    Text(TextBlock),
    /// A tool invocation request from the assistant.
    ToolUse(ToolUseBlock),
    /// The user's response to a tool invocation.
    ToolResult(ToolResultBlock),
    /// An extended thinking block (opaque — never classified or mutated).
    Thinking(ThinkingBlock),
    /// Any block type not recognized by this version of the crate.
    /// The raw map retains all fields byte-faithfully.
    Unknown(serde_json::Map<String, serde_json::Value>),
}

impl<'de> serde::Deserialize<'de> for AnthropicBlock {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let raw: serde_json::Value = serde_json::Value::deserialize(deserializer)?;
        let obj = raw
            .as_object()
            .ok_or_else(|| D::Error::custom("block must be a JSON object"))?;
        let block_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

        // Remove the `type` key before delegating to struct deserializers.
        // The typed structs don't declare a `type` field — it would fall into
        // `extra_fields` via `#[serde(flatten)]` causing double-emission on
        // re-serialization (both the explicit "type" entry and the extra_fields copy).
        fn strip_type(val: serde_json::Value) -> serde_json::Value {
            match val {
                serde_json::Value::Object(mut m) => {
                    m.remove("type");
                    serde_json::Value::Object(m)
                }
                other => other,
            }
        }

        match block_type {
            "text" => {
                let block: TextBlock =
                    serde_json::from_value(strip_type(raw)).map_err(D::Error::custom)?;
                Ok(AnthropicBlock::Text(block))
            }
            "tool_use" => {
                let block: ToolUseBlock =
                    serde_json::from_value(strip_type(raw)).map_err(D::Error::custom)?;
                Ok(AnthropicBlock::ToolUse(block))
            }
            "tool_result" => {
                let block: ToolResultBlock =
                    serde_json::from_value(strip_type(raw)).map_err(D::Error::custom)?;
                Ok(AnthropicBlock::ToolResult(block))
            }
            "thinking" => {
                let block: ThinkingBlock =
                    serde_json::from_value(strip_type(raw)).map_err(D::Error::custom)?;
                Ok(AnthropicBlock::Thinking(block))
            }
            _ => {
                // Unknown block — retain all fields including `type` verbatim.
                let map = match raw {
                    serde_json::Value::Object(m) => m,
                    other => {
                        let mut m = serde_json::Map::new();
                        m.insert("type".to_string(), other);
                        m
                    }
                };
                Ok(AnthropicBlock::Unknown(map))
            }
        }
    }
}

impl serde::Serialize for AnthropicBlock {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            AnthropicBlock::Text(b) => {
                // Serialize with type field injected
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("type", "text")?;
                map.serialize_entry("text", &b.text)?;
                if let Some(cc) = &b.cache_control {
                    map.serialize_entry("cache_control", cc)?;
                }
                for (k, v) in &b.extra_fields {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
            AnthropicBlock::ToolUse(b) => {
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("type", "tool_use")?;
                map.serialize_entry("id", &b.id)?;
                map.serialize_entry("name", &b.name)?;
                map.serialize_entry("input", &b.input)?;
                if let Some(cc) = &b.cache_control {
                    map.serialize_entry("cache_control", cc)?;
                }
                for (k, v) in &b.extra_fields {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
            AnthropicBlock::ToolResult(b) => {
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("type", "tool_result")?;
                map.serialize_entry("tool_use_id", &b.tool_use_id)?;
                if let Some(ie) = b.is_error {
                    map.serialize_entry("is_error", &ie)?;
                }
                if let Some(content) = &b.content {
                    map.serialize_entry("content", content)?;
                }
                if let Some(cc) = &b.cache_control {
                    map.serialize_entry("cache_control", cc)?;
                }
                for (k, v) in &b.extra_fields {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
            AnthropicBlock::Thinking(b) => {
                let mut map = serializer.serialize_map(None)?;
                map.serialize_entry("type", "thinking")?;
                map.serialize_entry("thinking", &b.thinking)?;
                if let Some(sig) = &b.signature {
                    map.serialize_entry("signature", sig)?;
                }
                for (k, v) in &b.extra_fields {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
            AnthropicBlock::Unknown(fields) => {
                // Re-emit the raw map verbatim
                fields.serialize(serializer)
            }
        }
    }
}

/// A text content block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextBlock {
    /// The text content.
    pub text: String,

    /// Optional cache control.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<RawBlob>,

    /// Unknown fields retained verbatim.
    #[serde(flatten)]
    pub extra_fields: serde_json::Map<String, serde_json::Value>,
}

/// A tool use block (assistant requesting a tool call).
///
/// The `input` field is opaque — it is the model-generated JSON arguments and must
/// never be re-parsed, reformatted, or classified (exempt from classification per AC13).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUseBlock {
    /// The tool call identifier.
    pub id: String,

    /// The name of the tool being called.
    pub name: String,

    /// The tool's input arguments — opaque, retained as raw bytes.
    ///
    /// This field is sacrosanct: it is model-generated JSON that must be preserved
    /// byte-for-byte. It is exempt from classification (returns `unknown` if a class
    /// is requested) and exempt from mutation.
    pub input: RawBlob,

    /// Optional cache control.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<RawBlob>,

    /// Unknown fields retained verbatim.
    #[serde(flatten)]
    pub extra_fields: serde_json::Map<String, serde_json::Value>,
}

/// A tool result block (user returning the result of a tool call).
///
/// The content can be either a string or an array of content blocks (for rich
/// tool results including images).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultBlock {
    /// The tool call identifier this result corresponds to.
    pub tool_use_id: String,

    /// Optional error flag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,

    /// The result content — either a plain string or an array of blocks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<ToolResultContent>,

    /// Optional cache control.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<RawBlob>,

    /// Unknown fields retained verbatim.
    #[serde(flatten)]
    pub extra_fields: serde_json::Map<String, serde_json::Value>,
}

/// Content of a tool result — string or block array.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    /// Plain string result.
    Text(String),
    /// Array of content blocks (for rich results).
    Blocks(Vec<ToolResultLeaf>),
}

/// A leaf block inside a tool result block array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultLeaf {
    /// The block type (typically `"text"` or `"image"`).
    #[serde(rename = "type")]
    pub block_type: String,

    /// Text content (present for `"text"` blocks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    /// Unknown fields retained verbatim (covers image source, media_type, etc.).
    #[serde(flatten)]
    pub extra_fields: serde_json::Map<String, serde_json::Value>,
}

/// A thinking block — opaque extended reasoning content from the model.
///
/// Thinking blocks are never classified or mutated. The `thinking` field content
/// is opaque to this crate. Per Resolved Decision 7 (OQ9), thinking blocks are
/// treated as a single opaque unit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingBlock {
    /// The thinking content — opaque, retained as raw bytes.
    pub thinking: String,

    /// Optional signature field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,

    /// Unknown fields retained verbatim.
    #[serde(flatten)]
    pub extra_fields: serde_json::Map<String, serde_json::Value>,
}

/// A tool definition in the Anthropic request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicTool {
    /// The tool name.
    pub name: String,

    /// Optional description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// The JSON schema for the tool's input — retained as raw bytes.
    pub input_schema: RawBlob,

    /// Optional cache control.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<RawBlob>,

    /// Unknown fields retained verbatim.
    #[serde(flatten)]
    pub extra_fields: serde_json::Map<String, serde_json::Value>,
}

// Implement a helper for enumerating addressable leaf blocks from an Anthropic body.
// This is used by the mutation API to find blocks by composite ID.
impl AnthropicBody {
    /// Iterate over all addressable leaf text positions in the body.
    ///
    /// Returns `(message_index, block_index, leaf_index_or_none)` tuples for every
    /// mutable text position. Used internally by the mutation API.
    pub(crate) fn text_leaves(&self) -> impl Iterator<Item = LeafRef> {
        let mut leaves = Vec::new();
        for (mi, msg) in self.messages.iter().enumerate() {
            match &msg.content {
                AnthropicContent::Text(_) => {
                    leaves.push(LeafRef::MessageString { msg_idx: mi });
                }
                AnthropicContent::Blocks(blocks) => {
                    for (bi, block) in blocks.iter().enumerate() {
                        match block {
                            AnthropicBlock::Text(_) => {
                                leaves.push(LeafRef::TextBlock {
                                    msg_idx: mi,
                                    blk_idx: bi,
                                });
                            }
                            AnthropicBlock::ToolResult(tr) => match &tr.content {
                                Some(ToolResultContent::Text(_)) => {
                                    leaves.push(LeafRef::ToolResultString {
                                        msg_idx: mi,
                                        blk_idx: bi,
                                    });
                                }
                                Some(ToolResultContent::Blocks(leaves_arr)) => {
                                    for (li, leaf) in leaves_arr.iter().enumerate() {
                                        if leaf.block_type == "text" && leaf.text.is_some() {
                                            leaves.push(LeafRef::ToolResultLeaf {
                                                msg_idx: mi,
                                                blk_idx: bi,
                                                leaf_idx: li,
                                            });
                                        }
                                    }
                                }
                                None => {}
                            },
                            _ => {}
                        }
                    }
                }
            }
        }
        leaves.into_iter()
    }
}

/// A reference to an addressable leaf position within an Anthropic body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LeafRef {
    /// Plain string content of a message (message index).
    MessageString { msg_idx: usize },
    /// A `TextBlock` at (message_index, block_index).
    TextBlock { msg_idx: usize, blk_idx: usize },
    /// A tool_result string content at (message_index, block_index).
    ToolResultString { msg_idx: usize, blk_idx: usize },
    /// A leaf inside a tool_result block array at (message, block, leaf).
    ToolResultLeaf {
        msg_idx: usize,
        blk_idx: usize,
        leaf_idx: usize,
    },
}

impl LeafRef {
    /// Encode this reference as a composite block ID string.
    ///
    /// Format: `"m{msg_idx}b{blk_idx}"` or `"m{msg_idx}b{blk_idx}l{leaf_idx}"`.
    pub fn id(&self) -> String {
        match self {
            LeafRef::MessageString { msg_idx } => format!("m{msg_idx}"),
            LeafRef::TextBlock { msg_idx, blk_idx } => format!("m{msg_idx}b{blk_idx}"),
            LeafRef::ToolResultString { msg_idx, blk_idx } => {
                format!("m{msg_idx}b{blk_idx}s")
            }
            LeafRef::ToolResultLeaf {
                msg_idx,
                blk_idx,
                leaf_idx,
            } => {
                format!("m{msg_idx}b{blk_idx}l{leaf_idx}")
            }
        }
    }
}

/// Enumerate `(block_id, text)` pairs for all mutable text leaves in a body.
///
/// Single-pass walk used by the classifier. Only mutable text leaves are included;
/// exempt blocks (tool_use, thinking, etc.) are skipped.
pub(crate) fn anthropic_leaf_texts(body: &AnthropicBody) -> Vec<(String, &str)> {
    let mut out = Vec::new();
    for (mi, msg) in body.messages.iter().enumerate() {
        match &msg.content {
            AnthropicContent::Text(s) => {
                out.push((format!("m{mi}"), s.as_str()));
            }
            AnthropicContent::Blocks(blocks) => {
                for (bi, block) in blocks.iter().enumerate() {
                    match block {
                        AnthropicBlock::Text(tb) => {
                            out.push((format!("m{mi}b{bi}"), tb.text.as_str()));
                        }
                        AnthropicBlock::ToolResult(tr) => match &tr.content {
                            Some(ToolResultContent::Text(s)) => {
                                out.push((format!("m{mi}b{bi}s"), s.as_str()));
                            }
                            Some(ToolResultContent::Blocks(leaves)) => {
                                for (li, leaf) in leaves.iter().enumerate() {
                                    if leaf.block_type == "text"
                                        && let Some(s) = leaf.text.as_deref()
                                    {
                                        out.push((format!("m{mi}b{bi}l{li}"), s));
                                    }
                                }
                            }
                            None => {}
                        },
                        _ => {}
                    }
                }
            }
        }
    }
    out
}
