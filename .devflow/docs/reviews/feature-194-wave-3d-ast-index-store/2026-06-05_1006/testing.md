# Testing Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05_1006
**PR**: #272 (Wave 3d AST on-disk index store)
**Cycle**: 2

## Scope & Cross-Cycle Awareness

Reviewed the test suites for the Wave 3d store sub-module: `format_tests.rs` (codec/binary
search), `builder_tests.rs` (A2/A4/A7–A14 + build_from_files), `reader_tests.rs`
(A1–A16 + C1–C7 + C3 corruption matrix), and `benches/ast_index_bench.rs`. `.devflow/**`
changes ignored per scope.

Cycle 1 (resolution-summary.md) already fixed 15 issues, including the 7 CRC-recomputed C3
corruption tests (`reader_tests.rs:524-775`), the observable C1 `windows(2)` assertion
(`builder_tests.rs:185-237`), and the A16 bound tightening to `< 1.8×`. Per instructions I do
NOT re-flag those. I verified the False Positive (re-index concurrency) is still correctly
documented as a precondition — no regression. This report covers REMAINING gaps and any NEW
issues only.

Overall the suite is strong: the C1–C7 contract is non-vacuously exercised, corruption tests
were confirmed non-vacuous in cycle 1, round-trip coverage (build→read) is solid, and edge
cases (empty corpus, zero-ngram files, multi-language) are present. The findings below are
targeted gaps, not systemic weaknesses.

## Issues in Your Changes (BLOCKING)

None. No blocking test issues. All tests added in this PR verify behavior (observable lookup
results, error messages, byte-level round-trips), not implementation internals.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**C6 contract verified through the wrong API — `AstFileMetaEntry::language()` has zero coverage** — Confidence: 90%
- `reader_tests.rs:142-165` (`a5_lang_recovery_from_file_meta`)
- Problem: Cycle 1 added `AstFileMetaEntry::language() -> Option<Language>` specifically
  "satisfies C6 externally" (resolution-summary.md, batch-1). The C6 rustdoc contract in
  `reader.rs:58-59` and `format.rs:193-198` states the recovery path is `file_meta(i).language()`.
  But the only C6 test calls the lower-level `lang_from_id(meta.lang_id)` directly and never
  invokes the `language()` accessor that the contract actually advertises. The public-API
  method that Wave 3f will call is therefore untested, and its documented `None` future-compat
  branch (unrecognised `lang_id`) is never exercised. `decode_file_meta` does not validate
  `lang_id`, so the `None` path is reachable and meaningful.
- Fix: Assert through the contract API and cover the `None` branch:
  ```rust
  // Replace the lang_from_id calls with the C6 accessor:
  assert_eq!(reader.file_meta(0).unwrap().language(), Some(Language::Rust));
  // And add a None-path test using a hand-written file_meta with an out-of-range lang_id:
  let meta = AstFileMetaEntry { lang_id: 250, node_count: 1 };
  assert_eq!(meta.language(), None, "unrecognised lang_id must map to None (C6 future-compat)");
  ```

### MEDIUM

**No multi-language round-trip asserts distinct per-language `lang_id` survives serialization** — Confidence: 82%
- `reader_tests.rs:45-79` (`build_3_file_index`) + `builder_tests.rs:308-331`
- Problem: The 3-file fixture mixes Rust/Python/Go and `a5` checks lang recovery, which is good.
  But no single test builds an index spanning 4+ languages and asserts every file's `lang_id`
  round-trips correctly AND its postings are independently retrievable. The multi-language
  surface (C6 across the full `lang_to_id`/`lang_from_id` table) is only spot-checked on three
  IDs. Since `lang_to_id`/`lang_from_id` is shared with the lexical index and was widened to
  `pub(crate)` for this PR, a regression in the mapping for a less-common language (e.g. Kotlin,
  Swift, C#) would go uncaught.
- Fix: Add a parametrized round-trip over a representative span of languages asserting
  `file_meta(i).language() == Some(expected_lang)` for each. Low cost, closes the C6 breadth gap.

## Pre-existing Issues (Not Blocking)

None applicable — this is net-new code; there is no pre-existing test code in this module to
flag.

## Suggestions (Lower Confidence)

- **`add_file` node_count overflow guard (PF-004 analog) has no test** - `builder.rs:294,340`
  (Confidence: 70%) — The `u32::try_from(lin.nodes.len())` → `IndexCorrupted` guard is
  load-bearing per PF-004 and the KNOWLEDGE anti-patterns list, but is untested. It is genuinely
  hard to test (needs >4B nodes, capped at 100K by `MAX_AST_NODES`), so the path is arguably
  unreachable in practice through the public API — which itself is worth a one-line note or a
  `debug_assert`-style characterization rather than a runtime test.

- **`a10_atomic_write_no_temp_leftovers` is mildly brittle to tempfile internals** -
  `builder_tests.rs:297` (Confidence: 62%) — The `.tmp` suffix filter assumes `NamedTempFile`
  produces a `.tmp`-suffixed name; `tempfile` actually generates random names without a fixed
  suffix. The assertion would pass vacuously if a temp file leaked under a non-`.tmp` name.
  Asserting `entries.len() == 2` (exactly the two index files) would be a stronger, less
  implementation-coupled check.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 2 | - |
| Pre-existing | - | - | 0 | 0 |

**Testing Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The test suite is comprehensive and behavior-focused. The two MEDIUM gaps both concern the C6
language-recovery contract: the public `language()` accessor added in cycle 1 is never called
by any test, and multi-language breadth is only spot-checked. Neither blocks merge (the
underlying `lang_from_id` path IS tested), but closing them would make the C6 contract that
Wave 3f depends on genuinely guaranteed rather than asserted-by-proxy.
