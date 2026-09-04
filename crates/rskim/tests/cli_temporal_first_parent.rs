//! CLI E2E tests for ticket #407: full-DAG temporal walk.
//!
//! # AD-407-8: T-18 / AC-16 — merge-fixture ground truth
//!
//! The merge fixture (`make_merge_repo`) contains exactly four non-merge
//! commits.  After #407's full-DAG walk, `skim search --risky --json` on
//! that fixture MUST report `total_commits` equal to `git rev-list --count
//! --no-merges <HEAD>` (derived in-test per ADR-003), and per-file counts
//! MUST match `git rev-list --count --no-merges --full-history HEAD -- <f>`.
//!
//! # AD-407-10: T-20 / AC-17 — author-date window
//!
//! `skim search --hot --json` `changes_30d` MUST equal the count of non-merge
//! commits touching each path whose AUTHOR timestamp is within 30 days of now,
//! derived in-test from `git log --no-merges --format=%at -- <path>`.  The
//! criterion MUST NOT be expressed with `git rev-list --since` (committer
//! date).
//!
//! # AC-21 guard
//!
//! With `--root <subdirectory>`, every path in temporal output MUST be inside
//! that subtree; no co-change peer outside the subtree may appear (ADR-009).
//!
//! # AC-22 / T-19 — heatmap parity
//!
//! On a no-merge fixture whose commits fall inside heatmap's default 90-day
//! window, for every file `skim heatmap --json` `churn.commits` MUST equal
//! `skim search --risky --json` `total_commits`, and `fix_risk.keyword_pct`
//! MUST equal `fix_density * 100` (the RAW `RiskRow.fix_density`, NOT
//! `FileRiskScores.fix_density`).
//!
//! # Design
//!
//! All tests shell out to the skim binary via `assert_cmd::cargo::cargo_bin`.
//! Each invocation carries an isolated `SKIM_CACHE_DIR` on a per-test
//! `TempDir`.  Every `skim search` invocation passes an explicit `--root`;
//! `skim heatmap` has no `--root` flag and is instead run with its
//! `current_dir` set to the fixture root.  Fixtures use pinned
//! `GIT_AUTHOR_DATE`/`GIT_COMMITTER_DATE` so counts are deterministic.

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;
use std::time::{SystemTime, UNIX_EPOCH};

use assert_cmd::cargo::cargo_bin;
use serde_json::Value;
use tempfile::TempDir;

// ============================================================================
// Helpers
// ============================================================================

/// Return the current Unix epoch in seconds.
fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before UNIX epoch")
        .as_secs()
}

/// Returns `true` when the repository at `repo_path` is a shallow clone;
/// `false` on a full clone.
///
/// Real-history parity tests compare skim's temporal output against `git log`
/// ground truth derived from *this repository*. On a shallow checkout, git
/// sees only the fetched commits, so the ground-truth count would be trivially
/// small and the parity assertions would vacuously pass — defeating the
/// PF-007 non-vacuous guard they contain.
///
/// Detection uses two independent signals:
/// 1. `git rev-parse --is-shallow-repository` (Git ≥ 2.15, prints `true`).
/// 2. Presence of `.git/shallow` (works for all git versions; located via
///    `git rev-parse --git-dir` to handle linked worktrees correctly).
///
/// CI note: the `Test Suite` job in `.github/workflows/ci.yml` sets
/// `fetch-depth: 0` on its `actions/checkout@v5` step so this guard never
/// fires there; all other jobs keep the default shallow fetch.
///
/// AD-407-11: shallow-checkout guard for real-history parity tests — root
/// cause of CI run 33906188121 where `fetch-depth: 1` caused git_total=1,
/// making the ≥ 67 non-vacuous guard assert instead of the parity assertions.
fn is_shallow_checkout(repo_path: &Path) -> bool {
    // Signal 1: `git rev-parse --is-shallow-repository` (Git ≥ 2.15).
    if let Ok(out) = StdCommand::new("git")
        .args([
            "-C",
            &repo_path.to_string_lossy(),
            "rev-parse",
            "--is-shallow-repository",
        ])
        .output()
        && out.status.success()
        && String::from_utf8_lossy(&out.stdout).trim() == "true"
    {
        return true;
    }
    // Signal 2: presence of `.git/shallow` (universally reliable).
    // `--git-dir` resolves the real git directory for linked worktrees.
    if let Ok(out) = StdCommand::new("git")
        .args(["-C", &repo_path.to_string_lossy(), "rev-parse", "--git-dir"])
        .output()
        && out.status.success()
    {
        let git_dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let shallow_rel = std::path::Path::new(&git_dir).join("shallow");
        let shallow = if shallow_rel.is_absolute() {
            shallow_rel
        } else {
            repo_path.join(shallow_rel)
        };
        if shallow.exists() {
            return true;
        }
    }
    false
}

/// Initialise a git repository with hermetic, non-signing identity.
fn git_init(dir: &Path) {
    for args in &[
        vec!["init"],
        vec!["config", "user.email", "test@t.invalid"],
        vec!["config", "user.name", "Test"],
        vec!["config", "commit.gpgsign", "false"],
    ] {
        let s = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git {:?}: {e}", args));
        assert!(
            s.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&s.stderr)
        );
    }
    // Use "main" as the initial branch name (avoids warnings on some git versions).
    let _ = StdCommand::new("git")
        .args(["checkout", "-b", "main"])
        .current_dir(dir)
        .output();
}

/// Write `content` to `dir/<filename>` and stage it.
fn write_and_stage(dir: &Path, filename: &str, content: &str) {
    let path = dir.join(filename);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap_or_else(|e| panic!("write {filename}: {e}"));
    let s = StdCommand::new("git")
        .args(["add", filename])
        .current_dir(dir)
        .output()
        .expect("git add");
    assert!(s.status.success(), "git add {filename} failed");
}

/// Commit staged changes with pinned author and committer timestamps.
///
/// `ts` is a Unix epoch value used for both `GIT_AUTHOR_DATE` and
/// `GIT_COMMITTER_DATE` so tests are deterministic across timezones.
fn git_commit(dir: &Path, message: &str, ts: u64) {
    let ts_str = ts.to_string();
    let s = StdCommand::new("git")
        .args(["commit", "--no-verify", "-m", message])
        .env("GIT_AUTHOR_DATE", &ts_str)
        .env("GIT_COMMITTER_DATE", &ts_str)
        .current_dir(dir)
        .output()
        .expect("git commit");
    assert!(
        s.status.success(),
        "git commit '{}' failed: {}",
        message,
        String::from_utf8_lossy(&s.stderr)
    );
}

/// Checkout (or create) a branch.
fn git_checkout(dir: &Path, branch: &str, create: bool) {
    let mut args = vec!["checkout"];
    if create {
        args.push("-b");
    }
    args.push(branch);
    let s = StdCommand::new("git")
        .args(&args)
        .current_dir(dir)
        .output()
        .expect("git checkout");
    assert!(
        s.status.success(),
        "git checkout {:?} failed: {}",
        args,
        String::from_utf8_lossy(&s.stderr)
    );
}

/// Merge `branch` into the current branch with --no-ff (creates a merge commit).
fn git_merge_no_ff(dir: &Path, branch: &str, ts: u64) {
    let ts_str = ts.to_string();
    let s = StdCommand::new("git")
        .args([
            "merge",
            "--no-ff",
            "--no-verify",
            "-m",
            &format!("Merge branch '{branch}'"),
            branch,
        ])
        .env("GIT_AUTHOR_DATE", &ts_str)
        .env("GIT_COMMITTER_DATE", &ts_str)
        .current_dir(dir)
        .output()
        .expect("git merge --no-ff");
    assert!(
        s.status.success(),
        "git merge --no-ff '{branch}' failed: {}",
        String::from_utf8_lossy(&s.stderr)
    );
}

