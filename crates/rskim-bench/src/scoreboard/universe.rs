//! The scoreboard oracle's own file universe (#203, #540).
//!
//! The oracle lists the files a plain-text search must be able to read, and
//! counts each candidate it leaves out, as an independent re-implementation of
//! the engine's universe (`crates/rskim/src/cmd/search/universe.rs`): nothing
//! here imports skim code. A policy change on the engine's side shows up as a
//! `universe.delta` or a `universe.skipped_by_reason_mismatch`.
//!
//! # Rules
//!
//! - **Candidates** — `git ls-files -z` (tracked) ∪ untracked-not-ignored
//!   files (`git ls-files -z --others --exclude-standard`) with no hidden
//!   (`.`-prefixed) path component. Gitlinks and skip-worktree entries are
//!   dropped. Hidden and secret-bearing tracked files are listed like any
//!   other file. On a verified-clean clone the untracked half is empty.
//! - **No language allow-list, no minified gate and no file cap.**
//! - **Left out, with a count per reason** ([`LEFT_OUT_REASONS`]) — binary,
//!   not UTF-8, larger than [`MAX_FILE_BYTES`], unreadable, vanished, outside
//!   the root, and symlinks. A symlink is counted and never read.
//! - **Containment** — a path that leaves the canonical root is counted and
//!   never read.
//!
//! Git runs under a [`GitIsolation`]: the same isolated `HOME` the skim
//! subprocess gets, with `GIT_CONFIG_NOSYSTEM=1`, so global excludes cannot
//! diverge between the two sides.
//!
//! # Known divergence (untracked files only; empty on a clean clone)
//!
//! The engine's walk honours `.ignore` files; `git ls-files
//! --exclude-standard` does not.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Context;

/// Files larger than this are left out (`too_large`): the engine's read bound.
pub const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// Bytes probed for a NUL byte by the binary check.
const BINARY_PROBE_BYTES: usize = 8192;

/// Timeout for each `git ls-files` call (seconds).
const GIT_TIMEOUT_SECS: u64 = 120;

/// Environment variables removed from every isolated git (or skim)
/// subprocess: anything that could point git at another repository or load
/// configuration from outside the isolated `HOME`.
const ISOLATION_REMOVED_ENV: &[&str] = &[
    "XDG_CONFIG_HOME",
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
];

// ============================================================================
// Git isolation
// ============================================================================

/// The isolated git environment shared by the oracle and the skim subprocess:
/// `HOME` points at a scoreboard-owned directory, `GIT_CONFIG_NOSYSTEM=1`,
/// and `ISOLATION_REMOVED_ENV` is cleared.
///
/// Apply the same instance to the skim command (see [`GitIsolation::apply`])
/// so both sides resolve global excludes from the same empty `HOME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIsolation {
    home: PathBuf,
}

impl GitIsolation {
    /// Isolate git under `home`.
    pub fn new(home: impl Into<PathBuf>) -> Self {
        GitIsolation { home: home.into() }
    }

    /// The isolated `HOME`.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Set `HOME` and `GIT_CONFIG_NOSYSTEM=1` on `cmd` and remove every
    /// variable in `ISOLATION_REMOVED_ENV`.
    pub fn apply(&self, cmd: &mut Command) {
        cmd.env("HOME", &self.home).env("GIT_CONFIG_NOSYSTEM", "1");
        for var in ISOLATION_REMOVED_ENV {
            cmd.env_remove(var);
        }
    }
}
// ============================================================================
// Text universe
// ============================================================================

