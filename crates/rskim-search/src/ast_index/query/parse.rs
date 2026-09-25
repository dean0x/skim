//! AST query parser — `String → AstQuery` boundary.
//!
//! The only public entry point is [`parse_ast_query`], which validates and
//! classifies a raw query string into a typed [`AstQuery`] variant.
//!
//! [`AstQuery`] lives here rather than in `engine` because this module is its
//! sole constructor; hoisting the type here breaks the otherwise-cyclic
//! `parse → engine → parse` import chain and lets dependencies flow
//! `engine → parse` in one direction only.

use crate::ast_index::{
    AstBigram, AstBigramEntry, AstNgramSet, AstTrigram, AstTrigramEntry, DEFAULT_AST_WEIGHT,
    NodeKindId, Pattern, lookup_pattern, vocab_lookup,
};
use crate::{Result, SearchError};

/// A parsed, validated AST structural query.
///
/// Created exclusively via [`parse_ast_query`] — the only
/// `String → AstQuery` boundary.
#[derive(Debug, Clone)]
pub enum AstQuery {
    /// Named catalog pattern (e.g. `"try-catch"`). Resolved at execution time.
    Pattern(&'static Pattern),
    /// Depth-1 bigram (`A > B`) or depth-2 trigram (`A > B > C`); deduped.
    Containment(AstNgramSet),
    /// Validated single node kind. Execution deferred to #283 (unigram index).
    SingleNode(NodeKindId),
}

impl PartialEq for AstQuery {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Pattern(a), Self::Pattern(b)) => std::ptr::eq(*a, *b),
            (Self::Containment(a), Self::Containment(b)) => a == b,
            (Self::SingleNode(a), Self::SingleNode(b)) => a == b,
            _ => false,
        }
    }
}

/// Maximum allowed byte length for a raw query string (reliability bound).
/// Aliased from [`crate::lexical::MAX_QUERY_BYTES`] so both layers share one source of truth.
pub(super) const MAX_AST_QUERY_BYTES: usize = crate::lexical::MAX_QUERY_BYTES;

const EMPTY_QUERY_MSG: &str = "empty AST query";

/// Parse a raw string into an [`AstQuery`].
///
/// **Only** `String → AstQuery` boundary; total (never panics). Rejects
/// strings longer than `MAX_AST_QUERY_BYTES` (4096 bytes).
///
/// | Input form | Result |
/// |---|---|
/// | `"try-catch"` | [`AstQuery::Pattern`] (hyphen → catalog lookup) |
/// | `"A > B"` | [`AstQuery::Containment`] bigram |
/// | `"A > B > C"` | [`AstQuery::Containment`] trigram |
/// | `"try_statement"` | [`AstQuery::SingleNode`] (vocab-validated) |
///
/// Returns [`SearchError::InvalidQuery`] for unknown kinds/patterns, empty
/// segments, `>>`, depth > 2, or inputs > 4096 bytes.
pub fn parse_ast_query(s: &str) -> Result<AstQuery> {
    if s.len() > MAX_AST_QUERY_BYTES {
        return Err(SearchError::InvalidQuery(format!(
            "AST query too long: {} bytes (max {MAX_AST_QUERY_BYTES})",
            s.len()
        )));
    }
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(SearchError::InvalidQuery(EMPTY_QUERY_MSG.into()));
    }
    if trimmed.contains(">>") {
        return Err(SearchError::InvalidQuery(
            "transitive ancestor operator `>>` is not supported; use `>` for direct containment"
                .into(),
        ));
    }

    let segments: Vec<&str> = trimmed.split('>').map(str::trim).collect();
    for seg in &segments {
        if seg.is_empty() {
            return Err(SearchError::InvalidQuery(
                "empty segment in query: check for trailing or doubled `>` operators".into(),
            ));
        }
    }

    match segments.len() {
        1 => parse_single(segments[0]),
        2 => parse_bigram(segments[0], segments[1]),
        3 => parse_trigram(segments[0], segments[1], segments[2]),
        n => Err(SearchError::InvalidQuery(format!(
            "containment depth > 2 is not supported ({n} segments); maximum is `A > B > C`"
        ))),
    }
}

fn parse_single(token: &str) -> Result<AstQuery> {
    if token.contains('-') {
        return Ok(AstQuery::Pattern(lookup_pattern(token)?));
    }
    vocab_lookup(token)
        .map(AstQuery::SingleNode)
        .ok_or_else(|| {
            SearchError::InvalidQuery(format!(
                "unknown node kind '{token}'; \
             use a valid tree-sitter node kind or a hyphenated pattern name"
            ))
        })
}

fn parse_bigram(a: &str, b: &str) -> Result<AstQuery> {
    let bigram = AstBigram::encode(kind(a)?, kind(b)?);
    Ok(AstQuery::Containment(AstNgramSet {
        bigrams: vec![AstBigramEntry {
            ngram: bigram,
            // weight/count unused on query path; meaningful only at index build.
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        }],
        trigrams: vec![],
    }))
}

fn parse_trigram(a: &str, b: &str, c: &str) -> Result<AstQuery> {
    let trigram = AstTrigram::encode(kind(a)?, kind(b)?, kind(c)?);
    Ok(AstQuery::Containment(AstNgramSet {
        bigrams: vec![],
        trigrams: vec![AstTrigramEntry {
            ngram: trigram,
            // weight/count unused on query path; meaningful only at index build.
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        }],
    }))
}

/// Resolve a containment segment to a [`NodeKindId`] or return `InvalidQuery`.
fn kind(seg: &str) -> Result<NodeKindId> {
    vocab_lookup(seg).ok_or_else(|| {
        SearchError::InvalidQuery(format!(
            "unknown node kind '{seg}' in containment query; \
             use a valid tree-sitter node kind (e.g. `function_item`, `block`)"
        ))
    })
}
