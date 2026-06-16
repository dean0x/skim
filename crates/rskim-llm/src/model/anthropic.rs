//! Anthropic `/v1/messages` request body model.
//!
//! This module models the Anthropic Messages API request body. Unknown fields at any
//! level are retained as raw byte blobs to guarantee byte-identical round-trips.
//!
//! # Non-exhaustive types
//!
//! [`AnthropicBlock`], [`AnthropicSystem`], [`AnthropicContent`], and
//! [`ToolResultContent`] are all `#[non_exhaustive]` — new variants added by
//! Anthropic in future schema versions will not break downstream crates that match on
//! these enums (additive-only insurance, per Resolved Decision 7).

use serde::{Deserialize, Serialize};

use super::RawBlob;

/// A complete Anthropic `/v1/messages` request body.
///
/// # Byte-identical round-trips
///
/// Byte identity is achieved via a raw-bytes cache (`raw_bytes`): the original
/// JSON bytes are stored verbatim on parse and returned by [`crate::serialize()`]
/// without re-encoding. This preserves insignificant whitespace, non-canonical
/// number tokens (`1.0e3`), `\uXXXX` escapes, and arbitrary field ordering.
///
/// After mutation via [`crate::mutate_block`], the raw bytes are updated in-place
/// using byte-range surgery (the replaced span only), so every byte outside the
/// mutated payload span remains byte-identical to the original input.
///
/// `extra_fields` retains all top-level fields not modeled as typed members.
/// It is used only on the fall-back rebuild path; on the normal path, `raw_bytes`
/// is used instead.
///
/// # No-envelope-mutation invariant (AC11)
///
/// The structural fields `model`, `messages`, and `extra_fields` are intentionally
/// not `pub` to enforce that no caller can drop, reorder, duplicate, or add messages,
/// or mutate envelope fields (`model`, `max_tokens`, etc.) through this crate's
/// public API. Read-only access is provided via [`AnthropicBody::model`],
/// [`AnthropicBody::messages`], and [`AnthropicBody::extra_fields`].
/// All mutation routes through [`crate::mutate_block`] which enforces the
/// text-for-text invariant. Envelope mutation lives in a separate layer above this
/// crate (Resolved Decision 7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicBody {
    /// The model identifier (e.g., `"claude-3-5-sonnet-20241022"`).
    ///
    /// Not `pub` — use [`AnthropicBody::model`] for read-only access.
    /// Mutation of the model identifier is envelope mutation and is forbidden
    /// in this crate (Resolved Decision 7; AC11).
    pub(crate) model: String,

    /// The conversation turns.
    ///
    /// Not `pub` — use [`AnthropicBody::messages`] for read-only access.
    /// Structural manipulation (push/remove/reorder) is forbidden in this crate
    /// (AC11 no-turn-manipulation invariant).
    pub(crate) messages: Vec<AnthropicMessage>,

    /// Unknown top-level fields retained for the fall-back rebuild path.
    ///
    /// This includes `system`, `max_tokens`, `tools`, `temperature`, `top_p`,
    /// `stream`, `tool_choice`, `metadata`, and any other top-level fields.
    ///
    /// Not `pub` — use [`AnthropicBody::extra_fields`] for read-only access.
    /// Inserting or removing extra fields is envelope mutation and is forbidden
    /// (AC11; Resolved Decision 7).
    #[serde(flatten)]
    pub(crate) extra_fields: serde_json::Map<String, serde_json::Value>,

    /// Original JSON bytes for byte-identical unmutated serialize.
    ///
    /// Set by [`crate::parse()`] from the input bytes. Updated by
    /// `mutate_block` via byte-range surgery after any mutation.
    /// Not serialized — this is crate-internal state only.
    #[serde(skip)]
    pub(crate) raw_bytes: Vec<u8>,
}

impl AnthropicBody {
    /// The model identifier (e.g., `"claude-3-5-sonnet-20241022"`).
    ///
    /// Read-only — envelope mutation is not supported in this crate (AC11).
    pub fn model(&self) -> &str {
        &self.model
    }

    /// The conversation turns, in order.
    ///
    /// Returns an immutable slice — structural manipulation (push/remove/reorder)
    /// is not supported through this crate's public API (AC11 no-turn-manipulation
    /// invariant). Use [`crate::mutate_block`] to replace leaf text payloads.
    pub fn messages(&self) -> &[AnthropicMessage] {
        &self.messages
    }