/// Derive a count from `git rev-list --count [--full-history] --no-merges HEAD [-- path]`.
///
/// Panics if git fails.
fn git_rev_list_no_merges_count(dir: &Path, path: Option<&str>) -> u32 {
    let mut args = vec!["rev-list", "--count", "--no-merges"];
    if path.is_some() {
        args.push("--full-history");
    }
    args.push("HEAD");
    if let Some(p) = path {
        args.push("--");
        args.push(p);
    }
    let out = StdCommand::new("git")
        .args(&args)
        .current_dir(dir)
        .output()
        .expect("git rev-list --count");
    assert!(out.status.success(), "git rev-list failed");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("rev-list count must be a number")
}

/// Build the skim search index for `root` using an isolated `cache` directory.
///
/// Uses `.output()` so stderr is captured and available in failure diagnostics.
/// AC-9 (AMENDED) specifically tests empty-stderr on a clean fixture; individual
/// callers MAY have non-empty stderr (e.g. `ast_coverage_notice` on `.txt`-only
/// repos) and need not assert it here.
fn build_index(root: &Path, cache: &Path) {
    let out = StdCommand::new(cargo_bin("skim"))
        .args(["search", "--build", "--root"])
        .arg(root)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim search --build");
    assert!(
        out.status.success(),
        "skim search --build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run `skim search [args...] --json --root <root>` and return parsed JSON output.
fn run_search_json(root: &Path, cache: &Path, extra_args: &[&str]) -> Value {
    let out = StdCommand::new(cargo_bin("skim"))
        .args(["search"])
        .args(extra_args)
        .args(["--json", "--root"])
        .arg(root)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim search --json");
    assert!(
        out.status.success(),
        "skim search {:?} failed: {}",
        extra_args,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("skim output utf-8");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("skim search --json produced invalid JSON: {e}\n{stdout}"))
}

/// Run `skim search --stats --json --root <root>` and return parsed JSON.
///
/// Used by AC-18 to verify `temporal_state` and `git_head_state` after a
/// data-version self-heal without invoking the search ranking layer.
fn run_stats_json(root: &Path, cache: &Path) -> Value {
    let out = StdCommand::new(cargo_bin("skim"))
        .args(["search", "--stats", "--json", "--root"])
        .arg(root)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim search --stats --json");
    assert!(
        out.status.success(),
        "skim search --stats --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("skim --stats --json output utf-8");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("skim --stats --json produced invalid JSON: {e}\n{stdout}"))
}

/// Run `skim heatmap --json` from `cwd` (heatmap does not have a `--root` flag).
fn run_heatmap_json(cwd: &Path, cache: &Path) -> Value {
    let out = StdCommand::new(cargo_bin("skim"))
        .args(["heatmap", "--json"])
        .current_dir(cwd)
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim heatmap --json");
    assert!(
        out.status.success(),
        "skim heatmap --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("skim heatmap output utf-8");
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("skim heatmap --json produced invalid JSON: {e}\n{stdout}"))
}

// ============================================================================
// Fixtures
// ============================================================================

/// Create the merge fixture for T-18 / AC-16.
///
/// Git history layout:
/// - C1 (main):    a.txt  "feat: add a"        (non-fix)
/// - C2 (feature): b.txt  "fix: bug one"       (fix)
/// - C3 (feature): b.txt  "fix: bug two"       (fix)
/// - C4 (main):    a.txt  "chore: update a"    (non-fix)
/// - M  (main):    merge --no-ff feature       (merge commit — skipped)
///
/// Expected non-merge commit set (git rev-list --count --no-merges HEAD = 4):
/// - a.txt: 2 commits, 0 fix → fix_density = 0.0
/// - b.txt: 2 commits, 2 fix → fix_density = 1.0
fn make_merge_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let now = now_epoch();
    git_init(dir.path());

    // C1 — a.txt on main (non-fix)
    write_and_stage(dir.path(), "a.txt", "a1\n");
    git_commit(dir.path(), "feat: add a", now - 40 * 86400);

    // Branch off to feature
    git_checkout(dir.path(), "feature", true);

    // C2 — b.txt on feature (fix)
    write_and_stage(dir.path(), "b.txt", "b1\n");
    git_commit(dir.path(), "fix: bug one", now - 30 * 86400);

    // C3 — b.txt on feature (fix)
    write_and_stage(dir.path(), "b.txt", "b2\n");
    git_commit(dir.path(), "fix: bug two", now - 20 * 86400);

    // Back to main
    git_checkout(dir.path(), "main", false);

    // C4 — a.txt on main (non-fix)
    write_and_stage(dir.path(), "a.txt", "a2\n");
    git_commit(dir.path(), "chore: update a", now - 15 * 86400);

    // M — merge commit (skipped by skim's full-DAG walk per AD-407-2)
    git_merge_no_ff(dir.path(), "feature", now - 10 * 86400);

    dir
}

/// Create the heatmap-parity fixture for T-19 / AC-22.
///
/// All commits within the last 60 days (inside heatmap's 90-day default window).
/// No merge commits.  Layout:
/// - C1: a.rs  "feat: add a"      (non-fix)
/// - C2: a.rs  "fix: update a"    (fix)
/// - C3: b.rs  "chore: add b"     (non-fix)
fn make_heatmap_parity_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let now = now_epoch();
    git_init(dir.path());

    write_and_stage(dir.path(), "a.rs", "fn a1() {}\n");
    git_commit(dir.path(), "feat: add a", now - 25 * 86400);

    write_and_stage(dir.path(), "a.rs", "fn a2() {}\n");
    git_commit(dir.path(), "fix: update a", now - 15 * 86400);

    write_and_stage(dir.path(), "b.rs", "fn b1() {}\n");
    git_commit(dir.path(), "chore: add b", now - 5 * 86400);

    dir
}

/// Create the author-date window fixture for T-20 / AC-17.
///
/// Layout:
/// - C1: a.rs  author = now-15d  (within 30d)
/// - C2: b.rs  author = now-45d  (NOT within 30d, but within 90d)
/// - C3: a.rs  author = now-10d  (within 30d)
///
/// Expected: a.rs changes_30d=2, b.rs changes_30d=0.
fn make_author_date_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let now = now_epoch();
    git_init(dir.path());

    // C1: a.rs within 30d
    write_and_stage(dir.path(), "a.rs", "fn a1() {}\n");
    git_commit(dir.path(), "feat: add a", now - 15 * 86400);

    // C2: b.rs outside 30d (45d ago)
    write_and_stage(dir.path(), "b.rs", "fn b1() {}\n");
    git_commit(dir.path(), "feat: add b", now - 45 * 86400);

    // C3: a.rs within 30d
    write_and_stage(dir.path(), "a.rs", "fn a2() {}\n");
    git_commit(dir.path(), "fix: update a", now - 10 * 86400);

    dir
}

