//! Tests for [`crate::ast_index::query`] — Wave 3f (#197).
//!
//! Organized into three test groups:
//! 1. **Parser** — pure `parse_ast_query` tests (no I/O).
//! 2. **Execution/scoring** — via `FakePostingSource` (no I/O, deterministic).
//! 3. **SearchLayer adapter** — via real `AstIndexBuilder`/`AstIndexReader`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{HashMap, HashSet};

use rskim_core::Language;
use tempfile::tempdir;

use super::*;
use crate::{
    FileId,
    ast_index::{
        AstBigram, AstBigramEntry, AstFileMetaEntry, AstIndexBuilder, AstNgramSet, AstPosting,
        AstTrigram, AstTrigramEntry, DEFAULT_AST_WEIGHT, StructuralMetrics, lookup_pattern,
        parse_ast_query, vocab_lookup,
    },
    types::SearchQuery,
};

// ============================================================================
// Compile-time Send + Sync assertion (B5)
// ============================================================================

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn b5_query_engine_is_send_sync() {
    assert_send_sync::<AstQueryEngine<AstIndexReader>>();
}

// ============================================================================
// FakePostingSource — in-memory test double
// ============================================================================

#[derive(Default)]
struct FakePostingSource {
    bigrams: HashMap<u32, Vec<AstPosting>>,
    trigrams: HashMap<u64, Vec<AstPosting>>,
    file_metas: HashMap<u32, AstFileMetaEntry>,
    avg_node_count: f32,
    file_count: u32,
}

impl FakePostingSource {
    fn with_file(mut self, doc_id: u32, lang: Language, node_count: u32) -> Self {
        use crate::index::lang_map::lang_to_id;
        self.file_metas.insert(
            doc_id,
            AstFileMetaEntry {
                lang_id: lang_to_id(lang),
                node_count,
                max_depth: 3,
                max_block_stmts: 5,
                max_params: 2,
                branch_count: 1,
            },
        );
        self.file_count = self.file_count.max(doc_id + 1);
        self
    }

    fn with_bigram(mut self, key: u32, postings: Vec<AstPosting>) -> Self {
        self.bigrams.insert(key, postings);
        self
    }

    fn with_trigram(mut self, key: u64, postings: Vec<AstPosting>) -> Self {
        self.trigrams.insert(key, postings);
        self
    }

    fn with_avg_node_count(mut self, avg: f32) -> Self {
        self.avg_node_count = avg;
        self
    }
}

impl AstPostingSource for FakePostingSource {
    fn lookup_bigram(&self, b: AstBigram) -> crate::Result<Vec<AstPosting>> {
        Ok(self.bigrams.get(&b.key()).cloned().unwrap_or_default())
    }

    fn lookup_trigram(&self, t: AstTrigram) -> crate::Result<Vec<AstPosting>> {
        Ok(self.trigrams.get(&t.key()).cloned().unwrap_or_default())
    }

    fn file_meta(&self, doc_id: u32) -> crate::Result<AstFileMetaEntry> {
        self.file_metas.get(&doc_id).copied().ok_or_else(|| {
            crate::SearchError::IndexCorrupted(format!("FakePostingSource: no meta for {doc_id}"))
        })
    }

    fn avg_node_count(&self) -> f32 {
        self.avg_node_count
    }

    fn file_count(&self) -> u32 {
        self.file_count
    }
}

// ============================================================================
// Test helpers
// ============================================================================

fn make_bigram_set(ngram: AstBigram, count: u32) -> AstNgramSet {
    AstNgramSet {
        bigrams: vec![AstBigramEntry {
            ngram,
            weight: DEFAULT_AST_WEIGHT,
            count,
        }],
        trigrams: vec![],
    }
}

fn make_trigram_set(ngram: AstTrigram, count: u32) -> AstNgramSet {
    AstNgramSet {
        bigrams: vec![],
        trigrams: vec![AstTrigramEntry {
            ngram,
            weight: DEFAULT_AST_WEIGHT,
            count,
        }],
    }
}

// ============================================================================
// GROUP 1: Parser tests (no I/O)
// ============================================================================

// --- Named pattern ---

#[test]
fn parse_named_pattern_returns_pattern_variant() {
    let q = parse_ast_query("try-catch").unwrap();
    let expected_pattern = lookup_pattern("try-catch").unwrap();
    assert!(matches!(q, AstQuery::Pattern(p) if std::ptr::eq(p, expected_pattern)));
}

#[test]
fn parse_named_pattern_trimmed() {
    let q = parse_ast_query("  try-catch  ").unwrap();
    assert!(matches!(q, AstQuery::Pattern(_)));
}

#[test]
fn parse_unknown_pattern_returns_error_with_list() {
    let err = parse_ast_query("no-such-pattern").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("no-such-pattern"), "msg: {msg}");
    // lookup_pattern lists all patterns in its error
    assert!(
        msg.contains("try-catch"),
        "should list valid patterns: {msg}"
    );
}

// --- Containment bigram ---

#[test]
fn parse_bigram_containment() {
    let for_id = vocab_lookup("for_statement").unwrap();
    let await_id = vocab_lookup("await_expression").unwrap();
    let expected_bigram = AstBigram::encode(for_id, await_id);

    let q = parse_ast_query("for_statement > await_expression").unwrap();
    match q {
        AstQuery::Containment(set) => {
            assert_eq!(set.bigrams.len(), 1);
            assert_eq!(set.trigrams.len(), 0);
            assert_eq!(set.bigrams[0].ngram, expected_bigram);
        }
        _ => unreachable!("expected Containment bigram"),
    }
}

#[test]
fn parse_bigram_whitespace_normalized() {
    let q1 = parse_ast_query("for_statement > await_expression").unwrap();
    let q2 = parse_ast_query("  for_statement  >  await_expression  ").unwrap();
    assert_eq!(q1, q2);
}

// --- Containment trigram ---

#[test]
fn parse_trigram_containment() {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let expr_id = vocab_lookup("expression_statement").unwrap();
    let expected_trigram = AstTrigram::encode(fn_id, block_id, expr_id);

    let q = parse_ast_query("function_item > block > expression_statement").unwrap();
    match q {
        AstQuery::Containment(set) => {
            assert_eq!(set.bigrams.len(), 0);
            assert_eq!(set.trigrams.len(), 1);
            assert_eq!(set.trigrams[0].ngram, expected_trigram);
        }
        _ => unreachable!("expected Containment trigram"),
    }
}

// --- Single node ---

#[test]
fn parse_single_node_valid_kind() {
    let try_id = vocab_lookup("try_statement").unwrap();
    let q = parse_ast_query("try_statement").unwrap();
    assert_eq!(q, AstQuery::SingleNode(try_id));
}

// --- Error cases ---

#[test]
fn parse_empty_string_invalid() {
    let err = parse_ast_query("").unwrap_err();
    assert!(err.to_string().contains("empty"), "err: {err}");
}

#[test]
fn parse_whitespace_only_invalid() {
    let err = parse_ast_query("   ").unwrap_err();
    assert!(err.to_string().contains("empty"), "err: {err}");
}

#[test]
fn parse_transitive_operator_invalid() {
    let err = parse_ast_query("a >> b").unwrap_err();
    assert!(err.to_string().contains(">>"), "err: {err}");
}

#[test]
fn parse_depth_gt2_invalid() {
    // A > B > C > D — 4 segments
    let err =
        parse_ast_query("function_item > block > expression_statement > identifier").unwrap_err();
    assert!(
        err.to_string().contains("depth") || err.to_string().contains("segment"),
        "err: {err}"
    );
}

#[test]
fn parse_trailing_gt_invalid() {
    let err = parse_ast_query("function_item >").unwrap_err();
    assert!(
        err.to_string().contains("empty segment") || err.to_string().contains("empty"),
        "err: {err}"
    );
}

#[test]
fn parse_leading_gt_invalid() {
    let err = parse_ast_query("> function_item").unwrap_err();
    assert!(
        err.to_string().contains("empty segment") || err.to_string().contains("empty"),
        "err: {err}"
    );
}

#[test]
fn parse_double_gt_invalid() {
    // a > > b (not >> but two separate >, with empty middle segment)
    let err = parse_ast_query("function_item > > block").unwrap_err();
    assert!(err.to_string().contains("empty"), "err: {err}");
}

#[test]
fn parse_unknown_kind_in_containment_invalid() {
    let err = parse_ast_query("not_a_kind > block").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("not_a_kind"), "err: {msg}");
}

#[test]
fn parse_unknown_single_kind_invalid() {
    // Non-hyphenated unknown name → InvalidQuery
    let err = parse_ast_query("totally_unknown_node_xyz").unwrap_err();
    assert!(
        err.to_string().contains("totally_unknown_node_xyz"),
        "err: {err}"
    );
}

#[test]
fn parse_query_too_long_invalid() {
    let long = "a".repeat(4097);
    let err = parse_ast_query(&long).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("4097") || msg.contains("too long"),
        "err: {msg}"
    );
}

#[test]
fn parse_exactly_4096_bytes_ok() {
    // Build a valid containment query (bigram "function_item > block") padded to
    // exactly 4096 bytes with leading spaces — trimming removes them, so the
    // length guard passes and the query is valid.
    let base = "function_item > block";
    let padding = " ".repeat(4096 - base.len());
    let s = format!("{padding}{base}");
    assert_eq!(s.len(), 4096, "precondition: string is exactly 4096 bytes");
    // Should succeed (valid query within length limit).
    let result = parse_ast_query(&s);
    assert!(
        result.is_ok(),
        "4096-byte valid query should be accepted: {result:?}"
    );
    // Result should be a Containment bigram.
    assert!(
        matches!(result.unwrap(), AstQuery::Containment(_)),
        "expected Containment bigram"
    );
}

// ============================================================================
// GROUP 2: Execution/scoring — FakePostingSource
// ============================================================================

// --- A1: named hit score > 0 ---

