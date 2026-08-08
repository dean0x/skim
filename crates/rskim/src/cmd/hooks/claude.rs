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
    /// Adds:
    /// - `hookSpecificOutput.additionalContext` — model-facing advisory text
    ///   (~1 KB; detailed provenance diff + remedies). Nested inside
    ///   `hookSpecificOutput` because Claude Code silently ignores top-level
    ///   `additionalContext`.
    /// - `systemMessage` — user-facing one-liner (≤200 chars). Top-level per
    ///   Claude Code hook protocol: shown in the UI, not model context.
    ///
    /// Non-blocking: does NOT set `permissionDecision` — ADR-006 must be
    /// preserved. Never writes to stderr — GRANITE #361 Bug 3 invariant.
    ///
    /// ADR-011/ADR-013 (amended): the advisory is SKIM_DEBUG-gated. The drift
    /// trigger (multiple clones, install-from-source, same-version rebuilds) is
    /// developer-specific; ordinary users essentially never encounter it. A
    /// context-optimising tool must not spend context by default. The gate lives
    /// in `build_advisory` (hook.rs), upstream of this method — `attach_advisory`
    /// is only called when the gate is open and the stamp passes the dedup check.
    fn attach_advisory(
        &self,
        response: &mut serde_json::Value,
        advisory_text: &str,
        system_msg: &str,
    ) {
        // Add model-facing advisory inside hookSpecificOutput.
        if let Some(hso) = response
            .get_mut("hookSpecificOutput")
            .and_then(|v| v.as_object_mut())
        {
            hso.insert(
                "additionalContext".to_string(),
                serde_json::Value::String(advisory_text.to_string()),
            );
        }
        // Add user-facing one-liner at top level (Claude Code protocol).
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
    /// Without this method the advisory is swallowed for exactly the call that
    /// matters most: the FIRST Bash call of a session frequently does not match
    /// a rewrite rule, but is also the first time the agent acts on skim output
    /// and most needs the provenance warning.
    ///
    /// Response shape (Claude Code):
    /// ```json
    /// {
    ///   "hookSpecificOutput": {
    ///     "hookEventName": "PreToolUse",
    ///     "additionalContext": "<advisory>"
    ///   },
    ///   "systemMessage": "<one-liner>"
    /// }
    /// ```
    ///
    /// Security: never sets `permissionDecision` (ADR-006).
    /// Zero stderr (GRANITE #361 Bug 3).
    fn format_advisory_only(
        &self,
        advisory_text: &str,
        system_msg: &str,
    ) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": advisory_text
            },
            "systemMessage": system_msg
        }))
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
