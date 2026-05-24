# Rust Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**`unsafe` block in `sha256_hex` can be replaced with safe code** - `crates/rskim/src/cmd/search/walk.rs:332`
**Confidence**: 90%
- Problem: `String::from_utf8_unchecked` is used with a `// SAFETY:` comment, but the invariant (all bytes are ASCII hex) can be trivially enforced by the safe `String::from_utf8(hex).unwrap()` or even `String::from_utf8(hex).expect("hex nibbles are always valid UTF-8")`. The `unsafe` block is correct but unnecessary -- the performance difference between the safe and unsafe paths here is negligible (one bounds check on 64 bytes, called once per file not once per byte), and the safe version provides the same guarantee with compiler-verified soundness.
- Fix: Replace the unsafe block with:
  ```rust
  // NIBBLES contains only ASCII hex characters, so hex is always valid UTF-8.
  String::from_utf8(hex).expect("hex nibbles are always valid UTF-8")
  ```

### MEDIUM

**`project_root_hash` truncates SHA-256 to 8 bytes (16 hex chars) -- collision risk for multi-project caching** - `crates/rskim/src/cmd/search/index.rs:306-311`
**Confidence**: 80%
- Problem: Using only 8 bytes (64 bits) of the SHA-256 digest as a directory name creates a non-trivial collision probability via the birthday paradox. With ~65,000 distinct project roots, the probability of at least one collision reaches ~50%. While 65K projects on a single machine is unlikely today, the truncation is overly aggressive and could cause one project's index to silently overwrite another's if they happen to collide. The format is baked into the cache path structure, so changing it later is a cache-invalidation event.
- Fix: Use 16 bytes (32 hex chars) instead of 8 bytes. This pushes the 50% collision threshold to ~2^64 distinct roots, well beyond any realistic scenario:
  ```rust
  for byte in digest.iter().take(16) {
      write!(hex, "{byte:02x}").unwrap();
  }
  ```
  Update `String::with_capacity(16)` to `String::with_capacity(32)`.

**`tempfile` promoted from dev-dependency to production dependency** - `crates/rskim/Cargo.toml:44`
**Confidence**: 85%
- Problem: `tempfile` was moved from `[dev-dependencies]` to `[dependencies]` to support the atomic manifest write in `manifest.rs`. This is correct for the functionality, but `tempfile` adds a runtime dependency (and transitive deps like `fastrand`, `once_cell`) to every user's binary. Since `tempfile` is only used in one place (`manifest.rs:221` for `NamedTempFile::new_in`), the trade-off is reasonable but worth acknowledging. Consider whether `std::fs::write` to a `.tmp` file with a manual `std::fs::rename` would avoid the dependency (at the cost of reimplementing atomic write logic that `tempfile` handles correctly, including cleanup on drop).
- Fix: This is an intentional design choice and the atomic semantics are valuable. No code change needed, but document the rationale in a brief comment near the `Cargo.toml` entry (e.g., `# Required for atomic manifest writes in cmd/search/manifest.rs`).

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`SkipReason` fields marked `#[allow(dead_code)]`** - `crates/rskim/src/cmd/search/types.rs:61` (Confidence: 65%) -- The `dead_code` suppression is applied to the entire enum, but the individual variant fields (`path`, `size`, `error`) are the dead parts. If these fields are only used for `Debug` output, consider implementing `Display` for `SkipReason` and using it in the summary output, which would make the fields genuinely used and remove the need for the suppression.

- **`is_minified` integer division could silently accept very short probes** - `crates/rskim/src/cmd/search/walk.rs:316` (Confidence: 70%) -- `probe.len() / newline_count > MINIFY_AVG_LINE_BYTES` uses integer division. If probe length is, say, 501 bytes with 1 newline, this correctly identifies it. But for files shorter than `MINIFY_AVG_LINE_BYTES` (500 bytes) with at least one newline, the division yields 0 or a small number and the check correctly passes. The zero-newline branch on line 314 already handles the degenerate case. The logic is correct but the two branches have subtly different semantics (zero newlines checks `probe.len()` directly, nonzero checks average). A unified `probe.len() / (newline_count + 1)` would be more consistent but changes behavior.

- **`write!(hex, ...)` unwrap in `project_root_hash` vs nibble table in `sha256_hex`** - `crates/rskim/src/cmd/search/index.rs:309` vs `walk.rs:324-332` (Confidence: 75%) -- Two hex-encoding functions exist in the same module family with different approaches: `project_root_hash` uses `write!` with `.unwrap()`, while `sha256_hex` uses a manual nibble table with `unsafe`. Consider consolidating into a single hex-encoding helper used by both, preferring the safe approach.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code demonstrates strong Rust idioms overall: proper use of `Result` propagation with `?`, typed error enums (`ReadOutcome`, `SkipReason`) instead of string-matching on error messages, compile-time assertions for invariants, `LazyLock` for static regex compilation, atomic file writes via `tempfile`, and thoughtful TOCTOU mitigation in the file walker. The parallel classification pipeline using rayon with sequential accumulation is architecturally sound. The `#[must_use]` annotation on `effective_max_files` and the `u32::try_from` guard against FileId overflow show attention to correctness.

Conditions for approval:
1. Remove the `unsafe` block in `sha256_hex` -- the safe alternative has identical correctness guarantees with negligible performance cost.
2. Consider (but not blocking) increasing the hash truncation length in `project_root_hash` from 8 to 16 bytes to reduce collision probability for the cache directory naming scheme.
