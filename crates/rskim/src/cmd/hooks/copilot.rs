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

    fn generate_script(&self, binary_path: &str, version: &str) -> String {
        super::generate_hook_script(binary_path, version, "copilot")
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
        let response = hook().format_response("skim test cargo");
        assert_eq!(response["permissionDecision"], "deny");
    }

    #[test]
    fn test_copilot_format_response_includes_command_in_reason() {
        let response = hook().format_response("skim test cargo");
        let reason = response["reason"].as_str().unwrap();
        assert!(
            reason.contains("skim test cargo"),
            "reason should contain the rewritten command, got: {reason}"
        );
        assert!(
            reason.starts_with("Use optimized command:"),
            "reason should start with prefix, got: {reason}"
        );
    }

    #[test]
    fn test_copilot_format_response_no_allow() {
        let response = hook().format_response("skim test cargo");
        // Must be "deny", never "allow" (Copilot's allow is broken)
        assert_ne!(
            response["permissionDecision"].as_str().unwrap(),
            "allow",
            "permissionDecision must be 'deny' until Copilot fixes 'allow'"
        );
    }

    #[test]
    fn test_copilot_format_response_no_hook_specific_output() {
        let response = hook().format_response("skim test cargo");
        // Copilot uses deny-with-suggestion, not hookSpecificOutput
        assert!(
            response.get("hookSpecificOutput").is_none(),
            "copilot should not use hookSpecificOutput"
        );
    }

    #[test]
    fn test_copilot_generate_script() {
        let script = hook().generate_script("/usr/local/bin/skim", "2.0.0");
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
            binary_path: "/usr/local/bin/skim".into(),
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
}
