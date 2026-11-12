//! Agent-permission writers for `skim init --permissions`.
//!
//! This module provides the `PermissionsProtocol` trait and per-agent writers
//! that seed read-only tool allow-list entries into each agent's config file.
//!
//! ## Design doctrine (user-ratified)
//!
//! skim **never self-approves**: permissions seeding happens ONLY at `skim init`
//! time, gated on interactive human consent via `confirm_grant()`. The runtime
//! hook **never** writes permissions. The one change made without consent is
//! the removal of entries that skim no longer seeds (see below), because it
//! can only make the allow list smaller. The `permissions_protocol_for_agent`
//! factory returns `None` for agents whose permission format is not supported
//! or is deliberately excluded (see per-agent notes below).
//!
//! ## Seed derivation
//!
//! The seeded tool list is the intersection of `READ_ONLY_SUBCOMMANDS` and
//! `wrapper_targets()` — always exactly 6 tools:
//! `df`, `diff`, `du`, `grep`, `ls`, `wc`.
//!
//! A `Bash(skim <tool>:*)` prefix entry does NOT bound the wrapped tool's
//! arguments, so only arg-safe read-only tools may ever be seeded. A tool with
//! an option that writes a file or starts a program that the caller names is
//! not arg-safe (see the `READ_ONLY_SUBCOMMANDS` invariants).
//!
//! ## Retired seed entries
//!
//! When a tool leaves the seed list, every `skim init` removes its entry
//! ([`PermissionsProtocol::prune_retired`]), with no consent prompt and also
//! with no terminal, because the removal can only make the allow list
//! smaller. Only an entry that a seed-tier sidecar recorded is removed
//! ([`retired_seed_entries`]). Copilot removes it from every project in its
//! file, because one sidecar serves all projects. The skim-owned writers
//! (Gemini, Codex) regenerate their file from the recorded entries that skim
//! still seeds, but only when the file is still exactly what skim wrote.
//! [`PermissionsProtocol::is_current`] judges the config as if that removal
//! had run, so a `--dry-run`, which only plans it, gives the same result as
//! the real run.
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

/// Whether a [`PermissionsProtocol::prune_retired`] call changes files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PruneMode {
    /// Return what would be removed and write nothing (`--dry-run`).
    Plan,
    /// Remove the entries and rewrite the sidecar.
    Apply,
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

    /// Remove the entries that a seed-tier sidecar records but that `current`
    /// (the current seed list) no longer contains; return the entries that
    /// were in the config and are now gone.
    ///
    /// This step needs no consent and no terminal: it only takes out entries
    /// that skim itself recorded, so it can only make the allow list smaller.
    /// The sidecar is the only record of what is skim's
    /// ([`retired_seed_entries`]): with no seed-tier sidecar (missing,
    /// unreadable, or mirror tier) nothing is removed, and that is not an
    /// error.
    ///
    /// With [`PruneMode::Apply`] the writer removes the entries from its
    /// config and rewrites the sidecar so that it records only the entries
    /// that skim still seeds, also when the config no longer held the retired
    /// ones (a stale record must not remove a later entry that the user adds).
    /// The sidecar never gains an entry here: the user did not consent to new
    /// entries. With [`PruneMode::Plan`] nothing is written and the result is
    /// the list that `Apply` would remove.
    ///
    /// Call this before [`seed`](PermissionsProtocol::seed): `seed` records
    /// only the current list, so it would drop the record of a retired entry.
    fn prune_retired(
        &self,
        config_dir: &Path,
        current: &[String],
        mode: PruneMode,
    ) -> anyhow::Result<Vec<String>>;

    /// Check whether all `entries` are already present in the agent config.
    ///
    /// The config is judged as if [`prune_retired`](PermissionsProtocol::prune_retired)
    /// had run: an entry that the prune removes does not make the config
    /// stale. Every `skim init` prunes before it calls this, and `--dry-run`
    /// only plans the prune, so both runs give the same result.
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
/// is always exactly the 6 tools declared in `READ_ONLY_SUBCOMMANDS` (registry
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

