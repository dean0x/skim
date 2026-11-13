//! Unit tests for `metrics.rs` (co-located file, `#[path]`-included).

use super::*;
use crate::scoreboard::golden::parse_golden;
use crate::scoreboard::runner::{SweepPage, TextOutput};
use crate::scoreboard::test_support::FixtureRepo;
use crate::scoreboard::types::SnippetLine;
use crate::scoreboard::universe::{GitIsolation, text_universe};

const SHA: &str = "b8a0a79463382347820f1c2572bde37b68e87c76";

fn row(path: &str, score: f64) -> ResultRow {
    ResultRow {
        path: path.to_string(),
        score,
        line: Some(1),
        snippet: Vec::new(),
    }
}

fn rows(paths: &[&str]) -> Vec<ResultRow> {
    let n = paths.len();
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| row(p, (n - i) as f64))
        .collect()
}

fn page(rows: Vec<ResultRow>, has_more: bool) -> ResultPage {
    ResultPage {
        rows,
        has_more,
        ..ResultPage::default()
    }
}

fn gt(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|p| p.to_string()).collect()
}

/// Honest pages of `paths` at `limit`.
fn honest_sweep(paths: &[&str], limit: u32) -> Sweep {
    let all = rows(paths);
    let l = limit as usize;
    let mut pages = Vec::new();
    let mut offset = 0;
    loop {
        let end = (offset + l).min(all.len());
        pages.push(SweepPage {
            offset: offset as u64,
            page: page(all[offset..end].to_vec(), end < all.len()),
        });
        offset += l;
        if offset >= all.len() {
            break;
        }
    }
    Sweep { limit, pages }
}

fn is_fail(o: &CheckOutcome) -> bool {
    !o.is_pass()
}

fn detail(o: &CheckOutcome) -> String {
    o.detail().unwrap_or_default().to_string()
}

// --- plan ---------------------------------------------------------------------

fn golden_with(body: &str) -> GoldenFile {
    parse_golden(&format!("corpus = \"skim\"\ncommit = \"{SHA}\"\n{body}")).unwrap()
}

fn checks_of(body: &str) -> Vec<CheckId> {
    let g = golden_with(body);
    let plan = plan(&g).unwrap();
    assert_eq!(plan.len(), 1);
    plan[0].checks()
}

#[test]
fn ident_concept_and_lexical_entries_run_the_lexical_checks_and_score_monotone() {
    let shared = vec![
        CheckId::LexicalRecall,
        CheckId::LexicalPrecision,
        CheckId::LexicalSilentFn,
        CheckId::OrderScoreMonotone,
        CheckId::ResultsUniquePaths,
    ];
    // Each kind adds the one check its frozen answer allows.
    let with = |extra: CheckId| [shared.clone(), vec![extra]].concat();
    assert_eq!(
        checks_of(
            "[[ident]]\nid = \"skim-L01\"\nquery = \"x_y\"\ndef = { path = \"a.rs\", line = 1 }\norigin = \"seed\"\n"
        ),
        with(CheckId::IdentDefFirst)
    );
    assert_eq!(
        checks_of("[[concept]]\nid = \"skim-C01\"\nquery = \"a b\"\nrelevant = 'a'\n"),
        shared
    );
    assert_eq!(
        checks_of(
            "[[lexical]]\nid = \"skim-X01\"\nquery = \"a b\"\ncategory = \"lang\"\nlang = \"rust\"\n"
        ),
        [vec![
            CheckId::LexicalRecall,
            CheckId::LexicalPrecision,
            CheckId::LexicalSilentFn,
            CheckId::OrderScoreMonotone,
            CheckId::ResultsUniquePaths,
            CheckId::LexicalRecallTracked,
        ]]
        .concat()
    );
}

#[test]
fn a_pagination_entry_runs_the_pagination_and_lexical_checks_and_score_monotone() {
    assert_eq!(
        checks_of("[[pagination]]\nid = \"skim-G001\"\nquery = \"build lock\"\nlimits = [3]\n"),
        vec![
            CheckId::LexicalRecall,
            CheckId::LexicalPrecision,
            CheckId::LexicalSilentFn,
            CheckId::PaginationComplete,
            CheckId::PaginationDisjoint,
            CheckId::PaginationOrdered,
            CheckId::PaginationHasMoreHonest,
            CheckId::OrderScoreMonotone,
            CheckId::ResultsUniquePaths,
        ]
    );
}

#[test]
fn a_prefix_entry_checks_prefix_consistency_with_the_lexical_checks() {
    assert_eq!(
        checks_of("[[prefix]]\nid = \"skim-F001\"\nquery = \"build lock\"\nlimits = [5]\n"),
        vec![
            CheckId::LexicalRecall,
            CheckId::LexicalPrecision,
            CheckId::LexicalSilentFn,
            CheckId::OrderPrefixConsistent,
            CheckId::OrderScoreMonotone,
            CheckId::ResultsUniquePaths,
        ]
    );
}

#[test]
fn a_lexical_entrys_lang_becomes_the_type_flag_and_the_oracle_filter() {
    let g = golden_with(
        "[[lexical]]\nid = \"skim-X01\"\nquery = \"fn\"\ncategory = \"lang\"\nlang = \"typescript\"\n\
         [[lexical]]\nid = \"skim-X02\"\nquery = \"fn\"\ncategory = \"short\"\n",
    );
    let p = plan(&g).unwrap();
    assert_eq!(p[0].flags, ["-t", "ts"]);
    assert!(p[1].flags.is_empty());
    let oracle = p[0].lexical_oracle().unwrap();
    assert!(oracle.matches("a.ts", "fn"));
    assert!(!oracle.matches("a.js", "fn"));
    assert!(p[1].lexical_oracle().unwrap().matches("a.js", "fn"));
}

#[test]
fn only_ident_and_concept_entries_measure_text_output() {
    let g = golden_with(
        "[[ident]]\nid = \"skim-L01\"\nquery = \"x_y\"\ndef = { path = \"a.rs\", line = 1 }\norigin = \"seed\"\n\
         [[concept]]\nid = \"skim-C01\"\nquery = \"a b\"\nrelevant = 'a'\n\
         [[lexical]]\nid = \"skim-X01\"\nquery = \"q\"\ncategory = \"short\"\n",
    );
    let p = plan(&g).unwrap();
    let measured: Vec<bool> = p.iter().map(PlannedQuery::measures_text).collect();
    assert_eq!(measured, [true, true, false]);
}

// --- lexical checks -------------------------------------------------------------

#[test]
fn an_exact_answer_passes_recall_precision_and_silent_fn() {
    let p = page(rows(&["b.rs", "a.rs"]), false);
    let truth = gt(&["a.rs", "b.rs"]);
    assert!(check_recall(&p.rows, &truth).is_pass());
    assert!(check_precision(&p.rows, &truth).is_pass());
    assert!(check_silent_fn(&p, &truth).is_pass());
}

#[test]
fn a_missing_file_fails_recall_and_silent_fn_naming_it() {
    let p = page(rows(&["a.rs"]), false);
    let truth = gt(&["a.rs", "b.rs"]);
    let recall = check_recall(&p.rows, &truth);
    assert!(is_fail(&recall));
    assert!(detail(&recall).contains("b.rs"), "{}", detail(&recall));
    let silent = check_silent_fn(&p, &truth);
    assert!(is_fail(&silent));
    assert!(detail(&silent).contains("b.rs"), "{}", detail(&silent));
    assert!(check_precision(&p.rows, &truth).is_pass());
}

#[test]
fn an_extra_file_fails_precision_naming_it() {
    let p = page(rows(&["a.rs", "z.rs"]), false);
    let precision = check_precision(&p.rows, &gt(&["a.rs"]));
    assert!(is_fail(&precision));
    assert!(detail(&precision).contains("z.rs"));
}

#[test]
fn score_must_not_increase_down_the_list() {
    assert!(check_score_monotone(&[row("a", 3.0), row("b", 3.0), row("c", 1.0)]).is_pass());
    assert!(check_score_monotone(&[]).is_pass());
    let o = check_score_monotone(&[row("a", 3.0), row("b", 1.0), row("c", 2.0)]);
    assert!(is_fail(&o));
    assert!(detail(&o).contains("rank 3"), "{}", detail(&o));
}

// --- unique paths -------------------------------------------------------------------

#[test]
fn lists_without_a_repeated_path_pass_unique_paths() {
    let full = rows(&["a", "b", "c"]);
    let limited = [(2, page(rows(&["a", "b"]), true))];
    assert!(check_unique_paths(&full, &[honest_sweep(&["a", "b", "c"], 2)], &limited).is_pass());
    assert!(check_unique_paths(&[], &[], &[]).is_pass());
}

#[test]
fn a_path_repeated_in_the_full_list_fails_unique_paths_naming_it() {
    // A tie: the list never rises, so score monotonicity lets it through.
    let full = vec![row("a", 2.0), row("a", 2.0), row("b", 1.0)];
    assert!(check_score_monotone(&full).is_pass());
    let o = check_unique_paths(&full, &[], &[]);
    assert!(is_fail(&o));
    assert_eq!(detail(&o), "full list: 1 file(s) shown more than once: a");
}

#[test]
fn a_path_repeated_on_a_limited_list_or_one_page_fails_unique_paths_naming_the_list() {
    let full = rows(&["a", "b", "c", "d"]);
    let limited = [
        (2, page(rows(&["a", "b"]), true)),
        (3, page(rows(&["a", "b", "b"]), true)),
    ];
    let mut sweep = honest_sweep(&["a", "b", "c", "d"], 2);
    sweep.pages[1].page.rows[1] = row("c", 1.0);
    let o = check_unique_paths(&full, &[sweep], &limited);
    let d = detail(&o);
    assert!(is_fail(&o));
    assert!(
        d.contains("limit 3: 1 file(s) shown more than once: b"),
        "{d}"
    );
    assert!(
        d.contains("L=2 offset 2: 1 file(s) shown more than once: c"),
        "{d}"
    );
    assert!(!d.contains("limit 2") && !d.contains("full list"), "{d}");
}

#[test]
fn a_path_on_two_different_pages_is_left_to_pagination_disjoint() {
    let full = rows(&["a", "b", "c", "d"]);
    let mut sweep = honest_sweep(&["a", "b", "c", "d"], 2);
    sweep.pages[1].page.rows[0] = row("b", 2.0);
    assert!(check_unique_paths(&full, std::slice::from_ref(&sweep), &[]).is_pass());
    assert!(is_fail(&check_pagination(&full, &[sweep]).disjoint));
}

