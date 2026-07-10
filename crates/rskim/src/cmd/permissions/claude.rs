//! Claude Code permissions writer.
//!
// Implementation-complete API consumed by Subtask 7 (init wiring + uninstall).
// Suppress dead_code until the callers land.
#![allow(dead_code)]
//!
//! Targets `{config_dir}/settings.json` → `permissions.allow` array.
//!
//! ## File ownership
//!
//! `settings.json` is **user-owned**: skim only adds or removes its own
//! entries. It never clobbers or reformats existing content.
//!
//! ## Write protocol
//!
//! 1. Back up `settings.json` before first modification.
//! 2. Load via `load_or_create_settings` (byte-capped, atomic).
//! 3. Navigate to `permissions.allow`; if the path exists but is not the
//!    expected type, return an actionable error — **never coerce**.
//! 4. Dedup: skip entries already present.
//! 5. Write atomically via `atomic_write_settings`.
//! 6. Compute `config_hash` and write the sidecar.
//!
//! ## Remove protocol
//!
//! Load the sidecar (fail-loud on missing/corrupt). Remove only entries that
//! are in the sidecar manifest **and** still byte-equal present in the allow
//! array. Leave everything else. Delete the sidecar on success.

use std::path::Path;

use crate::cmd::init::{
    MAX_SETTINGS_SIZE, atomic_write_settings, backup_settings_file, load_or_create_settings,
};
use crate::cmd::integrity::compute_file_hash;
use crate::cmd::permissions::sidecar::{PermissionSidecar, load_sidecar, write_sidecar};
use crate::cmd::permissions::{PermissionsProtocol, PermissionsTier, RemoveOutcome, SeedOutcome};

/// Config file name for Claude Code.
const CONFIG_FILENAME: &str = "settings.json";

/// Sidecar manifest file name (relative to config_dir).
const SIDECAR_FILENAME: &str = "skim-permissions.json";

pub(super) struct ClaudePermissions;

impl PermissionsProtocol for ClaudePermissions {
    fn agent_label(&self) -> &str {
        "Claude Code"
    }

    fn config_filename(&self) -> &str {
        CONFIG_FILENAME
    }

