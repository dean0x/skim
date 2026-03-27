//! Claude Code hook protocol implementation.
//!
//! Claude Code uses PreToolUse hooks. The hook reads JSON from stdin,
//! extracts tool_input.command, rewrites if matched, and emits
//! hookSpecificOutput with updatedInput. Never sets permissionDecision.

use super::{HookInput, HookProtocol, HookSupport};
use crate::cmd::session::AgentKind;

/// Claude Code hook implementation (PreToolUse hooks).
pub(crate) struct ClaudeCodeHook;

impl HookProtocol for ClaudeCodeHook {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn hook_support(&self) -> HookSupport {
        HookSupport::RealHook
    }

    fn parse_input(&self, json: &serde_json::Value) -> Option<HookInput> {
        super::parse_tool_input_command(json)
    }

    fn format_response(&self, rewritten_command: &str) -> serde_json::Value {
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "updatedInput": {
                    "command": rewritten_command
                }
            }
        })
    }

    fn generate_script(&self, binary_path: &str, version: &str) -> String {
        super::generate_hook_script(binary_path, version, "claude-code")
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::hooks::{InstallOpts, UninstallOpts};

    fn hook() -> ClaudeCodeHook {
        ClaudeCodeHook
    }

    #[test]
    fn test_claude_agent_kind() {
        assert_eq!(hook().agent_kind(), AgentKind::ClaudeCode);
    }

    #[test]
    fn test_claude_hook_support() {
        assert_eq!(hook().hook_support(), HookSupport::RealHook);
    }

    #[test]
    fn test_claude_parse_input_valid() {
        let json = serde_json::json!({
            "tool_input": {
                "command": "cargo test --nocapture"
            }
        });
        let result = hook().parse_input(&json);
        assert!(result.is_some());
        assert_eq!(result.unwrap().command, "cargo test --nocapture");
    }

    #[test]
    fn test_claude_parse_input_missing_tool_input() {
        let json = serde_json::json!({});
        assert!(hook().parse_input(&json).is_none());
    }

    #[test]
    fn test_claude_parse_input_missing_command() {
        let json = serde_json::json!({
            "tool_input": {
                "file_path": "/tmp/test.rs"
            }
        });
        assert!(hook().parse_input(&json).is_none());
    }

    #[test]
    fn test_claude_format_response() {
        let response = hook().format_response("skim test cargo");
        let output = response.get("hookSpecificOutput").unwrap();
        assert_eq!(output["hookEventName"], "PreToolUse");
        assert_eq!(output["updatedInput"]["command"], "skim test cargo");
    }

    #[test]
    fn test_claude_format_response_no_permission_decision() {
        let response = hook().format_response("skim test cargo");
        // SECURITY: Must never set permissionDecision
        assert!(response.get("permissionDecision").is_none());
    }

    #[test]
    fn test_claude_generate_script() {
        let script = hook().generate_script("/usr/local/bin/skim", "1.0.0");
        assert!(script.contains("#!/usr/bin/env bash"));
        assert!(script.contains("# skim-hook v1.0.0"));
        assert!(script.contains("SKIM_HOOK_VERSION=\"1.0.0\""));
        assert!(script.contains("exec \"/usr/local/bin/skim\" rewrite --hook --agent claude-code"));
    }

    #[test]
    fn test_claude_generate_script_init_comment() {
        let script = hook().generate_script("/usr/local/bin/skim", "1.0.0");
        assert!(script.contains("skim init --agent claude-code"));
    }

    #[test]
    fn test_claude_install_default() {
        let opts = InstallOpts {
            binary_path: "/usr/local/bin/skim".into(),
            version: "1.0.0".into(),
            config_dir: "/tmp/.claude".into(),
            project_scope: false,
            dry_run: false,
        };
        let result = hook().install(&opts).unwrap();
        assert!(result.script_path.is_none());
        assert!(!result.config_patched);
    }

    #[test]
    fn test_claude_uninstall_default() {
        let opts = UninstallOpts {
            config_dir: "/tmp/.claude".into(),
            force: false,
        };
        assert!(hook().uninstall(&opts).is_ok());
    }
}
