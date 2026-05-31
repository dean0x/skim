//! Install flow for `skim init`.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::flags::{DetectionEnv, InitFlags, detect_installed_agents, resolve_single_agent};
use super::helpers::{
    HOOK_SCRIPT_NAME, SETTINGS_BACKUP, atomic_write_settings, check_mark, load_or_create_settings,
    resolve_real_settings_path,
};
use super::state::{DetectedState, detect_state, has_skim_hook_entry, read_settings_json};
use crate::cmd::hooks::{generate_hook_script, protocol_for_agent};
use crate::cmd::session::{AgentKind, InstructionEnv};

/// Verify that the target agent appears to be installed on this system.
///
/// Checks for the expected config directory. If the agent's config dir
/// doesn't exist, returns an error with a helpful message rather than
/// silently creating an orphan config.
fn verify_agent_installed(
    state: &DetectedState,
    agent: AgentKind,
    flags: &InitFlags,
) -> anyhow::Result<()> {
    // Claude Code: always proceed (we create ~/.claude/ if needed)
    if agent == AgentKind::ClaudeCode {
        return Ok(());
    }

    // For --project mode, we always create the dir, so skip the check
    if flags.project {
        return Ok(());
    }

    // Check if the config dir exists (or a parent indicator)
    if !state.config_dir.exists() {
        let hint = match agent {
            AgentKind::Cursor => "Install Cursor from https://cursor.com",
            AgentKind::GeminiCli => "Install Gemini CLI: npm install -g @google/gemini-cli",
            AgentKind::CopilotCli => {
                "Install GitHub Copilot CLI: gh extension install github/gh-copilot"
            }
            AgentKind::CodexCli => "Install Codex CLI: npm install -g @openai/codex",
            AgentKind::Crush => "Install Crush from https://crushcode.ai",
            AgentKind::ClaudeCode => unreachable!("handled above"),
        };
        anyhow::bail!(
            "{} does not appear to be installed (config dir not found: {})\nhint: {}",
            agent.display_name(),
            state.config_dir.display(),
            hint
        );
    }

    Ok(())
}

/// Return `true` when the guidance section in the agent's instruction file is
/// already at the current skim version (or when guidance is disabled).
///
/// Returns `true` (treat as current) when:
/// - `--no-guidance` is set, or
/// - The agent has no instruction file (guidance feature not applicable), or
/// - The file content contains the versioned start marker for `skim_version`.
fn is_guidance_current(
    agent: AgentKind,
    flags: &InitFlags,
    skim_version: &str,
    env: &InstructionEnv,
) -> bool {
    if flags.no_guidance {
        return true;
    }
    let global = !flags.project;
    agent
        .instruction_file(global, env)
        .map(|p| {
            std::fs::read_to_string(&p)
                .ok()
                .map(|c| c.contains(&format!("{} v{}", GUIDANCE_START, skim_version)))
                .unwrap_or(false)
        })
        .unwrap_or(true) // No instruction file path = guidance not applicable for this agent
}

fn print_install_header(agent_name: &str) {
    println!();
    println!("  skim init -- {} integration setup", agent_name);
    println!();
}

fn print_collision_warning(hooks: &[String]) {
    println!("  WARNING: Other hooks detected for the same tool matcher:");
    for hook_cmd in hooks {
        println!("    - {hook_cmd}");
    }
    println!("  Both hooks will fire on Bash commands. This is usually harmless");
    println!("  but may cause unexpected behavior if the other hook also modifies commands.");
    println!();
}

fn print_already_up_to_date() {
    println!("  Already up to date. Nothing to do.");
    println!();
}

fn print_dual_scope_warning(warning: &str) {
    println!("  WARNING: {warning}");
    println!();
}

fn print_install_summary(state: &DetectedState) {
    println!("  Summary:");
    if !state.hook_installed || !state.hook_is_current() {
        let hook_script_path = state.config_dir.join("hooks").join(HOOK_SCRIPT_NAME);
        println!("    * Create hook script: {}", hook_script_path.display());
        println!(
            "    * Patch settings: {} (add PreToolUse hook)",
            state.settings_path.display()
        );
    }
    println!();
}

