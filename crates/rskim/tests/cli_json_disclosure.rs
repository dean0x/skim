//! D1 — `--json` disclosure split (ADR-015 / ADR-011 class 1).
//!
//! Every `--json` exit now declares a `Completeness`.  A `Lossy` declaration
//! owes the reader an unconditional stderr marker naming the tool and the
//! narrowest remedy that is *actually true* for that invocation; a `Reencoded`
//! declaration owes nothing and must stay silent.
//!
//! # RED observation (recorded before the fix, binary at 9058273)
//!
//! ```text
//! $ SKIM_DISABLE_ANALYTICS=1 skim git log -n 3 --json >out 2>err
//! exit=0
//! stdout bytes: 474
//! stderr bytes: 0      <-- 3 commit subjects served in place of the full log,
//!                          with the bodies stripped by the injected --format,
//!                          and NOT ONE BYTE of disclosure.
//! ```
//!
//! The same measurement on `skim git status --json` (883 stdout bytes, 0 stderr)
//! and on `skim psql --json < psql_select.txt` (794 stdout bytes, 0 stderr).
//! Each of the three positive tests below asserts the property that measurement
//! shows to be false.
//!
//! # Negative pins
//!
//! `git diff` / `git show` carry every hunk body in `DiffFileEntry::patch`
//! (D3 / #510) and therefore declare `Reencoded`.  Adding a marker there would
//! be a false disclosure, so their silence is pinned — with and without
//! `SKIM_DEBUG=1`, because a class-1 marker is unconditional in *both*
//! directions: it must not appear on a lossless path even in debug mode.
//!
//! # Surface under test
//!
//! Rewrite-engine surface only (skim binary invoked as a subcommand).  The
//! `--json` envelope handlers are shared with the PATH-wrapper surface, but the
//! dispatch front-end is not; see `cli_both_surfaces_paired.rs`.

use std::fs;
use std::process::Command;
use tempfile::TempDir;
mod common;

/// The substring that identifies a D1 JSON disclosure marker on stderr.
const JSON_MARKER: &str = "[skim] json view";

/// The legacy remedy literal — true wherever `strip_skim_flags` removes the
/// skim-only flags before the passthrough exec (git).
const LEGACY_REMEDY: &str = "SKIM_PASSTHROUGH=1 for full output";

/// Commit pinned for the `Reencoded` negative pins.
///
/// `b79f6e3` MUST be reachable from `main` (`git merge-base --is-ancestor
/// b79f6e3 origin/main`) — branch-only commits disappear after a squash-merge.
/// It is the same commit `cli_git_diff_json_content.rs` and
/// `cli_git_show_json_content.rs` pin.
const TEST_SHA: &str = "b79f6e3";

/// Range form of [`TEST_SHA`] for `git diff`.
const TEST_RANGE: &str = "b79f6e3^..b79f6e3";

// ============================================================================
// Helpers
// ============================================================================

