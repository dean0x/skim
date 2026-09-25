//! File-based caching layer for transformed output
//!
//! ARCHITECTURE: Cache transformed results with mtime-based invalidation.
//! - Cache key: SHA256(canonical_path + mtime_secs + mode)
//! - Cache location: $SKIM_CACHE_DIR or ~/.cache/skim/ (platform-specific)
//! - Invalidation: File mtime change or mode change
//! - Storage format: JSON with metadata
//!
//! # Cache-directory resolution (PF-002 fix)
//!
//! All skim cache subsystems (parser cache, tee output, default analytics.db)
//! resolve their root through [`cache_root`] / [`cache_root_from`] so that
//! `SKIM_CACHE_DIR` reliably relocates ALL cache state.
//!
//! # Lifecycle: this cache is UNBOUNDED (known gap, not yet addressed)
//!
//! There is no size cap, no TTL and no sweep. Every distinct key writes a new
//! `<sha256>.json` and nothing ever reclaims it. The only reclaim is an
//! explicit `--clear-cache` ([`clear_cache`]); the `fs::remove_file` on the
//! stale-entry branch of [`read_cache`] is effectively unreachable, because
//! `mtime_secs` and `mode` are *inside* the key — a mismatch on either lands
//! on a different filename, so the branch that deletes the superseded entry is
//! never the one a real invocation reaches.
//!
//! The key's fan-out has grown, so orphans accrue faster than they used to:
//! `line_numbers` and then `notice_bytes` joined the key, and `notice_bytes`
//! alone takes three distinct values for one file (hook origin, direct, batch)
//! — up to three orphans per edit rather than one. The
//! `CACHE_SCHEMA_VERSION` 2 → 3 bump orphaned every warm entry once.
//!
//! Adding a bound is a new cache-lifecycle policy (eviction order, budget,
//! when the sweep runs) and is deliberately **out of scope** here; this note
//! exists so the next reader finds the gap written down rather than by
//! surprise.

use anyhow::Result;
use rskim_core::Mode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::cascade::TruncationOptions;

// ============================================================================
// Single-source-of-truth cache-root resolvers (fixes PF-002)
// ============================================================================

/// Resolve the cache root from an explicit override or the platform default.
///
/// Convention (locked — avoids PF-002):
/// - If `override_dir` is `Some(p)` and `p` is non-empty (and not whitespace-only
///   UTF-8), use `p` as-is (caller's explicit override wins; we do NOT append `skim`).
/// - An empty or whitespace-only UTF-8 path is treated as unset.
///   Non-UTF8 `OsStr` paths are never trimmed — they are valid and used as-is.
/// - Otherwise fall back to `dirs::cache_dir().join("skim")`.
///
/// This function is **pure** — it performs no I/O, no mkdir.
pub(crate) fn cache_root_from(override_dir: Option<PathBuf>) -> Option<PathBuf> {
    override_dir
        .filter(|p| {
            if p.as_os_str().is_empty() {
                return false;
            }
            // For valid UTF-8 paths, treat whitespace-only values as unset.
            // Non-UTF-8 paths are preserved unchanged (they are valid filesystem paths).
            match p.to_str() {
                Some(s) => !s.trim().is_empty(),
                None => true, // non-UTF8: valid path, do not reject
            }
        })
        .or_else(|| dirs::cache_dir().map(|c| c.join("skim")))
}

/// Read `SKIM_CACHE_DIR` from the process environment as a `PathBuf`, if set.
///
/// Single env-read entry point shared by [`cache_root`] and
/// [`crate::cmd::hook_log::CacheEnv::from_process`] so the variable name is
/// only referenced in one place (avoids PF-002 drift).
pub(crate) fn read_cache_dir_env() -> Option<PathBuf> {
    std::env::var_os("SKIM_CACHE_DIR").map(PathBuf::from)
}

/// Resolve the cache root from the process environment.
///
/// Reads `SKIM_CACHE_DIR` via [`read_cache_dir_env`] and delegates to
/// [`cache_root_from`].
///
/// Single source of truth: `cmd::hook_log::CacheEnv::resolve_cache_dir` delegates
/// to [`cache_root_from`] directly, so the two resolvers cannot drift — there is
/// no "keep in sync" obligation (avoids PF-002).
pub(crate) fn cache_root() -> Option<PathBuf> {
    cache_root_from(read_cache_dir_env())
}

