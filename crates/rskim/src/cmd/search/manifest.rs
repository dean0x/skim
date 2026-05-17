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

use std::collections::HashMap;
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
}

// ============================================================================
// Manifest store
// ============================================================================

/// In-memory manifest loaded from or written to `index.skfiles`.
pub(super) struct FileManifest {
    /// Project root this manifest was built for.
    project_root: PathBuf,
    /// Directory where the `index.skfiles` sidecar lives.
    cache_dir: PathBuf,
    /// Entries keyed by `ManifestEntry::path`.
    entries: HashMap<String, ManifestEntry>,
}

impl FileManifest {
    /// Manifest filename inside the cache directory.
    pub const MANIFEST_FILENAME: &'static str = "index.skfiles";

    /// Current format version — bump this on any breaking schema change.
    pub const FORMAT_VERSION: u32 = 1;

    // -----------------------------------------------------------------------
    // Constructors
    // -----------------------------------------------------------------------

    /// Create a new, empty manifest (no file I/O).
    pub(super) fn new(project_root: PathBuf, cache_dir: PathBuf) -> Self {
        Self {
            project_root,
            cache_dir,
            entries: HashMap::new(),
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
        let mut entries = HashMap::with_capacity(1024);
        for line_result in lines {
            let line = match line_result {
                Ok(l) if l.trim().is_empty() => continue,
                Ok(l) => l,
                Err(_) => continue,
            };
            if let Ok(entry) = serde_json::from_str::<ManifestEntry>(&line) {
                entries.insert(entry.path.clone(), entry);
            }
            // Silently skip unparseable lines (partial-write recovery).
        }

        Ok(Self {
            project_root,
            cache_dir,
            entries,
        })
    }

    // -----------------------------------------------------------------------
    // Mutation
    // -----------------------------------------------------------------------

    /// Insert or replace an entry (keyed by `entry.path`).
    pub(super) fn insert(&mut self, entry: ManifestEntry) {
        self.entries.insert(entry.path.clone(), entry);
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
        };

        let tmp = NamedTempFile::new_in(&self.cache_dir)?;
        let mut buf = BufWriter::new(tmp);

        // Write header
        let header_json = serde_json::to_string(&header)?;
        writeln!(buf, "{header_json}")?;

        // Write entries (sorted for deterministic output)
        let mut paths: Vec<&str> = self.entries.keys().map(String::as_str).collect();
        paths.sort_unstable();
        for path in paths {
            let entry_json = serde_json::to_string(&self.entries[path])?;
            writeln!(buf, "{entry_json}")?;
        }

        // Flush the buffer before persisting so all bytes reach the temp file.
        buf.flush()?;
        let tmp = buf.into_inner().context("failed to flush manifest buffer")?;

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
