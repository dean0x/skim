//! AST n-gram newtypes for structural code search.
//!
//! This module provides two newtypes — [`AstBigram`] and [`AstTrigram`] — that
//! pack tree-sitter node-kind ID pairs (and triples) into compact integer keys,
//! matching the encoding used by [`crate::ast_weights`].
//!
//! It also exposes vocabulary helpers ([`vocab_lookup`], [`vocab_resolve`],
//! [`vocab_len`]) and IDF weight lookup functions ([`ast_bigram_idf`],
//! [`ast_trigram_idf`]) that fall back to [`DEFAULT_AST_WEIGHT`] for unknown
//! entries.
//!
//! # Encoding
//!
//! - **Bigram**: `(u32::from(parent) << 16) | u32::from(child)`
//! - **Trigram**: `(u64::from(grandparent) << 32) | (u64::from(parent) << 16) | u64::from(child)`
//!
//! These formulas match `crates/rskim-research/src/ast_types.rs` exactly and are
//! the same layout stored in [`crate::ast_weights::RUST_AST_BIGRAM_WEIGHTS`] (and
//! the other per-language tables).

use std::fmt;

use rskim_core::Language;

use crate::ast_weights::{NODE_KIND_VOCABULARY, ast_bigram_weight, ast_trigram_weight};

// ============================================================================
// Type alias
// ============================================================================

/// Compact numeric ID for a tree-sitter node kind string.
///
/// Indexes into [`NODE_KIND_VOCABULARY`]. `0` is the sentinel for unknown kinds
/// (maps to `""`).
pub type NodeKindId = u16;

// ============================================================================
// Constants
// ============================================================================

/// Default IDF weight returned when a bigram or trigram is not found in any
/// per-language weight table.
///
/// Matches [`crate::weights::DEFAULT_WEIGHT`] so that AST and lexical signals
/// are on the same scale.
pub const DEFAULT_AST_WEIGHT: f32 = 1.0;

// ============================================================================
// AstBigram newtype
// ============================================================================

/// A packed parent→child AST node-kind pair encoded as a `u32`.
///
/// Encoding: `(u32::from(parent) << 16) | u32::from(child)`.
///
/// The encoding matches [`crate::ast_weights::ast_bigram_weight`], so weight
/// lookup via [`ast_bigram_idf`] is a single binary-search call with no
/// transformation.
#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AstBigram(u32);

impl AstBigram {
    /// Encode a parent–child node-kind pair into an [`AstBigram`].
    ///
    /// `encode(parent, child)` = `(u32::from(parent) << 16) | u32::from(child)`.
    #[must_use]
    #[inline]
    pub fn encode(parent: NodeKindId, child: NodeKindId) -> Self {
        Self((u32::from(parent) << 16) | u32::from(child))
    }

    /// Decode an [`AstBigram`] back into its `(parent, child)` component IDs.
    #[must_use]
    #[inline]
    pub fn decode(self) -> (NodeKindId, NodeKindId) {
        let parent = (self.0 >> 16) as NodeKindId;
        let child = (self.0 & 0xFFFF) as NodeKindId;
        (parent, child)
    }

    /// Return the raw `u32` encoding key.
    ///
    /// Suitable for passing directly to [`crate::ast_weights::ast_bigram_weight`].
    #[must_use]
    #[inline]
    pub fn key(self) -> u32 {
        self.0
    }

    /// Construct an [`AstBigram`] directly from a raw `u32` key.
    ///
    /// Intended for internal crate use where the key is already in the encoded
    /// form (e.g. when iterating over a stored weight table).  External callers
    /// should use [`encode`][Self::encode] to guarantee the correct encoding.
    #[must_use]
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn from_raw(key: u32) -> Self {
        Self(key)
    }
}

impl fmt::Display for AstBigram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (parent, child) = self.decode();
        fmt_kind_id(f, parent)?;
        write!(f, " > ")?;
        fmt_kind_id(f, child)
    }
}

// ============================================================================
// AstTrigram newtype
// ============================================================================

/// A packed grandparent→parent→child AST node-kind triple encoded as a `u64`.
///
/// Encoding: `(u64::from(grandparent) << 32) | (u64::from(parent) << 16) | u64::from(child)`.
///
/// The encoding matches [`crate::ast_weights::ast_trigram_weight`].
#[repr(transparent)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AstTrigram(u64);

impl AstTrigram {
    /// Encode a grandparent–parent–child node-kind triple into an [`AstTrigram`].
    ///
    /// `encode(gp, parent, child)` = `(u64::from(gp) << 32) | (u64::from(parent) << 16) | u64::from(child)`.
    #[must_use]
    #[inline]
    pub fn encode(grandparent: NodeKindId, parent: NodeKindId, child: NodeKindId) -> Self {
        Self((u64::from(grandparent) << 32) | (u64::from(parent) << 16) | u64::from(child))
    }

