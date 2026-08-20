# Reliability Review Report

**Branch**: fix/init-pin-wrappers-header-comments -> main
**PR**: #488
**Date**: 2026-08-18 19:06
**Diff**: `git diff main...HEAD` (21 files, +1211 / -158)
**Constraint**: read-only analysis — no cargo build/test/clippy executed

---

## Scope and Method

Reliability lens per `devflow:reliability`: bounded iteration, assertion density in
production code, allocation discipline, indirection depth, plus the project's binding
rules (every loop and retry has a fixed upper bound; assert preconditions in production
code, not just tests) and skim's own MUST — *fail loud with actionable messages, never
silently* — and *tolerate incomplete code (rely on tree-sitter error nodes)*.

Decisions applied: **ADR-002** (count caps degrade to a lossless `--max-lines` path via
`SkimError::ComplexityLimit`, caught at one dispatch point; degrade must be fail-loud),
**ADR-004** (hook pins the absolute canonicalized binary path + commit sha).
Pitfalls checked: **PF-015** (provenance mechanism fails where its own tests cannot see;
"second gate" instance), **PF-016** (integrity check that fails open), **PF-018**
(idempotence fast path as a predicate over the union of effects; "enumerate every site
that PRODUCES a derived value and unify the derivation first").

Four items were interrogated as directed. Verdicts stated up front so the reasoning is
auditable, then the findings.

| Interrogation | Reliability verdict |
|---|---|
| `is_module_header_comment()` backwards walk bounded? | **NO — violates the fixed-upper-bound rule.** Terminates (finite tree, strictly decreasing sibling index — no infinite loop, no cycle even with ERROR nodes), but has no iteration cap and is super-linear. See B-1. |
| Byte slicing panic on multibyte source? | **SAFE — no finding.** `source.get(a..b)` at `minimal.rs:315` returns `Option`, yielding `None` on a non-char-boundary or out-of-range index instead of panicking. CJK/emoji in a comment cannot panic here. One caveat recorded as S-1. |
| `resolve_skim_binary()` failure behaviour across all three call sites | **MIXED.** `current_exe()` failure fails loud with an actionable hint (good, `helpers.rs:27-32`). `canonicalize()` failure degrades **silently** (B-3). The two consumers disagree on failure direction, producing an unrecoverable stuck state (B-2). |
| Doctor exit-code contract (0 healthy / 1 on drift) after `pin_is_current` + removed stale branch | **HONORED for the new branch.** The removed `"stale"` terminal was genuinely dead: doctor's `compiled_version`/`compiled_commit` (`doctor/mod.rs:33-34`) are the same `env!`/`option_env!` constants `hook_is_current()` reads (`state.rs:88,97`), so `commit_ok ∧ version_ok ∧ hook_uses_pinned_binary ⇒ hook_is_current`, and the `else` at `doctor/mod.rs:479` is reachable only via `!pin_is_current`. One pre-existing reports-a-problem-but-exits-0 hole remains, unrelated to the new branch (P-1). |

---

## Issues in Your Changes (BLOCKING)

### HIGH

**B-1. Unbounded, super-linear backwards sibling walk in `is_module_header_comment()`** — `crates/rskim-core/src/transform/minimal.rs:308-331`
**Confidence**: 92%
**Category**: 1 (added lines)

- **Problem**: The walk is a bare `loop` with no iteration cap:

  ```rust
  let mut current = node;
  loop {
      match current.prev_named_sibling() {
          None => return true,
          Some(prev) => { /* gap check */ if is_comment_node(...) { current = prev; } else { return false; } }
      }
  }
  ```

  It terminates — `prev_named_sibling()` strictly decreases the sibling index over a
  finite tree, so no cycle is reachable even through tree-sitter `ERROR` nodes — but it
  has **no fixed upper bound**, which is the binding project rule verbatim. The codebase
  already has the correct pattern for exactly this shape: `MAX_PARENT_WALK: usize = 500`
  in `transform/utils.rs:43`. The new walk does not adopt it.

- **Impact (why this is more than style)**: the per-call cost is not O(1).
  `ts_node__prev_sibling` in tree-sitter 0.25.10 (`src/node.c:190-227`) re-iterates the
  parent's children **from the beginning** on every call (`ts_node_iterate_children` then
  `while (ts_node_child_iterator_next(...)) { if (child.id == self.id) break; ... }`), so
  one `prev_named_sibling()` from sibling index *i* costs O(i). A walk from index *i*
  therefore costs O(i²), and `collect_removable_comments` invokes it once per root-level
  comment, giving **O(N³)** over a contiguous run of N root-level comments. At N=1,000
  that is ~2.5e8 child-iterator steps; at N=5,000, ~3e10. This blows the
  `<50ms / 1000 lines` design target by orders of magnitude on a plausible input: a large
  SQL dump banner, a generated fixture header, or a Bash/Python file opening with a big
  commented-out block containing no blank line (a blank line ends the run, so the trigger
  is specifically an *unbroken* run).

