//! State detection for `skim init` (B5).

use std::path::{Path, PathBuf};

use super::flags::InitFlags;
use super::helpers::{resolve_config_dir, resolve_config_dir_for_agent, HOOK_SCRIPT_NAME, SETTINGS_FILE};

/// Maximum settings.json size we'll read (10 MB). Anything larger is almost
/// certainly not a real Claude Code settings file and could cause OOM.
pub(super) const MAX_SETTINGS_SIZE: u64 = 10 * 1024 * 1024;

pub(super) struct DetectedState {
    pub(super) skim_binary: PathBuf,
    pub(super) skim_version: String,
    pub(super) config_dir: PathBuf,
    pub(super) settings_path: PathBuf,
    pub(super) settings_exists: bool,
    pub(super) hook_installed: bool,
    pub(super) hook_version: Option<String>,
    pub(super) marketplace_installed: bool,
    /// If installing to one scope and the other scope also has a hook
    pub(super) dual_scope_warning: Option<String>,
    /// Existing non-skim Bash PreToolUse hooks (plugin collision detection)
    pub(super) existing_bash_hooks: Vec<String>,
}

pub(super) fn detect_state(flags: &InitFlags) -> anyhow::Result<DetectedState> {
    let skim_binary = std::env::current_exe()?;
    let skim_version = env!("CARGO_PKG_VERSION").to_string();
    let config_dir = resolve_config_dir_for_agent(flags.project, flags.agent)?;
    let settings_path = config_dir.join(SETTINGS_FILE);
    let settings_exists = settings_path.exists();

    let mut hook_installed = false;
    let mut hook_version = None;
    let mut marketplace_installed = false;

    if let Some(json) = read_settings_json(&settings_path) {
        if let Some(arr) = json
            .get("hooks")
            .and_then(|h| h.get("PreToolUse"))
            .and_then(|v| v.as_array())
        {
            for entry in arr {
                if has_skim_hook_entry(entry) {
                    hook_installed = true;
                    hook_version = extract_hook_version_from_entry(entry, &config_dir);
                }
            }
        }
        if json
            .get("extraKnownMarketplaces")
            .and_then(|m| m.get("skim"))
            .is_some()
        {
            marketplace_installed = true;
        }
    }

    // Scan for existing non-skim Bash PreToolUse hooks (plugin collision detection)
    let existing_bash_hooks = scan_existing_bash_hooks(&settings_path);

    // Dual-scope check (B5)
    let dual_scope_warning = check_dual_scope(flags)?;

    Ok(DetectedState {
        skim_binary,
        skim_version,
        config_dir,
        settings_path,
        settings_exists,
        hook_installed,
        hook_version,
        marketplace_installed,
        dual_scope_warning,
        existing_bash_hooks,
    })
}

/// Scan settings.json for existing non-skim Bash PreToolUse hooks.
///
/// Returns the command strings of any Bash-matcher entries that are NOT skim entries.
/// Used for plugin collision detection — warns the user if another tool is also
/// intercepting Bash commands.
fn scan_existing_bash_hooks(settings_path: &Path) -> Vec<String> {
    let json = match read_settings_json(settings_path) {
        Some(j) => j,
        None => return Vec::new(),
    };

    let entries = match json
        .get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(|ptu| ptu.as_array())
    {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    let mut other_hooks = Vec::new();
    for entry in entries {
        // Only care about "Bash" matcher entries
        let is_bash_matcher = entry
            .get("matcher")
            .and_then(|m| m.as_str())
            .is_some_and(|m| m == "Bash");
        if !is_bash_matcher {
            continue;
        }
        // Skip skim entries
        if has_skim_hook_entry(entry) {
            continue;
        }
        // Extract command strings for reporting
        if let Some(hooks) = entry.get("hooks").and_then(|h| h.as_array()) {
            for hook in hooks {
                if let Some(cmd) = hook.get("command").and_then(|c| c.as_str()) {
                    other_hooks.push(cmd.to_string());
                }
            }
        }
    }

    other_hooks
}

pub(super) fn check_dual_scope(flags: &InitFlags) -> anyhow::Result<Option<String>> {
    let other_dir = if flags.project {
        // Installing project-level, check global
        resolve_config_dir(false)?
    } else {
        // Installing global, check project
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(".claude"),
            Err(_) => return Ok(None),
        }
    };

    let other_settings = other_dir.join(SETTINGS_FILE);
    let has_hook = read_settings_json(&other_settings)
        .and_then(|json| {
            json.get("hooks")?
                .get("PreToolUse")?
                .as_array()
                .map(|arr| arr.iter().any(has_skim_hook_entry))
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

/// Check if a PreToolUse entry contains a skim hook (substring match on "skim-rewrite").
pub(super) fn has_skim_hook_entry(entry: &serde_json::Value) -> bool {
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

/// Try to extract the skim version from the hook script referenced in a settings entry.
///
/// SECURITY: Validates that the resolved script path is within the expected
/// `{config_dir}/hooks/` directory to prevent arbitrary file reads via
/// attacker-controlled settings.json in `--project` mode.
pub(super) fn extract_hook_version_from_entry(
    entry: &serde_json::Value,
    config_dir: &Path,
) -> Option<String> {
    let hooks_dir = config_dir.join("hooks");
    let hooks = entry.get("hooks")?.as_array()?;
    for hook in hooks {
        let cmd = hook.get("command")?.as_str()?;
        if cmd.contains("skim-rewrite") {
            // Try reading the script file
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

            if let Ok(contents) = std::fs::read_to_string(&canonical) {
                for line in contents.lines() {
                    if let Some(ver) = line.strip_prefix("# skim-hook v").or_else(|| {
                        line.strip_prefix("export SKIM_HOOK_VERSION=\"")
                            .and_then(|s| s.strip_suffix('"'))
                    }) {
                        return Some(ver.to_string());
                    }
                }
            }
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

    #[test]
    fn test_scan_existing_bash_hooks_empty_settings() {
        let dir = tempfile::TempDir::new().unwrap();
        let settings_path = dir.path().join("settings.json");

        // No file at all
        let result = scan_existing_bash_hooks(&settings_path);
        assert!(result.is_empty());
    }

    #[test]
    fn test_scan_existing_bash_hooks_no_other_hooks() {
        let dir = tempfile::TempDir::new().unwrap();
        let settings_path = dir.path().join("settings.json");

        // Only skim hook
        let settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": "/home/.claude/hooks/skim-rewrite.sh"}]
                }]
            }
        });
        std::fs::write(&settings_path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let result = scan_existing_bash_hooks(&settings_path);
        assert!(result.is_empty(), "skim entries should be excluded");
    }

    #[test]
    fn test_scan_existing_bash_hooks_detects_other_bash_hook() {
        let dir = tempfile::TempDir::new().unwrap();
        let settings_path = dir.path().join("settings.json");

        // Settings with both skim and another Bash hook
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
        std::fs::write(&settings_path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let result = scan_existing_bash_hooks(&settings_path);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], "/usr/bin/other-security-hook");
    }

    #[test]
    fn test_scan_existing_bash_hooks_ignores_non_bash_matchers() {
        let dir = tempfile::TempDir::new().unwrap();
        let settings_path = dir.path().join("settings.json");

        // A non-Bash matcher should be ignored
        let settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Edit",
                    "hooks": [{"type": "command", "command": "/usr/bin/some-hook"}]
                }]
            }
        });
        std::fs::write(&settings_path, serde_json::to_string_pretty(&settings).unwrap()).unwrap();

        let result = scan_existing_bash_hooks(&settings_path);
        assert!(result.is_empty(), "non-Bash matchers should be ignored");
    }
}
