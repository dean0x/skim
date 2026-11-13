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
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rskim_bench::scoreboard::MAX_PAGES;
use rskim_bench::scoreboard::corpus::load_corpora;
use rskim_bench::scoreboard::golden::{
    ClashSplit, IntegrityContext, Origin, StakesExpect, check_integrity, parse_golden,
};
use rskim_bench::scoreboard::oracle::{LexicalQuery, ground_truth};
use rskim_bench::scoreboard::test_support::FixtureRepo;
use rskim_bench::scoreboard::universe::{
    GitIsolation, LEFT_OUT_REASONS, TextUniverse, text_universe,
};
use rskim_research::clone::verify_pinned_clone;
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
        "src/loops.rs",
        "fn walk() {\n    for a in 0..2 {\n        for b in 0..2 {\n            step(a, b);\n        }\n    }\n}\n",
    ),
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

[[lexical]]
id = "fixture-X02"
query = "marker"
category = "lang"
lang = "rust"

[[lexical]]
id = "fixture-X03"
query = "qzvx_absent_7713"
category = "zero-hit"

[[pagination]]
id = "fixture-G001"
query = "fn"
limits = [2]

[[prefix]]
id = "fixture-F001"
query = "fn"
limits = [1]
"#
    )
}

/// The golden entries the stub answers: (id, query, flags).
const IDENT: (&str, &str, &[&str]) = ("fixture-L01", "check_staleness", &[]);
const CONCEPT: (&str, &str, &[&str]) = ("fixture-C01", "build lock", &[]);
const LEXICAL: (&str, &str, &[&str]) = ("fixture-X01", "marker", &[]);
const LEXICAL_RUST: (&str, &str, &[&str]) = ("fixture-X02", "marker", &["-t", "rust"]);
const PAGINATION: (&str, &str, &[&str]) = ("fixture-G001", "fn", &[]);
const PREFIX: (&str, &str, &[&str]) = ("fixture-F001", "fn", &[]);
const PAGE_LIMIT: u32 = 2;
const PREFIX_LIMIT: u32 = 1;
const FULL_LIMIT: u32 = 1_000_000;

/// A query that no file of the fixture holds.
const ABSENT: &str = "qzvx_absent_7713";

// ============================================================================
// Stub skim
// ============================================================================

/// MOCK: the stub `skim`. Reads `--limit` / `--offset` / `--json` / `--`
/// from argv, keys the response on hex(query + "\n" + " flag" per flag
/// token), and serves `q_<key>/l<limit>_o<offset>.json`, falling back to
/// `q_<key>/l<limit>_default.json` (JSON) or `q_<key>/text.txt` (text mode).
/// A `q_<key>/exit_code` file sets the exit status. Every call is logged in
/// `calls.log` (key, mode, limit, offset) and `argv.log` (the argument list).
const STUB: &str = r#"#!/bin/sh
set -u
json=0
limit=20
offset=0
query=
flags=
printf '%s\n' "$*" >> "$STUB_DIR/argv.log"
if [ "${1:-}" = search ]; then shift; fi
while [ $# -gt 0 ]; do
  case "$1" in
    --json) json=1 ;;
    --root) shift ;;
    --limit) shift; limit=$1 ;;
    --offset) shift; offset=$1 ;;
    --) shift; query=$*; break ;;
    *) flags="$flags $1" ;;
  esac
  shift
done
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
  if [ -f "$dir/exit_code" ]; then exit "$(cat "$dir/exit_code")"; fi
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

/// One canned matching line.
#[derive(Debug, Clone)]
struct Row {
    path: String,
    line: u32,
    content: String,
    /// The role the output names for the line; `None` derives it from the
    /// line's text.
    role: Option<&'static str>,
}

impl Row {
    /// The role the engine names for this line: a declaration is `def`, a
    /// comment is `comment`, a line with a call is `call`, any other line is
    /// `text`.
    fn role(&self) -> &'static str {
        if let Some(role) = self.role {
            return role;
        }
        let line = self.content.trim_start();
        if ["fn ", "pub fn ", "struct ", "pub struct "]
            .iter()
            .any(|p| line.starts_with(p))
        {
            "def"
        } else if line.starts_with("//") {
            "comment"
        } else if line.contains('(') {
            "call"
        } else {
            "text"
        }
    }
}

/// `rows` grouped by path, in the order each path is first seen.
fn by_file(rows: &[Row]) -> Vec<(&str, Vec<&Row>)> {
    let mut files: Vec<(&str, Vec<&Row>)> = Vec::new();
    for row in rows {
        match files.iter_mut().find(|(path, _)| *path == row.path) {
            Some((_, lines)) => lines.push(row),
            None => files.push((&row.path, vec![row])),
        }
    }
    files
}

fn zero_left_out() -> Value {
    Value::Object(
        LEFT_OUT_REASONS
            .iter()
            .map(|r| (r.to_string(), json!(0)))
            .collect(),
    )
}

/// The definition sites a page states: the `def` lines of `rows` that hold
/// `query`.
fn definition_sites(rows: &[Row], query: &str) -> Vec<(String, u32)> {
    rows.iter()
        .filter(|r| r.role() == "def" && r.content.contains(query))
        .map(|r| (r.path.clone(), r.line))
        .collect()
}

/// The engine's JSON envelope for the page `offset..` of a list of `total`
/// files. `rows` are the page's matching lines; each file holds the lines
/// with its path, and is scored `total - rank` (strictly descending).
fn envelope(
    query: &str,
    rows: &[Row],
    offset: usize,
    total: usize,
    has_more: bool,
    searched_files: u64,
) -> Value {
    let words = query.split_whitespace().count();
    let files: Vec<Value> = by_file(rows)
        .into_iter()
        .enumerate()
        .map(|(i, (path, lines))| {
            json!({
                "path": path,
                "rank": offset + i + 1,
                "score": (total - (offset + i)) as f64,
                "words_matched": words,
                "words_total": words,
                "line_count": lines.len(),
                "roles": {},
                "occurrences": lines
                    .iter()
                    .map(|r| json!({"line": r.line, "role": r.role(), "text": r.content}))
                    .collect::<Vec<_>>(),
                "occurrences_elided": 0,
            })
        })
        .collect();
    let sites = definition_sites(rows, query);
    json!({
        "query": query,
        "match_mode": "substring",
        "case": "sensitive",
        "view": "labelled",
        "total_lines": rows.len(),
        "total_files": total,
        "has_more": has_more,
        "definitions": {
            "count": sites.len(),
            "at_least": false,
            "sites": sites
                .iter()
                .map(|(path, line)| json!({"path": path, "line": line}))
                .collect::<Vec<_>>(),
        },
        "files": files,
        "searched_files": searched_files,
        "left_out": zero_left_out(),
        "parse": {"files": 2, "bytes": 400, "budget_hit": false, "unlabelled_files": 0},
        "cost": {"files_read": searched_files, "bytes_read": 1000, "files_parsed": 2, "bytes_parsed": 400},
        "notices": [],
        "duration_ms": 1,
    })
}

