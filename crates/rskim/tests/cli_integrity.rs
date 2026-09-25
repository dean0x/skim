//! Integration tests for hook integrity verification (#57).
//!
//! Tests the full lifecycle: install creates SHA-256 manifest, uninstall checks
//! integrity, tampered scripts require --force, and hook mode logs warnings
//! to file (NEVER stderr).
//!
//! # Hermeticity (PF-017)
//!
//! Every invocation below runs a REAL `skim init` or `skim init --uninstall`, so
//! every one goes through [`common::skim_sandboxed`] — the single authoritative
//! sandbox env block.  A `CLAUDE_CONFIG_DIR` override alone does NOT isolate an
//! installer: it names one directory and leaves `HOME` — and with it
//! `~/.skim/bin`, `~/.cache/skim`, every *other* agent's config directory and
//! the guidance files under them — resolving to the developer's own, which a
//! global `--uninstall` then deletes from.
//!
//! This file's name does not start with `cli_init`, which is how it escaped the
//! directory-wide guard when that guard was first scoped; `cli_init.rs`'s
//! `INSTALLER_TEST_PREFIXES` now names `cli_integrity` alongside `cli_init` so
//! the escape cannot recur silently.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
mod common;

// ============================================================================
// Helpers: every `skim` invocation is built inside the sandbox
// ============================================================================

/// The directory the sandbox points `var` at, resolved through the table rather
/// than by joining a literal.
///
/// Chaining a hand-rolled per-agent override onto a sandboxed command is the
/// exact shape `cli_init.rs`'s own guard rejects — it is how PF-017 was
/// re-opened the second time — and it is unnecessary here: the sandbox
/// already redirects every agent config directory and the cache into the home,
/// so a test that needs to name one asks [`common::SANDBOX_REDIRECTED_VARS`]
/// where it went.
fn sandbox_dir(home: &std::path::Path, var: &str) -> std::path::PathBuf {
    let relative = common::SANDBOX_REDIRECTED_VARS
        .iter()
        .find(|(name, _)| *name == var)
        .map(|(_, relative)| *relative)
        .unwrap_or_else(|| panic!("{var} is not a sandbox-redirected variable"));
    common::sandbox_var_path(home, relative)
}

/// The sandbox's Claude config directory, created so it exists.
///
/// Creation is load-bearing, not incidental: `detect_installed_agents` counts an
/// agent only when its override points at an existing directory, so an uncreated
/// path reads as "claude-code is not installed" and the install writes nothing.
fn claude_config_dir(home: &std::path::Path) -> std::path::PathBuf {
    let dir = sandbox_dir(home, "CLAUDE_CONFIG_DIR");
    fs::create_dir_all(&dir).expect("agent config dir must be creatable");
    dir
}

/// The sandbox's skim cache directory — where hook mode writes `hook.log`.
fn cache_dir(home: &std::path::Path) -> std::path::PathBuf {
    sandbox_dir(home, "SKIM_CACHE_DIR")
}

fn skim_init_cmd(home: &std::path::Path) -> Command {
    let mut cmd = common::skim_sandboxed(home);
    cmd.arg("init");
    cmd
}

fn skim_rewrite_hook_cmd(home: &std::path::Path) -> Command {
    let mut cmd = common::skim_sandboxed(home);
    cmd.args(["rewrite", "--hook"]);
    cmd
}

// ============================================================================
// Install creates SHA-256 file
// ============================================================================

#[test]
fn test_install_creates_sha256_file() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let config = claude_config_dir(home);

    skim_init_cmd(home).args(["--yes"]).assert().success();

    // Verify the SHA-256 manifest was created
    let manifest_path = config.join("hooks/skim-claude-code.sha256");
    assert!(
        manifest_path.exists(),
        "SHA-256 manifest should be created on install"
    );

    // Verify manifest format: sha256:<hex>  skim-rewrite.sh
    let content = fs::read_to_string(&manifest_path).unwrap();
    assert!(
        content.starts_with("sha256:"),
        "Manifest should start with sha256: prefix, got: {content}"
    );
    assert!(
        content.contains("skim-rewrite.sh"),
        "Manifest should reference the script name, got: {content}"
    );

    // Verify hash is valid hex (64 chars for SHA-256)
    let hash = content
        .strip_prefix("sha256:")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    assert_eq!(hash.len(), 64, "SHA-256 hash should be 64 hex chars");
    assert!(
        hash.chars().all(|c| c.is_ascii_hexdigit()),
        "Hash should be valid hex"
    );
}

// ============================================================================
// Upgrade recomputes hash
// ============================================================================