- **`MAX_AST_NODES` does not save this** (confirming the cross-reviewer note, with the
  reliability consequence spelled out): the counter is incremented at `minimal.rs:86` and
  compared at `:87-93`, i.e. **before** `is_removable_comment` runs at `:95`. The cap
  bounds *how many nodes are visited*, never the work performed inside one visit. A file
  of 50,000 contiguous comment lines sits comfortably under the 100,000 cap and never
  trips it — so the ADR-002 degrade path (`SkimError::ComplexityLimit` → lossless
  `--max-lines` passthrough) is never reached and the process simply stalls. That is the
  worst failure mode available here: not a loud degrade, not an error, but a hang, on the
  default read path for Python/Ruby/SQL/Bash. Both `minimal` and `pseudo` are affected —
  `pseudo.rs:446` calls the same `is_removable_comment`.

- **Fix (preferred — removes the quadratic factor as well as the unbounded loop)**:
  compute the header block **once per file** with a single forward pass over the root's
  named children (`next_named_sibling()` is not the re-scanning direction), then make the
  predicate an O(1) byte comparison:

  ```rust
  /// End byte of the contiguous module-header comment run, computed once.
  fn module_header_end(root: Node, source: &str, language: Language) -> usize {
      let mut end = 0usize;
      let mut cur = root.named_child(0);
      while let Some(n) = cur {
          if !is_comment_node(n.kind(), language) { break; }
          if end > 0 {
              // blank-line break ends the header block
              if source.get(end..n.start_byte())
                  .is_some_and(|gap| gap.bytes().filter(|&b| b == b'\n').count() > 1) { break; }
          }
          end = n.end_byte();
          cur = n.next_named_sibling();
      }
      end
  }
  // predicate becomes: is_root_child && node.end_byte() <= header_end
  ```

  Thread `header_end` through `CommentWalkContext` (minimal) and the pseudo `ctx`,
  computing it once alongside `node_count`.

- **Fix (minimum acceptable, if the precompute is deferred)**: give the loop a fixed
  bound in the style already established at `utils.rs:43`, and treat exhaustion as
  "not a header" so behaviour stays conservative:

  ```rust
  /// A module header is never thousands of lines; bound the walk (cf. MAX_PARENT_WALK).
  const MAX_HEADER_WALK: usize = 512;
  let mut steps = 0usize;
  loop {
      steps += 1;
      if steps > MAX_HEADER_WALK { return false; }
      ...
  }
  ```

  Note this caps the loop but leaves the O(i) cost of each `prev_named_sibling()` call
  intact, reducing O(N³) to O(N²) rather than O(N). Prefer the precompute.

---

**B-2. Absent/empty binary pin is a permanent stuck state: `doctor` exits 1 forever recommending a command that cannot repair it** — `crates/rskim/src/cmd/init/install.rs:864-877` vs `crates/rskim/src/cmd/init/state.rs:59-76`
**Confidence**: 88%
**Category**: 1 (both pin checks are added in this diff)

