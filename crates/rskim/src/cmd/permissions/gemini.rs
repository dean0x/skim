//! Gemini CLI permissions writer.
//!
//! Targets `{config_dir}/policies/skim.toml` — a skim-OWNED file that is
//! **wholesale-replaced** on every seed. skim never merges into an existing
//! Gemini policy file; it regenerates it from the current entry list.
//!
//! ## File ownership
//!
//! `policies/skim.toml` is **skim-owned**: the entire file is regenerated on
//! each seed. No backup is taken (skim-owned files need no user backup).
//!
//! ## TOML format
//!
//! The file contains a header comment and an array of `[[rule]]` tables:
//!
//! ```toml
//! # skim-managed tool permissions — do not edit manually
//! # Regenerate with: skim init --permissions --agent gemini
//!
//! [[rule]]
//! action = "allow"
//! tool = "run_shell_command"
//! pattern = "skim df"
//!
//! [[rule]]
//! action = "allow"
//! tool = "run_shell_command"
//! pattern = "skim diff"
//! ```
//!
//! The Gemini policy schema is validated-in-principle pending deferred manual
//! e2e in a sandboxed `GEMINI_CONFIG_DIR`.
//!
//! ## Security
//!
//! Entries containing `"`, `'`, newlines, or ASCII control characters are
//! rejected with an error before any write — these can never be legitimate
//! tool pattern values (injection defense even though the TOML serializer
//! would escape them).

use std::path::Path;

use serde::Serialize;

use crate::cmd::integrity::compute_file_hash;
use crate::cmd::permissions::sidecar::{
    PermissionSidecar, SidecarError, load_sidecar, write_sidecar,
};
use crate::cmd::permissions::{PermissionsProtocol, PermissionsTier, RemoveOutcome, SeedOutcome};

/// Config path (relative to config_dir) for the Gemini policy file.
const CONFIG_FILENAME: &str = "policies/skim.toml";

/// Sidecar manifest file name (relative to config_dir).
const SIDECAR_FILENAME: &str = "skim-permissions.json";

/// Gemini tool matcher used in policy rules.
const GEMINI_TOOL: &str = "run_shell_command";

// ============================================================================
// TOML schema
// ============================================================================

/// Root of the generated TOML file (serialized as `[[rule]]` array).
#[derive(Serialize)]
struct GeminiPolicyFile {
    rule: Vec<GeminiRule>,
}

/// A single Gemini policy rule.
#[derive(Serialize)]
struct GeminiRule {
    action: String,
    tool: String,
    pattern: String,
}

pub(super) struct GeminiPermissions;

impl PermissionsProtocol for GeminiPermissions {
    fn agent_label(&self) -> &str {
        "Gemini CLI"
    }

    fn config_filename(&self) -> &str {
        CONFIG_FILENAME
    }

    fn render_entry(&self, tool: &str) -> String {
        format!("skim {tool}")
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

        // Validate all entries before any write.
        for entry in entries {
            validate_gemini_entry(entry)?;
        }

        let config_path = config_dir.join(CONFIG_FILENAME);

        // Check if the file is already current (sidecar hash match).
        if let Ok(sidecar) = load_sidecar(&config_dir.join(SIDECAR_FILENAME))
            && sidecar.entries == entries
            && config_path.exists()
            && let Ok(current_hash) = compute_file_hash(&config_path)
            && current_hash == sidecar.config_hash
        {
            return Ok(SeedOutcome::AlreadyCurrent);
        }

        // Generate the TOML content from entries.
        let toml_content = generate_toml(entries)?;

        // Ensure parent directory exists.
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Atomic write (tmp + rename).
        let tmp_path = config_path.with_extension("toml.tmp");
        std::fs::write(&tmp_path, &toml_content)?;
        std::fs::rename(&tmp_path, &config_path)?;

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

        Ok(SeedOutcome::Added {
            entries_added: entries.to_vec(),
        })
    }

    fn remove_seeded(&self, config_dir: &Path) -> anyhow::Result<RemoveOutcome> {
        let sidecar_path = config_dir.join(SIDECAR_FILENAME);

        // Fail-loud on missing or corrupt sidecar.
        let sidecar = load_sidecar(&sidecar_path).map_err(|e| {
            anyhow::anyhow!(
                "cannot remove Gemini permissions: {}\n\
                 hint: if the sidecar was deleted manually, remove {} manually",
                e,
                config_dir.join(CONFIG_FILENAME).display()
            )
        })?;

        let config_path = config_dir.join(CONFIG_FILENAME);
        if !config_path.exists() {
            // File already gone — delete sidecar and report nothing to remove.
            let _ = std::fs::remove_file(&sidecar_path);
            return Ok(RemoveOutcome::NothingToRemove);
        }

        // Verify hash before deleting (don't remove a tampered file).
        let current_hash = compute_file_hash(&config_path)?;
        if current_hash != sidecar.config_hash {
            anyhow::bail!(
                "Gemini policy file hash mismatch — file may have been modified manually: {}\n\
                 hint: inspect the file, then delete it and the sidecar manually if no longer needed",
                config_path.display()
            );
        }

        std::fs::remove_file(&config_path)?;
        let _ = std::fs::remove_file(&sidecar_path);

        Ok(RemoveOutcome::Removed {
            entries_removed: sidecar.entries,
        })
    }