    fn render_entry(&self, tool: &str) -> String {
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

        let config_path = config_dir.join(CONFIG_FILENAME);

        // Byte-cap check before loading (defense in depth; load_or_create_settings also checks).
        if config_path.exists() {
            let size = std::fs::metadata(&config_path)?.len();
            if size > MAX_SETTINGS_SIZE {
                anyhow::bail!(
                    "settings.json is too large ({size} bytes, max {max} bytes): {path}\n\
                     hint: This does not look like a valid Claude Code settings file",
                    max = MAX_SETTINGS_SIZE,
                    path = config_path.display()
                );
            }
        }

        let mut settings = load_or_create_settings(&config_path)?;

        // Navigate to permissions.allow, erroring loudly if type is wrong.
        let allow_array = get_or_create_allow_array(&mut settings, &config_path)?;

        // Collect already-present entries for dedup.
        let existing: Vec<String> = allow_array
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

        // Ensure config_dir exists.
        if !config_dir.exists() {
            std::fs::create_dir_all(config_dir)?;
        }

        // Back up before first write to a pre-existing user file.
        if config_path.exists() {
            backup_settings_file(config_dir, &config_path)?;
        }

        // Atomic write.
        atomic_write_settings(&settings, &config_path)?;

        // Compute hash and write sidecar.
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
                "cannot remove seeded permissions: {}\n\
                 hint: if the sidecar was deleted manually, no entries will be removed",
                e
            )
        })?;

        let config_path = config_dir.join(CONFIG_FILENAME);
        if !config_path.exists() {
            return Ok(RemoveOutcome::NothingToRemove);
        }

        let mut settings = load_or_create_settings(&config_path)?;

        // Navigate to permissions.allow. If absent, nothing to remove.
        let allow_array = match get_allow_array_mut(&mut settings) {
            Some(arr) => arr,
            None => return Ok(RemoveOutcome::NothingToRemove),
        };

        // Remove only entries that are BOTH in the sidecar AND byte-equal present.
        let mut entries_removed: Vec<String> = Vec::new();
        let seeded: std::collections::HashSet<&String> = sidecar.entries.iter().collect();

        allow_array.retain(|v| {
            let s = v.as_str().unwrap_or("");
            if seeded.contains(&s.to_string()) {
                entries_removed.push(s.to_string());
                false // remove from array
            } else {
                true // keep
            }
        });

        if entries_removed.is_empty() {
            return Ok(RemoveOutcome::NothingToRemove);
        }

        // Write the modified settings.
        backup_settings_file(config_dir, &config_path)?;
        atomic_write_settings(&settings, &config_path)?;

        // Delete the sidecar on success.
        let _ = std::fs::remove_file(&sidecar_path);

        Ok(RemoveOutcome::Removed { entries_removed })
    }

    fn is_current(&self, config_dir: &Path, entries: &[String]) -> bool {
        let config_path = config_dir.join(CONFIG_FILENAME);
        if !config_path.exists() {
            return false;
        }

        let settings = match load_or_create_settings(&config_path) {
            Ok(v) => v,
            Err(_) => return false,
        };

        let allow = match settings
            .get("permissions")
            .and_then(|p| p.get("allow"))
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

/// Navigate to `permissions.allow` array in a mutable `serde_json::Value`,
/// creating the path if absent. Returns an error if any segment exists but is
/// not the expected type (never coerces, never clobbers).
fn get_or_create_allow_array<'a>(
    settings: &'a mut serde_json::Value,
    path: &Path,
) -> anyhow::Result<&'a mut Vec<serde_json::Value>> {
    let obj = settings.as_object_mut().ok_or_else(|| {
        anyhow::anyhow!(
            "settings.json root is not a JSON object: {}",
            path.display()
        )
    })?;

    let permissions = obj
        .entry("permissions")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));

    if !permissions.is_object() {
        anyhow::bail!(
            "settings.json: `permissions` field exists but is not a JSON object — \
             cannot insert allow-list entries without clobbering existing content: {}\n\
             hint: inspect the file and resolve the type conflict manually",
            path.display()
        );
    }

    let allow = permissions
        .as_object_mut()
        .unwrap() // safe: checked above
        .entry("allow")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));

    if !allow.is_array() {
        anyhow::bail!(
            "settings.json: `permissions.allow` exists but is not a JSON array — \
             cannot insert allow-list entries without clobbering existing content: {}\n\
             hint: inspect the file and resolve the type conflict manually",
            path.display()
        );
    }

    Ok(allow.as_array_mut().unwrap()) // safe: checked above
}

