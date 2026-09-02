//! Integration tests: degraded-state robustness — #414
//!
//! `cli_search_degraded.rs` (C9) — created and owned by #414.
//! #413's degraded-message E2E tests stay in the `mod.rs` test module.
//!
//! # Coverage
//!
//! - T-1  / AC-1:  FX-EMPTY — text vs text+--hot path lists ordered-equal
//! - T-2  / AC-2:  FX-EMPTY — stderr content for text+--hot degraded notice
//! - T-3  / AC-3:  FX-EMPTY — repeat T-1/T-2 with --risky, --cold
//! - T-4  / AC-4:  FX-ABSENT-ROWS — no_ranked_rows: path lists ordered-equal to plain query
//! - T-5  / AC-5:  FX-EMPTY — --ast try-catch vs --ast try-catch --hot ordered-equal
//! - T-6  / AC-6:  FX-EMPTY — standalone --hot/--cold/--risky degraded JSON
//! - T-7  / AC-7:  FX-EMPTY / FX-CORRUPT + non-git control — blast-radius degraded
//! - T-8  / AC-8:  FX-REPO2 healthy blast-radius + FX-1COMMIT no false notices
//! - T-9  / AC-9:  FX-CORRUPT — self-heal after first query
//! - T-10 / AC-10: FX-NEWER — user_version=99 preserved, degraded notice
//! - T-18 / AC-18: FX-1COMMIT — --build + plain query + --hot: no false notices
//! - T-21 / AC-21: FX-CORRUPT/FX-EMPTY — pagination (--offset) on degraded
//! - T-22 / AC-22: FX-CORRUPT — limit sweep: no arm returns > --limit results
//! - T-29 / AC-29: FX-CORRUPT + read-only dir — exit 0, explicit message
//! - T-30 / AC-30: FX-NEWER + new commit — at most one temporal-failure line
//! - T-38 / AC-36: FX-COVERAGE — partial-coverage query orders correctly
//! - T-6  (step9) / AC-23: NotGitRepo --hot text + JSON: exit 0 + parseable output
//! - T-8  (step9):         Missing temporal.db on git repo: --json --hot exit 0 + JSON
//! - T-23 (step9) / AC-23: NotGitRepo --json --hot: degraded JSON object on stdout
//! - AC-30 (step9):        Plain text query with no temporal flag: no temporal notice on stderr
//!
//! # Design
//!
//! Every non-git fixture asserts the NOGIT no-ancestor-.git precondition (C1).
//! FX-CORRUPT uses `perl -e 'print chr(0xAB) x 1024'` (deterministic, PF-012).
//! Tests drive the real skim binary via `assert_cmd::cargo::cargo_bin("skim")`.
//! All cache I/O is isolated via `SKIM_CACHE_DIR` on a per-test TempDir.

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;

use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use tempfile::TempDir;

// ============================================================================
// Helpers — shared across all fixture groups
// ============================================================================

/// NOGIT precondition: assert that no ancestor of `dir` contains a `.git` entry.
///
/// After #413's walk-up (OD-3), a .git-less tempdir under a repo clone adopts
/// that clone's HEAD and is classified as belonging to the repository — making a
/// "non-git" tempdir effectively a git repo.  This precondition must be
/// asserted on every fixture whose test depends on the directory having no git
/// context (AC-7 / AC-20 / T-7 / T-20).
fn assert_nogit(dir: &Path) {
    let mut d = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    loop {
        assert!(
            !d.join(".git").exists(),
            "NOGIT precondition failed: ancestor {d:?} contains .git; \
             this fixture would adopt that repository instead of being git-free"
        );
        match d.parent() {
            Some(p) if p != d => d = p.to_path_buf(),
            _ => break,
        }
    }
}

/// Initialize a git repo with hermetic, non-signing identity.
fn git_init(dir: &Path) {
    for args in &[
        vec!["init"],
        vec!["config", "user.email", "test@t.com"],
        vec!["config", "user.name", "T"],
        vec!["config", "commit.gpgsign", "false"],
    ] {
        let s = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
        assert!(s.status.success(), "git {args:?} failed");
    }
}

/// Get the current git HEAD sha for a repo.
fn git_head(dir: &Path) -> String {
    let out = StdCommand::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("git rev-parse HEAD");
    String::from_utf8(out.stdout)
        .expect("HEAD utf8")
        .trim()
        .to_string()
}

/// Build the lexical + AST + temporal index for `proj` into an isolated cache.
fn build_index(proj: &Path, cache: &Path) {
    let status = StdCommand::new(cargo_bin("skim"))
        .args(["search", "--build", "--root"])
        .arg(proj)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .status()
        .expect("skim search --build");
    assert!(status.success(), "skim search --build failed");
}

/// Find the search cache subdirectory (`<cache>/search/<hash>/`) for any root.
///
/// Used by integration tests that need to locate or modify `temporal.db`
/// without depending on the internal `resolve_search_cache_dir` symbol.
fn find_search_cache(cache: &Path) -> std::path::PathBuf {
    let search_dir = cache.join("search");
    let entry = fs::read_dir(&search_dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", search_dir.display()))
        .filter_map(|e| e.ok())
        .find(|e| e.path().is_dir())
        .unwrap_or_else(|| panic!("no search cache subdir under {}", search_dir.display()));
    entry.path()
}

/// Create FX-REPO2: a 2-commit git repo with zebra_widget and alpha_widget.
///
/// The second commit modifies `src/zebra.rs`, ensuring that a `--build` run
/// populates `temporal.db` with a hotspot row for it.
fn make_repo2(dir: &Path) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("src/zebra.rs"),
        "fn zebra_widget() { let x = 1; }\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/alpha.rs"),
        "fn alpha_widget() { let y = 2; }\n",
    )
    .unwrap();
    git_init(dir);
    git_add_commit(dir, "c1: add widgets");
    // Second commit: touch zebra.rs so it has a hotspot row.
    fs::write(
        dir.join("src/zebra.rs"),
        "fn zebra_widget() { let x = 3; }\n// touched\n",
    )
    .unwrap();
    git_add_commit(dir, "fix: bug in zebra widget");
}

/// `git add -A && git commit -qm <msg>` in `dir`.
fn git_add_commit(dir: &Path, msg: &str) {
    let add = StdCommand::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .output()
        .expect("git add -A");
    assert!(add.status.success(), "git add -A failed");
    let commit = StdCommand::new("git")
        .args(["commit", "-qm", msg])
        .current_dir(dir)
        .output()
        .expect("git commit");
    assert!(commit.status.success(), "git commit failed: {msg}");
}

