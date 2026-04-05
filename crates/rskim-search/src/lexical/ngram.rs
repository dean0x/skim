//! N-gram extraction from source text.
//!
//! Provides two extraction modes:
//! - [`extract_ngrams`] — index-time: all bigrams with border weighting
//! - [`extract_query_ngrams`] — query-time: covering-set strategy for selective matching
