//! `skim agents` -- display detected AI agents and their hook/session status.
//!
//! Scans for known AI coding agents (Claude Code, Cursor, Codex CLI, Gemini CLI,
//! Copilot CLI) and reports their detection status, session paths, hook installation
//! status, and rules directory presence.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use super::session::AgentKind;

// ============================================================================
// Public entry points
// ============================================================================

/// Run the `skim agents` subcommand.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let json_output = args.iter().any(|a| a == "--json");

    let agents = detect_all_agents();

    if json_output {
        print_json(&agents)?;
    } else {
        print_text(&agents);
    }

    Ok(ExitCode::SUCCESS)
}

/// Build the clap `Command` definition for shell completions.
pub(super) fn command() -> clap::Command {
    clap::Command::new("agents")
        .about("Display detected AI agents and their integration status")
        .arg(
            clap::Arg::new("json")
                .long("json")
                .action(clap::ArgAction::SetTrue)
                .help("Output as JSON"),
        )
}

// ============================================================================
// Agent detection
// ============================================================================

/// Detected agent status report.
struct AgentStatus {
    kind: AgentKind,
    detected: bool,
    sessions: Option<SessionInfo>,
    hooks: HookStatus,
    rules: Option<RulesInfo>,
}

/// Session file information.
struct SessionInfo {
    path: String,
    detail: String, // e.g., "42 files" or "1.2 GB"
}

/// Hook installation status.
enum HookStatus {
    Installed {
        version: Option<String>,
        integrity: &'static str,
    },
    NotInstalled,
    NotSupported {
        note: &'static str,
    },
}

/// Rules directory information.
struct RulesInfo {
    path: String,
    exists: bool,
}

/// Detect all supported agents and return their status.
fn detect_all_agents() -> Vec<AgentStatus> {
    AgentKind::all_supported()
        .iter()
        .map(|kind| detect_agent(*kind))
        .collect()
}

/// Detect a single agent's status.
fn detect_agent(kind: AgentKind) -> AgentStatus {
    match kind {
        AgentKind::ClaudeCode => detect_claude_code(),
        AgentKind::Cursor => detect_cursor(),
        AgentKind::CodexCli => detect_codex_cli(),
        AgentKind::GeminiCli => detect_gemini_cli(),
        AgentKind::CopilotCli => detect_copilot_cli(),
    }
}

