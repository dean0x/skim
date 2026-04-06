//! Tree-sitter AST walker for the lexical index builder.
//!
//! Encapsulates mutable accumulation state in [`WalkContext`] and exposes a
//! single entry point, [`walk_and_classify`], for use by the index builder.

use rustc_hash::FxHashMap;

use crate::FieldClassifier;

use super::{Ngram, PostingEntry};
use super::ngram::extract_ngrams;

// ============================================================================
// Constants
// ============================================================================

/// Maximum AST traversal depth. Prevents stack overflow on pathologically
/// deep or adversarial ASTs. 128 levels covers all realistic source files.
pub(crate) const MAX_AST_DEPTH: u32 = 128;

// ============================================================================
// WalkContext
// ============================================================================

/// Mutable accumulation state for a single document walk.
///
/// Bundles the parameters that change per-document to reduce the arity of
/// [`walk_and_classify_inner`] from 7 to 3 (`ctx`, `node`, `depth`).
pub(crate) struct WalkContext<'a> {
    /// Source text of the document being walked.
    pub source: &'a str,
    /// Field classifier for the document's language.
    pub classifier: &'a dyn FieldClassifier,
    /// Stable numeric identifier for this document.
    pub doc_id: u32,
    /// Accumulated posting lists (ngram → entries).
    pub postings: &'a mut FxHashMap<Ngram, Vec<PostingEntry>>,
    /// Running total of n-gram tokens for this document.
    pub doc_len: &'a mut u32,
}

// ============================================================================
// Public entry point
// ============================================================================

/// Walk a tree-sitter AST, classifying and indexing each node.
///
/// Traversal stops at [`MAX_AST_DEPTH`] to prevent stack overflow on
/// pathological or adversarial ASTs.
///
/// This function is `pub(crate)` so the builder can call it directly.
pub(crate) fn walk_and_classify(
    node: tree_sitter::Node<'_>,
    ctx: &mut WalkContext<'_>,
) {
    walk_and_classify_inner(node, ctx, 0);
}

// ============================================================================
// Private recursive implementation
// ============================================================================

fn walk_and_classify_inner(
    node: tree_sitter::Node<'_>,
    ctx: &mut WalkContext<'_>,
    depth: u32,
) {
    if depth >= MAX_AST_DEPTH {
        return;
    }

    if let Some(field) = ctx.classifier.classify_node(&node, ctx.source) {
        if let Ok(text) = node.utf8_text(ctx.source.as_bytes()) {
            let ngrams = extract_ngrams(text);
            for (ngram, weight) in &ngrams {
                let entry = PostingEntry {
                    doc_id: ctx.doc_id,
                    field_id: field.as_u8(),
                    position: u32::try_from(node.start_byte()).unwrap_or(u32::MAX),
                    tf: weight.max(1.0).min(f32::from(u16::MAX)) as u16,
                };
                ctx.postings.entry(*ngram).or_default().push(entry);
            }
            *ctx.doc_len = ctx
                .doc_len
                .saturating_add(u32::try_from(ngrams.len()).unwrap_or(u32::MAX));
        }
    }

    // Recurse into children.
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            walk_and_classify_inner(child, ctx, depth + 1);
        }
    }
}

