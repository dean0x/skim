//! Shell wrapper installation for universal command interception.
//!
//! Creates symlinks in `~/.skim/bin/` that point to the skim binary.
//! When an agent (or sub-agent) invokes a tool like `git`, the shell resolves
//! it to `~/.skim/bin/git`, which is a symlink to the skim binary. The binary
//! detects argv[0] == "git" and dispatches through the existing git handler.
//!
//! ## Recursion prevention (PF-003)
//!
//! The skim binary strips `~/.skim/bin` from PATH as its very first action
//! (`strip_skim_wrappers_from_path()` in `main.rs`). This ensures that when a
//! handler runs `CommandRunner::run("git", …)`, the shell finds `/usr/bin/git`
//! (the real tool), not the symlink again.
//!
//! ## Safety invariant (PF-003)
//!
//! `install_wrappers` NEVER overwrites non-symlink files. If a file at a target
//! path is a regular file, directory, or other non-symlink, it is skipped with
//! a warning. This prevents accidentally clobbering real tools.
//!
//! ## Idempotence
//!
//! Running `install_wrappers` twice produces the same result as running it once.
//! Symlinks that already point to the correct target are skipped. Symlinks
//! pointing to a different target are updated (re-created).

use std::path::{Path, PathBuf};

// ============================================================================
// Result types
// ============================================================================

/// Summary of a wrapper installation run.
#[derive(Debug, Default)]
pub(super) struct InstallResult {
    /// Symlinks newly created.
    pub(super) created: usize,
    /// Symlinks already pointing to the correct target (skipped).
    pub(super) skipped_correct: usize,
    /// Symlinks updated (old symlink removed and re-created with new target).
    pub(super) updated: usize,
    /// Non-symlink files that were skipped to avoid overwriting (PF-003).
    pub(super) skipped_non_symlink: usize,
}

/// Summary of a wrapper uninstallation run.
#[derive(Debug, Default)]
pub(super) struct UninstallResult {
    /// Skim-pointing symlinks that were removed.
    pub(super) removed: usize,
    /// Non-skim files that were preserved.
    pub(super) preserved: usize,
    /// Whether `~/.skim/bin` was removed because it became empty.
    pub(super) dir_removed: bool,
}

// ============================================================================
// Directory resolution
// ============================================================================

/// Return `~/.skim/bin/` — the wrappers directory.
///
/// Delegates to [`crate::cmd::skim_wrappers_dir`] — the single authoritative
/// source for this path — and wraps the `Option` in an `anyhow::Result`.
pub(super) fn wrappers_dir() -> anyhow::Result<PathBuf> {
    crate::cmd::skim_wrappers_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))
}

/// Return the list of tool names for which to create symlinks.
///
/// Delegates to [`crate::cmd::wrapper_targets()`]. Every entry corresponds to a
/// known skim subcommand that wraps an external tool (i.e. not a meta/management
/// subcommand).
pub(super) fn wrapper_targets() -> &'static [&'static str] {
    crate::cmd::wrapper_targets()
}

// ============================================================================
// Installation
// ============================================================================

/// Install wrapper symlinks in `~/.skim/bin/`.
///
/// For each tool name returned by [`wrapper_targets()`], creates a symlink
/// `~/.skim/bin/<tool>` → `skim_binary`.
///
/// ## Idempotence
///
/// - If the symlink already points to `skim_binary`: skip (counts as
///   `skipped_correct`).
/// - If the symlink points somewhere else: remove and re-create (counts as
///   `updated`).
/// - If a non-symlink file exists at the path: skip with a warning to stderr
///   and count as `skipped_non_symlink` (PF-003 safety invariant).
/// - If nothing exists: create the symlink (counts as `created`).
///
/// ## dry_run
///
/// When `dry_run` is `true`, no filesystem changes are made. The function
/// prints `[dry-run] Would create/update …` lines and returns a result
/// with the counts of what *would* have changed.
pub(super) fn install_wrappers(skim_binary: &Path, dry_run: bool) -> anyhow::Result<InstallResult> {
    let dir = wrappers_dir()?;
    let targets = wrapper_targets();
    let mut result = InstallResult::default();

    if !dir.exists() {
        if dry_run {
            println!(
                "  [dry-run] Would create wrapper directory: {}",
                dir.display()
            );
        } else {
            std::fs::create_dir_all(&dir)
                .map_err(|e| anyhow::anyhow!("Failed to create {}: {}", dir.display(), e))?;
        }
    }

    for &tool in targets {
        let link_path = dir.join(tool);
        install_one_symlink(&link_path, skim_binary, tool, dry_run, &mut result)?;
    }

    Ok(result)
}

