//! File source abstraction for loading source files from the corpus.
//!
//! `GitCloneSource` clones repos with `git`; `FixtureSource` reads from
//! a local directory. Both implement `FileSource` for testing and production.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Context;
use rskim_core::Language;

use crate::config::RepoEntry;
use crate::types::SourceFile;

/// Maximum file size to accept (100 KiB).
const MAX_FILE_SIZE: u64 = 100 * 1024;

/// Number of bytes to inspect for null bytes (binary detection).
const BINARY_PROBE_BYTES: usize = 8192;

/// File extensions explicitly excluded for the lexical bigram corpus (data formats, not code).
///
/// This list is only applied when using the default `TARGET_EXTENSIONS`.
/// When an explicit extension list is passed to `walk_and_load`, no extensions
/// are excluded beyond what the caller provides.
const EXCLUDED_EXTENSIONS: &[&str] = &["json", "yaml", "yml", "toml", "md", "markdown"];

/// Target language file extensions accepted by the lexical bigram corpus.
const TARGET_EXTENSIONS: &[&str] = &["rs", "ts", "tsx", "py", "go", "java"];

/// Target file extensions for the AST n-gram corpus (all 14 tree-sitter languages).
pub const AST_TARGET_EXTENSIONS: &[&str] = &[
    "rs",    // Rust
    "ts",    // TypeScript
    "tsx",   // TypeScript (JSX)
    "js",    // JavaScript
    "jsx",   // JavaScript (JSX)
    "py",    // Python
    "go",    // Go
    "java",  // Java
    "c",     // C
    "h",     // C headers
    "cpp",   // C++
    "cc",    // C++
    "cxx",   // C++
    "hpp",   // C++ headers
    "cs",    // C#
    "rb",    // Ruby
    "sql",   // SQL
    "kt",    // Kotlin
    "kts",   // Kotlin script
    "swift", // Swift
    "md",    // Markdown
];

/// Abstraction over file loading — enables testing without network access.
pub trait FileSource: Send + Sync {
    fn fetch_files(&self, repo: &RepoEntry) -> anyhow::Result<Vec<SourceFile>>;
}

/// Production file source that clones repos from GitHub.
pub struct GitCloneSource {
    pub corpus_dir: PathBuf,
}

impl FileSource for GitCloneSource {
    fn fetch_files(&self, repo: &RepoEntry) -> anyhow::Result<Vec<SourceFile>> {
        let dest = ensure_cloned(&self.corpus_dir, repo)?;
        walk_and_load(&dest, None)
    }
}

/// An AST-aware file source that clones repos and walks with AST extensions.
pub struct AstGitCloneSource {
    pub corpus_dir: PathBuf,
}

impl FileSource for AstGitCloneSource {
    fn fetch_files(&self, repo: &RepoEntry) -> anyhow::Result<Vec<SourceFile>> {
        let dest = ensure_cloned(&self.corpus_dir, repo)?;
        walk_and_load_ast(&dest)
    }
}

/// Resolve the local clone directory for a repo, cloning it if not already present.
///
/// Returns the path to the checked-out repository root.
fn ensure_cloned(corpus_dir: &Path, repo: &RepoEntry) -> anyhow::Result<PathBuf> {
    let repo_name = extract_repo_name(&repo.url)?;
    let dest = corpus_dir.join(&repo_name);

    if !dest.exists() {
        clone_repo(&repo.url, &repo.commit, &dest)
            .with_context(|| format!("cloning {}", repo.url))?;
    }

    Ok(dest)
}

pub fn extract_repo_name(url: &str) -> anyhow::Result<String> {
    let name = url
        .rsplit('/')
        .next()
        .map(|s| s.trim_end_matches(".git").to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("cannot extract repo name from URL: {url}"))?;

    // Reject names that would escape the corpus directory via path traversal.
    if name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        anyhow::bail!("unsafe repo name extracted from URL (path traversal): {name:?}");
    }

    Ok(name)
}

/// Timeout for any single `git` subprocess (seconds).
const GIT_SUBPROCESS_TIMEOUT_SECS: u64 = 300;

/// Spawn a child process, hand it to a wait closure on a background thread,
/// and enforce a hard deadline.  Returns `Err` if spawning fails, the wait
/// closure returns an error, or the deadline expires.
///
/// The wait strategy is parameterised so callers can use either `Child::wait`
/// (discard output) or `Child::wait_with_output` (capture stdout/stderr)
/// without duplicating the spawn/channel/kill/join boilerplate.
///
/// # Platform notes
///
/// On timeout the child is killed via SIGKILL (Unix) or `taskkill /F` (Windows)
/// using the pid captured before the child is moved onto the background thread.
/// The background thread is then joined; because the process has already been
/// killed this join completes immediately.
fn run_with_timeout<F, T>(
    child: std::process::Child,
    label: &str,
    timeout_secs: u64,
    wait_fn: F,
) -> anyhow::Result<T>
where
    F: FnOnce(std::process::Child) -> std::io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    use std::sync::mpsc;
    use std::time::Duration;

    // Capture the pid before moving `child` onto the background thread so we
    // can send SIGKILL without needing the `Child` handle back from the thread.
    let child_id = child.id();
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        let _ = tx.send(wait_fn(child));
    });

    match rx.recv_timeout(Duration::from_secs(timeout_secs)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(anyhow::anyhow!("{label} wait error: {e}")),
        Err(_timeout) => {
            // Kill the process using its pid via a platform-appropriate signal.
            // `std::process::Command` does not give us back the `Child` after
            // handing it to the thread, so we use the raw pid.
            #[cfg(unix)]
            {
                // SAFETY: kill(2) is always safe to call with a valid pid.
                unsafe {
                    libc::kill(child_id as libc::pid_t, libc::SIGKILL);
                }
            }
            #[cfg(not(unix))]
            {
                // On Windows, TerminateProcess via taskkill is the safest
                // portable option available without the Child handle.
                let _ = std::process::Command::new("taskkill")
                    .args(["/F", "/PID", &child_id.to_string()])
                    .status();
            }
            // Join the background thread: the killed process exits quickly, so
            // this does not block indefinitely.  Joining prevents the thread
            // from becoming permanently detached after SIGKILL.
            let _ = handle.join();
            anyhow::bail!("{label} timed out after {timeout_secs}s");
        }
    }
}

/// Spawn a `git` command and wait for it to finish, killing it if it exceeds
/// `GIT_SUBPROCESS_TIMEOUT_SECS`.  Returns `Ok(true)` on success, `Ok(false)`
/// on non-zero exit, and `Err` if the process could not be spawned or the
/// timeout expired.
pub fn git_run_with_timeout(mut cmd: std::process::Command, label: &str) -> anyhow::Result<bool> {
    let child = cmd.spawn().with_context(|| format!("spawning {label}"))?;
    run_with_timeout(child, label, GIT_SUBPROCESS_TIMEOUT_SECS, |mut c| {
        c.wait().map(|s| s.success())
    })
}

