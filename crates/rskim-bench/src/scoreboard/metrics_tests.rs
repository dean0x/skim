//! Unit tests for `metrics.rs` (co-located file, `#[path]`-included).

use super::*;
use crate::scoreboard::golden::parse_golden;
use crate::scoreboard::runner::{SweepPage, TextOutput};
use crate::scoreboard::test_support::FixtureRepo;
use crate::scoreboard::types::{Degraded, SnippetLine};
use crate::scoreboard::universe::GitIsolation;

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
        verify_mode: VerifyMode::Substring,
        degraded: Vec::new(),
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
    let expected = vec![
        CheckId::LexicalRecall,
        CheckId::LexicalPrecision,
        CheckId::LexicalSilentFn,
        CheckId::LexicalVerifyMode,
        CheckId::OrderScoreMonotone,
    ];
    assert_eq!(
        checks_of(
            "[[ident]]\nid = \"skim-L01\"\nquery = \"x_y\"\ndef = { path = \"a.rs\", line = 1 }\norigin = \"seed\"\n"
        ),
        expected
    );
    assert_eq!(
        checks_of("[[concept]]\nid = \"skim-C01\"\nquery = \"a b\"\nrelevant = 'a'\n"),
        expected
    );
    assert_eq!(
        checks_of(
            "[[lexical]]\nid = \"skim-X01\"\nquery = \"a b\"\nmode = \"near\"\nnear = 3\ncategory = \"near\"\n"
        ),
        expected
    );
}

#[test]
fn a_temporal_pagination_entry_runs_pagination_and_lexical_checks_but_not_score_monotone() {
    assert_eq!(
        checks_of(
            "[[pagination]]\nid = \"skim-G001\"\nquery = \"build lock\"\nflags = [\"--hot\"]\nlimits = [3]\n"
        ),
        vec![
            CheckId::LexicalRecall,
            CheckId::LexicalPrecision,
            CheckId::LexicalSilentFn,
            CheckId::LexicalVerifyMode,
            CheckId::PaginationComplete,
            CheckId::PaginationDisjoint,
            CheckId::PaginationOrdered,
            CheckId::PaginationHasMoreHonest,
        ]
    );
}

#[test]
fn a_text_plus_ast_entry_has_no_oracle_checks() {
    assert_eq!(
        checks_of(
            "[[pagination]]\nid = \"skim-G002\"\nquery = \"x\"\nflags = [\"--ast\", \"try-catch\"]\nlimits = [3]\n"
        ),
        vec![
            CheckId::LexicalVerifyMode,
            CheckId::PaginationComplete,
            CheckId::PaginationDisjoint,
            CheckId::PaginationOrdered,
            CheckId::PaginationHasMoreHonest,
            CheckId::OrderScoreMonotone,
        ]
    );
}

