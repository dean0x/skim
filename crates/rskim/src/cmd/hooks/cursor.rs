//! Cursor hook protocol implementation.
//!
//! Cursor uses `beforeShellExecution` hooks via `.cursor/hooks.json`.
//! The hook reads JSON with command at top level (not nested under
//! tool_input like Claude Code), rewrites if matched, and responds
//! with `{ "permission": "allow", "updated_input": { "command": ... } }`.

use super::{HookInput, HookProtocol, HookSupport};
use crate::cmd::session::AgentKind;

/// Cursor hook implementation (`beforeShellExecution` via `.cursor/hooks.json`).
pub(crate) struct CursorHook;

impl HookProtocol for CursorHook {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Cursor
    }

    fn hook_support(&self) -> HookSupport {
        HookSupport::RealHook
    }

    fn parse_input(&self, json: &serde_json::Value) -> Option<HookInput> {
        // Cursor puts command at top level, not nested under tool_input
        let command = json.get("command").and_then(|c| c.as_str())?.to_string();
        // AD-HK-1: Extract session_id from top-level JSON field if present.
        // F8: Filter empty strings at the parse boundary so callers never see Some("").
        let session_id = json
            .get("session_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        Some(HookInput {
            command,
            session_id,
        })
    }

    fn format_response(&self, rewritten_command: &str) -> serde_json::Value {
        // SECURITY: "permission": "allow" is REQUIRED by Cursor's hook protocol.
        // This is NOT the same as Claude Code's permissionDecision -- Cursor's
        // protocol requires an explicit permission field in every hook response.
        serde_json::json!({
            "permission": "allow",
            "updated_input": {
                "command": rewritten_command
            }
        })
    }

    fn generate_script(&self, version: &str) -> String {
        super::generate_hook_script(version, "cursor")
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::hooks::{InstallOpts, UninstallOpts};

    fn hook() -> CursorHook {
        CursorHook
    }

    #[test]
    fn test_cursor_hook_is_real() {
        assert_eq!(hook().hook_support(), HookSupport::RealHook);
    }

    #[test]
    fn test_cursor_agent_kind() {
        assert_eq!(hook().agent_kind(), AgentKind::Cursor);
    }

    #[test]
    fn test_cursor_parse_input() {
        let json = serde_json::json!({
            "command": "cargo test --nocapture"
        });
        let result = hook().parse_input(&json);
        assert!(result.is_some());
        assert_eq!(result.unwrap().command, "cargo test --nocapture");
    }

    #[test]
    fn test_cursor_parse_input_missing_command() {
        let json = serde_json::json!({});
        assert!(hook().parse_input(&json).is_none());
    }

    #[test]
    fn test_cursor_parse_input_non_string_command() {
        let json = serde_json::json!({
            "command": 42
        });
        assert!(hook().parse_input(&json).is_none());
    }

    #[test]
    fn test_cursor_format_response() {
        let response = hook().format_response("skim cargo test");
        assert_eq!(response["permission"], "allow");
        assert_eq!(response["updated_input"]["command"], "skim cargo test");
    }

    #[test]
    fn test_cursor_format_response_has_required_permission_field() {
        // SECURITY: Cursor's hook protocol REQUIRES "permission": "allow" in
        // every response. This is NOT Claude Code's permissionDecision -- it is
        // a distinct, required field in Cursor's schema.
        let response = hook().format_response("skim cargo test");
        assert_eq!(
            response.get("permission").and_then(|v| v.as_str()),
            Some("allow"),
            "Cursor protocol requires 'permission' field set to 'allow'"
        );
    }

    #[test]
    fn test_cursor_format_response_no_hook_specific_output() {
        // Cursor uses permission/updated_input, not hookSpecificOutput
        let response = hook().format_response("skim cargo test");
        assert!(response.get("hookSpecificOutput").is_none());
    }

    #[test]
    fn test_cursor_format_response_no_permission_decision() {
        // Cursor must not emit Claude Code's permissionDecision field
        let response = hook().format_response("skim cargo test");
        assert!(
            response.get("permissionDecision").is_none(),
            "Cursor response must not contain Claude Code's permissionDecision"
        );
    }

    #[test]
    fn test_cursor_generate_script_bare_command() {
        let script = hook().generate_script("1.2.0");
        assert!(script.contains("#!/usr/bin/env bash"));
        assert!(script.contains("# skim-hook v1.2.0"));
        assert!(script.contains("SKIM_HOOK_VERSION=\"1.2.0\""));
        assert!(script.contains("exec skim rewrite --hook --agent cursor"));
    }

    #[test]
    fn test_cursor_generate_script_zero_stderr() {
        let script = hook().generate_script("1.0.0");
        // No eprintln or echo to stderr in generated script
        assert!(!script.contains(">&2"));
        assert!(!script.contains("echo"));
        assert!(!script.contains("eprintln"));
    }

    #[test]
    fn test_cursor_generate_script_init_comment() {
        let script = hook().generate_script("1.0.0");
        assert!(script.contains("skim init --agent cursor"));
    }

    #[test]
    fn test_cursor_install_default() {
        let opts = InstallOpts {
            version: "1.0.0".into(),
            config_dir: "/tmp/.cursor".into(),
            project_scope: false,
            dry_run: false,
        };
        let result = hook().install(&opts).unwrap();
        assert!(result.script_path.is_none());
        assert!(!result.config_patched);
    }

    #[test]
    fn test_cursor_uninstall_default() {
        let opts = UninstallOpts {
            config_dir: "/tmp/.cursor".into(),
            force: false,
        };
        assert!(hook().uninstall(&opts).is_ok());
    }

    // ========================================================================
    // B8: AD-HK-1 — Cursor session_id extraction
    // ========================================================================

    /// AD-HK-1: Cursor parse_input extracts session_id from top-level JSON.
    #[test]
    fn test_cursor_parse_input_extracts_session_id() {
        let json = serde_json::json!({
            "session_id": "cursor-session-xyz",
            "command": "cargo test"
        });
        let result = hook().parse_input(&json).unwrap();
        assert_eq!(result.command, "cargo test");
        assert_eq!(result.session_id, Some("cursor-session-xyz".to_string()));
    }

    /// F8: empty session_id is treated as None at the parse boundary.
    #[test]
    fn test_cursor_parse_input_empty_session_id_is_none() {
        let json = serde_json::json!({
            "session_id": "",
            "command": "cargo test"
        });
        let result = hook().parse_input(&json).unwrap();
        assert_eq!(result.command, "cargo test");
        assert!(
            result.session_id.is_none(),
            "empty session_id should yield None at parse boundary"
        );
    }

    /// AD-HK-1: session_id is None when absent from Cursor hook JSON.
    #[test]
    fn test_cursor_parse_input_no_session_id() {
        let json = serde_json::json!({
            "command": "cargo build --release"
        });
        let result = hook().parse_input(&json).unwrap();
        assert_eq!(result.command, "cargo build --release");
        assert!(
            result.session_id.is_none(),
            "session_id should be None when absent"
        );
    }
}
