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

#[cfg(unix)]
use crate::scoreboard::runner::{SkimRunner, SkimSandbox};

/// A runner over a POSIX shell script `body` that stands in for the skim
/// binary: written to `<dir>/skim`, run in a sandbox at `<dir>/home`. The
/// script prints a canned answer and exits with the status a test needs.
///
/// # Panics
///
/// Panics when the script cannot be written, does not start, or does not exit
/// within [`START_TIMEOUT_SECS`].
#[cfg(unix)]
pub fn script_runner(dir: &Path, body: &str) -> SkimRunner {
    let bin = write_script(dir, body);
    if let Err(e) = start_until_runnable(&bin, START_TIMEOUT_SECS) {
        panic!("the stand-in skim {} is not usable: {e:#}", bin.display());
    }
    SkimRunner::new(bin, SkimSandbox::new(dir.join("home")))
}

/// Write `body` as the executable POSIX shell script `<dir>/skim`.
#[cfg(unix)]
fn write_script(dir: &Path, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let bin = dir.join("skim");
    std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// How long one start of a stand-in script may run before it is killed. The
/// scripts only print and exit, so this bound is never close to being used.
#[cfg(unix)]
const START_TIMEOUT_SECS: u64 = 10;

/// Start the new script once, and try again while the kernel says it is busy.
///
/// On Linux, `execve(2)` fails with ETXTBSY (os error 26) while a child that
/// another test thread forked still holds the write handle of the new file.
/// That window ends when the child execs. The write handle of the caller is
/// already closed, so after one start succeeds, no later fork can get it, and
/// the test's own start cannot fail this way. The script bodies only print and
/// exit, so the extra start has no side effect.
///
/// Every start runs through the bounded runner, so a script that never exits
/// is killed, with its process group, after `timeout_secs`, and that is an
/// error. Bound: 5 tries, with a sleep of `25 ms × 2^try` after each busy try.
#[cfg(unix)]
fn start_until_runnable(bin: &Path, timeout_secs: u64) -> anyhow::Result<()> {
    use std::process::Stdio;

    const MAX_TRIES: u32 = 5;

    let label = format!("the stand-in skim {}", bin.display());
    for attempt in 0..MAX_TRIES {
        let mut cmd = Command::new(bin);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match rskim_research::clone::git_output_with_timeout(cmd, &label, timeout_secs) {
            Ok(_) => return Ok(()),
            Err(e) if is_busy(&e) => {
                std::thread::sleep(std::time::Duration::from_millis(25 << attempt));
            }
            Err(e) => return Err(e),
        }
    }
    anyhow::bail!("{label} is still busy after {MAX_TRIES} tries")
}

/// Whether `err` carries ETXTBSY (os error 26) anywhere in its cause chain.
#[cfg(unix)]
fn is_busy(err: &anyhow::Error) -> bool {
    const ETXTBSY: i32 = 26;

    err.chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|io| io.raw_os_error() == Some(ETXTBSY))
}

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

#[cfg(all(test, unix))]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn a_stand_in_that_never_exits_is_killed_at_the_start_bound() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_script(dir.path(), "sleep 8");

        let started = Instant::now();
        let err = start_until_runnable(&bin, 1).expect_err("a script that outlasts the bound");

        assert!(format!("{err:#}").contains("timed out after 1s"), "{err:#}");
        // The bound plus the kill grace of the runner, with room to spare, and
        // well short of the 8 s the script asks for.
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_stand_in_that_exits_with_a_failure_status_has_started() {
        let dir = tempfile::tempdir().unwrap();
        let bin = write_script(dir.path(), "exit 3");

        start_until_runnable(&bin, START_TIMEOUT_SECS).expect("a script that runs and exits");
    }

    #[test]
    fn a_stand_in_that_does_not_exist_is_not_retried() {
        let dir = tempfile::tempdir().unwrap();

        let started = Instant::now();
        let err = start_until_runnable(&dir.path().join("absent"), START_TIMEOUT_SECS)
            .expect_err("a missing script");

        assert!(format!("{err:#}").contains("spawning"), "{err:#}");
        // One try: the retry sleeps of a busy file alone add up to 775 ms.
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_busy_file_error_is_recognised_under_the_context_the_runner_adds() {
        let busy = anyhow::Error::new(std::io::Error::from_raw_os_error(26))
            .context("spawning the stand-in skim");
        let missing = anyhow::Error::new(std::io::Error::from_raw_os_error(2))
            .context("spawning the stand-in skim");

        assert!(is_busy(&busy));
        assert!(!is_busy(&missing));
        assert!(!is_busy(&anyhow::anyhow!("timed out")));
    }
}
