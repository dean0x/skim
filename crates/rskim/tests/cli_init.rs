//! Integration tests for `skim init` and `skim rewrite --hook` (#44).
//!
//! All tests use `tempfile::TempDir` + `CLAUDE_CONFIG_DIR` env override for
//! isolation. Non-interactive tests pass `--yes`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
mod common;

// ============================================================================
// Helper: build an isolated `skim init` command with CLAUDE_CONFIG_DIR override
// ============================================================================

fn skim_init_cmd(config_dir: &std::path::Path) -> Command {
    let mut cmd = common::skim();
    cmd.arg("init")
        .env("CLAUDE_CONFIG_DIR", config_dir.as_os_str());
    cmd
}

/// Returns true if the hook entry references the skim-rewrite script.
fn is_skim_hook(entry: &serde_json::Value) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hooks| {
            hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|s| s.contains("skim-rewrite"))
            })
        })
        .unwrap_or(false)
}

// ============================================================================
// Fresh install tests
// ============================================================================

#[test]
fn test_init_creates_hook_script() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    skim_init_cmd(config)
        .args(["--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created").or(predicate::str::contains("Patched")));

    let hook_script = config.join("hooks/skim-rewrite.sh");
    assert!(hook_script.exists(), "Hook script should be created");

    let content = fs::read_to_string(&hook_script).unwrap();
    assert!(
        content.starts_with("#!/usr/bin/env bash"),
        "Should have shebang"
    );
    assert!(
        content.contains("SKIM_HOOK_VERSION"),
        "Should export version"
    );
    assert!(
        content.contains("rewrite --hook"),
        "Should exec rewrite --hook"
    );
    // Pinned binary format (D5): SKIM_HOOK_BINARY exec directly (no _SKIM_BIN indirection).
    assert!(
        content.contains("export SKIM_HOOK_BINARY="),
        "Hook script must export SKIM_HOOK_BINARY (pinned binary format), got:\n{content}"
    );
    assert!(
        content.contains("export SKIM_HOOK_COMMIT="),
        "Hook script must export SKIM_HOOK_COMMIT (pinned binary format), got:\n{content}"
    );
    assert!(
        !content.contains("_SKIM_BIN="),
        "Hook script must NOT set the redundant _SKIM_BIN variable (D5 removed it), got:\n{content}"
    );
    assert!(
        content.contains("exec \"$SKIM_HOOK_BINARY\" rewrite --hook"),
        "Hook script must exec via $SKIM_HOOK_BINARY directly, got:\n{content}"
    );
    // PATH fallback must be present as a safety net.
    assert!(
        content.contains("exec skim rewrite --hook"),
        "Hook script must have bare PATH fallback, got:\n{content}"
    );

    // Check executable permissions
    let perms = fs::metadata(&hook_script).unwrap().permissions();
    assert_eq!(
        perms.mode() & 0o111,
        0o111,
        "Hook script should be executable"
    );
}

#[test]
fn test_init_creates_settings_from_scratch() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    skim_init_cmd(config).args(["--yes"]).assert().success();

    let settings_path = config.join("settings.json");
    assert!(settings_path.exists(), "settings.json should be created");

    let contents = fs::read_to_string(&settings_path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&contents).unwrap();

    // Verify hooks.PreToolUse exists with a skim entry
    let ptu = &json["hooks"]["PreToolUse"];
    assert!(ptu.is_array(), "PreToolUse should be an array");
    let arr = ptu.as_array().unwrap();
    assert!(!arr.is_empty(), "PreToolUse should have at least one entry");

    let skim_entry = arr.iter().find(|e| is_skim_hook(e));
    assert!(skim_entry.is_some(), "Should have a skim hook entry");
}

#[test]
fn test_init_preserves_existing_hooks() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();
    fs::create_dir_all(config).unwrap();

    // Pre-populate with an existing hook
    let existing = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "/usr/bin/other-hook", "timeout": 10}]
                }
            ]
        }
    });
    fs::write(
        config.join("settings.json"),
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();

    skim_init_cmd(config).args(["--yes"]).assert().success();

    let contents = fs::read_to_string(config.join("settings.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&contents).unwrap();

    let ptu = json["hooks"]["PreToolUse"].as_array().unwrap();
    assert!(
        ptu.len() >= 2,
        "Should have both existing and new hooks, got {}",
        ptu.len()
    );

    let other_exists = ptu.iter().any(|e| {
        e.get("hooks")
            .and_then(|h| h.as_array())
            .map(|hooks| {
                hooks.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(|s| s.contains("other-hook"))
                })
            })
            .unwrap_or(false)
    });
    assert!(other_exists, "Existing hook should be preserved");
}

// ============================================================================
// Pinned-binary format migration
// ============================================================================

/// When an existing hook script has the current version string but still uses
/// the old bare-command format (pre-F6: only `exec skim rewrite --hook`, no
/// `SKIM_HOOK_BINARY` export), `skim init` must rewrite it to the new pinned
/// format even though the version number has not changed.
///
/// This is the core regression guard for F6 (binary path pinning).
#[test]
fn test_init_migrates_bare_command_format_to_pinned() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    // Set up hooks directory with a hook script in the OLD bare-command format
    // (no SKIM_HOOK_BINARY export). The version string matches so that version
    // alone would NOT trigger re-generation — only the absence of
    // `export SKIM_HOOK_BINARY=` must trigger the rewrite.
    let hooks_dir = config.join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join("skim-rewrite.sh");
    let current_version = env!("CARGO_PKG_VERSION");
    let old_bare_content = format!(
        "#!/usr/bin/env bash\n\
         # skim-hook v{version}\n\
         # Generated by: skim init -- do not edit manually\n\
         export SKIM_HOOK_VERSION=\"{version}\"\n\
         exec skim rewrite --hook --agent claude-code\n",
        version = current_version,
    );
    fs::write(&hook_path, &old_bare_content).unwrap();

    // Run `skim init --yes` — should detect the old bare format (missing
    // SKIM_HOOK_BINARY) and rewrite the script even though the version matches.
    skim_init_cmd(config)
        .args(["--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated").or(predicate::str::contains("Created")));

    // The script must now export SKIM_HOOK_BINARY (F6 pinned format).
    let content = fs::read_to_string(&hook_path).unwrap();
    assert!(
        content.contains("export SKIM_HOOK_BINARY="),
        "Migrated script must export SKIM_HOOK_BINARY, got:\n{content}"
    );
    assert!(
        content.contains("exec \"$SKIM_HOOK_BINARY\" rewrite --hook"),
        "Migrated script must exec via $SKIM_HOOK_BINARY directly (D5), got:\n{content}"
    );
    assert!(
        !content.contains("_SKIM_BIN="),
        "Migrated script must not set _SKIM_BIN (D5 removed it), got:\n{content}"
    );
    // PATH fallback must still be present.
    assert!(
        content.contains("exec skim rewrite --hook"),
        "Migrated script must retain PATH fallback, got:\n{content}"
    );
}

// ============================================================================
// Idempotency
// ============================================================================

#[test]
fn test_init_idempotent_no_duplicates() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    // Run init twice
    skim_init_cmd(config).args(["--yes"]).assert().success();

    skim_init_cmd(config).args(["--yes"]).assert().success();

    let contents = fs::read_to_string(config.join("settings.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&contents).unwrap();

    let ptu = json["hooks"]["PreToolUse"].as_array().unwrap();
    // Count skim entries
    let skim_count = ptu.iter().filter(|e| is_skim_hook(e)).count();

    assert_eq!(
        skim_count, 1,
        "Should have exactly one skim entry, not duplicates"
    );
}

#[test]
fn test_init_updates_stale_hook_version() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    // Run init once
    skim_init_cmd(config).args(["--yes"]).assert().success();

    // Manually overwrite the hook script with an old version
    let hook_path = config.join("hooks/skim-rewrite.sh");
    let old_content = "#!/usr/bin/env bash\n# skim-hook v0.0.1\nexport SKIM_HOOK_VERSION=\"0.0.1\"\nexec skim rewrite --hook\n";
    fs::write(&hook_path, old_content).unwrap();

    // Run init again — should update the script
    skim_init_cmd(config)
        .args(["--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated").or(predicate::str::contains("Created")));

    // Verify new version in script
    let content = fs::read_to_string(&hook_path).unwrap();
    assert!(
        !content.contains("v0.0.1"),
        "Should have been updated from v0.0.1"
    );
}

