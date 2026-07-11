//! GitHub Copilot CLI permissions writer.
//!
//! Targets `{copilot_home}/permissions-config.json` with PER-PROJECT keys.
//!
//! ## File format
//!
//! The top-level JSON object is keyed by absolute project-root path. skim writes
//! only under the CURRENT project root determined from `cwd` (git repo root if
//! inside a repo, else refuses). Example:
//!
//! ```json
//! {
//!   "/Users/alice/my-project": {
//!     "allow": ["Bash(skim df:*)", "Bash(skim grep:*)", ...]
//!   }
//! }
//! ```
//!
//! ## Entry format
//!
//! Entries use Claude Code's `Bash(<cmd>:*)` allow syntax (Copilot CLI uses
//! the same JSON permission schema). The entry set is the same 8-tool seed
//! as the Claude Code writer.
//!
//! ## Schema note
//!
//! This schema is validated in principle, pending deferred Copilot CLI e2e in
//! a sandboxed `COPILOT_HOME`. The format closely mirrors Claude Code's
//! `settings.json` permission entries.
//!
//! ## Project root rule
//!
//! `seed`, `remove_seeded`, and `is_current` all determine the project root by
//! walking up from `cwd` looking for `.git` (up to 64 levels). This matches
//! `find_git_root_from_cwd` in install.rs. REFUSE (Err) when the project root
//! is zero/ambiguous (not in a git repo, cwd unreadable, root is filesystem root).

use std::path::Path;

use crate::cmd::init::{MAX_SETTINGS_SIZE, atomic_write_settings, load_or_create_settings};
use crate::cmd::integrity::compute_file_hash;
use crate::cmd::permissions::sidecar::{PermissionSidecar, load_sidecar, write_sidecar};
use crate::cmd::permissions::{PermissionsProtocol, PermissionsTier, RemoveOutcome, SeedOutcome};

/// Config filename (relative to copilot_home).
const CONFIG_FILENAME: &str = "permissions-config.json";

/// Sidecar filename (relative to copilot_home).
const SIDECAR_FILENAME: &str = "skim-permissions.json";

pub(super) struct CopilotPermissions;

impl PermissionsProtocol for CopilotPermissions {
    fn agent_label(&self) -> &str {
        "GitHub Copilot CLI"
    }

    fn config_filename(&self) -> &str {
        CONFIG_FILENAME
    }

    fn render_entry(&self, tool: &str) -> String {
        // Copilot CLI uses Claude Code's Bash() allow syntax.
        format!("Bash(skim {tool}:*)")
    }

    fn seed(
        &self,
        config_dir: &Path,
        _tier: PermissionsTier,
        entries: &[String],
    ) -> anyhow::Result<SeedOutcome> {
        if entries.is_empty() {
            return Ok(SeedOutcome::AlreadyCurrent);
        }

        let project_root = find_git_root_from_cwd().ok_or_else(|| {
            anyhow::anyhow!(
                "skim --permissions for Copilot CLI requires a git repository root.\n\
                 Run `skim init --permissions --agent copilot` from inside a git repo."
            )
        })?;

        // Reject filesystem root as project root.
        if project_root.parent().is_none() {
            anyhow::bail!(
                "resolved project root is the filesystem root (`/`) — refusing to write \
                 Copilot permissions for the root directory"
            );
        }

        let project_key = project_root.to_string_lossy().into_owned();
        let config_path = config_dir.join(CONFIG_FILENAME);

        // Byte-cap check.
        if config_path.exists() {
            let size = std::fs::metadata(&config_path)?.len();
            if size > MAX_SETTINGS_SIZE {
                anyhow::bail!(
                    "permissions-config.json is too large ({size} bytes, max {max} bytes): {path}",
                    max = MAX_SETTINGS_SIZE,
                    path = config_path.display()
                );
            }
        }

        // Load the full per-project config map.
        let mut map = load_permissions_map(&config_path)?;

        // Ensure the project key and allow array exist.
        let project_obj = map
            .entry(project_key.clone())
            .or_insert_with(|| serde_json::json!({"allow": []}));

        // Navigate to or create the allow array.
        let allow_array = ensure_allow_array(project_obj, &config_path)?;

        let existing: std::collections::HashSet<String> = allow_array
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();

        let mut entries_added: Vec<String> = Vec::new();
        for entry in entries {
            if !existing.contains(entry) {
                allow_array.push(serde_json::Value::String(entry.clone()));
                entries_added.push(entry.clone());
            }
        }

        if entries_added.is_empty() {
            return Ok(SeedOutcome::AlreadyCurrent);
        }

        // Write atomically.
        std::fs::create_dir_all(config_dir)?;
        let full_json = serde_json::Value::Object(map);
        atomic_write_settings(&full_json, &config_path)?;

        // Write sidecar.
        let config_hash = compute_file_hash(&config_path)?;
        let sidecar = PermissionSidecar {
            version: 1,
            tier: "seed".to_string(),
            entries: entries.to_vec(),
            source_mirrors: std::collections::HashMap::new(),
            config_hash,
        };
        let sidecar_path = config_dir.join(SIDECAR_FILENAME);
        write_sidecar(&sidecar_path, &sidecar)?;

        Ok(SeedOutcome::Added { entries_added })
    }

