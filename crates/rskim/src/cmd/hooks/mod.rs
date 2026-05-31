//! Hook protocol abstraction for multi-agent hook integration.
//!
//! Each agent that supports tool interception hooks implements `HookProtocol`.
//! Agents without hook support use awareness-only installation.
//!
//! ## Config lifecycle methods
//!
//! The trait includes config-lifecycle methods with sane defaults covering the
//! Claude Code `settings.json` / `hooks.PreToolUse` format. Agents that use a
//! different on-disk format override the relevant methods:
//!
//! | Agent       | Config file   | Event key    | Matcher           |
//! |-------------|---------------|--------------|-------------------|
//! | Claude Code | settings.json | PreToolUse   | Bash              |
//! | Cursor      | hooks.json    | preToolUse   | Shell             |
//! | Gemini CLI  | settings.json | BeforeTool   | run_shell_command |
//! | Copilot CLI | settings.json | preToolUse   | bash              |
//! | Crush       | crush.json    | PreToolUse   | Bash              |
//! | Codex CLI   | (none)        | (none)       | (none)            |

pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod copilot;
pub(crate) mod crush;
pub(crate) mod cursor;
pub(crate) mod gemini;

use super::session::AgentKind;

/// Whether an agent supports real hooks or awareness-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookSupport {
    /// Agent supports real tool interception hooks.
    RealHook,
    /// Agent has no hook mechanism; install awareness files only.
    AwarenessOnly,
}

/// Input extracted from agent's hook event JSON.
///
/// AD-HK-1: session_id is extracted from the hook JSON event so that every
/// skim command rewritten in hook mode is tagged with the originating agent
/// session. This enables per-session analytics grouping in the stats dashboard.
#[derive(Debug, Clone)]
pub(crate) struct HookInput {
    pub(crate) command: String,
    /// AD-HK-1: Originating agent session ID, if present in the hook JSON.
    pub(crate) session_id: Option<String>,
}

/// Result of a hook installation.
#[derive(Debug)]
#[allow(dead_code)] // Used in per-agent install() tests
pub(crate) struct InstallResult {
    pub(crate) script_path: Option<std::path::PathBuf>,
    pub(crate) config_patched: bool,
}

/// Options passed to install/uninstall.
#[derive(Debug)]
#[allow(dead_code)] // Used in per-agent install() tests
pub(crate) struct InstallOpts {
    pub(crate) version: String,
    pub(crate) config_dir: std::path::PathBuf,
    pub(crate) project_scope: bool,
    pub(crate) dry_run: bool,
}

/// Options for uninstall.
#[derive(Debug)]
#[allow(dead_code)] // Used in per-agent uninstall() tests
pub(crate) struct UninstallOpts {
    pub(crate) config_dir: std::path::PathBuf,
    pub(crate) force: bool,
}

/// Trait for agent-specific hook protocols.
///
/// Each agent's hook system is different. This trait normalizes:
/// - Hook event parsing (agent JSON -> HookInput)
/// - Response formatting (rewritten command -> agent JSON)
/// - Script generation (binary path -> shell script)
/// - Config lifecycle (config file name, event key, entry format)
/// - Installation/uninstallation
pub(crate) trait HookProtocol {
    #[allow(dead_code)] // Used in tests only
    fn agent_kind(&self) -> AgentKind;

    fn hook_support(&self) -> HookSupport;
    fn parse_input(&self, json: &serde_json::Value) -> Option<HookInput>;
    fn format_response(&self, rewritten_command: &str) -> serde_json::Value;

    #[allow(dead_code)] // Used in tests only
    fn generate_script(&self, version: &str) -> String;

    // -------------------------------------------------------------------------
    // Config lifecycle methods
    //
    // Default implementations match the Claude Code `settings.json` /
    // `hooks.PreToolUse` format. Agents with different config formats override
    // the relevant methods.
    // -------------------------------------------------------------------------