    /// Decode an [`AstTrigram`] back into its `(grandparent, parent, child)` component IDs.
    #[must_use]
    #[inline]
    pub fn decode(self) -> (NodeKindId, NodeKindId, NodeKindId) {
        let grandparent = ((self.0 >> 32) & 0xFFFF) as NodeKindId;
        let parent = ((self.0 >> 16) & 0xFFFF) as NodeKindId;
        let child = (self.0 & 0xFFFF) as NodeKindId;
        (grandparent, parent, child)
    }

    /// Return the raw `u64` encoding key.
    ///
    /// Suitable for passing directly to [`crate::ast_weights::ast_trigram_weight`].
    #[must_use]
    #[inline]
    pub fn key(self) -> u64 {
        self.0
    }

    /// Construct an [`AstTrigram`] directly from a raw `u64` key.
    ///
    /// Intended for internal crate use where the key is already in the encoded
    /// form.  External callers should use [`encode`][Self::encode].
    #[must_use]
    #[inline]
    #[allow(dead_code)]
    pub(crate) fn from_raw(key: u64) -> Self {
        Self(key)
    }
}

impl fmt::Display for AstTrigram {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (grandparent, parent, child) = self.decode();
        fmt_kind_id(f, grandparent)?;
        write!(f, " > ")?;
        fmt_kind_id(f, parent)?;
        write!(f, " > ")?;
        fmt_kind_id(f, child)
    }
}

// ============================================================================
// Vocabulary helpers
// ============================================================================

/// Look up the [`NodeKindId`] for a node-kind string.
///
/// Uses binary search on the sorted [`NODE_KIND_VOCABULARY`] table.
///
/// Returns `Some(0)` for `""` (the sentinel for unknown kinds) and `None` for
/// strings not present in the vocabulary.
#[must_use]
pub fn vocab_lookup(kind: &str) -> Option<NodeKindId> {
    NODE_KIND_VOCABULARY
        .binary_search(&kind)
        .ok()
        .map(|idx| idx as NodeKindId)
}

/// Resolve a [`NodeKindId`] back to its kind string.
///
/// Returns `Some("")` for ID `0` (the sentinel for unknown kinds) and `None`
/// for IDs beyond the vocabulary length.
#[must_use]
pub fn vocab_resolve(id: NodeKindId) -> Option<&'static str> {
    NODE_KIND_VOCABULARY.get(usize::from(id)).copied()
}

/// Return the total number of entries in [`NODE_KIND_VOCABULARY`].
#[must_use]
pub fn vocab_len() -> usize {
    NODE_KIND_VOCABULARY.len()
}

// ============================================================================
// Weight lookup
// ============================================================================

/// Look up the IDF weight for an [`AstBigram`] in the per-language weight table.
///
/// Falls back to [`DEFAULT_AST_WEIGHT`] when:
/// - The language has no AST weight table (JSON, YAML, TOML, and other
///   non-tree-sitter languages).
/// - The specific bigram is not in the table.
#[must_use]
pub fn ast_bigram_idf(lang: Language, bigram: AstBigram) -> f32 {
    ast_bigram_weight(lang.name(), bigram.key()).unwrap_or(DEFAULT_AST_WEIGHT)
}

/// Look up the IDF weight for an [`AstTrigram`] in the per-language weight table.
///
/// Falls back to [`DEFAULT_AST_WEIGHT`] when:
/// - The language has no AST weight table (JSON, YAML, TOML, and other
///   non-tree-sitter languages).
/// - The specific trigram is not in the table.
#[must_use]
pub fn ast_trigram_idf(lang: Language, trigram: AstTrigram) -> f32 {
    ast_trigram_weight(lang.name(), trigram.key()).unwrap_or(DEFAULT_AST_WEIGHT)
}

// ============================================================================
// Private helpers
// ============================================================================

/// Format a [`NodeKindId`] using the vocabulary.
///
/// - Known, non-empty string → write the string.
/// - ID `0` (sentinel / empty string) → write `"<unknown>"`.
/// - Out-of-bounds ID → write `"?{id}"`.
fn fmt_kind_id(f: &mut fmt::Formatter<'_>, id: NodeKindId) -> fmt::Result {
    match vocab_resolve(id) {
        Some("") => write!(f, "<unknown>"),
        Some(s) => write!(f, "{s}"),
        None => write!(f, "?{id}"),
    }
}

#[cfg(test)]
#[path = "ngram_tests.rs"]
mod tests;
