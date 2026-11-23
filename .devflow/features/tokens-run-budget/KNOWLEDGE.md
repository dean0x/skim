---
feature: tokens-run-budget
name: Multi-file --tokens run budget and the shared run-budget planner
description: "Use when changing --tokens for a file list or glob, the run planner and its caller trailer, the run notice, or the heatmap --map budget. Keywords: run_budget, set_fits, plan_cut_with_trailer, RunNoticeFacts."
category: domain-knowledge
directories: [crates/rskim/src/multi.rs, crates/rskim/src/run_budget.rs, crates/rskim/src/cascade.rs, crates/rskim/src/process.rs, crates/rskim/src/output/mod.rs, crates/rskim/src/main.rs, crates/rskim/src/cmd/heatmap/map.rs, crates/rskim/src/cmd/heatmap/mod.rs, crates/rskim/tests/]
created: 2026-11-18
updated: 2026-11-24
---

# Multi-file --tokens run budget and the shared run-budget planner

## Rules

- **KB-INV-1** One `--tokens N` budget covers the exact final stdout of a multi-file run: headers, `-n` numbers and markers. stderr is not counted. `cli_tokens_run_budget` counts the captured stdout with the same cl100k counter.
- **KB-INV-2** `run_budget_applies` is token-only: it is true when `batch` and a token budget are set. It selects `RunBudget::Tokens` and gives the run stage the `--tokens` stderr lines. A default batch selects `RunBudget::Chars` instead. `process_files` resolves the budget once; `RunBudget` lives in `output/mod.rs`.
- **KB-INV-3** Only the multi-file options set `batch`. Single-file and stdin reads keep the per-file cascade and its stderr lines. They never take the run stage.
- **KB-INV-4** A run that fits N writes the same stdout bytes as before the run stage. `RUN_SUM_STOP_FACTOR` and `MAX_DROP_RETRIES` bound the extra work. The goldens in `tests/fixtures/tokens_run_budget/` pin the bytes.
- **KB-INV-5** Step-down follows the cascade order from the start mode, one mode for the whole run. The walk skips a rung only when that rung empties every body that pass 1 left non-empty.
- **KB-INV-6** `write_run` writes the exact string whose joined count passed. No second assembly exists. `piece_prefix` builds the header bytes for the streaming writer and the run stage.
- **KB-INV-7** A changed run prints exactly one run notice on stderr, and it replaces the aggregate notice. A run that fits prints the aggregate notice as before.
- **KB-INV-8** The trailer is the last stdout line. Its numbers come from plan state, never from file text. In a multi-file run it is a `// ` comment line with the omitted count, the shown-of-total count and the `SKIM_PASSTHROUGH=1` hint. A caller of the planner supplies its own trailer form (see KB-INV-13).
- **KB-INV-9** Exit is 0 for a cut or an omission. The run fails only when every file fails. The notice total counts successes only.
- **KB-INV-10** Output is the same for any `--jobs` value. An explicit list keeps command-line order. Each glob or folder is sorted by path inside its own argument. A direct list compares resolved path text and keeps the first match. `z.ts` and `./z.ts` count as two paths. Rewritten `cat`, `head`, and `tail` keep repeated operands. `process_files` uses one rayon pool for plan and stats counts.
- **KB-INV-11** A run-level cut is never cached. Only plain `--mode m` rung results enter the cache. A warm run gives the same stdout and stderr as the cold run.
- **KB-INV-12** Budget arithmetic saturates. A missing count means over budget (`usize::MAX`). No content candidate leaves the planner without an exact count or a byte bound. A failed count at any planner stage gives the trailer-only result, for a multi-file run as for any other caller.
- **KB-INV-13** `plan_cut_with_trailer` is the one planner core. The caller gives the budget kind, the counter, a trailer-line function and a lazy trailer-cost function. `plan_cut` and `plan_char_cut` are the multi-file callers with `run_trailer_line`, and `set_fits` stays the count-free fit rule. Their output bytes did not change when the core took a caller trailer.
- **KB-INV-14** `skim heatmap --map` is the second caller. Every map piece is `Fixed` with floor equal to count. The first piece is the header, not a file. The trailer says `[skim] S of T files`, with S as kept pieces minus the header. If the header is over the budget or any count fails, stdout is the trailer-only line. A heuristic count is refused: `map_token_count` returns no count until cl100k is ready.
- **KB-AP-1** Do not print a candidate that the exact count did not accept. The planner returns the trailer only after the retries fail, or at once when a count fails.
- **KB-AP-2** Do not add a per-file cascade stderr line to a multi-file `--tokens` run. Pass 1 uses `CascadeLines::Quiet`. The run notice carries the remedy.
- **KB-AP-3** Do not cut a `Fixed` body. In a token run, retained pass-1 bodies and line-bound bodies stay whole or are omitted. A character run can cut retained pass-1 bodies.
- **KB-AP-4** Do not add a second notice type. The run notice shares one line builder with the default bounded notice. Do not charge it to an analytics row.
- **KB-AP-5** Do not change the empty rule back to "any emptied body skips the rung". A partly empty rung keeps the affected pass-1 bodies and can still fit.
- **KB-AP-6** Do not run a test of this feature with `SKIM_PASSTHROUGH` in the parent environment. The binary under test reads it and fails every compression assertion.
- **KB-AP-7** Do not give the map trailer the `SKIM_PASSTHROUGH` remedy or `run_trailer_line`. The map trailer names `--tokens`, because the map has no raw form to pass through. A new planner caller supplies its own trailer line and cost.
- **KB-AP-8** Do not give the map planner a counter that guesses. Without a ready cl100k count the map returns the trailer only.

