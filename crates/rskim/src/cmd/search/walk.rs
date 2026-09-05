//! Project-root discovery and recursive file walking for the index builder.
//!
//! # File cap (deterministic top-K selection — #379)
//!
//! `walk_metadata` (production) and `walk_and_read` (tests) always visit the
//! COMPLETE tree, then retain only the `max_files` entries with the smallest
//! `normalize_rel_path` keys (ascending `str` order) — see
//! [`collect_bounded_topk`]. This is an order-invariant SET function ("the K
//! smallest keys over the complete walked set"), so results are
//! byte-identical across runs regardless of parallel-walk thread scheduling.
//! A prior implementation terminated the walk early (`WalkState::Quit`) once
//! a shared atomic counter reached `max_files`, which made retained-set
//! MEMBERSHIP (not just order) depend on thread scheduling. Skipped files
//! (unsupported language, too large, non-UTF8) do not count toward the cap.
//!
//! # Git-tracked union (#402)
//!
//! On a git root (`root/.git` exists), [`walk_metadata`] unions the
//! ignore-walk output with git-tracked files that the `.gitignore`/hidden-file
//! rules would otherwise exclude. This ensures `skim search` finds every file
//! that `git grep` finds (ADR-007 dog-food ground truth). The union is
//! **default-on** and runs inside [`walk_metadata`] — the single discovery
//! funnel — so the index builder and the staleness scan always see the same
//! file set (AD-379-1). Union entries re-enter the same [`normalize_rel_path`]
//! top-K selection so the `max_files` cap applies to `walked ∪ tracked`.
//! Non-git roots (no `.git`), enumeration failures, and fail-soft gix errors
//! all degrade gracefully: the ignore-walk set is used alone (AD-402-3).
//!
//! # Skip conditions (in order checked)
//!
//! | Condition | Threshold |
//! |-----------|-----------|
//! | Unsupported language | `Language::from_path()` returns `None` |
//! | File too large | > 5 MB (`metadata.len()`) |
//! | Non-UTF8 | `read_to_string()` returns `Err` |
//! | Minified | avg line > 500 bytes in first 8 KB (tree-sitter langs only) |
//! | Cap reached | total accepted entries exceed `max_files` (reporting only — computed AFTER the complete walk; never terminates it) |

use std::collections::{BinaryHeap, HashSet};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

// ============================================================================
// Path normalization (shared between walk and consume)
// ============================================================================

/// Normalize a repo-relative `Path` to the canonical manifest key string.
///
/// Produces the same byte string that the manifest `BTreeMap<String>` stores as
/// its key: `to_string_lossy()` followed by `\\` → `/` replacement.
///
/// # Why this function exists (AD-373-2)
///
/// AD-373-2: the byte string used to ORDER FileIds (walk) and the byte strings
/// STORED as the manifest key (index.rs `consume`, `path_key` for `new_manifest.insert`)
/// and LOOKED UP for the lexical cache (index.rs `read_and_classify`, `path_key` for
/// `manifest.lookup`) must all be produced by this one function. Any divergence
/// reintroduces the #373 ordering skew (notably the `\\` → `/` normalization
/// on Windows).
///
/// Note: `temporal::normalize_blast_radius_path` is intentionally NOT consolidated
/// here — it carries an extra `strip_prefix("./")` step that serves a different
/// contract. Leave it in place (see the `#373 scope` NOTE in `temporal.rs`).
pub(super) fn normalize_rel_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

use anyhow::Context as _;
use ignore::{WalkBuilder, WalkState};
use rskim_core::Language;
use sha2::{Digest, Sha256};

use super::types::{SkipReason, WalkEntry};

#[cfg(test)]
use super::types::ReadFile;

// ============================================================================
// Constants
// ============================================================================

/// Maximum file size accepted for indexing (5 MiB).
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

// Compile-time guard: MAX_FILE_BYTES must fit in a usize so the pre-allocation
// in open_and_read (`size as usize`) is sound on every supported platform.
const _: () = assert!(
    MAX_FILE_BYTES <= usize::MAX as u64,
    "MAX_FILE_BYTES exceeds usize::MAX — update the cast in open_and_read"
);

/// Number of bytes inspected when checking for minified files.
const MINIFY_PROBE_BYTES: usize = 8192;

/// Average line length (bytes) above which a file is considered minified.
const MINIFY_AVG_LINE_BYTES: usize = 500;

/// Minimum total file size required before the minification heuristic fires.
///
/// AD-395-1: The two-signal minified gate requires BOTH (1) content.len() >=
/// MINIFY_MIN_BYTES AND (2) the first MINIFY_PROBE_BYTES probe is effectively
/// single-line (newline_count <= 1).  Defined as 8 × MINIFY_PROBE_BYTES (= 64
/// KiB) so the threshold is grounded in the existing probe-window constant
/// rather than an arbitrary number (applies ADR-003).  Genuine bundles
/// (100s KiB–MBs) remain caught; small generated / data-in-code files
/// (single-digit KiB, including the 1.4 KiB ticket repro) are indexed.
pub(super) const MINIFY_MIN_BYTES: usize = 8 * MINIFY_PROBE_BYTES; // 65_536

/// Maximum number of skip reasons collected during a walk or producer phase.
///
/// Large monorepos may encounter millions of unsupported files.  Collecting an
/// unbounded path list wastes memory; callers only need a representative sample
/// for diagnostics.  Once the cap is hit, [`SkipReason::CapReached`] entries
/// are still appended so the caller knows truncation occurred.
///
/// Exported `pub(super)` so `index.rs` can reuse the same cap for the merged
/// producer skip sample (AD-395-2).
pub(super) const MAX_SKIP_REASONS: usize = 10_000;

