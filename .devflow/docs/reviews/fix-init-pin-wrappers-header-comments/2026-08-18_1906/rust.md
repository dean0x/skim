# Rust Review Report

**Branch**: fix/init-pin-wrappers-header-comments -> main (PR #488)
**Date**: 2026-08-18 19:06
**Diff**: `git diff main...HEAD`
**Constraint honored**: no cargo build/test/clippy was run; clippy risks are reported for central verification.

---

## Issues in Your Changes (BLOCKING)

### HIGH

**`wrappers_blocks_fast_path` ignores `flags.project` — permanent idempotence break for `skim init --project --wrappers`** — `crates/rskim/src/cmd/init/install.rs:169`, `install.rs:503`, `install.rs:509`
**Confidence**: 92%

- Problem: the new predicate returns `true` for `Some(true)` unconditionally, but the effect it is gating (`maybe_install_wrappers`) is guarded by `if !flags.project` at both call sites (`install.rs:532` dry-run, `install.rs:567` real install). Unlike `--permissions`, `--wrappers` is **not** rejected alongside `--project`: `flags.rs:389` only errors for `permissions == Some(true) && project`. So `skim init --project --wrappers` is accepted, blocks the fast path, falls through to a full `execute_install` (hook script check, manifest rewrite, settings patch, guidance injection), and then installs **zero** wrappers because of the `!flags.project` guard. Every subsequent run repeats it — "Already up to date" becomes unreachable for that flag combination.
- This is precisely the failure mode PF-018 warns about: "an idempotence fast path [is] a predicate over the UNION of effects the command performs" — here the predicate over-approximates the effect set and converts a missing-check fix into churn (`avoids PF-018` is only partially satisfied).
- Fix — mirror the guard that governs the effect:
  ```rust
  fn wrappers_blocks_fast_path(flags: &InitFlags) -> bool {
      match flags.wrappers {
          // Only block when wrapper installation will actually run:
          // maybe_install_wrappers is called under `if !flags.project`.
          Some(true) => !flags.project,
          Some(false) => false,
          None => false, // load-bearing: see doc comment
      }
  }
  ```
  Add a unit test `test_wrappers_blocks_fast_path_some_true_with_project_does_not_block` alongside the three added at `install.rs:2447-2475`. (Alternative: reject `--wrappers + --project` at parse time in `flags.rs` next to the `--permissions` check, and keep the predicate as-is.)

**Unbounded, quadratic backward sibling walk in `is_module_header_comment`** — `crates/rskim-core/src/transform/minimal.rs:292`
**Confidence**: 85%

- Problem: `is_removable_comment` (`minimal.rs:148`) is invoked for **every node** by both walkers — `collect_removable_comments` (`minimal.rs:95`) and pseudo's `collect_noise_ranges` (`pseudo.rs:446`). For a contiguous run of N root-level comment nodes with no blank-line break, comment *k* walks back *k* siblings, so the run costs `N(N-1)/2` `prev_named_sibling()` calls. tree-sitter's previous-sibling lookup is not O(1) — it re-scans the parent's children from the front — so real cost is closer to O(N³) in the size of the run. A 1000-line file that is one contiguous `#`/`--` comment block (generated SQL dumps, vendored license/provenance headers, large commented-out prologues) is a realistic input and blows the stated `<50ms per 1000 lines` budget.
- Aggravating factor: PF-019 documents that `pseudo` is the mode the PreToolUse `cat`/`head`/`tail` rewrite selects (`cmd/rewrite/handlers.rs:44-52`), so this walk sits on the hottest agent-facing path, not a rare one.
- Secondary: the `loop { … }` at `minimal.rs:303` has no explicit upper bound. Termination is safe (the sibling chain strictly decreases), but the project reliability rule is "All loops and retries must have a fixed upper bound".
- Fix: derive the header extent once per transform instead of per node. Compute the end byte of the contiguous root-level comment run at walker setup and pass it down in `CommentWalkContext` / `NoiseWalkContext`; the per-node predicate then becomes `node.parent_is_root && node.end_byte() <= header_end` — O(1) per node, O(N) per file. If a localized change is preferred, add a cheap early-out before the walk (`source[..node.start_byte()]` containing a blank line ⇒ not a header) plus an explicit iteration bound with a documented rationale.

### MEDIUM

**`hook_status_line` hand-rolls a fourth canonical-binary-path derivation instead of `resolve_skim_binary()`** — `crates/rskim/src/cmd/doctor/mod.rs:483`
**Confidence**: 90%

- Problem: the PR's central improvement is unifying binary-path derivation behind `resolve_skim_binary()` (`init/helpers.rs:26`) so `create_hook_script`, `detect_state`, and `maybe_install_wrappers` agree byte-for-byte (`applies ADR-004`). The new pin-mismatch message re-implements the same normalization inline:
  ```rust
  let running = std::env::current_exe()
      .ok()
      .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)))
  ```
  This is the exact shape PF-018 names as the landmine — "three sites produce the 'same' binary path under THREE normalization policies … enumerate every site that PRODUCES that value and unify the derivation first". The PR closed three and opened a fourth in the same change.
- Impact today is display-only (the message could print a path that differs from the one `pin_is_current()` actually compared, giving the user a misleading diagnosis), but the duplicated policy is what future drift will exploit.
- Fix: widen the helper to `pub(crate) fn resolve_skim_binary()`, re-export it from `cmd::init`, and use it here:
  ```rust
  let running = crate::cmd::init::resolve_skim_binary()
      .map(|p| p.to_string_lossy().into_owned())
      .unwrap_or_else(|_| "?".to_string());
  ```
  Also fold the duplicated pinned-path canonicalization (`state.rs:69-71` and `install.rs:870-871` are the same three lines) into a single `canonicalize_or_raw(&Path) -> PathBuf` helper next to it.

**`DetectedState::pin_is_current()` re-resolves the running binary instead of using `self.skim_binary`** — `crates/rskim/src/cmd/init/state.rs:59`
**Confidence**: 90%

- Problem: `detect_state` (`state.rs:121`) already stores `resolve_skim_binary()?` into `DetectedState.skim_binary`. `pin_is_current()` ignores that field and calls `resolve_skim_binary()` again, so the method performs I/O (`current_exe` + `canonicalize`) on every call, is not a pure query over its own struct, and cannot be tested without the ambient process. The unit fixture proves the point: `make_state_with_pin` (`state.rs:1017`) sets `skim_binary: "/usr/local/bin/skim"` — a value the function under test never reads. `hook_facts()` calls it once per agent in `AgentKind::all_supported()`, multiplying the syscalls.
- Fix:
  ```rust
  pub(super) fn pin_is_current(&self) -> bool {
      let Some(pinned) = self.hook_binary_pin.as_deref() else {
          return false;
      };
      let pinned_path = std::path::Path::new(pinned);
      let canon_pinned =
          std::fs::canonicalize(pinned_path).unwrap_or_else(|_| pinned_path.to_owned());
      self.skim_binary == canon_pinned
  }
  ```
  The method then has no `Err(_) => false` arm to reason about (resolution already failed loudly in `detect_state`), and the three unit tests become deterministic instead of environment-dependent.

**`is_hook_script_current` fails OPEN where `pin_is_current` fails CLOSED on the same condition** — `crates/rskim/src/cmd/init/install.rs:868`
**Confidence**: 85%

- Problem: the let-chain
  ```rust
  if let Some(pin) = parse_binary_pin_from_script(&contents)
      && let Ok(running) = super::helpers::resolve_skim_binary()
  ```
  silently falls through to `true` ("script is current") when either `resolve_skim_binary()` returns `Err` or the pin line is present but unparseable. `DetectedState::pin_is_current()` treats the identical conditions as `false` ("stale"). PF-015 instance (3) is exactly this: two independent gates on the same property that disagree, where the unit test on one proves nothing about the other. Here the divergence is masked only because a blocked fast path routes into `create_hook_script`, which errors on `resolve_skim_binary()?` — an accident of ordering, not a design.
- Note also that `script_has_pinned_marker` already passed at `install.rs:857`, so a `None` from `parse_binary_pin_from_script` means the marker exists but the value is malformed — a state that should force a rewrite, not skip one.
- Fix — make the failure explicit and fail closed:
  ```rust
  let Ok(running) = super::helpers::resolve_skim_binary() else {
      return false; // cannot resolve running binary → treat as stale (mirrors pin_is_current)
  };
  let Some(pin) = parse_binary_pin_from_script(&contents) else {
      return false; // marker present but pin unparseable → rewrite
  };
  let pin_path = std::path::Path::new(pin.as_str());
  let canon_pin = std::fs::canonicalize(pin_path).unwrap_or_else(|_| pin_path.to_owned());
  running == canon_pin
  ```

---

## Issues in Code You Touched (Should Fix)

### MEDIUM

**PF-019's structural rule was not applied — the line map is still maintained in parallel** — `crates/rskim-core/src/transform/mod.rs:328`
**Confidence**: 90%

- Problem: PF-019's stated rule is "a derived index must be DERIVED, not maintained in parallel — either re-derive the line map from the transformed content (minimal's strategy) or make every normalization step return its own line delta so the map cannot diverge; never re-implement a text rule a second time against an array." The fix does the opposite: it adds a **second** hand-mirrored rule (`if result.is_empty() { continue; }`, `mod.rs:338-345`) to the parallel array. The next normalization rule added to `trim_and_normalize` (`minimal.rs:439`) will desync the map again, in exactly the same way, on exactly the same hot path.
- The added invariant test (`test_normalize_line_map_invariant_matches_trim_and_normalize`) is a good mitigation but is not the invariant: it covers four hardcoded fixtures, and its own comment documents a live divergence (all-blank input → text has 1 line, map has 0) that is only survivable because `format.rs:58` degrades via `.get(i).copied().unwrap_or(0)`.
- Fix (make the divergence unrepresentable rather than tested): have the single loop that produces the text also produce the kept-line indices, so there is one rule and one implementation:
  ```rust
  /// Returns the normalized text plus, for each output line, its index in `source.lines()`.
  pub(crate) fn trim_and_normalize_with_kept(source: &str) -> (String, Vec<usize>) { … }
  ```
  `trim_and_normalize` becomes `trim_and_normalize_with_kept(source).0`; `normalize_line_map_blanks` becomes an index gather (`kept.iter().map(|&i| line_map[i]).collect()`) and can no longer re-implement a text rule. The trailing-newline restore at `minimal.rs:462-464` then falls out of the same function, closing the documented all-blank divergence too.

