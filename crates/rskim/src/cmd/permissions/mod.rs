//! Agent-permission writers for `skim init --permissions`.
//!
//! This module provides the `PermissionsProtocol` trait and per-agent writers
//! that seed read-only tool allow-list entries into each agent's config file.
//!
//! ## Design doctrine (user-ratified)
//!
//! skim **never self-approves**: permissions seeding happens ONLY at `skim init`
//! time, gated on interactive human consent via `confirm_grant()`. The runtime
//! hook **never** writes permissions. The `permissions_protocol_for_agent`
//! factory returns `None` for agents whose permission format is not supported
//! or is deliberately excluded (see per-agent notes below).
//!
//! ## Seed derivation
//!
//! The seeded tool list is the intersection of `READ_ONLY_SUBCOMMANDS` and
//! `wrapper_targets()` — always exactly 7 tools:
//! `df`, `diff`, `du`, `grep`, `ls`, `tree`, `wc`.
//!
//! A `Bash(skim <tool>:*)` prefix entry does NOT bound the wrapped tool's
//! arguments, so only arg-safe read-only tools may ever be seeded. A tool with
//! an option that starts another program (`rg --pre`) is not arg-safe.
//!
//! ## Retired seed entries
//!
//! When a tool leaves the seed list, the user-owned writers (Claude, Copilot)
//! remove its entry on the next seed run, but only when a seed-tier sidecar
//! recorded it ([`retired_seed_entries`]). The skim-owned writers (Gemini,
//! Codex) regenerate their whole file, so they need no extra step.
//!
//! ## Registry invariant
//!
//! `READ_ONLY_SUBCOMMANDS` is referenced ONLY from this module (install-time).
//! It must never be imported from rewrite/dispatch paths.

pub(crate) mod sidecar;

pub(crate) mod claude;
mod codex;
mod copilot;
mod gemini;

use std::path::Path;

use crate::cmd::session::AgentKind;

/// Which tier of permissions to seed.
///
/// Re-exported here so the permissions module can use it without a cyclic import
/// through `cmd::init`. The canonical definition lives in `cmd::init::flags`.
pub(crate) use crate::cmd::init::PermissionsTier;

// ============================================================================
// Outcome types
// ============================================================================

/// Result of a [`PermissionsProtocol::seed`] call.
#[derive(Debug)]
pub(crate) enum SeedOutcome {
    /// The agent config was changed.
    ///
    /// `entries_added` contains only the entries actually inserted; entries
    /// that were already present are skipped (idempotent dedup).
    /// `entries_removed` contains the retired seed entries that were taken
    /// out (see [`retired_seed_entries`]); it is empty for most runs.
    Added {
        entries_added: Vec<String>,
        entries_removed: Vec<String>,
    },
    /// All requested entries were already present — config was not modified.
    AlreadyCurrent,
}

/// Result of a [`PermissionsProtocol::remove_seeded`] call.
#[derive(Debug)]
pub(crate) enum RemoveOutcome {
    /// Seeded entries were removed from the agent config.
    Removed { entries_removed: Vec<String> },
    /// No seeded entries were found — nothing to remove.
    NothingToRemove,
}

// ============================================================================
// Trait
// ============================================================================

/// Format-agnostic agent permission writer.
///
/// Each method takes explicit `&Path` arguments and **never reads environment
/// variables** — env-var resolution is the caller's responsibility (use
/// [`crate::cmd::init::DetectionEnv::resolve`] to obtain the config dir).
///
/// ## Read-once design note
///
/// The install path should read each config file **exactly once** per
/// operation. Callers that need to check currency before seeding can call
/// [`seed`] directly — it returns [`SeedOutcome::AlreadyCurrent`] when no
/// changes are needed, avoiding a separate `is_current` read. The
/// `is_current` method exists for inspection / dry-run use cases.
///
/// [`seed`]: PermissionsProtocol::seed
pub(crate) trait PermissionsProtocol {
    /// Human-readable agent label for consent prompts (e.g. `"Claude Code"`).
    fn agent_label(&self) -> &str;

    /// Name of the config file (or relative path) that this writer targets.
    ///
    /// For user-owned files this is the file modified in place.
    /// For skim-owned files this is the wholly-replaced file.
    fn config_filename(&self) -> &str;

    /// Convert a raw tool name (e.g. `"df"`) into the agent-native allowlist
    /// entry string that will be stored in the agent config and the sidecar.
    ///
    /// Examples:
    /// - Claude Code: `"df"` → `"Bash(skim df:*)"`
    /// - Gemini CLI:  `"df"` → `"skim df"` (TOML pattern value)
    /// - Codex CLI:   `"df"` → `"df"` (raw tool token, validated against charset)
    fn render_entry(&self, tool: &str) -> String;

