//! Check hook paths before a project install or uninstall changes them.
//!
//! Folders must resolve inside the project. At a skim-owned file name, only
//! the parent folder is checked: the writer replaces a link, not its target.
//! At a user config file name, one link to a regular file inside the root is
//! allowed. The writer uses the checked path.
//!
//! The folders above a file are checked before the open, not at the open. A
//! folder that another process replaces with a link after the check is
//! followed. The guidance path policy has the same limit.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::guidance_path::{
    GuidanceScope, PathRefusal, check_folder_in_root, resolve_project_at_root,
};
use super::helpers::{HOOK_SCRIPT_NAME, config_backup_name, resolve_real_settings_path};
use crate::cmd::hooks::{copilot::SKIM_JSON_NAME, protocol_for_agent};
use crate::cmd::integrity::manifest_path;
use crate::cmd::permissions::{permissions_protocol_for_agent, sidecar::SIDECAR_FILENAME};
use crate::cmd::session::AgentKind;

pub(super) const CURSOR_LEGACY_SETTINGS: &str = "settings.json";

pub(super) enum SettingsFile {
    Agent,
    CursorLegacy,
}

/// In project scope, use the checked path or stop. Resolve the original path
/// only for a global write.
pub(super) fn settings_write_path(
    scope: Option<&ProjectHookScope>,
    file: SettingsFile,
    path: &Path,
) -> anyhow::Result<PathBuf> {
    let Some(scope) = scope else {
        return resolve_real_settings_path(path);
    };
    let checked = match file {
        SettingsFile::Agent => scope.settings_path(),
        SettingsFile::CursorLegacy => scope.cursor_legacy_path(),
    };
    checked
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("no checked path for project settings"))
}

#[derive(Debug, PartialEq, Eq)]
enum HookTarget {
    Folder(PathBuf),
    Owned(PathBuf),
    UserFile(PathBuf),
}

impl HookTarget {
    fn path(&self) -> &Path {
        match self {
            Self::Folder(path) | Self::Owned(path) | Self::UserFile(path) => path,
        }
    }
}

/// One inventory of the folders and files that project hook writers can change.
/// Keep the agent folder first so a refusal names it before its children.
#[must_use]
fn project_hook_targets(agent: AgentKind) -> Vec<HookTarget> {
    use HookTarget::{Folder, Owned, UserFile};

    let dir = Path::new(agent.dot_dir_name());
    let hooks = dir.join("hooks");
    let mut targets = vec![
        Folder(dir.to_path_buf()),
        Folder(hooks.clone()),
        Owned(hooks.join(HOOK_SCRIPT_NAME)),
        Owned(manifest_path(dir, agent.cli_name())),
    ];

    let hook = protocol_for_agent(agent);
    if hook.uses_dedicated_hook_file() {
        targets.push(Owned(hooks.join(SKIM_JSON_NAME)));
    } else {
        targets.push(UserFile(dir.join(hook.config_filename())));
        targets.push(Owned(dir.join(config_backup_name(agent))));
    }
    if agent == AgentKind::Cursor {
        targets.push(UserFile(dir.join(CURSOR_LEGACY_SETTINGS)));
    }
    if let Some(permissions) = permissions_protocol_for_agent(agent) {
        targets.push(Owned(dir.join(SIDECAR_FILENAME)));
        let config = permissions.config_filename();
        match agent {
            AgentKind::CopilotCli => {
                targets.push(UserFile(dir.join(config)));
                targets.push(Owned(dir.join(format!("{config}.bak"))));
            }
            AgentKind::GeminiCli | AgentKind::CodexCli => {
                let file = dir.join(config);
                if let Some(parent) = file.parent() {
                    targets.push(Folder(parent.to_path_buf()));
                }
                targets.push(Owned(file));
            }
            // Claude uses the same settings file for hooks and permissions.
            AgentKind::ClaudeCode | AgentKind::Cursor | AgentKind::Crush => {}
        }
    }
    targets
}

/// The canonical project root and the checked hook config write paths.
#[derive(Debug)]
pub(super) struct ProjectHookScope {
    root: PathBuf,
    settings: Option<PathBuf>,
    cursor_legacy: Option<PathBuf>,
    copilot_permissions: Option<PathBuf>,
}