// Test-only per-thread counter: incremented once per live `gix` call in
// `list_tracked_files`.  Used by the AC13 unit test to discriminate cache
// hits (counter unchanged) from cache misses (counter increments).
//
// A THREAD-LOCAL (not a shared global `AtomicUsize`) so the count is isolated
// per test. libtest runs each test on its own thread, and `list_tracked_files`
// always runs on the caller's thread (the union merge in `merge_tracked_union`
// happens AFTER the parallel walk joins). A shared global counter would be
// racily incremented by every other test that calls `walk_metadata` on a git
// root (`test_walk_metadata_*`, the AC6/AC9 union tests) running concurrently
// under `RUST_TEST_THREADS`, which `#[serial]` does not guard against, making
// AC13's cache-hit/miss assertions flaky. Per-thread isolation removes the race.
#[cfg(test)]
thread_local! {
    pub(super) static ENUM_CALL_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// ============================================================================
// Typed read outcome
// ============================================================================

/// Strongly-typed result of [`open_and_read`].
///
/// Using an enum instead of `io::Error` avoids string-matching on error messages
/// to distinguish the "too large" case from genuine I/O failures.  The caller
/// matches on variants and never inspects error message text.
pub(super) enum ReadOutcome {
    /// File read successfully.
    Content(String),
    /// File content is not valid UTF-8.
    NonUtf8,
    /// File size (from the open file handle's metadata) exceeds [`MAX_FILE_BYTES`].
    TooLarge(u64),
    /// Any other I/O error (permission denied, broken pipe, etc.).
    Io(std::io::Error),
}

// ============================================================================
// Project root discovery
// ============================================================================

/// Walk up from `start` looking for a `.git` directory.
///
/// Returns the first ancestor that contains `.git/`, or `start` itself if none
/// is found (fallback: treat the provided directory as the root).
///
/// Delegates the bounded ancestor scan to
/// [`super::gitdir::discover_project_root_from_canonical`] after canonicalizing,
/// so both this entry-point and `gitdir::resolve_repo_toplevel` share the same
/// walk logic (AD-413-14).
///
/// # Errors
///
/// Returns `Err` if `start` cannot be canonicalized.
pub(super) fn discover_project_root(start: &Path) -> anyhow::Result<PathBuf> {
    let canonical = start
        .canonicalize()
        .with_context(|| format!("failed to canonicalize path: {}", start.display()))?;
    Ok(super::gitdir::discover_project_root_from_canonical(
        &canonical,
    ))
}

// ============================================================================
// File walking (batch) — retained for tests; streaming pipeline uses walk_metadata
// ============================================================================

/// Classify a single directory entry into a [`ClassifyOutcome`] of [`ReadFile`].
///
/// Handles language detection, size pre-screening (via cached `DirEntry`
/// metadata), file reading, and minification detection.  The caller
/// ([`collect_bounded_topk`]) is responsible for the bounded top-K cap and
/// for guarding the `skipped` vector length against [`MAX_SKIP_REASONS`].
#[cfg(test)]
fn classify_entry(entry: &ignore::DirEntry, root: &Path) -> ClassifyOutcome<ReadFile> {
    // Only process regular files.
    let file_type = match entry.file_type() {
        Some(ft) => ft,
        None => return ClassifyOutcome::Transparent,
    };
    if !file_type.is_file() {
        return ClassifyOutcome::Transparent;
    }

    let abs_path = entry.path();

    // --- Unsupported language ---
    let lang = match Language::from_path(abs_path) {
        Some(l) => l,
        None => {
            return ClassifyOutcome::Skip(SkipReason::UnsupportedLanguage(abs_path.to_path_buf()));
        }
    };

    // --- Fast size pre-screen using DirEntry cached metadata ---
    // entry.metadata() avoids an extra stat(2) syscall on 50 K-file repos.
    // If it fails we fall through and let the open() path handle the error.
    if let Ok(meta) = entry.metadata() {
        let size = meta.len();
        if size > MAX_FILE_BYTES {
            return ClassifyOutcome::Skip(SkipReason::TooLarge {
                path: abs_path.to_path_buf(),
                size,
            });
        }
    }

    // --- Open, size-check on handle, read (fixes TOCTOU race) ---
    // Open the file first so that the metadata check and the read operate
    // on the same inode.  Pre-allocate the buffer to the known size so
    // read_to_string does at most one allocation.
    let content = match open_and_read(abs_path) {
        ReadOutcome::Content(c) => c,
        ReadOutcome::NonUtf8 => {
            return ClassifyOutcome::Skip(SkipReason::NonUtf8(abs_path.to_path_buf()));
        }
        ReadOutcome::TooLarge(size) => {
            // File grew past the limit between the pre-screen and open.
            return ClassifyOutcome::Skip(SkipReason::TooLarge {
                path: abs_path.to_path_buf(),
                size,
            });
        }
        ReadOutcome::Io(e) => {
            return ClassifyOutcome::Skip(SkipReason::ReadError {
                path: abs_path.to_path_buf(),
                error: e.to_string(),
            });
        }
    };

    // --- Minification check (tree-sitter languages only) ---
    // Serde-based languages (JSON, YAML, TOML) produce long lines by design;
    // skip the minification check for them.
    if !lang.is_serde_based()
        && let Some(avg_line_bytes) = minified_metric(&content)
    {
        return ClassifyOutcome::Skip(SkipReason::Minified {
            path: abs_path.to_path_buf(),
            avg_line_bytes,
        });
    }

    let mtime = mtime_secs(entry);
    // Consistent with classify_metadata_core: return Transparent on root-escape
    // rather than falling back to the absolute path as a relative path.
    let rel_path = match abs_path.strip_prefix(root) {
        Ok(r) => r.to_path_buf(),
        Err(_) => return ClassifyOutcome::Transparent,
    };

    ClassifyOutcome::Accept(ReadFile {
        rel_path,
        lang,
        content,
        mtime,
    })
}

/// Walk `root` recursively, read each source file, and return the list of
/// [`ReadFile`]s along with collected [`SkipReason`]s.
///
/// Retained for use in tests. The production streaming pipeline uses
/// [`walk_metadata`] + per-file reading in the producer thread.
///
/// # Ordering and at-cap determinism (#379)
///
/// Delegates to [`collect_bounded_topk`], shared with [`walk_metadata`]: the
/// COMPLETE tree is visited, then the `max_files` entries with the smallest
/// [`normalize_rel_path`] keys are retained, sorted ascending. This is an
/// order-invariant SET function, so results are byte-identical across runs
/// regardless of parallel-walk thread scheduling — including at the cap,
/// where a prior sort-AFTER-early-terminate approach could not guarantee
/// determinism because early termination made retained-set membership
/// itself race-dependent before the sort ever ran (AD-379-7 successor).
///
/// # Errors
///
/// Returns `Err` only for fatal walker setup errors. Per-file read errors are
/// collected as [`SkipReason::ReadError`] and returned in the skipped list.
#[cfg(test)]
pub(super) fn walk_and_read(
    root: &Path,
    max_files: usize,
) -> anyhow::Result<(Vec<ReadFile>, Vec<SkipReason>)> {
    let mut builder = WalkBuilder::new(root);
    configure_builder(&mut builder);

    collect_bounded_topk(builder, max_files, root, classify_entry, read_file_key)
}

// ============================================================================
// Metadata-only walk (streaming pipeline)
// ============================================================================

/// Extract mtime as seconds since UNIX_EPOCH from a [`std::fs::Metadata`] value.
///
/// Returns `None` when the platform does not expose modification time or the
/// syscall fails. Used by [`classify_metadata_core`].
fn mtime_from_meta(meta: &std::fs::Metadata) -> Option<u64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

/// Classify an already-confirmed regular file from its path and optional metadata.
///
/// Shared by the ignore-walk path ([`classify_entry_metadata`]) and the
/// git-tracked union path ([`classify_tracked_path`]) so both apply byte-identical
/// language detection, the 5 MiB size gate, and mtime/size capture — and emit
/// identical [`SkipReason`]s.
///
/// AD-402-4: Unioned files reuse this core so a classify failure on a unioned
/// [`WalkEntry`] is caught by the existing `Pipeline::consume` desync-abort
/// (index.rs:711-725) and the `manifest_count != file_count` commit guard
/// (index.rs:417-424) — no new ADR-006 code needed.
fn classify_metadata_core(
    abs_path: &Path,
    meta_opt: Option<std::fs::Metadata>,
    root: &Path,
) -> ClassifyOutcome<WalkEntry> {
    // Language detection.
    let lang = match Language::from_path(abs_path) {
        Some(l) => l,
        None => {
            return ClassifyOutcome::Skip(SkipReason::UnsupportedLanguage(abs_path.to_path_buf()));
        }
    };

    // Recorded size in bytes (AD-379-2): persisted in the manifest so the
    // working-tree staleness scan can compare size as a second freshness hint
    // alongside mtime. `None` when the platform/syscall does not expose it.
    let size = meta_opt.as_ref().map(std::fs::Metadata::len);
    if let Some(len) = size
        && len > MAX_FILE_BYTES
    {
        return ClassifyOutcome::Skip(SkipReason::TooLarge {
            path: abs_path.to_path_buf(),
            size: len,
        });
    }

    let mtime = meta_opt.as_ref().and_then(mtime_from_meta);
    // Security: if abs_path escapes root (e.g. via a crafted git index `..`
    // entry that slipped past the list_tracked_files filter), return Transparent
    // rather than using the absolute path as rel_path.  Defense in depth —
    // list_tracked_files filters ParentDir/RootDir before this point, so this
    // arm fires only for unexpected escapes (AD-402-4 / path-containment).
    let rel_path = match abs_path.strip_prefix(root) {
        Ok(r) => r.to_path_buf(),
        Err(_) => return ClassifyOutcome::Transparent,
    };

    ClassifyOutcome::Accept(WalkEntry {
        abs_path: abs_path.to_path_buf(),
        rel_path,
        lang,
        mtime,
        size,
    })
}

/// Classify a single directory entry without reading its content.
///
/// Checks: file type, language detection, fast size pre-screen (DirEntry
/// metadata).  No I/O beyond the metadata already cached by the walker.
///
/// AD-402-4: Thin wrapper around [`classify_metadata_core`]; unchanged external
/// behavior. Capture metadata once to avoid double syscalls (size pre-screen
/// + mtime extraction both use the same [`std::fs::Metadata`]).
fn classify_entry_metadata(entry: &ignore::DirEntry, root: &Path) -> ClassifyOutcome<WalkEntry> {
    let file_type = match entry.file_type() {
        Some(ft) => ft,
        None => return ClassifyOutcome::Transparent,
    };
    if !file_type.is_file() {
        return ClassifyOutcome::Transparent;
    }
    // Capture metadata once; use it for the size pre-screen (AD-379-2) and
    // mtime extraction so the walker never calls entry.metadata() twice per file.
    classify_metadata_core(entry.path(), entry.metadata().ok(), root)
}

/// Classify a git-tracked path that the ignore-walker did NOT yield.
///
/// Uses [`std::fs::symlink_metadata`] to mirror `follow_links(false)`: a
/// symlink, directory (gitlink/submodule in the work tree), or other
/// non-regular-file entry is [`ClassifyOutcome::Transparent`] — skipped
/// silently, exactly as the walker skips non-file entries. A vanished or
/// unreadable path becomes a bounded [`SkipReason::ReadError`].
///
/// AD-402-4: Delegates to [`classify_metadata_core`] for identical language
/// detection, size gate, and mtime/size capture as the ignore-walk path.
fn classify_tracked_path(abs_path: &Path, root: &Path) -> ClassifyOutcome<WalkEntry> {
    let meta = match fs::symlink_metadata(abs_path) {
        Ok(m) => m,
        Err(e) => {
            return ClassifyOutcome::Skip(SkipReason::ReadError {
                path: abs_path.to_path_buf(),
                error: e.to_string(),
            });
        }
    };
    // Not a regular file (symlink, directory/gitlink, etc.) → Transparent.
    if !meta.is_file() {
        return ClassifyOutcome::Transparent;
    }
    classify_metadata_core(abs_path, Some(meta), root)
}

/// Walk `root` recursively, collecting file metadata without reading content.
///
/// Returns a sorted list of [`WalkEntry`]s and collected [`SkipReason`]s.
/// Content reading is deferred to the streaming producer in the index pipeline.
///
/// # Ordering and at-cap determinism (#379)
///
/// Delegates to [`collect_bounded_topk`] (shared with the test-only
/// [`walk_and_read`]): the COMPLETE tree is visited, then the `max_files`
/// entries with the smallest [`normalize_rel_path`] keys are retained, sorted
/// ascending (byte-wise `str` comparison). This gives deterministic FileId
/// assignment in the consumer — the sort key is byte-identical to the
/// manifest `BTreeMap<String>` key, so `FileId(n)` in the walk corresponds to
/// `sorted_paths()[n]` in the manifest, the invariant all five FileId
/// consumers depend on (applies ADR-006 / AD-379-4).
///
/// Selection is also an order-invariant SET function of the complete walked
/// set: retained membership at the cap depends only on each entry's key,
/// never on parallel-walk thread scheduling. The prior implementation sorted
/// AFTER an early `WalkState::Quit` terminated the walk at the cap, which
/// could not guarantee this because early termination made membership itself
/// race-dependent before the sort ever ran (the #379 bug this fixes).
///
/// AD-373-1: FileId assignment MUST use the same byte-wise order as the manifest
/// `BTreeMap<String>` resolution side (`sorted_paths`). `PathBuf::cmp` is
/// component-aware and diverges from `str::cmp` on nested dirs (`foo/bar.rs`
/// vs `foo.rs`), which mis-resolved `FileId`→path (#373). Sort by the
/// normalized String key under `str` ordering.
///
/// # Errors
///
/// Returns `Err` only for fatal walker-setup errors. Per-file metadata errors
/// are collected as [`SkipReason::ReadError`] in the skipped list.
pub(super) fn walk_metadata(
    root: &Path,
    max_files: usize,
    cache_dir: Option<&Path>,
) -> anyhow::Result<(Vec<WalkEntry>, Vec<SkipReason>)> {
    let mut builder = WalkBuilder::new(root);
    configure_builder(&mut builder);

    let (mut entries, mut skips) = collect_bounded_topk(
        builder,
        max_files,
        root,
        classify_entry_metadata,
        walk_entry_key,
    )?;

    // AD-402-1: Union git-tracked files the ignore-walker missed, through this
    // SAME funnel, so the index builder (index.rs `Pipeline::walk`) and the
    // staleness scan (staleness.rs `scan_working_tree`) always see the same file
    // set (AD-379-1). Consequence: an existing v5 index self-heals after an
    // upgrade — the staleness scan finds `added > 0` → `WorkingTreeChanged` →
    // one rebuild → stable thereafter. No index-format bump required.
    if root.join(".git").exists() {
        // Thread the caller-supplied cache_dir to merge_tracked_union so
        // tracked_files_memoized uses the pre-computed path (which respects
        // cache_dir_override) rather than re-resolving it via
        // resolve_search_cache_dir(root) here. tracked_files_memoized falls back
        // to its own resolution when cache_dir is None (staleness.rs, tests).
        // (AD-402-6 / finding: tracked-memo-redundant-resolve)
        let added = merge_tracked_union(root, max_files, &mut entries, &mut skips, cache_dir);
        if added > 0 && crate::debug::is_debug_enabled() {
            eprintln!(
                "skim search [debug]: unioned {added} git-tracked file(s) the .gitignore walk skipped"
            );
        }
    }
    Ok((entries, skips))
}

// ============================================================================
// Git-tracked union (#402)
// ============================================================================

/// Merge git-tracked files absent from `entries` into the SAME
/// [`normalize_rel_path`] top-K selection the walk uses, in place.
///
/// Returns the count of files successfully unioned in. Skips (symlinks, vanished
/// paths, unsupported language, too large) are appended to `skips` up to
/// [`MAX_SKIP_REASONS`].
///
/// AD-402-2: Union entries re-enter the same `normalize_rel_path` top-K rather
/// than being appended after truncation. `sort_by_cached_key` + `truncate` after
/// the merge preserves FileId↔`sorted_paths` determinism (AD-373-1 / PF-012).
///
/// AD-402-3: Enumeration failure (non-git root, gix parse error, etc.) is
/// fail-soft: returns 0 and leaves `entries` unchanged.
fn merge_tracked_union(
    root: &Path,
    max_files: usize,
    entries: &mut Vec<WalkEntry>,
    skips: &mut Vec<SkipReason>,
    cache_dir: Option<&Path>,
) -> usize {
    // AD-402-6: use memoized enumeration — only re-parses gix index when
    // .git/index has changed since the last call.  cache_dir is threaded from
    // walk_metadata (computed once there) to avoid a redundant
    // canonicalize+SHA-256 per query (finding: tracked-memo-redundant-resolve).
    let Some(tracked) = tracked_files_memoized(root, cache_dir) else {
        // AD-402-3: enumeration failure → fail-soft, keep ignore-walk set alone.
        return 0;
    };

    // Seed the dedup set from already-walked entries.
    let mut yielded: HashSet<String> = entries
        .iter()
        .map(|e| normalize_rel_path(&e.rel_path))
        .collect();

    // Also seed yielded from already-skipped paths so a tracked file that the
    // ignore-walk already classified as Skip (e.g. TooLarge) is not re-classified
    // by the union, which would append a duplicate SkipReason and fire an extra
    // symlink_metadata syscall.  SkipReason carries absolute paths; strip root and
    // normalize to match the dedup key (finding: merge-tracked-union-skip-dedup).
    for reason in skips.iter() {
        if let Some(abs_path) = reason.sort_key()
            && let Ok(rel) = abs_path.strip_prefix(root)
        {
            yielded.insert(normalize_rel_path(rel));
        }
    }

    let mut added = 0;
    for rel in tracked {
        let key = normalize_rel_path(&rel);
        if yielded.contains(&key) {
            continue; // already walked → skip (dedup by normalize_rel_path key)
        }
        // Mark seen before classify so duplicate index entries don't re-classify.
        yielded.insert(key);
        match classify_tracked_path(&root.join(&rel), root) {
            ClassifyOutcome::Accept(entry) => {
                entries.push(entry);
                added += 1;
            }
            ClassifyOutcome::Skip(reason) => {
                if skips.len() < MAX_SKIP_REASONS {
                    skips.push(reason);
                }
            }
            ClassifyOutcome::Transparent => {}
        }
    }

    if added > 0 {
        // AD-402-2: Re-run the SAME top-K (smallest max_files keys, ascending)
        // so FileId↔sorted_paths and cross-run determinism hold (PF-012).
        entries.sort_by_cached_key(|e| normalize_rel_path(&e.rel_path));
        entries.truncate(max_files);
    }
    added
}

/// Resolve the filesystem path to the git index file for `root`.
///
/// In a regular repository `root/.git` is a directory and the index lives at
/// `root/.git/index`. In a **linked git worktree** `root/.git` is a plain
/// file containing `gitdir: /abs/path/to/.git/worktrees/<name>` — the
/// worktree-specific index lives at `<gitdir>/index`, NOT at
/// `root/.git/index` (which does not exist as a path because `.git` is a
/// file). Without this helper, [`tracked_files_memoized`] would always fail
/// the `fs::metadata` call in a worktree and fall through to a live
/// [`list_tracked_files`] on every query, silently losing the memoization
/// bound AD-402-6 was designed to provide.
///
/// Returns `None` if `root/.git` is neither a directory nor a readable file
/// (non-git root). The caller should not invoke this for non-git roots
/// (gated by the `root.join(".git").exists()` check in [`walk_metadata`]).
///
/// AD-413-12: delegates to `staleness::resolve_git_dir` so the `gitdir:` POINTER has
/// ONE parser.  Two near-copies drifted (line-scan vs whole-file `strip_prefix`); #413
/// touches worktree indirection, so the duplicate is removed with it (applies ADR-001).
/// NOT a claim that all `.git`-path consumers are unified: `walk::discover_project_root`
/// owns the bounded ANCESTOR walk (a different job, reused by AD-413-14) and
/// `walk_metadata`'s `.git` existence probe stays local.  `hooks.rs` no longer hand-builds
/// `.git/hooks`: it resolves through this same parser plus `resolve_common_dir`
/// (AD-413-15), which is what makes the shared-hooks route correct.
fn resolve_git_index_path(root: &Path) -> Option<PathBuf> {
    super::staleness::resolve_git_dir(root).map(|d| d.join("index"))
}

/// Memoized tracked-file enumeration.
///
/// Reads a per-root sidecar cache (`{cache_dir}/tracked_union.cache`) keyed
/// on the `.git/index` fingerprint `(mtime_nanos, len_bytes)`. On call:
/// - Resolve the true git index path via [`resolve_git_index_path`] (handles
///   both regular repos and linked worktrees — see that function's doc).
/// - Stat the resolved index for the current fingerprint.
/// - If the sidecar fingerprint matches → return the cached list (cache HIT,
///   zero gix calls).
/// - Otherwise → call [`list_tracked_files`], rewrite the sidecar, return.
///
/// Fail-soft at every step: unresolvable cache dir, unreadable/corrupt sidecar,
/// missing git index → fall through to a live [`list_tracked_files`] call
/// (never a build gate). The sidecar is an OPTIMIZATION cache: absent/corrupt
/// simply forces one re-enumeration.
///
/// AD-402-6: Keying on the `.git/index` fingerprint means `git add` / `rm` /
/// `commit` / `checkout` — all of which rewrite `.git/index` — invalidate the
/// cache and force a fresh gix read. The memo skips only the re-parse, not the
/// top-K merge, so AD-379-1 build↔scan set-identity and PF-012 determinism
/// are unaffected.
fn tracked_files_memoized(root: &Path, cache_dir: Option<&Path>) -> Option<Vec<PathBuf>> {
    // Resolve the git index path, handling both regular repos (.git/ dir) and
    // linked worktrees (.git file pointing to the worktree-specific git dir).
    // Without this, a linked worktree would always fail the stat below and fall
    // through to a live list_tracked_files call on every query.
    //
    // Fail-soft: if resolve_git_index_path returns None (non-UTF-8 gitdir path,
    // IO error reading the .git pointer file, or unexpected format), fall through
    // to a live list_tracked_files call rather than propagating None out of this
    // function and silently dropping the entire tracked-union (AD-402-3).
    let Some(git_index_path) = resolve_git_index_path(root) else {
        return list_tracked_files(root);
    };

    // Stat the resolved index path for the fingerprint. If missing (newly-init
    // repo with no commits), fall through to a live call (which will also return
    // None from gix if the index truly does not exist).
    let Ok(git_meta) = fs::metadata(&git_index_path) else {
        return list_tracked_files(root);
    };
    // Use nanosecond-precision mtime for the fingerprint to close the 1-second
    // window where two same-second .git/index rewrites of equal byte length
    // would appear identical and serve a stale tracked list until the next
    // index change.  Sub-second precision makes same-nanosecond collisions
    // practically impossible.  Cast to u64: nanos since 1970 fit until 2554.
    // `mtime_from_meta` (which returns seconds) is NOT used here — it is only
    // for WalkEntry.mtime, compared against persisted manifest values in the
    // staleness scan.  Changing those to nanos would break the manifest format.
    // (finding: mtime-fingerprint-1s-granularity)
    //
    // Platform mtime unavailable: if mtime cannot be read the fingerprint would
    // degenerate to (0, len_bytes), making same-length rewrites of .git/index
    // appear identical → stale cache HIT. Skip the cache entirely on such
    // platforms and re-enumerate every call (correct; no optimization on this
    // platform). (finding: mtime-fingerprint-degenerate-zero)
    let Some(mtime_nanos) = git_meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
    else {
        return list_tracked_files(root);
    };
    let len_bytes = git_meta.len();

    // Use the caller-supplied cache dir if available (threaded from walk_metadata
    // to avoid a redundant canonicalize+SHA-256 per query); otherwise resolve it
    // here as a soft fallback.  AD-402-6: the sidecar is an optimization cache —
    // failure to locate the dir simply forces a live gix re-enumeration.
    let sidecar_path = match cache_dir {
        Some(d) => d.join("tracked_union.cache"),
        None => match resolve_search_cache_dir(root) {
            Ok(d) => d.join("tracked_union.cache"),
            Err(_) => return list_tracked_files(root),
        },
    };

    // Cache HIT path: read sidecar as raw bytes (byte-lossless for non-UTF-8
    // paths — see format_tracked_union_cache for rationale) and validate.
    if let Ok(content) = fs::read(&sidecar_path)
        && let Some(paths) = parse_tracked_union_cache(&content, mtime_nanos, len_bytes)
    {
        return Some(paths);
    }

    // Cache MISS: enumerate via gix, then rewrite the sidecar.
    let paths = list_tracked_files(root)?;
    // Fail-soft write — the sidecar is an optimization cache; if the write
    // fails (e.g., cache dir not yet created), the next call re-enumerates.
    let _ = fs::write(
        &sidecar_path,
        format_tracked_union_cache(mtime_nanos, len_bytes, &paths),
    );
    Some(paths)
}

/// Enumerate git-tracked repo-relative paths under `root`.
///
/// Returns `None` when `root` is not a git repo or when enumeration fails
/// (fail-soft — AD-402-3). The returned `Vec<PathBuf>` contains only
/// repo-toplevel-relative paths; all gix types are confined here.
///
/// AD-402-5: Reads the git index in-process with `gix` (Fork B —
/// user-resolved 2026-07-11). No subprocess, no git-binary dependency.
/// `gix 0.72` is already a workspace dep; only the `"index"` feature is
/// added. All gix types stay inside this function (gix-free `Vec<PathBuf>`
/// boundary, mirroring `rskim-search`'s type discipline). Manual
/// gitlink/skip-worktree filtering: no subprocess does this for you.
fn list_tracked_files(root: &Path) -> Option<Vec<PathBuf>> {
    #[cfg(test)]
    ENUM_CALL_COUNT.with(|c| c.set(c.get() + 1));

    use gix::bstr::ByteSlice as _;

    let repo = gix::open(root).ok()?;
    let index = repo.open_index().ok()?;
    // gix_index::File derefs to gix_index::State, which provides entries() and path().
    let state = &*index;

    let mut paths = Vec::new();
    for entry in state.entries() {
        // Skip gitlinks (submodules): mode COMMIT == 0o160000. A submodule
        // appears as a directory in the work tree, not an indexable file.
        if entry.mode == gix::index::entry::Mode::COMMIT {
            continue;
        }
        // Skip skip-worktree entries: the file is intentionally not
        // materialized in the work tree, so there is nothing to read.
        if entry
            .flags
            .contains(gix::index::entry::Flags::SKIP_WORKTREE)
        {
            continue;
        }
        // Convert repo-toplevel-relative BStr to PathBuf (lossy on non-UTF-8).
        // `.into_owned()` materialises `Cow<OsStr>` into `OsString`, giving
        // `PathBuf::from` a single unambiguous impl to call.
        let raw = entry.path(state);
        let path = PathBuf::from(raw.to_os_str_lossy().into_owned());
        // Security: skip entries that would escape `root` after `root.join(&path)`.
        // Legitimate git index paths are always repo-toplevel-relative with no
        // `..` or absolute prefix; a crafted `.git/index` could otherwise cause
        // skim to read files outside the repository (path traversal / local
        // information disclosure). `classify_metadata_core`'s strip_prefix guard
        // is defense in depth. Delegates to the canonical shared guard
        // `crate::cmd::is_repo_relative_safe` (applies ADR-008) so all git-path
        // containment checks stay in one place and cannot drift.
        if !crate::cmd::is_repo_relative_safe(&path) {
            continue;
        }
        paths.push(path);
    }
    Some(paths)
}

/// Serialize a tracked-file list to the sidecar cache format.
///
/// Returns a `Vec<u8>` (raw bytes, not a UTF-8 string) so that paths
/// containing invalid UTF-8 bytes round-trip without data loss.  On Unix,
/// [`OsStrExt::as_bytes`] encodes each path as its raw bytes (byte-lossless);
/// on non-Unix platforms, which lack a byte-exact OsStr API, `to_string_lossy`
/// is used as before (a latent limitation of those platforms' path model).
///
/// Format: `{mtime_nanos} {len_bytes}\0` header (nanoseconds since UNIX epoch
/// for sub-second fingerprint precision), then one repo-relative path per
/// subsequent NUL-terminated record.
///
/// NUL framing (git's own convention) is used instead of newline framing so
/// that paths containing embedded `\n` characters round-trip correctly.
///
/// Byte-exact encoding is REQUIRED for AD-379-1 build↔scan set-identity: a
/// file with a non-UTF-8 name is indexed on a cache MISS (live gix read
/// returns the real bytes) but would fail `symlink_metadata` on a cache HIT if
/// its path bytes were corrupted by `to_string_lossy`'s U+FFFD substitution,
/// silently dropping it from the union (finding: sidecar-lossy-round-trip).
///
/// The file is an optimization cache with no format version; absent or corrupt
/// → re-enumerate (fail-soft). Existing sidecars written by prior code (which
/// used `to_string_lossy`) remain parse-compatible for UTF-8 paths (byte-
/// identical) and trigger a re-enumeration for any non-UTF-8 path (the
/// U+FFFD-encoded form produces a different path that fails `symlink_metadata`
/// on the next classify call, so the sidecar is effectively invalid for that
/// path and will be overwritten on the next git-index mutation).
fn format_tracked_union_cache(mtime_nanos: u64, len_bytes: u64, paths: &[PathBuf]) -> Vec<u8> {
    let mut out = format!("{mtime_nanos} {len_bytes}\0").into_bytes();
    for p in paths {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            out.extend_from_slice(p.as_os_str().as_bytes());
        }
        #[cfg(not(unix))]
        {
            // Non-Unix: OsStr has no byte-exact API; fall back to lossy UTF-8.
            out.extend_from_slice(p.to_string_lossy().as_bytes());
        }
        out.push(0); // NUL record terminator
    }
    out
}

