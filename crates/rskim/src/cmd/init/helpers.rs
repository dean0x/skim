//! Shared helper functions and constants for `skim init`.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

// ============================================================================
// Shared constants
// ============================================================================

pub(super) const HOOK_SCRIPT_NAME: &str = "skim-rewrite.sh";
pub(super) const SETTINGS_BACKUP: &str = "settings.json.bak";

/// Resolve the running skim binary to its canonical absolute path.
///
/// Uses `std::env::current_exe()` and then `std::fs::canonicalize()` to follow
/// any symlinks. This is the single source of truth for the binary path used
/// in hook scripts (`SKIM_HOOK_BINARY`) and wrapper installation so that all
/// three sites agree — avoiding the two-clone mismatch where both clones share
/// the same version string but `hook_is_current()` cannot detect they are
/// different binaries.
///
/// Canonicalize failure (e.g., binary deleted while running) falls back to the
/// raw path from `current_exe()` rather than failing.
pub(crate) fn resolve_skim_binary() -> anyhow::Result<PathBuf> {
    let p = std::env::current_exe().map_err(|e| {
        anyhow::anyhow!(
            "cannot determine the skim binary path: {e}\n\
             hint: re-run `skim init` from the skim binary directly"
        )
    })?;
    Ok(std::fs::canonicalize(&p).unwrap_or(p))
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
///
/// `pub(crate)` so that the permissions writers (cmd/permissions/claude.rs) can
/// reuse this helper without duplicating the size-cap logic.
pub(crate) fn load_or_create_settings(path: &Path) -> anyhow::Result<serde_json::Value> {
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
///
/// On Unix, the temporary file is created with mode 0o600 (owner read/write only)
/// before the rename, so the settings file is never world-readable — regardless
/// of the process umask.
///
/// `pub(crate)` so that the permissions writers (cmd/permissions/claude.rs) can
/// reuse this helper without duplicating the atomic-write logic.
pub(crate) fn atomic_write_settings(
    settings: &serde_json::Value,
    path: &Path,
) -> anyhow::Result<()> {
    let pretty = serde_json::to_string_pretty(settings)?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, format!("{pretty}\n"))?;
    #[cfg(unix)]
    {
        let perms = std::fs::Permissions::from_mode(0o600);
        if let Err(e) = std::fs::set_permissions(&tmp_path, perms) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e.into());
        }
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

// ============================================================================
// Guidance content
// ============================================================================

/// Generate the skim guidance section content with version markers.
///
/// Principle-based guidance that helps agents decide when skim adds value
/// vs when to Read directly.
pub(super) fn guidance_content(version: &str) -> String {
    format!(
        r#"<!-- skim-start v{version} -->
## Skim — Context-Optimized Code Reading

`skim` is installed and a rewrite hook is active that automatically optimizes
shell commands. For explicit use, call `skim` via Bash.

### What skim does

Skim shows you the **structure** of a file — signatures, types, headings,
definitions — while stripping implementation details and body content.
It works on code files (functions/classes) and prose files (headings/sections).
Use `-n` / `--line-numbers` to enrich the output with original source line numbers.

### Command wrapping

The rewrite hook may also wrap supported shell commands (`ls`, `wc`,
`git diff`, `gh`, test runners, ...) as `skim <tool>`: the same command
runs with the same arguments and exit code, and skim compresses its
output. Seeing `skim ls` run in place of `ls` is expected behavior, not
an error. Search commands (`grep`, `rg`) are also wrapped but pass through
output byte-for-byte identical to the raw tool, with no compression.
File reads (`cat`, `head`, `tail` on code files) are rewritten
into direct skim reads (example: `cat file.ts` becomes
`skim file.ts --mode=pseudo`), so the output is a structured view, not
raw file contents; seeing `skim` run in place of the original command
is expected.

Compression changes how results are presented, and rewritten file reads
show a structured view rather than exact file contents; skim prints a
one-line stderr notice whenever the view it served differs from the raw
file. If compressed output ever looks incomplete,
garbled, or inconsistent with what you expected, flag it to the user
rather than silently working around it.

At install time the user may have approved a small allowlist of
these skim-wrapped commands, so some run without asking again while
others still prompt for approval — both are intended.
Do not change permission or allowlist settings yourself to avoid a
prompt. If a wrapped command is denied when you did not expect it,
surface the denial to the user rather than working around it.

### When to use skim

**General principle:** Use skim when structure is sufficient for your task.
If your next step requires the actual content (editing, understanding logic,
debugging), go straight to Read — don't skim first.

Skim earns its cost when you want to **orient** — understand what exists in a
file or across files without committing to reading all the content. Examples:
- Understanding what a module defines before deciding what to read in detail
- Surveying a directory of files to build a mental model of a codebase area
- Checking what sections a spec or config file contains

Skim wastes tokens when you already know you need the content — most commonly
when you're about to edit a file. Read it directly.

### Anti-pattern: skim then Read the same file

If you skim a file and then Read it (in full or in large part), you paid for
the file twice. This means skim was the wrong choice — you needed content,
not structure. Pick one tool per file based on what you actually need.

The valid skim→Read sequence is across **different files**: skim several files
to orient, then Read the one you actually need in detail.

### Quick Reference

```
skim <file>                      # structural overview (default mode)
skim -n <file>                   # structural overview with source line numbers
skim 'src/**/*.ts'               # multi-file scan (glob)
skim file1.ts file2.ts           # multi-file scan (explicit files)
skim src/                        # all files in directory recursively
skim <file> --max-lines 50       # cap output with AST-aware truncation
skim <file> --tokens 500         # fit output within a token budget

# Modes (most to least content):
# full → pseudo → structure (default) → minimal → signatures → types
skim <file> --mode=types         # type definitions only
skim <file> --mode=signatures    # function/method signatures only
skim <file> --mode=pseudo        # logic without syntactic noise
```

### Heatmap — Git History Risk Analysis

`skim heatmap` analyzes git history to surface risk hotspots: high-churn files,
tightly coupled file pairs, fix-after-touch patterns, module boundary violations,
and bus-factor concentration. Run it when you need to understand where risk lives
in a codebase before starting work.

**When to use heatmap:**
- Before a refactoring task — identify which files are risky to change
- When triaging bugs — find files with high fix density
- Exploring an unfamiliar codebase — understand coupling and ownership patterns
- Before code review — check if the changed files are high-risk hotspots

```
skim heatmap                     # default: last 90 days, text output
skim heatmap --json              # structured JSON for programmatic use
skim heatmap --path src/lib/     # scope to a subdirectory
skim heatmap --window sprint     # last 14 days only
skim heatmap --insights          # threshold-filtered findings only
skim heatmap --insights --json   # insights as JSON (agent-friendly)
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
        "---\ndescription: \"skim — context-optimized code reading for AI agents\"\nalwaysApply: true\n---\n\n{body}"
    )
}

// ============================================================================
// Settings backup helper (pub(crate) so permissions writers can reuse it)
// ============================================================================

/// Back up a settings file before first modification.
///
/// Creates `{config_dir}/settings.json.bak` — a byte-for-byte copy of
/// `real_path`. Rejects paths that became symlinks after resolution (TOCTOU
/// guard: a symlink appearing here after `resolve_real_settings_path()` ran
/// indicates a race or tamper attempt).
///
/// Used by the Claude permissions writer before modifying `settings.json`.
pub(crate) fn backup_settings_file(config_dir: &Path, real_path: &Path) -> anyhow::Result<()> {
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

/// Gate agent-permission seeding on explicit human consent at a TTY.
///
/// # Security contract
///
/// TTY-gating is the **primary defense** against prompt-injected agent
/// self-grant: if stdin is not an interactive terminal (CI, piped input,
/// sub-agent invocation), this function refuses immediately and returns
/// `false` — no I/O, no prompt, no grant.
///
/// Residual pty risk: a malicious agent that already controls the pty can
/// simulate keystrokes. This attack requires prior execution compromise, which
/// is outside the install-time threat model. Defense-in-depth controls (sidecar
/// manifests, per-tool scope) limit blast radius.
///
/// # Bypass prohibition
///
/// The `--yes` flag is for hook uninstall confirmation only. It does NOT bypass
/// this function — callers invoke `confirm_grant` unconditionally
/// whenever permissions are requested. Do NOT add a flag parameter that skips
/// the TTY check.
///
/// # Return value
///
/// Returns `true` only when stdin is an interactive TTY **and** the user
/// explicitly types `y` or `yes` (case-insensitive). Returns `false` for:
/// - Non-TTY stdin (CI, pipes, agent invocations).
/// - Empty input (default is deny).
/// - Anything other than `y` / `yes`.
/// - EOF before a response.
/// - Any I/O error.
///
/// # Arguments
///
/// - `agent_label` — human-readable agent name for the prompt (e.g. "Claude Code").
/// - `config_file` — exact path of the file that will be modified.
/// - `entries` — each entry that will be added, printed verbatim.
pub(crate) fn confirm_grant(agent_label: &str, config_file: &Path, entries: &[String]) -> bool {
    use std::io::{BufRead, IsTerminal, Read, Write};

    // Non-TTY: refuse immediately without printing anything.
    // This is the non-negotiable first gate — no prompt on pipes or sub-agents.
    if !std::io::stdin().is_terminal() {
        return false;
    }

    // Print the consent prompt.
    println!();
    println!(
        "skim wants to add allow-list entries to: {}",
        config_file.display()
    );
    println!("  Agent:  {agent_label}");
    println!("  Entries to add ({} total):", entries.len());
    for entry in entries {
        println!("    {entry}");
    }
    println!();
    println!("These entries grant skim read-only tool permissions at install time.");
    println!("skim never modifies permissions at runtime — only `skim init` does.");
    println!();

    print!("Grant these permissions? [y/N] ");
    if std::io::stdout().flush().is_err() {
        return false;
    }

    let mut input = String::new();
    // Read at most 64 bytes — a valid answer is always short.
    let n = match std::io::BufReader::new(std::io::stdin().lock().take(64)).read_line(&mut input) {
        Ok(n) => n,
        Err(_) => return false,
    };

    // EOF on a TTY (n == 0) → deny.
    if n == 0 {
        return false;
    }

    let trimmed = input.trim().to_ascii_lowercase();
    trimmed == "y" || trimmed == "yes"
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
    println!("                      Supported: claude-code, cursor, gemini, copilot, codex, crush");
    println!(
        "  --yes, -y           Skip confirmation (uninstall only; install is always non-interactive)"
    );
    println!("  --dry-run           Print actions without writing");
    println!("  --uninstall         Remove hook and clean up");
    println!("  --no-guidance       Skip injecting guidance into agent instruction file");
    println!("  --force             Force uninstall even if hook script was modified");
    println!("  --wrappers          Install PATH wrappers in ~/.skim/bin/ (skip prompt)");
    println!("  --no-wrappers       Skip PATH wrapper installation (skip prompt)");
    println!("  --help, -h          Print help information");
    println!();
    println!("Shell Wrappers:");
    println!("  PATH wrappers in ~/.skim/bin/ intercept tool calls from sub-agents that");
    println!("  bypass PreToolUse hooks. Each symlink (e.g. ~/.skim/bin/git) points to");
    println!("  the skim binary; skim detects the tool name from argv[0] and compresses");
    println!("  the output. Add to ~/.zshrc or ~/.bashrc to enable:");
    println!("    export PATH=\"$HOME/.skim/bin:$PATH\"");
    println!("    export SKIM_SESSION_ID=\"<your-session-id>\"  # optional, for analytics");
    println!();
    println!("Examples:");
    println!("  skim init                          Install for Claude Code (recommended)");
    println!("  skim init --agent cursor           Install for Cursor");
    println!("  skim init --agent gemini           Install for Gemini CLI");
    println!("  skim init --project                Install project-level hook");
    println!("  skim init --wrappers               Install with PATH wrappers");
    println!("  skim init --no-wrappers            Install without PATH wrappers");
    println!("  skim init --uninstall              Remove skim hook and wrappers");
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
        // Principle-based guidance
        assert!(content.contains("When to use skim"));
        assert!(content.contains("General principle"));
        assert!(content.contains("Anti-pattern: skim then Read"));
        // Quick reference covers new features
        assert!(content.contains("--tokens"));
        assert!(content.contains("--max-lines"));
        assert!(content.contains("--mode=types"));
        // No prescriptive decision table
        assert!(!content.contains("Choose ONE tool per file"));
        // SKIM_PASSTHROUGH is NOT documented in guidance — agents learn about it
        // from stderr hints emitted on compressed non-zero exits (shared.rs, mod.rs).
        assert!(!content.contains("SKIM_PASSTHROUGH"));
        // Command wrapping section explains that the rewrite hook may wrap
        // supported tools and that agents should flag garbled output to the user.
        assert!(
            content.contains("### Command wrapping"),
            "Guidance must contain '### Command wrapping' section"
        );
        assert!(
            content.contains("wrap supported shell commands"),
            "Guidance must explain which commands the hook may wrap"
        );
        assert!(
            content.contains("flag it to the user"),
            "Guidance must instruct agents to flag garbled compressed output"
        );
        // Permissions-awareness: agents must not change permission/allowlist settings
        assert!(
            content.contains("Do not change permission or allowlist settings"),
            "Guidance must instruct agents not to change permission or allowlist settings"
        );
        // No rskim mention in guidance body
        assert!(!content.contains("rskim"));
        // cat/head/tail file reads are described as rewritten into direct skim reads
        assert!(
            content.contains("`cat`, `head`, `tail`"),
            "Guidance must mention cat/head/tail file reads"
        );
        // Wording must convey that output is not raw file contents
        assert!(
            content.contains("not") && content.contains("raw"),
            "Guidance must explain that rewritten file reads are not raw file contents"
        );
        // stderr notice described
        assert!(
            content.contains("stderr notice"),
            "Guidance must mention the stderr notice emitted when view differs"
        );
        // Old incorrect wording must be gone
        assert!(
            !content.contains("does not change what the command did"),
            "Old incorrect claim must be replaced with corrected wording"
        );
        // Heatmap section present with key content
        assert!(content.contains("Heatmap"));
        assert!(content.contains("skim heatmap"));
        assert!(content.contains("risk"));
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

    // ---- confirm_grant — non-TTY refusal ----

    /// The test harness stdin is never a TTY, so confirm_grant must return false
    /// immediately without blocking on a read.
    #[test]
    fn test_confirm_grant_refuses_non_tty() {
        // In a test harness stdin is not a TTY. confirm_grant must return false
        // without blocking. We pass nonsense entries — they must not be written
        // to anything because the function exits before any output or read.
        let dir = tempfile::TempDir::new().unwrap();
        let config_file = dir.path().join("settings.json");
        let entries = vec!["Bash(skim df:*)".to_string()];
        let result = confirm_grant("Claude Code", &config_file, &entries);
        assert!(
            !result,
            "confirm_grant must return false in a non-TTY context"
        );
    }

    /// Multiple calls must all refuse consistently — confirm_grant must not
    /// change state or block on repeated invocations in a non-TTY context.
    #[test]
    fn test_confirm_grant_non_tty_is_idempotent() {
        let dir = tempfile::TempDir::new().unwrap();
        let config_file = dir.path().join("settings.json");
        let entries = vec![
            "Bash(skim grep:*)".to_string(),
            "Bash(skim ls:*)".to_string(),
        ];
        for _ in 0..3 {
            assert!(
                !confirm_grant("Claude Code", &config_file, &entries),
                "confirm_grant must consistently return false in non-TTY context"
            );
        }
    }

    // ---- backup_settings_file ----

    #[test]
    fn test_backup_settings_file_creates_backup() {
        let dir = tempfile::TempDir::new().unwrap();
        let settings = dir.path().join("settings.json");
        std::fs::write(&settings, b"{\"key\":\"value\"}").unwrap();

        backup_settings_file(dir.path(), &settings).unwrap();

        let backup = dir.path().join(SETTINGS_BACKUP);
        assert!(backup.exists(), "backup file must be created");
        let backup_bytes = std::fs::read(&backup).unwrap();
        assert_eq!(
            backup_bytes, b"{\"key\":\"value\"}",
            "backup must be a byte-exact copy"
        );
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