// ============================================================================
// Settings structure
// ============================================================================

#[test]
fn test_init_hook_structure() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    skim_init_cmd(config).args(["--yes"]).assert().success();

    let contents = fs::read_to_string(config.join("settings.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&contents).unwrap();

    let ptu = json["hooks"]["PreToolUse"].as_array().unwrap();
    let skim_entry = ptu.iter().find(|e| is_skim_hook(e)).unwrap();

    // Check structure: matcher, hooks array with type, command, timeout
    assert_eq!(skim_entry["matcher"], "Bash");
    let hooks = skim_entry["hooks"].as_array().unwrap();
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0]["type"], "command");
    assert_eq!(hooks[0]["timeout"], 5);
}

#[test]
fn test_init_no_permission_decision() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    skim_init_cmd(config).args(["--yes"]).assert().success();

    let contents = fs::read_to_string(config.join("settings.json")).unwrap();
    assert!(
        !contents.contains("permissionDecision"),
        "SECURITY: must never contain permissionDecision"
    );
}

// ============================================================================
// Symlinks
// ============================================================================

#[test]
fn test_init_preserves_symlinks() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();
    let real_dir = dir.path().join("real_claude");
    fs::create_dir_all(&real_dir).unwrap();

    // Create a real settings.json in the "real" location
    fs::write(real_dir.join("settings.json"), "{}").unwrap();

    // Create config dir and symlink settings.json
    fs::create_dir_all(config).unwrap();
    std::os::unix::fs::symlink(real_dir.join("settings.json"), config.join("settings.json"))
        .unwrap();

    skim_init_cmd(config).args(["--yes"]).assert().success();

    // The symlink should still exist
    assert!(
        config.join("settings.json").is_symlink(),
        "Symlink should be preserved"
    );

    // The real file should have the hook content
    let real_contents = fs::read_to_string(real_dir.join("settings.json")).unwrap();
    assert!(
        real_contents.contains("PreToolUse"),
        "Real file should have hook content"
    );
}

// ============================================================================
// Project mode
// ============================================================================

#[test]
fn test_init_project_mode() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("my-project");
    let claude_config = dir.path().join("claude-home");
    fs::create_dir_all(&project_dir).unwrap();
    fs::create_dir_all(&claude_config).unwrap();

    common::skim()
        .arg("init")
        .args(["--project", "--yes"])
        .env("CLAUDE_CONFIG_DIR", claude_config.as_os_str())
        .current_dir(&project_dir)
        .assert()
        .success();

    // Should create .claude/ directory in project
    let claude_dir = project_dir.join(".claude");
    assert!(claude_dir.exists(), ".claude dir should be created");
    assert!(
        claude_dir.join("settings.json").exists(),
        "settings.json should exist"
    );
    assert!(
        claude_dir.join("hooks/skim-rewrite.sh").exists(),
        "Hook script should exist"
    );
}

// ============================================================================
// Non-interactive mode
// ============================================================================

#[test]
fn test_init_yes_flag() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    // --yes should complete without stdin
    skim_init_cmd(config)
        .args(["--yes"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("Done!").or(predicate::str::contains("Already up to date")),
        );
}

#[test]
fn test_init_project_yes() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("proj");
    let claude_config = dir.path().join("claude-home");
    fs::create_dir_all(&project_dir).unwrap();
    fs::create_dir_all(&claude_config).unwrap();

    common::skim()
        .arg("init")
        .args(["--project", "--yes"])
        .env("CLAUDE_CONFIG_DIR", claude_config.as_os_str())
        .current_dir(&project_dir)
        .assert()
        .success();

    assert!(project_dir.join(".claude/settings.json").exists());
}

// ============================================================================
// Non-TTY detection
// ============================================================================

#[test]
fn test_init_non_tty_works_without_yes() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    // Non-TTY install should succeed without --yes (non-interactive by default)
    skim_init_cmd(config).assert().success().stdout(
        predicate::str::contains("Done!").or(predicate::str::contains("Already up to date")),
    );
}

// ============================================================================
// Dry-run
// ============================================================================

