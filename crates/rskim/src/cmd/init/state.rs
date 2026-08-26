//! State detection for `skim init` (B5).

use std::path::{Path, PathBuf};

use super::flags::{DetectionEnv, InitFlags};
use super::helpers::HOOK_SCRIPT_NAME;
use crate::cmd::hooks::{HookProtocol, protocol_for_agent};

/// Maximum settings.json size we'll read (10 MB). Anything larger is almost
/// certainly not a real Claude Code settings file and could cause OOM.
pub(crate) const MAX_SETTINGS_SIZE: u64 = 10 * 1024 * 1024;

pub(super) struct DetectedState {
    pub(super) skim_binary: PathBuf,
    pub(super) skim_version: String,
    pub(super) config_dir: PathBuf,
    /// Where hook artifacts (script, SHA sidecar, hook registration) live.
    ///
    /// Equals `config_dir` for all agents except Copilot CLI, which routes hook
    /// artifacts to `~/.copilot/` via `HookProtocol::hook_config_dir`.
    pub(super) hook_config_dir: PathBuf,
    pub(super) settings_path: PathBuf,
    pub(super) settings_exists: bool,
    pub(super) hook_installed: bool,
    pub(super) hook_version: Option<String>,
    /// Whether the hook script uses the pinned binary format (exports `SKIM_HOOK_BINARY`).
    pub(super) hook_uses_pinned_binary: bool,
    /// If installing to one scope and the other scope also has a hook
    pub(super) dual_scope_warning: Option<String>,
    /// Existing non-skim hooks for the agent's tool matcher (plugin collision detection)
    pub(super) existing_hooks: Vec<String>,
    /// CLI name of the target agent (e.g., "claude-code", "cursor") for integrity hashing
    pub(super) agent_cli_name: &'static str,
}

impl DetectedState {
    /// Returns `true` when the installed hook is at the current version and
    /// already uses the pinned binary format (no reinstall needed).
    pub(super) fn hook_is_current(&self) -> bool {
        self.hook_version.as_deref() == Some(&self.skim_version) && self.hook_uses_pinned_binary
    }
}

pub(super) fn detect_state(
    flags: &InitFlags,
    agent: crate::cmd::session::AgentKind,
    env: &DetectionEnv,
) -> anyhow::Result<DetectedState> {
    let skim_binary = std::env::current_exe()?;
    let skim_version = env!("CARGO_PKG_VERSION").to_string();
    let config_dir = env.resolve(agent, flags.project)?;
    let protocol = protocol_for_agent(agent);

    // Compute the hook artifact directory via the protocol seam.
    // For all agents except Copilot CLI this equals config_dir (passthrough).
    // For Copilot CLI it redirects to ~/.copilot (or $COPILOT_CONFIG_DIR).
    let hook_config_dir = protocol.hook_config_dir(
        &config_dir,
        flags.project,
        env.override_for(agent).is_some(),
    );

    let settings_path = config_dir.join(protocol.config_filename());
    let settings_exists = settings_path.exists();

    // Read the hook script once so both version extraction and bare-command detection
    // can reuse the same contents rather than making two separate fs::read_to_string calls.
    let hook_script_contents =
        std::fs::read_to_string(hook_config_dir.join("hooks").join(HOOK_SCRIPT_NAME)).ok();

    let mut hook_installed = false;
    let mut hook_version = None;
    let existing_hooks = if protocol.uses_dedicated_hook_file() {
        // Copilot-style: detect from hooks/skim.json, not settings.json.
        hook_installed = protocol.detect_hook_registration(&hook_config_dir);
        if hook_installed {
            // Version always comes from the script file (same for all agents).
            hook_version = hook_script_contents
                .as_deref()
                .and_then(parse_version_from_script);
        }
        protocol.scan_foreign_hooks(&hook_config_dir)
    } else {
        // settings.json-style: existing detection code (behaviorally unchanged).
        let parsed_settings = read_settings_json(&settings_path);
        if let Some(ref json) = parsed_settings
            && let Some(arr) = json
                .get("hooks")
                .and_then(|h| h.get(protocol.hook_event_key()))
                .and_then(|v| v.as_array())
        {
            for entry in arr {
                if protocol.is_skim_entry(entry) {
                    hook_installed = true;
                    hook_version = extract_hook_version_from_entry(
                        entry,
                        &hook_config_dir,
                        hook_script_contents.as_deref(),
                    );
                }
            }
        }
        scan_existing_hooks(
            parsed_settings.as_ref(),
            protocol.hook_event_key(),
            protocol.tool_matcher(),
            protocol.as_ref(),
        )
    };

    // Dual-scope check (B5)
    let dual_scope_warning = check_dual_scope(flags, agent, env)?;

    // Reuse the already-read hook script contents for pinned-binary detection.
    let hook_uses_pinned_binary = hook_script_contents
        .as_deref()
        .map(uses_pinned_binary)
        .unwrap_or(false);

    Ok(DetectedState {
        skim_binary,
        skim_version,
        config_dir,
        hook_config_dir,
        settings_path,
        settings_exists,
        hook_installed,
        hook_version,
        hook_uses_pinned_binary,
        dual_scope_warning,
        existing_hooks,
        agent_cli_name: agent.cli_name(),
    })
}

