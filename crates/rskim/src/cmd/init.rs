//! Interactive hook installation for Claude Code (#44)
//!
//! `skim init` installs skim as a Claude Code PreToolUse hook, enabling
//! automatic command rewriting. Supports global (`~/.claude/`) and project-level
//! (`.claude/`) installation with idempotent, atomic writes.
//!
//! The hook script calls `skim rewrite --hook` which reads Claude Code's
//! PreToolUse JSON, rewrites matched commands, and emits `updatedInput`.
//!
//! SECURITY INVARIANT: The hook NEVER sets `permissionDecision`. Unlike
//! competitors, our hook only sets `updatedInput` and lets Claude Code's
//! permission system evaluate independently.

use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

// ============================================================================
// Constants
// ============================================================================

const HOOK_SCRIPT_NAME: &str = "skim-rewrite.sh";
const SETTINGS_FILE: &str = "settings.json";
const SETTINGS_BACKUP: &str = "settings.json.bak";

// ============================================================================
// Public entry points
// ============================================================================

/// Run the `init` subcommand.
pub(crate) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    // Unix-only guard
    if !cfg!(unix) {
        anyhow::bail!(
            "skim init is only supported on Unix systems (macOS, Linux)\n\
             Windows support is planned for a future release."
        );
    }

    // Handle --help / -h
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Parse flags
    let flags = parse_flags(args)?;

    // Non-TTY detection (B3)
    if !flags.yes && !io::stdin().is_terminal() {
        eprintln!("error: skim init requires an interactive terminal");
        eprintln!("hint: use --yes for non-interactive mode (e.g., CI)");
        return Ok(ExitCode::FAILURE);
    }

    if flags.uninstall {
        return run_uninstall(&flags);
    }

    run_install(&flags)
}

/// Build the clap `Command` definition for shell completions.
pub(super) fn command() -> clap::Command {
    clap::Command::new("init")
        .about("Install skim as a Claude Code hook")
        .arg(
            clap::Arg::new("global")
                .long("global")
                .action(clap::ArgAction::SetTrue)
                .help("Install to user-level ~/.claude/ (default)"),
        )
        .arg(
            clap::Arg::new("project")
                .long("project")
                .action(clap::ArgAction::SetTrue)
                .help("Install to .claude/ in current directory"),
        )
        .arg(
            clap::Arg::new("yes")
                .long("yes")
                .short('y')
                .action(clap::ArgAction::SetTrue)
                .help("Non-interactive mode (skip prompts)"),
        )
        .arg(
            clap::Arg::new("dry-run")
                .long("dry-run")
                .action(clap::ArgAction::SetTrue)
                .help("Print actions without writing"),
        )
        .arg(
            clap::Arg::new("uninstall")
                .long("uninstall")
                .action(clap::ArgAction::SetTrue)
                .help("Remove hook and clean up"),
        )
}

// ============================================================================
// Flag parsing
// ============================================================================

#[derive(Debug)]
struct InitFlags {
    project: bool,
    yes: bool,
    dry_run: bool,
    uninstall: bool,
}

fn parse_flags(args: &[String]) -> anyhow::Result<InitFlags> {
    let mut project = false;
    let mut yes = false;
    let mut dry_run = false;
    let mut uninstall = false;

    for arg in args {
        match arg.as_str() {
            "--global" => { /* default, no-op */ }
            "--project" => project = true,
            "--yes" | "-y" => yes = true,
            "--dry-run" => dry_run = true,
            "--uninstall" => uninstall = true,
            other => {
                anyhow::bail!(
                    "unknown flag: '{other}'\n\
                     Run 'skim init --help' for usage information"
                );
            }
        }
    }

    Ok(InitFlags {
        project,
        yes,
        dry_run,
        uninstall,
    })
}

// ============================================================================
// State detection (B5)
// ============================================================================