#[test]
fn test_init_dry_run() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    skim_init_cmd(config)
        .args(["--yes", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[dry-run]"));

    // No files should have been created
    assert!(
        !config.join("settings.json").exists(),
        "Dry-run should not create files"
    );
    assert!(
        !config.join("hooks/skim-rewrite.sh").exists(),
        "Dry-run should not create hook script"
    );
}

// ============================================================================
// Uninstall
// ============================================================================

#[test]
fn test_init_uninstall() {
    // Sandboxed: HOME is a TempDir so uninstall cannot touch real ~/.gemini,
    // ~/.copilot, ~/.skim/bin, or ~/.claude/hooks/ (avoids PF-009 / PF-015).
    let home = TempDir::new().unwrap();
    let config = home.path().join(".claude"); // CLAUDE_CONFIG_DIR set by skim_sandboxed

    // Pre-create the config dir: detect_installed_agents() in override-mode
    // requires the override path to be an existing directory (p.is_dir()).
    fs::create_dir_all(&config).unwrap();

    // First install
    common::skim_sandboxed(home.path())
        .arg("init")
        .args(["--yes"])
        .assert()
        .success();

    // Then uninstall
    common::skim_sandboxed(home.path())
        .arg("init")
        .args(["--uninstall", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Removed").or(predicate::str::contains("Deleted")));

    // Hook script should be gone
    assert!(
        !config.join("hooks/skim-rewrite.sh").exists(),
        "Hook script should be deleted"
    );

    // Settings should exist but without skim entries
    let contents = fs::read_to_string(config.join("settings.json")).unwrap();
    assert!(
        !contents.contains("skim-rewrite"),
        "Hook entry should be removed"
    );
}

#[test]
fn test_init_uninstall_preserves_other_hooks() {
    // Sandboxed: HOME is a TempDir so uninstall cannot touch real home directory
    // artifacts (avoids PF-009 / PF-015).
    let home = TempDir::new().unwrap();
    let config = home.path().join(".claude"); // CLAUDE_CONFIG_DIR set by skim_sandboxed

    // Pre-create the config dir: detect_installed_agents() in override-mode
    // requires the override path to be an existing directory (p.is_dir()).
    fs::create_dir_all(&config).unwrap();

    // Install skim
    common::skim_sandboxed(home.path())
        .arg("init")
        .args(["--yes"])
        .assert()
        .success();

    // Manually add another hook
    let contents = fs::read_to_string(config.join("settings.json")).unwrap();
    let mut json: serde_json::Value = serde_json::from_str(&contents).unwrap();
    let ptu = json["hooks"]["PreToolUse"].as_array_mut().unwrap();
    ptu.push(serde_json::json!({
        "matcher": "Bash",
        "hooks": [{"type": "command", "command": "/usr/bin/other-hook", "timeout": 10}]
    }));
    fs::write(
        config.join("settings.json"),
        serde_json::to_string_pretty(&json).unwrap(),
    )
    .unwrap();

    // Uninstall skim
    common::skim_sandboxed(home.path())
        .arg("init")
        .args(["--uninstall", "--yes"])
        .assert()
        .success();

    // Other hook should remain
    let contents = fs::read_to_string(config.join("settings.json")).unwrap();
    assert!(
        contents.contains("other-hook"),
        "Other hooks should be preserved"
    );
}

#[test]
fn test_init_uninstall_when_not_installed() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    skim_init_cmd(config)
        .args(["--uninstall", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Nothing to uninstall"));
}

// ============================================================================
// Hermeticity regression guard (PF-009 / PF-015)
// ============================================================================

/// Regression guard: `skim init` artifacts (hooks, wrappers, guidance) must land
/// inside the TempDir home provided to `skim_sandboxed`, not in the developer's
/// real home directory.
///
/// Covers the three env-var override surfaces introduced in issue #472:
/// - `CLAUDE_CONFIG_DIR` → hook script + settings.json
/// - `SKIM_WRAPPERS_DIR` → wrapper symlinks (none expected without --wrappers)
/// - `GEMINI_CONFIG_DIR` / `COPILOT_CONFIG_DIR` → guidance files (if written)
#[test]
fn test_init_sandbox_artifacts_stay_inside_tempdir() {
    let home = TempDir::new().unwrap();

    // Pre-create the Claude config dir: detect_installed_agents() in override-mode
    // requires the override path to be an existing directory (p.is_dir()).
    let claude_config = home.path().join(".claude");
    fs::create_dir_all(&claude_config).unwrap();

    // Global install via the sandboxed helper (sets HOME + all per-agent config dirs).
    common::skim_sandboxed(home.path())
        .arg("init")
        .args(["--yes"])
        .assert()
        .success();

    // Claude Code hook artifacts must be inside home.path()/.claude/, not real ~/.claude/.
    assert!(
        claude_config.join("hooks/skim-rewrite.sh").exists(),
        "Hook script must land inside sandboxed Claude config (PF-009): \
         real ~/.claude/hooks/ must not be touched"
    );
    assert!(
        claude_config.join("settings.json").exists(),
        "settings.json must land inside sandboxed Claude config"
    );

    // Wrapper dir — only created by `--wrappers`; this is a bare install.
    // If anything was written to the wrapper dir, it must be inside the sandbox.
    let sandbox_wrappers = home.path().join(".skim").join("bin");
    if sandbox_wrappers.exists() {
        for entry in fs::read_dir(&sandbox_wrappers).unwrap() {
            let path = entry.unwrap().path();
            assert!(
                path.starts_with(home.path()),
                "Wrapper symlink must be inside TempDir, found outside: {:?}",
                path
            );
        }
    }

    // Uninstall via the same sandbox — cleanup must also stay inside TempDir.
    common::skim_sandboxed(home.path())
        .arg("init")
        .args(["--uninstall", "--yes"])
        .assert()
        .success();

    // Hook script must be gone from the sandbox (not from real ~/.claude/).
    assert!(
        !claude_config.join("hooks/skim-rewrite.sh").exists(),
        "Hook script must be removed from sandboxed Claude config after uninstall"
    );
}

// ============================================================================
// Backup
// ============================================================================

#[test]
fn test_init_creates_backup() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();
    fs::create_dir_all(config).unwrap();

    // Create an existing settings.json
    fs::write(config.join("settings.json"), "{}\n").unwrap();

    skim_init_cmd(config).args(["--yes"]).assert().success();

    assert!(
        config.join("settings.json.bak").exists(),
        "Backup should be created"
    );
}

// ============================================================================
// Edge cases
// ============================================================================

#[test]
fn test_init_empty_settings_file() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();
    fs::create_dir_all(config).unwrap();

    // Create a 0-byte settings.json
    fs::write(config.join("settings.json"), "").unwrap();

    skim_init_cmd(config).args(["--yes"]).assert().success();

    let contents = fs::read_to_string(config.join("settings.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&contents).unwrap();
    assert!(
        json["hooks"]["PreToolUse"].is_array(),
        "Should create valid structure from empty file"
    );
}

#[test]
fn test_init_malformed_json() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();
    fs::create_dir_all(config).unwrap();

    // Create a malformed settings.json
    fs::write(config.join("settings.json"), "{not valid json}").unwrap();

    skim_init_cmd(config)
        .args(["--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Failed to parse"));
}

// ============================================================================
// Hook mode tests (skim rewrite --hook)
// ============================================================================

fn hook_payload(command: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {
            "command": command
        }
    })
    .to_string()
}

#[test]
fn test_hook_cargo_test_match() {
    let output = common::skim()
        .args(["rewrite", "--hook"])
        .env_remove("SKIM_PASSTHROUGH")
        .write_stdin(hook_payload("cargo test"))
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(json["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert!(
        json["hookSpecificOutput"]["updatedInput"]["command"]
            .as_str()
            .unwrap()
            .contains("skim cargo test")
    );
}

#[test]
fn test_hook_no_match_empty_output() {
    let output = common::skim()
        .args(["rewrite", "--hook"])
        .write_stdin(hook_payload("echo hello"))
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "No match should produce empty stdout"
    );
}

#[test]
fn test_hook_already_rewritten_passthrough() {
    let output = common::skim()
        .args(["rewrite", "--hook"])
        .write_stdin(hook_payload("skim cargo test"))
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "Already-rewritten command should pass through"
    );
}

#[test]
fn test_hook_no_permission_decision() {
    let output = common::skim()
        .args(["rewrite", "--hook"])
        .write_stdin(hook_payload("cargo test"))
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("permissionDecision"),
        "SECURITY: hook must never set permissionDecision"
    );
}

#[test]
fn test_hook_malformed_json_exits_zero() {
    let output = common::skim()
        .args(["rewrite", "--hook"])
        .write_stdin("not json at all")
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "Malformed JSON should exit 0 with empty stdout"
    );
}

#[test]
fn test_hook_missing_command_field() {
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": {
            "description": "no command field here"
        }
    })
    .to_string();

    let output = common::skim()
        .args(["rewrite", "--hook"])
        .write_stdin(payload)
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "Missing command field should exit 0 with empty stdout"
    );
}

// ============================================================================
// Hook mode — compound commands (#45)
// ============================================================================

