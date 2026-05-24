# Architecture Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19
**Scope**: Incremental review of 2 commits (459d0af...HEAD): SearchAction enum refactor, Result-returning parse_flags, Display for StalenessCheck, infinite rebuild loop fix, HEAD detection hardening.

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Duplicate git-root discovery functions with divergent semantics** - `crates/rskim/src/cmd/init/install.rs:340` and `crates/rskim/src/cmd/search/walk.rs`
**Confidence**: 82%
- Problem: The diff adds a doc comment to `find_git_root_from_cwd` explicitly acknowledging that `discover_project_root` in `walk.rs` performs a similar traversal but with different semantics (`Option<PathBuf>` vs `anyhow::Result<PathBuf>`). Two functions walking up 256 ancestors to find `.git` is a minor modularity concern -- they share the same traversal logic but differ only in return type and fallback behavior.
- Fix: Consider extracting a shared private `find_git_root_inner(start: &Path) -> Option<PathBuf>` that both functions delegate to, adapting only the return type. This removes the risk of one function getting a bug fix (e.g., the 256-ancestor bound) while the other is forgotten. The doc comment is a good band-aid but shared code is more durable.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`check_staleness` returns tuple instead of a typed struct** - `crates/rskim/src/cmd/search/staleness.rs:189-233`
**Confidence**: 80%
- Problem: The function signature `(StalenessCheck, Option<FileManifest>)` is a bare tuple that requires callers to destructure positionally. With `auto_refresh_if_stale` returning `(bool, FileManifest)` and `check_staleness` returning `(StalenessCheck, Option<FileManifest>)`, there are now two distinct tuple-return patterns in the same module. Positional destructuring is fragile -- if the tuple order changes, the compiler may or may not catch misuse (e.g., if both types were `Option<T>`). This is a shallow module pattern (interface exposes as much as it hides).
- Fix: Introduce a small struct, e.g. `StalenessResult { pub status: StalenessCheck, pub manifest: Option<FileManifest> }`, used by both functions. This gives named field access at call sites, making `result.manifest` self-documenting versus `result.1`. Low effort, prevents a class of positional-swap bugs as the API evolves. Not blocking because the current callers are correct and few.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`auto_refresh_if_stale` serializes build + re-load manifest** - `staleness.rs:307-308` (Confidence: 70%) -- After a rebuild, the function calls `FileManifest::load(...)` to re-read the manifest from disk. Since `build_index` already constructs and writes the manifest internally, an alternative design could have `build_index` return the manifest directly, avoiding the extra disk round-trip. Minor efficiency concern for a cold-start path.

- **`SearchAction` enum and `parse_flags` could be replaced by clap derive** - `mod.rs:86-189` (Confidence: 65%) -- The hand-rolled flag parser with a `while i < args.len()` loop duplicates what clap's derive API provides (the rest of the CLI already uses clap). The manual parser is correct and well-tested, but it creates a maintenance asymmetry -- clap handles `skim search index` flags while the parent `skim search` uses bespoke parsing. This is a style/consistency observation, not a defect.

- **`StalenessCheck::Display` slicing could panic on empty strings** - `staleness.rs:43-44` (Confidence: 62%) -- The `HeadChanged` Display arm uses `&stored[..8.min(stored.len())]`. If `stored` were empty, `8.min(0)` = 0, producing `&""[..0]` which is safe. However, `HeadChanged` is only constructed when both stored and current are `Some` from SHA comparison, so empty strings are unreachable in practice. Not a real bug but worth a note.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Assessment

The incremental changes are architecturally sound and represent clear improvements:

1. **SearchAction enum** (OCP improvement) -- Replacing six boolean flags with a sum type makes the dispatch exhaustive at compile time. Adding a new action requires a new variant and a new match arm -- the compiler enforces completeness. This is textbook Open-Closed Principle applied well.

2. **Result-returning parse_flags** (DIP/boundary validation) -- Switching from silent fallback (`if let Some(n) = ...`) to explicit `anyhow::Result` propagation moves error handling to the boundary where it belongs. Invalid `--limit` values no longer silently default to 20.

3. **Manifest-reuse pattern** (performance/modularity) -- Having `check_staleness` and `auto_refresh_if_stale` return the loaded manifest alongside the staleness outcome eliminates a duplicate manifest load in both `run_stats` and `execute_query`. This is a clean elimination of redundant I/O.

4. **Infinite rebuild loop fix** (correctness) -- The 4-way match on `(stored, current)` HEAD is a correct state machine that handles all combinations, including the previously-broken `(None, None)` case for non-git projects. Well-documented with a truth table in the doc comment.

5. **Advisory build lock** (concurrency safety) -- Using `File::lock()` (std 1.84+) for exclusive advisory locking serializes concurrent index builds without external dependencies. Lock-file-survives-process-exit design is correct.

The two MEDIUM findings (duplicate git-root traversal and bare tuple returns) are evolutionary debt, not blocking defects. The conditions for approval are to consider the shared git-root helper when next touching either function.