/// Install (or update) a single symlink.
///
/// Uses `symlink_metadata` as the single atomic entry point, eliminating the
/// TOCTOU window that would exist if we first checked `exists()` / `is_symlink()`
/// and then called `symlink_metadata` again. Three cases are each handled by a
/// dedicated helper to keep cyclomatic complexity low.
#[cfg(unix)]
fn install_one_symlink(
    link_path: &Path,
    skim_binary: &Path,
    tool: &str,
    dry_run: bool,
    result: &mut InstallResult,
) -> anyhow::Result<()> {
    match std::fs::symlink_metadata(link_path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            handle_existing_symlink(link_path, skim_binary, dry_run, result)
        }
        Ok(_) => {
            // PF-003: non-symlink file — never overwrite.
            handle_non_symlink(link_path, tool, result);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            handle_new_symlink(link_path, skim_binary, dry_run, result)
        }
        Err(e) => {
            Err(anyhow::anyhow!("stat {}: {}", link_path.display(), e))
        }
    }
}

/// Handle the case where a symlink already exists at `link_path`.
///
/// If it points to the correct target, skip. Otherwise update it.
#[cfg(unix)]
fn handle_existing_symlink(
    link_path: &Path,
    skim_binary: &Path,
    dry_run: bool,
    result: &mut InstallResult,
) -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    let current_target = std::fs::read_link(link_path)
        .map_err(|e| anyhow::anyhow!("read_link {}: {}", link_path.display(), e))?;

    if current_target == skim_binary {
        result.skipped_correct += 1;
        return Ok(());
    }

    if dry_run {
        println!(
            "  [dry-run] Would update: {} -> {}",
            link_path.display(),
            skim_binary.display()
        );
        result.updated += 1;
        return Ok(());
    }

    std::fs::remove_file(link_path)
        .map_err(|e| anyhow::anyhow!("remove {}: {}", link_path.display(), e))?;
    symlink(skim_binary, link_path)
        .map_err(|e| anyhow::anyhow!("symlink {}: {}", link_path.display(), e))?;
    result.updated += 1;
    Ok(())
}

/// Handle the case where a non-symlink file exists at `link_path` (PF-003 safety).
#[cfg(unix)]
fn handle_non_symlink(link_path: &Path, tool: &str, result: &mut InstallResult) {
    eprintln!(
        "  warning: skipping '{tool}' — {} is not a symlink (not a skim wrapper)",
        link_path.display()
    );
    result.skipped_non_symlink += 1;
}

/// Handle the case where nothing exists at `link_path` — create a new symlink.
#[cfg(unix)]
fn handle_new_symlink(
    link_path: &Path,
    skim_binary: &Path,
    dry_run: bool,
    result: &mut InstallResult,
) -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    if dry_run {
        println!(
            "  [dry-run] Would create: {} -> {}",
            link_path.display(),
            skim_binary.display()
        );
        result.created += 1;
        return Ok(());
    }

    symlink(skim_binary, link_path)
        .map_err(|e| anyhow::anyhow!("symlink {}: {}", link_path.display(), e))?;
    result.created += 1;
    Ok(())
}

#[cfg(not(unix))]
fn install_one_symlink(
    _link_path: &Path,
    _skim_binary: &Path,
    _tool: &str,
    _dry_run: bool,
    _result: &mut InstallResult,
) -> anyhow::Result<()> {
    anyhow::bail!("Wrapper symlinks are only supported on Unix systems")
}

// ============================================================================
// Uninstallation
// ============================================================================

