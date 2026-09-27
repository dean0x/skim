//! Test support for the scoreboard: a tempdir git repository to use as a
//! corpus (MOCK: a stand-in for a pinned clone, so tests never touch the
//! network).
//!
//! Compiled only under `#[cfg(test)]` or the `test-utils` feature, so unit
//! tests here and the offline integration tests in `tests/` (#203 AC 3) share
//! one fixture builder.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test support: fail loudly

use std::path::Path;
use std::process::Command;

/// A throwaway git repository plus an isolated `HOME` for every git command
/// run against it, so the developer's global config (signing, hooks,
/// templates, global excludes) never leaks into a fixture.
pub struct FixtureRepo {
    root: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl FixtureRepo {
    /// `git init` a fresh repository on branch `main`.
    ///
    /// # Panics
    ///
    /// Panics if a tempdir cannot be created or `git` is unavailable — a
    /// broken fixture must fail the test, never pass it silently.
    pub fn new() -> Self {
        let repo = FixtureRepo {
            root: tempfile::tempdir().expect("fixture repo tempdir"),
            home: tempfile::tempdir().expect("fixture HOME tempdir"),
        };
        repo.git(&["init", "--quiet"]);
        repo
    }

    /// Repository root.
    pub fn root(&self) -> &Path {
        self.root.path()
    }

    /// The isolated `HOME` the fixture's git commands run under. Pass it to
    /// [`crate::scoreboard::universe::GitIsolation::new`] so the oracle sees
    /// the same (empty) global config.
    pub fn home(&self) -> &Path {
        self.home.path()
    }

    /// Write `contents` to `rel` (creating parent directories).
    pub fn write(&self, rel: &str, contents: impl AsRef<[u8]>) {
        let path = self.root().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    /// Run `git <args>` in the repository and return trimmed stdout.
    ///
    /// # Panics
    ///
    /// Panics when git exits non-zero, echoing its stderr.
    pub fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(self.root())
            .env("HOME", self.home())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("GIT_CONFIG_GLOBAL")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_AUTHOR_NAME", "Fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
            .env("GIT_COMMITTER_NAME", "Fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .output()
            .expect("git must be installed to run scoreboard tests");
        assert!(
            out.status.success(),
            "fixture git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Stage everything (`git add -A`, which honours `.gitignore`) and commit.
    /// Returns the new `HEAD` SHA.
    pub fn commit_all(&self, message: &str) -> String {
        self.git(&["add", "-A"]);
        self.git(&["commit", "--quiet", "--allow-empty", "-m", message]);
        self.head()
    }

    /// Force-add paths that `.gitignore` would otherwise exclude, then commit.
    pub fn commit_forced(&self, paths: &[&str], message: &str) -> String {
        let mut args = vec!["add", "--force", "--"];
        args.extend_from_slice(paths);
        self.git(&args);
        self.git(&["commit", "--quiet", "-m", message]);
        self.head()
    }

    /// Current `HEAD` SHA.
    pub fn head(&self) -> String {
        self.git(&["rev-parse", "HEAD"])
    }
}

impl Default for FixtureRepo {
    fn default() -> Self {
        Self::new()
    }
}
