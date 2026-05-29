//! Flag parsing for `skim init`.

use crate::cmd::session::AgentKind;

/// Parsed command-line flags for the init subcommand.
///
/// All fields are `Copy`-friendly primitive types so that the multi-agent
/// install loop can create per-agent `InitFlags` values using struct-update
/// syntax (`InitFlags { agent: Some(agent), ..flags }`) without cloning.
#[derive(Debug, Clone, Copy)]
pub(super) struct InitFlags {
    pub(super) project: bool,
    /// Accepted for backward compatibility (no-op for install, still used by uninstall).
    pub(super) yes: bool,
    pub(super) dry_run: bool,
    pub(super) uninstall: bool,
    pub(super) force: bool,
    pub(super) no_guidance: bool,
    /// Target agent for installation.
    ///
    /// `None` means auto-detect: scan installed agents and install to the first one found.
    /// Defaults to `None` when `--agent` is not supplied.
    /// Resolved to a concrete `AgentKind` by `resolve_agent()` before use.
    pub(super) agent: Option<AgentKind>,
    /// Whether to install/uninstall shell wrappers in `~/.skim/bin/`.
    ///
    /// `Some(true)` — `--wrappers` flag: install wrappers.
    /// `Some(false)` — `--no-wrappers` flag: skip wrappers.
    /// `None` — neither flag: prompt interactively (or default to false in non-TTY).
    pub(super) wrappers: Option<bool>,
}

/// Resolve a single explicit agent from flags, or `None` for auto-detect mode.
///
/// Returns `Some(kind)` when `--agent` was supplied explicitly.
/// Returns `None` when no `--agent` flag was given (auto-detect mode).
pub(super) fn resolve_single_agent(flags: &InitFlags) -> Option<AgentKind> {
    flags.agent
}

/// Injected environment values for [`detect_installed_agents`].
///
/// Created once at the CLI boundary and threaded to callers, eliminating
/// per-call env-var reads and enabling race-free unit testing. Mirrors the
/// [`crate::cmd::session::InstructionEnv`] pattern used for instruction files.
///
/// ARCHITECTURE: `from_process()` reads env exactly once at the system
/// boundary. Test code constructs this struct directly with controlled paths.
#[derive(Debug, Default)]
pub(super) struct DetectionEnv {
    pub(super) home_dir: Option<std::path::PathBuf>,
    /// `CLAUDE_CONFIG_DIR` override
    pub(super) claude_config_dir: Option<std::path::PathBuf>,
    /// `CURSOR_CONFIG_DIR` override
    pub(super) cursor_config_dir: Option<std::path::PathBuf>,
    /// `GEMINI_CONFIG_DIR` override
    pub(super) gemini_config_dir: Option<std::path::PathBuf>,
    /// `COPILOT_CONFIG_DIR` override
    pub(super) copilot_config_dir: Option<std::path::PathBuf>,
    /// `CODEX_CONFIG_DIR` override
    pub(super) codex_config_dir: Option<std::path::PathBuf>,
    /// `CRUSH_CONFIG_DIR` override
    pub(super) crush_config_dir: Option<std::path::PathBuf>,
}

impl DetectionEnv {
    /// Read env once at the system boundary.
    ///
    /// Call this in `main`-adjacent code, then thread the struct down to
    /// callers — never call from within library functions.
    pub(super) fn from_process() -> Self {
        let read = |name: &str| std::env::var_os(name).map(std::path::PathBuf::from);
        Self {
            home_dir: dirs::home_dir(),
            claude_config_dir: read("CLAUDE_CONFIG_DIR"),
            cursor_config_dir: read("CURSOR_CONFIG_DIR"),
            gemini_config_dir: read("GEMINI_CONFIG_DIR"),
            copilot_config_dir: read("COPILOT_CONFIG_DIR"),
            codex_config_dir: read("CODEX_CONFIG_DIR"),
            crush_config_dir: read("CRUSH_CONFIG_DIR"),
        }
    }

    /// Return the per-agent config-dir override for `agent`, if any.
    ///
    /// Single enumeration point for the agent → override-field mapping.
    /// `any_override` and the filter match in [`detect_installed_agents`]
    /// both delegate here so that adding a new agent only requires one edit.
    pub(super) fn override_for(&self, agent: AgentKind) -> Option<&std::path::Path> {
        match agent {
            AgentKind::ClaudeCode => self.claude_config_dir.as_deref(),
            AgentKind::Cursor => self.cursor_config_dir.as_deref(),
            AgentKind::GeminiCli => self.gemini_config_dir.as_deref(),
            AgentKind::CopilotCli => self.copilot_config_dir.as_deref(),
            AgentKind::CodexCli => self.codex_config_dir.as_deref(),
            AgentKind::Crush => self.crush_config_dir.as_deref(),
        }
    }
}

