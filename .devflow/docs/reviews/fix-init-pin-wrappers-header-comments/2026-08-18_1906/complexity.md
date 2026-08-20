# Complexity Review Report

**Branch**: fix/init-pin-wrappers-header-comments -> main
**PR**: #488
**Date**: 2026-08-18 19:06
**Diff command**: `git diff main...HEAD`
**Scope**: 21 files, +1211/-158. Focus: init fast-path gating, `hook_status_line` branch ladder, `is_module_header_comment`, end-state residue.

---

## Verdict on the three focus questions

| Question | Answer |
|---|---|
| Is the combined fast-path boolean still comprehensible and correctly ordered? | **Correctly ordered, but no longer comprehensible at a glance.** 7 terms, mixed polarity, mixed eager/lazy evaluation, and two terms perform syscalls. See B-3. |
| Is `hook_status_line`'s ladder readable after the new branch + removal? | **The ladder itself is fine (3 arms, correct order).** The problem is that its correctness now rests on an invariant replicated in three files with nothing enforcing it, and the new branch broke the function's dependency-injection contract. See B-1, B-2, B-4. |
| Cyclomatic complexity / nesting / length of `is_module_header_comment`? | **Length and nesting are fine** (37-line body, depth 3). The issues are an unbounded `loop` and a quadratic call pattern. See B-5. |

---

## Issues in Your Changes (BLOCKING)

### HIGH

**The "unknown"-commit rule is now triplicated, and the removed `else` fallback made agreement load-bearing** — `crates/rskim/src/cmd/doctor/mod.rs:466-489`
**Confidence**: 90%

- Problem: the same special case ("compiled commit == `\"unknown\"` ⇒ commit comparison is indeterminate ⇒ treat as OK") is now implemented independently in three places:
  - `crates/rskim/src/cmd/init/state.rs:97-108` (`hook_is_current()`)
  - `crates/rskim/src/cmd/init/install.rs:856-863` (`is_hook_script_current()`)
  - `crates/rskim/src/cmd/doctor/mod.rs:466-470` (`hook_status_line()`) — new in this PR

  I verified the dead-code claim for the removed `stale` terminal and it holds: with `hook_uses_pinned_binary` already forced true by the early return at `mod.rs:449`, `hook_is_current()` reduces to `version_ok ∧ commit_ok` under both the `unknown` and non-`unknown` branches, so `commit_ok ∧ version_ok ⇒ hook_is_current ⇒ !pin_is_current`. The deduction is sound **today**.

  The complexity cost is that it is only sound because three separately-maintained copies of the rule agree. There is no shared predicate and no test asserting the equivalence. If any one copy drifts (say `hook_is_current()` gains a fourth condition), the `else` at `mod.rs:479-488` stops being a proof and becomes a fabricated diagnostic: it prints `binary pin mismatch (hook: X, running: Y)` unconditionally, including when `X == Y`. A provenance tool that confidently misreports the cause is precisely the failure shape of PF-015 ("three such gates exist and one was wrong"), and the `else` was the safety net that previously absorbed exactly that class of drift. `avoids PF-015` is the intent; the implementation re-creates the pre-condition for it.
- Impact: the removal is correct but fragile. It converted an implicit invariant into an unenforced one, in the subsystem PF-015 was written about.
- Fix: extract one shared function and call it from all three sites, e.g. in `init/state.rs`:
  ```rust
  /// Compare a hook-recorded commit against the compiled commit.
  /// `None` == indeterminate (tarball/non-git build) — never a mismatch.
  pub(crate) fn commit_matches(hook_commit: Option<&str>, compiled_commit: &str) -> bool {
      if compiled_commit == "unknown" { return true; }
      hook_commit == Some(compiled_commit)
  }
  ```
  Then in `hook_status_line`, replace the hand-rolled `commit_ok` with `commit_matches(facts.hook_commit.as_deref(), compiled_commit)`. Add one test asserting `hook_is_current() == (version_ok && commit_matches(..))` so a future divergence fails loudly instead of silently invalidating the `else`.
- Note: the coordinator flags that the `commit_ok`/`unknown` fix and the stale-branch removal are coupled and must not be separated. This finding is the maintenance-side consequence of that coupling — it should be made explicit in code, not only in a comment.