/// Returns `true` when `contents` of a hook script use the pinned binary format:
/// exports `SKIM_HOOK_BINARY` which is the install-time canonical path.
///
/// Old hook scripts that only have bare `exec skim` (no `SKIM_HOOK_BINARY`) are
/// considered stale and cause a reinstall so they gain the pinned-binary format.
fn uses_pinned_binary(contents: &str) -> bool {
    super::script_has_pinned_marker(contents)
}

/// Check if the hook script at `config_dir/hooks/HOOK_SCRIPT_NAME` uses the
/// pinned binary format.  Used by tests that drive detection with a temp dir.
#[cfg(test)]
fn hook_script_uses_pinned_binary(config_dir: &Path) -> bool {
    let script_path = config_dir.join("hooks").join(HOOK_SCRIPT_NAME);
    std::fs::read_to_string(&script_path)
        .map(|c| uses_pinned_binary(&c))
        .unwrap_or(false)
}

/// Scan already-parsed settings JSON for existing non-skim hooks under `event_key`
/// that match the agent's `tool_matcher`.
///
/// Returns the command strings of any matching entries that are NOT skim entries.
/// Used for plugin collision detection -- warns the user if another tool is also
/// intercepting the same tool type.
///
/// `event_key` is the agent-specific hook event key (e.g., `"PreToolUse"`, `"BeforeTool"`).
/// `tool_matcher` is the agent-specific matcher string (e.g., `"Bash"`, `"Shell"`, `"bash"`).
/// `protocol` is used to determine whether an entry is a skim entry (agent-format-aware).
/// Accepts `Option<&Value>` so callers can reuse an already-parsed settings file
/// instead of re-reading from disk.
fn scan_existing_hooks(
    parsed: Option<&serde_json::Value>,
    event_key: &str,
    tool_matcher: &str,
    protocol: &dyn HookProtocol,
) -> Vec<String> {
    let Some(json) = parsed else {
        return Vec::new();
    };

    let Some(entries) = json
        .get("hooks")
        .and_then(|h| h.get(event_key))
        .and_then(|ptu| ptu.as_array())
    else {
        return Vec::new();
    };

    let mut other_hooks = Vec::new();
    for entry in entries {
        // Only care about entries matching the agent's tool matcher
        let is_matching_tool = entry
            .get("matcher")
            .and_then(|m| m.as_str())
            .is_some_and(|m| m == tool_matcher);
        if !is_matching_tool {
            continue;
        }
        // Skip skim entries using the agent-format-aware check.
        if protocol.is_skim_entry(entry) {
            continue;
        }
        // Claude Code / Gemini / Crush format: nested "hooks" array with "command" field.
        if let Some(hooks) = entry.get("hooks").and_then(|h| h.as_array()) {
            for hook in hooks {
                if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                    other_hooks.push(cmd.to_string());
                }
            }
        // Cursor flat format: top-level "command" field.
        } else if let Some(cmd) = entry.get("command").and_then(|c| c.as_str()) {
            other_hooks.push(cmd.to_string());
        // Copilot CLI format: top-level "bash" field.
        } else if let Some(cmd) = entry.get("bash").and_then(|c| c.as_str()) {
            other_hooks.push(cmd.to_string());
        }
    }

    other_hooks
}