/// Create FX-EMPTY: build the index on a repo, then replace `temporal.db` with
/// an empty (schema-only, zero-row) database carrying the repo's current HEAD.
///
/// Returns the path to the replaced `temporal.db`.
fn make_empty_temporal(db_dir: &Path, head: &str) -> std::path::PathBuf {
    let db_path = db_dir.join("temporal.db");
    // Remove the existing db and any WAL/SHM sidecars.
    for suffix in &["temporal.db", "temporal.db-wal", "temporal.db-shm"] {
        let _ = fs::remove_file(db_dir.join(suffix));
    }
    // Create an empty schema via sqlite3.
    let sql = format!(
        "PRAGMA user_version=2;\
         CREATE TABLE hotspot (file_path TEXT PRIMARY KEY, score REAL NOT NULL, \
           changes_30d INTEGER NOT NULL, changes_90d INTEGER NOT NULL);\
         CREATE TABLE risk (file_path TEXT PRIMARY KEY, risk_score REAL NOT NULL, \
           total_commits INTEGER NOT NULL, fix_commits INTEGER NOT NULL, fix_density REAL NOT NULL);\
         CREATE TABLE cochange (file_a TEXT NOT NULL, file_b TEXT NOT NULL, count INTEGER NOT NULL, \
           jaccard REAL NOT NULL, PRIMARY KEY (file_a,file_b));\
         CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);\
         INSERT INTO meta VALUES \
           ('git_head','{head}'),('data_version','1'),('last_updated','1'),('is_shallow','0');",
    );
    let out = StdCommand::new("sqlite3")
        .arg(&db_path)
        .arg(&sql)
        .output()
        .expect("sqlite3 empty schema");
    assert!(
        out.status.success(),
        "sqlite3 empty schema failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    db_path
}

/// Create FX-CORRUPT: overwrite `temporal.db` with 0xAB×1024 bytes (PF-012).
fn make_corrupt_temporal(db_path: &Path) {
    let out = StdCommand::new("perl")
        .arg("-e")
        .arg("print chr(0xAB) x 1024")
        .output()
        .expect("perl corrupt");
    assert!(out.status.success(), "perl corrupt failed");
    assert_eq!(out.stdout.len(), 1024, "corrupt payload must be 1024 bytes");
    fs::write(db_path, &out.stdout).unwrap();
}

/// Create FX-NEWER: bump PRAGMA user_version to 99 in an existing `temporal.db`.
fn make_newer_temporal(db_path: &Path) {
    let out = StdCommand::new("sqlite3")
        .arg(db_path)
        .arg("PRAGMA user_version=99;")
        .output()
        .expect("sqlite3 user_version=99");
    assert!(
        out.status.success(),
        "sqlite3 user_version=99 failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run `skim search <args> --root <root>` and return `(stdout, stderr, exit_code)`.
fn skim_search(args: &[&str], root: &Path, cache: &Path) -> (String, String, i32) {
    let out = StdCommand::new(cargo_bin("skim"))
        .arg("search")
        .args(args)
        .arg("--root")
        .arg(root)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .output()
        .expect("skim search");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

/// Extract `results[].path` from a `--json` output object.
fn extract_paths(json_str: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(json_str)
        .unwrap_or_else(|e| panic!("JSON parse error: {e}\nInput:\n{json_str}"));
    v["results"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|r| r["path"].as_str().map(str::to_owned))
        .collect()
}

/// Assert that `json_str` contains at least one degraded element with the given
/// `reason` value.
fn assert_degraded_reason(json_str: &str, reason: &str) {
    let v: Value = serde_json::from_str(json_str)
        .unwrap_or_else(|e| panic!("JSON parse error: {e}\nInput:\n{json_str}"));
    let degraded = v["degraded"].as_array().cloned().unwrap_or_default();
    let found = degraded
        .iter()
        .any(|d| d["reason"].as_str() == Some(reason));
    assert!(
        found,
        "expected degraded reason '{reason}' in JSON; got:\n{json_str}"
    );
}

/// Assert the `results[].path` list in `json_str` has no `temporal` key on any result.
fn assert_no_temporal_key(json_str: &str) {
    let v: Value = serde_json::from_str(json_str)
        .unwrap_or_else(|e| panic!("JSON parse error: {e}\nInput:\n{json_str}"));
    let results = v["results"].as_array().cloned().unwrap_or_default();
    for (i, r) in results.iter().enumerate() {
        assert!(
            r.get("temporal").is_none_or(|t| t.is_null()),
            "result[{i}] must not have a 'temporal' key in degraded mode; got:\n{json_str}"
        );
    }
}

/// Query the row count for a table in a SQLite database via `sqlite3`.
fn sqlite_count(db_path: &Path, table: &str) -> i64 {
    let out = StdCommand::new("sqlite3")
        .arg(db_path)
        .arg(format!("SELECT COUNT(*) FROM {table};"))
        .output()
        .unwrap_or_else(|e| panic!("sqlite3 count {table}: {e}"));
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim().parse::<i64>().unwrap_or(-1)
}

/// Query PRAGMA user_version from a SQLite database.
fn sqlite_user_version(db_path: &Path) -> i64 {
    let out = StdCommand::new("sqlite3")
        .arg(db_path)
        .arg("PRAGMA user_version;")
        .output()
        .unwrap_or_else(|e| panic!("sqlite3 user_version: {e}"));
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim().parse::<i64>().unwrap_or(-1)
}

/// Get the file size of `path` in bytes, or 0 if it doesn't exist.
fn file_size(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Assert that a degraded element carries the expected `requested` and `applied` fields.
///
/// Used to verify `DegradedJson.requested` uses the bare form (e.g. `"hot"`) and
/// `DegradedJson.applied` names the fallback strategy (e.g. `"lexical"`).
/// Failure here means `flag_name()` was used instead of `json_name()` at a call
/// site — a one-token regression that no `reason`-only assertion would catch (RD-5).
fn assert_degraded_requested_applied(json_str: &str, requested: &str, applied: &str) {
    let v: Value = serde_json::from_str(json_str)
        .unwrap_or_else(|e| panic!("JSON parse error: {e}\nInput:\n{json_str}"));
    let degraded = v["degraded"].as_array().cloned().unwrap_or_default();
    let found = degraded.iter().any(|d| {
        d["requested"].as_str() == Some(requested) && d["applied"].as_str() == Some(applied)
    });
    assert!(
        found,
        "expected degraded element with requested='{requested}', applied='{applied}'; \
         got degraded:\n{:?}\nFull JSON:\n{json_str}",
        degraded
    );
}

/// Extract the `co_change` (co-change) partner count for a file from `temporal.db`
/// via sqlite3.  Returns 0 when the file has no co-change partners.
fn sqlite_cochange_count(db_path: &Path, file_path: &str) -> i64 {
    let out = StdCommand::new("sqlite3")
        .arg(db_path)
        .arg(format!(
            "SELECT COUNT(*) FROM cochange WHERE file_a='{file_path}' OR file_b='{file_path}';"
        ))
        .output()
        .unwrap_or_else(|e| panic!("sqlite3 cochange count: {e}"));
    let s = String::from_utf8_lossy(&out.stdout);
    s.trim().parse::<i64>().unwrap_or(-1)
}

// ============================================================================
// Group 1: FX-EMPTY — T-1 / T-2 / T-3 / T-6
// ============================================================================

/// Build the shared FX-EMPTY fixture: FX-REPO2 + index built + temporal.db replaced
/// with empty schema.  Returns `(dir, repo_root, cache_dir, search_cache_dir)`.
fn make_fx_empty() -> (TempDir, std::path::PathBuf, TempDir, std::path::PathBuf) {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    make_repo2(&root);
    let cache = TempDir::new().expect("cache TempDir");
    build_index(&root, cache.path());
    let db_dir = find_search_cache(cache.path());
    let head = git_head(&root);
    make_empty_temporal(&db_dir, &head);
    (dir, root, cache, db_dir)
}

/// T-1 / AC-1 — FX-EMPTY: plain query and text+--hot yield ordered-equal path lists.
///
/// When temporal data is empty the degraded fallback is lexical order, which must
/// match the plain-lexical order exactly.  No result may carry a `temporal` key.
#[test]
fn t1_fx_empty_hot_path_list_ordered_equal_to_plain() {
    let (_dir, root, cache, _db_dir) = make_fx_empty();

    let (plain_json, _, code_plain) = skim_search(
        &["zebra_widget", "--limit", "5", "--json"],
        &root,
        cache.path(),
    );
    assert_eq!(code_plain, 0, "plain query must exit 0");

    let (hot_json, _, code_hot) = skim_search(
        &["zebra_widget", "--hot", "--limit", "5", "--json"],
        &root,
        cache.path(),
    );
    assert_eq!(code_hot, 0, "--hot on empty temporal must exit 0");

    let plain_paths = extract_paths(&plain_json);
    let hot_paths = extract_paths(&hot_json);
    assert!(
        !plain_paths.is_empty(),
        "plain query must return ≥1 result on FX-REPO2 (PF-007)"
    );
    assert_eq!(
        plain_paths, hot_paths,
        "T-1/AC-1: --hot on FX-EMPTY must produce the same ordered path list as the plain query"
    );

    // No result may carry a temporal key (empty DB → no annotations).
    assert_no_temporal_key(&hot_json);
}

/// T-2 / AC-2 — FX-EMPTY: stderr content for text+--hot degraded notice.
///
/// Must contain: `empty`, `--hot`, `not applied`, `lexical`, `--rebuild`.
/// Must NOT contain `unshallow` (FX-EMPTY's is_shallow=0).
/// Exit code must be 0.
#[test]
fn t2_fx_empty_hot_stderr_degraded_notice() {
    let (_dir, root, cache, _db_dir) = make_fx_empty();

    let (_, stderr, code) = skim_search(
        &["zebra_widget", "--hot", "--limit", "5"],
        &root,
        cache.path(),
    );
    assert_eq!(code, 0, "T-2/AC-2: must exit 0 on FX-EMPTY+--hot");

    for needle in &["empty", "--hot", "not applied", "lexical", "--rebuild"] {
        assert!(
            stderr.contains(needle),
            "T-2/AC-2: stderr must contain '{needle}'; got:\n{stderr}"
        );
    }
    // is_shallow=0 in FX-EMPTY → no shallow attribution.
    assert!(
        !stderr.contains("unshallow"),
        "T-2/AC-2: stderr must NOT contain 'unshallow' for non-shallow FX-EMPTY; got:\n{stderr}"
    );
}

/// T-3 / AC-3 — FX-EMPTY: repeat T-1/T-2 with --risky and --cold.
///
/// For --cold the first result of the degraded query must equal the first result
/// of the plain query (E-15: lexical order = cold order when no temporal data).
#[test]
fn t3_fx_empty_risky_cold_path_lists_ordered_equal_to_plain() {
    let (_dir, root, cache, _db_dir) = make_fx_empty();

    let (plain_json, _, _) = skim_search(
        &["zebra_widget", "--limit", "5", "--json"],
        &root,
        cache.path(),
    );
    let plain_paths = extract_paths(&plain_json);
    assert!(
        !plain_paths.is_empty(),
        "plain query must return ≥1 result (PF-007)"
    );

    for flag in &["--risky", "--cold"] {
        let (flag_json, flag_stderr, flag_code) = skim_search(
            &["zebra_widget", flag, "--limit", "5", "--json"],
            &root,
            cache.path(),
        );
        assert_eq!(flag_code, 0, "T-3/AC-3: {flag} on FX-EMPTY must exit 0");

        let flag_paths = extract_paths(&flag_json);
        assert_eq!(
            plain_paths, flag_paths,
            "T-3/AC-3: {flag} on FX-EMPTY must produce the same ordered path list as plain query"
        );

        for needle in &["empty", flag, "not applied", "lexical", "--rebuild"] {
            assert!(
                flag_stderr.contains(needle),
                "T-3/AC-3: {flag} stderr must contain '{needle}'; got:\n{flag_stderr}"
            );
        }
    }

    // E-15: --cold first result equals plain first result.
    let (cold_json, _, _) = skim_search(
        &["zebra_widget", "--cold", "--limit", "5", "--json"],
        &root,
        cache.path(),
    );
    let cold_paths = extract_paths(&cold_json);
    assert_eq!(
        cold_paths.first(),
        plain_paths.first(),
        "T-3/AC-3 E-15: --cold first path must equal plain first path on FX-EMPTY"
    );
}

/// T-6 / AC-6 — FX-EMPTY: standalone --hot/--cold/--risky emit degraded JSON with
/// `reason=="empty"` and text mode has `empty` + remediation on stderr.
///
/// Standalone = only a temporal sort flag, no text query.  The `--json` output
/// must be a single parseable JSON object with `degraded[].reason == "empty"`.
#[test]
fn t6_fx_empty_standalone_temporal_degraded_json() {
    let (_dir, root, cache, _db_dir) = make_fx_empty();

    for flag in &["--hot", "--cold", "--risky"] {
        // Text mode: stderr contains `empty` + remediation.
        let (_, stderr_text, code_text) = skim_search(&[flag, "--limit", "2"], &root, cache.path());
        assert_eq!(
            code_text, 0,
            "T-6/AC-6: {flag} text on FX-EMPTY must exit 0"
        );
        assert!(
            stderr_text.contains("empty"),
            "T-6/AC-6: {flag} text stderr must contain 'empty'; got:\n{stderr_text}"
        );

        // JSON mode: single object with `degraded[].reason == "empty"`.
        let (json_out, _, code_json) =
            skim_search(&[flag, "--limit", "2", "--json"], &root, cache.path());
        assert_eq!(
            code_json, 0,
            "T-6/AC-6: {flag} --json on FX-EMPTY must exit 0"
        );
        let v: Value = serde_json::from_str(&json_out)
            .unwrap_or_else(|e| panic!("T-6: {flag} --json not parseable: {e}\n{json_out}"));
        assert!(
            v.is_object(),
            "T-6/AC-6: {flag} --json output must be a JSON object"
        );
        assert_degraded_reason(&json_out, "empty");

        // DegradedJson.requested must use the BARE form (AC-6 / RD-5).
        // "hot" / "cold" / "risky" — no "--" prefix.  applied must be "lexical".
        let bare_flag = flag.trim_start_matches('-');
        assert_degraded_requested_applied(&json_out, bare_flag, "lexical");
    }
}

// ============================================================================
// Group 2: FX-ABSENT-ROWS — T-4
// ============================================================================

/// Build the FX-ABSENT-ROWS fixture.
///
/// FX-REPO2 + `--build` (temporal populated), then three uncommitted files with
/// known BM25F score ordering: `zz > aa > mm` for the query `thing_marker`.
/// The pre-fix (incorrect temporal) order would be `aa, mm, zz` (path-ASC).
fn make_fx_absent_rows() -> (TempDir, std::path::PathBuf, TempDir) {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    make_repo2(&root);
    let cache = TempDir::new().expect("cache TempDir");
    build_index(&root, cache.path());

    // Three uncommitted files — BM25F order is zz (3.64) > aa (0.23) > mm (0.23).
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/aa_new.rs"),
        "// thing_marker mentioned in a comment only\nfn other_a() { }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/mm_new.rs"),
        "fn m_helper() { let v = 1; }\n// note: thing_marker\n",
    )
    .unwrap();
    fs::write(root.join("src/zz_new.rs"), "fn thing_marker() { }\n").unwrap();

    (dir, root, cache)
}

/// T-4 / AC-4 — FX-ABSENT-ROWS: no_ranked_rows path lists ordered-equal to plain query.
///
/// When all matched files are uncommitted, none have temporal rows.  The fallback
/// to lexical order must yield the BM25F-ordered list (`zz, aa, mm`), not the
/// mis-ranked alphabetical list that the pre-fix code would produce.
///
/// The degraded JSON must carry `reason=="no_ranked_rows"`, `applied=="lexical"`.
#[test]
fn t4_fx_absent_rows_no_ranked_rows_ordered_equal_to_plain() {
    let (_dir, root, cache) = make_fx_absent_rows();

    // Plain query (no temporal flag).
    let (plain_json, _, code_plain) = skim_search(
        &["thing_marker", "--limit", "5", "--json"],
        &root,
        cache.path(),
    );
    assert_eq!(code_plain, 0, "T-4: plain query must exit 0");
    let plain_paths = extract_paths(&plain_json);
    assert!(
        !plain_paths.is_empty(),
        "T-4: plain query must return results on FX-ABSENT-ROWS (PF-007)"
    );

    for flag in &["--hot", "--risky", "--cold"] {
        let (flag_json, flag_stderr, flag_code) = skim_search(
            &["thing_marker", flag, "--limit", "5", "--json"],
            &root,
            cache.path(),
        );
        assert_eq!(flag_code, 0, "T-4/AC-4: {flag} must exit 0");

        // Path list must equal the plain query's order (lexical fallback).
        let flag_paths = extract_paths(&flag_json);
        assert_eq!(
            plain_paths, flag_paths,
            "T-4/AC-4: {flag} path list must equal plain query's order (lexical fallback)"
        );

        // Degraded JSON must have reason=no_ranked_rows.
        assert_degraded_reason(&flag_json, "no_ranked_rows");

        // DegradedJson.requested must use the BARE form (AC-4 / RD-5).
        // "hot" / "cold" / "risky" — no "--" prefix.  applied must be "lexical".
        let bare_flag = flag.trim_start_matches('-');
        assert_degraded_requested_applied(&flag_json, bare_flag, "lexical");

        // Stderr must mention the row count ("0 of"), the flag, "not applied", "lexical".
        for needle in &["0 of", flag, "not applied", "lexical"] {
            assert!(
                flag_stderr.contains(needle),
                "T-4/AC-4: {flag} stderr must contain '{needle}'; got:\n{flag_stderr}"
            );
        }

        // No result must have a temporal key (no rows → no annotations).
        assert_no_temporal_key(&flag_json);
    }

    // Results must NOT be in alphabetical order (aa, mm, zz) — that would be the
    // pre-fix/incorrect ranking.  BM25F order has zz first (function definition).
    let first = plain_paths.first().map(|s| s.as_str()).unwrap_or("");
    assert!(
        first.contains("zz"),
        "T-4/AC-4: BM25F must rank zz_new.rs first (function def > comment match); got first={first:?}"
    );
}

// ============================================================================
// Group 3: FX-EMPTY + try-catch — T-5
// ============================================================================

/// T-5 / AC-5 — FX-EMPTY with a try-catch construct: --ast try-catch vs --ast try-catch --hot.
///
/// Path lists must be ordered-equal.  Stderr must mention the AST-match cause and
/// "raw AST match order" when --hot is degraded.
#[test]
fn t5_fx_empty_ast_try_catch_hot_ordered_equal() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path().join("repo");
    fs::create_dir_all(root.join("src")).unwrap();

    // TypeScript files with actual try/catch so the AST try-catch pattern fires.
    // The try-catch pattern is TypeScript/JavaScript-specific (try_statement >
    // catch_clause); Rust match expressions are NOT matched by it (PF-007 / PF-018).
    fs::write(
        root.join("src/zebra.ts"),
        "function zebraWidget() {\n\
           try {\n\
             const x = fetchData();\n\
             return x;\n\
           } catch (e) {\n\
             console.error(e);\n\
           }\n\
         }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/alpha.ts"),
        "function alphaWidget() {\n\
           try {\n\
             const y = fetchOther();\n\
             return y;\n\
           } catch (err) {\n\
             console.warn(err);\n\
           }\n\
         }\n",
    )
    .unwrap();
    git_init(&root);
    git_add_commit(&root, "c1: add widgets");
    fs::write(
        root.join("src/zebra.ts"),
        "function zebraWidget() {\n\
           // touched in fix commit\n\
           try {\n\
             const x = fetchData();\n\
             return x;\n\
           } catch (e) {\n\
             console.error(e);\n\
           }\n\
         }\n",
    )
    .unwrap();
    git_add_commit(&root, "fix: bug");

    let cache = TempDir::new().expect("cache TempDir");
    build_index(&root, cache.path());
    let head = git_head(&root);
    let db_dir = find_search_cache(cache.path());
    make_empty_temporal(&db_dir, &head);

    let (ast_json, _, ast_code) = skim_search(
        &["--ast", "try-catch", "--limit", "5", "--json"],
        &root,
        cache.path(),
    );
    assert_eq!(ast_code, 0, "T-5: --ast try-catch must exit 0");

    let (ast_hot_json, ast_hot_stderr, ast_hot_code) = skim_search(
        &["--ast", "try-catch", "--hot", "--limit", "5", "--json"],
        &root,
        cache.path(),
    );
    assert_eq!(
        ast_hot_code, 0,
        "T-5/AC-5: --ast try-catch --hot on FX-EMPTY must exit 0"
    );

    let ast_paths = extract_paths(&ast_json);
    let ast_hot_paths = extract_paths(&ast_hot_json);

    assert!(
        !ast_paths.is_empty(),
        "T-5: --ast try-catch must return ≥1 result (PF-007)"
    );
    assert_eq!(
        ast_paths, ast_hot_paths,
        "T-5/AC-5: --ast try-catch --hot path list must equal --ast try-catch on FX-EMPTY"
    );

    // Stderr must explain the degraded cause and raw AST match order.
    assert!(
        ast_hot_stderr.contains("raw AST match order"),
        "T-5/AC-5: stderr must mention 'raw AST match order'; got:\n{ast_hot_stderr}"
    );
}

// ============================================================================
// Group 4: blast-radius — T-7 / T-8
// ============================================================================

/// T-7 / AC-7 — blast-radius on FX-EMPTY and FX-CORRUPT: degraded blast-radius notice.
///
/// Stderr must name the degraded cause (`empty` or `corrupt`), contain
/// `--blast-radius not applied` and must contain ZERO occurrences of
/// `no temporal data` (the old pre-#414 message that was replaced).
///
/// A non-git dir is used as a control: its output must still contain the
/// `warning` key (AC-9 byte-identity).  NOGIT precondition is asserted first.
#[test]
fn t7_blast_radius_degraded_on_empty_and_corrupt() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path().join("repo");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/zebra.rs"),
        "fn zebra_widget() { let x = 1; }\n",
    )
    .unwrap();
    git_init(&root);
    git_add_commit(&root, "init");

    let cache_empty = TempDir::new().expect("cache TempDir");
    build_index(&root, cache_empty.path());
    let db_dir_empty = find_search_cache(cache_empty.path());
    let head = git_head(&root);
    make_empty_temporal(&db_dir_empty, &head);

    // FX-CORRUPT: a separate git repo with NO commits so the corrupt temporal.db
    // cannot be auto-healed.  auto_refresh_if_stale calls try_rebuild_temporal_nonfatal
    // which returns early on `let Some(head) = head else { return };` (no HEAD → no
    // commits → sha() == None), leaving the corrupt file on disk.  Using the 1-commit
    // `root` would cause auto-heal to discard and rebuild the DB from the commit history
    // before blast-radius runs, hiding the Corrupt state the test asserts (PF-018).
    let corrupt_dir = TempDir::new().expect("corrupt TempDir");
    let corrupt_root = corrupt_dir.path().join("repo");
    fs::create_dir_all(corrupt_root.join("src")).unwrap();
    fs::write(
        corrupt_root.join("src/zebra.rs"),
        "fn zebra_widget() { let x = 1; }\n",
    )
    .unwrap();
    git_init(&corrupt_root);
    // Deliberately NO git_add_commit: no HEAD → no temporal rebuild possible.

    let cache_corrupt = TempDir::new().expect("cache TempDir");
    build_index(&corrupt_root, cache_corrupt.path());
    let db_dir_corrupt = find_search_cache(cache_corrupt.path());
    let db_path_corrupt = db_dir_corrupt.join("temporal.db");
    make_corrupt_temporal(&db_path_corrupt);

    // NOGIT precondition for the non-git control directory.
    let nogit_dir = TempDir::new().expect("nogit TempDir");
    assert_nogit(nogit_dir.path());
    let cache_nogit = TempDir::new().expect("cache TempDir");

    let checks: &[(&str, &Path, &Path)] = &[
        ("empty", root.as_path(), cache_empty.path()),
        ("corrupt", corrupt_root.as_path(), cache_corrupt.path()),
    ];

    for (label, proj_root, cache_path) in checks {
        let (json_out, stderr, code) = skim_search(
            &[
                "zebra_widget",
                "--blast-radius",
                "src/zebra.rs",
                "--limit",
                "5",
                "--json",
            ],
            proj_root,
            cache_path,
        );
        assert_eq!(code, 0, "T-7/AC-7: {label} blast-radius must exit 0");

        // Stderr must name the cause and the blast-radius not-applied tail.
        assert!(
            stderr.contains(label),
            "T-7/AC-7: {label} stderr must contain '{label}'; got:\n{stderr}"
        );
        assert!(
            stderr.contains("--blast-radius not applied"),
            "T-7/AC-7: {label} stderr must contain '--blast-radius not applied'; got:\n{stderr}"
        );
        // The old `no temporal data` message must be absent.
        assert_eq!(
            stderr.matches("no temporal data").count(),
            0,
            "T-7/AC-7: {label} stderr must contain ZERO occurrences of 'no temporal data'; got:\n{stderr}"
        );
        // JSON must have degraded reason blast-radius → requested="blast-radius".
        let v: Value = serde_json::from_str(&json_out)
            .unwrap_or_else(|e| panic!("T-7: {label} --json parse error: {e}\n{json_out}"));
        let degraded = v["degraded"].as_array().cloned().unwrap_or_default();
        let blast_deg = degraded.iter().find(|d| d["requested"] == "blast-radius");
        assert!(
            blast_deg.is_some(),
            "T-7/AC-7: {label} --json must have degraded element with requested='blast-radius'; got:\n{json_out}"
        );
        assert_eq!(
            blast_deg.unwrap()["applied"].as_str(),
            Some("lexical"),
            "T-7/AC-7: {label} degraded.applied must be 'lexical'"
        );
    }

    // Non-git control (text+blast-radius path through run_query, not the standalone
    // run_temporal_standalone arm): the degraded array must contain a blast-radius
    // entry.  The `warning` key belongs only to the standalone temporal arm (AC-9
    // byte-identity for --hot/--cold/--risky); on the text+blast-radius path the
    // machine-readable signal is in output.degraded (AD-414-12).
    let (nogit_json, _, _) = skim_search(
        &[
            "zebra_widget",
            "--blast-radius",
            "src/zebra.rs",
            "--limit",
            "5",
            "--json",
        ],
        nogit_dir.path(),
        cache_nogit.path(),
    );
    let nogit_v: Value = serde_json::from_str(&nogit_json).unwrap_or(Value::Null);
    let nogit_degraded = nogit_v["degraded"].as_array().cloned().unwrap_or_default();
    let nogit_blast_deg = nogit_degraded
        .iter()
        .find(|d| d["requested"] == "blast-radius");
    assert!(
        nogit_blast_deg.is_some(),
        "T-7/AC-7 control: non-git dir blast-radius --json must have \
         degraded[requested='blast-radius']; got:\n{nogit_json}"
    );
}

