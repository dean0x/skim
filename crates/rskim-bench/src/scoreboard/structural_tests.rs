//! Unit tests for `structural.rs` (co-located file, `#[path]`-included).

use std::collections::BTreeSet;
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
    rskim_search::all_patterns()
        .iter()
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
    let catalog: BTreeSet<&str> = rskim_search::all_patterns()
        .iter()
        .map(|p| p.name)
        .collect();
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
    /// The pattern's catalog `example`, verbatim (via `all_patterns()`).
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
// AC-3: independence from skim's AST search internals
// ============================================================================

/// `structural.rs` with every `//` comment removed (string literals kept, so
/// a `//` inside a string is not mistaken for a comment). Block comments are
/// kept as code, which can only make the check stricter.
fn strip_line_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(escaped) = chars.next() {
                    out.push(escaped);
                }
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '/' && chars.peek() == Some(&'/') {
            for skipped in chars.by_ref() {
                if skipped == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else {
            out.push(c);
            in_string = c == '"';
        }
    }
    out
}

/// The oracle's only permitted `rskim_search` item.
const ALLOWED_RSKIM_SEARCH_ITEMS: &[&str] = &["all_patterns"];

/// Every independence violation in `src`'s code (comments excluded).
fn independence_violations(src: &str) -> Vec<String> {
    let code = strip_line_comments(src);
    let mut violations: Vec<String> = ["ast_index", "compound", "linearize", "rskim_core"]
        .iter()
        .filter(|word| code.contains(*word))
        .map(|word| format!("code mentions `{word}`"))
        .collect();
    for (at, _) in code.match_indices("rskim_search") {
        let rest = &code[at + "rskim_search".len()..];
        let item: String = rest
            .strip_prefix("::")
            .map(|r| {
                r.chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect()
            })
            .unwrap_or_default();
        if !ALLOWED_RSKIM_SEARCH_ITEMS.contains(&item.as_str()) {
            let context: String = rest.chars().take(40).collect();
            violations.push(format!("disallowed use `rskim_search{context}`"));
        }
    }
    violations
}

#[test]
fn oracle_uses_only_pattern_names_from_skim() {
    let src = include_str!("structural.rs");
    assert_eq!(independence_violations(src), Vec::<String>::new());
    // The check is not vacuous: the oracle does name the catalog.
    assert!(strip_line_comments(src).contains("rskim_search::all_patterns()"));
}

#[test]
fn the_structural_scoring_module_is_independent_too() {
    // structural_metrics.rs turns the oracle's answers and skim's rows into
    // the structural checks: the same rule applies to it.
    let src = include_str!("structural_metrics.rs");
    assert_eq!(independence_violations(src), Vec::<String>::new());
    assert!(strip_line_comments(src).contains("StructuralOracle"));
}

#[test]
fn the_modules_the_structural_scoring_imports_are_independent_too() {
    // structural_metrics.rs imports these; a re-export of skim's AST code
    // from one of them would reach the scoring path without either file
    // above naming it.
    for (name, src) in [
        ("golden.rs", include_str!("golden.rs")),
        ("metrics.rs", include_str!("metrics.rs")),
        ("report.rs", include_str!("report.rs")),
        ("types.rs", include_str!("types.rs")),
    ] {
        assert_eq!(independence_violations(src), Vec::<String>::new(), "{name}");
    }
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
    ];
    for line in bad {
        assert!(!independence_violations(line).is_empty(), "{line}");
    }
    let fine = [
        "// cites crates/rskim-search/src/ast_index/linearize.rs:119-134",
        "/// see compound/reparse.rs and rskim_core::ast_size_limit",
        "let names = rskim_search::all_patterns(); // not ast_index",
    ];
    for line in fine {
        assert_eq!(
            independence_violations(line),
            Vec::<String>::new(),
            "{line}"
        );
    }
}
