//! Unit tests for `structural.rs` (co-located file, `#[path]`-included).
//!
//! The tests that need skim's pattern catalog live in `rskim-bench`
//! (`src/scoreboard/catalog_tests.rs`), which reads the catalog: every query
//! matches its pattern's catalog example and rejects a near-miss (AC-1), and
//! every catalog pattern is covered or listed uncovered with a reason. This
//! crate never reads the catalog.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use tree_sitter::{Query, QueryCursor};

use super::*;

/// Compiled once: `StructuralOracle` is `Sync`.
static ORACLE: LazyLock<StructuralOracle> =
    LazyLock::new(|| StructuralOracle::new().expect("every oracle query compiles"));

const TS_FAMILY: [OracleLang; 3] = [
    OracleLang::TypeScript,
    OracleLang::Tsx,
    OracleLang::JavaScript,
];

/// A Rust function with 19 body elements, one short of `god-function`.
const GOD_FUNCTION_19: &str = "fn big() { let a=1; let b=2; let c=3; let d=4; let e=5; let f=6; \
     let g=7; let h=8; let i=9; let j=10; let k=11; let l=12; let m=13; let n=14; let o=15; \
     let p=16; let q=17; let r=18; let s=19; }";

fn lines(pattern: &str, lang: OracleLang, source: &str) -> Vec<u32> {
    ORACLE
        .match_lines(&mut OracleScratch::new(), pattern, lang, source)
        .unwrap_or_else(|e| panic!("{pattern}.{lang}: {e:#}"))
}

fn intent(pattern: &str, lang: OracleLang, source: &str) -> Vec<u32> {
    ORACLE
        .intent_lines(pattern, lang, source)
        .unwrap_or_else(|e| panic!("{pattern} intent ({lang}): {e:#}"))
}

fn parses_cleanly(lang: OracleLang, source: &str) -> bool {
    !OracleScratch::new()
        .parse(lang, source)
        .unwrap()
        .root_node()
        .has_error()
}

// ============================================================================
// Registry: compilation, files on disk, coverage by pattern name
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
fn an_intent_node_kind_its_grammar_lacks_fails_to_compile() {
    // TS/JS kinds on the Rust grammar.
    static BAD_LOOP: IntentSpec = IntentSpec {
        pattern: "rust-nested-loop",
        langs: &[OracleLang::Rust],
        loop_kinds: &["for_expression", "for_statement"],
        boundary_kinds: &["function_item"],
    };
    static BAD_BOUNDARY: IntentSpec = IntentSpec {
        pattern: "rust-nested-loop",
        langs: &[OracleLang::Rust],
        loop_kinds: &["for_expression"],
        boundary_kinds: &["function_item", "arrow_function"],
    };
    let err = compile_intent(&BAD_LOOP, OracleLang::Rust).err().unwrap();
    assert!(
        format!("{err:#}").contains("loop kind \"for_statement\""),
        "{err:#}"
    );
    let err = compile_intent(&BAD_BOUNDARY, OracleLang::Rust)
        .err()
        .unwrap();
    assert!(
        format!("{err:#}").contains("boundary kind \"arrow_function\""),
        "{err:#}"
    );
}

#[test]
fn every_scm_file_on_disk_is_registered_exactly_once() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("queries");
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
    // The registry itself is kept in the same order, so a diff reads cleanly.
    let registry: Vec<(&str, &str)> = QUERIES
        .iter()
        .map(|q| (q.pattern, q.lang.as_str()))
        .collect();
    assert_eq!(registry, keys);
}

