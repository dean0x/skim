//! Index staleness detection and auto-refresh orchestration.
//!
//! Compares the git HEAD commit recorded in the manifest (`index.skfiles`)
//! against the current git HEAD at query time.  When they diverge, the index
//! is stale and should be rebuilt.
//!
//! # Module layout
//!
//! This module owns four distinct concerns, separated into sibling modules to
//! keep each one's scope clear:
//!
//! 1. **Git plumbing** ([`super::gitdir`]) — `HeadState`, `resolve_git_dir`,
//!    `resolve_common_dir`, `git_head_state`, `read_git_head`.  Pure file I/O,
//!    no git binary, no libgit2.
//!
//! 2. **Index staleness policy** (this file) — `StalenessCheck`, `WorkingTreeDelta`,
//!    `scan_working_tree`, `check_staleness`, `RefreshOutcome`, `auto_refresh_if_stale`.
//!
//! 3. **Temporal-DB concerns** ([`super::temporal_state`]) — `ReanchorPolicy`,
//!    `AnchorState`, `temporal_anchor_state`, `temporal_db_is_stale`,
//!    `warn_if_temporal_unverifiable`, `try_rebuild_temporal_nonfatal`.
//!
//! 4. **Git-fixture test helpers** (this file, `#[cfg(test)]`) — `create_real_git_repo`,
//!    `create_real_git_repo_with_dates`, `create_real_git_worktree`, `plant_meta_raw`.
//!
//! Re-exports from (1) and (3) preserve the `staleness::*` access path used by
//! `mod.rs`, `hooks.rs`, and the test suite — callers do not need to know which
//! sub-module owns each item.

use std::path::Path;

use super::manifest::FileManifest;

// Re-export git-plumbing items (owned by gitdir.rs).
// `hooks.rs` accesses `resolve_git_dir` and `resolve_common_dir` via
// `super::staleness::*` — these re-exports keep that path valid.
#[cfg(test)]
pub(super) use super::gitdir::resolve_repo_toplevel;
pub(super) use super::gitdir::{
    HeadState, git_head_state, read_git_head, resolve_common_dir, resolve_git_dir,
};

// Re-export temporal-DB items (owned by temporal_state.rs).
#[cfg(test)]
pub(super) use super::temporal_state::temporal_anchor_state;
pub(super) use super::temporal_state::{
    AnchorState, ReanchorPolicy, anchor_state_on_db, temporal_db_is_stale,
    try_rebuild_temporal_nonfatal, warn_if_temporal_unverifiable, warn_if_temporal_unverifiable_at,
};

// ============================================================================
// Staleness outcome
// ============================================================================

/// Outcome of comparing the manifest's stored HEAD against the current HEAD.
#[derive(Debug)]
pub(super) enum StalenessCheck {
    /// Index is up to date — stored HEAD matches current HEAD.
    Current,
    /// HEAD has advanced since the last index build.
    HeadChanged { stored: String, current: String },
    /// Manifest exists but was written without a git_head field
    /// (built by an older skim version, or a non-git project at build time).
    NoStoredHead,
    /// No index file found — treat as a cold start.
    NoIndex,
    /// Git HEAD is unchanged (or absent) but the working tree has uncommitted
    /// edits, additions, or deletions relative to the manifest (#379).
    ///
    /// Detected by a metadata-only scan (mtime + size) that runs ONLY after the
    /// cheap HEAD compare yields a Current-equivalent verdict (AD-379-5). The
    /// aggregate counts drive the `--stats` display and the rebuild log; no
    /// per-file path diff is retained (AD-379-9).
    WorkingTreeChanged {
        changed: usize,
        added: usize,
        removed: usize,
    },
}

impl std::fmt::Display for StalenessCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StalenessCheck::Current => write!(f, "current"),
            StalenessCheck::HeadChanged { stored, current } => write!(
                f,
                "stale (HEAD changed: {}…→{}…)",
                stored.get(..8).unwrap_or(stored),
                current.get(..8).unwrap_or(current),
            ),
            StalenessCheck::NoStoredHead => write!(f, "stale (no HEAD recorded)"),
            StalenessCheck::NoIndex => write!(f, "no index"),
            StalenessCheck::WorkingTreeChanged {
                changed,
                added,
                removed,
            } => write!(
                f,
                "stale (working tree changed: {changed} modified, {added} added, {removed} removed)",
            ),
        }
    }
}

// ============================================================================
// Working-tree staleness scan (#379)
// ============================================================================

