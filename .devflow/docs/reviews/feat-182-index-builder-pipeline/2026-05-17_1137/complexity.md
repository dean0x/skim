# Complexity Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**`build_index` exceeds 50-line threshold (85 lines, 6 inline pipeline steps)** - `index.rs:159-244`
**Confidence**: 85%
- Problem: The `build_index` function orchestrates 6 numbered pipeline steps inline: cache dir resolution, walk+read, manifest load, parallel classify, sequential build, and manifest write. At 85 lines it exceeds the 50-line function length warning threshold and has cyclomatic complexity around 8. Each step is cohesive in isolation but combining them into one function means the reader must hold all 6 stages in working memory simultaneously. The two manifest construction loops (lines 195-208 for classify, lines 224-233 for manifest write) are structurally similar but separated by the builder loop, adding cognitive load.
- Fix: Extract the classify and manifest-write phases into named helpers. The function is currently readable due to numbered comments, but extraction would improve testability and keep `build_index` as a pure orchestrator. For example:

```rust
fn classify_files(
    read_files: &[ReadFile],
    manifest: &FileManifest,
) -> Vec<ClassifiedFile> {
    read_files
        .par_iter()
        .map(|rf| {
            let path_key = rf.rel_path.to_string_lossy().replace('\\', "/");
            if let Some(entry) = manifest.lookup(&path_key)
                && entry.sha256 == rf.sha256
            {
                return (decode_field_map(&entry.field_map), true);
            }
            (run_classify(&rf.content, rf.lang), false)
        })
        .collect()
}

fn write_manifest(
    config: &IndexConfig,
    cache_dir: PathBuf,
    read_files: &[ReadFile],
    classified: &[ClassifiedFile],
) -> anyhow::Result<()> {
    let mut new_manifest = FileManifest::new(config.root.clone(), cache_dir);
    for (idx, rf) in read_files.iter().enumerate() {
        let (ref field_map, _) = classified[idx];
        let path_key = rf.rel_path.to_string_lossy().replace('\\', "/");
        new_manifest.insert(ManifestEntry {
            path: path_key,
            sha256: rf.sha256.clone(),
            lang: format!("{:?}", rf.lang).to_lowercase(),
            field_map: encode_field_map(field_map),
        });
    }
    new_manifest.save()
}
```

This would bring `build_index` down to ~40 lines as a clean orchestration sequence.

**`walk_and_read` exceeds 50-line threshold (106 lines, cyclomatic complexity ~12)** - `walk.rs:89-198`
**Confidence**: 82%
- Problem: The `walk_and_read` function is 106 lines with cyclomatic complexity around 12 (6 match arms with continue, 2 early returns, nested if conditions, the walker iteration loop). It has a linear filter pipeline structure where each step either continues (skip) or falls through, but the nesting of match/continue patterns across 5 sequential checks makes the function long. Each check also constructs a different `SkipReason` variant, which is necessary but adds visual bulk.
- Fix: Extract the per-file processing logic into a helper that returns `Result<ReadFile, SkipReason>`, then the main loop becomes a clean dispatch. This is a judgment call -- the current structure is idiomatic Rust (match + continue pattern), and the linear flow is clear. The function is at the boundary of acceptable complexity:

```rust
fn try_read_file(
    abs_path: &Path,
    root: &Path,
) -> Result<ReadFile, SkipReason> {
    let lang = Language::from_path(abs_path)
        .ok_or_else(|| SkipReason::UnsupportedLanguage(abs_path.to_path_buf()))?;

    let metadata = fs::metadata(abs_path)
        .map_err(|e| SkipReason::ReadError {
            path: abs_path.to_path_buf(),
            error: e.to_string(),
        })?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(SkipReason::TooLarge { path: abs_path.to_path_buf(), size: metadata.len() });
    }

    let content = fs::read_to_string(abs_path)
        .map_err(|_| SkipReason::NonUtf8(abs_path.to_path_buf()))?;

    if is_tree_sitter_language(lang) && is_minified(&content) {
        return Err(SkipReason::Minified(abs_path.to_path_buf()));
    }

    let sha256 = sha256_hex(content.as_bytes());
    let rel_path = abs_path.strip_prefix(root).unwrap_or(abs_path).to_path_buf();

    Ok(ReadFile { rel_path, lang, content, sha256 })
}
```

This would reduce `walk_and_read` to ~40 lines (walker setup + loop + dispatch).

### MEDIUM

**`FileManifest::load` has 5 early-return fallback branches (62 lines, CC ~10)** - `manifest.rs:109-171`
**Confidence**: 80%
- Problem: The `load` method has 5 distinct conditions that return `Ok(Self::new(...))` (file not found, empty file, corrupt header, wrong version, wrong root), followed by a parse loop with its own match/continue. While each branch is individually simple, the accumulated early returns make it moderately complex to reason about which conditions lead to an empty manifest vs. an error. The function reads well top-to-bottom but is near the complexity threshold.
- Fix: Consider extracting the header validation into a small helper:

```rust
fn validate_header(header_line: &str, project_root: &Path, format_version: u32) -> bool {
    let header: ManifestHeader = match serde_json::from_str(header_line) {
        Ok(h) => h,
        Err(_) => return false,
    };
    if header.version != format_version {
        return false;
    }
    let canonical = project_root.canonicalize().unwrap_or_else(|_| project_root.to_path_buf());
    PathBuf::from(&header.root) == canonical
}
```

This is a soft recommendation -- the current code is readable and the early-return pattern is idiomatic.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Duplicated `path_key` construction pattern** - `index.rs:198`, `index.rs:226` (Confidence: 70%) -- The expression `rf.rel_path.to_string_lossy().replace('\\', "/")` appears twice in `build_index`. If extracted to a method on `ReadFile` (e.g., `ReadFile::path_key(&self) -> String`), it removes duplication and makes the intent clearer.

- **Test helper duplication across test modules** - `manifest_tests.rs:32-46` vs `manifest.rs:243-264` (Confidence: 65%) -- The test file re-implements `encode_field_map` and `decode_field_map` locally instead of using the `super::` imports. This creates two copies of the same logic. The test helpers should call the production functions directly.

- **`parse_args` manual argument parsing** - `index.rs:83-127` (Confidence: 62%) -- Hand-rolling argument parsing when the project already depends on `clap` adds incidental complexity. The `next_value` helper with mutable index is a common source of off-by-one errors. For now the 4-flag scope is manageable, but if more flags are added, migrating to clap derive would reduce complexity.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code is well-structured with clean module separation (types, walk, manifest, index), good documentation, and each file stays under 330 lines. The two HIGH findings (`build_index` at 85 lines and `walk_and_read` at 106 lines) are at the upper boundary of acceptable complexity rather than deeply problematic -- both use linear pipeline patterns with clear numbered comments. The extraction refactors suggested would improve long-term maintainability but the code is understandable as-is. The MEDIUM finding on `FileManifest::load` is similarly at the boundary. No function exceeds critical thresholds (200 lines, CC > 20, nesting > 6). The clean separation of types.rs (pure data, 93 lines) and the small utility functions (`is_minified`, `sha256_hex`, `project_root_hash`) demonstrate good decomposition discipline overall.
