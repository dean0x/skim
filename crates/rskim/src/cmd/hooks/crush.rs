//! Crush hook protocol implementation.
//!
//! Crush uses PreToolUse hooks with a Claude Code-compatible format.
//! Config: `.crush/crush.json`
//! Input: `{ "tool_input": { "command": "..." } }`
//! Response: `{ "decision": "allow", "updated_input": { "command": "..." } }`

use super::{HookInput, HookProtocol, HookSupport};
use crate::cmd::session::AgentKind;

/// Crush hook implementation (PreToolUse hooks, Claude Code-compatible format).
///
/// Crush uses the same PreToolUse hook protocol as Claude Code, but stores its
/// config in `.crush/crush.json` instead of `.claude/settings.json`.
pub(crate) struct CrushHook;

impl HookProtocol for CrushHook {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Crush
    }

    fn hook_support(&self) -> HookSupport {
        HookSupport::RealHook
    }

    fn parse_input(&self, json: &serde_json::Value) -> Option<HookInput> {
        super::parse_tool_input_command(json)
    }

    fn format_response(&self, rewritten_command: &str) -> serde_json::Value {
        serde_json::json!({
            "decision": "allow",
            "updated_input": { "command": rewritten_command }
        })
    }

    fn generate_script(&self, version: &str) -> String {
        super::generate_hook_script(version, "crush")
    }

    // -------------------------------------------------------------------------
    // Config lifecycle overrides — Crush uses crush.json, not settings.json
    // -------------------------------------------------------------------------

    /// Crush stores hook config in `crush.json` (not `settings.json`).
    fn config_filename(&self) -> &'static str {
        "crush.json"
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::hooks::{InstallOpts, UninstallOpts};

    fn hook() -> CrushHook {
        CrushHook
    }

    #[test]
    fn test_crush_agent_kind() {
        assert_eq!(hook().agent_kind(), AgentKind::Crush);
    }

    #[test]
    fn test_crush_hook_support_is_real() {
        assert_eq!(hook().hook_support(), HookSupport::RealHook);
    }

    #[test]
    fn test_crush_parse_input_valid() {
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
    fn test_crush_parse_input_missing_tool_input() {
        let json = serde_json::json!({});
        assert!(hook().parse_input(&json).is_none());
    }

    #[test]
    fn test_crush_parse_input_missing_command() {
        let json = serde_json::json!({
            "tool_input": {
                "file_path": "/tmp/test.rs"
            }
        });
        assert!(hook().parse_input(&json).is_none());
    }

    #[test]
    fn test_crush_format_response() {
        let response = hook().format_response("skim cargo test");
        assert_eq!(response["decision"], "allow");
        assert_eq!(response["updated_input"]["command"], "skim cargo test");
    }

    #[test]
    fn test_crush_format_response_has_required_decision_field() {
        let response = hook().format_response("skim cargo test");
        assert_eq!(
            response.get("decision").and_then(|v| v.as_str()),
            Some("allow"),
            "Crush protocol requires 'decision' field set to 'allow'"
        );
    }

    #[test]
    fn test_crush_format_response_no_permission_decision() {
        // Crush must not emit Claude Code's permissionDecision field
        let response = hook().format_response("skim cargo test");
        assert!(response.get("permissionDecision").is_none());
    }

    #[test]
    fn test_crush_generate_script_bare_command() {
        let script = hook().generate_script("1.0.0");
        assert!(script.starts_with("#!/usr/bin/env bash\n"));
        assert!(script.contains("# skim-hook v1.0.0"));
        assert!(script.contains("SKIM_HOOK_VERSION=\"1.0.0\""));
        assert!(script.contains("exec skim rewrite --hook --agent crush"));
    }

    #[test]
    fn test_crush_generate_script_init_comment() {
        let script = hook().generate_script("1.0.0");
        assert!(script.contains("skim init --agent crush"));
    }

    #[test]
    fn test_crush_install_default() {
        let opts = InstallOpts {
            version: "1.0.0".into(),
            config_dir: "/tmp/.crush".into(),
            project_scope: false,
            dry_run: false,
        };
        let result = hook().install(&opts).unwrap();
        assert!(result.script_path.is_none());
        assert!(!result.config_patched);
    }

    #[test]
    fn test_crush_uninstall_default() {
        let opts = UninstallOpts {
            config_dir: "/tmp/.crush".into(),
            force: false,
        };
        assert!(hook().uninstall(&opts).is_ok());
    }

    // ========================================================================
    // AD-HK-1 — session_id extraction
    // ========================================================================

    #[test]
    fn test_crush_parse_input_extracts_session_id() {
        let json = serde_json::json!({
            "session_id": "crush-session-abc",
            "tool_input": {
                "command": "cargo build"
            }
        });
        let result = hook().parse_input(&json).unwrap();
        assert_eq!(result.command, "cargo build");
        assert_eq!(result.session_id, Some("crush-session-abc".to_string()));
    }

    #[test]
    fn test_crush_parse_input_no_session_id() {
        let json = serde_json::json!({
            "tool_input": {
                "command": "cargo test"
            }
        });
        let result = hook().parse_input(&json).unwrap();
        assert!(result.session_id.is_none());
    }

    #[test]
    fn test_crush_parse_input_empty_session_id_is_none() {
        let json = serde_json::json!({
            "session_id": "",
            "tool_input": {
                "command": "cargo build"
            }
        });
        let result = hook().parse_input(&json).unwrap();
        assert!(
            result.session_id.is_none(),
            "empty session_id should yield None at parse boundary"
        );
    }

    // ========================================================================
    // Phase 4: Config lifecycle override tests
    // ========================================================================

    #[test]
    fn test_crush_config_filename_is_crush_json() {
        assert_eq!(hook().config_filename(), "crush.json");
    }

    #[test]
    fn test_crush_hook_event_key_is_pre_tool_use() {
        // Crush uses the same event key as Claude Code
        assert_eq!(hook().hook_event_key(), "PreToolUse");
    }

    #[test]
    fn test_crush_upsert_hook_into_crush_json_format() {
        let mut config = serde_json::json!({});
        hook().upsert_hook(&mut config, "/path/skim-rewrite.sh").unwrap();

        let entries = config["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["matcher"], "Bash");
        assert_eq!(entries[0]["hooks"][0]["command"], "/path/skim-rewrite.sh");
    }

    #[test]
    fn test_crush_detect_hook_reads_crush_json() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": dir.path().join("hooks/skim-rewrite.sh").to_str().unwrap()}]
                }]
            }
        });
        // Crush uses crush.json, not settings.json
        std::fs::write(
            dir.path().join("crush.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        ).unwrap();
        assert!(hook().detect_hook(dir.path()));
    }

    #[test]
    fn test_crush_detect_hook_does_not_read_settings_json() {
        // crush.json is the config file — settings.json should be ignored
        let dir = tempfile::TempDir::new().unwrap();
        let config = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "/path/skim-rewrite.sh"}]
                }]
            }
        });
        // Write to settings.json only (wrong file for Crush)
        std::fs::write(
            dir.path().join("settings.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        ).unwrap();
        assert!(!hook().detect_hook(dir.path()), "crush should not detect hook from settings.json");
    }
}
