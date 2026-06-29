//! Project-root discovery and recursive file walking for the index builder.
//!
//! # File cap
//!
//! `walk_metadata` (production) and `walk_and_read` (tests) stop after
//! `max_files` files have been accepted. Skipped files (unsupported language,
//! too large, non-UTF8) do not count toward the cap.
//!
//! # Skip conditions (in order checked)
//!
//! | Condition | Threshold |
//! |-----------|-----------|
//! | Unsupported language | `Language::from_path()` returns `None` |
//! | File too large | > 5 MB (`metadata.len()`) |
//! | Non-UTF8 | `read_to_string()` returns `Err` |
//! | Minified | avg line > 500 bytes in first 8 KB (tree-sitter langs only) |
//! | Cap reached | `max_files` exceeded |

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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

/// Outcome of classifying a single [`ignore::DirEntry`].
///
/// `Transparent` covers non-file entries (directories, symlinks) that should be
/// silently skipped without recording a reason.
#[cfg(test)]
enum EntryOutcome {
    /// Entry is a readable source file ready to be added to the index.
    Accept(ReadFile),
    /// Entry should be skipped with a recorded reason.
    Skip(SkipReason),
    /// Entry is not a regular file; skip silently.
    Transparent,
}

/// Classify a single directory entry into an [`EntryOutcome`].
///
/// Handles language detection, size pre-screening (via cached `DirEntry`
/// metadata), file reading, and minification detection.  The caller is
/// responsible for the file-count cap and for guarding the `skipped` vector
/// length against [`MAX_SKIP_REASONS`].
#[cfg(test)]
fn classify_entry(entry: &ignore::DirEntry, root: &Path) -> EntryOutcome {
    // Only process regular files.
    let file_type = match entry.file_type() {
        Some(ft) => ft,
        None => return EntryOutcome::Transparent,
    };
    if !file_type.is_file() {
        return EntryOutcome::Transparent;
    }

    let abs_path = entry.path();

    // --- Unsupported language ---
    let lang = match Language::from_path(abs_path) {
        Some(l) => l,
        None => return EntryOutcome::Skip(SkipReason::UnsupportedLanguage(abs_path.to_path_buf())),
    };

    // --- Fast size pre-screen using DirEntry cached metadata ---
    // entry.metadata() avoids an extra stat(2) syscall on 50 K-file repos.
    // If it fails we fall through and let the open() path handle the error.
    if let Ok(meta) = entry.metadata() {
        let size = meta.len();
        if size > MAX_FILE_BYTES {
            return EntryOutcome::Skip(SkipReason::TooLarge {
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
            return EntryOutcome::Skip(SkipReason::NonUtf8(abs_path.to_path_buf()));
        }
        ReadOutcome::TooLarge(size) => {
            // File grew past the limit between the pre-screen and open.
            return EntryOutcome::Skip(SkipReason::TooLarge {
                path: abs_path.to_path_buf(),
                size,
            });
        }
        ReadOutcome::Io(e) => {
            return EntryOutcome::Skip(SkipReason::ReadError {
                path: abs_path.to_path_buf(),
                error: e.to_string(),
            });
        }
    };

    // --- Minification check (tree-sitter languages only) ---
    // Serde-based languages (JSON, YAML, TOML) produce long lines by design;
    // skip the minification check for them.
    if !lang.is_serde_based() && is_minified(&content) {
        return EntryOutcome::Skip(SkipReason::Minified(abs_path.to_path_buf()));
    }

    let mtime = mtime_secs(entry);
    let rel_path = abs_path
        .strip_prefix(root)
        .unwrap_or(abs_path)
        .to_path_buf();

    EntryOutcome::Accept(ReadFile {
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
/// # Ordering
///
/// Files are returned in lexicographic path order (sorted after parallel
/// collection for deterministic output).
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
    let files = Arc::new(Mutex::new(Vec::with_capacity(max_files.min(4096))));
    let skipped = Arc::new(Mutex::new(Vec::<SkipReason>::with_capacity(256)));
    let file_count = Arc::new(AtomicUsize::new(0));
    let cap_reached = Arc::new(AtomicBool::new(false));
    let root_buf = root.to_path_buf();

    let mut builder = WalkBuilder::new(root);
    configure_builder(&mut builder);

    builder.build_parallel().run(|| {
        let files = Arc::clone(&files);
        let skipped = Arc::clone(&skipped);
        let file_count = Arc::clone(&file_count);
        let cap_reached = Arc::clone(&cap_reached);
        let root = root_buf.clone();
        Box::new(move |entry_result| {
            handle_entry(
                entry_result,
                &files,
                &skipped,
                &file_count,
                &cap_reached,
                max_files,
                &root,
            )
        })
    });

    let mut files = Arc::try_unwrap(files)
        .map_err(|_| {
            anyhow::anyhow!("files Arc still has multiple owners after walker completion")
        })?
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());
    let skipped = Arc::try_unwrap(skipped)
        .map_err(|_| {
            anyhow::anyhow!("skipped Arc still has multiple owners after walker completion")
        })?
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());

    // Parallel threads may over-collect beyond max_files due to TOCTOU on the
    // atomic counter (multiple threads may pass the cap check before any of them
    // increments it).
    files.truncate(max_files);

    // AD-373-1 (ref): Sort by the same byte-wise normalized-string order as
    // `walk_metadata` / the manifest BTreeMap<String> so this test-only walker
    // and the production walker assign FileIds in the identical order.
    files.sort_by_key(|a| normalize_rel_path(&a.rel_path));

    Ok((files, skipped))
}

// ============================================================================
// Metadata-only walk (streaming pipeline)
// ============================================================================

/// Outcome of classifying a single [`ignore::DirEntry`] without reading content.
///
/// Used by [`walk_metadata`] / [`classify_entry_metadata`] which perform language
/// detection and fast size pre-screening only.  Content reading is deferred to
/// the streaming producer.
enum MetaOutcome {
    Accept(WalkEntry),
    Skip(SkipReason),
    Transparent,
}

/// Classify a single directory entry without reading its content.
///
/// Checks: file type, language detection, fast size pre-screen (DirEntry
/// metadata).  No I/O beyond the metadata already cached by the walker.
fn classify_entry_metadata(entry: &ignore::DirEntry, root: &Path) -> MetaOutcome {
    let file_type = match entry.file_type() {
        Some(ft) => ft,
        None => return MetaOutcome::Transparent,
    };
    if !file_type.is_file() {
        return MetaOutcome::Transparent;
    }

    let abs_path = entry.path();

    // Language detection.
    let lang = match Language::from_path(abs_path) {
        Some(l) => l,
        None => {
            return MetaOutcome::Skip(SkipReason::UnsupportedLanguage(abs_path.to_path_buf()));
        }
    };

    // Capture metadata once; use it for both the size pre-screen and mtime
    // extraction so the walker never calls entry.metadata() twice per file.
    let meta_opt = entry.metadata().ok();
    if let Some(ref meta) = meta_opt {
        let size = meta.len();
        if size > MAX_FILE_BYTES {
            return MetaOutcome::Skip(SkipReason::TooLarge {
                path: abs_path.to_path_buf(),
                size,
            });
        }
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

    MetaOutcome::Accept(WalkEntry {
        abs_path: abs_path.to_path_buf(),
        rel_path,
        lang,
        mtime,
    })
}

/// Walk `root` recursively, collecting file metadata without reading content.
///
/// Returns a sorted list of [`WalkEntry`]s and collected [`SkipReason`]s.
/// Content reading is deferred to the streaming producer in the index pipeline.
///
/// # Ordering
///
/// Entries are sorted by [`normalize_rel_path`] (byte-wise `str` comparison of
/// the normalized rel-path string) after parallel collection, giving
/// deterministic FileId assignment in the consumer.  The sort key is
/// byte-identical to the manifest `BTreeMap<String>` key, so `FileId(n)` in the
/// walk corresponds to `sorted_paths()[n]` in the manifest — the invariant that
/// all five FileId consumers depend on.
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
    let entries = Arc::new(Mutex::new(Vec::with_capacity(max_files.min(4096))));
    let skipped = Arc::new(Mutex::new(Vec::<SkipReason>::with_capacity(256)));
    let entry_count = Arc::new(AtomicUsize::new(0));
    let cap_reached = Arc::new(AtomicBool::new(false));
    let root_buf = root.to_path_buf();

    let mut builder = WalkBuilder::new(root);
    configure_builder(&mut builder);

    builder.build_parallel().run(|| {
        let entries = Arc::clone(&entries);
        let skipped = Arc::clone(&skipped);
        let entry_count = Arc::clone(&entry_count);
        let cap_reached = Arc::clone(&cap_reached);
        let root = root_buf.clone();
        Box::new(move |entry_result| {
            handle_metadata_entry(
                entry_result,
                &entries,
                &skipped,
                &entry_count,
                &cap_reached,
                max_files,
                &root,
            )
        })
    });

    let mut entries = Arc::try_unwrap(entries)
        .map_err(|_| {
            anyhow::anyhow!("entries Arc still has multiple owners after walker completion")
        })?
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());
    let skipped = Arc::try_unwrap(skipped)
        .map_err(|_| {
            anyhow::anyhow!("skipped Arc still has multiple owners after walker completion")
        })?
        .into_inner()
        .unwrap_or_else(|e| e.into_inner());

    // Parallel threads may over-collect beyond max_files due to TOCTOU on the
    // atomic counter.
    entries.truncate(max_files);

    // AD-373-1: Sort by the byte-wise normalized-string order that the manifest
    // BTreeMap<String> uses for key ordering. PathBuf::cmp is component-aware
    // and diverges from str::cmp on nested dirs (foo/bar.rs vs foo.rs), which
    // mis-resolved FileId→path (#373). normalize_rel_path produces the exact
    // byte string stored as the manifest key (index.rs `consume`) so assignment
    // order and resolution order are byte-identical.
    entries.sort_by_key(|a| normalize_rel_path(&a.rel_path));

    Ok((entries, skipped))
}

// ============================================================================
// Walker entry handlers
// ============================================================================

/// Process a single walker entry result for the metadata-only walk.
///
/// Extracted from the [`walk_metadata`] `build_parallel` closure to reduce
/// nesting depth and enable independent unit testing.  The parallel walker API
/// requires a `Box<dyn FnMut(…) -> WalkState>` closure; this function holds
/// all the logic so the closure is a thin delegation layer.
///
/// Mirrors [`handle_entry`] (the equivalent helper for [`walk_and_read`]).
///
/// # Mutex poisoning
///
/// All `.lock()` calls use `unwrap_or_else(|e| e.into_inner())` so that a
/// panic in one parallel thread does not cascade-abort the remaining threads
/// via a poisoned-lock panic.
fn handle_metadata_entry(
    entry_result: Result<ignore::DirEntry, ignore::Error>,
    entries: &Mutex<Vec<WalkEntry>>,
    skipped: &Mutex<Vec<SkipReason>>,
    entry_count: &AtomicUsize,
    cap_reached: &AtomicBool,
    max_files: usize,
    root: &Path,
) -> WalkState {
    if entry_count.load(Ordering::Relaxed) >= max_files {
        if !cap_reached.swap(true, Ordering::Relaxed) {
            let mut guard = skipped.lock().unwrap_or_else(|e| e.into_inner());
            if guard.len() < MAX_SKIP_REASONS {
                guard.push(SkipReason::CapReached);
            }
        }
        return WalkState::Quit;
    }

    match entry_result {
        Ok(entry) => match classify_entry_metadata(&entry, root) {
            MetaOutcome::Accept(we) => {
                entry_count.fetch_add(1, Ordering::Relaxed);
                entries.lock().unwrap_or_else(|e| e.into_inner()).push(we);
            }
            MetaOutcome::Skip(reason) => {
                let mut guard = skipped.lock().unwrap_or_else(|e| e.into_inner());
                if guard.len() < MAX_SKIP_REASONS {
                    guard.push(reason);
                }
            }
            MetaOutcome::Transparent => {}
        },
        Err(err) => {
            let path = match &err {
                ignore::Error::WithPath { path, .. } => path.clone(),
                _ => PathBuf::new(),
            };
            let mut guard = skipped.lock().unwrap_or_else(|e| e.into_inner());
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

/// Process a single walker entry result and update shared state.
///
/// Extracted from the `build_parallel` closure to reduce nesting depth and
/// enable independent unit testing.  The parallel walker API requires a
/// `Box<dyn FnMut(…) -> WalkState>` closure; this function holds all the
/// logic so the closure is a thin delegation layer.
///
/// # Mutex poisoning
///
/// All `.lock()` calls use `unwrap_or_else(|e| e.into_inner())` so that a
/// panic in one parallel thread does not cascade-abort the remaining threads
/// via a poisoned-lock panic.
#[cfg(test)]
fn handle_entry(
    entry_result: Result<ignore::DirEntry, ignore::Error>,
    files: &Mutex<Vec<ReadFile>>,
    skipped: &Mutex<Vec<SkipReason>>,
    file_count: &AtomicUsize,
    cap_reached: &AtomicBool,
    max_files: usize,
    root: &Path,
) -> WalkState {
    if file_count.load(Ordering::Relaxed) >= max_files {
        if !cap_reached.swap(true, Ordering::Relaxed) {
            let mut guard = skipped.lock().unwrap_or_else(|e| e.into_inner());
            if guard.len() < MAX_SKIP_REASONS {
                guard.push(SkipReason::CapReached);
            }
        }
        return WalkState::Quit;
    }

    match entry_result {
        Ok(entry) => match classify_entry(&entry, root) {
            EntryOutcome::Accept(file) => {
                file_count.fetch_add(1, Ordering::Relaxed);
                files.lock().unwrap_or_else(|e| e.into_inner()).push(file);
            }
            EntryOutcome::Skip(reason) => {
                let mut guard = skipped.lock().unwrap_or_else(|e| e.into_inner());
                if guard.len() < MAX_SKIP_REASONS {
                    guard.push(reason);
                }
            }
            EntryOutcome::Transparent => {}
        },
        Err(err) => {
            let path = match &err {
                ignore::Error::WithPath { path, .. } => path.clone(),
                _ => PathBuf::new(),
            };
            let mut guard = skipped.lock().unwrap_or_else(|e| e.into_inner());
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
