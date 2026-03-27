//! Agent detection logic for the `skim agents` subcommand.

use std::path::{Path, PathBuf};

use crate::cmd::init::MAX_SETTINGS_SIZE;
use crate::cmd::session::AgentKind;

use super::types::{AgentStatus, HookStatus, RulesInfo, SessionInfo};
use super::util::{count_files_in_dir, count_files_recursive, dir_size_human, tilde_path};

/// Detect all supported agents and return their status.
pub(super) fn detect_all_agents() -> Vec<AgentStatus> {
    let home = dirs::home_dir();
    AgentKind::all_supported()
        .iter()
        .copied()
        .map(|kind| detect_agent(kind, home.as_deref()))
        .collect()
}

/// Detect a single agent's status.
fn detect_agent(kind: AgentKind, home: Option<&Path>) -> AgentStatus {
    match kind {
        AgentKind::ClaudeCode => detect_claude_code(home),
        AgentKind::Cursor => detect_cursor(home),
        AgentKind::CodexCli => detect_codex_cli(home),
        AgentKind::GeminiCli => detect_gemini_cli(home),
        AgentKind::CopilotCli => detect_copilot_cli(),
        AgentKind::OpenCode => detect_opencode(),
    }
}

fn detect_claude_code(home: Option<&Path>) -> AgentStatus {
    let projects_dir = std::env::var("SKIM_PROJECTS_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| home.map(|h| AgentKind::ClaudeCode.config_dir(h).join("projects")));

    let detected = projects_dir.as_ref().is_some_and(|p| p.is_dir());

    let sessions = if detected {
        projects_dir.as_ref().map(|p| {
            let count = count_files_recursive(p, "jsonl");
            SessionInfo {
                path: tilde_path(p),
                detail: format!("{count} files"),
            }
        })
    } else {
        None
    };

    let config_dir = home.map(|h| AgentKind::ClaudeCode.config_dir(h));
    let hooks = detect_pretooluse_hook(config_dir.as_deref());

    let rules_dir = AgentKind::ClaudeCode.project_dir().join("rules");
    let rules = Some(RulesInfo {
        path: format!("{}/", rules_dir.display()),
        exists: rules_dir.is_dir(),
    });

    AgentStatus {
        kind: AgentKind::ClaudeCode,
        detected,
        sessions,
        hooks,
        rules,
    }
}

fn detect_cursor(home: Option<&Path>) -> AgentStatus {
    // config_dir() handles macOS vs Linux detection internally
    let state_path = home.and_then(|h| {
        let path = AgentKind::Cursor.config_dir(h);
        if path.is_dir() {
            Some(path)
        } else {
            None
        }
    });

    let detected = state_path.is_some();

    let sessions = state_path.as_ref().map(|p| {
        let size = dir_size_human(p);
        SessionInfo {
            path: tilde_path(p),
            detail: size,
        }
    });

    let hooks = detect_pretooluse_hook(state_path.as_deref());

    let rules_dir = AgentKind::Cursor.project_dir().join("rules");
    let rules = Some(RulesInfo {
        path: format!("{}/", rules_dir.display()),
        exists: rules_dir.is_dir(),
    });

    AgentStatus {
        kind: AgentKind::Cursor,
        detected,
        sessions,
        hooks,
        rules,
    }
}

fn detect_codex_cli(home: Option<&Path>) -> AgentStatus {
    let codex_dir = home.map(|h| AgentKind::CodexCli.config_dir(h));
    let detected = codex_dir.as_ref().is_some_and(|p| p.is_dir());

    let sessions = if detected {
        codex_dir.as_ref().and_then(|p| {
            let sessions_dir = p.join("sessions");
            if sessions_dir.is_dir() {
                let count = count_files_in_dir(&sessions_dir);
                Some(SessionInfo {
                    path: tilde_path(&sessions_dir),
                    detail: format!("{count} files"),
                })
            } else {
                None
            }
        })
    } else {
        None
    };

    // Codex CLI has experimental hook support
    let hooks = HookStatus::NotSupported {
        note: "experimental hooks only",
    };

    let rules = codex_dir.as_ref().map(|p| {
        let instructions_dir = p.join("instructions");
        RulesInfo {
            path: tilde_path(&instructions_dir),
            exists: instructions_dir.is_dir(),
        }
    });

    AgentStatus {
        kind: AgentKind::CodexCli,
        detected,
        sessions,
        hooks,
        rules,
    }
}

fn detect_gemini_cli(home: Option<&Path>) -> AgentStatus {
    let gemini_dir = home.map(|h| AgentKind::GeminiCli.config_dir(h));
    let detected = gemini_dir.as_ref().is_some_and(|p| p.is_dir());

    let sessions = None; // Gemini CLI doesn't persist session files locally

    // Gemini CLI supports BeforeTool/AfterTool hooks
    let hooks = if detected {
        let has_hook = gemini_dir
            .as_ref()
            .and_then(|p| read_settings_guarded(&p.join("settings.json")))
            .is_some_and(|v| has_skim_hook_in_settings(&v));
        if has_hook {
            HookStatus::Installed {
                version: None,
                integrity: "ok",
            }
        } else {
            HookStatus::NotInstalled
        }
    } else {
        HookStatus::NotInstalled
    };

    let rules = gemini_dir.as_ref().map(|p| {
        let settings = p.join("settings.json");
        RulesInfo {
            path: tilde_path(&settings),
            exists: settings.is_file(),
        }
    });

    AgentStatus {
        kind: AgentKind::GeminiCli,
        detected,
        sessions,
        hooks,
        rules,
    }
}

/// Maximum number of directory entries to scan in `detect_copilot_cli`
/// to prevent unbounded I/O on adversarial `.github/hooks/` directories.
const MAX_COPILOT_HOOK_ENTRIES: usize = 50;

fn detect_copilot_cli() -> AgentStatus {
    // Copilot CLI uses .github/hooks/ for hook configuration
    let hooks_dir = AgentKind::CopilotCli.project_dir().join("hooks");
    let detected = hooks_dir.is_dir();

    let sessions = None; // Copilot CLI sessions are cloud-managed

    let hooks = if detected {
        let has_skim_hook = std::fs::read_dir(hooks_dir).ok().is_some_and(|entries| {
            entries.flatten().take(MAX_COPILOT_HOOK_ENTRIES).any(|e| {
                let path = e.path();
                path.extension().is_some_and(|ext| ext == "json")
                    && std::fs::metadata(&path)
                        .ok()
                        .is_some_and(|m| m.len() <= MAX_SETTINGS_SIZE)
                    && std::fs::read_to_string(&path)
                        .ok()
                        .is_some_and(|c| c.contains("skim"))
            })
        });
        if has_skim_hook {
            HookStatus::Installed {
                version: None,
                integrity: "ok",
            }
        } else {
            HookStatus::NotInstalled
        }
    } else {
        HookStatus::NotInstalled
    };

    let rules = None; // Copilot uses .github/ conventions, not a separate rules dir

    AgentStatus {
        kind: AgentKind::CopilotCli,
        detected,
        sessions,
        hooks,
        rules,
    }
}

fn detect_opencode() -> AgentStatus {
    // OpenCode uses .opencode/ directory in project root
    let opencode_dir = std::env::var("SKIM_OPENCODE_DIR")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| AgentKind::OpenCode.project_dir());
    let detected = opencode_dir.is_dir();

    let sessions = if detected {
        let count = count_files_in_dir(&opencode_dir);
        Some(SessionInfo {
            path: tilde_path(&opencode_dir),
            detail: format!("{count} files"),
        })
    } else {
        None
    };

    let hooks = HookStatus::NotSupported {
        note: "TypeScript plugin model",
    };

    let rules = None; // OpenCode uses AGENTS.md, not a rules directory

    AgentStatus {
        kind: AgentKind::OpenCode,
        detected,
        sessions,
        hooks,
        rules,
    }
}

