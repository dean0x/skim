//! Mutation API: replace one text payload in a parsed body.
//!
//! # Invariants
//!
//! - **Invariant 8 (text-for-text):** Mutation replaces exactly one text payload.
//!   No structural fields, envelope fields, or non-payload bytes may be added or removed.
//! - **Position preserved:** The mutated block remains at the same index in the
//!   messages/blocks arrays.
//! - **Surrounding regions byte-identical:** All bytes outside the replaced payload span
//!   are byte-identical to the input.  This is enforced by byte-range surgery
//!   (`crate::splice`): the raw JSON bytes are scanned to find the exact span of the
//!   target string value, and only those bytes are replaced.  The `raw_bytes` cache is
//!   updated with the spliced result so that subsequent `serialize()` calls return it
//!   verbatim (satisfying AC9(b) and AC10).
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
use crate::splice::{find_leaf_span, splice_replace};
use crate::{LlmError, ParsedBody, Result};

/// Replace the text payload of a single block identified by `block_id`.
///
/// Only mutable blocks may be targeted. Exempt blocks (tool_use input, thinking,
/// unrecognized types) return [`LlmError::BlockNotMutable`]. Use
/// [`crate::list_blocks`] to enumerate blocks and check their `mutable` field
/// before calling this function.
///
/// # OpenAI limitation
///
/// OpenAI bodies are not yet mutable through this API. [`crate::list_blocks`]
/// returns an empty list for OpenAI bodies, and calling `mutate_block` on any
/// OpenAI body returns [`LlmError::BlockNotMutable`] with kind `"openai-not-implemented"`.
/// OpenAI mutation is tracked as a follow-up (#329).
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
/// - [`LlmError::BlockNotFound`] — no block with the given ID exists in the body
/// - [`LlmError::BlockNotMutable`] — block exists but is exempt from mutation
///   (tool_use, thinking, unknown block type, or OpenAI body)
/// - [`LlmError::NoTextPayload`] — block has no text payload
pub fn mutate_block(body: &mut ParsedBody, block_id: &str, new_text: &str) -> Result<Vec<u8>> {
    match body {
        ParsedBody::Anthropic(b) => mutate_anthropic(b, block_id, new_text),
        // OpenAI mutation is not yet implemented — follow-up tracked as #329.
        // Return BlockNotMutable (not BlockNotFound) so callers can distinguish
        // "this provider is unsupported" from "this ID does not exist".
        ParsedBody::OpenAi(_) => Err(LlmError::BlockNotMutable(
            block_id.to_string(),
            "openai-not-implemented".to_string(),
        )),
    }
}

fn mutate_anthropic(body: &mut AnthropicBody, block_id: &str, new_text: &str) -> Result<Vec<u8>> {
    // First try to find the block among mutable leaves (fast path).
    // If not found there, consult list_anthropic_blocks to distinguish
    // BlockNotMutable (block exists but is exempt) from BlockNotFound (absent).
    let leaf = match body.text_leaves().find(|l| l.id() == block_id) {
        Some(l) => l,
        None => {
            // Block not mutable — check whether it exists at all.
            let descriptor = list_anthropic_blocks(body)
                .into_iter()
                .find(|d| d.id == block_id);
            return match descriptor {
                Some(d) => Err(LlmError::BlockNotMutable(block_id.to_string(), d.kind)),
                None => Err(LlmError::BlockNotFound(block_id.to_string())),
            };
        }
    };

    // Byte-range surgery (AC9b / AC10):
    //
    // We locate the exact byte span of the string value named by `leaf` inside
    // `raw_bytes`, then splice-replace only those bytes.  Every byte outside the
    // replaced span is byte-identical to the original input — no reformatting of
    // numbers, whitespace, escapes, or envelope fields.
    //
    // The typed model fields are also updated so that subsequent `list_blocks` /
    // `text_leaves` calls see the new value, and repeated mutations chain correctly.
    let span = find_leaf_span(&body.raw_bytes, &leaf)?;
    let new_raw = splice_replace(&body.raw_bytes, span, new_text)?;

    // Update the typed field so that the in-memory model stays consistent with
    // the new raw bytes.  This is needed for repeat-mutation idempotency (AC9d).
    apply_leaf_mutation(body, &leaf, new_text)?;

    // Replace raw_bytes with the spliced result.  Subsequent serialize() calls
    // will return this verbatim (the "unmutated path" in serialize.rs), which is
    // now the post-mutation bytes.  We store it first and then return a clone
    // to avoid a second full-body allocation compared to .clone()/.clone().
    body.raw_bytes = new_raw;
    Ok(body.raw_bytes.clone())
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
/// # OpenAI limitation
///
/// OpenAI bodies currently expose **no mutable blocks**: this function returns
/// an empty `Vec` for any [`ParsedBody::OpenAi`] body. OpenAI mutation is
/// tracked as a follow-up (#329). If you call [`mutate_block`] on an OpenAI
/// body, it returns [`crate::LlmError::BlockNotMutable`].
///
/// Note: [`crate::classify_body`] does walk OpenAI content and returns
/// classification results with ids of the form `m{i}` / `m{i}p{j}`, but these
/// ids are **not** addressable through `list_blocks` or `mutate_block` on OpenAI
/// bodies.
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
        // OpenAI mutation is not yet implemented — follow-up tracked as #329.
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
