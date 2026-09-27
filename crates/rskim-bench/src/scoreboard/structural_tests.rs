//! Unit tests for `structural.rs` (co-located file, `#[path]`-included).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use super::*;

/// Compiled once: `StructuralOracle` is `Sync`.
static ORACLE: LazyLock<StructuralOracle> =
    LazyLock::new(|| StructuralOracle::new().expect("every oracle query compiles"));

const TS_FAMILY: [OracleLang; 3] = [
    OracleLang::TypeScript,
    OracleLang::Tsx,
    OracleLang::JavaScript,
];

fn catalog_example(pattern: &str) -> &'static str {
    catalog_patterns()
        .find(|p| p.name == pattern)
        .map(|p| p.example)
        .unwrap_or_else(|| panic!("{pattern} is not a catalog pattern"))
}

fn lines(pattern: &str, lang: OracleLang, source: &str) -> Vec<u32> {
    ORACLE
        .match_lines(pattern, lang, source)
        .unwrap_or_else(|e| panic!("{pattern}.{lang}: {e:#}"))
}

fn intent(pattern: &str, lang: OracleLang, source: &str) -> Vec<u32> {
    ORACLE
        .intent_lines(pattern, lang, source)
        .unwrap_or_else(|e| panic!("{pattern} intent ({lang}): {e:#}"))
}

fn parses_cleanly(lang: OracleLang, source: &str) -> bool {
    !parse(lang, source).unwrap().root_node().has_error()
}

// ============================================================================
// Registry: compilation, files on disk, catalog coverage
// ============================================================================

#[test]
fn every_registered_query_compiles_for_its_grammar() {
    for spec in QUERIES {
        compile_definition(spec)
            .unwrap_or_else(|e| panic!("{}.{}: {e:#}", spec.pattern, spec.lang));
    }
    for spec in INTENTS {
        for &lang in spec.langs {
            compile_intent(spec, lang)
                .unwrap_or_else(|e| panic!("{} ({lang}): {e:#}", spec.pattern));
        }
    }
    assert!(StructuralOracle::new().is_ok());
}

#[test]
fn every_scm_file_on_disk_is_registered_exactly_once() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("scoreboard/structural");
    let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".scm"))
        .collect();
    let registered: Vec<String> = query_sources().iter().map(QuerySource::file_name).collect();
    let registered_set: BTreeSet<String> = registered.iter().cloned().collect();
    assert_eq!(
        registered.len(),
        registered_set.len(),
        "duplicate registration"
    );
    assert_eq!(on_disk, registered_set);
    // The include_str! path's language suffix and the OracleLang agree.
    for q in query_sources() {
        let from_disk = std::fs::read_to_string(dir.join(q.file_name())).unwrap();
        assert_eq!(
            from_disk,
            q.source,
            "{} is not the file it names",
            q.file_name()
        );
    }
}

#[test]
fn query_sources_are_ordered_by_pattern_then_language_name() {
    let keys: Vec<(&str, &str)> = query_sources()
        .iter()
        .map(|q| (q.pattern, q.lang.as_str()))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
    assert_eq!(keys.len(), 53);
    // The registry itself is kept in the same order, so a diff reads cleanly.
    let registry: Vec<(&str, &str)> = QUERIES
        .iter()
        .map(|q| (q.pattern, q.lang.as_str()))
        .collect();
    assert_eq!(registry, keys);
}

#[test]
fn every_catalog_pattern_is_covered_or_uncovered_with_a_reason() {
    let catalog: BTreeSet<&str> = catalog_patterns().map(|p| p.name).collect();
    let coverage = catalog_coverage();
    assert_eq!(coverage.keys().copied().collect::<BTreeSet<_>>(), catalog);

    for (name, cov) in &coverage {
        match cov {
            PatternCoverage::Covered { langs } => {
                assert!(!langs.is_empty(), "{name}");
                assert!(
                    UNCOVERED.iter().all(|(n, _)| n != name),
                    "{name} is both covered and uncovered"
                );
            }
            PatternCoverage::Uncovered { reason } => {
                assert!(!reason.trim().is_empty(), "{name} has an empty reason");
                assert_ne!(
                    *reason, UNCLASSIFIED_REASON,
                    "catalog pattern {name} has no oracle query and no recorded reason"
                );
            }
        }
    }
    for q in QUERIES {
        assert!(
            catalog.contains(q.pattern),
            "query for unknown pattern {}",
            q.pattern
        );
    }
    for (name, _) in UNCOVERED {
        assert!(
            catalog.contains(name),
            "uncovered entry for unknown pattern {name}"
        );
    }
    for spec in INTENTS {
        assert!(
            catalog.contains(spec.pattern),
            "intent for unknown pattern {}",
            spec.pattern
        );
    }
}

#[test]
fn uncovered_patterns_are_the_three_that_cannot_be_encoded() {
    let uncovered: Vec<&str> = catalog_coverage()
        .into_iter()
        .filter(|(_, c)| matches!(c, PatternCoverage::Uncovered { .. }))
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        uncovered,
        ["deep-nesting", "java-synchronized", "ruby-begin-rescue"]
    );
}

// ============================================================================
// AC-1: every query matches its catalog example and rejects a near-miss
// ============================================================================