    /// Seed the agent config with the given native entries.
    ///
    /// For **user-owned files** (Claude `settings.json`): reads the file, deduplicates,
    /// inserts missing entries, writes atomically, writes the sidecar.
    ///
    /// For **skim-owned files** (Gemini TOML, Codex rules): generates the file
    /// wholesale from `entries`, writes atomically, writes the sidecar.
    ///
    /// Returns [`SeedOutcome::AlreadyCurrent`] when no changes are needed,
    /// allowing the caller to skip the TTY consent prompt.
    fn seed(
        &self,
        config_dir: &Path,
        tier: PermissionsTier,
        entries: &[String],
    ) -> anyhow::Result<SeedOutcome>;

    /// Remove only the entries that skim seeded from the agent config.
    ///
    /// Loads the sidecar to identify which entries were seeded. **Fails loud**
    /// on a missing or corrupt sidecar — callers must never silently proceed
    /// without a sidecar (risk: removing entries skim did not write).
    ///
    /// For user-owned files (Claude `settings.json`, Copilot `permissions-config.json`):
    /// removes only sidecar-manifest entries that are still byte-equal present in the
    /// allow array; leaves everything else.
    /// For skim-owned files (Gemini `policies/skim.toml`, Codex `rules/skim.rules`):
    /// deletes the file only after hash-verifying against the sidecar.
    ///
    /// ## Sidecar behavior when the config file is absent
    ///
    /// This diverges by config ownership:
    /// - **Skim-owned writers** (Gemini, Codex): both the config file and the sidecar are
    ///   skim artifacts. If the config file is already gone, the sidecar is also deleted
    ///   and `NothingToRemove` is returned — there is nothing left to clean up.
    /// - **User-owned writers** (Claude, Copilot): skim does not own the config file's
    ///   lifecycle. If the config file is absent the sidecar is left in place and
    ///   `NothingToRemove` is returned — the sidecar still correctly records what was
    ///   seeded and may inform future operations.
    fn remove_seeded(&self, config_dir: &Path) -> anyhow::Result<RemoveOutcome>;

    /// Check whether all `entries` are already present in the agent config.
    ///
    /// Returns `false` on any I/O or parse error (non-fatal).
    ///
    /// NOTE: This performs a config file read. In hot paths where the result
    /// feeds directly into a `seed()` call, prefer letting `seed()` return
    /// `AlreadyCurrent` instead of calling `is_current` + `seed` separately.
    fn is_current(&self, config_dir: &Path, entries: &[String]) -> bool;
}

// ============================================================================
// Factory
// ============================================================================

/// Return the `PermissionsProtocol` implementation for `agent`, or `None` if
/// skim does not write permissions for this agent.
///
/// | Agent      | Result   | Reason                                                          |
/// |------------|---------|-----------------------------------------------------------------|
/// | ClaudeCode | `Some`   | Writes to `settings.json` `permissions.allow` array.            |
/// | GeminiCli  | `Some`   | Owns `policies/skim.toml` (wholesale replacement).              |
/// | CodexCli   | `Some`   | Owns `rules/skim.rules` (Starlark prefix-rule lines).           |
/// | CopilotCli | `Some`   | Writes per-project permissions-config.json keyed by git root.   |
/// | Cursor     | `None`   | **Permanent**: IDE-only — its CLI has no rewrite-capable hook.  |
/// | Crush      | `None`   | **Permanent**: no permissions surface — no config file to write.|
pub(crate) fn permissions_protocol_for_agent(
    agent: AgentKind,
) -> Option<Box<dyn PermissionsProtocol>> {
    match agent {
        AgentKind::ClaudeCode => Some(Box::new(claude::ClaudePermissions)),
        AgentKind::GeminiCli => Some(Box::new(gemini::GeminiPermissions)),
        AgentKind::CodexCli => Some(Box::new(codex::CodexPermissions)),
        // Permanent None: Cursor is IDE-only — its CLI has no rewrite-capable
        // hook event; skim integrates via the IDE hook + .mdc guidance only.
        // No permissions file is ever seeded.
        AgentKind::Cursor => None,
        // Permanent None: Crush has no permissions surface — no config file to write.
        AgentKind::Crush => None,
        // Copilot writer: per-project permissions-config.json keyed by git root.
        // Schema validated in principle, pending deferred Copilot CLI e2e.
        AgentKind::CopilotCli => Some(Box::new(copilot::CopilotPermissions)),
    }
}

// ============================================================================
// Shared file-hashing helper (size-bounded)
// ============================================================================

