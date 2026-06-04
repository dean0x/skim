# Security Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**PR**: #269
**Date**: 2026-06-03_1834
**Cycle**: 2 (cross-cycle aware)

## Scope

Reviewed the security-relevant attack surface of the Wave 3c changes:
- `crates/rskim-search/src/ast_index/extract.rs` (new, 270 lines) — sole production code
- `crates/rskim-search/src/ast_index/mod.rs`, `src/lib.rs` — re-exports only
- `extract_tests.rs` and the lexical/temporal/index `*_tests.rs` edits — test-only

The function under review (`extract_ast_ngrams_with_weights` / `extract_ast_ngrams`)
is pure: no I/O, no SQL, no network, no filesystem, no auth, no secrets, no
deserialization of untrusted bytes, no `unsafe`. The only relevant security axis is
**denial-of-service via untrusted source code** (allocation, panic, integer overflow),
since `LinearNode` slices ultimately derive from attacker-controllable parsed source.

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

None.

## Pre-existing Issues (Not Blocking)

None.

## Cross-Cycle Verification (Cycle 1 fixes — confirmed holding)

Per Cross-Cycle Awareness, the following cycle-1 hardening was verified against current
code rather than re-raised:

- **u16 gap-fill overflow (PF-004)** — `extract.rs:159-160` uses
  `u32::from(node.depth) > u32::from(p) + 1`. The widen-before-add is present and
  load-bearing; no regression. (avoids PF-004)
- **Release-build panic safety on slice/index** — verified by construction, not asserts:
  - `ancestors[fill_start..d]` (line 167): gap-fill only runs when `node.depth > p+1`,
    so `d >= p+2` and `fill_start = p+1 < d`; both bounds ≤ `max_depth < table_len`.
  - `ancestors[d] = ...` (line 221): `d <= max_depth`, table sized `max_depth+1`.
    The three `debug_assert!`s document these invariants; the bounds hold in release
    without them. No panic path on adversarial depth sequences.
- **Ancestor table allocation bound** — `vec![None; max_depth+1]` where `max_depth`
  is `u16`, so ≤ 65536 × `size_of::<Option<u16>>()` ≈ 256 KiB worst case. Bounded.
- **HashMap capacity cap** — `nodes.len().min(1024)` present at line 145; prevents a
  100K node count driving oversized pre-allocation.

## Suggestions (Lower Confidence)

- **`count: u32` saturating semantics in the public DI core** — `extract.rs:198,214`
  (`entry.1 += 1`) (Confidence: 65%) — In the production path `count` is bounded by
  `DEFAULT_MAX_NODES` (100K), so overflow is impossible. However,
  `extract_ast_ngrams_with_weights` is a `pub` entry point accepting an arbitrary
  `&[LinearNode]`; a caller supplying >4.29B identical edges would wrap the `u32`.
  This is not realistically reachable (such a slice is itself infeasible to allocate)
  and would only produce a miscounted term frequency, not memory unsafety — hence a
  suggestion, not a finding. If desired, `saturating_add(1)` makes the bound explicit
  and matches the "every value has an explicit bound" reliability principle.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Security Score**: 10
**Recommendation**: APPROVED

No injection, auth, crypto, secret, deserialization, or SSRF surface exists in the diff.
The DoS-relevant paths (allocation, integer arithmetic, slice indexing) are bounded and
panic-safe by construction. All cycle-1 security hardening verified intact. No new issues
or regressions introduced in cycle 2.
