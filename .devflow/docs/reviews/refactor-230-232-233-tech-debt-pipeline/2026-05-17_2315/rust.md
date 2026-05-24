# Rust Review Report

**Branch**: PR #242 -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**Misleading "4-tier mtime/SHA cache" documentation** - `index.rs:10`, `index.rs:325`
**Confidence**: 85%
- Problem: The module doc comment (line 10) and the `read_and_classify` doc comment (line 325) both reference a "4-tier mtime/SHA cache" strategy. However, the actual implementation performs only a 2-tier check: it always computes the SHA-256 and then checks if it matches the manifest entry. The `mtime` field is persisted in `WalkEntry`, `ProcessedFile`, and `ManifestEntry` but is never consulted to skip SHA computation. The code path `read_and_classify` reads the file, computes SHA, and checks the manifest -- mtime is only passed through to storage.
- Fix: Either (a) update the doc comments to accurately describe the current behavior (SHA-match cache with mtime stored for future use), or (b) implement the mtime pre-screening check in `read_and_classify` before calling `sha256_hex`. Option (a) is the minimal fix:

```rust
// index.rs line 10:
//!    SHA-based cache, classifies; sends ProcessedFile on bounded channel

// index.rs line 325:
/// Read a file's content, apply SHA-based cache logic, and produce a
```

### MEDIUM

**Early return from consumer loop skips `producer_handle.join()`** - `index.rs:276-278`
**Confidence**: 82%
- Problem: If `next_file_id.checked_add(1)` returns `None` (overflow), the `?` operator causes `run()` to return `Err` immediately. The `producer_handle` is dropped without calling `join()`, making the producer thread detached. Any panic in the producer after this point would be silently swallowed rather than propagated.
- Impact: Practically unreachable -- `IndexConfig::DEFAULT_MAX_FILES` is 50,000 and `u32::MAX` is ~4 billion. The max_files cap makes overflow impossible under normal operation. The producer will also self-terminate cleanly (the closed channel causes `tx.send()` to return `Err`, triggering `break`).
- Fix: Wrap the consumer loop and join in a scope that always joins the producer, even on early return. For example, use a `Result` variable and defer the `?`:

```rust
let consume_result: anyhow::Result<()> = (|| {
    for pf in rx {
        // ... existing consumer logic ...
    }
    Ok(())
})();

// Always join the producer, even if the consumer errored.
producer_handle.join().map_err(|e| { /* ... */ })?;
consume_result?;
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`walk_metadata` and `walk_and_read` have duplicated parallel walker boilerplate** - `walk.rs:236-290`, `walk.rs:370-453` (Confidence: 70%) -- The two functions share nearly identical Arc/Mutex setup, WalkBuilder configuration, parallel dispatch closure structure, Arc::try_unwrap teardown, truncation, and sorting logic. While `walk_and_read` is now `#[cfg(test)]` only, a shared generic walker that accepts a classify callback would reduce maintenance surface.

- **`ProcessedFile` could benefit from `#[must_use]` on its construction** - `types.rs:108` (Confidence: 65%) -- The `ProcessedFile` struct is a critical pipeline message sent across a channel. Adding `#[must_use]` to the struct would prevent accidental construction without sending, though in practice this is already enforced by the send/receive pattern.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The streaming pipeline design is well-structured: bounded-channel backpressure, proper cleanup of the channel via RAII (drop of `tx` signals EOF), fail-soft error handling in both producer and consumer, and correct FileId sequencing that only advances on success. The concurrency model is sound -- a single producer thread with a main-thread consumer avoids shared mutable state entirely.

The `mtime` infrastructure (WalkEntry field, ManifestEntry serialization with `#[serde(default)]` for backward compat, mtime extraction helper) is correctly laid as groundwork for future pre-screening. The `#[cfg(test)]` gating of the old `walk_and_read` / `classify_entry` / `handle_entry` code is clean -- production code uses only the new streaming path.

The HIGH finding (misleading "4-tier" documentation) is the primary concern. Documentation that describes functionality not yet implemented creates confusion for future maintainers. The MEDIUM finding (orphaned producer on overflow) is practically unreachable but represents an architectural gap in the cleanup path. Both are straightforward to address.
