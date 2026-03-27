//! Install flow for `skim init`.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::flags::InitFlags;
use super::helpers::{
    check_mark, confirm_proceed, prompt_choice, HOOK_SCRIPT_NAME, SETTINGS_BACKUP, SETTINGS_FILE,
};
use super::state::{detect_state, has_skim_hook_entry, DetectedState};

/// Resolved install options from interactive prompts or --yes defaults.
struct InstallOptions {
    /// Whether to use project scope (overrides flags.project when user selects it interactively).
    project: bool,
    /// Whether to install the marketplace entry.
    install_marketplace: bool,
    /// Whether confirmation was already handled by the prompting phase.
    skip_confirmation: bool,
}

/// Prompt the user for install options (scope and marketplace).
///
/// In non-interactive mode (--yes), returns defaults immediately.
/// Returns `None` if the user chose project scope interactively (requires re-detection).
fn prompt_install_options(
    flags: &InitFlags,
    state: &DetectedState,
) -> anyhow::Result<InstallOptions> {
    if flags.yes {
        return Ok(InstallOptions {
            project: flags.project,
            install_marketplace: true,
            skip_confirmation: true,
        });
    }

    let mut use_project = flags.project;
    let mut skip_confirmation = false;

    // Scope prompt (informational -- scope is already determined by --project flag)
    if !flags.project {
        println!("  ? Where should skim install the hook?");
        println!("    [1] Global (~/.claude/settings.json)  [recommended]");
        println!("    [2] Project (.claude/settings.json)");
        let choice = prompt_choice("  Choice [1]: ", 1, &[1, 2])?;
        if choice == 2 {
            println!();
            println!("  Tip: use `skim init --project` to skip this prompt next time.");
            use_project = true;
            // User already made a deliberate scope choice -- skip confirmation later
            skip_confirmation = true;
        }
        println!();
    }

    // Plugin prompt
    let install_marketplace = if !state.marketplace_installed {
        println!("  ? Install the Skimmer plugin? (codebase orientation agent)");
        println!("    Adds /skim command and auto-orientation for new codebases");
        println!("    [1] Yes  [recommended]");
        println!("    [2] No");
        let choice = prompt_choice("  Choice [1]: ", 1, &[1, 2])?;
        println!();
        choice == 1
    } else {
        true
    };

    Ok(InstallOptions {
        project: use_project,
        install_marketplace,
        skip_confirmation,
    })
}

/// Verify that the target agent appears to be installed on this system.
///
/// Checks for the expected config directory. If the agent's config dir
/// doesn't exist, returns an error with a helpful message rather than
/// silently creating an orphan config.
fn verify_agent_installed(state: &DetectedState, flags: &InitFlags) -> anyhow::Result<()> {
    use crate::cmd::session::AgentKind;

    // Claude Code: always proceed (we create ~/.claude/ if needed)
    if flags.agent == AgentKind::ClaudeCode {
        return Ok(());
    }

    // For --project mode, we always create the dir, so skip the check
    if flags.project {
        return Ok(());
    }

    // Check if the config dir exists (or a parent indicator)
    if !state.config_dir.exists() {
        let hint = match flags.agent {
            AgentKind::Cursor => "Install Cursor from https://cursor.com",
            AgentKind::GeminiCli => "Install Gemini CLI: npm install -g @google/gemini-cli",
            AgentKind::CopilotCli => {
                "Install GitHub Copilot CLI: gh extension install github/gh-copilot"
            }
            AgentKind::CodexCli => "Install Codex CLI: npm install -g @openai/codex",
            AgentKind::OpenCode => {
                "Install OpenCode: go install github.com/opencode-ai/opencode@latest"
            }
            AgentKind::ClaudeCode => unreachable!("handled above"),
        };
        anyhow::bail!(
            "{} does not appear to be installed (config dir not found: {})\nhint: {}",
            flags.agent.display_name(),
            state.config_dir.display(),
            hint
        );
    }

    Ok(())
}

