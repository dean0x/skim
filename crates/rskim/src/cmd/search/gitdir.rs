//! Git directory and HEAD resolution.
//!
//! Pure file I/O — no git binary subprocess, no libgit2 dependency.
//! Handles ordinary repos (`.git/` directory), linked worktrees (`.git` file),
//! and subdirectory roots that adopt an enclosing repository.
//!
//! All reads are bounded and all failures are soft: if we can't read git state
//! we degrade gracefully to `HeadState::NotARepo` or `HeadState::Unresolved`.
//!
//! AD-413-10: #413 extended this hand-rolled reader instead of switching to `gix`
//! (ADR-008's in-process rule is satisfied either way): `gix 0.72.1`/`gix-ref 0.52.1`
//! contain ZERO reftable support (measured), so gix buys no correctness the ladder lacks,
//! while `check_staleness` runs on every query and ADR-003 forbids an unmeasured hot-path cost.
//! `resolve_git_dir` still resolves ONE directory and never walks up, because
//! `walk::resolve_git_index_path` and the bare-repo boundary (AD-413-11) depend on that.
//! Ancestor discovery lives one level up in `git_head_state`, which for a root with NO `.git`
//! at all adopts the nearest enclosing repository via `resolve_repo_toplevel` (AD-413-14),
//! using `discover_project_root_from_canonical` — the same bounded ancestor scan that
//! `walk::discover_project_root` delegates to, so `--root <subdir>` and a bare invocation
//! from that subdirectory agree.

use std::path::{Path, PathBuf};

// ============================================================================
// Byte caps for untrusted file reads (AD-412-4)
// ============================================================================

/// Maximum bytes read from the `commondir` file inside a linked-worktree gitdir.
/// Generous cap — a path is never longer than this.
pub(super) const MAX_COMMONDIR_BYTES: u64 = 4096;

/// Maximum bytes read from any single-SHA or symbolic-ref file: the `.git` file
/// pointer, a bare `HEAD`, or a loose ref under `refs/`.
/// HEAD is "ref: refs/heads/some-branch\n" (~40 bytes typical) or a raw 40-hex SHA;
/// the .git pointer is "gitdir: <absolute-path>\n" — 512 bytes is generous.
const MAX_REF_BYTES: u64 = 512;

/// Maximum bytes read from `packed-refs` — can be large in long-lived repos
/// but still finite (soft-fail to None rather than OOM on a crafted checkout).
const MAX_PACKED_REFS_BYTES: u64 = 1 << 20; // 1 MiB

// ============================================================================
// HeadState
// ============================================================================

/// AD-413-7: three states, deliberately NOT `Option<String>` — "not a git repo" and
/// "git repo whose HEAD I could not resolve" are different facts, and collapsing them
/// is what made #413 silent and its message wrong (avoids PF-016). A gitdir with no
/// `HEAD` file is `NotARepo`, not `Unresolved`, or `mkdir .git` gets the opposite lie.
#[derive(Debug, PartialEq, Eq)]
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

// ============================================================================
// Git directory resolution
// ============================================================================

/// Maximum number of ancestors to traverse when looking for a `.git` root.
/// 256 ancestors is far beyond any real filesystem depth.
pub(super) const MAX_ANCESTORS: usize = 256;

