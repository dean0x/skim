//! Guidance injection and removal for `skim init`.
//!
//! Handles reading, writing, and updating the skim guidance section in agent
//! instruction files (CLAUDE.md, .cursorrules, skim.mdc, etc.).

use std::path::{Path, PathBuf};

use anyhow::Context as _;

use super::guidance_path::{
    GlobalPathRefusal, GuidanceScope, PathRefusal, ResolvedPath, require_absolute_global,
    resolve_guidance_path,
};
use super::helpers::check_mark;
use crate::cmd::safe_fs::{self, ReadFailure};
use crate::cmd::session::{AgentKind, InstructionEnv};

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

// ============================================================================
// The guidance target
// ============================================================================

/// The text layout of a guidance file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GuidanceFormat {
    /// A Markdown file: CLAUDE.md, AGENTS.md, GEMINI.md and the Copilot file.
    Markdown,
    /// A Cursor rule file (`.mdc`), which skim owns.
    Mdc,
}

impl GuidanceFormat {
    fn of(path: &Path) -> Self {
        if path.extension().is_some_and(|ext| ext == "mdc") {
            Self::Mdc
        } else {
            Self::Markdown
        }
    }
}

/// A guidance file that skim may read and write: where it is, how to show it,
/// and what skim writes to it.
///
/// [`guidance_target`] is the one place that builds it. The install gate, the
/// writer, the remover and the dry-run all use it, so they agree on the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GuidanceTarget {
    /// The file to open and write. Absolute. A link at the file name is
    /// already followed.
    pub(super) path: PathBuf,
    /// The path to show in messages.
    pub(super) shown: PathBuf,
    /// The layout of the file, taken from the resolved `path`.
    pub(super) format: GuidanceFormat,
}

impl GuidanceTarget {
    /// The text skim writes to this file for `version`.
    ///
    /// This is the one place that chooses the text. It looks at the resolved
    /// path of the file, so two agents that share a file get the same text.
    pub(super) fn render(&self, version: &str) -> String {
        match self.format {
            GuidanceFormat::Markdown => super::helpers::guidance_content(version),
            GuidanceFormat::Mdc => super::helpers::guidance_content_mdc(version),
        }
    }
}

/// Why [`guidance_target`] gives no target.
#[derive(Debug)]
pub(super) enum TargetRefusal {
    /// A global path or a config-folder variable is wrong. The command stops
    /// before it writes anything.
    Stop(GlobalPathRefusal),
    /// This one file is left alone. The command goes on without it.
    Skip(PathRefusal),
}

impl TargetRefusal {
    /// Report the refusal. A skip prints one `Note:` line that names the file
    /// and returns `Ok`. A stop returns the error.
    pub(super) fn report(self) -> anyhow::Result<()> {
        match self {
            Self::Skip(refusal) => {
                println!("  Note: {refusal}");
                Ok(())
            }
            Self::Stop(refusal) => Err(refusal.into()),
        }
    }
}

/// The guidance file of `agent` in `scope`, after the path policy.
///
/// Returns `Ok(None)` when the agent has no guidance file in that scope
/// (Cursor has no global one). Reads the file system for the checks and changes
/// nothing.
pub(super) fn guidance_target(
    agent: AgentKind,
    scope: &GuidanceScope,
    env: &InstructionEnv,
) -> Result<Option<GuidanceTarget>, TargetRefusal> {
    let global = matches!(scope, GuidanceScope::Global);
    let Some(path) = agent.instruction_file(global, env) else {
        return Ok(None);
    };
    if global {
        require_absolute_global(agent, &path, env).map_err(TargetRefusal::Stop)?;
    }
    let ResolvedPath { path, shown } =
        resolve_guidance_path(&path, scope).map_err(TargetRefusal::Skip)?;
    let format = GuidanceFormat::of(&path);
    Ok(Some(GuidanceTarget {
        path,
        shown,
        format,
    }))
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
    let Some(target) = writable_target(agent, global, env)? else {
        return Ok(()); // refused: the note is printed, the file is untouched
    };
    let path = target.path.as_path();

    let version = env!("CARGO_PKG_VERSION");
    let new_content = target.render(version);

    if path.exists() {
        let existing = match read_existing_safely(path)? {
            Some(s) => s,
            None => return Ok(()), // soft skip (too large or unreadable)
        };

        if update_existing_guidance(path, &existing, version, &new_content)? {
            return Ok(());
        }
    } else {
        // File doesn't exist — create.
        guidance_create(path, &new_content)?;
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
        target.shown.display()
    );

    // For project scope, remind user to commit
    if !global {
        println!(
            "  Note: guidance added to {} — commit to share with your team.",
            target.shown.display()
        );
    }

    Ok(())
}