#[test]
fn coverage_of_reads_the_registry_then_the_uncovered_list() {
    // A registered pattern is covered in exactly its registered languages,
    // sorted.
    assert_eq!(
        coverage_of("try-finally"),
        PatternCoverage::Covered {
            langs: vec![
                OracleLang::Python,
                OracleLang::TypeScript,
                OracleLang::Tsx,
                OracleLang::JavaScript,
            ]
        }
    );
    for q in query_sources() {
        let registered: Vec<OracleLang> = query_sources()
            .iter()
            .filter(|other| other.pattern == q.pattern)
            .map(|other| other.lang)
            .collect();
        let PatternCoverage::Covered { langs } = coverage_of(q.pattern) else {
            panic!("{} is registered but not covered", q.file_name());
        };
        assert_eq!(
            langs.iter().copied().collect::<BTreeSet<_>>(),
            registered.into_iter().collect::<BTreeSet<_>>(),
            "{}",
            q.pattern
        );
    }

    // A listed pattern carries its own reason; no listed pattern also has a
    // query, and none is listed twice.
    let mut listed = BTreeSet::new();
    for &(name, reason) in UNCOVERED {
        assert!(listed.insert(name), "{name} is listed twice");
        assert!(!reason.trim().is_empty(), "{name} has an empty reason");
        assert_ne!(reason, UNCLASSIFIED_REASON, "{name}");
        assert_eq!(coverage_of(name), PatternCoverage::Uncovered { reason });
    }

    // A name the oracle has no record of reads as unclassified.
    assert_eq!(
        coverage_of("no-such-pattern"),
        PatternCoverage::Uncovered {
            reason: UNCLASSIFIED_REASON
        }
    );
}

// ============================================================================
// Encoded semantics beyond the catalog examples
// ============================================================================

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
    match ORACLE
        .file_matches(&mut OracleScratch::new(), "src/View.tsx", src)
        .unwrap()
    {
        FileMatches::Scored(report) => {
            assert_eq!(report.lang, OracleLang::Tsx);
            assert_eq!(report.definition["try-catch-finally"], [2]);
            assert_eq!(report.definition["try-catch"], [2]);
            assert_eq!(report.definition["method-with-body"], Vec::<u32>::new());
        }
        other => panic!("expected a scored file, got {other:?}"),
    }
}

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
    // A method counts.
    let method = format!("impl S {{\n    {with_tail}\n}}\n");
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
fn nested_loop_intent_resumes_after_a_function_boundary_closes() {
    // The loop after the closure is nested again: leaving the boundary
    // restores the enclosing loop's context.
    let ts = "for (;;) {\n  const g = () => {\n    for (;;) {}\n  };\n  for (;;) {}\n}\n";
    for lang in TS_FAMILY {
        assert!(parses_cleanly(lang, ts), "{lang}");
        assert_eq!(intent("nested-loop", lang, ts), [5], "{lang}");
    }
    let rust = "fn f() {\n    loop {\n        let g = || {\n            loop {}\n        };\n        loop {}\n    }\n}\n";
    assert!(parses_cleanly(OracleLang::Rust, rust));
    assert_eq!(intent("rust-nested-loop", OracleLang::Rust, rust), [6]);
}

/// A function whose loop (line 2) holds an `if` / `else if` chain of `arms`
/// arms, each with its own loop (lines 3 to `2 + arms`), then one more arm
/// whose loop sits behind an arrow function (line `3 + arms`).
fn deep_else_if_chain(arms: usize) -> String {
    let mut src = String::from("function f() {\n  for (;;) {\n    if (c0) { for (;;) {} }\n");
    for arm in 1..arms {
        src.push_str(&format!("    else if (c{arm}) {{ for (;;) {{}} }}\n"));
    }
    src.push_str("    else if (z) { const g = () => { for (;;) {} }; }\n  }\n}\n");
    src
}

/// Lines 3 to `2 + arms`: the chain's loops, all inside line 2's loop.
fn deep_else_if_chain_answer(arms: usize) -> Vec<u32> {
    (3..3 + u32::try_from(arms).unwrap()).collect()
}

#[test]
fn a_deep_else_if_chain_is_answered_in_one_pass() {
    // Every `else if` nests one level deeper (if_statement > else_clause >
    // if_statement), so the chain's last loop sits about 6,000 levels down
    // and each loop's nearest loop ancestor is the outer one at the top.
    // Walking up with `Node::parent`, which re-descends from the root on
    // every step, costs O(depth²) per loop: hours at this size. One
    // pre-order pass with a tree cursor is linear in the tree.
    const ARMS: usize = 3_000;
    let src = deep_else_if_chain(ARMS);
    assert!(parses_cleanly(OracleLang::TypeScript, &src));
    assert_eq!(
        intent("nested-loop", OracleLang::TypeScript, &src),
        deep_else_if_chain_answer(ARMS)
    );
    // The per-file path answers the same, and no definition query runs out
    // of in-progress matches at this depth.
    let FileMatches::Scored(report) = ORACLE
        .file_matches(&mut OracleScratch::new(), "src/deep.ts", &src)
        .unwrap()
    else {
        panic!("a small .ts file is scored");
    };
    assert_eq!(
        report.intent["nested-loop"],
        deep_else_if_chain_answer(ARMS)
    );
}