/// Aggregate working-tree change counts produced by [`scan_working_tree`].
///
/// AD-379-9: only aggregate counts are retained, never a per-file path-set diff
/// (detailed per-path logging is a separate `--verbose` follow-up ticket).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WorkingTreeDelta {
    /// Indexed files whose on-disk mtime OR size differs from the manifest.
    pub changed: usize,
    /// Files present on disk (under the builder's ignore config) but absent
    /// from the manifest.
    pub added: usize,
    /// Files recorded in the manifest but no longer present on disk.
    pub removed: usize,
}

impl WorkingTreeDelta {
    /// `true` when the working tree differs from the manifest in any dimension.
    pub fn is_dirty(self) -> bool {
        self.changed != 0 || self.added != 0 || self.removed != 0
    }
}

/// Scan the working tree under `root` and compare each indexed file's metadata
/// (mtime AND size) against the `manifest`.
///
/// Runs a metadata-only walk via [`super::walk::walk_metadata`] (AD-379-1: the
/// SAME ignore-config walk the rebuild uses, so the scanned file set is exactly
/// what a rebuild would index — no subprocess, no `git status` parsing). For
/// each walked file the normalized rel-path is the manifest key
/// ([`super::walk::normalize_rel_path`]); the comparison classifies it as:
///
/// - **added** — path not present in the indexed-entry map AND not present in
///   the persisted skip set (genuinely new file). AD-395-5: a file present in
///   the skip set with unchanged mtime+size is treated as **neither added nor
///   changed** — this is the real fix for the infinite refresh loop (#395).
/// - **changed** — path present in the indexed-entry map but mtime OR size
///   differs (AD-379-2: size closes the same-second-edit gap; a `None` stored
///   hint forces the changed verdict so the field is repopulated on rebuild);
///   OR path present in the skip set but mtime or size differs (changed skip →
///   exactly one rebuild, then stable again).
///
/// Manifest paths not seen during the walk are counted as **removed**.
/// OD-395-5: a None/None-hint skip is reconciled by path presence alone — the
/// loop is killed even where the filesystem exposes no mtime/size hints.
///
/// # Performance (AC15 / ADR-003)
///
/// Metadata/stat only — zero file content reads and zero SHA. A clean tree
/// yields a `WorkingTreeDelta` with all-zero counts (`is_dirty() == false`).
///
/// # Errors
///
/// Propagates only fatal walker-setup errors from `walk_metadata`. Per-file
/// metadata errors are absorbed by the walker (collected as skip reasons that
/// are not consulted here).
fn scan_working_tree(
    root: &Path,
    manifest: &FileManifest,
    max_files: usize,
) -> anyhow::Result<WorkingTreeDelta> {
    use std::collections::HashMap;

    use super::walk::{normalize_rel_path, walk_metadata};

    // Metadata-only walk under the builder's ignore config (AD-379-1).
    let (entries, _skipped) = walk_metadata(root, max_files, None)?;

    // Index the manifest by normalized rel-path → (mtime, size). The key is
    // already normalized (it is the stored manifest key), so no re-normalization.
    let mut manifest_index: HashMap<&str, (Option<u64>, Option<u64>)> = HashMap::new();
    for (path, mtime, size) in manifest.freshness_entries() {
        manifest_index.insert(path, (mtime, size));
    }

    // AD-395-5: Build a skip-index from the persisted content-skip set.
    // A walked file whose path matches a persisted skip with unchanged mtime+size
    // is counted as NEITHER added NOR changed — the real fix for the infinite
    // refresh loop that a FORMAT_VERSION bump alone does NOT resolve.
    //
    // OD-395-5: a None/None-hint skip is matched by PATH PRESENCE alone (the
    // loop is killed even where the filesystem exposes neither hint), intentionally
    // diverging from the indexed-entry `None → changed` semantics.  A changed skip
    // (mtime or size differs) counts as `changed` → exactly one rebuild.
    let mut skip_index: HashMap<&str, (Option<u64>, Option<u64>)> = HashMap::new();
    for (path, mtime, size) in manifest.skip_freshness_entries() {
        skip_index.insert(path, (mtime, size));
    }

    // Track which manifest paths we observe on disk so the remainder are deletions.
    let mut seen: std::collections::HashSet<&str> =
        std::collections::HashSet::with_capacity(manifest_index.len());

    let mut changed = 0usize;
    let mut added = 0usize;

    for entry in &entries {
        let key = normalize_rel_path(&entry.rel_path);
        // Single lookup: get_key_value yields the stored &str key so `seen`
        // borrows the manifest (not the freshly-allocated `key` String).
        match manifest_index.get_key_value(key.as_str()) {
            None => {
                // Not in the indexed-entry map. Check the skip-index.
                // AD-395-5: if the file is a persisted skip with unchanged
                // mtime+size, treat it as neither added nor changed (loop-killer).
                // OD-395-5: None/None hint → match by path presence alone.
                match skip_index.get(key.as_str()) {
                    Some(&(s_mtime, s_size)) => {
                        // Path is present in the skip-set.
                        // OD-395-5: None-hint → treat as unchanged (loop killed even without mtime/size).
                        let mtime_unchanged =
                            s_mtime.is_none_or(|stored| entry.mtime == Some(stored));
                        let size_unchanged = s_size.is_none_or(|stored| entry.size == Some(stored));
                        // AD-395-5: unchanged skip → neither added nor changed (loop-killer).
                        // Changed skip → one rebuild so the file isn't frozen in skip state.
                        if !mtime_unchanged || !size_unchanged {
                            changed += 1;
                        }
                    }
                    None => {
                        // Not in either map → genuinely new file.
                        added += 1;
                    }
                }
            }
            Some((stored_key, &(m_mtime, m_size))) => {
                seen.insert(stored_key);
                // AD-379-2: an indexed file is changed when EITHER mtime or size
                // differs. A `None` stored hint (pre-#379 manifest) forces the
                // changed verdict so the field is repopulated on the rebuild (AC10).
                let mtime_differs = match m_mtime {
                    Some(stored) => entry.mtime != Some(stored),
                    None => true,
                };
                let size_differs = match m_size {
                    Some(stored) => entry.size != Some(stored),
                    None => true,
                };
                if mtime_differs || size_differs {
                    changed += 1;
                }
            }
        }
    }

    // Removed = manifest entries never observed during the walk.
    // Note: skip entries are tracked separately and a vanished skip needs no
    // rebuild (it is simply gone), so removed_count is unchanged (AD-395-5).
    let removed = manifest_index.len() - seen.len();

    Ok(WorkingTreeDelta {
        changed,
        added,
        removed,
    })
}

