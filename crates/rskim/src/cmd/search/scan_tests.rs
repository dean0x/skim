//! Tests for the scan engine: exact counts, the parse budget and its order, the
//! bounds, deterministic quotas and the second read.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::Path;

use tempfile::TempDir;

use super::*;
use crate::cmd::search::matcher::{CaseMode, MatcherSpec, PatternKind};
use crate::cmd::search::universe::list_universe;

// ============================================================================
// Helpers
// ============================================================================

struct Tree {
    _dir: TempDir,
    root: PathBuf,
}

impl Tree {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        Tree { _dir: dir, root }
    }

    fn write(&self, rel: &str, content: impl AsRef<[u8]>) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

fn matcher(pattern: &str) -> Matcher {
    Matcher::new(MatcherSpec {
        pattern,
        kind: PatternKind::Literal,
        case: CaseMode::Sensitive,
        word: false,
    })
    .unwrap()
}

fn pattern(p: &str, identifier: bool) -> ScanQuery {
    ScanQuery {
        plan: QueryPlan::Pattern(Box::new(matcher(p))),
        identifier,
    }
}

fn words(ws: &[&str]) -> ScanQuery {
    ScanQuery {
        plan: QueryPlan::Words(ws.iter().map(|w| matcher(w)).collect()),
        identifier: false,
    }
}

fn config<'a>(
    root: &'a Path,
    query: &'a ScanQuery,
    limits: Limits,
    context: usize,
) -> ScanConfig<'a> {
    ScanConfig {
        root,
        query,
        limits,
        context,
    }
}

fn scan(tree: &Tree, query: &ScanQuery, limits: Limits, context: usize) -> ScanOutput {
    let listing = list_universe(&tree.root).unwrap();
    let config = config(&tree.root, query, limits, context);
    count(&config, listing).unwrap().label()
}

fn file<'a>(out: &'a ScanOutput, rel: &str) -> &'a FileScan {
    out.files
        .iter()
        .find(|f| f.path == Path::new(rel))
        .unwrap_or_else(|| panic!("no matched file {rel}: {:?}", out.files))
}

// ============================================================================
// Lines and clipping
// ============================================================================

#[test]
fn lines_split_on_newline_only_and_a_final_newline_starts_no_line() {
    let lines = |t| {
        numbered_lines(t)
            .map(|(n, s, l)| (n, s, l.to_string()))
            .collect::<Vec<_>>()
    };
    assert!(lines("").is_empty());
    assert_eq!(lines("a"), [(1, 0, "a".to_string())]);
    assert_eq!(lines("a\n"), [(1, 0, "a".to_string())]);
    assert_eq!(
        lines("a\n\nb\r\n"),
        [
            (1, 0, "a".to_string()),
            (2, 2, String::new()),
            (3, 3, "b\r".to_string())
        ]
    );
}

#[test]
fn a_long_line_is_clipped_at_a_character_boundary_with_the_hidden_byte_count() {
    let (text, hidden) = clip_line(&"x".repeat(1000), LINE_WIDTH_CAP);
    assert_eq!((text.len(), hidden), (300, 700));

    // A two-byte character straddles the cap: the cut moves back to its start.
    let line = format!("{}é{}", "x".repeat(299), "y".repeat(10));
    let (text, hidden) = clip_line(&line, LINE_WIDTH_CAP);
    assert_eq!(text.len(), 299);
    assert_eq!(hidden, 12);

    assert_eq!(
        clip_line("short\r", LINE_WIDTH_CAP),
        ("short".to_string(), 0)
    );
}

#[test]
fn the_scan_pool_never_has_more_than_eight_threads() {
    assert_eq!(pool_threads(0), 1);
    assert_eq!(pool_threads(1), 1);
    assert_eq!(pool_threads(4), 4);
    assert_eq!(pool_threads(8), 8);
    assert_eq!(pool_threads(64), MAX_SCAN_THREADS);
    assert_eq!(MAX_SCAN_THREADS, 8);
}

// ============================================================================
// Phase A: exact counts
// ============================================================================