    /// Unknown top-level fields retained for byte-identical round-trips.
    ///
    /// These are all top-level fields other than `model` and `messages`:
    /// `system`, `max_tokens`, `tools`, `temperature`, etc.
    ///
    /// Read-only — inserting or removing fields is envelope mutation and is
    /// not supported in this crate (Resolved Decision 7; AC11).
    pub fn extra_fields(&self) -> &serde_json::Map<String, serde_json::Value> {
        &self.extra_fields
    }
}

/// The `system` field — either a plain string or an array of content blocks.
///
/// Both forms are valid in the Anthropic API. The array form supports `cache_control`
/// on individual system entries.
///
/// # Note: not used in the typed model
///
/// `AnthropicBody` stores `system` (and all other non-`model`/`messages` top-level
/// fields) in `extra_fields` as a raw `serde_json::Value` to preserve byte identity.
/// This type is provided as public API for callers that want to parse the `system`
/// field from `extra_fields` manually; it is not used internally by [`crate::parse()`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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
///
/// # Note: not used in the typed model
///
/// `AnthropicBody` stores `tools` in `extra_fields` as a raw `serde_json::Value`
/// to preserve byte identity. This type is provided as public API for callers that
/// want to parse tool definitions from `extra_fields` manually; it is not used
/// internally by [`crate::parse()`].
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

// ---------------------------------------------------------------------------
// Canonical single-pass tree-walk
// ---------------------------------------------------------------------------
//
// The Anthropic message→content→block→leaf tree has one canonical traversal
// shared by classification, mutation, and descriptor enumeration.  All three
// consumers derive from `walk_leaves` below; a future schema change (new mutable
// block type, or a change to the ID scheme) requires only one edit here rather
// than synchronized edits across three sites.
//
// Walk semantics:
//   - Every position that carries a mutable text payload is visited exactly once.
//   - Exempt positions (ToolUse, Thinking, Unknown, non-text ToolResultLeaf) are
//     NOT visited — callers that need them (e.g. list_anthropic_blocks) extend
//     the walk independently at their level.
//   - The visitor `f` receives `(LeafRef, &str)` — the structural path to the
//     leaf and its current text value.  (A third `is_text_block_type` argument
//     was removed in a prior simplify pass; all callers use the 2-argument form.)