/// Where a query's positive fixture comes from.
#[derive(Clone, Copy)]
enum Positive {
    /// The pattern's catalog `example`, verbatim (via [`catalog_patterns`]).
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

#[test]
fn try_catch_finally_rejects_separate_and_nested_clauses() {
    let separate_catch = "try { a(); } catch (e) {}";
    let separate_finally = "try { a(); } finally { b(); }";
    // npm/rskim/scripts/postinstall.js (#546): a try/finally wrapping a
    // separate try/catch — no single try carries both clauses.
    let nested = "try {\n  try {\n    a();\n  } catch (e) {}\n} finally {\n  b();\n}\n";
    for lang in TS_FAMILY {
        for src in [separate_catch, separate_finally, nested] {
            assert!(parses_cleanly(lang, src));
            assert_eq!(
                lines("try-catch-finally", lang, src),
                Vec::<u32>::new(),
                "{lang}: {src}"
            );
        }
        // Each two-clause pattern does see its own half of the nested shape.
        assert_eq!(lines("try-catch", lang, nested), [2]);
        assert_eq!(lines("try-finally", lang, nested), [1]);
    }
}

#[test]
fn tsx_files_are_parsed_with_the_tsx_grammar() {
    let src = "function View() {\n  try {\n    load();\n  } catch (e) {\n    report(e);\n  } \
               finally {\n    done();\n  }\n  return <div className=\"x\">{label}</div>;\n}\n";
    // The JSX parses under TSX and not under plain TypeScript.
    assert!(parses_cleanly(OracleLang::Tsx, src));
    assert!(!parses_cleanly(OracleLang::TypeScript, src));
    assert_eq!(lines("try-catch-finally", OracleLang::Tsx, src), [2]);

    // The per-file wrapper routes `.tsx` to the TSX grammar.
    match ORACLE.file_matches("src/View.tsx", src).unwrap() {
        FileMatches::Scored(report) => {
            assert_eq!(report.lang, OracleLang::Tsx);
            assert_eq!(report.definition["try-catch-finally"], [2]);
            assert_eq!(report.definition["try-catch"], [2]);
            assert_eq!(report.definition["method-with-body"], Vec::<u32>::new());
        }
        other => panic!("expected a scored file, got {other:?}"),
    }
}

// ============================================================================
// Encoded semantics beyond the catalog examples
// ============================================================================

#[test]
fn match_lines_are_one_based_sorted_and_deduplicated() {
    let src = "function f() {\n  try { g(); } catch (e) {}\n}\n\ntry {} catch (e) {} try {} catch (e) {}\n";
    assert_eq!(lines("try-catch", OracleLang::TypeScript, src), [2, 5]);
}

#[test]
fn files_with_syntax_errors_are_still_queried() {
    let src = "try {\n  a();\n} catch (e) {\n  b();\n}\nconst = ;\n";
    assert!(!parses_cleanly(OracleLang::JavaScript, src));
    assert_eq!(lines("try-catch", OracleLang::JavaScript, src), [1]);
}

#[test]
fn empty_body_counts_only_named_non_comment_non_attribute_elements() {
    for lang in TS_FAMILY {
        assert_eq!(
            lines(
                "empty-catch",
                lang,
                "try { f(); } catch (e) { /* ignore */ }"
            ),
            [1]
        );
        assert_eq!(
            lines("empty-catch", lang, "try { f(); } catch (e) { ; }"),
            Vec::<u32>::new()
        );
    }
    let rust = |src| lines("empty-function", OracleLang::Rust, src);
    assert_eq!(rust("fn f() {\n    // later\n}\n"), [1]);
    // A tail expression is a body element: `{ self }` is not empty.
    assert_eq!(
        rust("impl S { fn get(&self) -> &Self { self } }"),
        Vec::<u32>::new()
    );
    assert_eq!(rust("fn f() {\n    #![allow(unused)]\n}\n"), [1]);
}

#[test]
fn god_function_threshold_is_twenty_body_elements() {
    let god = |src| lines("god-function", OracleLang::Rust, src);
    // 19 statements plus a tail expression: 20 elements.
    let with_tail = GOD_FUNCTION_19.replace("let s=19; }", "let s=19; s }");
    assert_eq!(god(&with_tail), [1]);
    // An attribute or a comment is not an element.
    let with_attr = GOD_FUNCTION_19.replace("let s=19; }", "let s=19; #[allow(unused)] /* c */ }");
    assert!(parses_cleanly(OracleLang::Rust, &with_attr));
    assert_eq!(god(&with_attr), Vec::<u32>::new());
    // A method counts; a closure is not a function.
    let method = format!("impl S {{\n    {}\n}}\n", catalog_example("god-function"));
    assert_eq!(god(&method), [2]);
}

#[test]
fn excessive_params_counts_self_but_not_attributes_or_closures() {
    let params = |src| lines("excessive-params", OracleLang::Rust, src);
    assert_eq!(
        params("impl S { fn f(&self, a: u8, b: u8, c: u8, d: u8) {} }"),
        [1]
    );
    assert_eq!(
        params("trait T {\n    fn f(a: u8, b: u8, c: u8, d: u8, e: u8);\n}\n"),
        [2]
    );
    assert_eq!(
        params("fn f(#[cfg(x)] a: u8, b: u8, c: u8, d: u8) {}"),
        Vec::<u32>::new()
    );
    assert_eq!(
        params("fn f() { let g = |a: u8, b: u8, c: u8, d: u8, e: u8| a; }"),
        Vec::<u32>::new()
    );
    assert_eq!(
        params("type F = fn(u8, u8, u8, u8, u8);"),
        Vec::<u32>::new()
    );
}

#[test]
fn class_method_requires_a_class_declaration() {
    let src = "class A {\n  m() {}\n}\nabstract class B {\n  n() {}\n}\n";
    assert_eq!(lines("class-method", OracleLang::TypeScript, src), [2]);
    assert_eq!(lines("class-method", OracleLang::Tsx, src), [2]);
}

#[test]
fn go_constructs_match_whatever_their_operands() {
    let go = |pattern, src| lines(pattern, OracleLang::Go, src);
    // A send whose channel and value are selectors (no identifier child).
    assert_eq!(
        go(
            "go-channel-send",
            "package main\nfunc f(s *S) { s.ch <- s.v }"
        ),
        [2]
    );
    // A select with no communication case.
    assert_eq!(
        go("go-select", "package main\nfunc main() {\n\tselect {}\n}\n"),
        [3]
    );
    assert_eq!(
        go(
            "go-select",
            "package main\nfunc f() {\n\tselect {\n\tdefault:\n\t}\n}\n"
        ),
        [3]
    );
    assert_eq!(
        go("go-goroutine", "package main\nfunc f() { go func() {}() }"),
        [2]
    );
}

#[test]
fn impl_method_and_match_with_arms_need_their_innermost_node() {
    assert_eq!(
        lines(
            "impl-method",
            OracleLang::Rust,
            "impl Foo {}\nimpl Bar {\n    fn b() {}\n}\n"
        ),
        [3]
    );
    assert_eq!(
        lines(
            "match-with-arms",
            OracleLang::Rust,
            "fn f(x: u8) {\n    match x {\n        _ => {}\n    }\n}\n"
        ),
        [2]
    );
}

// ============================================================================
// Intent oracles
// ============================================================================

#[test]
fn nested_loop_intent_is_any_loop_in_a_loop_of_the_same_function() {
    let for_of_while = "function f() {\n  for (const a of xs) {\n    while (ok()) {\n      step();\n    }\n  }\n}\n";
    let through_if = "for (;;) {\n  if (x) {\n    for (;;) {}\n  }\n}\n";
    let do_in_for = "for (let i = 0; i < n; i++) {\n  do {\n    y();\n  } while (z);\n}\n";
    for lang in TS_FAMILY {
        assert_eq!(intent("nested-loop", lang, for_of_while), [3], "{lang}");
        assert_eq!(intent("nested-loop", lang, through_if), [3], "{lang}");
        assert_eq!(intent("nested-loop", lang, do_in_for), [2], "{lang}");
        // The definition oracle (for → block → for) sees none of them.
        for src in [for_of_while, through_if, do_in_for] {
            assert_eq!(
                lines("nested-loop", lang, src),
                Vec::<u32>::new(),
                "{lang}: {src}"
            );
        }
        // The catalog example is nested under both.
        assert_eq!(
            intent("nested-loop", lang, catalog_example("nested-loop")),
            [1]
        );
    }
}

#[test]
fn nested_loop_intent_stops_at_function_boundaries() {
    let arrow =
        "for (const a of xs) {\n  const g = () => {\n    for (let i = 0; i < 3; i++) {}\n  };\n}\n";
    let declaration = "while (x) {\n  function h() {\n    do { y(); } while (z);\n  }\n}\n";
    let expression = "while (x) {\n  const h = function () {\n    while (y) {}\n  };\n}\n";
    let method = "for (;;) {\n  const o = {\n    m() {\n      for (;;) {}\n    },\n  };\n}\n";
    let generator = "for (;;) {\n  function* g() {\n    while (y) {}\n  }\n}\n";
    let single = "for (let i = 0; i < n; i++) {\n  work(i);\n}\n";
    for lang in TS_FAMILY {
        for src in [arrow, declaration, expression, method, generator, single] {
            assert!(parses_cleanly(lang, src), "{lang}: {src}");
            assert_eq!(
                intent("nested-loop", lang, src),
                Vec::<u32>::new(),
                "{lang}: {src}"
            );
        }
    }
}

#[test]
fn rust_nested_loop_intent_is_any_loop_in_a_loop_of_the_same_function() {
    let rust = |src| intent("rust-nested-loop", OracleLang::Rust, src);
    assert_eq!(
        rust(
            "fn f() {\n    for i in 0..n {\n        loop {\n            break;\n        }\n    }\n}\n"
        ),
        [3]
    );
    assert_eq!(
        rust(
            "fn f() {\n    while a() {\n        if b() {\n            while c() {}\n        }\n    }\n}\n"
        ),
        [4]
    );
    assert_eq!(rust(catalog_example("rust-nested-loop")), [1]);
}

#[test]
fn rust_nested_loop_intent_stops_at_function_boundaries_and_rejects_a_single_loop() {
    let rust = |src| intent("rust-nested-loop", OracleLang::Rust, src);
    let closure = "fn f() {\n    for i in 0..n {\n        let g = || {\n            for j in 0..m {}\n        };\n    }\n}\n";
    let inner_fn = "fn f() {\n    while go() {\n        fn inner() {\n            loop {}\n        }\n    }\n}\n";
    let single = "fn f() {\n    for i in 0..n {\n        work(i);\n    }\n}\n";
    for src in [closure, inner_fn, single] {
        assert!(parses_cleanly(OracleLang::Rust, src), "{src}");
        assert_eq!(rust(src), Vec::<u32>::new(), "{src}");
    }
    // The definition oracle matches the single loop: the catalog says the
    // trigram "also matches any for loop inside a block".
    assert_eq!(lines("rust-nested-loop", OracleLang::Rust, single), [2]);
}

#[test]
fn unknown_pattern_or_language_is_an_error() {
    assert!(
        ORACLE
            .match_lines("no-such-pattern", OracleLang::Rust, "")
            .is_err()
    );
    assert!(
        ORACLE
            .match_lines("try-catch", OracleLang::Rust, "")
            .is_err()
    );
    assert!(
        ORACLE
            .match_lines("deep-nesting", OracleLang::TypeScript, "")
            .is_err()
    );
    assert!(
        ORACLE
            .intent_lines("nested-loop", OracleLang::Rust, "")
            .is_err()
    );
    assert!(
        ORACLE
            .intent_lines("try-catch", OracleLang::TypeScript, "")
            .is_err()
    );
}

// ============================================================================
// Language classification and the per-file wrapper
// ============================================================================

#[test]
fn classify_mirrors_skims_extension_table() {
    let oracle = [
        ("a.rs", OracleLang::Rust),
        ("a.py", OracleLang::Python),
        ("a.pyi", OracleLang::Python),
        ("a.ts", OracleLang::TypeScript),
        ("a.d.ts", OracleLang::TypeScript),
        ("a.mts", OracleLang::TypeScript),
        ("a.cts", OracleLang::TypeScript),
        ("a.tsx", OracleLang::Tsx),
        ("a.js", OracleLang::JavaScript),
        ("a.jsx", OracleLang::JavaScript),
        ("a.cjs", OracleLang::JavaScript),
        ("npm/rskim/scripts/postinstall.js", OracleLang::JavaScript),
        ("a.mjs", OracleLang::JavaScript),
        ("a.go", OracleLang::Go),
    ];
    for (path, lang) in oracle {
        assert_eq!(classify(path), LangClass::Oracle(lang), "{path}");
    }
    let unscored = [
        ("A.java", "java"),
        ("README.md", "markdown"),
        ("a.markdown", "markdown"),
        ("a.c", "c"),
        ("a.h", "c"),
        ("a.cpp", "cpp"),
        ("a.hh", "cpp"),
        ("a.cs", "csharp"),
        ("a.rb", "ruby"),
        ("a.sql", "sql"),
        ("a.kt", "kotlin"),
        ("a.kts", "kotlin"),
        ("a.swift", "swift"),
    ];
    for (path, language) in unscored {
        assert_eq!(classify(path), LangClass::Unscored { language }, "{path}");
    }
    assert_eq!(
        classify("run.sh"),
        LangClass::NotIndexed {
            language: Some("bash"),
            size_capped: true
        }
    );
    for (path, language) in [
        ("a.json", "json"),
        ("a.yml", "yaml"),
        ("Cargo.toml", "toml"),
    ] {
        assert_eq!(
            classify(path),
            LangClass::NotIndexed {
                language: Some(language),
                size_capped: false
            },
            "{path}"
        );
    }
    // Case-sensitive, like skim; no extension; unknown extension.
    for path in ["A.TS", "Makefile", "a.txt", ".gitignore"] {
        assert_eq!(
            classify(path),
            LangClass::NotIndexed {
                language: None,
                size_capped: false
            },
            "{path}"
        );
    }
}

#[test]
fn oracle_lang_names_round_trip() {
    for lang in OracleLang::ALL {
        assert_eq!(lang.as_str().parse::<OracleLang>().unwrap(), lang);
        assert_eq!(lang.to_string(), lang.as_str());
    }
    assert!("TypeScript".parse::<OracleLang>().is_err());
    assert!("java".parse::<OracleLang>().is_err());
}

#[test]
fn size_cap_is_one_mib_inclusive() {
    let cap = AST_SIZE_CAP_BYTES;
    assert_eq!(cap, 1_048_576);
    assert!(within_size_cap(cap));
    assert!(!within_size_cap(cap + 1));

    // Every language skim size-caps counts (Bash included); data formats and
    // unknown extensions never do.
    let files = [
        ("exact.rs", cap),
        ("over.rs", cap + 1),
        ("over.tsx", cap + 1),
        ("over.sh", cap + 1),
        ("over.java", cap + 1),
        ("over.json", cap + 1),
        ("over.toml", cap + 1),
        ("over.txt", cap + 1),
        ("small.py", 10),
    ];
    assert_eq!(over_cap_count(files), 4);
}

#[test]
fn file_matches_gates_on_the_size_cap_at_the_exact_boundary() {
    let head = "fn f() {}\n//";
    let len = usize::try_from(AST_SIZE_CAP_BYTES).unwrap();
    let exact = format!("{head}{}", "x".repeat(len - head.len()));
    assert_eq!(exact.len(), len);
    match ORACLE.file_matches("src/big.rs", &exact).unwrap() {
        FileMatches::Scored(report) => {
            assert_eq!(report.lang, OracleLang::Rust);
            assert_eq!(report.definition["empty-function"], [1]);
        }
        other => panic!("a file of exactly 1 MiB is in the AST universe, got {other:?}"),
    }
    let over = format!("{exact}x");
    assert_eq!(
        ORACLE.file_matches("src/big.rs", &over).unwrap(),
        FileMatches::OverSizeCap(OracleLang::Rust)
    );
}

#[test]
fn file_matches_reports_every_pattern_of_the_language() {
    let src = "fn f() {\n    for i in 0..n {\n        for j in 0..m {}\n    }\n}\n";
    let FileMatches::Scored(report) = ORACLE.file_matches("src/lib.rs", src).unwrap() else {
        panic!("a small .rs file is scored");
    };
    let expected: BTreeSet<&str> = QUERIES
        .iter()
        .filter(|q| q.lang == OracleLang::Rust)
        .map(|q| q.pattern)
        .collect();
    assert_eq!(
        report.definition.keys().copied().collect::<BTreeSet<_>>(),
        expected
    );
    assert_eq!(report.definition["rust-nested-loop"], [2, 3]);
    assert_eq!(report.definition["function-with-body"], [1]);
    assert_eq!(report.definition["god-function"], Vec::<u32>::new());
    assert_eq!(
        report.intent.keys().copied().collect::<Vec<_>>(),
        ["rust-nested-loop"]
    );
    assert_eq!(report.intent["rust-nested-loop"], [3]);

    assert_eq!(
        ORACLE.file_matches("A.java", "class A {}").unwrap(),
        FileMatches::NotScored(LangClass::Unscored { language: "java" })
    );
    assert_eq!(
        ORACLE.file_matches("x.json", "{}").unwrap(),
        FileMatches::NotScored(LangClass::NotIndexed {
            language: Some("json"),
            size_capped: false
        })
    );
}

// ============================================================================
// Golden fingerprint: every table an answer depends on
// ============================================================================

#[test]
fn the_fingerprint_renders_the_compiled_in_inputs() {
    assert_eq!(fingerprint(), render_fingerprint(&ORACLE_INPUTS));
    assert_eq!(fingerprint(), fingerprint());
}

/// [`EXT_CLASSES`] with `edit` applied to the row that lists `ext`.
fn edited_ext_table(ext: &str, edit: impl FnOnce(&mut ExtClass)) -> Vec<ExtClass> {
    let mut table = EXT_CLASSES.to_vec();
    let row = table
        .iter_mut()
        .find(|row| row.extensions.contains(&ext))
        .unwrap_or_else(|| panic!("no extension-table row lists {ext:?}"));
    edit(row);
    table
}

#[test]
fn any_extension_table_edit_changes_the_fingerprint() {
    let base = fingerprint();
    let with_table = |table: &[ExtClass]| {
        render_fingerprint(&OracleInputs {
            ext_classes: table,
            ..ORACLE_INPUTS
        })
    };
    let edits = [
        (
            "drop an extension",
            edited_ext_table("cts", |row| row.extensions = &["ts", "mts"]),
        ),
        (
            "parse .tsx with the TypeScript grammar",
            edited_ext_table("tsx", |row| {
                row.class = LangClass::Oracle(OracleLang::TypeScript);
            }),
        ),
        (
            "stop AST-indexing Java",
            edited_ext_table("java", |row| {
                row.class = LangClass::NotIndexed {
                    language: Some("java"),
                    size_capped: true,
                };
            }),
        ),
        (
            "rename an unscored language",
            edited_ext_table("rb", |row| {
                row.class = LangClass::Unscored { language: "rbx" };
            }),
        ),
        (
            "stop counting Bash toward the size cap",
            edited_ext_table("sh", |row| {
                row.class = LangClass::NotIndexed {
                    language: Some("bash"),
                    size_capped: false,
                };
            }),
        ),
    ];
    let mut seen = BTreeSet::from([base]);
    for (what, table) in edits {
        assert!(
            seen.insert(with_table(&table)),
            "{what}: fingerprint unchanged"
        );
    }
    let dropped_row = &EXT_CLASSES[..EXT_CLASSES.len() - 1];
    assert!(seen.insert(with_table(dropped_row)), "drop a row");

    let unknown_counted = render_fingerprint(&OracleInputs {
        unknown_extension: LangClass::NotIndexed {
            language: None,
            size_capped: true,
        },
        ..ORACLE_INPUTS
    });
    assert!(seen.insert(unknown_counted), "the unknown-extension class");
}

#[test]
fn any_attribute_kind_edit_changes_the_fingerprint() {
    let base = fingerprint();
    let with_kinds = |kinds: &[&str]| {
        render_fingerprint(&OracleInputs {
            attribute_kinds: kinds,
            ..ORACLE_INPUTS
        })
    };
    assert_eq!(with_kinds(ATTRIBUTE_KINDS), base);
    let mut seen = BTreeSet::from([base]);
    for (what, kinds) in [
        ("drop a kind", &["attribute_item"][..]),
        (
            "add a kind",
            &["attribute_item", "inner_attribute_item", "macro_invocation"][..],
        ),
        ("no kinds", &[][..]),
    ] {
        assert!(
            seen.insert(with_kinds(kinds)),
            "{what}: fingerprint unchanged"
        );
    }
}

// ============================================================================
// AC-3: independence from skim's AST search internals
// ============================================================================
//
// A source scan over the structural scoring path: `structural.rs`,
// `structural_metrics.rs`, and every in-crate module they reach through a
// path, transitively. The set is computed from the sources at test time, so
// a new import widens the scan by itself. Skim's pattern catalog is read in
// exactly one place, `catalog_patterns`, whose body is pinned below: no other
// catalog field (the n-gram tables above all) can reach the scoreboard.

/// Where the scanned sources come from: the crate's `src/`, or an in-memory
/// tree in the scanner's own tests. Paths are relative to `src/`.
trait Sources {
    fn exists(&self, rel: &str) -> bool;
    fn read(&self, rel: &str) -> String;
}

/// `crates/rskim-bench/src/` on disk. Existence is looked up in the directory
/// listing, not with `is_file`, so it is exact about case on a
/// case-insensitive file system (where `Baseline.rs` would find `baseline.rs`).
struct CrateSources {
    files: BTreeSet<String>,
}

impl CrateSources {
    fn new() -> Self {
        Self {
            files: crate_source_files().into_iter().collect(),
        }
    }
}

fn src_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

impl Sources for CrateSources {
    fn exists(&self, rel: &str) -> bool {
        self.files.contains(rel)
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(src_dir().join(rel))
            .unwrap_or_else(|e| panic!("reading src/{rel}: {e}"))
    }
}

impl Sources for BTreeMap<&str, &str> {
    fn exists(&self, rel: &str) -> bool {
        self.contains_key(rel)
    }

