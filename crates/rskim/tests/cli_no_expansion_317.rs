//! Behavioral no-expansion integration tests (#317 — reports 7.1 and 7.2).
//!
//! ## #317 Invariant
//!
//! "compress, never truncate — and never expand" (CLAUDE.md / PR #317).
//!
//! skim's net-savings guard (`savings_decision` in `cmd/execution.rs`) ensures
//! that compressed output is NEVER emitted when it is larger (in tokens/bytes)
//! than the raw tool output.  When the guard fires, skim falls back to the raw
//! output verbatim.
//!
//! ## Reported regressions
//!
//! **Report 7.1 (ls)**: `skim ls -la <dir>` expanded output relative to raw
//!   `ls -la <dir>`.  The net-savings guard should have prevented this.
//!
//! **Report 7.2 (wc)**: `skim wc -c` on a tiny/empty input expanded output
//!   relative to raw `wc -c`.
//!
//! ## What these tests assert
//!
//! For each reported command:
//!   - Run skim (the binary under test) and capture stdout bytes.
//!   - Run the underlying tool directly and capture its stdout bytes.
//!   - Assert the two byte strings are EQUAL.
//!
//! Equality, not `skim_len <= raw_len`. Both commands covered here — `ls` and
//! `wc` — route through pure-passthrough handlers (ADR-009: `parse_ls_impl` and
//! the `wc` handler delegate to `passthrough_parse`), so raw is the whole
//! contract in both directions. A `<=` bound admits a handler that silently
//! SHRANK the output, which is the direction #317 actually forbids ("never show
//! less than the raw tool") and the direction with no upper limit on the damage.
//! Where a handler genuinely compresses, an equality assertion would be wrong
//! and the test does not belong in this file.

use std::fs;

use assert_cmd::Command;
mod common;

fn skim_cmd() -> Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

// ============================================================================
// Report 7.1 — `skim ls -la <dir>` must not expand relative to raw `ls -la`
// ============================================================================

/// `skim ls -la <tiny_dir>` stdout must be the exact bytes raw
/// `ls -la <tiny_dir>` serves.
///
/// This converts the unit-level guard logic into proof on the real reported
/// regression (report 7.1).  Expansion means the net-savings guard is not
/// firing on the `ls` command path; a shortfall means `ls` stopped being the
/// byte-faithful passthrough ADR-009 makes it (`parse_ls_impl` delegates to
/// `passthrough_parse`), which is how the pre-ADR-009 parser silently dropped
/// entries and the native `total N` header at scale.
#[test]
#[cfg(unix)]
fn no_expansion_ls_la_tiny_dir() {
    let dir = tempfile::tempdir().unwrap();
    // Populate a tiny directory — a few files so ls has something to format.
    for name in &["alpha.txt", "beta.txt", "gamma.txt"] {
        fs::write(dir.path().join(name), "x").unwrap();
    }
    let dir_path = dir.path().to_str().unwrap();

    // Run skim ls -la <dir>
    let skim_output = skim_cmd()
        .args(["ls", "-la", dir_path])
        .output()
        .expect("skim ls must not fail to spawn");

    // Run raw ls -la <dir>
    let raw_output = std::process::Command::new("ls")
        .args(["-la", dir_path])
        .output()
        .expect("ls must be available on Unix");

    let skim_len = skim_output.stdout.len();
    let raw_len = raw_output.stdout.len();

    // #317 invariant, in its strong form: `ls` is a pure passthrough, so the
    // served bytes must EQUAL raw, in both directions.
    assert_eq!(
        skim_output.stdout,
        raw_output.stdout,
        "report 7.1: skim ls -la diverged from raw ls -la\n  \
         raw={raw_len}B  skim={skim_len}B\n  \
         skim stdout={:?}\n  \
         raw stdout={:?}\n  \
         Expanding means the net-savings guard failed to fire on the ls path; \
         shrinking means the ls handler is no longer byte-faithful (ADR-009).",
        String::from_utf8_lossy(&skim_output.stdout),
        String::from_utf8_lossy(&raw_output.stdout)
    );
}