/// Hash `path`'s contents after confirming it falls within the permitted size bound.
///
/// Returns:
/// - `Ok(Some(hash))` — file is within [`crate::cmd::init::MAX_SETTINGS_SIZE`] and
///   hashed successfully.
/// - `Ok(None)` — file exceeds the size bound; callers apply context-specific semantics:
///   - `is_current` paths → return `false` (treat as not current).
///   - `remove_seeded` paths → return `Err` with an actionable message.
///   - seed-idempotency paths → treat as not-idempotent (fall through to seed logic).
///   - after-atomic-write paths → return `Err` (unexpected; skim controls the write).
/// - `Err(_)` — I/O or hash error; propagate to caller.
///
/// This is the single point through which all `compute_file_hash` calls in the
/// permissions module are routed so that MAX_SETTINGS_SIZE parity is enforced
/// consistently without duplicating the metadata guard at every call site.
pub(super) fn hash_if_bounded(path: &std::path::Path) -> anyhow::Result<Option<String>> {
    let size = std::fs::metadata(path)?.len();
    if size > crate::cmd::init::MAX_SETTINGS_SIZE {
        return Ok(None);
    }
    crate::cmd::integrity::compute_file_hash(path).map(Some)
}

// ============================================================================
// Seed derivation helper
// ============================================================================

/// Compute the agent-native seed entries for a permissions protocol.
///
/// The seed tool list is `READ_ONLY_SUBCOMMANDS ∩ wrapper_targets()`, which
/// is always exactly the 7 tools declared in `READ_ONLY_SUBCOMMANDS` (registry
/// tests enforce that it is a strict subset of `wrapper_targets()`).
///
/// Each tool is mapped through `protocol.render_entry(tool)` to produce the
/// agent-native entry string. Entries are returned in sorted order so that
/// identical seeding always produces the same sidecar and config diff.
pub(crate) fn seeded_entries(protocol: &dyn PermissionsProtocol) -> Vec<String> {
    use crate::cmd::registry::READ_ONLY_SUBCOMMANDS;
    // READ_ONLY_SUBCOMMANDS is already sorted (registry tests enforce this),
    // and render_entry is deterministic, so we get a stable sorted list.
    let mut entries: Vec<String> = READ_ONLY_SUBCOMMANDS
        .iter()
        .map(|&tool| protocol.render_entry(tool))
        .collect();
    // Sort after mapping in case render_entry reorders (defensive; currently
    // READ_ONLY_SUBCOMMANDS is sorted and render_entry preserves order, but
    // sorting here makes the invariant explicit and testable).
    entries.sort();
    entries
}

/// Entries that an earlier seed-tier run recorded in the sidecar at
/// `config_dir` but that `entries` (the current seed list) no longer contains.
///
/// The user-owned writers (Claude, Copilot) merge into the allow list, so
/// without this step a tool that left the seed list keeps its entry after a
/// re-seed. Only a `"seed"`-tier sidecar is consulted: a mirror-tier sidecar
/// records mirrors of the user's own rules, which a seed run must not remove.
/// A missing or unreadable sidecar gives an empty list (nothing is known to
/// be skim's, so nothing is removed).
pub(super) fn retired_seed_entries(config_dir: &Path, entries: &[String]) -> Vec<String> {
    match sidecar::load_sidecar(&config_dir.join(sidecar::SIDECAR_FILENAME)) {
        Ok(recorded) if recorded.tier == "seed" => recorded
            .entries
            .into_iter()
            .filter(|e| !entries.contains(e))
            .collect(),
        _ => Vec::new(),
    }
}