#[test]
fn a1_named_pattern_returns_positive_score() {
    // try-catch has bigram: try_statement → catch_clause
    let try_id = vocab_lookup("try_statement").unwrap();
    let catch_id = vocab_lookup("catch_clause").unwrap();
    let bigram = AstBigram::encode(try_id, catch_id);

    let source = FakePostingSource::default()
        .with_file(0, Language::TypeScript, 100)
        .with_bigram(
            bigram.key(),
            vec![AstPosting {
                doc_id: 0,
                count: 2,
            }],
        )
        .with_avg_node_count(100.0);

    let engine = AstQueryEngine::new(source);
    let q = parse_ast_query("try-catch").unwrap();
    let results = engine.search_ast(&q).unwrap();

    assert!(!results.is_empty(), "expected at least one result");
    assert!(results[0].1 > 0.0, "score must be positive");
}

// --- A2: depth filter — bigram matches, trigram is empty for adjacency-only ---

#[test]
fn a2_bigram_matches_adjacency_only_file() {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let expr_id = vocab_lookup("expression_statement").unwrap();

    let bigram = AstBigram::encode(fn_id, block_id);
    let trigram = AstTrigram::encode(fn_id, block_id, expr_id);

    // File 0: has the bigram but NOT the trigram.
    let source = FakePostingSource::default()
        .with_file(0, Language::Rust, 50)
        .with_bigram(
            bigram.key(),
            vec![AstPosting {
                doc_id: 0,
                count: 1,
            }],
        )
        // trigram posting list is empty (absent key)
        .with_avg_node_count(50.0);

    let engine = AstQueryEngine::new(source);

    // Bigram query: should return file 0.
    let bigram_q = AstQuery::Containment(make_bigram_set(bigram, 1));
    let bigram_results = engine.search_ast(&bigram_q).unwrap();
    assert_eq!(bigram_results.len(), 1);
    assert_eq!(bigram_results[0].0, FileId(0));

    // Trigram query: should return empty (depth filter).
    let trigram_q = AstQuery::Containment(make_trigram_set(trigram, 1));
    let trigram_results = engine.search_ast(&trigram_q).unwrap();
    assert!(
        trigram_results.is_empty(),
        "trigram should be absent for adjacency-only file"
    );
}

// --- A3: AND-intersect — multi-n-gram query keeps only files in EVERY posting list ---
//
// AD-374-1: `search_ast` uses AND-intersect (run_ngram_set with no mode param).
//
// Fixture: bigram1 appears in files 0+1; bigram2 appears only in file 0.
// AND-intersect result: ONLY file 0 (appears in both lists).
// This is a deliberate semantic change from the old OR-union behavior.
//
// The OR-union BM25F scoring property ("file with both n-grams scores higher")
// is preserved at the `score_ngram_set` layer and tested via A3b below.

#[test]
fn a3_union_ranking_both_clauses_scores_higher() {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let expr_id = vocab_lookup("expression_statement").unwrap();

    let bigram1 = AstBigram::encode(fn_id, block_id);
    let bigram2 = AstBigram::encode(block_id, expr_id);

    // File 0 has both bigrams; file 1 has only bigram1.
    let source = FakePostingSource::default()
        .with_file(0, Language::Rust, 50)
        .with_file(1, Language::Rust, 50)
        .with_bigram(
            bigram1.key(),
            vec![
                AstPosting {
                    doc_id: 0,
                    count: 2,
                },
                AstPosting {
                    doc_id: 1,
                    count: 2,
                },
            ],
        )
        .with_bigram(
            bigram2.key(),
            vec![AstPosting {
                doc_id: 0,
                count: 1,
            }],
        )
        .with_avg_node_count(50.0);

    let engine = AstQueryEngine::new(source);

    // Query: both bigrams (multi-n-gram set)
    let set = AstNgramSet {
        bigrams: {
            let mut v = vec![
                AstBigramEntry {
                    ngram: bigram1,
                    weight: DEFAULT_AST_WEIGHT,
                    count: 1,
                },
                AstBigramEntry {
                    ngram: bigram2,
                    weight: DEFAULT_AST_WEIGHT,
                    count: 1,
                },
            ];
            v.sort_unstable_by_key(|e| e.ngram.key());
            v
        },
        trigrams: vec![],
    };
    let q = AstQuery::Containment(set);
    let results = engine.search_ast(&q).unwrap();

    // AD-374-1 (AND-intersect): file 1 is NOT in bigram2's posting list →
    // AND-intersect removes it. Only file 0 (in BOTH lists) survives.
    // Pre-#374 (OR-union) this was 2; now it is correctly 1.
    assert_eq!(
        results.len(),
        1,
        "AND-intersect: only file 0 (in both posting lists) should survive; \
         file 1 (bigram1 only) must be excluded. Got {} results: {results:?}",
        results.len()
    );
    assert_eq!(
        results[0].0,
        FileId(0),
        "AND-intersect: the surviving file must be FileId(0)"
    );
}

/// A3b — OR-union scoring: a file matching both n-grams scores higher than one
/// matching only one when evaluated over the BM25F layer (OR-union path).
///
/// `run_ngram_set_with_capacity` preserves OR-union semantics for the P3 capacity
/// tests and for this scoring-property test.  The production `search_ast` path uses
/// AND-intersect (AD-374-1); this test isolates the scoring layer.
///
/// Renamed from the original A3 test body; the discriminating property (PF-007) is:
/// reverting `score_ngram_set` to ignore multi-hit files makes `score0 ≤ score1`.
#[test]
fn a3b_or_union_scoring_both_clauses_scores_higher() {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let expr_id = vocab_lookup("expression_statement").unwrap();

    let bigram1 = AstBigram::encode(fn_id, block_id);
    let bigram2 = AstBigram::encode(block_id, expr_id);

    let source = FakePostingSource::default()
        .with_file(0, Language::Rust, 50)
        .with_file(1, Language::Rust, 50)
        .with_bigram(
            bigram1.key(),
            vec![
                AstPosting { doc_id: 0, count: 2 },
                AstPosting { doc_id: 1, count: 2 },
            ],
        )
        .with_bigram(
            bigram2.key(),
            vec![AstPosting { doc_id: 0, count: 1 }],
        )
        .with_avg_node_count(50.0);

    let engine = AstQueryEngine::new(source);

    let set = AstNgramSet {
        bigrams: {
            let mut v = vec![
                AstBigramEntry { ngram: bigram1, weight: DEFAULT_AST_WEIGHT, count: 1 },
                AstBigramEntry { ngram: bigram2, weight: DEFAULT_AST_WEIGHT, count: 1 },
            ];
            v.sort_unstable_by_key(|e| e.ngram.key());
            v
        },
        trigrams: vec![],
    };

    // OR-union path: both files appear (no AND-intersect filter).
    let (results, _cap) = engine.run_ngram_set_with_capacity(&set, None).unwrap();
    assert_eq!(
        results.len(),
        2,
        "A3b OR-union: both files must appear in the scoring layer (before AND-intersect)"
    );

    let score0 = results.iter().find(|(f, _)| *f == FileId(0)).unwrap().1;
    let score1 = results.iter().find(|(f, _)| *f == FileId(1)).unwrap().1;
    assert!(
        score0 > score1,
        "A3b OR-union: file 0 (both n-grams) must score higher than file 1 (bigram1 only): \
         {score0} vs {score1}"
    );
}

// --- A4: per-lang IDF + length-norm ---

#[test]
fn a4_larger_node_count_lower_score() {
    // Same bigram, same TF, different node_count → larger file gets lower score.
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    let source = FakePostingSource::default()
        .with_file(0, Language::Rust, 50) // small file
        .with_file(1, Language::Rust, 200) // large file
        .with_bigram(
            bigram.key(),
            vec![
                AstPosting {
                    doc_id: 0,
                    count: 1,
                },
                AstPosting {
                    doc_id: 1,
                    count: 1,
                },
            ],
        )
        .with_avg_node_count(100.0); // avg is between small and large

    let engine = AstQueryEngine::new(source);
    let q = AstQuery::Containment(make_bigram_set(bigram, 1));
    let results = engine.search_ast(&q).unwrap();

    assert_eq!(results.len(), 2);
    let score0 = results.iter().find(|(f, _)| *f == FileId(0)).unwrap().1;
    let score1 = results.iter().find(|(f, _)| *f == FileId(1)).unwrap().1;
    assert!(
        score0 > score1,
        "smaller file should score higher: {score0} vs {score1}"
    );
}

#[test]
fn a4_unknown_lang_uses_1_0_idf_fallback() {
    // A file with an unrecognized lang_id (255) should still be scored with IDF=1.0.
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    // Use an unrecognized lang_id for file 0.
    let mut source = FakePostingSource::default().with_avg_node_count(100.0);
    source.file_metas.insert(
        0,
        AstFileMetaEntry {
            lang_id: 255, // unrecognized
            node_count: 100,
            max_depth: 3,
            max_block_stmts: 5,
            max_params: 2,
            branch_count: 1,
        },
    );
    source.file_count = 1;
    source.bigrams.insert(
        bigram.key(),
        vec![AstPosting {
            doc_id: 0,
            count: 1,
        }],
    );

    let engine = AstQueryEngine::new(source);
    let q = AstQuery::Containment(make_bigram_set(bigram, 1));
    let results = engine.search_ast(&q).unwrap();

    // File should still appear with a positive score (IDF=1.0 fallback).
    assert_eq!(results.len(), 1);
    assert!(
        results[0].1 > 0.0,
        "unknown-lang file should have positive score"
    );
}

// --- A5: synthetic-marker patterns match with IDF 1.0 ---

#[test]
fn a5_synthetic_marker_pattern_matches() {
    use crate::ast_index::structural::{DEEP_NODE, bucket_label};

    let bigram = AstBigram::encode(DEEP_NODE, bucket_label(0));

    let source = FakePostingSource::default()
        .with_file(0, Language::Rust, 80)
        .with_bigram(
            bigram.key(),
            vec![AstPosting {
                doc_id: 0,
                count: 3,
            }],
        )
        .with_avg_node_count(80.0);

    let engine = AstQueryEngine::new(source);
    let set = AstNgramSet {
        bigrams: vec![AstBigramEntry {
            ngram: bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        }],
        trigrams: vec![],
    };
    let q = AstQuery::Containment(set);
    let results = engine.search_ast(&q).unwrap();

    assert_eq!(results.len(), 1, "synthetic marker file should match");

    // A5 exact score: synthetic keys are absent from the weight table, so
    // ast_bigram_idf falls back to DEFAULT_AST_WEIGHT = 1.0.
    // Setup: node_count=80, avg=80.0 → nc == avg → length_norm = 1.0
    //   tf_norm = count / length_norm = 3.0 / 1.0 = 3.0
    //   score = idf * (tf_norm / (tf_norm + k1)) = 1.0 * (3.0 / (3.0 + 1.2))
    let k1 = AST_BM25_K1;
    let tf_norm = 3.0_f64; // count=3, length_norm=1.0
    let expected = tf_norm / (tf_norm + k1); // idf=1.0
    let got = results[0].1;
    assert!(
        (got - expected).abs() < 1e-9,
        "A5 synthetic score mismatch: got {got}, expected {expected} (idf=1.0, tf_norm={tf_norm})"
    );
}

