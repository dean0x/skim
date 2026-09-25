# Resolution Summary

**Branch**: fix/empirical-triage-defects -> main
**Date**: 2026-09-25T18:55:24Z
**Review**: .devflow/docs/reviews/fix-empirical-triage-defects/2026-09-25_1917
**Command**: /devflow:resolve

## Decisions Citations

- applies ADR-001 — resolve-build-family, resolve-execution-gh, resolve-output-markers, cleanup-cycle-runner
- applies ADR-005 — resolve-init-hooks-doctor (documentation-14)
- applies ADR-007 — resolve-docs-modes (documentation-01), resolve-minimal-pseudo
- applies ADR-008 — resolve-output-markers (documentation-02), resolve-minimal-pseudo
- applies ADR-011 — resolve-output-markers, resolve-execution-gh, resolve-cargo-canonical, cleanup-output-build, cleanup-cycle-runner
- applies ADR-016 — resolve-coordinate-space (rust-01), resolve-readme (documentation-16), resolve-minimal-pseudo
- applies ADR-019 — resolve-init-hooks-doctor (complexity-06, security-02), cleanup-init-doctor
- applies ADR-020 — resolve-analytics-schema (database-06, consistency-07), resolve-analytics-mod
- avoids PF-002 — global triage completeness assertion (137/137 accounted)
- avoids PF-009 — resolve-tests-remaining (testing-07)
- avoids PF-012 — resolve-execution-gh (security-03)
- avoids PF-015 / PF-016 — resolve-tests-sandbox (testing-06), cleanup-init-doctor, repair-temporal-ac5-guard
- avoids PF-017 — resolve-tests-sandbox (testing-01), resolve-tests-remaining (security-05)
- avoids PF-019 — resolve-minimal-pseudo (fold placed upstream of the mirrored normalisers)
- avoids PF-021 — resolve-execution-gh (performance-07 rationale)
- avoids PF-022 — rust-10 discharged via the `+1.98` fmt/clippy gate
- avoids PF-024 — resolve-execution-gh (architecture-01), resolve-build-family (security-04)
- avoids PF-025 — resolve-render-ghwatch (complexity-12), repair-analytics-notnull, resolve-tests-remaining
- avoids PF-026 — resolve-tests-remaining (testing-07, control pinned as well as subject)
- avoids PF-028 — resolve-claude-md (consistency-10, the 0-bytes figure anchored)
- avoids PF-031 — repair-temporal-ac5-guard
- avoids PF-033 — resolve-coordinate-space (rust-01/rust-03/rust-06, predicate and count moved together)
- avoids PF-036 — resolve-analytics-mod (database-01), resolve-stats (database-04), resolve-process-cache (architecture-04)
- avoids PF-037 — resolve-stats (database-02), resolve-analytics-schema
- avoids PF-038 — resolve-execution-gh (reliability-06)
- avoids PF-039 — resolve-output-markers (consistency-01), resolve-readme (documentation-04), cleanup-cycle-runner

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 137 |
| Fixed | 100 |
| False Positive | 3 |
| By Design | 2 |
| Deferred | 18 |
| Blocked | 0 |
| Escalated | 1 |
| Duplicates Collapsed | 13 |

## Verification

Final serial pass, 2026-09-26. Both crates, CI toolchain (1.98) for fmt and clippy.

| Command | Result |
|---------|--------|
| `cargo +1.98 fmt --all -- --check` | PASS |
| `cargo check -p rskim-core --all-targets` | PASS |
| `cargo check -p rskim --all-targets` | PASS |
| `cargo build -p rskim` | PASS |
| `cargo +1.98 clippy -p rskim-core --all-features --all-targets -- -D warnings` | PASS |
| `cargo +1.98 clippy -p rskim --all-features --all-targets -- -D warnings` | PASS |
| `cargo test -p rskim-core --doc` | PASS (17) |
| `cargo nextest run -p rskim-core -j 4` | PASS — 816 run, **816 passed**, 0 failed |
| `cargo nextest run -p rskim --all-targets -j 4` | PASS — 5697 run, **5697 passed**, 0 failed, 3 skipped |
| `cargo build --release -p rskim` | PASS (1m 50s; required by `positional_verify.rs`) |