// ============================================================================
// Staleness check
// ============================================================================

/// Compare the manifest's stored git HEAD against the current HEAD.
///
/// Returns the staleness outcome alongside the loaded manifest (when one
/// exists and was successfully parsed). Callers can consume the manifest
/// directly rather than re-loading it.
///
/// # Staleness rules
///
/// | stored HEAD  | current HEAD | outcome               |
/// |-------------|-------------|----------------------|
/// | absent       | absent       | `Current` (non-git, no change possible) |
/// | absent       | present      | `NoStoredHead` (git repo appeared; rebuild) |
/// | present      | absent       | `Current` (git unreadable, assume unchanged) |
/// | present      | present      | `Current` or `HeadChanged` (compare) |
///
/// Returns [`StalenessCheck::NoIndex`] when no `index.skidx` file exists in
/// `cache_dir` (cold start — index has never been built).
///
/// Returns [`StalenessCheck::NoStoredHead`] only when the manifest has no
/// stored HEAD **and** the project is currently a git repo (i.e. git HEAD
/// appeared since the last build — rebuild is warranted).
///
/// # AST self-heal (#199)
///
/// When the lexical index is CURRENT but the AST index is ABSENT or has a
/// FORMAT_VERSION below the current version (post-upgrade / crash-between-builds),
/// this function reports `NoStoredHead` so the next query triggers a full rebuild.
/// The version check uses [`rskim_search::AstIndexReader::index_version`] which
/// reads only the first 6 bytes of `ast_index.skidx` (magic + version) — cheap,
/// no mmap, no CRC verification.
///
/// # Lexical self-heal (ADR-006, #355 Finding 9)
///
/// `#355` bumped the LEXICAL index FORMAT_VERSION v2→v3 (bigram→trigram).  Without
/// this check, a user with an unchanged git HEAD and a v2 `index.skidx` would get a
/// hard error from `NgramIndexReader::open` ("unsupported format version: 2; please
/// rebuild the index") instead of an automatic rebuild.  This check reads only the
/// first 6 bytes of `index.skidx` (same cheap approach as the AST version check) and
/// reports `NoStoredHead` when the lexical version is below the current version so the
/// next query self-heals via a full rebuild — matching the documented ADR-006 intent.
pub(super) fn check_staleness(
    cache_dir: &Path,
    project_root: &Path,
) -> (StalenessCheck, Option<FileManifest>) {
    // Cold start: no lexical index file.
    let index_path = cache_dir.join("index.skidx");
    if !index_path.exists() {
        return (StalenessCheck::NoIndex, None);
    }

    // Lexical self-heal: if the on-disk FORMAT_VERSION is older than the current
    // version, return NoStoredHead to trigger a full rebuild so the user does not
    // see a hard error from NgramIndexReader::open (ADR-006, #355 Finding 9).
    // This is the exact mirror of the AST index_version check below.
    let lexical_stale = match rskim_search::NgramIndexReader::lexical_index_version(cache_dir) {
        Ok(v) => v < rskim_search::LEXICAL_INDEX_FORMAT_VERSION,
        Err(_) => true, // Corrupt / unreadable → rebuild.
    };

    // AST self-heal: if the lexical index exists but the AST index is absent
    // or has an old format version, report stale so both rebuild atomically.
    // This handles: post-upgrade (v1→v2), crash between lexical.build() and
    // ast.build(), first run after adding --ast to an existing install, and
    // coverage-policy changes that change which files are AST-indexed.
    // #405 (AD-405-15): AST_INDEX_FORMAT_VERSION bumped 2→3 for the 100 KiB→1 MiB
    // size-cap raise; a v2 index is stale and triggers a full cold rebuild
    // (skcache CACHE_FORMAT_VERSION also bumped 1→2 in ast_cache.rs so the
    // rebuild re-extracts every file from source rather than serving stale empty
    // entries from the SHA-keyed skcache — see AD-405-14).
    let ast_index_path = cache_dir.join("ast_index.skidx");
    let ast_stale = if !ast_index_path.exists() {
        true
    } else {
        match rskim_search::AstIndexReader::index_version(cache_dir) {
            Ok(v) => v < rskim_search::AST_INDEX_FORMAT_VERSION,
            Err(_) => true, // Corrupt / unreadable → rebuild.
        }
    };

    // Manifest self-heal: if the on-disk manifest has an old FORMAT_VERSION,
    // report stale to trigger a rebuild (AD-373-3). This handles manifest
    // version upgrades (e.g., 2→3 after the FileId ordering fix).
    let manifest_stale = match FileManifest::version_matches(cache_dir) {
        Ok(matches) => !matches,
        Err(_) => true, // Unreadable → rebuild.
    };

    let manifest = match FileManifest::load(project_root.to_path_buf(), cache_dir.to_path_buf()) {
        Ok(m) => m,
        // Cannot load the manifest — treat as no stored HEAD.
        Err(_) => return (StalenessCheck::NoStoredHead, None),
    };

    if lexical_stale || ast_stale || manifest_stale {
        // Lexical, AST, or manifest index is absent or below the current format version.
        // Return NoStoredHead to trigger a full rebuild, but carry the loaded
        // manifest so display consumers (e.g. `--stats`) still show the real HEAD.
        return (StalenessCheck::NoStoredHead, Some(manifest));
    }

    let stored = manifest.stored_git_head().map(str::to_string);

    // Read current HEAD.
    let current = read_git_head(project_root);

    // AD-379-5: the working-tree scan runs ONLY after the cheap HEAD compare
    // yields a Current-equivalent verdict — never on NoIndex/NoStoredHead/
    // HeadChanged (AC8). On those stale branches a rebuild already happens, so
    // scanning would be redundant work. `current_or_working_tree` upgrades a
    // would-be `Current` outcome to `WorkingTreeChanged` when the metadata scan
    // finds ≥1 uncommitted change/add/remove (AD-379-3: this also covers the
    // non-git `(None, None)` branch and AD-379-6: the git-unreadable
    // `(Some, None)` branch — both reach it).
    let current_or_working_tree = |manifest: &FileManifest| -> StalenessCheck {
        // Use the SAME cap the builder uses so the scanned file set matches a
        // rebuild's set exactly (AD-379-1).
        let max_files = super::types::IndexConfig::DEFAULT_MAX_FILES;
        match scan_working_tree(project_root, manifest, max_files) {
            Ok(delta) if delta.is_dirty() => StalenessCheck::WorkingTreeChanged {
                changed: delta.changed,
                added: delta.added,
                removed: delta.removed,
            },
            // Clean tree, or scan failed (degrade to Current — a scan failure
            // must not falsely force a rebuild; the next query retries).
            _ => StalenessCheck::Current,
        }
    };

    // AD-413-14-OD: an ADOPTED root (no `.git` entry of its own, HEAD comes from an
    // enclosing repository via resolve_repo_toplevel) must NOT use HEAD-based
    // invalidation.  A commit anywhere in the enclosing repo changes the repo HEAD
    // even when no files under the subtree changed, so `HeadChanged` would trigger a
    // full lexical+temporal rebuild on every unrelated commit (e.g. a commit to
    // `crates/rskim` when `--root crates/rskim-search` is in use).  For adopted roots
    // the working-tree metadata scan already detects any real change under the subtree
    // (mtime + size, same as the standard path), so HEAD-based invalidation is both
    // redundant and over-broad.  `resolve_git_dir` returning `None` is the correct
    // adopted-root signal: the root has no `.git` file/dir of its own, so its HEAD
    // comes from the ancestor walk and may advance on any unrelated commit.
    let is_adopted_root = resolve_git_dir(project_root).is_none();

    let outcome = match (stored.as_deref(), current.as_deref()) {
        // Non-git project (both None): no commit can have changed, but the
        // working tree still can — scan it (AD-379-3).
        (None, None) => current_or_working_tree(&manifest),
        // Git repo appeared since last build — rebuild to record HEAD.
        (None, Some(_)) => StalenessCheck::NoStoredHead,
        // Git is unreadable (worktree detached, submodule, fs error).
        // Stored HEAD exists so the project was a git repo at build time; trust
        // is broken, so scan the working tree and rebuild on any edit to recover
        // (AD-379-6) rather than serving a possibly-stale index unconditionally.
        (Some(_), None) => current_or_working_tree(&manifest),
        // Both present — compare HEADs first, then the working tree on a match.
        (Some(s), Some(c)) => {
            if s == c {
                current_or_working_tree(&manifest)
            } else if is_adopted_root {
                // AD-413-14-OD: adopted root — HEAD diverged because a commit in the
                // enclosing repo advanced the repo HEAD, but that commit may have touched
                // nothing under this subtree.  Scope invalidation to the working-tree
                // scan so that only changes actually landing under `project_root` drive
                // a rebuild (avoids full rebuild on every unrelated commit).
                current_or_working_tree(&manifest)
            } else {
                StalenessCheck::HeadChanged {
                    stored: s.to_string(),
                    current: c.to_string(),
                }
            }
        }
    };

    (outcome, Some(manifest))
}