/// Parse a sidecar cache produced by [`format_tracked_union_cache`].
///
/// Accepts raw bytes so that non-UTF-8 path bytes (written by
/// `format_tracked_union_cache` on Unix via `OsStrExt::as_bytes`) are
/// reconstructed byte-losslessly into `PathBuf`s via `OsStr::from_bytes`.
/// A `&str` input would require the caller to decode the file as UTF-8 first,
/// which reintroduces the lossy substitution we are trying to avoid
/// (finding: sidecar-lossy-round-trip).
///
/// Returns `Some(paths)` only when the header fingerprint matches
/// `(expected_mtime, expected_len)` exactly. Returns `None` on any mismatch
/// or parse error (corrupt/truncated sidecar → re-enumerate, fail-soft).
///
/// Paths are NUL-terminated records (see [`format_tracked_union_cache`]).
fn parse_tracked_union_cache(
    content: &[u8],
    expected_mtime: u64,
    expected_len: u64,
) -> Option<Vec<PathBuf>> {
    let mut parts = content.split(|&b| b == 0);
    // Header is always ASCII digits + space; parse as UTF-8.
    let header = std::str::from_utf8(parts.next()?).ok()?;
    let mut hparts = header.splitn(2, ' ');
    let mtime: u64 = hparts.next()?.parse().ok()?;
    let len: u64 = hparts.next()?.parse().ok()?;
    if mtime != expected_mtime || len != expected_len {
        return None; // fingerprint mismatch → cache miss
    }
    // Trailing NUL produces an empty final slice; filter it out.
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        Some(
            parts
                .filter(|s| !s.is_empty())
                .map(|b| PathBuf::from(std::ffi::OsStr::from_bytes(b)))
                .collect(),
        )
    }
    #[cfg(not(unix))]
    {
        Some(
            parts
                .filter(|s| !s.is_empty())
                .map(|b| PathBuf::from(String::from_utf8_lossy(b).into_owned()))
                .collect(),
        )
    }
}

