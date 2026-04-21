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
mod helpers;
mod install;
mod state;
mod uninstall;

use std::process::ExitCode;

use flags::parse_flags;
use helpers::print_help;
use install::run_install;
use uninstall::run_uninstall;

pub(crate) use helpers::resolve_config_dir_for_agent;
pub(crate) use state::has_skim_hook_entry;
pub(crate) use state::MAX_SETTINGS_SIZE;

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
}
