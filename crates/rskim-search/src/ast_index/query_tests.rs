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

// --- A3: OR-union ranking — both-clause file outscores one-clause ---

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

    assert_eq!(results.len(), 2);
    // Find scores by FileId (results are FileId-asc)
    let score0 = results.iter().find(|(f, _)| *f == FileId(0)).unwrap().1;
    let score1 = results.iter().find(|(f, _)| *f == FileId(1)).unwrap().1;
    assert!(
        score0 > score1,
        "file with both n-grams should score higher: {score0} vs {score1}"
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

// B4: equal scores exercise the FileId-ASC tie-break
//
// Two files built from identical inputs (same language, same node_count, same
// bigram count) produce bitwise-equal BM25 scores. The SearchLayer sort is
// score-DESC with FileId-ASC as a tie-break. This test confirms the tie-break
// branch is live: a refactor inverting the order would fail here.
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