**New E2E test bypasses the consolidated sandbox helper (PF-017 second-definition drift)** — `crates/rskim/tests/cli_init.rs:1732`
**Confidence**: 85%

- Problem: `test_init_rewrites_hook_when_pin_path_differs` uses `skim_init_cmd(config)` (`cli_init.rs:17`), which sets only `CLAUDE_CONFIG_DIR` — no `HOME`, no `SKIM_CACHE_DIR`, no `SKIM_DISABLE_ANALYTICS`, no `SKIM_WRAPPERS_DIR`, and none of the `env_remove` calls. It runs a real global `skim init --yes` against the developer's `$HOME` and writes analytics rows to the real `~/.cache/skim/analytics.db`. This is the same PR that consolidated `skim_sandboxed_with_bin` specifically to stop new tests from hand-rolling env blocks (PF-017, carry-forward rule (a)), and the sibling new test 40 lines below (`test_init_wrappers_bypasses_fast_path`) correctly uses `common::skim_sandboxed`. Wrapper deletion is not armed here only because `maybe_install_wrappers(None, …)` early-returns on non-TTY (`install.rs:730-736`) — a single flag change re-arms it.
- Fix: switch to `common::skim_sandboxed(home)` with `fs::create_dir_all(home.join(".claude"))` first (`detect_installed_agents` in override mode requires the dir to exist), and read the hook from `home.join(".claude/hooks/skim-rewrite.sh")`.

