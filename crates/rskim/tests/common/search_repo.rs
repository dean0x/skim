//! A throwaway git repository for `skim search` integration tests.
//!
//! The repository sits in its own folder next to a sandbox home, so the root
//! walk of `skim search` stops at the repository and never at the home folder.
//! Git runs with no user or system configuration.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One finished `skim search` run.
#[derive(Debug)]
pub struct Run {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    /// The stdout lines.
    pub fn lines(&self) -> Vec<&str> {
        self.stdout.lines().collect()
    }
}

/// A git repository in a temporary folder, with a sandbox home beside it.
pub struct SearchRepo {
    _dir: tempfile::TempDir,
    home: PathBuf,
    root: PathBuf,
}

impl SearchRepo {
    /// Create an empty repository.
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let home = dir.path().join("home");
        let root = dir.path().join("repo");
        fs::create_dir_all(&home).expect("home dir");
        fs::create_dir_all(&root).expect("repo dir");
        let repo = SearchRepo {
            _dir: dir,
            home,
            root,
        };
        repo.git(&["init", "-q", "."]);
        repo
    }

    /// The sandbox home folder.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The repository folder, as the test made it.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The repository folder, canonical: the form that `skim search` prints.
    pub fn canonical_root(&self) -> PathBuf {
        self.root.canonicalize().expect("canonical root")
    }

    /// Write a file below the repository, with its parent folders.
    pub fn write(&self, rel: &str, content: impl AsRef<[u8]>) {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent dir");
        }
        fs::write(path, content).expect("write file");
    }

    /// Run git in the repository and require success.
    pub fn git(&self, args: &[&str]) {
        let status = self.git_command().args(args).status().expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    /// Track every file now in the repository.
    pub fn track_all(&self) {
        self.git(&["add", "-A"]);
    }

    fn git_command(&self) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.root)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        cmd
    }

    /// A sandboxed `skim` command that starts in the repository.
    pub fn skim(&self) -> assert_cmd::Command {
        let mut cmd = super::skim_sandboxed(&self.home);
        cmd.current_dir(&self.root);
        cmd
    }

    /// Run `skim search ARGS` in the repository.
    pub fn search(&self, args: &[&str]) -> Run {
        self.search_in(&self.root, args)
    }

    /// Run `skim search ARGS` with `dir` as the working folder.
    pub fn search_in(&self, dir: &Path, args: &[&str]) -> Run {
        let mut cmd = self.skim();
        cmd.current_dir(dir).arg("search").args(args);
        finish(&mut cmd)
    }

    /// The `skim search ARGS` command as a plain `std` command, with the same
    /// sandbox. A test uses it when it must spawn the child and hold its pipes.
    pub fn std_search_command(&self, args: &[&str]) -> Command {
        let mut sandboxed = self.skim();
        sandboxed.arg("search").args(args);
        let mut cmd = Command::new(sandboxed.get_program());
        cmd.args(sandboxed.get_args());
        for (key, value) in sandboxed.get_envs() {
            match value {
                Some(value) => cmd.env(key, value),
                None => cmd.env_remove(key),
            };
        }
        if let Some(dir) = sandboxed.get_current_dir() {
            cmd.current_dir(dir);
        }
        cmd
    }
}

/// Run a command to the end and collect its result.
pub fn finish(cmd: &mut assert_cmd::Command) -> Run {
    let output = cmd.output().expect("run skim");
    Run {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}
