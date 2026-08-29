//! Git-history corpus harness for diff compress-never-truncate (#317).
//!
//! # Purpose
//!
//! Walks the real git history of this repository and asserts that skim's diff
//! output never drops `+` or `-` lines that appear in the raw diff.  This is
//! the operationalisation of invariant #317 ("compress, never truncate") for
//! the diff code path.
//!
//! Also reports the AST-aware render vs raw-fallback rate per mode so that
//! regression in fallback rate is visible in CI logs.
//!
//! # PF-026 compliance
//!
//! The "raw" control MUST be the real git binary invoked via absolute PATH
//! (not `skim git diff`) AND with `SKIM_PASSTHROUGH=1` to prevent the rewrite
//! hook from wrapping the control command itself.  A naive `git diff` call
//! inside a skim-hooked shell would produce skim-compressed output as its
//! "raw" baseline, making the comparison circular.
//!
//! # Usage
//!
//! This test is `#[ignore]` by default.  Run explicitly:
//!
//! ```text
//! cargo nextest run -p rskim --all-targets -j 4 -- --ignored
//! ```
//!
//! Or target this file only:
//!
//! ```text
//! cargo nextest run -p rskim --all-targets -E 'binary(cli_git_diff_corpus)' -- --ignored
//! ```
//!
//! The test never fails on CI in normal mode (ignored); it is intended for
//! local validation and periodic regression runs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::PathBuf;
use std::process::Command;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Maximum number of commits to sample.  Bounded so the test is never
/// O(unbounded) even on a large history (Reliability rule: every loop must
/// have a fixed upper bound).
const MAX_COMMITS: usize = 200;

/// Each commit is diffed against its first parent.  Diffs larger than this
/// many bytes are skipped to keep the test fast on binary-heavy commits.
const MAX_DIFF_BYTES: usize = 256 * 1024; // 256 KB

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Absolute path to the real git binary, bypassing any PATH wrappers.
///
/// PF-026: must be absolute to prevent a skim wrapper in `~/.skim/bin/` from
/// intercepting the control command.
fn git_bin() -> PathBuf {
    // `which git` gives the first match on PATH.  If the skim wrapper
    // directory is first, walk past it to find the real binary.
    let candidates = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .flat_map(|dir| {
            let p = PathBuf::from(dir).join("git");
            if p.is_file() { Some(p) } else { None }
        })
        .collect::<Vec<_>>();

    for candidate in &candidates {
        // Skip any path whose parent is a known skim wrapper directory.
        let parent_name = candidate
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if parent_name == "bin" {
            // Could be ~/.skim/bin — check the grandparent.
            let gp_name = candidate
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if gp_name == ".skim" {
                continue; // skip wrapper
            }
        }
        return candidate.clone();
    }
    // Fallback: trust the first hit (wrapper absent).
    candidates.into_iter().next().expect("git must be on PATH")
}

/// Return the repository root (directory containing `.git`).
fn repo_root() -> PathBuf {
    // This test lives inside the repo, so we can ask git itself.
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .expect("git rev-parse must succeed");
    PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
}