/// Walk up from `canonical` (a pre-canonicalized path) looking for the nearest
/// ancestor that contains a `.git` entry.
///
/// Returns the first matching ancestor as a `PathBuf`, or `canonical` itself
/// when no enclosing `.git` is found within the [`MAX_ANCESTORS`] bound.
///
/// The caller is responsible for canonicalizing `canonical` first — this variant
/// skips the `canonicalize()` call to avoid a redundant O(path depth) syscall
/// chain when the caller already holds a canonical path (e.g. `resolve_repo_toplevel`
/// and `walk::discover_project_root`).
pub(super) fn discover_project_root_from_canonical(canonical: &Path) -> PathBuf {
    let mut current = canonical;
    for _ in 0..MAX_ANCESTORS {
        if current.join(".git").exists() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    canonical.to_path_buf()
}

/// Resolve the git directory for a project root.
///
/// - If `.git` is a **directory**, returns it directly.
/// - If `.git` is a **file** (worktree), parses the `gitdir: <path>` pointer
///   and returns the resolved target path.
/// - Returns `None` when `.git` doesn't exist.
///
/// This mirrors git's own resolution logic for `git rev-parse --git-dir`.
/// AD-413-11: a BARE repo has no `.git` entry, so this returns `None` before any ref
/// logic — out of scope for #413 (0 files indexed; the AD-408-1 ghost filter would drop
/// every row).  `reftable` repos are also unresolvable: their `HEAD` is the stub
/// `ref: refs/heads/.invalid`.
pub(super) fn resolve_git_dir(project_root: &Path) -> Option<PathBuf> {
    use std::io::Read as _;

    let dot_git = project_root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    if dot_git.is_file() {
        // Worktree: .git is a file containing "gitdir: <absolute-or-relative-path>"
        // Bounded read: AD-412-4 — the .git pointer file is repository-controlled.
        let mut content = String::new();
        std::fs::File::open(&dot_git)
            .ok()?
            .take(MAX_REF_BYTES)
            .read_to_string(&mut content)
            .ok()?;
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
/// instead of returning nothing (OD-3, A9).  Uses `discover_project_root_from_canonical`
/// (the same bounded ancestor scan that `walk::discover_project_root` delegates to),
/// keeping the two callers consistent.
pub(super) fn resolve_repo_toplevel(project_root: &Path) -> Option<PathBuf> {
    // Never re-point a root that claims to be a repository already (AC17).
    // Use try_exists() to distinguish "absent" from "exists but unreadable" —
    // a permission error on `.git` must not silently trigger ancestor adoption
    // of an enclosing repository; treat the error conservatively as "present"
    // (return None / NotAdopted) so we never index a different repository's
    // history when the intended root is blocked by a permission fence.
    if project_root.join(".git").try_exists().unwrap_or(true) {
        return None;
    }
    let canonical = project_root.canonicalize().ok()?;
    // Call the local from-canonical variant to skip the redundant canonicalize()
    // that discover_project_root would perform on the already-canonical path
    // (O(path depth) lstat/readlink syscalls — measured, fixed per finding F2).
    let top = discover_project_root_from_canonical(&canonical);
    // `discover_project_root_from_canonical` returns the start path when no
    // enclosing repo is found.
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

// ============================================================================
// HEAD resolution
// ============================================================================

/// Per-worktree ref namespaces — these are never redirected to the common dir.
const PER_WORKTREE_REF_PREFIXES: [&str; 3] = ["refs/bisect/", "refs/worktree/", "refs/rewritten/"];

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
    use std::io::Read as _;

    let Some(git_dir) = resolve_git_dir(project_root)
        .or_else(|| resolve_repo_toplevel(project_root).and_then(|t| resolve_git_dir(&t)))
    else {
        return HeadState::NotARepo;
    };
    let head_path = git_dir.join("HEAD");
    // Bounded read: HEAD is short (ref line or raw SHA), but it's filesystem-controlled.
    // AD-412-4: treat as untrusted input.
    let mut content = String::new();
    match std::fs::File::open(&head_path)
        .and_then(|f| f.take(MAX_REF_BYTES).read_to_string(&mut content))
    {
        Ok(_) => {}
        Err(_) => {
            // F10: a gitdir with NO `HEAD` file is NOT a repo (`mkdir .git`, or a
            // `.git`-file pointer at a directory that does not exist) — classifying it
            // as `Unresolved` would tell the user "this repo's HEAD cannot be resolved"
            // about something that is not a repo.
            //
            // But a `HEAD` that EXISTS and cannot be READ (permissions, I/O error,
            // non-UTF-8 bytes) is the OTHER fact: a repo whose HEAD is unresolvable.
            // Collapsing that into `NotARepo` emits the opposite lie and is exactly the
            // absence-overloading AD-413-7 exists to prevent (avoids PF-016) — it is
            // also the "fs error" cause named in `HeadState::Unresolved`'s own doc.
            return if head_path.is_file() {
                HeadState::Unresolved
            } else {
                HeadState::NotARepo
            };
        }
    }
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

// ============================================================================
// Ref readers (bounded, defense-in-depth)
// ============================================================================

/// Read a loose ref from `dir` (e.g. `dir/refs/heads/main`).
///
/// Returns `None` when the file is absent, is not a regular file (symlinks
/// rejected — defense-in-depth; see AD-408-2 for the consciously-accepted
/// in-tree-symlink boundary), unreadable, or its content is not a valid
/// 40/64-hex commit SHA.
///
/// The read is bounded at [`MAX_REF_BYTES`] — a loose ref file contains only
/// a hex SHA newline (40–65 bytes), so a larger file is either corrupt or
/// adversarially crafted.
fn read_loose_ref(dir: &Path, ref_path: &str) -> Option<String> {
    use std::io::Read as _;

    let loose_path = dir.join(ref_path);
    // Defense-in-depth: reject non-regular ref files (symlinks could be directed
    // by repository content at arbitrary targets outside the git dir).
    // Adjacent to the consciously-accepted AD-408-2 boundary for in-tree symlinks;
    // this guard applies to ref-namespace symlinks only.
    match loose_path.symlink_metadata() {
        Ok(m) if m.file_type().is_file() => {}
        _ => return None,
    }
    let mut content = String::new();
    std::fs::File::open(&loose_path)
        .ok()?
        .take(MAX_REF_BYTES)
        .read_to_string(&mut content)
        .ok()?;
    let sha = content.trim().to_string();
    if is_hex_sha(&sha) { Some(sha) } else { None }
}

/// Scan `dir/packed-refs` for the SHA assigned to `ref_path`.
///
/// Returns `None` when the file is absent, unreadable, exceeds
/// [`MAX_PACKED_REFS_BYTES`], or the ref is not listed.
///
/// The read is capped at 1 MiB (defense-in-depth: a crafted checkout with an
/// enormous `packed-refs` must not drive the process OOM).  The scan uses
/// `BufReader::lines()` so that O(1) memory is used regardless of file size —
/// only one line is in memory at a time, and the existing early return stops
/// reading as soon as the target ref is found.  The `take()` cap is preserved
/// for the OOM guard; hitting it surfaces as an `Ok("")` EOF, not an error.
fn read_packed_ref(dir: &Path, ref_path: &str) -> Option<String> {
    use std::io::{BufRead as _, BufReader, Read as _};

    let packed_refs_path = dir.join("packed-refs");
    let file = std::fs::File::open(&packed_refs_path).ok()?;
    let reader = BufReader::new(file.take(MAX_PACKED_REFS_BYTES));
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break, // I/O error mid-read — stop scanning
        };
        // Skip comment/peeled-tag lines
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
    None
}

/// Resolve a symbolic ref (e.g. `refs/heads/main`) to its SHA.
///
/// AD-413-4: four probes in order — worktree loose, commondir loose, commondir
/// `packed-refs` (mandatory: the post-`git gc` steady state), worktree `packed-refs`.
///
/// AD-413-5: probe 1 stays FIRST and probe 4 stays LAST.  A plain repo or submodule
/// has no `commondir`, so probes 2–3 are skipped and this collapses to the pre-#413
/// two-probe behaviour — loose-beats-packed precedence and every existing test hold.
/// A `commondir` resolving to `git_dir` itself short-circuits for the same reason.
///
/// Per-worktree ref namespaces (`refs/bisect/`, `refs/worktree/`, `refs/rewritten/`)
/// skip ONLY probes 2–3 (the commondir redirect), because those refs are never shared
/// across worktrees.  Probe 4 (worktree-private `packed-refs`) still applies: a
/// `git pack-refs` on a worktree-private ref places it in the worktree's own
/// `packed-refs`, and skipping probe 4 would silently break resolution whenever that
/// file exists — contradicting the monotonicity guarantee in the doc above.
fn resolve_symbolic_ref(git_dir: &Path, ref_path: &str) -> Option<String> {
    if let Some(sha) = read_loose_ref(git_dir, ref_path) {
        // probe 1: worktree-private loose ref
        return Some(sha);
    }
    // Probes 2–3 only: per-worktree namespaces are never stored in the commondir.
    // Probe 4 below still applies to all ref paths (including per-worktree).
    let is_per_worktree = PER_WORKTREE_REF_PREFIXES
        .iter()
        .any(|p| ref_path.starts_with(p));
    if !is_per_worktree && let Some(common) = resolve_common_dir(git_dir) {
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
    // probe 4: worktree-private packed-refs (applies to all ref paths, including
    // per-worktree namespaces — the monotonicity guarantee requires probe 4 to be
    // reachable even when probes 2–3 were skipped for a per-worktree prefix).
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
// Commondir resolution (linked-worktree ladder)
// ============================================================================

/// Resolve the `commondir` of a linked-worktree gitdir.
///
/// AD-413-1: the `commondir` file is the shared ref store for a linked
/// worktree — all global refs (branches, tags) live in the main repo's
/// gitdir; the per-worktree gitdir only holds HEAD and per-worktree state.
///
/// A linked worktree's gitdir (e.g. `.git/worktrees/<name>/`) contains a
/// `commondir` file pointing at the main repo's gitdir.  Global refs live
/// there — the worktree-private gitdir's `refs/` is empty (measured, #413).
///
/// `pub(super)` so `hooks.rs` can call `super::staleness::resolve_common_dir` without
/// duplicating the parsing logic (AD-413-15 / Step 9).
pub(super) fn resolve_common_dir(git_dir: &Path) -> Option<PathBuf> {
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
                "skim search [debug]: commondir unreadable in {:?}",
                git_dir.display().to_string(),
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
    // `git_dir`, not `project_root` (unlike resolve_git_dir, which anchors its relative
    // `.git` pointer against project_root) — anchoring on `project_root` lands
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
                    "skim search [debug]: commondir target is not a git dir ({:?})",
                    joined.display().to_string(),
                );
            }
            return None;
        }
    };
    if !canonical.is_dir() || !canonical.join("HEAD").is_file() {
        // AD-413-3 sanity gate: the target must be a directory containing HEAD.
        if crate::debug::is_debug_enabled() {
            eprintln!(
                "skim search [debug]: commondir target is not a git dir ({:?})",
                canonical.display().to_string(),
            );
        }
        return None;
    }
    Some(canonical)
}
