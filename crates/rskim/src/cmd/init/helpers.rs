//! Shared helper functions and constants for `skim init`.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

// ============================================================================
// Shared constants
// ============================================================================

pub(super) const HOOK_SCRIPT_NAME: &str = "skim-rewrite.sh";
pub(super) const SETTINGS_FILE: &str = "settings.json";
pub(super) const SETTINGS_BACKUP: &str = "settings.json.bak";

// ============================================================================
// Config directory resolution (B6)
// ============================================================================

pub(super) fn resolve_config_dir(project: bool) -> anyhow::Result<PathBuf> {
    use crate::cmd::session::AgentKind;
    resolve_config_dir_for_agent(project, AgentKind::ClaudeCode)
}

/// Resolve the config directory for a specific agent.
///
/// For Claude Code: `CLAUDE_CONFIG_DIR` env > `~/.claude/` (or `.claude/` with --project)
/// For Cursor: `~/.cursor/` (macOS: `~/Library/Application Support/Cursor/`)
/// For Gemini: `~/.gemini/`
/// For Copilot: `~/.github/`
/// For others: falls back to `~/.{agent_cli_name}/`
pub(super) fn resolve_config_dir_for_agent(
    project: bool,
    agent: crate::cmd::session::AgentKind,
) -> anyhow::Result<PathBuf> {
    use crate::cmd::session::AgentKind;

    if project {
        let agent_dir_name = match agent {
            AgentKind::ClaudeCode => ".claude",
            AgentKind::Cursor => ".cursor",
            AgentKind::GeminiCli => ".gemini",
            AgentKind::CopilotCli => ".github",
            AgentKind::CodexCli => ".codex",
            AgentKind::OpenCode => ".opencode",
        };
        return Ok(std::env::current_dir()?.join(agent_dir_name));
    }

    // Check agent-specific env override
    if agent == AgentKind::ClaudeCode {
        if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
            return Ok(PathBuf::from(dir));
        }
    }

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;

    match agent {
        AgentKind::ClaudeCode => Ok(home.join(".claude")),
        AgentKind::Cursor => {
            // macOS: ~/Library/Application Support/Cursor/
            // Linux: ~/.config/Cursor/
            let macos_path = home
                .join("Library")
                .join("Application Support")
                .join("Cursor");
            if macos_path.is_dir() {
                Ok(macos_path)
            } else {
                Ok(home.join(".config").join("Cursor"))
            }
        }
        AgentKind::GeminiCli => Ok(home.join(".gemini")),
        AgentKind::CopilotCli => Ok(home.join(".github")),
        AgentKind::CodexCli => Ok(home.join(".codex")),
        AgentKind::OpenCode => Ok(home.join(".opencode")),
    }
}

/// Resolve a symlink to its absolute target path.
///
/// `read_link()` can return relative paths. This helper joins the relative
/// target with the symlink's parent directory, then canonicalizes to get an
/// absolute path.
pub(super) fn resolve_symlink(link: &Path) -> anyhow::Result<PathBuf> {
    let target = std::fs::read_link(link)?;
    if target.is_absolute() {
        Ok(target)
    } else {
        let parent = link.parent().ok_or_else(|| {
            anyhow::anyhow!("symlink has no parent directory: {}", link.display())
        })?;
        let resolved = parent.join(&target);
        std::fs::canonicalize(&resolved).map_err(|e| {
            anyhow::anyhow!(
                "failed to resolve symlink {} -> {}: {}",
                link.display(),
                resolved.display(),
                e
            )
        })
    }
}

// ============================================================================
// Interactive prompt helpers
// ============================================================================

pub(super) fn prompt_choice(prompt: &str, default: u32, valid: &[u32]) -> anyhow::Result<u32> {
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
pub(super) fn confirm_proceed() -> anyhow::Result<bool> {
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

pub(super) fn check_mark(ok: bool) -> &'static str {
    if ok {
        "\x1b[32m+\x1b[0m"
    } else {
        "\x1b[31m-\x1b[0m"
    }
}

// ============================================================================
// Help text
// ============================================================================

pub(super) fn print_help() {
    println!("skim init");
    println!();
    println!("  Install skim as an agent hook for automatic command rewriting");
    println!();
    println!("Usage: skim init [OPTIONS]");
    println!();
    println!("Options:");
    println!("  --global            Install to user-level config directory (default)");
    println!("  --project           Install to project-level config directory");
    println!("  --agent <name>      Target agent (default: claude-code)");
    println!(
        "                      Supported: claude-code, cursor, gemini, copilot, codex, opencode"
    );
    println!("  --yes, -y           Non-interactive mode (skip prompts)");
    println!("  --dry-run           Print actions without writing");
    println!("  --uninstall         Remove hook and clean up");
    println!("  --force             Force uninstall even if hook script was modified");
    println!("  --help, -h          Print help information");
    println!();
    println!("Examples:");
    println!("  skim init                          Interactive Claude Code setup (recommended)");
    println!("  skim init --yes                    Non-interactive with defaults");
    println!("  skim init --agent cursor --yes     Install for Cursor");
    println!("  skim init --agent gemini --yes     Install for Gemini CLI");
    println!("  skim init --project --yes          Install project-level hook");
    println!("  skim init --uninstall              Remove skim hook");
    println!("  skim init --dry-run                Preview actions without writing");
}
