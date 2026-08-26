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

use anyhow::Context;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Compute SHA-256 hash of file contents, returning the hex-encoded digest.
///
/// The error carries the path: `create_hook_script` propagates this failure to
/// the user with `?`, and a bare "Permission denied (os error 13)" would not be
/// actionable.
pub(crate) fn compute_file_hash(path: &Path) -> anyhow::Result<String> {
    let contents = std::fs::read(path)
        .with_context(|| format!("cannot read {} to compute its hash", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&contents);
    let result = hasher.finalize();
    Ok(format!("{:x}", result))
}

/// Write a hash manifest for an agent's hook script.
///
/// Creates the manifest at `{config_dir}/hooks/skim-{agent_cli_name}.sha256`.
/// The manifest contains a single line: `sha256:<hash>  <script_name>\n`.
///
/// Errors carry the manifest path and a remediation hint: `create_hook_script`
/// propagates this failure to the user with `?` (installing without tamper
/// detection is worse than a hard error), so the message must say which file
/// could not be written and what to do about it.
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
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create hook directory {}", parent.display()))?;
    }
    std::fs::write(&manifest_path, content).with_context(|| {
        format!(
            "cannot write integrity manifest {}\n\
             hint: the hook directory must be writable — skim refuses to install a hook \
             it cannot later verify",
            manifest_path.display()
        )
    })?;
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

/// Four-state integrity classification for a hook script.
///
/// Used by `skim doctor` to derive its verdict from the SHA-256 manifest
/// (an independent artefact) rather than from the hook script bytes
/// themselves — which are exactly what a tamper modifies (PF-016).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ScriptIntegrity {
    /// Hash matches the stored manifest — script is unmodified.
    Verified,
    /// No manifest present — pre-manifest install (backward compat).
    /// Treat as advisory, not failure.
    NoManifest,
    /// Script contents differ from the stored hash — tampered.
    Tampered,
    /// Script file cannot be read (missing, permission denied, etc.).
    Unreadable,
}

/// Classify the integrity of a hook script against its stored SHA-256 manifest.
///
/// Returns:
/// - `Verified`   — hash matches.
/// - `NoManifest` — no manifest file (pre-manifest install; backward compat).
/// - `Tampered`   — stored hash differs from current file hash.
/// - `Unreadable` — script file cannot be read (I/O error).
///
/// The real signatures of the helpers this calls:
/// - `read_hash_manifest(config_dir, agent_cli_name) -> Option<String>` — `None` when absent.
/// - `compute_file_hash(path) -> anyhow::Result<String>` — `Err` on I/O failure.
pub(crate) fn classify_script_integrity(
    config_dir: &Path,
    agent_cli_name: &str,
    script_path: &Path,
) -> ScriptIntegrity {
    let Some(stored) = read_hash_manifest(config_dir, agent_cli_name) else {
        return ScriptIntegrity::NoManifest;
    };
    match compute_file_hash(script_path) {
        Ok(cur) if cur == stored => ScriptIntegrity::Verified,
        Ok(_) => ScriptIntegrity::Tampered,
        Err(_) => ScriptIntegrity::Unreadable,
    }
}

/// Verify script integrity against stored hash.
///
/// Thin bool wrapper over [`classify_script_integrity`] — preserves the
/// existing call contract so the two existing callers
/// (`cmd/rewrite/hook.rs` and `cmd/init/uninstall.rs`) are unaffected.
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
    match classify_script_integrity(config_dir, agent_cli_name, script_path) {
        ScriptIntegrity::Verified | ScriptIntegrity::NoManifest => Ok(true),
        ScriptIntegrity::Tampered => Ok(false),
        ScriptIntegrity::Unreadable => {
            anyhow::bail!("cannot read hook script: {}", script_path.display())
        }
    }
}

