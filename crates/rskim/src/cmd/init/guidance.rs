//! Guidance injection and removal for `skim init`.
//!
//! Handles reading, writing, and updating the skim guidance section in agent
//! instruction files (CLAUDE.md, .cursorrules, skim.mdc, etc.).

use std::path::Path;

use anyhow::Context as _;

use super::helpers::check_mark;
use crate::cmd::safe_fs::{self, ReadFailure};
use crate::cmd::session::{AgentKind, InstructionEnv, RelativeOverride};

pub(super) const GUIDANCE_START: &str = "<!-- skim-start";
pub(super) const GUIDANCE_END: &str = "<!-- skim-end -->";

/// Maximum byte size for an instruction file before we skip reading it.
/// Prevents unbounded allocations on corrupted or adversarially crafted files.
pub(super) const MAX_INSTRUCTION_FILE_SIZE: u64 = 1_048_576; // 1 MiB

/// Find the skim guidance section markers in content.
/// Returns `Some((start_byte, end_byte))` where end_byte includes the end marker.
/// Returns `None` if markers are missing or in wrong order (corrupted file).
pub(super) fn find_skim_section(content: &str) -> Option<(usize, usize)> {
    let start = content.find(GUIDANCE_START)?;
    let end_marker = content.find(GUIDANCE_END)?;
    if start >= end_marker {
        return None; // Markers in wrong order
    }
    Some((start, end_marker + GUIDANCE_END.len()))
}

/// Why a global guidance path is refused.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum GlobalPathRefusal {
    /// A config-folder variable holds a relative path. The refusal names it.
    RelativeOverride(RelativeOverride),
    /// The path is relative, and no variable is the cause (a relative home folder).
    NotAbsolute(std::path::PathBuf),
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
    path: &std::path::Path,
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

/// Resolve the instruction file path for `agent`, falling back from global to
/// project scope when the agent does not support a global instruction file.
///
/// A global path must be absolute; a relative one is refused, with the cause named.
pub(super) fn resolve_instruction_path(
    agent: AgentKind,
    global: bool,
    env: &InstructionEnv,
) -> anyhow::Result<std::path::PathBuf> {
    match agent.instruction_file(global, env) {
        Some(p) => {
            if global {
                require_absolute_global(agent, &p, env)?;
            }
            Ok(p)
        }
        None if global => {
            eprintln!(
                "  {} does not support global guidance. Using project scope.",
                agent.display_name()
            );
            agent
                .instruction_file(false, env)
                .ok_or_else(|| anyhow::anyhow!("No instruction file for {}", agent.display_name()))
        }
        None => anyhow::bail!("No instruction file for {}", agent.display_name()),
    }
}

/// Read the instruction file at `path` as strict UTF-8, within
/// [`MAX_INSTRUCTION_FILE_SIZE`].
///
/// The open follows no link at the file name and does not block on a FIFO, and
/// the opened handle must be a regular file. Returns `Ok(None)` when the file
/// is missing (no warning), and `Ok(None)` after a warning when it must be
/// skipped: too large, not a regular file, not UTF-8, or unreadable. A skip is
/// a soft warning, not a hard error, and the file is left as it is.
pub(super) fn read_existing_safely(path: &Path) -> anyhow::Result<Option<String>> {
    let read = safe_fs::read_regular_bounded(path, MAX_INSTRUCTION_FILE_SIZE)
        .and_then(safe_fs::BoundedFile::into_text);
    match read {
        Ok((text, _permissions)) => Ok(Some(text)),
        Err(ReadFailure::Missing) => Ok(None),
        Err(failure) => {
            eprintln!(
                "  warning: {} {} (skipping guidance)",
                path.display(),
                skip_reason(&failure)
            );
            Ok(None)
        }
    }
}

/// The clause that says why a file is skipped.
fn skip_reason(failure: &ReadFailure) -> String {
    match failure {
        ReadFailure::TooLarge => {
            format!("is too large (over {MAX_INSTRUCTION_FILE_SIZE} bytes)")
        }
        ReadFailure::NotUtf8 => "is not valid UTF-8".to_string(),
        ReadFailure::Folder | ReadFailure::NotRegular => "is not a regular file".to_string(),
        ReadFailure::Symlink => "is a link".to_string(),
        ReadFailure::Missing => "is missing".to_string(),
        ReadFailure::Open(e) | ReadFailure::Inspect(e) | ReadFailure::Read(e) => {
            format!("could not be read: {e}")
        }
    }
}

