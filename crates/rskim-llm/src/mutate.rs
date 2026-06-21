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
    // `found_leaf.is_none()` guard stops further comparisons once a match is found.
    // `id_eq` compares byte-by-byte without allocating (unlike `id()` which
    // allocates a String via format!); the allocating `id()` is reserved for
    // error/descriptor paths where one allocation is acceptable.
    let mut found_leaf = None;
    body.walk_leaves(|leaf_ref, _text| {
        if found_leaf.is_none() && leaf_ref.id_eq(block_id) {
            found_leaf = Some(leaf_ref);
        }
    });

    let leaf = match found_leaf {
        Some(l) => l,
        None => {
            // Block not in mutable set — check whether it exists as an exempt block
            // to distinguish BlockNotMutable from BlockNotFound.
            // `exempt_block_kind` is a zero-alloc-per-leaf scan over exempt blocks only
            // (ToolUse, Thinking, Unknown, non-text ToolResultLeaf).  It avoids the
            // O(B) `list_anthropic_blocks` call (which calls `walk_leaves` again plus
            // Phase-2 with a `format!`-allocated id per leaf).  The mutable set was
            // already scanned above with zero-alloc `id_eq`; we only need the exempt
            // pass here.
            return match exempt_block_kind(body, block_id) {
                Some(kind) => Err(LlmError::BlockNotMutable(block_id.to_string(), kind)),
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
    // Index-validity invariant: every LeafRef is produced by walk_leaves over this
    // same body (in mutate_anthropic above), and no structural mutation occurs
    // between the walk and this call.  All indices are therefore in-bounds by
    // construction.
    //
    // We use `assert!` (not `debug_assert!`) so the check is active in release
    // builds as well.  A LeafRef desync cannot be triggered by untrusted input
    // alone — it requires a code bug (e.g., a future refactor passes a LeafRef
    // derived from a different body).  However, ADR-006 requires that an
    // unrecoverable desync fails loud rather than silently; and in release the
    // slice index on line 150 would already panic on out-of-bounds, but with
    // a cryptic index-panic message.  Keeping `assert!` gives a clear diagnostic
    // in all build modes at negligible cost (one usize comparison per mutation).
    let msg_idx = match leaf {
        LeafRef::MessageString { msg_idx } => *msg_idx,
        LeafRef::TextBlock { msg_idx, .. } => *msg_idx,
        LeafRef::ToolResultString { msg_idx, .. } => *msg_idx,
        LeafRef::ToolResultLeaf { msg_idx, .. } => *msg_idx,
    };
    assert!(
        msg_idx < body.messages.len(),
        "apply_leaf_mutation: msg_idx {msg_idx} out of bounds (messages.len={}); \
         LeafRef was not derived from this body (ADR-006)",
        body.messages.len()
    );

    // `payload_slot_mut` navigates the typed model to the mutable `&mut String` named
    // by `leaf`, collapsing all structural-mismatch arms to one `NoTextPayload` site
    // (ADR-001).  The LeafRef was produced by `walk_leaves`, so a mismatch here can
    // only be caused by a code bug, not by untrusted input.
    let slot = payload_slot_mut(body, leaf)?;
    *slot = new_text.to_string();
    Ok(())
}

/// Navigate `body` to the mutable `&mut String` named by `leaf`.
///
/// Returns `Err(LlmError::NoTextPayload)` if the structural path does not match
/// the expected shape for `leaf`.  Because every `LeafRef` is produced by
/// `walk_leaves` over the same body, a mismatch here indicates a code bug
/// (e.g., a concurrent structural mutation between the walk and this call),
/// not untrusted input.
///
/// Extracts the 4-level nested match / multi-site `NoTextPayload` duplication from
/// `apply_leaf_mutation` into a single helper with one error site per arm (ADR-001).
fn payload_slot_mut<'b>(body: &'b mut AnthropicBody, leaf: &LeafRef) -> Result<&'b mut String> {
    match leaf {
        LeafRef::MessageString { msg_idx } => match &mut body.messages[*msg_idx].content {
            AnthropicContent::Text(s) => Ok(s),
            _ => Err(LlmError::NoTextPayload(leaf.id())),
        },
        LeafRef::TextBlock { msg_idx, blk_idx } => match &mut body.messages[*msg_idx].content {
            AnthropicContent::Blocks(blocks) => match &mut blocks[*blk_idx] {
                AnthropicBlock::Text(tb) => Ok(&mut tb.text),
                _ => Err(LlmError::NoTextPayload(leaf.id())),
            },
            _ => Err(LlmError::NoTextPayload(leaf.id())),
        },
        LeafRef::ToolResultString { msg_idx, blk_idx } => {
            match &mut body.messages[*msg_idx].content {
                AnthropicContent::Blocks(blocks) => match &mut blocks[*blk_idx] {
                    AnthropicBlock::ToolResult(tr) => match &mut tr.content {
                        Some(ToolResultContent::Text(s)) => Ok(s),
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
                        // `text` is `Option<String>`: None means no text payload.
                        leaf_block
                            .text
                            .as_mut()
                            .ok_or_else(|| LlmError::NoTextPayload(leaf.id()))
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

/// Return the `kind` string for an exempt (non-mutable) block matching `block_id`,
/// or `None` if no exempt block has that ID.
///
/// This is the zero-alloc fast path for the error branch of `mutate_anthropic`:
/// rather than calling `list_anthropic_blocks` (which re-walks mutable leaves via
/// `walk_leaves` AND allocates a `format!` id per leaf in Phase 2), we iterate only
/// the exempt block positions (ToolUse, Thinking, Unknown, non-text ToolResultLeaf)
/// and compare against `block_id` using `format!` only at the point of a match.
///
/// This is Phase 2 of `list_anthropic_blocks` extracted as a targeted lookup:
/// O(B) iteration but no per-leaf allocation until a match is found (and matches
/// are expected to be rare — this is the error path).
fn exempt_block_kind(body: &AnthropicBody, block_id: &str) -> Option<String> {
    for (mi, msg) in body.messages.iter().enumerate() {
        if let AnthropicContent::Blocks(blocks) = &msg.content {
            for (bi, block) in blocks.iter().enumerate() {
                match block {
                    AnthropicBlock::ToolUse(_) => {
                        if format!("m{mi}b{bi}") == block_id {
                            return Some("tool_use".to_string());
                        }
                    }
                    AnthropicBlock::Thinking(_) => {
                        if format!("m{mi}b{bi}") == block_id {
                            return Some("thinking".to_string());
                        }
                    }
                    AnthropicBlock::Unknown(_) => {
                        if format!("m{mi}b{bi}") == block_id {
                            return Some("unknown".to_string());
                        }
                    }
                    AnthropicBlock::ToolResult(tr) => {
                        if let Some(ToolResultContent::Blocks(leaves)) = &tr.content {
                            for (li, leaf) in leaves.iter().enumerate() {
                                if !(leaf.block_type == "text" && leaf.text.is_some())
                                    && format!("m{mi}b{bi}l{li}") == block_id
                                {
                                    return Some(format!("tool_result-leaf-{}", leaf.block_type));
                                }
                            }
                        }
                    }
                    // Text and MessageString are mutable — handled by walk_leaves, not here.
                    _ => {}
                }
            }
        }
    }
    None
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
