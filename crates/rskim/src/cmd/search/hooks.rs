//! Git hook installation for automatic index refresh.
//!
//! Installs and removes marker-delimited blocks in git hook scripts so the
//! search index is refreshed automatically after commits, merges, and checkouts.
//!
//! # Hook block format
//!
//! ```sh
//! # skim-search-start
//! skim search --update 2>/dev/null &
//! # skim-search-end
//! ```
//!
//! # Idempotency
//!
//! `install_search_hooks` checks for the start/end markers before writing.
//! If the block is already present, the function is a no-op.  Running install
//! twice is safe and produces exactly one copy of the block.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

// ============================================================================
// Constants
// ============================================================================

const MARKER_START: &str = "# skim-search-start";
const MARKER_END: &str = "# skim-search-end";
const HOOK_BLOCK: &str =
    "# skim-search-start\nskim search --update 2>/dev/null &\n# skim-search-end";
const SHEBANG: &str = "#!/bin/sh";

/// User-facing notice emitted when the resolved hooks directory is the shared
/// `<commondir>/hooks` (every worktree of the clone shares it).
///
/// Named constant so the two emit sites (`install_search_hooks` and
/// `remove_search_hooks`) cannot drift — per the project convention established
/// by `NO_TEMPORAL_DATA_MSG` in `mod.rs` (#357 cycle-2 finding 2).
pub(super) const SHARED_HOOKS_SCOPE_MSG: &str =
    "skim search: this hooks directory is shared by every worktree of this clone";

/// Hook filenames to install into.
const HOOK_NAMES: &[&str] = &["post-commit", "post-merge", "post-checkout"];

// ============================================================================
// Outcome type
// ============================================================================

/// Outcome of a hooks install or remove operation.
///
/// Returned by [`install_search_hooks`] and [`remove_search_hooks`] so every
/// caller — including `skim init` — receives the resolved directory without
/// requiring a separate [`resolve_hooks_dir`] call.
///
/// The shared-scope notice (AC34(b)) is emitted by the operation itself;
/// callers receive `dir` for their own path-disclosure messages and `changed`
/// for suppression of empty-operation output.
#[derive(Debug)]
pub(crate) struct HooksOutcome {
    /// The resolved hooks directory (the directory written to or read from).
    pub dir: PathBuf,
    /// `true` when the operation actually changed at least one hook file.
    ///
    /// For `install_search_hooks`: `true` means at least one hook was created
    /// or had the skim block appended.  `false` means every hook already
    /// contained the markers (idempotent no-op).
    ///
    /// For `remove_search_hooks`: `true` means at least one marker block was
    /// found and removed.  `false` means no block was present.
    pub changed: bool,
}

// ============================================================================
// Hooks directory resolver
// ============================================================================

/// AD-413-15: resolve the hooks directory for the given project root.
///
/// For a linked worktree, routes to the shared `<commondir>/hooks` directory
/// so `install_search_hooks`, `remove_search_hooks`, and `has_search_hooks`
/// can never disagree about where the hooks live (they all call this function).
///
/// For a plain repo (`.git` is a directory) or a non-repo root, this returns
/// `<root>/.git/hooks` — byte-identical to the pre-#413 behavior, because a plain
/// repo has no `commondir` file and a non-repo root has no git dir at all.  The
/// same fallback covers the bare temp directories `hooks_tests.rs` creates.
///
/// **Submodules also move, deliberately.** A submodule's `.git` is a *file* whose
/// gitdir is `<super>/.git/modules/<name>`, so the pre-#413 hand-built path
/// `<sub>/.git/hooks` did not exist and both install and `has_search_hooks` were
/// silently broken there too.  This resolver returns `<super>/.git/modules/<name>/hooks`,
/// which is what `git -C <sub> rev-parse --git-path hooks` reports — a fix, not a
/// regression (the submodule gitdir is a complete ref store with no `commondir`,
/// so no redirection happens; only the `.git`-file indirection is followed).
///
/// Scope boundary (AD-413-15): this handles the worktree/submodule gitdir
/// indirection ONLY.  It deliberately does NOT adopt an ancestor repository for a
/// subdirectory root the way `staleness::git_head_state` does, so a subdirectory
/// root keeps today's `<root>/.git/hooks` behavior.
///
/// **Write-path security (AD-413-3 extension):** when `.git` is a FILE, the
/// `gitdir:` pointer is untrusted, repository-controlled input (ADR-008).
/// The resolver applies a two-stage gate before using any derived path as a
/// write destination:
///
/// 1. [`staleness::resolve_common_dir`] is tried first — it is already validated
///    by the AD-413-3 sanity gate (`HEAD` is_file + is_dir).  When it resolves,
///    the result is used directly (this is the normal linked-worktree path).
/// 2. If the commondir is absent (submodule, or unusual gitdir), the gitdir
///    itself must pass [`looks_like_git_dir`] (`HEAD` is_file plus `objects/`
///    and `refs/` subdirectories) before skim writes into it.  A malicious
///    pointer to an arbitrary directory — e.g. `~/Library/LaunchAgents` — has
///    no `objects/` or `refs/`, so it fails the gate and skim falls back to the
///    safe local `<root>/.git/hooks`.
pub(crate) fn resolve_hooks_dir(project_root: &Path) -> PathBuf {
    match super::staleness::resolve_git_dir(project_root) {
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
                if let Some(common) = super::staleness::resolve_common_dir(&git_dir) {
                    return common.join("hooks");
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
                    return project_root.join(".git").join("hooks");
                }
                git_dir.join("hooks")
            } else {
                // `.git` is a directory — already known to exist on disk and is
                // therefore a trustworthy base path.  Follow the commondir if
                // present (plain repos typically have none), otherwise use
                // git_dir directly.
                super::staleness::resolve_common_dir(&git_dir)
                    .unwrap_or(git_dir)
                    .join("hooks")
            }
        }
        None => project_root.join(".git").join("hooks"),
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