// --- pagination -------------------------------------------------------------------

#[test]
fn honest_complete_pages_pass_every_pagination_check() {
    let full = rows(&["a", "b", "c", "d", "e"]);
    for limit in [1, 2, 3, 5, 7] {
        let o = check_pagination(&full, &[honest_sweep(&["a", "b", "c", "d", "e"], limit)]);
        assert!(o.complete.is_pass(), "L={limit}: {:?}", o.complete);
        assert!(o.disjoint.is_pass(), "L={limit}");
        assert!(o.ordered.is_pass(), "L={limit}");
        assert!(
            o.has_more_honest.is_pass(),
            "L={limit}: {:?}",
            o.has_more_honest
        );
    }
}

#[test]
fn an_empty_list_paginates_honestly() {
    let o = check_pagination(&[], &[honest_sweep(&[], 3)]);
    assert!(o.complete.is_pass() && o.has_more_honest.is_pass());
}

#[test]
fn a_skipped_file_fails_complete_and_ordered() {
    let full = rows(&["a", "b", "c", "d"]);
    let mut sweep = honest_sweep(&["a", "b", "c", "d"], 2);
    sweep.pages[1].page.rows.remove(0); // "c" never shown
    let o = check_pagination(&full, &[sweep]);
    assert!(is_fail(&o.complete));
    assert!(detail(&o.complete).contains("L=2") && detail(&o.complete).contains('c'));
    assert!(is_fail(&o.ordered));
    assert!(o.disjoint.is_pass());
}

#[test]
fn a_file_on_two_pages_fails_disjoint() {
    let full = rows(&["a", "b", "c", "d"]);
    let mut sweep = honest_sweep(&["a", "b", "c", "d"], 2);
    sweep.pages[1].page.rows[0] = row("b", 2.0);
    let o = check_pagination(&full, &[sweep]);
    assert!(is_fail(&o.disjoint));
    assert!(detail(&o.disjoint).contains('b'));
}

#[test]
fn an_empty_page_claiming_more_is_dishonest() {
    let full = rows(&["a", "b"]);
    let mut sweep = honest_sweep(&["a", "b"], 2);
    sweep.pages[0].page.has_more = true;
    sweep.pages.push(SweepPage {
        offset: 2,
        page: page(Vec::new(), false),
    });
    let o = check_pagination(&full, &[sweep]);
    assert!(is_fail(&o.has_more_honest));
    let d = detail(&o.has_more_honest);
    assert!(d.contains("offset 0"), "{d}");
    assert!(o.complete.is_pass() && o.ordered.is_pass());
}

#[test]
fn empty_pages_that_never_stop_are_dishonest_and_unterminated() {
    let full = rows(&["a"]);
    let mut pages = vec![SweepPage {
        offset: 0,
        page: page(rows(&["a"]), true),
    }];
    for i in 1..MAX_PAGES {
        pages.push(SweepPage {
            offset: u64::from(i),
            page: page(Vec::new(), true),
        });
    }
    let o = check_pagination(&full, &[Sweep { limit: 1, pages }]);
    let d = detail(&o.has_more_honest);
    assert!(d.contains("63 empty page(s)"), "{d}");
    assert!(d.contains(&format!("within {MAX_PAGES} pages")), "{d}");
}

#[test]
fn has_more_false_while_rows_remain_is_dishonest() {
    let full = rows(&["a", "b", "c"]);
    let sweep = Sweep {
        limit: 2,
        pages: vec![SweepPage {
            offset: 0,
            page: page(rows(&["a", "b"]), false),
        }],
    };
    let o = check_pagination(&full, &[sweep]);
    assert!(is_fail(&o.has_more_honest));
    assert!(
        detail(&o.has_more_honest).contains("3 rows"),
        "{}",
        detail(&o.has_more_honest)
    );
    assert!(is_fail(&o.complete));
}

#[test]
fn every_limit_is_reported() {
    let full = rows(&["a", "b", "c"]);
    let mut bad = honest_sweep(&["a", "b", "c"], 1);
    bad.pages.remove(1);
    let o = check_pagination(&full, &[honest_sweep(&["a", "b", "c"], 2), bad]);
    let d = detail(&o.complete);
    assert!(d.contains("L=1") && !d.contains("L=2"), "{d}");
}

// --- prefix -------------------------------------------------------------------------

#[test]
fn a_limited_list_must_be_the_full_lists_prefix() {
    let full = rows(&["a", "b", "c", "d"]);
    assert!(check_prefix(&full, &[(2, page(rows(&["a", "b"]), true))]).is_pass());
    assert!(check_prefix(&full, &[(9, page(rows(&["a", "b", "c", "d"]), false))]).is_pass());

    let o = check_prefix(
        &full,
        &[
            (2, page(rows(&["a", "b"]), true)),
            (3, page(rows(&["a", "c", "x"]), true)),
        ],
    );
    let d = detail(&o);
    assert!(d.contains("limit 3") && !d.contains("limit 2"), "{d}");
    assert!(d.contains("2/3"), "{d}");
}

// --- measurements ---------------------------------------------------------------------

#[test]
fn bytes_through_the_def_line_end_at_the_line_that_starts_def_path() {
    let out = b"calls:\n  caller.rs:4: x\ndef a.rs:3-9: fn a() {\n    body\n}\ndef b.rs:1-2: fn b() {}\ntotal: 2\n";
    let through_a = "calls:\n  caller.rs:4: x\ndef a.rs:3-9: fn a() {\n".len() as u64;
    assert_eq!(bytes_through_def_line(out, "a.rs"), Some(through_a));
    assert_eq!(
        bytes_through_def_line(out, "b.rs"),
        Some(through_a + "    body\n}\ndef b.rs:1-2: fn b() {}\n".len() as u64)
    );
    assert_eq!(bytes_through_def_line(out, "c.rs"), None);
    // A path that merely starts another path is not that file's definition.
    assert_eq!(
        bytes_through_def_line(b"def a.rs.bak:1-2: x\n", "a.rs"),
        None
    );
    // A line that mentions the definition without starting with it is not one.
    assert_eq!(bytes_through_def_line(b"  def a.rs:1-2: x\n", "a.rs"), None);
}

#[test]
fn bytes_through_a_file_block_end_at_the_first_line_of_another_file() {
    let out = b"words: a b\na.rs  a=2 b=1\na.rs:3: [f] x\na.rs:9: y\nb.rs  a=1 b=1\nb.rs:1: z\ntotal: 2\n";
    let header = "words: a b\n".len() as u64;
    let a_block = "a.rs  a=2 b=1\na.rs:3: [f] x\na.rs:9: y\n".len() as u64;
    assert_eq!(
        bytes_through_file_block(out, "a.rs"),
        Some(header + a_block)
    );
    assert_eq!(
        bytes_through_file_block(out, "b.rs"),
        Some(header + a_block + "b.rs  a=1 b=1\nb.rs:1: z\n".len() as u64)
    );
    assert_eq!(bytes_through_file_block(out, "c.rs"), None);
    // `a.rs` does not claim the lines of a longer path that starts with it.
    assert_eq!(
        bytes_through_file_block(b"a.rs.bak  a=1\na.rs.bak:1: x\n", "a.rs"),
        None
    );
    assert_eq!(
        bytes_through_file_block(b"a.rs  a=1\na.rs.bak  a=1\n", "a.rs"),
        Some("a.rs  a=1\n".len() as u64),
        "the block of a.rs ends where the block of a.rs.bak starts"
    );
    // A block with no matching line still counts: its header is the block.
    assert_eq!(bytes_through_file_block(b"a.rs  a=1\n", "a.rs"), Some(10));
}

#[test]
fn precision_at_k_divides_by_the_rows_shown() {
    let rel = |p: &str| p.starts_with('r');
    assert_eq!(precision_at_k(&["r1", "x", "r2"], 5, rel), 2.0 / 3.0);
    assert_eq!(
        precision_at_k(&["r1", "x", "r2", "x", "x", "r3"], 5, rel),
        0.4
    );
    assert_eq!(precision_at_k(&[], 5, rel), 0.0);
}

#[test]
fn percentiles_use_the_nearest_rank() {
    assert_eq!(percentile(&[], 0.5), None);
    assert_eq!(percentile(&[7], 0.9), Some(7.0));
    assert_eq!(percentile(&[4, 1, 3, 2], 0.5), Some(2.0));
    assert_eq!(percentile(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 0.9), Some(9.0));
}

// --- ratchet ----------------------------------------------------------------------------

#[test]
fn ratchet_changes_follow_direction_and_tolerance() {
    use RatchetChange::*;
    assert_eq!(compare_ratchet("ident.mrr", 0.5, 0.5), Unchanged);
    assert_eq!(compare_ratchet("ident.mrr", 0.5, 1.0), Improved);
    assert_eq!(compare_ratchet("ident.mrr", 0.5, 0.4999), Regressed);
    assert_eq!(
        compare_ratchet("bytes.text_median", 1000.0, 1030.0),
        Unchanged
    );
    assert_eq!(
        compare_ratchet("bytes.text_median", 1000.0, 1031.0),
        Regressed
    );
    assert_eq!(
        compare_ratchet("bytes.text_median", 1000.0, 960.0),
        Improved
    );
    assert_eq!(compare_ratchet("universe.delta", 2.0, -1.0), Improved);
    assert_eq!(compare_ratchet("universe.delta", 0.0, 1.0), Regressed);
    assert_eq!(compare_ratchet("universe.delta", 1.0, -1.0), Changed);
    assert_eq!(
        compare_ratchet("concept.p10.baseline_count", 0.7, 0.8),
        Changed
    );
    assert_eq!(compare_ratchet("not.a.metric", 1.0, 2.0), Changed);
    assert_eq!(compare_ratchet("not.a.metric", 1.0, 1.0), Unchanged);
}

#[test]
fn every_ratchet_metric_is_defined_once() {
    let mut names: Vec<&str> = RATCHET_METRICS.iter().map(|d| d.name).collect();
    let n = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), n);
    assert!(metric_def("ident.mrr").is_some());
}

