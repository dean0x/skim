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

// ============================================================================
// B1 — Drift detection types and pure detector
// ============================================================================

/// A specific kind of binary-provenance drift detected between the hook script
/// and the currently running binary.
///
/// See ADR-013: drift is a TRUST fact about the bytes the agent is reasoning
/// over — surfacing it in-band prevents false "current behavior" reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DriftKind {
    /// SKIM_HOOK_BINARY is absent — hook script predates binary pinning.
    ///
    /// An unpinned hook script can resolve to any `skim` on $PATH, which on
    /// this machine can be a stale or divergent-branch clone. This is the
    /// dangerous state that ADR-004 addressed.
    ///
    /// Residual gap (accepted): a hook script predating SKIM_HOOK_VERSION goes
    /// undetected in-band. `skim doctor` (separate task) covers it.
    HookScriptUnpinned,
    /// SKIM_HOOK_BINARY points to a different binary than current_exe.
    BinaryPathMismatch,
    /// SKIM_HOOK_VERSION differs from the compiled-in version string.
    HookVersionMismatch,
    /// SKIM_HOOK_COMMIT differs from the compiled-in SKIM_GIT_COMMIT.
    CommitMismatch,
}

/// All environment values needed for drift detection.
///
/// Owned; NEVER reads process env in tests — tests construct `DriftEnv`
/// directly with known values so `detect_drift` remains deterministic.
///
/// `from_process()` is the single call site that reads process env and
/// resolves the current executable path.
pub(super) struct DriftEnv {
    /// Value of SKIM_HOOK_VERSION. `None` = not running under a skim hook script.
    ///
    /// When `None`, `detect_drift` emits nothing — this keeps all existing
    /// tests that invoke the hook without SKIM_HOOK_VERSION green and unmodified.
    pub(super) hook_version: Option<String>,
    /// Value of SKIM_HOOK_BINARY. `None` = hook script predates binary pinning.
    pub(super) hook_binary: Option<String>,
    /// Value of SKIM_HOOK_COMMIT.
    pub(super) hook_commit: Option<String>,
    /// Canonicalized path of the running executable.
    pub(super) current_exe: Option<std::path::PathBuf>,
    /// Compiled-in version string (`CARGO_PKG_VERSION`).
    pub(super) compiled_version: String,
    /// Compiled-in git commit (`SKIM_GIT_COMMIT`, or `"unknown"` if unset).
    pub(super) compiled_commit: String,
}

impl DriftEnv {
    /// Read all drift-relevant values from the current process.
    ///
    /// The only call site that touches process env and filesystem for drift
    /// purposes. `detect_drift` receives this struct and is a pure function
    /// of its argument.
    pub(super) fn from_process() -> Self {
        DriftEnv {
            hook_version: std::env::var("SKIM_HOOK_VERSION")
                .ok()
                .filter(|v| !v.is_empty()),
            hook_binary: std::env::var("SKIM_HOOK_BINARY")
                .ok()
                .filter(|v| !v.is_empty()),
            hook_commit: std::env::var("SKIM_HOOK_COMMIT")
                .ok()
                .filter(|v| !v.is_empty()),
            current_exe: std::env::current_exe().and_then(std::fs::canonicalize).ok(),
            compiled_version: env!("CARGO_PKG_VERSION").to_string(),
            compiled_commit: option_env!("SKIM_GIT_COMMIT")
                .unwrap_or("unknown")
                .to_string(),
        }
    }
}

/// Pure drift detector — no env reads, no hidden state.
///
/// Returns the set of drift kinds detected given the pre-built `DriftEnv`.
/// All data needed for detection is stored in `env`; callers build it once
/// via `DriftEnv::from_process()`.
///
/// ## Scoping rule (CRITICAL)
///
/// When `env.hook_version` is `None`, returns an empty `Vec` immediately.
/// Both-unset means we are NOT running under a skim-generated hook script
/// (manual invocation, test harness, third-party wrapper). This keeps all
/// existing `cli_e2e_rewrite.rs` tests green **without modification** — any
/// test that drives the hook surface without setting `SKIM_HOOK_VERSION` sees
/// zero drift, which is correct for those invocations.
///
/// **If you find yourself editing an existing `cli_e2e_rewrite.rs` test, the
/// scoping rule is implemented incorrectly.** Stop and reconsider.
///
/// ## Inversion fixes
///
/// **Inversion 1 (SKIM_HOOK_BINARY absent):** The original code skipped the
/// binary check when SKIM_HOOK_BINARY was absent. An absent pin is actually the
/// worst case — the hook can resolve to any skim on $PATH. Fix: `hook_binary = None`
/// now emits `HookScriptUnpinned`.
///
/// **Inversion 2 (binary/commit check only on version match):** The original
/// `check_hook_version_mismatch` called `check_hook_binary_mismatch` only when
/// versions *matched*, then returned. This means on the reporting machine
/// (hook=2.10.0, binary=2.11.0) the binary/commit check never fired. Fix: all
/// checks run whenever `hook_version` is `Some`.
pub(super) fn detect_drift(env: &DriftEnv) -> Vec<DriftKind> {
    // Scoping rule: absent SKIM_HOOK_VERSION → not running under a skim hook.
    let hook_version = match &env.hook_version {
        Some(v) => v,
        None => return vec![],
    };

    let mut drifts = Vec::new();

    // Version check.
    if hook_version != &env.compiled_version {
        drifts.push(DriftKind::HookVersionMismatch);
    }

    // Binary path check — always runs when hook_version is set.
    // Inversion 1 fix: absent hook_binary = HookScriptUnpinned (was: skip check).
    match &env.hook_binary {
        None => {
            drifts.push(DriftKind::HookScriptUnpinned);
        }
        Some(hook_binary) => {
            // Canonicalize hook_binary for reliable comparison (resolves symlinks).
            // This is the only filesystem read in detect_drift; it only runs when
            // hook_binary is Some AND hook_version is Some.
            let hook_canon = std::fs::canonicalize(hook_binary)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| hook_binary.clone());

            if let Some(current_exe) = &env.current_exe {
                let current_str = current_exe.display().to_string();
                if !current_str.is_empty() && current_str != hook_canon {
                    drifts.push(DriftKind::BinaryPathMismatch);
                }
            }
        }
    }

    // Commit check — always runs when hook_version is set.
    // Inversion 2 fix: was only called when versions MATCHED, now always runs.
    if let Some(hook_commit) = &env.hook_commit
        && env.compiled_commit != "unknown"
        && hook_commit != &env.compiled_commit
    {
        drifts.push(DriftKind::CommitMismatch);
    }

    drifts
}