    /// Name of the config file that contains hook configuration.
    ///
    /// Default: `"settings.json"` (Claude Code, Gemini CLI, Copilot CLI).
    /// Override: Cursor → `"hooks.json"`, Crush → `"crush.json"`.
    #[allow(dead_code)]
    fn config_filename(&self) -> &'static str {
        "settings.json"
    }

    /// The top-level event key under `hooks` where hook entries live.
    ///
    /// Default: `"PreToolUse"` (Claude Code, Crush).
    /// Override: Gemini CLI → `"BeforeTool"`, Cursor → `"preToolUse"`, Copilot CLI → `"preToolUse"`.
    #[allow(dead_code)]
    fn hook_event_key(&self) -> &'static str {
        "PreToolUse"
    }

    /// The tool matcher value used when inserting a hook entry.
    ///
    /// Default: `"Bash"`. Cursor overrides to `"Shell"`, Copilot CLI overrides to `"bash"`.
    #[allow(dead_code)]
    fn tool_matcher(&self) -> &'static str {
        "Bash"
    }

    /// Timeout in seconds for the hook command.
    ///
    /// Default: 5 seconds (matches Claude Code defaults).
    #[allow(dead_code)]
    fn hook_timeout(&self) -> u64 {
        5
    }

    /// Build a config entry JSON object for this agent's hook format.
    ///
    /// Default produces the Claude Code / Gemini / Crush format:
    /// ```json
    /// {
    ///   "matcher": "Bash",
    ///   "hooks": [{ "type": "command", "command": "<path>", "timeout": 5 }]
    /// }
    /// ```
    ///
    /// Agents with different formats (Cursor, Copilot CLI) override this method.
    #[allow(dead_code)]
    fn build_config_entry(&self, hook_script_path: &str) -> serde_json::Value {
        serde_json::json!({
            "matcher": self.tool_matcher(),
            "hooks": [{
                "type": "command",
                "command": hook_script_path,
                "timeout": self.hook_timeout()
            }]
        })
    }

    /// Return `true` if `entry` is a skim hook entry in this agent's config format.
    ///
    /// Default checks for `"skim-rewrite"` substring in any nested `command` value
    /// (Claude Code / Gemini / Copilot / Crush). Cursor overrides this.
    #[allow(dead_code)]
    fn is_skim_entry(&self, entry: &serde_json::Value) -> bool {
        entry
            .get("hooks")
            .and_then(|h| h.as_array())
            .is_some_and(|hooks| {
                hooks.iter().any(|hook| {
                    hook.get("command")
                        .and_then(|c| c.as_str())
                        .is_some_and(|cmd| cmd.contains("skim-rewrite"))
                })
            })
    }

    /// Upsert the skim hook entry into a parsed config JSON value in place.
    ///
    /// Removes any existing skim entry under `hooks.<event_key>`, then appends
    /// the new entry. Creates the `hooks` and event-key arrays if absent.
    ///
    /// Default implementation handles the Claude Code / Gemini / Copilot /
    /// Crush array-of-objects format. Cursor overrides this.
    #[allow(dead_code)]
    fn upsert_hook(
        &self,
        config: &mut serde_json::Value,
        hook_script_path: &str,
    ) -> anyhow::Result<()> {
        let obj = config
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("config root is not an object"))?;

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
                anyhow::anyhow!("config 'hooks.{}' is not an array", self.hook_event_key())
            })?;

        // Remove existing skim entries (idempotent upsert)
        event_arr.retain(|e| !self.is_skim_entry(e));

        // Append new entry
        event_arr.push(self.build_config_entry(hook_script_path));

        Ok(())
    }

    /// Remove all skim hook entries from a parsed config JSON value in place.
    ///
    /// Returns `true` if any entries were removed, `false` if none found.
    #[allow(dead_code)]
    fn remove_skim_entries(&self, config: &mut serde_json::Value) -> bool {
        let Some(hooks) = config.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
            return false;
        };

        let Some(event_arr) = hooks
            .get_mut(self.hook_event_key())
            .and_then(|v| v.as_array_mut())
        else {
            return false;
        };

        let before = event_arr.len();
        event_arr.retain(|e| !self.is_skim_entry(e));
        event_arr.len() < before
    }

    /// Detect whether skim is installed in the config at `config_dir`.
    ///
    /// Reads `config_dir/<config_filename()>`, parses JSON, and checks for
    /// a skim entry under `hooks.<hook_event_key()>`.
    ///
    /// Returns `true` when the config file exists and contains a skim entry.
    /// Returns `false` on any I/O or parse error (non-fatal).
    #[allow(dead_code)]
    fn detect_hook(&self, config_dir: &std::path::Path) -> bool {
        use crate::cmd::init::MAX_SETTINGS_SIZE;

        let config_path = config_dir.join(self.config_filename());
        let meta = match std::fs::metadata(&config_path) {
            Ok(m) => m,
            Err(_) => return false,
        };
        if meta.len() > MAX_SETTINGS_SIZE {
            return false;
        }
        let contents = match std::fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let json: serde_json::Value = match serde_json::from_str(&contents) {
            Ok(v) => v,
            Err(_) => return false,
        };

        json.get("hooks")
            .and_then(|h| h.get(self.hook_event_key()))
            .and_then(|v| v.as_array())
            .is_some_and(|entries| entries.iter().any(|e| self.is_skim_entry(e)))
    }

    /// Scan `config_dir/<config_filename()>` for non-skim hook entries in the
    /// same event bucket. Returns the command strings of any such entries.
    ///
    /// Used for collision-detection warnings during install.
    #[allow(dead_code)]
    fn scan_other_hooks(&self, config_dir: &std::path::Path) -> Vec<String> {
        use crate::cmd::init::MAX_SETTINGS_SIZE;

        let config_path = config_dir.join(self.config_filename());
        let meta = match std::fs::metadata(&config_path) {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };
        if meta.len() > MAX_SETTINGS_SIZE {
            return Vec::new();
        }
        let contents = match std::fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let json: serde_json::Value = match serde_json::from_str(&contents) {
            Ok(v) => v,
            Err(_) => return Vec::new(),
        };

        let entries = match json
            .get("hooks")
            .and_then(|h| h.get(self.hook_event_key()))
            .and_then(|v| v.as_array())
        {
            Some(arr) => arr,
            None => return Vec::new(),
        };

        let mut other = Vec::new();
        for entry in entries {
            if self.is_skim_entry(entry) {
                continue;
            }
            // Claude Code / Gemini / Crush format: nested "hooks" array with "command" field.
            if let Some(hooks) = entry.get("hooks").and_then(|h| h.as_array()) {
                for hook in hooks {
                    if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                        other.push(cmd.to_string());
                    }
                }
            // Cursor flat format: top-level "command" field.
            } else if let Some(cmd) = entry.get("command").and_then(|c| c.as_str()) {
                other.push(cmd.to_string());
            // Copilot CLI format: top-level "bash" field.
            } else if let Some(cmd) = entry.get("bash").and_then(|c| c.as_str()) {
                other.push(cmd.to_string());
            }
        }
        other
    }

    /// Default no-op install. Override for agents with real hook installation.
    #[allow(dead_code)] // Used in tests only
    fn install(&self, _opts: &InstallOpts) -> anyhow::Result<InstallResult> {
        Ok(InstallResult {
            script_path: None,
            config_patched: false,
        })
    }

    /// Default no-op uninstall. Override for agents with real hook removal.
    #[allow(dead_code)] // Used in tests only
    fn uninstall(&self, _opts: &UninstallOpts) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Shared parser for agents whose hook JSON nests the command under `tool_input.command`.