    fn remove_seeded(&self, config_dir: &Path) -> anyhow::Result<RemoveOutcome> {
        let sidecar_path = config_dir.join(SIDECAR_FILENAME);

        // Fail-loud on missing or corrupt sidecar.
        let sidecar = load_sidecar(&sidecar_path).map_err(|e| {
            anyhow::anyhow!(
                "cannot remove seeded Copilot permissions: {}\n\
                 hint: if the sidecar was deleted manually, no entries will be removed",
                e
            )
        })?;

        let project_root = find_git_root_from_cwd().ok_or_else(|| {
            anyhow::anyhow!(
                "cannot determine project root for Copilot permissions removal — \
                 run from inside a git repo"
            )
        })?;
        let project_key = project_root.to_string_lossy().into_owned();

        let config_path = config_dir.join(CONFIG_FILENAME);
        if !config_path.exists() {
            return Ok(RemoveOutcome::NothingToRemove);
        }

        let mut map = load_permissions_map(&config_path)?;

        let project_obj = match map.get_mut(&project_key) {
            Some(v) => v,
            None => return Ok(RemoveOutcome::NothingToRemove),
        };

        let allow_array = match project_obj.get_mut("allow").and_then(|a| a.as_array_mut()) {
            Some(arr) => arr,
            None => return Ok(RemoveOutcome::NothingToRemove),
        };

        let seeded: std::collections::HashSet<&String> = sidecar.entries.iter().collect();
        let mut entries_removed: Vec<String> = Vec::new();

        allow_array.retain(|v| {
            let s = v.as_str().unwrap_or("");
            if seeded.contains(&s.to_string()) {
                entries_removed.push(s.to_string());
                false
            } else {
                true
            }
        });

        if entries_removed.is_empty() {
            return Ok(RemoveOutcome::NothingToRemove);
        }

        // Write the updated map.
        let full_json = serde_json::Value::Object(map);
        atomic_write_settings(&full_json, &config_path)?;

        // Delete the sidecar on success.
        let _ = std::fs::remove_file(&sidecar_path);

        Ok(RemoveOutcome::Removed { entries_removed })
    }

