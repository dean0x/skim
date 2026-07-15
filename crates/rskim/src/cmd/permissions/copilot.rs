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

use super::hash_if_bounded;
use crate::cmd::init::{MAX_SETTINGS_SIZE, atomic_write_settings, load_or_create_settings};
use crate::cmd::permissions::sidecar::{
    PermissionSidecar, SIDECAR_FILENAME, load_sidecar, write_sidecar,
};
use crate::cmd::permissions::{PermissionsProtocol, PermissionsTier, RemoveOutcome, SeedOutcome};

/// Config filename (relative to copilot_home).
const CONFIG_FILENAME: &str = "permissions-config.json";

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
        // The file was just written by skim after a size-guarded load; oversized here is unexpected.
        let config_hash = hash_if_bounded(&config_path)?.ok_or_else(|| {
            anyhow::anyhow!(
                "permissions-config.json unexpectedly exceeds size limit after write — \
                 this is an internal error; please file a bug report"
            )
        })?;
        let sidecar = PermissionSidecar {
            version: 1,
            // tier is hardcoded to "seed": Mirror requests produce the same seed-set for
            // this agent.
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

        let seeded: std::collections::HashSet<&str> =
            sidecar.entries.iter().map(String::as_str).collect();
        let mut entries_removed: Vec<String> = Vec::new();

        allow_array.retain(|v| {
            let s = v.as_str().unwrap_or("");
            if seeded.contains(s) {
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

/// Walk up from `start` looking for a directory that contains `.git`.
///
/// Returns `None` when no `.git` is found within 64 ancestors.
/// Bounded at 64: realistic nesting is ≤ 20 levels; the cap limits stat calls
/// on slow/network filesystems while covering all real-world cases.
fn find_git_root_from(start: &Path) -> Option<std::path::PathBuf> {
    let mut current = start;
    for _ in 0..64 {
        if current.join(".git").exists() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
    None
}

/// Walk up from `cwd` looking for a directory that contains `.git`.
///
/// Thin wrapper around [`find_git_root_from`] for production use.
fn find_git_root_from_cwd() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    find_git_root_from(&cwd)
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

    // ---- seed: happy path ----

    #[test]
    fn test_seed_creates_permissions_config_with_allow_array() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = seeded_entries(p.as_ref());

        let outcome = p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();
        assert!(
            matches!(outcome, SeedOutcome::Added { .. }),
            "first seed must report Added"
        );

        let config_path = dir.path().join("permissions-config.json");
        assert!(
            config_path.exists(),
            "permissions-config.json must be created"
        );

        // Compute the expected project key the same way production code does it.
        let project_key = find_git_root_from_cwd()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        let content = std::fs::read_to_string(&config_path).unwrap();
        let map: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow = map[&project_key]["allow"].as_array().unwrap();
        let allow_strings: Vec<&str> = allow.iter().filter_map(|v| v.as_str()).collect();
        for entry in &entries {
            assert!(
                allow_strings.contains(&entry.as_str()),
                "allow array must contain entry: {entry}"
            );
        }
    }

    #[test]
    fn test_seed_writes_sidecar_with_config_hash() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = seeded_entries(p.as_ref());

        p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();

        let sidecar_path = dir.path().join("skim-permissions.json");
        assert!(sidecar_path.exists(), "sidecar must be created");
        let sidecar = load_sidecar(&sidecar_path).unwrap();
        assert_eq!(
            sidecar.entries, entries,
            "sidecar must record all seeded entries"
        );
        assert!(
            !sidecar.config_hash.is_empty(),
            "sidecar must record a non-empty config_hash"
        );
    }

    #[test]
    fn test_seed_idempotent_returns_already_current() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = seeded_entries(p.as_ref());

        p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();
        let second = p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();
        assert!(
            matches!(second, SeedOutcome::AlreadyCurrent),
            "second seed must return AlreadyCurrent"
        );
    }

    // ---- remove_seeded: happy path ----

    #[test]
    fn test_remove_seeded_removes_seeded_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = seeded_entries(p.as_ref());

        p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();

        let outcome = p.remove_seeded(dir.path()).unwrap();
        assert!(
            matches!(outcome, RemoveOutcome::Removed { .. }),
            "remove_seeded must report Removed"
        );

        // Allow array for the current project key must be empty after removal.
        let config_path = dir.path().join("permissions-config.json");
        let project_key = find_git_root_from_cwd()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let content = std::fs::read_to_string(&config_path).unwrap();
        let map: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow_after = map
            .get(&project_key)
            .and_then(|v| v.get("allow"))
            .and_then(|a| a.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
        assert_eq!(
            allow_after, 0,
            "allow array must be empty after removing all seeded entries"
        );

        // Sidecar must be deleted.
        assert!(
            !dir.path().join("skim-permissions.json").exists(),
            "sidecar must be deleted after successful remove_seeded"
        );
    }

    // ---- per-project-key isolation ----

    #[test]
    fn test_seed_and_remove_do_not_affect_other_project_keys() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = seeded_entries(p.as_ref());

        // Pre-populate config with an unrelated project key before seeding.
        let unrelated_key = "/some/unrelated/project";
        let unrelated_entries = vec![
            "Bash(skim df:*)".to_string(),
            "Bash(custom-tool:*)".to_string(),
        ];
        let initial_map = serde_json::json!({
            unrelated_key: { "allow": &unrelated_entries }
        });
        let config_path = dir.path().join("permissions-config.json");
        std::fs::write(
            &config_path,
            serde_json::to_string_pretty(&initial_map).unwrap(),
        )
        .unwrap();

        // Seed under the current project key.
        let seed_outcome = p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();
        assert!(
            matches!(seed_outcome, SeedOutcome::Added { .. }),
            "seed must add entries: {:?}",
            seed_outcome
        );

        // Assert the unrelated key is intact after seed.
        let content = std::fs::read_to_string(&config_path).unwrap();
        let map: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow_after_seed: Vec<String> = map[unrelated_key]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        assert_eq!(
            allow_after_seed, unrelated_entries,
            "seed must not touch the unrelated project key"
        );

        // Remove seeded entries.
        let remove_outcome = p.remove_seeded(dir.path()).unwrap();
        assert!(
            matches!(remove_outcome, RemoveOutcome::Removed { .. }),
            "remove_seeded must succeed: {:?}",
            remove_outcome
        );

        // Assert the unrelated key is still intact after remove.
        let content = std::fs::read_to_string(&config_path).unwrap();
        let map: serde_json::Value = serde_json::from_str(&content).unwrap();
        let allow_after_remove: Vec<String> = map[unrelated_key]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        assert_eq!(
            allow_after_remove, unrelated_entries,
            "remove_seeded must not touch the unrelated project key"
        );
    }

    // ---- find_git_root: injectable start-path tests (I-16) ----

    #[test]
    fn test_find_git_root_from_finds_git_in_ancestor() {
        // Arrange: tempdir with a nested subdir and a .git marker at the root level.
        let dir = tempfile::TempDir::new().unwrap();
        let git_marker = dir.path().join(".git");
        std::fs::create_dir_all(&git_marker).unwrap();
        let nested = dir.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();

        // Act: start from the nested subdir — should walk up and find .git at the root.
        let found = find_git_root_from(&nested);

        assert_eq!(
            found,
            Some(dir.path().to_path_buf()),
            "must find .git at the ancestor root"
        );
    }

    #[test]
    fn test_find_git_root_from_returns_none_without_git() {
        // Arrange: pure tempdir with no .git at any level in the tree.
        let dir = tempfile::TempDir::new().unwrap();

        let found = find_git_root_from(dir.path());
        assert!(
            found.is_none(),
            "must return None when no .git exists in the tree"
        );
    }

    // ---- find_git_root: ambient test (cwd wrapper) ----

    #[test]
    fn test_find_git_root_returns_some_in_git_repo() {
        // The test suite runs inside the skim-issues repo, so find_git_root_from_cwd()
        // (which delegates to find_git_root_from) should find a .git.
        let root = find_git_root_from_cwd();
        assert!(root.is_some(), "test suite should run inside a git repo");
    }
}
