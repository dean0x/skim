//! Session ID sidecar: PID-keyed files for fallback session attribution.
//!
//! ## Problem
//!
//! ~50% of skim analytics records are "untagged" (NULL `session_id`) because
//! direct skim invocations (e.g., `skim cargo test`) bypass the rewrite hook
//! that normally injects `--session-id=<value>`.
//!
//! ## Solution (AD-SC-1)
//!
//! **Write path** — On every hook invocation that carries a `session_id`, the
//! hook writes the value to `~/.cache/skim/sessions/{ppid}.id` (keyed by the
//! agent/shell PID, i.e., the parent of the hook process).
//!
//! **Read path** — Any skim invocation that lacks `--session-id` walks its
//! process ancestry (up to [`MAX_ANCESTRY_DEPTH`] levels) looking for a
//! matching sidecar file. The first fresh file found wins.
//!
//! ## Security
//!
//! - Files are written with `0o600` permissions (owner-only).
//! - Content is validated through [`crate::analytics::is_safe_session_id`]
//!   before being accepted (alphanumeric + `-_.`, max 128 chars).
//! - All write and read failures are silently ignored — this is a best-effort
//!   mechanism that must never break the main skim pipeline.
//!
//! ## Performance
//!
//! Write path: ≤1 ms. Read path: ≤2 ms. Both are fire-and-forget / early-exit.

use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// Maximum age of a sidecar file before it is considered stale.
///
/// A sidecar older than 6 hours is skipped during ancestry walk. This covers
/// typical agent session lengths while preventing stale data from long-lived
/// shell processes.
const SIDECAR_MAX_AGE: Duration = Duration::from_secs(6 * 3600);

/// Maximum age before a sidecar file is removed during opportunistic cleanup.
const CLEANUP_MAX_AGE: Duration = Duration::from_secs(24 * 3600);

/// Subdirectory of the skim cache that holds sidecar files.
const SESSIONS_DIR: &str = "sessions";

/// Maximum number of ancestry levels to walk when searching for a sidecar.
///
/// Walking 5 levels covers agent → shell → hook → skim invocations in practice.
const MAX_ANCESTRY_DEPTH: usize = 5;

// ============================================================================
// Public API
// ============================================================================

/// Write `session_id` to a PID-keyed sidecar file for fallback attribution.
///
/// The file is keyed by **PPID** (the caller's parent process ID) so that
/// sibling processes spawned by the same agent parent can later discover the
/// session via the ancestry walk in [`read_session_id`].
///
/// All failures are silently ignored — this is a fire-and-forget operation.
/// Callers should validate `session_id` through
/// [`crate::analytics::is_safe_session_id`] before calling this function.
pub(crate) fn write_session_id(session_id: &str, cache_dir: &Path) {
    // Defense-in-depth: reject malformed IDs even though callers should
    // have validated already.
    if !crate::analytics::is_safe_session_id(session_id) {
        return;
    }

    let Some(ppid) = get_ppid() else { return };

    let dir = cache_dir.join(SESSIONS_DIR);
    let _ = std::fs::create_dir_all(&dir);

    let file_path = dir.join(format!("{ppid}.id"));

    // On Unix, open with O_CREAT|O_WRONLY|O_TRUNC and mode 0o600 in a single
    // syscall so the file is never briefly world-readable (eliminates the
    // TOCTOU window that exists with fs::write followed by set_permissions).
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&file_path)
        {
            let _ = f.write_all(session_id.as_bytes());
        }
    }

    // On non-Unix platforms get_ppid() always returns None, so this branch
    // is unreachable in practice. It exists to keep the code compiling on
    // Windows without dead-code warnings.
    #[cfg(not(unix))]
    {
        let _ = std::fs::write(&file_path, session_id);
    }

    // Opportunistic cleanup — best-effort, errors ignored.
    cleanup_stale_rate_limited(&dir);
}

/// Walk process ancestry to find a session ID sidecar.
///
/// Starts from the current process PID and walks up to
/// [`MAX_ANCESTRY_DEPTH`] levels looking for a file at
/// `{cache_dir}/sessions/{pid}.id`. Returns the first fresh, valid session ID
/// found, or `None` if no matching file exists.
///
/// "Fresh" means the file's mtime is within [`SIDECAR_MAX_AGE`].
/// "Valid" means the content passes [`crate::analytics::is_safe_session_id`].
pub(crate) fn read_session_id(cache_dir: &Path) -> Option<String> {
    let sessions_dir = cache_dir.join(SESSIONS_DIR);
    let mut pid = std::process::id();

    for _ in 0..MAX_ANCESTRY_DEPTH {
        let file_path = sessions_dir.join(format!("{pid}.id"));

        if let Some(value) = try_read_sidecar(&file_path) {
            return Some(value);
        }

        pid = parent_of(pid)?;
    }

    None
}