Regression tests added: 40+ across `truncation_markers.rs`, `cli_transparency.rs`, `cli_doctor.rs`,
`cli_integrity.rs`, `analytics/{mod,schema}.rs`, `cmd/build/mod.rs`, `output/{mod,fidelity}.rs`,
`cmd/stats.rs`, `cmd/infra/gh/run_watch.rs`.

Final gate: **PASS** — 6,513 tests green across both crates, zero compiler warnings, zero clippy
warnings on 1.98, zero rustfmt diffs. Two validation passes were needed: pass 1 had a fully clean
compile gate and 4 test failures (3 real, 1 environmental), all repaired and re-verified in pass 2
with no regressions.

**No fix agent invoked a compiler.** Twenty-five agents were constrained away from cargo because
unbounded parallel builds have previously exhausted 64 GB RAM on this machine and forced a hard
restart. Verification during the fix phase was per-file `rustfmt +1.98 --check` (which proves the
parse), source reading, read-only `sqlite3` against a copy of the live analytics DB, and targeted
probes of the already-built debug binary. The serial validation pass is the real gate.

## Fixed Issues

| Issue | File:Line | Commit |
|-------|-----------|--------|
| testing-01 — CRITICAL: test sandbox closed the env axis but not cwd; `skim init` tests wrote real `.git/hooks`. Fixed structurally in `skim_sandboxed_with_bin` (covers all five consumer files) | crates/rskim/tests/common/mod.rs:228 | (pending) |
| documentation-01 — CRITICAL: `docs/modes.md` affirmatively false about two modes at 5 sites (4 listed + 1 found) | docs/modes.md:10,11,275,279,332 | (pending) |
| rust-01 — `types` mode emitted `(0 lines truncated)`; marker presence now follows the source-space count | crates/rskim-core/src/transform/truncate.rs:351 | (pending) |
| rust-02 — `mode_class_label("pseudo")` false for 7 of 15 languages; clause rewritten and verified per-arm | crates/rskim/src/output/mod.rs:756 | (pending) |
| rust-03 — `signatures` gained source counts but no gap markers; leading half landed, gap half pinned as self-retiring | crates/rskim-core/src/transform/signatures.rs:129 | (pending) |
| rust-06 — three unreachable `map_or` defaults inside `source_spaced`; gate materialised once | crates/rskim-core/src/transform/truncate.rs:301 | (pending) |
| rust-07 — `tokens_saved - tokens_lost` clamped at 0 in u64; widened to signed | crates/rskim/src/cmd/stats.rs:375 | (pending) |
| rust-08 — exit widening dropped the parser's success verdict; pinned by test | crates/rskim/src/cmd/build/mod.rs:299 | (pending) |
| rust-09 — `--show-stats` priced bytes the passthrough arms no longer emit | crates/rskim/src/cmd/build/mod.rs:277 | (pending) |
| rust-10 — CI lint gate on the 1.98 toolchain (PF-022) | workspace | (pending) |
| security-01 — HIGH: Windows NTSTATUS clamped to 0, so a crashed build exited SUCCESS. `clamp(1,255)` lower bound is the mechanism | crates/rskim/src/cmd/build/mod.rs:319 | (pending) |
| security-02 — dev-waiver rationale self-refuting; corrected to state `Verified` is accident-resistance, not an adversarial control | crates/rskim/src/cmd/hooks/mod.rs:608 | (pending) |
| security-03 — `RawFallback` ANSI-strip asymmetry; flag now mirrored from config | crates/rskim/src/cmd/execution.rs:634 | (pending) |
| security-04 — PF-024 open on the build family's raw arm; recorded as a hazard block | crates/rskim/src/cmd/build/mod.rs:260 | (pending) |
| security-05 — HIGH: 11 unsandboxed installs against the real `$HOME`, one a global fan-out | crates/rskim/tests/cli_init_permissions.rs:474 | (pending) |
| security-06 — `any_dev_pinned` demotes the staleness gate clone-wide; scope documented | crates/rskim/src/cmd/doctor/mod.rs:80 | (pending) |
| security-08 — `RawFallback::resolve` discards the re-run's stderr | crates/rskim/src/cmd/execution.rs:717 | (pending) |
| architecture-01 — HIGH: guard measured the injected argv and served the re-run; analytics booked a ratio between two commands | crates/rskim/src/cmd/execution.rs:1440 | (pending) |
| architecture-04 — batch run-scoped notice cost stored in a row-scoped column; both columns now NULL on batch rows | crates/rskim/src/multi.rs:337 | (pending) |
| architecture-07 — `output` ↔ `process` cycle broken; `count_token_pair` moved to `crate::tokens` | crates/rskim/src/tokens.rs:90 | (pending) |
| architecture-11 — `BuildResult` positional-constructor transposition hazard removed | crates/rskim/src/output/canonical.rs:288 | (pending) |
| performance-01 — HIGH: `gh` double round-trip; per-invocation gate `prepared_args != user_args` | crates/rskim/src/cmd/execution.rs:717 | (pending) |
| performance-02 — unsubstantiated pseudo/minimal reduction targets removed, not replaced | crates/rskim-core/src/transform/pseudo.rs:20 | (pending) |
| performance-03 — `view_notice_absolute` built 3× per read; computed once and threaded | crates/rskim/src/process.rs:284 | (pending) |
| database-01 — HIGH: `avg_savings_pct` a floored mean; population narrowed to rows that compressed | crates/rskim/src/analytics/mod.rs:602 | (pending) |
| database-02 — HIGH: `--json` published `delivered: 0` where text refuses to (PF-037) | crates/rskim/src/cmd/stats.rs:195 | (pending) |
| database-03 — `delivered.rows` / `.tokens` covered different populations; one shared predicate | crates/rskim/src/analytics/mod.rs:603 | (pending) |
| database-04 — delivered series is file-cohort-only; cohort now named | crates/rskim/src/cmd/stats.rs:456 | (pending) |
| database-05 — presence-gated ALTER was a TOCTOU; duplicate-column tolerated as success | crates/rskim/src/analytics/schema.rs:44 | (pending) |
| database-06 — `user_version = 4` orphans reachable from this branch's own history; hazard block corrected | crates/rskim/src/analytics/schema.rs:110 | (pending) |
| database-07 — no forward guard on a foreign NOT NULL column; hazard recorded, failure made observable | crates/rskim/src/analytics/mod.rs:543 | (pending) |
| database-08 — `served` gained its first production reader; `notice_bytes` documented as unread | crates/rskim/src/analytics/schema.rs:29 | (pending) |
| database-09 — untokenisable disclosure rows left the series silently; now counted | crates/rskim/src/analytics/mod.rs:1331 | (pending) |
| database-13 — case-sensitive column compare in a presence gate | crates/rskim/src/analytics/schema.rs:52 | (pending) |
| database-14 — missing-table error now names the DB path; scoped to `user_version >= 3` | crates/rskim/src/analytics/schema.rs:45 | (pending) |
| database-15 — `delivered` moved inside `summary` for consistent nesting | crates/rskim/src/cmd/stats.rs:186 | (pending) |
| documentation-02 — HIGH: rustdoc added by the accuracy-fix commit itself was inaccurate | crates/rskim/src/output/mod.rs:742 | (pending) |
| documentation-03 — HIGH: analytics header claimed a `user_version` guard the delivered step lacks | crates/rskim/src/analytics/mod.rs:18 | (pending) |
| documentation-04 — HIGH: README asserted "bypass all compression" this PR's own CLAUDE.md edit retracts | README.md:248,135 | (pending) |
| documentation-05 — HIGH: coordinate space of the elided count undocumented | CLAUDE.md:84 | (pending) |
| documentation-06 — HIGH: exit-code contract falsified by wrapped-tool forwarding | CLAUDE.md:146 | (pending) |
| documentation-07 — passthrough Exception 1 credited the wrong guard | CLAUDE.md:115 | (pending) |
| documentation-08 — "always summarises" false; the build family has two raw arms | CLAUDE.md:115 | (pending) |
| documentation-09 — disclosure-charging undocumented; the one ADR-001 mention scoped to where it does not happen | CLAUDE.md:138,140 | (pending) |
| documentation-10 — `skim doctor --help` exit-code line falsified by the `--dev` demotion | crates/rskim/src/cmd/doctor/mod.rs:1006 | (pending) |
| documentation-11 — analytics gotcha entry absent; `temporal.db`'s opposite policy was the only hit | CLAUDE.md:49 | (pending) |
| documentation-12 — Cursor `.mdc` goes through neither env resolver; it is cwd-relative | CLAUDE.md:50 | (pending) |
| documentation-13 — `grep dev-pinned` guard gap; fixed in code, so the caveat was reverted | crates/rskim/src/cmd/doctor/mod.rs:496,521 | (pending) |
| documentation-14 — HIGH: shipped agent guidance ordered modes wrong (minimal below structure) | crates/rskim/src/cmd/init/helpers.rs:225 | (pending) |
| documentation-15 — duplicate `skim init` help example | crates/rskim/src/cmd/init/helpers.rs:474 | (pending) |
| documentation-16 — README stated the line bound without ADR-016's N=1 carve-out | README.md:245,246,61 | (pending) |
| documentation-17 — cache-root fallback overstated as the root | CLAUDE.md:49,118 | (pending) |
| documentation-18 — ADR citation split for `--dev` (16 ADR-014 vs 0 ADR-019 in src) | CLAUDE.md:92 | (pending) |
| documentation-19 — doctor's demoted advisory printed a remedy the feature declares ineffective | crates/rskim/src/cmd/doctor/mod.rs:948 | (pending) |
| consistency-01 — HIGH: new build-family class-1 marker advertised an unreachable remedy (PF-039) | crates/rskim/src/output/mod.rs:648 | (pending) |
| consistency-03 — doc asserted `generate_hook_script` does not write the dev marker; it does | crates/rskim/src/cmd/hooks/mod.rs:591 | (pending) |
| consistency-04 — two docs said `eprintln!`; both emitters are `eprint!` | crates/rskim/src/output/fidelity.rs:237 | (pending) |
| consistency-05 — new test comment documented the pre-fix marker text | crates/rskim/tests/cli_transparency.rs:274 | (pending) |
| consistency-06 — build class parenthetical named rustc-only classes for make/gradle/maven/tsc | crates/rskim/src/output/mod.rs:663 | (pending) |
| consistency-07 — `V4_COLUMNS` named a version the branch refuses to claim | crates/rskim/src/analytics/schema.rs:29 | (pending) |
| consistency-08 — new `~` diff prefix user-visible and undocumented | crates/rskim/src/cmd/git/diff/render.rs:1389 | (pending) |
| consistency-09 — `RawFallback` decline notice class-2 gated but fires on a different-from-raw serve | crates/rskim/src/cmd/execution.rs:727 | (pending) |
| consistency-10 — only hard-coded line reference in either user doc | CLAUDE.md:115 | (pending) |
| consistency-11 — two class-1 markers hard-coded `for raw output` instead of `ELISION_HINT` | crates/rskim/src/cmd/execution.rs:1283 | (pending) |
| consistency-12 — test comments quoting the retired marker text | crates/rskim/tests/cli_transparency.rs:361 | (pending) |
| consistency-13 — `minimal` clause over-claimed after the #476 header fix | crates/rskim/src/output/mod.rs:757 | (pending) |
| reliability-03 — presence gate matched name only; a foreign same-named column was adopted silently | crates/rskim/src/analytics/schema.rs:44 | (pending) |
| reliability-04 — `WARNING_DETAIL_MAX` roll-up dropped messages and locations without naming them | crates/rskim/src/cmd/build/cargo.rs:283 | (pending) |
| reliability-06 — `RawFallback::resolve` discarded the spawn error at every verbosity (PF-038) | crates/rskim/src/cmd/execution.rs:717 | (pending) |
| reliability-08 — `persist_record` discarded both results silently | crates/rskim/src/analytics/mod.rs:1154 | (pending) |
| reliability-09 — `capped_lines` counted frames, not hidden jobs (720 → 36) | crates/rskim/src/cmd/infra/gh/run_watch.rs:347 | (pending) |
| reliability-10 — `debug_assert_eq!` compiled out; invariant now holds by construction | crates/rskim/src/cmd/build/cargo.rs:455 | (pending) |
| regression-02 — `--last-lines` mixed source and output space; producer gate was the real cause | crates/rskim-core/src/types.rs:442 | (pending) |
| regression-03 — exit codes and fd1/fd2 split changed observably without a contract update | crates/rskim/src/cmd/build/mod.rs:295 | (pending) |
| regression-04 — `error_messages` no longer carries the warning roll-up below the bound | crates/rskim/src/cmd/build/cargo.rs:452 | (pending) |
| regression-05 — `ts_comments_pseudo_max5` returned a bounded view with no code | crates/rskim-core/src/transform/minimal.rs:568 | (pending) |
| regression-06 — a foreign rebuild would reset the delivered series undetectably | crates/rskim/src/analytics/schema.rs | (pending) |
| regression-07 — green builds now render warnings; ADR-001 headroom note added | crates/rskim/src/output/canonical.rs:369 | (pending) |
| regression-08 — `decide_with_notice`'s `None`-token arm pinned as deliberate | crates/rskim/src/output/fidelity.rs:296 | (pending) |
| regression-11 — same-named `gh` jobs collapsed into one key and one was never emitted | crates/rskim/src/cmd/infra/gh/run_watch.rs:129 | (pending) |
| testing-02 — HIGH: version assertions could not fail; the doc comment's claim was false | crates/rskim/src/analytics/schema.rs:99 | (pending) |
| testing-03 — HIGH: `--last-lines` had zero behavioural coverage; `marker_count` blind to `above` | crates/rskim-core/tests/truncation_markers.rs | (pending) |
| testing-04 — sandbox guard file-scoped; widened to a prefix list, no exclusion added | crates/rskim/tests/cli_init.rs:2452 | (pending) |
| testing-05 — env guard scanned one crate; `ANTHROPIC_API_KEY` was unclassified | crates/rskim/tests/cli_init.rs:2513 | (pending) |
| testing-06 — 3 of 4 ADR-019 doctor tests silently passed on a tarball build | crates/rskim/tests/cli_doctor.rs:86 | (pending) |
| testing-07 — fixture pinned repo-local git config only, against byte-exact assertions | crates/rskim/tests/cli_git_diff_budget.rs:95 | (pending) |
| testing-08 — three vacuous negatives; one had a dead premise and was rewritten | crates/rskim/tests/cli_guardrail.rs:20 | (pending) |
| testing-10 — force-raw sidecar isolation per-binary, not per-test | crates/rskim/tests/cli_e2e_rewrite.rs:25 | (pending) |
| testing-11 — `make`-absent skips reported as PASS | crates/rskim/tests/cli_e2e_build_parsers.rs:103 | (pending) |
| testing-12 — the PR's `INSTA_UPDATE` claim is false, not loosely worded | PR body | (pending) |
| complexity-01 — HIGH: `savings_decision_with_notice` scaffolding shipped ahead of use | crates/rskim/src/cmd/execution.rs:82 | (pending) |
| complexity-04 — `mode_str` derived at 3 sites; all now use the existing `Mode::name()` | crates/rskim/src/main.rs:1292 | (pending) |
| complexity-06 — `hook_is_current && mode_matches` unencapsulated at 3 sites; module-private so the 4th cannot call it | crates/rskim/src/cmd/init/install.rs:125 | (pending) |
| complexity-07 — `hook_facts` fabricated a 12-field `InitFlags`; `detect_state` narrowed | crates/rskim/src/cmd/init/state.rs:206 | (pending) |
| complexity-08 — `cache_key` at six positional params; params struct added | crates/rskim/src/cache.rs:236 | (pending) |
| complexity-09 — `run_parsed_command` 172 lines / 4 concerns; two pure policies extracted | crates/rskim/src/cmd/build/mod.rs:150 | (pending) |
| complexity-10 — 13-column SELECT read by positional index; all columns aliased | crates/rskim/src/analytics/mod.rs:592 | (pending) |
| complexity-12 — `render_changed_only` hardcoded `breadcrumbs: true`; now derives the policy | crates/rskim/src/cmd/git/diff/render.rs:621 | (pending) |
| complexity-13 — `run_migrations` hosted two idioms; split into two named functions | crates/rskim/src/analytics/schema.rs:64 | (pending) |
| complexity-15 — `days_from_civil` lax day-of-month documented as display-only | crates/rskim/src/cmd/infra/gh/list.rs:181 | (pending) |