/// Why a file is left out of the [`TextUniverse`]. These are the `left_out`
/// keys of the plain-text search's JSON envelope, so the two sides compare
/// key for key:
///
/// - `binary`: a NUL byte in the first `BINARY_PROBE_BYTES` bytes;
/// - `not_utf8`: the content is not strict UTF-8;
/// - `too_large`: more than [`MAX_FILE_BYTES`] bytes;
/// - `unreadable`: an I/O error, or a handle that is not a regular file;
/// - `vanished`: tracked, but absent from the work tree;
/// - `outside_root`: the real path leaves the canonical root, or git printed
///   a path that is not a plain relative path;
/// - `symlink`: a tracked entry of mode `120000`, or a walked entry whose
///   file type is a symlink. It is counted and never read.
///
/// A file that is both too large and binary counts as `too_large`: the size
/// is known from the metadata, before any byte is read.
pub const LEFT_OUT_REASONS: [&str; 7] = [
    "binary",
    "not_utf8",
    "too_large",
    "unreadable",
    "vanished",
    "outside_root",
    "symlink",
];

/// git's index mode of a symlink.
const MODE_SYMLINK: &str = "120000";

/// git's index mode of a gitlink (a submodule).
const MODE_GITLINK: &str = "160000";

/// The files a plain-text search must be able to read: the text files of the
/// work tree, with no language allow-list and no minified gate, and a count
/// for each reason a candidate is left out ([`LEFT_OUT_REASONS`]).
///
/// The candidates are the tracked files (`git ls-files`, gitlinks and
/// skip-worktree entries dropped) plus the untracked, not-ignored files with
/// no hidden path component. Hidden and secret-bearing tracked files are
/// listed. The oracle lists untracked files with `git`,
/// which does not read `.ignore` files; on a verified-clean clone the
/// untracked half is empty.
///
/// A path that is not UTF-8 is keyed by its lossy decoding (the JSON rule of
/// the search output).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextUniverse {
    files: BTreeMap<String, String>,
    left_out: BTreeMap<&'static str, u64>,
}

impl TextUniverse {
    fn new() -> Self {
        TextUniverse {
            files: BTreeMap::new(),
            left_out: LEFT_OUT_REASONS.iter().map(|&r| (r, 0)).collect(),
        }
    }

    /// Number of files in the universe.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether no file is in the universe.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Whether `path` is in the universe.
    pub fn contains(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    /// Text of a file of the universe.
    pub fn text(&self, path: &str) -> Option<&str> {
        self.files.get(path).map(String::as_str)
    }

    /// `(path, text)` for every file, byte-wise sorted.
    pub fn files(&self) -> impl Iterator<Item = (&str, &str)> {
        self.files.iter().map(|(p, t)| (p.as_str(), t.as_str()))
    }

    /// How many candidates each reason of [`LEFT_OUT_REASONS`] left out; every
    /// reason is a key, zero counts included.
    pub fn left_out(&self) -> &BTreeMap<&'static str, u64> {
        &self.left_out
    }

    /// Candidates left out for `reason` (`0` for a name that is not a reason).
    pub fn left_out_count(&self, reason: &str) -> u64 {
        self.left_out.get(reason).copied().unwrap_or(0)
    }

    fn count(&mut self, reason: &'static str) {
        *self.left_out.entry(reason).or_insert(0) += 1;
    }
}

