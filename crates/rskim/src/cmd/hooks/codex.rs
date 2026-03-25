//! Codex CLI hook protocol implementation (awareness-only).
//!
//! Codex CLI has no PreToolUse hook equivalent. This implementation
//! returns awareness-only support with no-op methods for all hook operations.

use super::{HookInput, HookProtocol, HookSupport, InstallOpts, InstallResult, UninstallOpts};
use crate::cmd::session::AgentKind;

pub(crate) struct CodexCliHook;

impl HookProtocol for CodexCliHook {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::CodexCli
    }

    fn hook_support(&self) -> HookSupport {
        HookSupport::AwarenessOnly
    }

    fn parse_input(&self, _json: &serde_json::Value) -> Option<HookInput> {
        None // Not applicable -- awareness only
    }

    fn format_response(&self, _rewritten_command: &str) -> serde_json::Value {
        serde_json::Value::Null // Not applicable -- awareness only
    }

    fn generate_script(&self, _binary_path: &str, _version: &str) -> String {
        String::new() // Not applicable -- awareness only
    }

    fn install(&self, _opts: &InstallOpts) -> anyhow::Result<InstallResult> {
        // No-op: awareness-only agent has no hook to install
        Ok(InstallResult {
            script_path: None,
            config_patched: false,
        })
    }

    fn uninstall(&self, _opts: &UninstallOpts) -> anyhow::Result<()> {
        // No-op: awareness-only agent has no hook to uninstall
        Ok(())
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

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
        let script = hook().generate_script("/usr/local/bin/skim", "1.0.0");
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