struct DetectedState {
    skim_binary: PathBuf,
    skim_version: String,
    config_dir: PathBuf,
    settings_path: PathBuf,
    settings_exists: bool,
    hook_installed: bool,
    hook_version: Option<String>,
    marketplace_installed: bool,
    /// If installing to one scope and the other scope also has a hook
    dual_scope_warning: Option<String>,
}

fn detect_state(flags: &InitFlags) -> anyhow::Result<DetectedState> {
    let skim_binary = std::env::current_exe()?;
    let skim_version = env!("CARGO_PKG_VERSION").to_string();
    let config_dir = resolve_config_dir(flags.project)?;
    let settings_path = config_dir.join(SETTINGS_FILE);
    let settings_exists = settings_path.exists();

    let mut hook_installed = false;
    let mut hook_version = None;
    let mut marketplace_installed = false;

    if let Some(json) = read_settings_json(&settings_path) {
        if let Some(arr) = json
            .get("hooks")
            .and_then(|h| h.get("PreToolUse"))
            .and_then(|v| v.as_array())
        {
            for entry in arr {
                if has_skim_hook_entry(entry) {
                    hook_installed = true;
                    hook_version = extract_hook_version_from_entry(entry, &config_dir);
                }
            }
        }
        if json
            .get("extraKnownMarketplaces")
            .and_then(|m| m.get("skim"))
            .is_some()
        {
            marketplace_installed = true;
        }
    }

    // Dual-scope check (B5)
    let dual_scope_warning = check_dual_scope(flags)?;

    Ok(DetectedState {
        skim_binary,
        skim_version,
        config_dir,
        settings_path,
        settings_exists,
        hook_installed,
        hook_version,
        marketplace_installed,
        dual_scope_warning,
    })
}

fn check_dual_scope(flags: &InitFlags) -> anyhow::Result<Option<String>> {
    let other_dir = if flags.project {
        // Installing project-level, check global
        resolve_config_dir(false)?
    } else {
        // Installing global, check project
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(".claude"),
            Err(_) => return Ok(None),
        }
    };

    let other_settings = other_dir.join(SETTINGS_FILE);
    let has_hook = read_settings_json(&other_settings)
        .and_then(|json| {
            json.get("hooks")?
                .get("PreToolUse")?
                .as_array()
                .map(|arr| arr.iter().any(has_skim_hook_entry))
        })
        .unwrap_or(false);

    if !has_hook {
        return Ok(None);
    }

    let scope = if flags.project {
        "globally"
    } else {
        "in project"
    };
    let uninstall_scope = if flags.project {
        "--global"
    } else {
        "--project"
    };
    let path = other_settings.display();
    Ok(Some(format!(
        "skim hook is also installed {scope} ({path})\n  \
         Both hooks will fire, but this is harmless -- the second is a no-op.\n  \
         To remove: skim init {uninstall_scope} --uninstall"
    )))
}

/// Read and parse a settings.json file, returning `None` on any failure.
fn read_settings_json(path: &Path) -> Option<serde_json::Value> {
    let contents = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Check if a PreToolUse entry contains a skim hook (substring match on "skim-rewrite").
fn has_skim_hook_entry(entry: &serde_json::Value) -> bool {
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
}

/// Try to extract the skim version from the hook script referenced in a settings entry.
fn extract_hook_version_from_entry(entry: &serde_json::Value, config_dir: &Path) -> Option<String> {
    let hooks = entry.get("hooks")?.as_array()?;
    for hook in hooks {
        let cmd = hook.get("command")?.as_str()?;
        if cmd.contains("skim-rewrite") {
            // Try reading the script file
            let script_path = if cmd.starts_with('/') || cmd.starts_with('.') {
                PathBuf::from(cmd)
            } else {
                config_dir.join("hooks").join(HOOK_SCRIPT_NAME)
            };
            if let Ok(contents) = std::fs::read_to_string(script_path) {
                for line in contents.lines() {
                    if let Some(ver) = line.strip_prefix("# skim-hook v").or_else(|| {
                        line.strip_prefix("export SKIM_HOOK_VERSION=\"")
                            .and_then(|s| s.strip_suffix('"'))
                    }) {
                        return Some(ver.to_string());
                    }
                }
            }
        }
    }
    None
}

// ============================================================================
// Config directory resolution (B6)
// ============================================================================

fn resolve_config_dir(project: bool) -> anyhow::Result<PathBuf> {
    if project {
        Ok(std::env::current_dir()?.join(".claude"))
    } else if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        Ok(PathBuf::from(dir))
    } else {
        Ok(dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?
            .join(".claude"))
    }
}