// ============================================================================
// Bounded top-K collection (shared — #379 at-cap determinism fix)
// ============================================================================

/// Outcome of classifying a single [`ignore::DirEntry`], generic over the
/// accepted-entry type `T`.
///
/// `T` is [`WalkEntry`] for the metadata-only production walk
/// ([`classify_entry_metadata`]) or [`ReadFile`] for the test-only
/// content-reading walk ([`classify_entry`]). `Transparent` covers non-file
/// entries (directories, symlinks) that should be silently skipped without
/// recording a reason.
enum ClassifyOutcome<T> {
    /// Entry is a readable source file ready to be added to the index.
    Accept(T),
    /// Entry should be skipped with a recorded reason.
    Skip(SkipReason),
    /// Entry is not a regular file; skip silently.
    Transparent,
}

/// An accepted entry paired with its precomputed [`normalize_rel_path`] sort key.
///
/// `Ord`/`PartialOrd` compare ONLY `key`, in natural ascending `str` order
/// (the field is stored rather than recomputed on every heap comparison).
/// [`BinaryHeap`] is a MAX-heap, so `heap.pop()` removes the entry with the
/// LARGEST key — exactly the one to evict to retain the smallest `max_files`
/// keys.
///
/// This is the core of the #379 at-cap determinism fix: selection is "the
/// `max_files` smallest keys over the complete walked set" (an
/// order-invariant SET function), never "the first `max_files` visited"
/// (which depended on parallel-walk thread scheduling).
struct KeyedEntry<T> {
    key: String,
    entry: T,
}

