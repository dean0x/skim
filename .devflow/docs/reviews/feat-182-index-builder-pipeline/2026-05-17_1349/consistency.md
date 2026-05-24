# Consistency Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**SKIM_DEBUG check bypasses centralized debug module** - `crates/rskim/src/cmd/search/index.rs:196`
**Confidence**: 95%
- Problem: The index pipeline reads `SKIM_DEBUG` directly via `std::env::var_os("SKIM_DEBUG").is_some()`, but the codebase has a centralized `crate::debug::is_debug_enabled()` mechanism (initialized once in `main()` via `init_debug_from_env()`). The centralized module also checks truthiness (`1`/`true`/`yes`) while the index.rs check only checks presence — setting `SKIM_DEBUG=false` would trigger debug output in the index pipeline but not elsewhere. Every other module in the codebase (`heatmap/args.rs`, `git/show.rs`, `analytics/mod.rs`, `output/mod.rs`, `discover.rs`) uses the centralized `crate::debug::is_debug_enabled()`.
- Fix: Replace `std::env::var_os("SKIM_DEBUG").is_some()` with `crate::debug::is_debug_enabled()`. This is also a pure atomic load (no syscall), so the stated optimization goal of hoisting the env-var check is already satisfied:
```rust
let debug_enabled = crate::debug::is_debug_enabled();
```

### MEDIUM

**Inconsistent hex encoding approaches within the same PR** - `crates/rskim/src/cmd/search/walk.rs:323-332`, `crates/rskim/src/cmd/search/index.rs:302-311`
**Confidence**: 85%
- Problem: Two functions in the same PR encode SHA-256 digests as hex strings using different approaches. `sha256_hex()` in walk.rs uses a nibble lookup table with `unsafe { String::from_utf8_unchecked }`, while `project_root_hash()` in index.rs uses `write!(hex, "{byte:02x}").unwrap()`. The `unsafe` block in walk.rs is the only occurrence of `from_utf8_unchecked` in the entire codebase. While the safety invariant is correctly maintained (the NIBBLES table only contains ASCII hex chars), the inconsistency within a single PR is notable.
- Fix: Either use the same approach in both functions (the `write!` approach in index.rs is consistent with the rest of the codebase's avoidance of `unsafe`), or extract a shared hex-encoding helper. If the nibble table is preferred for hot-path performance in `sha256_hex`, add a brief comment justifying why the approach differs from `project_root_hash`.

**`is_tree_sitter_language()` duplicates logic available on `Language` type** - `crates/rskim/src/cmd/search/walk.rs:299-301`
**Confidence**: 82%
- Problem: The `is_tree_sitter_language()` function hardcodes `!matches!(lang, Language::Json | Language::Yaml | Language::Toml)`, which is the negation of `Language::is_serde_based()` from `rskim-core/src/types.rs`. However, the semantics are not identical: Markdown is tree-sitter based but sometimes treated as passthrough. The current function's intent (exclude serde-based languages from minification check) is correct and matches `!lang.is_serde_based()`. Using the existing method would be more maintainable — if a new serde-based language is added, only `is_serde_based()` needs updating.
- Fix: Replace the local function with the existing method:
```rust
// Before
fn is_tree_sitter_language(lang: Language) -> bool {
    !matches!(lang, Language::Json | Language::Yaml | Language::Toml)
}

// After - use existing method from Language
// In walk_and_read: replace is_tree_sitter_language(lang) with !lang.is_serde_based()
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`Language::name()` vs `Language::as_str()` naming convention** - `crates/rskim-core/src/types.rs:102,129` (Confidence: 70%) — The new `as_str()` method returns lowercase serialization-safe identifiers while the existing `name()` returns display-friendly names (e.g., "C++" vs "cpp"). The distinction is semantically sound, but `Mode::name()` already returns lowercase identifiers ("structure", "signatures"), which is the same role as `Language::as_str()`. Consider whether `Language::name()` should have been named `display_name()` or whether `as_str()` should follow the `name()` convention. Not blocking since both are documented and the PR explicitly calls out the difference.

- **`SkipReason` enum uses `#[allow(dead_code)]` on the entire enum** - `crates/rskim/src/cmd/search/types.rs:61` (Confidence: 65%) — The `dead_code` allow is applied to the whole enum with a comment saying "Fields are for diagnostic/debug output via {:?}". Some variants like `CapReached` are actively matched against in tests. The allow is needed only because SkipReason fields are constructed but never individually read (they're printed via Debug). This is fine but diverges from the codebase pattern where `#[allow(dead_code)]` is applied to individual fields/variants rather than entire types.

- **Help text inconsistency: `println!` vs `eprintln!` for "not implemented"** - `crates/rskim/src/cmd/search/mod.rs:47,56` (Confidence: 60%) — The help text uses `println!` (stdout) while the "not yet implemented" message uses `eprintln!` (stderr). This is actually correct behavior (help to stdout, errors to stderr), but worth noting that the help function uses raw `println!` while many other subcommands use a `print_help()` function that also writes to stdout.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The new search module follows established codebase conventions well overall: `pub(super)` visibility for module-internal APIs, `anyhow::Result` for error handling, `#[path = "..._tests.rs"]` for co-located test files (matching rskim-search crate), section separators, and documentation style. The main consistency gap is the SKIM_DEBUG bypass of the centralized debug module, which introduces a semantic difference (presence check vs truthiness check) and an unnecessary syscall where an atomic load would suffice. The two hex encoding approaches within a single PR also merit harmonization.
