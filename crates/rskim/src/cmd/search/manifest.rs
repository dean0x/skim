//! Binary manifest sidecar for the index builder.
//!
//! The manifest (`index.skfiles`) records, for each indexed file:
//! - its repo-relative path
//! - its content SHA-256
//! - the detected language
//! - the pre-computed field_map (encoded as `(start, end, field_discriminant)` triples)
//! - the file mtime and size at build time (working-tree staleness hints)
//!
//! # Format (AD-380-1, #380)
//!
//! As of FORMAT_VERSION 4 the manifest is a compact **binary** file with a
//! self-describing header, mirroring `ast_index.skcache`
//! (`crates/rskim-search/src/ast_index/ast_cache.rs`). The previous v2/v3 format
//! was a JSONL stream; binarizing removes the dominant on-disk cost of the index
//! sidecar (a ~13 MB JSONL field-map on large repos) and the 1-byte-per-posting
//! ASCII overhead (#380 / #174 re-baseline).
//!
//! File layout (all integers little-endian):
//!
//! ```text
//!   4 bytes : magic  ("SKFM")
//!   4 bytes : version (u32)            ← MANIFEST_FORMAT_VERSION
//!   4 bytes : entry_count (u32)
//!   header block:
//!     4 bytes : root_len (u32)
//!     root_len bytes : canonical root (UTF-8)
//!     1 byte  : git_head_present (0 = None, 1 = Some)
//!     [4 bytes : git_head_len (u32) + git_head_len bytes (UTF-8)]   (when present)
//!   entry_count × entry:
//!     4 bytes : path_len (u32) + path bytes
//!     4 bytes : sha_len (u32)  + sha bytes
//!     4 bytes : lang_len (u32) + lang bytes
//!     4 bytes : field_map_count (u32)
//!     field_map_count × (u32 start, u32 end, u8 discriminant)
//!     1 byte  : mtime_present (0/1) [+ 8 bytes u64 when present]
//!     1 byte  : size_present  (0/1) [+ 8 bytes u64 when present]
//! ```
//!
//! An empty or missing file is treated as a cold-start (no cache hits).
//!
//! # Atomicity (AD-380-8, ADR-006)
//!
//! Writes use a named temp file in the same directory, persisted (renamed) after
//! the full write succeeds. Readers never observe a partial write. The build
//! pipeline persists the lexical/AST indexes and `ast_index.skcache` BEFORE
//! `FileManifest::save`, so the manifest is always written LAST — a crash leaves
//! the prior (consistent) manifest in place and the next query self-heals.
//!
//! # Reject-whole on corruption (AD-380-3, AC-5)
//!
//! `load()` returns `Ok(empty)` on ANY structural problem (absent magic, wrong
//! version, declared count over the cap, file over the size cap, or a truncated
//! body). It NEVER returns a partially-recovered manifest shorter than what was
//! written, because the FileId↔path alignment invariant
//! (`sorted_paths()[n] == path-for-FileId(n)`) requires the manifest entry set to
//! be exactly the set the index was built against. A short manifest would silently
//! mis-resolve FileIds. Rejecting the whole file forces a clean rebuild instead.
//!
//! # Wrong-root detection
//!
//! The header embeds the canonical project root. If the header root does not
//! match the `project_root` passed to `FileManifest::load`, the entire manifest
//! is discarded (returns an empty manifest).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use tempfile::NamedTempFile;

// ============================================================================
// On-disk types
// ============================================================================

