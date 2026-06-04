# Complexity Review Report

**Branch**: feature/189-cli-temporal-search-flags -> main
**Date**: 2026-05-27T16:23

## Cross-Cycle Awareness

Prior cycle resolved 19/23 issues, including extraction of `parse_temporal_flag` helper and `run_query` parameter reduction via `&Flags`. These improvements are visible in the current code (e.g., `parse_temporal_flag` exists as a standalone function, `run_query` takes `&Flags` not individual parameters). This review focuses on remaining complexity after those refactors.

## Issues in Your Changes (BLOCKING)

### HIGH

**`format_temporal_text` function handles 4 enum variants with repeated empty-check + header logic (71 lines)** - `crates/rskim/src/cmd/search/temporal.rs:338-411`
**Confidence**: 82%
- Problem: `format_temporal_text` is a large match with 4 arms (Hotspots, Coldspots, Risks, Cochanges). The Hotspots/Coldspots arms share structure via `|` binding but still have internal branching (4 decision paths within one function). Each arm repeats the pattern: check empty, print header, print column headers, iterate rows. The function is at 74 lines with cyclomatic complexity around 8 (the `is_hot` checks add conditional branches within the combined arm).
- Fix: The hot/cold arms are already consolidated via `|` binding with `is_hot` discriminator, which is the right approach. However, the two `if is_hot` checks inside the combined arm could be simplified by computing both strings upfront:
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
This eliminates one level of branching.

### MEDIUM

**`resort_partners_by_temporal` builds parallel score Vec then index-sort then clone permutation (54 lines)** - `crates/rskim/src/cmd/search/temporal.rs:277-331`
**Confidence**: 83%
- Problem: The function builds a parallel `Vec<f64>` of scores, then creates an index Vec, sorts it, and applies the permutation by cloning all items into a new Vec. The three-phase approach (compute scores -> sort indices -> apply permutation via clone) is correct but adds cognitive overhead. The `match sort_mode` inside the score-building closure duplicates the `.map()` pipeline structure, differing only in which DB method is called.
- Fix: Extract the score-fetching closure to reduce the match duplication:
```rust
let score_fn: Box<dyn Fn(&CochangeRow) -> anyhow::Result<f64>> = match sort_mode {
    TemporalSort::Hot | TemporalSort::Cold => Box::new(|row| {
        let partner = cochange_partner(row, normalized);
        Ok(db.hotspot_for_file(partner)?.map(|h| h.score).unwrap_or(0.0))
    }),
    TemporalSort::Risky => Box::new(|row| {
        let partner = cochange_partner(row, normalized);
        Ok(db.risk_for_file(partner)?.map(|r| r.risk_score).unwrap_or(0.0))
    }),
};
let scores: Vec<f64> = partners.iter().map(|row| score_fn(row)).collect::<anyhow::Result<_>>()?;
```
This consolidates the two match arms into a single pipeline with a pluggable score function.

**`parse_flags` manual argument parser is 73 lines with 14 match arms** - `crates/rskim/src/cmd/search/mod.rs:192-265`
**Confidence**: 80%
- Problem: The `parse_flags` function uses a manual `while` loop with index manipulation (`i += 1` for consumed tokens) and 14 match arms. The cyclomatic complexity is approximately 14 (one per arm plus the outer loop). While each arm is individually simple, the function as a whole requires careful reading to verify correctness of the `i += 1` advancement, especially for two-token flags like `--limit <val>` and `--blast-radius <path>`. The temporal flag parsing was already extracted to `parse_temporal_flag` (prior cycle fix), which helps.
- Fix: This is at the upper edge of acceptable complexity for a hand-rolled arg parser. The extraction of `parse_temporal_flag` and `parse_limit_value` was the right call from the prior cycle. No further extraction is strictly necessary, but if the flag count grows further, consider a table-driven approach. Currently acceptable.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`format_temporal_json` repeats envelope-construction pattern across 3 arms** - `crates/rskim/src/cmd/search/temporal.rs:475-535` (Confidence: 70%) -- Each match arm constructs a typed envelope struct and calls `serde_json::to_string_pretty`. The repetition is mitigated by using distinct typed structs (which prevents field drift), so this is a structural choice rather than duplication. No action needed unless a 4th output variant is added.

- **`apply_temporal_enrichment` Hot/Cold vs Risky arms have similar annotate+sort pattern** - `crates/rskim/src/cmd/search/temporal.rs:556-603` (Confidence: 65%) -- The Hot/Cold arm calls `annotate_hotspots` then sorts, and the Risky arm calls `annotate_risks` then sorts. The sorting closures differ in which field they extract. A generic approach could unify them, but with only 2 arms and clear separation of concern (hotspot vs risk scoring), the current explicit match is arguably more readable.

- **`normalize_blast_radius_path` has 4 nested levels for path resolution** - `crates/rskim/src/cmd/search/temporal.rs:39-107` (Confidence: 62%) -- The function handles absolute vs relative paths, root-relative vs CWD-relative fallback, canonicalization, and prefix stripping. Nesting reaches 4 levels in the relative+CWD fallback branch. However, path normalization is inherently multi-step and the comments explain each level clearly. The function is 68 lines which is within the warning zone but justified by the domain complexity.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 8/10

The codebase demonstrates strong complexity management throughout this 3,140-line change:

- **Good decomposition**: The temporal module is split into clear responsibility areas: path normalization, DB helpers, standalone dispatch, enrichment, and formatters. Each function has a single responsibility.
- **Prior cycle improvements visible**: `parse_temporal_flag`, `parse_limit_value`, `resolve_blast_radius_filter`, and `cochange_partner_paths` are all well-extracted helpers that keep parent functions manageable.
- **Type-driven dispatch**: `TemporalSort` enum and `TemporalQueryOutput` enum make the control flow explicit and exhaustive. No magic strings or boolean flags for mode selection.
- **Functions under threshold**: All functions are under 80 lines. No function exceeds cyclomatic complexity of 14. Nesting stays at 4 or below.
- **Test coverage is comprehensive**: 1,129 lines of temporal tests plus 261 lines of query tests cover empty tables, edge cases, JSON shape validation, and cross-cutting concerns.

The two MEDIUM findings are patterns that are at the edge of acceptable complexity but not violations. The single HIGH finding (`format_temporal_text`) is a legitimate simplification opportunity that would reduce branching.

**Recommendation**: APPROVED_WITH_CONDITIONS

Conditions: The HIGH finding (`format_temporal_text` branching) is a minor code clarity improvement, not a correctness issue. Approve if the author chooses to address it in this PR or defer to a follow-up.