    fn read(&self, rel: &str) -> String {
        self.get(rel)
            .map(|src| (*src).to_owned())
            .unwrap_or_else(|| panic!("no source {rel}"))
    }
}

/// Every `.rs` file under `src/`, relative to it, sorted.
fn crate_source_files() -> Vec<String> {
    /// Far above the crate's size: the walk stays bounded regardless.
    const MAX_FILES: usize = 1024;
    let root = src_dir();
    let mut dirs = vec![root.clone()];
    let mut files = Vec::new();
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let rel = path.strip_prefix(&root).unwrap();
                files.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
        assert!(files.len() + dirs.len() < MAX_FILES, "src/ walk runaway");
    }
    files.sort();
    files
}

// ---------------------------------------------------------------------------
// Lexing: comments out, literals kept or blanked
// ---------------------------------------------------------------------------

/// A source file with its comments removed, in two views.
struct CodeViews {
    /// String and char literals kept verbatim: what the forbidden-word scan
    /// reads, so a `#[path]` or `include!` naming skim's sources is caught.
    literal: String,
    /// Every literal replaced by `""`: what the identifier scans read, so a
    /// name inside a message or a test fixture is not a reference.
    bare: String,
}

fn is_ident_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

/// Splits Rust source into [`CodeViews`]: line and (nested) block comments
/// removed; plain, byte, C and raw strings (any `#` count) and char literals
/// recognised, lifetimes and labels (`'a`) left as code.
fn code_views(src: &str) -> CodeViews {
    let s: Vec<char> = src.chars().collect();
    let mut views = CodeViews {
        literal: String::with_capacity(src.len()),
        bare: String::with_capacity(src.len()),
    };
    let mut i = 0;
    while i < s.len() {
        let next = s.get(i + 1).copied();
        if s[i] == '/' && next == Some('/') {
            // A line comment ends at (and keeps) its newline.
            i = s[i..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(s.len(), |n| i + n);
        } else if s[i] == '/' && next == Some('*') {
            i = block_comment_end(&s, i);
            views.literal.push(' ');
            views.bare.push(' ');
        } else if let Some(end) = literal_end(&s, i) {
            views.literal.extend(&s[i..end]);
            views.bare.push_str("\"\"");
            i = end;
        } else {
            views.literal.push(s[i]);
            views.bare.push(s[i]);
            i += 1;
        }
    }
    views
}

/// The index just past the block comment opening at `start`, nesting
/// counted; the end of the source if it is unterminated.
fn block_comment_end(s: &[char], start: usize) -> usize {
    let mut depth = 0usize;
    let mut i = start;
    while i < s.len() {
        match (s[i], s.get(i + 1).copied()) {
            ('/', Some('*')) => {
                depth += 1;
                i += 2;
            }
            ('*', Some('/')) => {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    return i;
                }
            }
            _ => i += 1,
        }
    }
    s.len()
}

