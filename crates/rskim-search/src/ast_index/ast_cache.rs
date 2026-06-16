//! Incremental AST n-gram cache (`ast_index.skcache`).
//!
//! Persists per-file `(AstNgramSet, StructuralMetrics, node_count)` triples
//! keyed by content SHA-256 so that unchanged files skip re-extraction on
//! incremental `skim search index --update` runs.
//!
//! # Design
//!
//! - **Key:** content SHA-256 (hex, 64 chars) — matches the sole cache
//!   authority used by the lexical `index.skfiles` manifest.  No mtime,
//!   no path — purely content-addressed.
//! - **Format:** compact binary (`ast_index.skcache`).
//!   Header: 4-byte magic + 1-byte version + 4-byte entry count.
//!   Each entry: 64-byte SHA key, 4-byte payload length, then payload bytes.
//!   Payload: little-endian packed `(AstNgramSet, StructuralMetrics, u32)`.
//! - **Version:** `CACHE_FORMAT_VERSION = 1`.  A version mismatch discards the
//!   entire cache and cold-starts extraction.  Any change to the extraction
//!   algorithm or AST weight tables MUST bump this constant so stale weights
//!   are never reused from cache.  (applies ADR-003)
//! - **Self-pruning:** the cache is rebuilt from scratch each build — only the
//!   current build's files are inserted, then written atomically.  Deleted or
//!   renamed files naturally age out.
//! - **ADR-006 safety:** the cache is written AFTER `ast_builder.build()` and
//!   BEFORE `new_manifest.save()`.  A skcache write failure must propagate as
//!   `Err` so the manifest is never saved (self-heal on next query).
//! - **Corrupt/truncated entry → cache miss:** a bad entry causes only that
//!   file to be re-extracted; the build continues normally.
//!
//! # Crate-boundary design
//!
//! The module lives in **rskim-search** alongside the types it serialises
//! (`AstBigram`, `AstTrigram`, `StructuralMetrics`).  The rskim bin-crate's
//! `index.rs` calls the public API (`load` / `lookup` / `insert` / `save`).
//!
//! # SHA collision
//!
//! SHA-256 collision is not a practical threat and the lexical cache already
//! trusts SHA-256 as sole authority (index.rs comment, line 18-21).  This is
//! an accepted risk mirroring the existing design. (applies ADR-003)
//!
//! # mtime granularity
//!
//! Correctly a non-issue: this cache keys on content SHA, never on mtime.
//! mtime is stored in `ManifestEntry` as a forward-looking hint only and is
//! not consulted for any cache decision here. (applies ADR-003)

use std::collections::HashMap;
use std::path::Path;

use crate::Result;
use crate::ast_index::extract::{AstBigramEntry, AstNgramSet, AstTrigramEntry};
use crate::ast_index::structural::StructuralMetrics;
use crate::io_util::atomic_write;

// ============================================================================
// Format constants
// ============================================================================

/// Magic bytes at the start of every `ast_index.skcache` file.
const CACHE_MAGIC: &[u8; 4] = b"SKAC";

/// Current on-disk format version.
///
/// **Bump this constant** whenever ANY of the following change:
/// - `crates/rskim-search/src/ast_index/ast_weights.rs` (auto-generated IDF
///   weight tables) — stale IDF weights produce wrong n-gram scores in the index.
/// - `extract_ast_ngrams_with_metrics` in `extract.rs` — changes to the
///   extraction algorithm would make cached n-grams diverge from fresh results.
/// - The binary layout of `CachedAstEntry` itself.
///
/// A version mismatch at load time discards the entire cache cleanly so the
/// first incremental build after any such change re-extracts everything. (applies ADR-003)
pub const CACHE_FORMAT_VERSION: u8 = 1;

/// Sidecar filename inside the cache directory.
pub const CACHE_FILENAME: &str = "ast_index.skcache";

/// SHA-256 hex string length (64 lowercase ASCII chars).
const SHA_HEX_LEN: usize = 64;

