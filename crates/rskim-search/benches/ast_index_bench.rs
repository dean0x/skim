//! Criterion benchmarks for AST n-gram on-disk index: build + query.
//!
//! Run with: cargo bench -p rskim-search --bench ast_index_bench
//!
//! Benchmark groups:
//!   1. build_1000_files  — build_from_files over ~1000 Rust functions (A15)
//!
//! A16 (index size ratio < 1.8×) is a normal unit test in reader_tests.rs.
//! Measured baseline: ~1.23×.  Bound < 1.8× leaves ~46% margin above baseline
//! while catching genuine O(files²) bloat regressions.  On-disk compression
//! (delta + VarInt / Roaring posting) tracked in issue #273.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use rskim_core::Language;
use rskim_search::{AstIndexBuilder, FileId};
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
// Criterion main
// ============================================================================

criterion_group!(benches, bench_build_1000_files);
criterion_main!(benches);