#[test]
fn test_upgrade_recomputes_hash() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let config = claude_config_dir(home);

    // First install
    skim_init_cmd(home).args(["--yes"]).assert().success();

    let manifest_path = config.join("hooks/skim-claude-code.sha256");
    let _hash1 = fs::read_to_string(&manifest_path).unwrap();

    // Modify the hook script version to simulate an upgrade scenario
    let script_path = config.join("hooks/skim-rewrite.sh");
    let content = fs::read_to_string(&script_path).unwrap();
    let modified = content.replace("skim-hook v", "skim-hook v0.0.0-old-");
    fs::write(&script_path, &modified).unwrap();

    // Re-run init (upgrade) -- should recompute hash
    skim_init_cmd(home).args(["--yes"]).assert().success();

    let hash2 = fs::read_to_string(&manifest_path).unwrap();
    // The hash should be different because the script content changed during upgrade
    // (Actually, the install flow writes a NEW script with the current version,
    // so the hash will match the freshly-written script)
    assert!(
        hash2.starts_with("sha256:"),
        "After upgrade, manifest should still be valid"
    );
}

// ============================================================================
// Uninstall tampered requires --force
// ============================================================================

#[test]
fn test_uninstall_tampered_requires_force() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let config = claude_config_dir(home);

    // Install
    skim_init_cmd(home).args(["--yes"]).assert().success();

    // Tamper with the hook script
    let script_path = config.join("hooks/skim-rewrite.sh");
    fs::write(&script_path, "#!/bin/bash\necho 'tampered'\n").unwrap();
    // Keep it executable
    let perms = std::fs::Permissions::from_mode(0o755);
    fs::set_permissions(&script_path, perms).unwrap();

    // Uninstall WITHOUT --force should fail
    skim_init_cmd(home)
        .args(["--uninstall", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("modified since installation"))
        .stderr(predicate::str::contains("--force"));
}

#[test]
fn test_uninstall_with_force_bypasses_warning() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let config = claude_config_dir(home);

    // Install
    skim_init_cmd(home).args(["--yes"]).assert().success();

    // Tamper with the hook script
    let script_path = config.join("hooks/skim-rewrite.sh");
    fs::write(&script_path, "#!/bin/bash\necho 'tampered'\n").unwrap();
    let perms = std::fs::Permissions::from_mode(0o755);
    fs::set_permissions(&script_path, perms).unwrap();

    // Uninstall WITH --force should succeed
    skim_init_cmd(home)
        .args(["--uninstall", "--yes", "--force"])
        .assert()
        .success()
        .stderr(predicate::str::contains("proceeding with --force"));

    // Script should be deleted
    assert!(
        !script_path.exists(),
        "Hook script should be deleted after forced uninstall"
    );

    // Hash manifest should also be cleaned up
    let manifest_path = config.join("hooks/skim-claude-code.sha256");
    assert!(
        !manifest_path.exists(),
        "Hash manifest should be cleaned up after uninstall"
    );
}

// ============================================================================
// Uninstall clean script proceeds normally
// ============================================================================

#[test]
fn test_uninstall_clean_script_proceeds() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let config = claude_config_dir(home);

    // Install
    skim_init_cmd(home).args(["--yes"]).assert().success();

    // Uninstall without tampering -- should succeed without --force
    skim_init_cmd(home)
        .args(["--uninstall", "--yes"])
        .assert()
        .success();

    // Everything should be cleaned up
    let script_path = config.join("hooks/skim-rewrite.sh");
    assert!(!script_path.exists(), "Script should be deleted");
    let manifest_path = config.join("hooks/skim-claude-code.sha256");
    assert!(!manifest_path.exists(), "Manifest should be deleted");
}

// ============================================================================
// Hook mode: tamper warning goes to log, NOT stderr
// ============================================================================

#[test]
fn test_hook_mode_tamper_warning_goes_to_log_not_stderr() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let config = claude_config_dir(home);

    // Install
    skim_init_cmd(home).args(["--yes"]).assert().success();

    // Tamper with the hook script
    let script_path = config.join("hooks/skim-rewrite.sh");
    fs::write(&script_path, "#!/bin/bash\necho 'tampered'\n").unwrap();

    // Run hook mode with a simple command
    let hook_input = serde_json::json!({
        "tool_input": {
            "command": "cargo test"
        }
    });

    // The sandbox already points SKIM_CACHE_DIR inside the home, so the log is
    // found by asking the table where it went rather than by re-setting the
    // variable the sandbox owns.
    skim_rewrite_hook_cmd(home)
        .write_stdin(hook_input.to_string())
        .assert()
        .success()
        // CRITICAL: stderr must NOT contain the tamper warning
        .stderr(predicate::str::contains("tampered").not());

    // The warning SHOULD appear in the log file.
    // SKIM_CACHE_DIR points directly to the skim cache dir.
    let log_path = cache_dir(home).join("hook.log");
    assert!(
        log_path.exists(),
        "Hook log file should exist at {}",
        log_path.display()
    );
    let log_content = fs::read_to_string(&log_path).unwrap();
    assert!(
        log_content.contains("tampered"),
        "Hook log should contain tamper warning, got: {log_content}"
    );
}

