# Security Review Report

**Branch**: feat/186-query-engine -> main
**Date**: 2026-05-18T14:50

## Issues in Your Changes (BLOCKING)

No CRITICAL or HIGH security issues found in the changed lines.

## Issues in Code You Touched (Should Fix)

No security issues found in adjacent code.

## Pre-existing Issues (Not Blocking)

No CRITICAL pre-existing security issues detected.

## Suggestions (Lower Confidence)

- **Redundant validation in inner layer** - `query.rs:58-60` / `reader.rs:321-324` (Confidence: 70%) -- `BM25FConfig::validate()` is called both in `QueryEngine::search` and in `NgramIndexReader::search`. This is technically defense-in-depth (good), but if `QueryEngine` is the canonical trust boundary, the inner-layer call is redundant work. Not a vulnerability, just a design consideration for whether to remove the inner-layer check once all callers are guaranteed to go through `QueryEngine`.

- **`SearchQuery` derives `Deserialize` without size-limited deserialization** - `types.rs:273` (Confidence: 65%) -- `SearchQuery` derives `Deserialize`, and the `text` field is an unbounded `String`. If any future HTTP/RPC endpoint deserializes a `SearchQuery` directly from untrusted JSON, an attacker could submit a multi-gigabyte `text` field that would be fully allocated before `QueryEngine` ever checks `MAX_QUERY_BYTES`. The `QueryEngine` validation is correct for the current in-process usage, but does not protect against deserialization-time memory exhaustion. This is informational since no network endpoint exists today.

- **No validation of `limit`/`offset` ranges** - `query.rs:46-63` (Confidence: 60%) -- `QueryEngine` validates `text` length and `bm25f_config` but does not validate `limit` or `offset`. While the inner layer handles these safely (via `skip`/`take` on an iterator), extremely large `offset` or `limit` values are silently accepted. Not exploitable today since the reader clamps `limit` to `unwrap_or(20)`, but if a future layer allocates a `Vec::with_capacity(limit)` it could cause memory exhaustion.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED

## Analysis Notes

### What was reviewed

- `crates/rskim-search/src/lexical/query.rs` (new, ~76 lines) -- QueryEngine decorator
- `crates/rskim-search/src/lexical/query_tests.rs` (new, ~282 lines) -- 16 tests
- `crates/rskim-search/src/lexical/mod.rs` (modified, 2 lines) -- module + re-export
- `crates/rskim-search/src/lib.rs` (modified, 1 line) -- crate-level re-export

### Security strengths observed

1. **Trust boundary validation pattern is sound.** `QueryEngine` validates all untrusted input fields (`text` length, `bm25f_config` validity) before any index I/O occurs. This is the correct "parse at boundaries, trust internally" pattern.

2. **Byte-length check prevents oversized queries.** `query.text.len() > MAX_QUERY_BYTES` uses byte length (not character count), which is the correct metric for memory/resource bounding in Rust where `String::len()` returns byte count.

3. **Empty query short-circuit.** Returning `Ok(vec![])` for empty text avoids unnecessary index access and prevents potential edge cases in ngram extraction with empty strings.

4. **BM25F config validation rejects NaN and negative values.** The `cfg.validate()` call catches `NaN`, `Infinity`, and negative values for `k1`, `field_boosts`, and `field_b` -- preventing floating-point poisoning of scoring logic.

5. **No unsafe code.** The entire change uses safe Rust with no `unsafe` blocks.

6. **No panics in production paths.** The only `unwrap()` is in a doc comment example (which uses `no_run`). All production code returns `Result`.

7. **Decorator pattern preserves inner-layer invariants.** `QueryEngine` only adds validation -- it does not modify the query before forwarding, so inner-layer assumptions remain intact.

8. **Test coverage includes adversarial inputs.** Tests cover oversized queries, NaN config values, negative config values, empty strings, whitespace-only strings, single-character strings, and Unicode -- a solid boundary-testing suite.
