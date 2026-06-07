//! Criterion benchmarks for Wave 3f: AST Structural Pattern Query Engine.
//!
//! Run with: cargo bench -p rskim-search --bench ast_query
//!
//! Goal: `search_ast` over a 10k-file synthetic index < 100ms (C1).
//!
//! Benchmark groups:
//!   1. hot_bigram             — query a bigram present in every file (single lang)
//!   2. hot_bigram_mixed_lang  — same bigram over a mixed-language corpus (exercises IDF cache)
//!   3. rare_trigram           — query a trigram in few files
//!   4. multi_ngram            — query a multi-n-gram pattern (try-catch)
//!   5. multi_ngram_overlap    — multi-n-gram with bigram+trigram overlap (exercises meta_cache)

#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use rskim_core::Language;
use rskim_search::{
    AstIndexBuilder, FileId,
    ast_index::{
        AstBigram, AstBigramEntry, AstNgramSet, AstQuery, AstQueryEngine, AstTrigram,
        AstTrigramEntry, DEFAULT_AST_WEIGHT, StructuralMetrics, parse_ast_query, vocab_lookup,
    },
};
use tempfile::TempDir;

// ============================================================================
// Synthetic index builder helpers
// ============================================================================

/// Build a synthetic index with `n` Rust files, each containing the given
/// bigram (count=1) and node_count=100.
///
/// Returns the TempDir (keep alive) and the opened reader.
fn build_index_with_bigram(
    n: usize,
    bigram: AstBigram,
) -> (TempDir, rskim_search::ast_index::AstIndexReader) {
    let dir = TempDir::new().unwrap();
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    for i in 0..n {
        let set = AstNgramSet {
            bigrams: vec![AstBigramEntry {
                ngram: bigram,
                weight: DEFAULT_AST_WEIGHT,
                count: 1 + (i as u32 % 5),
            }],
            trigrams: vec![],
        };
        builder
            .add_file_ngrams(
                FileId(i as u32),
                Language::Rust,
                &set,
                100,
                StructuralMetrics::default(),
            )
            .unwrap();
    }

    let reader = builder.build().unwrap();
    (dir, reader)
}

/// Build a synthetic index with `n` files, cycling through Rust/Python/Go/Java
/// languages. Each file contains the given bigram (count=1). This exercises the
/// per-n-gram IDF last-value cache over multiple distinct languages.
fn build_index_mixed_language(
    n: usize,
    bigram: AstBigram,
) -> (TempDir, rskim_search::ast_index::AstIndexReader) {
    let langs = [
        Language::Rust,
        Language::Python,
        Language::Go,
        Language::Java,
    ];
    let dir = TempDir::new().unwrap();
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    for i in 0..n {
        let lang = langs[i % langs.len()];
        let set = AstNgramSet {
            bigrams: vec![AstBigramEntry {
                ngram: bigram,
                weight: DEFAULT_AST_WEIGHT,
                count: 1 + (i as u32 % 5),
            }],
            trigrams: vec![],
        };
        builder
            .add_file_ngrams(
                FileId(i as u32),
                lang,
                &set,
                100,
                StructuralMetrics::default(),
            )
            .unwrap();
    }

    let reader = builder.build().unwrap();
    (dir, reader)
}

/// Build a synthetic index with `n` Rust files.
/// Every 10th file contains the given trigram in addition to the bigram.
fn build_index_with_rare_trigram(
    n: usize,
    bigram: AstBigram,
    trigram: AstTrigram,
) -> (TempDir, rskim_search::ast_index::AstIndexReader) {
    let dir = TempDir::new().unwrap();
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    for i in 0..n {
        let mut set = AstNgramSet {
            bigrams: vec![AstBigramEntry {
                ngram: bigram,
                weight: DEFAULT_AST_WEIGHT,
                count: 1,
            }],
            trigrams: vec![],
        };
        if i % 10 == 0 {
            set.trigrams.push(AstTrigramEntry {
                ngram: trigram,
                weight: DEFAULT_AST_WEIGHT,
                count: 1,
            });
        }
        builder
            .add_file_ngrams(
                FileId(i as u32),
                Language::Rust,
                &set,
                100,
                StructuralMetrics::default(),
            )
            .unwrap();
    }

    let reader = builder.build().unwrap();
    (dir, reader)
}

/// Build a synthetic index with `n` Rust files, each containing BOTH
/// the given bigram and trigram. This exercises the meta_cache cross-n-gram
/// hit path: scoring two n-grams means the same file_meta is requested twice.
fn build_index_with_bigram_and_trigram(
    n: usize,
    bigram: AstBigram,
    trigram: AstTrigram,
) -> (TempDir, rskim_search::ast_index::AstIndexReader) {
    let dir = TempDir::new().unwrap();
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    for i in 0..n {
        let set = AstNgramSet {
            bigrams: vec![AstBigramEntry {
                ngram: bigram,
                weight: DEFAULT_AST_WEIGHT,
                count: 2 + (i as u32 % 3),
            }],
            trigrams: vec![AstTrigramEntry {
                ngram: trigram,
                weight: DEFAULT_AST_WEIGHT,
                count: 1 + (i as u32 % 2),
            }],
        };
        builder
            .add_file_ngrams(
                FileId(i as u32),
                Language::Rust,
                &set,
                100,
                StructuralMetrics::default(),
            )
            .unwrap();
    }

    let reader = builder.build().unwrap();
    (dir, reader)
}