/// Delete hash manifest for an agent. No-op if the file does not exist.
pub(crate) fn remove_hash_manifest(config_dir: &Path, agent_cli_name: &str) -> anyhow::Result<()> {
    let path = manifest_path(config_dir, agent_cli_name);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// Write hash manifest for an awareness file.
///
/// Uses the key pattern `{agent_cli_name}-awareness` to track generated awareness
/// files separately from hook scripts. This enables uninstall to detect user
/// modifications and require `--force` for tampered awareness files.
#[allow(dead_code)] // Used in tests; consumed when init writes awareness files for non-Claude agents
pub(crate) fn write_awareness_hash(
    config_dir: &Path,
    agent_cli_name: &str,
    awareness_path: &Path,
) -> anyhow::Result<()> {
    let hash = compute_file_hash(awareness_path)?;
    let key = format!("{agent_cli_name}-awareness");
    let file_name = awareness_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("awareness");
    write_hash_manifest(config_dir, &key, file_name, &hash)
}

/// Verify integrity of an awareness file against stored hash.
///
/// Returns `Ok(true)` if valid or no manifest (backward compat), `Ok(false)` if tampered.
#[allow(dead_code)] // Used in tests; consumed when uninstall checks awareness file integrity
pub(crate) fn verify_awareness_integrity(
    config_dir: &Path,
    agent_cli_name: &str,
    awareness_path: &Path,
) -> anyhow::Result<bool> {
    let key = format!("{agent_cli_name}-awareness");
    verify_script_integrity(config_dir, &key, awareness_path)
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

    #[test]
    fn test_awareness_hash_round_trip() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        std::fs::create_dir_all(config_dir.join("hooks")).unwrap();

        // Create a fake awareness file
        let awareness_path = config_dir.join("AGENTS.md");
        std::fs::write(
            &awareness_path,
            "# skim awareness\nGenerated by skim init\n",
        )
        .unwrap();

        // Write awareness hash
        write_awareness_hash(config_dir, "crush", &awareness_path).unwrap();

        // Verify — should be valid
        let valid = verify_awareness_integrity(config_dir, "crush", &awareness_path).unwrap();
        assert!(valid, "freshly written awareness hash should verify");

        // Tamper with the awareness file
        std::fs::write(&awareness_path, "# modified by user\n").unwrap();

        // Verify — should be tampered
        let valid = verify_awareness_integrity(config_dir, "crush", &awareness_path).unwrap();
        assert!(!valid, "modified awareness file should fail verification");
    }

    // ---- classify_script_integrity ----

    #[test]
    fn test_classify_verified_when_hash_matches() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        std::fs::create_dir_all(config_dir.join("hooks")).unwrap();

        let script_path = config_dir.join("hooks/skim-rewrite.sh");
        std::fs::write(&script_path, "#!/bin/bash\nexec skim rewrite --hook\n").unwrap();
        let hash = compute_file_hash(&script_path).unwrap();
        write_hash_manifest(config_dir, "claude-code", "skim-rewrite.sh", &hash).unwrap();

        let result = classify_script_integrity(config_dir, "claude-code", &script_path);
        assert_eq!(result, ScriptIntegrity::Verified);
    }

    #[test]
    fn test_classify_no_manifest_when_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        std::fs::create_dir_all(config_dir.join("hooks")).unwrap();

        let script_path = config_dir.join("hooks/skim-rewrite.sh");
        std::fs::write(&script_path, "#!/bin/bash\nexec skim rewrite --hook\n").unwrap();
        // No manifest written.

        let result = classify_script_integrity(config_dir, "claude-code", &script_path);
        assert_eq!(result, ScriptIntegrity::NoManifest);
    }

    #[test]
    fn test_classify_tampered_when_hash_differs() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        std::fs::create_dir_all(config_dir.join("hooks")).unwrap();

        let script_path = config_dir.join("hooks/skim-rewrite.sh");
        std::fs::write(&script_path, "#!/bin/bash\nexec skim rewrite --hook\n").unwrap();
        let hash = compute_file_hash(&script_path).unwrap();
        write_hash_manifest(config_dir, "claude-code", "skim-rewrite.sh", &hash).unwrap();

        // Tamper: append one byte.
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&script_path)
            .unwrap();
        f.write_all(b"X").unwrap();

        let result = classify_script_integrity(config_dir, "claude-code", &script_path);
        assert_eq!(result, ScriptIntegrity::Tampered);
    }

    #[test]
    fn test_classify_unreadable_when_file_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        std::fs::create_dir_all(config_dir.join("hooks")).unwrap();

        // Write manifest but no script file.
        write_hash_manifest(
            config_dir,
            "claude-code",
            "skim-rewrite.sh",
            "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2",
        )
        .unwrap();
        let missing = config_dir.join("hooks/skim-rewrite.sh");

        let result = classify_script_integrity(config_dir, "claude-code", &missing);
        assert_eq!(result, ScriptIntegrity::Unreadable);
    }

    #[test]
    fn test_verify_script_integrity_is_thin_wrapper_over_classifier() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        std::fs::create_dir_all(config_dir.join("hooks")).unwrap();

        let script_path = config_dir.join("hooks/skim-rewrite.sh");
        std::fs::write(&script_path, "#!/bin/bash\nexec skim rewrite --hook\n").unwrap();
        let hash = compute_file_hash(&script_path).unwrap();
        write_hash_manifest(config_dir, "claude-code", "skim-rewrite.sh", &hash).unwrap();

        // Verified → Ok(true)
        assert!(verify_script_integrity(config_dir, "claude-code", &script_path).unwrap());

        // NoManifest → Ok(true) (backward compat)
        let no_manifest_dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(no_manifest_dir.path().join("hooks")).unwrap();
        std::fs::write(no_manifest_dir.path().join("hooks/skim-rewrite.sh"), "x").unwrap();
        assert!(
            verify_script_integrity(
                no_manifest_dir.path(),
                "claude-code",
                &no_manifest_dir.path().join("hooks/skim-rewrite.sh")
            )
            .unwrap()
        );
    }

    #[test]
    fn test_awareness_hash_missing_manifest() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();

        let awareness_path = config_dir.join("AGENTS.md");
        std::fs::write(&awareness_path, "# some content\n").unwrap();

        // No manifest written — should return Ok(true) for backward compat
        let valid = verify_awareness_integrity(config_dir, "codex", &awareness_path).unwrap();
        assert!(
            valid,
            "missing manifest should be treated as valid (backward compat)"
        );
    }
}
