<!-- trust: trusted -->
# Codebase Research: How much of skim's hook binary-pinning machinery serves only a multi-clone from-source dev setup?

**Date**: 2026-08-19T12:02Z
**Trust**: trusted (local source, git history, GitHub issues, project decision records)
**Files examined**: 12 source files, 4 decision/knowledge records, 9 issues, 1 review summary
**Constraint honoured**: read-only. No cargo build/test/clippy was run. All findings are source-reasoned.

---

## Verdict in one paragraph

**The owner's concern is about 60% correct, and correct about the expensive part.** The
pinned-script *format* — absolute-path `exec`, `[ -x ]` guard, PATH fallback, shell quoting,
version marker — is genuinely end-user justified, is ~110 LOC, and produced **zero** findings
in the 11-reviewer sweep. Everything built on top of it to *compare* pins back — the commit
gate, `pin_is_current()`, the second gate `is_hook_script_current()`, the triplicated
`"unknown"` rule, the four canonical-path derivations, and the staleness-vs-repo-HEAD
section — is ~440 LOC whose unique coverage over plain version currency is a same-version
same-commit two-build scenario that only a from-source developer has. That layer is the
source of **9 of the 11 named blocking items** in the review. One piece of it
(`print_staleness_section`) does not merely fail to help end users — it **actively produces a
false `exit 1` for every npm- and curl-installed user who runs `skim doctor` inside their own
git repository**. That is the owner's hypothesis confirmed in its strongest form.

---

## 1. The surface, quantified

### Production LOC (excluding tests)

| LOC | Piece | Location |
|----:|-------|----------|
| 21 | `resolve_skim_binary()` | `init/helpers.rs:14-34` |
| 8 | `DetectedState` pin/commit fields | `init/state.rs:26-33` |
| 34 | `pin_is_current()` | `init/state.rs:43-76` |
| 36 | `hook_is_current()` | `init/state.rs:78-113` |
| 14 | `detect_state` pin extraction | `init/state.rs:200-213` |
| 8 | `uses_pinned_binary()` (1-line alias) | `init/state.rs:233-240` |
| 16 | `parse_commit_from_script()` | `init/state.rs:410-425` |
| 29 | `parse_binary_pin_from_script()` | `init/state.rs:427-455` |
| 19 | `HookFacts` pin/commit fields | `init/mod.rs:54-72` |
| 11 | `script_has_pinned_marker()` | `init/mod.rs:167-177` |
| 24 | `wrappers_blocks_fast_path()` | `init/install.rs:152-175` |
| 20 | 7-term fast-path gate | `init/install.rs:495-514` |
| 52 | `is_hook_script_current()` | `init/install.rs:827-878` |
| 57 | `create_hook_script()` pin-write block | `init/install.rs:894-950` |
| 92 | `generate_hook_script()` + `shell_single_quote()` | `hooks/mod.rs:495-586` |
| 61 | `hook_status_line()` pin/currency block | `doctor/mod.rs:448-508` |
| 225 | `DriftKind` + `DriftEnv` + `detect_drift` + `log_drift_warnings` | `rewrite/hook.rs:20-244` |
| **727** | **subtotal** | |
| 145 | `print_staleness_section()` (binary vs. repo HEAD) | `doctor/mod.rs:620-757` |
| ~250 | `$PATH` scan: `scan_path_for_skim`, `query_binary_info`, `parse_commit_from_version_output`, `print_path_section` | `doctor/mod.rs:104-356` |
| **~1122** | **total provenance surface** | across 6 modules |

Plus **~40 dedicated unit/E2E tests** (enumerated by name in `init/state.rs`,
`init/install.rs`, `doctor/mod.rs`, `rewrite/hook.rs`, `tests/cli_init.rs`,
`tests/cli_doctor.rs`, `tests/cli_provenance.rs`).

### Distinct predicates: 8, answering 4 overlapping questions

