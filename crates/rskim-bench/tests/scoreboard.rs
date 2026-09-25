//! Offline integration tests for the `scoreboard` binary (#203 AC 2, 3, 5).
//!
//! Every test drives the real `scoreboard` binary (`CARGO_BIN_EXE_scoreboard`)
//! against:
//!
//! - a tempdir git fixture as the corpus, cloned into `<corpus-dir>/fixture`
//!   so the production corpus source verifies and reuses it with no network
//!   access;
//! - a data dir holding `corpora.toml`, `golden/fixture.toml`, and (per
//!   test) `known_failures.toml` / `baseline.json`;
//! - MOCK: a STUB `skim`, a POSIX sh script that answers each invocation with
//!   a canned JSON or text response chosen from its argv (query, flags,
//!   `--limit`, `--offset`) and logs every call. It never touches
//!   `target/*/skim` (PF-019).
//!
//! The canned "correct" answers are derived from the oracle's own ground
//! truth over the fixture, then each test breaks exactly the behaviour it is
//! about.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code: fail loudly

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

use rskim_bench::scoreboard::MAX_PAGES;
use rskim_bench::scoreboard::golden::{IntegrityContext, Origin, check_integrity, parse_golden};
use rskim_bench::scoreboard::oracle::{LexicalQuery, MatchMode, ground_truth};
use rskim_bench::scoreboard::test_support::FixtureRepo;
use rskim_bench::scoreboard::universe::{GitIsolation, Universe};
use serde_json::{Value, json};

// ============================================================================
// Fixture corpus and golden set
// ============================================================================

const FILES: &[(&str, &str)] = &[
    (
        "src/staleness.rs",
        "// staleness checks\npub fn check_staleness() -> bool {\n    true\n}\n",
    ),
    (
        "src/other.rs",
        "fn refresh() {\n    if check_staleness() {}\n}\n",
    ),
    (
        "src/lock.rs",
        "/// Holds the build lock.\npub struct BuildLock;\n\nfn acquire() {}\n",
    ),
    ("src/marker.rs", "// elision marker\nfn marker() {}\n"),
    (
        "README.md",
        "# Fixture\n\nThe build lock and the elision marker.\n",
    ),
];

const DEF_PATH: &str = "src/staleness.rs";
const DEF_LINE: u32 = 2;

fn golden_toml(commit: &str, def_line: u32) -> String {
    format!(
        r#"corpus = "fixture"
commit = "{commit}"

[[ident]]
id = "fixture-L01"
query = "check_staleness"
def = {{ path = "{DEF_PATH}", line = {def_line} }}
origin = "seed"

[[concept]]
id = "fixture-C01"
query = "build lock"
relevant = '(?i)build[_\s-]*lock'

[[lexical]]
id = "fixture-X01"
query = "marker"
category = "substr"

[[pagination]]
id = "fixture-G001"
query = "fn"
limits = [2]

[[prefix]]
id = "fixture-F001"
query = "fn"
flags = ["--hot"]
limits = [1]

[[prefix]]
id = "fixture-F002"
flags = ["--ast", "god-function"]
limits = [1]
"#
    )
}

/// The golden entries the stub answers: (id, query, flags).
const IDENT: (&str, &str, &[&str]) = ("fixture-L01", "check_staleness", &[]);
const CONCEPT: (&str, &str, &[&str]) = ("fixture-C01", "build lock", &[]);
const LEXICAL: (&str, &str, &[&str]) = ("fixture-X01", "marker", &[]);
const PAGINATION: (&str, &str, &[&str]) = ("fixture-G001", "fn", &[]);
const PREFIX: (&str, &str, &[&str]) = ("fixture-F001", "fn", &["--hot"]);
/// Standalone `--ast`: no query (the stub keys it on the empty string) and
/// no oracle.
const AST: (&str, &str, &[&str]) = ("fixture-F002", "", &["--ast", "god-function"]);
const PAGE_LIMIT: u32 = 2;
const PREFIX_LIMIT: u32 = 1;
const FULL_LIMIT: u32 = 1_000_000;

// ============================================================================
// Stub skim
// ============================================================================

/// MOCK: the stub `skim`. Reads `--limit` / `--offset` / `--json` / `--`
/// from argv, keys the response on hex(query + "\n" + " flag" per flag
/// token), and serves `q_<key>/l<limit>_o<offset>.json`, falling back to
/// `q_<key>/l<limit>_default.json` (JSON) or `q_<key>/text.txt` (text mode).
const STUB: &str = r#"#!/bin/sh
set -u
mode=query
json=0
limit=20
offset=0
query=
flags=
if [ "${1:-}" = search ]; then shift; fi
while [ $# -gt 0 ]; do
  case "$1" in
    --build) mode=build ;;
    --stats) mode=stats ;;
    --json) json=1 ;;
    --root) shift ;;
    --limit) shift; limit=$1 ;;
    --offset) shift; offset=$1 ;;
    --) shift; query=$*; break ;;
    *) flags="$flags $1" ;;
  esac
  shift