/// The target `inject_guidance` writes.
///
/// Returns `Ok(None)` after it printed a note when the path policy leaves the
/// file alone. Returns an error when a global path is wrong. An agent with no
/// global file (Cursor) uses its project file.
fn writable_target(
    agent: AgentKind,
    global: bool,
    env: &InstructionEnv,
) -> anyhow::Result<Option<GuidanceTarget>> {
    let scope = GuidanceScope::for_flag(global)?;
    let found = match guidance_target(agent, &scope, env) {
        Ok(None) if global => {
            eprintln!(
                "  {} does not support global guidance. Using project scope.",
                agent.display_name()
            );
            guidance_target(agent, &GuidanceScope::for_flag(false)?, env)
        }
        other => other,
    };
    match found {
        Ok(Some(target)) => Ok(Some(target)),
        Ok(None) => anyhow::bail!("No instruction file for {}", agent.display_name()),
        Err(refusal) => refusal.report().map(|()| None),
    }
}

/// Remove skim guidance section from the agent's main instruction file.
pub(super) fn remove_guidance(
    agent: AgentKind,
    global: bool,
    env: &InstructionEnv,
) -> anyhow::Result<()> {
    let scope = GuidanceScope::for_flag(global)?;
    let target = match guidance_target(agent, &scope, env) {
        Ok(target) => target,
        Err(refusal) => {
            refusal.report()?;
            None
        }
    };

    // A missing file has nothing to remove. A refused file is left alone.
    if let Some(target) = target.filter(|t| t.path.exists()) {
        strip_target(&target)?;
    }

    // Also clean legacy .cursorrules for Cursor
    if agent == AgentKind::Cursor {
        clean_legacy_cursorrules()?;
    }

    Ok(())
}

/// Remove the skim section from the file of `target`.
///
/// A file that holds only the skim section (and every `.mdc` file, which skim
/// owns) is deleted. Any other file is rewritten without the section.
fn strip_target(target: &GuidanceTarget) -> anyhow::Result<()> {
    let path = target.path.as_path();
    let content = match read_existing_safely(path)? {
        Some(s) => s,
        None => return Ok(()), // soft skip (too large or unreadable)
    };
    if let Some(stripped) = strip_skim_section(&content) {
        if target.format == GuidanceFormat::Mdc || stripped.is_empty() {
            std::fs::remove_file(path)?;
        } else {
            atomic_write_stripped(path, &stripped)?;
        }
        println!(
            "  {} Removed guidance from {}",
            check_mark(true),
            target.shown.display()
        );
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
    // The file is in the project folder, so the project link rules apply.
    let scope = GuidanceScope::for_flag(false)?;
    let legacy = match resolve_guidance_path(Path::new(".cursorrules"), &scope) {
        Ok(resolved) => resolved,
        Err(refusal) => {
            println!("  Note: {refusal}");
            return Ok(());
        }
    };
    if !legacy.path.exists() {
        return Ok(());
    }
    // Route through read_existing_safely to apply the same bounded read
    // that protects all other instruction file reads.
    let content = match read_existing_safely(&legacy.path)? {
        Some(s) => s,
        None => return Ok(()), // file too large or unreadable — skip silently
    };
    if let Some(cleaned) = strip_skim_section(&content) {
        // Leave the file in place even when cleaned is empty (user may own it).
        atomic_write_stripped(&legacy.path, &cleaned)?;
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
