//! Interactive hook installation for Claude Code (#44)
//!
//! `skim init` installs skim as a Claude Code PreToolUse hook, enabling
//! automatic command rewriting. Supports global (`~/.claude/`) and project-level
//! (`.claude/`) installation with idempotent, atomic writes.
//!
//! The hook script calls `skim rewrite --hook` which reads Claude Code's
//! PreToolUse JSON, rewrites matched commands, and emits `updatedInput`.
//!
//! SECURITY INVARIANT (Claude Code): The Claude Code hook NEVER sets
//! `permissionDecision`. It only emits `updatedInput` inside
//! `hookSpecificOutput` and lets Claude Code's permission system evaluate
//! independently. Other agents have their own required response fields
//! (e.g., Cursor uses `"permission": "allow"`, Gemini CLI uses
//! `"decision": "allow"`) -- see each agent's `format_response()` in
//! `cmd/hooks/` for protocol-specific documentation.

mod flags;
mod guidance;
mod helpers;
mod install;
mod state;
mod uninstall;
pub(super) mod wrappers;

use std::path::PathBuf;
use std::process::ExitCode;

use flags::parse_flags;
use helpers::print_help;
use install::run_install;
use uninstall::run_uninstall;

pub(crate) use flags::DetectionEnv;
pub(crate) use flags::PermissionsTier;
pub(crate) use helpers::atomic_write_settings;
pub(crate) use helpers::backup_settings_file;
pub(crate) use helpers::load_or_create_settings;
pub(crate) use state::MAX_SETTINGS_SIZE;
pub(crate) use state::has_skim_hook_entry;

// ============================================================================
// B4: HookFacts DTO seam — cross-module hook state projection for `skim doctor`
// ============================================================================

/// Snapshot of hook installation facts for `skim doctor`.
///
/// This is a projection of [`state::DetectedState`] with only the fields that
/// cross the `cmd::init` module boundary. Internal state structs stay private.
pub(crate) struct HookFacts {
    /// True when any skim hook entry is present in the agent's config.
    pub(crate) hook_installed: bool,
    /// Version string recorded in the hook script (`SKIM_HOOK_VERSION`).
    pub(crate) hook_version: Option<String>,
    /// Git commit recorded in the hook script (`SKIM_HOOK_COMMIT`).
    pub(crate) hook_commit: Option<String>,
    /// Absolute path that the hook script pins as the binary (`SKIM_HOOK_BINARY`).
    pub(crate) hook_binary_pin: Option<String>,
    /// Whether the hook uses the pinned-binary format (exports `SKIM_HOOK_BINARY`).
    pub(crate) hook_uses_pinned_binary: bool,
    /// Whether the hook is fully current (version + pinned binary + commit all match).
    pub(crate) hook_is_current: bool,
    /// Path to the hook script file (`hook_config_dir/hooks/skim-rewrite.sh`).
    pub(crate) hook_script_path: PathBuf,
    /// Integrity classification of the hook script against its SHA-256 manifest.
    ///
    /// Derived from the manifest (an independent artefact), NOT from the hook
    /// script bytes — which is exactly what a tamper modifies (PF-016).
    pub(crate) script_integrity: crate::cmd::integrity::ScriptIntegrity,
}

/// Gather hook installation facts for a given agent — used by `skim doctor`.
///
/// Runs `detect_state` with the real process environment using global scope
/// (`project: false`), which is the scope that fires for every session.
/// Returns an error only if the config-dir resolver itself fails (e.g.,
/// cannot determine home directory).
pub(crate) fn hook_facts(agent: crate::cmd::session::AgentKind) -> anyhow::Result<HookFacts> {
    let init_flags = flags::InitFlags {
        project: false,
        yes: false,
        dry_run: false,
        uninstall: false,
        force: false,
        no_guidance: false,
        agent: Some(agent),
        wrappers: None,
        permissions: None,
        permissions_tier: flags::PermissionsTier::Seed,
    };
    let env = DetectionEnv::from_process();
    let detected = state::detect_state(&init_flags, agent, &env)?;

    // Evaluate hook_is_current() before partially moving out of `detected`.
    let is_current = detected.hook_is_current();
    let hook_script_path = detected
        .hook_config_dir
        .join("hooks")
        .join(helpers::HOOK_SCRIPT_NAME);

    // Integrity is derived from the SHA-256 manifest (independent of the script
    // bytes), so a tampered script cannot influence this verdict (PF-016).
    //
    // Verification: `detected.hook_config_dir` is the same directory that
    // `create_hook_script` in `install.rs` passes to `write_hash_manifest`
    // (`write_hash_manifest(&state.hook_config_dir, state.agent_cli_name, ...)`).
    // This alignment holds for every agent — including Copilot, whose
    // `hook_config_dir` redirects to `~/.copilot/` via `HookProtocol::hook_config_dir`.
    let script_integrity = crate::cmd::integrity::classify_script_integrity(
        &detected.hook_config_dir,
        agent.cli_name(),
        &hook_script_path,
    );

    Ok(HookFacts {
        hook_installed: detected.hook_installed,
        hook_version: detected.hook_version,
        hook_commit: detected.hook_commit,
        hook_binary_pin: detected.hook_binary_pin,
        hook_uses_pinned_binary: detected.hook_uses_pinned_binary,
        hook_is_current: is_current,
        hook_script_path,
        script_integrity,
    })
}