// ============================================================================
// Install flow
// ============================================================================

fn run_install(flags: &InitFlags) -> anyhow::Result<ExitCode> {
    let state = detect_state(flags)?;

    // Print header
    println!();
    println!("  skim init -- Claude Code integration setup");
    println!();

    // Print detected state
    println!("  Checking current state...");
    println!(
        "  {} skim binary: {} (v{})",
        check_mark(true),
        state.skim_binary.display(),
        state.skim_version
    );

    let config_label = if state.settings_exists {
        "exists"
    } else {
        "will be created"
    };
    println!(
        "  {} Claude config: {} ({})",
        check_mark(state.settings_exists),
        state.settings_path.display(),
        config_label
    );

    let hook_label = if state.hook_installed {
        match &state.hook_version {
            Some(v) if v == &state.skim_version => format!("installed (v{v})"),
            Some(v) => format!("installed (v{v} -> v{} available)", state.skim_version),
            None => "installed".to_string(),
        }
    } else {
        "not installed".to_string()
    };
    println!(
        "  {} Hook: {}",
        check_mark(state.hook_installed),
        hook_label
    );
    println!();

    // Already up to date check
    if state.hook_installed
        && state.hook_version.as_deref() == Some(&state.skim_version)
        && state.marketplace_installed
    {
        println!("  Already up to date. Nothing to do.");
        println!();
        return Ok(ExitCode::SUCCESS);
    }

    // Dual-scope warning
    if let Some(ref warning) = state.dual_scope_warning {
        println!("  WARNING: {warning}");
        println!();
    }

    // Interactive prompts (or defaults for --yes)
    let install_marketplace = if flags.yes {
        true
    } else {
        // Scope prompt (informational — scope is already determined by --project flag)
        if !flags.project {
            println!("  ? Where should skim install the hook?");
            println!("    [1] Global (~/.claude/settings.json)  [recommended]");
            println!("    [2] Project (.claude/settings.json)");
            let choice = prompt_choice("  Choice [1]: ", 1, &[1, 2])?;
            if choice == 2 {
                // User chose project but didn't pass --project flag
                // Re-run with project scope
                println!();
                println!("  Tip: use `skim init --project` to skip this prompt next time.");
                let project_flags = InitFlags {
                    project: true,
                    yes: flags.yes,
                    dry_run: flags.dry_run,
                    uninstall: false,
                };
                return run_install_inner(&project_flags);
            }
            println!();
        }

        // Plugin prompt
        if !state.marketplace_installed {
            println!("  ? Install the Skimmer plugin? (codebase orientation agent)");
            println!("    Adds /skim command and auto-orientation for new codebases");
            println!("    [1] Yes  [recommended]");
            println!("    [2] No");
            let choice = prompt_choice("  Choice [1]: ", 1, &[1, 2])?;
            println!();
            choice == 1
        } else {
            true
        }
    };

    // Print summary
    let hook_script_path = state.config_dir.join("hooks").join(HOOK_SCRIPT_NAME);
    println!("  Summary:");
    if !state.hook_installed || state.hook_version.as_deref() != Some(&state.skim_version) {
        println!("    * Create hook script: {}", hook_script_path.display());
        println!(
            "    * Patch settings: {} (add PreToolUse hook)",
            state.settings_path.display()
        );
    }
    if install_marketplace && !state.marketplace_installed {
        println!("    * Register marketplace: skim (dean0x/skim)");
    }
    println!();

    // Confirmation
    if !flags.yes && !confirm_proceed()? {
        println!("  Cancelled.");
        return Ok(ExitCode::SUCCESS);
    }

    if flags.dry_run {
        print_dry_run_actions(&state, install_marketplace);
        return Ok(ExitCode::SUCCESS);
    }

    // Execute installation
    execute_install(&state, install_marketplace)?;

    println!();
    println!("  Done! skim is now active in Claude Code.");
    println!();
    if install_marketplace {
        println!("  Next step -- install the Skimmer plugin in Claude Code:");
        println!("    /install skimmer@skim");
        println!();
    }

    Ok(ExitCode::SUCCESS)
}

