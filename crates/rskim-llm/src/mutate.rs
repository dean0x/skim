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
/// OpenAI mutation is tracked as a follow-up (#332).
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
        // OpenAI mutation is not yet implemented — follow-up tracked as #332.
        // Return BlockNotMutable (not BlockNotFound) so callers can distinguish
        // "this provider is unsupported" from "this ID does not exist".
        ParsedBody::OpenAi(_) => Err(LlmError::BlockNotMutable(
            block_id.to_string(),
            "openai-not-implemented".to_string(),
        )),
    }
}

fn mutate_anthropic(body: &mut AnthropicBody, block_id: &str, new_text: &str) -> Result<Vec<u8>> {
    // Scan mutable leaves for the one whose composite id matches `block_id`.
    // `walk_leaves` yields (LeafRef, text) pairs with no early exit, so the
    // `found_leaf.is_none()` guard stops further `id()` comparisons once a match
    // is found (each `LeafRef::id()` call allocates, so we avoid re-deriving the
    // id for the remaining leaves after the match).
    let mut found_leaf = None;
    body.walk_leaves(|leaf_ref, _text| {
        if found_leaf.is_none() && leaf_ref.id() == block_id {
            found_leaf = Some(leaf_ref);
        }
    });

    let leaf = match found_leaf {
        Some(l) => l,
        None => {
            // Block not in mutable set — check whether it exists at all (as an
            // exempt block) to distinguish BlockNotMutable from BlockNotFound.
            // We do a single pass over list_anthropic_blocks rather than a second
            // walk_leaves pass, because exempt blocks are not visited by walk_leaves.
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
    // Subsequent `walk_leaves` calls will see the new value.
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
/// tracked as a follow-up (#332). If you call [`mutate_block`] on an OpenAI
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
        // OpenAI mutation is not yet implemented — follow-up tracked as #332.
        ParsedBody::OpenAi(_) => vec![],
    }
}

/// Build a descriptor list for all leaves in the body (mutable and exempt).
///
/// Derived from [`AnthropicBody::walk_leaves`] for mutable entries plus a single
/// supplementary pass for exempt blocks (ToolUse, Thinking, Unknown, non-text
/// ToolResultLeaf).  This is the single source of truth for the descriptor view.
fn list_anthropic_blocks(body: &AnthropicBody) -> Vec<BlockDescriptor> {
    // Phase 1: collect all mutable leaves via the canonical walk (no duplicate
    // traversal logic — the kind mapping lives here rather than in the walk).
    let mut out: Vec<BlockDescriptor> = Vec::new();
    body.walk_leaves(|leaf_ref, _text| {
        let kind = match &leaf_ref {
            LeafRef::MessageString { .. } => "message-string",
            LeafRef::TextBlock { .. } => "text",
            LeafRef::ToolResultString { .. } => "tool_result-string",
            LeafRef::ToolResultLeaf { .. } => "tool_result-leaf-text",
        };
        out.push(BlockDescriptor {
            id: leaf_ref.id(),
            mutable: true,
            kind: kind.to_string(),
        });
    });

    // Phase 2: supplement with exempt (non-mutable) block positions.
    // These are not visited by walk_leaves, so we enumerate them here.
    for (mi, msg) in body.messages.iter().enumerate() {
        if let AnthropicContent::Blocks(blocks) = &msg.content {
            for (bi, block) in blocks.iter().enumerate() {
                match block {
                    AnthropicBlock::ToolUse(_) => {
                        out.push(BlockDescriptor {
                            id: format!("m{mi}b{bi}"),
                            mutable: false,
                            kind: "tool_use".to_string(),
                        });
                    }
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
                    AnthropicBlock::ToolResult(tr) => {
                        // Non-text (e.g. image) leaves inside a tool_result block array
                        // are exempt.  Text leaves were already added in Phase 1;
                        // the None / Text(string) content cases were also covered.
                        if let Some(ToolResultContent::Blocks(leaves)) = &tr.content {
                            for (li, leaf) in leaves.iter().enumerate() {
                                if !(leaf.block_type == "text" && leaf.text.is_some()) {
                                    out.push(BlockDescriptor {
                                        id: format!("m{mi}b{bi}l{li}"),
                                        mutable: false,
                                        kind: format!("tool_result-leaf-{}", leaf.block_type),
                                    });
                                }
                            }
                        }
                    }
                    // Text and MessageString are mutable — handled in Phase 1.
                    _ => {}
                }
            }
        }
    }

    out
}