/// Navigate to `permissions.allow` array in a mutable `serde_json::Value`,
/// returning `None` if any segment is absent. Does not create missing segments.
fn get_allow_array_mut(settings: &mut serde_json::Value) -> Option<&mut Vec<serde_json::Value>> {
    settings
        .get_mut("permissions")?
        .get_mut("allow")?
        .as_array_mut()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::permissions::permissions_protocol_for_agent;
    use crate::cmd::permissions::seeded_entries;
    use crate::cmd::session::AgentKind;

    fn protocol() -> Box<dyn PermissionsProtocol> {
        permissions_protocol_for_agent(AgentKind::ClaudeCode).unwrap()
    }

    // ---- render_entry ----

    #[test]
    fn test_render_entry_produces_bash_prefix() {
        let p = protocol();
        assert_eq!(p.render_entry("df"), "Bash(skim df:*)");
        assert_eq!(p.render_entry("grep"), "Bash(skim grep:*)");
        assert_eq!(p.render_entry("wc"), "Bash(skim wc:*)");
    }

    // ---- seed: happy path ----

    #[test]
    fn test_seed_creates_permissions_allow() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = seeded_entries(p.as_ref());

        let outcome = p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();
        assert!(
            matches!(outcome, SeedOutcome::Added { .. }),
            "first seed must report Added"
        );

        // Verify settings.json contains all entries.
        let settings_path = dir.path().join("settings.json");
        let contents = std::fs::read_to_string(&settings_path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&contents).unwrap();
        let allow = json["permissions"]["allow"].as_array().unwrap();
        for entry in &entries {
            assert!(
                allow.iter().any(|v| v.as_str() == Some(entry.as_str())),
                "allow array must contain entry: {entry}"
            );
        }
    }

    #[test]
    fn test_seed_creates_sidecar() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = seeded_entries(p.as_ref());

        p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();

        let sidecar_path = dir.path().join("skim-permissions.json");
        assert!(sidecar_path.exists(), "sidecar must be created after seed");

        let sidecar = load_sidecar(&sidecar_path).unwrap();
        assert_eq!(sidecar.version, 1);
        assert_eq!(sidecar.tier, "seed");
        assert_eq!(sidecar.entries, entries);
        assert!(
            !sidecar.config_hash.is_empty(),
            "config_hash must be non-empty"
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
            "second seed with same entries must return AlreadyCurrent"
        );
    }

    #[test]
    fn test_seed_dedup_skips_existing_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        // Pre-populate with one entry.
        let existing_entry = "Bash(skim df:*)".to_string();
        let settings = serde_json::json!({
            "permissions": {
                "allow": [existing_entry.clone()]
            }
        });
        let settings_path = dir.path().join("settings.json");
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        let p = protocol();
        let entries = vec![existing_entry.clone(), "Bash(skim grep:*)".to_string()];
        let outcome = p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();

        match outcome {
            SeedOutcome::Added { entries_added } => {
                assert!(
                    !entries_added.contains(&existing_entry),
                    "already-present entry must not be in entries_added"
                );
                assert!(
                    entries_added.contains(&"Bash(skim grep:*)".to_string()),
                    "new entry must be in entries_added"
                );
            }
            SeedOutcome::AlreadyCurrent => panic!("should have added one new entry"),
        }
    }

    // ---- seed: security negatives ----

    #[test]
    fn test_seed_non_array_allow_returns_error() {
        let dir = tempfile::TempDir::new().unwrap();
        // `permissions.allow` is a string, not an array.
        let settings = serde_json::json!({
            "permissions": {
                "allow": "not-an-array"
            }
        });
        let settings_path = dir.path().join("settings.json");
        let original_bytes = serde_json::to_vec_pretty(&settings).unwrap();
        std::fs::write(&settings_path, &original_bytes).unwrap();

        let p = protocol();
        let entries = vec!["Bash(skim df:*)".to_string()];
        let result = p.seed(dir.path(), PermissionsTier::Seed, &entries);

        assert!(result.is_err(), "non-array allow must return Err");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not a JSON array") || err.contains("permissions.allow"),
            "error must describe the type conflict: {err}"
        );

        // File bytes must be unchanged.
        let after = std::fs::read(&settings_path).unwrap();
        assert_eq!(
            after, original_bytes,
            "settings.json must be unchanged after error"
        );
    }

    #[test]
    fn test_seed_non_object_permissions_returns_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let settings = serde_json::json!({ "permissions": "not-an-object" });
        let settings_path = dir.path().join("settings.json");
        let original_bytes = serde_json::to_vec_pretty(&settings).unwrap();
        std::fs::write(&settings_path, &original_bytes).unwrap();

        let p = protocol();
        let entries = vec!["Bash(skim df:*)".to_string()];
        let result = p.seed(dir.path(), PermissionsTier::Seed, &entries);

        assert!(result.is_err(), "non-object permissions must return Err");
        let after = std::fs::read(&settings_path).unwrap();
        assert_eq!(after, original_bytes, "settings.json must be unchanged");
    }

    #[test]
    fn test_seed_oversized_settings_returns_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let settings_path = dir.path().join("settings.json");

        // Create a sparse file exceeding MAX_SETTINGS_SIZE.
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut file = std::fs::File::create(&settings_path).unwrap();
            file.seek(SeekFrom::Start(MAX_SETTINGS_SIZE + 1)).unwrap();
            file.write_all(b"x").unwrap();
        }

        let p = protocol();
        let entries = vec!["Bash(skim df:*)".to_string()];
        let result = p.seed(dir.path(), PermissionsTier::Seed, &entries);
        assert!(result.is_err(), "oversized settings must return Err");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("too large") || err.contains("settings.json"),
            "error must mention size: {err}"
        );
    }

    // ---- remove_seeded ----

    #[test]
    fn test_remove_seeded_removes_manifest_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = vec![
            "Bash(skim df:*)".to_string(),
            "Bash(skim grep:*)".to_string(),
        ];

        // Pre-populate settings with the seeded entries PLUS an unrelated one.
        let settings = serde_json::json!({
            "permissions": {
                "allow": ["Bash(skim df:*)", "Bash(skim grep:*)", "SomeOtherEntry"]
            }
        });
        let settings_path = dir.path().join("settings.json");
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        // Write sidecar that recorded only the two seeded entries.
        let hash = compute_file_hash(&settings_path).unwrap();
        let sidecar = PermissionSidecar {
            version: 1,
            tier: "seed".to_string(),
            entries: entries.clone(),
            source_mirrors: std::collections::HashMap::new(),
            config_hash: hash,
        };
        let sidecar_path = dir.path().join("skim-permissions.json");
        write_sidecar(&sidecar_path, &sidecar).unwrap();

        let outcome = p.remove_seeded(dir.path()).unwrap();
        assert!(
            matches!(outcome, RemoveOutcome::Removed { .. }),
            "should report Removed"
        );

        // Verify the unrelated entry is still present.
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        let allow = after["permissions"]["allow"].as_array().unwrap();
        let entry_strs: Vec<&str> = allow.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            entry_strs.contains(&"SomeOtherEntry"),
            "unrelated entry must be preserved"
        );
        assert!(
            !entry_strs.contains(&"Bash(skim df:*)"),
            "seeded entry must be removed"
        );
        assert!(
            !entry_strs.contains(&"Bash(skim grep:*)"),
            "seeded entry must be removed"
        );

        // Sidecar must be deleted.
        assert!(
            !sidecar_path.exists(),
            "sidecar must be deleted after successful remove"
        );
    }

    #[test]
    fn test_remove_seeded_corrupt_sidecar_fails_loud() {
        let dir = tempfile::TempDir::new().unwrap();
        // Write corrupt sidecar.
        let sidecar_path = dir.path().join("skim-permissions.json");
        std::fs::write(&sidecar_path, b"{ not valid json }").unwrap();

        let p = protocol();
        let result = p.remove_seeded(dir.path());
        assert!(result.is_err(), "corrupt sidecar must return Err");

        // settings.json must not be touched.
        let settings_path = dir.path().join("settings.json");
        assert!(
            !settings_path.exists(),
            "settings.json must not be created/modified on sidecar error"
        );
    }

    #[test]
    fn test_remove_seeded_missing_sidecar_fails_loud() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let result = p.remove_seeded(dir.path());
        assert!(
            result.is_err(),
            "missing sidecar must return Err (not silent NothingToRemove)"
        );
    }

    // ---- is_current ----

    #[test]
    fn test_is_current_returns_true_when_all_entries_present() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = vec!["Bash(skim df:*)".to_string()];

        p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();
        assert!(
            p.is_current(dir.path(), &entries),
            "is_current must return true after seeding"
        );
    }

    #[test]
    fn test_is_current_returns_false_for_missing_entry() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = vec!["Bash(skim df:*)".to_string()];

        // Don't seed — settings.json absent.
        assert!(
            !p.is_current(dir.path(), &entries),
            "is_current must return false when settings.json absent"
        );
    }
}
