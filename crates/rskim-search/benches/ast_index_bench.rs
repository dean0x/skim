//! Criterion benchmarks for AST n-gram on-disk index: build + query.
//!
//! Run with: cargo bench -p rskim-search --bench ast_index_bench
//!
//! Benchmark groups:
//!   1. build_1000_files       — build_from_files over ~1000 Rust functions (A15)
//!   2. extraction_overhead    — compare extract_ast_ngrams (v1) vs
//!                               extract_ast_ngrams_with_metrics (v2) on a
//!                               representative linearized corpus. Empirically
//!                               backs P1 (extraction overhead <15%).
//!
//! A16 (index size ratio < 2.2×) is a normal unit test in reader_tests.rs.
//! Measured baseline: ~1.23× (v1), ~1.3× (v2 with structural markers).
//! Bound < 2.2× absorbs the deliberate v2 capability expansion while still
//! catching genuine O(files²) bloat regressions.  On-disk compression
//! (delta + VarInt / Roaring posting) tracked in issue #273.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use rskim_core::Language;
use rskim_search::{
    AstIndexBuilder, FileId,
    ast_index::{LinearNode, extract_ast_ngrams, extract_ast_ngrams_with_metrics, linearize_source},
};
use tempfile::TempDir;

// ============================================================================
// Fixture helpers (reused from linearize_bench patterns)
// ============================================================================

/// Generate a Rust source file with `n` simple functions.
fn gen_rust_fns(n: usize) -> String {
    (0..n)
        .map(|i| format!("pub fn func_{i}(x: i32, y: i32) -> i32 {{ x + y + {i} }}\n"))
        .collect()
}

// ============================================================================
// Group 1: Build 1000 files (A15: < 10s)
// ============================================================================

fn bench_build_1000_files(c: &mut Criterion) {
    let mut group = c.benchmark_group("ast_index_build");
    group.sample_size(10); // Each iter is expensive — 10 samples sufficient

    // Pre-generate 1000 source strings outside the timed closure.
    let sources: Vec<String> = (0..1000).map(|_| gen_rust_fns(1)).collect();

    group.bench_function("build_1000_rust_files", |b| {
        b.iter_batched(
            || {
                // Setup closure: create a fresh temp dir per iteration
                let dir = TempDir::new().unwrap();
                let files: Vec<(FileId, String, Language)> = sources
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (FileId(i as u32), s.clone(), Language::Rust))
                    .collect();
                (dir, files)
            },
            |(dir, files)| {
                // Timed closure: parallel build
                let file_refs: Vec<(FileId, &str, Language)> = files
                    .iter()
                    .map(|(id, s, lang)| (*id, s.as_str(), *lang))
                    .collect();
                AstIndexBuilder::build_from_files(
                    black_box(dir.path().to_path_buf()),
                    black_box(&file_refs),
                )
                .unwrap();
                dir // drop after timing
            },
            criterion::BatchSize::SmallInput,
        )
    });

    group.finish();
}

// ============================================================================
// Group 2: Extraction overhead — v1 vs v2 path (P1: < 15% overhead)
// ============================================================================

fn bench_extraction_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("ast_extraction_overhead");

    // Generate a representative Rust source once, linearize it once outside
    // the timed closure so both benches measure only the extraction step.
    let source = gen_rust_fns(50); // ~50 functions — representative corpus
    let nodes: Vec<LinearNode> = linearize_source(&source, Language::Rust)
        .expect("linearize failed")
        .nodes;

    // v1 path: extract_ast_ngrams discards metrics
    group.bench_function("extract_v1_no_metrics", |b| {
        b.iter(|| extract_ast_ngrams(black_box(&nodes), black_box(Language::Rust)))
    });

    // v2 path: extract_ast_ngrams_with_metrics returns StructuralMetrics too
    group.bench_function("extract_v2_with_metrics", |b| {
        b.iter(|| {
            extract_ast_ngrams_with_metrics(black_box(&nodes), black_box(Language::Rust))
        })
    });

    group.finish();
}

// ============================================================================
// Criterion main
// ============================================================================

criterion_group!(benches, bench_build_1000_files, bench_extraction_overhead);
criterion_main!(benches);