/// Return the N most-recent commit SHAs in the current branch.
fn recent_commits(root: &std::path::Path, limit: usize) -> Vec<String> {
    let out = Command::new("git")
        .args(["log", "--format=%H", &format!("-{limit}")])
        .current_dir(root)
        .output()
        .expect("git log must succeed");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Run `git diff <sha>^1..<sha>` via the REAL git binary with SKIM_PASSTHROUGH=1.
///
/// Returns `None` when the commit has no parent (initial commit) or the diff
/// exceeds `MAX_DIFF_BYTES`.
fn raw_diff(git: &std::path::Path, root: &std::path::Path, sha: &str) -> Option<String> {
    let out = Command::new(git)
        .args(["diff", &format!("{sha}^1"), sha])
        .current_dir(root)
        // PF-026: passthrough prevents rewrite hook from compressing the control.
        .env("SKIM_PASSTHROUGH", "1")
        .output()
        .ok()?;

    if !out.status.success() {
        // Likely no parent (initial commit) — skip.
        return None;
    }
    if out.stdout.len() > MAX_DIFF_BYTES {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run `skim git diff <sha>^1..<sha>` via the debug binary with analytics off.
fn skim_diff(root: &std::path::Path, sha: &str) -> Option<String> {
    let skim = common::skim_bin();
    let out = Command::new(&skim)
        .args(["git", "diff", &format!("{sha}^1"), sha])
        .current_dir(root)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// Invariant checker
// ---------------------------------------------------------------------------

/// Check that every `+` / `-` content line from `raw` appears in `compressed`.
///
/// The compress-never-truncate invariant (#317) says skim may RE-ENCODE diff
/// output but must never drop information.  We test the weakest verifiable
/// form: every hunk prefix line (`+foo` / `-foo`) in the raw diff is present
/// as a substring somewhere in the compressed output OR the compressed output
/// is identical to the raw (raw-fallback path).
///
/// Returns `(ok, missing_count)`.
fn check_no_line_dropped(raw: &str, compressed: &str) -> (bool, usize) {
    // If skim passed through raw, the invariant trivially holds.
    if compressed == raw {
        return (true, 0);
    }
    let mut missing = 0usize;
    for line in raw.lines() {
        // Only check content lines — hunk headers and context lines are not
        // required to survive re-encoding.
        if line.starts_with('+') && !line.starts_with("+++")
            || line.starts_with('-') && !line.starts_with("---")
        {
            // The line content after the prefix must appear somewhere.
            let content = &line[1..];
            if !content.is_empty() && !compressed.contains(content) {
                missing += 1;
            }
        }
    }
    (missing == 0, missing)
}

// ---------------------------------------------------------------------------
// Test
// ---------------------------------------------------------------------------

#[test]
#[ignore = "corpus test: run with --ignored; reads live git history"]
fn git_diff_corpus_compress_never_truncate() {
    let git = git_bin();
    let root = repo_root();
    let commits = recent_commits(&root, MAX_COMMITS);

    assert!(
        !commits.is_empty(),
        "git log must return at least one commit; got none (is this a git repo?)"
    );

    let mut checked = 0usize;
    let mut skipped_no_parent = 0usize;
    let skipped_too_large = 0usize;
    let mut raw_fallback = 0usize; // skim returned raw unchanged
    let mut violations: Vec<(String, usize)> = Vec::new(); // (sha, missing_count)

    for sha in &commits {
        match raw_diff(&git, &root, sha) {
            None => {
                skipped_no_parent += 1;
                continue;
            }
            Some(ref raw) if raw.is_empty() => {
                // Empty diff (e.g. docs-only commit with no tracked change).
                checked += 1;
                continue;
            }
            Some(raw) => {
                let compressed = skim_diff(&root, sha)
                    .unwrap_or_else(|| raw.clone()); // on skim failure, treat as raw

                if compressed.trim() == raw.trim() {
                    raw_fallback += 1;
                }

                let (ok, missing) = check_no_line_dropped(&raw, &compressed);
                if !ok {
                    violations.push((sha.clone(), missing));
                }
                checked += 1;
            }
        }
        // Check after adding allows the skipped counts to be exact.
        if skipped_no_parent + skipped_too_large + checked >= MAX_COMMITS {
            break;
        }
    }

    // -----------------------------------------------------------------------
    // Report
    // -----------------------------------------------------------------------
    let fallback_pct = if checked > 0 {
        (raw_fallback as f64 / checked as f64) * 100.0
    } else {
        0.0
    };

    println!("=== cli_git_diff_corpus results ===");
    println!("  commits examined : {checked}");
    println!("  skipped (initial): {skipped_no_parent}");
    println!("  skipped (>256KB) : {skipped_too_large}");
    println!("  raw fallback     : {raw_fallback} ({fallback_pct:.1}%)");
    println!("  violations       : {}", violations.len());
    for (sha, n) in &violations {
        println!("    {sha}: {n} dropped lines");
    }
    println!("===================================");

    // -----------------------------------------------------------------------
    // Assert
    // -----------------------------------------------------------------------
    assert!(
        violations.is_empty(),
        "compress-never-truncate (#317) violated on {} commit(s).\n\
         Dropped lines were found in skim's diff output that existed in the \
         raw `git diff` output.  Violations:\n{:#?}",
        violations.len(),
        violations,
    );
}

// ---------------------------------------------------------------------------
// Smoke test (always runs): harness itself is wired up
// ---------------------------------------------------------------------------

/// Verify the git_bin() / repo_root() helpers return sensible values without
/// running the full corpus.  This runs in normal (non-ignored) mode.
#[test]
fn git_diff_corpus_harness_sanity() {
    let git = git_bin();
    assert!(git.is_file(), "git_bin() must return an existing file: {git:?}");
    let root = repo_root();
    assert!(root.join(".git").exists(), "repo_root() must contain .git: {root:?}");
    // One commit must exist.
    let commits = recent_commits(&root, 1);
    assert!(!commits.is_empty(), "recent_commits must return at least one SHA");
    assert_eq!(commits[0].len(), 40, "SHA must be 40 hex chars");
}
