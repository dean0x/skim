# Resolution Summary

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05_0025
**Review**: .devflow/docs/reviews/feature-194-wave-3d-ast-index-store/2026-06-05_0025
**PR**: #272
**Command**: /resolve

## Decisions Citations

- applies ADR-001 — batch-1-source (issues 1,2,3,4,7,8,9,10,11), batch-2-tests (issues 2,4)
- applies ADR-002 — batch-2-tests (issue 3, A16 empirically-grounded bound)
- avoids PF-002 — batch-1-source (issues 4,12), batch-2-tests (no deferrals)
- avoids PF-004 — batch-1-source (issue 8, serialize_index refactor preserves `u32::try_from`, no silent narrowing)
- avoids PF-005 — batch-2-tests (issue 3, A16 bound verified against measured 1.23x baseline, not copied verbatim)

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 16 |
| Fixed | 15 |
| False Positive | 1 |
| Deferred | 0 |
| Blocked | 0 |

Plus 1 pre-existing/inherent item (mmap TOCTOU) — not actionable, documented as an inherent mmap constraint shared with the lexical index sibling.

## Fixed Issues
| Issue | File:Line | Batch |
|-------|-----------|-------|
| Module visibility: `pub mod builder/reader` → private `mod` + `pub use` (parity with siblings) | `store/mod.rs:30,32` | batch-1 |
| Surface contract C3/C5/C6/C7 in rustdoc; disambiguate `AstPostingEntry.count` provenance | `reader.rs:50`, `format.rs:164` | batch-1 |
| Add `AstFileMetaEntry::language() -> Option<Language>` accessor (satisfies C6 externally) | `format.rs:184-190` | batch-1 |
| Remove dead `lang_from_id` re-export under `#[allow(unused_imports)]` (zero-warnings) | `format.rs:36-38` | batch-1 |
| Rename on-disk structs `AstBigramEntry`/`AstTrigramEntry` → `*TableEntry` (collision with extract.rs) | `format.rs:129,147` | batch-1 |
| Drop redundant `AST_` constant prefix; `AST_SKIDX_MAGIC` → `SKAX_MAGIC` | `format.rs:49-71` | batch-1 |
| Add O(n) allocation-free doc_id monotonicity check on read → `IndexCorrupted` (C1 defense) | `reader.rs:292-340` | batch-1 |
| Extract generic `serialize_entry_table` helper; remove duplicated bigram/trigram blocks | `builder.rs:345-511` | batch-1 |
| `read_array` error reports bytes-available-from-offset, not whole-buffer length | `format.rs:200-212` | batch-1 |
| Soften atomic-write crash-safety doc (rename durability needs dir fsync); match cochange posture | `builder.rs:1-9` | batch-1 |
| Document `build_from_files` peak-memory bound + reference #273 (chunking deferred to Wave 4) | `builder.rs:256-277` | batch-1 |
| Add 7 C3 reader corruption tests (CRC-recomputed): OOB offset, slice overflow, misaligned length, non-ascending doc_ids | `reader_tests.rs:524-748` | batch-2 |
| Make C1 sort/uniqueness observable (10-file×3-key `windows(2)` assertion); fix mislabeled `// C2` comment | `builder_tests.rs:185-239` | batch-2 |
| Tighten A16 size-ratio bound `< 3.0` → `< 1.8` (measured 1.23x baseline, 1.46x margin) | `reader_tests.rs:498-523` | batch-2 |
| Update stale `#[ignore]`/5% comments to active `< 1.8x` bound | `reader_tests.rs:395`, `ast_index_bench.rs:8` | batch-2 |

## False Positives
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| Re-index read/write interleave can yield silent wrong results | `builder.rs:11-15,336-338` | Already documented as an explicit precondition ("NOT atomic together; callers MUST serialize re-index against concurrent reads; no generation marker, reserved bytes stay zero"). Clear and complete — no code change needed. A header generation marker is a tracked Wave-4 follow-up, not in-scope for this PR. Confirmed intentional posture matching the lexical sibling (avoids PF-002). |

## Deferred to Tech Debt
None. The three scope-bounded items (directory fsync, build_from_files chunking, re-index generation marker) were addressed with the in-PR documentation fix per reviewer guidance; the code-level hardening for each is a tracked Wave-4 / #273 follow-up rather than a deferred review finding.

## Blocked
None.

## Verification
- `cargo build -p rskim-search` — clean (0 warnings)
- `cargo clippy -p rskim-search --all-targets -- -D warnings` — clean
- `cargo fmt -p rskim-search --check` — clean
- `cargo test -p rskim-search` — 640 pass (was 633; +7 new C3 corruption tests). New C3 tests sanity-checked as non-vacuous (fail/panic when the guard is removed).

## Follow-up Notes
- PR description states "632 tests" / "636 tests"; actual is now 640. Documentation drift in the PR body — update when next touching it.
