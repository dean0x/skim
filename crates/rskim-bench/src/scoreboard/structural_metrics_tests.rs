//! Unit tests for `structural_metrics.rs` (co-located file, `#[path]`-included).

use super::*;
use crate::scoreboard::golden::parse_golden;
use crate::scoreboard::test_support::{catalog, oracle};
use crate::scoreboard::types::VerifyMode;

const NESTED: &str = "fn walk() {\n    for a in 0..2 {\n        for b in 0..2 {\n            work(a, b);\n        }\n    }\n}\n";
const SINGLE: &str = "fn one() {\n    for i in 0..3 {\n        work(i);\n    }\n}\n";
const TS_TRY: &str = "try {\n  go();\n} catch (e) {\n  log(e);\n}\n";
const TSX_TRY: &str = "try { go(); } catch (e) {}\nconst v = <div />;\n";

/// A small universe: two Rust files with a loop (one nested), one without,
/// a `.ts` and a `.tsx` try/catch, files the oracle does not score, and one
/// Rust file one byte over the AST size cap.
fn universe_files() -> Vec<(String, String)> {
    let over_cap = format!("// {}\n", "x".repeat(1024 * 1024));
    vec![
        ("src/nested.rs".to_string(), NESTED.to_string()),
        ("src/single.rs".to_string(), SINGLE.to_string()),
        ("src/plain.rs".to_string(), "fn f() {}\n".to_string()),
        ("src/huge.rs".to_string(), over_cap),
        ("web/a.ts".to_string(), TS_TRY.to_string()),
        ("web/b.tsx".to_string(), TSX_TRY.to_string()),
        (
            "lib/C.java".to_string(),
            "class C { void m() {} }\n".to_string(),
        ),
        ("README.md".to_string(), "# Fixture\n".to_string()),
    ]
}

fn answers() -> OracleAnswers {
    let files = universe_files();
    OracleAnswers::compute(
        oracle(),
        files.iter().map(|(p, t)| (p.as_str(), t.as_str())),
    )
    .unwrap()
}

fn row(path: &str, line: Option<u32>) -> ResultRow {
    ResultRow {
        path: path.to_string(),
        score: 1.0,
        line,
        snippet: Vec::new(),
    }
}

fn page(rows: Vec<ResultRow>) -> ResultPage {
    ResultPage {
        rows,
        has_more: false,
        verify_mode: VerifyMode::Substring,
        degraded: Vec::new(),
    }
}

fn call(rows: Vec<ResultRow>, size_excluded_files: u64) -> AstPage {
    AstPage {
        page: page(rows),
        coverage: AstCoverage {
            size_excluded_files,
            ..AstCoverage::default()
        },
    }
}

fn target(pattern: &str, lang: OracleLang, precision: PrecisionClass) -> StructuralTarget {
    StructuralTarget {
        pattern: pattern.to_string(),
        lang,
        precision,
        expect_oracle_empty: false,
    }
}

/// `target` declared a false-positive guard (`expect_oracle_empty = true`).
fn guard(pattern: &str, lang: OracleLang) -> StructuralTarget {
    StructuralTarget {
        expect_oracle_empty: true,
        ..target(pattern, lang, PrecisionClass::Hard)
    }
}

fn files(entries: &[(&str, &[u32])]) -> MatchFiles {
    entries
        .iter()
        .map(|(p, lines)| (p.to_string(), lines.to_vec()))
        .collect()
}

fn detail(o: &CheckOutcome) -> String {
    o.detail().unwrap_or_default().to_string()
}

// --- oracle answers -------------------------------------------------------------