| Predicate | Question answered | Site |
|---|---|---|
| `script_has_pinned_marker` | does the script use F6 format? | `init/mod.rs:173` |
| `uses_pinned_binary` | *(alias of the above)* | `init/state.rs:238` |
| `hook_is_current()` | version + format + commit match? | `init/state.rs:87` |
| `pin_is_current()` | recorded pin == running binary? | `init/state.rs:59` |
| `is_hook_script_current()` | version + format + commit + **pin** match? | `init/install.rs:846` |
| `detect_drift()` (4 kinds) | same four questions, at hook runtime | `rewrite/hook.rs:125` |
| `hook_status_line()` reason chain | same four questions, for display | `doctor/mod.rs:459-497` |
| `wrappers_blocks_fast_path()` | should the fast path be bypassed? | `init/install.rs:169` |

**Four independent canonical-path derivations**, three of which disagree on failure:

```
init/helpers.rs:33    canonicalize(&p).unwrap_or(p)                    // keeps raw path
doctor/mod.rs:96-100  .map(|p| canonicalize(&p).unwrap_or(p))          // keeps raw path
doctor/mod.rs:483-487 .and_then(|p| canonicalize(&p).ok().or(Some(p))) // keeps raw path (new in PR #488)
rewrite/hook.rs:86    current_exe().and_then(canonicalize).ok()        // DISCARDS raw -> None
```

**Three independent copies of the `"unknown"`-commit rule**: `init/state.rs:97-108`,
`init/install.rs:856-863`, `doctor/mod.rs:466-470`.

---

## 2. Classification: who benefits from each piece

### END_USER (keep — this is the real value)

| Piece | Concrete end-user scenario |
|---|---|
| **Absolute-path exec + `[ -x ]` guard + PATH fallback** (`hooks/mod.rs:573-585`) | User runs `cargo install rskim`; binary lands in `~/.cargo/bin/skim`. Claude Code launched from the macOS Dock/Finder inherits a login-shell-less `PATH` without `~/.cargo/bin`. A bare `exec skim rewrite --hook` would fail, and hook mode must never fail loudly (`rewrite/hook.rs` protocol step 3: "on parse/extract failure: exit 0, empty stdout") — so the user gets **silent total non-function** with no error. Pinning the absolute path fixes this outright. Same for nvm-managed npm global bins and `~/.local/bin`. **This piece needs no comparison logic at all.** |
| **`shell_single_quote()`** (`hooks/mod.rs:513-516`) | Any user whose home directory contains a space (`/Users/Jane Doe/`). Without quoting the generated script is syntactically broken. |
| **`export SKIM_HOOK_VERSION` + version terms in the currency predicates** (`state.rs:88`, `install.rs:850-851`) | User on skim 2.9 (pre-F6 bare-exec hook) runs `brew upgrade skim` to 2.11 then `skim init`. The version marker mismatches → script is regenerated → user gains absolute pinning. This is the entire hook-format upgrade mechanism. |
| **`script_has_pinned_marker()` / `uses_pinned_binary()`** (`init/mod.rs:173`) | Same upgrade scenario: migrates pre-F6 scripts. Covered by `test_init_migrates_bare_command_format_to_pinned`. |
| **`DriftKind::HookScriptUnpinned` + `HookVersionMismatch`** (`rewrite/hook.rs:135-143`) | User upgrades skim but never re-runs `skim init`; `hook.log` records why compression looks stale. (Weak — PF-018 itself calls hook.log "a log nobody reads" — but it costs nothing at install time and sits off the install path.) |
| **doctor integrity block: `Tampered` / `Unreadable` / `NoManifest`** (`doctor/mod.rs:399-437`) | PF-016's fix. A package manager or an editor overwrote the hook script; doctor derives its verdict from the independent `.sha256` manifest instead of the tampered bytes. Genuinely end-user protective and unrelated to pinning. |
| **doctor "Running binary" line + `$PATH` scan report** (`doctor/mod.rs:36-52`, `104-356`) | User has `brew install skim` **and** later `cargo install rskim`; two binaries on `$PATH`. Doctor answers "which one wins?" — a real question with a real answer. *(The exit-1 escalation is a different matter — see BOTH below.)* |

### DEV_MULTICLONE (only matters with several coexisting builds)

