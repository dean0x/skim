//! Claude Code permissions writer.
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

use std::collections::{HashMap, HashSet};
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
// Mirror tier (Claude-scoped by design)
// ============================================================================
//
// The Mirror tier is Claude-scoped: it reads the EXISTING Claude settings.json
// `permissions.allow` list and proposes skim-prefixed mirrors for any entry
// that names a wrapped tool. Non-Claude agents have no parseable Bash() source
// rules; callers must fall back to Seed entries with a printed notice.

/// A proposed mirror entry.
///
/// Each `MirrorProposal` is derived from one `source` allow entry in the
/// current settings.json and paired with the mirrored form that skim would
/// add. `is_mutating` is true when the tool is NOT in `READ_ONLY_SUBCOMMANDS`
/// (consent must highlight mutating tools).
#[derive(Debug, Clone)]
pub(crate) struct MirrorProposal {
    /// The source allow entry (e.g. `"Bash(git:*)"`)
    pub(crate) source: String,
    /// The mirror allow entry with `"skim "` prefix (e.g. `"Bash(skim git:*)"`)
    pub(crate) mirror: String,
    /// True when the mirrored tool is NOT in `READ_ONLY_SUBCOMMANDS` (i.e., mutating).
    /// Consent display must mark these with a `(mutating)` suffix.
    pub(crate) is_mutating: bool,
}

/// Validate that `entry` is an acceptable source for mirror derivation.
///
/// Accepted shapes (exact only):
/// - `Bash(<tool>:*)` — single token
/// - `Bash(<tool> <sub>:*)` — tool + subcommand
///
/// Requirements for acceptance:
/// - Starts with `Bash(` and ends with `:*)`
/// - Inner content is `<tool>` or `<tool> <sub>`; each part matches `^[A-Za-z0-9._-]+$`
/// - `<tool>` must be in `wrapper_targets()`
/// - No `*` or regex-ish chars inside the inner content (outside the trailing `:*`)
///
/// Rejected: `Bash(*)`, any wildcard/regex shape, non-Bash rules, malformed parens.
fn is_valid_mirror_source(entry: &str) -> bool {
    // Must start with `Bash(` and end with `:*)`
    let inner = match entry
        .strip_prefix("Bash(")
        .and_then(|s| s.strip_suffix(":*)"))
    {
        Some(i) => i,
        None => return false,
    };

    // Inner must not be empty (rejects `Bash(:*)`)
    if inner.is_empty() {
        return false;
    }

    // Inner must not contain `*`, `?`, `[`, `]` or parentheses (wildcard/regex guards)
    if inner
        .chars()
        .any(|c| matches!(c, '*' | '?' | '[' | ']' | '(' | ')'))
    {
        return false;
    }

    // Split into tool (required) and optional sub, separated by a single space.
    // More than one space means something unusual — reject.
    let parts: Vec<&str> = inner.splitn(2, ' ').collect();
    let tool = parts[0];
    let sub = parts.get(1).copied();

    // Tool must be in wrapper_targets().
    if !crate::cmd::registry::wrapper_targets().contains(&tool) {
        return false;
    }

    // Each part must match `^[A-Za-z0-9._-]+$` (strict; no spaces, no special chars).
    let valid_chars = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };

    if !valid_chars(tool) {
        return false;
    }

    if sub.is_some_and(|s| !valid_chars(s)) {
        return false;
    }

    true
}

/// Compute the mirror entry string for a validated source entry.
///
/// `Bash(tool:*)` → `Bash(skim tool:*)`
/// `Bash(tool sub:*)` → `Bash(skim tool sub:*)`
///
/// Returns `None` when `source` is not in the expected shape (defensive; callers
/// should call `is_valid_mirror_source` first).
fn compute_mirror(source: &str) -> Option<String> {
    let inner = source.strip_prefix("Bash(")?.strip_suffix(":*)")?;
    Some(format!("Bash(skim {inner}:*)"))
}