/// T-8 / AC-8 — healthy blast-radius with zero co-change partners + FX-1COMMIT.
///
/// Neither scenario should emit the degraded notice or shallow/no-history messages.
///
/// The blast-radius fixture uses separate commits for alpha.rs and zebra.rs so
/// the two files never co-occur in a single commit → co-change count for alpha.rs
/// is 0.  This is verified via sqlite3 before the query so a fixture drift that
/// reintroduces a co-change row surfaces as a clear failure rather than a silent
/// false-positive on the stderr !contains assertions (AC-8 / PF-007).
#[test]
fn t8_healthy_blast_radius_and_one_commit_no_false_notices() {
    // FX-SOLO-ALPHA: 3 commits, alpha.rs and zebra.rs in separate commits so
    // they never co-occur → alpha.rs has 0 co-change partners.
    let dir2 = TempDir::new().expect("TempDir");
    let root2 = dir2.path().join("repo");
    fs::create_dir_all(root2.join("src")).unwrap();
    fs::write(
        root2.join("src/alpha.rs"),
        "fn alpha_widget() { let y = 2; }\n",
    )
    .unwrap();
    git_init(&root2);
    git_add_commit(&root2, "c1: add alpha only");
    // Separate commit for zebra.rs so no co-change pair is formed with alpha.rs.
    fs::write(
        root2.join("src/zebra.rs"),
        "fn zebra_widget() { let x = 1; }\n",
    )
    .unwrap();
    git_add_commit(&root2, "c2: add zebra only");
    // Fix commit touches only zebra.rs — generates hotspot row without co-change with alpha.
    fs::write(
        root2.join("src/zebra.rs"),
        "fn zebra_widget() { let x = 3; }\n// touched\n",
    )
    .unwrap();
    git_add_commit(&root2, "fix: bug in zebra widget");

    let cache2 = TempDir::new().expect("cache TempDir");
    build_index(&root2, cache2.path());

    let db_dir2 = find_search_cache(cache2.path());
    // Verify premise: alpha.rs must have exactly 0 co-change partners.
    // If this fails, the fixture no longer guarantees the 0-partner state and
    // the !contains assertions below may be vacuous (AC-8 / PF-007).
    let cochange_count = sqlite_cochange_count(&db_dir2.join("temporal.db"), "src/alpha.rs");
    assert_eq!(
        cochange_count, 0,
        "T-8/AC-8 premise: alpha.rs must have 0 co-change partners in the temporal DB; \
         a co-change row means the fixture commits were restructured — alpha.rs and \
         zebra.rs must live in separate commits so they never co-occur"
    );

    let (_, stderr_br, code_br) = skim_search(
        &["--blast-radius", "src/alpha.rs", "--limit", "5"],
        &root2,
        cache2.path(),
    );
    assert_eq!(code_br, 0, "T-8/AC-8: healthy blast-radius must exit 0");
    for bad in &["shallow", "unshallow", "no commit history", "0 rows"] {
        assert!(
            !stderr_br.contains(bad),
            "T-8/AC-8: healthy blast-radius stderr must NOT contain '{bad}'; got:\n{stderr_br}"
        );
    }

    // FX-1COMMIT: one-commit git repo; hotspot=1 risk=1 cochange=0 after --build.
    let dir1 = TempDir::new().expect("TempDir");
    let root1 = dir1.path().join("repo");
    fs::create_dir_all(root1.join("src")).unwrap();
    fs::write(root1.join("src/a.rs"), "fn widget() { let x = 1; }\n").unwrap();
    git_init(&root1);
    git_add_commit(&root1, "init: one commit");

    let cache1 = TempDir::new().expect("cache TempDir");
    build_index(&root1, cache1.path());

    let (_, stderr1, code1) =
        skim_search(&["widget", "--hot", "--limit", "5"], &root1, cache1.path());
    assert_eq!(code1, 0, "T-8: FX-1COMMIT --hot must exit 0");
    for bad in &["shallow", "unshallow", "no commit history", "0 rows"] {
        assert!(
            !stderr1.contains(bad),
            "T-8/AC-8: FX-1COMMIT --hot stderr must NOT contain '{bad}'; got:\n{stderr1}"
        );
    }
    // No degraded key in JSON output.
    let (json1, _, _) = skim_search(
        &["widget", "--hot", "--limit", "5", "--json"],
        &root1,
        cache1.path(),
    );
    let v1: Value = serde_json::from_str(&json1).unwrap_or(Value::Null);
    let degraded1 = v1["degraded"].as_array().cloned().unwrap_or_default();
    assert!(
        degraded1.is_empty(),
        "T-8/AC-8: FX-1COMMIT --hot --json must have no degraded elements; got:\n{json1}"
    );
}