/// One indexed-file record. Held in-memory and serialized into the binary body.
///
/// The struct intentionally keeps the same shape it had under the JSONL format
/// so the builder (`index.rs`) and the working-tree scan (`staleness.rs`) need no
/// changes — only the on-disk encoding changed in #380.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// `None` (encoded as a 0 presence byte) keeps backward-compatible semantics:
    /// a missing hint forces a stale verdict so the field is repopulated on rebuild.
    pub mtime: Option<u64>,
    /// File size in bytes when the manifest was written.
    ///
    /// AD-379-2: working-tree staleness compares BOTH mtime AND size against the
    /// current on-disk file. mtime is second-resolution on many filesystems, so a
    /// same-second edit can leave mtime unchanged; size is the second freshness
    /// hint that closes that gap (an edit that changes the byte length is detected
    /// even when mtime is preserved). A same-size, same-second swap remains
    /// deliberately undetectable without SHA — off the hot path by design (AC9).
    ///
    /// `None` (encoded as a 0 presence byte) forces a stale verdict on the next
    /// working-tree scan so the field is repopulated by the one-time rebuild (AC10).
    pub size: Option<u64>,
}

// ============================================================================
// Format constants
// ============================================================================

/// Magic bytes at the start of every binary `index.skfiles` file (AD-380-1).
///
/// "SKFM" = SKim File Manifest. Mirrors the `SKAC` magic used by
/// `ast_index.skcache`. `load()` returns `Ok(empty)` when these bytes are absent
/// (e.g. an old JSONL manifest), which drives the v3→v4 self-heal rebuild.
const MANIFEST_MAGIC: &[u8; 4] = b"SKFM";

/// Header byte length: 4 (magic) + 4 (version) + 4 (entry_count).
const FILE_HEADER_BYTES: usize = 4 + 4 + 4;

// ============================================================================
// Safety limits
// ============================================================================

/// Maximum number of entries accepted from a manifest file.
///
/// Guards against unbounded memory growth from a corrupted manifest declaring
/// millions of entries. 60 000 files far exceeds any realistic monorepo. The
/// decoder rejects BEFORE allocating when the declared count exceeds this cap
/// (AD-380-3 / AC-3).
const MAX_MANIFEST_ENTRIES: usize = 60_000;

/// Maximum manifest file size accepted before reading into memory.
///
/// A forged length prefix could otherwise request a multi-gigabyte allocation.
/// Reject oversized files up front rather than discovering OOM mid-parse.
/// 256 MiB is several orders of magnitude larger than any realistic manifest
/// and matches `MAX_CACHE_FILE_BYTES` in `ast_cache.rs` (AD-380-3 / AC-3).
const MAX_MANIFEST_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// Maximum byte length accepted for any single length-prefixed field
/// (path / sha / lang) or the root string.
///
/// A realistic path or SHA is well under 1 KiB; 64 KiB is a generous bound that
/// rejects a forged `u32::MAX` length prefix BEFORE slicing (AD-380-3 / AC-3).
const MAX_FIELD_BYTES: usize = 64 * 1024;

/// Maximum number of field_map triples accepted in a single decoded entry.
///
/// Each triple is 9 bytes on disk; a single huge file rarely has more than a few
/// thousand field spans. 1M caps a forged `field_map_count` before the per-entry
/// `Vec::with_capacity` (AD-380-3 / AC-3). Distinct concept from the file-level
/// entry cap (`MAX_MANIFEST_ENTRIES`).
const MAX_FIELD_MAP_TRIPLES: usize = 1_000_000;

// ============================================================================
// Binary codec
// ============================================================================

/// Append a length-prefixed (`u32` saturating) byte slice to `buf` (AD-380-3).
fn write_lp_bytes(buf: &mut Vec<u8>, bytes: &[u8]) {
    // AD-380-3: saturating `try_from`, never `as u32` — an oversized length is
    // clamped to u32::MAX (the decoder then rejects it via MAX_FIELD_BYTES).
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    buf.extend_from_slice(&len.to_le_bytes());
    buf.extend_from_slice(bytes);
}

/// Append an `Option<u64>` as a presence byte followed by the value (when Some).
fn write_opt_u64(buf: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(v) => {
            buf.push(1u8);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        None => buf.push(0u8),
    }
}

