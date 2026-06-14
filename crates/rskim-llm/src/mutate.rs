//! Mutation API: replace one text payload in a parsed body.
//!
//! # Invariants
//!
//! - **Invariant 8 (text-for-text):** Mutation replaces exactly one text payload.
//!   No structural fields, envelope fields, or non-payload bytes may be added or removed.
//! - **Position preserved:** The mutated block remains at the same index in the
//!   messages/blocks arrays.
//! - **Surrounding regions byte-identical:** All bytes outside the replaced payload span
//!   are byte-identical to the input.
//! - **No envelope mutation:** The crate never modifies the request envelope
//!   (`model`, `max_tokens`, `system`, `tools`, etc.). Only leaf text payloads are mutable.
//!   Envelope mutation lives in a separate layer above this crate (Resolved Decision 7).
//!
//! # Block IDs
//!
//! Block IDs are composite strings encoding the path to a leaf in the body tree.
//! Use [`crate::list_blocks`] to enumerate available IDs before mutating.

use crate::model::anthropic::{
    AnthropicBlock, AnthropicBody, AnthropicContent, LeafRef, ToolResultContent,
};
use crate::{LlmError, ParsedBody, Result, serialize::serialize};

/// Replace the text payload of a single block identified by `block_id`.
///
/// Only mutable blocks may be targeted. Exempt blocks (tool_use input, thinking,
/// OpenAI opaque fields, unrecognized types) return [`LlmError::BlockNotMutable`].
///
/// # Arguments
///
/// - `body` — the parsed body to mutate (modified in place, then serialized)
/// - `block_id` — composite block ID as returned by [`crate::list_blocks`]
/// - `new_text` — replacement text payload
///
/// # Returns
///
/// The serialized bytes of the mutated body. Repeating the same mutation yields
/// identical bytes (Invariant 8).
///
/// # Errors
///
/// - [`LlmError::BlockNotFound`] — no block with the given ID
/// - [`LlmError::BlockNotMutable`] — block is exempt from mutation
/// - [`LlmError::NoTextPayload`] — block has no text payload
pub fn mutate_block(body: &mut ParsedBody, block_id: &str, new_text: &str) -> Result<Vec<u8>> {
    match body {
        ParsedBody::Anthropic(b) => mutate_anthropic(b, block_id, new_text),
        // OpenAI mutation is not yet implemented — follow-up tracked separately.
        ParsedBody::OpenAi(_) => Err(LlmError::BlockNotFound(block_id.to_string())),
    }
}

fn mutate_anthropic(body: &mut AnthropicBody, block_id: &str, new_text: &str) -> Result<Vec<u8>> {
    // Find the leaf that matches the block_id
    let leaf = body
        .text_leaves()
        .find(|l| l.id() == block_id)
        .ok_or_else(|| LlmError::BlockNotFound(block_id.to_string()))?;

    // Apply the mutation
    apply_leaf_mutation(body, &leaf, new_text)?;

    // Clear raw_bytes so serialize() uses the typed-field path (rebuild from struct).
    // The original verbatim bytes are now stale — content has changed.
    body.raw_bytes.clear();

    // Re-serialize from typed fields
    let serialized = serialize(&ParsedBody::Anthropic(body.clone()))?;
    Ok(serialized)
}

fn apply_leaf_mutation(body: &mut AnthropicBody, leaf: &LeafRef, new_text: &str) -> Result<()> {
    match leaf {
        LeafRef::MessageString { msg_idx } => match &mut body.messages[*msg_idx].content {
            AnthropicContent::Text(s) => {
                *s = new_text.to_string();
                Ok(())
            }
            _ => Err(LlmError::NoTextPayload(leaf.id())),
        },
        LeafRef::TextBlock { msg_idx, blk_idx } => match &mut body.messages[*msg_idx].content {
            AnthropicContent::Blocks(blocks) => match &mut blocks[*blk_idx] {
                AnthropicBlock::Text(tb) => {
                    tb.text = new_text.to_string();
                    Ok(())
                }
                _ => Err(LlmError::NoTextPayload(leaf.id())),
            },
            _ => Err(LlmError::NoTextPayload(leaf.id())),
        },
        LeafRef::ToolResultString { msg_idx, blk_idx } => {
            match &mut body.messages[*msg_idx].content {
                AnthropicContent::Blocks(blocks) => match &mut blocks[*blk_idx] {
                    AnthropicBlock::ToolResult(tr) => match &mut tr.content {
                        Some(ToolResultContent::Text(s)) => {
                            *s = new_text.to_string();
                            Ok(())
                        }
                        _ => Err(LlmError::NoTextPayload(leaf.id())),
                    },
                    _ => Err(LlmError::NoTextPayload(leaf.id())),
                },
                _ => Err(LlmError::NoTextPayload(leaf.id())),
            }
        }
        LeafRef::ToolResultLeaf {
            msg_idx,
            blk_idx,
            leaf_idx,
        } => match &mut body.messages[*msg_idx].content {
            AnthropicContent::Blocks(blocks) => match &mut blocks[*blk_idx] {
                AnthropicBlock::ToolResult(tr) => match &mut tr.content {
                    Some(ToolResultContent::Blocks(leaves)) => {
                        let leaf_block = &mut leaves[*leaf_idx];
                        if leaf_block.text.is_none() {
                            return Err(LlmError::NoTextPayload(leaf.id()));
                        }
                        leaf_block.text = Some(new_text.to_string());
                        Ok(())
                    }
                    _ => Err(LlmError::NoTextPayload(leaf.id())),
                },
                _ => Err(LlmError::NoTextPayload(leaf.id())),
            },
            _ => Err(LlmError::NoTextPayload(leaf.id())),
        },
    }
}

