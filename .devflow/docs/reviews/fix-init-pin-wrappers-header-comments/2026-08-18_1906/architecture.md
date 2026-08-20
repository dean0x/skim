# Architecture Review Report

**Branch**: fix/init-pin-wrappers-header-comments -> main (PR #488)
**Date**: 2026-08-18 19:06
**Focus**: architecture — abstraction boundaries, SOLID, coupling, layering, DI/purity, interception-surface conflation
**Diff**: `git diff main...HEAD` (21 files, +1211/-158)

---

## Architectural Thesis

This PR's stated goal is *unification*: one `resolve_skim_binary()` feeding three
write/compare sites, one new `pin_is_current()` predicate, one new
`wrappers_blocks_fast_path()` gate. Read as a whole, the diff does the opposite of
what it claims at the boundary level.

The provenance concern — *"is the binary that this artifact points at the binary that is
running?"* — is now expressed **five separate times** in three different modules, with
**three different failure semantics** and **no shared type**. `resolve_skim_binary()` is
declared "the single source of truth" in its own doc comment
(`crates/rskim/src/cmd/init/helpers.rs:14-25`) but is `pub(super)` inside a private
`mod helpers` (`crates/rskim/src/cmd/init/mod.rs:20`), so **`cmd::doctor` and
`cmd::rewrite` are physically incapable of calling it**. The claim is unenforceable by
construction, and the diff proves it: `doctor/mod.rs:483-487` hand-rolls a fresh copy.

That is the architectural core of this review. The individual bugs other reviewers found
(fail-open/fail-closed divergence, fourth derivation site, triplicated `"unknown"` rule)
are not five bugs — they are **one missing module**: there is no provenance abstraction,
only a helper trapped behind the wrong visibility wall.

`applies PF-015` — PF-015's rule (3) is *"when a fix targets a predicate, enumerate every
gate between the CLI entry point and that predicate."* This PR did enumerate the gates and
patched each one **independently**, which is the fix PF-015 asks for at the behavioural
level but is the exact structural cause PF-015 describes at the design level. The next
provenance change has to find all five sites again, unaided by the compiler.

---

## Issues in Your Changes (BLOCKING)

### HIGH

**`hook_status_line` breaks its own purity contract — impure env read inside a function documented as pure and testable** — `crates/rskim/src/cmd/doctor/mod.rs:483-487`
**Confidence**: 92%

- **Problem**: `hook_status_line(facts, agent_cli_name, compiled_version, compiled_commit)`
  is designed as a pure function of its four arguments — that is why `compiled_version`
  and `compiled_commit` are *parameters* rather than `env!()` reads, and why the doctor KB
  documents it as "a pure, testable function that produces `(bool, String)`". The new
  pin-mismatch terminal reaches directly into process state:

  ```rust
  let running = std::env::current_exe()
      .ok()
      .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)))
      .map(|p| p.to_string_lossy().into_owned())
      .unwrap_or_else(|| "?".to_string());
  ```

  Three distinct architectural defects in five lines:
  1. **DI violation** — every other environment-derived value in this function arrives as
     a parameter; this one is pulled from the ambient process. The dependency direction
     inverts: a display function now depends on the OS.
  2. **Duplicate of a helper in the same file** — `current_exe_canonical()` at
     `doctor/mod.rs:96-100` is *semantically identical*
     (`.ok().map(|p| canonicalize(&p).unwrap_or(p))` vs
     `.ok().and_then(|p| canonicalize(&p).ok().or(Some(p)))` — the `and_then`/`Some`
     form is a `map` written the long way). It is already called at `doctor/mod.rs:37`
     in the same `run()` flow, ~440 lines earlier, to print the "Running binary" section.
     The value was **already computed** and thrown away.
  3. **Untestable by construction** — the three new unit tests
     (`doctor/mod.rs` test module, `test_hook_status_line_pin_mismatch_*`) can only assert
     `line.contains("running:")`. They cannot assert *which* path is rendered, because the
     function no longer accepts it. This is PF-015 defect (2) verbatim — *"tests
     constructing already-populated values exercise the DISPLAY layer only"* — reproduced
     inside the very fix that cites PF-015 in its own comments.

- **Impact**: The `(facts, …) -> (bool, String)` seam was the one clean boundary in the
  doctor module. It is now leaky: `hook_status_line` cannot be exercised for two clones
  without actually being two clones. `avoids PF-016`'s "derive the verdict from an
  independent artifact" discipline holds for the *verdict*, but the *cause string* now
  comes from an unmanaged source.

- **Fix**: Carry the running path in the DTO that already carries every other fact.
  `HookFacts` (`crates/rskim/src/cmd/init/mod.rs:60-75`) is built by `hook_facts()`
  (`init/mod.rs:98-135`), which is already the impure boundary — it calls
  `detect_state()`, `classify_script_integrity()`, and `pin_is_current()`. Add one field:

  ```rust
  // init/mod.rs — HookFacts
  /// Canonical path of the running binary, as resolved by `resolve_skim_binary()`.
  /// Carried here so `hook_status_line` stays a pure function of its arguments.
  pub(crate) running_binary: Option<PathBuf>,

  // init/mod.rs — hook_facts()
  running_binary: helpers::resolve_skim_binary().ok(),

  // doctor/mod.rs:483-487 — replace the inline derivation
  let running = facts.running_binary.as_ref()
      .map(|p| p.to_string_lossy().into_owned())
      .unwrap_or_else(|| "?".to_string());
  ```

  This kills the fourth derivation site *and* restores purity *and* makes the path
  assertable in the existing unit tests, in one change.

---

**`resolve_skim_binary()` is a "single source of truth" that three of its four consumers cannot reach** — `crates/rskim/src/cmd/init/helpers.rs:14-31`, `crates/rskim/src/cmd/init/mod.rs:20`
**Confidence**: 90%

- **Problem**: The helper's doc comment asserts *"This is the single source of truth for
  the binary path used in hook scripts (`SKIM_HOOK_BINARY`) and wrapper installation so
  that all three sites agree"*. Its signature is
  `pub(super) fn resolve_skim_binary() -> anyhow::Result<PathBuf>` inside
  `mod helpers` — a **private** module declaration (`init/mod.rs:20`). `pub(super)` from
  `cmd::init::helpers` means visible in `cmd::init` and below. Therefore:

  | Site | Module | Can call `resolve_skim_binary()`? | What it actually does |
  |---|---|---|---|
  | `create_hook_script` `install.rs:938` | `cmd::init` | yes | calls it ✓ |
  | `detect_state` `state.rs:121` | `cmd::init` | yes | calls it ✓ |
  | `maybe_install_wrappers` `install.rs:747` | `cmd::init` | yes | calls it ✓ |
  | `pin_is_current` `state.rs:65` | `cmd::init` | yes | calls it ✓ |
  | `is_hook_script_current` `install.rs:869` | `cmd::init` | yes | calls it ✓ |
  | `current_exe_canonical` `doctor/mod.rs:96-100` | `cmd::doctor` | **no** | reimplements |
  | pin-mismatch terminal `doctor/mod.rs:483-487` | `cmd::doctor` | **no** | reimplements |
  | `DriftEnv::from_process` `rewrite/hook.rs:86` | `cmd::rewrite` | **no** | reimplements, **divergently** |

  And the three implementations disagree on failure:

  ```rust
  // init/helpers.rs:26-31   — canonicalize fails → fall back to the RAW path
  Ok(std::fs::canonicalize(&p).unwrap_or(p))
  // doctor/mod.rs:96-100    — canonicalize fails → fall back to the RAW path
  .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
  // rewrite/hook.rs:86      — canonicalize fails → None, RAW PATH DISCARDED
  std::env::current_exe().and_then(std::fs::canonicalize).ok()
  ```

  `rewrite/hook.rs:86` is the value that decides hook-exec-time drift. `SKIM_HOOK_BINARY`
  is written by `resolve_skim_binary()`, which *keeps* the raw path on canonicalize
  failure; `DriftEnv.current_exe` *drops* it and becomes `None`, so the comparison is
  skipped and drift silently fails open on precisely the machines the KB warns about
  (symlinked Homebrew cellar, `/tmp → /private/tmp`).

- **Impact**: DIP violation at module scope. Three modules (`init`, `doctor`, `rewrite`)
  each own a private copy of a cross-cutting domain rule. The PR closed three drift sites
  and opened a fourth in the same commit — a net-zero unification. The compiler cannot
  help: adding a fifth is a two-line change nobody will notice.

- **Fix**: Promote the concept out of `cmd::init` into a module all three consumers can
  depend on — e.g. `crates/rskim/src/cmd/provenance.rs` (sibling of the existing
  `cmd/integrity.rs`, which is exactly this pattern done right: `pub(crate)`, four-state
  enum, consumed by both `doctor` and `rewrite::hook`):

  ```rust
  // cmd/provenance.rs
  pub(crate) fn running_binary() -> anyhow::Result<PathBuf>;   // was resolve_skim_binary
  pub(crate) fn canonical(p: &Path) -> PathBuf;                // one canonicalize policy
  pub(crate) fn commit_matches(hook: Option<&str>, compiled: &str) -> bool; // the "unknown" rule
  ```

  Then `init/helpers.rs`, `doctor/mod.rs:96` and `rewrite/hook.rs:86` all delegate, and
  the `"unknown"`-commit rule stops being triplicated across `state.rs:97-108`,
  `install.rs:856-863`, `doctor/mod.rs:466-470`.

---

**Two predicates, one invariant, opposite verdicts — `pin_is_current()` fails closed, `is_hook_script_current()` fails open, and they are wired in series** — `crates/rskim/src/cmd/init/state.rs:59-77` + `crates/rskim/src/cmd/init/install.rs:864-876`
**Confidence**: 88%

- **Problem**: Both functions answer *"does the recorded pin equal the running binary?"*.
  Both are new in this diff. They disagree on the two edge cases:

  | Condition | `pin_is_current()` (state.rs:60-76) | `is_hook_script_current()` (install.rs:868-876) |
  |---|---|---|
  | pin absent / empty | `return false` — **stale** | `if let Some(pin)` never fires → falls through to `true` — **current** |
  | `resolve_skim_binary()` errors | `Err(_) => false` — **stale** | `if let Ok(running)` never fires → `true` — **current** |
  | pin present and differs | `false` | `false` (agree) |

  They are **wired in series** on the same CLI path:
  `run_install_single` gates the fast path on `state.pin_is_current()`
  (`install.rs:506`), and when that bypasses, `execute_install` (`install.rs:639`) →
  `create_hook_script` → `is_hook_script_current` (`install.rs:896`) decides whether to
  actually rewrite. So the fail-closed predicate opens the door and the fail-open
  predicate closes it again.

  **Concrete non-convergence** (this is the architectural payload):
  `script_has_pinned_marker` (`init/mod.rs:173-177`) matches the literal prefix
  `export SKIM_HOOK_BINARY=` and therefore returns `true` for `export SKIM_HOOK_BINARY=''`.
  `parse_binary_pin_from_script` (`state.rs:436-455`) rejects the empty value and returns
  `None`. Pre-B5b installs produced exactly this script — the `create_hook_script` comment
  at `install.rs:930-934` says so: *"a silent empty path produces an unpinned hook script,
  which is exactly the state this whole change exists to eliminate."* For that population:

  1. `hook_uses_pinned_binary` = true, `hook_binary_pin` = `None`
  2. `hook_is_current()` = true; `pin_is_current()` = **false** → fast path bypassed ✓
  3. `is_hook_script_current()` → version ✓, marker ✓, commit ✓, pin check **skipped** →
     **true** → prints `Skipped: … (already v2.11.x)`, returns without rewriting
     (`install.rs:896-913`)
  4. `skim doctor` → `!facts.pin_is_current` → `✗ binary pin mismatch (hook: ?, running: …)`
     → **exit 1**

  Every subsequent `skim init` repeats step 3. `init` says "already current", `doctor` says
  "drift, run `skim init`". Permanent, self-contradicting, no convergence path short of
  `--force` or deleting the script — for the exact user population this PR exists to repair.

- **Impact**: This is PF-015 defect (3) — *"a separate version-only gate still
  short-circuited the CLI … three such gates exist and one was wrong"* — recurring one
  layer down. The PR fixed the *behaviour* at both gates but preserved the *structure*
  that made PF-015 possible: two implementations of one predicate, updated by hand,
  kept consistent only by the author's memory. `test_init_rewrites_hook_when_pin_path_differs`
  (`cli_init.rs`) covers only the non-empty-wrong-path case, so the divergence is invisible
  to CI.

- **Fix**: Collapse to one predicate with one failure direction (**closed** — a pin you
  cannot verify is not a pin you should trust):

  ```rust
  // state.rs — the single implementation, over parsed contents, not the file
  pub(super) fn pin_matches(pin: Option<&str>, running: Option<&Path>) -> bool {
      match (pin, running) {
          (Some(p), Some(r)) => r == canonical(Path::new(p)).as_path(),
          _ => false,   // absent pin or unresolvable binary → NOT current
      }
  }
  ```

  `DetectedState::pin_is_current()` becomes
  `pin_matches(self.hook_binary_pin.as_deref(), resolve_skim_binary().ok().as_deref())`;
  `is_hook_script_current()` calls the same function on
  `parse_binary_pin_from_script(&contents)`. Then the empty-pin script fails both gates
  and `skim init` converges on the first run.

  **Open question (75%)**: whether `is_hook_script_current` should exist at all once
  `DetectedState` already carries `hook_binary_pin`, `hook_version` and `hook_commit`
  parsed from the same file. It re-reads and re-parses the script that `detect_state`
  already read (`state.rs:175` comments explicitly claim "there is ONE source of truth"
  for those fields). Deleting it in favour of `state`-derived predicates would remove the
  second gate permanently rather than keeping it in sync. Not verified far enough to
  assert; flagging as the structural follow-up.

### MEDIUM

**`is_module_header_comment` opts out of the compiler-enforced language exhaustiveness that every sibling policy function in the file uses** — `crates/rskim-core/src/transform/minimal.rs:292-296`
**Confidence**: 88%

- **Problem**: The new comment-preservation policy dispatches on `Language` with a
  wildcard:

  ```rust
  match language {
      Language::Python | Language::Ruby | Language::Sql | Language::Bash => {}
      _ => return false,
  }
  ```

  Its two siblings in the same file — `is_comment_node` (`minimal.rs:120-139`) and
  `is_doc_comment` (`minimal.rs:163-227`) — both enumerate **every** `Language` variant
  with no `_` arm. That is deliberate: CLAUDE.md's *"Adding a Language"* procedure is
  compiler-guided (add the variant in `types.rs`, then fix every match arm the compiler
  flags). The wildcard silently opts new languages out of module-header preservation with
  zero compiler signal.

  Compounding it: the language set here is a **restatement** of a fact already encoded in
  `is_doc_comment` — Python (`:177`), Ruby (`:206`), SQL (`:218`) and Bash (`:222`) are
  exactly the arms that return `false` ("no doc-comment convention"). The same
  language-policy fact now lives in two functions that must be kept in sync by hand. Same
  disease as findings 2 and 3, in a different crate.

- **Impact**: OCP violation. Adding a 16th tree-sitter language compiles clean and quietly
  strips its module headers in minimal/pseudo. This is the `rskim-core` analogue of the
  hook KB's own anti-pattern: *"Adding a new required script line but wiring it into only
  one currency predicate."*

- **Fix**: Enumerate exhaustively, and derive the set from the existing fact rather than
  restating it:

  ```rust
  fn has_module_header_convention(language: Language) -> bool {
      match language {
          Language::Python | Language::Ruby | Language::Sql | Language::Bash => true,
          Language::TypeScript | Language::JavaScript | Language::Rust | Language::Go
          | Language::Java | Language::C | Language::Cpp | Language::CSharp
          | Language::Kotlin | Language::Swift => false,   // covered by is_doc_comment
          Language::Markdown | Language::Json | Language::Yaml | Language::Toml => false,
      }
  }
  ```

  **Architectural note on the quadratic walk** (confirmed independently by other
  reviewers; I add only the design reading): the O(N³) backwards walk exists *because*
  `is_removable_comment` (`minimal.rs:148-157`) is a per-node predicate with no shared
  state. "Am I in the header block?" is a property of the **file prefix**, not of a node —
  it should be computed **once** per parse (walk root's named children forward until the
  first non-comment or blank-line break; record the byte offset) and then answered by
  `node.start_byte() < header_end`. That reframing is O(N) *and* removes the wildcard
  dispatch problem's blast radius, because the scan happens at one place instead of once
  per comment. The current shape is a policy-per-node abstraction being asked to answer a
  document-scoped question.

---

## Issues in Code You Touched (Should Fix)

### HIGH

**The PATH-wrapper interception surface has no pin-currency concept at all, and `skim doctor` is structurally incapable of reporting wrapper drift** — `crates/rskim/src/cmd/doctor/mod.rs:544-570` + `crates/rskim/src/cmd/init/install.rs:503-514`
**Confidence**: 85%

- **Problem**: This is the surface-conflation finding the review brief asked for, and it
  is an **omission**, not a conflation. The PR touches `maybe_install_wrappers`
  (`install.rs:747`) to route through `resolve_skim_binary()` and adds
  `wrappers_blocks_fast_path` (`install.rs:246-252`) — but the *wrong-clone hazard the
  entire feature exists to eliminate* is only detected on the **hook** surface:

  - Hook surface: `hook_binary_pin` recorded in the script → `pin_is_current()` →
    `HookFacts.pin_is_current` → `hook_status_line` → `✗` → **exit 1**.
  - Wrapper surface: `~/.skim/bin/<tool>` symlinks → **nothing**. `print_wrapper_section`
    (`doctor/mod.rs:544-570`) calls `read_dir`, filters `is_symlink()`, and prints
    `✓ {dir} ({count} symlinks)`. It **never calls `read_link`**, never resolves a target,
    never compares to the running binary. It returns `()` — `doctor/mod.rs:64` invokes it
    with no assignment, unlike `print_hook_section` at `:57` and `print_staleness_section`
    at `:72` which both feed the `drift` flag. **The wrapper section cannot contribute to
    the exit code by construction.**

  So: every symlink in `~/.skim/bin/` can point at clone A's binary while clone B is
  running, and `skim doctor` prints `✓ … (8 symlinks)` and exits `0 HEALTHY`.

  The fast path makes it sticky. With no `--wrappers` flag, `wrappers_blocks_fast_path`
  returns `false` (`install.rs:250`, correctly — that arm is load-bearing), so a
  hook-current install returns at `install.rs:512-513` and never reaches
  `maybe_install_wrappers` at `:568`. Even when the fast path *is* bypassed,
  `maybe_install_wrappers(None, …)` returns early on non-TTY (`install.rs:732-735`).
  Stale wrappers are therefore neither repaired by default nor reported.

- **Impact**: `avoids PF-004` — PF-004 establishes that the wrapper surface and the
  rewrite-engine surface are independent and that a guarantee on one is not a guarantee on
  the other. This diff honours that in the *rewrite* direction (nothing here wrongly
  claims wrapper coverage) but leaves the provenance invariant asymmetric: the surface
  that exists **specifically to intercept sub-agents that bypass hooks** is the surface
  with no provenance check. CLAUDE.md sells `doctor` as *"Exit 0 healthy / 1 on any drift
  — works as a CI pre-flight"*; a whole drift class is outside its reach. A sub-agent
  routed through a stale wrapper executes a different clone's compression logic with zero
  signal on any channel.

  Note this is not purely pre-existing: before this PR the two-clone case hit the fast
  path and *nothing* was repaired; after it, the hook is repaired and the wrapper is not,
  which makes `doctor`'s green wrapper line actively misleading about a state the tool now
  knows about.

- **Fix**: Give the wrapper section the same shape as the hook section — make it a
  drift-returning function reading the same provenance primitive:

  ```rust
  fn print_wrapper_section() -> bool {          // was ()
      // …
      let running = current_exe_canonical();     // ← provenance::running_binary() after finding 2
      let mut stale = 0usize;
      for entry in read_dir(&wrappers_dir)? {
          let target = std::fs::read_link(entry.path()).ok()
              .map(|t| canonical(&t));
          if target.as_deref() != running.as_deref() { stale += 1; }
      }
      if stale > 0 {
          println!("  ✗  {} ({count} symlinks, {stale} pointing at another binary) — \
                    run `skim init --wrappers` to re-point", wrappers_dir.display());
          return true;
      }
      // …
  }
  ```

  and at `doctor/mod.rs:64`: `if print_wrapper_section() { drift = true; }`.
  This is the same `HookFacts.pin_is_current` idea applied to the second surface, and it
  falls out almost free once finding 2's shared module exists.

### MEDIUM

**`check_hook_integrity` narrows a four-state classification to `bool` at the module boundary, so `Unreadable` is unreportable on the only channel hook mode is allowed to use** — `crates/rskim/src/cmd/rewrite/hook.rs:577-609`
**Confidence**: 82%

- **Problem**: The diff correctly *reasons* that `Unreadable` must not suppress drift
  (`hook.rs:601-608`, matching the corrected comment at `hook.rs:355-360`) — that part is
  right and closes the mis-documented coupling. But it preserves the lossy boundary that
  makes the state unobservable. `check_hook_integrity` consumes
  `verify_script_integrity`, the thin `bool` wrapper, so the four-state
  `ScriptIntegrity` enum is collapsed to `Verified|NoManifest|Unreadable → false`,
  `Tampered → true`. Consequences:

  - The `Ok(false)` (Tampered) arm logs via `log_hook_warning` (`hook.rs:594-597`).
  - The `Err(_)` (Unreadable) arm returns `false` and **logs nothing** (`hook.rs:601-608`).
  - `script_path.exists()` is already true at `hook.rs:573`, so `Unreadable` means the
    file exists but cannot be read — permission damage, a real anomaly.

  Meanwhile `cmd::doctor` consumes the **rich** enum (`HookFacts.script_integrity`) and
  treats `Unreadable` as drift with an early return (`doctor/mod.rs:410-418`). Two
  consumers of one classification, one getting the full type and one getting a bool that
  cannot express "could not verify" — an ISP-shaped narrowing at the wrong boundary.

  `applies PF-016`: PF-016's rule is *"an integrity failure must be a distinct LOUD state,
  never an empty result that doubles as 'nothing wrong'"*. `Unreadable → false` is exactly
  an empty result doubling as "nothing wrong" on the hook channel. The PR's own comment
  says the state *"will be visible via `skim doctor`"* — true, but that requires the user
  to independently suspect a problem and run doctor, and `hook.log` is the *only* channel
  hook mode may write to (zero-stderr invariant, and ADR-013 forbids injecting provenance
  notices into agent context). Silence on `hook.log` is the one outcome PF-016 rules out.

- **Impact**: The corrupt-hook signal is silent on the hook channel for one of the two
  failure states, and the asymmetry between doctor's enum and the hook's bool guarantees
  future consumers pick whichever is convenient rather than whichever is correct.

- **Fix**: `classify_script_integrity` is already `pub(crate)` and already used by
  `init/mod.rs::hook_facts`. Call it directly and match all four states — the bool wrapper
  stays for the `uninstall.rs` legacy caller:

  ```rust
  match crate::cmd::integrity::classify_script_integrity(&config_dir, agent_name, &script_path) {
      ScriptIntegrity::Verified | ScriptIntegrity::NoManifest => false,
      ScriptIntegrity::Tampered => { /* existing rate-limited warn */ true }
      ScriptIntegrity::Unreadable => {
          if should_warn_today(&stamp_path) {
              crate::cmd::hook_log::log_hook_warning(&format!(
                  "hook script unreadable (cannot verify integrity): {} \
                   (run `skim init --yes` to reinstall)", script_path.display()));
          }
          false   // still does NOT suppress drift — the corrected #479 semantics
      }
  }
  ```

  Behaviour for drift is unchanged; the state stops being invisible.

---

**The blank-line policy is implemented twice — text and line-map — and the new "invariant" test carves out a divergence rather than eliminating it** — `crates/rskim-core/src/transform/minimal.rs:444-470` + `crates/rskim-core/src/transform/mod.rs:328-359`
**Confidence**: 85%

- **Problem**: `trim_and_normalize` (text) and `normalize_line_map_blanks` (map) must
  encode the *same* blank-line rules or the `--line-numbers` output misattributes source
  lines. They are separate functions with hand-mirrored logic, and this PR is the
  **second** desync in the pair: rule 2 (3+ blanks capped at 2) was mirrored previously;
  rule 1 (leading blanks dropped) is being mirrored now, after shipping a line-number
  corruption bug. Two functions, one reason to change — textbook SRP violation with a
  demonstrated two-incident history.

  The mitigation the PR adds is a good instinct executed as a carve-out.
  `test_normalize_line_map_invariant_matches_trim_and_normalize` (`transform/mod.rs` test
  module) asserts `map.len() == trim_and_normalize(text).lines().count()` over **four
  hardcoded cases**, then immediately documents and asserts a **known violation** of that
  invariant:

  > *"Known divergence: all-blank input → text gets 1 line (trailing-newline restore at
  > minimal.rs:406-408) but map returns []. Harmless: format.rs degrades via
  > `.get(i).unwrap_or(0)`…"*

  An invariant with a documented exception asserted in the same test is not an invariant —
  it is a snapshot. The next transformation rule added to `trim_and_normalize` will pass
  all four cases and desync case five, exactly as this one did.