///
/// Used by Claude Code, Crush, Copilot CLI, and Gemini CLI. Cursor differs (top-level `command`).
/// Codex is awareness-only and returns `None` from `parse_input` directly.
///
/// AD-HK-1: Also extracts `session_id` from the top-level JSON field when present.
/// Claude Code emits `{ "session_id": "...", "tool_input": { "command": "..." } }`.
pub(crate) fn parse_tool_input_command(json: &serde_json::Value) -> Option<HookInput> {
    let command = json
        .get("tool_input")
        .and_then(|ti| ti.get("command"))
        .and_then(|c| c.as_str())?
        .to_string();
    // AD-HK-1: Extract session_id from top-level JSON field (Claude Code / Copilot / Gemini).
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

/// Upsert a skim hook entry into a versioned config JSON value in place.
///
/// Shared by Cursor and Copilot CLI, both of which wrap their event arrays in:
/// ```json
/// { "version": 1, "hooks": { "<event_key>": [...] } }
/// ```
///
/// The `protocol` is used to obtain `hook_event_key()`, `is_skim_entry()`, and
/// `build_config_entry()` so the caller's overrides are respected.
pub(crate) fn upsert_hook_versioned(
    config: &mut serde_json::Value,
    hook_script_path: &str,
    protocol: &dyn HookProtocol,
) -> anyhow::Result<()> {
    let obj = config
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("config root is not an object"))?;

    // Ensure version field is present.
    obj.entry("version").or_insert(serde_json::json!(1));

    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("config 'hooks' is not an object"))?;

    let event_arr = hooks
        .entry(protocol.hook_event_key())
        .or_insert_with(|| serde_json::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "config 'hooks.{}' is not an array",
                protocol.hook_event_key()
            )
        })?;

    // Remove existing skim entries (idempotent upsert).
    event_arr.retain(|e| !protocol.is_skim_entry(e));

    // Append new entry.
    event_arr.push(protocol.build_config_entry(hook_script_path));

    Ok(())
}

