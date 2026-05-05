//! Copilot CLI hook protocol implementation.
//!
//! Copilot CLI uses preToolUse hooks. The hook reads JSON from stdin,
//! extracts tool_input.command, rewrites if matched, and emits a
//! deny-with-suggestion response.
//!
//! ARCHITECTURE NOTE: Copilot's `allow` + `updatedInput` is currently broken.
//! Only `deny` works reliably. We use deny-with-suggestion: the deny reason
//! contains the optimized command for the user to accept manually.
//!
//! UPGRADE PATH: When Copilot ships working `allow` + `updatedInput`,
//! change `format_response` only (one-file change).

use super::{HookInput, HookProtocol, HookSupport};
use crate::cmd::session::AgentKind;

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
        // Deny-with-suggestion: Copilot's `allow` + `updatedInput` is broken.
        // When `allow` ships, change this to:
        //   { "permissionDecision": "allow", "updatedInput": { "command": rewritten_command } }
        serde_json::json!({
            "permissionDecision": "deny",
            "reason": format!("Use optimized command: {}", rewritten_command)
        })
    }

    fn generate_script(&self, version: &str) -> String {
        super::generate_hook_script(version, "copilot")
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
    /// Format: `{ "type": "command", "bash": "<path>", "matcher": "bash", "timeoutSec": 5 }`
    fn build_config_entry(&self, hook_script_path: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "command",
            "bash": hook_script_path,
            "matcher": self.tool_matcher(),
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
    fn upsert_hook(&self, config: &mut serde_json::Value, hook_script_path: &str) -> anyhow::Result<()> {
        let obj = config
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("config root is not an object"))?;

        // Ensure version field is present
        obj.entry("version").or_insert(serde_json::json!(1));

        let hooks = obj
            .entry("hooks")
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("config 'hooks' is not an object"))?;

        let event_arr = hooks
            .entry(self.hook_event_key())
            .or_insert_with(|| serde_json::Value::Array(Vec::new()))
            .as_array_mut()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "config 'hooks.{}' is not an array",
                    self.hook_event_key()
                )
            })?;

        // Remove existing skim entries (idempotent upsert)
        event_arr.retain(|e| !self.is_skim_entry(e));

        // Append new entry
        event_arr.push(self.build_config_entry(hook_script_path));

        Ok(())
    }
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
    fn test_copilot_format_response_is_deny() {
        let response = hook().format_response("skim cargo test");
        assert_eq!(response["permissionDecision"], "deny");
    }

    #[test]
    fn test_copilot_format_response_includes_command_in_reason() {
        let response = hook().format_response("skim cargo test");
        let reason = response["reason"].as_str().unwrap();
        assert!(
            reason.contains("skim cargo test"),
            "reason should contain the rewritten command, got: {reason}"
        );
        assert!(
            reason.starts_with("Use optimized command:"),
            "reason should start with prefix, got: {reason}"
        );
    }

    #[test]
    fn test_copilot_format_response_no_allow() {
        let response = hook().format_response("skim cargo test");
        // Must be "deny", never "allow" (Copilot's allow is broken)
        assert_ne!(
            response["permissionDecision"].as_str().unwrap(),
            "allow",
            "permissionDecision must be 'deny' until Copilot fixes 'allow'"
        );
    }

    #[test]
    fn test_copilot_format_response_no_hook_specific_output() {
        let response = hook().format_response("skim cargo test");
        // Copilot uses deny-with-suggestion, not hookSpecificOutput
        assert!(
            response.get("hookSpecificOutput").is_none(),
            "copilot should not use hookSpecificOutput"
        );
    }

    #[test]
    fn test_copilot_generate_script_bare_command() {
        let script = hook().generate_script("2.0.0");
        assert!(script.contains("#!/usr/bin/env bash"));
        assert!(script.contains("# skim-hook v2.0.0"));
        assert!(script.contains("skim init --agent copilot"));
        assert!(script.contains("SKIM_HOOK_VERSION=\"2.0.0\""));
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
        let entry = hook().build_config_entry("/home/user/.github/hooks/skim-rewrite.sh");
        // Must use "bash" field, not "command"
        assert_eq!(
            entry["bash"],
            "/home/user/.github/hooks/skim-rewrite.sh",
            "Copilot entries must use 'bash' field"
        );
        assert!(
            entry.get("command").is_none(),
            "Copilot entries must NOT use 'command' field"
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
        hook().upsert_hook(&mut config, "/path/skim-rewrite.sh").unwrap();
        hook().upsert_hook(&mut config, "/path/skim-rewrite.sh").unwrap();

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
        hook().upsert_hook(&mut config, "/path/skim-rewrite.sh").unwrap();

        assert!(config["hooks"]["preToolUse"].is_array(), "should use lowercase preToolUse");
        assert!(config["hooks"].get("PreToolUse").is_none(), "should NOT use uppercase PreToolUse");
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
        ).unwrap();
        assert!(hook().detect_hook(dir.path()));
    }
}
