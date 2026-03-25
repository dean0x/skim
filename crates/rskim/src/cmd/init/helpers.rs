//! Shared helper functions for `skim init`.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

// ============================================================================
// Config directory resolution (B6)
// ============================================================================

pub(super) fn resolve_config_dir(project: bool) -> anyhow::Result<PathBuf> {
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