/// Return `true` when `hooks_dir` differs from the plain `<root>/.git/hooks`.
///
/// A simple path inequality is sufficient because `resolve_hooks_dir` builds
/// the plain path with the same `project_root.join(".git").join("hooks")`
/// expression, so the comparison is byte-identical for plain repos.
fn is_shared_hooks_dir(project_root: &Path, hooks_dir: &Path) -> bool {
    let plain = project_root.join(".git").join("hooks");
    hooks_dir != plain
}

// ============================================================================
// Public API
// ============================================================================

/// Install skim search hooks in the resolved hooks directory for `project_root`.
///
/// For each of `post-commit`, `post-merge`, and `post-checkout`:
/// - If the hook doesn't exist, creates it with `#!/bin/sh` and the skim block.
/// - If the hook exists but doesn't have the markers, appends the block.
/// - If the hook already has the markers, leaves it unchanged (idempotent).
///
/// For a linked worktree, the resolved directory is `<commondir>/hooks` (shared
/// by every worktree of the clone).  For a plain repo, it is `<root>/.git/hooks`
/// (identical to the pre-413 behavior — AC5b monotonicity).
///
/// The hooks directory is created if it doesn't exist.
///
/// # Shared-scope disclosure (AC34(b))
///
/// When the resolved hooks directory is the shared `<commondir>/hooks` rather
/// than the local `<root>/.git/hooks`, this function emits a notice to stderr:
///
/// ```text
/// skim search: this hooks directory is shared by every worktree of this clone
/// ```
///
/// The notice is emitted here — not at the call site — so every caller
/// (including `skim init`) inherits it automatically without repeating the check.
///
/// # Errors
///
/// Returns `Err` on I/O failures during file creation or modification.
pub(crate) fn install_search_hooks(project_root: &Path) -> anyhow::Result<HooksOutcome> {
    let hooks_dir = resolve_hooks_dir(project_root);
    std::fs::create_dir_all(&hooks_dir)?;

    let mut any_changed = false;
    for name in HOOK_NAMES {
        let hook_path = hooks_dir.join(name);
        any_changed |= install_one_hook(&hook_path)?;
    }

    // AC34(b): disclose the clone-wide scope from inside this function so every
    // caller (including `skim init`) inherits the notice automatically.
    if is_shared_hooks_dir(project_root, &hooks_dir) {
        eprintln!("{SHARED_HOOKS_SCOPE_MSG}");
    }

    Ok(HooksOutcome {
        dir: hooks_dir,
        changed: any_changed,
    })
}

/// Remove the skim marker block from all search hooks for `project_root`.
///
/// For each hook, strips the `# skim-search-start … # skim-search-end` block.
/// Leaves all other content intact.  Non-fatal: missing hooks are silently skipped.
///
/// # Shared-scope disclosure (AC34(b))
///
/// When the resolved hooks directory is the shared `<commondir>/hooks` AND at
/// least one marker block was removed, this function emits a notice to stderr.
/// The notice is emitted here so every caller inherits it automatically.
///
/// # Errors
///
/// Returns `Err` on I/O failures when reading or writing hook files.
pub(crate) fn remove_search_hooks(project_root: &Path) -> anyhow::Result<HooksOutcome> {
    let hooks_dir = resolve_hooks_dir(project_root);
    let mut any_removed = false;
    for name in HOOK_NAMES {
        let hook_path = hooks_dir.join(name);
        if hook_path.exists() {
            any_removed |= remove_from_hook(&hook_path)?;
        }
    }
    // AC34(b): disclose the clone-wide scope only when something was actually
    // removed — mirrors the conditional disclosure in `run_remove_hooks`.
    if any_removed && is_shared_hooks_dir(project_root, &hooks_dir) {
        eprintln!("{SHARED_HOOKS_SCOPE_MSG}");
    }
    Ok(HooksOutcome {
        dir: hooks_dir,
        changed: any_removed,
    })
}