/// Write `new_content` as a new instruction file at `path` (create mode).
pub(super) fn guidance_create(path: &std::path::Path, new_content: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write_stripped(path, &format!("{new_content}\n"))
}

/// Replace the existing skim section in `existing` with `new_content` (update mode).
pub(super) fn guidance_update(
    path: &std::path::Path,
    existing: &str,
    start: usize,
    end: usize,
    new_content: &str,
) -> anyhow::Result<()> {
    let updated = format!("{}{}{}", &existing[..start], new_content, &existing[end..]);
    atomic_write_stripped(path, &updated)
}

/// Append `new_content` to the end of `existing` (append mode).
pub(super) fn guidance_append(
    path: &std::path::Path,
    existing: &str,
    new_content: &str,
) -> anyhow::Result<()> {
    let mut content = existing.to_owned();
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push('\n');
    content.push_str(new_content);
    content.push('\n');
    atomic_write_stripped(path, &content)
}

/// Handle guidance update for a file that already exists on disk.
///
/// Returns `Ok(true)` when the update is complete and no further action is
/// needed (skip or in-place update).  Returns `Ok(false)` when the caller
/// should proceed with the post-write footer prints (append path).
pub(super) fn update_existing_guidance(
    path: &std::path::Path,
    existing: &str,
    version: &str,
    new_content: &str,
) -> anyhow::Result<bool> {
    // Detect corrupted markers (present but in wrong order)
    if find_skim_section(existing).is_none() && existing.contains(GUIDANCE_START) {
        eprintln!(
            "  warning: skim markers in {} appear corrupted (skipping guidance update)",
            path.display()
        );
        return Ok(true); // treated as done — do not attempt write
    }

    if let Some((start, end)) = find_skim_section(existing) {
        // Same version? Skip.
        if existing[start..end].contains(&format!("v{version}")) {
            println!(
                "  {} Guidance already current (v{})",
                check_mark(true),
                version
            );
            return Ok(true);
        }

        // Different version — update in place.
        guidance_update(path, existing, start, end, new_content)?;
        println!(
            "  {} Updated guidance in {} (-> v{})",
            check_mark(true),
            path.display(),
            version
        );
        return Ok(true);
    }

    // No skim section — append (caller prints footer).
    guidance_append(path, existing, new_content)?;
    Ok(false)
}

/// Inject skim guidance section into the agent's main instruction file.
///
/// Four modes:
/// - **Create**: File doesn't exist → create with just the guidance section
/// - **Append**: File exists but has no skim section → append to end
/// - **Update**: File has a skim section with older version → replace in place
/// - **Skip**: File has a skim section with current version → idempotent no-op
pub(super) fn inject_guidance(
    agent: AgentKind,
    global: bool,
    env: &InstructionEnv,
) -> anyhow::Result<()> {
    let path = resolve_instruction_path(agent, global, env)?;
    let path = super::helpers::resolve_real_settings_path(&path)?;

    let version = env!("CARGO_PKG_VERSION");
    let is_mdc = path.extension().is_some_and(|ext| ext == "mdc");
    let new_content = if is_mdc {
        super::helpers::guidance_content_mdc(version)
    } else {
        super::helpers::guidance_content(version)
    };

    if path.exists() {
        let existing = match read_existing_safely(&path)? {
            Some(s) => s,
            None => return Ok(()), // soft skip (too large or unreadable)
        };

        if update_existing_guidance(&path, &existing, version, &new_content)? {
            return Ok(());
        }
    } else {
        // File doesn't exist — create.
        guidance_create(&path, &new_content)?;
    }

    // Legacy cleanup: remove skim markers from .cursorrules when writing skim.mdc.
    // Use file_name() rather than contains() to avoid false positives on paths
    // like /some/skim.mdc-backup/CLAUDE.md.
    if path.file_name().is_some_and(|n| n == "skim.mdc") {
        clean_legacy_cursorrules()?;
    }

    println!(
        "  {} Installed guidance in {}",
        check_mark(true),
        path.display()
    );

    // For project scope, remind user to commit
    if !global {
        println!(
            "  Note: guidance added to {} — commit to share with your team.",
            path.display()
        );
    }

    Ok(())
}

