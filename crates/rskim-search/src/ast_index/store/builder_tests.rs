//! Tests for [`AstIndexBuilder`].

#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use tempfile::tempdir;

use super::*;
use crate::{
    FileId,
    ast_index::{
        AstBigram, AstBigramEntry, AstNgramSet, AstTrigram, AstTrigramEntry, DEFAULT_AST_WEIGHT,
        StructuralMetrics,
    },
};
use rskim_core::Language;

// ============================================================================
// Helpers: build synthetic AstNgramSet without touching tree-sitter
// ============================================================================

/// Build a minimal [`AstNgramSet`] with a single bigram.
fn single_bigram_set(bigram_key: u32, count: u32) -> AstNgramSet {
    AstNgramSet {
        bigrams: vec![AstBigramEntry {
            ngram: AstBigram(bigram_key),
            weight: DEFAULT_AST_WEIGHT,
            count,
        }],
        trigrams: vec![],
    }
}

/// Build a minimal [`AstNgramSet`] with a single trigram.
fn single_trigram_set(trigram_key: u64, count: u32) -> AstNgramSet {
    AstNgramSet {
        bigrams: vec![],
        trigrams: vec![AstTrigramEntry {
            ngram: AstTrigram(trigram_key),
            weight: DEFAULT_AST_WEIGHT,
            count,
        }],
    }
}

/// Build an [`AstNgramSet`] with multiple bigrams.
fn multi_bigram_set(pairs: &[(u32, u32)]) -> AstNgramSet {
    let mut bigrams: Vec<AstBigramEntry> = pairs
        .iter()
        .map(|&(key, count)| AstBigramEntry {
            ngram: AstBigram(key),
            weight: DEFAULT_AST_WEIGHT,
            count,
        })
        .collect();
    bigrams.sort_unstable_by_key(|e| e.ngram.key());
    AstNgramSet {
        bigrams,
        trigrams: vec![],
    }
}

// ============================================================================
// A7: Empty build (zero files)
// ============================================================================

#[test]
fn a7_empty_build_succeeds() {
    let dir = tempdir().unwrap();
    let builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let reader = builder.build().unwrap();
    assert_eq!(reader.file_count(), 0);
    assert_eq!(reader.avg_node_count(), 0.0);
    assert_eq!(reader.avg_bigram_count(), 0.0);
    assert_eq!(reader.avg_trigram_count(), 0.0);
}

#[test]
fn a7_empty_build_post_mmap_is_none_when_no_postings() {
    let dir = tempdir().unwrap();
    let builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    // With zero files there are zero postings, so skpost should be 0 bytes.
    // The reader must handle this without mmap-ing a zero-length file.
    let reader = builder.build().unwrap();
    // lookup should return empty (no panic)
    let postings = reader.lookup_bigram(AstBigram(42)).unwrap();
    assert!(postings.is_empty());
}

// ============================================================================
// A8: Zero-ngram files still get a FileMetaEntry
// ============================================================================

#[test]
fn a8_zero_ngram_file_gets_meta_entry() {
    let dir = tempdir().unwrap();
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    // Add three files: one with n-grams, two without
    let set_with = single_bigram_set(0x0001_0002, 1);
    let empty_set = AstNgramSet::default();

    builder
        .add_file_ngrams(
            FileId(0),
            Language::Rust,
            &set_with,
            100,
            StructuralMetrics::default(),
        )
        .unwrap();
    builder
        .add_file_ngrams(
            FileId(1),
            Language::Python,
            &empty_set,
            0,
            StructuralMetrics::default(),
        )
        .unwrap();
    builder
        .add_file_ngrams(
            FileId(2),
            Language::Go,
            &empty_set,
            50,
            StructuralMetrics::default(),
        )
        .unwrap();

    let reader = builder.build().unwrap();
    assert_eq!(reader.file_count(), 3);

    // All three should have meta entries
    let meta0 = reader.file_meta(0).unwrap();
    let meta1 = reader.file_meta(1).unwrap();
    let meta2 = reader.file_meta(2).unwrap();

    assert_eq!(meta0.node_count, 100);
    assert_eq!(meta1.node_count, 0);
    assert_eq!(meta2.node_count, 50);
}

// ============================================================================
// A9: FileId guards
// ============================================================================