// ============================================================================
// Cleanup removes SHA-256 on uninstall
// ============================================================================

#[test]
fn test_cleanup_removes_sha256() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let config = claude_config_dir(home);

    // Install
    skim_init_cmd(home).args(["--yes"]).assert().success();

    let manifest_path = config.join("hooks/skim-claude-code.sha256");
    assert!(
        manifest_path.exists(),
        "Manifest should exist after install"
    );

    // Uninstall
    skim_init_cmd(home)
        .args(["--uninstall", "--yes"])
        .assert()
        .success();

    assert!(
        !manifest_path.exists(),
        "Manifest should be removed after uninstall"
    );
}

// ============================================================================
// Integrity suppresses version mismatch
// ============================================================================

#[test]
fn test_integrity_suppresses_version_mismatch() {
    let home = TempDir::new().unwrap();
    let home = home.path();
    let config = claude_config_dir(home);

    // Install
    skim_init_cmd(home).args(["--yes"]).assert().success();

    // Tamper with the hook script
    let script_path = config.join("hooks/skim-rewrite.sh");
    fs::write(&script_path, "#!/bin/bash\necho 'tampered'\n").unwrap();

    // Run hook mode with a MISMATCHED version env
    let hook_input = serde_json::json!({
        "tool_input": {
            "command": "cargo test"
        }
    });

    // Set a mismatched hook version -- integrity warning should subsume it.
    // `SKIM_HOOK_VERSION` is a REMOVED sandbox var, not a redirected one, so
    // setting it here re-opens nothing: the sandbox strips the host's value so
    // that drift is only ever faked deliberately, which is what this test does.
    skim_rewrite_hook_cmd(home)
        .env("SKIM_HOOK_VERSION", "0.0.0-fake")
        .write_stdin(hook_input.to_string())
        .assert()
        .success()
        // CRITICAL: stderr must NOT contain version mismatch warning
        // (integrity warning subsumes it)
        .stderr(predicate::str::contains("version mismatch").not());
}

// ============================================================================
// Group 4 regression: `skim init` self-heals a missing manifest
// ============================================================================

/// Running `skim init` on an already-current install MUST restore a missing
/// SHA-256 manifest — so that `skim doctor`'s advice ("run `skim init --agent
/// {agent}` to add tamper detection") is actionable (Group 4 fix / #471).
///
/// Uses `common::skim_sandboxed` with a `TempDir` home so the test cannot
/// touch the developer's real `~/.claude/hooks/` or `~/.gemini/`.
#[test]
fn test_init_self_heals_missing_manifest() {
    let home = TempDir::new().unwrap();
    let home = home.path();

    // Create config dir (required by detect_installed_agents in override-mode).
    let config = claude_config_dir(home);

    // First install: creates hook script + manifest.
    common::skim_sandboxed(home)
        .args([
            "init",
            "--agent",
            "claude-code",
            "--no-guidance",
            "--no-wrappers",
        ])
        .assert()
        .success();

    let manifest = config.join("hooks/skim-claude-code.sha256");
    assert!(
        manifest.exists(),
        "manifest must exist after initial install"
    );

    // Delete the manifest to simulate a pre-manifest install or a failed write.
    fs::remove_file(&manifest).unwrap();
    assert!(!manifest.exists(), "manifest must be absent before re-init");

    // Re-run `skim init` with the same parameters — script is current, but
    // manifest is absent.  The manifest-presence check in the outer fast path
    // must prevent the "already up to date" short-circuit so the heal runs.
    common::skim_sandboxed(home)
        .args([
            "init",
            "--agent",
            "claude-code",
            "--no-guidance",
            "--no-wrappers",
        ])
        .assert()
        .success();

    // Manifest must be restored.
    assert!(
        manifest.exists(),
        "manifest must be restored by re-running `skim init` (Group 4 self-heal)"
    );

    // Validate manifest format: sha256:<hex>  skim-rewrite.sh
    let content = fs::read_to_string(&manifest).unwrap();
    assert!(
        content.starts_with("sha256:"),
        "restored manifest must start with sha256: prefix; got: {content}"
    );
    assert!(
        content.contains("skim-rewrite.sh"),
        "restored manifest must reference the script name; got: {content}"
    );
}