/// Maximum number of file entries the cache will accept on load.
///
/// Guards against allocation bombs from a corrupted file claiming millions of
/// entries.  Mirrors `MAX_MANIFEST_ENTRIES` in `manifest.rs`.
const MAX_CACHE_ENTRIES: usize = 60_000;

/// Maximum per-entry payload size in bytes.
///
/// Guards against forged length prefixes that would request a multi-GB
/// allocation.  A realistic entry (hundreds of bigrams + trigrams) is well
/// under a few KB; 1 MiB is a generous upper bound.  (applies ADR-003)
const MAX_ENTRY_BYTES: usize = 1024 * 1024; // 1 MiB

/// Maximum number of bigrams or trigrams in a single decoded cache entry.
///
/// Used in `decode_entry` to bound per-entry n-gram vector pre-allocations —
/// a distinct concept from the whole-file entry cap (`MAX_CACHE_ENTRIES`).
/// Derived from `MAX_ENTRY_BYTES`: a payload saturated with the smallest n-gram
/// entry (bigram = 12 bytes) could hold at most 87,381 entries, but realistic
/// Rust files rarely exceed a few thousand n-grams.  64 KiB-worth is generous.
/// Using a dedicated constant avoids reusing the unrelated file-count cap and
/// matches the one-constant-per-concept discipline. (applies ADR-003)
const MAX_NGRAMS_PER_ENTRY: usize = MAX_ENTRY_BYTES / BIGRAM_ENTRY_BYTES; // ~87 K

/// Maximum total file size for `ast_index.skcache` before reading into memory.
///
/// Mirrors `MAX_MANIFEST_FILE_BYTES` in `manifest.rs`.  A valid skcache
/// at 60,000 files × 1 MiB/entry would be 60 GiB — far beyond any realistic
/// project.  256 MiB is a generous whole-file cap that rejects obviously
/// corrupt or adversarial files without blocking any real build.
/// The per-entry cap (`MAX_ENTRY_BYTES`) and entry-count cap
/// (`MAX_CACHE_ENTRIES`) apply inside the file once it is loaded.  (applies ADR-003)
const MAX_CACHE_FILE_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB

// ============================================================================
// Cached payload
// ============================================================================

/// The triple cached per file: n-grams, structural metrics, and node count.
///
/// `node_count` is `u32` matching `derive_ast_entry`'s return type.
/// `StructuralMetrics.max_depth` is `u16` — arithmetic on it must widen to
/// `u32` before ordering operations. (avoids PF-004)
///
/// `Default` yields an empty entry (all zero/empty fields), mirroring what
/// `derive_ast_entry` returns for non-tree-sitter / large / empty files.
/// Storing and serving empty entries from cache is correct — they avoid
/// re-calling `linearize_source` on data-format files (JSON/YAML/TOML)
/// that are known to produce no n-grams.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CachedAstEntry {
    /// Deduplicated structural n-grams for the file.
    pub ngrams: AstNgramSet,
    /// Per-file structural complexity metrics.
    pub metrics: StructuralMetrics,
    /// Number of CST nodes seen during linearization (0 for non-tree-sitter
    /// languages, empty files, and files >100 KiB).
    pub node_count: u32,
}

// ============================================================================
// Binary codec
// ============================================================================
//
// Layout of a single payload blob (variable length):
//   u32_le: bigram_count
//   u32_le: trigram_count
//   bigram_count × AstBigramEntry:
//     u32_le: ngram key
//     f32_le: weight
//     u32_le: count
//   trigram_count × AstTrigramEntry:
//     u64_le: ngram key
//     f32_le: weight
//     u32_le: count
//   u16_le: max_depth
//   u16_le: max_block_stmts
//   u16_le: max_params
//   u32_le: branch_count
//   u32_le: node_count

const BIGRAM_ENTRY_BYTES: usize = 4 + 4 + 4; // u32 key + f32 weight + u32 count
const TRIGRAM_ENTRY_BYTES: usize = 8 + 4 + 4; // u64 key + f32 weight + u32 count
const METRICS_BYTES: usize = 2 + 2 + 2 + 4; // 3×u16 + u32
const NODE_COUNT_BYTES: usize = 4;

