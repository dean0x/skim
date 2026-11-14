//! Guidance injection and removal for `skim init`.
//!
//! Handles reading, writing, and updating the skim guidance section in agent
//! instruction files (CLAUDE.md, .cursorrules, skim.mdc, etc.). The shape of a
//! file and the currency of its block are decided in `guidance_block.rs`.

use std::path::{Path, PathBuf};

use anyhow::Context as _;

use super::guidance_block::{
    BlockStatus, GuidanceFormat, GuidanceHooks, block_status, skim_shaped, splice, strip_section,
    strip_skim_section,
};
use super::guidance_path::{
    GlobalPathRefusal, GuidanceScope, PathRefusal, ResolvedPath, require_absolute_global,
    resolve_guidance_path,
};
use super::helpers::{check_mark, guidance_content, render_file};
use crate::cmd::hooks::{HookSupport, protocol_for_agent};
use crate::cmd::safe_fs::{self, ReadFailure};
use crate::cmd::session::{AgentKind, InstructionEnv};

/// Maximum byte size for an instruction file before we skip reading it.
/// Prevents unbounded allocations on corrupted or adversarially crafted files.
pub(super) const MAX_INSTRUCTION_FILE_SIZE: u64 = 1_048_576; // 1 MiB

// ============================================================================
// The guidance target
// ============================================================================

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
    /// The layout and the text of the file, taken from the resolved `path`.
    pub(super) format: GuidanceFormat,
}

impl GuidanceTarget {
    /// The text skim writes to a new file for `version`: the frontmatter of a
    /// file skim owns, then the block.
    ///
    /// This is the one place that chooses the text. It looks at the resolved
    /// path of the file, so two agents that share a file get the same text.
    pub(super) fn render(&self, version: &str) -> String {
        render_file(&self.format, version)
    }

