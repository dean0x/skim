//! Cursor hook protocol implementation.
//!
//! Cursor uses `preToolUse` hooks via `.cursor/hooks.json`.
//! The hook reads JSON with command at top level (not nested under
//! tool_input like Claude Code), rewrites if matched, and responds
//! with `{ "permission": "allow", "updated_input": { "command": ... } }`.
//!
//! ## Config format (hooks.json)
//!
//! Cursor's hooks.json wraps entries in `{ "version": 1, "hooks": { "preToolUse": [...] } }`.
//! Each entry is a flat object `{ "command": "<path>", "matcher": "Shell", "timeout": 5 }`.
//! This differs from the Claude Code format which nests a `"hooks"` array inside each entry.

use super::{HookInput, HookProtocol, HookSupport};
use crate::cmd::session::AgentKind;

/// Cursor hook implementation (`preToolUse` hooks via `.cursor/hooks.json`).
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

    // -------------------------------------------------------------------------
    // Config lifecycle overrides — Cursor uses hooks.json with flat entry format
    // -------------------------------------------------------------------------

    /// Cursor stores hook config in `hooks.json` (not `settings.json`).
    fn config_filename(&self) -> &'static str {
        "hooks.json"
    }

    /// Cursor uses `preToolUse` (lowercase) as the event key.
    fn hook_event_key(&self) -> &'static str {
        "preToolUse"
    }

    /// Cursor matches on `Shell` tool name.
    fn tool_matcher(&self) -> &'static str {
        "Shell"
    }

    /// Cursor's config entry is a flat object (no nested `"hooks"` array).
    ///
    /// Format: `{ "command": "<path>", "matcher": "Shell", "timeout": 5 }`
    fn build_config_entry(&self, hook_script_path: &str) -> serde_json::Value {
        serde_json::json!({
            "command": hook_script_path,
            "matcher": self.tool_matcher(),
            "timeout": self.hook_timeout()
        })
    }

    /// Cursor's `hooks.json` uses a top-level `"version"` field and wraps event
    /// arrays under `"hooks": { "preToolUse": [...] }`.
    ///
    /// This differs from the Claude Code default which stores hooks directly at
    /// the top level without a version wrapper.
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

        // Append new flat-format entry
        event_arr.push(self.build_config_entry(hook_script_path));

        Ok(())
    }

    /// Cursor entries are flat objects — skim entries have a top-level `"command"` field.
    fn is_skim_entry(&self, entry: &serde_json::Value) -> bool {
        entry
            .get("command")
            .and_then(|c| c.as_str())
            .is_some_and(|c| c.contains("skim-rewrite"))
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

    // ========================================================================
    // Phase 4: Config lifecycle override tests (AC-2)
    // ========================================================================

    #[test]
    fn test_cursor_config_filename() {
        assert_eq!(hook().config_filename(), "hooks.json");
    }

    #[test]
    fn test_cursor_hook_event_key() {
        assert_eq!(hook().hook_event_key(), "preToolUse");
    }

    #[test]
    fn test_cursor_tool_matcher() {
        assert_eq!(hook().tool_matcher(), "Shell");
    }

    #[test]
    fn test_cursor_build_config_entry_flat_format() {
        // Cursor entries must be flat — no nested "hooks" array
        let entry = hook().build_config_entry("/home/user/.cursor/hooks/skim-rewrite.sh");
        assert_eq!(entry["command"], "/home/user/.cursor/hooks/skim-rewrite.sh");
        assert_eq!(entry["matcher"], "Shell");
        assert!(entry.get("timeout").is_some(), "should have timeout field");
        assert!(
            entry.get("hooks").is_none(),
            "Cursor entries must NOT have a nested 'hooks' array"
        );
    }

    #[test]
    fn test_cursor_upsert_hook_creates_version_wrapper() {
        let mut config = serde_json::json!({});
        hook().upsert_hook(&mut config, "/path/skim-rewrite.sh").unwrap();

        assert_eq!(config["version"], 1, "Cursor config must have version: 1");
        assert!(
            config["hooks"]["preToolUse"].is_array(),
            "entries should be under hooks.preToolUse"
        );
    }

    #[test]
    fn test_cursor_upsert_hook_idempotent() {
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
    fn test_cursor_is_skim_entry_positive() {
        let entry = serde_json::json!({
            "command": "/home/user/.cursor/hooks/skim-rewrite.sh",
            "matcher": "Shell",
            "timeout": 5
        });
        assert!(hook().is_skim_entry(&entry));
    }

    #[test]
    fn test_cursor_is_skim_entry_negative() {
        // Not a skim entry — different command
        let other = serde_json::json!({
            "command": "/home/user/.cursor/hooks/other-hook.sh",
            "matcher": "Shell",
            "timeout": 5
        });
        assert!(!hook().is_skim_entry(&other));

        // Not a skim entry — no command field
        let no_command = serde_json::json!({
            "matcher": "Shell"
        });
        assert!(!hook().is_skim_entry(&no_command));
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