/// The intent answer the way the oracle first computed it: every loop the
/// query `[(<loop kind>) …] @loop` captures whose ancestors, walked with
/// `Node::parent`, reach a loop kind before a boundary kind. Quadratic in
/// the depth, so only for small sources: the reference the one-pass walk
/// must agree with.
fn ancestor_walk_intent(spec: &IntentSpec, lang: OracleLang, source: &str) -> Vec<u32> {
    let tree = OracleScratch::new().parse(lang, source).unwrap();
    let kinds: Vec<String> = spec.loop_kinds.iter().map(|k| format!("({k})")).collect();
    let query = Query::new(&lang.grammar(), &format!("[{}] @loop", kinds.join(" "))).unwrap();
    let mut cursor = QueryCursor::new();
    let mut lines = BTreeSet::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    while let Some(m) = matches.next() {
        for capture in m.captures {
            let nested = std::iter::successors(capture.node.parent(), Node::parent)
                .map(|ancestor| ancestor.kind())
                .find(|kind| spec.loop_kinds.contains(kind) || spec.boundary_kinds.contains(kind))
                .is_some_and(|kind| spec.loop_kinds.contains(&kind));
            if nested {
                lines.insert(first_line(capture.node).unwrap());
            }
        }
    }
    lines.into_iter().collect()
}

#[test]
fn the_intent_answer_agrees_with_the_ancestor_walk() {
    let chain = deep_else_if_chain(40);
    let ts: &[&str] = &[
        "function f() {\n  for (const a of xs) {\n    while (ok()) {\n      step();\n    }\n  }\n}\n",
        "for (;;) {\n  if (x) {\n    for (;;) {}\n  }\n}\n",
        "for (let i = 0; i < n; i++) {\n  do {\n    y();\n  } while (z);\n}\n",
        "for (const a of xs) {\n  const g = () => {\n    for (let i = 0; i < 3; i++) {}\n  };\n}\n",
        "while (x) {\n  function h() {\n    do { y(); } while (z);\n  }\n}\n",
        "for (;;) {\n  const o = {\n    m() {\n      for (;;) {}\n    },\n  };\n}\n",
        "for (;;) {\n  function* g() {\n    while (y) {}\n  }\n}\n",
        "for (;;) {\n  const g = () => {\n    for (;;) {}\n  };\n  for (;;) {}\n}\n",
        // Error recovery: a broken inner loop header.
        "for (;;) {\n  for (;; {\n    x();\n  }\n  while (y) {}\n}\n",
        &chain,
    ];
    let rust: &[&str] = &[
        "fn f() {\n    for i in 0..n {\n        loop {\n            break;\n        }\n    }\n}\n",
        "fn f() {\n    while a() {\n        if b() {\n            while c() {}\n        }\n    }\n}\n",
        "fn f() {\n    for i in 0..n {\n        let g = || {\n            for j in 0..m {}\n        };\n    }\n}\n",
        "fn f() {\n    while go() {\n        fn inner() {\n            loop {}\n        }\n    }\n}\n",
        "fn f() {\n    loop {\n        let g = || {\n            loop {}\n        };\n        loop {}\n    }\n}\n",
        "fn f() {\n    for i in 0..n {\n        for j in 0.. {\n    }\n}\n",
    ];
    for spec in INTENTS {
        for &lang in spec.langs {
            let sources = if lang == OracleLang::Rust { rust } else { ts };
            for src in sources {
                assert_eq!(
                    intent(spec.pattern, lang, src),
                    ancestor_walk_intent(spec, lang, src),
                    "{} ({lang}): {src}",
                    spec.pattern
                );
            }
        }
    }
}