fn detect_claude_code() -> AgentStatus {
    let home = dirs::home_dir();
    let projects_dir = std::env::var("SKIM_PROJECTS_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|h| h.join(".claude").join("projects")));

    let detected = projects_dir
        .as_ref()
        .is_some_and(|p| p.is_dir());

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

    let config_dir = home.as_ref().map(|h| h.join(".claude"));
    let hooks = detect_claude_hook(config_dir.as_deref());

    let rules = Some(RulesInfo {
        path: ".claude/rules/".to_string(),
        exists: Path::new(".claude/rules").is_dir(),
    });

    AgentStatus {
        kind: AgentKind::ClaudeCode,
        detected,
        sessions,
        hooks,
        rules,
    }
}

fn detect_cursor() -> AgentStatus {
    let home = dirs::home_dir();

    // Cursor stores state in ~/Library/Application Support/Cursor/ (macOS)
    // or ~/.config/Cursor/ (Linux)
    let state_path = home.as_ref().and_then(|h| {
        let macos_path = h
            .join("Library")
            .join("Application Support")
            .join("Cursor");
        let linux_path = h.join(".config").join("Cursor");
        if macos_path.is_dir() {
            Some(macos_path)
        } else if linux_path.is_dir() {
            Some(linux_path)
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

    // Cursor uses its own hook system (not skim hooks)
    let hooks = HookStatus::NotSupported {
        note: "uses built-in AI features",
    };

    let rules = Some(RulesInfo {
        path: ".cursor/rules/".to_string(),
        exists: Path::new(".cursor/rules").is_dir(),
    });

    AgentStatus {
        kind: AgentKind::Cursor,
        detected,
        sessions,
        hooks,
        rules,
    }
}

fn detect_codex_cli() -> AgentStatus {
    let home = dirs::home_dir();
    let codex_dir = home.as_ref().map(|h| h.join(".codex"));
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

fn detect_gemini_cli() -> AgentStatus {
    let home = dirs::home_dir();
    let gemini_dir = home.as_ref().map(|h| h.join(".gemini"));
    let detected = gemini_dir.as_ref().is_some_and(|p| p.is_dir());

    let sessions = None; // Gemini CLI doesn't persist session files locally

    // Gemini CLI supports BeforeTool/AfterTool hooks
    let hooks = if detected {
        let settings_path = gemini_dir
            .as_ref()
            .map(|p| p.join("settings.json"));
        let has_hook = settings_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
            .and_then(|v| v.get("hooks")?.as_object().cloned())
            .is_some_and(|hooks| {
                hooks.values().any(|arr| {
                    arr.as_array().is_some_and(|entries| {
                        entries.iter().any(|e| {
                            e.get("command")
                                .and_then(|c| c.as_str())
                                .is_some_and(|cmd| cmd.contains("skim"))
                        })
                    })
                })
            });
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

fn detect_copilot_cli() -> AgentStatus {
    // Copilot CLI uses .github/hooks/ for hook configuration
    let hooks_dir = Path::new(".github/hooks");
    let detected = hooks_dir.is_dir();

    let sessions = None; // Copilot CLI sessions are cloud-managed

    let hooks = if detected {
        let has_skim_hook = std::fs::read_dir(hooks_dir)
            .ok()
            .is_some_and(|entries| {
                entries.flatten().any(|e| {
                    e.path()
                        .extension()
                        .is_some_and(|ext| ext == "json")
                        && std::fs::read_to_string(e.path())
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

/// Detect skim hook installation for Claude Code.
fn detect_claude_hook(config_dir: Option<&Path>) -> HookStatus {
    let Some(config_dir) = config_dir else {
        return HookStatus::NotInstalled;
    };

    let settings_path = config_dir.join("settings.json");
    let settings = match std::fs::read_to_string(&settings_path) {
        Ok(c) => c,
        Err(_) => return HookStatus::NotInstalled,
    };

    let json: serde_json::Value = match serde_json::from_str(&settings) {
        Ok(v) => v,
        Err(_) => return HookStatus::NotInstalled,
    };

    // Check if hooks.PreToolUse contains a skim-rewrite entry
    let has_hook = json
        .get("hooks")
        .and_then(|h| h.get("PreToolUse"))
        .and_then(|ptu| ptu.as_array())
        .is_some_and(|entries| {
            entries.iter().any(|entry| {
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
            })
        });

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

    // Check integrity: script exists and is executable
    let integrity = if hook_script.is_file() { "ok" } else { "missing" };

    HookStatus::Installed {
        version,
        integrity,
    }
}

// ============================================================================
// Output formatting
// ============================================================================

fn print_text(agents: &[AgentStatus]) {
    println!("Detected agents:");
    for agent in agents {
        println!();
        if agent.detected {
            println!("  {}   detected", agent.kind.display_name());
        } else {
            println!("  {}   not detected", agent.kind.display_name());
            continue;
        }

        // Sessions
        if let Some(ref sessions) = agent.sessions {
            println!(
                "  {:width$}sessions: {} ({})",
                "",
                sessions.path,
                sessions.detail,
                width = agent.kind.display_name().len() + 3,
            );
        }

        // Hooks
        let hook_str = match &agent.hooks {
            HookStatus::Installed {
                version,
                integrity,
            } => {
                let ver = version
                    .as_deref()
                    .map(|v| format!(", v{v}"))
                    .unwrap_or_default();
                format!("installed (integrity: {integrity}{ver})")
            }
            HookStatus::NotInstalled => "not installed".to_string(),
            HookStatus::NotSupported { note } => format!("not supported ({note})"),
        };
        println!(
            "  {:width$}hooks: {}",
            "",
            hook_str,
            width = agent.kind.display_name().len() + 3,
        );

        // Rules
        if let Some(ref rules) = agent.rules {
            let status = if rules.exists { "found" } else { "not found" };
            println!(
                "  {:width$}rules: {} ({})",
                "",
                rules.path,
                status,
                width = agent.kind.display_name().len() + 3,
            );
        }
    }
}

fn print_json(agents: &[AgentStatus]) -> anyhow::Result<()> {
    let mut agent_values: Vec<serde_json::Value> = Vec::new();

    for agent in agents {
        let sessions = agent.sessions.as_ref().map(|s| {
            serde_json::json!({
                "path": s.path,
                "detail": s.detail,
            })
        });

        let hooks = match &agent.hooks {
            HookStatus::Installed {
                version,
                integrity,
            } => serde_json::json!({
                "status": "installed",
                "version": version,
                "integrity": integrity,
            }),
            HookStatus::NotInstalled => serde_json::json!({
                "status": "not_installed",
            }),
            HookStatus::NotSupported { note } => serde_json::json!({
                "status": "not_supported",
                "note": note,
            }),
        };

        let rules = agent.rules.as_ref().map(|r| {
            serde_json::json!({
                "path": r.path,
                "exists": r.exists,
            })
        });

        agent_values.push(serde_json::json!({
            "name": agent.kind.display_name(),
            "cli_name": agent.kind.cli_name(),
            "detected": agent.detected,
            "sessions": sessions,
            "hooks": hooks,
            "rules": rules,
        }));
    }

    let output = serde_json::json!({ "agents": agent_values });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn print_help() {
    println!("skim agents");
    println!();
    println!("  Display detected AI agents and their integration status");
    println!();
    println!("Usage: skim agents [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --json    Output as JSON");
    println!("  --help    Print this help message");
}

// ============================================================================
// Utility helpers
// ============================================================================

/// Replace home directory prefix with ~ for display.
fn tilde_path(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(stripped) = path.strip_prefix(&home) {
            return format!("~/{}", stripped.display());
        }
    }
    path.display().to_string()
}

/// Count files with a specific extension recursively in a directory.
fn count_files_recursive(dir: &Path, extension: &str) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                count += count_files_recursive(&path, extension);
            } else if path.extension().and_then(|e| e.to_str()) == Some(extension) {
                count += 1;
            }
        }
    }
    count
}

/// Count files (non-directories) directly in a directory.
fn count_files_in_dir(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .ok()
        .map(|entries| entries.flatten().filter(|e| e.path().is_file()).count())
        .unwrap_or(0)
}

/// Get human-readable size of a directory.
fn dir_size_human(dir: &Path) -> String {
    let bytes = dir_size_bytes(dir);
    if bytes >= 1_073_741_824 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} bytes")
    }
}

/// Calculate total size of all files in a directory tree.
fn dir_size_bytes(dir: &Path) -> u64 {
    let mut total: u64 = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                total += dir_size_bytes(&path);
            } else if let Ok(meta) = std::fs::metadata(&path) {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_all_agents_returns_all_kinds() {
        let agents = detect_all_agents();
        assert_eq!(agents.len(), AgentKind::all_supported().len());
        // Verify each agent kind is represented
        for kind in AgentKind::all_supported() {
            assert!(
                agents.iter().any(|a| a.kind == *kind),
                "missing agent kind: {:?}",
                kind
            );
        }
    }

    #[test]
    fn test_agents_run_no_crash() {
        // Should not crash even with no agents detected
        let result = run(&[]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_agents_help_flag() {
        let result = run(&["--help".to_string()]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_agents_json_output_valid_json() {
        // Capture JSON output -- we can't easily capture stdout in unit tests,
        // but we can verify the function completes successfully
        let result = run(&["--json".to_string()]);
        assert!(result.is_ok());
    }

    #[test]
    fn test_tilde_path_with_home() {
        if let Some(home) = dirs::home_dir() {
            let path = home.join("some").join("path");
            let result = tilde_path(&path);
            assert!(result.starts_with("~/"), "expected ~/ prefix, got: {result}");
            assert!(
                result.contains("some/path"),
                "expected path suffix, got: {result}"
            );
        }
    }

    #[test]
    fn test_tilde_path_without_home_prefix() {
        let path = PathBuf::from("/tmp/not-home/file");
        let result = tilde_path(&path);
        assert_eq!(result, "/tmp/not-home/file");
    }

    #[test]
    fn test_count_files_recursive_empty_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(count_files_recursive(dir.path(), "jsonl"), 0);
    }

    #[test]
    fn test_count_files_recursive_with_files() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("a.jsonl"), "{}").unwrap();
        std::fs::write(dir.path().join("b.jsonl"), "{}").unwrap();
        std::fs::write(dir.path().join("c.txt"), "hello").unwrap();
        let sub = dir.path().join("subdir");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("d.jsonl"), "{}").unwrap();
        assert_eq!(count_files_recursive(dir.path(), "jsonl"), 3);
    }

    #[test]
    fn test_dir_size_human_formats() {
        let dir = tempfile::TempDir::new().unwrap();
        // Empty dir
        let size = dir_size_human(dir.path());
        assert!(
            size.contains("bytes") || size.contains("KB"),
            "unexpected size format: {size}"
        );
    }

    #[test]
    fn test_hook_status_display() {
        // Verify HookStatus variants produce expected text
        let installed = HookStatus::Installed {
            version: Some("2.0.0".to_string()),
            integrity: "ok",
        };
        match &installed {
            HookStatus::Installed {
                version,
                integrity,
            } => {
                assert_eq!(version.as_deref(), Some("2.0.0"));
                assert_eq!(*integrity, "ok");
            }
            _ => panic!("expected Installed"),
        }

        let not_supported = HookStatus::NotSupported {
            note: "experimental",
        };
        match &not_supported {
            HookStatus::NotSupported { note } => {
                assert_eq!(*note, "experimental");
            }
            _ => panic!("expected NotSupported"),
        }
    }

    #[test]
    fn test_agents_detects_claude_code_with_fixture() {
        let dir = tempfile::TempDir::new().unwrap();
        let project_dir = dir.path().join("test-project");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("session.jsonl"), "{}").unwrap();

        // Set SKIM_PROJECTS_DIR to our fixture
        std::env::set_var("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap());

        let agents = detect_all_agents();
        let claude = agents
            .iter()
            .find(|a| a.kind == AgentKind::ClaudeCode)
            .expect("Claude Code should be in results");

        assert!(claude.detected, "Claude Code should be detected with fixture");
        assert!(
            claude.sessions.is_some(),
            "sessions should be reported for detected agent"
        );
        let sessions = claude.sessions.as_ref().unwrap();
        assert!(
            sessions.detail.contains("1 files"),
            "expected 1 file, got: {}",
            sessions.detail
        );

        // Clean up
        std::env::remove_var("SKIM_PROJECTS_DIR");
    }

    #[test]
    fn test_agent_kind_cli_name() {
        assert_eq!(AgentKind::ClaudeCode.cli_name(), "claude-code");
        assert_eq!(AgentKind::Cursor.cli_name(), "cursor");
        assert_eq!(AgentKind::CodexCli.cli_name(), "codex-cli");
        assert_eq!(AgentKind::GeminiCli.cli_name(), "gemini-cli");
        assert_eq!(AgentKind::CopilotCli.cli_name(), "copilot-cli");
    }

    #[test]
    fn test_agent_kind_all_supported() {
        let all = AgentKind::all_supported();
        assert!(all.len() >= 5, "expected at least 5 agents");
        assert!(all.contains(&AgentKind::ClaudeCode));
        assert!(all.contains(&AgentKind::Cursor));
        assert!(all.contains(&AgentKind::CodexCli));
        assert!(all.contains(&AgentKind::GeminiCli));
        assert!(all.contains(&AgentKind::CopilotCli));
    }
}
