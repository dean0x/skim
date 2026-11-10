//! Claude Code hook protocol implementation.
//!
//! Claude Code uses two hook events, and one script serves both.
//!
//! - `PreToolUse`: the hook reads JSON from stdin, extracts
//!   tool_input.command, rewrites if matched, and emits hookSpecificOutput
//!   with updatedInput.
//! - `PostToolUse` (auto permission mode only): the tool has already run. The
//!   hook reads the captured `tool_response` ([`PostToolUseInput`]) and emits
//!   hookSpecificOutput with updatedToolOutput ([`format_post_response`]).
//!
//! Neither response ever sets a permission decision.

use super::{HookInput, HookProtocol, HookSupport};
use crate::cmd::session::AgentKind;

/// The Claude Code event that fires after a tool call has run and succeeded.
pub(crate) const POST_TOOL_USE_EVENT: &str = "PostToolUse";

/// The only tool whose output the post-run hook may replace.
const BASH_TOOL_NAME: &str = "Bash";

/// A Claude Code `PostToolUse` hook input for a Bash tool call, parsed once at
/// the boundary.
///
/// A value of this type is always a `PostToolUse` event for the `Bash` tool,
/// with a string `tool_input.command` and an object `tool_response`. The
/// borrow ties it to the parsed hook JSON. `tool_response` stays a map and not a
/// struct of known fields, so [`format_post_response`] can copy every key of it,
/// the keys Claude Code adds later too.
#[derive(Debug)]
#[allow(dead_code)] // Read by the post-run hook path once it is wired in.
pub(crate) struct PostToolUseInput<'a> {
    /// Top-level `permission_mode`; `None` when missing or not a string.
    pub(crate) permission_mode: Option<&'a str>,
    /// `tool_input.command`: the command Claude Code ran.
    pub(crate) command: &'a str,
    /// The captured result of the run.
    pub(crate) tool_response: &'a serde_json::Map<String, serde_json::Value>,
}

impl<'a> PostToolUseInput<'a> {
    /// Parse a hook input. Returns `None` for any other event, any other tool,
    /// and any input without a string command or an object `tool_response`.
    #[allow(dead_code)] // Called by the post-run hook path once it is wired in.
    pub(crate) fn parse(json: &'a serde_json::Value) -> Option<Self> {
        if json.get("hook_event_name")?.as_str()? != POST_TOOL_USE_EVENT
            || json.get("tool_name")?.as_str()? != BASH_TOOL_NAME
        {
            return None;
        }
        let command = json.get("tool_input")?.get("command")?.as_str()?;
        let tool_response = json.get("tool_response")?.as_object()?;
        let permission_mode = json
            .get("permission_mode")
            .and_then(serde_json::Value::as_str);
        Some(Self {
            permission_mode,
            command,
            tool_response,
        })
    }
}

/// Build the `PostToolUse` response that replaces a Bash call's captured output.
///
/// Claude Code treats `updatedToolOutput` as a replacement, not a patch, so the
/// result keeps every key of `tool_response` (`interrupted`, `isImage`,
/// `returnCodeInterpretation`, `noOutputExpected`, and any key not known today)
/// and changes only two. `stdout` becomes `stdout`. `stderr` becomes the original
/// text followed by `marker`; a newline goes in between when the original is not
/// empty and does not end with one.
///
/// Returns `None` when `tool_response.stderr` is present and not a string: the
/// response would otherwise overwrite data it cannot read.
///
/// SECURITY: the only keys are `hookSpecificOutput.hookEventName` and
/// `hookSpecificOutput.updatedToolOutput`. The response never carries a
/// decision, `continue`, `systemMessage` or `permissionDecision`.
#[allow(dead_code)] // Called by the post-run hook path once it is wired in.
pub(crate) fn format_post_response(
    tool_response: &serde_json::Map<String, serde_json::Value>,
    stdout: &str,
    marker: &str,
) -> Option<serde_json::Value> {
    let original_stderr = match tool_response.get("stderr") {
        None => "",
        Some(serde_json::Value::String(text)) => text.as_str(),
        Some(_) => return None,
    };
    let separator = if original_stderr.is_empty() || original_stderr.ends_with('\n') {
        ""
    } else {
        "\n"
    };

    let mut updated = tool_response.clone();
    updated.insert("stdout".to_string(), stdout.into());
    updated.insert(
        "stderr".to_string(),
        format!("{original_stderr}{separator}{marker}").into(),
    );
    Some(serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": POST_TOOL_USE_EVENT,
            "updatedToolOutput": serde_json::Value::Object(updated)
        }
    }))
}

