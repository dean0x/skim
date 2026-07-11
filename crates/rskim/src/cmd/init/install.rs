//! Install flow for `skim init`.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::flags::{
    DetectionEnv, InitFlags, PermissionsTier, detect_installed_agents, resolve_single_agent,
};
use super::helpers::{
    HOOK_SCRIPT_NAME, SETTINGS_BACKUP, atomic_write_settings, check_mark, confirm_grant,
    confirm_proceed, load_or_create_settings, resolve_real_settings_path,
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
            // Route through read_existing_safely to apply the 1 MiB size guard,
            // matching the parallel path in inject_guidance.
            read_existing_safely(&p)
                .ok()
                .flatten()
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
        let hook_script_path = state.hook_config_dir.join("hooks").join(HOOK_SCRIPT_NAME);
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

// ============================================================================
// Permissions install helpers
// ============================================================================

/// Returns `true` when the "already up to date" fast path must be bypassed
/// because permissions work may be needed.
///
/// Rule:
/// - `Some(false)` → never block (explicit opt-out).
/// - `Some(true)` → always block when a writer exists (explicit request).
/// - `None` → block only when the sidecar exists but entries are stale (auto-update).
fn permissions_blocks_fast_path(
    flags: &InitFlags,
    agent: AgentKind,
    perm_dir: &std::path::Path,
) -> bool {
    match flags.permissions {
        Some(false) => false,
        Some(true) => crate::cmd::permissions::permissions_protocol_for_agent(agent).is_some(),
        None => {
            let Some(protocol) = crate::cmd::permissions::permissions_protocol_for_agent(agent)
            else {
                return false;
            };
            // Stale = sidecar exists AND entries are not all present in agent config.
            let sidecar_path = perm_dir.join("skim-permissions.json");
            if !sidecar_path.exists() {
                return false; // Never seeded → don't auto-seed on upgrade.
            }
            let entries = crate::cmd::permissions::seeded_entries(protocol.as_ref());
            !protocol.is_current(perm_dir, &entries)
        }
    }
}

/// Resolve whether consent has been obtained to seed permissions.
///
/// Calls `confirm_grant` at most once (the single consent resolution point).
///
/// Returns `Ok(true)` when the user consented (or when nothing new will be written).
/// Returns `Ok(false)` when consent was not obtained: non-TTY, explicit opt-out,
/// user declined, or no writer exists for this agent.
fn resolve_permissions_consent(
    flags: &InitFlags,
    agent: AgentKind,
    perm_dir: &std::path::Path,
) -> anyhow::Result<bool> {
    // Explicit opt-out.
    if flags.permissions == Some(false) {
        return Ok(false);
    }

    // No writer for this agent (Cursor, Crush).
    let Some(protocol) = crate::cmd::permissions::permissions_protocol_for_agent(agent) else {
        return Ok(false);
    };

    // Determine whether seeding is desired.
    let requested = match flags.permissions {
        Some(true) => true,
        None => {
            // Auto: only when sidecar exists and is stale.
            let sidecar_path = perm_dir.join("skim-permissions.json");
            if !sidecar_path.exists() {
                return Ok(false);
            }
            let entries = crate::cmd::permissions::seeded_entries(protocol.as_ref());
            !protocol.is_current(perm_dir, &entries)
        }
        Some(false) => return Ok(false), // already handled above; exhaustive match
    };
    if !requested {
        return Ok(false);
    }

    // Blanket tier + Codex: hard error (Codex is unsandboxed; blanket is too broad).
    if agent == AgentKind::CodexCli && flags.permissions_tier == PermissionsTier::Blanket {
        anyhow::bail!(
            "Blanket permission tier is not supported for Codex CLI: Codex runs without \
             a sandbox — blanket grants are too broad.\n\
             hint: use --permissions-tier seed instead"
        );
    }

    // Compute entries and target file for the consent prompt.
    let (entries_for_prompt, config_file) = match flags.permissions_tier {
        PermissionsTier::Mirror if agent == AgentKind::ClaudeCode => {
            let proposals = crate::cmd::permissions::claude::propose_mirrors(perm_dir)?;
            if proposals.is_empty() {
                // No eligible source entries to mirror — treat as already current.
                return Ok(true);
            }
            // Display entries with a `[mutating tool]` annotation for user clarity.
            // The annotation is display-only: actual writes use `p.mirror` verbatim.
            let entries: Vec<String> = proposals
                .iter()
                .map(|p| {
                    if p.is_mutating {
                        format!("{} [mutating tool]", p.mirror)
                    } else {
                        p.mirror.clone()
                    }
                })
                .collect();
            (entries, perm_dir.join(protocol.config_filename()))
        }
        _ => {
            if flags.permissions_tier == PermissionsTier::Mirror {
                println!("  mirror tier is not supported for {agent}; falling back to seed tier");
            }
            let entries = crate::cmd::permissions::seeded_entries(protocol.as_ref());
            (entries, perm_dir.join(protocol.config_filename()))
        }
    };

    // Blanket tier: second confirmation before the main consent prompt.
    if flags.permissions_tier == PermissionsTier::Blanket {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return Ok(false);
        }
        println!();
        println!("  WARNING: Blanket permission tier grants skim broad access to all wrapped");
        println!("  tools, including mutating operations. This is a broad grant.");
        println!();
        if !confirm_proceed()? {
            return Ok(false);
        }
    }

    // Codex CLI: additional "unsandboxed" confirmation (Codex lacks a sandbox).
    if agent == AgentKind::CodexCli {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            return Ok(false);
        }
        println!();
        println!("  WARNING: Codex CLI does not sandbox tool calls. Seeding permissions for");
        println!("  Codex may allow unintended tool access if the model is compromised.");
        println!();
        if !confirm_proceed()? {
            return Ok(false);
        }
    }

    // Non-negotiable TTY gate — confirm_grant returns false on non-TTY.
    let granted = confirm_grant(protocol.agent_label(), &config_file, &entries_for_prompt);
    Ok(granted)
}