/// Read and parse a JSON settings file with a size guard.
///
/// Returns `None` if the file is missing, too large (> [`MAX_SETTINGS_SIZE`]),
/// or not valid JSON.
fn read_settings_guarded(path: &Path) -> Option<serde_json::Value> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_SETTINGS_SIZE {
        return None;
    }
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Check whether a Gemini CLI settings object contains any hook whose
/// command references "skim".
fn has_skim_hook_in_settings(settings: &serde_json::Value) -> bool {
    let hooks = match settings.get("hooks").and_then(|v| v.as_object()) {
        Some(h) => h,
        None => return false,
    };
    hooks.values().any(|arr| {
        arr.as_array().is_some_and(|entries| {
            entries.iter().any(|e| {
                e.get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|cmd| cmd.contains("skim"))
            })
        })
    })
}

/// Detect skim hook via the PreToolUse + skim-rewrite.sh pattern.
///
/// Shared by Claude Code and Cursor, which both use the same hook mechanism.
fn detect_pretooluse_hook(config_dir: Option<&Path>) -> HookStatus {
    let Some(config_dir) = config_dir else {
        return HookStatus::NotInstalled;
    };

    let settings_path = config_dir.join("settings.json");

    let json = match read_settings_guarded(&settings_path) {
        Some(v) => v,
        None => return HookStatus::NotInstalled,
    };

    // Check if hooks.PreToolUse contains a skim-rewrite entry
    let has_hook = json
        .get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(|ptu| ptu.as_array())
        .is_some_and(|entries| entries.iter().any(crate::cmd::init::has_skim_hook_entry));

    if !has_hook {
        return HookStatus::NotInstalled;
    }

    // Try to extract version from hook script
    let hook_script = config_dir.join("hooks").join("skim-rewrite.sh");
    let version = std::fs::read_to_string(&hook_script)
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                line.strip_prefix("# skim-hook v")
                    .or_else(|| {
                        line.strip_prefix("export SKIM_HOOK_VERSION=\"")
                            .and_then(|s| s.strip_suffix('"'))
                    })
                    .map(|s| s.to_string())
            })
        });

    // Check integrity using SHA-256 verification
    let integrity = if !hook_script.is_file() {
        "missing"
    } else {
        match crate::cmd::integrity::verify_script_integrity(
            config_dir,
            "claude-code",
            &hook_script,
        ) {
            Ok(true) => "ok",
            Ok(false) => "tampered",
            Err(_) => "unknown",
        }
    };

    HookStatus::Installed { version, integrity }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_all_agents_returns_all_kinds() {
        let agents = detect_all_agents();
        assert_eq!(agents.len(), AgentKind::all_supported().len());
        for kind in AgentKind::all_supported() {
            assert!(
                agents.iter().any(|a| a.kind == *kind),
                "missing agent kind: {:?}",
                kind
            );
        }
    }

    #[test]
    fn test_detect_pretooluse_hook_integrity_ok() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = dir.path();
        let hooks_dir = config.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();

        let settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": hooks_dir.join("skim-rewrite.sh").to_str().unwrap()}]
                }]
            }
        });
        std::fs::write(
            config.join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        let script_path = hooks_dir.join("skim-rewrite.sh");
        std::fs::write(
            &script_path,
            "#!/usr/bin/env bash\n# skim-hook v1.0.0\nexec skim rewrite --hook\n",
        )
        .unwrap();
        let hash = crate::cmd::integrity::compute_file_hash(&script_path).unwrap();
        crate::cmd::integrity::write_hash_manifest(config, "claude-code", "skim-rewrite.sh", &hash)
            .unwrap();

        let status = detect_pretooluse_hook(Some(config));
        match status {
            HookStatus::Installed { integrity, .. } => {
                assert_eq!(
                    integrity, "ok",
                    "integrity should be 'ok' for valid script+hash"
                );
            }
            other => panic!("expected HookStatus::Installed, got: {other:?}"),
        }
    }

    #[test]
    fn test_detect_pretooluse_hook_integrity_tampered() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = dir.path();
        let hooks_dir = config.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();

        let settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": hooks_dir.join("skim-rewrite.sh").to_str().unwrap()}]
                }]
            }
        });
        std::fs::write(
            config.join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        let script_path = hooks_dir.join("skim-rewrite.sh");
        std::fs::write(
            &script_path,
            "#!/usr/bin/env bash\n# skim-hook v1.0.0\nexec skim rewrite --hook\n",
        )
        .unwrap();
        let hash = crate::cmd::integrity::compute_file_hash(&script_path).unwrap();
        crate::cmd::integrity::write_hash_manifest(config, "claude-code", "skim-rewrite.sh", &hash)
            .unwrap();

        // Tamper with the script
        std::fs::write(&script_path, "#!/usr/bin/env bash\necho HACKED\n").unwrap();

        let status = detect_pretooluse_hook(Some(config));
        match status {
            HookStatus::Installed { integrity, .. } => {
                assert_eq!(
                    integrity, "tampered",
                    "integrity should be 'tampered' for modified script"
                );
            }
            other => panic!("expected HookStatus::Installed, got: {other:?}"),
        }
    }

    #[test]
    fn test_detect_pretooluse_hook_integrity_missing_script() {
        let dir = tempfile::TempDir::new().unwrap();
        let config = dir.path();
        let hooks_dir = config.join("hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();

        let settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Bash",
                    "hooks": [{"type": "command", "command": hooks_dir.join("skim-rewrite.sh").to_str().unwrap()}]
                }]
            }
        });
        std::fs::write(
            config.join("settings.json"),
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        let status = detect_pretooluse_hook(Some(config));
        match status {
            HookStatus::Installed { integrity, .. } => {
                assert_eq!(
                    integrity, "missing",
                    "integrity should be 'missing' for absent script"
                );
            }
            other => panic!("expected HookStatus::Installed, got: {other:?}"),
        }
    }

    #[test]
    fn test_has_skim_hook_in_settings_true() {
        let settings = serde_json::json!({
            "hooks": {
                "BeforeTool": [{
                    "command": "/usr/local/bin/skim rewrite --hook"
                }]
            }
        });
        assert!(has_skim_hook_in_settings(&settings));
    }

    #[test]
    fn test_has_skim_hook_in_settings_false() {
        let settings = serde_json::json!({
            "hooks": {
                "BeforeTool": [{
                    "command": "/usr/local/bin/other-tool"
                }]
            }
        });
        assert!(!has_skim_hook_in_settings(&settings));
    }

    #[test]
    fn test_has_skim_hook_in_settings_no_hooks() {
        let settings = serde_json::json!({ "theme": "dark" });
        assert!(!has_skim_hook_in_settings(&settings));
    }

    #[test]
    fn test_read_settings_guarded_rejects_oversized() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("big.json");
        let data = vec![b' '; (MAX_SETTINGS_SIZE as usize) + 1];
        std::fs::write(&path, data).unwrap();
        assert!(read_settings_guarded(&path).is_none());
    }

    #[test]
    fn test_read_settings_guarded_valid() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("ok.json");
        std::fs::write(&path, r#"{"key":"value"}"#).unwrap();
        let v = read_settings_guarded(&path);
        assert!(v.is_some());
        assert_eq!(v.unwrap().get("key").unwrap().as_str().unwrap(), "value");
    }
}
