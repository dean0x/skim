# Consistency Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**PR**: #272
**Date**: 2026-06-05 00:25

## Scope

New submodule `crates/rskim-search/src/ast_index/store/` (`format.rs` / `builder.rs` /
`reader.rs` + sidecar `*_tests.rs`) reviewed against the two siblings it claims to mirror:

- Lexical index — `crates/rskim-search/src/index/` (`NgramIndexBuilder` / `NgramIndexReader`,
  magic `b"SKIX"`, files `index.skidx` / `index.skpost`)
- Co-change matrix — `crates/rskim-search/src/cochange/` (`CochangeMatrixBuilder` /
  `CochangeMatrixReader`, magic `b"SKCC"`, file `cochange.skcc`)

Overall the PR mirrors the sibling layout faithfully. Test-module wiring, error-variant
choice, atomic-write strategy, `new()` directory handling, average/offset cast patterns, and
doc-comment structure all match. A small number of naming/visibility deviations are below.

## Issues in Your Changes (BLOCKING)

### CRITICAL
None.

### HIGH

**Public submodule visibility breaks encapsulation parity with both siblings** — `crates/rskim-search/src/ast_index/store/mod.rs:30,32`
**Confidence**: 92%
- Problem: The new module declares `pub mod builder;` and `pub mod reader;`. Both siblings
  keep their submodules private and expose only the public types via `pub use`:
  - `index/mod.rs`: `mod builder; mod format; mod reader;` then `pub use builder::NgramIndexBuilder;`
  - `cochange/mod.rs`: `mod builder; mod format; mod reader;` then `pub use builder::CochangeMatrixBuilder;`
  Exposing `builder` / `reader` as `pub mod` leaks the module path (e.g. `ast_index::store::builder::AstIndexBuilder`)
  as a second public route alongside the re-export, widening the public API surface beyond
  what the siblings expose and inviting downstream code to depend on the internal path.
- Impact: Public API surface divergence; the "mirrors the lexical index pattern" claim does
  not hold for module encapsulation. `format` is correctly `pub(crate)`, so only `builder`/`reader` deviate.
- Fix: Match the siblings — make the submodules private and rely on the existing re-exports:
  ```rust
  mod builder;
  pub(crate) mod format;
  mod reader;

  pub use builder::AstIndexBuilder;
  pub use format::AstFileMetaEntry;
  pub use reader::{AstIndexReader, AstPosting};
  ```
  (`format` stays `pub(crate)` because `AstFileMetaEntry` is re-exported from it; the lexical
  sibling keeps `format` fully private, but the cochange/`AstFileMetaEntry` re-export need is the
  reason `pub(crate)` is acceptable here.)

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Duplicate type names `AstBigramEntry` / `AstTrigramEntry` in the same feature module tree** — `crates/rskim-search/src/ast_index/store/format.rs:129,147`
**Confidence**: 88%
- Problem: `AstBigramEntry` and `AstTrigramEntry` are already defined and **publicly exported**
  from `ast_index/extract.rs:38,52` (the n-gram extraction structs carrying `ngram` + `count`,
  re-exported from `lib.rs`). `format.rs` redefines structs with the identical names as the
  on-disk lookup-table entries (carrying `key` + `posting_offset` + `posting_length`). Two
  semantically different structs share one name within the same `ast_index` subtree. Neither
  sibling has this collision: lexical names its on-disk entry `SkidxEntry` and cochange names
  its entries `PairEntry` / `FileCommitEntry` — distinct, role-descriptive names that never
  clash with extraction types. The builder imports the format variants via `super::format::{...}`
  (`builder.rs:40-41`) so it compiles, but a reader of `builder.rs` cannot tell which
  `AstBigramEntry` is in scope without resolving the import.
- Impact: Reader confusion and a latent footgun — a future `use crate::ast_index::AstBigramEntry`
  added to `builder.rs` would silently shadow or conflict with the format import. The
  format-vs-extraction distinction is exactly the kind of thing the sibling naming avoids.
- Fix: Rename the on-disk lookup-table structs to role-descriptive names that do not collide,
  mirroring the lexical `SkidxEntry` convention — e.g. `AstBigramTableEntry` / `AstTrigramTableEntry`
  (or `AstBigramSkidxEntry` / `AstTrigramSkidxEntry`). Update `format.rs`, the `builder.rs`
  import block, and the format/builder tests. These types are `pub(crate)`, so the rename is
  internal-only with no external API impact.