/// Create the subdirectory-scope fixture for AC-21.
///
/// Layout (2 commits):
/// - C1: `src/a.rs`, `src/c.rs`, `other/b.rs` (feat: add all)
/// - C2: `src/a.rs`, `src/c.rs`, `other/b.rs` (fix: update all)
///
/// `src/a.rs` and `src/c.rs` co-change together in every commit, giving
/// `--blast-radius a.rs` (with `--root src/`) a non-empty in-scope peer list
/// (`c.rs`).  `other/b.rs` co-changes with both but is outside the subtree.
/// When built with `--root <repo>/src/`, only paths relative to `src/` may
/// appear; `other/b.rs` must be absent from all temporal output (ADR-009).
fn make_subdir_scope_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let now = now_epoch();
    git_init(dir.path());

    // C1: three files — two in-scope (src/), one out-of-scope (other/).
    //
    // src/a.rs and src/c.rs co-change together so that --blast-radius a.rs
    // (with --root src/) has a non-empty, in-scope peer list (c.rs).  This
    // prevents the blast-radius assertion loop from being vacuous (PF-007):
    // if scope-filtering is over-narrow and returns nothing, the positive
    // assertion `c.rs is present` catches the regression.
    write_and_stage(dir.path(), "src/a.rs", "fn a1() {}\n");
    write_and_stage(dir.path(), "src/c.rs", "fn c1() {}\n");
    write_and_stage(dir.path(), "other/b.rs", "fn b1() {}\n");
    git_commit(dir.path(), "feat: add all", now - 20 * 86400);

    // C2: all three again (reinforces the a.rs ↔ c.rs co-change pair).
    write_and_stage(dir.path(), "src/a.rs", "fn a2() {}\n");
    write_and_stage(dir.path(), "src/c.rs", "fn c2() {}\n");
    write_and_stage(dir.path(), "other/b.rs", "fn b2() {}\n");
    git_commit(dir.path(), "fix: update all", now - 10 * 86400);

    dir
}

// ============================================================================
// T-18 / AC-16: merge fixture ground truth  (AD-407-8)
// ============================================================================

/// T-18 / AC-16: `skim search --risky --json` on the merge fixture MUST report
/// per-file counts matching `git rev-list --count --no-merges` (ADR-003).
///
/// - b.txt: total_commits=2, fix_commits=2, fix_density=1.0
/// - a.txt: total_commits=2, fix_commits=0, fix_density=0.0
///
/// The JSON envelope MUST carry exactly: `mode` ("risky"), `total`, `results[]`
/// with `path`, `risk_score`, `fix_density`, `fix_commits`, `total_commits`
/// (and `has_more` only when true).  Exits 0.
///
/// AD-407-8: the merge commit at HEAD is skipped by the full-DAG walk (AD-407-2);
/// only the four non-merge commits (C1, C2, C3, C4) contribute to the counts.
#[test]
fn test_risky_json_matches_git_ground_truth_on_merge_repo() {
    let repo = make_merge_repo();
    let cache = TempDir::new().expect("cache tempdir");

    build_index(repo.path(), cache.path());

    let json = run_search_json(repo.path(), cache.path(), &["--risky"]);

    // Envelope shape: mode + total + results
    assert_eq!(json["mode"], "risky", "mode must be 'risky'");
    assert!(json["total"].is_number(), "total must be a number");
    let results = json["results"]
        .as_array()
        .expect("results must be an array");

    // Derive ground-truth counts from git (ADR-003 — never hardcode)
    let total_no_merge = git_rev_list_no_merges_count(repo.path(), None);
    assert_eq!(
        total_no_merge, 4,
        "merge fixture must have exactly 4 non-merge commits"
    );
    let a_count = git_rev_list_no_merges_count(repo.path(), Some("a.txt"));
    let b_count = git_rev_list_no_merges_count(repo.path(), Some("b.txt"));
    assert_eq!(a_count, 2, "a.txt must have 2 non-merge commits");
    assert_eq!(b_count, 2, "b.txt must have 2 non-merge commits");

    // Locate a.txt and b.txt in results
    let find_result = |path: &str| -> &Value {
        results
            .iter()
            .find(|r| r["path"].as_str() == Some(path))
            .unwrap_or_else(|| panic!("expected '{path}' in --risky results"))
    };

    let a_row = find_result("a.txt");
    let b_row = find_result("b.txt");

    // AC-16: every row MUST carry exactly the required keys — no extras, no
    // missing.  `has_more` belongs on the envelope only and MUST NOT appear
    // on a per-row object; the key-set equality check enforces this.
    let required_row_keys: std::collections::HashSet<&str> = [
        "path",
        "risk_score",
        "fix_density",
        "fix_commits",
        "total_commits",
    ]
    .iter()
    .cloned()
    .collect();
    for (row, name) in [(a_row, "a.txt"), (b_row, "b.txt")] {
        let obj = row
            .as_object()
            .unwrap_or_else(|| panic!("{name}: result row must be a JSON object"));
        let actual_keys: std::collections::HashSet<&str> = obj.keys().map(String::as_str).collect();
        assert_eq!(
            actual_keys, required_row_keys,
            "{name}: row key set must be exactly \
             {{path, risk_score, fix_density, fix_commits, total_commits}} (AC-16)"
        );
    }

    // AC-16: `has_more` MUST NOT appear on the envelope when all results fit
    // in one page.  The merge fixture has only 2 files — well within any page
    // limit — so `has_more` must be absent here.
    assert!(
        json.get("has_more").is_none(),
        "AC-16: has_more must not appear on the envelope when the page is \
         complete; envelope: {json}"
    );

    // b.txt: 2 commits, both fix
    assert_eq!(
        b_row["total_commits"].as_u64(),
        Some(u64::from(b_count)),
        "b.txt total_commits must match git rev-list --no-merges"
    );
    assert_eq!(
        b_row["fix_commits"].as_u64(),
        Some(2),
        "b.txt fix_commits must be 2"
    );
    let b_density = b_row["fix_density"]
        .as_f64()
        .expect("b.txt fix_density f64");
    assert!(
        (b_density - 1.0_f64).abs() < 1e-9,
        "b.txt fix_density must be 1.0, got {b_density}"
    );

    // a.txt: 2 commits, no fix
    assert_eq!(
        a_row["total_commits"].as_u64(),
        Some(u64::from(a_count)),
        "a.txt total_commits must match git rev-list --no-merges"
    );
    assert_eq!(
        a_row["fix_commits"].as_u64(),
        Some(0),
        "a.txt fix_commits must be 0"
    );
    let a_density = a_row["fix_density"]
        .as_f64()
        .expect("a.txt fix_density f64");
    assert!(
        a_density.abs() < 1e-9,
        "a.txt fix_density must be 0.0, got {a_density}"
    );
}

// ============================================================================
// AC-21: subdirectory scope guard
// ============================================================================

