//! Removal of the search git hooks that older versions of skim wrote.
//!
//! Skim 2.x installed a marker-delimited block into three git hook scripts so
//! the search index refreshed after commits, merges and checkouts. There is no
//! index any more, and the block now makes a hook run a command that exits 2.
//! `skim init` and `skim init --uninstall` remove the block with this module.
//!
//! # Old hook block format
//!
//! ```sh
//! # skim-search-start
//! skim search --update 2>/dev/null &
//! # skim-search-end
//! ```
//!
//! # What removal touches
//!
//! Removal deletes the lines from the start marker to the end marker, and no
//! other byte of the hook. A marker only counts when it is a whole line. A hook
//! that is a link, is not UTF-8 text or is larger than [`MAX_HOOK_BYTES`] is
//! not skim's and stays as it is. The rewrite keeps the file permissions, so a
//! hook that was not executable does not start to run.
//!
//! The three hooks are independent. A hook that cannot be read or rewritten (for
//! example one that another user owns, with mode 600) is reported as a
//! [`HookFailure`] and the other hooks are still handled.

use std::fmt;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

// ============================================================================
// Constants
// ============================================================================

const MARKER_START: &str = "# skim-search-start";
const MARKER_END: &str = "# skim-search-end";

/// Largest hook file removal reads. The old block sat in small shell scripts; a
/// bigger file is not one that skim wrote.
const MAX_HOOK_BYTES: u64 = 1 << 20;

/// User-facing notice emitted when the resolved hooks directory is the shared
/// `<commondir>/hooks` (every worktree of the clone shares it).
pub(super) const SHARED_HOOKS_SCOPE_MSG: &str =
    "skim search: this hooks directory is shared by every worktree of this clone";

/// The hook files the old block was written into.
const HOOK_NAMES: &[&str] = &["post-commit", "post-merge", "post-checkout"];

// ============================================================================
// Outcome type
// ============================================================================

/// Outcome of [`remove_search_hooks`].
///
/// The shared-scope notice (AC34(b)) is emitted by the operation itself;
/// callers receive `dir` for their own path-disclosure messages, `changed`
/// for suppression of empty-operation output and `failures` for the hooks that
/// were left as they were.
#[derive(Debug)]
pub(crate) struct HooksOutcome {
    /// The resolved hooks directory, in canonical form when it exists.
    pub dir: PathBuf,
    /// `true` when at least one old block was found and removed. `false` means
    /// no hook held a block.
    pub changed: bool,
    /// The hooks that could not be read or rewritten, at most one for each name
    /// in [`HOOK_NAMES`]. A hook in this list is unchanged.
    pub failures: Vec<HookFailure>,
}

/// What [`find_search_hooks`] found in the hooks directory.
///
/// It exists only when there is something to report: a block, a hook that could
/// not be read, or both.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HooksFinding {
    /// The resolved hooks directory, in canonical form when it exists.
    pub dir: PathBuf,
    /// `true` when a readable hook holds a complete old block.
    pub block_found: bool,
    /// The hooks that could not be read. Whether they hold a block is unknown.
    pub unreadable: Vec<HookFailure>,
}

/// What skim did to a hook when it failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookStep {
    /// Look at the file (`symlink_metadata`).
    Inspect,
    /// Read the text of the file.
    Read,
    /// Write the file again without the block.
    Rewrite,
}

impl HookStep {
    fn verb(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::Read => "read",
            Self::Rewrite => "rewrite",
        }
    }
}

/// A hook that skim could not read or rewrite.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct HookFailure {
    /// The hook file.
    pub path: PathBuf,
    /// What skim was doing.
    pub step: HookStep,
    /// Why the system call failed.
    pub kind: std::io::ErrorKind,
}

impl HookFailure {
    fn new(path: &Path, step: HookStep, error: &std::io::Error) -> Self {
        Self {
            path: path.to_path_buf(),
            step,
            kind: error.kind(),
        }
    }
}

