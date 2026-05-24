# Documentation Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**Missing CHANGELOG entry for the core feature of this PR** - `CHANGELOG.md:10-12`
**Confidence**: 95%
- Problem: The `[Unreleased]` section documents `skim dig`/`skim nslookup` (#168) and `skim make`/`skim gmake` (#167) but has no entry for the index builder pipeline (#182), which is the primary feature of this PR. The PR adds `skim search index` with walk/manifest/pipeline orchestration, incremental builds, and parallel classification -- all undocumented in the CHANGELOG. This is a significant user-facing feature addition.
- Fix: Add a CHANGELOG entry under `[Unreleased] > ### Added`:
```markdown
- **`skim search index` subcommand** -- Build or update the n-gram search index for the current project. Walk/classify/build pipeline with parallel tree-sitter classification (rayon), JSONL manifest sidecar for incremental builds (SHA-256 cache hits skip re-classification), atomic write ordering (.skpost -> .skidx -> .skfiles), minified file detection, and 50K file cap. `--force` flag for full rebuild, `--root` for explicit project root, `--max-files` override. (#182)
```

**Help text advertises unimplemented options without marking them** - `crates/rskim/src/cmd/search/mod.rs:66-80`
**Confidence**: 90%
- Problem: The `skim search` help text lists `--lang`, `--ast`, `--json`, and `--limit` options, plus query examples (`skim search "fn parse"`, `skim search --lang rust "impl Iterator"`), but the query path returns `"not yet implemented"`. A user running any of these documented examples will get a failure with a confusing message. The help text presents these as working features.
- Fix: Either remove the unimplemented options/examples from the help text until they are implemented, or clearly mark them as upcoming. For example:
```rust
fn print_help() {
    println!(
        "\
Usage: skim search <SUBCOMMAND> [OPTIONS]

Search code using layered n-gram indexing.

Subcommands:
  index    Build or update the search index for the current project

Options:
  -h, --help       Print this help message

Examples:
  skim search index              Build the search index
  skim search index --force      Rebuild from scratch

Query mode (skim search <QUERY>) is not yet implemented."
    );
}
```

### MEDIUM

**Missing `Language::as_str` entry in CLAUDE.md API reference** - `CLAUDE.md`
**Confidence**: 80%
- Problem: The `Language` enum gained a new public method `as_str(self) -> &'static str` in `crates/rskim-core/src/types.rs` (lines 124-149). This is a public API addition to the core crate but is not mentioned anywhere in CLAUDE.md. While CLAUDE.md does not exhaustively document every method, `as_str` introduces a new serialization convention (stable lowercase identifiers for languages) that future contributors should be aware of when adding new language variants.
- Fix: Add a note to the "Adding New Language" section in CLAUDE.md, e.g., after step 7:
```markdown
8. **Add `as_str()` arm** - Add a lowercase string mapping in `Language::as_str()` for serialization (e.g., `Self::NewLang => "newlang"`)
```

**CLAUDE.md missing `search` in Subcommands listing** - `CLAUDE.md:152-180`
**Confidence**: 85%
- Problem: The Subcommands section in CLAUDE.md lists all subcommand categories (Meta/utility, Analysis, Multi-category dispatchers, Direct tool subcommands) but does not include `skim search` or `skim search index`. The `search` subcommand is already in the `KNOWN_SUBCOMMANDS` array and dispatch table.
- Fix: Add `search` to the appropriate section in CLAUDE.md, for instance under Analysis or as its own group:
```markdown
**Search:**
- `search index` -- Build or update the n-gram search index (`--force`, `--root`, `--max-files`)
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**README.md not updated with `search` subcommand** - `README.md:579-587`
**Confidence**: 82%
- Problem: The README's "Project Status" section was updated for `dig`/`nslookup` in the Infra line (line 585) but does not mention the search index feature anywhere. The `skim search index` subcommand is a significant new capability that users would want to discover.
- Fix: This can be deferred to the release that ships the query interface, but a note could be added under an appropriate heading. At minimum, a brief mention under "Command Output Compression" or a new section. Given that query mode is not yet implemented, a short note is sufficient:
```markdown
**Search (preview):**
- `skim search index` -- Build n-gram search index with incremental updates
```

## Pre-existing Issues (Not Blocking)

No pre-existing documentation issues at CRITICAL severity were found in unchanged code.

## Suggestions (Lower Confidence)

- **Test file module docs could describe test strategy** - `crates/rskim/src/cmd/search/index_tests.rs:1` (Confidence: 65%) -- The module doc is a bare `//! Integration tests for the index builder pipeline (index.rs).` A brief description of the test strategy (tempdir isolation, `--index-dir` for test cache separation, coverage of full/incremental/force builds) would help future contributors understand the approach.

- **`ReadOutcome` enum could document its design rationale in the type doc** - `crates/rskim/src/cmd/search/walk.rs:57-71` (Confidence: 70%) -- The doc comment explains what each variant means but does not explain _why_ this enum exists instead of using `io::Error`. The commit message (8356d30) explains the rationale (avoiding fragile string-match error classification), but that context is lost to future readers of the code. Adding a single sentence like "Using typed variants avoids string-matching on io::Error messages to distinguish too-large files from genuine I/O failures" would help. (Note: The doc at line 62-63 partially addresses this but could be more explicit.)

- **`unsafe` block in `sha256_hex` lacks `// SAFETY:` convention** - `crates/rskim/src/cmd/search/walk.rs:332` (Confidence: 75%) -- The comment uses `// SAFETY:` but Rust convention uses `// SAFETY:` as a doc-level comment on the unsafe block itself. This is minor and the safety justification is present, but matching the convention (`// SAFETY: ...` directly above the `unsafe` block) would be more consistent.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Documentation Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

The new code modules (index.rs, walk.rs, manifest.rs, types.rs) are very well documented internally -- module-level docs with data flow diagrams, doc comments on all public and private functions with parameter descriptions, error conditions, and architectural rationale. The code-level documentation is exemplary. However, the user-facing documentation has significant gaps: the CHANGELOG omits the PR's core feature, the help text advertises unimplemented options as if they work, and CLAUDE.md/README.md are not updated for the new subcommand. These gaps would mislead both end users and future contributors.