#[test]
fn a9_duplicate_file_id_rejected() {
    let dir = tempdir().unwrap();
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let empty = AstNgramSet::default();

    builder
        .add_file_ngrams(
            FileId(0),
            Language::Rust,
            &empty,
            0,
            StructuralMetrics::default(),
        )
        .unwrap();
    let err = builder
        .add_file_ngrams(
            FileId(0),
            Language::Rust,
            &empty,
            0,
            StructuralMetrics::default(),
        )
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("duplicate"), "expected 'duplicate' in: {msg}");
}

#[test]
fn a9_non_sequential_first_id_rejected() {
    let dir = tempdir().unwrap();
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let empty = AstNgramSet::default();

    // First FileId should be 0, not 5
    let err = builder
        .add_file_ngrams(
            FileId(5),
            Language::Rust,
            &empty,
            0,
            StructuralMetrics::default(),
        )
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("sequential"),
        "expected 'sequential' in: {msg}"
    );
}

#[test]
fn a9_gap_in_file_ids_rejected() {
    let dir = tempdir().unwrap();
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let empty = AstNgramSet::default();

    builder
        .add_file_ngrams(
            FileId(0),
            Language::Rust,
            &empty,
            0,
            StructuralMetrics::default(),
        )
        .unwrap();
    // FileId(2) skips FileId(1)
    let err = builder
        .add_file_ngrams(
            FileId(2),
            Language::Rust,
            &empty,
            0,
            StructuralMetrics::default(),
        )
        .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("sequential"),
        "expected 'sequential' in: {msg}"
    );
}

// ============================================================================
// A2: Multi-file posting merge — sorted unique doc_ids
// ============================================================================

#[test]
fn a2_posting_merge_sorted_unique_doc_ids() {
    // NOTE ON THE SORT (defense-in-depth, not exercised by construction):
    // The builder's sequential-FileId invariant means FileId(n).0 == n for every
    // file added, so doc_ids are inserted into posting lists in ascending order.
    // The `sort_unstable_by_key(|p| p.doc_id)` call in `build()` therefore never
    // observes out-of-order input — it is defense-in-depth against future callers
    // that might bypass the sequential guard (e.g. a parallel merge variant).
    //
    // What this test CAN verify (C1): the output is sorted and contains no
    // duplicate doc_ids.  We assert this via `windows(2)` on a larger corpus
    // (10 files × 3 distinct keys) so the invariant is observable even if the
    // sort is a no-op in the current implementation.

    let dir = tempdir().unwrap();
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let key_a: u32 = 0x0001_0002;
    let key_b: u32 = 0x0002_0003;
    let key_c: u32 = 0x0003_0004;

    // 10 files, each contributing all three bigram keys.
    for i in 0..10u32 {
        let set = multi_bigram_set(&[(key_a, 1), (key_b, 2), (key_c, 3)]);
        builder
            .add_file_ngrams(
                FileId(i),
                Language::Rust,
                &set,
                10,
                StructuralMetrics::default(),
            )
            .unwrap();
    }

    let reader = builder.build().unwrap();

    // C1: each posting list is sorted ascending by doc_id with no duplicates.
    for &key in &[key_a, key_b, key_c] {
        let postings = reader
            .lookup_bigram(crate::ast_index::AstBigram(key))
            .unwrap();
        assert_eq!(postings.len(), 10, "expected 10 postings for key {key:#x}");
        assert!(
            postings.windows(2).all(|w| w[0].doc_id < w[1].doc_id),
            "C1 violated: postings for key {key:#x} are not strictly ascending by doc_id: {postings:?}"
        );
    }

    // C1 (no duplicate doc_ids): spot-check key_a has exactly one entry per file.
    let postings_a = reader
        .lookup_bigram(crate::ast_index::AstBigram(key_a))
        .unwrap();
    let unique: std::collections::HashSet<u32> = postings_a.iter().map(|p| p.doc_id).collect();
    assert_eq!(
        unique.len(),
        10,
        "C1 violated: duplicate doc_ids in posting list for key_a"
    );
}

// ============================================================================
// A4/C4/C5: count is the per-file structural term-frequency
// ============================================================================

#[test]
fn a4_count_preserved_from_ngram_entry() {
    let dir = tempdir().unwrap();
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let key: u32 = 0x0003_0004;

    let set = single_bigram_set(key, 7);
    builder
        .add_file_ngrams(
            FileId(0),
            Language::Rust,
            &set,
            100,
            StructuralMetrics::default(),
        )
        .unwrap();

    let reader = builder.build().unwrap();
    let postings = reader
        .lookup_bigram(crate::ast_index::AstBigram(key))
        .unwrap();

    assert_eq!(postings.len(), 1);
    assert_eq!(
        postings[0].count, 7,
        "count should be the term-frequency from extraction, not 1"
    );
}