fn ident(id: &str, rank: Option<u64>, first_correct: Option<u64>) -> IdentSample {
    IdentSample {
        id: id.to_string(),
        rank,
        rank_baseline_alpha: Some(2),
        rank_baseline_count: Some(1),
        anchor_eq_def: rank == Some(1),
        def_line_in_snippet: true,
        text_bytes: 100,
        first_correct_bytes: first_correct,
        rg_text_bytes: 300,
        rg_first_correct_bytes: Some(50),
        rgw_text_bytes: 200,
        rgw_first_correct_bytes: Some(30),
    }
}

fn samples(idents: Vec<IdentSample>) -> CorpusSamples {
    CorpusSamples {
        universe_delta: 0,
        skipped_mismatch: 0,
        indexed_tracked: 9,
        tracked_text: 10,
        idents,
        concepts: Vec::new(),
        refs: Vec::new(),
        roles: BTreeMap::new(),
        costs: Vec::new(),
    }
}

#[test]
fn ident_ratchets_pool_ranks_bytes_and_misses() {
    let s = samples(vec![
        ident("a-1", Some(1), Some(40)),
        ident("a-2", Some(2), Some(80)),
        ident("a-3", None, None),
    ]);
    let r = ratchet_values(&[&s]);
    assert_eq!(r["ident.def_top1"], 0.3333);
    assert_eq!(r["ident.mrr"], 0.5);
    assert_eq!(r["ident.anchor_eq_def"], 0.3333);
    assert_eq!(r["bytes.first_correct_median"], 40.0);
    assert_eq!(r["bytes.first_correct_misses"], 1.0);
    assert_eq!(r["bytes.rg_first_correct_median"], 50.0);
    assert_eq!(r["bytes.text_median"], 100.0);
    assert_eq!(r["coverage.tracked_text"], 0.9);
    assert_eq!(r["universe.delta"], 0.0);
    assert_eq!(r["ident.def_top1.baseline_alpha"], 0.0);
    assert_eq!(r["ident.mrr.baseline_alpha"], 0.5);
    assert_eq!(r["ident.def_top1.baseline_count"], 1.0);
    assert_eq!(r["ident.mrr.baseline_count"], 1.0);
    assert!(
        !r.contains_key("concept.p10"),
        "no concept entries, no concept metrics"
    );
    for name in r.keys() {
        assert!(metric_def(name).is_some(), "{name} has no MetricDef");
    }
}

#[test]
fn beating_a_baseline_follows_the_metric_direction() {
    let r = BTreeMap::from([
        ("ident.mrr".to_string(), 0.9),
        ("ident.mrr.baseline_alpha".to_string(), 0.5),
        ("concept.p10".to_string(), 0.7),
        ("concept.p10.baseline_count".to_string(), 0.7),
        ("bytes.text_median".to_string(), 900.0),
        ("bytes.rg_text_median".to_string(), 800.0),
    ]);
    assert_eq!(beats_baseline("ident.mrr", &r), Some(Beat::Yes));
    assert_eq!(beats_baseline("concept.p10", &r), Some(Beat::Tie));
    assert_eq!(beats_baseline("bytes.text_median", &r), Some(Beat::No));
    assert_eq!(beats_baseline("ident.def_top1", &r), None, "not measured");
    assert_eq!(
        beats_baseline("universe.delta", &r),
        None,
        "no baseline to beat"
    );
}

#[test]
fn aggregate_ratchets_pool_every_corpus() {
    let a = samples(vec![ident("a-1", Some(1), Some(10))]);
    let mut b = samples(vec![ident("b-1", Some(4), Some(30))]);
    b.universe_delta = -2;
    let r = ratchet_values(&[&a, &b]);
    assert_eq!(r["ident.mrr"], 0.625);
    assert_eq!(r["universe.delta"], -2.0);
    assert_eq!(r["coverage.tracked_text"], 0.9);
}

// --- evaluate (fixture corpus) -----------------------------------------------------

/// `page` with the engine's file accounting: `searched` files, none left out.
fn accounted(page: ResultPage, searched: u64) -> ResultPage {
    ResultPage {
        searched_files: Some(searched),
        left_out: Some(BTreeMap::new()),
        ..page
    }
}

#[test]
fn evaluate_scores_a_fixture_corpus_end_to_end() {
    let repo = FixtureRepo::new();
    repo.write("src/a.rs", "// check_staleness lives in b\n");
    repo.write("src/b.rs", "fn x() {}\npub fn check_staleness() {}\n");
    repo.write("src/c.rs", "// build lock\nfn y() {}\n");
    let commit = repo.commit_all("init");
    let text = text_universe(repo.root(), &GitIsolation::new(repo.home())).unwrap();
    let golden = parse_golden(&format!(
        "corpus = \"skim\"\ncommit = \"{commit}\"\n\
         [[ident]]\nid = \"skim-L01\"\nquery = \"check_staleness\"\ndef = {{ path = \"src/b.rs\", line = 2 }}\norigin = \"seed\"\n\
         [[concept]]\nid = \"skim-C01\"\nquery = \"lock\"\nrelevant = '(?i)build[_\\s-]*lock'\n"
    ))
    .unwrap();
    let plan = plan(&golden).unwrap();

    let mut ident_rows = vec![row("src/b.rs", 2.0), row("src/a.rs", 1.0)];
    ident_rows[0].line = Some(2);
    ident_rows[0].snippet = vec![SnippetLine {
        line_number: 2,
        content: "pub fn check_staleness() {}".to_string(),
        is_match: true,
    }];
    let def_line = "def src/b.rs:2-2: pub fn check_staleness() {}\n";
    let ident_text = format!(
        "{def_line}other:\n  src/a.rs:1: // check_staleness lives in b\ntotal: 2 lines in 2 files\n"
    );
    let concept_text = "src/c.rs  lock=1\nsrc/c.rs:1: // build lock\nsrc/a.rs  lock=1\n";
    let observations = vec![
        EntryObservation {
            id: "skim-L01".to_string(),
            full: accounted(page(ident_rows, false), 3),
            sweeps: Vec::new(),
            limited: Vec::new(),
            text: Some(TextOutput {
                stdout: ident_text.into_bytes(),
                stderr: Vec::new(),
            }),
        },
        EntryObservation {
            id: "skim-C01".to_string(),
            // The ground truth for "lock" is src/c.rs only; skim adds a.rs.
            full: page(rows(&["src/c.rs", "src/a.rs"]), false),
            sweeps: Vec::new(),
            limited: Vec::new(),
            text: Some(TextOutput {
                stdout: concept_text.as_bytes().to_vec(),
                stderr: b"note\n".to_vec(),
            }),
        },
    ];

    let e = evaluate(&text, &plan, &observations).unwrap();

    let outcome = |id: &str, check: CheckId| {
        e.outcomes
            .iter()
            .find(|(i, c, _)| i == id && *c == check)
            .map(|(_, _, o)| o.clone())
            .unwrap()
    };
    assert!(outcome("skim-L01", CheckId::LexicalRecall).is_pass());
    assert!(outcome("skim-L01", CheckId::LexicalPrecision).is_pass());
    assert!(outcome("skim-L01", CheckId::ResultsUniquePaths).is_pass());
    assert!(is_fail(&outcome("skim-C01", CheckId::LexicalPrecision)));
    let sorted = {
        let mut s = e.outcomes.clone();
        s.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        s
    };
    assert_eq!(e.outcomes, sorted, "outcomes are sorted by (id, check)");

    assert_eq!(e.searched_files, 3);
    assert_eq!(e.samples.universe_delta, 0);
    assert_eq!(e.samples.skipped_mismatch, 0);
    assert_eq!((e.samples.indexed_tracked, e.samples.tracked_text), (3, 3));
    let l = &e.samples.idents[0];
    assert_eq!(
        (l.rank, l.anchor_eq_def, l.def_line_in_snippet),
        (Some(1), true, true)
    );
    // Ground truth {a.rs, b.rs}: path order puts b.rs second, and so does
    // the occurrence count (one each, ties by path).
    assert_eq!(
        (l.rank_baseline_alpha, l.rank_baseline_count),
        (Some(2), Some(2))
    );
    assert_eq!(l.first_correct_bytes, Some(def_line.len() as u64));
    let rg = "src/a.rs:1:// check_staleness lives in b\nsrc/b.rs:2:pub fn check_staleness() {}\n";
    assert_eq!(l.rg_first_correct_bytes, Some(rg.len() as u64));
    let c = &e.samples.concepts[0];
    assert_eq!((c.p5, c.p10), (0.5, 0.5));
    assert_eq!((c.p5_baseline_alpha, c.p5_baseline_count), (1.0, 1.0));
    assert_eq!(c.text_bytes, concept_text.len() as u64 + 5);
    assert_eq!(
        c.first_relevant_bytes,
        Some("src/c.rs  lock=1\nsrc/c.rs:1: // build lock\n".len() as u64),
        "the block of the first relevant ranked file, not of the first file"
    );
}

#[test]
fn evaluate_counts_the_files_and_left_out_reasons_that_differ_from_the_oracle() {
    let repo = FixtureRepo::new();
    repo.write("a.rs", "x\n");
    repo.write("b.rs", "x\n");
    let commit = repo.commit_all("init");
    let text = text_universe(repo.root(), &GitIsolation::new(repo.home())).unwrap();
    let golden = parse_golden(&format!(
        "corpus = \"skim\"\ncommit = \"{commit}\"\n[[lexical]]\nid = \"skim-X01\"\nquery = \"x\"\ncategory = \"short\"\n"
    ))
    .unwrap();
    let plan = plan(&golden).unwrap();
    // The engine searched 1 of the 2 files and left out one binary file the
    // oracle does not know of.
    let full = ResultPage {
        searched_files: Some(1),
        left_out: Some(BTreeMap::from([("binary".to_string(), 1)])),
        ..page(rows(&["a.rs", "b.rs"]), false)
    };
    let observations = vec![EntryObservation {
        id: "skim-X01".to_string(),
        full,
        sweeps: Vec::new(),
        limited: Vec::new(),
        text: None,
    }];

    let e = evaluate(&text, &plan, &observations).unwrap();

    assert_eq!(e.searched_files, 1);
    assert_eq!(e.left_out, BTreeMap::from([("binary".to_string(), 1)]));
    assert_eq!(e.samples.universe_delta, 1, "the oracle has one file more");
    assert_eq!(e.samples.skipped_mismatch, 1, "binary: skim 1, oracle 0");
    assert_eq!(
        (e.samples.indexed_tracked, e.samples.tracked_text),
        (1, 2),
        "coverage is the searched files over the oracle's"
    );
}