/// Cache entry with metadata for validation.
#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    /// Original file path (for debugging).
    path: String,
    /// File modification time (seconds since UNIX epoch).
    mtime_secs: u64,
    /// Transformation mode.
    mode: String,
    /// Cached transformed output.
    content: String,
    /// Original token count (optional for backward compatibility).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_tokens: Option<usize>,
    /// Transformed token count (optional for backward compatibility).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transformed_tokens: Option<usize>,
    /// Diagnostic metadata: records the effective mode when cascade selected a
    /// different mode than the one requested.  Written for post-hoc inspection
    /// of cache entries (e.g. `jq .effective_mode ~/.cache/skim/*.json`) but
    /// intentionally not returned by [`read_cache`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effective_mode: Option<String>,
    /// Parse quality tier at transform time: "full", "degraded", or "passthrough".
    ///
    /// Old cache entries without this field deserialize with `None` (backward-compatible).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parse_tier: Option<String>,
    /// Whether the served view differs from the raw file bytes.
    ///
    /// Written from the authoritative byte comparison in `process_file` so the
    /// cache-hit path in `try_cached_result` does not have to infer it from the
    /// mode (consistency-2: mode-inference is wrong when the ADR-001 guardrail
    /// chose raw bytes).
    ///
    /// `None` on backward-compatible reads of old entries; callers fall back to
    /// mode-inference in that case (`mode != Mode::Full`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    view_differs: Option<bool>,
}

/// Data returned on a successful cache lookup.
#[derive(Debug)]
pub(crate) struct CacheHit {
    /// Transformed output content.
    pub(crate) content: String,
    /// Original token count (if available).
    pub(crate) original_tokens: Option<usize>,
    /// Transformed token count (if available).
    pub(crate) transformed_tokens: Option<usize>,
    /// Whether the served view differs from the raw file bytes.
    ///
    /// `None` for cache entries written before this field was added — callers
    /// fall back to mode-inference (`mode != Mode::Full`) in that case.
    pub(crate) view_differs: Option<bool>,
}

/// Everything that identifies a cache entry.
///
/// **One** declaration of that identity, shared by the read side
/// ([`read_cache`]), the write side (embedded in [`CacheWriteParams`]) and the
/// hash itself ([`cache_key`]).  A component added here is in the key on
/// *both* paths or on neither; it cannot be threaded into the write side while
/// the read side keeps hashing the old shape.
///
/// [`cache_key`] is the one place in this module where a silent mistake is
/// unrecoverable: a wrong key does not error, it serves **another
/// invocation's stdout** (see `notice_bytes`).  That is why the identity is a
/// named record rather than a run of positional arguments.
///
/// The file's modification time is deliberately **not** a field: it is not
/// caller-supplied.  [`read_cache`] and [`write_cache`] each stat the file and
/// hand the result to [`cache_key`], so no caller can pair a key with an mtime
/// that disagrees with the entry it is about to read or write.
pub(crate) struct CacheKeyParams<'a> {
    /// Source file.  Canonicalised into the hash, so two paths naming the same
    /// file share one entry.
    pub(crate) path: &'a Path,
    /// Transformation mode.
    pub(crate) mode: Mode,
    /// Truncation options (max_lines, last_lines, token_budget).
    pub(crate) trunc: TruncationOptions,
    /// Whether line numbers were applied.
    ///
    /// Line-numbered and unnumbered outputs are cached separately because they differ.
    pub(crate) line_numbers: bool,
    /// Byte length of the prospective lossy-view marker.
    ///
    /// The ADR-001 guard charges this disclosure against the compressed view, so
    /// two invocations that differ only in rewrite origin or batch-ness can
    /// produce different stdout for the same file. See [`cache_key`] and
    /// `process::view_notice_cache_bytes`, which is the sole producer of this
    /// value on the live path.
    pub(crate) notice_bytes: usize,
}

/// Parameters for writing a cache entry.
pub(crate) struct CacheWriteParams<'a> {
    /// What identifies this entry — the same record [`read_cache`] takes, so
    /// the two sides cannot describe different entries.
    pub(crate) key: CacheKeyParams<'a>,
    /// Transformed output to cache.
    pub(crate) content: &'a str,
    /// Original token count (if computed).
    pub(crate) original_tokens: Option<usize>,
    /// Transformed token count (if computed).
    pub(crate) transformed_tokens: Option<usize>,
    /// Effective mode after cascade (diagnostic metadata only).
    pub(crate) effective_mode: Option<Mode>,
    /// Parse quality tier: "full", "degraded", or "passthrough" (diagnostic metadata).
    pub(crate) parse_tier: Option<String>,
    /// Whether the served view differs from the raw file bytes.
    ///
    /// Computed from the authoritative byte comparison in `process_file` and stored
    /// here so the cache-hit path in `try_cached_result` can reproduce the correct
    /// answer without re-reading the file (consistency-2).
    pub(crate) view_differs: bool,
}