// ============================================================================
// Bench: hot bigram (all 10k files match, single language)
// ============================================================================

fn bench_hot_bigram(c: &mut Criterion) {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    let (_dir, reader) = build_index_with_bigram(10_000, bigram);
    let engine = AstQueryEngine::new(reader);
    let q = parse_ast_query("function_item > block").unwrap();

    let mut group = c.benchmark_group("ast_query");
    group.sample_size(10);
    group.bench_function("hot_bigram_10k_files", |b| {
        b.iter(|| engine.search_ast(black_box(&q)).unwrap())
    });
    group.finish();
}

// ============================================================================
// Bench: hot bigram (all 10k files match, mixed languages)
//
// Exercises the per-n-gram IDF last-value cache: with 4 cycling languages,
// the cache must update on each language transition rather than hitting on
// every posting. This reveals the actual IDF cache cost masked by single-lang
// benchmarks.
// ============================================================================

fn bench_hot_bigram_mixed_lang(c: &mut Criterion) {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);

    let (_dir, reader) = build_index_mixed_language(10_000, bigram);
    let engine = AstQueryEngine::new(reader);
    let q = parse_ast_query("function_item > block").unwrap();

    let mut group = c.benchmark_group("ast_query");
    group.sample_size(10);
    group.bench_function("hot_bigram_mixed_lang_10k_files", |b| {
        b.iter(|| engine.search_ast(black_box(&q)).unwrap())
    });
    group.finish();
}

// ============================================================================
// Bench: rare trigram (~1k/10k files match)
// ============================================================================

fn bench_rare_trigram(c: &mut Criterion) {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let expr_id = vocab_lookup("expression_statement").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);
    let trigram = AstTrigram::encode(fn_id, block_id, expr_id);

    let (_dir, reader) = build_index_with_rare_trigram(10_000, bigram, trigram);
    let engine = AstQueryEngine::new(reader);
    let q = parse_ast_query("function_item > block > expression_statement").unwrap();

    let mut group = c.benchmark_group("ast_query");
    group.sample_size(10);
    group.bench_function("rare_trigram_10k_files", |b| {
        b.iter(|| engine.search_ast(black_box(&q)).unwrap())
    });
    group.finish();
}

// ============================================================================
// Bench: multi-n-gram named pattern (try-catch)
// ============================================================================

fn bench_multi_ngram_pattern(c: &mut Criterion) {
    // Build index with the try-catch bigram: try_statement → catch_clause.
    let try_id = vocab_lookup("try_statement").unwrap();
    let catch_id = vocab_lookup("catch_clause").unwrap();
    let bigram = AstBigram::encode(try_id, catch_id);

    let (_dir, reader) = build_index_with_bigram(10_000, bigram);
    let engine = AstQueryEngine::new(reader);
    let q = parse_ast_query("try-catch").unwrap();

    let mut group = c.benchmark_group("ast_query");
    group.sample_size(10);
    group.bench_function("multi_ngram_pattern_10k_files", |b| {
        b.iter(|| engine.search_ast(black_box(&q)).unwrap())
    });
    group.finish();
}

// ============================================================================
// Bench: multi-n-gram with bigram+trigram overlap (exercises meta_cache)
//
// Every file has both the bigram and trigram, so scoring them both exercises
// the cross-n-gram meta_cache hit path. This scenario was absent from the
// original bench, masking the meta_cache cost.
// ============================================================================

fn bench_multi_ngram_overlap(c: &mut Criterion) {
    let fn_id = vocab_lookup("function_item").unwrap();
    let block_id = vocab_lookup("block").unwrap();
    let expr_id = vocab_lookup("expression_statement").unwrap();
    let bigram = AstBigram::encode(fn_id, block_id);
    let trigram = AstTrigram::encode(fn_id, block_id, expr_id);

    let (_dir, reader) = build_index_with_bigram_and_trigram(10_000, bigram, trigram);
    let engine = AstQueryEngine::new(reader);

    // Build a query set with both the bigram and trigram.
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

    let mut group = c.benchmark_group("ast_query");
    group.sample_size(10);
    group.bench_function("multi_ngram_overlap_10k_files", |b| {
        b.iter(|| engine.search_ast(black_box(&q)).unwrap())
    });
    group.finish();
}

// ============================================================================
// Criterion main
// ============================================================================

criterion_group!(
    benches,
    bench_hot_bigram,
    bench_hot_bigram_mixed_lang,
    bench_rare_trigram,
    bench_multi_ngram_pattern,
    bench_multi_ngram_overlap,
);
criterion_main!(benches);
