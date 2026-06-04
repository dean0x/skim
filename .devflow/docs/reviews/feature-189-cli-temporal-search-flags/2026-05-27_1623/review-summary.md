# Code Review Summary - Cycle 3

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27_1623
**Cycle**: 3 (Incremental after Cycle 2: 19 fixed, 4 false positive)

## Merge Recommendation: CHANGES_REQUESTED

**Reason**: Seven blocking issues across security, architecture, consistency, testing, reliability, database, and documentation require resolution before merge. Two issues appear in multiple reviewer reports (subprocess timeout, prepare_cached usage), indicating high confidence patterns.

---

## Convergence Status

**Reviewer Consensus**: Strong agreement across 11 reviewers (security, architecture, performance, complexity, consistency, regression, testing, reliability, rust, database, documentation) on issue classification and severity. Cross-cycle patterns:

- **Subprocess timeout** flagged by 3 reviewers (security, reliability, database) at 80-85% confidence
- **CHANGELOG/CLAUDE.md documentation gaps** flagged by 1 reviewer at 95%/92% confidence
- **prepare_cached in query methods** flagged by 1 reviewer at 82% confidence
- **Missing staleness check in combined path** flagged by 1 reviewer at 82% confidence
- **JSON output consistency** flagged by 1 reviewer at 82% confidence
- **Test coverage gap** flagged by 1 reviewer at 82% confidence
- **Output.total reassignment** flagged by 1 reviewer at 83% confidence
- All other findings are singular with 80%+ confidence

**No disagreements** between reviewers on categorization or severity.

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| **Blocking (in your changes)** | 0 | 4 | 7 | 0 | **11** |
| **Should Fix (code you touched)** | 0 | 0 | 1 | 0 | **1** |
| **Pre-existing (not blocking)** | 0 | 0 | 0 | 0 | **0** |
| **Suggestions** | - | - | - | 12 | **12** |
| **TOTAL** | 0 | 4 | 8 | 12 | **24** |

---

## Blocking Issues (Must Fix Before Merge)

### CRITICAL
(none)

### HIGH

#### 1. JSON warning output inconsistent with typed-struct pattern
**Files**: `crates/rskim/src/cmd/search/mod.rs:510`
**Reviewer**: Consistency (82% confidence)
**Category**: Category 1 (Issues in Your Changes)

The standalone temporal path emits a warning as hand-formatted JSON `println!("{{\"warning\": \"...\"}}");` while all other JSON output uses typed `#[derive(Serialize)]` structs. This reintroduces the hand-formatted JSON pattern that cycle 2 specifically fixed. Cycle 2 moved to typed structs to prevent key drift.

**Fix**: Create a small typed `StatusJson` struct and use it consistently:
```rust
#[derive(Serialize)]
struct StatusJson<'a> { warning: &'a str }
let msg = StatusJson { warning: "no temporal data -- run 'skim heatmap' to populate" };
println!("{}", serde_json::to_string(&msg)?);
```

#### 2. format_temporal_text HIGH cyclomatic complexity (71 lines, CC~8)
**Files**: `crates/rskim/src/cmd/search/temporal.rs:338-411`
**Reviewer**: Complexity (82% confidence)
**Category**: Category 1 (Issues in Your Changes)

The `format_temporal_text` match has 4 arms with repeated empty-check + header logic. The hot/cold arms are already consolidated via `|` binding, but the internal `is_hot` checks add branching. Refactoring opportunity to reduce branching.

**Fix**: Compute header strings upfront to eliminate conditional branching:
```rust
let (empty_msg, header_msg) = if is_hot {
    ("No hotspot data available.", format!("Hotspots (top {}, 90-day decay):\n", rows.len()))
} else {
    ("No coldspot data available.", format!("Coldspots (top {}, least active):\n", rows.len()))
};
if rows.is_empty() {
    writeln!(w, "{empty_msg}")?;
    return Ok(());
}
writeln!(w, "{header_msg}")?;
```