#[test]
fn test_hook_compound_command_rewrite() {
    // Send a compound command (&&) through hook mode — first segment should be rewritten
    let output = common::skim()
        .args(["rewrite", "--hook"])
        .env_remove("SKIM_PASSTHROUGH")
        .write_stdin(hook_payload("cargo test && cargo clippy"))
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(json["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    let rewritten = json["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap();
    assert!(
        rewritten.contains("skim cargo test"),
        "First segment should be rewritten, got: {rewritten}"
    );
    assert!(
        rewritten.contains("&&"),
        "Compound operator should be preserved, got: {rewritten}"
    );
}

#[test]
fn test_hook_pipe_command_passthrough() {
    // Pipe command where neither segment matches a rewrite rule — empty output
    let output = common::skim()
        .args(["rewrite", "--hook"])
        .write_stdin(hook_payload("echo hello | grep world"))
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "Non-matching pipe command should produce empty stdout, got: {stdout}"
    );
}

// ============================================================================
// Hook mode — version mismatch warning (#44 A2)
// ============================================================================

#[test]
fn test_hook_version_mismatch_warning() {
    // Use a temp dir for cache to avoid stamp file pollution across tests.
    let cache_dir = TempDir::new().unwrap();

    // Set SKIM_HOOK_VERSION to a value that differs from the compiled version.
    // The warning now goes to hook.log (NEVER stderr -- GRANITE #361 Bug 3).
    let output = common::skim()
        .args(["rewrite", "--hook"])
        .env_remove("SKIM_PASSTHROUGH")
        .env("SKIM_HOOK_VERSION", "0.0.1")
        .env("SKIM_CACHE_DIR", cache_dir.path().as_os_str())
        .write_stdin(hook_payload("cargo test"))
        .assert()
        .success();

    // CRITICAL: stderr MUST be empty in hook mode (zero-stderr invariant)
    let stderr = String::from_utf8(output.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.is_empty(),
        "Hook mode must have zero stderr even on version mismatch, got: {stderr}"
    );

    // The rewrite should still succeed
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(
        json["hookSpecificOutput"]["updatedInput"]["command"]
            .as_str()
            .unwrap()
            .contains("skim cargo test"),
        "Rewrite should succeed despite version mismatch"
    );

    // The hook response carries no drift information: systemMessage and
    // additionalContext are absent. Drift is recorded to hook.log only;
    // skim doctor is the on-demand diagnostic.
    assert!(
        json.get("systemMessage").is_none(),
        "hook response must not contain systemMessage: {json}"
    );
    assert!(
        json["hookSpecificOutput"]
            .get("additionalContext")
            .is_none(),
        "hook response must not contain additionalContext: {json}"
    );

    // Verify warning went to hook.log file instead
    let hook_log = cache_dir.path().join("hook.log");
    assert!(
        hook_log.exists(),
        "Version mismatch warning should be written to hook.log"
    );
    let log_content = fs::read_to_string(&hook_log).unwrap();
    assert!(
        log_content.contains("version mismatch"),
        "hook.log should contain version mismatch warning, got: {log_content}"
    );
}

/// F6: When `SKIM_HOOK_BINARY` is set to a path that differs from the running
/// binary, a daily warning must appear in hook.log — never in stderr.
#[test]
fn test_hook_binary_mismatch_warning() {
    let cache_dir = TempDir::new().unwrap();

    // Set SKIM_HOOK_BINARY to a path that does not match the running binary.
    // Use a plausible but different path: /tmp/skim-other.  The versions match
    // so only the binary path mismatch check fires (check_hook_binary_mismatch).
    let current_version = env!("CARGO_PKG_VERSION");
    let output = common::skim()
        .args(["rewrite", "--hook"])
        .env_remove("SKIM_PASSTHROUGH")
        .env("SKIM_HOOK_VERSION", current_version)
        .env("SKIM_HOOK_BINARY", "/tmp/skim-other-binary")
        .env("SKIM_CACHE_DIR", cache_dir.path().as_os_str())
        .write_stdin(hook_payload("cargo test"))
        .assert()
        .success();

    // CRITICAL: zero-stderr invariant must hold.
    let stderr = String::from_utf8(output.get_output().stderr.clone()).unwrap();
    assert!(
        stderr.is_empty(),
        "Hook mode must have zero stderr even on binary mismatch, got: {stderr}"
    );

    // The rewrite must still succeed.
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(
        json["hookSpecificOutput"]["updatedInput"]["command"]
            .as_str()
            .unwrap()
            .contains("skim cargo test"),
        "Rewrite must succeed despite binary mismatch"
    );

    // The hook response carries no drift information: systemMessage and
    // additionalContext are absent. Drift is recorded to hook.log only;
    // skim doctor is the on-demand diagnostic.
    assert!(
        json.get("systemMessage").is_none(),
        "hook response must not contain systemMessage: {json}"
    );
    assert!(
        json["hookSpecificOutput"]
            .get("additionalContext")
            .is_none(),
        "hook response must not contain additionalContext: {json}"
    );

    // Warning must appear in hook.log.
    let hook_log = cache_dir.path().join("hook.log");
    assert!(
        hook_log.exists(),
        "Binary mismatch warning should be written to hook.log"
    );
    let log_content = fs::read_to_string(&hook_log).unwrap();
    assert!(
        log_content.contains("binary path mismatch") || log_content.contains("binary"),
        "hook.log should contain binary mismatch warning, got: {log_content}"
    );
}

// ============================================================================
// Help text
// ============================================================================

#[test]
fn test_init_help() {
    common::skim()
        .args(["init", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim init"))
        .stdout(predicate::str::contains("--global"))
        .stdout(predicate::str::contains("--project"))
        .stdout(predicate::str::contains("--yes"))
        .stdout(predicate::str::contains("--dry-run"))
        .stdout(predicate::str::contains("--uninstall"));
}

#[test]
fn test_rewrite_hook_help() {
    common::skim()
        .args(["rewrite", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--hook"));
}

// ============================================================================
// Guidance injection
// ============================================================================

#[test]
fn test_init_creates_guidance() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    // Create a CLAUDE.md at the "global" location (config_dir/../CLAUDE.md won't work,
    // so we test via project mode which creates CLAUDE.md in CWD)
    let project_dir = TempDir::new().unwrap();

    common::skim()
        .arg("init")
        .args(["--project", "--yes"])
        .env("CLAUDE_CONFIG_DIR", config.as_os_str())
        .current_dir(project_dir.path())
        .assert()
        .success();

    // Check that CLAUDE.md was created with guidance
    let claude_md = project_dir.path().join("CLAUDE.md");
    assert!(
        claude_md.exists(),
        "CLAUDE.md should be created with guidance"
    );
    let content = fs::read_to_string(&claude_md).unwrap();
    assert!(
        content.contains("<!-- skim-start"),
        "CLAUDE.md should contain skim guidance section"
    );
    assert!(
        content.contains("<!-- skim-end -->"),
        "CLAUDE.md should have closing marker"
    );
    assert!(
        content.contains("skim") || content.contains("rskim"),
        "Guidance should reference skim or rskim"
    );
}

#[test]
fn test_init_no_guidance_flag() {
    let dir = TempDir::new().unwrap();
    let project_dir = TempDir::new().unwrap();

    common::skim()
        .arg("init")
        .args(["--project", "--yes", "--no-guidance"])
        .env("CLAUDE_CONFIG_DIR", dir.path().as_os_str())
        .current_dir(project_dir.path())
        .assert()
        .success();

    // CLAUDE.md should not exist (no guidance injected, file not created)
    let claude_md = project_dir.path().join("CLAUDE.md");
    assert!(
        !claude_md.exists(),
        "CLAUDE.md should not be created with --no-guidance"
    );
}

#[test]
fn test_init_uninstall_removes_guidance() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();
    let project_dir = TempDir::new().unwrap();

    // First install with guidance
    common::skim()
        .arg("init")
        .args(["--project", "--yes"])
        .env("CLAUDE_CONFIG_DIR", config.as_os_str())
        .current_dir(project_dir.path())
        .assert()
        .success();

    // Verify install created guidance
    let claude_md = project_dir.path().join("CLAUDE.md");
    assert!(claude_md.exists(), "CLAUDE.md should exist after install");

    // Then uninstall
    common::skim()
        .arg("init")
        .args(["--project", "--uninstall", "--yes"])
        .env("CLAUDE_CONFIG_DIR", config.as_os_str())
        .current_dir(project_dir.path())
        .assert()
        .success();

    // CLAUDE.md should not contain skim guidance (or be deleted if it was the only content)
    let claude_md = project_dir.path().join("CLAUDE.md");
    if claude_md.exists() {
        let content = fs::read_to_string(&claude_md).unwrap();
        assert!(
            !content.contains("skim-start"),
            "Guidance section should be removed after uninstall"
        );
    }
    // If file doesn't exist, that's also correct (was only skim content)
}

#[test]
fn test_init_guidance_idempotent() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();
    let project_dir = TempDir::new().unwrap();

    // Install twice
    for _ in 0..2 {
        common::skim()
            .arg("init")
            .args(["--project", "--yes"])
            .env("CLAUDE_CONFIG_DIR", config.as_os_str())
            .current_dir(project_dir.path())
            .assert()
            .success();
    }

    // CLAUDE.md should have exactly one skim section
    let claude_md = project_dir.path().join("CLAUDE.md");
    assert!(claude_md.exists(), "CLAUDE.md should exist after init");
    let content = fs::read_to_string(&claude_md).unwrap();
    let start_count = content.matches("<!-- skim-start").count();
    assert_eq!(
        start_count, 1,
        "Should have exactly one skim section, found {}",
        start_count
    );
}

#[test]
fn test_init_dry_run_shows_guidance() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();
    let project_dir = TempDir::new().unwrap();

    common::skim()
        .arg("init")
        .args(["--project", "--yes", "--dry-run"])
        .env("CLAUDE_CONFIG_DIR", config.as_os_str())
        .current_dir(project_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("guidance"));
}

// ============================================================================
// Cursor .mdc format
// ============================================================================

#[test]
fn test_init_cursor_creates_mdc() {
    let config_dir = TempDir::new().unwrap();
    let project_dir = TempDir::new().unwrap();

    common::skim()
        .arg("init")
        .args(["--project", "--yes", "--agent", "cursor"])
        .env("CLAUDE_CONFIG_DIR", config_dir.path().as_os_str())
        .current_dir(project_dir.path())
        .assert()
        .success();

    // Should create .cursor/rules/skim.mdc with frontmatter
    let mdc = project_dir.path().join(".cursor/rules/skim.mdc");
    assert!(mdc.exists(), ".cursor/rules/skim.mdc should be created");
    let content = fs::read_to_string(&mdc).unwrap();
    assert!(content.starts_with("---\n"), "Should have YAML frontmatter");
    assert!(
        content.contains("alwaysApply: true"),
        "Should have alwaysApply"
    );
    assert!(
        content.contains("<!-- skim-start"),
        "Should have skim start marker"
    );
    assert!(
        content.contains("<!-- skim-end -->"),
        "Should have skim end marker"
    );
}

#[test]
fn test_init_cursor_uninstall_deletes_mdc() {
    let config_dir = TempDir::new().unwrap();
    let project_dir = TempDir::new().unwrap();

    // Install
    common::skim()
        .arg("init")
        .args(["--project", "--yes", "--agent", "cursor"])
        .env("CLAUDE_CONFIG_DIR", config_dir.path().as_os_str())
        .current_dir(project_dir.path())
        .assert()
        .success();

    let mdc = project_dir.path().join(".cursor/rules/skim.mdc");
    assert!(mdc.exists(), "skim.mdc should exist after install");

    // Uninstall
    common::skim()
        .arg("init")
        .args(["--project", "--uninstall", "--yes", "--agent", "cursor"])
        .env("CLAUDE_CONFIG_DIR", config_dir.path().as_os_str())
        .current_dir(project_dir.path())
        .assert()
        .success();

    assert!(!mdc.exists(), "skim.mdc should be deleted on uninstall");
}

#[test]
fn test_init_cursor_cleans_legacy_cursorrules() {
    let config_dir = TempDir::new().unwrap();
    let project_dir = TempDir::new().unwrap();

    // Pre-populate a .cursorrules with skim markers (legacy format)
    let cursorrules = project_dir.path().join(".cursorrules");
    fs::write(
        &cursorrules,
        "# User rules\n\n<!-- skim-start v1.0.0 -->\nold guidance\n<!-- skim-end -->\n\n# More user rules\n",
    )
    .unwrap();

    // Install Cursor (should create .mdc AND clean legacy .cursorrules)
    common::skim()
        .arg("init")
        .args(["--project", "--yes", "--agent", "cursor"])
        .env("CLAUDE_CONFIG_DIR", config_dir.path().as_os_str())
        .current_dir(project_dir.path())
        .assert()
        .success();

    // New .mdc should exist
    let mdc = project_dir.path().join(".cursor/rules/skim.mdc");
    assert!(mdc.exists(), ".cursor/rules/skim.mdc should be created");

    // Legacy .cursorrules should still exist (user may have created it)
    assert!(
        cursorrules.exists(),
        ".cursorrules should NOT be deleted (user owns it)"
    );

    // But skim markers should be removed from .cursorrules
    let content = fs::read_to_string(&cursorrules).unwrap();
    assert!(
        !content.contains("skim-start"),
        "Skim markers should be removed from .cursorrules, got: {content}"
    );
    assert!(
        content.contains("User rules"),
        "User content should be preserved in .cursorrules"
    );
}

// ============================================================================
// Phase 6: Multi-agent awareness in skim init
// ============================================================================

#[test]
fn test_init_help_mentions_agent_flag() {
    // init --help should document the --agent flag for multi-agent support
    common::skim()
        .args(["init", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--agent"));
}

#[test]
fn test_rewrite_help_mentions_agent_flag() {
    // rewrite --help should mention the --agent flag
    common::skim()
        .args(["rewrite", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--agent"));
}

// ============================================================================
// Guidance upgrade bypass tests (issue 11)
// ============================================================================

#[test]
fn test_init_guidance_upgrade_updates_stale_version() {
    // Verifies that is_guidance_current returns false when the guidance section
    // contains a stale version marker, causing a re-run of init --yes to
    // update guidance rather than print "Already up to date".
    let dir = TempDir::new().unwrap();
    let config = dir.path();
    let project_dir = TempDir::new().unwrap();

    // Step 1: fresh install — creates guidance at the current version
    common::skim()
        .arg("init")
        .args(["--project", "--yes"])
        .env("CLAUDE_CONFIG_DIR", config.as_os_str())
        .current_dir(project_dir.path())
        .assert()
        .success();

    let claude_md = project_dir.path().join("CLAUDE.md");
    assert!(
        claude_md.exists(),
        "CLAUDE.md should exist after initial install"
    );

    // Step 2: manually overwrite the guidance section with an old version marker
    let content = fs::read_to_string(&claude_md).unwrap();
    assert!(
        content.contains("<!-- skim-start"),
        "Initial install should have created a skim-start marker"
    );
    // Replace the versioned marker with an obviously stale one
    let stale_content = {
        let start = content
            .find("<!-- skim-start")
            .expect("start marker must exist");
        let marker_end = content[start..]
            .find(" -->")
            .expect("marker closing must exist");
        let mut s = content.clone();
        s.replace_range(start..start + marker_end + 4, "<!-- skim-start v0.0.1 -->");
        s
    };
    fs::write(&claude_md, &stale_content).unwrap();
    assert!(
        stale_content.contains("<!-- skim-start v0.0.1 -->"),
        "Stale marker should be present after manual overwrite"
    );

    // Step 3: re-run init --yes — should NOT say "Already up to date"
    let output = common::skim()
        .arg("init")
        .args(["--project", "--yes"])
        .env("CLAUDE_CONFIG_DIR", config.as_os_str())
        .current_dir(project_dir.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8_lossy(&output);

    assert!(
        !stdout.contains("Already up to date"),
        "Should not say 'Already up to date' when guidance version is stale; got:\n{stdout}"
    );

    // Step 4: verify the guidance was updated to the current version
    let updated = fs::read_to_string(&claude_md).unwrap();
    assert!(
        !updated.contains("v0.0.1"),
        "Stale version marker should have been replaced"
    );
    assert!(
        updated.contains("<!-- skim-start v"),
        "Updated file should have a versioned skim-start marker"
    );
    // The new marker should not be the old stale version
    let current_version = env!("CARGO_PKG_VERSION");
    assert!(
        updated.contains(&format!("<!-- skim-start v{current_version} -->")),
        "Updated marker should reference the current binary version ({current_version})"
    );
}

#[test]
fn test_init_no_marketplace_in_settings() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    skim_init_cmd(config).args(["--yes"]).assert().success();

    let settings = fs::read_to_string(config.join("settings.json")).unwrap();
    assert!(
        !settings.contains("marketplace"),
        "SECURITY: settings must never contain marketplace field"
    );
}

// ============================================================================
// Multi-agent auto-detect install and uninstall loop (issues 3 & 4)
// ============================================================================

/// Create an isolated temp directory for an agent's config path.
///
/// Returns `(TempDir, PathBuf)`. The caller must hold the `TempDir` alive for
/// the duration of the test; dropping it deletes the directory prematurely.
fn create_agent_config_dir() -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    (dir, path)
}

#[test]
fn test_init_multi_agent_auto_detect_installs_claude_and_gemini() {
    // Create isolated config dirs for Claude Code and Gemini CLI.
    // Setting CLAUDE_CONFIG_DIR and GEMINI_CONFIG_DIR causes detect_installed_agents
    // to scope detection to only those two agents (override mode).
    let (claude_dir, claude_path) = create_agent_config_dir();
    let (gemini_dir, gemini_path) = create_agent_config_dir();
    // Project dir: `skim init --project` installs to CWD-relative dirs, so both
    // agents write to <project>/.claude/ and <project>/.gemini/ respectively —
    // no home-directory writes occur during the test.
    let project_dir = TempDir::new().unwrap();

    common::skim()
        .args(["init", "--project", "--yes", "--no-guidance"])
        .env("CLAUDE_CONFIG_DIR", &claude_path)
        .env("GEMINI_CONFIG_DIR", &gemini_path)
        .current_dir(project_dir.path())
        .assert()
        .success();

    // Claude Code: settings.json with hook entry
    let claude_settings = project_dir.path().join(".claude/settings.json");
    assert!(
        claude_settings.exists(),
        "Claude Code settings.json should be created by auto-detect install"
    );
    let claude_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&claude_settings).unwrap()).unwrap();
    let ptu = claude_json["hooks"]["PreToolUse"].as_array().unwrap();
    assert!(
        ptu.iter().any(is_skim_hook),
        "Claude Code settings.json should contain a skim PreToolUse hook"
    );

    // Gemini CLI: settings.json with hook entry under .gemini/ using BeforeTool event key
    let gemini_settings = project_dir.path().join(".gemini/settings.json");
    assert!(
        gemini_settings.exists(),
        "Gemini CLI settings.json should be created by auto-detect install"
    );
    let gemini_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&gemini_settings).unwrap()).unwrap();
    // Gemini CLI uses BeforeTool (not PreToolUse) as its hook event key
    let gbefore = gemini_json["hooks"]["BeforeTool"].as_array().unwrap();
    assert!(
        gbefore.iter().any(is_skim_hook),
        "Gemini CLI settings.json should contain a skim BeforeTool hook"
    );

    // Hook scripts must be created for both agents
    assert!(
        project_dir
            .path()
            .join(".claude/hooks/skim-rewrite.sh")
            .exists(),
        "Claude Code hook script should be created"
    );
    assert!(
        project_dir
            .path()
            .join(".gemini/hooks/skim-rewrite.sh")
            .exists(),
        "Gemini CLI hook script should be created"
    );

    // Keep TempDirs alive until end of test.
    drop(claude_dir);
    drop(gemini_dir);
}

