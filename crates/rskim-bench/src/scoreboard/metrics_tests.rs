//! Unit tests for `metrics.rs` (co-located file, `#[path]`-included).

use rskim_oracle::structural::OracleLang;

use super::*;
use crate::scoreboard::golden::parse_golden;
use crate::scoreboard::runner::{SweepPage, TextOutput};
use crate::scoreboard::test_support::FixtureRepo;
use crate::scoreboard::types::{Degraded, SnippetLine};
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
        CheckId::LexicalVerifyMode,
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
            "[[lexical]]\nid = \"skim-X01\"\nquery = \"a b\"\nmode = \"near\"\nnear = 3\ncategory = \"near\"\n"
        ),
        with(CheckId::LexicalRecallTracked)
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
            CheckId::ResultsUniquePaths,
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
            CheckId::ResultsUniquePaths,
        ]
    );
}

#[test]
fn standalone_prefix_entries_check_prefix_and_monotonicity_only_where_score_ranks() {
    assert_eq!(
        checks_of(
            "[[prefix]]\nid = \"skim-F001\"\nflags = [\"--ast\", \"god-function\"]\nlimits = [5]\n"
        ),
        vec![
            CheckId::OrderPrefixConsistent,
            CheckId::OrderScoreMonotone,
            CheckId::ResultsUniquePaths
        ]
    );
    assert_eq!(
        checks_of("[[prefix]]\nid = \"skim-F002\"\nflags = [\"--hot\"]\nlimits = [5]\n"),
        vec![CheckId::OrderPrefixConsistent, CheckId::ResultsUniquePaths]
    );
    let g = golden_with("[[prefix]]\nid = \"skim-F002\"\nflags = [\"--hot\"]\nlimits = [5]\n");
    let q = &plan(&g).unwrap()[0];
    assert_eq!((q.arm, q.query.as_deref()), (Arm::HotCold, None));
    assert!(!q.measures_text());
}

fn ast_entry(id: &str, pattern: &str, lang: &str, precision: &str) -> String {
    format!(
        "[[ast]]\nid = \"{id}\"\npattern = \"{pattern}\"\nlang = \"{lang}\"\nprecision = \"{precision}\"\n"
    )
}

#[test]
fn an_ast_entry_runs_the_structural_checks_by_its_precision_class() {
    assert_eq!(
        checks_of(&ast_entry("skim-A1", "try-catch", "typescript", "hard")),
        vec![
            CheckId::ResultsUniquePaths,
            CheckId::StructuralRecall,
            CheckId::StructuralPrecision,
            CheckId::StructuralCoverage,
        ]
    );
    assert_eq!(
        checks_of(&ast_entry("skim-A2", "rust-nested-loop", "rust", "ratchet")),
        vec![
            CheckId::ResultsUniquePaths,
            CheckId::StructuralRecall,
            CheckId::StructuralCoverage,
        ],
        "a ratchet-class entry records structural.precision.<id> instead"
    );
}

#[test]
fn an_ast_entry_plans_a_standalone_ast_call_judged_by_the_structural_oracle() {
    let g = golden_with(&ast_entry("skim-A1", "try-catch", "tsx", "hard"));
    let q = &plan(&g).unwrap()[0];
    assert_eq!((q.kind, q.arm), (EntryKind::Ast, Arm::Ast));
    assert_eq!(q.query, None);
    assert_eq!(q.flags.ast.as_deref(), Some("try-catch"));
    assert!(q.lexical_oracle().is_none());
    assert_eq!(
        q.structural_target(),
        Some(&StructuralTarget {
            pattern: "try-catch".to_string(),
            lang: OracleLang::Tsx,
            precision: PrecisionClass::Hard,
            expect_oracle_empty: false,
        })
    );
    assert!(!q.measures_text());
    // A false-positive guard carries its flag into the target and runs the
    // same checks (precision is what it guards).
    let guard = golden_with(&format!(
        "{}expect_oracle_empty = true\n",
        ast_entry("skim-A2", "try-catch-finally", "javascript", "hard")
    ));
    let q = &plan(&guard).unwrap()[0];
    assert!(q.structural_target().unwrap().expect_oracle_empty);
    assert!(q.runs(CheckId::StructuralPrecision) && q.runs(CheckId::StructuralRecall));
    // No other entry kind gets a structural target.
    let lexical =
        golden_with("[[lexical]]\nid = \"skim-X01\"\nquery = \"a\"\ncategory = \"substr\"\n");
    let q = &plan(&lexical).unwrap()[0];
    assert!(q.structural_target().is_none() && q.lexical_oracle().is_some());
}

