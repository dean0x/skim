# Consistency Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**Mixed error types within the same module (`std::io::Result` vs `anyhow::Result`)** - `manifest.rs:109`, `manifest.rs:205`
**Confidence**: 88%
- Problem: `FileManifest::load()` returns `std::io::Result<Self>` while `FileManifest::save()` returns `anyhow::Result<()>`. Similarly, `walk.rs` functions return `std::io::Result` while `index.rs` functions return `anyhow::Result`. The rest of the `cmd/` crate uses `anyhow::Result` uniformly for all fallible operations (see `heatmap/mod.rs`, `discover.rs`, `stats.rs`, etc.). Mixing error types forces callers to handle two error domains and is inconsistent with established project patterns.
- Fix: Unify to `anyhow::Result` for all public-facing functions in the search module, matching the codebase convention:
```rust
// manifest.rs
pub(super) fn load(project_root: PathBuf, cache_dir: PathBuf) -> anyhow::Result<Self> {
    // ... same logic, but anyhow wraps io::Error automatically via ?
}

// walk.rs
pub(super) fn discover_project_root(start: &Path) -> anyhow::Result<PathBuf> { ... }
pub(super) fn walk_and_read(root: &Path, max_files: usize) -> anyhow::Result<(Vec<ReadFile>, Vec<SkipReason>)> { ... }
```

### MEDIUM

**SHA-256 hex encoding uses inconsistent pattern** - `walk.rs:231-239`, `index.rs:277-286`
**Confidence**: 85%
- Problem: The new code defines two separate functions (`sha256_hex` in walk.rs and `project_root_hash` in index.rs) that both manually iterate bytes with `write!(hex, "{byte:02x}")`. The existing codebase (`cache.rs:140`, `integrity.rs:25`) uses the idiomatic `format!("{:x}", hasher.finalize())` one-liner pattern. This creates two patterns for the same operation.
- Fix: Align with the existing codebase pattern:
```rust
// walk.rs
pub(super) fn sha256_hex(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

// index.rs
fn project_root_hash(canonical_root: &Path) -> String {
    let input = canonical_root.to_string_lossy();
    let digest = format!("{:x}", Sha256::digest(input.as_bytes()));
    digest[..16].to_string()
}
```

**Duplicated `encode_field_map`/`decode_field_map` in test file** - `manifest_tests.rs:32-46`
**Confidence**: 90%
- Problem: `manifest_tests.rs` redefines `encode_field_map` and `decode_field_map` as private functions, duplicating the implementations already exported as `pub(super)` from `manifest.rs:243-264`. Since the test module is loaded via `#[path = "manifest_tests.rs"] mod tests;`, `super::encode_field_map` and `super::decode_field_map` are directly accessible.
- Fix: Remove the duplicate definitions and import from parent:
```rust
// manifest_tests.rs - remove lines 32-46 and use super:: imports
use super::{FileManifest, ManifestEntry, encode_field_map, decode_field_map};
```

**Argument parse errors propagated raw instead of user-friendly message** - `index.rs:65`
**Confidence**: 82%
- Problem: `index::run()` propagates parse errors via `let config = parse_args(args)?;` which bubbles the raw `anyhow::Error` up to the CLI. The established pattern in `heatmap/mod.rs:52-59` catches parse errors and prints a user-friendly message with a help hint before returning `ExitCode::FAILURE`. Since `search/mod.rs:42` also propagates via `return index::run(rest);`, parse errors like "unknown argument: --typo" will not follow the expected UX pattern.
- Fix: Match the heatmap pattern:
```rust
// index.rs
pub(super) fn run(args: &[String]) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let config = match parse_args(args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skim search index: {e}");
            eprintln!("Run `skim search index --help` for usage.");
            return Ok(ExitCode::FAILURE);
        }
    };
    // ...
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Test file organization diverges from codebase convention** - `index_tests.rs`, `manifest_tests.rs`, `walk_tests.rs`
**Confidence**: 80%
- Problem: The new search module uses `#[path = "..._tests.rs"]` to split tests into separate files. No other module in `crates/rskim/src/cmd/` uses this pattern -- all other modules either inline tests at the bottom of the source file (e.g., `discover.rs:609`, `stats.rs:643`, `learn.rs:727`) or keep them in the same file. While `#[path]` is valid Rust, it introduces a novel convention that may confuse contributors who expect the established inline pattern.
- Fix: This is a style choice that does not break anything. If the team prefers the new pattern for large test suites, document it. Otherwise, move tests inline to match existing convention. Given the test volume (30+ tests across 3 files), the separate-file approach is a reasonable evolution -- but note it as a new pattern being introduced.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`SkipReason::NonUtf8` misattributes I/O errors** - `walk.rs:168-170` (Confidence: 72%) -- `fs::read_to_string` can fail for reasons other than non-UTF-8 content (e.g., permission denied after metadata succeeded). The catch-all maps all errors to `NonUtf8`, which could confuse diagnostic output.

- **`--index-dir` flag undocumented in help** - `index.rs:293-316` (Confidence: 65%) -- The `--index-dir` flag is parsed in `parse_args` (line 102) but not listed in `print_help()`. The comment says "Internal/test flag" which may justify omission, but other internal flags in the codebase are typically documented with a `(internal)` note or not accepted at all in production builds.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 3 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The new search module is well-structured and follows most codebase conventions (section separators, module layout with types.rs, doc comments, `anyhow` usage in entry points). The main consistency gaps are the mixed error return types (`std::io::Result` vs `anyhow::Result`), divergent SHA-256 hex encoding pattern, duplicated test helpers, and missing user-friendly parse error handling. None are critical, but fixing the error type inconsistency and the duplicate test helpers would bring the code fully in line with established patterns.