fn encode_entry(entry: &CachedAstEntry) -> Vec<u8> {
    let bigram_count = entry.ngrams.bigrams.len();
    let trigram_count = entry.ngrams.trigrams.len();
    let payload_len = 4
        + 4
        + bigram_count * BIGRAM_ENTRY_BYTES
        + trigram_count * TRIGRAM_ENTRY_BYTES
        + METRICS_BYTES
        + NODE_COUNT_BYTES;

    let mut buf = Vec::with_capacity(payload_len);

    // Applies PF-004: try_from with saturating fallback, not `as u32`.
    buf.extend_from_slice(
        &u32::try_from(bigram_count)
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    buf.extend_from_slice(
        &u32::try_from(trigram_count)
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );

    for b in &entry.ngrams.bigrams {
        buf.extend_from_slice(&b.ngram.key().to_le_bytes());
        buf.extend_from_slice(&b.weight.to_le_bytes());
        buf.extend_from_slice(&b.count.to_le_bytes());
    }

    for t in &entry.ngrams.trigrams {
        buf.extend_from_slice(&t.ngram.key().to_le_bytes());
        buf.extend_from_slice(&t.weight.to_le_bytes());
        buf.extend_from_slice(&t.count.to_le_bytes());
    }

    // StructuralMetrics (avoids PF-004: stored at declared widths)
    buf.extend_from_slice(&entry.metrics.max_depth.to_le_bytes()); // u16
    buf.extend_from_slice(&entry.metrics.max_block_stmts.to_le_bytes()); // u16
    buf.extend_from_slice(&entry.metrics.max_params.to_le_bytes()); // u16
    buf.extend_from_slice(&entry.metrics.branch_count.to_le_bytes()); // u32

    // node_count (u32, avoids PF-004: stored at declared width)
    buf.extend_from_slice(&entry.node_count.to_le_bytes());

    buf
}

/// Decode a payload blob produced by `encode_entry`.
///
/// Returns `None` on any structural error (truncated, count overflow, etc.).
/// A `None` result is treated as a cache miss by the caller — the file is
/// re-extracted and correctness is preserved.
fn decode_entry(buf: &[u8]) -> Option<CachedAstEntry> {
    use crate::ast_index::{AstBigram, AstTrigram};

    let mut pos = 0usize;

    macro_rules! read_u32 {
        () => {{
            let bytes = buf.get(pos..pos + 4)?;
            pos += 4;
            u32::from_le_bytes(bytes.try_into().ok()?)
        }};
    }
    macro_rules! read_u64 {
        () => {{
            let bytes = buf.get(pos..pos + 8)?;
            pos += 8;
            u64::from_le_bytes(bytes.try_into().ok()?)
        }};
    }
    macro_rules! read_f32 {
        () => {{
            let bytes = buf.get(pos..pos + 4)?;
            pos += 4;
            f32::from_le_bytes(bytes.try_into().ok()?)
        }};
    }
    macro_rules! read_u16 {
        () => {{
            let bytes = buf.get(pos..pos + 2)?;
            pos += 2;
            u16::from_le_bytes(bytes.try_into().ok()?)
        }};
    }

    let bigram_count = read_u32!() as usize;
    let trigram_count = read_u32!() as usize;

    // Sanity-check per-entry n-gram counts using the dedicated per-entry cap
    // (not the whole-file entry-count cap) to prevent giant Vec pre-allocations
    // from a single forged entry. (applies ADR-003)
    if bigram_count > MAX_NGRAMS_PER_ENTRY || trigram_count > MAX_NGRAMS_PER_ENTRY {
        return None;
    }

    let mut bigrams = Vec::with_capacity(bigram_count);
    for _ in 0..bigram_count {
        let key = read_u32!();
        let weight = read_f32!();
        let count = read_u32!();
        bigrams.push(AstBigramEntry {
            ngram: AstBigram::from_raw(key),
            weight,
            count,
        });
    }

    let mut trigrams = Vec::with_capacity(trigram_count);
    for _ in 0..trigram_count {
        let key = read_u64!();
        let weight = read_f32!();
        let count = read_u32!();
        trigrams.push(AstTrigramEntry {
            ngram: AstTrigram::from_raw(key),
            weight,
            count,
        });
    }

    // StructuralMetrics (avoids PF-004: read at declared widths)
    let max_depth = read_u16!();
    let max_block_stmts = read_u16!();
    let max_params = read_u16!();
    let branch_count = read_u32!();

    let node_count = read_u32!();

    // Reject if there are trailing bytes — indicates format mismatch.
    if pos != buf.len() {
        return None;
    }

    Some(CachedAstEntry {
        ngrams: AstNgramSet { bigrams, trigrams },
        metrics: StructuralMetrics {
            max_depth,
            max_block_stmts,
            max_params,
            branch_count,
        },
        node_count,
    })
}

// ============================================================================
// File-level codec
// ============================================================================
//
// File layout:
//   4 bytes: CACHE_MAGIC ("SKAC")
//   1 byte:  CACHE_FORMAT_VERSION
//   4 bytes: entry_count (u32_le)
//   entry_count entries, each:
//     64 bytes: SHA-256 hex key (ASCII)
//     4 bytes:  payload_len (u32_le)
//     payload_len bytes: encoded CachedAstEntry

fn encode_file(entries: &HashMap<String, CachedAstEntry>) -> Vec<u8> {
    // Applies PF-004: try_from with saturating fallback, not `as u32`.
    let entry_count = u32::try_from(entries.len()).unwrap_or(u32::MAX);
    // Pre-size for header + rough per-entry estimate.
    let mut buf = Vec::with_capacity(9 + entries.len() * (SHA_HEX_LEN + 4 + 256));

    buf.extend_from_slice(CACHE_MAGIC);
    buf.push(CACHE_FORMAT_VERSION);
    buf.extend_from_slice(&entry_count.to_le_bytes());

    for (sha, entry) in entries {
        debug_assert_eq!(sha.len(), SHA_HEX_LEN, "SHA key must be exactly 64 chars");
        buf.extend_from_slice(sha.as_bytes());
        let payload = encode_entry(entry);
        // Applies PF-004: try_from with saturating fallback, not `as u32`.
        buf.extend_from_slice(
            &u32::try_from(payload.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        buf.extend_from_slice(&payload);
    }

    buf
}

/// Decode the entire cache file, returning the entries map.
///
/// Returns `None` on version mismatch, magic mismatch, or truncated header —
/// callers treat `None` as an empty cache (cold start) with no user-visible error.
/// Individual corrupt entries are skipped (entry → cache miss), not fatal.
fn decode_file(buf: &[u8]) -> Option<HashMap<String, CachedAstEntry>> {
    if buf.len() < 9 {
        return None;
    }
    if &buf[0..4] != CACHE_MAGIC {
        return None;
    }
    if buf[4] != CACHE_FORMAT_VERSION {
        // Version mismatch → discard entire cache, cold-start extraction.
        // Guards ast_weights.rs regeneration and extract.rs algorithm changes.
        return None;
    }
    let entry_count = u32::from_le_bytes(buf[5..9].try_into().ok()?) as usize;
    if entry_count > MAX_CACHE_ENTRIES {
        // Corrupt entry count — reject.
        return None;
    }

    let mut pos = 9usize;
    let mut map = HashMap::with_capacity(entry_count);

    for _ in 0..entry_count {
        // Read the 64-byte SHA key.
        let sha_end = pos + SHA_HEX_LEN;
        let Some(sha_bytes) = buf.get(pos..sha_end) else {
            // Truncated — stop reading but return what we have.
            break;
        };
        let Ok(sha_str) = std::str::from_utf8(sha_bytes) else {
            // Not valid UTF-8 — skip remaining entries (corrupt at this position).
            break;
        };
        let sha = sha_str.to_string();
        pos = sha_end;

        // Read the 4-byte payload length.
        let Some(len_bytes) = buf.get(pos..pos + 4) else {
            break;
        };
        let payload_len = u32::from_le_bytes(len_bytes.try_into().ok()?) as usize;
        pos += 4;

        // Reject oversized payloads — prevents allocation bombs. (applies ADR-003)
        if payload_len > MAX_ENTRY_BYTES {
            // Skip this entry; try to continue parsing subsequent entries.
            // We cannot safely skip `payload_len` bytes because the length itself
            // is suspect — stop parsing rather than risking reading garbage offsets.
            break;
        }

        // Read the payload.
        let Some(payload) = buf.get(pos..pos + payload_len) else {
            break;
        };
        pos += payload_len;

        // Decode; skip on corrupt — treat as cache miss for this file only.
        if let Some(entry) = decode_entry(payload) {
            map.insert(sha, entry);
        }
        // A corrupt entry is simply not inserted — the file will be re-extracted.
    }

    Some(map)
}

// ============================================================================
// Public API
// ============================================================================

/// In-memory AST n-gram cache, backed by `ast_index.skcache`.
///
/// The cache stores its own `cache_dir` so callers do not need to carry the path
/// separately for [`AstNgramCache::save`].  This mirrors the [`crate::FileManifest`]
/// pattern where both the root and the cache directory are stored as struct fields.
///
/// # Lifecycle
///
/// 1. [`AstNgramCache::load`] at the start of a build — reads the prior skcache
///    and stores `cache_dir` for later writes.
/// 2. [`AstNgramCache::lookup`] during the consume loop — for each file, check
///    whether the SHA already has cached n-grams.
/// 3. [`AstNgramCache::insert`] during the consume loop — record fresh payloads
///    on cache miss.
/// 4. [`AstNgramCache::save`] after `ast_builder.build()` and BEFORE
///    `new_manifest.save()` — atomically writes the new skcache. (applies ADR-006)
pub struct AstNgramCache {
    /// Entries keyed by content SHA-256 (64-char hex string).
    entries: HashMap<String, CachedAstEntry>,
    /// Directory where `ast_index.skcache` is written.
    ///
    /// `PathBuf::new()` (empty path) when the cache was created via [`Self::empty`]
    /// with no backing store — callers must not call [`Self::save`] on such instances.
    cache_dir: std::path::PathBuf,
}

impl AstNgramCache {
    /// Load the prior `ast_index.skcache` from `cache_dir`.
    ///
    /// Stores `cache_dir` internally so [`Self::save`] requires no path argument,
    /// matching the [`crate::FileManifest`] pattern.
    ///
    /// Returns an empty cache on any of:
    /// - File not found (first build, or post-upgrade).
    /// - Version mismatch (extraction algorithm or weight table changed).
    /// - Corrupt magic or header.
    /// - I/O error.
    ///
    /// Never returns `Err` — failures are always silently degraded to an empty
    /// cache so the build continues with full re-extraction.
    #[must_use]
    pub fn load(cache_dir: &Path) -> Self {
        let path = cache_dir.join(CACHE_FILENAME);

        // Guard: reject oversized skcache files before reading into memory.
        // Mirrors `FileManifest::load`'s `MAX_MANIFEST_FILE_BYTES` pre-check.
        // The per-entry caps (MAX_ENTRY_BYTES, MAX_CACHE_ENTRIES) apply inside
        // `decode_file` after the whole-file read, so an unbounded `fs::read`
        // without this guard would materialise the entire file in RAM first.
        // (applies ADR-003 — per-file caps are necessary but not sufficient)
        if path
            .metadata()
            .is_ok_and(|m| m.len() > MAX_CACHE_FILE_BYTES)
        {
            // Oversized — discard silently and cold-start.
            return Self::with_dir(cache_dir);
        }

        let Ok(bytes) = std::fs::read(&path) else {
            // Not found or unreadable — cold start.
            return Self::with_dir(cache_dir);
        };
        // Version mismatch or corrupt magic → None → cold start.
        decode_file(&bytes)
            .map(|entries| Self {
                entries,
                cache_dir: cache_dir.to_owned(),
            })
            .unwrap_or_else(|| Self::with_dir(cache_dir))
    }

    /// Create an empty cache bound to `cache_dir`.
    ///
    /// Use this when constructing a cache that will later be populated and saved
    /// (e.g. on `--force` builds or in `flush_empty`).  The stored `cache_dir`
    /// is used by [`Self::save`] so callers need not carry the path separately.
    ///
    /// Prefer [`Self::load`] for the incremental build path — it reads the prior
    /// skcache AND stores the directory.  This constructor is for the `--force`
    /// and empty-project paths where no prior skcache is consulted.
    #[must_use]
    pub fn with_dir(cache_dir: &Path) -> Self {
        Self {
            entries: HashMap::new(),
            cache_dir: cache_dir.to_owned(),
        }
    }

    /// Create a detached empty cache with no backing store.
    ///
    /// For test helpers and throwaway consumers (e.g. ADR-006 abort tests) that
    /// inspect the cache in-memory but never call [`Self::save`].  Calling `save`
    /// on an `empty()` instance will attempt to write to the current directory
    /// (empty path) and likely fail — callers that need `save` must use
    /// [`Self::with_dir`] or [`Self::load`] instead.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            entries: HashMap::new(),
            cache_dir: std::path::PathBuf::new(),
        }
    }

    /// Look up a cached entry by content SHA-256 hex string.
    ///
    /// Returns `Some(&CachedAstEntry)` on a hit, `None` on a miss.
    ///
    /// # SHA-keying semantics
    ///
    /// Two distinct paths with byte-identical content share one SHA and thus
    /// one cache entry.  This is correct — AST n-grams are content-derived and
    /// path-independent.  A renamed file with the same content is a cache hit.
    #[must_use]
    pub fn lookup(&self, sha: &str) -> Option<&CachedAstEntry> {
        self.entries.get(sha)
    }

    /// Insert or replace a cached entry for the given SHA.
    ///
    /// Called on every cache miss so fresh payloads are recorded for the next
    /// build.  Also called for empty payloads (data-format files, large files)
    /// so they are served from cache on the next build without re-calling
    /// `linearize_source`. Empty payloads are valid cache entries, not corrupt.
    pub fn insert(&mut self, sha: String, entry: CachedAstEntry) {
        self.entries.insert(sha, entry);
    }

    /// Insert `entry` for `sha` if absent, then return a shared borrow.
    ///
    /// Uses the HashMap Entry API so the key is hashed exactly once — no
    /// insert-then-lookup double-probe.  The borrow is valid for `'_` (tied
    /// to `&mut self`), so the caller can use the returned reference for
    /// `add_file_ngrams` without an additional lookup.
    ///
    /// # Duplicate-SHA note
    ///
    /// If two distinct files share the same content SHA (content-addressed
    /// deduplication), only the first insert wins and both files borrow the
    /// same entry — which is the correct and intended behaviour.
    pub fn get_or_insert(&mut self, sha: String, entry: CachedAstEntry) -> &CachedAstEntry {
        self.entries.entry(sha).or_insert(entry)
    }

    /// Atomically write `ast_index.skcache` to the `cache_dir` stored at construction.
    ///
    /// Uses `io_util::atomic_write` (temp file + rename) so readers never
    /// observe a partial write.  The written file contains only the entries
    /// accumulated during the current build — deleted/renamed files self-prune
    /// because their SHAs are never inserted.
    ///
    /// The `cache_dir` is stored as a field (set by [`Self::load`] or
    /// [`Self::with_dir`]), matching the [`crate::FileManifest`] pattern where
    /// callers do not need to carry the path separately.
    ///
    /// # Errors
    ///
    /// Returns `Err` on I/O failure (temp file, write, rename).  The caller
    /// (index.rs `Pipeline::run`) must propagate the error BEFORE calling
    /// `new_manifest.save()`, ensuring the manifest is never saved when the
    /// skcache write fails.  This preserves the ADR-006 invariant: the next
    /// query self-heals via full rebuild. (applies ADR-006)
    pub fn save(&self) -> Result<()> {
        let path = self.cache_dir.join(CACHE_FILENAME);
        let buf = encode_file(&self.entries);
        atomic_write(&self.cache_dir, &path, &buf)
    }

    /// Return the number of entries in the cache.
    ///
    /// Used in tests and summary reporting.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return `true` when the cache contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "ast_cache_tests.rs"]
mod tests;
