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

/// Hook filenames to install into.
const HOOK_NAMES: &[&str] = &["post-commit", "post-merge", "post-checkout"];

// ============================================================================
// Hooks directory resolver
// ============================================================================

/// AD-413-15: resolve the hooks directory for the given project root.
///
/// For a linked worktree, routes to the shared `<commondir>/hooks` directory
/// so `install_search_hooks`, `remove_search_hooks`, and `has_search_hooks`
/// can never disagree about where the hooks live (they all call this function).
///
/// For a plain repo (`.git` is a directory), submodule, or non-repo root,
/// falls back to `<root>/.git/hooks` — identical to the pre-413 behavior.
/// The fallback is also used for bare temp directories (as `hooks_tests.rs`
/// creates) because those have no `commondir` file.
pub(crate) fn resolve_hooks_dir(project_root: &Path) -> PathBuf {
    match super::staleness::resolve_git_dir(project_root) {
        Some(git_dir) => super::staleness::resolve_common_dir(&git_dir)
            .unwrap_or(git_dir)
            .join("hooks"),
        None => project_root.join(".git").join("hooks"),
    }
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
/// # Errors
///
/// Returns `Err` on I/O failures during file creation or modification.
pub(crate) fn install_search_hooks(project_root: &Path) -> anyhow::Result<()> {
    let hooks_dir = resolve_hooks_dir(project_root);
    std::fs::create_dir_all(&hooks_dir)?;

    for name in HOOK_NAMES {
        let hook_path = hooks_dir.join(name);
        install_one_hook(&hook_path)?;
    }

    Ok(())
}

/// Remove the skim marker block from all search hooks for `project_root`.
///
/// For each hook, strips the `# skim-search-start … # skim-search-end` block.
/// Leaves all other content intact.  Non-fatal: missing hooks are silently skipped.
///
/// Returns `true` if at least one marker block was found and removed; `false`
/// if the hooks directory had no skim marker blocks.  Callers use this to
/// suppress the "removed from" success line when no block was present (AC31(a)).
///
/// # Errors
///
/// Returns `Err` on I/O failures when reading or writing hook files.
pub(crate) fn remove_search_hooks(project_root: &Path) -> anyhow::Result<bool> {
    let hooks_dir = resolve_hooks_dir(project_root);
    let mut any_removed = false;
    for name in HOOK_NAMES {
        let hook_path = hooks_dir.join(name);
        if hook_path.exists() {
            any_removed |= remove_from_hook(&hook_path)?;
        }
    }
    Ok(any_removed)
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
fn install_one_hook(hook_path: &Path) -> anyhow::Result<()> {
    if hook_path.exists() {
        let content = std::fs::read_to_string(hook_path)?;
        // Idempotent: if markers already present, skip.
        if content.contains(MARKER_START) {
            return Ok(());
        }
        // Append block to existing hook.
        let new_content = append_block(&content);
        write_hook_atomic(hook_path, &new_content)?;
    } else {
        // Create new hook with shebang + block.
        let content = format!("{SHEBANG}\n{HOOK_BLOCK}\n");
        write_hook_atomic(hook_path, &content)?;
    }
    Ok(())
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