/// `skim ls <dir>` (without -la) must also serve raw's exact bytes.
///
/// Covers the bare `ls` path alongside the `-la` variant; both reach the same
/// pure-passthrough handler.
#[test]
#[cfg(unix)]
fn no_expansion_ls_plain_tiny_dir() {
    let dir = tempfile::tempdir().unwrap();
    for name in &["one.txt", "two.txt"] {
        fs::write(dir.path().join(name), "x").unwrap();
    }
    let dir_path = dir.path().to_str().unwrap();

    let skim_output = skim_cmd()
        .args(["ls", dir_path])
        .output()
        .expect("skim ls must spawn");

    let raw_output = std::process::Command::new("ls")
        .arg(dir_path)
        .output()
        .expect("ls must be available");

    let skim_len = skim_output.stdout.len();
    let raw_len = raw_output.stdout.len();

    assert_eq!(
        skim_output.stdout,
        raw_output.stdout,
        "report 7.1 (plain ls): skim diverged from raw ls\n  \
         raw={raw_len}B  skim={skim_len}B\n  \
         skim stdout={:?}\n  \
         raw stdout={:?}",
        String::from_utf8_lossy(&skim_output.stdout),
        String::from_utf8_lossy(&raw_output.stdout)
    );
}

// ============================================================================
// Report 7.2 — `skim wc -c` must not expand relative to raw `wc -c`
// ============================================================================

/// `skim wc -c` on a tiny/empty input must serve the exact bytes raw `wc -c`
/// serves.
///
/// This converts the unit-level guard logic into proof on the real reported
/// regression (report 7.2).
///
/// Asserted as EQUALITY, not `skim_len <= raw_len`. A non-expansion bound is
/// satisfied by a handler that silently SHRANK the output, which is the
/// direction #317 actually forbids ("never show less than the raw tool") — and
/// `wc` emits only a count, with no accompanying view a reader could falsify a
/// wrong number against.
///
/// We use a tiny file (`"hello\n"`) passed as an argument so both skim and raw
/// wc process the same input without depending on stdin piping in tests.
#[test]
#[cfg(unix)]
fn no_expansion_wc_c_tiny_input() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("tiny.txt");
    fs::write(&file, "hello\n").unwrap();
    let file_path = file.to_str().unwrap();

    // Run skim wc -c <file>
    let skim_output = skim_cmd()
        .args(["wc", "-c", file_path])
        .output()
        .expect("skim wc must not fail to spawn");

    // Run raw wc -c <file>
    let raw_output = std::process::Command::new("wc")
        .args(["-c", file_path])
        .output()
        .expect("wc must be available on Unix");

    let skim_len = skim_output.stdout.len();
    let raw_len = raw_output.stdout.len();

    // #317 invariant, in its strong form: on the passthrough family the served
    // bytes must EQUAL raw, in both directions.
    assert_eq!(
        skim_output.stdout,
        raw_output.stdout,
        "report 7.2: skim wc -c diverged from raw wc -c\n  \
         raw={raw_len}B  skim={skim_len}B\n  \
         skim stdout={:?}\n  \
         raw stdout={:?}\n  \
         Expanding means the net-savings guard failed to fire on the wc path; \
         shrinking means the wc handler is not byte-faithful.",
        String::from_utf8_lossy(&skim_output.stdout),
        String::from_utf8_lossy(&raw_output.stdout)
    );
}

/// `skim wc -c` on an empty file (report 7.2, edge case).
///
/// `wc -c` on an empty file emits "0 <filename>". skim must serve those exact
/// bytes — the empty-input edge is where a handler is most likely to emit its
/// own "nothing to show" rendering instead of the tool's answer, and a `<=`
/// bound would call that a pass.
#[test]
#[cfg(unix)]
fn no_expansion_wc_c_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("empty.txt");
    fs::write(&file, "").unwrap();
    let file_path = file.to_str().unwrap();

    let skim_output = skim_cmd()
        .args(["wc", "-c", file_path])
        .output()
        .expect("skim wc must spawn");

    let raw_output = std::process::Command::new("wc")
        .args(["-c", file_path])
        .output()
        .expect("wc must be available");

    let skim_len = skim_output.stdout.len();
    let raw_len = raw_output.stdout.len();

    assert_eq!(
        skim_output.stdout,
        raw_output.stdout,
        "report 7.2 (empty file): skim wc -c diverged from raw wc -c\n  \
         raw={raw_len}B  skim={skim_len}B\n  \
         skim stdout={:?}\n  \
         raw stdout={:?}",
        String::from_utf8_lossy(&skim_output.stdout),
        String::from_utf8_lossy(&raw_output.stdout)
    );
}