### Out-of-scope fixes included (not among the 137)

| Issue | File:Line | Why |
|-------|-----------|-----|
| `read_pipe` discarded a 64 MiB buffer at the cap, delivering **zero bytes** where the raw tool produced 64 MiB — a live #317 "compress, never truncate" violation | crates/rskim/src/runner.rs:381 | Found while fixing reliability-06; too severe to leave |
| `cli_temporal_first_parent.rs` — 4 assertions calibrated on pre-squash history, 3 failing on `main` | crates/rskim/tests/cli_temporal_first_parent.rs:1136 | Pre-existing red on `main`; blocks this PR's CI. **Separable — revert if preferred.** |
| `pseudo` mode undocumented, and 6 supported languages undocumented (12 claimed vs 18 real) | docs/usage.md, crates/rskim/README.md, crates/rskim-core/README.md | Found during the doc sweep; user-facing omissions |
| Structure-mode reduction figure contradicted its own benchmark (70-80% documented, 60.3% measured) | README.md, docs/modes.md, types.rs, structure.rs, docs/usage.md, 2 crate READMEs | Converged on the 60–80% form that contains the measurement |

## False Positives

| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| performance-05 | crates/rskim-core/src/types.rs:442 | The load-bearing premise is false: `build_newline_table` is a local binding in `minimal.rs`/`pseudo.rs`, never returned up, and is **never called at all** for Structure/Signatures/Types — the paths this block serves. The proposed zero-cost fix does not exist, and the finding concedes ~2 µs is within budget. |
| consistency-15 | crates/rskim/src/cmd/infra/gh/mod.rs:224 | The two predicates cannot disagree: `shared::user_steers_output(args)` short-circuits to raw passthrough **before** the route match, and `GH_OUTPUT_STEERING_FLAGS` includes `--json` for every subcommand except `api` and `run watch`. The "route does not inject" state is unreachable. |
| regression-12 | crates/rskim/src/analytics/schema.rs | Unreachable consequence: `AnalyticsDb::open` executes `PRAGMA journal_mode=WAL` **before** `run_migrations`, so the open path already required write access at every version. `skim stats` could not have worked on a read-only DB before, and cannot newly break on one. |