impl<T> KeyedEntry<T> {
    fn new(key: String, entry: T) -> Self {
        Self { key, entry }
    }
}

impl<T> PartialEq for KeyedEntry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl<T> Eq for KeyedEntry<T> {}

impl<T> PartialOrd for KeyedEntry<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for KeyedEntry<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}

/// Shared mutable state for one [`collect_bounded_topk`] parallel walk.
///
/// Grouped into a struct (rather than four separate parameters) so
/// [`handle_bounded_entry`] stays under Clippy's `too_many_arguments`
/// threshold; the fields are always used together as one logical unit — the
/// in-progress bounded top-K collector state.
struct BoundedCollector<'a, T> {
    heap: &'a Mutex<BinaryHeap<KeyedEntry<T>>>,
    skipped: &'a Mutex<Vec<SkipReason>>,
    /// Count of every accepted entry seen so far, INCLUDING ones later
    /// evicted from the heap. Used only for the post-walk reporting-only
    /// [`SkipReason::CapReached`] signal — never consulted to decide
    /// [`WalkState::Continue`] vs `Quit` (the walk always continues).
    total_seen: &'a AtomicUsize,
    max_files: usize,
}

/// Walk `root` in parallel via `builder`, classify each entry with
/// `classify`, and retain only the `max_files` entries with the smallest
/// `key_of` keys.
///
/// Shared by [`walk_metadata`] (production) and [`walk_and_read`] (test-only):
/// both were previously hand-copied instances of the same cap-during-walk
/// race, where a shared atomic counter triggered `WalkState::Quit` once
/// `max_files` was reached, terminating the ENTIRE parallel walk. That made
/// outcome MEMBERSHIP — not just order — depend on thread scheduling: the
/// prior sort-before-truncate fix (#379/AD-379-7) sorted a set whose
/// membership was already race-dependent, which cannot restore determinism.
/// Consolidating both walkers onto this one function means the
/// deterministic-selection guarantee now lives in exactly one place.
///
/// # Determinism
///
/// The walk always visits the COMPLETE tree — [`handle_bounded_entry`]
/// unconditionally returns [`WalkState::Continue`] regardless of the cap —
/// so retained membership is a pure function of each entry's `key_of` value,
/// never of visitation order or thread scheduling.
///
/// # Memory bound
///
/// Resident memory stays O(max_files) by construction: the heap evicts its
/// largest key whenever it grows past `max_files` (never an unbounded
/// collect-then-truncate).
///
/// # Errors
///
/// Returns `Err` only if the shared state `Arc`s still have outstanding
/// clones after the parallel walk completes — a walker-internal invariant
/// violation, not a per-file error.
fn collect_bounded_topk<T, C, K>(
    builder: WalkBuilder,
    max_files: usize,
    root: &Path,
    classify: C,
    key_of: K,
) -> anyhow::Result<(Vec<T>, Vec<SkipReason>)>
where
    T: Send,
    C: Fn(&ignore::DirEntry, &Path) -> ClassifyOutcome<T> + Copy + Send,
    K: Fn(&T) -> String + Copy + Send,
{
    let heap: Arc<Mutex<BinaryHeap<KeyedEntry<T>>>> = Arc::new(Mutex::new(
        BinaryHeap::with_capacity(max_files.min(4096).saturating_add(1)),
    ));
    let skipped = Arc::new(Mutex::new(Vec::<SkipReason>::with_capacity(256)));
    let total_seen = Arc::new(AtomicUsize::new(0));
    let root_buf = root.to_path_buf();

    builder.build_parallel().run(|| {
        let heap = Arc::clone(&heap);
        let skipped = Arc::clone(&skipped);
        let total_seen = Arc::clone(&total_seen);
        let root = root_buf.clone();
        Box::new(move |entry_result| {
            let collector = BoundedCollector {
                heap: &heap,
                skipped: &skipped,
                total_seen: &total_seen,
                max_files,
            };
            handle_bounded_entry(entry_result, &collector, &root, classify, key_of)
        })
    });

    let heap = Arc::try_unwrap(heap)
        .map_err(|_| anyhow::anyhow!("heap Arc still has multiple owners after walker completion"))?
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());
    let mut skipped = Arc::try_unwrap(skipped)
        .map_err(|_| {
            anyhow::anyhow!("skipped Arc still has multiple owners after walker completion")
        })?
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());

    // Reporting-only cap signal (AD-379-7 successor): derived from a
    // total-seen counter AFTER the complete walk, so it can NEVER control
    // termination. `total_seen` counts every accepted entry, including ones
    // later evicted from the heap; if it exceeds max_files, the cap was in
    // effect.
    let seen = total_seen.load(Ordering::Relaxed);
    if seen > max_files && skipped.len() < MAX_SKIP_REASONS {
        skipped.push(SkipReason::CapReached);
    }

    // Drain the heap and sort ascending by key: the K smallest keys over the
    // complete walked set, deterministic regardless of walk/thread-scheduling
    // order (fix for #379).
    let mut sorted: Vec<KeyedEntry<T>> = heap.into_vec();
    sorted.sort_by(|a, b| a.key.cmp(&b.key));
    let entries: Vec<T> = sorted.into_iter().map(|k| k.entry).collect();
    debug_assert!(entries.len() <= max_files);

    Ok((entries, skipped))
}

