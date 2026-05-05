//! Uninstall flow for `skim init` (B10).

use super::flags::{detect_installed_agents, resolve_agent, resolve_single_agent, InitFlags};
use super::helpers::{
    atomic_write_settings, check_mark, confirm_proceed, load_or_create_settings,
    resolve_config_dir_for_agent, resolve_real_settings_path, HOOK_SCRIPT_NAME,
};
use super::state::{has_skim_hook_entry, read_settings_json};
use crate::cmd::hooks::protocol_for_agent;
use crate::cmd::session::InstructionEnv;

/// Remove skim hook entries from a settings value.
///
/// 1. Removes skim entries from `hooks.<event_key>` array
/// 2. Cleans up empty arrays/objects
fn remove_skim_from_settings(settings: &mut serde_json::Value, event_key: &str) {
    let Some(obj) = settings.as_object_mut() else {
        return;
    };

    if let Some(hooks_obj) = obj.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        if let Some(arr) = hooks_obj
            .get_mut(event_key)
            .and_then(|p| p.as_array_mut())
        {
            arr.retain(|entry| !has_skim_hook_entry(entry));
            if arr.is_empty() {
                hooks_obj.remove(event_key);
            }
        }
        if hooks_obj.is_empty() {
            obj.remove("hooks");
        }
    }
}

pub(super) fn run_uninstall(flags: &InitFlags) -> anyhow::Result<std::process::ExitCode> {
    if resolve_single_agent(flags).is_none() {
        return run_uninstall_auto_detect(flags);
    }
    run_uninstall_single(flags)
}

/// Uninstall skim from all detected agents when no explicit `--agent` was given.
fn run_uninstall_auto_detect(flags: &InitFlags) -> anyhow::Result<std::process::ExitCode> {
    let agents = detect_installed_agents();
    if agents.is_empty() {
        println!("  No supported agents found. Nothing to uninstall.");
        return Ok(std::process::ExitCode::SUCCESS);
    }

    // Single-agent fast path: skip the loop overhead when only one agent is detected.
    // Preserves original error propagation for test assertions.
    if agents.len() == 1 {
        let agent_flags = InitFlags {
            agent: Some(agents[0]),
            ..*flags
        };
        return run_uninstall_for_agent(&agent_flags, agents[0]);
    }

    let mut any_failed = false;
    for &agent in &agents {
        let agent_flags = InitFlags {
            agent: Some(agent),
            ..*flags
        };
        match run_uninstall_for_agent(&agent_flags, agent) {
            Ok(code) if code == std::process::ExitCode::SUCCESS => {}
            Ok(code) => {
                any_failed = true;
                eprintln!("  ✗ {}: uninstall failed (exit code: {:?})", agent.display_name(), code);
            }
            Err(e) => {
                any_failed = true;
                eprintln!("  ✗ {}: uninstall failed — {e}", agent.display_name());
            }
        }
    }

    Ok(if any_failed {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    })
}

/// Uninstall skim for a single explicit agent (dispatched by `run_uninstall`).
fn run_uninstall_single(flags: &InitFlags) -> anyhow::Result<std::process::ExitCode> {
    let agent = resolve_agent(flags);
    run_uninstall_for_agent(flags, agent)
}

fn run_uninstall_for_agent(flags: &InitFlags, agent: crate::cmd::session::AgentKind) -> anyhow::Result<std::process::ExitCode> {
    let protocol = protocol_for_agent(agent);
    let config_dir = resolve_config_dir_for_agent(flags.project, agent)?;
    let settings_path = config_dir.join(protocol.config_filename());
    let hook_script_path = config_dir.join("hooks").join(HOOK_SCRIPT_NAME);

    // Check if anything is installed
    let settings_has_hook = read_settings_json(&settings_path)
        .and_then(|json| {
            json.get("hooks")?
                .get(protocol.hook_event_key())?
                .as_array()
                .map(|arr| arr.iter().any(has_skim_hook_entry))
        })
        .unwrap_or(false);

    let script_exists = hook_script_path.exists();

    if !settings_has_hook && !script_exists {
        println!("  skim hook not found. Nothing to uninstall.");
        return Ok(std::process::ExitCode::SUCCESS);
    }

    // Integrity check (#57): warn if hook script has been modified since install
    if script_exists {
        if let Ok(false) = crate::cmd::integrity::verify_script_integrity(
            &config_dir,
            agent.cli_name(),
            &hook_script_path,
        ) {
            if !flags.force {
                eprintln!("warning: hook script has been modified since installation");
                eprintln!("hint: use --force to uninstall anyway");
                return Ok(std::process::ExitCode::FAILURE);
            }
            // --force provided: proceed despite tamper, but inform user
            eprintln!("warning: hook script has been modified (proceeding with --force)");
        }
    }

    // Interactive confirmation
    if !flags.yes {
        println!();
        println!("  skim init --uninstall");
        println!();
        if settings_has_hook {
            println!("    * Remove hook entry from {}", settings_path.display());
        }
        if script_exists {
            println!("    * Delete {}", hook_script_path.display());
        }
        println!();
        if !confirm_proceed()? {
            println!("  Cancelled.");
            return Ok(std::process::ExitCode::SUCCESS);
        }
    }

    if flags.dry_run {
        if settings_has_hook {
            println!(
                "  [dry-run] Would remove hook entry from {}",
                settings_path.display()
            );
        }
        if script_exists {
            println!("  [dry-run] Would delete {}", hook_script_path.display());
        }
        return Ok(std::process::ExitCode::SUCCESS);
    }

    // Remove from settings.json
    if settings_has_hook {
        let real_path = resolve_real_settings_path(&settings_path)?;
        let mut settings = load_or_create_settings(&real_path)?;

        remove_skim_from_settings(&mut settings, protocol.hook_event_key());

        atomic_write_settings(&settings, &real_path)?;

        println!(
            "  {} Removed: hook entry from {}",
            check_mark(true),
            settings_path.display()
        );
    }

    // Delete hook script and hash manifest
    if script_exists {
        std::fs::remove_file(&hook_script_path)?;
        println!(
            "  {} Deleted: {}",
            check_mark(true),
            hook_script_path.display()
        );

        // Clean up hash manifest (#57)
        let _ = crate::cmd::integrity::remove_hash_manifest(&config_dir, agent.cli_name());
    }

    // Remove guidance from instruction file
    let global = !flags.project;
    let env = InstructionEnv::from_process();
    super::install::remove_guidance(agent, global, &env)?;

    println!();
    println!("  skim hook has been uninstalled.");
    println!();

    Ok(std::process::ExitCode::SUCCESS)
}