impl fmt::Display for HookFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `{:?}` quotes ESC/CR/LF that a repository-controlled path can hold.
        write!(
            f,
            "could not {} hook {:?} ({})",
            self.step.verb(),
            self.path,
            self.kind
        )
    }
}

// ============================================================================
// Hooks directory resolver
// ============================================================================

/// Resolve the hooks directory for the given project root.
///
/// Returns `Err(ancestor)` when `project_root` has no `.git` entry of its own
/// but IS inside an enclosing git repository (i.e.
/// [`gitdir::resolve_repo_toplevel`] finds an ancestor).  The root is not a
/// place that hosts hooks, and the caller names `ancestor` instead.
///
/// For a linked worktree, routes to the shared `<commondir>/hooks` directory
/// so `remove_search_hooks` and `find_search_hooks` can never disagree about
/// where the hooks live (they both call this function).
///
/// For a plain repo (`.git` is a directory) or a **true** non-repo root (no
/// enclosing repository either), returns `Ok(<root>/.git/hooks)`.  The
/// true-non-repo case covers the bare temp directories `hooks_tests.rs`
/// creates.
///
/// A submodule's `.git` is a *file* whose gitdir is `<super>/.git/modules/<name>`.
/// This resolver returns `<super>/.git/modules/<name>/hooks`, which is what
/// `git -C <sub> rev-parse --git-path hooks` reports (the submodule gitdir is a
/// complete ref store with no `commondir`, so no redirection happens; only the
/// `.git`-file indirection is followed).
///
/// **Write-path security (AD-413-3 extension):** when `.git` is a FILE, the
/// `gitdir:` pointer is untrusted, repository-controlled input (SEARCH-ADR-008).
/// The resolver applies a two-stage gate before using any derived path as a
/// write destination:
///
/// 1. [`gitdir::resolve_common_dir`] is tried first — it is already validated
///    by the AD-413-3 sanity gate (`HEAD` is_file + is_dir).  When it resolves,
///    the result is used directly (this is the normal linked-worktree path).
/// 2. If the commondir is absent (submodule, or unusual gitdir), the gitdir
///    itself must pass [`looks_like_git_dir`] (`HEAD` is_file plus `objects/`
///    and `refs/` subdirectories) before skim writes into it.  A malicious
///    pointer to an arbitrary directory — e.g. `~/Library/LaunchAgents` — has
///    no `objects/` or `refs/`, so it fails the gate and skim falls back to the
///    safe local `<root>/.git/hooks`.
///
/// The discovered enclosing ancestor is returned alongside the refusal, so the
/// caller does not walk again to name it.
///
/// - `Ok(dir)` — a valid hooks directory; caller may use it.
/// - `Err(ancestor)` — `project_root` is a subdirectory of `ancestor`;
///   caller must refuse with an error naming `ancestor`.
pub(crate) fn resolve_hooks_dir(project_root: &Path) -> Result<PathBuf, PathBuf> {
    match super::gitdir::resolve_git_dir(project_root) {
        Some(git_dir) => {
            let dot_git = project_root.join(".git");
            if dot_git.is_file() {
                // `.git` is a FILE — the resolved gitdir is untrusted,
                // repository-controlled input.  Apply the AD-413-3 write-path
                // sanity gate in two stages before using any derived path as a
                // write destination.
                //
                // Stage 1: try the commondir.  `resolve_common_dir` validates
                // it (is_dir + HEAD is_file — the existing AD-413-3 gate).
                // A real linked worktree always has a commondir, so this is the
                // expected fast path.
                if let Some(common) = super::gitdir::resolve_common_dir(&git_dir) {
                    return Ok(common.join("hooks"));
                }
                // Stage 2: no commondir (submodule gitdir, or an unusual
                // configuration).  The gitdir itself must look like a complete
                // git directory (HEAD + objects/ + refs/) before skim writes
                // into it.  A linked-worktree per-worktree gitdir does NOT have
                // objects/ or refs/ (those live in the primary .git/), so it
                // correctly fails this check — but it always resolves via the
                // commondir in Stage 1 above.
                if !looks_like_git_dir(&git_dir) {
                    if crate::debug::is_debug_enabled() {
                        eprintln!(
                            "skim search [debug]: gitdir {git_dir:?} failed the AD-413-3 \
                             write-path sanity gate (HEAD + objects/ + refs/ required); \
                             falling back to local hooks path"
                        );
                    }
                    // Safe fallback: contained inside the project root.
                    return Ok(project_root.join(".git").join("hooks"));
                }
                Ok(git_dir.join("hooks"))
            } else {
                // `.git` is a directory — already known to exist on disk and is
                // therefore a trustworthy base path.  Follow the commondir if
                // present (plain repos typically have none), otherwise use
                // git_dir directly.
                Ok(super::gitdir::resolve_common_dir(&git_dir)
                    .unwrap_or(git_dir)
                    .join("hooks"))
            }
        }
        None => {
            // No `.git` at this root.  Check whether the root is a subdirectory
            // of an enclosing git repository.
            //
            // If it IS inside a repo, return Err(ancestor).  The caller refuses
            // and names the enclosing repository.
            //
            // If it is NOT inside any repo (true non-repo, e.g. the bare temp
            // dirs that hooks_tests creates), return the local fallback.
            if let Some(ancestor) = super::gitdir::resolve_repo_toplevel(project_root) {
                return Err(ancestor);
            }
            Ok(project_root.join(".git").join("hooks"))
        }
    }
}

