//! Tee/recovery: save raw output on failure for post-hoc inspection (#52)
//!
//! When a command fails, the raw output is saved to a tee directory for recovery.
//! Files are rotated to prevent unbounded growth. A hint is emitted to stderr
//! so the user knows where to find the saved output.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;

static TEE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Configuration for tee behavior
pub(crate) struct TeeConfig {
    /// Maximum number of tee files to keep
    pub(crate) max_files: usize,
    /// Maximum file size in bytes (files larger than this are truncated)
    pub(crate) max_file_size: usize,
    /// Minimum output length to trigger a tee save (chars)
    pub(crate) min_output_len: usize,
}

impl Default for TeeConfig {
    fn default() -> Self {
        Self {
            max_files: 20,
            max_file_size: 1_048_576, // 1MB
            min_output_len: 500,
        }
    }
}

/// Returns the tee directory path (`~/.cache/skim/tee/`), creating it with
/// owner-only permissions if it does not yet exist.
pub(crate) fn get_tee_dir() -> Result<PathBuf> {
    let tee_dir = crate::cache::get_cache_dir()?.join("tee");

    #[cfg(unix)]
    {
        use std::fs::DirBuilder;
        use std::os::unix::fs::DirBuilderExt;

        let mut builder = DirBuilder::new();
        builder.mode(0o700); // rwx------
        builder.recursive(true);
        builder.create(&tee_dir)?;
    }

    #[cfg(not(unix))]
    {
        fs::create_dir_all(&tee_dir)?;
    }

    Ok(tee_dir)
}

/// Save raw output to a tee file in the specified directory.
///
/// Returns `Ok(None)` if the output is too short to save (below `min_output_len`).
/// Returns `Ok(Some(path))` on success with the path to the saved file.
///
/// This is the DI-friendly version; production callers use [`save_tee`] which
/// delegates to `get_tee_dir()` automatically.
fn save_tee_to_dir(
    raw_output: &str,
    config: &TeeConfig,
    tee_dir: &Path,
) -> Result<Option<PathBuf>> {
    if raw_output.len() < config.min_output_len {
        return Ok(None);
    }

    let epoch_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let pid = std::process::id();
    let seq = TEE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let filename = format!("{epoch_secs}_{pid}_{seq}.txt");
    let file_path = tee_dir.join(filename);

    // Truncate if exceeds max_file_size (ensure we don't split a UTF-8 char)
    let content = if raw_output.len() > config.max_file_size {
        let mut boundary = config.max_file_size;
        while boundary > 0 && !raw_output.is_char_boundary(boundary) {
            boundary -= 1;
        }
        &raw_output[..boundary]
    } else {
        raw_output
    };

    fs::write(&file_path, content)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&file_path, fs::Permissions::from_mode(0o600));
    }

    Ok(Some(file_path))
}

/// Save raw output to a tee file for recovery (uses default tee directory).
///
/// Returns `Ok(None)` if the output is too short to save (below `min_output_len`).
/// Returns `Ok(Some(path))` on success with the path to the saved file.
pub(crate) fn save_tee(raw_output: &str, config: &TeeConfig) -> Result<Option<PathBuf>> {
    let tee_dir = get_tee_dir()?;
    save_tee_to_dir(raw_output, config, &tee_dir)
}

/// Emit a hint to stderr indicating where the raw output was saved.
pub(crate) fn emit_tee_hint(path: &Path, writer: &mut impl Write) -> io::Result<()> {
    writeln!(writer, "[skim:tee] Raw output saved to {}", path.display())
}

/// Rotate tee files to stay within `max_files` limit.
///
/// Lists `.txt` files in the tee directory, sorts by name (epoch-based naming
/// ensures chronological order), and deletes the oldest files until the count
/// is at or below `max_files`.
pub(crate) fn rotate_tee_files(tee_dir: &Path, max_files: usize) -> Result<()> {
    let mut files: Vec<PathBuf> = fs::read_dir(tee_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "txt") && path.is_file())
        .collect();

    if files.len() <= max_files {
        return Ok(());
    }

    // Sort by filename (epoch-based = chronological)
    files.sort();

    let to_remove = files.len() - max_files;
    for path in files.iter().take(to_remove) {
        if let Err(e) = fs::remove_file(path) {
            eprintln!("[skim:tee] failed to remove {}: {e}", path.display());
        }
    }

    Ok(())
}