**Two pin-currency gates in series with opposite absent-pin semantics** — `crates/rskim/src/cmd/init/state.rs:59-73` and `crates/rskim/src/cmd/init/install.rs:868-876`
**Confidence**: 80%

- Problem: this PR adds a *second* implementation of "does the hook's pin equal the running binary". Both canonicalize and compare, but they disagree on the absent-pin case, and they sit in series on the same code path:
  - `pin_is_current()` (`state.rs:60-62`): pin absent → `false` (stale, bypass fast path).
  - `is_hook_script_current()` (`install.rs:868-876`): `if let Some(pin) = parse_binary_pin_from_script(..)` → pin absent → check **skipped** → returns `true` (current, skip rewrite).

  These are reachable together. `script_has_pinned_marker` (`init/mod.rs:173-177`) matches on the prefix `export SKIM_HOOK_BINARY=` and accepts an empty value; `parse_binary_pin_from_script` (`state.rs:449-451`) returns `None` when the value is empty. So a script with a bare `export SKIM_HOOK_BINARY=` line satisfies the marker but yields no pin. Result: `pin_is_current()` = false → fast path bypassed at `install.rs:504-511` → `execute_install` → `create_hook_script` → `is_hook_script_current()` = true at `install.rs:896` → prints `Skipped: ... (already vX)` and returns. Every `skim init` does the full install dance and never fixes the pin, while `skim doctor` reports `binary pin mismatch (hook: ?, running: …)` forever. This is PF-015's "SECOND GATE" defect verbatim, re-introduced by the new predicate rather than avoided by it.
- Impact: a non-converging `skim init` for a malformed-but-marker-valid script. Reachability in the wild is limited (requires a hand-edited or truncated script), which is why this is 80% and not higher — but the two-gate structure itself is the durable defect, and PF-015 exists because this exact structure already shipped once.
- Fix: make `is_hook_script_current` fail closed on an absent pin so both gates agree:
  ```rust
  // Marker present but no parseable pin → malformed script, force a rewrite.
  let Some(pin) = parse_binary_pin_from_script(&contents) else { return false; };
  let Ok(running) = super::helpers::resolve_skim_binary() else { return false; };
  let pin_path = std::path::Path::new(pin.as_str());
  let canon_pin = std::fs::canonicalize(pin_path).unwrap_or_else(|_| pin_path.to_owned());
  running == canon_pin
  ```
  Better still: have `is_hook_script_current` and `pin_is_current` share one `pin_matches(pin: Option<&str>) -> bool` helper so there is one definition of the comparison.
- Sub-finding (same hunk, **confidence 85%**): the `&& let Ok(running) = super::helpers::resolve_skim_binary()` arm at `install.rs:869` is defensively dead. `detect_state` already calls `resolve_skim_binary()?` at `state.rs:121`, so `run_install_single` aborts before `create_hook_script` can ever observe an `Err`. The `let-else` form above removes the dead arm.

### MEDIUM

**New pin-mismatch branch reimplements `current_exe_canonical()` from 386 lines above it in the same file** — `crates/rskim/src/cmd/doctor/mod.rs:483-487`
**Confidence**: 95%

- Problem: `doctor/mod.rs:96-100` already defines
  ```rust
  fn current_exe_canonical() -> Option<PathBuf> {
      std::env::current_exe().ok().map(|p| std::fs::canonicalize(&p).unwrap_or(p))
  }
  ```
  and it is already used at `mod.rs:37`. The new branch hand-rolls the identical logic in a more convoluted shape:
  ```rust
  let running = std::env::current_exe().ok()
      .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)))
      .map(|p| p.to_string_lossy().into_owned())
      .unwrap_or_else(|| "?".to_string());
  ```
  The `and_then` closure returns `Some(..)` on every path (`.ok().or(Some(p))` is total), so `and_then` is a `map` in disguise — a reader has to prove that before they can conclude the `None` case is unreachable from that arm.
  This also quietly breaks the PR's own headline invariant. The KB (and `helpers.rs:16-25`) assert `resolve_skim_binary()` is the single source of truth for the canonical binary path; this PR adds a fourth independent computation of it. `helpers::resolve_skim_binary` is `pub(super)` to `cmd::init`, so `cmd::doctor` genuinely cannot call it — which is the real signal: the helper is scoped one level too tightly for the invariant it claims.