- **Impact**: The line map is the contract behind `-n` / `--line-numbers`, which agents
  use to cite source locations. Silent misattribution is a correctness failure that looks
  like working output. Coupling here is temporal (must-change-together) with no compiler or
  type-level enforcement.

- **Fix**: Make the map a *byproduct of the one traversal that produces the text*, not a
  replay of it:

  ```rust
  pub(crate) fn trim_and_normalize_with_map(
      source: &str,
      line_map: Option<Vec<usize>>,
  ) -> (String, Option<Vec<usize>>)
  ```

  Both outputs then come from a single loop and cannot disagree by construction;
  `trim_and_normalize(s)` becomes `trim_and_normalize_with_map(s, None).0`. The
  all-blank-input divergence disappears rather than being documented. Keep the invariant
  test — but as a property test over generated blank-line patterns rather than four
  literals.

---

## Pre-existing Issues (Not Blocking)

**`DriftEnv::from_process()` discards the raw path on canonicalize failure while `SKIM_HOOK_BINARY` retains it** — `crates/rskim/src/cmd/rewrite/hook.rs:86`
**Confidence**: 85%

- Not touched by this diff, so informational. `std::env::current_exe().and_then(std::fs::canonicalize).ok()`
  yields `None` when canonicalize fails, whereas `resolve_skim_binary()` (which *wrote*
  the pin being compared against) falls back to the raw path. `check_hook_binary_mismatch`
  skips the comparison when either side is absent, so hook-exec drift detection fails open
  on exactly the symlinked-binary machines the KB flags as the machine-dependent risk.
  Folded into finding 2's fix — a shared `provenance` module makes this arm converge for
  free. Reported here rather than as blocking per the Iron Law.