/// Encode a single entry into `buf`.
fn encode_entry(buf: &mut Vec<u8>, entry: &ManifestEntry) {
    write_lp_bytes(buf, entry.path.as_bytes());
    write_lp_bytes(buf, entry.sha256.as_bytes());
    write_lp_bytes(buf, entry.lang.as_bytes());

    // field_map: count (saturating u32) + triples.
    // AD-380-3: try_from with saturating fallback, never `as u32`.
    let triple_count = u32::try_from(entry.field_map.len()).unwrap_or(u32::MAX);
    buf.extend_from_slice(&triple_count.to_le_bytes());
    for (start, end, disc) in &entry.field_map {
        buf.extend_from_slice(&u32::try_from(*start).unwrap_or(u32::MAX).to_le_bytes());
        buf.extend_from_slice(&u32::try_from(*end).unwrap_or(u32::MAX).to_le_bytes());
        buf.push(*disc);
    }

    write_opt_u64(buf, entry.mtime);
    write_opt_u64(buf, entry.size);
}

/// Cursor-based reader over the manifest body. Every read is bounds-checked and
/// returns `None` on truncation, which the caller propagates as "reject whole"
/// (AD-380-3 / AC-5).
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn read_u8(&mut self) -> Option<u8> {
        let b = *self.buf.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn read_u32(&mut self) -> Option<u32> {
        let bytes = self.buf.get(self.pos..self.pos + 4)?;
        self.pos += 4;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }

    fn read_u64(&mut self) -> Option<u64> {
        let bytes = self.buf.get(self.pos..self.pos + 8)?;
        self.pos += 8;
        Some(u64::from_le_bytes(bytes.try_into().ok()?))
    }

    /// Read a length-prefixed UTF-8 string, rejecting BEFORE slicing when the
    /// declared length exceeds `MAX_FIELD_BYTES` (AD-380-3 / AC-3).
    fn read_lp_string(&mut self) -> Option<String> {
        let len = self.read_u32()? as usize;
        if len > MAX_FIELD_BYTES {
            return None;
        }
        let bytes = self.buf.get(self.pos..self.pos + len)?;
        self.pos += len;
        // Lossless: manifest strings are always valid UTF-8 when written; a bad
        // slice means corruption → reject whole.
        std::str::from_utf8(bytes).ok().map(str::to_owned)
    }

    fn read_opt_u64(&mut self) -> Option<Option<u64>> {
        match self.read_u8()? {
            0 => Some(None),
            1 => Some(Some(self.read_u64()?)),
            // Any other presence byte → corruption.
            _ => None,
        }
    }
}

/// Decode one entry from the cursor. Returns `None` on any truncation or bound
/// violation (the caller rejects the whole file).
fn decode_entry(cur: &mut Cursor<'_>) -> Option<ManifestEntry> {
    let path = cur.read_lp_string()?;
    let sha256 = cur.read_lp_string()?;
    let lang = cur.read_lp_string()?;

    let triple_count = cur.read_u32()? as usize;
    // AD-380-3 / AC-3: reject a forged field_map_count BEFORE the with_capacity.
    if triple_count > MAX_FIELD_MAP_TRIPLES {
        return None;
    }
    let mut field_map = Vec::with_capacity(triple_count);
    for _ in 0..triple_count {
        let start = cur.read_u32()? as usize;
        let end = cur.read_u32()? as usize;
        let disc = cur.read_u8()?;
        field_map.push((start, end, disc));
    }

    let mtime = cur.read_opt_u64()?;
    let size = cur.read_opt_u64()?;

    Some(ManifestEntry {
        path,
        sha256,
        lang,
        field_map,
        mtime,
        size,
    })
}

/// Parsed file-level header (magic + version + entry_count already validated).
struct DecodedHeader {
    root: String,
    git_head: Option<String>,
    entry_count: usize,
    /// Byte offset of the first entry (just past the header block).
    body_offset: usize,
}