/// Returns the skim cache directory, creating it with owner-only permissions if it does not
/// yet exist.
///
/// Honors `SKIM_CACHE_DIR` (via [`cache_root`]) so that ALL subsystems that call this
/// function — parser cache, tee output, and the default analytics.db path — relocate
/// consistently when the env var is set. Fixes PF-002.
pub(crate) fn get_cache_dir() -> Result<PathBuf> {
    let cache_dir =
        cache_root().ok_or_else(|| anyhow::anyhow!("Failed to determine cache directory"))?;

    #[cfg(unix)]
    {
        use std::fs::DirBuilder;
        use std::os::unix::fs::DirBuilderExt;

        let mut builder = DirBuilder::new();
        builder.mode(0o700); // rwx------
        builder.recursive(true);
        builder.create(&cache_dir)?;
    }

    #[cfg(not(unix))]
    {
        fs::create_dir_all(&cache_dir)?;
    }

    Ok(cache_dir)
}

/// Cache schema version — MUST be bumped whenever output bytes change.
///
/// This constant is folded into the SHA-256 hash that names every cache file.
/// When this value changes, every existing cache entry silently misses (the
/// old file is at a different hash path) and is re-generated on first access.
/// This guarantees that a warm cache never serves stale bytes after an
/// output-format change in a later phase of the fidelity overhaul.
///
/// **Rule**: bump this constant in the same commit that changes output bytes.
/// Do not update it for changes that do not affect what `transform()` emits.
///
/// v3 (ADR-001 amendment 2026-09-24): the ADR-001 net-savings guard now charges
/// the stderr lossy-view disclosure, so stdout bytes depend on `notice_bytes`
/// as well. Every v2 entry was written by a guard that priced the body only.
const CACHE_SCHEMA_VERSION: u32 = 3;

/// Generate the cache key for the entry `p` identifies, as of `mtime`.
///
/// `mtime` is separate from [`CacheKeyParams`] because it is read from the
/// filesystem by [`read_cache`] / [`write_cache`] immediately before this call,
/// never supplied by a caller — see that type's note.
///
/// `line_numbers` is included in the key because line-numbered and unnumbered outputs
/// differ in content and should be cached independently.
///
/// `notice_bytes` is included because the ADR-001 guard now charges the stderr
/// disclosure against the compressed view, and that disclosure's size depends
/// on the rewrite origin (`SKIM_REWRITTEN_FROM`) and on whether the read is part
/// of a batch — **neither of which appears anywhere else in this key**. Without
/// it, `cat foo.ts` (rewritten to a `cat`-origin read whose marker costs 162 B)
/// and `skim foo.ts --mode=pseudo` (direct, 128 B) hash identically and serve
/// each other's stdout wherever that 34-byte difference straddles the guard's
/// threshold — a silent correctness bug with no error and no diagnostic.
///
/// A length rather than the text: it is exactly what the guard prices on, it
/// collapses origin, batch-ness and mode into one field, and it makes a future
/// marker-wording edit self-invalidating.
///
/// `CACHE_SCHEMA_VERSION` is included so that any change to the output format
/// (a later phase of the fidelity overhaul) automatically invalidates all warm
/// entries without needing to clear the cache manually.
fn cache_key(p: &CacheKeyParams<'_>, mtime: SystemTime) -> Result<String> {
    let canonical_path = p.path.canonicalize()?;
    let mtime_secs = mtime.duration_since(SystemTime::UNIX_EPOCH)?.as_secs();

    let opt_str = |opt: Option<usize>| opt.map_or("none".to_string(), |n| n.to_string());

    // Bound out because inline format args take an identifier, not a field
    // access. The hash input is byte-identical to the positional form this
    // replaced, so warm v3 entries are not orphaned by the refactor.
    let mode = p.mode;
    let hash_input = format!(
        "cache_schema_v{}|{}|{}|{:?}|{}|{}|{}|{}|{}",
        CACHE_SCHEMA_VERSION,
        canonical_path.display(),
        mtime_secs,
        mode,
        opt_str(p.trunc.max_lines),
        opt_str(p.trunc.last_lines),
        opt_str(p.trunc.token_budget),
        p.line_numbers as u8,
        p.notice_bytes,
    );

    let mut hasher = Sha256::new();
    hasher.update(hash_input.as_bytes());

    Ok(format!("{:x}", hasher.finalize()))
}