// --- Golden BM25 with non-trivial length_norm (nc != avg) ---

#[test]
fn bm25_golden_value_non_trivial_length_norm() {
    // Verifies the full BM25 formula with nc != avg so length_norm != 1.0.
    //
    // Setup: node_count=200, avg=100, count=1, Language::Rust.
    //   length_norm = 1 - 0.75 + 0.75 * (200/100) = 0.25 + 1.5 = 1.75
    //   tf_norm = 1.0 / 1.75
    //   idf = ast_bigram_idf(Rust, bigram)
    //   expected = idf * (tf_norm / (tf_norm + 1.2))
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    let source = FakePostingSource::default()
        .with_file(0, Language::Rust, 200)
        .with_bigram(
            bigram.key(),
            vec![AstPosting {
                doc_id: 0,
                count: 1,
            }],
        )
        .with_avg_node_count(100.0);

    let engine = AstQueryEngine::new(source);
    let q = AstQuery::Containment(make_bigram_set(bigram, 1));
    let results = engine.search_ast(&q).unwrap();

    assert_eq!(results.len(), 1);

    use crate::ast_index::ast_bigram_idf;
    let idf = f64::from(ast_bigram_idf(Language::Rust, bigram));
    let avg = 100.0_f64;
    let nc = 200.0_f64;
    let k1 = AST_BM25_K1;
    let b = AST_BM25_B;
    let length_norm = 1.0 - b + b * (nc / avg); // = 1.75
    let tf_norm = 1.0_f64 / length_norm;
    let expected = idf * (tf_norm / (tf_norm + k1));

    let got = results[0].1;
    assert!(
        (got - expected).abs() < 1e-9,
        "golden BM25 mismatch: got {got}, expected {expected} (length_norm={length_norm})"
    );
}

// --- avg_node_count == 0.0 path: length_norm → 1.0 ---

#[test]
fn bm25_avg_zero_length_norm_fallback_positive_score() {
    // When avg_node_count == 0.0, the bm25 function takes the length_norm=1.0
    // branch. The result must still be a positive score.
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    // avg = 0.0 triggers the defensive branch.
    let source = FakePostingSource::default()
        .with_file(0, Language::Rust, 100)
        .with_bigram(
            bigram.key(),
            vec![AstPosting {
                doc_id: 0,
                count: 1,
            }],
        )
        .with_avg_node_count(0.0);

    let engine = AstQueryEngine::new(source);
    let q = AstQuery::Containment(make_bigram_set(bigram, 1));
    let results = engine.search_ast(&q).unwrap();

    assert_eq!(results.len(), 1, "should return the file even with avg=0");
    assert!(
        results[0].1 > 0.0,
        "score must be positive with avg=0 (length_norm=1.0 fallback)"
    );
}

// --- Double-counting PINNED test ---

#[test]
fn double_counting_pinned_bigram_plus_trigram() {
    // A file matching both a bigram and its containing trigram gets
    // contributions from BOTH (double-counting tolerated in Wave 3f).
    // This test pins the current additive behavior.
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let expr_id = vocab_lookup("expression_statement").unwrap();

    let bigram = AstBigram::encode(fn_id, block_id);
    let trigram = AstTrigram::encode(fn_id, block_id, expr_id);

    let node_count: u32 = 100;
    let avg: f32 = 100.0;

    let source = FakePostingSource::default()
        .with_file(0, Language::Rust, node_count)
        .with_bigram(
            bigram.key(),
            vec![AstPosting {
                doc_id: 0,
                count: 2,
            }],
        )
        .with_trigram(
            trigram.key(),
            vec![AstPosting {
                doc_id: 0,
                count: 1,
            }],
        )
        .with_avg_node_count(avg);

    let engine = AstQueryEngine::new(source);

    // Build a set with both bigram + trigram.
    let set = AstNgramSet {
        bigrams: vec![AstBigramEntry {
            ngram: bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        }],
        trigrams: vec![AstTrigramEntry {
            ngram: trigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        }],
    };
    let q = AstQuery::Containment(set);
    let results = engine.search_ast(&q).unwrap();

    assert_eq!(results.len(), 1);

    // Manually compute expected score for pinning.
    // length_norm = 1 - 0.75 + 0.75 * (100/100) = 1.0
    // IDF for bigram in Rust: ast_bigram_idf(Rust, bigram) as f64
    // IDF for trigram in Rust: ast_trigram_idf(Rust, trigram) as f64
    use crate::ast_index::{ast_bigram_idf, ast_trigram_idf};
    let idf_b = f64::from(ast_bigram_idf(Language::Rust, bigram));
    let idf_t = f64::from(ast_trigram_idf(Language::Rust, trigram));
    let k1 = AST_BM25_K1;
    // tf_norm for bigram (count=2, length_norm=1.0): 2.0 / 1.0 = 2.0
    let tf_norm_b = 2.0_f64 / 1.0;
    let contrib_b = idf_b * (tf_norm_b / (tf_norm_b + k1));
    // tf_norm for trigram (count=1, length_norm=1.0): 1.0
    let tf_norm_t = 1.0_f64;
    let contrib_t = idf_t * (tf_norm_t / (tf_norm_t + k1));
    let expected = contrib_b + contrib_t;

    let got = results[0].1;
    assert!(
        (got - expected).abs() < 1e-9,
        "double-count golden score mismatch: got {got}, expected {expected}"
    );
}

// --- A6: SingleNode execute → InvalidQuery with "unigram" and "#283" ---

#[test]
fn a6_single_node_execution_defers_to_283() {
    let try_id = vocab_lookup("try_statement").unwrap();
    let source = FakePostingSource::default().with_avg_node_count(0.0);
    let engine = AstQueryEngine::new(source);

    let q = AstQuery::SingleNode(try_id);
    let err = engine.search_ast(&q).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("unigram"),
        "error must mention 'unigram': {msg}"
    );
    assert!(msg.contains("#283"), "error must reference '#283': {msg}");
}

// --- B2: postcondition — FileId-asc, unique, all scores > 0 ---

#[test]
fn b2_results_sorted_file_id_asc_unique() {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    // 5 files, all matching the bigram.
    let postings: Vec<AstPosting> = (0..5u32)
        .map(|i| AstPosting {
            doc_id: i,
            count: 1,
        })
        .collect();

    let mut source = FakePostingSource::default().with_avg_node_count(100.0);
    for i in 0..5u32 {
        source = source.with_file(i, Language::Rust, 100);
    }
    source.bigrams.insert(bigram.key(), postings);

    let engine = AstQueryEngine::new(source);
    let q = AstQuery::Containment(make_bigram_set(bigram, 1));
    let results = engine.search_ast(&q).unwrap();

    assert_eq!(results.len(), 5);

    // Sorted ascending by FileId.
    for i in 1..results.len() {
        assert!(
            results[i - 1].0 < results[i].0,
            "results not sorted: {:?} before {:?}",
            results[i - 1].0,
            results[i].0
        );
    }

    // All scores > 0.
    for (fid, score) in &results {
        assert!(*score > 0.0, "score for {fid} must be positive: {score}");
    }

    // Unique FileIds (no duplicates).
    let ids: HashSet<FileId> = results.iter().map(|(f, _)| *f).collect();
    assert_eq!(ids.len(), 5, "duplicate FileIds in result");
}

// --- B3: absent key → Ok(vec![]), empty index → Ok(vec[]) ---

#[test]
fn b3_absent_key_returns_empty() {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    let source = FakePostingSource::default()
        .with_file(0, Language::Rust, 100)
        .with_avg_node_count(100.0);
    // No bigram posting inserted → absent key.

    let engine = AstQueryEngine::new(source);
    let q = AstQuery::Containment(make_bigram_set(bigram, 1));
    let results = engine.search_ast(&q).unwrap();
    assert!(results.is_empty(), "absent key should return empty results");
}

#[test]
fn b3_empty_index_returns_empty() {
    let source = FakePostingSource::default(); // no files, no postings
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    let engine = AstQueryEngine::new(source);
    let q = AstQuery::Containment(make_bigram_set(bigram, 1));
    let results = engine.search_ast(&q).unwrap();
    assert!(results.is_empty());
}

#[test]
fn b3_corrupt_doc_id_propagated() {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    // Posting references doc_id=99 but no meta for it.
    let source = FakePostingSource::default()
        .with_avg_node_count(100.0)
        .with_bigram(
            bigram.key(),
            vec![AstPosting {
                doc_id: 99,
                count: 1,
            }],
        );

    let engine = AstQueryEngine::new(source);
    let q = AstQuery::Containment(make_bigram_set(bigram, 1));
    let err = engine.search_ast(&q).unwrap_err();
    // Should propagate as IndexCorrupted (from FakePostingSource.file_meta)
    assert!(
        matches!(err, crate::SearchError::IndexCorrupted(_)),
        "expected IndexCorrupted, got: {err:?}"
    );
}

// --- Performance unit tripwire (CI-safe, gated behind --ignored for flake safety) ---