pub(super) fn run_install(flags: &InitFlags) -> anyhow::Result<std::process::ExitCode> {
    let state = detect_state(flags)?;

    // Verify agent is installed before proceeding
    verify_agent_installed(&state, flags)?;

    // Print header
    println!();
    println!(
        "  skim init -- {} integration setup",
        flags.agent.display_name()
    );
    println!();

    // Print detected state
    print_detected_state(&state);

    // Plugin collision warning: other Bash PreToolUse hooks exist
    if !state.existing_bash_hooks.is_empty() {
        println!("  WARNING: Other Bash PreToolUse hooks detected:");
        for hook_cmd in &state.existing_bash_hooks {
            println!("    - {hook_cmd}");
        }
        println!("  Both hooks will fire on Bash commands. This is usually harmless");
        println!("  but may cause unexpected behavior if the other hook also modifies commands.");
        println!();
    }

    // Already up to date check
    if state.hook_installed
        && state.hook_version.as_deref() == Some(&state.skim_version)
        && state.marketplace_installed
    {
        println!("  Already up to date. Nothing to do.");
        println!();
        return Ok(std::process::ExitCode::SUCCESS);
    }

    // Dual-scope warning
    if let Some(ref warning) = state.dual_scope_warning {
        println!("  WARNING: {warning}");
        println!();
    }

    // Prompt for options (or use defaults for --yes)
    let options = prompt_install_options(flags, &state)?;

    // Re-detect state with the resolved scope (may differ from flags if user
    // changed scope interactively)
    let flags_override = InitFlags {
        project: options.project,
        yes: flags.yes,
        dry_run: flags.dry_run,
        uninstall: false,
        force: flags.force,
        agent: flags.agent,
    };
    let state = detect_state(&flags_override)?;

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
    if options.install_marketplace && !state.marketplace_installed {
        println!("    * Register marketplace: skim (dean0x/skim)");
    }
    println!();

    // Confirmation (skip if user already confirmed via scope change or --yes)
    if !flags.yes && !options.skip_confirmation && !confirm_proceed()? {
        println!("  Cancelled.");
        return Ok(std::process::ExitCode::SUCCESS);
    }

    if flags_override.dry_run {
        print_dry_run_actions(&state, options.install_marketplace);
        return Ok(std::process::ExitCode::SUCCESS);
    }

    // Execute installation
    execute_install(&state, options.install_marketplace)?;

    println!();
    println!(
        "  Done! skim is now active in {}.",
        flags_override.agent.display_name()
    );
    println!();
    if options.install_marketplace {
        println!(
            "  Next step -- install the Skimmer plugin in {}:",
            flags_override.agent.display_name()
        );
        println!("    /install skimmer@skim");
        println!();
    }

    Ok(std::process::ExitCode::SUCCESS)
}

/// Print the detected state summary to stdout.
pub(super) fn print_detected_state(state: &DetectedState) {
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
        "  {} Config: {} ({})",
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

/// Validate that a path is safe to interpolate into a double-quoted bash string.
///
/// Rejects characters that can escape double-quote context or inject commands:
/// - `"` (closes the quote)
/// - `` ` `` (command substitution)
/// - `$` (variable/command expansion)
/// - `\` (escape sequences)
/// - newline / null byte (command injection)
///
/// Paths from `current_exe()` on any mainstream OS should never contain these,
/// so this guard only fires on adversarial or corrupted environments.
fn validate_shell_safe_path(path: &str) -> anyhow::Result<()> {
    const UNSAFE_CHARS: &[char] = &['"', '`', '$', '\\', '\n', '\0'];
    if let Some(bad) = path.chars().find(|c| UNSAFE_CHARS.contains(c)) {
        anyhow::bail!(
            "binary path contains shell-unsafe character {:?}: {}\n\
             hint: reinstall skim to a path without special characters",
            bad,
            path
        );
    }
    Ok(())
}

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
    // Binary path is quoted to handle spaces, but we must also reject
    // characters that can escape double-quote context in bash.
    let binary_path = state.skim_binary.display().to_string();
    validate_shell_safe_path(&binary_path)?;

    let agent_flag = if state.agent_cli_name == "claude-code" {
        String::new()
    } else {
        format!(" --agent {}", state.agent_cli_name)
    };
    let script_content = format!(
        "#!/usr/bin/env bash\n\
         # skim-hook v{version}\n\
         # Generated by: skim init -- do not edit manually\n\
         export SKIM_HOOK_VERSION=\"{version}\"\n\
         exec \"{binary_path}\" rewrite --hook{agent_flag}\n",
        version = state.skim_version,
    );

    // Atomic write: write to tmp, then rename to final path.
    // A crash mid-write produces a tmp file instead of a truncated script.
    let tmp_path = hooks_dir.join(format!("{HOOK_SCRIPT_NAME}.tmp"));
    std::fs::write(&tmp_path, script_content)?;

    // Set executable permissions on the tmp file before renaming
    #[cfg(unix)]
    {
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&tmp_path, perms)?;
    }

    std::fs::rename(&tmp_path, &script_path)?;

    // Compute and store SHA-256 hash for integrity verification (#57)
    if let Ok(hash) = crate::cmd::integrity::compute_file_hash(&script_path) {
        let _ = crate::cmd::integrity::write_hash_manifest(
            &state.config_dir,
            state.agent_cli_name,
            HOOK_SCRIPT_NAME,
            &hash,
        );
    }

    Ok(())
}

// ============================================================================
// Settings.json patching (B8)
// ============================================================================

