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
///
/// Sandboxed: HOME is a TempDir so uninstall cannot touch real ~/.copilot,
/// ~/.skim/bin, or any other home-directory artifact (avoids PF-009 / PF-015).
#[test]
fn test_copilot_uninstall_corrupt_sidecar_loud_notice_non_fatal() {
    let home = TempDir::new().unwrap();
    // skim_sandboxed sets COPILOT_CONFIG_DIR=home/.copilot; use that as config_dir.
    let config_dir = home.path().join(".copilot");

    // Pre-create the config dir: single-agent install verifies config dir exists
    // before proceeding (guards against "agent not installed" false installs).
    std::fs::create_dir_all(&config_dir).unwrap();

    // First, do a full install to create real hook artifacts.
    common::skim_sandboxed(home.path())
        .args(["init", "--agent", "copilot", "--no-guidance"])
        .assert()
        .success();

    // Now write a corrupt sidecar where permissions would live.
    let sidecar_path = config_dir.join("skim-permissions.json");
    std::fs::write(&sidecar_path, b"{ INVALID JSON !!!").unwrap();

    // Uninstall must not fail hard; it emits a notice and continues.
    let out = common::skim_sandboxed(home.path())
        .args(["init", "--agent", "copilot", "--uninstall", "--yes"])
        .output()
        .expect("skim must run");

    assert!(
        out.status.success(),
        "uninstall must exit 0 even with a corrupt sidecar (non-fatal path); \
         stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // A loud notice must appear on stderr. The exact phrase comes from
    // `cmd/init/uninstall.rs` line ~270:
    //   eprintln!("  Notice: could not remove seeded permissions (sidecar issue): {e}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("could not remove seeded permissions"),
        "uninstall must print the corrupt-sidecar remediation notice, got stderr:\n{stderr}"
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

// ============================================================================
// Fix 3: Copilot dry-run must show register-hook line, not patch-settings line
// ============================================================================

/// `skim init --agent copilot --no-guidance --dry-run` must print a
/// "Would write: …/hooks/skim.json" line referencing the skim.json hook
/// registration file, and must NOT print "Would patch", "Would back up",
/// or "Patch settings" (those are for settings.json-based agents only).
#[test]
fn test_copilot_dry_run_shows_register_hook_not_patch_settings() {
    let tmp = TempDir::new().unwrap();

    let output = common::skim()
        .args(["init", "--agent", "copilot", "--no-guidance", "--dry-run"])
        .env("COPILOT_CONFIG_DIR", tmp.path())
        .output()
        .expect("skim init must run");

    assert!(
        output.status.success(),
        "skim init --dry-run must exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Must reference hooks/skim.json (the dedicated hook file for Copilot)
    assert!(
        stdout.contains("skim.json"),
        "dry-run output must reference skim.json hook registration; got:\n{stdout}"
    );

    // Must show register-hook wording with Copilot's event key.
    // Literal from print_dry_run_actions (uses_dedicated_hook_file() == true branch):
    //   "  [dry-run] Would write: … (register preToolUse hook)"
    // Copilot's hook_event_key() is "preToolUse" (lowercase, per copilot.rs:161).
    // A regression that drops this line while leaving skim.json elsewhere would
    // still pass the skim.json assert above — this assertion catches that gap.
    assert!(
        stdout.contains("register preToolUse hook"),
        "Copilot dry-run must show register-hook wording with Copilot's event key (preToolUse); got:\n{stdout}"
    );

    // Must NOT show "Would patch" (that is settings.json-based agent wording)
    assert!(
        !stdout.contains("Would patch"),
        "Copilot dry-run must NOT show 'Would patch'; got:\n{stdout}"
    );

    // Must NOT show "Would back up" (no settings.json backup for Copilot)
    assert!(
        !stdout.contains("Would back up"),
        "Copilot dry-run must NOT show 'Would back up'; got:\n{stdout}"
    );

    // Must NOT show "Patch settings" (summary line for settings.json agents)
    assert!(
        !stdout.contains("Patch settings"),
        "Copilot dry-run must NOT show 'Patch settings' summary line; got:\n{stdout}"
    );
}
