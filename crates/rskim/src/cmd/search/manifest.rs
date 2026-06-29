//! JSONL manifest sidecar for the index builder.
//!
//! The manifest (`index.skfiles`) records, for each indexed file:
//! - its repo-relative path
//! - its content SHA-256
//! - the detected language
//! - the pre-computed field_map (encoded as `(start, end, field_discriminant)` triples)
//!
//! # Format
//!
//! The file is a JSONL stream with two sections:
//!
//! 1. **Header line** — a `ManifestHeader` JSON object.
//! 2. **Entry lines** — one `ManifestEntry` JSON object per indexed file.
//!
//! An empty or missing file is treated as a cold-start (no cache hits).
//!
//! # Atomicity
//!
//! Writes use a named temp file in the same directory, persisted (renamed) after
//! the full write succeeds. Readers never observe a partial write.
//!
//! # Wrong-root detection
//!
//! The header embeds the canonical project root. If the header root does not
//! match the `project_root` passed to `FileManifest::load`, the entire manifest
//! is discarded (returns an empty manifest).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, BufWriter, Write as IoWrite};
use std::path::PathBuf;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

// ============================================================================
// On-disk types
// ============================================================================

/// First line of the manifest JSONL stream — project metadata.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ManifestHeader {
    /// Format version — bump on breaking changes.
    pub version: u32,
    /// Canonical path of the project root when the manifest was written.
    pub root: String,
    /// The git HEAD commit SHA or ref that was current when the manifest was
    /// written. Used for staleness detection on subsequent queries.
    ///
    /// `serde(default)` preserves backward compat: old manifests without this
    /// field deserialize with `git_head: None`.
    #[serde(default)]
    pub git_head: Option<String>,
}

/// One line per indexed file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ManifestEntry {
    /// Repo-relative path (forward slashes, no leading `.`).
    pub path: String,
    /// Hex-encoded SHA-256 of the file content (64 lowercase hex chars).
    pub sha256: String,
    /// Language name as a lowercase string (e.g. `"rust"`, `"typescript"`).
    pub lang: String,
    /// Field map encoded as `(start_byte, end_byte, field_discriminant)` triples.
    pub field_map: Vec<(usize, usize, u8)>,
    /// File modification time as seconds since UNIX_EPOCH when the manifest was written.
    ///
    /// Used as a fast pre-screening hint to skip SHA computation when the file has not
    /// changed (mtime match → likely SHA match → reuse field_map).  SHA-256 is always
    /// computed on mtime mismatch or when this field is absent.
    ///
    /// `serde(default)` ensures backward compatibility: old manifests without this field
    /// deserialize with `mtime: None`.
    #[serde(default)]
    pub mtime: Option<u64>,
}

// ============================================================================
// Safety limits
// ============================================================================

/// Maximum number of entries accepted from a manifest file.
///
/// Guards against unbounded memory growth from a corrupted manifest with
/// millions of lines. 60 000 files far exceeds any realistic monorepo.
const MAX_MANIFEST_ENTRIES: usize = 60_000;

/// Maximum manifest file size accepted before parsing.
///
/// A single multi-gigabyte line (no newlines) would cause `BufReader::lines`
/// to allocate that entire line into a `String`. Reject oversized files up
/// front rather than discovering OOM mid-parse.
///
/// 256 MiB is several orders of magnitude larger than any realistic manifest.
const MAX_MANIFEST_FILE_BYTES: u64 = 256 * 1024 * 1024;

// ============================================================================
// Manifest store
// ============================================================================

/// In-memory manifest loaded from or written to `index.skfiles`.
pub(super) struct FileManifest {
    /// Project root this manifest was built for.
    project_root: PathBuf,
    /// Directory where the `index.skfiles` sidecar lives.
    cache_dir: PathBuf,
    /// Entries keyed by `ManifestEntry::path`, stored in sorted order via
    /// [`BTreeMap`] so that [`Self::sorted_paths`] and [`Self::save`] never
    /// need to sort the keys — iteration order is alphabetical by construction.
    entries: BTreeMap<String, ManifestEntry>,
    /// Git HEAD SHA stored when the manifest was last written.
    ///
    /// Set via [`Self::set_git_head`], persisted by [`Self::save`], and
    /// recovered by [`Self::stored_git_head`] after a [`Self::load`].
    git_head: Option<String>,
}

impl FileManifest {
    /// Manifest filename inside the cache directory.
    pub const MANIFEST_FILENAME: &'static str = "index.skfiles";