/// Process a single walker entry result against the shared bounded top-K
/// state.
///
/// Generic over the accepted-entry type `T` so [`walk_metadata`]
/// (`T = WalkEntry`) and [`walk_and_read`] (`T = ReadFile`) share one
/// implementation of the deterministic at-cap selection (#379 fix).
///
/// # Determinism (fix for #379)
///
/// Always returns [`WalkState::Continue`] — the walk must visit the COMPLETE
/// tree. The pre-fix code returned `WalkState::Quit` once a shared atomic
/// counter reached `max_files`, terminating the whole parallel walk and
/// making retained-set MEMBERSHIP depend on thread-scheduling order.
///
/// # Mutex poisoning
///
/// All `.lock()` calls use `unwrap_or_else(|e| e.into_inner())` so that a
/// panic in one parallel thread does not cascade-abort the remaining threads
/// via a poisoned-lock panic.
fn handle_bounded_entry<T, C, K>(
    entry_result: Result<ignore::DirEntry, ignore::Error>,
    collector: &BoundedCollector<'_, T>,
    root: &Path,
    classify: C,
    key_of: K,
) -> WalkState
where
    C: Fn(&ignore::DirEntry, &Path) -> ClassifyOutcome<T>,
    K: Fn(&T) -> String,
{
    match entry_result {
        Ok(entry) => match classify(&entry, root) {
            ClassifyOutcome::Accept(item) => {
                collector.total_seen.fetch_add(1, Ordering::Relaxed);
                let key = key_of(&item);
                let mut guard = collector.heap.lock().unwrap_or_else(|e| e.into_inner());
                guard.push(KeyedEntry::new(key, item));
                if guard.len() > collector.max_files {
                    guard.pop();
                }
                debug_assert!(guard.len() <= collector.max_files);
            }
            ClassifyOutcome::Skip(reason) => {
                let mut guard = collector.skipped.lock().unwrap_or_else(|e| e.into_inner());
                if guard.len() < MAX_SKIP_REASONS {
                    guard.push(reason);
                }
            }
            ClassifyOutcome::Transparent => {}
        },
        Err(err) => {
            let path = match &err {
                ignore::Error::WithPath { path, .. } => path.clone(),
                _ => PathBuf::new(),
            };
            let mut guard = collector.skipped.lock().unwrap_or_else(|e| e.into_inner());
            if guard.len() < MAX_SKIP_REASONS {
                guard.push(SkipReason::ReadError {
                    path,
                    error: err.to_string(),
                });
            }
        }
    }
    WalkState::Continue
}

