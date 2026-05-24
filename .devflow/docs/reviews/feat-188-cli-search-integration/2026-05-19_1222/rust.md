# Rust Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19
**Scope**: Incremental review of commits 459d0af...HEAD (2 commits: SearchAction enum refactor, infinite rebuild loop fix)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**`is_hex_sha` doc says "lowercase" but implementation accepts uppercase** - `crates/rskim/src/cmd/search/staleness.rs:155-161`
**Confidence**: 82%
- Problem: The doc comment states "lowercase hex commit hash" but `is_ascii_hexdigit()` returns `true` for both uppercase (`A-F`) and lowercase (`a-f`). While git itself always outputs lowercase hex, the function contract documented in the comment is stricter than the actual behavior. If `is_hex_sha` is ever used to validate user-supplied input, uppercase strings would pass validation contrary to the documented contract.
- Fix: Either restrict the implementation to match the docs (`b.is_ascii_lowercase() || b.is_ascii_digit()` or add `&& s == s.to_lowercase()`), or update the doc to say "hex" instead of "lowercase hex":
  ```rust
  /// Return `true` if `s` looks like a 40-character (SHA-1) or 64-character
  /// (SHA-256) hex commit hash.
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Duplicate `metadata()` syscall in `extract_snippet`** - `crates/rskim/src/cmd/search/snippet.rs:124,137`
**Confidence**: 88%
- Problem: When the mtime guard is active (manifest has mtime), `std::fs::metadata(&abs_path)` is called at line 124 for the mtime check and again at line 137 for the size guard. This is a redundant syscall per snippet extraction. With the default 20 results, this adds up to 20 extra `stat(2)` calls per query.
- Fix: Fetch metadata once and reuse:
  ```rust
  let abs_path = root.join(rel_path);
  let meta = std::fs::metadata(&abs_path).ok();

  // Mtime guard
  if let Some(stored_mtime) = manifest_entry.and_then(|e| e.mtime) {
      let current_mtime = meta.as_ref()
          .and_then(|m| m.modified().ok())
          .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
          .map(|d| d.as_secs());
      if current_mtime != Some(stored_mtime) {
          return SnippetOutcome::Stale;
      }
  }

  // Size guard
  const MAX_SNIPPET_FILE_BYTES: u64 = 5 * 1024 * 1024;
  let file_size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
  if file_size > MAX_SNIPPET_FILE_BYTES {
      return SnippetOutcome::Unavailable;
  }
  ```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`extract_context_window` iterates lines twice** - `crates/rskim/src/cmd/search/snippet.rs:66,83` (Confidence: 65%) -- `content.lines().count()` at line 66 iterates all lines to get the count, then `.lines().enumerate().skip().take()` at line 83 iterates again. For very large files (e.g. near the 5 MB cap), this scans up to 10 MB of text. A single-pass approach using `lines().enumerate()` with early termination could avoid the double scan, though the size guard makes this unlikely to matter in practice.

- **`SearchAction::Query` variant carries allocation even for non-query actions** - `crates/rskim/src/cmd/search/mod.rs:99` (Confidence: 62%) -- When the user runs `--build`, `query_parts` is allocated as an empty `Vec<String>` and then `query_parts.join(" ")` produces an empty `String` that is placed into the `Query("")` variant before `action_flag.unwrap_or_else()` discards it. Not a real cost, but the `unwrap_or_else` closure always runs the join. Moving query assembly inside the closure or using `unwrap_or(SearchAction::Query(String::new()))` would be marginally cleaner.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | - | 0 | 1 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Positive Observations

1. **Excellent enum-driven dispatch**: Replacing the boolean flag cascade (`if flags.build / if flags.rebuild / ...`) with `SearchAction` enum and `match` is textbook Rust -- makes illegal states unrepresentable and ensures exhaustive dispatch.

2. **Result-returning `parse_flags`**: The refactor from silently ignoring bad flags to returning `anyhow::Result` with clear error messages is a significant robustness improvement. Error messages are actionable (show valid flags, expected format).

3. **Sound staleness state machine**: The `(stored, current)` match with four cases (None/None, None/Some, Some/None, Some/Some) eliminates the infinite rebuild loop on non-git projects cleanly, with a well-documented truth table.

4. **BTreeMap for deterministic manifest**: Switching `HashMap -> BTreeMap` eliminates the `.sort_unstable()` in `sorted_paths()` and `save()`, reducing allocation and ensuring alphabetical order by construction.

5. **Advisory build lock**: The `File::lock()` approach in `build_index` with RAII-based release is clean and prevents concurrent index corruption without adding a new dependency.

6. **TOCTOU fix in `write_hook_atomic`**: Moving from a fixed `.tmp` suffix to `NamedTempFile::new_in` closes the symlink attack vector.

7. **Path traversal guard in `read_git_head`**: The `ref_path.starts_with("refs/")` check is a practical defense against crafted `.git/HEAD` files.

8. **Comprehensive test coverage**: New tests cover error cases (missing `--limit` value, non-numeric limit, unrecognised flags, missing `--root` value), edge cases (no .git dir, corrupt index), and the stale marker display -- 56 new tests is thorough.

### Conditions for Approval

The duplicate metadata syscall (Should Fix) is the only condition -- it is a minor performance inefficiency, not a correctness issue. If the author prefers to defer this to a follow-up, that is acceptable.
