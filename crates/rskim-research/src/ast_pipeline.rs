//! High-level AST weight-table pipeline.
//!
//! Encapsulates the extract → stabilize → rekey → IDF → assemble sequence so
//! that both the `ast-run` CLI handler and integration tests can call a single
//! function rather than repeating the orchestration logic.

use std::collections::HashMap;

use crate::ast_extract;
use crate::ast_idf;
use crate::ast_types::{self, AstWeightTable, NodeKindVocabulary};
use crate::types::SourceFile;

/// Build an [`AstWeightTable`] from a pre-loaded corpus.
///
/// Runs the full pipeline:
/// 1. Create a shared [`NodeKindVocabulary`].
/// 2. Extract per-language bigram/trigram document-frequency maps via
///    [`ast_extract::extract_ast_ngrams_from_corpus`].
/// 3. [`stabilize`](NodeKindVocabulary::stabilize) the vocabulary
///    (sort alphabetically, reassign IDs) and obtain the remap table.
/// 4. Re-key all DF maps with the new IDs.
/// 5. Compute IDF weights per language and assemble the final table.
///
/// # Parameters
///
/// - `files`: Pre-loaded source files for the entire corpus.
/// - `threshold`: Minimum IDF score; entries below this are excluded.
/// - `collect_trigrams`: Whether to collect grandparent→parent→child trigrams
///   in addition to parent→child bigrams.
/// - `generated_at`: Timestamp string stored in the table's `generated_at`
///   field — supplied by the caller so this function remains pure (no I/O).
///
/// # Panics
///
/// Does not panic for well-formed inputs; the vocabulary overflow assertion in
/// [`NodeKindVocabulary::get_or_insert`] fires only if the corpus contains more
/// than 65 535 distinct node kinds, which is unreachable in practice.
#[must_use]
pub fn build_ast_weight_table(
    files: &[SourceFile],
    threshold: f32,
    collect_trigrams: bool,
    generated_at: &str,
) -> AstWeightTable {
    let mut vocab = NodeKindVocabulary::new();

    let (raw_bigram_df_maps, raw_trigram_df_maps, corpus_stats) =
        ast_extract::extract_ast_ngrams_from_corpus(files, &mut vocab, collect_trigrams);

    // Stabilize the vocabulary (alphabetical sort, reassign IDs) and obtain the
    // old→new ID remap table.  All DF map keys encoded with pre-stabilize IDs
    // must be re-keyed before IDF computation.
    let remap = vocab.stabilize();

    let total_docs = corpus_stats.total_files;

    let mut bigram_weights_map: HashMap<String, Vec<ast_types::AstBigramWeight>> = HashMap::new();
    let mut trigram_weights_map: HashMap<String, Vec<ast_types::AstTrigramWeight>> = HashMap::new();

    for (lang, df_map) in &raw_bigram_df_maps {
        let rekeyed = ast_types::rekey_bigram_df_map(df_map, &remap);
        let weights = ast_idf::compute_ast_bigram_weights(&rekeyed, total_docs, threshold, &vocab);
        bigram_weights_map.insert(lang.clone(), weights);
    }

    for (lang, df_map) in &raw_trigram_df_maps {
        let rekeyed = ast_types::rekey_trigram_df_map(df_map, &remap);
        let weights = ast_idf::compute_ast_trigram_weights(&rekeyed, total_docs, threshold, &vocab);
        trigram_weights_map.insert(lang.clone(), weights);
    }

    AstWeightTable {
        version: 1,
        generated_at: generated_at.to_string(),
        vocabulary: vocab.kinds().to_vec(),
        corpus_stats,
        bigram_weights: bigram_weights_map,
        trigram_weights: trigram_weights_map,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::types::SourceFile;
    use rskim_core::Language;
    use std::path::PathBuf;

    fn make_file(content: &str, language: Language) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test_file"),
            content: content.to_string(),
            language,
        }
    }

    /// Full pipeline smoke test: load fixture Rust source, run the pipeline,
    /// verify the output AstWeightTable has expected structural properties.
    ///
    /// This test exercises the sequence:
    /// extract_ast_ngrams_from_corpus → stabilize → rekey → IDF → assemble.
    #[test]
    fn pipeline_produces_non_empty_table_from_rust_source() {
        let source = include_str!("../tests/fixtures/sample_rust.rs");
        let files = vec![make_file(source, Language::Rust)];

        let table = build_ast_weight_table(&files, 0.0, true, "test");

        // Vocabulary must be non-empty: real Rust source yields many node kinds.
        assert!(
            !table.vocabulary.is_empty(),
            "vocabulary should not be empty for real Rust source"
        );

        // Vocabulary must be sorted alphabetically (stabilize invariant).
        let sorted = {
            let mut v = table.vocabulary.clone();
            v.sort();
            v
        };
        assert_eq!(
            table.vocabulary, sorted,
            "vocabulary must be in alphabetical order after stabilize"
        );

        // Bigram weights must be present for Rust.
        let rust_bigrams = table.bigram_weights.get("Rust").unwrap();
        assert!(
            !rust_bigrams.is_empty(),
            "expected non-empty bigram weights for Rust"
        );

        // Every IDF value must be finite and positive when threshold is 0.0.
        for w in rust_bigrams {
            assert!(
                w.idf.is_finite() && w.idf > 0.0,
                "IDF value {:.6} is not a positive finite number for bigram ({}, {})",
                w.idf,
                w.parent_kind,
                w.child_kind
            );
        }

        // Corpus stats must reflect the single file we provided.
        assert_eq!(table.corpus_stats.total_files, 1);
    }

    /// Verify the pipeline handles multiple files and produces weights for each
    /// language present.
    #[test]
    fn pipeline_handles_multi_language_corpus() {
        let rust_source = include_str!("../tests/fixtures/sample_rust.rs");
        let ts_source = include_str!("../tests/fixtures/sample_typescript.ts");
        let files = vec![
            make_file(rust_source, Language::Rust),
            make_file(ts_source, Language::TypeScript),
        ];

        let table = build_ast_weight_table(&files, 0.0, false, "test");

        // Both languages should have bigram weight entries.
        assert!(
            table.bigram_weights.contains_key("Rust"),
            "Rust weights should be present"
        );
        assert!(
            table.bigram_weights.contains_key("TypeScript"),
            "TypeScript weights should be present"
        );

        // When collect_trigrams is false, trigram maps must be empty vecs (not absent keys).
        // The pipeline inserts empty vecs for each language found.
        for (lang, trigrams) in &table.trigram_weights {
            assert!(
                trigrams.is_empty(),
                "trigrams should be empty for {lang} when collect_trigrams=false"
            );
        }

        // Total files: 2 (one per language, no deduplication).
        assert_eq!(table.corpus_stats.total_files, 2);
    }

    /// Verify that empty input produces an empty table without panicking.
    #[test]
    fn pipeline_empty_corpus_returns_empty_table() {
        let table = build_ast_weight_table(&[], 1.5, true, "test");

        assert!(table.vocabulary.is_empty());
        assert!(table.bigram_weights.is_empty());
        assert!(table.trigram_weights.is_empty());
        assert_eq!(table.corpus_stats.total_files, 0);
    }

    /// Verify threshold filters out low-IDF entries.
    ///
    /// With a very high threshold, no bigrams should survive.
    #[test]
    fn pipeline_high_threshold_produces_empty_weights() {
        let source = include_str!("../tests/fixtures/sample_rust.rs");
        let files = vec![make_file(source, Language::Rust)];

        // IDF max for 1 document is ln(1/2)+1 ≈ 0.31, so threshold=10.0 eliminates all.
        let table = build_ast_weight_table(&files, 10.0, false, "test");

        for (lang, weights) in &table.bigram_weights {
            assert!(
                weights.is_empty(),
                "expected no weights for {lang} with threshold=10.0"
            );
        }
    }

    /// Verify that `generated_at` is passed through unmodified.
    #[test]
    fn pipeline_preserves_generated_at() {
        let table = build_ast_weight_table(&[], 0.0, false, "unix:12345");
        assert_eq!(table.generated_at, "unix:12345");
    }
}