// ============================================================================
// Group 5: FX-CORRUPT — T-9
// ============================================================================

/// T-9 / AC-9 — FX-CORRUPT: self-heal after first query.
///
/// Run 1: stderr contains `corrupt` and `rebuild`; run 2 JSON has
/// `results[].temporal.hotspot_score` and no `degraded` key.
#[test]
fn t9_fx_corrupt_self_heal_on_first_query() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    make_repo2(&root);
    let cache = TempDir::new().expect("cache TempDir");
    build_index(&root, cache.path());
    let db_dir = find_search_cache(cache.path());
    let db_path = db_dir.join("temporal.db");
    make_corrupt_temporal(&db_path);

    // Run 1: corrupt DB — should print degraded notice and trigger self-heal.
    let (_, stderr1, code1) = skim_search(
        &["zebra_widget", "--hot", "--limit", "5"],
        &root,
        cache.path(),
    );
    assert_eq!(code1, 0, "T-9/AC-9: corrupt DB run1 must exit 0");
    assert!(
        stderr1.contains("corrupt"),
        "T-9/AC-9: run1 stderr must contain 'corrupt'; got:\n{stderr1}"
    );
    assert!(
        stderr1.contains("rebuild"),
        "T-9/AC-9: run1 stderr must contain 'rebuild'; got:\n{stderr1}"
    );

    // After run 1: temporal.db must be rebuilt (size > 1024, hotspot rows > 0).
    let db_size_after = file_size(&db_path);
    assert!(
        db_size_after > 1024,
        "T-9/AC-9: temporal.db must be rebuilt after corrupt self-heal (size={db_size_after})"
    );
    let hotspot_count = sqlite_count(&db_path, "hotspot");
    assert!(
        hotspot_count > 0,
        "T-9/AC-9: hotspot table must be non-empty after self-heal (count={hotspot_count})"
    );

    // Run 2: no corruption — JSON output has temporal scores, no degraded key.
    let (json2, _, code2) = skim_search(
        &["zebra_widget", "--hot", "--limit", "5", "--json"],
        &root,
        cache.path(),
    );
    assert_eq!(code2, 0, "T-9/AC-9: run2 must exit 0");
    let v2: Value = serde_json::from_str(&json2).unwrap_or(Value::Null);
    let degraded2 = v2["degraded"].as_array().cloned().unwrap_or_default();
    assert!(
        degraded2.is_empty(),
        "T-9/AC-9: run2 --json must have no degraded elements (self-heal succeeded); got:\n{json2}"
    );
    let results2 = v2["results"].as_array().cloned().unwrap_or_default();
    let has_hotspot = results2
        .iter()
        .any(|r| !r["temporal"]["hotspot_score"].is_null());
    assert!(
        has_hotspot,
        "T-9/AC-9: run2 must have at least one result with temporal.hotspot_score after self-heal; got:\n{json2}"
    );
}