/// Check whether a proposed mirror is already covered by any deny or ask rule.
///
/// Conservative coverage: exact string match OR the deny/ask entry is
/// `Bash(skim:*)` (blanket skim deny/ask that covers all skim-prefixed mirrors).
fn is_mirror_covered(mirror: &str, deny_rules: &[&str], ask_rules: &[&str]) -> bool {
    for &rules in &[deny_rules, ask_rules] {
        for &rule in rules {
            // Exact match.
            if rule == mirror {
                return true;
            }
            // Blanket skim deny/ask covers all `Bash(skim …:*)` mirrors.
            if rule == "Bash(skim:*)" {
                return true;
            }
        }
    }
    false
}

/// Helper: extract string values from a JSON array at `settings[key1][key2]`.
fn extract_string_list<'a>(
    settings: &'a serde_json::Value,
    key1: &str,
    key2: &str,
) -> Vec<&'a str> {
    settings
        .get(key1)
        .and_then(|p| p.get(key2))
        .and_then(|a| a.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default()
}

/// Compute mirror proposals from the current `settings.json` allow list.
///
/// Reads `settings.json` **once**: the same document is used for source
/// extraction, deny/ask consultation, dedup against existing mirrors, and
/// (downstream) the actual write via [`seed_mirrors`].
///
/// Returns proposals whose mirrors are not already in the allow list and
/// are not covered by any deny/ask rule.
pub(crate) fn propose_mirrors(config_dir: &Path) -> anyhow::Result<Vec<MirrorProposal>> {
    let config_path = config_dir.join(CONFIG_FILENAME);
    let settings = load_or_create_settings(&config_path)?;

    let allow: Vec<&str> = extract_string_list(&settings, "permissions", "allow");
    let deny: Vec<&str> = extract_string_list(&settings, "permissions", "deny");
    let ask: Vec<&str> = extract_string_list(&settings, "permissions", "ask");

    let allow_set: HashSet<&str> = allow.iter().copied().collect();

    let mut proposals = Vec::new();
    for &source in &allow {
        if !is_valid_mirror_source(source) {
            continue;
        }

        let mirror = match compute_mirror(source) {
            Some(m) => m,
            None => continue,
        };

        // Skip if the mirror is covered by a deny or ask rule.
        if is_mirror_covered(&mirror, &deny, &ask) {
            continue;
        }

        // Skip if the mirror already exists in the allow list (dedup).
        if allow_set.contains(mirror.as_str()) {
            continue;
        }

        // Determine if the tool is mutating (not in READ_ONLY_SUBCOMMANDS).
        let tool = source
            .strip_prefix("Bash(")
            .and_then(|s| s.strip_suffix(":*)"))
            .and_then(|inner| inner.split(' ').next())
            .unwrap_or("");
        let is_mutating = !crate::cmd::registry::is_read_only(tool);

        proposals.push(MirrorProposal {
            source: source.to_string(),
            mirror,
            is_mutating,
        });
    }

    Ok(proposals)
}