/// The labelled view's text for `rows`: one `<role> <path>:<line>: <text>`
/// line per matching line.
fn text_labelled(rows: &[Row]) -> String {
    rows.iter()
        .map(|r| format!("{} {}:{}: {}\n", r.role(), r.path, r.line, r.content))
        .collect()
}

/// The word view's text for `rows`: per file a `<path>  <word counts>` header,
/// its `<path>:<line>: <text>` lines and an empty line.
fn text_words(rows: &[Row], words: &str) -> String {
    by_file(rows)
        .into_iter()
        .map(|(path, lines)| {
            let body: String = lines
                .iter()
                .map(|r| format!("{path}:{}: {}\n", r.line, r.content))
                .collect();
            format!("{path}  {words}\n{body}\n")
        })
        .collect()
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
    text: TextUniverse,
}

impl Harness {
    /// A fixture corpus, its golden set, and a stub that answers every
    /// golden query correctly. The ledger is empty, and there is no
    /// baseline.
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
        let text = text_universe(&root, &GitIsolation::new(source.home())).unwrap();

        let h = Harness {
            _source: source,
            corpus_dir,
            data_dir: tempfile::tempdir().unwrap(),
            stub_dir: tempfile::tempdir().unwrap(),
            out_dir: tempfile::tempdir().unwrap(),
            commit,
            text,
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
        h.write_ledger("");
        h.install_stub();
        h.write_correct_responses();
        h
    }

    fn write_golden(&self, def_line: u32) {
        self.write_golden_with(def_line, "");
    }