/// Spawn a `git` command and wait for its output, killing it if it exceeds
/// `timeout_secs`.  Returns the captured [`std::process::Output`] on success.
///
/// Unlike [`git_run_with_timeout`], this variant uses `wait_with_output()` on
/// the background thread so that stdout/stderr are captured for the caller.
/// The `cmd` must have `stdout(Stdio::piped())` set by the caller.
pub fn git_output_with_timeout(
    mut cmd: std::process::Command,
    label: &str,
    timeout_secs: u64,
) -> anyhow::Result<std::process::Output> {
    let child = cmd.spawn().with_context(|| format!("spawning {label}"))?;
    run_with_timeout(child, label, timeout_secs, |c| c.wait_with_output())
}

fn clone_repo(url: &str, commit: &str, dest: &Path) -> anyhow::Result<()> {
    let dest_str = dest
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("dest path is not valid UTF-8: {}", dest.display()))?;

    // Hardened git clone flags:
    //   - credential.helper=''  : suppress credential prompts (fail fast on auth errors)
    //   - transfer.fsckObjects=true : reject corrupted/malicious objects
    let security_args = [
        "-c",
        "credential.helper=",
        "-c",
        "transfer.fsckObjects=true",
    ];

    // Try shallow clone first for speed.
    let mut shallow_cmd = std::process::Command::new("git");
    shallow_cmd
        .args(security_args)
        .args(["clone", "--depth", "1", url])
        .arg(dest);
    let shallow_ok =
        git_run_with_timeout(shallow_cmd, "git clone --depth 1").context("running git clone")?;

    if shallow_ok {
        // Shallow clone succeeded — check if the pinned commit is reachable.
        let checkout_ok = std::process::Command::new("git")
            .args(["-C", dest_str, "cat-file", "-t", commit])
            .status()
            .context("checking if pinned commit exists in shallow clone")?
            .success();

        if checkout_ok {
            let status = std::process::Command::new("git")
                .args(["-C", dest_str, "checkout", commit])
                .status()
                .context("running git checkout on shallow clone")?;
            if status.success() {
                return Ok(());
            }
        }

        // Pinned commit not in shallow clone — remove and do full clone.
        std::fs::remove_dir_all(dest)
            .with_context(|| format!("removing shallow clone at {}", dest.display()))?;
    }

    // Full clone to access the pinned commit.
    let mut full_cmd = std::process::Command::new("git");
    full_cmd.args(security_args).args(["clone", url]).arg(dest);
    let ok =
        git_run_with_timeout(full_cmd, "git clone (full)").context("running full git clone")?;

    if !ok {
        anyhow::bail!("git clone failed for {url}");
    }

    // Checkout the pinned commit.
    let status = std::process::Command::new("git")
        .args(["-C", dest_str, "checkout", commit])
        .status()
        .context("running git checkout")?;

    if !status.success() {
        anyhow::bail!("git checkout {commit} failed in {}", dest.display());
    }

    Ok(())
}

/// Walk `root` and load all source files matching the given extension list.
///
/// If `extensions` is `None`, the default lexical corpus extensions
/// (`TARGET_EXTENSIONS`) are used and `EXCLUDED_EXTENSIONS` is applied.
/// If `extensions` is `Some(list)`, only those extensions are accepted and
/// the exclusion list is NOT applied — the caller controls what is included.
pub(crate) fn walk_and_load(
    root: &Path,
    extensions: Option<&[&str]>,
) -> anyhow::Result<Vec<SourceFile>> {
    let mut files = Vec::new();

    // Build a HashSet once before the walk so extension lookup is O(1) per entry
    // instead of O(n) linear scan through the slice.
    let allowed_set: Option<HashSet<&str>> = extensions.map(|exts| exts.iter().copied().collect());

    let walker = ignore::WalkBuilder::new(root)
        .hidden(false) // include dot-files but .gitignore is respected
        .build();

    for entry in walker {
        let entry = entry.context("walking directory")?;
        if entry.file_type().map(|t| !t.is_file()).unwrap_or(true) {
            continue;
        }

        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        match &allowed_set {
            None => {
                // Default lexical mode: apply exclusion list then target list.
                if EXCLUDED_EXTENSIONS.contains(&ext.as_str()) {
                    continue;
                }
                if !TARGET_EXTENSIONS.contains(&ext.as_str()) {
                    continue;
                }
            }
            Some(allowed) => {
                // Explicit extension set: no exclusion, only allow listed exts.
                if !allowed.contains(ext.as_str()) {
                    continue;
                }
            }
        }

        // Skip files that are too large.
        if entry
            .metadata()
            .map(|m| m.len() > MAX_FILE_SIZE)
            .unwrap_or(false)
        {
            continue;
        }

        // Detect language from extension.
        let language = match Language::from_extension(&ext) {
            Some(lang) => lang,
            None => continue,
        };

        // Read and validate content.
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => continue,
        };

        // Binary detection: look for null bytes in first BINARY_PROBE_BYTES.
        let probe_len = bytes.len().min(BINARY_PROBE_BYTES);
        if bytes[..probe_len].contains(&0u8) {
            continue;
        }

        // Require valid UTF-8.
        let content = match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };

        files.push(SourceFile {
            path: path.to_path_buf(),
            language,
            content,
        });
    }

    Ok(files)
}

/// Walk `root` and load source files for all 14 tree-sitter languages.
///
/// Uses `AST_TARGET_EXTENSIONS` as the extension filter. No exclusion list
/// is applied — the caller decides which extensions to accept.
pub fn walk_and_load_ast(root: &Path) -> anyhow::Result<Vec<SourceFile>> {
    walk_and_load(root, Some(AST_TARGET_EXTENSIONS))
}

/// Test file source that reads from a fixture directory.
pub struct FixtureSource {
    pub fixture_dir: PathBuf,
}

impl FileSource for FixtureSource {
    fn fetch_files(&self, _repo: &RepoEntry) -> anyhow::Result<Vec<SourceFile>> {
        walk_and_load(&self.fixture_dir, None)
    }
}

/// Load all source files from a directory (public helper for the codegen step).
pub fn load_fixture_files(dir: &Path) -> anyhow::Result<Vec<SourceFile>> {
    walk_and_load(dir, None)
}