/// Return `true` if `path` looks like a complete git directory.
///
/// Requires all three:
/// - `path` itself is a directory,
/// - `path/HEAD` is a file,
/// - `path/objects/` is a directory,
/// - `path/refs/` is a directory.
///
/// Used as the AD-413-3 write-path sanity gate for gitdir pointers that
/// arrived via an untrusted `gitdir:` file.  A linked-worktree per-worktree
/// gitdir (`<primary>/.git/worktrees/<name>/`) has `HEAD` but NOT `objects/`
/// or `refs/` (those belong to the primary repo), so it correctly FAILS this
/// check — in that case skim always reaches the write destination through
/// `commondir` (Stage 1 of `resolve_hooks_dir`), never through this gate.
fn looks_like_git_dir(path: &Path) -> bool {
    path.is_dir()
        && path.join("HEAD").is_file()
        && path.join("objects").is_dir()
        && path.join("refs").is_dir()
}

/// Return `true` when a `commondir` redirect is in effect for `project_root`,
/// meaning every linked worktree of this clone shares the same hooks directory.
///
/// Uses the same `resolve_git_dir` → `resolve_common_dir` resolution chain that
/// [`resolve_hooks_dir`]'s Stage 1 follows, so the predicate stays in sync with
/// the actual write-path logic.
///
/// A plain repository (no `commondir` file) returns `false` — it has exactly one
/// worktree and nothing to share.  A submodule (`.git` is a file but the
/// referenced gitdir has no `commondir`) also returns `false` — the submodule's
/// gitdir is a private, self-contained ref store, not a shared anchor.
///
/// The previous path-inequality approach (`hooks_dir != root.join(".git/hooks")`)
/// was incorrect for submodules: `resolve_hooks_dir` reaches
/// `<super>/.git/modules/<name>/hooks` via the `.git`-file pointer (Stage 2 of
/// the write-path gate), which differs from the hand-built plain path even though
/// no clone-wide sharing is occurring.
fn is_shared_hooks_dir(project_root: &Path) -> bool {
    super::gitdir::resolve_git_dir(project_root)
        .and_then(|d| super::gitdir::resolve_common_dir(&d))
        .is_some()
}

// ============================================================================
// Public API
// ============================================================================