    fn is_current(&self, config_dir: &Path, entries: &[String]) -> bool {
        let sidecar_path = config_dir.join(SIDECAR_FILENAME);
        let config_path = config_dir.join(CONFIG_FILENAME);

        let sidecar = match load_sidecar(&sidecar_path) {
            Ok(s) => s,
            Err(SidecarError::NotFound(_)) => return false,
            Err(_) => return false,
        };

        if sidecar.entries != entries {
            return false;
        }

        if !config_path.exists() {
            return false;
        }

        compute_file_hash(&config_path)
            .map(|h| h == sidecar.config_hash)
            .unwrap_or(false)
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Generate TOML content for the given entry patterns.
///
/// Each entry is used as the `pattern` field of a `[[rule]]` table with
/// `action = "allow"` and `tool = "run_shell_command"`.
fn generate_toml(entries: &[String]) -> anyhow::Result<String> {
    let file = GeminiPolicyFile {
        rule: entries
            .iter()
            .map(|e| GeminiRule {
                action: "allow".to_string(),
                tool: GEMINI_TOOL.to_string(),
                pattern: e.clone(),
            })
            .collect(),
    };

    let body = toml::to_string_pretty(&file)
        .map_err(|e| anyhow::anyhow!("failed to serialize Gemini policy TOML: {e}"))?;

    Ok(format!(
        "# skim-managed tool permissions — do not edit manually\n\
         # Regenerate with: skim init --permissions --agent gemini\n\
         \n{body}"
    ))
}

/// Validate a Gemini entry (pattern value) for injection safety.
///
/// Rejects entries containing double-quotes, single-quotes, newlines, or
/// ASCII control characters. These are never legitimate tool pattern values
/// and could indicate injection — reject outright even though the TOML
/// serializer would escape them.
fn validate_gemini_entry(entry: &str) -> anyhow::Result<()> {
    if entry.is_empty() {
        anyhow::bail!("Gemini entry must not be empty");
    }
    if entry.contains('"') || entry.contains('\'') {
        anyhow::bail!(
            "Gemini entry contains a quote character — rejected for injection safety: {entry:?}"
        );
    }
    if entry
        .chars()
        .any(|c| c == '\n' || c == '\r' || c.is_ascii_control())
    {
        anyhow::bail!(
            "Gemini entry contains a newline or control character — rejected for injection safety: {entry:?}"
        );
    }
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::init::MAX_SETTINGS_SIZE;
    use crate::cmd::permissions::permissions_protocol_for_agent;
    use crate::cmd::permissions::seeded_entries;
    use crate::cmd::session::AgentKind;

    fn protocol() -> Box<dyn PermissionsProtocol> {
        permissions_protocol_for_agent(AgentKind::GeminiCli).unwrap()
    }

    // ---- render_entry ----

    #[test]
    fn test_render_entry_produces_skim_prefix() {
        let p = protocol();
        assert_eq!(p.render_entry("df"), "skim df");
        assert_eq!(p.render_entry("grep"), "skim grep");
    }

    // ---- generate_toml ----

    #[test]
    fn test_generate_toml_contains_rule_tables() {
        let entries = vec!["skim df".to_string(), "skim grep".to_string()];
        let content = generate_toml(&entries).unwrap();
        assert!(content.contains("[[rule]]"), "must contain [[rule]] tables");
        assert!(
            content.contains("run_shell_command"),
            "must reference Gemini tool name"
        );
        assert!(content.contains("skim df"), "must contain first pattern");
        assert!(content.contains("skim grep"), "must contain second pattern");
        assert!(
            content.contains("action = \"allow\""),
            "must contain allow action"
        );
    }

    #[test]
    fn test_generate_toml_has_header_comment() {
        let entries = vec!["skim df".to_string()];
        let content = generate_toml(&entries).unwrap();
        assert!(
            content.starts_with("# skim-managed"),
            "must start with skim header comment"
        );
    }

    // ---- validate_gemini_entry ----

    #[test]
    fn test_validate_rejects_double_quote() {
        assert!(validate_gemini_entry("skim \"df\"").is_err());
    }

    #[test]
    fn test_validate_rejects_single_quote() {
        assert!(validate_gemini_entry("skim 'df'").is_err());
    }

    #[test]
    fn test_validate_rejects_newline() {
        assert!(validate_gemini_entry("skim df\nmalicious").is_err());
    }

    #[test]
    fn test_validate_rejects_control_char() {
        assert!(validate_gemini_entry("skim df\x01").is_err());
    }

    #[test]
    fn test_validate_rejects_empty() {
        assert!(validate_gemini_entry("").is_err());
    }

    #[test]
    fn test_validate_accepts_valid_pattern() {
        assert!(validate_gemini_entry("skim df").is_ok());
        assert!(validate_gemini_entry("skim grep").is_ok());
        assert!(validate_gemini_entry("skim rg").is_ok());
    }

    // ---- seed: happy path ----

    #[test]
    fn test_seed_creates_toml_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = seeded_entries(p.as_ref());

        let outcome = p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();
        assert!(
            matches!(outcome, SeedOutcome::Added { .. }),
            "first seed must report Added"
        );

        let toml_path = dir.path().join("policies/skim.toml");
        assert!(toml_path.exists(), "TOML file must be created");

        let contents = std::fs::read_to_string(&toml_path).unwrap();
        for entry in &entries {
            assert!(
                contents.contains(entry.as_str()),
                "TOML must contain entry: {entry}"
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
        assert!(sidecar_path.exists(), "sidecar must be created");
        let sidecar = load_sidecar(&sidecar_path).unwrap();
        assert_eq!(sidecar.entries, entries);
        assert!(!sidecar.config_hash.is_empty());
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

    // ---- seed: injection rejection ----

    #[test]
    fn test_seed_rejects_entry_with_quotes() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = vec!["skim \"injected\"".to_string()];
        let result = p.seed(dir.path(), PermissionsTier::Seed, &entries);
        assert!(result.is_err(), "entry with quotes must be rejected");
        assert!(
            !dir.path().join("policies/skim.toml").exists(),
            "no file must be written after injection rejection"
        );
    }

    #[test]
    fn test_seed_rejects_entry_with_newline() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = vec!["skim df\nmalicious".to_string()];
        let result = p.seed(dir.path(), PermissionsTier::Seed, &entries);
        assert!(result.is_err(), "entry with newline must be rejected");
    }

    // ---- remove_seeded ----

    #[test]
    fn test_remove_seeded_deletes_file_when_hash_matches() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = seeded_entries(p.as_ref());

        p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();
        let toml_path = dir.path().join("policies/skim.toml");
        assert!(toml_path.exists());

        let outcome = p.remove_seeded(dir.path()).unwrap();
        assert!(matches!(outcome, RemoveOutcome::Removed { .. }));
        assert!(
            !toml_path.exists(),
            "TOML file must be deleted after remove_seeded"
        );
    }

    #[test]
    fn test_remove_seeded_fails_when_file_modified() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = seeded_entries(p.as_ref());

        p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();

        // Modify the file to trigger hash mismatch.
        let toml_path = dir.path().join("policies/skim.toml");
        std::fs::write(&toml_path, b"# tampered\n").unwrap();

        let result = p.remove_seeded(dir.path());
        assert!(result.is_err(), "hash mismatch must cause error");
    }