- Impact: three copies of one path-resolution rule inside one crate; a future change to canonicalization semantics has to find all of them.
- Fix: use the existing local helper —
  ```rust
  let running = current_exe_canonical()
      .map(|p| p.display().to_string())
      .unwrap_or_else(|| "?".to_string());
  ```
  and separately consider promoting `resolve_skim_binary` to `pub(crate)` and having `current_exe_canonical` delegate to it, so the "single source of truth" claim in `helpers.rs:16-25` is actually true.

**The new branch breaks `hook_status_line`'s dependency-injection contract, capping what its tests can assert** — `crates/rskim/src/cmd/doctor/mod.rs:377-382, 483-487`
**Confidence**: 90%

- Problem: `hook_status_line` takes `compiled_version` and `compiled_commit` as parameters precisely so it stays pure and fully assertable (the caller at `mod.rs:525` supplies them from `env!`/`option_env!`). The new `else` arm reaches straight into process state instead. The consequence is visible in the tests this PR adds: `test_hook_status_line_pin_mismatch_verified_is_drift` can only assert `line.contains("running:")` (`mod.rs:~1274`) — it cannot assert the path, cannot construct a pin-equals-running case, and cannot assert the branch does *not* fire when the paths match. That is a display-layer-only assertion over an acquisition-layer value: the shape PF-015 §(2) calls out ("fixture-shaped tests exercise the DISPLAY layer only").
- Impact: the one branch this PR was written to make reachable is the one branch whose output tests cannot pin down.
- Fix: inject it like the other two ambient values.
  ```rust
  fn hook_status_line(
      facts: &crate::cmd::init::HookFacts,
      agent_cli_name: &str,
      compiled_version: &str,
      compiled_commit: &str,
      running_binary: &str,   // caller passes current_exe_canonical()
  ) -> (bool, String)
  ```
  Callers pass `&current_exe_canonical().map(|p| p.display().to_string()).unwrap_or_else(|| "?".into())`. Tests can then assert the full message and add the negative case.

**Seven-term fast-path condition: mixed polarity, mixed eager/lazy, hidden syscalls** — `crates/rskim/src/cmd/init/install.rs:504-511`
**Confidence**: 90%

- Problem:
  ```rust
  if state.hook_installed
      && state.hook_is_current()
      && state.pin_is_current()
      && guidance_current
      && !permissions_blocked
      && !wrappers_blocked
      && manifest_present
  ```
  Ordering is correct — `hook_installed` short-circuits ahead of `pin_is_current()`, so the syscalls never run for an uninstalled hook. Comprehensibility is the problem, and it crossed the line with this PR:
  1. **7 boolean terms** (was 5). The complexity guideline puts 5+ conditions in the HIGH band.
  2. **Mixed polarity.** Five read positively ("is current", "present") and two are negated `*_blocks_fast_path` results. The reader has to invert two of seven terms mid-expression.
  3. **Mixed eager/lazy.** Four terms are locals computed unconditionally at lines 492, 493, 500, 503; two are lazily-evaluated method calls; one is a field. Short-circuiting therefore saves nothing for four of the seven, but a reader must still trace which are which.
  4. **Hidden I/O in a boolean.** `state.pin_is_current()` performs `current_exe()` + up to two `canonicalize()` syscalls (`state.rs:63-71`). Nothing at the call site signals that. (Related, and confirmed by another reviewer: `pin_is_current()` re-derives the running path instead of reading `DetectedState.skim_binary`, which `detect_state` populates from the same helper at `state.rs:121` — the syscalls are avoidable entirely.)
  5. **No diagnosis.** The condition answers "is it current?" but discards *why* it wasn't, which is the thing a user re-running `skim init` actually wants and the thing `skim doctor` reconstructs separately.
