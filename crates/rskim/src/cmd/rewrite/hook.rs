//! Hook mode (#44) — Claude Code PreToolUse integration.
//!
//! Runs as an agent PreToolUse hook via `HookProtocol`. Reads JSON from stdin,
//! extracts the command field, rewrites if matched, and emits agent-specific
//! hook-protocol JSON. Each agent's `format_response()` controls the response shape.

use std::io::Read;
use std::process::ExitCode;

use crate::cmd::session::AgentKind;

use super::compound::{split_compound, try_rewrite_compound};
use super::engine::try_rewrite;
use super::types::CompoundSplitResult;

/// Parse the `--agent <name>` flag from rewrite args.
///
/// Returns `None` if `--agent` is not present or the value is missing.
/// Logs a warning for unknown agent names (never errors — hook mode must
/// never fail). Callers default `None` to `AgentKind::ClaudeCode`.
pub(super) fn parse_agent_flag(args: &[String]) -> Option<AgentKind> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--agent" {
            i += 1;
            if i < args.len() {
                let result = AgentKind::from_str(&args[i]);
                if result.is_none() {
                    crate::cmd::hook_log::log_hook_warning(&format!(
                        "unknown --agent value '{}', falling back to claude-code",
                        &args[i]
                    ));
                }
                return result;
            }
        }
        i += 1;
    }
    None
}

/// Maximum bytes to read from stdin in hook mode (64 KiB).
/// Hook payloads are small JSON objects; this prevents unbounded allocation.
pub(super) const HOOK_MAX_STDIN_BYTES: u64 = 64 * 1024;

/// Maximum time (in seconds) a hook invocation is allowed before self-termination.
///
/// Prevents slow hook processing from hanging the agent indefinitely.
/// The hook exits cleanly (exit 0, empty stdout) on timeout — this is a
/// passthrough, not an error. Logs a warning to hook.log for debugging.
pub(super) const HOOK_TIMEOUT_SECS: u64 = 5;