## Overview

A glob, directory or explicit file list uses one budget for the whole run. `--tokens N` counts stdout in tokens. A default run counts stdout and its view notice in Unicode scalar values. The total must be below `MAX_STDOUT_CHARS`. A one-match glob or directory, or a list that dedups to one path, still uses the run path.

Pass 1 runs the per-file code and counts each piece. If the set fits, the run writes it. If not, the run tries smaller views. If no mode fits, the planner cuts whole lines or omits trailing files. One stderr notice states what changed.

The planner in `run_budget.rs` has a second caller: `skim heatmap --map` (`cmd/heatmap/map.rs`). It reuses the planner for its token budget and its own trailer, and it has no step-down, no cut and no stderr notice.

## Run stage in `process_files`

`prepare_results` in `multi.rs` is the entry. It runs in the pool that `process_files` builds.

1. **Pass 1.** `process_file` runs for each path with the real options. Each success gets a `Fixed` piece that borrows the pass-1 body. Character piece counts add the prefix and body counts. Token piece counts use the joined text.
2. **Fit test.** `select_run` calls `set_fits` on the pass-1 set. A set that fits goes out unchanged, with the aggregate notice as before.
3. **Rung walk.** `walk_rungs` (from `cascade.rs`) goes over the modes after the start mode. For each rung, `run_rung` with `RungKind::Probe` transforms every file with `token_budget: None`. It stops counting when one piece is over N or the running sum is over 2N. Workers still transform after the stop, because the empty test needs every body.
4. **Cut base.** When no rung fits, reuse the last non-empty probe if it is complete. If it is incomplete, rebuild that rung with `RungKind::CutBase`. If no probe gives a cut base, use the start mode. Each worker pre-cuts only cuttable bodies over N.
5. **Plan.** `plan_cut` (or `plan_char_cut`) in `run_budget.rs` chooses how many files stay (`kept_prefix`). It splits the budget with `shares` (max-min). It cuts over-share bodies that permit cuts, then counts the joined string.
6. **Write.** `write_run` writes the accepted string. Error lines, the notice, stats and analytics follow, from the final bodies.

`final_results` uses the final bodies and clears stale token counts for a changed run. Stats and analytics still count tokens. Omitted files get no row, and both notice columns stay NULL for shown files.

### Run limits

Token runs use N for `fit`, `rung` and `cut`. Character runs subtract the strict margin and a notice reserve from `MAX_STDOUT_CHARS`:

- `fit` reserves the aggregate notice for pass 1.
- `rung` reserves the widest step-down notice with no cut, omission or path clauses.
- `cut` reserves the widest notice with cut and omission counts and the two longest display paths.

The limits saturate and satisfy `cut <= rung <= fit`. `seal_run` checks the exact stdout and notice before output. If needed, it uses only the trailer and removes path clauses from the notice. File errors, warnings, the `Processed` summary, stats and debug lines are outside the limit. Per-file stderr errors have no run count limit.

## Planner rules (`run_budget.rs`)