| Piece | Why it is dev-only |
|---|---|
| **`export SKIM_HOOK_COMMIT` + commit terms** (`state.rs:97-110`, `install.rs:856-863`) | The code **explicitly skips the commit check when the compiled SHA is `"unknown"`** — which is exactly the `cargo install` / crates.io / source-tarball path (`build.rs:7-17` yields `"unknown"` with no `.git`). For released artifacts, the version string already identifies the code: two artifacts at v2.11.0 came from the same commit. The commit gate only discriminates when semver has *not* moved — i.e. unreleased development builds. **#466's own text concedes the harm is report-only**: *"the hook execs `SKIM_HOOK_BINARY` by absolute path, so it does run the newly built binary. Only the recorded provenance string is wrong. No output is mis-compressed as a result."* Its reproduction is literally "binary rebuilt in place" on the dev machine. |
| **`pin_is_current()`** (`state.rs:59-76`) | Its unique coverage over `hook_is_current()` is exactly: *version equal ∧ (commit equal ∨ commit unknown) ∧ path differs*. For shipped artifacts that set is **two builds of identical source at two paths** — behaviourally the same binary. PF-018 states the residual harm precisely: *"`skim doctor` then prints the WRONG pin path on a green exit-0 line."* Since PR #488 the loop is now circular: doctor's `!facts.pin_is_current` term (`doctor/mod.rs:459`) is what turns that into exit 1, so the predicate generates the very failure it then repairs. |
| **`parse_binary_pin_from_script()`** (`state.rs:436-455`) | Sole consumers are the two equality gates and doctor's pin display. |
| **`is_hook_script_current()`** (`install.rs:846-878`) | Every term duplicates `hook_is_current()` + `pin_is_current()` over the *same file* `detect_state` already read (`state.rs:140-141`). It exists only because #470 patched a second gate rather than deleting it. |
| **`DriftKind::BinaryPathMismatch` + `CommitMismatch`** (`rewrite/hook.rs:145-169`) | The `hook.log` remediation strings literally read `run \`./target/release/skim init --yes\`` — a path that exists only in a source checkout. |
| **`print_staleness_section()`** (`doctor/mod.rs:620-757`, 145 LOC) | Shells out to `git cat-file` / `git rev-list` against the *cwd repo*, and its remediation is `cargo build -p rskim --release`. It is unambiguously a source-tree feature. **See §4 — it is worse than useless for end users.** |

### BOTH (end-user scenario stated, but the enforcement half is dev-only)

| Piece | End-user half | Dev-only half |
|---|---|---|
| **`resolve_skim_binary()`** (`helpers.rs:26-34`) | The pin, the wrapper symlink target, and `DetectedState.skim_binary` must agree, or `skim init` churns on symlinked layouts. Per the KB (`:46`) and PF-018's "LANDMINE", this is real and machine-dependent. | But the *canonicalization requirement itself is downstream of the equality gate*. With no gate you would pin whatever `current_exe()` returns and nothing would need to match. **And canonicalizing may be actively wrong for package managers**: on macOS it resolves `/opt/homebrew/bin/skim` → `/opt/homebrew/Cellar/skim/<ver>/bin/skim`, a path `brew upgrade` deletes — so the pin dies at the user's first upgrade and the hook silently degrades to the PATH fallback, i.e. the exact state ADR-004 exists to prevent. *(Platform-dependent — `std::env::current_exe()` already resolves symlinks on Linux via `/proc/self/exe` but not necessarily on macOS. Confidence: Medium; worth an empirical check.)* |
| **doctor `$PATH` drift → exit 1** (`doctor/mod.rs:340-343`) | Reporting two installs is useful. | `[WINS — not the running binary]` setting `drift = true` is wrong for a user who *deliberately* keeps brew + cargo installs. Note: **this term already detects the wrong-clone hazard directly and independently of the hook pin** — which makes the hook-pin equality gate redundant for diagnosis. |
| **doctor `hook_status_line` reason chain** (`doctor/mod.rs:459-497`) | Reporting *why* a hook is stale is useful on any install. | The `!facts.pin_is_current` disjunct escalating to exit 1 is the dev-only half. |
| **7-term fast-path gate** (`install.rs:504-514`) | `guidance_current`, `permissions_blocked`, `manifest_present` are end-user terms and are fine. | `pin_is_current()` and `!wrappers_blocked` are the two terms added by PR #488, and both are implicated in non-convergence (C2, C5, S3). |

---

## 3. What the decision records actually say