fn print_completion_message(agent_name: &str) {
    println!();
    println!("  Done! skim is now active in {agent_name}.");
    println!();
}

pub(super) fn run_install(flags: &InitFlags) -> anyhow::Result<std::process::ExitCode> {
    if let Some(agent) = resolve_single_agent(flags) {
        // Explicit --agent: single-agent mode
        run_install_single(flags, agent)
    } else {
        // No --agent: auto-detect all installed agents
        run_install_auto_detect(flags)
    }
}

/// Install skim for all detected agents when no explicit `--agent` was given.
fn run_install_auto_detect(flags: &InitFlags) -> anyhow::Result<std::process::ExitCode> {
    let agents = detect_installed_agents(&DetectionEnv::from_process());
    if agents.is_empty() {
        eprintln!(
            "No supported agents found. Install one of: Claude Code, Cursor, Gemini CLI, \
             Copilot CLI, Codex CLI, Crush"
        );
        return Ok(std::process::ExitCode::FAILURE);
    }

    // Single-agent fast path: skip the loop overhead when only one agent is detected.
    // This also preserves the original error propagation behaviour (errors are returned
    // rather than caught-and-summarised), which is important for test assertions.
    if agents.len() == 1 {
        let agent_flags = InitFlags {
            agent: Some(agents[0]),
            ..*flags
        };
        return run_install_single(&agent_flags, agents[0]);
    }

    let mut any_failed = false;
    for &agent in &agents {
        let agent_flags = InitFlags {
            agent: Some(agent),
            ..*flags
        };
        match run_install_single(&agent_flags, agent) {
            Ok(code) if code == std::process::ExitCode::SUCCESS => {}
            Ok(code) => {
                any_failed = true;
                eprintln!(
                    "  ✗ {}: failed (exit code: {:?})",
                    agent.display_name(),
                    code
                );
            }
            Err(e) => {
                any_failed = true;
                eprintln!("  ✗ {}: failed — {e}", agent.display_name());
            }
        }
    }

    Ok(if any_failed {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    })
}

