# Security Review Report

**Branch**: feat/183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25T18:30

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

- **NaN `half_life_days` bypasses `decay_weight` output contract in release builds** - `scoring.rs:58` (Confidence: 65%) — `debug_assert!(half_life_days > 0.0)` is stripped in release. If a direct caller passes `NaN` as `half_life_days`, the division produces `NaN` and `f64::NAN.clamp(0.0, 1.0)` returns `NaN`, violating the documented `[0.0, 1.0]` return contract. The primary caller `compute_file_risk_scores` is protected by `assert!` on line 99, so this only affects hypothetical direct callers of the public `decay_weight` API. The PR already added a NaN guard for `elapsed_days` — applying the same pattern to `half_life_days` would make the function self-consistent.

- **`FileRiskScores` derives `Deserialize` without range validation** - `types.rs:268` (Confidence: 60%) — The struct documents that both fields are in `[0.0, 1.0]`, but deriving `Deserialize` allows construction of arbitrary `f64` values (NaN, Infinity, negative, >1.0) from untrusted input. Currently `FileRiskScores` is only computed internally and serialized for JSON output — never deserialized from external sources — so this is not exploitable today. If a future consumer deserializes from external JSON, the invariant would not be enforced. Consider adding a note or a `#[serde(deserialize_with = ...)]` validator if external deserialization becomes a use case.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED

## Rationale

This PR has a minimal security surface. All code is pure computation with no I/O, no network access, no secrets, no user-facing input parsing, no file system writes, and no database queries. Key security-relevant observations:

1. **No injection vectors** — No SQL, shell, path traversal, or command injection surfaces exist. File paths use `to_string_lossy()` for display only, never for filesystem operations.

2. **Regex is safe from ReDoS** — The `is_fix_commit` regex uses the Rust `regex` crate which guarantees linear-time matching (no backtracking engine). The pattern `(?i)\b(fix|bug|hotfix|patch|revert)\b` is simple alternation with word boundaries.

3. **Numeric boundary enforcement** — The `compute_file_risk_scores` function uses `assert!` (not `debug_assert!`) to reject `half_life_days <= 0.0`, ensuring the precondition holds in both debug and release builds. The `decay_weight` function handles NaN `elapsed_days` explicitly (returns 1.0), positive infinity (returns 0.0 via clamp), and negative infinity (returns 1.0 via clamp). Output is always clamped to `[0.0, 1.0]` for well-formed inputs.

4. **No unsafe code** — No `unsafe` blocks anywhere in the changed files.

5. **No secrets or credentials** — No hardcoded tokens, API keys, or credentials. No environment variable reads.

6. **Serde derives are output-only** — `Serialize`/`Deserialize` on `FileRiskScores` is used for JSON serialization in CLI output. No deserialization from external/untrusted sources exists in the current codebase.

7. **Prior cycle fixes verified** — The Cycle 1 fix upgrading `debug_assert!` to `assert!` on the `compute_file_risk_scores` boundary is confirmed in place (line 99), addressing the most important security-relevant precondition enforcement.