- Impact: the predicate governing install idempotence is the highest-traffic decision in this subsystem and now takes real effort to verify by eye. Every prior bug in this area (#466, #471, #477, #478) was a mis-gated condition.
- Fix: name it, and return the reason rather than a bare bool.
  ```rust
  /// Why the "already up to date" fast path was declined; `None` == take it.
  enum FastPathBlocker { NotInstalled, HookStale, PinStale, GuidanceStale, Permissions, Wrappers, ManifestMissing }

  fn fast_path_blocker(state: &DetectedState, guidance_current: bool,
                       permissions_blocked: bool, wrappers_blocked: bool,
                       manifest_present: bool) -> Option<FastPathBlocker> {
      if !state.hook_installed        { return Some(FastPathBlocker::NotInstalled); }
      if !state.hook_is_current()     { return Some(FastPathBlocker::HookStale); }
      if !state.pin_is_current()      { return Some(FastPathBlocker::PinStale); }
      if !guidance_current            { return Some(FastPathBlocker::GuidanceStale); }
      if permissions_blocked          { return Some(FastPathBlocker::Permissions); }
      if wrappers_blocked             { return Some(FastPathBlocker::Wrappers); }
      if !manifest_present            { return Some(FastPathBlocker::ManifestMissing); }
      None
  }
  ```
  Call site becomes `if fast_path_blocker(..).is_none() { print_already_up_to_date(); return ...; }`. Every guard reads in one polarity, ordering is explicit rather than emergent from `&&`, and each blocker becomes directly unit-testable — which would also have made B-2's untestable branch testable.

**`is_module_header_comment`: unbounded `loop`, and quadratic when called across a comment-dense file** — `crates/rskim-core/src/transform/minimal.rs:293-329` (fn at ~293, loop at ~305)
**Confidence**: 90%

- Problem: length (37-line body) and nesting (depth 3) are both fine. Two other things are not.
  1. **Unbounded loop.** The backward `prev_named_sibling()` walk is a bare `loop { .. }` with no counter. It does terminate — each iteration moves strictly backwards through a finite sibling list — but the project reliability rule is explicit: *"All loops and retries must have a fixed upper bound — no unbounded `while(true)`."* Every other bounded traversal in this module carries an explicit cap (`MAX_AST_DEPTH`, `MAX_AST_NODES`, imported at `pseudo.rs:23`); this one does not, so it also reads as inconsistent with local convention.
  2. **Quadratic call pattern.** `is_module_header_comment` is called from `is_removable_comment` (`minimal.rs:148-157`), which is called once per node from `collect_removable_comments` (`minimal.rs:95`) and again from `pseudo.rs:446`. I confirmed the short-circuit ahead of it does not save you for the four target languages: `is_doc_comment` returns a hard `false` for Python, Ruby, SQL, and Bash (`minimal.rs:~193, ~202, ~216, ~220`). So for a file whose root children are N contiguous comments, comment *i* walks back *i* siblings — Θ(N²) total. A 20k-line contiguous comment block (a commented-out SQL dump, a generated Bash header, a fixture file) is ~2·10⁸ gap scans. Against the stated budget of <50ms per 1000 lines and <1s for 100 files, that is a real cliff.
- Impact: a per-node recomputation of what is actually a per-file property, plus an unbounded loop in a hot transform path.
- Fix: compute the header extent once and make the per-node test O(1). This removes the loop, the quadratic behaviour, and the reliability-rule violation together:
  ```rust
  /// End byte of the module header block (0 when the file has no header).
  /// Computed once per transform, forward, bounded by root child count.
  fn module_header_end(root: Node, source: &str, language: Language) -> usize {
      if !matches!(language, Language::Python | Language::Ruby | Language::Sql | Language::Bash) {
          return 0;
      }
      let mut end = 0usize;
      let mut prev_end: Option<usize> = None;
      for i in 0..root.named_child_count() {
          let Some(child) = root.named_child(i) else { break };
          if !is_comment_node(child.kind(), language) { break; }
          if let Some(pe) = prev_end
              && source.get(pe..child.start_byte())
                       .is_some_and(|g| g.bytes().filter(|&b| b == b'\n').count() > 1)
          { break; }
          end = child.end_byte();
          prev_end = Some(end);
      }
      end
  }
  ```
  Then `is_module_header_comment` becomes `node.parent().is_some_and(|p| p.parent().is_none()) && node.end_byte() <= ctx.module_header_end`. The five new unit tests carry over essentially unchanged.
- Open question (not investigated — see note at end): the extent would need threading through `collect_removable_comments`' context struct and `pseudo.rs`'s `ctx`. If that plumbing is judged too invasive for this PR, the minimal mitigation is a bounded walk (`for _ in 0..MAX_HEADER_COMMENTS`) which fixes the reliability-rule violation but not the quadratic term.

**End-state residue: three tombstone comments and two rotting line references in production code**
**Confidence**: 90%

Consolidated — five instances of the same pattern, all added or left by this PR. Project rule: *"Leave the end-state, not the transition — after removing or renaming, strip the residue: tombstone comments ('no longer does X'), leftover migration scaffolding. Git holds the history."*

| # | Location | Residue |
|---|---|---|
| 1 | `crates/rskim/src/cmd/doctor/mod.rs:473-474` | `"The former \"stale\" fallback that followed was dead code and has been removed."` — narrates a deletion. The *reason* (`commit_ok ∧ version_ok ⇒ pin path`) is worth keeping; the obituary is not. |
| 2 | `crates/rskim/src/cmd/init/helpers.rs:24-25` | `"preserving the behaviour of the pre-unification code at install.rs:895-906"` — tombstone **plus** a stale line reference. `create_hook_script` now begins at `install.rs:880`; lines 895-906 no longer hold the referenced code, so the pointer is already wrong at merge time. |
| 3 | `crates/rskim/src/cmd/init/state.rs:56-58` | `"before the fix would have been taken by the former fast-path check"` — describes the transition, not the end state. |
| 4 | `crates/rskim/src/cmd/doctor/mod.rs:371-372` | Docblock still says `Unreadable → names the suppression coupling`, which this PR deleted from the code (`mod.rs:410-418`) and added a test forbidding (`mod.rs:1002-1008`). *Confirmed independently by another reviewer; listed here for completeness as the sharpest instance of the rule.* |
| 5 | `crates/rskim/src/cmd/doctor/mod.rs:~847` (test comment) | `"The \"binary pin mismatch\" path at line ~477 was previously unreachable"` — a line number embedded in a comment, guaranteed to rot on the next edit. |

- Fix: rewrite 1-3 as end-state statements (e.g. for #1: *"`commit_ok ∧ version_ok` implies the mismatch is in the pin path — two clones at the same commit."*), delete the `install.rs:895-906` pointer entirely, correct #4 to match the code, and replace "at line ~477" with the branch name.

---

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`hook_status_line` is now ~131 lines with cyclomatic complexity ~13** — `crates/rskim/src/cmd/doctor/mod.rs:377-507`
**Confidence**: 85%

- Problem: the function spans 377-507. Decision points: 1 early return + a 4-arm `match` + a closure containing an `if` + 2 guard returns + a 3-arm `if/else if/else` ≈ 13. Both figures sit in the HIGH band (50-200 lines; complexity 10-20). It was already long before this PR; this PR added ~20 lines and one more nested branch to it. It now does four separable jobs: integrity gating, advisory composition, pin-format gating, and mismatch-cause derivation.
- Impact: this is the function every future hook-state change must be re-verified against, and the one whose reachability analysis the removed `else` now depends on.
- Fix: extract the cause derivation, which is self-contained and would carry B-1 and B-2 with it:
  ```rust
  fn mismatch_reason(facts: &HookFacts, compiled_version: &str,
                     compiled_commit: &str, running_binary: &str) -> String { .. }
  ```
  `hook_status_line` drops to ~100 lines, the reason ladder becomes independently testable, and the `else`-is-the-only-terminal argument lives next to the code that depends on it.
- Minor, same function: `let pin = facts.hook_binary_pin.as_deref().unwrap_or("?")` is computed twice (`mod.rs:460` and `mod.rs:500`). Hoist above the branch.

---

## Pre-existing Issues (Not Blocking)

### MEDIUM

**A fourth canonical-path computation with different failure semantics** — `crates/rskim/src/cmd/rewrite/hook.rs:86`
**Confidence**: 85%

- Problem: `std::env::current_exe().and_then(std::fs::canonicalize).ok()` yields `None` when canonicalization fails, whereas `resolve_skim_binary()` (`helpers.rs:26-33`) and `current_exe_canonical()` (`doctor/mod.rs:96-100`) both fall back to the raw path. So on a machine where canonicalization fails, `check_hook_binary_mismatch` silently skips the comparison while `skim init` and `skim doctor` still perform it.
- Not in this diff — informational only. Worth folding into the same cleanup as B-1, since that PR would already be consolidating path resolution and the KB claim at `helpers.rs:16-25` ("single source of truth") is only true once all four sites route through one helper.

---

## Suggestions (Lower Confidence)

- **`wrappers_blocks_fast_path` is a constant in a match's clothing** — `crates/rskim/src/cmd/init/install.rs:169-175` (Confidence: 70%) — three arms, two returning `false`; the whole body reduces to `flags.wrappers == Some(true)`. The explicit match does document each case and the `None` arm's comment is genuinely load-bearing, so this is defensible as-is; noting it only because the mirrored `permissions_blocks_fast_path` immediately below has real per-arm logic, which sets an expectation this one doesn't meet.
- **Review-cycle labels have no external referent** — `install.rs:854` ("B5c follow-up"), `doctor/mod.rs:465` ("Defect 3"), `mod.rs:836` ("Fix 4b"), `transform/mod.rs:676` ("Fix 5c invariant"), `minimal.rs:474` ("Fix 3e"), `install.rs:498` ("Group 4 fix") (Confidence: 65%) — unlike `#477`, these resolve to nothing a future reader can look up. The codebase already carries ~40 such labels on `main`, so this is house style rather than a new violation; flagging only because this PR adds roughly a dozen more and the "leftover scaffolding" rule technically covers them.
- **`is_module_header_comment`'s language gate is a statement-position `match`** — `minimal.rs:294-297` (Confidence: 60%) — `match language { A|B|C|D => {}, _ => return false }` reads oddly next to `matches!` used elsewhere in the file (`minimal.rs:272`). `if !matches!(language, ..) { return false; }` is the local idiom.

---

## Open Questions

1. **Header-extent plumbing cost** (relates to B-5): I did not trace how much surgery threading a precomputed `module_header_end` through `collect_removable_comments`' context and `pseudo.rs:446`'s `ctx` would require. If it is more than a field addition, the bounded-walk mitigation is the pragmatic fallback. Confidence the O(N²) itself is real: 90%. Confidence it matters on realistic inputs: 70%.
2. **Empty-pin reachability** (relates to B-1, second finding): I verified the mechanism (`script_has_pinned_marker` accepts `export SKIM_HOOK_BINARY=` with an empty value while `parse_binary_pin_from_script` returns `None` for it) by reading both functions. I did not construct the fixture to confirm the non-converging init end-to-end. Confidence in the mechanism: 90%. Confidence a user hits it: 55%. The two-gate structure is worth fixing regardless of reachability.

---

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 5 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 1 | 0 |

**Complexity Score**: 6/10
**Recommendation**: APPROVED_WITH_CONDITIONS

Rationale: nothing here is a correctness defect at merge time — the fast-path ordering is right, the dead-branch removal is provably sound, and `is_module_header_comment` terminates. The score reflects that the PR's central improvement (unifying binary-path resolution behind `resolve_skim_binary()`) is undercut in the same diff by three new duplications of the logic it was meant to centralize: the inline `current_exe` at `doctor/mod.rs:483`, the second pin comparison at `install.rs:868`, and the third copy of the `"unknown"`-commit rule at `doctor/mod.rs:466`. Each is individually small; together they re-create the multi-gate topology that PF-015 documents as this subsystem's recurring failure mode, and the removal of the `stale` fallback deleted the net that used to catch it.

Conditions for merge — the two HIGH findings:
1. Extract a shared `commit_matches()` used by all three sites, with a test pinning `hook_is_current()`'s equivalence (B-1).
2. Make `is_hook_script_current`'s absent-pin case fail closed so it agrees with `pin_is_current()` (B-1, second finding), and drop the dead `Ok(running)` arm.

The MEDIUM findings — reuse `current_exe_canonical()`, inject `running_binary` as a parameter, extract `fast_path_blocker()`, bound the header walk, strip the five residue instances — are all mechanical and would leave this subsystem materially easier to verify than it was before the PR.