/// Inner install function for when user changes scope interactively.
fn run_install_inner(flags: &InitFlags) -> anyhow::Result<ExitCode> {
    let state = detect_state(flags)?;
    let hook_script_path = state.config_dir.join("hooks").join(HOOK_SCRIPT_NAME);

    println!();
    println!("  Summary:");
    println!("    * Create hook script: {}", hook_script_path.display());
    println!(
        "    * Patch settings: {} (add PreToolUse hook)",
        state.settings_path.display()
    );
    println!("    * Register marketplace: skim (dean0x/skim)");
    println!();

    // No confirmation needed — already confirmed scope change

    if flags.dry_run {
        print_dry_run_actions(&state, true);
        return Ok(ExitCode::SUCCESS);
    }

    execute_install(&state, true)?;

    println!();
    println!("  Done! skim is now active in Claude Code.");
    println!();
    println!("  Next step -- install the Skimmer plugin in Claude Code:");
    println!("    /install skimmer@skim");
    println!();

    Ok(ExitCode::SUCCESS)
}

fn execute_install(state: &DetectedState, install_marketplace: bool) -> anyhow::Result<()> {
    // B7: Create hook script
    create_hook_script(state)?;

    // B8: Patch settings.json
    patch_settings(state, install_marketplace)?;

    Ok(())
}

// ============================================================================
// Hook script generation (B7)
// ============================================================================

fn create_hook_script(state: &DetectedState) -> anyhow::Result<()> {
    let hooks_dir = state.config_dir.join("hooks");
    let script_path = hooks_dir.join(HOOK_SCRIPT_NAME);

    // Create hooks directory if needed
    if !hooks_dir.exists() {
        std::fs::create_dir_all(&hooks_dir)?;
        #[cfg(unix)]
        {
            let perms = std::fs::Permissions::from_mode(0o755);
            std::fs::set_permissions(&hooks_dir, perms)?;
        }
    }

    // Check if existing script has same version (idempotent)
    if script_path.exists() {
        if let Ok(contents) = std::fs::read_to_string(&script_path) {
            let version_line = format!("# skim-hook v{}", state.skim_version);
            if contents.contains(&version_line) {
                println!(
                    "  {} Skipped: {} (already v{})",
                    check_mark(true),
                    script_path.display(),
                    state.skim_version
                );
                return Ok(());
            }
            // Different version — will overwrite
            if let Some(old_ver) = &state.hook_version {
                println!(
                    "  {} Updated: {} (v{} -> v{})",
                    check_mark(true),
                    script_path.display(),
                    old_ver,
                    state.skim_version
                );
            } else {
                println!("  {} Updated: {}", check_mark(true), script_path.display());
            }
        }
    } else {
        println!("  {} Created: {}", check_mark(true), script_path.display());
    }

    // Generate script content
    // Binary path is quoted to handle spaces
    let binary_path = state.skim_binary.display();
    let script_content = format!(
        "#!/usr/bin/env bash\n\
         # skim-hook v{version}\n\
         # Generated by: skim init -- do not edit manually\n\
         export SKIM_HOOK_VERSION=\"{version}\"\n\
         exec \"{binary_path}\" rewrite --hook\n",
        version = state.skim_version,
    );

    std::fs::write(&script_path, script_content)?;

    // Set executable permissions
    #[cfg(unix)]
    {
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&script_path, perms)?;
    }

    Ok(())
}

// ============================================================================
// Settings.json patching (B8)
// ============================================================================

