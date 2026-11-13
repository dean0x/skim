//! skim's `--ast` pattern catalog as the scoreboard sees it (#541).
//!
//! [`skim_catalog`] returns the catalog as a frozen table of each pattern's
//! name, `exact` flag and example snippet, as the last skim build that had
//! `--ast` held them. The pipeline (`Inputs::load`) and `golden-gen --ast`
//! call it once and pass the result to everything that needs the catalog.
//!
//! The structural oracle (`rskim_oracle::structural`) never sees the catalog:
//! it knows its own registry by pattern name, and [`catalog_coverage`]
//! crosses the two, so a catalog pattern the oracle has no query for is listed
//! as uncovered instead of silently skipped.

use std::collections::BTreeMap;

use rskim_oracle::structural::{PatternCoverage, coverage_of};

/// One entry of skim's pattern catalog, as the scoreboard may see it. The
/// n-gram tables that encode how skim matches the pattern are left out, so
/// no oracle answer or score can depend on them (AC-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogPattern {
    /// The `--ast` pattern name.
    pub name: &'static str,
    /// skim's `exact` flag, from which `golden-gen` proposes an entry's
    /// precision class. No oracle answer or score reads it.
    pub exact: bool,
    /// The catalog's example snippet, which the oracle's fixture tests match.
    pub example: &'static str,
}

/// Shorthand for one row of [`CATALOG`].
const fn pattern(name: &'static str, exact: bool, example: &'static str) -> CatalogPattern {
    CatalogPattern {
        name,
        exact,
        example,
    }
}

/// The frozen catalog, in catalog order.
const CATALOG: [CatalogPattern; 29] = [
    pattern(
        "try-catch",
        true,
        "try { doWork(); } catch (e) { handle(e); }",
    ),
    pattern("try-finally", true, "try { open(); } finally { close(); }"),
    pattern(
        "python-try-except",
        true,
        "try:\n    do_work()\nexcept Exception as e:\n    handle(e)",
    ),
    pattern(
        "ruby-begin-rescue",
        false,
        "def call\n  do_work\nrescue => e\n  handle(e)\nend",
    ),
    pattern("empty-catch", true, "try { f(); } catch (e) {}"),
    pattern(
        "try-catch-finally",
        true,
        "try { f(); } catch (e) { g(); } finally { h(); }",
    ),
    pattern(
        "nested-loop",
        true,
        "for (let i=0; i<n; i++) { for (let j=0; j<m; j++) { work(i,j); } }",
    ),
    pattern(
        "rust-nested-loop",
        false,
        "fn outer() { for i in 0..n { for j in 0..m { work(i, j); } } }",
    ),
    pattern(
        "call-in-loop",
        false,
        "for (const x of items) { process(x); }",
    ),
    pattern(
        "deep-nesting",
        true,
        "function a() { if (x) { for (;;) { if (y) { doWork(); } } } }",
    ),
    pattern(
        "python-nested-loop",
        false,
        "for i in range(n):\n    for j in range(m):\n        work(i, j)",
    ),
    pattern(
        "go-goroutine",
        true,
        "package main\nfunc serve() { go handle(conn) }",
    ),
    pattern(
        "go-defer",
        true,
        "package main\nfunc run() { defer cleanup() }",
    ),
    pattern(
        "go-channel-send",
        true,
        "package main\nfunc send(ch chan string, msg string) { ch <- msg }",
    ),
    pattern(
        "go-select",
        true,
        "package main\nfunc recv(ch chan string) { select { case msg := <-ch: handle(msg) } }",
    ),
    pattern(
        "rust-unsafe-block",
        true,
        "fn write_raw(ptr: *mut i32, val: i32) { unsafe { *ptr = val; } }",
    ),
    pattern(
        "java-synchronized",
        true,
        "class C { void inc() { synchronized (this) { count++; } } }",
    ),
    pattern("function-with-body", true, "fn foo() { let x = 1; }"),
    pattern("method-with-body", true, "class C { foo() { return 1; } }"),
    pattern(
        "match-with-arms",
        true,
        "fn check(x: Result<i32,()>) -> i32 { match x { Ok(v) => v, Err(_) => 0 } }",
    ),
    pattern("empty-function", true, "fn todo_later() {}"),
    pattern(
        "god-function",
        true,
        "fn big() { let a=1; let b=2; let c=3; let d=4; let e=5; let f=6; let g=7; \
         let h=8; let i=9; let j=10; let k=11; let l=12; let m=13; let n=14; \
         let o=15; let p=16; let q=17; let r=18; let s=19; let t=20; }",
    ),
    pattern(
        "excessive-params",
        true,
        "fn many(a: i32, b: i32, c: i32, d: i32, e: i32) -> i32 { a + b + c + d + e }",
    ),
    pattern("unhandled-result", false, "doSomething();"),
    pattern(
        "switch-with-cases",
        true,
        "switch (x) { case 1: break; default: other(); }",
    ),
    pattern("impl-method", true, "impl Foo { fn bar(&self) { } }"),
    pattern("class-method", true, "class Foo { bar() { return 1; } }"),
    pattern("ternary-expression", false, "const x = flag ? a : b;"),
    pattern(
        "numeric-literal-in-expression",
        false,
        "const result = x * 42 + 7;",
    ),
];

/// skim's pattern catalog as [`CatalogPattern`] rows, in catalog order.
pub fn skim_catalog() -> Vec<CatalogPattern> {
    CATALOG.to_vec()
}

/// The structural oracle's coverage of every pattern in `catalog`, keyed by
/// pattern name. A pattern the oracle has neither a query nor a recorded
/// reason for reads as `UNCLASSIFIED_REASON`, never as absent.
pub fn catalog_coverage(catalog: &[CatalogPattern]) -> BTreeMap<&'static str, PatternCoverage> {
    catalog
        .iter()
        .map(|p| (p.name, coverage_of(p.name)))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code — fail loudly
#[path = "catalog_tests.rs"]
mod tests;
