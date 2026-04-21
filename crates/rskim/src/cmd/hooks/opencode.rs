//! OpenCode hook protocol implementation.
//!
//! OpenCode uses a TypeScript plugin model -- there is no shell hook equivalent.
//! This implementation provides awareness-only support: it registers the agent
//! as recognized but does not intercept tool calls.

use super::{HookProtocol, HookSupport};
use crate::cmd::session::AgentKind;

/// OpenCode awareness-only hook.
///
/// OpenCode has no shell hook mechanism, so all methods are no-ops.
/// The provider exists so that `skim init --agent opencode` gives
/// a clear "awareness-only" message instead of "unknown agent".
pub(crate) struct OpenCodeHook;

impl HookProtocol for OpenCodeHook {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::OpenCode
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

    fn hook() -> OpenCodeHook {
        OpenCodeHook
    }

    #[test]
    fn test_opencode_hook_support_is_awareness() {
        assert_eq!(hook().hook_support(), HookSupport::AwarenessOnly);
    }

    #[test]
    fn test_opencode_parse_input_returns_none() {
        let json = serde_json::json!({
            "tool_input": {
                "command": "cargo test"
            }
        });
        assert!(hook().parse_input(&json).is_none());
    }

    #[test]
    fn test_opencode_format_response_returns_null() {
        let response = hook().format_response("skim test cargo");
        assert_eq!(response, serde_json::Value::Null);
    }

    #[test]
    fn test_opencode_agent_kind() {
        assert_eq!(hook().agent_kind(), AgentKind::OpenCode);
    }

    #[test]
    fn test_opencode_generate_script_empty() {
        let script = hook().generate_script("1.0.0");
        assert!(script.is_empty());
    }

    #[test]
    fn test_opencode_install_noop() {
        let opts = InstallOpts {
            version: "1.0.0".into(),
            config_dir: "/tmp/.opencode".into(),
            project_scope: false,
            dry_run: false,
        };
        let result = hook().install(&opts).unwrap();
        assert!(result.script_path.is_none());
        assert!(!result.config_patched);
    }

    #[test]
    fn test_opencode_uninstall_noop() {
        let opts = UninstallOpts {
            config_dir: "/tmp/.opencode".into(),
            force: false,
        };
        assert!(hook().uninstall(&opts).is_ok());
    }
}