/// Generate a standard hook script for an agent.
///
/// Shared by all RealHook agents. The script sets `SKIM_HOOK_VERSION` and
/// `exec`s PATH-resolved `skim` with `rewrite --hook --agent <agent_cli_name>`.
///
/// SECURITY: Hook scripts use PATH-resolved `skim` (bare command, no absolute path)
/// so users can upgrade skim without reinstalling hooks. An absolute binary path
/// would pin the hook to the install-time location and silently break on upgrades.
///
/// # Panics
///
/// Panics if `version` or `agent_cli_name` contain shell-unsafe characters.
#[allow(dead_code)] // Called by per-agent generate_script() impls, which are test-only
pub(crate) fn generate_hook_script(version: &str, agent_cli_name: &str) -> String {
    assert!(
        version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'),
        "version contains unsafe characters for shell interpolation: {version}"
    );
    assert!(
        agent_cli_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "agent_cli_name contains unsafe characters for shell interpolation: {agent_cli_name}"
    );
    format!(
        "#!/usr/bin/env bash\n\
         # skim-hook v{version}\n\
         # Generated by: skim init --agent {agent_cli_name} -- do not edit manually\n\
         export SKIM_HOOK_VERSION=\"{version}\"\n\
         # Prepend wrapper dir so wrappers work even in restricted PATH environments.\n\
         # Guard with directory existence: skip if ~/.skim/bin was never created\n\
         # (i.e. `skim init --wrappers` has not been run). The skim binary strips\n\
         # this entry at startup to prevent infinite recursion.\n\
         # TRUST: $HOME is the user's own trust domain. An attacker with write\n\
         # access to $HOME already has broader control than PATH manipulation\n\
         # would provide. No ownership check is performed; $HOME membership is\n\
         # the sole trust criterion (defence-in-depth is satisfied by the\n\
         # directory-existence guard above and the recursion-prevention strip).\n\
         [ -d \"$HOME/.skim/bin\" ] && export PATH=\"$HOME/.skim/bin${{PATH:+:$PATH}}\"\n\
         exec skim rewrite --hook --agent {agent_cli_name}\n"
    )
}