    /// The golden file plus `extra` entries appended.
    fn write_golden_with(&self, def_line: u32, extra: &str) {
        let dir = self.data_dir.path().join("golden");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("fixture.toml"),
            format!("{}{extra}", golden_toml(&self.commit, def_line)),
        )
        .unwrap();
    }

    /// The ledger: `body`.
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
    }

    // --- canned responses ---------------------------------------------------

    /// The files the stub says it searched.
    fn searched(&self) -> u64 {
        u64::try_from(self.text.len()).unwrap()
    }

    /// The oracle's ground truth for `query` over the text universe, as one
    /// row per file anchored on the first line holding the query's first
    /// word. `first` moves one path to rank 1.
    fn correct_rows(&self, query: &str, first: Option<&str>) -> Vec<Row> {
        let q = LexicalQuery::new(query, None).unwrap();
        let mut paths = ground_truth(self.text.files(), &q);
        if let Some(first) = first {
            let at = paths.iter().position(|p| p == first).unwrap();
            let p = paths.remove(at);
            paths.insert(0, p);
        }
        let token = query.split_whitespace().next().unwrap();
        paths
            .into_iter()
            .map(|path| {
                let text = self.text.text(&path).unwrap();
                let (idx, line) = text
                    .split('\n')
                    .enumerate()
                    .find(|(_, l)| l.contains(token))
                    .unwrap_or((0, ""));
                Row {
                    path,
                    line: u32::try_from(idx + 1).unwrap(),
                    content: line.to_string(),
                    role: None,
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

    /// Make the stub exit with `code` after it prints the canned answer of
    /// `query` (a query with no hit exits 1).
    fn write_exit_code(&self, query: &str, flags: &[&str], code: i32) {
        self.write_response(query, flags, "exit_code", &code.to_string());
    }

    /// Serve `body` as the full list of `query`.
    fn write_full_json(&self, query: &str, flags: &[&str], body: &Value) {
        self.write_response(
            query,
            flags,
            &format!("l{FULL_LIMIT}_o0.json"),
            &body.to_string(),
        );
    }

    fn write_full(&self, query: &str, flags: &[&str], rows: &[Row]) {
        let body = envelope(query, rows, 0, by_file(rows).len(), false, self.searched());
        self.write_full_json(query, flags, &body);
    }

    /// Honest pages `--offset 0, limit, 2·limit, …` of `rows` (one row per file).
    fn write_pages(&self, query: &str, flags: &[&str], rows: &[Row], limit: u32) {
        let limit = limit as usize;
        let mut offset = 0;
        loop {
            let end = (offset + limit).min(rows.len());
            let body = envelope(
                query,
                &rows[offset..end],
                offset,
                rows.len(),
                end < rows.len(),
                self.searched(),
            );
            self.write_response(
                query,
                flags,
                &format!("l{limit}_o{offset}.json"),
                &body.to_string(),
            );
            offset += limit;
            if offset >= rows.len() {
                break;
            }
        }
    }

    fn write_limited(&self, query: &str, flags: &[&str], rows: &[Row], limit: u32) {
        let n = (limit as usize).min(rows.len());
        let body = envelope(
            query,
            &rows[..n],
            0,
            rows.len(),
            n < rows.len(),
            self.searched(),
        );
        self.write_response(
            query,
            flags,
            &format!("l{limit}_o0.json"),
            &body.to_string(),
        );
    }

    fn write_text(&self, query: &str, flags: &[&str], text: &str) {
        self.write_response(query, flags, "text.txt", text);
    }

    /// Serve the ident entry: its full list and its labelled text.
    fn write_ident(&self, rows: &[Row]) {
        let (_, q, f) = IDENT;
        self.write_full(q, f, rows);
        self.write_text(q, f, &text_labelled(rows));
    }

    fn write_correct_responses(&self) {
        self.write_ident(&self.correct_rows(IDENT.1, Some(DEF_PATH)));

        let (_, q, f) = CONCEPT;
        let rows = self.correct_rows(q, None);
        self.write_full(q, f, &rows);
        self.write_text(q, f, &text_words(&rows, "build 1/2 lock 1/2"));

        let (_, q, f) = LEXICAL;
        self.write_full(q, f, &self.correct_rows(q, None));

        let (_, q, f) = LEXICAL_RUST;
        let rows: Vec<Row> = self
            .correct_rows(q, None)
            .into_iter()
            .filter(|r| r.path.ends_with(".rs"))
            .collect();
        self.write_full(q, f, &rows);

        // A query with no hit prints a normal envelope and exits 1.
        self.write_full(ABSENT, &[], &[]);
        self.write_exit_code(ABSENT, &[], 1);

        let (_, q, f) = PAGINATION;
        let rows = self.correct_rows(q, None);
        self.write_full(q, f, &rows);
        self.write_pages(q, f, &rows, PAGE_LIMIT);

        let (_, q, f) = PREFIX;
        let rows = self.correct_rows(q, None);
        self.write_full(q, f, &rows);
        self.write_limited(q, f, &rows, PREFIX_LIMIT);
    }

    /// Serve the lexical entry's full list without one ground-truth file: a
    /// silent false negative.
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

    /// The argument list of every call the stub received.
    fn argv(&self) -> Vec<String> {
        fs::read_to_string(self.stub_dir.path().join("argv.log"))
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

/// The failure detail the report holds for `(id, check)`.
fn detail_of(report: &Value, id: &str, check: &str) -> String {
    report["corpora"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|c| c["checks"].as_array().unwrap().iter())
        .find(|c| c["id"] == id && c["check"] == check)
        .and_then(|c| c["detail"].as_str())
        .unwrap_or_default()
        .to_string()
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
        "lexical.recall_tracked",
        "order.score_monotone",
    ] {
        assert_eq!(
            outcome(&report, "fixture-X01", check).as_deref(),
            Some("pass"),
            "{check}"
        );
    }
    // The first definition site is the frozen definition line.
    assert_eq!(
        outcome(&report, "fixture-L01", "ident.def_first").as_deref(),
        Some("pass")
    );
    assert_eq!(
        outcome(&report, "fixture-G001", "pagination.complete").as_deref(),
        Some("pass")
    );
    // Every prefix list is checked for ordering by score too.
    assert_eq!(
        outcome(&report, "fixture-F001", "order.prefix_consistent").as_deref(),
        Some("pass")
    );
    assert_eq!(
        outcome(&report, "fixture-F001", "order.score_monotone").as_deref(),
        Some("pass")
    );
    // A zero-hit query exits 1 with a normal envelope: that is a page.
    assert_eq!(
        outcome(&report, "fixture-X03", "lexical.silent_fn").as_deref(),
        Some("pass")
    );
    // The accounting the output states agrees with the oracle's.
    assert_eq!(report["corpora"][0]["universe"]["delta"], 0);
    assert_eq!(
        report["corpora"][0]["ratchet"]["coverage.tracked_text"],
        1.0
    );

    let bless = h.bless(None);
    assert_exit(&bless, 0);
    let check = h.check();
    assert_exit(&check, 0);
    assert_eq!(h.report()["gate"]["status"], "pass");
}

#[test]
fn the_scoreboard_only_runs_searches_and_never_builds_or_asks_for_stats() {
    let h = Harness::new();
    assert_exit(&h.run(), 0);

    let argv = h.argv();
    assert!(!argv.is_empty());
    for line in &argv {
        assert!(line.starts_with("search "), "{line}");
        for retired in ["--build", "--rebuild", "--update", "--stats"] {
            assert!(!line.contains(retired), "{line}");
        }
    }
}

#[test]
fn the_rg_word_rows_are_info_and_the_cost_rows_follow_the_cost_the_output_states() {
    let h = Harness::new();
    assert_exit(&h.run(), 0);
    let report = h.report();
    let corpus = &report["corpora"][0];

    // The `rg -nw` byte columns are in the INFO section of the report.
    let reference = corpus["info"]["reference"].as_object().unwrap();
    for name in ["bytes.rgw_text_median", "bytes.rgw_first_correct_median"] {
        assert!(
            reference[name].as_f64().is_some_and(|v| v > 0.0),
            "{name}: {reference:?}"
        );
    }

    // The stub states the cost counters of every call: their medians are
    // RATCHET values, and no `rg -nw` row is.
    for ratchet in [&corpus["ratchet"], &report["aggregate"]["ratchet"]] {
        assert_eq!(ratchet["cost.files_parsed_median"], 2.0);
        assert_eq!(ratchet["cost.bytes_parsed_median"], 400.0);
        assert_eq!(ratchet["cost.bytes_read_median"], 1000.0);
        let names: Vec<&String> = ratchet.as_object().unwrap().keys().collect();
        assert!(
            names.iter().all(|n| !n.starts_with("bytes.rgw")),
            "{names:?}"
        );
    }

    // The cost reaches the baseline, the `rg -nw` rows do not, and the gate
    // stays green.
    assert_exit(&h.bless(None), 0);
    let baseline = fs::read_to_string(h.baseline_path()).unwrap();
    assert!(baseline.contains("cost.bytes_parsed_median"));
    assert!(
        !baseline.contains("rgw"),
        "an rg -nw row is in the baseline"
    );
    assert_exit(&h.check(), 0);
    assert_eq!(h.report()["gate"]["status"], "pass");
}

#[test]
fn the_text_bytes_end_at_the_definition_line_and_at_the_end_of_the_first_relevant_block() {
    let h = Harness::new();
    // The definition line is the fourth line; a decoy line names a path that
    // only starts like the definition's path, and a call line follows.
    let ident = [
        "call src/other.rs:2: if check_staleness() {}\n",
        "ref src/other.rs:1: fn refresh() {\n",
        "def src/staleness.rs.bak:2: decoy\n",
        "def src/staleness.rs:2: pub fn check_staleness() -> bool {\n",
        "call src/other.rs:9: later\n",
    ];
    let (_, q, f) = IDENT;
    h.write_text(q, f, &ident.concat());
    // The first relevant file is README.md. Its block has two lines and ends
    // at a line of another path, though that path starts like README.md.
    let concept = [
        "README.md  build 1/2 lock 1/2\n",
        "README.md:3: The build lock and the elision marker.\n",
        "README.md:5: more\n",
        "README.md.orig:1: not a line of README.md\n",
        "src/lock.rs  build 1/2 lock 1/2\n",
        "src/lock.rs:1: /// Holds the build lock.\n",
    ];
    let (_, q, f) = CONCEPT;
    h.write_text(q, f, &concept.concat());

    assert_exit(&h.run(), 0);

    let report = h.report();
    let corpus = &report["corpora"][0];
    let len = |lines: &[&str]| lines.iter().map(|l| l.len()).sum::<usize>() as f64;
    assert_eq!(
        corpus["ratchet"]["bytes.first_correct_median"],
        len(&ident[..4])
    );
    assert_eq!(
        corpus["info"]["reference"]["bytes.first_relevant_median"],
        len(&concept[..3])
    );
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
fn a_file_that_holds_only_some_of_the_words_is_not_a_row() {
    let h = Harness::new();
    h.bless_current();

    // The word view lists a file with one of the two words after the files
    // with both. It is not in the AND set: no row, so no precision failure.
    let (_, q, f) = CONCEPT;
    let rows = h.correct_rows(q, None);
    let mut body = envelope(q, &rows, 0, rows.len(), false, h.searched());
    body["files"].as_array_mut().unwrap().push(json!({
        "path": "src/staleness.rs",
        "rank": rows.len() + 1,
        "score": 0.5,
        "words_matched": 1,
        "words_total": 2,
        "line_count": 1,
        "roles": {},
        "occurrences": [{"line": 1, "role": "comment", "text": "// staleness checks"}],
        "occurrences_elided": 0,
    }));
    h.write_full_json(q, f, &body);

    assert_exit(&h.check(), 0);
    let report = h.report();
    for check in [
        "lexical.precision",
        "lexical.recall",
        "results.unique_paths",
    ] {
        assert_eq!(
            outcome(&report, "fixture-C01", check).as_deref(),
            Some("pass"),
            "{check}"
        );
    }
}

#[test]
fn a_type_filter_is_passed_as_dash_t_and_scored_by_the_oracles_type_table() {
    let h = Harness::new();
    assert_exit(&h.run(), 0);
    assert!(
        h.argv()
            .iter()
            .any(|a| a.contains("-t rust") && a.ends_with("-- marker")),
        "{:?}",
        h.argv()
    );
    let report = h.report();
    assert_eq!(
        outcome(&report, "fixture-X02", "lexical.precision").as_deref(),
        Some("pass")
    );

    // A skim that ignores the type filter lists README.md too.
    let (_, q, f) = LEXICAL_RUST;
    h.write_full(q, f, &h.correct_rows(q, None));
    assert_exit(&h.run(), 0);
    let report = h.report();
    assert_eq!(
        outcome(&report, "fixture-X02", "lexical.precision").as_deref(),
        Some("fail")
    );
    assert!(detail_of(&report, "fixture-X02", "lexical.precision").contains("README.md"));
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
        &envelope(q, &rows[last..], last, rows.len(), true, h.searched()).to_string(),
    );
    h.write_response(
        q,
        f,
        &format!("l{PAGE_LIMIT}_default.json"),
        &envelope(q, &[], 0, rows.len(), true, h.searched()).to_string(),
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

#[test]
fn a_text_view_that_grows_is_a_ratchet_regression_in_bytes() {
    let h = Harness::new();
    h.bless_current();

    // The same answer in a text view twice as long.
    let (_, q, f) = IDENT;
    let rows = h.correct_rows(q, Some(DEF_PATH));
    let text = text_labelled(&rows);
    h.write_text(q, f, &format!("{text}{text}"));
    let check = h.check();

    assert_exit(&check, 1);
    let failures = gate_failures(&h.report());
    assert!(
        failures
            .iter()
            .any(|(kind, check, _, message)| kind == "ratchet"
                && check == "bytes.text_median"
                && message.contains("regressed")),
        "{failures:#?}"
    );
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

[[xfail]]
issue = "#9004"
check = "lexical.recall_tracked"
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
    assert!(
        err.contains("fixture/fixture-X01 lexical.recall_tracked: pass -> xfail"),
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
    // Dropping a ground-truth file fails both recall checks; `recall` ledgers them.
    let recall = r##"[[xfail]]
issue = "#9001"
check = "lexical.recall"
ids = ["fixture-X01"]

[[xfail]]
issue = "#9001"
check = "lexical.recall_tracked"
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

#[test]
fn an_error_envelope_from_skim_is_a_harness_error_naming_the_message() {
    let h = Harness::new();
    let (_, q, f) = LEXICAL;
    h.write_full_json(
        q,
        f,
        &json!({"error": "pattern rejected: nothing to search"}),
    );
    h.write_exit_code(q, f, 2);

    let check = h.check();

    assert_exit(&check, 2);
    let err = stderr(&check);
    assert!(err.contains("fixture-X01"), "{err}");
    assert!(err.contains("pattern rejected: nothing to search"), "{err}");
    assert!(
        !h.report_path().exists(),
        "a harness error writes no report"
    );
}

#[test]
fn an_envelope_without_searched_files_is_a_harness_error() {
    let h = Harness::new();
    let (_, q, f) = LEXICAL;
    let rows = h.correct_rows(q, None);
    let mut body = envelope(q, &rows, 0, rows.len(), false, h.searched());
    body.as_object_mut().unwrap().remove("searched_files");
    h.write_full_json(q, f, &body);

    let check = h.check();

    assert_exit(&check, 2);
    assert!(
        stderr(&check).contains("searched_files"),
        "{}",
        stderr(&check)
    );
}

/// A file listed twice is invisible to the set-based checks (recall,
/// precision) and to `order.score_monotone` (the list never rises), so it
/// needs its own HARD check.
#[test]
fn a_path_listed_twice_in_a_full_list_fails_unique_paths() {
    let h = Harness::new();
    h.bless_current();

    // The envelope groups the lines of one path into one file entry, so a
    // path listed twice is two file entries.
    let (_, q, f) = IDENT;
    let rows = h.correct_rows(q, Some(DEF_PATH));
    let mut body = envelope(q, &rows, 0, rows.len(), false, h.searched());
    let first = body["files"][0].clone();
    body["files"].as_array_mut().unwrap().insert(1, first);
    h.write_full_json(q, f, &body);
    let check = h.check();

    assert_exit(&check, 1);
    let err = stderr(&check);
    assert!(err.contains("results.unique_paths"), "{err}");
    assert!(err.contains("fixture-L01"), "{err}");
    let report = h.report();
    assert_eq!(
        outcome(&report, "fixture-L01", "results.unique_paths").as_deref(),
        Some("fail")
    );
    let failures = gate_failures(&report);
    assert!(
        failures
            .iter()
            .any(|(kind, check, ids, message)| kind == "unledgered"
                && check == "results.unique_paths"
                && ids == &["fixture-L01".to_string()]
                && message.contains(DEF_PATH)),
        "{failures:#?}"
    );
}

/// Every entry kind — ident, concept, lexical, pagination and prefix — is
/// checked for a repeated path.
#[test]
fn a_repeated_last_row_fails_unique_paths_on_every_entry_kind() {
    let h = Harness::new();
    h.bless_current();

    for (_, q, f) in [IDENT, CONCEPT, LEXICAL, PAGINATION, PREFIX] {
        let rows = h.correct_rows(q, None);
        let mut body = envelope(q, &rows, 0, rows.len(), false, h.searched());
        let last = body["files"].as_array().unwrap().last().unwrap().clone();
        body["files"].as_array_mut().unwrap().push(last);
        h.write_full_json(q, f, &body);
    }
    let check = h.check();

    assert_exit(&check, 1);
    let failures = gate_failures(&h.report());
    let unique: Vec<&Vec<String>> = failures
        .iter()
        .filter(|(kind, check, _, _)| kind == "unledgered" && check == "results.unique_paths")
        .map(|(_, _, ids, _)| ids)
        .collect();
    let every_id: Vec<String> = [IDENT, CONCEPT, LEXICAL, PAGINATION, PREFIX]
        .iter()
        .map(|(id, _, _)| id.to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    assert_eq!(unique, vec![&every_id], "{failures:#?}");
}

/// A harness error writes no report, and must not leave an older one
/// behind either: `bless --from` would then bless a run that never passed.
#[test]
fn a_harness_error_leaves_no_stale_report_to_bless() {
    let h = Harness::new();
    assert_exit(&h.run(), 0);
    let md = h.out_dir.path().join("report.md");
    assert!(h.report_path().exists() && md.exists());
    let (_, q, f) = LEXICAL;
    h.write_response(
        q,
        f,
        &format!("l{FULL_LIMIT}_o0.json"),
        "skim search: boom\n",
    );

    let check = h.check();

    assert_exit(&check, 2);
    assert!(
        !h.report_path().exists(),
        "a stale report.json survived a harness error"
    );
    assert!(!md.exists(), "a stale report.md survived a harness error");
    let bless = h.bless(None);
    assert_ne!(
        bless.status.code(),
        Some(0),
        "bless --from accepted a stale report:\n{}",
        stderr(&bless)
    );
    assert!(!h.baseline_path().exists());
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
            universe: Some(&h.text),
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

// ============================================================================
// #540: the frozen-answer tables, end to end
// ============================================================================

/// A `[[refs]]` entry for `check_staleness`; `sites` are `(path, line, role)`.
/// The name is a whole word on two lines of the fixture: the definition
/// (`src/staleness.rs:2`) and the call (`src/other.rs:2`).
fn refs_entry(sites: &[(&str, u32, &str)]) -> String {
    let sites: String = sites
        .iter()
        .map(|(path, line, role)| {
            format!("  {{ path = \"{path}\", line = {line}, role = \"{role}\" }},\n")
        })
        .collect();
    format!(
        "\n[[refs]]\nid = \"fixture-R01\"\nquery = \"check_staleness\"\n\
         def = {{ path = \"{DEF_PATH}\", line = {DEF_LINE} }}\norigin = \"curated\"\n\
         sites = [\n{sites}]\n"
    )
}

/// The refs entry that holds exactly the whole-word lines of the fixture.
fn exact_refs_entry() -> String {
    refs_entry(&[("src/staleness.rs", 2, "def"), ("src/other.rs", 2, "call")])
}

/// A `[[clash]]` entry for `check_staleness`; `defs` and `calls` are
/// `(path, line)`. The fixture defines the name once and holds it as a whole
/// word on two lines only, so a test that needs two definitions treats the call
/// line as the second one, and freezes that same line as the call: the entry
/// only has to hold whole-word lines. Its calls are not split by definition.
fn clash_entry(defs: &[(&str, u32)], calls: &[(&str, u32)], split: &str) -> String {
    let defs: Vec<String> = defs
        .iter()
        .map(|(path, line)| format!("{{ path = \"{path}\", line = {line} }}"))
        .collect();
    let calls: String = calls
        .iter()
        .map(|(path, line)| {
            format!("  {{ path = \"{path}\", line = {line}, def = \"unassigned\" }},\n")
        })
        .collect();
    format!(
        "\n[[clash]]\nid = \"fixture-D01\"\nquery = \"check_staleness\"\nsplit = \"{split}\"\n\
         origin = \"curated\"\ndefs = [ {} ]\ncalls = [\n{calls}]\n",
        defs.join(", ")
    )
}

const TWO_DEFS: &[(&str, u32)] = &[("src/staleness.rs", 2), ("src/other.rs", 2)];

/// The call line of the fixture, which `TWO_DEFS` also lists as its second
/// definition.
const CLASH_CALLS: &[(&str, u32)] = &[("src/other.rs", 2)];

/// A `[[concept]]` entry on the fixture's `build lock` query, which freezes
/// `first`. The query is the one of `fixture-C01`, so the stub already
/// answers it.
fn concept_with_first(id: &str, first: &str) -> String {
    format!(
        "\n[[concept]]\nid = \"{id}\"\nquery = \"build lock\"\nrelevant = '(?i)build[_\\s-]*lock'\n\
         first = [\"{first}\"]\n"
    )
}

/// One `[[stakes]]` entry on the fixture's `marker` query (or `query`).
fn stakes_entry(id: &str, query: &str, flags: &str, expect: &str, exit: Option<u8>) -> String {
    let exit = exit.map(|c| format!("exit = {c}\n")).unwrap_or_default();
    format!(
        "\n[[stakes]]\nid = \"{id}\"\nquery = \"{query}\"\nflags = {flags}\nexpect = \"{expect}\"\n{exit}"
    )
}

/// The stakes entries that read exit codes, and the ones that read the new
/// flags (`-l`, `-c`, `-i`, `-g`, `-t`, `-F -E`, `-w`).
fn stakes_entries() -> String {
    [
        stakes_entry("fixture-S01", ABSENT, "[]", "exit", Some(1)),
        stakes_entry("fixture-S02", "marker", "[]", "exit", Some(0)),
        stakes_entry("fixture-S03", "marker", "[\"-l\"]", "files", None),
        stakes_entry("fixture-S04", "marker", "[\"-c\"]", "counts", None),
        stakes_entry("fixture-S05", "MARKER", "[\"-i\"]", "lines", None),
        stakes_entry("fixture-S06", "marker", "[\"-g\", \"*.md\"]", "lines", None),
        stakes_entry("fixture-S07", "marker", "[\"-t\", \"rust\"]", "files", None),
        stakes_entry("fixture-S08", "marker", "[\"-F\", \"-E\"]", "exit", Some(2)),
        stakes_entry("fixture-S09", "marker", "[\"-w\"]", "lines", None),
    ]
    .concat()
}

/// Every new table at once, each with the answer the fixture really has.
fn frozen_answer_entries() -> String {
    [
        exact_refs_entry(),
        clash_entry(TWO_DEFS, CLASH_CALLS, "none"),
        concept_with_first("fixture-C02", "src/lock.rs"),
        stakes_entries(),
    ]
    .concat()
}

impl Harness {
    /// The row for `path:line` of the fixture.
    fn row(&self, path: &str, line: u32) -> Row {
        let text = self.text.text(path).unwrap();
        Row {
            path: path.to_string(),
            line,
            content: text
                .split('\n')
                .nth(usize::try_from(line - 1).unwrap())
                .unwrap()
                .to_string(),
            role: None,
        }
    }

    /// Serve the ident query with two definition sites (the stub states the
    /// definitions of a clash): `rows` as its list, `TWO_DEFS` as its sites.
    fn write_two_defs(&self, rows: &[Row]) {
        let (_, q, f) = IDENT;
        let mut body = envelope(q, rows, 0, by_file(rows).len(), false, self.searched());
        body["definitions"] = json!({
            "count": 2,
            "at_least": false,
            "sites": TWO_DEFS
                .iter()
                .map(|(path, line)| json!({"path": path, "line": line}))
                .collect::<Vec<_>>(),
        });
        self.write_full_json(q, f, &body);
        self.write_text(q, f, &text_labelled(rows));
    }

    /// Serve `lines` (`(path, line)`) as the full JSON list of `query` with
    /// `flags`.
    fn write_lines(&self, query: &str, flags: &[&str], lines: &[(&str, u32)]) {
        let rows: Vec<Row> = lines.iter().map(|(p, l)| self.row(p, *l)).collect();
        self.write_full(query, flags, &rows);
    }

    /// Serve every stakes entry of [`stakes_entries`] with the answer a
    /// correct search gives.
    fn write_stakes_responses(&self) {
        // Exit stakes read the status of a text call only.
        self.write_text(ABSENT, &[], "");
        self.write_exit_code(ABSENT, &[], 1);
        self.write_text("marker", &[], "README.md:3\n");
        self.write_text("marker", &["-F", "-E"], "");
        self.write_exit_code("marker", &["-F", "-E"], 2);
        // `-l` prints the paths in path order, `-c` prints `path:count`.
        self.write_text("marker", &["-l"], "README.md\nsrc/marker.rs\n");
        self.write_text("marker", &["-c"], "README.md:1\nsrc/marker.rs:2\n");
        // The listed lines.
        let all = [("README.md", 3), ("src/marker.rs", 1), ("src/marker.rs", 2)];
        self.write_lines("MARKER", &["-i"], &all);
        self.write_lines("marker", &["-w"], &all);
        self.write_lines("marker", &["-g", "*.md"], &[("README.md", 3)]);
        // `-t rust` is the answer of the lexical entry that shares the call.
    }
}

/// A harness that serves the new entries with the answers a correct search
/// gives.
fn frozen_answer_harness() -> Harness {
    let h = Harness::new();
    h.write_golden_with(DEF_LINE, &frozen_answer_entries());
    h.write_two_defs(&h.correct_rows(IDENT.1, Some(DEF_PATH)));
    h.write_stakes_responses();
    h
}

/// `check` over a golden file with `extra` appended: it must stop at
/// integrity with exit 2 and leave no report. Gives stderr.
fn integrity_failure_of(extra: &str) -> String {
    let h = Harness::new();
    h.write_golden_with(DEF_LINE, extra);
    let check = h.check();
    assert_exit(&check, 2);
    let err = stderr(&check);
    assert!(err.contains("golden integrity failed"), "{err}");
    assert!(
        !h.report_path().exists(),
        "a harness error writes no report"
    );
    err
}

#[test]
fn a_refs_entry_whose_sites_differ_from_the_whole_word_lines_exits_2_at_integrity() {
    // One whole-word line is not listed. The definition line carries the role
    // `call`, so the entry has a call site and lacks only `src/other.rs:2`.
    let err = integrity_failure_of(&refs_entry(&[("src/staleness.rs", 2, "call")]));
    assert!(
        err.contains("fixture-R01") && err.contains("missing") && err.contains("src/other.rs:2"),
        "{err}"
    );

    // One listed line does not hold the word as a whole word.
    let err = integrity_failure_of(&refs_entry(&[
        ("src/staleness.rs", 2, "def"),
        ("src/other.rs", 2, "call"),
        ("src/other.rs", 1, "call"),
    ]));
    assert!(
        err.contains("fixture-R01")
            && err.contains("not a whole-word line")
            && err.contains("src/other.rs:1"),
        "{err}"
    );

    // The same entry with the exact set passes integrity.
    let h = Harness::new();
    h.write_golden_with(DEF_LINE, &exact_refs_entry());
    assert_exit(&h.run(), 0);
}

#[test]
fn a_clash_with_one_definition_or_a_split_exits_2_at_integrity() {
    let err = integrity_failure_of(&clash_entry(&TWO_DEFS[..1], CLASH_CALLS, "none"));
    assert!(
        err.contains("fixture-D01") && err.contains("2 or more"),
        "{err}"
    );

    let err = integrity_failure_of(&clash_entry(TWO_DEFS, CLASH_CALLS, "exact"));
    assert!(
        err.contains("fixture-D01") && err.contains("split"),
        "{err}"
    );

    // The two-definition entry with no split passes integrity.
    let h = Harness::new();
    h.write_golden_with(DEF_LINE, &clash_entry(TWO_DEFS, CLASH_CALLS, "none"));
    assert_exit(&h.run(), 0);
}

#[test]
fn a_stakes_entry_the_oracle_cannot_read_exits_2_at_integrity() {
    // `-g` with an alternation, an unknown type, and a flag the search lacks.
    for flags in [
        "[\"-g\", \"*.{rs,md}\"]",
        "[\"-t\", \"cobol\"]",
        "[\"--hidden\"]",
    ] {
        let err =
            integrity_failure_of(&stakes_entry("fixture-S01", "marker", flags, "lines", None));
        assert!(err.contains("fixture-S01"), "{flags}: {err}");
    }
    // An exit the text universe contradicts: `marker` is in the fixture.
    let err = integrity_failure_of(&stakes_entry(
        "fixture-S01",
        "marker",
        "[]",
        "exit",
        Some(1),
    ));
    assert!(
        err.contains("fixture-S01") && err.contains("matching line"),
        "{err}"
    );
}

#[test]
fn the_frozen_answer_entries_pass_when_the_output_states_them() {
    let h = frozen_answer_harness();

    assert_exit(&h.run(), 0);

    let report = h.report();
    let checks = [
        ("fixture-R01", "refs.recall"),
        ("fixture-R01", "refs.calls_recall"),
        ("fixture-R01", "universe.accounted"),
        ("fixture-D01", "clash.defs_complete"),
        ("fixture-D01", "clash.def_count"),
        ("fixture-D01", "clash.calls_recall"),
        ("fixture-C02", "concept.first_right_top5"),
        ("fixture-L01", "ident.def_first"),
        ("fixture-S01", "stakes.exit"),
        ("fixture-S02", "stakes.exit"),
        ("fixture-S03", "stakes.files"),
        ("fixture-S04", "stakes.counts"),
        ("fixture-S05", "stakes.lines"),
        ("fixture-S06", "stakes.lines"),
        ("fixture-S07", "stakes.files"),
        ("fixture-S08", "stakes.exit"),
        ("fixture-S09", "stakes.lines"),
    ];
    for (id, check) in checks {
        assert_eq!(
            outcome(&report, id, check).as_deref(),
            Some("pass"),
            "{id} {check}: {}",
            detail_of(&report, id, check)
        );
    }
    // The first-file check is for the concepts that freeze one.
    assert_eq!(
        outcome(&report, "fixture-C01", "concept.first_right_top5"),
        None
    );
    // A `-l` or `-c` call states no definitions: the entry runs the checks of
    // its own expectation and the unique-path check, nothing else.
    assert_eq!(outcome(&report, "fixture-S03", "stakes.lines"), None);
}

#[test]
fn a_call_the_output_labels_as_text_fails_the_call_recall() {
    let h = frozen_answer_harness();
    let mut rows = h.correct_rows(IDENT.1, Some(DEF_PATH));
    rows[1].role = Some("text");
    h.write_two_defs(&rows);

    assert_exit(&h.run(), 0);

    let report = h.report();
    for (id, check) in [
        ("fixture-R01", "refs.calls_recall"),
        ("fixture-D01", "clash.calls_recall"),
    ] {
        assert_eq!(outcome(&report, id, check).as_deref(), Some("fail"), "{id}");
        let d = detail_of(&report, id, check);
        assert!(d.contains("src/other.rs:2 (role text)"), "{d}");
    }
    // The sites themselves are all listed.
    assert_eq!(
        outcome(&report, "fixture-R01", "refs.recall").as_deref(),
        Some("pass")
    );
}

#[test]
fn a_definition_list_that_misses_a_definition_fails_the_clash_checks() {
    let h = frozen_answer_harness();
    // The ordinary ident page states one definition site.
    h.write_ident(&h.correct_rows(IDENT.1, Some(DEF_PATH)));

    assert_exit(&h.run(), 0);

    let report = h.report();
    assert_eq!(
        outcome(&report, "fixture-D01", "clash.defs_complete").as_deref(),
        Some("fail")
    );
    let d = detail_of(&report, "fixture-D01", "clash.defs_complete");
    assert!(
        d.contains("missing 1 of 2") && d.contains("src/other.rs:2"),
        "{d}"
    );
    assert_eq!(
        outcome(&report, "fixture-D01", "clash.def_count").as_deref(),
        Some("fail")
    );
}

#[test]
fn a_concept_whose_first_file_is_not_in_the_top_five_fails() {
    let h = frozen_answer_harness();
    h.write_golden_with(
        DEF_LINE,
        &format!(
            "{}{}",
            frozen_answer_entries(),
            concept_with_first("fixture-C03", "src/marker.rs")
        ),
    );

    assert_exit(&h.run(), 0);

    // `build lock` lists src/lock.rs, not src/marker.rs.
    let report = h.report();
    assert_eq!(
        outcome(&report, "fixture-C02", "concept.first_right_top5").as_deref(),
        Some("pass")
    );
    assert_eq!(
        outcome(&report, "fixture-C03", "concept.first_right_top5").as_deref(),
        Some("fail")
    );
}

#[test]
fn a_search_that_counts_a_different_number_of_files_fails_the_universe_accounting() {
    let h = frozen_answer_harness();
    let files = h.text.len();
    // One file more than the text universe holds, with no reason for it.
    let (_, q, f) = IDENT;
    let rows = h.correct_rows(q, Some(DEF_PATH));
    let mut body = envelope(q, &rows, 0, rows.len(), false, h.searched() + 1);
    body["left_out"] = zero_left_out();
    h.write_full_json(q, f, &body);

    assert_exit(&h.run(), 0);

    let report = h.report();
    assert_eq!(
        outcome(&report, "fixture-R01", "universe.accounted").as_deref(),
        Some("fail")
    );
    let d = detail_of(&report, "fixture-R01", "universe.accounted");
    assert!(
        d.contains(&format!("searched_files is {}", files + 1))
            && d.contains(&format!("has {files}")),
        "{d}"
    );
    assert_eq!(report["corpora"][0]["universe"]["delta"], -1);
}

#[test]
fn a_search_that_leaves_a_file_out_without_saying_why_fails_the_universe_accounting() {
    let h = frozen_answer_harness();
    let (_, q, f) = IDENT;
    let rows = h.correct_rows(q, Some(DEF_PATH));
    let mut body = envelope(q, &rows, 0, rows.len(), false, h.searched());
    body["left_out"]["symlink"] = json!(1);
    h.write_full_json(q, f, &body);

    assert_exit(&h.run(), 0);

    let report = h.report();
    assert_eq!(
        outcome(&report, "fixture-R01", "universe.accounted").as_deref(),
        Some("fail")
    );
    assert!(detail_of(&report, "fixture-R01", "universe.accounted").contains("symlink"));
}

#[test]
fn each_stakes_expectation_fails_on_the_answer_it_judges() {
    let h = frozen_answer_harness();
    // `-l` lists a file that holds no match; `-c` counts one line short;
    // `-i` misses a line; `-t rust` lists README.md; the rejected flag set
    // exits 0.
    h.write_text("marker", &["-l"], "README.md\nsrc/lock.rs\nsrc/marker.rs\n");
    h.write_text("marker", &["-c"], "README.md:1\nsrc/marker.rs:1\n");
    h.write_lines("MARKER", &["-i"], &[("README.md", 3), ("src/marker.rs", 1)]);
    h.write_lines(
        "marker",
        &["-t", "rust"],
        &[("README.md", 3), ("src/marker.rs", 1)],
    );
    h.write_exit_code("marker", &["-F", "-E"], 0);

    assert_exit(&h.run(), 0);

    let report = h.report();
    let expected = [
        ("fixture-S03", "stakes.files", "src/lock.rs"),
        ("fixture-S04", "stakes.counts", "src/marker.rs"),
        ("fixture-S05", "stakes.lines", "src/marker.rs:2"),
        ("fixture-S07", "stakes.files", "README.md"),
        ("fixture-S08", "stakes.exit", "exit code is 0, expected 2"),
    ];
    for (id, check, naming) in expected {
        assert_eq!(outcome(&report, id, check).as_deref(), Some("fail"), "{id}");
        let d = detail_of(&report, id, check);
        assert!(d.contains(naming), "{id}: {d}");
    }
    // The entries the answers still satisfy stay green.
    for (id, check) in [
        ("fixture-S06", "stakes.lines"),
        ("fixture-S09", "stakes.lines"),
    ] {
        assert_eq!(outcome(&report, id, check).as_deref(), Some("pass"), "{id}");
    }
}

#[test]
fn a_zero_hit_stakes_entry_fails_while_the_run_exits_0_and_passes_at_exit_1() {
    let h = frozen_answer_harness();
    h.write_exit_code(ABSENT, &[], 0);

    assert_exit(&h.run(), 0);

    let report = h.report();
    assert_eq!(
        outcome(&report, "fixture-S01", "stakes.exit").as_deref(),
        Some("fail")
    );
    let d = detail_of(&report, "fixture-S01", "stakes.exit");
    assert!(d.contains("exit code is 0, expected 1"), "{d}");

    h.write_exit_code(ABSENT, &[], 1);
    assert_exit(&h.run(), 0);
    assert_eq!(
        outcome(&h.report(), "fixture-S01", "stakes.exit").as_deref(),
        Some("pass")
    );
}

/// The ledger of the failures the stub gives `fixture-S01` and `fixture-C03`.
const FROZEN_ANSWER_LEDGER: &str = r##"[[xfail]]
issue = "#552"
check = "stakes.exit"
ids = ["fixture-S01"]

[[xfail]]
issue = "#556"
check = "concept.first_right_top5"
ids = ["fixture-C03"]
"##;

#[test]
fn the_new_hard_failures_are_ledgered_by_id_and_a_first_bless_needs_no_reason() {
    let h = frozen_answer_harness();
    h.write_golden_with(
        DEF_LINE,
        &format!(
            "{}{}",
            frozen_answer_entries(),
            concept_with_first("fixture-C03", "src/marker.rs")
        ),
    );
    h.write_exit_code(ABSENT, &[], 0);

    // Without a ledger the gate names the new failures.
    let unledgered = h.check();
    assert_exit(&unledgered, 1);
    let err = stderr(&unledgered);
    for check in ["concept.first_right_top5", "stakes.exit"] {
        assert!(err.contains(check), "{check}: {err}");
    }

    // Ledgered, they are `xfail`; a first bless takes them with no reason.
    h.write_ledger(FROZEN_ANSWER_LEDGER);
    assert_exit(&h.run(), 0);
    assert_eq!(
        outcome(&h.report(), "fixture-S01", "stakes.exit").as_deref(),
        Some("xfail")
    );
    assert_exit(&h.bless(None), 0);
    assert_exit(&h.check(), 0);
    assert_eq!(h.report()["gate"]["status"], "pass");

    // When the engine fixes one, the ledger entry is an XPASS until removed.
    h.write_exit_code(ABSENT, &[], 1);
    let fixed = h.check();
    assert_exit(&fixed, 1);
    let err = stderr(&fixed);
    assert!(
        err.contains("XPASS") && err.contains("stakes.exit"),
        "{err}"
    );
}

#[test]
fn ident_def_first_fails_when_the_first_definition_is_another_line_of_the_right_file() {
    let h = Harness::new();
    assert_exit(&h.run(), 0);
    assert_eq!(
        outcome(&h.report(), "fixture-L01", "ident.def_first").as_deref(),
        Some("pass"),
        "the definition line is the first definition site"
    );

    // The right file ranks first, but its definition site is line 4.
    let mut rows = h.correct_rows(IDENT.1, Some(DEF_PATH));
    rows[0].line = 4;
    rows[0].content = "pub fn check_staleness() {}".to_string();
    h.write_ident(&rows);
    assert_exit(&h.run(), 0);

    let report = h.report();
    assert_eq!(
        outcome(&report, "fixture-L01", "ident.def_first").as_deref(),
        Some("fail")
    );
    let d = detail_of(&report, "fixture-L01", "ident.def_first");
    assert!(
        d.contains("src/staleness.rs:4") && d.contains("expected src/staleness.rs:2"),
        "{d}"
    );
}

// ============================================================================
// Frozen golden data of the four corpora (#540)
// ============================================================================

/// The four corpora of `corpora.toml`.
const CORPORA: [&str; 4] = ["skim", "ripgrep", "flask", "zod"];

/// The scoreboard data dir of this crate (`corpora.toml`, `golden/`).
fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scoreboard")
}

/// The raw text of the checked-in golden file of `corpus`.
fn real_golden_text(corpus: &str) -> String {
    let path = data_dir().join("golden").join(format!("{corpus}.toml"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn each_golden_file_loads_and_holds_at_least_three_refs_entries() {
    for corpus in CORPORA {
        let golden =
            parse_golden(&real_golden_text(corpus)).unwrap_or_else(|e| panic!("{corpus}: {e:#}"));
        assert_eq!(golden.corpus, corpus);
        assert!(
            golden.refs.len() >= 3,
            "{corpus} holds {} [[refs]] entries, expected 3 or more",
            golden.refs.len()
        );
    }
}

#[test]
fn the_skim_golden_file_freezes_both_definitions_of_run_parsed_command() {
    let golden = parse_golden(&real_golden_text("skim")).unwrap();
    let clash = golden
        .clashes
        .iter()
        .find(|c| c.query == "run_parsed_command")
        .expect("no [[clash]] entry for run_parsed_command");

    let mut defs: Vec<(&str, u32)> = clash
        .defs
        .iter()
        .map(|d| (d.path.as_str(), d.line))
        .collect();
    defs.sort_unstable();
    assert_eq!(
        defs,
        vec![
            ("crates/rskim/src/cmd/build/mod.rs", 150),
            ("crates/rskim/src/cmd/git/mod.rs", 414),
        ]
    );
    assert_eq!(clash.split, ClashSplit::None);
    assert!(!clash.calls.is_empty(), "the clash freezes no call site");
}

#[test]
fn each_golden_file_holds_a_clash_entry() {
    for corpus in CORPORA {
        let golden =
            parse_golden(&real_golden_text(corpus)).unwrap_or_else(|e| panic!("{corpus}: {e:#}"));
        assert!(
            !golden.clashes.is_empty(),
            "{corpus} holds no [[clash]] entry, expected 1 or more"
        );
    }
}

#[test]
fn each_golden_file_holds_two_concept_entries_with_first_files() {
    for corpus in CORPORA {
        let golden = parse_golden(&real_golden_text(corpus)).unwrap();
        let with_first = golden
            .concepts
            .iter()
            .filter(|c| c.first.as_ref().is_some_and(|first| !first.is_empty()))
            .count();
        assert!(
            with_first >= 2,
            "{corpus} holds {with_first} concept entries with `first`, expected 2 or more"
        );
    }
}

#[test]
fn each_golden_file_holds_a_stakes_entry_for_every_flag_and_every_exit_code() {
    for corpus in CORPORA {
        let golden = parse_golden(&real_golden_text(corpus)).unwrap();
        for flag in ["-i", "-E", "-l", "-c", "-g", "-t", "-w", "-C"] {
            assert!(
                golden.stakes.iter().any(|s| s.has_flag(flag)),
                "{corpus} holds no [[stakes]] entry with {flag}"
            );
        }
        for exit in [0, 1, 2] {
            assert!(
                golden
                    .stakes
                    .iter()
                    .any(|s| s.expect == StakesExpect::Exit && s.exit == Some(exit)),
                "{corpus} holds no [[stakes]] entry with exit = {exit}"
            );
        }
        for expect in [
            StakesExpect::Lines,
            StakesExpect::Files,
            StakesExpect::Counts,
        ] {
            assert!(
                golden.stakes.iter().any(|s| s.expect == expect),
                "{corpus} holds no [[stakes]] entry with expect = {expect:?}"
            );
        }
    }
}

/// Golden integrity of the checked-in files against the pinned clones. It
/// runs no skim: it loads each golden file and each clone, and applies the
/// same `check_integrity` the scoreboard applies before its first query.
///
/// Run it after a golden edit, with the clones in `.bench-corpus/scoreboard`:
/// `cargo nextest run -p rskim-bench --run-ignored only the_golden_files_pass_integrity`.
#[test]
#[ignore = "needs the pinned corpus clones under .bench-corpus/scoreboard"]
fn the_golden_files_pass_integrity_against_the_pinned_clones() {
    let specs = load_corpora(&data_dir().join("corpora.toml")).unwrap();
    let clones = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.bench-corpus/scoreboard");
    let home = tempfile::tempdir().unwrap();
    let git = GitIsolation::new(home.path());

    for spec in &specs {
        let root = clones.join(&spec.name);
        let state = verify_pinned_clone(&root, &spec.commit).unwrap();
        assert!(
            state.is_reusable(),
            "{}: the clone is not the pinned one: {state}",
            spec.name
        );
        let golden = parse_golden(&real_golden_text(&spec.name)).unwrap();
        let text = text_universe(&root, &git).unwrap();
        let violations = check_integrity(
            &golden,
            &IntegrityContext {
                corpus: &spec.name,
                commit: &spec.commit,
                universe: Some(&text),
                ledger: &[],
            },
        );
        assert!(
            violations.is_empty(),
            "{}: {} integrity violation(s):\n{}",
            spec.name,
            violations.len(),
            violations
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