/// The index just past the string or char literal starting at `start`, if
/// one does: `"…"`, `b"…"`, `c"…"`, `r#"…"#`, `br"…"`, `'x'`, `'\n'`,
/// `b'x'`. A lifetime or label (`'a`) and a raw identifier (`r#name`) are
/// not literals.
fn literal_end(s: &[char], start: usize) -> Option<usize> {
    let at = |k: usize| s.get(k).copied();
    let mut i = start;
    if matches!(at(i), Some('b' | 'c' | 'r')) {
        if start > 0 && is_ident_char(s[start - 1]) {
            return None; // inside an identifier
        }
        if matches!(at(i), Some('b' | 'c')) && matches!(at(i + 1), Some('"' | '\'' | 'r')) {
            i += 1;
        }
        if at(i) == Some('r') {
            let hashes = s[i + 1..].iter().take_while(|&&c| c == '#').count();
            if at(i + 1 + hashes) != Some('"') {
                return None; // an identifier, or a raw identifier
            }
            let close = (i + 2 + hashes..s.len())
                .find(|&k| s[k] == '"' && (1..=hashes).all(|h| at(k + h) == Some('#')));
            return Some(close.map_or(s.len(), |k| k + 1 + hashes));
        }
    }
    match at(i) {
        Some('"') => {
            let mut k = i + 1;
            while k < s.len() {
                match s[k] {
                    '\\' => k += 2,
                    '"' => return Some(k + 1),
                    _ => k += 1,
                }
            }
            Some(s.len())
        }
        Some('\'') => match (at(i + 1), at(i + 2)) {
            (Some('\\'), _) => {
                // An escape: `'\n'`, `'\''`, `'\u{1F600}'`.
                let close = (i + 3..s.len()).find(|&k| s[k] == '\'');
                Some(close.map_or(s.len(), |k| k + 1))
            }
            (Some(c), Some('\'')) if c != '\'' => Some(i + 3),
            _ => None, // a lifetime or a label
        },
        _ => None,
    }
}