## By Design

| Issue | File:Line | Rationale (ADR/doc) |
|-------|-----------|---------------------|
| performance-07 | crates/rskim/src/cmd/execution.rs:717 | The type's own `# PF-021` section states buffering is sound only because its consumer buffers anyway, and explicitly forbids reuse on a streaming sink. The finding itself concludes "No action now". |
| regression-10 | crates/rskim/src/cmd/git/push.rs:248 | `push.rs:196-202` documents the removal as deliberate and names the inversion restoring it would re-open: without the TAB guard, a `[remote rejected]` message parses as a ref and a FAILED push reports as successful. |

## Fix Separately

| Issue | File:Line | Reason | Tracked |
|-------|-----------|--------|---------|
| architecture-02 | crates/rskim/src/output/fidelity.rs:202 | Collapsing 8 guard entry points to 3 changes every call site at once | (pending) |
| architecture-03 | crates/rskim/src/output/mod.rs:861 | Moving `Served` across the presentation/persistence boundary | (pending) |
| architecture-06 | crates/rskim/src/cmd/infra/gh/mod.rs:127 | `CONFIG` → `config_for(route)` touches every gh route module | (pending) |
| architecture-08 | crates/rskim/src/cmd/git/diff/render.rs:621 | Deleting the test-only renderer; parameterised instead this round | (pending) |
| architecture-09 | crates/rskim-core/src/transform/truncate.rs:60 | `source_range` as a required field touches every `NodeSpan` producer | (pending) |
| architecture-10 | crates/rskim/src/cache.rs:239 | Closing the key gap means tokenising on the cache-key path | (pending) |
| complexity-02 | crates/rskim/src/output/fidelity.rs:201 | Same shim-collapse as architecture-02 | (pending) |
| complexity-03 | crates/rskim/src/process.rs:254 | Redesign of the cost-accounting surface | (pending) |
| complexity-11 | crates/rskim/src/output/mod.rs:648 | `NoticeClass` type for the ADR-011 taxonomy (80 prose citations, 18 files) | (pending) |
| complexity-14 | crates/rskim/src/cmd/git/diff/render.rs | Seven touched files at 2,400–4,000 lines | (pending) |
| complexity-16 | crates/rskim/src/cmd/git/diff/render.rs:1359 | `emit_source_line` at 5 params, 9 call sites | (pending) |
| complexity-17 | crates/rskim/src/cmd/infra/gh/run_watch.rs:264 | Reordering the suffix-heuristic chain risks pinned parse behaviour | (pending) |
| database-10 | crates/rskim/src/analytics/schema.rs:72 | Only a table rebuild restores NOT NULL — the very operation whose hazard is documented | (pending) |
| database-11 | crates/rskim/src/cmd/stats.rs:120 | Deriving the window label from the prune policy moves every dashboard figure | (pending) |
| database-12 | crates/rskim/src/cmd/stats.rs:120 | PF-036 self-measurement exclusion moves every number at once | (pending) |
| performance-04 | crates/rskim/src/cmd/git/diff/render.rs:1445 | Revisiting the enrichment budget is an ADR-003 conversation | (pending) |
| performance-06 | crates/rskim/src/analytics/schema.rs:193 | Retiring the per-open reconcile depends on settling `user_version` | (pending) |
| reliability-07 | crates/rskim/src/cache.rs:212 | Adding a cache bound is a new lifecycle policy | (pending) |
| reliability-11 | crates/rskim/src/cmd/git/diff/render.rs:1445 | Changing `MIN_RAW_SIZE_FOR_GUARDRAIL` is an ADR-001 decision | (pending) |