// ============================================================================
// Extra: wc -l (report 7.2 variant — line count)
// ============================================================================

/// `skim wc -l` on a tiny input must serve the exact bytes raw `wc -l` serves.
///
/// The line-count variant of report 7.2. `wc -l` is the consumer #317's own
/// prose names, and its output is a bare number with no view to falsify it
/// against, so equality is the only assertion that means anything here.
#[test]
#[cfg(unix)]
fn no_expansion_wc_l_tiny_input() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lines.txt");
    fs::write(&file, "line1\nline2\nline3\n").unwrap();
    let file_path = file.to_str().unwrap();

    let skim_output = skim_cmd()
        .args(["wc", "-l", file_path])
        .output()
        .expect("skim wc must spawn");

    let raw_output = std::process::Command::new("wc")
        .args(["-l", file_path])
        .output()
        .expect("wc must be available");

    let skim_len = skim_output.stdout.len();
    let raw_len = raw_output.stdout.len();

    assert_eq!(
        skim_output.stdout,
        raw_output.stdout,
        "report 7.2 (wc -l): skim diverged from raw wc -l\n  \
         raw={raw_len}B  skim={skim_len}B\n  \
         skim stdout={:?}\n  \
         raw stdout={:?}",
        String::from_utf8_lossy(&skim_output.stdout),
        String::from_utf8_lossy(&raw_output.stdout)
    );
}

// ============================================================================
// B1 regression — `skim git diff --raw / --dirstat` must serve real git output
// ============================================================================
//
// Before the fix, `parse_unified_diff` returned zero files for non-unified
// git diff output formats (e.g. `--raw`, `--dirstat`).  The code treated
// "zero parsed files" as "no changes", printing "No changes" to stderr and
// emitting 0 bytes to stdout — total content loss.
//
// The fix: when the unified parser yields no files but the raw output is
// non-empty, serve the raw bytes verbatim (#317 compress-never-truncate).

/// Create a hermetic two-commit repo and return `(tempdir_guard, repo_path)`.
///
/// PF-026: raw git is invoked by absolute path so the skim rewrite hook on
/// the developer's machine cannot interpose on the baseline measurement.
fn two_commit_git_repo() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).expect("create repo dir");

    let git_in = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap_or_else(|e| panic!("git {} spawn failed: {e}", args.join(" ")));
        assert!(
            out.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    };

    git_in(&["init", "-b", "main"]);
    git_in(&["config", "user.email", "test@example.com"]);
    git_in(&["config", "user.name", "Test"]);
    git_in(&["config", "diff.algorithm", "myers"]);
    // Put the file in a subdirectory so `--dirstat` aggregates at the `src/`
    // level and produces non-empty output.  Root-only diffs produce no dirstat
    // output on macOS git 2.50.1 (Apple Git-155) because the root dir `.` has
    // no named-directory contribution to report.  `--raw` still works for
    // root-level files, but using a subdir is compatible with both flags.
    std::fs::create_dir_all(repo.join("src")).expect("create src dir");
    std::fs::write(repo.join("src/file.rs"), "fn before() {}\n").expect("write before");
    git_in(&["add", "src/file.rs"]);
    git_in(&["commit", "-m", "before"]);
    std::fs::write(repo.join("src/file.rs"), "fn after() {}\n").expect("write after");
    git_in(&["add", "src/file.rs"]);
    git_in(&["commit", "-m", "after"]);

    (dir, repo)
}

