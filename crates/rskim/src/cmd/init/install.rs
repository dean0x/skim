//! Install flow for `skim init`.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use super::flags::InitFlags;
use super::helpers::{
    atomic_write_settings, check_mark, load_or_create_settings, resolve_real_settings_path,
    HOOK_SCRIPT_NAME, SETTINGS_BACKUP, SETTINGS_FILE,
};
use super::state::{detect_state, has_skim_hook_entry, DetectedState};
use crate::cmd::hooks::generate_hook_script;
use crate::cmd::session::{AgentKind, InstructionEnv};

/// Verify that the target agent appears to be installed on this system.
///
/// Checks for the expected config directory. If the agent's config dir
/// doesn't exist, returns an error with a helpful message rather than
/// silently creating an orphan config.
fn verify_agent_installed(state: &DetectedState, flags: &InitFlags) -> anyhow::Result<()> {
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

/// Return `true` when the guidance section in the agent's instruction file is
/// already at the current skim version (or when guidance is disabled).
///
/// Returns `true` (treat as current) when:
/// - `--no-guidance` is set, or
/// - The agent has no instruction file (guidance feature not applicable), or
/// - The file content contains the versioned start marker for `skim_version`.
fn is_guidance_current(flags: &InitFlags, skim_version: &str, env: &InstructionEnv) -> bool {
    if flags.no_guidance {
        return true;
    }
    let global = !flags.project;
    flags
        .agent
        .instruction_file(global, env)
        .map(|p| {
            std::fs::read_to_string(&p)
                .ok()
                .map(|c| c.contains(&format!("{} v{}", GUIDANCE_START, skim_version)))
                .unwrap_or(false)
        })
        .unwrap_or(true) // No instruction file path = guidance not applicable for this agent
}

pub(super) fn run_install(flags: &InitFlags) -> anyhow::Result<std::process::ExitCode> {
    let env = InstructionEnv::from_process();
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
    let guidance_current = is_guidance_current(flags, &state.skim_version, &env);
    if state.hook_installed
        && state.hook_is_current()
        && state.marketplace_installed
        && guidance_current
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

    // Install marketplace entry by default.
    let install_marketplace = true;
    let global = !flags.project;

    // Print summary
    let hook_script_path = state.config_dir.join("hooks").join(HOOK_SCRIPT_NAME);
    println!("  Summary:");
    if !state.hook_installed || !state.hook_is_current() {
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

    if flags.dry_run {
        print_dry_run_actions(&state, install_marketplace, flags.no_guidance, global, &env)?;
        return Ok(std::process::ExitCode::SUCCESS);
    }

    // Execute installation
    execute_install(&state, install_marketplace, flags.no_guidance, global, &env)?;

    println!();
    println!(
        "  Done! skim is now active in {}.",
        flags.agent.display_name()
    );
    println!();
    if install_marketplace {
        println!(
            "  Next step -- install the Skimmer plugin in {}:",
            flags.agent.display_name()
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

fn execute_install(
    state: &DetectedState,
    install_marketplace: bool,
    no_guidance: bool,
    global: bool,
    env: &InstructionEnv,
) -> anyhow::Result<()> {
    // B7: Create hook script
    create_hook_script(state)?;

    // B8: Patch settings.json
    patch_settings(state, install_marketplace)?;

    // Inject guidance into agent instruction file
    if !no_guidance {
        let agent = AgentKind::from_str(state.agent_cli_name).ok_or_else(|| {
            anyhow::anyhow!(
                "unrecognised agent CLI name {:?}; this is a bug in state detection",
                state.agent_cli_name
            )
        })?;
        inject_guidance(agent, global, env)?;
    }

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
            let has_bare_cmd = contents
                .lines()
                .any(|l| l.trim_start().starts_with("exec skim "));
            if contents.contains(&version_line) && has_bare_cmd {
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
// Settings.json patching (B8)
// ============================================================================

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
// Guidance injection
// ============================================================================

const GUIDANCE_START: &str = "<!-- skim-start";
const GUIDANCE_END: &str = "<!-- skim-end -->";

/// Maximum byte size for an instruction file before we skip reading it.
/// Prevents unbounded allocations on corrupted or adversarially crafted files.
const MAX_INSTRUCTION_FILE_SIZE: u64 = 1_048_576; // 1 MiB

/// Find the skim guidance section markers in content.
/// Returns `Some((start_byte, end_byte))` where end_byte includes the end marker.
/// Returns `None` if markers are missing or in wrong order (corrupted file).
fn find_skim_section(content: &str) -> Option<(usize, usize)> {
    let start = content.find(GUIDANCE_START)?;
    let end_marker = content.find(GUIDANCE_END)?;
    if start >= end_marker {
        return None; // Markers in wrong order
    }
    Some((start, end_marker + GUIDANCE_END.len()))
}

/// Resolve the instruction file path for `agent`, falling back from global to
/// project scope when the agent does not support a global instruction file.
fn resolve_instruction_path(
    agent: AgentKind,
    global: bool,
    env: &InstructionEnv,
) -> anyhow::Result<std::path::PathBuf> {
    match agent.instruction_file(global, env) {
        Some(p) => Ok(p),
        None if global => {
            eprintln!(
                "  {} does not support global guidance. Using project scope.",
                agent.display_name()
            );
            agent
                .instruction_file(false, env)
                .ok_or_else(|| anyhow::anyhow!("No instruction file for {}", agent.display_name()))
        }
        None => anyhow::bail!("No instruction file for {}", agent.display_name()),
    }
}

/// Read the instruction file at `path`, applying size and read-error guards.
///
/// Returns `Ok(None)` when the file should be skipped (too large or unreadable),
/// which is treated as a soft warning rather than a hard error.
fn read_existing_safely(path: &std::path::Path) -> anyhow::Result<Option<String>> {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > MAX_INSTRUCTION_FILE_SIZE {
            eprintln!(
                "  warning: {} is too large ({} bytes), skipping guidance",
                path.display(),
                meta.len()
            );
            return Ok(None);
        }
    }
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) => {
            eprintln!(
                "  warning: could not read {}: {} (skipping guidance)",
                path.display(),
                e
            );
            Ok(None)
        }
    }
}

/// Write `new_content` as a new instruction file at `path` (create mode).
fn guidance_create(path: &std::path::Path, new_content: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    atomic_write_stripped(path, &format!("{new_content}\n"))
}

/// Replace the existing skim section in `existing` with `new_content` (update mode).
fn guidance_update(
    path: &std::path::Path,
    existing: &str,
    start: usize,
    end: usize,
    new_content: &str,
) -> anyhow::Result<()> {
    let updated = format!("{}{}{}", &existing[..start], new_content, &existing[end..]);
    atomic_write_stripped(path, &updated)
}

/// Append `new_content` to the end of `existing` (append mode).
fn guidance_append(
    path: &std::path::Path,
    existing: &str,
    new_content: &str,
) -> anyhow::Result<()> {
    let mut content = existing.to_owned();
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push('\n');
    content.push_str(new_content);
    content.push('\n');
    atomic_write_stripped(path, &content)
}

/// Inject skim guidance section into the agent's main instruction file.
///
/// Four modes:
/// - **Create**: File doesn't exist → create with just the guidance section
/// - **Append**: File exists but has no skim section → append to end
/// - **Update**: File has a skim section with older version → replace in place
/// - **Skip**: File has a skim section with current version → idempotent no-op
pub(super) fn inject_guidance(
    agent: AgentKind,
    global: bool,
    env: &InstructionEnv,
) -> anyhow::Result<()> {
    let path = resolve_instruction_path(agent, global, env)?;
    let path = super::helpers::resolve_real_settings_path(&path)?;

    let version = env!("CARGO_PKG_VERSION");
    let is_mdc = path.extension().is_some_and(|ext| ext == "mdc");
    let new_content = if is_mdc {
        super::helpers::guidance_content_mdc(version)
    } else {
        super::helpers::guidance_content(version)
    };

    if path.exists() {
        let existing = match read_existing_safely(&path)? {
            Some(s) => s,
            None => return Ok(()), // soft skip (too large or unreadable)
        };

        // Detect corrupted markers (present but in wrong order)
        if find_skim_section(&existing).is_none() && existing.contains(GUIDANCE_START) {
            eprintln!(
                "  warning: skim markers in {} appear corrupted (skipping guidance update)",
                path.display()
            );
            return Ok(());
        }

        if let Some((start, end)) = find_skim_section(&existing) {
            // Same version? Skip.
            if existing[start..end].contains(&format!("v{version}")) {
                println!(
                    "  {} Guidance already current (v{})",
                    check_mark(true),
                    version
                );
                return Ok(());
            }

            // Different version — update in place.
            guidance_update(&path, &existing, start, end, &new_content)?;
            println!(
                "  {} Updated guidance in {} (-> v{})",
                check_mark(true),
                path.display(),
                version
            );
            return Ok(());
        }

        // No skim section — append.
        guidance_append(&path, &existing, &new_content)?;
    } else {
        // File doesn't exist — create.
        guidance_create(&path, &new_content)?;
    }

    // Legacy cleanup: remove skim markers from .cursorrules if this is a Cursor agent
    if is_mdc && path.to_string_lossy().contains("skim.mdc") {
        clean_legacy_cursorrules()?;
    }

    println!(
        "  {} Installed guidance in {}",
        check_mark(true),
        path.display()
    );

    // For project scope, remind user to commit
    if !global {
        println!(
            "  Note: guidance added to {} — commit to share with your team.",
            path.display()
        );
    }

    Ok(())
}

/// Remove skim guidance section from the agent's main instruction file.
pub(super) fn remove_guidance(
    agent: AgentKind,
    global: bool,
    env: &InstructionEnv,
) -> anyhow::Result<()> {
    let path = match agent.instruction_file(global, env) {
        Some(p) if p.exists() => p,
        _ => {
            // For Cursor, even if the new path doesn't exist, check legacy .cursorrules
            if agent == AgentKind::Cursor {
                clean_legacy_cursorrules()?;
            }
            return Ok(());
        }
    };

    // Issue 5: resolve symlinks before operating on the path
    let path = super::helpers::resolve_real_settings_path(&path)?;

    let content = match read_existing_safely(&path)? {
        Some(s) => s,
        None => return Ok(()), // soft skip (too large or unreadable)
    };
    if let Some((start, end)) = find_skim_section(&content) {
        if path.extension().is_some_and(|ext| ext == "mdc") {
            // Skim owns .mdc files entirely — delete on removal
            std::fs::remove_file(&path)?;
        } else {
            let mut updated = format!(
                "{}{}",
                content[..start].trim_end_matches('\n'),
                &content[end..]
            )
            .trim()
            .to_string();
            if !updated.is_empty() {
                updated.push('\n');
            }

            if updated.is_empty() {
                // File was only the skim section — delete the file
                std::fs::remove_file(&path)?;
            } else {
                // Atomic write using dynamic extension (issue 10)
                atomic_write_stripped(&path, &updated)?;
            }
        }
        println!(
            "  {} Removed guidance from {}",
            check_mark(true),
            path.display()
        );
    }

    // Also clean legacy .cursorrules for Cursor
    if agent == AgentKind::Cursor {
        clean_legacy_cursorrules()?;
    }

    Ok(())
}

/// Remove the skim section from `content`, stripping surrounding blank lines.
///
/// Returns `None` if no skim section was found.
/// Returns `Some(cleaned)` where `cleaned` is the trimmed remainder with a
/// trailing newline appended when non-empty.
fn strip_skim_section(content: &str) -> Option<String> {
    let (start, end) = find_skim_section(content)?;
    let trimmed = format!(
        "{}{}",
        content[..start].trim_end_matches('\n'),
        &content[end..]
    )
    .trim()
    .to_string();
    let final_content = if trimmed.is_empty() {
        String::new()
    } else {
        trimmed + "\n"
    };
    Some(final_content)
}

/// Atomically write `content` to `path`, using a sibling `.tmp`-suffixed file.
///
/// The tmp extension mirrors the original file extension so rename targets the
/// correct filesystem entry (e.g. `skim.mdc.tmp` → `skim.mdc`).
///
/// Cleans up the tmp file on both write and rename failures (S1).
fn atomic_write_stripped(path: &std::path::Path, content: &str) -> anyhow::Result<()> {
    // Build tmp extension: "<original_ext>.tmp" or "tmp" if no extension.
    let tmp_ext = match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{ext}.tmp"),
        None => "tmp".to_string(),
    };
    let tmp_path = path.with_extension(&tmp_ext);
    if let Err(e) = std::fs::write(&tmp_path, content) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e.into());
    }
    Ok(())
}

/// Clean up skim markers from legacy `.cursorrules` during Cursor migration.
///
/// Leaves the file in place (even if empty) since the user may have created it
/// intentionally. Only removes the skim section markers.
fn clean_legacy_cursorrules() -> anyhow::Result<()> {
    let legacy = std::path::PathBuf::from(".cursorrules");
    if !legacy.exists() {
        return Ok(());
    }
    // S2: apply resolve_real_settings_path so symlinks are handled consistently
    let legacy = super::helpers::resolve_real_settings_path(&legacy)?;
    if let Ok(content) = std::fs::read_to_string(&legacy) {
        if let Some(cleaned) = strip_skim_section(&content) {
            // Leave the file in place even when cleaned is empty (user may own it).
            atomic_write_stripped(&legacy, &cleaned)?;
            println!("  {} Cleaned legacy .cursorrules markers", check_mark(true));
        }
    }
    Ok(())
}

// ============================================================================
// Dry-run output (B11)
// ============================================================================

pub(super) fn print_dry_run_actions(
    state: &DetectedState,
    install_marketplace: bool,
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
    if install_marketplace {
        println!(
            "  [dry-run] Would register: skim marketplace in {}",
            SETTINGS_FILE
        );
    }
    if !no_guidance {
        let agent = AgentKind::from_str(state.agent_cli_name).ok_or_else(|| {
            anyhow::anyhow!(
                "unrecognised agent CLI name {:?}; this is a bug in state detection",
                state.agent_cli_name
            )
        })?;
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
    use crate::cmd::init::helpers::guidance_content;

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
}