/// Detect all agents whose config directories exist on this system.
///
/// Used in auto-detect mode (no `--agent` flag) to install/uninstall skim
/// for every agent that is currently installed.
///
/// Accepts a [`DetectionEnv`] so that callers can override per-agent config
/// directory paths without touching process-level env vars, enabling
/// race-free testing. Pass `&DetectionEnv::from_process()` at the CLI
/// boundary for production use.
///
/// When any per-agent env override is set, detection is restricted to only
/// agents with an explicit override that points to an existing directory. This
/// ensures test isolation: setting only `CLAUDE_CONFIG_DIR` restricts
/// auto-detect to Claude Code only, preserving single-agent test behaviour.
///
/// Returns an empty `Vec` when no supported agents are found.
pub(super) fn detect_installed_agents(env: &DetectionEnv) -> Vec<AgentKind> {
    let any_override = AgentKind::all_supported()
        .iter()
        .any(|&a| env.override_for(a).is_some());

    AgentKind::all_supported()
        .iter()
        .filter(|&&agent| {
            if any_override {
                // In override mode: only include agents with an explicit env path
                // that points to an existing directory.
                env.override_for(agent).map(|p| p.is_dir()).unwrap_or(false)
            } else {
                // Normal mode: detect by home-dir presence
                env.home_dir
                    .as_ref()
                    .map(|h| agent.config_dir(h).is_dir())
                    .unwrap_or(false)
            }
        })
        .copied()
        .collect()
}

/// Resolve the target agent from flags (single-agent compatibility shim).
///
/// - If `flags.agent` is `Some(kind)`, return it directly.
/// - If `None`, scan for installed agents via `env.home_dir` and return the
///   first found. Falls back to `AgentKind::ClaudeCode` when nothing is detected
///   (mirrors old default behaviour so `skim init` without `--agent` still works
///   on a clean system).
///
/// Accepts a [`DetectionEnv`] so that callers can inject a controlled home dir
/// in tests — the same DI pattern used by [`detect_installed_agents`].
///
/// Used by `run_uninstall` for single-agent uninstall. Install uses
/// `detect_installed_agents()` instead to support multi-agent auto-detect.
pub(super) fn resolve_agent(flags: &InitFlags, env: &DetectionEnv) -> AgentKind {
    if let Some(agent) = flags.agent {
        return agent;
    }

    // Auto-detect: pick the first agent whose config directory exists.
    for &agent in AgentKind::all_supported() {
        if let Some(ref h) = env.home_dir {
            let config_dir = agent.config_dir(h);
            if config_dir.is_dir() {
                return agent;
            }
        }
    }

    // Fallback: Claude Code (backward-compatible default)
    AgentKind::ClaudeCode
}

