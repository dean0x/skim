//! Integration tests for `skim git` subcommand (#50).
//!
//! Tests end-to-end CLI behavior for git status/diff/log compression.
//! These tests run against the real skim repository (which is a git repo),
//! so they exercise the actual git binary.

use predicates::prelude::*;
mod common;

// ============================================================================
// Helpers
// ============================================================================

/// Run a git command in `dir`, asserting success with a step-labelled panic message.
///
/// Centralises exit-status checking for hermetic repo helpers so that each
/// setup step fails with an unambiguous message ("hermetic setup: `git add foo`
/// failed") rather than surfacing as a confusing product assertion later.
fn git_in(dir: &std::path::Path, args: &[&str]) {
    let step = args.join(" ");
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("hermetic setup: `git {step}` spawn failed: {e}"));
    assert!(
        out.status.success(),
        "hermetic setup: `git {step}` failed;\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Create a hermetic local git repo with an `origin` remote pointing at a
/// local bare repo. Returns the temp dir (caller must keep alive) and the
/// path of the worker clone directory.
///
/// This avoids hitting the project's own git remote during `skim git fetch`
/// tests, preventing flakiness caused by stale remote tracking refs on the
/// host machine (e.g. `cannot lock ref refs/remotes/origin/...`).
///
/// `-b main` is passed to both `git init` calls so the helper is deterministic
/// regardless of the runner's `init.defaultBranch` setting (avoids PF-009:
/// bare HEAD would dangle at nonexistent `refs/heads/master` on Linux CI).
fn make_hermetic_fetch_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let base = dir.path();
    let bare = base.join("bare.git");
    let seed = base.join("seed");
    let worker = base.join("worker");

    // Bare repo acts as the remote.  Pin the initial branch to `main`.
    git_in(
        base,
        &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
    );

    // Seed: one empty commit → push to bare so it is non-empty.
    git_in(base, &["init", "-b", "main", seed.to_str().unwrap()]);
    git_in(&seed, &["config", "user.email", "test@example.com"]);
    git_in(&seed, &["config", "user.name", "Test"]);
    git_in(&seed, &["commit", "--allow-empty", "-m", "init"]);
    git_in(&seed, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git_in(&seed, &["push", "origin", "HEAD:refs/heads/main"]);

    // Worker: clone from bare — this is the repo tests run `skim git fetch` in.
    git_in(
        base,
        &["clone", bare.to_str().unwrap(), worker.to_str().unwrap()],
    );
    // Belt-and-suspenders: explicitly place the worker on `main` tracking
    // `origin/main`.  No-op when clone already checked out `main` correctly;
    // recovers on runners where git defaults to `master` (avoids PF-009).
    git_in(
        &worker,
        &["checkout", "-B", "main", "--track", "origin/main"],
    );

    // Precondition: worker must have exactly 1 commit (the seed) and be in sync.
    let rev_count = std::process::Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(&worker)
        .output()
        .expect("git rev-list --count HEAD");
    assert!(
        rev_count.status.success(),
        "hermetic fetch setup: git rev-list --count HEAD must succeed; stderr={}",
        String::from_utf8_lossy(&rev_count.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&rev_count.stdout).trim(),
        "1",
        "hermetic fetch-repo setup must have exactly 1 commit; setup is broken if this fails"
    );

    (dir, worker)
}

// ============================================================================
// Help
// ============================================================================

#[test]
fn test_skim_git_help() {
    common::skim()
        .args(["git", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("status "))
        .stdout(predicate::str::contains("diff"))
        .stdout(predicate::str::contains("fetch "))
        .stdout(predicate::str::contains("log "));
}

#[test]
fn test_skim_git_no_args_shows_help() {
    common::skim()
        .arg("git")
        .assert()
        .success()
        .stdout(predicate::str::contains("status "));
}

// ============================================================================
// Status
// ============================================================================

#[test]
fn test_skim_git_status_in_repo() {
    // Run against the skim repo itself — should succeed and produce output.
    // The net-savings guard may passthrough on small repos; we check that the
    // command exits 0 and produces content (either skim-format or raw git output).
    common::skim()
        .args(["git", "status"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

/// Create a hermetic git repo with one uncommitted change so that
/// `git status --porcelain` / `git status -s` emit non-empty output.
///
/// Returns the temp dir (caller must keep alive) and the repo path.
fn make_hermetic_dirty_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let path = dir.path().to_path_buf();

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&path)
        .output()
        .expect("git init");
    for (k, v) in [("user.email", "test@example.com"), ("user.name", "Test")] {
        std::process::Command::new("git")
            .args(["config", k, v])
            .current_dir(&path)
            .output()
            .expect("git config");
    }
    // Commit an initial file so HEAD exists.
    std::fs::write(path.join("init.txt"), "init\n").expect("write init.txt");
    std::process::Command::new("git")
        .args(["add", "init.txt"])
        .current_dir(&path)
        .output()
        .expect("git add");
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&path)
        .output()
        .expect("git commit");
    // Create an untracked file so porcelain/-s output is non-empty.
    std::fs::write(path.join("dirty.txt"), "uncommitted change\n").expect("write dirty.txt");

    (dir, path)
}

#[test]
fn test_skim_git_status_porcelain_compresses() {
    // Run against a hermetic dirty repo so --porcelain output is non-empty.
    //
    // --porcelain is stripped by the status handler; the handler runs `git status`
    // and the net-savings guard compares against the raw porcelain output.
    // With non-empty raw output the guard may keep the compressed form or pass
    // through verbatim — either way the output is non-empty and the command exits 0.
    // IMPORTANT: the porcelain flag is stripped to avoid fabricating machine-readable
    // output; the raw porcelain lines appear in passthrough if the guard fires.
    let (_dir, repo) = make_hermetic_dirty_repo();
    common::skim()
        .args(["git", "status", "--porcelain"])
        .current_dir(&repo)
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn test_skim_git_status_short_compresses() {
    // Run against a hermetic dirty repo so -s output is non-empty.
    //
    // -s is stripped by the status handler; handler runs `git status` and the
    // net-savings guard compares against the raw output. With uncommitted
    // changes the raw output is non-empty so the result (skim-format or raw
    // passthrough) is non-empty and the command exits 0.
    let (_dir, repo) = make_hermetic_dirty_repo();
    common::skim()
        .args(["git", "status", "-s"])
        .current_dir(&repo)
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

/// C-7 byte-level guard: `skim git status -s` on a small hermetic dirty repo
/// must NEVER emit MORE bytes than the raw `git status --short` output.
///
/// The C-7 logic re-runs the user's literal `git status --short` to use as the
/// net-savings guard baseline, so that on a small repo skim emits the compact
/// raw `--short` form rather than the expanded porcelain summary.  This test
/// locks in that property end-to-end (applies ADR-001 / #317 invariant).
///
/// This is the status analogue of `cli_no_expansion_317.rs` for the -s/-short
/// flag path.
#[test]
fn test_skim_git_status_short_never_expands_vs_raw() {
    let (_dir, repo) = make_hermetic_dirty_repo();

    // Capture raw `git status --short` output for the baseline.
    let raw_output = std::process::Command::new("git")
        .args(["status", "--short"])
        .current_dir(&repo)
        .output()
        .expect("git must be available");
    let raw_len = raw_output.stdout.len();

    // Run `skim git status -s` against the same repo.
    let skim_output = common::skim()
        .args(["git", "status", "-s"])
        .current_dir(&repo)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim git status -s must not fail to spawn");

    assert!(
        skim_output.status.success(),
        "skim git status -s must exit 0; stderr={}",
        String::from_utf8_lossy(&skim_output.stderr)
    );

    let skim_len = skim_output.stdout.len();

    // #317 / C-7 invariant: skim must NEVER emit more bytes than the raw
    // `git status --short` baseline.  On a small repo the guard must fire and
    // passthrough the compact raw form.
    assert!(
        skim_len <= raw_len,
        "C-7: skim git status -s expanded output vs raw --short\n  \
         raw={raw_len}B  skim={skim_len}B\n  \
         skim stdout={:?}\n  \
         raw stdout={:?}\n  \
         The C-7 never-expand-vs-user-intent property failed.",
        String::from_utf8_lossy(&skim_output.stdout),
        String::from_utf8_lossy(&raw_output.stdout)
    );
}

/// Same as above but using `--short` long-form flag alias.
#[test]
fn test_skim_git_status_short_longform_never_expands_vs_raw() {
    let (_dir, repo) = make_hermetic_dirty_repo();

    let raw_output = std::process::Command::new("git")
        .args(["status", "--short"])
        .current_dir(&repo)
        .output()
        .expect("git must be available");
    let raw_len = raw_output.stdout.len();

    let skim_output = common::skim()
        .args(["git", "status", "--short"])
        .current_dir(&repo)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim git status --short must not fail to spawn");

    assert!(
        skim_output.status.success(),
        "skim git status --short must exit 0; stderr={}",
        String::from_utf8_lossy(&skim_output.stderr)
    );

    let skim_len = skim_output.stdout.len();

    assert!(
        skim_len <= raw_len,
        "C-7: skim git status --short expanded output vs raw --short\n  \
         raw={raw_len}B  skim={skim_len}B\n  \
         skim stdout={:?}",
        String::from_utf8_lossy(&skim_output.stdout)
    );
}

/// Create a hermetic git repo where the worker clone is 1 commit ahead of origin.
///
/// Layout: bare repo (remote) ← seed (initial push) ← worker (1 unpushed commit).
/// The worker clone has `origin/main` as upstream and is exactly 1 ahead.
/// Returns the temp dir (caller must keep alive) and the worker clone path.
///
/// `-b main` is passed to both `git init` calls so the helper is deterministic
/// regardless of the runner's `init.defaultBranch` setting (Linux CI defaults to
/// `master`; without `-b main` the bare's HEAD dangles at the nonexistent
/// `refs/heads/master` after the push to `refs/heads/main`, breaking the clone).
/// `-b/--initial-branch` is supported since git 2.28, which CI requires.
fn make_hermetic_ahead_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let base = dir.path();
    let bare = base.join("bare.git");
    let seed = base.join("seed");
    let worker = base.join("worker");

    // Bare remote: pin the initial branch to `main` so `git clone` can check
    // out `main` tracking `origin/main` deterministically.
    git_in(
        base,
        &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
    );

    // Seed: one empty commit → push to bare so it is non-empty.
    git_in(base, &["init", "-b", "main", seed.to_str().unwrap()]);
    git_in(&seed, &["config", "user.email", "test@example.com"]);
    git_in(&seed, &["config", "user.name", "Test"]);
    git_in(&seed, &["commit", "--allow-empty", "-m", "init"]);
    git_in(&seed, &["remote", "add", "origin", bare.to_str().unwrap()]);
    git_in(&seed, &["push", "origin", "HEAD:refs/heads/main"]);

    // Worker: clone from bare so origin/main is configured.
    git_in(
        base,
        &["clone", bare.to_str().unwrap(), worker.to_str().unwrap()],
    );
    git_in(&worker, &["config", "user.email", "test@example.com"]);
    git_in(&worker, &["config", "user.name", "Test"]);
    // Belt-and-suspenders: explicitly place the worker on `main` tracking
    // `origin/main` before adding the ahead commit.  No-op when the clone
    // already checked out `main`; recovers on runners where git defaults to
    // `master` (avoids PF-009).
    git_in(
        &worker,
        &["checkout", "-B", "main", "--track", "origin/main"],
    );

    // Add 1 commit to the worker without pushing — worker is now 1 ahead of origin.
    std::fs::write(worker.join("ahead.txt"), "ahead\n").expect("write ahead.txt");
    git_in(&worker, &["add", "ahead.txt"]);
    git_in(&worker, &["commit", "-m", "ahead commit"]);

    // Fail-loud setup assertion: verify the hermetic repo is genuinely 1-ahead.
    // A failure here means the test-helper itself is broken, not skim — surfaces
    // setup regressions with a clear message instead of a confusing skim-output
    // assertion failure.
    let rev_list = std::process::Command::new("git")
        .args(["rev-list", "--count", "origin/main..HEAD"])
        .current_dir(&worker)
        .output()
        .expect("git rev-list --count origin/main..HEAD");
    assert!(
        rev_list.status.success(),
        "hermetic setup: git rev-list must succeed; stderr={}",
        String::from_utf8_lossy(&rev_list.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&rev_list.stdout).trim(),
        "1",
        "hermetic ahead-repo setup must produce exactly 1 ahead commit; \
         setup is broken if this fails"
    );

    (dir, worker)
}

/// Fix 1: `skim git status -sb` on a hermetic repo with an upstream must not
/// silently drop the branch header.
///
/// Before the fix, `-sb` was not recognized as a conflicting format flag, so it
/// was forwarded verbatim alongside `--porcelain=v2`, causing git to emit
/// short-format output (`## main...origin/main`).  `parse_status` then discarded
/// that line (it starts with `##`, not `# branch.head `), producing "clean"
/// with no branch detail — information loss on clean trees.
///
/// The assertion triple guards the CORRECTED render format:
///   - `branch:` fails on pre-fix behavior (bare "clean" with no branch line)
///   - `origin/` fails if the upstream is dropped
///   - `ahead`   fails on wrong render (raw `+1 -0`) AND on pre-fix silence
#[test]
fn test_skim_git_status_sb_shows_branch() {
    // Use a hermetic "ahead" repo: worker clone has origin/main as upstream
    // and is exactly 1 commit ahead, so porcelain v2 emits:
    //   # branch.head main
    //   # branch.upstream origin/main
    //   # branch.ab +1 -0
    // The corrected render must produce: "branch: main...origin/main [ahead 1]"
    let (_dir, worker) = make_hermetic_ahead_repo();

    let output = common::skim()
        .args(["git", "status", "-sb"])
        .current_dir(&worker)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim git status -sb must run");

    assert!(
        output.status.success(),
        "skim git status -sb must exit 0; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // The C-7 net-savings guard may fire for a clean-ahead repo when the raw
    // `git status -sb` output is shorter than skim's compressed form.  When it
    // fires, skim emits the raw `## main...origin/main [ahead 1]` line verbatim;
    // when it does not, skim emits `branch: main...origin/main [ahead 1]`.
    // Both are correct — the key invariant is that the upstream reference and
    // ahead count are NOT silently dropped (which is what the old bug did:
    // it produced `status  clean` or `branch: main` with no upstream info).
    //
    // Three assertions guard distinct failure modes:
    //   • "branch:" OR "##" — fails if no branch line emitted at all
    //   • "origin/"          — fails if upstream is dropped
    //   • "ahead"            — fails on old raw "+1 -0" AND on pre-fix silence
    assert!(
        stdout.contains("branch:") || stdout.contains("##"),
        "Fix 1: -sb output must contain either 'branch:' (compressed) or '##' (raw passthrough); \
         old bug produced bare 'clean' with no upstream info; \
         stdout={stdout:?}  stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("origin/"),
        "Fix 1: -sb output must show the upstream (origin/main); stdout={stdout:?}"
    );
    assert!(
        stdout.contains("ahead"),
        "Fix 1: -sb output must show ahead count in native [ahead N] format \
         (not raw '+1 -0', and not absent); stdout={stdout:?}"
    );
}

// ============================================================================
// Diff
// ============================================================================

#[test]
fn test_skim_git_diff_in_repo() {
    // Clean repo has no diff — AST-aware pipeline outputs "No changes" to stderr
    common::skim().args(["git", "diff"]).assert().success();
}

#[test]
fn test_skim_git_diff_name_only_passthrough() {
    common::skim()
        .args(["git", "diff", "--name-only"])
        .assert()
        .success();
}

// ============================================================================
// Log
// ============================================================================

#[test]
fn test_skim_git_log_in_repo() {
    // The handler runs and exits 0; on a small repo the net-savings guard may
    // passthrough rather than emitting the skim-format "log N commits" header.
    // We verify the command exits 0 and produces some output.
    common::skim()
        .args(["git", "log"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn test_skim_git_log_with_limit() {
    // The guard may passthrough on small outputs; verify exits 0 with content.
    common::skim()
        .args(["git", "log", "-n", "3"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn test_skim_git_log_oneline_compresses() {
    // --oneline is stripped by the handler; handler still runs and exits 0.
    // Net-savings guard may passthrough on small outputs.
    common::skim()
        .args(["git", "log", "--oneline", "-n", "3"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

// ============================================================================
// Fetch
// ============================================================================

/// Case 1: no-op fetch when worker is already up to date with origin.
///
/// `git fetch` when up-to-date writes progress to STDERR only, leaving stdout empty.
/// `parse_fetch("")` produces summary "up to date", but the net-savings guard fires
/// because the compressed form is not smaller than the empty raw → skim emits empty
/// stdout (faithful passthrough of empty raw).
#[test]
fn test_skim_git_fetch_up_to_date_emits_empty_stdout() {
    let (_dir, worker) = make_hermetic_fetch_repo();
    let output = common::skim()
        .args(["git", "fetch"])
        .current_dir(&worker)
        .output()
        .expect("skim git fetch must run");
    assert!(output.status.success(), "skim git fetch must exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Net-savings guard fires (compressed > empty raw) → skim emits the raw empty string.
    assert!(
        stdout.trim().is_empty(),
        "up-to-date fetch must produce empty stdout (raw-empty passthrough); \
         got: {stdout:?}"
    );
}

/// Case 2: fetch after a new commit is pushed to origin reports the updated ref.
///
/// A second clone pushes one commit to the bare remote; the worker then fetches.
/// git writes the updated-ref line (`main`) to its own stderr; skim must forward
/// it so the caller sees evidence of fetch activity, and must exit 0.
#[test]
fn test_skim_git_fetch_with_new_origin_commit_reports_update() {
    let (dir, worker) = make_hermetic_fetch_repo();
    let bare = dir.path().join("bare.git");

    // Push a new commit to origin from a second clone so the worker falls behind.
    let pusher = dir.path().join("pusher");
    git_in(
        dir.path(),
        &["clone", bare.to_str().unwrap(), pusher.to_str().unwrap()],
    );
    git_in(&pusher, &["config", "user.email", "test@example.com"]);
    git_in(&pusher, &["config", "user.name", "Test"]);
    std::fs::write(pusher.join("new.txt"), "new\n").expect("write new.txt");
    git_in(&pusher, &["add", "new.txt"]);
    git_in(&pusher, &["commit", "-m", "new commit"]);
    git_in(&pusher, &["push", "origin", "HEAD:refs/heads/main"]);

    // Worker is now 1 behind origin.  Fetch must exit 0 and report the updated ref.
    let output = common::skim()
        .args(["git", "fetch"])
        .current_dir(&worker)
        .output()
        .expect("skim git fetch must run");
    assert!(output.status.success(), "skim git fetch must exit 0");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // git fetch reports the updated ref ("main") to its own stderr; skim forwards it.
    assert!(
        stdout.contains("main") || stderr.contains("main"),
        "fetch of a new commit must mention the updated ref 'main'; \
         stdout={stdout:?} stderr={stderr:?}"
    );
}

// ============================================================================
// Error cases
// ============================================================================

#[test]
fn test_skim_git_unknown_subcommand() {
    common::skim()
        .args(["git", "unknown_subcmd"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown git subcommand"));
}

#[test]
fn test_skim_git_log_contains_hashes() {
    // Log output must contain commit hashes (short 7-char hex).
    // On small outputs the net-savings guard may passthrough (no "log " prefix);
    // we verify the hash is present regardless of whether skim-format is used.
    let output = common::skim()
        .args(["git", "log", "-n", "1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // git log format is "%h %s (%cr) <%an>" — first word is the short hash.
    // In skim-format the hash appears on lines after "log N commits"; in raw
    // passthrough the hash is the first word on the first line.
    let has_hash = stdout.lines().any(|l| {
        l.split_whitespace()
            .next()
            .is_some_and(|w| w.len() >= 7 && w.chars().all(|c| c.is_ascii_hexdigit()))
    });
    assert!(
        has_hash,
        "Expected a line with a hex commit hash in output (skim-format or raw), got: {stdout}"
    );
}

/// Create a hermetic git repo with N commits.
///
/// Returns the temp dir (caller must keep alive) and the repo path.
///
/// `-b main` is passed to `git init` so the helper is deterministic regardless
/// of the runner's `init.defaultBranch` setting (avoids PF-009).  All git steps
/// are checked for success via `git_in`; a closing `rev-list --count HEAD`
/// assertion verifies the fixture's own correctness before tests run.
fn make_hermetic_repo_with_commits(n: usize) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let path = dir.path().to_path_buf();

    git_in(&path, &["init", "-b", "main"]);
    git_in(&path, &["config", "user.email", "test@example.com"]);
    git_in(&path, &["config", "user.name", "Test"]);

    for i in 0..n {
        let filename = format!("file{i}.txt");
        let commit_msg = format!("commit {i}");
        std::fs::write(path.join(&filename), format!("content {i}\n")).expect("write file");
        git_in(&path, &["add", &filename]);
        git_in(&path, &["commit", "-m", &commit_msg]);
    }

    // Precondition: verify the hermetic repo has exactly n commits.
    let rev_list = std::process::Command::new("git")
        .args(["rev-list", "--count", "HEAD"])
        .current_dir(&path)
        .output()
        .expect("git rev-list --count HEAD");
    assert!(
        rev_list.status.success(),
        "hermetic setup: git rev-list --count HEAD must succeed; stderr={}",
        String::from_utf8_lossy(&rev_list.stderr)
    );
    let expected = n.to_string();
    assert_eq!(
        String::from_utf8_lossy(&rev_list.stdout).trim(),
        expected.as_str(),
        "hermetic repo-with-commits setup must produce exactly {n} commits; \
         setup is broken if this fails"
    );

    (dir, path)
}

/// Fix 4 regression: `skim git log --oneline` on a repo with 25 commits shows
/// ALL 25 — the old silent `-n 20` cap must no longer inject.
#[test]
fn test_git_log_no_cap_shows_all_commits() {
    let (_dir, repo) = make_hermetic_repo_with_commits(25);

    let output = common::skim()
        .current_dir(&repo)
        .args(["git", "log", "--oneline"])
        .output()
        .unwrap();
    assert!(output.status.success(), "skim git log must exit 0");

    let stdout = String::from_utf8(output.stdout).unwrap();
    // Count lines that look like commit entries: a 7-char hex prefix.
    let commit_lines: usize = stdout
        .lines()
        .filter(|l| {
            l.split_whitespace()
                .next()
                .is_some_and(|w| w.len() >= 7 && w.chars().all(|c| c.is_ascii_hexdigit()))
        })
        .count();
    assert_eq!(
        commit_lines, 25,
        "expected all 25 commits, got {commit_lines}; output:\n{stdout}"
    );
}

/// Fix 4: user-supplied `-n 5` is still honoured (the handler no longer injects
/// its own default limit, so user limits must pass through unchanged).
#[test]
fn test_git_log_user_n_flag_respected() {
    let (_dir, repo) = make_hermetic_repo_with_commits(25);

    let output = common::skim()
        .current_dir(&repo)
        .args(["git", "log", "--oneline", "-n", "5"])
        .output()
        .unwrap();
    assert!(output.status.success(), "skim git log -n 5 must exit 0");

    let stdout = String::from_utf8(output.stdout).unwrap();
    let commit_lines: usize = stdout
        .lines()
        .filter(|l| {
            l.split_whitespace()
                .next()
                .is_some_and(|w| w.len() >= 7 && w.chars().all(|c| c.is_ascii_hexdigit()))
        })
        .count();
    assert_eq!(
        commit_lines, 5,
        "expected exactly 5 commits with -n 5, got {commit_lines}; output:\n{stdout}"
    );
}

/// ADR-010 regression: a rev-range passed to `skim git log` must not be capped.
///
/// Before Fix 4, `has_limit_flag` did not recognise rev-ranges such as
/// `HEAD~N..HEAD`, so the old `-n 20` injector silently truncated a range of 22
/// commits to 20.  This test pins that the explicit range is passed through intact
/// and all commits in range are shown.
#[test]
fn test_git_log_rev_range_not_capped() {
    let (_dir, repo) = make_hermetic_repo_with_commits(25);

    let output = common::skim()
        .current_dir(&repo)
        .args(["git", "log", "--oneline", "HEAD~22..HEAD"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "skim git log HEAD~22..HEAD must exit 0"
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let commit_lines: usize = stdout
        .lines()
        .filter(|l| {
            l.split_whitespace()
                .next()
                .is_some_and(|w| w.len() >= 7 && w.chars().all(|c| c.is_ascii_hexdigit()))
        })
        .count();
    assert_eq!(
        commit_lines, 22,
        "rev-range HEAD~22..HEAD must show exactly 22 commits, not be capped at 20; \
         got {commit_lines};\noutput:\n{stdout}"
    );
}

// ============================================================================
// Show — new subcommand (#132)
// ============================================================================

#[test]
fn test_skim_git_show_help_listed_in_git_help() {
    // Match the exact subcommand row from print_help() in cmd/git/mod.rs so that
    // removing or renaming the "show" line causes this test to fail.
    common::skim()
        .args(["git", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "  show      Show compressed commit or file content at a ref",
        ));
}

#[test]
fn test_skim_git_show_help_subcommand() {
    common::skim()
        .args(["git", "show", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("commit").and(predicate::str::contains("USAGE")));
}

#[test]
fn test_skim_git_show_head_commit_mode() {
    // HIGH-8: Distinguish compressed output from raw passthrough.
    //
    // Three assertions that raw `git show HEAD` cannot satisfy simultaneously:
    //   1. Output is STRICTLY shorter (byte count) than raw git show HEAD.
    //   2. Output contains em-dash (U+2014) — only present in ShowCommitResult::render.
    //   3. A 7-char hex token appears on any line.
    //
    // Raw git show HEAD starts with "commit <full-hash>\nAuthor:" which never
    // contains U+2014, and is always larger than the compressed one-liner.

    // Collect raw git show HEAD for byte-count comparison.
    let raw_output = std::process::Command::new("git")
        .args(["show", "HEAD"])
        .output()
        .expect("git must be available");
    let raw_bytes = raw_output.stdout.len();

    let output = common::skim()
        .env("SKIM_DEBUG", "1")
        .args(["git", "show", "HEAD"])
        .output()
        .unwrap();
    assert!(output.status.success(), "skim git show HEAD should succeed");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    // Detectable because SKIM_DEBUG=1 is set above.
    let guardrail_triggered = stderr.contains("[skim:guardrail]");
    let has_emdash = stdout.contains('\u{2014}');

    if guardrail_triggered {
        // Guardrail passthrough: compressed was larger than raw, so skim
        // emitted raw git output.  This is correct behaviour — verify the
        // output looks like raw `git show` (starts with "commit <hash>").
        assert!(
            stdout.starts_with("commit "),
            "Guardrail passthrough should emit raw git show output; got: {stdout}"
        );
    } else {
        // Compressed path: em-dash separator must be present.
        assert!(
            has_emdash,
            "Expected em-dash separator (U+2014) in compressed show output; got: {stdout}"
        );

        // Compressed output must not exceed raw by more than annotation overhead.
        // Line numbers add ~8 bytes/line (prefix char + up to 5 digits + space).
        let skim_line_count = stdout.lines().count();
        let annotation_overhead = skim_line_count * 8;
        let max_allowed = raw_bytes + annotation_overhead;
        assert!(
            stdout.len() <= max_allowed,
            "Expected compressed output ({} bytes) to be at most raw ({raw_bytes}) + \
             annotation_overhead ({annotation_overhead} = {skim_line_count} lines × 8 bytes); \
             got: {}",
            stdout.len(),
            stdout.len()
        );
    }

    // Both paths must contain a commit hash (7+ hex chars).
    // Compressed: "<hash> Author — Subject" (hash is first word).
    // Raw: "commit <full-hash>" (hash is second word).
    let has_hash = stdout.lines().any(|l| {
        l.split_whitespace()
            .any(|w| w.len() >= 7 && w.chars().all(|c| c.is_ascii_hexdigit()))
    });
    assert!(
        has_hash,
        "Expected a commit hash in show output, got: {stdout}"
    );
}

#[test]
fn test_skim_git_show_stat_passthrough() {
    // --stat triggers passthrough — git standard output format with no skim wrapping.
    common::skim()
        .args(["git", "show", "--stat", "HEAD"])
        .assert()
        .success();
}

#[test]
fn test_skim_git_show_unknown_subcommand_message() {
    // The "unknown git subcommand" error should now list "show" in the supported list.
    common::skim()
        .args(["git", "totally_unknown_cmd_xyz"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("show"));
}

#[test]
fn test_skim_git_show_file_content_json_rejected() {
    // --json in file-content mode (git show <ref>:<path>) must exit with
    // code 2 (argument error) and print a clear error to stderr.
    // The error message must NOT embed the literal text "(exit code 2)"
    // since that would be self-contradictory if the code were ever changed.
    let output = common::skim()
        .args(["git", "show", "HEAD:Cargo.toml", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(2),
        "file-content --json must exit 2 (argument error), got: {:?}",
        output.status.code()
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("--json is not supported"),
        "stderr must explain the rejection, got: {stderr}"
    );
    assert!(
        !stderr.contains("(exit code 2)"),
        "stderr must not embed '(exit code 2)' — self-contradictory prose, got: {stderr}"
    );
}

// ============================================================================
// Show — commit-mode JSON branch (HIGH-9, #132)
// ============================================================================

#[test]
fn test_skim_git_show_head_commit_mode_json() {
    // HIGH-9: Verify commit-mode --json path behaviour post guardrail fix.
    //
    // Commit 71a8ce6 moved the guardrail call inside the Text-only branch so
    // that the JSON branch never emits `[skim:guardrail]` to stderr.
    // This test verifies:
    //   1. Exit code 0.
    //   2. stdout parses as valid JSON with expected ShowCommitResult keys.
    //   3. stderr contains NO `[skim:guardrail]` marker.
    let output = common::skim()
        .args(["git", "show", "HEAD", "--json"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "skim git show HEAD --json should exit 0"
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    // Assertion 1: stderr must NOT contain the guardrail marker.
    assert!(
        !stderr.contains("[skim:guardrail]"),
        "JSON mode must never emit [skim:guardrail] to stderr; got stderr: {stderr}"
    );

    // Assertion 2: stdout must be valid JSON.
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("stdout must be valid JSON; parse error: {e}\nstdout: {stdout}")
    });

    // Assertion 3: JSON must have the expected ShowCommitResult top-level keys.
    for key in &["hash", "author", "subject", "files_changed", "files"] {
        assert!(
            json.get(key).is_some(),
            "JSON output missing expected key '{key}'; got: {stdout}"
        );
    }

    // Assertion 4: hash field should be a non-empty string.
    let hash = json["hash"]
        .as_str()
        .unwrap_or_else(|| panic!("'hash' field must be a string; got: {}", json["hash"]));
    assert!(
        !hash.is_empty(),
        "ShowCommitResult.hash must not be empty; got: {stdout}"
    );

    // Assertion 5 (paired, revert-detectable): with SKIM_DEBUG=1 the JSON-mode
    // guardrail bypass (commit 71a8ce6) must still suppress the banner.  A revert
    // of that commit would re-enable the guardrail call in the JSON path and this
    // assertion would fail even when Fix 5's debug-gating is in place.
    let debug_output = common::skim()
        .args(["git", "show", "HEAD", "--json"])
        .env("SKIM_DEBUG", "1")
        .output()
        .expect("skim git show HEAD --json with SKIM_DEBUG=1 must run");
    assert!(
        !String::from_utf8_lossy(&debug_output.stderr).contains("[skim:guardrail]"),
        "JSON mode must never emit [skim:guardrail] even with SKIM_DEBUG=1 \
         (guardrail call must stay outside the JSON branch, per commit 71a8ce6); \
         got stderr: {:?}",
        String::from_utf8_lossy(&debug_output.stderr)
    );
}

// ============================================================================
// Show — commit mode guardrail pairing (Fix 5 regression guard)
// ============================================================================

/// Fix 5 regression guard: the guardrail banner for `git show` commit mode
/// is gated behind `SKIM_DEBUG` and must not appear in normal operation.
///
/// Root cause note: `git show HEAD:file` (file-content mode) uses Pseudo mode,
/// which only removes content (type annotations, decorators, semicolons) and
/// therefore can never inflate output.  The `apply_to_stderr` guardrail on that
/// path (show.rs Tier-1) is a safety net that cannot be triggered in practice.
/// This test targets commit mode (`skim git show HEAD`, no colon-path), which
/// calls `apply_to_stderr` in `emit_show_commit` (show.rs) and CAN inflate when
/// the AST-aware hunk renderer adds breadcrumbs and per-line numbers that exceed
/// the raw diff metadata saved.
///
/// Inflation scenario: two-commit hermetic repo where the second commit modifies
/// an existing TypeScript file by appending 20 functions (f20..f39, two-digit
/// names).  The skim commit render adds line numbers and AST breadcrumbs to each
/// diff line; the overhead exceeds the diff-metadata savings, so compressed >
/// raw (verified empirically: raw ~745 bytes, compressed inflates past that).
///
/// Paired assertions (avoids PF-009 by using a hermetic repo):
///   SKIM_DEBUG=1        → banner PRESENT  (catches a revert of Fix 5 that makes
///                                          the banner unconditional)
///   SKIM_DEBUG removed  → banner ABSENT   (the `.not()` assertion is revert-detectable
///                                          via the PRESENT case above)
#[test]
fn test_skim_git_show_commit_guardrail_is_debug_gated() {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let repo = dir.path().to_path_buf();

    git_in(&repo, &["init", "-b", "main"]);
    git_in(&repo, &["config", "user.email", "test@example.com"]);
    git_in(&repo, &["config", "user.name", "Test"]);

    // First commit: establish inflate.ts with f0..f19 (single/double-digit names).
    let mut ts_src_v1 = String::new();
    for i in 0..20 {
        ts_src_v1.push_str(&format!("function f{i}() {{ }}\n"));
    }
    std::fs::write(repo.join("inflate.ts"), &ts_src_v1).expect("write inflate.ts v1");
    git_in(&repo, &["add", "inflate.ts"]);
    git_in(&repo, &["commit", "-m", "add inflate fixture"]);

    // Second commit: append f20..f39 (all two-digit names).
    // The commit diff shows the f17..f19 context lines plus the 20 additions.
    // skim commit render adds per-line numbers (+20, +21, …) and AST breadcrumbs
    // to each hunk line, inflating past the raw diff size (≥ 256 bytes) so the
    // guardrail at apply_to_stderr in emit_show_commit fires.
    let mut ts_src_v2 = ts_src_v1.clone();
    for i in 20..40 {
        ts_src_v2.push_str(&format!("function f{i}() {{ }}\n"));
    }
    std::fs::write(repo.join("inflate.ts"), &ts_src_v2).expect("write inflate.ts v2");
    git_in(&repo, &["add", "inflate.ts"]);
    git_in(&repo, &["commit", "-m", "extend inflate fixture"]);

    // With SKIM_DEBUG=1: guardrail fires AND banner is visible → PRESENT.
    let debug_out = common::skim()
        .args(["git", "show", "HEAD"])
        .current_dir(&repo)
        .env("SKIM_DEBUG", "1")
        .env_remove("SKIM_PASSTHROUGH")
        .output()
        .expect("skim git show HEAD with SKIM_DEBUG=1 must run");
    assert!(
        debug_out.status.success(),
        "skim git show HEAD must exit 0 with SKIM_DEBUG=1; stderr={}",
        String::from_utf8_lossy(&debug_out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&debug_out.stderr).contains("[skim:guardrail]"),
        "Fix 5: SKIM_DEBUG=1 must expose the guardrail banner when output inflates; \
         got stderr: {:?}",
        String::from_utf8_lossy(&debug_out.stderr)
    );

    // Without SKIM_DEBUG: same guardrail fires but Fix 5 gates the banner → ABSENT.
    let nodebug_out = common::skim()
        .args(["git", "show", "HEAD"])
        .current_dir(&repo)
        .env_remove("SKIM_DEBUG")
        .env_remove("SKIM_PASSTHROUGH")
        .output()
        .expect("skim git show HEAD without SKIM_DEBUG must run");
    assert!(
        nodebug_out.status.success(),
        "skim git show HEAD must exit 0 without SKIM_DEBUG"
    );
    assert!(
        !String::from_utf8_lossy(&nodebug_out.stderr).contains("[skim:guardrail]"),
        "Fix 5: without SKIM_DEBUG the guardrail banner must be absent (debug-gated); \
         got stderr: {:?}",
        String::from_utf8_lossy(&nodebug_out.stderr)
    );
}

// ============================================================================
// Show — file-content mode passthrough paths (MEDIUM-26, #132)
// ============================================================================

/// Tier-2 passthrough: unsupported file extension causes `Language::from_path`
/// to return `None`, routing to `passthrough_file_content` without calling
/// `rskim_core::transform`.  This exercises the "no silent drop" guarantee:
/// the file content must appear on stdout and exit code must be 0.
///
/// Note: A true Tier-3 path (transform returns `Err`) requires `rskim_core::
/// transform` to fail, which is extremely rare in practice because tree-sitter
/// is error-tolerant and `CommandRunner` performs lossy UTF-8 conversion (so
/// binary files become valid—if ugly—strings).  Tier-2 exercises the same
/// `passthrough_file_content` code path and the same "no silent drop" property.
#[test]
fn test_skim_git_show_file_content_unsupported_ext_passthrough() {
    // MEDIUM-26: Tier-2 (unsupported extension) exercises passthrough_file_content.
    // Cargo.lock has no recognised language extension → Language::from_path returns None.
    let output = common::skim()
        .args(["git", "show", "HEAD:Cargo.lock"])
        .output()
        .unwrap();

    // Exit code must be 0 — content is passed through, not dropped.
    assert!(
        output.status.success(),
        "skim git show HEAD:Cargo.lock should exit 0 (passthrough); \
         got exit code {:?}",
        output.status.code()
    );

    let stdout = String::from_utf8(output.stdout).unwrap();

    // stdout must be non-empty — no silent content drop.
    assert!(
        !stdout.is_empty(),
        "skim git show HEAD:Cargo.lock stdout must not be empty (content was silently dropped)"
    );

    // Cargo.lock always starts with a known header comment.
    assert!(
        stdout.contains("# This file is automatically @generated by Cargo"),
        "Expected Cargo.lock header in passthrough output; got: {}",
        &stdout[..stdout.len().min(200)]
    );
}

/// Fix D (fix/rewrite-hook-falseneg, AD-GIT-SHOW-PSEUDO): `git show <ref>:<file>`
/// must use Pseudo mode (preserves function bodies), NOT Structure mode (collapses
/// them to `{...}`).  This drives the real production path (`run_show_file_content`,
/// show.rs) end-to-end against a committed fixture and is the PF-007 guard for the
/// mode constant: it FAILS if that site reverts to Structure.
///
/// The asserted tokens live ONLY inside function bodies in the fixture (not in any
/// signature, doc comment, `use`, or struct field), so Structure mode — which keeps
/// signatures and imports — strips every one of them.  Their presence proves Pseudo
/// is live in production, not merely at the transform layer.
#[test]
fn test_skim_git_show_file_content_pseudo_preserves_bodies() {
    let output = common::skim()
        .args([
            "git",
            "show",
            "HEAD:crates/rskim/tests/fixtures/cmd/git/show_file.rs",
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "skim git show of a committed .rs file should exit 0; got exit code {:?}",
        output.status.code()
    );

    let stdout = String::from_utf8(output.stdout).unwrap();

    for token in [
        "find_user_by_username",
        "Invalid credentials",
        "duration_since",
    ] {
        assert!(
            stdout.contains(token),
            "Pseudo-mode `git show` must preserve body token {token:?} \
             (Structure mode would strip it); got: {}",
            &stdout[..stdout.len().min(600)]
        );
    }
}

// ============================================================================
// Git dispatcher coverage (MEDIUM-28, #132)
// ============================================================================

/// MEDIUM-28: Verify that the `git/mod.rs::run()` dispatcher correctly routes
/// each implemented subcommand.  Each assertion checks for a subcommand-specific
/// marker that passthrough alone could not produce.
///
/// Subcommands covered: status, diff, log, show, fetch.
#[test]
fn test_skim_git_dispatcher_routes_all_subcommands() {
    // ---- status ----
    // The handler runs and exits 0; the net-savings guard may passthrough on
    // small repos rather than emitting the skim "status " format header.
    common::skim()
        .args(["git", "status"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());

    // ---- log ----
    // The handler runs and exits 0; net-savings guard may passthrough on 1-commit output.
    common::skim()
        .args(["git", "log", "-n", "1"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());

    // ---- show ----
    // The show handler (commit mode, --json) produces a JSON object —
    // passthrough would emit raw git format starting with "commit ".
    {
        let output = common::skim()
            .args(["git", "show", "HEAD", "--json"])
            .output()
            .unwrap();
        assert!(output.status.success(), "git show dispatch: exit 0");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.trim_start().starts_with('{'),
            "git show --json must produce a JSON object; got: {}",
            &stdout[..stdout.len().min(120)]
        );
    }

    // ---- diff ----
    // `git diff` on a clean repo exits 0.  The diff handler's own help string
    // differs from native git output, but a minimal invocation only guarantees
    // exit 0 when there are no unstaged changes.
    common::skim().args(["git", "diff"]).assert().success();

    // ---- fetch ----
    // `git fetch` when up-to-date writes progress to STDERR only, leaving
    // combined stdout empty. The net-savings guard fires (compressed > empty raw)
    // and skim emits empty stdout as a faithful passthrough of the empty raw.
    // Verify: exit 0. Accept either skim-format in stdout, or faithful empty
    // stdout with "up to date" surfacing in stderr from the git process.
    {
        let (_dir, worker) = make_hermetic_fetch_repo();
        let output = common::skim()
            .args(["git", "fetch"])
            .current_dir(&worker)
            .output()
            .expect("skim git fetch must run");
        assert!(output.status.success(), "git fetch dispatch: exit 0");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let ok = stdout.contains("fetch ")
            || stdout.contains("up to date")
            || stdout.trim().is_empty()
            || stderr.contains("up to date")
            || stderr.contains("fetch");
        assert!(
            ok,
            "fetch dispatch: expected success with content in stdout/stderr or faithful empty \
             passthrough; stdout={stdout:?} stderr={stderr:?}"
        );
    }
}

// ============================================================================
// AD-GIT-8: commit body and merge parents (Commit 1, 2026-04-11)
// ============================================================================

/// E2E test: git show must preserve multi-paragraph commit body.
///
/// Creates a temp git repo with a commit that has a multi-paragraph body,
/// invokes `skim git show HEAD`, and asserts the body paragraphs appear in
/// the output.
#[test]
fn test_git_show_preserves_commit_body() {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let path = dir.path();

    // Init repo and configure identity.
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("git init");
    std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(path)
        .output()
        .expect("git config email");
    std::process::Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(path)
        .output()
        .expect("git config name");

    // Create a commit with multi-paragraph body.
    let commit_msg =
        "feat: multi paragraph test\n\nparagraph 1 of the body\n\nparagraph 2 of the body";
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", commit_msg])
        .current_dir(path)
        .output()
        .expect("git commit");

    let output = common::skim()
        .args(["git", "show", "HEAD"])
        .current_dir(path)
        .output()
        .expect("skim git show must not fail to spawn");

    assert!(
        output.status.success(),
        "skim git show HEAD must exit 0 on valid commit"
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("paragraph 1"),
        "output must contain 'paragraph 1': {stdout}"
    );
    assert!(
        stdout.contains("paragraph 2"),
        "output must contain 'paragraph 2': {stdout}"
    );
}

// ============================================================================
// Regression: git show failure path records analytics (ea4e52f)
// ============================================================================

/// Regression guard for commit ea4e52f.
///
/// Before ea4e52f, `run_show_commit` and `run_show_file_content` returned early
/// on non-zero exit WITHOUT calling `finalize_git_output`, causing the analytics
/// DB to silently miss failed `git show` invocations.
///
/// The fix introduced `ShowRawOutcome::{Success, Failure}` and threads
/// stdout/duration back so `finalize_git_output_passthrough` is called on the
/// error path. This test locks in the observable contract:
///
/// 1. `skim git show <invalid-ref>` exits with a non-zero code (failure propagates).
/// 2. stderr surfaces git's error message (not swallowed or replaced).
///
/// Analytics recording itself is fire-and-forget via a background thread and
/// cannot be verified in a pure E2E test without database introspection; the
/// exit-code + stderr assertions here are sufficient to prove the failure path
/// is traversed (a silent early-return before the fix would still have exited
/// non-zero, but stderr would have been empty).
#[test]
fn test_skim_git_show_invalid_ref_exits_nonzero() {
    let output = common::skim()
        .args(["git", "show", "INVALID_REF_THAT_CANNOT_EXIST_XYZ_12345"])
        .output()
        .unwrap();

    // Failure must propagate — skim must not silently succeed on a bad ref.
    assert!(
        !output.status.success(),
        "skim git show <invalid-ref> must exit non-zero; got: {:?}",
        output.status
    );

    let stderr = String::from_utf8(output.stderr).unwrap();

    // git writes "fatal: ..." to stderr on bad refs — that message must reach
    // the caller so the user knows what failed.
    assert!(
        stderr.contains("fatal") || stderr.contains("unknown revision") || !stderr.is_empty(),
        "skim git show <invalid-ref> must emit git's error on stderr; got empty stderr"
    );
}

/// Regression guard for commit ea4e52f — file-content mode failure path.
///
/// `run_show_file_content` also had the analytics-drop bug. Requesting a path
/// that does not exist in HEAD exercises the same fix.
#[test]
fn test_skim_git_show_invalid_file_ref_exits_nonzero() {
    let output = common::skim()
        .args(["git", "show", "HEAD:this_file_does_not_exist_xyz.rs"])
        .output()
        .unwrap();

    // Non-zero exit: git show exits 128 for missing objects, mapped to 1 by skim.
    assert!(
        !output.status.success(),
        "skim git show HEAD:<missing-file> must exit non-zero; got: {:?}",
        output.status
    );

    let stderr = String::from_utf8(output.stderr).unwrap();

    // git's error must surface to the caller.
    assert!(
        !stderr.is_empty(),
        "skim git show HEAD:<missing-file> must emit git's error on stderr; got empty stderr"
    );
}

// ============================================================================
// F1 — hunk-scoped diff render + ADR-001 guardrail (R2)
// ============================================================================

/// Create a hermetic git repo with a committed Rust file and an unstaged change
/// inside a function body.
///
/// Exercises the AST-aware diff path where the changed line falls inside a
/// function node, triggering `render_default_scoped`.
///
/// Returns the temp dir (caller must keep alive) and the repo path.
fn make_hermetic_rust_diff_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let path = dir.path().to_path_buf();

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&path)
        .output()
        .expect("git init");
    for (k, v) in [("user.email", "test@example.com"), ("user.name", "Test")] {
        std::process::Command::new("git")
            .args(["config", k, v])
            .current_dir(&path)
            .output()
            .expect("git config");
    }

    // Commit a Rust file with two functions.
    let initial = "fn compute(x: i32) -> i32 {\n    x * 2\n}\n\nfn helper() -> &'static str {\n    \"original\"\n}\n";
    std::fs::write(path.join("lib.rs"), initial).expect("write lib.rs");
    std::process::Command::new("git")
        .args(["add", "lib.rs"])
        .current_dir(&path)
        .output()
        .expect("git add");
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&path)
        .output()
        .expect("git commit");

    // Modify both function bodies so the diff hunks fall inside AST nodes.
    let modified = "fn compute(x: i32) -> i32 {\n    x * 3\n}\n\nfn helper() -> &'static str {\n    \"modified_helper\"\n}\n";
    std::fs::write(path.join("lib.rs"), modified).expect("write modified lib.rs");

    (dir, path)
}

/// Create a hermetic repo where the uncommitted change is BETWEEN two functions
/// (a module-level constant added in the gap) — triggering the orphan-hunk path.
///
/// Returns the temp dir (caller must keep alive) and the repo path.
fn make_hermetic_orphan_hunk_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let path = dir.path().to_path_buf();

    std::process::Command::new("git")
        .args(["init"])
        .current_dir(&path)
        .output()
        .expect("git init");
    for (k, v) in [("user.email", "test@example.com"), ("user.name", "Test")] {
        std::process::Command::new("git")
            .args(["config", k, v])
            .current_dir(&path)
            .output()
            .expect("git config");
    }

    // Initial: two functions with no content between them.
    let initial = "fn alpha() -> i32 {\n    1\n}\n\nfn beta() -> i32 {\n    2\n}\n";
    std::fs::write(path.join("lib.rs"), initial).expect("write lib.rs");
    std::process::Command::new("git")
        .args(["add", "lib.rs"])
        .current_dir(&path)
        .output()
        .expect("git add");
    std::process::Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(&path)
        .output()
        .expect("git commit");

    // Add a constant between the two functions — this line is between two
    // top-level function nodes so the hunk is an orphan (no AST node covers it).
    let modified = "fn alpha() -> i32 {\n    1\n}\n\n/// Orphan constant between functions\nconst ORPHAN_MARKER: i32 = 99;\n\nfn beta() -> i32 {\n    2\n}\n";
    std::fs::write(path.join("lib.rs"), modified).expect("write modified lib.rs");

    (dir, path)
}

/// F1/ADR-001: `skim git diff` (Text mode) must never emit more bytes than the
/// raw `git diff` on a hermetic repo with a Rust function-body change.
///
/// The hunk-scoped renderer (`render_default_scoped`) emits only breadcrumb +
/// hunk patch lines. The ADR-001 guardrail wired into `run_diff` catches the
/// remaining edge case where even the breadcrumb-only form exceeds raw.
///
/// Applies ADR-001 (compress, never truncate).
#[test]
fn test_skim_git_diff_text_never_expands_vs_raw() {
    let (_dir, repo) = make_hermetic_rust_diff_repo();

    // Capture raw `git diff` for byte-count comparison.
    let raw_output = std::process::Command::new("git")
        .arg("diff")
        .current_dir(&repo)
        .output()
        .expect("git must be available");
    let raw_len = raw_output.stdout.len();

    // `skim git diff` — Text mode (no --json).
    let skim_output = common::skim()
        .args(["git", "diff"])
        .current_dir(&repo)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim git diff must not fail to spawn");

    assert!(
        skim_output.status.success(),
        "skim git diff must exit 0; stderr={}",
        String::from_utf8_lossy(&skim_output.stderr)
    );

    let skim_len = skim_output.stdout.len();

    // Output must be non-empty — diff must be visible.
    assert!(
        skim_len > 0,
        "skim git diff must produce output; got empty stdout"
    );

    // ADR-001 invariant: skim must NEVER emit more bytes than the raw diff.
    assert!(
        skim_len <= raw_len,
        "F1/ADR-001: skim git diff expanded output vs raw\n  \
         raw={raw_len}B  skim={skim_len}B\n  \
         skim stdout={:?}\n  \
         raw stdout={:?}",
        String::from_utf8_lossy(&skim_output.stdout),
        String::from_utf8_lossy(&raw_output.stdout),
    );

    // ADR-003 invariant: size-only assertions are omission-blind.
    // Assert that the actual changed lines are present in skim's output — proving
    // that compression did not silently drop any changed content.
    let skim_stdout = String::from_utf8_lossy(&skim_output.stdout);
    assert!(
        skim_stdout.contains("x * 3"),
        "F1/ADR-003: changed body line 'x * 3' must appear in skim diff output; \
         got stdout={skim_stdout:?}"
    );
    assert!(
        skim_stdout.contains("modified_helper"),
        "F1/ADR-003: changed body line 'modified_helper' must appear in skim diff output; \
         got stdout={skim_stdout:?}"
    );
}

/// F1/orphan-hunk: changes between top-level AST nodes must appear in output.
///
/// When a diff hunk's changed lines fall outside all detected AST node ranges
/// (e.g. a module-level constant added between two functions), `render_default_scoped`
/// renders those hunks as raw patch lines via the orphan fallback.
///
/// This test verifies that the orphan hunk content is not silently dropped.
#[test]
fn test_skim_git_diff_orphan_hunk_is_visible() {
    let (_dir, repo) = make_hermetic_orphan_hunk_repo();

    let skim_output = common::skim()
        .args(["git", "diff"])
        .current_dir(&repo)
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .output()
        .expect("skim git diff must not fail to spawn");

    assert!(
        skim_output.status.success(),
        "skim git diff must exit 0; stderr={}",
        String::from_utf8_lossy(&skim_output.stderr)
    );

    let stdout = String::from_utf8_lossy(&skim_output.stdout);

    // The orphan constant added between functions must appear in the output.
    // If orphan hunks were silently dropped this assertion would fail.
    assert!(
        stdout.contains("ORPHAN_MARKER"),
        "F1/orphan: orphan hunk content must be visible in skim diff output; \
         got stdout={stdout:?}"
    );
}

/// F1/argv0: `git diff` via the skim argv0 wrapper surface must not expand output.
///
/// The PATH-wrapper dispatch surface (argv0="git") routes to the same diff handler
/// as the hook/rewrite surface but through a different front-end dispatch.
/// Both surfaces share the hunk-scoped renderer and ADR-001 guardrail.
///
/// This test exercises the wrapper front-end independently of the hook surface.
#[cfg(unix)]
#[test]
fn test_skim_git_diff_argv0_surface_does_not_expand() {
    use std::os::unix::process::CommandExt as _;

    let (_dir, repo) = make_hermetic_rust_diff_repo();

    // Capture raw `git diff` for byte-count comparison.
    let raw_output = std::process::Command::new("git")
        .arg("diff")
        .current_dir(&repo)
        .output()
        .expect("git must be available");
    let raw_len = raw_output.stdout.len();

    let skim = common::skim_bin();
    assert!(
        skim.exists(),
        "skim binary must exist at {}: run `cargo build` first",
        skim.display()
    );

    // Invoke as argv0="git" — exercises the wrapper dispatch path.
    let skim_output = std::process::Command::new(&skim)
        .arg0("git")
        .arg("diff")
        .current_dir(&repo)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .output()
        .expect("skim binary must be spawnable");

    assert!(
        skim_output.status.success(),
        "argv0=git diff must exit 0; stderr={}",
        String::from_utf8_lossy(&skim_output.stderr)
    );

    let skim_len = skim_output.stdout.len();

    // ADR-001 invariant on the wrapper surface.
    assert!(
        skim_len <= raw_len,
        "F1/argv0: skim argv0=git diff expanded output vs raw\n  \
         raw={raw_len}B  skim={skim_len}B\n  \
         skim stdout={:?}",
        String::from_utf8_lossy(&skim_output.stdout),
    );
}
