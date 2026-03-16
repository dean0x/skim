//! File-based caching layer for transformed output
//!
//! ARCHITECTURE: Cache transformed results with mtime-based invalidation.
//! - Cache key: SHA256(canonical_path + mtime_secs + mode)
//! - Cache location: ~/.cache/skim/ (platform-specific)
//! - Invalidation: File mtime change or mode change
//! - Storage format: JSON with metadata

use anyhow::Result;
use rskim_core::Mode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
}

/// Data returned on a successful cache lookup.
#[derive(Debug)]
pub struct CacheHit {
    /// Transformed output content.
    pub content: String,
    /// Original token count (if available).
    pub original_tokens: Option<usize>,
    /// Transformed token count (if available).
    pub transformed_tokens: Option<usize>,
}

/// Parameters for writing a cache entry.
pub struct CacheWriteParams<'a> {
    /// Path to the source file.
    pub path: &'a Path,
    /// Transformation mode used for the cache key.
    pub mode: Mode,
    /// Transformed output to cache.
    pub content: &'a str,
    /// Original token count (if computed).
    pub original_tokens: Option<usize>,
    /// Transformed token count (if computed).
    pub transformed_tokens: Option<usize>,
    /// Maximum output lines (part of cache key).
    pub max_lines: Option<usize>,
    /// Token budget (part of cache key).
    pub token_budget: Option<usize>,
    /// Effective mode after cascade (diagnostic metadata only).
    pub effective_mode: Option<Mode>,
}