#### 3. Unbounded subprocess in read_git_head — no timeout on git rev-parse HEAD
**Files**: `crates/rskim/src/cmd/search/temporal.rs:152-164`
**Reviewers**: Security (80%), Reliability (85%), Database (72%) — **Tripled confidence from cross-reviewer agreement**
**Category**: Category 1 (Issues in Your Changes)

The `read_git_head()` spawns `git rev-parse HEAD` via `Command::new("git").output()` with no timeout. The doc comment explicitly acknowledges the risk: "It is NOT safe to use on network-mounted repos or corrupted `.git` directories where the subprocess may hang indefinitely." However, no timeout is applied. If `.git` is on a network mount or corrupted, the CLI hangs forever with no upper bound.

**Fix**: Add a 5-second timeout via thread + channel pattern:
```rust
use std::time::Duration;

fn read_git_head(root: &Path) -> Option<String> {
    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "HEAD"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = child.wait_with_output();
        let _ = tx.send(result);
    });
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(output)) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        }
        _ => {
            drop(handle); // best-effort cleanup
            None
        }
    }
}
```

#### 4. CHANGELOG.md missing entry for temporal search flags
**Files**: `CHANGELOG.md:8`
**Reviewer**: Documentation (95% confidence)
**Category**: Category 1 (Issues in Your Changes)

The `[Unreleased]` section has no entry for the 4 new CLI flags (`--hot`, `--cold`, `--risky`, `--blast-radius`), 6 new public methods, and schema v2 migration. This is a significant public API surface addition.

**Fix**: Add under `### Added` in `[Unreleased]`:
```markdown
- **`skim search` temporal query flags** — Composable temporal search: `--hot` (hotspot sort), `--cold` (coldspot sort), `--risky` (bug-fix density sort), and `--blast-radius FILE` (co-change pre-filter). Flags work standalone (no query text) or combined with text queries for enriched/re-sorted results. New `temporal.rs` module with path normalization, staleness detection, and JSON/text output formatters. Per-file DB lookups (`hotspot_for_file`, `risk_for_file`, `cochanges_for_file`) with performance indexes (schema v2 migration). Graceful degradation when temporal DB is absent. (#189)
```

### MEDIUM

#### 5. Empty `--blast-radius=` value not rejected
**Files**: `crates/rskim/src/cmd/search/mod.rs:175-177`
**Reviewer**: Security (85% confidence)
**Category**: Category 1 (Issues in Your Changes)

The `--blast-radius=` (equals form) parsing uses `trim_start_matches("--blast-radius=")` which produces an empty string when the user passes `--blast-radius=` with no value. The empty string flows into `normalize_blast_radius_path("")` → `Path::new("")` which can resolve to the current directory on some platforms, producing a confusing error message.

**Fix**: Add an empty-string guard in `parse_temporal_flag`:
```rust
s if s.starts_with("--blast-radius=") => {
    let val = s.trim_start_matches("--blast-radius=");
    if val.is_empty() {
        anyhow::bail!("--blast-radius requires a file path");
    }
    *blast_radius = Some(val.to_string());
    Ok(false)
}
```

#### 6. Missing staleness check in combined text+temporal path
**Files**: `crates/rskim/src/cmd/search/mod.rs:446-496`
**Reviewer**: Architecture (82% confidence)
**Category**: Category 1 (Issues in Your Changes)

`run_temporal_standalone` checks temporal staleness via `check_temporal_staleness()` with a warning. However, `run_query` (combined text+temporal path) opens the temporal DB and uses it for enrichment without ever checking staleness. Users running `skim search "auth" --hot` get temporally-enriched results from a potentially stale DB with no warning, while `skim search --hot` warns them. This asymmetry means the combined path silently serves stale data.

**Fix**: Add a staleness check in `run_query` after opening the temporal DB:
```rust
if let Some(ref db) = temporal_db {
    if let Some(warning) = temporal::check_temporal_staleness(db, &root) {
        eprintln!("{warning}");
    }
}
```

#### 7. Per-file lookup methods use `prepare()` instead of `prepare_cached()`
**Files**: `crates/rskim-search/src/temporal/storage_ops.rs:162, 202, 236, 271`
**Reviewer**: Database (82% confidence)
**Category**: Category 1 (Issues in Your Changes)

