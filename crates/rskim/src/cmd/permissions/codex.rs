//! Codex CLI permissions writer.
//!
//! Targets `{config_dir}/rules/skim.rules` — a skim-OWNED file containing
//! Starlark-flavored prefix-rule lines. The entire file is **wholesale-replaced**
//! on every seed. skim never merges into an existing Codex rules file.
//!
//! ## File ownership
//!
//! `rules/skim.rules` is **skim-owned**: the entire file is regenerated from
//! the current entry list on each seed. No backup is taken (skim-owned files
//! need no user backup).
//!
//! ## Entry model
//!
//! Entries stored in the sidecar (and passed to `seed()`) are **raw tool names**
//! (e.g. `"df"`, `"grep"`) — the narrowest representation. Each tool name is
//! wrapped into a Starlark allow line when writing the rules file:
//!
//! ```text
//! allow("Bash", "skim df")
//! ```
//!
//! ## Security
//!
//! Every entry (tool token) must match `[A-Za-z0-9._/-]+`. Anything outside
//! that charset is rejected with an error before any write — Starlark injection
//! defense.
//!
//! ## HookSupport independence
//!
//! Codex CLI is `HookSupport::AwarenessOnly` for hook installation (`cmd/hooks/codex.rs`).
//! Permissions capability is **independent** of hook support — Codex can receive
//! permission entries even though it has no runtime hook. The permissions file
//! is read at launch time by Codex, not via a hook event.
//!
//! ## Blanket tier refusal
//!
//! Codex structurally refuses the `Blanket` tier: blanket-tier grants would
//! allow unrestricted tool access, which is incompatible with the Codex
//! narrowest-set model. The `seed()` method returns `Err` if called with
//! `PermissionsTier::Blanket`.

use std::path::Path;

use crate::cmd::integrity::compute_file_hash;
use crate::cmd::permissions::sidecar::{
    PermissionSidecar, SIDECAR_FILENAME, load_sidecar, write_sidecar,
};
use crate::cmd::permissions::{PermissionsProtocol, PermissionsTier, RemoveOutcome, SeedOutcome};

/// Config path (relative to config_dir) for the Codex rules file.
const CONFIG_FILENAME: &str = "rules/skim.rules";

pub(super) struct CodexPermissions;

impl PermissionsProtocol for CodexPermissions {
    fn agent_label(&self) -> &str {
        "Codex CLI"
    }

    fn config_filename(&self) -> &str {
        CONFIG_FILENAME
    }

    /// Returns the raw tool name (e.g. `"df"`).
    ///
    /// The Codex writer wraps this into a Starlark line at write time.
    /// Entries stored in the sidecar are these raw tool names.
    fn render_entry(&self, tool: &str) -> String {
        tool.to_string()
    }

