//! Unit tests for `gate.rs` (co-located file, `#[path]`-included).

use std::collections::BTreeMap;

use super::*;
use crate::scoreboard::baseline::{Baseline, BaselineCorpus, HardState};
use crate::scoreboard::golden::parse_golden;
use crate::scoreboard::metrics::plan;
use crate::scoreboard::report::{
    CorpusInfo, CoverageReport, FailureKind, GateStatus, SkippedByReason, UniverseReport,
};

// --- ledger ---------------------------------------------------------------------

const LEDGER: &str = r##"
[[xfail]]
issue = "#544"
check = "pagination.has_more_honest"
ids = ["skim-G001", "skim-G002"]
note = "query.rs:676-693 pool_was_capped"

[[xfail]]
issue = "#547"
check = "order.score_monotone"
ids = ["skim-F003"]
"##;

#[test]
fn the_design_ledger_parses_and_answers_by_check_and_id() {
    let l = Ledger::parse(LEDGER).unwrap();
    assert_eq!(l.entries().len(), 2);
    assert_eq!(
        l.issue(CheckId::PaginationHasMoreHonest, "skim-G002"),
        Some("#544")
    );
    assert_eq!(
        l.issue(CheckId::OrderScoreMonotone, "skim-F003"),
        Some("#547")
    );
    assert_eq!(l.issue(CheckId::PaginationComplete, "skim-G001"), None);
    assert_eq!(
        l.entries()[0].note.as_deref(),
        Some("query.rs:676-693 pool_was_capped")
    );
}

#[test]
fn ledger_entries_must_name_a_real_ticket_a_known_check_and_ids() {
    let entry = |issue: &str, check: &str, ids: &str, extra: &str| {
        format!("[[xfail]]\nissue = \"{issue}\"\ncheck = \"{check}\"\nids = {ids}\n{extra}")
    };
    for (raw, why) in [
        (
            entry("#NEW", "lexical.recall", "[\"skim-X01\"]", ""),
            "placeholder issue",
        ),
        (
            entry("544", "lexical.recall", "[\"skim-X01\"]", ""),
            "issue without #",
        ),
        (
            entry("#0", "lexical.recall", "[\"skim-X01\"]", ""),
            "issue #0",
        ),
        (
            entry("#544", "lexical.recal", "[\"skim-X01\"]", ""),
            "unknown check",
        ),
        (entry("#544", "lexical.recall", "[]", ""), "no ids"),
        (entry("#544", "lexical.recall", "[\" \"]", ""), "blank id"),
        (
            entry(
                "#544",
                "lexical.recall",
                "[\"skim-X01\"]",
                "owner = \"x\"\n",
            ),
            "unknown field",
        ),
        (
            format!(
                "{}{}",
                entry("#544", "lexical.recall", "[\"skim-X01\"]", ""),
                entry("#545", "lexical.recall", "[\"skim-X01\"]", "")
            ),
            "duplicate (check, id)",
        ),
    ] {
        assert!(Ledger::parse(&raw).is_err(), "{why}: {raw}");
    }
}

#[test]
fn a_missing_ledger_file_is_an_empty_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let l = Ledger::load(&dir.path().join("known_failures.toml")).unwrap();
    assert!(l.entries().is_empty());
}

#[test]
fn ledger_ids_belong_to_the_longest_matching_corpus_name() {
    let corpora = ["zod", "zod-mini", "skim"];
    assert_eq!(corpus_of("zod-mini-X01", &corpora), Some("zod-mini"));
    assert_eq!(corpus_of("zod-X01", &corpora), Some("zod"));
    assert_eq!(corpus_of("skim-", &corpora), None);
    assert_eq!(corpus_of("flask-X01", &corpora), None);

    let l = Ledger::parse(
        "[[xfail]]\nissue = \"#1\"\ncheck = \"lexical.recall\"\nids = [\"skim-X01\", \"flask-X01\"]\n",
    )
    .unwrap();
    assert_eq!(l.unassigned(&corpora), vec!["flask-X01"]);
    let refs = l.refs_for_corpus("skim", &corpora);
    assert_eq!(
        refs,
        vec![LedgerRef {
            check: CheckId::LexicalRecall,
            id: "skim-X01"
        }]
    );
}

#[test]
fn classification_applies_the_xfail_and_xpass_rules() {
    let fail = CheckOutcome::fail("x");
    assert_eq!(classify(&CheckOutcome::Pass, None), Outcome::Pass);
    assert_eq!(classify(&fail, None), Outcome::Fail);
    assert_eq!(classify(&fail, Some("#544")), Outcome::Xfail);
    assert_eq!(classify(&CheckOutcome::Pass, Some("#544")), Outcome::Xpass);
}

