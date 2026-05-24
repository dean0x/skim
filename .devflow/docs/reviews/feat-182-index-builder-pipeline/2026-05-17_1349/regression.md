# Regression Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**Search help text changed without backward-compatible alias** - `crates/rskim/src/cmd/search/mod.rs:55-81`
**Confidence**: 82%
- Problem: The old `search.rs` help text showed `Usage: skim search [OPTIONS] <QUERY>` and listed only query-related options. The new `search/mod.rs` help text now shows `Usage: skim search <SUBCOMMAND|QUERY> [OPTIONS]` and adds the `index` subcommand. While the new help is more complete and correct, the format change could break scripts or documentation that parse help output.
- Fix: This is an intentional expansion of the search subcommand surface area. The old behavior (no-args prints help, unknown query returns FAILURE) is preserved in the new code. The new `index` subcommand dispatch is checked *before* the help flag check, which is correct (and tested via `test_index_help_dispatches_to_index_not_parent`). Low actual regression risk since the underlying behavior is preserved. Informational only.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`unsafe` block in sha256_hex could use safe alternative** - `crates/rskim/src/cmd/search/walk.rs:332` (Confidence: 65%) -- `String::from_utf8_unchecked` is used with a hand-rolled hex encoder. While the SAFETY comment is correct (NIBBLES only contains ASCII hex chars), `String::from_utf8(hex).unwrap()` or `String::from_utf8(hex).expect("hex is always ASCII")` would provide the same performance with a debug-mode safety net and zero `unsafe` usage.

- **Manifest `entries` HashMap uses default hasher on attacker-controlled paths** - `crates/rskim/src/cmd/search/manifest.rs:74` (Confidence: 62%) -- The `entries: HashMap<String, ManifestEntry>` uses the default `RandomState` hasher, which is fine for correctness and security. However, if this index pipeline is ever exposed to adversarial repos with crafted filenames, hash flooding is theoretically possible. Current usage (local repos only, behind CLI) makes this a non-issue in practice.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED

## Detailed Regression Analysis

### 1. Lost Functionality Check

- **No exports removed**: The `search::run` function retains its `pub(crate)` signature with identical parameters `(&[String], &AnalyticsConfig) -> Result<ExitCode>`. The file was renamed from `search.rs` to `search/mod.rs` (git detects this as R061 -- 61% rename), but the public API surface is unchanged.
- **No CLI options removed**: All original `skim search` behaviors are preserved:
  - Empty args -> help (SUCCESS)
  - `--help` / `-h` -> help (SUCCESS)
  - Unknown query -> "not yet implemented" (FAILURE)
- **New `index` subcommand added**: `skim search index` is additive functionality, not a replacement.

### 2. Broken Behavior Check

- **Return types unchanged**: `run()` still returns `anyhow::Result<ExitCode>`.
- **Default values unchanged**: No existing defaults were modified.
- **Side effects preserved**: The `search` command's output patterns (stdout for help, stderr for errors) are preserved.
- **InfraToolConfig field addition**: The `skip_ansi_strip: bool` field was added to `InfraToolConfig`. All 7 existing tool configs (aws, curl, docker, gh, kubectl, terraform, wget) were updated with `skip_ansi_strip: false`, preserving their existing ANSI-stripping behavior. Only the new DNS tools (dig, nslookup) use `skip_ansi_strip: true`.
- **`tempfile` promotion**: Moved from `[dev-dependencies]` to `[dependencies]` because `manifest.rs` uses `NamedTempFile` in production code for atomic writes. This is correct -- the dependency was already compiled in test builds, now it's also available in release builds where it's needed.

### 3. Intent vs Reality Check

- Commit `bb94833` ("feat(#182): index builder pipeline with incremental updates") matches the implementation: walk, classify in parallel, build sequentially, write manifest atomically.
- Commit `a619cee` ("refactor: migrate search.rs to search/ module") is a pure file relocation with no functional changes, as stated.
- Commit `a686e78` ("feat(#168): dig/nslookup DNS output compression") adds dig and nslookup support with correct wiring in dispatch, rewrite rules, and E2E tests.
- The `skip_ansi_strip` field on `InfraToolConfig` is correctly motivated: dig and nslookup use TABs as field separators, and `strip_ansi_escapes` would incorrectly remove them.

### 4. Incomplete Migration Check

- **Dispatch table sync**: Both `KNOWN_SUBCOMMANDS` array and the `dispatch()` match arm were updated to include `dig` and `nslookup`. No orphaned entries.
- **Rewrite rules sync**: `INFRA_RULES` count updated from 26 to 28, and `EXPECTED_RULE_COUNT` test constant updated accordingly. The integrity test `test_rule_count_matches_expected` would catch any mismatch.
- **All consumers updated**: The `rskim-search/src/lib.rs` doc comment was updated to reference the new module path (`cmd/search/mod.rs`).
- **All 4059 tests pass**, including existing regression tests and the new test `test_index_help_dispatches_to_index_not_parent` which guards against the `--help` interception bug.

### 5. Test Coverage Assessment

The PR adds comprehensive test coverage:
- 16 walk tests (discovery, file types, skip reasons, SHA-256, minification)
- 12 manifest tests (roundtrip, wrong-root, field_map encoding)
- 15 index pipeline tests (full build, incremental, force, mixed languages, max-files, argument validation)
- 6 DNS E2E rewrite tests
- 1 regression test for `skim search index --help` dispatch