#[test]
fn test_init_multi_agent_auto_detect_uninstalls_claude_and_gemini() {
    // Create isolated config dirs for Claude Code and Gemini CLI.
    let (claude_dir, claude_path) = create_agent_config_dir();
    let (gemini_dir, gemini_path) = create_agent_config_dir();
    let project_dir = TempDir::new().unwrap();

    // Step 1: install both agents
    common::skim()
        .args(["init", "--project", "--yes", "--no-guidance"])
        .env("CLAUDE_CONFIG_DIR", &claude_path)
        .env("GEMINI_CONFIG_DIR", &gemini_path)
        .current_dir(project_dir.path())
        .assert()
        .success();

    // Verify hook scripts exist after install
    let claude_hook = project_dir.path().join(".claude/hooks/skim-rewrite.sh");
    let gemini_hook = project_dir.path().join(".gemini/hooks/skim-rewrite.sh");
    assert!(
        claude_hook.exists(),
        "Claude Code hook should exist after install"
    );
    assert!(
        gemini_hook.exists(),
        "Gemini CLI hook should exist after install"
    );

    // Step 2: uninstall both agents without specifying --agent
    common::skim()
        .args(["init", "--project", "--uninstall", "--yes", "--force"])
        .env("CLAUDE_CONFIG_DIR", &claude_path)
        .env("GEMINI_CONFIG_DIR", &gemini_path)
        .current_dir(project_dir.path())
        .assert()
        .success();

    // Hook scripts should be gone
    assert!(
        !claude_hook.exists(),
        "Claude Code hook script should be deleted after uninstall"
    );
    assert!(
        !gemini_hook.exists(),
        "Gemini CLI hook script should be deleted after uninstall"
    );

    // Settings files should have skim hook entries removed.
    // Both files must still exist (uninstall patches them, not deletes them).
    let claude_settings = project_dir.path().join(".claude/settings.json");
    assert!(
        claude_settings.exists(),
        "Claude Code settings.json must still exist after uninstall (hook entries are removed, file is kept)"
    );
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&claude_settings).unwrap()).unwrap();
    let hooks = json.get("hooks");
    assert!(
        hooks.is_none() || hooks.and_then(|h| h.get("PreToolUse")).is_none(),
        "Claude Code hooks.PreToolUse should be removed after uninstall"
    );

    let gemini_settings = project_dir.path().join(".gemini/settings.json");
    assert!(
        gemini_settings.exists(),
        "Gemini CLI settings.json must still exist after uninstall (hook entries are removed, file is kept)"
    );
    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&gemini_settings).unwrap()).unwrap();
    let hooks = json.get("hooks");
    // Gemini CLI uses BeforeTool as its hook event key
    assert!(
        hooks.is_none() || hooks.and_then(|h| h.get("BeforeTool")).is_none(),
        "Gemini CLI hooks.BeforeTool should be removed after uninstall"
    );

    // Keep TempDirs alive until end of test.
    drop(claude_dir);
    drop(gemini_dir);
}

