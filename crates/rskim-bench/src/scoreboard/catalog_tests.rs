//! Unit tests for `catalog.rs` (co-located file, `#[path]`-included): the
//! structural oracle crossed with skim's pattern catalog. Everything that
//! needs the catalog lives here, since the oracle crate never reads it.

use std::collections::BTreeSet;

use rskim_oracle::structural::{
    FileMatches, FileReport, INTENTS, LangClass, OracleLang, OracleScratch, PatternCoverage,
    UNCLASSIFIED_REASON, UNCOVERED, extension_classes, query_sources,
};

use super::*;
use crate::scoreboard::test_support::{catalog, oracle};

const TS_FAMILY: [OracleLang; 3] = [
    OracleLang::TypeScript,
    OracleLang::Tsx,
    OracleLang::JavaScript,
];

fn catalog_example(pattern: &str) -> &'static str {
    catalog()
        .iter()
        .find(|p| p.name == pattern)
        .map(|p| p.example)
        .unwrap_or_else(|| panic!("{pattern} is not a catalog pattern"))
}

/// The oracle's answers for `source` as a `lang` file, through the per-file
/// path the scoreboard runs (a path with `lang`'s first extension in the
/// oracle's own table).
fn report(lang: OracleLang, source: &str) -> FileReport {
    let ext = extension_classes()
        .find(|&(_, class)| class == LangClass::Oracle(lang))
        .map(|(ext, _)| ext)
        .unwrap_or_else(|| panic!("no extension for {lang}"));
    match oracle().file_matches(&mut OracleScratch::new(), &format!("fixture.{ext}"), source) {
        Ok(FileMatches::Scored(report)) => report,
        other => panic!("fixture.{ext} is not scored: {other:?}"),
    }
}

fn lines(pattern: &str, lang: OracleLang, source: &str) -> Vec<u32> {
    report(lang, source)
        .definition
        .remove(pattern)
        .unwrap_or_else(|| panic!("no {lang} query for {pattern}"))
}

fn intent(pattern: &str, lang: OracleLang, source: &str) -> Vec<u32> {
    report(lang, source)
        .intent
        .remove(pattern)
        .unwrap_or_else(|| panic!("no {lang} intent oracle for {pattern}"))
}

/// Whether `source` parses without an ERROR or MISSING node under `lang`'s
/// grammar (the fixtures are meant to be valid code).
fn parses_cleanly(lang: OracleLang, source: &str) -> bool {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang.grammar()).unwrap();
    !parser.parse(source, None).unwrap().root_node().has_error()
}

// ============================================================================
// Coverage: every catalog pattern is covered or uncovered with a reason
// ============================================================================

#[test]
fn every_catalog_pattern_is_covered_or_uncovered_with_a_reason() {
    let names: BTreeSet<&str> = catalog().iter().map(|p| p.name).collect();
    let coverage = catalog_coverage(catalog());
    assert_eq!(coverage.keys().copied().collect::<BTreeSet<_>>(), names);

    for (name, cov) in &coverage {
        match cov {
            PatternCoverage::Covered { langs } => assert!(!langs.is_empty(), "{name}"),
            PatternCoverage::Uncovered { reason } => {
                assert!(!reason.trim().is_empty(), "{name} has an empty reason");
                assert_ne!(
                    *reason, UNCLASSIFIED_REASON,
                    "catalog pattern {name} has no oracle query and no recorded reason"
                );
            }
        }
    }
    // The oracle names no pattern the catalog does not have.
    for q in query_sources() {
        assert!(
            names.contains(q.pattern),
            "query for unknown pattern {}",
            q.pattern
        );
    }
    for (name, _) in UNCOVERED {
        assert!(
            names.contains(name),
            "uncovered entry for unknown pattern {name}"
        );
    }
    for spec in INTENTS {
        assert!(
            names.contains(spec.pattern),
            "intent for unknown pattern {}",
            spec.pattern
        );
    }
}

#[test]
fn uncovered_patterns_are_the_three_that_cannot_be_encoded() {
    let uncovered: Vec<&str> = catalog_coverage(catalog())
        .into_iter()
        .filter(|(_, c)| matches!(c, PatternCoverage::Uncovered { .. }))
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        uncovered,
        ["deep-nesting", "java-synchronized", "ruby-begin-rescue"]
    );
}

#[test]
fn a_catalog_pattern_newer_than_the_oracle_reads_as_unclassified() {
    let fake = [
        CatalogPattern {
            name: "try-catch",
            exact: true,
            example: "",
        },
        CatalogPattern {
            name: "deep-nesting",
            exact: false,
            example: "",
        },
        CatalogPattern {
            name: "brand-new-pattern",
            exact: false,
            example: "",
        },
    ];
    let coverage = catalog_coverage(&fake);
    assert_eq!(
        coverage.keys().copied().collect::<Vec<_>>(),
        ["brand-new-pattern", "deep-nesting", "try-catch"]
    );
    assert_eq!(
        coverage["brand-new-pattern"],
        PatternCoverage::Uncovered {
            reason: UNCLASSIFIED_REASON
        }
    );
    assert!(matches!(
        coverage["deep-nesting"],
        PatternCoverage::Uncovered { reason } if reason != UNCLASSIFIED_REASON
    ));
    assert!(matches!(
        coverage["try-catch"],
        PatternCoverage::Covered { .. }
    ));
}

