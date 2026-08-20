//! Copilot CLI hook protocol implementation.
//!
//! ## Hook artifact location
//!
//! Copilot CLI hook artifacts (script, SHA sidecar, `skim.json` registration)
//! live under `~/.copilot/hooks/`, separate from the `~/.github/` directory
//! that holds settings and project instructions. `dot_dir_name()` stays
//! `".github"` — only `hook_config_dir()` redirects to `~/.copilot`.
//!
//! When `COPILOT_CONFIG_DIR` is set (test sandboxing or user override) or the
//! install is project-scoped, the redirect is suppressed and the caller-supplied
//! `resolved_config_dir` is used verbatim.
//!
//! ## Hook registration format
//!
//! Instead of injecting an entry into a settings file, Copilot CLI uses a
//! dedicated `hooks/skim.json` with the envelope:
//! ```json
//! { "version": 1, "hooks": { "preToolUse": [ <command entry> ] } }
//! ```
//! Foreign hooks are other `*.json` files in the same `hooks/` directory.
//!
//! ## Response format
//!
//! On rewrite, `format_response` returns:
//! ```json
//! { "modifiedArgs": { "command": "<rewritten command>" } }
//! ```
//! No `permissionDecision` field is emitted — skim never self-approves.
//! Copilot CLI joins Claude Code in the no-verb column: a permission verb is
//! only emitted where the host protocol forces a decision field (Gemini, Crush,
//! Cursor). `modifiedArgs` is respected by Copilot CLI >= v1.0.24; older CLIs
//! silently ignore it (inert passthrough — not a breakage).
//!
//! ## Legacy migration
//!
//! Earlier versions of skim wrote Copilot hook artifacts to `~/.github/hooks/`
//! (the config dir) rather than `~/.copilot/hooks/` (the dedicated hook dir).
//! `migrate_copilot_legacy` cleans up those artifacts during install. The new
//! artifacts are written and verified FIRST; legacy removal is guard-gated and
//! individually non-fatal.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::{HookInput, HookProtocol, HookSupport};
use crate::cmd::session::AgentKind;

/// Filename of the skim-owned hook registration for Copilot CLI.
/// Exported so install.rs can build the dry-run / summary path without a
/// second string literal.
pub(crate) const SKIM_JSON_NAME: &str = "skim.json";

/// Maximum number of hook-registration files enumerated per directory scan.
/// Bounds the directory walk against adversarial or corrupt hook directories;
/// matches the cap in `HookProtocol::scan_other_hooks`.
const MAX_HOOK_FILES: usize = 50;

/// Copilot CLI hook implementation (preToolUse hooks, deny-with-suggestion).
pub(crate) struct CopilotCliHook;