/// Remove every string in `targets` from `allow`; return the removed strings
/// in their allow-list order. Used by the user-owned writers (Claude, Copilot)
/// to take out [`retired_seed_entries`].
pub(super) fn remove_entries(
    allow: &mut Vec<serde_json::Value>,
    targets: &[String],
) -> Vec<String> {
    let mut removed = Vec::new();
    allow.retain(|v| match v.as_str() {
        Some(s) if targets.iter().any(|t| t == s) => {
            removed.push(s.to_string());
            false
        }
        _ => true,
    });
    removed
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- factory: permanent Nones (pinned) ----

    #[test]
    fn test_factory_cursor_returns_none_permanently() {
        // Cursor is IDE-only — no CLI hook, no permissions file. This None
        // is PERMANENT. If this test fails, investigate before flipping it.
        assert!(
            permissions_protocol_for_agent(AgentKind::Cursor).is_none(),
            "Cursor must permanently return None from permissions factory (IDE-only)"
        );
    }

    #[test]
    fn test_factory_crush_returns_none_permanently() {
        // Crush has no permissions surface — no config file to write. Permanent None.
        assert!(
            permissions_protocol_for_agent(AgentKind::Crush).is_none(),
            "Crush must permanently return None from permissions factory (no permissions surface)"
        );
    }

    #[test]
    fn test_factory_copilot_returns_some() {
        // Copilot CLI has a permanent permissions writer (per-project permissions-config.json).
        assert!(
            permissions_protocol_for_agent(AgentKind::CopilotCli).is_some(),
            "Copilot must return Some from permissions factory (per-project writer)"
        );
    }

    // ---- factory: Some agents ----

    #[test]
    fn test_factory_claude_returns_some() {
        assert!(
            permissions_protocol_for_agent(AgentKind::ClaudeCode).is_some(),
            "ClaudeCode must have a permissions writer"
        );
    }

    #[test]
    fn test_factory_gemini_returns_some() {
        assert!(
            permissions_protocol_for_agent(AgentKind::GeminiCli).is_some(),
            "GeminiCli must have a permissions writer"
        );
    }

    #[test]
    fn test_factory_codex_returns_some() {
        assert!(
            permissions_protocol_for_agent(AgentKind::CodexCli).is_some(),
            "CodexCli must have a permissions writer"
        );
    }

    // ---- literal-7 seed pin (Claude) ----

    /// Claude seeded entries must equal exactly the 7 `Bash(skim <tool>:*)` strings.
    ///
    /// This test pins the entry set. Any future addition requires a deliberate
    /// edit to READ_ONLY_SUBCOMMANDS AND this assertion — drift is never silent.
    #[test]
    fn test_claude_seeded_entries_exact_7() {
        let protocol = permissions_protocol_for_agent(AgentKind::ClaudeCode).unwrap();
        let entries = seeded_entries(protocol.as_ref());
        assert_eq!(
            entries,
            vec![
                "Bash(skim df:*)",
                "Bash(skim diff:*)",
                "Bash(skim du:*)",
                "Bash(skim grep:*)",
                "Bash(skim ls:*)",
                "Bash(skim tree:*)",
                "Bash(skim wc:*)",
            ],
            "Claude seeded entries must be exactly the 7 Bash(skim <tool>:*) strings (sorted)"
        );
    }

    /// No agent's seed list may name `rg`: its `--pre`, `--hostname-bin` and
    /// `-z` options start another program, and every seeded rule is a prefix
    /// rule that does not bound the arguments.
    #[test]
    fn test_no_agent_seeds_rg() {
        for &agent in &[
            AgentKind::ClaudeCode,
            AgentKind::GeminiCli,
            AgentKind::CodexCli,
            AgentKind::CopilotCli,
        ] {
            let p = permissions_protocol_for_agent(agent).unwrap();
            let rg_entry = p.render_entry("rg");
            let entries = seeded_entries(p.as_ref());
            assert!(
                !entries.contains(&rg_entry),
                "{agent:?} must not seed '{rg_entry}', got {entries:?}"
            );
            assert_eq!(entries.len(), 7, "{agent:?} must seed exactly 7 entries");
        }
    }

    // ---- per-agent render_entry ----

    #[test]
    fn test_claude_render_entry_format() {
        let p = permissions_protocol_for_agent(AgentKind::ClaudeCode).unwrap();
        assert_eq!(p.render_entry("df"), "Bash(skim df:*)");
        assert_eq!(p.render_entry("grep"), "Bash(skim grep:*)");
        assert_eq!(p.render_entry("wc"), "Bash(skim wc:*)");
    }

    #[test]
    fn test_gemini_render_entry_format() {
        let p = permissions_protocol_for_agent(AgentKind::GeminiCli).unwrap();
        assert_eq!(p.render_entry("df"), "skim df");
        assert_eq!(p.render_entry("grep"), "skim grep");
    }

    #[test]
    fn test_codex_render_entry_is_raw_tool_name() {
        let p = permissions_protocol_for_agent(AgentKind::CodexCli).unwrap();
        assert_eq!(p.render_entry("df"), "df");
        assert_eq!(p.render_entry("grep"), "grep");
        assert_eq!(p.render_entry("ls"), "ls");
    }

    // ---- seeded_entries is sorted ----

    #[test]
    fn test_seeded_entries_are_sorted() {
        for &agent in &[
            AgentKind::ClaudeCode,
            AgentKind::GeminiCli,
            AgentKind::CodexCli,
            AgentKind::CopilotCli,
        ] {
            let p = permissions_protocol_for_agent(agent).unwrap();
            let entries = seeded_entries(p.as_ref());
            let mut sorted = entries.clone();
            sorted.sort();
            assert_eq!(
                entries, sorted,
                "seeded_entries must be sorted for agent {:?}",
                agent
            );
        }
    }
}