/// Validate the fixed file header (magic + version + count) and parse the
/// variable header block (root + git_head). Returns `None` on any mismatch
/// (absent magic, wrong version, count over cap, truncated header) so callers
/// cold-start (AD-380-2 / AC-2).
fn decode_header(buf: &[u8]) -> Option<DecodedHeader> {
    if buf.len() < FILE_HEADER_BYTES {
        return None;
    }
    if &buf[0..4] != MANIFEST_MAGIC {
        // Absent magic → old JSONL manifest or unrelated bytes → cold start.
        return None;
    }
    let version = u32::from_le_bytes(buf[4..8].try_into().ok()?);
    if version != FileManifest::FORMAT_VERSION {
        // Below-current (or future) version → cold start / rebuild (AC-2, AC-4).
        return None;
    }
    let entry_count = u32::from_le_bytes(buf[8..12].try_into().ok()?) as usize;
    // AD-380-3 / AC-3: reject a forged entry_count BEFORE allocating the map.
    if entry_count > MAX_MANIFEST_ENTRIES {
        return None;
    }

    // Variable header block: root string + optional git_head.
    let mut cur = Cursor {
        buf,
        pos: FILE_HEADER_BYTES,
    };
    let root = cur.read_lp_string()?;
    let git_head = match cur.read_u8()? {
        0 => None,
        1 => Some(cur.read_lp_string()?),
        _ => return None,
    };

    Some(DecodedHeader {
        root,
        git_head,
        entry_count,
        body_offset: cur.pos,
    })
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
    /// Entries keyed by `ManifestEntry::path`, stored in sorted order via
    /// [`BTreeMap`] so that [`Self::sorted_paths`] and [`Self::save`] never
    /// need to sort the keys — iteration order is byte-wise string order by
    /// construction (the FileId↔path ordering contract; see [`Self::sorted_paths`]).
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
    /// layout was unchanged, but the walk-side FileId assignment order now uses
    /// byte-wise comparison of `normalize_rel_path` instead of `PathBuf::cmp`.
    /// A pre-existing v2 manifest could have been built with the old, skewed
    /// ordering → it would silently serve wrong files even if otherwise fresh.
    /// Bumping to v3 makes the FORMAT_VERSION-mismatch staleness path detect every
    /// v2 manifest as stale and rebuild it once on the next query (AD-373-3).
    ///
    /// v3 → v4: Binarize the sidecar (#380, AD-380-2). The encoding changed from
    /// a JSONL stream to the compact binary format documented in the module
    /// header (SKFM magic). Both the immediate predecessor (v3 JSONL) and older
    /// v2 JSONL manifests lack the binary magic, so `version_matches` reports a
    /// mismatch and `check_staleness` rebuilds once on the next query —
    /// correctness-on-upgrade with no manual `--rebuild`, for BOTH git and
    /// non-git roots (AC-4). The bump is monotonic 2→3→4 (ADR-006); #373 owns
    /// 2→3 and #380 owns 3→4. Behavior of THIS ticket per ADR-004 (not a #NEW
    /// placeholder).
    pub const FORMAT_VERSION: u32 = 4;

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
    /// Returns an empty manifest on any of these conditions (AD-380-3 / AC-5 —
    /// reject WHOLE, never partial):
    /// - The file does not exist.
    /// - The file exceeds `MAX_MANIFEST_FILE_BYTES`.
    /// - The magic is absent (e.g. an old JSONL manifest), version != 4, or the
    ///   declared entry count exceeds `MAX_MANIFEST_ENTRIES`.
    /// - The header's `root` does not match the canonical `project_root`.
    /// - The body is truncated or any entry is structurally corrupt.
    ///
    /// # Errors
    ///
    /// Only returns `Err` for unexpected I/O errors that aren't "file not found".
    pub(super) fn load(project_root: PathBuf, cache_dir: PathBuf) -> anyhow::Result<Self> {
        let manifest_path = cache_dir.join(Self::MANIFEST_FILENAME);

        // Guard: reject suspiciously large manifest files BEFORE reading into
        // memory. An unbounded `fs::read` would otherwise materialise the whole
        // file in RAM (AD-380-3 / AC-3).
        match std::fs::metadata(&manifest_path) {
            Ok(m) if m.len() > MAX_MANIFEST_FILE_BYTES => {
                return Ok(Self::new(project_root, cache_dir));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Cold start — no manifest yet.
                return Ok(Self::new(project_root, cache_dir));
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("failed to stat manifest: {}", manifest_path.display())
                });
            }
        }

        let buf = match std::fs::read(&manifest_path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::new(project_root, cache_dir));
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("failed to read manifest: {}", manifest_path.display())
                });
            }
        };

        // Decode the header; absent magic / wrong version / over-cap count all
        // cold-start (AC-2, AC-4).
        let Some(header) = decode_header(&buf) else {
            return Ok(Self::new(project_root, cache_dir));
        };

        // Root mismatch check — compare canonical paths (no allocation: borrow
        // the header root as a `Path` rather than building a `PathBuf`).
        let canonical_root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.clone());
        if canonical_root.as_path() != Path::new(&header.root) {
            return Ok(Self::new(project_root, cache_dir));
        }

        // Decode exactly `entry_count` entries. ANY truncation / corruption →
        // reject WHOLE so a short, FileId-misaligned manifest is never returned
        // (AD-380-3 / AC-5).
        let mut cur = Cursor::new(&buf);
        cur.pos = header.body_offset;
        let mut entries = BTreeMap::new();
        for _ in 0..header.entry_count {
            match decode_entry(&mut cur) {
                Some(entry) => {
                    entries.insert(entry.path.clone(), entry);
                }
                None => {
                    // Truncated / corrupt body — discard everything decoded so
                    // far and cold-start (reject whole, AC-5).
                    return Ok(Self::new(project_root, cache_dir));
                }
            }
        }

        Ok(Self {
            project_root,
            cache_dir,
            entries,
            git_head: header.git_head,
        })
    }

    /// Check if an on-disk manifest file exists and has the current FORMAT_VERSION.
    ///
    /// Returns:
    /// - `Ok(true)` if the manifest exists and version matches (current binary v4).
    /// - `Ok(false)` if the manifest exists but is stale (old JSONL v2/v3 — no
    ///   binary magic, or a different version int).
    /// - `Ok(true)` if no manifest file exists (cold start; the `NoIndex` path
    ///   handles the missing-index case separately).
    ///
    /// This is the AD-380-2 / AC-4 self-heal hook used by `check_staleness`. It
    /// reads only the fixed 12-byte header (magic + version + count) — no body
    /// parse, no whole-file read. It is independent of git HEAD state, so the
    /// v3→v4 rebuild fires for BOTH git AND non-git roots (AC-4).
    pub(super) fn version_matches(cache_dir: &Path) -> anyhow::Result<bool> {
        let manifest_path = cache_dir.join(Self::MANIFEST_FILENAME);

        let header = match read_fixed_header(&manifest_path) {
            Ok(Some(h)) => h,
            // No manifest file — treat as matching (cold start; NoIndex handles
            // the missing-lexical-index case).
            Ok(None) => return Ok(true),
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("failed to open manifest: {}", manifest_path.display())
                });
            }
        };

        // A v2/v3 JSONL manifest starts with `{` (ASCII 0x7B), never the SKFM
        // magic, so `magic_ok` is false → below-current → rebuild (AC-4).
        let magic_ok = &header[0..4] == MANIFEST_MAGIC;
        let version = u32::from_le_bytes(header[4..8].try_into().unwrap_or([0; 4]));
        Ok(magic_ok && version == Self::FORMAT_VERSION)
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

    /// Iterate `(path, mtime, size)` freshness tuples for every indexed file.
    ///
    /// Used by the working-tree staleness scan (AD-379-2) to compare the
    /// recorded mtime AND size of each indexed file against the current
    /// on-disk metadata. Iteration is in byte-wise key order (BTreeMap), so the
    /// caller observes paths in the same order as [`Self::sorted_paths`].
    pub(super) fn freshness_entries(
        &self,
    ) -> impl Iterator<Item = (&str, Option<u64>, Option<u64>)> {
        self.entries
            .values()
            .map(|e| (e.path.as_str(), e.mtime, e.size))
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
    /// AD-380-3 (#380): the binary `load()` rejects the WHOLE file on any
    /// truncation rather than returning a short manifest, so this invariant holds
    /// across the JSONL→binary migration — a corrupt binary manifest can never
    /// yield `sorted_paths()` shorter than the set the index was built against.
    pub(super) fn sorted_paths(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }

    /// Return the total number of indexed entries.
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

    /// Serialize the whole manifest to the binary on-disk format (AD-380-1).
    ///
    /// Pure function of the in-memory state — factored out of [`Self::save`] so
    /// it is unit-testable without touching the filesystem. Entries are emitted
    /// in `BTreeMap` (byte-wise key) order, preserving the FileId↔path contract.
    fn encode(&self) -> Vec<u8> {
        let canonical_root = self
            .project_root
            .canonicalize()
            .unwrap_or_else(|_| self.project_root.clone());
        let root_str = canonical_root.to_string_lossy();

        // Pre-size: fixed header + header block + a rough per-entry estimate.
        let mut buf = Vec::with_capacity(FILE_HEADER_BYTES + 128 + self.entries.len() * 160);

        buf.extend_from_slice(MANIFEST_MAGIC);
        buf.extend_from_slice(&Self::FORMAT_VERSION.to_le_bytes());
        // AD-380-3: saturating entry count.
        let entry_count = u32::try_from(self.entries.len()).unwrap_or(u32::MAX);
        buf.extend_from_slice(&entry_count.to_le_bytes());

        // Variable header block: root + optional git_head.
        write_lp_bytes(&mut buf, root_str.as_bytes());
        write_opt_str(&mut buf, self.git_head.as_deref());

        for entry in self.entries.values() {
            encode_entry(&mut buf, entry);
        }

        buf
    }

    /// Atomically write all entries to `{cache_dir}/index.skfiles`.
    ///
    /// Uses a named temp file in `cache_dir` and renames it into place so
    /// readers never observe a partial write (AD-380-8, ADR-006).
    ///
    /// # Errors
    ///
    /// Returns `Err` on I/O failures during the write or rename.
    pub(super) fn save(&self) -> anyhow::Result<()> {
        let buf = self.encode();

        let mut tmp = NamedTempFile::new_in(&self.cache_dir)?;
        {
            use std::io::Write as _;
            tmp.write_all(&buf)?;
            tmp.as_file().sync_all()?;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // Owner-only: the manifest embeds repo paths and structure.
            std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600))?;
        }

        let manifest_path = self.cache_dir.join(Self::MANIFEST_FILENAME);
        tmp.persist(&manifest_path)
            .map_err(|e| anyhow::anyhow!("failed to persist manifest: {}", e.error))?;

        Ok(())
    }
}

// ============================================================================
// Free helpers
// ============================================================================

/// Append an `Option<&str>` as a presence byte plus a length-prefixed value.
fn write_opt_str(buf: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(s) => {
            buf.push(1u8);
            write_lp_bytes(buf, s.as_bytes());
        }
        None => buf.push(0u8),
    }
}

/// Read the fixed 12-byte file header (magic + version + entry_count) without
/// reading the body. Returns `Ok(None)` when the file does not exist or is
/// shorter than the fixed header (a stale/truncated file the caller will rebuild).
fn read_fixed_header(path: &Path) -> std::io::Result<Option<[u8; FILE_HEADER_BYTES]>> {
    use std::io::Read as _;

    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut header = [0u8; FILE_HEADER_BYTES];
    match file.read_exact(&mut header) {
        Ok(()) => Ok(Some(header)),
        // Too short to hold a header — treat as absent (will be overwritten).
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e),
    }
}

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