// ============================================================================
// Auto-refresh
// ============================================================================

/// What kind of build (if any) [`auto_refresh_if_stale`] performed.
///
/// Callers that only need to know *whether* a build ran should call
/// [`RefreshOutcome::refreshed`].  Callers that need to distinguish a first
/// build (no prior index) from an incremental refresh should use
/// [`RefreshOutcome::is_first_build`].
///
/// The AST coverage notice cadence (D-4 / AC-405-8) is the primary consumer
/// of this distinction: it fires on `FirstBuild` and is silent on `Incremental`.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum RefreshOutcome {
    /// Index was current; no rebuild was needed.
    UpToDate,
    /// First build: no prior index existed (`NoIndex` staleness variant).
    /// The AST coverage notice **must** fire on this outcome (D-4 cadence).
    FirstBuild,
    /// Incremental refresh: the index existed but was stale (`HeadChanged`,
    /// `NoStoredHead`, or `WorkingTreeChanged`).  The AST coverage notice
    /// **must be silent** on this outcome (AC-405-8).
    Incremental,
}

impl RefreshOutcome {
    /// Returns `true` when any build ran — either first or incremental.
    pub fn refreshed(&self) -> bool {
        !matches!(self, RefreshOutcome::UpToDate)
    }

    /// Returns `true` **only** for the very first index build (`NoIndex`).
    ///
    /// Use this instead of [`refreshed`](Self::refreshed) when the caller
    /// must distinguish a first build from an incremental refresh — e.g.
    /// to decide whether to emit the AST coverage notice.
    pub fn is_first_build(&self) -> bool {
        matches!(self, RefreshOutcome::FirstBuild)
    }
}