#[test]
#[ignore = "wall-clock assertion: run explicitly or rely on Criterion bench (ast_query.rs) for the real perf signal"]
fn perf_tripwire_10k_postings_under_1s() {
    use std::time::Instant;

    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    // 10k postings (one per file).
    let postings: Vec<AstPosting> = (0..10_000u32)
        .map(|i| AstPosting {
            doc_id: i,
            count: 1 + (i % 5),
        })
        .collect();

    let mut source = FakePostingSource::default().with_avg_node_count(100.0);
    for i in 0..10_000u32 {
        source = source.with_file(i, Language::Rust, 100);
    }
    source.bigrams.insert(bigram.key(), postings);

    let engine = AstQueryEngine::new(source);
    let q = AstQuery::Containment(make_bigram_set(bigram, 1));

    let start = Instant::now();
    let results = engine.search_ast(&q).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(results.len(), 10_000);
    assert!(
        elapsed.as_secs() < 1,
        "10k posting query took too long: {elapsed:?}"
    );
}

// --- Gap-fix #6: duplicate query n-gram key must score exactly once ---

#[test]
fn gap6_duplicate_query_ngram_key_scores_once() {
    // Construct an AstNgramSet with two AstBigramEntry items sharing the same key.
    // run_ngram_set must dedup before lookup so the key is looked up once, not twice.
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    // Build a source that returns a known posting for the bigram key.
    let source = FakePostingSource::default()
        .with_file(0, Language::Rust, 100)
        .with_bigram(
            bigram.key(),
            vec![AstPosting {
                doc_id: 0,
                count: 2,
            }],
        )
        .with_avg_node_count(100.0);

    let engine = AstQueryEngine::new(source);

    // Single-entry set → baseline score.
    let single = AstQuery::Containment(make_bigram_set(bigram, 1));
    let single_results = engine.search_ast(&single).unwrap();
    assert_eq!(single_results.len(), 1);
    let baseline_score = single_results[0].1;

    // Duplicate-entry set (same key, same ngram, sorted): should produce same score.
    let dup_set = AstNgramSet {
        bigrams: vec![
            AstBigramEntry {
                ngram: bigram,
                weight: DEFAULT_AST_WEIGHT,
                count: 1,
            },
            AstBigramEntry {
                ngram: bigram,
                weight: DEFAULT_AST_WEIGHT,
                count: 1,
            },
        ],
        trigrams: vec![],
    };
    let dup_results = engine.search_ast(&AstQuery::Containment(dup_set)).unwrap();
    assert_eq!(dup_results.len(), 1);
    let dup_score = dup_results[0].1;

    assert!(
        (dup_score - baseline_score).abs() < 1e-12,
        "duplicate key must score once: baseline={baseline_score}, dup={dup_score}"
    );
}

// ============================================================================
// GROUP 3: SearchLayer adapter — real AstIndexBuilder/AstIndexReader
// ============================================================================

fn build_small_index() -> (tempfile::TempDir, AstIndexReader) {
    let dir = tempdir().unwrap();

    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    // File 0: Rust, has bigram, count=3
    let set0 = AstNgramSet {
        bigrams: vec![AstBigramEntry {
            ngram: bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 3,
        }],
        trigrams: vec![],
    };
    builder
        .add_file_ngrams(
            FileId(0),
            Language::Rust,
            &set0,
            100,
            StructuralMetrics::default(),
        )
        .unwrap();

    // File 1: Python, has bigram, count=1
    let set1 = AstNgramSet {
        bigrams: vec![AstBigramEntry {
            ngram: bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        }],
        trigrams: vec![],
    };
    builder
        .add_file_ngrams(
            FileId(1),
            Language::Python,
            &set1,
            80,
            StructuralMetrics::default(),
        )
        .unwrap();

    let reader = builder.build().unwrap();
    (dir, reader)
}

// B4: ast_pattern = None → Ok(vec![])

#[test]
fn b4_none_ast_pattern_returns_empty() {
    let (dir, reader) = build_small_index();
    let engine = AstQueryEngine::new(reader);

    let query = SearchQuery::new("some text"); // ast_pattern = None
    let results = engine.search(&query).unwrap();
    assert!(results.is_empty(), "None ast_pattern should return empty");
    drop(dir);
}

// B4: Some("") → InvalidQuery

#[test]
fn b4_empty_string_ast_pattern_invalid() {
    let (dir, reader) = build_small_index();
    let engine = AstQueryEngine::new(reader);

    let mut query = SearchQuery::new("text");
    query.ast_pattern = Some("".into());
    let err = engine.search(&query).unwrap_err();
    assert!(err.to_string().contains("empty"), "err: {err}");
    drop(dir);
}

// B4: Some("try-catch") → results, score-desc

#[test]
fn b4_named_pattern_search_returns_results() {
    // Build an index with the try-catch bigram.
    let dir = tempdir().unwrap();
    let try_id = vocab_lookup("try_statement").unwrap();
    let catch_id = vocab_lookup("catch_clause").unwrap();
    let bigram = AstBigram::encode(try_id, catch_id);

    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let set = AstNgramSet {
        bigrams: vec![AstBigramEntry {
            ngram: bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 2,
        }],
        trigrams: vec![],
    };
    builder
        .add_file_ngrams(
            FileId(0),
            Language::TypeScript,
            &set,
            100,
            StructuralMetrics::default(),
        )
        .unwrap();

    let reader = builder.build().unwrap();
    let engine = AstQueryEngine::new(reader);

    let mut query = SearchQuery::new("query text");
    query.ast_pattern = Some("try-catch".into());
    let results = engine.search(&query).unwrap();

    assert!(!results.is_empty(), "should return results for try-catch");
    assert_eq!(results[0].file_id, FileId(0));
    drop(dir);
}

// B4: limit / offset honored — assert exact FileId identities in score-DESC order

#[test]
fn b4_limit_and_offset_honored() {
    let dir = tempdir().unwrap();
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    // Files 0–4 with counts 5,4,3,2,1 → scores strictly decreasing by FileId.
    // score-DESC order is FileId(0), FileId(1), FileId(2), FileId(3), FileId(4).
    for i in 0..5u32 {
        let set = AstNgramSet {
            bigrams: vec![AstBigramEntry {
                ngram: bigram,
                weight: DEFAULT_AST_WEIGHT,
                count: 5 - i, // different counts → different scores
            }],
            trigrams: vec![],
        };
        builder
            .add_file_ngrams(
                FileId(i),
                Language::Rust,
                &set,
                100,
                StructuralMetrics::default(),
            )
            .unwrap();
    }
    let reader = builder.build().unwrap();
    let engine = AstQueryEngine::new(reader);

    let mut query = SearchQuery::new("q");
    query.ast_pattern = Some("function_item > block".into());
    query.limit = Some(2);
    query.offset = Some(1);
    let results = engine.search(&query).unwrap();

    // offset=1, limit=2 → skip the top-scored file (FileId(0)), take FileId(1) and FileId(2).
    assert_eq!(results.len(), 2, "limit=2 should yield 2 results");
    let ids: Vec<FileId> = results.iter().map(|r| r.file_id).collect();
    assert_eq!(
        ids,
        vec![FileId(1), FileId(2)],
        "offset=1,limit=2 should yield [FileId(1), FileId(2)] in score-DESC order: got {ids:?}"
    );

    // Sibling: offset past the end → empty result.
    let mut query_past = SearchQuery::new("q");
    query_past.ast_pattern = Some("function_item > block".into());
    query_past.limit = Some(2);
    query_past.offset = Some(10);
    let past_results = engine.search(&query_past).unwrap();
    assert!(
        past_results.is_empty(),
        "offset=10 past end should return empty"
    );

    drop(dir);
}

// B4: lang filter restricts to one language

#[test]
fn b4_lang_filter_restricts_results() {
    let (dir, reader) = build_small_index();
    let engine = AstQueryEngine::new(reader);

    let mut query = SearchQuery::new("q");
    query.ast_pattern = Some("function_item > block".into());
    query.lang = Some(Language::Python);
    let results = engine.search(&query).unwrap();

    // Only file 1 is Python.
    assert!(
        results.iter().all(|r| r.file_id == FileId(1)),
        "lang filter should restrict to Python file (FileId 1): {results:?}"
    );
    drop(dir);
}

// B4: file_filter allowlist restricts FileIds

#[test]
fn b4_file_filter_restricts_file_ids() {
    let (dir, reader) = build_small_index();
    let engine = AstQueryEngine::new(reader);

    let mut query = SearchQuery::new("q");
    query.ast_pattern = Some("function_item > block".into());
    query.file_filter = Some(HashSet::from([FileId(0)]));
    let results = engine.search(&query).unwrap();

    assert!(
        results.iter().all(|r| r.file_id == FileId(0)),
        "file_filter should restrict to FileId(0): {results:?}"
    );
    drop(dir);
}

// B4: results sorted score-DESC, FileId-ASC tie-break; name()=="ast"

#[test]
fn b4_results_sorted_score_desc() {
    let dir = tempdir().unwrap();
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    // File 0: low count, high node_count → lower score
    let set0 = AstNgramSet {
        bigrams: vec![AstBigramEntry {
            ngram: bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        }],
        trigrams: vec![],
    };
    builder
        .add_file_ngrams(
            FileId(0),
            Language::Rust,
            &set0,
            500,
            StructuralMetrics::default(),
        )
        .unwrap();
    // File 1: higher count, low node_count → higher score
    let set1 = AstNgramSet {
        bigrams: vec![AstBigramEntry {
            ngram: bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 5,
        }],
        trigrams: vec![],
    };
    builder
        .add_file_ngrams(
            FileId(1),
            Language::Rust,
            &set1,
            50,
            StructuralMetrics::default(),
        )
        .unwrap();
    let reader = builder.build().unwrap();
    let engine = AstQueryEngine::new(reader);

    let mut query = SearchQuery::new("q");
    query.ast_pattern = Some("function_item > block".into());
    let results = engine.search(&query).unwrap();

    assert_eq!(results.len(), 2);
    // First result should be FileId(1) (higher score).
    assert_eq!(
        results[0].file_id,
        FileId(1),
        "higher-score file should be first"
    );
    drop(dir);
}

#[test]
fn b4_name_is_ast() {
    let (dir, reader) = build_small_index();
    let engine = AstQueryEngine::new(reader);
    assert_eq!(engine.name(), "ast");
    drop(dir);
}

// B6: result fields — line_range==0..0, match_positions==[], field==Other, snippet==None

