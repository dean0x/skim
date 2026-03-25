//! SHA-256 hook integrity verification (#57).
//!
//! Provides hash-based tamper detection for skim hook scripts. Each agent's
//! hook script gets a companion `.sha256` manifest file stored alongside the
//! hook in `{config_dir}/hooks/`. The manifest format is:
//!
//! ```text
//! sha256:<hex_digest>  <script_name>
//! ```
//!
//! Verification follows the behavior matrix:
//! - Hook execution: log-only warnings (NEVER stderr -- GRANITE #361 Bug 3)
//! - Uninstall: stderr warning, require `--force` if tampered
//! - Install/upgrade: always recompute hash

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Compute SHA-256 hash of file contents, returning the hex-encoded digest.
pub(crate) fn compute_file_hash(path: &Path) -> anyhow::Result<String> {
    let contents = std::fs::read(path)?;
    let mut hasher = Sha256::new();
    hasher.update(&contents);
    let result = hasher.finalize();
    Ok(format!("{:x}", result))
}

/// Write a hash manifest for an agent's hook script.
///
/// Creates the manifest at `{config_dir}/hooks/skim-{agent_cli_name}.sha256`.
/// The manifest contains a single line: `sha256:<hash>  <script_name>\n`.
pub(crate) fn write_hash_manifest(
    config_dir: &Path,
    agent_cli_name: &str,
    script_name: &str,
    hash: &str,
) -> anyhow::Result<()> {
    let manifest_path = manifest_path(config_dir, agent_cli_name);
    let content = format!("sha256:{hash}  {script_name}\n");
    // Ensure the hooks directory exists (caller may have already created it,
    // but this is idempotent).
    if let Some(parent) = manifest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&manifest_path, content)?;
    Ok(())
}

/// Read hash from manifest file. Returns `None` if the manifest is missing
/// or cannot be parsed.
pub(crate) fn read_hash_manifest(config_dir: &Path, agent_cli_name: &str) -> Option<String> {
    let path = manifest_path(config_dir, agent_cli_name);
    let content = std::fs::read_to_string(&path).ok()?;
    content
        .strip_prefix("sha256:")
        .and_then(|s| s.split_whitespace().next())
        .map(|s| s.to_string())
}

/// Verify script integrity against stored hash.
///
/// Returns:
/// - `Ok(true)` if the hash matches OR if no manifest exists (backward compat)
/// - `Ok(false)` if the stored hash differs from the current file hash (tampered)
/// - `Err` if the script file cannot be read
pub(crate) fn verify_script_integrity(
    config_dir: &Path,
    agent_cli_name: &str,
    script_path: &Path,
) -> anyhow::Result<bool> {
    let stored_hash = match read_hash_manifest(config_dir, agent_cli_name) {
        Some(h) => h,
        None => return Ok(true), // Missing hash = backward compat, treat as valid
    };
    let current_hash = compute_file_hash(script_path)?;
    Ok(stored_hash == current_hash)
}