/// What reading one candidate gave.
#[derive(Debug, PartialEq, Eq)]
enum TextOutcome {
    Admitted(String),
    LeftOut(&'static str),
    /// Not a candidate at all: a directory (a gitlink or a nested
    /// repository).
    Skipped,
}

/// Compute the text universe of the repository whose toplevel is `root`.
///
/// # Errors
///
/// Returns an error if `root` is not the toplevel of a git repository, if
/// `git ls-files` fails to spawn, times out or exits non-zero, if its output
/// cannot be parsed, or if two non-UTF-8 paths decode to the same lossy
/// string.
pub fn text_universe(root: &Path, git: &GitIsolation) -> anyhow::Result<TextUniverse> {
    if !root.join(".git").exists() {
        anyhow::bail!("{} is not a git repository root (no .git)", root.display());
    }
    let canonical_root = std::fs::canonicalize(root)
        .with_context(|| format!("canonicalizing {}", root.display()))?;
    let tracked = tracked_entries(root, git)?;
    let untracked: BTreeSet<Vec<u8>> = ls_files(root, git, &["--others", "--exclude-standard"])?
        .into_iter()
        .filter(|p| !tracked.contains_key(p) && !has_hidden_component(p))
        .collect();

    let mut universe = TextUniverse::new();
    let candidates = tracked
        .into_iter()
        .chain(untracked.into_iter().map(|p| (p, false)));
    for (raw, is_symlink_entry) in candidates {
        let abs = abs_path(root, &raw);
        let path = match String::from_utf8(raw) {
            Ok(p) => p,
            Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        };
        match candidate_outcome(&path, is_symlink_entry, &abs, &canonical_root) {
            TextOutcome::Admitted(text) => {
                anyhow::ensure!(
                    universe.files.insert(path.clone(), text).is_none(),
                    "two paths of {} decode to {path:?}",
                    root.display()
                );
            }
            TextOutcome::LeftOut(reason) => universe.count(reason),
            TextOutcome::Skipped => {}
        }
    }
    Ok(universe)
}

/// What one candidate gives. A path that is not a plain relative path
/// (absolute, or with a `..` component) leaves the root: it is counted
/// `outside_root` and never read, so the accounting stays complete. A tracked
/// symlink is counted `symlink` and never read. Any other path is read.
fn candidate_outcome(
    path: &str,
    is_symlink_entry: bool,
    abs: &Path,
    canonical_root: &Path,
) -> TextOutcome {
    if !is_safe_relative(path) {
        return TextOutcome::LeftOut("outside_root");
    }
    if is_symlink_entry {
        return TextOutcome::LeftOut("symlink");
    }
    read_text(abs, canonical_root)
}

/// `root` joined with the raw path bytes git printed, so a file whose name is
/// not UTF-8 is still read.
fn abs_path(root: &Path, raw: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        root.join(std::ffi::OsStr::from_bytes(raw))
    }
    #[cfg(not(unix))]
    {
        root.join(String::from_utf8_lossy(raw).as_ref())
    }
}

/// The tracked entries of `git ls-files -t -s -z`: raw path to "is a
/// symlink". Gitlinks (`160000`) and skip-worktree entries (tag `S`) are
/// dropped, as the search drops them.
fn tracked_entries(root: &Path, git: &GitIsolation) -> anyhow::Result<BTreeMap<Vec<u8>, bool>> {
    let mut entries = BTreeMap::new();
    for record in split_records(&ls_files_raw(root, git, &["-t", "-s"])?) {
        let tab = record
            .iter()
            .position(|&b| b == b'\t')
            .context("git ls-files -s record has no tab")?;
        let meta =
            std::str::from_utf8(&record[..tab]).context("git ls-files -s metadata is not UTF-8")?;
        let mut fields = meta.split(' ');
        let (Some(tag), Some(mode), Some(_object), Some(_stage), None) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            anyhow::bail!("unexpected git ls-files -t -s record {meta:?}");
        };
        if mode == MODE_GITLINK || tag == "S" {
            continue;
        }
        entries.insert(record[tab + 1..].to_vec(), mode == MODE_SYMLINK);
    }
    Ok(entries)
}