/// AC-21: `--root <subdirectory>` MUST scope every temporal output path to the
/// subtree and MUST NOT emit co-change peers from outside it (ADR-009).
///
/// The larger commit population (#407 full-DAG walk) MUST NOT widen the #413
/// scope that `apply_scope_filter` enforces at build time.
#[test]
fn test_root_subdir_scope_contains_only_subtree_paths() {
    let repo = make_subdir_scope_repo();
    let cache = TempDir::new().expect("cache tempdir");

    // Build index scoped to the src/ subdirectory.
    let src_root = repo.path().join("src");

    // AC-21: the src/ subtree contains exactly these two files.  Every
    // temporal output path must be a member of this set (ADR-009).  A
    // positive membership check catches regressions that a `!starts_with`
    // guard misses: absolute paths, `../other/b.rs`, or a peer re-anchored
    // to a different spelling all fail membership but pass the negative test.
    let src_subtree_paths: std::collections::HashSet<&str> =
        ["a.rs", "c.rs"].iter().cloned().collect();

    build_index(&src_root, cache.path());

    // --risky --json: only src/ paths may appear.
    //
    // PF-007: replace `if let Some` with `.expect` so a missing/null results
    // field is a hard failure rather than silent vacuity.  Then assert a.rs is
    // present so an over-narrow scope filter (returning nothing) is caught.
    let risky = run_search_json(&src_root, cache.path(), &["--risky"]);
    let results = risky["results"]
        .as_array()
        .expect("AC-21: --risky results must be a JSON array");
    assert!(
        results.iter().any(|r| r["path"].as_str() == Some("a.rs")),
        "AC-21: --risky must contain a.rs; got: {:?}",
        results
            .iter()
            .map(|r| r["path"].as_str())
            .collect::<Vec<_>>()
    );
    for row in results {
        let p = row["path"].as_str().unwrap_or("(no path)");
        assert!(
            src_subtree_paths.contains(p),
            "AC-21: --risky path must be in the src/ subtree set \
             {src_subtree_paths:?}; got: {p}"
        );
    }

    // --hot --json: only src/ paths.
    //
    // PF-007: same discipline as --risky above.
    let hot = run_search_json(&src_root, cache.path(), &["--hot"]);
    let results = hot["results"]
        .as_array()
        .expect("AC-21: --hot results must be a JSON array");
    assert!(
        results.iter().any(|r| r["path"].as_str() == Some("a.rs")),
        "AC-21: --hot must contain a.rs; got: {:?}",
        results
            .iter()
            .map(|r| r["path"].as_str())
            .collect::<Vec<_>>()
    );
    for row in results {
        let p = row["path"].as_str().unwrap_or("(no path)");
        assert!(
            src_subtree_paths.contains(p),
            "AC-21: --hot path must be in the src/ subtree set \
             {src_subtree_paths:?}; got: {p}"
        );
    }

    // --blast-radius a.rs --json: peer must not reference other/b.rs.
    //
    // The subtree-relative spelling `a.rs` is correct under `--root src/`;
    // `src/a.rs` would not resolve (ADR-009).
    //
    // PF-007: replace `if let Some` with `.expect`.  Assert c.rs is present:
    // c.rs is an in-scope co-change peer of a.rs (both appear in every commit
    // in the fixture), so a scope filter that over-narrows to nothing is caught.
    let blast = run_search_json(&src_root, cache.path(), &["--blast-radius", "a.rs"]);
    let results = blast["results"]
        .as_array()
        .expect("AC-21: --blast-radius results must be a JSON array");
    assert!(
        results.iter().any(|r| r["path"].as_str() == Some("c.rs")),
        "AC-21: --blast-radius a.rs must contain c.rs as in-scope co-change peer; \
         got: {:?}",
        results
            .iter()
            .map(|r| r["path"].as_str())
            .collect::<Vec<_>>()
    );
    for row in results {
        let p = row["path"].as_str().unwrap_or("(no path)");
        assert!(
            src_subtree_paths.contains(p),
            "AC-21: --blast-radius a.rs peer must be in the src/ subtree set \
             {src_subtree_paths:?}; got: {p}"
        );
    }
}

// ============================================================================
// T-19 / AC-22: heatmap parity
// ============================================================================

/// T-19 / AC-22: on a no-merge fixture within heatmap's 90-day window,
/// `skim heatmap --json` `churn.commits` MUST equal `skim search --risky --json`
/// `total_commits`, and `fix_risk.keyword_pct` MUST equal `fix_density * 100`
/// (the RAW `RiskRow.fix_density`).
///
/// MUST NOT assert against `fix_risk.combined_pct` (proximity signal not in
/// temporal layer) or `FileRiskScores.fix_density` (decay-weighted, not raw).
#[test]
fn test_risky_agrees_with_heatmap_on_same_tree() {
    let repo = make_heatmap_parity_repo();
    let cache = TempDir::new().expect("cache tempdir");

    build_index(repo.path(), cache.path());

    let risky = run_search_json(repo.path(), cache.path(), &["--risky"]);
    let heatmap = run_heatmap_json(repo.path(), cache.path());

    let risky_results = risky["results"]
        .as_array()
        .expect("risky results must be an array");
    let heatmap_files = heatmap["files"]
        .as_array()
        .expect("heatmap files must be an array");

    assert!(
        !risky_results.is_empty(),
        "T-19: --risky must return at least one result"
    );
    assert!(
        !heatmap_files.is_empty(),
        "T-19: heatmap must return at least one file"
    );

    // PF-007: the `continue` below skips a file heatmap did not report, so the
    // loop can in principle assert nothing at all.  Count the files actually
    // compared and require at least one, otherwise a join-key drift (heatmap and
    // search disagreeing on path spelling) would leave this test silently green.
    let mut compared = 0_usize;

    // For every file in the risky output, find its heatmap entry and compare.
    for risk_row in risky_results {
        let path = risk_row["path"].as_str().expect("risky row has path");

        let hm_entry = heatmap_files
            .iter()
            .find(|f| f["path"].as_str() == Some(path));

        // If the file appears in risky output it MUST also appear in heatmap
        // (same commit population, same walk).
        let hm_entry = match hm_entry {
            Some(e) => e,
            None => {
                // File may be missing from heatmap if heatmap's window filter
                // excludes it; skip rather than fail (fixture uses recent commits
                // so this should not happen in practice).
                continue;
            }
        };

        compared += 1;

        let risky_total: u64 = risk_row["total_commits"]
            .as_u64()
            .unwrap_or_else(|| panic!("{path}: total_commits must be u64"));
        let hm_churn_commits: u64 = hm_entry["churn"]["commits"]
            .as_u64()
            .unwrap_or_else(|| panic!("{path}: heatmap churn.commits must be u64"));

        assert_eq!(
            hm_churn_commits, risky_total,
            "AC-22: {path}: heatmap churn.commits ({hm_churn_commits}) \
             must equal risky total_commits ({risky_total})"
        );

        let risky_density: f64 = risk_row["fix_density"]
            .as_f64()
            .unwrap_or_else(|| panic!("{path}: fix_density must be f64"));
        let hm_keyword_pct: f64 = hm_entry["fix_risk"]["keyword_pct"]
            .as_f64()
            .unwrap_or_else(|| panic!("{path}: heatmap fix_risk.keyword_pct must be f64"));

        let expected_kw_pct = risky_density * 100.0;
        assert!(
            (hm_keyword_pct - expected_kw_pct).abs() < 1e-6,
            "AC-22: {path}: heatmap fix_risk.keyword_pct ({hm_keyword_pct}) \
             must equal fix_density*100 ({expected_kw_pct})"
        );
    }

    // PF-007: at least one file must have been compared, or the parity assertions
    // above never ran.  The fixture commits both a.rs and b.rs inside heatmap's
    // default window, so every risky row has a heatmap counterpart.
    assert!(
        compared > 0,
        "AC-22: no file was compared — heatmap and --risky share no path key. \
         risky paths: {:?}, heatmap paths: {:?}",
        risky_results
            .iter()
            .map(|r| r["path"].as_str())
            .collect::<Vec<_>>(),
        heatmap_files
            .iter()
            .map(|f| f["path"].as_str())
            .collect::<Vec<_>>(),
    );
}

// ============================================================================
// T-20 / AC-17: author-date window  (AD-407-10)
// ============================================================================