---

## Suggestions (Lower Confidence)

- **`pin_is_current()` performs filesystem I/O behind a `DetectedState` accessor** —
  `crates/rskim/src/cmd/init/state.rs:59-77` (Confidence: 74%) — every other
  `DetectedState` method is a pure query over already-captured fields; this one calls
  `resolve_skim_binary()` and `canonicalize()` on each invocation, so `state.pin_is_current()`
  at `install.rs:506` is a hidden syscall in what reads as a struct getter, and its result
  can differ between two calls on the same `DetectedState`. Capturing it as a field during
  `detect_state()` (where the other provenance facts are already captured) would keep the
  DTO honest.

- **`wrappers_blocks_fast_path` is scope-blind where its sibling gate is not** —
  `crates/rskim/src/cmd/init/install.rs:246-252` (Confidence: 72%) — it takes only
  `&InitFlags` and ignores `flags.project`, while wrapper installation itself is guarded by
  `if !flags.project` at `install.rs:532` and `:567`, and `permissions_blocks_fast_path`
  takes `(flags, agent, perm_dir)`. So `skim init --project --wrappers` bypasses the fast
  path to perform work that is then unconditionally skipped. The asymmetry with
  `--permissions`, which has an explicit mutual-exclusion guard, suggests the intended
  design is a guard rather than a silent no-op.