impl HookProtocol for CopilotCliHook {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::CopilotCli
    }

    fn hook_support(&self) -> HookSupport {
        HookSupport::RealHook
    }

    fn parse_input(&self, json: &serde_json::Value) -> Option<HookInput> {
        super::parse_tool_input_command(json)
    }

    fn format_response(&self, rewritten_command: &str) -> serde_json::Value {
        // modifiedArgs response: echo the tool-arguments shape with only the command rewritten.
        // No permissionDecision field — skim never self-approves (no-verb column: only emit
        // a permission verb where the host protocol forces a decision field, never voluntarily).
        // Respected by Copilot CLI >= v1.0.24; older CLIs silently ignore it (inert passthrough).
        serde_json::json!({
            "modifiedArgs": {
                "command": rewritten_command
            }
        })
    }

    fn generate_script(&self, version: &str, binary_path: &str) -> String {
        super::generate_hook_script(version, "copilot", binary_path)
    }

    // -------------------------------------------------------------------------
    // Hook artifact location seam overrides
    //
    // Copilot CLI hook artifacts live under ~/.copilot/ (not ~/.github/),
    // and registration uses hooks/skim.json (not a settings.json entry).
    // -------------------------------------------------------------------------

    /// Hook artifacts live under `~/.copilot/` (or `$COPILOT_CONFIG_DIR` if
    /// the env override is active, or the project dir for project-scope installs).
    fn hook_config_dir(
        &self,
        resolved_config_dir: &Path,
        project_scope: bool,
        has_override: bool,
    ) -> PathBuf {
        if project_scope || has_override {
            // Project scope: keep in the project tree.
            // Env override: use the caller-supplied path verbatim (sandbox safety).
            resolved_config_dir.to_path_buf()
        } else {
            // Global default: hook artifacts go under ~/.copilot, separate from
            // the ~/.github settings/rules dir (dot_dir_name stays ".github").
            dirs::home_dir()
                .map(|h| h.join(".copilot"))
                .unwrap_or_else(|| resolved_config_dir.to_path_buf())
        }
    }

    fn uses_dedicated_hook_file(&self) -> bool {
        true
    }

    fn detect_hook_registration(&self, hook_config_dir: &Path) -> bool {
        let skim_json = hook_config_dir.join("hooks").join(SKIM_JSON_NAME);
        copilot_skim_json_has_entry(&skim_json)
    }

    fn install_hook_registration(
        &self,
        hook_config_dir: &Path,
        script_path: &str,
    ) -> anyhow::Result<bool> {
        write_copilot_skim_json(hook_config_dir, script_path, self)?;
        Ok(true)
    }

    fn remove_hook_registration(&self, hook_config_dir: &Path) -> anyhow::Result<bool> {
        let skim_json = hook_config_dir.join("hooks").join(SKIM_JSON_NAME);
        if skim_json.exists() {
            std::fs::remove_file(&skim_json)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn scan_foreign_hooks(&self, hook_config_dir: &Path) -> Vec<String> {
        scan_copilot_foreign_hooks(hook_config_dir)
    }

    // -------------------------------------------------------------------------
    // Config lifecycle overrides — Copilot CLI uses preToolUse (lowercase)
    // and a "bash" field format for entries
    // -------------------------------------------------------------------------

    /// Copilot CLI uses lowercase `preToolUse` instead of `PreToolUse`.
    fn hook_event_key(&self) -> &'static str {
        "preToolUse"
    }

    /// Copilot CLI matches on `bash` tool name.
    fn tool_matcher(&self) -> &'static str {
        "bash"
    }

    /// Copilot CLI config entry uses `"bash"` field (not `"command"`) and
    /// `"timeoutSec"` (not `"timeout"`).
    ///
    /// Format: `{ "type": "command", "bash": "<path>", "timeoutSec": 5 }`
    ///
    /// Note: the `matcher` field is intentionally omitted — it is not in the
    /// documented Copilot CLI hook schema and an unrecognised field risks the
    /// entire hooks file being rejected. Conservative posture: documented fields only.
    fn build_config_entry(&self, hook_script_path: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "command",
            "bash": hook_script_path,
            "timeoutSec": self.hook_timeout()
        })
    }

    /// Copilot entries use `"bash"` field — skim entries reference `"skim-rewrite"` there.
    fn is_skim_entry(&self, entry: &serde_json::Value) -> bool {
        entry
            .get("bash")
            .and_then(|c| c.as_str())
            .is_some_and(|c| c.contains("skim-rewrite"))
    }

    /// Copilot's config wraps event arrays in `{ "version": 1, "hooks": { "preToolUse": [...] } }`.
    /// Delegates to the shared `upsert_hook_versioned` helper.
    fn upsert_hook(
        &self,
        config: &mut serde_json::Value,
        hook_script_path: &str,
    ) -> anyhow::Result<()> {
        super::upsert_hook_versioned(config, hook_script_path, self)
    }
}

// ============================================================================
// Copilot hook-file helpers
// ============================================================================