/// Install skim for a single, explicit agent.
fn run_install_single(
    flags: &InitFlags,
    agent: AgentKind,
) -> anyhow::Result<std::process::ExitCode> {
    let env = InstructionEnv::from_process();
    let state = detect_state(flags, agent)?;

    verify_agent_installed(&state, agent, flags)?;
    print_install_header(agent.display_name());
    print_detected_state(&state);

    if !state.existing_hooks.is_empty() {
        print_collision_warning(&state.existing_hooks);
    }

    let guidance_current = is_guidance_current(agent, flags, &state.skim_version, &env);
    if state.hook_installed && state.hook_is_current() && guidance_current {
        print_already_up_to_date();
        return Ok(std::process::ExitCode::SUCCESS);
    }

    if let Some(ref warning) = state.dual_scope_warning {
        print_dual_scope_warning(warning);
    }

    let global = !flags.project;
    print_install_summary(&state);

    if flags.dry_run {
        print_dry_run_actions(&state, flags.no_guidance, global, &env)?;
        // Also show dry-run for wrappers if they would be installed.
        if !flags.project {
            maybe_install_wrappers(flags.wrappers, flags.dry_run)?;
        }
        return Ok(std::process::ExitCode::SUCCESS);
    }

    execute_install(&state, flags.no_guidance, global, &env)?;

    // Install shell wrappers (global scope only — wrappers are per-user, not per-project).
    if !flags.project {
        maybe_install_wrappers(flags.wrappers, flags.dry_run)?;
    }

    print_completion_message(agent.display_name());

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

/// Resolve the `AgentKind` for a state's `agent_cli_name`.
///
/// Returns an error when the name is unrecognised — this would indicate a bug
/// in state detection, since `DetectedState` is always built from a known `AgentKind`.
fn agent_from_state(state: &DetectedState) -> anyhow::Result<AgentKind> {
    AgentKind::from_str(state.agent_cli_name).ok_or_else(|| {
        anyhow::anyhow!(
            "unrecognised agent CLI name {:?}; this is a bug in state detection",
            state.agent_cli_name
        )
    })
}

fn execute_install(
    state: &DetectedState,
    no_guidance: bool,
    global: bool,
    env: &InstructionEnv,
) -> anyhow::Result<()> {
    // B7: Create hook script
    create_hook_script(state)?;

    // Legacy migration: if this is Cursor, clean skim entries from settings.json
    // before writing to the correct hooks.json. This removes stale entries that
    // may have been written in a previous skim version when Cursor was (incorrectly)
    // treated as using the Claude Code settings.json / PreToolUse format.
    if let Some(AgentKind::Cursor) = AgentKind::from_str(state.agent_cli_name) {
        migrate_cursor_legacy_settings(&state.config_dir)?;
    }

    // B8: Patch settings.json (or hooks.json for Cursor)
    patch_settings(state)?;

    // Inject guidance into agent instruction file
    if !no_guidance {
        inject_guidance(agent_from_state(state)?, global, env)?;
    }

    // Search: install git hooks and start a background index build.
    // Non-fatal: failures here must not abort the agent hook setup above.
    // find_git_root_from_cwd already verifies .git exists, so the inner
    // .git check is redundant — collapse both conditions.
    if let Some(project_root) = find_git_root_from_cwd() {
        if let Err(e) = crate::cmd::search::hooks::install_search_hooks(project_root.as_path()) {
            eprintln!("  Note: could not install search hooks: {e}");
        } else {
            println!("  {} Search hooks installed", check_mark(true));
        }

        // Spawn a background build — fire-and-forget, non-blocking, non-fatal.
        //
        // The `Child` handle is intentionally dropped without calling `wait()`.
        // On Unix the child process is reparented to init (PID 1) when this
        // process exits and will be reaped there.  The `skim init` command exits
        // immediately after this block, so the window for a zombie entry in the
        // process table is negligible.  The build lock in `build_index` prevents
        // concurrent writes when multiple callers overlap.
        if let Some(exe) = std::env::current_exe().ok()
            && let Ok(child) = std::process::Command::new(&exe)
                .args(["search", "--build"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        {
            eprintln!("  Search index build started (PID {})", child.id());
            drop(child); // Detach: do not wait for the background process.
        }
    }

    Ok(())
}

/// Install or prompt for shell wrapper installation in `~/.skim/bin/`.
///
/// - `Some(true)`: install unconditionally.
/// - `Some(false)`: skip.
/// - `None`: prompt interactively on TTY; default to false on non-TTY.
///
/// Wrapper installation is global-only; callers should not call this for
/// `--project` scope installs.
fn maybe_install_wrappers(wrappers: Option<bool>, dry_run: bool) -> anyhow::Result<()> {
    use super::helpers::confirm_proceed;
    use std::io::IsTerminal;

    let should_install = match wrappers {
        Some(v) => v,
        None => {
            if !std::io::stdin().is_terminal() {
                // Non-interactive: default to false, do not prompt.
                return Ok(());
            }
            println!();
            println!("  Shell wrappers in ~/.skim/bin/ let sub-agents bypass hooks.");
            println!("  Install PATH wrappers? (requires adding ~/.skim/bin to PATH)");
            confirm_proceed()?
        }
    };

    if !should_install {
        return Ok(());
    }

    let skim_binary = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("cannot determine skim binary path: {e}"))?;

    let result = super::wrappers::install_wrappers(&skim_binary, dry_run)?;

    if dry_run {
        println!(
            "  [dry-run] Wrappers: would create {}, update {}, skip {} (correct), \
             skip {} (non-symlink)",
            result.created, result.updated, result.skipped_correct, result.skipped_non_symlink
        );
    } else {
        println!(
            "  {} Wrappers: created {}, updated {}, skipped {} (already correct)",
            super::helpers::check_mark(true),
            result.created,
            result.updated,
            result.skipped_correct,
        );
        if result.skipped_non_symlink > 0 {
            println!(
                "  Warning: {} path(s) skipped — existing non-symlink files were not overwritten",
                result.skipped_non_symlink
            );
        }
        println!();
        println!("  To enable wrappers, add to ~/.zshrc or ~/.bashrc:");
        println!("    export PATH=\"$HOME/.skim/bin:$PATH\"");
        println!();
        println!("  Set SKIM_SESSION_ID in your shell profile for analytics attribution:");
        println!("    export SKIM_SESSION_ID=\"<your-session-id>\"");
    }

    Ok(())
}

/// Walk up from `cwd` looking for a directory that contains `.git`.
///
/// Returns `None` when no `.git` is found within 256 ancestors.
///
/// # Note
///
/// `crate::cmd::search::walk::discover_project_root` performs a similar walk
/// but returns `anyhow::Result<PathBuf>` (falling back to `cwd` when no `.git`
/// is found) and canonicalises the start path first.  This function has
/// different semantics: callers here need `None` to mean "not a git repo" so
/// they can skip hook installation entirely.  The two functions are kept separate
/// to preserve the distinct caller contracts; they share the same 256-ancestor
/// bound to prevent unbounded traversal.
fn find_git_root_from_cwd() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let mut current = cwd.as_path();
    for _ in 0..256 {
        if current.join(".git").exists() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
    None
}

// ============================================================================
// Hook script generation (B7)
// ============================================================================

/// Return `true` when the script at `script_path` already contains the
/// expected version marker *and* a bare `exec skim …` invocation, meaning the
/// file is already up to date and can be skipped.
fn is_hook_script_current(script_path: &std::path::Path, version: &str) -> bool {
    let Ok(contents) = std::fs::read_to_string(script_path) else {
        return false;
    };
    let version_line = format!("# skim-hook v{version}");
    let has_bare_cmd = contents
        .lines()
        .any(|l| l.trim_start().starts_with("exec skim "));
    contents.contains(&version_line) && has_bare_cmd
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
        if is_hook_script_current(&script_path, &state.skim_version) {
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
    } else {
        println!("  {} Created: {}", check_mark(true), script_path.display());
    }

    // Delegate to the shared generator in hooks/mod.rs so install.rs and per-agent
    // HookProtocol impls always produce identical scripts. generate_hook_script
    // validates that version and agent_cli_name are shell-safe and panics if not —
    // both values are &'static str from AgentKind::cli_name() and
    // compile-time CARGO_PKG_VERSION, so this is safe.
    let script_content = generate_hook_script(&state.skim_version, state.agent_cli_name);

    // Atomic write: write to tmp, then rename to final path.
    // A crash mid-write produces a tmp file instead of a truncated script.
    // Cleanup guard: remove tmp on any failure (S1).
    let tmp_path = hooks_dir.join(format!("{HOOK_SCRIPT_NAME}.tmp"));
    if let Err(e) = std::fs::write(&tmp_path, script_content) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }

    // Set executable permissions on the tmp file before renaming
    #[cfg(unix)]
    {
        let perms = std::fs::Permissions::from_mode(0o755);
        if let Err(e) = std::fs::set_permissions(&tmp_path, perms) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e.into());
        }
    }

    if let Err(e) = std::fs::rename(&tmp_path, &script_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }

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
// Legacy Cursor migration (AC-9)
// ============================================================================