    #[test]
    fn test_remove_seeded_missing_sidecar_fails_loud() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let result = p.remove_seeded(dir.path());
        assert!(result.is_err(), "missing sidecar must return Err");
    }

    // ---- oversized file ----

    #[test]
    fn test_seed_oversized_existing_file_is_replaced_not_error() {
        // Gemini owns the file: an oversized EXISTING file gets replaced (wholesale).
        // The oversized check only applies to user-owned files. No error expected.
        let dir = tempfile::TempDir::new().unwrap();
        let toml_dir = dir.path().join("policies");
        std::fs::create_dir_all(&toml_dir).unwrap();
        let toml_path = toml_dir.join("skim.toml");

        // Write an "oversized" file (Gemini owns it, so it's replaced).
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut file = std::fs::File::create(&toml_path).unwrap();
            file.seek(SeekFrom::Start(MAX_SETTINGS_SIZE + 1)).unwrap();
            file.write_all(b"x").unwrap();
        }

        let p = protocol();
        let entries = vec!["skim df".to_string()];
        // Seed should succeed — Gemini replaces the file wholesale.
        let result = p.seed(dir.path(), PermissionsTier::Seed, &entries);
        assert!(
            result.is_ok(),
            "Gemini seed must replace oversized owned file: {:?}",
            result
        );
    }
}
