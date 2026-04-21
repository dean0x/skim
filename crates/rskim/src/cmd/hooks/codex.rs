//! Codex CLI hook protocol implementation (awareness-only).
//!
//! Codex CLI has no PreToolUse hook equivalent. This implementation
//! returns awareness-only support with no-op methods for all hook operations.

use super::{HookProtocol, HookSupport};
use crate::cmd::session::AgentKind;

/// Codex CLI awareness-only hook (no PreToolUse equivalent).
pub(crate) struct CodexCliHook;

impl HookProtocol for CodexCliHook {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::CodexCli
    }

    fn hook_support(&self) -> HookSupport {
        HookSupport::AwarenessOnly
    }

    fn parse_input(&self, _json: &serde_json::Value) -> Option<super::HookInput> {
        None
    }

    fn format_response(&self, _rewritten_command: &str) -> serde_json::Value {
        serde_json::Value::Null
    }

    fn generate_script(&self, _version: &str) -> String {
        String::new()
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::hooks::{InstallOpts, UninstallOpts};

    fn hook() -> CodexCliHook {
        CodexCliHook
    }

    #[test]
    fn test_codex_hook_support_is_awareness() {
        assert_eq!(hook().hook_support(), HookSupport::AwarenessOnly);
    }

    #[test]
    fn test_codex_parse_input_returns_none() {
        let json = serde_json::json!({
            "tool_input": {
                "command": "cargo test"
            }
        });
        assert!(hook().parse_input(&json).is_none());
    }

    #[test]
    fn test_codex_format_response_returns_null() {
        let response = hook().format_response("skim test cargo");
        assert!(response.is_null());
    }

    #[test]
    fn test_codex_agent_kind() {
        assert_eq!(hook().agent_kind(), AgentKind::CodexCli);
    }

    #[test]
    fn test_codex_generate_script_empty() {
        let script = hook().generate_script("1.0.0");
        assert!(script.is_empty());
    }

    #[test]
    fn test_codex_install_noop() {
        let opts = InstallOpts {
            binary_path: "/usr/local/bin/skim".into(),
            version: "1.0.0".into(),
            config_dir: "/tmp/.codex".into(),
            project_scope: false,
            dry_run: false,
        };
        let result = hook().install(&opts).unwrap();
        assert!(result.script_path.is_none());
        assert!(!result.config_patched);
    }

    #[test]
    fn test_codex_uninstall_noop() {
        let opts = UninstallOpts {
            config_dir: "/tmp/.codex".into(),
            force: false,
        };
        assert!(hook().uninstall(&opts).is_ok());
    }
}