    /// Current format version — bump this on any breaking schema change.
    ///
    /// v1 → v2: Custom field mapping for JSON/YAML/TOML/Markdown (Issue #193).
    /// Existing v1 indexes must be re-indexed because field classifications have
    /// changed: previously all bytes were Other; now structural elements receive
    /// TypeDefinition, SymbolName, StringLiteral, etc.
    ///
    /// v2 → v3: Fix FileId↔path ordering skew (#373). The on-disk JSONL byte
    /// layout is unchanged, but the walk-side FileId assignment order now uses
    /// byte-wise comparison of `normalize_rel_path` (same order as this manifest's
    /// `BTreeMap<String>` key iteration) instead of `PathBuf::cmp` (component-
    /// aware, diverges on nested dirs). A pre-existing v2 manifest could have
    /// been built with the old, skewed ordering → it would silently serve wrong
    /// files even if otherwise fresh (unchanged git HEAD / mtimes). Bumping to v3
    /// makes the existing FORMAT_VERSION-mismatch staleness path detect every v2
    /// manifest as stale and rebuild it once on the next query — correctness-on-
    /// upgrade with no manual `--rebuild` required (AD-373-3).
    ///
    /// AD-373-3: bumped 2→3 for #373. The FileId↔path skew fix is in-memory-only
    /// (serialized layout unchanged), so an otherwise-fresh v2 index would keep
    /// serving skewed FileIds (wrong files) with no self-heal. The version bump
    /// forces a one-time automatic rebuild on the next query via the existing
    /// FORMAT_VERSION-mismatch staleness path = correctness-on-upgrade. Behavior
    /// of THIS ticket per ADR-004 (not a #NEW placeholder).
    pub const FORMAT_VERSION: u32 = 3;

    // -----------------------------------------------------------------------
    // Constructors
    // -----------------------------------------------------------------------

    /// Create a new, empty manifest (no file I/O).
    pub(super) fn new(project_root: PathBuf, cache_dir: PathBuf) -> Self {
        Self {
            project_root,
            cache_dir,
            entries: BTreeMap::new(),
            git_head: None,
        }
    }