- **Module-header preservation is a four-language policy in a fifteen-language transform**
  — `crates/rskim-core/src/transform/minimal.rs:292-296` (Confidence: 70%) — C/C++/Java/TS
  license and SPDX headers (`/* … */`, non-JSDoc) are still stripped in minimal/pseudo
  while Python/Ruby/SQL/Bash ones are preserved. `docs/modes.md` now documents the split,
  so it reads as intentional, but "preserve the file's provenance header" is a
  language-independent user expectation and the per-language divergence will surface as a
  bug report.

---

## Open Questions (unresolved — stated rather than investigated)

1. **Should `is_hook_script_current` survive at all?** (75%) — `DetectedState` already
   carries `hook_version`, `hook_commit`, `hook_binary_pin` parsed from the same file, and
   `state.rs:175` claims single-source-of-truth for them. `is_hook_script_current` re-reads
   and re-parses the script inside `create_hook_script`. Deleting the second gate would
   close the PF-015 recurrence permanently rather than keeping two gates aligned. Not
   traced far enough to assert.

2. **Empty-pin population size** (70%) — the non-convergence in finding 3 requires a hook
   script containing `export SKIM_HOOK_BINARY=''`. The `install.rs:930-934` comment states
   pre-B5b code produced exactly that, but I did not confirm the range of released versions
   affected. The *structural* divergence (fail-open vs fail-closed on one invariant) stands
   regardless of population size.

3. **Wrapper-target staleness in practice** (80%) — I verified `print_wrapper_section`
   never resolves symlink targets and never feeds `drift`. I did not verify whether
   `install_wrappers` re-points existing symlinks when it *is* reached (the `updated`
   counter in `InstallResult` suggests yes). The doctor-side blindness is confirmed either
   way.

---

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | - |
| Should Fix | - | 1 | 2 | - |
| Pre-existing | - | - | 1 | 0 |

**Architecture Score**: 5/10

The individual fixes in this PR are behaviourally correct and well-reasoned — the comments
are unusually good, the pitfalls are cited, and the `"unknown"`-commit and Unreadable-drift
corrections are genuine improvements. The score reflects the boundary work, not the bug
fixes: a PR whose thesis is *unification* ships a fourth derivation of the unified value,
a second implementation of the new predicate with inverted failure semantics, a
triplicated commit rule, and a "single source of truth" helper that three of its consumers
cannot import. The provenance concern needs a module (`cmd/provenance.rs`, modelled on the
existing `cmd/integrity.rs`), not a `pub(super)` helper — and the PATH-wrapper surface
needs the pin invariant it currently lacks entirely.

**Recommendation**: CHANGES_REQUESTED