/// Seed a throwaway git repo with one commit and one unstaged modification.
///
/// Raw git is invoked by absolute path (`/usr/bin/git`, PF-026) with
/// `SKIM_PASSTHROUGH` removed, so a live rewrite hook on the developer machine
/// cannot intercept the setup commands.
fn git_repo(dir: &std::path::Path) {
    let git = |args: &[&str]| {
        let ok = Command::new("/usr/bin/git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env_remove("SKIM_PASSTHROUGH")
            .output()
            .expect("/usr/bin/git must be available")
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q", "-b", "main"]);
    fs::write(
        dir.join("src.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&[
        "commit",
        "-qm",
        "seed commit with a reasonably long subject line",
    ]);
    // Leave an unstaged modification so `git status` has something to report.
    fs::write(
        dir.join("src.rs"),
        "fn main() {\n    println!(\"world\");\n}\n",
    )
    .unwrap();
}

/// Assert `stdout` parses as JSON, returning the parsed value.
fn parse_json(stdout: &[u8], what: &str) -> serde_json::Value {
    serde_json::from_slice(stdout).unwrap_or_else(|e| {
        panic!(
            "{what}: stdout must remain valid JSON after the disclosure split \
             (the marker goes to stderr, never stdout); parse error: {e}\nstdout: {}",
            String::from_utf8_lossy(stdout)
        )
    })
}

// ============================================================================
// Lossy declarations — the marker MUST fire
// ============================================================================

/// `skim git log --json` declares `Lossy` and therefore owes a disclosure.
///
/// The envelope cannot be the full log: the handler injects
/// `--format=%h %s (%cr) <%an>` (so every commit body is gone) and `parse_log`
/// keeps only `is_commit_line` matches (so a `git log -p` patch body is
/// filtered out).  Before D1 that loss was silent — see the RED observation in
/// the module docs.
///
/// The remedy stays the legacy literal because `strip_skim_flags("git", …)`
/// removes bare `--json` before the passthrough exec, so
/// `SKIM_PASSTHROUGH=1 skim git log --json` really does re-exec git.
#[test]
fn git_log_json_discloses_on_stderr_and_keeps_stdout_json() {
    let dir = TempDir::new().unwrap();
    git_repo(dir.path());

    let out = common::skim()
        .current_dir(dir.path())
        .args(["git", "log", "-n", "3", "--json"])
        .output()
        .expect("skim git log --json must not fail to spawn");

    assert!(
        out.status.success(),
        "skim git log --json must exit 0; got {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    // stdout is unchanged: still a parseable JSON envelope.
    parse_json(&out.stdout, "git log --json");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[skim] json view of 'git'"),
        "D1 RED: `git log --json` serves a summarised view and must disclose it \
         on stderr (measured pre-fix: 0 stderr bytes); got: {stderr:?}"
    );
    assert!(
        stderr.contains(LEGACY_REMEDY),
        "git strips --json before the passthrough exec, so the marker must carry \
         the legacy remedy; got: {stderr:?}"
    );
}

/// `skim git status --json` declares `Lossy` with no countable unit.
///
/// `parse_status` folds the injected `--porcelain=v2` records into counted
/// groups, so there is no 1:1 line correspondence to report — the marker takes
/// the countless arm ("summarised, not the full tool output") and still names
/// the tool and the remedy.
#[test]
fn git_status_json_discloses_with_the_countless_wording() {
    let dir = TempDir::new().unwrap();
    git_repo(dir.path());

    let out = common::skim()
        .current_dir(dir.path())
        .args(["git", "status", "--json"])
        .output()
        .expect("skim git status --json must not fail to spawn");

    assert!(
        out.status.success(),
        "skim git status --json must exit 0; got {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    parse_json(&out.stdout, "git status --json");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[skim] json view of 'git'"),
        "D1 RED: `git status --json` serves a summary and must disclose it; \
         got: {stderr:?}"
    );
    assert!(
        stderr.contains("summarised, not the full tool output"),
        "status has no 1:1 unit to count, so the countless arm must be used; \
         got: {stderr:?}"
    );
    assert!(
        stderr.contains(LEGACY_REMEDY),
        "git strips --json before the passthrough exec; got: {stderr:?}"
    );
}

/// `skim psql --json` declares `Lossy` **and** takes the narrow remedy arm.
///
/// `--json` is skim-only for `git` alone; `strip_skim_flags("psql", …)` leaves
/// it in place, so `SKIM_PASSTHROUGH=1 skim psql --json` would hand `--json` to
/// the real psql, which rejects it.  Printing the legacy hint there would be a
/// remedy that cannot work — the marker must name the only true one instead.
#[test]
fn psql_json_marker_names_the_only_reachable_remedy() {
    let fixture = include_str!("fixtures/cmd/db/psql_select.txt");

    let out = common::skim()
        .args(["psql", "--json"])
        .write_stdin(fixture)
        .output()
        .expect("skim psql --json must not fail to spawn");

    assert!(
        out.status.success(),
        "skim psql --json must exit 0; got {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    parse_json(&out.stdout, "psql --json");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[skim] json view of 'psql'"),
        "D1 RED: `psql --json` serves a parsed summary and must disclose it \
         (measured pre-fix: 0 stderr bytes); got: {stderr:?}"
    );
    assert!(
        stderr.contains("run 'psql' directly for the full output"),
        "the narrow remedy arm must fire for a tool whose --json is NOT stripped \
         before the passthrough exec; got: {stderr:?}"
    );
    assert!(
        !stderr.contains("SKIM_PASSTHROUGH=1"),
        "printing the legacy hint here would be a false remedy: the hatch would \
         forward --json to the real psql and fail; got: {stderr:?}"
    );
}

// ============================================================================
// Reencoded declarations — the marker MUST NOT fire (negative pins)
// ============================================================================

/// `skim git diff <range> --json` declares `Reencoded`: every hunk body is
/// carried in `files[].patch` (D3 / #510), so no disclosure is owed.
///
/// Checked with **and without** `SKIM_DEBUG=1`.  A class-1 marker is
/// unconditional, which cuts both ways: debug mode must not conjure one on a
/// lossless path.  The assertion targets the marker substring rather than
/// stderr emptiness because `SKIM_DEBUG=1` legitimately prints a provenance
/// line (`[skim] 2.11.0 (<sha>) exe=… pid=…`).
#[test]
fn git_diff_json_stays_silent_reencoded() {
    for debug in [false, true] {
        let mut cmd = common::skim();
        cmd.args(["git", "diff", TEST_RANGE, "--json"]);
        if debug {
            cmd.env("SKIM_DEBUG", "1");
        } else {
            cmd.env_remove("SKIM_DEBUG");
        }
        let out = cmd
            .output()
            .expect("skim git diff --json must not fail to spawn");

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains(JSON_MARKER),
            "git diff --json is Reencoded (patch bodies carried in files[].patch); \
             a disclosure marker there would be a FALSE disclosure. \
             SKIM_DEBUG={debug}; stderr: {stderr:?}"
        );
    }
}

/// `skim git show <sha> --json` declares `Reencoded` for the same reason as
/// `git diff` — header fields plus every hunk body are carried.  Pinned with
/// and without `SKIM_DEBUG=1`.
#[test]
fn git_show_json_stays_silent_reencoded() {
    for debug in [false, true] {
        let mut cmd = common::skim();
        cmd.args(["git", "show", TEST_SHA, "--json"]);
        if debug {
            cmd.env("SKIM_DEBUG", "1");
        } else {
            cmd.env_remove("SKIM_DEBUG");
        }
        let out = cmd
            .output()
            .expect("skim git show --json must not fail to spawn");

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains(JSON_MARKER),
            "git show --json is Reencoded (header + patch bodies carried); \
             a disclosure marker there would be a FALSE disclosure. \
             SKIM_DEBUG={debug}; stderr: {stderr:?}"
        );
    }
}
