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
    /// SKIM_HOOK_COMMIT recorded in the installed hook script.
    /// `None` when the script is absent or predates commit pinning.
    pub(super) hook_commit: Option<String>,
    /// Absolute path that the hook script's `SKIM_HOOK_BINARY` exports.
    /// `None` when the script is absent or predates binary pinning.
    pub(super) hook_binary_pin: Option<String>,
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
    /// Returns `true` when the hook's recorded binary pin points to the same
    /// canonical path as the currently-running binary.
    ///
    /// # Why this is separate from `hook_is_current`
    ///
    /// `hook_is_current` checks version + pinned format + commit. When two skim
    /// clones sit on the same commit (e.g., `main` vs a worktree at the same
    /// SHA), all three of those fields match — yet the hook still points to the
    /// wrong binary. `pin_is_current` catches that remaining case by comparing
    /// the actual file-system path, not just the version/commit metadata.
    ///
    /// `skim doctor` can display the `hook_binary_pin` field independently
    /// (PF-015: display-without-gate pattern), which is why this predicate is
    /// separate: it lets the doctor report a pin-mismatch cause with
    /// `hook_is_current=true, pin_is_current=false` before the fix would have
    /// been taken by the former fast-path check.
    pub(super) fn pin_is_current(&self) -> bool {
        let Some(ref pinned) = self.hook_binary_pin else {
            return false; // no pin recorded → treat as stale
        };
        // Resolve the running binary to its canonical path so symlinked
        // installations are compared correctly (ADR-004).
        match super::helpers::resolve_skim_binary() {
            Ok(running) => {
                let pinned_path = std::path::Path::new(pinned.as_str());
                // Canonicalize the pinned path too, in case it was recorded as
                // a symlink target that has since been re-targeted.
                let canon_pinned =
                    std::fs::canonicalize(pinned_path).unwrap_or_else(|_| pinned_path.to_owned());
                running == canon_pinned
            }
            Err(_) => false, // cannot resolve running binary — treat as mismatch
        }
    }

    /// Returns `true` when the installed hook is at the current version, uses
    /// the pinned binary format, AND pins the same git commit as this binary.
    ///
    /// # B5c — commit-pin comparison
    ///
    /// A plain version-only check misses in-place rebuilds at the same semver:
    /// the hook may pin an older commit while the binary has a newer one. When
    /// `SKIM_GIT_COMMIT` is "unknown" (tarball builds) the commit check is
    /// skipped to avoid spurious "not current" verdicts on every invocation.
    pub(super) fn hook_is_current(&self) -> bool {
        if !(self.hook_version.as_deref() == Some(&self.skim_version)
            && self.hook_uses_pinned_binary)
        {
            return false;
        }

        // B5c: also require that the hook's recorded commit matches the
        // compiled-in commit. Skip the check when the compiled commit is
        // "unknown" (tarball/non-git build) — we have no reliable anchor.
        let compiled_commit = option_env!("SKIM_GIT_COMMIT").unwrap_or("unknown");
        if compiled_commit != "unknown" {
            if let Some(ref hook_commit) = self.hook_commit {
                if hook_commit != compiled_commit {
                    return false;
                }
            }
            // hook_commit is None → script predates commit pinning → not current
            // (hook_uses_pinned_binary is true but commit is absent means the
            // script format is inconsistent; treat as stale to force a reinstall).
            else {
                return false;
            }
        }

        true
    }
}

