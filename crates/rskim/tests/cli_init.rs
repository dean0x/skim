//! Integration tests for `skim init` and `skim rewrite --hook` (#44).
//!
//! # Hermeticity
//!
//! `skim init`, `skim init --uninstall` and `skim doctor` are an installer and
//! an uninstaller, so a test that runs them against the developer's real `$HOME`
//! does not merely read state — it *deletes* state. A global `--uninstall` with
//! no `--agent` removes wrapper symlinks from `~/.skim/bin` and guidance files
//! from `~/.gemini` and `~/.copilot` for every configured agent (PF-017).
//!
//! Every invocation in this file is therefore built by [`Sandbox`], which owns a
//! `TempDir` and routes through `common::skim_sandboxed`. The convention is not
//! left to memory: [`test_every_invocation_in_this_file_is_sandboxed`] scans this
//! file's own source and fails on any unsandboxed constructor, and
//! [`test_sandbox_env_block_classifies_every_env_var_the_crate_reads`] scans
//! `crates/rskim/src` and fails when a newly-added env read has no sandbox entry.
//!
//! Non-interactive tests pass `--yes`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
mod common;

// ============================================================================
// Sandbox — the only way to build a skim invocation in this file
// ============================================================================

/// A `TempDir` home plus the env block that confines a `skim` invocation to it.
///
/// Holding the `TempDir` inside the sandbox is what makes the confinement hard
/// to get wrong: a command cannot be built without a live sandbox, and the
/// sandbox cannot outlive the directory its env block points at. Every config
/// directory, cache directory and wrapper directory the child process resolves
/// lands under [`Sandbox::home`].
struct Sandbox {
    home: TempDir,
}

impl Sandbox {
    /// A sandbox with no agent config directory pre-created.
    ///
    /// Use for invocations that never reach agent detection (`--help`,
    /// `rewrite --hook`) or that create the directories they need themselves.
    fn bare() -> Self {
        Self {
            home: TempDir::new().expect("failed to create sandbox home"),
        }
    }

    /// A sandbox with `.claude/` already present — the common case.
    ///
    /// `detect_installed_agents()` in override mode only considers an agent
    /// whose override path is an existing directory (`p.is_dir()`), so a
    /// missing `.claude/` silently reduces an auto-detect install to a no-op.
    fn new() -> Self {
        let sandbox = Self::bare();
        sandbox.claude_config();
        sandbox
    }

    /// The sandbox home — `$HOME` for every invocation this sandbox builds.
    fn home(&self) -> &std::path::Path {
        self.home.path()
    }

    /// Create and return an agent config directory inside the sandbox.
    ///
    /// `dot_dir` must match the relative path the sandbox env block assigns to
    /// that agent's override variable (`common::SANDBOX_REDIRECTED_VARS`).
    fn config_dir(&self, dot_dir: &str) -> std::path::PathBuf {
        let path = self.home().join(dot_dir);
        fs::create_dir_all(&path).expect("failed to create sandbox config dir");
        path
    }

    /// `$CLAUDE_CONFIG_DIR` for invocations this sandbox builds.
    fn claude_config(&self) -> std::path::PathBuf {
        self.config_dir(".claude")
    }

    /// `$SKIM_CACHE_DIR` for invocations this sandbox builds — where `hook.log`
    /// and the force-raw sidecars land.
    fn cache_dir(&self) -> std::path::PathBuf {
        self.config_dir(".cache/skim")
    }

    /// Create and return a working directory for a `--project` install.
    fn project_dir(&self, name: &str) -> std::path::PathBuf {
        let path = self.home().join(name);
        fs::create_dir_all(&path).expect("failed to create sandbox project dir");
        path
    }

    /// Build a `skim` invocation confined to this sandbox.
    ///
    /// This is the single call site of `common::skim_sandboxed` in this file,
    /// and the guard test asserts it stays that way.
    fn skim(&self) -> Command {
        common::skim_sandboxed(self.home())
    }

    /// [`Sandbox::skim`] with the `init` subcommand already applied.
    fn init(&self) -> Command {
        let mut cmd = self.skim();
        cmd.arg("init");
        cmd
    }
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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    sandbox
        .init()
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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    sandbox.init().args(["--yes"]).assert().success();

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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

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

    sandbox.init().args(["--yes"]).assert().success();

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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

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
    sandbox
        .init()
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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    // Run init twice
    sandbox.init().args(["--yes"]).assert().success();

    sandbox.init().args(["--yes"]).assert().success();

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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    // Run init once
    sandbox.init().args(["--yes"]).assert().success();