    fn is_current(&self, config_dir: &Path, entries: &[String]) -> bool {
        let config_path = config_dir.join(CONFIG_FILENAME);
        if !config_path.exists() {
            return false;
        }

        let project_root = match find_git_root_from_cwd() {
            Some(r) => r,
            None => return false,
        };
        let project_key = project_root.to_string_lossy().into_owned();

        let map = match load_permissions_map(&config_path) {
            Ok(m) => m,
            Err(_) => return false,
        };

        let allow = match map
            .get(&project_key)
            .and_then(|v| v.get("allow"))
            .and_then(|a| a.as_array())
        {
            Some(arr) => arr,
            None => return false,
        };

        let existing: std::collections::HashSet<&str> =
            allow.iter().filter_map(|v| v.as_str()).collect();

        entries.iter().all(|e| existing.contains(e.as_str()))
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Walk up from `cwd` looking for a directory that contains `.git`.
///
/// Returns `None` when no `.git` is found within 64 ancestors.
/// Bounded at 64: realistic nesting is ≤ 20 levels; the cap limits stat calls
/// on slow/network filesystems while covering all real-world cases.
fn find_git_root_from_cwd() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let mut current = cwd.as_path();
    for _ in 0..64 {
        if current.join(".git").exists() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
    None
}

/// Load the permissions-config.json top-level object, or return an empty map.
fn load_permissions_map(
    config_path: &Path,
) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
    let settings = load_or_create_settings(config_path)?;
    match settings {
        serde_json::Value::Object(m) => Ok(m),
        _ => anyhow::bail!(
            "permissions-config.json root is not a JSON object: {}",
            config_path.display()
        ),
    }
}

/// Navigate to or create the `allow` array inside a project object.
fn ensure_allow_array<'a>(
    project_obj: &'a mut serde_json::Value,
    config_path: &Path,
) -> anyhow::Result<&'a mut Vec<serde_json::Value>> {
    let obj = project_obj.as_object_mut().ok_or_else(|| {
        anyhow::anyhow!(
            "permissions-config.json project entry is not a JSON object: {}",
            config_path.display()
        )
    })?;

    let allow = obj
        .entry("allow")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));

    if !allow.is_array() {
        anyhow::bail!(
            "permissions-config.json: project `allow` field is not a JSON array: {}",
            config_path.display()
        );
    }

    Ok(allow.as_array_mut().unwrap())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::permissions::{permissions_protocol_for_agent, seeded_entries};
    use crate::cmd::session::AgentKind;

    fn protocol() -> Box<dyn PermissionsProtocol> {
        permissions_protocol_for_agent(AgentKind::CopilotCli).unwrap()
    }

    // ---- render_entry ----

    #[test]
    fn test_copilot_render_entry_format() {
        let p = protocol();
        assert_eq!(p.render_entry("df"), "Bash(skim df:*)");
        assert_eq!(p.render_entry("ls"), "Bash(skim ls:*)");
        assert_eq!(p.render_entry("grep"), "Bash(skim grep:*)");
    }

    // ---- seeded_entries ----

    #[test]
    fn test_copilot_seeded_entries_exact_8() {
        let p = protocol();
        let entries = seeded_entries(p.as_ref());
        assert_eq!(entries.len(), 8, "Copilot must seed exactly 8 entries");
        assert!(entries.contains(&"Bash(skim ls:*)".to_string()));
        assert!(entries.contains(&"Bash(skim grep:*)".to_string()));
    }

    // ---- is_current: no config → false ----

    #[test]
    fn test_copilot_is_current_no_config_returns_false() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = vec!["Bash(skim df:*)".to_string()];
        assert!(!p.is_current(dir.path(), &entries));
    }

    // ---- remove_seeded: missing sidecar → Err ----

    #[test]
    fn test_copilot_remove_seeded_missing_sidecar_fails_loud() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let result = p.remove_seeded(dir.path());
        assert!(result.is_err(), "missing sidecar must return Err");
    }

    // ---- remove_seeded: corrupt sidecar → Err ----

    #[test]
    fn test_copilot_remove_seeded_corrupt_sidecar_fails_loud() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("skim-permissions.json"), b"{ not json }").unwrap();
        let p = protocol();
        let result = p.remove_seeded(dir.path());
        assert!(result.is_err(), "corrupt sidecar must return Err");
    }

    // ---- find_git_root: returns None when no .git ----

    #[test]
    fn test_find_git_root_returns_some_in_git_repo() {
        // The test suite runs inside the skim-issues repo, so this should find a .git.
        let root = find_git_root_from_cwd();
        assert!(root.is_some(), "test suite should run inside a git repo");
    }
}