/// Delete hash manifest for an agent. No-op if the file does not exist.
pub(crate) fn remove_hash_manifest(config_dir: &Path, agent_cli_name: &str) -> anyhow::Result<()> {
    let path = manifest_path(config_dir, agent_cli_name);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// Compute the manifest file path for a given agent.
fn manifest_path(config_dir: &Path, agent_cli_name: &str) -> PathBuf {
    config_dir
        .join("hooks")
        .join(format!("skim-{agent_cli_name}.sha256"))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_file_hash_deterministic() {
        let dir = tempfile::TempDir::new().unwrap();
        let file_path = dir.path().join("test.sh");
        std::fs::write(&file_path, "#!/bin/bash\necho hello\n").unwrap();

        let hash1 = compute_file_hash(&file_path).unwrap();
        let hash2 = compute_file_hash(&file_path).unwrap();

        assert_eq!(hash1, hash2, "Same file contents should produce same hash");
        assert_eq!(hash1.len(), 64, "SHA-256 hex digest should be 64 chars");
        // Verify it's valid hex
        assert!(
            hash1.chars().all(|c| c.is_ascii_hexdigit()),
            "Hash should be hex"
        );
    }

    #[test]
    fn test_compute_file_hash_different_content() {
        let dir = tempfile::TempDir::new().unwrap();
        let file1 = dir.path().join("a.sh");
        let file2 = dir.path().join("b.sh");
        std::fs::write(&file1, "content A").unwrap();
        std::fs::write(&file2, "content B").unwrap();

        let hash1 = compute_file_hash(&file1).unwrap();
        let hash2 = compute_file_hash(&file2).unwrap();

        assert_ne!(
            hash1, hash2,
            "Different content should produce different hashes"
        );
    }

    #[test]
    fn test_write_and_read_hash_manifest() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        std::fs::create_dir_all(config_dir.join("hooks")).unwrap();

        let hash = "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2";
        write_hash_manifest(config_dir, "claude-code", "skim-rewrite.sh", hash).unwrap();

        let read_back = read_hash_manifest(config_dir, "claude-code");
        assert_eq!(read_back, Some(hash.to_string()));

        // Verify manifest file content format
        let manifest = config_dir.join("hooks/skim-claude-code.sha256");
        let content = std::fs::read_to_string(&manifest).unwrap();
        assert_eq!(content, format!("sha256:{hash}  skim-rewrite.sh\n"));
    }

    #[test]
    fn test_read_hash_manifest_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = read_hash_manifest(dir.path(), "nonexistent-agent");
        assert_eq!(result, None);
    }

    #[test]
    fn test_verify_script_integrity_valid() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        std::fs::create_dir_all(config_dir.join("hooks")).unwrap();

        // Create a script file
        let script_path = config_dir.join("hooks/skim-rewrite.sh");
        std::fs::write(&script_path, "#!/bin/bash\nexec skim rewrite --hook\n").unwrap();

        // Compute and store hash
        let hash = compute_file_hash(&script_path).unwrap();
        write_hash_manifest(config_dir, "claude-code", "skim-rewrite.sh", &hash).unwrap();

        // Verify -- should be valid
        let result = verify_script_integrity(config_dir, "claude-code", &script_path).unwrap();
        assert!(result, "Unmodified script should verify as valid");
    }

    #[test]
    fn test_verify_script_integrity_tampered() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        std::fs::create_dir_all(config_dir.join("hooks")).unwrap();

        // Create a script file and store its hash
        let script_path = config_dir.join("hooks/skim-rewrite.sh");
        std::fs::write(&script_path, "#!/bin/bash\nexec skim rewrite --hook\n").unwrap();
        let hash = compute_file_hash(&script_path).unwrap();
        write_hash_manifest(config_dir, "claude-code", "skim-rewrite.sh", &hash).unwrap();

        // Tamper with the script
        std::fs::write(&script_path, "#!/bin/bash\nexec malicious-command\n").unwrap();

        // Verify -- should be tampered
        let result = verify_script_integrity(config_dir, "claude-code", &script_path).unwrap();
        assert!(!result, "Modified script should verify as tampered");
    }

    #[test]
    fn test_verify_script_integrity_missing_hash_backward_compat() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        std::fs::create_dir_all(config_dir.join("hooks")).unwrap();

        // Create a script file but NO hash manifest
        let script_path = config_dir.join("hooks/skim-rewrite.sh");
        std::fs::write(&script_path, "#!/bin/bash\nexec skim rewrite --hook\n").unwrap();

        // Verify -- should treat as valid (backward compat)
        let result = verify_script_integrity(config_dir, "claude-code", &script_path).unwrap();
        assert!(
            result,
            "Missing hash manifest should be treated as valid (backward compat)"
        );
    }

    #[test]
    fn test_remove_hash_manifest() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        std::fs::create_dir_all(config_dir.join("hooks")).unwrap();

        // Create manifest
        write_hash_manifest(config_dir, "claude-code", "skim-rewrite.sh", "abc123").unwrap();
        assert!(config_dir.join("hooks/skim-claude-code.sha256").exists());

        // Remove it
        remove_hash_manifest(config_dir, "claude-code").unwrap();
        assert!(!config_dir.join("hooks/skim-claude-code.sha256").exists());
    }

    #[test]
    fn test_remove_hash_manifest_nonexistent_is_noop() {
        let dir = tempfile::TempDir::new().unwrap();
        // Should not error when manifest doesn't exist
        let result = remove_hash_manifest(dir.path(), "nonexistent");
        assert!(result.is_ok());
    }

    #[test]
    fn test_write_hash_manifest_creates_hooks_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        // hooks/ dir does NOT exist yet

        write_hash_manifest(config_dir, "claude-code", "skim-rewrite.sh", "abc123").unwrap();
        assert!(config_dir.join("hooks/skim-claude-code.sha256").exists());
    }

    #[test]
    fn test_upgrade_recomputes_hash() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        std::fs::create_dir_all(config_dir.join("hooks")).unwrap();

        let script_path = config_dir.join("hooks/skim-rewrite.sh");

        // Version 1 content
        let v1_content = "#!/bin/bash\n# skim-hook v1.0.0\nexec skim rewrite --hook\n";
        std::fs::write(&script_path, v1_content).unwrap();
        let hash_v1 = compute_file_hash(&script_path).unwrap();
        write_hash_manifest(config_dir, "claude-code", "skim-rewrite.sh", &hash_v1).unwrap();

        // Simulate upgrade: overwrite with new version
        let v2_content = "#!/bin/bash\n# skim-hook v2.0.0\nexec skim rewrite --hook\n";
        std::fs::write(&script_path, v2_content).unwrap();

        // Old hash should detect tamper
        let tampered = verify_script_integrity(config_dir, "claude-code", &script_path).unwrap();
        assert!(!tampered, "Old hash should detect new content");

        // Recompute hash (simulating what install does on upgrade)
        let hash_v2 = compute_file_hash(&script_path).unwrap();
        write_hash_manifest(config_dir, "claude-code", "skim-rewrite.sh", &hash_v2).unwrap();

        // New hash should verify
        let valid = verify_script_integrity(config_dir, "claude-code", &script_path).unwrap();
        assert!(valid, "Recomputed hash should verify after upgrade");
        assert_ne!(
            hash_v1, hash_v2,
            "Different content should yield different hashes"
        );
    }

    #[test]
    fn test_manifest_path_per_agent() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();

        let path_claude = manifest_path(config_dir, "claude-code");
        let path_cursor = manifest_path(config_dir, "cursor");

        assert_ne!(path_claude, path_cursor);
        assert!(path_claude.ends_with("skim-claude-code.sha256"));
        assert!(path_cursor.ends_with("skim-cursor.sha256"));
    }
}