/// Compute the [`normalize_rel_path`] sort key for a [`WalkEntry`].
fn walk_entry_key(e: &WalkEntry) -> String {
    normalize_rel_path(&e.rel_path)
}

/// Compute the [`normalize_rel_path`] sort key for a [`ReadFile`].
#[cfg(test)]
fn read_file_key(e: &ReadFile) -> String {
    normalize_rel_path(&e.rel_path)
}

// ============================================================================
// Private helpers
// ============================================================================

/// Extract mtime from a `DirEntry` as seconds since UNIX_EPOCH.
///
/// Returns `None` if the platform does not expose mtime or the syscall fails.
/// Used as a fast pre-screening hint; SHA-256 is always the correctness
/// guarantee for cache invalidation.
///
/// Only called from the test-only [`classify_entry`]. Production code
/// ([`classify_entry_metadata`]) captures a single [`std::fs::Metadata`] for
/// both size pre-screening and mtime extraction to avoid double syscalls.
/// Delegates to [`mtime_from_meta`] (the shared helper) to keep the two paths
/// byte-identical and avoid a second diverging copy of the same chain
/// (finding: mtime-secs-test-divergence).
#[cfg(test)]
fn mtime_secs(entry: &ignore::DirEntry) -> Option<u64> {
    entry.metadata().ok().as_ref().and_then(mtime_from_meta)
}

/// Configure a [`WalkBuilder`] with the project-standard ignore rules.
///
/// On a git root (`root/.git` exists`) the ignore-walk output produced by this
/// builder is subsequently **unioned** with git-tracked files in
/// [`walk_metadata`] (see [`merge_tracked_union`]). The builder alone is not
/// the authoritative source of indexable files on git roots.
fn configure_builder(builder: &mut WalkBuilder) {
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .require_git(false)
        .follow_links(false);
}

