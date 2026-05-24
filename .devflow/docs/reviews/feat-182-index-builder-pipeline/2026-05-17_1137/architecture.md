# Architecture Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**`build_index` orchestrates too many concerns (SRP tendency)** - `index.rs:158-244`
**Confidence**: 82%
- Problem: `build_index()` is a single 86-line function that handles cache directory resolution, file walking, manifest loading, parallel classification, sequential index building, manifest construction, and manifest persistence. While it is still readable today (pipeline steps are sequential and well-commented), it mixes I/O orchestration with data transformation in a way that will compound as features like incremental deletions, partial rebuilds, or progress reporting are added. Each step mutates or depends on the previous, but the function owns all of them.
- Fix: This is not blocking at current size, but as the pipeline grows, extract the classify-and-build phase into a testable function that takes `(read_files, manifest) -> (classified, IndexResult)`, keeping `build_index` as the thin I/O shell. This would let you unit-test the classification/cache-hit logic without touching the filesystem.

**Hand-rolled argument parser bypasses clap (OCP violation)** - `index.rs:83-127`
**Confidence**: 85%
- Problem: The rest of the CLI uses clap with derive API (as documented in CLAUDE.md). This subcommand introduces a hand-rolled `parse_args` with manual `--flag=val` / `--flag val` parsing, a custom `next_value` helper, and no validation beyond "unknown argument". This creates a pattern inconsistency: future contributors must maintain two parsing paradigms. Adding a new flag requires manually extending the match chain rather than adding a struct field with a derive attribute.
- Fix: Migrate to a clap derive struct:
  ```rust
  #[derive(clap::Parser)]
  struct IndexArgs {
      #[arg(long)]
      root: Option<PathBuf>,
      #[arg(long)]
      force: bool,
      #[arg(long)]
      max_files: Option<usize>,
      #[arg(long, hide = true)]
      index_dir: Option<PathBuf>,
  }
  ```
  This aligns with the rest of the codebase and gets free help generation, shell completions, and validation.

### MEDIUM

**`FileManifest::load` mixes error return types (`std::io::Result` vs `anyhow`)** - `manifest.rs:109`
**Confidence**: 84%
- Problem: `FileManifest::load` returns `std::io::Result<Self>` while `FileManifest::save` returns `anyhow::Result<()>`. The doc comment says "Only returns Err for unexpected I/O errors" but the actual behavior is to swallow parse errors, version mismatches, and root mismatches into `Ok(empty)`. The mixed error types within the same struct's API is a layering inconsistency -- callers must handle two different error types for the same abstraction.
- Fix: Unify both methods to return `anyhow::Result<Self>` / `anyhow::Result<()>`, which is what the calling code in `index.rs` expects (it uses `?` in an `anyhow::Result` context). The `std::io::Result` return type on `load` forces the call site `manifest.rs:190` in `index.rs` to rely on the implicit `From<io::Error> for anyhow::Error` conversion, which works but is inconsistent.

**`run_classify` silently swallows all errors** - `index.rs:250-256`
**Confidence**: 83%
- Problem: `run_classify` calls `classify_source(content, lang).unwrap_or_default()` which silently discards classification errors, returning an empty field map. While error-tolerant behavior is appropriate for unsupported languages, a classification failure on a file that was already accepted by the walker (supported language, valid UTF-8, not minified) likely indicates a bug or a tree-sitter grammar regression. Silently mapping it to `SearchField::Other` degrades search ranking quality without any diagnostic signal.
- Fix: Log classification failures to stderr when `SKIM_DEBUG` is enabled:
  ```rust
  fn run_classify(content: &str, lang: Language) -> FieldMap {
      match classify_source(content, lang) {
          Ok(fm) => fm,
          Err(e) => {
              if crate::debug_enabled() {
                  eprintln!("skim: classify failed for {:?}: {e}", lang);
              }
              Vec::new()
          }
      }
  }
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`tempfile` promoted from dev-dependency to production dependency** - `Cargo.toml:44`
**Confidence**: 88%
- Problem: `tempfile` was previously a `[dev-dependencies]` entry and is now moved to `[dependencies]`. This is correct for the atomic manifest write pattern (using `NamedTempFile`), but `tempfile` pulls in `fastrand` and `rustix`/`windows-sys` as transitive dependencies that ship in the production binary. The `rskim-search` crate already uses `tempfile` in its `NgramIndexBuilder::atomic_write()` (see `builder.rs:87`), so this pattern is established -- but it means the search module's manifest sidecar duplicates an atomic-write pattern that exists in the library crate.
- Fix: Consider whether `FileManifest::save` could delegate to a shared atomic-write utility in `rskim-search` (where `tempfile` is already a production dep) rather than bringing the dependency into the CLI crate separately. If the current structure is intentional (manifest is CLI-only, not library concern), document that choice in the module doc comment.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`rskim-search` lib.rs doc comment references old file path** - `rskim-search/src/lib.rs:11`
**Confidence**: 90%
- Problem: The doc comment states "CLI/binary code in `crates/rskim/src/cmd/search.rs` handles user-facing I/O" but the file has been renamed to `crates/rskim/src/cmd/search/mod.rs` in this PR. The reference is now stale.
- Fix: Update the path reference to `crates/rskim/src/cmd/search/mod.rs` or generalize to `crates/rskim/src/cmd/search/` since it is now a module directory.

## Suggestions (Lower Confidence)

- **Manifest format could use a more compact binary encoding** - `manifest.rs` (Confidence: 65%) -- JSONL works well for debuggability but for projects with 50,000 files, the manifest could be 10+ MB of JSON text. A future iteration might benefit from a binary format with an index, though JSONL is a defensible choice for v1.

- **`walk_and_read` reads all file contents eagerly into memory** - `walk.rs:89-198` (Confidence: 70%) -- For very large projects (50,000 files at up to 5MB each), this could theoretically require significant memory. In practice, typical source files are small, and the 5MB cap limits the worst case. Worth noting for future scaling but not a concrete issue today.

- **`discover_project_root` could traverse beyond project boundaries** - `walk.rs:52-68` (Confidence: 62%) -- The function walks up the entire filesystem hierarchy looking for `.git`. If invoked in a deeply nested path outside any repo, it traverses to `/`. The `canonicalize` + parent walk is bounded by filesystem depth, so this is not a real risk, but a max-depth guard would make the bound explicit per the reliability principles.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Architecture Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The module decomposition is sound: pure data types in `types.rs`, file I/O in `walk.rs`, persistence in `manifest.rs`, and orchestration in `index.rs` follows the established pattern from `heatmap/` and aligns with the project's separation of concerns. The dependency direction is correct -- CLI modules depend on `rskim-search` library types, never the reverse. The incremental build design (SHA-256 manifest, atomic writes, wrong-root detection) is well-considered for a v1 pipeline.

Conditions for approval:
1. **Unify error return types** on `FileManifest` (`load` and `save` should both use `anyhow::Result`).
2. **Migrate argument parsing to clap** or document explicitly why this subcommand diverges from the codebase standard. The hand-rolled parser is the most significant consistency violation.