**ADR-004** (`.devflow/learning/decisions.md:33-41`) — read in full, not summarised. Its
**Context** field is unambiguous about the originating problem:

> "The hook previously invoked skim in a way that could resolve to whatever `skim` was first
> on $PATH — **which on this machine can be a DIFFERENT/stale clone on a divergent branch**
> (documented recurring hazard in CLAUDE.md 'Commands' and MEMORY.md
> skim-bash-path-resolves-wrong-clone), silently exercising the wrong binary"

and its Consequences call this *"the #1 operational footgun **in this repo**"*.

**So ADR-004's stated motivating problem is a dev-machine problem.** But the *mechanism* it
chose — absolute-path pinning — happens to solve a genuine, unrelated end-user problem (PATH
not containing the install dir in GUI-launched agents). ADR-004 is therefore correct in its
decision and misleading in its rationale: the decision should be re-justified on the
end-user ground, because that is the ground that survives.

The **commit** half of ADR-004 ("plus that build's commit sha", "build-identity handshake so
drift is observable") has no end-user ground at all, and the code concedes it by skipping the
check when the SHA is `"unknown"`.

**ADR-013** (`decisions.md:114`) is the strongest internal precedent for reducing this
machinery. It is a **reversal**, recorded after building and shipping the alternative:

> "skim does NOT inject provenance-drift notices into agent context (REVERSAL, third
> amendment). **Detection stays, delivery becomes on-demand.** … with `skim doctor` as the
> on-demand diagnosis path … Rejected after being built and shipped to a branch: the in-band
> advisory."

The project has already once decided that provenance detection should *report on demand
rather than act automatically*. Demoting the pin from an init-time gate to a doctor advisory
is the same move applied one layer down — consistent with ADR-013, not a departure from it.

**PF-015 / PF-018** are the two pitfalls that produced `pin_is_current()`. Read carefully,
neither claims end-user harm. PF-015's shape is *display-without-gate*; PF-018's stated
consequence is *"`skim doctor` then prints the WRONG pin path on a green exit-0 line"*.
PF-018 also predicted, in advance, exactly what went wrong:

> "THE LANDMINE in fixing #477: three sites produce the 'same' binary path under THREE
> normalization policies … A naive pin-equality gate therefore FALSE-NEGATIVES on every
> Homebrew / cargo-install / symlinked-bin layout and makes `skim init` rewrite the hook on
> EVERY run and churn wrapper symlinks — converting a missing-check bug into an
> infinite-churn bug."

**PR #488 shipped a fourth derivation** (`doctor/mod.rs:483-487`) in the same commit that
declared `resolve_skim_binary()` the single source of truth. PF-018's landmine went off.

**Issue #477** is the origin of `pin_is_current()`, and its own "Observed" section describes
a scenario the predicate **cannot detect**: *"the binary at that path was replaced by a
different build"* — the path is unchanged, so a path-equality check returns `true`. The
predicate does not cover its own motivating report.

---

## 4. The finding that settles it: a dev-only check that fails end users

`print_staleness_section()` (`doctor/mod.rs:704-711`):

```rust
if !commit_exists {
    println!("  ✗  SHA {compiled_commit} not found in this repo — \
             built from a different repository");
    return true;   // -> drift -> exit 1
}
```

Chain of evidence:

1. `crates/rskim/build.rs:7-17` embeds `git rev-parse --short HEAD`, falling back to
   `"unknown"` only when git fails or there is no repo.
2. `.github/workflows/release.yml:110` runs `actions/checkout@v5`, then `:170` runs
   `cargo build --release --target … -p rskim --features proxy` — **inside a git checkout**.
   So every GitHub-Release binary carries a **real** SHA.
3. npm packages (`npm/cli-*`) and the curl installer distribute those same GitHub-Release
   binaries. Those users' `SKIM_GIT_COMMIT` is a real skim SHA.
4. `skim doctor` gates only on `compiled_commit == "unknown"` (`:635`) and
   `--is-inside-work-tree` (`:663`). It never checks whether the repo it is standing in *is
   skim's*.
5. Therefore: **an npm- or curl-installed user who runs `skim doctor` from inside any of
   their own git projects gets `✗ SHA abc1234 not found in this repo — built from a
   different repository` and `Status: DRIFT DETECTED — exit 1`.**