// ============================================================================
// B3 — Session-keyed dedup stamp helpers
// ============================================================================

/// Compute the drift stamp key for this binary state.
///
/// Format: `{version}-{commit}-{hash(canonical_exe)[..12]}`
///
/// The exe hash is what re-arms the advisory when two builds share the same
/// version string but live at different paths — the exact scenario that caused
/// two false "current behavior" reports in one session (ADR-013).
fn drift_stamp_key(env: &DriftEnv) -> String {
    let exe_hash = env
        .current_exe
        .as_deref()
        .map(hash_path_12)
        .unwrap_or_else(|| "000000000000".to_string());
    format!(
        "{}-{}-{}",
        env.compiled_version, env.compiled_commit, exe_hash
    )
}

/// Compute a 12-character hex FNV-1a hash of a path string.
///
/// Used to distinguish builds that share a version string but live at different
/// paths, without storing the full path in the stamp file.
fn hash_path_12(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    let mut h: u64 = 0xcbf29ce484222325; // FNV offset basis
    for byte in s.bytes() {
        h ^= u64::from(byte);
        h = h.wrapping_mul(0x100000001b3); // FNV prime
    }
    let hex = format!("{h:016x}");
    // SAFETY: hex is always 16 ASCII chars; slicing at byte 12 is safe.
    hex[..12].to_string()
}

/// Resolve the stamp path for the current session/day.
///
/// Session-keyed when `session_id` is set and passes `is_safe_session_id`.
/// Falls back to a daily stamp when no session is available — this makes
/// emit-every-call structurally impossible.
///
/// # Security
///
/// `session_id` is untrusted input. The `is_safe_session_id` guard is the
/// trust boundary; only validated session IDs reach `format!("{sid}.stamp")`.
/// We never use a bare `.join()` on unvalidated input to prevent path traversal.
fn drift_stamp_path(
    cache_dir: &std::path::Path,
    session_id: Option<&str>,
    today: &str,
) -> std::path::PathBuf {
    let dir = cache_dir.join("hook-drift");
    let filename = if let Some(sid) = session_id.filter(|s| crate::analytics::is_safe_session_id(s))
    {
        // Session-keyed: one advisory per session per drift state.
        // B3: reject warn_once_daily for this channel — a new session on the same
        // day MUST re-arm the advisory (daily-per-key misses new sessions, which
        // is precisely the observed failure: two sessions, two false measurements).
        format!("{sid}.stamp")
    } else {
        // Fallback: daily stamp when no session_id is available.
        format!("daily-{today}.stamp")
    };
    dir.join(filename)
}

/// Check whether the drift advisory should fire for this stamp path/key pair.
///
/// Returns `true` (and writes the stamp) if the advisory should fire.
/// Returns `false` if the stamp already exists with the same key.
///
/// # Idempotency
///
/// The stamp key encodes `{version}-{commit}-{exe_hash}`. If the binary is
/// rebuilt or replaced, the key changes and the advisory re-arms.
fn write_drift_stamp(stamp_path: &std::path::Path, key: &str) -> bool {
    if let Ok(existing) = std::fs::read_to_string(stamp_path)
        && existing.trim() == key
    {
        return false; // Already emitted for this drift state in this session
    }
    let _ = std::fs::create_dir_all(stamp_path.parent().unwrap_or(std::path::Path::new(".")));
    let _ = std::fs::write(stamp_path, key);
    true
}

/// Prune drift stamps older than 7 days; if still over 64, trim oldest to 32.
///
/// Called post-flush (best-effort). The 5 s watchdog may call `process::exit(0)`
/// after flush; pruning is post-flush so it never delays the hook response.
///
/// All loops are bounded by the directory entry count.
fn prune_drift_stamps(dir: &std::path::Path) {
    use std::time::{Duration, SystemTime};

    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };

    // Collect entries (bounded: at most N entries in the directory).
    let entries: Vec<_> = read_dir
        .flatten()
        .filter(|e| e.path().extension().map(|x| x == "stamp").unwrap_or(false))
        .collect();

    let cutoff = SystemTime::now()
        .checked_sub(Duration::from_secs(7 * 24 * 3600))
        .unwrap_or(SystemTime::UNIX_EPOCH);

    let mut survivors: Vec<(std::time::SystemTime, std::path::PathBuf)> = Vec::new();
    // Bounded: at most entries.len() iterations.
    for entry in &entries {
        let path = entry.path();
        if let Ok(meta) = std::fs::metadata(&path) {
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if modified < cutoff {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            survivors.push((modified, path));
        }
    }

    // If still over 64, trim oldest to 32 (bounded: survivors.len() iterations).
    if survivors.len() > 64 {
        survivors.sort_by_key(|(t, _)| *t); // oldest first
        let to_remove = survivors.len().saturating_sub(32);
        for (_, path) in survivors.iter().take(to_remove) {
            let _ = std::fs::remove_file(path);
        }
    }
}