// ============================================================================
// Fix 3: Gemini dry-run uses BeforeTool hook key (not hardcoded PreToolUse)
// ============================================================================

/// `skim init --agent gemini --no-guidance --dry-run` must show "BeforeTool"
/// in the patch-settings description line, not the hardcoded "PreToolUse".
#[test]
fn test_gemini_dry_run_shows_before_tool_hook_key() {
    let (gemini_dir, gemini_path) = create_agent_config_dir();

    let output = common::skim()
        .args(["init", "--agent", "gemini", "--no-guidance", "--dry-run"])
        .env("GEMINI_CONFIG_DIR", &gemini_path)
        .output()
        .expect("skim init must run");

    assert!(
        output.status.success(),
        "skim init --agent gemini --dry-run must exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Gemini uses BeforeTool — must appear in the hook-key description
    assert!(
        stdout.contains("BeforeTool"),
        "Gemini dry-run output must mention 'BeforeTool'; got:\n{stdout}"
    );

    // The patch-description line must use BeforeTool, not PreToolUse.
    // Scope assertion to the Would-patch / Patch-settings line to avoid false
    // positives from other output sections that could mention PreToolUse.
    let patch_line = stdout
        .lines()
        .find(|l| l.contains("Would patch") || l.contains("Patch settings"));
    let line = patch_line.expect(
        "dry-run must emit a 'Would patch' or 'Patch settings' summary line; got nothing in stdout",
    );
    assert!(
        !line.contains("PreToolUse"),
        "Gemini dry-run patch line must use 'BeforeTool', not 'PreToolUse'; line: {line}"
    );

    drop(gemini_dir);
}

// ============================================================================
// B5c-CLI: commit drift triggers rewrite
// ============================================================================

/// B5c-CLI: `skim init --yes` must rewrite the hook script when the installed
/// script records a stale git commit but the version is current.
///
/// This drives the actual `skim init` binary to cover the full detection path:
/// `detect_state` reads the script, `hook_is_current()` returns false (stale
/// commit), the fast path is skipped, and `create_hook_script` rewrites the
/// script with the current commit.
#[test]
fn test_init_rewrites_hook_on_stale_commit_same_version() {
    let compiled_commit = option_env!("SKIM_GIT_COMMIT").unwrap_or("unknown");
    if compiled_commit == "unknown" {
        // Tarball / crates.io build: commit check is skipped; test is not applicable.
        return;
    }

    let dir = TempDir::new().unwrap();
    let config = dir.path();

    // Step 1: Install once to get a valid hook structure (settings.json + script).
    skim_init_cmd(config).args(["--yes"]).assert().success();

    // Step 2: Overwrite the hook script with the correct version but a STALE commit.
    let hook_path = config.join("hooks/skim-rewrite.sh");
    let current_version = env!("CARGO_PKG_VERSION");
    let stale_commit = "000000000000stale";
    assert_ne!(
        stale_commit, compiled_commit,
        "sanity: stale placeholder must differ from the actual compiled commit"
    );
    let stale_script = format!(
        "#!/usr/bin/env bash\n\
         # skim-hook v{version}\n\
         # Generated by: skim init -- do not edit manually\n\
         export SKIM_HOOK_VERSION=\"{version}\"\n\
         export SKIM_HOOK_BINARY='/usr/local/bin/skim'\n\
         export SKIM_HOOK_COMMIT={stale}\n\
         if [ -x \"$SKIM_HOOK_BINARY\" ]; then exec \"$SKIM_HOOK_BINARY\" rewrite --hook --agent claude-code; fi\n\
         exec skim rewrite --hook --agent claude-code\n",
        version = current_version,
        stale = stale_commit,
    );
    fs::write(&hook_path, &stale_script).unwrap();

    // Step 3: Re-run `skim init --yes` — must detect the commit drift and rewrite.
    skim_init_cmd(config)
        .args(["--yes"])
        .assert()
        .success()
        // Must print "Updated" or "Created" — NOT "Skipped".
        .stdout(predicate::str::contains("Updated").or(predicate::str::contains("Created")))
        .stdout(predicate::str::contains("Skipped").not());

    // Step 4: The rewritten script must now record the current (non-stale) commit.
    let updated_content = fs::read_to_string(&hook_path).unwrap();
    assert!(
        updated_content.contains(&format!("export SKIM_HOOK_COMMIT={compiled_commit}")),
        "Rewritten script must pin the current commit ({compiled_commit}), got:\n{updated_content}"
    );
    assert!(
        !updated_content.contains(stale_commit),
        "Rewritten script must NOT retain the stale commit ({stale_commit}), got:\n{updated_content}"
    );
}

/// After a commit-correct install, re-running `skim init --yes` must skip
/// (no rewrite) when version AND commit are both current.
///
/// Guards against the fix over-correcting: init must not rewrite on every invocation.
#[test]
fn test_init_skips_when_version_and_commit_are_current() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    // First install — establishes a fully-pinned script.
    skim_init_cmd(config).args(["--yes"]).assert().success();

    // Second install with the identical binary — must be idempotent.
    skim_init_cmd(config)
        .args(["--yes"])
        .assert()
        .success()
        // Must NOT rewrite the script.
        .stdout(
            predicate::str::contains("Already up to date").or(predicate::str::contains("Skipped")),
        )
        .stdout(predicate::str::contains("Updated").not());
}