/// Check for staleness and rebuild the index if needed.
///
/// Returns `(outcome, manifest)` where:
/// - `outcome` is [`RefreshOutcome::UpToDate`] when the index was already
///   current, [`RefreshOutcome::FirstBuild`] after a first-time (NoIndex)
///   build, and [`RefreshOutcome::Incremental`] after any incremental refresh
///   (HeadChanged / NoStoredHead / WorkingTreeChanged).
/// - `manifest` is the [`FileManifest`] loaded from disk after any rebuild,
///   ready for callers (e.g. query execution) to use without a second load.
///
/// This is a convenience wrapper for the query path: call it before opening
/// the reader so callers always get a fresh index.
///
/// # HEAD threading (O-A / #289)
///
/// `read_git_head(root)` is called ONCE at function entry and the result is
/// threaded into `rebuild_temporal`. Note that `check_staleness` also calls
/// `read_git_head` internally — both calls are advisory and safe because the
/// lexical manifest records the HEAD that `build_index` writes, and
/// `rebuild_temporal` records the HEAD passed here. If a commit lands between
/// the two reads the manifest will record the pre-commit HEAD and temporal.db
/// will record the post-commit HEAD; both will appear stale on the next query,
/// triggering one more refresh. This is the accepted TOCTOU trade-off.
/// AD-413 Finding 2: returns the `HeadState` resolved at function entry so
/// temporal-consuming call sites do not need a second `git_head_state` call.
/// All four call sites in `mod.rs` (--ast arm, `run_update`, `execute_query`,
/// `run_temporal_standalone`) previously discarded this value and re-called
/// `git_head_state` right after returning, triggering an extra full HEAD-ladder
/// traversal on every query (three traversals instead of two on a linked
/// worktree with packed-refs).
pub(super) fn auto_refresh_if_stale(
    root: &Path,
    cache_dir: &Path,
    _analytics: &crate::analytics::AnalyticsConfig,
    reanchor: ReanchorPolicy,
) -> anyhow::Result<(RefreshOutcome, FileManifest, HeadState)> {
    use super::index::{build_index, build_index_rechecked};
    use super::types::IndexConfig;

    // Classify git HEAD state once at function entry so rebuild_temporal records
    // the same SHA that will be in the manifest after build_index runs.
    // Step 6 (AD-413-7): use the three-state HeadState rather than Option<String>
    // so the same read feeds both the temporal rebuild and the anchor check.
    let head_state = git_head_state(root);
    let current_head: Option<&str> = head_state.sha();

    let (staleness, existing_manifest) = check_staleness(cache_dir, root);

    if matches!(staleness, StalenessCheck::Current) {
        // Index is current — return the manifest we already loaded.
        let manifest = existing_manifest.unwrap_or_else(|| {
            // Defensive fallback: should not happen (Current implies manifest loaded).
            FileManifest::new(root.to_path_buf(), cache_dir.to_path_buf())
        });

        // AD-TMP-2: temporal.db has its own staleness gate, independent of
        // lexical staleness (#357 BUG B). The lexical index is current, but
        // temporal.db may be missing or HEAD-divergent (post-upgrade, manual
        // delete, or 2nd+ query after a --rebuild that predated this fix).
        // Check and self-heal here BEFORE the early return, so that a bare
        // `skim search --hot` (routed via auto_refresh_if_stale) always has
        // fresh temporal data when the lexical index is current.
        // Non-fatal by ADR-006/D5: temporal failure must NOT fail the query.
        //
        // Guard ordering (#357 cycle-2 finding 19): `let Some(head)` is evaluated
        // FIRST (short-circuits on non-git dirs where current_head=None BEFORE the
        // temporal_db_is_stale() call, avoiding a wasted DB open).
        // `temporal_db_is_stale` only runs when HEAD is readable.
        if let Some(head) = current_head
            && temporal_db_is_stale(cache_dir, head)
        {
            try_rebuild_temporal_nonfatal(root, cache_dir, Some(head), "self-heal", reanchor);
        }

        return Ok((RefreshOutcome::UpToDate, manifest, head_state));
    }

    // All rebuild paths share the same config.
    let config = IndexConfig {
        root: root.to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache_dir.to_path_buf()),
    };

    // Determine whether this is a first build (NoIndex) before moving `staleness`
    // into the match below, so the outcome can be tagged correctly afterward.
    let is_no_index = matches!(staleness, StalenessCheck::NoIndex);

    // Tracks whether a pipeline build actually ran. Every arm below rebuilds
    // unconditionally EXCEPT WorkingTreeChanged, which may skip the rebuild when
    // a concurrent peer already refreshed the index (AD-379-8). When the build is
    // skipped we must report `UpToDate` and skip the post-rebuild temporal hook
    // (nothing was rebuilt), so the steady-state no-op contract (AC7/AC14) holds.
    let mut build_ran = true;

    match staleness {
        StalenessCheck::HeadChanged { stored, current } => {
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search [debug]: index stale — HEAD changed ({}…→{}…); rebuilding",
                    stored.get(..8).unwrap_or(&stored),
                    current.get(..8).unwrap_or(&current),
                );
            }
            build_index(&config)?;
        }
        StalenessCheck::NoStoredHead => {
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search [debug]: index stale — no stored HEAD or format upgrade; rebuilding",
                );
            }
            build_index(&config)?;
        }
        StalenessCheck::NoIndex => {
            build_index(&config)?;
        }
        StalenessCheck::WorkingTreeChanged {
            changed,
            added,
            removed,
        } => {
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search [debug]: index stale — working tree changed \
                     ({changed} modified, {added} added, {removed} removed); rebuilding",
                );
            }
            // AD-379-8: a concurrent peer may have already rebuilt the index.
            // `build_index_rechecked` re-runs check_staleness under the build lock
            // and skips the rebuild if the index is now current.
            let built = build_index_rechecked(&config, || {
                // Re-evaluate staleness under the lock: skip the rebuild unless the
                // working tree is STILL dirty (a peer may have already rebuilt).
                matches!(
                    check_staleness(cache_dir, root).0,
                    StalenessCheck::WorkingTreeChanged { .. }
                )
            })?;
            build_ran = built.is_some();
        }
        StalenessCheck::Current => {
            // Already handled above — unreachable here.
            unreachable!("Current branch handled before staleness match");
        }
    }

    if !build_ran {
        // Concurrent peer already refreshed the index — reload the manifest and
        // return UpToDate (steady-state no-op contract AC7/AC14).
        let manifest = existing_manifest
            .unwrap_or_else(|| FileManifest::new(root.to_path_buf(), cache_dir.to_path_buf()));
        return Ok((RefreshOutcome::UpToDate, manifest, head_state));
    }

    // After a rebuild, load the freshly written manifest for the caller.
    // This manifest was written by `build_index` and records `current_head`.
    let manifest = FileManifest::load(root.to_path_buf(), cache_dir.to_path_buf())?;

    // ── #289 temporal build hook point ───────────────────────────────────────
    // Populate temporal.db AFTER the lexical+AST manifest is persisted.
    // (applies ADR-006: temporal is a derived satellite; must not be written
    // off a half-built index)
    //
    // `rebuild_temporal` acquires its own bounded `.skim-build.lock` around
    // the parse+sync phase and degrades gracefully on non-git dirs, gix errors,
    // or CapacityExceeded — a temporal failure MUST NOT fail the lexical refresh.
    //
    // `head` is the HEAD SHA read at function entry above. Passing `None` when
    // the project is non-git: try_rebuild_temporal_nonfatal no-ops gracefully.
    // `reanchor` is threaded from the caller: only the explicit build arms
    // (`--build`, `--rebuild`, `--update`) pass `Allow`; query-triggered refreshes
    // pass `Refuse` so anchor mismatch leaves temporal.db untouched (PF-017).
    try_rebuild_temporal_nonfatal(root, cache_dir, current_head, "post-rebuild", reanchor);
    // ─────────────────────────────────────────────────────────────────────────

    let outcome = if is_no_index {
        RefreshOutcome::FirstBuild
    } else {
        RefreshOutcome::Incremental
    };
    Ok((outcome, manifest, head_state))
}

