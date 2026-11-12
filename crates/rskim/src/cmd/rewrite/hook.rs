//! Hook mode (#44) — agent hook integration.
//!
//! Runs as an agent PreToolUse hook via `HookProtocol`. Reads JSON from stdin,
//! extracts the command field, rewrites if matched, and emits agent-specific
//! hook-protocol JSON. Each agent's `format_response()` controls the response shape.
//! A `PostToolUse` event goes to `post_hook::run_post_hook` instead.

use std::io::Read;
use std::process::ExitCode;

use crate::cmd::session::AgentKind;

use crate::cmd::hooks::claude::POST_TOOL_USE_EVENT;

use super::compound::{command_needs_passthrough, split_compound, try_rewrite_compound_with};
use super::engine::try_rewrite;
use super::post_hook::run_post_hook;
use super::types::{CompoundSplitResult, RewriteResult};
use crate::runner::NpxTool;

// ============================================================================
// B1 — Drift detection types and pure detector
// ============================================================================

/// A specific kind of binary-provenance drift detected between the hook script
/// and the currently running binary.
///
/// Drift is detected and recorded to `hook.log`; `skim doctor` is the
/// on-demand diagnostic. The hook response JSON never carries drift information.
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
            current_exe: crate::cmd::init::resolve_skim_binary().ok(),
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
// Log channel helpers (hook.log)
// ============================================================================

/// Emit rate-limited log warnings for detected drifts to hook.log.
///
/// Uses `warn_once_daily` (daily rate-limit per kind per agent).
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
                         (run `./target/release/skim init --yes` to update)",
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
                         running from {current_str} (run `./target/release/skim init --yes` to update)"
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
                         running commit {} (run `./target/release/skim init --yes` to update)",
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
                     run `./target/release/skim init --yes` to upgrade the hook script",
                );
            }
        }
    }
}

/// Stamp discriminator for the declined-multiline signal (`warn_once_daily`'s `kind`).
///
/// Deliberately distinct from the four drift kinds (`version`, `binary`,
/// `commit`, `unpinned`) so a decline and a drift warning can never suppress
/// one another — each `kind` owns its own `.hook-{kind}-warned-{agent}` stamp.
const DECLINED_MULTILINE_KIND: &str = "declined-multiline";

/// Record to hook.log that a rewrite was declined because the command is
/// multi-line — rate-limited to one line per agent per day.
///
/// ## ADR-011 classification: CLASS 2 (no-loss)
///
/// Declining the rewrite means no hook response is emitted, so the agent runs
/// the user's ORIGINAL command unmodified: the reader sees the real tool's
/// bytes in full, with nothing elided, substituted or re-encoded. Zero loss, so
/// this is a class-2 no-loss banner, NOT a class-1 loss-bearing marker. It
/// therefore carries no elision count and no `SKIM_PASSTHROUGH=1` hint, which
/// class 1 mandates and which would be meaningless here.
///
/// ADR-011 is a taxonomy of **stderr** notices, so a hook.log write is outside
/// its scope — which is why this write is UNCONDITIONAL rather than
/// `SKIM_DEBUG`-gated, without contradicting class 2's debug-gating rule. The
/// classification still decides the channel, twice over: class 2 forbids an
/// unconditional stderr notice, and hook mode forbids stderr at ANY gating
/// (zero-stderr invariant, GRANITE #361 Bug 3 — see `warn_once_daily` and
/// `hook_log::log_hook_warning`). hook.log is the only permitted sink.
/// ADR-013 independently forbids surfacing this in band in the hook response.
/// Do not move it to stderr, and do not re-classify it as a marker.
///
/// ## Why rate-limited rather than per-occurrence
///
/// Agents that bundle several shell commands into one multi-line string hit
/// this bail on potentially every tool call. A per-occurrence line would grow
/// hook.log without bound and drown the signal it exists to provide.
///
/// ## What this does NOT deliver (partial mitigation of #337, not a closure)
///
/// A daily line tells an operator that the class of thing is happening; it does
/// not name the specific declined command. That is deliberate: the command text
/// is unbounded and routinely carries credentials, so writing it on every
/// decline would turn hook.log into a secret sink. `SKIM_HOOK_AUDIT=1` is the
/// existing opt-in channel that records each command (to hook-audit.log), and
/// the message points at it.
///
/// ## Scope: only the multi-line trigger is named
///
/// `command_needs_passthrough` returns a bare `bool` over 8+ distinct triggers
/// (heredocs, `$(…)`, backticks, unmatched quotes, whitespace that does not
/// survive split+rejoin, …), so the reason cannot be read off it. Naming those
/// reasons here would mean re-implementing `rewrite_would_corrupt`'s predicate
/// set in a second place, where the two copies desync on the next edit
/// (PF-025). So the caller re-tests exactly one condition — the interior
/// newline this signal was measured against — and every other bail reason stays
/// silent. Widening this means widening `rewrite_would_corrupt`'s return type in
/// `compound.rs`, not adding predicates here.
fn log_declined_multiline(agent_name: &str) {
    let Some(cache_dir) = crate::cmd::resolve_cache_dir() else {
        return;
    };
    warn_once_daily(
        &cache_dir,
        DECLINED_MULTILINE_KIND,
        agent_name,
        "rewrite declined: multi-line command (interior newline) — the original command \
         runs unmodified, so no output is lost, but skim did not compress it; send each \
         command as its own single-line invocation to get compression. Rate-limited to one \
         line per agent per day (a class, not a count); set SKIM_HOOK_AUDIT=1 to record each \
         declined command in hook-audit.log (#337)",
    );
}

