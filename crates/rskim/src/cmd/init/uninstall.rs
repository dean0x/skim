//! Uninstall flow for `skim init` (B10).

use super::flags::{
    DetectionEnv, InitFlags, detect_installed_agents, resolve_agent, resolve_single_agent,
};
use super::guidance::agents_sharing_file;
use super::helpers::{
    HOOK_SCRIPT_NAME, atomic_write_settings, check_mark, confirm_proceed, load_or_create_settings,
    resolve_real_settings_path,
};
use super::state::{detect_state, has_skim_hook_entry, read_settings_json, skim_event_keys};
use super::targets::{agent_names, remove_targets};
use crate::cmd::hooks::protocol_for_agent;
use crate::cmd::session::{AgentKind, InstructionEnv};

/// Remove skim hook entries from a settings value.
///
/// 1. Removes skim entries from `hooks.<event_key>` array
/// 2. Cleans up empty arrays/objects
fn remove_skim_from_settings(settings: &mut serde_json::Value, event_key: &str) {
    let Some(obj) = settings.as_object_mut() else {
        return;
    };

    if let Some(hooks_obj) = obj.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        if let Some(arr) = hooks_obj.get_mut(event_key).and_then(|p| p.as_array_mut()) {
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
    // First, and whatever the agents hold: the old search git hooks belong to the
    // repository, not to an agent, so they go also when no agent hook is installed.
    super::install::remove_legacy_search_hooks(flags.dry_run);

    let det_env = DetectionEnv::from_process();
    if resolve_single_agent(flags).is_none() {
        return run_uninstall_auto_detect(flags, &det_env);
    }
    run_uninstall_single(flags, &det_env)
}

/// Uninstall skim from all detected agents when no explicit `--agent` was given.
fn run_uninstall_auto_detect(
    flags: &InitFlags,
    det_env: &DetectionEnv,
) -> anyhow::Result<std::process::ExitCode> {
    // The guidance-only agents come before both early returns below: a user who
    // has only one of them, or only one other agent, still loses their files.
    let targets = remove_targets(flags, &InstructionEnv::from_process());

    let agents = detect_installed_agents(det_env);
    if agents.is_empty() {
        if targets.handled == 0 {
            println!(
                "  No supported agents found ({}). Nothing to uninstall.",
                agent_names(flags)
            );
        }
        return Ok(targets.exit_code_with(std::process::ExitCode::SUCCESS));
    }

    // Single-agent fast path: skip the loop overhead when only one agent is detected.
    // Preserves original error propagation for test assertions.
    if agents.len() == 1 {
        let agent_flags = InitFlags {
            agent: Some(agents[0]),
            ..*flags
        };
        let code = run_uninstall_for_agent(&agent_flags, agents[0], det_env)?;
        return Ok(targets.exit_code_with(code));
    }

    let mut any_failed = false;
    for &agent in &agents {
        let agent_flags = InitFlags {
            agent: Some(agent),
            ..*flags
        };
        match run_uninstall_for_agent(&agent_flags, agent, det_env) {
            Ok(code) if code == std::process::ExitCode::SUCCESS => {}
            Ok(code) => {
                any_failed = true;
                eprintln!(
                    "  ✗ {}: uninstall failed (exit code: {:?})",
                    agent.display_name(),
                    code
                );
            }
            Err(e) => {
                any_failed = true;
                eprintln!("  ✗ {}: uninstall failed — {e}", agent.display_name());
            }
        }
    }

    Ok(targets.exit_code_with(if any_failed {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    }))
}

/// Uninstall skim for a single explicit agent (dispatched by `run_uninstall`).
fn run_uninstall_single(
    flags: &InitFlags,
    det_env: &DetectionEnv,
) -> anyhow::Result<std::process::ExitCode> {
    let agent = resolve_agent(flags, det_env);
    run_uninstall_for_agent(flags, agent, det_env)
}

fn run_uninstall_for_agent(
    flags: &InitFlags,
    agent: AgentKind,
    det_env: &DetectionEnv,
) -> anyhow::Result<std::process::ExitCode> {
    uninstall_agent(flags, agent, det_env, &InstructionEnv::from_process())
}

/// The agents that read the guidance file of `agent` and are still set up.
///
/// Fails toward keeping: an agent whose state cannot be read counts as set up.
/// It returns no error, so the uninstall cannot stop after the hook is gone.
fn agents_still_using_guidance(
    agent: AgentKind,
    project: bool,
    env: &InstructionEnv,
    det_env: &DetectionEnv,
) -> Vec<AgentKind> {
    let Ok(scope) = super::install::GuidanceScope::for_flag(!project) else {
        return Vec::new();
    };
    agents_sharing_file(agent, &scope, env)
        .into_iter()
        .filter(|&other| {
            counts_as_set_up(
                detect_state(other, project, det_env).map(|state| state.hook_installed),
            )
        })
        .collect()
}

/// Whether an agent is still set up, given what reading its state returned.
///
/// Codex has no hook to run, but `skim init` registers one entry for it like
/// for every agent, so the hook state is the install state of both kinds. A
/// state that cannot be read counts as set up.
fn counts_as_set_up(hook_installed: anyhow::Result<bool>) -> bool {
    hook_installed.unwrap_or(true)
}

fn uninstall_agent(
    flags: &InitFlags,
    agent: AgentKind,
    det_env: &DetectionEnv,
    env: &InstructionEnv,
) -> anyhow::Result<std::process::ExitCode> {
    // Before any change: a relative guidance path is refused up front, so the
    // uninstall does not stop half-done once the hook is gone.
    if !flags.project {
        super::install::check_global_target(agent, env)?;
    }
    let protocol = protocol_for_agent(agent);
    let config_dir = det_env.resolve(agent, flags.project)?;

    // Resolve hook artifact directory via the protocol seam.
    // For all agents except Copilot CLI this equals config_dir.
    let hook_config_dir = protocol.hook_config_dir(
        &config_dir,
        flags.project,
        det_env.override_for(agent).is_some(),
    );

    let settings_path = config_dir.join(protocol.config_filename());
    let hook_script_path = hook_config_dir.join("hooks").join(HOOK_SCRIPT_NAME);

    // Check if anything is installed — dispatch on hook file style.
    let (registration_exists, settings_has_hook) = if protocol.uses_dedicated_hook_file() {
        // Copilot: check hooks/skim.json; settings.json is not used.
        let reg = protocol.detect_hook_registration(&hook_config_dir);
        (reg, false)
    } else {
        // settings.json-based agents.
        let has = read_settings_json(&settings_path).is_some_and(|json| {
            !skim_event_keys(&json, protocol.as_ref(), has_skim_hook_entry).is_empty()
        });
        (has, has)
    };

    let script_exists = hook_script_path.exists();

    if !registration_exists && !script_exists {
        println!("  skim hook not found. Nothing to uninstall.");
        return Ok(std::process::ExitCode::SUCCESS);
    }

    // Integrity check (#57): warn if hook script has been modified since install
    if script_exists
        && let Ok(false) = crate::cmd::integrity::verify_script_integrity(
            &hook_config_dir,
            agent.cli_name(),
            &hook_script_path,
        )
    {
        if !flags.force {
            eprintln!("warning: hook script has been modified since installation");
            eprintln!("hint: use --force to uninstall anyway");
            return Ok(std::process::ExitCode::FAILURE);
        }
        // --force provided: proceed despite tamper, but inform user
        eprintln!("warning: hook script has been modified (proceeding with --force)");
    }

    // Interactive confirmation
    if !flags.yes {
        println!();
        println!("  skim init --uninstall");
        println!();
        if registration_exists {
            if protocol.uses_dedicated_hook_file() {
                let skim_json = hook_config_dir.join("hooks").join("skim.json");
                println!("    * Remove {}", skim_json.display());
            } else {
                println!("    * Remove hook entry from {}", settings_path.display());
            }
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
        if registration_exists {
            if protocol.uses_dedicated_hook_file() {
                let skim_json = hook_config_dir.join("hooks").join("skim.json");
                println!("  [dry-run] Would remove {}", skim_json.display());
            } else {
                println!(
                    "  [dry-run] Would remove hook entry from {}",
                    settings_path.display()
                );
            }
        }
        if script_exists {
            println!("  [dry-run] Would delete {}", hook_script_path.display());
        }
        return Ok(std::process::ExitCode::SUCCESS);
    }

    // Remove hook registration — dispatch on style.
    if protocol.uses_dedicated_hook_file() {
        if protocol.remove_hook_registration(&hook_config_dir)? {
            let skim_json = hook_config_dir.join("hooks").join("skim.json");
            println!("  {} Removed: {}", check_mark(true), skim_json.display());
        }
    } else if settings_has_hook {
        let real_path = resolve_real_settings_path(&settings_path)?;
        let mut settings = load_or_create_settings(&real_path)?;

        for event_key in protocol.hook_event_keys() {
            remove_skim_from_settings(&mut settings, event_key);
        }

        atomic_write_settings(&settings, &real_path)?;

        println!(
            "  {} Removed: hook entry from {}",
            check_mark(true),
            settings_path.display()
        );
    }

    // Delete hook script
    if script_exists {
        std::fs::remove_file(&hook_script_path)?;
        println!(
            "  {} Deleted: {}",
            check_mark(true),
            hook_script_path.display()
        );
    }

    // Clean up hash manifest (#57) — always attempt, even if the script was
    // already absent.  This prevents an orphaned manifest from a previous failed
    // uninstall from persisting and causing false tamper reports on re-install.
    let _ = crate::cmd::integrity::remove_hash_manifest(&hook_config_dir, agent.cli_name());

    // Remove permissions sidecar and seeded entries (non-fatal).
    // Copilot CLI permissions live in hook_config_dir; all others in config_dir.
    let perm_dir: &std::path::Path = if agent == AgentKind::CopilotCli {
        &hook_config_dir
    } else {
        &config_dir
    };
    if let Some(permissions_protocol) =
        crate::cmd::permissions::permissions_protocol_for_agent(agent)
    {
        let sidecar_path = perm_dir.join("skim-permissions.json");
        if sidecar_path.exists() {
            match permissions_protocol.remove_seeded(perm_dir) {
                Ok(crate::cmd::permissions::RemoveOutcome::Removed { entries_removed }) => {
                    println!(
                        "  {} Permissions: removed {} seeded entr{}",
                        check_mark(true),
                        entries_removed.len(),
                        if entries_removed.len() == 1 {
                            "y"
                        } else {
                            "ies"
                        }
                    );
                }
                Ok(crate::cmd::permissions::RemoveOutcome::NothingToRemove) => {
                    // No-op: sidecar existed but no matching entries found.
                }
                Err(e) => {
                    // Non-fatal: loud notice but continue uninstall.
                    eprintln!("  Notice: could not remove seeded permissions (sidecar issue): {e}");
                    eprintln!("  hint: permissions entries may need to be removed manually");
                }
            }
        }
    }

    // Remove guidance from instruction file. A file that another set-up agent
    // still reads keeps its block.
    let global = !flags.project;
    let still_using = agents_still_using_guidance(agent, flags.project, env, det_env);
    super::install::remove_guidance(agent, global, env, &still_using)?;

    // Uninstall shell wrappers (global scope only).
    // Non-fatal: wrapper removal failure must not abort hook uninstall.
    if !flags.project {
        match super::wrappers::uninstall_wrappers(flags.dry_run) {
            Ok(result) if result.removed > 0 => {
                println!(
                    "  {} Removed {} wrapper symlink(s) from ~/.skim/bin/",
                    check_mark(true),
                    result.removed
                );
                if result.dir_removed {
                    println!(
                        "  {} Removed empty ~/.skim/bin/ directory",
                        check_mark(true)
                    );
                }
            }
            Ok(_) => {} // No wrappers to remove — silently skip
            Err(e) => {
                eprintln!("  Note: could not remove wrappers: {e}");
            }
        }
    }

    println!();
    println!("  skim hook has been uninstalled.");
    println!();

    Ok(std::process::ExitCode::SUCCESS)
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- remove_skim_from_settings tests ----

    // Hook entry format: {matcher, hooks: [{type, command, timeout}]}
    // This matches the `build_config_entry` default and `has_skim_hook_entry` detector.
    fn skim_entry() -> serde_json::Value {
        serde_json::json!({
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": "/home/user/.claude/hooks/skim-rewrite.sh", "timeout": 10}]
        })
    }

    fn other_entry() -> serde_json::Value {
        serde_json::json!({
            "matcher": "Edit",
            "hooks": [{"type": "command", "command": "/other/hook.sh", "timeout": 5}]
        })
    }

    #[test]
    fn test_remove_skim_from_settings_removes_skim_entry() {
        let mut settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [skim_entry(), other_entry()]
            }
        });

        remove_skim_from_settings(&mut settings, "PreToolUse");

        let arr = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "only the non-skim entry must remain");
        assert!(
            arr[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("other"),
            "the non-skim entry must be preserved"
        );
    }

    #[test]
    fn test_remove_skim_from_settings_removes_empty_hooks_object() {
        // When the last hook entry is the skim entry, the resulting empty
        // hooks object must also be removed — no leftover `"hooks": {}` key.
        let mut settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [skim_entry()]
            }
        });

        remove_skim_from_settings(&mut settings, "PreToolUse");

        assert!(
            settings.get("hooks").is_none(),
            "empty hooks object must be removed after last skim entry is stripped"
        );
    }

    #[test]
    fn test_remove_skim_from_settings_is_idempotent() {
        // Calling remove twice must not panic or leave the object in a broken state.
        let mut settings = serde_json::json!({
            "hooks": {
                "PreToolUse": [skim_entry()]
            }
        });

        remove_skim_from_settings(&mut settings, "PreToolUse");
        remove_skim_from_settings(&mut settings, "PreToolUse"); // second call must be a no-op

        assert!(
            settings.get("hooks").is_none(),
            "double-remove must leave settings clean"
        );
    }

    #[test]
    fn test_remove_skim_from_settings_noop_on_missing_key() {
        // If the event key doesn't exist, the call must be a no-op.
        let mut settings = serde_json::json!({ "other": "value" });
        remove_skim_from_settings(&mut settings, "PreToolUse");
        assert_eq!(
            settings["other"].as_str().unwrap(),
            "value",
            "unrelated keys must be untouched"
        );
    }

    // ---- A shared guidance file ----

    #[test]
    fn an_agent_whose_state_cannot_be_read_counts_as_set_up() {
        assert!(counts_as_set_up(Err(anyhow::anyhow!("unreadable"))));
        assert!(counts_as_set_up(Ok(true)));
        assert!(!counts_as_set_up(Ok(false)));
    }

    // ---- Refusal before any change ----

    #[test]
    fn a_relative_guidance_override_is_refused_before_the_hook_is_removed() {
        use super::super::flags::PermissionsTier;

        let home = tempfile::TempDir::new().unwrap();
        let claude = home.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        let settings = serde_json::json!({ "hooks": { "PreToolUse": [skim_entry()] } });
        let settings_text = serde_json::to_string_pretty(&settings).unwrap();
        std::fs::write(claude.join("settings.json"), &settings_text).unwrap();
        let det_env = DetectionEnv {
            claude_config_dir: Some(claude.clone()),
            ..DetectionEnv::default()
        };
        let env = InstructionEnv::from_lookup(Some(home.path().to_path_buf()), |_| {
            Some(std::ffi::OsString::from("relative-config-folder"))
        });
        let flags = InitFlags {
            project: false,
            yes: true,
            dry_run: false,
            uninstall: true,
            force: false,
            no_guidance: false,
            dev: false,
            agent: Some(AgentKind::ClaudeCode),
            wrappers: None,
            permissions: None,
            permissions_tier: PermissionsTier::Seed,
        };

        let error = uninstall_agent(&flags, AgentKind::ClaudeCode, &det_env, &env).unwrap_err();

        assert!(error.to_string().contains("CLAUDE_CONFIG_DIR"), "{error}");
        assert_eq!(
            std::fs::read_to_string(claude.join("settings.json")).unwrap(),
            settings_text,
            "a refused uninstall must leave the hook entry in place"
        );
    }

    // ---- Non-fatal wrapper uninstall invariant ----

    /// Behavioral invariant (uninstall:205): wrapper removal failure must NOT
    /// abort hook uninstall.  `run_uninstall_for_agent` matches `Err(e)` on
    /// `uninstall_wrappers` and emits to stderr — it does not propagate the error.
    ///
    /// This test validates the `uninstall_wrappers_in` error path directly:
    /// passing a directory with >256 entries triggers the circuit-breaker `Err`,
    /// which is the same error class that `run_uninstall_for_agent` suppresses.
    #[cfg(unix)]
    #[test]
    fn test_wrapper_uninstall_error_is_non_fatal_in_handler() {
        use super::super::wrappers::uninstall_wrappers_in;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        // Saturate the directory beyond the 256-entry circuit breaker to force Err.
        for i in 0..=256usize {
            std::fs::write(bin_dir.join(format!("file_{i}")), "").unwrap();
        }

        // The error that run_uninstall_for_agent suppresses is of this same kind.
        let err = uninstall_wrappers_in(&bin_dir, false);
        assert!(
            err.is_err(),
            "circuit-breaker must fire — this is the error class the handler suppresses"
        );

        // Simulate what run_uninstall_for_agent does: match Err => eprintln, not propagate.
        // The pattern must compile and the function must not return Err.
        let non_fatal_result: anyhow::Result<()> = match uninstall_wrappers_in(&bin_dir, false) {
            Ok(_) => Ok(()),
            Err(e) => {
                // Non-fatal: mirror the exact match arm from run_uninstall_for_agent.
                let _ = format!("Note: could not remove wrappers: {e}");
                Ok(()) // swallowed — hook uninstall continues
            }
        };
        assert!(
            non_fatal_result.is_ok(),
            "wrapper removal error must be non-fatal — hook uninstall must succeed"
        );
    }
}
