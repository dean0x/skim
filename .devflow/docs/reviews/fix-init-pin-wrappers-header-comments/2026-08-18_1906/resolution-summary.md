# Resolution Summary

**Branch**: fix/init-pin-wrappers-header-comments -> main
**Date**: 2026-08-21
**Review**: .devflow/docs/reviews/fix-init-pin-wrappers-header-comments/2026-08-18_1906
**Command**: /resolve
**Issues filed**: #489-#502 (14 issues)
**Commits**: 7bb8cac, 37c87d6, 088c431, 9be0d67, d2f5e12

---

## Decisions Citations

| Citation | Attached to | What it governed |
|----------|-------------|------------------|
| `applies ADR-004` | 37c87d6, d2f5e12 | Absolute-path binary pinning kept as the end-user mechanism; wrapper install/uninstall never touches symlinks whose target stem is not `skim`/`rskim` (foreign symlinks are advisory-only in the new `print_wrapper_section`). |
| `applies ADR-013` | 37c87d6 | "Detection stays, delivery becomes on-demand." Pin equality demoted from an install-time gate to a `skim doctor` advisory — the same reversal ADR-013 already made one layer up. |
| `avoids PF-015` | 37c87d6, d2f5e12 | The "SECOND GATE" shape. `is_hook_script_current()` deleted outright rather than aligned, so two predicates answering one question can no longer disagree. |
| `avoids PF-016` | d2f5e12 | Fail-open integrity. The fast path's `manifest_present` term is replaced by `integrity_verified`; `Tampered`/`NoManifest`/`Unreadable` all fall through to regeneration instead of re-hashing divergent bytes into a clean manifest. Also cited for the `--project`/`--wrappers` parse-time rejection. |
| `avoids PF-017` | d2f5e12, 37c87d6 | Every test added or rewritten routes through the sandboxed harness; the one new un-sandboxed shell-out was deleted with its test. Post-run check confirms `~/.skim/bin` does not exist and `~/.claude` was untouched. |
| `avoids PF-018` | 37c87d6, d2f5e12 | Canonical-path derivation sites reduced 4 → 1. `resolve_skim_binary()` widened to `pub(crate)`, re-exported, and shared by `doctor` and `rewrite/hook.rs`; the residual `current_exe_canonical()` helper deleted. Also the basis on which escalated finding security-04 was closed (see Escalations). |
| `avoids PF-019` | 7bb8cac | The `pseudo` walker is the mode the `cat`/`head`/`tail` rewrite selects, so the linearization was applied to **both** `minimal::collect_removable_comments` and `pseudo::collect_noise_ranges`, not just the file under review. |

---

## Statistics

| Metric | Value |
|--------|-------|
| Total Issues | 115 |
| Fixed | 58 |
| False Positive | 2 |
| By Design | 3 |
| Deferred | 6 |
| Blocked | 0 |
| Escalated | 0 |
| Remaining Open | 46 |

`Deferred` = FIX_SEPARATE (6) + TECH_DEBT (0). By Design (3) and Escalated are counted
separately and excluded from Deferred.

**Triage split (115 issues, exactly one verdict each):** 103 FIX_NOW · 6 FIX_SEPARATE ·
3 BY_DESIGN · 2 FALSE_POSITIVE · 1 ESCALATED · 0 TECH_DEBT.

**Post-restructure re-derivation of the 103 FIX_NOW:** 31 RESOLVED_BY_RESTRUCTURE ·
3 ALREADY_FIXED · 15 SUPERSEDED · 37 STILL_APPLIES · 17 IN_PROGRESS_ELSEWHERE
(the documentation group).

**`Escalated` is 0 because the one escalation was resolved, not deferred.** security-04 is
counted under Fixed; see the Escalations section for the evidence.

> **`Remaining Open` (46) is a real number, not a rounding artifact.** It is derived by
> checking each surviving finding against the working tree at `d2f5e12`, not by trusting the
> re-derivation. Of the 52 SUPERSEDED + STILL_APPLIES survivors, 11 closed and 41 did not; of
> the 17-item documentation group, 12 closed and 5 did not. Five source files carrying
> findings — `transform/mod.rs`, `tests/ruby_transform.rs`, `tests/sql_transform.rs`,
> `tests/bash_transform.rs`, `tests/cli.rs`, `tests/common/mod.rs` — appear nowhere in
> `git diff a012058..HEAD`. The final gate passing is not evidence that these closed; it is
> evidence that nothing regressed. They are enumerated in **Remaining Open** below.

---

## Empirical Verification

This run was explicitly required to **verify findings rather than trust source reasoning**.
That requirement changed several outcomes, and in two cases it was the only thing standing
between the branch and a shipped defect.

### 1. The O(N³) claim was confirmed by measurement, and adjudicated between reviewers

Three reviewers derived three mutually inconsistent complexity classes from the same
tree-sitter version, and no reviewer had run anything. Measurement settled it:

| Measurement | Result |
|-------------|--------|
| OLS exponent alpha (N ≥ 100) | **2.893** — effectively cubic |
| N=200→400 doubling ratio | **8.16×** (O(N³) predicts 8×; O(N² log N) predicts 4.5×) |
| N=1000, `--mode=minimal` | **23,920 ms** against a `<50 ms / 1000 lines` budget |
| Structure-mode control (same fixtures, same parse) | flat 6–8 ms — the parse is exonerated |

**Adjudication:** reliability's `O(N³)` derivation was **correct**; performance's
`Θ(L² log L)` derivation was **refuted** by the doubling ratio. complexity's `Θ(N²)` was
also low. A source-only review would have shipped the wrong exponent into the fix rationale.

`MAX_AST_NODES` was also tested at the boundary: N=100,001 **timed out at 120 s**. The cap
fires at node 100,001 but the algorithm must do O(i²) work for every node before it — roughly
38 days of computation — so the guard is functionally unreachable.