// ============================================================================
// Fix 1: `init --wrappers` on a current install runs wrappers (C-3)
// ============================================================================

/// `skim init --wrappers` on a fully-current hook install must install wrappers
/// even though the fast path fires (C-3 fix: `maybe_install_wrappers` now runs
/// inside the fast path before `print_already_up_to_date`).
///
/// Previously `--wrappers` bypassed the fast path entirely; after C-3 the fast
/// path IS hit and "Already up to date" IS printed, but wrappers ARE still
/// installed because they run first inside the fast-path block.
#[test]
fn test_init_wrappers_bypasses_fast_path() {
    let home = TempDir::new().unwrap();
    let home_path = home.path();

    // Pre-create the claude config dir: detect_installed_agents() in override-mode
    // requires the override path to be an existing directory (p.is_dir()).
    fs::create_dir_all(home_path.join(".claude")).unwrap();

    // Step 1: Fresh install without wrappers — hook becomes current.
    common::skim_sandboxed(home_path)
        .arg("init")
        .args(["--yes"])
        .assert()
        .success();

    // Step 2: Re-run with --wrappers — fast path fires AND wrappers are installed.
    let out = common::skim_sandboxed(home_path)
        .arg("init")
        .args(["--yes", "--wrappers"])
        .output()
        .unwrap();

    assert!(out.status.success(), "init --wrappers must succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    // After C-3: fast path fires (wrappers run inside it), so "Already up to date"
    // IS printed at the end.
    assert!(
        stdout.contains("Already up to date"),
        "init --wrappers on a current install must print the fast-path message after installing wrappers, got:\n{stdout}"
    );
    // The wrappers line must appear (created or already-correct).
    assert!(
        stdout.contains("Wrappers:"),
        "init --wrappers on a current install must run wrapper installation, got:\n{stdout}"
    );
}

// ============================================================================
// Fix 2: `init --force` bypasses fast path (A-1)
// ============================================================================

/// `skim init --force` on a fully-current install must NOT exit via the fast
/// path — it must run the full install path even when everything is up to date.
///
/// Before A-1, `flags.force` was never read in `install.rs` so `--force` was
/// silently a no-op (empirically confirmed; see empirical-doctor-init-verification.md).
#[test]
fn test_init_force_bypasses_fast_path() {
    let home = TempDir::new().unwrap();
    let home_path = home.path();

    fs::create_dir_all(home_path.join(".claude")).unwrap();

    // Step 1: Fresh install — hook becomes current.
    common::skim_sandboxed(home_path)
        .arg("init")
        .args([
            "--yes",
            "--agent",
            "claude-code",
            "--no-guidance",
            "--no-wrappers",
        ])
        .assert()
        .success();

    // Step 2: Re-run with --force — must NOT print "Already up to date".
    let out = common::skim_sandboxed(home_path)
        .arg("init")
        .args([
            "--yes",
            "--agent",
            "claude-code",
            "--no-guidance",
            "--no-wrappers",
            "--force",
        ])
        .output()
        .unwrap();

    assert!(out.status.success(), "init --force must succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("Already up to date"),
        "init --force must NOT hit the fast path on a current install, got:\n{stdout}"
    );
}

/// `skim init --force` on an install whose hook lacks `SKIM_HOOK_BINARY` (absent
/// pin, repairable) must rewrite the hook script and add the pin.
///
/// This exercises the repair path: even though the hook's format is stale
/// (`hook_is_current() == false`), `--force` ensures we always run through the
/// full install path.  Without `--force`, the same result is reached via the
/// slow path (hook is already detected as stale), so the test mainly demonstrates
/// that `--force` does not interfere with the repair.
#[test]
fn test_init_force_repairs_unpinned_hook() {
    let home = TempDir::new().unwrap();
    let home_path = home.path();

    fs::create_dir_all(home_path.join(".claude")).unwrap();

    // Step 1: Write a pre-pin hook script directly (simulates a legacy install
    // without SKIM_HOOK_BINARY).
    let hooks_dir = home_path.join(".claude/hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
    let hook_path = hooks_dir.join("skim-rewrite.sh");
    let version = env!("CARGO_PKG_VERSION");
    let unpinned_script = format!(
        "#!/usr/bin/env bash\n\
         # skim-hook v{version}\n\
         export SKIM_HOOK_VERSION=\"{version}\"\n\
         exec skim rewrite --hook --agent claude-code\n"
    );
    fs::write(&hook_path, &unpinned_script).unwrap();
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&hook_path).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&hook_path, perms).unwrap();
    }

    // Also create a minimal settings.json so init can find the hook entry.
    let settings_path = home_path.join(".claude/settings.json");
    fs::write(
        &settings_path,
        r#"{"hooks":{"PreToolUse":[{"matcher":"","hooks":[{"type":"command","command":"skim-rewrite.sh rewrite --hook --agent claude-code"}]}]}}"#,
    )
    .unwrap();

    // Step 2: Run init with --force — must detect the missing pin and rewrite.
    common::skim_sandboxed(home_path)
        .arg("init")
        .args([
            "--yes",
            "--agent",
            "claude-code",
            "--no-guidance",
            "--no-wrappers",
            "--force",
        ])
        .assert()
        .success();

    // Step 3: Rewritten script must export SKIM_HOOK_BINARY.
    let updated = fs::read_to_string(&hook_path).unwrap();
    assert!(
        updated.contains("export SKIM_HOOK_BINARY="),
        "repaired script must export SKIM_HOOK_BINARY, got:\n{updated}"
    );
    // And must NOT use the old _SKIM_BIN local variable (D5 cleanup).
    assert!(
        !updated.contains("_SKIM_BIN="),
        "repaired script must not set _SKIM_BIN (D5 removed it), got:\n{updated}"
    );
}