/// A token of [`CodeViews::bare`]: an identifier (or number), `::`, or any
/// other non-space character.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Ident(String),
    PathSep,
    Punct(char),
}

fn tokens(code: &str) -> Vec<Tok> {
    let s: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if is_ident_char(s[i]) {
            let len = s[i..].iter().take_while(|&&c| is_ident_char(c)).count();
            out.push(Tok::Ident(s[i..i + len].iter().collect()));
            i += len;
        } else if s[i] == ':' && s.get(i + 1) == Some(&':') {
            out.push(Tok::PathSep);
            i += 2;
        } else {
            if !s[i].is_whitespace() {
                out.push(Tok::Punct(s[i]));
            }
            i += 1;
        }
    }
    out
}

fn has_ident(toks: &[Tok], name: &str) -> bool {
    toks.iter()
        .any(|tok| matches!(tok, Tok::Ident(ident) if ident == name))
}

// ---------------------------------------------------------------------------
// The scan set: the import closure of the structural scoring path
// ---------------------------------------------------------------------------

/// Every path in `toks`, `use` groups expanded: `a::{b, c::{self, d}}`
/// yields `a::b`, `a::c::self` and `a::c::d`; a glob `a::*` yields `a::*`.
fn paths(toks: &[Tok]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let starts = matches!(toks[i], Tok::Ident(_)) && (i == 0 || toks[i - 1] != Tok::PathSep);
        i = if starts {
            path_tree(toks, i, &[], &mut out)
        } else {
            i + 1
        };
    }
    out
}