#[test]
fn every_matching_line_is_counted_and_every_file_is_accounted() {
    let tree = Tree::new();
    tree.write("a.rs", "needle one\nplain\nneedle two\nneedle three\n");
    tree.write("b.txt", "a needle\n");
    tree.write("none.txt", "nothing here\n");
    tree.write("bin.dat", b"needle\0needle");
    tree.write("latin.txt", b"needle caf\xe9\n");

    let out = scan(&tree, &pattern("needle", false), Limits::default(), 0);

    assert_eq!(out.files.len(), 2);
    assert_eq!(file(&out, "a.rs").lines, 3);
    assert_eq!(file(&out, "b.txt").lines, 1);
    assert_eq!(out.total_lines(), 4);
    assert_eq!(out.searched_files, 3, "a.rs, b.txt and none.txt");
    assert_eq!(out.left_out.get(LeftOut::Binary), 1);
    assert_eq!(out.left_out.get(LeftOut::NotUtf8), 1);
    assert_eq!(out.left_out.total(), 2);
    assert_eq!(out.cost.files_read, 3);
}

#[test]
fn a_word_query_counts_the_matching_lines_of_each_word_and_the_union() {
    let tree = Tree::new();
    tree.write("a.txt", "alpha\nbeta\nalpha beta\ngamma\nalpha\n");
    tree.write("b.txt", "beta only\n");

    let out = scan(
        &tree,
        &words(&["alpha", "beta", "delta"]),
        Limits::default(),
        0,
    );

    let a = file(&out, "a.txt");
    assert_eq!(a.word_lines, [3, 2, 0]);
    assert_eq!(a.lines, 4, "lines that hold any word");
    assert_eq!(file(&out, "b.txt").word_lines, [0, 1, 0]);
}

#[test]
fn a_counts_only_scan_has_counts_and_no_labels_or_lines() {
    let tree = Tree::new();
    tree.write("a.rs", "fn needle() {}\nneedle();\n");
    let query = pattern("needle", true);
    let listing = list_universe(&tree.root).unwrap();
    let config = config(&tree.root, &query, Limits::default(), 0);

    let out = count(&config, listing).unwrap().into_counts();

    let a = file(&out, "a.rs");
    assert_eq!(a.lines, 2);
    assert_eq!(a.roles.total(), 0);
    assert!(a.records.is_empty() && a.defs.is_empty());
    assert_eq!(out.cost.files_parsed, 0);
}

// ============================================================================
// Phase B: roles, records, holders
// ============================================================================

#[test]
fn the_definition_is_kept_apart_and_calls_carry_their_holder() {
    let tree = Tree::new();
    tree.write(
        "src/lib.rs",
        "pub fn launch() {}\n\nfn run() {\n    launch();\n}\n// launch\n",
    );

    let out = scan(&tree, &pattern("launch", true), Limits::default(), 0);

    let f = file(&out, "src/lib.rs");
    assert_eq!(f.unlabelled, None);
    assert_eq!(f.defs.len(), 1);
    assert_eq!(f.defs[0].line, 1);
    assert_eq!(f.defs[0].def_lines, Some((1, 1)));
    assert_eq!(f.defs[0].column, 8);
    let call = f.records.iter().find(|r| r.role == Role::Call).unwrap();
    assert_eq!(call.line, 4);
    assert_eq!(call.holder.as_ref().unwrap().name, "run");
    assert_eq!(f.roles.get(Role::Def), 1);
    assert_eq!(f.roles.get(Role::Call), 1);
    assert_eq!(f.roles.get(Role::Comment), 1);
    assert_eq!(f.roles.total(), f.lines);
}

#[test]
fn a_hit_in_a_longer_identifier_is_partial_in_an_identifier_query_only() {
    let tree = Tree::new();
    tree.write("a.rs", "fn launch_pool() {}\nlet x = launch;\n");

    let ident = scan(&tree, &pattern("launch", true), Limits::default(), 0);
    let f = file(&ident, "a.rs");
    assert_eq!(f.roles.get(Role::Partial), 1);
    assert_eq!(f.roles.get(Role::Ref), 1);

    let plain = scan(&tree, &pattern("launch", false), Limits::default(), 0);
    assert_eq!(file(&plain, "a.rs").roles.get(Role::Partial), 0);
}