#[test]
fn applying_the_ledger_records_issue_and_detail() {
    let l = Ledger::parse(LEDGER).unwrap();
    let records = apply_ledger(
        &[
            (
                "skim-G001".to_string(),
                CheckId::PaginationHasMoreHonest,
                CheckOutcome::fail("empty page"),
            ),
            (
                "skim-G002".to_string(),
                CheckId::PaginationHasMoreHonest,
                CheckOutcome::Pass,
            ),
            (
                "skim-X01".to_string(),
                CheckId::LexicalRecall,
                CheckOutcome::Pass,
            ),
        ],
        &l,
    );
    assert_eq!(
        records[0],
        CheckRecord {
            id: "skim-G001".to_string(),
            check: CheckId::PaginationHasMoreHonest,
            outcome: Outcome::Xfail,
            issue: Some("#544".to_string()),
            detail: Some("empty page".to_string()),
        }
    );
    assert_eq!(
        (records[1].outcome, records[1].issue.as_deref()),
        (Outcome::Xpass, Some("#544"))
    );
    assert_eq!(
        (records[2].outcome, &records[2].issue, &records[2].detail),
        (Outcome::Pass, &None, &None)
    );
}

#[test]
fn a_ledger_ref_to_a_check_that_never_runs_on_its_entry_is_rejected() {
    let golden = parse_golden(
        "corpus = \"skim\"\ncommit = \"b8a0a79463382347820f1c2572bde37b68e87c76\"\n\
         [[prefix]]\nid = \"skim-F001\"\nquery = \"fn\"\nflags = [\"--hot\"]\nlimits = [5]\n",
    )
    .unwrap();
    let plan = plan(&golden).unwrap();
    let refs = [
        LedgerRef {
            check: CheckId::OrderPrefixConsistent,
            id: "skim-F001",
        },
        LedgerRef {
            check: CheckId::OrderScoreMonotone,
            id: "skim-F001",
        },
    ];
    let v = unplanned_ledger_refs(&refs, &plan);
    assert_eq!(v.len(), 1, "{v:?}");
    assert!(
        v[0].contains("order.score_monotone") && v[0].contains("skim-F001"),
        "{v:?}"
    );
}

// --- gate -------------------------------------------------------------------------

fn record(id: &str, check: CheckId, outcome: Outcome, detail: Option<&str>) -> CheckRecord {
    CheckRecord {
        id: id.to_string(),
        check,
        outcome,
        issue: matches!(outcome, Outcome::Xfail | Outcome::Xpass).then(|| "#544".to_string()),
        detail: detail.map(str::to_string),
    }
}

fn corpus(name: &str, checks: Vec<CheckRecord>, ratchet: &[(&str, f64)]) -> CorpusReport {
    CorpusReport {
        name: name.to_string(),
        commit: "c".repeat(40),
        golden_sha256: format!("g-{name}"),
        universe: UniverseReport {
            oracle: 1,
            skim_file_count: 1,
            delta: 0,
            skipped_by_reason: SkippedByReason {
                oracle: BTreeMap::new(),
                skim: BTreeMap::new(),
            },
        },
        coverage: CoverageReport {
            indexed_tracked: 1,
            tracked_text: 1,
            ratio: 1.0,
        },
        checks,
        ratchet: ratchet.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        info: CorpusInfo {
            oracle_skipped_by_reason: BTreeMap::new(),
            unindexed_hits: BTreeMap::new(),
            idents: Vec::new(),
            concepts: Vec::new(),
        },
    }
}

/// A baseline that blesses exactly `corpora` (pass / xfail records only).
fn baseline_of(corpora: &[CorpusReport], aggregate: &[(&str, f64)]) -> Baseline {
    Baseline {
        schema: 1,
        golden_sha256: "set".to_string(),
        corpora: corpora
            .iter()
            .map(|c| {
                let mut hard: BTreeMap<String, BTreeMap<String, HardState>> = BTreeMap::new();
                for r in &c.checks {
                    if let Some(s) = HardState::from_outcome(r.outcome) {
                        hard.entry(r.id.clone())
                            .or_default()
                            .insert(r.check.as_str().to_string(), s);
                    }
                }
                (
                    c.name.clone(),
                    BaselineCorpus {
                        commit: c.commit.clone(),
                        golden_sha256: c.golden_sha256.clone(),
                        hard,
                        ratchet: c.ratchet.clone(),
                    },
                )
            })
            .collect(),
        aggregate: aggregate.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        accepted_regressions: Vec::new(),
    }
}

