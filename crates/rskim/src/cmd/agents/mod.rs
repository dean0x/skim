//! `skim agents` -- display detected AI agents and their hook/session status.
//!
//! Scans for known AI coding agents (Claude Code, Cursor, Codex CLI, Gemini CLI,
//! Copilot CLI) and reports their detection status, session paths, hook installation
//! status, and rules directory presence.

mod detection;
mod formatting;
mod types;
mod util;

use std::process::ExitCode;

use detection::detect_all_agents;
use formatting::{print_help, print_json, print_text};

/// Run the `skim agents` subcommand.
pub(crate) fn run(
    args: &[String],
    _analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::session::AgentKind;
    use types::HookStatus;

    /// Stub analytics config for tests — analytics disabled, no cost override.
    const TEST_ANALYTICS: crate::analytics::AnalyticsConfig = crate::analytics::AnalyticsConfig {
        enabled: false,
        input_cost_per_mtok: None,
    };

    #[test]
    fn test_agents_run_no_crash() {
        let result = run(&[], &TEST_ANALYTICS);
        assert!(result.is_ok());
    }

    #[test]
    fn test_agents_help_flag() {
        let result = run(&["--help".to_string()], &TEST_ANALYTICS);
        assert!(result.is_ok());
    }

    #[test]
    fn test_agents_json_output_valid_json() {
        let agents = detect_all_agents();
        assert_eq!(
            agents.len(),
            AgentKind::all_supported().len(),
            "agent count should match supported kinds"
        );

        let result = run(&["--json".to_string()], &TEST_ANALYTICS);
        assert!(result.is_ok());

        for agent in &agents {
            match &agent.hooks {
                HookStatus::Installed { integrity, .. } => {
                    assert!(
                        ["ok", "tampered", "missing", "unknown"].contains(integrity),
                        "unexpected integrity value: {integrity}"
                    );
                }
                HookStatus::NotInstalled => {}
                HookStatus::NotSupported { note } => {
                    assert!(!note.is_empty(), "NotSupported note should not be empty");
                }
            }
        }
    }

    #[test]
    fn test_hook_status_display() {
        let installed = HookStatus::Installed {
            version: Some("2.0.0".to_string()),
            integrity: "ok",
        };
        match &installed {
            HookStatus::Installed { version, integrity } => {
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
    fn test_agent_kind_cli_name() {
        assert_eq!(AgentKind::ClaudeCode.cli_name(), "claude-code");
        assert_eq!(AgentKind::Cursor.cli_name(), "cursor");
        assert_eq!(AgentKind::CodexCli.cli_name(), "codex");
        assert_eq!(AgentKind::GeminiCli.cli_name(), "gemini");
        assert_eq!(AgentKind::CopilotCli.cli_name(), "copilot");
        assert_eq!(AgentKind::OpenCode.cli_name(), "opencode");
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
