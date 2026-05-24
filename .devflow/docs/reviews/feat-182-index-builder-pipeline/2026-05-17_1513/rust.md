# Rust Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**Mutex `.unwrap()` in parallel walker can panic and abort process** - `walk.rs:261,272,275,287`
**Confidence**: 85%
- Problem: Four `.unwrap()` calls on `Mutex::lock()` inside the parallel walker closure. If any thread panics (e.g., due to an OS-level allocation failure in `classify_entry` or a panic in `ReadFile` construction), the Mutex becomes poisoned and all subsequent `.lock().unwrap()` calls in other threads will panic, cascading into process abort. The `Arc::try_unwrap(...).expect(...)` and `.into_inner().expect(...)` at lines 300-307 compound this: if a thread panics while holding the lock, `into_inner()` returns `Err` (poisoned) and the `expect` panics again.
- Fix: Use `.lock().unwrap_or_else(|e| e.into_inner())` to recover from poisoned mutexes (the data is still valid even after another thread panics). Alternatively, since `classify_entry` is infallible (returns an enum, never panics), this is low-probability, but the `expect` messages at lines 300-307 are misleading — they say "no thread panicked" but that is the exact scenario that would trigger the failure. A more defensive approach:
```rust
// Replace .lock().unwrap() with:
let mut guard = files.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
guard.push(file);
```

### MEDIUM

**`write!(hex, ...).unwrap()` in `index.rs:316` — writing to a String cannot fail** - `index.rs:316`
**Confidence**: 82%
- Problem: `write!` to a `String` is infallible (the `fmt::Write` impl for `String` always returns `Ok`), so this `.unwrap()` is technically safe but is inconsistent with the rest of the PR which replaced the similar pattern in `walk.rs:388` with `String::from_utf8(hex).expect(...)`. This is a consistency issue within the same PR.
- Fix: Add a comment like `// Writing to String never fails` or use the same nibble-table approach as `sha256_hex` in `walk.rs` for consistency. Not blocking, but worth noting.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Race between `file_count.fetch_add` and `files.lock().push` allows over-counting** - `walk.rs:271-272`
**Confidence**: 80%
- Problem: The atomic increment at line 271 happens before the actual push at line 272. If a thread increments the counter but then another thread checks the cap at line 259 and quits, the counter reports one more accepted file than what actually ends up in the `files` vec. This is benign because `files.truncate(max_files)` at line 312 handles the over-collection, but the increment happening outside the lock means the counter can briefly diverge from actual vec length. The current code acknowledges this with the truncate, but incrementing inside the lock would make the invariant tighter.
- Fix: Move `fetch_add` inside the lock scope, or accept the current approach is sufficient given the truncate safety net (comment already documents the TOCTOU). Current behavior is correct but sub-optimal.

## Pre-existing Issues (Not Blocking)

_None identified at CRITICAL severity._

## Suggestions (Lower Confidence)

- **Consider `parking_lot::Mutex` for non-poisoning semantics** - `walk.rs` (Confidence: 65%) — `parking_lot::Mutex` never poisons, which would eliminate the entire class of cascading-panic issues in the parallel walker without behavioral changes.

- **`sync_data()` in manifest save may be excessive for a cache file** - `manifest.rs:277` (Confidence: 62%) — `sync_data()` forces a physical write to disk before the rename. For a cache file that can be regenerated, this adds latency on every save. The atomic rename already provides crash consistency for the reader (they see either the old or new file). The `sync_data` only protects against power-loss leaving zeros in the new file — a scenario where the cache would simply be regenerated on next run anyway.

- **`cap_reached` flag only prevents duplicate `SkipReason::CapReached` pushes, not duplicate quit signals** - `walk.rs:260` (Confidence: 70%) — Multiple threads can independently observe `file_count >= max_files` at line 259 and return `WalkState::Quit`. The `cap_reached` flag only gates the push of `SkipReason::CapReached`. This is fine (multiple Quit returns just accelerate shutdown) but the naming suggests it controls "whether we reached the cap" when it really controls "whether we recorded the skip reason."

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code demonstrates strong Rust patterns: proper use of type-safe enums (`EntryOutcome`, `ReadOutcome`), explicit ownership transfer via `zip/into_iter`, bounded loops, compile-time assertions, and atomic writes for crash safety. The parallel walker refactoring from sequential to `build_parallel()` is well-structured with proper TOCTOU documentation. The main concern is the `.unwrap()` on mutex locks inside the parallel closure — while unlikely to trigger in practice (since `classify_entry` is infallible), the cascading-panic risk in a production CLI warrants using poison-recovery or `parking_lot`.