#[test]
fn b6_result_fields_are_honest_defaults() {
    let dir = tempdir().unwrap();
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let set = AstNgramSet {
        bigrams: vec![AstBigramEntry {
            ngram: bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        }],
        trigrams: vec![],
    };
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
    let engine = AstQueryEngine::new(reader);

    let mut query = SearchQuery::new("q");
    query.ast_pattern = Some("function_item > block".into());
    let results = engine.search(&query).unwrap();

    assert!(!results.is_empty());
    let r = &results[0];
    assert_eq!(r.line_range, 0..0, "line_range should be 0..0");
    assert!(
        r.match_positions.is_empty(),
        "match_positions should be empty"
    );
    assert_eq!(
        r.field,
        crate::types::SearchField::Other,
        "field should be Other"
    );
    assert!(r.snippet.is_none(), "snippet should be None");
    drop(dir);
}

// B4: equal-score tie-break — identical inputs produce bitwise-equal scores, FileId-ASC wins

#[test]
fn b4_equal_scores_tie_break_file_id_asc() {
    let dir = tempdir().unwrap();
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    // Two files with identical inputs → identical BM25 scores.
    // Same language, node_count, posting count → same length_norm, tf_norm, idf.
    for i in 0..2u32 {
        let set = AstNgramSet {
            bigrams: vec![AstBigramEntry {
                ngram: bigram,
                weight: DEFAULT_AST_WEIGHT,
                count: 1,
            }],
            trigrams: vec![],
        };
        builder
            .add_file_ngrams(
                FileId(i),
                Language::Rust,
                &set,
                100,
                StructuralMetrics::default(),
            )
            .unwrap();
    }
    let reader = builder.build().unwrap();
    let engine = AstQueryEngine::new(reader);

    let mut query = SearchQuery::new("q");
    query.ast_pattern = Some("function_item > block".into());
    let results = engine.search(&query).unwrap();

    assert_eq!(results.len(), 2, "both files should match");

    // Scores must be bitwise-equal (same inputs, deterministic formula).
    let score0 = results
        .iter()
        .find(|r| r.file_id == FileId(0))
        .unwrap()
        .score;
    let score1 = results
        .iter()
        .find(|r| r.file_id == FileId(1))
        .unwrap()
        .score;
    assert_eq!(
        score0, score1,
        "precondition: both files must have equal scores (score0={score0}, score1={score1})"
    );

    // When scores tie, FileId-ASC tie-break must apply: FileId(0) before FileId(1).
    let ids: Vec<FileId> = results.iter().map(|r| r.file_id).collect();
    assert_eq!(
        ids,
        vec![FileId(0), FileId(1)],
        "tied scores must be broken FileId-ASC: got {ids:?}"
    );

    drop(dir);
}

// ============================================================================
// #286 Wave 4 perf tests — AC1–AC12
// ============================================================================

// ---- AC1/AC5: P1 and P3 score equivalence (FakePostingSource path) ----------
//
// For a fixed corpus + multi-n-gram query the FULL (FileId, f64) vector from
// search_ast must be BYTE-IDENTICAL (files, score bits, order) before and
// after the P1 partial-decode and P3 capacity changes.
// The fixture includes TWO FILES WITH IDENTICAL SCORES to exercise the
// FileId-ASC tie-break.

#[test]
fn ac1_ac5_multi_ngram_score_equivalence_with_tied_scores() {
    // Craft a corpus where files 0 and 1 produce identical BM25 scores.
    // Same lang, same node_count, same TF per n-gram → identical result.
    // Files 2 and 3 have different inputs to exercise the general path too.
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let expr_id = vocab_lookup("expression_statement").unwrap();

    let bigram = AstBigram::encode(fn_id, block_id);
    let trigram = AstTrigram::encode(fn_id, block_id, expr_id);

    // Build the source (multi-n-gram to exercise meta_cache path).
    let source = FakePostingSource::default()
        // Files 0 and 1: identical parameters → identical scores (tie).
        .with_file(0, Language::Rust, 100)
        .with_file(1, Language::Rust, 100)
        // File 2: different count → different (higher) score.
        .with_file(2, Language::Rust, 100)
        // File 3: different node_count → different (lower) score.
        .with_file(3, Language::Rust, 200)
        .with_bigram(
            bigram.key(),
            vec![
                AstPosting {
                    doc_id: 0,
                    count: 1,
                },
                AstPosting {
                    doc_id: 1,
                    count: 1,
                }, // tied with 0
                AstPosting {
                    doc_id: 2,
                    count: 3,
                }, // higher TF
                AstPosting {
                    doc_id: 3,
                    count: 1,
                }, // larger file
            ],
        )
        .with_trigram(
            trigram.key(),
            vec![
                AstPosting {
                    doc_id: 0,
                    count: 1,
                },
                AstPosting {
                    doc_id: 1,
                    count: 1,
                }, // tied with 0
                AstPosting {
                    doc_id: 2,
                    count: 1,
                },
                AstPosting {
                    doc_id: 3,
                    count: 1,
                },
            ],
        )
        .with_avg_node_count(100.0);

    let engine = AstQueryEngine::new(source);

    let set = AstNgramSet {
        bigrams: vec![AstBigramEntry {
            ngram: bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        }],
        trigrams: vec![AstTrigramEntry {
            ngram: trigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        }],
    };
    let q = AstQuery::Containment(set);
    let results = engine.search_ast(&q).unwrap();

    // Must return all 4 files.
    assert_eq!(results.len(), 4, "all 4 files should match");

    // Results are FileId-ASC (search_ast contract).
    for i in 1..results.len() {
        assert!(
            results[i - 1].0 < results[i].0,
            "results must be FileId-ASC: {:?}",
            results.iter().map(|(f, _)| f).collect::<Vec<_>>()
        );
    }

    // Files 0 and 1 must have BYTE-IDENTICAL scores (tied).
    let s0 = results.iter().find(|(f, _)| *f == FileId(0)).unwrap().1;
    let s1 = results.iter().find(|(f, _)| *f == FileId(1)).unwrap().1;
    assert_eq!(
        s0.to_bits(),
        s1.to_bits(),
        "files 0 and 1 must have bitwise-equal scores: s0={s0}, s1={s1}"
    );

    // All scores must be positive.
    for (fid, s) in &results {
        assert!(*s > 0.0, "score for {fid} must be positive: {s}");
    }

    // Relative ordering of non-tied files: file 2 (higher TF) must score
    // strictly above files 0/1; file 3 (larger node_count) must score strictly
    // below files 0/1.  The fixture was built to discriminate these paths, so
    // asserting the relative ordering proves the non-tie scoring path is also
    // correct — not just the tie-break (#286).
    let s2 = results.iter().find(|(f, _)| *f == FileId(2)).unwrap().1;
    let s3 = results.iter().find(|(f, _)| *f == FileId(3)).unwrap().1;
    assert!(
        s2 > s0,
        "file 2 (higher TF) must score above tied files 0/1: s2={s2}, s0={s0}"
    );
    assert!(
        s3 < s0,
        "file 3 (larger node_count) must score below tied files 0/1: s3={s3}, s0={s0}"
    );

    // Run a second time and assert byte-identical output (determinism check).
    let results2 = engine.search_ast(&q).unwrap();
    assert_eq!(results.len(), results2.len(), "second run length mismatch");
    for ((f1, s1_val), (f2, s2_val)) in results.iter().zip(results2.iter()) {
        assert_eq!(f1, f2, "FileId mismatch between runs");
        assert_eq!(
            s1_val.to_bits(),
            s2_val.to_bits(),
            "score bits differ between runs for {f1}: first={s1_val}, second={s2_val}"
        );
    }
}

// ---- AC2: P1 API contract — file_lang_and_node_count matches file_meta ------
// (tested in reader_tests.rs for the real AstIndexReader)
// Here we verify the DEFAULT impl on FakePostingSource agrees.

#[test]
fn ac2_default_file_lang_and_node_count_equals_file_meta_fields() {
    use crate::index::lang_map::lang_to_id;

    // FakePostingSource uses the default trait method (delegating to file_meta).
    let source = FakePostingSource::default()
        .with_file(0, Language::Rust, 42)
        .with_file(1, Language::Python, 99);

    let (lid0, nc0) = source.file_lang_and_node_count(0).unwrap();
    let meta0 = source.file_meta(0).unwrap();
    assert_eq!(lid0, meta0.lang_id, "lang_id mismatch for doc 0");
    assert_eq!(nc0, meta0.node_count, "node_count mismatch for doc 0");
    assert_eq!(lid0, lang_to_id(Language::Rust));
    assert_eq!(nc0, 42);

    let (lid1, nc1) = source.file_lang_and_node_count(1).unwrap();
    let meta1 = source.file_meta(1).unwrap();
    assert_eq!(lid1, meta1.lang_id);
    assert_eq!(nc1, meta1.node_count);
    assert_eq!(lid1, lang_to_id(Language::Python));
    assert_eq!(nc1, 99);

    // Out-of-range: same Err variant as file_meta.
    let err_meta = source.file_meta(99).unwrap_err();
    let err_lite = source.file_lang_and_node_count(99).unwrap_err();
    assert!(
        matches!(err_meta, crate::SearchError::IndexCorrupted(_)),
        "file_meta oob should be IndexCorrupted: {err_meta:?}"
    );
    assert!(
        matches!(err_lite, crate::SearchError::IndexCorrupted(_)),
        "file_lang_and_node_count oob should be IndexCorrupted: {err_lite:?}"
    );
}

// ---- AC3: P1 unknown lang_id forward-compat ---------------------------------

#[test]
fn ac3_unknown_lang_id_still_scores_positively() {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    let mut source = FakePostingSource::default().with_avg_node_count(100.0);
    source.file_metas.insert(
        0,
        AstFileMetaEntry {
            lang_id: 200, // unknown
            node_count: 100,
            max_depth: 3,
            max_block_stmts: 5,
            max_params: 2,
            branch_count: 1,
        },
    );
    source = source.with_file(1, Language::Rust, 100);
    source.bigrams.insert(
        bigram.key(),
        vec![
            AstPosting {
                doc_id: 0,
                count: 1,
            },
            AstPosting {
                doc_id: 1,
                count: 1,
            },
        ],
    );

    let engine = AstQueryEngine::new(source);
    let q = AstQuery::Containment(make_bigram_set(bigram, 1));
    let results = engine.search_ast(&q).unwrap();

    assert_eq!(
        results.len(),
        2,
        "both files (including unknown-lang) should match"
    );

    let s0 = results.iter().find(|(f, _)| *f == FileId(0)).unwrap().1;
    let s1 = results.iter().find(|(f, _)| *f == FileId(1)).unwrap().1;
    assert!(s0 > 0.0, "unknown-lang file must have positive score: {s0}");
    assert!(s1 > 0.0, "known-lang file must have positive score: {s1}");
}