impl AnthropicBody {
    /// Single-pass walk over all mutable text leaves.
    ///
    /// The closure `f` is called once per mutable leaf with `(leaf_ref, text_value)`.
    /// Exempt positions (tool_use, thinking, unknown, non-text tool_result leaves)
    /// are skipped — only positions reachable via [`LeafRef`] are visited.
    ///
    /// This is the single source of truth for the message→content→block→leaf
    /// traversal.  [`anthropic_leaf_texts`] and `list_anthropic_blocks` derive
    /// from this walk.  `mutate_anthropic` calls it directly for the mutable-leaf
    /// search.
    pub(crate) fn walk_leaves<'a>(&'a self, mut f: impl FnMut(LeafRef, &'a str)) {
        for (mi, msg) in self.messages.iter().enumerate() {
            match &msg.content {
                AnthropicContent::Text(s) => {
                    f(LeafRef::MessageString { msg_idx: mi }, s.as_str());
                }
                AnthropicContent::Blocks(blocks) => {
                    for (bi, block) in blocks.iter().enumerate() {
                        match block {
                            AnthropicBlock::Text(tb) => {
                                f(
                                    LeafRef::TextBlock {
                                        msg_idx: mi,
                                        blk_idx: bi,
                                    },
                                    tb.text.as_str(),
                                );
                            }
                            AnthropicBlock::ToolResult(tr) => match &tr.content {
                                Some(ToolResultContent::Text(s)) => {
                                    f(
                                        LeafRef::ToolResultString {
                                            msg_idx: mi,
                                            blk_idx: bi,
                                        },
                                        s.as_str(),
                                    );
                                }
                                Some(ToolResultContent::Blocks(leaves_arr)) => {
                                    for (li, leaf) in leaves_arr.iter().enumerate() {
                                        if leaf.block_type == "text"
                                            && let Some(s) = leaf.text.as_deref()
                                        {
                                            f(
                                                LeafRef::ToolResultLeaf {
                                                    msg_idx: mi,
                                                    blk_idx: bi,
                                                    leaf_idx: li,
                                                },
                                                s,
                                            );
                                        }
                                    }
                                }
                                None => {}
                            },
                            // Exempt: ToolUse, Thinking, Unknown — not visited
                            _ => {}
                        }
                    }
                }
            }
        }
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
    /// Four formats are used, one per leaf kind:
    ///
    /// | Variant | Format |
    /// |---------|--------|
    /// | `MessageString` | `"m{msg_idx}"` |
    /// | `TextBlock` | `"m{msg_idx}b{blk_idx}"` |
    /// | `ToolResultString` | `"m{msg_idx}b{blk_idx}s"` |
    /// | `ToolResultLeaf` | `"m{msg_idx}b{blk_idx}l{leaf_idx}"` |
    ///
    /// These are the only ID formats produced by [`anthropic_leaf_texts`] and
    /// `list_anthropic_blocks` (both derived from [`AnthropicBody::walk_leaves`]).
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

    /// Compare this leaf's composite ID to `candidate` without allocating.
    ///
    /// Equivalent to `self.id() == candidate` but avoids the `String` allocation
    /// that `id()` produces.  Use this in the mutate-block search hot path
    /// (called once per leaf until a match is found).  Use `id()` only for
    /// error messages or descriptor construction where allocation is acceptable.
    ///
    /// # ID formats (byte-by-byte match)
    ///
    /// | Variant | Pattern |
    /// |---------|---------|
    /// | `MessageString` | `"m{i}"` |
    /// | `TextBlock` | `"m{i}b{j}"` |
    /// | `ToolResultString` | `"m{i}b{j}s"` |
    /// | `ToolResultLeaf` | `"m{i}b{j}l{k}"` |
    pub(crate) fn id_eq(&self, candidate: &str) -> bool {
        // Compare byte-by-byte without allocating a String.
        // We use a tiny inline helper that writes usize digits into a stack buffer
        // and matches them against the candidate byte-by-byte, avoiding any heap
        // allocation (unlike `id()` which calls `format!`).
        //
        // The helper is a simple cursor over a fixed-size stack buffer; 64 bytes
        // is far larger than any realistic id (max realistic: "m999b999l999" = 12
        // bytes for msg_idx/blk_idx/leaf_idx all in the hundreds).
        struct Cursor {
            buf: [u8; 64],
            pos: usize,
        }
        impl Cursor {
            fn new() -> Self {
                Self { buf: [0u8; 64], pos: 0 }
            }
            fn push(&mut self, b: u8) {
                if self.pos < self.buf.len() {
                    self.buf[self.pos] = b;
                    self.pos += 1;
                }
            }
            fn push_usize(&mut self, mut n: usize) {
                // Write decimal digits for `n` into the buffer.
                // We write into a small sub-buffer in reverse then copy forward.
                let mut tmp = [0u8; 20]; // max usize decimal = 20 digits
                let mut len = 0;
                if n == 0 {
                    self.push(b'0');
                    return;
                }
                while n > 0 {
                    tmp[len] = b'0' + (n % 10) as u8;
                    n /= 10;
                    len += 1;
                }
                for i in (0..len).rev() {
                    self.push(tmp[i]);
                }
            }
            fn as_bytes(&self) -> &[u8] {
                &self.buf[..self.pos]
            }
        }

        let mut c = Cursor::new();
        match self {
            LeafRef::MessageString { msg_idx } => {
                c.push(b'm');
                c.push_usize(*msg_idx);
            }
            LeafRef::TextBlock { msg_idx, blk_idx } => {
                c.push(b'm');
                c.push_usize(*msg_idx);
                c.push(b'b');
                c.push_usize(*blk_idx);
            }
            LeafRef::ToolResultString { msg_idx, blk_idx } => {
                c.push(b'm');
                c.push_usize(*msg_idx);
                c.push(b'b');
                c.push_usize(*blk_idx);
                c.push(b's');
            }
            LeafRef::ToolResultLeaf {
                msg_idx,
                blk_idx,
                leaf_idx,
            } => {
                c.push(b'm');
                c.push_usize(*msg_idx);
                c.push(b'b');
                c.push_usize(*blk_idx);
                c.push(b'l');
                c.push_usize(*leaf_idx);
            }
        }
        c.as_bytes() == candidate.as_bytes()
    }
}

/// Enumerate `(block_id, text)` pairs for all mutable text leaves in a body.
///
/// Derived from [`AnthropicBody::walk_leaves`] — single source of truth for the
/// tree traversal.  Only mutable text leaves are included; exempt blocks
/// (tool_use, thinking, etc.) are skipped.
pub(crate) fn anthropic_leaf_texts(body: &AnthropicBody) -> Vec<(String, &str)> {
    let mut out = Vec::new();
    body.walk_leaves(|leaf_ref, text| {
        out.push((leaf_ref.id(), text));
    });
    out
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    /// Verify that `id_eq` agrees with `id() == candidate` for all four ID shapes.
    ///
    /// This tests real equivalence — each assertion can fail if the formats diverge.
    /// Avoids PF-007 (vacuous tests): the test would fail if id_eq returned `true`
    /// unconditionally, or if id() produced a different format than id_eq.
    #[test]
    fn id_eq_agrees_with_id_for_all_shapes() {
        // Shape 1: MessageString → "m{i}"
        let ms = LeafRef::MessageString { msg_idx: 3 };
        let ms_id = ms.id();
        assert!(ms.id_eq(&ms_id), "id_eq must match id() for MessageString");
        assert_eq!(ms_id, "m3");
        assert!(!ms.id_eq("m4"), "id_eq must not match a different index");
        assert!(!ms.id_eq("m3b0"), "id_eq must not match a longer id");
        assert!(!ms.id_eq("m3s"), "id_eq must not match a suffixed id");

        // Shape 2: TextBlock → "m{i}b{j}"
        let tb = LeafRef::TextBlock {
            msg_idx: 1,
            blk_idx: 5,
        };
        let tb_id = tb.id();
        assert!(tb.id_eq(&tb_id), "id_eq must match id() for TextBlock");
        assert_eq!(tb_id, "m1b5");
        assert!(!tb.id_eq("m1b6"), "id_eq must not match a different blk_idx");
        assert!(!tb.id_eq("m1b5s"), "id_eq must not match the ToolResultString suffix");

        // Shape 3: ToolResultString → "m{i}b{j}s"
        let trs = LeafRef::ToolResultString {
            msg_idx: 0,
            blk_idx: 2,
        };
        let trs_id = trs.id();
        assert!(trs.id_eq(&trs_id), "id_eq must match id() for ToolResultString");
        assert_eq!(trs_id, "m0b2s");
        assert!(!trs.id_eq("m0b2"), "id_eq must not match the TextBlock (no suffix) form");
        assert!(!trs.id_eq("m0b2l0"), "id_eq must not match the ToolResultLeaf form");

        // Shape 4: ToolResultLeaf → "m{i}b{j}l{k}"
        let trl = LeafRef::ToolResultLeaf {
            msg_idx: 2,
            blk_idx: 0,
            leaf_idx: 7,
        };
        let trl_id = trl.id();
        assert!(trl.id_eq(&trl_id), "id_eq must match id() for ToolResultLeaf");
        assert_eq!(trl_id, "m2b0l7");
        assert!(!trl.id_eq("m2b0l8"), "id_eq must not match a different leaf_idx");
        assert!(!trl.id_eq("m2b0"), "id_eq must not match the shorter TextBlock form");
    }

    /// Guard: id_eq must handle multi-digit indices correctly.
    /// A naive string comparison bug or off-by-one in the buffer write could
    /// produce incorrect results for indices ≥ 10.
    #[test]
    fn id_eq_multi_digit_indices() {
        let leaf = LeafRef::ToolResultLeaf {
            msg_idx: 10,
            blk_idx: 20,
            leaf_idx: 30,
        };
        let id = leaf.id();
        assert_eq!(id, "m10b20l30");
        assert!(leaf.id_eq("m10b20l30"));
        assert!(!leaf.id_eq("m10b20l3"), "must not match a prefix of a multi-digit leaf_idx");
        assert!(!leaf.id_eq("m1b20l30"), "must not match single-digit prefix of msg_idx");
    }
}
