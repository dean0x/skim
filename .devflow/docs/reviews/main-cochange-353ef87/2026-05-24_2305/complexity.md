# Complexity Review Report

**Branch**: 353ef87 (co-change matrix builder)
**Date**: 2026-05-24
**Focus**: Complexity

## Issues in Your Changes (BLOCKING)

### HIGH

**`accumulate_pairs` exceeds 50-line function length threshold (73 lines)** - `builder.rs:132-206`
**Confidence**: 90%
- Problem: At 73 lines (including doc comments), this function handles path resolution, deduplication, pair generation with nested loops, max-pairs safety checks, and stats assembly. It has cyclomatic complexity ~8 (for loop, if, match, nested for+for, if, saturating_add branches). The nested `for i / for j` loop at lines 177-194 reaches nesting depth 4 (fn > for > for > if), which is at the warning threshold.
- Fix: Extract the inner pair-generation loop into a helper function:
```rust
fn generate_pairs(
    ids: &[u32],
    pair_counts: &mut HashMap<(u32, u32), u32>,
    max_pairs: usize,
) -> Result<()> {
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            let (a, b) = (ids[i].min(ids[j]), ids[i].max(ids[j]));
            debug_assert!(a < b);
            if !pair_counts.contains_key(&(a, b)) && pair_counts.len() >= max_pairs {
                return Err(SearchError::IndexCorrupted(
                    "co-change pair count exceeds safety limit".into(),
                ));
            }
            let entry = pair_counts.entry((a, b)).or_insert(0);
            *entry = entry.saturating_add(1);
        }
    }
    Ok(())
}
```
This reduces `accumulate_pairs` to ~45 lines and drops nesting depth to 3.

**`serialize` function is 79 lines long** - `builder.rs:209-288`
**Confidence**: 85%
- Problem: At 79 lines, `serialize` is well above the 50-line threshold. It handles sorting file entries, sorting pair entries, overflow-checked byte computation, buffer assembly, CRC32 computation, and header construction. However, its cyclomatic complexity is low (~3) and control flow is linear -- it reads as a sequence of steps. The length is primarily driven by checked arithmetic and verbose error messages, which are desirable for a binary format encoder.
- Fix: Consider extracting the sorted-entry collection into a helper:
```rust
fn collect_sorted_file_entries(counts: &HashMap<u32, u32>) -> Vec<FileCommitEntry> { ... }
fn collect_sorted_pair_entries(counts: &HashMap<(u32, u32), u32>) -> Vec<PairEntry> { ... }
```
This would bring `serialize` under 50 lines while keeping the linear flow clear.

### MEDIUM

**`pairs_for_file` O(n) linear scan documented but not bounded** - `reader.rs:189-208`
**Confidence**: 80%
- Problem: The function performs a full linear scan over all pair entries (up to 2M pairs = 24 MB). While the O(n) complexity is documented in the doc comment, the function itself has no cap on the result vector size. A file that co-changes with thousands of partners will allocate an unbounded Vec. The comment at line 180-182 acknowledges this and describes a future binary search optimization, but no bound is enforced in the current implementation.
- Fix: Add an optional `limit` parameter or a hard cap on `results.len()` to prevent unbounded allocation:
```rust
const MAX_PARTNERS: usize = 1000;
if results.len() >= MAX_PARTNERS { break; }
```
Alternatively, since the scan is documented and MAX_PAIRS caps total entries at 2M, this is acceptable for v1 if the linear scan cost is tolerable.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Redundant min/max after sort+dedup** - `builder.rs:179-180` (Confidence: 70%) -- After `ids.sort_unstable(); ids.dedup();`, the vector is sorted ascending, so `ids[i] < ids[j]` is guaranteed when `i < j`. The `.min()/.max()` calls at line 179-180 are redundant since the pair is already canonical by construction. The `debug_assert!(a < b)` on line 183 confirms the author knows this. Removing the min/max would simplify slightly but the current form is defensive and correct.

- **Type alias `AccumulatedPairs` is a 3-tuple** - `builder.rs:122` (Confidence: 65%) -- The type alias `(HashMap<(u32, u32), u32>, HashMap<u32, u32>, CochangeStats)` packs three semantically distinct values into a positional tuple. A named struct would be more self-documenting, though the alias is only used internally.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

### Passed Checks

- **Nesting depth**: Maximum nesting is 4 levels in `accumulate_pairs` (at the warning threshold, not critical). All other functions stay at 2-3 levels.
- **Cyclomatic complexity**: All functions have complexity < 10. `decode_header` has ~5 (multiple validation branches), `accumulate_pairs` ~8, everything else < 5.
- **Magic numbers**: All constants are well-named (`SKCC_MAGIC`, `FORMAT_VERSION`, `HEADER_SIZE`, `FILE_COMMIT_ENTRY_SIZE`, `PAIR_ENTRY_SIZE`, `COUPLING_MAX_FILES`, `MAX_PAIRS`). Byte offsets in encode/decode functions (e.g., `buf[0..4]`, `buf[4..6]`) are derived from the structs' documented layouts and use named size constants.
- **File lengths**: `builder.rs` (315 lines), `format.rs` (297 lines), `reader.rs` (273 lines) -- all under the 300-line warning threshold (or just barely over for builder.rs, which includes doc comments and section separators).
- **Parameter counts**: All functions have 2-4 parameters, well within acceptable range.
- **5-minute understandability**: The module structure (format = pure codec, builder = write path, reader = query path) is clear and well-separated. Doc comments are thorough. A newcomer can follow the flow: builder accumulates pairs from history, serializes to binary, reader mmaps and queries. The `.skcc` format layout is documented in multiple places.
- **Bounded iteration**: All loops iterate over finite collections (commits, file IDs, pair entries). The `MAX_PAIRS` safety cap prevents unbounded memory growth. `saturating_add` prevents counter overflow.
- **Error handling**: Consistently uses `Result` types with descriptive `SearchError::IndexCorrupted` messages. No `unwrap()`/`expect()` outside `#[cfg(test)]`.
- **Checked arithmetic**: All size computations use `checked_mul`/`checked_add` to prevent overflow on 32-bit targets.
- **Test coverage**: 885 lines of tests across 3 test files covering edge cases (empty, boundary values, corruption, deduplication, safety caps, Send+Sync).

**Complexity Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

Two functions (`accumulate_pairs` at 73 lines, `serialize` at 79 lines) exceed the 50-line threshold. Both have low cyclomatic complexity and linear control flow, so the practical readability impact is moderate. The suggested extractions would bring them into compliance without changing behavior. The overall architecture is clean, well-documented, and follows single-responsibility: format.rs handles pure encoding/decoding, builder.rs handles accumulation and writing, reader.rs handles mmap-based querying.
