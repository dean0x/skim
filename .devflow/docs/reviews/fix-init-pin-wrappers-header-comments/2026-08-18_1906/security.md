# Security Review Report

**Branch**: fix/init-pin-wrappers-header-comments -> main
**PR**: #488
**Date**: 2026-08-18 19:06
**Diff**: `git diff main...HEAD`

## Threat-model note (frames every finding below)

Anyone who can write `~/.claude/hooks/skim-rewrite.sh` already achieves arbitrary code
execution the next time the agent runs a Bash command. So none of the hook-script findings
below are privilege escalations. Their security value is **detection integrity**: the
`.sha256` manifest, `pin_is_current()`, and `skim doctor`'s exit code are marketed as a
CI pre-flight (`Exit 0 healthy / 1 on any drift`, CLAUDE.md). A control that blesses a
divergent script defeats the only mechanism that exists to notice it. Findings are scored
on that basis, not on escalation.

---

## Issues in Your Changes (BLOCKING)

### HIGH

**Quadratic backwards walk in `is_module_header_comment` — untrusted-input DoS that bypasses the existing `MAX_AST_NODES` bound** — `crates/rskim-core/src/transform/minimal.rs:308-330` (wired in at `:155`)
**Confidence**: 90%

- **Problem**: `is_module_header_comment` is called from `is_removable_comment` for **every**
  comment node, and `is_removable_comment` is called once per node by
  `collect_removable_comments` (`minimal.rs:95`) and by pseudo mode
  (`pseudo.rs:446`). For Python/Ruby/SQL/Bash, `is_doc_comment` returns `false`
  (`minimal.rs:174-178, 203-207, 216-219, 220-223`), so the short-circuit chain never
  stops before reaching the new helper. For each root-level comment the helper walks
  **backwards through every preceding sibling** in an unbounded `loop`. Given `N`
  contiguous root-level comment lines with no blank-line break, total work is
  `O(N²)` — and `prev_named_sibling()` is not itself O(1) in tree-sitter (it rescans
  the parent's child list), so the real constant is worse.

- **Why the existing bound does not save it**: `collect_removable_comments` enforces
  `MAX_AST_NODES = 100_000` (`minimal.rs:86-93`) explicitly *"to prevent memory
  exhaustion"* — but the counter is checked **per node visited**, and the new walk runs
  *inside* `is_removable_comment` at `:95`, i.e. before the cap can fire for the
  remaining nodes. The cap therefore bounds `N` at ~100k but bounds total work at
  `100_000² / 2 ≈ 5×10⁹` sibling steps. The module's own security contract ("Enforces
  MAX_AST_NODES to prevent memory exhaustion", `minimal.rs:65-67`) is silently
  downgraded from `O(nodes)` to `O(nodes²)` by this change.

- **Impact**: A `.py` / `.sh` / `.sql` / `.rb` file consisting of a long contiguous run of
  comment lines (no blank lines) hangs skim for minutes instead of degrading via the
  `ComplexityLimit` → lossless-passthrough path the cap was designed to reach
  (`minimal.rs:83-85`). Reachable from any repo an agent reads, including attacker-supplied
  ones; `skim src/` fans out over rayon so several such files pin every core. This blows
  through the stated targets (`<50ms/1000 lines`, `<1s for 100 files`) by orders of
  magnitude. Also violates the project reliability rule *"All loops and retries must have a
  fixed upper bound"*. Note the benchmark gate will not catch it — the blowup only appears
  at large `N`, and fixture files are small.

- **Fix**: The header block is a single contiguous prefix of the root's named children, so
  it can be computed **once per file** instead of re-derived per comment. Compute the
  header's end byte during the existing walk and reduce the predicate to an O(1) compare:

  ```rust
  // Compute once (e.g. cached on CommentWalkContext / the pseudo ctx):
  fn module_header_end_byte(root: Node, source: &str, language: Language) -> usize {
      let mut end = 0usize;
      let mut prev_end: Option<usize> = None;
      for i in 0..root.named_child_count() {
          let Some(child) = root.named_child(i) else { break };
          if !is_comment_node(child.kind(), language) {
              break;
          }
          if let Some(pe) = prev_end
              && let Some(gap) = source.get(pe..child.start_byte())
              && gap.bytes().filter(|&b| b == b'\n').take(2).count() > 1
          {
              break; // blank-line break ends the header block
          }
          end = child.end_byte();
          prev_end = Some(end);
      }
      end
  }

  // Per-node predicate becomes O(1):
  fn is_module_header_comment(node: Node, header_end: usize, language: Language) -> bool {
      matches!(language, Language::Python | Language::Ruby | Language::Sql | Language::Bash)
          && node.parent().map(|p| p.parent().is_none()).unwrap_or(false)
          && node.end_byte() <= header_end
  }
  ```

  This is semantically identical to the current walk and makes total cost `O(nodes)`.
  Also add `.take(2)` to the gap newline count (`minimal.rs:319`) so the scan short-circuits
  instead of counting every newline in the gap.

  Add a regression test with a large contiguous comment prefix (e.g. 50k lines) asserting
  it completes within the benchmark budget — without one this class of defect is invisible
  to the suite (the same "tests at the wrong layer" shape as `PF-015`).

---

## Issues in Code You Touched (Should Fix)

### HIGH

**New pin gate validates `SKIM_HOOK_BINARY` but the hook execs `$_SKIM_BIN` — and the fast path then re-hashes the divergent script into a `Verified` manifest** — `crates/rskim/src/cmd/init/install.rs:864-876` (new) interacting with `:895-914`
**Confidence**: 85%

- **Problem**: `generate_hook_script` writes the binary path into **two** independent shell
  constructs (`crates/rskim/src/cmd/hooks/mod.rs:578-582`):

  ```sh
  export SKIM_HOOK_BINARY={quoted}      # ← the only field any checker parses
  _SKIM_BIN={quoted}
  if [ -x "$_SKIM_BIN" ]; then
    exec "$_SKIM_BIN" rewrite --hook --agent {agent}   # ← the field actually executed
  ```

  Both new gates added by this PR — `is_hook_script_current` (`install.rs:868-876`) and
  `DetectedState::pin_is_current` (`state.rs:59-76`) — parse only `SKIM_HOOK_BINARY` via
  `parse_binary_pin_from_script` (`state.rs:436-455`). Nothing anywhere parses `_SKIM_BIN`.
  A script whose two values diverge passes both new gates while exec'ing a different binary.

- **Impact (detection-control bypass, not escalation)**: the fail-open chain is concrete and
  self-reinforcing:
  1. A `_SKIM_BIN`-only edit changes the file bytes → `classify_script_integrity` → `Tampered`
     → `skim doctor` correctly reports `✗ ... tampered` and advises *"run `skim init --agent
     {agent}` to reinstall"*.
  2. `skim init` calls `is_hook_script_current` (`install.rs:896`), which **does not consult
     the manifest at all**. Version, pinned marker, commit, and `SKIM_HOOK_BINARY` all still
     match → returns `true`.
  3. The early-return branch then calls `compute_file_hash(&script_path)` +
     `write_hash_manifest(...)` on the **on-disk (divergent) bytes** (`install.rs:900-906`),
     prints `Skipped: … (already vX)`, and returns.
  4. `skim doctor` now reports the divergent script as `Verified` / healthy, exit 0.

  Doctor's own remediation advice launders the tamper into a clean bill of health. This is the
  `PF-016` fail-open shape re-entering through a different door, and the `PF-015` "second,
  independent gate" shape: the gate the CLI actually reaches is not the one that was hardened.

- **Note on ownership**: the manifest self-heal at `:895-914` is pre-existing (#471). What this
  PR adds is the pin comparison at `:864-876` — the check whose *entire purpose* is "does the
  hook point at the right binary?" — and it validates a field that is not the one executed.
  That is why this is Should-Fix rather than informational.

- **Fix** (two independent parts, both cheap):

  1. Validate the field that is executed. Either parse `_SKIM_BIN` and require it to equal
     `SKIM_HOOK_BINARY`, or — better — stop duplicating the value in the generated script so
     there is only one field to check:

     ```sh
     export SKIM_HOOK_BINARY={quoted}
     if [ -x "$SKIM_HOOK_BINARY" ]; then
       exec "$SKIM_HOOK_BINARY" rewrite --hook --agent {agent}
     fi
     ```
     Single source of truth in the script mirrors `resolve_skim_binary()` being the single
     source of truth in the code — the same unification this PR is built around.

  2. Make `is_hook_script_current` integrity-aware so the early return cannot bless
     unverified bytes:

     ```rust
     // install.rs, before the "Skipped" early return at :896
     if !matches!(
         crate::cmd::integrity::classify_script_integrity(&state.hook_config_dir,
                                                          state.agent_cli_name),
         ScriptIntegrity::Verified | ScriptIntegrity::NoManifest
     ) {
         // Tampered/Unreadable → fall through and regenerate, never re-hash in place.
     } else if is_hook_script_current(&script_path, &state.skim_version) { … }
     ```
     A `Tampered` script must be **regenerated**, never re-hashed in place. Add a CLI-level
     test (`PF-015` rule: drive the binary, not the predicate) asserting
     `doctor → tampered` ⇒ `init` rewrites ⇒ `doctor → healthy` *with new bytes*.

### MEDIUM

**The two new pin gates disagree when the pin is absent/empty — `is_hook_script_current` fails open while `pin_is_current` fails closed, producing an unfixable doctor/init loop** — `crates/rskim/src/cmd/init/install.rs:868-876` vs `crates/rskim/src/cmd/init/state.rs:59-62`
**Confidence**: 88%

- **Problem**: `script_has_pinned_marker` (`init/mod.rs:173-177`) accepts any line starting
  with `export SKIM_HOOK_BINARY=`, including `export SKIM_HOOK_BINARY=''`.
  `parse_binary_pin_from_script` (`state.rs:449-451`) rejects the empty value and returns
  `None`. The two new gates then diverge:
  - `pin_is_current()` (`state.rs:60-62`): `None` → `false` (**correct** — "no pin recorded →
    treat as stale").
  - `is_hook_script_current()` (`install.rs:868`): `if let Some(pin) = …` — `None` skips the
    whole check and the function returns `true` (**fails open**).

  The same divergence occurs when `resolve_skim_binary()` returns `Err`: `pin_is_current` →
  `false`, `is_hook_script_current` → the `&& let Ok(running)` chain short-circuits → `true`.

- **Impact**: an unpinned-but-marker-bearing script produces a permanent stuck state:
  `run_install_single`'s fast path is blocked (`pin_is_current()` = false, `install.rs:506`)
  → falls through to `create_hook_script` → `is_hook_script_current` = true → prints
  `Skipped: … (already vX)` and re-writes the manifest without regenerating. `skim doctor`
  keeps reporting `binary pin mismatch` and exiting 1 forever, and its advice (`run skim init`)
  never fixes it. This is exactly the `PF-015` "three such gates exist and one was wrong"
  pattern, reintroduced by adding the same logic to two gates with different `None` semantics.

- **Fix**: make the `None`/`Err` cases fail closed, matching `pin_is_current`:

  ```rust
  // install.rs:868-876
  let Some(pin) = parse_binary_pin_from_script(&contents) else {
      return false; // marker present but no usable pin → stale, regenerate
  };
  let Ok(running) = super::helpers::resolve_skim_binary() else {
      return false; // cannot resolve → cannot certify current
  };
  let pin_path = std::path::Path::new(pin.as_str());
  if running != pin_path { return false; }
  ```

  Better still: have `is_hook_script_current` delegate to the same predicate
  `pin_is_current()` uses, so the two gates cannot drift again (`PF-018`: *"before adding an
  equality gate on a derived value, enumerate every site that PRODUCES that value and unify
  the derivation first"* — this PR unified the *producer* via `resolve_skim_binary()` but left
  two independent *comparators*).

**Re-canonicalizing the pin read from the hook script can only loosen the comparison (and adds a symlink-retarget TOCTOU)** — `crates/rskim/src/cmd/init/state.rs:67-72` and `crates/rskim/src/cmd/init/install.rs:871-872`
**Confidence**: 82%

- **Problem**: both new comparators canonicalize the pin value that was read out of the hook
  script before comparing:

  ```rust
  let canon_pinned = std::fs::canonicalize(pinned_path).unwrap_or_else(|_| pinned_path.to_owned());
  running == canon_pinned
  ```

  Per `ADR-004` the pin is *written* as the canonicalized absolute path (`create_hook_script`
  → `resolve_skim_binary()`, `install.rs:938`), so for every legitimately-installed script
  `canonicalize(pin) == pin` and the extra call is a no-op. The **only** inputs whose value
  changes under re-canonicalization are non-canonical pins — i.e. hand-written or
  externally-modified ones, precisely the case the check exists to catch. The inline comment
  ("in case it was recorded as a symlink target that has since been re-targeted") describes a
  state `resolve_skim_binary()` makes unreachable at write time.

- **Impact**: a pin of `/tmp/x` where `/tmp/x → <running binary>` compares **equal**, so
  `pin_is_current()` reports the hook as correctly pinned — while the script execs through
  `/tmp/x`, a path whose target can be swapped at any time after the check (classic
  check-vs-use gap). Combined with the `_SKIM_BIN` finding above, both new comparators
  certify a script that will not necessarily run the certified binary. Low exploitability
  (requires script-write access), but it strictly weakens the control for zero benefit.

- **Fix**: compare the pin **as recorded**, since it is canonical by construction:

  ```rust
  // state.rs:67-72
  running.as_path() == std::path::Path::new(pinned.as_str())
  ```
  and identically at `install.rs:871-872`. If tolerating legacy non-canonical pins is
  genuinely required, do it explicitly and narrowly (accept a match on *either* the raw or
  the canonicalized form only for pins written before the canonicalizing installer shipped),
  rather than silently widening the comparison for all inputs. As a side benefit this removes
  a `stat`/`realpath` syscall on an untrusted path from the `skim init` hot path (a pin on a
  hung network mount currently blocks it).

---

## Pre-existing Issues (Not Blocking)

None at CRITICAL severity. Per the Iron Law, non-critical pre-existing issues in untouched
lines are out of scope for this report. The manifest self-heal at `install.rs:895-914` is
pre-existing but is reported above because the new pin gate is the control that should have
closed it.

---

## Suggestions (Lower Confidence)

- **Unescaped hook-script content rendered into doctor's status line** — `crates/rskim/src/cmd/doctor/mod.rs:488` (Confidence: 68%) — `format!("binary pin mismatch (hook: {pin}, running: {running})")` prints `pin`, read verbatim from the hook script, straight to the terminal. A pin containing ANSI/CSI bytes can rewrite or clear the surrounding doctor report. `ADR-012` ("never filter escape sequences originating from source") governs skim's *reading* role, not its own diagnostic output — this is skim speaking, so escaping or lossy-quoting `pin` here is consistent with that ADR rather than a violation of it. No escalation (writing the script already gives exec), but it can suppress the very drift line meant to be read.

- **New test bypasses the sandbox helper this same PR extracted (`PF-017`)** — `crates/rskim/tests/cli_init.rs:1728` (Confidence: 75%) — `test_init_rewrites_hook_when_pin_path_differs` uses `skim_init_cmd`, which sets only `CLAUDE_CONFIG_DIR` (`cli_init.rs:17-22`), while the PR simultaneously extracts `skim_sandboxed_with_bin` to close a `PF-017` env-leak gap. `HOME`, `SKIM_CACHE_DIR`, `SKIM_WRAPPERS_DIR`, `GEMINI_CONFIG_DIR`, `CODEX_HOME`, and `CRUSH_CONFIG_DIR` are unset, and `resolve_agent` (`flags.rs:262-270`) selects the first agent whose real-`HOME` config dir exists — so on a machine without `~/.claude` but with `~/.gemini`, this test installs into the developer's real `~/.gemini/`. Route it through `common::skim_sandboxed(home)` like the sibling `test_init_wrappers_bypasses_fast_path` already does.

- **Fourth inline copy of the binary-path normalization the PR set out to unify** — `crates/rskim/src/cmd/doctor/mod.rs:483-487` (Confidence: 74%) — re-implements `current_exe() → canonicalize().or(raw)` rather than calling `resolve_skim_binary()`. Behaviourally identical today, so display-only, but `PF-018` names multi-site normalization drift as the specific landmine in this area. Widen `resolve_skim_binary()`'s visibility (`pub(crate)`) and call it here so a future change to the policy cannot desync doctor's rendered "running" path from the path `pin_is_current()` actually compared.

---

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 1 | 2 | - |
| Pre-existing | - | - | 0 | 0 |

**Decisions applied**: `applies ADR-004` (pin is written canonical — the basis for the
re-canonicalization finding); `avoids PF-015` (second-gate divergence between
`is_hook_script_current` and `pin_is_current`; CLI-level rather than predicate-level tests);
`avoids PF-016` (integrity check that blesses rather than rejects on failure);
`avoids PF-017` (new test outside the sandbox helper); `avoids PF-018` (unify the derivation
before adding an equality gate; fast path as a predicate over the union of effects).
`PF-012` was checked and does **not** apply — no compress/raw guard is involved in this diff.

**Security Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

The PR's direction is right — unifying binary-path resolution through `resolve_skim_binary()`
and adding `pin_is_current()` genuinely closes `PF-018`'s two-clone gap, and the
`skim_sandboxed_with_bin` extraction is a real `PF-017` improvement. What blocks it is
one new untrusted-input DoS in `is_module_header_comment` that defeats an existing
resource-exhaustion bound, plus two integrity gates that fail open where their twin fails
closed. All four findings have local, low-risk fixes.