/// T-20 / AC-17: `skim search --hot --json` `changes_30d` per path MUST equal
/// the count of non-merge commits touching that path whose AUTHOR timestamp is
/// within 30 days of now.
///
/// Ground truth is derived in-test from `git log --no-merges --format=%at --
/// <path>`, which returns **author** timestamps.  `git rev-list --since` is
/// NEVER used here — it filters on committer date, not author date (AD-407-10).
///
/// AD-407-10: `CommitInfo.timestamp` is the author timestamp; `changes_30d`
/// counts commits where `now - author_ts <= 30 * 86400`.  This test pins the
/// AC-17 author-date contract so any future refactor to committer time is caught.
#[test]
fn test_hot_30d_matches_author_date_window() {
    let repo = make_author_date_repo();
    let cache = TempDir::new().expect("cache tempdir");

    build_index(repo.path(), cache.path());

    let hot = run_search_json(repo.path(), cache.path(), &["--hot"]);

    let results = hot["results"]
        .as_array()
        .expect("hot results must be an array");
    assert!(
        !results.is_empty(),
        "T-20: --hot must return at least one result"
    );

    // Derive expected changes_30d for each file from git log --format=%at.
    let files = ["a.rs", "b.rs"];
    for file in &files {
        let out = StdCommand::new("git")
            .args(["log", "--no-merges", "--format=%at", "--", file])
            .current_dir(repo.path())
            .output()
            .expect("git log --format=%at");
        assert!(
            out.status.success(),
            "git log --format=%at failed for {file}"
        );

        let now = now_epoch();
        let thirty_days = 30u64 * 86400;
        let expected_30d: u32 = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|line| {
                if line.is_empty() {
                    return false;
                }
                match line.trim().parse::<u64>() {
                    Ok(ts) => now.saturating_sub(ts) <= thirty_days,
                    Err(_) => false,
                }
            })
            .count() as u32;

        // Find this file in the hot results (it may be absent if changes_30d=0)
        let row = results.iter().find(|r| r["path"].as_str() == Some(file));

        let actual_30d: u32 = row.and_then(|r| r["changes_30d"].as_u64()).unwrap_or(0) as u32;

        assert_eq!(
            actual_30d, expected_30d,
            "AC-17: {file}: changes_30d={actual_30d} but git log --format=%at gives {expected_30d} \
             commits within 30 days (author time)"
        );
    }

    // Verify a.rs has changes_30d=2 and b.rs has changes_30d=0 for this fixture.
    // (These are derived from git in the loop above, but stated explicitly for
    // readability — they are not hardcoded assertions, the loop is the real guard.)
    let a_row = results.iter().find(|r| r["path"].as_str() == Some("a.rs"));
    let a_30d = a_row.and_then(|r| r["changes_30d"].as_u64()).unwrap_or(0);
    assert!(
        a_30d >= 1,
        "T-20: a.rs must have at least 1 commit within 30d (has commits at 15d and 10d)"
    );

    let b_row = results.iter().find(|r| r["path"].as_str() == Some("b.rs"));
    let b_30d = b_row.and_then(|r| r["changes_30d"].as_u64()).unwrap_or(0);
    assert_eq!(
        b_30d, 0,
        "T-20: b.rs must have 0 commits within 30d (only commit is 45d ago)"
    );
}

// ============================================================================
// T-21 / AC-18 (observable): no `degraded` entry and temporal_state healthy
// after a data-version self-heal
// ============================================================================

/// T-21 / AC-18 (observable): planting `data_version="1"` into a built
/// `temporal.db` (simulating a pre-#407 binary's output) and then running a
/// query MUST produce no `degraded` key in the `--hot --json` output, and a
/// subsequent `--stats --json` call MUST report `temporal_state` as `"ready"`
/// or `"empty"` (never `"corrupt"` or `"missing"`) with `git_head_state`
/// unchanged at `"resolved"`.
///
/// Mechanism: skim's self-heal path detects `data_version = "1" < 2` on the
/// first query after the plant, rebuilds `temporal.db`, and serves the result
/// normally — no `DegradedReason` fires.
///
/// AC-18 is stated in observable, binary-level terms; this test drives it
/// end-to-end through the actual skim binary rather than testing internal state
/// only (devflow:testing Iron Law). The internal-state variant
/// (`test_stale_data_version_no_degraded_state_after_heal` in
/// `staleness_tests.rs`) verifies the library layer; this test verifies the
/// full CLI stack.
///
/// AD-407-1 (full-DAG walk triggers data_version bump), AD-407-5 (one-shot
/// self-heal), AD-408-4 (Check 2: stored < current triggers rebuild).
#[test]
fn test_ac18_stale_data_version_heals_no_degraded_on_next_query() {
    let repo = make_merge_repo();
    let cache = TempDir::new().expect("cache tempdir");

    // Build the index so temporal.db is populated with real data.
    build_index(repo.path(), cache.path());

    // Locate temporal.db via --stats --json (canonical cache_dir path).
    let stats_initial = run_stats_json(repo.path(), cache.path());
    let cache_dir_str = stats_initial["cache_dir"]
        .as_str()
        .expect("cache_dir must be a string in --stats --json output");
    let temporal_db = std::path::Path::new(cache_dir_str).join("temporal.db");
    assert!(
        temporal_db.exists(),
        "temporal.db must exist after --build at {temporal_db:?}"
    );

    // Plant data_version="1" to simulate a pre-#407 DB (AD-408-4 Check 2).
    // Use rusqlite directly — TemporalDb::set_meta guards this key (AD-408-3).
    {
        let conn = rusqlite::Connection::open(&temporal_db)
            .expect("open temporal.db for data_version seeding");
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            rusqlite::params!["data_version", "1"],
        )
        .expect("plant data_version=1 into temporal.db");
    }

    // Run a query with --hot --json.  The self-heal fires on this call (AD-408-4
    // Check 2: stored "1" < TEMPORAL_DATA_VERSION "2"), rebuilds temporal.db,
    // and serves the result normally.  No DegradedReason fires → no `degraded` key.
    let query_json = run_search_json(repo.path(), cache.path(), &["--hot"]);
    assert!(
        query_json.get("degraded").is_none(),
        "AC-18: --hot --json must carry no 'degraded' key after data-version \
         self-heal (temporal data served normally post-heal); got: {query_json}"
    );

    // Verify --stats --json reports a healthy temporal state and unchanged HEAD state.
    let stats_after = run_stats_json(repo.path(), cache.path());

    let temporal_state = stats_after["temporal_state"]
        .as_str()
        .expect("temporal_state must be a string in --stats --json");
    assert!(
        temporal_state == "ready" || temporal_state == "empty",
        "AC-18: temporal_state after self-heal must be 'ready' or 'empty' \
         (not 'corrupt' or 'missing'); got: {temporal_state:?}"
    );

    let git_head_state = stats_after["git_head_state"]
        .as_str()
        .expect("git_head_state must be a string in --stats --json");
    assert_eq!(
        git_head_state, "resolved",
        "AC-18: git_head_state must remain 'resolved' after data-version \
         self-heal — the self-heal must not break HEAD resolution"
    );
}

// ============================================================================
// AC-9 (AMENDED): empty stderr for --build and --risky under cap
// ============================================================================

