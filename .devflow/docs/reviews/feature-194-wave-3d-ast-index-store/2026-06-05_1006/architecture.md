# Architecture Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05_1006
**PR**: #272 (Review CYCLE 2)
**Scope**: `crates/rskim-search/src/ast_index/store/` (format/builder/reader triad), `ast_index/mod.rs`, `lib.rs`, `index/mod.rs`, `Cargo.toml`, bench. `.devflow/**` ignored.

## Summary of Assessment

The Wave 3d store is a clean, well-layered addition. The module split (`format` = pure codec, `builder` = write-side I/O, `reader` = mmap read-side I/O) faithfully mirrors both the lexical `index/` and `cochange/` siblings. Cycle 1 already corrected module visibility (private `mod` + `pub use`), the magic/constant naming, and the on-disk-vs-extract struct name collision. The `lang_map` widening to `pub(crate)` (single source of truth for language↔u8) is the correct DIP/DRY move and avoids a second mapping table.

No blocking architectural issues were found in the changed lines. The findings below are consistency/SRP observations consistent with the sibling modules, plus low-confidence suggestions.

## Issues in Your Changes (BLOCKING)

### CRITICAL
None.

### HIGH
None.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`atomic_write` is now triplicated across three builders** — `crates/rskim-search/src/ast_index/store/builder.rs:561`
**Confidence**: 82%
- Problem: `fn atomic_write(dir, path, data)` now exists as a near-identical copy in three sibling builders: `index/builder.rs:87`, `cochange/builder.rs:331`, and `ast_index/store/builder.rs:561`. The AST copy adds an extra step (Unix `set_permissions(0o644)`) that the lexical copy (`index/builder.rs`) does not have — so the three copies have already begun to diverge. This is a DRY/SRP smell: crash-safe atomic file replacement is one concern (one reason to change — e.g. adding the directory-fsync that all three docs flag as a follow-up) but is now owned in three places. A future fix to rename durability must be applied three times or the modules silently diverge further.
- Why it matters: The PR doc and module rustdoc explicitly defer a directory fsync "as a follow-up" in all three builders. With three copies, that follow-up is three edits, and the lexical copy already lacks the `0o644` step the AST/cochange copies have — exactly the drift this duplication invites.
- Fix: Extract a single `pub(crate) fn atomic_write(dir, path, data, mode: Option<u32>)` (or an `AtomicFile` helper) into a shared module (e.g. `crates/rskim-search/src/index/` already houses `lang_map` as a shared primitive, or a new `crate::io_util`), and have all three builders call it. This is a Wave-4-scope refactor touching the lexical sibling, so it is reasonable to track rather than land in #272 — but per ADR-001 ("fix all noticed issues immediately regardless of scope") it should at minimum be surfaced to the user for a fix-now decision rather than left silent. Note: the AST builder itself introduces the third copy, so it is in-scope for this PR.

## Pre-existing Issues (Not Blocking)

None beyond the duplication noted above (which the AST builder participates in by adding the third copy).

## Suggestions (Lower Confidence)

- **`AstIndexBuilder` does not implement the `LayerBuilder` trait** despite exposing an identical `add_file(id, content, lang) -> Result<()>` signature — `crates/rskim-search/src/ast_index/store/builder.rs:287` (Confidence: 65%) — The lexical `NgramIndexBuilder` implements `LayerBuilder` (`index/builder.rs:230`); the AST builder does not. This is defensible — `LayerBuilder::build` returns `Box<dyn SearchLayer>`, and `AstIndexReader` is a posting-list primitive for Wave 3f, not yet a `SearchLayer`, so forcing the trait would be an ISP violation (implementing a method whose return type doesn't fit). Worth a one-line rustdoc note on `add_file` explaining why the trait is intentionally not implemented, so the asymmetry with the lexical sibling reads as deliberate rather than an oversight.

- **`file_meta` offset math uses unchecked `* BIGRAM_ENTRY_SIZE` / `* TRIGRAM_ENTRY_SIZE`** while `open` uses `checked_mul` — `crates/rskim-search/src/ast_index/store/reader.rs:244-246` (Confidence: 60%) — Not a real defect: `open` already validated `idx_mmap.len() == expected_idx_size` using checked arithmetic over the same counts, so these products provably cannot overflow `usize` here. The end-bound is also `checked_add` + bounds-filtered. A short comment ("counts validated non-overflowing in `open`") would make the asymmetry self-documenting and pre-empt a future reviewer re-flagging it.

## Decisions Applied

- **applies ADR-001** — the `atomic_write` triplication and the `LayerBuilder`-asymmetry note are surfaced (not silently deferred) per "if you see something, do something," for a user fix-now decision.
- **avoids PF-004** — confirmed the builder's `node_count`/posting-length narrowing uses `u32::try_from` (builder.rs:140, 294, 340), not silent `as u32`; consistent with the lexical count-narrowing sibling. No new finding.
- ADR-002 / PF-005 relate to test acceptance bounds (A16), out of scope for architecture focus; already resolved in Cycle 1.

## Cross-Cycle Awareness

Parsed `2026-06-05_0025/resolution-summary.md`. Did not re-flag the 15 fixed items (module visibility, struct-name collision, magic prefix, C-contract rustdoc, `serialize_entry_table` extraction, etc.) or the documented mmap-TOCTOU false-positive. The `serialize_entry_table` generic helper extracted in Cycle 1 (builder.rs:117) verified present and correctly removes the prior bigram/trigram duplication — a genuine SRP improvement. The `atomic_write` finding is NEW (the prior cycle reviewed serialization duplication, not the cross-builder write-path duplication).

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Architecture Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS — no blocking issues; the `atomic_write` triplication (MEDIUM) should be surfaced to the user for a consolidate-now-or-track decision per ADR-001.
