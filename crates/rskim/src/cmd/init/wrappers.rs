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
        .map(|p| p.to_path_buf())
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
/// Delegates to [`install_wrappers_in`] with the canonical wrappers directory
/// from [`wrappers_dir()`]. This is the public API used by `skim init --wrappers`.
pub(super) fn install_wrappers(skim_binary: &Path, dry_run: bool) -> anyhow::Result<InstallResult> {
    let dir = wrappers_dir()?;
    install_wrappers_in(&dir, skim_binary, dry_run)
}

/// Install wrapper symlinks in `dir`.
///
/// Accepts an explicit directory so that callers (including tests) can pass a
/// temporary directory instead of always writing to `~/.skim/bin/`. The public
/// API [`install_wrappers`] delegates here after resolving the canonical path.
///
/// For each tool name returned by [`wrapper_targets()`], creates a symlink
/// `<dir>/<tool>` → `skim_binary`.
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
pub(super) fn install_wrappers_in(
    dir: &Path,
    skim_binary: &Path,
    dry_run: bool,
) -> anyhow::Result<InstallResult> {
    let targets = wrapper_targets();
    let mut result = InstallResult::default();

    if !dir.exists() {
        if dry_run {
            println!(
                "  [dry-run] Would create wrapper directory: {}",
                dir.display()
            );
        } else {
            std::fs::create_dir_all(dir)
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
        Err(e) => Err(anyhow::anyhow!("stat {}: {}", link_path.display(), e)),
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
/// Delegates to [`uninstall_wrappers_in`] with the canonical wrappers directory
/// from [`wrappers_dir()`]. This is the public API used by `skim init --uninstall`.
pub(super) fn uninstall_wrappers(dry_run: bool) -> anyhow::Result<UninstallResult> {
    let dir = wrappers_dir()?;
    uninstall_wrappers_in(&dir, dry_run)
}

/// Remove skim-pointing symlinks from `dir`.
///
/// Accepts an explicit directory so that callers (including tests) can pass a
/// temporary directory instead of always operating on `~/.skim/bin/`. The
/// public API [`uninstall_wrappers`] delegates here after resolving the
/// canonical path.
///
/// Only removes symlinks whose target filename is exactly `"skim"` or
/// `"rskim"`. Preserves all other files (regular files, other symlinks,
/// directories). If the directory is empty after cleanup, it is removed.
///
/// When `dry_run` is `true`, no filesystem changes are made.
pub(super) fn uninstall_wrappers_in(dir: &Path, dry_run: bool) -> anyhow::Result<UninstallResult> {
    // Circuit breaker: ~/.skim/bin/ is managed entirely by skim and should
    // never contain more entries than ~2× the number of wrapper targets (~59).
    // A count well above that indicates a corrupted or adversarial directory.
    const MAX_ENTRIES: usize = 128;

    let mut result = UninstallResult::default();

    if !dir.exists() {
        return Ok(result);
    }

    let entries =
        std::fs::read_dir(dir).map_err(|e| anyhow::anyhow!("read {}: {}", dir.display(), e))?;

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
        classify_and_remove_entry(&entry.path(), dry_run, &mut result)?;
    }

    // Remove the directory if it is now empty.
    if !dry_run
        && let Ok(mut remaining) = std::fs::read_dir(dir)
        && remaining.next().is_none()
    {
        let _ = std::fs::remove_dir(dir);
        result.dir_removed = true;
    }

    Ok(result)
}

/// Classify a single directory entry and remove it if it is a skim-pointing symlink.
///
/// Three outcomes:
/// - Not a symlink (or stat error) → `result.preserved` incremented, no removal.
/// - Symlink whose target stem is not `"skim"` or `"rskim"` → `result.preserved`
///   incremented, no removal.
/// - Symlink whose target stem is `"skim"` or `"rskim"` → removal (or dry-run
///   report) and `result.removed` incremented.
///
/// Non-UTF-8 symlink targets are treated as non-skim (preserved) and emit a
/// debug-level warning so the edge case is visible without surfacing noise in
/// normal operation.
fn classify_and_remove_entry(
    path: &Path,
    dry_run: bool,
    result: &mut UninstallResult,
) -> anyhow::Result<()> {
    // Only process symlinks; preserve anything else.
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => {
            result.preserved += 1;
            return Ok(());
        }
    };

    if !meta.file_type().is_symlink() {
        result.preserved += 1;
        return Ok(());
    }

    // Only remove symlinks whose target filename stem is exactly "skim" or "rskim".
    // Using file_stem() (not file_name()) strips extensions such as ".exe" on
    // Windows, matching the behaviour of extract_argv0_stem() in main.rs.
    // Substring matching (e.g. contains("skim")) would incorrectly remove
    // symlinks pointing to binaries like `/usr/local/bin/someskimmer`.
    let target = std::fs::read_link(path)
        .map_err(|e| anyhow::anyhow!("read_link {}: {}", path.display(), e))?;
    let stem = match target.file_stem().and_then(|s| s.to_str()) {
        Some(s) => s,
        None => {
            // Non-UTF-8 path component — cannot match "skim"/"rskim"; preserve.
            // Emit a debug warning so non-UTF-8 targets are diagnosable.
            if std::env::var_os("SKIM_DEBUG").is_some() {
                eprintln!(
                    "  debug: preserving {} — target {:?} has non-UTF-8 stem",
                    path.display(),
                    target
                );
            }
            result.preserved += 1;
            return Ok(());
        }
    };

    if stem != "skim" && stem != "rskim" {
        result.preserved += 1;
        return Ok(());
    }

    // This is a skim-pointing symlink — remove it.
    if dry_run {
        println!("  [dry-run] Would remove: {}", path.display());
    } else {
        std::fs::remove_file(path)
            .map_err(|e| anyhow::anyhow!("remove {}: {}", path.display(), e))?;
    }
    result.removed += 1;
    Ok(())
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
        // install_wrappers_in creates the directory itself — don't pre-create it.
        let install_dir = tmp.path().join(".skim").join("bin");

        let result = install_wrappers_in(&install_dir, &fake_skim, false).unwrap();

        // Every wrapper target must be created.
        let targets = wrapper_targets();
        assert_eq!(
            result.created,
            targets.len(),
            "install_wrappers_in must create a symlink for each wrapper target"
        );
        assert_eq!(result.skipped_correct, 0);
        assert_eq!(result.updated, 0);
        assert_eq!(result.skipped_non_symlink, 0);

        // Each symlink must exist and point to the skim binary.
        for &tool in targets {
            let link = install_dir.join(tool);
            assert!(link.is_symlink(), "symlink for {tool} must exist");
            assert_eq!(
                std::fs::read_link(&link).unwrap(),
                fake_skim,
                "symlink for {tool} must point to skim binary"
            );
        }
    }

    /// Calling install_wrappers_in twice is idempotent: the second call skips
    /// every already-correct symlink and creates nothing new.
    #[cfg(unix)]
    #[test]
    fn test_install_wrappers_in_idempotent() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let fake_skim = tmp.path().join("skim");
        std::fs::write(&fake_skim, "").unwrap();
        let install_dir = tmp.path().join("bin");

        // First install.
        let r1 = install_wrappers_in(&install_dir, &fake_skim, false).unwrap();
        assert!(r1.created > 0, "first install must create symlinks");

        // Second install — all symlinks are already correct.
        let r2 = install_wrappers_in(&install_dir, &fake_skim, false).unwrap();
        assert_eq!(
            r2.skipped_correct,
            wrapper_targets().len(),
            "second install must skip all already-correct symlinks"
        );
        assert_eq!(r2.created, 0, "second install must create nothing new");
        assert_eq!(r2.updated, 0, "second install must update nothing");
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

        // uninstall_wrappers_in uses exact stem matching ("skim" / "rskim"), not
        // substring matching, so symlinks to binaries like `someskimmer` are safe.
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

        // Call the real function with the temp directory.
        let result = uninstall_wrappers_in(&bin_dir, false).unwrap();

        assert_eq!(
            result.removed, 1,
            "only the skim-pointing symlink must be removed"
        );
        assert_eq!(result.preserved, 1, "non-skim symlink must be preserved");

        // The skim-pointing symlink must be gone.
        assert!(
            !skim_link.exists() && !skim_link.is_symlink(),
            "skim symlink must have been removed"
        );
        // The non-skim symlink must still be there.
        assert!(
            other_link.is_symlink(),
            "non-skim symlink must be preserved"
        );
    }

    /// uninstall_wrappers_in removes the directory when it becomes empty.
    #[cfg(unix)]
    #[test]
    fn test_uninstall_removes_empty_dir() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let fake_skim = tmp.path().join("skim");
        std::fs::write(&fake_skim, "").unwrap();
        let link = bin_dir.join("git");
        std::os::unix::fs::symlink(&fake_skim, &link).unwrap();

        let result = uninstall_wrappers_in(&bin_dir, false).unwrap();

        assert_eq!(result.removed, 1);
        assert!(result.dir_removed, "empty directory must be removed");
        assert!(!bin_dir.exists(), "bin_dir must no longer exist");
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

        let result = uninstall_wrappers_in(&bin_dir, false).unwrap();

        assert_eq!(
            result.removed, 0,
            "someskimmer-pointing symlink must be preserved"
        );
        assert_eq!(result.preserved, 1, "non-skim symlink must be preserved");
        assert!(link.is_symlink(), "the symlink must still exist");
    }

    /// uninstall_wrappers_in removes symlinks pointing to a file named "rskim".
    ///
    /// The binary is published as `rskim` on crates.io. Users who installed via
    /// `cargo install rskim` will have symlinks targeting a file named "rskim",
    /// not "skim". The uninstaller must accept both names.
    #[cfg(unix)]
    #[test]
    fn test_uninstall_removes_rskim_pointed_symlinks() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        // Target file named "rskim" (cargo-installed binary name).
        let fake_rskim = tmp.path().join("rskim");
        std::fs::write(&fake_rskim, "").unwrap();

        let link = bin_dir.join("git");
        std::os::unix::fs::symlink(&fake_rskim, &link).unwrap();

        let result = uninstall_wrappers_in(&bin_dir, false).unwrap();

        assert_eq!(
            result.removed, 1,
            "symlink pointing to 'rskim' binary must be removed"
        );
        assert_eq!(result.preserved, 0);
        assert!(
            !link.exists() && !link.is_symlink(),
            "the rskim-pointing symlink must have been removed"
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

    /// uninstall_wrappers_in returns an error when the directory contains more
    /// than MAX_ENTRIES (128) files. This circuit breaker prevents unbounded
    /// iteration on corrupted or adversarial directories.
    #[cfg(unix)]
    #[test]
    fn test_uninstall_circuit_breaker_fires_above_128_entries() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        // Create 129 files — one above the MAX_ENTRIES limit.
        for i in 0..=128usize {
            std::fs::write(bin_dir.join(format!("file_{i}")), "").unwrap();
        }

        let result = uninstall_wrappers_in(&bin_dir, false);
        assert!(
            result.is_err(),
            "directories with >128 entries must trigger the circuit breaker"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("128"),
            "error message must mention the 128 entry limit: {err}"
        );
    }

    /// classify_and_remove_entry uses file_stem() so that "skim.exe" on Windows
    /// matches "skim". Regression guard: file_name() would return "skim.exe"
    /// which does NOT equal "skim", breaking uninstall on Windows.
    #[cfg(unix)]
    #[test]
    fn test_uninstall_removes_symlink_to_skim_exe() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        // Simulate a Windows-style binary name as the symlink target.
        let fake_skim_exe = tmp.path().join("skim.exe");
        std::fs::write(&fake_skim_exe, "").unwrap();

        let link = bin_dir.join("git");
        std::os::unix::fs::symlink(&fake_skim_exe, &link).unwrap();

        let result = uninstall_wrappers_in(&bin_dir, false).unwrap();

        assert_eq!(
            result.removed, 1,
            "symlink pointing to 'skim.exe' must be removed (file_stem strips .exe)"
        );
        assert_eq!(result.preserved, 0);
        assert!(
            !link.exists() && !link.is_symlink(),
            "the skim.exe-pointing symlink must have been removed"
        );
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
