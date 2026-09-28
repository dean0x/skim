# LEDGER — Fidelity Defect Batch (13 fixes + 1 prerequisite)

| Field | Value |
|---|---|
| Campaign | Fidelity Defect Batch — 13 fixes, one PR |
| Branch | `fix/fidelity-defect-batch-13` |
| Base SHA | `c2b4378` (`fix: close empirical-triage defects — credential scrub, pipe fidelity, cargo +toolchain, ADR citations (#561)`) |
| Base branch | `main` |
| Date stamp | `2026-09-28_2027` |
| Test plan | `.devflow/docs/evidence-fix-fidelity-defect-batch-13.md` (TP-1 … TP-17 / AC-1 … AC-17) |
| Pinned baseline binary | `target/skim-baseline-c2b4378` (read-only; `skim` 2.11.0 @ `c2b4378`) |

## What this ledger is for

This ledger is the campaign's **falsifiability device**. The repository merges by
squash only, so `main` receives exactly one commit whose message is the PR body and
every per-commit rationale is discarded at merge time. Per-fix commits on this branch
are a branch-local review-and-revert device, nothing more. Rationale must therefore
live in two places that survive the merge: the PR body, and this file. Its second and
more important job is to make a *late* edit visible: per-commit mutation of this
ledger is a two-line diff (one status-table row plus one `Status:` field), so any
later change to an acceptance command or a measured `BEFORE` shows up as an
out-of-shape diff that a reviewer can see. An acceptance check written after the fix
is a check that cannot fail; an acceptance check written and measured *before* the
fix, recorded here, and then quietly rewritten, is a visible act. Gate 0 is the whole
point: **if the measured `BEFORE` does not exhibit the defect, the fix is mis-scoped
— stop and re-triage.** A clean `BEFORE` means the acceptance command is wrong, not
that the defect is absent.

**SHAs recorded in this ledger are branch-local.** After squash-merge they resolve to
nothing on `main`, and they disappear entirely when the branch is deleted. Every
link-back must use a **PR-scoped permalink** — `/pull/MMM/commits/<sha>` — which
survives both squash-merge and branch deletion. Do not write a bare SHA or a
`dean0x/skim@<sha>` reference as a durable pointer.

Status vocabulary is closed: `TODO` · `IN-PROGRESS` · `LANDED` · `DROPPED`.

## Commits (branch-local)

**These 13 SHAs exist only on `fix/fidelity-defect-batch-13`. They will not resolve on
`main` after the squash-merge, and they disappear entirely when the branch is deleted. A
durable link-back needs a PR-scoped permalink — `/pull/<N>/commits/<sha>` — never a bare
SHA.** This is the same rule the paragraph above states; it is restated here because this
is the table a reviewer will copy SHAs out of.

| # | SHA | Subject | Sections |
|---|---|---|---|
| 1 | `e9bfbf4` | test: close the assert_render_fidelity vacuity hole | C0g |
| 2 | `2d8a4d1` | fix(core): pseudo mode preserves type-level declarations and terminators | F1, F1b, F1c |
| 3 | `d3e32fa` | fix(git): serve machine-contract flags raw, ahead of the net-savings verdict | G, F6, F9 |
| 4 | `20c9c64` | fix(git): push renders both sides of a refspec pair | F7, F8 |
| 5 | `c83b57c` | fix(execution): serve the tool's own bytes exactly on the Passthrough verdict | F13 |
| 6 | `9f0864e` | fix(git): git show serves a blob byte-faithfully | F4 |
| 7 | `706fa27` | fix(pkg): npm ls keeps each package's name and version | F3 |
| 8 | `7f4e0dd` | fix(infra): gh run list renders identifiers that gh accepts | F10 |
| 9 | `959bd9f` | test: make the rewrite-engine tests PATH-hermetic | (F2 prerequisite) |
| 10 | `a2983e9` | fix(rewrite): decline to rewrite a program that cannot be resolved | F2 |
| 11 | `9a73593` | fix(rewrite): signal the interior-newline bail to hook.log | F12 |
| 12 | `48ded28` | fix(cli): honour skim's own flags after a positional argument | F11 |
| 13 | `69d7d57` | docs: document the diff line-number axis convention and this batch's doc debt | F5 |

**Deviation from the plan: 13 commits, not 16.** The plan's table wanted one commit per
fix. A reviewer comparing the two counts should not read the difference as work gone
missing — it is **five file collisions**, and path-scoped staging cannot split a file
while hunk-level staging is interactive and therefore unavailable to this workflow:

| File | Fixes that collide in it |
|---|---|
| `crates/rskim-core/src/transform/pseudo.rs` | F1, F1b, F1c |
| `crates/rskim/src/cmd/git/mod.rs` | G, `--raw`, the `--json` fix, F13's call-site routing |
| `crates/rskim/src/cmd/git/push.rs` | F7, F8 |
| `crates/rskim/tests/cli_git.rs` | F4's repairs, F5's doc sweep |
| `crates/rskim/tests/cli_passthrough_coverage.rs` | F11, F5's doc sweep |

**The cost is low.** Under squash-merge `main` receives one commit regardless, so the
per-fix granularity was never going to survive the merge; the review value survives where
it always had to — in the commit messages and in this ledger's per-fix sections. And in
every one of the five collisions the independent revert is not a state anyone would
choose: F1 alone produces valid TypeScript (F1b is the unparseability — see F1b's
isolation matrix), F8 changed **zero production lines**, and F13's `cmd/git/mod.rs` line
is a one-token call-site swap to a helper committed beside it.

**One provenance fact, recorded because it is counterintuitive and was verified.** The
`LINE NUMBERS:` help-text block F5 added to `print_diff_help()` landed in commit **3**
(`d3e32fa`), **not** the docs commit — `crates/rskim/src/cmd/git/diff/mod.rs` carried both
the gate hoist and the help text, so it collided the same way the five files above did.
Confirmed at `crates/rskim/src/cmd/git/diff/mod.rs:132` via `git log -S 'LINE NUMBERS'`,
which returns `d3e32fa` and nothing else. F5's `AFTER` field is **correct** in claiming the
help text was updated; only its commit position is surprising. The same collision spreads
the rest of F5's doc sweep across commits **6** (`tests/cli_git.rs`) and **12**
(`tests/cli_passthrough_coverage.rs`), so F5's material occupies four commits and the
`C14` label in the status table below names only the last of them.

## Status

| ID | Sev | Commit | Fix | Area | TP | Status |
|---|---|---|---|---|---|---|
| C0g | — | C0g | close `assert_render_fidelity` vacuity hole | `crates/rskim/tests/cli_git_diff_modes.rs` | TP-17 | LANDED |
| F1 | BLOCKING | C1 | pseudo preserves type-level declarations (TS) | `crates/rskim-core/src/transform/pseudo.rs` | TP-2 | LANDED |
| F1b | BLOCKING | C2 | pseudo preserves `;` in type/member bodies (TS) | `crates/rskim-core/src/transform/pseudo.rs` | TP-3 | LANDED |
| F1c | BLOCKING | C3 | pseudo preserves declaration-terminating `;` (Rust) | `crates/rskim-core/src/transform/pseudo.rs` | TP-4 | LANDED |
| G | PREREQ | C4 | git contract-flag passthrough gate | `crates/rskim/src/cmd/git/mod.rs` | TP-1 | LANDED |
| F6 | HIGH | C4 (absorbed into G) | `git log --stat` no longer dropped | `crates/rskim/src/cmd/git/mod.rs`, `log.rs` | TP-9 | LANDED |
| F9 | MED | C4 (subsumed by G) | `git status --porcelain=v2` served verbatim (premise corrected) | `crates/rskim/src/cmd/git/mod.rs`, `status.rs` | TP-12 | LANDED |
| F7 | HIGH | C5 | push renders destination ref (`src:dst`) | `crates/rskim/src/cmd/git/push.rs` | TP-10 | LANDED |
| F8 | HIGH | C6 | push renders ref name on `--delete` | `crates/rskim/src/cmd/git/push.rs` | TP-11 | LANDED |
| F13 | LOW | C7 | `--porcelain -z` extra trailing byte | `crates/rskim/src/cmd/execution.rs`, `git/mod.rs` | TP-16 | LANDED |
| F4 | HIGH | C8 | `git show <rev>:<path>` byte-faithful + honours `--mode` + class-1 marker | `crates/rskim/src/cmd/git/show.rs` | TP-7 | LANDED |
| F3 | BLOCKING | C9 | `npm ls` keeps package versions | `crates/rskim/src/cmd/pkg/npm/ls.rs` | TP-6 | LANDED |
| F10 | MED | C10 | `gh run list` stops prepending `#` | `crates/rskim/src/cmd/infra/gh/list.rs` | TP-13 | LANDED |
| F2 | BLOCKING | C11 | `rg` rewrite declines when binary unresolvable | `crates/rskim/src/cmd/rewrite/engine.rs`, `runner.rs` | TP-5 | LANDED |
| F12 | MED | C12 | interior-newline rewrite bail emits a signal | `crates/rskim/src/cmd/rewrite/hook.rs` | TP-15 | LANDED |
| F11 | MED | C13 | `--debug`/`--passthrough` honoured after positional | `crates/rskim/src/main.rs` | TP-14 | LANDED |
| F5 | HIGH | C14 | diff line-number axis convention documented | docs only | TP-8 | LANDED |

Rows are in **commit-sequence order**, which is not ID order and not wave order.

---

## C0g — close the `assert_render_fidelity` vacuity hole

- **Defect:** `assert_render_fidelity` is the shared fidelity oracle for the whole
  `git diff` render test file, and every assertion it makes is inside a `for` loop over
  a collection it does not require to be non-empty. Handed a render (or a raw diff)
  that parses to zero emissions, all of its loops iterate zero times and the helper
  returns success. Twelve call sites in twelve distinct test functions depend on it, so
  a change that silently emptied the parse — a render format move, an `ln_width`
  mis-derivation, a fixture that stops producing a diff — would turn the file's entire
  fidelity guarantee green while asserting nothing.
- **Root cause:** `crates/rskim/tests/cli_git_diff_modes.rs:229` (`fn assert_render_fidelity`);
  `emissions` bound at `:247`, consumed by the unguarded `for &(marker, line) in &emissions`
  at `:251`. The helper has a **second** vacuity surface at `:232` (`for content in &model.changed_content`),
  and an early `return` at `:244-246` for the `diff --git` raw-hunk fallback which skips
  the emission checks entirely. `parse_emissions` is at `:196`.
- **Precondition:** none — this is a unit-level property of the helper itself, asserted
  against a deliberately empty input rather than against a repository.
- **Acceptance (argv):** no skim argv — C0g is a property of a test helper, asserted
  against a deliberately empty input rather than against a repository, so the acceptance
  surface is a source audit plus a per-perturbation rebuild. `SKIM_DISABLE_ANALYTICS=1`
  and `--no-cache` do not apply (skim is never invoked).

  ```bash
  cd /Users/dean/Sandbox/skim-issues
  F=crates/rskim/tests/cli_git_diff_modes.rs
  # (a) shape audit — both loops, the early return, and the absent non-empty guard
  /usr/bin/grep -n 'fn parse_emissions\|fn assert_render_fidelity\|for content in &model.changed_content\|let emissions\|for &(marker, line) in &emissions\|is_empty' "$F"
  # (b) call-site census — 13 grep hits, 12 of them call sites, 1 the definition
  /usr/bin/grep -c 'assert_render_fidelity' "$F"
  /usr/bin/grep -n 'assert_render_fidelity(' "$F"
  # (c) runtime falsification — perturb exactly ONE parse_emissions drop path, rebuild,
  #     re-run the twelve call sites, restore, repeat. Eight perturbations:
  #       ln_width+1 · ln_width-1 · marker byte -> '~' · separator -> tab ·
  #       separator -> pipe · numbers blanked · header-only render · empty stdout
  cargo build -p rskim && cargo nextest run -p rskim --test cli_git_diff_modes -j 4
  ```
- **BEFORE (measured at c2b4378):** **DEFECT REPRODUCED, and worse than the plan
  stated.** Source: `assert_render_fidelity` at `tests/cli_git_diff_modes.rs:229`; **all
  four** axis assertions sit inside the `for` at `:251` over `emissions` bound at `:247`;
  the second surface is `for content in &model.changed_content` at `:232`; there is **no
  `is_empty` check anywhere in the function**. Call-site census: **12 call sites in 12
  test functions** (the 13th grep hit is the definition itself).
  **Runtime falsification** — perturbing one `parse_emissions` drop path at a time on a
  real 20-emission render yielded **0 emissions** for every one of: `ln_width + 1`,
  `ln_width - 1`, marker byte → `~`, separator → tab, separator → pipe, numbers blanked,
  header-only render, and empty stdout. **In every case all four assertions were skipped
  and the helper returned success.** The oracle is not weak on these inputs; it is silent.
- **AFTER (required):** the helper fails, not passes, when handed a render that parses to
  zero emissions. Add `assert!(!emissions.is_empty(), …)` immediately after `:247`. All
  twelve existing call sites stay green — the amendment must change no test's verdict, only
  close the hole. The `changed_content` surface at `:232` is a second hole of the same
  shape; if it is not closed in this commit, say so in the commit message rather than
  leaving the reader to infer that one `assert!` closed both.
- **Regression test:** `rskim` · `tests/cli_git_diff_modes.rs::assert_render_fidelity_rejects_empty_emissions`
  (new; a negative test that hands the helper an empty render and expects a panic, via
  `std::panic::catch_unwind`) · `cargo build -p rskim && cargo nextest run -p rskim --test cli_git_diff_modes -j 4`
- **Re-bless:** none
- **Status:** LANDED

---

## F1 — pseudo mode preserves type-level declarations (TypeScript)

- **Defect:** `--mode=pseudo` strips the `type_annotation` of a TypeScript
  `property_signature`, so an interface member is rendered as a bare name. The
  annotation is the *entire content* of such a declaration: `interface Config { name: string; value: number }`
  renders as `interface Config { name; value }`, which does not parse as TypeScript
  (`tsc` reports hard `TS1131`/`TS1128`) and in the union case renders two distinct
  members byte-identically. This is a lossy transform with no class-1 disclosure.
- **Root cause:** `crates/rskim-core/src/transform/pseudo.rs` — `"type_annotation"` is
  listed in the TypeScript `strip_kinds` table at `:259` (table `:255-270`); the strip
  gate is `if rules.strip_kinds.contains(&kind)` at `:830`; the two existing exemptions
  are the return-type guard at `:839` (`pos.is_return_type_field`) and the E1 parameter
  guard at `:847-856`; the unexempted path pushes the strip range at `:858-871`. The new
  guard goes beside the E1 guard at `:847`.
- **Precondition:** stderr **must contain** `[skim] pseudo view:` **and must not contain**
  `[skim:guardrail]`. This is mandatory and is asserted *before* any assertion about
  stdout. Under an ADR-001 raw passthrough, stdout **is** the source file, and the source
  file already contains `name: string;` — so a naive "stdout contains `name: string`"
  assertion passes green against a completely unfixed binary. The guardrail is
  content-sensitive rather than size-sensitive and **no TypeScript or Python file in this
  repository clears it under pseudo**, so the fixture must be purpose-built and sized
  empirically until the guardrail banner disappears and the pseudo-view marker appears.
  Target at least 2x the minimum clearing margin so a later ADR-001 tuning does not
  silently re-mask the test. **Measured consequence:** the fixture grew from **1026 B**
  (md5 `9d6bf2d8…`) to **1104 B** (md5 `8d262c37…`) to restore the ≥2× L2 margin,
  because the fix restores 75 B of annotation text and therefore spends its own headroom —
  the 2× target has to be met by the *post-fix* view, not the pre-fix one. Post-fix L2
  measures **2.20× bytes / 2.17× tokens**. The hook-origin form of the marker is larger than the
  direct-invocation form, so the L2 re-check (`SKIM_REWRITTEN_FROM=cat`) is part of the
  precondition, not a nicety: a fixture that clears the direct marker but not the
  hook-origin one is fixed for a human and still masked for every agent.