/// `skim git diff --raw` must serve the same bytes as real `git diff --raw --no-color`.
///
/// The skim wrapper injects `--no-color` before calling git, so the comparison
/// baseline is `git diff --no-color --raw` (PF-026: invoked by absolute path).
/// Before the fix this test would fail with skim emitting 0 bytes.
#[test]
fn git_diff_raw_flag_serves_real_git_output() {
    let (_dir, repo) = two_commit_git_repo();

    // Raw control: invoke git by absolute path so the skim rewrite hook cannot
    // intercept it (PF-026).  Use --no-color to match what skim injects internally.
    let raw_out = std::process::Command::new("/usr/bin/git")
        .args(["diff", "--no-color", "--raw", "HEAD~1..HEAD"])
        .current_dir(&repo)
        .output()
        .expect("git must run");
    assert!(raw_out.status.success(), "git diff --raw must succeed");

    let raw_bytes = raw_out.stdout.len();
    assert!(
        raw_bytes > 0,
        "git diff --raw must produce non-empty output (sanity check)"
    );

    // skim output — must equal the raw bytes, not 0.
    let skim_out = skim_cmd()
        .current_dir(&repo)
        .args(["git", "diff", "--raw", "HEAD~1..HEAD"])
        .output()
        .expect("skim git diff must run");

    let skim_bytes = skim_out.stdout.len();
    assert_eq!(
        skim_bytes,
        raw_bytes,
        "skim git diff --raw: expected {raw_bytes} bytes (same as git), got {skim_bytes}\n\
         skim stdout={:?}\n\
         raw stdout={:?}\n\
         This means the parser yielded zero files and the raw bytes were not served (#317).",
        String::from_utf8_lossy(&skim_out.stdout),
        String::from_utf8_lossy(&raw_out.stdout),
    );
}

/// `skim git diff --dirstat` must serve the same bytes as real `git diff --dirstat --no-color`.
///
/// `--dirstat` format is also non-unified and was silently dropped to 0 bytes
/// before the B1 fix.
#[test]
fn git_diff_dirstat_flag_serves_real_git_output() {
    let (_dir, repo) = two_commit_git_repo();

    let raw_out = std::process::Command::new("/usr/bin/git")
        .args(["diff", "--no-color", "--dirstat", "HEAD~1..HEAD"])
        .current_dir(&repo)
        .output()
        .expect("git must run");
    assert!(raw_out.status.success(), "git diff --dirstat must succeed");

    let raw_bytes = raw_out.stdout.len();
    assert!(
        raw_bytes > 0,
        "git diff --dirstat must produce non-empty output (sanity check)"
    );

    let skim_out = skim_cmd()
        .current_dir(&repo)
        .args(["git", "diff", "--dirstat", "HEAD~1..HEAD"])
        .output()
        .expect("skim git diff must run");

    let skim_bytes = skim_out.stdout.len();
    assert_eq!(
        skim_bytes,
        raw_bytes,
        "skim git diff --dirstat: expected {raw_bytes} bytes, got {skim_bytes}\n\
         skim stdout={:?}\n\
         raw stdout={:?}",
        String::from_utf8_lossy(&skim_out.stdout),
        String::from_utf8_lossy(&raw_out.stdout),
    );
}

/// `skim git diff` with genuinely no changes must emit 0 bytes (correct).
///
/// This verifies that the B1 fix does not break the true-empty case — when git
/// diff legitimately produces empty output (no changes), skim must also emit
/// nothing (not serve an empty string to stdout as a write).
#[test]
fn git_diff_no_changes_emits_nothing() {
    let (_dir, repo) = two_commit_git_repo();

    // Diff the first commit against itself — truly no changes.
    let skim_out = skim_cmd()
        .current_dir(&repo)
        .args(["git", "diff", "HEAD~1..HEAD~1"])
        .output()
        .expect("skim git diff must run");

    assert_eq!(
        skim_out.stdout.len(),
        0,
        "skim git diff with no changes must emit 0 bytes, got {:?}",
        String::from_utf8_lossy(&skim_out.stdout),
    );
}
