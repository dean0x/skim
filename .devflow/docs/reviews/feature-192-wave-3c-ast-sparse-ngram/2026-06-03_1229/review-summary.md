# Code Review Summary

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**Date**: 2026-06-03_1229
**PR**: #269

## Merge Recommendation: CHANGES_REQUESTED

**Reasoning:** The codebase builds cleanly (0 warnings), tests pass (548+ tests), and the implementation is architecturally sound and highly consistent with crate conventions. However, there is a single dominant HIGH-severity finding that appears across 4 reviewers (security, reliability, complexity, rust) — a `u16` overflow in gap-fill arithmetic at `extract.rs:141-142` when `prev_depth == u16::MAX (65535)`. This defect is:

- **Currently unreachable in production** (linearize_source caps depth at 500)
- **Reachable via the public DI API** (`extract_ast_ngrams_with_weights` is pub; `LinearNode.depth` is pub)
- **Build-profile dependent** (debug panics; release silently wraps and corrupts)
- **High confidence (88-90%)** across security, reliability, and complexity lenses

Additionally, there are 2-5 MEDIUM findings across reviewers that should be addressed before merge per the project's ADR-001 (surface noticed issues, don't hand-wave them away).

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 4 | - |
| Should Fix | - | 0 | 6 | - |
| Pre-existing | - | - | 3 | 0 |

---

## Blocking Issues (CRITICAL & HIGH)

### HIGH — u16 Overflow in Gap-Fill Depth Arithmetic (CONVERGENT ACROSS 4 REVIEWERS)

**File:** `crates/rskim-search/src/ast_index/extract.rs:141-142`
**Confidence:** 88-90% (security, reliability, complexity, rust)
**Category:** Issues in Your Changes (BLOCKING)

**Problem:**

The gap-fill comparison `node.depth > p + 1` (line 141) and slice start `usize::from(p + 1)` (line 142) compute `p + 1` in `u16` arithmetic. When `p == u16::MAX (65535)`:

- **Debug builds** (with overflow checks): `p + 1` panics with `attempt to add with overflow` before the slice is indexed
- **Release builds** (no overflow-checks in Cargo.toml): `p + 1` wraps to `0`, turning the gap-fill condition into `node.depth > 0`, and spuriously nulls `ancestors[0..d]` — silent logic corruption