#[test]
fn evaluate_needs_an_observation_that_states_the_file_accounting() {
    let repo = FixtureRepo::new();
    repo.write("a.rs", "x\n");
    let commit = repo.commit_all("init");
    let text = text_universe(repo.root(), &GitIsolation::new(repo.home())).unwrap();
    let golden = parse_golden(&format!(
        "corpus = \"skim\"\ncommit = \"{commit}\"\n[[lexical]]\nid = \"skim-X01\"\nquery = \"x\"\ncategory = \"short\"\n"
    ))
    .unwrap();
    let plan = plan(&golden).unwrap();
    let observations = vec![EntryObservation {
        id: "skim-X01".to_string(),
        full: page(rows(&["a.rs"]), false),
        sweeps: Vec::new(),
        limited: Vec::new(),
        text: None,
    }];
    let err = evaluate(&text, &plan, &observations).expect_err("no accounting is no pass");
    assert!(format!("{err:#}").contains("searched_files"), "{err:#}");
}

#[test]
fn evaluate_rejects_observations_that_do_not_follow_the_plan() {
    let repo = FixtureRepo::new();
    repo.write("a.rs", "x\n");
    let commit = repo.commit_all("init");
    let text = text_universe(repo.root(), &GitIsolation::new(repo.home())).unwrap();
    let golden = parse_golden(&format!(
        "corpus = \"skim\"\ncommit = \"{commit}\"\n[[lexical]]\nid = \"skim-X01\"\nquery = \"x\"\ncategory = \"short\"\n"
    ))
    .unwrap();
    let plan = plan(&golden).unwrap();
    assert!(evaluate(&text, &plan, &[]).is_err());
}

// --- #540: the frozen-answer checks (pure) ----------------------------------------

use crate::scoreboard::types::{Cost, DefinitionSite, Definitions, Occurrence, Role};

fn occ(path: &str, line: u32, role: Option<Role>) -> Occurrence {
    Occurrence {
        path: path.to_string(),
        line,
        role,
    }
}

fn with_occurrences(occurrences: Vec<Occurrence>) -> ResultPage {
    ResultPage {
        occurrences,
        ..ResultPage::default()
    }
}

fn with_definitions(count: Option<u64>, at_least: bool, sites: &[(&str, u32)]) -> ResultPage {
    ResultPage {
        definitions: Some(Definitions {
            count,
            at_least,
            sites: sites
                .iter()
                .map(|(p, l)| DefinitionSite {
                    path: p.to_string(),
                    line: *l,
                })
                .collect(),
        }),
        ..ResultPage::default()
    }
}

/// `launch`: defined at lib.rs:1, called at lib.rs:2 and main.rs:1, named in
/// a comment at lib.rs:3 and in LICENSE:1.
fn refs_entry() -> RefsEntry {
    golden_with(
        "[[refs]]\nid = \"skim-R01\"\nquery = \"launch\"\n\
         def = { path = \"src/lib.rs\", line = 1 }\norigin = \"curated\"\n\
         sites = [\n\
           { path = \"src/lib.rs\", line = 1, role = \"def\" },\n\
           { path = \"src/lib.rs\", line = 2, role = \"call\" },\n\
           { path = \"src/lib.rs\", line = 3, role = \"comment\" },\n\
           { path = \"src/main.rs\", line = 1, role = \"call\" },\n\
           { path = \"LICENSE\", line = 1, role = \"text\" },\n\
         ]\n",
    )
    .refs
    .remove(0)
}

/// Every frozen site of [`refs_entry`], labelled as the entry freezes it.
fn refs_occurrences() -> Vec<Occurrence> {
    vec![
        occ("src/lib.rs", 1, Some(Role::Def)),
        occ("src/lib.rs", 2, Some(Role::Call)),
        occ("src/lib.rs", 3, Some(Role::Comment)),
        occ("src/main.rs", 1, Some(Role::Call)),
        occ("LICENSE", 1, Some(Role::Text)),
    ]
}

#[test]
fn refs_recall_fails_when_one_frozen_site_is_absent_and_passes_when_all_are_present() {
    let entry = refs_entry();
    assert!(check_refs_recall(&with_occurrences(refs_occurrences()), &entry).is_pass());

    // The role and extra occurrences do not matter to recall.
    let mut extra = refs_occurrences();
    extra[1].role = None;
    extra.push(occ("src/other.rs", 9, Some(Role::Ref)));
    assert!(check_refs_recall(&with_occurrences(extra), &entry).is_pass());

    let mut absent = refs_occurrences();
    absent.remove(3);
    let d = detail(&check_refs_recall(&with_occurrences(absent), &entry));
    assert!(
        d.contains("missing 1 of 5") && d.contains("src/main.rs:1"),
        "{d}"
    );

    // A page with no occurrence at all (the output states none) fails.
    assert!(is_fail(&check_refs_recall(&ResultPage::default(), &entry)));
}

#[test]
fn refs_calls_recall_fails_when_a_frozen_call_has_any_role_other_than_call_or_is_absent() {
    let entry = refs_entry();
    assert!(check_refs_calls_recall(&with_occurrences(refs_occurrences()), &entry).is_pass());

    // Only the frozen calls need the call role; the comment may be anything.
    let mut loose = refs_occurrences();
    loose[2].role = None;
    assert!(check_refs_calls_recall(&with_occurrences(loose), &entry).is_pass());

    for wrong in [Some(Role::Ref), Some(Role::Def), Some(Role::Partial), None] {
        let mut o = refs_occurrences();
        o[1].role = wrong;
        let d = detail(&check_refs_calls_recall(&with_occurrences(o), &entry));
        assert!(
            d.contains("1 of 2") && d.contains("src/lib.rs:2"),
            "{wrong:?}: {d}"
        );
    }

    let mut absent = refs_occurrences();
    absent.remove(3);
    let d = detail(&check_refs_calls_recall(&with_occurrences(absent), &entry));
    assert!(d.contains("src/main.rs:1 (absent)"), "{d}");
}

#[test]
fn refs_call_precision_is_the_frozen_calls_in_the_call_group_over_its_size() {
    let entry = refs_entry();
    // Call group {lib.rs:2, main.rs:1, lib.rs:3}: two of three are frozen calls.
    let mut o = refs_occurrences();
    o[2].role = Some(Role::Call);
    let p = refs_call_precision(&with_occurrences(o), &entry).unwrap();
    assert!((p - 2.0 / 3.0).abs() < 1e-9, "{p}");

    assert_eq!(
        refs_call_precision(&with_occurrences(refs_occurrences()), &entry),
        Some(1.0)
    );
    // A page that labels no call has no precision to record.
    let unlabelled = vec![occ("src/lib.rs", 2, None), occ("src/main.rs", 1, None)];
    assert_eq!(
        refs_call_precision(&with_occurrences(unlabelled), &entry),
        None
    );
    assert_eq!(refs_call_precision(&ResultPage::default(), &entry), None);
    // A call group with no frozen call scores 0.
    assert_eq!(
        refs_call_precision(
            &with_occurrences(vec![occ("src/other.rs", 4, Some(Role::Call))]),
            &entry
        ),
        Some(0.0)
    );
}

/// `run` is defined at a.rs:1 and b.rs:1; its calls are a.rs:2 and c.rs:7.
fn clash_entry() -> ClashEntry {
    golden_with(
        "[[clash]]\nid = \"skim-D01\"\nquery = \"run\"\nsplit = \"none\"\norigin = \"curated\"\n\
         defs = [ { path = \"src/a.rs\", line = 1 }, { path = \"src/b.rs\", line = 1 } ]\n\
         calls = [\n\
           { path = \"src/a.rs\", line = 2, def = 0 },\n\
           { path = \"src/c.rs\", line = 7, def = \"unassigned\" },\n\
         ]\n",
    )
    .clashes
    .remove(0)
}

#[test]
fn clash_defs_complete_fails_when_one_frozen_definition_is_absent() {
    let entry = clash_entry();
    let both = with_definitions(Some(2), false, &[("src/a.rs", 1), ("src/b.rs", 1)]);
    assert!(check_clash_defs_complete(&both, &entry).is_pass());
    // The order of the sites does not matter here.
    let reversed = with_definitions(Some(2), false, &[("src/b.rs", 1), ("src/a.rs", 1)]);
    assert!(check_clash_defs_complete(&reversed, &entry).is_pass());

    let one = with_definitions(Some(2), false, &[("src/a.rs", 1)]);
    let d = detail(&check_clash_defs_complete(&one, &entry));
    assert!(
        d.contains("missing 1 of 2") && d.contains("src/b.rs:1"),
        "{d}"
    );
    // The right file at another line is not the definition.
    let drifted = with_definitions(Some(2), false, &[("src/a.rs", 1), ("src/b.rs", 4)]);
    assert!(is_fail(&check_clash_defs_complete(&drifted, &entry)));
    // No definitions in the output fails closed.
    assert!(is_fail(&check_clash_defs_complete(
        &ResultPage::default(),
        &entry
    )));
}

#[test]
fn clash_def_count_accepts_an_exact_count_and_the_at_least_form_only_inside_its_bounds() {
    let entry = clash_entry(); // frozen: 2
    let count = |count: Option<u64>, at_least: bool, unlabelled: u64| {
        let mut page = with_definitions(count, at_least, &[]);
        page.unlabelled_files = unlabelled;
        check_clash_def_count(&page, &entry)
    };
    assert!(count(Some(2), false, 0).is_pass());
    assert!(is_fail(&count(Some(1), false, 0)));
    assert!(is_fail(&count(Some(3), false, 0)));
    // Without the budget note, unlabelled files change nothing.
    assert!(is_fail(&count(Some(1), false, 5)));

    // `at_least`: count <= frozen <= count + unlabelled_files.
    assert!(count(Some(1), true, 1).is_pass());
    assert!(count(Some(1), true, 37).is_pass());
    assert!(count(Some(2), true, 0).is_pass(), "an exact count passes");
    assert!(is_fail(&count(Some(1), true, 0)), "frozen is out of reach");
    assert!(
        is_fail(&count(Some(3), true, 9)),
        "the count is above frozen"
    );

    // The output states no count, or no definitions at all.
    let d = detail(&count(None, false, 0));
    assert!(d.contains("no definitions.count"), "{d}");
    assert!(is_fail(&check_clash_def_count(
        &ResultPage::default(),
        &entry
    )));
}

