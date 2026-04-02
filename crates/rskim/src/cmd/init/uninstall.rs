//! Uninstall flow for `skim init` (B10).

use super::flags::InitFlags;
use super::helpers::{
    atomic_write_settings, check_mark, confirm_proceed, load_or_create_settings,
    resolve_config_dir_for_agent, resolve_real_settings_path, HOOK_SCRIPT_NAME, SETTINGS_FILE,
};
use super::state::{has_skim_hook_entry, read_settings_json};

/// Remove skim hook entries and marketplace registration from a settings.json value.
///
/// 1. Removes skim entries from `hooks.PreToolUse` array
/// 2. Cleans up empty arrays/objects
/// 3. Removes `skim` from `extraKnownMarketplaces`
fn remove_skim_from_settings(settings: &mut serde_json::Value) {
    let obj = match settings.as_object_mut() {
        Some(obj) => obj,
        None => return,
    };

    // Remove skim from PreToolUse
    let hooks_empty = obj
        .get_mut("hooks")
        .and_then(|h| h.as_object_mut())
        .map(|hooks_obj| {
            let ptu_empty = hooks_obj
                .get_mut("PreToolUse")
                .and_then(|ptu| ptu.as_array_mut())
                .map(|arr| {
                    arr.retain(|entry| !has_skim_hook_entry(entry));
                    arr.is_empty()
                })
                .unwrap_or(false);
            if ptu_empty {
                hooks_obj.remove("PreToolUse");
            }
            hooks_obj.is_empty()
        })
        .unwrap_or(false);
    if hooks_empty {
        obj.remove("hooks");
    }

    // Remove from extraKnownMarketplaces
    let mkts_empty = obj
        .get_mut("extraKnownMarketplaces")
        .and_then(|m| m.as_object_mut())
        .map(|mkts_obj| {
            mkts_obj.remove("skim");
            mkts_obj.is_empty()
        })
        .unwrap_or(false);
    if mkts_empty {
        obj.remove("extraKnownMarketplaces");
    }
}

pub(super) fn run_uninstall(flags: &InitFlags) -> anyhow::Result<std::process::ExitCode> {
    let config_dir = resolve_config_dir_for_agent(flags.project, flags.agent)?;
    let settings_path = config_dir.join(SETTINGS_FILE);
    let hook_script_path = config_dir.join("hooks").join(HOOK_SCRIPT_NAME);

    // Check if anything is installed
    let settings_has_hook = read_settings_json(&settings_path)
        .and_then(|json| {
            json.get("hooks")?
                .get("PreToolUse")?
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
            flags.agent.cli_name(),
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
            println!("    * Remove skim from extraKnownMarketplaces");
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
            println!("  [dry-run] Would remove skim from extraKnownMarketplaces");
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

        remove_skim_from_settings(&mut settings);

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
        let _ = crate::cmd::integrity::remove_hash_manifest(&config_dir, flags.agent.cli_name());
    }

    // Remove guidance from instruction file
    let global = !flags.project;
    super::install::remove_guidance(flags.agent, global)?;

    println!();
    println!("  skim hook has been uninstalled.");
    println!();

    Ok(std::process::ExitCode::SUCCESS)
}