/// Seed permissions into the agent config after consent has been obtained.
///
/// # Ordering invariant
///
/// Must be called AFTER `patch_settings` for all agents, and AFTER
/// `migrate_copilot_legacy` for Copilot CLI, so that new hook artifacts are
/// in place before permissions are written.
fn write_permissions(state: &DetectedState, tier: PermissionsTier) -> anyhow::Result<()> {
    let agent = agent_from_state(state)?;
    let Some(protocol) = crate::cmd::permissions::permissions_protocol_for_agent(agent) else {
        return Ok(());
    };

    // Copilot CLI: permissions-config.json lives in hook_config_dir (~/.copilot),
    // not in config_dir (~/.github). For all other agents, use config_dir.
    let perm_dir: &std::path::Path = if agent == AgentKind::CopilotCli {
        &state.hook_config_dir
    } else {
        &state.config_dir
    };

    let outcome = match tier {
        PermissionsTier::Mirror if agent == AgentKind::ClaudeCode => {
            let proposals = crate::cmd::permissions::claude::propose_mirrors(perm_dir)?;
            crate::cmd::permissions::claude::seed_mirrors(perm_dir, &proposals)?
        }
        _ => {
            let entries = crate::cmd::permissions::seeded_entries(protocol.as_ref());
            protocol.seed(perm_dir, tier, &entries)?
        }
    };

    match outcome {
        crate::cmd::permissions::SeedOutcome::Added { entries_added } => {
            println!(
                "  {} Permissions: added {} entr{} to {}",
                check_mark(true),
                entries_added.len(),
                if entries_added.len() == 1 { "y" } else { "ies" },
                protocol.config_filename()
            );
        }
        crate::cmd::permissions::SeedOutcome::AlreadyCurrent => {
            println!("  {} Permissions: already current", check_mark(true));
        }
    }

    Ok(())
}

pub(super) fn run_install(flags: &InitFlags) -> anyhow::Result<std::process::ExitCode> {
    let det_env = DetectionEnv::from_process();
    if let Some(agent) = resolve_single_agent(flags) {
        // Explicit --agent: single-agent mode
        run_install_single(flags, agent, &det_env)
    } else {
        // No --agent: auto-detect all installed agents
        run_install_auto_detect(flags, &det_env)
    }
}

/// Build the per-agent `InitFlags` for auto-detect mode.
///
/// Codex CLI is excluded from the permissions fan-out: it runs without a
/// sandbox, so blanket or seed permissions require explicit `--agent codex`
/// consent, not implicit fan-out. Any other agent inherits the caller's flags.
fn agent_flags_for_auto_detect(agent: AgentKind, flags: &InitFlags) -> InitFlags {
    InitFlags {
        agent: Some(agent),
        // Suppress permissions for Codex in auto-detect mode.
        // Explicit `skim init --agent codex --permissions` bypasses this path.
        permissions: if agent == AgentKind::CodexCli
            && matches!(flags.permissions, Some(true) | None)
        {
            Some(false)
        } else {
            flags.permissions
        },
        ..*flags
    }
}

