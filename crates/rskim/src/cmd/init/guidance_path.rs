//! Where a guidance file may be read and written.
//!
//! The path policy for the instruction files that `skim init` changes. It looks
//! at paths only; the text of a file is read and written elsewhere
//! (`guidance.rs`).
//!
//! - **Global scope.** The path must be absolute. A link at the file name is
//!   followed, and it must end at a regular file.
//! - **Project scope.** Everything stays inside the project folder (the root).
//!   The deepest existing folder above the file is resolved and must lie inside
//!   the root. A link at the file name is followed only when it ends inside the
//!   root at a regular file, so `CLAUDE.md -> AGENTS.md` keeps working.
//!
//! A refusal is a typed value that names the file, never a panic. The caller
//! prints it as one `Note:` line and leaves the file alone.
//!
//! The folders above a file are checked before the open, not at the open. A
//! folder that another process replaces with a link after the check is
//! followed. `skim search` states the same limit.

use std::io;
use std::path::{Component, Path, PathBuf};

use crate::cmd::display_path;
use crate::cmd::session::{AgentKind, InstructionEnv, RelativeOverride};

// ============================================================================
// Global paths are absolute
// ============================================================================

/// Why a global guidance path is refused.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum GlobalPathRefusal {
    /// A config-folder variable holds a relative path. The refusal names it.
    RelativeOverride(RelativeOverride),
    /// The path is relative, and no variable is the cause (a relative home folder).
    NotAbsolute(PathBuf),
}

impl std::fmt::Display for GlobalPathRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RelativeOverride(refused) => refused.fmt(f),
            Self::NotAbsolute(path) => write!(
                f,
                "the global guidance path {path:?} is not absolute. \
                 The home folder must be an absolute path."
            ),
        }
    }
}

impl std::error::Error for GlobalPathRefusal {}

/// Refuse a global guidance `path` that is not absolute.
///
/// Call it before any read or write of a global guidance file: a relative path
/// would land in the working directory. A relative config-folder variable is
/// reported by name first, because that is what the user must fix.
pub(super) fn require_absolute_global(
    agent: AgentKind,
    path: &Path,
    env: &InstructionEnv,
) -> Result<(), GlobalPathRefusal> {
    env.checked_override(agent)
        .map_err(GlobalPathRefusal::RelativeOverride)?;
    if path.is_absolute() {
        Ok(())
    } else {
        Err(GlobalPathRefusal::NotAbsolute(path.to_path_buf()))
    }
}

/// Run [`require_absolute_global`] on the global instruction file of `agent`.
///
/// An agent with no global instruction file passes: nothing is read or written.
pub(super) fn check_global_target(
    agent: AgentKind,
    env: &InstructionEnv,
) -> Result<(), GlobalPathRefusal> {
    match agent.instruction_file(true, env) {
        Some(path) => require_absolute_global(agent, &path, env),
        None => Ok(()),
    }
}

// ============================================================================
// Scope and resolution
// ============================================================================

/// Where a guidance file lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GuidanceScope {
    /// The agent's folder in the home folder (or the folder a variable names).
    Global,
    /// A file of the project folder `root`. Relative paths are relative to it.
    Project { root: PathBuf },
}

impl GuidanceScope {
    /// The scope for the `global` flag. The project scope is the current folder.
    pub(super) fn for_flag(global: bool) -> anyhow::Result<Self> {
        if global {
            return Ok(Self::Global);
        }
        let root = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("could not read the current folder: {e}"))?;
        Ok(Self::Project { root })
    }
}

/// A guidance path that the policy accepts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedPath {
    /// The path to open and write. Absolute. A link at the file name is already
    /// followed, so this is the file itself.
    pub(super) path: PathBuf,
    /// The path to show in messages: relative to the project folder in project
    /// scope, the same as `path` in global scope.
    pub(super) shown: PathBuf,
}

/// Why a guidance path is refused. Each refusal names the file and leaves it
/// unchanged.
#[derive(Debug)]
pub(super) enum PathRefusal {
    /// The file, or a folder above it, leads outside the project folder.
    LeavesProject { path: PathBuf },
    /// A link at the file name does not end at a regular file.
    NotRegular { path: PathBuf },
    /// The path could not be checked.
    Unchecked { path: PathBuf, error: io::Error },
}

impl PathRefusal {
    /// The file the refusal names, as the caller wrote it.
    pub(super) fn path(&self) -> &Path {
        match self {
            Self::LeavesProject { path }
            | Self::NotRegular { path }
            | Self::Unchecked { path, .. } => path,
        }
    }
}

impl std::fmt::Display for PathRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LeavesProject { path } => write!(
                f,
                "{} leads outside this project folder. skim leaves it unchanged.",
                display_path(path)
            ),
            Self::NotRegular { path } => write!(
                f,
                "{} is a link that does not end at a regular file. skim leaves it unchanged.",
                display_path(path)
            ),
            Self::Unchecked { path, error } => write!(
                f,
                "skim could not check {} ({error}). It leaves it unchanged.",
                display_path(path)
            ),
        }
    }
}