#[test]
fn clash_calls_recall_fails_when_a_call_of_either_definition_is_not_labelled_call() {
    let entry = clash_entry();
    let all = with_occurrences(vec![
        occ("src/a.rs", 2, Some(Role::Call)),
        occ("src/c.rs", 7, Some(Role::Call)),
    ]);
    assert!(check_clash_calls_recall(&all, &entry).is_pass());

    let missing_second = with_occurrences(vec![occ("src/a.rs", 2, Some(Role::Call))]);
    let d = detail(&check_clash_calls_recall(&missing_second, &entry));
    assert!(
        d.contains("src/c.rs:7 (absent)") && d.contains("either definition"),
        "{d}"
    );

    let wrong_role = with_occurrences(vec![
        occ("src/a.rs", 2, Some(Role::Ref)),
        occ("src/c.rs", 7, Some(Role::Call)),
    ]);
    assert!(is_fail(&check_clash_calls_recall(&wrong_role, &entry)));
}

#[test]
fn ident_def_first_needs_the_first_definition_site_to_be_the_frozen_line() {
    let def = DefSite {
        path: "src/b.rs".to_string(),
        line: 2,
    };
    let first =
        |sites: &[(&str, u32)]| check_def_first(&with_definitions(Some(1), false, sites), &def);
    assert!(first(&[("src/b.rs", 2)]).is_pass());
    // Only the first site counts.
    assert!(first(&[("src/b.rs", 2), ("src/z.rs", 9)]).is_pass());
    // The right file, another line.
    let d = detail(&first(&[("src/b.rs", 5)]));
    assert!(
        d.contains("src/b.rs:5") && d.contains("expected src/b.rs:2"),
        "{d}"
    );
    // The frozen line is present but not first.
    assert!(is_fail(&first(&[("src/z.rs", 9), ("src/b.rs", 2)])));
    assert!(is_fail(&first(&[])), "no site");
    assert!(is_fail(&check_def_first(&ResultPage::default(), &def)));
}

#[test]
fn concept_first_right_top5_passes_only_when_a_frozen_first_file_is_in_the_first_five() {
    let first = vec!["src/guard.rs".to_string(), "src/fidelity.rs".to_string()];
    let list = |paths: &[&str]| check_first_right_top5(&rows(paths), &first);
    assert!(list(&["a", "b", "c", "d", "src/fidelity.rs"]).is_pass());
    assert!(list(&["src/guard.rs"]).is_pass());
    let d = detail(&list(&["a", "b", "c", "d", "e", "src/guard.rs"]));
    assert!(d.contains("first 5"), "{d}");
    assert!(is_fail(&list(&[])), "an empty list has no first file");
}

#[test]
fn recall_tracked_reads_the_text_universe_ground_truth() {
    let gt = gt(&["LICENSE", "src/lib.rs"]);
    assert!(check_recall_tracked(&rows(&["src/lib.rs", "LICENSE"]), &gt).is_pass());
    let d = detail(&check_recall_tracked(&rows(&["src/lib.rs"]), &gt));
    assert!(
        d.contains("missing 1 of 2 text-universe") && d.contains("LICENSE"),
        "{d}"
    );
}

/// A repository whose text universe is two text files, one binary file and
/// one file over the size cap.
fn accounted_corpus() -> (FixtureRepo, TextUniverse) {
    let repo = FixtureRepo::new();
    repo.write("a.rs", "fn a() {}\n");
    repo.write("LICENSE", "MIT\n");
    repo.write("logo.png", b"\x89PNG\0\0".as_slice());
    repo.write(
        "huge.txt",
        vec![b'x'; usize::try_from(crate::scoreboard::universe::MAX_FILE_BYTES).unwrap() + 1],
    );
    repo.commit_all("init");
    let text = text_universe(repo.root(), &GitIsolation::new(repo.home())).unwrap();
    (repo, text)
}

fn accounted_page(searched: Option<u64>, left_out: &[(&str, u64)]) -> ResultPage {
    ResultPage {
        searched_files: searched,
        left_out: Some(left_out.iter().map(|(r, n)| (r.to_string(), *n)).collect()),
        ..ResultPage::default()
    }
}

#[test]
fn universe_accounted_needs_the_searched_files_and_every_left_out_count_to_match() {
    let (_repo, text) = accounted_corpus();
    assert_eq!((text.len(), text.left_out_count("binary")), (2, 1));
    let exact = [("binary", 1), ("too_large", 1)];
    assert!(check_universe_accounted(&accounted_page(Some(2), &exact), &text).is_pass());
    // A reason with a zero count may be omitted or listed.
    let zeros = [
        ("binary", 1),
        ("too_large", 1),
        ("symlink", 0),
        ("vanished", 0),
    ];
    assert!(check_universe_accounted(&accounted_page(Some(2), &zeros), &text).is_pass());

    // TP-12: the searched files differ, the left-out counts agree.
    let d = detail(&check_universe_accounted(
        &accounted_page(Some(1), &exact),
        &text,
    ));
    assert!(
        d.contains("searched_files is 1") && d.contains("has 2"),
        "{d}"
    );
    assert!(d.contains("accounted 3 files, the oracle 4"), "{d}");

    // TP-13: one left-out count differs, the totals still agree.
    let moved = [("binary", 2), ("too_large", 0)];
    let d = detail(&check_universe_accounted(
        &accounted_page(Some(2), &moved),
        &text,
    ));
    assert!(
        d.contains("left_out.binary is 2") && d.contains("left_out.too_large is 0"),
        "{d}"
    );
    assert!(!d.contains("searched_files"), "{d}");

    // A reason the oracle does not know, with a count, is a difference.
    let unknown = [("binary", 1), ("too_large", 1), ("minified", 3)];
    let d = detail(&check_universe_accounted(
        &accounted_page(Some(2), &unknown),
        &text,
    ));
    assert!(d.contains("left_out.minified is 3"), "{d}");
}

#[test]
fn universe_accounted_fails_closed_when_the_output_states_no_accounting() {
    let (_repo, text) = accounted_corpus();
    let no_searched = accounted_page(None, &[("binary", 1), ("too_large", 1)]);
    assert!(detail(&check_universe_accounted(&no_searched, &text)).contains("no searched_files"));
    let no_left_out = ResultPage {
        searched_files: Some(2),
        ..ResultPage::default()
    };
    assert!(detail(&check_universe_accounted(&no_left_out, &text)).contains("no left_out"));
}

/// Four lines hold `launch` as text: lib.rs:1-2, main.rs:1, LICENSE:1.
fn stakes_truth() -> BTreeSet<(String, u32)> {
    [
        ("LICENSE", 1),
        ("src/lib.rs", 1),
        ("src/lib.rs", 2),
        ("src/main.rs", 1),
    ]
    .iter()
    .map(|(p, l)| (p.to_string(), *l))
    .collect()
}

fn stakes_page(lines: &[(&str, u32)], counts: &[(&str, u64)]) -> ResultPage {
    ResultPage {
        rows: rows(
            &lines
                .iter()
                .map(|(p, _)| *p)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>(),
        ),
        occurrences: lines.iter().map(|(p, l)| occ(p, *l, None)).collect(),
        line_counts: counts.iter().map(|(p, n)| (p.to_string(), *n)).collect(),
        ..ResultPage::default()
    }
}

#[test]
fn stakes_lines_files_and_counts_compare_the_page_with_the_regex_oracle() {
    let truth = stakes_truth();
    let lines = [
        ("LICENSE", 1),
        ("src/lib.rs", 1),
        ("src/lib.rs", 2),
        ("src/main.rs", 1),
    ];
    let counts = [("LICENSE", 1), ("src/lib.rs", 2), ("src/main.rs", 1)];
    let exact = stakes_page(&lines, &counts);
    assert!(check_stakes_lines(&exact, &truth).is_pass());
    assert!(check_stakes_files(&exact, &truth).is_pass());
    assert!(check_stakes_counts(&exact, &truth).is_pass());

    // A missing line: it is the only line of src/main.rs, so the lines, the
    // files and the counts all fail on that file.
    let short = stakes_page(&lines[..3], &[("LICENSE", 1), ("src/lib.rs", 2)]);
    let d = detail(&check_stakes_lines(&short, &truth));
    assert!(
        d.contains("1 matching line(s) missing") && d.contains("src/main.rs:1"),
        "{d}"
    );
    let d = detail(&check_stakes_files(&short, &truth));
    assert!(
        d.contains("1 file(s) missing") && d.contains("src/main.rs"),
        "{d}"
    );
    let d = detail(&check_stakes_counts(&short, &truth));
    assert!(d.contains("src/main.rs has 0 line(s), the oracle 1"), "{d}");

    // An extra line in a known file: files agree, lines and counts differ.
    let mut more = lines.to_vec();
    more.push(("src/lib.rs", 3));
    let extra = stakes_page(
        &more,
        &[("LICENSE", 1), ("src/lib.rs", 3), ("src/main.rs", 1)],
    );
    let d = detail(&check_stakes_lines(&extra, &truth));
    assert!(
        d.contains("1 matching line(s) not in the oracle: src/lib.rs:3"),
        "{d}"
    );
    assert!(check_stakes_files(&extra, &truth).is_pass());
    let d = detail(&check_stakes_counts(&extra, &truth));
    assert!(d.contains("src/lib.rs has 3 line(s), the oracle 2"), "{d}");

    // An extra file.
    let stray = stakes_page(&[("src/zz.rs", 4)], &[("src/zz.rs", 1)]);
    assert!(detail(&check_stakes_files(&stray, &truth)).contains("not in the oracle: src/zz.rs"));
    assert!(
        detail(&check_stakes_counts(&stray, &truth))
            .contains("src/zz.rs has 1 line(s), the oracle 0")
    );

    // No answer at all, for a query the oracle finds hits for, fails.
    assert!(is_fail(&check_stakes_lines(&ResultPage::default(), &truth)));
    assert!(is_fail(&check_stakes_counts(
        &ResultPage::default(),
        &truth
    )));
    // And an empty answer for a query with no hit passes.
    let none = BTreeSet::new();
    assert!(check_stakes_lines(&ResultPage::default(), &none).is_pass());
    assert!(check_stakes_files(&ResultPage::default(), &none).is_pass());
    assert!(check_stakes_counts(&ResultPage::default(), &none).is_pass());
}