- **`Body`** has three forms. `Whole` can be cut. `PreCut` stores the worker's kept lines, its own marker, the total view lines and the marked count. `Fixed` keeps the body as-is, or the planner omits it.
- **`set_fits`** is the one fit rule for pass 1 and every rung. A set whose bytes are at most N fits with no count. Otherwise it fails when a piece count is over N or the sum is over `RUN_SUM_STOP_FACTOR` times N. If neither fails, one exact count of the joined string decides. A failed count means no fit.
- **`plan_cut_with_trailer`** is the core. `plan_cut` (tokens) and `plan_char_cut` (characters) call it with `run_trailer_line` and a trailer cost of `trailer_cost(pieces.len(), count)`. The caller gives `trailer_line(kept, total)`, which returns a line with a final newline or `None`, and `reserve`, which returns the budget cost of the widest trailer (`usize::MAX` if its count fails). The kept and total counts can include a header piece.
- **Full-set check first.** The planner tries the full set with the full trailer through the same fit rule, including the trailer bytes and its separator newline. `reserve` is lazy: the planner calls it only after this check misses. A full set with no trailer (`None`) needs no reserve.
- **`kept_prefix`** sums file floors in run order. A `Fixed` floor is the full piece count. Other floors use the lesser of the whole piece and its prefix plus the shortest available marker. If all character floors fit and the full set has no trailer, no trailer reserve is necessary. Otherwise the planner reserves the trailer with its widest numbers and, for characters, its join newline. Token runs always reserve the trailer for prefix selection. A kept prefix of zero gives the trailer-only result.
- **Cuts** count hidden view lines (`ViewLines`). A `PreCut` body whose marked count fits its share keeps the worker marker as it is. Otherwise the core cuts `kept` again with `SourceTotal(total_view_lines)`. A `Fixed` body is never cut, so a caller with only `Fixed` pieces gets keep-or-drop.
- **Retries.** If the exact count is over N, the planner drops the last kept file, at most `MAX_DROP_RETRIES` times. Then it returns the trailer only.
- **Failed count.** A failed count gives the trailer-only result at once: in the full-set check, in `shares`, in a worker cut, or in the join count of the retry loop. The planner does not drop a file after a failed count. `failed_counter_returns_only_the_trailer`, `failed_cut_count_returns_the_caller_trailer_without_a_drop` and `caller_trailer_does_not_require_another_count_if_the_first_count_fails` pin this.
- **Floor.** When N is below the trailer cost, stdout is the trailer line only. This is the one case where stdout can be over N. `--tokens 0` and values over `MAX_TOKEN_BUDGET` are still usage errors.

## Second caller: `skim heatmap --map` (`cmd/heatmap/map.rs`)

`write_output` in `cmd/heatmap/mod.rs` passes the `--map` budget to `map::render` as a token budget (`RunBudget::Tokens`) with `map_token_count` as the counter.

1. The header (title line, warnings and, for an empty list, `(none)`) is piece 0. If its count is missing or over the budget, `render` returns the trailer-only line `[skim] 0 of T files`.
2. Each ranked file becomes one `Fixed` block (`file_block`): its stats line, the first changes-with partner and its symbol rows. The planner keeps or drops a whole block. Blocks are in hotspot order from `ranked_files`.
3. Construction stops when the running sum of counts passes `RUN_SUM_STOP_FACTOR` times the budget. This bounds the counting and symbol reads. Files past the stop are not read, but T still counts every candidate (`test_map_stops_counting_at_twice_the_budget_but_counts_all_omissions`). A missing count for a block gives the trailer-only line.
4. `plan_cut_with_trailer` gets `trailer_line(kept, ..)` and a `reserve` that counts the widest trailer. It returns the trailer only when a block was left out or any shown block has a symbol reason. A full map with no reason has no trailer (`test_full_map_has_no_trailer`).
5. The trailer is `[skim] S of T files`, then `; reason: count` for each symbol reason in the shown blocks only, then ` — adjust the map budget with --tokens`. S is the kept count minus the header piece. Reason labels sort by name.

`map_production_code_does_not_read_a_clock` forbids a clock and `parse_bounded` in the production half of `map.rs`.

## Run notice and trailer (`output/mod.rs`)

`run_view_marker` builds the line from `RunNoticeFacts`. The single-input bound shares the `view_notice_line` builder. The notice names the served mode and run limit. It adds the step-down, cut and omission clauses only when they apply. Paths go through `display_path`.

For token runs, the remedies are a larger `--tokens`, fewer files and `SKIM_PASSTHROUGH=1`. For character runs, they are `--tokens N`, a typed `--mode` and `SKIM_PASSTHROUGH=1`. The character notice suggests fewer files only when at least two files succeeded.

`cut` counts shown files with a run cut, not files with a per-file marker only. A cut of a retained pass-1 body uses the requested view, although the notice names one served view for the run.