impl ProjectHookScope {
    pub(super) fn check(agent: AgentKind, root: &Path) -> Result<Self, PathRefusal> {
        let root = fs::canonicalize(root).map_err(|error| PathRefusal::Unchecked {
            path: PathBuf::from("."),
            error,
        })?;
        Self::check_at_root(agent, root)
    }

    fn check_at_root(agent: AgentKind, root: PathBuf) -> Result<Self, PathRefusal> {
        let mut settings = None;
        let mut cursor_legacy = None;
        let mut copilot_permissions = None;
        let hook = protocol_for_agent(agent);
        for target in project_hook_targets(agent) {
            let relative = target.path();
            debug_assert!(relative.is_relative());
            debug_assert!(
                relative
                    .components()
                    .all(|c| matches!(c, Component::Normal(_)))
            );
            match &target {
                HookTarget::Folder(_) => {
                    check_folder_in_root(&root.join(relative), relative, &root)?;
                }
                HookTarget::Owned(_) => {
                    let parent = relative.parent().unwrap_or(Path::new(""));
                    check_folder_in_root(&root.join(parent), relative, &root)?;
                }
                HookTarget::UserFile(_) => {
                    let checked = resolve_project_at_root(relative, &root)?.path;
                    check_config_file(&root.join(relative), relative)?;
                    if !hook.uses_dedicated_hook_file()
                        && relative.file_name() == Some(hook.config_filename().as_ref())
                    {
                        settings = Some(checked);
                    } else if agent == AgentKind::Cursor
                        && relative.file_name() == Some(CURSOR_LEGACY_SETTINGS.as_ref())
                    {
                        cursor_legacy = Some(checked);
                    } else if agent == AgentKind::CopilotCli {
                        // Copilot's only user file is its permission config.
                        copilot_permissions = Some(checked);
                    }
                }
            }
        }
        Ok(Self {
            root,
            settings,
            cursor_legacy,
            copilot_permissions,
        })
    }

    /// Use the same project root as the guidance path policy.
    pub(super) fn for_current_folder(agent: AgentKind) -> anyhow::Result<Self> {
        match GuidanceScope::for_flag(false)? {
            GuidanceScope::Project { root } => Ok(Self::check(agent, &root)?),
            GuidanceScope::Global => {
                anyhow::bail!("could not check hook paths without a project folder")
            }
        }
    }

    /// Check paths again after consent, without choosing a new root.
    pub(super) fn recheck(&self, agent: AgentKind) -> Result<Self, PathRefusal> {
        Self::check_at_root(agent, self.root.clone())
    }

    #[must_use]
    pub(super) fn settings_path(&self) -> Option<&Path> {
        self.settings.as_deref()
    }

    #[must_use]
    pub(super) fn cursor_legacy_path(&self) -> Option<&Path> {
        self.cursor_legacy.as_deref()
    }

    pub(super) fn permission_config_path(&self, agent: AgentKind) -> anyhow::Result<Option<&Path>> {
        let checked = match agent {
            AgentKind::ClaudeCode => self.settings_path(),
            AgentKind::CopilotCli => self.copilot_permissions.as_deref(),
            _ => return Ok(None),
        };
        let path = checked.ok_or_else(|| anyhow::anyhow!("missing checked permission path"))?;
        Ok(Some(path))
    }
}

/// Readers expect a regular config file. For a link, permission prune also
/// needs the first target to be the file, not another link.
fn check_config_file(file: &Path, shown: &Path) -> Result<(), PathRefusal> {
    let unchecked = |error| PathRefusal::Unchecked {
        path: shown.to_path_buf(),
        error,
    };
    match fs::symlink_metadata(file) {
        Ok(meta) if meta.is_file() => return Ok(()),
        Ok(meta) if meta.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(unchecked(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(unchecked(error)),
    }

    let target = fs::read_link(file).map_err(unchecked)?;
    let first = file.parent().unwrap_or(Path::new("")).join(target);
    let meta = fs::symlink_metadata(first).map_err(unchecked)?;
    if !meta.is_file() {
        return Err(PathRefusal::NotRegular {
            path: shown.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "hook_scope_tests.rs"]
mod tests;