/// Remove skim hook entries from a JSON settings value, using the Claude Code
/// format (nested `hooks` array under `hooks.<event_key>`).
///
/// This is the mirror of `remove_skim_from_settings` in `uninstall.rs`, but
/// operating on a mutable `serde_json::Value` and a specific event key.
fn remove_skim_entries_from_event(settings: &mut serde_json::Value, event_key: &str) -> bool {
    let Some(obj) = settings.as_object_mut() else {
        return false;
    };

    if let Some(hooks_obj) = obj.get_mut("hooks").and_then(|h| h.as_object_mut())
        && let Some(arr) = hooks_obj.get_mut(event_key).and_then(|p| p.as_array_mut())
    {
        let before = arr.len();
        arr.retain(|entry| !has_skim_hook_entry(entry));
        let removed = arr.len() < before;
        if arr.is_empty() {
            hooks_obj.remove(event_key);
        }
        if hooks_obj.is_empty() {
            obj.remove("hooks");
        }
        return removed;
    }
    false
}

/// Clean up skim hook entries from Cursor's legacy `settings.json`.
///
/// Earlier skim versions incorrectly wrote Cursor hook config to `settings.json`
/// using the Claude Code / PreToolUse format. This migration removes those stale
/// entries before writing to the correct `hooks.json` location, so both files
/// are kept clean.
///
/// Non-fatal: if `settings.json` doesn't exist or can't be parsed, this is
/// silently skipped (the correct `hooks.json` is still written).
fn migrate_cursor_legacy_settings(config_dir: &std::path::Path) -> anyhow::Result<()> {
    let legacy_path = config_dir.join("settings.json");
    if !legacy_path.exists() {
        return Ok(());
    }

    let Some(mut settings) = read_settings_json(&legacy_path) else {
        return Ok(()); // Unreadable or invalid — skip silently
    };

    // Remove skim entries from all legacy event keys (both casing variants)
    let removed_pre = remove_skim_entries_from_event(&mut settings, "PreToolUse");
    let removed_lower = remove_skim_entries_from_event(&mut settings, "preToolUse");

    if removed_pre || removed_lower {
        // Atomic write to legacy path
        let real_path = resolve_real_settings_path(&legacy_path)?;
        atomic_write_settings(&settings, &real_path)?;
        println!(
            "  {} Cleaned legacy settings.json entry (migrated to hooks.json)",
            check_mark(true)
        );
    }

    Ok(())
}