// ---- AC4: Format version and FILE_META_SIZE constants unchanged -------------

#[test]
fn ac4_format_constants_unchanged() {
    use crate::ast_index::store::format::{FILE_META_SIZE, FORMAT_VERSION};
    assert_eq!(FORMAT_VERSION, 2, "FORMAT_VERSION must remain 2 (AC4)");
    assert_eq!(FILE_META_SIZE, 15, "FILE_META_SIZE must remain 15 (AC4)");
}

// ---- AC6: P3 posting-driven sizing — selective vs broad, multi-n-gram path ---
//
// AC6 exists to prove that the initial capacity of `scores` is a function of
// the query's actual posting-list sizes, NOT `file_count()`.  We cannot
// directly inspect internal FxHashMap capacity from a test, so we verify the
// *discriminating property* instead: a selective query with a 5-posting list
// in a 100-file corpus produces exactly 5 results (and not, say, a wrong count
// or corrupted scores due to a realloc-induced ordering artifact).
//
// The multi-n-gram fixture (two bigrams) exercises the `meta_cache` path where
// P3's `reserve()` call is most load-bearing.  Reverting P3 to `file_count()`
// sizing would still pass the count assertions, but the following golden-score
// sub-assertion would catch any score corruption caused by a capacity/rehash
// regression: the 5 files that match the selective bigram get an extra
// contribution from the second n-gram that the broad-only files do not, so
// their scores must strictly exceed the broad-only files' scores.
// (discriminating assertion, #286)

#[test]
fn ac6_selective_query_returns_exact_posting_count_multi_ngram() {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let try_id = vocab_lookup("try_statement").unwrap();
    let catch_id = vocab_lookup("catch_clause").unwrap();

    let broad_bigram = AstBigram::encode(fn_id, block_id); // matches all 100 files
    let selective_bigram = AstBigram::encode(try_id, catch_id); // matches only first 5 files

    let mut source = FakePostingSource::default().with_avg_node_count(100.0);
    for i in 0..100u32 {
        source = source.with_file(i, Language::Rust, 100);
    }
    // Broad n-gram: 100 postings (≈ file_count).
    let broad_postings: Vec<AstPosting> = (0..100u32)
        .map(|i| AstPosting {
            doc_id: i,
            count: 1,
        })
        .collect();
    source.bigrams.insert(broad_bigram.key(), broad_postings);
    // Selective n-gram: only 5 postings — << file_count.
    // Using count=2 so the selective-match score differs from the broad-only score.
    let selective_postings: Vec<AstPosting> = (0..5u32)
        .map(|i| AstPosting {
            doc_id: i,
            count: 2,
        })
        .collect();
    source
        .bigrams
        .insert(selective_bigram.key(), selective_postings);

    let engine = AstQueryEngine::new(source);

    // Single-n-gram broad: should return all 100 files (AC5-equivalent check).
    let q_broad = AstQuery::Containment(make_bigram_set(broad_bigram, 1));
    let broad_results = engine.search_ast(&q_broad).unwrap();
    assert_eq!(
        broad_results.len(),
        100,
        "broad single-ngram query must return all 100 files"
    );

    // Single-n-gram selective: exactly 5 files with FileIds in [0,5).
    let q_sel = AstQuery::Containment(make_bigram_set(selective_bigram, 1));
    let sel_results = engine.search_ast(&q_sel).unwrap();
    assert_eq!(
        sel_results.len(),
        5,
        "selective query must return exactly 5 files (posting-list length, not file_count)"
    );
    for (fid, score) in &sel_results {
        assert!(fid.0 < 5, "selective result FileId must be in [0,5): {fid}");
        assert!(*score > 0.0, "score must be positive: {score}");
    }

    // Multi-n-gram query: files 0–4 match BOTH bigrams (broad + selective),
    // files 5–99 match only broad.  Exercise the meta_cache P3 path.
    let mut bigram_entries = vec![
        AstBigramEntry {
            ngram: broad_bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        },
        AstBigramEntry {
            ngram: selective_bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        },
    ];
    bigram_entries.sort_unstable_by_key(|e| e.ngram.key());
    let multi_set = AstNgramSet {
        bigrams: bigram_entries,
        trigrams: vec![],
    };

    // P3 path: OR-union via run_ngram_set_with_capacity (preserves legacy scoring
    // behavior for capacity tests — AD-374-1 note: search_ast uses AND-intersect
    // since #374, but P3 is a scoring-layer property tested here via the OR-union path).
    let (multi_results, _) = engine.run_ngram_set_with_capacity(&multi_set, None).unwrap();
    assert_eq!(
        multi_results.len(),
        100,
        "multi-ngram OR-union (P3 path) must return all 100 files; \
         files 5–99 are in the broad list but NOT the selective list"
    );

    // Discriminating: files 0–4 must score strictly higher than files 5–99
    // (they have an extra contribution from the selective bigram at count=2).
    // Reverting P3 to file_count() sizing does not break this assertion, but
    // any score-corruption from a P3 realloc regression would.
    let score_0 = multi_results
        .iter()
        .find(|(f, _)| *f == FileId(0))
        .unwrap()
        .1;
    let score_5 = multi_results
        .iter()
        .find(|(f, _)| *f == FileId(5))
        .unwrap()
        .1;
    assert!(
        score_0 > score_5,
        "selective-match files must outscore broad-only files: score_0={score_0}, score_5={score_5}"
    );

    // AND-intersect guard (AD-374-1): search_ast with 2 lists → only files 0–4
    // (in BOTH lists) survive.
    let q_multi_and = AstQuery::Containment(multi_set);
    let and_results = engine.search_ast(&q_multi_and).unwrap();
    assert_eq!(
        and_results.len(),
        5,
        "AND-intersect (search_ast, AD-374-1) must return only 5 files (files 0–4 in both lists); \
         files 5–99 are in broad-only and are correctly excluded"
    );
    for (fid, _) in &and_results {
        assert!(fid.0 < 5, "AND-intersect result must be FileId in [0,5), got: {fid}");
    }
}

// ---- AC7: P3 empty-first-ngram does not under-size -------------------------
//
// When the FIRST n-gram has an empty posting list and a LATER n-gram has a
// large posting list, the `reserve()` call in `ScoringCtx::score_postings`
// must handle the large list correctly.  Reverting P3 to a single
// `file_count()` pre-alloc would also pass the count assertion; the
// discriminating assertion verifies that multi-n-gram scores are correct
// (the large-bigram contribution appears, and empty-bigram contributes
// nothing), proving reserve() executed on the right posting list.
// (discriminating assertion, #286)

#[test]
fn ac7_empty_first_ngram_large_second_returns_correct_results() {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let try_id = vocab_lookup("try_statement").unwrap();
    let catch_id = vocab_lookup("catch_clause").unwrap();

    let empty_bigram = AstBigram::encode(fn_id, block_id);
    let large_bigram = AstBigram::encode(try_id, catch_id);

    let mut source = FakePostingSource::default().with_avg_node_count(100.0);
    for i in 0..200u32 {
        source = source.with_file(i, Language::Rust, 100);
    }
    source.bigrams.insert(empty_bigram.key(), vec![]);
    let large_postings: Vec<AstPosting> = (0..200u32)
        .map(|i| AstPosting {
            doc_id: i,
            count: 1,
        })
        .collect();
    source.bigrams.insert(large_bigram.key(), large_postings);

    let engine = AstQueryEngine::new(source);

    let mut bigram_entries = vec![
        AstBigramEntry {
            ngram: empty_bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        },
        AstBigramEntry {
            ngram: large_bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        },
    ];
    bigram_entries.sort_unstable_by_key(|e| e.ngram.key());
    let set = AstNgramSet {
        bigrams: bigram_entries,
        trigrams: vec![],
    };

    // P3 path (OR-union via run_ngram_set_with_capacity): all 200 files must appear
    // despite the empty first n-gram — AD-374-1 note: search_ast uses AND-intersect
    // since #374; the P3 reserve() property is a scoring-layer concern tested here
    // via the OR-union path which score_ngram_set drives directly.
    let (results, _) = engine.run_ngram_set_with_capacity(&set, None).unwrap();

    assert_eq!(
        results.len(),
        200,
        "all 200 files must appear despite empty first n-gram (AC7 P3 OR-union path)"
    );
    for (_, s) in &results {
        assert!(*s > 0.0, "all scores must be positive");
    }

    // Discriminating: every file's score must equal exactly the score from the
    // large bigram alone (the empty bigram contributes nothing).  This verifies
    // that the empty-first-list path executes the large-list reserve() correctly
    // and that no scoring was corrupted by a grow-from-zero realloc.
    // Compute the expected score directly (single-n-gram reference path).
    let ref_set = make_bigram_set(large_bigram, 1);
    let (ref_results, _) = engine.run_ngram_set_with_capacity(&ref_set, None).unwrap();
    assert_eq!(
        ref_results.len(),
        200,
        "reference single-ngram must also return 200 files"
    );
    // Both result vectors are FileId-ASC — compare element-by-element.
    for ((fid_multi, s_multi), (fid_ref, s_ref)) in results.iter().zip(ref_results.iter()) {
        assert_eq!(fid_multi, fid_ref, "FileId order mismatch");
        assert_eq!(
            s_multi.to_bits(),
            s_ref.to_bits(),
            "score mismatch for {fid_multi}: multi={s_multi}, ref={s_ref} (empty n-gram must contribute nothing)"
        );
    }

    // AND-intersect guard (AD-374-1): search_ast with empty_bigram in the set →
    // intersection is empty (empty ∩ large = empty) → 0 results.
    let q_and = AstQuery::Containment(set);
    let and_results = engine.search_ast(&q_and).unwrap();
    assert_eq!(
        and_results.len(),
        0,
        "AND-intersect (search_ast, AD-374-1): empty-bigram list intersected with large list = 0 results; \
         any file in the empty list is vacuously absent from the intersection"
    );
}