/// Read one candidate and admit it or name the reason it is left out.
/// `canonical_root` is the canonical work-tree root.
fn read_text(abs: &Path, canonical_root: &Path) -> TextOutcome {
    use std::io::Read as _;

    let meta = match std::fs::symlink_metadata(abs) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return TextOutcome::LeftOut("vanished");
        }
        Err(_) => return TextOutcome::LeftOut("unreadable"),
    };
    let kind = meta.file_type();
    if kind.is_symlink() {
        return TextOutcome::LeftOut("symlink");
    }
    if kind.is_dir() {
        return TextOutcome::Skipped;
    }
    if !kind.is_file() {
        return TextOutcome::LeftOut("unreadable");
    }
    match std::fs::canonicalize(abs) {
        Ok(real) if real.starts_with(canonical_root) => {}
        Ok(_) => return TextOutcome::LeftOut("outside_root"),
        Err(_) => return TextOutcome::LeftOut("unreadable"),
    }
    if meta.len() > MAX_FILE_BYTES {
        return TextOutcome::LeftOut("too_large");
    }
    // Bounded read: a file that grew since the metadata call is cut at the cap.
    let mut bytes = Vec::new();
    let read =
        std::fs::File::open(abs).and_then(|f| f.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes));
    if read.is_err() {
        return TextOutcome::LeftOut("unreadable");
    }
    if !u64::try_from(bytes.len()).is_ok_and(|n| n <= MAX_FILE_BYTES) {
        return TextOutcome::LeftOut("too_large");
    }
    if has_nul_probe(&bytes) {
        return TextOutcome::LeftOut("binary");
    }
    match String::from_utf8(bytes) {
        Ok(text) => TextOutcome::Admitted(text),
        Err(_) => TextOutcome::LeftOut("not_utf8"),
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Whether the first 8 KiB contain a NUL byte (the binary probe).
fn has_nul_probe(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_PROBE_BYTES)].contains(&0)
}

/// A non-empty relative path made only of normal components — the oracle's
/// own containment rule, standing in for skim's `is_repo_relative_safe` and
/// `Language::from_path`'s `..` rejection (`crates/rskim-core/src/types.rs`).
fn is_safe_relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// Whether any `/`-separated component starts with `.`.
fn has_hidden_component(path: &[u8]) -> bool {
    path.split(|&b| b == b'/')
        .any(|seg| seg.first() == Some(&b'.'))
}

/// `git -C <root> ls-files -z <extra>` under `git`'s isolation, with
/// repository discovery fenced at `root`'s parent so a non-repository `root`
/// can never borrow an enclosing checkout. Returns the raw path bytes.
fn ls_files(root: &Path, git: &GitIsolation, extra: &[&str]) -> anyhow::Result<Vec<Vec<u8>>> {
    Ok(split_records(&ls_files_raw(root, git, extra)?))
}