fn patch_settings(state: &DetectedState, install_marketplace: bool) -> anyhow::Result<()> {
    // Ensure config dir exists
    if !state.config_dir.exists() {
        std::fs::create_dir_all(&state.config_dir)?;
    }

    // Resolve symlinks before writing (don't replace symlink with regular file)
    let real_settings_path = if state.settings_path.is_symlink() {
        std::fs::read_link(&state.settings_path)?
    } else {
        state.settings_path.clone()
    };

    // Read existing settings or start fresh
    let mut settings: serde_json::Value = if state.settings_exists {
        let contents = std::fs::read_to_string(&real_settings_path)?;
        if contents.trim().is_empty() {
            // Empty file — treat as {}
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            serde_json::from_str(&contents).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to parse {}: {}\n\
                     hint: Fix the JSON manually, then re-run `skim init`",
                    real_settings_path.display(),
                    e
                )
            })?
        }
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };

    let obj = settings
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("settings.json root is not an object"))?;

    // Back up existing file
    if state.settings_exists {
        let backup_path = state.config_dir.join(SETTINGS_BACKUP);
        std::fs::copy(&real_settings_path, &backup_path)?;
        println!(
            "  {} Backed up: {} -> {}",
            check_mark(true),
            state.settings_path.display(),
            SETTINGS_BACKUP
        );
    }

    // Build the hook script path
    let hook_script_path = state.config_dir.join("hooks").join(HOOK_SCRIPT_NAME);
    let hook_script_str = hook_script_path.display().to_string();

    // Ensure hooks.PreToolUse array exists
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("settings.json 'hooks' is not an object"))?;

    let pre_tool_use = hooks
        .entry("PreToolUse")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("settings.json 'hooks.PreToolUse' is not an array"))?;

    // Search for existing skim entry and remove it (to update in place)
    pre_tool_use.retain(|entry| !has_skim_hook_entry(entry));

    // Build the new hook entry
    let hook_entry = serde_json::json!({
        "matcher": "Bash",
        "hooks": [{
            "type": "command",
            "command": hook_script_str,
            "timeout": 5
        }]
    });
    pre_tool_use.push(hook_entry);

    // Add marketplace (if opted in)
    if install_marketplace {
        let marketplaces = obj
            .entry("extraKnownMarketplaces")
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .ok_or_else(|| {
                anyhow::anyhow!("settings.json 'extraKnownMarketplaces' is not an object")
            })?;

        marketplaces.insert(
            "skim".to_string(),
            serde_json::json!({"source": {"source": "github", "repo": "dean0x/skim"}}),
        );
    }

    // Atomic write: write to tmp, then rename
    let pretty = serde_json::to_string_pretty(&settings)?;
    let tmp_path = real_settings_path.with_extension("json.tmp");
    std::fs::write(&tmp_path, format!("{pretty}\n"))?;
    std::fs::rename(&tmp_path, &real_settings_path)?;

    println!(
        "  {} Patched: {} (PreToolUse hook added)",
        check_mark(true),
        state.settings_path.display()
    );

    if install_marketplace {
        println!(
            "  {} Registered: skim marketplace in {}",
            check_mark(true),
            SETTINGS_FILE
        );
    }

    Ok(())
}

// ============================================================================
// Uninstall flow (B10)
// ============================================================================