use super::helpers::{atomic_write_settings, load_or_create_settings, resolve_real_settings_path};

/// Back up the settings file before modification.
///
/// Re-checks that `real_path` is not a symlink immediately before copying to
/// close the TOCTOU window between `resolve_real_settings_path()` and the
/// actual I/O. Without this guard, an attacker could replace the file with a
/// symlink after resolution, causing `fs::copy` to overwrite an arbitrary
/// target.
fn backup_settings(
    config_dir: &std::path::Path,
    real_path: &std::path::Path,
) -> anyhow::Result<()> {
    // Guard: reject if the path became a symlink since resolution
    if real_path.is_symlink() {
        anyhow::bail!(
            "settings path became a symlink after resolution: {}\n\
             hint: this may indicate a symlink race; please verify the path manually",
            real_path.display()
        );
    }
    let backup_path = config_dir.join(SETTINGS_BACKUP);
    std::fs::copy(real_path, &backup_path)?;
    Ok(())
}

/// Insert or update the skim hook entry in `hooks.PreToolUse`.
fn upsert_hook_entry(
    settings: &mut serde_json::Value,
    hook_script_path: &str,
) -> anyhow::Result<()> {
    let obj = settings
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("settings.json root is not an object"))?;

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

    // Remove existing skim entry (to update in place)
    pre_tool_use.retain(|entry| !has_skim_hook_entry(entry));

    // Insert new entry
    pre_tool_use.push(serde_json::json!({
        "matcher": "Bash",
        "hooks": [{
            "type": "command",
            "command": hook_script_path,
            "timeout": 5
        }]
    }));

    Ok(())
}

fn patch_settings(state: &DetectedState, install_marketplace: bool) -> anyhow::Result<()> {
    // Ensure config dir exists
    if !state.config_dir.exists() {
        std::fs::create_dir_all(&state.config_dir)?;
    }

    let real_path = resolve_real_settings_path(&state.settings_path)?;
    let mut settings = load_or_create_settings(&real_path)?;

    // Back up existing file (re-check existence to avoid TOCTOU race)
    if real_path.exists() {
        backup_settings(&state.config_dir, &real_path)?;
        println!(
            "  {} Backed up: {} -> {}",
            check_mark(true),
            state.settings_path.display(),
            SETTINGS_BACKUP
        );
    }

    // Upsert hook entry
    let hook_script_path = state.config_dir.join("hooks").join(HOOK_SCRIPT_NAME);
    upsert_hook_entry(&mut settings, &hook_script_path.display().to_string())?;

    // Add marketplace (if opted in)
    if install_marketplace {
        let obj = settings
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("settings.json root is not an object"))?;

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

    atomic_write_settings(&settings, &real_path)?;

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
// Dry-run output (B11)
// ============================================================================

pub(super) fn print_dry_run_actions(state: &DetectedState, install_marketplace: bool) {
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
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upsert_hook_entry_idempotent() {
        let mut settings = serde_json::json!({});
        upsert_hook_entry(&mut settings, "/path/to/skim-rewrite.sh").unwrap();
        upsert_hook_entry(&mut settings, "/path/to/skim-rewrite.sh").unwrap();

        let entries = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(
            entries.len(),
            1,
            "running upsert twice should produce exactly one entry, not a duplicate"
        );
    }

    // ---- Shell-safe path validation (SEC-1) ----

    #[test]
    fn test_validate_shell_safe_path_normal_paths() {
        assert!(validate_shell_safe_path("/usr/local/bin/skim").is_ok());
        assert!(validate_shell_safe_path("/home/user/.cargo/bin/skim").is_ok());
        assert!(validate_shell_safe_path("/path/with spaces/skim").is_ok());
    }

    #[test]
    fn test_validate_shell_safe_path_rejects_double_quote() {
        let result = validate_shell_safe_path("/path/with\"quote/skim");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("shell-unsafe"));
    }

    #[test]
    fn test_validate_shell_safe_path_rejects_backtick() {
        assert!(validate_shell_safe_path("/path/with`cmd`/skim").is_err());
    }

    #[test]
    fn test_validate_shell_safe_path_rejects_dollar() {
        assert!(validate_shell_safe_path("/path/$HOME/skim").is_err());
    }

    #[test]
    fn test_validate_shell_safe_path_rejects_backslash() {
        assert!(validate_shell_safe_path("/path/with\\escape/skim").is_err());
    }

    #[test]
    fn test_validate_shell_safe_path_rejects_newline() {
        assert!(validate_shell_safe_path("/path/with\nnewline/skim").is_err());
    }

    #[test]
    fn test_validate_shell_safe_path_rejects_null_byte() {
        assert!(validate_shell_safe_path("/path/with\0null/skim").is_err());
    }
}
