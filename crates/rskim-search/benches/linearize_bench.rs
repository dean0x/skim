//! Criterion benchmarks for CST linearization.
//!
//! Run with: cargo bench -p rskim-search --bench linearize_bench
//!
//! Benchmark groups:
//!   1. linearize_languages   — 14 tree-sitter languages, fixed fixture
//!   2. linearize_scaling     — Rust, 10/50/100/500/1000 functions
//!   3. linearize_depth       — controlled nesting: 5/10/50/100/200 levels
//!   4. init_latency          — LazyLock LANG_MAPS initialization

#![allow(clippy::unwrap_used, clippy::expect_used)]

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use rskim_core::Language;
use rskim_search::linearize_source;

// ============================================================================
// Fixture helpers
// ============================================================================

/// Generate a Rust source file with `n` simple functions.
fn gen_rust_fns(n: usize) -> String {
    (0..n)
        .map(|i| format!("pub fn func_{i}(x: i32, y: i32) -> i32 {{ x + y + {i} }}\n"))
        .collect()
}

/// Generate a Rust source file with a single function nested `depth` levels deep.
fn gen_rust_nested(depth: usize) -> String {
    let open: String = "{ if true ".repeat(depth);
    let body = "let x = 1;";
    let close: String = " }".repeat(depth);
    format!("fn nested() {open}{body}{close}")
}

/// Inline fixtures for languages without standard fixture files.
const RUST_FIXTURE: &str = include_str!("../../../tests/fixtures/rust/simple.rs");
const TS_FIXTURE: &str = include_str!("../../../tests/fixtures/typescript/simple.ts");
const PY_FIXTURE: &str = include_str!("../../../tests/fixtures/python/simple.py");
const GO_FIXTURE: &str = "package main\nimport \"fmt\"\nfunc main() { fmt.Println(\"hello\") }\n";
const JAVA_FIXTURE: &str = "public class Foo { public static void main(String[] args) { System.out.println(\"hi\"); } }";
const C_FIXTURE: &str = "#include <stdio.h>\nint main() { printf(\"hi\"); return 0; }\n";
const CPP_FIXTURE: &str = "#include <iostream>\nint main() { std::cout << \"hi\"; return 0; }\n";
const JS_FIXTURE: &str = "function foo(x) { return x + 1; }\nconst bar = (y) => y * 2;\n";
const CS_FIXTURE: &str = "using System;\nclass Foo { static void Main() { Console.WriteLine(\"hi\"); } }\n";
const RUBY_FIXTURE: &str = "def greet(name)\n  puts \"Hello, #{name}\"\nend\n";
const SQL_FIXTURE: &str = "SELECT id, name FROM users WHERE active = 1 ORDER BY name;\n";
const KOTLIN_FIXTURE: &str = "fun greet(name: String): String = \"Hello, $name\"\n";
const SWIFT_FIXTURE: &str = "func greet(name: String) -> String { return \"Hello, \\(name)\" }\n";
const MD_FIXTURE: &str = "# Hello\n\nThis is a **paragraph** with `code`.\n\n## Section\n\nMore text.\n";

// ============================================================================
// Group 1: Per-language linearization
// ============================================================================

fn bench_linearize_languages(c: &mut Criterion) {
    let mut group = c.benchmark_group("linearize_languages");

    let fixtures: &[(&str, Language, &str)] = &[
        ("Rust", Language::Rust, RUST_FIXTURE),
        ("TypeScript", Language::TypeScript, TS_FIXTURE),
        ("Python", Language::Python, PY_FIXTURE),
        ("Go", Language::Go, GO_FIXTURE),
        ("Java", Language::Java, JAVA_FIXTURE),
        ("C", Language::C, C_FIXTURE),
        ("Cpp", Language::Cpp, CPP_FIXTURE),
        ("JavaScript", Language::JavaScript, JS_FIXTURE),
        ("CSharp", Language::CSharp, CS_FIXTURE),
        ("Ruby", Language::Ruby, RUBY_FIXTURE),
        ("Sql", Language::Sql, SQL_FIXTURE),
        ("Kotlin", Language::Kotlin, KOTLIN_FIXTURE),
        ("Swift", Language::Swift, SWIFT_FIXTURE),
        ("Markdown", Language::Markdown, MD_FIXTURE),
    ];

    for &(name, lang, source) in fixtures {
        group.bench_with_input(BenchmarkId::from_parameter(name), source, |b, src| {
            b.iter(|| linearize_source(black_box(src), black_box(lang)).unwrap())
        });
    }

    group.finish();
}

// ============================================================================
// Group 2: Scaling by number of functions
// ============================================================================

fn bench_linearize_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("linearize_scaling");

    for &n_fns in &[10usize, 50, 100, 500, 1000] {
        let source = gen_rust_fns(n_fns);
        // Benchmarks end-to-end linearize_source, including parsing overhead.
        group.bench_with_input(
            BenchmarkId::new("rust_fns", n_fns),
            &source,
            |b, src| {
                b.iter(|| linearize_source(black_box(src.as_str()), black_box(Language::Rust)).unwrap())
            },
        );
    }

    group.finish();
}

// ============================================================================
// Group 3: Nesting depth
// ============================================================================

fn bench_linearize_depth(c: &mut Criterion) {
    let mut group = c.benchmark_group("linearize_depth");

    for &depth in &[5usize, 10, 50, 100, 200] {
        let source = gen_rust_nested(depth);
        group.bench_with_input(
            BenchmarkId::new("rust_nested_depth", depth),
            &source,
            |b, src| {
                b.iter(|| linearize_source(black_box(src.as_str()), black_box(Language::Rust)).unwrap())
            },
        );
    }

    group.finish();
}

// ============================================================================
// Group 4: LazyLock initialization latency
// ============================================================================

fn bench_init_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("init_latency");

    // The LazyLock is already initialized by previous benchmarks, but we
    // benchmark the steady-state cost of accessing it (one atomic load).
    group.bench_function("lang_maps_access", |b| {
        b.iter(|| {
            // Access LANG_MAPS through linearize_source to measure end-to-end
            // cost once it's warm. Use empty source to minimize parse work.
            linearize_source(black_box(""), black_box(Language::Rust)).unwrap()
        })
    });

    group.finish();
}

// ============================================================================
// Criterion main
// ============================================================================

criterion_group!(
    benches,
    bench_linearize_languages,
    bench_linearize_scaling,
    bench_linearize_depth,
    bench_init_latency
);
criterion_main!(benches);
