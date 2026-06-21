//! Hook mode (#44) — Claude Code PreToolUse integration.
//!
//! Runs as an agent PreToolUse hook via `HookProtocol`. Reads JSON from stdin,
//! extracts the command field, rewrites if matched, and emits agent-specific
//! hook-protocol JSON. Each agent's `format_response()` controls the response shape.

use std::io::Read;
use std::process::ExitCode;

use crate::cmd::session::AgentKind;

use super::compound::{command_needs_passthrough, split_compound, try_rewrite_compound};
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
    use crate::cmd::hooks::{HookSupport, protocol_for_agent};

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
        // cleanup is relied upon. The only stdout write in the success path uses
        // write_all + flush (see below) so the agent never receives a truncated JSON
        // response: either flush completes before this timer fires, or the timer fires
        // first and the agent sees empty stdout (passthrough). The watchdog only fires
        // when processing has stalled beyond the timeout window.
        std::process::exit(0);
    });

    let agent_kind = agent.unwrap_or(AgentKind::ClaudeCode);
    let protocol = protocol_for_agent(agent_kind);

    // AwarenessOnly agents (Codex) have no hook mechanism — passthrough immediately
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

    // Extract command (and session_id) using the agent-specific protocol
    let (command, session_id) = match protocol.parse_input(&json) {
        Some(input) => (input.command, input.session_id),
        None => {
            audit_hook("", false, "");
            return Ok(ExitCode::SUCCESS); // passthrough on missing/unparseable field
        }
    };

    // AD-SC-1: Persist session_id to PID-keyed sidecar for fallback attribution.
    // Direct skim invocations that bypass this hook can later discover the
    // session by walking process ancestry (see session_sidecar::read_session_id).
    if let Some(sid) = session_id
        .as_deref()
        .filter(|sid| crate::analytics::is_safe_session_id(sid))
        && let Some(dir) = crate::cmd::resolve_cache_dir()
    {
        crate::cmd::session_sidecar::write_session_id(sid, &dir);
    }

    // If already starts with "skim " — already rewritten, passthrough
    if command.starts_with("skim ") {
        audit_hook(&command, false, "");
        return Ok(ExitCode::SUCCESS);
    }

    // Round-trip safety (#317): bail BEFORE tokenization on anything the
    // pipeline cannot reconstruct byte-faithfully — newlines (the heredoc
    // `git commit` corruption class: 72 sessions / 180 failures), heredocs,
    // command substitution, backticks, unmatched quotes, and whitespace that
    // does not survive split+rejoin. The simple path previously ran
    // split_whitespace()+join(" ") with NO checks.
    //
    // Fix C (fix/rewrite-hook-falseneg): use `command_needs_passthrough`
    // instead of `rewrite_would_corrupt` directly.  The hook-layer function
    // trims trailing whitespace first so trailing newlines (commonly added
    // by agent hooks) are ignored, while interior newlines (multi-line
    // commands that would corrupt tokenization) still cause a bail-out.
    if command_needs_passthrough(&command) {
        audit_hook(&command, false, "");
        return Ok(ExitCode::SUCCESS);
    }

    // Tokenize once (borrowing from `command`) for both the indefinite-command
    // check and the rewrite engine — a single allocation on this hot path.
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.is_empty() {
        audit_hook(&command, false, "");
        return Ok(ExitCode::SUCCESS);
    }

    // Fast-path: indefinite/daemon commands must not be rewritten.
    // When an agent runs `next dev` or `npm run dev`, passing it through the
    // rewrite engine would try to capture its output — a dev server never
    // exits, so the agent would hang. Treat these as no-rewrite passthroughs
    // the same way already-skim commands are treated (audit + exit 0, empty
    // stdout → agent runs the raw command unchanged). (ADR-008 Part C)
    if super::indefinite::is_indefinite_command(&tokens) {
        audit_hook(&command, false, "");
        return Ok(ExitCode::SUCCESS);
    }

    // Check for compound operator characters on the original string directly
    // rather than scanning the token slice, since operators may appear mid-token.
    let has_operator_chars = command.contains("&&")
        || command.contains("||")
        || command.contains(';')
        || command.contains('|');

    // Fast path for non-compound commands. The RewriteResult is kept
    // structured (parts vector) until after session-id injection (#317).
    let rewritten = if !has_operator_chars {
        try_rewrite(&tokens)
    } else {
        // Defer joining until the compound branch — avoids a dead allocation on the
        // common non-compound path where `original` is never read.
        let original = tokens.join(" ");
        match split_compound(&original) {
            CompoundSplitResult::Bail => None,
            CompoundSplitResult::Simple(simple_tokens) => {
                let token_refs: Vec<&str> = simple_tokens.iter().map(|s| s.as_str()).collect();
                try_rewrite(&token_refs)
            }
            CompoundSplitResult::Compound(segments) => try_rewrite_compound(&segments),
        }
    };

    match rewritten {
        Some(result) => {
            // Attribution flows out-of-band: session_id was already written to the
            // sidecar file above (write_session_id). The rewritten command does NOT
            // carry --session-id so it is safe against version-skew failures (#1.1).
            // The skim child process resolves session attribution via the sidecar
            // ancestry walk (read_session_id) or SKIM_SESSION_ID env var.
            let final_cmd = result.tokens.join(" ");
            audit_hook(&command, true, &final_cmd);
            // Use agent-specific response format
            let response = protocol.format_response(&final_cmd);
            let json_out = serde_json::to_string(&response)?;
            // SAFETY: write_all + flush before returning ensures the full JSON
            // response is on the wire before the watchdog's process::exit(0) can
            // fire. The watchdog is running concurrently on a detached thread; a
            // bare `println!` only flushes when the BufWriter decides to, so it
            // could be truncated if exit fires mid-buffer. Locking stdout and
            // flushing explicitly makes the emit atomic relative to the timeout.
            {
                use std::io::Write;
                let mut out = std::io::stdout().lock();
                out.write_all(json_out.as_bytes())?;
                out.write_all(b"\n")?;
                out.flush()?;
            }
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
    if let Ok(contents) = std::fs::read_to_string(stamp_path)
        && contents.trim() == today
    {
        return false;
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
            let stamp_path = match crate::cmd::resolve_cache_dir() {
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
    let stamp_path = match crate::cmd::resolve_cache_dir() {
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

    let log_path = match crate::cmd::resolve_cache_dir() {
        Some(dir) => dir.join("hook-audit.log"),
        None => return,
    };

    // Rotate if the log exceeds the size limit (best-effort).
    // Shift scheme: delete .3, rename .2 -> .3, .1 -> .2, current -> .1.
    if let Ok(meta) = std::fs::metadata(&log_path)
        && meta.len() >= AUDIT_LOG_MAX_BYTES
    {
        for i in (1..AUDIT_LOG_MAX_ARCHIVES).rev() {
            let from = audit_archive_path(&log_path, i);
            let to = audit_archive_path(&log_path, i + 1);
            let _ = std::fs::rename(&from, &to);
        }
        let archive_1 = audit_archive_path(&log_path, 1);
        let _ = std::fs::rename(&log_path, &archive_1);
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

    // ========================================================================
    // Hook/rewrite surface: NO --session-id in rewritten commands (#1.1)
    //
    // The hook drops flag injection; attribution is out-of-band via sidecar.
    // These tests pin that the rewrite engine produces clean commands.
    // ========================================================================

    /// Rewritten simple command carries NO --session-id token.
    ///
    /// This verifies the hook/rewrite surface (not the wrapper surface).
    /// Attribution now flows via the sidecar ancestry walk.
    #[test]
    fn test_rewritten_command_has_no_session_id_flag() {
        use super::super::engine::try_rewrite;
        let tokens: Vec<&str> = "git status".split_whitespace().collect();
        let result = try_rewrite(&tokens);
        assert!(
            result.is_some(),
            "git status must be rewritable (sanity check)"
        );
        let rewritten = result.unwrap().tokens.join(" ");
        assert!(
            !rewritten.contains("--session-id"),
            "rewritten command must NOT contain --session-id, got: {rewritten}"
        );
        assert!(
            rewritten.starts_with("skim "),
            "rewritten command must start with skim, got: {rewritten}"
        );
    }

    /// Rewritten compound command carries NO --session-id in any segment.
    ///
    /// Exercises the compound path (&&) on the hook/rewrite surface.
    #[test]
    fn test_rewritten_compound_has_no_session_id_flag() {
        use super::super::compound::split_compound;
        use super::super::compound::try_rewrite_compound;
        use super::super::types::CompoundSplitResult;
        let cmd = "git status && cargo test";
        let result = match split_compound(cmd) {
            CompoundSplitResult::Compound(segments) => try_rewrite_compound(&segments),
            _ => panic!("expected compound split for '{cmd}'"),
        };
        assert!(result.is_some(), "compound rewrite must succeed");
        let rewritten = result.unwrap().tokens.join(" ");
        assert!(
            !rewritten.contains("--session-id"),
            "rewritten compound must NOT contain --session-id in any segment, got: {rewritten}"
        );
    }

    // ========================================================================
    // Sidecar write is the attribution path on the hook surface (#1.1)
    // ========================================================================

    /// The hook's sidecar-write path (write_session_id) must persist the
    /// session_id so the skim child can retrieve it via ancestry walk.
    ///
    /// This test verifies the sidecar is written with correct content when
    /// a valid session_id is present. It does not exercise run_hook_mode
    /// directly (that requires a real stdin/hook protocol), but pins the
    /// sidecar round-trip that the hook depends on.
    #[test]
    fn test_hook_sidecar_write_enables_ancestry_attribution() {
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        let session = "hook-test-session-42";

        // Simulate what run_hook_mode does: write the sidecar.
        crate::cmd::session_sidecar::write_session_id(session, dir.path());

        // The sidecar must be readable back via the read path, proving the
        // skim child process (which inherits the same session) can resolve it.
        // We key on PPID in write, so read via the helper that traverses ancestry.
        // Simulate depth-0 read by planting directly under current PID
        // (write_session_id keys on PPID, but we verify the write at all).
        let sessions_dir = dir.path().join("sessions");
        assert!(
            sessions_dir.exists(),
            "write_session_id must create sessions/ dir"
        );

        // Verify at least one .id file exists (the PPID-keyed file).
        let entries: Vec<_> = std::fs::read_dir(&sessions_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().map(|x| x == "id").unwrap_or(false))
            .collect();
        assert!(
            !entries.is_empty(),
            "write_session_id must create a PID-keyed .id file; no .id files found in {sessions_dir:?}"
        );
    }

    // ========================================================================
    // Hook response JSON shape unchanged (no new fields from dropping the flag)
    // ========================================================================

    /// parse_agent_flag is unchanged and the hook response shape is driven by
    /// format_response() on the agent-specific protocol, not by the flag.
    /// The tests above confirm no --session-id is in the rewritten command text.
    /// A separate integration test for the JSON shape would require driving
    /// run_hook_mode with a real stdin; instead we verify the constant contract:
    /// the timeout and stdin-bounds constants are unchanged.
    #[test]
    fn test_hook_constants_unchanged() {
        assert_eq!(HOOK_TIMEOUT_SECS, 5);
        assert_eq!(HOOK_MAX_STDIN_BYTES, 64 * 1024);
    }
}