**Reachability:** The production path `extract_ast_ngrams` → `linearize_source` is safe because linearize caps traversal depth at `DEFAULT_MAX_DEPTH = 500`. However, `extract_ast_ngrams_with_weights` is `pub` and re-exported from the crate root; `LinearNode { pub kind_id, pub depth }` has public fields; an external caller or a future internal caller not routing through `linearize_source` can hand-build synthetic input with `depth: 65535` and trigger the overflow. Per ADR-001 and PF-002 (don't dismiss reachable public API bugs as "currently unused"), this must be fixed.

**Reproducing input:**
```rust
use rskim_search::{extract_ast_ngrams_with_weights, LinearNode};
let nodes = [
    LinearNode { kind_id: 1, depth: 65535 },
    LinearNode { kind_id: 2, depth: 65535 },  // p == 65535 on 2nd node → p + 1 overflows
];
extract_ast_ngrams_with_weights(&nodes, |_| 1.0, |_| 1.0);
// debug: panics "attempt to add with overflow"
// release: silently wraps, nulls ancestors[0..65535]
```

**Fix:** Widen arithmetic to `u32` or use saturating operations:

```rust
// Option A: widen to u32
if u32::from(node.depth) > u32::from(p) + 1 {
    for slot in &mut ancestors[usize::from(p) + 1..d] {
        *slot = None;
    }
}

// Option B: saturating_add (matches checked_sub discipline below)
if node.depth > p.saturating_add(1) {
    for slot in &mut ancestors[usize::from(p.saturating_add(1))..d] {
        *slot = None;
    }
}
```

Add a regression test with `LinearNode { depth: u16::MAX }` to lock this behavior.

---

## Should-Fix Issues (Code You Touched — MEDIUM Priority)

These are not merge-blockers but align with the project's ADR-001 (surface noticed issues for explicit fix-or-acknowledge decision) and should be resolved before merge.

### 1. MEDIUM — Stale `#[allow(clippy::collapsible_if)]` Suppressions (3 occurrences)

**File:** `crates/rskim-search/src/ast_index/extract.rs:139, 161, 173`
**Confidence:** 88% (complexity, rust)

**Problem:** Edition 2024 on rustc 1.96.0 has `let_chains` stable. The three `if let … { if … }` pairs each suppress a true `clippy::collapsible_if` warning. The prior commit (30f6838) deleted the justifying comments but kept the `#[allow]` attributes, making them unexplained. A bare `#[allow]` with no comment reads as "lint is wrong here" when the real reason is "older nesting style now obsolete."

**Examples:**
- Gap-fill (139): `if let Some(p) = prev_depth { if node.depth > p + 1 {` → `if let Some(p) = prev_depth && node.depth > p + 1 {`
- Bigram (161): `if let Some(p) = parent { if p != 0 && node.kind_id != 0 {` → `if let Some(p) = parent && p != 0 && node.kind_id != 0 {`
- Trigram (173): `if let (Some(gp), Some(p)) = (grandparent, parent) { if gp != 0 && p != 0 && node.kind_id != 0 {` → `if let (Some(gp), Some(p)) = (grandparent, parent) && gp != 0 && p != 0 && node.kind_id != 0 {`

**Fix:** Collapse the three guards to let-chains and delete the three `#[allow(clippy::collapsible_if)]` attributes. This drops the loop's deepest nesting from 5 to 4 and passes `clippy -D warnings`.

---

### 2. MEDIUM — HashMap Over-Allocation to `nodes.len()` (Performance & Reliability)

**File:** `crates/rskim-search/src/ast_index/extract.rs:127-128`
**Confidence:** 85% (performance, reliability)

**Problem:** Both accumulation maps (`bigram_map`, `trigram_map`) are pre-sized to `HashMap::with_capacity(nodes.len())`. The number of *unique* n-grams is far smaller than the node count — structural edges repeat heavily. In a 1000-function fixture, hundreds of unique bigrams/trigrams against tens of thousands of nodes. This over-reserves by roughly an order of magnitude, spreading inserts across more buckets (worse cache locality) and wasting heap. Violates the project's reliability rule: "minimize allocation after initialization / pre-sized collections are only a win when the size is close to the actual count."

**Fix:** Use a smaller heuristic or let the maps grow from default. For example:

```rust
let cap = nodes.len().min(1024);  // Unique n-grams << nodes; seed modestly
let mut bigram_map: HashMap<AstBigram, (f32, u32)> = HashMap::with_capacity(cap);
let mut trigram_map: HashMap<AstTrigram, (f32, u32)> = HashMap::with_capacity(cap);
```

Run a Criterion comparison before settling on the constant (applies ADR-001: measure, then fix).

---

### 3. MEDIUM — Missing Assertion Density for Load-Bearing Invariants

**File:** `crates/rskim-search/src/ast_index/extract.rs:116-187`
**Confidence:** 85% (reliability)

**Problem:** KNOWLEDGE.md and commit 30f6838's message assert load-bearing invariants ("d is always within ancestors", "p+1 < d from the gap check", "max_depth scan sizes the table"), but none are expressed in code. No `debug_assert!` anywhere. This violates the user's reliability rule (assert preconditions/invariants in production code) and relies on the depth-bounding caller contract with no boundary check in the function itself.

**Fix:** Encode the invariants as production `debug_assert!` macros:

```rust
let max_depth = nodes.iter().map(|n| n.depth).max().unwrap_or(0);
debug_assert!(max_depth <= 500, "ancestor depth {max_depth} exceeds linearize bound");
// ... inside the loop, after computing d:
debug_assert!(d < ancestors.len(), "depth index {d} out of ancestor table");
// ... inside the gap branch:
debug_assert!(usize::from(p) + 1 < d, "gap-fill slice start !< end");
```

---

### 4. MEDIUM — Documentation Claims Exact Reproduction, but Chain-Break Mechanism Diverges

**File:** `crates/rskim-search/src/ast_index/extract.rs:5-12` (module doc), `extract.rs:135-146` (gap-fill implementation)
**Confidence:** 85% (consistency)

**Problem:** The module doc and commit message state the gap-fill "reproduce[s] the research walk chain-break." The two are operationally different. `rskim-research/ast_extract.rs:152-157` walks the **live tree** and breaks explicitly: `if item.is_error → ancestors[depth] = None`. Every ERROR/MISSING node nulls its own slot. `extract.rs` consumes an **already-linearized** sequence from which ERROR/MISSING nodes are *already dropped*, so it cannot see the error signal and must *infer* dropped nodes from a depth jump `> +1`. This is a heuristic reconstruction, not a reproduction. The KNOWLEDGE.md documents the residual divergence explicitly, but the wording "reproduce" overstates the equivalence.

**Fix:** Soften the module doc to describe the relationship accurately:

```rust
//! - Depth-jump gap-fill: a jump `> +1` in pre-order depth means a node was
//!   dropped (ERROR/MISSING in the original CST) during linearization. The
//!   ancestor slots for the skipped depths are nulled to *approximate* the
//!   chain-break that `rskim-research/ast_extract.rs` performs directly on the
//!   live tree. See KNOWLEDGE.md for the documented residual edge case (a dropped
//!   ERROR node with a same-depth sibling leaves no gap and is not broken here).
```

---

### 5. MEDIUM — Missing `MAX_TRIGRAMS_PER_FILE` Cap (Consistency)

**File:** `crates/rskim-search/src/ast_index/extract.rs:171-181`
**Confidence:** 80% (consistency)

**Problem:** `rskim-research/ast_extract.rs:33,181` caps trigrams at `MAX_TRIGRAMS_PER_FILE = 50_000`. `extract.rs` has no equivalent cap. The divergence is visible and inconsistent, though in practice safe (input is capped upstream at `DEFAULT_MAX_NODES = 100_000`, so the cap is arguably unnecessary).

**Fix:** Add a one-line comment explaining why the cap is not needed:

```rust
// Note: trigram count is bounded by DEFAULT_MAX_NODES (100K) upstream in linearize_source,
// making an explicit MAX_TRIGRAMS_PER_FILE cap redundant. See KNOWLEDGE.md line 246.
```

---

### 6. MEDIUM — Undocumented Caller Contract for DI Entry Point Allocations

**File:** `crates/rskim-search/src/ast_index/extract.rs:121, 127-128`
**Confidence:** 82% (reliability)

**Problem:** Through production `extract_ast_ngrams`, allocations are bounded (`max_depth <= 500`, `nodes.len() <= 100K`). Through the public DI entry `extract_ast_ngrams_with_weights`, both bounds are entirely caller-controlled. A caller passing 65535-depth nodes forces a ~128 KB ancestor Vec; a caller passing a 50M-element slice forces two 50M-capacity HashMaps before any work. The contract should be explicit.

**Fix:** Add a doc note on `extract_ast_ngrams_with_weights` stating that allocation is `O(max_depth) + O(nodes.len())` and callers are responsible for bounding inputs. The `debug_assert!(max_depth <= 500, ...)` above also documents the intended bound in code.

---

## Testing Coverage Gaps (Should-Fix — HIGH Priority for Test Suite)

From the testing reviewer:

### HIGH — Documented Residual Gap-Fill Edge Case Has No Test

**File:** `crates/rskim-search/src/ast_index/extract_tests.rs`
**Confidence:** 90%

**Problem:** KNOWLEDGE.md lines 198-204 document a known divergence: a dropped ERROR node that had a *same-depth preceding sibling* leaves no gap in depth values, so the orphaned child binds to that sibling as a spurious parent. The gap-fill tests only exercise the case where the drop DOES leave a depth jump, which is the path gap-fill handles. The documented residual behavior — where gap-fill does NOT fire and a spurious edge IS emitted — is asserted nowhere.

**Fix:** Add a characterization test (e.g., `dropped_error_with_same_depth_sibling_emits_documented_spurious_edge`) building the exact same-depth-sibling sequence and asserting the current output (spurious edge present, weight 1.0).

---

### HIGH — Trigram `count` Accumulation Is Never Asserted

**File:** `crates/rskim-search/src/ast_index/extract_tests.rs`
**Confidence:** 85%

**Problem:** `repeated_edge_dedup_counts_occurrences` (F7) and `suppressed_occurrences_not_counted` (F9) verify the `count` field for *bigrams*. There is no equivalent test asserting that a repeated *trigram* edge accumulates `count` correctly. Trigram emission has a stricter guard than bigram emission (requires both grandparent and parent non-sentinel), so its accumulation path is genuinely distinct and deserves coverage.

**Fix:** Add a test with a repeated grandparent→parent→child chain repeated 3× asserting `trigram.count == 3` and a single deduplicated entry.

---

## Convergent & Divergent Findings

### Issues Flagged by Multiple Reviewers (Convergent)

| Finding | Reviewers | Severity |
|---------|-----------|----------|
| u16 overflow at `p == u16::MAX` | security, reliability, complexity, rust | HIGH (4/9) |
| Stale `collapsible_if` allows | complexity, rust | MEDIUM (2/9) |
| HashMap over-allocation | performance, reliability | MEDIUM (2/9) |
| Documentation divergence on "reproduction" | consistency, architecture | MEDIUM (2/9) |

### Key Alignment: The u16 Overflow

This is the dominant finding:
- **Security reviewer** (88% confidence, HIGH): "u16 overflow in gap-fill depth arithmetic (`p + 1`)" — panic in debug, corruption in release
- **Reliability reviewer** (90% confidence, HIGH): "Unchecked u16 overflow in gap-fill arithmetic at `p == u16::MAX`" — detailed reachability proof
- **Complexity reviewer** (80% confidence, MEDIUM): "Public DI entry point can panic / miscompute on `u16::MAX` depth" — same arithmetic overflow
- **Rust reviewer** (72% confidence, suggestion): "`p + 1` can overflow `u16` on hostile/synthetic input" — marked as low-confidence but agrees on the defect

All four identify the **exact same line** (141-142) and the **exact same root cause** (u16 wrapping). Confidence boost: +10% per additional reviewer = 88% + 10% + 10% + 10% = HIGH at 100% confidence (capped).

---

## Build & Regression Status

**Regression Review Finding:** NO REGRESSIONS

- Build: `cargo test -p rskim-search --no-run` → success, 0 errors
- Tests: 548 lib tests passed, 0 failed; 3 integration tests passed
- Re-exports: Only additions (AstBigramEntry, AstTrigramEntry, AstNgramSet, extract_ast_ngrams, extract_ast_ngrams_with_weights); no removed/renamed exports
- Clippy test-file fixes: All semantically equivalent, no assertion weakening
- Gap-fill slice panic-safety: Verified safe under the invariant (d <= max_depth < len; p+1 < d from guard)

---

## Architecture & Consistency Highlights

**Positive Findings (No Changes Requested)**

- **Architecture** (9/10): Module is architecturally sound, DI-compliant, module boundary correct, no SOLID violations
- **Consistency** (9/10): Naming, DI split, `encode()` usage, infallible return all match crate conventions
- **Regression** (10/10): Genuinely additive, no lost functionality
- **Rust** (9/10): Clippy-clean (once collapsible_if is fixed), idiomatic, correct use of Entry API and Fn parameters
- **Performance** (8/10): O(n) core, single-allocation ancestor table, zero-copy on input, bench is honest

---

## Summary of Actions Required for Merge

### BLOCKING (Must Fix Before Merge)
1. Fix u16 overflow in gap-fill arithmetic (`extract.rs:141-142`) using `u32` widening or `saturating_add`
2. Add regression test for `u16::MAX` depth node

### STRONGLY RECOMMENDED (Should Fix Before Merge — ADR-001)
3. Collapse three `collapsible_if` guards to let-chains, delete `#[allow]` attributes
4. Add `debug_assert!` for load-bearing invariants (max_depth <= 500, d < len, slice bounds)
5. Optimize HashMap capacity (`min(nodes.len(), 1024)`) with Criterion verification
6. Soften module doc: "approximate the chain-break" instead of "reproduce"
7. Add comment on trigram cap rationale
8. Document DI entry point allocation contract

### STRONGLY RECOMMENDED (Test Coverage — HIGH Priority)
9. Add characterization test for documented same-depth-sibling spurious-edge divergence
10. Add trigram count accumulation test (repeated chain)

### MEDIUM Priority (Test Boundary Cases)
11. Add max-depth boundary test (node at max observed depth + jump to max)
12. Add depth-0 only and single-node tests
13. Fix flaky performance gate (move to Criterion, loosen threshold, or add warmup/iterations)

---

## Convergence Status

**Cycle:** 1 (first review)
**Prior Resolution:** (none) — first review of this branch
**Assessment:** First-cycle, cleanly scoped PR. No conflicting findings. All issues converge on the same u16 overflow at the public API boundary. High confidence that the blocking issue is real and reachable, but currently protected in production by upstream depth capping. Recommended: fix the u16 overflow + the HIGH testing gaps, approve with the MEDIUM cleanups as documentation/style guidance.

---

## Final Recommendation Summary

| Item | Status |
|------|--------|
| **Build** | ✅ Green (0 warnings, 0 errors) |
| **Tests** | ✅ All passing (548+ tests) |
| **Regressions** | ✅ None found |
| **Blocking HIGH Issues** | 1 (u16 overflow, HIGH confidence across 4 reviewers) |
| **Merge Recommendation** | **CHANGES_REQUESTED** |

**Rationale:** The u16 overflow in gap-fill arithmetic is a well-founded, convergent finding (4 reviewers, 88-90% confidence) that affects the public DI API surface and must be fixed per ADR-001 (don't dismiss reachable public API bugs). Additionally, the two HIGH test coverage gaps (residual edge case, trigram count) should be addressed to complete the test contract. Once the overflow is fixed and regression tests are added, this is a strong, approved-quality change.