`run_trailer_line` reuses the shared elision marker. It counts successful files only, and it starts on its own line.

## Multi-file passthrough

`SKIM_PASSTHROUGH=1` or `--passthrough` serves raw bytes in run order with no headers, view notice or size limit. It bypasses the run budget. `resolve_explicit_paths` shares command-line order, first-occurrence duplicate removal and path diagnostics with the transform path for a list.

`passthrough_paths` serves the resolved paths for a list, directory or glob. It reports each input error and continues. If at least one file succeeds, the exit code is 0. A non-empty set in which every file fails exits 1. An empty set exits 0; a glob with no matches still fails during path resolution. An output error stops the run. A closed stdout pipe exits 141 on Unix.

## Anti-Patterns

**KB-AP-1.** The 2N stop and the byte-length shortcut only reject or accept early. The exact count of the joined string is the last check. BPE joins can add tokens, so a sum of piece counts is not a bound. `joined_overcounts_stop_after_three_drops` pins the retry count. A failed count is not a retry case: the planner returns the trailer only and never prints content it could not count. In the map, `test_piece_count_failure_discards_all_map_content` and `test_exact_join_count_failure_discards_all_map_content` pin this.

**KB-AP-2.** `cascade_for_token_budget` prints in its fit arm, in `fallback_line_truncate` and in the serde cascade. A multi-file pass 1 calls it, so the call must be quiet. Stdout bytes and the returned mode are equal for `Print` and `Quiet`. The cascade tests cover each arm. The count-failure warning always prints.

**KB-AP-3.** `run_rung` keeps the pass-1 body if it has no language, the transform fails, or the rung empties the body. Language comes from `explicit_lang` or the path extension, not the shebang. For tokens, these bodies and line-bound bodies are `Fixed`. A large `Fixed` body can end the shown prefix. For characters, retained bodies can be cut, including raw bodies with no language. A rung error adds no stderr line. Omission is a path-order suffix in both cases.

**KB-AP-4.** Batch analytics rows are unmeasured by design. See the long comment in `multi.rs` and the `analytics` knowledge base. The rows carry the requested mode and a null notice.

**KB-AP-5.** `partly_empty_signatures_keep_the_const_body_and_fit` and `a_partly_empty_rung_keeps_pass_one_and_can_fit` fail when `all` becomes `any` in the empty test of `run_rung`.

**KB-AP-6.** An exported `SKIM_PASSTHROUGH` also turns on the new list passthrough. Tests remove it from the base environment and set it only on the child. They also remove `SKIM_REWRITTEN_FROM` and `SKIM_DEBUG`. Prove a test discriminates by changing one production line and watching the test fail. The phase notes record such proofs for the bound, the trailer count, the empty flag, the notice predicate and the empty rule.

**KB-AP-7.** The run trailer ends with the `SKIM_PASSTHROUGH=1` hint because a multi-file read has a raw form. The map trailer ends with `adjust the map budget with --tokens`. `map.rs` builds its own trailer with `format_trailer`, and the trailer carries symbol reason counts. A new caller must give both the line and its widest-cost function to `plan_cut_with_trailer`.

**KB-AP-8.** `map_token_count` in `cmd/heatmap/mod.rs` returns no count when cl100k is not ready, because a heuristic count cannot enforce an exact token budget. `test_map_refuses_counts_without_cl100k` pins this. The planner treats the missing count as a failed count, so the map prints the trailer only.

## Gotchas

**KB-INV-1.** With `-n`, both budgets count the text after `process_file` adds numbers. A run cut counts hidden view lines, not hidden source lines. Number prefixes can prevent the cut from detecting a fence or heredoc.

**KB-INV-2, KB-INV-3.** `multi_options` in `main.rs` and its test twin are the only places that set `batch`. Add a new batch caller there, or the run stage will not start.

**KB-INV-5.** The 2N early stop can reject a rung that would fit, for example many tiny files with a small N. This is a chosen default, not a measured threshold. The cost is a smaller served view, never an over-budget one.

**KB-INV-6.** `piece_prefix` takes the index in all results, not in successes. A failed first file still leaves the separator before the next header. `piece_prefix_matches_stream_bytes_after_a_failed_first_file` pins this.

**KB-INV-7.** The aggregate notice counts all paths. The run notice counts successes. Both are correct for their own case. A cold fitting run no longer names the stepped-down view of one file, because the per-file lines stop.