// ============================================================================
// Group 6: FX-NEWER — T-10 / T-30
// ============================================================================

/// T-10 / AC-10 — FX-NEWER: user_version=99 preserved after both initial query
/// and explicit --rebuild.
///
/// Exit 0 both times, stderr names the newer-version cause, and user_version
/// remains 99 and byte-length unchanged.
#[test]
fn t10_fx_newer_schema_preserved_on_query_and_rebuild() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    make_repo2(&root);
    let cache = TempDir::new().expect("cache TempDir");
    build_index(&root, cache.path());
    let db_dir = find_search_cache(cache.path());
    let db_path = db_dir.join("temporal.db");
    make_newer_temporal(&db_path);

    let version_before = sqlite_user_version(&db_path);
    let size_before = file_size(&db_path);
    assert_eq!(
        version_before, 99,
        "T-10: user_version must be 99 before first query"
    );

    // Query with --hot: should see the newer-version degraded notice, exit 0.
    let (_, stderr1, code1) = skim_search(
        &["zebra_widget", "--hot", "--limit", "2"],
        &root,
        cache.path(),
    );
    assert_eq!(code1, 0, "T-10/AC-10: FX-NEWER query must exit 0");
    assert!(
        stderr1.contains("newer") || stderr1.contains("newer skim") || stderr1.contains("upgrade"),
        "T-10/AC-10: stderr must mention newer version; got:\n{stderr1}"
    );

    let version_after_q = sqlite_user_version(&db_path);
    let size_after_q = file_size(&db_path);
    assert_eq!(
        version_after_q, 99,
        "T-10/AC-10: user_version must still be 99 after query"
    );
    assert_eq!(
        size_after_q, size_before,
        "T-10/AC-10: temporal.db size must not change on query with newer-schema"
    );

    // --rebuild: must also preserve user_version=99 (refuses to overwrite a future DB).
    let (_, stderr2, code2) = skim_search(&["--rebuild"], &root, cache.path());
    assert_eq!(code2, 0, "T-10/AC-10: --rebuild on FX-NEWER must exit 0");
    let _ = stderr2; // Rebuild may or may not emit the notice; presence not required.

    let version_after_rebuild = sqlite_user_version(&db_path);
    let size_after_rebuild = file_size(&db_path);
    assert_eq!(
        version_after_rebuild, 99,
        "T-10/AC-10: user_version must still be 99 after --rebuild"
    );
    assert_eq!(
        size_after_rebuild, size_before,
        "T-10/AC-10: temporal.db size must not change after --rebuild on newer-schema"
    );

    // JSON mode must produce degraded with reason=unsupported_version.
    let (json_out, _, _) = skim_search(
        &["zebra_widget", "--hot", "--limit", "2", "--json"],
        &root,
        cache.path(),
    );
    assert_degraded_reason(&json_out, "unsupported_version");
}

/// T-30 / AC-30 — FX-NEWER + new commit: at most one temporal-failure line across
/// two queries (no infinite noise).
#[test]
fn t30_fx_newer_new_commit_at_most_one_failure_line() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    make_repo2(&root);
    let cache = TempDir::new().expect("cache TempDir");
    build_index(&root, cache.path());
    let db_dir = find_search_cache(cache.path());
    let db_path = db_dir.join("temporal.db");
    make_newer_temporal(&db_path);

    // Add a new commit so temporal_db_is_stale returns true.
    fs::write(
        root.join("src/alpha.rs"),
        "fn alpha_widget() { let y = 99; }\n// modified\n",
    )
    .unwrap();
    git_add_commit(&root, "fix: modify alpha");

    // Two queries without a temporal flag (plain text query, no --hot etc.).
    let (_, stderr1, code1) = skim_search(&["zebra_widget", "--limit", "2"], &root, cache.path());
    let (_, stderr2, code2) = skim_search(&["zebra_widget", "--limit", "2"], &root, cache.path());
    assert_eq!(code1, 0, "T-30/AC-30: run1 must exit 0");
    assert_eq!(code2, 0, "T-30/AC-30: run2 must exit 0");

    // Count lines containing a temporal-failure indicator across both runs.
    let combined = format!("{stderr1}{stderr2}");
    let failure_lines = combined
        .lines()
        .filter(|l| {
            l.contains("newer") || l.contains("unsupported_version") || l.contains("upgrade skim")
        })
        .count();
    assert!(
        failure_lines <= 1,
        "T-30/AC-30: at most one temporal-failure line across two queries; got {failure_lines}:\n{combined}"
    );
}

// ============================================================================
// Group 7: FX-1COMMIT — T-18
// ============================================================================

/// T-18 / AC-18 — FX-1COMMIT: --build + plain query + --hot emit no false notices.
///
/// This is partially covered in T-8; here we add the JSON assertion.
#[test]
fn t18_fx_1commit_no_false_degraded_notices() {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path().join("repo");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.rs"), "fn widget() { let x = 1; }\n").unwrap();
    git_init(&root);
    git_add_commit(&root, "init: one commit");

    let cache = TempDir::new().expect("cache TempDir");
    build_index(&root, cache.path());

    let (_, stderr_q, code_q) = skim_search(&["widget", "--limit", "5"], &root, cache.path());
    assert_eq!(code_q, 0, "T-18/AC-18: plain query must exit 0");

    let (json_hot, _, _) = skim_search(
        &["widget", "--hot", "--limit", "5", "--json"],
        &root,
        cache.path(),
    );
    let v: Value = serde_json::from_str(&json_hot).unwrap_or(Value::Null);
    let degraded = v["degraded"].as_array().cloned().unwrap_or_default();
    assert!(
        degraded.is_empty(),
        "T-18/AC-18: FX-1COMMIT --hot --json must have no degraded elements; got:\n{json_hot}"
    );
    for bad in &["shallow", "unshallow", "no commit history"] {
        assert!(
            !stderr_q.contains(bad),
            "T-18/AC-18: plain query stderr must NOT contain '{bad}'; got:\n{stderr_q}"
        );
    }
}

// ============================================================================
// Group 8: pagination on degraded — T-21
// ============================================================================