// ============================================================================
// Shared test helpers (visible within cmd::search via pub(super))
// ============================================================================

/// Create a real git repository with commits.
///
/// Canonical shared helper used by `staleness_tests.rs`, `temporal_build_tests.rs`,
/// and `mod.rs` test modules — eliminates the three near-verbatim copies that would
/// otherwise drift independently (see #357 cycle-2 findings 9/14, and the plan's
/// step 6 recommendation). `pub(super)` makes it accessible to all `#[cfg(test)]`
/// users within `crate::cmd::search` via `super::staleness::create_real_git_repo`.
///
/// For tests that need per-commit date control, use [`create_real_git_repo_with_dates`].
///
/// Returns the full 40-hex SHA of HEAD.
#[cfg(test)]
#[allow(clippy::type_complexity)]
pub(super) fn create_real_git_repo(
    dir: &std::path::Path,
    commit_files: &[(&str, &[(&str, &str)])],
) -> String {
    let with_dates: Vec<(&str, Option<&str>, &[(&str, &str)])> = commit_files
        .iter()
        .map(|(msg, files)| (*msg, None, *files))
        .collect();
    create_real_git_repo_with_dates(dir, &with_dates)
}

/// Extended form of [`create_real_git_repo`] that accepts an optional per-commit
/// date string (e.g. `"2025-10-01 00:00:00 +0000"`) injected via
/// `GIT_AUTHOR_DATE` / `GIT_COMMITTER_DATE`.  When `date` is `None` the commit
/// is made with the current wall-clock time (same behaviour as
/// `create_real_git_repo`).
///
/// Prefer this over hand-rolling `Command::new("git")` add/commit blocks with
/// env-var date overrides in individual tests — it keeps all dated and undated
/// tests on the same shared setup path.
///
/// Returns the full 40-hex SHA of HEAD.
#[cfg(test)]
#[allow(clippy::type_complexity)]
pub(super) fn create_real_git_repo_with_dates(
    dir: &std::path::Path,
    commit_files: &[(&str, Option<&str>, &[(&str, &str)])],
) -> String {
    use std::fs;
    use std::process::Command;

    Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(dir)
        .output()
        .expect("git config email");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .expect("git config name");

    for (msg, date, files) in commit_files {
        for (name, content) in *files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create dir");
            }
            fs::write(&path, content).expect("write file");
            Command::new("git")
                .args(["add", name])
                .current_dir(dir)
                .output()
                .expect("git add");
        }
        let mut cmd = Command::new("git");
        cmd.args(["commit", "-m", msg]).current_dir(dir);
        if let Some(d) = date {
            cmd.env("GIT_AUTHOR_DATE", d).env("GIT_COMMITTER_DATE", d);
        }
        cmd.output().expect("git commit");
    }

    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("git rev-parse HEAD");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Create a linked git worktree rooted at `worktree` and checked out on `branch`.