// ============================================================================
// AC-1: every query matches its catalog example and rejects a near-miss
// ============================================================================

/// Where a query's positive fixture comes from.
#[derive(Clone, Copy)]
enum Positive {
    /// The pattern's catalog `example`, verbatim (via [`skim_catalog`]).
    Catalog,
    /// A hand fixture mirroring the catalog example, for a language the
    /// example is not written in.
    Hand(&'static str),
}

struct Fixture {
    pattern: &'static str,
    lang: OracleLang,
    positive: Positive,
    expected: &'static [u32],
    near_miss: &'static str,
}

const GOD_FUNCTION_19: &str = "fn big() { let a=1; let b=2; let c=3; let d=4; let e=5; let f=6; \
     let g=7; let h=8; let i=9; let j=10; let k=11; let l=12; let m=13; let n=14; let o=15; \
     let p=16; let q=17; let r=18; let s=19; }";

/// TypeScript-family fixtures: the catalog examples are valid TypeScript,
/// TSX and JavaScript alike.
const TS_FAMILY_FIXTURES: &[(&str, Positive, &[u32], &str)] = &[
    (
        "try-catch",
        Positive::Catalog,
        &[1],
        "try { open(); } finally { close(); }",
    ),
    (
        "try-finally",
        Positive::Catalog,
        &[1],
        "try { f(); } catch (e) { g(); }",
    ),
    (
        "empty-catch",
        Positive::Catalog,
        &[1],
        "try { f(); } catch (e) { g(); }",
    ),
    (
        "try-catch-finally",
        Positive::Catalog,
        &[1],
        "try { a(); } catch (e) { b(); }\ntry { c(); } finally { d(); }",
    ),
    (
        "nested-loop",
        Positive::Catalog,
        &[1],
        "for (let i = 0; i < n; i++) { work(i); }",
    ),
    (
        "call-in-loop",
        Positive::Catalog,
        &[1],
        "for (let i = 0; i < n; i++) { process(i); }",
    ),
    (
        "method-with-body",
        Positive::Catalog,
        &[1],
        "class C { foo = () => 1; }",
    ),
    (
        "unhandled-result",
        Positive::Catalog,
        &[1],
        "const r = doSomething();",
    ),
    (
        "switch-with-cases",
        Positive::Catalog,
        &[1],
        "if (x === 1) { other(); }",
    ),
    (
        "class-method",
        Positive::Catalog,
        &[1],
        "const Foo = class { bar() { return 1; } };",
    ),
    (
        "ternary-expression",
        Positive::Catalog,
        &[1],
        "function f() { return flag ? a : b; }",
    ),
    (
        "numeric-literal-in-expression",
        Positive::Catalog,
        &[1],
        "const result = 42;",
    ),
];

fn fixtures() -> Vec<Fixture> {
    let mut out: Vec<Fixture> = TS_FAMILY_FIXTURES
        .iter()
        .flat_map(|&(pattern, positive, expected, near_miss)| {
            TS_FAMILY.into_iter().map(move |lang| Fixture {
                pattern,
                lang,
                positive,
                expected,
                near_miss,
            })
        })
        .collect();
    out.extend([
        Fixture {
            pattern: "try-finally",
            lang: OracleLang::Python,
            positive: Positive::Hand("try:\n    open_file()\nfinally:\n    close_file()\n"),
            expected: &[1],
            near_miss: "try:\n    f()\nexcept Exception:\n    g()\n",
        },
        Fixture {
            pattern: "python-try-except",
            lang: OracleLang::Python,
            positive: Positive::Catalog,
            expected: &[1],
            near_miss: "try:\n    f()\nfinally:\n    g()\n",
        },
        Fixture {
            pattern: "python-nested-loop",
            lang: OracleLang::Python,
            positive: Positive::Catalog,
            expected: &[2],
            near_miss: "for i in range(n):\n    work(i)\n",
        },
        Fixture {
            pattern: "unhandled-result",
            lang: OracleLang::Rust,
            positive: Positive::Hand("fn main() {\n    do_something();\n}\n"),
            expected: &[2],
            near_miss: "fn main() {\n    let r = do_something();\n}\n",
        },
        Fixture {
            pattern: "unhandled-result",
            lang: OracleLang::Go,
            positive: Positive::Hand("package main\n\nfunc main() {\n\tdoSomething()\n}\n"),
            expected: &[4],
            near_miss: "package main\n\nfunc main() {\n\tr := doSomething()\n\t_ = r\n}\n",
        },
        Fixture {
            pattern: "rust-nested-loop",
            lang: OracleLang::Rust,
            positive: Positive::Catalog,
            expected: &[1],
            // The definition matches ANY for loop that is a statement of a
            // block (the catalog says so), so its near-miss is a for loop in
            // expression position; the un-nested-loop near-miss is asserted on
            // the intent oracle below.
            near_miss: "fn f() { let _ = for i in 0..n { work(i); }; }",
        },
        Fixture {
            pattern: "rust-unsafe-block",
            lang: OracleLang::Rust,
            positive: Positive::Catalog,
            expected: &[1],
            near_miss: "unsafe fn write_raw(ptr: *mut i32, val: i32) { ptr.write(val); }",
        },
        Fixture {
            pattern: "function-with-body",
            lang: OracleLang::Rust,
            positive: Positive::Catalog,
            expected: &[1],
            near_miss: "trait T { fn foo(); }",
        },
        Fixture {
            pattern: "match-with-arms",
            lang: OracleLang::Rust,
            positive: Positive::Catalog,
            expected: &[1],
            near_miss: "fn check(x: Never) -> ! { match x {} }",
        },
        Fixture {
            pattern: "empty-function",
            lang: OracleLang::Rust,
            positive: Positive::Catalog,
            expected: &[1],
            near_miss: "fn f() { g(); }",
        },
        Fixture {
            pattern: "god-function",
            lang: OracleLang::Rust,
            positive: Positive::Catalog,
            expected: &[1],
            near_miss: GOD_FUNCTION_19,
        },
        Fixture {
            pattern: "excessive-params",
            lang: OracleLang::Rust,
            positive: Positive::Catalog,
            expected: &[1],
            near_miss: "fn few(a: i32, b: i32, c: i32, d: i32) -> i32 { a + b + c + d }",
        },
        Fixture {
            pattern: "impl-method",
            lang: OracleLang::Rust,
            positive: Positive::Catalog,
            expected: &[1],
            near_miss: "impl Foo { const X: i32 = 1; }",
        },
        Fixture {
            pattern: "go-goroutine",
            lang: OracleLang::Go,
            positive: Positive::Catalog,
            expected: &[2],
            near_miss: "package main\nfunc serve() { handle(conn) }",
        },
        Fixture {
            pattern: "go-defer",
            lang: OracleLang::Go,
            positive: Positive::Catalog,
            expected: &[2],
            near_miss: "package main\nfunc run() { cleanup() }",
        },
        Fixture {
            pattern: "go-channel-send",
            lang: OracleLang::Go,
            positive: Positive::Catalog,
            expected: &[2],
            near_miss: "package main\nfunc recv(ch chan string) string { return <-ch }",
        },
        Fixture {
            pattern: "go-select",
            lang: OracleLang::Go,
            positive: Positive::Catalog,
            expected: &[2],
            near_miss: "package main\nfunc f(x int) { switch x { case 1: g() } }",
        },
    ]);
    out
}

#[test]
fn fixtures_cover_every_registered_query_exactly_once() {
    let fixtures = fixtures();
    let keys: Vec<(&str, OracleLang)> = fixtures.iter().map(|f| (f.pattern, f.lang)).collect();
    let unique: BTreeSet<(&str, OracleLang)> = keys.iter().copied().collect();
    assert_eq!(
        keys.len(),
        unique.len(),
        "a (pattern, lang) has two fixtures"
    );
    let registered: BTreeSet<(&str, OracleLang)> = query_sources()
        .iter()
        .map(|q| (q.pattern, q.lang))
        .collect();
    assert_eq!(unique, registered);
}

#[test]
fn every_query_matches_its_catalog_example_and_rejects_its_near_miss() {
    for f in fixtures() {
        let positive = match f.positive {
            Positive::Catalog => catalog_example(f.pattern),
            Positive::Hand(src) => src,
        };
        let id = format!("{}.{}", f.pattern, f.lang);
        // A fixture that does not parse would prove nothing.
        assert!(
            parses_cleanly(f.lang, positive),
            "{id}: positive fixture has syntax errors"
        );
        assert!(
            parses_cleanly(f.lang, f.near_miss),
            "{id}: near-miss has syntax errors"
        );
        assert_eq!(
            lines(f.pattern, f.lang, positive),
            f.expected,
            "{id} positive"
        );
        assert_eq!(
            lines(f.pattern, f.lang, f.near_miss),
            Vec::<u32>::new(),
            "{id} near-miss"
        );
    }
}

// ============================================================================
// The nested-loop intent oracles on the catalog examples
// ============================================================================

#[test]
fn the_nested_loop_catalog_examples_are_nested_by_intent_too() {
    for lang in TS_FAMILY {
        assert_eq!(
            intent("nested-loop", lang, catalog_example("nested-loop")),
            [1],
            "{lang}"
        );
    }
    assert_eq!(
        intent(
            "rust-nested-loop",
            OracleLang::Rust,
            catalog_example("rust-nested-loop")
        ),
        [1]
    );
}

#[test]
fn the_god_function_catalog_example_counts_as_a_method_too() {
    let method = format!("impl S {{\n    {}\n}}\n", catalog_example("god-function"));
    assert_eq!(lines("god-function", OracleLang::Rust, &method), [2]);
}
