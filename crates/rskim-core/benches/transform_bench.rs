//! Performance benchmarks for skim transformations
//!
//! Run with: cargo bench

#![allow(clippy::unwrap_used)] // Unwrapping is acceptable in benchmarks

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use rskim_core::{transform, Language, Mode};

// ============================================================================
// Benchmark Fixtures
// ============================================================================

const SMALL_TS: &str = include_str!("../../../tests/fixtures/typescript/simple.ts");
const SMALL_PY: &str = include_str!("../../../tests/fixtures/python/simple.py");
const SMALL_RS: &str = include_str!("../../../tests/fixtures/rust/simple.rs");
const SMALL_GO: &str = include_str!("../../../tests/fixtures/go/simple.go");
const SMALL_JAVA: &str = include_str!("../../../tests/fixtures/java/Simple.java");

// Medium complexity TypeScript
const MEDIUM_TS: &str = include_str!("../../../tests/fixtures/typescript/types.ts");

// Generate large file for stress testing
fn generate_large_typescript(num_functions: usize) -> String {
    let mut result = String::with_capacity(num_functions * 100);
    for i in 0..num_functions {
        result.push_str(&format!(
            "export function func{i}(a: number, b: number): number {{\n    return a + b;\n}}\n\n",
            i = i
        ));
    }
    result
}

// ============================================================================
// Structure Mode Benchmarks
// ============================================================================

fn bench_structure_mode(c: &mut Criterion) {
    let mut group = c.benchmark_group("structure_mode");

    // TypeScript
    group.bench_function("typescript_small", |b| {
        b.iter(|| transform(black_box(SMALL_TS), Language::TypeScript, Mode::Structure).unwrap())
    });

    group.bench_function("typescript_medium", |b| {
        b.iter(|| transform(black_box(MEDIUM_TS), Language::TypeScript, Mode::Structure).unwrap())
    });

    // Python
    group.bench_function("python_small", |b| {
        b.iter(|| transform(black_box(SMALL_PY), Language::Python, Mode::Structure).unwrap())
    });

    // Rust
    group.bench_function("rust_small", |b| {
        b.iter(|| transform(black_box(SMALL_RS), Language::Rust, Mode::Structure).unwrap())
    });

    // Go
    group.bench_function("go_small", |b| {
        b.iter(|| transform(black_box(SMALL_GO), Language::Go, Mode::Structure).unwrap())
    });

    // Java
    group.bench_function("java_small", |b| {
        b.iter(|| transform(black_box(SMALL_JAVA), Language::Java, Mode::Structure).unwrap())
    });

    group.finish();
}

// ============================================================================
// Signatures Mode Benchmarks
// ============================================================================

fn bench_signatures_mode(c: &mut Criterion) {
    let mut group = c.benchmark_group("signatures_mode");

    group.bench_function("typescript_small", |b| {
        b.iter(|| transform(black_box(SMALL_TS), Language::TypeScript, Mode::Signatures).unwrap())
    });

    group.bench_function("python_small", |b| {
        b.iter(|| transform(black_box(SMALL_PY), Language::Python, Mode::Signatures).unwrap())
    });

    group.bench_function("rust_small", |b| {
        b.iter(|| transform(black_box(SMALL_RS), Language::Rust, Mode::Signatures).unwrap())
    });

    group.finish();
}

// ============================================================================
// Types Mode Benchmarks
// ============================================================================

fn bench_types_mode(c: &mut Criterion) {
    let mut group = c.benchmark_group("types_mode");

    group.bench_function("typescript_medium", |b| {
        b.iter(|| transform(black_box(MEDIUM_TS), Language::TypeScript, Mode::Types).unwrap())
    });

    group.bench_function("rust_small", |b| {
        b.iter(|| transform(black_box(SMALL_RS), Language::Rust, Mode::Types).unwrap())
    });

    group.finish();
}

// ============================================================================
// Scaling Benchmarks (File Size)
// ============================================================================

fn bench_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("scaling");

    for size in [10, 50, 100, 500, 1000] {
        let large_ts = generate_large_typescript(size);

        group.bench_with_input(
            BenchmarkId::new("functions", size),
            &large_ts,
            |b, input| {
                b.iter(|| {
                    transform(black_box(input), Language::TypeScript, Mode::Structure).unwrap()
                })
            },
        );
    }

    group.finish();
}

// ============================================================================
// Mode Comparison Benchmarks
// ============================================================================

fn bench_mode_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("mode_comparison");

    for mode in [Mode::Structure, Mode::Signatures, Mode::Types, Mode::Full] {
        group.bench_with_input(
            BenchmarkId::new("typescript", format!("{:?}", mode)),
            &mode,
            |b, &mode| {
                b.iter(|| transform(black_box(SMALL_TS), Language::TypeScript, mode).unwrap())
            },
        );
    }

    group.finish();
}

// ============================================================================
// Language Comparison Benchmarks
// ============================================================================

fn bench_language_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("language_comparison");

    let languages = [
        (Language::TypeScript, SMALL_TS),
        (Language::Python, SMALL_PY),
        (Language::Rust, SMALL_RS),
        (Language::Go, SMALL_GO),
        (Language::Java, SMALL_JAVA),
    ];

    for (lang, source) in languages {
        group.bench_with_input(
            BenchmarkId::from_parameter(format!("{:?}", lang)),
            &source,
            |b, &input| b.iter(|| transform(black_box(input), lang, Mode::Structure).unwrap()),
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_structure_mode,
    bench_signatures_mode,
    bench_types_mode,
    bench_scaling,
    bench_mode_comparison,
    bench_language_comparison
);
criterion_main!(benches);