    fn seed(
        &self,
        config_dir: &Path,
        tier: PermissionsTier,
        entries: &[String],
    ) -> anyhow::Result<SeedOutcome> {
        // Blanket tier is structurally refused for Codex.
        if tier == PermissionsTier::Blanket {
            anyhow::bail!(
                "Codex CLI does not support the blanket permission tier: \
                 blanket-tier grants would allow unrestricted tool access, \
                 which is incompatible with the Codex narrowest-set model. \
                 Use --permissions-tier=seed (the default) instead."
            );
        }

        if entries.is_empty() {
            return Ok(SeedOutcome::AlreadyCurrent);
        }

        // Validate all entries before any write.
        for entry in entries {
            validate_codex_entry(entry)?;
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

        // Generate the rules file content.
        let content = generate_rules(entries);

        // Ensure parent directory exists.
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Atomic write (tmp + rename).
        let tmp_path = config_path.with_extension("rules.tmp");
        std::fs::write(&tmp_path, &content)?;
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
                "cannot remove Codex permissions: {}\n\
                 hint: if the sidecar was deleted manually, remove {} manually",
                e,
                config_dir.join(CONFIG_FILENAME).display()
            )
        })?;

        let config_path = config_dir.join(CONFIG_FILENAME);
        if !config_path.exists() {
            let _ = std::fs::remove_file(&sidecar_path);
            return Ok(RemoveOutcome::NothingToRemove);
        }

        // Verify hash before deleting.
        let current_hash = compute_file_hash(&config_path)?;
        if current_hash != sidecar.config_hash {
            anyhow::bail!(
                "Codex rules file hash mismatch — file may have been modified manually: {}\n\
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

/// Validate a Codex entry (raw tool token).
///
/// Every entry must match `[A-Za-z0-9._/-]+` — no spaces, no shell-special
/// characters, no empty strings. This is the Starlark injection charset.
fn validate_codex_entry(entry: &str) -> anyhow::Result<()> {
    if entry.is_empty() {
        anyhow::bail!("Codex entry must not be empty (Starlark injection rejection)");
    }
    if !entry
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '/' || c == '-')
    {
        anyhow::bail!(
            "Codex entry contains invalid characters — must match [A-Za-z0-9._/-]+: {entry:?}\n\
             hint: Starlark injection rejection; these can never be legitimate tool tokens"
        );
    }
    Ok(())
}

/// Generate Starlark-flavored rules file content from raw tool name entries.
///
/// Each entry `"df"` becomes a line: `allow("Bash", "skim df")`
fn generate_rules(entries: &[String]) -> String {
    let mut out = String::new();
    out.push_str("# skim-managed tool permissions — do not edit manually\n");
    out.push_str("# Regenerate with: skim init --permissions --agent codex\n");
    out.push('\n');
    for entry in entries {
        out.push_str(&format!("allow(\"Bash\", \"skim {entry}\")\n"));
    }
    out
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
        permissions_protocol_for_agent(AgentKind::CodexCli).unwrap()
    }

    // ---- render_entry ----

    #[test]
    fn test_render_entry_returns_raw_tool_name() {
        let p = protocol();
        assert_eq!(p.render_entry("df"), "df");
        assert_eq!(p.render_entry("grep"), "grep");
        assert_eq!(p.render_entry("ls"), "ls");
    }

    // ---- generate_rules ----

    #[test]
    fn test_generate_rules_wraps_tool_names_in_starlark() {
        let entries = vec!["df".to_string(), "grep".to_string()];
        let content = generate_rules(&entries);
        assert!(
            content.contains("allow(\"Bash\", \"skim df\")"),
            "must contain Starlark allow line for df"
        );
        assert!(
            content.contains("allow(\"Bash\", \"skim grep\")"),
            "must contain Starlark allow line for grep"
        );
    }

    #[test]
    fn test_generate_rules_has_header_comment() {
        let content = generate_rules(&["df".to_string()]);
        assert!(
            content.starts_with("# skim-managed"),
            "must start with header comment"
        );
    }

    // ---- validate_codex_entry ----

    #[test]
    fn test_validate_accepts_valid_tool_names() {
        assert!(validate_codex_entry("df").is_ok());
        assert!(validate_codex_entry("grep").is_ok());
        assert!(validate_codex_entry("rg").is_ok());
        assert!(validate_codex_entry("tree").is_ok());
        assert!(validate_codex_entry("wc").is_ok());
        assert!(validate_codex_entry("diff").is_ok());
        assert!(validate_codex_entry("du").is_ok());
        assert!(validate_codex_entry("ls").is_ok());
    }

    #[test]
    fn test_validate_rejects_empty() {
        assert!(
            validate_codex_entry("").is_err(),
            "empty entry must be rejected"
        );
    }

    #[test]
    fn test_validate_rejects_space() {
        // "skim df" has a space — must be rejected (Codex entries are raw tool names)
        assert!(
            validate_codex_entry("skim df").is_err(),
            "entry with space must be rejected"
        );
    }

    #[test]
    fn test_validate_rejects_quote() {
        assert!(
            validate_codex_entry("df\"").is_err(),
            "entry with quote must be rejected"
        );
        assert!(
            validate_codex_entry("df'").is_err(),
            "entry with single quote must be rejected"
        );
    }

    #[test]
    fn test_validate_rejects_newline() {
        assert!(
            validate_codex_entry("df\nmalicious").is_err(),
            "entry with newline must be rejected"
        );
    }

    #[test]
    fn test_validate_rejects_semicolon() {
        assert!(
            validate_codex_entry("df;rm -rf /").is_err(),
            "entry with semicolon must be rejected"
        );
    }

    #[test]
    fn test_validate_accepts_dots_dashes_slashes() {
        // Valid chars in charset: letters, digits, '.', '_', '/', '-'
        assert!(validate_codex_entry("my-tool").is_ok());
        assert!(validate_codex_entry("my.tool").is_ok());
        assert!(validate_codex_entry("my_tool").is_ok());
        assert!(validate_codex_entry("sub/cmd").is_ok());
    }

    // ---- blanket tier refusal ----

    #[test]
    fn test_seed_blanket_tier_returns_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = vec!["df".to_string()];
        let result = p.seed(dir.path(), PermissionsTier::Blanket, &entries);
        assert!(result.is_err(), "blanket tier must be refused");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("blanket") || err.contains("Blanket"),
            "error must name the blanket tier: {err}"
        );
        // No file must be written.
        assert!(
            !dir.path().join("rules/skim.rules").exists(),
            "no rules file must be created after blanket tier refusal"
        );
    }

    #[test]
    fn test_seed_seed_tier_succeeds() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = vec!["df".to_string()];
        let result = p.seed(dir.path(), PermissionsTier::Seed, &entries);
        assert!(result.is_ok(), "seed tier must succeed: {:?}", result);
    }

    #[test]
    fn test_seed_mirror_tier_succeeds() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = vec!["df".to_string()];
        let result = p.seed(dir.path(), PermissionsTier::Mirror, &entries);
        assert!(result.is_ok(), "mirror tier must succeed: {:?}", result);
    }

    // ---- seed: happy path ----

    #[test]
    fn test_seed_creates_rules_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = seeded_entries(p.as_ref()); // raw tool names: ["df", "diff", ...]

        let outcome = p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();
        assert!(
            matches!(outcome, SeedOutcome::Added { .. }),
            "first seed must report Added"
        );

        let rules_path = dir.path().join("rules/skim.rules");
        assert!(rules_path.exists(), "rules file must be created");

        let contents = std::fs::read_to_string(&rules_path).unwrap();
        // Each tool name should appear in the file as a Starlark line.
        for entry in &entries {
            let expected_line = format!("allow(\"Bash\", \"skim {entry}\")");
            assert!(
                contents.contains(&expected_line),
                "rules file must contain: {expected_line}"
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
    fn test_seed_rejects_entry_with_space() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = vec!["skim df".to_string()]; // space not allowed in Codex entries
        let result = p.seed(dir.path(), PermissionsTier::Seed, &entries);
        assert!(result.is_err(), "entry with space must be rejected");
        assert!(
            !dir.path().join("rules/skim.rules").exists(),
            "no file must be written after injection rejection"
        );
    }

    #[test]
    fn test_seed_rejects_entry_with_quote() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = vec!["df\"".to_string()];
        let result = p.seed(dir.path(), PermissionsTier::Seed, &entries);
        assert!(result.is_err(), "entry with quote must be rejected");
    }

    // ---- remove_seeded ----

    #[test]
    fn test_remove_seeded_deletes_file_when_hash_matches() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = seeded_entries(p.as_ref());

        p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();
        let rules_path = dir.path().join("rules/skim.rules");
        assert!(rules_path.exists());

        let outcome = p.remove_seeded(dir.path()).unwrap();
        assert!(matches!(outcome, RemoveOutcome::Removed { .. }));
        assert!(
            !rules_path.exists(),
            "rules file must be deleted after remove_seeded"
        );
    }

    #[test]
    fn test_remove_seeded_fails_when_file_modified() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = protocol();
        let entries = seeded_entries(p.as_ref());

        p.seed(dir.path(), PermissionsTier::Seed, &entries).unwrap();

        // Modify the file to trigger hash mismatch.
        let rules_path = dir.path().join("rules/skim.rules");
        std::fs::write(&rules_path, b"# tampered\n").unwrap();

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
}