The four new query methods (`cochanges_for_file`, `top_hotspots`, `top_risks`, `top_coldspots`) all use `prepare()` which compiles the SQL statement from scratch on every call. The existing write helpers use `prepare_cached()`. The `annotate_hotspots`/`annotate_risks` callers invoke `hotspot_for_file`/`risk_for_file` in loops over search results, so statements are compiled N times per enrichment pass.

**Fix**: Replace `prepare()` with `prepare_cached()` in all four methods:
```rust
let mut stmt = self.conn.prepare_cached(
    "SELECT file_a, file_b, count, jaccard FROM cochange WHERE file_a = ?1 \
     UNION ALL \
     SELECT file_a, file_b, count, jaccard FROM cochange WHERE file_b = ?1 \
     ORDER BY jaccard DESC LIMIT 10000",
).map_err(db_err)?;
```

#### 8. CLAUDE.md subcommands section missing `search`
**Files**: `CLAUDE.md:152`
**Reviewer**: Documentation (92% confidence)
**Category**: Category 1 (Issues in Your Changes)

The Subcommands section lists `heatmap` but does not list `search` at all, despite `search` being the project's primary code search tool with new temporal functionality. This is a project reference document.

**Fix**: Add `search` to the "Analysis:" category in Subcommands:
```markdown
**Analysis:**
- `heatmap` — Git history risk/coupling analysis: churn, co-change coupling, stability scores, author concentration, fix-after-touch, module encapsulation (`--json`, `--since`, `--last`, `--window`, `--path`, `--top`, `--insights`)
- `search` — Code search with BM25F n-gram indexing: `--build`, `--rebuild`, `--update`, `--stats`, `--install-hooks`, `--remove-hooks`, `--json`, `--limit N`, `--root PATH`. Temporal flags: `--hot`, `--cold`, `--risky` (mutually exclusive sort modes), `--blast-radius FILE` (co-change pre-filter). Composable with text queries.
```

#### 9. Missing error-path test for `resolve_blast_radius_filter` when temporal DB is `None`
**Files**: `crates/rskim/src/cmd/search/mod.rs:424-430`
**Reviewer**: Testing (82% confidence)
**Category**: Category 1 (Issues in Your Changes)