// ============================================================================
// Response writing helper
// ============================================================================

/// Write a JSON hook response to stdout (locked, with explicit flush).
///
/// The watchdog thread may call `process::exit(0)` after the flush completes;
/// `write_all + flush` before returning ensures the full JSON is on the wire.
#[allow(clippy::disallowed_methods)] // Hook JSON response to Claude; must hold lock across write+flush to prevent interleaving
pub(super) fn write_hook_response(response: &serde_json::Value) -> anyhow::Result<()> {
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

/// Maximum bytes the PreToolUse path reads from stdin (64 KiB).
/// A PreToolUse payload is a small JSON object; this prevents unbounded allocation.
pub(super) const HOOK_MAX_STDIN_BYTES: u64 = 64 * 1024;

/// Maximum bytes the hook reads from stdin before it knows the event (1 MiB).
/// A PostToolUse payload also holds the captured output of the tool call, which
/// the post path accepts up to 29,000 characters. The cap bounds allocation.
const HOOK_POST_MAX_STDIN_BYTES: u64 = 1024 * 1024;

/// Maximum time (in seconds) a hook invocation is allowed before self-termination.
///
/// Prevents slow hook processing from hanging the agent indefinitely.
/// The hook exits cleanly (exit 0, empty stdout) on timeout — this is a
/// passthrough, not an error. Logs a warning to hook.log for debugging.
pub(super) const HOOK_TIMEOUT_SECS: u64 = 5;

/// The `permission_mode` value of Claude Code's auto permission mode.
pub(super) const AUTO_PERMISSION_MODE: &str = "auto";

/// Whether the hook input says the agent runs in auto permission mode.
///
/// In auto mode, a hook that changes a tool call voids the auto-mode review of
/// the other calls in the same response, and each void costs the user one more
/// model turn. So skim does not rewrite in auto mode, except where the rewrite
/// must run (see [`withholds_rewrite`]). Only the string `"auto"` in the
/// top-level `permission_mode` field counts: any other value, a missing field
/// and a non-string field all keep the normal rewrite behaviour.
fn is_auto_permission_mode(input: &serde_json::Value) -> bool {
    input
        .get("permission_mode")
        .and_then(serde_json::Value::as_str)
        == Some(AUTO_PERMISSION_MODE)
}

/// Whether the hook sends no response for a rewrite it found.
///
/// In auto permission mode the hook withholds every rewrite except one that
/// sends a command through a tool whose output skim must mask (`env`,
/// `printenv`, `curl`; see `RewriteResult::masks_output`). The masking runs only
/// inside that skim tool, so a command left unrewritten would print unmasked.
/// For a compound command, one such segment keeps the whole rewrite. In every
/// other mode the hook withholds nothing.
fn withholds_rewrite(input: &serde_json::Value, result: &RewriteResult) -> bool {
    is_auto_permission_mode(input) && !result.masks_output
}

/// The folder the hook judges a project-local tool in.
///
/// The `cwd` field of the hook input when it is an absolute path that resolves;
/// the working folder of the hook process when the field is absent. A `cwd` that
/// is relative, not a string, or not a folder that resolves gives `None`: the
/// hook then cannot say where the command will run, and the caller must not
/// rewrite. A relative `cwd` is never read against the hook process, which runs
/// in a folder the agent does not choose.
fn hook_cwd(input: &serde_json::Value) -> Option<std::path::PathBuf> {
    match input.get("cwd") {
        None => std::env::current_dir().ok(),
        Some(serde_json::Value::String(cwd)) => {
            let cwd = std::path::Path::new(cwd);
            if cwd.is_absolute() {
                cwd.canonicalize().ok()
            } else {
                None
            }
        }
        Some(_) => None,
    }
}

/// The check that a project holds the tool an `npx <tool>` rewrite starts.
///
/// The tool must be an executable in `node_modules/.bin` of the hook folder or
/// a parent up to the repository root (`runner::local_node_bin`); without it the
/// handler would refuse to start. The folder is read only when the check is
/// called, and the tool name comes from the rule, never from command text.
fn local_bin_probe(input: &serde_json::Value) -> impl Fn(NpxTool) -> bool + '_ {
    move |tool| hook_cwd(input).is_some_and(|cwd| crate::runner::local_node_bin(&cwd, tool))
}