/// Install skim for all detected agents when no explicit `--agent` was given.
fn run_install_auto_detect(
    flags: &InitFlags,
    det_env: &DetectionEnv,
) -> anyhow::Result<std::process::ExitCode> {
    let agents = detect_installed_agents(det_env);
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
        let agent_flags = agent_flags_for_auto_detect(agents[0], flags);
        return run_install_single(&agent_flags, agents[0], det_env);
    }

    let mut any_failed = false;
    for &agent in &agents {
        let agent_flags = agent_flags_for_auto_detect(agent, flags);
        match run_install_single(&agent_flags, agent, det_env) {
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
    det_env: &DetectionEnv,
) -> anyhow::Result<std::process::ExitCode> {
    let env = InstructionEnv::from_process();
    let state = detect_state(flags, agent, det_env)?;

    // The directory where permissions sidecar and config live.
    // Copilot CLI: hook_config_dir (~/.copilot). All others: config_dir.
    let perm_dir: &std::path::Path = if agent == AgentKind::CopilotCli {
        &state.hook_config_dir
    } else {
        &state.config_dir
    };

    verify_agent_installed(&state, agent, flags)?;
    print_install_header(agent.display_name());
    print_detected_state(&state);

    if !state.existing_hooks.is_empty() {
        print_collision_warning(&state.existing_hooks);
    }

    let guidance_current = is_guidance_current(agent, flags, &state.skim_version, &env);
    let permissions_blocked = permissions_blocks_fast_path(flags, agent, perm_dir);

    if state.hook_installed && state.hook_is_current() && guidance_current && !permissions_blocked {
        print_already_up_to_date();
        return Ok(std::process::ExitCode::SUCCESS);
    }

    if let Some(ref warning) = state.dual_scope_warning {
        print_dual_scope_warning(warning);
    }

    let global = !flags.project;
    print_install_summary(&state);

    if flags.dry_run {
        // Dry-run writes nothing, so consent is not required to DISPLAY what
        // would happen.  We enumerate the permission entries unconditionally
        // (based on flags and writer availability) and annotate them with
        // "(consent required at install)" so the user understands a real run
        // would still prompt for approval.  We deliberately do NOT call
        // resolve_permissions_consent here — that function may prompt on a
        // real TTY and must never fire on a pure-display path.
        print_dry_run_actions(
            &state,
            flags.no_guidance,
            global,
            &env,
            flags.permissions,
            flags.permissions_tier,
            perm_dir,
        )?;
        // Also show dry-run for wrappers if they would be installed.
        if !flags.project {
            maybe_install_wrappers(flags.wrappers, flags.dry_run)?;
        }
        return Ok(std::process::ExitCode::SUCCESS);
    }

    // Resolve consent ONCE before any install action.
    // Returns Ok(false) on non-TTY (the test harness never has a TTY).
    let grant_permissions = resolve_permissions_consent(flags, agent, perm_dir)?;

    // F2/F3: Print a loud notice when --permissions was explicitly requested
    // but refused because stdin is not a terminal.  A TTY user who types 'n'
    // gets no extra notice (they saw the prompt).  Default (None) non-TTY
    // installs stay silent.
    if flags.permissions == Some(true) && !grant_permissions {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal() {
            println!(
                "  Note: --permissions was requested but not granted: non-interactive \
                 session (a human must approve at a TTY; --yes cannot grant). \
                 Re-run skim init --permissions interactively."
            );
        }
    }

    execute_install(
        &state,
        flags.no_guidance,
        global,
        &env,
        grant_permissions,
        flags.permissions_tier,
    )?;

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
    grant_permissions: bool,
    tier: PermissionsTier,
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

    // Legacy migration: if this is Copilot CLI, remove stale skim artifacts from
    // ~/.github/ AFTER new artifacts have been written to ~/.copilot/.
    // state.config_dir  = ~/.github (rules dir; legacy hook location)
    // state.hook_config_dir = ~/.copilot (new hook artifact location)
    //
    // Ordering: migrate BEFORE write_permissions for Copilot, so that
    // the new hook_config_dir is fully established before permissions are written.
    if let Some(AgentKind::CopilotCli) = AgentKind::from_str(state.agent_cli_name) {
        migrate_copilot_legacy(&state.config_dir, &state.hook_config_dir)?;
    }

    // Write permissions if consent was obtained.
    // Ordering invariant: after patch_settings for all agents; after
    // migrate_copilot_legacy for Copilot CLI (see comment above).
    if grant_permissions {
        write_permissions(state, tier)?;
    }

    // Inject guidance into agent instruction file
    if !no_guidance {
        inject_guidance(agent_from_state(state)?, global, env)?;
    }

    // Search hooks + background index — non-fatal, delegated to helper.
    install_search_integration();

    Ok(())
}

/// Install search git hooks and spawn a background index build.
///
/// Non-fatal: all failures are printed as notices but do not abort the caller.
/// `find_git_root_from_cwd` already verifies `.git` exists, so no inner `.git`
/// check is needed — the outer `if let Some(...)` is the only guard required.
fn install_search_integration() {
    let Some(project_root) = find_git_root_from_cwd() else {
        return;
    };

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
    if let Ok(exe) = std::env::current_exe()
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

    print_wrapper_install_result(&result, dry_run);

    Ok(())
}

/// Print the post-install summary for wrapper installation.
///
/// Separated from `maybe_install_wrappers` to keep the decision/IO boundary
/// clear and to allow independent unit testing of the output logic.
fn print_wrapper_install_result(result: &super::wrappers::InstallResult, dry_run: bool) {
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
}

/// Walk up from `cwd` looking for a directory that contains `.git`.
///
/// Returns `None` when no `.git` is found within 64 ancestors.
///
/// The bound is 64: realistic repo nesting is rarely deeper than 10–20 levels,
/// and capping at 64 reduces worst-case `stat` calls on slow/network filesystems
/// while still covering all real-world cases.
///
/// # Note
///
/// `crate::cmd::search::walk::discover_project_root` performs a similar walk
/// but returns `anyhow::Result<PathBuf>` (falling back to `cwd` when no `.git`
/// is found) and canonicalises the start path first.  This function has
/// different semantics: callers here need `None` to mean "not a git repo" so
/// they can skip hook installation entirely.  The two functions are kept separate
/// to preserve the distinct caller contracts.
fn find_git_root_from_cwd() -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let mut current = cwd.as_path();
    for _ in 0..64 {
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
/// expected version marker *and* exports `SKIM_HOOK_BINARY` (the pinned binary
/// format introduced in F6), meaning the file is already up to date and can be
/// skipped.
///
/// Old scripts that only have a bare `exec skim …` line (pre-F6) are treated as
/// stale: they lack the pinned-binary exports and must be regenerated.
fn is_hook_script_current(script_path: &std::path::Path, version: &str) -> bool {
    let Ok(contents) = std::fs::read_to_string(script_path) else {
        return false;
    };
    let version_line = format!("# skim-hook v{version}");
    contents.contains(&version_line) && super::script_has_pinned_marker(&contents)
}

fn create_hook_script(state: &DetectedState) -> anyhow::Result<()> {
    let hooks_dir = state.hook_config_dir.join("hooks");
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

    // Canonicalize the current binary path for embedding in the hook script.
    // Falls back gracefully: canonicalize → current_exe → empty string.
    // The path is single-quoted inside generate_hook_script, so spaces are safe.
    let binary_path = std::env::current_exe()
        .and_then(|p| std::fs::canonicalize(&p).or(Ok(p)))
        .unwrap_or_default();
    let binary_path_str = binary_path.to_string_lossy();

    // Delegate to the shared generator in hooks/mod.rs so install.rs and per-agent
    // HookProtocol impls always produce identical scripts. generate_hook_script
    // validates that version and agent_cli_name are shell-safe and panics if not —
    // both values are &'static str from AgentKind::cli_name() and
    // compile-time CARGO_PKG_VERSION, so this is safe.
    let script_content =
        generate_hook_script(&state.skim_version, state.agent_cli_name, &binary_path_str);

    atomic_write_executable(&hooks_dir, &script_path, &script_content)?;

    // Compute and store SHA-256 hash for integrity verification (#57)
    if let Ok(hash) = crate::cmd::integrity::compute_file_hash(&script_path) {
        let _ = crate::cmd::integrity::write_hash_manifest(
            &state.hook_config_dir,
            state.agent_cli_name,
            HOOK_SCRIPT_NAME,
            &hash,
        );
    }

    Ok(())
}

/// Atomically write `content` to `script_path` and set executable permissions.
///
/// Writes to a `.tmp` sibling first, then renames — a crash mid-write produces
/// a tmp file instead of a truncated script.  The tmp file is removed on any
/// failure (cleanup guard).  On Unix, `0o755` permissions are applied to the
/// tmp file *before* the rename so the final path is always executable.
fn atomic_write_executable(
    dir: &std::path::Path,
    script_path: &std::path::Path,
    content: &str,
) -> anyhow::Result<()> {
    let tmp_path = dir.join(format!("{HOOK_SCRIPT_NAME}.tmp"));

    if let Err(e) = std::fs::write(&tmp_path, content) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }

    #[cfg(unix)]
    {
        let perms = std::fs::Permissions::from_mode(0o755);
        if let Err(e) = std::fs::set_permissions(&tmp_path, perms) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e.into());
        }
    }

    if let Err(e) = std::fs::rename(&tmp_path, script_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
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
// Legacy Copilot CLI migration
// ============================================================================

/// Returns `true` when `entry` is a Copilot CLI skim hook entry (uses the `bash` field).
///
/// Equivalent to `CopilotCliHook::is_skim_entry` but defined here to keep the migration
/// helper self-contained and avoid importing the protocol.
fn is_copilot_skim_entry(entry: &serde_json::Value) -> bool {
    entry
        .get("bash")
        .and_then(|v| v.as_str())
        .is_some_and(|s| s.contains("skim-rewrite"))
}

/// Remove all Copilot-format skim entries from `settings` (in-place).
///
/// Handles both `"preToolUse"` and `"PreToolUse"` event keys.
/// Cleans up empty event-key arrays and the empty `"hooks"` object afterwards.
/// Returns `true` if any entries were removed.
fn remove_copilot_skim_from_settings(settings: &mut serde_json::Value) -> bool {
    let mut changed = false;
    for event_key in ["preToolUse", "PreToolUse"] {
        // Two-pass: read-only pass to detect, then mutable pass to remove.
        // This avoids simultaneous mutable + immutable borrow of the same value.
        let has_skim = settings
            .get("hooks")
            .and_then(|h| h.get(event_key))
            .and_then(|v| v.as_array())
            .is_some_and(|arr| arr.iter().any(is_copilot_skim_entry));
        if !has_skim {
            continue;
        }
        changed = true;
        if let Some(arr) = settings
            .get_mut("hooks")
            .and_then(|h| h.get_mut(event_key))
            .and_then(|v| v.as_array_mut())
        {
            arr.retain(|e| !is_copilot_skim_entry(e));
        }
        // Clean up empty event-key array.
        let is_empty = settings
            .get("hooks")
            .and_then(|h| h.get(event_key))
            .and_then(|v| v.as_array())
            .is_some_and(|a| a.is_empty());
        if is_empty && let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) {
            hooks.remove(event_key);
        }
    }
    // Clean up empty hooks object.
    let hooks_empty = settings
        .get("hooks")
        .and_then(|v| v.as_object())
        .is_some_and(|h| h.is_empty());
    if hooks_empty {
        settings.as_object_mut().map(|o| o.remove("hooks"));
    }
    changed
}