#[test]
fn the_oracle_answers_every_registered_pair_over_the_universe() {
    let a = answers();
    assert_eq!(
        a.definition("rust-nested-loop", OracleLang::Rust).unwrap(),
        &files(&[("src/nested.rs", &[2, 3]), ("src/single.rs", &[2])])
    );
    assert_eq!(
        a.intent("rust-nested-loop", OracleLang::Rust),
        Some(&files(&[("src/nested.rs", &[3])])),
        "only the inner loop sits in a loop of the same function"
    );
    assert_eq!(
        a.definition("try-catch", OracleLang::TypeScript).unwrap(),
        &files(&[("web/a.ts", &[1])])
    );
    assert_eq!(
        a.definition("try-catch", OracleLang::Tsx).unwrap(),
        &files(&[("web/b.tsx", &[1])]),
        ".tsx is parsed with the TSX grammar and kept apart from .ts"
    );
    // A registered pair with no match is answered, and empty.
    assert!(
        a.definition("go-select", OracleLang::Go)
            .unwrap()
            .is_empty()
    );
    // No query for the pair: an error, never "no match".
    assert!(
        a.definition("god-function", OracleLang::TypeScript)
            .is_err()
    );
    assert_eq!(a.intent("try-catch", OracleLang::TypeScript), None);
    // src/huge.rs is over the cap: outside the universe, counted as over-cap.
    assert_eq!(a.scored_files(OracleLang::Rust), 3);
    assert_eq!(a.scored_files(OracleLang::Go), 0);
    assert_eq!(a.over_cap(), 1);
}

#[test]
fn several_oracle_failures_report_the_first_failing_file_by_path_in_any_order() {
    let result = |path: &'static str| -> (&'static str, anyhow::Result<u32>) {
        if path.starts_with("ok") {
            (path, Ok(7))
        } else {
            (path, Err(anyhow::anyhow!("structural oracle on {path}")))
        }
    };
    for order in [
        ["ok.rs", "src/b.rs", "src/a.rs", "src/c.rs"],
        ["src/c.rs", "src/a.rs", "ok.rs", "src/b.rs"],
        ["src/b.rs", "src/c.rs", "ok.rs", "src/a.rs"],
    ] {
        let err = first_failure_by_path(order.into_iter().map(result).collect()).unwrap_err();
        assert_eq!(
            format!("{err:#}"),
            "3 universe files failed the structural oracle (the first by path is reported): \
             structural oracle on src/a.rs",
            "{order:?}"
        );
    }
    // One failure: its own error, as is.
    let one = first_failure_by_path(vec![result("ok.rs"), result("src/z.rs")]).unwrap_err();
    assert_eq!(format!("{one:#}"), "structural oracle on src/z.rs");
    // No failure: every file's report, in the order given.
    assert_eq!(
        first_failure_by_path(vec![result("ok2.rs"), result("ok1.rs")]).unwrap(),
        vec![("ok2.rs", 7), ("ok1.rs", 7)]
    );
}

#[test]
fn the_oracle_answers_do_not_depend_on_file_order_or_threads() {
    let mut reversed = universe_files();
    reversed.reverse();
    let again = OracleAnswers::compute(
        oracle(),
        reversed.iter().map(|(p, t)| (p.as_str(), t.as_str())),
    )
    .unwrap();
    assert_eq!(again, answers());
    assert_eq!(answers(), answers());
}

// --- splitting skim's rows --------------------------------------------------------

#[test]
fn rows_split_by_extension_keep_skim_order_and_tsx_apart() {
    let all = page(vec![
        row("web/z.ts", Some(1)),
        row("web/b.tsx", Some(1)),
        row("web/a.ts", Some(3)),
        row("web/c.mts", Some(2)),
        row("web/d.js", Some(1)),
        row("lib/C.java", Some(1)),
    ]);
    let ts = rows_in(&all, OracleLang::TypeScript);
    let paths: Vec<&str> = ts.rows.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, ["web/z.ts", "web/a.ts", "web/c.mts"]);
    let tsx = rows_in(&all, OracleLang::Tsx);
    assert_eq!(tsx.rows, vec![row("web/b.tsx", Some(1))]);
    assert!(rows_in(&all, OracleLang::Rust).rows.is_empty());
    // files_in is rows_in's distinct files, without copying a row.
    for lang in OracleLang::ALL {
        assert_eq!(
            files_in(&all, lang),
            distinct_files(&rows_in(&all, lang).rows),
            "{lang}"
        );
    }
}