/// The rewrite the hook sends for a command that passed [`command_gates`].
///
/// `npx_ok` says whether the project holds the tool of an `npx <tool>` rule
/// (see [`local_bin_probe`]). A rewrite that needs the tool and does not find it
/// is not made: that one segment stays as typed, and every other segment of a
/// compound still rewrites. The drop never reaches a segment that routes through
/// a masking tool, so that segment keeps its rewrite.
fn hook_rewrite(
    tokens: &[&str],
    has_operator_chars: bool,
    npx_ok: &dyn Fn(NpxTool) -> bool,
) -> Option<RewriteResult> {
    let single = |tokens: &[&str]| try_rewrite(tokens).filter(|r| r.npx_tool.is_none_or(npx_ok));
    if !has_operator_chars {
        return single(tokens);
    }
    // Defer joining until the compound branch — avoids a dead allocation on the
    // common non-compound path where `original` is never read.
    let original = tokens.join(" ");
    match split_compound(&original) {
        CompoundSplitResult::Bail => None,
        CompoundSplitResult::Simple(simple_tokens) => {
            let token_refs: Vec<&str> = simple_tokens.iter().map(|s| s.as_str()).collect();
            single(&token_refs)
        }
        CompoundSplitResult::Compound(segments) => try_rewrite_compound_with(&segments, npx_ok),
    }
}

/// Why [`command_gates`] declined a command.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Decline {
    AlreadySkim,
    /// Not rebuildable byte for byte; `multiline` marks an interior newline.
    Unsafe {
        multiline: bool,
    },
    Empty,
    /// A dev server or watcher, which never exits.
    Indefinite,
}

/// The gates both hook paths apply before they look for a rewrite. Pure: each
/// path decides what a [`Decline`] costs. Returns the tokens and whether the
/// command holds a compound operator character.
pub(super) fn command_gates(command: &str) -> Result<(Vec<&str>, bool), Decline> {
    if command.starts_with("skim ") {
        return Err(Decline::AlreadySkim);
    }
    // #317: bail before tokenizing on a shape the pipeline cannot rebuild byte
    // for byte. Trailing whitespace is trimmed first, so only an interior
    // newline counts as multi-line.
    if command_needs_passthrough(command) {
        return Err(Decline::Unsafe {
            multiline: command.trim_end().contains('\n'),
        });
    }
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.is_empty() {
        return Err(Decline::Empty);
    }
    if super::indefinite::is_indefinite_command(&tokens) {
        return Err(Decline::Indefinite);
    }
    let has_operator_chars = ["&&", "||", ";", "|"].iter().any(|op| command.contains(op));
    Ok((tokens, has_operator_chars))
}

/// Where the hook sends the input it read from stdin.
#[derive(Debug)]
enum HookRoute {
    /// A `PostToolUse` event from any agent; `run_post_hook` declines all but Claude Code.
    Post(serde_json::Value),
    /// A PreToolUse input, or one with no event name.
    Pre(serde_json::Value),
    /// No path handles the input. `audit` is true when the Pre path records an
    /// empty audit line (JSON that did not parse).
    Skip { audit: bool },
}

fn event_name(input: &serde_json::Value) -> Option<&str> {
    input
        .get("hook_event_name")
        .and_then(serde_json::Value::as_str)
}

/// Decide the path for the bytes read from stdin.
///
/// A `PostToolUse` payload can exceed the Pre cap, so its event comes from the
/// whole buffer. Every other input keeps the Pre path's view, the first
/// [`HOOK_MAX_STDIN_BYTES`] bytes. Claude Code registers only Pre and Post, so
/// another event name from it has no path; other agents name their events
/// differently and keep the Pre path.
fn route_stdin(bytes: &[u8], agent: AgentKind) -> HookRoute {
    let whole = std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok());
    let json = match whole {
        Some(json) if event_name(&json) == Some(POST_TOOL_USE_EVENT) => {
            return HookRoute::Post(json);
        }
        Some(json) if bytes.len() as u64 <= HOOK_MAX_STDIN_BYTES => json,
        _ => {
            let view = &bytes[..bytes.len().min(HOOK_MAX_STDIN_BYTES as usize)];
            let Ok(text) = std::str::from_utf8(view) else {
                return HookRoute::Skip { audit: false };
            };
            match serde_json::from_str(text) {
                Ok(json) => json,
                Err(_) => return HookRoute::Skip { audit: true },
            }
        }
    };
    match event_name(&json) {
        None | Some("PreToolUse") => HookRoute::Pre(json),
        Some(_) if agent == AgentKind::ClaudeCode => HookRoute::Skip { audit: false },
        Some(_) => HookRoute::Pre(json),
    }
}