#[test]
fn a_query_over_the_match_limit_is_an_error_naming_it() {
    // `(try_statement (catch_clause)) @match` holds one in-progress match per
    // enclosing try until that try's catch clause: three here.
    let src = "try {\n  try {\n    try { a(); } catch (e) {}\n  } catch (e) {}\n} catch (e) {}\n";
    assert_eq!(lines("try-catch", OracleLang::TypeScript, src), [1, 2, 3]);

    let mut tight = OracleScratch::with_match_limit(1).unwrap();
    let err = ORACLE
        .match_lines(&mut tight, "try-catch", OracleLang::TypeScript, src)
        .expect_err("three nested matches need three in-progress states");
    let msg = format!("{err:#}");
    assert!(msg.contains("try-catch.typescript.scm"), "{msg}");
    assert!(msg.contains("match limit of 1 "), "{msg}");
    // The per-file path names the file and the query that tripped.
    let err = ORACLE
        .file_matches(
            &mut OracleScratch::with_match_limit(1).unwrap(),
            "src/nested.ts",
            src,
        )
        .expect_err("a query over its limit fails the file");
    let msg = format!("{err:#}");
    assert!(msg.contains("structural oracle on src/nested.ts"), "{msg}");
    assert!(msg.contains(".typescript.scm needed more than"), "{msg}");

    // The limit stays inside tree-sitter's contract.
    assert!(OracleScratch::with_match_limit(0).is_err());
    assert!(OracleScratch::with_match_limit(MAX_MATCH_LIMIT + 1).is_err());
    assert!(OracleScratch::with_match_limit(MAX_MATCH_LIMIT).is_ok());
    assert_eq!(
        OracleScratch::new().cursor.match_limit(),
        ORACLE_MATCH_LIMIT
    );
}

#[test]
fn one_scratch_serves_files_of_every_language_in_turn() {
    let files = [
        (
            "src/a.rs",
            "fn f() {\n    for i in 0..n {\n        for j in 0..m {}\n    }\n}\n",
        ),
        (
            "src/View.tsx",
            "function View() {\n  try {\n    load();\n  } catch (e) {}\n  return <div>{x}</div>;\n}\n",
        ),
        ("src/b.py", "try:\n    a()\nfinally:\n    b()\n"),
        (
            "cmd/main.go",
            "package main\n\nfunc main() {\n\tdefer f()\n\tselect {}\n}\n",
        ),
        ("src/c.js", "for (;;) {\n  for (;;) {}\n}\n"),
        (
            "src/d.ts",
            "try {\n  try {\n    try { a(); } catch (e) {}\n  } catch (e) {}\n} catch (e) {}\n",
        ),
        (
            "src/a.rs",
            "fn f() {\n    for i in 0..n {\n        for j in 0..m {}\n    }\n}\n",
        ),
    ];
    let mut shared = OracleScratch::new();
    for (path, text) in files {
        let reused = ORACLE.file_matches(&mut shared, path, text).unwrap();
        let fresh = ORACLE
            .file_matches(&mut OracleScratch::new(), path, text)
            .unwrap();
        assert_eq!(reused, fresh, "{path}");
        let FileMatches::Scored(report) = reused else {
            panic!("{path} is scored");
        };
        assert!(
            report.definition.values().any(|lines| !lines.is_empty()),
            "{path} matches something, so the comparison means something"
        );
    }

    // A run that trips the match limit leaves the scratch usable: the next
    // file gets its full answer, and the limit still holds after it.
    let (path, nested) = files[5];
    let mut tight = OracleScratch::with_match_limit(1).unwrap();
    assert!(ORACLE.file_matches(&mut tight, path, nested).is_err());
    let flat = "try { a(); } catch (e) {}\n";
    assert_eq!(
        ORACLE.file_matches(&mut tight, "src/e.ts", flat).unwrap(),
        ORACLE
            .file_matches(&mut OracleScratch::new(), "src/e.ts", flat)
            .unwrap()
    );
    assert!(ORACLE.file_matches(&mut tight, path, nested).is_err());
}