// ============================================================================
// Private helpers
// ============================================================================

/// Try to read a sidecar file at `path`.
///
/// Returns `Some(session_id)` if:
/// 1. The file exists.
/// 2. Its mtime is within [`SIDECAR_MAX_AGE`] (not stale).
/// 3. Its content passes [`crate::analytics::is_safe_session_id`].
///
/// Returns `None` in all other cases (missing file, stale, invalid content).
fn try_read_sidecar(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let mtime = metadata.modified().ok()?;
    let age = SystemTime::now()
        .duration_since(mtime)
        .unwrap_or(Duration::MAX);
    if age > SIDECAR_MAX_AGE {
        return None;
    }

    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    crate::analytics::is_safe_session_id(trimmed).then(|| trimmed.to_string())
}

/// Maximum interval between opportunistic cleanup runs.
///
/// Cleanup touches every file in the sessions directory — running it on every
/// hook write (potentially thousands per session) adds unbounded overhead.
/// This constant gates cleanup behind a sentinel file so it runs at most once
/// per hour regardless of write frequency.
const CLEANUP_RATE_LIMIT: Duration = Duration::from_secs(3600);

/// Sentinel file name written into `sessions_dir` after each cleanup run.
const CLEANUP_SENTINEL: &str = ".last_cleanup";

/// Run [`cleanup_stale`] only when the sentinel file is absent or older than
/// [`CLEANUP_RATE_LIMIT`].
///
/// Writes a fresh sentinel after each cleanup run. All errors are silently
/// ignored — this is best-effort.
fn cleanup_stale_rate_limited(sessions_dir: &Path) {
    let sentinel = sessions_dir.join(CLEANUP_SENTINEL);

    if let Ok(age) = std::fs::metadata(&sentinel)
        .and_then(|m| m.modified())
        .map(|mtime| {
            SystemTime::now()
                .duration_since(mtime)
                .unwrap_or(Duration::MAX)
        })
    {
        if age < CLEANUP_RATE_LIMIT {
            // Cleaned up recently — skip.
            return;
        }
    }

    cleanup_stale(sessions_dir);

    // Refresh sentinel (best-effort).
    let _ = std::fs::write(&sentinel, b"");
}

