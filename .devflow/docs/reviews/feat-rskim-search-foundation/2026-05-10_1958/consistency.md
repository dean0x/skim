# Consistency Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10
**PR**: #213

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Inconsistent clippy allow annotation on test module** - `crates/rskim/src/cmd/search.rs:72`
**Confidence**: 90%
- Problem: The test module in `search.rs` lacks the `#[allow(clippy::unwrap_used)]` annotation that is consistently applied to test modules in both `rskim-core` and `rskim-search` library crates. Both `crates/rskim-core/src/types.rs` and `crates/rskim-search/src/types.rs` use `#[allow(clippy::unwrap_used)]` on their test modules, and `rskim-core/src/lib.rs` uses `#[allow(clippy::expect_used)]`. While `search.rs` tests currently use `.unwrap()` without issue (the `rskim` binary crate does not deny `unwrap_used` in its `[lints.clippy]`), this deviates from the pattern established by the two library crates whose clippy deny rules make the annotation mandatory. Since the search command will eventually wire into the `rskim-search` library (per the PR description), adopting the annotation now keeps the convention consistent if the lint config is later unified.
- Fix: Add the annotation for forward-compatibility and convention alignment:
```rust
#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
```

**`rskim-search` dependency removed from `rskim` binary but doc comments still reference it** - `crates/rskim/src/cmd/search.rs:4,9`
**Confidence**: 85%
- Problem: The `rskim-search` crate dependency was removed from `crates/rskim/Cargo.toml` in this PR (the diff shows the line `rskim-search = { version = "0.1.0", path = "../rskim-search" }` was deleted). However, the module-level doc comments in `search.rs` still say "The full search implementation lives in `rskim-search` library crate" (line 4) and "- `rskim-search` crate: types, traits, indexing layer implementations" (line 9). This creates a mismatch: the binary crate does not actually depend on `rskim-search`, so these comments describe an integration that does not exist yet.
- Fix: Either (a) add back the `rskim-search` dependency to `crates/rskim/Cargo.toml` if it should be wired in, or (b) update the doc comments to say something like "The full search implementation will be provided by the `rskim-search` library crate (dependency not yet wired in)."

## Issues in Code You Touched (Should Fix)

_No issues found._

## Pre-existing Issues (Not Blocking)

_No issues found._

## Suggestions (Lower Confidence)

- **`SearchQuery` missing `Serialize`/`Deserialize`** - `crates/rskim-search/src/types.rs:105` (Confidence: 65%) -- All other public structs in the file (`FileId`, `SearchField`, `TemporalFlags`, `SearchResult`, `IndexStats`) derive both `Serialize` and `Deserialize`. `SearchQuery` derives neither. This may be intentional (queries are constructed in-process, not deserialized from JSON), but the asymmetry is notable. If a future `--json` input mode or API endpoint accepts queries, the derive would be needed.

- **`rskim-core::Language` lacks `Serialize`/`Deserialize` but `rskim-search` types that reference it do** - `crates/rskim-search/src/types.rs:110` (Confidence: 60%) -- `SearchQuery.lang` is `Option<rskim_core::Language>`, but `Language` in rskim-core does not derive `Serialize`/`Deserialize`. If `SearchQuery` ever needs serde derives (see above), this would be a blocking gap. Currently not an issue since `SearchQuery` skips serde derives.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new `rskim-search` crate demonstrates strong consistency with the existing codebase:

- Edition 2024 is uniform across all three workspace crates.
- `thiserror` 2.0 is used via workspace dependency in both library crates.
- Error type structure (`SearchError`) mirrors `SkimError` pattern: typed variants with `#[from]` conversions, a `Result<T>` type alias, and `thiserror::Error` derive.
- Section headers (`// ===...`) are consistent across all files.
- Test module `#[cfg(test)]` + `#[allow(clippy::unwrap_used)]` pattern matches `rskim-core`.
- Clippy lint config in `Cargo.toml` (`[lints.clippy]`) is identical between the two library crates.
- The `pub(crate) fn run(args, analytics)` command signature in `search.rs` matches every other command module.
- Import reordering (alphabetical within braces) is consistent with the `cargo fmt` pass applied to the rest of the crate.
- `#[must_use]` annotations on `SearchField::name()` and `SearchQuery::new()` match the idiom used elsewhere in the project.

The two MEDIUM findings are minor documentation/annotation alignment issues. No architectural, naming, error-handling, or API-style inconsistencies were found.