#[test]
fn unscored_rows_count_every_row_no_entry_scores_per_called_pattern() {
    let patterns = BTreeMap::from([
        (
            "try-catch".to_string(),
            call(
                vec![
                    row("web/a.ts", Some(3)),  // scored (typescript entry)
                    row("web/b.tsx", Some(1)), // tsx: no entry
                    row("lib/C.java", None),   // no oracle grammar
                    row("README.md", None),    // no oracle grammar
                    row("run.sh", None),       // never AST-indexed
                ],
                0,
            ),
        ),
        (
            "rust-nested-loop".to_string(),
            call(vec![row("src/nested.rs", Some(3))], 0),
        ),
    ]);
    let targets = [
        target("try-catch", OracleLang::TypeScript, PrecisionClass::Hard),
        target(
            "rust-nested-loop",
            OracleLang::Rust,
            PrecisionClass::Ratchet,
        ),
    ];
    assert_eq!(
        unscored_rows(&targets, &patterns),
        BTreeMap::from([
            ("rust-nested-loop".to_string(), 0),
            ("try-catch".to_string(), 4),
        ])
    );
    assert!(unscored_rows(&targets, &BTreeMap::new()).is_empty());
}

#[test]
fn every_row_of_a_pattern_with_no_entry_is_unscored_even_in_an_oracle_language() {
    // The corpus scores rust-nested-loop in Rust only. skim is still called
    // for every catalog pattern: god-function (covered, but no entry here)
    // and deep-nesting (no oracle at all) count every row they return.
    let targets = [target(
        "rust-nested-loop",
        OracleLang::Rust,
        PrecisionClass::Ratchet,
    )];
    let patterns = BTreeMap::from([
        (
            "god-function".to_string(),
            call(
                vec![row("src/app.py", Some(310)), row("src/big.rs", Some(1))],
                0,
            ),
        ),
        (
            "deep-nesting".to_string(),
            call(
                vec![row("src/nested.rs", Some(2)), row("lib/C.java", None)],
                0,
            ),
        ),
        ("go-select".to_string(), call(Vec::new(), 0)),
        (
            "rust-nested-loop".to_string(),
            call(vec![row("src/nested.rs", Some(3))], 0),
        ),
    ]);
    assert_eq!(
        unscored_rows(&targets, &patterns),
        BTreeMap::from([
            ("deep-nesting".to_string(), 2),
            ("go-select".to_string(), 0),
            ("god-function".to_string(), 2),
            ("rust-nested-loop".to_string(), 0),
        ]),
        "a called pattern with no row still reads 0, so its first row is a visible move"
    );

    // The same predicate, row by row, in skim's order.
    let scored = BTreeSet::from([("rust-nested-loop", OracleLang::Rust)]);
    let god = &patterns["god-function"].page;
    let unscored: Vec<&str> = unscored_in("god-function", god, &scored)
        .map(|r| r.path.as_str())
        .collect();
    assert_eq!(unscored, ["src/app.py", "src/big.rs"]);
    let nested = &patterns["rust-nested-loop"].page;
    assert_eq!(unscored_in("rust-nested-loop", nested, &scored).count(), 0);
}

#[test]
fn skim_is_called_for_every_catalog_pattern_covered_or_not() {
    let called = called_patterns(catalog());
    let names: BTreeSet<&str> = catalog().iter().map(|p| p.name).collect();
    assert_eq!(called.iter().copied().collect::<BTreeSet<_>>(), names);
    assert_eq!(called.len(), names.len(), "each pattern once");
    assert!(called.windows(2).all(|w| w[0] < w[1]), "sorted: {called:?}");
    // No oracle query, and oracle queries with no corpus entry, alike.
    for pattern in [
        "deep-nesting",
        "java-synchronized",
        "ruby-begin-rescue",
        "go-select",
        "god-function",
        "excessive-params",
    ] {
        assert!(called.contains(&pattern), "{pattern}");
    }
}

