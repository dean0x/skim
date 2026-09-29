# tracking: fidelity defect batch — 13 defects + 1 prerequisite, one PR

Empirical triage against the release binary at `c2b4378` classified roughly 30
agent-reported skim defects: **13 real**, 5 already repaired, 8 working-as-designed, 3
environmental, 1 retracted. Triage also surfaced **4 real defects nobody had reported**,
two of them high severity.

Two root causes account for most of the batch:

1. **skim has no notion of "this output format is a machine contract."** Every `git` case
   that passes through byte-identically today does so by *coincidence* of two blunt
   mechanisms — the ADR-001 net-savings byte comparison, and "non-zero exit implies
   forward raw." Bare `--porcelain` survives only because compression happens to be
   larger than raw on the measured input. The passing cases are not correct, they are
   lucky, and any change to a renderer's byte count silently converts one of them into a
   corrupted machine contract.
2. **`--mode=pseudo` destroys type-level declarations**, producing output that does not
   parse as the source language (`tsc` gives hard `TS1131` / `TS1128` errors) and, in one
   case, renders two distinct union members byte-identically.

All 13 items plus one shared prerequisite land in a **single PR**, each with its own
branch-local commit, its own regression test, and a **falsifiable** acceptance check
measured against the pinned baseline binary *before* the change is written.

- **Branch:** `fix/fidelity-defect-batch-13`
- **Base:** `c2b4378` on `main`
- **Ledger:** `.devflow/docs/tickets/fix-fidelity-defect-batch-13/2026-09-28_2027/LEDGER.md`
- **Test plan:** `.devflow/docs/evidence-fix-fidelity-defect-batch-13.md` (TP-1 … TP-17)

---

## PREREQ

- [ ] **G** · PREREQ · git contract-flag passthrough gate — a machine-contract flag routes
      the subcommand to raw passthrough instead of a parsing handler · `crates/rskim/src/cmd/git/mod.rs`

**G** is **non-droppable**: **F6**, **F9** and **F13** depend on it and drop with it as a
unit.

## BLOCKING

- [ ] **F1** · BLOCKING · pseudo mode preserves type-level declarations, so a TypeScript
      interface member keeps its type annotation and the view parses · `crates/rskim-core/src/transform/pseudo.rs`
- [ ] **F1b** · BLOCKING · pseudo mode preserves the `;` inside a TypeScript type or
      interface body · `crates/rskim-core/src/transform/pseudo.rs`
- [ ] **F1c** · BLOCKING · pseudo mode preserves the declaration-terminating `;` on a Rust
      trait method signature · `crates/rskim-core/src/transform/pseudo.rs`
- [ ] **F2** · BLOCKING · the rewrite engine declines to rewrite when the target binary
      cannot be resolved, instead of emitting a command that errors · `crates/rskim/src/cmd/rewrite/engine.rs`, `crates/rskim/src/runner.rs`
- [ ] **F3** · BLOCKING · `npm ls` keeps each package version in its compressed summary
      instead of reading and discarding it · `crates/rskim/src/cmd/pkg/npm/ls.rs`

## HIGH

- [ ] **F4** · HIGH · `git show <rev>:<path>` is byte-faithful by default, honours an
      explicit `--mode`, and discloses the opt-in lossy view with a class-1 marker · `crates/rskim/src/cmd/git/show.rs`
- [ ] **F5** · HIGH · the diff line-number axis convention is documented in help text and
      mode docs (documentation only — no runtime change) · `crates/rskim/src/cmd/git/diff/*.rs`, `README.md`
- [ ] **F6** · HIGH · `git log --stat` reaches the reader instead of being silently
      dropped by the commit-header line filter (absorbed into **G**) · `crates/rskim/src/cmd/git/mod.rs`, `crates/rskim/src/cmd/git/log.rs`
- [ ] **F7** · HIGH · `git push` renders both sides of an asymmetric refspec (`src:dst`)
      rather than the source side alone · `crates/rskim/src/cmd/git/push.rs`
- [ ] **F8** · HIGH · `git push --delete` renders the deleted ref name instead of a blank,
      on git's real porcelain delete shape · `crates/rskim/src/cmd/git/push.rs`

## MED

- [ ] **F9** · MED · `git status --porcelain=v2 --branch` keeps its `# branch.ab`
      ahead/behind header (subsumed by **G**) · `crates/rskim/src/cmd/git/mod.rs`, `crates/rskim/src/cmd/git/status.rs`
- [ ] **F10** · MED · `gh run list` stops prepending `#` to a `databaseId`, so the
      identifier can be reused and does not autolink to an unrelated issue · `crates/rskim/src/cmd/infra/gh/list.rs`
- [ ] **F11** · MED · `--debug` and `--passthrough` are honoured when they appear after a
      file positional · `crates/rskim/src/main.rs`
- [ ] **F12** · MED · an interior-newline rewrite bail records a rate-limited signal in the
      hook log instead of exiting silently · `crates/rskim/src/cmd/rewrite/hook.rs`

## LOW

