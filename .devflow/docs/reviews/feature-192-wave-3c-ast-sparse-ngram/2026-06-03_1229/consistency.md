# Consistency Review Report

**Branch**: feature/192-wave-3c-ast-sparse-ngram -> main
**PR**: #269
**Date**: 2026-06-03 12:29
**Focus**: Consistency (naming, error-handling pattern, DI pattern, encode() usage, research-walk fidelity)

## Scope

Reviewed `crates/rskim-search/src/ast_index/extract.rs` (NEW, 236 lines) against its sibling
modules (`linearize.rs`, `ngram.rs`), the lexical `ngram.rs` extractor, and the
`rskim-research/src/ast_extract.rs` walk it claims to reproduce. Pattern skill
`devflow:consistency` loaded. Decisions context (ADR-001, PF-002, PF-003) and the
`ast-index` feature knowledge applied.

Headline: the module is highly consistent with crate conventions. Naming, the DI split,
`encode()` usage, and the infallible return are all confirmed to match established siblings.
No blocking issues. Two MEDIUM observations about behavioral fidelity to the research walk,
plus low-confidence notes.

---

## Issues in Your Changes (BLOCKING)

None.

---

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Documentation claims exact reproduction of the research walk, but the chain-break mechanism diverges** — `extract.rs:5-12`, `extract.rs:135-146`
**Confidence**: 85%
- Problem: The module doc and commit message state the gap-fill "reproduce[s] the research walk
  chain-break." The two are NOT operationally equivalent. `rskim-research/ast_extract.rs:152-157`
  walks the **live tree** and breaks the chain by an explicit, exact signal: `if item.is_error`
  → `ancestors[depth] = None`. Every ERROR/MISSING node nulls its own slot, deterministically.
  `extract.rs` consumes an **already-linearized** `Vec<LinearNode>` from which `linearize.rs`
  has *already dropped* ERROR/MISSING nodes (`linearize.rs` invariant
  `node_count == nodes.len() + error_count`). It therefore cannot see the error signal and must
  **infer** dropped nodes from a depth jump `> +1` (`extract.rs:141`). This is a heuristic
  reconstruction, not a reproduction. The feature knowledge itself documents the residual
  divergence (KNOWLEDGE.md "Residual documented divergence": a dropped ERROR node with a
  same-depth preceding sibling leaves no depth gap, so the heuristic misses the break and emits
  a spurious edge — a case the research walk handles correctly).