    /// Load the manifest from `{cache_dir}/index.skfiles`.
    ///
    /// Returns an empty manifest on any of these conditions:
    /// - The file does not exist.
    /// - The header is missing, corrupt, or has a wrong format version.
    /// - The header's `root` does not match the canonical `project_root`.
    /// - Any I/O error reading the file.
    ///
    /// Per-entry parse errors are silently skipped (best-effort recovery).
    ///
    /// # Errors
    ///
    /// Only returns `Err` for unexpected I/O errors that aren't "file not found".
    pub(super) fn load(project_root: PathBuf, cache_dir: PathBuf) -> anyhow::Result<Self> {
        let manifest_path = cache_dir.join(Self::MANIFEST_FILENAME);

        let file = match std::fs::File::open(&manifest_path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Cold start — no manifest yet.
                return Ok(Self::new(project_root, cache_dir));
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("failed to open manifest: {}", manifest_path.display())
                });
            }
        };

        // Guard: reject suspiciously large manifest files before allocating
        // line buffers. A single line without a newline would allocate the
        // entire file content into one String — cap to MAX_MANIFEST_FILE_BYTES.
        if file
            .metadata()
            .is_ok_and(|m| m.len() > MAX_MANIFEST_FILE_BYTES)
        {
            return Ok(Self::new(project_root, cache_dir));
        }

        let reader = BufReader::new(file);
        let mut lines = reader.lines();

        // --- Parse header line ---
        let header_line = match lines.next() {
            Some(Ok(line)) => line,
            _ => {
                // Empty or unreadable file — treat as cold start.
                return Ok(Self::new(project_root, cache_dir));
            }
        };

        let header: ManifestHeader = match serde_json::from_str(&header_line) {
            Ok(h) => h,
            Err(_) => return Ok(Self::new(project_root, cache_dir)),
        };

        // Version check
        if header.version != Self::FORMAT_VERSION {
            return Ok(Self::new(project_root, cache_dir));
        }

        // Root mismatch check — compare canonical strings
        let canonical_root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.clone());
        let header_root = PathBuf::from(&header.root);
        if canonical_root != header_root {
            return Ok(Self::new(project_root, cache_dir));
        }

        // --- Parse entry lines ---
        let mut entries = BTreeMap::new();
        for line_result in lines {
            // Hard cap: stop reading if the manifest is unreasonably large.
            // Protects against corrupted files with millions of valid entries.
            if entries.len() >= MAX_MANIFEST_ENTRIES {
                break;
            }

            let line = match line_result {
                Ok(l) if l.trim().is_empty() => continue,
                Ok(l) => l,
                Err(_) => continue,
            };
            if let Ok(entry) = serde_json::from_str::<ManifestEntry>(&line) {
                let key = entry.path.clone();
                entries.insert(key, entry);
            }
            // Silently skip unparseable lines (partial-write recovery).
        }

        Ok(Self {
            project_root,
            cache_dir,
            entries,
            git_head: header.git_head,
        })
    }

    // -----------------------------------------------------------------------
    // Mutation
    // -----------------------------------------------------------------------

    /// Insert or replace an entry (keyed by `entry.path`).
    pub(super) fn insert(&mut self, entry: ManifestEntry) {
        let key = entry.path.clone();
        self.entries.insert(key, entry);
    }

    // -----------------------------------------------------------------------
    // Query
    // -----------------------------------------------------------------------

    /// Look up a manifest entry by its repo-relative path.
    ///
    /// Returns `None` if the file has not been indexed before.
    pub(super) fn lookup(&self, path: &str) -> Option<&ManifestEntry> {
        self.entries.get(path)
    }

    /// Return entry paths sorted in byte-wise string order.
    ///
    /// # Invariant
    ///
    /// The index build pipeline sorts walk entries by `walk::normalize_rel_path`
    /// (byte-wise `str` comparison of the normalized rel-path) and assigns
    /// `FileId`s sequentially (0, 1, 2, …) in the consumer loop.  Because
    /// `normalize_rel_path` produces exactly the key string stored in this
    /// `BTreeMap<String>`, `BTreeMap` iteration order is byte-identical to the
    /// walk's FileId-assignment order — so `sorted_paths()[n]` is the path for
    /// `FileId(n)` by construction.
    ///
    /// AD-373-1: FileId assignment uses the same byte-wise normalized-String
    /// order as this `BTreeMap<String>` resolution side.  `PathBuf::cmp` is
    /// component-aware and diverges from `str::cmp` on nested dirs (`foo/bar.rs`
    /// vs `foo.rs`), causing `FileId`→path mis-resolution (#373) — fixed by
    /// sorting the walk with `normalize_rel_path` instead.
    ///
    /// Because `entries` is a [`BTreeMap`], keys are always in byte-wise string
    /// order — iteration is O(n) with no additional allocation or sort.
    pub(super) fn sorted_paths(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }

    /// Return the total number of indexed entries.
    ///
    /// Used in tests and future callers that need the count without loading all paths.
    #[allow(dead_code)]
    pub(super) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Return the git HEAD that was recorded when the index was last built.
    ///
    /// `None` when the manifest was written by an older skim version that did
    /// not store git HEAD, or when no HEAD was available at build time (e.g.
    /// a non-git project).
    pub(super) fn stored_git_head(&self) -> Option<&str> {
        self.git_head.as_deref()
    }

    /// Set the git HEAD to record in the next [`Self::save`] call.
    pub(super) fn set_git_head(&mut self, head: Option<String>) {
        self.git_head = head;
    }

    // -----------------------------------------------------------------------
    // Persistence
    // -----------------------------------------------------------------------

    /// Atomically write all entries to `{cache_dir}/index.skfiles`.
    ///
    /// Uses a named temp file in `cache_dir` and renames it into place so
    /// readers never observe a partial write.
    ///
    /// # Errors
    ///
    /// Returns `Err` on I/O failures during the write or rename.
    pub(super) fn save(&self) -> anyhow::Result<()> {
        let canonical_root = self
            .project_root
            .canonicalize()
            .unwrap_or_else(|_| self.project_root.clone());

        let header = ManifestHeader {
            version: Self::FORMAT_VERSION,
            root: canonical_root.to_string_lossy().into_owned(),
            git_head: self.git_head.clone(),
        };

        let tmp = NamedTempFile::new_in(&self.cache_dir)?;
        let mut buf = BufWriter::new(tmp);

        // Write header
        let header_json = serde_json::to_string(&header)?;
        writeln!(buf, "{header_json}")?;

        // Write entries in sorted order (BTreeMap guarantees alphabetical iteration).
        for entry in self.entries.values() {
            let entry_json = serde_json::to_string(entry)?;
            writeln!(buf, "{entry_json}")?;
        }

        // Flush the buffer before persisting so all bytes reach the temp file.
        buf.flush()?;
        let tmp = buf
            .into_inner()
            .context("failed to flush manifest buffer")?;

        let manifest_path = self.cache_dir.join(Self::MANIFEST_FILENAME);
        tmp.persist(&manifest_path)
            .map_err(|e| anyhow::anyhow!("failed to persist manifest: {}", e.error))?;

        Ok(())
    }
}

// ============================================================================
// Helpers re-exported for use in tests
// ============================================================================

/// Encode a field_map slice into the compact `(start, end, discriminant)` format.
pub(super) fn encode_field_map(
    field_map: &[(std::ops::Range<usize>, rskim_search::SearchField)],
) -> Vec<(usize, usize, u8)> {
    field_map
        .iter()
        .map(|(r, f)| (r.start, r.end, f.discriminant()))
        .collect()
}

/// Decode a compact field_map back to `(Range<usize>, SearchField)`.
///
/// Unknown discriminants are silently filtered out.
pub(super) fn decode_field_map(
    encoded: &[(usize, usize, u8)],
) -> Vec<(std::ops::Range<usize>, rskim_search::SearchField)> {
    encoded
        .iter()
        .filter_map(|(start, end, disc)| {
            rskim_search::SearchField::from_discriminant(*disc).map(|f| (*start..*end, f))
        })
        .collect()
}

// ============================================================================
// Tests (co-located in manifest_tests.rs)
// ============================================================================

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
