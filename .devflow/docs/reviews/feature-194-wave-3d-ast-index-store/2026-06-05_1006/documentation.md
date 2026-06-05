# Documentation Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05_1006
**PR**: #272 (Wave 3d AST on-disk index store)
**Cycle**: 2 (Cycle 1 fixed 15 issues)

## Scope

In-code rustdoc and comments for the AST on-disk index store:
`store/{format,builder,reader,mod}.rs`, `benches/ast_index_bench.rs`, and the
small edits to `ast_index/mod.rs`, `lib.rs`, `index/mod.rs`, `Cargo.toml`.
`.devflow/**` artifacts (KNOWLEDGE.md, decisions, prior review files) were
excluded from assessment per scope instructions.

## Cross-Cycle Awareness

Read the cycle-1 resolution summary. The cycle-1 documentation findings —
C3/C5/C6/C7 contract surfacing in rustdoc, `AstPostingEntry.count` provenance
disambiguation, atomic-write durability caveat softening, `build_from_files`
peak-memory bound, and stale 5%/`<3x` A16 comment updates — are all confirmed
fixed in current code and were NOT re-raised. The single false positive
(re-index interleave precondition) is documented as an explicit precondition in
`builder.rs:16-20`; verified present, not re-flagged.

## Verification Performed

- **C1–C7 contract on reader's public API**: documented on `AstPosting` rustdoc
  (`reader.rs:48-61`) and on the `lookup_bigram`/`lookup_trigram`/`file_meta`
  methods. Accurate — C1's "at most one per doc_id" guarantee is enforced by the
  strict-ascending check in `lookup_postings_generic` (`reader.rs:366-373`,
  rejects `doc_id <= prev`), which the doc references correctly.
- **On-disk binary format**: header byte layout (`format.rs:76-89`), entry sizes
  16B/20B/8B/5B (`format.rs:55-68` + per-struct layout blocks), and CRC coverage
  (`format.rs:462-466`, `mod.rs:1-13`, `reader.rs:159-162`) all documented and
  internally consistent with the encode/decode implementations.
- **Atomic-write durability caveat**: `builder.rs:11-14` accurately states the
  rename-needs-dir-fsync limitation and that no directory fsync is performed.
- **`build_from_files` peak-memory bound**: documented with #273 reference
  (`builder.rs:319-324`).
- **Stale A16 bound**: NO stale 5%/`<3x` references remain in store code. The
  bench (`ast_index_bench.rs:8-11`) and the A16 test (`reader_tests.rs:395,466,
  507,518`) all state/assert `< 1.8×` consistently.

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

None at >=80% confidence.

## Pre-existing Issues (Not Blocking)

None.

## Suggestions (Lower Confidence)

- **`build_from_files` rustdoc still says "<10 s target" without margin context** -
  `builder.rs:314` (Confidence: 65%) — The method doc states "This meets the
  <10 s target for 1,000 files" but does not mention the measured ~12.8 ms
  actual. The KNOWLEDGE.md and bench both cite the measured figure; the method
  rustdoc reads slightly conservative by comparison. Purely a clarity nit — the
  claim is correct, just less informative than it could be.

- **`serialize_index` comment references a renamed helper** - `builder.rs:448`
  (Confidence: 70%) — The inline comment "build_entry_table writes each posting
  into postings_buf and returns the sorted entry table" refers to
  `build_entry_table`, but the actual helper is named `serialize_entry_table`
  (defined `builder.rs:117`). Minor code-comment drift — the comment names a
  function that does not exist under that name. Low impact (internal comment,
  not public API) but a one-word fix keeps the comment aligned with the code.

- **PR description test-count drift (632/636 vs actual 640)** - PR #272 body
  (Confidence: 90% that drift exists; reported as Suggestion because it is the
  PR description, not reviewable source) — The PR body states "632/636 tests";
  the cycle-1 resolution summary confirms the actual count is now 640 (+7 C3
  corruption tests). This is documentation drift in the PR body, not in-code
  documentation. The documentation skill does not review PR descriptions, and
  the orchestrator hint asked only to confirm the drift — confirmed real. Update
  the PR body when next touched; not merge-blocking.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Documentation Score**: 9/10
**Recommendation**: APPROVED

The in-code documentation for the Wave 3d store is exemplary: every public API
carries rustdoc with parameters, errors, and contract references; the binary
format is documented at module, struct, and field granularity with matching
byte-offset layouts; the C1–C7 contract is surfaced on the reader's public
surface; and all cycle-1 documentation findings are resolved. The two
in-code suggestions are minor clarity/drift nits (a renamed-helper comment and
a conservative-but-correct method-doc claim); the PR-body test-count drift is
outside in-code documentation scope and is a non-blocking PR-description update.
