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
//! `wrapper_targets()` — always exactly 8 tools:
//! `df`, `diff`, `du`, `grep`, `ls`, `rg`, `tree`, `wc`.
//!
//! A `Bash(skim <tool>:*)` prefix entry does NOT bound the wrapped tool's
//! arguments, so only arg-safe read-only tools may ever be seeded.
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
    /// New entries were added to the agent config.
    ///
    /// `entries_added` contains only the entries actually inserted; entries
    /// that were already present are skipped (idempotent dedup).
    Added { entries_added: Vec<String> },
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
    /// For user-owned files: removes only sidecar-manifest entries that are
    /// still byte-equal present in the allow array; leaves everything else.
    /// For skim-owned files: deletes the file only after hash-verifying against
    /// the sidecar.
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
/// | Agent      | Result   | Reason                                                      |
/// |------------|----------|-------------------------------------------------------------|
/// | ClaudeCode | `Some`   | Writes to `settings.json` `permissions.allow` array.        |
/// | GeminiCli  | `Some`   | Owns `policies/skim.toml` (wholesale replacement).          |
/// | CodexCli   | `Some`   | Owns `rules/skim.rules` (Starlark prefix-rule lines).       |
/// | Cursor     | `None`   | **Permanent**: IDE-only integration; no CLI permissions.    |
/// | Crush      | `None`   | **Permanent**: no permissions writer (WS2B decision).       |
/// | CopilotCli | `None`   | **Transitional**: Subtask 7 adds the Copilot writer after   |
/// |            |          | the hook re-home; do not flip this `None` before that task. |
pub(crate) fn permissions_protocol_for_agent(
    agent: AgentKind,
) -> Option<Box<dyn PermissionsProtocol>> {
    match agent {
        AgentKind::ClaudeCode => Some(Box::new(claude::ClaudePermissions)),
        AgentKind::GeminiCli => Some(Box::new(gemini::GeminiPermissions)),
        AgentKind::CodexCli => Some(Box::new(codex::CodexPermissions)),
        // Permanent None: Cursor is IDE-only (WS2B). skim integrates via the
        // IDE hook + .mdc guidance only. The Cursor CLI has no rewrite-capable
        // hook event, so no permissions file is ever seeded.
        AgentKind::Cursor => None,
        // Permanent None: Crush has no permissions writer (ratified in WS2B).
        AgentKind::Crush => None,
        // Copilot writer: per-project permissions-config.json keyed by git root.
        // Schema validated in principle, pending deferred Copilot CLI e2e.
        AgentKind::CopilotCli => Some(Box::new(copilot::CopilotPermissions)),
    }
}

// ============================================================================
// Seed derivation helper
// ============================================================================

/// Compute the agent-native seed entries for a permissions protocol.
///
/// The seed tool list is `READ_ONLY_SUBCOMMANDS ∩ wrapper_targets()`, which
/// is always exactly the 8 tools declared in `READ_ONLY_SUBCOMMANDS` (registry
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
        // Crush has no permissions writer (WS2B decision). Permanent None.
        assert!(
            permissions_protocol_for_agent(AgentKind::Crush).is_none(),
            "Crush must permanently return None from permissions factory (WS2B)"
        );
    }

    #[test]
    fn test_factory_copilot_returns_some() {
        // Copilot writer was added in Subtask 7 (per-project permissions-config.json).
        // This Some is PERMANENT — Copilot CLI now has a writer.
        assert!(
            permissions_protocol_for_agent(AgentKind::CopilotCli).is_some(),
            "Copilot must return Some from permissions factory (writer added in Subtask 7)"
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

    // ---- literal-8 seed pin (Claude) ----

    /// Claude seeded entries must equal exactly the 8 `Bash(skim <tool>:*)` strings.
    ///
    /// This test pins the entry set. Any future addition requires a deliberate
    /// edit to READ_ONLY_SUBCOMMANDS AND this assertion — drift is never silent.
    #[test]
    fn test_claude_seeded_entries_exact_8() {
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
                "Bash(skim rg:*)",
                "Bash(skim tree:*)",
                "Bash(skim wc:*)",
            ],
            "Claude seeded entries must be exactly the 8 Bash(skim <tool>:*) strings (sorted)"
        );
    }

    #[test]
    fn test_claude_seeded_entries_count_is_8() {
        let protocol = permissions_protocol_for_agent(AgentKind::ClaudeCode).unwrap();
        let entries = seeded_entries(protocol.as_ref());
        assert_eq!(
            entries.len(),
            8,
            "seed must always derive exactly 8 entries"
        );
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