/// Open `path`, verify its on-disk size via the file handle (not a separate
/// `stat(2)` call), then read it into a `String`.
///
/// Using the file handle for both the metadata check and the read prevents the
/// TOCTOU race where a file could be swapped between the size check and the
/// actual read.
///
/// Returns a [`ReadOutcome`] variant rather than an `io::Error` so that the
/// caller can match on typed cases without inspecting error message text.
pub(super) fn open_and_read(path: &Path) -> ReadOutcome {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return ReadOutcome::Io(e),
    };
    let meta = match file.metadata() {
        Ok(m) => m,
        Err(e) => return ReadOutcome::Io(e),
    };
    let size = meta.len();
    if size > MAX_FILE_BYTES {
        return ReadOutcome::TooLarge(size);
    }
    // Pre-size the buffer to avoid reallocation; +1 so read_to_string can
    // detect EOF without an extra allocation.
    // Safety: MAX_FILE_BYTES <= usize::MAX is guaranteed by the compile-time
    // assertion above, so this cast is sound.
    let mut content = String::with_capacity((size as usize).saturating_add(1));
    match file.read_to_string(&mut content) {
        Ok(_) => ReadOutcome::Content(content),
        // read_to_string returns InvalidData for non-UTF-8 content.
        Err(e) if e.kind() == std::io::ErrorKind::InvalidData => ReadOutcome::NonUtf8,
        Err(e) => ReadOutcome::Io(e),
    }
}

/// Two-signal minified gate: returns `Some(avg_line_bytes)` when content is
/// classified as minified, `None` when it should be indexed.
///
/// AD-395-1: A file is minified only when BOTH signals hold:
/// 1. **Size gate** — `content.len() >= MINIFY_MIN_BYTES` (= 64 KiB).
///    Small single-long-line files (data-in-code, generated, long-path/prose,
///    the 1.4 KiB ticket repro) fail this gate and are INDEXED.
/// 2. **Single-line signal** — the first `MINIFY_PROBE_BYTES` probe has
///    `newline_count <= 1`.  Large multi-line files that merely contain some
///    long lines (e.g. 200 × 600-byte lines) also fail this signal and are
///    INDEXED.  Only genuine single-line bundles (one giant line across the
///    whole probe) pass.
///
/// When both signals hold, `avg_line_bytes` is computed over the **whole
/// file** (not just the probe) and returned in `Some` so the skip message can
/// print a meaningful number (e.g. `"avg line 70000 > 500 bytes"` for a
/// 70 KB bundle, rather than the degenerate `"avg line 8192"` that a
/// probe-only average would produce).
///
/// The whole-file average introduces a third, implicit gate: if the file has
/// enough newlines after the single-line probe region that the full average
/// falls at or below `MINIFY_AVG_LINE_BYTES` (500), the function returns
/// `None` and the file is **indexed** rather than skipped.  This affects
/// so-called "blob-then-code" residuals — files whose first 8 KiB is a
/// single long line but whose remainder has many normal lines.  That behaviour
/// is correct and intentional (recall-positive), but it diverges from the
/// plan's OD-395-2 note that such files would be "dropped wholesale (now
/// NAMED)"; readers of OD-395-2 should treat those files as indexed.
pub(super) fn minified_metric(content: &str) -> Option<usize> {
    // Signal 1: size gate (AD-395-1).
    if content.len() < MINIFY_MIN_BYTES {
        return None;
    }

    let probe_len = content.len().min(MINIFY_PROBE_BYTES);
    // probe_len <= content.len(), so the slice is always in-bounds.
    let probe = &content.as_bytes()[..probe_len];
    let newline_count = probe.iter().filter(|&&b| b == b'\n').count();

    // Signal 2: effectively single-line probe (AD-395-1).
    if newline_count > 1 {
        return None;
    }

    // Both signals hold → compute the TRUE average line length over the WHOLE
    // file for the skip message (AD-395-1 / AD-395-6 observability).
    //
    // Using `probe.len() / newline_count` produces a degenerate constant
    // (always 8192 when newline_count ∈ {0, 1}, probe_len == 8192), which
    // hides the actual file size in the "avg line N > 500 bytes" notice.
    // A genuine single-line bundle (e.g. 70 KB of `"x".repeat(70_000)`)
    // should display "avg line 70000 > 500 bytes", not "avg line 8192".
    //
    // Counting over the full content is safe here: we are already past both
    // gate checks (size ≥ 64 KiB and probe is single-line) and are about to
    // skip the file anyway, so a linear scan is not on the hot path.
    let full_newlines = content.bytes().filter(|&b| b == b'\n').count();
    let avg_line_bytes = content.len() / (full_newlines + 1);

    if avg_line_bytes > MINIFY_AVG_LINE_BYTES {
        Some(avg_line_bytes)
    } else {
        None
    }
}

/// Resolve the per-project search cache directory.
///
/// Path: `{base_cache}/search/{sha256(canonical_root)[..16]}/`.
/// The base is `SKIM_CACHE_DIR` (if set) or the platform cache dir.
///
/// AD-400-2 (supersedes AD-381-2): callers reach here only AFTER
/// `resolve_root_and_cache` has validated that the root exists and is a
/// directory (#400), so a `canonicalize()` failure now signals a real error
/// and is propagated — the former pure-lexical fallback for non-existent roots
/// (`canonical_or_normalized` / `lexically_normalize`) is removed. This
/// remains a defensive backstop: any *internal* caller (e.g. the test-only
/// `IndexCli` path, or `build_index_rechecked` with `cache_dir_override: None`)
/// that supplies a non-existent root now fails loud here instead of silently
/// hashing a ghost path.
///
/// Moved from `index.rs` to `walk.rs` so that both the discovery module (walk)
/// and the build module (index) depend on this shared path helper without
/// introducing a bidirectional import cycle (finding: walk-index-cyclic-coupling).
pub(super) fn resolve_search_cache_dir(root: &Path) -> anyhow::Result<PathBuf> {
    let base = crate::cmd::resolve_cache_dir()
        .ok_or_else(|| anyhow::anyhow!("failed to resolve skim cache directory"))?;
    let canonical = root.canonicalize().map_err(|e| {
        anyhow::anyhow!("failed to canonicalize search root {}: {e}", root.display())
    })?;
    Ok(base.join("search").join(project_root_hash(&canonical)))
}

/// Compute a 16-char hex prefix of the SHA-256 of the canonical project root path.
///
/// Used as a stable directory name in the search cache.
fn project_root_hash(canonical_root: &Path) -> String {
    let input = canonical_root.to_string_lossy();
    sha256_hex(input.as_bytes())[..16].to_string()
}

/// Compute the SHA-256 of `data` and return it as a 64-character lowercase hex string.
///
/// Uses a const nibble lookup table instead of `write!` format calls to avoid
/// per-byte `fmt::Write` overhead on the hot path (called once per indexed file).
pub(super) fn sha256_hex(data: &[u8]) -> String {
    const NIBBLES: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(data);
    let mut hex = vec![0u8; 64];
    for (i, byte) in digest.iter().enumerate() {
        hex[i * 2] = NIBBLES[(byte >> 4) as usize];
        hex[i * 2 + 1] = NIBBLES[(byte & 0x0f) as usize];
    }
    // NIBBLES contains only ASCII hex characters, so hex is always valid UTF-8.
    String::from_utf8(hex).expect("hex nibbles are always valid UTF-8")
}

// ============================================================================
// Tests (co-located in walk_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "walk_tests.rs"]
mod tests;
