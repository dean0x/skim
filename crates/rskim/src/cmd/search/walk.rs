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
//! # Skip conditions (in order checked)
//!
//! | Condition | Threshold |
//! |-----------|-----------|
//! | Unsupported language | `Language::from_path()` returns `None` |
//! | File too large | > 5 MB (`metadata.len()`) |
//! | Non-UTF8 | `read_to_string()` returns `Err` |
//! | Minified | avg line > 500 bytes in first 8 KB (tree-sitter langs only) |
//! | Cap reached | total accepted entries exceed `max_files` (reporting only — computed AFTER the complete walk; never terminates it) |

use std::collections::BinaryHeap;
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

/// Maximum number of ancestors to traverse when looking for a `.git` root.
/// 256 ancestors is far beyond any real filesystem depth.
const MAX_ANCESTORS: usize = 256;

/// Maximum number of skip reasons collected during a walk.
///
/// Large monorepos may encounter millions of unsupported files.  Collecting an
/// unbounded path list wastes memory; callers only need a representative sample
/// for diagnostics.  Once the cap is hit, [`SkipReason::CapReached`] entries
/// are still appended so the caller knows truncation occurred.
const MAX_SKIP_REASONS: usize = 10_000;

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
/// # Errors
///
/// Returns `Err` if `start` cannot be canonicalized.
pub(super) fn discover_project_root(start: &Path) -> anyhow::Result<PathBuf> {
    let canonical = start
        .canonicalize()
        .with_context(|| format!("failed to canonicalize path: {}", start.display()))?;

    let mut current = canonical.as_path();
    for _ in 0..MAX_ANCESTORS {
        if current.join(".git").exists() {
            return Ok(current.to_path_buf());
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }

    // No .git found — fall back to the canonical form of the provided path.
    Ok(canonical)
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
    if !lang.is_serde_based() && is_minified(&content) {
        return ClassifyOutcome::Skip(SkipReason::Minified(abs_path.to_path_buf()));
    }

    let mtime = mtime_secs(entry);
    let rel_path = abs_path
        .strip_prefix(root)
        .unwrap_or(abs_path)
        .to_path_buf();

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

/// Classify a single directory entry without reading its content.
///
/// Checks: file type, language detection, fast size pre-screen (DirEntry
/// metadata).  No I/O beyond the metadata already cached by the walker.
fn classify_entry_metadata(entry: &ignore::DirEntry, root: &Path) -> ClassifyOutcome<WalkEntry> {
    let file_type = match entry.file_type() {
        Some(ft) => ft,
        None => return ClassifyOutcome::Transparent,
    };
    if !file_type.is_file() {
        return ClassifyOutcome::Transparent;
    }

    let abs_path = entry.path();

    // Language detection.
    let lang = match Language::from_path(abs_path) {
        Some(l) => l,
        None => {
            return ClassifyOutcome::Skip(SkipReason::UnsupportedLanguage(abs_path.to_path_buf()));
        }
    };

    // Capture metadata once; use it for the size pre-screen, the recorded
    // size (AD-379-2), and mtime extraction so the walker never calls
    // entry.metadata() twice per file.
    let meta_opt = entry.metadata().ok();
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

    let mtime = meta_opt.and_then(|m| {
        m.modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
    });
    let rel_path = abs_path
        .strip_prefix(root)
        .unwrap_or(abs_path)
        .to_path_buf();

    ClassifyOutcome::Accept(WalkEntry {
        abs_path: abs_path.to_path_buf(),
        rel_path,
        lang,
        mtime,
        size,
    })
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
) -> anyhow::Result<(Vec<WalkEntry>, Vec<SkipReason>)> {
    let mut builder = WalkBuilder::new(root);
    configure_builder(&mut builder);

    collect_bounded_topk(
        builder,
        max_files,
        root,
        classify_entry_metadata,
        walk_entry_key,
    )
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
#[cfg(test)]
fn mtime_secs(entry: &ignore::DirEntry) -> Option<u64> {
    entry
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| {
            t.duration_since(std::time::SystemTime::UNIX_EPOCH)
                .ok()
                .map(|d| d.as_secs())
        })
}

/// Configure a [`WalkBuilder`] with the project-standard ignore rules.
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

/// Returns `true` if the content appears minified.
///
/// Minification heuristic: probe the first [`MINIFY_PROBE_BYTES`] bytes. If
/// they contain no newlines, or the average bytes-per-line exceeds
/// [`MINIFY_AVG_LINE_BYTES`], the file is considered minified.
pub(super) fn is_minified(content: &str) -> bool {
    let probe_len = content.len().min(MINIFY_PROBE_BYTES);
    // probe_len <= content.len(), so the slice is always in-bounds.
    let probe = &content.as_bytes()[..probe_len];
    let newline_count = probe.iter().filter(|&&b| b == b'\n').count();
    if newline_count == 0 {
        return probe.len() > MINIFY_AVG_LINE_BYTES;
    }
    probe.len() / newline_count > MINIFY_AVG_LINE_BYTES
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
