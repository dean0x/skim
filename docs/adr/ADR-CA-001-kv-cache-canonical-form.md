# ADR-CA-001: KV-Cache Canonical Form for skim proxy (#306)

**Date:** 2026-08-09  
**Status:** Accepted  
**Deciders:** User resolution 2026-07-17 (OD-1, OD-2, OD-3, OD-4)  
**Related:** `rskim-align/src/lib.rs` (AD-CA-1..AD-CA-13), plan `.devflow/docs/design/l3-wave3/2026-07-17_1916/306-kv-cache-alignment-plan.md`

---

## Context

`skim proxy` forwards LLM requests to Anthropic and OpenAI. Identical prompts with
differently-ordered JSON (tool key order, `tools` array element order, top-level
envelope key order) result in KV-cache misses on the provider side, even when the
content is semantically identical. This issue compounds in multi-turn conversations
where each turn appends messages but reuses the same tools and system prompt.

## Decision

**Canonical form v1 (in scope):**

1. **Within-object key sort** (AD-CA-2) — Every JSON object within `tools`, `system`
   (block form), and `functions` value spans is re-serialized with keys in ascending
   lexicographic order. This makes tool/schema definitions byte-identical regardless
   of the client's serialization key order.

2. **Tools/functions array element reorder** (AD-CA-12) — The `tools` (Anthropic) and
   `tools`/`functions` (OpenAI) arrays are sorted using a two-part total sort key:
   - Part 1: provider-shape-aware tool name (`name` for Anthropic/legacy-functions;
     `function.name` for OpenAI tools-array format).
   - Part 2: canonicalized-compact-bytes tie-break (full element re-serialized, for
     name-collision determinism).
   
   The reorder is verified lossless by `tools_arrays_set_equal` (multiset equality:
   element reorder is sanctioned; any drop, duplicate, or mutation fails the gate
   → whole-request passthrough).

3. **Envelope key order** (AD-CA-13) — Top-level (envelope) object keys are emitted
   in ascending lexicographic order. Value spans are re-emitted verbatim except for
   `tools` and `system`, which carry their own canonicalization.

4. **`cache_control` marker injection** (AD-CA-4/5) — For Anthropic bodies only:
   up to 2 `{"type":"ephemeral"}` markers injected at stable structural positions:
   - Last tool object (after canonical key sort + element reorder).
   - Last block-form system text block.
   
   Injection budget: `min(eligible_positions, 4 − client_marker_count)`, capped at 2
   for v1. If the total client marker count ≥ 4, zero skim markers are injected.

5. **Fail-open doctrine** — Every error condition (parse failure, duplicate key,
   depth > 32, self-verify failure, set-equality failure) returns the original input
   bytes unmodified (SHA-256-equal). Partial output is never emitted.

6. **AD-CA-7 triple self-verify** — Three independent checks guard each aligned body:
   - **Reorder path**: `tools_arrays_set_equal(original_tools, canonical_reordered_tools) == true`.
   - **Envelope path**: output `messages` span byte-identical to input `messages` span.
   - **Injection path**: each injected span verified against the pre-injection canonical bytes.

## Consequences

### Accepted: one-time provider-cache warm on skim upgrade (OD-3)

Upgrading to a skim version that includes this alignment causes **one cache miss** per
unique request shape (tool set + system prompt combination). The provider has not seen
the new canonical form before.

**Why this is accepted:**
- After the first turn in the new canonical form, subsequent identical requests hit the
  cache as normal.
- The long-term benefit (cache hits on every aligned turn) outweighs the one-time miss.
- `--no-cache-align` lets operators opt out during the warm-up period.
- No `canonical_version` marker is injected — adding a version field would itself
  invalidate the cache on every future skim upgrade, a worse tradeoff.

**Mitigation:** Users can run `skim proxy --no-cache-align` until the warm-up period
passes (typically one request per unique tool set). `SKIM_PASSTHROUGH=1` bypasses all
transforms entirely.

### Accepted: element reorder visible to the model

The canonical element reorder of the `tools`/`functions` array changes the ORDER in
which tools are presented to the model. The content of each tool is unchanged (verified
by `tools_arrays_set_equal`). For models that treat tool order as a hint for selection,
this may subtly change behavior on the first aligned request. This is documented behavior
and accepted as the cost of KV-cache alignment.

### Not in scope (v1)

- Semantic-equivalence number re-formatting (e.g. `1e3 → 1000`): explicitly **not** done.
  Numbers are compared and emitted as raw token bytes to avoid cache-key drift from
  floating-point normalization.