/// Check whether a skim hook is installed in the opposite scope (global vs project)
/// and return a warning string if so.
///
/// # Copilot CLI
///
/// This function reads `<config_dir>/settings.json` for the hook event key, which is
/// the Claude Code / Gemini / Cursor format. For Copilot CLI, `config_dir` resolves
/// to `~/.github/` and the event key is `preToolUse`. Since skim v2.11.0, Copilot
/// hook registration is written to `~/.copilot/hooks/skim.json` (not to
/// `~/.github/settings.json`), so this function **never fires for Copilot CLI** —
/// `has_hook` will always be `false` because the settings file skim no longer writes.
/// This is intentionally conservative: the warning is suppressed rather than
/// producing a false positive. A Copilot-aware dual-scope check can be added if
/// needed in a future subtask.
pub(super) fn check_dual_scope(
    flags: &InitFlags,
    agent: crate::cmd::session::AgentKind,
    env: &DetectionEnv,
) -> anyhow::Result<Option<String>> {
    let other_dir = if flags.project {
        // Installing project-level, check global
        env.resolve(agent, false)?
    } else {
        // Installing global, check project
        match env.resolve(agent, true) {
            Ok(dir) => dir,
            Err(_) => return Ok(None),
        }
    };

    let protocol = protocol_for_agent(agent);
    let other_settings = other_dir.join(protocol.config_filename());
    let has_hook = read_settings_json(&other_settings)
        .and_then(|json| {
            json.get("hooks")?
                .get(protocol.hook_event_key())?
                .as_array()
                .map(|arr| arr.iter().any(|e| protocol.is_skim_entry(e)))
        })
        .unwrap_or(false);

    if !has_hook {
        return Ok(None);
    }

    let scope = if flags.project {
        "globally"
    } else {
        "in project"
    };
    let uninstall_scope = if flags.project {
        "--global"
    } else {
        "--project"
    };
    let path = other_settings.display();
    Ok(Some(format!(
        "skim hook is also installed {scope} ({path})\n  \
         Both hooks will fire, but this is harmless -- the second is a no-op.\n  \
         To remove: skim init {uninstall_scope} --uninstall"
    )))
}