- **Problem**: the two new pin predicates disagree on the *same* two failure inputs, in
  opposite directions. This is a liveness/recoverability defect, not merely an
  inconsistency: the system reaches a state it can report but cannot leave.

  | Input | `pin_is_current()` (state.rs:59-76) | `is_hook_script_current()` (install.rs:868-876) |
  |---|---|---|
  | pin absent / empty value | `None` → `false` (fails **closed**) | `if let Some(pin)` not taken → check skipped → `true` (fails **open**) |
  | `resolve_skim_binary()` errors | `Err(_)` → `false` (fails **closed**) | `let Ok(running)` not taken → check skipped → `true` (fails **open**) |

- **The absent-pin state is reachable and self-consistent.** `script_has_pinned_marker`
  (`init/mod.rs:173-177`) returns `true` for any line beginning
  `export SKIM_HOOK_BINARY=`, while `parse_binary_pin_from_script`
  (`state.rs:436-451`) returns `None` when the parsed value is empty — so
  `export SKIM_HOOK_BINARY=''` yields `hook_uses_pinned_binary = true` **and**
  `hook_binary_pin = None`. The B5b comment at `install.rs:930-933` documents that a
  silent empty path "is exactly the state this whole change exists to eliminate,"
  confirming the state existed in the wild before the `generate_hook_script` assert
  (`hooks/mod.rs:558-562`) was added; any hook installed by such a build still carries it.

- **The resulting loop, traced end to end**:
  1. `skim doctor` — `hook_uses_pinned_binary` is true so the unpinned early return at
     `doctor/mod.rs:449` does not fire; `!pin_is_current` at `:459` fires; version and
     commit both match so the `else` at `:479` produces
     `binary pin mismatch (hook: ?, running: /path/to/skim)`; drift → **exit 1**, advising
     `skim init --yes`.
  2. `skim init --yes` — the fast path at `install.rs:504-511` is blocked by
     `state.pin_is_current()` being `false`, so it correctly falls through to
     `execute_install` → `create_hook_script` (`install.rs:639`, `:880`).
  3. `create_hook_script` at `:896` calls `is_hook_script_current`, which **skips** the
     pin check and returns `true` → prints `Skipped: … (already v2.11.0)` at `:906-911`
     and returns `Ok(())` at `:912`. **The empty pin is never rewritten.**
  4. Go to 1. Forever. `flags.force` is not a term anywhere in `install.rs` outside test
     fixtures, so there is no operator escape hatch short of deleting the script by hand.

- **Impact**: this is the textbook shape of PF-018's warning — "converting a missing-check
  bug into an infinite-churn bug" — inverted into an infinite *non-repair*. It also
  reproduces PF-015 instance (3) exactly: a fix applied to one predicate while a second,
  independent gate between the CLI entry point and that predicate still short-circuits.
  And it breaks skim's fail-loud MUST in the most damaging way available to a
  diagnostic: the tool is loud, but the remedy it names is a no-op, so the user's
  corrective action never converges.