/// Factory: create the appropriate HookProtocol implementation for a given agent.
pub(crate) fn protocol_for_agent(kind: AgentKind) -> Box<dyn HookProtocol> {
    match kind {
        AgentKind::ClaudeCode => Box::new(claude::ClaudeCodeHook),
        AgentKind::Cursor => Box::new(cursor::CursorHook),
        AgentKind::GeminiCli => Box::new(gemini::GeminiCliHook),
        AgentKind::CopilotCli => Box::new(copilot::CopilotCliHook),
        AgentKind::CodexCli => Box::new(codex::CodexCliHook),
        AgentKind::Crush => Box::new(crush::CrushHook),
    }
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tool_input_command_valid() {
        let json = serde_json::json!({
            "tool_input": {
                "command": "cargo test --nocapture"
            }
        });
        let result = parse_tool_input_command(&json);
        assert!(result.is_some());
        assert_eq!(result.unwrap().command, "cargo test --nocapture");
    }

    #[test]
    fn test_parse_tool_input_command_missing_tool_input() {
        let json = serde_json::json!({});
        assert!(parse_tool_input_command(&json).is_none());
    }

    #[test]
    fn test_parse_tool_input_command_missing_command() {
        let json = serde_json::json!({
            "tool_input": {
                "file_path": "/tmp/test.rs"
            }
        });
        assert!(parse_tool_input_command(&json).is_none());
    }

    #[test]
    fn test_parse_tool_input_command_non_string() {
        let json = serde_json::json!({
            "tool_input": {
                "command": 42
            }
        });
        assert!(parse_tool_input_command(&json).is_none());
    }

    #[test]
    fn test_generate_hook_script_structure() {
        let script = generate_hook_script("1.2.3", "test-agent");
        assert!(script.starts_with("#!/usr/bin/env bash\n"));
        assert!(script.contains("# skim-hook v1.2.3"));
        assert!(script.contains("skim init --agent test-agent"));
        assert!(script.contains("SKIM_HOOK_VERSION=\"1.2.3\""));
        // PATH prepend must be guarded by directory existence check and appear
        // BEFORE the exec line. The guard avoids polluting PATH when
        // `skim init --wrappers` has not been run.
        assert!(
            script.contains("[ -d \"$HOME/.skim/bin\" ] && export PATH=\"$HOME/.skim/bin"),
            "hook script must guard PATH prepend with directory existence check"
        );
        let path_pos = script.find("export PATH=").unwrap();
        let exec_pos = script.find("exec skim rewrite").unwrap();
        assert!(
            path_pos < exec_pos,
            "PATH export must appear before exec line"
        );
        assert!(script.contains("exec skim rewrite --hook --agent test-agent"));
        // Trust assumption must be documented inline so reviewers understand
        // why no ownership check is performed.
        assert!(
            script.contains("TRUST:"),
            "hook script must contain inline TRUST comment documenting the $HOME trust assumption"
        );
    }

    #[test]
    #[should_panic(expected = "version contains unsafe characters")]
    fn test_generate_hook_script_rejects_unsafe_version() {
        generate_hook_script("1.0.0$(evil)", "test-agent");
    }

    #[test]
    #[should_panic(expected = "agent_cli_name contains unsafe characters")]
    fn test_generate_hook_script_rejects_unsafe_agent_name() {
        generate_hook_script("1.0.0", "agent;rm -rf /");
    }

    // ========================================================================
    // B8: AD-HK-1 — session_id extraction in parse_tool_input_command
    // ========================================================================

    /// AD-HK-1: session_id is extracted from the top-level JSON field.
    #[test]
    fn test_parse_tool_input_command_extracts_session_id() {
        let json = serde_json::json!({
            "session_id": "my-session-abc",
            "tool_input": {
                "command": "cargo build"
            }
        });
        let result = parse_tool_input_command(&json).unwrap();
        assert_eq!(result.command, "cargo build");
        assert_eq!(result.session_id, Some("my-session-abc".to_string()));
    }

    /// AD-HK-1: session_id is None when the field is absent.
    #[test]
    fn test_parse_tool_input_command_no_session_id() {
        let json = serde_json::json!({
            "tool_input": {
                "command": "cargo test"
            }
        });
        let result = parse_tool_input_command(&json).unwrap();
        assert_eq!(result.command, "cargo test");
        assert!(
            result.session_id.is_none(),
            "session_id should be None when not in JSON"
        );
    }

    /// F8: empty string session_id is treated as None at the parse boundary.
    #[test]
    fn test_parse_tool_input_command_empty_session_id_is_none() {
        let json = serde_json::json!({
            "session_id": "",
            "tool_input": {
                "command": "cargo build"
            }
        });
        let result = parse_tool_input_command(&json).unwrap();
        assert_eq!(result.command, "cargo build");
        assert!(
            result.session_id.is_none(),
            "empty session_id should yield None at parse boundary"
        );
    }

    /// AD-HK-1: non-string session_id is treated as None (no panic).
    #[test]
    fn test_parse_tool_input_command_non_string_session_id_is_none() {
        let json = serde_json::json!({
            "session_id": 12345,
            "tool_input": {
                "command": "go test ./..."
            }
        });
        let result = parse_tool_input_command(&json).unwrap();
        assert!(
            result.session_id.is_none(),
            "non-string session_id should yield None"
        );
    }

    // ========================================================================
    // Phase 3: Config lifecycle method tests (default implementation via
    // ClaudeCodeHook which uses all defaults)
    // ========================================================================

    #[test]
    fn test_default_config_filename() {
        let hook = claude::ClaudeCodeHook;
        assert_eq!(hook.config_filename(), "settings.json");
    }

    #[test]
    fn test_default_hook_event_key() {
        let hook = claude::ClaudeCodeHook;
        assert_eq!(hook.hook_event_key(), "PreToolUse");
    }

    #[test]
    fn test_default_tool_matcher() {
        let hook = claude::ClaudeCodeHook;
        assert_eq!(hook.tool_matcher(), "Bash");
    }

    #[test]
    fn test_default_hook_timeout() {
        let hook = claude::ClaudeCodeHook;
        assert_eq!(hook.hook_timeout(), 5);
    }

    #[test]
    fn test_default_build_config_entry() {
        let hook = claude::ClaudeCodeHook;
        let entry = hook.build_config_entry("/path/to/skim-rewrite.sh");
        assert_eq!(entry["matcher"], "Bash");
        let hooks_arr = entry["hooks"].as_array().unwrap();
        assert_eq!(hooks_arr.len(), 1);
        assert_eq!(hooks_arr[0]["type"], "command");
        assert_eq!(hooks_arr[0]["command"], "/path/to/skim-rewrite.sh");
        assert_eq!(hooks_arr[0]["timeout"], 5);
    }

    #[test]
    fn test_default_is_skim_entry_true() {
        let hook = claude::ClaudeCodeHook;
        let entry = serde_json::json!({
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": "/home/.claude/hooks/skim-rewrite.sh"}]
        });
        assert!(hook.is_skim_entry(&entry));
    }

    #[test]
    fn test_default_is_skim_entry_false() {
        let hook = claude::ClaudeCodeHook;
        let entry = serde_json::json!({
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": "/usr/bin/other-tool"}]
        });
        assert!(!hook.is_skim_entry(&entry));
    }

    #[test]
    fn test_default_upsert_hook_creates_entry() {
        let hook = claude::ClaudeCodeHook;
        let mut config = serde_json::json!({});
        hook.upsert_hook(&mut config, "/path/to/skim-rewrite.sh")
            .unwrap();

        let entries = config["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["matcher"], "Bash");
    }

    #[test]
    fn test_default_upsert_hook_idempotent() {
        let hook = claude::ClaudeCodeHook;
        let mut config = serde_json::json!({});
        hook.upsert_hook(&mut config, "/path/to/skim-rewrite.sh")
            .unwrap();
        hook.upsert_hook(&mut config, "/path/to/skim-rewrite.sh")
            .unwrap();

        let entries = config["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(
            entries.len(),
            1,
            "idempotent upsert should not duplicate entries"
        );
    }

    #[test]
    fn test_default_upsert_hook_preserves_other_entries() {
        let hook = claude::ClaudeCodeHook;
        let mut config = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "/usr/bin/other-tool"}]
                }]
            }
        });
        hook.upsert_hook(&mut config, "/path/skim-rewrite.sh")
            .unwrap();

        let entries = config["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(entries.len(), 2, "should preserve non-skim entries");
    }

    #[test]
    fn test_default_remove_skim_entries_removes() {
        let hook = claude::ClaudeCodeHook;
        let mut config = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "/home/.claude/hooks/skim-rewrite.sh"}]
                }]
            }
        });
        let removed = hook.remove_skim_entries(&mut config);
        assert!(removed);
        let entries = config["hooks"]["PreToolUse"].as_array().unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_default_remove_skim_entries_no_match() {
        let hook = claude::ClaudeCodeHook;
        let mut config = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "/usr/bin/other-tool"}]
                }]
            }
        });
        let removed = hook.remove_skim_entries(&mut config);
        assert!(!removed);
    }

    #[test]
    fn test_default_detect_hook_installed() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        let config = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": config_dir.join("hooks/skim-rewrite.sh").to_str().unwrap()}]
                }]
            }
        });
        std::fs::write(
            config_dir.join("settings.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let hook = claude::ClaudeCodeHook;
        assert!(hook.detect_hook(config_dir));
    }

    #[test]
    fn test_default_detect_hook_not_installed() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        let config = serde_json::json!({ "theme": "dark" });
        std::fs::write(
            config_dir.join("settings.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let hook = claude::ClaudeCodeHook;
        assert!(!hook.detect_hook(config_dir));
    }

    #[test]
    fn test_default_detect_hook_missing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let hook = claude::ClaudeCodeHook;
        assert!(
            !hook.detect_hook(dir.path()),
            "missing config file should return false"
        );
    }

    #[test]
    fn test_default_scan_other_hooks_empty_when_only_skim() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        let config = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "/home/.claude/hooks/skim-rewrite.sh"}]
                }]
            }
        });
        std::fs::write(
            config_dir.join("settings.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let hook = claude::ClaudeCodeHook;
        let others = hook.scan_other_hooks(config_dir);
        assert!(others.is_empty(), "only skim entry should return empty vec");
    }

    #[test]
    fn test_default_scan_other_hooks_returns_non_skim() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        let config = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Bash",
                        "hooks": [{"type": "command", "command": "/home/.claude/hooks/skim-rewrite.sh"}]
                    },
                    {
                        "matcher": "Bash",
                        "hooks": [{"type": "command", "command": "/usr/bin/other-security-hook"}]
                    }
                ]
            }
        });
        std::fs::write(
            config_dir.join("settings.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let hook = claude::ClaudeCodeHook;
        let others = hook.scan_other_hooks(config_dir);
        assert_eq!(others, vec!["/usr/bin/other-security-hook"]);
    }

    // ========================================================================
    // upsert_hook_versioned — direct unit tests for the shared DRY helper
    // ========================================================================

    /// upsert_hook_versioned creates a versioned config from scratch.
    #[test]
    fn test_upsert_hook_versioned_creates_from_empty() {
        let mut config = serde_json::json!({});
        let hook = cursor::CursorHook;
        upsert_hook_versioned(&mut config, "/path/to/skim-rewrite.sh", &hook).unwrap();

        assert_eq!(config["version"], 1);
        let arr = config["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["command"], "/path/to/skim-rewrite.sh");
    }

    /// upsert_hook_versioned is idempotent: a second call replaces the existing entry.
    #[test]
    fn test_upsert_hook_versioned_idempotent() {
        let mut config = serde_json::json!({});
        let hook = cursor::CursorHook;
        upsert_hook_versioned(&mut config, "/path/to/skim-rewrite.sh", &hook).unwrap();
        upsert_hook_versioned(&mut config, "/path/to/skim-rewrite.sh", &hook).unwrap();

        let arr = config["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "idempotent upsert must not duplicate entries");
    }

    /// upsert_hook_versioned preserves pre-existing non-skim entries.
    #[test]
    fn test_upsert_hook_versioned_preserves_other_entries() {
        let other_entry = serde_json::json!({
            "command": "/usr/bin/other-hook",
            "matcher": "Shell",
            "timeout": 10
        });
        let mut config = serde_json::json!({
            "version": 1,
            "hooks": { "preToolUse": [other_entry] }
        });
        let hook = cursor::CursorHook;
        upsert_hook_versioned(&mut config, "/path/to/skim-rewrite.sh", &hook).unwrap();

        let arr = config["hooks"]["preToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 2, "non-skim entry must be preserved");
        let commands: Vec<&str> = arr.iter().filter_map(|e| e["command"].as_str()).collect();
        assert!(commands.contains(&"/usr/bin/other-hook"));
        assert!(commands.contains(&"/path/to/skim-rewrite.sh"));
    }

    /// upsert_hook_versioned errors when config root is not an object.
    #[test]
    fn test_upsert_hook_versioned_errors_on_non_object_root() {
        let mut config = serde_json::json!([]);
        let hook = cursor::CursorHook;
        let result = upsert_hook_versioned(&mut config, "/path/to/skim-rewrite.sh", &hook);
        assert!(result.is_err(), "non-object root must return an error");
    }

    // ========================================================================
    // scan_other_hooks — Cursor flat "command" and Copilot flat "bash" formats
    // ========================================================================

    /// Cursor flat format: scan_other_hooks extracts top-level "command" from
    /// a non-skim entry in hooks.json.
    #[test]
    fn test_scan_other_hooks_cursor_flat_command_format() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        // Cursor uses hooks.json with a versioned wrapper and flat entries
        // where each entry has a top-level "command" field.
        let config = serde_json::json!({
            "version": 1,
            "hooks": {
                "preToolUse": [
                    {
                        "command": "/home/user/.cursor/hooks/skim-rewrite.sh",
                        "matcher": "Shell",
                        "timeout": 5
                    },
                    {
                        "command": "/usr/bin/other-cursor-hook",
                        "matcher": "Shell",
                        "timeout": 10
                    }
                ]
            }
        });
        std::fs::write(
            config_dir.join("hooks.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let hook = cursor::CursorHook;
        let others = hook.scan_other_hooks(config_dir);
        assert_eq!(
            others,
            vec!["/usr/bin/other-cursor-hook"],
            "scan_other_hooks must extract non-skim entries via flat 'command' field"
        );
    }

    /// Copilot CLI format: scan_other_hooks extracts top-level "bash" from
    /// a non-skim entry in settings.json.
    #[test]
    fn test_scan_other_hooks_copilot_flat_bash_format() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_dir = dir.path();
        // Copilot CLI uses settings.json with a versioned wrapper and flat entries
        // where each entry has a top-level "bash" field instead of "command".
        let config = serde_json::json!({
            "version": 1,
            "hooks": {
                "preToolUse": [
                    {
                        "type": "command",
                        "bash": "/home/user/.copilot/hooks/skim-rewrite.sh",
                        "matcher": "bash",
                        "timeoutSec": 5
                    },
                    {
                        "type": "command",
                        "bash": "/usr/bin/other-copilot-hook",
                        "matcher": "bash",
                        "timeoutSec": 10
                    }
                ]
            }
        });
        std::fs::write(
            config_dir.join("settings.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();

        let hook = copilot::CopilotCliHook;
        let others = hook.scan_other_hooks(config_dir);
        assert_eq!(
            others,
            vec!["/usr/bin/other-copilot-hook"],
            "scan_other_hooks must extract non-skim entries via flat 'bash' field"
        );
    }
}