/// Parses the path starting at `toks[i]` (an identifier, `*` or `{`) under
/// `prefix` into `out`; returns the index after it.
fn path_tree(toks: &[Tok], mut i: usize, prefix: &[String], out: &mut Vec<Vec<String>>) -> usize {
    let mut path = prefix.to_vec();
    loop {
        match toks.get(i) {
            Some(Tok::Ident(name)) => path.push(name.clone()),
            Some(Tok::Punct('*')) => {
                path.push("*".to_owned());
                out.push(path);
                return i + 1;
            }
            Some(Tok::Punct('{')) => return path_group(toks, i + 1, &path, out),
            _ => {
                out.push(path);
                return i;
            }
        }
        if toks.get(i + 1) != Some(&Tok::PathSep) {
            out.push(path);
            return i + 1;
        }
        i += 2;
    }
}

/// Parses the members of the `{…}` group whose first member is `toks[i]`;
/// returns the index after its `}`.
fn path_group(toks: &[Tok], mut i: usize, prefix: &[String], out: &mut Vec<Vec<String>>) -> usize {
    while let Some(tok) = toks.get(i) {
        i = match tok {
            Tok::Punct('}') => return i + 1,
            Tok::Ident(kw) if kw == "as" => i + 2, // `as alias`
            Tok::Ident(_) | Tok::Punct('*' | '{') => path_tree(toks, i, prefix, out),
            _ => i + 1,
        };
    }
    i
}

/// The module a source file defines: `scoreboard/golden.rs` →
/// `[scoreboard, golden]`, `scoreboard/mod.rs` → `[scoreboard]`, `lib.rs` →
/// the crate root `[]`.
fn module_of(rel: &str) -> Vec<String> {
    let mut module: Vec<String> = rel
        .trim_end_matches(".rs")
        .split('/')
        .map(str::to_owned)
        .collect();
    if module.last().is_some_and(|m| m == "mod") || module == ["lib"] {
        module.pop();
    }
    module
}

fn is_module_root(rel: &str) -> bool {
    rel == "lib.rs" || rel.ends_with("/mod.rs")
}

/// The source file of `module`, if the crate has one.
fn module_file(sources: &dyn Sources, module: &[String]) -> Option<String> {
    if module.is_empty() {
        return Some("lib.rs".to_owned()).filter(|f| sources.exists(f));
    }
    let base = module.join("/");
    [format!("{base}.rs"), format!("{base}/mod.rs")]
        .into_iter()
        .find(|f| sources.exists(f))
}

/// The in-crate source file that `path`, written in a file of `module`,
/// reaches: the deepest module along it. Conservative wherever exact Rust
/// scoping would need name resolution: `super` is the file's parent module
/// (inside an inline `mod tests` it is really the file itself, which is
/// scanned anyway), and a relative path whose first segment names a child of
/// this module or of its parent counts, as a `use` may have imported it.
fn resolve(sources: &dyn Sources, module: &[String], path: &[String]) -> Option<String> {
    let first = path.first()?.as_str();
    let supers = path.iter().take_while(|s| *s == "super").count();
    let (mut at, rest): (Vec<String>, &[String]) = match first {
        "crate" => (Vec::new(), &path[1..]),
        "self" => (module.to_vec(), &path[1..]),
        "super" => (
            module[..module.len().saturating_sub(supers)].to_vec(),
            &path[supers..],
        ),
        _ if path.len() < 2 => return None,
        _ => {
            let parent = &module[..module.len().saturating_sub(1)];
            let base = [module, parent].into_iter().find(|base| {
                module_file(sources, &[base.to_vec(), vec![first.to_owned()]].concat()).is_some()
            })?;
            (base.to_vec(), path)
        }
    };
    for segment in rest {
        match segment.as_str() {
            "self" => {}
            "*" => break,
            name => {
                let next = [at.clone(), vec![name.to_owned()]].concat();
                if module_file(sources, &next).is_none() {
                    break;
                }
                at = next;
            }
        }
    }
    module_file(sources, &at)
}

/// Where the structural scoring path starts: the oracle and the scoring.
const SCAN_ROOTS: &[&str] = &[
    "scoreboard/structural.rs",
    "scoreboard/structural_metrics.rs",
];

/// `roots` and every in-crate source file they reach through a path,
/// transitively, keyed by path relative to `src/`. Out-of-line child modules
/// are not followed: [`independence_violations`] admits only `#[cfg(test)]`
/// ones.
fn scan_set(sources: &dyn Sources, roots: &[&str]) -> BTreeMap<String, String> {
    /// Far above the crate's module count: the walk stays bounded regardless.
    const MAX_SCANNED: usize = 256;
    let mut scanned = BTreeMap::new();
    let mut pending: Vec<String> = roots.iter().map(|r| (*r).to_owned()).collect();
    while let Some(rel) = pending.pop() {
        if scanned.contains_key(&rel) {
            continue;
        }
        assert!(scanned.len() < MAX_SCANNED, "scan set runaway at {rel}");
        let src = sources.read(&rel);
        let module = module_of(&rel);
        pending.extend(
            paths(&tokens(&code_views(&src).bare))
                .iter()
                .filter_map(|path| resolve(sources, &module, path))
                .filter(|dep| !scanned.contains_key(dep)),
        );
        scanned.insert(rel, src);
    }
    scanned
}

// ---------------------------------------------------------------------------
// The rules
// ---------------------------------------------------------------------------