pub(super) fn detect_state(
    flags: &InitFlags,
    agent: crate::cmd::session::AgentKind,
    env: &DetectionEnv,
) -> anyhow::Result<DetectedState> {
    let skim_binary = super::helpers::resolve_skim_binary()?;
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
    let existing_hooks;

    if protocol.uses_dedicated_hook_file() {
        // Copilot-style: detect from hooks/skim.json, not settings.json.
        hook_installed = protocol.detect_hook_registration(&hook_config_dir);
        if hook_installed {
            // Version always comes from the script file (same for all agents).
            hook_version = hook_script_contents
                .as_deref()
                .and_then(parse_version_from_script);
        }
        existing_hooks = protocol.scan_foreign_hooks(&hook_config_dir);
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
                    // Fallback: read version from the same script file used for
                    // hook_commit and hook_binary_pin, so there is ONE source of
                    // truth per file. This covers agents (e.g. Cursor) whose
                    // settings entry format is flat (no nested "hooks" array) and
                    // is therefore not understood by extract_hook_version_from_entry.
                    // Also covers the path-containment failure case: a failed
                    // security check returns None and must not masquerade as a
                    // version mismatch.
                    if hook_version.is_none() {
                        hook_version = hook_script_contents
                            .as_deref()
                            .and_then(parse_version_from_script);
                    }
                }
            }
        }
        existing_hooks = scan_existing_hooks(
            parsed_settings.as_ref(),
            protocol.hook_event_key(),
            protocol.tool_matcher(),
            protocol.as_ref(),
        );
    }

    // Dual-scope check (B5)
    let dual_scope_warning = check_dual_scope(flags, agent, env)?;

    // Reuse the already-read hook script contents for pinned-binary detection.
    let hook_uses_pinned_binary = hook_script_contents
        .as_deref()
        .map(uses_pinned_binary)
        .unwrap_or(false);

    // B5c / B4: extract the commit and binary-pin from the hook script.
    let hook_commit = hook_script_contents
        .as_deref()
        .and_then(parse_commit_from_script);
    let hook_binary_pin = hook_script_contents
        .as_deref()
        .and_then(parse_binary_pin_from_script);

    Ok(DetectedState {
        skim_binary,
        skim_version,
        config_dir,
        hook_config_dir,
        settings_path,
        settings_exists,
        hook_installed,
        hook_version,
        hook_commit,
        hook_binary_pin,
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

/// Extract the git commit recorded in the hook script (`export SKIM_HOOK_COMMIT=<sha>`).
///
/// Returns `None` when the line is absent (script predates commit pinning).
/// The commit value is unquoted in the generated script (hex only), so no
/// quote stripping is needed.
pub(crate) fn parse_commit_from_script(contents: &str) -> Option<String> {
    for line in contents.lines() {
        if let Some(val) = line.trim_start().strip_prefix("export SKIM_HOOK_COMMIT=") {
            let val = val.trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Extract the binary path recorded in the hook script (`export SKIM_HOOK_BINARY=...`).
///
/// The value is single-quoted in the generated script (see `shell_single_quote`
/// in `hooks/mod.rs`). This parser handles:
/// - `'...'` (single-quoted, normal form)
/// - `'...'\''...'` (single-quote escape sequences inside the path)
/// - Bare unquoted values (forward-compat safety net)
///
/// Returns `None` when the line is absent (script predates binary pinning).
pub(crate) fn parse_binary_pin_from_script(contents: &str) -> Option<String> {
    for line in contents.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("export SKIM_HOOK_BINARY=") {
            let val = if rest.starts_with('\'') {
                // Single-quoted: strip outer quotes and unescape `'\''` → `'`
                let inner = rest
                    .strip_prefix('\'')
                    .and_then(|s| s.strip_suffix('\''))
                    .unwrap_or(rest);
                inner.replace("'\\''", "'")
            } else {
                rest.to_string()
            };
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
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

    // ---- parse_commit_from_script ----

    #[test]
    fn test_parse_commit_from_script_present() {
        let script = "#!/usr/bin/env bash\nexport SKIM_HOOK_COMMIT=abc1234\nexec skim rewrite\n";
        assert_eq!(
            parse_commit_from_script(script),
            Some("abc1234".to_string())
        );
    }

    #[test]
    fn test_parse_commit_from_script_absent() {
        let script = "#!/usr/bin/env bash\nexport SKIM_HOOK_VERSION=\"2.5.1\"\nexec skim rewrite\n";
        assert_eq!(parse_commit_from_script(script), None);
    }

    #[test]
    fn test_parse_commit_from_script_empty() {
        assert_eq!(parse_commit_from_script(""), None);
    }

    // ---- parse_binary_pin_from_script ----

    #[test]
    fn test_parse_binary_pin_from_script_single_quoted() {
        let script =
            "#!/usr/bin/env bash\nexport SKIM_HOOK_BINARY='/usr/local/bin/skim'\nexec skim\n";
        assert_eq!(
            parse_binary_pin_from_script(script),
            Some("/usr/local/bin/skim".to_string())
        );
    }

    #[test]
    fn test_parse_binary_pin_from_script_with_space_in_path() {
        let script =
            "#!/usr/bin/env bash\nexport SKIM_HOOK_BINARY='/path/with spaces/skim'\nexec skim\n";
        assert_eq!(
            parse_binary_pin_from_script(script),
            Some("/path/with spaces/skim".to_string())
        );
    }

    #[test]
    fn test_parse_binary_pin_from_script_absent() {
        let script = "#!/usr/bin/env bash\nexport SKIM_HOOK_VERSION=\"2.5.1\"\nexec skim\n";
        assert_eq!(parse_binary_pin_from_script(script), None);
    }

    // ---- hook_is_current: B5c regression test ----

    /// B5c: hook_is_current() must return false when versions match, binary is
    /// pinned, but the recorded SKIM_HOOK_COMMIT differs from the compiled commit.
    ///
    /// This is the exact scenario observed live: hook pinned `ca6e756`, binary
    /// `d5695c1`, `skim init --yes` printed "Already up to date" and left the
    /// hook byte-identical.
    #[test]
    fn test_hook_is_current_commit_mismatch_returns_false() {
        let compiled_commit = option_env!("SKIM_GIT_COMMIT").unwrap_or("unknown");
        // Only meaningful when the binary has a real commit (not a tarball build).
        if compiled_commit == "unknown" {
            return; // Nothing to test without a real compile-time commit.
        }

        let state = DetectedState {
            skim_binary: std::path::PathBuf::from("/usr/local/bin/skim"),
            skim_version: env!("CARGO_PKG_VERSION").to_string(),
            config_dir: std::path::PathBuf::from("/tmp/test-config"),
            hook_config_dir: std::path::PathBuf::from("/tmp/test-config"),
            settings_path: std::path::PathBuf::from("/tmp/test-config/settings.json"),
            settings_exists: true,
            hook_installed: true,
            hook_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            // Record a DIFFERENT commit (not the compiled one)
            hook_commit: Some("aaaaaaaastale".to_string()),
            hook_binary_pin: Some("/usr/local/bin/skim".to_string()),
            hook_uses_pinned_binary: true,
            dual_scope_warning: None,
            existing_hooks: vec![],
            agent_cli_name: "claude-code",
        };

        assert!(
            !state.hook_is_current(),
            "hook_is_current() must return false when commit pin differs from compiled commit"
        );
    }

    /// B5c: hook_is_current() returns true when version, binary pin, AND commit
    /// all match the compiled binary.
    #[test]
    fn test_hook_is_current_all_matching_returns_true() {
        let compiled_commit = option_env!("SKIM_GIT_COMMIT").unwrap_or("unknown");
        // When commit is "unknown" the commit check is skipped, so version+pinned is enough.
        let hook_commit = if compiled_commit == "unknown" {
            None // test the skip-check branch
        } else {
            Some(compiled_commit.to_string()) // test the matching branch
        };

        let state = DetectedState {
            skim_binary: std::path::PathBuf::from("/usr/local/bin/skim"),
            skim_version: env!("CARGO_PKG_VERSION").to_string(),
            config_dir: std::path::PathBuf::from("/tmp/test-config"),
            hook_config_dir: std::path::PathBuf::from("/tmp/test-config"),
            settings_path: std::path::PathBuf::from("/tmp/test-config/settings.json"),
            settings_exists: true,
            hook_installed: true,
            hook_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            hook_commit,
            hook_binary_pin: Some("/usr/local/bin/skim".to_string()),
            hook_uses_pinned_binary: true,
            dual_scope_warning: None,
            existing_hooks: vec![],
            agent_cli_name: "claude-code",
        };

        // When the compiled commit is "unknown" (tarball), the commit check is
        // skipped and hook_is_current() only checks version + pinned.
        // When hook_commit is None and compiled_commit is "unknown", the function
        // should return true (no commit to compare).
        if compiled_commit == "unknown" {
            // Tarball build: commit check skipped → true (version + pinned match)
            assert!(
                state.hook_is_current(),
                "tarball build: hook_is_current() should return true when version and pinned match"
            );
        } else {
            // Real git build: all three match
            assert!(
                state.hook_is_current(),
                "hook_is_current() should return true when version, pinned, and commit all match"
            );
        }
    }

    // ---- Defect 2 regression: hook_version fallback for flat-format entries ----

    /// `extract_hook_version_from_entry` returns `None` for Cursor's flat format
    /// (top-level "command" field, no nested "hooks" array).  The fix in
    /// `detect_state` falls back to `parse_version_from_script` from the
    /// pre-read `hook_script_contents`, ensuring version is extracted from the
    /// same source as `hook_commit` and `hook_binary_pin`.
    #[test]
    fn test_extract_hook_version_from_entry_returns_none_for_cursor_flat_format() {
        // Cursor flat format: top-level "command", no nested "hooks".
        let entry = serde_json::json!({
            "matcher": "Shell",
            "command": "/path/with spaces/hooks/skim-rewrite.sh"
        });
        let dir = tempfile::TempDir::new().unwrap();
        // No nested "hooks" key → None immediately, before containment check.
        let result = extract_hook_version_from_entry(&entry, dir.path(), None);
        assert!(
            result.is_none(),
            "flat Cursor entry (no nested 'hooks' key) must yield None from extract_hook_version_from_entry"
        );
    }

    /// After `extract_hook_version_from_entry` returns `None`, the fallback to
    /// `parse_version_from_script` must resolve the version from the script text.
    /// This is the integration property the fix provides.
    #[test]
    fn test_hook_version_fallback_from_script_for_cursor_flat_format() {
        let script = "#!/usr/bin/env bash\n\
                      # skim-hook v2.11.0\n\
                      export SKIM_HOOK_VERSION=\"2.11.0\"\n\
                      export SKIM_HOOK_BINARY='/path/with spaces/target/release/skim'\n\
                      export SKIM_HOOK_COMMIT=d5695c1\n\
                      exec skim rewrite --hook --agent cursor\n";

        // extract_hook_version_from_entry returns None (flat format).
        let entry = serde_json::json!({
            "matcher": "Shell",
            "command": "/path/with spaces/hooks/skim-rewrite.sh"
        });
        let dir = tempfile::TempDir::new().unwrap();
        let ver_from_entry = extract_hook_version_from_entry(&entry, dir.path(), Some(script));
        assert!(
            ver_from_entry.is_none(),
            "flat format should still return None"
        );

        // Fallback path: parse_version_from_script succeeds.
        let ver_from_script = parse_version_from_script(script);
        assert_eq!(
            ver_from_script,
            Some("2.11.0".to_string()),
            "fallback via parse_version_from_script must resolve version from script text"
        );
    }

    /// A hook script at a path with spaces must still yield the correct version
    /// through `parse_version_from_script` (the fallback path used by the fix).
    #[test]
    fn test_hook_version_extracted_from_script_path_with_space() {
        let dir = tempfile::TempDir::new().unwrap();
        // Simulate a hooks dir whose path contains a space (e.g. ~/Library/Application Support/Cursor/hooks/).
        let hooks_dir = dir.path().join("Cursor hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let script_path = hooks_dir.join(super::super::helpers::HOOK_SCRIPT_NAME);
        let script_contents = "#!/usr/bin/env bash\n\
                               export SKIM_HOOK_VERSION=\"2.11.0\"\n\
                               export SKIM_HOOK_COMMIT=d5695c1\n\
                               export SKIM_HOOK_BINARY='/path/with spaces/target/release/skim'\n\
                               exec skim rewrite --hook --agent cursor\n";
        std::fs::write(&script_path, script_contents).unwrap();

        let contents = std::fs::read_to_string(&script_path).unwrap();

        // Version from script.
        let ver = parse_version_from_script(&contents);
        assert_eq!(
            ver,
            Some("2.11.0".to_string()),
            "version must be extracted from a script at a path with a space"
        );

        // Commit from script.
        let commit = parse_commit_from_script(&contents);
        assert_eq!(
            commit,
            Some("d5695c1".to_string()),
            "commit must be extracted from a script at a path with a space"
        );

        // Binary pin from script.
        let pin = parse_binary_pin_from_script(&contents);
        assert_eq!(
            pin,
            Some("/path/with spaces/target/release/skim".to_string()),
            "binary pin must be extracted from a script at a path with a space"
        );
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

    // ---- pin_is_current ----

    fn make_state_with_pin(pin: Option<String>) -> DetectedState {
        DetectedState {
            skim_binary: std::path::PathBuf::from("/usr/local/bin/skim"),
            skim_version: env!("CARGO_PKG_VERSION").to_string(),
            config_dir: std::path::PathBuf::from("/tmp/test"),
            hook_config_dir: std::path::PathBuf::from("/tmp/test"),
            settings_path: std::path::PathBuf::from("/tmp/test/settings.json"),
            settings_exists: false,
            hook_installed: true,
            hook_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            hook_commit: None,
            hook_binary_pin: pin,
            hook_uses_pinned_binary: true,
            dual_scope_warning: None,
            existing_hooks: vec![],
            agent_cli_name: "claude-code",
        }
    }

    #[test]
    fn test_pin_is_current_no_pin_returns_false() {
        let state = make_state_with_pin(None);
        assert!(
            !state.pin_is_current(),
            "absent pin must return false (treat as stale)"
        );
    }

    #[test]
    fn test_pin_is_current_wrong_path_returns_false() {
        // A path that doesn't match the running binary must return false.
        let state = make_state_with_pin(Some("/definitely/not/the/running/binary".to_string()));
        assert!(
            !state.pin_is_current(),
            "a non-matching pin must return false"
        );
    }

    #[test]
    fn test_pin_is_current_matching_path_returns_true() {
        // Use the actual running binary path so the comparison succeeds.
        let running = std::env::current_exe()
            .ok()
            .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)));
        let Some(running_path) = running else {
            return; // cannot determine running binary in this environment — skip
        };
        let state = make_state_with_pin(Some(running_path.to_string_lossy().to_string()));
        assert!(
            state.pin_is_current(),
            "pin matching the running binary must return true"
        );
    }
}
