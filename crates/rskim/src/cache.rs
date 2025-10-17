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

/// Cache entry with metadata for validation
#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    /// Original file path (for debugging)
    path: String,
    /// File modification time (seconds since UNIX epoch)
    mtime_secs: u64,
    /// Transformation mode
    mode: String,
    /// Cached transformed output
    content: String,
}

/// Get platform-specific cache directory (~/.cache/skim/ on Linux/macOS)
fn get_cache_dir() -> Result<PathBuf> {
    let cache_dir = dirs::cache_dir()
        .ok_or_else(|| anyhow::anyhow!("Failed to determine cache directory"))?
        .join("skim");

    // Create cache directory if it doesn't exist
    fs::create_dir_all(&cache_dir)?;

    Ok(cache_dir)
}

/// Generate cache key from file path, mtime, and mode
fn cache_key(path: &Path, mtime: SystemTime, mode: Mode) -> Result<String> {
    // Get canonical path (resolves symlinks, relative paths)
    let canonical_path = path.canonicalize()?;

    // Convert mtime to seconds since UNIX epoch
    let mtime_secs = mtime
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_secs();

    // Create hash input: "path|mtime|mode"
    let mode_str = format!("{:?}", mode);
    let hash_input = format!(
        "{}|{}|{}",
        canonical_path.display(),
        mtime_secs,
        mode_str
    );

    // Generate SHA256 hash
    let mut hasher = Sha256::new();
    hasher.update(hash_input.as_bytes());
    let hash = hasher.finalize();

    // Convert to hex string
    Ok(format!("{:x}", hash))
}

/// Read cached output if valid (mtime matches)
pub fn read_cache(path: &Path, mode: Mode) -> Option<String> {
    // Get file metadata
    let metadata = fs::metadata(path).ok()?;
    let mtime = metadata.modified().ok()?;

    // Generate cache key
    let key = cache_key(path, mtime, mode).ok()?;
    let cache_dir = get_cache_dir().ok()?;
    let cache_file = cache_dir.join(format!("{}.json", key));

    // Check if cache file exists
    if !cache_file.exists() {
        return None;
    }

    // Read cache file
    let cache_content = fs::read_to_string(&cache_file).ok()?;
    let entry: CacheEntry = serde_json::from_str(&cache_content).ok()?;

    // Validate cache entry matches current file state
    let mtime_secs = mtime.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
    let mode_str = format!("{:?}", mode);

    if entry.mtime_secs == mtime_secs && entry.mode == mode_str {
        Some(entry.content)
    } else {
        // Cache is stale, delete it
        let _ = fs::remove_file(&cache_file);
        None
    }
}

/// Write transformed output to cache
pub fn write_cache(path: &Path, mode: Mode, content: &str) -> Result<()> {
    // Get file metadata
    let metadata = fs::metadata(path)?;
    let mtime = metadata.modified()?;

    // Generate cache key
    let key = cache_key(path, mtime, mode)?;
    let cache_dir = get_cache_dir()?;
    let cache_file = cache_dir.join(format!("{}.json", key));

    // Create cache entry
    let mtime_secs = mtime.duration_since(SystemTime::UNIX_EPOCH)?.as_secs();
    let entry = CacheEntry {
        path: path.display().to_string(),
        mtime_secs,
        mode: format!("{:?}", mode),
        content: content.to_string(),
    };

    // Write to cache file
    let json = serde_json::to_string(&entry)?;
    fs::write(&cache_file, json)?;

    Ok(())
}

/// Clear entire cache directory
pub fn clear_cache() -> Result<()> {
    let cache_dir = get_cache_dir()?;

    if cache_dir.exists() {
        fs::remove_dir_all(&cache_dir)?;
        fs::create_dir_all(&cache_dir)?;
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
        let key1 = cache_key(path, mtime, Mode::Structure).unwrap();
        let key2 = cache_key(path, mtime, Mode::Structure).unwrap();
        assert_eq!(key1, key2);

        // Different mode should produce different key
        let key3 = cache_key(path, mtime, Mode::Signatures).unwrap();
        assert_ne!(key1, key3);
    }

    #[test]
    fn test_cache_read_write() {
        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "test content").unwrap();
        let path = temp_file.path().to_path_buf();

        // Initially no cache
        assert!(read_cache(&path, Mode::Structure).is_none());

        // Write to cache
        let content = "transformed output";
        write_cache(&path, Mode::Structure, content).unwrap();

        // Read from cache
        let cached = read_cache(&path, Mode::Structure).unwrap();
        assert_eq!(cached, content);

        // Different mode should not find cache
        assert!(read_cache(&path, Mode::Signatures).is_none());
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
        write_cache(&path, Mode::Structure, "cached v1").unwrap();
        assert_eq!(
            read_cache(&path, Mode::Structure).unwrap(),
            "cached v1"
        );

        // Sleep to ensure mtime resolution (some filesystems have 1-second resolution)
        std::thread::sleep(std::time::Duration::from_secs(1));

        // Modify file (changes mtime)
        {
            let mut file = File::create(&path).unwrap();
            file.write_all(b"modified content").unwrap();
            file.flush().unwrap();
        }

        // Cache should be invalidated (mtime changed)
        assert!(read_cache(&path, Mode::Structure).is_none());
    }
}