- [ ] **F13** · LOW · `--porcelain -z` output gains no extra trailing byte on the ADR-001
      passthrough verdict · `crates/rskim/src/cmd/execution.rs`, `crates/rskim/src/cmd/git/mod.rs`

## Test harness

- [ ] **C0g** · — · close the `assert_render_fidelity` vacuity hole, where every assertion
      sits inside a loop over a collection that is not required to be non-empty · `crates/rskim/tests/cli_git_diff_modes.rs`

---

## Link-back convention

Use **PR-scoped permalinks** of the form `/pull/MMM/commits/<sha>`, where `MMM` is this
PR's number (it does not exist yet — leave the placeholder until the PR is opened, then
substitute it everywhere in one pass).

A PR-scoped permalink survives both squash-merge and branch deletion. A bare SHA, or a
`dean0x/skim@<sha>` reference, does **not**: the repository merges by squash only, so
`main` receives a single commit and every per-fix SHA on this branch becomes unreachable
once the branch is deleted. Per-fix commits here are a branch-local review-and-revert
device, which is also why rationale has to live in the PR body and the in-repo ledger
rather than in commit messages.

Checking a box on this issue should carry the PR-scoped permalink for the commit that
did the work, so a reader can reach the diff and its ledger row from the checklist.

---

## Deferred / not in this PR

Filed separately. The ledger records the reasoning for each so it is not re-litigated —
re-opening one means answering the stated reason, not merely re-raising the symptom.

- **Charged class-1 marker on the `git diff` path.** `lossy_view_marker` is roughly
  90–110 B; the measured headroom on the pinned fixture is **68 B**. Charging it flips
  that fixture to raw and trips the non-vacuity assertion in `headers_fit_the_raw_budget`.
  ADR-011 class 3 says serving raw *is* conformant when the marker does not fit, so the
  flip is designed behaviour rather than a regression — but on a 68 B headroom the flip is
  not confined to one fixture, and broadly suppressing diff enrichment leaves the reader
  worse off than an undisclosed lossy view does. Wants a flip-rate measurement over
  roughly 50 real commits first. `TWO_COLUMN_BYTES` must not be raised to make the answer
  come out (PF-027).
- **Python dataclass field annotations.** A module-level `x: int = 5` and a dataclass
  field `x: int` produce identical parent kinds; only ancestor scope separates them.
  Needs a threaded `in_class_body` bool, an `@dataclass` check and a new fixture, and
  would force amending `test_python_pseudo_strips_variable_annotation`. No reported
  instance — speculative benefit against a real amendment risk. Reconsider when an
  instance is reported.
- **`grep` resolving to BSD grep rather than the shell's bundled ugrep.** Not a fidelity
  defect in skim's own output: skim faithfully represents the `grep` it ran. A PATH probe
  is structurally blind to shell functions, so **F2**'s machinery cannot detect it.
  Belongs on `#319`; proposed shape there is a `skim doctor` warning naming shadowed
  tools, since `doctor` is already the provenance surface.

**Explicitly out of scope** (not deferrals): `head`/`tail` marker-inside-N (ADR-016,
working as designed); the compressed-vs-raw diff format switch, tracked on `#509` as a
deliberate class-2 policy; the ugrep regex errors (environmental).

---

## Related work

- `#576` — expected to be **settled as a side effect** of the **G** prerequisite rather
  than by a change of its own, since a contract-flag gate routes the affected invocation
  to raw passthrough. **Confirm before relying on it:** `#576` concerns
  `git diff --quiet -- <file>`, which is an *exit-code* contract rather than an
  output-format contract, and `--quiet` is currently gated in `crates/rskim/src/cmd/git/fetch.rs`
  and `crates/rskim/src/cmd/git/push.rs` but not anywhere under `crates/rskim/src/cmd/git/diff/`.
  The side effect therefore holds only if `--quiet` is included in **G**'s flag set and
  **G** covers the `diff` subcommand. Attribution to this batch is an open review item.
- `#337` — **F12** is a **partial mitigation**, not a completion. Full composed-command
  support (parse instead of bail) remains open there.
- `#319` — the shadowed-tool deferral above.
- `#509` — the compressed-vs-raw diff format switch, out of scope here.
- `#505` — the standing consolidation hub from the earlier `#488` batch, for cross-reference.
- `#317` is the byte-fidelity precedent the whole batch reasons from ("compress, never
  truncate"; bail rather than reconstruct unfaithfully).

---

## Conventions for this issue

Every issue number above is written inside a code span — `` `#576` ``, `` `#319` ``,
`` `#337` ``, `` `#509` ``, `` `#505` `` — and that is deliberate, not stylistic. GitHub
does **not** autolink a code span, and it *does* link a bare `#number` regardless of the
prose around it: a "does NOT close" heading above bare references still links every
reference under it. Backticking is the only reliable way to name an issue here without
creating a cross-reference.

There are **no closing keywords anywhere in this issue body**, and none may be added.
Closing keywords belong **only in the PR body** — one per line, one issue per line, since
a combined form links only the first reference on the line.