CLAUDE.md sells doctor as *"Exit 0 healthy / 1 on any drift — works as a CI pre-flight."*
This makes it red in the common case, with a message that is both alarming and false, and
whose implicit remedy (`cargo build -p rskim --release`, printed two branches down) the user
cannot perform. *(Source-reasoned, not executed — but the four inputs are each directly
verifiable in-tree. Confidence: High.)*

**This is the owner's hypothesis in its strongest form**: 145 LOC that exist solely for the
multi-clone dev loop, shipped in the binary, degrading the end-user experience.

---

## 5. Cost side, weighed honestly

From `review-summary.md` (11 reviewers, 9 of 11 CHANGES_REQUESTED, scores 5–8/10):

| Review item | Pin machinery? | Reviewers |
|---|---|---|
| C1 `is_module_header_comment` quadratic walk | no | 6 |
| **C2 `pin_is_current` fails closed / `is_hook_script_current` fails open, in series → `skim init` never converges** | **yes** | **7** |
| **C3 fourth hand-rolled canonical-path derivation** | **yes** | **5** |
| C4 new test bypasses `skim_sandboxed` | test *of* the pin fix | 5 |
| C5 `wrappers_blocks_fast_path` ignores `flags.project` | fast-path | 3 |
| **S1 wrapper surface has no pin invariant, by construction** | **yes** | 1 |
| S2 analytics KB unreachable | no | 1 |
| S3 repeat `--wrappers` performs full side-effect set (destroys `settings.json.bak`) | fast-path | 1 |
| **S4 doctor docblock carries the false claim #479 deleted** | **yes** | 2 |
| **S5 `"unknown"`-commit rule triplicated; deleted `else` was the safety net** | **yes** | 1 |
| **S6 gates validate `SKIM_HOOK_BINARY` but the hook execs `$_SKIM_BIN`** | **yes** | 1 |