/// Run the `init` subcommand.
pub(crate) fn run(
    args: &[String],
    _analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
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

    if flags.uninstall {
        return run_uninstall(&flags);
    }

    run_install(&flags)
}

/// Returns `true` when hook script `contents` exports `SKIM_HOOK_BINARY`,
/// indicating the F6 pinned-binary format.
///
/// This is the single source of truth for the "has pinned binary marker" scan,
/// shared by `uses_pinned_binary` (state detection) and `is_hook_script_current`
/// (reinstall idempotence check) so that a format change updates both in lockstep.
pub(super) fn script_has_pinned_marker(contents: &str) -> bool {
    contents
        .lines()
        .any(|l| l.trim_start().starts_with("export SKIM_HOOK_BINARY="))
}

/// Build the clap `Command` definition for shell completions.
pub(super) fn command() -> clap::Command {
    clap::Command::new("init")
        .about("Install skim as an agent hook")
        .arg(
            clap::Arg::new("global")
                .long("global")
                .action(clap::ArgAction::SetTrue)
                .help("Install to user-level config directory (default)"),
        )
        .arg(
            clap::Arg::new("project")
                .long("project")
                .action(clap::ArgAction::SetTrue)
                .help("Install to project-level config directory"),
        )
        .arg(
            clap::Arg::new("agent")
                .long("agent")
                .value_name("NAME")
                .help("Target agent (default: claude-code)"),
        )
        .arg(
            clap::Arg::new("yes")
                .long("yes")
                .short('y')
                .action(clap::ArgAction::SetTrue)
                .help("Skip confirmation (uninstall only; install is always non-interactive)"),
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
        .arg(
            clap::Arg::new("no-guidance")
                .long("no-guidance")
                .action(clap::ArgAction::SetTrue)
                .help("Skip injecting guidance into agent instruction file"),
        )
        .arg(
            clap::Arg::new("force")
                .long("force")
                .action(clap::ArgAction::SetTrue)
                .help("Force operation (e.g., uninstall tampered hook)"),
        )
        .arg(
            clap::Arg::new("wrappers")
                .long("wrappers")
                .action(clap::ArgAction::SetTrue)
                .conflicts_with("no-wrappers")
                .help("Install PATH wrappers in ~/.skim/bin/ (skip interactive prompt)"),
        )
        .arg(
            clap::Arg::new("no-wrappers")
                .long("no-wrappers")
                .action(clap::ArgAction::SetTrue)
                .conflicts_with("wrappers")
                .help("Skip PATH wrapper installation (skip interactive prompt)"),
        )
        .arg(
            clap::Arg::new("permissions")
                .long("permissions")
                .action(clap::ArgAction::SetTrue)
                .conflicts_with("no-permissions")
                .conflicts_with("project")
                .help(
                    "Seed agent-native allow-list entries for skim read-only tools \
                     (user-scope only; incompatible with --project)",
                ),
        )
        .arg(
            clap::Arg::new("no-permissions")
                .long("no-permissions")
                .action(clap::ArgAction::SetTrue)
                .conflicts_with("permissions")
                .help("Skip seeding agent permission entries"),
        )
        .arg(
            clap::Arg::new("permissions-tier")
                .long("permissions-tier")
                .value_name("TIER")
                .help(
                    "Which tier of permissions to seed: seed (default), mirror, blanket. \
                     Effective only when --permissions is set.",
                ),
        )
}
