# Regression Review Report

**Branch**: feat/195-bm25f-bench -> main
**Date**: 2026-05-22
**PR**: #247

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**CLI flag renamed from `--output` to `--format` for bench and tune subcommands -- breaking for existing scripts** - `crates/rskim-bench/src/main.rs:82`, `crates/rskim-bench/src/main.rs:101`
**Confidence**: 82%
- Problem: The `BenchArgs` and `TuneArgs` structs previously used `--output` (field name `output: String`) for the format flag. This PR renames the flag to `--format` (field name `format: OutputFormat`). Any existing scripts or CI that invoked `cargo run --bin rskim-bench -- bench --output json` or `cargo run --bin rskim-bench -- tune --output json` will break with an "unexpected argument" error. The `ReportArgs` struct already used `--format`, so this is a consistency fix (resolving an issue flagged in the prior review), but it is still a breaking CLI change.
- Mitigating factors: The crate is `publish = false` and internal-only. No external consumers exist. No scripts or CI reference these flags outside the crate (verified by search). The PR description explicitly states "zero changes to any existing crate."
- Fix: Acceptable as-is given internal-only status. If backward compatibility were needed, a `#[arg(alias = "output")]` attribute would preserve the old flag name.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

**`TuningResult` serde compatibility: `Vec<f32>` changed to `[f32; FIELD_COUNT]` -- old JSON files will fail to deserialize if field count differs** - `crates/rskim-bench/src/types.rs:85-87`
**Confidence**: 80%
- Problem: `TuningResult.best_field_boosts` and `TuningResult.best_field_b` changed from `Vec<f32>` to `[f32; FIELD_COUNT]`. Both serialize to JSON arrays, so wire format is compatible when the length matches. However, if any previously-saved JSON bench results (from `bench --format json`) exist with a different array length, deserializing them into the new type will fail. With `FIELD_COUNT = 8` and the old code always producing 8-element vectors, this is safe in practice, but the implicit contract is now strict.
- Severity: LOW -- the crate is internal-only, not published, and the old JSON files are ephemeral benchmark artifacts.

## Suggestions (Lower Confidence)

- **`run_bench` FileId starts at 0 for each parallel repo -- differs from `run_tune` which reassigns globally unique IDs** - `crates/rskim-bench/src/main.rs:258` (Confidence: 72%) -- In `run_bench`, each repo independently gets FileIds starting at 0 because repos are processed independently (par_iter, each gets its own index). In `run_tune`, repos are loaded in parallel then IDs are reassigned globally. The bench case is correct because each repo's index is independent, but the asymmetry could confuse future maintainers. Consider documenting why bench uses per-repo IDs while tune requires global IDs.

- **`EvalResult` type removed without deprecation** - `crates/rskim-bench/src/types.rs` (Confidence: 65%) -- The `EvalResult` struct (query, reciprocal_rank, found_in_top_k, rank) was removed entirely. No code references it (verified by grep), so this is safe. However, it was a `pub` type with `Serialize, Deserialize` derives, so any external consumer (even a local script) that imported it would break. Given `publish = false`, this is informational only.

- **Error swallowed in tuning closure returns 0.0 MRR** - `crates/rskim-bench/src/main.rs:393-402` (Confidence: 68%) -- When `evaluate_split` fails inside the `coordinate_descent` closure, the error is logged to stderr (capped at 5) and 0.0 is returned. The post-loop warning at line 406-410 reports total error count. This is a reasonable design since the closure signature requires `f64`, but repeated failures could silently degrade tuning quality by treating error configs as "worst possible" rather than "unknown."

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 1 |

**Regression Score**: 9/10
**Recommendation**: APPROVED

## Regression Checklist

- [x] No exports removed without deprecation -- `EvalResult` removed but zero consumers exist (verified)
- [x] Return types backward compatible -- `aggregate_results` now returns `anyhow::Result<BenchResult>` (additive error info, not breaking since all call sites updated)
- [x] Default values unchanged -- CLI defaults preserved (`markdown` format, `.bench-corpus` dir)
- [x] Side effects preserved -- stderr logging maintained, parallelism is additive
- [x] All consumers of changed APIs updated -- `run_on_files` signature change (added `repo_url` param) updated in all 6 call sites (3 in main.rs, 3 in tests)
- [x] `evaluate_split` signature change (added `bm25f_override` param) updated in all 4 call sites
- [x] `QrelInput.content` changed from `String` to `&str` -- all construction sites updated
- [x] CLI flag rename (`--output` -> `--format`) -- no external scripts affected
- [x] `TuningResult` field types (`Vec<f32>` -> `[f32; FIELD_COUNT]`) -- serde-compatible
- [x] No files outside `rskim-bench` modified (except `Cargo.lock` adding `rayon`)
- [x] All 92 bench tests pass
- [x] All 4,238 workspace tests pass (0 failures)
- [x] Commit messages match implementation -- all batch refactors verified against code

## Analysis Notes

This PR is a well-executed batch of refactoring, deduplication, and parallelization changes to the internal `rskim-bench` crate. The changes address issues flagged in the prior review (hardcoded field counts, duplicate file-loading logic, inconsistent CLI flags, `Vec` vs fixed-size arrays, parser reuse, and more). All modifications are confined to the bench crate and its lock file entry. No existing crate's public API, behavior, or test surface is affected. The PR delivers on its stated scope: "zero changes to any existing crate."
