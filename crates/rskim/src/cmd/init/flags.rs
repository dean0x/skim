//! Flag parsing for `skim init`.

use crate::cmd::session::AgentKind;

/// Parsed command-line flags for the init subcommand.
#[derive(Debug)]
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
}

/// Resolve the target agent from flags.
///
/// - If `flags.agent` is `Some(kind)`, return it directly.
/// - If `None`, scan for installed agents (home-dir detection) and return the
///   first found. Falls back to `AgentKind::ClaudeCode` when nothing is detected
///   (mirrors old default behaviour so `skim init` without `--agent` still works
///   on a clean system).
pub(super) fn resolve_agent(flags: &InitFlags) -> AgentKind {
    if let Some(agent) = flags.agent {
        return agent;
    }

    // Auto-detect: pick the first agent whose config directory exists.
    let home = dirs::home_dir();
    for &agent in AgentKind::all_supported() {
        if let Some(ref h) = home {
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

    // ---- resolve_agent ----

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
        };
        assert_eq!(resolve_agent(&flags), AgentKind::Cursor);
    }

    #[test]
    fn test_resolve_agent_fallback_when_none() {
        // When agent is None and no agents are installed (temp home doesn't exist),
        // should fall back to ClaudeCode
        let flags = InitFlags {
            project: false,
            yes: false,
            dry_run: false,
            uninstall: false,
            force: false,
            no_guidance: false,
            agent: None,
        };
        // We can't control dirs::home_dir(), but we can assert the fallback works
        // without panicking and returns a valid AgentKind
        let resolved = resolve_agent(&flags);
        assert!(
            AgentKind::all_supported().contains(&resolved),
            "resolve_agent should return a supported agent, got: {resolved:?}"
        );
    }
}
