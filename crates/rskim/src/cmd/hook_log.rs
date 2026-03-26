//! Log file for hook-mode diagnostics (#57).
//!
//! CRITICAL DESIGN CONSTRAINT: Hook-mode warnings MUST go to a log file,
//! NEVER to stderr. Claude Code treats stderr+exit(0) as an error
//! (GRANITE #361 Bug 3). This module provides a file-based logging path
//! that is safe for hook execution context.
//!
//! Log location: `~/.cache/skim/hook.log`
//! Rotation: 1 MB max, 3 archived copies (`.1`, `.2`, `.3`)

use std::io::Write;
use std::path::Path;

/// Maximum log file size before rotation (1 MB).
const MAX_LOG_SIZE: u64 = 1024 * 1024;

/// Maximum number of archive files to keep.
const MAX_ARCHIVES: u32 = 3;

/// Log a warning to `~/.cache/skim/hook.log` with rotation.
///
/// NEVER outputs to stderr -- safe for use in hook execution context.
/// All failures are silently ignored to never break the hook.
pub(crate) fn log_hook_warning(message: &str) {
    let log_path = match cache_dir() {
        Some(dir) => dir.join("hook.log"),
        None => return,
    };

    // Ensure cache directory exists
    let _ = std::fs::create_dir_all(log_path.parent().unwrap_or(Path::new(".")));

    // Rotate if needed before appending
    rotate_if_needed(&log_path);

    // Append the warning with timestamp
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        let timestamp = timestamp_string();
        let _ = writeln!(file, "[{timestamp}] {message}");
    }
}

/// Rotate log file if it exceeds [`MAX_LOG_SIZE`].
///
/// Shift scheme: delete `.3`, rename `.2` -> `.3`, `.1` -> `.2`, current -> `.1`.
fn rotate_if_needed(log_path: &Path) {
    let size = std::fs::metadata(log_path).map(|m| m.len()).unwrap_or(0);
    if size < MAX_LOG_SIZE {
        return;
    }

    // Shift archives: .3 is deleted, .2 -> .3, .1 -> .2
    for i in (1..MAX_ARCHIVES).rev() {
        let from = archive_path(log_path, i);
        let to = archive_path(log_path, i + 1);
        let _ = std::fs::rename(&from, &to);
    }

    // Current -> .1
    let archive_1 = archive_path(log_path, 1);
    let _ = std::fs::rename(log_path, &archive_1);
}

/// Build the path for an archive file (e.g., `hook.log.1`, `hook.log.2`).
fn archive_path(log_path: &Path, index: u32) -> std::path::PathBuf {
    let mut path = log_path.as_os_str().to_owned();
    path.push(format!(".{index}"));
    std::path::PathBuf::from(path)
}

/// Get the skim cache directory, respecting `$SKIM_CACHE_DIR` override and
/// platform conventions.
///
/// Priority: `SKIM_CACHE_DIR` env > `dirs::cache_dir()/skim`.
/// The env override enables test isolation on all platforms (especially macOS
/// where `dirs::cache_dir()` ignores `$XDG_CACHE_HOME`).
pub(super) fn cache_dir() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("SKIM_CACHE_DIR") {
        return Some(std::path::PathBuf::from(dir));
    }
    dirs::cache_dir().map(|c| c.join("skim"))
}

