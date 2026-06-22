//! Unit tests for the build_lock module.
//!
//! Each test creates its own `tempfile::tempdir()` so tests are fully
//! parallel-safe without `#[serial]`.
//!
//! Cross-platform note: two DISTINCT `File` handles on the same path
//! produce separate lock objects (Unix flock is per-open-fd; Windows
//! LockFileEx is per-handle).  All tests that need a second contender
//! open a second `File` rather than re-using the first.

#![allow(clippy::unwrap_used)]

use std::time::{Duration, Instant};

use super::acquire_bounded;
// `acquire` is tested indirectly: e2e tests in index_tests.rs / temporal_build_tests.rs
// call super::super::build_lock::acquire.  Import it here to confirm it compiles.
#[allow(unused_imports)]
use super::acquire;

// ──────────────────────────────────────────────────────────────────────────────
// T-B1  acquire_blocks_while_held
// ──────────────────────────────────────────────────────────────────────────────

/// While the first handle holds the lock a second `acquire_bounded` call
/// returns Err (WouldBlock path → deadline expiry).
#[test]
fn acquire_blocks_while_held() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path();

    let h1 = acquire_bounded(
        "holder",
        cache,
        Duration::from_millis(5),
        Duration::from_millis(50),
    )
    .unwrap();

    // Second attempt on a *distinct* File handle — must hit WouldBlock and time out.
    let result = acquire_bounded(
        "waiter",
        cache,
        Duration::from_millis(5),
        Duration::from_millis(50),
    );

    assert!(
        result.is_err(),
        "second acquire must fail while h1 holds the lock"
    );

    drop(h1);
}

// ──────────────────────────────────────────────────────────────────────────────
// T-B2  release_on_drop_allows_reacquire
// ──────────────────────────────────────────────────────────────────────────────

/// After the holder drops the file the lock is free; a subsequent call
/// must succeed.
#[test]
fn release_on_drop_allows_reacquire() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path();

    {
        let _h = acquire_bounded(
            "first",
            cache,
            Duration::from_millis(5),
            Duration::from_millis(50),
        )
        .unwrap();
        // _h drops here, releasing the lock.
    }

    let result = acquire_bounded(
        "second",
        cache,
        Duration::from_millis(5),
        Duration::from_millis(50),
    );
    assert!(
        result.is_ok(),
        "reacquire after drop must succeed; got: {result:?}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// T-B3  retry_then_succeed_after_holder_releases
// ──────────────────────────────────────────────────────────────────────────────

/// A helper thread holds the lock ~30 ms then drops it; the main thread's
/// `acquire_bounded` retries (WouldBlock) then succeeds (Ok).
#[test]
fn retry_then_succeed_after_holder_releases() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_path = tmp.path().to_path_buf();

    // Spawn a thread that holds the lock for ~30 ms.
    let cache_for_thread = cache_path.clone();
    let handle = std::thread::spawn(move || {
        let _h = acquire_bounded(
            "thread-holder",
            &cache_for_thread,
            Duration::from_millis(5),
            Duration::from_millis(500),
        )
        .expect("thread-holder must acquire");
        std::thread::sleep(Duration::from_millis(30));
        // _h drops here, releasing the lock.
    });

    // Give the thread a head-start so we hit WouldBlock on the first try.
    std::thread::sleep(Duration::from_millis(5));

    // Main thread waits up to 1 s — long enough for the thread to release.
    let result = acquire_bounded(
        "main",
        &cache_path,
        Duration::from_millis(5),
        Duration::from_secs(1),
    );

    handle.join().expect("helper thread panicked");

    assert!(
        result.is_ok(),
        "main must acquire after thread releases; got: {result:?}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// T-B4  open_error_on_nonexistent_dir
// ──────────────────────────────────────────────────────────────────────────────

/// When the cache directory does not exist the open fails; error message
/// must contain "failed to open build lock".
#[test]
fn open_error_on_nonexistent_dir() {
    let tmp = tempfile::tempdir().unwrap();
    // A path whose parent does not exist.
    let missing = tmp.path().join("nope").join("inner");

    let result = acquire_bounded(
        "caller",
        &missing,
        Duration::from_millis(5),
        Duration::from_millis(50),
    );

    assert!(result.is_err(), "must fail when cache dir does not exist");
    let msg = format!("{:#}", result.unwrap_err());
    assert!(
        msg.contains("failed to open build lock"),
        "error must contain 'failed to open build lock'; got: {msg}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// T-B5  acquire_returns_file_holding_lock  (API contract)
// ──────────────────────────────────────────────────────────────────────────────

/// The returned `File` must genuinely hold the advisory lock: a second
/// `acquire_bounded` on the same dir must fail while the first `File` is alive.
#[test]
fn acquire_returns_file_holding_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path();

    // Public `acquire` — uses the real poll/deadline consts internally.
    // We use `acquire_bounded` with a short deadline to keep the test fast.
    let _holder = acquire_bounded(
        "holder",
        cache,
        Duration::from_millis(5),
        Duration::from_millis(500),
    )
    .unwrap();

    // A second acquire must fail while _holder is alive.
    let second = acquire_bounded(
        "second",
        cache,
        Duration::from_millis(5),
        Duration::from_millis(50),
    );

    assert!(
        second.is_err(),
        "second acquire must fail while holder is alive"
    );
    // _holder drops here.
}

// ──────────────────────────────────────────────────────────────────────────────
// T-B6  deadline_error_is_actionable  (API contract)
// ──────────────────────────────────────────────────────────────────────────────

/// The deadline-expired error must contain both ".skim-build.lock" (the lock
/// path) and "delete the lock file" (the remediation hint).
#[test]
fn deadline_error_is_actionable() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path();

    // Hold the lock so the second call times out.
    let _holder = acquire_bounded(
        "holder",
        cache,
        Duration::from_millis(5),
        Duration::from_millis(500),
    )
    .unwrap();

    let err = acquire_bounded(
        "waiter",
        cache,
        Duration::from_millis(5),
        Duration::from_millis(50),
    )
    .unwrap_err();

    let msg = format!("{err:#}");
    assert!(
        msg.contains(".skim-build.lock"),
        "error must contain '.skim-build.lock'; got: {msg}"
    );
    assert!(
        msg.contains("delete the lock file"),
        "error must contain 'delete the lock file'; got: {msg}"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// T-B7  bounded_loop_terminates_on_deadline  (perf/reliability)
// ──────────────────────────────────────────────────────────────────────────────

/// Hold the lock; time `acquire_bounded` with a 60 ms deadline.
/// Assert: returns Err AND elapsed ≥ 60 ms.
/// Lower-bound only — no upper-bound assertion (flaky under load).
#[test]
fn bounded_loop_terminates_on_deadline() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path();

    let _holder = acquire_bounded(
        "holder",
        cache,
        Duration::from_millis(5),
        Duration::from_millis(500),
    )
    .unwrap();

    let start = Instant::now();
    let result = acquire_bounded(
        "waiter",
        cache,
        Duration::from_millis(5),
        Duration::from_millis(60),
    );
    let elapsed = start.elapsed();

    assert!(result.is_err(), "must return Err when deadline expires");
    assert!(
        elapsed >= Duration::from_millis(60),
        "must wait at least the deadline duration; elapsed={elapsed:?}"
    );
}
