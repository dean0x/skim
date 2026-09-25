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
    acquire_bounded(
        caller,
        cache_dir,
        Duration::from_millis(LOCK_POLL_MS),
        Duration::from_secs(LOCK_DEADLINE_SECS),
    )
}

/// Inner bounded implementation — extracted for testability.
///
/// `poll` is the sleep interval between `try_lock` attempts. `deadline_after`
/// is the maximum total wait time before returning `Err`. The deadline error
/// message reports `deadline_after` (not a hardcoded constant), so sub-second
/// test deadlines produce truthful messages.
fn acquire_bounded(
    caller: &str,
    cache_dir: &Path,
    poll: Duration,
    deadline_after: Duration,
) -> anyhow::Result<std::fs::File> {
    use anyhow::Context as _;

    let lock_path = cache_dir.join(".skim-build.lock");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("failed to open build lock: {}", lock_path.display()))?;

    let deadline = Instant::now() + deadline_after;
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
                        "another skim build has held {} for >{:?}; \
                         if no build is running, delete the lock file and retry",
                        lock_path.display(),
                        deadline_after,
                    ));
                }
                std::thread::sleep(poll);
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(anyhow::anyhow!(e))
                    .with_context(|| "failed to acquire exclusive build lock");
            }
        }
    }
    Ok(lock_file)
}

#[cfg(test)]
#[path = "build_lock_tests.rs"]
mod tests;