// ============================================================================
// B2 — Advisory message formatting
// ============================================================================

/// Format the full advisory text for the model-facing `additionalContext` field.
///
/// # ADR-005 tension
///
/// This text names `SKIM_PASSTHROUGH=1`, which steady-state guidance deliberately
/// omits (per ADR-005 Corollary, protected by a negative test). The tension is
/// justified here because:
/// (1) this advisory fires only on detected drift, not on every invocation, and
/// (2) it scopes passthrough to a single verification read, not habitual bypass.
///
/// # Size
///
/// Target ≈1.1 KB; hard-capped at 4 000 characters against Claude Code's
/// `hookSpecificOutput` limit. The cap is implemented defensively — normal
/// advisory text is well under 4 000 characters.
fn format_drift_advisory(drifts: &[DriftKind], env: &DriftEnv) -> String {
    const HARD_CAP: usize = 4_000;

    let current_exe_str = env
        .current_exe
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(unknown)".to_string());
    let hook_version = env.hook_version.as_deref().unwrap_or("(none)");
    let hook_binary = env.hook_binary.as_deref().unwrap_or("(absent — unpinned)");
    let hook_commit = env.hook_commit.as_deref().unwrap_or("(absent)");

    let mut drift_lines = String::new();
    for kind in drifts {
        match kind {
            DriftKind::HookScriptUnpinned => {
                drift_lines.push_str(
                    "  - UNPINNED SCRIPT   SKIM_HOOK_BINARY not set; hook predates binary pinning\n",
                );
            }
            DriftKind::BinaryPathMismatch => {
                drift_lines.push_str(&format!(
                    "  - BINARY PATH       Hook installed from: {hook_binary}\n\
                     \x20                     Running from:        {current_exe_str}\n"
                ));
            }
            DriftKind::HookVersionMismatch => {
                drift_lines.push_str(&format!(
                    "  - VERSION MISMATCH  Hook v{hook_version} vs binary v{}\n",
                    env.compiled_version
                ));
            }
            DriftKind::CommitMismatch => {
                drift_lines.push_str(&format!(
                    "  - COMMIT MISMATCH   Hook commit: {hook_commit}  Binary commit: {}\n",
                    env.compiled_commit
                ));
            }
        }
    }

    let text = format!(
        "[skim provenance advisory]\n\
         \n\
         Binary provenance drift detected. The skim binary currently serving\n\
         rewrites may not match the source tree under review. Output from skim\n\
         during this session may reflect a stale or sibling-clone build's behavior.\n\
         \n\
         Drift detected:\n\
         {drift_lines}\n\
         Provenance:\n\
           Running binary:  {current_exe_str}\n\
           Binary version:  {}\n\
           Binary commit:   {}\n\
           Hook version:    {hook_version}\n\
           Hook binary pin: {hook_binary}\n\
           Hook commit pin: {hook_commit}\n\
         \n\
         ACTION REQUIRED: Distrust skim-mediated output until resolved. Any skim\n\
         read during this session may show a different build's view of the code.\n\
         \n\
         Remedies (in order):\n\
           1. Rebuild and re-pin:  cargo build -p rskim && skim init --yes\n\
           2. Verify a single read without skim (bypasses rewriting entirely):\n\
              SKIM_PASSTHROUGH=1 <your-command>\n\
           3. Full diagnosis: skim doctor\n\
         \n\
         Residual gap (accepted): a hook script predating SKIM_HOOK_VERSION goes\n\
         undetected in-band. `skim doctor` covers it (B4, separate task).\n",
        env.compiled_version, env.compiled_commit,
    );

    // Defensive cap — normal text is well under HARD_CAP.
    if text.len() > HARD_CAP {
        let mut truncated = text[..HARD_CAP.saturating_sub(40)].to_string();
        truncated.push_str("\n... [advisory truncated at 4000 chars]");
        truncated
    } else {
        text
    }
}

/// Format the one-line system message for the user-facing channel.
///
/// Capped at 200 characters. The model cannot run `cargo build` or edit
/// `~/.zshrc`; the cap ensures the message is addressed to the person who can.
fn format_drift_system_message(drifts: &[DriftKind]) -> String {
    const CAP: usize = 200;
    let kinds: Vec<&str> = drifts
        .iter()
        .map(|d| match d {
            DriftKind::HookScriptUnpinned => "unpinned",
            DriftKind::BinaryPathMismatch => "binary-mismatch",
            DriftKind::HookVersionMismatch => "version-mismatch",
            DriftKind::CommitMismatch => "commit-mismatch",
        })
        .collect();
    let msg = format!(
        "[skim] Provenance drift ({}). Distrust skim output. Fix: cargo build -p rskim && skim init --yes",
        kinds.join(", ")
    );
    if msg.len() > CAP {
        msg[..CAP].to_string()
    } else {
        msg
    }
}

// ============================================================================
// B2/B3 — Advisory construction (stamp check + format)
// ============================================================================

/// Check stamp and build advisory strings if this session hasn't seen this drift
/// state yet.
///
/// Returns `Some((advisory_text, system_message))` if the advisory should fire,
/// `None` otherwise (already emitted or drifts is empty).
///
/// ## Healthy path
///
/// When `drifts` is empty the function returns `None` immediately — no cache
/// directory is resolved, no filesystem is touched. This preserves the <10 ms
/// startup budget in the common case.
fn build_advisory(
    drifts: &[DriftKind],
    drift_env: Option<&DriftEnv>,
    session_id: Option<&str>,
) -> Option<(String, String)> {
    if drifts.is_empty() {
        return None; // Healthy path — zero filesystem access beyond DriftEnv::from_process()
    }
    let de = drift_env?;
    let cache_dir = crate::cmd::resolve_cache_dir()?;
    build_advisory_inner(drifts, de, session_id, &cache_dir)
}

