# Documentation Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**PR**: #272
**Date**: 2026-06-05 00:25

## Summary of Assessment

This PR is unusually well-documented. The on-disk binary format is fully specified in
code (`format.rs` module doc + per-struct byte-range layout tables), every public API
type carries rustdoc, both `unsafe` blocks have SAFETY comments, and `KNOWLEDGE.md` is
accurate and consistent with the implementation (byte sizes 48/16/20/8/5 match exactly;
the cochange atomic-write and lexical-rename comparisons are factually correct). The
findings below are narrow doc-completeness and doc-drift items, not structural gaps.

## Issues in Your Changes (BLOCKING)

None at CRITICAL or HIGH.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Contract guarantees C3, C5, C6, C7 are not documented at the code API surface** — `crates/rskim-search/src/ast_index/store/reader.rs:50`
**Confidence**: 92%
- Problem: The review brief asks specifically whether C1–C7 are documented at the API
  surface "not just in the PR body." They are fully tabled in `KNOWLEDGE.md` (lines for
  the C1–C7 table) and the PR body, but in code the contract is only partially surfaced.
  The `AstPosting` doc comment says "Guarantees upheld by the reader (C1–C5)" and then
  only enumerates C1, C2, C4. The `lookup_bigram` / `lookup_trigram` rustdoc cite
  C1–C4. C5 (count is structural TF for BM25), C6 (`file_meta` → `lang_id` →
  `lang_from_id` to recover `Language`), and C7 (`Send + Sync`) are never labeled in
  rustdoc. C6 in particular belongs on `file_meta()` (`reader.rs:221`), which currently
  documents the index semantics but never references the contract ID or the
  `lang_from_id` recovery step that Wave 3f depends on.
- Impact: A Wave 3f maintainer reading the rustdoc (the natural first stop) sees a
  "C1–C5" claim that under-delivers, and must leave the code to find C6/C7. The
  contract IDs are the cross-wave coupling vocabulary; they should resolve from the API
  surface they govern.
- Fix: Update the `AstPosting` doc header to "(C1–C7)" and enumerate C3 and C5 alongside
  C1/C2/C4. Add a `// C6:` note to `file_meta()` rustdoc pointing at `lang_from_id`. Add
  a one-line `/// C7: AstIndexReader is Send + Sync` near the existing Send+Sync prose
  block (`reader.rs:88`).

**`AstPostingEntry` doc references a `.count` field that does not exist on the local `AstBigramEntry`/`AstTrigramEntry`** — `crates/rskim-search/src/ast_index/store/format.rs:164`
**Confidence**: 85%
- Problem: The `AstPostingEntry` doc says `count` "is taken directly from
  `AstBigramEntry.count` / `AstTrigramEntry.count`". Within `format.rs` the structs
  named `AstBigramEntry` (line 129) and `AstTrigramEntry` (line 147) have only
  `key`/`posting_offset`/`posting_length` — no `count` field. The `.count` field lives
  on the *identically-named but different* types in `extract.rs`. Rustdoc intra-doc
  link resolution will bind `[AstBigramEntry]` to the local struct, so the reference is
  misleading at the exact place a maintainer parses the byte format.
- Impact: A future maintainer parsing the file from `format.rs` alone (the stated goal)
  will look for a `count` field on the local entry struct, not find it, and be confused
  about provenance.
- Fix: Disambiguate, e.g. "taken from the `count` field of the extraction-side
  `AstBigramEntry`/`AstTrigramEntry` in `extract.rs` (the structural term-frequency
  produced by `extract_ast_ngrams`)" — or use a fully-qualified path
  `crate::ast_index::AstBigramEntry`.

## Pre-existing Issues (Not Blocking)

None relevant to this diff.

## Suggestions (Lower Confidence)

- **`avg_node_count` field doc could note the f32 precision-loss path** — `crates/rskim-search/src/ast_index/store/format.rs:113` (Confidence: 62%) — The builder computes averages as `f64` then narrows to `f32` (`builder.rs:303-305`); the field doc describes the value but not that it is a lossy `f32`. Minor; the byte-layout table already marks it f32.
- **`reserved (= 0)` header bytes lack a forward-compat note** — `crates/rskim-search/src/ast_index/store/format.rs:90` (Confidence: 60%) — The 6 reserved bytes are documented as `= 0` and `mod.rs`/`builder.rs` note "no generation marker," but the `format.rs` field-level doc does not state whether a reader must reject non-zero reserved bytes. A one-line intent note would help the next version bump.

## Accuracy Check: KNOWLEDGE.md (verified against implementation)

| Claim in KNOWLEDGE.md | Verdict |
|---|---|
| Byte sizes: header 48B, bigram 16B, trigram 20B, posting 8B, file meta 5B | ACCURATE (match `format.rs` constants and layout tables) |
| Magic `b"SKAX"` v1, distinct from lexical `b"SKIX"` | ACCURATE |
| CRC32 covers `idx_mmap[48..expected_idx_size]` (bigram+trigram+file_meta) | ACCURATE (matches `reader.rs:153` and `builder.rs:467-470`) |
| Atomic write `NamedTempFile + sync_all + persist`, "stronger than lexical (simple rename)" | ACCURATE (cochange uses sync_all; lexical `index/builder.rs` uses persist WITHOUT sync_all) |
| `node_count` uses `u32::try_from`, not `as u32` (applies PF-004 analog) | ACCURATE (`builder.rs:221`, `:260`) |
| `post_mmap = None` when `postings_file_size == 0` | ACCURATE (`reader.rs:164`) |
| C1–C7 contract table | ACCURATE vs behavior; but only partially mirrored in code rustdoc (see MEDIUM finding) |
| `lang_map` widened to `pub(crate)` | ACCURATE (`index/mod.rs` diff) |
| `build_from_files` ~12.8 ms / 1000 files; size ratio ~1.23×, guard `< 3.0×` | Plausible and consistent with the A16 commit message; not independently re-measured |

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 2 | - |
| Pre-existing | - | - | 0 | 0 |

**Documentation Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

Conditions are non-blocking polish: surface C3/C5/C6/C7 in code rustdoc to match the
KNOWLEDGE.md/PR-body contract (applies ADR-001 — fix the C-contract drift while here),
and disambiguate the `AstPostingEntry` `.count` provenance reference in `format.rs`.
