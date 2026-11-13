//! Project-root discovery and git directory lookup.
//!
//! Pure file I/O — no git binary subprocess, no libgit2 dependency.
//! Handles ordinary repos (`.git/` directory), linked worktrees (`.git` file),
//! and subdirectory roots that adopt an enclosing repository.
//!
//! All reads are bounded and all failures are soft: a git directory that cannot
//! be read is reported as `None`.
//!
//! `resolve_git_dir` resolves ONE directory and never walks up. Ancestor
//! discovery lives in [`walk_up`], the bounded ancestor scan that finds the
//! root of a query. [`find_repository`] builds on it for a root with NO `.git`
//! at all: it adopts the nearest enclosing repository, so `--root <subdir>` and
//! a bare invocation from that subdirectory agree.
//!
//! An ancestor `.git` is adopted only when the current user owns it, as git
//! does by default. The start folder is never checked: the user chose it.

use std::path::{Path, PathBuf};

// ============================================================================
// Byte caps for untrusted file reads
// ============================================================================

/// Maximum bytes read from the `commondir` file inside a linked-worktree gitdir.
/// Generous cap — a path is never longer than this.
const MAX_COMMONDIR_BYTES: u64 = 4096;

/// Maximum bytes read from the `.git` file pointer.
/// The pointer is "gitdir: <absolute-path>\n" — 512 bytes is generous.
const MAX_REF_BYTES: u64 = 512;

// ============================================================================
// Git directory resolution
// ============================================================================

/// Maximum number of ancestors to traverse when looking for a `.git` root.
/// 256 ancestors is far beyond any real filesystem depth.
pub(super) const MAX_ANCESTORS: usize = 256;

/// Most entries of `GIT_CEILING_DIRECTORIES` that the root walk reads.
pub(super) const MAX_CEILING_ENTRIES: usize = 64;

/// Whether the current user owns a `.git` entry. It is a plain function so a
/// test can put a stand-in in [`RootRules::owns`]; a test cannot change the
/// owner of a file.
pub(super) type OwnerCheck = fn(&Path) -> bool;

/// The folders where the ancestor walk for a repository root must stop, and
/// the owner rule for the `.git` entry it finds.
///
/// The walk never steps into a ceiling folder, never reaches the home folder
/// from below, and never reaches the filesystem root from below. A start folder
/// is never excluded: it can be a ceiling, the home folder or `/` itself.
#[derive(Debug, Clone)]
pub(super) struct RootRules {
    pub(super) ceilings: Vec<PathBuf>,
    pub(super) home: Option<PathBuf>,
    /// How many absolute entries of `GIT_CEILING_DIRECTORIES` lie past the
    /// first [`MAX_CEILING_ENTRIES`]. Those entries are not read, so when this
    /// is not zero the walk does not go up at all: it cannot pass a ceiling it
    /// did not read.
    pub(super) ceilings_dropped: usize,
    /// Whether the current user owns the `.git` entry at a path.
    pub(super) owns: OwnerCheck,
}

impl Default for RootRules {
    fn default() -> Self {
        RootRules {
            ceilings: Vec::new(),
            home: None,
            ceilings_dropped: 0,
            owns: owned_by_current_user,
        }
    }
}

impl RootRules {
    /// The rules of this process: `GIT_CEILING_DIRECTORIES` and the home
    /// folder.
    pub(super) fn from_env() -> Self {
        let (ceilings, ceilings_dropped) = std::env::var_os("GIT_CEILING_DIRECTORIES")
            .map(|value| (parse_ceilings(&value), dropped_ceilings(&value)))
            .unwrap_or_default();
        let home = dirs::home_dir().and_then(|home| home.canonicalize().ok());
        RootRules {
            ceilings,
            home,
            ceilings_dropped,
            ..RootRules::default()
        }
    }

    /// Whether the walk ends before `candidate`, an ancestor of the start.
    fn stops_at(&self, candidate: &Path) -> bool {
        self.ceilings_dropped > 0
            || candidate.parent().is_none()
            || self.home.as_deref() == Some(candidate)
            || self.ceilings.iter().any(|c| c == candidate)
    }
}

/// Whether the current user owns the `.git` entry at `dot_git`, the rule git
/// applies by default. An entry that cannot be examined is not owned.
fn owned_by_current_user(dot_git: &Path) -> bool {
    gix::sec::identity::is_path_owned_by_current_user(dot_git).unwrap_or(false)
}

/// The usable entries of a `GIT_CEILING_DIRECTORIES` value: the first
/// [`MAX_CEILING_ENTRIES`] absolute paths. An entry is kept as written, and its
/// canonical form is added when that differs, because the walk compares
/// canonical paths.
fn parse_ceilings(value: &std::ffi::OsStr) -> Vec<PathBuf> {
    let mut ceilings = Vec::new();
    for entry in std::env::split_paths(value)
        .filter(|p| p.is_absolute())
        .take(MAX_CEILING_ENTRIES)
    {
        if let Ok(canonical) = entry.canonicalize()
            && canonical != entry
        {
            ceilings.push(canonical);
        }
        ceilings.push(entry);
    }
    ceilings
}