/// Returns the platform-specific cache directory (`~/.cache/skim/` on Linux/macOS),
/// creating it with owner-only permissions if it does not yet exist.
fn get_cache_dir() -> Result<PathBuf> {
    let cache_dir = dirs::cache_dir()
        .ok_or_else(|| anyhow::anyhow!("Failed to determine cache directory"))?
        .join("skim");

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

/// Generate cache key from file path, mtime, mode, max_lines, and token_budget.
fn cache_key(
    path: &Path,
    mtime: SystemTime,
    mode: Mode,
    max_lines: Option<usize>,
    token_budget: Option<usize>,
) -> Result<String> {
    let canonical_path = path.canonicalize()?;
    let mtime_secs = mtime.duration_since(SystemTime::UNIX_EPOCH)?.as_secs();

    let fmt_opt = |opt: Option<usize>| opt.map_or("none".to_string(), |n| n.to_string());
    let canonical_display = canonical_path.display();
    let max_lines_str = fmt_opt(max_lines);
    let token_budget_str = fmt_opt(token_budget);
    let hash_input =
        format!("{canonical_display}|{mtime_secs}|{mode:?}|{max_lines_str}|{token_budget_str}");

    let mut hasher = Sha256::new();
    hasher.update(hash_input.as_bytes());

    Ok(format!("{:x}", hasher.finalize()))
}

/// Read cached output if valid (mtime matches).
///
/// Returns a [`CacheHit`] on cache hit, `None` on miss.
pub fn read_cache(
    path: &Path,
    mode: Mode,
    max_lines: Option<usize>,
    token_budget: Option<usize>,
) -> Option<CacheHit> {
    let metadata = fs::metadata(path).ok()?;
    let mtime = metadata.modified().ok()?;

    let key = cache_key(path, mtime, mode, max_lines, token_budget).ok()?;
    let cache_file = get_cache_dir().ok()?.join(format!("{key}.json"));

    let cache_content = fs::read_to_string(&cache_file).ok()?;
    let entry: CacheEntry = serde_json::from_str(&cache_content).ok()?;

    // Belt-and-suspenders validation: verify mtime/mode match even though
    // they are already encoded in the cache key hash (guards against collisions).
    let mtime_secs = mtime.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
    let mode_str = format!("{mode:?}");

    if entry.mtime_secs == mtime_secs && entry.mode == mode_str {
        Some(CacheHit {
            content: entry.content,
            original_tokens: entry.original_tokens,
            transformed_tokens: entry.transformed_tokens,
        })
    } else {
        // Stale entry: best-effort cleanup.
        let _ = fs::remove_file(&cache_file);
        None
    }
}

/// Write transformed output to cache.
pub fn write_cache(params: &CacheWriteParams<'_>) -> Result<()> {
    let metadata = fs::metadata(params.path)?;
    let mtime = metadata.modified()?;

    let key = cache_key(
        params.path,
        mtime,
        params.mode,
        params.max_lines,
        params.token_budget,
    )?;
    let cache_file = get_cache_dir()?.join(format!("{key}.json"));

    let mtime_secs = mtime.duration_since(SystemTime::UNIX_EPOCH)?.as_secs();
    let mode = params.mode;
    let entry = CacheEntry {
        path: params.path.display().to_string(),
        mtime_secs,
        mode: format!("{mode:?}"),
        content: params.content.to_string(),
        original_tokens: params.original_tokens,
        transformed_tokens: params.transformed_tokens,
        effective_mode: params.effective_mode.map(|m| format!("{m:?}")),
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
pub fn clear_cache() -> Result<()> {
    let cache_dir = get_cache_dir()?;

    if cache_dir.exists() {
        for entry in fs::read_dir(&cache_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
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

    #[test]
    fn test_cache_key_generation() {
        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "test content").unwrap();
        let path = temp_file.path();

        let metadata = fs::metadata(path).unwrap();
        let mtime = metadata.modified().unwrap();

        // Same inputs should produce same key
        let key1 = cache_key(path, mtime, Mode::Structure, None, None).unwrap();
        let key2 = cache_key(path, mtime, Mode::Structure, None, None).unwrap();
        assert_eq!(key1, key2);

        // Different mode should produce different key
        let key3 = cache_key(path, mtime, Mode::Signatures, None, None).unwrap();
        assert_ne!(key1, key3);

        // Different max_lines should produce different key
        let key4 = cache_key(path, mtime, Mode::Structure, Some(50), None).unwrap();
        assert_ne!(key1, key4);

        // Same max_lines should produce same key
        let key5 = cache_key(path, mtime, Mode::Structure, Some(50), None).unwrap();
        assert_eq!(key4, key5);

        // Different token_budget should produce different key
        let key6 = cache_key(path, mtime, Mode::Structure, None, Some(500)).unwrap();
        assert_ne!(key1, key6);

        // Same token_budget should produce same key
        let key7 = cache_key(path, mtime, Mode::Structure, None, Some(500)).unwrap();
        assert_eq!(key6, key7);

        // Different max_lines + token_budget combination
        let key8 = cache_key(path, mtime, Mode::Structure, Some(50), Some(500)).unwrap();
        assert_ne!(key4, key8);
        assert_ne!(key6, key8);
    }

    #[test]
    fn test_cache_read_write() {
        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "test content").unwrap();
        let path = temp_file.path().to_path_buf();

        // Initially no cache
        assert!(read_cache(&path, Mode::Structure, None, None).is_none());

        // Write to cache with token counts
        let content = "transformed output";
        write_cache(&CacheWriteParams {
            path: &path,
            mode: Mode::Structure,
            content,
            original_tokens: Some(100),
            transformed_tokens: Some(50),
            max_lines: None,
            token_budget: None,
            effective_mode: None,
        })
        .unwrap();

        // Read from cache
        let hit = read_cache(&path, Mode::Structure, None, None).unwrap();
        assert_eq!(hit.content, content);
        assert_eq!(hit.original_tokens, Some(100));
        assert_eq!(hit.transformed_tokens, Some(50));

        // Different mode should not find cache
        assert!(read_cache(&path, Mode::Signatures, None, None).is_none());

        // Different max_lines should not find cache
        assert!(read_cache(&path, Mode::Structure, Some(50), None).is_none());

        // Different token_budget should not find cache
        assert!(read_cache(&path, Mode::Structure, None, Some(500)).is_none());
    }

    #[test]
    fn test_cache_read_write_with_token_budget() {
        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "test content for token budget").unwrap();
        let path = temp_file.path().to_path_buf();

        let token_budget = Some(500);

        // No cache initially
        assert!(read_cache(&path, Mode::Structure, None, token_budget).is_none());

        // Write with token_budget
        write_cache(&CacheWriteParams {
            path: &path,
            mode: Mode::Structure,
            content: "budget-transformed output",
            original_tokens: Some(200),
            transformed_tokens: Some(80),
            max_lines: None,
            token_budget,
            effective_mode: None,
        })
        .unwrap();

        // Read with same token_budget succeeds
        let hit = read_cache(&path, Mode::Structure, None, token_budget).unwrap();
        assert_eq!(hit.content, "budget-transformed output");
        assert_eq!(hit.original_tokens, Some(200));
        assert_eq!(hit.transformed_tokens, Some(80));

        // Read without token_budget misses (different cache key)
        assert!(read_cache(&path, Mode::Structure, None, None).is_none());

        // Read with different token_budget misses
        assert!(read_cache(&path, Mode::Structure, None, Some(1000)).is_none());

        // Read with same budget + different mode misses
        assert!(read_cache(&path, Mode::Signatures, None, token_budget).is_none());
    }

    #[test]
    fn test_cache_stores_effective_mode() {
        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "effective mode test content").unwrap();
        let path = temp_file.path().to_path_buf();

        // Write with effective_mode set (simulates cascade escalation)
        write_cache(&CacheWriteParams {
            path: &path,
            mode: Mode::Structure,
            content: "escalated output",
            original_tokens: Some(150),
            transformed_tokens: Some(60),
            max_lines: None,
            token_budget: Some(100),
            effective_mode: Some(Mode::Signatures),
        })
        .unwrap();

        // Read back succeeds (effective_mode is diagnostic-only, not part of CacheHit)
        let hit = read_cache(&path, Mode::Structure, None, Some(100)).unwrap();
        assert_eq!(hit.content, "escalated output");
        assert_eq!(hit.original_tokens, Some(150));
        assert_eq!(hit.transformed_tokens, Some(60));

        // Verify the effective_mode field was serialized in the raw JSON
        let metadata = fs::metadata(&path).unwrap();
        let mtime = metadata.modified().unwrap();
        let key = cache_key(&path, mtime, Mode::Structure, None, Some(100)).unwrap();
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

        // Write initial content
        {
            let mut file = File::create(&path).unwrap();
            file.write_all(b"original content").unwrap();
            file.flush().unwrap();
        }

        // Write to cache
        write_cache(&CacheWriteParams {
            path: &path,
            mode: Mode::Structure,
            content: "cached v1",
            original_tokens: None,
            transformed_tokens: None,
            max_lines: None,
            token_budget: None,
            effective_mode: None,
        })
        .unwrap();
        let hit = read_cache(&path, Mode::Structure, None, None).unwrap();
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
        assert!(read_cache(&path, Mode::Structure, None, None).is_none());
    }
}