**Every format constant carries a redundant `AST_` prefix, unlike both siblings** — `crates/rskim-search/src/ast_index/store/format.rs:49-71`
**Confidence**: 85%
- Problem: The new module prefixes all seven format constants with `AST_`
  (`AST_FORMAT_VERSION`, `AST_HEADER_SIZE`, `AST_BIGRAM_ENTRY_SIZE`, `AST_TRIGRAM_ENTRY_SIZE`,
  `AST_POSTING_ENTRY_SIZE`, `AST_FILE_META_SIZE`, plus `AST_SKIDX_MAGIC`). Both siblings use
  bare, module-scoped names:
  - lexical: `FORMAT_VERSION`, `SKIDX_HEADER_SIZE`, `SKIDX_ENTRY_SIZE`, `POSTING_ENTRY_SIZE`, `FILE_META_SIZE`, `SKIDX_MAGIC`
  - cochange: `FORMAT_VERSION`, `HEADER_SIZE`, `FILE_COMMIT_ENTRY_SIZE`, `PAIR_ENTRY_SIZE`, `SKCC_MAGIC`
  The established convention is: bare `FORMAT_VERSION`, bare size constants (no module prefix),
  and a magic-bytes constant prefixed only with the 4-byte tag (`SKIDX_`/`SKCC_`). Since these
  constants are `pub(crate)` and always referenced through `super::format::`, the `AST_` prefix
  is pure redundancy at every call site.
- Impact: Style inconsistency across the three parallel format modules; the "mirror" claim is
  weakened. Not behavior-affecting.
- Fix: Drop the `AST_` prefix to match siblings: `FORMAT_VERSION`, `HEADER_SIZE`,
  `BIGRAM_ENTRY_SIZE`, `TRIGRAM_ENTRY_SIZE`, `POSTING_ENTRY_SIZE`, `FILE_META_SIZE`. For the
  magic constant, follow the tag-prefix convention with `SKAX_MAGIC` (matching `SKIDX_MAGIC` /
  `SKCC_MAGIC`) rather than `AST_SKIDX_MAGIC` — note the current name is also slightly
  misleading since the bytes are `b"SKAX"`, not `SKIDX`. This is a `pub(crate)`-only rename
  touching `format.rs`, `builder.rs`, `reader.rs`, and the test files.

## Pre-existing Issues (Not Blocking)

**Lexical `format.rs` uses inline `crate::Result` while cochange + AST use `use crate::{Result, SearchError}`** — `crates/rskim-search/src/index/format.rs`
**Confidence**: 80%
- The new module matches the **newer** cochange convention (`use crate::{Result, SearchError}`),
  not the older lexical one (`crate::Result` at each return position). This is the correct choice —
  the divergence is in the pre-existing lexical file, not the new code. Informational only; no
  action required in this PR.

## Suggestions (Lower Confidence)

- **Reader exposes more public accessors than the lexical reader** — `crates/rskim-search/src/ast_index/store/reader.rs:199-217` (Confidence: 62%) — the AST reader adds `file_count`, `avg_node_count`, `avg_bigram_count`, `avg_trigram_count` getters where the lexical reader bundles such data into a single `stats() -> IndexStats` method. Consider whether a future `AstIndexStats` aggregate would better match the lexical `stats()` shape for Wave 3f, but the individual getters are reasonable and not a blocking inconsistency.

## Decisions Applied

- **applies ADR-001** — "fix all noticed issues immediately." The builder explicitly cites it
  (`builder.rs:145`) and the prior self-review commit (9f300ea) hardened `as u32` narrowing.
  All three findings above are noticed issues surfaced for immediate resolution per this ADR.
- **avoids PF-004** — `node_count` uses `u32::try_from` with an `IndexCorrupted` error on
  overflow (`builder.rs:221-227, 260-266`) instead of a silent `as u32` narrowing. Verified
  consistent with the lexical sibling's count-narrowing pattern. No violation.

## Summary
| Category    | CRITICAL | HIGH | MEDIUM | LOW |
|-------------|----------|------|--------|-----|
| Blocking    | 0        | 1    | 0      | -   |
| Should Fix  | -        | 0    | 2      | -   |
| Pre-existing| -        | -    | 0      | 1   |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The module is a faithful structural mirror of its siblings (test wiring, error variants,
atomic write, cast patterns, doc structure, `new()` semantics all match). Three naming/visibility
deviations from the established sibling conventions should be resolved before merge: public
submodule visibility (HIGH), the `AstBigramEntry`/`AstTrigramEntry` name collision with the
exported extraction types (MEDIUM), and the redundant `AST_` constant prefix (MEDIUM). None
affect behavior; all are mechanical, `pub(crate)`-or-internal renames.