// --- HARD checks ---------------------------------------------------------------------

#[test]
fn recall_passes_when_every_oracle_file_is_returned_and_names_the_missing_ones() {
    let oracle = files(&[("a.rs", &[1]), ("b.rs", &[2])]);
    assert!(check_recall(&[row("b.rs", None), row("a.rs", None)], &oracle).is_pass());
    let miss = check_recall(&[row("a.rs", None), row("x.rs", None)], &oracle);
    assert!(!miss.is_pass());
    assert!(
        detail(&miss).contains("missing 1 of 2"),
        "{}",
        detail(&miss)
    );
    assert!(detail(&miss).contains("b.rs"), "{}", detail(&miss));
    assert!(check_recall(&[], &MatchFiles::new()).is_pass());
}

#[test]
fn precision_passes_when_every_returned_file_matches_and_names_the_extras() {
    let oracle = files(&[("a.rs", &[1]), ("b.rs", &[2])]);
    assert!(check_precision(&[row("a.rs", None)], &oracle).is_pass());
    let extra = check_precision(
        &[row("a.rs", None), row("x.rs", None), row("x.rs", None)],
        &oracle,
    );
    assert!(!extra.is_pass());
    assert!(
        detail(&extra).contains("1 returned file(s)"),
        "{}",
        detail(&extra)
    );
    assert!(detail(&extra).contains("x.rs"), "{}", detail(&extra));
}

#[test]
fn coverage_must_equal_the_oracle_over_cap_count_with_nothing_undetermined() {
    let skim = |excluded: u64, undetermined: u64| AstCoverage {
        size_excluded_files: excluded,
        undetermined_files: undetermined,
        excluded_by_lang: if excluded > 0 {
            BTreeMap::from([("rust".to_string(), excluded)])
        } else {
            BTreeMap::new()
        },
    };
    assert!(check_coverage(&skim(2, 0), 2).is_pass());
    assert!(check_coverage(&AstCoverage::default(), 0).is_pass());

    let mismatch = check_coverage(&skim(1, 0), 2);
    assert!(!mismatch.is_pass());
    let d = detail(&mismatch);
    assert!(
        d.contains("is 1") && d.contains("counts 2") && d.contains("rust 1"),
        "{d}"
    );

    // The cap in the message is rendered from the oracle's constant.
    assert!(d.contains("over the 1 MiB AST cap"), "{d}");

    let clean_but_oracle_sees_one = check_coverage(&AstCoverage::default(), 1);
    assert!(detail(&clean_but_oracle_sees_one).contains("by language: none"));

    let undetermined = check_coverage(&skim(2, 3), 2);
    assert!(detail(&undetermined).contains("undetermined_files is 3"));
}

#[test]
fn byte_sizes_read_in_mebibytes_only_when_whole() {
    assert_eq!(byte_size(AST_SIZE_CAP_BYTES), "1 MiB");
    assert_eq!(byte_size(5 * 1024 * 1024), "5 MiB");
    assert_eq!(byte_size(100 * 1024), "102400 bytes");
    assert_eq!(byte_size(1024 * 1024 + 1), "1048577 bytes");
    assert_eq!(byte_size(0), "0 bytes");
}

// --- measurements ------------------------------------------------------------------------

#[test]
fn a_sample_measures_file_level_fractions_anchors_and_intent() {
    let a = answers();
    let t = target(
        "rust-nested-loop",
        OracleLang::Rust,
        PrecisionClass::Ratchet,
    );
    // skim: nested.rs anchored on the inner loop (an oracle match line),
    // single.rs anchored off any match line, plain.rs is a false positive.
    let rows = [
        row("src/nested.rs", Some(3)),
        row("src/single.rs", Some(1)),
        row("src/plain.rs", Some(1)),
    ];
    let s = measure("skim-ast-rust-nested-loop-rust", &t, &rows, &a).unwrap();
    assert_eq!(
        s,
        StructuralSample {
            id: "skim-ast-rust-nested-loop-rust".to_string(),
            pattern: "rust-nested-loop".to_string(),
            lang: OracleLang::Rust,
            precision_class: PrecisionClass::Ratchet,
            expect_oracle_empty: false,
            oracle_files: 2,
            skim_files: 3,
            recall: 1.0,
            precision: 0.6667,
            intent_files: Some(1),
            intent_recall: Some(1.0),
            intent_precision: Some(0.3333),
            line_on_match: 1,
        }
    );
}