- **Acceptance (argv):**

  ```bash
  cd /Users/dean/Sandbox/skim-issues
  B="$PWD/target/skim-baseline-c2b4378"; T="$(/usr/bin/mktemp -d)"
  NPX=/Users/dean/.nvm/versions/node/v22.22.3/bin/npx   # tsc is NOT on $PATH on this host
  F=tests/fixtures/typescript/type_level_members.ts
  # fixture identity
  /usr/bin/wc -c "$F"; /sbin/md5 -q "$F"
  # raw control — real tool, absolute path, per-command SKIM_PASSTHROUGH (never exported, PF-026)
  SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 /bin/cat "$F" > "$T/raw.ts"
  # L1 — direct invocation
  SKIM_DISABLE_ANALYTICS=1 "$B" "$F" --mode=pseudo --no-cache > "$T/l1.ts" 2> "$T/l1.err"
  # L2 — hook-origin marker form (the larger marker; L1 clearing does not imply L2 clearing)
  SKIM_DISABLE_ANALYTICS=1 SKIM_REWRITTEN_FROM=cat "$B" "$F" --mode=pseudo --no-cache \
    > "$T/l2.ts" 2> "$T/l2.err"
  # PRECONDITION, ASSERTED FIRST, AT BOTH LEVELS
  for e in "$T/l1.err" "$T/l2.err"; do
    /usr/bin/grep -c '\[skim\] pseudo view:' "$e"   # must be 1
    /usr/bin/grep -c '\[skim:guardrail\]'    "$e"   # must be 0
    /usr/bin/wc -c "$e"                             # marker size -> headroom arithmetic
  done
  /usr/bin/cmp "$T/l1.ts" "$T/l2.ts"                # stdout byte-identical across L1/L2
  # served-view loss
  /usr/bin/wc -c "$T/raw.ts" "$T/l1.ts"
  /usr/bin/sed -n '10p' "$T/l1.ts" | /sbin/md5 -q   # both union members must NOT collide
  /usr/bin/sed -n '11p' "$T/l1.ts" | /sbin/md5 -q
  # reparse, raw then served (served view written with a .ts suffix so tsc accepts it)
  "$NPX" --yes typescript tsc --noEmit --skipLibCheck --noImplicitAny "$T/raw.ts"; echo "raw rc=$?"
  "$NPX" --yes typescript tsc --noEmit --skipLibCheck --noImplicitAny "$T/l1.ts"; echo "served rc=$?"
  # guard accounting
  SKIM_DISABLE_ANALYTICS=1 "$B" "$F" --mode=pseudo --no-cache --show-stats 2>&1 | /usr/bin/grep -i token
  ```
- **BEFORE (measured at c2b4378):** **DEFECT REPRODUCED.** Fixture
  `tests/fixtures/typescript/type_level_members.ts`, **1104 B**, md5
  `8d262c37aff6bf49b435a4463f20017f`.
  **Precondition CLEARS at both levels** — L1: raw 1104 / served 671 / marker 129 → clears,
  headroom **304 B**, **3.36×**; L2: stdout byte-identical to L1 (`cmp`), marker 163 →
  clears, headroom **270 B**, **2.66×**. `--show-stats`:
  `259 tokens → 147 tokens (43.2% reduction)`.
  **Served view, measured losses:** `id: UserId;` → `id`, `readonly createdAt: Date;` →
  `createdAt`, `[account: string]: number;` → `[account: string]`, and — the collision that
  makes this more than untidy — **both union members render as `  | { amount currency }`,
  md5 `3bf73ed9461878707298e13d7e75ddf0` for both** (served lines 10 and 11). Two
  distinct declarations become byte-identical.
  **Reparse:** `tsc --noEmit --skipLibCheck --noImplicitAny` on raw → **exit 0**; on the
  served view → **exit 1**, with TS1131 (10:7) + TS1128 (10:23) + TS1109 (11:3) +
  TS1005 (11:14).
- **AFTER (required):** a `property_signature` member's `type_annotation` survives
  intact, so the interface body renders as `name: string` / `value: number` and the
  emitted view parses under `tsc --noEmit`. Scope is `property_signature` **only** —
  class properties stay stripped. `interface_body` is an alias of `object_type` and
  `property_signature` occurs only under those two, while class fields are
  `public_field_definition` under `class_body`; the sets do not intersect, so
  `test_typescript_pseudo_strips_class_property_annotation` (`pseudo.rs:1268`) needs no
  amendment. `index_signature` is to be included defensively **only after** a real parse
  dump confirms the node-kind name. `readonly` is also in the TypeScript `strip_kinds`
  table (`:268`) and `property_signature` admits `optional('readonly')`, so the
  interaction needs its own test.
- **Regression test:** `rskim-core` · `src/transform/pseudo.rs::{test_typescript_pseudo_preserves_interface_member_annotation`
  (`:1579`), `test_typescript_pseudo_preserves_interface_member_readonly` (`:1597`),
  `test_typescript_pseudo_preserves_index_signature_type` (`:1611`),
  `test_typescript_pseudo_preserves_index_signature_readonly` (`:1625`)`}` — the
  `index_signature` arm the AFTER field made conditional on a real parse dump **was**
  confirmed and is covered, and `readonly` has its own test as required ·
  `cargo nextest run -p rskim-core -j 4 -E 'test(/pseudo/)'` — **and**
  `rskim` · `tests/cli_pseudo_type_fidelity.rs::{fixture_sizes_are_load_bearing`
  (`:222`), `direct_pseudo_view_preserves_type_level_declarations` (`:245`),
  `hook_origin_pseudo_view_preserves_type_level_declarations` (`:256`),
  `hook_origin_and_direct_views_are_byte_identical` (`:268`),
  `small_file_stays_masked_at_hook_origin_so_the_fix_is_invisible_there` (`:305`)`}` ·
  `cargo build -p rskim && cargo nextest run -p rskim --test cli_pseudo_type_fidelity -j 4`.
  **Path corrected against the working tree:** the file is
  `crates/rskim/tests/cli_pseudo_type_fidelity.rs`; the plan's
  `tests/cli_pseudo_fidelity.rs` **does not exist**, and neither does the single test name
  it gave (`typescript_interface_member_annotation_reaches_the_reader`).
  These five integration tests are **shared with F1b**, because both defects are only
  jointly observable in one served view — the file asserts the post-fix view as a whole
  rather than one defect at a time, and the per-defect discrimination lives in F1b's
  isolation matrix instead.
  Both layers are required by Gate 3: the `rskim-core` test proves the transform is
  right but cannot prove a user ever sees it, because ADR-001 does not exist in
  `rskim-core` (no dependency on `output/`). Only the integration test can catch a
  re-masking by the guard.
- **Re-bless:** `crates/rskim-core/tests/snapshots/truncation_golden__ts_comments_pseudo_unbounded.snap`
  — **one cell expected to move.** Written expectation, to be checked against reality at
  Gate 6: that snapshot currently renders `export interface Config {` / `    name` /
  `    value` (from `tests/fixtures/typescript/comments.ts:44-46`); it must become
  `    name: string` / `    value: number` once F1b restores the `;`, or
  `    name: string` / `    value: number` without terminators if F1 lands alone.
  **Not expected to move, with reasons:** `ts_comments_pseudo_max5` (its window ends at
  output line 4, well above the interface); all four `ts_simple_pseudo_*` cells
  (`tests/fixtures/typescript/simple.ts` contains no interface or type alias); every
  `go_*`, `python_*`, `md_*` and `rust_*` cell (no TypeScript); every non-pseudo mode cell.
  Line counts do not change — the annotation is restored on the same line — so the
  `(50 lines truncated)` marker in `ts_comments_pseudo_max5` must be byte-identical
  afterwards. Per `truncation_golden.rs:16-21`, "no snapshots moved" is never evidence of
  correctness; enumerate first, classify each move, then `INSTA_UPDATE=always`.
- **Status:** LANDED

---

## F1b — pseudo mode preserves `;` in TypeScript type and interface bodies

- **Defect:** the pseudo `;`-stripping rule is unconditional except for for-loop
  headers, so it also strips the member terminators inside an `object_type` /
  `interface_body`. Combined with F1 the body becomes unparseable; on its own it produces
  `interface Config { name: string value: number }`, which `tsc` rejects.
- **Root cause:** `crates/rskim-core/src/transform/pseudo.rs:895-907` — the semicolon
  guard `if rules.strip_semicolons && kind == ";"` at `:895`, whose only preservation
  conditions are `is_for_loop_direct_child` (`:896-905`) and the threaded `in_for_header`
  (`:906`); everything else is pushed as a strip range at `:907`. TypeScript sets
  `strip_semicolons: true` at `:269`.
- **Precondition:** stderr **must contain** `[skim] pseudo view:` **and must not contain**
  `[skim:guardrail]` — the same vacuity trap as F1, for the same reason: the raw source
  already contains the `;`, so a raw passthrough satisfies a naive stdout assertion
  against an unfixed binary. Assert stderr first. Shares F1's fixture, which grew
  1026 B → **1104 B** to keep the post-fix L2 margin at ≥2× (see F1's precondition);
  post-fix L2 measures **2.20× bytes / 2.17× tokens**.
- **Acceptance (argv):** F1's run, plus the isolation matrix that separates the two
  defects — without it a single `tsc` exit code cannot say which of F1 and F1b caused it.

  ```bash
  # (F1's block first; $T and $NPX carry over)
  # F1 alone: annotation gone, terminators intact
  printf 'interface U { id; email }\n'                > "$T/f1-only.ts"
  # F1b alone: annotations intact, terminators gone
  printf 'interface U { id: string email: string }\n' > "$T/f1b-only.ts"
  "$NPX" --yes typescript tsc --noEmit --skipLibCheck --noImplicitAny "$T/f1-only.ts"
  echo "F1-only rc=$?"
  "$NPX" --yes typescript tsc --noEmit --skipLibCheck --noImplicitAny "$T/f1b-only.ts"
  echo "F1b-only rc=$?"
  # the two `;` losses in the served view of F1's fixture
  /usr/bin/grep -n 'amount\|currency\|^  id\|^  email' "$T/raw.ts" "$T/l1.ts"
  ```
- **BEFORE (measured at c2b4378):** **DEFECT REPRODUCED**, same run as F1.
  **`;` losses:** `{ amount: number; currency: "USD" }` → `{ amount currency }`;
  `id: UserId;` / `email: string;` → `id` / `email`.
  **Isolation matrix — the two defects are separable and only one of them is fatal alone:**
  `{ id; email }` (F1 only, terminators intact) → `tsc` **exit 0, no diagnostics**;
  `{ id: string email: string }` (F1b only, annotations intact) → **exit 1, TS1005**. So
  F1b is the unparseability, F1 is the information loss and the member collision — a
  single `tsc` exit code cannot attribute the failure, which is why the matrix is part of
  the acceptance command and not a nicety.
- **AFTER (required):** a `;` whose parent is `object_type` or `interface_body` is
  preserved; every other `;` keeps today's behaviour. Class-body `;` (under
  `public_field_definition` / `class_body`) is deliberately **not** admitted, matching
  F1's scope decision — so `private value` in the golden TypeScript fixture must still
  render without its terminator after this commit.
- **Regression test:** `rskim-core` · `src/transform/pseudo.rs::test_typescript_pseudo_preserves_interface_body_semicolon`
  (`:1709`) · `cargo nextest run -p rskim-core -j 4 -E 'test(/pseudo/)'` — **and** the
  five shared integration tests in `rskim` ·
  `tests/cli_pseudo_type_fidelity.rs` (enumerated under F1) ·
  `cargo build -p rskim && cargo nextest run -p rskim --test cli_pseudo_type_fidelity -j 4`.
  **Path corrected against the working tree:** the plan's `tests/cli_pseudo_fidelity.rs`
  and its test name `typescript_interface_body_semicolon_reaches_the_reader` **do not
  exist**; the integration layer landed as one shared file, for the reason given in F1.
- **Re-bless:** `crates/rskim-core/tests/snapshots/truncation_golden__ts_comments_pseudo_unbounded.snap`
  — **one cell expected to move** (the same cell as F1; if F1 lands first, this commit's
  diff on that file is the two `;` characters only). Written expectation for Gate 6:
  this change reaches only languages with `strip_semicolons: true`, which in the golden
  matrix is **TypeScript and Rust only** — Python, Go and Markdown are all
  `strip_semicolons: false` (`pseudo.rs:287`, `:306`, `:408`) and their fixtures contain
  no `;` at all. **Not expected to move, with reasons:** `ts_comments_pseudo_max5`
  (window ends above the interface); all four `ts_simple_pseudo_*` cells (their only
  semicolons terminate an `expression_statement` and a `lexical_declaration`, neither
  admitted); all `rust_*` cells (no `object_type`/`interface_body` in Rust); every
  `python_*`, `go_*`, `md_*` cell.
- **Status:** LANDED

---

## F1c — pseudo mode preserves the declaration-terminating `;` (Rust)

- **Defect:** the same unconditional `;` strip removes the terminator from a Rust trait
  method signature, so `fn compute(&self, x: i32) -> i32;` renders as
  `fn compute(&self, x: i32) -> i32`, which inside a `trait` block reads as a method
  *with* a body whose opening brace is missing — the declaration's apparent kind has
  changed. The state is already blessed into a golden snapshot, so the bug is pinned as
  correct behaviour today.
