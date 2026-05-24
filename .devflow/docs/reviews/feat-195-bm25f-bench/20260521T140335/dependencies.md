---
focus: dependencies
reviewer: Reviewer
timestamp: 2026-05-21T14:03:35Z
pr: 247
branch: feat/195-bm25f-bench
head: 101385eb
---

# Dependencies Review

## Summary

The new `rskim-bench` crate adds 12 direct dependencies, all of which already exist in the workspace — no new transitive dependencies are introduced (Cargo.lock diff is only the new crate entry). One dependency (`tempfile`) is redundantly listed in both `[dependencies]` and `[dev-dependencies]`, and the internal path dependencies omit `version` fields, deviating from the convention used by sibling crates.

## Findings

### should-fix — Redundant `tempfile` in `[dev-dependencies]`
- **File:** `crates/rskim-bench/Cargo.toml:35`
- **Confidence:** 95%
- **Description:** `tempfile` is listed in both `[dependencies]` (line 32) and `[dev-dependencies]` (line 35). Since the production code in `main.rs` uses `tempfile::tempdir()` for benchmark index directories, the `[dependencies]` entry is correct and necessary. The `[dev-dependencies]` entry is completely redundant — Cargo resolves `tempfile` from `[dependencies]` for both production and test compilation. This adds noise and could cause confusion about where the dependency actually belongs.
- **Suggestion:** Remove the `[dev-dependencies]` section entirely (since `tempfile` is its only entry):
  ```toml
  # Remove these lines:
  [dev-dependencies]
  tempfile = { workspace = true }
  ```

### should-fix — Internal path dependencies missing `version` field
- **File:** `crates/rskim-bench/Cargo.toml:20-22`
- **Confidence:** 82%
- **Description:** The three internal path dependencies (`rskim-search`, `rskim-core`, `rskim-research`) use path-only references without a `version` field. While the crate is `publish = false` so this has no functional impact, other workspace crates follow a convention of including the version alongside the path. For example, `crates/rskim/Cargo.toml` uses `rskim-core = { version = "2.10.0", path = "../rskim-core" }` and `crates/rskim-research/Cargo.toml` uses `rskim-core = { version = "2.9.0", path = "../rskim-core" }`. Matching the convention makes dependencies self-documenting and prevents surprises if `publish = false` is ever removed.
- **Suggestion:** Add version fields to match workspace convention:
  ```toml
  rskim-search = { version = "0.1.0", path = "../rskim-search" }
  rskim-core = { version = "2.10.0", path = "../rskim-core" }
  rskim-research = { version = "0.1.0", path = "../rskim-research" }
  ```

### informational — No new transitive dependencies introduced
- **File:** `Cargo.lock`
- **Confidence:** 98%
- **Description:** The Cargo.lock diff is minimal (30 lines) — it only adds the `rskim-bench` package entry itself. All 12 direct dependencies (`anyhow`, `clap`, `rskim-core`, `rskim-research`, `rskim-search`, `serde`, `serde_json`, `sha2`, `tempfile`, `tree-sitter`, `tree-sitter-go`, `tree-sitter-python`, `tree-sitter-rust`) already exist in the workspace and resolve to existing versions. This is an exemplary pattern for additive crates — zero supply chain expansion.

### informational — All dependencies are actively used
- **File:** `crates/rskim-bench/Cargo.toml:19-32`
- **Confidence:** 95%
- **Description:** Source grep confirms every declared dependency is imported and used:
  - `sha2` — `split.rs` (deterministic train/test split via SHA-256)
  - `tree-sitter`, `tree-sitter-rust`, `tree-sitter-python`, `tree-sitter-go` — `extract/*.rs` (AST-based symbol extraction)
  - `anyhow` — `harness.rs`, `main.rs`, `qrel.rs` (error propagation)
  - `clap` — `main.rs` (CLI argument parsing)
  - `serde`, `serde_json` — `types.rs`, `report.rs`, `main.rs` (serialization)
  - `rskim-core` — `types.rs`, `extract/mod.rs`, `qrel.rs` (Language enum)
  - `rskim-search` — `harness.rs`, `configs.rs`, `types.rs` (BM25F search engine)
  - `rskim-research` — `main.rs` (corpus management)
  - `tempfile` — `main.rs` (temporary index directories)

### informational — Edition and workspace conventions followed
- **File:** `crates/rskim-bench/Cargo.toml:4`
- **Confidence:** 98%
- **Description:** The crate uses `edition = "2024"`, consistent with all other workspace crates. All external dependencies use `{ workspace = true }` inheritance rather than inline version specifiers, which is the correct pattern for this workspace. The `publish = false` flag is appropriate for an internal benchmarking tool. Clippy lint configuration (`unwrap_used = "deny"`, `expect_used = "deny"`, `panic = "deny"`) matches the workspace standard.

### informational — `cargo audit` not available
- **File:** N/A
- **Confidence:** 100%
- **Description:** `cargo audit` is not installed in this environment, so automated advisory scanning was not possible. However, since no new crate versions are introduced (all deps already existed in the lockfile), the advisory surface is unchanged from the main branch. No manual review of known advisories is warranted.

## Verdict
APPROVED_WITH_CONDITIONS

The dependency manifest is clean and well-structured. No new supply chain surface is introduced. Two minor hygiene issues should be addressed: (1) remove the redundant `tempfile` dev-dependency, and (2) add version fields to internal path dependencies to match workspace conventions. Neither is blocking.

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | - | 0 | 2 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Dependencies Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS
