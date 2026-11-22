---
feature: default-view-bound
name: Default single-input view output limit
description: "Use when changing the default skim FILE or stdin output limit, the rung walk, the cut budget, the bound cache state or the bound notice. Keywords: MAX_STDOUT_CHARS, default_view, char_budget, BoundNotice."
category: domain-knowledge
directories: [crates/rskim/src/, crates/rskim-core/src/transform/]
created: 2026-11-18
updated: 2026-11-18
---

# Default single-input view output limit

## Rules

- **KB-INV-1** The limit is strict: stdout characters plus the printed stderr notice characters must be below `MAX_STDOUT_CHARS`. Count Unicode scalar values (`chars().count()`), not bytes. `output::within_output_limit` is the one predicate; the post-hook capture check uses it too.
- **KB-INV-2** `default_read` is true when the user gives no `--mode`, `--max-lines`, `--last-lines`, `--tokens` or `--symbol`. `mode_given` comes from clap `value_source`, not from the mode value. `file_process_options` sets `trunc.char_budget` only for a default single-file or stdin read. A default batch uses `RunBudget::Chars` for the whole run instead. Raw reads bypass both paths.
- **KB-INV-3** The rung walk order is structure, signatures, types. The walk returns `Fits`, `NoneFits`, `AllEmpty` or `NoOutput`. A rung that is empty after `trim()`, or degraded, never fits.
- **KB-INV-4** The cut budget is `limit - 1 - trailing_newline - notice_chars`. The `1` is the strict margin. The `trailing_newline` is 1 when the rung text ends with `\n`, because the core adds that newline after its search. The notice is the one built with `cut: true`.
- **KB-INV-5** `truncate_with_count_source` re-measures after the literal pull-back. A pull-back keeps fewer lines, so the omitted count can gain digits and the marker can grow.
- **KB-INV-6** `enforce_default_assembly` is the last check. It fails closed: a total at or above the limit becomes the marker line only, with `cut` set. It never panics in release.
- **KB-INV-7** A warm cache hit re-checks stored content bytes plus the rebuilt notice against the limit. A total at or above the limit is a miss, not a hit.
- **KB-INV-8** The cache key has the suffix `|char_budget:<n>` only when a budget is set. Schema version 4 and later store `BoundState` (`NotFired` or `Fired`). A budgeted read of an entry without `BoundState` is a miss.
- **KB-INV-9** One constructor, `process::single_file_notice`, builds the notice for the fit measure, the printed line and the analytics charge. `bound_outcomes_measure_print_and_charge_one_notice` pins this.
- **KB-INV-10** Boundary tests use the totals limit-1 (accepted) and limit (refused). Prove each test by one-character mutation of the production line, not by reading the test.
- **KB-INV-11** A unit test that writes the cache has `#[serial_test::serial]` and sets its own tempdir `SKIM_CACHE_DIR`. It restores the old value at the end.
- **KB-AP-1** Do not measure the fit with a notice that has `cut: false` and then cut with that notice. The cut notice is longer. Use the `cut: true` notice for the cut budget.
- **KB-AP-2** Do not reserve only the strict margin. An exact candidate fit then gives a view that has only the marker.
- **KB-AP-3** Do not add a second notice line or a second notice builder. A bound outcome replaces the legacy notice. A second builder makes the printed and charged costs differ silently.
- **KB-AP-4** Do not serve cached bytes because the stored state says "fired". The stored state is not the authority for the limit. Re-measure.
- **KB-AP-5** Do not cut a view without a source line map when a map exists. Without the map, the omitted count is in view space and the count is wrong.

## Overview

The default character limit counts final stdout and the view notice. For `skim FILE` or `skim -`, it applies per input. For a directory, glob or explicit list, it applies to the whole run. A one-match glob or directory, or a list that dedups to one path, still uses the run path.

Typed modes, `--symbol`, `--max-lines`, `--last-lines`, `--tokens` and hook file rewrites do not use the default character limit. Line limits stay per file. `--tokens N` on a multi-file read sets a separate limit on the whole stdout. `SKIM_PASSTHROUGH=1` and `--passthrough` serve raw output.

For a single input, `process.rs` (`default_view`) chooses the view. `cascade.rs` walks the modes, and `truncate_with_count_source` cuts whole lines. `cache.rs` stores and replays the outcome. For a batch, see `tokens-run-budget` for the shared run stage and its fit, rung and cut limits.

## Decision flow in `default_view`

1. Step A keeps the old behavior. The output-size guard runs on the structure view with the legacy notice charge. If view plus notice is below the limit, return it. `bound` is `None`.
2. If the raw text (with line numbers when asked) is below the limit, serve raw with no label. `bound` is `None`, `guardrail_triggered` is true.
3. Otherwise walk the rungs. `Fits` serves that mode with `cut: false`. `NoneFits` cuts the last non-empty rung on whole lines with `cut: true`. `AllEmpty` and `NoOutput` (no language, or degraded parse) cut the raw text with `raw_line_cut` and the notice names mode full.
4. Run `enforce_default_assembly` on the result.

