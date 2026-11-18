---
feature: tokens-run-budget
name: Whole-run --tokens budget for multi-file reads
description: "Use when changing --tokens for a glob, directory or file list, the run planner, the run notice or trailer, or SKIM_PASSTHROUGH for a file list. Keywords: run_budget, set_fits, plan_cut, run_rung, RunNoticeFacts."
category: domain-knowledge
directories: [crates/rskim/src/multi.rs, crates/rskim/src/run_budget.rs, crates/rskim/src/cascade.rs, crates/rskim/src/process.rs, crates/rskim/src/output/mod.rs, crates/rskim/src/main.rs, crates/rskim/tests/]
created: 2026-11-18
updated: 2026-11-18
---

# Whole-run --tokens budget for multi-file reads

## Rules

- **KB-INV-1** One `--tokens N` budget covers the exact final stdout of a multi-file run: headers, `-n` numbers and markers. stderr is not counted. `cli_tokens_run_budget` counts the captured stdout with the same cl100k counter.
- **KB-INV-2** `run_budget_applies` is the one predicate. It is true when `batch` is set and a token budget is set. It starts the run stage and it makes the stage own the `--tokens` stderr lines.
- **KB-INV-3** Only the multi-file options set `batch`. Single-file and stdin reads keep the per-file cascade and its stderr lines. They never take the run stage.
- **KB-INV-4** A run that fits N writes the same stdout bytes as before the run stage. `RUN_SUM_STOP_FACTOR` and `MAX_DROP_RETRIES` bound the extra work. The goldens in `tests/fixtures/tokens_run_budget/` pin the bytes.
- **KB-INV-5** Step-down follows the cascade order from the start mode, one mode for the whole run. The walk skips a rung only when that rung empties every body that pass 1 left non-empty.
- **KB-INV-6** `write_run` writes the exact string whose joined count passed. No second assembly exists. `piece_prefix` builds the header bytes for the streaming writer and the run stage.
- **KB-INV-7** A changed run prints exactly one run notice on stderr, and it replaces the aggregate notice. A run that fits prints the aggregate notice as before.
- **KB-INV-8** The trailer is the last stdout line. Its numbers come from plan state, never from file text. It is a `// ` comment line with the omitted count, the shown-of-total count and the `SKIM_PASSTHROUGH=1` hint.
- **KB-INV-9** Exit is 0 for a cut or an omission. The run fails only when every file fails. The notice total counts successes only.
- **KB-INV-10** Output is the same for any `--jobs` value. Files keep path order. All token work runs in one rayon pool that `process_files` builds.
- **KB-INV-11** A run-level cut is never cached. Only plain `--mode m` rung results enter the cache. A warm run gives the same stdout and stderr as the cold run.
- **KB-INV-12** Budget arithmetic saturates. A missing count means over budget (`usize::MAX`). No content candidate leaves the planner without an exact count or a byte bound.
- **KB-AP-1** Do not print a candidate that the exact count did not accept. The planner returns the trailer only after the retries fail.
- **KB-AP-2** Do not add a per-file cascade stderr line to a multi-file `--tokens` run. Pass 1 uses `CascadeLines::Quiet`. The run notice carries the remedy.
- **KB-AP-3** Do not cut a body that is `Fixed`. Retained pass-1 bodies and line-bound bodies (`--max-lines`, `--last-lines`) stay whole or are omitted.
- **KB-AP-4** Do not add a second notice type. The run notice shares one line builder with the default bounded notice. Do not charge it to an analytics row.
- **KB-AP-5** Do not change the empty rule back to "any emptied body skips the rung". A partly empty rung keeps the affected pass-1 bodies and can still fit.
- **KB-AP-6** Do not run a test of this feature with `SKIM_PASSTHROUGH` in the parent environment. The binary under test reads it and fails every compression assertion.

## Overview

`--tokens N` used to apply to each file of a multi-file read. The combined output had no bound. Now one budget bounds the whole output of a glob, a directory or an explicit file list. A glob or directory that matches one file takes the same path.

A run goes through these steps. Pass 1 runs the old per-file code and counts each piece. If the set fits N, the run writes it. If not, the run steps down through the cascade modes. If no mode fits, the planner cuts each over-share file on whole lines and omits trailing files. One stderr notice then tells the reader what changed.

## Run stage in `process_files`

`prepare_results` in `multi.rs` is the entry. It runs in the pool that `process_files` builds.