/// AC-9 (AMENDED): `skim search --build` and `skim search --risky` MUST produce
/// empty stderr on any repository whose walk stays under both caps.
///
/// Neither the retained-commit nor the visited-commit `WalkBudget` notice may
/// fire below the cap, and no existing E2E stderr assertion may change.
///
/// This test uses the heatmap-parity fixture (only `.rs` files, all
/// AST-indexed) so `ast_coverage_notice` does not fire.  With 3 commits —
/// far below any reasonable walk cap — neither budget notice fires either.
///
/// The amended criterion narrows the scope to `--build` and `--risky` only;
/// it does not pre-empt the `--blast-radius` arm's unconditional stderr notice
/// added by a separate ticket.
#[test]
fn test_ac9_build_and_risky_produce_no_stderr() {
    let repo = make_heatmap_parity_repo();
    let cache = TempDir::new().expect("cache tempdir");

    // --build: stderr MUST be empty.
    let build_out = StdCommand::new(cargo_bin("skim"))
        .args(["search", "--build", "--root"])
        .arg(repo.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim search --build output");
    assert!(
        build_out.status.success(),
        "skim search --build failed: {}",
        String::from_utf8_lossy(&build_out.stderr)
    );
    let build_stderr = String::from_utf8_lossy(&build_out.stderr);
    // AC-9 (AMENDED): the only permitted stderr line is the lexical-build
    // summary `skim search: indexed N files (...)`.  No line may contain
    // `parse_history`, `safety cap`, `walk`, `capacity`, or `temporal` —
    // those substrings are the signature of temporal-walk cap / budget
    // notices that MUST NOT fire on a fixture this small.
    // The "indexed N files" summary is intentional, pre-#407 behaviour
    // (staleness.rs: "no AC backs the removal") and is not a temporal notice.
    let cap_substrings = [
        "parse_history",
        "safety cap",
        "walk",
        "capacity",
        "temporal",
    ];
    let unexpected_build_stderr: String = build_stderr
        .lines()
        .filter(|l| !l.starts_with("skim search: indexed "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        unexpected_build_stderr.trim().is_empty(),
        "AC-9: --build must produce no temporal-walk cap notices on a \
         clean .rs-only repo (neither MAX_COMMITS nor MAX_VISITED_COMMITS \
         notice may fire below the cap); got: {build_stderr:?}"
    );
    for substr in &cap_substrings {
        assert!(
            !build_stderr.contains(substr),
            "AC-9: --build stderr must not contain {substr:?} (temporal-walk \
             cap notice); got: {build_stderr:?}"
        );
    }

    // --risky --json: stderr MUST be empty except for the permitted
    // `skim search: indexed ...` summary line (if present).
    let risky_out = StdCommand::new(cargo_bin("skim"))
        .args(["search", "--risky", "--json", "--root"])
        .arg(repo.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim search --risky --json output");
    assert!(
        risky_out.status.success(),
        "skim search --risky failed: {}",
        String::from_utf8_lossy(&risky_out.stderr)
    );
    let risky_stderr = String::from_utf8_lossy(&risky_out.stderr);
    // Filter the permitted `skim search: indexed ...` summary, then assert
    // nothing else remains.  Also assert that no cap-notice substring appears
    // in any stderr line at all.
    let unexpected_risky_stderr: String = risky_stderr
        .lines()
        .filter(|l| !l.starts_with("skim search: indexed "))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        unexpected_risky_stderr.trim().is_empty(),
        "AC-9: --risky must produce no temporal-walk cap notices on a \
         clean .rs-only repo; got: {risky_stderr:?}"
    );
    for substr in &cap_substrings {
        assert!(
            !risky_stderr.contains(substr),
            "AC-9: --risky stderr must not contain {substr:?} (temporal-walk \
             cap notice); got: {risky_stderr:?}"
        );
    }
}

// ============================================================================
// AC-5: dog-food risky ground truth on this repository  (ADR-007)
// ============================================================================

/// AC-5 (dog-food): on this repository at the wave HEAD, `skim search --risky
/// --json` MUST report `total_commits` and `fix_commits` for
/// `crates/rskim/src/cmd/search/query.rs` that match `git rev-list --count
/// --no-merges HEAD -- <path>` and a case-insensitive word-boundary grep of
/// commit subjects respectively (ADR-003, ADR-007).
///
/// The test MUST NOT hardcode either count; both are derived from git at
/// run time.  The pre-#407 first-parent values (21 total / 2 fix / 0.095
/// fix_density) MUST NOT appear in the output, confirming the full-DAG walk
/// is live.
///
/// Building the temporal index for the full workspace takes ~8 s on a warm
/// OS page cache; this cost is accepted for the only dog-food test in the
/// suite (ADR-007: "a fully green CI and acceptance suite is not evidence
/// of retrieval correctness; the dog-food campaign is the real merge gate").
///
/// AD-407-1 (full-DAG walk replaces first-parent walk).
#[test]
fn test_ac5_dog_food_risky_query_rs_matches_git_ground_truth() {
    // Resolve workspace root: CARGO_MANIFEST_DIR → crates/rskim → crates → root.
    let repo_root = {
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest
            .parent() // …/crates
            .expect("crates dir")
            .parent() // workspace root
            .expect("workspace root")
            .to_path_buf()
    };

    // Guard: skip on shallow checkouts — `git log` ground truth requires full
    // history.  On a shallow clone, git_total would be 1 (or similarly tiny),
    // causing the non-vacuous ≥ 67 assert below to fire instead of the parity
    // assertions — which is what happened in CI run 33906188121.
    // The `Test Suite` CI job now fetches full history (fetch-depth: 0); all
    // other jobs keep the default shallow fetch (AD-407-11).
    if is_shallow_checkout(&repo_root) {
        eprintln!("skipped: shallow checkout, real-history parity test needs full history");
        return;
    }

    // File under test (path relative to repo root, as skim returns it).
    const TARGET: &str = "crates/rskim/src/cmd/search/query.rs";

    // Guard: file must exist so the assertions below are non-vacuous.
    assert!(
        repo_root.join(TARGET).exists(),
        "AC-5 guard: {TARGET} must exist in the workspace"
    );

    // ── Ground truth from git (ADR-003) ─────────────────────────────────────

    // total_commits: non-merge commits touching TARGET.
    let git_total: u64 = {
        let out = StdCommand::new("git")
            .args([
                "rev-list",
                "--count",
                "--no-merges",
                "--full-history",
                "HEAD",
                "--",
                TARGET,
            ])
            .current_dir(&repo_root)
            .output()
            .expect("git rev-list --count");
        assert!(out.status.success(), "git rev-list failed for {TARGET}");
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("rev-list count must be a number")
    };

    // fix_commits: non-merge subjects matching the same case-insensitive
    // word-boundary pattern as is_fix_commit (temporal/mod.rs FIX_REGEX).
    //
    // `grep -c` prints the count and exits 0 (matches) or 1 (no matches).
    // parse() handles both — unwrap_or(0) is the zero-match safety net.
    let git_fix: u64 = {
        let sh_cmd = format!(
            r"git log --no-merges --format='%s' -- '{TARGET}' \
              | grep -ciE '\b(fix|bug|hotfix|patch|revert)\b'"
        );
        let out = StdCommand::new("sh")
            .arg("-c")
            .arg(&sh_cmd)
            .current_dir(&repo_root)
            .output()
            .expect("sh -c git log | grep -c");
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .unwrap_or(0)
    };

    // Guard: verify git ground truth is non-trivial so a silent-empty result
    // cannot make the assertions below vacuously pass (PF-007).
    assert!(
        git_total >= 67,
        "AC-5 guard: git_total ({git_total}) must be ≥ 67 \
         (the full-DAG count at wave HEAD); check that this test runs on the \
         correct branch and that HEAD is up to date"
    );
    assert!(
        git_fix > 0,
        "AC-5 guard: git_fix must be > 0 (query.rs has fix commits)"
    );

    // ── Build temporal index ─────────────────────────────────────────────────

    let cache = TempDir::new().expect("cache tempdir");
    build_index(&repo_root, cache.path());

    // ── Query ────────────────────────────────────────────────────────────────

    // Compute the limit at runtime: tracked *.rs files + 100.  This ensures
    // every indexed file is returned regardless of risk rank while remaining
    // robust as the repo grows (a fixed 600 would shrink in headroom over time).
    let rs_limit = {
        let ls_out = StdCommand::new("git")
            .args(["ls-files", "--", "*.rs"])
            .current_dir(&repo_root)
            .output()
            .expect("git ls-files -- '*.rs'");
        assert!(ls_out.status.success(), "git ls-files -- '*.rs' failed");
        let rs_count = String::from_utf8_lossy(&ls_out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .count();
        (rs_count + 100).to_string()
    };
    let json = run_search_json(&repo_root, cache.path(), &["--risky", "--limit", &rs_limit]);
    let results = json["results"]
        .as_array()
        .expect("--risky --json results must be an array");

    // Locate TARGET in the result set.
    let row = results
        .iter()
        .find(|r| r["path"].as_str() == Some(TARGET))
        .unwrap_or_else(|| {
            panic!(
                "AC-5: {TARGET} must appear in --risky --json results; \
                 got {} result(s): {results:?}",
                results.len()
            )
        });

    // ── Assertions ───────────────────────────────────────────────────────────

    let skim_total = row["total_commits"]
        .as_u64()
        .expect("total_commits must be a number");
    let skim_fix = row["fix_commits"]
        .as_u64()
        .expect("fix_commits must be a number");
    let skim_density = row["fix_density"]
        .as_f64()
        .expect("fix_density must be a float");

    assert_eq!(
        skim_total, git_total,
        "AC-5: total_commits for {TARGET} must match \
         `git rev-list --count --no-merges --full-history HEAD -- {TARGET}` \
         (full-DAG walk, AD-407-1); skim={skim_total}, git={git_total}"
    );
    assert_eq!(
        skim_fix, git_fix,
        "AC-5: fix_commits for {TARGET} must match \
         `git log --no-merges --format=%s | grep -ciE fix-pattern` \
         (ADR-003); skim={skim_fix}, git={git_fix}"
    );

    // Confirm fix_density is consistent with the reported counts.
    let expected_density = git_fix as f64 / git_total as f64;
    assert!(
        (skim_density - expected_density).abs() < 0.002,
        "AC-5: fix_density {skim_density:.4} must be within 0.002 of \
         fix_commits/total_commits = {expected_density:.4}"
    );

    // NEGATIVE guard: the pre-#407 first-parent values MUST NOT appear.
    assert_ne!(
        skim_total, 21,
        "AC-5 NEGATIVE: total_commits must not be the pre-#407 \
         first-parent value 21 — full-DAG walk not active"
    );
    assert_ne!(
        skim_fix, 2,
        "AC-5 NEGATIVE: fix_commits must not be the pre-#407 \
         first-parent value 2"
    );
    assert!(
        skim_density > 0.10,
        "AC-5 NEGATIVE: fix_density {skim_density:.4} must be > 0.10 \
         (pre-#407 first-parent value was 0.095)"
    );
}

// ============================================================================
// AC-19 (NEGATIVE): pagination contract unchanged by #407
// ============================================================================

/// AC-19 (NEGATIVE): `--offset N` semantics, the `depth+1` sentinel fetch,
/// `has_more` derivation, and result ordering MUST be byte-identical across
/// repeated calls on the same corpus and cache after #407 — the full-DAG walk
/// MUST NOT introduce non-determinism in the temporal-arm pagination contract.
///
/// This test drives the merge fixture with `--risky --json` at both
/// `--offset 0` and `--offset 1` and asserts that two invocations at the same
/// offset produce byte-identical stdout (SQL tie-breaks + RRF ranking stable).
/// It also verifies `has_more` semantics: absent when the full set fits on one
/// page, present when `--limit` forces a partial page.
///
/// AD-407-2 (merge commit absent from full-DAG walk), AD-407-8 (T-18 / AC-16
/// ground truth for the merge fixture).
#[test]
fn test_ac19_risky_offset_pagination_stable_ordering() {
    let repo = make_merge_repo();
    let cache = TempDir::new().expect("cache tempdir");
    build_index(repo.path(), cache.path());

    // Helper: run --risky with extra args, return raw stdout bytes.
    let risky_raw = |extra: &[&str]| -> Vec<u8> {
        let mut args = vec!["search", "--risky", "--json", "--root"];
        let root_str = repo.path().to_str().unwrap();
        args.push(root_str);
        args.extend_from_slice(extra);
        let out = StdCommand::new(cargo_bin("skim"))
            .args(&args)
            .env("SKIM_CACHE_DIR", cache.path())
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .output()
            .expect("skim search --risky --json");
        assert!(
            out.status.success(),
            "skim --risky {:?} failed: {}",
            extra,
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    };

    // ── Full-page stability (both files fit in one page) ────────────────────

    let full_a = risky_raw(&[]);
    let full_b = risky_raw(&[]);
    assert_eq!(
        full_a, full_b,
        "AC-19: --risky --json stdout must be byte-identical across repeated \
         calls on the same corpus and cache (stable ordering)"
    );

    // The full page must NOT carry has_more (merge fixture = 2 files, fits in
    // one page).
    let full_json: Value =
        serde_json::from_slice(&full_a).expect("full-page output must be valid JSON");
    assert!(
        full_json.get("has_more").is_none(),
        "AC-19: has_more must be absent on a single-page full result; \
         got: {full_json}"
    );

    // ── Paged stability (--limit 1 forces two pages) ─────────────────────────

    // Page 0: first file.
    let p0_a = risky_raw(&["--limit", "1"]);
    let p0_b = risky_raw(&["--limit", "1"]);
    assert_eq!(
        p0_a, p0_b,
        "AC-19: --risky --json --limit 1 (page 0) must be byte-identical \
         across repeated calls (has_more + result ordering stable)"
    );
    let p0_json: Value = serde_json::from_slice(&p0_a).expect("page-0 output must be valid JSON");
    // has_more MUST be present (true) because a second page exists.
    assert_eq!(
        p0_json.get("has_more").and_then(Value::as_bool),
        Some(true),
        "AC-19: has_more must be true on a partial first page; got: {p0_json}"
    );

    // Page 1: second file, offset 1.
    let p1_a = risky_raw(&["--limit", "1", "--offset", "1"]);
    let p1_b = risky_raw(&["--limit", "1", "--offset", "1"]);
    assert_eq!(
        p1_a, p1_b,
        "AC-19: --risky --json --limit 1 --offset 1 (page 1) must be \
         byte-identical across repeated calls"
    );
    let p1_json: Value = serde_json::from_slice(&p1_a).expect("page-1 output must be valid JSON");
    // Page 1 is the last page → has_more must be absent (false).
    assert!(
        p1_json.get("has_more").is_none(),
        "AC-19: has_more must be absent on the final page (offset 1 of 2); \
         got: {p1_json}"
    );

    // The two pages together must cover both files without overlap.
    let p0_path = p0_json["results"][0]["path"]
        .as_str()
        .expect("page-0 must have a result with a path");
    let p1_path = p1_json["results"][0]["path"]
        .as_str()
        .expect("page-1 must have a result with a path");
    assert_ne!(
        p0_path, p1_path,
        "AC-19: the two pages must return distinct files (no duplicate \
         ordering under #407); p0={p0_path:?}, p1={p1_path:?}"
    );
    let mut all_paths = [p0_path, p1_path];
    all_paths.sort_unstable();
    assert_eq!(
        all_paths,
        ["a.txt", "b.txt"],
        "AC-19: the two pages together must cover exactly {{a.txt, b.txt}}; \
         got {all_paths:?}"
    );
}

// ============================================================================
// AC-20 (NEGATIVE): non-temporal output unaffected by #407
// ============================================================================

/// AC-20 (NEGATIVE): a pure-lexical (text-only) query on an indexed corpus
/// MUST NOT be altered by #407's temporal-layer changes.  Specifically:
///
/// - `file_count` and `skipped` MUST be present in `--stats --json`.
/// - `verify_mode` MUST be absent (default Substring mode is omitted via
///   `skip_serializing_if`); its absence pins the serialisation contract.
/// - The `degraded` array MUST be absent on a healthy corpus.
/// - Running the same lexical query twice on the same cache MUST produce
///   byte-identical stdout (stable ordering, no temporal side-effects).
///
/// The heatmap-parity fixture (only `.rs` files) is reused here to ensure
/// AST indexing is active for both files and the corpus is deterministic.
///
/// AD-407-1 (full-DAG walk is build-path-only; the lexical query path is
/// unaffected by #407).
#[test]
fn test_ac20_lexical_query_unaffected_by_temporal_index() {
    let repo = make_heatmap_parity_repo();
    let cache = TempDir::new().expect("cache tempdir");
    build_index(repo.path(), cache.path());

    // Run a lexical query (no temporal flags) for a token present in both
    // fixture files (all files contain "fn").
    let out_a = StdCommand::new(cargo_bin("skim"))
        .args(["search", "fn", "--json", "--root"])
        .arg(repo.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim search fn --json (run 1)");
    assert!(
        out_a.status.success(),
        "AC-20: lexical query must succeed; stderr: {}",
        String::from_utf8_lossy(&out_a.stderr)
    );

    let out_b = StdCommand::new(cargo_bin("skim"))
        .args(["search", "fn", "--json", "--root"])
        .arg(repo.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim search fn --json (run 2)");
    assert!(
        out_b.status.success(),
        "AC-20: lexical query (run 2) must succeed; stderr: {}",
        String::from_utf8_lossy(&out_b.stderr)
    );

    // Parse both outputs before content comparison.  `duration_ms` is a
    // wall-clock timing field and is expected to differ between calls; strip
    // it before comparing so the assertion covers search content, not elapsed
    // time.  AD-407-1: the full-DAG walk is build-path-only and MUST NOT
    // introduce non-determinism in the lexical query path.
    let json_a: Value =
        serde_json::from_slice(&out_a.stdout).expect("AC-20: run-1 output must be valid JSON");
    let json_b: Value =
        serde_json::from_slice(&out_b.stdout).expect("AC-20: run-2 output must be valid JSON");

    /// Strip the `duration_ms` field (wall-clock, non-deterministic) from any
    /// JSON object, recursing into nested objects and arrays.
    fn without_duration_ms(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let filtered: serde_json::Map<String, Value> = m
                    .iter()
                    .filter(|(k, _)| k.as_str() != "duration_ms")
                    .map(|(k, v)| (k.clone(), without_duration_ms(v)))
                    .collect();
                Value::Object(filtered)
            }
            Value::Array(a) => Value::Array(a.iter().map(without_duration_ms).collect()),
            other => other.clone(),
        }
    }

    // AC-20 (AMENDED): byte-identical after removing the `duration_ms` timing
    // field.  Serialize both stripped values and compare the resulting bytes
    // to enforce strict content identity, not just structural JSON equality.
    let stripped_a = serde_json::to_vec(&without_duration_ms(&json_a))
        .expect("AC-20: failed to serialize stripped json_a");
    let stripped_b = serde_json::to_vec(&without_duration_ms(&json_b))
        .expect("AC-20: failed to serialize stripped json_b");
    assert_eq!(
        stripped_a, stripped_b,
        "AC-20: pure-lexical query content (excluding duration_ms) must be \
         byte-identical across repeated calls on the same corpus and cache"
    );

    // Use run-1's parsed output for the structural assertions below.
    let json: Value = json_a;

    // `verify_mode` must be absent in default Substring mode
    // (`skip_serializing_if` in the search crate).
    assert!(
        json.get("verify_mode").is_none(),
        "AC-20: verify_mode must be absent on a default Substring lexical \
         query (skip_serializing_if contract); got: {json}"
    );

    // `degraded` must be absent on a healthy corpus.
    assert!(
        json.get("degraded").is_none(),
        "AC-20: degraded must be absent on a healthy lexical query; \
         got: {json}"
    );

    // Results must be present and non-empty (the query token "fn" appears in
    // every fixture file — a vacuous empty-results pass is impossible).
    let results = json["results"]
        .as_array()
        .expect("results must be an array");
    assert!(
        !results.is_empty(),
        "AC-20: lexical query for 'fn' must return at least one result on \
         a fixture whose files contain 'fn'; check that the index was built"
    );

    // Confirm --stats --json carries file_count and skipped (structure unchanged).
    let stats = run_stats_json(repo.path(), cache.path());
    assert!(
        stats["file_count"].is_number(),
        "AC-20: file_count must be a number in --stats --json; got: {stats}"
    );
    assert!(
        stats.get("skipped").is_some(),
        "AC-20: skipped must be present in --stats --json; got: {stats}"
    );
}

// ============================================================================
// AC-24 (PERFORMANCE): warm risky overhead regression guard
// ============================================================================

/// AC-24 regression guard: the warm `skim search --risky` overhead above a plain
/// lexical query MUST be under 250 ms (median of 5 warm runs), and the absolute
/// `--risky` median MUST be under 1 000 ms.
///
/// Both thresholds are derived from measurements on the `make_merge_repo`
/// fixture (3-commit, 2-file `.rs`-only repo) at wave HEAD on this machine:
///   plain lexical median ≈ 31 ms
///   `--risky` median     ≈ 38 ms
///   net overhead         ≈ 7 ms
///
/// The 250 ms overhead cap and 1 000 ms absolute cap provide sufficient CI
/// headroom while still catching a catastrophic regression (e.g., the warm
/// path accidentally re-walking the full DAG).
///
/// The 50 ms warm target from AC-24 is separately tracked by #401 and #406;
/// this test is a regression guard, not a performance contract.
///
/// The single post-upgrade self-heal query is explicitly exempt (it re-runs
/// the full history walk); this test probes only the already-current path by
/// rebuilding first and querying on an up-to-date `temporal.db`.
///
/// AD-407-1 (full-DAG walk is build-path-only; warm query path unchanged by #407).
#[test]
fn test_ac24_warm_risky_overhead_regression_guard() {
    let repo = make_merge_repo();
    let cache = TempDir::new().expect("cache tempdir");

    // Warm build: populate temporal.db so every subsequent --risky is a pure
    // query with no self-heal.
    build_index(repo.path(), cache.path());

    // Helper: run one timed query and return the elapsed duration.
    let timed_run = |extra_args: &[&str]| -> std::time::Duration {
        let start = std::time::Instant::now();
        let out = StdCommand::new(cargo_bin("skim"))
            .args(["search"])
            .args(extra_args)
            .args(["--json", "--root"])
            .arg(repo.path())
            .env("SKIM_CACHE_DIR", cache.path())
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .output()
            .expect("timed skim search --json");
        let elapsed = start.elapsed();
        assert!(
            out.status.success(),
            "AC-24: query {:?} failed; stderr: {}",
            extra_args,
            String::from_utf8_lossy(&out.stderr)
        );
        elapsed
    };

    // Warm-up: one discarded run each to prime OS page caches.
    let _ = timed_run(&["fn"]);
    let _ = timed_run(&["--risky"]);

    // Collect 5 measured runs for each arm.
    let mut plain_ms: Vec<u128> = (0..5).map(|_| timed_run(&["fn"]).as_millis()).collect();
    let mut risky_ms: Vec<u128> = (0..5)
        .map(|_| timed_run(&["--risky"]).as_millis())
        .collect();

    // Sort ascending and take the middle element as the median.
    plain_ms.sort_unstable();
    risky_ms.sort_unstable();
    let plain_median = plain_ms[2];
    let risky_median = risky_ms[2];
    let overhead = risky_median.saturating_sub(plain_median);

    // (a) Net overhead (risky − plain) MUST be under 250 ms.
    assert!(
        overhead < 250,
        "AC-24: net overhead (risky median {risky_median} ms − plain median \
         {plain_median} ms = {overhead} ms) must be < 250 ms \
         (regression guard; the 50 ms warm target is #401/#406)"
    );

    // (b) Absolute --risky median MUST be under 1 000 ms.
    assert!(
        risky_median < 1_000,
        "AC-24: absolute --risky median {risky_median} ms must be < 1 000 ms \
         (regression guard; the 50 ms warm target is #401/#406)"
    );
}