/// Clone a repository with full history (no `--depth 1`) for co-change analysis.
///
/// Unlike [`GitCloneSource`] which shallow-clones to a pinned commit, this
/// function always performs a full clone and stays at HEAD.  Full history is
/// required by [`rskim_search::temporal::GixSource`] to compute co-change
/// signal across the entire commit log.
///
/// # Idempotency
///
/// If `dest` already exists the function returns `Ok(())` immediately without
/// re-cloning, matching the behaviour of [`clone_repo`].
///
/// # Errors
///
/// Returns an error if:
/// - `url` fails the HTTPS prefix check (to guard against shell-injection via
///   `git://` or `file://` schemes).
/// - The `git clone` subprocess fails or times out.
pub fn clone_with_history(url: &str, dest: &Path) -> anyhow::Result<()> {
    if !url.starts_with("https://") {
        anyhow::bail!("clone_with_history: url must start with 'https://', got: {url}");
    }

    // Skip if already cloned (idempotent).
    //
    // Verify that the directory contains a valid git repository, not just a
    // leftover from a partial or interrupted clone.  A partial clone creates
    // the destination directory but may not write `.git/HEAD`, so checking for
    // that file distinguishes a complete clone from a broken one.
    if dest.exists() {
        if dest.join(".git").join("HEAD").exists() {
            return Ok(());
        }
        // Partial clone detected: remove the broken directory and re-clone.
        std::fs::remove_dir_all(dest)
            .with_context(|| format!("removing partial clone at {}", dest.display()))?;
    }

    let security_args = [
        "-c",
        "credential.helper=",
        "-c",
        "transfer.fsckObjects=true",
    ];

    let mut cmd = std::process::Command::new("git");
    cmd.args(security_args)
        .args(["clone", "--single-branch", url])
        .arg(dest);

    let ok = git_run_with_timeout(cmd, "git clone (full history)")
        .with_context(|| format!("cloning {url} with full history"))?;

    if !ok {
        anyhow::bail!("git clone failed for {url}");
    }

    Ok(())
}

// ============================================================================
// Pinned full-history clone (search scoreboard corpora, #203)
// ============================================================================

/// Timeout (seconds) for the network-bound steps of a pinned-history clone:
/// `git clone` and the `git fetch origin <sha>` fallback. Matches
/// [`GIT_SUBPROCESS_TIMEOUT_SECS`], which already bounds full clones elsewhere
/// in this module.
const PINNED_NETWORK_TIMEOUT_SECS: u64 = 300;

/// Timeout (seconds) for the local steps of a pinned-history clone:
/// `git checkout` and every reuse-verification command.
const PINNED_LOCAL_TIMEOUT_SECS: u64 = 120;

/// Maximum number of `git status` lines kept in a [`PinnedCloneState::Dirty`]
/// verdict (diagnostics only; the verdict itself is decided on emptiness).
const DIRTY_SAMPLE_MAX_LINES: usize = 20;

/// Name of the ownership marker written inside `<dest>/.git/` once a clone
/// created by [`ensure_pinned_history_clone`] exists. Living under `.git/`
/// keeps it invisible to `git status`, so it never makes the tree "dirty".
const OWNERSHIP_MARKER: &str = "skim-scoreboard-pinned-clone";

/// Security args shared by the network-bound git subprocesses of a
/// pinned-history clone: suppress credential prompts (fail fast on auth
/// errors) and reject corrupted or malicious objects.
///
/// Transfer fsck runs `index-pack --strict`, which promotes every fsck
/// warning to an error. Exactly one message id is downgraded:
/// `zeroPaddedFilemode` (a tree mode written as `040000` by old git
/// versions). It is cosmetic, and real corpora carry it: pallets/flask's
/// history does (object `0b404df8…`), so a strict clone of flask fails.
/// Every other check, including `hasDotgit` and the other path checks,
/// stays fatal.
const PINNED_CLONE_SECURITY_ARGS: [&str; 6] = [
    "-c",
    "credential.helper=",
    "-c",
    "transfer.fsckObjects=true",
    "-c",
    "fetch.fsck.zeroPaddedFilemode=ignore",
];

/// Environment variables that would redirect a `git -C <dest> …` command at a
/// different repository, index, or object store than the one at `dest` (they
/// are set, for example, when this code runs inside a git hook). Every git
/// subprocess spawned for a pinned clone removes them.
const GIT_REDIRECT_ENV_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
];

/// The state of a directory that should hold a pinned full-history clone, as
/// judged by [`verify_pinned_clone`].
///
/// Only [`PinnedCloneState::Reusable`] means "use it as is"; every other
/// variant names the first reuse condition that failed, so callers can report
/// why a clone was rejected (or, after a scoreboard run, what skim changed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinnedCloneState {
    /// `dest` is the root of a non-shallow repository, `HEAD` is the pinned
    /// commit, and `git status` (including ignored files) is empty.
    Reusable,
    /// `dest` does not exist.
    Missing,
    /// `dest` exists but is not the root of its own git repository: not a
    /// directory, a symlink, an empty or partial directory, or a directory
    /// whose repository toplevel is somewhere else.
    NotRepositoryRoot { detail: String },
    /// `HEAD` does not resolve to the pinned commit (`actual` is `None` when
    /// `HEAD` cannot be resolved at all).
    HeadMismatch {
        expected: String,
        actual: Option<String>,
    },
    /// The repository is a shallow clone; the temporal layer needs full
    /// history.
    Shallow,
    /// `git status --porcelain --untracked-files=all --ignored` is not empty.
    /// `sample` holds its first lines.
    Dirty { sample: String },
}

impl PinnedCloneState {
    /// Whether the clone can be reused as is.
    pub fn is_reusable(&self) -> bool {
        matches!(self, PinnedCloneState::Reusable)
    }
}

impl std::fmt::Display for PinnedCloneState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PinnedCloneState::Reusable => write!(f, "reusable"),
            PinnedCloneState::Missing => write!(f, "missing"),
            PinnedCloneState::NotRepositoryRoot { detail } => {
                write!(f, "not a repository root ({detail})")
            }
            PinnedCloneState::HeadMismatch { expected, actual } => match actual {
                Some(actual) => write!(f, "HEAD is {actual}, expected {expected}"),
                None => write!(f, "HEAD does not resolve, expected {expected}"),
            },
            PinnedCloneState::Shallow => write!(f, "shallow clone (full history required)"),
            PinnedCloneState::Dirty { sample } => write!(f, "working tree not clean:\n{sample}"),
        }
    }
}

/// Ensure `dest` is a full-history clone of `url`, checked out (detached) at
/// `commit`, reusing an existing clone when it is verifiably in that state.
///
/// # Reuse
///
/// An existing `dest` is reused, with no network access, only when
/// [`verify_pinned_clone`] reports [`PinnedCloneState::Reusable`]:
/// - `dest` is the toplevel of its own repository (repository discovery is
///   fenced at `dest`'s parent, so an empty directory inside some other
///   checkout never borrows that checkout's `HEAD`);
/// - `git rev-parse HEAD` equals `commit`;
/// - `git rev-parse --is-shallow-repository` is `false`;
/// - `git status --porcelain --untracked-files=all --ignored` is empty.
///
/// `--ignored` is stricter than a plain `status`: a file skim (or anything
/// else) writes into the corpus root is caught even when the corpus's own
/// `.gitignore` or the user's global excludes would hide it.
///
/// Otherwise `dest` is deleted and re-cloned **once**. A failure of that one
/// fresh clone (clone, fetch, checkout, or post-clone verification) is
/// returned as `Err` with no further retry; the scoreboard maps it to a
/// harness error, never to a regression.
///
/// # Deletion safety
///
/// `dest` is deleted only when it is an empty directory or carries the
/// ownership marker a previous call wrote into `dest/.git/`. Any other
/// non-reusable `dest` (say, a developer's own checkout that a mistyped
/// `--corpus-dir` points at) is left untouched and reported as an error.
///
/// # Clone shape
///
/// `git clone --no-checkout` with no `--depth` and no `--filter`: a shallow or
/// blobless clone would make the temporal layer fetch objects lazily. The
/// commit is then checked out with `checkout --detach`. If the commit is not
/// reachable from the cloned refs, `fetch origin <commit>` runs once and the
/// checkout is retried once. Every git subprocess is bounded by a timeout.
///
/// # Errors
///
/// Returns an error if `url` does not start with `https://`, if `commit` is
/// not a 40-character lowercase hex SHA, if a non-reusable `dest` is not
/// owned by this function, if any git subprocess fails to spawn, times out or
/// exits non-zero, or if the fresh clone fails verification.
pub fn ensure_pinned_history_clone(url: &str, commit: &str, dest: &Path) -> anyhow::Result<()> {
    if !url.starts_with("https://") {
        anyhow::bail!("ensure_pinned_history_clone: url must start with 'https://', got: {url}");
    }
    ensure_pinned_history_clone_from(url, commit, dest)
}