/// Inner helper: build advisory using an explicit `cache_dir`.
///
/// Separated from `build_advisory` for testability — tests can supply a temp
/// dir without touching `SKIM_CACHE_DIR`.
///
/// Returns `None` immediately when `drifts` is empty (healthy path) — callers
/// can test this guard directly without routing through `build_advisory`.
fn build_advisory_inner(
    drifts: &[DriftKind],
    de: &DriftEnv,
    session_id: Option<&str>,
    cache_dir: &std::path::Path,
) -> Option<(String, String)> {
    if drifts.is_empty() {
        return None; // Healthy path — zero filesystem access
    }
    let key = drift_stamp_key(de);
    let today = today_date_string();
    let stamp = drift_stamp_path(cache_dir, session_id, &today);
    if !write_drift_stamp(&stamp, &key) {
        return None; // Already emitted for this session/day and drift state
    }
    let advisory_text = format_drift_advisory(drifts, de);
    let system_msg = format_drift_system_message(drifts);
    Some((advisory_text, system_msg))
}

// ============================================================================
// Log channel helpers (hook.log, kept for audit trail per B3)
// ============================================================================

/// Emit rate-limited log warnings for detected drifts to hook.log.
///
/// B3: The log channel continues to use `warn_once_daily` (daily rate-limit).
/// The in-band advisory channel uses session-keyed stamps (via `build_advisory`).
/// Both may fire; they are independent channels.
fn log_drift_warnings(
    drifts: &[DriftKind],
    env: &DriftEnv,
    agent_name: &str,
    cache_dir: &std::path::Path,
) {
    let hook_version = env.hook_version.as_deref().unwrap_or("?");
    let hook_binary = env.hook_binary.as_deref().unwrap_or("(absent)");
    let hook_commit = env.hook_commit.as_deref().unwrap_or("?");
    let current_str = env
        .current_exe
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    for kind in drifts {
        match kind {
            DriftKind::HookVersionMismatch => {
                warn_once_daily(
                    cache_dir,
                    "version",
                    agent_name,
                    &format!(
                        "version mismatch: hook script v{hook_version}, binary v{} \
                         (run `skim init --yes` to update)",
                        env.compiled_version
                    ),
                );
            }
            DriftKind::BinaryPathMismatch => {
                warn_once_daily(
                    cache_dir,
                    "binary",
                    agent_name,
                    &format!(
                        "binary path mismatch: hook was installed from {hook_binary}, \
                         running from {current_str} (run `skim init --yes` to update)"
                    ),
                );
            }
            DriftKind::CommitMismatch => {
                warn_once_daily(
                    cache_dir,
                    "commit",
                    agent_name,
                    &format!(
                        "binary rebuilt in-place: hook commit {hook_commit}, \
                         running commit {} (run `skim init --yes` to update)",
                        env.compiled_commit
                    ),
                );
            }
            DriftKind::HookScriptUnpinned => {
                warn_once_daily(
                    cache_dir,
                    "unpinned",
                    agent_name,
                    "hook script predates binary pinning (SKIM_HOOK_BINARY not set); \
                     run `skim init --yes` to upgrade the hook script",
                );
            }
        }
    }
}

// ============================================================================
// Response writing helper
// ============================================================================