// ---- AC8: P2 scalar IDF cache score-equivalence (avoids PF-005) ------------
//
// The `last_lang`/`last_idf` scalar cache in `ScoringCtx::score_postings`
// collapses O(postings) IDF lookups to O(distinct-langs-in-run).  This test
// verifies that the cached path produces BYTE-IDENTICAL scores to a naive
// no-cache reference computation on a doc_id-interleaved mixed-language corpus.
//
// A stale-cache regression (wrong `last_idf` used across language boundaries)
// would cause score divergence between the cached and reference paths.
// (discriminating assertion, #286)

#[test]
fn ac8_scalar_idf_cache_score_equivalence() {
    use crate::ast_index::ast_bigram_idf;

    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    // Interleaved corpus: Rust(0), Python(1), Go(2), Rust(3), Python(4), Go(5).
    // The interleaving maximises cache misses on `last_lang` for each posting,
    // exercising the branch that updates `last_lang`/`last_idf`.
    let langs = [
        Language::Rust,
        Language::Python,
        Language::Go,
        Language::Rust,
        Language::Python,
        Language::Go,
    ];
    let avg_node_count: f32 = 100.0;
    let mut source = FakePostingSource::default().with_avg_node_count(avg_node_count);
    for (i, &lang) in langs.iter().enumerate() {
        source = source.with_file(i as u32, lang, 100);
    }
    let postings: Vec<AstPosting> = (0..6u32)
        .map(|i| AstPosting {
            doc_id: i,
            count: 1 + i, // distinct TF per file
        })
        .collect();
    source.bigrams.insert(bigram.key(), postings.clone());

    let engine = AstQueryEngine::new(source);
    let q = AstQuery::Containment(make_bigram_set(bigram, 1));
    let results = engine.search_ast(&q).unwrap();

    assert_eq!(results.len(), 6, "all 6 files must match");

    // Compute reference scores without any IDF cache — naive per-posting lookup.
    let avg = f64::from(avg_node_count);
    let k1 = AST_BM25_K1;
    let b = AST_BM25_B;
    for (i, &lang) in langs.iter().enumerate() {
        let posting = &postings[i];
        let idf = f64::from(ast_bigram_idf(lang, bigram));
        let nc = 100.0_f64;
        let length_norm = 1.0 - b + b * (nc / avg); // = 1.0 (nc == avg)
        let tf_norm = f64::from(posting.count) / length_norm;
        let ref_score = idf * (tf_norm / (tf_norm + k1));

        let (_, cached_score) = results
            .iter()
            .find(|(f, _)| f.0 == i as u32)
            .expect("file must be in results");

        assert_eq!(
            cached_score.to_bits(),
            ref_score.to_bits(),
            "scalar IDF cache must produce byte-identical score for doc {i} (lang={lang:?}): \
             cached={cached_score}, ref={ref_score}"
        );
    }
}

// ---- AC9–AC11 shared fixture: 4 Rust + 3 Python files on function_item>block -

/// Build a `function_item > block` index with 4 Rust files (ids 0–3) and 3
/// Python files (ids 4–6).  The `TempDir` is returned alongside the engine so
/// the caller keeps it alive for the duration of the test.
fn build_mixed_lang_engine() -> (
    tempfile::TempDir,
    AstQueryEngine<crate::ast_index::AstIndexReader>,
) {
    use crate::ast_index::StructuralMetrics;
    let dir = tempfile::tempdir().unwrap();
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);
    let set = make_bigram_set(bigram, 1);
    let mut builder = crate::ast_index::AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    for i in 0..4u32 {
        builder
            .add_file_ngrams(
                FileId(i),
                Language::Rust,
                &set,
                100,
                StructuralMetrics::default(),
            )
            .unwrap();
    }
    for i in 4..7u32 {
        builder
            .add_file_ngrams(
                FileId(i),
                Language::Python,
                &set,
                100,
                StructuralMetrics::default(),
            )
            .unwrap();
    }
    let engine = AstQueryEngine::new(builder.build().unwrap());
    (dir, engine)
}

// ---- AC9: P4 lang filter narrows to strict non-empty subset (real reader) ---

#[test]
fn ac9_p4_lang_filter_narrows_to_strict_subset_real_reader() {
    let (_dir, engine) = build_mixed_lang_engine();

    let mut q_all = crate::types::SearchQuery::new("q");
    q_all.ast_pattern = Some("function_item > block".into());
    q_all.limit = Some(100);
    let unfiltered = engine.search(&q_all).unwrap();
    assert_eq!(unfiltered.len(), 7, "unfiltered must return all 7 files");

    let mut q_rust = crate::types::SearchQuery::new("q");
    q_rust.ast_pattern = Some("function_item > block".into());
    q_rust.lang = Some(Language::Rust);
    q_rust.limit = Some(100);
    let rust_only = engine.search(&q_rust).unwrap();

    assert!(!rust_only.is_empty(), "filtered result must be non-empty");
    assert!(
        rust_only.len() < unfiltered.len(),
        "filtered must be strict subset: filtered={}, unfiltered={}",
        rust_only.len(),
        unfiltered.len()
    );
    assert_eq!(rust_only.len(), 4, "exactly 4 Rust files should match");
    for r in &rust_only {
        assert!(
            r.file_id.0 < 4,
            "filtered result must be a Rust file: {}",
            r.file_id
        );
    }
    let unfiltered_ids: std::collections::HashSet<FileId> =
        unfiltered.iter().map(|r| r.file_id).collect();
    for r in &rust_only {
        assert!(
            unfiltered_ids.contains(&r.file_id),
            "filtered FileId {} must be in unfiltered set",
            r.file_id
        );
    }
}

// ---- AC10: P4 lang=None path unchanged (avoids PF-006) ---------------------

#[test]
fn ac10_p4_lang_none_path_unchanged_real_reader() {
    let (_dir, engine) = build_mixed_lang_engine();

    let mut q = crate::types::SearchQuery::new("q");
    q.ast_pattern = Some("function_item > block".into());
    q.limit = Some(100);
    let results1 = engine.search(&q).unwrap();
    let results2 = engine.search(&q).unwrap();

    assert_eq!(results1.len(), 7, "lang=None must return all 7 files");
    assert_eq!(results1.len(), results2.len(), "deterministic");
    for (r1, r2) in results1.iter().zip(results2.iter()) {
        assert_eq!(r1.file_id, r2.file_id, "FileId mismatch");
        assert_eq!(r1.score.to_bits(), r2.score.to_bits(), "score bits differ");
    }
}

// ---- AC11: P4 filter-composition order (file_filter + lang + limit) ---------

#[test]
fn ac11_p4_filter_composition_with_file_filter_and_limit() {
    let (_dir, engine) = build_mixed_lang_engine();

    // file_filter = {0, 1, 2, 4, 5}, lang = Rust, limit = 2.
    // Expected: Rust files in allowlist = {0, 1, 2}; limit=2 → top-2 by score
    // (tied → FileId-ASC → [0, 1]).
    let mut q = crate::types::SearchQuery::new("q");
    q.ast_pattern = Some("function_item > block".into());
    q.lang = Some(Language::Rust);
    q.file_filter = Some(
        [FileId(0), FileId(1), FileId(2), FileId(4), FileId(5)]
            .iter()
            .copied()
            .collect::<std::collections::HashSet<FileId>>(),
    );
    q.limit = Some(2);

    let results = engine.search(&q).unwrap();
    assert_eq!(results.len(), 2, "limit=2 should yield 2 results");

    // All three Rust files (0, 1, 2) pass the lang+allowlist filters and have
    // identical scores (same lang, node_count, TF).  Score-DESC/FileId-ASC
    // tie-break must yield [FileId(0), FileId(1)] as the top-2.
    // Asserting the exact ordered pair (not just allowlist membership) is what
    // AC11 exists to prove: the score-DESC/FileId-ASC ordering-under-limit
    // is correct when lang, file_filter, and limit are all active (#286).
    let ids: Vec<FileId> = results.iter().map(|r| r.file_id).collect();
    assert_eq!(
        ids,
        vec![FileId(0), FileId(1)],
        "tied scores + limit=2 must yield [FileId(0), FileId(1)] in FileId-ASC order: got {ids:?}"
    );
}

// ---- AC12: P4 search_ast returns unfiltered results (lang param always None) -

#[test]
fn ac12_search_ast_is_unfiltered_after_p4() {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    let source = FakePostingSource::default()
        .with_file(0, Language::Rust, 100)
        .with_file(1, Language::Python, 100)
        .with_file(2, Language::Go, 100)
        .with_bigram(
            bigram.key(),
            vec![
                AstPosting {
                    doc_id: 0,
                    count: 1,
                },
                AstPosting {
                    doc_id: 1,
                    count: 1,
                },
                AstPosting {
                    doc_id: 2,
                    count: 1,
                },
            ],
        )
        .with_avg_node_count(100.0);

    let engine = AstQueryEngine::new(source);
    let q = AstQuery::Containment(make_bigram_set(bigram, 1));

    let r1 = engine.search_ast(&q).unwrap();
    let r2 = engine.search_ast(&q).unwrap();

    assert_eq!(
        r1.len(),
        3,
        "search_ast must return all 3 files (unfiltered)"
    );
    assert_eq!(r1.len(), r2.len(), "deterministic");

    for (a, b) in r1.iter().zip(r2.iter()) {
        assert_eq!(a.0, b.0, "FileId differs between calls");
        assert_eq!(
            a.1.to_bits(),
            b.1.to_bits(),
            "score bits differ between calls"
        );
    }

    for i in 1..r1.len() {
        assert!(r1[i - 1].0 < r1[i].0, "results must be FileId-ASC");
    }
}