/// Returns `true` when the parsed `.bak` value is "skim-shaped":
/// it contains a Copilot-format skim entry, indicating skim wrote this backup.
fn copilot_bak_is_skim_shaped(v: &serde_json::Value) -> bool {
    v.get("hooks")
        .and_then(|h| h.get("preToolUse").or_else(|| h.get("PreToolUse")))
        .and_then(|arr| arr.as_array())
        .is_some_and(|entries| entries.iter().any(is_copilot_skim_entry))
}

/// Returns `true` when `script_path` is safe to delete (skim-owned, unmodified).
///
/// Tries SHA-256 verification via the sidecar first (exact-match required).
/// Falls back to checking for the `# skim-hook v` marker header when no sidecar
/// exists (backward compat for installs that predate the sidecar feature).
/// Returns `false` on any doubt (hash mismatch, read error, marker absent).
fn legacy_script_is_skim_owned(
    config_dir: &std::path::Path,
    script_path: &std::path::Path,
) -> bool {
    let has_sidecar = crate::cmd::integrity::read_hash_manifest(config_dir, "copilot").is_some();
    if has_sidecar {
        matches!(
            crate::cmd::integrity::verify_script_integrity(config_dir, "copilot", script_path),
            Ok(true)
        )
    } else {
        std::fs::read_to_string(script_path)
            .ok()
            .is_some_and(|c| c.contains("# skim-hook v"))
    }
}

