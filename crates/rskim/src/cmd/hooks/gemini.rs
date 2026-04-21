//! Gemini CLI hook protocol implementation.
//!
//! Implements the `HookProtocol` trait for Gemini CLI's BeforeTool event.
//!
//! Gemini CLI's hook protocol is nearly identical to Claude Code's:
//! - Config: `.gemini/settings.json`
//! - Event: `BeforeTool`
//! - Input: `{ "tool_name": "shell", "tool_input": { "command": "cargo test" } }`
//! - Response: `{ "decision": "allow", "tool_input": { "command": "skim test cargo" } }`
//!
//! SECURITY: Zero stderr in hook mode (GRANITE #361 lesson).
//! SECURITY: Absolute binary path in generated scripts (GRANITE #685 lesson).

use super::{HookInput, HookProtocol, HookSupport};
use crate::cmd::session::AgentKind;

/// Gemini CLI hook implementation.
pub(crate) struct GeminiCliHook;

impl HookProtocol for GeminiCliHook {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::GeminiCli
    }

    fn hook_support(&self) -> HookSupport {
        HookSupport::RealHook
    }

    fn parse_input(&self, json: &serde_json::Value) -> Option<HookInput> {
        super::parse_tool_input_command(json)
    }

    fn format_response(&self, rewritten_command: &str) -> serde_json::Value {
        // SECURITY: "decision": "allow" is REQUIRED by Gemini CLI's hook protocol.
        // This is NOT the same as Claude Code's permissionDecision -- Gemini CLI's
        // BeforeTool response schema requires an explicit decision field.
        serde_json::json!({
            "decision": "allow",
            "tool_input": {
                "command": rewritten_command
            }
        })
    }

    fn generate_script(&self, binary_path: &str, version: &str) -> String {
        super::generate_hook_script(binary_path, version, "gemini")
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::hooks::{InstallOpts, UninstallOpts};

    fn hook() -> GeminiCliHook {
        GeminiCliHook
    }

    #[test]
    fn test_gemini_hook_is_real() {
        assert_eq!(hook().hook_support(), HookSupport::RealHook);
        assert_eq!(hook().agent_kind(), AgentKind::GeminiCli);
    }

    #[test]
    fn test_gemini_parse_input() {
        let json = serde_json::json!({
            "tool_name": "shell",
            "tool_input": {
                "command": "cargo test"
            }
        });
        let input = hook().parse_input(&json).expect("should parse input");
        assert_eq!(input.command, "cargo test");
    }

    #[test]
    fn test_gemini_format_response() {
        let response = hook().format_response("skim test cargo");
        assert_eq!(response["decision"], "allow");
        assert_eq!(response["tool_input"]["command"], "skim test cargo");
    }

    #[test]
    fn test_gemini_format_response_has_required_decision_field() {
        // SECURITY: Gemini CLI's BeforeTool protocol REQUIRES "decision": "allow"
        // in every response. This is NOT Claude Code's permissionDecision -- it is
        // a distinct, required field in Gemini CLI's schema.
        let response = hook().format_response("skim test cargo");
        assert_eq!(
            response.get("decision").and_then(|v| v.as_str()),
            Some("allow"),
            "Gemini CLI protocol requires 'decision' field set to 'allow'"
        );
    }

    #[test]
    fn test_gemini_format_response_no_permission_decision() {
        // Gemini must not emit Claude Code's permissionDecision field
        let response = hook().format_response("skim test cargo");
        assert!(
            response.get("permissionDecision").is_none(),
            "Gemini response must not contain Claude Code's permissionDecision"
        );
    }

    #[test]
    fn test_gemini_generate_script_uses_bare_command() {
        let script = hook().generate_script("/usr/local/bin/skim", "1.2.3");
        assert!(
            script.contains("exec skim rewrite --hook"),
            "script must use bare skim command, got: {script}"
        );
        assert!(
            !script.contains("\"/usr/local/bin/skim\""),
            "script must NOT contain hardcoded binary path, got: {script}"
        );
    }

    #[test]
    fn test_gemini_generate_script_has_version() {
        let script = hook().generate_script("/usr/local/bin/skim", "0.9.0");
        assert!(
            script.contains("SKIM_HOOK_VERSION=\"0.9.0\""),
            "script must export SKIM_HOOK_VERSION, got: {script}"
        );
        assert!(
            script.contains("# skim-hook v0.9.0"),
            "script must contain version comment, got: {script}"
        );
    }

    #[test]
    fn test_gemini_parse_input_missing_command() {
        // Missing tool_input entirely
        let json = serde_json::json!({"tool_name": "shell"});
        assert!(hook().parse_input(&json).is_none());

        // tool_input present but no command
        let json = serde_json::json!({
            "tool_name": "shell",
            "tool_input": {}
        });
        assert!(hook().parse_input(&json).is_none());

        // command is not a string
        let json = serde_json::json!({
            "tool_name": "shell",
            "tool_input": {
                "command": 42
            }
        });
        assert!(hook().parse_input(&json).is_none());
    }

    #[test]
    fn test_gemini_generate_script_has_agent_flag() {
        let script = hook().generate_script("/usr/local/bin/skim", "1.0.0");
        assert!(
            script.contains("--agent gemini"),
            "script must pass --agent gemini flag, got: {script}"
        );
    }

    #[test]
    fn test_gemini_generate_script_has_shebang() {
        let script = hook().generate_script("/usr/local/bin/skim", "1.0.0");
        assert!(
            script.starts_with("#!/usr/bin/env bash"),
            "script must start with bash shebang, got: {script}"
        );
    }

    #[test]
    fn test_gemini_install_default() {
        let opts = InstallOpts {
            binary_path: "/usr/local/bin/skim".into(),
            version: "1.0.0".into(),
            config_dir: "/tmp/.gemini".into(),
            project_scope: false,
            dry_run: false,
        };
        let result = hook().install(&opts).unwrap();
        assert!(result.script_path.is_none());
        assert!(!result.config_patched);
    }

    #[test]
    fn test_gemini_uninstall_default() {
        let opts = UninstallOpts {
            config_dir: "/tmp/.gemini".into(),
            force: false,
        };
        assert!(hook().uninstall(&opts).is_ok());
    }
}
