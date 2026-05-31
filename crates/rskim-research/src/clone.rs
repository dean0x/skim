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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

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
}