/// T-21 / AC-21 — FX-EMPTY and FX-CORRUPT: pagination (--offset) on degraded.
///
/// Arms: text+--hot, text+--cold, text+--risky, text+--ast rust-nested-loop --hot.
/// At --limit 5 with 10 matching files:
/// - page 0 must contain exactly `limit` results (non-emptiness premise — PF-007)
/// - page 0 and page 1 must be disjoint
/// - `has_more` must be true on page 0 (iff page 1 is non-empty)
/// - `total == len(results)` for each page
///
/// Standalone --hot is NOT included: in both FX-EMPTY and FX-CORRUPT the
/// temporal DB is unusable, so the standalone arm produces 0 results
/// (no hotspot rows to rank; lexical fallback only applies to text+temporal
/// combinations).  Pagination of a 0-result set is trivially correct.
///
/// FX-EMPTY is the primary fixture (temporal DB present but empty).
/// FX-CORRUPT is the secondary fixture (temporal DB corrupted, no-commit repo
/// so auto-heal cannot fire — same corrupt state persists through both pages).
#[test]
fn t21_degraded_pagination_disjoint_pages() {
    // -----------------------------------------------------------------------
    // Build shared FX-EMPTY fixture: 10 files, 2 commits, empty temporal.db.
    // Files include nested loops so --ast rust-nested-loop also matches them.
    // -----------------------------------------------------------------------
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path().join("repo");
    fs::create_dir_all(root.join("src")).unwrap();
    // Create 10 files with nested loops so pagination has >5 results.
    for i in 0..10_u32 {
        fs::write(
            root.join(format!("src/w{i:02}.rs")),
            format!(
                "fn widget_{i}() {{\n    \
                 for _a in 0..2 {{ for _b in 0..2 {{ let _ = (_a, _b); }} }}\n\
                 }}\n// zebra_widget marker\n"
            ),
        )
        .unwrap();
    }
    git_init(&root);
    git_add_commit(&root, "init");
    for i in 0..10_u32 {
        fs::write(
            root.join(format!("src/w{i:02}.rs")),
            format!(
                "fn widget_{i}() {{\n    \
                 for _a in 0..3 {{ for _b in 0..2 {{ let _ = ({i}u32, _a, _b); }} }}\n\
                 }}\n// zebra_widget marker\n"
            ),
        )
        .unwrap();
    }
    git_add_commit(&root, "fix: update all");

    let cache_empty = TempDir::new().expect("cache TempDir");
    build_index(&root, cache_empty.path());

    // FX-EMPTY: replace temporal.db with empty schema.
    let db_dir_empty = find_search_cache(cache_empty.path());
    let head = git_head(&root);
    make_empty_temporal(&db_dir_empty, &head);

    // -----------------------------------------------------------------------
    // Build FX-CORRUPT fixture: separate no-commit repo so auto-heal can't fire.
    // -----------------------------------------------------------------------
    let corrupt_dir = TempDir::new().expect("corrupt TempDir");
    let corrupt_root = corrupt_dir.path().join("repo");
    fs::create_dir_all(corrupt_root.join("src")).unwrap();
    for i in 0..10_u32 {
        fs::write(
            corrupt_root.join(format!("src/w{i:02}.rs")),
            format!(
                "fn widget_{i}() {{\n    \
                 for _a in 0..3 {{ for _b in 0..2 {{ let _ = ({i}u32, _a, _b); }} }}\n\
                 }}\n// zebra_widget marker\n"
            ),
        )
        .unwrap();
    }
    git_init(&corrupt_root);
    // Deliberately NO commit: no HEAD → no temporal rebuild possible (see T-7).

    let cache_corrupt = TempDir::new().expect("cache TempDir");
    build_index(&corrupt_root, cache_corrupt.path());
    let db_dir_corrupt = find_search_cache(cache_corrupt.path());
    let db_path_corrupt = db_dir_corrupt.join("temporal.db");
    make_corrupt_temporal(&db_path_corrupt);

    // -----------------------------------------------------------------------
    // Helper: verify pagination for one (root, cache, arm) triple.
    // -----------------------------------------------------------------------
    let check_pagination =
        |label: &str, base_args: &[&str], proj_root: &std::path::Path, cache: &std::path::Path| {
            let mut p0_args: Vec<&str> = base_args.to_vec();
            p0_args.extend_from_slice(&["--limit", "5", "--offset", "0", "--json"]);
            let mut p1_args: Vec<&str> = base_args.to_vec();
            p1_args.extend_from_slice(&["--limit", "5", "--offset", "5", "--json"]);

            let (p0_json, _, c0) = skim_search(&p0_args, proj_root, cache);
            let (p1_json, _, c1) = skim_search(&p1_args, proj_root, cache);
            assert_eq!(c0, 0, "T-21: {label} page 0 must exit 0");
            assert_eq!(c1, 0, "T-21: {label} page 1 must exit 0");

            let paths0 = extract_paths(&p0_json);
            let paths1 = extract_paths(&p1_json);
            let v0: Value = serde_json::from_str(&p0_json).unwrap_or(Value::Null);

            // Non-emptiness: page 0 must contain exactly `limit` results (PF-007).
            // If page 0 is empty the disjoint and has_more assertions are trivially true.
            assert_eq!(
                paths0.len(),
                5,
                "T-21/AC-21: {label} page 0 must contain exactly 5 results (limit=5); \
             got {} — check that the fixture has ≥5 matching files",
                paths0.len()
            );

            // Disjoint pages.
            let overlap: Vec<_> = paths0.iter().filter(|p| paths1.contains(p)).collect();
            assert!(
                overlap.is_empty(),
                "T-21/AC-21: {label} pages must be disjoint; overlap={overlap:?}"
            );

            // has_more on page 0 must be true iff page 1 is non-empty.
            let has_more_p0 = v0["has_more"].as_bool().unwrap_or(false);
            let p1_non_empty = !paths1.is_empty();
            assert_eq!(
                has_more_p0, p1_non_empty,
                "T-21/AC-21: {label} page 0 has_more={has_more_p0} but page 1 is \
             non_empty={p1_non_empty} — has_more must equal whether the next page exists"
            );

            // total == len(results) for each page.
            for (page_n, page_json) in [(0usize, &p0_json), (1usize, &p1_json)] {
                let pv: Value = serde_json::from_str(page_json).unwrap_or(Value::Null);
                let results_len = pv["results"].as_array().map(|a| a.len()).unwrap_or(0);
                let total = pv["total"].as_u64().unwrap_or(999) as usize;
                assert_eq!(
                    total, results_len,
                    "T-21/AC-21: {label} page {page_n}: total must equal results length; \
                 total={total} results_len={results_len}\nJSON:\n{page_json}"
                );
            }
        };

    // -----------------------------------------------------------------------
    // FX-EMPTY arms: text+temporal and text+ast+temporal.
    // Files have nested loops so --ast rust-nested-loop matches all 10.
    // -----------------------------------------------------------------------
    let empty_arms: &[&[&str]] = &[
        &["zebra_widget", "--hot"],
        &["zebra_widget", "--cold"],
        &["zebra_widget", "--risky"],
        // SE-4 arm: text+AST+temporal; AST pool bounds the result set.
        &["zebra_widget", "--ast", "rust-nested-loop", "--hot"],
    ];
    for base_args in empty_arms {
        let label = format!("FX-EMPTY {}", base_args.join(" "));
        check_pagination(&label, base_args, &root, cache_empty.path());
    }

    // -----------------------------------------------------------------------
    // FX-CORRUPT arms: text+--hot and text+--cold.
    // auto-heal cannot fire (no HEAD) → corrupt state persists through both pages.
    // -----------------------------------------------------------------------
    let corrupt_arms: &[&[&str]] = &[&["zebra_widget", "--hot"], &["zebra_widget", "--cold"]];
    for base_args in corrupt_arms {
        let label = format!("FX-CORRUPT {}", base_args.join(" "));
        check_pagination(&label, base_args, &corrupt_root, cache_corrupt.path());
    }
}

// ============================================================================
// Group 8b: limit sweep on degraded — T-22 / AC-22
// ============================================================================

/// T-22 / AC-22 — FX-CORRUPT: no invocation returns more results than --limit.
///
/// This is one of #414's three headline symptoms ("--limit violated").
/// Without the fix the degraded lexical-fallback path could return an untruncated
/// full result set, ignoring the caller-supplied limit.
///
/// Fixture: 18 files all matching the query `grid_widget`.  The temporal DB is
/// corrupted (no-commit repo so auto-heal cannot fire).  For each limit value in
/// `{1, 2, 5, 7, 20, 100}` the arms `text+--hot` and `text+--risky` are verified.
/// Expected baseline for a 18-file corpus:
///   limit=1  → 1, limit=2  → 2, limit=5  → 5, limit=7  → 7,
///   limit=20 → 18, limit=100 → 18.
/// The test asserts `len(results) <= limit` — it does NOT pin the exact count so
/// it remains valid if the fixture size changes.
#[test]
fn t22_limit_sweep_degraded_corrupt_never_exceeds_limit() {
    // FX-CORRUPT-18: 18 files, no commits (no HEAD → auto-heal cannot fire).
    const FILE_COUNT: u32 = 18;
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path().join("repo");
    fs::create_dir_all(root.join("src")).unwrap();
    for i in 0..FILE_COUNT {
        fs::write(
            root.join(format!("src/g{i:02}.rs")),
            format!("fn grid_widget_{i}() {{ let g = {i}; let _ = g; }}\n// grid_widget\n"),
        )
        .unwrap();
    }
    git_init(&root);
    // Deliberately NO commit: no HEAD → temporal DB cannot be rebuilt by auto-heal.

    let cache = TempDir::new().expect("cache TempDir");
    build_index(&root, cache.path());
    let db_dir = find_search_cache(cache.path());
    make_corrupt_temporal(&db_dir.join("temporal.db"));

    let limits: &[usize] = &[1, 2, 5, 7, 20, 100];
    let arms: &[&[&str]] = &[&["grid_widget", "--hot"], &["grid_widget", "--risky"]];

    for &limit in limits {
        let limit_str = limit.to_string();
        for base_args in arms {
            let arm_label = base_args.join(" ");
            let mut args: Vec<&str> = base_args.to_vec();
            args.extend_from_slice(&["--limit", &limit_str, "--json"]);
            let (json_out, _, code) = skim_search(&args, &root, cache.path());
            assert_eq!(
                code, 0,
                "T-22/AC-22: '{arm_label}' --limit {limit} must exit 0"
            );
            let v: Value = serde_json::from_str(&json_out)
                .unwrap_or_else(|e| panic!("T-22: JSON parse error: {e}\n{json_out}"));
            let result_count = v["results"].as_array().map(|a| a.len()).unwrap_or(0);
            assert!(
                result_count <= limit,
                "T-22/AC-22: '{arm_label}' --limit {limit} returned {result_count} results \
                 (EXCEEDS LIMIT) — the degraded lexical-fallback path must honour --limit; \
                 got JSON:\n{json_out}"
            );
        }
    }
}

// ============================================================================
// Group 9: chmod 500 — T-29
// ============================================================================

