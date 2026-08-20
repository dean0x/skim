# Consistency Review Report

**Branch**: fix/init-pin-wrappers-header-comments -> main
**PR**: #488
**Date**: 2026-08-18 19:06
**Diff**: `git diff main...HEAD` (21 files, +1211/-158)

---

## Scope of the consistency lens

Explicitly checked, per the review brief:

| Check | Result |
|-------|--------|
| `wrappers_blocks_fast_path()` mirrors `permissions_blocks_fast_path()` (Option<bool>, ordering, docs) | **Deviates** — see B3, B4 |
| `pin_is_current()` naming/placement matches `hook_is_current()` | Naming consistent; **implementation style deviates** — see B5, B6 |
| New doctor status strings match existing doctor vocabulary / exit-code contract | **Deviates on the value label** — see B2. Exit-code contract (0 healthy / 1 any drift) is honored: `!hook_is_current \|\| !pin_is_current` returns `(true, …)` and `run()` maps `drift → 1`. |
| New stderr notices comply with ADR-011 two-class taxonomy | **Clean** — the diff adds zero `eprintln!`/`debug_log!`/`elision_marker` calls. All new doctor output is stdout; `hook.rs` changes are comment-only. No classification obligation is triggered. |
| `skim_sandboxed_with_bin` consistent with `skim_sandboxed` contract; all shell-out sites use it (PF-017) | Helper contract is **consistent and well-documented**; **one new test bypasses it** — see S3 |

---

## Issues in Your Changes (BLOCKING)

### HIGH

**Fourth hand-rolled canonical-exe resolution, in a file that already has the helper** — `crates/rskim/src/cmd/doctor/mod.rs:483-487`
**Confidence**: 96%

- Problem: the new pin-mismatch branch inlines its own `current_exe()` + `canonicalize()` chain:
  ```rust
  let running = std::env::current_exe()
      .ok()
      .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)))
      .map(|p| p.to_string_lossy().into_owned())
      .unwrap_or_else(|| "?".to_string());
  ```
  This is byte-for-byte equivalent to `current_exe_canonical()` — which lives **in the same file at `doctor/mod.rs:96`** — and semantically equivalent to `resolve_skim_binary()` (`init/helpers.rs:26`), the helper this very PR introduced as the single source of truth. The PR's own stated goal is "`resolve_skim_binary()` unification across state.rs/install.rs", and the KB records the invariant as "the **only** place where the running binary's canonical absolute path is computed". This adds a fifth normalization site, which is exactly the failure mode `PF-018` names ("three sites produce the 'same' binary path under THREE normalization policies … RULE: before adding an equality gate on a derived value, enumerate every site that PRODUCES that value and unify the derivation first"). `avoids PF-018` is only partially satisfied.
- Secondary facet: `hook_status_line` is documented at `doctor/mod.rs:361` as "a pure, testable function", and every other environment-derived input (`compiled_version`, `compiled_commit`) arrives as a **parameter**. Reading `std::env::current_exe()` inside the function body breaks that convention and is why the three new unit tests (`mod.rs:1250-1327`) can only assert `line.contains("running:")` instead of an exact expected path. `run()` already computes `let running_path = current_exe_canonical();` at `doctor/mod.rs:37`.
- Fix: thread the already-computed value through, keeping the function pure and the derivation single-sourced.
  ```rust
  // doctor/mod.rs — run(): running_path is already computed at :37
  let hook_drift = print_hook_section(compiled_version, compiled_commit, running_path.as_deref())?;

  // hook_status_line(..., running_path: Option<&Path>)
  } else {
      let running = running_path
          .map(|p| p.to_string_lossy().into_owned())
          .unwrap_or_else(|| "?".to_string());
      format!("binary pin mismatch (hook: {pin}, running: {running})")
  };
  ```
  If threading is deferred, at minimum call the existing `current_exe_canonical()` rather than re-inlining it.

### MEDIUM

**New status string breaks the reason-chain value vocabulary** — `crates/rskim/src/cmd/doctor/mod.rs:488`
**Confidence**: 90%

- Problem: the three reasons in the same `if/else if/else` chain label the running binary's value differently:
  - `:476` — `commit mismatch (hook: {…}, binary: {compiled_commit})`
  - `:478` — `version mismatch (hook: {…}, binary: {compiled_version})`
  - `:488` — `binary pin mismatch (hook: {pin}, running: {running})` ← `running:` instead of `binary:`

  A user (or a log grep) reading three adjacent doctor lines sees `hook:`/`binary:` twice and `hook:`/`running:` once for the same conceptual slot. Additionally `{pin}` is printed twice on the rendered line, because the enclosing format at `:492-494` already emits `pin: {pin}`:
  `✗ claude-code  installed  pin: /a/b/skim  [binary pin mismatch (hook: /a/b/skim, running: /c/d/skim)]  — run …`