/// Atomically write `hooks/skim.json` under `hook_config_dir`.
///
/// The file envelope is:
/// ```json
/// { "version": 1, "hooks": { "preToolUse": [ <command entry> ] } }
/// ```
/// `script_path` is embedded in the `"bash"` field of the command entry.
/// The hooks directory is created if it does not exist.
///
/// Uses a `.tmp`-sibling write + rename (crash-safe), the same idiom used by
/// `atomic_write_settings` and `write_sidecar`. On Unix the temporary file is
/// created with mode `0o600` (owner read/write only) before the rename.
pub(crate) fn write_copilot_skim_json(
    hook_config_dir: &Path,
    script_path: &str,
    protocol: &CopilotCliHook,
) -> anyhow::Result<()> {
    let hooks_dir = hook_config_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir)?;
    let skim_json_path = hooks_dir.join(SKIM_JSON_NAME);
    let entry = protocol.build_config_entry(script_path);
    let json = serde_json::json!({
        "version": 1,
        "hooks": {
            "preToolUse": [entry]
        }
    });
    let content = serde_json::to_string_pretty(&json)?;
    let tmp_path = skim_json_path.with_extension("json.tmp");
    std::fs::write(&tmp_path, format!("{content}\n"))?;
    #[cfg(unix)]
    {
        let perms = std::fs::Permissions::from_mode(0o600);
        if let Err(e) = std::fs::set_permissions(&tmp_path, perms) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e.into());
        }
    }
    std::fs::rename(&tmp_path, &skim_json_path)?;
    Ok(())
}

/// Returns `true` when `skim_json` exists and contains a skim command entry
/// under `hooks.preToolUse`.
fn copilot_skim_json_has_entry(skim_json: &Path) -> bool {
    use crate::cmd::init::MAX_SETTINGS_SIZE;
    // Reject oversized files — a valid skim.json is always small.
    if std::fs::metadata(skim_json)
        .map(|m| m.len() > MAX_SETTINGS_SIZE)
        .unwrap_or(true)
    {
        return false;
    }
    let Ok(contents) = std::fs::read_to_string(skim_json) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) else {
        return false;
    };
    json.get("hooks")
        .and_then(|h| h.get("preToolUse"))
        .and_then(|v| v.as_array())
        .is_some_and(|arr| arr.iter().any(|e| CopilotCliHook.is_skim_entry(e)))
}