The `resolve_blast_radius_filter` function has a branch where `blast_radius` is `Some` but `temporal_db` is `None` — it prints a warning and returns `Ok(None)`. This degradation path (which users will hit when they haven't run `skim heatmap`) has no unit test. The function is only tested indirectly through integration tests that always provide a DB.

**Fix**: Add a unit test:
```rust
#[test]
fn test_resolve_blast_radius_filter_no_db() {
    let root = Path::new("/tmp");
    let result = resolve_blast_radius_filter(Some("src/auth.rs"), &None, root);
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}
```

#### 10. No assertion that `blast_radius_paths` FileId set is non-empty before injecting into query
**Files**: `crates/rskim/src/cmd/search/query.rs:75-83`
**Reviewer**: Reliability (82% confidence)
**Category**: Category 1 (Issues in Your Changes)

When `config.blast_radius_paths` is `Some(allowed_paths)`, the code builds a `file_ids` HashSet. If none of the allowed paths match entries in the manifest (e.g., path normalization differences), the result is `Some(empty_set)`, which silently yields zero results with no warning. The user sees "no results" with no indication the blast-radius filter eliminated everything due to a path mismatch.

**Fix**: Log a warning when the resulting `file_ids` set is empty:
```rust
if let Some(ref allowed_paths) = config.blast_radius_paths {
    let mut file_ids = std::collections::HashSet::new();
    for (idx, path) in sorted.iter().enumerate() {
        if allowed_paths.contains(*path) {
            file_ids.insert(rskim_search::FileId(idx as u32));
        }
    }
    if file_ids.is_empty() {
        eprintln!(
            "skim search: blast-radius filter matched 0 indexed files \
             (allowed {} paths, index has {} files)",
            allowed_paths.len(),
            sorted.len()
        );
    }
    sq.file_filter = Some(file_ids);
}
```

#### 11. Public struct `SearchQuery` gains required field without `Default` derive
**Files**: `crates/rskim-search/src/types.rs:368`
**Reviewer**: Regression (82% confidence)
**Category**: Category 1 (Issues in Your Changes)

`SearchQuery` is public and exported via `pub use`. The new `file_filter: Option<HashSet<FileId>>` field is added to a struct that does NOT derive `Default`. Any external downstream consumer constructing `SearchQuery` via struct literal syntax (not `SearchQuery::new()`) will get a compile error after upgrading.

**Fix**: Either add `#[non_exhaustive]` (if not already present), document the change in CHANGELOG as a minor API surface change, or verify no external consumers use struct-literal construction. Since `SearchQuery::new()` initializes `file_filter: None`, internal callers and the bench crate are unaffected.

---

## Should-Fix Issues (Code You Touched)

#### 1. `output.total` reassignment after enrichment masks actual result count
**Files**: `crates/rskim/src/cmd/search/mod.rs:484`
**Reviewer**: Architecture (83% confidence)
**Category**: Category 2 (Issues in Code You Touched)

After `apply_temporal_enrichment` (which sorts in-place but does not add/remove elements), `output.total` is reassigned to `output.results.len()`. Since enrichment never changes the slice length, this assignment is always a no-op. However, it signals to future readers that enrichment might change the count, which is misleading. Worse, if someone later adds filtering inside enrichment, the `total` field would silently diverge.

**Fix**: Remove the `output.total = output.results.len();` line. Add a comment explaining the contract:
```rust
// Enrichment mutates annotations and order but never changes the result set size.
// Do not reassign output.total.
```

---

## Pre-existing Issues (Not Blocking)

(none)

---

## Suggestions (Lower Confidence)

Fourteen suggestions at 60-72% confidence, distributed across security (2), performance (2), complexity (4), consistency (3), reliability (3), rust (3), and database (3). All are improvement opportunities below the blocking threshold. Key patterns:

- Subprocess timeout acknowledged with doc-comment but not enforced (3 reviewers, 60-72% confidence)
- Clone in permutation apply (bounded to window size 100-500, 60% confidence)
- Staleness warning may leak git commit hashes (60% confidence, stderr-only)
- Various code clarity/efficiency improvements for edge cases

---

## Quality Assessment

### Strengths

1. **Comprehensive test coverage**: 1,390 lines of new tests across temporal, query, and storage modules covering empty tables, edge cases, JSON/text format validation, path normalization, flag parsing, and staleness detection.

2. **Strong performance practices**: BM25F pre-filter in innermost loop, UNION ALL over OR, pre-truncate window bounding, lazy DB open, hoisted manifest traversal, result-set bounded annotation.

3. **Security-first design**: Parameterized SQL everywhere, path traversal defense via canonicalize+strip_prefix, no shell invocation, database file permissions (0o600), capacity limits (500K row clamp), input validation, graceful degradation.

4. **Clear architecture**: Temporal storage operations in library crate, CLI-specific concerns in binary crate, clean module boundaries, Strategy Pattern dispatch for language-specific parsing.

5. **Error handling**: Consistent `anyhow::Result` at CLI layer, `Result<T, SearchError>` at library layer, per-file errors emit warnings rather than aborting enrichment.

6. **Code-level documentation**: Module-level `//!` docs, per-function doc-comments with `# Errors` sections, algorithm explanations, "why" comments throughout.

### Weaknesses

1. **Project-level documentation gaps**: CHANGELOG, CLAUDE.md subcommands, and README missing entries for the new temporal flags. This is a significant public API surface.

2. **Subprocess timeout not enforced**: Self-documented risk in comments but no implementation barrier.

3. **Test coverage edge case**: Staleness check test silently skips in git-less environments; blast-radius degradation path not tested.

4. **Consistency remnants**: One instance of hand-formatted JSON reintroduces pattern that cycle 2 improved away from.

5. **Minor query optimization**: `prepare()` instead of `prepare_cached()` on hot-path per-file lookups.

---

## Cross-Cycle Comparison

**Cycle 2**: 23 issues total — 19 fixed, 4 false positive (all confirmed as false positive: annotate N+1 at 0.2ms threshold, path traversal defense via canonicalize, structural duplication below refactoring threshold, JSON key error/warning intentional per acceptance criteria).

**Cycle 3**: 24 issues total — 11 blocking (4 HIGH, 7 MEDIUM), 1 should-fix (MEDIUM), 0 pre-existing, 12 suggestions (60-72% confidence).

**Regression analysis**: Zero regressions detected on items fixed in cycle 2 (BM25F pre-filter, hoisted sorted_paths, UNION ALL, pre-truncate window, in-place sort, per-file N+1, structural duplication all verified intact).

**New patterns in cycle 3**: 
- Documentation gaps (CHANGELOG, CLAUDE.md, README) not flagged in prior cycles because code quality was the focus
- JSON consistency reintroduction (cycle 2 fixed this pattern; one instance missed in this PR)
- Test coverage edge case (graceful degradation path untested)
- Query optimization opportunity (prepare vs prepare_cached)

---

## Action Plan

### Immediate (Required for Merge)

1. **Fix subprocess timeout** in `read_git_head` — Add 5-second timeout via thread+channel
2. **Add JSON StatusJson struct** — Replace hand-formatted JSON with typed struct
3. **Add staleness check in run_query** — Emit warning for combined text+temporal path
4. **Simplify format_temporal_text** — Compute headers upfront to reduce branching
5. **Update CHANGELOG.md** — Add entry for temporal search flags under `[Unreleased]`
6. **Update CLAUDE.md** — Add `search` subcommand to Subcommands section
7. **Replace prepare() with prepare_cached()** — Four query methods in storage_ops.rs
8. **Add empty-string guard** for `--blast-radius=` — Reject empty values explicitly
9. **Add unit test** for `resolve_blast_radius_filter` with missing DB
10. **Add warning** when blast-radius filter matches zero files — Diagnose path mismatches
11. **Handle SearchQuery field addition** — Document in CHANGELOG or add `#[non_exhaustive]`

### Should-Fix (Code You Touched)

1. **Remove output.total reassignment** after enrichment — Add contract comment

### Optional Improvements (Next PR)

- Add `#[ignore]` to git-dependent tests with CI skip validation
- Document subprocess risk mitigation approach in devflow docs
- Extract ResourceManager score computation in `resort_partners_by_temporal` (minor refactoring)
- Update README with search subcommand examples
- Document `prepare_cached` performance pattern in codebase guidelines

---

## Summary

This is a well-executed 3,140-line feature that adds substantial temporal search capabilities with strong architecture, performance, and security practices. However, 11 blocking issues (4 HIGH, 7 MEDIUM) must be resolved before merge:

- **Subprocess timeout** (HIGH, security/reliability): Non-blocking subprocess poses indefinite hang risk
- **Documentation gaps** (HIGH, documentation): CHANGELOG and CLAUDE.md missing entries for public API
- **JSON consistency** (HIGH, consistency): One instance of hand-formatted JSON reintroduces pattern cycle 2 fixed
- **Format_temporal_text complexity** (HIGH, complexity): Cyclomatic complexity reduction opportunity
- **Empty blast-radius value** (MEDIUM, security): Equals-form parsing doesn't reject empty values
- **Missing staleness check** (MEDIUM, architecture): Combined path doesn't warn of stale temporal data
- **prepare_cached** (MEDIUM, database): Hot-path queries compile statements repeatedly
- **Test coverage** (MEDIUM, testing): Graceful degradation path untested
- **Empty filter warning** (MEDIUM, reliability): Silent zero-results when filter matches nothing
- **SearchQuery field** (MEDIUM, regression): Public struct field addition without `#[non_exhaustive]`
- **output.total reassignment** (MEDIUM, architecture): Should-fix in code you touched

All are actionable and fixable. The 1 should-fix item is a minor code clarity improvement.

**Merge Status**: CHANGES_REQUESTED. Approve after addressing the 11 blocking issues (estimated 2-3 hours of targeted fixes).