    // Manually overwrite the hook script with an old version
    let hook_path = config.join("hooks/skim-rewrite.sh");
    let old_content = "#!/usr/bin/env bash\n# skim-hook v0.0.1\nexport SKIM_HOOK_VERSION=\"0.0.1\"\nexec skim rewrite --hook\n";
    fs::write(&hook_path, old_content).unwrap();

    // Run init again — should update the script
    sandbox
        .init()
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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    sandbox.init().args(["--yes"]).assert().success();

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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    sandbox.init().args(["--yes"]).assert().success();

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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();
    let real_dir = sandbox.home().join("real_claude");
    fs::create_dir_all(&real_dir).unwrap();

    // Create a real settings.json in the "real" location
    fs::write(real_dir.join("settings.json"), "{}").unwrap();

    // Symlink settings.json into the config dir
    std::os::unix::fs::symlink(real_dir.join("settings.json"), config.join("settings.json"))
        .unwrap();

    sandbox.init().args(["--yes"]).assert().success();

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
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("my-project");

    sandbox
        .init()
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
    let sandbox = Sandbox::new();

    // --yes should complete without stdin
    sandbox.init().args(["--yes"]).assert().success().stdout(
        predicate::str::contains("Done!").or(predicate::str::contains("Already up to date")),
    );
}