// ---- AC6b/AC7b: P3 capacity hook — posting-driven sizing verified (#286) ----
//
// The plan (AC6/AC7) called for a `#[cfg(test)]` capacity hook that was not
// added in the initial implementation.  `run_ngram_set_with_capacity` exposes
// the `scores` map capacity after scoring so we can confirm P3 reserves
// proportional to the posting-list length rather than `file_count()`.
//
// For a selective 5-posting query in a 100-file corpus the capacity after
// scoring must be ≥ 5 (all results fit) and < 100 (not file_count-sized).
// For an empty-first + 200-posting second the capacity must be ≥ 200.

#[test]
fn ac6b_p3_capacity_bounded_by_posting_list_not_file_count() {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let try_id = vocab_lookup("try_statement").unwrap();
    let catch_id = vocab_lookup("catch_clause").unwrap();

    let broad_bigram = AstBigram::encode(fn_id, block_id);
    let selective_bigram = AstBigram::encode(try_id, catch_id);

    let mut source = FakePostingSource::default().with_avg_node_count(100.0);
    for i in 0..100u32 {
        source = source.with_file(i, Language::Rust, 100);
    }
    // Selective n-gram: only 5 postings.
    let selective_postings: Vec<AstPosting> = (0..5u32)
        .map(|i| AstPosting {
            doc_id: i,
            count: 1,
        })
        .collect();
    source
        .bigrams
        .insert(selective_bigram.key(), selective_postings);

    let engine = AstQueryEngine::new(source);

    // Single-n-gram selective query: capacity must be ≥ 5 (holds results)
    // and strictly < file_count (100), proving P3 does NOT pre-allocate file_count.
    let set = make_bigram_set(selective_bigram, 1);
    let (results, cap) = engine.run_ngram_set_with_capacity(&set, None).unwrap();
    assert_eq!(
        results.len(),
        5,
        "selective query must return exactly 5 files"
    );
    assert!(
        cap >= 5,
        "capacity must be at least posting-list length (5): cap={cap}"
    );
    assert!(
        cap < 100,
        "capacity must be < file_count (100) for selective queries (P3): cap={cap}"
    );

    // Multi-n-gram with broad bigram (100 postings) + selective (5): capacity
    // must grow to fit all 100 results.
    let mut source2 = FakePostingSource::default().with_avg_node_count(100.0);
    for i in 0..100u32 {
        source2 = source2.with_file(i, Language::Rust, 100);
    }
    let broad_postings: Vec<AstPosting> = (0..100u32)
        .map(|i| AstPosting {
            doc_id: i,
            count: 1,
        })
        .collect();
    source2.bigrams.insert(broad_bigram.key(), broad_postings);
    let selective_postings2: Vec<AstPosting> = (0..5u32)
        .map(|i| AstPosting {
            doc_id: i,
            count: 2,
        })
        .collect();
    source2
        .bigrams
        .insert(selective_bigram.key(), selective_postings2);

    let engine2 = AstQueryEngine::new(source2);
    let mut bigram_entries = vec![
        AstBigramEntry {
            ngram: broad_bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        },
        AstBigramEntry {
            ngram: selective_bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        },
    ];
    bigram_entries.sort_unstable_by_key(|e| e.ngram.key());
    let multi_set = AstNgramSet {
        bigrams: bigram_entries,
        trigrams: vec![],
    };
    let (multi_results, multi_cap) = engine2
        .run_ngram_set_with_capacity(&multi_set, None)
        .unwrap();
    assert_eq!(
        multi_results.len(),
        100,
        "multi-ngram must return all 100 files"
    );
    assert!(
        multi_cap >= 100,
        "capacity must grow to fit all 100 results after broad n-gram: cap={multi_cap}"
    );
}

#[test]
fn ac7b_p3_empty_first_ngram_capacity_grows_for_second() {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let try_id = vocab_lookup("try_statement").unwrap();
    let catch_id = vocab_lookup("catch_clause").unwrap();

    let empty_bigram = AstBigram::encode(fn_id, block_id);
    let large_bigram = AstBigram::encode(try_id, catch_id);

    let mut source = FakePostingSource::default().with_avg_node_count(100.0);
    for i in 0..200u32 {
        source = source.with_file(i, Language::Rust, 100);
    }
    source.bigrams.insert(empty_bigram.key(), vec![]);
    let large_postings: Vec<AstPosting> = (0..200u32)
        .map(|i| AstPosting {
            doc_id: i,
            count: 1,
        })
        .collect();
    source.bigrams.insert(large_bigram.key(), large_postings);

    let engine = AstQueryEngine::new(source);
    let mut bigram_entries = vec![
        AstBigramEntry {
            ngram: empty_bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        },
        AstBigramEntry {
            ngram: large_bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        },
    ];
    bigram_entries.sort_unstable_by_key(|e| e.ngram.key());
    let set = AstNgramSet {
        bigrams: bigram_entries,
        trigrams: vec![],
    };
    let (results, cap) = engine.run_ngram_set_with_capacity(&set, None).unwrap();
    assert_eq!(
        results.len(),
        200,
        "all 200 files must appear despite empty first n-gram"
    );
    assert!(
        cap >= 200,
        "capacity must grow to fit 200 results (large second n-gram): cap={cap}"
    );
}

// ---- P4 forward-compat: unknown lang_id file in corpus with active lang filter ----
//
// AC3 tests search_ast (which always passes lang_filter=None).  This test
// instead uses the SearchLayer path (lang_filter=Some) and places a file with
// an UNKNOWN lang_id alongside a valid-language file.  The unknown-lang file
// must be SKIPPED (not scored) when the lang filter is active (#286 P4).

#[test]
fn p4_unknown_lang_id_skipped_under_active_lang_filter() {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    // Corpus: file 0 has unknown lang_id 200; file 1 has Language::Rust.
    let mut source = FakePostingSource::default().with_avg_node_count(100.0);
    source.file_metas.insert(
        0,
        AstFileMetaEntry {
            lang_id: 200, // unknown — lang_from_id returns None
            node_count: 100,
            max_depth: 3,
            max_block_stmts: 5,
            max_params: 2,
            branch_count: 1,
        },
    );
    source = source.with_file(1, Language::Rust, 100);
    source.file_count = 2;
    source.bigrams.insert(
        bigram.key(),
        vec![
            AstPosting {
                doc_id: 0,
                count: 1,
            },
            AstPosting {
                doc_id: 1,
                count: 1,
            },
        ],
    );

    let engine = AstQueryEngine::new(source);

    // Without lang filter: both files should score positively (AC3 behaviour).
    let q_no_filter = AstQuery::Containment(make_bigram_set(bigram, 1));
    let unfiltered = engine.search_ast(&q_no_filter).unwrap();
    assert_eq!(
        unfiltered.len(),
        2,
        "without lang filter both files must score"
    );

    // With lang filter = Rust: unknown-lang file (0) must be SKIPPED; only file 1 returned.
    let set = make_bigram_set(bigram, 1);
    let (filtered, _cap) = engine
        .run_ngram_set_with_capacity(&set, Some(Language::Rust))
        .unwrap();
    assert_eq!(
        filtered.len(),
        1,
        "with lang=Rust filter, unknown-lang file must be skipped: got {filtered:?}"
    );
    assert_eq!(
        filtered[0].0,
        FileId(1),
        "only the Rust file must appear in filtered results"
    );
    assert!(filtered[0].1 > 0.0, "Rust file must have positive score");
}

// ---- AC1/AC5 extended: independent BM25 score verification on multi-n-gram path ----
//
// The original ac1_ac5 test verifies determinism, tie equality, and relative
// ordering, but does not compute an independent expected BM25 value.  This
// companion test adds an exact golden-score assertion for the multi-n-gram
// meta_cache path, so a systematic scoring defect in the new bm25_with_lite /
// LiteMeta path that preserved determinism and relative ordering would still
// fail (#286).

#[test]
fn ac1_ac5_multi_ngram_independent_golden_score() {
    use crate::ast_index::{ast_bigram_idf, ast_trigram_idf};

    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let expr_id = vocab_lookup("expression_statement").unwrap();

    let bigram = AstBigram::encode(fn_id, block_id);
    let trigram = AstTrigram::encode(fn_id, block_id, expr_id);

    // Single file corpus: node_count=100, avg=100 → length_norm=1.0.
    // bigram count=2, trigram count=1.
    let source = FakePostingSource::default()
        .with_file(0, Language::Rust, 100)
        .with_bigram(
            bigram.key(),
            vec![AstPosting {
                doc_id: 0,
                count: 2,
            }],
        )
        .with_trigram(
            trigram.key(),
            vec![AstPosting {
                doc_id: 0,
                count: 1,
            }],
        )
        .with_avg_node_count(100.0);

    let engine = AstQueryEngine::new(source);

    let set = AstNgramSet {
        bigrams: vec![AstBigramEntry {
            ngram: bigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        }],
        trigrams: vec![AstTrigramEntry {
            ngram: trigram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        }],
    };
    let q = AstQuery::Containment(set);
    let results = engine.search_ast(&q).unwrap();

    assert_eq!(results.len(), 1);

    // Independent golden BM25 computation:
    //   length_norm = 1 - 0.75 + 0.75 * (100/100) = 1.0
    //   bigram:  tf_norm = 2.0 / 1.0 = 2.0
    //            score_b = idf_b * (2.0 / (2.0 + 1.2))
    //   trigram: tf_norm = 1.0 / 1.0 = 1.0
    //            score_t = idf_t * (1.0 / (1.0 + 1.2))
    //   total   = score_b + score_t
    let k1 = AST_BM25_K1;
    let idf_b = f64::from(ast_bigram_idf(Language::Rust, bigram));
    let idf_t = f64::from(ast_trigram_idf(Language::Rust, trigram));
    let tf_norm_b = 2.0_f64; // count=2, length_norm=1.0
    let tf_norm_t = 1.0_f64; // count=1, length_norm=1.0
    let expected = idf_b * (tf_norm_b / (tf_norm_b + k1)) + idf_t * (tf_norm_t / (tf_norm_t + k1));

    let got = results[0].1;
    assert!(
        (got - expected).abs() < 1e-9,
        "multi-ngram meta_cache golden score mismatch: got={got}, expected={expected} \
         (idf_b={idf_b}, idf_t={idf_t}, tf_norm_b={tf_norm_b}, tf_norm_t={tf_norm_t})"
    );
}