**9 of 11 named blocking items are pin or fast-path machinery.** Nine of the eleven
"Remaining ≥80%" table rows are too. The two heaviest — C2 and C3 — are both *recurrences of
pitfalls the project already recorded*: C2 is PF-015 defect (3) ("a separate version-only
gate still short-circuited the CLI") one layer down, and C3 is PF-018's landmine, both inside
the PR that cites them by number.

C2 deserves emphasis because it is not a bug, it is a **shape**: two gates answering one
question, wired in series, with opposite degenerate-input polarity, produce a `skim init` that
never converges and a `skim doctor` that stays red forever — with **no operator escape hatch**,
because `flags.force` is parsed (`flags.rs:315, 402`) and **never read anywhere in
`install.rs`** (verified: the only read site in the whole `init/` module is
`uninstall.rs:156`). `skim init --force` today is silently a no-op.

That shape has now recurred three times (#466 → #470 → #477 → #488). Each fix added a
predicate rather than removing one. That is the maintenance cost the owner is sensing, and it
is real.

---

## 6. Recommendation: **(b′)** — a sharpened (b)

> **Keep the pinned-script format and exactly one currency predicate. Delete the second gate
> outright, demote pin equality from an init-time gate to a doctor advisory, gate the
> dev-only staleness check, and wire `--force` as the dev escape hatch.**

This is option (b) with three refinements: it *deletes* `is_hook_script_current()` rather
than aligning it (closing C2 structurally instead of patching it), it fixes the staleness
false-positive that neither (b) nor (c) addresses, and it supplies the cheap dev-only
substitute that step 4 asked for. It stops short of (c) because (c) would take the version
and format terms with it, and those are the hook-upgrade mechanism.

### DELETE

| # | What | Where | Rationale |
|---|---|---|---|
| D1 | **`is_hook_script_current()` entirely** (52 LOC) + its 6 unit tests (`install.rs:1531-1668`). Replace the call site at `install.rs:896` with `state.hook_is_current()`. | `install.rs:827-878, 896` | Every term duplicates a `DetectedState` field parsed from the same file (`state.rs:140-141, 175-186`). Kills C2's two-gates-in-series shape permanently, removes the fail-open/fail-closed divergence, and removes one of the three `"unknown"` copies (S5). This answers architecture's open question ("should `is_hook_script_current` survive at all?") — **no**. |
| D2 | **`state.pin_is_current()` from the fast-path conjunction** (one term). Keep the method and `HookFacts.pin_is_current`. | `install.rs:506` | Its unique coverage is DEV_MULTICLONE (§2). Removing it shrinks the conjunction and removes one of the two hidden-syscall terms complexity flagged. |
| D3 | **The hand-rolled canonical path in `hook_status_line`** | `doctor/mod.rs:483-487` | C3. Thread the value already computed at `doctor/mod.rs:37` in as a parameter — this also restores `hook_status_line`'s purity contract, which is why its three new tests can only assert `line.contains("running:")`. |
| D4 | **The local `commit_ok` derivation in doctor** | `doctor/mod.rs:466-470` | S5's third copy. Consume `HookFacts` instead. |
| D5 | **`_SKIM_BIN` from the generated script**; `exec "$SKIM_HOOK_BINARY"` directly. | `hooks/mod.rs:580-582` | S6: the gates validate `SKIM_HOOK_BINARY` while the script execs `_SKIM_BIN`. One field, one thing to check — the single-source-of-truth the PR is built around, applied to the script itself. |

### CHANGE

| # | What | Where | End-user-visible effect |
|---|---|---|---|
| C-1 | `!facts.pin_is_current` **no longer sets `drift`**. It emits an appended advisory, exactly like `NoManifest`: `⚠ binary pin mismatch (hook: X, running: Y) — run \`skim init --force\` to re-pin`. `!facts.hook_is_current` keeps setting drift. | `doctor/mod.rs:459` | **`skim doctor` stops exiting 1 for a user who has skim installed twice at the same version.** Today it does. The line is still printed. |
| C-2 | `print_staleness_section`: change `!commit_exists` from `return true` to `return false` with a `–` informational line ("SHA not present in this repo — not a source checkout of skim"). | `doctor/mod.rs:704-711` | **npm/curl-installed users stop getting `Status: DRIFT DETECTED — exit 1` when running `skim doctor` inside their own projects.** §4. |
| C-3 | Adopt S3's fix: drop `wrappers_blocks_fast_path` from the conjunction; call `maybe_install_wrappers` *inside* the fast path before returning. | `install.rs:503-514` | Fixes C5 and S3 together; stops a repeat `skim init --wrappers` from destroying the user's pre-skim `settings.json.bak` via unconditional `backup_settings`. Conjunction returns to 5 terms. |
| C-4 | Promote `resolve_skim_binary()` to `pub(crate)` (or move to `cmd/provenance.rs` per architecture's preferred fix) and call it from `doctor/mod.rs:96` and `rewrite/hook.rs:86`. | 3 sites | Collapses 4 derivations to 1 and fixes `rewrite/hook.rs:86`'s `DISCARDS raw → None` divergence, which currently makes hook-time drift fail open on symlinked machines. |

### ADD

| # | What | Where |
|---|---|---|
| A-1 | **Make `flags.force` a term**: `if flags.force { /* skip fast path */ }` in the fast-path condition. | `install.rs:504` |

One line. It is the cheap dev-only substitute step 4 asked for, it closes the "no operator
escape hatch" half of C2, and it fixes a standalone latent bug — `skim init --force` is
parsed today and silently ignored.

### KEEP unchanged

- `generate_hook_script`'s absolute-path exec, `[ -x ]` guard, PATH fallback,
  `shell_single_quote` — the actual end-user value, zero review findings.
- `hook_is_current()` **including its commit term** — one predicate, one site, one copy of the
  `"unknown"` rule. Load-bearing for the dev (re-pin after in-place rebuild, #466), inert for
  tarball users by design. This is where the dev's legitimate need is paid for, cheaply.
- `script_has_pinned_marker`, `parse_version_from_script`, `parse_commit_from_script`,
  `parse_binary_pin_from_script` — all still needed by `detect_state` and doctor reporting.
- The whole `rewrite/hook.rs` drift detector and the `hook.log` channel — read-only, off the
  install path, produced no findings in this review, and covers ADR-013's on-demand model.
- doctor's integrity block (PF-016) and `$PATH` scan — the `$PATH` scan is in fact the *direct*
  wrong-clone detector, which is why the hook-pin equality gate is redundant for diagnosis.

### Net effect

- **≈ −150 production LOC**, **≈ −8 tests**.
- Predicates: 8 → 5. Canonical-path derivations: 4 → 1. `"unknown"`-rule copies: 3 → 1.
- Closes structurally (not by patching): **C2, C3, C5, S1-adjacent, S3, S5, S6**.
- End-user-visible behaviour changes, in full: (1) `skim doctor` no longer exits 1 on
  two-same-version-installs; (2) `skim doctor` no longer exits 1 for npm/curl users in a
  foreign git repo; (3) `skim init --yes` no longer rewrites the hook when only the pin path
  differs — use `--force`; (4) a repeat `skim init --wrappers` stops overwriting
  `settings.json.bak`. Items (1), (2) and (4) are strict improvements; (3) is the deliberate
  trade.

### Why not (a), (c), or "delete it all"

- **(a) Keep as-is** is not defensible. The review findings are not ordinary bugs — C2 and C3
  are the *third* recurrence of a shape the project has already written down twice (PF-015,
  PF-018), and each prior fix added a predicate instead of removing one. Patching again
  guarantees a fourth.
- **(c) Dev-only / env-var** goes too far. The version and format terms in the currency
  predicates are the hook-upgrade mechanism for every user who runs `brew upgrade skim`; an
  env-var-gated pin would take them along or require duplicating them.
- **Delete the pinning outright** would reintroduce the silent-total-failure case for
  GUI-launched agents whose `PATH` lacks `~/.cargo/bin` (§2, END_USER row 1). That failure is
  invisible — hook mode never errors — which makes it the worst kind to reintroduce.

---

## 7. Confidence

| Claim | Confidence | Basis |
|---|---|---|
| Absolute-path pinning is end-user justified | **High** | `hooks/mod.rs:573-585` + `rewrite/hook.rs:296-305` fail-silent protocol |
| Commit pin is dev-only | **High** | `state.rs:97-98` + `install.rs:856-857` skip on `"unknown"`; `build.rs:15`; #466's own text |
| `pin_is_current()` unique coverage is dev-only | **High** | `state.rs:59-76` vs `state.rs:87-113`; #477's Observed section describes a case it cannot detect |
| `print_staleness_section` false-positives for npm/curl users | **High** | `build.rs:7-17` + `release.yml:110,170` + `doctor/mod.rs:635,663,704-711`. Source-reasoned, not executed. |
| `skim init --force` is silently a no-op today | **High** | `grep` over `init/`: parsed at `flags.rs:315,402`; only read site is `uninstall.rs:156` |
| 9 of 11 blocking items are pin/fast-path | **High** | `review-summary.md:68-311` |
| Canonicalization is counterproductive for Homebrew pins | **Medium** | Mechanism-level inference from `helpers.rs:33`; `std::env::current_exe()` symlink behaviour is platform-dependent — needs an empirical check on macOS |
| Recommendation (b′) is the right call | **Medium-High** | Follows from the above plus ADR-013's precedent; the LOC/risk estimates are unverified by compilation |

## 8. Limitations

- **Nothing was executed.** No `cargo build`/`test`/`clippy` (constraint: a serial benchmark
  is running). Every claim is source-reasoned. The same limitation applies to the 11-reviewer
  set this analysis weighs — see its own "⚠ Verification Gap" section.
- **Three claims deserve empirical confirmation before acting**: (1) the staleness
  false-positive, reproducible in ~60s by running a release-workflow binary's `skim doctor`
  inside an unrelated git repo; (2) `current_exe()`'s macOS symlink behaviour for the
  Homebrew-cellar concern; (3) whether deleting `is_hook_script_current()` leaves any
  behaviour uncovered by `hook_is_current()` — the two read the same file but through
  different code paths (`detect_state` caches contents at `state.rs:140-141`;
  `is_hook_script_current` re-reads at `install.rs:847`).
- **Not investigated**: the permissions-seeding subsystem, the wrapper install/uninstall
  symlink logic beyond S1, and `cmd/integrity.rs` internals — all adjacent but out of scope.
- **Population size is unknown.** Nobody knows how many skim end users exist or how they
  install. The end-user scenarios above are mechanism-level (they follow from the code and the
  packaging), not usage-data-backed.