1. **Pass 1.** `process_file` runs for each path with the real options. Each success gets a `Fixed` piece with prefix, count and floor.
2. **Fit test.** `select_run` calls `set_fits` on the pass-1 set. A set that fits goes out unchanged, with the aggregate notice as before.
3. **Rung walk.** `walk_rungs` (from `cascade.rs`) goes over the modes after the start mode. For each rung, `run_rung` with `RungKind::Probe` transforms every file with `token_budget: None`. It stops counting when one piece is over N or the running sum is over 2N. Workers still transform after the stop, because the empty test needs every body.
4. **Cut base.** When no rung fits, reuse the last non-empty probe if it is complete. If it is incomplete, rebuild that rung with `RungKind::CutBase`. If no probe gives a cut base, use the start mode. Each worker pre-cuts only cuttable bodies over N.
5. **Plan.** `plan_cut` in `run_budget.rs` chooses how many files stay (`kept_prefix`). It splits the budget with `shares` (max-min). It cuts over-share bodies that permit cuts, then counts the joined string.
6. **Write.** `write_run` writes the accepted string. Error lines, the notice, stats and analytics follow, from the final bodies.

`final_results` replaces each shown output with the final body and clears the cached token counts when the run changed. Omitted successes get no analytics row.

## Planner rules (`run_budget.rs`)

- **`Body`** has three forms. `Whole` can be cut. `PreCut` stores the worker's kept lines, its own marker, the total view lines and the marked count. `Fixed` keeps the body as-is, or the planner omits it.
- **`set_fits`** is the one fit rule for pass 1 and every rung. A set whose bytes are at most N fits with no count. Otherwise it fails when a piece count is over N or the sum is over `RUN_SUM_STOP_FACTOR` times N. If neither fails, one exact count of the joined string decides.
- **`kept_prefix`** is one linear pass over the floors. A `Fixed` floor is the full piece count. For other bodies, the floor is the lesser of the piece count and prefix plus the compact marker. The trailer cost uses the widest numbers. There is no binary search.
- **Cuts** count hidden view lines (`ViewLines`). A `PreCut` body whose marked count fits its share keeps the worker marker as it is. Otherwise the core cuts `kept` again with `SourceTotal(total_view_lines)`.
- **Retries.** If the exact count is over N, the planner drops the last kept file, at most `MAX_DROP_RETRIES` times. Then it returns the trailer only.
- **Floor.** When N is below the trailer cost, stdout is the trailer line only. This is the one case where stdout can be over N. `--tokens 0` and values over `MAX_TOKEN_BUDGET` are still usage errors.

## Run notice and trailer (`output/mod.rs`)

`RunNoticeFacts` holds the budget, requested and served modes, differing and total counts, cut and omitted counts, and the last shown and first omitted paths. `run_view_marker` builds the line. `view_notice_line` is the shared builder, and the default bounded notice uses it too. Each clause drops out when its count is zero: the step-down clause, the cut clause and the omitted clause. Paths go through `display_path`, so the notice stays on one line. The notice names only remedies that a multi-file run accepts: a larger `--tokens`, fewer files, `SKIM_PASSTHROUGH=1`.

`cut` counts each shown file with a run cut. The planner never cuts a retained pass-1 body. Its source-line marker stays intact and does not add to `cut`.

`run_trailer_line` reuses the shared elision marker. It counts successful files only, and it starts on its own line.

## Passthrough for an explicit list

`SKIM_PASSTHROUGH=1` streams raw bytes in sorted path order, with no headers or size limit. It does this with or without `--tokens`. `resolve_explicit_paths` shares the path order, duplicate removal and path-resolution diagnostics with the transform path. The gate sits in `main.rs` after the stdin-mix check.

For an explicit list, `passthrough_explicit_files` reports each input error with `display_path` and continues to the next file. Bytes already sent to stdout stay in the output. If at least one file succeeds, the exit code is 0. If every file fails, the exit code is 1. An output error stops the run at once. A closed stdout pipe exits 141.

## Anti-Patterns

**KB-AP-1.** The 2N stop and the byte-length shortcut only reject or accept early. The exact count of the joined string is the last check. BPE joins can add tokens, so a sum of piece counts is not a bound. `joined_overcounts_stop_after_three_drops` pins the retry count.

**KB-AP-2.** `cascade_for_token_budget` prints in its fit arm, in `fallback_line_truncate` and in the serde cascade. A multi-file pass 1 calls it, so the call must be quiet. Stdout bytes and the returned mode are equal for `Print` and `Quiet`. The cascade tests cover each arm. The count-failure warning always prints.

**KB-AP-3.** `run_rung` keeps the pass-1 body if it has no language, the transform fails, or the rung empties the body. Language comes from `explicit_lang` or the path extension, not the shebang. These bodies and line-bound bodies are `Fixed`. A rung error adds no stderr line. Omission is a path-order suffix, so a large `Fixed` body can end the shown prefix. `partly_empty_cut_keeps_the_exact_pass_one_body_or_omits_it` checks this rule.