/// Run as an agent PreToolUse hook.
///
/// Protocol:
/// 1. Read JSON from stdin (bounded)
/// 2. Extract `tool_input.command`
/// 3. On parse/extract failure: exit 0, empty stdout (passthrough)
/// 4. Run command through rewrite logic
/// 5. On match: emit hook response JSON, exit 0
/// 6. On no match: exit 0, empty stdout (passthrough)
///
/// When `agent` is None or ClaudeCode, uses existing Claude Code logic.
/// Other agents passthrough (exit 0) until Phase 2 adds implementations.
///
/// SECURITY NOTE: Response shape is agent-specific — see each agent's
/// `format_response()` in `hooks/`. Claude Code never sets `permissionDecision`;
/// Copilot uses `permissionDecision: deny` (deny-with-suggestion pattern).
pub(super) fn run_hook_mode(agent: Option<AgentKind>) -> anyhow::Result<ExitCode> {
    use crate::cmd::hooks::{protocol_for_agent, HookSupport};

    // SKIM_PASSTHROUGH=1 disables all hook rewriting — the agent sees no hook response,
    // which is equivalent to a passthrough (the original command runs unchanged).
    if crate::cmd::is_passthrough_mode() {
        return Ok(ExitCode::SUCCESS);
    }

    // Watchdog: self-terminate after HOOK_TIMEOUT_SECS to prevent hanging the agent.
    // Uses a detached thread so it doesn't interfere with normal processing.
    // On timeout: log warning, exit 0 (passthrough — agent sees empty stdout).
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(HOOK_TIMEOUT_SECS));
        crate::cmd::hook_log::log_hook_warning("hook processing timed out after 5s, exiting");
        // SAFETY: process::exit(0) is intentional here. In hook mode, timeout means
        // passthrough (the agent sees empty stdout and proceeds normally). No Drop-based
        // cleanup is relied upon — all writes use explicit flush before this point, and
        // the watchdog only fires when processing has stalled beyond the timeout window.
        std::process::exit(0);
    });

    let agent_kind = agent.unwrap_or(AgentKind::ClaudeCode);
    let protocol = protocol_for_agent(agent_kind);

    // AwarenessOnly agents (Codex, OpenCode) have no hook mechanism — passthrough immediately
    if protocol.hook_support() == HookSupport::AwarenessOnly {
        return Ok(ExitCode::SUCCESS);
    }

    // #57: Integrity check — log-only (NEVER stderr, GRANITE #361 Bug 3).
    // Only run for Claude Code where we have the hook script infrastructure.
    // TODO: Extend integrity checks to Cursor, Gemini, and Copilot once their
    // hook script install paths are validated (they also report RealHook support).
    if agent_kind == AgentKind::ClaudeCode {
        let integrity_failed = check_hook_integrity(agent_kind);
        if !integrity_failed {
            // A2: Version mismatch check — rate-limited daily warning
            check_hook_version_mismatch(agent_kind);
        }
    }

    // Read stdin (bounded)
    let mut stdin_buf = String::new();
    let bytes_read = std::io::stdin()
        .lock()
        .take(HOOK_MAX_STDIN_BYTES)
        .read_to_string(&mut stdin_buf);

    let stdin_buf = match bytes_read {
        Ok(_) => stdin_buf,
        Err(_) => return Ok(ExitCode::SUCCESS), // passthrough on read failure
    };

    // Parse as JSON
    let json: serde_json::Value = match serde_json::from_str(&stdin_buf) {
        Ok(v) => v,
        Err(_) => {
            audit_hook("", false, "");
            return Ok(ExitCode::SUCCESS); // passthrough on parse failure
        }
    };

    // Extract command using the agent-specific protocol
    let command = match protocol.parse_input(&json) {
        Some(input) => input.command,
        None => {
            audit_hook("", false, "");
            return Ok(ExitCode::SUCCESS); // passthrough on missing/unparseable field
        }
    };

    // If already starts with "skim " — already rewritten, passthrough
    if command.starts_with("skim ") {
        audit_hook(&command, false, "");
        return Ok(ExitCode::SUCCESS);
    }

    // Check for compound operator characters on the original string directly,
    // before tokenizing, to avoid unnecessary allocations on the hot path.
    let has_operator_chars = command.contains("&&")
        || command.contains("||")
        || command.contains(';')
        || command.contains('|');

    // Tokenize into Vec<&str> (borrowing from `command`) to avoid String allocations.
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.is_empty() {
        audit_hook(&command, false, "");
        return Ok(ExitCode::SUCCESS);
    }

    let original = tokens.join(" ");

    // Fast path for non-compound commands
    let rewritten = if !has_operator_chars {
        try_rewrite(&tokens).map(|r| r.tokens.join(" "))
    } else {
        match split_compound(&original) {
            CompoundSplitResult::Bail => None,
            CompoundSplitResult::Simple(simple_tokens) => {
                let token_refs: Vec<&str> = simple_tokens.iter().map(|s| s.as_str()).collect();
                try_rewrite(&token_refs).map(|r| r.tokens.join(" "))
            }
            CompoundSplitResult::Compound(segments) => {
                try_rewrite_compound(&segments).map(|r| r.tokens.join(" "))
            }
        }
    };

    match rewritten {
        Some(ref rewritten_cmd) => {
            audit_hook(&command, true, rewritten_cmd);
            // Use agent-specific response format
            let response = protocol.format_response(rewritten_cmd);
            let json_out = serde_json::to_string(&response)?;
            println!("{json_out}");
        }
        None => {
            audit_hook(&command, false, "");
        }
    }

    Ok(ExitCode::SUCCESS)
}

/// Resolve the hook config directory for the given agent.
///
/// Delegates to the canonical `resolve_config_dir_for_agent` in `init/helpers.rs`
/// which handles agent-specific env overrides and home-directory fallback.
fn resolve_hook_config_dir(agent: AgentKind) -> Option<std::path::PathBuf> {
    crate::cmd::init::resolve_config_dir_for_agent(false, agent).ok()
}

/// Check if a daily rate-limit stamp allows warning today.
/// Returns `true` if caller should emit warning, `false` if already warned today.
/// Updates the stamp file as a side effect.
pub(super) fn should_warn_today(stamp_path: &std::path::Path) -> bool {
    let today = today_date_string();
    if let Ok(contents) = std::fs::read_to_string(stamp_path) {
        if contents.trim() == today {
            return false;
        }
    }
    let _ = std::fs::create_dir_all(stamp_path.parent().unwrap_or(std::path::Path::new(".")));
    let _ = std::fs::write(stamp_path, &today);
    true
}

