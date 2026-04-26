//! Shared helper functions and constants for `skim init`.

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

/// Resolve the config directory for a specific agent.
///
/// For Claude Code: `CLAUDE_CONFIG_DIR` env > `~/.claude/` (or `.claude/` with --project)
/// For Cursor: `~/.cursor/` (macOS: `~/Library/Application Support/Cursor/`)
/// For Gemini: `~/.gemini/`
/// For Copilot: `~/.github/`
/// For others: falls back to `~/.{agent_cli_name}/`
pub(crate) fn resolve_config_dir_for_agent(
    project: bool,
    agent: crate::cmd::session::AgentKind,
) -> anyhow::Result<PathBuf> {
    use crate::cmd::session::AgentKind;

    if project {
        return Ok(std::env::current_dir()?.join(agent.dot_dir_name()));
    }

    // Check agent-specific env override
    if agent == AgentKind::ClaudeCode {
        if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
            return Ok(PathBuf::from(dir));
        }
    }

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;

    Ok(agent.config_dir(&home))
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
// Settings I/O helpers (shared by install and uninstall)
// ============================================================================

/// Resolve symlinks on the settings path, returning the original path if not a symlink.
pub(super) fn resolve_real_settings_path(path: &Path) -> anyhow::Result<PathBuf> {
    if path.is_symlink() {
        resolve_symlink(path)
    } else {
        Ok(path.to_path_buf())
    }
}

/// Read and parse a settings.json file, creating an empty object for missing or empty files.
///
/// Rejects files larger than [`super::state::MAX_SETTINGS_SIZE`] to prevent OOM.
pub(super) fn load_or_create_settings(path: &Path) -> anyhow::Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::Value::Object(serde_json::Map::new()));
    }

    let file_size = std::fs::metadata(path)?.len();
    if file_size > super::state::MAX_SETTINGS_SIZE {
        anyhow::bail!(
            "settings.json is too large ({} bytes, max {} bytes): {}\n\
             hint: This does not look like a valid Claude Code settings file",
            file_size,
            super::state::MAX_SETTINGS_SIZE,
            path.display()
        );
    }

    let contents = std::fs::read_to_string(path)?;
    if contents.trim().is_empty() {
        return Ok(serde_json::Value::Object(serde_json::Map::new()));
    }

    serde_json::from_str(&contents).map_err(|e| {
        anyhow::anyhow!(
            "Failed to parse {}: {}\n\
             hint: Fix the JSON manually, then re-run `skim init`",
            path.display(),
            e
        )
    })
}

/// Atomically write settings JSON to disk using tmp+rename.
pub(super) fn atomic_write_settings(
    settings: &serde_json::Value,
    path: &Path,
) -> anyhow::Result<()> {
    let pretty = serde_json::to_string_pretty(settings)?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, format!("{pretty}\n"))?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

// ============================================================================
// Guidance content
// ============================================================================

/// Generate the skim guidance section content with version markers.
///
/// This is a short (~300 token), self-contained preamble that tells agents
/// skim is available for structural code exploration. Uses `skim` (or
/// `rskim` as fallback) via Bash. Framed as strong directives, not soft
/// suggestions, to maximise adoption across all agent sessions.
pub(super) fn guidance_content(version: &str) -> String {
    format!(
        r#"<!-- skim-start v{version} -->
## Skim — Context-Optimized Code Reading

**IMPORTANT**: `skim` is installed and a rewrite hook is active that automatically
optimizes shell commands. For explicit use, call `skim` via Bash (or `rskim` if
`skim` is not in PATH).

### Rules

1. **ALWAYS prefer `skim <file>` over Read** when exploring or understanding code —
   60-80% fewer tokens with the same structural insight.
2. Use `skim <file> --mode=signatures` for API surfaces and interface discovery.
3. Use `skim 'src/**/*.ts'` for scanning directories or multi-file exploration.
4. **Use Read only when**:
   - You need exact line content for an Edit operation
   - You need a specific small section (< 50 lines) you will modify next
   - The file is non-code (images, binary)

### Quick Reference

```
skim <file>                    # structural overview (default)
skim <file> --mode=signatures  # API surface only
skim 'src/**/*.ts'             # multi-file scan
```
<!-- skim-end -->"#,
        version = version
    )
}