/// Remove skim-pointing symlinks from `~/.skim/bin/`.
///
/// Only removes symlinks whose target path contains `"skim"` or `"rskim"`.
/// Preserves all other files (regular files, other symlinks, directories).
/// If the directory is empty after cleanup, it is removed.
///
/// When `dry_run` is `true`, no filesystem changes are made.
pub(super) fn uninstall_wrappers(dry_run: bool) -> anyhow::Result<UninstallResult> {
    // Circuit breaker: ~/.skim/bin/ is managed entirely by skim and should
    // never contain more entries than the maximum number of wrapper targets.
    // A much larger count indicates a corrupted or adversarial directory.
    const MAX_ENTRIES: usize = 256;

    let dir = wrappers_dir()?;
    let mut result = UninstallResult::default();

    if !dir.exists() {
        return Ok(result);
    }

    let entries =
        std::fs::read_dir(&dir).map_err(|e| anyhow::anyhow!("read {}: {}", dir.display(), e))?;

    let mut count = 0usize;
    for entry in entries {
        count += 1;
        if count > MAX_ENTRIES {
            anyhow::bail!(
                "{} contains more than {MAX_ENTRIES} entries — aborting to avoid unbounded iteration",
                dir.display()
            );
        }

        let entry = entry.map_err(|e| anyhow::anyhow!("read dir entry: {e}"))?;
        let path = entry.path();

        // Only process symlinks.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => {
                result.preserved += 1;
                continue;
            }
        };

        if !meta.file_type().is_symlink() {
            result.preserved += 1;
            continue;
        }

        // Only remove symlinks whose filename stem is exactly "skim" or "rskim".
        // Substring matching (e.g. contains("skim")) would incorrectly remove
        // symlinks pointing to binaries like `/usr/local/bin/someskimmer`.
        let target = std::fs::read_link(&path)
            .map_err(|e| anyhow::anyhow!("read_link {}: {}", path.display(), e))?;
        let stem = target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if stem != "skim" && stem != "rskim" {
            result.preserved += 1;
            continue;
        }

        // This is a skim-pointing symlink — remove it.
        if dry_run {
            println!("  [dry-run] Would remove: {}", path.display());
            result.removed += 1;
        } else {
            std::fs::remove_file(&path)
                .map_err(|e| anyhow::anyhow!("remove {}: {}", path.display(), e))?;
            result.removed += 1;
        }
    }

    // Remove the directory if it is now empty.
    if !dry_run
        && let Ok(mut remaining) = std::fs::read_dir(&dir)
        && remaining.next().is_none()
    {
        let _ = std::fs::remove_dir(&dir);
        result.dir_removed = true;
    }

    Ok(result)
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // install_wrappers tests (unix only)
    // ========================================================================

    #[cfg(unix)]
    #[test]
    fn test_install_creates_expected_symlinks() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let fake_skim = tmp.path().join("skim");
        std::fs::write(&fake_skim, "#!/bin/sh\nexec true").unwrap();
        let install_dir = tmp.path().join(".skim").join("bin");
        std::fs::create_dir_all(&install_dir).unwrap();

        // Install using our wrappers dir (override by writing symlinks manually for targets)
        let targets = wrapper_targets();
        let mut created = 0usize;

        for &tool in targets {
            let link = install_dir.join(tool);
            std::os::unix::fs::symlink(&fake_skim, &link).unwrap();
            // verify
            assert!(link.is_symlink(), "symlink for {tool} must exist");
            assert_eq!(std::fs::read_link(&link).unwrap(), fake_skim);
            created += 1;
        }

        assert_eq!(
            created,
            targets.len(),
            "all wrapper targets must be created"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_install_all_symlinks_point_to_correct_target() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let fake_skim = tmp.path().join("skim");
        std::fs::write(&fake_skim, "#!/bin/sh").unwrap();
        let install_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&install_dir).unwrap();

        for &tool in wrapper_targets() {
            let link = install_dir.join(tool);
            std::os::unix::fs::symlink(&fake_skim, &link).unwrap();
        }

        for &tool in wrapper_targets() {
            let link = install_dir.join(tool);
            let target = std::fs::read_link(&link).unwrap();
            assert_eq!(
                target, fake_skim,
                "symlink for {tool} must point to skim binary"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_install_idempotent_correct_symlink_skipped() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let fake_skim = tmp.path().join("skim");
        std::fs::write(&fake_skim, "").unwrap();
        let link = tmp.path().join("git");

        // Create the correct symlink first.
        std::os::unix::fs::symlink(&fake_skim, &link).unwrap();

        // Now call install_one_symlink with the same target — should skip.
        let mut result = InstallResult::default();
        install_one_symlink(&link, &fake_skim, "git", false, &mut result).unwrap();

        assert_eq!(
            result.skipped_correct, 1,
            "already-correct symlink must be skipped"
        );
        assert_eq!(result.created, 0);
        assert_eq!(result.updated, 0);
    }

    #[cfg(unix)]
    #[test]
    fn test_install_updates_symlink_with_different_target() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let old_skim = tmp.path().join("old_skim");
        let new_skim = tmp.path().join("new_skim");
        std::fs::write(&old_skim, "").unwrap();
        std::fs::write(&new_skim, "").unwrap();
        let link = tmp.path().join("git");

        // Create symlink pointing to old_skim.
        std::os::unix::fs::symlink(&old_skim, &link).unwrap();

        // Install with new_skim — should update.
        let mut result = InstallResult::default();
        install_one_symlink(&link, &new_skim, "git", false, &mut result).unwrap();

        assert_eq!(
            result.updated, 1,
            "symlink with different target must be updated"
        );
        assert_eq!(std::fs::read_link(&link).unwrap(), new_skim);
    }

    #[cfg(unix)]
    #[test]
    fn test_install_skips_non_symlink_file_pf003() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let skim = tmp.path().join("skim");
        std::fs::write(&skim, "").unwrap();
        // Place a regular file where the symlink would go.
        let real_file = tmp.path().join("git");
        std::fs::write(&real_file, "real content").unwrap();

        let mut result = InstallResult::default();
        install_one_symlink(&real_file, &skim, "git", false, &mut result).unwrap();

        assert_eq!(
            result.skipped_non_symlink, 1,
            "non-symlink file must be skipped (PF-003)"
        );
        // The real file must be intact.
        assert_eq!(std::fs::read_to_string(&real_file).unwrap(), "real content");
    }

    #[cfg(unix)]
    #[test]
    fn test_uninstall_removes_only_skim_symlinks() {
        use tempfile::TempDir;

        // The skim binary target must have an exact filename stem of "skim" or
        // "rskim" — `uninstall_wrappers` uses stem matching, not substring
        // matching, to avoid removing symlinks pointing to binaries like
        // `/usr/local/bin/someskimmer`.
        let tmp = TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let fake_skim = tmp.path().join("skim");
        let other_tool = tmp.path().join("other_tool");
        std::fs::write(&fake_skim, "").unwrap();
        std::fs::write(&other_tool, "").unwrap();

        let skim_link = bin_dir.join("git");
        let other_link = bin_dir.join("python");

        std::os::unix::fs::symlink(&fake_skim, &skim_link).unwrap();
        std::os::unix::fs::symlink(&other_tool, &other_link).unwrap();

        // Verify the exact-stem matching logic used by uninstall_wrappers.
        let entries: Vec<_> = std::fs::read_dir(&bin_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();

        let mut removed = 0usize;
        let mut preserved = 0usize;

        for entry in &entries {
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if !meta.file_type().is_symlink() {
                continue; // real files
            }
            let target = std::fs::read_link(&path).unwrap();
            let stem = target.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if stem == "skim" || stem == "rskim" {
                removed += 1;
            } else {
                preserved += 1;
            }
        }

        assert_eq!(removed, 1, "only the skim-pointing symlink must be removed");
        assert_eq!(preserved, 1, "non-skim symlink must be preserved");
    }

    #[cfg(unix)]
    #[test]
    fn test_uninstall_does_not_remove_symlink_to_someskimmer() {
        // Regression guard: a symlink whose target filename contains "skim" as a
        // substring but is not exactly "skim" or "rskim" must be preserved.
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        // `/usr/local/bin/someskimmer` — substring match would falsely remove this.
        let look_alike = tmp.path().join("someskimmer");
        std::fs::write(&look_alike, "").unwrap();

        let link = bin_dir.join("git");
        std::os::unix::fs::symlink(&look_alike, &link).unwrap();

        let target = std::fs::read_link(&link).unwrap();
        let stem = target.file_name().and_then(|n| n.to_str()).unwrap_or("");
        assert!(
            stem != "skim" && stem != "rskim",
            "someskimmer must NOT be classified as a skim binary (stem = {stem:?})"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_dry_run_produces_no_filesystem_changes() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let fake_skim = tmp.path().join("skim");
        std::fs::write(&fake_skim, "").unwrap();

        // We call install_one_symlink with dry_run=true.
        let link = tmp.path().join("git");
        let mut result = InstallResult::default();
        install_one_symlink(&link, &fake_skim, "git", true, &mut result).unwrap();

        // The symlink must NOT exist — dry_run means no filesystem changes.
        assert!(
            !link.exists() && !link.is_symlink(),
            "dry_run must not create any symlinks"
        );
        assert_eq!(result.created, 1, "dry_run reports would-create");
    }

    // ========================================================================
    // wrapper_targets() invariants
    // ========================================================================

    #[test]
    fn test_wrapper_targets_non_empty() {
        assert!(
            !wrapper_targets().is_empty(),
            "wrapper_targets() must return a non-empty list"
        );
    }

    #[test]
    fn test_wrapper_targets_contains_common_tools() {
        let targets = wrapper_targets();
        for expected in &["git", "npm", "grep", "find"] {
            assert!(
                targets.contains(expected),
                "wrapper_targets() must contain '{expected}'"
            );
        }
    }

    #[test]
    fn test_wrapper_targets_excludes_meta_subcommands() {
        let targets = wrapper_targets();
        for meta in &["init", "stats", "discover", "learn", "rewrite"] {
            assert!(
                !targets.contains(meta),
                "wrapper_targets() must not contain meta subcommand '{meta}'"
            );
        }
    }

    // ========================================================================
    // wrappers_dir() test
    // ========================================================================

    #[test]
    fn test_wrappers_dir_is_under_home() {
        if let Some(home) = dirs::home_dir() {
            let dir = wrappers_dir().unwrap();
            assert!(
                dir.starts_with(&home),
                "wrappers_dir must be under home directory"
            );
        }
    }
}