/// Migrate legacy Copilot CLI skim artifacts from `legacy_config_dir` (`~/.github`) to
/// `new_hook_config_dir` (`~/.copilot`), removing each stale artifact under an independent
/// conservative guard.
///
/// # Ordering guarantee
///
/// This function MUST be called AFTER all new artifacts have been written at
/// `new_hook_config_dir`. If `hooks/skim-rewrite.sh` or `hooks/skim.json` are absent the
/// function returns immediately without touching anything at the legacy location.
///
/// # Per-artifact removal guards (each independent)
///
/// 1. `settings.json` — skim `bash` entries removed surgically; all other keys preserved.
/// 2. `settings.json.bak` — deleted only when the backup is skim-shaped (contains a skim
///    `preToolUse` entry), indicating skim's own backup step created it.
/// 3. `hooks/skim-rewrite.sh` — deleted only when SHA-verified (sidecar exists + hash
///    matches) or, with no sidecar, when the script contains the `# skim-hook v` marker.
/// 4. `hooks/skim-copilot.sha256` — always deleted (skim-owned metadata, no user content).
/// 5. `hooks/` directory — `rmdir` (silent no-op when non-empty or missing; user files survive).
fn migrate_copilot_legacy(
    legacy_config_dir: &std::path::Path,
    new_hook_config_dir: &std::path::Path,
) -> anyhow::Result<()> {
    // No-op when the legacy and new locations are the same directory.
    // This occurs when COPILOT_CONFIG_DIR is set (both config_dir and
    // hook_config_dir resolve to the override value), meaning the hook
    // artifacts were already written to the correct location — there is
    // nothing stale to clean up.
    if legacy_config_dir == new_hook_config_dir {
        return Ok(());
    }

    // Ordering guard: new artifacts must already exist before we touch the legacy location.
    let new_script = new_hook_config_dir.join("hooks").join(HOOK_SCRIPT_NAME);
    let new_skim_json = new_hook_config_dir.join("hooks").join("skim.json");
    if !new_script.exists() || !new_skim_json.exists() {
        return Ok(());
    }

    // ---- 1. settings.json: surgical skim entry removal ----
    let settings_path = legacy_config_dir.join("settings.json");
    if settings_path.is_file()
        && let Some(mut settings) = read_settings_json(&settings_path)
        && remove_copilot_skim_from_settings(&mut settings)
        && let Ok(real_path) = resolve_real_settings_path(&settings_path)
    {
        // Non-fatal: new artifacts are already in place; stale entry is harmless.
        let _ = atomic_write_settings(&settings, &real_path);
        println!(
            "  {} Cleaned legacy Copilot skim entry from {}",
            check_mark(true),
            settings_path.display()
        );
    }

    // ---- 2. settings.json.bak: delete only if skim-shaped ----
    let bak_path = legacy_config_dir.join(SETTINGS_BACKUP);
    if bak_path.is_file() {
        let is_skim = read_settings_json(&bak_path)
            .as_ref()
            .is_some_and(copilot_bak_is_skim_shaped);
        if is_skim {
            let _ = std::fs::remove_file(&bak_path);
            println!(
                "  {} Removed skim-owned legacy Copilot settings backup",
                check_mark(true)
            );
        }
    }

    // ---- 3. hooks/skim-rewrite.sh: delete when hash-verified or marker-present ----
    let legacy_hooks_dir = legacy_config_dir.join("hooks");
    let legacy_script = legacy_hooks_dir.join(HOOK_SCRIPT_NAME);
    if legacy_script.is_file() && legacy_script_is_skim_owned(legacy_config_dir, &legacy_script) {
        let _ = std::fs::remove_file(&legacy_script);
        println!("  {} Removed legacy Copilot hook script", check_mark(true));
    }

    // ---- 4. skim-copilot.sha256 sidecar: always remove (skim-owned metadata) ----
    let _ = crate::cmd::integrity::remove_hash_manifest(legacy_config_dir, "copilot");

    // ---- 5. hooks/ dir: rmdir — silent no-op when non-empty or missing ----
    let _ = std::fs::remove_dir(&legacy_hooks_dir);

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
    let agent = agent_from_state(state)?;
    let protocol = protocol_for_agent(agent);
    let hook_script_path = state.hook_config_dir.join("hooks").join(HOOK_SCRIPT_NAME);

    // Agents that store hook registration in a dedicated file (e.g. Copilot CLI
    // uses ~/.copilot/hooks/skim.json) bypass the settings.json write entirely.
    if protocol.uses_dedicated_hook_file() {
        protocol.install_hook_registration(
            &state.hook_config_dir,
            &hook_script_path.display().to_string(),
        )?;
        println!("  {} Registered hook in hooks/skim.json", check_mark(true),);
        return Ok(());
    }

    // settings.json path — all other agents.

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
pub(super) use super::guidance::{
    GUIDANCE_START, inject_guidance, read_existing_safely, remove_guidance,
};

// Re-export additional guidance symbols for the unit tests below (via `use super::*`).
// `read_existing_safely` is already exported above for production use; omit here.
#[cfg(test)]
pub(super) use super::guidance::{
    MAX_INSTRUCTION_FILE_SIZE, atomic_write_stripped, find_skim_section, guidance_append,
    guidance_update, strip_skim_section, update_existing_guidance,
};

// ============================================================================
// Dry-run output (B11)
// ============================================================================

pub(super) fn print_dry_run_actions(
    state: &DetectedState,
    no_guidance: bool,
    global: bool,
    env: &InstructionEnv,
    permissions: Option<bool>,
    tier: PermissionsTier,
    perm_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let hook_script_path = state.hook_config_dir.join("hooks").join(HOOK_SCRIPT_NAME);

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

    // Enumerate permissions entries without consulting consent.
    // Dry-run writes nothing, so no consent is required to DISPLAY what would
    // happen.  We show entries whenever permissions are "active" for this run
    // and annotate them with "(consent required at install)" to make clear that
    // a real install would still require an interactive approval.
    if permissions != Some(false) {
        let agent = agent_from_state(state)?;
        if let Some(protocol) = crate::cmd::permissions::permissions_protocol_for_agent(agent) {
            // Determine whether permissions are active for this dry-run.
            let permissions_active = match permissions {
                Some(false) => false, // explicit opt-out (already guarded above)
                Some(true) => true,   // explicitly requested
                None => {
                    // Auto-mode: active only when the sidecar already exists and
                    // is stale (same condition as permissions_blocks_fast_path).
                    let sidecar_path = perm_dir.join("skim-permissions.json");
                    if sidecar_path.exists() {
                        let entries = crate::cmd::permissions::seeded_entries(protocol.as_ref());
                        !protocol.is_current(perm_dir, &entries)
                    } else {
                        false
                    }
                }
            };
            if permissions_active {
                let entries = match tier {
                    PermissionsTier::Mirror if agent == AgentKind::ClaudeCode => {
                        let proposals = crate::cmd::permissions::claude::propose_mirrors(perm_dir)
                            .unwrap_or_default();
                        if proposals.is_empty() {
                            // No mirroring candidates: fall back to seed entries.
                            crate::cmd::permissions::seeded_entries(protocol.as_ref())
                        } else {
                            proposals
                                .into_iter()
                                .map(|p| {
                                    if p.is_mutating {
                                        format!("{} [mutating tool]", p.mirror)
                                    } else {
                                        p.mirror
                                    }
                                })
                                .collect::<Vec<_>>()
                        }
                    }
                    _ => crate::cmd::permissions::seeded_entries(protocol.as_ref()),
                };
                let config_file = perm_dir.join(protocol.config_filename());
                println!(
                    "  [dry-run] Would seed permissions (consent required at install): {}",
                    config_file.display()
                );
                for entry in &entries {
                    println!("    {entry}");
                }
            }
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

    // ---- is_hook_script_current ----

    #[test]
    fn test_is_hook_script_current_pinned_format_returns_true() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("skim-rewrite.sh");
        // New F6 pinned format: version comment + SKIM_HOOK_BINARY export.
        let content = "#!/bin/sh\n# skim-hook v1.2.3\n\
                       export SKIM_HOOK_BINARY='/usr/local/bin/skim'\n\
                       export SKIM_HOOK_COMMIT=abc1234\n\
                       _SKIM_BIN='/usr/local/bin/skim'\n\
                       if [ -x \"$_SKIM_BIN\" ]; then exec \"$_SKIM_BIN\" rewrite --hook --agent claude-code; fi\n\
                       exec skim rewrite --hook --agent claude-code\n";
        std::fs::write(&path, content).unwrap();

        assert!(
            is_hook_script_current(&path, "1.2.3"),
            "matching version + SKIM_HOOK_BINARY export must return true"
        );
    }

    #[test]
    fn test_is_hook_script_current_old_bare_exec_returns_false() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("skim-rewrite.sh");
        // Pre-F6 bare-exec format is stale — no SKIM_HOOK_BINARY export.
        let content = "#!/bin/sh\n# skim-hook v1.2.3\nexec skim rewrite --hook \"$@\"\n";
        std::fs::write(&path, content).unwrap();

        assert!(
            !is_hook_script_current(&path, "1.2.3"),
            "old bare-exec format (no SKIM_HOOK_BINARY) must return false even when version matches"
        );
    }

    #[test]
    fn test_is_hook_script_current_wrong_version_returns_false() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("skim-rewrite.sh");
        // Script has v1.0.0 with new pinned format but we check for v2.0.0
        let content = "#!/bin/sh\n# skim-hook v1.0.0\n\
                       export SKIM_HOOK_BINARY='/usr/local/bin/skim'\n\
                       exec skim rewrite --hook\n";
        std::fs::write(&path, content).unwrap();

        assert!(
            !is_hook_script_current(&path, "2.0.0"),
            "mismatched version must return false"
        );
    }

    #[test]
    fn test_is_hook_script_current_missing_file_returns_false() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.sh");

        assert!(
            !is_hook_script_current(&path, "1.0.0"),
            "unreadable/missing file must return false"
        );
    }

    #[test]
    fn test_is_hook_script_current_missing_pinned_binary_returns_false() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("skim-rewrite.sh");
        // Version matches but no SKIM_HOOK_BINARY export at all
        let content = "#!/bin/sh\n# skim-hook v1.2.3\nskim rewrite --hook \"$@\"\n";
        std::fs::write(&path, content).unwrap();

        assert!(
            !is_hook_script_current(&path, "1.2.3"),
            "missing SKIM_HOOK_BINARY export must return false even when version matches"
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
        assert_eq!(
            on_disk, corrupted,
            "file must be unchanged when markers are corrupted"
        );
    }

    #[test]
    fn test_update_existing_guidance_same_version_skips() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let existing = "<!-- skim-start v2.0.0 -->\nguidance\n<!-- skim-end -->\n";
        std::fs::write(&path, existing).unwrap();

        let result = update_existing_guidance(&path, existing, "2.0.0", "new content").unwrap();

        assert!(
            result,
            "same-version path must return true (done, skip write)"
        );
        // File should not have been modified.
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            on_disk, existing,
            "file must be unchanged when version matches"
        );
    }

    #[test]
    fn test_update_existing_guidance_different_version_updates_in_place() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let old_guidance = guidance_content("1.0.0");
        let existing = format!("# Header\n\n{old_guidance}\n\n# Footer\n");
        std::fs::write(&path, &existing).unwrap();

        let new_guidance = guidance_content("3.0.0");
        let result = update_existing_guidance(&path, &existing, "3.0.0", &new_guidance).unwrap();

        assert!(
            result,
            "different-version path must return true (update done)"
        );
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.contains("v3.0.0"),
            "updated file must contain new version"
        );
        assert!(
            !on_disk.contains("v1.0.0"),
            "updated file must not contain old version"
        );
        assert!(
            on_disk.contains("# Header"),
            "surrounding content must be preserved"
        );
        assert!(
            on_disk.contains("# Footer"),
            "surrounding content must be preserved"
        );
    }

    #[test]
    fn test_update_existing_guidance_no_section_appends() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let existing = "# My Rules\n\nSome existing content.\n";
        std::fs::write(&path, existing).unwrap();

        let new_guidance = guidance_content("2.5.0");
        let result = update_existing_guidance(&path, existing, "2.5.0", &new_guidance).unwrap();

        assert!(
            !result,
            "no-section path must return false (caller should print footer)"
        );
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert!(
            on_disk.starts_with("# My Rules"),
            "existing content must be kept"
        );
        assert!(
            on_disk.contains("<!-- skim-start v2.5.0 -->"),
            "new guidance must be appended"
        );
    }

    // ---- agent_from_state ----

    /// Build a minimal `DetectedState` with the given `agent_cli_name` for
    /// `agent_from_state` tests. All other fields are inert defaults.
    fn state_with_cli_name(agent_cli_name: &'static str) -> DetectedState {
        DetectedState {
            skim_binary: std::path::PathBuf::from("/usr/bin/skim"),
            skim_version: "1.0.0".to_string(),
            config_dir: std::path::PathBuf::from("/tmp"),
            hook_config_dir: std::path::PathBuf::from("/tmp"),
            settings_path: std::path::PathBuf::from("/tmp/settings.json"),
            settings_exists: false,
            hook_installed: false,
            hook_version: None,
            hook_uses_pinned_binary: false,
            dual_scope_warning: None,
            existing_hooks: vec![],
            agent_cli_name,
        }
    }

    #[test]
    fn test_agent_from_state_valid_name_returns_ok() {
        // Every known cli_name must round-trip through agent_from_state without error.
        for agent in AgentKind::all_supported() {
            let state = state_with_cli_name(agent.cli_name());
            assert!(
                agent_from_state(&state).is_ok(),
                "agent_from_state must succeed for known cli_name {:?}",
                agent.cli_name()
            );
        }
    }

    #[test]
    fn test_agent_from_state_invalid_name_returns_err() {
        // An unrecognised cli_name is a bug in state detection; must produce an Err.
        let state = state_with_cli_name("not-a-real-agent");
        let err = agent_from_state(&state).unwrap_err();
        assert!(
            err.to_string().contains("unrecognised agent CLI name"),
            "error message must mention 'unrecognised agent CLI name', got: {err}"
        );
    }

    // ---- maybe_install_wrappers project-scope guard ----

    #[test]
    fn test_project_scope_skips_wrapper_installation() {
        // Invariant: maybe_install_wrappers must NOT be called when flags.project is true.
        // This test exercises the guard by verifying that the skip path in
        // maybe_install_wrappers (wrappers: Some(false)) returns Ok without installing.
        // The call-site guards in run_install_single at lines 228 and 237 enforce this
        // invariant; this test locks down the skip path to catch accidental removal.
        let result = maybe_install_wrappers(Some(false), false);
        assert!(
            result.is_ok(),
            "maybe_install_wrappers(Some(false), _) must return Ok without touching the filesystem"
        );
    }

    // ---- Legacy Copilot CLI migration ----

    /// Populate the new hook artifact location with the minimum files needed to
    /// satisfy `migrate_copilot_legacy`'s ordering guard.
    fn setup_new_copilot_artifacts(new_dir: &tempfile::TempDir) {
        let new_hooks = new_dir.path().join("hooks");
        std::fs::create_dir_all(&new_hooks).unwrap();
        std::fs::write(
            new_hooks.join(HOOK_SCRIPT_NAME),
            "#!/bin/sh\n# skim-hook v9.9.9\nexec skim rewrite --hook --agent copilot\n",
        )
        .unwrap();
        std::fs::write(
            new_hooks.join("skim.json"),
            r#"{"version":1,"hooks":{"preToolUse":[{"type":"command","bash":"/tmp/skim-rewrite.sh"}]}}"#,
        )
        .unwrap();
    }

    #[test]
    fn test_migrate_copilot_legacy_no_op_when_new_artifacts_missing() {
        // Ordering guard: if new artifacts are absent, legacy location must not be touched.
        let legacy_dir = tempfile::TempDir::new().unwrap();
        let new_dir = tempfile::TempDir::new().unwrap();
        // Do NOT call setup_new_copilot_artifacts — ordering guard must fire.

        let legacy_hooks = legacy_dir.path().join("hooks");
        std::fs::create_dir_all(&legacy_hooks).unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "preToolUse": [{"bash": legacy_hooks.join(HOOK_SCRIPT_NAME).display().to_string()}]
            }
        });
        let settings_path = legacy_dir.path().join("settings.json");
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        super::migrate_copilot_legacy(legacy_dir.path(), new_dir.path()).unwrap();

        // settings.json must be unchanged (still has the skim entry)
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert!(
            on_disk["hooks"]["preToolUse"]
                .as_array()
                .is_some_and(|a| !a.is_empty()),
            "settings.json must not be touched when new artifacts are absent"
        );
    }

    #[test]
    fn test_migrate_copilot_legacy_removes_settings_skim_entry() {
        let legacy_dir = tempfile::TempDir::new().unwrap();
        let new_dir = tempfile::TempDir::new().unwrap();
        setup_new_copilot_artifacts(&new_dir);

        // Create legacy settings.json with a Copilot-format skim entry and user content.
        let legacy_hooks = legacy_dir.path().join("hooks");
        std::fs::create_dir_all(&legacy_hooks).unwrap();
        let settings = serde_json::json!({
            "hooks": {
                "preToolUse": [
                    {"bash": legacy_hooks.join(HOOK_SCRIPT_NAME).display().to_string(), "type": "command"}
                ]
            },
            "user_key": "must_be_preserved"
        });
        let settings_path = legacy_dir.path().join("settings.json");
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&settings).unwrap(),
        )
        .unwrap();

        super::migrate_copilot_legacy(legacy_dir.path(), new_dir.path()).unwrap();

        let result: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        // skim entry must be gone
        assert!(
            result.get("hooks").is_none()
                || result["hooks"].get("preToolUse").is_none()
                || result["hooks"]["preToolUse"]
                    .as_array()
                    .is_some_and(|a| a.is_empty()),
            "skim preToolUse entry must be removed from settings.json"
        );
        // user content must survive
        assert_eq!(
            result.get("user_key").and_then(|v| v.as_str()),
            Some("must_be_preserved"),
            "non-skim keys must be preserved in settings.json"
        );
    }

    #[test]
    fn test_migrate_copilot_legacy_removes_sidecar_always() {
        let legacy_dir = tempfile::TempDir::new().unwrap();
        let new_dir = tempfile::TempDir::new().unwrap();
        setup_new_copilot_artifacts(&new_dir);

        let legacy_hooks = legacy_dir.path().join("hooks");
        std::fs::create_dir_all(&legacy_hooks).unwrap();
        let sidecar = legacy_hooks.join("skim-copilot.sha256");
        std::fs::write(&sidecar, "sha256:abc123  skim-rewrite.sh\n").unwrap();

        super::migrate_copilot_legacy(legacy_dir.path(), new_dir.path()).unwrap();
        assert!(!sidecar.exists(), "SHA sidecar must always be removed");
    }

    #[test]
    fn test_migrate_copilot_legacy_removes_script_when_hash_verified() {
        let legacy_dir = tempfile::TempDir::new().unwrap();
        let new_dir = tempfile::TempDir::new().unwrap();
        setup_new_copilot_artifacts(&new_dir);

        let legacy_hooks = legacy_dir.path().join("hooks");
        std::fs::create_dir_all(&legacy_hooks).unwrap();
        let legacy_script = legacy_hooks.join(HOOK_SCRIPT_NAME);
        std::fs::write(
            &legacy_script,
            "#!/bin/sh\n# skim-hook v1.0.0\nexec skim rewrite --hook --agent copilot\n",
        )
        .unwrap();
        let hash = crate::cmd::integrity::compute_file_hash(&legacy_script).unwrap();
        crate::cmd::integrity::write_hash_manifest(
            legacy_dir.path(),
            "copilot",
            HOOK_SCRIPT_NAME,
            &hash,
        )
        .unwrap();

        super::migrate_copilot_legacy(legacy_dir.path(), new_dir.path()).unwrap();
        assert!(
            !legacy_script.exists(),
            "SHA-verified legacy script must be removed"
        );
    }

    #[test]
    fn test_migrate_copilot_legacy_preserves_tampered_script() {
        let legacy_dir = tempfile::TempDir::new().unwrap();
        let new_dir = tempfile::TempDir::new().unwrap();
        setup_new_copilot_artifacts(&new_dir);

        let legacy_hooks = legacy_dir.path().join("hooks");
        std::fs::create_dir_all(&legacy_hooks).unwrap();
        let legacy_script = legacy_hooks.join(HOOK_SCRIPT_NAME);
        let original = "#!/bin/sh\n# skim-hook v1.0.0\nexec skim rewrite --hook\n";
        std::fs::write(&legacy_script, original).unwrap();
        // Write hash for the original content, then tamper.
        let hash = crate::cmd::integrity::compute_file_hash(&legacy_script).unwrap();
        crate::cmd::integrity::write_hash_manifest(
            legacy_dir.path(),
            "copilot",
            HOOK_SCRIPT_NAME,
            &hash,
        )
        .unwrap();
        std::fs::write(&legacy_script, "#!/bin/sh\necho INJECTED\n").unwrap();

        super::migrate_copilot_legacy(legacy_dir.path(), new_dir.path()).unwrap();
        assert!(
            legacy_script.exists(),
            "tampered legacy script must be preserved (hash mismatch = user modification)"
        );
    }

    #[test]
    fn test_migrate_copilot_legacy_removes_script_by_marker_fallback() {
        // No sidecar: fall back to the `# skim-hook v` marker header.
        let legacy_dir = tempfile::TempDir::new().unwrap();
        let new_dir = tempfile::TempDir::new().unwrap();
        setup_new_copilot_artifacts(&new_dir);

        let legacy_hooks = legacy_dir.path().join("hooks");
        std::fs::create_dir_all(&legacy_hooks).unwrap();
        let legacy_script = legacy_hooks.join(HOOK_SCRIPT_NAME);
        std::fs::write(
            &legacy_script,
            "#!/bin/sh\n# skim-hook v1.0.0\nexec skim rewrite --hook --agent copilot\n",
        )
        .unwrap();
        // No sidecar written.

        super::migrate_copilot_legacy(legacy_dir.path(), new_dir.path()).unwrap();
        assert!(
            !legacy_script.exists(),
            "marker-identified script (no sidecar) must be removed via fallback"
        );
    }

    #[test]
    fn test_migrate_copilot_legacy_deletes_bak_if_skim_shaped() {
        let legacy_dir = tempfile::TempDir::new().unwrap();
        let new_dir = tempfile::TempDir::new().unwrap();
        setup_new_copilot_artifacts(&new_dir);

        let bak = serde_json::json!({
            "hooks": {
                "preToolUse": [
                    {"bash": "/home/.github/hooks/skim-rewrite.sh", "type": "command"}
                ]
            }
        });
        let bak_path = legacy_dir.path().join(SETTINGS_BACKUP);
        std::fs::write(&bak_path, serde_json::to_string_pretty(&bak).unwrap()).unwrap();

        super::migrate_copilot_legacy(legacy_dir.path(), new_dir.path()).unwrap();
        assert!(
            !bak_path.exists(),
            "skim-shaped .bak (contains skim preToolUse entry) must be deleted"
        );
    }

    #[test]
    fn test_migrate_copilot_legacy_preserves_bak_if_not_skim_shaped() {
        let legacy_dir = tempfile::TempDir::new().unwrap();
        let new_dir = tempfile::TempDir::new().unwrap();
        setup_new_copilot_artifacts(&new_dir);

        // .bak contains only user settings — no skim entry.
        let bak = serde_json::json!({"copilot": {"editor": "neovim"}});
        let bak_path = legacy_dir.path().join(SETTINGS_BACKUP);
        std::fs::write(&bak_path, serde_json::to_string_pretty(&bak).unwrap()).unwrap();

        super::migrate_copilot_legacy(legacy_dir.path(), new_dir.path()).unwrap();
        assert!(
            bak_path.exists(),
            "non-skim-shaped .bak (user settings only) must be preserved"
        );
    }

    #[test]
    fn test_migrate_copilot_legacy_idempotent() {
        // Two consecutive calls must both succeed without errors.
        let legacy_dir = tempfile::TempDir::new().unwrap();
        let new_dir = tempfile::TempDir::new().unwrap();
        setup_new_copilot_artifacts(&new_dir);

        // No legacy artifacts — first and second call are both no-ops.
        super::migrate_copilot_legacy(legacy_dir.path(), new_dir.path())
            .expect("first call must not error");
        super::migrate_copilot_legacy(legacy_dir.path(), new_dir.path())
            .expect("second call (idempotent) must not error");
    }

    // ---- agent_flags_for_auto_detect: Codex permissions exclusion pin ----

    /// INVARIANT: Codex must never receive permissions in auto-detect mode.
    ///
    /// This test pins the exclusion. If it fails, investigate before changing
    /// `agent_flags_for_auto_detect` — Codex is unsandboxed and blanket/seed
    /// grants require explicit `--agent codex --permissions` consent.
    #[test]
    fn test_codex_excluded_from_auto_detect_permissions_fan_out() {
        let base_flags = super::super::flags::InitFlags {
            project: false,
            yes: false,
            dry_run: false,
            uninstall: false,
            force: false,
            no_guidance: false,
            agent: None,
            wrappers: None,
            permissions: Some(true),
            permissions_tier: super::super::flags::PermissionsTier::Seed,
        };

        // Codex must get permissions=Some(false) regardless of the caller flags.
        let codex_flags = super::agent_flags_for_auto_detect(AgentKind::CodexCli, &base_flags);
        assert_eq!(
            codex_flags.permissions,
            Some(false),
            "Codex must be excluded from auto-detect permissions fan-out"
        );

        // Other agents must inherit the caller's permissions flag unchanged.
        let claude_flags = super::agent_flags_for_auto_detect(AgentKind::ClaudeCode, &base_flags);
        assert_eq!(
            claude_flags.permissions,
            Some(true),
            "ClaudeCode must inherit permissions=Some(true) from caller flags"
        );
        let gemini_flags = super::agent_flags_for_auto_detect(AgentKind::GeminiCli, &base_flags);
        assert_eq!(
            gemini_flags.permissions,
            Some(true),
            "GeminiCli must inherit permissions=Some(true) from caller flags"
        );
    }

    #[test]
    fn test_codex_excluded_when_permissions_is_none() {
        // When permissions=None (auto mode), Codex must also be excluded.
        let base_flags = super::super::flags::InitFlags {
            project: false,
            yes: false,
            dry_run: false,
            uninstall: false,
            force: false,
            no_guidance: false,
            agent: None,
            wrappers: None,
            permissions: None,
            permissions_tier: super::super::flags::PermissionsTier::Seed,
        };
        let codex_flags = super::agent_flags_for_auto_detect(AgentKind::CodexCli, &base_flags);
        assert_eq!(
            codex_flags.permissions,
            Some(false),
            "Codex must be excluded from auto permissions fan-out even in None (auto) mode"
        );
    }

    #[test]
    fn test_codex_not_excluded_when_permissions_is_false() {
        // When --no-permissions is set, Codex's permissions stays Some(false) — no change.
        let base_flags = super::super::flags::InitFlags {
            project: false,
            yes: false,
            dry_run: false,
            uninstall: false,
            force: false,
            no_guidance: false,
            agent: None,
            wrappers: None,
            permissions: Some(false),
            permissions_tier: super::super::flags::PermissionsTier::Seed,
        };
        let codex_flags = super::agent_flags_for_auto_detect(AgentKind::CodexCli, &base_flags);
        assert_eq!(
            codex_flags.permissions,
            Some(false),
            "Codex permissions must remain Some(false) when caller says Some(false)"
        );
    }

    // ---- permissions_blocks_fast_path ----

    #[test]
    fn test_permissions_blocks_fast_path_some_false_never_blocks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let flags = super::super::flags::InitFlags {
            project: false,
            yes: false,
            dry_run: false,
            uninstall: false,
            force: false,
            no_guidance: false,
            agent: None,
            wrappers: None,
            permissions: Some(false),
            permissions_tier: super::super::flags::PermissionsTier::Seed,
        };
        assert!(
            !super::permissions_blocks_fast_path(&flags, AgentKind::ClaudeCode, tmp.path()),
            "Some(false) must never block the fast path"
        );
    }

    #[test]
    fn test_permissions_blocks_fast_path_some_true_blocks_when_writer_exists() {
        let tmp = tempfile::TempDir::new().unwrap();
        let flags = super::super::flags::InitFlags {
            project: false,
            yes: false,
            dry_run: false,
            uninstall: false,
            force: false,
            no_guidance: false,
            agent: None,
            wrappers: None,
            permissions: Some(true),
            permissions_tier: super::super::flags::PermissionsTier::Seed,
        };
        assert!(
            super::permissions_blocks_fast_path(&flags, AgentKind::ClaudeCode, tmp.path()),
            "Some(true) must block the fast path when a writer exists"
        );
        // Cursor has no writer → must not block even with Some(true).
        assert!(
            !super::permissions_blocks_fast_path(&flags, AgentKind::Cursor, tmp.path()),
            "Some(true) must not block when agent has no writer (Cursor)"
        );
    }

    #[test]
    fn test_permissions_blocks_fast_path_none_no_sidecar_does_not_block() {
        let tmp = tempfile::TempDir::new().unwrap();
        let flags = super::super::flags::InitFlags {
            project: false,
            yes: false,
            dry_run: false,
            uninstall: false,
            force: false,
            no_guidance: false,
            agent: None,
            wrappers: None,
            permissions: None,
            permissions_tier: super::super::flags::PermissionsTier::Seed,
        };
        // No sidecar → no auto-seed → does not block.
        assert!(
            !super::permissions_blocks_fast_path(&flags, AgentKind::ClaudeCode, tmp.path()),
            "None with no sidecar must not block the fast path"
        );
    }
}