/// Remove the old skim marker block from the search hooks of `project_root`.
///
/// For each of `post-commit`, `post-merge` and `post-checkout`, deletes the lines
/// from `# skim-search-start` to `# skim-search-end`. Every other byte of the
/// hook stays as it is. A missing hook, a hook without a complete block and a
/// hook that is not skim's to rewrite (see the module doc) are skipped.
///
/// # Shared-scope disclosure (AC34(b))
///
/// When the resolved hooks directory is the shared `<commondir>/hooks` AND at
/// least one block was removed, this function emits [`SHARED_HOOKS_SCOPE_MSG`]
/// to stderr, so every caller inherits the notice.
///
/// A hook that cannot be read or rewritten does not stop the others: it is listed
/// in [`HooksOutcome::failures`] and stays as it is.
///
/// # Errors
///
/// Returns `Err` when `project_root` is a subdirectory of a git repository and
/// holds no hooks of its own.
pub(crate) fn remove_search_hooks(project_root: &Path) -> anyhow::Result<HooksOutcome> {
    let hooks_dir = match resolve_hooks_dir(project_root) {
        Ok(d) => d,
        Err(ancestor) => {
            // {:?} quotes ESC/CR/LF that a repository-controlled path can hold.
            anyhow::bail!(
                "{:?} is a subdirectory of the git repository {:?} and holds no git hooks of its own",
                project_root,
                ancestor,
            );
        }
    };
    let removal = remove_from_dir(&hooks_dir, &read_hook);
    if removal.changed && is_shared_hooks_dir(project_root) {
        eprintln!("{SHARED_HOOKS_SCOPE_MSG}");
    }
    Ok(HooksOutcome {
        dir: canonical_if_exists(hooks_dir),
        changed: removal.changed,
        failures: removal.failures,
    })
}

/// What the hooks of `project_root` hold: the hooks directory and what is wrong
/// in it, or `None` when no hook there holds a complete old block and every hook
/// could be read. It writes nothing.
///
/// Uses the same directory, the same hook list and the same block rule as
/// [`remove_search_hooks`], so a block found here is a block that removal takes
/// out. A hook that cannot be read is listed in [`HooksFinding::unreadable`]: it
/// is unknown, not clean. A `project_root` that is a subdirectory of a git
/// repository holds no hooks, so it gives `None`.
pub(crate) fn find_search_hooks(project_root: &Path) -> Option<HooksFinding> {
    let hooks_dir = resolve_hooks_dir(project_root).ok()?;
    let scan = scan_dir(&hooks_dir, &read_hook);
    (scan.block_found || !scan.unreadable.is_empty()).then(|| HooksFinding {
        dir: canonical_if_exists(hooks_dir),
        block_found: scan.block_found,
        unreadable: scan.unreadable,
    })
}

// ============================================================================
// Private helpers
// ============================================================================

/// A hook file that skim may rewrite: its text and its permissions.
struct Hook {
    text: String,
    permissions: std::fs::Permissions,
}

/// How a hook file is read. The tests replace it with a reader that fails.
type ReadHook<'a> = &'a dyn Fn(&Path) -> Result<Option<Hook>, HookFailure>;

/// What one pass of [`remove_from_dir`] did.
struct Removal {
    changed: bool,
    failures: Vec<HookFailure>,
}

/// What one pass of [`scan_dir`] saw.
struct Scan {
    block_found: bool,
    unreadable: Vec<HookFailure>,
}

/// Remove the old block from each hook of `hooks_dir`. A hook that fails is
/// listed and the next hook is still handled.
fn remove_from_dir(hooks_dir: &Path, read: ReadHook<'_>) -> Removal {
    let mut removal = Removal {
        changed: false,
        failures: Vec::new(),
    };
    for name in HOOK_NAMES {
        match remove_from_hook(&hooks_dir.join(name), read) {
            Ok(removed) => removal.changed |= removed,
            Err(failure) => removal.failures.push(failure),
        }
    }
    removal
}

/// Look for the old block in each hook of `hooks_dir`, and write nothing. A hook
/// that cannot be read is listed and the next hook is still read.
fn scan_dir(hooks_dir: &Path, read: ReadHook<'_>) -> Scan {
    let mut scan = Scan {
        block_found: false,
        unreadable: Vec::new(),
    };
    for name in HOOK_NAMES {
        match read(&hooks_dir.join(name)) {
            Ok(Some(hook)) => scan.block_found |= strip_block(&hook.text) != hook.text,
            Ok(None) => {}
            Err(failure) => scan.unreadable.push(failure),
        }
    }
    scan
}

