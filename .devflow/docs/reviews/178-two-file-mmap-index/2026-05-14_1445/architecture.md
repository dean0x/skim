# Architecture Review Report

**Branch**: feat/178-two-file-mmap-index -> main
**Date**: 2026-05-14
**PR**: #224

## Issues in Your Changes (BLOCKING)

### HIGH

**`tempfile` is a production dependency but should be dev-only** - `crates/rskim-search/Cargo.toml:23`
**Confidence**: 90%
- Problem: `tempfile` is listed as a `[dependencies]` (production) dependency, but it is only used in `builder.rs:13` for the `atomic_write` helper and in test files. The `tempfile` crate is designed for tests and temporary scratch work, not production persistence. Its use in `atomic_write` is functionally correct (temp file + rename = atomic), but pulling `tempfile` as a production dependency increases the dependency footprint unnecessarily. More importantly, `tempfile` opens files in platform-specific temp directories by default; here it is configured with `new_in(dir)` which is fine, but the API surface exposed to production is broader than needed.
- Fix: The atomic-write pattern only needs `std::fs::File` + `std::fs::rename`. Replace the `tempfile` usage with a manual temp file approach:
  ```rust
  fn atomic_write(dir: &Path, path: &Path, data: &[u8]) -> Result<()> {
      let tmp_path = dir.join(format!(".{}.tmp", path.file_name().unwrap().to_string_lossy()));
      std::fs::write(&tmp_path, data)?;
      std::fs::rename(&tmp_path, path)?;
      Ok(())
  }
  ```
  Then move `tempfile` from `[dependencies]` to `[dev-dependencies]` where it is already used by the test helpers.

---

**`lib.rs` "NO I/O" architectural claim is violated by the `index` module** - `crates/rskim-search/src/lib.rs:5`
**Confidence**: 85%
- Problem: The crate's top-level doc comment explicitly states `"IMPORTANT: This is a LIBRARY with NO I/O."` and `"Accepts pre-parsed data, not file paths"`. However, the new `index` module performs extensive file I/O: `builder.rs` writes two files to disk via `atomic_write`, and `reader.rs` opens files with `std::fs::File::open` and memory-maps them. Both `NgramIndexBuilder::new` and `NgramIndexReader::open` accept `Path` arguments. This contradicts the stated architectural constraint and could mislead future contributors about what this crate is allowed to do.
- Fix: Update the crate-level doc comment to acknowledge the I/O boundary:
  ```rust
  //! # Architecture
  //!
  //! Core types and traits (`types.rs`) are pure with no I/O.
  //! The `index` module provides on-disk persistence via memory-mapped files.
  //! CLI/binary code in `crates/rskim/src/cmd/search.rs` handles user-facing I/O.
  ```
  This preserves the intent (core types are pure) while accurately documenting that the index module is an I/O layer within the same crate.

### MEDIUM

**`entries.len() as u32` truncation in `build()` is unchecked** - `crates/rskim-search/src/index/builder.rs:238`
**Confidence**: 85%
- Problem: In the `build()` method, `entries.len() as u32` silently truncates if the number of unique bigrams exceeds `u32::MAX` (4 billion). While unlikely in practice (max possible distinct bigrams is 65536 for `u16` keys), the same builder already uses `u32::try_from` for `posting_length` (line 201-205) and `content.len()` (line 127-133), establishing a pattern of checked conversions. This inconsistency means one conversion path is unguarded.
- Fix: Use the checked conversion pattern established elsewhere:
  ```rust
  let ngram_count = u32::try_from(entries.len()).map_err(|_| {
      SearchError::IndexCorrupted(format!(
          "ngram count {} exceeds u32::MAX", entries.len()
      ))
  })?;
  ```
  Note: `postings_buf.len() as u64` (line 240) is safe since `usize` to `u64` is widening on all 64-bit targets, and `self.total_doc_length as f32 / self.file_count as f32` (line 177) is intentional precision-lossy for BM25 averaging.

---