#[test]
fn empty_denominators_read_as_one_and_patterns_without_intent_have_none() {
    let a = answers();
    let t = target("go-select", OracleLang::Go, PrecisionClass::Hard);
    let s = measure("x-ast-go-select-go", &t, &[], &a).unwrap();
    assert_eq!((s.oracle_files, s.skim_files), (0, 0));
    assert_eq!((s.recall, s.precision), (1.0, 1.0));
    assert_eq!(
        (s.intent_files, s.intent_recall, s.intent_precision),
        (None, None, None)
    );
    assert_eq!(s.line_on_match, 0);
    let ts = target("try-catch", OracleLang::TypeScript, PrecisionClass::Hard);
    let missed = measure("x", &ts, &[], &a).unwrap();
    assert_eq!((missed.recall, missed.precision), (0.0, 1.0));
}

#[test]
fn a_false_positive_guards_sample_says_it_is_one() {
    let a = answers();
    // A fixed guard reads oracle 0 / skim 0, like an ordinary empty entry:
    // only the flag tells the two apart in the report.
    let fixed = measure(
        "x-ast-go-select-go",
        &guard("go-select", OracleLang::Go),
        &[],
        &a,
    )
    .unwrap();
    assert!(fixed.expect_oracle_empty);
    assert_eq!((fixed.oracle_files, fixed.skim_files), (0, 0));
    let plain = target("go-select", OracleLang::Go, PrecisionClass::Hard);
    assert!(
        !measure("x-ast-go-select-go", &plain, &[], &a)
            .unwrap()
            .expect_oracle_empty
    );
}

#[test]
fn a_scored_entry_runs_all_three_checks_against_its_patterns_call() {
    let evidence = StructuralEvidence {
        answers: answers(),
        patterns: BTreeMap::from([(
            "try-catch".to_string(),
            call(vec![row("web/a.ts", Some(1)), row("web/x.ts", Some(1))], 1),
        )]),
    };
    let t = target("try-catch", OracleLang::TypeScript, PrecisionClass::Hard);
    let rows = rows_in(&evidence.patterns["try-catch"].page, OracleLang::TypeScript).rows;
    let score = score_entry("x-ast-try-catch-typescript", &t, &rows, &evidence).unwrap();
    assert!(score.recall.is_pass());
    assert!(detail(&score.precision).contains("web/x.ts"));
    assert!(score.coverage.is_pass(), "{:?}", score.coverage);
    assert_eq!(score.sample.line_on_match, 1);

    let not_called = target("rust-nested-loop", OracleLang::Rust, PrecisionClass::Hard);
    let err = score_entry("x", &not_called, &[], &evidence).unwrap_err();
    assert!(
        format!("{err:#}").contains("--ast rust-nested-loop"),
        "{err:#}"
    );
}

#[test]
fn an_entry_is_vacuous_only_when_the_oracle_and_skim_both_find_nothing() {
    let a = answers();
    let go = target("go-select", OracleLang::Go, PrecisionClass::Hard);
    assert!(is_vacuous(&go, &[], &a).unwrap());
    assert!(!is_vacuous(&go, &[row("x.go", Some(1))], &a).unwrap());
    let ts = target("try-catch", OracleLang::TypeScript, PrecisionClass::Hard);
    assert!(!is_vacuous(&ts, &[], &a).unwrap(), "the oracle finds a.ts");
    let unknown = target("god-function", OracleLang::Go, PrecisionClass::Hard);
    assert!(is_vacuous(&unknown, &[], &a).is_err());
}