/// Save raw output on failure, rotate old files, and emit a hint to stderr.
///
/// Only tees if exit_code is non-zero (or unknown/signal-killed).
/// `exit_code == Some(0)` and `exit_code == None` (no process ran) skip tee.
pub(crate) fn tee_on_failure(
    raw: &str,
    exit_code: Option<i32>,
    config: &TeeConfig,
    writer: &mut impl Write,
) -> Result<()> {
    // Only tee on actual failure (non-zero exit code)
    match exit_code {
        Some(0) | None => return Ok(()),
        Some(_) => {}
    }

    if let Some(path) = save_tee(raw, config)? {
        let tee_dir = path.parent().unwrap_or(Path::new("."));
        rotate_tee_files(tee_dir, config.max_files)?;
        emit_tee_hint(&path, writer)?;
    }

    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_save_tee_below_min_length() {
        let dir = TempDir::new().unwrap();
        let config = TeeConfig {
            min_output_len: 100,
            ..TeeConfig::default()
        };
        let result = save_tee_to_dir("short", &config, dir.path()).unwrap();
        assert!(
            result.is_none(),
            "Output below min_output_len should not be saved"
        );
    }

    #[test]
    fn test_save_tee_creates_file() {
        let dir = TempDir::new().unwrap();
        let config = TeeConfig {
            min_output_len: 5,
            ..TeeConfig::default()
        };
        let content = "x".repeat(100);
        let result = save_tee_to_dir(&content, &config, dir.path()).unwrap();
        assert!(
            result.is_some(),
            "Output above min_output_len should be saved"
        );
        let path = result.unwrap();
        assert!(path.exists());
        let saved = fs::read_to_string(&path).unwrap();
        assert_eq!(saved, content);
    }

    #[test]
    fn test_save_tee_truncates_large_output() {
        let dir = TempDir::new().unwrap();
        let config = TeeConfig {
            min_output_len: 5,
            max_file_size: 50,
            ..TeeConfig::default()
        };
        let content = "x".repeat(200);
        let result = save_tee_to_dir(&content, &config, dir.path()).unwrap();
        let path = result.unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert_eq!(
            saved.len(),
            50,
            "Saved content should be truncated to max_file_size"
        );
    }

    #[test]
    fn test_rotate_tee_files_removes_oldest() {
        let dir = TempDir::new().unwrap();

        // Create 5 files
        for i in 0..5 {
            let path = dir.path().join(format!("{i:010}_1.txt"));
            fs::write(&path, "content").unwrap();
        }

        // Rotate to keep 3
        rotate_tee_files(dir.path(), 3).unwrap();

        let remaining: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "txt"))
            .collect();
        assert_eq!(
            remaining.len(),
            3,
            "Should keep only 3 files after rotation"
        );
    }

    #[test]
    fn test_rotate_tee_files_noop_when_under_limit() {
        let dir = TempDir::new().unwrap();

        for i in 0..2 {
            let path = dir.path().join(format!("{i}_1.txt"));
            fs::write(&path, "content").unwrap();
        }

        rotate_tee_files(dir.path(), 10).unwrap();

        let remaining: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(
            remaining.len(),
            2,
            "No files should be removed when under limit"
        );
    }

    #[test]
    fn test_emit_tee_hint_format() {
        let path = Path::new("/tmp/skim/tee/12345_678.txt");
        let mut buf = Vec::new();
        emit_tee_hint(path, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("[skim:tee]"),
            "expected [skim:tee] prefix, got: {output}"
        );
        assert!(
            output.contains("/tmp/skim/tee/12345_678.txt"),
            "expected path in output, got: {output}"
        );
    }

    #[test]
    fn test_tee_on_failure_skips_success() {
        let config = TeeConfig {
            min_output_len: 5,
            ..TeeConfig::default()
        };
        let mut buf = Vec::new();
        tee_on_failure("some long output content here", Some(0), &config, &mut buf).unwrap();
        assert!(buf.is_empty(), "Should not tee on exit code 0");
    }

    #[test]
    fn test_tee_on_failure_skips_none() {
        let config = TeeConfig {
            min_output_len: 5,
            ..TeeConfig::default()
        };
        let mut buf = Vec::new();
        tee_on_failure("some long output content here", None, &config, &mut buf).unwrap();
        assert!(buf.is_empty(), "Should not tee on None exit code");
    }

    #[test]
    fn test_tee_on_failure_saves_on_nonzero() {
        let config = TeeConfig {
            min_output_len: 5,
            ..TeeConfig::default()
        };
        let content = "x".repeat(600);
        let mut buf = Vec::new();
        tee_on_failure(&content, Some(1), &config, &mut buf).unwrap();
        let hint = String::from_utf8(buf).unwrap();
        assert!(
            hint.contains("[skim:tee]"),
            "Should emit tee hint on failure, got: {hint}"
        );
    }
}