#[test]
fn a_file_with_no_grammar_gives_text_and_costs_no_parse() {
    let tree = Tree::new();
    tree.write("notes.txt", "launch the worker\n");
    tree.write("Makefile", "launch:\n\techo launch\n");

    let out = scan(&tree, &pattern("launch", true), Limits::default(), 0);

    assert_eq!(file(&out, "notes.txt").roles.get(Role::Text), 1);
    assert_eq!(file(&out, "Makefile").roles.get(Role::Text), 2);
    assert_eq!(out.cost.files_parsed, 0);
    assert_eq!(out.parse.unlabelled_files, 0);
}

#[test]
fn a_tsx_file_is_parsed_with_the_tsx_grammar() {
    let tree = Tree::new();
    tree.write(
        "Page.tsx",
        "function Page() {\n  return <Card title=\"x\" />;\n}\nconst top = Card();\n",
    );

    let out = scan(&tree, &pattern("Card", true), Limits::default(), 0);

    let f = file(&out, "Page.tsx");
    assert_eq!(f.unlabelled, None);
    assert_eq!(f.roles.get(Role::Call), 1);
    assert_eq!(f.roles.get(Role::Ref), 1, "the JSX element");
}

// ============================================================================
// Bounds
// ============================================================================

#[test]
fn the_stored_line_cap_keeps_the_first_lines_and_counts_the_rest() {
    let tree = Tree::new();
    let body: String = (0..20).map(|i| format!("// needle {i}\n")).collect();
    tree.write("a.rs", &body);
    let limits = Limits {
        max_stored_lines: 5,
        ..Limits::default()
    };

    let out = scan(&tree, &pattern("needle", false), limits, 0);

    let f = file(&out, "a.rs");
    assert_eq!(f.lines, 20);
    assert_eq!(f.roles.get(Role::Comment), 20, "roles count every line");
    assert_eq!(
        f.records.iter().map(|r| r.line).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    assert_eq!(out.total_lines(), 20);
    assert_eq!(out.listed_lines(), 5);
}

#[test]
fn the_stored_line_quota_is_assigned_in_parse_order_across_files() {
    let tree = Tree::new();
    tree.write("src/a.rs", "needle\nneedle\nneedle\n");
    tree.write("docs/b.md", "needle\nneedle\nneedle\n");
    tree.write("tests/c.rs", "needle\nneedle\nneedle\n");
    let limits = Limits {
        max_stored_lines: 4,
        ..Limits::default()
    };

    let out = scan(&tree, &pattern("needle", false), limits, 0);

    // Parse order: source, then test, then docs.
    assert_eq!(file(&out, "src/a.rs").records.len(), 3);
    assert_eq!(file(&out, "tests/c.rs").records.len(), 1);
    assert_eq!(file(&out, "docs/b.md").records.len(), 0);
    assert_eq!(out.listed_lines(), 4);
    assert_eq!(out.total_lines(), 9);
}

#[test]
fn the_definition_cap_keeps_the_first_definitions_in_parse_order_and_counts_the_rest() {
    let tree = Tree::new();
    tree.write(
        "src/a.rs",
        "fn needle() {}\nfn needle() {}\nfn needle() {}\n",
    );
    tree.write("src/b.rs", "fn needle() {}\n");
    tree.write("src/c.rs", "fn needle() {}\n");
    let limits = Limits {
        max_def_sites: 3,
        ..Limits::default()
    };

    let out = scan(&tree, &pattern("needle", true), limits, 0);

    let defs = |rel: &str| {
        file(&out, rel)
            .defs
            .iter()
            .map(|r| r.line)
            .collect::<Vec<_>>()
    };
    assert_eq!(defs("src/a.rs"), [1, 2, 3]);
    assert_eq!(defs("src/b.rs"), Vec::<u32>::new());
    assert_eq!(defs("src/c.rs"), Vec::<u32>::new());
    for rel in ["src/a.rs", "src/b.rs", "src/c.rs"] {
        let expected = if rel == "src/a.rs" { 3 } else { 1 };
        assert_eq!(
            file(&out, rel).roles.get(Role::Def),
            expected,
            "roles count every definition line: {rel}"
        );
    }
    assert_eq!(out.total_lines(), 5);
}

#[test]
fn the_definition_cap_stops_inside_a_file_and_gives_the_same_result_twice() {
    let tree = Tree::new();
    tree.write(
        "src/a.rs",
        "fn needle() {}\nfn needle() {}\nfn needle() {}\n",
    );
    let limits = Limits {
        max_def_sites: 2,
        ..Limits::default()
    };

    let first = scan(&tree, &pattern("needle", true), limits, 0);
    let second = scan(&tree, &pattern("needle", true), limits, 0);

    let lines = |out: &ScanOutput| {
        file(out, "src/a.rs")
            .defs
            .iter()
            .map(|r| r.line)
            .collect::<Vec<_>>()
    };
    assert_eq!(lines(&first), [1, 2]);
    assert_eq!(lines(&first), lines(&second));
    assert_eq!(file(&first, "src/a.rs").roles.get(Role::Def), 3);
}

#[test]
fn two_scans_of_one_tree_give_equal_results_and_cost_counters() {
    let tree = Tree::new();
    for i in 0..30 {
        tree.write(
            &format!("src/m{i}.rs"),
            format!("fn needle{i}() {{}}\nfn run() {{ needle{i}(); needle(); }}\n"),
        );
    }
    let limits = Limits {
        max_stored_lines: 7,
        parse_files: 10,
        ..Limits::default()
    };

    let first = scan(&tree, &pattern("needle", true), limits, 0);
    let second = scan(&tree, &pattern("needle", true), limits, 0);

    assert_eq!(first, second);
    assert_eq!(first.cost, second.cost);
}

#[test]
fn a_file_over_the_per_file_parse_cap_is_unlabelled_with_its_lines_counted() {
    let tree = Tree::new();
    let big = format!("fn needle() {{}}\n{}", "// pad\n".repeat(200));
    tree.write("big.rs", &big);
    let limits = Limits {
        per_file_parse_cap: 1000,
        ..Limits::default()
    };

    let out = scan(&tree, &pattern("needle", false), limits, 0);

    let f = file(&out, "big.rs");
    assert_eq!(f.unlabelled, Some(Unlabelled::ParseCap));
    assert_eq!(f.roles.get(Role::Unlabelled), 1);
    assert_eq!(
        f.records.len(),
        1,
        "the line is still listed, as unlabelled"
    );
    assert_eq!(out.parse.unlabelled_files, 1);
    assert_eq!(out.parse.by_reason[&Unlabelled::ParseCap], 1);
    assert!(!out.parse.budget_hit, "the cap is not the budget");
    assert_eq!(out.cost.files_parsed, 0);
}

#[test]
fn a_matched_file_above_the_walk_node_cap_is_unlabelled_with_walk_cap() {
    let tree = Tree::new();
    // Three syntax nodes per statement: more than the 100,000 node cap.
    let source = format!("needle;\n{}", "x;\n".repeat(60_000));
    tree.write("wide.js", &source);

    let out = scan(&tree, &pattern("needle", true), Limits::default(), 0);

    let f = file(&out, "wide.js");
    assert_eq!(f.unlabelled, Some(Unlabelled::WalkCap));
    assert_eq!(f.roles.get(Role::Unlabelled), 1);
    assert_eq!(out.parse.unlabelled_files, 1);
}

#[test]
fn the_parse_budget_labels_files_in_parse_order_and_names_the_rest() {
    let tree = Tree::new();
    // Both match. The file with a declaration keyword before the match is
    // parsed first.
    tree.write("a_calls.rs", "fn run() {\n    target();\n}\n");
    tree.write("z_defines.rs", "fn target() {}\n");
    let limits = Limits {
        parse_files: 1,
        ..Limits::default()
    };

    let out = scan(&tree, &pattern("target", true), limits, 0);

    assert_eq!(file(&out, "z_defines.rs").unlabelled, None);
    assert_eq!(file(&out, "z_defines.rs").roles.get(Role::Def), 1);
    assert_eq!(
        file(&out, "a_calls.rs").unlabelled,
        Some(Unlabelled::Budget)
    );
    assert_eq!(file(&out, "a_calls.rs").roles.get(Role::Unlabelled), 1);
    assert!(out.parse.budget_hit);
    assert_eq!(out.parse.unlabelled_files, 1);
    assert_eq!(out.cost.files_parsed, 1);
}

#[test]
fn the_parse_byte_budget_skips_a_file_it_cannot_fit_and_keeps_a_smaller_one() {
    let tree = Tree::new();
    tree.write(
        "src/big.rs",
        format!("fn needle() {{}}\n{}", "// x\n".repeat(100)),
    );
    tree.write("src/small.rs", "fn needle() {}\n");
    let limits = Limits {
        parse_bytes: 200,
        ..Limits::default()
    };

    let out = scan(&tree, &pattern("needle", true), limits, 0);

    assert_eq!(
        file(&out, "src/big.rs").unlabelled,
        Some(Unlabelled::Budget)
    );
    assert_eq!(file(&out, "src/small.rs").unlabelled, None);
    assert!(out.parse.budget_hit);
}

// ============================================================================
// The second read
// ============================================================================

#[test]
fn a_file_that_changes_between_the_reads_is_counted_as_changed_and_not_listed() {
    let tree = Tree::new();
    tree.write("a.rs", "fn needle() {}\nneedle();\n");
    tree.write("b.rs", "fn needle() {}\n");
    let query = pattern("needle", true);
    let listing = list_universe(&tree.root).unwrap();
    let config = config(&tree.root, &query, Limits::default(), 0);

    let counted = count(&config, listing).unwrap();
    // The change happens between phase A and phase B.
    tree.write("a.rs", "fn something_else() {}\n// and more text\n");
    let out = counted.label();

    let a = file(&out, "a.rs");
    assert_eq!(a.unlabelled, Some(Unlabelled::Changed));
    assert_eq!(a.lines, 2, "the count of phase A stands");
    assert_eq!(a.roles.get(Role::Unlabelled), 2);
    assert!(a.records.is_empty() && a.defs.is_empty());
    assert_eq!(file(&out, "b.rs").unlabelled, None);
    // The totals still add up: every matching line is listed or counted.
    assert_eq!(out.total_lines(), 3);
    assert_eq!(out.listed_lines(), 1);
    assert_eq!(out.parse.by_reason[&Unlabelled::Changed], 1);
}

#[test]
fn a_file_that_changes_to_the_same_length_is_counted_as_changed() {
    let tree = Tree::new();
    tree.write("a.rs", "fn needle() {}\nneedle();\n");
    let query = pattern("needle", true);
    let listing = list_universe(&tree.root).unwrap();
    let config = config(&tree.root, &query, Limits::default(), 0);

    let counted = count(&config, listing).unwrap();
    // One letter moves: the text has the same length and no longer matches.
    tree.write("a.rs", "fn needle() {}\nneedel();\n");
    let out = counted.label();

    let a = file(&out, "a.rs");
    assert_eq!(a.unlabelled, Some(Unlabelled::Changed));
    assert_eq!(a.lines, 2);
    assert!(a.records.is_empty() && a.defs.is_empty());
}

#[test]
fn a_file_that_is_deleted_between_the_reads_is_counted_as_changed() {
    let tree = Tree::new();
    tree.write("a.rs", "fn needle() {}\n");
    let query = pattern("needle", true);
    let listing = list_universe(&tree.root).unwrap();
    let config = config(&tree.root, &query, Limits::default(), 0);

    let counted = count(&config, listing).unwrap();
    fs::remove_file(tree.root.join("a.rs")).unwrap();
    let out = counted.label();

    assert_eq!(file(&out, "a.rs").unlabelled, Some(Unlabelled::Changed));
    assert_eq!(out.total_lines(), 1);
}

// ============================================================================
// Word view: the best lines
// ============================================================================

#[test]
fn the_word_view_keeps_the_lines_with_the_most_distinct_words_then_the_earliest() {
    let tree = Tree::new();
    tree.write(
        "a.txt",
        "alpha\nalpha beta\nbeta\nalpha beta gamma\nalpha beta\n",
    );

    let out = scan(
        &tree,
        &words(&["alpha", "beta", "gamma"]),
        Limits::default(),
        0,
    );

    let f = file(&out, "a.txt");
    assert_eq!(
        f.records
            .iter()
            .map(|r| (r.line, r.distinct_words))
            .collect::<Vec<_>>(),
        [(2, 2), (4, 3)],
        "line 4 holds three words; line 2 is the earliest with two"
    );
    assert_eq!(f.lines, 5);
}

#[test]
fn a_better_line_pushes_out_the_latest_of_the_equal_worst_lines() {
    let tree = Tree::new();
    tree.write("a.txt", "alpha\nalpha\nalpha beta\n");

    let out = scan(&tree, &words(&["alpha", "beta"]), Limits::default(), 0);

    let f = file(&out, "a.txt");
    assert_eq!(
        f.records.iter().map(|r| r.line).collect::<Vec<_>>(),
        [1, 3],
        "of the two one-word lines, the earlier stays"
    );
}

#[test]
fn the_word_view_stores_two_lines_per_file_in_rank_order_up_to_the_cap() {
    let tree = Tree::new();
    tree.write("src/a.rs", "alpha beta\nalpha\nbeta\n");
    tree.write("docs/b.md", "alpha beta\nalpha\nbeta\n");
    let limits = Limits {
        max_stored_lines: 3,
        ..Limits::default()
    };

    let out = scan(&tree, &words(&["alpha", "beta"]), limits, 0);

    assert_eq!(file(&out, "src/a.rs").records.len(), 2);
    assert_eq!(file(&out, "docs/b.md").records.len(), 1);
}

// ============================================================================
// Context
// ============================================================================

#[test]
fn context_lines_surround_stored_records_and_merge_where_they_touch() {
    let tree = Tree::new();
    tree.write("a.txt", "l1\nl2\nneedle\nl4\nl5\nl6\nneedle\nl8\n");

    let out = scan(&tree, &pattern("needle", false), Limits::default(), 1);

    let f = file(&out, "a.txt");
    assert_eq!(
        f.context.iter().map(|c| c.line).collect::<Vec<_>>(),
        [2, 4, 6, 8]
    );
    assert_eq!(f.text_of(3), Some(("needle", 0)));
    assert_eq!(f.text_of(2), Some(("l2", 0)));
    assert_eq!(f.text_of(5), None);
    assert_eq!(out.context_hidden, 0);
}

#[test]
fn a_context_line_that_is_a_record_is_not_repeated_as_context() {
    let tree = Tree::new();
    tree.write("a.txt", "needle\nneedle\nl3\n");

    let out = scan(&tree, &pattern("needle", false), Limits::default(), 1);

    let f = file(&out, "a.txt");
    assert_eq!(f.context.iter().map(|c| c.line).collect::<Vec<_>>(), [3]);
}

#[test]
fn context_stops_at_the_cap_and_counts_the_hidden_lines() {
    let tree = Tree::new();
    let mut body = String::new();
    for _ in 0..10 {
        body.push_str("needle\n");
        body.push_str(&"pad\n".repeat(50));
    }
    tree.write("a.txt", &body);
    let limits = Limits {
        max_context_lines: 30,
        ..Limits::default()
    };

    let out = scan(&tree, &pattern("needle", false), limits, 5);

    let f = file(&out, "a.txt");
    assert_eq!(f.context.len(), 30, "context stops at the cap");
    // 10 records, each with up to 5 lines above and 5 below, clipped at the
    // file start: 9 * 10 + 5 = 95 wanted lines.
    assert_eq!(out.context_hidden, 95 - 30);
}

#[test]
fn no_context_is_kept_without_the_flag() {
    let tree = Tree::new();
    tree.write("a.txt", "l1\nneedle\nl3\n");

    let out = scan(&tree, &pattern("needle", false), Limits::default(), 0);

    assert!(file(&out, "a.txt").context.is_empty());
}

// ============================================================================
// Lines that are long
// ============================================================================

#[test]
fn a_stored_line_is_clipped_and_keeps_the_hidden_byte_count() {
    let tree = Tree::new();
    tree.write("a.txt", format!("needle {}\n", "x".repeat(1000)));

    let out = scan(&tree, &pattern("needle", false), Limits::default(), 0);

    let record = &file(&out, "a.txt").records[0];
    assert_eq!(record.text.len(), LINE_WIDTH_CAP);
    assert_eq!(record.truncated_bytes, 1007 - 300);
}