/// #57: Check hook script integrity.
///
/// Uses SHA-256 hash verification. Warnings go to log file only (NEVER
/// stderr). Returns `true` if integrity check failed (tampered), `false`
/// if valid, missing, or check was skipped.
fn check_hook_integrity(agent: AgentKind) -> bool {
    let config_dir = match resolve_hook_config_dir(agent) {
        Some(dir) => dir,
        None => return false,
    };

    let agent_name = agent.cli_name();
    let script_path = config_dir.join("hooks").join("skim-rewrite.sh");

    if !script_path.exists() {
        return false;
    }

    match crate::cmd::integrity::verify_script_integrity(&config_dir, agent_name, &script_path) {
        Ok(true) => false, // Valid or missing hash (backward compat)
        Ok(false) => {
            // Tampered! Log warning to file (NEVER stderr).
            // Rate-limit: per-agent daily stamp to avoid log spam.
            let stamp_path = match cache_dir() {
                Some(dir) => dir.join(format!(".hook-integrity-warned-{agent_name}")),
                None => {
                    crate::cmd::hook_log::log_hook_warning(&format!(
                        "hook script tampered: {}",
                        script_path.display()
                    ));
                    return true;
                }
            };

            if should_warn_today(&stamp_path) {
                crate::cmd::hook_log::log_hook_warning(&format!(
                    "hook script tampered: {} (run `skim init --yes` to reinstall)",
                    script_path.display()
                ));
            }
            true
        }
        Err(_) => false, // Script unreadable — don't block the hook
    }
}

/// A2: Check for version mismatch between hook script and binary.
///
/// If `SKIM_HOOK_VERSION` is set and differs from the compiled version,
/// emit a daily warning to hook.log. Rate-limited via per-agent stamp file.
fn check_hook_version_mismatch(agent: AgentKind) {
    let hook_version = match std::env::var("SKIM_HOOK_VERSION") {
        Ok(v) => v,
        Err(_) => return, // not set — nothing to check
    };

    let compiled_version = env!("CARGO_PKG_VERSION");
    if hook_version == compiled_version {
        return; // versions match
    }

    let agent_name = agent.cli_name();

    // Rate limit: per-agent, warn at most once per day
    let stamp_path = match cache_dir() {
        Some(dir) => dir.join(format!(".hook-version-warned-{agent_name}")),
        None => return,
    };

    if should_warn_today(&stamp_path) {
        // Emit warning to hook log (NEVER stderr -- GRANITE #361 Bug 3)
        crate::cmd::hook_log::log_hook_warning(&format!(
            "version mismatch: hook script v{hook_version}, binary v{compiled_version} (run `skim init --yes` to update)"
        ));
    }
}

/// Maximum audit log size before rotation (10 MiB).
const AUDIT_LOG_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Maximum number of audit log archive files to keep.
const AUDIT_LOG_MAX_ARCHIVES: u32 = 3;

/// A3: Audit logging for hook invocations.
///
/// When `SKIM_HOOK_AUDIT=1`, appends a JSON line to `~/.cache/skim/hook-audit.log`.
/// The log is rotated when it exceeds [`AUDIT_LOG_MAX_BYTES`] to prevent unbounded
/// disk growth. Rotation uses the same shift scheme as `hook_log.rs`:
/// delete `.3`, rename `.2` -> `.3`, `.1` -> `.2`, current -> `.1`.
/// Failures are silently ignored (never break the hook).
fn audit_hook(original: &str, matched: bool, rewritten: &str) {
    if std::env::var("SKIM_HOOK_AUDIT").as_deref() != Ok("1") {
        return;
    }

    let log_path = match cache_dir() {
        Some(dir) => dir.join("hook-audit.log"),
        None => return,
    };

    // Rotate if the log exceeds the size limit (best-effort).
    // Shift scheme: delete .3, rename .2 -> .3, .1 -> .2, current -> .1.
    if let Ok(meta) = std::fs::metadata(&log_path) {
        if meta.len() >= AUDIT_LOG_MAX_BYTES {
            for i in (1..AUDIT_LOG_MAX_ARCHIVES).rev() {
                let from = audit_archive_path(&log_path, i);
                let to = audit_archive_path(&log_path, i + 1);
                let _ = std::fs::rename(&from, &to);
            }
            let archive_1 = audit_archive_path(&log_path, 1);
            let _ = std::fs::rename(&log_path, &archive_1);
        }
    }

    // Build JSON line
    let entry = serde_json::json!({
        "timestamp": today_date_string(),
        "original": original,
        "matched": matched,
        "rewritten": rewritten,
    });

    // Append (best-effort)
    let _ = std::fs::create_dir_all(log_path.parent().unwrap_or(std::path::Path::new(".")));
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        use std::io::Write;
        let _ = writeln!(file, "{}", entry);
    }
}

