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

## Status history

| Date       | Status   | Note |
|------------|----------|------|
| 2026-07-17 | Proposed | OD-1 element reorder decided by user |
| 2026-07-17 | Accepted | OD-2 envelope key order decided by user |
| 2026-07-17 | Accepted | OD-3 cache churn accepted by user (no canonical_version marker) |
| 2026-08-09 | Active   | Implemented in rskim-align v0.1.0 (#306 Phase 1-5) |