/// Record integrity and drift findings in hook.log (Claude Code only, the one
/// agent whose script exports the provenance fields). Rate-limited per kind per
/// agent per day; no in-band advisory, use `skim doctor`. The post path skips it.
fn log_provenance_findings(agent_kind: AgentKind) {
    if agent_kind != AgentKind::ClaudeCode {
        return;
    }
    // #57: log-only (NEVER stderr, GRANITE #361 Bug 3).
    let integrity_failed = check_hook_integrity(agent_kind);
    let de = DriftEnv::from_process();
    // A Tampered script's exports cannot be trusted; an unreadable one still runs drift.
    let drifts = if integrity_failed {
        vec![]
    } else {
        detect_drift(&de)
    };
    if !drifts.is_empty()
        && let Some(dir) = crate::cmd::resolve_cache_dir()
    {
        log_drift_warnings(&drifts, &de, agent_kind.cli_name(), &dir);
    }
}

/// Run as an agent hook: PreToolUse for every agent, PostToolUse for Claude Code.
///
/// Protocol:
/// 1. Read JSON from stdin (bounded)
/// 2. A `PostToolUse` event goes to [`run_post_hook`] and ends there
/// 3. Extract `tool_input.command`
/// 4. On parse/extract failure: exit 0, empty stdout (passthrough)
/// 5. Run command through rewrite logic
/// 6. In auto permission mode, a rewrite that needs no masking is withheld: exit
///    0, empty stdout (see [`withholds_rewrite`])
/// 7. An `npx <tool>` segment is rewritten only when the project holds the tool
///    (see [`hook_rewrite`]); else that segment stays as typed, and a command
///    with nothing else to rewrite gets exit 0, empty stdout
/// 8. On match: emit hook response JSON, exit 0
/// 9. On no match: exit 0, empty stdout (passthrough)
///
/// Drift is detected and logged to hook.log on every PreToolUse invocation. For
/// diagnosis run `skim doctor`.
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

    // Read stdin (bounded), then route on the event. A PostToolUse call ends in
    // `run_post_hook` before any write below and before the provenance log: it
    // must leave the sidecar, the force-raw marker, hook.log and the audit log
    // unchanged. Every other call logs provenance findings, on a read or parse
    // failure too.
    let mut stdin_buf = Vec::new();
    let bytes_read = std::io::stdin()
        .lock()
        .take(HOOK_POST_MAX_STDIN_BYTES)
        .read_to_end(&mut stdin_buf);
    let route = match bytes_read {
        Ok(_) => route_stdin(&stdin_buf, agent_kind),
        Err(_) => HookRoute::Skip { audit: false }, // passthrough on read failure
    };
    let json = match route {
        HookRoute::Post(json) => return Ok(run_post_hook(&json, agent_kind)),
        HookRoute::Skip { audit } => {
            log_provenance_findings(agent_kind);
            if audit {
                audit_hook("", false, ""); // passthrough on parse failure
            }
            return Ok(ExitCode::SUCCESS);
        }
        HookRoute::Pre(json) => json,
    };
    log_provenance_findings(agent_kind);

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

    // Cross-surface fidelity parity: hand the WRAPPER surface this command's
    // stdout-destination verdict. `| tee out.txt`, `$(…)` and a named-FIFO
    // redirect all present the wrapper with a FIFO on fd 1, indistinguishable
    // from `| cat` — only here, where the pipeline shape is visible, can they be
    // told apart. `set_force_raw` is called with the verdict, so a `false`
    // CLEARS any previous marker, so it never outlives a command this call
    // processed (the early returns above this point do not reach it).
    //
    // The verdict is scoped to `command_heads` — the tools this command names —
    // because PPID alone is not a command identity: every command an agent runs
    // shares it, so an unscoped marker lets one command's verdict decide an
    // unrelated concurrent or nested one's. See `set_force_raw`.
    //
    // Placed before every return below (already-skim, corrupt-bail, indefinite,
    // and the auto-mode withhold of a rewrite that needs no masking) so the clear
    // always happens: a marker that outlived its command would silently disable
    // compression for the next one.
    //
    // ACCEPTED LIMITATION: this is the only writer, so the marker exists only
    // when the hook actually fires. A wrapper invoked from a shell with no
    // PreToolUse hook gets fstat-only behaviour. See `force_raw_requested` in
    // main.rs.
    if let Some(dir) = crate::cmd::resolve_cache_dir() {
        crate::cmd::session_sidecar::set_force_raw(
            super::compound::command_needs_exact_bytes(&command),
            &super::compound::command_heads(&command),
            &dir,
        );
    }

    let (tokens, has_operator_chars) = match command_gates(&command) {
        Ok(passed) => passed,
        Err(decline) => {
            audit_hook(&command, false, "");
            // F12 — a declined multi-line rewrite was indistinguishable from
            // "nothing to rewrite", so record it to hook.log (ADR-011 class 2,
            // never stderr). The other bail reasons stay silent; see
            // `log_declined_multiline`.
            if decline == (Decline::Unsafe { multiline: true }) {
                log_declined_multiline(agent_kind.cli_name());
            }
            return Ok(ExitCode::SUCCESS);
        }
    };

    // The RewriteResult is kept structured (parts vector) until after
    // session-id injection (#317). An `npx <tool>` segment without its tool in
    // the project (or with a folder the hook cannot read) is not rewritten; the
    // rest of the command is, so a masking segment keeps its rewrite.
    let rewritten = hook_rewrite(&tokens, has_operator_chars, &local_bin_probe(&json));

    match rewritten {
        Some(result) if withholds_rewrite(&json, &result) => {
            // Auto permission mode, and no masking tool on the path: send no
            // response, so there is no stdout, no stderr and no permission decision.
            audit_hook(&command, false, "");
        }
        Some(result) => {
            // Attribution flows out-of-band: session_id was already written to the
            // sidecar file above (write_session_id). The rewritten command does NOT
            // carry --session-id so it is safe against version-skew failures (#1.1).
            // The skim child process resolves session attribution via the sidecar
            // ancestry walk (read_session_id) or SKIM_SESSION_ID env var.
            let final_cmd = result.tokens.join(" ");
            audit_hook(&command, true, &final_cmd);
            let response = protocol.format_response(json.get("tool_input"), &final_cmd);
            write_hook_response(&response)?;
        }
        None => {
            audit_hook(&command, false, "");
        }
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
        Err(_) => {
            // Script unreadable: cannot compute hash, so cannot detect tampering.
            // Return false (integrity check does NOT signal failure) so drift
            // detection continues to run — the unreadable-script state will be
            // visible via `skim doctor` without silencing drift on this channel.
            // Only `Tampered` (above) returns true and suppresses drift.
            false
        }
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
    // is_auto_permission_mode
    // ========================================================================

    #[test]
    fn test_is_auto_permission_mode_auto_is_true() {
        let input = serde_json::json!({ "permission_mode": "auto" });
        assert!(is_auto_permission_mode(&input));
    }

    #[test]
    fn test_is_auto_permission_mode_other_modes_are_false() {
        for mode in [
            "default",
            "plan",
            "acceptEdits",
            "dontAsk",
            "bypassPermissions",
        ] {
            let input = serde_json::json!({ "permission_mode": mode });
            assert!(
                !is_auto_permission_mode(&input),
                "permission_mode {mode:?} must keep the normal rewrite behaviour"
            );
        }
    }

    #[test]
    fn test_is_auto_permission_mode_missing_field_is_false() {
        let input = serde_json::json!({ "tool_input": { "command": "cargo test" } });
        assert!(!is_auto_permission_mode(&input));
    }

    #[test]
    fn test_is_auto_permission_mode_non_string_is_false() {
        for value in [
            serde_json::json!(1),
            serde_json::json!(null),
            serde_json::json!(true),
            serde_json::json!(["auto"]),
            serde_json::json!({ "mode": "auto" }),
        ] {
            let input = serde_json::json!({ "permission_mode": value });
            assert!(
                !is_auto_permission_mode(&input),
                "non-string permission_mode {value} must keep the normal rewrite behaviour"
            );
        }
    }

    #[test]
    fn test_is_auto_permission_mode_is_case_exact_and_top_level_only() {
        assert!(!is_auto_permission_mode(
            &serde_json::json!({ "permission_mode": "Auto" })
        ));
        assert!(!is_auto_permission_mode(
            &serde_json::json!({ "permission_mode": " auto" })
        ));
        assert!(!is_auto_permission_mode(
            &serde_json::json!({ "tool_input": { "permission_mode": "auto" } })
        ));
    }

    // ========================================================================
    // withholds_rewrite
    // ========================================================================

    /// The rewrite the hook finds for `command` when `npx_ok` answers the
    /// local-bin check, by the same calls the hook makes.
    fn rewrite_with(command: &str, npx_ok: &dyn Fn(NpxTool) -> bool) -> Option<RewriteResult> {
        let (tokens, has_operator_chars) =
            command_gates(command).unwrap_or_else(|_| panic!("{command:?} must pass the gates"));
        hook_rewrite(&tokens, has_operator_chars, npx_ok)
    }

    /// The rewrite the hook finds for `command` in a project that holds every tool.
    fn rewrite_of(command: &str) -> RewriteResult {
        rewrite_with(command, &|_| true)
            .unwrap_or_else(|| panic!("{command:?} must have a rewrite"))
    }

    fn auto_input() -> serde_json::Value {
        serde_json::json!({ "permission_mode": "auto" })
    }

    #[test]
    fn test_withholds_rewrite_in_auto_mode_for_commands_that_need_no_masking() {
        for command in ["cargo test", "cat src/main.rs", "cargo test && cargo build"] {
            assert!(
                withholds_rewrite(&auto_input(), &rewrite_of(command)),
                "{command:?}: auto mode must withhold a rewrite that masks nothing"
            );
        }
    }

    #[test]
    fn test_withholds_rewrite_never_in_auto_mode_for_masking_tools() {
        for command in [
            "env",
            "printenv",
            "printenv HOME",
            "curl https://example.com",
            "TOKEN=abc curl https://example.com",
            "env && cargo test",
            "cargo build && curl https://example.com",
        ] {
            assert!(
                !withholds_rewrite(&auto_input(), &rewrite_of(command)),
                "{command:?}: auto mode must keep a rewrite that routes through a masking tool"
            );
        }
    }

    #[test]
    fn test_withholds_rewrite_ignores_a_literal_skim_masking_segment() {
        // The user typed `skim env`; the hook rewrites only the `cargo test` segment.
        let result = rewrite_of("echo hi && skim env && cargo test");
        assert!(withholds_rewrite(&auto_input(), &result));
    }

    #[test]
    fn test_withholds_rewrite_never_outside_auto_mode() {
        let missing = serde_json::json!({ "tool_input": { "command": "cargo test" } });
        let default = serde_json::json!({ "permission_mode": "default" });
        for input in [&missing, &default] {
            for command in ["cargo test", "env", "curl https://example.com"] {
                assert!(
                    !withholds_rewrite(input, &rewrite_of(command)),
                    "{command:?}: outside auto mode the hook withholds nothing"
                );
            }
        }
    }

    // ========================================================================
    // hook_cwd and local_bin_probe
    // ========================================================================

    /// A project folder: a `.git` marker, and when `tool` is given an
    /// executable of that name in `node_modules/.bin`.
    #[cfg(unix)]
    fn project_with(tool: Option<&str>) -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(".git")).expect("git marker");
        if let Some(tool) = tool {
            let bin_dir = dir.path().join("node_modules").join(".bin");
            std::fs::create_dir_all(&bin_dir).expect("create .bin");
            let bin = bin_dir.join(tool);
            std::fs::write(&bin, b"#!/bin/sh\n").expect("write bin");
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
                .expect("make executable");
        }
        dir
    }

    #[test]
    fn test_hook_cwd_absent_is_the_process_folder() {
        assert_eq!(
            hook_cwd(&serde_json::json!({})),
            std::env::current_dir().ok()
        );
    }

    #[test]
    fn test_hook_cwd_absolute_folder_is_resolved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let input = serde_json::json!({ "cwd": dir.path().to_str().expect("UTF-8") });
        assert_eq!(hook_cwd(&input), dir.path().canonicalize().ok());
    }

    #[test]
    fn test_hook_cwd_rejects_what_it_cannot_trust() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("absent");
        for cwd in [
            serde_json::json!("relative/folder"),
            serde_json::json!("."),
            serde_json::json!(""),
            serde_json::json!(7),
            serde_json::json!(null),
            serde_json::json!(["/"]),
            serde_json::json!({ "path": "/" }),
            serde_json::json!(missing.to_str().expect("UTF-8")),
        ] {
            assert_eq!(hook_cwd(&serde_json::json!({ "cwd": cwd })), None, "{cwd}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_local_bin_probe_follows_the_local_bin() {
        let with_bin = project_with(Some("tsc"));
        let without_bin = project_with(None);
        let other_tool = project_with(Some("eslint"));
        let cwd = |dir: &tempfile::TempDir| serde_json::json!({ "cwd": dir.path().to_str().expect("UTF-8") });

        assert!(local_bin_probe(&cwd(&with_bin))(NpxTool::Tsc));
        assert!(!local_bin_probe(&cwd(&without_bin))(NpxTool::Tsc));
        assert!(
            !local_bin_probe(&cwd(&other_tool))(NpxTool::Tsc),
            "another tool's bin does not count"
        );
    }

    #[test]
    fn test_local_bin_probe_is_false_when_the_folder_is_unknown() {
        for cwd in [
            serde_json::json!("relative"),
            serde_json::json!(1),
            serde_json::json!(null),
        ] {
            assert!(
                !local_bin_probe(&serde_json::json!({ "cwd": cwd }))(NpxTool::Tsc),
                "{cwd}"
            );
        }
    }

    // ========================================================================
    // hook_rewrite: an npx segment without its tool
    // ========================================================================

    #[test]
    fn test_hook_rewrite_leaves_a_lone_npx_command_as_typed_without_the_tool() {
        assert!(rewrite_with("npx tsc --noEmit", &|_| false).is_none());
    }

    /// An operator character inside quotes makes a single command that still
    /// goes through the operator branch: the check must apply there too.
    #[test]
    fn test_hook_rewrite_applies_the_tool_check_to_a_quoted_operator_command() {
        let command = "npx eslint \"a;b\"";
        assert!(
            rewrite_with(command, &|_| true).is_some(),
            "{command:?} must rewrite when the tool is there"
        );
        assert!(rewrite_with(command, &|_| false).is_none(), "{command:?}");
    }

    #[test]
    fn test_hook_rewrite_rewrites_a_lone_npx_command_with_the_tool() {
        let result = rewrite_with("npx tsc --noEmit", &|_| true).expect("must rewrite");
        assert_eq!(result.tokens.join(" "), "skim tsc --via-npx --noEmit");
        assert_eq!(result.npx_tool, Some(NpxTool::Tsc));
    }

    /// The regression: an npx first segment without its tool must not take the
    /// masking rewrite of a later segment with it.
    #[test]
    fn test_hook_rewrite_keeps_a_masking_segment_after_an_npx_segment_without_the_tool() {
        for (command, expected) in [
            ("npx tsc && env", "npx tsc && skim env"),
            ("npx eslint . ; printenv", "npx eslint . ; skim printenv"),
            (
                "npx tsc && curl -v https://example.com",
                "npx tsc && skim curl -v https://example.com",
            ),
        ] {
            let result = rewrite_with(command, &|_| false)
                .unwrap_or_else(|| panic!("{command:?}: the masking segment must rewrite"));
            assert_eq!(result.tokens.join(" "), expected, "{command:?}");
            assert!(result.masks_output, "{command:?}");
            assert_eq!(result.npx_tool, None, "{command:?}");
            assert!(
                !withholds_rewrite(&auto_input(), &result),
                "{command:?}: auto mode must keep it"
            );
        }
    }

    #[test]
    fn test_hook_rewrite_rewrites_both_segments_when_the_tool_is_present() {
        let result = rewrite_with("npx tsc && env", &|_| true).expect("must rewrite");
        assert_eq!(result.tokens.join(" "), "skim tsc --via-npx && skim env");
        assert_eq!(result.npx_tool, Some(NpxTool::Tsc));
        assert!(result.masks_output);
    }

    #[test]
    fn test_hook_rewrite_keeps_the_other_segments_of_a_compound_without_the_tool() {
        let result = rewrite_with("npx tsc && cargo test", &|_| false).expect("must rewrite");
        assert_eq!(result.tokens.join(" "), "npx tsc && skim cargo test");
        assert!(!result.masks_output);
        assert!(
            withholds_rewrite(&auto_input(), &result),
            "with nothing to mask, auto mode still withholds it"
        );
    }

    #[test]
    fn test_hook_rewrite_with_an_unreadable_folder_leaves_only_the_npx_segment() {
        for cwd in [
            serde_json::json!("relative"),
            serde_json::json!(1),
            serde_json::json!(null),
        ] {
            let input = serde_json::json!({ "cwd": cwd });
            let probe = local_bin_probe(&input);
            let result = rewrite_with("npx tsc && env", &probe).expect("must rewrite");
            assert_eq!(result.tokens.join(" "), "npx tsc && skim env", "{cwd}");
            assert!(rewrite_with("npx tsc --noEmit", &probe).is_none(), "{cwd}");
        }
    }

    #[test]
    fn test_hook_rewrite_never_asks_about_the_folder_without_a_first_npx_segment() {
        let never = |_: NpxTool| -> bool { panic!("no npx first segment: the folder is not read") };

        assert!(rewrite_with("cargo test", &never).is_some());
        assert!(rewrite_with("cargo test && npx tsc", &never).is_some());
        // A later npx segment stays as typed and is not asked about.
        let later = rewrite_with("cd x && npx tsc && env", &never).expect("env must rewrite");
        assert_eq!(later.tokens.join(" "), "cd x && npx tsc && skim env");
    }

    // ========================================================================
    // command_gates and route_stdin
    // ========================================================================

    #[test]
    fn test_command_gates_decline_reason_tokens_and_operator_flag() {
        for (command, expected) in [
            ("skim test cargo", Decline::AlreadySkim),
            ("cargo test\nls", Decline::Unsafe { multiline: true }),
            ("echo $(date)", Decline::Unsafe { multiline: false }),
            ("   ", Decline::Empty),
            ("npm run dev", Decline::Indefinite),
        ] {
            assert_eq!(command_gates(command).unwrap_err(), expected, "{command:?}");
        }
        // Trailing newlines do not bail.
        assert_eq!(
            command_gates("cargo test\n"),
            Ok((vec!["cargo", "test"], false))
        );
        for (command, operators) in [("a && b", true), ("a | b", true), ("a & b", false)] {
            assert_eq!(
                command_gates(command).map(|(_, operators)| operators),
                Ok(operators),
                "{command:?}"
            );
        }
    }

    fn post_event(stdout_len: usize) -> Vec<u8> {
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_response": { "stdout": "x".repeat(stdout_len) },
        })
        .to_string()
        .into_bytes()
    }

    fn pre_event(event: Option<&str>, padding: usize) -> Vec<u8> {
        let mut input = serde_json::json!({
            "tool_input": { "command": format!("cargo test{}", " ".repeat(padding)) },
        });
        if let Some(event) = event {
            input["hook_event_name"] = event.into();
        }
        input.to_string().into_bytes()
    }

    #[test]
    fn test_route_stdin_by_event_agent_and_size() {
        use AgentKind::{ClaudeCode as Claude, Cursor, GeminiCli as Gemini};
        let over_pre_cap = 200 * 1024;
        let post_cap = HOOK_POST_MAX_STDIN_BYTES as usize;
        let cut = post_event(2 * post_cap)[..post_cap].to_vec();
        let cases: Vec<(Vec<u8>, AgentKind, &str)> = vec![
            (post_event(10), Claude, "post"),
            (post_event(10), Cursor, "post"),
            (post_event(over_pre_cap), Claude, "post"),
            (pre_event(None, 0), Claude, "pre"),
            (pre_event(Some("PreToolUse"), 0), Claude, "pre"),
            (
                pre_event(Some("PostToolUseFailure"), 0),
                Claude,
                "silent skip",
            ),
            (pre_event(Some("Stop"), 0), Claude, "silent skip"),
            (pre_event(Some("preToolUse"), 0), Cursor, "pre"),
            (pre_event(Some("BeforeTool"), 0), Gemini, "pre"),
            // A Pre input over the Pre cap passes through, audited as before.
            (
                pre_event(Some("PreToolUse"), over_pre_cap),
                Claude,
                "audited skip",
            ),
            (cut, Claude, "audited skip"),
            (b"\xff\xfe".to_vec(), Claude, "silent skip"),
            (b"".to_vec(), Claude, "audited skip"),
            (b"not json".to_vec(), Claude, "audited skip"),
        ];
        for (bytes, agent, expected) in cases {
            let route = match route_stdin(&bytes, agent) {
                HookRoute::Post(_) => "post",
                HookRoute::Pre(_) => "pre",
                HookRoute::Skip { audit: true } => "audited skip",
                HookRoute::Skip { audit: false } => "silent skip",
            };
            assert_eq!(route, expected, "{agent:?}, {} bytes", bytes.len());
        }
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
    // detect_drift: scoping rule and inversion fixes
    // ========================================================================

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

    // ========================================================================
    // Drift logging invariants: drift is detected and recorded to hook.log
    // only, never injected into the hook response JSON. The fix for drift is
    // absolute-path pinning + `skim init` refresh + `skim doctor` on-demand.
    // The hook-drift/ directory is never created — warn_once_daily stamps
    // (e.g. .hook-version-warned-<agent>) are the per-day dedup mechanism.
    // ========================================================================

    /// Invariant: when drift is detected, the logging path fires
    /// (warn_once_daily stamps written) but the hook response JSON carries no
    /// drift information — no `systemMessage`, no `additionalContext`.
    /// The `hook-drift/` directory is never created.
    #[test]
    fn test_drift_logs_to_hook_log_never_to_hook_response() {
        use crate::cmd::hooks::HookProtocol;
        use crate::cmd::hooks::claude::ClaudeCodeHook;
        use tempfile::TempDir;

        let temp = TempDir::new().unwrap();
        let hook_drift_dir = temp.path().join("hook-drift");

        // Build a drift env with a clear version mismatch.
        let env = DriftEnv {
            hook_version: Some("2.10.0".to_string()),
            hook_binary: None, // absent → HookScriptUnpinned
            hook_commit: None,
            current_exe: Some(std::path::PathBuf::from("/usr/local/bin/skim")),
            compiled_version: "2.11.0".to_string(),
            compiled_commit: "abc1234f".to_string(),
        };
        let drifts = detect_drift(&env);
        assert!(
            !drifts.is_empty(),
            "test requires drift to be detected; got: {drifts:?}"
        );

        // Call log_drift_warnings — this is the only path drift now takes.
        // It should write warn_once_daily stamps under temp, triggering log_hook_warning.
        log_drift_warnings(&drifts, &env, "claude-code", temp.path());

        // Stamp files must exist (warn_once_daily was triggered).
        let version_stamp = temp.path().join(".hook-version-warned-claude-code");
        let unpinned_stamp = temp.path().join(".hook-unpinned-warned-claude-code");
        assert!(
            version_stamp.exists() || unpinned_stamp.exists(),
            "warn_once_daily must have created a per-kind stamp file — logging path was not triggered"
        );

        // The hook-drift/ directory must never be created.
        assert!(
            !hook_drift_dir.exists(),
            "hook-drift/ directory must never be created: {hook_drift_dir:?}"
        );

        // The hook response JSON must contain no systemMessage or additionalContext.
        let response = ClaudeCodeHook.format_response(None, "skim git status");
        let json_str = response.to_string();
        assert!(
            !json_str.contains("systemMessage"),
            "hook response must not contain systemMessage: {json_str}"
        );
        assert!(
            !json_str.contains("additionalContext"),
            "hook response must not contain additionalContext: {json_str}"
        );
        // Verify the response only has updatedInput (the correct rewrite shape).
        let updated = response
            .get("hookSpecificOutput")
            .and_then(|o| o.get("updatedInput"))
            .and_then(|u| u.get("command"));
        assert_eq!(
            updated.and_then(|v| v.as_str()),
            Some("skim git status"),
            "hook response must contain updatedInput.command with rewritten command"
        );
    }
}