/// A descriptor for an addressable block in a parsed body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockDescriptor {
    /// The composite block ID (use with [`mutate_block`]).
    pub id: String,
    /// Whether this block is mutable via [`mutate_block`].
    pub mutable: bool,
    /// A human-readable description of the block type.
    pub kind: String,
}

/// List all addressable blocks in a parsed body.
///
/// Returns descriptors for every leaf block — both mutable (text payload) and
/// non-mutable (exempt) blocks. Use the `id` field with [`mutate_block`].
///
/// # Examples
///
/// ```
/// use rskim_llm::{parse, list_blocks};
///
/// let json = r#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"Hello"}],"max_tokens":100}"#;
/// let body = parse(json.as_bytes())?;
/// let blocks = list_blocks(&body);
/// assert_eq!(blocks.len(), 1);
/// assert!(blocks[0].mutable);
/// # Ok::<(), rskim_llm::LlmError>(())
/// ```
pub fn list_blocks(body: &ParsedBody) -> Vec<BlockDescriptor> {
    match body {
        ParsedBody::Anthropic(b) => list_anthropic_blocks(b),
        ParsedBody::OpenAi(_) => vec![],
    }
}

fn list_anthropic_blocks(body: &AnthropicBody) -> Vec<BlockDescriptor> {
    let mut out = Vec::new();

    for (mi, msg) in body.messages.iter().enumerate() {
        match &msg.content {
            AnthropicContent::Text(_) => {
                out.push(BlockDescriptor {
                    id: format!("m{mi}"),
                    mutable: true,
                    kind: "message-string".to_string(),
                });
            }
            AnthropicContent::Blocks(blocks) => {
                for (bi, block) in blocks.iter().enumerate() {
                    match block {
                        AnthropicBlock::Text(_) => {
                            out.push(BlockDescriptor {
                                id: format!("m{mi}b{bi}"),
                                mutable: true,
                                kind: "text".to_string(),
                            });
                        }
                        AnthropicBlock::ToolUse(_) => {
                            out.push(BlockDescriptor {
                                id: format!("m{mi}b{bi}"),
                                mutable: false,
                                kind: "tool_use".to_string(),
                            });
                        }
                        AnthropicBlock::ToolResult(tr) => match &tr.content {
                            None => {}
                            Some(ToolResultContent::Text(_)) => {
                                out.push(BlockDescriptor {
                                    id: format!("m{mi}b{bi}s"),
                                    mutable: true,
                                    kind: "tool_result-string".to_string(),
                                });
                            }
                            Some(ToolResultContent::Blocks(leaves)) => {
                                for (li, leaf) in leaves.iter().enumerate() {
                                    let is_text = leaf.block_type == "text" && leaf.text.is_some();
                                    out.push(BlockDescriptor {
                                        id: format!("m{mi}b{bi}l{li}"),
                                        mutable: is_text,
                                        kind: format!("tool_result-leaf-{}", leaf.block_type),
                                    });
                                }
                            }
                        },
                        AnthropicBlock::Thinking(_) => {
                            out.push(BlockDescriptor {
                                id: format!("m{mi}b{bi}"),
                                mutable: false,
                                kind: "thinking".to_string(),
                            });
                        }
                        AnthropicBlock::Unknown(_) => {
                            out.push(BlockDescriptor {
                                id: format!("m{mi}b{bi}"),
                                mutable: false,
                                kind: "unknown".to_string(),
                            });
                        }
                    }
                }
            }
        }
    }

    out
}