/// Report the [`PinnedCloneState`] of `dest` against the pinned `commit`.
///
/// Read-only: it never modifies `dest`. The scoreboard runs it before its
/// queries (through [`ensure_pinned_history_clone`]) and again after them, to
/// prove skim wrote nothing into the corpus root.
///
/// # Errors
///
/// Returns an error if `commit` is not a 40-character lowercase hex SHA, if
/// `dest` cannot be inspected, or if a git subprocess fails to spawn, times
/// out, or fails in a way that says nothing about `dest` (for example
/// `git status` itself erroring). A clean non-zero exit that answers a reuse
/// question (not a repository, `HEAD` unresolvable) is a verdict, not an
/// error.
pub fn verify_pinned_clone(dest: &Path, commit: &str) -> anyhow::Result<PinnedCloneState> {
    validate_pinned_commit(commit)?;

    let meta = match std::fs::symlink_metadata(dest) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PinnedCloneState::Missing);
        }
        Err(e) => {
            return Err(anyhow::anyhow!(e).context(format!("inspecting {}", dest.display())));
        }
    };
    if !meta.is_dir() {
        return Ok(PinnedCloneState::NotRepositoryRoot {
            detail: "not a directory (or a symlink)".to_string(),
        });
    }

    let canonical = std::fs::canonicalize(dest)
        .with_context(|| format!("canonicalizing {}", dest.display()))?;

    let toplevel_out = run_pinned_git(&canonical, &["rev-parse", "--show-toplevel"])?;
    if !toplevel_out.status.success() {
        return Ok(PinnedCloneState::NotRepositoryRoot {
            detail: first_line_lossy(&toplevel_out.stderr),
        });
    }
    let toplevel = PathBuf::from(stdout_trimmed(&toplevel_out));
    let toplevel_canonical = std::fs::canonicalize(&toplevel).unwrap_or(toplevel);
    if toplevel_canonical != canonical {
        return Ok(PinnedCloneState::NotRepositoryRoot {
            detail: format!("repository toplevel is {}", toplevel_canonical.display()),
        });
    }

    let head_out = run_pinned_git(&canonical, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let actual = head_out
        .status
        .success()
        .then(|| stdout_trimmed(&head_out))
        .filter(|s| !s.is_empty());
    if actual.as_deref() != Some(commit) {
        return Ok(PinnedCloneState::HeadMismatch {
            expected: commit.to_string(),
            actual,
        });
    }

    let shallow_out = run_pinned_git(&canonical, &["rev-parse", "--is-shallow-repository"])?;
    if !shallow_out.status.success() {
        anyhow::bail!(
            "git rev-parse --is-shallow-repository failed in {}: {}",
            canonical.display(),
            first_line_lossy(&shallow_out.stderr)
        );
    }
    if stdout_trimmed(&shallow_out) != "false" {
        return Ok(PinnedCloneState::Shallow);
    }

    let status_out = run_pinned_git(
        &canonical,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored",
        ],
    )?;
    if !status_out.status.success() {
        anyhow::bail!(
            "git status failed in {}: {}",
            canonical.display(),
            first_line_lossy(&status_out.stderr)
        );
    }
    let status = String::from_utf8_lossy(&status_out.stdout);
    if !status.trim().is_empty() {
        let sample = status
            .lines()
            .take(DIRTY_SAMPLE_MAX_LINES)
            .collect::<Vec<_>>()
            .join("\n");
        return Ok(PinnedCloneState::Dirty { sample });
    }

    Ok(PinnedCloneState::Reusable)
}

/// [`ensure_pinned_history_clone`] without the `https://` scheme check, so
/// unit tests can point it at a local `file://` fixture remote. Private:
/// production callers must go through the checked entry point.
fn ensure_pinned_history_clone_from(url: &str, commit: &str, dest: &Path) -> anyhow::Result<()> {
    validate_pinned_commit(commit)?;

    let before = verify_pinned_clone(dest, commit)?;
    if before.is_reusable() {
        return Ok(());
    }
    if before != PinnedCloneState::Missing {
        remove_owned_clone(dest, &before)?;
    }

    clone_pinned_history_once(url, commit, dest)?;

    match verify_pinned_clone(dest, commit)? {
        PinnedCloneState::Reusable => Ok(()),
        after => anyhow::bail!(
            "fresh clone of {url} at {commit} into {} failed verification: {after}",
            dest.display()
        ),
    }
}

/// Require a full 40-character lowercase hex SHA — the form `git rev-parse`
/// prints, so the reuse comparison is exact, and a form that can never be
/// read as a git option (`--upload-pack=…`) or a revision expression.
fn validate_pinned_commit(commit: &str) -> anyhow::Result<()> {
    let ok = commit.len() == 40
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !ok {
        anyhow::bail!("pinned commit must be a 40-character lowercase hex SHA, got: {commit:?}");
    }
    Ok(())
}

/// Delete a non-reusable `dest`, but only if it is an empty directory or
/// carries the ownership marker (see [`OWNERSHIP_MARKER`]).
fn remove_owned_clone(dest: &Path, state: &PinnedCloneState) -> anyhow::Result<()> {
    let meta = std::fs::symlink_metadata(dest)
        .with_context(|| format!("inspecting {}", dest.display()))?;

    let is_empty_dir = meta.is_dir()
        && std::fs::read_dir(dest)
            .with_context(|| format!("listing {}", dest.display()))?
            .next()
            .is_none();
    let is_owned = meta.is_dir() && dest.join(".git").join(OWNERSHIP_MARKER).is_file();

    if !(is_empty_dir || is_owned) {
        anyhow::bail!(
            "refusing to delete {}: it is not reusable ({state}) and was not created by the \
             scoreboard (no .git/{OWNERSHIP_MARKER} marker); remove it by hand or choose a \
             different corpus directory",
            dest.display()
        );
    }

    std::fs::remove_dir_all(dest).with_context(|| format!("removing {}", dest.display()))
}