done
case $mode in
  build) printf 'build\n' >> "$STUB_DIR/calls.log"; exit 0 ;;
  stats) cat "$STUB_DIR/stats.json"; exit 0 ;;
esac
key=$(printf '%s\n%s' "$query" "$flags" | od -An -v -tx1 | tr -d ' \n')
printf '%s json=%s limit=%s offset=%s\n' "$key" "$json" "$limit" "$offset" >> "$STUB_DIR/calls.log"
dir="$STUB_DIR/q_$key"
if [ "$json" = 0 ]; then
  f="$dir/text.txt"
elif [ -f "$dir/l${limit}_o${offset}.json" ]; then
  f="$dir/l${limit}_o${offset}.json"
else
  f="$dir/l${limit}_default.json"
fi
if [ -f "$f" ]; then
  cat "$f"
  exit 0
fi
printf 'stub skim: no canned response for query=%s flags=%s limit=%s offset=%s json=%s\n' "$query" "$flags" "$limit" "$offset" "$json" >&2
exit 97
"#;

/// The stub's response key for a query and its flag tokens.
fn key(query: &str, flags: &[&str]) -> String {
    let flags: String = flags.iter().map(|f| format!(" {f}")).collect();
    format!("{query}\n{flags}")
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One canned result row.
#[derive(Debug, Clone)]
struct Row {
    path: String,
    line: u32,
    content: String,
}

fn row_json(row: &Row, score: f64) -> Value {
    json!({
        "path": row.path,
        "score": score,
        "field": "text",
        "line_number": row.line,
        "snippet": {"lines": [{"line_number": row.line, "content": row.content, "is_match": true}]},
        "stale": false,
    })
}

/// A lexical-envelope page: `rows` are ranks `offset..offset + rows.len()`
/// of a list of `total_len` rows, scored `total_len - rank` (strictly
/// descending).
fn page_json(query: &str, rows: &[Row], offset: usize, total_len: usize, has_more: bool) -> String {
    let results: Vec<Value> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| row_json(r, (total_len - (offset + i)) as f64))
        .collect();
    json!({
        "query": query,
        "total": rows.len(),
        "duration_ms": 1,
        "has_more": has_more,
        "results": results,
    })
    .to_string()
}

/// A standalone `--ast` page (`crates/rskim-search/src/compound/output.rs`):
/// rows carry `score`, `line` and a one-line `snippet` string.
fn ast_page_json(rows: &[(Row, f64)], has_more: bool) -> String {
    let results: Vec<Value> = rows
        .iter()
        .map(|(r, score)| {
            json!({
                "path": r.path,
                "score": score,
                "line": r.line,
                "snippet": r.content,
            })
        })
        .collect();
    json!({"total": rows.len(), "has_more": has_more, "results": results}).to_string()
}

/// skim's text format (`query.rs::format_text_output`) for the first 20 rows.
fn text_output(query: &str, rows: &[Row]) -> String {
    if rows.is_empty() {
        return format!("no results for {query:?}\n");
    }
    let shown: Vec<&Row> = rows.iter().take(20).collect();
    let mut out = String::new();
    for (i, r) in shown.iter().enumerate() {
        let score = (rows.len() - i) as f64;
        out.push_str(&format!(
            "{}:{}  [text]  score: {score:.2}\n  >  {:>4}│ {}\n\n",
            r.path, r.line, r.line, r.content
        ));
    }
    out.push_str(&format!("{} result(s) for {query:?} in 7ms\n", shown.len()));
    out
}

// ============================================================================
// Harness
// ============================================================================

struct Harness {
    /// Keeps the source repository (the clone's origin) alive.
    _source: FixtureRepo,
    corpus_dir: tempfile::TempDir,
    data_dir: tempfile::TempDir,
    stub_dir: tempfile::TempDir,
    out_dir: tempfile::TempDir,
    commit: String,
    universe: Universe,
}

impl Harness {
    /// A fixture corpus, its golden set, and a stub that answers every
    /// golden query correctly. No ledger and no baseline.
    fn new() -> Self {
        let source = FixtureRepo::new();
        for (path, contents) in FILES {
            source.write(path, contents);
        }
        let commit = source.commit_all("fixture corpus");

        let corpus_dir = tempfile::tempdir().unwrap();
        let root = corpus_dir.path().join("fixture");
        source.git(&[
            "clone",
            "--quiet",
            source.root().to_str().unwrap(),
            root.to_str().unwrap(),
        ]);
        let universe = Universe::compute(&root, &GitIsolation::new(source.home())).unwrap();

        let h = Harness {
            _source: source,
            corpus_dir,
            data_dir: tempfile::tempdir().unwrap(),
            stub_dir: tempfile::tempdir().unwrap(),
            out_dir: tempfile::tempdir().unwrap(),
            commit,
            universe,
        };
        fs::write(
            h.data_dir.path().join("corpora.toml"),
            format!(
                "[[repos]]\nurl = \"https://example.invalid/scoreboard/fixture\"\ncommit = \"{}\"\nlanguage = \"Rust\"\n",
                h.commit
            ),
        )
        .unwrap();
        h.write_golden(DEF_LINE);
        h.install_stub();
        h.write_correct_responses();
        h
    }

