//! Query validation boundary for the skim-search lexical layer.
//!
//! [`QueryEngine`] is a [`SearchLayer`] decorator that validates incoming
//! [`SearchQuery`] values at the trust boundary before delegating to an inner
//! layer. Empty queries short-circuit to `Ok(vec![])`, oversized queries and
//! invalid BM25F configs are rejected with [`SearchError::InvalidQuery`], and
//! all other queries are forwarded unchanged.

use crate::{Result, SearchError, SearchLayer, SearchQuery, SearchResult};

/// Maximum allowed byte length for a query text string.
///
/// Queries longer than this are rejected before any index I/O occurs.
/// 4 KiB is well beyond any reasonable human or tool-generated query.
pub const MAX_QUERY_BYTES: usize = 4096;

/// A [`SearchLayer`] decorator that validates queries at the trust boundary.
///
/// Wrap any existing [`SearchLayer`] in a `QueryEngine` to ensure callers
/// receive well-typed errors for invalid input rather than undefined behaviour
/// from downstream index code.
///
/// # Example
///
/// ```rust,no_run
/// use rskim_search::{QueryEngine, SearchQuery, SearchLayer};
///
/// fn run(inner: Box<dyn SearchLayer>) {
///     let engine = QueryEngine::new(inner);
///     let results = engine.search(&SearchQuery::new("handleRequest")).unwrap();
///     println!("{} results", results.len());
/// }
/// ```
pub struct QueryEngine {
    inner: Box<dyn SearchLayer>,
}

impl QueryEngine {
    /// Wrap an existing [`SearchLayer`] with query validation.
    #[must_use]
    pub fn new(inner: Box<dyn SearchLayer>) -> Self {
        Self { inner }
    }
}

impl SearchLayer for QueryEngine {
    // Intentional defense-in-depth: the inner layer may also validate empty
    // text and BM25F config, but we validate at the decorator boundary so the
    // behaviour is independent of the inner layer.
    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        if query.text.is_empty() {
            return Ok(Vec::new());
        }

        if query.text.len() > MAX_QUERY_BYTES {
            return Err(SearchError::InvalidQuery(format!(
                "query exceeds maximum length of {MAX_QUERY_BYTES} bytes (got {} bytes)",
                query.text.len()
            )));
        }

        if let Some(ref cfg) = query.bm25f_config {
            cfg.validate()?;
        }

        self.inner.search(query)
    }

    fn name(&self) -> &str {
        "query-engine"
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
