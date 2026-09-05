//! Shared git fixture helpers for integration tests.
//!
//! Extracted from `cli_search_blast_weights.rs` and `cli_temporal_first_parent.rs`
//! (verbatim duplicates — #409 finding 5) to eliminate silent drift: a hermeticity
//! fix applied to one copy (e.g. adding `-c core.hooksPath=/dev/null`) will now
//! reach every consumer automatically.
//!
//! Import in each integration-test binary with:
//! ```rust
//! mod common;
//! use common::git_fixture::{now_epoch, git_init, write_and_stage, git_commit};
//! ```
#![allow(dead_code)]

use std::fs;
use std::path::Path;
use std::process::Command as StdCommand;
use std::time::{SystemTime, UNIX_EPOCH};

/// Return the current Unix epoch in seconds.
pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before UNIX epoch")
        .as_secs()
}

/// Initialise a git repository with hermetic, non-signing identity.
pub fn git_init(dir: &Path) {
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
pub fn write_and_stage(dir: &Path, filename: &str, content: &str) {
    write_and_stage_bytes(dir, filename, content.as_bytes());
}

/// Write raw bytes to `filename` (relative to `dir`) and `git add` it.
///
/// Use this sibling of [`write_and_stage`] when the content is non-UTF-8
/// (e.g. a binary fixture that should appear in git history but be skipped
/// by skim's UTF-8 content filter, as in the AC-7 seed-unindexed test).
pub fn write_and_stage_bytes(dir: &Path, filename: &str, content: &[u8]) {
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
pub fn git_commit(dir: &Path, message: &str, ts: u64) {
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
