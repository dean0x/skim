//! Output formatting for the `skim agents` subcommand.
//!
//! Text output uses the `colored` crate for status indicators (D7: respects
//! `NO_COLOR`). JSON output is unchanged (D5).
//
// No spinner: agent detection is filesystem-stat-based (<10ms).
// A sub-second spinner degrades UX rather than improving it (D1).

use colored::Colorize;

use super::types::{AgentStatus, HookStatus};

// Text output format is not a stable interface.
// Use `--json` for scripting and automation (D5).
pub(super) fn print_text(agents: &[AgentStatus]) {
    println!("Detected agents:");
    for agent in agents {
        println!();
        if agent.detected {
            // Detected: green `+` + bold name
            println!(
                "  {} {}",
                crate::cmd::ux::success_mark(),
                agent.kind.display_name().bold(),
            );
        } else {
            // Not detected: red `-` + dimmed label
            println!(
                "  {} {} {}",
                crate::cmd::ux::fail_mark(),
                agent.kind.display_name(),
                "(not detected)".dimmed(),
            );
            continue;
        }

        // Indent continuation lines to align past the header row:
        // 2 leading spaces + 1 mark char + 1 space + 1 extra padding = 5.
        let indent = " ".repeat(agent.kind.display_name().len() + 5);

        // Sessions
        if let Some(ref sessions) = agent.sessions {
            println!(
                "{}sessions: {} ({})",
                indent, sessions.path, sessions.detail,
            );
        }

        // Hooks — color by status
        let hook_str = match &agent.hooks {
            HookStatus::Installed { version, integrity } => {
                let ver = version
                    .as_deref()
                    .map(|v| format!(", v{v}"))
                    .unwrap_or_default();
                format!("installed (integrity: {integrity}{ver})")
                    .green()
                    .to_string()
            }
            HookStatus::NotInstalled => {
                let hint = format!(
                    "\n{}  hint: run `skim init --agent {}`",
                    indent,
                    agent.kind.cli_name(),
                );
                format!("{}{}", "not installed".yellow(), hint.dimmed())
            }
            HookStatus::NotSupported { note } => {
                format!("not supported ({note})").dimmed().to_string()
            }
        };
        println!("{}hooks: {}", indent, hook_str);

        // Rules
        if let Some(ref rules) = agent.rules {
            let status = if rules.exists {
                "found".green().to_string()
            } else {
                "not found".yellow().to_string()
            };
            println!("{}rules: {} ({})", indent, rules.path, status);
        }
    }
}

pub(super) fn print_json(agents: &[AgentStatus]) -> anyhow::Result<()> {
    let agent_values: Vec<serde_json::Value> = agents
        .iter()
        .map(|agent| {
            let sessions = agent.sessions.as_ref().map(|s| {
                serde_json::json!({
                    "path": s.path,
                    "detail": s.detail,
                })
            });

            let hooks = match &agent.hooks {
                HookStatus::Installed { version, integrity } => serde_json::json!({
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

            serde_json::json!({
                "name": agent.kind.display_name(),
                "cli_name": agent.kind.cli_name(),
                "detected": agent.detected,
                "sessions": sessions,
                "hooks": hooks,
                "rules": rules,
            })
        })
        .collect();

    let output = serde_json::json!({ "agents": agent_values });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

pub(super) fn print_help() {
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