#[test]
fn stakes_exit_fails_on_a_zero_hit_entry_whose_run_exits_zero_and_passes_when_it_exits_one() {
    let exit = |code: i32| ResultPage {
        exit_code: code,
        ..ResultPage::default()
    };
    assert!(check_stakes_exit(&exit(1), 1).is_pass());
    let d = detail(&check_stakes_exit(&exit(0), 1));
    assert!(d.contains("exit code is 0, expected 1"), "{d}");
    assert!(check_stakes_exit(&exit(0), 0).is_pass());
    assert!(is_fail(&check_stakes_exit(&exit(2), 1)));
    // A process killed by a signal has no code; the runner records -1.
    assert!(is_fail(&check_stakes_exit(&exit(-1), 1)));
}

// --- #540: planning the new kinds ---------------------------------------------------

const REFS_TOML: &str = "[[refs]]\nid = \"skim-R01\"\nquery = \"launch\"\n\
     def = { path = \"a.rs\", line = 1 }\norigin = \"curated\"\n\
     sites = [ { path = \"a.rs\", line = 1, role = \"def\" } ]\n";

const CLASH_TOML: &str = "[[clash]]\nid = \"skim-D01\"\nquery = \"run\"\nsplit = \"none\"\n\
     origin = \"curated\"\ndefs = [ { path = \"a.rs\", line = 1 }, { path = \"b.rs\", line = 1 } ]\n\
     calls = []\n";

fn stakes_toml(expect: &str, exit: Option<u8>) -> String {
    let exit = exit.map_or(String::new(), |e| format!("exit = {e}\n"));
    format!(
        "[[stakes]]\nid = \"skim-S01\"\nquery = \"qzvx_absent_7713\"\nflags = []\nexpect = \"{expect}\"\n{exit}"
    )
}

#[test]
fn a_refs_entry_runs_the_refs_checks_and_the_unique_path_check_only() {
    assert_eq!(
        checks_of(REFS_TOML),
        vec![
            CheckId::ResultsUniquePaths,
            CheckId::RefsRecall,
            CheckId::RefsCallsRecall,
            CheckId::UniverseAccounted,
        ]
    );
    let g = golden_with(REFS_TOML);
    let q = &plan(&g).unwrap()[0];
    assert_eq!(q.kind, EntryKind::Refs);
    assert_eq!(q.query, "launch");
    assert!(q.flags.is_empty());
    assert!(q.refs_entry().is_some());
    assert!(!q.measures_text());
}

#[test]
fn a_clash_entry_runs_the_clash_checks_and_the_unique_path_check_only() {
    assert_eq!(
        checks_of(CLASH_TOML),
        vec![
            CheckId::ResultsUniquePaths,
            CheckId::ClashDefsComplete,
            CheckId::ClashDefCount,
            CheckId::ClashCallsRecall,
        ]
    );
}

#[test]
fn a_stakes_entry_runs_the_check_its_expect_names() {
    for (expect, exit, check) in [
        ("lines", None, CheckId::StakesLines),
        ("files", None, CheckId::StakesFiles),
        ("counts", None, CheckId::StakesCounts),
        ("exit", Some(1), CheckId::StakesExit),
    ] {
        assert_eq!(
            checks_of(&stakes_toml(expect, exit)),
            vec![CheckId::ResultsUniquePaths, check],
            "expect = {expect}"
        );
    }
}

#[test]
fn a_stakes_entry_compiles_its_flags_into_the_regex_oracle_query() {
    let g = golden_with(
        "[[stakes]]\nid = \"skim-S02\"\nquery = \"Launch\"\nflags = [\"-i\", \"-w\"]\nexpect = \"lines\"\n",
    );
    let q = &plan(&g).unwrap()[0];
    assert_eq!(q.flags, ["-i", "-w"], "the runner passes the flags to skim");
    let stakes = q.stakes_oracle().unwrap();
    let StakesRun::Search { query, filter } = &stakes.run else {
        unreachable!("a valid query is a search: {:?}", stakes.run);
    };
    assert!(query.matches_line("fn launch() {}"), "-i");
    assert!(!query.matches_line("fn relaunch() {}"), "-w");
    assert!(filter.is_empty());
}

#[test]
fn a_stakes_entry_compiles_its_type_and_glob_flags_into_a_path_filter() {
    let g = golden_with(
        "[[stakes]]\nid = \"skim-S03\"\nquery = \"x\"\nflags = [\"-t\", \"rust\", \"-g\", \"!tests\"]\nexpect = \"files\"\n",
    );
    let stakes = plan(&g)
        .unwrap()
        .remove(0)
        .stakes_oracle()
        .cloned()
        .unwrap();
    let StakesRun::Search { filter, .. } = stakes.run else {
        unreachable!("a valid query is a search");
    };
    assert!(filter.keeps("src/a.rs"));
    assert!(!filter.keeps("tests/a.rs") && !filter.keeps("src/a.py"));
    // A type or glob the oracle does not cover stops the plan instead of
    // being ignored.
    for flags in ["[\"-t\", \"haskell\"]", "[\"-g\", \"*.{rs,py}\"]"] {
        let unknown = golden_with(&format!(
            "[[stakes]]\nid = \"skim-S04\"\nquery = \"x\"\nflags = {flags}\nexpect = \"files\"\n"
        ));
        assert!(plan(&unknown).is_err(), "{flags}");
    }
}

#[test]
fn a_stakes_usage_error_is_read_from_the_flags_not_searched() {
    for flags in [
        "[\"-F\", \"-E\"]",
        "[\"-l\", \"-c\"]",
        "[\"-l\", \"-C\", \"2\"]",
        "[\"-c\", \"--all\"]",
    ] {
        let g = golden_with(&format!(
            "[[stakes]]\nid = \"skim-S05\"\nquery = \"x\"\nflags = {flags}\nexpect = \"exit\"\nexit = 2\n"
        ));
        let stakes = plan(&g)
            .unwrap()
            .remove(0)
            .stakes_oracle()
            .cloned()
            .unwrap();
        assert!(matches!(stakes.run, StakesRun::Usage(_)), "{flags}");
    }
}

#[test]
fn only_a_lexical_table_entry_gains_the_tracked_recall_check() {
    // A paginated query has a lexical oracle but is not a `[[lexical]]` entry.
    let paginated =
        checks_of("[[pagination]]\nid = \"skim-G001\"\nquery = \"build lock\"\nlimits = [3]\n");
    assert!(!paginated.contains(&CheckId::LexicalRecallTracked));
}

#[test]
fn a_concept_entry_runs_first_right_top5_only_when_it_freezes_first_files() {
    let without = "[[concept]]\nid = \"skim-C01\"\nquery = \"a b\"\nrelevant = 'a'\n";
    assert!(!checks_of(without).contains(&CheckId::ConceptFirstRightTop5));
    let with = format!("{without}first = [\"src/a.rs\"]\n");
    assert!(checks_of(&with).contains(&CheckId::ConceptFirstRightTop5));
    let g = golden_with(&with);
    let q = &plan(&g).unwrap()[0];
    assert_eq!(q.first_files(), Some(["src/a.rs".to_string()].as_slice()));
}

#[test]
fn every_check_that_runs_on_an_entry_applies_to_its_kind() {
    let body = format!(
        "[[ident]]\nid = \"skim-L01\"\nquery = \"x_y\"\ndef = {{ path = \"a.rs\", line = 1 }}\norigin = \"seed\"\n\
         [[concept]]\nid = \"skim-C01\"\nquery = \"a b\"\nrelevant = 'a'\nfirst = [\"a.rs\"]\n\
         [[lexical]]\nid = \"skim-X01\"\nquery = \"q\"\ncategory = \"short\"\n\
         [[pagination]]\nid = \"skim-G001\"\nquery = \"x\"\nlimits = [3]\n\
         [[prefix]]\nid = \"skim-F001\"\nquery = \"x\"\nlimits = [5]\n\
         {REFS_TOML}{CLASH_TOML}{}",
        stakes_toml("exit", Some(1)),
    );
    let planned = plan(&golden_with(&body)).unwrap();
    let kinds: BTreeSet<EntryKind> = planned.iter().map(|q| q.kind).collect();
    assert_eq!(kinds.len(), 8, "one entry of every kind: {kinds:?}");
    for q in &planned {
        for check in CheckId::ALL {
            if q.runs(*check) {
                assert!(
                    check.applies_to(q.kind),
                    "{check} runs on {} ({:?})",
                    q.id,
                    q.kind
                );
            }
        }
        assert!(q.runs(CheckId::ResultsUniquePaths), "{}", q.id);
    }
}

// --- #540: RATCHET and reference rows -----------------------------------------------

#[test]
fn the_new_ratchet_and_reference_rows_are_defined_with_their_direction() {
    let direction = |name: &str| metric_def(name).expect("metric is defined").direction;
    assert_eq!(direction("refs.call_precision"), Direction::HigherBetter);
    for name in [
        "cost.files_parsed_median",
        "cost.bytes_parsed_median",
        "cost.bytes_read_median",
    ] {
        assert_eq!(direction(name), Direction::LowerBetter, "{name}");
        assert_eq!(metric_def(name).unwrap().tolerance, Tolerance::Exact);
    }
    for lang in ["rust", "python", "typescript", "tsx", "javascript", "go"] {
        let name = format!("roles.precision.{lang}");
        assert_eq!(direction(&name), Direction::HigherBetter, "{name}");
    }
    // The `rg -nw` columns are INFO reference rows: not RATCHET metrics.
    for name in ["bytes.rgw_text_median", "bytes.rgw_first_correct_median"] {
        assert!(metric_def(name).is_none(), "{name} must not ratchet");
    }
    // Moves read as the bar says.
    assert_eq!(
        compare_ratchet("refs.call_precision", 0.8, 0.6),
        RatchetChange::Regressed
    );
    assert_eq!(
        compare_ratchet("cost.files_parsed_median", 3.0, 2.0),
        RatchetChange::Improved
    );
    assert_eq!(
        compare_ratchet("cost.bytes_read_median", 3.0, 4.0),
        RatchetChange::Regressed
    );
}

fn cost(files_parsed: u64, bytes_parsed: u64, bytes_read: u64) -> Cost {
    Cost {
        files_read: 0,
        bytes_read,
        files_parsed,
        bytes_parsed,
    }
}

