//! Integration tests for `skim init` and `skim rewrite --hook` (#44).
//!
//! All tests use `tempfile::TempDir` + `CLAUDE_CONFIG_DIR` env override for
//! isolation. Non-interactive tests pass `--yes`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

// ============================================================================
// Helper: build an isolated `skim init` command with CLAUDE_CONFIG_DIR override
// ============================================================================

fn skim_init_cmd(config_dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
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
// Marketplace
// ============================================================================

#[test]
fn test_init_adds_marketplace() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    skim_init_cmd(config).args(["--yes"]).assert().success();

    let contents = fs::read_to_string(config.join("settings.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&contents).unwrap();

    let skim_mkt = &json["extraKnownMarketplaces"]["skim"];
    assert!(
        skim_mkt.is_object(),
        "Should have extraKnownMarketplaces.skim"
    );
    assert_eq!(skim_mkt["source"]["repo"], "dean0x/skim");
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
    fs::create_dir_all(&project_dir).unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg("init")
        .args(["--project", "--yes"])
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
    fs::create_dir_all(&project_dir).unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .arg("init")
        .args(["--project", "--yes"])
        .current_dir(&project_dir)
        .assert()
        .success();

    assert!(project_dir.join(".claude/settings.json").exists());
}

// ============================================================================
// Non-TTY detection
// ============================================================================

#[test]
fn test_init_non_tty_without_yes_fails() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    // When invoked without --yes and stdin is not a terminal (piped),
    // should fail with a hint.
    // Note: assert_cmd by default provides non-TTY stdin.
    skim_init_cmd(config)
        .assert()
        .failure()
        .stderr(predicate::str::contains("interactive terminal"))
        .stderr(predicate::str::contains("--yes"));
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
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    // First install
    skim_init_cmd(config).args(["--yes"]).assert().success();

    // Then uninstall
    skim_init_cmd(config)
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
    assert!(
        !contents.contains("\"skim\""),
        "Marketplace entry should be removed"
    );
}

#[test]
fn test_init_uninstall_preserves_other_hooks() {
    let dir = TempDir::new().unwrap();
    let config = dir.path();

    // Install skim
    skim_init_cmd(config).args(["--yes"]).assert().success();

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
    skim_init_cmd(config)
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
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "--hook"])
        .write_stdin(hook_payload("cargo test"))
        .assert()
        .success();

    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(json["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert!(json["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap()
        .contains("skim test cargo"));
}

#[test]
fn test_hook_no_match_empty_output() {
    let output = Command::cargo_bin("skim")
        .unwrap()
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
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "--hook"])
        .write_stdin(hook_payload("skim test cargo"))
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
    let output = Command::cargo_bin("skim")
        .unwrap()
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
    let output = Command::cargo_bin("skim")
        .unwrap()
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

    let output = Command::cargo_bin("skim")
        .unwrap()
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
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "--hook"])
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
        rewritten.contains("skim test cargo"),
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
    let output = Command::cargo_bin("skim")
        .unwrap()
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
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["rewrite", "--hook"])
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
            .contains("skim test cargo"),
        "Rewrite should succeed despite version mismatch"
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

// ============================================================================
// Help text
// ============================================================================

#[test]
fn test_init_help() {
    Command::cargo_bin("skim")
        .unwrap()
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
    Command::cargo_bin("skim")
        .unwrap()
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

    Command::cargo_bin("skim")
        .unwrap()
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
        content.contains("npx rskim"),
        "Guidance should reference npx rskim"
    );
}

#[test]
fn test_init_no_guidance_flag() {
    let dir = TempDir::new().unwrap();
    let project_dir = TempDir::new().unwrap();

    Command::cargo_bin("skim")
        .unwrap()
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
    Command::cargo_bin("skim")
        .unwrap()
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
    Command::cargo_bin("skim")
        .unwrap()
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
        Command::cargo_bin("skim")
            .unwrap()
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

    Command::cargo_bin("skim")
        .unwrap()
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

    Command::cargo_bin("skim")
        .unwrap()
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
    Command::cargo_bin("skim")
        .unwrap()
        .arg("init")
        .args(["--project", "--yes", "--agent", "cursor"])
        .env("CLAUDE_CONFIG_DIR", config_dir.path().as_os_str())
        .current_dir(project_dir.path())
        .assert()
        .success();

    let mdc = project_dir.path().join(".cursor/rules/skim.mdc");
    assert!(mdc.exists(), "skim.mdc should exist after install");

    // Uninstall
    Command::cargo_bin("skim")
        .unwrap()
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
    Command::cargo_bin("skim")
        .unwrap()
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
    Command::cargo_bin("skim")
        .unwrap()
        .args(["init", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--agent"));
}

#[test]
fn test_rewrite_help_mentions_agent_flag() {
    // rewrite --help should mention the --agent flag
    Command::cargo_bin("skim")
        .unwrap()
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
    Command::cargo_bin("skim")
        .unwrap()
        .arg("init")
        .args(["--project", "--yes"])
        .env("CLAUDE_CONFIG_DIR", config.as_os_str())
        .current_dir(project_dir.path())
        .assert()
        .success();

    let claude_md = project_dir.path().join("CLAUDE.md");
    assert!(claude_md.exists(), "CLAUDE.md should exist after initial install");

    // Step 2: manually overwrite the guidance section with an old version marker
    let content = fs::read_to_string(&claude_md).unwrap();
    assert!(
        content.contains("<!-- skim-start"),
        "Initial install should have created a skim-start marker"
    );
    // Replace the versioned marker with an obviously stale one
    let stale_content = {
        let start = content.find("<!-- skim-start").expect("start marker must exist");
        let marker_end = content[start..].find(" -->").expect("marker closing must exist");
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
    let output = Command::cargo_bin("skim")
        .unwrap()
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
