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
//! discovery lives in `resolve_repo_toplevel`, which for a root with NO `.git`
//! at all adopts the nearest enclosing repository using
//! `discover_project_root_from_canonical`, the same bounded ancestor scan that
//! finds the root of a query, so `--root <subdir>` and a bare invocation from
//! that subdirectory agree.

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

/// The folders where the ancestor walk for a repository root must stop.
///
/// The walk never steps into a ceiling folder, never reaches the home folder
/// from below, and never reaches the filesystem root from below. A start folder
/// is never excluded: it can be a ceiling, the home folder or `/` itself.
#[derive(Debug, Default, Clone)]
pub(super) struct RootRules {
    pub(super) ceilings: Vec<PathBuf>,
    pub(super) home: Option<PathBuf>,
    /// `GIT_CEILING_DIRECTORIES` holds more than [`MAX_CEILING_ENTRIES`]
    /// absolute entries. The entries past the bound are not read, so the walk
    /// does not go up at all: it cannot pass a ceiling it did not read.
    pub(super) ceiling_overflow: bool,
}

impl RootRules {
    /// The rules of this process: `GIT_CEILING_DIRECTORIES` and the home
    /// folder.
    pub(super) fn from_env() -> Self {
        let (ceilings, ceiling_overflow) = std::env::var_os("GIT_CEILING_DIRECTORIES")
            .map(|value| (parse_ceilings(&value), too_many_ceilings(&value)))
            .unwrap_or_default();
        let home = dirs::home_dir().and_then(|home| home.canonicalize().ok());
        RootRules {
            ceilings,
            home,
            ceiling_overflow,
        }
    }

    /// Whether the walk ends before `candidate`, an ancestor of the start.
    fn stops_at(&self, candidate: &Path) -> bool {
        self.ceiling_overflow
            || candidate.parent().is_none()
            || self.home.as_deref() == Some(candidate)
            || self.ceilings.iter().any(|c| c == candidate)
    }
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

/// Whether a `GIT_CEILING_DIRECTORIES` value holds more absolute entries than
/// [`parse_ceilings`] reads. It reads at most one entry past the bound.
fn too_many_ceilings(value: &std::ffi::OsStr) -> bool {
    std::env::split_paths(value)
        .filter(|p| p.is_absolute())
        .nth(MAX_CEILING_ENTRIES)
        .is_some()
}

/// Walk up from `canonical` (a pre-canonicalized path) looking for the nearest
/// ancestor that contains a `.git` entry, under the process's [`RootRules`].
///
/// Returns the first matching ancestor as a `PathBuf`, or `canonical` itself
/// when no enclosing `.git` is found within the [`MAX_ANCESTORS`] bound, or when
/// a ceiling, the home folder or `/` ends the walk first.
///
/// The caller is responsible for canonicalizing `canonical` first — this variant
/// skips the `canonicalize()` call to avoid a redundant O(path depth) syscall
/// chain when the caller already holds a canonical path (e.g.
/// `resolve_repo_toplevel`).
fn discover_project_root_from_canonical(canonical: &Path) -> PathBuf {
    discover_project_root_with(canonical, &RootRules::from_env())
}

/// [`discover_project_root_from_canonical`] under explicit `rules`.
pub(super) fn discover_project_root_with(canonical: &Path, rules: &RootRules) -> PathBuf {
    let mut current = canonical;
    for _ in 0..MAX_ANCESTORS {
        if current != canonical && rules.stops_at(current) {
            break;
        }
        if current.join(".git").exists() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    canonical.to_path_buf()
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

/// Walk up from `project_root` to the nearest enclosing git repository when
/// `project_root` itself has no `.git` entry of its own.
///
/// Returns `None` when:
/// - `project_root` already has a `.git` entry (never re-point it).
/// - No enclosing repo is found within the `MAX_ANCESTORS` bound.
/// - The first-match ancestor does not contain `project_root` (containment check).
/// - The ancestor's git directory has no readable `HEAD` file.
///
/// Uses `discover_project_root_from_canonical`, the same bounded ancestor scan
/// that finds the root of a query.
pub(super) fn resolve_repo_toplevel(project_root: &Path) -> Option<PathBuf> {
    // Never re-point a root that claims to be a repository already.
    // Use try_exists() to distinguish "absent" from "exists but unreadable" —
    // a permission error on `.git` must not silently trigger ancestor adoption
    // of an enclosing repository; treat the error conservatively as "present"
    // (return None / NotAdopted) so we never index a different repository's
    // history when the intended root is blocked by a permission fence.
    if project_root.join(".git").try_exists().unwrap_or(true) {
        return None;
    }
    let canonical = project_root.canonicalize().ok()?;
    // Call the local from-canonical variant to skip the redundant canonicalize()
    // that discover_project_root would perform on the already-canonical path
    // (O(path depth) lstat/readlink syscalls — measured, fixed per finding F2).
    let top = discover_project_root_from_canonical(&canonical);
    // `discover_project_root_from_canonical` returns the start path when no
    // enclosing repo is found.
    if top == canonical {
        return None;
    }
    // Containment: `project_root` must live inside the discovered toplevel.
    if !canonical.starts_with(&top) {
        return None;
    }
    // F10: the toplevel must have a git dir with a readable HEAD file.
    let git_dir = resolve_git_dir(&top)?;
    git_dir.join("HEAD").is_file().then_some(top)
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
