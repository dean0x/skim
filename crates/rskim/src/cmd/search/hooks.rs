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
use std::path::Path;

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
// Public API
// ============================================================================

/// Install skim search hooks in `.git/hooks/` for the given `project_root`.
///
/// For each of `post-commit`, `post-merge`, and `post-checkout`:
/// - If the hook doesn't exist, creates it with `#!/bin/sh` and the skim block.
/// - If the hook exists but doesn't have the markers, appends the block.
/// - If the hook already has the markers, leaves it unchanged (idempotent).
///
/// The hooks directory is created if it doesn't exist.
///
/// # Errors
///
/// Returns `Err` on I/O failures during file creation or modification.
pub(crate) fn install_search_hooks(project_root: &Path) -> anyhow::Result<()> {
    let hooks_dir = project_root.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;

    for name in HOOK_NAMES {
        let hook_path = hooks_dir.join(name);
        install_one_hook(&hook_path)?;
    }

    Ok(())
}

/// Remove the skim marker block from all search hooks in `project_root`.
///
/// For each hook, strips the `# skim-search-start … # skim-search-end` block.
/// Leaves all other content intact.  Non-fatal: missing hooks are silently skipped.
///
/// # Errors
///
/// Returns `Err` on I/O failures when reading or writing hook files.
pub(crate) fn remove_search_hooks(project_root: &Path) -> anyhow::Result<()> {
    let hooks_dir = project_root.join(".git").join("hooks");
    for name in HOOK_NAMES {
        let hook_path = hooks_dir.join(name);
        if hook_path.exists() {
            remove_from_hook(&hook_path)?;
        }
    }
    Ok(())
}

/// Return `true` if any of the search hook files contain the skim markers.
///
/// Used in tests and by external callers that check hook installation state.
#[allow(dead_code)]
pub(crate) fn has_search_hooks(project_root: &Path) -> bool {
    let hooks_dir = project_root.join(".git").join("hooks");
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
fn remove_from_hook(hook_path: &Path) -> anyhow::Result<()> {
    let content = std::fs::read_to_string(hook_path)?;
    if !content.contains(MARKER_START) {
        return Ok(()); // Nothing to remove.
    }
    let stripped = strip_block(&content);
    write_hook_atomic(hook_path, &stripped)?;
    Ok(())
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

    // Trim the newline immediately after the end marker (if any).
    let after_end = &content[end_byte..];
    let skip_newline = if after_end.starts_with('\n') { 1 } else { 0 };

    let before = content[..start_pos].trim_end_matches('\n');
    let after = &content[end_byte + skip_newline..];

    if before.is_empty() {
        after.to_string()
    } else {
        format!("{before}\n{after}")
    }
}

/// Atomically write `content` to `hook_path` via a sibling `.tmp` file.
///
/// On Unix, sets executable permission (0o755) so the hook can be run by git.
fn write_hook_atomic(hook_path: &Path, content: &str) -> anyhow::Result<()> {
    let tmp_path = hook_path.with_extension("tmp");
    if let Err(e) = std::fs::write(&tmp_path, content) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }
    // Set executable permission before rename.
    #[cfg(unix)]
    {
        let perms = std::fs::Permissions::from_mode(0o755);
        if let Err(e) = std::fs::set_permissions(&tmp_path, perms) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e.into());
        }
    }
    if let Err(e) = std::fs::rename(&tmp_path, hook_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }
    Ok(())
}

// ============================================================================
// Tests (co-located in hooks_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
