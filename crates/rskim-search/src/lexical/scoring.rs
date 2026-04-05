//! BM25F scoring formula with per-field boost weights.
//!
//! Implements the Okapi BM25F variant that weights term frequencies
//! per semantic field (type definitions, function signatures, etc.)
//! using the boost factors from [`SearchField::default_boost`].
