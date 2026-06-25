//! Core types for rskim-research bigram IDF analysis tool.

use std::path::PathBuf;

use rskim_core::Language;
use serde::{Deserialize, Serialize};

/// A single bigram with its IDF weight.
///
/// The bigram is encoded as a `u16` where the high byte is the first byte
/// and the low byte is the second byte of the byte pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BigramWeight {
    pub bigram: u16,
    pub idf: f32,
}

/// A source file loaded from the corpus.
pub struct SourceFile {
    pub path: PathBuf,
    pub language: Language,
    pub content: String,
}

/// Aggregated statistics about the analyzed corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorpusStats {
    pub total_files: u32,
    pub total_bigrams: u64,
    pub unique_bigrams: usize,
    pub deduplicated_files: u32,
    pub language_breakdown: Vec<LanguageCount>,
}

/// File count for a single language in the corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageCount {
    pub language: String,
    pub file_count: u32,
}

/// Result of comparing uniform vs. border-weighted selectivity strategies.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub uniform_selectivity: f64,
    pub border_weighted_selectivity: f64,
    pub improvement_pct: f64,
}

/// The full weight table written to JSON and used for codegen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightTable {
    pub version: u8,
    pub generated_at: String,
    pub corpus_stats: CorpusStats,
    pub weights: Vec<BigramWeight>,
}

/// A single trigram with its IDF weight.
///
/// The trigram is encoded as a `u32` where:
/// - bits 23-16 = first byte (b1)
/// - bits 15-8  = second byte (b2)
/// - bits 7-0   = third byte (b3)
///
/// This matches the `Ngram` encoding in `rskim-search`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrigramWeight {
    pub trigram: u32,
    pub idf: f32,
}

/// The full trigram weight table written to JSON and used for codegen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrigramWeightTable {
    pub version: u8,
    pub generated_at: String,
    pub corpus_stats: CorpusStats,
    pub weights: Vec<TrigramWeight>,
}

/// Statistics from the SHA-256 deduplication pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeduplicationStats {
    pub total_files_seen: u32,
    pub unique_files: u32,
    pub duplicates_removed: u32,
}
