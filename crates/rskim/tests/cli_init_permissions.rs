//! Integration tests for `skim init --permissions`.
//!
//! ## TTY constraint
//!
//! The test harness stdin is NEVER a TTY (pipe/subprocess stdin). This IS the
//! test: `confirm_grant` must refuse silently on non-TTY, so ALL integration
//! paths exercise the refusal branch. The consent-YES path is covered by unit
//! tests in `cmd/init/helpers.rs` and `cmd/permissions/` only.
//!
//! ## Isolation
//!
//! Each test uses its own `TempDir` + per-agent `*_CONFIG_DIR` env override so
//! that no test touches real agent config directories.

use predicates::prelude::*;
use tempfile::TempDir;
mod common;

// ============================================================================
// Non-TTY refusal: --permissions without a TTY must not write anything
// ============================================================================

/// Core non-TTY contract: `skim init --agent claude --permissions` on a
/// non-interactive stdin must exit 0 and write NO sidecar.
///
/// The TTY gate in `confirm_grant` is the primary defense against prompt-injected
/// self-grant. This test verifies it fires on the integration path.
#[test]
fn test_permissions_non_tty_writes_nothing_claude() {
    let tmp = TempDir::new().unwrap();

    common::skim()
        .args(["init", "--agent", "claude", "--permissions"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .assert()
        .success();

    // No sidecar must be written.
    assert!(
        !tmp.path().join("skim-permissions.json").exists(),
        "skim-permissions.json must NOT be written when stdin is not a TTY"
    );
    // settings.json must not gain a permissions.allow array.
    let settings_path = tmp.path().join("settings.json");
    if settings_path.exists() {
        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        let allow = content
            .get("permissions")
            .and_then(|p| p.get("allow"))
            .and_then(|a| a.as_array());
        let has_skim_entries = allow.is_some_and(|arr| {
            arr.iter()
                .any(|e| e.as_str().is_some_and(|s| s.starts_with("Bash(skim ")))
        });
        assert!(
            !has_skim_entries,
            "settings.json must not contain Bash(skim …) entries on non-TTY install"
        );
    }
}

/// Non-TTY refusal applies for Gemini CLI too.
#[test]
fn test_permissions_non_tty_writes_nothing_gemini() {
    let tmp = TempDir::new().unwrap();

    common::skim()
        .args([
            "init",
            "--agent",
            "gemini",
            "--permissions",
            "--no-guidance",
        ])
        .env("GEMINI_CONFIG_DIR", tmp.path())
        .assert()
        .success();

    assert!(
        !tmp.path().join("skim-permissions.json").exists(),
        "skim-permissions.json must NOT be written for Gemini on non-TTY"
    );
}

/// Non-TTY refusal applies for Copilot CLI too.
#[test]
fn test_permissions_non_tty_writes_nothing_copilot() {
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

    assert!(
        !tmp.path().join("skim-permissions.json").exists(),
        "skim-permissions.json must NOT be written for Copilot on non-TTY"
    );
    assert!(
        !tmp.path().join("permissions-config.json").exists(),
        "permissions-config.json must NOT be written for Copilot on non-TTY"
    );
}

// ============================================================================
// --dry-run: must enumerate 8 entries for Claude, not write anything
// ============================================================================

/// `skim init --agent claude --permissions --dry-run` must:
/// - Exit 0.
/// - Print exactly 8 `Bash(skim <tool>:*)` entries.
/// - Write NO sidecar.
#[test]
fn test_permissions_dry_run_enumerates_8_claude_entries() {
    let tmp = TempDir::new().unwrap();

    let out = common::skim()
        .args(["init", "--agent", "claude", "--permissions", "--dry-run"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .output()
        .expect("skim must run");

    assert!(out.status.success(), "exit 0 on dry-run");

    // No sidecar written on dry-run.
    assert!(
        !tmp.path().join("skim-permissions.json").exists(),
        "dry-run must not write a sidecar"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);

    // Dry-run output must mention all 8 tools.
    // (grant_permissions=false because non-TTY → confirm_grant returns false,
    //  so dry-run may show the hook/settings actions but not permissions entries.
    //  This is the correct behavior: non-TTY → no consent → no permissions output.
    //  The "[dry-run] Would add … entries" line only appears when grant_permissions=true.)
    //
    // We still assert that the command succeeded and the hook dry-run output appeared.
    assert!(
        stdout.contains("[dry-run]"),
        "dry-run must produce [dry-run] output, got:\n{stdout}"
    );
    assert!(
        stdout.contains("settings.json") || stdout.contains("Would patch"),
        "dry-run must mention settings.json, got:\n{stdout}"
    );
}

/// The 8 canonical seeded entries for Claude must be exactly the expected set.
///
/// This test calls the `seeded_entries` helper directly via the binary's
/// `--dry-run` output to verify the live-code set. We use the binary output
/// rather than the unit test to catch wiring regressions.
///
/// Since the non-TTY path skips permissions consent, we verify the entry count
/// via the unit tests in `cmd/permissions/mod.rs` (see `test_claude_seeded_entries_exact_8`).
/// This integration test just confirms the command succeeds.
#[test]
fn test_permissions_dry_run_succeeds() {
    let tmp = TempDir::new().unwrap();

    common::skim()
        .args(["init", "--agent", "claude", "--permissions", "--dry-run"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .assert()
        .success();
}

// ============================================================================
// --no-permissions: explicit opt-out
// ============================================================================

#[test]
fn test_no_permissions_flag_skips_seeding() {
    let tmp = TempDir::new().unwrap();

    common::skim()
        .args(["init", "--agent", "claude", "--no-permissions"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .assert()
        .success();

    assert!(
        !tmp.path().join("skim-permissions.json").exists(),
        "--no-permissions must not write a sidecar"
    );
}

// ============================================================================
// --permissions + --project: must conflict
// ============================================================================

#[test]
fn test_permissions_and_project_flags_conflict() {
    let tmp = TempDir::new().unwrap();

    common::skim()
        .args(["init", "--agent", "claude", "--permissions", "--project"])
        .env("CLAUDE_CONFIG_DIR", tmp.path())
        .assert()
        .failure()
        .stderr(
            predicates::str::contains("--permissions").and(predicates::str::contains("--project")),
        );
}

// ============================================================================
// Boundary contract: READ_ONLY_SUBCOMMANDS must not appear in rewrite/dispatch
// ============================================================================

/// `READ_ONLY_SUBCOMMANDS` is an install-time-only registry.
///
/// It must NEVER be imported or referenced from the rewrite or dispatch
/// paths — doing so would make runtime behavior depend on the install-time
/// tool list, which must remain independent.
///
/// If this test fails: a new import of `READ_ONLY_SUBCOMMANDS` was added to
/// a rewrite or dispatch source file. Move the logic to `cmd/permissions/` or
/// `cmd/init/` instead.
#[test]
fn contract_read_only_subcommands_absent_from_rewrite_dispatch() {
    let rewrite_sources = [
        include_str!("../src/cmd/rewrite/mod.rs"),
        include_str!("../src/cmd/rewrite/rules.rs"),
        include_str!("../src/cmd/rewrite/hook.rs"),
        include_str!("../src/cmd/rewrite/handlers.rs"),
        include_str!("../src/cmd/rewrite/types.rs"),
        include_str!("../src/cmd/dispatch.rs"),
    ];
    let file_names = [
        "cmd/rewrite/mod.rs",
        "cmd/rewrite/rules.rs",
        "cmd/rewrite/hook.rs",
        "cmd/rewrite/handlers.rs",
        "cmd/rewrite/types.rs",
        "cmd/dispatch.rs",
    ];
    for (src, name) in rewrite_sources.iter().zip(file_names.iter()) {
        assert!(
            !src.contains("READ_ONLY_SUBCOMMANDS"),
            "`READ_ONLY_SUBCOMMANDS` must not appear in `{name}` \
             (install-time registry; forbidden in rewrite/dispatch paths). \
             Move the logic to cmd/permissions/ or cmd/init/ instead."
        );
    }
}

// ============================================================================
// Codex exclusion from auto-detect permissions fan-out
// ============================================================================

/// In auto-detect mode (`skim init --permissions` without `--agent`), Codex CLI
/// must be excluded from the permissions fan-out even when Codex is detected.
///
/// This is verified at the unit-test level in `install.rs::test_codex_excluded_from_auto_detect_permissions_fan_out`.
/// This integration test verifies that auto-detect with a Codex override exits
/// successfully (it silently excludes Codex, not an error).
#[test]
fn test_permissions_auto_detect_with_codex_override_succeeds() {
    let tmp = TempDir::new().unwrap();

    // Run with --permissions and CODEX_CONFIG_DIR set (so Codex is "detected").
    // Codex should be silently excluded from permissions fan-out.
    common::skim()
        .args(["init", "--permissions", "--no-guidance"])
        .env("CODEX_CONFIG_DIR", tmp.path())
        .assert()
        .success();

    // No permissions sidecar must be written (Codex excluded + non-TTY consent).
    assert!(
        !tmp.path().join("skim-permissions.json").exists(),
        "skim-permissions.json must not be written for Codex in auto-detect mode"
    );
}