- **Root cause:** `crates/rskim-core/src/transform/pseudo.rs:895-907` — the same
  semicolon guard as F1b. Rust sets `strip_semicolons: true` at `:299` and
  `strip_kinds: &[]` at `:298`, so the `;` strip is the *only* Rust noise rule in pseudo
  mode and this commit adds a third preservation condition rather than editing a table.
  The blessed buggy snapshot line is
  `crates/rskim-core/tests/snapshots/truncation_golden__rust_simple_pseudo_unbounded.snap:31`
  (**corrected from the plan's `:30`**), with the same line mirrored at
  `truncation_golden__rust_simple_pseudo_last10.snap:7`.
- **Precondition:** none — CLI observation is structurally unreachable (token-neutral
  transform). Stated as a stderr precondition in the plan by analogy with F1/F1b, and the
  analogy does not hold: removing 205 semicolons is a **0.0% token change**, so ADR-001
  never elects `Keep` and the pseudo-view marker cannot be made to appear at any fixture
  size (see BEFORE). There is no precondition to satisfy because there is no served view
  to condition on.
- **Acceptance (argv):** there is no skim argv for the served view — the transform is
  token-neutral, so ADR-001 elects `Passthrough` at every size and no CLI invocation can
  exhibit it (see BEFORE). The acceptance surface is therefore the committed golden
  snapshot body, which pins the buggy render, plus the masking demonstration that proves
  the CLI arm is unreachable rather than merely untried.

  ```bash
  cd /Users/dean/Sandbox/skim-issues
  B="$PWD/target/skim-baseline-c2b4378"; T="$(/usr/bin/mktemp -d)"
  RUSTC=/Users/dean/.cargo/bin/rustc
  S=crates/rskim-core/tests/snapshots/truncation_golden__rust_simple_pseudo_unbounded.snap
  # drop insta's YAML front matter (lines 1..the second `---`), keep the rendered body
  /usr/bin/sed '1,/^---$/d' "$S" > "$T/served.rs"
  # `--crate-type lib` is REQUIRED for this check to mean anything: without it rustc
  # defaults to a bin crate and BOTH arms fail E0601 (`main` function not found), which
  # is crate-shape noise, not a parse verdict. Verified: bare
  # `rustc --edition 2021 --emit=metadata tests/fixtures/rust/simple.rs` exits non-zero
  # on the RAW control too, so the BEFORE's "raw exit 0" is only reproducible with this flag.
  "$RUSTC" --edition 2021 --crate-type lib --emit=metadata -o "$T/served.meta" "$T/served.rs"
  echo "served rc=$?"
  # RAW control — the same fixture, untransformed
  "$RUSTC" --edition 2021 --crate-type lib --emit=metadata -o "$T/raw.meta" \
    tests/fixtures/rust/simple.rs
  echo "raw rc=$?"
  # unreachability: five escalating fixtures, L1 and L2, must ALL stay masked
  for f in "$T"/esc-716.rs "$T"/esc-2235.rs "$T"/esc-2955.rs "$T"/esc-3495.rs "$T"/esc-4035.rs; do
    /usr/bin/wc -c "$f"
    SKIM_DISABLE_ANALYTICS=1                         "$B" "$f" --mode=pseudo --no-cache \
      2>&1 >/dev/null | /usr/bin/grep -c '\[skim:guardrail\]'
    SKIM_DISABLE_ANALYTICS=1 SKIM_REWRITTEN_FROM=cat "$B" "$f" --mode=pseudo --no-cache \
      2>&1 >/dev/null | /usr/bin/grep -c '\[skim:guardrail\]'
  done
  # token neutrality, the reason the guard can never elect Keep
  SKIM_DISABLE_ANALYTICS=1 "$B" "$T/esc-4035.rs" --mode=pseudo --no-cache --show-stats 2>&1 \
    | /usr/bin/grep -i token
  /usr/bin/grep -o ';' "$T/esc-4035.rs" | /usr/bin/wc -l    # semicolons the transform removes
  ```
- **BEFORE (measured at c2b4378):** **DEFECT REPRODUCED at the snapshot, and
  STRUCTURALLY UNREACHABLE at the CLI.** `rustc --edition 2021 --emit=metadata` on the
  committed golden snapshot body: **exit 1**, `error: expected ';', found '}'` at
  `27:37`, with `help: add ';' here`. RAW control (`tests/fixtures/rust/simple.rs`):
  **exit 0**. The buggy render is blessed, so the defect is currently pinned as correct
  behaviour.
  **Token neutrality — the reason no CLI arm exists:** five escalating fixtures
  (716 / 2235 / 2955 / 3495 / 4035 B) are **all MASKED at L1 and at L2**; removing **205
  semicolons** moved **1,129 → 1,129 tokens (0.0%)**. ADR-001's `Keep` branch is
  unreachable for any Rust input at any size, because the transform costs nothing and
  therefore saves nothing. A bigger fixture does not help; there is no size at which the
  guard flips.
- **AFTER (required):** the criterion is *strip the `;` where its absence is merely
  untidy; keep it where its absence changes what the declaration appears to be.* Admits
  `function_signature_item`, `struct_item`, `associated_type`. Excludes
  `use_declaration`, `let_declaration`, `expression_statement`, `const_item`,
  `static_item`. Non-goal, to be stated explicitly in the commit message: Rust pseudo
  output remains non-reparseable via `use` and `let` — that is pre-existing and tracked
  separately, and this commit must not be read as claiming otherwise.
  `test_rust_pseudo_trait_preserves_return_type` (`pseudo.rs:1858`) asserts only
  `-> i32` and `fn compute`, never that the `;` is absent, so it needs no assertion
  change — **but its doc comment at `:1859-1860` states "the trailing `;` on trait method
  signatures is still stripped", which this commit makes false.** Correct that comment in
  this commit.
- **Regression test:** `rskim-core` · `src/transform/pseudo.rs::{test_rust_pseudo_preserves_trait_method_signature_terminator`
  (`:2027`), `test_rust_pseudo_preserves_struct_item_terminator` (`:2044`),
  `test_rust_pseudo_preserves_associated_type_terminator` (`:2065`)`}` — **three tests,
  one per admitted node kind** (`function_signature_item`, `struct_item`,
  `associated_type`), which is the AFTER field's criterion covered exhaustively rather
  than by its leading case. **Corrected against the working tree:** the plan's single
  `test_rust_pseudo_preserves_trait_signature_semicolon` **does not exist** ·
  `cargo nextest run -p rskim-core -j 4 -E 'test(/pseudo/)'`, plus the snapshot
  re-bless below. **No integration file is attached to F1c** — see the ruling below. **No `rskim` integration test: Gate 3 was relaxed by orchestrator
  ruling for F1c specifically**, because ADR-001's `Keep` branch is unreachable for any
  Rust input at any size, so an integration test could only assert that the guard serves
  raw — which is true before and after the fix and therefore proves nothing. Gate 3's
  requirement stands unchanged for F1 and F1b, where the `Keep` branch *is* reachable and
  the integration layer is the only thing that can catch a re-masking.
- **Re-bless:** **two cells expected to move**, written expectation for Gate 6:
  - `crates/rskim-core/tests/snapshots/truncation_golden__rust_simple_pseudo_unbounded.snap`
    (line `:31`, `    fn compute(&self, x: i32) -> i32` gains its `;`)
  - `crates/rskim-core/tests/snapshots/truncation_golden__rust_simple_pseudo_last10.snap`
    (line `:7`, the same source line inside the tail window)

  **Not expected to move, with reasons:** `rust_simple_pseudo_max15` and
  `rust_simple_pseudo_max5` — `tests/fixtures/rust/simple.rs:27` is the only
  `function_signature_item` in the fixture and both windows close above it (`max15`'s last
  content line is `    value: i32,` and `max5`'s is `pub fn add(...)`); both
  `rust_comments_pseudo_*` cells — the only two semicolons in
  `tests/fixtures/rust/comments.rs` are a `let_declaration` at `:14` and a `const_item`
  at `:51`, both explicitly excluded by the criterion; every `ts_*`, `python_*`, `go_*`
  and `md_*` cell; every non-pseudo mode cell. Line counts do not change, so the
  `(20 lines truncated)`, `(30 lines truncated)` and `(25 lines above)` markers must be
  byte-identical afterwards.

  **Scope of the surrounding matrix, for the record, because the plan's count is loose:**
  there are 76 golden cells, of which **28** are pseudo-mode; the 9 `*_pseudo_unbounded`
  cells do **not** each have three bounded variants — the five `*_simple_pseudo_*`
  families have three (`max15`, `max5`, `last10`) and the four `*_comments_pseudo_*`
  families have only `max5`. Of those 28 pseudo cells only **12** belong to a
  `strip_semicolons: true` language (6 Rust, 6 TypeScript), which bounds the whole
  F1/F1b/F1c blast radius. The expected moved set across all three commits is **3
  distinct files**.
- **Status:** LANDED

---

## G — git contract-flag passthrough gate

- **Defect:** skim has no notion of "this output format is a machine contract." Every
  `git` case that passes through byte-identically today does so by *coincidence* of
  **three** blunt mechanisms, not two — the ADR-001 net-savings byte comparison,
  "non-zero exit implies forward raw," and, third, the **empty-parse early return**:
  `skim git diff --raw` is byte-identical **not** because of the guard but because
  `parse_unified_diff` yields no files and `run_diff` writes `raw_diff` before
  `savings_decision` is ever consulted. That third mechanism is *branch-lucky, which is
  narrower than guard-lucky* — it survives a change in byte counts but not a change that
  makes the parser produce one file, so counting it as guard-luck understates how easily
  it breaks. Bare `--porcelain` survives only because compression happens to be
  larger than the raw bytes on the measured input. The passing cases are not correct,
  they are lucky, and any change to the renderer's byte count silently converts one of
  them into a corrupted machine contract. This is the root cause of F6, F9 and F13.
- **Root cause:** `crates/rskim/src/cmd/git/mod.rs` — the subcommand dispatch
  `match subcmd.as_str()` at `:71` with its arms at `:72-78` routes every recognised
  subcommand straight into a parsing handler with no contract-flag check anywhere above
  it; `subcmd_args` is bound at `:63`, which is the insertion point. The unknown-subcommand
  arm already does the right thing at `:91` (`super::run_raw_passthrough`), and
  `run_passthrough` is at `:313`. The only per-subcommand precedent is ad hoc and
  incomplete: `log.rs:31` gates `--format`/`--pretty`, `fetch.rs:28` gates
  `--dry-run`/`-q`/`--quiet`, `push.rs:73` gates `--porcelain`/`--no-porcelain`/`--quiet`/`-q`
  — and `cmd/git/diff/` gates nothing at all. `user_has_flag` is
  `crates/rskim/src/cmd/mod.rs:352`.
- **Precondition:** the working tree must contain at least one modified, staged or
  untracked path, so that `git status --porcelain` produces **non-empty** output. On a
  clean tree both the gated and the ungated binary emit zero bytes and a byte-equality
  assertion passes green against an unfixed binary. State the dirty-tree setup inside the
  hermetic fixture repository rather than relying on the ambient checkout.
- **Acceptance (argv):**

  ```bash
  cd /Users/dean/Sandbox/skim-issues
  B="$PWD/target/skim-baseline-c2b4378"; T="$(/usr/bin/mktemp -d)"; GIT=/usr/bin/git
  C="-c user.email=t@t -c user.name=t -c commit.gpgsign=false"
  # ---- hermetic fixture: 3 commits with NON-EMPTY diffstats (F6), a dirty tree (G),
  #      and an upstream diverged +2 -1 (F9). No ambient checkout is consulted.
  "$GIT" init -q --bare "$T/up.git"
  "$GIT" init -q "$T/repo"; cd "$T/repo"
  for n in 1 2 3; do printf 'fn f%s() { let x = %s; }\n' "$n" "$n" > "f$n.rs"
    "$GIT" add "f$n.rs"; "$GIT" $C commit -q -m "c$n"; done
  printf 'fn clean() {}\n' > clean.rs; "$GIT" add clean.rs; "$GIT" $C commit -q -m clean
  BASE=$("$GIT" rev-parse HEAD)
  "$GIT" remote add origin "$T/up.git"
  "$GIT" $C commit -q --allow-empty -m remote-only
  "$GIT" push -q origin HEAD:refs/heads/main
  "$GIT" update-ref refs/remotes/origin/main "$("$GIT" rev-parse HEAD)"
  "$GIT" update-ref refs/heads/main "$BASE"          # rewind local WITHOUT reset
  "$GIT" symbolic-ref HEAD refs/heads/main
  "$GIT" $C commit -q --allow-empty -m ahead1
  "$GIT" $C commit -q --allow-empty -m ahead2
  "$GIT" branch --set-upstream-to=origin/main main   # -> `# branch.ab +2 -1`
  printf 'dirty\n' > dirty.txt                       # PRECONDITION: non-empty --porcelain
  # ---- 20 cases, stdout bytes raw vs served. `--no-cache` is NOT passed: the git path
  #      forwards it to real git, which rejects it (`error: unknown option 'no-cache'`).
  set -- "status --porcelain=v2 --branch" "status --porcelain=v2" "status --porcelain -z" \
         "status -sz" "status --porcelain" "status --porcelain=v1" \
         "log --stat -n 3" "log --shortstat -n 3" "log --numstat -n 3" \
         "log --name-only -n 3" "log --name-status -n 3" "log --graph -n 3" \
         "diff --quiet -- clean.rs"
  for c in "$@"; do
    SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 "$GIT" $c > "$T/raw"  2>"$T/raw.err"; rrc=$?
    SKIM_DISABLE_ANALYTICS=1                    "$B" git $c > "$T/skim" 2>"$T/skim.err"; src=$?
    printf '%-34s raw=%-6s skim=%-6s skim_err=%-4s rc=%s/%s\n' "$c" \
      "$(/usr/bin/wc -c <"$T/raw")" "$(/usr/bin/wc -c <"$T/skim")" \
      "$(/usr/bin/wc -c <"$T/skim.err")" "$rrc" "$src"
  done
  # PF-026: SKIM_PASSTHROUGH=1 is prefixed to the RAW-CONTROL command only, never
  # exported. Exported, all 20 cases read byte-identical and the BEFORE is uniformly
  # clean — which is the tell that the harness, not the binary, produced the result.
  # ---- byte-identity of the five stat-family renders, and the --graph bytes verbatim
  for c in --stat --shortstat --numstat --name-only --name-status; do
    SKIM_DISABLE_ANALYTICS=1 "$B" git log $c -n 3 | /sbin/md5 -q; done
  SKIM_DISABLE_ANALYTICS=1 "$B" git log --graph -n 3 | /usr/bin/xxd
  # ---- real-repo cross-check
  cd /Users/dean/Sandbox/skim-issues
  SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 /usr/bin/git log --stat -n 3 | /usr/bin/wc -c
  SKIM_DISABLE_ANALYTICS=1 "$B" git log --stat -n 3 > "$T/rr" 2>"$T/rr.err"
  /usr/bin/wc -c "$T/rr" "$T/rr.err"
  ```
- **BEFORE (measured at c2b4378):** **DEFECT REPRODUCED on 18 of 20 cases.** Hermetic
  3-commit fixture, stdout bytes raw vs baseline, with **stderr 0 B and exit 0 on every
  loss case** — silent, undisclosed, and indistinguishable from success:

  | argv | raw | served |
  |---|---|---|
  | `status --porcelain=v2 --branch` | 215 | 97 |
  | `status --porcelain=v2` | 142 | 97 |
  | `status --porcelain -z` | 33 | **34** |
  | `status -sz` | 33 | **34** |
  | `log --stat -n 3` | 1738 | 324 |
  | `log --shortstat -n 3` | 730 | 324 |
  | `log --numstat -n 3` | 1039 | 324 |
  | `log --name-only -n 3` | 903 | 324 |
  | `log --name-status -n 3` | 951 | 324 |
  | `log --graph -n 3` | 622 | **15** (`log no commits\n`) |

  All five stat-family flags served a **byte-identical 324 B** — the same commit list
  regardless of which stat the reader asked for, which is the signature of a filter
  rather than a renderer. `diff --quiet -- clean.rs`: rc 0, stdout 0, **stderr 11 B
  `No changes\n`** (a non-empty stderr where git writes none).
  **VACUOUS (guard-lucky, measures nothing):** bare `--porcelain` and `--porcelain=v1`,
  both 33 → 33.
  **Real-repo cross-check:** `log --stat -n 3` raw **32733** → skim **374**, stderr 0.
  **PF-026 note:** a first run exported `SKIM_PASSTHROUGH=1` and reported all 20 cases
  byte-identical. **A uniformly clean BEFORE is the tell** — Gate 0's own rule fired
  correctly and caught the harness, not the binary.
- **AFTER (required):** `skim git <sub> <contract-flag> …` serves the real `git` bytes
  verbatim on stdout and forwards `git`'s exit code. **The gate is deliberately not
  separator-aware:** use `user_has_flag` over the **full** argument list, not
  `args_before_separator`. The error modes are asymmetric — a false positive serves raw
  git bytes, which is byte-faithful and guard-neutral because `parse_tier == "passthrough"`
  skips the ADR-001 guard entirely, whereas a false negative corrupts a machine contract,
  which is the entire defect class. Routing through `args_before_separator` can only
  convert a true into a false, i.e. can only manufacture false negatives. Document that
  asymmetry in the constant's doc comment, and document the known gap rather than fixing
  it: bundled shorts (`git status -sz`) will not match `-z`. **G is non-droppable** — F6,
  F9 and F13 drop with it as a unit.

  **`--json` regression addendum (found after landing, fixed in the same commit; the
  remedy's scope was narrowed after review, because the first wording of this addendum
  overclaimed it).** G's gate fires **ahead of every handler**, while `--json` is
  extracted **inside** handlers by `extract_output_format`, and `run_passthrough`
  forwarded the caller's argv to git unfiltered — so a skim-only flag reached git and git
  rejected the whole invocation.

  **Newly broken by the gate — two spellings, both measured:**
  `status --porcelain --json` went from exit 0 / **2500 B** at `c2b4378` to
  `error: unknown option 'json'`, exit 1; `log --stat --json` went from exit 0 /
  **359 B** to the same error. **This WIDENED a pre-existing defect rather than
  introducing one** — that framing stands: `diff --stat --json`,
  `log --format=%H --json`, `show --raw --json` and `fetch --dry-run --json` were
  **already** exit 1 at the base SHA, because the per-command spellings that gated ahead
  of `extract_output_format` did so there too. Keep those two sets distinct when reading
  what follows; they are both four-ish and they are not the same four.

  **The remedy is narrower than the defect class, and the two must not be conflated.**
  `--json` **disarms the ADR-022 gate** — `caller_requested_json` (`cmd/git/mod.rs:295`),
  consulted in the dispatch guard at `:94` — which is where the regression was introduced
  and where it is fixed. `--mode` keeps the gate armed but is dropped from the forwarded
  argv by `strip_git_view_flags` (`:352`). That filter is placed **at the sink**
  (`run_passthrough`, `:632`, consulting it at `:647`) rather than at the gate, so all
  **six** `run_passthrough` call sites are covered structurally by one edit rather than
  six: the ADR-022 gate (`mod.rs:96`) plus the **five per-command gates** that also reach
  the sink — `show.rs:344` (`PASSTHROUGH_FLAGS`), `show.rs:348` (`ShowMode::MultiRef`),
  `fetch.rs:29` (`--dry-run`/`-q`/`--quiet`), and the two `--help` gates at
  `commit.rs:54` and `push.rs:65`.

  **A side effect of that placement is not a fix for those five.** None of them disarms
  on `--json`. What the sink-level filter changed for them is the *failure mode*: the
  pre-existing `show`/`fetch` spellings go from `exit 1` to a **lossless raw serve**.
  That is an improvement, not a fix — those callers asked for JSON and receive git's
  bytes instead. **Explicitly: `--json` on those spellings still does not produce JSON.**
  Extending the disarm into `show.rs` and `fetch.rs` is **out of this batch's scope** and
  needs a follow-up. `strip_git_view_flags`' own doc comment records this rather than
  leaving a reader to infer it:

  > `--json` reaches here only from the per-command gates this module does not own —
  > `show.rs`'s `PASSTHROUGH_FLAGS` and `ShowMode::MultiRef`, and `fetch.rs`'s
  > `--dry-run`/`--quiet` — where it was already a hard error before the shared gate
  > existed (measured at `c2b4378`: `skim git show --raw --json HEAD` →
  > `fatal: unrecognized argument: --json`, exit 1). Stripping upgrades those to a
  > lossless raw serve. The complete fix is to disarm *those* gates on `--json` the way
  > this one now is, which is outside this change's file scope.

  Of the four base-SHA exit-1 spellings, only two are `show`/`fetch` and therefore only
  two land in that raw-serve bucket. The other two had no gate left standing against
  them: `--format`/`--pretty` were **hoisted out of `log.rs`** into
  `MACHINE_CONTRACT_FLAGS` (`:214-216`) and `cmd/git/diff/` never carried a local gate at
  all (no `user_has_flag` and no `run_passthrough` call site in that module), so
  `log --format=%H --json` and `diff --stat --json` now route through the ADR-022 gate,
  which disarms. **Source-read, NOT measured:** that says no gate refuses them any more,
  not that the handler emits a well-formed envelope for a stat payload. Measure both
  before claiming either is fixed.

  **`README.md:98` — "All subcommands support `--json` for machine-readable output" — is
  therefore still not unconditionally true, and was deliberately left unedited.** Both
  available edits are wrong. Weakening it would launder a pre-existing gap the way the
  docs agent declined to launder this regression; strengthening it would be false. The
  line becomes true when the follow-up lands, not when this batch does.

  The bound flags (`--max-lines` / `--tokens` / `--last-lines`) are deliberately **not**
  stripped: no git handler implements them, so dropping them would turn a hard error into
  a **silently unbounded serve**, which is the exact failure ADR-016 exists to prevent.
- **Regression test:** `rskim` · `tests/cli_git_contract_flags.rs::porcelain_output_is_byte_identical_to_real_git`
  (new; a table-driven test over the gated flag set, each case comparing `skim git …`
  stdout against the real `git …` stdout byte for byte in a hermetic fixture repo) ·
  `cargo build -p rskim && cargo nextest run -p rskim --test cli_git_contract_flags -j 4`,
  plus `rskim` · `src/cmd/git/mod.rs::test_contract_flag_gate_*` unit tests for the
  predicate itself · `cargo nextest run -p rskim --bins -j 4 -E 'test(/contract_flag/)'`
- **Re-bless:** none. `cmd/mod\.rs$` in `ci.yml`'s `SEARCH_PATHS` is `$`-anchored, so
  `cmd/git/mod.rs` does not match it and this commit is scoreboard-free.
- **Status:** LANDED

---

## F6 — `git log --stat` no longer dropped

- **Defect:** `skim git log --stat` silently discards the diffstat block. skim injects
  its own `--format` and then filters the child's output down to lines matching the
  `%h`-format commit-header shape, so every stat line — which is not hex-prefixed — is
  dropped on the floor with no marker and no banner. The reader asked for the stat and
  received a commit list.
- **Root cause:** `crates/rskim/src/cmd/git/log.rs` — the existing contract-flag list at
  `:31` covers only `--format` and `--pretty`, so `--stat` never reaches it;
  `injected_log_format` at `:261-267` unconditionally injects a format; and
  `is_commit_line` at `:277-281` filters to hex-prefixed lines, which is what actually
  discards the stat block. The fix lands in the shared gate in
  `crates/rskim/src/cmd/git/mod.rs` (see **G**), not as a fourth entry in `log.rs:31`.
- **Precondition:** the commit range under test must contain at least one commit with a
  **non-empty diffstat**. An empty-diffstat range produces no stat lines for the filter
  to drop and the check is vacuous.
- **Acceptance (argv):** reuses **G**'s hermetic fixture (`$T/repo`); the precondition it
  adds is that the range carry a non-empty diffstat, which G's three content commits supply.

  ```bash
  cd "$T/repo"; B=/Users/dean/Sandbox/skim-issues/target/skim-baseline-c2b4378
  for c in --stat --shortstat --numstat --name-only --name-status --graph; do
    SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 /usr/bin/git log $c -n 3 > "$T/raw"  2>/dev/null
    SKIM_DISABLE_ANALYTICS=1                    "$B" git log $c -n 3 > "$T/skim" 2>"$T/skim.err"
    printf 'log %-14s raw=%-6s skim=%-5s err=%s\n' "$c" \
      "$(/usr/bin/wc -c <"$T/raw")" "$(/usr/bin/wc -c <"$T/skim")" \
      "$(/usr/bin/wc -c <"$T/skim.err")"
  done
  # the --graph served bytes, in full — not a prefix
  SKIM_DISABLE_ANALYTICS=1 "$B" git log --graph -n 3 | /usr/bin/xxd
  # real-repo cross-check, same six flags
  cd /Users/dean/Sandbox/skim-issues
  for c in --stat --shortstat --numstat --name-only --name-status --graph; do
    SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 /usr/bin/git log $c -n 3 > "$T/raw" 2>/dev/null
    SKIM_DISABLE_ANALYTICS=1 "$B" git log $c -n 3 > "$T/skim" 2>"$T/skim.err"
    printf 'log %-14s raw=%-6s skim=%-5s err=%s\n' "$c" \
      "$(/usr/bin/wc -c <"$T/raw")" "$(/usr/bin/wc -c <"$T/skim")" \
      "$(/usr/bin/wc -c <"$T/skim.err")"
  done
  ```
- **BEFORE (measured at c2b4378):** **DEFECT REPRODUCED.** The hermetic log rows are
  G's (see the table there): all five stat-family flags collapse to a byte-identical
  324 B and `--graph` to 15 B.
  **Real repo, `-n 3`:** `--stat` 32733 → **374**, `--shortstat` 15847 → **374**,
  `--numstat` 28852 → **374**, `--name-only` 27514 → **374**, `--name-status` 28050 →
  **374**, `--graph` 16285 → **15**; **stderr 0 B throughout**. The convergence on one
  number across five different requests is the finding: the diffstat is not compressed,
  it is filtered out.
  `--graph` stdout was the **whole** of `log no commits\n` —
  `6c6f 6720 6e6f 2063 6f6d 6d69 7473 0a` — i.e. skim reported no commits for a range
  that has three.
- **AFTER (required):** `--stat` is in the gate's contract-flag set, so `skim git log --stat`
  reaches `run_raw_passthrough` and the diffstat arrives byte-identical to real `git`. No
  change to `log.rs:31`, `injected_log_format` or `is_commit_line`: the gate short-circuits
  above all three.
- **Regression test:** `rskim` · `tests/cli_git_contract_flags.rs::log_stat_reaches_the_reader`
  (new; a case in G's table-driven test) · `cargo build -p rskim && cargo nextest run -p rskim --test cli_git_contract_flags -j 4`
- **Re-bless:** none
- **Status:** LANDED

---

## F9 — `git status --porcelain=v2` is served verbatim (premise corrected)

- **Defect:** **PREMISE CORRECTED AT GATE 0 — the reported defect does not exist.** The
  claim was that `skim git status --porcelain=v2 --branch` loses git's `# branch.ab +N -M`
  ahead/behind header. It does not: raw `# branch.ab +2 -1` renders as
  `[ahead 2, behind 1]`, so the counts **survive, correctly and in order**.

  The real loss on this argv is twofold. **(a)** `# branch.oid` is dropped outright — no
  prefix match for it exists anywhere in `status.rs`. **(b)** The **wholesale format
  substitution**: the caller asked for porcelain v2, a machine contract, and received
  skim's prose summary (215 B → 97 B). (b) is the defect that matters and it is exactly
  the class **G** addresses, which is why F9 lands as subsumed-by-G with no code change of
  its own. What Gate 0 caught here is not a mis-scoped fix but a **mis-stated reason** for
  a correct one — recorded rather than quietly retained, because a ledger that keeps a
  falsified premise cannot be used to check the next one.
- **Root cause:** `crates/rskim/src/cmd/git/status.rs` — `strip_conflicting_flags` at
  `:32` drops `--short`/`--porcelain`/`--porcelain=*`/`--null`/`--long` entirely;
  `run_status` at `:94` re-injects `--porcelain=v2` at `:123`; and the `raw_override`
  capture at `:154` (`let user_raw_override: Option<String> = runner…`, consumed at
  `:171`) spawns a **second** `git` subprocess purely to have the user's own format
  available as a fallback. No code change is needed in this file — with `--porcelain` in
  the gate, `git status --porcelain=v2 --branch` never reaches the parser at all.
- **Precondition:** HEAD must have an upstream configured in the fixture repository.
  Without an upstream git emits no `# branch.ab` line whatsoever, and an assertion that
  the line is present fails for the wrong reason while an assertion that it is *preserved*
  passes vacuously.
- **Acceptance (argv):** reuses **G**'s hermetic fixture (`$T/repo`), whose upstream is
  deliberately diverged `+2 -1` — without an upstream git emits no `# branch.ab` line at all
  and every assertion about it is vacuous.

  ```bash
  cd "$T/repo"; B=/Users/dean/Sandbox/skim-issues/target/skim-baseline-c2b4378
  # NON-VACUOUS case
  SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 /usr/bin/git status --porcelain=v2 --branch > "$T/raw"
  SKIM_DISABLE_ANALYTICS=1 "$B" git status --porcelain=v2 --branch > "$T/skim" 2>"$T/skim.err"
  /usr/bin/wc -c "$T/raw" "$T/skim"
  /usr/bin/grep -n '^# branch\.' "$T/raw"     # every v2 header line git actually emitted
  /bin/cat "$T/skim"                          # what reached the reader instead
  # VACUITY control — bare --porcelain is guard-lucky and measures nothing
  SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 /usr/bin/git status --porcelain | /usr/bin/wc -c
  SKIM_DISABLE_ANALYTICS=1 "$B" git status --porcelain | /usr/bin/wc -c
  # PREMISE CHECK — which v2 header prefixes have a renderer at all
  cd /Users/dean/Sandbox/skim-issues
  /usr/bin/grep -n 'branch\.ab\|branch\.oid\|branch\.head\|branch\.upstream\|ahead\|behind' \
    crates/rskim/src/cmd/git/status.rs
  # real-repo cross-check
  SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 /usr/bin/git status --porcelain=v2 --branch | /usr/bin/wc -c
  SKIM_DISABLE_ANALYTICS=1 "$B" git status --porcelain=v2 --branch
  ```
- **BEFORE (measured at c2b4378):** **PREMISE CORRECTED — the reported defect does not
  exist.** Raw `# branch.ab +2 -1` renders as `[ahead 2, behind 1]`: the counts
  **survive, correctly and in order**. The real losses are (a) `# branch.oid`, for which
  **no prefix match exists anywhere in `status.rs`**, and (b) the wholesale format
  substitution — the caller asked for porcelain v2 and received skim's prose summary.
  **Hermetic:** `--porcelain=v2 --branch` 215 → 97, **NON-VACUOUS**; bare `--porcelain`
  33 → 33, **VACUOUS**.
  **Real repo:** raw 136 → skim 92, and skim rendered
  `branch: main...throwaway/main [ahead 2, behind 1]`.
  Gate 0 did its job here: the fix is subsumed by G either way, but the ledger's stated
  reason for it was wrong and is corrected above rather than quietly retained.
- **AFTER (required):** git's bytes serve verbatim with `# branch.ab` intact. Have the
  gate short-circuit **before** the `raw_override` capture at `status.rs:154`, which
  removes a second `git` subprocess on every gated call — the capture exists only to feed
  the parser's fallback, and a gated call has no parser. `test_parse_status_upstream_both_zero_no_bracket`
  (`status.rs:560`) pins skim's *own summary* rendering, which is defensible behaviour —
  an in-sync branch genuinely has nothing to report. **Do not amend it.**
  **No production line changed for F9.** The fix is **G**'s gate subsuming the whole
  output format — the argv never reaches the parser, so there is nothing to repair in it —
  rather than a parse repair on the `# branch.ab` path, which measurement showed was
  never broken.
- **Regression test:** `rskim` · `tests/cli_git_contract_flags.rs::porcelain_v2_branch_keeps_ahead_behind_header`
  (new; a case in G's table-driven test) · `cargo build -p rskim && cargo nextest run -p rskim --test cli_git_contract_flags -j 4`
- **Re-bless:** none
- **Status:** LANDED

---

## F7 — push renders the destination ref of a refspec pair

- **Defect:** `skim git push` renders only the *source* side of a refspec, so
  `git push origin feature:main` reports `feature` and the reader never learns which
  remote ref was written. For a push whose source and destination differ — the shape
  most likely to be a mistake — skim's summary omits precisely the half that matters.
- **Root cause:** `crates/rskim/src/cmd/git/push.rs:419-429` (`fn extract_short_ref`).
  `:421` takes the first whitespace/tab-delimited field, then `:422`
  (`src.split(':').next()`) discards everything after the colon — the destination — before
  `:423-427` strip the `refs/heads/` or `refs/tags/` prefix. Sole call site is `:280`
  (`let short_ref = extract_short_ref(rest.trim());`).
- **Precondition:** the refspec under test must be **asymmetric** — `src` and `dst` must
  differ. A symmetric refspec (`main:main`, `refs/heads/old:refs/heads/old`) renders
  identically before and after the fix, so the check passes green against an unfixed
  binary. This is not hypothetical: it is exactly how
  `test_deleted_ref_porcelain_happy_path` (`push.rs:781`) came to pass either way.
- **Acceptance (argv):**

  ```bash
  cd /Users/dean/Sandbox/skim-issues
  B="$PWD/target/skim-baseline-c2b4378"; T="$(/usr/bin/mktemp -d)"; GIT=/usr/bin/git
  C="-c user.email=t@t -c user.name=t -c commit.gpgsign=false"
  "$GIT" init -q --bare "$T/sink.git"
  "$GIT" init -q "$T/src"; cd "$T/src"
  printf 'x\n' > a; "$GIT" add a; "$GIT" $C commit -q -m c0
  for b in fa fb side2; do "$GIT" branch "$b"; done
  # PRECONDITION: the refspec must be ASYMMETRIC (src != dst). A symmetric refspec
  # renders identically before and after the fix — that is exactly how
  # test_deleted_ref_porcelain_happy_path came to pass either way.
  SKIM_DISABLE_ANALYTICS=1 "$B" git push "$T/sink.git" fa:refs/heads/dst-aaa \
    > "$T/skim" 2>"$T/skim.err"
  printf 'rc=%s stdout=%s stderr=%s\n' "$?" "$(/usr/bin/wc -c <"$T/skim")" \
    "$(/usr/bin/wc -c <"$T/skim.err")"
  /bin/cat "$T/skim"; /usr/bin/sed -n '2p' "$T/skim" | /usr/bin/xxd   # the ref line, verbatim
  # RAW control — real git, absolute path, per-command SKIM_PASSTHROUGH (PF-026)
  SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 "$GIT" push "$T/sink.git" fb:refs/heads/dst-bbb \
    > "$T/raw" 2>"$T/raw.err"
  printf 'rc=%s stdout=%s stderr=%s\n' "$?" "$(/usr/bin/wc -c <"$T/raw")" \
    "$(/usr/bin/wc -c <"$T/raw.err")"
  /bin/cat "$T/raw.err"          # git names BOTH sides here; skim's stdout names one
  # force path
  SKIM_DISABLE_ANALYTICS=1 "$B" git push "$T/sink.git" +side2:refs/heads/dst
  # guard accounting — served vs the INJECTED-porcelain baseline (raw_override: None, PF-024)
  SKIM_DISABLE_ANALYTICS=1 "$B" git push "$T/sink.git" fa:refs/heads/dst-eee --show-stats 2>&1 \
    | /usr/bin/grep -i token
  ```
- **BEFORE (measured at c2b4378):** **DEFECT REPRODUCED.** rc 0, stdout **149 B**,
  **stderr 0 B**: `push 1 pushed` / ` * fa [new]` / ` To <LAB>/sink.git`. Line 2
  verbatim: `20 2a 20 66 61 20 5b 6e 65 77 5d 0a` — the source side only.
  **RAW control:** rc 0, stdout 0, stderr 157 B, containing
  ` * [new branch]      fb -> dst-bbb`. **The destination is absent from skim's render**,
  and it is the half most likely to be the mistake.
  **Force path:** `+side2:refs/heads/dst` → ` + side2 [forced]` — same loss, same shape.
  **Guard:** 149 B served against a **174 B injected-porcelain baseline**
  (`raw_override: None`, PF-024) → `Keep`, **25 B headroom**, corroborated by
  `--show-stats` `74 → 62 tokens (16.2%)`. The fix has room to name both sides.
- **AFTER (required):** an asymmetric refspec renders both sides. Add a **new**
  `format_ref_pair` and leave `extract_short_ref` alone so its three existing unit tests
  (`test_extract_short_ref_heads` `:707`, `test_extract_short_ref_tags` `:712`,
  `test_extract_short_ref_bare` `:717`) stay green without amendment. A symmetric refspec
  must keep rendering as a single short name — the new renderer must not turn
  `main:main` into `main:main`.
- **Regression test:** `rskim` · `src/cmd/git/push.rs::test_format_ref_pair_renders_asymmetric_refspec`
  (new, in the owning module's `#[cfg(test)]`) · `cargo nextest run -p rskim --bins -j 4 -E 'test(/push/)'`
- **Re-bless:** none
- **Status:** LANDED

---

## F8 — push renders the ref name on `--delete`

- **Defect:** a real `git push --porcelain` deleted-ref line has an **empty source side**.
  `extract_short_ref` takes the source side, so a deletion renders with a blank ref name:
  the reader is told something was deleted but not what.
- **Root cause:** `crates/rskim/src/cmd/git/push.rs:419-429` (`fn extract_short_ref`) —
  the same function as F7, which is why this commit **must follow F7**. `:422`
  (`src.split(':').next()`) returns the empty string for git's real delete shape, which
  the file's own documentation records twice as `-\t:refs/heads/old\t[deleted]` (`:24` and
  `:234`). The blank then flows into `deleted.push(format!("- {short_ref} [deleted]"))` at
  `:287`.
- **Precondition:** the porcelain fixture line must use git's **real** delete shape, with
  an empty source side (`-\t:refs/heads/old\t[deleted]`). A symmetric refspec makes the
  check vacuous. `test_deleted_ref_porcelain_happy_path` (`push.rs:781`) feeds
  `-\trefs/heads/old:refs/heads/old\t[deleted]`, which is **not** git's delete shape and
  which passes either way. Per Gate 4 this is a *fixture* defect, not a test whose
  polarity should be inverted: **correct the fixture, do not invert the test.**
- **Acceptance (argv):** reuses **F7**'s fixture (`$T/src`, `$T/sink.git`) and adds a
  remote literally named `throwaway`, because `--delete <remote> <ref>` needs a named remote.

  ```bash
  cd "$T/src"; GIT=/usr/bin/git
  B=/Users/dean/Sandbox/skim-issues/target/skim-baseline-c2b4378
  "$GIT" remote add throwaway "$T/sink.git"
  for r in bbb ccc ddd; do "$GIT" push -q throwaway "fb:refs/heads/dst-$r"; done
  # PRECONDITION: git's REAL delete shape has an EMPTY source side. The pre-F8 unit
  # fixture fed `-\trefs/heads/old:refs/heads/old\t[deleted]`, which is NOT that shape.
  SKIM_DISABLE_ANALYTICS=1 "$B" git push --delete throwaway dst-bbb > "$T/skim" 2>"$T/skim.err"
  printf 'rc=%s stdout=%s stderr=%s\n' "$?" "$(/usr/bin/wc -c <"$T/skim")" \
    "$(/usr/bin/wc -c <"$T/skim.err")"
  /bin/cat "$T/skim"; /usr/bin/sed -n '2p' "$T/skim" | /usr/bin/xxd   # the blank, in hex
  # RAW control
  SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 "$GIT" push --delete throwaway dst-ccc 2>&1
  # GROUND TRUTH — the exact porcelain line skim's parser reads
  SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 "$GIT" push --porcelain --delete throwaway dst-ddd \
    | /usr/bin/xxd
  "$GIT" --version
  # FIXTURE-VACUITY check, measured separately: the pre-F8 unit fixture passes against
  # BOTH the parent renderer and F7's, so no assertion on it can discriminate.
  cd /Users/dean/Sandbox/skim-issues
  cargo nextest run -p rskim --bins -j 4 -E 'test(/deleted_ref_porcelain/)'
  ```
- **BEFORE (measured at c2b4378):** **DEFECT REPRODUCED.**
  `skim git push --delete throwaway dst-bbb`: rc 0, stdout **152 B**, stderr 0 B, with
  ` -  [deleted]` — xxd `20 2d 20 20 5b 64 65 6c 65 74 65 64 5d 0a`, a **double space**
  where the ref name belongs. **Raw control:** ` - [deleted]         dst-ccc`.
  **Ground truth (git 2.50.1):** `-<TAB>:refs/heads/dst-ddd<TAB>[deleted]` — the source
  side is **empty**, exactly the side `extract_short_ref` reads.
  **Fixture vacuity, measured separately:** the pre-F8 test passes against **BOTH** the
  parent renderer and F7's, emitting the identical witness
  `"push 1 deleted\n - old [deleted]"`. The symmetric fixture's source side carried
  `old` — the one side the broken renderer reads — so **no assertion on that fixture
  could discriminate**. This is a fixture defect, not a test polarity defect.
- **AFTER (required):** `- old [deleted]` — the destination ref name, short form, never
  blank. **Landed as test-correctness only: ZERO production lines changed.** The render
  was already fixed by **F7 (C5)**'s `format_ref_pair`, which handles the empty-source
  case as a deletion; what remained here was the vacuous fixture. F8 is therefore the
  **test-correctness commit that makes the delete path provable** — before it, the path
  was correct and unprovable, which is the same review risk as incorrect and unnoticed.
  That also makes F7's ordering dependency load-bearing rather than merely convenient. The informational-line regression at `push.rs:763-772` (AD-GP-2: a `- Some info text`
  line without a tab must not produce a deleted ref) must stay green.
- **Regression test:** `rskim` · `src/cmd/git/push.rs::test_deleted_ref_porcelain_happy_path`
  (**amended fixture**, `:781`) and `src/cmd/git/push.rs::test_format_ref_pair_empty_source_is_a_deletion`
  (new) · `cargo nextest run -p rskim --bins -j 4 -E 'test(/push/)'`
- **Re-bless:** none
- **Status:** LANDED

---

## F13 — `--porcelain -z` gains an extra trailing byte

- **Defect:** on the ADR-001 `Passthrough` verdict, skim appends a newline to raw git
  output that does not already end in one. With `-z` the last record ends in NUL, so the
  gate fires and the reader receives one byte git never wrote. For a NUL-delimited machine
  contract that trailing byte is a malformed final record.
- **Root cause:** **reframed from the plan — this is a generic-sink fix, not a git fix.**
  The reachable leak is `crates/rskim/src/cmd/execution.rs:1659`, **not** the git call
  site: the contract-flag gate is git-only (`has_machine_contract_flag` has exactly **one**
  production call site), so the git arm is **doubly dead** — gated by G, and inert anyway
  because every ungated git argv measured is newline-terminated or empty. It is rerouted
  regardless, as defense in depth, but it is not what the fix is for.
  The guard is `crates/rskim/src/cmd/execution.rs:174-180`
  (`fn write_and_flush`), whose `ensure_trailing_newline` branch at `:176-178` appends the
  byte; `emit_raw_passthrough` at `:210-214` is the sink that passes `true` for that
  parameter (`:212`). **This corrects the plan's pointer**, which named
  `emit_raw_passthrough` at `:169-176` — `:169-176` is `write_and_flush`'s doc comment and
  signature, and `emit_raw_passthrough` is 40 lines further down at `:210`. The leaking
  call site is `crates/rskim/src/cmd/git/mod.rs:532`
  (`let (tier, status) = exec::emit_raw_passthrough(emit_raw)?;`), inside the
  `exec::SavingsDecision::Passthrough` arm.
- **Precondition:** two conditions, both mandatory. (1) The raw git output must **not
  already end in `\n`** — with `-z` git's final record ends in NUL, which is what arms the
  guard; any fixture whose output happens to end in a newline makes the check vacuous.
  (2) The ADR-001 verdict must be **`Passthrough`**, not `Keep`: the `Keep` arm at
  `git/mod.rs:523` writes through `write_line_to_stdout`, a different sink with a
  different newline contract, so a fixture that lands on `Keep` measures nothing about
  this defect. Assert the served tier, not just the bytes. Because the reachable arm is
  the generic sink, the acceptance command drives `curl` — a `file://` body with no
  trailing newline — rather than `git`.
- **Acceptance (argv):** the reachable arm is the **generic sink**, so the acceptance
  command drives `curl`, not `git`. The git arm is measured only for the record, on the
  pre-gate baseline.

  ```bash
  cd /Users/dean/Sandbox/skim-issues
  B="$PWD/target/skim-baseline-c2b4378"; T="$(/usr/bin/mktemp -d)"
  # PRECONDITION (1): the raw bytes must NOT already end in \n, or the guard never arms.
  # Sizes and the final byte are chosen to reproduce the measured numbers exactly:
  printf 'abcdefghijklmnopqrstuvwxyze'     > "$T/body.txt"  # 27 B, unterminated, last=0x65
  printf 'a'                               > "$T/one.txt"   #  1 B, unterminated
  printf 'abcdefghijklmnopqrstuvwxyz123\n'  > "$T/nl.txt"    # 30 B, TERMINATED control
  printf 'abcdefghijklmnopqrstuvwxyz123456' > "$T/probe32.txt"  # 32 B, helper probe
  for f in body one nl; do
    SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 /usr/bin/curl -s "file://$T/$f.txt" \
      > "$T/$f.raw" 2>/dev/null
    SKIM_DISABLE_ANALYTICS=1 "$B" curl -s "file://$T/$f.txt" > "$T/$f.skim" 2>"$T/$f.err"
    printf '%-5s raw=%-4s skim=%-4s err=%-3s rawlast=%-2s skimlast=%s\n' "$f" \
      "$(/usr/bin/wc -c <"$T/$f.raw")" "$(/usr/bin/wc -c <"$T/$f.skim")" \
      "$(/usr/bin/wc -c <"$T/$f.err")" \
      "$(/usr/bin/tail -c1 "$T/$f.raw"  | /usr/bin/xxd -p)" \
      "$(/usr/bin/tail -c1 "$T/$f.skim" | /usr/bin/xxd -p)"
  done
  # PRECONDITION (2): the verdict must be Passthrough, not Keep. Discriminator — a body
  # that genuinely compresses lands on Keep and measures nothing about this defect.
  # ($T/big.json = an indented JSON body of ~5279 B with ONE top-level key)
  SKIM_DISABLE_ANALYTICS=1 "$B" curl -s "file://$T/big.json" > "$T/big.skim" 2>/dev/null
  /usr/bin/wc -c "$T/big.json" "$T/big.skim"; /usr/bin/sed -n 1p "$T/big.skim"
  # helper probe — the sink in isolation
  SKIM_DISABLE_ANALYTICS=1 "$B" curl -s "file://$T/probe32.txt" | /usr/bin/wc -c
  # GIT ARM, for the record only (doubly dead once G lands: gated, and inert because every
  # ungated git argv measured is newline-terminated or empty). Run on the PRE-gate baseline.
  cd "$T/repo"    # G's fixture
  for c in "status -z" "status --porcelain -z"; do
    SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 /usr/bin/git $c | /usr/bin/wc -c
    SKIM_DISABLE_ANALYTICS=1 "$B" git $c | /usr/bin/wc -c
    SKIM_DISABLE_ANALYTICS=1 "$B" git $c | /usr/bin/tail -c2 | /usr/bin/xxd
  done
  ```
- **BEFORE (measured at c2b4378):** **DEFECT REPRODUCED on the generic sink.**
  **Reachable arm (`curl`):** a `file://` body served **28 B against 27 B**
  (`rawlast=65`, `skimlast=0a` — one byte skim appended and `curl` never wrote); a 1 B
  body served **2 B against 1 B**, i.e. a **100% overhead** on the smallest input.
  Exit 0/0 and **zero bytes on stderr** in both cases.
  **Control:** a 30 B newline-terminated body served 30 → 30, unchanged — the guard is
  the terminator, not the size.
  **Precondition-(2) discriminator:** a 5279 B indented-JSON body served **3942 B** with
  line 1 `curl response object with 1 key`, i.e. `Keep`, which measures nothing about
  this defect. Helper probe: 33 B against 32 B.
  **Git arm, on the pre-gate baseline:** `git status -z` **51 B against 50 B** with a
  trailing `0a` after the terminating NUL; `--porcelain -z` 51 against 41. Once G lands
  this arm is **doubly dead** — gated, and inert because every ungated git argv measured
  is newline-terminated or empty.
- **AFTER (required):** stdout is byte-identical to real `git`, with no appended newline.
  Add a **new** `exec::emit_raw_passthrough_exact`. Do **not** reroute through
  `emit_raw_passthrough_split` (`execution.rs:257`), whose contract is two streams on two
  descriptors, and do **not** flip the shared helper's newline guard globally: five other
  call sites are documented as load-bearing on it —
  `crates/rskim/src/cmd/log.rs:128`, `crates/rskim/src/cmd/test/shared.rs:184`,
  `crates/rskim/src/cmd/test/shared.rs:231`, and the streamed-sink mirror in
  `crates/rskim/src/cmd/file/passthrough_stream.rs:143-152` (`ensure_newline` at `:148`),
  which reproduces the guard deliberately and carries a "do not simplify this to a
  constant without changing the buffered sink in the same commit" note. **Measured
  correction to that count:** the load-bearing sites are **three calls plus one mirror**,
  not five — `cmd/log.rs:128`, `cmd/test/shared.rs:184`, `cmd/test/shared.rs:231`, and the
  `passthrough_stream.rs:143-152` mirror. The AFTER text said "five" while enumerating
  four; the enumeration was right and the count was wrong. Note for the reviewer: there are eight `emit_raw_passthrough`
  call sites in total, and `crates/rskim/src/cmd/git/log.rs:192` is a second one inside the
  git subsystem (the large-output degrade path, which appends an elision marker of its
  own); confirm it is not a second leak before asserting that `:532` is the only one.
- **Regression test:** `rskim` · `tests/cli_curl_fidelity.rs::{unterminated_body_is_byte_identical_to_raw_curl`
  (`:258`), `single_byte_body_is_byte_identical_to_raw_curl` (`:272`),
  `newline_terminated_body_is_byte_identical_to_raw_curl` (`:289`),
  `the_guard_can_still_elect_keep` (`:341`)`}` — **this file is the generic-sink
  reframe's acceptance layer, which is why it belongs here and not under `git`.** Each of
  the first three asserts full stdout byte-equality against real `curl`, not a
  `contains`; the fourth is precondition-(2)'s discriminator, proving the other three
  landed on `Passthrough` rather than on an arm the guard never armed ·
  `cargo build -p rskim && cargo nextest run -p rskim --test cli_curl_fidelity -j 4`,
  plus `rskim` · `src/cmd/execution.rs::{guard_on_appends_a_byte_the_tool_never_emitted`
  (`:3594`), `passthrough_verdict_sink_is_byte_exact` (`:3612`),
  `the_two_raw_sinks_stay_declared_with_one_shape` (`:3637`)`}` — the third is a
  **compile-level** pin: the two coercions *are* the assertion, so deleting either sink
  is `E0425` and drifting one signature is `E0308`, and neither sink is called because
  both lock real stdout ·
  `cargo nextest run -p rskim --bins -j 4 -E 'test(/passthrough_verdict_sink/) + test(/appends_a_byte/) + test(/two_raw_sinks/)'`.
  **Corrected against the working tree:** the plan named
  `tests/cli_git_contract_flags.rs::porcelain_z_output_has_no_extra_trailing_byte` and
  `src/cmd/execution.rs::test_emit_raw_passthrough_exact_appends_nothing` — **neither
  exists.** The helper itself does land: `exec::emit_raw_passthrough_exact` is
  `execution.rs:266`, called at `:1728` (the generic sink, the reachable leak) and at
  `git/mod.rs:886` (the defense-in-depth git arm).
- **Re-bless:** none
- **Status:** LANDED

---

## F4 — `git show <rev>:<path>` byte-faithful, honours `--mode`, discloses the lossy view

- **Defect:** `skim git show <rev>:<path>` runs the blob through the pseudo transform
  unconditionally. Three separate faults follow: the default is lossy where the caller
  asked for a file's contents at a revision (a byte contract, commonly piped); an
  explicit `--mode` is ignored; and the lossy view carries **no** class-1 disclosure
  marker, although `process.rs` emits one for the byte-identical transform on the file
  path. No `lossy_view_marker` call exists anywhere under `cmd/git/` today.
- **Root cause:** `crates/rskim/src/cmd/git/show.rs:887` — the unconditional
  `let config = TransformConfig::with_mode(Mode::Pseudo);`, documented at `:875` as
  `Fix D` / `AD-GIT-SHOW-PSEUDO`. **The root cause is `:887`, not `:919`**, which the plan
  conflated: `:919` is the **uncharged** `apply_to_stderr` shim — the site where a class-1
  marker must be *introduced and charged* (facet 3), which is a consequence of the defect,
  not its cause. Fixing `:919` alone would disclose a lossy view that should not have been
  served at all.
  **Caveat on the byte-faithfulness claim:** it holds for **valid-UTF-8 blobs only**.
  `CommandRunner::run` converts lossily at `crates/rskim/src/runner.rs:472`, `:498` and
  `:506`, so a blob with invalid UTF-8 cannot be served byte-faithfully through this path
  regardless of what `show.rs` does. State that scope in the commit message rather than
  claiming byte-faithfulness unconditionally.
- **Precondition:** stderr **must not contain** `[skim:guardrail]` for the `BEFORE` to
  exhibit the defect at all. If the transformed blob is larger than raw, ADR-001 already
  serves raw — stdout is then byte-identical to `git show` and the defect is masked, so a
  naive byte-equality assertion passes green against a completely unfixed binary. The
  fixture blob must be one on which the transform is *served*. For the `--mode` arm,
  assert that the requested mode's marker appears on stderr rather than inferring the mode
  from stdout. For the marker arm, the precondition is the marker's own presence on stderr.
- **Acceptance (argv):**

  ```bash
  cd /Users/dean/Sandbox/skim-issues
  B="$PWD/target/skim-baseline-c2b4378"; T="$(/usr/bin/mktemp -d)"; REV=c2b4378
  # FACET 1 — default blob path is lossy. PRECONDITION: stderr must NOT carry
  # [skim:guardrail]; a blob whose transform exceeds raw is served raw and masks the defect,
  # so `simple.ts` is carried as the masked control rather than dropped.
  for p in crates/rskim-core/src/transform/utils.rs tests/fixtures/typescript/types.ts \
           tests/fixtures/typescript/comments.ts tests/fixtures/typescript/simple.ts; do
    SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 /usr/bin/git show "$REV:$p" > "$T/raw" 2>/dev/null
    SKIM_DISABLE_ANALYTICS=1 "$B" git show "$REV:$p" > "$T/skim" 2>"$T/err"
    printf '%-48s raw=%-6s skim=%-6s err=%-3s guardrail=%s\n' "$p" \
      "$(/usr/bin/wc -c <"$T/raw")" "$(/usr/bin/wc -c <"$T/skim")" \
      "$(/usr/bin/wc -c <"$T/err")" "$(/usr/bin/grep -c '\[skim:guardrail\]' "$T/err")"
    /usr/bin/cmp "$T/raw" "$T/skim"
  done
  # the interface body, before and after the transform
  SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 /usr/bin/git show \
    "$REV:tests/fixtures/typescript/types.ts" | /usr/bin/grep -n 'interface User' -A4
  SKIM_DISABLE_ANALYTICS=1 "$B" git show "$REV:tests/fixtures/typescript/types.ts" \
    | /usr/bin/grep -n 'interface User' -A4
  # FACET 2 — no class-1 disclosure anywhere on this path
  SKIM_DISABLE_ANALYTICS=1 "$B" git show "$REV:tests/fixtures/typescript/types.ts" \
    2>&1 >/dev/null | /usr/bin/wc -c
  SKIM_DISABLE_ANALYTICS=1 SKIM_DEBUG=1 "$B" git show "$REV:tests/fixtures/typescript/types.ts" \
    2>&1 >/dev/null | /bin/cat
  /usr/bin/grep -rn 'lossy_view_marker' crates/rskim/src/cmd/git/   # must be empty
  # FACET 3 — --mode is not a silent no-op; it is a hard `git` error
  SKIM_DISABLE_ANALYTICS=1 "$B" git show --mode=full "$REV:tests/fixtures/typescript/types.ts" \
    > "$T/m1" 2>"$T/m1.err"; printf 'rc=%s stdout=%s\n' "$?" "$(/usr/bin/wc -c <"$T/m1")"
  /bin/cat "$T/m1.err"
  SKIM_DISABLE_ANALYTICS=1 "$B" git show --mode full "$REV:tests/fixtures/typescript/types.ts" \
    > "$T/m2" 2>"$T/m2.err"; printf 'rc=%s\n' "$?"; /bin/cat "$T/m2.err"
  # escape hatch is byte-faithful
  SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 "$B" git show \
    "$REV:tests/fixtures/typescript/types.ts" > "$T/ph"
  /usr/bin/wc -c "$T/ph"; /usr/bin/cmp "$T/ph" "$T/raw"
  ```
- **BEFORE (measured at c2b4378):** **ALL THREE FACETS REPRODUCED.**
  **Facet 1 — lossy on every supported blob:** `crates/rskim-core/src/transform/utils.rs`
  24675 → 24083, `tests/fixtures/typescript/types.ts` **605 → 475**,
  `tests/fixtures/typescript/comments.ts` 1153 → 807,
  `tests/fixtures/typescript/simple.ts` 275 → 275 (the guard chose raw — carried as the
  **masked control**, which is the shape a naive byte-equality assertion would have
  mistaken for a fix). `cmp` differs at **byte 1** for `types.ts`;
  `interface User { id: UserId; … }` → `interface User { id name email }`.
  **Facet 2 — no disclosure:** stderr **0 bytes**; under `SKIM_DEBUG=1` only the 99 B
  provenance line, **no marker**; `grep -rn lossy_view_marker crates/rskim/src/cmd/git/`
  → **no matches**.
  **Facet 3 — `--mode` is NOT a silent no-op**, which corrects the defect statement's
  "ignored": `--mode=full` → rc **1**, stdout 0 B, stderr 42 B
  `fatal: unrecognized argument: --mode=full`; `--mode full` → rc 1,
  `fatal: ambiguous argument 'full'`. The flag reaches `git`, not skim.
  `SKIM_PASSTHROUGH=1` returns 605 B, `cmp`-identical to raw — so the byte-faithful view
  exists today and is reachable only by opting out of skim.
- **AFTER (required):** three parts, all shipped in one commit. (1) The default blob path
  is **byte-faithful** — stdout equals `git show <rev>:<path>` byte for byte. (2) An
  explicit `--mode` is honoured via `extract_diff_mode`. (3) The opt-in transform path
  emits the class-1 `lossy_view_marker`. **This is a decision reversal, not a bug fix:**
  `Mode::Pseudo` on the blob path is a documented decision (`AD-GIT-SHOW-PSEUDO`, "Fix D")
  pinned by three tests, and the commit message must say so and record that the original
  rationale has partly decayed — it cites stripping "visibility modifiers", which Rust
  pseudo no longer does (`pseudo.rs:290-297` records `visibility_modifier`'s removal from
  `strip_kinds` as API surface).
- **Regression test:** `rskim` · three **amended** tests in `src/cmd/git/show.rs` —
  `test_fix_d_file_content_transform_uses_pseudo_mode` (`:1578`),
  `test_fix_d_pseudo_mode_preserves_function_body` (`:1598`),
  `test_fix_d_pseudo_vs_structure_discriminates_body_tokens` (`:1633`) — plus
  `rskim` · `tests/cli_git_show_blob.rs::{blob_default_is_byte_faithful, blob_honours_explicit_mode, blob_transform_emits_lossy_view_marker}`
  (new) · `cargo nextest run -p rskim --bins -j 4 -E 'test(/show/)'` and
  `cargo build -p rskim && cargo nextest run -p rskim --test cli_git_show_blob -j 4`.
  Per Gate 4 these three are the **only** tests in the campaign that must be amended
  rather than supplemented.
- **Re-bless:** none
- **Status:** LANDED

---

## F3 — `npm ls` keeps package versions

- **Defect:** `skim npm ls` reads each dependency's `version` out of the JSON and then
  discards it unless that dependency also carries a non-empty `problems` array. A healthy
  tree therefore compresses to a count with no versions at all — which is the one thing a
  reader runs `npm ls` to learn. The regex tier is worse: it never looks at versions,
  counts lines, and reports only a total and a flagged count.
- **Root cause:** `crates/rskim/src/cmd/pkg/npm/ls.rs` — JSON tier `try_parse_ls_json`
  at **`:59-88`** (**corrected from the plan's `:58-86`**): the version is read at `:68`
  (`let version = dep.get("version")…unwrap_or("?")`) and is then referenced **only**
  inside the `problems` branch at `:70-79`, so on a clean tree it is computed and thrown
  away; the returned `details` vector at `:86` is empty. Regex tier `try_parse_ls_regex`
  at **`:90-115`** (**corrected from the plan's `:88-113`**): counts non-empty lines at
  `:92-102` and returns `details: vec![]` at `:113`. Tier 3 passthrough is at `:56`.
- **Precondition:** the served tier must be the JSON tier (or, for the regex-tier test,
  the regex tier) and **not** `Passthrough`. Output *grows* under this fix, so on a small
  dependency tree the compressed render can legitimately exceed raw and ADR-001 will serve
  raw — in which case stdout already contains every version and a naive "stdout contains
  `lodash@4.17.21`" assertion passes green against a completely unfixed binary. Assert the
  served tier (`--show-stats`, or `SKIM_DEBUG=1` and read the banner) before asserting
  anything about stdout. Per the plan's ADR-001 flip audit, if the observable on a small
  tree is legitimately "serves raw", that must be **stated in this ledger**, not
  discovered at verification time.
- **Acceptance (argv):**

  ```bash
  cd /Users/dean/Sandbox/skim-issues
  B="$PWD/target/skim-baseline-c2b4378"; T="$(/usr/bin/mktemp -d)"
  NPM=/Users/dean/.nvm/versions/node/v22.22.3/bin/npm
  /bin/mkdir -p "$T/proj"; cd "$T/proj"
  printf '{"name":"f3","version":"1.0.0","dependencies":{"ms":"2.1.3"}}\n' > package.json
  "$NPM" install --silent --no-audit --no-fund
  # PRECONDITION, ASSERTED FIRST: the served tier must be JSON (or, for the regex case,
  # regex) and NOT Passthrough. Output GROWS under this fix, so on a small tree ADR-001
  # can legitimately serve raw — in which case stdout already carries every version and a
  # naive `contains ms@2.1.3` assertion passes green against a completely unfixed binary.
  SKIM_DISABLE_ANALYTICS=1 SKIM_DEBUG=1 "$B" npm ls 2>&1 >/dev/null | /bin/cat
  # RAW control
  SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 "$NPM" ls > "$T/raw" 2>"$T/raw.err"
  printf 'rc=%s stdout=%s\n' "$?" "$(/usr/bin/wc -c <"$T/raw")"; /bin/cat "$T/raw"
  # served view
  SKIM_DISABLE_ANALYTICS=1 "$B" npm ls > "$T/skim" 2>"$T/skim.err"
  printf 'rc=%s stdout=%s stderr=%s\n' "$?" "$(/usr/bin/wc -c <"$T/skim")" \
    "$(/usr/bin/wc -c <"$T/skim.err")"; /bin/cat "$T/skim"
  # does the NAME survive, or only the version? (grep both)
  /usr/bin/grep -c 'ms' "$T/skim"; /usr/bin/grep -c '2\.1\.3' "$T/skim"
  # guard accounting / headroom
  SKIM_DISABLE_ANALYTICS=1 "$B" npm ls --show-stats 2>&1 | /usr/bin/grep -i token
  ```
- **BEFORE (measured at c2b4378):** **DEFECT REPRODUCED, and broader than reported.**
  RAW: rc 0, stdout **149 B**, containing `└── ms@2.1.3`. SKIM: rc 0, stdout **27 B**
  `npm list 1 total 0 flagged`, stderr 0 B.
  **The package NAME is dropped as well as the version** — the render carries neither,
  only two integers, so the reader learns nothing that `npm ls` is run to learn.
  `SKIM_DEBUG=1` → `[skim:warning] npm ls: JSON parse failed, using regex`, so **the
  regex tier is the live one** on this fixture and a JSON-tier-only fix would not be
  observable here.
  `--show-stats`: `73 tokens → 9 tokens (87.7%)`. **Headroom 122 B** — the fix grows the
  output, and 122 B is what it has to spend before ADR-001 flips to raw.
- **AFTER (required):** each dependency's version reaches the reader in the compressed
  summary on both the JSON and the regex tier, and the `problems` detail lines keep their
  existing `{name}@{version}: {msg}` shape. Output grows, so the ADR-001 flip risk is
  real and must be measured, not reasoned about.
- **Regression test:** `rskim` · `src/cmd/pkg/npm/ls.rs::{test_ls_json_keeps_versions_on_clean_tree, test_ls_regex_keeps_versions}`
  (new, in this module's own `#[cfg(test)]` block — **not** in
  `tests/cli_e2e_pkg_parsers.rs`, which spawns a nested cargo and would make the fast loop
  expensive) · `cargo nextest run -p rskim --bins -j 4 -E 'test(/npm/)'`
- **Re-bless:** none
- **Status:** LANDED

---

## F10 — `gh run list` stops prepending `#` to run identifiers

- **Defect:** `skim gh run list` renders each run's `databaseId` as `#12345`. A
  `databaseId` is not an issue number and the `#` makes it unusable: it cannot be pasted
  into `gh run view`, and on GitHub a bare `#12345` autolinks to an unrelated issue or PR.
  The prefix is correct for `issue list` and `pr list`, where the field genuinely is
  `number`, and wrong for `run list`.
- **Root cause:** `crates/rskim/src/cmd/infra/gh/list.rs:328-335`
  (`fn json_entry_to_infra_item` at `:328`): the label is built by an `.or_else()` chain
  that tries `number` at `:330` and falls back to `databaseId` at `:332`, then applies
  `.map(|n| format!("#{n}"))` at **`:333`** to *both* alternatives indiscriminately.
  **Additional site the plan does not name:** the regex tier `try_parse_regex` at `:478`
  prepends `#` again at **`:486`** (`label: format!("#{num}")`), so a fix confined to the
  JSON tier leaves the two tiers rendering the same run differently.
- **Precondition:** the served tier must be the **JSON** tier and not the regex tier or
  `Passthrough`. Output *shrinks* under this fix, which is the safe ADR-001 direction, but
  the tier still decides which of the two `#`-prepending sites produced the observable —
  a regex-tier render would keep the `#` after a JSON-tier-only fix and the test would
  read as a failed fix rather than an unfixed second site. Assert the tier, and cover both
  tiers.
- **Acceptance (argv):** skim spawns `gh`, so the fixture is a `gh` shim earlier on
  `PATH` that serves the injected JSON for skim's own `--json` invocation and a table
  otherwise, making both the JSON tier and the raw control reachable from one shim.

  ```bash
  cd /Users/dean/Sandbox/skim-issues
  B="$PWD/target/skim-baseline-c2b4378"; T="$(/usr/bin/mktemp -d)"; /bin/mkdir -p "$T/bin"
  # printf, not a heredoc: an indented heredoc terminator does not close the body
  printf '%s\n' '#!/bin/sh' \
    'for a in "$@"; do case "$a" in --json*) exec /bin/cat "$0.json";; esac; done' \
    'exec /bin/cat "$0.table"' > "$T/bin/gh"
  /bin/chmod +x "$T/bin/gh"
  # $T/bin/gh.json  = 960 B of `gh run list --json ...` rows (the guard BASELINE)
  # $T/bin/gh.table = the 532 B default table form (the RAW control; no `#` anywhere)
  # KEY PRESENCE — is the .or_else() ordering load-bearing on this input?
  /usr/bin/grep -o '"number"'     "$T/bin/gh.json" | /usr/bin/wc -l    # expect 0  ABSENT
  /usr/bin/grep -o '"databaseId"' "$T/bin/gh.json" | /usr/bin/wc -l    # expect 3  every row
  # PRECONDITION, ASSERTED FIRST: the served tier must be JSON, not regex, not Passthrough
  PATH="$T/bin:$PATH" SKIM_DISABLE_ANALYTICS=1 SKIM_DEBUG=1 "$B" gh run list 2>&1 >/dev/null \
    | /bin/cat
  # RAW control
  SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 "$T/bin/gh" run list > "$T/raw" 2>/dev/null
  # served view
  PATH="$T/bin:$PATH" SKIM_DISABLE_ANALYTICS=1 "$B" gh run list > "$T/skim" 2>"$T/err"
  printf 'raw=%s skim=%s err=%s\n' "$(/usr/bin/wc -c <"$T/raw")" \
    "$(/usr/bin/wc -c <"$T/skim")" "$(/usr/bin/wc -c <"$T/err")"
  /bin/cat "$T/skim"; /usr/bin/grep -c '#' "$T/skim"; /usr/bin/grep -c '#' "$T/raw"
  # FALSIFICATION — the rendered token must be pasteable into the tool it names
  /opt/homebrew/bin/gh run view '#36362655859'; echo "hashed rc=$?"
  /opt/homebrew/bin/gh run view  36362655859;   echo "bare   rc=$?"
  # guard accounting
  PATH="$T/bin:$PATH" SKIM_DISABLE_ANALYTICS=1 "$B" gh run list --show-stats 2>&1 \
    | /usr/bin/grep -i token
  # REGEX TIER — the anchor that makes it structurally unable to label a run ID
  /usr/bin/grep -n 'RE_GH_TAB_ROW' crates/rskim/src/cmd/infra/gh/list.rs
  ```
- **BEFORE (measured at c2b4378):** **DEFECT REPRODUCED.** SKIM stdout **536 B**,
  stderr 0 B, rc 0, rows rendered ` #36362655859: …`. **RAW control 532 B — this is raw
  `gh run list -L 3`**, gh's own tab-separated table
  (`STATUS CONCLUSION TITLE WORKFLOW BRANCH EVENT ID ELAPSED AGE`), whose ID column
  carries **no** `#`.
  **The 4 B gap is coincidence, not three hashes.** skim's render is a structurally
  different rendering — ` #ID: title (status/conclusion) · workflow · branch · event ·
  elapsed` — so `536 − 532` does not decompose into 3 × `#`, and no arithmetic relates
  the two figures at all. Recorded explicitly because the coincidence **obscures the
  more interesting fact**: skim's `gh run list` is a **net expansion against real raw
  output, and the ADR-001 guard structurally cannot see it** — `raw_override: None` makes
  the guard's baseline the **960 B injected JSON** rather than the 532 B the reader would
  otherwise have received, so the guard reports `Keep` on a view that is *larger* than
  raw. After the fix the served size is **533 B**, so the expansion narrows from **+4 B
  to +1 B** and is **NOT closed**. That is a separately filed issue and explicitly not
  something F10 fixes.
  **Falsification, against the tool the token names:** `gh run view '#36362655859'` →
  **HTTP 404: Not Found**; `gh run view 36362655859` → resolves. The `#` does not merely
  look wrong, it makes the value unusable.
  **Key presence in the injected JSON:** `"number"` → **0 occurrences (ABSENT)**,
  `"databaseId"` → **3 (every row)**. So the `.or_else()` ordering was **not**
  load-bearing on this input, which closes the plan's open item: there is no
  `databaseId`-less row to decide about here.
  **Guard:** 536 B served against a **960 B injected-JSON baseline** → `Keep`. Output
  shrinks under the fix, the safe ADR-001 direction.
- **AFTER (required):** a `run list` row renders its `databaseId` bare; `issue list` and
  `pr list` rows keep `#`. Replace the `.or_else()` chain with a two-arm match so the
  prefix is a property of *which field matched*, not of the chain's fallthrough. **Open
  item flagged by the plan:** if `databaseId` can be absent from a `run list` row, the
  current `.or_else()` ordering is load-bearing in a way a two-arm match changes —
  establish what a `databaseId`-less row should render *before* rewriting the chain.
  **Measured and closed:** `"number"` is absent from every injected row and `"databaseId"`
  present in all three, so the ordering was **not** load-bearing on this input.
  **Divergence recorded rather than reconciled: the regex tier at `:486` is deliberately
  NOT changed.** `RE_GH_TAB_ROW` is anchored `^(\d+)\t`, and a `gh run list` row starts
  with a *status word* with the ID in **column 7** — so 0 of 3 rows match and that tier
  **cannot label a run ID at all**. There is no divergence to reconcile because there is
  no run-ID label on that path, and changing the anchor to create one would **regress
  `gh pr list`**, whose rows genuinely do start with the number the `#` belongs to.
- **Regression test:** `rskim` · `src/cmd/infra/gh/list.rs::{test_run_list_label_has_no_hash_prefix, test_issue_list_label_keeps_hash_prefix, test_regex_tier_run_list_label_has_no_hash_prefix}`
  (new, beside the existing fixtures at `:718`/`:740`/`:790`) ·
  `cargo nextest run -p rskim --bins -j 4 -E 'test(/gh/)'`
- **Re-bless:** none
- **Status:** LANDED

---

## F2 — the rewrite engine declines when the target binary cannot be resolved

- **Defect:** the rewrite engine will rewrite `rg …` into `skim rg …` whether or not
  anything named `rg` can actually be found. When it cannot, the reader gets a command
  that fails, in place of one that would have worked — a rewrite that changes semantics,
  which is exactly what `#317` forbids.
- **Root cause:** `crates/rskim/src/cmd/rewrite/engine.rs` — `try_rewrite` at `:35` runs
  three pre-loop global bails before reaching the rule loop, the third being
  `if has_cargo_toolchain_override(command_tokens)` at `:69` (helper at `:312`); there is
  no resolvability bail. `crates/rskim/src/runner.rs:288` (`if program.contains('/')`) is
  the existing explicit-path rule the new probe must mirror.
  `crates/rskim/src/cmd/doctor/mod.rs:186` (`fn is_executable`) is the helper to hoist so
  there is one implementation rather than two.
- **Precondition:** the program named in the acceptance command must be genuinely
  unresolvable **on the PATH the probe sees**. Two facts make that non-trivial and both
  must hold in the fixture. `strip_skim_wrappers_from_path()` runs as `main()`'s first
  statement, so the probe observes an already-modified PATH — not the shell's. And a shell
  **function** is invisible to any PATH probe by construction, so the harness must invoke
  the binary directly rather than through a shell that defines one. A fixture that relies
  on the ambient environment will measure the wrong thing on a different machine.
- **Acceptance (argv):** three steps, because the defect is only visible when the
  engine's decision and the fate of what it emitted are measured **separately**.

  ```bash
  cd /Users/dean/Sandbox/skim-issues
  B="$PWD/target/skim-baseline-c2b4378"; T="$(/usr/bin/mktemp -d)"
  # PRECONDITION: `rg` must be unresolvable on the PATH THE PROBE SEES. Two facts make
  # that non-trivial: strip_skim_wrappers_from_path() runs as main()'s first statement, so
  # the probe sees an already-modified PATH; and a shell FUNCTION is invisible to any PATH
  # probe by construction. Invoke the binary directly, never through a shell that defines one.
  /usr/bin/env zsh -ic 'whence -w rg'                        # rg: function
  /bin/sh -c 'command -v rg'; echo "sh rc=$?"                # nothing, rc 1
  /bin/sh -c 'IFS=:; n=0; for d in $PATH; do n=$((n+1)); [ -x "$d/rg" ] && echo "$d/rg"; done; echo "entries=$n"'
  # STEP 1 — the engine's decision, in isolation
  SKIM_DISABLE_ANALYTICS=1 "$B" rewrite 'rg -n pattern crates/' > "$T/s1.out" 2>"$T/s1.err"
  printf 'rc=%s stdout=%s stderr=%s\n' "$?" "$(/usr/bin/wc -c <"$T/s1.out")" \
    "$(/usr/bin/wc -c <"$T/s1.err")"; /bin/cat "$T/s1.out"
  # STEP 2 — run exactly what Step 1 emitted
  /bin/sh -c "$(/bin/cat "$T/s1.out")" > "$T/s2.out" 2>"$T/s2.err"
  printf 'rc=%s stdout=%s stderr=%s\n' "$?" "$(/usr/bin/wc -c <"$T/s2.out")" \
    "$(/usr/bin/wc -c <"$T/s2.err")"; /bin/cat "$T/s2.err"
  # RAW control — the command the agent actually typed, in the shell that defines `rg`
  /usr/bin/env zsh -ic 'rg -n pattern crates/' > "$T/raw.out" 2>"$T/raw.err"
  printf 'rc=%s stdout=%s\n' "$?" "$(/usr/bin/wc -c <"$T/raw.out")"
  # ZERO-COMPRESSION control — with a real-ripgrep shim on PATH, in == out
  /bin/mkdir -p "$T/bin"
  printf '#!/bin/sh\nexec /bin/cat "$0.fixture"\n' > "$T/bin/rg"; /bin/chmod +x "$T/bin/rg"
  # ($T/bin/rg.fixture = 458 B of ripgrep-shaped `-n` output)
  PATH="$T/bin:$PATH" SKIM_DISABLE_ANALYTICS=1 "$B" rg -n pattern crates/ > "$T/z.out" 2>/dev/null
  /usr/bin/wc -c "$T/bin/rg.fixture" "$T/z.out"; /usr/bin/cmp "$T/bin/rg.fixture" "$T/z.out"
  # BLAST RADIUS — count the `--suggest` sites too; a scan keyed on .success()/.failure()
  # is structurally blind to them (see AFTER)
  /usr/bin/grep -rn '"match":false' crates/rskim/tests/ | /usr/bin/wc -l
  ```
- **BEFORE (measured at c2b4378):** **DEFECT REPRODUCED, and the two steps must be
  measured separately or it is invisible.**
  **Step 1 (the engine's decision):** rc 0, stdout **27 B** `skim rg -n pattern crates/`,
  stderr 0 B — **no existence check, no bail**. On its own this reads as success.
  **Step 2 (running what Step 1 emitted):** rc **1**, stdout 0 B, stderr 83 B
  `error: 'rg' not found`.
  **Raw control — the command the agent actually typed:** rc 0, stdout **20711 B**.
  `rg` on this host is a shell **function with no binary behind it**, so the rewrite
  replaced a working command with a failing one. The `rg` binary is genuinely
  unresolvable: `whence -w rg` → `rg: function`; `/bin/sh command -v rg` → nothing,
  exit 1; a direct PATH walk over **35 entries** → not resolvable.
  **Exit 1 collides with ripgrep's own no-matches code**, so a caller keying on `$?`
  cannot distinguish "not installed" from "no matches" — the failure is not just loud,
  it is ambiguous.
  **Zero-compression control** with a real ripgrep shim: **458 B in / 458 B out**, `cmp`
  identical — so the rewrite buys nothing on this tool even when it works.
- **AFTER (required):** a **fourth pre-loop global bail** in `engine.rs`, beside
  `has_cargo_toolchain_override` at `:69`, following the same `#317` "bail rather than
  reconstruct unfaithfully" precedent. The predicate is program-agnostic, so it does not
  belong on a rule — and `RewriteRule` cannot express a runtime predicate anyway, since all
  seven of its fields are `&'static [&'static str]` or `bool`. New helper
  `runner::program_resolves(program: &str) -> bool` mirroring `runner.rs:288`'s
  explicit-path rule; hoist `doctor/mod.rs:186`'s `is_executable` and have `doctor` call
  the shared version. Cache in a `OnceLock<RwLock<HashMap<String, bool>>>` because
  `try_rewrite_compound` calls `try_rewrite` once per pipeline segment.
  **Document the semantic limitation in-code:** the probe answers *"would `Command::new(p)`
  find it?"*, **not** *"would the shell run something for `p`?"*. The decline is correct
  here but answers an adjacent question — a deliberate conservative approximation, and
  saying so in the source is part of the fix.
  **Integration-test blast radius: 61 sites, not 48.** The 13 sites the first scan missed
  are `--suggest` sites asserting `"match":false` — which a **declined** rewrite also
  produces — so a scan keyed on `.success()` / `.failure()` is **structurally blind** to
  them: they neither pass nor fail differently when the engine starts declining, they just
  start being right for a different reason. Enumerate `"match":false` separately; the
  exit-status scan cannot find them by construction.
- **Regression test:** `rskim` · `src/cmd/rewrite/engine.rs::test_unresolvable_program_declines_rewrite`
  and `src/runner.rs::test_program_resolves_*` (new) ·
  `cargo nextest run -p rskim --bins -j 4 -E 'test(/rewrite/) + test(/program_resolves/)'`
- **Re-bless:** none
- **Status:** LANDED

---

## F12 — an interior-newline rewrite bail emits a signal

- **Defect:** when the hook bails on a command it cannot reconstruct byte-faithfully —
  the interior-newline / heredoc corruption class — it exits silently. The reader (and the
  agent) has no way to learn that a rewrite was declined, so a bundling agent whose every
  command bails looks identical to one whose commands are all being rewritten
  successfully. The behaviour is correct; its invisibility is the defect.
- **Root cause:** `crates/rskim/src/cmd/rewrite/hook.rs:467-470` — the bail block
  `if command_needs_passthrough(&command) { audit_hook(&command, false, ""); return Ok(ExitCode::SUCCESS); }`.
  **The plan's `:468` is the `audit_hook` line inside that block**, not the block's head.
  The enclosing function is `run_hook_mode` at `:316`, where `agent_kind` is bound at
  `:341` and `crate::cmd::resolve_cache_dir()` is already called at `:373`/`:415`/`:441` —
  so both are in scope at the bail site, as the plan states. `warn_once_daily` is in the
  same file at `:577`.
- **Precondition:** two conditions. (1) The command must contain an **interior** newline,
  not merely a trailing one — `command_needs_passthrough` trims trailing whitespace first,
  precisely so that the trailing newlines agent hooks commonly append do not trigger a
  bail; a fixture with only a trailing newline exercises the opposite branch. (2) The
  daily stamp file must be **absent** at the start of the check. `warn_once_daily` is
  rate-limited per kind per agent per day, so a second run in the same day emits nothing
  and reads as a failed fix. Point `SKIM_CACHE_DIR` at a fresh temporary directory, and
  assert the second invocation is silent as a separate positive check.
- **Acceptance (argv):** hook mode is reached as `skim rewrite --hook`; the payload
  skim reads is `{"tool_input":{"command":…}}` (a flat `{"command":…}` object hits the
  missing-command-field early exit and produces 0 B on every arm, which would read as a
  false positive for the bail under test).

  ```bash
  cd /Users/dean/Sandbox/skim-issues
  B="$PWD/target/skim-baseline-c2b4378"; T="$(/usr/bin/mktemp -d)"
  # PRECONDITION (2): a FRESH SKIM_CACHE_DIR per case. warn_once_daily is rate-limited per
  # kind per agent per day, so a reused cache dir makes a fixed binary read as unfixed.
  i=0
  for p in '{"tool_input":{"command":"cargo test\ncargo build"}}' \
           '{"tool_input":{"command":"cargo test"}}' \
           '{"tool_input":{"command":"cargo test\n"}}'; do
    i=$((i+1)); C="$T/c$i"
    printf '%s' "$p" | SKIM_DISABLE_ANALYTICS=1 SKIM_CACHE_DIR="$C" \
      "$B" rewrite --hook --agent claude > "$T/o$i" 2>"$T/e$i"
    printf 'case%s rc=%s stdout=%-3s stderr=%-3s cache=[%s]\n' "$i" "$?" \
      "$(/usr/bin/wc -c <"$T/o$i")" "$(/usr/bin/wc -c <"$T/e$i")" \
      "$(/bin/ls -A "$C" 2>/dev/null | /usr/bin/tr '\n' ' ')"
    /bin/cat "$T/o$i"; echo
  done
  # case1 = PRECONDITION (1): an INTERIOR newline. case2 = positive control (rewritten).
  # case3 = trailing-newline control, which command_needs_passthrough trims first and so
  # exercises the OPPOSITE branch — it must be rewritten, not bailed.
  # the bail is SILENT, not merely debug-gated
  printf '%s' '{"tool_input":{"command":"cargo test\ncargo build"}}' \
    | SKIM_DISABLE_ANALYTICS=1 SKIM_DEBUG=1 SKIM_CACHE_DIR="$T/c4" \
      "$B" rewrite --hook --agent claude 2>&1 >/dev/null | /usr/bin/wc -c
  /bin/ls -A "$T/c4" 2>/dev/null
  ```
- **BEFORE (measured at c2b4378):** **DEFECT REPRODUCED.** Interior-newline payload:
  rc 0, stdout **0 B**, stderr **0 B**, and `ls -A $SKIM_CACHE_DIR` printed
  **nothing** — no `hook.log`, no stamp, no sidecar. The bail leaves no trace anywhere.
  **Positive control** `{"tool_input":{"command":"cargo test"}}`: rc 0, stdout **99 B**
  carrying the rewritten `updatedInput` — so the 0 B above is the bail, not a broken
  harness.
  **Trailing-newline control** (`"cargo test\n"`): rc 0, stdout **99 B**, rewritten —
  confirming `trim_end` is what separates the interior case from the trailing one, and
  that precondition (1) is load-bearing.
  `SKIM_DEBUG=1` adds only the 122 B provenance line, so **the bail is silent, not
  debug-gated** — it is not reachable even by opting in to diagnostics.
- **AFTER (required):** the bail records a rate-limited signal in `hook.log` via
  `warn_once_daily` — **not** a per-occurrence log. A multi-line-bundle bail is potentially
  per-command on a bundling agent, and a per-occurrence entry would grow `hook.log`
  without bound and drown the signal it exists to carry. Frame this in the commit message
  as a **partial mitigation** of `#337` (full composed-command support), not as its
  closure.
- **Regression test:** `rskim` · `tests/cli_hook_decline_signal.rs::{test_multiline_bail_logs_rate_limited_signal_to_hook_log`
  (`:103`), `test_multiline_bail_writes_nothing_to_stderr` (`:185`),
  `test_rewritten_commands_log_no_decline_signal` (`:220`)`}` ·
  `cargo build -p rskim && cargo nextest run -p rskim --test cli_hook_decline_signal -j 4`.
  **The second of these closes a real gap: no pre-existing test constrained stderr on the
  bail path**, so a later change that made the decline chatty would have been caught by
  nothing — and a chatty bail is a worse defect than a silent one, because it taxes every
  command an agent bundles.
  **Corrected against the working tree:** the plan named
  `src/cmd/rewrite/hook.rs::{test_interior_newline_bail_warns_once, test_interior_newline_bail_second_run_is_silent}`
  — **neither exists**, and the line it cites for its neighbour (`:815`) has moved. The
  work landed at the integration layer instead, which is the correct one: the signal's
  observables are a file inside `SKIM_CACHE_DIR` and the *absence* of stderr, neither of
  which a unit test on `hook.rs` can see. The rate limiter itself keeps its unit pins,
  `test_warn_once_daily_creates_kind_stamped_file` (`hook.rs:899`) and
  `test_warn_once_daily_distinct_kinds_use_distinct_stamps` (`:916`).
- **Re-bless:** none
- **Status:** LANDED

---

## F11 — `--debug` / `--passthrough` honoured after a positional argument

- **Defect:** skim's flag zone is computed with a `take_while` that stops at the first
  token not starting with `-`. For a file operation the first positional is the *file*, so
  every skim flag after it is silently ignored: `skim file.ts --debug` does not enable
  debug, and `skim file.ts --passthrough` does not pass through. The flag is accepted
  without complaint and does nothing.
- **Root cause:** `crates/rskim/src/main.rs:942` —
  `let skim_flag_zone: Vec<&String> = pre_sep.iter().take_while(|a| a.starts_with('-')).collect();`
  (the file's only `take_while`). `pre_sep` is bound at `:939` from the `--` separator
  position at `:935-938`; the zone is consumed by the `--debug` scan at `:945` and the
  `--passthrough` latch below it. `resolve_invocation` is at `:107` and already contains
  the correct zone logic in its own loop; `first_positional` and `is_subcommand_token` do
  not yet exist.
- **Precondition:** the flag must appear after a positional that is **not** a known
  subcommand — i.e. a file-op invocation. The zone rule ends at a known subcommand, so a
  fixture like `skim grep -e --passthrough f` exercises the *unchanged* branch and passes
  identically before and after the fix.
- **Acceptance (argv):** `--no-cache` is deliberately **not** used here: it is a skim
  flag, so inserting it perturbs the very argv under test. Cache isolation is done with a
  fresh `SKIM_CACHE_DIR` instead, which leaves the flag zone untouched.

  ```bash
  cd /Users/dean/Sandbox/skim-issues
  B="$PWD/target/skim-baseline-c2b4378"; T="$(/usr/bin/mktemp -d)"
  F="$T/f.ts"   # a 719 B TypeScript fixture
  /usr/bin/wc -c "$F"
  # PRECONDITION: the flag must follow a positional that is NOT a known subcommand, i.e. a
  # file op. `skim grep -e --passthrough f` puts a known subcommand first, giving an empty
  # zone, and exercises the UNCHANGED branch — it passes identically before and after.
  n=0
  run() { n=$((n+1)); SKIM_DISABLE_ANALYTICS=1 SKIM_CACHE_DIR="$T/cache$n" "$B" "$@" \
            > "$T/o$n" 2>"$T/e$n"
          printf '%-46s rc=%s stdout=%-4s stderr=%-3s exe=%s\n' "$*" "$?" \
            "$(/usr/bin/wc -c <"$T/o$n")" "$(/usr/bin/wc -c <"$T/e$n")" \
            "$(/usr/bin/grep -c 'exe=' "$T/e$n")"; }
  run "$F"                                   # ctl
  run --passthrough "$F"                     # flag BEFORE the positional — honoured
  run "$F" --passthrough                     # DEFECT shape 1
  run --mode structure --passthrough "$F"    # DEFECT shape 2 (space form ends the zone)
  run --mode=structure --passthrough "$F"    # `=` form keeps the zone open — already worked
  run "$F" --debug                           # DEFECT — no `exe=` provenance line
  # the decisive comparison: `--passthrough` after the positional == no flag at all
  /usr/bin/cmp "$T/o3" "$T/o1"
  ```
- **BEFORE (measured at c2b4378):** **TWO DEFECT SHAPES REPRODUCED, plus one arm that
  already worked.** Raw fixture 719 B.

  | argv | stdout | verdict |
  |---|---|---|
  | `ctl f.ts` | 236 B / stderr 77 B / no `exe=` | control |
  | `--passthrough f.ts` | **719 B** | honoured |
  | `f.ts --passthrough` | **236 B**, rc 0, no diagnostic | **DEFECT shape 1** |
  | `--mode structure --passthrough f.ts` | **236 B** | **DEFECT shape 2** |
  | `--mode=structure --passthrough f.ts` | **719 B** | already worked |
  | `f.ts --debug` | stderr 77 B, no `exe=` | **DEFECT** |

  **Shape 2 is the one the plan does not predict:** the flag is *before* the positional
  and still ignored, because the space form of `--mode` puts a bare `structure` in
  `pre_sep` and `take_while` stops there. The `=` form keeps the zone open, which is why
  the two spellings diverge.
  **The decisive measurement:** `cmp b.out ctl.out` is **IDENTICAL** — `--passthrough`
  after the positional is not weakened, it is exactly equivalent to passing no flag at all.
  Accepted without complaint, does nothing.
- **AFTER (required):** extract two **pure** helpers — `first_positional` and
  `is_subcommand_token` — from `resolve_invocation`'s existing loop and have
  `resolve_invocation` call them too, so the two implementations cannot drift. Zone rule:
  no positional implies all of `pre_sep`; a positional that is a known subcommand ends the
  zone there; otherwise (a file op) all of `pre_sep`. **Do not hoist `resolve_invocation`
  itself** — it returns owned data and has error paths. Thread safety: both helpers are
  pure functions of an already-materialised `&[String]` and spawn nothing, so the
  `THREADS_SPAWNED` ordering invariant documented at `:950-955` is untouched. This
  preserves security-5 **without amendment**:
  `skim grep -e --passthrough f` has a known subcommand as its first positional, giving an
  empty zone, so `--passthrough` reaches `grep` untouched. `test_security5_passthrough_as_grep_data_arg_not_consumed`
  (`crates/rskim/tests/cli_passthrough_coverage.rs:1304`) must **not** be reverted,
  weakened or amended.
- **Regression test:** `rskim` · `src/main.rs::{test_first_positional_*, test_is_subcommand_token_*}`
  (new) and `rskim` · `tests/cli_passthrough_coverage.rs::passthrough_after_file_positional_is_honoured`
  (new) · `cargo nextest run -p rskim --bins -j 4 -E 'test(/positional/)'` and
  `cargo build -p rskim && cargo nextest run -p rskim --test cli_passthrough_coverage -j 4`
- **Re-bless:** none, but note that `crates/rskim/src/main.rs` is one of `ci.yml`'s
  `SEARCH_PATHS`, so this commit arms the `Search Scoreboard` job. The job is a no-op pass
  for this change (nothing in `rskim-search` or the bench path reaches the flag zone), and
  by C13 the scoreboard is already armed by F1 anyway.
- **Status:** LANDED

---

## F5 — the diff line-number axis convention is documented

- **Defect:** `skim git diff`'s enriched render prints one line-number column, and which
  axis that number belongs to — old or new — is never stated. The convention is
  recoverable but undisclosed: a reader cannot tell from the output which side a number
  indexes, and has to read the renderer to find out.
- **Root cause:** documentation only — `crates/rskim/src/cmd/git/diff/*.rs` help text and
  the mode docs (`README.md`). There is no code defect: `emit_patch_line` already emits
  `-` with the old cursor and `+`/space with the new, so the axis is recoverable from the
  prefix byte that is already on every line. The active tripwire that forecloses the
  alternative is `const TWO_COLUMN_BYTES: usize = 76` at
  `crates/rskim/tests/cli_git_diff_budget.rs:72`, asserted at `:402-406` inside
  `headers_fit_the_raw_budget` (`:377`) with the standing instruction "Do not raise the
  constant to silence this (PF-027)" and the PF-027 note at `:61`.
- **Precondition:** none — this is a docs change with no runtime observable.
- **Acceptance (argv):** no acceptance command can assert a documentation claim. What
  *is* measurable is the undisclosed convention in the render, and the tripwire that
  forecloses the two-column alternative.

  ```bash
  cd /Users/dean/Sandbox/skim-issues
  B="$PWD/target/skim-baseline-c2b4378"; T="$(/usr/bin/mktemp -d)"
  # ($T/fixture = the pinned diff fixture from cli_git_diff_budget.rs; run inside its repo)
  SKIM_DISABLE_ANALYTICS=1 SKIM_PASSTHROUGH=1 /usr/bin/git diff -- "$T/fixture" > "$T/raw"
  SKIM_DISABLE_ANALYTICS=1 "$B" git diff -- "$T/fixture" > "$T/skim" 2>"$T/err"
  printf 'raw=%s skim=%s err=%s\n' "$(/usr/bin/wc -c <"$T/raw")" \
    "$(/usr/bin/wc -c <"$T/skim")" "$(/usr/bin/wc -c <"$T/err")"
  /bin/cat "$T/skim"
  # the number column, in emitted order — the axis is recoverable ONLY from the prefix byte
  /usr/bin/awk '{print substr($0,1,12)}' "$T/skim"
  # legend census: there is none
  /usr/bin/grep -ci 'old line\|new line\|axis\|legend' "$T/skim"
  # the tripwire, and the three measured margins it guards
  /usr/bin/grep -n 'TWO_COLUMN_BYTES\|PF-027' crates/rskim/tests/cli_git_diff_budget.rs
  cargo build -p rskim && cargo nextest run -p rskim --test cli_git_diff_budget -j 4
  ```
- **BEFORE (measured at c2b4378):** **DEFECT REPRODUCED as an undisclosed convention.**
  The render verbatim (SKIM **215 B** vs **278 B** raw, stderr 0 B, rc 0): the number
  column reads **`1, 2, 3, 4, 2, 3, 4, 5, 6, 7`** — `2`, `3` and `4` each appear
  **twice**, and the sequence runs **backward** at the `-4 → +2` boundary — **with no
  legend**. A reader cannot tell from the output which axis any number indexes, and the
  repeats make the obvious guess (one monotonic counter) wrong.
  **Tripwire:** `const TWO_COLUMN_BYTES: usize = 76`; three independent fixtures measure
  margins of **68 / 63 / 54 B**, **all under 76**. So the second column does not fit on
  any of the three, not merely on the one the plan cites — two columns is closed by
  measurement, not by preference.
- **AFTER (required):** the axis convention is stated in the `git diff` help text and in
  the mode docs. **Two columns is a closed decision, not an open option.** The measured
  margin on the pinned fixture is **68 B** against a `TWO_COLUMN_BYTES` of **76 B**, so a
  second column does not fit and the tripwire exists to stop anyone re-opening it by
  raising the constant. This is an *undisclosed-convention* defect, not a fidelity defect —
  say so, and do not touch `TWO_COLUMN_BYTES`, the fixture sizes or the thresholds in
  `cli_git_diff_budget.rs`.
- **Regression test:** none new — a documentation claim has no runtime observable to
  assert. The guard is the **existing** `headers_fit_the_raw_budget`
  (`crates/rskim/tests/cli_git_diff_budget.rs:377`), which must stay green and unmodified;
  verification is TP-8, whose method is **manual**. Scope command, to confirm the tripwire
  was not disturbed: `cargo build -p rskim && cargo nextest run -p rskim --test cli_git_diff_budget -j 4`
- **Re-bless:** none
- **Status:** LANDED

---

## Deferred

Recorded here **so they are not re-litigated.** Each of the three was considered, has a
stated reason for exclusion, and is filed as its own issue rather than absorbed into this
PR. Re-opening one requires answering the reason below, not merely re-raising the symptom.

### D1 — charged class-1 marker on the `git diff` path

`cmd/git/diff/mod.rs` still calls the **uncharged** `apply_to_stderr` shim, so a class-1
disclosure marker on the diff path is not priced into the ADR-001 net-savings verdict the
way `process.rs` prices it via `guardrail::apply_to_stderr_with_notice`. Charging it is
the consistent thing to do and is nonetheless deferred, for a measured reason:
`lossy_view_marker` is roughly **90–110 B**, and the measured headroom on the pinned
fixture is **68 B**. Charging the marker therefore flips that fixture to raw, which trips
`headers_fit_the_raw_budget`'s non-vacuity assertion — the served-vs-raw check at
`cli_git_diff_budget.rs:393-399`, which exists precisely to catch a render that has
suppressed itself.

ADR-011 **class 3** says a marker that does not fit is conformant to serve raw: the marker
is an *input* to view selection, not only an output of it, and it is allowed to tip the
verdict and thereby suppress itself. So the class-3 reading is not a loophole — it is the
designed behaviour, and by that reading charging the marker here would be correct and the
fixture flip would be expected rather than a regression. The reason to defer is not
conformance, it is **reach**: on a 68 B headroom the flip is not confined to one fixture.
Charging a 90–110 B notice against that margin broadly suppresses diff enrichment across
real commits, and a reader who loses AST breadcrumbs on most diffs is worse off than a
reader who keeps them with an undisclosed lossy view — the very trade ADR-003 already
settled when it recorded this view at 2-5x raw on large diffs.

What would settle it is a **flip-rate measurement over roughly 50 real commits**: if
enrichment survives on most of them the charge is affordable and should land; if it does
not, the right move is a cheaper marker or a per-view charging policy, not a bigger budget.
`TWO_COLUMN_BYTES` must not be raised to make either answer come out (PF-027). Until that
measurement exists this stays deferred.

### D2 — Python dataclass field annotations

Pseudo mode strips a dataclass field's annotation (`x: int`) along with module-level
variable annotations (`x: int = 5`), because the two produce **identical parent kinds** —
only ancestor scope distinguishes them. Distinguishing them needs a threaded
`in_class_body` bool, an `@dataclass` decorator check, and a new fixture, and it would
force amending `test_python_pseudo_strips_variable_annotation`
(`crates/rskim-core/src/transform/pseudo.rs:1382`). There is **no reported instance** of
this defect. That combination — speculative benefit against a real amendment to a test
that currently pins correct behaviour — is why it is out. It is not a disagreement about
whether dataclass fields are API surface; they plausibly are. It is a cost judgement,
and it flips if an instance is reported.

### D3 — `grep` resolving to BSD grep rather than the shell's bundled ugrep

Not a fidelity defect in skim's own output: skim faithfully represents the `grep` it
actually ran, and the divergence a reader sees comes from *which* `grep` that was. It is
also structurally outside F2's reach — F2's probe is a PATH probe, and a shell **function**
is invisible to a PATH probe by construction (which is the same limitation F2 documents
in-code). So the machinery this PR is adding cannot detect it, and extending F2 to try
would be extending it past what it can honestly answer. Belongs on `#319`. Proposed shape
there: a `skim doctor` warning that names shadowed tools, since `doctor` is already the
provenance surface and already reports every `skim` on `$PATH` with which one wins.

### Explicitly out of scope (not deferrals)

`head`/`tail` marker-inside-N (ADR-016 — working as designed), the compressed-vs-raw diff
format switch (`#509` — a deliberate class-2 policy), and the ugrep regex errors
(environmental).

---

## Notes

This section records **pre-existing failures found during the campaign that are
deliberately not fixed here.** Per the plan's failure-and-recovery rule, a failing test
must be classified as a regression or as pre-existing *before* it is debugged — run it at
`origin/main` in a scratch worktree, not on this branch. A pre-existing failure is
recorded here, is not fixed in this PR, and does not gate it; absorbing unrelated repairs
is how a 13-fix batch becomes unreviewable. Also record here any CI red that reproduces at
`origin/main`, with its issue number in backticks.

**(a) `cargo nextest run -p rskim-core` reported `833 passed (1 leaky)` on one run and
`833 passed` on the previous one** — across a **comments-only change that provably
altered no non-comment line** (F1c's doc-comment correction). A leak verdict that moves
while the code under test does not is most likely nextest's timing-sensitive leak
detection rather than a real change in process behaviour. **NOT VERIFIED** — recorded
because a later reader who sees `(1 leaky)` appear once should know it has already been
observed on an inert diff, not so that the hypothesis is treated as established. No issue
filed; it is not a skim behaviour.

**(b) `crates/rskim/tests/cli_git.rs:2043` and `:2057` slice `&stdout[..600]` as a BYTE
index into a `str`.** A 600th byte that is not a UTF-8 char boundary panics — and it
panics **inside the panic message**, so the original assertion failure is replaced by a
`byte index 600 is not a char boundary` panic and the real diagnostic is lost. This is
reachable here and not theoretical: `git log --json` output in this repository carries em
dashes. **Pre-existing at `origin/main`; reported, not fixed**, per the campaign rule
against absorbing unrelated repairs — a 13-fix batch that also fixes test-harness
paper cuts is a batch nobody can review. The repair is `stdout.char_indices()` or
`floor_char_boundary`, in its own commit, on its own branch.
