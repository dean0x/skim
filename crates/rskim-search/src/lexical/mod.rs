//! BM25F fielded scoring engine for the skim-search lexical index.
//!
//! This module exposes:
//! - [`BM25FConfig`] — per-field boost and normalisation parameters.
//! - [`classify_source`] — map source byte ranges to [`crate::SearchField`] variants.
//! - [`bm25f_score`] — compute the BM25F score for a single query term.
//! - [`dominant_field`] — return the [`crate::SearchField`] with the highest TF.
//!
//! # Format impact
//!
//! Enabling BM25F requires format v2 (see [`crate::index::format`]): the header
//! gains `avg_field_lengths: [f32; 8]` and each [`FileMetaEntry`] gains
//! `field_lengths: [u32; 8]`.

pub mod classifier;
pub mod config;
pub mod scoring;

pub use classifier::classify_source;
pub use config::{BM25FConfig, FIELD_COUNT};
pub use scoring::{bm25f_score, dominant_field};