/// Clone `url` with full history (`--no-checkout`) into `dest`, mark it as
/// owned, and check out `commit` detached — fetching the commit once
/// explicitly if the clone did not bring it in. Exactly one attempt; the
/// caller owns the "re-clone once" contract.
fn clone_pinned_history_once(url: &str, commit: &str, dest: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating parent dir for {}", dest.display()))?;
    }

    let mut clone_cmd = std::process::Command::new("git");
    clone_cmd
        .args(PINNED_CLONE_SECURITY_ARGS)
        .args(["clone", "--no-checkout", "--", url])
        .arg(dest);
    scrub_git_redirect_env(&mut clone_cmd);
    let clone_out = run_captured(
        clone_cmd,
        "git clone --no-checkout (pinned history)",
        PINNED_NETWORK_TIMEOUT_SECS,
    )?;
    if !clone_out.status.success() {
        anyhow::bail!(
            "git clone --no-checkout failed for {url}: {}",
            last_lines_lossy(&clone_out.stderr)
        );
    }

    // Absolute from here on, so every later command gets a discovery fence.
    let dest = std::fs::canonicalize(dest)
        .with_context(|| format!("canonicalizing fresh clone {}", dest.display()))?;
    let marker = dest.join(".git").join(OWNERSHIP_MARKER);
    std::fs::write(&marker, format!("{url}\n{commit}\n"))
        .with_context(|| format!("writing ownership marker {}", marker.display()))?;

    if checkout_detached(&dest, commit)? {
        return Ok(());
    }

    let mut fetch_args: Vec<&str> = PINNED_CLONE_SECURITY_ARGS.to_vec();
    fetch_args.extend(["fetch", "origin", commit]);
    let fetch_out = run_pinned_git_with_timeout(&dest, &fetch_args, PINNED_NETWORK_TIMEOUT_SECS)?;
    if !fetch_out.status.success() {
        anyhow::bail!(
            "commit {commit} is not reachable in {url}: git fetch origin {commit} failed: {}",
            last_lines_lossy(&fetch_out.stderr)
        );
    }

    if !checkout_detached(&dest, commit)? {
        anyhow::bail!(
            "git checkout --detach {commit} failed in {} after fetch",
            dest.display()
        );
    }
    Ok(())
}

/// `git checkout --detach <commit>` in `dest`. `Ok(false)` on a non-zero
/// exit (typically: commit not present yet).
fn checkout_detached(dest: &Path, commit: &str) -> anyhow::Result<bool> {
    let out = run_pinned_git(dest, &["checkout", "--quiet", "--detach", commit])?;
    Ok(out.status.success())
}

/// Run `git -C <dir> <args>` under [`PINNED_LOCAL_TIMEOUT_SECS`].
fn run_pinned_git(dir: &Path, args: &[&str]) -> anyhow::Result<std::process::Output> {
    run_pinned_git_with_timeout(dir, args, PINNED_LOCAL_TIMEOUT_SECS)
}

/// Run `git -C <dir> <args>` with stdout/stderr captured, the redirecting
/// `GIT_*` variables removed, and repository discovery fenced at `dir`'s
/// parent (`GIT_CEILING_DIRECTORIES`), so a `dir` that is not itself a
/// repository can never resolve to an enclosing one.
fn run_pinned_git_with_timeout(
    dir: &Path,
    args: &[&str],
    timeout_secs: u64,
) -> anyhow::Result<std::process::Output> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    scrub_git_redirect_env(&mut cmd);
    match dir.parent().filter(|p| p.is_absolute()) {
        Some(parent) => cmd.env("GIT_CEILING_DIRECTORIES", parent),
        None => cmd.env_remove("GIT_CEILING_DIRECTORIES"),
    };
    let label = format!("git {} (pinned history)", args.join(" "));
    run_captured(cmd, &label, timeout_secs)
}

/// Remove every [`GIT_REDIRECT_ENV_VARS`] entry from `cmd`'s environment.
fn scrub_git_redirect_env(cmd: &mut std::process::Command) {
    for var in GIT_REDIRECT_ENV_VARS {
        cmd.env_remove(var);
    }
}

/// Pipe stdout/stderr and run `cmd` through [`git_output_with_timeout`].
fn run_captured(
    mut cmd: std::process::Command,
    label: &str,
    timeout_secs: u64,
) -> anyhow::Result<std::process::Output> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    git_output_with_timeout(cmd, label, timeout_secs)
}

/// Trimmed stdout of a git command, decoded lossily.
fn stdout_trimmed(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Most lines of git stderr quoted by [`last_lines_lossy`].
const STDERR_TAIL_LINES: usize = 4;

/// The last [`STDERR_TAIL_LINES`] non-empty lines of a byte buffer, lossily
/// decoded and joined with ` | `. Used for clone and fetch failures, where
/// git prints progress (`Cloning into …`) first and the cause last.
fn last_lines_lossy(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return "(no output)".to_string();
    }
    let start = lines.len().saturating_sub(STDERR_TAIL_LINES);
    lines.get(start..).unwrap_or_default().join(" | ")
}

