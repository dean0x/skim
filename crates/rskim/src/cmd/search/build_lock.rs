//! Shared advisory build lock for `skim search` index builds.
//!
//! # Responsibility
//!
//! This module owns ONE bounded lock-acquisition implementation used by both
//! `index.rs` (lexical/AST build) and `temporal_build.rs` (temporal rebuild).
//! Neither module implements its own lock loop; both delegate here so that the
//! poll interval, deadline, and waiting message stay in sync.
//!
//! # Usage
//!
//! ```rust,ignore
//! let _lock = build_lock::acquire("skim search index", &pipeline.cache_dir)?;
//! ```
//!
//! The returned [`std::fs::File`] holds the exclusive advisory lock. The lock
//! is released when the file is dropped (at function end or on early return).

use std::path::Path;
use std::time::{Duration, Instant};

/// Lock polling interval (milliseconds).
const LOCK_POLL_MS: u64 = 200;

/// Lock acquisition deadline (seconds).
const LOCK_DEADLINE_SECS: u64 = 120;

/// Acquire the shared advisory build lock at `{cache_dir}/.skim-build.lock`.
///
/// `caller` is a short prefix used in the waiting notice emitted to stderr
/// (e.g. `"skim search index"` or `"skim search"`). The notice is printed at
/// most once per acquisition attempt.
///
/// # Returns
///
/// An open [`std::fs::File`] holding the exclusive advisory lock. The lock is
/// released when the file is dropped.
///
/// # Errors
///
/// Returns `Err` when the lock file cannot be opened or the 120-second deadline
/// expires without acquiring it.
pub(super) fn acquire(caller: &str, cache_dir: &Path) -> anyhow::Result<std::fs::File> {
    use anyhow::Context as _;

    let lock_path = cache_dir.join(".skim-build.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("failed to open build lock: {}", lock_path.display()))?;

    let deadline = Instant::now() + Duration::from_secs(LOCK_DEADLINE_SECS);
    let mut noticed = false;
    loop {
        match lock_file.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) => {
                if !noticed {
                    eprintln!(
                        "{caller}: waiting for concurrent build to finish \
                         (lock: {}) …",
                        lock_path.display()
                    );
                    noticed = true;
                }
                if Instant::now() >= deadline {
                    return Err(anyhow::anyhow!(
                        "another skim build has held {} for >{} s; \
                         if no build is running, delete the lock file and retry",
                        lock_path.display(),
                        LOCK_DEADLINE_SECS,
                    ));
                }
                std::thread::sleep(Duration::from_millis(LOCK_POLL_MS));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(anyhow::anyhow!(e))
                    .with_context(|| "failed to acquire exclusive build lock");
            }
        }
    }
    Ok(lock_file)
}