- **Fix**: make both gates fail in the same direction — closed — and make "pin present but
  unusable" a rewrite trigger rather than a skip:

  ```rust
  // install.rs, replacing lines 868-876
  // Absent/empty pin, or an unresolvable running binary, must force a rewrite —
  // matching pin_is_current()'s fail-closed treatment (state.rs:60-62, :74).
  let Some(pin) = parse_binary_pin_from_script(&contents) else { return false; };
  let Ok(running) = super::helpers::resolve_skim_binary() else { return false; };
  let pin_path = std::path::Path::new(pin.as_str());
  let canon_pin = std::fs::canonicalize(pin_path).unwrap_or_else(|_| pin_path.to_owned());
  if running != canon_pin { return false; }
  ```

  Then add a production-side invariant so the two predicates cannot silently desync again
  (the KB names this precise anti-pattern: *"Adding a new required script line but wiring
  it into only one currency predicate"*). A debug assertion at the top of
  `create_hook_script` costs nothing in release and turns a future desync into a test
  failure instead of a stuck user:

  ```rust
  debug_assert!(
      !(state.pin_is_current() && !is_hook_script_current(&script_path, &state.skim_version)),
      "pin currency predicates disagree — a state doctor reports must be repairable by init"
  );
  ```

---

### MEDIUM

**B-3. `resolve_skim_binary()` degrades silently on `canonicalize()` failure, writing an unusable pin into the hook script and wrapper symlinks** — `crates/rskim/src/cmd/init/helpers.rs:33`
**Confidence**: 84%
**Category**: 1 (added line)

- **Problem**: `Ok(std::fs::canonicalize(&p).unwrap_or(p))` swallows the failure with no
  diagnostic on any channel. The `current_exe()` half is exemplary — `map_err` with an
  actionable hint at `:27-32`. The `canonicalize` half has neither an error nor a
  `debug_log!`, so the two halves of one function apply opposite fail-loud policies.

- **Impact**: `canonicalize` fails when the binary was replaced or unlinked while running
  (on Linux `current_exe()` then yields `/path/to/skim (deleted)`), or when any parent
  path component lacks execute permission. The raw fallback path then flows unchecked into
  all three consumers this PR unified:
  - `create_hook_script` (`install.rs:938`) embeds it as `SKIM_HOOK_BINARY`. The
    generated `if [ -x "$_SKIM_BIN" ]` guard always fails, so every hook invocation falls
    through to bare `exec skim` on `$PATH` — **precisely the wrong-clone hazard ADR-004
    exists to eliminate**, silently re-armed. The `generate_hook_script` assert at
    `hooks/mod.rs:558` only rejects the *empty* string, so a non-empty broken path sails
    through.
  - `maybe_install_wrappers` (`install.rs:747`) creates 8 symlinks pointing at the
    non-existent target.
  - `detect_state` (`state.rs:121`) stores it as the comparison basis, so
    `pin_is_current()` then reports mismatch against a path that was never valid.

  The B5b comment directly above the `create_hook_script` call site (`install.rs:930-933`)
  argues that this class of failure "must be loud." Unifying the three sites behind one
  helper (correct, and what PF-018 asks for) also unified them behind one silent fallback.

- **Fix**: keep the fallback (erroring here would regress installs on exotic-but-valid
  layouts) but make the degrade observable, consistent with the ADR-011 taxonomy — this is
  a no-loss internal fallback, so `SKIM_DEBUG`-gated is the right class, plus a
  user-visible note on the install path where the consequence is durable:

  ```rust
  pub(super) fn resolve_skim_binary() -> anyhow::Result<PathBuf> {
      let p = std::env::current_exe().map_err(|e| { /* unchanged */ })?;
      match std::fs::canonicalize(&p) {
          Ok(c) => Ok(c),
          Err(e) => {
              crate::debug_log!("resolve_skim_binary: canonicalize({}) failed: {e}", p.display());
              Ok(p)
          }
      }
  }
  ```

  and in `create_hook_script`, before embedding, assert the invariant that actually
  matters for the pin — that the path is executable — so a broken pin is refused rather
  than installed:

  ```rust
  anyhow::ensure!(
      binary_path.exists(),
      "resolved skim binary {} does not exist — refusing to write an unusable hook pin (ADR-004); \
       hint: reinstall skim, then re-run `skim init`",
      binary_path.display()
  );
  ```

---

**B-4. Doctor re-derives the running binary path inline instead of through the unified resolver — a fourth production site for a value PF-018 required be unified** — `crates/rskim/src/cmd/doctor/mod.rs:483-487`
**Confidence**: 90%
**Category**: 1 (added lines)

- **Problem**: the new pin-mismatch terminal computes its own `running` path:

  ```rust
  let running = std::env::current_exe()
      .ok()
      .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)))
      .map(|p| p.to_string_lossy().into_owned())
      .unwrap_or_else(|| "?".to_string());
  ```

  This is behaviourally identical to `resolve_skim_binary()` **today**, but it is a
  hand-copied fourth derivation of the exact value PF-018 says must have a single
  producer: *"before adding an equality gate on a derived value, enumerate every site that
  PRODUCES that value and unify the derivation first."* The PR unified three sites and
  then added a fourth in the same change.

- **Impact**: reliability-relevant because this string is the *evidence* the operator acts
  on. The comparison that decided drift happened in `pin_is_current()` via
  `resolve_skim_binary()`; the path displayed came from this copy. If the two ever diverge
  (e.g. B-3's fix adds a fallback rule here but not there), doctor prints
  `binary pin mismatch (hook: X, running: X)` — two identical paths and a mismatch verdict
  — which is an unactionable diagnostic. Note this is also the second-definition-drift
  shape PF-017 recorded on the test-harness axis five days ago: *"parameterize the axis
  that varies, or the helper WILL be copied."*

- **Fix**: widen the helper's visibility from `pub(super)` to `pub(crate)` and call it,
  so the displayed path is by construction the compared path:

  ```rust
  let running = crate::cmd::init::helpers::resolve_skim_binary()
      .map(|p| p.to_string_lossy().into_owned())
      .unwrap_or_else(|_| "?".to_string());
  ```

---

## Issues in Code You Touched (Should Fix)

### HIGH

**S-1. The widened fast-path condition makes the manifest self-heal reachable with a *tampered* script, laundering it to `Verified`** — `crates/rskim/src/cmd/init/install.rs:503-511` (added terms) reaching `install.rs:896-912` (existing self-heal)
**Confidence**: 85%
**Category**: 2 (the reachability-widening lines are yours; the self-heal block is not)

- **Problem**: `create_hook_script`'s idempotent early return recomputes the SHA-256 of
  the **script currently on disk** and writes it as the manifest (`:900-905`). It never
  consults `ScriptIntegrity`. `is_hook_script_current` only validates the version comment,
  the `SKIM_HOOK_BINARY` marker, the commit, and the pin — a tamper that preserves those
  four while rewriting the `exec` line passes all of them.

- **What this PR changed**: two new terms were added to the fast-path condition —
  `state.pin_is_current()` at `:506` and `!wrappers_blocked` at `:509`. Each is a new way
  to *fall through* to `create_hook_script` on an otherwise-current install. So
  `skim init --wrappers` (or any two-clone pin mismatch) on a machine with a tampered
  hook script now reaches `:900`, overwrites the good manifest with the tampered script's
  hash, and `skim doctor` reports `Verified` on the next run. Before this diff the fast
  path would have returned at `:512` and left the stale manifest — and therefore the
  `Tampered` verdict — intact.

- **Impact**: this is the PF-016 fail-open shape re-entering through a new door. PF-016's
  transferable rule is that a health check must derive its verdict from an artifact
  *independent* of the one under test; here the remediation command overwrites the
  independent artifact from the artifact under test. The loudest signal in the system is
  silenced by the command doctor tells the user to run. It is Should-Fix rather than
  Blocking only because the self-heal block itself is pre-existing (#471 Group 4).

- **Fix**: gate the self-heal on the manifest being absent or already matching — never
  overwrite a manifest that currently disagrees with the script:

  ```rust
  if is_hook_script_current(&script_path, &state.skim_version) {
      use crate::cmd::integrity::ScriptIntegrity;
      match crate::cmd::integrity::classify_script_integrity(&state.hook_config_dir, state.agent_cli_name) {
          ScriptIntegrity::Tampered => {
              // Do NOT re-bless: fall through and regenerate the script from source.
          }
          _ => { /* existing compute_file_hash + write_hash_manifest + "Skipped" */ }
      }
  }
  ```

---

## Pre-existing Issues (Not Blocking)

### MEDIUM

**P-1. `skim doctor` reports a hook detection failure but exits 0 — the one remaining hole in the exit-code contract** — `crates/rskim/src/cmd/doctor/mod.rs:515-522`
**Confidence**: 91%
**Category**: 3 (unchanged lines; reported because the exit-code contract was explicitly interrogated)

- **Problem**: when `hook_facts(agent)` returns `Err`, the loop prints
  `–  {agent}  (detection error: {e})` with the *neutral* `–` marker and `continue`s
  without setting `any_drift`. Every other channel that cannot answer its question
  (`Tampered`, `Unreadable`, absent pin) is drift; this one is not.

- **Verdict on the interrogation**: the *new* `pin_is_current` branch is correct — it sets
  drift and exits 1, and the removed `"stale"` terminal was provably dead (see the Scope
  table). This hole predates the PR: `detect_state` previously called `current_exe()?` and
  now calls `resolve_skim_binary()?` (`state.rs:121`), an identical failure surface, so
  reachability is unchanged by this diff. Reported informationally because it is the only
  state where doctor prints a problem and still claims `Status: HEALTHY — exit 0`.

- **Fix (separate PR)**: treat undetectable as drift, consistent with PF-016's rule that a
  check which cannot answer must not read as "nothing wrong":
  `println!("  ✗ {} (detection error: {e})", agent.cli_name()); any_drift = true; continue;`

---

## Suggestions (Lower Confidence)

- **`source.get()` returning `None` silently skips the blank-line break check** — `crates/rskim-core/src/transform/minimal.rs:317-322` (Confidence: 70%) — the `&&`-chain means a `None` from `get()` falls through to the `is_comment_node` branch and *continues* the walk, treating an unreadable gap as "no blank line". Panic-safe and conservative (preserves the comment rather than stripping it), so no correctness risk — but it is a silent fallback on a path where `is_go_doc_comment` at `:242-248` uses direct `&source[a..b]` indexing instead. Worth a one-line comment recording that `None` is deliberately treated as "adjacent".

- **Tree-sitter `ERROR` nodes make header preservation silently inconsistent on malformed files** — `crates/rskim-core/src/transform/minimal.rs:301-304` (Confidence: 68%) — when the parser wraps the top of a file in an `ERROR` node, the comments nest one level deeper, `is_root_child` is `false`, and the module header is stripped. No crash and no unbounded work, so it satisfies "tolerate incomplete code" in the safety sense — but the same file parses differently before and after an unrelated syntax error appears, which is the kind of nondeterminism worth a fixture in `tests/fixtures/python/`.

- **Allocation in the walk is already minimal — no finding, recorded for completeness** (Confidence: 75%) — `gap.bytes().filter(...).count()` at `:319` allocates nothing, and `is_module_header_comment` holds one `Node` (a `Copy` value type, no indirection). Allocation discipline and indirection-depth rules are satisfied throughout the diff; `Vec` growth in `collect_removable_comments` is pre-existing and bounded by `MAX_AST_NODES`.

---

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | - |
| Should Fix | - | 1 | 0 | - |
| Pre-existing | - | - | 1 | 0 |

**Reliability Score**: 5/10

Two of the four interrogations resolved clean (byte slicing is genuinely panic-safe; the
doctor exit-code contract is honored for the new branch and the removed `"stale"` arm was
provably dead). The remaining two are real and both are unbounded-or-unrecoverable in the
reliability sense rather than merely stylistic: a `loop` with no cap that `MAX_AST_NODES`
structurally cannot bound because the counter increments before the work (B-1), and a
pair of predicates failing in opposite directions that produces a state the system reports
but cannot exit (B-2). The diff's error handling is otherwise notably disciplined — `?`
propagation on manifest writes, the loud `current_exe()` hint, three shell-safety asserts
in `generate_hook_script`.

**Recommendation**: CHANGES_REQUESTED

Merge-blocking: **B-1** (add the bound — the forward-pass precompute is strongly preferred
over the constant cap, since it fixes the complexity too) and **B-2** (align the two pin
gates fail-closed; without it `skim doctor` can enter a state no documented command
repairs). B-3, B-4 and S-1 are small, localized, and cheapest to fix in this PR while the
context is loaded.