/// Write a JSON hook response to stdout (locked, with explicit flush).
///
/// The watchdog thread may call `process::exit(0)` after the flush completes;
/// `write_all + flush` before returning ensures the full JSON is on the wire.
fn write_hook_response(response: &serde_json::Value) -> anyhow::Result<()> {
    use std::io::Write;
    let json_out = serde_json::to_string(response)?;
    let mut out = std::io::stdout().lock();
    out.write_all(json_out.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

// ============================================================================
// Parse agent flag
// ============================================================================

/// Parse the `--agent <name>` flag from rewrite args.
///
/// Returns `None` if `--agent` is not present or the value is missing.
/// Logs a warning for unknown agent names (never errors — hook mode must
/// never fail). Callers default `None` to `AgentKind::ClaudeCode`.
pub(super) fn parse_agent_flag(args: &[String]) -> Option<AgentKind> {
    let pos = args.iter().position(|a| a == "--agent")?;
    let value = args.get(pos + 1)?;
    let result = AgentKind::from_str(value);
    if result.is_none() {
        crate::cmd::hook_log::log_hook_warning(&format!(
            "unknown --agent value '{value}', falling back to claude-code"
        ));
    }
    result
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
/// 6. On no match: exit 0, empty stdout (passthrough); OR emit advisory-only
///    response if drift was detected (B2: `format_advisory_only`)
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
        // write_all + flush (see write_hook_response below) so the agent never receives
        // a truncated JSON response: either flush completes before this timer fires, or
        // the timer fires first and the agent sees empty stdout (passthrough). The
        // watchdog only fires when processing has stalled beyond the timeout window.
        std::process::exit(0);
    });

    let agent_kind = agent.unwrap_or(AgentKind::ClaudeCode);
    let protocol = protocol_for_agent(agent_kind);

    // AwarenessOnly agents (Codex) have no hook mechanism — passthrough immediately
    if protocol.hook_support() == HookSupport::AwarenessOnly {
        return Ok(ExitCode::SUCCESS);
    }

    // B1: Build DriftEnv early. DriftEnv::from_process() reads env vars and
    // resolves the current executable path. detect_drift() is a pure function
    // of its argument (no env reads, no hidden state). Both operations are cheap.
    //
    // Drift detection is currently scoped to ClaudeCode only — it has the hook
    // script infrastructure (SKIM_HOOK_BINARY, SKIM_HOOK_VERSION, SKIM_HOOK_COMMIT).
    // TODO: Extend to Cursor, Gemini, and Copilot once their hook script install
    // paths are validated.
    //
    // The log channel (warn_once_daily → hook.log) fires immediately for audit.
    // The in-band advisory channel fires after session_id is known (further below).
    let (drift_env, drifts) = if agent_kind == AgentKind::ClaudeCode {
        // #57: Integrity check — log-only (NEVER stderr, GRANITE #361 Bug 3).
        let integrity_failed = check_hook_integrity(agent_kind);

        let de = DriftEnv::from_process();
        // Only detect drift if integrity is intact; integrity failure subsumes drift.
        let ds = if !integrity_failed {
            detect_drift(&de)
        } else {
            vec![]
        };

        // Log channel: rate-limited daily warnings to hook.log (kept per B3).
        if !ds.is_empty()
            && let Some(dir) = crate::cmd::resolve_cache_dir()
        {
            log_drift_warnings(&ds, &de, agent_kind.cli_name(), &dir);
        }

        (Some(de), ds)
    } else {
        (None, vec![])
    };

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

    // B2/B3: Check if in-band advisory should fire for this session.
    //
    // build_advisory returns None immediately when drifts is empty (healthy path),
    // so the stamp directory is never created or read on the healthy path. Only
    // a positive detection (non-empty drifts) may touch the filesystem here.
    //
    // Open question (for empirical validation): Claude Code's published schema does
    // not show an example of `additionalContext` returned together with `updatedInput`
    // in the same response. If validation shows Claude Code drops one when both are
    // present, the fallback is to suppress the rewrite for the one drifted call
    // (emit advisory-only via `format_advisory_only`) and resume rewriting on the
    // next call. That change is localized to the match arm below — do not build
    // the fallback now, but do not paint us into a corner either.
    let advisory = build_advisory(&drifts, drift_env.as_ref(), session_id.as_deref());

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
            let mut response = protocol.format_response(&final_cmd);
            // B2: Attach drift advisory to the response if armed (non-blocking,
            // exit 0 preserved, no permissionDecision added — ADR-006 intact).
            if let Some((advisory_text, system_msg)) = &advisory {
                protocol.attach_advisory(&mut response, advisory_text, system_msg);
            }
            write_hook_response(&response)?;
        }
        None => {
            audit_hook(&command, false, "");
            // B2: format_advisory_only is essential, not optional.
            //
            // A no-match currently exits 0 with empty stdout. The FIRST Bash call
            // of a session frequently does not match a rewrite rule (e.g. a bare
            // `ls` that is already a wrapper, or a one-off diagnostic command).
            // Without format_advisory_only, the advisory is swallowed for exactly
            // the call where the agent most needs it — before any skim output has
            // been trusted.
            if let Some((advisory_text, system_msg)) = &advisory
                && let Some(advisory_response) =
                    protocol.format_advisory_only(advisory_text, system_msg)
            {
                write_hook_response(&advisory_response)?;
            }
        }
    }

    // B3: Post-flush stamp pruning (best-effort).
    // Called after write_hook_response so the response is on the wire before we
    // do any extra work. The 5 s watchdog may call process::exit(0) after flush;
    // pruning is intentionally post-flush so it never delays the agent's response.
    // Only runs when drift was detected (drifts non-empty) to avoid any filesystem
    // access on the healthy path.
    if !drifts.is_empty()
        && let Some(dir) = crate::cmd::resolve_cache_dir()
    {
        prune_drift_stamps(&dir.join("hook-drift"));
    }

    Ok(ExitCode::SUCCESS)
}