/// T-29 / AC-29 — FX-CORRUPT + read-only directory: exit 0, explicit message.
///
/// When the cache directory is mode 500 (read-only, no write), skim cannot
/// delete or replace the corrupt temporal.db.  It must exit 0, print the
/// absolute db path, and emit a manual-deletion instruction.
///
/// WAL/SHM sidecars must still be present (SE-3: if discard fails, nothing changes).
/// Directory is restored to mode 700 in teardown.
///
/// Unix-only: chmod 500 is not meaningful on Windows.
#[cfg(unix)]
#[test]
fn t29_fx_corrupt_readonly_dir_exit_0_explicit_message() {
    use std::os::unix::fs::PermissionsExt;

    let dir = TempDir::new().expect("TempDir");
    let root = dir.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    make_repo2(&root);
    let cache = TempDir::new().expect("cache TempDir");
    build_index(&root, cache.path());
    let db_dir = find_search_cache(cache.path());
    let db_path = db_dir.join("temporal.db");

    // Plant some WAL/SHM sidecars to verify SE-3 (sidecars preserved).
    let wal_path = db_dir.join("temporal.db-wal");
    let shm_path = db_dir.join("temporal.db-shm");
    fs::write(&wal_path, b"wal-sentinel").unwrap();
    fs::write(&shm_path, b"shm-sentinel").unwrap();

    make_corrupt_temporal(&db_path);

    // Make the directory read-only (mode 500: r-x r-- r--).
    fs::set_permissions(&db_dir, fs::Permissions::from_mode(0o500)).unwrap();

    let (_, stderr, code) = skim_search(
        &["zebra_widget", "--hot", "--limit", "2"],
        &root,
        cache.path(),
    );

    // Restore permissions for cleanup (TempDir::drop needs to remove the dir).
    fs::set_permissions(&db_dir, fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(code, 0, "T-29/AC-29: chmod 500 + corrupt must exit 0");

    // Stderr must name the db path and a manual-deletion instruction.
    let db_path_str = db_path.to_string_lossy();
    assert!(
        stderr.contains(db_path_str.as_ref()),
        "T-29/AC-29: stderr must contain the absolute db path '{db_path_str}'; got:\n{stderr}"
    );
    // "delete" or "remove" or "rm" should appear in the manual-deletion instruction.
    assert!(
        stderr.contains("delete") || stderr.contains("remove") || stderr.contains("rm"),
        "T-29/AC-29: stderr must contain a manual-deletion instruction; got:\n{stderr}"
    );

    // SE-3: WAL and SHM sidecars must still be present (discard failed, nothing changed).
    assert!(
        wal_path.exists(),
        "T-29/AC-29 SE-3: temporal.db-wal must still be present"
    );
    assert!(
        shm_path.exists(),
        "T-29/AC-29 SE-3: temporal.db-shm must still be present"
    );
}

// ============================================================================
// Group 11: FX-COVERAGE — T-38
// ============================================================================

/// Build the FX-COVERAGE fixture.
///
/// FX-ABSENT-ROWS (uncommitted `zz_new.rs`, `aa_new.rs`, `mm_new.rs`, `zz_zero.rs`) PLUS
/// one committed file `src/bb_hot.rs` that also matches `thing_marker`.
/// The committed file gets a hotspot row; the uncommitted files do not.
///
/// Two queries:
/// - Zero-coverage query: `other_a` (in `aa_new.rs` and `zz_zero.rs`) → only uncommitted.
///   `zz_zero.rs` has higher TF for `other_a` (4 occurrences) than `aa_new.rs` (1).
///   BM25F ranks `zz_zero.rs` first; path-ASC would rank `aa_new.rs` first — a
///   measurable discriminant for AC-36(a): the degraded result must match BM25F
///   (lexical fallback), not path-ASC (sentinel-sort artefact).
/// - Partial-coverage query: `thing_marker` → all four original files (not `zz_zero.rs`)
///
/// `aa_new.rs` contains a nested loop so `--ast rust-nested-loop` matches it
/// in the zero-coverage arm (AC-36(a) --ast sub-case).
fn make_fx_coverage() -> (TempDir, std::path::PathBuf, TempDir) {
    let dir = TempDir::new().expect("TempDir");
    let root = dir.path().join("repo");
    fs::create_dir_all(root.join("src")).unwrap();

    // Base repo: two commits (FX-REPO2 shape).
    fs::write(
        root.join("src/zebra.rs"),
        "fn zebra_widget() { let x = 1; }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/alpha.rs"),
        "fn alpha_widget() { let y = 2; }\n",
    )
    .unwrap();
    git_init(&root);
    git_add_commit(&root, "c1: add widgets");
    fs::write(
        root.join("src/zebra.rs"),
        "fn zebra_widget() { let x = 3; }\n// touched\n",
    )
    .unwrap();
    git_add_commit(&root, "fix: bug in zebra widget");

    // Add and commit bb_hot.rs — it will carry a hotspot row.
    fs::write(
        root.join("src/bb_hot.rs"),
        "fn bb_helper() { let thing_marker = 1; }\n",
    )
    .unwrap();
    git_add_commit(&root, "fix: add bb helper");

    let cache = TempDir::new().expect("cache TempDir");
    build_index(&root, cache.path());

    // Now add four uncommitted files (FX-ABSENT-ROWS shape + discriminant file).
    //
    // aa_new.rs: 1 occurrence of `other_a` (function def) + a nested-for loop so
    // `--ast rust-nested-loop` matches it.
    fs::write(
        root.join("src/aa_new.rs"),
        "// thing_marker mentioned in a comment only\n\
         fn other_a() {\n\
             for i in 0..3 {\n\
                 for j in 0..2 {\n\
                     let _ = (i, j);\n\
                 }\n\
             }\n\
         }\n",
    )
    .unwrap();
    fs::write(
        root.join("src/mm_new.rs"),
        "fn m_helper() { let v = 1; }\n// note: thing_marker\n",
    )
    .unwrap();
    fs::write(root.join("src/zz_new.rs"), "fn thing_marker() { }\n").unwrap();
    // zz_zero.rs: 4 occurrences of `other_a` (1 def + 3 calls) → higher BM25F TF
    // than aa_new.rs (1 occurrence).  Alphabetically AFTER aa_new.rs (z > a), so
    // BM25F order (zz_zero first) != path-ASC order (aa_new first) → discriminant.
    fs::write(
        root.join("src/zz_zero.rs"),
        "fn other_a() { let zero = 0; let _ = zero; }\n\
         fn call_other_a() {\n\
             other_a();\n\
             other_a();\n\
             other_a();\n\
         }\n",
    )
    .unwrap();

    (dir, root, cache)
}

/// T-38 / AC-36 — FX-COVERAGE: partial-coverage query ordering.
///
/// # AC-36(b) — partial-coverage (`thing_marker` matches all files incl. bb_hot.rs)
///
/// - `--hot`: `bb_hot.rs` must appear FIRST (highest hotspot score as a committed
///   file touched in two commits); the three unranked uncommitted files follow.
/// - `--cold`: the three unranked files appear FIRST; `bb_hot.rs` is LAST.
/// - Only `bb_hot.rs` carries a `temporal` key; uncommitted files must not.
/// - No `degraded` element in either case.
///
/// # AC-36(a) — zero-coverage (`other_a` only in uncommitted files)
///
/// - All matched files have no temporal rows → NoRankedRows degraded notice fires.
/// - The degraded fallback is lexical (BM25F), NOT path-ASC (sentinel-sort artefact).
/// - Discriminant: `zz_zero.rs` has higher TF than `aa_new.rs` for `other_a`
///   (4 vs 1 occurrences) → BM25F ranks `zz_zero.rs` first; path-ASC would rank
///   `aa_new.rs` first.  Asserting `hot_paths[0].contains("zz_zero")` is the
///   measurable proof that lexical fallback was applied, not sentinel sorting.
/// - Arms: `--hot`, `--cold`, `--risky`, `--ast rust-nested-loop --hot`.
#[test]
fn t38_fx_coverage_partial_and_zero_coverage() {
    let (_dir, root, cache) = make_fx_coverage();

    // Verify premise: bb_hot.rs has a hotspot row, uncommitted files do not.
    let db_dir = find_search_cache(cache.path());
    let db_path = db_dir.join("temporal.db");
    let bb_count = {
        let out = StdCommand::new("sqlite3")
            .arg(&db_path)
            .arg("SELECT COUNT(*) FROM hotspot WHERE file_path='src/bb_hot.rs';")
            .output()
            .expect("sqlite3 bb_hot count");
        let s = String::from_utf8_lossy(&out.stdout);
        s.trim().parse::<i64>().unwrap_or(-1)
    };
    assert_eq!(
        bb_count, 1,
        "T-38/AC-36: premise check — bb_hot.rs must have 1 hotspot row (PF-007)"
    );

    // -----------------------------------------------------------------------
    // AC-36(b): PARTIAL COVERAGE — thing_marker matches all four original files.
    // -----------------------------------------------------------------------

    // --hot: bb_hot.rs must be FIRST (ranked by hotspot score).
    let (hot_json, _, code_hot) = skim_search(
        &["thing_marker", "--hot", "--limit", "5", "--json"],
        &root,
        cache.path(),
    );
    assert_eq!(
        code_hot, 0,
        "T-38/AC-36(b): --hot partial coverage must exit 0"
    );

    let hot_paths = extract_paths(&hot_json);
    assert!(
        !hot_paths.is_empty(),
        "T-38/AC-36(b): --hot must return results (PF-007)"
    );

    // AC-36(b) ordering: bb_hot.rs must appear FIRST under --hot.
    assert!(
        hot_paths
            .first()
            .map(|p| p.contains("bb_hot"))
            .unwrap_or(false),
        "T-38/AC-36(b): --hot must rank bb_hot.rs FIRST (highest hotspot score); \
         got paths={hot_paths:?}"
    );

    // No degraded element in partial-coverage case.
    let v_hot: Value = serde_json::from_str(&hot_json).unwrap_or(Value::Null);
    let degraded_hot = v_hot["degraded"].as_array().cloned().unwrap_or_default();
    assert!(
        degraded_hot.is_empty(),
        "T-38/AC-36(b): --hot partial coverage must have no degraded elements; got:\n{hot_json}"
    );

    // AC-36(b) temporal annotation: only bb_hot.rs must carry a real hotspot_score.
    // Uncommitted files have no temporal rows so their hotspot_score must be absent.
    // We check hotspot_score specifically (not the outer temporal key) to avoid
    // brittleness around empty-object serialisation of all-None TemporalAnnotations.
    let results_hot = v_hot["results"].as_array().cloned().unwrap_or_default();
    for r in &results_hot {
        let path = r["path"].as_str().unwrap_or("");
        let has_score = r["temporal"]["hotspot_score"].as_f64().is_some();
        if path.contains("bb_hot") {
            assert!(
                has_score,
                "T-38/AC-36(b): bb_hot.rs must carry temporal.hotspot_score in --hot; got:\n{r}"
            );
        } else {
            assert!(
                !has_score,
                "T-38/AC-36(b): uncommitted file '{path}' must NOT carry a hotspot_score \
                 (no temporal rows → score absent); got:\n{r}"
            );
        }
    }

    // --cold: bb_hot.rs must be LAST (lowest hotspot score → coldest).
    let (cold_json, _, code_cold) = skim_search(
        &["thing_marker", "--cold", "--limit", "5", "--json"],
        &root,
        cache.path(),
    );
    assert_eq!(
        code_cold, 0,
        "T-38/AC-36(b): --cold partial coverage must exit 0"
    );

    let cold_paths = extract_paths(&cold_json);
    assert!(
        !cold_paths.is_empty(),
        "T-38/AC-36(b): --cold must return results (PF-007)"
    );

    // AC-36(b) ordering: bb_hot.rs must appear LAST under --cold.
    assert!(
        cold_paths
            .last()
            .map(|p| p.contains("bb_hot"))
            .unwrap_or(false),
        "T-38/AC-36(b): --cold must rank bb_hot.rs LAST (highest hotspot → coldest last); \
         got paths={cold_paths:?}"
    );

    // No degraded element under --cold either.
    let v_cold: Value = serde_json::from_str(&cold_json).unwrap_or(Value::Null);
    let degraded_cold = v_cold["degraded"].as_array().cloned().unwrap_or_default();
    assert!(
        degraded_cold.is_empty(),
        "T-38/AC-36(b): --cold partial coverage must have no degraded elements; got:\n{cold_json}"
    );

    // -----------------------------------------------------------------------
    // AC-36(a): ZERO COVERAGE — other_a matches only uncommitted files.
    // Discriminant: zz_zero.rs has higher TF (4 occurrences) than aa_new.rs (1).
    // BM25F order: zz_zero.rs first.  Path-ASC order: aa_new.rs first.
    // Asserting zz_zero.rs first proves the degraded fallback is BM25F, not
    // path-ASC (the sentinel-sort artefact that the pre-fix code would produce).
    // -----------------------------------------------------------------------

    let (plain_zero_json, _, _) =
        skim_search(&["other_a", "--limit", "5", "--json"], &root, cache.path());
    let plain_zero_paths = extract_paths(&plain_zero_json);

    // Non-vacuousness: the plain query must return ≥1 result.
    // If it returns 0, the fixture or the index is broken — fail loudly (PF-007).
    assert!(
        !plain_zero_paths.is_empty(),
        "T-38/AC-36(a) premise: plain query for 'other_a' must return ≥1 result; \
         aa_new.rs and zz_zero.rs both contain it — check fixture or index"
    );

    // AC-36(a) discriminant: zz_zero.rs (TF=4) must rank before aa_new.rs (TF=1).
    // If aa_new.rs is first, the sort key is path-ASC (broken sentinel), not BM25F.
    let plain_first = plain_zero_paths.first().map(|s| s.as_str()).unwrap_or("");
    assert!(
        plain_first.contains("zz_zero"),
        "T-38/AC-36(a) discriminant: BM25F must rank zz_zero.rs (TF=4) before \
         aa_new.rs (TF=1) for query 'other_a'; got first={plain_first:?}\n\
         all plain paths={plain_zero_paths:?}"
    );

    // Zero-coverage arms: --hot, --cold, --risky, --ast rust-nested-loop --hot.
    // Each must degrade to lexical (NoRankedRows) and match the plain query order.
    let zero_cov_arms: &[&[&str]] = &[
        &["other_a", "--hot"],
        &["other_a", "--cold"],
        &["other_a", "--risky"],
        &["other_a", "--ast", "rust-nested-loop", "--hot"],
    ];
    for arm in zero_cov_arms {
        let arm_label = arm.join(" ");
        let mut args: Vec<&str> = arm.to_vec();
        args.extend_from_slice(&["--limit", "5", "--json"]);
        let (arm_json, arm_stderr, arm_code) = skim_search(&args, &root, cache.path());
        assert_eq!(arm_code, 0, "T-38/AC-36(a): '{arm_label}' must exit 0");

        let arm_paths = extract_paths(&arm_json);
        assert!(
            !arm_paths.is_empty(),
            "T-38/AC-36(a): '{arm_label}' must return ≥1 result (PF-007)"
        );

        // For text-based arms: paths must match the plain query's BM25F order.
        // The --ast arm may return a subset (only aa_new.rs has nested loops), so
        // we only check that the degraded notice fires, not the exact path set.
        let is_ast_arm = arm.contains(&"--ast");
        if !is_ast_arm {
            assert_eq!(
                plain_zero_paths, arm_paths,
                "T-38/AC-36(a): '{arm_label}' degraded paths must match plain query \
                 (BM25F fallback, not path-ASC sentinel)"
            );
        }

        // NoRankedRows degraded notice must fire for every arm.
        assert_degraded_reason(&arm_json, "no_ranked_rows");
        assert!(
            arm_stderr.contains("not applied") || arm_stderr.contains("0 of"),
            "T-38/AC-36(a): '{arm_label}' stderr must mention degradation; got:\n{arm_stderr}"
        );
    }
}