/// Return `true` if any of the search hook files contain the skim markers.
///
/// Used in tests and by external callers that check hook installation state.
#[allow(dead_code)]
pub(crate) fn has_search_hooks(project_root: &Path) -> bool {
    let hooks_dir = resolve_hooks_dir(project_root);
    HOOK_NAMES.iter().any(|name| {
        let p = hooks_dir.join(name);
        std::fs::read_to_string(&p)
            .map(|c| c.contains(MARKER_START))
            .unwrap_or(false)
    })
}

// ============================================================================
// Private helpers
// ============================================================================

/// Install the skim block into a single hook file.
///
/// Returns `true` if the file was created or had the block appended;
/// `false` if the skim markers were already present (idempotent no-op).
fn install_one_hook(hook_path: &Path) -> anyhow::Result<bool> {
    if hook_path.exists() {
        let content = std::fs::read_to_string(hook_path)?;
        // Idempotent: if markers already present, skip.
        if content.contains(MARKER_START) {
            return Ok(false);
        }
        // Append block to existing hook.
        let new_content = append_block(&content);
        write_hook_atomic(hook_path, &new_content)?;
    } else {
        // Create new hook with shebang + block.
        let content = format!("{SHEBANG}\n{HOOK_BLOCK}\n");
        write_hook_atomic(hook_path, &content)?;
    }
    Ok(true)
}

/// Strip the skim marker block from a hook file.
///
/// Returns `true` if a marker block was found and removed; `false` if the file
/// contained no skim markers (no-op).
fn remove_from_hook(hook_path: &Path) -> anyhow::Result<bool> {
    let content = std::fs::read_to_string(hook_path)?;
    if !content.contains(MARKER_START) {
        return Ok(false); // Nothing to remove.
    }
    let stripped = strip_block(&content);
    write_hook_atomic(hook_path, &stripped)?;
    Ok(true)
}

/// Append the skim block to existing hook content.
fn append_block(existing: &str) -> String {
    let mut result = existing.to_string();
    if !result.ends_with('\n') {
        result.push('\n');
    }
    result.push_str(HOOK_BLOCK);
    result.push('\n');
    result
}

/// Remove the skim marker block and the surrounding blank lines from `content`.
fn strip_block(content: &str) -> String {
    let start_pos = match content.find(MARKER_START) {
        Some(p) => p,
        None => return content.to_string(),
    };
    let end_pos = match content.find(MARKER_END) {
        Some(p) => p,
        None => return content.to_string(),
    };
    if end_pos < start_pos {
        return content.to_string(); // Corrupted — leave intact.
    }
    let end_byte = end_pos + MARKER_END.len();

    let before = content[..start_pos].trim_end_matches('\n');
    // Consume the newline immediately after the end marker (if any).
    let after_start = if content[end_byte..].starts_with('\n') {
        end_byte + 1
    } else {
        end_byte
    };
    let after = &content[after_start..];

    if before.is_empty() {
        after.to_string()
    } else {
        format!("{before}\n{after}")
    }
}

/// Atomically write `content` to `hook_path` via an unpredictably-named temp
/// file in the same directory (so the rename is always on the same filesystem).
///
/// Using `NamedTempFile::new_in` instead of a fixed `.tmp` suffix avoids
/// a symlink/TOCTOU attack where an adversary pre-creates a predictable path
/// and redirects the write to an arbitrary target.
///
/// On Unix, sets executable permission (0o755) so the hook can be run by git.
fn write_hook_atomic(hook_path: &Path, content: &str) -> anyhow::Result<()> {
    let parent = hook_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("hook path has no parent directory"))?;

    let mut tmp = NamedTempFile::new_in(parent)?;

    use std::io::Write as _;
    tmp.write_all(content.as_bytes())?;

    // Set executable permission before persist.
    #[cfg(unix)]
    {
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(tmp.path(), perms)?;
    }

    tmp.persist(hook_path)
        .map_err(|e| anyhow::anyhow!("failed to persist hook file: {}", e))?;

    Ok(())
}

// ============================================================================
// Tests (co-located in hooks_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