#[test]
fn is_structural_names_exactly_the_ast_entries() {
    // Every entry kind, including the two that call `--ast` without being
    // scored by the structural oracle (a standalone `--ast` [[prefix]] and a
    // text + `--ast` [[pagination]]).
    let g = golden_with(&format!(
        "[[ident]]\nid = \"skim-L01\"\nquery = \"x_y\"\ndef = {{ path = \"a.rs\", line = 1 }}\norigin = \"seed\"\n\
         [[concept]]\nid = \"skim-C01\"\nquery = \"a b\"\nrelevant = 'a'\n\
         [[lexical]]\nid = \"skim-X01\"\nquery = \"q\"\ncategory = \"short\"\n\
         [[pagination]]\nid = \"skim-G002\"\nquery = \"x\"\nflags = [\"--ast\", \"try-catch\"]\nlimits = [3]\n\
         [[prefix]]\nid = \"skim-F001\"\nflags = [\"--ast\", \"god-function\"]\nlimits = [5]\n\
         {}",
        ast_entry("skim-A1", "try-catch", "tsx", "hard")
    ));
    let p = plan(&g).unwrap();
    assert_eq!(p.len(), 6);
    for q in &p {
        assert_eq!(q.is_structural(), q.kind == EntryKind::Ast, "{}", q.id);
        assert_eq!(
            q.is_structural(),
            q.structural_target().is_some(),
            "{}",
            q.id
        );
        // The structural checks run exactly on the structural entries, and
        // score monotonicity never on them (their list is path-ordered).
        assert_eq!(
            q.runs(CheckId::StructuralRecall),
            q.is_structural(),
            "{}",
            q.id
        );
        if q.is_structural() {
            assert!(!q.runs(CheckId::OrderScoreMonotone), "{}", q.id);
        }
    }
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
        oracle_less_rows: BTreeMap::new(),
        structural: Vec::new(),
        unscored_rows: BTreeMap::new(),
        refs: Vec::new(),
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
fn row_counts_of_entries_without_an_oracle_ratchet_per_entry() {
    let mut a = samples(Vec::new());
    a.oracle_less_rows = BTreeMap::from([("a-F002".to_string(), 7), ("a-F003".to_string(), 12)]);
    let r = ratchet_values(&[&a]);
    assert_eq!(r["oracle_less.full_rows.a-F002"], 7.0);
    assert_eq!(r["oracle_less.full_rows.a-F003"], 12.0);
    for name in r.keys() {
        assert!(metric_def(name).is_some(), "{name} has no MetricDef");
    }

    // A shrink is a regression (blessing it needs a reason); growth is an
    // improvement; either way the value moved, so the gate asks for a bless.
    let name = "oracle_less.full_rows.a-F003";
    assert_eq!(compare_ratchet(name, 12.0, 11.0), RatchetChange::Regressed);
    assert_eq!(compare_ratchet(name, 12.0, 13.0), RatchetChange::Improved);
    assert_eq!(compare_ratchet(name, 12.0, 12.0), RatchetChange::Unchanged);
    assert!(
        metric_def("oracle_less.full_rows.").is_none(),
        "the metric names an entry"
    );

    // The aggregate keeps each corpus's entries apart (ids are corpus-prefixed).
    let mut b = samples(Vec::new());
    b.oracle_less_rows = BTreeMap::from([("b-F003".to_string(), 4)]);
    let agg = ratchet_values(&[&a, &b]);
    assert_eq!(agg["oracle_less.full_rows.a-F003"], 12.0);
    assert_eq!(agg["oracle_less.full_rows.b-F003"], 4.0);
    assert!(
        !ratchet_values(&[&samples(Vec::new())])
            .keys()
            .any(|k| k.starts_with("oracle_less.")),
        "no entry without an oracle, no row-count metric"
    );
}

fn structural_sample(id: &str, class: PrecisionClass, intent: bool) -> StructuralSample {
    StructuralSample {
        id: id.to_string(),
        pattern: "rust-nested-loop".to_string(),
        lang: OracleLang::Rust,
        precision_class: class,
        expect_oracle_empty: false,
        oracle_files: 4,
        skim_files: 5,
        recall: 1.0,
        precision: 0.8,
        intent_files: intent.then_some(2),
        intent_recall: intent.then_some(0.5),
        intent_precision: intent.then_some(0.2),
        line_on_match: 3,
    }
}

#[test]
fn structural_measurements_ratchet_per_entry_and_unscored_rows_per_pattern() {
    let mut a = samples(Vec::new());
    a.structural = vec![
        structural_sample("a-ast-1", PrecisionClass::Ratchet, true),
        structural_sample("a-ast-2", PrecisionClass::Hard, false),
    ];
    a.unscored_rows = BTreeMap::from([("rust-nested-loop".to_string(), 2)]);
    let r = ratchet_values(&[&a]);
    assert_eq!(r["structural.precision.a-ast-1"], 0.8);
    assert!(
        !r.contains_key("structural.precision.a-ast-2"),
        "a hard-class entry's precision is the HARD check"
    );
    assert_eq!(r["structural.line_on_match.a-ast-1"], 3.0);
    assert_eq!(r["structural.line_on_match.a-ast-2"], 3.0);
    assert_eq!(r["structural.intent_recall.a-ast-1"], 0.5);
    assert_eq!(r["structural.intent_precision.a-ast-1"], 0.2);
    assert!(!r.contains_key("structural.intent_recall.a-ast-2"));
    assert_eq!(r["structural.unscored_rows.rust-nested-loop"], 2.0);
    for name in r.keys() {
        assert!(metric_def(name).is_some(), "{name} has no MetricDef");
    }

    // The aggregate keeps entries apart and sums unscored rows per pattern.
    let mut b = samples(Vec::new());
    b.structural = vec![structural_sample("b-ast-1", PrecisionClass::Ratchet, false)];
    b.unscored_rows = BTreeMap::from([
        ("rust-nested-loop".to_string(), 5),
        ("try-catch".to_string(), 0),
    ]);
    let agg = ratchet_values(&[&a, &b]);
    assert_eq!(agg["structural.unscored_rows.rust-nested-loop"], 7.0);
    assert_eq!(agg["structural.unscored_rows.try-catch"], 0.0);
    assert!(agg.contains_key("structural.precision.a-ast-1"));
    assert!(agg.contains_key("structural.precision.b-ast-1"));
    assert!(
        !ratchet_values(&[&samples(Vec::new())])
            .keys()
            .any(|k| k.starts_with("structural.")),
        "no [[ast]] entry, no structural metric"
    );
}

#[test]
fn structural_ratchets_are_exact_and_unscored_rows_are_neutral() {
    for name in [
        "structural.precision.a-ast-1",
        "structural.line_on_match.a-ast-1",
        "structural.intent_recall.a-ast-1",
        "structural.intent_precision.a-ast-1",
    ] {
        assert_eq!(
            compare_ratchet(name, 0.5, 0.5),
            RatchetChange::Unchanged,
            "{name}"
        );
        assert_eq!(
            compare_ratchet(name, 0.5, 0.4999),
            RatchetChange::Regressed,
            "{name}"
        );
        assert_eq!(
            compare_ratchet(name, 0.5, 0.6),
            RatchetChange::Improved,
            "{name}"
        );
    }
    // No oracle judges unscored rows: a shrink (skim dropped rows, or a new
    // entry scores them) and a growth (rows in a language no entry scores)
    // both read `changed`, so the gate asks for a bless and bless needs no
    // `--accept-regression` reason.
    let unscored = "structural.unscored_rows.try-catch";
    assert_eq!(
        metric_def(unscored).map(|d| d.direction),
        Some(Direction::Neutral)
    );
    assert_eq!(
        compare_ratchet(unscored, 2.0, 2.0),
        RatchetChange::Unchanged
    );
    for current in [0.0, 1.0, 3.0] {
        assert_eq!(
            compare_ratchet(unscored, 2.0, current),
            RatchetChange::Changed,
            "2 -> {current}"
        );
    }
    for bare in [
        "structural.precision.",
        "structural.line_on_match.",
        "structural.unscored_rows.",
        "structural.recall",
    ] {
        assert!(metric_def(bare).is_none(), "{bare} names no entry");
    }
    // No family prefix shadows another or a fixed metric.
    for (i, (a, _)) in STRUCTURAL_FAMILIES.iter().enumerate() {
        for (b, _) in &STRUCTURAL_FAMILIES[i + 1..] {
            assert!(!a.starts_with(b) && !b.starts_with(a), "{a} / {b}");
        }
        assert!(
            RATCHET_METRICS.iter().all(|d| !d.name.starts_with(a)),
            "{a}"
        );
        assert!(!a.starts_with(ORACLE_LESS_ROWS) && !ORACLE_LESS_ROWS.starts_with(a));
    }
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
    let text = text_universe(repo.root(), &GitIsolation::new(repo.home())).unwrap();
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

    let e = evaluate(
        &universe,
        &text,
        &stats,
        &plan,
        &observations,
        &StructuralEvidence::default(),
    )
    .unwrap();

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
fn evaluate_records_the_full_list_size_of_entries_without_an_oracle() {
    let repo = FixtureRepo::new();
    repo.write("a.rs", "x\n");
    let commit = repo.commit_all("init");
    let universe = Universe::compute(repo.root(), &GitIsolation::new(repo.home())).unwrap();
    let text = text_universe(repo.root(), &GitIsolation::new(repo.home())).unwrap();
    let golden = parse_golden(&format!(
        "corpus = \"skim\"\ncommit = \"{commit}\"\n\
         [[lexical]]\nid = \"skim-X01\"\nquery = \"x\"\ncategory = \"short\"\n\
         [[prefix]]\nid = \"skim-F003\"\nflags = [\"--ast\", \"god-function\"]\nlimits = [2]\n"
    ))
    .unwrap();
    let plan = plan(&golden).unwrap();
    let stats = StatsSnapshot {
        file_count: 1,
        skipped_by_reason: BTreeMap::new(),
        temporal_state: None,
    };
    let structural = rows(&["a.rs", "b.rs", "c.rs"]);
    let observations = vec![
        EntryObservation {
            id: "skim-X01".to_string(),
            full: page(rows(&["a.rs"]), false),
            sweeps: Vec::new(),
            limited: Vec::new(),
            text: None,
        },
        EntryObservation {
            id: "skim-F003".to_string(),
            full: page(structural.clone(), false),
            sweeps: Vec::new(),
            limited: vec![(2, page(structural[..2].to_vec(), true))],
            text: None,
        },
    ];

    let e = evaluate(
        &universe,
        &text,
        &stats,
        &plan,
        &observations,
        &StructuralEvidence::default(),
    )
    .unwrap();

    assert_eq!(
        e.samples.oracle_less_rows,
        BTreeMap::from([("skim-F003".to_string(), 3)]),
        "only the entry without an oracle is counted"
    );
}

#[test]
fn evaluate_rejects_observations_that_do_not_follow_the_plan() {
    let repo = FixtureRepo::new();
    repo.write("a.rs", "x\n");
    let commit = repo.commit_all("init");
    let universe = Universe::compute(repo.root(), &GitIsolation::new(repo.home())).unwrap();
    let text = text_universe(repo.root(), &GitIsolation::new(repo.home())).unwrap();
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
    assert!(
        evaluate(
            &universe,
            &text,
            &stats,
            &plan,
            &[],
            &StructuralEvidence::default()
        )
        .is_err()
    );
}

// --- evaluate: [[ast]] entries --------------------------------------------------------

#[test]
fn evaluate_scores_ast_entries_against_the_structural_oracle() {
    use crate::scoreboard::structural_metrics::OracleAnswers;
    use crate::scoreboard::test_support::oracle;
    use crate::scoreboard::types::{AstCoverage, AstPage};

    let repo = FixtureRepo::new();
    repo.write(
        "src/nested.rs",
        "fn walk() {\n    for a in 0..2 {\n        for b in 0..2 {}\n    }\n}\n",
    );
    repo.write("src/plain.rs", "fn f() {}\n");
    repo.write("web/a.ts", "try {\n  go();\n} catch (e) {}\n");
    repo.write("lib/C.java", "class C { void m() {} }\n");
    let commit = repo.commit_all("init");
    let universe = Universe::compute(repo.root(), &GitIsolation::new(repo.home())).unwrap();
    let text = text_universe(repo.root(), &GitIsolation::new(repo.home())).unwrap();
    let golden = parse_golden(&format!(
        "corpus = \"skim\"\ncommit = \"{commit}\"\n{}{}",
        ast_entry(
            "skim-ast-rust-nested-loop-rust",
            "rust-nested-loop",
            "rust",
            "ratchet"
        ),
        ast_entry(
            "skim-ast-try-catch-typescript",
            "try-catch",
            "typescript",
            "hard"
        ),
    ))
    .unwrap();
    let plan = plan(&golden).unwrap();
    let stats = StatsSnapshot {
        file_count: 4,
        skipped_by_reason: BTreeMap::new(),
        temporal_state: None,
    };
    let ast_page = |rows: Vec<ResultRow>, excluded: u64| AstPage {
        page: page(rows, false),
        coverage: AstCoverage {
            size_excluded_files: excluded,
            ..AstCoverage::default()
        },
    };
    let mut nested = row("src/nested.rs", 1.0);
    nested.line = Some(3);
    let patterns = BTreeMap::from([
        (
            "rust-nested-loop".to_string(),
            // plain.rs is a false positive; C.java is unscored.
            ast_page(
                vec![nested, row("src/plain.rs", 1.0), row("lib/C.java", 1.0)],
                0,
            ),
        ),
        // skim misses a.ts; its coverage disagrees with the oracle (0).
        ("try-catch".to_string(), ast_page(Vec::new(), 1)),
    ]);
    let evidence = StructuralEvidence {
        answers: OracleAnswers::compute(oracle(), universe.files()).unwrap(),
        patterns,
    };
    let observations: Vec<EntryObservation> = plan
        .iter()
        .map(|q| {
            let t = q.structural_target().unwrap();
            EntryObservation::for_ast(&q.id, &evidence.patterns[&t.pattern], t.lang)
        })
        .collect();

    let e = evaluate(&universe, &text, &stats, &plan, &observations, &evidence).unwrap();

    let outcome = |id: &str, check: CheckId| {
        e.outcomes
            .iter()
            .find(|(i, c, _)| i == id && *c == check)
            .map(|(_, _, o)| o.clone())
    };
    let loops = "skim-ast-rust-nested-loop-rust";
    let tc = "skim-ast-try-catch-typescript";
    assert!(outcome(loops, CheckId::StructuralRecall).unwrap().is_pass());
    assert!(
        outcome(loops, CheckId::StructuralCoverage)
            .unwrap()
            .is_pass()
    );
    assert_eq!(
        outcome(loops, CheckId::StructuralPrecision),
        None,
        "ratchet class"
    );
    assert!(
        outcome(loops, CheckId::ResultsUniquePaths)
            .unwrap()
            .is_pass()
    );
    assert!(detail(&outcome(tc, CheckId::StructuralRecall).unwrap()).contains("web/a.ts"));
    assert!(outcome(tc, CheckId::StructuralPrecision).unwrap().is_pass());
    assert!(is_fail(&outcome(tc, CheckId::StructuralCoverage).unwrap()));

    let s = &e.samples.structural;
    assert_eq!(s.len(), 2);
    assert_eq!((s[0].oracle_files, s[0].skim_files), (1, 2));
    assert_eq!((s[0].precision, s[0].line_on_match), (0.5, 1));
    assert_eq!(s[0].intent_recall, Some(1.0));
    assert_eq!((s[1].recall, s[1].intent_recall), (0.0, None));
    assert_eq!(
        e.samples.unscored_rows,
        BTreeMap::from([
            ("rust-nested-loop".to_string(), 1),
            ("try-catch".to_string(), 0)
        ])
    );
    assert!(
        e.samples.oracle_less_rows.is_empty(),
        "[[ast]] entries have an oracle"
    );
    assert!(e.unindexed_hits.is_empty());

    // Without the pattern's call, scoring an [[ast]] entry is an error.
    assert!(
        evaluate(
            &universe,
            &text,
            &stats,
            &plan,
            &observations,
            &StructuralEvidence::default()
        )
        .is_err()
    );
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

    // A missing line: the file is still found, the count is short.
    let short = stakes_page(&lines[..3], &[("LICENSE", 1), ("src/lib.rs", 2)]);
    let d = detail(&check_stakes_lines(&short, &truth));
    assert!(
        d.contains("1 matching line(s) missing") && d.contains("src/main.rs:1"),
        "{d}"
    );
    assert!(
        is_fail(&check_stakes_files(&short, &truth)),
        "main.rs is gone"
    );

    // An extra line in a known file: files agree, lines and counts differ.
    let mut more = lines.to_vec();
    more.push(("src/lib.rs", 3));
    let extra = stakes_page(
        &more,
        &[("LICENSE", 1), ("src/lib.rs", 3), ("src/main.rs", 1)],
    );
    assert!(
        detail(&check_stakes_lines(&extra, &truth)).contains("src/lib.rs:3 not in the oracle")
            || detail(&check_stakes_lines(&extra, &truth))
                .contains("not in the oracle: src/lib.rs:3")
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
    assert_eq!((q.kind, q.arm), (EntryKind::Refs, Arm::Lexical));
    assert_eq!(q.query.as_deref(), Some("launch"));
    assert!(q.refs_entry().is_some() && q.oracle.is_some());
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
    let stakes = q.stakes_oracle().unwrap();
    assert!(stakes.query.matches_line("fn launch() {}"), "-i");
    assert!(!stakes.query.matches_line("fn relaunch() {}"), "-w");
    // A flag the oracle cannot judge stops the plan instead of being ignored.
    let filtered = golden_with(
        "[[stakes]]\nid = \"skim-S03\"\nquery = \"x\"\nflags = [\"-t\", \"rust\"]\nexpect = \"lines\"\n",
    );
    assert!(plan(&filtered).is_err());
}

#[test]
fn only_a_lexical_table_entry_gains_the_tracked_recall_check() {
    // A paginated query has a lexical oracle but is not a `[[lexical]]` entry.
    let paginated = checks_of(
        "[[pagination]]\nid = \"skim-G001\"\nquery = \"build lock\"\nflags = []\nlimits = [3]\n",
    );
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
         [[pagination]]\nid = \"skim-G001\"\nquery = \"x\"\nflags = []\nlimits = [3]\n\
         [[prefix]]\nid = \"skim-F001\"\nflags = [\"--hot\"]\nlimits = [5]\n\
         {}{REFS_TOML}{CLASH_TOML}{}",
        ast_entry("skim-A1", "try-catch", "tsx", "hard"),
        stakes_toml("exit", Some(1)),
    );
    let planned = plan(&golden_with(&body)).unwrap();
    let kinds: BTreeSet<EntryKind> = planned.iter().map(|q| q.kind).collect();
    assert_eq!(kinds.len(), 9, "one entry of every kind: {kinds:?}");
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
    let universe = Universe::compute(repo.root(), &git).unwrap();
    let text = text_universe(repo.root(), &git).unwrap();
    assert_eq!(
        (universe.len(), text.len()),
        (4, 5),
        "LICENSE is only in the text universe"
    );

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
    let stats = StatsSnapshot {
        file_count: 4,
        skipped_by_reason: BTreeMap::new(),
        temporal_state: None,
    };

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
    let run = |tracked: bool| {
        evaluate(
            &universe,
            &text,
            &stats,
            &plan,
            &observe(tracked),
            &StructuralEvidence::default(),
        )
        .unwrap()
    };

    let e = run(false);
    let outcome = |e: &CorpusEvaluation, id: &str, check: CheckId| {
        e.outcomes
            .iter()
            .find(|(i, c, _)| i == id && *c == check)
            .map(|(_, _, o)| o.clone())
            .expect("the check ran")
    };
    // The indexed universe has no LICENSE, so plain recall passes while the
    // text-universe recall sees the miss.
    assert!(outcome(&e, "skim-X01", CheckId::LexicalRecall).is_pass());
    let d = detail(&outcome(&e, "skim-X01", CheckId::LexicalRecallTracked));
    assert!(d.contains("LICENSE"), "{d}");
    assert!(outcome(&run(true), "skim-X01", CheckId::LexicalRecallTracked).is_pass());

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

    // The samples the new rows read; a frozen entry is not oracle-less.
    assert_eq!(e.samples.refs.len(), 1);
    assert_eq!(e.samples.refs[0].call_precision, Some(1.0));
    assert_eq!(e.samples.costs, vec![cost(2, 50, 100)]);
    assert!(e.samples.oracle_less_rows.is_empty());
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