    /// The block skim writes to this file for `version`, without frontmatter.
    pub(super) fn block(&self, version: &str) -> String {
        guidance_content(version, self.format.hooks)
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
    let Some(ResolvedPath { path, shown }) = resolved_path(agent, scope, env)? else {
        return Ok(None);
    };
    let format = GuidanceFormat::for_path(&path, hooks_for_file(&path, scope, env));
    Ok(Some(GuidanceTarget {
        path,
        shown,
        format,
    }))
}

/// The file of `agent` in `scope`, resolved by the path policy.
fn resolved_path(
    agent: AgentKind,
    scope: &GuidanceScope,
    env: &InstructionEnv,
) -> Result<Option<ResolvedPath>, TargetRefusal> {
    let global = matches!(scope, GuidanceScope::Global);
    let Some(path) = agent.instruction_file(global, env) else {
        return Ok(None);
    };
    if global {
        require_absolute_global(agent, &path, env).map_err(TargetRefusal::Stop)?;
    }
    resolve_guidance_path(&path, scope)
        .map(Some)
        .map_err(TargetRefusal::Skip)
}

/// Whether the text of the file at the resolved `path` may speak of a skim
/// hook.
///
/// It may not when any agent that reads the file has no hook. The test is on
/// the resolved path, so Codex (no hook) and Crush (a hook) share one
/// `AGENTS.md` and never rewrite each other's block, and a `CLAUDE.md` that is
/// a link to `AGENTS.md` gets the same text as `AGENTS.md`. It does not depend
/// on which agents are installed.
fn hooks_for_file(path: &Path, scope: &GuidanceScope, env: &InstructionEnv) -> GuidanceHooks {
    let hookless_reader = AgentKind::all_supported().iter().any(|&other| {
        protocol_for_agent(other).hook_support() == HookSupport::AwarenessOnly
            && matches!(
                resolved_path(other, scope, env),
                Ok(Some(resolved)) if resolved.path == path
            )
    });
    if hookless_reader {
        GuidanceHooks::NoHook
    } else {
        GuidanceHooks::Hook
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
pub(super) fn guidance_create(path: &Path, new_content: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write_stripped(path, &format!("{new_content}\n"))
}

/// Append `new_content` to the end of `existing` (append mode).
pub(super) fn guidance_append(
    path: &Path,
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

/// Why [`update_existing_guidance`] left a file as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LeftReason {
    /// A start marker with no end marker after it.
    BrokenMarkers,
    /// A file that skim owns, with no skim markers: its text is not skim's.
    NoMarkers,
}

/// What [`update_existing_guidance`] did to a file that exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UpdateOutcome {
    /// The block is current. Nothing was written.
    AlreadyCurrent,
    /// The block was replaced. `changed` is `true` when the marker named this
    /// version and the text differed, `false` when the version differed.
    Replaced { changed: bool },
    /// The file is as it was.
    Left(LeftReason),
    /// The file had no block, so one was appended. The caller prints the
    /// install footer.
    Appended,
}

impl UpdateOutcome {
    /// The line that tells the user what happened to the file shown as
    /// `shown`, or `None` when the caller prints the footer.
    pub(super) fn message(&self, shown: &Path, version: &str) -> Option<String> {
        let shown = shown.display();
        match self {
            Self::AlreadyCurrent => Some(format!(
                "  {} Guidance already current (v{version})",
                check_mark(true)
            )),
            Self::Replaced { changed: false } => Some(format!(
                "  {} Updated guidance in {shown} (-> v{version})",
                check_mark(true)
            )),
            Self::Replaced { changed: true } => Some(format!(
                "  Note: Replaced changed guidance in {shown}; review the diff before a commit"
            )),
            Self::Left(LeftReason::BrokenMarkers) => Some(format!(
                "  warning: skim markers in {shown} appear corrupted (skipping guidance update)"
            )),
            Self::Left(LeftReason::NoMarkers) => Some(format!(
                "  Note: {shown} holds no skim markers, so skim leaves it unchanged. Move it aside to let skim write it."
            )),
            Self::Appended => None,
        }
    }

    fn print(&self, shown: &Path, version: &str) {
        match self.message(shown, version) {
            Some(line) if matches!(self, Self::Left(LeftReason::BrokenMarkers)) => {
                eprintln!("{line}");
            }
            Some(line) => println!("{line}"),
            None => {}
        }
    }
}

/// Handle guidance update for a file that already exists on disk.
///
/// The block is compared with `block_status`. A stale or changed block is
/// replaced; a current one, a broken one and an owned file with no markers are
/// left alone; a file with no block gets one appended.
pub(super) fn update_existing_guidance(
    target: &GuidanceTarget,
    existing: &str,
    version: &str,
) -> anyhow::Result<UpdateOutcome> {
    let block = target.block(version);
    let changed = match block_status(existing, &block, &target.format) {
        BlockStatus::Current => return Ok(UpdateOutcome::AlreadyCurrent),
        BlockStatus::Broken => return Ok(UpdateOutcome::Left(LeftReason::BrokenMarkers)),
        BlockStatus::Absent if target.format.owned => {
            return Ok(UpdateOutcome::Left(LeftReason::NoMarkers));
        }
        BlockStatus::Absent => {
            guidance_append(&target.path, existing, &block)?;
            return Ok(UpdateOutcome::Appended);
        }
        BlockStatus::Stale { .. } => false,
        BlockStatus::Changed => true,
    };
    let Some(updated) = splice(existing, &block, &target.format) else {
        return Ok(UpdateOutcome::Left(LeftReason::BrokenMarkers));
    };
    atomic_write_stripped(&target.path, &updated)?;
    Ok(UpdateOutcome::Replaced { changed })
}

/// Inject skim guidance section into the agent's main instruction file.
///
/// Four modes:
/// - **Create**: File doesn't exist → create with just the guidance section
/// - **Append**: File exists but has no skim section → append to end
/// - **Update**: File has a skim section that is not current → replace it
/// - **Skip**: File has a skim section that is current → idempotent no-op
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

    if path.exists() {
        let existing = match read_existing_safely(path)? {
            Some(s) => s,
            None => return Ok(()), // soft skip (too large or unreadable)
        };

        let outcome = update_existing_guidance(&target, &existing, version)?;
        outcome.print(&target.shown, version);
        if outcome != UpdateOutcome::Appended {
            return Ok(());
        }
    } else {
        // File doesn't exist — create.
        guidance_create(path, &target.render(version))?;
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
/// A file that is skim-shaped (it holds only the skim section, and for a file
/// skim owns, its frontmatter) is deleted. Any other file is rewritten without
/// the section, and the text of the user stays. A file with no section, or a
/// broken one, is left as it is.
fn strip_target(target: &GuidanceTarget) -> anyhow::Result<()> {
    let path = target.path.as_path();
    let content = match read_existing_safely(path)? {
        Some(s) => s,
        None => return Ok(()), // soft skip (too large or unreadable)
    };
    if let Some(stripped) = strip_section(&content, target.format.frontmatter) {
        if skim_shaped(&content, &target.format) {
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