The first rung is reused from the caller when its mode matches the requested mode and the guard did not replace it. Later rungs are transformed again with line numbers on, so a source line map always exists for an exact omitted count. JSON, YAML and TOML (serde languages) have one structure rung; they do not step down.

## Cut details

- The count source is `LineMap` when the map covers all view lines. It counts source lines after the last kept source line. Map value 0 marks a synthetic line and does not advance the source cut. With a short or absent map the source is `ViewLines`: hidden view lines.
- `SourceTotal` is the older rule (source total minus kept view lines). The token budget path still uses it.
- When line numbers are shown, the measure adds a prefix width per line: `ilog10(source lines) + 2`. The marker has no number.
- The core pull-back moves the cut out of a multi-line string literal or Markdown fence. If no clean prefix fits, it keeps the marker.

## Cache

`BoundState::Fired` stores the served mode name, the original view size and `cut`. A read parses the mode with `Mode::parse` and requires the stored effective mode to equal it; otherwise it is a miss. The cache shape is always `File`; stdin is not cached. A default read and an explicit `--mode structure` read use different keys, so neither serves the other in either order.

## Anti-Patterns

**KB-AP-1, KB-AP-2.** The fit test uses a `cut: false` notice, because a fitting view has no cut clause. The cut path uses a `cut: true` notice, which has the extra clause. The core adds the final newline after its own budget search, so reserve it apart from the strict margin. Pinned by `cut_rung_reserves_the_final_newline_at_an_exact_candidate_boundary` (cases with and without a trailing newline, `extra` 0 and 1).

**KB-AP-3.** `write_result_and_stats` prints the line and `record_file_analytics` charges it through `file_analytics_notice`. Both call `single_file_notice` with the same mode name, `view_differs` and bound. Keep both calls adjacent in the flow.

**KB-AP-4, KB-INV-7.** `try_cached_result` calls `notice_chars` with the stored `view_differs` and `bound`. The cached text may come from an older limit or a hand-edited entry.

**KB-AP-5.** The cut closure must pass the map through `cut_line_map` so later line-number annotation keeps source numbers and the marker line has none.

## Gotchas

**KB-INV-1.** The two docs (`docs/modes.md`, `docs/usage.md`) state the limit in prose. Change them when `MAX_STDOUT_CHARS` changes. The init guidance text also names the limit and a test edits that text.

**KB-INV-5.** The pull-back loop does at most `open` checks. Each failed check removes at least one line, so it ends with a fitting prefix or the marker-only fallback. A marker-only fallback uses the compact form without the hint.

**KB-INV-6.** The assembly fallback marker is built from the raw line count. It is not a view of the content; it only keeps the output below the limit.

**KB-INV-8.** Unbudgeted inputs keep their old hashes, so no cache of a typed mode is lost. A change of the bound state shape needs a schema version bump.

**KB-INV-10.** Test the boundary on both sides. A test at limit-1 only does not detect a `<=` mutation. Change the production comparison by one character and see the test fail; a test that stays green does not discriminate.

**KB-INV-11.** Two tests that set `SKIM_CACHE_DIR` at the same time race. Use `serial` and a private tempdir. Do not share one with another cache test.

## Key Files

- `crates/rskim/src/process.rs`: `default_view`, `enforce_default_assembly`, `try_cached_result`, `single_file_notice`, `notice_chars`, `raw_line_cut`, stdin passthrough with `BoundShape::Stdin`.
- `crates/rskim/src/cascade.rs`: `RungWalk`, `walk_rungs`, `select_for_char_budget`, `CharBudgetRung`.
- `crates/rskim/src/cache.rs`: `BoundState`, `CACHE_SCHEMA_VERSION`, key suffix `char_budget`.
- `crates/rskim/src/main.rs`: `file_process_options`, `file_analytics_notice`, `record_file_analytics`.
- `crates/rskim/src/output/mod.rs`: `MAX_STDOUT_CHARS`, `within_output_limit`, `BoundNotice`, `BoundShape`, `bounded_view_marker`.
- `crates/rskim-core/src/transform/truncate.rs`: `TruncationCountSource` (`SourceTotal`, `LineMap`, `ViewLines`), `truncate_with_count_source`.
- `crates/rskim/tests/cli_default_view_bound.rs`: end-to-end bound tests (step-down, cuts, stdin, unbounded selections, warm cache).

## Related

- Feature knowledge: `symbol-read` (one declaration with its own cache key), `file-wrapper-fidelity` (notice and fidelity rules), `analytics` (cost charge), `tokens-run-budget` (the shared run planner and notice rules).
- Source: `crates/rskim/src/cmd/rewrite/post_hook.rs` (shares `within_output_limit`), `docs/modes.md`, `docs/usage.md`.