#[test]
fn test_init_project_yes() {
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("proj");

    sandbox
        .init()
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
fn test_init_non_tty_works_without_yes() {
    let sandbox = Sandbox::new();

    // Non-TTY install should succeed without --yes (non-interactive by default)
    sandbox.init().assert().success().stdout(
        predicate::str::contains("Done!").or(predicate::str::contains("Already up to date")),
    );
}

// ============================================================================
// Dry-run
// ============================================================================

#[test]
fn test_init_dry_run() {
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    sandbox
        .init()
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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    // First install
    sandbox.init().args(["--yes"]).assert().success();

    // Then uninstall
    sandbox
        .init()
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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    // Install skim
    sandbox.init().args(["--yes"]).assert().success();

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
    sandbox
        .init()
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
    // This is a GLOBAL uninstall with no `--agent`: before sandboxing it reached
    // `uninstall_wrappers` -> real `~/.skim/bin` and `remove_guidance` -> real
    // `~/.gemini/GEMINI.md`, the exact destructive path PF-017 names. The
    // assertion itself only depends on `$CLAUDE_CONFIG_DIR` being an empty
    // existing directory, so confining it costs nothing.
    let sandbox = Sandbox::new();

    sandbox
        .init()
        .args(["--uninstall", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Nothing to uninstall"));
}

// ============================================================================
// Hermeticity regression guard (PF-009 / PF-015)
// ============================================================================

/// Regression guard: `skim init` artifacts (hooks, wrappers, guidance) must land
/// inside the `TempDir` home owned by [`Sandbox`], not in the developer's real
/// home directory.
///
/// Covers every env-var override surface the sandbox redirects — `HOME`, the
/// five agent config dirs, the wrapper dir and the cache dir. The wrapper and
/// cache axes are asserted by walking the sandbox for anything that escaped it,
/// which is the only assertion shape that stays honest as the sandbox grows:
/// naming the artifacts one by one would pass for a var the block forgot.
#[test]
fn test_init_sandbox_artifacts_stay_inside_tempdir() {
    let sandbox = Sandbox::new();
    let claude_config = sandbox.claude_config();

    // Global install via the sandboxed helper (sets HOME + all per-agent config dirs).
    sandbox.init().args(["--yes"]).assert().success();

    // Claude Code hook artifacts must be inside the sandbox, not real ~/.claude/.
    assert!(
        claude_config.join("hooks/skim-rewrite.sh").exists(),
        "Hook script must land inside sandboxed Claude config (PF-017): \
         real ~/.claude/hooks/ must not be touched"
    );
    assert!(
        claude_config.join("settings.json").exists(),
        "settings.json must land inside sandboxed Claude config"
    );

    // Every redirected variable must resolve to a path inside the sandbox home.
    // `HOME` maps to the home itself, so `starts_with` holds for all of them.
    for (var, relative) in common::SANDBOX_REDIRECTED_VARS {
        let resolved = common::sandbox_var_path(sandbox.home(), relative);
        assert!(
            resolved.starts_with(sandbox.home()),
            "{var} must resolve inside the sandbox home, got: {}",
            resolved.display()
        );
    }

    // Wrapper dir — only created by `--wrappers`; this is a bare install.
    // If anything was written to the wrapper dir, it must be inside the sandbox.
    let sandbox_wrappers = sandbox.home().join(".skim").join("bin");
    if sandbox_wrappers.exists() {
        for entry in fs::read_dir(&sandbox_wrappers).unwrap() {
            let path = entry.unwrap().path();
            assert!(
                path.starts_with(sandbox.home()),
                "Wrapper symlink must be inside TempDir, found outside: {}",
                path.display()
            );
        }
    }

    // Uninstall via the same sandbox — cleanup must also stay inside TempDir.
    sandbox
        .init()
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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    // Create an existing settings.json
    fs::write(config.join("settings.json"), "{}\n").unwrap();

    sandbox.init().args(["--yes"]).assert().success();

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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    // Create a 0-byte settings.json
    fs::write(config.join("settings.json"), "").unwrap();

    sandbox.init().args(["--yes"]).assert().success();

    let contents = fs::read_to_string(config.join("settings.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&contents).unwrap();
    assert!(
        json["hooks"]["PreToolUse"].is_array(),
        "Should create valid structure from empty file"
    );
}

#[test]
fn test_init_malformed_json() {
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    // Create a malformed settings.json
    fs::write(config.join("settings.json"), "{not valid json}").unwrap();

    sandbox
        .init()
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
    let sandbox = Sandbox::bare();
    let output = sandbox
        .skim()
        .args(["rewrite", "--hook"])
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
    let sandbox = Sandbox::bare();
    let output = sandbox
        .skim()
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
    let sandbox = Sandbox::bare();
    let output = sandbox
        .skim()
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
    let sandbox = Sandbox::bare();
    let output = sandbox
        .skim()
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
    let sandbox = Sandbox::bare();
    let output = sandbox
        .skim()
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

    let sandbox = Sandbox::bare();
    let output = sandbox
        .skim()
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
    let sandbox = Sandbox::bare();
    let output = sandbox
        .skim()
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
    let sandbox = Sandbox::bare();
    let output = sandbox
        .skim()
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
    // The sandbox's own SKIM_CACHE_DIR keeps the stamp file and hook.log
    // per-test; the force-raw sidecar is PPID-keyed and would otherwise bleed
    // between tests sharing a nextest runner.
    let sandbox = Sandbox::bare();
    let cache_dir = sandbox.cache_dir();

    // Set SKIM_HOOK_VERSION to a value that differs from the compiled version.
    // The warning now goes to hook.log (NEVER stderr -- GRANITE #361 Bug 3).
    let output = sandbox
        .skim()
        .args(["rewrite", "--hook"])
        .env("SKIM_HOOK_VERSION", "0.0.1")
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
    let hook_log = cache_dir.join("hook.log");
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
    let sandbox = Sandbox::bare();
    let cache_dir = sandbox.cache_dir();

    // Set SKIM_HOOK_BINARY to a path that does not match the running binary.
    // Use a plausible but different path: /tmp/skim-other.  The versions match
    // so only the binary path mismatch check fires (check_hook_binary_mismatch).
    let current_version = env!("CARGO_PKG_VERSION");
    let output = sandbox
        .skim()
        .args(["rewrite", "--hook"])
        .env("SKIM_HOOK_VERSION", current_version)
        .env("SKIM_HOOK_BINARY", "/tmp/skim-other-binary")
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
    let hook_log = cache_dir.join("hook.log");
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
    let sandbox = Sandbox::bare();
    sandbox
        .skim()
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
    let sandbox = Sandbox::bare();
    sandbox
        .skim()
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
    // Create a CLAUDE.md at the "global" location (config_dir/../CLAUDE.md won't work,
    // so we test via project mode which creates CLAUDE.md in CWD)
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("proj");

    sandbox
        .init()
        .args(["--project", "--yes"])
        .current_dir(&project_dir)
        .assert()
        .success();

    // Check that CLAUDE.md was created with guidance
    let claude_md = project_dir.join("CLAUDE.md");
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
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("proj");

    sandbox
        .init()
        .args(["--project", "--yes", "--no-guidance"])
        .current_dir(&project_dir)
        .assert()
        .success();

    // CLAUDE.md should not exist (no guidance injected, file not created)
    let claude_md = project_dir.join("CLAUDE.md");
    assert!(
        !claude_md.exists(),
        "CLAUDE.md should not be created with --no-guidance"
    );
}

#[test]
fn test_init_uninstall_removes_guidance() {
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("proj");

    // First install with guidance
    sandbox
        .init()
        .args(["--project", "--yes"])
        .current_dir(&project_dir)
        .assert()
        .success();

    // Verify install created guidance
    let claude_md = project_dir.join("CLAUDE.md");
    assert!(claude_md.exists(), "CLAUDE.md should exist after install");

    // Then uninstall
    sandbox
        .init()
        .args(["--project", "--uninstall", "--yes"])
        .current_dir(&project_dir)
        .assert()
        .success();

    // CLAUDE.md should not contain skim guidance (or be deleted if it was the only content)
    let claude_md = project_dir.join("CLAUDE.md");
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
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("proj");

    // Install twice
    for _ in 0..2 {
        sandbox
            .init()
            .args(["--project", "--yes"])
            .current_dir(&project_dir)
            .assert()
            .success();
    }

    // CLAUDE.md should have exactly one skim section
    let claude_md = project_dir.join("CLAUDE.md");
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
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("proj");

    sandbox
        .init()
        .args(["--project", "--yes", "--dry-run"])
        .current_dir(&project_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("guidance"));
}

// ============================================================================
// Cursor .mdc format
// ============================================================================

#[test]
fn test_init_cursor_creates_mdc() {
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("proj");

    sandbox
        .init()
        .args(["--project", "--yes", "--agent", "cursor"])
        .current_dir(&project_dir)
        .assert()
        .success();

    // Should create .cursor/rules/skim.mdc with frontmatter
    let mdc = project_dir.join(".cursor/rules/skim.mdc");
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
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("proj");

    // Install
    sandbox
        .init()
        .args(["--project", "--yes", "--agent", "cursor"])
        .current_dir(&project_dir)
        .assert()
        .success();

    let mdc = project_dir.join(".cursor/rules/skim.mdc");
    assert!(mdc.exists(), "skim.mdc should exist after install");

    // Uninstall
    sandbox
        .init()
        .args(["--project", "--uninstall", "--yes", "--agent", "cursor"])
        .current_dir(&project_dir)
        .assert()
        .success();

    assert!(!mdc.exists(), "skim.mdc should be deleted on uninstall");
}

#[test]
fn test_init_cursor_cleans_legacy_cursorrules() {
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("proj");

    // Pre-populate a .cursorrules with skim markers (legacy format)
    let cursorrules = project_dir.join(".cursorrules");
    fs::write(
        &cursorrules,
        "# User rules\n\n<!-- skim-start v1.0.0 -->\nold guidance\n<!-- skim-end -->\n\n# More user rules\n",
    )
    .unwrap();

    // Install Cursor (should create .mdc AND clean legacy .cursorrules)
    sandbox
        .init()
        .args(["--project", "--yes", "--agent", "cursor"])
        .current_dir(&project_dir)
        .assert()
        .success();

    // New .mdc should exist
    let mdc = project_dir.join(".cursor/rules/skim.mdc");
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
    let sandbox = Sandbox::bare();
    sandbox
        .skim()
        .args(["init", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--agent"));
}

#[test]
fn test_rewrite_help_mentions_agent_flag() {
    // rewrite --help should mention the --agent flag
    let sandbox = Sandbox::bare();
    sandbox
        .skim()
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
    let sandbox = Sandbox::new();
    let project_dir = sandbox.project_dir("proj");

    // Step 1: fresh install — creates guidance at the current version
    sandbox
        .init()
        .args(["--project", "--yes"])
        .current_dir(&project_dir)
        .assert()
        .success();

    let claude_md = project_dir.join("CLAUDE.md");
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
    let output = sandbox
        .init()
        .args(["--project", "--yes"])
        .current_dir(&project_dir)
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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    sandbox.init().args(["--yes"]).assert().success();

    let settings = fs::read_to_string(config.join("settings.json")).unwrap();
    assert!(
        !settings.contains("marketplace"),
        "SECURITY: settings must never contain marketplace field"
    );
}

// ============================================================================
// Multi-agent auto-detect install and uninstall loop (issues 3 & 4)
// ============================================================================

#[test]
fn test_init_multi_agent_auto_detect_installs_claude_and_gemini() {
    // The sandbox sets an override for every agent, and detect_installed_agents
    // in override mode only picks up agents whose override path EXISTS. Creating
    // just .claude/ and .gemini/ therefore scopes detection to those two.
    let sandbox = Sandbox::new();
    sandbox.config_dir(".gemini");
    // Project dir: `skim init --project` installs to CWD-relative dirs, so both
    // agents write to <project>/.claude/ and <project>/.gemini/ respectively —
    // no home-directory writes occur during the test.
    let project_dir = sandbox.project_dir("proj");

    sandbox
        .skim()
        .args(["init", "--project", "--yes", "--no-guidance"])
        .current_dir(&project_dir)
        .assert()
        .success();

    // Claude Code: settings.json with hook entry
    let claude_settings = project_dir.join(".claude/settings.json");
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
    let gemini_settings = project_dir.join(".gemini/settings.json");
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
        project_dir.join(".claude/hooks/skim-rewrite.sh").exists(),
        "Claude Code hook script should be created"
    );
    assert!(
        project_dir.join(".gemini/hooks/skim-rewrite.sh").exists(),
        "Gemini CLI hook script should be created"
    );
}

#[test]
fn test_init_multi_agent_auto_detect_uninstalls_claude_and_gemini() {
    // Scope auto-detect to Claude Code and Gemini CLI by creating only those
    // two override directories inside the sandbox.
    let sandbox = Sandbox::new();
    sandbox.config_dir(".gemini");
    let project_dir = sandbox.project_dir("proj");

    // Step 1: install both agents
    sandbox
        .skim()
        .args(["init", "--project", "--yes", "--no-guidance"])
        .current_dir(&project_dir)
        .assert()
        .success();

    // Verify hook scripts exist after install
    let claude_hook = project_dir.join(".claude/hooks/skim-rewrite.sh");
    let gemini_hook = project_dir.join(".gemini/hooks/skim-rewrite.sh");
    assert!(
        claude_hook.exists(),
        "Claude Code hook should exist after install"
    );
    assert!(
        gemini_hook.exists(),
        "Gemini CLI hook should exist after install"
    );

    // Step 2: uninstall both agents without specifying --agent
    sandbox
        .skim()
        .args(["init", "--project", "--uninstall", "--yes", "--force"])
        .current_dir(&project_dir)
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
    let claude_settings = project_dir.join(".claude/settings.json");
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

    let gemini_settings = project_dir.join(".gemini/settings.json");
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
}

// ============================================================================
// Fix 3: Gemini dry-run uses BeforeTool hook key (not hardcoded PreToolUse)
// ============================================================================

/// `skim init --agent gemini --no-guidance --dry-run` must show "BeforeTool"
/// in the patch-settings description line, not the hardcoded "PreToolUse".
#[test]
fn test_gemini_dry_run_shows_before_tool_hook_key() {
    let sandbox = Sandbox::bare();
    sandbox.config_dir(".gemini");

    let output = sandbox
        .skim()
        .args(["init", "--agent", "gemini", "--no-guidance", "--dry-run"])
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

    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    // Step 1: Install once to get a valid hook structure (settings.json + script).
    sandbox.init().args(["--yes"]).assert().success();

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
    sandbox
        .init()
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
    let sandbox = Sandbox::new();

    // First install — establishes a fully-pinned script.
    sandbox.init().args(["--yes"]).assert().success();

    // Second install with the identical binary — must be idempotent.
    sandbox
        .init()
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
    let sandbox = Sandbox::new();

    // Step 1: Fresh install without wrappers — hook becomes current.
    sandbox.init().args(["--yes"]).assert().success();

    // Step 2: Re-run with --wrappers — fast path fires AND wrappers are installed.
    let out = sandbox
        .init()
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
    let sandbox = Sandbox::new();

    // Step 1: Fresh install — hook becomes current.
    sandbox
        .init()
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
    let out = sandbox
        .init()
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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    // Step 1: Write a pre-pin hook script directly (simulates a legacy install
    // without SKIM_HOOK_BINARY).
    let hooks_dir = config.join("hooks");
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
    let settings_path = config.join("settings.json");
    fs::write(
        &settings_path,
        r#"{"hooks":{"PreToolUse":[{"matcher":"","hooks":[{"type":"command","command":"skim-rewrite.sh rewrite --hook --agent claude-code"}]}]}}"#,
    )
    .unwrap();

    // Step 2: Run init with --force — must detect the missing pin and rewrite.
    sandbox
        .init()
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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    // Write a sentinel settings.json so patch_settings has something to back up.
    let settings_path = config.join("settings.json");
    let original_content = r#"{"custom":"user setting"}"#;
    fs::write(&settings_path, original_content).unwrap();

    let bak_path = config.join("settings.json.bak");

    // Step 1: First install with --wrappers — creates bak from the original file.
    sandbox
        .init()
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
    sandbox
        .init()
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
    let sandbox = Sandbox::new();
    let config = sandbox.claude_config();

    // Step 1: Fresh install.
    sandbox
        .skim()
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

    let script_path = config.join("hooks/skim-rewrite.sh");
    let manifest_path = config.join("hooks/skim-claude-code.sha256");
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
    let out = sandbox
        .skim()
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
    let doctor_out = sandbox
        .skim()
        .arg("doctor")
        .current_dir(sandbox.home())
        .env("PATH", common::hermetic_path())
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
// Dev mode is a property of the COMMAND, not sticky state (ADR-014)
// ============================================================================

/// The dev declaration an installed hook script carries.
///
/// Duplicated from `cmd::hooks::HOOK_DEV_MARKER`, which is `pub(crate)` inside a
/// bin-only crate and unreachable from an integration test. The duplication is
/// the point: this literal is the on-disk contract, so a test that followed the
/// production constant could never fail on a format change.
const DEV_MARKER_LINE: &str = "export SKIM_HOOK_DEV=1";

/// Install a hook, then leave it with NO integrity manifest — and, when
/// `declare_dev`, with the dev declaration appended.
///
/// Deleting the manifest is what makes this pair discriminating. With a manifest
/// present, appending the marker yields `Tampered`, and the pre-existing repair
/// path regenerates the script for a reason that has nothing to do with the
/// mode — so a passing test would prove nothing about `mode_matches`. With the
/// manifest gone the verdict is `NoManifest`, whose `create_hook_script` arm
/// SKIPS the write and re-stamps the on-disk bytes; the mode term is then the
/// only thing that can send the same script down the regeneration path instead.
fn install_then_declare(sandbox: &Sandbox, declare_dev: bool) -> std::path::PathBuf {
    let config = sandbox.claude_config();
    sandbox
        .skim()
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

    let script_path = config.join("hooks/skim-rewrite.sh");
    if declare_dev {
        let current = fs::read_to_string(&script_path).unwrap();
        fs::write(&script_path, format!("{current}{DEV_MARKER_LINE}\n")).unwrap();
    }
    fs::remove_file(config.join("hooks/skim-claude-code.sha256")).unwrap();
    script_path
}

/// `skim init` with no dev request must STRIP a dev declaration from the
/// installed script.
///
/// This is the counterweight to the commit-gate waiver and the reason
/// `mode_matches` exists as its own term: without it, a script that declares dev
/// mode survives every subsequent `skim init`, so the declaration becomes sticky
/// state and the feature would need an undo flag. ADR-014 rules that dev mode is
/// a property of the COMMAND — re-running the installer without the flag reverts
/// to strict pinning.
#[test]
fn test_init_without_dev_request_strips_a_dev_declaration() {
    let sandbox = Sandbox::new();
    let script_path = install_then_declare(&sandbox, true);

    let declared = fs::read_to_string(&script_path).unwrap();
    assert!(
        declared.contains("SKIM_HOOK_DEV"),
        "setup must leave the declaration in the script"
    );

    let out = sandbox
        .skim()
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
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(out.status.success(), "init must succeed, got:\n{stdout}");
    let reverted = fs::read_to_string(&script_path).unwrap();
    assert!(
        !reverted.contains("SKIM_HOOK_DEV"),
        "a plain `skim init` must rewrite the script back to strict, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("Skipped"),
        "the mode mismatch must send the script down the write path, not the \
         skip-and-re-stamp path, got:\n{stdout}"
    );
}

/// The control: identical setup MINUS the declaration takes the skip path.
///
/// Without this, the test above could be passing because a missing manifest
/// alone forces a rewrite — which would make `mode_matches` unobservable and the
/// assertion vacuous.
#[test]
fn test_init_without_a_declaration_still_takes_the_skip_path() {
    let sandbox = Sandbox::new();
    let script_path = install_then_declare(&sandbox, false);

    let out = sandbox
        .skim()
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
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(out.status.success(), "init must succeed, got:\n{stdout}");
    assert!(
        stdout.contains("Skipped"),
        "a strict script with no manifest must be skipped and re-stamped, not \
         rewritten — otherwise the test above proves nothing, got:\n{stdout}"
    );
    assert!(
        script_path.exists(),
        "the script must survive the self-heal"
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
    let sandbox = Sandbox::new();

    let out = sandbox
        .skim()
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

// ============================================================================
// Hermeticity guards (PF-017)
// ============================================================================
//
// PF-017's durable lesson is that sandboxing an installer's own tests is a
// convention, and a convention that must be remembered is one that will be
// forgotten: five days after the first fix shipped, a new test hand-rolled its
// own env block and dropped two variables from it. The two tests below turn the
// convention into a failure. They guard different things and neither subsumes
// the other — the first catches a test that escapes the sandbox, the second
// catches a sandbox that has stopped covering the program.

/// Guard: every `skim` invocation in this file is built by [`Sandbox`].
///
/// `skim init --uninstall` with no `--agent` removes wrapper symlinks and
/// guidance files for every configured agent, so an unsandboxed invocation here
/// does not read the developer's home directory — it deletes from it. There is
/// no consent gate on that path to catch the mistake later.
#[test]
fn test_every_invocation_in_this_file_is_sandboxed() {
    // `include_str!` embeds this file's own source, so each needle is assembled
    // from fragments: spelled out whole, a needle would match its own text here
    // and the guard would fail on itself rather than on a real violation.
    let source = include_str!("cli_init.rs");

    let forbidden = [
        (
            concat!("common::", "skim()"),
            "builds an UNSANDBOXED command against the real $HOME",
        ),
        (
            concat!("common::", "skim_with_analytics"),
            "writes to a caller-chosen analytics DB rather than the sandbox's",
        ),
        (
            concat!("common::", "skim_sandboxed_with_bin"),
            "is the low-level builder — go through Sandbox::skim",
        ),
        (
            concat!("cargo", "_bin"),
            "resolves the binary directly, skipping the sandbox env block",
        ),
        (
            concat!("Command", "::new("),
            "constructs a bare command that inherits the host environment",
        ),
    ];

    for (needle, why) in forbidden {
        assert!(
            !source.contains(needle),
            "cli_init.rs uses `{needle}`, which {why}. Build every invocation \
             with `Sandbox::skim()` / `Sandbox::init()` instead — a global \
             `skim init --uninstall` DELETES real agent config (PF-017)."
        );
    }

    // Exactly one call to the sandboxed constructor: the one in `Sandbox::skim`.
    // A second call site is a second sandbox definition waiting to drift.
    let sandboxed = concat!("common::", "skim_sandboxed(");
    let call_sites = source.matches(sandboxed).count();
    assert_eq!(
        call_sites, 1,
        "`{sandboxed}` must appear exactly once in cli_init.rs (inside \
         `Sandbox::skim`), found {call_sites}."
    );

    // Setting a variable the sandbox owns re-opens the hole the sandbox closes:
    // a hand-rolled override is the exact shape PF-017 caught the second time.
    for (var, _) in common::SANDBOX_REDIRECTED_VARS {
        let needle = format!(".env(\"{var}\"");
        assert!(
            !source.contains(&needle),
            "cli_init.rs sets `{var}` by hand, but the sandbox already redirects \
             it. Use the matching `Sandbox` accessor for that path instead."
        );
    }
}

/// Guard: the sandbox env block still accounts for every variable the program
/// reads.
///
/// A hand-maintained enumeration of overrides is always incomplete (PF-017), so
/// this checks the enumeration against the source rather than trusting it: each
/// env var `crates/rskim/src` reads must be redirected, pinned, removed, or
/// explicitly inherited. Adding an env read without classifying it fails here.
///
/// Scope is the binary crate because that is where all env access lives —
/// `rskim-core` is a pure transform library with no I/O side effects. A read
/// added there would escape this guard, which is the cost of the narrow scope.
#[test]
fn test_sandbox_env_block_classifies_every_env_var_the_crate_reads() {
    /// Indirect reads (`env::var(SOMETHING)`) whose argument is neither a string
    /// literal nor a resolvable `const`. `name` is the `|name: &str|` parameter
    /// of the `read` closures in `DetectionEnv::from_process` and
    /// `InstructionEnv::from_process`; their call sites are string literals and
    /// are covered by the `read("…")` pattern below.
    const EXPECTED_INDIRECT_READS: &[&str] = &["name"];

    let src_root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let sources = rust_sources_under(src_root);
    assert!(
        !sources.is_empty(),
        "found no Rust sources under {} — the guard would pass vacuously",
        src_root.display()
    );

    // Two passes: consts first, because a read may resolve a const defined in
    // another file.
    let mut consts = BTreeMap::new();
    let mut bodies = Vec::with_capacity(sources.len());
    for path in &sources {
        let body = fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        collect_string_consts(&body, &mut consts);
        bodies.push(body);
    }

    let mut names = BTreeSet::new();
    let mut unresolved = BTreeSet::new();
    for body in &bodies {
        collect_env_reads(body, &consts, &mut names, &mut unresolved);
    }

    let unexpected: Vec<&String> = unresolved
        .iter()
        .filter(|ident| !EXPECTED_INDIRECT_READS.contains(&ident.as_str()))
        .collect();
    assert!(
        unexpected.is_empty(),
        "env var(s) read through an unresolvable expression: {unexpected:?}. \
         The sandbox cannot classify what it cannot name — give the variable a \
         `const NAME: &str = \"…\";` binding, or add the argument to \
         EXPECTED_INDIRECT_READS with a note on where its literals live."
    );

    // A variable must be in EXACTLY one table. Listing one in two tables is
    // silently resolved by whichever loop in `skim_sandboxed_with_bin` runs
    // last — a var in both REDIRECTED and REMOVED ends up removed, and the
    // union check below would still call it classified.
    let mut classified: BTreeSet<String> = BTreeSet::new();
    let tables = common::SANDBOX_REDIRECTED_VARS
        .iter()
        .map(|(var, _)| *var)
        .chain(common::SANDBOX_PINNED_VARS.iter().map(|(var, _)| *var))
        .chain(common::SANDBOX_REMOVED_VARS.iter().copied())
        .chain(common::SANDBOX_INHERITED_VARS.iter().copied());
    for var in tables {
        assert!(
            classified.insert(var.to_string()),
            "`{var}` appears in more than one sandbox table; the later loop in \
             `skim_sandboxed_with_bin` silently wins. Keep each variable in \
             exactly one table."
        );
    }

    let unclassified: Vec<&String> = names
        .iter()
        .filter(|name| !classified.contains(name.as_str()))
        .collect();
    assert!(
        unclassified.is_empty(),
        "env var(s) read by crates/rskim/src with no sandbox entry: \
         {unclassified:?}. Add each to exactly one table in tests/common/mod.rs \
         — SANDBOX_REDIRECTED_VARS if it names a path that could reach real user \
         state, SANDBOX_REMOVED_VARS if a host value would leak session state \
         into a test, SANDBOX_PINNED_VARS if tests need a fixed value, or \
         SANDBOX_INHERITED_VARS with a written reason why the host value is safe."
    );
}

/// Collect `.rs` files under `root`, with explicit bounds on the walk.
///
/// The bounds are what make a symlink cycle fail loudly instead of hanging.
fn rust_sources_under(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    /// Upper bound on directories visited — the crate has well under 100.
    const MAX_DIRS: usize = 1024;
    /// Upper bound on files collected.
    const MAX_FILES: usize = 4096;

    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    let mut dirs_visited: usize = 0;

    while let Some(dir) = pending.pop() {
        dirs_visited += 1;
        assert!(
            dirs_visited <= MAX_DIRS,
            "directory walk exceeded {MAX_DIRS} directories under {} — cycle?",
            root.display()
        );
        let entries = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("failed to read dir {}: {e}", dir.display()));
        for entry in entries {
            let path = entry
                .unwrap_or_else(|e| panic!("failed to read entry in {}: {e}", dir.display()))
                .path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                assert!(
                    files.len() < MAX_FILES,
                    "collected more than {MAX_FILES} Rust sources under {}",
                    root.display()
                );
                files.push(path);
            }
        }
    }
    files
}

/// Record `const NAME: &str = "value";` bindings so an indirect env read
/// written as `env::var(NAME)` can be resolved back to its variable name.
fn collect_string_consts(source: &str, out: &mut BTreeMap<String, String>) {
    for line in source.lines() {
        let Some(idx) = line.find("const ") else {
            continue;
        };
        let rest = &line[idx + "const ".len()..];
        let Some((name, tail)) = rest.split_once(':') else {
            continue;
        };
        let Some((ty, value)) = tail.split_once('=') else {
            continue;
        };
        if ty.trim() != "&str" {
            continue;
        }
        let Some(value) = value.trim().strip_prefix('"') else {
            continue;
        };
        // Everything up to the closing quote; `split` always yields a first item.
        let value = value.split('"').next().unwrap_or_default();
        out.insert(name.trim().to_string(), value.to_string());
    }
}

/// Extract env-var names read by `source` into `names`, and the arguments of
/// reads that could not be resolved to a name into `unresolved`.
fn collect_env_reads(
    source: &str,
    consts: &BTreeMap<String, String>,
    names: &mut BTreeSet<String>,
    unresolved: &mut BTreeSet<String>,
) {
    // `read(` is the `|name: &str|` closure both `from_process` impls use. It is
    // the ambiguous one — it also matches `fs::read("path")` — so only its
    // quoted form is taken, and only when the literal has env-var shape.
    const READ_CLOSURE: &str = "read(";
    const PATTERNS: &[&str] = &["env::var(", "env::var_os(", READ_CLOSURE];

    for pattern in PATTERNS {
        for (idx, _) in source.match_indices(pattern) {
            let rest = &source[idx + pattern.len()..];
            if let Some(quoted) = rest.strip_prefix('"') {
                let name = quoted.split('"').next().unwrap_or_default();
                if *pattern == READ_CLOSURE && !has_env_var_shape(name) {
                    continue;
                }
                names.insert(name.to_string());
            } else if *pattern != READ_CLOSURE {
                let ident: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                match consts.get(&ident) {
                    Some(value) => {
                        names.insert(value.clone());
                    }
                    None => {
                        unresolved.insert(ident);
                    }
                }
            }
        }
    }
}

/// `SCREAMING_SNAKE_CASE` — the shape every env var this crate reads has.
fn has_env_var_shape(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}