#[test]
fn unknown_pattern_or_language_is_an_error() {
    assert!(
        ORACLE
            .match_lines(
                &mut OracleScratch::new(),
                "no-such-pattern",
                OracleLang::Rust,
                ""
            )
            .is_err()
    );
    assert!(
        ORACLE
            .match_lines(&mut OracleScratch::new(), "try-catch", OracleLang::Rust, "")
            .is_err()
    );
    assert!(
        ORACLE
            .match_lines(
                &mut OracleScratch::new(),
                "deep-nesting",
                OracleLang::TypeScript,
                ""
            )
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
    match ORACLE
        .file_matches(&mut OracleScratch::new(), "src/big.rs", &exact)
        .unwrap()
    {
        FileMatches::Scored(report) => {
            assert_eq!(report.lang, OracleLang::Rust);
            assert_eq!(report.definition["empty-function"], [1]);
        }
        other => panic!("a file of exactly 1 MiB is in the AST universe, got {other:?}"),
    }
    let over = format!("{exact}x");
    assert_eq!(
        ORACLE
            .file_matches(&mut OracleScratch::new(), "src/big.rs", &over)
            .unwrap(),
        FileMatches::OverSizeCap(OracleLang::Rust)
    );
}

#[test]
fn file_matches_reports_every_pattern_of_the_language() {
    let src = "fn f() {\n    for i in 0..n {\n        for j in 0..m {}\n    }\n}\n";
    let FileMatches::Scored(report) = ORACLE
        .file_matches(&mut OracleScratch::new(), "src/lib.rs", src)
        .unwrap()
    else {
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
        ORACLE
            .file_matches(&mut OracleScratch::new(), "A.java", "class A {}")
            .unwrap(),
        FileMatches::NotScored(LangClass::Unscored { language: "java" })
    );
    assert_eq!(
        ORACLE
            .file_matches(&mut OracleScratch::new(), "x.json", "{}")
            .unwrap(),
        FileMatches::NotScored(LangClass::NotIndexed {
            language: Some("json"),
            size_capped: false
        })
    );
}

// ============================================================================
// Golden fingerprint: every table an answer depends on
// ============================================================================

/// Renders each edited copy of the oracle inputs and asserts that its
/// fingerprint differs from the compiled-in inputs' and from every other
/// edit's, so each edit is proven to reach the golden digest on its own.
fn assert_each_edit_changes_the_fingerprint<'a>(
    edits: impl IntoIterator<Item = (&'a str, OracleInputs<'a>)>,
) {
    // No `..`: a field added to `OracleInputs` stops this from compiling
    // until its author adds a test of edits to it below.
    let OracleInputs {
        size_cap: _,
        ext_classes: _,
        unknown_extension: _,
        attribute_kinds: _,
        queries: _,
        intents: _,
    } = ORACLE_INPUTS;
    let mut seen = BTreeMap::from([(fingerprint(), "the compiled-in inputs")]);
    for (what, inputs) in edits {
        if let Some(earlier) = seen.insert(inputs.to_string(), what) {
            panic!("{what}: the fingerprint is the same as for {earlier}");
        }
    }
}

#[test]
fn the_fingerprint_does_not_depend_on_the_registry_order() {
    // The fingerprint sorts the queries, so only their content is an input.
    let mut reordered = QUERIES.to_vec();
    reordered.reverse();
    let reordered = OracleInputs {
        queries: &reordered,
        ..ORACLE_INPUTS
    };
    assert_eq!(reordered.to_string(), fingerprint());
}

#[test]
fn any_size_cap_edit_changes_the_fingerprint() {
    assert_each_edit_changes_the_fingerprint([
        (
            "one byte more",
            OracleInputs {
                size_cap: AST_SIZE_CAP_BYTES + 1,
                ..ORACLE_INPUTS
            },
        ),
        (
            "one byte less",
            OracleInputs {
                size_cap: AST_SIZE_CAP_BYTES - 1,
                ..ORACLE_INPUTS
            },
        ),
    ]);
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
    let dropped_ext = edited_ext_table("cts", |row| row.extensions = &["ts", "mts"]);
    let tsx_as_ts = edited_ext_table("tsx", |row| {
        row.class = LangClass::Oracle(OracleLang::TypeScript);
    });
    let java_not_indexed = edited_ext_table("java", |row| {
        row.class = LangClass::NotIndexed {
            language: Some("java"),
            size_capped: true,
        };
    });
    let renamed_unscored = edited_ext_table("rb", |row| {
        row.class = LangClass::Unscored { language: "rbx" };
    });
    let bash_uncapped = edited_ext_table("sh", |row| {
        row.class = LangClass::NotIndexed {
            language: Some("bash"),
            size_capped: false,
        };
    });
    let with_table = |ext_classes| OracleInputs {
        ext_classes,
        ..ORACLE_INPUTS
    };
    assert_each_edit_changes_the_fingerprint([
        ("drop an extension", with_table(&dropped_ext)),
        (
            "parse .tsx with the TypeScript grammar",
            with_table(&tsx_as_ts),
        ),
        ("stop AST-indexing Java", with_table(&java_not_indexed)),
        ("rename an unscored language", with_table(&renamed_unscored)),
        (
            "stop counting Bash toward the size cap",
            with_table(&bash_uncapped),
        ),
        (
            "drop a row",
            with_table(&EXT_CLASSES[..EXT_CLASSES.len() - 1]),
        ),
        (
            "count unknown extensions toward the size cap",
            OracleInputs {
                unknown_extension: LangClass::NotIndexed {
                    language: None,
                    size_capped: true,
                },
                ..ORACLE_INPUTS
            },
        ),
    ]);
}