/// Remove sidecar files older than [`CLEANUP_MAX_AGE`].
///
/// Called via [`cleanup_stale_rate_limited`] on the write path. All errors are
/// silently ignored.
fn cleanup_stale(sessions_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(sessions_dir) else {
        return;
    };

    let now = SystemTime::now();

    for entry in entries.flatten() {
        // Skip the rate-limit sentinel itself.
        if entry.file_name() == CLEANUP_SENTINEL {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        let age = now.duration_since(mtime).unwrap_or(Duration::MAX);
        if age > CLEANUP_MAX_AGE {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Return the PPID of the current process on Unix platforms.
///
/// Returns `None` on non-Unix platforms or if the result is ≤ 0.
#[cfg(unix)]
fn get_ppid() -> Option<u32> {
    // SAFETY: getppid() is always safe to call — it has no preconditions and
    // always succeeds. The result is a valid non-negative PID on success.
    let ppid = unsafe { libc::getppid() };
    if ppid <= 0 {
        None
    } else {
        Some(ppid as u32)
    }
}

#[cfg(not(unix))]
fn get_ppid() -> Option<u32> {
    None
}

/// Return the parent PID of `pid` on Linux by reading `/proc/{pid}/stat`.
///
/// The stat format is `"pid (comm) state ppid ..."`. The `comm` field may
/// contain spaces and parentheses, so we find the last `)` to locate field
/// boundaries reliably.
#[cfg(target_os = "linux")]
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Find the closing paren of the comm field (last `)` is safest).
    let after_comm = stat.rfind(')')? + 1;
    // Remaining fields: " state ppid ..."
    let fields: Vec<&str> = stat[after_comm..].split_whitespace().collect();
    // fields[0] = state, fields[1] = ppid
    let ppid: u32 = fields.get(1)?.parse().ok()?;
    if ppid == 0 {
        None
    } else {
        Some(ppid)
    }
}

/// Return the parent PID of `pid` on macOS using `proc_pidinfo(PROC_PIDTASKALLINFO)`.
///
/// `proc_pidinfo` fills a `proc_taskallinfo` struct whose `.pbsd.pbi_ppid`
/// field holds the parent PID. This avoids the deprecated `sysctl` path and
/// uses the stable libproc API available on macOS 10.5+.
#[cfg(target_os = "macos")]
fn parent_of(pid: u32) -> Option<u32> {
    use std::mem;

    // SAFETY: `proc_taskallinfo` is a plain C struct; zero-initialising it is
    // valid. `proc_pidinfo` fills it in-place via the raw pointer. The buffer
    // size matches the struct size exactly, as required by the API.
    // Flavor PROC_PIDTASKALLINFO (2) pairs with the proc_taskallinfo struct.
    let mut info: libc::proc_taskallinfo = unsafe { mem::zeroed() };
    let size = mem::size_of::<libc::proc_taskallinfo>() as libc::c_int;

    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTASKALLINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };

    // A return value of 0 or negative signals an error. A return value that
    // is positive but less than the expected struct size indicates a short
    // read — the remaining bytes were never filled in and would contain
    // zeroes from the mem::zeroed() initialisation. Both cases must be
    // rejected to avoid returning a garbage PPID.
    if ret < size {
        return None;
    }

    let ppid = info.pbsd.pbi_ppid;
    if ppid == 0 {
        None
    } else {
        Some(ppid)
    }
}

/// Fallback for non-Linux, non-macOS Unix (e.g., FreeBSD, Windows).
///
/// Ancestry walk is not supported on these platforms; the read path returns
/// `None` immediately when this is reached.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn parent_of(_pid: u32) -> Option<u32> {
    None
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Write a raw sidecar file into `dir/sessions/{pid}.id` with the given
    /// content, bypassing `write_session_id` so tests can control the exact
    /// content and mtime.
    fn write_raw_sidecar(sessions_dir: &Path, pid: u32, content: &str) -> PathBuf {
        std::fs::create_dir_all(sessions_dir).unwrap();
        let path = sessions_dir.join(format!("{pid}.id"));
        std::fs::write(&path, content).unwrap();
        path
    }

    /// Set the mtime of a file to `SystemTime::now() - age` using the `filetime`
    /// crate (the standard portable approach for tests).
    fn set_file_age(path: &Path, age: Duration) {
        use filetime::{set_file_mtime, FileTime};
        let target_mtime = SystemTime::now()
            .checked_sub(age)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let ft = FileTime::from_system_time(target_mtime);
        set_file_mtime(path, ft).unwrap();
    }

    // -----------------------------------------------------------------------
    // write_session_id / read_session_id roundtrip
    // -----------------------------------------------------------------------

    /// AD-SC-1: Basic write→read roundtrip using the current process's own PID.
    ///
    /// `write_session_id` keys on PPID; we simulate that by writing directly to
    /// the sessions dir keyed to `std::process::id()` and reading back.
    #[test]
    fn test_write_read_roundtrip() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);

        // Write a sidecar keyed to our own PID so `read_session_id` depth-0 finds it.
        write_raw_sidecar(&sessions_dir, std::process::id(), "test-session-abc");

        let result = read_session_id(dir.path());
        assert_eq!(result, Some("test-session-abc".to_string()));
    }

    /// write_session_id creates the sessions directory if it does not exist.
    #[test]
    fn test_write_creates_sessions_dir() {
        let dir = TempDir::new().unwrap();
        // The sessions sub-directory does not exist yet.
        assert!(!dir.path().join(SESSIONS_DIR).exists());

        write_session_id("my-session", dir.path());

        assert!(dir.path().join(SESSIONS_DIR).exists());
    }

    /// A second write to the same sidecar overwrites the content and refreshes
    /// the mtime (staleness is reset).
    #[test]
    fn test_overwrite_updates_mtime() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);
        let Some(ppid) = get_ppid() else { return }; // non-Unix: skip

        // First write
        write_session_id("session-v1", dir.path());
        let path = sessions_dir.join(format!("{ppid}.id"));
        assert!(path.exists());

        // Age the file by 10 hours so it would be stale.
        set_file_age(&path, Duration::from_secs(10 * 3600));

        // Verify stale (sanity check)
        let meta = std::fs::metadata(&path).unwrap();
        let age = SystemTime::now()
            .duration_since(meta.modified().unwrap())
            .unwrap_or(Duration::MAX);
        assert!(age > SIDECAR_MAX_AGE, "should be stale before second write");

        // Second write should refresh mtime and update content.
        write_session_id("session-v2", dir.path());

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.trim(), "session-v2");

        let meta2 = std::fs::metadata(&path).unwrap();
        let age2 = SystemTime::now()
            .duration_since(meta2.modified().unwrap())
            .unwrap_or(Duration::MAX);
        assert!(
            age2 < SIDECAR_MAX_AGE,
            "mtime should be refreshed after second write"
        );
    }

    // -----------------------------------------------------------------------
    // read_session_id: negative cases
    // -----------------------------------------------------------------------

    /// Returns None when no matching sidecar file exists at any ancestry level.
    #[test]
    fn test_read_nonexistent() {
        let dir = TempDir::new().unwrap();
        let result = read_session_id(dir.path());
        assert_eq!(result, None);
    }

    /// Returns None for a sidecar that exceeds SIDECAR_MAX_AGE.
    #[test]
    fn test_read_stale_file() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);

        let path = write_raw_sidecar(&sessions_dir, std::process::id(), "stale-session");
        // Set mtime to 7 hours ago (past the 6h threshold).
        set_file_age(&path, Duration::from_secs(7 * 3600));

        let result = read_session_id(dir.path());
        assert_eq!(result, None, "stale sidecar should be ignored");
    }

    /// Returns None for content that fails is_safe_session_id (e.g., spaces).
    #[test]
    fn test_read_invalid_content() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);

        write_raw_sidecar(
            &sessions_dir,
            std::process::id(),
            "bad session id with spaces!",
        );

        let result = read_session_id(dir.path());
        assert_eq!(result, None, "invalid content should be rejected");
    }

    // -----------------------------------------------------------------------
    // Ancestry walk
    // -----------------------------------------------------------------------

    /// Depth-1 walk: sidecar keyed to PPID is found when there is no depth-0 sidecar.
    #[cfg(unix)]
    #[test]
    fn test_read_walks_ancestry() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let ppid = get_ppid().expect("must have ppid on unix");

        // Write only at the parent's PID — no sidecar for the current process.
        write_raw_sidecar(&sessions_dir, ppid, "parent-session");

        let result = read_session_id(dir.path());
        assert_eq!(result, Some("parent-session".to_string()));
    }

    // -----------------------------------------------------------------------
    // Isolation: different PID keys don't interfere
    // -----------------------------------------------------------------------

    #[test]
    fn test_concurrent_pids_isolated() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);

        // Write two different PIDs with different session IDs.
        let pid_a: u32 = 11111;
        let pid_b: u32 = 22222;

        write_raw_sidecar(&sessions_dir, pid_a, "session-for-a");
        write_raw_sidecar(&sessions_dir, pid_b, "session-for-b");

        // Read back each directly.
        let val_a = try_read_sidecar(&sessions_dir.join(format!("{pid_a}.id")));
        let val_b = try_read_sidecar(&sessions_dir.join(format!("{pid_b}.id")));

        assert_eq!(val_a, Some("session-for-a".to_string()));
        assert_eq!(val_b, Some("session-for-b".to_string()));
    }

    // -----------------------------------------------------------------------
    // cleanup_stale
    // -----------------------------------------------------------------------

    /// Files older than CLEANUP_MAX_AGE are removed when a new sidecar is written.
    ///
    /// Requires Unix because `write_session_id` calls `get_ppid()` which returns
    /// `None` (and returns early) on non-Unix platforms, so no cleanup is ever
    /// triggered there.
    #[cfg(unix)]
    #[test]
    fn test_cleanup_removes_old_files() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);

        // Plant a stale file (25 hours old — over the 24h cleanup threshold).
        let stale_pid: u32 = 99999;
        let stale_path = write_raw_sidecar(&sessions_dir, stale_pid, "old-session");
        set_file_age(&stale_path, Duration::from_secs(25 * 3600));

        assert!(
            stale_path.exists(),
            "stale file should exist before trigger"
        );

        // Trigger cleanup by writing a new sidecar (cleanup runs on every write).
        write_session_id("new-session", dir.path());

        assert!(
            !stale_path.exists(),
            "stale file should have been cleaned up"
        );
    }

    // -----------------------------------------------------------------------
    // Platform: parent_of / get_ppid
    // -----------------------------------------------------------------------

    /// parent_of(current_pid) returns a valid PID on Linux/macOS.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn test_parent_of_current_process() {
        let current_pid = std::process::id();
        let ppid = parent_of(current_pid);
        assert!(
            ppid.is_some(),
            "parent_of(current_pid) must return Some on Linux/macOS"
        );
        assert!(ppid.unwrap() > 0, "ppid must be > 0");
    }

    // -----------------------------------------------------------------------
    // Fallback priority: explicit session_id wins over sidecar
    // -----------------------------------------------------------------------

    /// When an explicit session ID is already resolved, `read_session_id` is the
    /// fallback — it is only used when the caller has `None`. This test confirms
    /// that a sidecar written for the current PID is readable, and that a
    /// pre-existing `Some` value is unaffected by what the sidecar contains.
    #[test]
    fn test_read_session_id_is_a_fallback() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);
        write_raw_sidecar(&sessions_dir, std::process::id(), "sidecar-value");

        // When no explicit session_id is present, the sidecar is used.
        let from_sidecar = None::<String>.or_else(|| read_session_id(dir.path()));
        assert_eq!(from_sidecar, Some("sidecar-value".to_string()));

        // When an explicit session_id is already present, the sidecar is not used.
        let explicit = Some("explicit-value".to_string());
        let resolved = explicit.or_else(|| read_session_id(dir.path()));
        assert_eq!(resolved, Some("explicit-value".to_string()));
    }

    // -----------------------------------------------------------------------
    // write_session_id: validation guard (issue write-validate)
    // -----------------------------------------------------------------------

    /// write_session_id must silently reject invalid session IDs and not create
    /// any file. This is the defense-in-depth guard added to the function itself
    /// regardless of whether the caller already validated.
    #[cfg(unix)]
    #[test]
    fn test_write_rejects_invalid_session_id() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);

        // A session ID with spaces fails is_safe_session_id.
        write_session_id("bad session id!", dir.path());

        // No sidecar file should have been created.
        let Some(ppid) = get_ppid() else { return };
        let sidecar = sessions_dir.join(format!("{ppid}.id"));
        assert!(
            !sidecar.exists(),
            "write_session_id must not create a file for an invalid session ID"
        );
    }

    // -----------------------------------------------------------------------
    // cleanup_stale_rate_limited (issue cleanup-hot-path)
    // -----------------------------------------------------------------------

    /// cleanup_stale_rate_limited must skip the full directory scan when the
    /// sentinel file is fresh (written within CLEANUP_RATE_LIMIT).
    #[test]
    fn test_cleanup_rate_limited_skips_when_sentinel_fresh() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);
        std::fs::create_dir_all(&sessions_dir).unwrap();

        // Plant a stale sidecar that would be removed if cleanup ran.
        let stale_pid: u32 = 88888;
        let stale_path = write_raw_sidecar(&sessions_dir, stale_pid, "old-session");
        set_file_age(&stale_path, Duration::from_secs(25 * 3600));

        // Write a fresh sentinel so cleanup is skipped.
        let sentinel = sessions_dir.join(CLEANUP_SENTINEL);
        std::fs::write(&sentinel, b"").unwrap();
        // Sentinel is brand new — within CLEANUP_RATE_LIMIT.

        cleanup_stale_rate_limited(&sessions_dir);

        assert!(
            stale_path.exists(),
            "stale file must NOT be removed when cleanup is rate-limited by a fresh sentinel"
        );
    }

    /// cleanup_stale_rate_limited must run cleanup when the sentinel is older
    /// than CLEANUP_RATE_LIMIT.
    #[test]
    fn test_cleanup_rate_limited_runs_when_sentinel_old() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);
        std::fs::create_dir_all(&sessions_dir).unwrap();

        // Plant a stale sidecar.
        let stale_pid: u32 = 77777;
        let stale_path = write_raw_sidecar(&sessions_dir, stale_pid, "old-session");
        set_file_age(&stale_path, Duration::from_secs(25 * 3600));

        // Write an old sentinel (2 hours old — past the 1-hour rate limit).
        let sentinel = sessions_dir.join(CLEANUP_SENTINEL);
        std::fs::write(&sentinel, b"").unwrap();
        set_file_age(&sentinel, CLEANUP_RATE_LIMIT + Duration::from_secs(1));

        cleanup_stale_rate_limited(&sessions_dir);

        assert!(
            !stale_path.exists(),
            "stale file must be removed when sentinel is older than CLEANUP_RATE_LIMIT"
        );
    }
}
