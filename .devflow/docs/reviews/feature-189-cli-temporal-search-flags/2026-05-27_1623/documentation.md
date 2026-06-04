# Documentation Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T16:23
**Cycle**: 3 (cross-cycle: 23 prior issues, 19 fixed, 4 FP)

## Issues in Your Changes (BLOCKING)

### HIGH

**CHANGELOG.md missing entry for temporal search flags** - `CHANGELOG.md:8`
**Confidence**: 95%
- Problem: The `[Unreleased]` section in CHANGELOG.md has no entry for the temporal search flags (`--hot`, `--cold`, `--risky`, `--blast-radius`). This is a significant new public API surface (4 new CLI flags, 6 new public methods on `TemporalDb`, a new `file_filter` field on `SearchQuery`, and a schema migration to v2). The CHANGELOG already documents `skim search index` from issue #182 in the same Unreleased section, so this feature belongs alongside it.
- Fix: Add an entry under `### Added` in the `[Unreleased]` section:
```markdown
- **`skim search` temporal query flags** — Composable temporal search: `--hot` (hotspot sort), `--cold` (coldspot sort), `--risky` (bug-fix density sort), and `--blast-radius FILE` (co-change pre-filter). Flags work standalone (no query text) or combined with text queries for enriched/re-sorted results. New `temporal.rs` module with path normalization, staleness detection, and JSON/text output formatters. Per-file DB lookups (`hotspot_for_file`, `risk_for_file`, `cochanges_for_file`) with performance indexes (schema v2 migration). Graceful degradation when temporal DB is absent. (#189)
```

**CLAUDE.md subcommands section missing `search`** - `CLAUDE.md:152`
**Confidence**: 92%
- Problem: The Subcommands section in CLAUDE.md lists `heatmap` under "Analysis:" but does not list `search` at all. The `search` subcommand has existed since at least the `skim search index` feature (#182), and this PR adds substantial new temporal query functionality. This is the project's primary developer reference, and the omission means agents and contributors have no discoverable documentation of `skim search` flags, temporal options, or composition rules.
- Fix: Add `search` to the "Analysis:" category in the Subcommands section:
```markdown
**Analysis:**
- `heatmap` — Git history risk/coupling analysis: churn, co-change coupling, stability scores, author concentration, fix-after-touch, module encapsulation (`--json`, `--since`, `--last`, `--window`, `--path`, `--top`, `--insights`)
- `search` — Code search with BM25F n-gram indexing: `--build`, `--rebuild`, `--update`, `--stats`, `--install-hooks`, `--remove-hooks`, `--json`, `--limit N`, `--root PATH`. Temporal flags: `--hot`, `--cold`, `--risky` (mutually exclusive sort modes), `--blast-radius FILE` (co-change pre-filter). Composable with text queries.
```

### MEDIUM

**README.md has no mention of the search subcommand or temporal flags** - `README.md`
**Confidence**: 85%
- Problem: The README is the project's public-facing documentation. It documents the `heatmap` subcommand (line 95) but has zero mention of `skim search` or any of its flags. With 4 new composable temporal flags forming a significant user-facing feature, users have no way to discover this capability from the README.
- Fix: Add a section for the search subcommand near the heatmap documentation, covering basic usage examples and the temporal flag composition model. At minimum, the feature list should mention code search.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`run_temporal_standalone` missing doc-comment** - `crates/rskim/src/cmd/search/mod.rs:498`
**Confidence**: 85%
- Problem: `run_temporal_standalone` is a top-level dispatch function for a new execution path but has no doc-comment. All other `run_*` functions in this module follow the pattern of at least a brief description. While `pub(crate)` scope means it is not a public API, the function orchestrates temporal DB open, staleness check, query dispatch, and output formatting -- enough complexity to warrant a brief explanation.
- Fix: Add a doc-comment:
```rust
/// Execute a standalone temporal query (no text search).
///
/// Handles graceful degradation when the temporal database is absent,
/// staleness warnings, and JSON/text output formatting.
fn run_temporal_standalone(
```

**`resolve_blast_radius_filter` algorithm doc could mention the target-file inclusion** - `crates/rskim/src/cmd/search/mod.rs:407`
**Confidence**: 80%
- Problem: The doc-comment for `resolve_blast_radius_filter` says it "returns the full set of paths to restrict the search to (partners + the target file itself)" -- this is correct but the inclusion of the target file in the result set is a subtle behavioral detail that is easy to miss. The inline comment at line 438 explains the rationale well, but the doc-comment could surface this more prominently since it affects search behavior (text queries will match the target file itself, not just its partners).
- Fix: Consider making the doc-comment's first sentence more explicit:
```rust
/// Resolve blast-radius partner paths from the temporal database.
///
/// Returns the set of co-change partners PLUS the target file itself,
/// so text queries surface matches within the target in addition to
/// its coupling partners.
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Temporal DB schema migration documentation** - `crates/rskim-search/src/temporal/storage.rs:135` (Confidence: 70%) -- The v1-to-v2 migration adds 3 performance indexes but no migration guide or note in CLAUDE.md's architecture section about the temporal schema versioning strategy. For a library crate with external consumers, documenting the schema versioning approach (PRAGMA user_version, forward migration only) would help contributors.

- **`cochanges_for_file` LIMIT 10000 magic number** - `crates/rskim-search/src/temporal/storage_ops.rs:166` (Confidence: 65%) -- The hardcoded `LIMIT 10000` in the SQL query is undocumented as a constant. While the doc-comment is otherwise excellent (explains UNION ALL choice, index usage, canonical ordering), the 10K cap is implicit. A named constant or doc mention would help future maintainers understand the bound.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Documentation Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

The in-code documentation quality is notably strong: module-level `//!` docs, per-function doc-comments with `# Errors` sections, algorithm explanations, and "why" comments throughout `temporal.rs`. The code-level documentation is well above average. However, the project-level documentation (CHANGELOG, CLAUDE.md, README) has significant gaps for a feature that introduces 4 new CLI flags and 6 new public library methods. The CHANGELOG and CLAUDE.md entries are the two blocking items -- they are the primary discovery mechanisms for contributors and agents working on this codebase.