/// Claude Code hook implementation (PreToolUse and PostToolUse hooks).
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

    /// `PreToolUse` carries the rewrite; `PostToolUse` carries the output
    /// replacement. No other event is registered. `PostToolUseFailure` in
    /// particular has no `updatedToolOutput` shape, so a failed run stays raw.
    fn hook_event_keys(&self) -> Vec<&'static str> {
        vec![self.hook_event_key(), POST_TOOL_USE_EVENT]
    }

    fn format_response(
        &self,
        tool_input: Option<&serde_json::Value>,
        rewritten_command: &str,
    ) -> serde_json::Value {
        // Claude Code replaces the whole tool input with `updatedInput`, so keep
        // every field of the original input and change only `command` (#586).
        let updated_input = match tool_input {
            Some(serde_json::Value::Object(fields)) => {
                let mut fields = fields.clone();
                fields.insert("command".to_string(), rewritten_command.into());
                serde_json::Value::Object(fields)
            }
            _ => serde_json::json!({ "command": rewritten_command }),
        };
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "updatedInput": updated_input
            }
        })
    }

    fn generate_script(&self, version: &str, binary_path: &str) -> String {
        super::generate_hook_script(version, "claude-code", binary_path, super::HookMode::Strict)
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
        let response = hook().format_response(None, "skim cargo test");
        let output = response.get("hookSpecificOutput").unwrap();
        assert_eq!(output["hookEventName"], "PreToolUse");
        assert_eq!(output["updatedInput"]["command"], "skim cargo test");
    }

    #[test]
    fn test_claude_format_response_keeps_every_tool_input_field() {
        let tool_input = serde_json::json!({
            "command": "cargo test",
            "description": "Run the tests",
            "timeout": 600000,
            "run_in_background": true,
            "dangerouslyDisableSandbox": false
        });
        let response = hook().format_response(Some(&tool_input), "skim cargo test");

        let mut expected = tool_input.clone();
        expected["command"] = serde_json::json!("skim cargo test");
        assert_eq!(response["hookSpecificOutput"]["updatedInput"], expected);
    }

    #[test]
    fn test_claude_format_response_without_an_input_object_sends_the_command_only() {
        for tool_input in [None, Some(serde_json::json!("cargo test"))] {
            let response = hook().format_response(tool_input.as_ref(), "skim cargo test");
            assert_eq!(
                response["hookSpecificOutput"]["updatedInput"],
                serde_json::json!({ "command": "skim cargo test" })
            );
        }
    }

    #[test]
    fn test_claude_format_response_no_permission_decision() {
        let tool_input = serde_json::json!({
            "command": "cargo test",
            "description": "Run the tests",
            "dangerouslyDisableSandbox": true
        });
        for input in [None, Some(&tool_input)] {
            let response = hook().format_response(input, "skim cargo test");
            // SECURITY: Must never set permissionDecision, at the top level or
            // under hookSpecificOutput, also when the input fields are copied.
            assert!(response.get("permissionDecision").is_none());
            let mut keys: Vec<&str> = response["hookSpecificOutput"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, ["hookEventName", "updatedInput"]);
        }
    }

    #[test]
    fn test_claude_generate_script_pinned_binary() {
        let script = hook().generate_script("1.0.0", "/usr/local/bin/skim");
        assert!(script.contains("#!/usr/bin/env bash"));
        assert!(script.contains("# skim-hook v1.0.0"));
        assert!(script.contains("SKIM_HOOK_VERSION=\"1.0.0\""));
        assert!(script.contains("export SKIM_HOOK_BINARY="));
        assert!(script.contains("export SKIM_HOOK_COMMIT="));
        assert!(script.contains("exec \"$SKIM_HOOK_BINARY\" rewrite --hook --agent claude-code"));
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

    // ------------------------------------------------------------------------
    // Hook event keys
    // ------------------------------------------------------------------------

    #[test]
    fn test_claude_hook_event_keys_cover_pre_and_post_tool_use() {
        assert_eq!(hook().hook_event_keys(), ["PreToolUse", "PostToolUse"]);
    }

    #[test]
    fn test_claude_keeps_pre_tool_use_as_its_primary_event_key() {
        assert_eq!(hook().hook_event_key(), "PreToolUse");
    }

    #[test]
    fn test_claude_upsert_hook_registers_both_events() {
        let mut config = serde_json::json!({});
        hook()
            .upsert_hook(&mut config, "/path/to/skim-rewrite.sh")
            .unwrap();

        for event in ["PreToolUse", "PostToolUse"] {
            let entries = config["hooks"][event].as_array().unwrap();
            assert_eq!(entries.len(), 1, "{event} should hold one skim entry");
            assert_eq!(entries[0]["matcher"], "Bash");
            assert_eq!(
                entries[0]["hooks"][0]["command"],
                "/path/to/skim-rewrite.sh"
            );
            assert_eq!(entries[0]["hooks"][0]["timeout"], 5);
        }
        // Only the two events skim serves: no failure event, no other key.
        let mut events: Vec<&str> = config["hooks"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        events.sort_unstable();
        assert_eq!(events, ["PostToolUse", "PreToolUse"]);
    }

    #[test]
    fn test_claude_upsert_hook_is_idempotent_for_both_events() {
        let mut config = serde_json::json!({});
        for _ in 0..2 {
            hook()
                .upsert_hook(&mut config, "/path/to/skim-rewrite.sh")
                .unwrap();
        }
        for event in ["PreToolUse", "PostToolUse"] {
            assert_eq!(config["hooks"][event].as_array().unwrap().len(), 1);
        }
    }

    #[test]
    fn test_claude_upsert_hook_keeps_foreign_post_tool_use_entries() {
        let foreign = serde_json::json!({
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": "/usr/bin/other-hook"}]
        });
        let mut config = serde_json::json!({ "hooks": { "PostToolUse": [foreign.clone()] } });
        hook()
            .upsert_hook(&mut config, "/path/to/skim-rewrite.sh")
            .unwrap();

        let entries = config["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], foreign);
        assert!(hook().is_skim_entry(&entries[1]));
    }

    #[test]
    fn test_claude_upsert_hook_rejects_a_post_tool_use_that_is_not_an_array() {
        let mut config = serde_json::json!({ "hooks": { "PostToolUse": "not-an-array" } });
        assert!(
            hook()
                .upsert_hook(&mut config, "/path/to/skim-rewrite.sh")
                .is_err()
        );
    }

    // ------------------------------------------------------------------------
    // PostToolUseInput
    // ------------------------------------------------------------------------

    fn post_input() -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "permission_mode": "auto",
            "tool_input": { "command": "cargo test" },
            "tool_response": { "stdout": "ok", "stderr": "", "interrupted": false }
        })
    }

    #[test]
    fn test_post_input_parses_a_bash_post_tool_use_event() {
        let json = post_input();
        let input = PostToolUseInput::parse(&json).unwrap();
        assert_eq!(input.permission_mode, Some("auto"));
        assert_eq!(input.command, "cargo test");
        assert_eq!(input.tool_response["stdout"], "ok");
    }

    #[test]
    fn test_post_input_permission_mode_is_none_when_missing_or_not_a_string() {
        let mut missing = post_input();
        missing.as_object_mut().unwrap().remove("permission_mode");
        let mut numeric = post_input();
        numeric["permission_mode"] = serde_json::json!(1);
        for json in [missing, numeric] {
            assert_eq!(
                PostToolUseInput::parse(&json).unwrap().permission_mode,
                None
            );
        }
    }

    #[test]
    fn test_post_input_declines_any_other_event() {
        for event in [
            serde_json::json!("PreToolUse"),
            serde_json::json!("PostToolUseFailure"),
            serde_json::json!("Stop"),
            serde_json::json!(7),
        ] {
            let mut json = post_input();
            json["hook_event_name"] = event;
            assert!(PostToolUseInput::parse(&json).is_none());
        }
        let mut json = post_input();
        json.as_object_mut().unwrap().remove("hook_event_name");
        assert!(PostToolUseInput::parse(&json).is_none());
    }

    #[test]
    fn test_post_input_declines_any_other_tool() {
        for tool in [
            serde_json::json!("Read"),
            serde_json::json!("bash"),
            serde_json::json!(null),
        ] {
            let mut json = post_input();
            json["tool_name"] = tool;
            assert!(PostToolUseInput::parse(&json).is_none());
        }
        let mut json = post_input();
        json.as_object_mut().unwrap().remove("tool_name");
        assert!(PostToolUseInput::parse(&json).is_none());
    }

    #[test]
    fn test_post_input_declines_a_missing_or_non_string_command() {
        for tool_input in [
            serde_json::json!({}),
            serde_json::json!({ "command": ["cargo", "test"] }),
            serde_json::json!("cargo test"),
        ] {
            let mut json = post_input();
            json["tool_input"] = tool_input;
            assert!(PostToolUseInput::parse(&json).is_none());
        }
        let mut json = post_input();
        json.as_object_mut().unwrap().remove("tool_input");
        assert!(PostToolUseInput::parse(&json).is_none());
    }

    #[test]
    fn test_post_input_declines_a_tool_response_that_is_not_an_object() {
        for response in [
            serde_json::json!("ok"),
            serde_json::json!(["ok"]),
            serde_json::json!(null),
        ] {
            let mut json = post_input();
            json["tool_response"] = response;
            assert!(PostToolUseInput::parse(&json).is_none());
        }
        let mut json = post_input();
        json.as_object_mut().unwrap().remove("tool_response");
        assert!(PostToolUseInput::parse(&json).is_none());
    }

    #[test]
    fn test_post_input_declines_input_that_is_not_an_object() {
        for json in [
            serde_json::json!([]),
            serde_json::json!("PostToolUse"),
            serde_json::json!(null),
        ] {
            assert!(PostToolUseInput::parse(&json).is_none());
        }
    }

    // ------------------------------------------------------------------------
    // format_post_response
    // ------------------------------------------------------------------------

    const MARKER: &str = "[skim] compressed 100->40 B; rerun with SKIM_PASSTHROUGH=1 for raw";

    fn response_map(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value.as_object().unwrap().clone()
    }

    fn updated_output(
        tool_response: &serde_json::Value,
        stdout: &str,
    ) -> serde_json::Map<String, serde_json::Value> {
        let response = format_post_response(&response_map(tool_response.clone()), stdout, MARKER)
            .expect("a tool_response with a string stderr gets a response");
        response["hookSpecificOutput"]["updatedToolOutput"]
            .as_object()
            .unwrap()
            .clone()
    }

    #[test]
    fn test_post_response_copies_every_tool_response_key() {
        let tool_response = serde_json::json!({
            "stdout": "raw output",
            "stderr": "",
            "interrupted": false,
            "isImage": false,
            "noOutputExpected": false,
            "returnCodeInterpretation": "grep: no match",
            "futureField": { "nested": [1, 2, 3] }
        });
        let updated = updated_output(&tool_response, "compressed");

        let mut expected_keys: Vec<&String> = tool_response.as_object().unwrap().keys().collect();
        let mut keys: Vec<&String> = updated.keys().collect();
        expected_keys.sort_unstable();
        keys.sort_unstable();
        assert_eq!(keys, expected_keys, "no key is dropped and none is added");
        for key in [
            "interrupted",
            "isImage",
            "noOutputExpected",
            "returnCodeInterpretation",
            "futureField",
        ] {
            assert_eq!(
                updated[key], tool_response[key],
                "{key} must be copied as is"
            );
        }
    }

    #[test]
    fn test_post_response_replaces_stdout() {
        let tool_response = serde_json::json!({ "stdout": "raw output", "stderr": "" });
        let updated = updated_output(&tool_response, "compressed");
        assert_eq!(updated["stdout"], "compressed");
    }

    #[test]
    fn test_post_response_stderr_is_the_marker_when_the_original_is_empty() {
        let tool_response = serde_json::json!({ "stdout": "raw", "stderr": "" });
        assert_eq!(updated_output(&tool_response, "c")["stderr"], MARKER);
    }

    #[test]
    fn test_post_response_stderr_is_the_marker_when_the_original_is_missing() {
        let tool_response = serde_json::json!({ "stdout": "raw" });
        assert_eq!(updated_output(&tool_response, "c")["stderr"], MARKER);
    }

    #[test]
    fn test_post_response_adds_a_newline_before_the_marker_when_stderr_lacks_one() {
        let tool_response = serde_json::json!({ "stdout": "raw", "stderr": "warning: x" });
        assert_eq!(
            updated_output(&tool_response, "c")["stderr"],
            format!("warning: x\n{MARKER}")
        );
    }

    #[test]
    fn test_post_response_adds_no_second_newline_when_stderr_ends_with_one() {
        let tool_response = serde_json::json!({ "stdout": "raw", "stderr": "warning: x\n" });
        assert_eq!(
            updated_output(&tool_response, "c")["stderr"],
            format!("warning: x\n{MARKER}")
        );
    }

    #[test]
    fn test_post_response_declines_a_stderr_that_is_not_a_string() {
        for stderr in [
            serde_json::json!(null),
            serde_json::json!(3),
            serde_json::json!(["x"]),
        ] {
            let tool_response =
                response_map(serde_json::json!({ "stdout": "raw", "stderr": stderr }));
            assert!(format_post_response(&tool_response, "c", MARKER).is_none());
        }
    }

    #[test]
    fn test_post_response_does_not_change_the_tool_response_it_reads() {
        let tool_response = response_map(serde_json::json!({ "stdout": "raw", "stderr": "e" }));
        let before = tool_response.clone();
        let _ = format_post_response(&tool_response, "c", MARKER);
        assert_eq!(tool_response, before);
    }

    #[test]
    fn test_post_response_key_set_is_exact_and_carries_no_decision() {
        let tool_response = serde_json::json!({
            "stdout": "raw output",
            "stderr": "e",
            "interrupted": false,
            "isImage": false
        });
        let response =
            format_post_response(&response_map(tool_response), "compressed", MARKER).unwrap();

        // SECURITY: no permission decision, at the top level or under
        // hookSpecificOutput, and none of the keys that steer the host.
        let top: Vec<&str> = response
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(top, ["hookSpecificOutput"]);
        let mut inner: Vec<&str> = response["hookSpecificOutput"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        inner.sort_unstable();
        assert_eq!(inner, ["hookEventName", "updatedToolOutput"]);
        assert_eq!(
            response["hookSpecificOutput"]["hookEventName"],
            "PostToolUse"
        );

        let text = response.to_string();
        for key in [
            "decision",
            "continue",
            "systemMessage",
            "permissionDecision",
        ] {
            assert!(
                !text.contains(&format!("\"{key}\"")),
                "the response must not carry a {key} key"
            );
        }
    }
}
