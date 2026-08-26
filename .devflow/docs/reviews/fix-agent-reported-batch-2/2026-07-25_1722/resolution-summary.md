# Resolution Summary

**Branch**: fix/agent-reported-batch-2 -> main
**Date**: 2026-07-25
**Review**: .devflow/docs/reviews/fix-agent-reported-batch-2/2026-07-25_1722
**Command**: /resolve
**PR**: #456 (cycle 3)

## Decisions Citations

- applies ADR-002 — resolve-log-cliff (64 MiB ceiling degrades losslessly with partial output instead of hard-erroring)
- applies ADR-003 — resolve-docs-source (git-diff docs corrected to hunk-scoped guardrailed render)
- applies ADR-005 — resolve-docs-source (guidance template edit kept all pinned positive/negative asserts)
- applies ADR-009 — resolve-grep-rg (skip_ansi_strip: true restores byte-faithfulness), resolve-tests-e2e (native path:line:content asserts)
- applies ADR-010 — resolve-tests-git (rev-range no-cap regression test), PR-body correction (N+1 probe claim deleted)
- applies ADR-011 — resolve-diff-careful/bounds (elision marker via footer, exact-count full-line assert), resolve-execution (signal-kill marker unconditional; banner stays gated), resolve-log-cliff (truncation marker unconditional)
- avoids PF-004 — resolve-tests-wrapper (D2b paired pipe/file coverage restored with a still-compressing tool)
- avoids PF-006 — resolve-grep-rg (tab-preservation e2e tests for grep/rg)
- avoids PF-008 — resolve-status-flags (closed CONFLICTING_SHORT_OPTS set, partial-strip, `--` terminator), resolve-status-ahead-behind (Malformed(raw) surfaces payload, never silent in-sync)
- avoids PF-009 — resolve-tests-git (all three hermetic helpers: `-b main`, per-step success asserts, closing preconditions, shared `git_in` helper)

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 113 |
| Fixed | 99 |
| False Positive | 0 |
| By Design | 0 |
| Deferred | 13 |
| Blocked | 0 |
| Escalated | 1 |

_(Note: `Deferred` = `## Fix Separately` count + `## Deferred to Tech Debt` count combined — the two sections are distinct by scope, but the Statistics row aggregates both for the convergence parser.)_

## Verification (central, serial — per [[parallel-agents-build-safety]])
Coder agents edited only; verification ran centrally and serially.
| Command | Result |
|---------|--------|
| cargo fmt --all -- --check | PASS |
| cargo clippy -p rskim --all-features --all-targets -- -D warnings | PASS (zero warnings) |
| cargo build -p rskim | PASS |
| cargo nextest run -p rskim --all-targets -j 4 | PASS — 4369/4369 |
| cargo test -p rskim --doc | N/A (bin-only crate, no doctests) |

Regression tests added: 25+ (see per-batch list)

Final gate: PASS after 3 fix attempts — one over the nominal 2-attempt budget, taken deliberately: attempt 1 fixed RawPassthrough exhaustive-match compile errors (10 sites) + 2 clippy lints; attempt 2 fixed the guardrail debug-gate test (retargeted to commit mode — `git show HEAD:file` uses Pseudo mode, which cannot inflate); attempt 3 (completed mainline during an agent-spawn outage) fixed a REAL parser defect its predecessor's test exposed: an empty line inside an open hunk didn't consume the hunk budget, so the hunk swallowed the next file's headers — empty blank-context lines now count against both sides (crates/rskim/src/cmd/file/diff.rs), plus the fixture's hand-written hunk header corrected to match its body (`@@ -10,3 +11,5 @@`).

## Fixed Issues

Fixes were applied by 13 edit-only Coder batches; verification and commits are centralized (per parallel-agents build-safety). Grouped by dedup key; member issue ids in brackets.

Commit map (group → SHA): diff.rs parser/counts → `4f280c6` · grep/rg passthrough + RawPassthrough + analytics → `5a584d9` · git status flags + AheadBehind → `b60c127` · git log 64 MiB degrade → `99d5872` · test hardening/renames/fixtures → `7f637a3` · docs (CHANGELOG/README/CLAUDE.md/modes/guidance/source-docs) → `8549ad9` · review artifacts → `9f2b97d` · PR #456 body correction → gh pr edit (no commit).

