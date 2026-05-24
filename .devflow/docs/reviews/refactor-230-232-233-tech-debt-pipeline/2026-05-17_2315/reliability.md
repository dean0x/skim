# Reliability Review Report

**Branch**: PR #242 -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**FileId overflow error returned inside consumer loop aborts entire build** - `index.rs:276-278`
**Confidence**: 85%
- Problem: When `next_file_id` overflows u32 (via `checked_add`), the error propagates via `?` out of `Pipeline::run()`, aborting the entire index build. This contradicts the stated fail-soft design where "a single file that fails to index should not abort a 50 K-file build." At file 4,294,967,295 the overflow causes the entire index (including all previously processed files) to be lost because `builder.build()` and `new_manifest.save()` never run.
- Fix: Treat the overflow as a per-file error (log under debug, break out of the consumer loop) rather than a fatal error. The index and manifest should still be flushed for the files that were successfully processed:
```rust
next_file_id = match next_file_id.checked_add(1) {
    Some(id) => id,
    None => {
        if debug_enabled {
            eprintln!(
                "skim search index [debug]: next_file_id overflows u32; stopping indexing"
            );
        }
        break; // stop accepting new files, but flush what we have
    }
};
```

### MEDIUM

**Producer thread panic payload inspection is incomplete** - `index.rs:295-302`
**Confidence**: 82%
- Problem: The `producer_handle.join()` error handler only attempts to downcast the panic payload to `&String`. Rust panic payloads from `panic!("literal")` are `&str`, not `String`. The `downcast_ref::<String>()` call will fail for the most common panic types, always producing the fallback `<non-string panic>` message, losing diagnostic information.
- Fix: Try both `&str` and `String`:
```rust
producer_handle.join().map_err(|e| {
    let msg = e.downcast_ref::<&str>()
        .copied()
        .or_else(|| e.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic>");
    anyhow::anyhow!("producer thread panicked: {msg}")
})?;
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Mtime field stored but never used for pre-screening** - `index.rs:361-367`, `manifest.rs:61-70`
**Confidence**: 88%
- Problem: The PR description mentions "mtime pre-screening" and the `ManifestEntry` now carries an `mtime: Option<u64>` field. However, the `read_and_classify` function (the 4-tier cache logic on lines 361-378) only checks `cached.sha256 == sha` -- it never consults the mtime value for the fast pre-screening hint described in the manifest field's own doc comment: "skip SHA computation when the file has not changed (mtime match -> likely SHA match -> reuse field_map)." The mtime is recorded into the manifest but never read back for any optimization. This is dead data today -- the "4-tier" cache is only 2 tiers (force flag + SHA match).
- Fix: Either implement the mtime pre-screening optimization that skips SHA computation when `entry.mtime == cached.mtime` (which is the stated goal), or remove the "4-tier" language from comments to accurately describe the 2-tier behavior that exists. If implementing:
```rust
// Quick mtime pre-screen: if mtime matches, skip SHA computation.
if !force
    && let Some(cached) = manifest.lookup(&path_key)
    && entry.mtime.is_some()
    && cached.mtime == entry.mtime
{
    // mtime match → assume content unchanged, reuse field_map.
    return Ok(ProcessedFile {
        rel_path: entry.rel_path.clone(),
        lang: entry.lang,
        content,
        sha256: sha256_hex(content.as_bytes()), // still compute for manifest correctness
        mtime: entry.mtime,
        field_map: decode_field_map(&cached.field_map),
        cache_hit: true,
    });
}
```

**Channel capacity comment overstates worst-case memory** - `index.rs:159-162`
**Confidence**: 80%
- Problem: The comment says "64 x 5 MiB max file size = 320 MiB worst-case buffered in the channel." However, `ProcessedFile` also includes `sha256: String` (64 bytes), `rel_path: PathBuf`, `field_map: Vec<...>`, and `lang`. The field_map in particular can be proportional to file complexity. The actual worst-case per-slot is `5 MiB content + field_map + metadata`, making 320 MiB an undercount. For a reliability-focused review, the bound documentation should be accurate.
- Fix: Update the comment to acknowledge the overhead: "64 x (5 MiB max content + field_map + metadata) -- approximately 320-350 MiB worst-case."

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Producer skips counter uses Relaxed ordering across thread boundary** - `index.rs:240,310` (Confidence: 65%) -- `AtomicU32` with `Ordering::Relaxed` for `producer_skips` is technically sufficient since the `join()` call provides the happens-before synchronization, but the intent is non-obvious. An `Acquire`/`Release` pair would make the synchronization contract self-documenting.

- **walk_metadata TOCTOU on atomic counter can over-collect** - `walk.rs:390-403` (Confidence: 70%) -- The `entry_count` check and `fetch_add` are separate operations, so parallel threads can over-collect past `max_files`. The code handles this with `entries.truncate(max_files)` on line 447, which is correct, but worth noting that the `CapReached` skip reason may be pushed even when the cap was not actually reached (a thread saw >= max_files due to TOCTOU but the truncation brought it back). This is cosmetic, not a correctness issue.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The streaming pipeline is well-designed with proper backpressure, bounded channel capacity, and correct producer-consumer lifecycle (tx drop signals EOF, join propagates panics). The core bounded-iteration and allocation discipline patterns are solid. The two blocking items are: (1) the FileId overflow path violates the fail-soft contract by aborting the entire build, and (2) the panic payload downcast misses `&str` payloads, degrading diagnostics. The mtime field is stored but not used for its stated purpose, which is a should-fix incomplete feature rather than a reliability defect.
