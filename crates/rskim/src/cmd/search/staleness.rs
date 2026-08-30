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

/// AD-413-7: three states, deliberately NOT `Option<String>` — "not a git repo" and
/// "git repo whose HEAD I could not resolve" are different facts, and collapsing them
/// is what made #413 silent and its message wrong (avoids PF-016). A gitdir with no
/// `HEAD` file is `NotARepo`, not `Unresolved`, or `mkdir .git` gets the opposite lie.
#[derive(Debug, PartialEq)]
pub(super) enum HeadState {
    /// No `.git` entry found at `project_root` or any enclosing ancestor.
    NotARepo,
    /// git dir found and HEAD readable, but the commit SHA could not be resolved
    /// (unborn branch, unsupported ref backend, corrupt HEAD, fs error).
    Unresolved,
    /// git dir found and HEAD successfully resolved to a commit SHA.
    Resolved(String),
}

impl HeadState {
    /// Return the commit SHA when resolved, or `None` for `NotARepo` / `Unresolved`.
    pub(super) fn sha(&self) -> Option<&str> {
        match self {
            HeadState::Resolved(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// AD-413-14: walk up from `project_root` to the nearest enclosing git repository
/// when `project_root` itself has no `.git` entry of its own.
///
/// Returns `None` when:
/// - `project_root` already has a `.git` entry (never re-point it — AC17).
/// - No enclosing repo is found within the `MAX_ANCESTORS` bound.
/// - The first-match ancestor does not contain `project_root` (containment check).
/// - The ancestor's git directory has no readable `HEAD` file (F10).
///
/// This is what makes `--root <subdirectory>` adopt the enclosing repo's HEAD
/// instead of returning nothing (OD-3, A9).  Reuses the same bounded walk
/// that [`super::walk::discover_project_root`] uses, keeping the two callers
/// consistent.
pub(super) fn resolve_repo_toplevel(project_root: &Path) -> Option<PathBuf> {
    // Never re-point a root that claims to be a repository already (AC17).
    if project_root.join(".git").exists() {
        return None;
    }
    let canonical = project_root.canonicalize().ok()?;
    // Bounded ancestor walk (same MAX_ANCESTORS constant as the builder walk).
    let top = super::walk::discover_project_root(&canonical).ok()?;
    // `discover_project_root` returns the start path when no enclosing repo is found.
    if top == canonical {
        return None;
    }
    // Containment: `project_root` must live inside the discovered toplevel.
    if !canonical.starts_with(&top) {
        return None;
    }
    // F10: the toplevel must have a git dir with a readable HEAD file.
    let git_dir = resolve_git_dir(&top)?;
    git_dir.join("HEAD").is_file().then_some(top)
}

/// Classify the git HEAD state for `project_root`.
///
/// Resolution order:
/// 1. `resolve_git_dir(project_root)` — locate `.git` or follow the worktree pointer.
/// 2. If that fails: `resolve_repo_toplevel(project_root).and_then(resolve_git_dir)` —
///    walk up to the nearest enclosing repo (enables `--root <subdirectory>`, AD-413-14).
/// 3. Read `<git_dir>/HEAD`.
/// 4. If a symbolic ref (`ref: refs/heads/<branch>`): validate via AD-413-6 guard,
///    then `resolve_symbolic_ref` (4-probe ladder, AD-413-4/5, including commondir).
/// 5. If a raw 40/64-hex SHA (detached HEAD): return `Resolved` directly.
/// 6. Otherwise: `Unresolved` (unborn branch, unsupported ref backend, corrupt HEAD).
pub(super) fn git_head_state(project_root: &Path) -> HeadState {
    let Some(git_dir) = resolve_git_dir(project_root)
        .or_else(|| resolve_repo_toplevel(project_root).and_then(|t| resolve_git_dir(&t)))
    else {
        return HeadState::NotARepo;
    };
    // F10: a gitdir with no HEAD file is NOT a repo — do not emit the opposite lie.
    let Ok(content) = std::fs::read_to_string(git_dir.join("HEAD")) else {
        return HeadState::NotARepo;
    };
    let head_str = content.trim();
    if let Some(ref_path) = head_str.strip_prefix("ref: ") {
        // AD-413-6: a symbolic HEAD must both start with `refs/` AND pass
        // `crate::cmd::is_repo_relative_safe` (the ADR-008 canonical guard). The prefix check
        // alone let `ref: refs/../../../outside-sha` read a file outside the root and PERSIST it
        // into `index.skfiles` and `temporal.db`'s `META_GIT_HEAD` (measured, #413).
        if !ref_path.starts_with("refs/") || !crate::cmd::is_repo_relative_safe(Path::new(ref_path))
        {
            return HeadState::Unresolved;
        }
        match resolve_symbolic_ref(&git_dir, ref_path) {
            Some(sha) => HeadState::Resolved(sha),
            None => HeadState::Unresolved,
        }
    } else if is_hex_sha(head_str) {
        HeadState::Resolved(head_str.to_string())
    } else {
        HeadState::Unresolved
    }
}

/// Read the current git HEAD SHA for `project_root`.
///
/// Resolution order (AD-413-7):
/// 1. `git_head_state(project_root)` — full state resolution including linked-worktree
///    commondir ladder (AD-413-4/5) and subdirectory ancestor walk (AD-413-14).
/// 2. Returns the SHA from `HeadState::Resolved`, or `None` for `NotARepo`/`Unresolved`.
///
/// Returns `None` when:
/// - No `.git` exists at or above `project_root`.
/// - HEAD is readable but the commit SHA cannot be resolved (unborn branch,
///   unsupported ref backend, corrupt HEAD, fs error).
///   Call `git_head_state` directly when you need to distinguish `NotARepo`
///   from `Unresolved` (e.g. for advisory messages or anchor checks).
pub(super) fn read_git_head(project_root: &Path) -> Option<String> {
    match git_head_state(project_root) {
        HeadState::Resolved(sha) => Some(sha),
        _ => None,
    }
}

/// Read a loose ref from `dir` (e.g. `dir/refs/heads/main`).
///
/// Returns `None` when the file is absent, unreadable, or its content is not
/// a valid 40/64-hex commit SHA.
fn read_loose_ref(dir: &Path, ref_path: &str) -> Option<String> {
    let loose_path = dir.join(ref_path);
    if let Ok(content) = std::fs::read_to_string(&loose_path) {
        let sha = content.trim().to_string();
        if is_hex_sha(&sha) {
            return Some(sha);
        }
    }
    None
}

/// Scan `dir/packed-refs` for the SHA assigned to `ref_path`.
///
/// Returns `None` when the file is absent, unreadable, or the ref is not
/// listed.
fn read_packed_ref(dir: &Path, ref_path: &str) -> Option<String> {
    let packed_refs_path = dir.join("packed-refs");
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

/// Maximum bytes read from a `commondir` pointer file.
///
/// AD-413-3: `commondir` is untrusted input (applies ADR-008): the read is capped at
/// `MAX_COMMONDIR_BYTES`, the target is canonicalized, and it must be a directory
/// containing `HEAD`. A sanity gate, not a sandbox — a real commondir lives outside the root.
const MAX_COMMONDIR_BYTES: u64 = 4096;

/// AD-413-1: reads a linked worktree's `commondir` pointer, which names the SHARED
/// ref store. `refs/heads/*`, `refs/tags/*`, `refs/remotes/*` and `packed-refs` live
/// there — the worktree-private gitdir's `refs/` is empty (measured, #413).
fn resolve_common_dir(git_dir: &Path) -> Option<PathBuf> {
    use std::io::Read as _;
    let file = std::fs::File::open(git_dir.join("commondir")).ok()?;
    let mut buf = String::new();
    if file
        .take(MAX_COMMONDIR_BYTES)
        .read_to_string(&mut buf)
        .is_err()
    {
        if crate::debug::is_debug_enabled() {
            eprintln!(
                "skim search [debug]: commondir unreadable in {}",
                git_dir.display()
            );
        }
        return None;
    }
    let first = buf.lines().next()?.trim();
    if first.is_empty() {
        return None;
    }
    let raw = PathBuf::from(first);
    // AD-413-2: a relative `commondir` resolves against the WORKTREE GITDIR (git's
    // default content is `"../.."`); an absolute one is used as-is. Same `is_absolute()`
    // branch shape as `resolve_git_dir`, DIFFERENT anchor: `commondir` resolves against
    // `git_dir`, not `project_root` (staleness.rs:99) — anchoring on `project_root` lands
    // two levels above the worktree root.
    let joined = if raw.is_absolute() {
        raw
    } else {
        git_dir.join(raw)
    };
    let canonical = match joined.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            if crate::debug::is_debug_enabled() {
                eprintln!(
                    "skim search [debug]: commondir target is not a git dir ({})",
                    joined.display()
                );
            }
            return None;
        }
    };
    if !canonical.is_dir() || !canonical.join("HEAD").is_file() {
        // AD-413-3 sanity gate: the target must be a directory containing HEAD.
        if crate::debug::is_debug_enabled() {
            eprintln!(
                "skim search [debug]: commondir target is not a git dir ({})",
                canonical.display()
            );
        }
        return None;
    }
    Some(canonical)
}

/// Per-worktree ref namespaces — these are never redirected to the common dir.
const PER_WORKTREE_REF_PREFIXES: [&str; 3] = ["refs/bisect/", "refs/worktree/", "refs/rewritten/"];

/// Resolve a symbolic ref (e.g. `refs/heads/main`) to its SHA.
///
/// AD-413-4: four probes in order — worktree loose, commondir loose, commondir
/// `packed-refs` (mandatory: the post-`git gc` steady state), worktree `packed-refs`.
/// `refs/bisect|worktree|rewritten/*` stop at probe 1: git keeps those per-worktree.
///
/// AD-413-5: probe 1 stays FIRST and probe 4 stays LAST, and a plain repo or submodule
/// has no `commondir`, so probes 2–3 are skipped and this collapses to the pre-#413
/// two-probe behaviour — loose-beats-packed precedence and every existing test hold.
/// A `commondir` resolving to `git_dir` itself short-circuits for the same reason.
fn resolve_symbolic_ref(git_dir: &Path, ref_path: &str) -> Option<String> {
    if let Some(sha) = read_loose_ref(git_dir, ref_path) {
        // probe 1: worktree-private loose ref
        return Some(sha);
    }
    if PER_WORKTREE_REF_PREFIXES
        .iter()
        .any(|p| ref_path.starts_with(p))
    {
        // per-worktree namespaces are never redirected to the common dir
        return None;
    }
    if let Some(common) = resolve_common_dir(git_dir) {
        let same = git_dir.canonicalize().ok().is_some_and(|g| g == common);
        if !same {
            // I2 short-circuit: skip probes 2–3 when commondir == git_dir
            if let Some(sha) = read_loose_ref(&common, ref_path) {
                // probe 2: commondir loose ref
                return Some(sha);
            }
            if let Some(sha) = read_packed_ref(&common, ref_path) {
                // probe 3: commondir packed-refs (post-git-gc steady state)
                return Some(sha);
            }
        }
    }
    // probe 4: worktree-private packed-refs (pre-#413 fallback, kept for monotonicity)
    read_packed_ref(git_dir, ref_path)
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
// Temporal staleness helpers
// ============================================================================

/// Read a single TEXT value from the `meta` table of `temporal.db`.
///
/// Opens a lightweight read-only connection (no WAL pragma, no permission
/// reset, no migrations) and queries the `meta` table for `key`.  Returns
/// `None` when the file is absent, the connection cannot be opened, or the key
/// has no row.
///
/// Shared by [`temporal_db_is_stale`] (for both `git_head` and `data_version`
/// keys), [`warn_if_temporal_unverifiable`] (for `git_head`), and
/// [`temporal_anchor_state`] (for `git_toplevel`) — one implementation, no drift.
fn read_temporal_meta(cache_dir: &Path, key: &str) -> Option<String> {
    let db_path = cache_dir.join("temporal.db");
    if !db_path.exists() {
        return None;
    }
    // Lightweight read-only open: no WAL pragma, no permission reset, no migrations.
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        rusqlite::params![key],
        |row| row.get(0),
    )
    .ok()
}

/// Return `true` when `temporal.db` is missing or its stored `META_GIT_HEAD`
/// does not match `current_head`.
///
/// `current_head` is the HEAD SHA already read by the caller (non-optional —
/// callers must check `current_head.is_some()` BEFORE calling this helper; on
/// non-git dirs the guard short-circuits before reaching this function).
///
/// # Performance (ADR-003)
///
/// Delegates to [`read_temporal_meta`] which uses the same lightweight
/// read-only SQLite open (no WAL pragma, no permission reset, no migrations).
/// This avoids the full `TemporalDb::open` cost on the steady-state
/// Current-path where the DB is checked but then immediately re-opened by the
/// dispatch arm.  The caller is responsible for the full `TemporalDb::open`
/// when it actually queries the DB.
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
/// inconsistent with this module's subprocess-free design. `current_head` is
/// the single HEAD read already performed at `auto_refresh_if_stale` entry;
/// passing it here avoids a second HEAD read and keeps one HEAD-reading
/// authority per call.
pub(super) fn temporal_db_is_stale(cache_dir: &Path, current_head: &str) -> bool {
    let db_path = cache_dir.join("temporal.db");
    if !db_path.exists() {
        return true;
    }

    // Check 1: HEAD match — absent row or mismatch both report stale.
    let stored_head = read_temporal_meta(cache_dir, rskim_search::META_GIT_HEAD);
    if stored_head.as_deref() != Some(current_head) {
        return true;
    }

    // AD-408-4: Check 2: data-version gate.
    // The DB is stale when the stored data_version is absent or numerically less
    // than TEMPORAL_DATA_VERSION, forcing a self-heal rebuild on the next query
    // (applies ADR-006; mirrors the lexical/AST/manifest self-heal in
    // check_staleness). Meta values are TEXT — version comparison is numeric to
    // correctly order multi-digit values (string compare mis-orders "10" vs "2").
    // An absent or non-integer stored value is treated as stale (pre-fix DB).
    // Uses `stored < current` (NOT `!=`) so a DB written by a newer binary is
    // NOT needlessly rebuilt by an older post-fix binary (no downgrade loop).
    let stored_version = read_temporal_meta(cache_dir, rskim_search::META_DATA_VERSION);
    match stored_version.as_deref() {
        Some(v) => match v.parse::<u64>() {
            Ok(n) => n < u64::from(rskim_search::TEMPORAL_DATA_VERSION),
            // Non-integer stored value → treat as stale.
            Err(_) => true,
        },
        // Absent data_version row → stale (pre-fix DB that lacks the ghost filter).
        None => true,
    }
}

/// Emit an advisory warning when git HEAD is unresolvable but `temporal.db`
/// has data that cannot be verified as current (AD-413-9).
///
/// Triple-gated (R5):
/// 1. `HeadState::Unresolved` (zero cost on healthy repos or non-repos).
/// 2. `temporal.db` exists (zero SQLite opens unless needed — AC24).
/// 3. A `git_head` row is recorded (no DB on the unborn-branch no-loop case).
///
/// Never called from `auto_refresh_if_stale` — that path is reached on every
/// query, so emitting there would produce permanent stderr noise on plain
/// non-temporal queries, which #414 SE-1/AC-30 forbids (A1 wiring correction).
/// See Step 7 wiring in the plan for the correct call sites.
pub(super) fn warn_if_temporal_unverifiable(cache_dir: &Path, head: &HeadState) {
    if !matches!(head, HeadState::Unresolved) {
        return; // zero cost on healthy repos and on non-repos
    }
    if !cache_dir.join("temporal.db").exists() {
        return; // zero SQLite opens unless needed (AC24 guard ordering)
    }
    let Some(stored) = read_temporal_meta(cache_dir, rskim_search::META_GIT_HEAD) else {
        return; // no recorded HEAD → no advisory (unborn-branch no-loop case, Case A)
    };
    eprintln!(
        "skim search: git HEAD is unresolvable here — temporal ranking is served from \
         recorded commit {}… and cannot be verified as current",
        stored.get(..8).unwrap_or(&stored)
    );
}

/// State of the repository anchor recorded in `temporal.db`'s `meta` table.
///
/// AD-413-16: the toplevel that produced temporal rows is persisted as
/// `meta.git_toplevel` so query arms can refuse rather than silently serving
/// data from a different repository when the indexed root has been retargeted.
#[derive(Debug, PartialEq)]
pub(super) enum AnchorState {
    /// Root has its own `.git` — the anchor mechanism is irrelevant (plain repo or submodule).
    /// Gate 1 of `temporal_anchor_state` returns this for every non-adopted root (AC32).
    NotAdopted,
    /// No `temporal.db` or no `git_toplevel` row — adopt and record on the next rebuild.
    Absent,
    /// Persisted toplevel matches the live resolution — temporal data is trustworthy.
    Agrees,
    /// Persisted toplevel was written by a DIFFERENT repository than the current one.
    /// Temporal-consuming query arms must refuse (no rows served, no rebuild, exit 0).
    /// Explicit build arms (`--build`/`--rebuild`/`--update`) re-anchor loudly.
    Differs { recorded: String, live: PathBuf },
}

/// AD-413-16: compare the persisted repository anchor in `temporal.db` against
/// the toplevel that would be adopted for `root` today.
///
/// Cost: `NotAdopted` is returned for every root that has a `.git` entry — both
/// AC32 corpora and every existing user — performing zero DB reads and zero
/// SQLite opens.  Only an adopted (subdirectory) root reads the anchor row.
pub(super) fn temporal_anchor_state(cache_dir: &Path, root: &Path) -> AnchorState {
    // Gate 1: root that owns `.git` is never re-pointed (AC17, AC32).
    let Some(top) = resolve_repo_toplevel(root) else {
        return AnchorState::NotAdopted;
    };
    // Gate 2: no DB means no anchor — adopt and record on the next build.
    if !cache_dir.join("temporal.db").exists() {
        return AnchorState::Absent;
    }
    match read_temporal_meta(cache_dir, rskim_search::META_GIT_TOPLEVEL) {
        None => AnchorState::Absent,
        Some(rec) if Path::new(&rec) == top.as_path() => AnchorState::Agrees,
        Some(rec) => AnchorState::Differs {
            recorded: rec,
            live: top,
        },
    }
}

/// Rebuild `temporal.db` non-fatally, swallowing any error per ADR-006/D5.
///
/// This is the single implementation of the D5 non-fatal-swallow contract that
/// was previously duplicated in three structurally-divergent copies across
/// `run_build` (mod.rs), the BUG-B self-heal (here), and the post-rebuild hook
/// (below). Centralising it prevents the copies from drifting independently —
/// a single edit here updates all three call sites.
///
/// # Contract (ADR-006/D5)
///
/// - `rebuild_temporal` is always called when `head` is `Some`.
/// - If `rebuild_temporal` returns `Err`, the error is SWALLOWED (never propagated).
/// - A debug-gated warning is emitted to stderr via `eprintln!` when the error
///   is swallowed and `SKIM_DEBUG=1` / `--debug` is set.
/// - Callers never see a temporal failure — only lexical/AST failures propagate.
///
/// # Parameters
///
/// - `root`: project root passed to `rebuild_temporal`.
/// - `cache_dir`: cache directory containing `temporal.db`.
/// - `head`: the git HEAD SHA to record; `None` skips the rebuild (non-git dir).
/// - `debug_label`: short label for the debug message (e.g. `"self-heal"`,
///   `"post-rebuild"`, `"--rebuild hook"`).
/// - `allow_reanchor`: when `false`, a `Differs` anchor state (PF-017) causes the
///   temporal rebuild to be SKIPPED, leaving `temporal.db` byte-unchanged.  Pass
///   `true` only from the explicit build arms (`--build`, `--rebuild`, `--update`)
///   so that only user-initiated rebuilds may retarget the repository anchor.
pub(super) fn try_rebuild_temporal_nonfatal(
    root: &Path,
    cache_dir: &Path,
    head: Option<&str>,
    debug_label: &str,
    allow_reanchor: bool,
) {
    use super::temporal_build::{current_epoch_secs, rebuild_temporal};

    let Some(head) = head else { return };
    // PF-017: a changed `--root` toplevel also changes the adopted HEAD, so without
    // this gate `check_staleness` would report `HeadChanged`, `auto_refresh_if_stale`
    // would rebuild, and `record_temporal_anchor` would overwrite the anchor — on a
    // PLAIN LEXICAL QUERY that never asked for temporal data.  Only the three explicit
    // build arms pass `allow_reanchor: true`; every other caller (self-heal, query-path
    // post-rebuild) passes `false`, leaving `temporal.db` untouched on anchor mismatch.
    if !allow_reanchor
        && let AnchorState::Differs { recorded, live } = temporal_anchor_state(cache_dir, root)
    {
        if crate::debug::is_debug_enabled() {
            eprintln!(
                "skim search [debug]: temporal rebuild skipped — anchor mismatch \
                 (recorded={recorded}, live={}); use `skim search --rebuild` to re-anchor",
                live.display(),
            );
        }
        return;
    }
    if let Err(e) = rebuild_temporal(root, cache_dir, head, current_epoch_secs()) {
        // Ignore temporal errors — they must not fail the lexical/AST query (ADR-006/D5).
        if crate::debug::is_debug_enabled() {
            eprintln!("skim search [debug]: temporal {debug_label} error (non-fatal): {e}");
        }
    }
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
pub(super) fn auto_refresh_if_stale(
    root: &Path,
    cache_dir: &Path,
    _analytics: &crate::analytics::AnalyticsConfig,
    allow_reanchor: bool,
) -> anyhow::Result<(RefreshOutcome, FileManifest)> {
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
            try_rebuild_temporal_nonfatal(root, cache_dir, Some(head), "self-heal", allow_reanchor);
        }

        return Ok((RefreshOutcome::UpToDate, manifest));
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
    let did_build: bool = match staleness {
        StalenessCheck::Current => unreachable!(),
        StalenessCheck::NoIndex => {
            eprintln!("skim search: building index…");
            let result = build_index(&config)?;
            eprintln!(
                "skim search: indexed {} files in {:.1}s",
                result.file_count,
                result.duration.as_secs_f64()
            );
            true
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
            true
        }
        StalenessCheck::NoStoredHead => {
            // Manifest exists but no HEAD recorded — could be an old build or
            // a git repo that appeared since the last non-git build.
            // Rebuild to get a fresh manifest with HEAD stored.
            eprintln!("skim search: refreshing index (no HEAD recorded)…");
            build_index(&config)?;
            true
        }
        StalenessCheck::WorkingTreeChanged {
            changed,
            added,
            removed,
        } => {
            // Uncommitted working-tree edits with an unchanged git HEAD (#379).
            // AD-379-4: a FULL rebuild (not a per-file incremental writer) so the
            // FileId↔sorted_paths alignment invariant is preserved (ADR-006).
            // AD-379-8: build_index_rechecked re-checks staleness AFTER acquiring
            // the build lock and SKIPS the rebuild when a concurrent peer already
            // refreshed the index — collapsing a rebuild stampede to one build.
            eprintln!(
                "skim search: index stale (working tree changed: \
                 {changed} modified, {added} added, {removed} removed), refreshing…"
            );
            let built = build_index_rechecked(&config, || {
                // Re-evaluate staleness under the lock: skip the rebuild unless the
                // working tree is STILL dirty (a peer may have already rebuilt).
                matches!(
                    check_staleness(cache_dir, root).0,
                    StalenessCheck::WorkingTreeChanged { .. }
                )
            })?;
            built.is_some()
        }
    };

    // If the rebuild was skipped because a peer already refreshed (AD-379-8),
    // the index is now Current: return without re-running the temporal hook.
    if !did_build {
        let manifest = FileManifest::load(root.to_path_buf(), cache_dir.to_path_buf())?;
        return Ok((RefreshOutcome::UpToDate, manifest));
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
    // `allow_reanchor` is threaded from the caller: only the explicit build arms
    // (`--build`, `--rebuild`, `--update`) pass `true`; query-triggered refreshes
    // pass `false` so anchor mismatch leaves temporal.db untouched (PF-017).
    try_rebuild_temporal_nonfatal(
        root,
        cache_dir,
        current_head,
        "post-rebuild",
        allow_reanchor,
    );
    // ─────────────────────────────────────────────────────────────────────────

    let outcome = if is_no_index {
        RefreshOutcome::FirstBuild
    } else {
        RefreshOutcome::Incremental
    };
    Ok((outcome, manifest))
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