impl std::error::Error for PathRefusal {}

/// Apply the path policy to `path` in `scope`.
///
/// Reads the file system and changes nothing: no folder is created. Missing
/// folders and a missing file are fine; the caller creates them after this
/// returns.
pub(super) fn resolve_guidance_path(
    path: &Path,
    scope: &GuidanceScope,
) -> Result<ResolvedPath, PathRefusal> {
    match scope {
        GuidanceScope::Global => resolve_global(path),
        GuidanceScope::Project { root } => {
            let root = std::fs::canonicalize(root).map_err(|e| unchecked(path, e))?;
            resolve_project_at_root(path, &root)
        }
    }
}

/// The identity of a path that passed the policy, for shared-file checks.
///
/// Resolve existing ancestors too: two config folders can name the same file
/// before that file or its immediate parent exists. This key does not grant
/// permission to read or write; callers still use the checked target path.
/// If a path cannot be checked, keep its original identity.
pub(super) fn guidance_file_key(path: &Path) -> PathBuf {
    for ancestor in path.ancestors() {
        match std::fs::canonicalize(ancestor) {
            Ok(real) => {
                if let Ok(suffix) = path.strip_prefix(ancestor) {
                    return real.join(suffix);
                }
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => break,
        }
    }
    path.to_path_buf()
}

fn unchecked(path: &Path, error: io::Error) -> PathRefusal {
    PathRefusal::Unchecked {
        path: path.to_path_buf(),
        error,
    }
}

/// Global scope: an absolute path whose final link, if any, ends at a regular
/// file. No folder above the file is checked.
fn resolve_global(path: &Path) -> Result<ResolvedPath, PathRefusal> {
    if !path.is_absolute() {
        return Err(unchecked(
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "the path is not absolute"),
        ));
    }
    let real = follow_final_link(path, path, None)?;
    Ok(ResolvedPath {
        shown: real.clone(),
        path: real,
    })
}

/// The file and its parent folders must stay inside the caller's canonical root.
pub(super) fn resolve_project_at_root(
    path: &Path,
    root: &Path,
) -> Result<ResolvedPath, PathRefusal> {
    // A `..` step could climb out of the root in a part of the path that does
    // not exist yet, where no check below can see it.
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(PathRefusal::LeavesProject {
            path: path.to_path_buf(),
        });
    }
    let file = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };

    if let Some(parent) = file.parent() {
        check_folder_in_root(parent, path, root)?;
    }

    let real = follow_final_link(&file, path, Some(root))?;
    let shown = real
        .strip_prefix(root)
        .map_or_else(|_| real.clone(), Path::to_path_buf);
    Ok(ResolvedPath { path: real, shown })
}

/// The deepest existing folder must lie inside the canonical `root`.
/// Folders below it do not exist, so no link can be in them.
pub(super) fn check_folder_in_root(
    folder: &Path,
    shown: &Path,
    root: &Path,
) -> Result<(), PathRefusal> {
    let existing = deepest_existing(folder).map_err(|e| unchecked(shown, e))?;
    if !existing.starts_with(root) {
        return Err(PathRefusal::LeavesProject {
            path: shown.to_path_buf(),
        });
    }
    if !std::fs::metadata(existing)
        .map_err(|e| unchecked(shown, e))?
        .is_dir()
    {
        return Err(unchecked(shown, io::ErrorKind::NotADirectory.into()));
    }
    Ok(())
}

/// The canonical path of the deepest ancestor of `folder` (or `folder` itself)
/// that exists.
fn deepest_existing(folder: &Path) -> io::Result<PathBuf> {
    for ancestor in folder.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => return std::fs::canonicalize(ancestor),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no folder above the file exists",
    ))
}

/// Resolve a link at the last component of `file`. `shown` names the file in a
/// refusal. With a `root`, the link must end inside it.
///
/// A path that is not a link is returned as it is, whether or not it exists:
/// the open checks the type of the opened file. A link must end at a regular
/// file. A link whose target is gone does not.
fn follow_final_link(
    file: &Path,
    shown: &Path,
    root: Option<&Path>,
) -> Result<PathBuf, PathRefusal> {
    let meta = match std::fs::symlink_metadata(file) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(file.to_path_buf()),
        Err(e) => return Err(unchecked(shown, e)),
    };
    if !meta.file_type().is_symlink() {
        return Ok(file.to_path_buf());
    }
    let not_regular = || PathRefusal::NotRegular {
        path: shown.to_path_buf(),
    };
    let real = match std::fs::canonicalize(file) {
        Ok(real) => real,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(not_regular()),
        Err(e) => return Err(unchecked(shown, e)),
    };
    if root.is_some_and(|root| !real.starts_with(root)) {
        return Err(PathRefusal::LeavesProject {
            path: shown.to_path_buf(),
        });
    }
    match std::fs::metadata(&real) {
        Ok(meta) if meta.is_file() => Ok(real),
        Ok(_) => Err(not_regular()),
        Err(e) => Err(unchecked(shown, e)),
    }
}

#[cfg(test)]
#[path = "guidance_path_tests.rs"]
mod tests;