#[test]
fn a_false_positive_guard_is_vacuous_only_without_a_scored_file_in_its_language() {
    let a = answers();
    // The universe has scored TypeScript files: both empty is the fixed state.
    assert!(!is_vacuous(&guard("try-catch-finally", OracleLang::TypeScript), &[], &a).unwrap());
    // No Go file at all: the guard judges nothing, rows or not.
    let go = guard("go-select", OracleLang::Go);
    assert!(is_vacuous(&go, &[], &a).unwrap());
    assert!(is_vacuous(&go, &[row("cmd/x.go", Some(1))], &a).unwrap());
    // The only Python file is over the AST size cap: outside the AST
    // universe, so it is not a scored file either.
    let over_cap = format!("# {}\n", "x".repeat(1024 * 1024));
    let capped = OracleAnswers::compute(oracle(), [("big.py", over_cap.as_str())]).unwrap();
    assert_eq!(capped.scored_files(OracleLang::Python), 0);
    assert!(
        is_vacuous(
            &guard("python-try-except", OracleLang::Python),
            &[],
            &capped
        )
        .unwrap()
    );
    // Unflagged entries keep the both-empty rule.
    let plain = target(
        "python-try-except",
        OracleLang::Python,
        PrecisionClass::Hard,
    );
    assert!(is_vacuous(&plain, &[], &capped).unwrap());
    assert!(!is_vacuous(&plain, &[row("big.py", Some(1))], &capped).unwrap());
}

#[test]
fn a_false_positive_guard_is_not_vacuous_and_its_flag_must_match_an_empty_oracle() {
    let a = answers();
    // The oracle finds no try/catch/finally anywhere in the universe.
    let fp = guard("try-catch-finally", OracleLang::TypeScript);
    assert!(
        !is_vacuous(&fp, &[], &a).unwrap(),
        "both empty is the fixed state"
    );
    assert!(!is_vacuous(&fp, &[row("web/a.ts", Some(1))], &a).unwrap());
    assert_eq!(unexpected_oracle_matches(&fp, &a).unwrap(), None);
    // A flag on an entry whose oracle matches is stale.
    let stale = guard("try-catch", OracleLang::TypeScript);
    assert_eq!(
        unexpected_oracle_matches(&stale, &a).unwrap(),
        Some(&files(&[("web/a.ts", &[1])]))
    );
    // Unflagged entries are never reported, whatever the oracle finds.
    let plain = target("try-catch", OracleLang::TypeScript, PrecisionClass::Hard);
    assert_eq!(unexpected_oracle_matches(&plain, &a).unwrap(), None);
    assert!(unexpected_oracle_matches(&guard("god-function", OracleLang::Go), &a).is_err());
}

#[test]
fn a_false_positive_guard_passes_when_skim_is_empty_and_fails_precision_on_a_row() {
    let a = answers();
    let evidence = |rows: Vec<ResultRow>| StructuralEvidence {
        answers: a.clone(),
        patterns: BTreeMap::from([("try-catch-finally".to_string(), call(rows, 1))]),
    };
    let fp = guard("try-catch-finally", OracleLang::TypeScript);

    // Fixed: no row, nothing expected. Recall and precision read 1 (an empty
    // denominator), and every HARD check passes.
    let fixed = score_entry("skim-fp", &fp, &[], &evidence(Vec::new())).unwrap();
    assert!(fixed.recall.is_pass() && fixed.precision.is_pass() && fixed.coverage.is_pass());
    assert_eq!((fixed.sample.oracle_files, fixed.sample.skim_files), (0, 0));
    assert_eq!((fixed.sample.recall, fixed.sample.precision), (1.0, 1.0));

    // The false positive (back): precision fails and reads 0.
    let rows = vec![row("web/a.ts", Some(1))];
    let broken = score_entry("skim-fp", &fp, &rows, &evidence(rows.clone())).unwrap();
    assert!(broken.recall.is_pass());
    assert!(!broken.precision.is_pass());
    assert!(
        detail(&broken.precision).contains("web/a.ts"),
        "{:?}",
        broken.precision
    );
    assert_eq!(broken.sample.precision, 0.0);
}