/// The NUL-separated, non-empty records of `git ls-files -z` output.
fn split_records(stdout: &[u8]) -> Vec<Vec<u8>> {
    stdout
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

/// The stdout of `git -C <root> ls-files -z <extra>`; see [`ls_files`].
fn ls_files_raw(root: &Path, git: &GitIsolation, extra: &[&str]) -> anyhow::Result<Vec<u8>> {
    let canonical = std::fs::canonicalize(root)
        .with_context(|| format!("canonicalizing {}", root.display()))?;
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(&canonical)
        .args(["ls-files", "-z"])
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    git.apply(&mut cmd);
    if let Some(parent) = canonical.parent() {
        cmd.env("GIT_CEILING_DIRECTORIES", parent);
    }
    let label = format!("git ls-files {} (scoreboard universe)", extra.join(" "));
    let out = rskim_research::clone::git_output_with_timeout(cmd, &label, GIT_TIMEOUT_SECS)?;
    if !out.status.success() {
        anyhow::bail!(
            "{label} failed in {}: {}",
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
mod tests {
    use super::*;
    use crate::scoreboard::test_support::FixtureRepo;

    /// `len` bytes of `x` whose only `newlines` newlines sit at the very end.
    fn one_long_line(len: usize, newlines: usize) -> String {
        let mut s = "x".repeat(len - newlines);
        s.push_str(&"\n".repeat(newlines));
        s
    }

    #[test]
    fn safe_relative_paths_only() {
        assert!(is_safe_relative("src/main.rs"));
        assert!(!is_safe_relative("../escape.rs"));
        assert!(!is_safe_relative("a/../../b.rs"));
        assert!(!is_safe_relative("/etc/passwd"));
        assert!(!is_safe_relative(""));
    }

    #[test]
    fn global_excludes_come_only_from_the_isolated_home() {
        let repo = FixtureRepo::new();
        repo.write("src/main.rs", "fn main() {}\n");
        repo.commit_all("init");
        repo.write("src/untracked.rs", "fn u() {}\n");

        let excluding_home = tempfile::tempdir().unwrap();
        let ignore = excluding_home.path().join(".config").join("git");
        std::fs::create_dir_all(&ignore).unwrap();
        std::fs::write(ignore.join("ignore"), "untracked.rs\n").unwrap();

        let excluded =
            text_universe(repo.root(), &GitIsolation::new(excluding_home.path())).unwrap();
        let clean = text(&repo);
        assert!(!excluded.contains("src/untracked.rs"));
        assert!(clean.contains("src/untracked.rs"));
    }

    // --- text universe -----------------------------------------------------------

    fn text(repo: &FixtureRepo) -> TextUniverse {
        text_universe(repo.root(), &GitIsolation::new(repo.home())).unwrap()
    }

    #[test]
    fn the_text_universe_has_no_extension_allow_list_and_no_minified_gate() {
        let repo = FixtureRepo::new();
        repo.write("src/main.rs", "fn main() {}\n");
        repo.write("LICENSE", "MIT\n");
        repo.write("scripts/build", "#!/bin/sh\necho build\n");
        repo.write("notes.txt", "a note\n");
        repo.write("bundle.js", one_long_line(70_000, 0));
        repo.commit_all("init");

        let t = text(&repo);
        for path in [
            "src/main.rs",
            "LICENSE",
            "scripts/build",
            "notes.txt",
            "bundle.js",
        ] {
            assert!(t.contains(path), "{path}");
        }
        assert_eq!(t.text("LICENSE"), Some("MIT\n"));
        assert_eq!(t.len(), 5);
    }

    #[test]
    fn every_left_out_reason_is_a_key_and_a_clean_tree_leaves_none_out() {
        let repo = FixtureRepo::new();
        repo.write("a.rs", "fn a() {}\n");
        repo.commit_all("init");

        let t = text(&repo);
        let keys: Vec<&str> = t.left_out().keys().copied().collect();
        let mut expected = LEFT_OUT_REASONS.to_vec();
        expected.sort_unstable();
        assert_eq!(keys, expected);
        assert!(t.left_out().values().all(|&n| n == 0), "{:?}", t.left_out());
    }

    #[test]
    fn each_left_out_reason_counts_its_files() {
        let repo = FixtureRepo::new();
        repo.write("ok.txt", "fine\n");
        repo.write("blob.bin", b"abc\0def".as_slice());
        repo.write("latin1.txt", [0x66u8, 0x6f, 0xe9, 0x0a].as_slice());
        let big = vec![b'x'; usize::try_from(MAX_FILE_BYTES).unwrap() + 1];
        repo.write("huge.txt", big);
        repo.write("gone.txt", "will vanish\n");
        repo.commit_all("init");
        std::fs::remove_file(repo.root().join("gone.txt")).unwrap();

        let t = text(&repo);
        assert_eq!(t.files().map(|(p, _)| p).collect::<Vec<_>>(), ["ok.txt"]);
        for (reason, n) in [
            ("binary", 1),
            ("not_utf8", 1),
            ("too_large", 1),
            ("vanished", 1),
            ("symlink", 0),
            ("unreadable", 0),
            ("outside_root", 0),
        ] {
            assert_eq!(t.left_out_count(reason), n, "{reason}");
        }
        assert_eq!(t.left_out_count("no_such_reason"), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_tracked_symlink_is_counted_symlink_and_never_read() {
        let repo = FixtureRepo::new();
        repo.write("real.rs", "fn real() {}\n");
        std::os::unix::fs::symlink("real.rs", repo.root().join("link.rs")).unwrap();
        repo.commit_all("init");

        let t = text(&repo);
        assert_eq!(t.files().map(|(p, _)| p).collect::<Vec<_>>(), ["real.rs"]);
        assert_eq!(t.left_out_count("symlink"), 1);
    }

    #[cfg(unix)]
    #[test]
    fn an_untracked_walked_symlink_is_counted_symlink_and_never_read() {
        let repo = FixtureRepo::new();
        repo.write("real.rs", "fn real() {}\n");
        repo.commit_all("init");
        // Untracked, so git lists it as a plain walked entry (not a mode 120000
        // index entry); it points inside the root, so a read would admit it.
        std::os::unix::fs::symlink("real.rs", repo.root().join("link.rs")).unwrap();
        assert!(
            repo.git(&["ls-files", "--others", "--exclude-standard"])
                .contains("link.rs"),
            "fixture: the walk lists the untracked symlink"
        );

        let t = text(&repo);
        assert!(t.contains("real.rs"));
        assert!(!t.contains("link.rs"));
        assert_eq!(t.left_out_count("symlink"), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_tracked_file_replaced_by_a_fifo_is_unreadable_and_never_opened() {
        use std::sync::mpsc;
        use std::time::Duration;

        let repo = FixtureRepo::new();
        repo.write("pipe.txt", "was a file\n");
        repo.write("kept.txt", "kept\n");
        repo.commit_all("init");
        let fifo = repo.root().join("pipe.txt");
        std::fs::remove_file(&fifo).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success(), "fixture: mkfifo");

        // Opening a FIFO that has no writer blocks forever, so a guard that
        // regressed would hang this test instead of failing it: build the
        // universe on a thread and bound the wait.
        let (root, home) = (repo.root().to_path_buf(), repo.home().to_path_buf());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(text_universe(&root, &GitIsolation::new(home)));
        });
        let t = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the build must finish: a non-regular file is left out without being opened")
            .unwrap();

        assert_eq!(t.files().map(|(p, _)| p).collect::<Vec<_>>(), ["kept.txt"]);
        assert_eq!(t.left_out_count("unreadable"), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_tracked_file_that_cannot_be_opened_is_unreadable() {
        use std::os::unix::fs::PermissionsExt as _;

        let repo = FixtureRepo::new();
        repo.write("locked.txt", "locked\n");
        repo.write("kept.txt", "kept\n");
        repo.commit_all("init");
        let locked = repo.root().join("locked.txt");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::File::open(&locked).is_ok() {
            // A superuser opens any file: the open-error arm cannot be reached.
            eprintln!("skipped: this user can open a mode 000 file");
            return;
        }

        let t = text(&repo);
        assert_eq!(t.files().map(|(p, _)| p).collect::<Vec<_>>(), ["kept.txt"]);
        assert_eq!(t.left_out_count("unreadable"), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_tracked_file_reached_through_a_symlinked_parent_is_outside_root_and_never_read() {
        let repo = FixtureRepo::new();
        repo.write("dir/f.txt", "inside\n");
        repo.write("kept.txt", "kept\n");
        repo.commit_all("init");
        // `dir/f.txt` stays tracked and is a regular file as far as an lstat of
        // the path can tell, but its real path leaves the root.
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("f.txt"), "outside secret\n").unwrap();
        std::fs::remove_dir_all(repo.root().join("dir")).unwrap();
        std::os::unix::fs::symlink(outside.path(), repo.root().join("dir")).unwrap();

        let t = text(&repo);
        assert!(t.contains("kept.txt"));
        assert!(!t.contains("dir/f.txt"));
        assert_eq!(t.left_out_count("outside_root"), 1);
        assert!(t.files().all(|(_, body)| !body.contains("outside secret")));
    }

    // APFS refuses a file name that is not UTF-8, so only Linux runs this test.
    #[cfg(target_os = "linux")]
    #[test]
    fn two_non_utf8_paths_with_one_lossy_form_are_an_error() {
        use std::os::unix::ffi::OsStrExt as _;

        let repo = FixtureRepo::new();
        // 0xFF and 0xFE are both invalid UTF-8: each decodes to U+FFFD, so both
        // names become the key "a\u{FFFD}.txt".
        for (name, body) in [(&b"a\xff.txt"[..], "one\n"), (&b"a\xfe.txt"[..], "two\n")] {
            std::fs::write(repo.root().join(std::ffi::OsStr::from_bytes(name)), body).unwrap();
        }
        repo.commit_all("init");

        let err = text_universe(repo.root(), &GitIsolation::new(repo.home())).unwrap_err();
        assert!(format!("{err:#}").contains("decode to"), "{err:#}");
    }

    #[test]
    fn a_file_over_the_cap_that_is_also_binary_counts_as_too_large() {
        let repo = FixtureRepo::new();
        let mut big = vec![b'x'; usize::try_from(MAX_FILE_BYTES).unwrap() + 1];
        big[0] = 0;
        repo.write("huge.bin", big);
        repo.commit_all("init");

        let t = text(&repo);
        assert_eq!(t.left_out_count("too_large"), 1);
        assert_eq!(t.left_out_count("binary"), 0);
    }

    #[test]
    fn gitlinks_skip_worktree_entries_and_directories_are_not_candidates() {
        let repo = FixtureRepo::new();
        repo.write("kept.txt", "kept\n");
        repo.write("sparse.txt", "sparse\n");
        repo.commit_all("init");
        // A gitlink: a submodule entry in the index, a directory in the tree.
        repo.git(&[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000,1111111111111111111111111111111111111111,sub",
        ]);
        std::fs::create_dir(repo.root().join("sub")).unwrap();
        repo.git(&["update-index", "--skip-worktree", "sparse.txt"]);
        std::fs::remove_file(repo.root().join("sparse.txt")).unwrap();

        let t = text(&repo);
        assert_eq!(
            t.files().map(|(p, _)| p).collect::<Vec<_>>(),
            vec!["kept.txt"]
        );
        assert!(
            t.left_out().values().all(|&n| n == 0),
            "neither entry is counted: {:?}",
            t.left_out()
        );
    }

    #[test]
    fn untracked_files_join_unless_ignored_or_under_a_hidden_component() {
        let repo = FixtureRepo::new();
        repo.write(".gitignore", "ignored.txt\n");
        repo.write("tracked.txt", "t\n");
        repo.commit_all("init");
        repo.write("new.txt", "n\n");
        repo.write("ignored.txt", "i\n");
        repo.write(".hidden/secret.txt", "h\n");

        let t = text(&repo);
        assert!(t.contains("tracked.txt") && t.contains("new.txt"));
        assert!(t.contains(".gitignore"), "a tracked hidden file is listed");
        assert!(!t.contains("ignored.txt") && !t.contains(".hidden/secret.txt"));
    }

    #[test]
    fn a_path_that_leaves_the_root_is_counted_outside_root_and_never_read() {
        let repo = FixtureRepo::new();
        repo.write("inside.txt", "inside\n");
        let root = std::fs::canonicalize(repo.root()).unwrap();
        // Both files exist and are readable, so a read would admit them.
        for (path, abs) in [
            ("../escape.txt", root.join("inside.txt")),
            ("/etc/passwd", root.join("inside.txt")),
        ] {
            for symlink_entry in [false, true] {
                assert_eq!(
                    candidate_outcome(path, symlink_entry, &abs, &root),
                    TextOutcome::LeftOut("outside_root"),
                    "{path} (symlink entry: {symlink_entry})"
                );
            }
        }
        assert_eq!(
            candidate_outcome("inside.txt", false, &root.join("inside.txt"), &root),
            TextOutcome::Admitted("inside\n".to_string())
        );
    }

    #[test]
    fn a_directory_that_is_not_a_repository_root_is_an_error_for_the_text_universe() {
        let dir = tempfile::tempdir().unwrap();
        let err = text_universe(dir.path(), &GitIsolation::new(dir.path())).unwrap_err();
        assert!(
            format!("{err:#}").contains("not a git repository root"),
            "{err:#}"
        );
    }
}