### Newly discovered, recommended for the deferred set

- **`structure.rs`'s Markdown header extractor emits 8 line-map entries for 13 output lines.** This
  single producer defect blocks three separate things: the real `signatures` gap-marker fix
  (rust-03's preferred form), the correct `md_simple_structure_last10` value, and two `markdown`
  cells that move **28 → 34** (the only cells in a 335-cell sweep that get worse). Highest-value
  item in the deferred set.
- `OutputFormat` lives in `cmd/execution.rs` but is imported by `output/`; relocating it would close
  the `runner → output → cmd::execution → runner` loop the `read_pipe` fix introduced **and** shorten
  the pre-existing `cmd ↔ output` cycle.
- `exit_code_from_status` should move to `cmd::execution` and be adopted by the 8 sibling
  `clamp(0, 255)` sites that carry the same negative-reads-as-success hole.
- `cli.rs:~542` carries a pre-existing arithmetic inconsistency (693−109−84 = 500, stated 544).
  Needs a measurement pass, not a guess.

## Deferred to Tech Debt

_None._ Every refactor in this set is mechanical — collapsing shims, splitting files, moving a type
across a boundary, adding a cache bound. None requires a system redesign, so per the matrix they are
FIX_SEPARATE rather than TECH_DEBT.

## Escalations

| Issue | File:Line | Security Concern |
|-------|-----------|-----------------|
| security-07 | crates/rskim/src/cmd/execution.rs:1553 | `format_analytics_label` scrubs credentials only for the `db` and `infra` families; `build`/`test`/`lint`/`pkg` fall through to `rest.to_string()`, persisting argv verbatim (500-char truncation) into `analytics.db`'s `original_cmd`. Escalated rather than fixed because nobody could confirm whether any wrapped subcommand in those families takes a secret in argv (`cargo publish` appears not to be wrapped), and moving the scrub into the default arm changes the label for every family. **Needs a human decision.** |

## Blocked

_None._

## Duplicates

| Issue | Duplicate Of | File:Line |
|-------|-------------|-----------|
| reliability-01 | rust-01 | crates/rskim-core/src/transform/truncate.rs:301 |
| reliability-02 | security-01 | crates/rskim/src/cmd/build/mod.rs:319 |
| reliability-05 | consistency-01 | crates/rskim/src/output/mod.rs |
| regression-01 | performance-01 | crates/rskim/src/cmd/infra/gh/mod.rs:127 |
| regression-09 | testing-03 | crates/rskim-core/tests/truncation_markers.rs:918 |
| complexity-02 | architecture-02 | crates/rskim/src/output/fidelity.rs:201 |
| complexity-05 | rust-06 | crates/rskim-core/src/transform/truncate.rs:285 |
| rust-04 | database-05 | crates/rskim/src/analytics/schema.rs:44 |
| rust-05 | regression-02 | crates/rskim-core/src/types.rs:442 |
| architecture-05 | database-05 | crates/rskim/src/analytics/schema.rs:64 |
| consistency-02 | documentation-02 | crates/rskim/src/output/mod.rs:742 |
| consistency-14 | regression-02 | truncation_golden__md_simple_structure_last10.snap |
| testing-09 | security-05 | crates/rskim/tests/cli_init_permissions.rs:480 |

## Corrections to the review, found by verification

Recorded because they change what a reader should trust in the focus reports:

- **security-01's stated mechanism was wrong.** Windows `ExitStatus::code()` never returns `None`; the
  reachable path is a *negative* NTSTATUS. A fix written to the report would not have closed it.
- **testing-01's scope was wrong.** Zero raw constructors exist; 57 of 77 invocation sites left cwd
  uncontrolled. The remedy is one line in a shared helper, not 44 call-site edits.
- **regression-02 was 2 of 5 goldens, not 3 of 4.** The three `25` markers are positionally exact, and
  the "25 vs 21" claim is not reproducible. Go and Rust were passing **on the broken path** — the
  fallback coincides with truth whenever the window collapses nothing.
- **testing-06 was 3 of 4 tests**, not 4; the fourth installs via the real `--dev` flag.
- **regression-04's title was wrong** — `error_messages` was not removed; the roll-up moved out of it.
- **database-01's line and mechanism were wrong** — the expression is at `:602`, and the flooring
  happens at record time (`:1067`), not in SQL.
- **testing-12 is understated** — `INSTA_UPDATE` appears in tracked source, so the PR claim is false,
  not loosely worded.
- **testing-08 was partly wrong** — one of the three "vacuous" assertions had a *dead premise*: the
  guardrail now fires on tiny files since the 256-byte floor was removed. Adding `SKIM_DEBUG=1` would
  have turned it red, not green.
- **Two reviewer-proposed fixes were actively wrong.** Narrowing `route_rerunnable` would have removed
  the PF-024 *fidelity fix*, not the second call; and `skip_net_savings_guard: true` serves the
  *compressed* view unconditionally, the opposite of the stated intent.
- **A reviewer-proposed marker clause was wrong for maven** (it keeps `[ERROR]` stack frames; the
  `[INFO]` stream is what goes) and **for Python** (parameter annotations are stripped there, preserved
  in TypeScript).