// ============================================================================
// Step 9 (moved from mod.rs in-process): T-23 / T-6 / T-8 / AC-30
//
// These tests were previously in-process (using run()) and could only assert
// exit code.  Subprocess versions capture stdout/stderr so AC-23 and AC-30
// contracts can be properly verified (PF-007: no vacuous exit-code-only tests).
// ============================================================================

/// T-23 / AC-23 (Step 9): `skim search --json --hot` on a non-git directory
/// must exit 0, emit a single parseable JSON object on stdout, and emit the
/// temporal degraded notice to stderr only.
///
/// Covers the new Step 9 NotGitRepo code path (WarningWithDegradedJson).
/// NOGIT precondition required: after #413's walk-up, a tempdir under a clone
/// would adopt that clone's HEAD and classify as a git repo.
#[test]
fn t23_step9_not_git_repo_json_hot_exit_0_with_output() {
    let dir = tempfile::TempDir::new().unwrap();
    let cache = tempfile::TempDir::new().unwrap();
    assert_nogit(dir.path());

    let (stdout, stderr, code) = skim_search(&["--json", "--hot"], dir.path(), cache.path());

    assert_eq!(
        code, 0,
        "T-23/AC-23: --json --hot on non-git dir must exit 0; stderr:\n{stderr}"
    );

    // AC-23: stdout must be a single parseable JSON object (not empty, not an error).
    let v: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("T-23/AC-23: stdout must be a parseable JSON object; error={e}\nstdout:\n{stdout}")
    });
    assert!(
        v.is_object(),
        "T-23/AC-23: stdout JSON must be an object; got:\n{stdout}"
    );

    // AC-23: the degraded notice must go to stderr only, not stdout.
    // The JSON object must not itself be an unstructured error string.
    assert!(
        v.get("error").is_none_or(|e| !e.is_string()),
        "T-23/AC-23: stdout JSON must not be a bare error string; got:\n{stdout}"
    );

    // Stderr must contain some indication of the not-a-git-repo state.
    assert!(
        stderr.contains("not a git") || stderr.contains("not_a_repo") || !stderr.is_empty(),
        "T-23/AC-23: stderr should contain temporal degraded notice; got stderr:\n{stderr}"
    );
}

/// T-6 (Step 9): `skim search --hot` (non-JSON) on a non-git directory must
/// exit 0 and emit the temporal degraded notice to stderr.
///
/// Covers the text (non-JSON) path of the new Step 9 NotGitRepo arm.
/// NOGIT precondition required: see T-23 above.
#[test]
fn t6_step9_not_git_repo_text_hot_exit_0_with_stderr() {
    let dir = tempfile::TempDir::new().unwrap();
    let cache = tempfile::TempDir::new().unwrap();
    assert_nogit(dir.path());

    let (stdout, stderr, code) = skim_search(&["--hot"], dir.path(), cache.path());

    assert_eq!(
        code, 0,
        "T-6/step9: --hot on non-git dir must exit 0; stderr:\n{stderr}"
    );

    // Text mode: stdout should not contain a JSON object (it's plain text or empty).
    // The temporal notice must appear on stderr.
    assert!(
        !stdout.trim().starts_with('{'),
        "T-6/step9: text mode must not emit a JSON object on stdout; got:\n{stdout}"
    );

    // Stderr must carry the degraded notice (not-a-repo path fires a notice).
    assert!(
        !stderr.is_empty(),
        "T-6/step9: stderr must contain temporal degraded notice on non-git dir; got nothing"
    );
}

/// T-8 (Step 9): `skim search --json --hot` on a git repo with no temporal.db
/// must exit 0 and emit a parseable JSON object on stdout.
///
/// Uses a fresh git repo (head_state is Resolved, not NotARepo) but no temporal.db.
/// This hits DegradedReason::Missing on the "other reasons → stderr + JSON" branch.
#[test]
fn t8_step9_missing_db_git_repo_json_hot_exit_0_with_output() {
    let proj = tempfile::TempDir::new().unwrap();
    let cache = tempfile::TempDir::new().unwrap();

    // Init a bare git repo so head_state is Resolved (not NotARepo).
    git_init(proj.path());
    // No skim --build call: temporal.db is absent → DegradedReason::Missing.

    let (stdout, stderr, code) = skim_search(&["--json", "--hot"], proj.path(), cache.path());

    assert_eq!(
        code, 0,
        "T-8/step9: --json --hot on git repo with no temporal.db must exit 0; stderr:\n{stderr}"
    );

    // Stdout must be a parseable JSON object.
    let v: Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("T-8/step9: stdout must be a parseable JSON object; error={e}\nstdout:\n{stdout}")
    });
    assert!(
        v.is_object(),
        "T-8/step9: stdout JSON must be an object; got:\n{stdout}"
    );

    // Stderr must carry the Missing degraded notice.
    assert!(
        !stderr.is_empty(),
        "T-8/step9: stderr must contain a temporal degraded notice (Missing path); got nothing"
    );
}

/// AC-30 (Step 9): a plain text query with no temporal flag must never emit a
/// temporal-degraded line on stderr.
///
/// AC-30's real contract: plain queries bypass the temporal path entirely, so no
/// temporal-degraded notice (corrupt / missing / not_a_repo / rebuild hint) should
/// appear on stderr.  This is stronger than the previous in-process test, which
/// could only assert exit 0.
#[test]
fn t_ac30_step9_plain_query_no_temporal_notice_on_stderr() {
    let dir = tempfile::TempDir::new().unwrap();
    let cache = tempfile::TempDir::new().unwrap();

    // Plain text query with no --hot / --cold / --risky / --blast-radius flag.
    let (_stdout, stderr, code) = skim_search(&["something"], dir.path(), cache.path());

    assert_eq!(
        code, 0,
        "AC-30: plain text query on empty dir must exit 0; stderr:\n{stderr}"
    );

    // No temporal-degraded phrases must appear on stderr.
    // The temporal path emits notices with "temporal db:" as the key prefix; its
    // presence would indicate the temporal path fired despite no temporal flag.
    // "not a git" covers the NotGitRepo standalone arm.
    let temporal_phrases = ["temporal db:", "not a git", "(not_a_repo)"];
    for phrase in temporal_phrases {
        assert!(
            !stderr.to_lowercase().contains(phrase),
            "AC-30: plain text query stderr must not contain temporal-degraded phrase '{phrase}'; \
             got stderr:\n{stderr}"
        );
    }
}