// --- report sections -------------------------------------------------------------------------

#[test]
fn intent_fields_are_omitted_when_absent_and_samples_round_trip() {
    let a = answers();
    let plain = measure(
        "x-ast-try-catch-typescript",
        &target("try-catch", OracleLang::TypeScript, PrecisionClass::Hard),
        &[row("web/a.ts", Some(1))],
        &a,
    )
    .unwrap();
    let json = serde_json::to_string(&plain).unwrap();
    assert!(!json.contains("intent"), "{json}");
    assert!(
        json.contains("\"lang\":\"typescript\"") && json.contains("\"precision_class\":\"hard\"")
    );
    assert_eq!(
        serde_json::from_str::<StructuralSample>(&json).unwrap(),
        plain
    );

    let nested = measure(
        "x-ast-rust-nested-loop-rust",
        &target(
            "rust-nested-loop",
            OracleLang::Rust,
            PrecisionClass::Ratchet,
        ),
        &[row("src/nested.rs", Some(3))],
        &a,
    )
    .unwrap();
    let json = serde_json::to_string(&nested).unwrap();
    for key in ["intent_files", "intent_recall", "intent_precision"] {
        assert!(json.contains(key), "{key}: {json}");
    }
    assert_eq!(
        serde_json::from_str::<StructuralSample>(&json).unwrap(),
        nested
    );
}

#[test]
fn the_coverage_comparison_lists_each_distinct_skim_value() {
    assert_eq!(coverage_comparison(&StructuralEvidence::default()), None);
    let evidence = StructuralEvidence {
        answers: answers(),
        patterns: BTreeMap::from([
            ("a".to_string(), call(Vec::new(), 1)),
            ("b".to_string(), call(Vec::new(), 1)),
            ("c".to_string(), call(Vec::new(), 0)),
        ]),
    };
    assert_eq!(
        coverage_comparison(&evidence),
        Some(CoverageComparison {
            oracle_over_cap: 1,
            skim_size_excluded_files: vec![0, 1],
            skim_undetermined_files: vec![0],
        })
    );
}

#[test]
fn uncovered_patterns_list_the_oracle_gaps_and_the_patterns_no_corpus_scores() {
    let none = uncovered_patterns(catalog(), std::iter::empty());
    let names: Vec<&str> = none.iter().map(|p| p.name.as_str()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "sorted by name");
    assert_eq!(names.len(), catalog().len());
    let cause = |list: &[UncoveredPattern], name: &str| {
        list.iter().find(|p| p.name == name).map(|p| p.cause)
    };
    for no_oracle in ["deep-nesting", "java-synchronized", "ruby-begin-rescue"] {
        assert_eq!(
            cause(&none, no_oracle),
            Some(UncoveredCause::NoOracle),
            "{no_oracle}"
        );
    }
    assert_eq!(cause(&none, "try-catch"), Some(UncoveredCause::NoEntry));
    let reason = &none.iter().find(|p| p.name == "try-catch").unwrap().reason;
    // The languages follow `OracleLang`'s order, not the alphabet.
    assert!(
        reason.contains("(the oracle covers it in: typescript, tsx, javascript)"),
        "{reason}"
    );

    let golden = parse_golden(
        "corpus = \"skim\"\ncommit = \"b8a0a79463382347820f1c2572bde37b68e87c76\"\n\
         [[ast]]\nid = \"skim-ast-try-catch-tsx\"\npattern = \"try-catch\"\nlang = \"tsx\"\nprecision = \"hard\"\n",
    )
    .unwrap();
    let with_entry = uncovered_patterns(catalog(), [&golden]);
    assert_eq!(
        cause(&with_entry, "try-catch"),
        None,
        "an entry in any language covers it"
    );
    assert_eq!(with_entry.len(), none.len() - 1);
}