// ============================================================================
// Settings.json patching (B8)
// ============================================================================

/// Back up the settings file before modification.
///
/// Re-checks that `real_path` is not a symlink immediately before copying to
/// close the TOCTOU window between `resolve_real_settings_path()` and the
/// actual I/O. Without this guard, an attacker could replace the file with a
/// symlink after resolution, causing `fs::copy` to overwrite an arbitrary
/// target.
///
/// **Residual TOCTOU window**: a narrow window still exists between the
/// `is_symlink()` check below and `fs::copy`. Exploitation requires an attacker
/// with local write access to the config directory and kernel-level scheduling
/// control. This residual risk is accepted: the user's config directory is
/// expected to be owner-writable only, and local write access already implies
/// broader compromise. The guard is defence-in-depth, not a full guarantee.
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

fn patch_settings(state: &DetectedState) -> anyhow::Result<()> {
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

    // Upsert hook entry via the agent-specific protocol (correct event key and format)
    let agent = agent_from_state(state)?;
    let protocol = protocol_for_agent(agent);
    let hook_script_path = state.config_dir.join("hooks").join(HOOK_SCRIPT_NAME);
    protocol.upsert_hook(&mut settings, &hook_script_path.display().to_string())?;

    atomic_write_settings(&settings, &real_path)?;

    println!(
        "  {} Patched: {} ({} hook added)",
        check_mark(true),
        state.settings_path.display(),
        protocol.hook_event_key(),
    );

    Ok(())
}

// ============================================================================
// Guidance injection (see guidance.rs)
// ============================================================================

// Re-export guidance symbols used by production code in this module.
pub(super) use super::guidance::{GUIDANCE_START, inject_guidance, remove_guidance};

// Re-export additional guidance symbols for the unit tests below (via `use super::*`).
#[cfg(test)]
pub(super) use super::guidance::{
    MAX_INSTRUCTION_FILE_SIZE, atomic_write_stripped, find_skim_section, guidance_append,
    guidance_update, read_existing_safely, strip_skim_section, update_existing_guidance,
};

// ============================================================================
// Dry-run output (B11)
// ============================================================================