- Message content canonicalization: the entire `messages` value span is preserved
  byte-identical. No message order is changed.
- Metadata field normalization: `model`, `metadata`, authorization material are
  byte-identical after alignment.

## Canonical form specification (normative)

```
Given a valid Anthropic or OpenAI request body JSON object:

1. Parse the top-level object; fail-open on any error.
2. For each value span in {tools, functions, system}:
   a. Parse the span as JSON.
   b. Recursively sort all object keys (lexicographic ascending).
   c. For tools/functions: apply element sort (two-part key: name, then bytes).
   d. Re-emit in compact form (no whitespace).
   e. Verify: tools_arrays_set_equal(original, canonical) == true.
3. Re-emit the top-level object with keys in lexicographic ascending order.
   - messages value span: copy byte-identical from input (no modification).
   - tools/system spans: use canonical spans from step 2.
4. For Anthropic: inject cache_control markers at stable positions.
5. Triple self-verify (AD-CA-7); fail-open on any verification failure.
```

## AC17 gate history

The `align_anthropic_64tools` CI bench gate guards worst-case latency for the
64-tool × depth-8 Anthropic fixture. The gate uses Criterion's **upper bound of
the 95% confidence interval of the mean** (the third value in
`time: [low mean high]`), which is a conservative ceiling well below p99.

### What profiling showed (before R6 optimisation)

Stage micro-benchmarks (commit 3c7318d6) isolated each pipeline stage. The
dominant cost was Stage E (`tools_arrays_set_equal`) — two full `parse_raw_node`
tree builds (one per array) scaled O(n × depth) with 64 tools at depth 8. This
accounted for approximately 79% of total runtime. Stage B
(`sort_tools_array` / canonicalization) was the remaining hot path. All other
stages (spans, SHA-256, volatile scan, marker injection) were sub-millisecond.

### R6 optimisation (commit 7b0ed407, 2026-08-14)

Eliminated two redundant `parse_raw_node` calls: the canonical array is parsed
once before the reorder-gate loop and threaded into both AD-CA-7 gates, removing
the O(n × depth) re-parse that had been repeated for each gate independently.

### Measured results (2026-08-14, 3 runs, macOS Apple Silicon, release build)

**Before R6** (bench_r1/r2/r3, Aug 12 2026):

| Run | low       | mean      | high (upper bound) |
|-----|-----------|-----------|-------------------|
| 1   | 7.6538 ms | 7.6885 ms | 7.7413 ms         |
| 2   | 7.8935 ms | 7.9254 ms | **7.9624 ms**     |
| 3   | 7.7457 ms | 7.7881 ms | 7.8474 ms         |

Worst upper bound before R6: **7.9624 ms**

**After R6** (r6bench1/r6bench2/r6bench3, Aug 14 2026):

| Run | low       | mean      | high (upper bound) |
|-----|-----------|-----------|-------------------|
| 1   | 6.6585 ms | 6.6736 ms | 6.6879 ms         |
| 2   | 6.7471 ms | 6.7662 ms | 6.7854 ms         |
| 3   | 6.8038 ms | 6.8776 ms | **6.9696 ms**     |

Worst upper bound after R6: **6.9696 ms**

Improvement: 12.5% on upper bound, 13.1% on mean. Noise floor for this
fixture is 4.03%; the improvement is 3× the noise floor and is a real finding.

### Gate derivation

- Measured worst upper bound (3 runs): **6.9696 ms**
- Headroom: +25% (well above the 4.03% noise floor so CI does not flap)
  → 6.9696 × 1.25 = 8.712 ms → rounded up to **9.0 ms**
- Hard ceiling: #303 proxy budget is 10 ms; 9.0 ms < 10 ms ✓
- **New CI gate: `< 9.0 ms`** (replaces the prior 5.0 ms unmeasured halving)

The prior 5.0 ms gate (and the 2.5 ms figure that appeared in early planning)
were unmeasured halvings of the #303 proxy latency bar — no empirical measurement
backed them. They were replaced by this grounded derivation per ADR-003 / PF-005.

## Status history

| Date       | Status   | Note |
|------------|----------|------|
| 2026-07-17 | Proposed | OD-1 element reorder decided by user |
| 2026-07-17 | Accepted | OD-2 envelope key order decided by user |
| 2026-07-17 | Accepted | OD-3 cache churn accepted by user (no canonical_version marker) |
| 2026-08-09 | Active   | Implemented in rskim-align v0.1.0 (#306 Phase 1-5) |
| 2026-08-14 | Active   | R6 parse-once opt; AC17 gate re-baselined to 9.0 ms from measured 6.9696 ms worst upper bound |