// ============================================================================
// Fix 3: repeat `init --wrappers` does not clobber settings.json.bak (C-3)
// ============================================================================

/// Running `skim init --wrappers` twice on a current install must NOT clobber
/// `settings.json.bak`.  Before C-3, `--wrappers` bypassed the fast path and
/// re-ran `execute_install`, which re-invoked `patch_settings` which created a
/// new `.bak` that overwrote the user's original backup.
#[test]
fn test_init_repeat_wrappers_does_not_clobber_bak() {
    let home = TempDir::new().unwrap();
    let home_path = home.path();

    fs::create_dir_all(home_path.join(".claude")).unwrap();

    // Write a sentinel settings.json so patch_settings has something to back up.
    let settings_path = home_path.join(".claude/settings.json");
    let original_content = r#"{"custom":"user setting"}"#;
    fs::write(&settings_path, original_content).unwrap();

    let bak_path = home_path.join(".claude/settings.json.bak");

    // Step 1: First install with --wrappers — creates bak from the original file.
    common::skim_sandboxed(home_path)
        .arg("init")
        .args([
            "--yes",
            "--agent",
            "claude-code",
            "--no-guidance",
            "--wrappers",
        ])
        .assert()
        .success();

    // The bak must exist and preserve the original content.
    assert!(
        bak_path.exists(),
        "settings.json.bak must be created on first install"
    );
    let bak_after_first = fs::read_to_string(&bak_path).unwrap();
    assert!(
        bak_after_first.contains("user setting"),
        "bak must preserve original content, got:\n{bak_after_first}"
    );

    // Step 2: Second install with --wrappers on a now-current hook.
    // After C-3: fast path fires (wrappers run inside it, hook is NOT reinstalled),
    // so settings.json is NOT re-read and bak is NOT overwritten.
    common::skim_sandboxed(home_path)
        .arg("init")
        .args([
            "--yes",
            "--agent",
            "claude-code",
            "--no-guidance",
            "--wrappers",
        ])
        .assert()
        .success();

    // The bak must still contain the original content — NOT the post-install
    // settings.json (which has the skim hook entry added).
    let bak_after_second = fs::read_to_string(&bak_path).unwrap();
    assert_eq!(
        bak_after_first, bak_after_second,
        "settings.json.bak must not be clobbered by a second --wrappers run"
    );
}

// ============================================================================
// Fix: integrity-aware self-heal — tampered hooks are REPAIRED, not laundered
// ============================================================================

/// After tampering with a hook script (appending one byte) that still passes
/// `hook_is_current()` (version/commit markers unchanged), `skim init` must
/// REPAIR the script by regenerating from source — NOT launder the tampered
/// bytes into the manifest.
///
/// Before this fix, the self-heal path inside `create_hook_script` would
/// re-hash the on-disk tampered bytes and write them into the manifest
/// when `hook_is_current() == true`, so `skim doctor` subsequently reported
/// `Verified` for the wrong content.
///
/// After this fix:
/// - `skim init` prints "Repaired" and regenerates the script.
/// - The script content matches freshly generated content (not the tampered bytes).
/// - `skim doctor` reports `Verified` (for the now-correct content).
#[test]
fn test_init_repairs_tampered_hook_not_launders() {
    let home = TempDir::new().unwrap();
    let home_path = home.path();

    std::fs::create_dir_all(home_path.join(".claude")).unwrap();

    // Step 1: Fresh install.
    common::skim_sandboxed(home_path)
        .args([
            "init",
            "--yes",
            "--agent",
            "claude-code",
            "--no-guidance",
            "--no-wrappers",
        ])
        .assert()
        .success();

    let script_path = home_path.join(".claude/hooks/skim-rewrite.sh");
    let manifest_path = home_path.join(".claude/hooks/skim-claude-code.sha256");
    assert!(script_path.exists(), "hook script must exist after init");
    assert!(manifest_path.exists(), "manifest must exist after init");

    // Capture the known-good content produced by the initial install.
    let good_content = fs::read(&script_path).expect("must be able to read hook script");

    // Step 2: Tamper — append one byte to the script while leaving the
    // version/commit markers unchanged, so hook_is_current() still returns true.
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&script_path)
            .expect("must be able to open hook script for appending");
        f.write_all(b"X").unwrap();
    }

    // Confirm the tamper is detectable: the script now differs from good_content.
    let tampered_content = fs::read(&script_path).unwrap();
    assert_ne!(
        good_content, tampered_content,
        "tampered script must differ from the original"
    );

    // Step 3: Re-run `skim init`. The self-heal path must REPAIR the script
    // (regenerate from source), NOT launder (hash-and-bless the tampered bytes).
    let out = common::skim_sandboxed(home_path)
        .args([
            "init",
            "--yes",
            "--agent",
            "claude-code",
            "--no-guidance",
            "--no-wrappers",
        ])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "init after tamper must succeed, got:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Repaired"),
        "init must report 'Repaired' for a tampered script, got:\n{stdout}"
    );

    // Step 4: The on-disk script must now match the known-good content —
    // NOT the tampered bytes.
    let repaired_content = fs::read(&script_path).unwrap();
    assert_eq!(
        good_content, repaired_content,
        "repaired script content must match the freshly generated known-good content, \
         not the tampered bytes"
    );

    // Step 5: `skim doctor` must now report Verified (not Tampered).
    // This confirms that the manifest was recomputed from the repaired content.
    //
    // We run doctor from the sandbox home (not a git repo) and prepend the
    // test binary's directory to PATH so the PATH scan does not spuriously
    // report drift from an unrelated release build.
    let bin = common::skim_bin();
    let bin_dir = bin.parent().expect("skim binary has a parent directory");
    let hermetic_path = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let doctor_out = common::skim_sandboxed(home_path)
        .arg("doctor")
        .current_dir(home_path)
        .env("PATH", hermetic_path)
        .output()
        .unwrap();

    let doctor_stdout = String::from_utf8_lossy(&doctor_out.stdout);
    assert!(
        doctor_out.status.success(),
        "skim doctor must exit 0 after repair, got:\n{doctor_stdout}"
    );
    assert!(
        !doctor_stdout.contains("tampered"),
        "skim doctor must NOT report tampered after repair, got:\n{doctor_stdout}"
    );
}

// ============================================================================
// Fix: --project --wrappers mutual exclusion
// ============================================================================

/// `skim init --project --wrappers` must be rejected at parse time with an
/// actionable error message — not silently accepted while installing zero
/// wrappers.
///
/// Before this fix, `--project` silently suppressed wrapper installation
/// (`if !flags.project { maybe_install_wrappers(...) }`) with no diagnostic,
/// matching the shape of the existing `--permissions + --project` guard.
#[test]
fn test_init_project_and_wrappers_is_rejected() {
    let home = TempDir::new().unwrap();
    let home_path = home.path();

    std::fs::create_dir_all(home_path.join(".claude")).unwrap();

    let out = common::skim_sandboxed(home_path)
        .args([
            "init",
            "--yes",
            "--agent",
            "claude-code",
            "--project",
            "--wrappers",
        ])
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "--project --wrappers must fail, but it succeeded"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stderr}{}", String::from_utf8_lossy(&out.stdout));
    assert!(
        combined.contains("mutually exclusive"),
        "--project --wrappers must report a mutual-exclusion error, got:\n{combined}"
    );
}