/// Seed mirror entries into `settings.json`.
///
/// Implements re-run semantics: previously recorded mirrors (from the sidecar)
/// are removed first, then the new set is added. This ensures stale mirrors
/// (from sources that no longer exist or were removed) are cleaned up.
///
/// Writes the sidecar with tier `"mirror"` and the `source_mirrors` provenance map.
/// Returns `AlreadyCurrent` when no changes are needed.
pub(crate) fn seed_mirrors(
    config_dir: &Path,
    proposals: &[MirrorProposal],
) -> anyhow::Result<SeedOutcome> {
    if proposals.is_empty() {
        return Ok(SeedOutcome::AlreadyCurrent);
    }

    let config_path = config_dir.join(CONFIG_FILENAME);

    // Byte-cap check.
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

    // Load previously recorded mirror entries from the sidecar (for re-run cleanup).
    let sidecar_path = config_dir.join(SIDECAR_FILENAME);
    let prev_mirrors: Vec<String> = match load_sidecar(&sidecar_path) {
        Ok(s) if s.tier == "mirror" => s.entries,
        _ => vec![],
    };

    let mut settings = load_or_create_settings(&config_path)?;
    let allow_array = get_or_create_allow_array(&mut settings, &config_path)?;

    // Re-run semantics: remove previously recorded mirrors first.
    if !prev_mirrors.is_empty() {
        let prev_set: HashSet<&String> = prev_mirrors.iter().collect();
        allow_array.retain(|v| {
            let s = v.as_str().unwrap_or("");
            !prev_set.contains(&s.to_string())
        });
    }

    // Collect current allow values (post-removal) for dedup.
    let existing: HashSet<String> = allow_array
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();

    // Add fresh mirrors.
    let all_mirrors: Vec<String> = proposals.iter().map(|p| p.mirror.clone()).collect();
    let mut entries_added: Vec<String> = Vec::new();
    for mirror in &all_mirrors {
        if !existing.contains(mirror) {
            allow_array.push(serde_json::Value::String(mirror.clone()));
            entries_added.push(mirror.clone());
        }
    }

    if entries_added.is_empty() && prev_mirrors.is_empty() {
        return Ok(SeedOutcome::AlreadyCurrent);
    }

    // Write settings.json.
    if !config_dir.exists() {
        std::fs::create_dir_all(config_dir)?;
    }
    if config_path.exists() {
        backup_settings_file(config_dir, &config_path)?;
    }
    atomic_write_settings(&settings, &config_path)?;

    // Compute hash and write sidecar with source_mirrors provenance.
    let config_hash = compute_file_hash(&config_path)?;
    let source_mirrors: HashMap<String, String> = proposals
        .iter()
        .map(|p| (p.source.clone(), p.mirror.clone()))
        .collect();

    let sidecar = PermissionSidecar {
        version: 1,
        tier: "mirror".to_string(),
        entries: all_mirrors,
        source_mirrors,
        config_hash,
    };
    write_sidecar(&sidecar_path, &sidecar)?;

    if entries_added.is_empty() {
        Ok(SeedOutcome::AlreadyCurrent)
    } else {
        Ok(SeedOutcome::Added { entries_added })
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

    // ---- mirror tier: is_valid_mirror_source ----

    #[test]
    fn test_valid_mirror_source_single_tool() {
        // git is in wrapper_targets()
        assert!(is_valid_mirror_source("Bash(git:*)"));
        assert!(is_valid_mirror_source("Bash(ls:*)"));
        assert!(is_valid_mirror_source("Bash(grep:*)"));
    }

    #[test]
    fn test_valid_mirror_source_tool_with_sub() {
        // cargo build, git diff — tool in wrapper_targets()
        assert!(is_valid_mirror_source("Bash(cargo:*)"));
    }

    #[test]
    fn test_invalid_mirror_source_wildcard() {
        // Bare `Bash(*)` must be rejected
        assert!(!is_valid_mirror_source("Bash(*)"));
    }

    #[test]
    fn test_invalid_mirror_source_wildcard_in_tool() {
        // `*` inside the tool name must be rejected
        assert!(!is_valid_mirror_source("Bash(git*:*)"));
    }

    #[test]
    fn test_invalid_mirror_source_non_wrapper_tool() {
        // `rm` is not in wrapper_targets()
        assert!(!is_valid_mirror_source("Bash(rm:*)"));
    }

    #[test]
    fn test_invalid_mirror_source_not_bash_prefix() {
        // Non-Bash rules must be rejected
        assert!(!is_valid_mirror_source("Edit(ls:*)"));
        assert!(!is_valid_mirror_source("Read(*:*)"));
    }

    #[test]
    fn test_invalid_mirror_source_space_with_invalid_sub() {
        // Subcommand with space — only valid charset [A-Za-z0-9._-] allowed
        assert!(!is_valid_mirror_source("Bash(rm -rf:*)"));
    }

    #[test]
    fn test_invalid_mirror_source_missing_trailing_star() {
        // Trailing `:*)` must be present
        assert!(!is_valid_mirror_source("Bash(git:)"));
        assert!(!is_valid_mirror_source("Bash(git)"));
    }

    // ---- mirror tier: compute_mirror ----

    #[test]
    fn test_compute_mirror_single_tool() {
        assert_eq!(
            compute_mirror("Bash(git:*)"),
            Some("Bash(skim git:*)".to_string())
        );
        assert_eq!(
            compute_mirror("Bash(ls:*)"),
            Some("Bash(skim ls:*)".to_string())
        );
    }

    #[test]
    fn test_compute_mirror_tool_with_sub() {
        assert_eq!(
            compute_mirror("Bash(cargo build:*)"),
            Some("Bash(skim cargo build:*)".to_string())
        );
    }

    // ---- mirror tier: is_mirror_covered ----

    #[test]
    fn test_mirror_covered_exact_deny() {
        assert!(is_mirror_covered(
            "Bash(skim git:*)",
            &["Bash(skim git:*)"],
            &[]
        ));
    }

    #[test]
    fn test_mirror_covered_blanket_deny() {
        assert!(is_mirror_covered("Bash(skim ls:*)", &["Bash(skim:*)"], &[]));
    }

    #[test]
    fn test_mirror_covered_exact_ask() {
        assert!(is_mirror_covered(
            "Bash(skim grep:*)",
            &[],
            &["Bash(skim grep:*)"]
        ));
    }

    #[test]
    fn test_mirror_not_covered() {
        assert!(!is_mirror_covered(
            "Bash(skim ls:*)",
            &["Bash(skim git:*)"],
            &[]
        ));
        assert!(!is_mirror_covered("Bash(skim ls:*)", &[], &[]));
    }

    // ---- mirror tier: propose_mirrors ----

    #[test]
    fn test_propose_mirrors_empty_settings() {
        let dir = tempfile::TempDir::new().unwrap();
        let proposals = propose_mirrors(dir.path()).unwrap();
        assert!(proposals.is_empty(), "no source rules → no proposals");
    }

    #[test]
    fn test_propose_mirrors_with_valid_source() {
        let dir = tempfile::TempDir::new().unwrap();
        let settings = serde_json::json!({
            "permissions": {
                "allow": ["Bash(git:*)", "Bash(ls:*)"]
            }
        });
        let path = dir.path().join("settings.json");
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let proposals = propose_mirrors(dir.path()).unwrap();
        let mirrors: Vec<&str> = proposals.iter().map(|p| p.mirror.as_str()).collect();
        assert!(mirrors.contains(&"Bash(skim git:*)"), "git → skim git");
        assert!(mirrors.contains(&"Bash(skim ls:*)"), "ls → skim ls");
    }

    #[test]
    fn test_propose_mirrors_skips_bash_wildcard() {
        let dir = tempfile::TempDir::new().unwrap();
        let settings = serde_json::json!({
            "permissions": {
                "allow": ["Bash(*)", "Bash(git:*)"]
            }
        });
        let path = dir.path().join("settings.json");
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let proposals = propose_mirrors(dir.path()).unwrap();
        // `Bash(*)` must be rejected; `Bash(git:*)` is valid
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].mirror, "Bash(skim git:*)");
    }

    #[test]
    fn test_propose_mirrors_skips_denied_mirror() {
        let dir = tempfile::TempDir::new().unwrap();
        let settings = serde_json::json!({
            "permissions": {
                "allow": ["Bash(git:*)"],
                "deny":  ["Bash(skim git:*)"]
            }
        });
        let path = dir.path().join("settings.json");
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let proposals = propose_mirrors(dir.path()).unwrap();
        assert!(
            proposals.is_empty(),
            "mirror covered by deny rule must be skipped"
        );
    }

    #[test]
    fn test_propose_mirrors_skips_blanket_denied() {
        let dir = tempfile::TempDir::new().unwrap();
        let settings = serde_json::json!({
            "permissions": {
                "allow": ["Bash(git:*)", "Bash(ls:*)"],
                "deny":  ["Bash(skim:*)"]
            }
        });
        let path = dir.path().join("settings.json");
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let proposals = propose_mirrors(dir.path()).unwrap();
        assert!(
            proposals.is_empty(),
            "blanket skim deny must suppress all mirrors"
        );
    }

    #[test]
    fn test_propose_mirrors_skips_existing_mirror() {
        let dir = tempfile::TempDir::new().unwrap();
        // `Bash(skim git:*)` already in the allow list — must not be proposed again.
        let settings = serde_json::json!({
            "permissions": {
                "allow": ["Bash(git:*)", "Bash(skim git:*)"]
            }
        });
        let path = dir.path().join("settings.json");
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let proposals = propose_mirrors(dir.path()).unwrap();
        assert!(
            proposals.is_empty(),
            "already-mirrored source must not be proposed again"
        );
    }

    #[test]
    fn test_propose_mirrors_marks_mutating_tools() {
        let dir = tempfile::TempDir::new().unwrap();
        // git is mutating (not in READ_ONLY_SUBCOMMANDS); ls is read-only.
        let settings = serde_json::json!({
            "permissions": {
                "allow": ["Bash(git:*)", "Bash(ls:*)"]
            }
        });
        let path = dir.path().join("settings.json");
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let proposals = propose_mirrors(dir.path()).unwrap();
        let git_proposal = proposals.iter().find(|p| p.mirror == "Bash(skim git:*)");
        let ls_proposal = proposals.iter().find(|p| p.mirror == "Bash(skim ls:*)");

        assert!(git_proposal.is_some(), "git proposal must exist");
        assert!(ls_proposal.is_some(), "ls proposal must exist");
        assert!(git_proposal.unwrap().is_mutating, "git is mutating");
        assert!(!ls_proposal.unwrap().is_mutating, "ls is read-only");
    }

    // ---- mirror tier: seed_mirrors ----

    #[test]
    fn test_seed_mirrors_adds_entries() {
        let dir = tempfile::TempDir::new().unwrap();
        let settings = serde_json::json!({
            "permissions": {
                "allow": ["Bash(git:*)"]
            }
        });
        let path = dir.path().join("settings.json");
        std::fs::write(&path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let proposals = propose_mirrors(dir.path()).unwrap();
        assert!(!proposals.is_empty());

        let outcome = seed_mirrors(dir.path(), &proposals).unwrap();
        assert!(matches!(outcome, SeedOutcome::Added { .. }));

        // Verify mirror is in settings.json
        let updated: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let allow = updated["permissions"]["allow"].as_array().unwrap();
        let allow_strs: Vec<&str> = allow.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            allow_strs.contains(&"Bash(skim git:*)"),
            "mirror must be added"
        );
        assert!(
            allow_strs.contains(&"Bash(git:*)"),
            "source must be preserved"
        );
    }

    #[test]
    fn test_seed_mirrors_writes_sidecar_with_provenance() {
        let dir = tempfile::TempDir::new().unwrap();
        let settings = serde_json::json!({
            "permissions": { "allow": ["Bash(ls:*)"] }
        });
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        let proposals = propose_mirrors(dir.path()).unwrap();
        seed_mirrors(dir.path(), &proposals).unwrap();

        let sidecar_path = dir.path().join("skim-permissions.json");
        let sidecar = crate::cmd::permissions::sidecar::load_sidecar(&sidecar_path).unwrap();
        assert_eq!(sidecar.tier, "mirror");
        assert!(sidecar.entries.contains(&"Bash(skim ls:*)".to_string()));
        assert_eq!(
            sidecar.source_mirrors.get("Bash(ls:*)").map(String::as_str),
            Some("Bash(skim ls:*)")
        );
    }

    #[test]
    fn test_seed_mirrors_rerun_removes_stale_mirrors() {
        let dir = tempfile::TempDir::new().unwrap();
        // Initial settings: git is a source
        let settings = serde_json::json!({
            "permissions": { "allow": ["Bash(git:*)"] }
        });
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        let proposals = propose_mirrors(dir.path()).unwrap();
        seed_mirrors(dir.path(), &proposals).unwrap();

        // Now remove git from source, add ls
        let settings2 = serde_json::json!({
            "permissions": { "allow": ["Bash(ls:*)"] }
        });
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::to_string_pretty(&settings2).unwrap(),
        )
        .unwrap();

        let proposals2 = propose_mirrors(dir.path()).unwrap();
        seed_mirrors(dir.path(), &proposals2).unwrap();

        let updated: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("settings.json")).unwrap(),
        )
        .unwrap();
        let allow: Vec<&str> = updated["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        // Old git mirror must be removed; new ls mirror must be present
        assert!(
            !allow.contains(&"Bash(skim git:*)"),
            "stale git mirror must be removed"
        );
        assert!(
            allow.contains(&"Bash(skim ls:*)"),
            "new ls mirror must be present"
        );
    }

    #[test]
    fn test_seed_mirrors_empty_proposals_returns_already_current() {
        let dir = tempfile::TempDir::new().unwrap();
        let proposals = vec![];
        let outcome = seed_mirrors(dir.path(), &proposals).unwrap();
        assert!(matches!(outcome, SeedOutcome::AlreadyCurrent));
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