/// Resolve the hook config directory for the given agent.
///
/// Delegates to `DetectionEnv::resolve()` — the single authoritative
/// config-dir resolver — so per-agent env overrides (`CURSOR_CONFIG_DIR`,
/// `GEMINI_CONFIG_DIR`, `COPILOT_CONFIG_DIR`, …) are honored in hook mode
/// just as they are during install/uninstall.
///
/// Routes the resolved dir through `HookProtocol::hook_config_dir` so agents
/// like Copilot CLI that store hook artifacts under a different root
/// (e.g. `~/.copilot/`) are handled correctly.
fn resolve_hook_config_dir(agent: AgentKind) -> Option<std::path::PathBuf> {
    let env = crate::cmd::init::DetectionEnv::from_process();
    let config_dir = env.resolve(agent, false).ok()?;
    let protocol = crate::cmd::hooks::protocol_for_agent(agent);
    let has_override = env.override_for(agent).is_some();
    Some(protocol.hook_config_dir(&config_dir, false, has_override))
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

/// Emit a rate-limited warning to hook.log at most once per day per agent per kind.
///
/// `kind` is the stamp discriminator (e.g. "version", "binary", "commit") — it
/// determines the stamp filename `.hook-{kind}-warned-{agent_name}` so that each
/// warning category fires independently once per day.
///
/// All output goes to hook.log via `log_hook_warning` — NEVER to stderr
/// (zero-stderr invariant in hook mode, #361 Bug 3).
pub(super) fn warn_once_daily(
    cache_dir: &std::path::Path,
    kind: &str,
    agent_name: &str,
    msg: &str,
) {
    let stamp = cache_dir.join(format!(".hook-{kind}-warned-{agent_name}"));
    if should_warn_today(&stamp) {
        crate::cmd::hook_log::log_hook_warning(msg);
    }
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
    use crate::cmd::hooks::HookProtocol;

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
    // warn_once_daily — kind-stamped rate-limit helper
    // ========================================================================

    #[test]
    fn test_warn_once_daily_creates_kind_stamped_file() {
        let dir = tempfile::TempDir::new().unwrap();
        // First call: stamp must be created with the correct kind-qualified name.
        warn_once_daily(dir.path(), "version", "claude-code", "test message");
        let stamp = dir.path().join(".hook-version-warned-claude-code");
        assert!(
            stamp.exists(),
            "warn_once_daily must create .hook-{{kind}}-warned-{{agent}} stamp"
        );
        // Second call same day: stamp already written for today, so suppressed.
        assert!(
            !should_warn_today(&stamp),
            "stamp must suppress a repeat warn_once_daily call on the same day"
        );
    }

    #[test]
    fn test_warn_once_daily_distinct_kinds_use_distinct_stamps() {
        let dir = tempfile::TempDir::new().unwrap();
        warn_once_daily(dir.path(), "binary", "claude-code", "binary msg");
        warn_once_daily(dir.path(), "commit", "claude-code", "commit msg");
        // Each kind produces its own stamp file — they must be independent.
        assert!(
            dir.path().join(".hook-binary-warned-claude-code").exists(),
            "binary kind must have its own stamp"
        );
        assert!(
            dir.path().join(".hook-commit-warned-claude-code").exists(),
            "commit kind must have its own stamp"
        );
    }

    // ========================================================================
    // B1 — detect_drift: scoping rule and inversion fixes
    // ========================================================================

    /// Helper: build a DriftEnv with all fields matching (healthy state).
    fn healthy_drift_env() -> DriftEnv {
        DriftEnv {
            hook_version: Some("2.11.0".to_string()),
            hook_binary: Some("/usr/local/bin/skim".to_string()),
            hook_commit: Some("abc1234f".to_string()),
            current_exe: Some(std::path::PathBuf::from("/usr/local/bin/skim")),
            compiled_version: "2.11.0".to_string(),
            compiled_commit: "abc1234f".to_string(),
        }
    }

    /// Scoping rule: when hook_version is None, detect_drift returns empty.
    ///
    /// This is the critical guard that keeps all existing cli_e2e_rewrite.rs
    /// tests green without modification — tests that drive the hook surface
    /// without setting SKIM_HOOK_VERSION are not running under a skim hook
    /// script, so no drift is possible.
    #[test]
    fn test_detect_drift_no_hook_version_emits_nothing() {
        let env = DriftEnv {
            hook_version: None, // not running under a skim hook
            hook_binary: Some("/wrong/path".to_string()),
            hook_commit: Some("deadbeef".to_string()),
            current_exe: Some(std::path::PathBuf::from("/right/path")),
            compiled_version: "2.11.0".to_string(),
            compiled_commit: "abc12345".to_string(),
        };
        let drifts = detect_drift(&env);
        assert!(
            drifts.is_empty(),
            "hook_version=None must yield no drifts: {drifts:?}"
        );
    }

    /// Healthy env: all values match → no drift.
    #[test]
    fn test_detect_drift_healthy_no_drift() {
        // For this test we use a path that exists on disk so canonicalize succeeds.
        let current_exe = std::env::current_exe().unwrap();
        let env = DriftEnv {
            hook_version: Some("2.11.0".to_string()),
            hook_binary: Some(current_exe.display().to_string()),
            hook_commit: Some("abc1234f".to_string()),
            current_exe: Some(current_exe),
            compiled_version: "2.11.0".to_string(),
            compiled_commit: "abc1234f".to_string(),
        };
        let drifts = detect_drift(&env);
        assert!(
            drifts.is_empty(),
            "all-matching env must yield no drifts: {drifts:?}"
        );
    }

    /// Inversion 1 fix: absent SKIM_HOOK_BINARY → HookScriptUnpinned (was: skip check).
    ///
    /// An unpinned hook script resolves to whatever `skim` is first on $PATH,
    /// which on this machine can be a stale or divergent-branch clone.
    #[test]
    fn test_detect_drift_hook_script_unpinned_inversion1_fixed() {
        let env = DriftEnv {
            hook_version: Some("2.11.0".to_string()),
            hook_binary: None, // absent = unpinned — the dangerous state
            hook_commit: None,
            current_exe: Some(std::path::PathBuf::from("/usr/local/bin/skim")),
            compiled_version: "2.11.0".to_string(),
            compiled_commit: "abc1234f".to_string(),
        };
        let drifts = detect_drift(&env);
        assert!(
            drifts.contains(&DriftKind::HookScriptUnpinned),
            "absent hook_binary must emit HookScriptUnpinned (inversion 1 fix): {drifts:?}"
        );
    }

    /// Version mismatch → HookVersionMismatch.
    #[test]
    fn test_detect_drift_version_mismatch() {
        let env = DriftEnv {
            hook_version: Some("2.10.0".to_string()), // older hook
            hook_binary: None,                        // also unpinned — both fire
            hook_commit: None,
            current_exe: Some(std::path::PathBuf::from("/usr/local/bin/skim")),
            compiled_version: "2.11.0".to_string(),
            compiled_commit: "abc1234f".to_string(),
        };
        let drifts = detect_drift(&env);
        assert!(
            drifts.contains(&DriftKind::HookVersionMismatch),
            "version mismatch must emit HookVersionMismatch: {drifts:?}"
        );
    }

    /// Binary path mismatch → BinaryPathMismatch.
    ///
    /// Uses a path that definitely does not exist so canonicalize falls back to
    /// the raw string, making the comparison deterministic.
    #[test]
    fn test_detect_drift_binary_path_mismatch() {
        let env = DriftEnv {
            hook_version: Some("2.11.0".to_string()),
            hook_binary: Some("/tmp/skim-other-binary-nonexistent".to_string()),
            hook_commit: Some("abc1234f".to_string()),
            current_exe: Some(std::path::PathBuf::from("/usr/local/bin/skim")),
            compiled_version: "2.11.0".to_string(),
            compiled_commit: "abc1234f".to_string(),
        };
        let drifts = detect_drift(&env);
        assert!(
            drifts.contains(&DriftKind::BinaryPathMismatch),
            "path mismatch must emit BinaryPathMismatch: {drifts:?}"
        );
    }

    /// Commit mismatch → CommitMismatch.
    #[test]
    fn test_detect_drift_commit_mismatch() {
        let current_exe = std::env::current_exe().unwrap();
        let env = DriftEnv {
            hook_version: Some("2.11.0".to_string()),
            hook_binary: Some(current_exe.display().to_string()),
            hook_commit: Some("oldcommit".to_string()),
            current_exe: Some(current_exe),
            compiled_version: "2.11.0".to_string(),
            compiled_commit: "newcommit".to_string(),
        };
        let drifts = detect_drift(&env);
        assert!(
            drifts.contains(&DriftKind::CommitMismatch),
            "commit mismatch must emit CommitMismatch: {drifts:?}"
        );
    }

    /// Inversion 2 fix: commit check runs even when versions DIFFER.
    ///
    /// The original code called check_hook_binary_mismatch only when versions
    /// *matched*, then returned — so on the reporting machine (hook=2.10.0,
    /// binary=2.11.0) the commit check never ran. This test proves the fix:
    /// CommitMismatch is detected even when HookVersionMismatch is also present.
    #[test]
    fn test_detect_drift_commit_check_runs_regardless_of_version_inversion2_fixed() {
        let current_exe = std::env::current_exe().unwrap();
        let env = DriftEnv {
            hook_version: Some("2.10.0".to_string()), // version DIFFERS
            hook_binary: Some(current_exe.display().to_string()),
            hook_commit: Some("old_commit_sha".to_string()),
            current_exe: Some(current_exe),
            compiled_version: "2.11.0".to_string(),
            compiled_commit: "new_commit_sha".to_string(),
        };
        let drifts = detect_drift(&env);
        // Both version AND commit drift must be detected even though versions differ.
        assert!(
            drifts.contains(&DriftKind::HookVersionMismatch),
            "version mismatch must be detected: {drifts:?}"
        );
        assert!(
            drifts.contains(&DriftKind::CommitMismatch),
            "commit mismatch must ALSO be detected when versions differ (inversion 2 fix): {drifts:?}"
        );
    }

    // ========================================================================
    // B3 — Stamp helpers
    // ========================================================================

    /// hash_path_12 produces a 12-character hex string.
    #[test]
    fn test_hash_path_12_length() {
        let h = hash_path_12(std::path::Path::new("/usr/local/bin/skim"));
        assert_eq!(h.len(), 12, "hash must be exactly 12 hex chars: {h}");
        assert!(
            h.chars().all(|c| c.is_ascii_hexdigit()),
            "hash must be hex: {h}"
        );
    }

    /// hash_path_12 is deterministic.
    #[test]
    fn test_hash_path_12_deterministic() {
        let a = hash_path_12(std::path::Path::new("/usr/local/bin/skim"));
        let b = hash_path_12(std::path::Path::new("/usr/local/bin/skim"));
        assert_eq!(a, b);
    }

    /// hash_path_12 distinguishes different paths.
    #[test]
    fn test_hash_path_12_distinct_paths() {
        let a = hash_path_12(std::path::Path::new("/usr/local/bin/skim"));
        let b = hash_path_12(std::path::Path::new("/home/user/.cargo/bin/skim"));
        assert_ne!(a, b, "different paths must produce different hashes");
    }

    /// Healthy path: detect_drift returns empty → build_advisory returns None
    /// → stamp directory is NOT created.
    ///
    /// B3: "The healthy path must touch the filesystem ZERO times."
    /// This test proves that when drifts = [] (the common production case),
    /// the hook-drift/ stamp directory is never created.
    #[test]
    fn test_no_stamp_dir_on_healthy_path() {
        let temp = tempfile::TempDir::new().unwrap();
        let stamp_dir = temp.path().join("hook-drift");

        // Healthy env: all fields match.
        let env = DriftEnv {
            hook_version: Some("2.11.0".to_string()),
            hook_binary: Some("/usr/local/bin/skim".to_string()),
            hook_commit: Some("abc1234f".to_string()),
            current_exe: Some(std::path::PathBuf::from("/usr/local/bin/skim")),
            compiled_version: "2.11.0".to_string(),
            compiled_commit: "abc1234f".to_string(),
        };

        // Empty drifts → build_advisory_inner is never called → no stamp dir.
        let drifts: Vec<DriftKind> = vec![];
        let result = build_advisory_inner(&drifts, &env, None, temp.path());
        assert!(
            result.is_none(),
            "empty drifts must yield no advisory: {result:?}"
        );

        assert!(
            !stamp_dir.exists(),
            "hook-drift stamp directory must NOT be created on the healthy path: {stamp_dir:?}"
        );
    }

    /// Drift detected → stamp directory IS created.
    #[test]
    fn test_stamp_dir_created_on_drift() {
        let temp = tempfile::TempDir::new().unwrap();
        let stamp_dir = temp.path().join("hook-drift");

        let env = DriftEnv {
            hook_version: Some("2.10.0".to_string()),
            hook_binary: None,
            hook_commit: None,
            current_exe: Some(std::path::PathBuf::from("/usr/local/bin/skim")),
            compiled_version: "2.11.0".to_string(),
            compiled_commit: "abc1234f".to_string(),
        };
        let drifts = detect_drift(&env);
        assert!(!drifts.is_empty());

        let result = build_advisory_inner(&drifts, &env, None, temp.path());
        assert!(
            result.is_some(),
            "drift must produce an advisory on first call"
        );
        assert!(stamp_dir.exists(), "stamp dir must be created on drift");
    }

    /// Advisory is deduped: second call with same key returns None.
    #[test]
    fn test_advisory_deduped_same_session() {
        let temp = tempfile::TempDir::new().unwrap();

        let env = DriftEnv {
            hook_version: Some("2.10.0".to_string()),
            hook_binary: None,
            hook_commit: None,
            current_exe: Some(std::path::PathBuf::from("/usr/local/bin/skim")),
            compiled_version: "2.11.0".to_string(),
            compiled_commit: "abc1234f".to_string(),
        };
        let drifts = detect_drift(&env);

        // First call: advisory fires.
        let first = build_advisory_inner(&drifts, &env, Some("test-session-123"), temp.path());
        assert!(first.is_some(), "first call must produce advisory");

        // Second call same session: suppressed.
        let second = build_advisory_inner(&drifts, &env, Some("test-session-123"), temp.path());
        assert!(
            second.is_none(),
            "second call with same session must be suppressed"
        );
    }

    /// Advisory re-arms for a different session.
    #[test]
    fn test_advisory_rearmed_for_different_session() {
        let temp = tempfile::TempDir::new().unwrap();

        let env = DriftEnv {
            hook_version: Some("2.10.0".to_string()),
            hook_binary: None,
            hook_commit: None,
            current_exe: Some(std::path::PathBuf::from("/usr/local/bin/skim")),
            compiled_version: "2.11.0".to_string(),
            compiled_commit: "abc1234f".to_string(),
        };
        let drifts = detect_drift(&env);

        let first = build_advisory_inner(&drifts, &env, Some("session-aaa"), temp.path());
        assert!(first.is_some());

        let second = build_advisory_inner(&drifts, &env, Some("session-bbb"), temp.path());
        assert!(
            second.is_some(),
            "different session must re-arm the advisory"
        );
    }

    // ========================================================================
    // B2 — Advisory message format and size
    // ========================================================================

    #[test]
    fn test_advisory_text_under_4000_chars() {
        let env = healthy_drift_env();
        let drifts = vec![
            DriftKind::HookVersionMismatch,
            DriftKind::BinaryPathMismatch,
            DriftKind::CommitMismatch,
            DriftKind::HookScriptUnpinned,
        ];
        let text = format_drift_advisory(&drifts, &env);
        assert!(
            text.len() <= 4_000,
            "advisory text must be ≤ 4000 chars, got {}",
            text.len()
        );
    }

    #[test]
    fn test_system_message_under_200_chars() {
        let drifts = vec![
            DriftKind::HookVersionMismatch,
            DriftKind::BinaryPathMismatch,
            DriftKind::CommitMismatch,
            DriftKind::HookScriptUnpinned,
        ];
        let msg = format_drift_system_message(&drifts);
        assert!(
            msg.len() <= 200,
            "system message must be ≤ 200 chars, got {}",
            msg.len()
        );
    }

    #[test]
    fn test_advisory_text_names_passthrough() {
        // ADR-005 tension: advisory deliberately names SKIM_PASSTHROUGH=1.
        let env = healthy_drift_env();
        let drifts = vec![DriftKind::HookVersionMismatch];
        let text = format_drift_advisory(&drifts, &env);
        assert!(
            text.contains("SKIM_PASSTHROUGH"),
            "advisory must name SKIM_PASSTHROUGH as a verification escape hatch"
        );
    }

    #[test]
    fn test_advisory_text_contains_remedies() {
        let env = healthy_drift_env();
        let drifts = vec![DriftKind::HookScriptUnpinned];
        let text = format_drift_advisory(&drifts, &env);
        assert!(
            text.contains("cargo build") && text.contains("skim init"),
            "advisory must name the rebuild+re-pin remedy: {text}"
        );
        assert!(
            text.contains("skim doctor"),
            "advisory must reference skim doctor: {text}"
        );
    }

    // ========================================================================
    // B6 — Wrapper surface: no in-band provenance advisory
    //
    // The hook-response channel exists ONLY on the rewrite surface. On the
    // PATH-wrapper surface (argv[0] dispatch via detect_argv0_dispatch) the only
    // channels are stdout (must stay byte-faithful, #317) and stderr (ADR-011
    // would class a provenance banner as a no-loss raw-fallback banner →
    // SKIM_DEBUG-gated → invisible by default, reproducing the exact failure
    // mode this task fixes).
    //
    // Wrappers therefore get NO in-band advisory. This test proves the property
    // at the protocol boundary: non-Claude protocols (which map structurally to
    // the non-hook dispatch paths) return no-op responses from attach_advisory
    // and format_advisory_only.
    // ========================================================================

    #[test]
    fn wrapper_surface_emits_no_provenance_advisory() {
        // CopilotCliHook represents a non-Claude protocol — its default-trait
        // implementation must be a no-op for both advisory methods.
        use crate::cmd::hooks::copilot::CopilotCliHook;
        let hook = CopilotCliHook;
        let advisory_text = "drift detected";
        let system_msg = "drift system message";

        // format_advisory_only must return None (no response JSON).
        assert!(
            hook.format_advisory_only(advisory_text, system_msg)
                .is_none(),
            "Non-Claude protocol: format_advisory_only must be None \
             (wrapper surface has no in-band advisory channel)"
        );

        // attach_advisory must be a no-op — response unchanged.
        let mut response = serde_json::json!({"test": "value"});
        let original = response.clone();
        hook.attach_advisory(&mut response, advisory_text, system_msg);
        assert_eq!(
            response, original,
            "Non-Claude protocol: attach_advisory must be a no-op \
             (wrapper surface has no in-band advisory channel)"
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