/// Read and parse a settings.json file, returning `None` on any failure.
///
/// Rejects files larger than [`MAX_SETTINGS_SIZE`] to prevent OOM from
/// maliciously crafted settings files (especially in `--project` mode where
/// the file is under repository control).
pub(super) fn read_settings_json(path: &Path) -> Option<serde_json::Value> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > MAX_SETTINGS_SIZE {
        return None;
    }
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Check if a PreToolUse entry contains a skim hook in Claude Code / Gemini / Crush format.
///
/// Checks for `"skim-rewrite"` substring in a nested `hooks[].command` value.
/// This is the Claude Code / Gemini / Crush format. For Cursor and Copilot CLI,
/// use `protocol.is_skim_entry()` which dispatches to agent-specific logic.
pub(crate) fn has_skim_hook_entry(entry: &serde_json::Value) -> bool {
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

/// Extract the skim hook version from the text contents of a hook script.
///
/// Recognises two version formats:
/// - New format: `export SKIM_HOOK_VERSION="x.y.z"`
/// - Legacy format: `# skim-hook vx.y.z`
fn parse_version_from_script(contents: &str) -> Option<String> {
    for line in contents.lines() {
        if let Some(ver) = line.strip_prefix("# skim-hook v").or_else(|| {
            line.strip_prefix("export SKIM_HOOK_VERSION=\"")
                .and_then(|s| s.strip_suffix('"'))
        }) {
            return Some(ver.to_string());
        }
    }
    None
}

/// Try to extract the skim version from the hook script referenced in a settings entry.
///
/// `prefetched_contents` is the already-read hook script text from `detect_state`.
/// When provided, the file read is skipped after path validation succeeds, avoiding
/// a duplicate `fs::read_to_string` call. Pass `None` to always read from disk.
///
/// SECURITY: Validates that the resolved script path is within the expected
/// `{config_dir}/hooks/` directory to prevent arbitrary file reads via
/// attacker-controlled settings.json in `--project` mode.
pub(super) fn extract_hook_version_from_entry(
    entry: &serde_json::Value,
    config_dir: &Path,
    prefetched_contents: Option<&str>,
) -> Option<String> {
    let hooks_dir = config_dir.join("hooks");
    let hooks = entry.get("hooks")?.as_array()?;
    for hook in hooks {
        let cmd = hook.get("command")?.as_str()?;
        if !cmd.contains("skim-rewrite") {
            continue;
        }

        // Resolve the script path.
        let script_path = if cmd.starts_with('/') || cmd.starts_with('.') {
            PathBuf::from(cmd)
        } else {
            hooks_dir.join(HOOK_SCRIPT_NAME)
        };

        // Validate the resolved path is within the expected hooks directory.
        // canonicalize() resolves symlinks and ".." to get the real path.
        let canonical = std::fs::canonicalize(&script_path).ok()?;
        let canonical_hooks_dir = std::fs::canonicalize(&hooks_dir).ok()?;
        if !canonical.starts_with(&canonical_hooks_dir) {
            // Path escapes the hooks directory -- skip version extraction.
            return None;
        }

        // Use prefetched contents when available (path validated above), otherwise
        // fall back to reading from disk (e.g. in tests or when called standalone).
        let owned;
        let contents: &str = if let Some(pre) = prefetched_contents {
            pre
        } else {
            owned = std::fs::read_to_string(&canonical).ok()?;
            &owned
        };

        if let Some(ver) = parse_version_from_script(contents) {
            return Some(ver);
        }
    }
    None
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::hooks::claude::ClaudeCodeHook;
    use crate::cmd::hooks::copilot::CopilotCliHook;
    use crate::cmd::hooks::cursor::CursorHook;

    #[test]
    fn test_hook_script_uses_pinned_binary_true_for_new_format() {
        // New F6 format: exports SKIM_HOOK_BINARY → pinned.
        let dir = tempfile::TempDir::new().unwrap();
        let hooks_dir = dir.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            hooks_dir.join(HOOK_SCRIPT_NAME),
            "#!/usr/bin/env bash\n\
             export SKIM_HOOK_VERSION=\"2.5.1\"\n\
             export SKIM_HOOK_BINARY='/usr/local/bin/skim'\n\
             export SKIM_HOOK_COMMIT=abc1234\n\
             _SKIM_BIN='/usr/local/bin/skim'\n\
             if [ -x \"$_SKIM_BIN\" ]; then\n\
               exec \"$_SKIM_BIN\" rewrite --hook --agent claude-code\n\
             fi\n\
             exec skim rewrite --hook --agent claude-code\n",
        )
        .unwrap();
        assert!(hook_script_uses_pinned_binary(dir.path()));
    }

    #[test]
    fn test_hook_script_uses_pinned_binary_false_for_bare_command() {
        // Old "bare skim" format (pre-F6) is stale → not pinned.
        let dir = tempfile::TempDir::new().unwrap();
        let hooks_dir = dir.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            hooks_dir.join(HOOK_SCRIPT_NAME),
            "#!/usr/bin/env bash\nexport SKIM_HOOK_VERSION=\"2.5.1\"\nexec skim rewrite --hook\n",
        )
        .unwrap();
        assert!(!hook_script_uses_pinned_binary(dir.path()));
    }

    #[test]
    fn test_hook_script_uses_pinned_binary_false_for_old_absolute_path() {
        // Oldest format (hardcoded absolute path, no SKIM_HOOK_BINARY export) → not pinned.
        let dir = tempfile::TempDir::new().unwrap();
        let hooks_dir = dir.path().join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        std::fs::write(
            hooks_dir.join(HOOK_SCRIPT_NAME),
            "#!/usr/bin/env bash\nexec \"/usr/local/bin/skim\" rewrite --hook\n",
        )
        .unwrap();
        assert!(!hook_script_uses_pinned_binary(dir.path()));
    }

    #[test]
    fn test_hook_script_uses_pinned_binary_false_when_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(!hook_script_uses_pinned_binary(dir.path()));
    }

    #[test]
    fn test_scan_existing_hooks_none_input() {
        // No parsed settings at all
        let result = scan_existing_hooks(None, "PreToolUse", "Bash", &ClaudeCodeHook);
        assert!(result.is_empty());
    }

    #[test]
    fn test_scan_existing_hooks_no_other_hooks() {
        // Only skim hook — Claude Code format
        let settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "/home/.claude/hooks/skim-rewrite.sh"}]
                }]
            }
        });

        let result = scan_existing_hooks(Some(&settings), "PreToolUse", "Bash", &ClaudeCodeHook);
        assert!(result.is_empty(), "skim entries should be excluded");
    }

    #[test]
    fn test_scan_existing_hooks_detects_other_hook() {
        // Settings with both skim and another hook with the same matcher (Claude Code format)
        let settings = serde_json::json!({
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

        let result = scan_existing_hooks(Some(&settings), "PreToolUse", "Bash", &ClaudeCodeHook);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "/usr/bin/other-security-hook");
    }

    #[test]
    fn test_scan_existing_hooks_ignores_non_matching_matchers() {
        // An entry with a different matcher should be ignored
        let settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Edit",
                    "hooks": [{"type": "command", "command": "/usr/bin/some-hook"}]
                }]
            }
        });

        let result = scan_existing_hooks(Some(&settings), "PreToolUse", "Bash", &ClaudeCodeHook);
        assert!(
            result.is_empty(),
            "entries with a different matcher should be ignored"
        );
    }

    #[test]
    fn test_scan_existing_hooks_cursor_format() {
        // Cursor flat format: non-skim entry uses top-level "command" field
        let settings = serde_json::json!({
            "hooks": {
                "preToolUse": [
                    {
                        "matcher": "Shell",
                        "command": "/home/.cursor/hooks/skim-rewrite.sh"
                    },
                    {
                        "matcher": "Shell",
                        "command": "/usr/bin/other-cursor-hook"
                    }
                ]
            }
        });

        let result = scan_existing_hooks(Some(&settings), "preToolUse", "Shell", &CursorHook);
        assert_eq!(result.len(), 1, "non-skim Cursor entry should be detected");
        assert_eq!(result[0], "/usr/bin/other-cursor-hook");
    }

    #[test]
    fn test_scan_existing_hooks_cursor_skim_entry_excluded() {
        // Cursor skim entry should be excluded from collision results
        let settings = serde_json::json!({
            "hooks": {
                "preToolUse": [{
                    "matcher": "Shell",
                    "command": "/home/.cursor/hooks/skim-rewrite.sh"
                }]
            }
        });

        let result = scan_existing_hooks(Some(&settings), "preToolUse", "Shell", &CursorHook);
        assert!(result.is_empty(), "Cursor skim entry should be excluded");
    }

    #[test]
    fn test_scan_existing_hooks_copilot_format() {
        // Copilot CLI format: non-skim entry uses top-level "bash" field.
        //
        // NOTE: the `/home/.github/hooks/skim-rewrite.sh` path is INTENTIONAL.
        // `scan_existing_hooks` is called from the settings.json path in `detect_state`,
        // which is only reached for agents that do NOT use a dedicated hook file. For
        // Copilot CLI (which uses `hooks/skim.json`), this code path is bypassed.
        // The `~/.github` literal here preserves the migration-window recognition behavior:
        // `is_skim_entry` must continue to recognise legacy entries written to settings.json
        // by older skim versions, so that `migrate_copilot_legacy` can surgically remove them.
        let settings = serde_json::json!({
            "hooks": {
                "preToolUse": [
                    {
                        "matcher": "bash",
                        "bash": "/home/.github/hooks/skim-rewrite.sh"
                    },
                    {
                        "matcher": "bash",
                        "bash": "/usr/bin/other-copilot-hook"
                    }
                ]
            }
        });

        let result = scan_existing_hooks(Some(&settings), "preToolUse", "bash", &CopilotCliHook);
        assert_eq!(result.len(), 1, "non-skim Copilot entry should be detected");
        assert_eq!(result[0], "/usr/bin/other-copilot-hook");
    }

    #[test]
    fn test_scan_existing_hooks_copilot_skim_entry_excluded() {
        // Copilot skim entry should be excluded from collision results.
        //
        // NOTE: `~/.github/hooks/skim-rewrite.sh` path is INTENTIONAL — same
        // migration-window rationale as `test_scan_existing_hooks_copilot_format` above.
        let settings = serde_json::json!({
            "hooks": {
                "preToolUse": [{
                    "matcher": "bash",
                    "bash": "/home/.github/hooks/skim-rewrite.sh"
                }]
            }
        });

        let result = scan_existing_hooks(Some(&settings), "preToolUse", "bash", &CopilotCliHook);
        assert!(result.is_empty(), "Copilot skim entry should be excluded");
    }

    // ---- parse_version_from_script ----

    #[test]
    fn test_parse_version_from_script_new_format() {
        let script =
            "#!/usr/bin/env bash\nexport SKIM_HOOK_VERSION=\"2.5.1\"\nexec skim rewrite --hook\n";
        assert_eq!(parse_version_from_script(script), Some("2.5.1".to_string()));
    }

    #[test]
    fn test_parse_version_from_script_legacy_format() {
        let script = "#!/usr/bin/env bash\n# skim-hook v1.3.0\nexec skim rewrite --hook\n";
        assert_eq!(parse_version_from_script(script), Some("1.3.0".to_string()));
    }

    #[test]
    fn test_parse_version_from_script_no_version() {
        let script = "#!/usr/bin/env bash\nexec skim rewrite --hook\n";
        assert_eq!(parse_version_from_script(script), None);
    }

    #[test]
    fn test_parse_version_from_script_empty() {
        assert_eq!(parse_version_from_script(""), None);
    }
}