/// Read cached output if valid (mtime matches).
///
/// Returns a [`CacheHit`] on cache hit, `None` on miss.
pub(crate) fn read_cache(p: &CacheKeyParams<'_>) -> Option<CacheHit> {
    let metadata = fs::metadata(p.path).ok()?;
    let mtime = metadata.modified().ok()?;

    let key = cache_key(p, mtime).ok()?;
    let cache_file = get_cache_dir().ok()?.join(format!("{key}.json"));

    let cache_content = fs::read_to_string(&cache_file).ok()?;
    let entry: CacheEntry = serde_json::from_str(&cache_content).ok()?;

    // Belt-and-suspenders validation: verify mtime/mode match even though
    // they are already encoded in the cache key hash (guards against collisions).
    let mtime_secs = mtime.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
    let mode = p.mode;
    let mode_str = format!("{mode:?}");

    if entry.mtime_secs == mtime_secs && entry.mode == mode_str {
        Some(CacheHit {
            content: entry.content,
            original_tokens: entry.original_tokens,
            transformed_tokens: entry.transformed_tokens,
            view_differs: entry.view_differs,
        })
    } else {
        // Stale entry: best-effort cleanup.
        let _ = fs::remove_file(&cache_file);
        None
    }
}

/// Write transformed output to cache.
///
/// See the module-level **Lifecycle** note: nothing reclaims what this writes.
pub(crate) fn write_cache(params: &CacheWriteParams<'_>) -> Result<()> {
    let metadata = fs::metadata(params.key.path)?;
    let mtime = metadata.modified()?;

    let key = cache_key(&params.key, mtime)?;
    let cache_file = get_cache_dir()?.join(format!("{key}.json"));

    let mtime_secs = mtime.duration_since(SystemTime::UNIX_EPOCH)?.as_secs();
    let mode = params.key.mode;
    let entry = CacheEntry {
        path: params.key.path.display().to_string(),
        mtime_secs,
        mode: format!("{mode:?}"),
        content: params.content.to_string(),
        original_tokens: params.original_tokens,
        transformed_tokens: params.transformed_tokens,
        effective_mode: params.effective_mode.map(|m| format!("{m:?}")),
        parse_tier: params.parse_tier.clone(),
        view_differs: Some(params.view_differs),
    };

    let json = serde_json::to_string(&entry)?;
    fs::write(&cache_file, json)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&cache_file, fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

/// Clear entire cache directory.
///
/// Removes all files inside the cache directory rather than the directory
/// itself. This avoids ENOTEMPTY races when concurrent processes write
/// cache entries during deletion.
pub(crate) fn clear_cache() -> Result<()> {
    let cache_dir = get_cache_dir()?;

    if cache_dir.exists() {
        for entry in fs::read_dir(&cache_dir)? {
            let entry = entry?;
            let path = entry.path();
            // Only remove JSON cache files; skip analytics.db and other non-cache files.
            if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
                // Best-effort removal; ignore errors from concurrent access.
                let _ = fs::remove_file(&path);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    /// The common cache identity in these tests: no line numbers, no
    /// disclosure. Tests that vary one of those build on it with `..`, so the
    /// field they are exercising is the only one they name.
    fn key_params(path: &Path, mode: Mode, trunc: TruncationOptions) -> CacheKeyParams<'_> {
        CacheKeyParams {
            path,
            mode,
            trunc,
            line_numbers: false,
            notice_bytes: 0,
        }
    }

    // ========================================================================
    // C2: single-source-of-truth contract
    // cache::cache_root() and cmd::resolve_cache_dir() (which delegates to
    // hook_log::CacheEnv::from_process().resolve_cache_dir()) must agree for
    // the SAME env state (avoids PF-002 regression).
    //
    // Note: hook_log is a private module; we access it through the pub(crate)
    // re-export cmd::resolve_cache_dir which already delegates to CacheEnv.
    // ========================================================================

    /// C2: Assert cache::cache_root() == cmd::resolve_cache_dir() for the same env
    /// state, proving single-source-of-truth for cache-dir resolution (PF-002 guard).
    ///
    /// Uses #[serial_test::serial] to prevent env-var mutation races.
    #[test]
    #[serial_test::serial]
    fn test_c2_cache_root_agrees_with_cmd_resolver_no_override() {
        // Safety: serial ensures we are single-threaded when touching env vars.
        // Unset SKIM_CACHE_DIR to test default resolution path.
        // SAFETY: test-only, serial-gated.
        unsafe { std::env::remove_var("SKIM_CACHE_DIR") };

        let from_cache = cache_root();
        // cmd::resolve_cache_dir() delegates to CacheEnv::from_process().resolve_cache_dir()
        let from_cmd = crate::cmd::resolve_cache_dir();

        assert_eq!(
            from_cache, from_cmd,
            "cache_root() and cmd::resolve_cache_dir() must agree \
             (PF-002 regression guard — SKIM_CACHE_DIR unset)"
        );
    }

    /// C2 override path: both resolvers agree when SKIM_CACHE_DIR is set.
    #[test]
    #[serial_test::serial]
    fn test_c2_cache_root_agrees_with_cmd_resolver_with_override() {
        // SAFETY: test-only, serial-gated.
        unsafe { std::env::set_var("SKIM_CACHE_DIR", "/tmp/skim-test-cache-c2") };

        let from_cache = cache_root();
        // cmd::resolve_cache_dir() delegates to CacheEnv::from_process().resolve_cache_dir()
        let from_cmd = crate::cmd::resolve_cache_dir();

        // Restore before any assertion can fail.
        unsafe { std::env::remove_var("SKIM_CACHE_DIR") };

        assert_eq!(
            from_cache, from_cmd,
            "cache_root() and cmd::resolve_cache_dir() must agree \
             (PF-002 regression guard — SKIM_CACHE_DIR set)"
        );

        // Also assert the expected resolved path.
        assert_eq!(
            from_cache,
            Some(std::path::PathBuf::from("/tmp/skim-test-cache-c2")),
            "SKIM_CACHE_DIR should be used as-is (no 'skim' suffix appended)"
        );
    }

    /// B7: empty SKIM_CACHE_DIR is treated as unset — falls back to platform default.
    #[test]
    #[serial_test::serial]
    fn test_b7_empty_skim_cache_dir_treated_as_unset() {
        // SAFETY: test-only, serial-gated.
        unsafe { std::env::set_var("SKIM_CACHE_DIR", "") };
        let with_empty = cache_root();
        unsafe { std::env::remove_var("SKIM_CACHE_DIR") };
        let without = cache_root();

        assert_eq!(
            with_empty, without,
            "Empty SKIM_CACHE_DIR must fall back to platform default (B7)"
        );
        // Both should resolve to the platform default (~/.cache/skim).
        assert!(
            with_empty.is_some(),
            "Platform default must be available (dirs::cache_dir works)"
        );
    }

    /// B5: neither SKIM_CACHE_DIR nor SKIM_ANALYTICS_DB set => default path unchanged.
    #[test]
    #[serial_test::serial]
    fn test_b5_default_path_uses_platform_cache_dir() {
        // SAFETY: test-only, serial-gated.
        unsafe { std::env::remove_var("SKIM_CACHE_DIR") };

        let root = cache_root();
        let expected = dirs::cache_dir().map(|c| c.join("skim"));
        assert_eq!(
            root, expected,
            "Default cache root must be ~/.cache/skim (B5)"
        );
    }

    #[test]
    fn test_cache_key_generation() {
        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "test content").unwrap();
        let path = temp_file.path();

        let metadata = fs::metadata(path).unwrap();
        let mtime = metadata.modified().unwrap();

        let default_trunc = TruncationOptions::default();

        // Same inputs should produce same key
        let key1 = cache_key(&key_params(path, Mode::Structure, default_trunc), mtime).unwrap();
        let key2 = cache_key(&key_params(path, Mode::Structure, default_trunc), mtime).unwrap();
        assert_eq!(key1, key2);

        // Different mode should produce different key
        let key3 = cache_key(&key_params(path, Mode::Signatures, default_trunc), mtime).unwrap();
        assert_ne!(key1, key3);

        // Different max_lines should produce different key
        let trunc_max = TruncationOptions {
            max_lines: Some(50),
            ..Default::default()
        };
        let key4 = cache_key(&key_params(path, Mode::Structure, trunc_max), mtime).unwrap();
        assert_ne!(key1, key4);

        // Same max_lines should produce same key
        let key5 = cache_key(&key_params(path, Mode::Structure, trunc_max), mtime).unwrap();
        assert_eq!(key4, key5);

        // Different token_budget should produce different key
        let trunc_budget = TruncationOptions {
            token_budget: Some(500),
            ..Default::default()
        };
        let key6 = cache_key(&key_params(path, Mode::Structure, trunc_budget), mtime).unwrap();
        assert_ne!(key1, key6);

        // Same token_budget should produce same key
        let key7 = cache_key(&key_params(path, Mode::Structure, trunc_budget), mtime).unwrap();
        assert_eq!(key6, key7);

        // Different max_lines + token_budget combination
        let trunc_both = TruncationOptions {
            max_lines: Some(50),
            token_budget: Some(500),
            ..Default::default()
        };
        let key8 = cache_key(&key_params(path, Mode::Structure, trunc_both), mtime).unwrap();
        assert_ne!(key4, key8);
        assert_ne!(key6, key8);

        // Different last_lines should produce different key
        let trunc_last = TruncationOptions {
            last_lines: Some(10),
            ..Default::default()
        };
        let key9 = cache_key(&key_params(path, Mode::Structure, trunc_last), mtime).unwrap();
        assert_ne!(key1, key9);

        // Same last_lines should produce same key
        let key10 = cache_key(&key_params(path, Mode::Structure, trunc_last), mtime).unwrap();
        assert_eq!(key9, key10);

        // Different line_numbers should produce different key
        let key11 = cache_key(
            &CacheKeyParams {
                line_numbers: true,
                ..key_params(path, Mode::Structure, default_trunc)
            },
            mtime,
        )
        .unwrap();
        assert_ne!(key1, key11);
    }

    #[test]
    fn test_cache_read_write() {
        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "test content").unwrap();
        let path = temp_file.path().to_path_buf();
        let default_trunc = TruncationOptions::default();

        // Initially no cache
        assert!(read_cache(&key_params(&path, Mode::Structure, default_trunc)).is_none());

        // Write to cache with token counts
        let content = "transformed output";
        write_cache(&CacheWriteParams {
            key: key_params(&path, Mode::Structure, default_trunc),
            content,
            original_tokens: Some(100),
            transformed_tokens: Some(50),
            effective_mode: None,
            parse_tier: None,
            view_differs: false,
        })
        .unwrap();

        // Read from cache
        let hit = read_cache(&key_params(&path, Mode::Structure, default_trunc)).unwrap();
        assert_eq!(hit.content, content);
        assert_eq!(hit.original_tokens, Some(100));
        assert_eq!(hit.transformed_tokens, Some(50));

        // Different mode should not find cache
        assert!(read_cache(&key_params(&path, Mode::Signatures, default_trunc)).is_none());

        // Different max_lines should not find cache
        let trunc_max = TruncationOptions {
            max_lines: Some(50),
            ..Default::default()
        };
        assert!(read_cache(&key_params(&path, Mode::Structure, trunc_max)).is_none());

        // Different last_lines should not find cache
        let trunc_last = TruncationOptions {
            last_lines: Some(10),
            ..Default::default()
        };
        assert!(read_cache(&key_params(&path, Mode::Structure, trunc_last)).is_none());

        // Different token_budget should not find cache
        let trunc_budget = TruncationOptions {
            token_budget: Some(500),
            ..Default::default()
        };
        assert!(read_cache(&key_params(&path, Mode::Structure, trunc_budget)).is_none());
    }

    #[test]
    fn test_cache_read_write_with_token_budget() {
        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "test content for token budget").unwrap();
        let path = temp_file.path().to_path_buf();

        let trunc = TruncationOptions {
            token_budget: Some(500),
            ..Default::default()
        };

        // No cache initially
        assert!(read_cache(&key_params(&path, Mode::Structure, trunc)).is_none());

        // Write with token_budget
        write_cache(&CacheWriteParams {
            key: key_params(&path, Mode::Structure, trunc),
            content: "budget-transformed output",
            original_tokens: Some(200),
            transformed_tokens: Some(80),
            effective_mode: None,
            parse_tier: None,
            view_differs: false,
        })
        .unwrap();

        // Read with same token_budget succeeds
        let hit = read_cache(&key_params(&path, Mode::Structure, trunc)).unwrap();
        assert_eq!(hit.content, "budget-transformed output");
        assert_eq!(hit.original_tokens, Some(200));
        assert_eq!(hit.transformed_tokens, Some(80));

        // Read without token_budget misses (different cache key)
        let default_trunc = TruncationOptions::default();
        assert!(read_cache(&key_params(&path, Mode::Structure, default_trunc)).is_none());

        // Read with different token_budget misses
        let trunc_1000 = TruncationOptions {
            token_budget: Some(1000),
            ..Default::default()
        };
        assert!(read_cache(&key_params(&path, Mode::Structure, trunc_1000)).is_none());

        // Read with same budget + different mode misses
        assert!(read_cache(&key_params(&path, Mode::Signatures, trunc)).is_none());
    }

    #[test]
    fn test_cache_stores_effective_mode() {
        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "effective mode test content").unwrap();
        let path = temp_file.path().to_path_buf();

        let trunc = TruncationOptions {
            token_budget: Some(100),
            ..Default::default()
        };

        // Write with effective_mode set (simulates cascade escalation)
        write_cache(&CacheWriteParams {
            key: key_params(&path, Mode::Structure, trunc),
            content: "escalated output",
            original_tokens: Some(150),
            transformed_tokens: Some(60),
            effective_mode: Some(Mode::Signatures),
            parse_tier: None,
            view_differs: true,
        })
        .unwrap();

        // Read back succeeds (effective_mode is diagnostic-only, not part of CacheHit)
        let hit = read_cache(&key_params(&path, Mode::Structure, trunc)).unwrap();
        assert_eq!(hit.content, "escalated output");
        assert_eq!(hit.original_tokens, Some(150));
        assert_eq!(hit.transformed_tokens, Some(60));

        // Verify the effective_mode field was serialized in the raw JSON
        let metadata = fs::metadata(&path).unwrap();
        let mtime = metadata.modified().unwrap();
        let key = cache_key(&key_params(&path, Mode::Structure, trunc), mtime).unwrap();
        let cache_file = get_cache_dir().unwrap().join(format!("{key}.json"));
        let raw_json = fs::read_to_string(&cache_file).unwrap();
        let raw: serde_json::Value = serde_json::from_str(&raw_json).unwrap();
        assert_eq!(
            raw["effective_mode"].as_str(),
            Some("Signatures"),
            "effective_mode should be serialized in cache entry JSON"
        );
    }

    #[test]
    fn test_cache_invalidation_on_mtime_change() {
        use std::fs::File;
        use std::io::Write as IoWrite;

        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();
        let default_trunc = TruncationOptions::default();

        // Write initial content
        {
            let mut file = File::create(&path).unwrap();
            file.write_all(b"original content").unwrap();
            file.flush().unwrap();
        }

        // Write to cache
        write_cache(&CacheWriteParams {
            key: key_params(&path, Mode::Structure, default_trunc),
            content: "cached v1",
            original_tokens: None,
            transformed_tokens: None,
            effective_mode: None,
            parse_tier: None,
            view_differs: false,
        })
        .unwrap();
        let hit = read_cache(&key_params(&path, Mode::Structure, default_trunc)).unwrap();
        assert_eq!(hit.content, "cached v1");

        // Sleep to ensure mtime resolution (some filesystems have 1-second resolution)
        std::thread::sleep(std::time::Duration::from_secs(1));

        // Modify file (changes mtime)
        {
            let mut file = File::create(&path).unwrap();
            file.write_all(b"modified content").unwrap();
            file.flush().unwrap();
        }

        // Cache should be invalidated (mtime changed)
        assert!(read_cache(&key_params(&path, Mode::Structure, default_trunc)).is_none());
    }

    /// A1: CACHE_SCHEMA_VERSION is folded into the hash key.
    ///
    /// Proof strategy: compare the production key (which includes
    /// `cache_schema_v{N}|...`) against a key computed from the same inputs
    /// but WITHOUT any version prefix (the pre-A1 baseline).  They must
    /// differ, which means bumping CACHE_SCHEMA_VERSION always produces a
    /// different filename and automatically invalidates any warm entry written
    /// by an older build.
    ///
    /// If this test ever fails, `hash_input` no longer includes the schema
    /// version and the safety guarantee is broken — a format change in a
    /// later phase of the fidelity overhaul could serve stale cached bytes.
    #[test]
    fn test_schema_version_in_cache_key() {
        use sha2::{Digest, Sha256};

        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "schema version test content").unwrap();
        let path = temp_file.path();
        let metadata = fs::metadata(path).unwrap();
        let mtime = metadata.modified().unwrap();
        let trunc = TruncationOptions::default();

        // Production key — uses `cache_schema_v{CACHE_SCHEMA_VERSION}|...`
        let key_production = cache_key(&key_params(path, Mode::Structure, trunc), mtime).unwrap();

        // Simulate the pre-A1 hash: same inputs, NO version prefix.
        // If `key_production == key_legacy`, CACHE_SCHEMA_VERSION is absent
        // from hash_input and the invalidation guarantee is broken.
        let canonical = path.canonicalize().unwrap();
        let mtime_secs = mtime
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let legacy_input = format!(
            "{}|{}|{:?}|none|none|none|0",
            canonical.display(),
            mtime_secs,
            Mode::Structure,
        );
        let mut hasher = Sha256::new();
        hasher.update(legacy_input.as_bytes());
        let key_legacy = format!("{:x}", hasher.finalize());

        assert_ne!(
            key_production, key_legacy,
            "CACHE_SCHEMA_VERSION must be included in hash_input. \
             Without it, output-format changes in later phases cannot \
             invalidate warm cache entries. \
             Bump CACHE_SCHEMA_VERSION in the same commit that changes \
             what transform() emits."
        );
    }

    /// The hook-rewritten read and the hand-typed read of the SAME file must
    /// not share a cache entry.
    ///
    /// `cat foo.ts` is rewritten to a `cat`-origin `skim foo.ts --mode=pseudo`;
    /// `skim foo.ts --mode=pseudo` is that same command minus the origin tag.
    /// Path, mtime, mode, truncation options and `line_numbers` are identical,
    /// so before `notice_bytes` joined the key the two hashed to the SAME file
    /// name.
    ///
    /// That was harmless only while the two produced identical stdout. The
    /// ADR-001 guard now charges the stderr disclosure, and the two markers
    /// cost different amounts (162 B with the origin, 128 B without), so a file
    /// whose saving falls between them is legitimately COMPRESSED for one
    /// invocation and served RAW for the other. A shared key hands one
    /// invocation the other's stdout, with no error and no diagnostic.
    ///
    /// RED before the `notice_bytes` field: all three keys are equal.
    #[test]
    fn test_cache_key_separates_hook_origin_from_direct_and_batch() {
        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "export function f(a) {{ return a; }}").unwrap();
        let path = temp_file.path();
        let mtime = fs::metadata(path).unwrap().modified().unwrap();
        let trunc = TruncationOptions::default();

        // The two markers `process.rs` would emit for these two invocations.
        let origin_notice = crate::output::lossy_view_marker(Some("cat"), "pseudo", 1, 1)
            .expect("differing=1 must produce a marker");
        let direct_notice = crate::output::lossy_view_marker(None, "pseudo", 1, 1)
            .expect("differing=1 must produce a marker");
        assert_ne!(
            origin_notice.len(),
            direct_notice.len(),
            "precondition: the two invocations must cost the guard different \
             amounts, or there is nothing for the key to separate"
        );

        let orig_len = origin_notice.len();
        let dir_len = direct_notice.len();
        let key_origin = cache_key(
            &CacheKeyParams {
                notice_bytes: orig_len,
                ..key_params(path, Mode::Pseudo, trunc)
            },
            mtime,
        )
        .unwrap();
        let key_direct = cache_key(
            &CacheKeyParams {
                notice_bytes: dir_len,
                ..key_params(path, Mode::Pseudo, trunc)
            },
            mtime,
        )
        .unwrap();
        // A batch read of the same file is charged nothing (one aggregate
        // marker covers the whole run), so it is a third distinct verdict.
        let key_batch = cache_key(&key_params(path, Mode::Pseudo, trunc), mtime).unwrap();

        assert_ne!(
            key_origin, key_direct,
            "`cat foo.ts` and `skim foo.ts --mode=pseudo` must not share a \
             cache entry — the guard charges them different disclosures, so \
             they can produce different stdout for identical input"
        );
        assert_ne!(
            key_batch, key_direct,
            "a batch read pays no marginal disclosure and must key separately"
        );
        assert_ne!(
            key_batch, key_origin,
            "a batch read pays no marginal disclosure and must key separately"
        );
    }
}