/// Names of skim's AST search stack (and its core crate): forbidden anywhere
/// in a scanned file's code or literals.
const FORBIDDEN_WORDS: &[&str] = &["ast_index", "compound", "linearize", "rskim_core"];

/// Catalog `Pattern` members that encode how skim matches a pattern (its
/// n-gram tables and their resolvers) or reach `rskim_core` (the example's
/// language): never named on the scoring path.
const CATALOG_INTERNALS: &[&str] = &[
    "bigrams",
    "trigrams",
    "resolved_bigrams",
    "resolved_trigrams",
    "example_lang",
];

/// The `rskim_search` items the oracle may name: the catalog, read in
/// `catalog_patterns` only.
const ORACLE_SKIM_ITEMS: &[&str] = &["all_patterns"];

/// The `rskim_search` items the rest of the scoring path may name, too:
/// `FileId`, the plain `u32` id the crate's rank helpers (`crate::metrics`)
/// take.
const SCORING_PATH_SKIM_ITEMS: &[&str] = &["all_patterns", "FileId"];

/// Skim's pattern-catalog entry points other than `all_patterns`: never
/// named in the crate.
const OTHER_CATALOG_ENTRY_POINTS: &[&str] =
    &["lookup_pattern", "parse_ast_query", "pattern_to_query_set"];

/// The one read of skim's pattern catalog, verbatim up to whitespace. It
/// projects each entry to the facts the scoreboard may use; exposing another
/// catalog field means editing this pin, a deliberate and reviewed change.
const CATALOG_ACCESSOR: &str = "
fn catalog_patterns() -> impl Iterator<Item = CatalogPattern> {
    rskim_search::all_patterns().iter().map(|p| CatalogPattern {
        name: p.name,
        exact: p.exact,
        example: p.example,
    })
}";

/// Out-of-line child modules (`mod name;`) that are not `#[cfg(test)]`: code
/// of this module in a file the scan does not read.
fn untested_child_modules(bare: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (at, _) in bare.match_indices("mod") {
        let after = &bare[at + "mod".len()..];
        let name: String = after
            .trim_start()
            .chars()
            .take_while(|&c| is_ident_char(c))
            .collect();
        let is_declaration = bare[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !is_ident_char(c))
            && after.starts_with(char::is_whitespace)
            && !name.is_empty()
            && after.trim_start()[name.len()..]
                .trim_start()
                .starts_with(';');
        if !is_declaration {
            continue;
        }
        let item_start = bare[..at].rfind([';', '{', '}']).map_or(0, |p| p + 1);
        let attributes: String = bare[item_start..at]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if !attributes.contains("#[cfg(test)]") {
            found.push(name);
        }
    }
    found
}

/// Every independence violation in the file `rel` (source `src`), naming
/// only the `allowed` `rskim_search` items.
fn independence_violations(rel: &str, src: &str, allowed: &[&str]) -> Vec<String> {
    let views = code_views(src);
    let mut violations: Vec<String> = FORBIDDEN_WORDS
        .iter()
        .filter(|word| views.literal.contains(**word))
        .map(|word| format!("{rel}: mentions `{word}`"))
        .collect();
    for (at, _) in views.literal.match_indices("rskim_search") {
        let rest = &views.literal[at + "rskim_search".len()..];
        let item: String = rest
            .strip_prefix("::")
            .map(|r| r.chars().take_while(|&c| is_ident_char(c)).collect())
            .unwrap_or_default();
        if !allowed.contains(&item.as_str()) {
            let context: String = rest.chars().take(40).collect();
            violations.push(format!("{rel}: names `rskim_search{context}`"));
        }
    }
    let toks = tokens(&views.bare);
    violations.extend(
        CATALOG_INTERNALS
            .iter()
            .filter(|member| has_ident(&toks, member))
            .map(|member| format!("{rel}: reads catalog member `{member}`")),
    );
    if !is_module_root(rel) {
        violations.extend(
            untested_child_modules(&views.bare)
                .into_iter()
                .map(|name| format!("{rel}: declares module `{name}` the scan cannot see")),
        );
    }
    violations
}

/// `src` with all whitespace removed: a layout-independent comparison.
fn squash(src: &str) -> String {
    src.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The item `fn <name>(…) … { … }` in `bare`, through its closing brace.
fn fn_item<'a>(bare: &'a str, name: &str) -> Option<&'a str> {
    let start = bare.find(&format!("fn {name}("))?;
    let open = start + bare[start..].find('{')?;
    let mut depth = 0usize;
    for (k, c) in bare[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&bare[start..=open + k]);
                }
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn oracle_uses_only_pattern_names_from_skim() {
    let rel = "scoreboard/structural.rs";
    let src = CrateSources::new().read(rel);
    assert_eq!(
        independence_violations(rel, &src, ORACLE_SKIM_ITEMS),
        Vec::<String>::new()
    );
    // The check is not vacuous: the oracle does name the catalog.
    assert!(
        code_views(&src)
            .bare
            .contains("rskim_search::all_patterns()")
    );
}

#[test]
fn the_structural_scoring_path_is_independent() {
    let scanned = scan_set(&CrateSources::new(), SCAN_ROOTS);
    // Not vacuous: the walk reaches what the scoring imports, and what those
    // modules import in turn (a re-export in any of them would reach the
    // scoring path without the two roots naming it).
    for rel in [
        "scoreboard/golden.rs",
        "scoreboard/metrics.rs",
        "scoreboard/report.rs",
        "scoreboard/types.rs",
        "scoreboard/oracle.rs",
        "scoreboard/runner.rs",
        "scoreboard/universe.rs",
        "scoreboard/baseline.rs",
        "scoreboard/mod.rs",
        "metrics.rs",
    ] {
        assert!(
            scanned.contains_key(rel),
            "{rel} is on the scoring path; scanned: {:?}",
            scanned.keys()
        );
    }
    let violations: Vec<String> = scanned
        .iter()
        .flat_map(|(rel, src)| independence_violations(rel, src, SCORING_PATH_SKIM_ITEMS))
        .collect();
    assert_eq!(violations, Vec::<String>::new());
}