- Fix: match the sibling vocabulary and drop the duplicate:
  ```rust
  format!("binary pin mismatch (running binary: {running})")
  ```
  or, to keep the paired shape, `format!("binary pin mismatch (hook: {pin}, binary: {running})")` and accept the duplication. Either way, pick one label for the running-binary slot across all three reasons. The three new unit tests assert `line.contains("running:")` (`mod.rs:1266`), so they must be updated with whichever label is chosen.

**Wrapper helper filed under the "Permissions install helpers" section banner** — `crates/rskim/src/cmd/init/install.rs:169`
**Confidence**: 92%

- Problem: `install.rs` organizes helpers behind `// ====` section banners. `wrappers_blocks_fast_path` was inserted at `:169`, immediately after the `// Permissions install helpers` banner at `:152-155` and **before** `permissions_blocks_fast_path` (`:184`). A wrapper-domain predicate now sits inside the permissions section, and it displaced the permissions function from the top of its own section. Wrapper helpers otherwise live near `maybe_install_wrappers` / `print_wrapper_install_result` (`:744`, `:778`).
- Fix: either move the function next to `maybe_install_wrappers`, or give it its own banner:
  ```rust
  // ============================================================================
  // Wrapper install helpers
  // ============================================================================

  fn wrappers_blocks_fast_path(flags: &InitFlags) -> bool { … }

  // ============================================================================
  // Permissions install helpers
  // ============================================================================
  ```

**"mirrors `permissions_blocks_fast_path`" is not accurate, and the arm ordering is inverted** — `crates/rskim/src/cmd/init/install.rs:157-174`
**Confidence**: 88%

- Problem: two deviations from the function it claims to mirror.
  1. **Semantics differ on the load-bearing arm.** `permissions_blocks_fast_path`'s `None` arm *can* block — it returns `!protocol.is_current(perm_dir, &entries)` when a sidecar exists (`install.rs:192-204`). `wrappers_blocks_fast_path`'s `None` arm never blocks. The doc body explains *why* wrappers must differ, but the one-line header ("Rule (mirrors `permissions_blocks_fast_path`)") tells a future reader the tri-state rule is the same, which is the opposite of the truth for the only arm that matters.
  2. **Arm ordering is reversed.** Wrappers documents and matches `Some(true) / Some(false) / None`; permissions documents and matches `Some(false) / Some(true) / None`. Two functions presented as a mirrored pair should enumerate the same discriminants in the same order so a side-by-side read is mechanical.
- Fix: align the ordering and correct the claim.
  ```rust
  /// Returns `true` when the "already up to date" fast path must be bypassed
  /// because wrapper installation was explicitly requested.
  ///
  /// Structural counterpart to `permissions_blocks_fast_path`, but the `None`
  /// arm deliberately DIVERGES: permissions auto-updates a stale sidecar on
  /// `None`, wrappers must not block on `None`.
  ///
  /// Rule:
  /// - `Some(false)` → never block (explicit `--no-wrappers`).
  /// - `Some(true)`  → always block (explicit `--wrappers`).
  /// - `None`        → never block — load-bearing. …
  fn wrappers_blocks_fast_path(flags: &InitFlags) -> bool {
      match flags.wrappers {
          Some(false) => false,
          Some(true) => true,
          None => false,
      }
  }
  ```

**`pin_is_current()` re-resolves the binary instead of reading `self.skim_binary`, unlike every other `DetectedState` predicate** — `crates/rskim/src/cmd/init/state.rs:59-77`
**Confidence**: 90%

