# Architecture Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**PR**: #272
**Date**: 2026-06-05 00:25
**Scope**: `crates/rskim-search/src/ast_index/store/` (format/builder/reader/mod), `index/mod.rs` lang_map widening, `lib.rs` re-exports, `Cargo.toml`

## Summary of Assessment

The store submodule is well-architected. Layering is clean: `format.rs` is a pure
`&[u8]` codec with no `std::fs`/`std::io::Write` (verified — only `crc32fast` and slice
ops), `builder.rs` owns all writes, `reader.rs` owns all mmap reads. Dependency direction
points inward (store depends on format, builder/reader depend on format, all depend on the
shared `lang_map` and `ast_index` extraction layer — no cycles). The DI convention is
honored (`extract_ast_ngrams_with_weights` core vs `extract_ast_ngrams` wrapper feeding the
builder). SOLID adherence is strong: SRP is respected per file, the generic
`binary_search_entries` follows OCP/DRY for both entry tables, and `AstIndexReader: Send +
Sync` is compile-verified.

No CRITICAL or HIGH architecture issues found. Findings are MEDIUM/LOW: a public API-surface
inconsistency with the lexical sibling, a leaky encoding detail in a public type, and dead
re-export plumbing. The `lang_map` `pub(crate)` widening (ADR-001 single-source-of-truth) is
sound and does not leak internals beyond the crate.

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Module visibility inconsistent with lexical sibling — `pub mod builder`/`pub mod reader` vs lexical `mod builder`/`mod reader`** — `store/mod.rs:30-32`
**Confidence**: 88%
- Problem: The store declares `pub mod builder; pub(crate) mod format; pub mod reader;`,
  then re-exports the public types via `pub use`. The lexical sibling (`index/mod.rs:30-33`)
  declares all three submodules private (`mod builder; mod format; mod reader;`) and exposes
  only `pub use builder::NgramIndexBuilder` / `pub use reader::NgramIndexReader`. The PR's
  stated goal is "Mirrors lexical index format.rs/builder.rs/reader.rs split," but the
  visibility differs. With `pub mod builder`/`pub mod reader`, the module paths
  `ast_index::store::builder` and `ast_index::store::reader` become part of the public API,
  exposing internal helper fns/types and creating a second import path for the same types
  (e.g. `store::builder::AstIndexBuilder` and the re-exported `store::AstIndexBuilder`).
  Two paths to one type is the kind of API-surface ambiguity that the lexical module
  deliberately avoids.
- Impact: Wider public surface than intended for a "library-only, no CLI" wave; future
  refactors of submodule internals become semver-visible; inconsistent with the documented
  mirror of the lexical index. Consistency beats cleverness here (project quality rule).
- Fix: Make the submodules private to match the sibling, keeping only the curated re-exports:
  ```rust
  mod builder;
  pub(crate) mod format;
  mod reader;

  pub use builder::AstIndexBuilder;
  pub use format::AstFileMetaEntry;
  pub use reader::{AstIndexReader, AstPosting};
  ```
  If a submodule genuinely needs to stay `pub` (e.g. for doc visibility), document why it
  diverges from the lexical pattern.

**`AstFileMetaEntry` leaks the on-disk `lang_id: u8` encoding into the public API** — `format.rs:184-190` (re-exported at `lib.rs:36` and `store/mod.rs:35`)
**Confidence**: 80%
- Problem: `AstFileMetaEntry` is a public type (the codec/on-disk struct) re-exported all the
  way to the crate root, but its `lang_id` field is a raw 1-byte format ID. To get a usable
  `Language`, every external caller must independently call `lang_from_id` — which is
  `pub(crate)` and therefore NOT reachable outside the crate. The result: a public type whose
  only language field is unusable by external consumers without re-deriving the mapping (a
  leaky abstraction — infrastructure encoding detail surfacing in the public domain type).
  The Reader Contract C6 ("`file_meta(i)` recovers `lang_id` → call `lang_from_id`") cannot be
  satisfied by an out-of-crate caller. This works today only because the consumer (Wave 3f) is
  in-crate, but the type is published at the crate root as if it were a public-API contract.
- Impact: Either the public re-export is premature (no external consumer can use the language
  field), or the API needs a `lang() -> Option<Language>` accessor. As written it advertises a
  capability the public surface can't deliver.
- Fix: Either (a) demote `AstFileMetaEntry` to `pub(crate)` until Wave 3f defines the real
  query-side public shape, or (b) add an accessor that resolves the language internally:
  ```rust
  impl AstFileMetaEntry {
      #[must_use]
      pub fn language(&self) -> Option<rskim_core::Language> {
          crate::index::lang_map::lang_from_id(self.lang_id)
      }
  }
  ```
  Option (b) hides the encoding and makes C6 satisfiable from outside the crate.

### LOW

**Dead re-export plumbing suppressed by `#[allow(unused_imports)]` — `lang_from_id` in format.rs** — `format.rs:36-38`
**Confidence**: 85%
- Problem: `format.rs` re-exports `pub(crate) use crate::index::lang_map::lang_from_id` under
  `#[allow(unused_imports)]`. Verified that nothing imports `lang_from_id` through `format`:
  `reader.rs`, `builder.rs`, `format_tests.rs`, and `builder_tests.rs` do not reference it,
  and `reader_tests.rs:14` imports it directly from `crate::index::lang_map`. The re-export is
  dead plumbing, and the `#[allow]` suppresses what the project's zero-warnings policy would
  otherwise surface. The comment ("used in tests and by readers") is inaccurate — readers do
  not use it.
- Impact: Misleading code (`format.rs` advertises a `lang_from_id` re-export that no module
  consumes); an `#[allow]` masking an unused import in a crate that denies warnings elsewhere.
- Fix: Remove lines 36-38. Tests already import `lang_from_id` from the canonical
  `crate::index::lang_map` path; let Wave 3f add a real re-export when reader code actually
  recovers `Language`. (avoids the dead-code accumulation the project's "delete dead code"
  quality rule targets.)

## Pre-existing Issues (Not Blocking)

None relevant to architecture in the touched files.

## Suggestions (Lower Confidence)

- **`pub fn add_file` / `pub fn add_file_ngrams` on the AST builder are public while the lexical sibling's `add_file` is private** - `builder.rs:138,214` (Confidence: 70%) — The lexical builder keeps `fn add_file` private and exposes indexing through other entry points; the AST builder exposes both the merge primitive and the convenience wrapper publicly. This is defensible for a library-only wave (callers need a way in pre-CLI), but worth a deliberate decision rather than drift — consider whether `build_from_files` should be the only public construction path, mirroring how the lexical index is driven.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 2 | 1 |
| Pre-existing | - | - | 0 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

Strong layering and SOLID adherence; the codec/write/read boundary is genuinely clean and the
DI split is correct. Conditions before merge: (1) align submodule visibility with the lexical
sibling or document the divergence, (2) resolve the `AstFileMetaEntry` leaky-encoding /
unreachable-`lang_from_id` mismatch (demote the type or add a `language()` accessor), and
(3) remove the dead `lang_from_id` re-export in `format.rs`. None block functionally; all are
API-surface hygiene that is cheapest to fix before the type is published.