fn run_uninstall(flags: &InitFlags) -> anyhow::Result<ExitCode> {
    let config_dir = resolve_config_dir(flags.project)?;
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
        return Ok(ExitCode::SUCCESS);
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
            return Ok(ExitCode::SUCCESS);
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
        return Ok(ExitCode::SUCCESS);
    }

    // Remove from settings.json
    if settings_has_hook {
        // Resolve symlinks
        let real_path = if settings_path.is_symlink() {
            std::fs::read_link(&settings_path)?
        } else {
            settings_path.clone()
        };

        let contents = std::fs::read_to_string(&real_path)?;
        let mut settings: serde_json::Value = serde_json::from_str(&contents)?;

        if let Some(obj) = settings.as_object_mut() {
            // Remove skim from PreToolUse
            if let Some(hooks) = obj.get_mut("hooks") {
                if let Some(hooks_obj) = hooks.as_object_mut() {
                    if let Some(ptu) = hooks_obj.get_mut("PreToolUse") {
                        if let Some(arr) = ptu.as_array_mut() {
                            arr.retain(|entry| !has_skim_hook_entry(entry));
                            // Clean up empty array
                            if arr.is_empty() {
                                hooks_obj.remove("PreToolUse");
                            }
                        }
                    }
                    // Clean up empty hooks object
                    if hooks_obj.is_empty() {
                        obj.remove("hooks");
                    }
                }
            }

            // Remove from extraKnownMarketplaces
            if let Some(mkts) = obj.get_mut("extraKnownMarketplaces") {
                if let Some(mkts_obj) = mkts.as_object_mut() {
                    mkts_obj.remove("skim");
                    if mkts_obj.is_empty() {
                        obj.remove("extraKnownMarketplaces");
                    }
                }
            }
        }

        // Atomic write
        let pretty = serde_json::to_string_pretty(&settings)?;
        let tmp_path = real_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, format!("{pretty}\n"))?;
        std::fs::rename(&tmp_path, &real_path)?;

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

    println!();
    println!("  skim hook has been uninstalled.");
    println!();

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Dry-run output (B11)
// ============================================================================

fn print_dry_run_actions(state: &DetectedState, install_marketplace: bool) {
    let hook_script_path = state.config_dir.join("hooks").join(HOOK_SCRIPT_NAME);

    println!("  [dry-run] Would create: {}", hook_script_path.display());
    if state.settings_exists {
        println!(
            "  [dry-run] Would back up: {} -> {}",
            state.settings_path.display(),
            SETTINGS_BACKUP
        );
    }
    println!(
        "  [dry-run] Would patch: {} (add PreToolUse hook)",
        state.settings_path.display()
    );
    if install_marketplace {
        println!(
            "  [dry-run] Would register: skim marketplace in {}",
            SETTINGS_FILE
        );
    }
}

// ============================================================================
// Interactive prompt helpers
// ============================================================================

fn prompt_choice(prompt: &str, default: u32, valid: &[u32]) -> anyhow::Result<u32> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(default);
    }
    match trimmed.parse::<u32>() {
        Ok(n) if valid.contains(&n) => Ok(n),
        _ => Ok(default),
    }
}

/// Prompt the user with "Proceed? [Y/n]" and return `true` if confirmed.
fn confirm_proceed() -> anyhow::Result<bool> {
    print!("  ? Proceed? [Y/n] ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let trimmed = input.trim().to_lowercase();
    let confirmed = trimmed.is_empty() || trimmed == "y" || trimmed == "yes";
    if confirmed {
        println!();
    }
    Ok(confirmed)
}

fn check_mark(ok: bool) -> &'static str {
    if ok {
        "\x1b[32m+\x1b[0m"
    } else {
        "\x1b[31m-\x1b[0m"
    }
}

// ============================================================================
// Help text
// ============================================================================

fn print_help() {
    println!("skim init");
    println!();
    println!("  Install skim as a Claude Code hook for automatic command rewriting");
    println!();
    println!("Usage: skim init [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --global       Install to user-level ~/.claude/ (default)");
    println!("  --project      Install to .claude/ in current directory");
    println!("  --yes, -y      Non-interactive mode (skip prompts)");
    println!("  --dry-run      Print actions without writing");
    println!("  --uninstall    Remove hook and clean up");
    println!("  --help, -h     Print help information");
    println!();
    println!("Examples:");
    println!("  skim init                   Interactive setup (recommended)");
    println!("  skim init --yes             Non-interactive with defaults");
    println!("  skim init --project --yes   Install project-level hook");
    println!("  skim init --uninstall       Remove skim hook");
    println!("  skim init --dry-run         Preview actions without writing");
}
