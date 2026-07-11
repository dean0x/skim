//! Integration tests for Copilot CLI permissions and migration with skim init.
//!
//! Verifies that:
//! - `skim init --agent copilot --permissions` respects the non-TTY refusal gate.
//! - The Copilot permissions writer uses `hook_config_dir` (via `COPILOT_CONFIG_DIR`
//!   in tests, which makes config_dir == hook_config_dir).
//! - Uninstall with a corrupt sidecar produces a loud notice but exits 0.

use tempfile::TempDir;
mod common;

// ============================================================================
// Copilot CLI permissions: non-TTY refusal
// ============================================================================

/// `skim init --agent copilot --permissions` must exit 0 on non-TTY and write
/// no permissions artifacts.
#[test]
fn test_copilot_permissions_non_tty_writes_nothing() {
    let tmp = TempDir::new().unwrap();

    common::skim()
        .args([
            "init",
            "--agent",
            "copilot",
            "--permissions",
            "--no-guidance",
        ])
        .env("COPILOT_CONFIG_DIR", tmp.path())
        .assert()
        .success();

    // The Copilot permissions writer creates permissions-config.json keyed by git root.
    // On non-TTY, consent is not obtained, so no file must be written.
    assert!(
        !tmp.path().join("skim-permissions.json").exists(),
        "skim-permissions.json must not be written for Copilot on non-TTY"
    );
    assert!(
        !tmp.path().join("permissions-config.json").exists(),
        "permissions-config.json must not be written for Copilot on non-TTY"
    );
}

// ============================================================================
// Copilot CLI: uninstall with corrupt sidecar produces loud notice, exits 0
// ============================================================================

/// After writing a corrupt sidecar, `skim init --agent copilot --uninstall --yes`
/// must print a loud notice to stderr but still exit 0 (non-fatal).
///
/// This exercises the non-fatal sidecar error path in `run_uninstall_for_agent`.
#[test]
fn test_copilot_uninstall_corrupt_sidecar_loud_notice_non_fatal() {
    let tmp = TempDir::new().unwrap();

    // First, do a full install to create real hook artifacts.
    common::skim()
        .args(["init", "--agent", "copilot", "--no-guidance"])
        .env("COPILOT_CONFIG_DIR", tmp.path())
        .assert()
        .success();

    // Now write a corrupt sidecar where permissions would live.
    let sidecar_path = tmp.path().join("skim-permissions.json");
    std::fs::write(&sidecar_path, b"{ INVALID JSON !!!").unwrap();

    // Uninstall must not fail hard; it emits a notice and continues.
    let out = common::skim()
        .args(["init", "--agent", "copilot", "--uninstall", "--yes"])
        .env("COPILOT_CONFIG_DIR", tmp.path())
        .output()
        .expect("skim must run");

    assert!(
        out.status.success(),
        "uninstall must exit 0 even with a corrupt sidecar (non-fatal path); \
         stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A loud notice must appear on stderr (exact wording may vary).
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Notice") || stderr.contains("notice") || stderr.contains("sidecar"),
        "uninstall must print a loud notice about the corrupt sidecar, got stderr:\n{stderr}"
    );
}

// ============================================================================
// Copilot CLI: --no-permissions skips seeding
// ============================================================================

#[test]
fn test_copilot_no_permissions_flag_skips_seeding() {
    let tmp = TempDir::new().unwrap();

    common::skim()
        .args([
            "init",
            "--agent",
            "copilot",
            "--no-permissions",
            "--no-guidance",
        ])
        .env("COPILOT_CONFIG_DIR", tmp.path())
        .assert()
        .success();

    assert!(
        !tmp.path().join("permissions-config.json").exists(),
        "permissions-config.json must not be written with --no-permissions"
    );
}
