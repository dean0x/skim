//! Staleness detection via git HEAD comparison.
//!
//! Compares the git HEAD commit recorded in the manifest (`index.skfiles`)
//! against the current git HEAD at query time.  When they diverge, the index
//! is stale and should be rebuilt.
//!
//! # Design
//!
//! - Pure file I/O — no git binary subprocess, no libgit2 dependency.
//! - Handles ordinary repos (`.git/` directory) and worktrees (`.git` file).
//! - Follows `ref: refs/heads/<branch>` symbolic refs with packed-refs fallback.
//! - All failures are soft: if we can't read git state we degrade gracefully.

use std::path::{Path, PathBuf};

use super::manifest::FileManifest;

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
        }
    }
}

// ============================================================================
// Git HEAD resolution
// ============================================================================

/// Resolve the git directory for a project root.
///
/// - If `.git` is a **directory**, returns it directly.
/// - If `.git` is a **file** (worktree), parses the `gitdir: <path>` pointer
///   and returns the resolved target path.
/// - Returns `None` when `.git` doesn't exist.
///
/// This mirrors git's own resolution logic for `git rev-parse --git-dir`.
pub(super) fn resolve_git_dir(project_root: &Path) -> Option<PathBuf> {
    let dot_git = project_root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    if dot_git.is_file() {
        // Worktree: .git is a file containing "gitdir: <absolute-or-relative-path>"
        let content = std::fs::read_to_string(&dot_git).ok()?;
        let gitdir_line = content.lines().find(|l| l.starts_with("gitdir:"))?;
        let target = gitdir_line.strip_prefix("gitdir:").map(str::trim)?;
        let target_path = PathBuf::from(target);
        if target_path.is_absolute() {
            Some(target_path)
        } else {
            // Relative to the directory containing the .git file
            Some(project_root.join(target_path))
        }
    } else {
        None
    }
}

/// Read the current git HEAD for `project_root`.
///
/// Resolution order:
/// 1. `resolve_git_dir(project_root)` — locate `.git` or follow the worktree pointer.
/// 2. Read `<git_dir>/HEAD`.
/// 3. If it is a symbolic ref (`ref: refs/heads/<branch>`):
///    a. Try `<git_dir>/<ref_path>` (loose ref).
///    b. Fall back to `<git_dir>/packed-refs`.
/// 4. If HEAD is a raw 40-hex SHA (detached HEAD), return it directly.
///
/// Returns `None` when:
/// - `.git` does not exist (not a git repo).
/// - Any I/O failure prevents reading the necessary files.
pub(super) fn read_git_head(project_root: &Path) -> Option<String> {
    let git_dir = resolve_git_dir(project_root)?;
    let head_content = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head_str = head_content.trim();

    if let Some(ref_path) = head_str.strip_prefix("ref: ") {
        // Validate the ref path to prevent path traversal attacks via a
        // crafted `.git/HEAD` (e.g. `ref: ../../etc/shadow`).
        if !ref_path.starts_with("refs/") {
            return None;
        }
        // Symbolic ref — resolve through loose refs then packed-refs
        resolve_symbolic_ref(&git_dir, ref_path)
    } else if is_hex_sha(head_str) {
        // Detached HEAD — raw SHA
        Some(head_str.to_string())
    } else {
        None
    }
}

/// Resolve a symbolic ref (e.g. `refs/heads/main`) to its SHA.
///
/// Tries the loose ref file first; falls back to `packed-refs`.
fn resolve_symbolic_ref(git_dir: &Path, ref_path: &str) -> Option<String> {
    // 1. Loose ref: <git_dir>/refs/heads/<branch>
    let loose_path = git_dir.join(ref_path);
    if let Ok(content) = std::fs::read_to_string(&loose_path) {
        let sha = content.trim().to_string();
        if is_hex_sha(&sha) {
            return Some(sha);
        }
    }

    // 2. packed-refs fallback
    let packed_refs_path = git_dir.join("packed-refs");
    if let Ok(content) = std::fs::read_to_string(&packed_refs_path) {
        for line in content.lines() {
            // Skip comment lines
            if line.starts_with('#') || line.starts_with('^') {
                continue;
            }
            // Format: "<sha> <ref>"
            let mut parts = line.splitn(2, ' ');
            if let (Some(sha), Some(name)) = (parts.next(), parts.next())
                && name.trim() == ref_path
                && is_hex_sha(sha)
            {
                return Some(sha.to_string());
            }
        }
    }

    None
}