    fn write_golden(&self, def_line: u32) {
        let dir = self.data_dir.path().join("golden");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("fixture.toml"),
            golden_toml(&self.commit, def_line),
        )
        .unwrap();
    }

    fn write_ledger(&self, body: &str) {
        fs::write(self.data_dir.path().join("known_failures.toml"), body).unwrap();
    }

    fn baseline_path(&self) -> PathBuf {
        self.data_dir.path().join("baseline.json")
    }

    fn report_path(&self) -> PathBuf {
        self.out_dir.path().join("report.json")
    }

    fn stub_path(&self) -> PathBuf {
        self.stub_dir.path().join("skim")
    }

    fn install_stub(&self) {
        let script = STUB.replacen(
            "set -u\n",
            &format!("set -u\nSTUB_DIR='{}'\n", self.stub_dir.path().display()),
            1,
        );
        let path = self.stub_path();
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        self.write_stats(Some("ready"));
    }

    /// The stub's `--stats --json` answer: the fixture universe, plus
    /// `temporal_state` (`None` leaves the key out).
    fn write_stats(&self, temporal_state: Option<&str>) {
        let mut stats = json!({
            "file_count": self.universe.len(),
            "skipped_by_reason": self.universe.persisted_skipped_by_reason(),
        });
        if let Some(state) = temporal_state {
            stats["temporal_state"] = json!(state);
        }
        fs::write(self.stub_dir.path().join("stats.json"), stats.to_string()).unwrap();
    }

    // --- canned responses ---------------------------------------------------

    /// The oracle's ground truth for an `and` query, as rows anchored on the
    /// first line holding the query's first token. `first` moves one path to
    /// rank 1.
    fn correct_rows(&self, query: &str, first: Option<&str>) -> Vec<Row> {
        let q = LexicalQuery::new(query, MatchMode::And, None).unwrap();
        let mut paths = ground_truth(self.universe.files(), &q);
        if let Some(first) = first {
            let at = paths.iter().position(|p| p == first).unwrap();
            let p = paths.remove(at);
            paths.insert(0, p);
        }
        let token = query.split_whitespace().next().unwrap();
        paths
            .into_iter()
            .map(|path| {
                let text = self.universe.text(&path).unwrap();
                let (idx, line) = text
                    .split('\n')
                    .enumerate()
                    .find(|(_, l)| l.contains(token))
                    .unwrap_or((0, ""));
                Row {
                    path,
                    line: u32::try_from(idx + 1).unwrap(),
                    content: line.to_string(),
                }
            })
            .collect()
    }

    fn response_dir(&self, query: &str, flags: &[&str]) -> PathBuf {
        let dir = self
            .stub_dir
            .path()
            .join(format!("q_{}", key(query, flags)));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_response(&self, query: &str, flags: &[&str], file: &str, body: &str) {
        fs::write(self.response_dir(query, flags).join(file), body).unwrap();
    }

    fn write_full(&self, query: &str, flags: &[&str], rows: &[Row]) {
        let body = page_json(query, rows, 0, rows.len(), false);
        self.write_response(query, flags, &format!("l{FULL_LIMIT}_o0.json"), &body);
    }

    /// Honest pages `--offset 0, limit, 2·limit, …` of `rows`.
    fn write_pages(&self, query: &str, flags: &[&str], rows: &[Row], limit: u32) {
        let limit = limit as usize;
        let mut offset = 0;
        loop {
            let end = (offset + limit).min(rows.len());
            let body = page_json(
                query,
                &rows[offset..end],
                offset,
                rows.len(),
                end < rows.len(),
            );
            self.write_response(query, flags, &format!("l{limit}_o{offset}.json"), &body);
            offset += limit;
            if offset >= rows.len() {
                break;
            }
        }
    }

    fn write_limited(&self, query: &str, flags: &[&str], rows: &[Row], limit: u32) {
        let n = (limit as usize).min(rows.len());
        let body = page_json(query, &rows[..n], 0, rows.len(), n < rows.len());
        self.write_response(query, flags, &format!("l{limit}_o0.json"), &body);
    }

    fn write_text(&self, query: &str, flags: &[&str], rows: &[Row]) {
        self.write_response(query, flags, "text.txt", &text_output(query, rows));
    }

    /// Rows for the standalone `--ast` entry, scored `scores` in rank order
    /// (at most two rows). With no oracle, any structural match set will do.
    fn ast_rows(&self, scores: &[f64]) -> Vec<(Row, f64)> {
        let rows = self.correct_rows(IDENT.1, Some(DEF_PATH));
        assert!(
            scores.len() <= rows.len(),
            "the fixture has {} rows",
            rows.len()
        );
        rows.into_iter().zip(scores.iter().copied()).collect()
    }

    /// Serve the standalone `--ast` entry: `scored` as its full list, and the
    /// full list's first rows as its `--limit` page.
    fn write_ast(&self, scored: &[(Row, f64)]) {
        let (_, q, f) = AST;
        self.write_response(
            q,
            f,
            &format!("l{FULL_LIMIT}_o0.json"),
            &ast_page_json(scored, false),
        );
        let n = (PREFIX_LIMIT as usize).min(scored.len());
        self.write_response(
            q,
            f,
            &format!("l{PREFIX_LIMIT}_o0.json"),
            &ast_page_json(&scored[..n], n < scored.len()),
        );
    }

    fn write_ident(&self, rows: &[Row]) {
        let (_, q, f) = IDENT;
        self.write_full(q, f, rows);
        self.write_text(q, f, rows);
    }

    fn write_correct_responses(&self) {
        self.write_ident(&self.correct_rows(IDENT.1, Some(DEF_PATH)));

        let (_, q, f) = CONCEPT;
        let rows = self.correct_rows(q, None);
        self.write_full(q, f, &rows);
        self.write_text(q, f, &rows);

        let (_, q, f) = LEXICAL;
        self.write_full(q, f, &self.correct_rows(q, None));

        let (_, q, f) = PAGINATION;
        let rows = self.correct_rows(q, None);
        self.write_full(q, f, &rows);
        self.write_pages(q, f, &rows, PAGE_LIMIT);

        let (_, q, f) = PREFIX;
        let rows = self.correct_rows(q, None);
        self.write_full(q, f, &rows);
        self.write_limited(q, f, &rows, PREFIX_LIMIT);

        self.write_ast(&self.ast_rows(&[2.0, 1.0]));
    }

    /// Serve the lexical entry's full list without one ground-truth file
    /// (and an empty `degraded[]`): a silent false negative.
    fn drop_lexical_ground_truth_file(&self) -> String {
        let (_, q, f) = LEXICAL;
        let mut rows = self.correct_rows(q, None);
        assert!(rows.len() >= 2, "the fixture needs >= 2 ground-truth files");
        let dropped = rows.remove(0).path;
        self.write_full(q, f, &rows);
        dropped
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.stub_dir.path().join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    // --- running the binary ---------------------------------------------------

    fn scoreboard(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_scoreboard"))
            .args(args)
            .env("NO_COLOR", "1")
            .output()
            .expect("spawning the scoreboard binary")
    }

    fn engine(&self, subcommand: &str) -> Output {
        let stub = self.stub_path();
        self.scoreboard(&[
            subcommand,
            "--skim-bin",
            stub.to_str().unwrap(),
            "--corpus-dir",
            self.corpus_dir.path().to_str().unwrap(),
            "--data-dir",
            self.data_dir.path().to_str().unwrap(),
            "--out",
            self.out_dir.path().to_str().unwrap(),
        ])
    }

    fn run(&self) -> Output {
        self.engine("run")
    }

    fn check(&self) -> Output {
        self.engine("check")
    }

    fn bless(&self, accept_regression: Option<&str>) -> Output {
        let report = self.report_path();
        let mut args = vec![
            "bless",
            "--from",
            report.to_str().unwrap(),
            "--data-dir",
            self.data_dir.path().to_str().unwrap(),
        ];
        if let Some(reason) = accept_regression {
            args.extend(["--accept-regression", reason]);
        }
        self.scoreboard(&args)
    }

    fn golden_gen(&self, corpus: &str) -> Output {
        self.scoreboard(&[
            "golden-gen",
            "--corpus",
            corpus,
            "--corpus-dir",
            self.corpus_dir.path().to_str().unwrap(),
            "--data-dir",
            self.data_dir.path().to_str().unwrap(),
        ])
    }

    fn report(&self) -> Value {
        serde_json::from_slice(&fs::read(self.report_path()).unwrap()).unwrap()
    }

    /// `run` then `bless`, both asserted to succeed: a baseline matching the
    /// stub's current answers.
    fn bless_current(&self) {
        let run = self.run();
        assert_exit(&run, 0);
        let bless = self.bless(None);
        assert_exit(&bless, 0);
        assert!(
            self.baseline_path().exists(),
            "bless wrote no baseline.json"
        );
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn assert_exit(out: &Output, code: i32) {
    assert_eq!(
        out.status.code(),
        Some(code),
        "expected exit {code}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        stderr(out)
    );
}

/// The report's gate failures as `(kind, check, ids, message)`.
fn gate_failures(report: &Value) -> Vec<(String, String, Vec<String>, String)> {
    report["gate"]["failures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["kind"].as_str().unwrap_or_default().to_string(),
                f["check"].as_str().unwrap_or_default().to_string(),
                f["ids"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|i| i.as_str().unwrap().to_string())
                    .collect(),
                f["message"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// The report's outcome for `(id, check)`.
fn outcome(report: &Value, id: &str, check: &str) -> Option<String> {
    report["corpora"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|c| c["checks"].as_array().unwrap().iter())
        .find(|c| c["id"] == id && c["check"] == check)
        .map(|c| c["outcome"].as_str().unwrap().to_string())
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn a_correct_skim_passes_run_bless_and_check() {
    let h = Harness::new();

    let run = h.run();
    assert_exit(&run, 0);
    assert!(h.out_dir.path().join("report.md").exists());
    let report = h.report();
    for check in [
        "lexical.recall",
        "lexical.precision",
        "lexical.silent_fn",
        "lexical.verify_mode",
        "order.score_monotone",
    ] {
        assert_eq!(
            outcome(&report, "fixture-X01", check).as_deref(),
            Some("pass"),
            "{check}"
        );
    }
    assert_eq!(
        outcome(&report, "fixture-G001", "pagination.complete").as_deref(),
        Some("pass")
    );
    assert_eq!(
        outcome(&report, "fixture-F001", "order.prefix_consistent").as_deref(),
        Some("pass")
    );
    // `--hot` reorders the list: exempt from score monotonicity.
    assert_eq!(
        outcome(&report, "fixture-F001", "order.score_monotone"),
        None
    );
    assert_eq!(report["corpora"][0]["universe"]["delta"], 0);

    let bless = h.bless(None);
    assert_exit(&bless, 0);
    let check = h.check();
    assert_exit(&check, 0);
    assert_eq!(h.report()["gate"]["status"], "pass");
}

#[test]
fn a_dropped_ground_truth_file_is_a_silent_false_negative_failure() {
    let h = Harness::new();
    h.bless_current();

    let dropped = h.drop_lexical_ground_truth_file();
    let check = h.check();

    assert_exit(&check, 1);
    let err = stderr(&check);
    assert!(err.contains("lexical.silent_fn"), "{err}");
    assert!(err.contains("fixture-X01"), "{err}");
    let report = h.report();
    assert_eq!(
        outcome(&report, "fixture-X01", "lexical.silent_fn").as_deref(),
        Some("fail")
    );
    let failures = gate_failures(&report);
    assert!(
        failures
            .iter()
            .any(|(kind, check, ids, message)| kind == "unledgered"
                && check == "lexical.silent_fn"
                && ids == &["fixture-X01".to_string()]
                && message.contains(&dropped)),
        "{failures:#?}"
    );
}

#[test]
fn an_empty_page_claiming_has_more_fails_honesty_and_the_sweep_stops_at_max_pages() {
    let h = Harness::new();
    h.bless_current();

    // The last real page lies (`has_more: true`), and every later offset is
    // an empty page that also claims more.
    let (_, q, f) = PAGINATION;
    let rows = h.correct_rows(q, None);
    assert!(rows.len() > PAGE_LIMIT as usize);
    let last = ((rows.len() - 1) / PAGE_LIMIT as usize) * PAGE_LIMIT as usize;
    h.write_response(
        q,
        f,
        &format!("l{PAGE_LIMIT}_o{last}.json"),
        &page_json(q, &rows[last..], last, rows.len(), true),
    );
    h.write_response(
        q,
        f,
        &format!("l{PAGE_LIMIT}_default.json"),
        r#"{"total":0,"has_more":true,"results":[]}"#,
    );
    let before = h.calls().len();

    let check = h.check();

    assert_exit(&check, 1);
    let err = stderr(&check);
    assert!(err.contains("pagination.has_more_honest"), "{err}");
    assert!(err.contains("fixture-G001"), "{err}");
    let report = h.report();
    assert_eq!(
        outcome(&report, "fixture-G001", "pagination.has_more_honest").as_deref(),
        Some("fail")
    );
    // Every row was still shown, in order, once.
    for check in [
        "pagination.complete",
        "pagination.disjoint",
        "pagination.ordered",
    ] {
        assert_eq!(
            outcome(&report, "fixture-G001", check).as_deref(),
            Some("pass"),
            "{check}"
        );
    }
    let sweep_calls = h.calls()[before..]
        .iter()
        .filter(|c| c.starts_with(&key(q, f)) && c.contains(&format!("json=1 limit={PAGE_LIMIT} ")))
        .count();
    assert_eq!(
        sweep_calls, MAX_PAGES as usize,
        "the sweep must stop at MAX_PAGES"
    );
}

#[test]
fn a_ledgered_check_that_passes_is_an_xpass_failure_asking_for_promotion() {
    let h = Harness::new();
    h.bless_current();
    h.write_ledger(
        r##"[[xfail]]
issue = "#9001"
check = "lexical.silent_fn"
ids = ["fixture-X01"]
note = "fixture"
"##,
    );

    let check = h.check();

    assert_exit(&check, 1);
    let err = stderr(&check);
    assert!(err.contains("XPASS"), "{err}");
    assert!(err.contains("promote"), "{err}");
    let report = h.report();
    assert_eq!(
        outcome(&report, "fixture-X01", "lexical.silent_fn").as_deref(),
        Some("xpass")
    );
    assert!(
        gate_failures(&report)
            .iter()
            .any(|(kind, check, ids, message)| kind == "xpass"
                && check == "lexical.silent_fn"
                && ids == &["fixture-X01".to_string()]
                && message.contains("#9001")
                && message.contains("promote")),
        "{:#?}",
        gate_failures(&report)
    );
}

#[test]
fn a_ratchet_improvement_fails_until_blessed() {
    let h = Harness::new();
    // Baseline: the defining file ranks second.
    h.write_ident(&h.correct_rows(IDENT.1, Some("src/other.rs")));
    h.bless_current();
    assert_eq!(h.report()["corpora"][0]["ratchet"]["ident.mrr"], 0.5);

    // Now it ranks first: better, but unblessed.
    h.write_ident(&h.correct_rows(IDENT.1, Some(DEF_PATH)));
    let check = h.check();

    assert_exit(&check, 1);
    let err = stderr(&check);
    assert!(err.contains("ident.mrr"), "{err}");
    assert!(err.contains("bless required"), "{err}");
    let failures = gate_failures(&h.report());
    assert!(
        failures
            .iter()
            .any(|(kind, check, _, message)| kind == "ratchet"
                && check == "ident.mrr"
                && message.contains("improved")
                && message.contains("bless required")),
        "{failures:#?}"
    );

    // Blessing the improvement makes the gate green again.
    assert_exit(&h.bless(None), 0);
    assert_exit(&h.check(), 0);
}

#[test]
fn a_ratchet_regression_needs_an_accepted_reason_to_bless() {
    let h = Harness::new();
    h.bless_current();

    h.write_ident(&h.correct_rows(IDENT.1, Some("src/other.rs")));
    assert_exit(&h.run(), 0);

    let refused = h.bless(None);
    assert_exit(&refused, 1);
    let err = stderr(&refused);
    assert!(err.contains("ident.mrr"), "{err}");
    assert!(err.contains("--accept-regression"), "{err}");

    let accepted = h.bless(Some("fixture: def file demoted on purpose"));
    assert_exit(&accepted, 0);
    let baseline: Value = serde_json::from_slice(&fs::read(h.baseline_path()).unwrap()).unwrap();
    let accepted = baseline["accepted_regressions"].as_array().unwrap();
    assert_eq!(accepted.len(), 1);
    assert_eq!(
        accepted[0]["reason"],
        "fixture: def file demoted on purpose"
    );
    assert_exit(&h.check(), 0);
}

/// Ledgering a check that the baseline blessed as `pass` downgrades it to
/// `xfail`: the gate asks for a bless, and the bless needs a reason.
#[test]
fn a_newly_ledgered_failure_needs_an_accepted_reason_to_bless() {
    let h = Harness::new();
    h.bless_current();

    h.drop_lexical_ground_truth_file();
    h.write_ledger(
        r##"[[xfail]]
issue = "#9004"
check = "lexical.recall"
ids = ["fixture-X01"]

[[xfail]]
issue = "#9004"
check = "lexical.silent_fn"
ids = ["fixture-X01"]
"##,
    );
    assert_exit(&h.check(), 1);

    let refused = h.bless(None);
    assert_exit(&refused, 1);
    let err = stderr(&refused);
    assert!(
        err.contains("fixture/fixture-X01 lexical.recall: pass -> xfail"),
        "{err}"
    );
    assert!(
        err.contains("fixture/fixture-X01 lexical.silent_fn: pass -> xfail"),
        "{err}"
    );
    assert!(err.contains("--accept-regression"), "{err}");

    assert_exit(&h.bless(Some("fixture: #9004 filed")), 0);
    let baseline: Value = serde_json::from_slice(&fs::read(h.baseline_path()).unwrap()).unwrap();
    let accepted = baseline["accepted_regressions"].as_array().unwrap();
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0]["reason"], "fixture: #9004 filed");
    assert_exit(&h.check(), 0);
}

#[test]
fn bless_refuses_a_report_with_unledgered_hard_failures() {
    let h = Harness::new();
    h.drop_lexical_ground_truth_file();
    assert_exit(&h.run(), 0);

    let bless = h.bless(None);

    assert_exit(&bless, 1);
    let err = stderr(&bless);
    assert!(err.contains("lexical.silent_fn"), "{err}");
    assert!(err.contains("fixture-X01"), "{err}");
    assert!(!h.baseline_path().exists());
}

#[test]
fn removing_a_ledger_entry_for_a_failing_check_fails_naming_check_and_ids() {
    let h = Harness::new();
    h.drop_lexical_ground_truth_file();
    let recall = r##"[[xfail]]
issue = "#9001"
check = "lexical.recall"
ids = ["fixture-X01"]
"##;
    let silent = r##"
[[xfail]]
issue = "#9001"
check = "lexical.silent_fn"
ids = ["fixture-X01"]
"##;
    h.write_ledger(&format!("{recall}{silent}"));
    h.bless_current();
    assert_exit(&h.check(), 0);
    assert_eq!(
        outcome(&h.report(), "fixture-X01", "lexical.silent_fn").as_deref(),
        Some("xfail")
    );

    h.write_ledger(recall);
    let check = h.check();

    assert_exit(&check, 1);
    let err = stderr(&check);
    let line = err
        .lines()
        .find(|l| l.contains("lexical.silent_fn"))
        .unwrap_or_else(|| panic!("no failure line names the check:\n{err}"));
    assert!(line.contains("fixture-X01"), "{line}");
}

#[test]
fn golden_def_line_drift_is_a_harness_error() {
    let h = Harness::new();
    h.write_golden(DEF_LINE + 1);

    let check = h.check();

    assert_exit(&check, 2);
    let err = stderr(&check);
    assert!(err.contains("fixture-L01"), "{err}");
    assert!(err.contains("does not contain"), "{err}");
    assert!(
        !h.report_path().exists(),
        "a harness error writes no report"
    );
}

#[test]
fn unparsable_skim_output_is_a_harness_error() {
    let h = Harness::new();
    let (_, q, f) = LEXICAL;
    h.write_response(
        q,
        f,
        &format!("l{FULL_LIMIT}_o0.json"),
        "skim search: boom\n",
    );

    let check = h.check();

    assert_exit(&check, 2);
    let err = stderr(&check);
    assert!(err.contains("fixture-X01"), "{err}");
    assert!(err.contains("JSON"), "{err}");
}

/// skim refuses its temporal data (here: a `temporal.db` written by a newer
/// skim) and says so in `--stats`, so `--hot` is not applied. A ledgered
/// `--hot` check then "passes" on the fallback order; the gate must not call
/// that an XPASS ("promote: remove the ledger entry"), which would bake the
/// broken temporal layer into the ledger and the baseline.
#[test]
fn unusable_temporal_data_is_a_harness_error_not_an_xpass() {
    let h = Harness::new();
    h.bless_current();
    h.write_ledger(
        r##"[[xfail]]
issue = "#9002"
check = "order.prefix_consistent"
ids = ["fixture-F001"]
note = "fixture"
"##,
    );
    h.write_stats(Some("newer-schema"));
    fs::remove_file(h.report_path()).unwrap();

    let check = h.check();

    assert_exit(&check, 2);
    let err = stderr(&check);
    assert!(err.contains("newer-schema"), "{err}");
    assert!(err.contains("fixture-F001"), "{err}");
    assert!(!err.contains("XPASS"), "{err}");
    assert!(
        !h.report_path().exists(),
        "a harness error writes no report"
    );
}

/// A `--hot` query whose JSON discloses that the temporal ranking was not
/// applied (`degraded[]`) cannot be scored as a `--hot` result.
#[test]
fn a_temporal_ranking_skim_reports_as_degraded_is_a_harness_error() {
    let h = Harness::new();
    let (_, q, f) = PREFIX;
    let rows = h.correct_rows(q, None);
    let mut body: Value = serde_json::from_str(&page_json(q, &rows, 0, rows.len(), false)).unwrap();
    body["degraded"] = json!([{
        "subsystem": "temporal",
        "reason": "missing",
        "requested": "hot",
        "applied": "lexical",
        "message": "temporal.db is missing; --hot not applied",
        "remediation": "skim search --rebuild",
    }]);
    h.write_response(q, f, &format!("l{FULL_LIMIT}_o0.json"), &body.to_string());

    let check = h.check();

    assert_exit(&check, 2);
    let err = stderr(&check);
    assert!(err.contains("fixture-F001"), "{err}");
    assert!(err.contains("degraded"), "{err}");
    assert!(
        !h.report_path().exists(),
        "a harness error writes no report"
    );
}

/// A standalone `--ast` entry has no oracle, so an empty full list satisfies
/// every check that runs on it. If skim's structural layer broke and returned
/// nothing, a ledgered `order.score_monotone` (#547 on the real corpora) would
/// "XPASS" and ask for a promotion that bakes the breakage into the ledger and
/// the baseline. It is a harness error naming the corpus and the entry.
#[test]
fn an_empty_list_without_an_oracle_is_a_harness_error_not_an_xpass() {
    let h = Harness::new();
    // Scores rise down the list: the #547 shape, ledgered.
    h.write_ast(&h.ast_rows(&[1.0, 2.0]));
    h.write_ledger(
        r##"[[xfail]]
issue = "#9003"
check = "order.score_monotone"
ids = ["fixture-F002"]
note = "fixture"
"##,
    );
    h.bless_current();
    assert_eq!(
        outcome(&h.report(), "fixture-F002", "order.score_monotone").as_deref(),
        Some("xfail")
    );
    h.write_ast(&[]);
    fs::remove_file(h.report_path()).unwrap();

    let check = h.check();

    assert_exit(&check, 2);
    let err = stderr(&check);
    assert!(err.contains("corpus fixture"), "{err}");
    assert!(err.contains("fixture-F002"), "{err}");
    assert!(err.contains("empty"), "{err}");
    assert!(!err.contains("XPASS"), "{err}");
    assert!(
        !h.report_path().exists(),
        "a harness error writes no report"
    );
}

/// With no oracle to judge a `--ast` list, its row count is a RATCHET value:
/// a silent shrink fails the gate, and blessing it needs a reason.
#[test]
fn a_shrinking_list_without_an_oracle_is_a_ratchet_regression() {
    let h = Harness::new();
    h.bless_current();
    let metric = "oracle_less.full_rows.fixture-F002";
    assert_eq!(h.report()["corpora"][0]["ratchet"][metric], 2.0);

    h.write_ast(&h.ast_rows(&[2.0]));
    let check = h.check();

    assert_exit(&check, 1);
    let failures = gate_failures(&h.report());
    assert!(
        failures
            .iter()
            .any(|(kind, check, _, message)| kind == "ratchet"
                && check == metric
                && message.contains("regressed")),
        "{failures:#?}"
    );
    let refused = h.bless(None);
    assert_exit(&refused, 1);
    let err = stderr(&refused);
    assert!(err.contains(metric), "{err}");
    assert!(err.contains("--accept-regression"), "{err}");

    assert_exit(
        &h.bless(Some("fixture: one structural match removed on purpose")),
        0,
    );
    assert_exit(&h.check(), 0);
}

#[test]
fn golden_gen_prints_integrity_clean_ident_candidates() {
    let h = Harness::new();
    let out = h.golden_gen("fixture");
    assert_exit(&out, 0);

    // stdout is a proposal: `[[ident]]` entries to paste under a golden header.
    let proposal = String::from_utf8(out.stdout).unwrap();
    let golden = parse_golden(&format!(
        "corpus = \"fixture\"\ncommit = \"{}\"\n{proposal}",
        h.commit
    ))
    .unwrap();
    let mut got: Vec<(&str, &str, u32)> = golden
        .idents
        .iter()
        .map(|e| (e.query.as_str(), e.def.path.as_str(), e.def.line))
        .collect();
    got.sort_unstable();
    // refresh / BuildLock / acquire occur in one file only (ground truth < 2).
    assert_eq!(
        got,
        vec![
            ("check_staleness", "src/staleness.rs", 2),
            ("marker", "src/marker.rs", 2)
        ]
    );
    assert!(golden.idents.iter().all(|e| e.origin == Origin::Generated));
    let violations = check_integrity(
        &golden,
        &IntegrityContext {
            corpus: "fixture",
            commit: &h.commit,
            universe: Some(&h.universe),
            ledger: &[],
        },
    );
    assert!(violations.is_empty(), "{violations:?}");
}

#[test]
fn golden_gen_names_the_known_corpora_for_an_unknown_one() {
    let h = Harness::new();
    let out = h.golden_gen("nope");
    assert_exit(&out, 2);
    assert!(stderr(&out).contains("known: fixture"), "{}", stderr(&out));
}

#[test]
fn two_consecutive_runs_write_identical_reports_apart_from_latency() {
    let h = Harness::new();

    assert_exit(&h.run(), 0);
    let first = fs::read_to_string(h.report_path()).unwrap();
    assert_exit(&h.run(), 0);
    let second = fs::read_to_string(h.report_path()).unwrap();

    let strip = |raw: &str| -> Value {
        let mut v: Value = serde_json::from_str(raw).unwrap();
        assert!(v.get("latency").is_some(), "report has no latency section");
        v.as_object_mut().unwrap().remove("latency");
        v
    };
    assert_eq!(strip(&first), strip(&second));
    // Byte-level: everything before the (last) latency key is identical.
    let prefix = |raw: &str| raw[..raw.find("\"latency\"").unwrap()].to_string();
    assert_eq!(prefix(&first), prefix(&second));
}
