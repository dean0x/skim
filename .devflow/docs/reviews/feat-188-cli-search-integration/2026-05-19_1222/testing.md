# Testing Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19
**Scope**: Incremental (459d0af...HEAD, 2 commits)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Missing Display trait tests for StalenessCheck** - `crates/rskim/src/cmd/search/staleness.rs:36-49`
**Confidence**: 85%
- Problem: The new `Display` impl for `StalenessCheck` is used in user-facing output (`--stats` text and JSON, debug log messages) but has no dedicated unit tests. The `HeadChanged` variant performs string slicing (`&stored[..8.min(stored.len())]`) which could panic on non-ASCII or short inputs; this is tested implicitly via integration paths, but the `Display` formatting contract itself (the exact "stale (HEAD changed: ...)" strings) is not verified.
- Fix: Add tests for each `Display` variant in `staleness_tests.rs`:
```rust
#[test]
fn test_staleness_display_current() {
    assert_eq!(StalenessCheck::Current.to_string(), "current");
}

#[test]
fn test_staleness_display_head_changed() {
    let s = StalenessCheck::HeadChanged {
        stored: "a".repeat(40),
        current: "b".repeat(40),
    };
    assert!(s.to_string().contains("stale"));
    assert!(s.to_string().contains("aaaaaaaa"));
}

#[test]
fn test_staleness_display_no_stored_head() {
    assert_eq!(StalenessCheck::NoStoredHead.to_string(), "stale (no HEAD recorded)");
}

#[test]
fn test_staleness_display_no_index() {
    assert_eq!(StalenessCheck::NoIndex.to_string(), "no index");
}
```

**Missing test for `StalenessCheck::Display` with short SHA** - `crates/rskim/src/cmd/search/staleness.rs:43-44`
**Confidence**: 82%
- Problem: The `HeadChanged` Display impl slices at `8.min(stored.len())`, which handles short strings correctly via `min()`. However, there is no test that exercises the boundary where `stored.len() < 8`. If a corrupt manifest stored a 3-char HEAD, the Display path would silently truncate -- this path needs explicit coverage.
- Fix: Add a test with a short stored SHA (e.g., `"abc"`) and verify it does not panic and produces a reasonable output string.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`parse_flags` lacks test for conflicting action flags** - `crates/rskim/src/cmd/search/mod.rs:120-189`
**Confidence**: 80%
- Problem: `parse_flags` uses `action_flag = Some(...)` which means the last action flag wins when multiple are provided (e.g., `--build --rebuild`). There is no test validating this "last-wins" behavior. If the intent is that conflicting flags should error, this is a behavioral gap. If "last wins" is intentional, a test should document that contract.
- Fix: Add a test clarifying the expected behavior:
```rust
#[test]
fn test_parse_flags_last_action_wins_when_multiple_specified() {
    // Document that the last action flag takes precedence.
    let flags = parse_flags(&["--build".to_string(), "--rebuild".to_string()]).unwrap();
    assert_eq!(flags.action, SearchAction::Rebuild);
}
```

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`test_execute_query_corrupt_index_returns_err_not_panic` accepts both Ok and Err** - `crates/rskim/src/cmd/search/query_tests.rs:266-289`
**Confidence**: 80%
- Problem: This test uses a `match` that accepts both `Ok(_)` and `Err(e)` outcomes as valid, making the assertion very weak. The test cannot fail unless the code panics. While the stated intent is "no panic", in practice the test provides no regression signal if behavior changes from "rebuilds successfully" to "returns error" or vice versa. The comment acknowledges this ("both outcomes are acceptable"), but it means the test does not actually validate any specific behavior.
- Fix: This is a pre-existing design choice from a prior commit. If the test should validate a specific outcome, split it into two tests: one where auto-refresh is expected to succeed and one where it cannot (e.g., by also corrupting the source files so rebuild fails).

## Suggestions (Lower Confidence)

- **Missing test for `auto_refresh_if_stale` manifest return value on rebuild paths** - `crates/rskim/src/cmd/search/staleness.rs:307-309` (Confidence: 75%) -- The `NoIndex` and `HeadChanged` rebuild paths both load the manifest after rebuild, but only the `HeadChanged` path has a test that verifies the returned manifest contains the new HEAD (`test_auto_refresh_rebuilds_on_head_changed`). The `NoIndex` path (`test_auto_refresh` via `test_auto_refresh_rebuilds_on_no_stored_head`) also checks it but there is no `NoIndex`-path-specific test verifying the returned manifest.

- **`test_execute_query_no_git_dir_returns_ok_or_graceful_err` accepts any outcome** - `crates/rskim/src/cmd/search/query_tests.rs:239-263` (Confidence: 70%) -- Same pattern as the corrupt-index test: both Ok and Err are accepted, so the test only guards against panics. For a non-git project with a valid source file, the expected behavior could be more precisely asserted.

- **No negative test for `is_hex_sha` rejecting 41/63 character strings** - `crates/rskim/src/cmd/search/staleness.rs:160-161` (Confidence: 65%) -- The `is_hex_sha` function was updated to accept 40 or 64 chars but no test validates that 41-char or 63-char strings are rejected. The existing SHA-256 acceptance test covers the positive case; a boundary test for off-by-one lengths would strengthen confidence.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Testing Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The incremental changes add strong test coverage for the new `parse_flags` Result-based API (error cases, short flags, combined flags) and for the infinite-rebuild-loop fix (non-git project, git-appeared, unreadable-git scenarios). The tests follow good practices: behavior-focused, tempdir-based isolation, clear AAA structure, and meaningful assertion messages.

The primary gaps are: (1) the new `Display` impl for `StalenessCheck` has no dedicated tests despite being user-facing, and (2) the conflicting-action-flags behavior is undocumented by tests. Neither is blocking, but both should be addressed before or shortly after merge.