**KB-AP-4.** Batch analytics rows are unmeasured by design. See the long comment in `multi.rs` and the `analytics` knowledge base. The rows carry the requested mode and a null notice.

**KB-AP-5.** `partly_empty_signatures_keep_the_const_body_and_fit` and `a_partly_empty_rung_keeps_pass_one_and_can_fit` fail when `all` becomes `any` in the empty test of `run_rung`.

**KB-AP-6.** An exported `SKIM_PASSTHROUGH` also turns on the new list passthrough. Tests remove it from the base environment and set it only on the child. They also remove `SKIM_REWRITTEN_FROM` and `SKIM_DEBUG`. Prove a test discriminates by changing one production line and watching the test fail. The phase notes record such proofs for the bound, the trailer count, the empty flag, the notice predicate and the empty rule.

## Gotchas

**KB-INV-1.** With `-n`, the run cuts annotated text. `process_file` returns annotated text only, so the fence and heredoc pull-back can miss. The count stays exact. Without a cut line map, the marker counts view lines.

**KB-INV-2, KB-INV-3.** `multi_options` in `main.rs` and its test twin are the only places that set `batch`. Add a new batch caller there, or the run stage will not start.

**KB-INV-5.** The 2N early stop can reject a rung that would fit, for example many tiny files with a small N. This is a chosen default, not a measured threshold. The cost is a smaller served view, never an over-budget one.

**KB-INV-6.** `piece_prefix` takes the index in all results, not in successes. A failed first file still leaves the separator before the next header. `piece_prefix_matches_stream_bytes_after_a_failed_first_file` pins this.

**KB-INV-7.** The aggregate notice counts all paths. The run notice counts successes. Both are correct for their own case. A cold fitting run no longer names the stepped-down view of one file, because the per-file lines stop.

**KB-INV-8.** The trailer keeps the shared marker form, with a dash and "1 files" for one file. It also prints with `--no-header`. A body with no final newline gets a newline before the trailer.

**KB-INV-10.** `--show-stats` totals come from the shown final bodies. The suffix says "across S file(s)" with S as the shown count. The original count comes from pass 1.

**KB-INV-11.** A cache tamper test must target a batch rung entry. A one-match glob has that key. A single-file read can have another notice-cost key. `cli_cache` has the cold, warm, jobs and tamper tests.

**Cost.** Over N, every rung transforms every file, and one cut-base re-run can follow (cache hits). Memory holds the pass-1 set, at most two probe sets, the cut base and one transient file per worker. The cost was measured once in the PR and is not gated.

**Other limits.** `display_path` replaces non-ASCII characters with `?`. A count failure inside the run prints no new line and fails closed.

## Tests

- `crates/rskim/tests/cli_tokens_run_budget.rs`: end-to-end bounds, floor, step-down, markers, trailer, notice, analytics, closed stderr, jobs equality, goldens.
- `crates/rskim/tests/cli_cache.rs`: cold and warm equality, tamper of a batch rung, run cuts not cached.
- `crates/rskim/tests/cli_passthrough_coverage.rs`: explicit list passthrough, collectors, closed pipe, tiny-budget floor.
- `crates/rskim/tests/cli_token_budget.rs`: the glob test checks the whole-run bound.
- Unit tests live in `run_budget.rs`, `multi.rs`, `cascade.rs` and `output/mod.rs`.

## Key Files

- `crates/rskim/src/multi.rs`: `prepare_results`, `select_run`, `run_rung`, `make_piece`, `precut_piece`, `final_results`, `piece_prefix`, `write_run`, `resolve_explicit_paths`, `passthrough_explicit_files`.
- `crates/rskim/src/run_budget.rs`: `Body`, `Piece`, `set_fits`, `plan_cut`, `RUN_SUM_STOP_FACTOR`, `MAX_DROP_RETRIES`.
- `crates/rskim/src/output/mod.rs`: `RunNoticeFacts`, `run_view_marker`, `run_trailer_line`, `view_notice_line`.
- `crates/rskim/src/cascade.rs`: `CascadeLines`, `walk_rungs`, `serde_rungs`.
- `crates/rskim/src/process.rs`: `run_budget_applies`, the `run_transform` choice of `Quiet` or `Print`.
- `crates/rskim/src/main.rs`: `--tokens` help text, `MAX_TOKEN_BUDGET`, the explicit-list passthrough gate, `multi_options`.
- `crates/rskim/src/cmd/init/helpers.rs`: the `skim init` guidance line for `--tokens`.

## Related

- Feature knowledge: `default-view-bound` (the single-input character limit; shares the rung walk and the notice line builder), `file-wrapper-fidelity` (the single-file compact marker and its stderr channel), `analytics` (unmeasured batch rows).
- Source: `README.md`, `crates/rskim/README.md`, `CHANGELOG.md` (state that one budget covers the whole output of a run).
