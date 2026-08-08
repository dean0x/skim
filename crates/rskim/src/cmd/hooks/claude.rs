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

    /// Attach drift advisory in-band to a Claude Code hook response.
    ///
    /// ADR-013 split-gate — two channels, different ADR-011 classes:
    ///
    /// - `system_msg` (always present): top-level `systemMessage`. User-facing
    ///   only ("shown to you, not to Claude" — hooks.md). Zero model context.
    ///   Fires unconditionally whenever drift is detected and stamp passes.
    ///
    /// - `advisory_text` (`Some` only when `SKIM_DEBUG=1`): nested inside
    ///   `hookSpecificOutput.additionalContext`. Model-facing; persisted to the
    ///   session transcript and replayed on `--continue`/`--resume`. Gated to
    ///   avoid permanently spending context by default.
    ///
    /// Non-blocking: does NOT set `permissionDecision` — ADR-006 preserved.
    /// Zero stderr — GRANITE #361 Bug 3 invariant.
    fn attach_advisory(
        &self,
        response: &mut serde_json::Value,
        advisory_text: Option<&str>,
        system_msg: &str,
    ) {
        // ADR-013: add model-facing advisory inside hookSpecificOutput only when
        // SKIM_DEBUG=1 (advisory_text is Some).
        if let Some(text) = advisory_text
            && let Some(hso) = response
                .get_mut("hookSpecificOutput")
                .and_then(|v| v.as_object_mut())
        {
            hso.insert(
                "additionalContext".to_string(),
                serde_json::Value::String(text.to_string()),
            );
        }
        // Always add user-facing one-liner at top level (Claude Code protocol).
        if let Some(obj) = response.as_object_mut() {
            obj.insert(
                "systemMessage".to_string(),
                serde_json::Value::String(system_msg.to_string()),
            );
        }
    }

    /// Build an advisory-only Claude Code hook response (no rewrite).
    ///
    /// Used when no command rewrite matched but drift was detected.
    /// The response carries only the advisory fields — `updatedInput` is
    /// intentionally absent so the agent runs the original command unchanged.
    ///
    /// ADR-013 split-gate — response shape varies by debug state:
    ///
    /// Debug OFF (`advisory_text` is `None`):
    /// ```json
    /// { "systemMessage": "<one-liner>" }
    /// ```
    ///
    /// Debug ON (`advisory_text` is `Some`):
    /// ```json
    /// {
    ///   "hookSpecificOutput": {
    ///     "hookEventName": "PreToolUse",
    ///     "additionalContext": "<full advisory>"
    ///   },
    ///   "systemMessage": "<one-liner>"
    /// }
    /// ```
    ///
    /// Both shapes are valid per the hook spec: `systemMessage` is a universal
    /// top-level field; `hookSpecificOutput` is only required when making a
    /// PreToolUse-specific decision (`permissionDecision`, `updatedInput`, etc.).
    ///
    /// Security: never sets `permissionDecision` (ADR-006).
    /// Zero stderr (GRANITE #361 Bug 3).
    fn format_advisory_only(
        &self,
        advisory_text: Option<&str>,
        system_msg: &str,
    ) -> Option<serde_json::Value> {
        match advisory_text {
            Some(text) => Some(serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "additionalContext": text
                },
                "systemMessage": system_msg
            })),
            None => Some(serde_json::json!({
                "systemMessage": system_msg
            })),
        }
    }

    fn generate_script(&self, version: &str, binary_path: &str) -> String {
        super::generate_hook_script(version, "claude-code", binary_path)
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
        let response = hook().format_response("skim cargo test");
        let output = response.get("hookSpecificOutput").unwrap();
        assert_eq!(output["hookEventName"], "PreToolUse");
        assert_eq!(output["updatedInput"]["command"], "skim cargo test");
    }

    #[test]
    fn test_claude_format_response_no_permission_decision() {
        let response = hook().format_response("skim cargo test");
        // SECURITY: Must never set permissionDecision
        assert!(response.get("permissionDecision").is_none());
    }

    #[test]
    fn test_claude_generate_script_pinned_binary() {
        let script = hook().generate_script("1.0.0", "/usr/local/bin/skim");
        assert!(script.contains("#!/usr/bin/env bash"));
        assert!(script.contains("# skim-hook v1.0.0"));
        assert!(script.contains("SKIM_HOOK_VERSION=\"1.0.0\""));
        assert!(script.contains("export SKIM_HOOK_BINARY="));
        assert!(script.contains("export SKIM_HOOK_COMMIT="));
        assert!(script.contains("exec \"$_SKIM_BIN\" rewrite --hook --agent claude-code"));
        // PATH fallback must still be present.
        assert!(script.contains("exec skim rewrite --hook --agent claude-code"));
    }

    #[test]
    fn test_claude_generate_script_init_comment() {
        let script = hook().generate_script("1.0.0", "/usr/local/bin/skim");
        assert!(script.contains("skim init --agent claude-code"));
    }

    #[test]
    fn test_claude_install_default() {
        let opts = InstallOpts {
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