pub(super) fn print_dry_run_actions(
    state: &DetectedState,
    no_guidance: bool,
    global: bool,
    env: &InstructionEnv,
) -> anyhow::Result<()> {
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
    if !no_guidance {
        let agent = agent_from_state(state)?;
        if let Some(path) = agent.instruction_file(global, env) {
            println!("  [dry-run] Would inject guidance into {}", path.display());
        }
    }
    Ok(())
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::hooks::protocol_for_agent;
    use crate::cmd::init::helpers::guidance_content;
    use crate::cmd::session::AgentKind;

    #[test]
    fn test_upsert_hook_entry_idempotent() {
        let protocol = protocol_for_agent(AgentKind::ClaudeCode);
        let mut settings = serde_json::json!({});
        protocol
            .upsert_hook(&mut settings, "/path/to/skim-rewrite.sh")
            .unwrap();
        protocol
            .upsert_hook(&mut settings, "/path/to/skim-rewrite.sh")
            .unwrap();

        let entries = settings["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(
            entries.len(),
            1,
            "running upsert twice should produce exactly one entry, not a duplicate"
        );
    }

    // ---- find_skim_section unit tests ----

    #[test]
    fn test_find_skim_section_normal_case() {
        let content =
            "Before\n<!-- skim-start v1.0.0 -->\nsome guidance\n<!-- skim-end -->\nAfter\n";
        let result = find_skim_section(content);
        assert!(result.is_some(), "Should find section with both markers");
        let (start, end) = result.unwrap();
        assert_eq!(
            &content[start..],
            "<!-- skim-start v1.0.0 -->\nsome guidance\n<!-- skim-end -->\nAfter\n"
        );
        assert_eq!(
            &content[..end],
            "Before\n<!-- skim-start v1.0.0 -->\nsome guidance\n<!-- skim-end -->"
        );
    }

    #[test]
    fn test_find_skim_section_markers_in_wrong_order() {
        // End marker appears before start marker — corrupted file
        let content = "<!-- skim-end -->\nsome content\n<!-- skim-start v1.0.0 -->\n";
        assert!(
            find_skim_section(content).is_none(),
            "Should return None when end marker precedes start marker"
        );
    }

    #[test]
    fn test_find_skim_section_only_start_marker() {
        let content = "<!-- skim-start v1.0.0 -->\nsome guidance\nno end marker\n";
        assert!(
            find_skim_section(content).is_none(),
            "Should return None when only start marker is present"
        );
    }

    #[test]
    fn test_find_skim_section_only_end_marker() {
        let content = "some content\n<!-- skim-end -->\nmore content\n";
        assert!(
            find_skim_section(content).is_none(),
            "Should return None when only end marker is present"
        );
    }

    #[test]
    fn test_find_skim_section_empty_input() {
        assert!(
            find_skim_section("").is_none(),
            "Should return None for empty input"
        );
    }

    #[test]
    fn test_find_skim_section_adjacent_markers() {
        // Start and end markers with no content between them
        let content = "prefix\n<!-- skim-start v2.0.0 --><!-- skim-end -->\nsuffix\n";
        let result = find_skim_section(content);
        assert!(
            result.is_some(),
            "Should find section when markers are adjacent"
        );
        let (start, end) = result.unwrap();
        // start should point to the start marker; end should cover the end marker
        assert!(content[start..].starts_with("<!-- skim-start"));
        assert!(content[..end].ends_with("<!-- skim-end -->"));
    }

    // ---- Guidance injection ----

    #[test]
    fn test_inject_guidance_appends_to_existing() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let existing = "# Existing Content\n\nSome rules here.\n";
        std::fs::write(&path, existing).unwrap();

        let version = "2.1.0";
        let guidance = guidance_content(version);
        guidance_append(&path, existing, &guidance).unwrap();

        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.starts_with("# Existing Content"));
        assert!(result.contains("<!-- skim-start v2.1.0 -->"));
    }

    #[test]
    fn test_inject_guidance_updates_stale_version() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");

        let old_guidance = guidance_content("1.0.0");
        let existing = format!("# Header\n\n{}\n\n# Footer\n", old_guidance);
        std::fs::write(&path, &existing).unwrap();

        let new_guidance = guidance_content("2.1.0");
        let (start, end) = find_skim_section(&existing).expect("markers should be present");
        guidance_update(&path, &existing, start, end, &new_guidance).unwrap();

        let result = std::fs::read_to_string(&path).unwrap();
        assert!(result.contains("v2.1.0"));
        assert!(!result.contains("v1.0.0"));
        assert!(result.contains("# Header"));
        assert!(result.contains("# Footer"));
    }

    #[test]
    fn test_remove_guidance_strips_section() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");

        let guidance = guidance_content("2.1.0");
        let existing = format!("# Header\n\n{}\n\n# Footer\n", guidance);
        std::fs::write(&path, &existing).unwrap();

        let stripped = strip_skim_section(&existing).expect("should find and strip skim section");
        atomic_write_stripped(&path, &stripped).unwrap();

        let result = std::fs::read_to_string(&path).unwrap();
        assert!(!result.contains("skim-start"));
        assert!(result.contains("# Header"));
        assert!(result.contains("# Footer"));
    }

    #[test]
    fn test_remove_guidance_deletes_empty_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");

        let guidance = guidance_content("2.1.0");
        std::fs::write(&path, format!("{}\n", guidance)).unwrap();
        assert!(path.exists());

        let content = std::fs::read_to_string(&path).unwrap();
        let stripped = strip_skim_section(&content).expect("should find skim section");
        if stripped.trim().is_empty() {
            std::fs::remove_file(&path).unwrap();
        } else {
            std::fs::write(&path, &stripped).unwrap();
        }

        assert!(!path.exists(), "Empty file should be deleted");
    }

    // ---- read_existing_safely: oversized-file guard ----

    #[test]
    fn test_read_existing_safely_oversized_file_returns_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("large.md");

        // Write a file that is exactly one byte over the limit.
        let large_content = vec![b'x'; MAX_INSTRUCTION_FILE_SIZE as usize + 1];
        std::fs::write(&path, &large_content).unwrap();

        let result = read_existing_safely(&path).unwrap();
        assert!(
            result.is_none(),
            "read_existing_safely must return None for files exceeding MAX_INSTRUCTION_FILE_SIZE"
        );
    }

    #[test]
    fn test_read_existing_safely_file_at_limit_is_accepted() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("ok.md");

        // A file exactly at the limit should pass.
        let content = vec![b'y'; MAX_INSTRUCTION_FILE_SIZE as usize];
        std::fs::write(&path, &content).unwrap();

        let result = read_existing_safely(&path).unwrap();
        assert!(
            result.is_some(),
            "read_existing_safely must accept files at exactly MAX_INSTRUCTION_FILE_SIZE bytes"
        );
    }

    // ---- update_existing_guidance: all four code paths ----

    #[test]
    fn test_update_existing_guidance_corrupted_markers_skips() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        // End marker appears before start marker — corrupted.
        let corrupted = "<!-- skim-end -->\nsome content\n<!-- skim-start v1.0.0 -->\n";
        std::fs::write(&path, corrupted).unwrap();

        let result = update_existing_guidance(&path, corrupted, "2.0.0", "new content").unwrap();

        assert!(
            result,
            "corrupted markers path must return true (done, skip write)"
        );
        // File should not have been modified.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, corrupted, "file must be unchanged when markers are corrupted");
    }

    #[test]
    fn test_update_existing_guidance_same_version_skips() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let existing = "<!-- skim-start v2.0.0 -->\nguidance\n<!-- skim-end -->\n";
        std::fs::write(&path, existing).unwrap();

        let result = update_existing_guidance(&path, existing, "2.0.0", "new content").unwrap();

        assert!(result, "same-version path must return true (done, skip write)");
        // File should not have been modified.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(on_disk, existing, "file must be unchanged when version matches");
    }

    #[test]
    fn test_update_existing_guidance_different_version_updates_in_place() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let old_guidance = guidance_content("1.0.0");
        let existing = format!("# Header\n\n{old_guidance}\n\n# Footer\n");
        std::fs::write(&path, &existing).unwrap();

        let new_guidance = guidance_content("3.0.0");
        let result =
            update_existing_guidance(&path, &existing, "3.0.0", &new_guidance).unwrap();

        assert!(result, "different-version path must return true (update done)");
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("v3.0.0"), "updated file must contain new version");
        assert!(!on_disk.contains("v1.0.0"), "updated file must not contain old version");
        assert!(on_disk.contains("# Header"), "surrounding content must be preserved");
        assert!(on_disk.contains("# Footer"), "surrounding content must be preserved");
    }

    #[test]
    fn test_update_existing_guidance_no_section_appends() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let existing = "# My Rules\n\nSome existing content.\n";
        std::fs::write(&path, existing).unwrap();

        let new_guidance = guidance_content("2.5.0");
        let result =
            update_existing_guidance(&path, existing, "2.5.0", &new_guidance).unwrap();

        assert!(
            !result,
            "no-section path must return false (caller should print footer)"
        );
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(on_disk.starts_with("# My Rules"), "existing content must be kept");
        assert!(on_disk.contains("<!-- skim-start v2.5.0 -->"), "new guidance must be appended");
    }
}