pub(super) fn parse_flags(args: &[String]) -> anyhow::Result<InitFlags> {
    let mut project = false;
    let mut yes = false;
    let mut dry_run = false;
    let mut uninstall = false;
    let mut force = false;
    let mut no_guidance = false;
    let mut agent: Option<AgentKind> = None;
    let mut wrappers: Option<bool> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--global" => { /* default, no-op */ }
            "--project" => project = true,
            "--yes" | "-y" => yes = true,
            "--dry-run" => dry_run = true,
            "--uninstall" => uninstall = true,
            "--force" => force = true,
            "--no-guidance" => no_guidance = true,
            "--wrappers" => {
                if wrappers == Some(false) {
                    anyhow::bail!(
                        "--wrappers and --no-wrappers are mutually exclusive\n\
                         Use one or the other, not both."
                    );
                }
                wrappers = Some(true);
            }
            "--no-wrappers" => {
                if wrappers == Some(true) {
                    anyhow::bail!(
                        "--wrappers and --no-wrappers are mutually exclusive\n\
                         Use one or the other, not both."
                    );
                }
                wrappers = Some(false);
            }
            "--agent" => {
                i += 1;
                if i >= args.len() {
                    anyhow::bail!(
                        "missing value for --agent\n\
                         Supported: {}",
                        AgentKind::all_supported()
                            .iter()
                            .map(|a| a.cli_name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                // Reject removed agents with a clear error message
                let name = args[i].as_str();
                if name == "opencode" {
                    anyhow::bail!(
                        "agent 'opencode' has been removed from skim.\n\
                         Use 'crush' instead: skim init --agent crush\n\
                         Install Crush: https://crushcode.ai"
                    );
                }
                agent = Some(AgentKind::parse_cli_arg(name)?);
            }
            other => {
                anyhow::bail!(
                    "unknown flag: '{other}'\n\
                     Run 'skim init --help' for usage information"
                );
            }
        }
        i += 1;
    }

    Ok(InitFlags {
        project,
        yes,
        dry_run,
        uninstall,
        force,
        no_guidance,
        agent,
        wrappers,
    })
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_flags_default_agent_is_none() {
        // No --agent flag → agent is None (auto-detect at runtime)
        let flags = parse_flags(&["--yes".to_string()]).unwrap();
        assert_eq!(flags.agent, None);
    }

    #[test]
    fn test_parse_flags_agent_cursor() {
        let flags = parse_flags(&[
            "--yes".to_string(),
            "--agent".to_string(),
            "cursor".to_string(),
        ])
        .unwrap();
        assert_eq!(flags.agent, Some(AgentKind::Cursor));
    }

    #[test]
    fn test_parse_flags_agent_gemini() {
        let flags = parse_flags(&[
            "--agent".to_string(),
            "gemini".to_string(),
            "--yes".to_string(),
        ])
        .unwrap();
        assert_eq!(flags.agent, Some(AgentKind::GeminiCli));
    }

    #[test]
    fn test_parse_flags_agent_unknown_errors() {
        let result = parse_flags(&["--agent".to_string(), "unknown-agent".to_string()]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown agent"),
            "error should mention unknown agent: {err}"
        );
    }

    #[test]
    fn test_parse_flags_agent_missing_value_errors() {
        let result = parse_flags(&["--agent".to_string()]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("missing value"),
            "error should mention missing value: {err}"
        );
    }

    #[test]
    fn test_parse_flags_backward_compat_no_agent() {
        // No --agent flag → agent is None (auto-detect), other flags still work
        let flags = parse_flags(&["--yes".to_string(), "--dry-run".to_string()]).unwrap();
        assert_eq!(flags.agent, None);
        assert!(flags.yes);
        assert!(flags.dry_run);
    }

    #[test]
    fn test_parse_flags_agent_opencode_removed() {
        // 'opencode' was removed — should give a clear migration error
        let result = parse_flags(&["--agent".to_string(), "opencode".to_string()]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("opencode"),
            "error should mention opencode: {err}"
        );
        assert!(
            err.contains("crush") || err.contains("removed"),
            "error should guide to crush or say removed: {err}"
        );
    }

    #[test]
    fn test_parse_flags_agent_crush() {
        let flags = parse_flags(&["--agent".to_string(), "crush".to_string()]).unwrap();
        assert_eq!(flags.agent, Some(AgentKind::Crush));
    }

    // ---- resolve_single_agent ----

    #[test]
    fn test_resolve_single_agent_returns_explicit() {
        let flags = InitFlags {
            project: false,
            yes: false,
            dry_run: false,
            uninstall: false,
            force: false,
            no_guidance: false,
            agent: Some(AgentKind::Cursor),
            wrappers: None,
        };
        assert_eq!(resolve_single_agent(&flags), Some(AgentKind::Cursor));
    }

    #[test]
    fn test_resolve_single_agent_returns_none_when_auto_detect() {
        let flags = InitFlags {
            project: false,
            yes: false,
            dry_run: false,
            uninstall: false,
            force: false,
            no_guidance: false,
            agent: None,
            wrappers: None,
        };
        assert_eq!(resolve_single_agent(&flags), None);
    }

    // ---- detect_installed_agents ----

    #[test]
    fn test_detect_installed_agents_returns_subset_of_supported() {
        // We can't control which agents are actually installed in the test env,
        // but every returned agent must be in the supported list.
        let detected = detect_installed_agents(&DetectionEnv::from_process());
        let supported = AgentKind::all_supported();
        for agent in &detected {
            assert!(
                supported.contains(agent),
                "detect_installed_agents returned unsupported agent: {agent:?}"
            );
        }
    }

    #[test]
    fn test_detect_installed_agents_no_duplicates() {
        let detected = detect_installed_agents(&DetectionEnv::from_process());
        for (i, a) in detected.iter().enumerate() {
            for b in &detected[i + 1..] {
                assert_ne!(
                    a, b,
                    "detect_installed_agents returned duplicate agent: {a:?}"
                );
            }
        }
    }

    #[test]
    fn test_detect_installed_agents_with_injected_env_two_agents() {
        // Construct a DetectionEnv directly (no process env reads) with two
        // directories that will both exist — use the system temp dir as a
        // stand-in for a real config dir.
        let tmp = std::env::temp_dir();
        let env = DetectionEnv {
            home_dir: None,
            claude_config_dir: Some(tmp.clone()),
            gemini_config_dir: Some(tmp.clone()),
            cursor_config_dir: None,
            copilot_config_dir: None,
            codex_config_dir: None,
            crush_config_dir: None,
        };
        let detected = detect_installed_agents(&env);
        assert_eq!(
            detected.len(),
            2,
            "should detect exactly the two agents whose env paths exist"
        );
        assert!(detected.contains(&AgentKind::ClaudeCode));
        assert!(detected.contains(&AgentKind::GeminiCli));
    }

    #[test]
    fn test_detect_installed_agents_empty_when_no_dirs_exist() {
        let env = DetectionEnv {
            home_dir: None,
            claude_config_dir: Some(std::path::PathBuf::from("/nonexistent-dir-abc123")),
            cursor_config_dir: None,
            gemini_config_dir: None,
            copilot_config_dir: None,
            codex_config_dir: None,
            crush_config_dir: None,
        };
        let detected = detect_installed_agents(&env);
        assert!(
            detected.is_empty(),
            "should detect no agents when override path does not exist"
        );
    }

    // ---- resolve_agent (compat shim) ----

    #[test]
    fn test_resolve_agent_explicit() {
        let flags = InitFlags {
            project: false,
            yes: false,
            dry_run: false,
            uninstall: false,
            force: false,
            no_guidance: false,
            agent: Some(AgentKind::Cursor),
            wrappers: None,
        };
        // env is unused when agent is explicit; default env is fine
        assert_eq!(
            resolve_agent(&flags, &DetectionEnv::default()),
            AgentKind::Cursor
        );
    }

    #[test]
    fn test_resolve_agent_fallback_when_none() {
        // Inject a non-existent home dir so no agent config dirs can match,
        // forcing the ClaudeCode fallback. Previously called dirs::home_dir()
        // directly, making this branch untestable.
        let flags = InitFlags {
            project: false,
            yes: false,
            dry_run: false,
            uninstall: false,
            force: false,
            no_guidance: false,
            agent: None,
            wrappers: None,
        };
        let env = DetectionEnv {
            home_dir: Some(std::path::PathBuf::from(
                "/tmp/__skim_test_nonexistent_home_dir__",
            )),
            ..DetectionEnv::default()
        };
        assert_eq!(
            resolve_agent(&flags, &env),
            AgentKind::ClaudeCode,
            "should fall back to ClaudeCode when no agent config dirs exist"
        );
    }

    // ---- --wrappers / --no-wrappers ----

    #[test]
    fn test_parse_flags_wrappers_true() {
        let flags = parse_flags(&["--wrappers".to_string()]).unwrap();
        assert_eq!(flags.wrappers, Some(true), "--wrappers must set Some(true)");
    }

    #[test]
    fn test_parse_flags_no_wrappers_false() {
        let flags = parse_flags(&["--no-wrappers".to_string()]).unwrap();
        assert_eq!(
            flags.wrappers,
            Some(false),
            "--no-wrappers must set Some(false)"
        );
    }

    #[test]
    fn test_parse_flags_wrappers_absent_is_none() {
        let flags = parse_flags(&["--yes".to_string()]).unwrap();
        assert_eq!(
            flags.wrappers, None,
            "absent --wrappers flag must yield None"
        );
    }

    #[test]
    fn test_parse_flags_wrappers_and_no_wrappers_conflict() {
        let result = parse_flags(&["--wrappers".to_string(), "--no-wrappers".to_string()]);
        assert!(
            result.is_err(),
            "--wrappers and --no-wrappers must conflict"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("mutually exclusive"),
            "error must mention mutual exclusion: {err}"
        );
    }
}
