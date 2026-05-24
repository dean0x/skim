# Security Review Report

**Branch**: feat/177-sparse-ngram-algorithm -> main
**Date**: 2026-05-14
**PR**: #222

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none)

## Analysis Notes

### Scope of Changes

The PR adds a new `ngram` module to `rskim-search` (a pure library crate with no I/O) consisting of:

- `ngram.rs` (301 lines) -- Ngram newtype, document extraction, query extraction with border-weighted selectivity
- `ngram_tests.rs` (392 lines) -- 382 dedicated tests
- `lib.rs` (2 lines changed) -- module declaration and re-exports
- `weights.rs` -- cargo fmt reformatting only (no semantic changes)

### Security Properties Verified

1. **No unsafe code** -- Zero `unsafe` blocks, no `transmute`, no raw pointer manipulation, no `#[no_mangle]` FFI.

2. **No unwrap/panic in production code** -- All `.unwrap()` usage is confined to `ngram_tests.rs` (gated behind `#[cfg(test)]`). Production code uses `unwrap_or`, `map`, and safe iterator chains.

3. **No I/O or external access** -- Module imports only `std::collections::HashMap`, `std::fmt`, and internal `crate::weights`. No filesystem, network, process, or environment variable access. Consistent with the library's "no I/O" architecture documented in `lib.rs`.

4. **Bounded memory allocation** -- `HashMap::with_capacity` uses `bytes.len().min(256)` (line 188), capping initial allocation at 256 entries regardless of input size. The `u16` key space naturally limits the HashMap to at most 65,536 entries for any input.

5. **HashDoS resistance** -- Uses `std::collections::HashMap` with Rust's default SipHash-1-3 randomized hasher. Combined with the bounded `u16` key space, this is not exploitable.

6. **Safe indexing** -- `covered[pos + 1]` (line 272) is safe because `pos` originates from `bytes.windows(2).enumerate()`, guaranteeing `pos <= bytes.len() - 2`, so `pos + 1 <= bytes.len() - 1` is always a valid index into the `covered` vec of length `bytes.len()`.

7. **No integer overflow** -- The only `as` casts are `(self.0 >> 8) as u8` and `(self.0 & 0xFF) as u8` in `to_bytes()` (line 62), both of which are lossless truncations of already-masked u16 values. All other integer conversions use `u16::from(u8)` which is infallible widening. The `f64::from(f32) * f64::from(f32)) as f32` pattern (line 259) can lose precision but not overflow to undefined behavior.

8. **No secrets or credentials** -- No hardcoded secrets, API keys, tokens, or credentials anywhere in the changed files.

9. **No deserialization of untrusted data** -- The module operates on `&str` byte slices using safe iterator patterns. No serde, no parsing of structured formats.

10. **debug_assert only** -- The sorted-weights precondition check (lines 182-185, 235-238) uses `debug_assert!` which is stripped in release builds, so a malformed weights table cannot cause a panic in production (it would silently produce incorrect results via binary search, but not a crash or security issue).

11. **Snyk SAST scan** -- Zero issues found across the entire project.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 10/10
**Recommendation**: APPROVED

This is a pure, safe Rust library module with no I/O, no unsafe code, no unwraps in production paths, bounded allocations, and a naturally limited key space. The attack surface is effectively zero -- all functions accept `&str` and `&[(u16, f32)]` slices and return owned `Vec` values with no side effects. The code follows Rust's memory safety guarantees throughout and presents no security concerns.