///
/// Runs `git -C primary branch <branch>` (creates the branch from the current HEAD)
/// then `git -C primary worktree add <worktree> <branch>`, and returns the full 40-hex
/// SHA of `HEAD` as seen from the linked worktree.
///
/// **Hermeticity (I3/I5):** sets `GIT_CONFIG_GLOBAL=/dev/null` and
/// `GIT_CONFIG_SYSTEM=/dev/null` on every subprocess so ambient `core.hooksPath`,
/// `init.defaultBranch`, and `extensions.*` configuration does not bleed in.
/// Branch names may contain `/` (e.g. `wave/probe-413` — F12 slashed-branch coverage).
///
/// `primary` must already be an initialised git repository with at least one commit
/// (call [`create_real_git_repo`] first).  `worktree` must not yet exist.
///
/// `pub(super)` makes it accessible from all `#[cfg(test)]` modules within
/// `crate::cmd::search` via `super::staleness::create_real_git_worktree`.
#[cfg(test)]
pub(super) fn create_real_git_worktree(
    primary: &std::path::Path,
    worktree: &std::path::Path,
    branch: &str,
) -> String {
    use std::process::Command;

    // Create the branch at HEAD of the primary repository.
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(["branch", branch])
        .current_dir(primary)
        .output()
        .expect("git branch (spawn)");
    assert!(
        out.status.success(),
        "git branch {:?} failed: {}",
        branch,
        String::from_utf8_lossy(&out.stderr),
    );

    // Add the linked worktree checked out on that branch.
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .arg("worktree")
        .arg("add")
        .arg(worktree)
        .arg(branch)
        .current_dir(primary)
        .output()
        .expect("git worktree add (spawn)");
    assert!(
        out.status.success(),
        "git worktree add {:?} failed: {}",
        branch,
        String::from_utf8_lossy(&out.stderr),
    );

    // Return the resolved HEAD SHA from the linked worktree.
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(["rev-parse", "HEAD"])
        .current_dir(worktree)
        .output()
        .expect("git rev-parse HEAD in worktree");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Test-only helper: write a single `meta` key/value pair directly via raw SQL,
/// bypassing the `TemporalDb::set_meta` version-attestation guard (AD-408-3).
///
/// Tests need to construct adversarial persisted states — a half-attested DB
/// (`git_head` present, `data_version` absent), a corrupt/non-integer
/// `data_version`, or a future/legacy version — that the production `set_meta`
/// guard deliberately rejects with a `debug_assert!`. Those raw-bytes scenarios
/// (simulating a DB written by another / older / corrupt binary) belong at the
/// storage layer, not the guarded domain API.
///
/// Requires the `meta` table to already exist (created by `TemporalDb::open`)
/// and opens its own short-lived connection, so any live `TemporalDb` handle on
/// the same file must be dropped first to avoid write contention. `pub(super)`
/// makes it reachable from all `#[cfg(test)]` modules within
/// `crate::cmd::search` (`staleness_tests.rs`, `temporal_tests.rs`).
#[cfg(test)]
pub(super) fn plant_meta_raw(db_path: &std::path::Path, key: &str, value: &str) {
    let conn = rusqlite::Connection::open(db_path).expect("open temporal.db for meta plant");
    conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        rusqlite::params![key, value],
    )
    .expect("plant meta row");
}

/// Test-only re-export of `scan_working_tree` for AC9 / AC7 integration tests
/// that construct manifest state directly rather than going through `build_index`.
///
/// `pub(super)` makes it accessible from sibling test modules
/// (`index_tests.rs`, `staleness_tests.rs`) via `super::staleness::...`.
#[cfg(test)]
pub(super) fn scan_working_tree_test_hook(
    root: &std::path::Path,
    manifest: &super::manifest::FileManifest,
    max_files: usize,
) -> anyhow::Result<WorkingTreeDelta> {
    scan_working_tree(root, manifest, max_files)
}

// ============================================================================
// Tests (co-located in staleness_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "staleness_tests.rs"]
mod tests;