// ============================================================================
// A10: Atomic write — no temp file leftovers
// ============================================================================

#[test]
fn a10_atomic_write_no_temp_leftovers() {
    let dir = tempdir().unwrap();
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let empty = AstNgramSet::default();
    builder
        .add_file_ngrams(
            FileId(0),
            Language::Rust,
            &empty,
            0,
            StructuralMetrics::default(),
        )
        .unwrap();
    builder.build().unwrap();

    // Only the two expected files should exist (plus any Criterion/test artifacts)
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();

    // Exactly the two index files
    assert!(
        entries.contains(&"ast_index.skidx".to_string()),
        "expected ast_index.skidx, found: {entries:?}"
    );
    assert!(
        entries.contains(&"ast_index.skpost".to_string()),
        "expected ast_index.skpost, found: {entries:?}"
    );
    // No temp files (no `.tmp` or similar)
    let temp_files: Vec<_> = entries.iter().filter(|e| e.ends_with(".tmp")).collect();
    assert!(
        temp_files.is_empty(),
        "unexpected temp files: {temp_files:?}"
    );
}

// ============================================================================
// avg_node_count correctness
// ============================================================================

#[test]
fn avg_node_count_computed_correctly() {
    let dir = tempdir().unwrap();
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let empty = AstNgramSet::default();

    builder
        .add_file_ngrams(
            FileId(0),
            Language::Rust,
            &empty,
            10,
            StructuralMetrics::default(),
        )
        .unwrap();
    builder
        .add_file_ngrams(
            FileId(1),
            Language::Python,
            &empty,
            20,
            StructuralMetrics::default(),
        )
        .unwrap();
    builder
        .add_file_ngrams(
            FileId(2),
            Language::Go,
            &empty,
            30,
            StructuralMetrics::default(),
        )
        .unwrap();

    let reader = builder.build().unwrap();
    // avg = (10 + 20 + 30) / 3 = 20.0
    assert!(
        (reader.avg_node_count() - 20.0).abs() < 1e-4,
        "avg_node_count should be 20.0, got {}",
        reader.avg_node_count()
    );
}

// ============================================================================
// build_from_files determinism
// ============================================================================

#[test]
fn build_from_files_deterministic_bytes() {
    let dir1 = tempdir().unwrap();
    let dir2 = tempdir().unwrap();

    let files: Vec<(FileId, &str, Language)> = vec![
        (
            FileId(0),
            "pub fn foo(x: i32) -> i32 { x + 1 }",
            Language::Rust,
        ),
        (FileId(1), "def bar(y): return y * 2", Language::Python),
        (FileId(2), "", Language::Go), // non-tree-sitter → empty set
    ];

    // Sequential build
    let mut seq_builder = AstIndexBuilder::new(dir1.path().to_path_buf()).unwrap();
    for (id, content, lang) in &files {
        seq_builder.add_file(*id, content, *lang).unwrap();
    }
    seq_builder.build().unwrap();

    // Parallel build_from_files
    let file_refs: Vec<(FileId, &str, Language)> = files.clone();
    AstIndexBuilder::build_from_files(dir2.path().to_path_buf(), &file_refs).unwrap();

    // Compare the two .skidx files byte-for-byte
    let seq_idx = std::fs::read(dir1.path().join("ast_index.skidx")).unwrap();
    let par_idx = std::fs::read(dir2.path().join("ast_index.skidx")).unwrap();
    assert_eq!(
        seq_idx, par_idx,
        "build_from_files must produce identical bytes to sequential add_file"
    );

    // Compare the two .skpost files byte-for-byte
    let seq_post = std::fs::read(dir1.path().join("ast_index.skpost")).unwrap();
    let par_post = std::fs::read(dir2.path().join("ast_index.skpost")).unwrap();
    assert_eq!(
        seq_post, par_post,
        "build_from_files skpost must be identical to sequential skpost"
    );
}

// ============================================================================
// new: missing directory
// ============================================================================

#[test]
fn new_missing_dir_returns_io_error() {
    let err = AstIndexBuilder::new(std::path::PathBuf::from("/nonexistent/path/xyz")).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("IO error") || msg.contains("does not exist"),
        "expected IO/not-found error, got: {msg}"
    );
}