- Impact: Low runtime risk (the spurious edge almost always hits the 1.0 default weight, per the
  documented analysis), but the wording "reproduce" / "research walk chain-break" overstates the
  equivalence and could mislead a future maintainer into assuming bit-identical output. This is a
  documentation-consistency issue, not a logic bug. Per ADR-001 (flag noticed issues honestly)
  and PF-002 (don't hand-wave), it is recorded here rather than dropped.
- Fix: Soften the module doc to describe the relationship accurately, e.g.:
  ```
  //! - Depth-jump gap-fill: a jump `> +1` in pre-order depth means a node was
  //!   dropped (ERROR/MISSING in the original CST) during linearization. The
  //!   ancestor slots for the skipped depths are nulled to *approximate* the
  //!   chain-break that `rskim-research/ast_extract.rs` performs directly on the
  //!   live tree. See KNOWLEDGE.md for the documented residual edge case (a dropped
  //!   ERROR node with a same-depth sibling leaves no gap and is not broken here).
  ```
  No code change required — the divergence is intentional and documented; only the claim of
  exact reproduction needs tempering.

**Trigram output has no per-file cap, unlike the research walk** — `extract.rs:171-181`
**Confidence**: 80%
- Problem: `rskim-research/ast_extract.rs:33,181` caps trigrams at
  `MAX_TRIGRAMS_PER_FILE = 50_000` as an explicit memory guard. `extract.rs` has no equivalent
  cap. The two modules otherwise share constants (both reference `MAX_FILE_SIZE`,
  `MAX_FILE_SIZE_LARGE`, `AstWalkConfig::DEFAULT_MAX_*`), so the omission is a visible deviation.
- Impact: In practice this is **bounded and safe**: input is capped upstream at
  `DEFAULT_MAX_NODES = 100_000` nodes (enforced in `linearize.rs`/`AstWalkConfig`), the
  `HashMap` deduplicates by key, and `count: u32` cannot overflow because total emissions ≤ node
  count ≤ 100K (the entry doc-comment at `extract.rs:55-57` states this invariant). So the cap
  is arguably unnecessary here. The inconsistency is the concern, not a defect.
- Fix: Either (a) add a one-line comment noting why no `MAX_TRIGRAMS_PER_FILE` cap is needed
  (upstream `DEFAULT_MAX_NODES` bound makes it redundant) to pre-empt the "why does research cap
  but this doesn't?" question, or (b) leave as-is — the upstream bound is a legitimate
  justification for the deviation. Recommend (a): a comment costs nothing and documents the
  intentional divergence.

---

## Pre-existing Issues (Not Blocking)

None identified in the consistency dimension. The pre-existing clippy fixes bundled into the
first commit (`manual_range_contains`, `cloned_ref_to_slice_refs`, `field_reassign_with_default`,
`single_match`) move the touched test files *toward* crate consistency and align with ADR-001
(fix noticed issues). No regressions observed in the diffed test files.

---

## Consistency Confirmations (no action needed)

These were the primary review targets from the brief; all pass:

- **Naming vs siblings**: PASS. `extract_ast_ngrams` / `extract_ast_ngrams_with_weights` mirror
  the lexical sibling `ngram.rs:188,215` exactly (`extract_ngrams` / `extract_ngrams_with_weights`).
  Types `AstBigramEntry` / `AstTrigramEntry` / `AstNgramSet` follow the `Ast*` PascalCase
  convention of `ngram.rs` (`AstBigram`, `AstTrigram`). `NodeKindId` reused from the shared
  parent module (`mod.rs:48`), not redefined — matches `linearize.rs:32` and `ngram.rs:29`.
- **DI pattern**: PASS. The `_with_weights` DI core + thin production wrapper split is the
  identical shape used by lexical `ngram.rs` (`extract_ngrams_with_weights` core,
  `extract_ngrams` wrapper) and is explicitly documented as "the project's dependency-injection
  convention" in KNOWLEDGE.md.
- **Error-handling / Result pattern**: PASS and justified. The crate convention is
  `Result<T> = std::result::Result<T, SearchError>` (`types.rs:628`), used by fallible I/O paths
  like `linearize_source`. `extract.rs` is **infallible** (returns `AstNgramSet` directly). This
  is consistent — the lexical sibling `extract_ngrams` is also infallible (`-> Vec<(Ngram, f32)>`),
  and KNOWLEDGE.md states all n-gram encoding/extraction is pure with "the one error path"
  (`SearchError::Ast`) confined to `linearize_source`. Pure in-memory transforms over already-
  validated input correctly do not return `Result`. The `#[must_use]` attributes
  (`extract.rs:104,221`) match the crate's Rust convention for value-returning functions.
- **`encode()` usage**: PASS. `extract.rs:164,176` use `AstBigram::encode` / `AstTrigram::encode`,
  never `from_raw`. This respects the KNOWLEDGE.md anti-pattern: external callers must use
  `encode()`; `from_raw` is `pub(crate)` for internal weight-table iteration only. Correct.
- **Gap-fill slicing panic-safety** (recent commit 30f6838): VERIFIED SAFE. The simplified
  `ancestors[usize::from(p + 1)..d]` slice (`extract.rs:142`) cannot panic: the loop only runs
  when `node.depth > p + 1` so `p+1 < d` (non-empty, ordered range); `d = node.depth as usize`
  and `ancestors` is sized `max_depth + 1` where `max_depth` is the max over all `node.depth`,
  so `d <= max_depth < ancestors.len()`. `p <= prev_depth <= max_depth <= 500` rules out the
  `p + 1` u16 overflow. Parent/grandparent resolution via `checked_sub` (`extract.rs:151,156`)
  safely handles depth-0/1 nodes. The commit's panic-safety claim holds; PF-003 satisfied
  (verified directly rather than trusting the commit message).

---

## Suggestions (Lower Confidence)

- **Allocation-strategy divergence from research walk** - `extract.rs:121` (Confidence: 70%) —
  `extract.rs` pre-sizes the ancestor table to `max_depth + 1` via a single up-front pass, while
  `rskim-research/ast_extract.rs:137` starts at 64 and grows on demand (with a comment explaining
  the choice avoids ~4 KiB waste per file). Both are defensible; the difference is justified by
  different call patterns (single-file vs corpus loop) and is noted in KNOWLEDGE.md. Not worth a
  change, but a one-line comment cross-referencing the deliberate difference would aid the next
  reader comparing the two files.

---

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 2 | - |
| Pre-existing | - | - | 0 | 0 |

**Consistency Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The module is a strong consistency match for the crate: naming, DI split, `encode()` usage, and
the infallible return all align with the lexical `ngram.rs` sibling and the documented
conventions. The two MEDIUM items are documentation-fidelity issues, not logic defects — the
"reproduces the research walk chain-break" wording overstates an intentional heuristic divergence
(already documented in KNOWLEDGE.md), and the missing `MAX_TRIGRAMS_PER_FILE` cap is a visible
deviation that is safe due to upstream bounds but would benefit from an explanatory comment.
Both are low-effort doc/comment fixes. Approving on condition the doc wording is tempered.