/// Read a hook file, or `Ok(None)` when it is absent or is not skim's to rewrite.
///
/// Skim only rewrites a regular file of at most [`MAX_HOOK_BYTES`] that is UTF-8
/// text. A link is not followed: the rewrite replaces the file at `path`, and
/// that would turn the link into a copy. The read is bounded by the cap, not by
/// the size the file had when it was inspected.
fn read_hook(path: &Path) -> Result<Option<Hook>, HookFailure> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        // A `.git` that is a file (a gitdir that failed the write-path gate) makes
        // the hooks path unreachable: there is no hook there.
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(HookFailure::new(path, HookStep::Inspect, &e)),
    };
    if !meta.is_file() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_HOOK_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| HookFailure::new(path, HookStep::Read, &e))?;
    if bytes.len() as u64 > MAX_HOOK_BYTES {
        return Ok(None);
    }
    Ok(String::from_utf8(bytes).ok().map(|text| Hook {
        text,
        permissions: meta.permissions(),
    }))
}

/// Strip the old block from one hook file.
///
/// Returns `true` if a block was removed, `false` if the hook was left as it is.
fn remove_from_hook(hook_path: &Path, read: ReadHook<'_>) -> Result<bool, HookFailure> {
    let Some(hook) = read(hook_path)? else {
        return Ok(false);
    };
    let stripped = strip_block(&hook.text);
    if stripped == hook.text {
        return Ok(false);
    }
    write_hook_atomic(hook_path, &stripped, hook.permissions)
        .map_err(|e| HookFailure::new(hook_path, HookStep::Rewrite, &e))?;
    Ok(true)
}

/// Delete every complete old block from `content`, line by line.
///
/// A block starts at a line that is exactly the start marker and ends at the next
/// line that is exactly the end marker. Both marker lines and all lines between
/// them go; every other byte stays. A marker inside a longer line is text of
/// another script, and a start marker with no end marker is not a block, so both
/// stay.
fn strip_block(content: &str) -> String {
    let mut kept = String::with_capacity(content.len());
    // The lines of a block that has started and not yet ended.
    let mut open_block = String::new();
    let mut in_block = false;
    for line in content.split_inclusive('\n') {
        let text = line.trim_end_matches(['\r', '\n']);
        if in_block {
            if text == MARKER_END {
                in_block = false;
                open_block.clear();
            } else {
                open_block.push_str(line);
            }
        } else if text == MARKER_START {
            in_block = true;
            open_block.push_str(line);
        } else {
            kept.push_str(line);
        }
    }
    // A block with no end marker is not ours: keep all of its lines.
    kept.push_str(&open_block);
    kept
}

/// The canonical form of `dir`, or `dir` itself when it does not exist.
fn canonical_if_exists(dir: PathBuf) -> PathBuf {
    dir.canonicalize().unwrap_or(dir)
}

/// Atomically write `content` to `hook_path` via an unpredictably-named temp
/// file in the same directory (so the rename is always on the same filesystem).
///
/// Using `NamedTempFile::new_in` instead of a fixed `.tmp` suffix avoids
/// a symlink/TOCTOU attack where an adversary pre-creates a predictable path
/// and redirects the write to an arbitrary target.
///
/// The file gets `permissions`, the ones the hook had. A hook that was not
/// executable stays one that git does not run.
///
/// The data reaches the disk before the rename, so a power loss cannot leave the
/// user's own hook lines as an empty file.
fn write_hook_atomic(
    hook_path: &Path,
    content: &str,
    permissions: std::fs::Permissions,
) -> std::io::Result<()> {
    let parent = hook_path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "hook path has no parent directory",
        )
    })?;

    let mut tmp = NamedTempFile::new_in(parent)?;

    use std::io::Write as _;
    tmp.write_all(content.as_bytes())?;
    std::fs::set_permissions(tmp.path(), permissions)?;
    tmp.as_file().sync_all()?;

    tmp.persist(hook_path).map_err(|e| e.error)?;

    Ok(())
}

// ============================================================================
// Tests (co-located in hooks_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