/// Return `true` if `s` looks like a 40-character (SHA-1) or 64-character
/// (SHA-256) hex commit hash.
///
/// Git repos using `extensions.objectFormat = sha256` emit 64-hex-char hashes.
/// Accepting both lengths avoids silent staleness degradation in SHA-256 repos.
fn is_hex_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
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
    // ast.build(), and first run after adding --ast to an existing install.
    let ast_index_path = cache_dir.join("ast_index.skidx");
    let ast_stale = if !ast_index_path.exists() {
        true
    } else {
        match rskim_search::AstIndexReader::index_version(cache_dir) {
            Ok(v) => v < rskim_search::AST_INDEX_FORMAT_VERSION,
            Err(_) => true, // Corrupt / unreadable → rebuild.
        }
    };

    let manifest = match FileManifest::load(project_root.to_path_buf(), cache_dir.to_path_buf()) {
        Ok(m) => m,
        // Cannot load the manifest — treat as no stored HEAD.
        Err(_) => return (StalenessCheck::NoStoredHead, None),
    };

    if lexical_stale || ast_stale {
        // Lexical or AST index is absent or below the current format version.
        // Return NoStoredHead to trigger a full rebuild, but carry the loaded
        // manifest so display consumers (e.g. `--stats`) still show the real HEAD.
        return (StalenessCheck::NoStoredHead, Some(manifest));
    }

    let stored = manifest.stored_git_head().map(str::to_string);

    // Read current HEAD.
    let current = read_git_head(project_root);

    let outcome = match (stored.as_deref(), current.as_deref()) {
        // Non-git project (both None): nothing can have changed.
        (None, None) => StalenessCheck::Current,
        // Git repo appeared since last build — rebuild to record HEAD.
        (None, Some(_)) => StalenessCheck::NoStoredHead,
        // Git is unreadable (worktree detached, submodule, fs error).
        // Stored HEAD exists so the project was a git repo at build time;
        // assume the index is still valid rather than triggering a rebuild.
        (Some(_), None) => StalenessCheck::Current,
        // Both present — compare.
        (Some(s), Some(c)) => {
            if s == c {
                StalenessCheck::Current
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
// Temporal staleness helper
// ============================================================================

/// Return `true` when `temporal.db` is missing or its stored `META_GIT_HEAD`
/// does not match `current_head`.
///
/// Uses the same pure file-IO [`read_git_head`] approach as the rest of this
/// module — no subprocess, no `git rev-parse`.
///
/// # AD-TMP-2 / AD-TMP-3
///
/// AD-TMP-2: temporal.db staleness is INDEPENDENT of lexical staleness (#357
/// BUG B). The lexical-Current early-return in `auto_refresh_if_stale` (below)
/// skipped the temporal hook, so a missing or HEAD-divergent temporal.db stayed
/// stale forever while the lexical index was current (post-upgrade, manual
/// delete, or 2nd+ query after a temporal-less rebuild due to BUG A). This
/// helper checks temporal.db's stored META_GIT_HEAD against the `current_head`
/// already read at function entry in `auto_refresh_if_stale`. Self-heals the
/// stuck-stale (deadbeef) case. Non-fatal by ADR-006/D5.
///
/// AD-TMP-3: production temporal staleness uses file-IO HEAD comparison here,
/// not `check_temporal_staleness` from `temporal.rs` — that helper is
/// `#[cfg(test)]`-only and uses a `git rev-parse` subprocess, which is
/// inconsistent with this module's subprocess-free design. The `current_head`
/// parameter is the single HEAD read already performed at `auto_refresh_if_stale`
/// entry; passing it here avoids a second HEAD read and keeps one HEAD-reading
/// authority per call.
///
/// Returns `true` for `current_head = None` (non-git) — the caller handles the
/// None case before calling `rebuild_temporal` so this effectively degrades to
/// a no-op on non-git dirs via the caller's `if let Some(ref head)` guard.
pub(super) fn temporal_db_is_stale(cache_dir: &Path, current_head: Option<&str>) -> bool {
    let db_path = cache_dir.join("temporal.db");
    if !db_path.exists() {
        return true;
    }
    // Compare temporal.db's stored META_GIT_HEAD against current_head.
    let stored_head = rskim_search::TemporalDb::open(&db_path)
        .ok()
        .and_then(|db| db.get_meta(rskim_search::META_GIT_HEAD).ok().flatten());
    match (stored_head.as_deref(), current_head) {
        (Some(stored), Some(current)) => stored != current,
        // temporal.db exists but has no META_GIT_HEAD → stale.
        (None, _) => true,
        // current_head is None (non-git): treat as stale so caller's
        // `if let Some(ref head)` guard fires and no-ops via rebuild_temporal.
        (_, None) => true,
    }
}

// ============================================================================
// Auto-refresh
// ============================================================================

/// Check for staleness and rebuild the index if needed.
///
/// Returns `(refreshed, manifest)` where:
/// - `refreshed` is `true` when the index was rebuilt, `false` when already current.
/// - `manifest` is the [`FileManifest`] loaded from disk after any rebuild, ready
///   for callers (e.g. query execution) to use without a second load.
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
pub(super) fn auto_refresh_if_stale(
    root: &Path,
    cache_dir: &Path,
    _analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<(bool, FileManifest)> {
    use super::index::build_index;
    use super::temporal_build::{current_epoch_secs, rebuild_temporal};
    use super::types::IndexConfig;

    // Read the current git HEAD once at function entry so rebuild_temporal can
    // record the same SHA that will be in the manifest after build_index runs.
    let current_head: Option<String> = read_git_head(root);

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
        if temporal_db_is_stale(cache_dir, current_head.as_deref())
            && let Some(ref head) = current_head
            && let Err(e) = rebuild_temporal(root, cache_dir, head, current_epoch_secs())
            && crate::debug::is_debug_enabled()
        {
            eprintln!("skim search [debug]: temporal self-heal error (non-fatal): {e}");
        }

        return Ok((false, manifest));
    }

    // All rebuild paths share the same config.
    let config = IndexConfig {
        root: root.to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache_dir.to_path_buf()),
    };

    match staleness {
        StalenessCheck::Current => unreachable!(),
        StalenessCheck::NoIndex => {
            eprintln!("skim search: building index…");
            let result = build_index(&config)?;
            eprintln!(
                "skim search: indexed {} files in {:.1}s",
                result.file_count,
                result.duration.as_secs_f64()
            );
        }
        StalenessCheck::HeadChanged { stored, current } => {
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search [debug]: HEAD changed ({} -> {}), refreshing index…",
                    stored.get(..8).unwrap_or(&stored),
                    current.get(..8).unwrap_or(&current)
                );
            } else {
                eprintln!("skim search: index stale (HEAD changed), refreshing…");
            }
            build_index(&config)?;
        }
        StalenessCheck::NoStoredHead => {
            // Manifest exists but no HEAD recorded — could be an old build or
            // a git repo that appeared since the last non-git build.
            // Rebuild to get a fresh manifest with HEAD stored.
            eprintln!("skim search: refreshing index (no HEAD recorded)…");
            build_index(&config)?;
        }
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
    // the project is non-git: `rebuild_temporal` skips gracefully (parse_history
    // returns an error on a non-git dir anyway).
    if let Some(ref head) = current_head
        && let Err(e) = rebuild_temporal(root, cache_dir, head, current_epoch_secs())
    {
        // Ignore temporal errors — they must not fail the lexical/AST refresh (D5).
        if crate::debug::is_debug_enabled() {
            eprintln!("skim search [debug]: temporal rebuild error (non-fatal): {e}");
        }
    }
    // ─────────────────────────────────────────────────────────────────────────

    Ok((true, manifest))
}

// ============================================================================
// Tests (co-located in staleness_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "staleness_tests.rs"]
mod tests;
