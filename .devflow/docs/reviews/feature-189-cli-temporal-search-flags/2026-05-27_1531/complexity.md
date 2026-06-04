# Complexity Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T15:31

## Issues in Your Changes (BLOCKING)

### HIGH

**`parse_flags` function is approaching critical complexity (15+ match arms, 90 lines)** - `crates/rskim/src/cmd/search/mod.rs:155`
**Confidence**: 85%
- Problem: The `parse_flags` function is a single `while`/`match` loop with 15 match arms spanning lines 155-244 (90 lines). The temporal flags (`--hot`, `--cold`, `--risky`, `--blast-radius` plus `--blast-radius=`) added 5 new arms, pushing cyclomatic complexity to approximately 18 (each `if let`, match arm, and conditional guard is a decision point). This is in the WARNING-to-CRITICAL range (10-20). The function also has 7 mutable locals initialized at the top, making it harder to reason about state evolution.
- Fix: Extract temporal flag parsing into a dedicated helper. The `--hot`/`--cold`/`--risky` arm with its inner `match` and mutual-exclusivity check and the `--blast-radius` arm (both `--blast-radius <val>` and `--blast-radius=<val>`) can be factored into `parse_temporal_flag(arg, next_arg, &mut temporal_sort, &mut blast_radius) -> Result<bool>` that returns whether it consumed the arg. This keeps `parse_flags` under 15 arms and isolates temporal validation.

**`run_query` has 7 parameters** - `crates/rskim/src/cmd/search/mod.rs:386`
**Confidence**: 82%
- Problem: `run_query` now takes 7 parameters (`text`, `limit`, `json`, `root_override`, `temporal_sort`, `blast_radius`, `analytics`). The complexity skill flags functions with >5 parameters as HIGH severity. The two new temporal parameters were added to an already-long signature.
- Fix: Group related parameters into a struct. The simplest improvement: `run_query` already constructs a `QueryConfig` internally -- pass `&Flags` (or a subset struct) directly rather than unpacking all fields into individual parameters. Alternatively, move `temporal_sort` and `blast_radius` into `QueryConfig` since they influence query execution.

### MEDIUM

**`format_temporal_text` has duplicated Hotspots/Coldspots arms (32 lines near-identical)** - `crates/rskim/src/cmd/search/temporal.rs:305`
**Confidence**: 85%
- Problem: The `Hotspots` arm (lines 310-325) and `Coldspots` arm (lines 326-341) contain nearly identical code: same column headers, same `writeln!` format string, same iteration pattern. The only differences are the empty-data message and the heading text. This is 32 lines of duplication within a single function, and the diff shows Cycle 1 consolidated the original version but split it back out when addressing the `or`-pattern approach. The function totals 80 lines (4 match arms, each with an empty-check early return and a formatting loop).
- Fix: Collapse the two arms using an `or`-pattern with a local `(empty_msg, header)` tuple:
  ```rust
  TemporalQueryOutput::Hotspots(rows) | TemporalQueryOutput::Coldspots(rows) => {
      let (empty_msg, header) = if matches!(output, TemporalQueryOutput::Hotspots(_)) {
          ("No hotspot data available.", format!("Hotspots (top {}, 90-day decay):\n", rows.len()))
      } else {
          ("No coldspot data available.", format!("Coldspots (top {}, least active):\n", rows.len()))
      };
      if rows.is_empty() { writeln!(w, "{empty_msg}")?; return Ok(()); }
      writeln!(w, "{header}")?;
      // ...shared column headers and row loop...
  }
  ```
  This was the pattern in the original diff but was apparently unwound. The `format_temporal_json` function (line 388) already uses this `or`-pattern successfully.

**`run_query` blast-radius path resolution block has 4 nesting levels** - `crates/rskim/src/cmd/search/mod.rs:410-433`
**Confidence**: 80%
- Problem: Lines 410-433 contain a conditional block that nests 4 levels deep: `if let (Some, Some)` -> `partners.iter().map` -> `if p.file_a == normalized` -> collect/insert. The `else if` branch at line 431 also has a compound boolean condition (`temporal_db.is_none() && (temporal_sort.is_some() || blast_radius.is_some())`). This section intermixes path resolution, cochange partner extraction, and warning emission in a single flow.
- Fix: Extract a helper function `resolve_blast_radius_filter(raw_path, db, root) -> Result<HashSet<String>>` that encapsulates path normalization, partner lookup, target file insertion, and the empty-data warning. The calling code becomes a simple `if let` assignment:
  ```rust
  let blast_radius_paths = match (blast_radius, &temporal_db) {
      (Some(raw), Some(db)) => Some(resolve_blast_radius_filter(raw, db, &root)?),
      _ => { /* emit warning if flags set but no db */ None }
  };
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`storage_ops.rs` top-N query methods are structurally identical (3x ~25 lines each)** - `crates/rskim-search/src/temporal/storage_ops.rs:187-269`
**Confidence**: 80%
- Problem: `top_hotspots`, `top_risks`, and `top_coldspots` follow an identical pattern: prepare SQL, `query_map` with `params![limit as i64]`, collect into `Vec<_>`, map errors. The only differences are the SQL string, the row struct, and the column-to-field mappings. This is ~75 lines of boilerplate that will need to be touched three times for any cross-cutting change (e.g., adding a new column or changing the error mapping).
- Fix: This is acceptable for now since Rust's type system makes generic row-mapping closures verbose, and each method is individually short (~25 lines). However, if a fourth query method is added, consider a private `fn query_ordered<T>(sql, limit, row_mapper) -> Result<Vec<T>>` helper.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`annotate_hotspots` and `annotate_risks` share identical structure** - `crates/rskim/src/cmd/search/temporal.rs:528,550` (Confidence: 70%) -- Both iterate `results.iter_mut()`, call a per-file DB lookup, construct a `TemporalAnnotation` on `Ok(Some)`, and emit a warning on `Err`. A generic `annotate_with<F>` helper could unify them, but the field assignments differ per variant, making the closure signature awkward. Low-value refactor.

- **`normalize_blast_radius_path` has 3 nesting levels in the relative-path branch** - `crates/rskim/src/cmd/search/temporal.rs:58-78` (Confidence: 65%) -- The `else` branch for relative paths nests: `if root_relative.exists()` -> `else` -> `match cwd_relative`. The logic is well-commented and each branch is short, but a `resolve_relative_path(p, root) -> Option<PathBuf>` helper could flatten it.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

The code is well-structured overall -- good use of helper extraction (`resort_partners_by_temporal`, `annotate_hotspots`, `annotate_risks`, `cochange_partner`) and each new function has a clear single responsibility. The main concerns are: (1) `parse_flags` accumulating match arms toward critical complexity, (2) `run_query` growing to 7 parameters, (3) a duplicated Hotspots/Coldspots text formatter, and (4) the blast-radius resolution block in `run_query` that could be extracted. None are critical, but addressing the HIGH items before merge prevents the complexity from compounding in subsequent PRs.