- Problem: `DetectedState` is a detection *snapshot*, and its sibling predicate `hook_is_current()` (`state.rs:87`) is a pure comparison over already-captured fields. `pin_is_current()` instead calls `super::helpers::resolve_skim_binary()` at predicate-evaluation time — even though this same PR changed `detect_state` (`state.rs:121`) to populate `DetectedState.skim_binary` from **that exact helper**. The struct therefore carries the value the predicate needs and the predicate ignores it.
- Impact beyond style: the field is now dead for its primary consumer, and the new unit fixture advertises a value that does nothing. `make_state_with_pin` (`state.rs:1015`) sets `skim_binary: PathBuf::from("/usr/local/bin/skim")`, but `test_pin_is_current_matching_path_returns_true` (`state.rs:1050`) has to re-derive `current_exe()` to construct a passing pin — a reader comparing the two lines gets contradictory signals about what the predicate reads. It also makes the predicate non-hermetic (result depends on the test *process's* own exe) and re-syscalls on every call.
- Fix:
  ```rust
  pub(super) fn pin_is_current(&self) -> bool {
      let Some(ref pinned) = self.hook_binary_pin else {
          return false; // no pin recorded → treat as stale
      };
      let pinned_path = std::path::Path::new(pinned.as_str());
      let canon_pinned =
          std::fs::canonicalize(pinned_path).unwrap_or_else(|_| pinned_path.to_owned());
      self.skim_binary == canon_pinned
  }
  ```
  The `Err(_) => false` arm disappears because `detect_state` already propagated the resolution failure with `?`. The unit fixture can then set `skim_binary` to a synthetic path and the tests become fully hermetic.

**Pin-path comparison implemented twice, with opposite failure semantics** — `crates/rskim/src/cmd/init/state.rs:59-77` and `crates/rskim/src/cmd/init/install.rs:864-874`
**Confidence**: 90%

- Problem: both blocks answer the same question — "does the script's `SKIM_HOOK_BINARY` pin equal the running binary's canonical path?" — with near-identical code (parse pin → `canonicalize(...).unwrap_or(raw)` → compare to `resolve_skim_binary()`), but they disagree on both degenerate inputs:

  | Condition | `pin_is_current()` (state.rs) | `is_hook_script_current()` (install.rs) |
  |---|---|---|
  | pin absent from script | `false` → stale | pin check **skipped** → stays current |
  | `resolve_skim_binary()` returns `Err` | `false` → stale | pin check **skipped** → stays current |

  Failure scenario: `current_exe()` fails (deleted-while-running, exotic mount). `pin_is_current()` returns `false`, so the fast path at `install.rs:504-511` is bypassed and the user does *not* see "Already up to date"; execution reaches `create_hook_script`, where `is_hook_script_current()` skips the pin check and returns `true`, so the script is **not** rewritten. `skim init` then reports the "Skipped" path for a hook the other predicate just declared stale — two predicates, one question, two verdicts.
- This is the shape `PF-015` calls out ("SECOND GATE — the #466 fix corrected `hook_is_current` … but a separate version-only gate, `is_hook_script_current`, still short-circuited the CLI"), and the KB records the codebase's own remedy for exactly this: `script_has_pinned_marker` is a shared single-source-of-truth scanner used by *both* predicates. The pin comparison should follow that precedent rather than being written twice.
- Fix: extract one helper next to `resolve_skim_binary()` and call it from both sites.
  ```rust
  // init/helpers.rs
  /// Returns `true` when `pin` resolves to the same canonical path as the
  /// running binary. Absent/unresolvable → `false` (fail closed: prefer a
  /// needless rewrite over leaving a wrong pin in place).
  pub(super) fn pin_matches_running(pin: &str) -> bool {
      let Ok(running) = resolve_skim_binary() else { return false };
      let p = std::path::Path::new(pin);
      running == std::fs::canonicalize(p).unwrap_or_else(|_| p.to_owned())
  }
  ```
  `state.rs:59` becomes `self.hook_binary_pin.as_deref().is_some_and(helpers::pin_matches_running)`; `install.rs:864` becomes `if !parse_binary_pin_from_script(&contents).is_some_and(|p| helpers::pin_matches_running(&p)) { return false; }`.

---

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`hook_status_line` doc comment still claims `Unreadable` names the suppression coupling** — `crates/rskim/src/cmd/doctor/mod.rs:371-372`
**Confidence**: 95%

- Problem: the function's docblock states:
  ```
  /// - `Tampered`   → drift (`✗`), names the suppression coupling.
  /// - `Unreadable` → drift (`✗`), names the suppression coupling.
  ```
  This PR removed exactly that claim from the `Unreadable` message (`doctor/mod.rs:412-417`) and added a test asserting it must never reappear (`mod.rs:1002-1008`: "Unreadable message must NOT claim drift detection is silenced"). The docblock 40 lines above now contradicts both the code and its own test, and it is the first thing a maintainer reads. The corresponding `hook.rs:598-608` comment *was* corrected in this PR — the doctor docblock was missed.
- Fix:
  ```rust
  /// - `Tampered`   → drift (`✗`), names the drift-suppression coupling
  ///   (`check_hook_integrity` returns `true`, so `detect_drift` is skipped).
  /// - `Unreadable` → drift (`✗`). Does NOT claim drift is silenced —
  ///   `Unreadable` maps to `integrity_failed = false`, so drift detection
  ///   still runs at hook-exec time.
  ```

**`Mode::Minimal` rustdoc not updated alongside `docs/modes.md`** — `crates/rskim-core/src/types.rs:567` and `:642`
**Confidence**: 88%

- Problem: the behavior change (`is_module_header_comment` preserving Python/Ruby/SQL/Bash module headers) was documented in `docs/modes.md:10` and `:280`, but the canonical in-code descriptions were not:
  - `types.rs:567` — `/// Minimal cleanup - strip non-doc comments, normalize blank lines`
  - `types.rs:642` — `/// - Minimal(1): Strip non-doc comments, ~15-30% reduction`

  `Mode` is public API of the published `rskim-core` crate, so these are the rustdoc surface users see on docs.rs — a stricter statement than the prose doc that *was* corrected. Two doc surfaces for one behavior now disagree.
- Fix: mirror the modes.md wording.
  ```rust
  /// Minimal cleanup - strip non-doc comments (module-header comments in
  /// Python/Ruby/SQL/Bash are preserved), normalize blank lines
  ```
  and `/// - Minimal(1): Strip non-doc comments except module headers, ~15-30% reduction`.

**Two new tests in the same file use two different sandbox harnesses** — `crates/rskim/tests/cli_init.rs:1732` vs `:1785`
**Confidence**: 92%

- Problem: this PR adds two adjacent integration tests for the same subsystem with different hermeticity guarantees:
  - `test_init_rewrites_hook_when_pin_path_differs` (`:1732`) uses `skim_init_cmd(config)` (`cli_init.rs:17`), which overrides **only** `CLAUDE_CONFIG_DIR` — the helper the KB names as the "Known remaining gap" for PF-017.
  - `test_init_wrappers_bypasses_fast_path` (`:1785`) uses `common::skim_sandboxed(home_path)`, the full sandbox.

  Since the PR's own stated deliverable is "`skim_sandboxed_with_bin` extracted to close a PF-017 env-leak gap", adding a **new** shell-out to `skim init --yes` through the un-sandboxed helper in the same change is a direct inconsistency. Concretely, `skim_init_cmd` leaves `HOME`, `SKIM_CACHE_DIR`, `SKIM_WRAPPERS_DIR`, `CODEX_HOME`, `CRUSH_CONFIG_DIR`, `GEMINI_CONFIG_DIR`, and `COPILOT_CONFIG_DIR` pointing at the developer's real home. (`common::skim()` does set `SKIM_DISABLE_ANALYTICS=1`, and this particular test passes neither `--wrappers` nor `--uninstall`, so today's blast radius is small — but the pattern is the one PF-017 documents as latent-until-armed.) `avoids PF-017` is only partially satisfied.
- Fix: route the new test through the sandbox like its sibling.
  ```rust
  let home = TempDir::new().unwrap();
  fs::create_dir_all(home.path().join(".claude")).unwrap();
  let hook_path = home.path().join(".claude/hooks/skim-rewrite.sh");
  common::skim_sandboxed(home.path()).arg("init").args(["--yes"]).assert().success();
  ```
  (Note the `create_dir_all` prerequisite — `detect_installed_agents()` in override-mode requires the config dir to already exist.)

**Fast-path condition gained a `--wrappers` term but still swallows `--dry-run`** — `crates/rskim/src/cmd/init/install.rs:504-514`
**Confidence**: 88%

- Problem: the fast-path predicate you modified now reads `hook_installed && hook_is_current() && pin_is_current() && guidance_current && !permissions_blocked && !wrappers_blocked && manifest_present`, then early-returns at `:512`. The `if flags.dry_run` block sits at `:522` — **after** the return. So `skim init --dry-run` against a fully-current install prints "Already up to date. Nothing to do." and previews nothing, which is the identical asymmetry that motivated adding the `--wrappers` term (`--permissions` was a term, `--wrappers` was not).
- `PF-018`'s recorded resolution names three items — "add a `pin_is_current(current_exe)` term **and** a `wrappers_blocks_fast_path()` term mirroring `permissions_blocks_fast_path`, **and move the dry-run block ahead of the early return**". Two of three landed. `applies PF-018` is incomplete, and the general rule PF-018 states ("treat an idempotence fast path as a predicate over the UNION of effects the command performs … every flag added later must either become a term or it is silently swallowed") is still violated by one flag.
- Fix: hoist the dry-run block above the fast-path check, or add `&& !flags.dry_run` to the condition. Hoisting is preferable — a dry run should never write the self-heal manifest either.

---

## Pre-existing Issues (Not Blocking)

None reaching the CRITICAL bar. Noted for context only, not for this PR:

- `doctor/mod.rs:494` emits `run \`./target/release/skim init --yes\` to update` in user-facing output — a repo-local development path shipped to end users. Pre-existing, untouched by this diff.
- `cli_init.rs:17` `skim_init_cmd` remains the harness for 26 shell-outs. Migrating them is the separate PF-017 cleanup the KB already tracks.

---

## Suggestions (Lower Confidence)

- **`match` over `Option<bool>` returning only bool literals** — `crates/rskim/src/cmd/init/install.rs:170-174` (Confidence: 70%) — `matches!(flags.wrappers, Some(true))` is the idiomatic single-expression form; whether `clippy::match_like_matches_macro` actually fires depends on its arm-comment exemption, which I could not verify without running clippy (read-only constraint). The three-arm `match` does document each case explicitly, which is a defensible reason to keep it — but then the arm ordering should still match its stated mirror (see B4).
- **`node.parent().map(...).unwrap_or(false)`** — `crates/rskim-core/src/transform/minimal.rs:302` (Confidence: 65%) — the codebase uses `is_some_and` elsewhere (e.g. `cli_init.rs:33`); `node.parent().is_some_and(|p| p.parent().is_none())` reads closer to the prevailing idiom.
- **Inverted assertion left no CLI-tier negative case** — `crates/rskim/tests/cli.rs:400-404` (Confidence: 70%) — `test_cli_minimal_mode_python_shebang` flipped its only comment assertion from "stripped" to "preserved". Non-header stripping is still covered at the library tier (`integration.rs:1608`), so this is coverage-shape rather than a gap, but the CLI test now asserts preservation only.

---

## What is consistent (verified, no action)

- `is_module_header_comment` (`minimal.rs:292`) — naming matches the `is_*_comment` / `is_*_node` predicate family; placed alongside `is_go_declaration` / `is_doc_comment`; delegates the comment-kind test to the shared `is_comment_node` (`minimal.rs:120`) rather than re-scanning kinds; its language set `{Python, Ruby, Sql, Bash}` is **exactly** the set for which `is_doc_comment` returns unconditional `false` (`minimal.rs:174-224`), so the stated rationale holds against the code.
- `pin_is_current` / `hook_is_current` naming, visibility (`pub(super)`), and `impl DetectedState` placement are consistent.
- New unit-test names (`test_wrappers_blocks_fast_path_{some_true,some_false,none}_*`) follow the existing `permissions_blocks_fast_path` test naming.
- `skim_sandboxed_with_bin` / `skim_sandboxed` split: the doc contract ("All sandbox env-var documentation lives on that function") is stated and honored; the delegation is a true thin wrapper; the new E2E test uses `hermetic_path()` consistently with the other `cli_doctor.rs` tests.
- `commit_ok` "unknown" handling (`doctor/mod.rs:466-470`) correctly mirrors `hook_is_current()`'s tarball-build treatment (`state.rs:99-103`) — the two now agree, which is the point.
- ADR-011: no new stderr notices, so no classification obligation arises.

---

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 5 | - |
| Should Fix | - | 0 | 4 | - |
| Pre-existing | - | - | 0 | 2 |

**Consistency Score**: 6/10

The two headline mechanisms (`pin_is_current`, `wrappers_blocks_fast_path`) land in the right places with the right names, and the ADR-011/exit-code contracts are respected. The recurring theme in the findings is **single-source-of-truth erosion in a PR whose stated purpose is unification**: the canonical-binary-path derivation is re-inlined a fourth time in doctor, the pin comparison is written twice with divergent failure semantics, and the doc surfaces (doctor docblock, `Mode::Minimal` rustdoc, the "mirrors" claim) drifted from the code they describe. None of these break behavior on the happy path, but each one re-arms the class of defect PF-015 and PF-018 were written about.

**Recommendation**: CHANGES_REQUESTED

Minimum to clear: B1 (use `current_exe_canonical()` / thread `running_path`), B6 (single pin-comparison helper), and S1 (stale `Unreadable` docblock that its own new test contradicts). B2/B3/B4/B5 and S2/S3/S4 are low-risk mechanical follow-ups that can land in the same pass.