/// How many absolute entries of a `GIT_CEILING_DIRECTORIES` value lie past the
/// ones [`parse_ceilings`] reads. The count is exact; its only bound is the
/// length of the value, which the operating system limits.
fn dropped_ceilings(value: &std::ffi::OsStr) -> usize {
    std::env::split_paths(value)
        .filter(|p| p.is_absolute())
        .skip(MAX_CEILING_ENTRIES)
        .count()
}

/// Where the walk up from a start folder ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Walk {
    /// The folder with the nearest `.git` entry: the start folder, or an
    /// ancestor that the current user owns.
    Found(PathBuf),
    /// No `.git` entry within the [`MAX_ANCESTORS`] bound, or a ceiling, the
    /// home folder or `/` ended the walk first.
    NotFound,
    /// The nearest ancestor with a `.git` entry holds one that another user
    /// owns. The walk stopped below this folder.
    NotOwned(PathBuf),
}

impl Walk {
    /// The folder a query takes as its root when the walk started at `start`:
    /// the repository folder, else `start` itself.
    pub(super) fn root(&self, start: &Path) -> PathBuf {
        match self {
            Walk::Found(top) => top.clone(),
            Walk::NotFound | Walk::NotOwned(_) => start.to_path_buf(),
        }
    }
}

/// Walk up from `canonical` (a pre-canonicalized path) looking for the nearest
/// ancestor that contains a `.git` entry, under `rules`.
///
/// An ancestor `.git` that `rules.owns` does not accept ends the walk with
/// [`Walk::NotOwned`]: git refuses such a repository by default, and the walk
/// must not adopt a tree that the user did not choose. The `.git` entry of the
/// start folder itself is not checked.
///
/// The caller is responsible for canonicalizing `canonical` first, to avoid a
/// redundant O(path depth) syscall chain when the caller already holds a
/// canonical path.
pub(super) fn walk_up(canonical: &Path, rules: &RootRules) -> Walk {
    let mut current = canonical;
    for _ in 0..MAX_ANCESTORS {
        if current != canonical && rules.stops_at(current) {
            break;
        }
        let dot_git = current.join(".git");
        if dot_git.exists() {
            if current != canonical && !(rules.owns)(&dot_git) {
                return Walk::NotOwned(current.to_path_buf());
            }
            return Walk::Found(current.to_path_buf());
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    Walk::NotFound
}

/// Resolve the git directory for a project root.
///
/// - If `.git` is a **directory**, returns it directly.
/// - If `.git` is a **file** (worktree), parses the `gitdir: <path>` pointer
///   and returns the resolved target path.
/// - Returns `None` when `.git` doesn't exist.
///
/// This mirrors git's own resolution logic for `git rev-parse --git-dir`.
/// A BARE repo has no `.git` entry, so this returns `None` for it.
pub(super) fn resolve_git_dir(project_root: &Path) -> Option<PathBuf> {
    use std::io::Read as _;

    let dot_git = project_root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    if dot_git.is_file() {
        // Worktree: .git is a file containing "gitdir: <absolute-or-relative-path>"
        // Bounded read: AD-412-4 — the .git pointer file is repository-controlled.
        let mut content = String::new();
        std::fs::File::open(&dot_git)
            .ok()?
            .take(MAX_REF_BYTES)
            .read_to_string(&mut content)
            .ok()?;
        let gitdir_line = content.lines().find(|l| l.starts_with("gitdir:"))?;
        let target = gitdir_line.strip_prefix("gitdir:").map(str::trim)?;
        let target_path = PathBuf::from(target);
        if target_path.is_absolute() {
            Some(target_path)
        } else {
            // Relative to the directory containing the .git file
            Some(project_root.join(target_path))
        }
    } else {
        None
    }
}

/// The repository that holds a root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Repository {
    /// The top folder of the repository; the root itself when it has a `.git`
    /// entry.
    pub(super) top: PathBuf,
    /// The root relative to `top`; empty when the root is `top`.
    pub(super) prefix: PathBuf,
}

/// What the lookup of the repository that holds a root found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RepoLookup {
    /// The root is in this repository.
    Within(Repository),
    /// No repository the search may use holds the root: there is none within
    /// the walk bounds, or the walk stopped (a ceiling, the home folder, `/`,
    /// or a `.git` entry that another user owns). The file walk is then the
    /// whole universe.
    Outside,
    /// A repository holds the root, but the search cannot use it. The text
    /// says why. There is no partial universe for it: tracked files would
    /// leave the answer with no count.
    Unresolved(String),
}