#[test]
fn any_attribute_kind_edit_changes_the_fingerprint() {
    let with_kinds = |attribute_kinds| OracleInputs {
        attribute_kinds,
        ..ORACLE_INPUTS
    };
    assert_each_edit_changes_the_fingerprint([
        ("drop a kind", with_kinds(&["attribute_item"][..])),
        (
            "add a kind",
            with_kinds(&["attribute_item", "inner_attribute_item", "macro_invocation"][..]),
        ),
        ("no kinds", with_kinds(&[][..])),
    ]);
}

/// [`QUERIES`] with `edit` applied to the query registered as `file_name`.
fn edited_queries(file_name: &str, edit: impl FnOnce(&mut OracleQuery)) -> Vec<OracleQuery> {
    let mut queries = QUERIES.to_vec();
    let query = queries
        .iter_mut()
        .find(|q| q.file_name() == file_name)
        .unwrap_or_else(|| panic!("no query is registered as {file_name}"));
    edit(query);
    queries
}

#[test]
fn any_query_edit_changes_the_fingerprint() {
    let removed: Vec<OracleQuery> = QUERIES
        .iter()
        .filter(|q| q.file_name() != "try-catch.typescript.scm")
        .copied()
        .collect();
    // Same length, one byte different: the text itself is fingerprinted,
    // not only its length.
    let text_edit = edited_queries("go-defer.go.scm", |q| {
        let edited = q.source.replacen("@match", "@matcH", 1);
        assert_eq!(edited.len(), q.source.len());
        assert_ne!(edited, q.source);
        q.source = edited.leak();
    });
    let other_grammar = edited_queries("go-defer.go.scm", |q| q.lang = OracleLang::Rust);
    let filter_kind = edited_queries("empty-catch.tsx.scm", |q| {
        q.filter = Some(PostFilter::AtLeast {
            capture: "body",
            min: 0,
        });
    });
    let filter_capture = edited_queries("empty-catch.tsx.scm", |q| {
        q.filter = Some(PostFilter::Empty { capture: "block" });
    });
    let filter_dropped = edited_queries("empty-function.rust.scm", |q| q.filter = None);
    let filter_added = edited_queries("function-with-body.rust.scm", |q| {
        q.filter = EMPTY_BODY_FILTER;
    });
    let params_threshold = edited_queries("excessive-params.rust.scm", |q| {
        q.filter = Some(PostFilter::AtLeast {
            capture: "params",
            min: 6,
        });
    });
    let body_threshold = edited_queries("god-function.rust.scm", |q| {
        q.filter = Some(PostFilter::AtLeast {
            capture: "body",
            min: 19,
        });
    });
    let with_queries = |queries| OracleInputs {
        queries,
        ..ORACLE_INPUTS
    };
    assert_each_edit_changes_the_fingerprint([
        ("remove a query", with_queries(&removed)),
        ("edit a query's text", with_queries(&text_edit)),
        (
            "move a query to another grammar",
            with_queries(&other_grammar),
        ),
        ("change a post-filter's kind", with_queries(&filter_kind)),
        (
            "change a post-filter's capture",
            with_queries(&filter_capture),
        ),
        ("drop a post-filter", with_queries(&filter_dropped)),
        ("add a post-filter", with_queries(&filter_added)),
        (
            "change excessive-params' bound",
            with_queries(&params_threshold),
        ),
        ("change god-function's bound", with_queries(&body_threshold)),
    ]);
}