/// Build the path for an audit log archive file (e.g., `hook-audit.log.1`).
fn audit_archive_path(log_path: &std::path::Path, index: u32) -> std::path::PathBuf {
    let mut path = log_path.as_os_str().to_owned();
    path.push(format!(".{index}"));
    std::path::PathBuf::from(path)
}

/// Re-export `cache_dir` from `hook_log` to avoid duplication.
/// See `hook_log::cache_dir` for full documentation.
fn cache_dir() -> Option<std::path::PathBuf> {
    crate::cmd::hook_log::cache_dir()
}

/// Get today's date as YYYY-MM-DD string.
fn today_date_string() -> String {
    // Use SystemTime to avoid pulling in chrono dependency
    let now = std::time::SystemTime::now();
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Convert to days since epoch, then to date components
    let days = secs / 86400;
    // Simple date calculation (good enough for stamp file purposes)
    let (year, month, day) = crate::cmd::hook_log::days_to_date(days);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // parse_agent_flag
    // ========================================================================

    #[test]
    fn test_parse_agent_flag_present() {
        let args = vec![
            "--hook".to_string(),
            "--agent".to_string(),
            "claude-code".to_string(),
        ];
        assert_eq!(parse_agent_flag(&args), Some(AgentKind::ClaudeCode));
    }

    #[test]
    fn test_parse_agent_flag_codex() {
        let args = vec![
            "--hook".to_string(),
            "--agent".to_string(),
            "codex".to_string(),
        ];
        assert_eq!(parse_agent_flag(&args), Some(AgentKind::CodexCli));
    }

    #[test]
    fn test_parse_agent_flag_absent() {
        let args = vec!["--hook".to_string()];
        assert_eq!(parse_agent_flag(&args), None);
    }

    #[test]
    fn test_parse_agent_flag_missing_value() {
        let args = vec!["--hook".to_string(), "--agent".to_string()];
        assert_eq!(parse_agent_flag(&args), None);
    }

    // ========================================================================
    // Hook timeout constant
    // ========================================================================

    #[test]
    fn test_hook_timeout_constant() {
        assert_eq!(
            HOOK_TIMEOUT_SECS, 5,
            "Hook timeout must be 5 seconds (Claude Code hook timeout is 5s)"
        );
    }

    #[test]
    fn test_hook_max_stdin_bytes_constant() {
        assert_eq!(
            HOOK_MAX_STDIN_BYTES,
            64 * 1024,
            "Hook max stdin must be 64 KiB"
        );
    }

    // ========================================================================
    // should_warn_today rate-limit helper
    // ========================================================================

    #[test]
    fn test_should_warn_today_no_stamp() {
        let dir = tempfile::TempDir::new().unwrap();
        let stamp = dir.path().join("stamp");
        assert!(
            should_warn_today(&stamp),
            "should warn when no stamp exists"
        );
        assert!(stamp.exists(), "stamp file should be created");
    }

    #[test]
    fn test_should_warn_today_same_day_stamp() {
        let dir = tempfile::TempDir::new().unwrap();
        let stamp = dir.path().join("stamp");
        // First call should warn and write stamp
        assert!(should_warn_today(&stamp), "first call should warn");
        // Second call same day should NOT warn
        assert!(
            !should_warn_today(&stamp),
            "second call same day should not warn"
        );
    }
}