fn gate(
    corpora: &[CorpusReport],
    aggregate: &[(&str, f64)],
    complete: bool,
    baseline: Option<&Baseline>,
) -> GateReport {
    let aggregate: BTreeMap<String, f64> =
        aggregate.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    evaluate(&GateInputs {
        corpora,
        aggregate: &aggregate,
        complete,
        baseline,
    })
}

fn pass_record(id: &str) -> CheckRecord {
    record(id, CheckId::LexicalRecall, Outcome::Pass, None)
}

#[test]
fn a_run_equal_to_its_baseline_passes() {
    let c = vec![corpus(
        "skim",
        vec![pass_record("skim-X01")],
        &[("ident.mrr", 0.9)],
    )];
    let b = baseline_of(&c, &[("ident.mrr", 0.9)]);
    let g = gate(&c, &[("ident.mrr", 0.9)], true, Some(&b));
    assert_eq!(g.status, GateStatus::Pass, "{:?}", g.failures);
    assert!(g.failures.is_empty());
}

#[test]
fn no_baseline_means_bless_required() {
    let c = vec![corpus("skim", vec![pass_record("skim-X01")], &[])];
    let g = gate(&c, &[], true, None);
    assert_eq!(g.status, GateStatus::Fail);
    assert_eq!(g.failures.len(), 1);
    assert_eq!(g.failures[0].kind, FailureKind::Baseline);
    assert!(g.failures[0].message.contains("bless"));
}

#[test]
fn unledgered_failures_are_grouped_by_check_with_ids_and_details() {
    let checks = vec![
        record(
            "skim-X02",
            CheckId::LexicalSilentFn,
            Outcome::Fail,
            Some("missing b.rs"),
        ),
        record(
            "skim-X01",
            CheckId::LexicalSilentFn,
            Outcome::Fail,
            Some("missing a.rs"),
        ),
        record(
            "skim-X01",
            CheckId::LexicalRecall,
            Outcome::Xfail,
            Some("missing a.rs"),
        ),
    ];
    let c = vec![corpus("skim", checks, &[])];
    let b = baseline_of(&c, &[]);
    let g = gate(&c, &[], true, Some(&b));
    let f: Vec<&GateFailure> = g
        .failures
        .iter()
        .filter(|f| f.kind == FailureKind::Unledgered)
        .collect();
    assert_eq!(f.len(), 1, "{:?}", g.failures);
    assert_eq!(f[0].check.as_deref(), Some("lexical.silent_fn"));
    assert_eq!(f[0].ids, vec!["skim-X01", "skim-X02"]);
    assert!(
        f[0].message.contains("skim-X01: missing a.rs"),
        "{}",
        f[0].message
    );
    assert!(
        f[0].message.contains("known_failures.toml"),
        "{}",
        f[0].message
    );
    assert!(
        g.failures.iter().all(|f| f.kind == FailureKind::Unledgered),
        "a failing record is not also a baseline diff: {:?}",
        g.failures
    );
}

#[test]
fn an_xpass_asks_for_promotion_naming_the_ticket() {
    let c = vec![corpus(
        "skim",
        vec![record(
            "skim-G001",
            CheckId::PaginationComplete,
            Outcome::Xpass,
            None,
        )],
        &[],
    )];
    let g = gate(&c, &[], true, Some(&baseline_of(&c, &[])));
    let f = &g.failures[0];
    assert_eq!(
        (f.kind, f.ids.clone()),
        (FailureKind::Xpass, vec!["skim-G001".to_string()])
    );
    assert!(
        f.message.contains("XPASS") && f.message.contains("#544") && f.message.contains("promote")
    );
}

#[test]
fn a_ratchet_change_in_either_direction_needs_a_bless() {
    let base = vec![corpus(
        "skim",
        Vec::new(),
        &[("ident.mrr", 0.5), ("bytes.text_median", 1000.0)],
    )];
    let b = baseline_of(&base, &[]);
    let better = vec![corpus(
        "skim",
        Vec::new(),
        &[("ident.mrr", 0.6), ("bytes.text_median", 1029.0)],
    )];
    let g = gate(&better, &[], true, Some(&b));
    assert_eq!(
        g.failures.len(),
        1,
        "bytes within 3% pass: {:?}",
        g.failures
    );
    let f = &g.failures[0];
    assert_eq!(
        (f.kind, f.check.as_deref()),
        (FailureKind::Ratchet, Some("ident.mrr"))
    );
    assert!(
        f.message.contains("improved") && f.message.contains("bless required"),
        "{}",
        f.message
    );
    assert!(f.message.contains("skim"), "{}", f.message);

    let worse = vec![corpus(
        "skim",
        Vec::new(),
        &[("ident.mrr", 0.4), ("bytes.text_median", 1000.0)],
    )];
    let g = gate(&worse, &[], true, Some(&b));
    assert!(
        g.failures[0].message.contains("regressed"),
        "{:?}",
        g.failures
    );
}