| Fix (dedup group) | File(s) | Issues |
|-------|-----------|--------|
| Hunk-budget body parsing — deleted `-- `/added `++ ` content lines can no longer be misparsed as file headers; 3 regression tests | cmd/file/diff.rs | security-01, rust-05 |
| Unconditional `mem::take(patch_lines)` — no cross-file leak | cmd/file/diff.rs | rust-07 |
| Entry-denominated shown/total counts; elision marker moved to footer channel; units aligned | cmd/file/diff.rs | architecture-05, complexity-09, regression-11, consistency-09 |
| MAX_INPUT_LINES guard (lossless passthrough degrade) + retention stops past display cap (counters stay exact) | cmd/file/diff.rs | security-03, reliability-03, complexity-05, architecture-03, performance-05 |
| `into_iter()` moves patch lines — second full-body allocation removed | cmd/file/diff.rs | performance-04, complexity-04, rust-01 |
| elision_marker unit table documents "files"; module doc updated; full-line marker assert | cmd/file/diff.rs, output/mod.rs | architecture-11, reliability-10, documentation-08, testing-11 |
| `skip_ansi_strip: true` in grep+rg (byte-faithful; TAB preserved) + 2 tab e2e tests | cmd/file/grep.rs, rg.rs, cli_e2e_tab_preservation.rs | architecture-01, regression-01, rust-06 |
| Dead GrepArgs classifier + 8 self-referential tests deleted (~145 lines) | cmd/file/grep.rs | architecture-04, complexity-01, consistency-03, regression-07, testing-02, rust-04 |
| Transition-tense comments → end-state; rg `-c`/`--files` unit coverage | cmd/file/rg.rs | documentation-10, testing-12 |
| `RawPassthrough` payload-less variant — per-invocation stdout clone removed; shared `passthrough_parse` hoisted to file/mod.rs | output/mod.rs, cmd/file/{grep,rg,mod}.rs, execution.rs | reliability-06, rust-02, architecture-07, complexity-07 |
| Analytics BPE short-circuit when compressed == raw (memcmp beats second tokenize) | analytics/mod.rs | performance-01 |
| Closed CONFLICTING_SHORT_OPTS {s,z}; partial cluster strip (`-suno`→`-uno`); `--null`/`--long` detected; scan stops at `--`; doc de-staled; 5 new + 3 updated tests | cmd/git/status.rs | security-04, security-05, security-06, complexity-10, rust-09, architecture-09, reliability-08, regression-05, consistency-06 |
| `AheadBehind` enum (Absent/Counts/Malformed): `[gone]` rendered; malformed ab surfaces raw payload; parse at classify_line; full-line assert_eq! renders | cmd/git/status.rs | architecture-02, security-07, complexity-08, regression-12, reliability-02, rust-03, consistency-10, testing-04 |
| 64 MiB ceiling: `read_pipe_degrade` keeps partial output + unconditional elision marker (no commit cap added — ADR-010); `is_commit_line` shape filter; 9 tests | cmd/git/log.rs, runner.rs | reliability-01, performance-03, reliability-09 |
| Signal-kill unconditional loss-bearing marker (vs benign exit-1 collision); double clone collapsed; strip_ansi_cow; SKIM_DEBUG idiom unified; show.rs doc | execution.rs, process.rs, git/show.rs | reliability-04, performance-06, performance-07, consistency-08, consistency-07 |
| Hermetic helpers hardened (`git_in` shared helper, `-b main`, per-step asserts, preconditions ×3 helpers); fetch disjunction split; rev-range no-cap test; guardrail debug-gate paired tests | tests/cli_git.rs | consistency-05, testing-06, reliability-05, rust-10, complexity-03, testing-09, testing-10, regression-10, testing-08 |
| rg native-format assert; grep doc-invariants asserted (line parity, no prefix); vacuous `<stdin>` assert replaced; exit-2 stderr contract pinned; 3 test renames | tests/cli_e2e_failure_modes.rs, cli_e2e_file_h_flag.rs, cli_e2e_exit_codes.rs | regression-09, testing-03, testing-07, architecture-08, testing-13 (+part stale-names) |
| Guardrail fires-and-silent paired test; D2b wrapper paired coverage restored with compressing `ls` stub; near-duplicate deleted; renames; 3 orphaned fixtures deleted | tests/cli_guardrail.rs, cli_wrapper_argv0.rs, cli_rewrite.rs, fixtures | regression-03, testing-01, regression-02, complexity-06, architecture-06, consistency-04 (+part stale-names) |
| Stale test names renamed to actual behavior (across e2e + wrapper files) | tests/* | complexity-02, consistency-02, regression-08, testing-05 |
| CHANGELOG: contradicting entries rewritten; Fix 1–5 entries added incl. Breaking Changes (Fix 3, Fix 4) | CHANGELOG.md | documentation-01, documentation-02, consistency-01, regression-04 |
| README: SKIM_DEBUG documented; hunk-scoped wording; pipeline caveat narrowed | README.md | documentation-05, documentation-03(part), documentation-09 |
| CLAUDE.md ADR-011 two-class taxonomy; modes.md +6 language rows; KB version label | CLAUDE.md, docs/modes.md, KNOWLEDGE.md | documentation-06, documentation-07, documentation-11 |
| git-diff docs corrected at 4 source sites incl. --help text; guidance template names only truly-compressing tools, grep/rg passthrough stated (pinned asserts intact) | git/diff/mod.rs, render.rs, init/helpers.rs | documentation-03, documentation-04 |
| PR #456 body: stale N+1-probe claim removed (ADR-010) | (gh pr edit) | architecture-10, regression-06, rust-08 |

## False Positives
None.

## By Design
None.

## Fix Separately
| Issue | File:Line | Reason | Tracked |
|-------|-----------|--------|---------|
| reliability-07 — git status double-spawn non-atomicity (C-7 baseline) | cmd/git/status.rs:84 | Pre-existing; trigger broadened but root cause predates PR | #458 |
| performance-02 — cache-hit re-reads file on every hook-rewritten cat/head/tail (HIGH) | process.rs:213 | Needs CacheEntry schema field (view_differs persistence) — out of PR blast radius | #459 |
| performance-08 — orphan-pass linear scan micro-opt | ADR-003-sensitive code | Outside diff; #317-sensitive surface | #462 |
| performance-09 — passthrough truncation Vec micro-opt | outside diff | Outside diff; micro-optimization | #462 |
| dependencies-01 — 3 sibling manifests still pin rskim-core 2.10.0 | rskim-{search,tokens,research}/Cargo.toml | Release plumbing; becomes hard failure at next major bump | #457 |
| dependencies-02 — Language enum not #[non_exhaustive] (semver hazard, document convention now, attribute at 3.0.0) | rskim-core/src/types.rs:21 | Semver-major to add now; needs convention doc + 3.0.0 schedule | #457 |
| dependencies-03 — cargo publish -p rskim cannot succeed (path-only deps, publish=false) | release.yml:348 | Pre-existing release pipeline issue; armed by next tag | #457 |
| dependencies-04 — no cargo-deny/audit/dependabot advisory gate | .github/ | CI plumbing addition | #461 |
| compliance-01 — Copilot permissions sidecar not project-keyed (erasure path breaks) | cmd/permissions/copilot.rs:156 | On main, not in PR diff; owned by planned permissions-bugs branch (PF-010) | #460 |
| compliance-02 — sidecar over-claims user-authored entries (uninstall revokes user grants) | cmd/permissions/{claude,copilot}.rs | Same — permissions follow-on branch (PF-010) | #460 |
| compliance-03 — sidecar lacks timestamp/binary identity; tier hardcoded "seed" | cmd/permissions/sidecar.rs:45 | Same — permissions follow-on branch (PF-010) | #460 |
| compliance-04 — Copilot writes user-owned config with no backup (Claude does) | cmd/permissions/copilot.rs:134 | Same — permissions follow-on branch (PF-010) | #460 |
| compliance-05 — .gitignore flip publishes .devflow KB content without review step | .gitignore:76 | Process control, not code defect; needs workflow decision | #463 |

## Deferred to Tech Debt
None.

## Escalations
| Issue | File:Line | Security Concern |
|-------|-----------|-----------------|
| security-02 — Fix 2's new patch_lines carry raw ESC/CSI bytes from file content into a skim-synthesized diff render (skip_ansi_strip: true removed the whole-stdout pass; per-field defense restored on paths but not body lines or the two passthrough paths) | cmd/file/diff.rs:37 | Terminal escape injection vs #317 byte-faithfulness collide: filtering strips content bytes (and strip_ansi eats tabs — PF-006); not filtering allows content-controlled escape sequences in a synthesized render. Needs a human/ADR ruling that also covers the grep/rg direction (where byte-faithful passthrough was chosen). Left UNCHANGED pending ruling. |

## Blocked
None.

## Cross-Cycle Note
Cycle 3 of a converging pipeline. Cycle 1: 20 fixed / 0 FP. Cycle 2: 13 fixed / 1 FP / 2 deferred. Cycle 3 reviewed the true PR scope (main...HEAD; the stale .last-review-head base was corrected — see review-summary.md): 113 raw findings → ~45 distinct defect groups, 99 fixed, 0 FP, 13 deferred with tickets, 1 escalated. The cycle-2 FP (env!/option_env!) and deferrals #424/#425 were not re-raised; the dead-GrepArgs deletion is a new, larger instance of the #425 class and was fixed rather than deferred.