**KB-INV-8.** The trailer keeps the shared marker form, with a dash and "1 files" for one file. It also prints with `--no-header`. A body with no final newline gets a newline before the trailer.

**KB-INV-12, KB-INV-13.** The trailer-only result is the planner's own fail-closed path: `trailer_only` calls `trailer_line(0, total)` and returns an empty body list with `omitted` equal to the piece count. A multi-file run gets it on a failed count too. The planner's `total` is the piece count. A caller whose pieces include a header must not copy that total into its trailer.

**KB-INV-14.** The map ignores the planner's total. Its trailer closure reads only the kept count, and takes T from the candidate list, so T covers files that construction never turned into pieces. S is kept minus the header piece. `trailer_line` in `map.rs` has a `debug_assert!` on S and the retained reasons. Only the reasons of the shown prefix go into the trailer.

**KB-INV-10.** `--show-stats` totals come from the shown final bodies. The suffix says "across S file(s)" with S as the shown count. The original count comes from pass 1. If headers alone force a character cut or omission, measured empty bodies still report zero tokens.

**KB-INV-11.** A cache tamper test must target a batch rung entry. A one-match glob has that key. A single-file read can have another notice-cost key. `cli_cache` has the cold, warm, jobs and tamper tests.

**Cost.** A cold over-limit run can make about three transform passes. In a 239-file measurement, the cache grew from 239 to 717 entries. Each rung uses its typed-mode keys; no run cut enters the cache. Pass 1 copies every successful output to make owned pieces. Memory also holds up to two probe sets, the cut base and one transient file per worker. `raw_line_cut` makes about `log2(lines)` passes, or roughly 1 GB of work near the 50 MiB read limit.

**Other limits.** `display_path` replaces non-ASCII characters with `?`. A count failure inside the run prints no new line and fails closed.

## Tests

- `crates/rskim/tests/cli_tokens_run_budget.rs`: end-to-end bounds, floor, step-down, markers, trailer, notice, analytics, closed stderr, jobs equality, goldens.
- `crates/rskim/tests/cli_cache.rs`: cold and warm equality, tamper of a batch rung, run cuts not cached.
- `crates/rskim/tests/cli_passthrough_coverage.rs`: explicit list passthrough, collectors, closed pipe, tiny-budget floor.
- `crates/rskim/tests/cli_token_budget.rs`: the glob test checks the whole-run bound.
- Unit tests live in `run_budget.rs`, `multi.rs`, `cascade.rs` and `output/mod.rs`. The caller-trailer tests in `run_budget.rs` start with `caller_trailer_` and `widest_caller_trailer_`. The map tests are in `cmd/heatmap/map.rs` and `cmd/heatmap/mod.rs`.

## Key Files

- `crates/rskim/src/multi.rs`: `prepare_results`, `select_run`, `run_rung`, `char_limits`, `seal_run`, `final_results`, `piece_prefix`, `write_run`, `resolve_explicit_paths`, `passthrough_paths`.
- `crates/rskim/src/run_budget.rs`: `Body`, `Piece`, `set_fits`, `plan_cut_with_trailer`, `plan_cut`, `plan_char_cut`, `RUN_SUM_STOP_FACTOR`, `MAX_DROP_RETRIES`.
- `crates/rskim/src/cmd/heatmap/map.rs`: `render`, `file_block`, `trailer_line`, `format_trailer` (the map caller of the planner).
- `crates/rskim/src/cmd/heatmap/mod.rs`: `write_output` (the `--map` branch), `map_token_count`.
- `crates/rskim/src/output/mod.rs`: `RunBudget`, `RunNoticeFacts`, `run_view_marker`, `run_trailer_line`, `view_notice_line`.
- `crates/rskim/src/cascade.rs`: `CascadeLines`, `walk_rungs`, `serde_rungs`.
- `crates/rskim/src/process.rs`: `run_budget_applies`, the `run_transform` choice of `Quiet` or `Print`.
- `crates/rskim/src/main.rs`: `--tokens` help text, `MAX_TOKEN_BUDGET`, the explicit-list passthrough gate, `multi_options`.
- `crates/rskim/src/cmd/init/helpers.rs`: the `skim init` guidance line for `--tokens`.

## Related

- Feature knowledge: `default-view-bound` (the single-input character limit; shares the rung walk and the notice line builder), `file-wrapper-fidelity` (the single-file compact marker and its stderr channel), `analytics` (unmeasured batch rows).
- Source: `README.md`, `crates/rskim/README.md`, `CHANGELOG.md` (state that one budget covers the whole output of a run).