/// Generate skim guidance content wrapped in Cursor `.mdc` frontmatter.
///
/// Cursor's `.mdc` format requires YAML frontmatter. Skim owns the entire file.
pub(super) fn guidance_content_mdc(version: &str) -> String {
    let body = guidance_content(version);
    format!(
        "---\ndescription: \"skim — ALWAYS use skim over Read for code exploration\"\nalwaysApply: true\n---\n\n{body}"
    )
}

// ============================================================================
// Interactive prompt helpers
// ============================================================================

/// Prompt the user with "Proceed?" and return `true` if confirmed.
///
/// Uses `inquire::Confirm` when stdin is a terminal (D3) for a polished
/// interactive prompt. Falls back to raw `read_line()` in non-TTY environments
/// (CI, piped input) so automation is never broken.
///
/// Ctrl+C during the `inquire` prompt is treated as `Ok(false)` rather than
/// an error (D4).
pub(super) fn confirm_proceed() -> anyhow::Result<bool> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return confirm_proceed_raw();
    }
    match inquire::Confirm::new("Proceed?")
        .with_default(true)
        .prompt()
    {
        Ok(yes) => Ok(yes),
        Err(inquire::InquireError::OperationCanceled)
        | Err(inquire::InquireError::OperationInterrupted) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Raw (non-TTY) fallback for [`confirm_proceed`].
fn confirm_proceed_raw() -> anyhow::Result<bool> {
    use std::io::{BufRead, Read, Write};
    print!("Proceed? [Y/n] ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::BufReader::new(std::io::stdin().lock().take(256)).read_line(&mut input)?;
    let trimmed = input.trim().to_lowercase();
    let confirmed = trimmed.is_empty() || trimmed == "y" || trimmed == "yes";
    if confirmed {
        println!();
    }
    Ok(confirmed)
}

/// Colored status mark re-exported for the `init` module namespace.
pub(super) use crate::cmd::ux::check_mark;

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
    println!("  --yes, -y           Skip confirmation (uninstall only; install is always non-interactive)");
    println!("  --dry-run           Print actions without writing");
    println!("  --uninstall         Remove hook and clean up");
    println!("  --no-guidance       Skip injecting guidance into agent instruction file");
    println!("  --force             Force uninstall even if hook script was modified");
    println!("  --help, -h          Print help information");
    println!();
    println!("Examples:");
    println!("  skim init                          Install for Claude Code (recommended)");
    println!("  skim init --agent cursor           Install for Cursor");
    println!("  skim init --agent gemini           Install for Gemini CLI");
    println!("  skim init --project                Install project-level hook");
    println!("  skim init --uninstall              Remove skim hook");
    println!("  skim init --uninstall --yes        Uninstall without confirmation");
    println!("  skim init --dry-run                Preview actions without writing");
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_guidance_content_has_version_markers() {
        let content = guidance_content("2.1.0");
        assert!(content.starts_with("<!-- skim-start v2.1.0 -->"));
        assert!(content.ends_with("<!-- skim-end -->"));
        // Version appears in the skim-start marker
        assert!(content.contains("v2.1.0"));
        // Strong directive language
        assert!(content.contains("ALWAYS prefer"));
        assert!(content.contains("Use Read only when"));
        // SKIM_PASSTHROUGH is NOT documented in guidance — agents learn about it
        // from stderr hints emitted on compressed non-zero exits (shared.rs, mod.rs).
        assert!(!content.contains("SKIM_PASSTHROUGH"));
    }

    #[test]
    fn test_guidance_content_mdc_has_frontmatter() {
        let content = guidance_content_mdc("2.1.0");
        assert!(
            content.starts_with("---\n"),
            "Should start with YAML frontmatter"
        );
        assert!(content.contains("alwaysApply: true"));
        assert!(content.contains("description:"));
        assert!(content.contains("<!-- skim-start v2.1.0 -->"));
        assert!(content.contains("<!-- skim-end -->"));
    }

    #[test]
    fn test_load_or_create_settings_missing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("does-not-exist.json");
        let result = load_or_create_settings(&path).unwrap();
        assert!(result.is_object());
        assert!(result.as_object().unwrap().is_empty());
    }

    #[test]
    fn test_load_or_create_settings_empty_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "  \n").unwrap();
        let result = load_or_create_settings(&path).unwrap();
        assert!(result.is_object());
        assert!(result.as_object().unwrap().is_empty());
    }
}