/// The entries that a seed-tier sidecar records, split by the current seed
/// list. Empty (all fields) when there is no seed-tier sidecar.
#[derive(Debug, Default)]
pub(super) struct SeedRecord {
    /// Recorded entries that the current seed list still contains.
    pub(super) kept: Vec<String>,
    /// Recorded entries that the current seed list no longer contains.
    pub(super) retired: Vec<String>,
    /// The config file hash that the sidecar recorded.
    pub(super) config_hash: String,
}

/// Split the entries that an earlier seed-tier run recorded in the sidecar at
/// `config_dir` into the ones that `entries` (the current seed list) still
/// contains and the retired ones that it no longer contains.
///
/// This is the single source of "what is retired". Only a `"seed"`-tier
/// sidecar is consulted: a mirror-tier sidecar records mirrors of the user's
/// own rules, which must not be removed. A missing or unreadable sidecar
/// gives an empty record (nothing is known to be skim's, so nothing is
/// removed).
pub(super) fn retired_seed_entries(config_dir: &Path, entries: &[String]) -> SeedRecord {
    match sidecar::load_sidecar(&config_dir.join(sidecar::SIDECAR_FILENAME)) {
        Ok(recorded) if recorded.tier == "seed" => {
            let (kept, retired) = recorded
                .entries
                .into_iter()
                .partition(|e| entries.contains(e));
            SeedRecord {
                kept,
                retired,
                config_hash: recorded.config_hash,
            }
        }
        _ => SeedRecord::default(),
    }
}

/// Write the seed-tier sidecar for `entries`, with the hash of `config_path`
/// as it is on disk now. Used by the user-owned writers (Claude, Copilot)
/// after a config write, and by a prune also when only the sidecar's record
/// of a retired entry is stale (the entry is already gone from the config).
pub(super) fn write_seed_sidecar(
    config_dir: &Path,
    config_path: &Path,
    entries: &[String],
) -> anyhow::Result<()> {
    // skim wrote or size-checked the file just before this call; oversized here is unexpected.
    let config_hash = hash_if_bounded(config_path)?.ok_or_else(|| {
        anyhow::anyhow!(
            "{} unexpectedly exceeds the size limit — \
             this is an internal error; please file a bug report",
            config_path.display()
        )
    })?;
    let recorded = sidecar::PermissionSidecar {
        version: 1,
        tier: "seed".to_string(),
        entries: entries.to_vec(),
        source_mirrors: std::collections::HashMap::new(),
        config_hash,
    };
    sidecar::write_sidecar(&config_dir.join(sidecar::SIDECAR_FILENAME), &recorded)
}

/// Remove every string in `targets` from `allow`; return the removed strings
/// in their allow-list order. Used by the user-owned writers (Claude, Copilot)
/// to take out the retired entries of a [`SeedRecord`].
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

    // ---- literal-6 seed pin (Claude) ----

    /// Claude seeded entries must equal exactly the 6 `Bash(skim <tool>:*)` strings.
    ///
    /// This test pins the entry set. Any future addition requires a deliberate
    /// edit to READ_ONLY_SUBCOMMANDS AND this assertion — drift is never silent.
    #[test]
    fn test_claude_seeded_entries_exact_6() {
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
                "Bash(skim wc:*)",
            ],
            "Claude seeded entries must be exactly the 6 Bash(skim <tool>:*) strings (sorted)"
        );
    }

    /// No agent's seed list may name `rg` or `tree`: each has an option that
    /// is not read-only, and every seeded rule is a prefix rule that does not
    /// bound the arguments.
    #[test]
    fn test_no_agent_seeds_rg_or_tree() {
        for &agent in &[
            AgentKind::ClaudeCode,
            AgentKind::GeminiCli,
            AgentKind::CodexCli,
            AgentKind::CopilotCli,
        ] {
            let p = permissions_protocol_for_agent(agent).unwrap();
            let entries = seeded_entries(p.as_ref());
            for tool in ["rg", "tree"] {
                let entry = p.render_entry(tool);
                assert!(
                    !entries.contains(&entry),
                    "{agent:?} must not seed '{entry}', got {entries:?}"
                );
            }
            assert_eq!(entries.len(), 6, "{agent:?} must seed exactly 6 entries");
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
