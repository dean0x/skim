//! Query validation boundary for the skim-search lexical layer.
//!
//! [`QueryEngine`] is a [`SearchLayer`] decorator that validates incoming
//! [`SearchQuery`] values at the trust boundary before delegating to an inner
//! layer. This keeps validation concerns out of low-level index code.
//!
//! # Validation rules
//!
//! 1. Empty `query.text` → immediate `Ok(vec![])` (no round-trip to inner layer).
//! 2. `query.text.len() > MAX_QUERY_BYTES` → `Err(SearchError::InvalidQuery)`.
//! 3. `query.bm25f_config` present and invalid → `Err(SearchError::InvalidQuery)`
//!    (fail-fast before any index I/O).
//! 4. All other queries are forwarded **unchanged** to the inner layer.
//!
//! # Design notes
//!
//! - The original `SearchQuery` is passed to the inner layer unchanged so that
//!   the inner layer retains full control over scoring, filtering, and pagination.
//! - There is no `PreparedQuery` or ngram-budget logic in this layer; those are
//!   deferred to Wave 4.

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
    pub fn new(inner: Box<dyn SearchLayer>) -> Self {
        Self { inner }
    }
}

impl SearchLayer for QueryEngine {
    /// Validate the query, then delegate to the inner layer.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::InvalidQuery`] when:
    /// - `query.text.len()` exceeds [`MAX_QUERY_BYTES`].
    /// - `query.bm25f_config` is present but fails validation.
    ///
    /// Returns whatever the inner layer returns for all other errors.
    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        // Rule 1: empty text → short-circuit without touching the inner layer.
        if query.text.is_empty() {
            return Ok(Vec::new());
        }

        // Rule 2: length guard — reject unreasonably large queries.
        if query.text.len() > MAX_QUERY_BYTES {
            return Err(SearchError::InvalidQuery(format!(
                "query exceeds maximum length of {MAX_QUERY_BYTES} bytes (got {} bytes)",
                query.text.len()
            )));
        }

        // Rule 3: BM25F config validation — fail fast before index I/O.
        if let Some(ref cfg) = query.bm25f_config {
            cfg.validate()?;
        }

        // Rule 4: delegate the original query unchanged.
        self.inner.search(query)
    }

    fn name(&self) -> &str {
        "query-engine"
    }
}

// -----------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