### 2. The first fix reached only O(N²); CPU profiling found the real root cause

The obvious fix (hoist the boundary to one forward pass) was implemented and re-measured. It
was a **224× improvement that was still quadratic** — alpha converged to ~2.0 at large N.
Re-measuring rather than declaring victory is what caught this.

`sample(1)` call-graph profiling of a real 10,000-comment run (8,841 samples, 100% on the
main thread) attributed **100% of self time to tree-sitter node navigation** and zero to any
skim transform, string, or range code. The root cause was not in skim's algorithm at all:

> **`TSNode` carries no parent pointer.** Every tree-sitter API that needs a node's parent —
> `parent()`, `next_sibling()`, `next_named_sibling()`, `named_child(i)` — re-derives it by
> walking **down from the tree root**, linearly scanning children. Each call costs
> O(index-of-node-within-its-parent). Called once per root-level node, that is O(N²).

This exonerated two innocent suspects by measurement (0 frames each): the tree-sitter parse
itself, and the range/output assembly (`adjust_range_for_line_removal`, `remove_ranges`,
`build_newline_table`). It also showed the intermediate fix's swap of `named_child(i)` for
`next_named_sibling()` was a **lateral move** — both are O(i), because
`ts_node__next_sibling`'s first action is `ts_node_parent`. Two in-code comments asserting
`"O(1) — two pointer dereferences"` and `"the sibling chain is O(1) per step"` were
**factually wrong**, and are precisely what made the defect invisible to source review.

The real fix uses a `TreeCursor` (which maintains an explicit ancestor stack), replaces the
parent walk with a `depth != 1` test against the cursor depth, and threads `in_function_body`
down both walkers so `is_inside_function_body`'s upward ancestor walk could be deleted.

| N | Before | After | Speedup |
|--:|-------:|------:|--------:|
| 1000 | 133.8 ms | 9.5 ms | 14× |
| 4000 | 2009.1 ms | 18.8 ms | 107× |
| **8000** | **8003.6 ms** | **30.4 ms** | **263×** |

Ratio per doubling fell from 3.79–3.98 (quadratic) to 1.34–1.62 (linear, rising toward 1.0 as
the fixed ~8 ms startup amortizes). Minimal mode now matches structure mode's 34 ms baseline,
as it should. **Correctness was verified by hashing 231 outputs (75 repo fixtures × 3 modes +
2 synthetic) before and after: all 231 byte-for-byte identical.**

### 3. The parser cache masks the defect completely — which is why review and `cargo bench` both missed it

`~/Library/Caches/skim/` holds hundreds of `.json` parser-cache entries. **A cached run
returns in ~5 ms regardless of N.** Every timing above required a fresh `SKIM_CACHE_DIR` and
unique file content per invocation. This single fact explains the whole miss: ad-hoc testing
looks instant, `cargo bench` re-reads cached fixtures, and no reviewer could see it by reading
code. The scaling guard added in 088c431 is therefore specified as a **ratio** assertion with
a fresh cache dir, not an absolute millisecond budget.

### 4. Three doctor/init claims were empirically confirmed

Run in a full sandbox (all six agent config-dir overrides plus `SKIM_WRAPPERS_DIR`,
`SKIM_CACHE_DIR`, `SKIM_DISABLE_ANALYTICS=1`):

| Claim | Verdict | Decisive evidence |
|-------|---------|-------------------|
| `skim doctor` falsely reports DRIFT in a foreign repo | **CONFIRMED** | Throwaway `git init` repo → `✗ SHA f00e37a not found in this repo — built from a different repository`, **exit 1**, with no actionable remedy. Every npm/curl-installed user hits this in their own projects. |
| `skim init --force` is a silent no-op | **CONFIRMED** | Force run output **byte-identical** to a plain repeat; hook mtime `1787143546` → `1787143546`, md5 `4edaaab7…` → `4edaaab7…`, both unchanged. `flags.force` was read only in `uninstall.rs:156`. |
| `current_exe()` + `canonicalize()` resolves symlinks (PF-018 landmine) | **CONFIRMED — landmine does not fire** | Installed via a symlink; `SKIM_HOOK_BINARY` recorded the **resolved target**. Doctor via symlink and via real path both show a matching pin. Neither reports a mismatch. |

The third result is what closed the escalation (see Escalations).

### 5. Two defects were introduced *by the fixes* and caught by the gate

State this plainly, because it is the strongest argument for the empirical requirement:

1. **Wiring `--force` opened a new path to the integrity-laundering site.** `--force` had been
   an inert flag; making it a real fast-path bypass term meant `skim init --force` now reached
   `create_hook_script`, hit `hook_is_current() == true`, and **blessed a tampered script's
   hash without rewriting the script** — turning the *advertised remedy* into the laundering
   path. Fixed in d2f5e12.
2. **The first integrity fix was unreachable.** The initial repair logic sat behind a fast
   path that returned before any integrity check ran: a tampered script keeps its version and
   commit markers, so `hook_is_current()` stayed true and `skim init` printed "Already up to
   date" while `skim doctor` kept reporting `Tampered`. Neither command could repair the
   divergence. Fixed in d2f5e12 by replacing the `manifest_present` term with
   `integrity_verified`.

Both were introduced by remediation, not by the original branch, and neither was visible from
reading the patch. Both are now fixed and covered by tests that assert **script content is
restored**, not merely that a message printed.

---

## Scope Change

Mid-run, the repo owner approved **restructuring** the hook binary-pinning machinery
(option **b′**) rather than continuing to patch it. This is why 31 findings resolved without
individual fixes, and it is the single largest determinant of this run's shape.

**The analysis that justified it:**