**Unit test passes vacuously when it cannot resolve the running binary** — `crates/rskim/src/cmd/init/state.rs:1054`
**Confidence**: 85%

- Problem: `test_pin_is_current_matching_path_returns_true` does `let Some(running_path) = running else { return; };` — a silent green pass with zero assertions if `current_exe()` ever fails. PF-015 instance (2) is about tests that certify nothing; a skip that reads as a pass is the same class.
- Fix: `let running_path = running.expect("current_exe() must resolve in the test environment");` (the module already carries `#[allow(clippy::expect_used)]`). If the `pin_is_current()` refactor above lands, this test needs no ambient process at all and the branch disappears.

---

## Pre-existing Issues (Not Blocking)

None at CRITICAL severity in unchanged code. `crates/rskim` carries no `[lints.clippy]` table (only `rskim-core` denies `unwrap_used`/`expect_used`/`panic`), so the binary crate relies entirely on CI's `clippy --all-features -- -D warnings` for lint enforcement — informational.

---

## Suggestions (Lower Confidence)

- **Unused threaded parameter in test helper** — `crates/rskim-core/src/transform/minimal.rs:495` (Confidence: 75%) — `nth_root_comment(tree, _source, n)` never reads `_source`, yet it is passed at all five call sites. Drop the parameter.
- **`map(..).unwrap_or(false)` reads worse than `is_some_and`** — `crates/rskim-core/src/transform/minimal.rs:300` (Confidence: 70%) — `node.parent().map(|p| p.parent().is_none()).unwrap_or(false)` → `node.parent().is_some_and(|p| p.parent().is_none())`. `clippy::map_unwrap_or` is pedantic-only so this will not fail `-D warnings`, but the codebase already prefers `is_some_and` (e.g. `cli_init.rs:33`).
- **Module-header preservation is unbounded in length** — `crates/rskim-core/src/transform/minimal.rs:292` (Confidence: 70%) — the heuristic preserves the whole contiguous top-of-file comment run with no cap, so a large commented-out prologue survives minimal/pseudo intact. `crates/rskim/tests/cli.rs:404` now asserts a comment literally named `# regular comment` is preserved, which reads as over-broad. Consider capping the preserved header (e.g. first N lines) or requiring a directive-ish prefix, and note the token-reduction impact against the 15-30% minimal-mode target.

