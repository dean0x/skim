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
    /// Target agent for installation (default: ClaudeCode)
    pub(super) agent: AgentKind,
}

pub(super) fn parse_flags(args: &[String]) -> anyhow::Result<InitFlags> {
    let mut project = false;
    let mut yes = false;
    let mut dry_run = false;
    let mut uninstall = false;
    let mut force = false;
    let mut no_guidance = false;
    let mut agent = AgentKind::ClaudeCode;

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
                agent = AgentKind::parse_cli_arg(&args[i])?;
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
    fn test_parse_flags_default_agent_is_claude_code() {
        let flags = parse_flags(&["--yes".to_string()]).unwrap();
        assert_eq!(flags.agent, AgentKind::ClaudeCode);
    }

    #[test]
    fn test_parse_flags_agent_cursor() {
        let flags = parse_flags(&[
            "--yes".to_string(),
            "--agent".to_string(),
            "cursor".to_string(),
        ])
        .unwrap();
        assert_eq!(flags.agent, AgentKind::Cursor);
    }

    #[test]
    fn test_parse_flags_agent_gemini() {
        let flags = parse_flags(&[
            "--agent".to_string(),
            "gemini".to_string(),
            "--yes".to_string(),
        ])
        .unwrap();
        assert_eq!(flags.agent, AgentKind::GeminiCli);
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
        // No --agent flag should default to ClaudeCode
        let flags = parse_flags(&["--yes".to_string(), "--dry-run".to_string()]).unwrap();
        assert_eq!(flags.agent, AgentKind::ClaudeCode);
        assert!(flags.yes);
        assert!(flags.dry_run);
    }
}