/// First non-empty line of a byte buffer (typically git's stderr), lossily
/// decoded, for compact error messages.
fn first_line_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("(no output)")
        .to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
    }

    fn dummy_repo() -> RepoEntry {
        crate::config::RepoEntry {
            url: "https://github.com/example/repo".to_string(),
            commit: "4649aa9700619f94cf9c66876e9549d83420e16c".to_string(),
            language: "Rust".to_string(),
            deep_clone: false,
        }
    }

    #[test]
    fn fixture_source_loads_rust_file() {
        let source = FixtureSource {
            fixture_dir: fixtures_dir(),
        };
        let files = source.fetch_files(&dummy_repo()).unwrap();
        let rust_file = files.iter().find(|f| {
            f.path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == "sample_rust.rs")
                .unwrap_or(false)
        });
        assert!(rust_file.is_some(), "should find sample_rust.rs");
        assert_eq!(rust_file.unwrap().language, Language::Rust);
    }

    #[test]
    fn binary_file_is_skipped() {
        let source = FixtureSource {
            fixture_dir: fixtures_dir(),
        };
        let files = source.fetch_files(&dummy_repo()).unwrap();
        let bin_file = files.iter().find(|f| {
            f.path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == "binary_file.bin")
                .unwrap_or(false)
        });
        // .bin has no target extension so it's excluded by extension filter
        assert!(bin_file.is_none(), "binary file should be skipped");
    }

    #[test]
    fn json_file_is_skipped() {
        let source = FixtureSource {
            fixture_dir: fixtures_dir(),
        };
        let files = source.fetch_files(&dummy_repo()).unwrap();
        let json_file = files.iter().find(|f| {
            f.path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e == "json")
                .unwrap_or(false)
        });
        assert!(json_file.is_none(), "json files should be excluded");
    }

    #[test]
    fn empty_file_is_included() {
        let source = FixtureSource {
            fixture_dir: fixtures_dir(),
        };
        let files = source.fetch_files(&dummy_repo()).unwrap();
        let empty = files.iter().find(|f| {
            f.path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n == "empty_file.rs")
                .unwrap_or(false)
        });
        assert!(empty.is_some(), "empty Rust file should be included");
        assert_eq!(empty.unwrap().content, "");
    }

    #[test]
    fn fixture_source_is_trait_object_compatible() {
        let source: Box<dyn FileSource> = Box::new(FixtureSource {
            fixture_dir: fixtures_dir(),
        });
        // Just verifying it compiles as a trait object.
        let _ = source.fetch_files(&dummy_repo());
    }

    // --- extract_repo_name validation tests ---

    #[test]
    fn extract_repo_name_normal_url() {
        assert_eq!(
            extract_repo_name("https://github.com/owner/myrepo.git").unwrap(),
            "myrepo"
        );
    }

    #[test]
    fn extract_repo_name_no_git_suffix() {
        assert_eq!(
            extract_repo_name("https://github.com/owner/myrepo").unwrap(),
            "myrepo"
        );
    }

    #[test]
    fn extract_repo_name_rejects_dot_dot() {
        assert!(
            extract_repo_name("https://github.com/owner/..").is_err(),
            "'..' should be rejected as path traversal"
        );
    }

    #[test]
    fn extract_repo_name_rejects_single_dot() {
        assert!(
            extract_repo_name("https://github.com/owner/.").is_err(),
            "'.' should be rejected as path traversal"
        );
    }

    #[test]
    fn extract_repo_name_rejects_slash_in_name() {
        // Constructed URL where last segment itself contains a slash-like char
        // after URL decoding — reject any embedded slash or backslash.
        assert!(
            extract_repo_name("https://github.com/owner/a/b").is_ok(),
            "'b' is the last segment and is safe"
        );
        // Backslash in the extracted name is the real concern.
        // Simulate by passing a raw string that yields a backslash via rsplit('/').
        assert!(
            extract_repo_name("https://github.com/owner/a\\b").is_err(),
            "backslash in repo name should be rejected"
        );
    }

    #[test]
    fn extract_repo_name_empty_url() {
        assert!(extract_repo_name("").is_err(), "empty URL should fail");
    }

    // --- walk_and_load with explicit extension list ---

    /// Verify that `walk_and_load(root, Some(&["rs", "md"]))` includes `.md`
    /// files even though they appear in `EXCLUDED_EXTENSIONS`.  This exercises
    /// the `Some(extensions)` branch and guards against regressions where the
    /// exclusion list is accidentally applied to caller-supplied extension lists.
    #[test]
    fn walk_and_load_explicit_extensions_includes_md() {
        let root = fixtures_dir();
        let files = walk_and_load(&root, Some(&["rs", "md"])).unwrap();

        // .md file must be present
        let has_md = files.iter().any(|f| {
            f.path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e == "md")
                .unwrap_or(false)
        });
        assert!(
            has_md,
            "walk_and_load with explicit exts should include .md files"
        );

        // .rs files must also be present
        let has_rs = files.iter().any(|f| {
            f.path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e == "rs")
                .unwrap_or(false)
        });
        assert!(
            has_rs,
            "walk_and_load with explicit exts should include .rs files"
        );

        // .ts files must not be included (not in the explicit list)
        let has_ts = files.iter().any(|f| {
            f.path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e == "ts")
                .unwrap_or(false)
        });
        assert!(
            !has_ts,
            "walk_and_load with explicit exts must not include .ts files"
        );
    }

    // --- AstGitCloneSource trait-object compatibility ---

    #[test]
    fn ast_git_clone_source_is_trait_object_compatible() {
        let _source: Box<dyn FileSource> = Box::new(AstGitCloneSource {
            corpus_dir: PathBuf::from("/tmp/corpus"),
        });
        // Verifying this compiles as a trait object is sufficient.
    }

    // ========================================================================
    // ensure_pinned_history_clone / verify_pinned_clone (#203)
    // ========================================================================

    use std::process::Command;

    /// A `git` command for building fixtures, isolated from the developer's
    /// global and system config so fixture commits are deterministic (no
    /// signing, hooks, or templates leak in).
    fn fixture_git(dir: &Path, home: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(dir)
            .env("HOME", home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("GIT_CONFIG_GLOBAL")
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
            .args(args);
        cmd
    }

    /// Run a fixture git command and return its trimmed stdout; panics (test
    /// failure) when git fails, so a broken fixture never passes silently.
    fn fixture_git_ok(dir: &Path, home: &Path, args: &[&str]) -> String {
        let out = fixture_git(dir, home, args)
            .output()
            .expect("git must be installed to run the pinned-clone tests");
        assert!(
            out.status.success(),
            "fixture git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A local fixture "remote": `first` and `second` on `main`, plus `off`, a
    /// commit reachable only from `refs/pinned/off` (not from any branch or
    /// tag), which a plain `git clone` does not bring in.
    struct FixtureRemote {
        home: tempfile::TempDir,
        repo: tempfile::TempDir,
        first: String,
        second: String,
        off: String,
    }

    impl FixtureRemote {
        fn new() -> Self {
            let home = tempfile::tempdir().unwrap();
            let repo = tempfile::tempdir().unwrap();
            let (h, r) = (home.path(), repo.path());
            fixture_git_ok(r, h, &["init", "--quiet"]);
            fixture_git_ok(
                r,
                h,
                &["config", "uploadpack.allowReachableSHA1InWant", "true"],
            );

            std::fs::write(r.join("a.txt"), "one\n").unwrap();
            fixture_git_ok(r, h, &["add", "a.txt"]);
            fixture_git_ok(r, h, &["commit", "--quiet", "-m", "first"]);
            let first = fixture_git_ok(r, h, &["rev-parse", "HEAD"]);

            std::fs::write(r.join("b.txt"), "two\n").unwrap();
            fixture_git_ok(r, h, &["add", "b.txt"]);
            fixture_git_ok(r, h, &["commit", "--quiet", "-m", "second"]);
            let second = fixture_git_ok(r, h, &["rev-parse", "HEAD"]);

            fixture_git_ok(r, h, &["checkout", "--quiet", "--detach"]);
            std::fs::write(r.join("c.txt"), "off-branch\n").unwrap();
            fixture_git_ok(r, h, &["add", "c.txt"]);
            fixture_git_ok(r, h, &["commit", "--quiet", "-m", "off"]);
            let off = fixture_git_ok(r, h, &["rev-parse", "HEAD"]);
            fixture_git_ok(r, h, &["update-ref", "refs/pinned/off", &off]);
            fixture_git_ok(r, h, &["checkout", "--quiet", "main"]);

            FixtureRemote {
                home,
                repo,
                first,
                second,
                off,
            }
        }

        fn url(&self) -> String {
            format!("file://{}", self.repo.path().display())
        }
    }

    /// Fresh destination path (not yet created) inside its own tempdir.
    fn fresh_dest() -> (tempfile::TempDir, PathBuf) {
        let parent = tempfile::tempdir().unwrap();
        let dest = parent.path().join("corpus");
        (parent, dest)
    }

    fn state(dest: &Path, commit: &str) -> PinnedCloneState {
        verify_pinned_clone(dest, commit).unwrap()
    }

    #[test]
    fn ensure_rejects_non_https_url() {
        let (_parent, dest) = fresh_dest();
        let err = ensure_pinned_history_clone("http://example.com/repo", &"a".repeat(40), &dest)
            .expect_err("http:// must be rejected");
        assert!(err.to_string().contains("https://"), "{err}");
        assert!(!dest.exists(), "a rejected url must not create dest");
    }

    #[test]
    fn ensure_rejects_malformed_commit_without_touching_dest() {
        let (_parent, dest) = fresh_dest();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("keep.txt"), "user data").unwrap();

        for bad in [
            "B8A0A79463382347820F1C2572BDE37B68E87C76", // uppercase: rev-parse prints lowercase
            "b8a0a79",                                  // abbreviated
            "--upload-pack=touch /tmp/pwned",           // option-shaped
            "HEAD",
        ] {
            let err = ensure_pinned_history_clone_from("file:///nonexistent", bad, &dest)
                .expect_err("malformed commit must be rejected");
            assert!(err.to_string().contains("40-character"), "{bad}: {err}");
        }
        assert!(
            dest.join("keep.txt").exists(),
            "dest must be left untouched"
        );
    }

    #[test]
    fn fresh_clone_is_full_history_detached_and_clean() {
        let remote = FixtureRemote::new();
        let (_parent, dest) = fresh_dest();

        ensure_pinned_history_clone_from(&remote.url(), &remote.second, &dest).unwrap();

        assert_eq!(state(&dest, &remote.second), PinnedCloneState::Reusable);
        let h = remote.home.path();
        assert_eq!(
            fixture_git_ok(&dest, h, &["rev-list", "--count", "HEAD"]),
            "2"
        );
        assert_eq!(
            fixture_git_ok(&dest, h, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "HEAD",
            "the pinned commit must be checked out detached"
        );
        assert!(dest.join("a.txt").is_file() && dest.join("b.txt").is_file());
    }

    #[test]
    fn second_call_reuses_the_clone_without_touching_the_remote() {
        let remote = FixtureRemote::new();
        let url = remote.url();
        let first = remote.first.clone();
        let (_parent, dest) = fresh_dest();
        ensure_pinned_history_clone_from(&url, &first, &dest).unwrap();

        // Invisible to `git status`; disappears if dest is deleted and re-cloned.
        let sentinel = dest.join(".git").join("reuse-sentinel");
        std::fs::write(&sentinel, "present").unwrap();
        // With the remote gone, any re-clone attempt would fail.
        drop(remote);

        ensure_pinned_history_clone_from(&url, &first, &dest)
            .expect("second call must reuse the clone, not contact the deleted remote");
        assert!(sentinel.exists(), "reuse must not delete or recreate dest");
    }

    #[test]
    fn verify_reports_missing_dest() {
        let (_parent, dest) = fresh_dest();
        assert_eq!(state(&dest, &"a".repeat(40)), PinnedCloneState::Missing);
    }

    #[test]
    fn verify_rejects_wrong_head() {
        let remote = FixtureRemote::new();
        let (_parent, dest) = fresh_dest();
        ensure_pinned_history_clone_from(&remote.url(), &remote.first, &dest).unwrap();

        assert_eq!(
            state(&dest, &remote.second),
            PinnedCloneState::HeadMismatch {
                expected: remote.second.clone(),
                actual: Some(remote.first.clone()),
            }
        );
    }

    #[test]
    fn verify_rejects_shallow_clone_at_the_right_commit() {
        let remote = FixtureRemote::new();
        let (_parent, dest) = fresh_dest();
        let dest_str = dest.to_str().unwrap();
        fixture_git_ok(
            remote.home.path(),
            remote.home.path(),
            &["clone", "--quiet", "--depth", "1", &remote.url(), dest_str],
        );

        // HEAD is `second`, so shallowness is the only failing condition.
        assert_eq!(state(&dest, &remote.second), PinnedCloneState::Shallow);
    }

    #[test]
    fn verify_rejects_untracked_file() {
        let remote = FixtureRemote::new();
        let (_parent, dest) = fresh_dest();
        ensure_pinned_history_clone_from(&remote.url(), &remote.second, &dest).unwrap();
        std::fs::write(dest.join("written-by-skim.txt"), "x").unwrap();

        assert!(matches!(
            state(&dest, &remote.second),
            PinnedCloneState::Dirty { sample } if sample.contains("written-by-skim.txt")
        ));
    }

    #[test]
    fn verify_rejects_modified_tracked_file() {
        let remote = FixtureRemote::new();
        let (_parent, dest) = fresh_dest();
        ensure_pinned_history_clone_from(&remote.url(), &remote.second, &dest).unwrap();
        std::fs::write(dest.join("a.txt"), "changed\n").unwrap();

        assert!(matches!(
            state(&dest, &remote.second),
            PinnedCloneState::Dirty { .. }
        ));
    }

    #[test]
    fn verify_rejects_ignored_file_a_plain_status_would_hide() {
        let remote = FixtureRemote::new();
        let (_parent, dest) = fresh_dest();
        ensure_pinned_history_clone_from(&remote.url(), &remote.second, &dest).unwrap();
        std::fs::write(dest.join(".git").join("info").join("exclude"), "*.log\n").unwrap();
        std::fs::write(dest.join("build.log"), "x").unwrap();

        assert!(matches!(
            state(&dest, &remote.second),
            PinnedCloneState::Dirty { sample } if sample.contains("build.log")
        ));
    }

    #[test]
    fn verify_rejects_empty_dir_nested_in_a_clean_repo_at_the_pinned_commit() {
        // Without a discovery fence, `git -C <empty dir>` walks up and reports
        // the ENCLOSING repo's HEAD — which here is the pinned commit, clean.
        let remote = FixtureRemote::new();
        let (_parent, outer) = fresh_dest();
        ensure_pinned_history_clone_from(&remote.url(), &remote.second, &outer).unwrap();
        let nested = outer.join("nested-corpus");
        std::fs::create_dir(&nested).unwrap();

        assert!(matches!(
            state(&nested, &remote.second),
            PinnedCloneState::NotRepositoryRoot { .. }
        ));
    }

    #[test]
    fn verify_rejects_a_regular_file_as_dest() {
        let (_parent, dest) = fresh_dest();
        std::fs::write(&dest, "not a directory").unwrap();
        assert!(matches!(
            state(&dest, &"a".repeat(40)),
            PinnedCloneState::NotRepositoryRoot { .. }
        ));
    }

    #[test]
    fn ensure_reclones_an_owned_clone_at_the_wrong_commit() {
        let remote = FixtureRemote::new();
        let (_parent, dest) = fresh_dest();
        ensure_pinned_history_clone_from(&remote.url(), &remote.first, &dest).unwrap();

        ensure_pinned_history_clone_from(&remote.url(), &remote.second, &dest).unwrap();

        assert_eq!(state(&dest, &remote.second), PinnedCloneState::Reusable);
    }

    #[test]
    fn ensure_reclones_an_owned_dirty_clone() {
        let remote = FixtureRemote::new();
        let (_parent, dest) = fresh_dest();
        ensure_pinned_history_clone_from(&remote.url(), &remote.second, &dest).unwrap();
        std::fs::write(dest.join("stray.txt"), "x").unwrap();

        ensure_pinned_history_clone_from(&remote.url(), &remote.second, &dest).unwrap();

        assert_eq!(state(&dest, &remote.second), PinnedCloneState::Reusable);
        assert!(!dest.join("stray.txt").exists());
    }

    #[test]
    fn ensure_replaces_an_owned_shallow_clone_with_full_history() {
        let remote = FixtureRemote::new();
        let (_parent, dest) = fresh_dest();
        let h = remote.home.path();
        fixture_git_ok(
            h,
            h,
            &[
                "clone",
                "--quiet",
                "--depth",
                "1",
                &remote.url(),
                dest.to_str().unwrap(),
            ],
        );
        // Simulate a clone this module created earlier.
        std::fs::write(dest.join(".git").join(OWNERSHIP_MARKER), "owned").unwrap();

        ensure_pinned_history_clone_from(&remote.url(), &remote.second, &dest).unwrap();

        assert_eq!(state(&dest, &remote.second), PinnedCloneState::Reusable);
        assert_eq!(
            fixture_git_ok(&dest, h, &["rev-list", "--count", "HEAD"]),
            "2"
        );
    }

    #[test]
    fn ensure_refuses_to_delete_a_directory_it_does_not_own() {
        let remote = FixtureRemote::new();
        let (_parent, dest) = fresh_dest();
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("precious.rs"), "fn main() {}\n").unwrap();

        let err = ensure_pinned_history_clone_from(&remote.url(), &remote.second, &dest)
            .expect_err("an unowned non-empty directory must not be deleted");

        assert!(err.to_string().contains("refusing to delete"), "{err}");
        assert!(dest.join("precious.rs").exists());
    }

    #[test]
    fn ensure_refuses_to_replace_an_unowned_checkout() {
        // e.g. a developer's own clone that a mistyped --corpus-dir points at.
        let remote = FixtureRemote::new();
        let (_parent, dest) = fresh_dest();
        let h = remote.home.path();
        fixture_git_ok(
            h,
            h,
            &["clone", "--quiet", &remote.url(), dest.to_str().unwrap()],
        );
        std::fs::write(dest.join("work-in-progress.rs"), "fn wip() {}\n").unwrap();

        assert!(ensure_pinned_history_clone_from(&remote.url(), &remote.second, &dest).is_err());
        assert!(dest.join("work-in-progress.rs").exists());
    }

    #[test]
    fn ensure_clones_into_an_existing_empty_directory() {
        let remote = FixtureRemote::new();
        let (_parent, dest) = fresh_dest();
        std::fs::create_dir_all(&dest).unwrap();

        ensure_pinned_history_clone_from(&remote.url(), &remote.first, &dest).unwrap();

        assert_eq!(state(&dest, &remote.first), PinnedCloneState::Reusable);
    }

    #[test]
    fn ensure_fetches_a_pinned_commit_that_no_branch_reaches() {
        let remote = FixtureRemote::new();
        let (_parent, dest) = fresh_dest();

        ensure_pinned_history_clone_from(&remote.url(), &remote.off, &dest).unwrap();

        assert_eq!(state(&dest, &remote.off), PinnedCloneState::Reusable);
        assert!(dest.join("c.txt").is_file());
    }

    #[test]
    fn ensure_fails_once_for_a_commit_the_remote_does_not_have() {
        let remote = FixtureRemote::new();
        let (_parent, dest) = fresh_dest();
        let absent = "0123456789abcdef0123456789abcdef01234567";

        let err = ensure_pinned_history_clone_from(&remote.url(), absent, &dest)
            .expect_err("an unknown commit must fail, not loop");

        assert!(err.to_string().contains("not reachable"), "{err}");
    }

    /// Lowercase hex SHA → raw 20 bytes (for hand-built tree objects).
    fn sha_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Write `bytes` as a tree object without git's own checks
    /// (`hash-object --literally`), returning its SHA.
    fn literal_tree(repo: &Path, home: &Path, bytes: &[u8]) -> String {
        let file = repo.join(".git").join("literal-tree");
        std::fs::write(&file, bytes).unwrap();
        let sha = fixture_git_ok(
            repo,
            home,
            &[
                "hash-object",
                "-t",
                "tree",
                "--literally",
                "-w",
                file.to_str().unwrap(),
            ],
        );
        std::fs::remove_file(&file).unwrap();
        sha
    }

    /// A fixture remote whose only commit (on `main`) has a hand-built root
    /// tree with one entry `<mode> <name>` pointing at a normal subtree that
    /// holds `f.txt`. Returns `(home, repo, commit)`.
    fn remote_with_root_entry(
        mode: &str,
        name: &str,
    ) -> (tempfile::TempDir, tempfile::TempDir, String) {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let (h, r) = (home.path(), repo.path());
        fixture_git_ok(r, h, &["init", "--quiet"]);

        let blob_file = r.join(".git").join("blob-src");
        std::fs::write(&blob_file, "zero padded history\n").unwrap();
        let blob = fixture_git_ok(r, h, &["hash-object", "-w", blob_file.to_str().unwrap()]);

        let mut sub = b"100644 f.txt\0".to_vec();
        sub.extend(sha_bytes(&blob));
        let subtree = literal_tree(r, h, &sub);

        let mut root = format!("{mode} {name}\0").into_bytes();
        root.extend(sha_bytes(&subtree));
        let root_tree = literal_tree(r, h, &root);

        let commit = fixture_git_ok(r, h, &["commit-tree", &root_tree, "-m", "legacy tree"]);
        fixture_git_ok(r, h, &["update-ref", "refs/heads/main", &commit]);
        (home, repo, commit)
    }

    #[test]
    fn ensure_clones_a_history_with_zero_padded_file_modes() {
        // pallets/flask's history carries trees written with `040000`
        // directory modes. Strict transfer fsck rejects them unless that one
        // benign message id is downgraded.
        let (_home, repo, commit) = remote_with_root_entry("040000", "sub");
        let url = format!("file://{}", repo.path().display());
        let (_parent, dest) = fresh_dest();

        ensure_pinned_history_clone_from(&url, &commit, &dest).unwrap();

        assert_eq!(state(&dest, &commit), PinnedCloneState::Reusable);
        assert!(dest.join("sub").join("f.txt").is_file());
    }

    #[test]
    fn ensure_keeps_every_other_fsck_check_and_reports_gits_own_error() {
        // A tree entry named `.git` (hasDotgit) must still abort the clone,
        // and the error must carry git's fsck message, not the leading
        // "Cloning into ..." progress line.
        let (_home, repo, commit) = remote_with_root_entry("40000", ".git");
        let url = format!("file://{}", repo.path().display());
        let (_parent, dest) = fresh_dest();

        let err = ensure_pinned_history_clone_from(&url, &commit, &dest)
            .expect_err("a .git tree entry must fail transfer fsck");

        let msg = format!("{err:#}");
        assert!(msg.contains("hasDotgit"), "{msg}");
    }
}