/// [`INTENTS`] with `edit` applied to the spec of `pattern`.
fn edited_intents(pattern: &str, edit: impl FnOnce(&mut IntentSpec)) -> Vec<IntentSpec> {
    let mut intents = INTENTS.to_vec();
    let spec = intents
        .iter_mut()
        .find(|spec| spec.pattern == pattern)
        .unwrap_or_else(|| panic!("no intent spec for {pattern}"));
    edit(spec);
    intents
}

#[test]
fn any_intent_edit_changes_the_fingerprint() {
    let removed: Vec<IntentSpec> = INTENTS
        .iter()
        .filter(|spec| spec.pattern != "nested-loop")
        .copied()
        .collect();
    let renamed = edited_intents("rust-nested-loop", |spec| {
        spec.pattern = "rust-loop-in-loop"
    });
    let lang_dropped = edited_intents("nested-loop", |spec| {
        spec.langs = &[OracleLang::TypeScript, OracleLang::JavaScript];
    });
    let loop_dropped = edited_intents("nested-loop", |spec| {
        spec.loop_kinds = &["for_statement", "for_in_statement", "while_statement"];
    });
    let loop_added = edited_intents("rust-nested-loop", |spec| {
        spec.loop_kinds = &[
            "for_expression",
            "while_expression",
            "loop_expression",
            "match_expression",
        ];
    });
    let boundary_dropped = edited_intents("rust-nested-loop", |spec| {
        spec.boundary_kinds = &["function_item"];
    });
    let boundary_added = edited_intents("nested-loop", |spec| {
        spec.boundary_kinds = &[
            "function_declaration",
            "function_expression",
            "arrow_function",
            "method_definition",
            "generator_function",
            "generator_function_declaration",
            "class_body",
        ];
    });
    let with_intents = |intents| OracleInputs {
        intents,
        ..ORACLE_INPUTS
    };
    assert_each_edit_changes_the_fingerprint([
        ("remove an intent", with_intents(&removed)),
        ("rename an intent", with_intents(&renamed)),
        ("drop an intent language", with_intents(&lang_dropped)),
        ("drop a loop kind", with_intents(&loop_dropped)),
        ("add a loop kind", with_intents(&loop_added)),
        ("drop a boundary kind", with_intents(&boundary_dropped)),
        ("add a boundary kind", with_intents(&boundary_added)),
    ]);
}

// ============================================================================
// Grammar identity: each query names the grammar version Cargo.lock resolves
// ============================================================================

/// The grammar crate an oracle language parses with, and — for the crate
/// that ships two grammars — the language constant
/// ([`OracleLang::grammar`]).
fn grammar_crate(lang: OracleLang) -> (&'static str, Option<&'static str>) {
    match lang {
        OracleLang::Rust => ("tree-sitter-rust", None),
        OracleLang::Python => ("tree-sitter-python", None),
        OracleLang::TypeScript => ("tree-sitter-typescript", Some("LANGUAGE_TYPESCRIPT")),
        OracleLang::Tsx => ("tree-sitter-typescript", Some("LANGUAGE_TSX")),
        OracleLang::JavaScript => ("tree-sitter-javascript", None),
        OracleLang::Go => ("tree-sitter-go", None),
    }
}

/// A string field of a `Cargo.lock` `[[package]]` entry.
fn field<'v>(package: &'v toml::Value, key: &str) -> Option<&'v str> {
    package.get(key).and_then(toml::Value::as_str)
}

/// The version the workspace `Cargo.lock` resolves for each dependency of
/// this crate, read from this crate's lock entry: a dependency listed as
/// `name version` (the lock holds several versions of it) carries its
/// version, and a bare `name` is the lock's one package of that name.
fn locked_dependency_versions() -> BTreeMap<String, String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock");
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let lock: toml::Table = raw
        .parse()
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let packages = lock["package"].as_array().expect("Cargo.lock has packages");
    let this = packages
        .iter()
        .find(|p| field(p, "name") == Some(env!("CARGO_PKG_NAME")))
        .expect("Cargo.lock has an entry for this crate");
    let dependencies = this["dependencies"].as_array().expect("its dependencies");
    dependencies
        .iter()
        .map(|dependency| {
            let dependency = dependency.as_str().expect("a dependency is a string");
            let mut words = dependency.split(' ');
            let name = words.next().expect("a dependency has a name").to_string();
            let version = words.next().map_or_else(
                || {
                    let versions: Vec<&str> = packages
                        .iter()
                        .filter(|p| field(p, "name") == Some(name.as_str()))
                        .filter_map(|p| field(p, "version"))
                        .collect();
                    assert_eq!(versions.len(), 1, "{name}: {versions:?}");
                    versions[0].to_string()
                },
                str::to_string,
            );
            (name, version)
        })
        .collect()
}