**Builder directly imports Reader, creating a cycle-adjacent coupling** - `crates/rskim-search/src/index/builder.rs:20`
**Confidence**: 82%
- Problem: `builder.rs` imports `super::reader::NgramIndexReader` solely for the `build()` method's final step: opening the freshly-written index and returning `Box<dyn SearchLayer>`. This creates a direct dependency from the write-path module to the read-path module. While not a circular dependency (reader does not import builder), it couples the build step to a specific reader implementation. The `LayerBuilder` trait's contract is `fn build(self) -> Result<Box<dyn SearchLayer>>`, which forces the builder to produce a reader -- this is a trait design constraint, not a builder design flaw.
- Fix: This is acceptable given the trait contract, but document the coupling rationale in a comment on the import line. If a future wave introduces alternative reader implementations, consider extracting the "open after build" step into the caller or a factory function. No code change required now.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`SearchField::from_discriminant` duplicates `#[repr(u8)]` mapping manually** - `crates/rskim-search/src/types.rs:94-106`
**Confidence**: 80%
- Problem: The `from_discriminant` method manually maps `0 => TypeDefinition, 1 => FunctionSignature, ...` which duplicates the `#[repr(u8)]` discriminant assignments on lines 59-73. If a new variant is added to `SearchField`, the developer must update three locations: the enum definition, `from_discriminant`, and `name()`. The test `test_search_field_discriminant_roundtrip` catches drift, which mitigates the risk, but three-way sync is fragile.
- Fix: No immediate change needed -- the roundtrip test provides an adequate compile-time + test-time safety net. For future hardening, consider using a procedural macro or the `num_enum` crate to derive `TryFrom<u8>` automatically. This is a should-fix, not blocking.

## Pre-existing Issues (Not Blocking)

No pre-existing architectural issues found in unchanged code.

## Suggestions (Lower Confidence)

- **`lang_map.rs` also duplicates a manual enum mapping** - `crates/rskim-search/src/index/lang_map.rs:21-71` (Confidence: 70%) -- `lang_to_id` and `lang_from_id` manually enumerate all 17 `rskim_core::Language` variants. Adding a language requires updating both functions plus tests. A macro or `strum`-style derive could eliminate this, but the exhaustive `match` provides compile-time enforcement when new variants are added to the enum, which is a reasonable trade-off.

- **`postings_buf.len() as u64` widening cast could be made explicit** - `crates/rskim-search/src/index/builder.rs:240` (Confidence: 65%) -- While `usize` to `u64` is always safe on 64-bit targets (the primary deployment platform), a `u64::from()` or comment noting the widening intent would maintain the "explicit about casts" pattern established elsewhere in the file.

- **Reader computes `entries_start`/`entries_end` in multiple methods** - `crates/rskim-search/src/index/reader.rs:121,139,151` (Confidence: 62%) -- Three methods independently compute `SKIDX_HEADER_SIZE + ngram_count * SKIDX_ENTRY_SIZE` offsets. A private `fn entries_range(&self) -> Range<usize>` helper would centralize this and prevent drift if the layout changes.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

### Architecture Strengths

The module decomposition is excellent. The four-module split (format.rs = pure codec, builder.rs = write path, reader.rs = read path, lang_map.rs = stable enum mapping) follows clear separation of concerns. Key design qualities:

1. **Clean layering**: `format.rs` explicitly declares "No `std::fs` or `std::io::Write`" and upholds this -- it is a pure codec operating on `&[u8]` slices. All I/O is confined to builder/reader.
2. **Trait conformance**: `NgramIndexBuilder` implements `LayerBuilder` and `NgramIndexReader` implements `SearchLayer`, respecting the crate's trait hierarchy.
3. **Atomicity contract**: The two-file write order (.skpost then .skidx as commit point) is well-documented and correctly implemented.
4. **Error-first design**: Every fallible path returns `Result<T>` with contextual `SearchError` variants. No `.unwrap()` or `.expect()` in production code.
5. **Stable on-disk format**: Explicit `#[repr(u8)]` discriminants, documented byte layouts, magic bytes, version field, and CRC32 checksum demonstrate format evolution awareness.
6. **Deep modules**: The public surface is just `NgramIndexBuilder` and `NgramIndexReader` (2 types), hiding all internal format structs, encode/decode functions, and BM25 scoring behind `pub(crate)` visibility. This is a textbook "deep module" design.

### Why CHANGES_REQUESTED (not BLOCK)

The two HIGH issues are real architectural concerns but not correctness bugs. The `tempfile` production dependency is a footprint issue (not a security or correctness risk), and the `lib.rs` doc comment is misleading but not functionally broken. Both should be addressed before merge for architectural hygiene, but neither would cause runtime failures.