/// Find the repository that holds `root` (a canonical path): `root` itself when
/// it has a `.git` entry, else the nearest enclosing repository under `rules`.
///
/// A repository is `Unresolved` when `root` holds a `.git` entry that cannot be
/// examined, or when the enclosing repository found has no readable git
/// directory or `HEAD` file. Only "no repository" gives `Outside`.
pub(super) fn find_repository(root: &Path, rules: &RootRules) -> RepoLookup {
    // `try_exists` tells "absent" from "exists but unreadable": a permission
    // error on `.git` must not trigger the adoption of an enclosing repository.
    match root.join(".git").try_exists() {
        Ok(true) => {
            return RepoLookup::Within(Repository {
                top: root.to_path_buf(),
                prefix: PathBuf::new(),
            });
        }
        Ok(false) => {}
        Err(e) => {
            return RepoLookup::Unresolved(format!(
                "cannot look for a .git entry in {root:?}: {e}"
            ));
        }
    }
    let top = match walk_up(root, rules) {
        Walk::Found(top) => top,
        Walk::NotFound | Walk::NotOwned(_) => return RepoLookup::Outside,
    };
    // The top folder must contain `root`, and have a git dir with a readable
    // HEAD file.
    let Ok(prefix) = root.strip_prefix(&top) else {
        return RepoLookup::Unresolved(format!("{root:?} is not inside {top:?}"));
    };
    let prefix = prefix.to_path_buf();
    let Some(git_dir) = resolve_git_dir(&top) else {
        return RepoLookup::Unresolved(format!("the .git entry of {top:?} cannot be read"));
    };
    if !git_dir.join("HEAD").is_file() {
        return RepoLookup::Unresolved(format!(
            "the git directory {git_dir:?} has no readable HEAD file"
        ));
    }
    RepoLookup::Within(Repository { top, prefix })
}

/// The nearest enclosing git repository of `project_root`, when
/// `project_root` itself has no `.git` entry of its own.
///
/// Returns `None` when `project_root` already has a `.git` entry (never
/// re-point it), when no usable enclosing repository is found, and for every
/// failure of the lookup: this serves the removal of old hooks, which takes no
/// action on a repository it cannot read.
pub(super) fn resolve_repo_toplevel(project_root: &Path) -> Option<PathBuf> {
    if project_root.join(".git").try_exists().unwrap_or(true) {
        return None;
    }
    let canonical = project_root.canonicalize().ok()?;
    match find_repository(&canonical, &RootRules::from_env()) {
        RepoLookup::Within(repository) => Some(repository.top),
        RepoLookup::Outside | RepoLookup::Unresolved(_) => None,
    }
}

// ============================================================================
// Commondir resolution (linked-worktree ladder)
// ============================================================================

/// Resolve the `commondir` of a linked-worktree gitdir.
///
/// The `commondir` file is the shared store for a linked worktree — all
/// global refs (branches, tags) and the shared `hooks` folder live in the main
/// repo's gitdir; the per-worktree gitdir only holds HEAD and per-worktree
/// state.
///
/// A linked worktree's gitdir (e.g. `.git/worktrees/<name>/`) contains a
/// `commondir` file pointing at the main repo's gitdir.
pub(super) fn resolve_common_dir(git_dir: &Path) -> Option<PathBuf> {
    use std::io::Read as _;
    let file = std::fs::File::open(git_dir.join("commondir")).ok()?;
    let mut buf = String::new();
    if file
        .take(MAX_COMMONDIR_BYTES)
        .read_to_string(&mut buf)
        .is_err()
    {
        if crate::debug::is_debug_enabled() {
            eprintln!(
                "skim search [debug]: commondir unreadable in {:?}",
                git_dir.display().to_string(),
            );
        }
        return None;
    }
    let first = buf.lines().next()?.trim();
    if first.is_empty() {
        return None;
    }
    let raw = PathBuf::from(first);
    // AD-413-2: a relative `commondir` resolves against the WORKTREE GITDIR (git's
    // default content is `"../.."`); an absolute one is used as-is. Same `is_absolute()`
    // branch shape as `resolve_git_dir`, DIFFERENT anchor: `commondir` resolves against
    // `git_dir`, not `project_root` (unlike resolve_git_dir, which anchors its relative
    // `.git` pointer against project_root) — anchoring on `project_root` lands
    // two levels above the worktree root.
    let joined = if raw.is_absolute() {
        raw
    } else {
        git_dir.join(raw)
    };
    let canonical = match joined.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search [debug]: commondir target is not a git dir ({:?})",
                    joined.display().to_string(),
                );
            }
            return None;
        }
    };
    if !canonical.is_dir() || !canonical.join("HEAD").is_file() {
        // AD-413-3 sanity gate: the target must be a directory containing HEAD.
        if crate::debug::is_debug_enabled() {
            eprintln!(
                "skim search [debug]: commondir target is not a git dir ({:?})",
                canonical.display().to_string(),
            );
        }
        return None;
    }
    Some(canonical)
}

#[cfg(test)]
#[path = "gitdir_tests.rs"]
mod tests;