/// Generate a timestamp string in ISO-8601 format (UTC approximation).
///
/// Uses `days_to_date` (Howard Hinnant calendar algorithm) to avoid
/// pulling in chrono. Includes hour:minute:second for log granularity.
fn timestamp_string() -> String {
    let now = std::time::SystemTime::now();
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86400;
    let day_secs = secs % 86400;
    let (year, month, day) = days_to_date(days);
    let hour = day_secs / 3600;
    let minute = (day_secs % 3600) / 60;
    let second = day_secs % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert days since Unix epoch to (year, month, day).
/// Algorithm from http://howardhinnant.github.io/date_algorithms.html
pub(super) fn days_to_date(days_since_epoch: u64) -> (u64, u64, u64) {
    let z = days_since_epoch + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_rotation_at_1mb() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("hook.log");

        // Create a log file just over 1 MB
        let content = "x".repeat(MAX_LOG_SIZE as usize + 100);
        std::fs::write(&log_path, &content).unwrap();

        // Trigger rotation
        rotate_if_needed(&log_path);

        // Original should be gone, archive .1 should exist
        assert!(
            !log_path.exists(),
            "Original log should be renamed during rotation"
        );
        let archive1 = archive_path(&log_path, 1);
        assert!(archive1.exists(), "Archive .1 should exist after rotation");

        // Verify archive content matches original
        let archived_content = std::fs::read_to_string(&archive1).unwrap();
        assert_eq!(archived_content, content);
    }

    #[test]
    fn test_rotation_shifts_existing_archives() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("hook.log");

        // Create existing archives
        std::fs::write(archive_path(&log_path, 1), "archive 1 content").unwrap();
        std::fs::write(archive_path(&log_path, 2), "archive 2 content").unwrap();

        // Create an oversized current log
        let big_content = "y".repeat(MAX_LOG_SIZE as usize + 1);
        std::fs::write(&log_path, &big_content).unwrap();

        rotate_if_needed(&log_path);

        // .1 should now contain old current log
        let a1 = std::fs::read_to_string(archive_path(&log_path, 1)).unwrap();
        assert_eq!(a1, big_content);

        // .2 should contain old .1
        let a2 = std::fs::read_to_string(archive_path(&log_path, 2)).unwrap();
        assert_eq!(a2, "archive 1 content");

        // .3 should contain old .2
        let a3 = std::fs::read_to_string(archive_path(&log_path, 3)).unwrap();
        assert_eq!(a3, "archive 2 content");
    }

    #[test]
    fn test_rotation_not_triggered_under_limit() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("hook.log");

        // Create a small log file
        std::fs::write(&log_path, "small log entry\n").unwrap();

        rotate_if_needed(&log_path);

        // File should still exist unchanged
        assert!(log_path.exists(), "Small log should not be rotated");
        assert!(
            !archive_path(&log_path, 1).exists(),
            "No archive should be created"
        );
    }

    #[test]
    fn test_rotation_missing_file_is_noop() {
        let dir = tempfile::TempDir::new().unwrap();
        let log_path = dir.path().join("nonexistent.log");

        // Should not panic or error
        rotate_if_needed(&log_path);

        assert!(!log_path.exists());
    }

    #[test]
    fn test_timestamp_string_format() {
        let ts = timestamp_string();
        // Should match ISO-8601 pattern: YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(ts.len(), 20, "Timestamp should be 20 chars: {ts}");
        assert!(ts.ends_with('Z'), "Timestamp should end with Z: {ts}");
        assert_eq!(&ts[4..5], "-", "Dash after year: {ts}");
        assert_eq!(&ts[7..8], "-", "Dash after month: {ts}");
        assert_eq!(&ts[10..11], "T", "T separator: {ts}");
        assert_eq!(&ts[13..14], ":", "Colon after hour: {ts}");
        assert_eq!(&ts[16..17], ":", "Colon after minute: {ts}");
    }

    #[test]
    fn test_archive_path_format() {
        let log = std::path::PathBuf::from("/tmp/hook.log");
        assert_eq!(
            archive_path(&log, 1),
            std::path::PathBuf::from("/tmp/hook.log.1")
        );
        assert_eq!(
            archive_path(&log, 3),
            std::path::PathBuf::from("/tmp/hook.log.3")
        );
    }

    #[test]
    fn test_log_hook_warning_triggers_rotation() {
        // End-to-end: call log_hook_warning with a >1MB log file already in place.
        // Verifies that log_hook_warning rotates the existing file to .1 and
        // creates a fresh hook.log with the new message.
        let dir = tempfile::TempDir::new().unwrap();
        let cache = dir.path().join("skim-cache");
        std::fs::create_dir_all(&cache).unwrap();

        // Pre-fill hook.log just over the rotation threshold
        let log_path = cache.join("hook.log");
        let big_content = "z".repeat(MAX_LOG_SIZE as usize + 100);
        std::fs::write(&log_path, &big_content).unwrap();

        // Override SKIM_CACHE_DIR so log_hook_warning writes to our temp dir
        std::env::set_var("SKIM_CACHE_DIR", &cache);
        log_hook_warning("rotation integration test");
        std::env::remove_var("SKIM_CACHE_DIR");

        // The old oversized log should be archived to .1
        let archive1 = archive_path(&log_path, 1);
        assert!(
            archive1.exists(),
            "Archive .1 should exist after rotation triggered by log_hook_warning"
        );
        let archived = std::fs::read_to_string(&archive1).unwrap();
        assert_eq!(
            archived, big_content,
            "Archive .1 should contain the original oversized content"
        );

        // The new hook.log should contain the freshly written message
        assert!(log_path.exists(), "hook.log should be recreated after rotation");
        let new_content = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            new_content.contains("rotation integration test"),
            "New hook.log should contain the warning message, got: {new_content}"
        );
    }
}