- **~727 LOC** of production provenance surface across 6 modules (~1,122 counting the `$PATH`
  scan and staleness section), plus ~40 dedicated tests.
- **8 distinct predicates answering 4 overlapping questions**; **4 independent canonical-path
  derivations**, three of which disagreed on failure; **3 independent copies** of the
  `"unknown"`-commit rule.
- **9 of the 11 named blocking items** in the 11-reviewer sweep were pin or fast-path
  machinery. The two heaviest were *recurrences of pitfalls the project had already written
  down* — PF-015 and PF-018 — inside the PR that cited them by number. The shape had recurred
  three times (#466 → #470 → #477 → #488), each fix **adding** a predicate rather than
  removing one.
- **Dev-only machinery was degrading end users.** The clearest case: 145 LOC of staleness
  checking that exists solely for the multi-clone dev loop produced a false
  `DRIFT DETECTED — exit 1` for every npm/curl-installed user running `skim doctor` inside
  their own repo (confirmed empirically, above).

**What the restructure did:**

| Change | Effect |
|--------|--------|
| Deleted `is_hook_script_current()` | Closed the two-gates-in-series shape structurally instead of patching it; removed the fail-open/fail-closed divergence and one copy of the `"unknown"` rule. |
| Deleted `wrappers_blocks_fast_path()` | Removed the fast-path term whose missing `flags.project` predicate caused the non-convergence; `maybe_install_wrappers` moved *inside* the fast path. |
| Demoted pin equality to a doctor advisory | `applies ADR-013`. Pin mismatch now prints `⚠` and exits 0; `hook_is_current()` still drives drift. |
| Removed `_SKIM_BIN` from generated scripts | Scripts exec `$SKIM_HOOK_BINARY` directly — one field, one thing to validate. |
| `resolve_skim_binary()` → `pub(crate)`, shared | Canonical-path derivations 4 → 1. |
| Wired `flags.force` as a real bypass term | Supplied the operator escape hatch the old design lacked. |

**Deliberate end-user-visible trade:** `skim init --yes` no longer rewrites the hook when only
the pin *path* differs — use `--force`. Every other behavior change is a strict improvement
and all are documented in `CHANGELOG.md` (added in d2f5e12).

**Cost of the restructure:** it invalidated 15 findings into new residual forms (SUPERSEDED)
rather than closing them, and it did not touch the 37 findings outside the pin/fast-path
blast radius. That is the bulk of **Remaining Open**.

---

## Verification

| Command | Result |
|---------|--------|
| `cargo fmt -- --check` | PASS |
| `cargo clippy -p rskim-core --all-targets -- -D warnings` | PASS |
| `cargo clippy -p rskim --all-features --all-targets -- -D warnings` | PASS |
| `cargo build -p rskim-core` | PASS |
| `cargo build -p rskim` | PASS |
| `cargo nextest run -p rskim --all-targets` | PASS |
| `cargo nextest run -p rskim-core` | PASS |
| `cargo test -p rskim-core --doc` | PASS |
| PF-017 sandbox check (`~/.skim/bin` absent, `~/.claude` untouched) | PASS |

**4957 tests, 0 failures.** The gate was run **serially**, not fanned out across agents
(avoids the parallel-build hazard recorded in memory).

Regression tests added: 12

- Quadratic-scaling ratio guard (N=4000 vs N=8000, 2.5× threshold, 2 ms noise floor that
  **fails** rather than skipping) — replaces a guard that was vacuous because the
  linearization made its `t1_ms < 5.0` skip condition always true.
- Two cubic smoke tests retightened and relabeled (2000 ms → 200 ms; 3000 ms → 500 ms), each
  documented as unable to distinguish linear from quadratic at N=500, cross-referencing the
  ratio guard.
- `tests/fixtures/python/large_header.py` (506 lines / 501 comment lines) plus two tests
  asserting all 500 header comments survive in minimal and pseudo.
- Tampered-script repair: asserts script **content is restored**, not just that a message
  printed.
- Wrapper target mismatch via a stale binary at a different path; correct wrappers → no drift;
  foreign symlink is advisory and left untouched.
- `--project`/`--wrappers` parse-time rejection.
- `--force` bypasses the fast path; `--force` repairs an unpinned hook.
- Repeat `--wrappers` does not clobber `settings.json.bak` (`assert_eq!` on the backup before
  and after).

**Final gate: PASS**

---

## Fixed Issues

58 issues. Grouped by the commit that closed them.

### 7bb8cac — `perf(core): fix O(N^3) module-header comment detection, now linear` (7)

| Issue | File:Line | Commit |
|-------|-----------|--------|
| security-01 — unbounded backward sibling walk (DoS) | `crates/rskim-core/src/transform/minimal.rs:308-330` | 7bb8cac |
| performance-01 — quadratic/cubic complexity, CRITICAL | `crates/rskim-core/src/transform/minimal.rs:292-331` | 7bb8cac |
| rust-02 — quadratic complexity on the pseudo hot path | `crates/rskim-core/src/transform/minimal.rs:292` | 7bb8cac |
| reliability-01 — bare `loop` with no iteration bound | `crates/rskim-core/src/transform/minimal.rs:308-331` | 7bb8cac |
| complexity-06 — unbounded loop, per-node recomputation of a per-file property | `crates/rskim-core/src/transform/minimal.rs:293-329` | 7bb8cac |
| rust-11 — `map(...).unwrap_or(false)` instead of `is_some_and` | `crates/rskim-core/src/transform/minimal.rs:300` | 7bb8cac |
| consistency-14 — same non-idiomatic `map/unwrap_or` | `crates/rskim-core/src/transform/minimal.rs:302` | 7bb8cac |

Expression deleted outright: the root-child test is now `depth != 1` against the walker depth.

### 37c87d6 — `fix(init): wire --force flag, fix doctor out-of-repo drift false-positive` (31)

| Issue | File:Line | Commit |
|-------|-----------|--------|
| reliability-02 — fail-open/fail-closed pin gates in series, non-convergent repair | `crates/rskim/src/cmd/init/install.rs:864-877` | 37c87d6 |
| security-03 — fail-open divergence on absent/unparseable pin | `crates/rskim/src/cmd/init/install.rs:868-876` | 37c87d6 |
| regression-02 — `skim init` never converges, `doctor` red forever | `crates/rskim/src/cmd/init/state.rs:59-76` | 37c87d6 |
| rust-05 — let-chain falls through to `true` on `Err`/`None` | `crates/rskim/src/cmd/init/install.rs:868` | 37c87d6 |
| complexity-02 — two gates, opposite degenerate-input polarity | `crates/rskim/src/cmd/init/state.rs:59-73` | 37c87d6 |
| consistency-06 — duplicate predicate with divergent semantics | `crates/rskim/src/cmd/init/state.rs:59-77` | 37c87d6 |
| architecture-03 — dual-predicate divergence | `crates/rskim/src/cmd/init/state.rs:59-77` | 37c87d6 |
| consistency-01 — fourth canonical-path derivation | `crates/rskim/src/cmd/doctor/mod.rs:483-487` | 37c87d6 |
| security-07 — normalization drift (DRY violation) | `crates/rskim/src/cmd/doctor/mod.rs:483-487` | 37c87d6 |
| rust-03 — closed three derivation sites, opened a fourth | `crates/rskim/src/cmd/doctor/mod.rs:483` | 37c87d6 |
| reliability-04 — displayed path ≠ compared path | `crates/rskim/src/cmd/doctor/mod.rs:483-487` | 37c87d6 |
| complexity-03 — local helper ignored; `resolve_skim_binary` too tightly scoped | `crates/rskim/src/cmd/doctor/mod.rs:483-487` | 37c87d6 |
| architecture-01 — fourth-derivation half (purity residual → complexity-04) | `crates/rskim/src/cmd/doctor/mod.rs:483-487` | 37c87d6 |
| testing-01 — new shell-out bypasses the PF-017 sandbox | `crates/rskim/tests/cli_init.rs:1737, 1758` | 37c87d6 |
| security-06 — test isolation gap | `crates/rskim/tests/cli_init.rs:1728` | 37c87d6 |
| rust-07 — un-sandboxed `skim init` against the developer's `$HOME` | `crates/rskim/tests/cli_init.rs:1732` | 37c87d6 |
| consistency-09 — inconsistent test harness beside a correct sibling | `crates/rskim/tests/cli_init.rs:1732` | 37c87d6 |
| compliance-01 — PF-017 sandbox bypass | `crates/rskim/tests/cli_init.rs:1732` | 37c87d6 |
| performance-03 — redundant script re-read / pin re-parse | `crates/rskim/src/cmd/init/install.rs:868-877` | 37c87d6 |
| complexity-01 — `"unknown"`-commit rule triplicated | `crates/rskim/src/cmd/doctor/mod.rs:466-489` | 37c87d6 |
| consistency-03 — wrapper predicate under the permissions banner | `crates/rskim/src/cmd/init/install.rs:169` | 37c87d6 |
| consistency-04 — misleading "mirrors" docblock + reversed arm order | `crates/rskim/src/cmd/init/install.rs:157-174` | 37c87d6 |
| complexity-09 — hook drift detection fails open on symlinked layouts | `crates/rskim/src/cmd/rewrite/hook.rs:86` | 37c87d6 |
| architecture-08 — divergent failure semantics across the comparison | `crates/rskim/src/cmd/rewrite/hook.rs:86` | 37c87d6 |
| testing-09 — hand-rolled `format!` script fixture | `crates/rskim/tests/cli_init.rs:1745-1757` | 37c87d6 |
| testing-03 — vacuous assertion, no fast-path reachability control | `crates/rskim/tests/cli_init.rs:1786-1818` | 37c87d6 |
| regression-03 — repeat `--wrappers` clobbers `settings.json.bak` | `crates/rskim/src/cmd/init/install.rs:503-514` | 37c87d6 |
| consistency-07 — stale `Unreadable` docblock line | `crates/rskim/src/cmd/doctor/mod.rs:371-372` | 37c87d6 |
| documentation-08 — docblock contradicts code and a test in the same file | `crates/rskim/src/cmd/doctor/mod.rs:371-372` | 37c87d6 |
| documentation-11 — tombstone comment + rotted line pointer | `crates/rskim/src/cmd/init/helpers.rs:25` | 37c87d6 |
| security-04 — re-canonicalized pin comparison (escalated; see Escalations) | `crates/rskim/src/cmd/init/install.rs:871-872` | 37c87d6 |

### 088c431 — `test(core): make quadratic-scaling guard always assert` (1)

| Issue | File:Line | Commit |
|-------|-----------|--------|
| performance-05 — no fixture or guard could observe the regression | `crates/rskim-core/tests/integration.rs:1706`; `crates/rskim-core/src/transform/minimal.rs:1188` | 088c431 |

### 9be0d67 — `docs: correct documentation drift across KB, modes, and CLAUDE.md` (11)

| Issue | File:Line | Commit |
|-------|-----------|--------|
| documentation-01 — analytics KB invisible to feature-knowledge matching | `.devflow/features/index.md` | 9be0d67 |
| documentation-02 — four citations of a nonexistent PF-002 | `.devflow/features/analytics/KNOWLEDGE.md:24, 126, 144, 154` | 9be0d67 |
| documentation-03 — Rule 5 misdescribes the batch persist path | `.devflow/features/analytics/KNOWLEDGE.md:69, 111` | 9be0d67 |
| documentation-04 — no credential-scrubbing invariant for a persisted-command path | `.devflow/features/analytics/KNOWLEDGE.md:122-128, 30` | 9be0d67 |
| compliance-02 — same missing scrubbing invariant (compliance lens) | `.devflow/features/analytics/KNOWLEDGE.md:122` | 9be0d67 |
| documentation-05 — stale public rustdoc for `Mode::Minimal` | `crates/rskim-core/src/types.rs:567, 642` | 9be0d67 |
| consistency-08 — same stale rustdoc (consistency lens) | `crates/rskim-core/src/types.rs:567, 571-575, 642` | 9be0d67 |
| documentation-06 — `hook_is_current()` definition omits the commit check | `.devflow/features/hook-binary-pinning/KNOWLEDGE.md:56, 150, 252` | 9be0d67 |
| documentation-07 — fast-path condition count contradicts its own code block | `.devflow/features/hook-binary-pinning/KNOWLEDGE.md:60` | 9be0d67 |
| documentation-09 — over-broad language scope in the Minimal row | `docs/modes.md:10` | 9be0d67 |
| documentation-12 — doctor described as comparing SHAs, not paths | `CLAUDE.md:83` | 9be0d67 |

documentation-05 and consistency-08 were the same two-line edit and were deliberately fixed once.

### d2f5e12 — `fix(init,doctor): close tampered-script repair loop; add wrapper pin invariant` (8)

| Issue | File:Line | Commit |
|-------|-----------|--------|
| security-02 — integrity laundering: tampered bytes re-hashed into a `Verified` manifest | `crates/rskim/src/cmd/init/install.rs:827-845` | d2f5e12 |
| reliability-05 — same fail-open integrity path (PF-016) | `crates/rskim/src/cmd/init/install.rs:827-845` | d2f5e12 |
| regression-01 — `--project --wrappers` accepted but installs zero wrappers | `crates/rskim/src/cmd/init/install.rs:169-175`; `flags.rs:387-396` | d2f5e12 |
| rust-01 — same flag combination, idempotence break | `crates/rskim/src/cmd/init/install.rs:169, 503, 509` | d2f5e12 |
| architecture-10 — scope-blind predicate vs scope-aware sibling | `crates/rskim/src/cmd/init/install.rs:169-175` | d2f5e12 |
| architecture-05 — wrapper surface could not affect the exit code by construction | `crates/rskim/src/cmd/doctor/mod.rs:543-569` | d2f5e12 |
| architecture-02 — residual second implementation of the canonical-path rule | `crates/rskim/src/cmd/doctor/mod.rs:96-100` | d2f5e12 |
| regression-07 — undocumented doctor exit-code changes | `CHANGELOG.md [Unreleased]` | d2f5e12 |

---

## False Positives

| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| reliability-09 | `crates/rskim-core/src/transform/minimal.rs:319` | Filed by its own reviewer as `type: no-finding`, "recorded for completeness". Verified independently: `gap.bytes().filter(...).count()` is a zero-allocation iterator chain over `&str` bytes, and `is_module_header_comment` holds one `Node` — a `Copy` value type. Allocation and indirection-depth rules are satisfied throughout the diff. Nothing to fix. |
| architecture-14 | `crates/rskim/src/cmd/doctor/mod.rs:544-570` | The open question is answered by cited code, and the answer removes the finding. `install_wrappers_in`'s documented idempotence contract (`wrappers.rs:113-121`) states "If the symlink points somewhere else: remove and re-create (counts as `updated`)", implemented via `install_one_symlink` → `handle_existing_symlink` (`wrappers.rs:170-173`). Repair does re-point stale symlinks. No repair gap exists; the doctor-side reporting blindness was the only real defect and was filed separately as architecture-05 (fixed in d2f5e12). |

---

## By Design

| Issue | File:Line | Rationale (ADR/doc) |
|-------|-----------|---------------------|
| complexity-10 | `crates/rskim/src/cmd/init/install.rs:169-175` | The explicit three-arm match is deliberate and documented at `install.rs:159-168`, which gives a six-line load-bearing rationale for the `None` arm: if `None` blocked the fast path, every non-TTY `skim init` (CI, test harness) would reinstall the hook on each run, breaking idempotence tests. The PR description independently records "None must never block (load bearing)". The reviewer argues against their own suggestion. *(Moot in the end state — the function was deleted by the restructure; the separate arm-ordering and misleading-mirror complaints were tracked and fixed under consistency-04.)* |
| consistency-13 | `crates/rskim/src/cmd/init/install.rs:170-174` | Duplicate of complexity-10, same citation and same rationale. The reviewer concedes "The three-arm `match` does document each case explicitly, which is a defensible reason to keep it" and could not verify whether `clippy::match_like_matches_macro` even fires given its arm-comment exemption. The arm-ordering half was tracked under consistency-04. |
| architecture-11 | `crates/rskim-core/src/transform/minimal.rs:292-296` | The four-language scope is documented and deliberate (`minimal.rs:281-285`): Python, Ruby, SQL and Bash are exactly the languages where `is_doc_comment` returns unconditional `false`, verified against `minimal.rs:174-178, 203-207, 216-223`. Also documented user-facing at `docs/modes.md:10` and `:280`; the reviewer concedes "docs/modes.md documents the split as intentional". Extending preservation to C/C++/Java/TS would change minimal/pseudo output for 11 more languages — a feature decision, not a defect. The related complaint that the *doc* over-claimed its scope was real and was fixed under documentation-09 (9be0d67). |

---

## Fix Separately

| Issue | File:Line | Reason | Tracked |
|-------|-----------|--------|---------|
| rust-09 | `crates/rskim/Cargo.toml` | Adding a `[lints.clippy]` table to the binary crate is a crate-wide lint-policy change that could surface arbitrary pre-existing violations across ~50 modules. Filed by the reviewer as informational with no suggested fix, explicitly stating no panic surfaces were introduced by this PR. | #498 |
| consistency-12 | `crates/rskim/tests/cli_init.rs:17` | `skim_init_cmd` is the harness for 26 shell-outs; 2 were new in this PR (fixed under testing-01) and 24 are pre-existing. Redefining it to delegate to `skim_sandboxed_with_bin` changes the runtime environment of 24 tests at once. Both reviewers scope it out; the hook-binary-pinning KB already records it as a "Known remaining gap". | #499 |
| testing-08 | `crates/rskim/tests/cli_init.rs` | Duplicate of consistency-12. Iron Law scoping is correct — 24 pre-existing un-sandboxed call sites do not block this PR. The reviewer explicitly asks for a tracking issue. | #499 |
| architecture-12 | `crates/rskim/src/cmd/init/install.rs` + `state.rs` | *Superseded by events:* asked whether `is_hook_script_current` should exist at all. The restructure answered **no** and deleted it (37c87d6). The ticket should be closed as done rather than filed, or refiled as the narrower question of whether `DetectedState` should own all currency predicates. | #500 |
| architecture-13 | `crates/rskim/src/cmd/init/install.rs:930-934` | Investigation task, not a code defect: determining which released versions emitted `export SKIM_HOOK_BINARY=''` requires archaeology across published tags. The reviewer states the structural divergence stands regardless of population size; it affects prioritization only. | #500 |
| documentation-14 | `docs/modes.md:3, 16, 83, 150, 223, 254` | The remedy is authoring new normative content (a full `## Minimal Mode` section with What's Preserved / What's Removed / Per-Language subsections), not correcting existing content. Verified: modes.md has no Minimal section, only the comparison-table row this PR edited plus a cross-reference at `:280`. The reviewer explicitly scopes it to a follow-up PR. | #501 |

---

## Deferred to Tech Debt

| Issue | File:Line | Risk Factor |
|-------|-----------|-------------|
| *(none)* | — | No issue received a TECH_DEBT verdict. All 6 deferrals are FIX_SEPARATE and are listed above. |

---

## Escalations

| Issue | File:Line | Security Concern |
|-------|-----------|------------------|
| security-04 — **RESOLVED EMPIRICALLY; no longer an open escalation** | `crates/rskim/src/cmd/init/state.rs:67-72`; `crates/rskim/src/cmd/init/install.rs:871-872` | Both new comparators re-canonicalized the pin read from the hook script before comparing it to the running binary, creating a check-vs-use gap: a pin of `/tmp/x` where `/tmp/x → <running binary>` would pass, and the symlink target could be swapped after the check. It was escalated rather than verdicted because the proposed remedy (compare the pin as recorded) is the exact change PF-018 names as "THE LANDMINE" — it would false-negative on Homebrew/cargo-install/symlinked layouts and convert a missing-check bug into an infinite-churn bug, and the reviewer's premise ("only tampered pins are non-canonical") is defeated by `resolve_skim_binary()`'s own `unwrap_or` raw-path fallback. |

**Resolution — by measurement, not by judgment.** The empirical run settled the question the
escalation could not: `canonicalize()` runs **before** the pin is recorded, so a symlinked
install already stores the real target path. A binary was installed via a symlink and
`SKIM_HOOK_BINARY` recorded the **resolved target**, not the symlink path. `skim doctor` was
then invoked through both the symlink and the real path; **both showed a matching pin and
neither reported a mismatch.** PF-018's churn landmine therefore does not fire, and the
escalation's central risk — that tightening the comparison would re-arm it — is moot because
the pin and the running path are the same real path by construction.

**End state (verified at `d2f5e12`), stated precisely:**

- The `install.rs:871-872` comparator **is gone** — `grep canonicalize crates/rskim/src/cmd/init/install.rs`
  returns zero hits, and `is_hook_script_current()` no longer exists.
- The `state.rs:67-72` re-canonicalization **survives** at `state.rs:71`, but
  `pin_is_current()` is no longer wired into the install fast path; it is consumed only by
  `hook_facts()`/doctor, where pin mismatch is an advisory `⚠` that does not set drift. The
  check-vs-use gap is therefore no longer on a gating path.

This is recorded as **resolved and counted under Fixed**, not deferred. It is not an open
escalation and requires no human decision.

---

## Blocked

| Issue | File:Line | Blocker |
|-------|-----------|---------|
| *(none)* | — | No issue was blocked. Every triaged issue reached a disposition; the build/test gate ran clean and no external dependency, credential, or decision gate stalled the work. |

---

## Remaining Open

**46 issues carried a FIX_NOW verdict and are still present in the working tree at `d2f5e12`.**

These are not deferred, not by-design, and not false positives — they were triaged as
fix-now, survived the restructure, and were not reached before the run ended. Each row below
was verified against the working tree, not inferred from the re-derivation.

**Strongest evidence:** six files carrying findings appear **nowhere** in
`git diff a012058..HEAD` — `crates/rskim-core/src/transform/mod.rs`,
`crates/rskim-core/tests/ruby_transform.rs`, `sql_transform.rs`, `bash_transform.rs`,
`crates/rskim/tests/cli.rs`, and `crates/rskim/tests/common/mod.rs`.

### `rskim-core` transform (14)

| Issue | File:Line | Status | Tracked |
|-------|-----------|--------|---------|
| regression-06 (a) | `crates/rskim-core/src/transform/minimal.rs:344-387` | No cap on the preserved header run; a 100%-comment file still yields 0% reduction. Now *pinned* by the branch's own fixture — `integration.rs` asserts all 500 header comments survive, so any cap must update those tests. | #494 |
| regression-06 (b) / rust-12 | `crates/rskim-core/src/transform/minimal.rs:159-160` | SQL block comments are node kind `marginalia`; `grep -rn marginalia crates/rskim-core/src` returns zero hits, so a `-- SPDX` line after a `/* … */` banner is still stripped. | #494 |
| performance-02 | `crates/rskim-core/src/transform/minimal.rs:281, 296` | `is_go_doc_comment` still walks forward via `next_named_sibling()` per Go comment. **Severity effectively raised:** 7bb8cac's own docblock now establishes as fact that `next_named_sibling()` is O(i). This is the last surviving instance of the exact defect class the branch just proved costs 8 s at N=8000. | #494 |
| performance-04 | `crates/rskim-core/src/transform/minimal.rs:371` | `gap.bytes().filter(...).count() > 1` — no `.take(2)`. Impact now negligible (runs once per leading-prefix child, not per comment node); cosmetic. | #494 |
| reliability-07 | `crates/rskim-core/src/transform/minimal.rs:369-374` | `source.get(..)` returning `None` short-circuits the `&&` chain, so an unreadable gap silently extends the header. Conservative and panic-safe, still undocumented; `is_go_doc_comment` does the same job with direct indexing at `:287`. | #494 |
| reliability-08 | `crates/rskim-core/src/transform/minimal.rs:360-384, 436` | ERROR-node determinism gap, now **doubled**: both `compute_header_end_byte` and the `depth != 1` test independently deny header status when an ERROR node wraps the file top. Needs a malformed-file fixture pinning current output. | #494 |
| architecture-04 / complexity-12 | `crates/rskim-core/src/transform/minimal.rs:347, 430` | The wildcard `_ => return 0/false` that opts new languages out with zero compiler signal is now **duplicated** across `compute_header_end_byte` and `is_module_header_comment`, defeating the CLAUDE.md "Adding a Language" exhaustive-match checklist twice over. | (untracked) |
| rust-10 / testing-11 | `crates/rskim-core/src/transform/minimal.rs:618` | `nth_root_comment<'a>(tree, _source, n)` still takes and ignores `_source`; 7bb8cac rewrote the surrounding test module and kept the dead parameter. | (untracked) |
| documentation-17 | `crates/rskim-core/src/transform/minimal.rs:4, 31, 416` | Rustdoc still lists shebangs as a motivating case for module-header preservation, though shebangs are handled by the independent `is_shebang` guard and SQL has no shebang concept. | (untracked) |
| rust-06 / architecture-07 | `crates/rskim-core/src/transform/mod.rs:328-359` vs `minimal.rs:570-596` | `trim_and_normalize` and `normalize_line_map_blanks` still hand-mirror the same two rules. **`transform/mod.rs` was never touched by this branch.** The "invariant" test at `mod.rs:708-742` remains a four-literal snapshot asserting its own known divergence. `-n` correctness is the contract at stake (PF-019). | #493 |
| documentation-10 | `crates/rskim-core/src/transform/mod.rs:737` | Comment still reads "restore at minimal.rs:406-408" — that range is the `"Range exceeds source length"` error branch; the actual restore is in `trim_and_normalize`. | (untracked) |

### Tests and docs (12)

| Issue | File:Line | Status | Tracked |
|-------|-----------|--------|---------|
| regression-04 | `ruby_transform.rs:142-155`, `sql_transform.rs:126-140` | File untouched. The negative case for the blank-line-break rule still exists only for Python. | #496 |
| testing-06 | `bash_transform.rs:290-298` | File untouched. The only Bash minimal-mode test asserts `result.is_ok()` and nothing about comment content — for the one language this branch newly enrolled. | #496 |
| regression-05 | `docs/modes.md:10`; `integration.rs:2466-2481` | `docs/modes.md` changed by exactly one line (the language-scope fix); the `15-30%` figure is still unqualified despite header-only files now reducing 0%. | #496 |
| regression-09 / consistency-15 | `crates/rskim/tests/cli.rs:385-404` | File untouched. E2E tier still has no negative case for comment stripping in minimal mode. | #496 |
| compliance-03 | `crates/rskim/tests/common/mod.rs:81-84` | File untouched. `.env_remove` covers `SKIM_HOOK_VERSION`/`SKIM_HOOK_BINARY` but **not** `SKIM_HOOK_COMMIT`, which `DriftEnv::from_process()` reads alongside them. Breaks PF-017's strict-superset property. One line. | #496 |
| compliance-04 | `crates/rskim/tests/common/mod.rs:79` | File untouched. `SKIM_ANALYTICS_DB` is not removed and takes precedence over `SKIM_CACHE_DIR`; containment still depends on `SKIM_DISABLE_ANALYTICS=1` — a control conditional on another control. | #496 |
| testing-05 | `crates/rskim/src/cmd/init/helpers.rs:26` | Nothing tests the `canonicalize()` symlink resolution that is the entire reason the helper exists — now **more** important, since it feeds six consumers including the hook-exec drift path. | #492 |
| testing-02 | `crates/rskim/tests/cli_doctor.rs:143` | `.failure()` still accepts any non-zero status. Now sits on `test_doctor_exits_1_on_tampered_script`, one of only two remaining tests asserting the "exit 1 on drift" CI contract. One token. | #489 |
| testing-10 | `crates/rskim/tests/cli_init.rs:1878` | Still asserts "not wrong" rather than "correct" — `updated.contains("export SKIM_HOOK_BINARY=")` would pass if the script pinned a third unrelated path. | #496 |
| documentation-15 / -16 | `.devflow/features/analytics/KNOWLEDGE.md:6, 17` | `directories` still includes the catch-all `crates/rskim/src`; `updated: 2026-06-25` unchanged on a file committed 2026-08-18. | (untracked) |

### `init` / `doctor` / `hook` (20)

| Issue | File:Line | Status | Tracked |
|-------|-----------|--------|---------|
| rust-04 / consistency-05 / architecture-09 | `crates/rskim/src/cmd/init/state.rs:59-76` | `pin_is_current()` still calls `resolve_skim_binary()` at `:65` instead of reading `self.skim_binary`, which `detect_state` already populated. The fixture still sets a `skim_binary` the predicate provably never reads. Blast radius reduced (advisory path only), tier now Standard. | #492 |
| regression-08 / rust-08 / testing-04 | `crates/rskim/src/cmd/init/state.rs:1058` | `let Some(running_path) = running else { return; };` still inside a `#[test]` — an early return is reported as a PASS. Additionally `state.rs:1057` holds a byte-for-byte copy of the very expression 37c87d6 deleted from `doctor/mod.rs`. | #492 |
| reliability-03 | `crates/rskim/src/cmd/init/helpers.rs:33` | `Ok(canonicalize(&p).unwrap_or(p))` still swallows failure on every channel, with no `debug_log!`. **Blast radius widened by the restructure** — six consumers now share this one silent fallback. | #492 |
| consistency-10 | `crates/rskim/src/cmd/init/install.rs:523` | The `if flags.dry_run` block still sits **after** the fast-path early return, so `skim init --dry-run` on a current install prints "Already up to date" and previews nothing. Partial mitigation only: `--dry-run --wrappers` now previews wrappers. | #491 |
| testing-07 | `crates/rskim/src/cmd/init/install.rs:784` | The `result.created + result.updated > 0` gate on the PATH-setup blurb is still untested on either side — though now cheap to close, since an existing test already performs a second `--wrappers` run that lands in the `== 0` branch. | #496 |
| complexity-05 | `crates/rskim/src/cmd/init/install.rs:496-502` | Materially improved (7 terms → 6, hidden I/O hoisted into named bindings) but not closed: mixed polarity remains and the condition still discards *which* term failed — the one thing a user re-running `skim init` wants. Now Standard-tier. | #491 |
| security-05 | `crates/rskim/src/cmd/doctor/mod.rs:475, 491, 492, 503` | `{pin}`, read verbatim from the hook script, is still interpolated straight into terminal output — now at **four** render sites, up from three. No escaping or lossy-quoting helper exists. | #490 |
| consistency-02 | `crates/rskim/src/cmd/doctor/mod.rs:467, 470, 491-492` | Label drift unchanged (`binary:` at `:467`/`:470`, `running:` at `:492`) and the pin is still rendered **twice** in one format string (`pin: {pin}` then `hook: {pin}`). | #490 |
| complexity-04 | `crates/rskim/src/cmd/doctor/mod.rs:485` | Impurity survives in identical form; only the callee changed (`resolve_skim_binary()` instead of `current_exe()`). Tests still can only assert `line.contains("running:")`, never the path. | #490 |
| complexity-08 | `crates/rskim/src/cmd/doctor/mod.rs:377-506` | Still ~130 lines doing four separable jobs, and the duplicated `facts.hook_binary_pin.as_deref().unwrap_or("?")` binding is now computed **three** times where triage found two. | #490 |
| consistency-11 | `crates/rskim/src/cmd/doctor/mod.rs:454, 476, 493` | `run \`./target/release/skim init --yes\`` — a repo-local development path — still shipped in end-user diagnostic output at three sites, while `:406`/`:415` correctly say `skim init --agent {agent}`. | #490 |
| reliability-06 | `crates/rskim/src/cmd/doctor/mod.rs:518` | `hook_facts(agent)` returning `Err` still prints the neutral `–` marker and `continue`s **without setting `any_drift`** — the one state where doctor prints a problem and reports HEALTHY exit 0. Two lines. | #489 |
| complexity-07 | `crates/rskim/src/cmd/init/install.rs:506`; `doctor/mod.rs:481-482`; `cli_init.rs:1726-1728` | The two originally-cited sites were fixed, but the replacement commits freshly violate the same "leave the end-state, not the transition" rule — e.g. "(C-3 fix: repeat `--wrappers` no longer clobbers settings.json.bak)". | #497 |
| complexity-11 | `crates/rskim/src/cmd/doctor/mod.rs`, `install.rs` | **Worse, not better.** The originally-cited labels mostly went away with their functions, but a fresh generation replaced them — `C-1`, `C-2`, `C-3`, `D4`, `D5`, `A-1` — none resolving to anything a future reader can look up. | #497 |
| architecture-06 | `crates/rskim/src/cmd/rewrite/hook.rs:601-608` | `check_hook_integrity`'s `Err(_)` arm still returns `false` and logs **nothing**, even though `script_path.exists()` already passed — "file exists but cannot be read" is indistinguishable from "nothing wrong" (PF-016). | #502 |
| documentation-13 | `crates/rskim/src/cmd/rewrite/hook.rs:559-563` | Docblock still says "`false` if valid, missing, or check was skipped" and does not cover the `Err(_)` unreadable arm — directly above six lines of body comment explaining that arm. | (untracked) |

### Suggested sequencing for the follow-up

1. **performance-02** — the last live instance of the O(N²) `parent()`-derived traversal class, in a file whose docblock now proves the cost. Highest value, and the fix pattern is already written and measured.
2. **rust-06 / architecture-07** — `-n` line-map correctness; `transform/mod.rs` was never opened this cycle.
3. **security-05, reliability-06, architecture-06** — the three remaining security/fail-open findings.
4. **compliance-03 / -04, testing-02, testing-10, rust-10** — five one-line or one-token edits.
5. **regression-04, testing-06, regression-09, testing-05** — the untouched test files; pure additions, no production risk.