#[test]
fn ratchet_values_pool_the_call_precision_and_the_cost_medians() {
    let mut s = samples(vec![
        ident("a-1", Some(1), Some(40)),
        ident("a-2", Some(2), None),
    ]);
    s.refs = vec![
        RefsSample {
            id: "r1".into(),
            call_precision: Some(1.0),
        },
        RefsSample {
            id: "r2".into(),
            call_precision: Some(0.5),
        },
        RefsSample {
            id: "r3".into(),
            call_precision: None,
        },
    ];
    s.costs = vec![cost(3, 100, 1000), cost(5, 300, 2000), cost(1, 50, 500)];
    let r = ratchet_values(&[&s]);
    assert_eq!(
        r["refs.call_precision"], 0.75,
        "the mean skips an entry with no value"
    );
    assert_eq!(r["cost.files_parsed_median"], 3.0);
    assert_eq!(r["cost.bytes_parsed_median"], 100.0);
    assert_eq!(r["cost.bytes_read_median"], 1000.0);
    // The `rg -nw` columns are INFO: they never reach the RATCHET values.
    assert!(
        r.keys().all(|name| !name.contains("rgw")),
        "{:?}",
        r.keys().collect::<Vec<_>>()
    );

    // Two corpora pool their entries.
    let mut t = samples(Vec::new());
    t.refs = vec![RefsSample {
        id: "r4".into(),
        call_precision: Some(0.25),
    }];
    t.costs = vec![cost(9, 900, 9000)];
    let pooled = ratchet_values(&[&s, &t]);
    assert_eq!(pooled["refs.call_precision"], 0.5833);
    assert_eq!(
        pooled["cost.files_parsed_median"], 3.0,
        "median of 1, 3, 5, 9"
    );
}

fn concept_sample(id: &str) -> ConceptSample {
    ConceptSample {
        id: id.to_string(),
        p5: 0.5,
        p10: 0.5,
        p5_baseline_alpha: 0.2,
        p10_baseline_alpha: 0.2,
        p5_baseline_count: 0.4,
        p10_baseline_count: 0.4,
        text_bytes: 100,
        first_relevant_bytes: None,
        rg_text_bytes: 300,
        rgw_text_bytes: 200,
    }
}

#[test]
fn reference_values_pool_the_rg_word_columns_over_idents_and_concepts() {
    let mut s = samples(vec![
        ident("a-1", Some(1), Some(40)),
        IdentSample {
            rgw_text_bytes: 400,
            rgw_first_correct_bytes: None,
            ..ident("a-2", Some(2), None)
        },
    ]);
    s.concepts = vec![ConceptSample {
        rgw_text_bytes: 600,
        ..concept_sample("a-c1")
    }];
    let r = reference_values(&[&s]);
    // The text median is over both kinds of entry (200, 400, 600); the
    // first-correct median is over the idents that state one.
    assert_eq!(r["bytes.rgw_text_median"], 400.0);
    assert_eq!(r["bytes.rgw_first_correct_median"], 30.0);
    assert_eq!(r.len(), 2);

    // Two corpora pool their entries.
    let t = samples(vec![IdentSample {
        rgw_first_correct_bytes: Some(90),
        ..ident("b-1", Some(1), Some(10))
    }]);
    let pooled = reference_values(&[&s, &t]);
    assert_eq!(
        pooled["bytes.rgw_first_correct_median"], 30.0,
        "lower middle of 30, 90"
    );
    assert_eq!(
        pooled["bytes.rgw_text_median"], 200.0,
        "lower middle of 200, 200, 400, 600"
    );
}

#[test]
fn a_reference_row_with_no_value_is_absent() {
    assert!(reference_values(&[&samples(Vec::new())]).is_empty());
    // Idents that never state a first-correct byte count give no median.
    let miss = IdentSample {
        rgw_first_correct_bytes: None,
        ..ident("a-1", None, None)
    };
    let r = reference_values(&[&samples(vec![miss])]);
    assert!(!r.contains_key("bytes.rgw_first_correct_median"));
    assert!(r.contains_key("bytes.rgw_text_median"));
}

#[test]
fn a_row_with_no_value_is_absent_so_it_cannot_fail_the_gate() {
    // The current binary states no cost and labels no call: no entry has a
    // value for these rows.
    let mut s = samples(vec![ident("a-1", Some(1), Some(40))]);
    s.refs = vec![RefsSample {
        id: "r1".into(),
        call_precision: None,
    }];
    let r = ratchet_values(&[&s]);
    for name in r.keys() {
        assert!(
            !name.starts_with("refs.") && !name.starts_with("cost.") && !name.starts_with("roles."),
            "{name} has no value to record"
        );
    }
    // The rows are defined all the same.
    assert!(metric_def("roles.precision.go").is_some());
    assert!(metric_def("cost.bytes_parsed_median").is_some());
}

// --- #540: evaluate over the new kinds ----------------------------------------------

#[test]
fn evaluate_judges_refs_clash_stakes_and_tracked_recall_entries_against_their_frozen_answers() {
    let repo = FixtureRepo::new();
    repo.write(
        "src/lib.rs",
        "pub fn launch() {}\nfn start() { launch(); }\n// launch is documented\n",
    );
    repo.write("src/main.rs", "fn main() { launch(); }\n");
    repo.write("LICENSE", "launch it\n");
    repo.write("src/a.rs", "pub fn run() {}\nfn go() { run(); }\n");
    repo.write("src/b.rs", "pub fn run(x: u32) {}\n");
    let commit = repo.commit_all("init");
    let git = GitIsolation::new(repo.home());
    let text = text_universe(repo.root(), &git).unwrap();
    assert_eq!(text.len(), 5);

    let golden = parse_golden(&format!(
        "corpus = \"skim\"\ncommit = \"{commit}\"\n\
         [[lexical]]\nid = \"skim-X01\"\nquery = \"launch\"\ncategory = \"substr\"\n\
         {}{}\
         [[stakes]]\nid = \"skim-S01\"\nquery = \"launch\"\nexpect = \"exit\"\nexit = 0\n\
         [[stakes]]\nid = \"skim-S02\"\nquery = \"qzvx_absent_7713\"\nexpect = \"exit\"\nexit = 1\n\
         [[stakes]]\nid = \"skim-S03\"\nquery = \"launch\"\nexpect = \"lines\"\n",
        refs_entry_toml(),
        clash_entry_toml()
    ))
    .unwrap();
    let plan = plan(&golden).unwrap();

    let all_zero: BTreeMap<String, u64> = LEFT_OUT_REASONS
        .iter()
        .map(|r| (r.to_string(), 0))
        .collect();
    let refs_page = ResultPage {
        occurrences: refs_occurrences(),
        searched_files: Some(5),
        left_out: Some(all_zero),
        cost: Some(cost(2, 50, 100)),
        ..ResultPage::default()
    };
    let clash_page = ResultPage {
        occurrences: vec![
            occ("src/a.rs", 2, Some(Role::Call)),
            occ("src/c.rs", 7, Some(Role::Call)),
        ],
        ..with_definitions(Some(2), false, &[("src/a.rs", 1), ("src/b.rs", 1)])
    };
    let launch_lines = [
        ("LICENSE", 1),
        ("src/lib.rs", 1),
        ("src/lib.rs", 2),
        ("src/lib.rs", 3),
        ("src/main.rs", 1),
    ];
    let page_for = |id: &str, tracked: bool| -> Option<ResultPage> {
        Some(match id {
            "skim-X01" => {
                let mut paths = vec!["src/lib.rs", "src/main.rs"];
                if tracked {
                    paths.push("LICENSE");
                }
                page(rows(&paths), false)
            }
            "skim-R01" => refs_page.clone(),
            "skim-D01" => clash_page.clone(),
            "skim-S01" => ResultPage {
                exit_code: 0,
                ..ResultPage::default()
            },
            "skim-S02" => ResultPage {
                exit_code: 1,
                ..ResultPage::default()
            },
            "skim-S03" => stakes_page(&launch_lines, &[]),
            _ => return None,
        })
    };
    let observe = |tracked: bool| -> Vec<EntryObservation> {
        plan.iter()
            .map(|q| EntryObservation {
                id: q.id.clone(),
                full: page_for(&q.id, tracked).expect("a page is frozen for every planned entry"),
                sweeps: Vec::new(),
                limited: Vec::new(),
                text: None,
            })
            .collect()
    };
    let run = |tracked: bool| evaluate(&text, &plan, &observe(tracked)).unwrap();

    let e = run(false);
    let outcome = |e: &CorpusEvaluation, id: &str, check: CheckId| {
        e.outcomes
            .iter()
            .find(|(i, c, _)| i == id && *c == check)
            .map(|(_, _, o)| o.clone())
            .expect("the check ran")
    };
    // Both recall checks read the one text universe, so a missing LICENSE
    // fails both and a page that lists it passes both.
    for check in [CheckId::LexicalRecall, CheckId::LexicalRecallTracked] {
        let d = detail(&outcome(&e, "skim-X01", check));
        assert!(d.contains("LICENSE"), "{check}: {d}");
        assert!(outcome(&run(true), "skim-X01", check).is_pass(), "{check}");
    }

    for check in [
        CheckId::RefsRecall,
        CheckId::RefsCallsRecall,
        CheckId::UniverseAccounted,
    ] {
        assert!(outcome(&e, "skim-R01", check).is_pass(), "{check}");
    }
    for check in [
        CheckId::ClashDefsComplete,
        CheckId::ClashDefCount,
        CheckId::ClashCallsRecall,
    ] {
        assert!(outcome(&e, "skim-D01", check).is_pass(), "{check}");
    }
    assert!(outcome(&e, "skim-S01", CheckId::StakesExit).is_pass());
    assert!(outcome(&e, "skim-S02", CheckId::StakesExit).is_pass());
    assert!(outcome(&e, "skim-S03", CheckId::StakesLines).is_pass());
    for id in ["skim-R01", "skim-D01", "skim-S01", "skim-S02", "skim-S03"] {
        assert!(
            outcome(&e, id, CheckId::ResultsUniquePaths).is_pass(),
            "{id}"
        );
    }

    // A frozen call the page lists with no role is not a call: only the clash
    // call recall fails, and it names the site and the missing role.
    let unlabelled_clash_page = ResultPage {
        occurrences: vec![
            occ("src/a.rs", 2, Some(Role::Call)),
            occ("src/c.rs", 7, None),
        ],
        ..clash_page.clone()
    };
    let mut unlabelled = observe(false);
    unlabelled
        .iter_mut()
        .find(|o| o.id == "skim-D01")
        .expect("the clash entry is planned")
        .full = unlabelled_clash_page;
    let e = evaluate(&text, &plan, &unlabelled).unwrap();
    let calls = outcome(&e, "skim-D01", CheckId::ClashCallsRecall);
    assert!(is_fail(&calls), "a call with no role must fail");
    let d = detail(&calls);
    assert!(d.contains("src/c.rs:7 (role none)"), "{d}");
    for check in [CheckId::ClashDefsComplete, CheckId::ClashDefCount] {
        assert!(outcome(&e, "skim-D01", check).is_pass(), "{check}");
    }

    // The samples the new rows read; a frozen entry is not oracle-less.
    assert_eq!(e.samples.refs.len(), 1);
    assert_eq!(e.samples.refs[0].call_precision, Some(1.0));
    assert_eq!(e.samples.costs, vec![cost(2, 50, 100)]);
    // The role labels of the refs page (3, all confirmed) and of the clash page
    // (1, confirmed) both reach the Rust tally.
    assert_eq!(
        e.samples.roles,
        BTreeMap::from([(
            OracleLang::Rust,
            RoleTally {
                labelled: 4,
                confirmed: 4
            }
        )])
    );
}