#[test]
fn skims_pattern_catalog_is_read_in_exactly_one_place() {
    let sources = CrateSources::new();
    let mut reads = Vec::new();
    for rel in &sources.files {
        let toks = tokens(&code_views(&sources.read(rel)).bare);
        for name in std::iter::once(&"all_patterns").chain(OTHER_CATALOG_ENTRY_POINTS) {
            if has_ident(&toks, name) {
                reads.push(format!("{rel}: {name}"));
            }
        }
    }
    assert_eq!(
        reads,
        ["scoreboard/structural.rs: all_patterns"],
        "skim's pattern catalog is read through `structural::catalog_patterns` only"
    );
    let rel = "scoreboard/structural.rs";
    let bare = code_views(&sources.read(rel)).bare;
    let catalog_calls = tokens(&bare)
        .iter()
        .filter(|tok| matches!(tok, Tok::Ident(name) if name == "all_patterns"))
        .count();
    assert_eq!(catalog_calls, 1, "{rel} names `all_patterns` once");
    let accessor = fn_item(&bare, "catalog_patterns").expect("`catalog_patterns` exists");
    assert!(
        has_ident(&tokens(accessor), "all_patterns"),
        "the one read is `catalog_patterns`"
    );
    assert_eq!(
        squash(accessor),
        squash(CATALOG_ACCESSOR),
        "`catalog_patterns` projects the catalog to name / exact / example only"
    );
}

#[test]
fn independence_check_catches_imports_and_ignores_comments() {
    let bad = [
        "use rskim_search::ast_index::linearize_source;",
        "use rskim_search::{all_patterns, compound};",
        "let t = rskim_search::AstQuery::Pattern(p);",
        "use rskim_search as rs;",
        "let l = rskim_core::Language::Rust;",
        "use crate::x; let s = \"//\"; linearize(s);",
        // TP-5: an allowed catalog read that goes on to skim's n-gram tables.
        "pub fn probe() -> usize { rskim_search::all_patterns().iter()\
         .map(|p| p.bigrams.len() + p.trigrams.len()).sum() }",
        "let n = pattern.resolved_trigrams().len();",
        "let lang = p.example_lang;",
        // Literals that would hide code from a naive scanner.
        "let c = '\"'; let n = p.bigrams.len(); let d = '\"';",
        "let s = r#\"say \"hi\" // not a comment\"#; let n = p.trigrams.len();",
        "fn f<'a>(p: &'a P) -> usize { p.bigrams.len() }",
        // A child module is code the scan cannot see; a path attribute
        // naming skim's sources is caught through its literal.
        "mod helpers;",
        "pub(crate) mod helpers ;",
        "#[cfg(test)] #[path = \"../../../rskim-search/src/ast_index/patterns.rs\"] mod p;",
    ];
    for code in bad {
        assert!(
            !independence_violations("scoreboard/x.rs", code, SCORING_PATH_SKIM_ITEMS).is_empty(),
            "{code}"
        );
    }
    let fine = [
        "// cites crates/rskim-search/src/ast_index/linearize.rs:119-134",
        "/// see compound/reparse.rs and rskim_core::ast_size_limit",
        "let names = rskim_search::all_patterns(); // not ast_index",
        "/* p.bigrams /* nested */ p.trigrams */ let n = 1;",
        "let msg = \"p.bigrams and p.trigrams are skim's\";",
        "let s = r#\"p.bigrams\"#; let t = b\"trigrams\"; let u = b'x';",
        "let bigram_count = 2; let modules = 1; mod inline { }",
        "#[cfg(test)]\n#[allow(clippy::unwrap_used)] // test code\n#[path = \"x_tests.rs\"]\nmod tests;",
        "use crate::metrics::mrr; let id = rskim_search::FileId(1);",
    ];
    for code in fine {
        assert_eq!(
            independence_violations("scoreboard/x.rs", code, SCORING_PATH_SKIM_ITEMS),
            Vec::<String>::new(),
            "{code}"
        );
    }
    // The oracle itself names nothing from skim but the catalog.
    assert!(
        !independence_violations("x.rs", "rskim_search::FileId(1)", ORACLE_SKIM_ITEMS).is_empty()
    );
    // A module root declares the module tree; its `mod` items are not scanned code.
    assert_eq!(
        independence_violations(
            "scoreboard/mod.rs",
            "pub mod golden_gen;",
            ORACLE_SKIM_ITEMS
        ),
        Vec::<String>::new()
    );
}

#[test]
fn paths_expand_use_groups_and_skip_comments_and_literals() {
    let code = code_views(
        "use crate::scoreboard::{golden::{self, G}, metrics as m, *};\n\
         let x = a::B::<T>::new(); // c::D\n\
         let s = \"e::F\";",
    );
    let got = paths(&tokens(&code.bare));
    for want in [
        &["crate", "scoreboard", "golden", "self"][..],
        &["crate", "scoreboard", "golden", "G"][..],
        &["crate", "scoreboard", "metrics"][..],
        &["crate", "scoreboard", "*"][..],
        &["a", "B"][..],
    ] {
        assert!(got.iter().any(|p| p == want), "{want:?} in {got:?}");
    }
    for absent in ["c", "e", "m", "as"] {
        assert!(
            got.iter().all(|p| p.len() < 2 || p[0] != absent),
            "{absent} in {got:?}"
        );
    }
}

#[test]
fn the_scan_follows_every_in_crate_path_transitively() {
    let tree: BTreeMap<&str, &str> = BTreeMap::from([
        ("lib.rs", "pub mod metrics;\npub mod scoreboard;\n"),
        ("metrics.rs", "pub fn mrr() {}\n"),
        (
            "scoreboard/mod.rs",
            "pub mod a;\npub mod b;\npub mod c;\npub mod d;\npub mod e;\n\
             pub const MAX: u32 = 1;\n",
        ),
        (
            "scoreboard/a.rs",
            "use crate::scoreboard::{b::{self, B}, MAX};\n\
             // crate::scoreboard::e::E\n\
             fn g() -> &'static str { crate::metrics::mrr(); \"crate::scoreboard::e\" }\n\
             #[cfg(test)]\nmod tests {\n    use super::*;\n}\n",
        ),
        ("scoreboard/b.rs", "use super::c::C;\n"),
        ("scoreboard/c.rs", "pub fn f() { d::x(); }\n"),
        (
            "scoreboard/d.rs",
            "pub use rskim_search::lookup_pattern as x;\n",
        ),
        ("scoreboard/e.rs", "unreached\n"),
    ]);
    let scanned = scan_set(&tree, &["scoreboard/a.rs"]);
    assert_eq!(
        scanned.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "metrics.rs",
            "scoreboard/a.rs",
            "scoreboard/b.rs",
            "scoreboard/c.rs",
            "scoreboard/d.rs",
            "scoreboard/mod.rs",
        ]
    );
    // The re-export two imports away is on the scanned path, and caught.
    assert_eq!(
        independence_violations(
            "scoreboard/d.rs",
            &scanned["scoreboard/d.rs"],
            SCORING_PATH_SKIM_ITEMS
        ),
        ["scoreboard/d.rs: names `rskim_search::lookup_pattern as x;\n`"]
    );
}