#[test]
fn new_and_dropped_metrics_need_a_bless() {
    let b = baseline_of(&[corpus("skim", Vec::new(), &[("ident.mrr", 0.5)])], &[]);
    let g = gate(
        &[corpus("skim", Vec::new(), &[("concept.p10", 0.5)])],
        &[],
        true,
        Some(&b),
    );
    let checks: Vec<&str> = g
        .failures
        .iter()
        .filter_map(|f| f.check.as_deref())
        .collect();
    assert_eq!(checks, vec!["concept.p10", "ident.mrr"], "{:?}", g.failures);
}

#[test]
fn hard_state_changes_against_the_baseline_need_a_bless() {
    let base = vec![corpus("skim", vec![pass_record("skim-X01")], &[])];
    let b = baseline_of(&base, &[]);
    let now = vec![corpus(
        "skim",
        vec![record(
            "skim-X01",
            CheckId::LexicalRecall,
            Outcome::Xfail,
            Some("m"),
        )],
        &[],
    )];
    let g = gate(&now, &[], true, Some(&b));
    assert_eq!(g.failures.len(), 1, "{:?}", g.failures);
    let f = &g.failures[0];
    assert_eq!(f.kind, FailureKind::Baseline);
    assert_eq!(f.check.as_deref(), Some("lexical.recall"));
    assert_eq!(f.ids, vec!["skim-X01"]);
    assert!(f.message.contains("pass -> xfail"), "{}", f.message);
}

#[test]
fn commit_golden_and_corpus_set_changes_need_a_bless() {
    let base = vec![
        corpus("skim", Vec::new(), &[]),
        corpus("zod", Vec::new(), &[]),
    ];
    let b = baseline_of(&base, &[]);
    let mut now = vec![
        corpus("skim", Vec::new(), &[]),
        corpus("flask", Vec::new(), &[]),
    ];
    now[0].commit = "d".repeat(40);
    now[0].golden_sha256 = "changed".to_string();
    let g = gate(&now, &[], true, Some(&b));
    let text: Vec<String> = g.failures.iter().map(|f| f.message.clone()).collect();
    let all = text.join("\n");
    assert!(all.contains("commit"), "{all}");
    assert!(all.contains("golden"), "{all}");
    assert!(
        all.contains("flask") && all.contains("not in the baseline"),
        "{all}"
    );
    assert!(all.contains("zod") && all.contains("not run"), "{all}");
}

#[test]
fn a_partial_run_ignores_missing_corpora_and_aggregates() {
    let base = vec![
        corpus("skim", Vec::new(), &[("ident.mrr", 0.5)]),
        corpus("zod", Vec::new(), &[]),
    ];
    let b = baseline_of(&base, &[("ident.mrr", 0.7)]);
    let now = vec![corpus("skim", Vec::new(), &[("ident.mrr", 0.5)])];
    let g = gate(&now, &[("ident.mrr", 0.5)], false, Some(&b));
    assert_eq!(g.status, GateStatus::Pass, "{:?}", g.failures);
}

#[test]
fn aggregate_changes_are_reported_only_when_no_corpus_reports_them() {
    let base = vec![corpus(
        "skim",
        Vec::new(),
        &[("ident.mrr", 0.5), ("bytes.text_p90", 10.0)],
    )];
    let b = baseline_of(&base, &[("ident.mrr", 0.5), ("bytes.text_p90", 10.0)]);
    let now = vec![corpus(
        "skim",
        Vec::new(),
        &[("ident.mrr", 0.6), ("bytes.text_p90", 10.0)],
    )];
    let g = gate(
        &now,
        &[("ident.mrr", 0.6), ("bytes.text_p90", 20.0)],
        true,
        Some(&b),
    );
    let messages: Vec<&str> = g.failures.iter().map(|f| f.message.as_str()).collect();
    assert_eq!(g.failures.len(), 2, "{messages:?}");
    assert!(
        messages
            .iter()
            .any(|m| m.contains("aggregate") && m.contains("20")),
        "{messages:?}"
    );
}