/// What is wrong with `source`'s grammar header, if anything: its second
/// line must be `; Grammar: <crate> <locked version>.`, with the language
/// constant in parentheses before the full stop for a crate that ships two
/// grammars, and no other line may start `; Grammar:`.
fn grammar_header_problem(
    source: &str,
    lang: OracleLang,
    locked: &BTreeMap<String, String>,
) -> Option<String> {
    let (krate, constant) = grammar_crate(lang);
    let Some(version) = locked.get(krate) else {
        return Some(format!("{krate} is not a locked dependency of this crate"));
    };
    let constant = constant.map_or_else(String::new, |c| format!(" ({c})"));
    let expected = format!("; Grammar: {krate} {version}{constant}.");
    let headers: Vec<(usize, &str)> = source
        .lines()
        .enumerate()
        .filter(|(_, line)| line.starts_with("; Grammar:"))
        .collect();
    match headers.as_slice() {
        [(1, header)] if *header == expected => None,
        [(1, header)] => Some(format!("header {header:?}, expected {expected:?}")),
        [] => Some(format!(
            "no grammar header, expected {expected:?} on line 2"
        )),
        _ => Some(format!(
            "grammar headers on lines {:?}, expected one on line 2",
            headers.iter().map(|(i, _)| i + 1).collect::<Vec<_>>()
        )),
    }
}

#[test]
fn every_query_names_the_grammar_version_cargo_lock_resolves() {
    let locked = locked_dependency_versions();
    let problems: Vec<String> = query_sources()
        .iter()
        .filter_map(|q| {
            grammar_header_problem(q.source, q.lang, &locked)
                .map(|problem| format!("{}: {problem}", q.file_name()))
        })
        .collect();
    assert!(
        problems.is_empty(),
        "query grammar headers disagree with Cargo.lock (a grammar bump must edit every \
         header it touches, which changes the golden digest and asks for a bless):\n{}",
        problems.join("\n")
    );
}

#[test]
fn a_stale_missing_or_misplaced_grammar_header_is_a_problem() {
    let locked = BTreeMap::from([
        ("tree-sitter-rust".to_string(), "0.24.0".to_string()),
        ("tree-sitter-typescript".to_string(), "0.23.2".to_string()),
    ]);
    let rust = |header: &str| format!("; Title.\n{header}\n(function_item) @match\n");
    let current = rust("; Grammar: tree-sitter-rust 0.24.0.");
    assert_eq!(
        grammar_header_problem(&current, OracleLang::Rust, &locked),
        None
    );
    for (what, source, lang) in [
        (
            "an older version",
            rust("; Grammar: tree-sitter-rust 0.23.0."),
            OracleLang::Rust,
        ),
        (
            "another grammar crate",
            rust("; Grammar: tree-sitter-go 0.24.0."),
            OracleLang::Rust,
        ),
        (
            "no header",
            "; Title.\n(function_item) @match\n".to_string(),
            OracleLang::Rust,
        ),
        (
            "the header off line 2",
            "; Title.\n;\n; Grammar: tree-sitter-rust 0.24.0.\n(function_item) @match\n"
                .to_string(),
            OracleLang::Rust,
        ),
        (
            "a second header",
            format!("{current}; Grammar: tree-sitter-rust 0.24.0.\n"),
            OracleLang::Rust,
        ),
        (
            "the other constant of a two-grammar crate",
            "; Title.\n; Grammar: tree-sitter-typescript 0.23.2 (LANGUAGE_TYPESCRIPT).\n"
                .to_string(),
            OracleLang::Tsx,
        ),
        (
            "a grammar that is not a locked dependency",
            "; Title.\n; Grammar: tree-sitter-go 0.25.0.\n".to_string(),
            OracleLang::Go,
        ),
    ] {
        assert!(
            grammar_header_problem(&source, lang, &locked).is_some(),
            "{what}: accepted"
        );
    }
}
