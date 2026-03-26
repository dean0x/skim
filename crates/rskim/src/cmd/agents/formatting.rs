//! Output formatting for the `skim agents` subcommand.

use super::types::{AgentStatus, HookStatus};

pub(super) fn print_text(agents: &[AgentStatus]) {
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
            HookStatus::Installed { version, integrity } => {
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