fn refs_entry_toml() -> &'static str {
    "[[refs]]\nid = \"skim-R01\"\nquery = \"launch\"\n\
     def = { path = \"src/lib.rs\", line = 1 }\norigin = \"curated\"\n\
     sites = [\n\
       { path = \"src/lib.rs\", line = 1, role = \"def\" },\n\
       { path = \"src/lib.rs\", line = 2, role = \"call\" },\n\
       { path = \"src/lib.rs\", line = 3, role = \"comment\" },\n\
       { path = \"src/main.rs\", line = 1, role = \"call\" },\n\
       { path = \"LICENSE\", line = 1, role = \"text\" },\n\
     ]\n"
}

fn clash_entry_toml() -> &'static str {
    "[[clash]]\nid = \"skim-D01\"\nquery = \"run\"\nsplit = \"none\"\norigin = \"curated\"\n\
     defs = [ { path = \"src/a.rs\", line = 1 }, { path = \"src/b.rs\", line = 1 } ]\n\
     calls = [\n\
       { path = \"src/a.rs\", line = 2, def = 0 },\n\
       { path = \"src/c.rs\", line = 7, def = \"unassigned\" },\n\
     ]\n"
}

#[test]
fn evaluate_judges_each_stakes_expectation_with_its_own_comparison() {
    let repo = FixtureRepo::new();
    repo.write(
        "src/lib.rs",
        "pub fn launch() {}\nfn start() { launch(); }\n// launch is documented\n",
    );
    repo.write("src/main.rs", "fn main() { launch(); }\n");
    repo.write("LICENSE", "launch it\n");
    let commit = repo.commit_all("init");
    let git = GitIsolation::new(repo.home());
    let text = text_universe(repo.root(), &git).unwrap();

    // One query, three expectations: the lines, the files and the counts.
    let golden = parse_golden(&format!(
        "corpus = \"skim\"\ncommit = \"{commit}\"\n\
         [[stakes]]\nid = \"skim-S01\"\nquery = \"launch\"\nexpect = \"lines\"\n\
         [[stakes]]\nid = \"skim-S02\"\nquery = \"launch\"\nexpect = \"files\"\n\
         [[stakes]]\nid = \"skim-S03\"\nquery = \"launch\"\nexpect = \"counts\"\n",
    ))
    .unwrap();
    let plan = plan(&golden).unwrap();
    let all_lines = [
        ("LICENSE", 1),
        ("src/lib.rs", 1),
        ("src/lib.rs", 2),
        ("src/lib.rs", 3),
        ("src/main.rs", 1),
    ];
    let right_counts = [("LICENSE", 1), ("src/lib.rs", 3), ("src/main.rs", 1)];
    let short_counts = [("LICENSE", 1), ("src/lib.rs", 2), ("src/main.rs", 1)];

    // The `launch` line of src/lib.rs:3 is not listed.
    let missing_line = stakes_page(
        &[all_lines[0], all_lines[1], all_lines[2], all_lines[4]],
        &short_counts,
    );
    // Every line is listed, one file count is short.
    let short_count = stakes_page(&all_lines, &short_counts);
    // Every line and count is right, one file is listed that holds no match.
    let extra_file = ResultPage {
        rows: rows(&["LICENSE", "src/lib.rs", "src/main.rs", "src/zz.rs"]),
        ..stakes_page(&all_lines, &right_counts)
    };

    for (name, page, lines, files, counts) in [
        ("a missing line", missing_line, false, true, false),
        ("a short count", short_count, true, true, false),
        ("an extra file", extra_file, true, false, true),
        (
            "an exact page",
            stakes_page(&all_lines, &right_counts),
            true,
            true,
            true,
        ),
    ] {
        let observations: Vec<EntryObservation> = plan
            .iter()
            .map(|q| EntryObservation {
                id: q.id.clone(),
                full: accounted(page.clone(), 3),
                sweeps: Vec::new(),
                limited: Vec::new(),
                text: None,
            })
            .collect();
        let e = evaluate(&text, &plan, &observations).unwrap();
        let passed = |id: &str, check: CheckId| {
            e.outcomes
                .iter()
                .find(|(i, c, _)| i == id && *c == check)
                .map(|(_, _, o)| o.is_pass())
                .expect("the check ran")
        };
        assert_eq!(passed("skim-S01", CheckId::StakesLines), lines, "{name}");
        assert_eq!(passed("skim-S02", CheckId::StakesFiles), files, "{name}");
        assert_eq!(passed("skim-S03", CheckId::StakesCounts), counts, "{name}");
    }
}

// --- roles.precision ----------------------------------------------------------------

fn tally(labelled: u64, confirmed: u64) -> RoleTally {
    RoleTally {
        labelled,
        confirmed,
    }
}

/// `launch` is defined at line 1 of `src/lib.rs` and called at line 2.
const RUST_LAUNCH: &str = "fn launch() {}\nfn start() { launch(); }\n// launch\n";
/// `launch` is imported at line 1 of `app.py` and called at line 2.
const PYTHON_LAUNCH: &str = "from m import launch\nlaunch()\n";

fn launch_source(path: &str) -> Option<&'static str> {
    match path {
        "src/lib.rs" => Some(RUST_LAUNCH),
        "app.py" => Some(PYTHON_LAUNCH),
        _ => None,
    }
}

#[test]
fn page_role_tallies_confirm_a_label_only_when_the_oracle_lists_that_line_for_that_role() {
    let page = with_occurrences(vec![
        occ("src/lib.rs", 1, Some(Role::Def)),  // confirmed
        occ("src/lib.rs", 2, Some(Role::Call)), // confirmed
        occ("src/lib.rs", 2, Some(Role::Def)),  // a call line labelled def
        occ("src/lib.rs", 1, Some(Role::Call)), // a def line labelled call
        occ("src/lib.rs", 3, Some(Role::Call)), // a comment labelled call
        occ("app.py", 1, Some(Role::Import)),   // confirmed
        occ("app.py", 2, Some(Role::Call)),     // confirmed
        occ("app.py", 2, Some(Role::Import)),   // a call line labelled import
    ]);
    let t = page_role_tallies(&page, "launch", launch_source).unwrap();
    assert_eq!(t[&OracleLang::Rust], tally(5, 2));
    assert_eq!(t[&OracleLang::Python], tally(3, 2));
    assert_eq!(t.len(), 2);
}

#[test]
fn page_role_tallies_skip_other_roles_other_languages_and_count_a_line_once_per_role() {
    let page = with_occurrences(vec![
        occ("src/lib.rs", 1, Some(Role::Def)),
        occ("src/lib.rs", 1, Some(Role::Def)),
        occ("src/lib.rs", 2, Some(Role::Ref)),
        occ("src/lib.rs", 3, Some(Role::Comment)),
        occ("src/lib.rs", 3, Some(Role::Partial)),
        occ("src/lib.rs", 3, None),
        occ("README.md", 1, Some(Role::Def)),
        occ("notes.txt", 1, Some(Role::Call)),
    ]);
    let t = page_role_tallies(&page, "launch", launch_source).unwrap();
    assert_eq!(t, BTreeMap::from([(OracleLang::Rust, tally(1, 1))]));
    assert!(
        page_role_tallies(&ResultPage::default(), "launch", launch_source)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn page_role_tallies_count_a_label_on_a_file_outside_the_universe_as_not_confirmed() {
    let page = with_occurrences(vec![
        occ("src/gone.rs", 1, Some(Role::Def)),
        occ("src/lib.rs", 1, Some(Role::Def)),
    ]);
    let t = page_role_tallies(&page, "launch", launch_source).unwrap();
    assert_eq!(t[&OracleLang::Rust], tally(2, 1));
}

#[test]
fn page_role_tallies_fail_when_the_role_oracle_fails() {
    let page = with_occurrences(vec![occ("src/lib.rs", 1, Some(Role::Def))]);
    let err = page_role_tallies(&page, "", launch_source).unwrap_err();
    assert!(format!("{err:#}").contains("non-empty name"), "{err:#}");
}

#[test]
fn roles_precision_pools_labels_over_corpora_and_leaves_a_language_with_no_label_out() {
    let mut a = samples(Vec::new());
    a.roles = BTreeMap::from([
        (OracleLang::Rust, tally(1, 1)),
        (OracleLang::Python, tally(2, 0)),
    ]);
    let mut b = samples(Vec::new());
    b.roles = BTreeMap::from([(OracleLang::Rust, tally(3, 1))]);

    let one = ratchet_values(&[&a]);
    assert_eq!(one["roles.precision.rust"], 1.0);
    assert_eq!(one["roles.precision.python"], 0.0);

    // Pooled labels, not the mean of the two corpus ratios (that would be 0.6667).
    let pooled = ratchet_values(&[&a, &b]);
    assert_eq!(pooled["roles.precision.rust"], 0.5);
    for lang in ["typescript", "tsx", "javascript", "go"] {
        assert!(
            !pooled.contains_key(&format!("roles.precision.{lang}")),
            "{lang} has no label, so it has no value"
        );
    }
}