#[test]
fn standalone_prefix_entries_check_prefix_and_monotonicity_only_where_score_ranks() {
    assert_eq!(
        checks_of(
            "[[prefix]]\nid = \"skim-F001\"\nflags = [\"--ast\", \"god-function\"]\nlimits = [5]\n"
        ),
        vec![CheckId::OrderPrefixConsistent, CheckId::OrderScoreMonotone]
    );
    assert_eq!(
        checks_of("[[prefix]]\nid = \"skim-F002\"\nflags = [\"--hot\"]\nlimits = [5]\n"),
        vec![CheckId::OrderPrefixConsistent]
    );
    let g = golden_with("[[prefix]]\nid = \"skim-F002\"\nflags = [\"--hot\"]\nlimits = [5]\n");
    let q = &plan(&g).unwrap()[0];
    assert_eq!((q.arm, q.query.as_deref()), (Arm::HotCold, None));
    assert!(!q.measures_text());
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
fn a_disclosed_miss_is_not_silent() {
    let mut p = page(rows(&["a.rs"]), false);
    p.degraded = vec![Degraded {
        subsystem: "temporal".to_string(),
        reason: "missing".to_string(),
        requested: None,
        applied: None,
    }];
    let truth = gt(&["a.rs", "b.rs"]);
    assert!(is_fail(&check_recall(&p.rows, &truth)));
    assert!(check_silent_fn(&p, &truth).is_pass());
}

#[test]
fn an_extra_file_fails_precision_naming_it() {
    let p = page(rows(&["a.rs", "z.rs"]), false);
    let precision = check_precision(&p.rows, &gt(&["a.rs"]));
    assert!(is_fail(&precision));
    assert!(detail(&precision).contains("z.rs"));
}

#[test]
fn verify_mode_must_match_the_declared_mode() {
    let mut p = page(Vec::new(), false);
    assert!(check_verify_mode(&p, &VerifyMode::Substring).is_pass());
    p.verify_mode = VerifyMode::Phrase;
    let o = check_verify_mode(&p, &VerifyMode::Near);
    assert!(is_fail(&o));
    assert!(
        detail(&o).contains("phrase") && detail(&o).contains("near"),
        "{}",
        detail(&o)
    );
}

#[test]
fn score_must_not_increase_down_the_list() {
    assert!(check_score_monotone(&[row("a", 3.0), row("b", 3.0), row("c", 1.0)]).is_pass());
    assert!(check_score_monotone(&[]).is_pass());
    let o = check_score_monotone(&[row("a", 3.0), row("b", 1.0), row("c", 2.0)]);
    assert!(is_fail(&o));
    assert!(detail(&o).contains("rank 3"), "{}", detail(&o));
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
fn durations_are_normalized_away() {
    assert_eq!(
        normalize_durations(b"3 result(s) for \"q\" in 1234ms\nin ms in 5s"),
        b"3 result(s) for \"q\" in 0ms\nin ms in 5s".to_vec()
    );
}

#[test]
fn bytes_through_the_def_block_include_its_trailing_blank_line() {
    let out = b"a.rs:3  [text]  score: 2.00\n  >     3| x\n\nb.rs:9  [text]  score: 1.00\n  >     9| y\n\n2 result(s) for \"q\" in 0ms\n";
    let first = "a.rs:3  [text]  score: 2.00\n  >     3| x\n\n".len() as u64;
    assert_eq!(bytes_through_block(out, "a.rs"), Some(first));
    assert_eq!(
        bytes_through_block(out, "b.rs"),
        Some(first + "b.rs:9  [text]  score: 1.00\n  >     9| y\n\n".len() as u64)
    );
    assert_eq!(bytes_through_block(out, "c.rs"), None);
    // A path that merely prefixes another header is not that file's block.
    assert_eq!(bytes_through_block(b"a.rs.bak:1  [text]\n\n", "a.rs"), None);
    // A line-less header (short query) still counts.
    assert_eq!(bytes_through_block(b"a.rs  [text]\n", "a.rs"), Some(13));
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

#[test]
fn evaluate_scores_a_fixture_corpus_end_to_end() {
    let repo = FixtureRepo::new();
    repo.write("src/a.rs", "// check_staleness lives in b\n");
    repo.write("src/b.rs", "fn x() {}\npub fn check_staleness() {}\n");
    repo.write("src/c.rs", "// build lock\nfn y() {}\n");
    let commit = repo.commit_all("init");
    let universe = Universe::compute(repo.root(), &GitIsolation::new(repo.home())).unwrap();
    let golden = parse_golden(&format!(
        "corpus = \"skim\"\ncommit = \"{commit}\"\n\
         [[ident]]\nid = \"skim-L01\"\nquery = \"check_staleness\"\ndef = {{ path = \"src/b.rs\", line = 2 }}\norigin = \"seed\"\n\
         [[concept]]\nid = \"skim-C01\"\nquery = \"lock\"\nrelevant = '(?i)build[_\\s-]*lock'\n"
    ))
    .unwrap();
    let plan = plan(&golden).unwrap();
    let stats = StatsSnapshot {
        file_count: 3,
        skipped_by_reason: BTreeMap::new(),
        temporal_state: None,
    };

    let mut ident_rows = vec![row("src/b.rs", 2.0), row("src/a.rs", 1.0)];
    ident_rows[0].line = Some(2);
    ident_rows[0].snippet = vec![SnippetLine {
        line_number: 2,
        content: "pub fn check_staleness() {}".to_string(),
        is_match: true,
    }];
    let observations = vec![
        EntryObservation {
            id: "skim-L01".to_string(),
            full: page(ident_rows, false),
            sweeps: Vec::new(),
            limited: Vec::new(),
            text: Some(TextOutput {
                stdout: b"src/b.rs:2  [text]  score: 2.00\n  >     2| pub fn check_staleness() {}\n\nsrc/a.rs:1  [text]  score: 1.00\n\n2 result(s) for \"check_staleness\" in 3ms\n".to_vec(),
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
                stdout: b"src/c.rs:1\n\n".to_vec(),
                stderr: b"note\n".to_vec(),
            }),
        },
    ];

    let e = evaluate(&universe, &stats, &plan, &observations).unwrap();

    let outcome = |id: &str, check: CheckId| {
        e.outcomes
            .iter()
            .find(|(i, c, _)| i == id && *c == check)
            .map(|(_, _, o)| o.clone())
            .unwrap()
    };
    assert!(outcome("skim-L01", CheckId::LexicalRecall).is_pass());
    assert!(outcome("skim-L01", CheckId::LexicalPrecision).is_pass());
    assert!(is_fail(&outcome("skim-C01", CheckId::LexicalPrecision)));
    let sorted = {
        let mut s = e.outcomes.clone();
        s.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
        s
    };
    assert_eq!(e.outcomes, sorted, "outcomes are sorted by (id, check)");

    assert_eq!(e.samples.universe_delta, 0);
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
    let block = "src/b.rs:2  [text]  score: 2.00\n  >     2| pub fn check_staleness() {}\n\n";
    assert_eq!(l.first_correct_bytes, Some(block.len() as u64));
    let rg = "src/a.rs:1:// check_staleness lives in b\nsrc/b.rs:2:pub fn check_staleness() {}\n";
    assert_eq!(l.rg_first_correct_bytes, Some(rg.len() as u64));
    let c = &e.samples.concepts[0];
    assert_eq!((c.p5, c.p10), (0.5, 0.5));
    assert_eq!((c.p5_baseline_alpha, c.p5_baseline_count), (1.0, 1.0));
    assert_eq!(c.text_bytes, 17);
}

#[test]
fn evaluate_rejects_observations_that_do_not_follow_the_plan() {
    let repo = FixtureRepo::new();
    repo.write("a.rs", "x\n");
    let commit = repo.commit_all("init");
    let universe = Universe::compute(repo.root(), &GitIsolation::new(repo.home())).unwrap();
    let golden = parse_golden(&format!(
        "corpus = \"skim\"\ncommit = \"{commit}\"\n[[lexical]]\nid = \"skim-X01\"\nquery = \"x\"\ncategory = \"short\"\n"
    ))
    .unwrap();
    let plan = plan(&golden).unwrap();
    let stats = StatsSnapshot {
        file_count: 1,
        skipped_by_reason: BTreeMap::new(),
        temporal_state: None,
    };
    assert!(evaluate(&universe, &stats, &plan, &[]).is_err());
}