/// Enumerate other `*.json` files in `hook_config_dir/hooks/` (i.e. everything
/// except `skim.json`) and return the `"bash"` command strings found within.
fn scan_copilot_foreign_hooks(hook_config_dir: &Path) -> Vec<String> {
    use crate::cmd::init::MAX_SETTINGS_SIZE;
    let hooks_dir = hook_config_dir.join("hooks");
    let Ok(read_dir) = std::fs::read_dir(&hooks_dir) else {
        return Vec::new();
    };
    let mut other = Vec::new();
    for entry in read_dir.flatten().take(MAX_HOOK_FILES) {
        let path = entry.path();
        // Only *.json files that are not our own skim.json.
        if path.extension() != Some(OsStr::new("json")) {
            continue;
        }
        if path.file_name() == Some(OsStr::new(SKIM_JSON_NAME)) {
            continue;
        }
        // Reject oversized files before reading — a valid hook file is small.
        if std::fs::metadata(&path)
            .map(|m| m.len() > MAX_SETTINGS_SIZE)
            .unwrap_or(true)
        {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) else {
            continue;
        };
        // Extract "bash" fields from preToolUse entries in the foreign file.
        if let Some(arr) = json
            .get("hooks")
            .and_then(|h| h.get("preToolUse"))
            .and_then(|v| v.as_array())
        {
            for e in arr {
                if let Some(bash) = e.get("bash").and_then(|c| c.as_str()) {
                    other.push(bash.to_string());
                }
            }
        }
    }
    other
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::hooks::{InstallOpts, UninstallOpts};

    fn hook() -> CopilotCliHook {
        CopilotCliHook
    }

    #[test]
    fn test_copilot_hook_is_real() {
        assert_eq!(hook().hook_support(), HookSupport::RealHook);
    }

    #[test]
    fn test_copilot_parse_input() {
        let json = serde_json::json!({
            "tool_input": {
                "command": "cargo test --all"
            }
        });
        let result = hook().parse_input(&json);
        assert!(result.is_some());
        assert_eq!(result.unwrap().command, "cargo test --all");
    }

    #[test]
    fn test_copilot_parse_input_missing_tool_input() {
        let json = serde_json::json!({});
        assert!(hook().parse_input(&json).is_none());
    }

    #[test]
    fn test_copilot_parse_input_missing_command() {
        let json = serde_json::json!({
            "tool_input": {
                "file_path": "/tmp/test.rs"
            }
        });
        assert!(hook().parse_input(&json).is_none());
    }

    #[test]
    fn test_copilot_format_response_returns_modified_args() {
        let response = hook().format_response("skim cargo test --all");
        // Must return modifiedArgs with the rewritten command
        assert_eq!(
            response["modifiedArgs"]["command"], "skim cargo test --all",
            "modifiedArgs.command must equal the rewritten command"
        );
    }

    #[test]
    fn test_copilot_format_response_no_reason_field() {
        let response = hook().format_response("skim cargo test");
        // No reason field — modifiedArgs response is terse
        assert!(
            response.get("reason").is_none(),
            "format_response must not include a reason field"
        );
    }

    #[test]
    fn test_copilot_format_response_no_permission_decision() {
        // skim never self-approves — no permissionDecision in the response
        let response = hook().format_response("skim cargo test");
        assert!(
            response.get("permissionDecision").is_none(),
            "Copilot response must NOT include permissionDecision (no-verb column)"
        );
    }

    #[test]
    fn test_copilot_format_response_no_hook_specific_output() {
        let response = hook().format_response("skim cargo test");
        // Copilot uses modifiedArgs, not hookSpecificOutput
        assert!(
            response.get("hookSpecificOutput").is_none(),
            "copilot should not use hookSpecificOutput"
        );
    }

    #[test]
    fn test_copilot_generate_script_pinned_binary() {
        let script = hook().generate_script("2.0.0", "/usr/local/bin/skim");
        assert!(script.contains("#!/usr/bin/env bash"));
        assert!(script.contains("# skim-hook v2.0.0"));
        assert!(script.contains("skim init --agent copilot"));
        assert!(script.contains("SKIM_HOOK_VERSION=\"2.0.0\""));
        assert!(script.contains("export SKIM_HOOK_BINARY="));
        assert!(script.contains("export SKIM_HOOK_COMMIT="));
        assert!(script.contains("exec \"$SKIM_HOOK_BINARY\" rewrite --hook --agent copilot"));
        // PATH fallback must still be present.
        assert!(script.contains("exec skim rewrite --hook --agent copilot"));
    }

    #[test]
    fn test_copilot_agent_kind() {
        assert_eq!(hook().agent_kind(), AgentKind::CopilotCli);
    }

    #[test]
    fn test_copilot_install_default() {
        let opts = InstallOpts {
            version: "1.0.0".into(),
            config_dir: "/tmp/.copilot".into(),
            project_scope: false,
            dry_run: false,
        };
        let result = hook().install(&opts).unwrap();
        assert!(result.script_path.is_none());
        assert!(!result.config_patched);
    }

    #[test]
    fn test_copilot_uninstall_default() {
        let opts = UninstallOpts {
            config_dir: "/tmp/.copilot".into(),
            force: false,
        };
        assert!(hook().uninstall(&opts).is_ok());
    }

    // ========================================================================
    // Phase 4: Config lifecycle override tests (AC-4)
    // ========================================================================

    #[test]
    fn test_copilot_config_filename() {
        assert_eq!(hook().config_filename(), "settings.json");
    }

    #[test]
    fn test_copilot_hook_event_key_is_lowercase() {
        assert_eq!(hook().hook_event_key(), "preToolUse");
    }

    #[test]
    fn test_copilot_tool_matcher() {
        assert_eq!(hook().tool_matcher(), "bash");
    }

    #[test]
    fn test_copilot_build_config_entry_uses_bash_field() {
        let entry = hook().build_config_entry("/home/user/.copilot/hooks/skim-rewrite.sh");
        // Must use "bash" field, not "command"
        assert_eq!(
            entry["bash"], "/home/user/.copilot/hooks/skim-rewrite.sh",
            "Copilot entries must use 'bash' field"
        );
        assert!(
            entry.get("command").is_none(),
            "Copilot entries must NOT use 'command' field"
        );
    }

    #[test]
    fn test_copilot_build_config_entry_no_matcher_field() {
        // `matcher` is not in the documented Copilot CLI hook schema.
        // An unknown field risks the whole skim.json being rejected — omit it.
        let entry = hook().build_config_entry("/path/skim-rewrite.sh");
        assert!(
            entry.get("matcher").is_none(),
            "build_config_entry must NOT include 'matcher' (undocumented field risks file rejection)"
        );
    }

    #[test]
    fn test_copilot_build_config_entry_uses_timeout_sec() {
        let entry = hook().build_config_entry("/path/skim-rewrite.sh");
        // Must use "timeoutSec", not "timeout"
        assert!(
            entry.get("timeoutSec").is_some(),
            "Copilot entries must use 'timeoutSec' field"
        );
        assert!(
            entry.get("timeout").is_none(),
            "Copilot entries must NOT use 'timeout' field"
        );
    }

    #[test]
    fn test_copilot_is_skim_entry_checks_bash_field() {
        // Positive: bash field contains skim-rewrite
        let skim_entry = serde_json::json!({
            "type": "command",
            "bash": "/home/user/.github/hooks/skim-rewrite.sh",
            "matcher": "bash",
            "timeoutSec": 5
        });
        assert!(hook().is_skim_entry(&skim_entry));

        // Negative: bash field points to something else
        let other_entry = serde_json::json!({
            "type": "command",
            "bash": "/home/user/hooks/other-hook.sh",
            "matcher": "bash"
        });
        assert!(!hook().is_skim_entry(&other_entry));

        // Negative: has "command" field with skim-rewrite (wrong format for Copilot)
        let wrong_format = serde_json::json!({
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": "/path/skim-rewrite.sh"}]
        });
        assert!(!hook().is_skim_entry(&wrong_format));
    }

    #[test]
    fn test_copilot_upsert_hook_idempotent() {
        let mut config = serde_json::json!({});
        hook()
            .upsert_hook(&mut config, "/path/skim-rewrite.sh")
            .unwrap();
        hook()
            .upsert_hook(&mut config, "/path/skim-rewrite.sh")
            .unwrap();

        let entries = config["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(
            entries.len(),
            1,
            "running upsert twice should produce exactly one entry, not a duplicate"
        );
    }

    #[test]
    fn test_copilot_upsert_hook_uses_pre_tool_use_lowercase() {
        let mut config = serde_json::json!({});
        hook()
            .upsert_hook(&mut config, "/path/skim-rewrite.sh")
            .unwrap();

        assert!(
            config["hooks"]["preToolUse"].is_array(),
            "should use lowercase preToolUse"
        );
        assert!(
            config["hooks"].get("PreToolUse").is_none(),
            "should NOT use uppercase PreToolUse"
        );
    }

    #[test]
    fn test_copilot_detect_hook_reads_bash_field() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = serde_json::json!({
            "hooks": {
                "preToolUse": [{
                    "type": "command",
                    "bash": "/home/.github/hooks/skim-rewrite.sh",
                    "matcher": "bash",
                    "timeoutSec": 5
                }]
            }
        });
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();
        // Legacy settings.json detection still works (backward compat; migration support).
        assert!(hook().detect_hook(dir.path()));
    }

    // ========================================================================
    // hook_config_dir seam — global install redirects to ~/.copilot;
    // project scope and env override suppress the redirect
    // ========================================================================

    #[test]
    fn test_copilot_hook_config_dir_passthrough_when_override() {
        let dir = tempfile::TempDir::new().unwrap();
        // has_override=true → use resolved_config_dir verbatim (sandbox safety)
        let result = hook().hook_config_dir(dir.path(), false, true);
        assert_eq!(
            result,
            dir.path(),
            "override must suppress ~/.copilot redirect"
        );
    }

    #[test]
    fn test_copilot_hook_config_dir_passthrough_when_project_scope() {
        let dir = tempfile::TempDir::new().unwrap();
        // project_scope=true → keep in project tree
        let result = hook().hook_config_dir(dir.path(), true, false);
        assert_eq!(
            result,
            dir.path(),
            "project scope must suppress ~/.copilot redirect"
        );
    }

    #[test]
    fn test_copilot_hook_config_dir_redirects_globally_without_override() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = hook().hook_config_dir(dir.path(), false, false);
        // Must NOT equal the passed-in dir (which represents ~/.github)
        // and must end with ".copilot".
        assert_ne!(
            result,
            dir.path(),
            "global default must redirect away from ~/.github"
        );
        assert!(
            result.ends_with(".copilot"),
            "global default must point to ~/.copilot, got: {}",
            result.display()
        );
    }

    #[test]
    fn test_copilot_uses_dedicated_hook_file() {
        assert!(
            hook().uses_dedicated_hook_file(),
            "Copilot must use dedicated hook file"
        );
    }

    // ========================================================================
    // write_copilot_skim_json and detect_hook_registration —
    // envelope shape, idempotent detect, and remove lifecycle
    // ========================================================================

    /// install writes hooks/skim.json with version=1 envelope and a single
    /// preToolUse command entry referencing the script path.
    #[test]
    fn test_copilot_install_writes_skim_json_with_exact_envelope() {
        let dir = tempfile::TempDir::new().unwrap();
        let script_path = dir.path().join("hooks/skim-rewrite.sh");

        write_copilot_skim_json(dir.path(), script_path.to_str().unwrap(), &CopilotCliHook)
            .unwrap();

        let skim_json = dir.path().join("hooks/skim.json");
        assert!(skim_json.exists(), "hooks/skim.json must be created");

        let content = std::fs::read_to_string(&skim_json).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Exact envelope shape
        assert_eq!(json["version"], 1, "version must be 1");
        assert!(
            json["hooks"]["preToolUse"].is_array(),
            "preToolUse must be array"
        );

        let entries = json["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 1, "exactly one preToolUse entry");

        let entry = &entries[0];
        // Copilot entry uses "bash" field, "type": "command", "timeoutSec"
        assert_eq!(entry["type"], "command", "entry must have type=command");
        assert_eq!(
            entry["bash"],
            script_path.to_str().unwrap(),
            "entry bash field must reference the script path"
        );
        assert!(
            entry.get("timeoutSec").is_some(),
            "entry must have timeoutSec"
        );
        assert!(
            entry.get("command").is_none(),
            "entry must NOT use command field (Copilot uses bash)"
        );
        // matcher is not in the documented schema — absence guards against file rejection.
        assert!(
            entry.get("matcher").is_none(),
            "entry must NOT include matcher (undocumented field risks file rejection)"
        );
    }

    /// install_hook_registration returns true and creates skim.json.
    #[test]
    fn test_copilot_install_hook_registration_returns_true() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = hook()
            .install_hook_registration(dir.path(), "/tmp/skim-rewrite.sh")
            .unwrap();
        assert!(
            result,
            "install_hook_registration must return true for Copilot"
        );
        assert!(dir.path().join("hooks/skim.json").exists());
    }

    /// detect_hook_registration returns true after install.
    #[test]
    fn test_copilot_detect_hook_registration_after_install() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("hooks/skim-rewrite.sh");
        write_copilot_skim_json(dir.path(), script.to_str().unwrap(), &CopilotCliHook).unwrap();
        assert!(
            hook().detect_hook_registration(dir.path()),
            "must detect after install"
        );
    }

    /// detect_hook_registration returns false when hooks/skim.json is absent.
    #[test]
    fn test_copilot_detect_hook_registration_absent() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(
            !hook().detect_hook_registration(dir.path()),
            "must be false when skim.json absent"
        );
    }

    // ========================================================================
    // scan_foreign_hooks — foreign hook detection, entry cap, and size guard
    // ========================================================================

    /// copilot_skim_json_has_entry returns false for an oversized skim.json.
    #[test]
    fn test_copilot_skim_json_has_entry_oversized_returns_false() {
        use crate::cmd::init::MAX_SETTINGS_SIZE;
        use std::io::{Seek, SeekFrom, Write};
        let dir = tempfile::TempDir::new().unwrap();
        let hooks_dir = dir.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let skim_json = hooks_dir.join("skim.json");
        // Sparse write: seek past the cap so metadata reports an oversized file
        // without allocating 10 MiB on disk.
        let mut f = std::fs::File::create(&skim_json).unwrap();
        f.seek(SeekFrom::Start(MAX_SETTINGS_SIZE + 1)).unwrap();
        f.write_all(b"\n").unwrap();
        assert!(
            !copilot_skim_json_has_entry(&skim_json),
            "oversized skim.json must return false (size guard prevents unbounded read)"
        );
    }

    /// scan_copilot_foreign_hooks silently skips oversized foreign hook files.
    #[test]
    fn test_scan_copilot_foreign_hooks_skips_oversized() {
        use crate::cmd::init::MAX_SETTINGS_SIZE;
        use std::io::{Seek, SeekFrom, Write};
        let dir = tempfile::TempDir::new().unwrap();
        let hooks_dir = dir.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        // An oversized foreign hook file — must be skipped, not read.
        let foreign = hooks_dir.join("other-tool.json");
        let mut f = std::fs::File::create(&foreign).unwrap();
        f.seek(SeekFrom::Start(MAX_SETTINGS_SIZE + 1)).unwrap();
        f.write_all(b"\n").unwrap();
        let others = scan_copilot_foreign_hooks(dir.path());
        assert!(
            others.is_empty(),
            "oversized foreign hook file must be skipped (size guard)"
        );
    }

    /// scan_foreign_hooks: foreign *.json in hooks dir is detected.
    #[test]
    fn test_copilot_scan_foreign_hooks_detects_foreign_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let hooks_dir = dir.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();

        // Our own skim.json — must NOT be returned
        let skim_json = serde_json::json!({
            "version": 1,
            "hooks": {
                "preToolUse": [{
                    "type": "command",
                    "bash": "/home/.copilot/hooks/skim-rewrite.sh",
                    "matcher": "bash"
                }]
            }
        });
        std::fs::write(
            hooks_dir.join("skim.json"),
            serde_json::to_string_pretty(&skim_json).unwrap(),
        )
        .unwrap();

        // A foreign hook — must be returned
        let foreign_json = serde_json::json!({
            "version": 1,
            "hooks": {
                "preToolUse": [{
                    "type": "command",
                    "bash": "/usr/bin/other-copilot-hook",
                    "matcher": "bash"
                }]
            }
        });
        std::fs::write(
            hooks_dir.join("other-tool.json"),
            serde_json::to_string_pretty(&foreign_json).unwrap(),
        )
        .unwrap();

        let others = hook().scan_foreign_hooks(dir.path());
        assert_eq!(others, vec!["/usr/bin/other-copilot-hook"]);
    }

    /// scan_foreign_hooks: only skim.json in hooks dir → empty result.
    #[test]
    fn test_copilot_scan_foreign_hooks_empty_when_only_skim_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let hooks_dir = dir.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let script = hooks_dir.join("skim-rewrite.sh");
        write_copilot_skim_json(dir.path(), script.to_str().unwrap(), &CopilotCliHook).unwrap();

        let others = hook().scan_foreign_hooks(dir.path());
        assert!(others.is_empty(), "only skim.json must return empty vec");
    }

    /// scan_foreign_hooks: empty hooks dir → empty result.
    #[test]
    fn test_copilot_scan_foreign_hooks_empty_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let others = hook().scan_foreign_hooks(dir.path());
        assert!(others.is_empty());
    }

    /// remove_hook_registration deletes skim.json.
    #[test]
    fn test_copilot_remove_hook_registration_deletes_skim_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let script = dir.path().join("hooks/skim-rewrite.sh");
        write_copilot_skim_json(dir.path(), script.to_str().unwrap(), &CopilotCliHook).unwrap();
        let skim_json = dir.path().join("hooks/skim.json");
        assert!(skim_json.exists());

        let removed = hook().remove_hook_registration(dir.path()).unwrap();
        assert!(removed, "must return true when skim.json was present");
        assert!(!skim_json.exists(), "skim.json must be removed");
    }

    /// remove_hook_registration is a no-op when skim.json is absent.
    #[test]
    fn test_copilot_remove_hook_registration_noop_when_absent() {
        let dir = tempfile::TempDir::new().unwrap();
        let removed = hook().remove_hook_registration(dir.path()).unwrap();
        assert!(!removed, "must return false when skim.json was absent");
    }

    // ========================================================================
    // Non-regression: non-Copilot agents use default hook_config_dir passthrough
    // ========================================================================

    #[test]
    fn test_non_copilot_hook_config_dir_is_passthrough() {
        use crate::cmd::hooks::claude::ClaudeCodeHook;
        let dir = tempfile::TempDir::new().unwrap();
        let result = ClaudeCodeHook.hook_config_dir(dir.path(), false, false);
        assert_eq!(
            result,
            dir.path(),
            "ClaudeCodeHook must pass through hook_config_dir unchanged"
        );
    }
}