/// Remove skim guidance section from the agent's main instruction file.
pub(super) fn remove_guidance(
    agent: AgentKind,
    global: bool,
    env: &InstructionEnv,
) -> anyhow::Result<()> {
    let candidate = agent.instruction_file(global, env);
    if global && let Some(p) = &candidate {
        require_absolute_global(agent, p, env)?;
    }
    let path = match candidate {
        Some(p) if p.exists() => p,
        _ => {
            // For Cursor, even if the new path doesn't exist, check legacy .cursorrules
            if agent == AgentKind::Cursor {
                clean_legacy_cursorrules()?;
            }
            return Ok(());
        }
    };

    // Resolve symlinks before operating on the path so writes target the real
    // file, not a dangling or redirected symlink (mirrors inject_guidance).
    let path = super::helpers::resolve_real_settings_path(&path)?;

    let content = match read_existing_safely(&path)? {
        Some(s) => s,
        None => return Ok(()), // soft skip (too large or unreadable)
    };
    if let Some(stripped) = strip_skim_section(&content) {
        if path.extension().is_some_and(|ext| ext == "mdc") {
            // Skim owns .mdc files entirely — delete on removal
            std::fs::remove_file(&path)?;
        } else if stripped.is_empty() {
            // File was only the skim section — delete the file
            std::fs::remove_file(&path)?;
        } else {
            // Atomic write using dynamic extension (issue 10)
            atomic_write_stripped(&path, &stripped)?;
        }
        println!(
            "  {} Removed guidance from {}",
            check_mark(true),
            path.display()
        );
    }

    // Also clean legacy .cursorrules for Cursor
    if agent == AgentKind::Cursor {
        clean_legacy_cursorrules()?;
    }

    Ok(())
}

/// Remove the skim section from `content`, stripping surrounding blank lines.
///
/// Returns `None` if no skim section was found.
/// Returns `Some(cleaned)` where `cleaned` is the trimmed remainder with a
/// trailing newline appended when non-empty.
pub(super) fn strip_skim_section(content: &str) -> Option<String> {
    let (start, end) = find_skim_section(content)?;
    let before = content[..start].trim_end_matches('\n');
    let after = &content[end..];
    let combined = format!("{before}{after}");
    let trimmed = combined.trim();
    Some(if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    })
}

/// Write `content` to the guidance file at `path`: the one writer of every
/// guidance file.
///
/// The write goes through [`safe_fs::write_atomic`]: a random temporary name in
/// the same folder, created exclusively, then renamed over `path`. A file that
/// already exists keeps its mode. A new file is created with mode 0600.
pub(super) fn atomic_write_stripped(path: &Path, content: &str) -> anyhow::Result<()> {
    let permissions = match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => Some(meta.permissions()),
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(e)
                .with_context(|| format!("could not read the mode of {}", path.display()));
        }
    };
    safe_fs::write_atomic(path, content, permissions)
        .with_context(|| format!("could not write {}", path.display()))
}

/// Clean up skim markers from legacy `.cursorrules` during Cursor migration.
///
/// Leaves the file in place (even if empty) since the user may have created it
/// intentionally. Only removes the skim section markers.
pub(super) fn clean_legacy_cursorrules() -> anyhow::Result<()> {
    let legacy = std::path::PathBuf::from(".cursorrules");
    if !legacy.exists() {
        return Ok(());
    }
    // S2: apply resolve_real_settings_path so symlinks are handled consistently
    let legacy = super::helpers::resolve_real_settings_path(&legacy)?;
    // Route through read_existing_safely to apply the 1 MiB size guard
    // that protects all other instruction file reads.
    let content = match read_existing_safely(&legacy)? {
        Some(s) => s,
        None => return Ok(()), // file too large or unreadable — skip silently
    };
    if let Some(cleaned) = strip_skim_section(&content) {
        // Leave the file in place even when cleaned is empty (user may own it).
        atomic_write_stripped(&legacy, &cleaned)?;
        println!("  {} Cleaned legacy .cursorrules markers", check_mark(true));
    }
    Ok(())
}

// ============================================================================
// Tests (co-located in guidance_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "guidance_tests.rs"]
mod tests;