---

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 3 | - |
| Should Fix | - | 0 | 3 | - |
| Pre-existing | - | - | 0 | 1 |

**Decisions applied**: `applies ADR-004` (canonical absolute binary pin — `resolve_skim_binary()` unification is correct and Homebrew/cargo-install-safe), `avoids PF-018` (partially — the fast-path predicate now over-approximates for `--project`, and a fourth path-derivation site was introduced), `avoids PF-015` (partially — the two currency gates still disagree on resolution failure), `avoids PF-017` (partially — one new test bypasses the consolidated helper), `avoids PF-019` (partially — the instance is fixed, the parallel-array shape remains).

**Rust Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The core unification (`resolve_skim_binary()`), the tri-state `Option<bool>` handling, the `"unknown"` sentinel treatment in `commit_ok`, and the dead-`"stale"`-arm removal are all correct and well-documented. No panic surfaces were introduced: `source.get(gap_start..gap_end)` at `minimal.rs:315` is multibyte-safe (`str::get` returns `None` on a non-char-boundary rather than panicking), all new `unwrap`/`expect`/`panic!` are inside `#[cfg(test)]` modules carrying the appropriate `#[allow]`, and `resolve_skim_binary()` returns `anyhow::Result` with an actionable hint rather than swallowing the `io::Error`. Let-chains are valid (both crates are `edition = "2024"`), and `wrappers_blocks_fast_path`'s three-arm match will not trip `clippy::match_like_matches_macro` (the non-last arms do not share a single bool value). The blockers above are the `--project` interaction, the quadratic header walk on the pseudo hot path, and three single-source-of-truth regressions.
