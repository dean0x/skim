# Testing Review Report

**Branch**: fix/init-pin-wrappers-header-comments -> main
**PR**: #488
**Date**: 2026-08-18 19:06

---

## Directed-question answers (up front)

**Q1 — Does `skim_sandboxed_with_bin` set every override?** Yes. `crates/rskim/tests/common/mod.rs:66-86` sets all five agent config-dir overrides (`CLAUDE_CONFIG_DIR`, `GEMINI_CONFIG_DIR`, `COPILOT_CONFIG_DIR`, `CODEX_HOME`, `CRUSH_CONFIG_DIR`) plus `SKIM_WRAPPERS_DIR`, `SKIM_CACHE_DIR`, `HOME`, `SKIM_DISABLE_ANALYTICS=1`, `NO_COLOR=1`, and removes `SKIM_REWRITTEN_FROM` / `SKIM_PASSTHROUGH` / `SKIM_HOOK_VERSION` / `SKIM_HOOK_BINARY`. The extraction is correct and `skim_sandboxed` delegates cleanly (applies ADR-004, avoids PF-017). Note the base changed from `common::skim()` (`cargo_bin`) to `assert_cmd::Command::new(bin)`; the two env vars `skim()` contributed (`SKIM_DISABLE_ANALYTICS`, `NO_COLOR`) plus the `SKIM_REWRITTEN_FROM` removal were all re-added explicitly, so nothing was lost in the move.

**Q2 — Can any init/doctor shellout touch the real home?**
- `cli_doctor.rs`: **clean**. All 6 invocations route through `skim_sandboxed` / `skim_sandboxed_with_bin` (lines 52, 100, 138, 185, 199, 230).
- `cli.rs`: **clean**. No `init` / `--uninstall` / `doctor` shellouts at all.
- `cli_init.rs`: **not clean**. 26 call sites use the un-sandboxed `skim_init_cmd` helper (`cli_init.rs:17-22`, which sets only `CLAUDE_CONFIG_DIR`) versus 8 that use `skim_sandboxed`. 24 of those 26 are pre-existing (Category 3, informational — the KB already records this as a "Known remaining gap"). **2 are newly added by this PR** in `test_init_rewrites_hook_when_pin_path_differs` — see Blocking finding 1.

**Q3 — `wrappers_blocks_fast_path(None)`?** **Covered.** `crates/rskim/src/cmd/init/install.rs` adds `test_wrappers_blocks_fast_path_none_does_not_block`, which asserts `None` must NOT block and names the load-bearing rationale in the comment. All three tri-state arms are covered. No gap.

**Q4 — `resolve_skim_binary()` on a symlinked binary?** **Not covered.** See Should-Fix finding 5.

**Q5 — `pin_is_current` "binary pin mismatch" doctor branch + exit code?** Branch is covered at both tiers (3 new unit tests in `doctor/mod.rs`, 1 new E2E in `cli_doctor.rs`), and is non-vacuous because `test_doctor_exits_0_after_clean_init` (`cli_doctor.rs:86`) is a live negative control that would fail if `pin_is_current` were stuck false. **The exit code itself is not verified** — see Blocking finding 2.

**Q6 — Two interception surfaces.** No conflation found. Every new init/doctor test drives the CLI/install surface. `test_init_wrappers_bypasses_fast_path` covers wrapper *installation* (symlink creation + fast-path bypass) only; neither its name, its comment, nor its assertions claim anything about the argv[0] dispatch surface. `test_line_numbers_pseudo_leading_blank_lines` and the `rskim-core` assertions are transform-layer and surface-independent. Correctly avoids PF-004.

**Q7 — Are the six inverted/renamed `rskim-core` assertions still behaviour-focused?** Yes. All six assert observable transform output (`result.contains(...)` on rendered text), not internal state or call paths. The renames (`test_ruby_minimal_strips_comments` -> `test_ruby_minimal_preserves_module_header_comments`, `test_sql_minimal_strips_comments` -> `test_sql_minimal_preserves_module_header_comments`) now describe the expected behaviour accurately, and the two fixtures that were restructured (`integration.rs:1609` Python, `ruby_transform.rs:159` Ruby) were changed so the *stripping* half of each test still has a real negative case rather than being deleted. This is the correct way to invert an assertion. The `test_sql_minimal_reduces_tokens` repoint from `sql/simple.sql` to `sql/comments.sql` is legitimate — `simple.sql` is header-comments-only so it can no longer demonstrate reduction, and `simple.sql`'s new behaviour is still asserted by `test_sql_minimal_preserves_module_header_comments`.

---

## Issues in Your Changes (BLOCKING)

### HIGH

**New test bypasses the sandbox helper this same PR extracted to close PF-017** — `crates/rskim/tests/cli_init.rs:1737` and `:1758`
**Confidence**: 90%

- Problem: `test_init_rewrites_hook_when_pin_path_differs` shells out to `skim init --yes` twice via `skim_init_cmd(config)` (`cli_init.rs:17-22`), which sets only `CLAUDE_CONFIG_DIR` — no `HOME`, no `SKIM_CACHE_DIR`, no `SKIM_WRAPPERS_DIR`, no `CODEX_HOME` / `CRUSH_CONFIG_DIR` / `GEMINI_CONFIG_DIR` / `COPILOT_CONFIG_DIR`. The PR's stated purpose includes "`skim_sandboxed_with_bin` extracted to close a PF-017 env-leak gap", and `cli_doctor.rs` was fully converted — but this brand-new init test adds two more un-sandboxed call sites (avoids PF-017 is violated here).
- Impact: I traced the concrete blast radius and it is currently narrow, not catastrophic: `detect_installed_agents` runs in `any_override` mode (because `CLAUDE_CONFIG_DIR` is set) so only claude-code is selected (`init/flags.rs:228-232`); guidance resolves via `claude_config_dir` before `home_dir` (`session/types.rs:197-203`) so real `~/.claude/CLAUDE.md` is safe; and `maybe_install_wrappers(None, ..)` early-returns on non-TTY (`install.rs:731-735`) so real `~/.skim/bin` is safe. **The real exposure is `SKIM_CACHE_DIR` being unset** (cache/hook.log resolve to the developer's real `~/.cache/skim`), plus the fact that all three protections above are incidental rather than asserted. Adding `--wrappers` to this test, or any future init code path that writes to the cache dir, silently escapes the TempDir with no test failure to warn you.
- Fix: route the new test through the helper the PR just created. The step-2 script-patching still works because `skim_sandboxed` writes the hook under `$HOME/.claude/hooks/`:

```rust
#[test]
fn test_init_rewrites_hook_when_pin_path_differs() {
    let dir = TempDir::new().unwrap();
    let home = dir.path();
    // detect_installed_agents() in override mode requires the dir to exist.
    fs::create_dir_all(home.join(".claude")).unwrap();

    common::skim_sandboxed(home)
        .args(["init", "--agent", "claude-code", "--yes", "--no-wrappers"])
        .assert()
        .success();

    let hook_path = home.join(".claude/hooks/skim-rewrite.sh");
    // ... unchanged patch-and-reinstall steps, using skim_sandboxed(home) ...
}
```

### MEDIUM

**Doctor exit-code assertion does not verify the exit code** — `crates/rskim/tests/cli_doctor.rs:204`
**Confidence**: 88%

- Problem: `test_doctor_exits_1_on_binary_pin_mismatch` is named for exit 1 and comments `// exit 1 (pin drift)`, but asserts `.failure()`, which `assert_cmd` satisfies for *any* non-zero status. A panic (101), an arg-parse error (1 via a different path), or a future exit-code change to 2 would all keep this test green while the documented contract ("Exit codes: `0` success / `1` general error", CLAUDE.md) silently regressed. `skim doctor`'s "exit 0 healthy / 1 on any drift — works as a CI pre-flight" contract is exactly the kind of thing consumers script against.
- Note: the pre-existing `test_doctor_exits_1_and_names_tamper_after_hook_modification` (line 143) has the same weakness, so this is consistent with local style rather than a novel mistake — but the PR description explicitly claims exit-code coverage for the new branch, and the new line is yours.
- Fix:

```rust
        .assert()
        .code(1) // exit 1 (pin drift) — not merely non-zero
        .stdout(predicates::prelude::predicate::str::contains(
            "binary pin mismatch",
        ));
```

**Fast-path-bypass test has no same-harness control, so it can pass vacuously** — `crates/rskim/tests/cli_init.rs:1786-1818`
**Confidence**: 82%

- Problem: `test_init_wrappers_bypasses_fast_path` asserts only the *absence* of `"Already up to date"` after `init --yes --wrappers`. It never establishes that the fast path was reachable in the first place. The fast path needs all seven conditions (`install.rs:500-512`: `hook_installed && hook_is_current() && pin_is_current() && guidance_current && !permissions_blocked && !wrappers_blocked && manifest_present`). If any of the other six became false under this harness — e.g. `guidance_current` regressing because `skim_sandboxed` sets `HOME` while step 1 omits `--no-guidance`, or `manifest_present` breaking — the test would pass while proving nothing about `wrappers_blocks_fast_path`.
- Impact: `test_init_skips_when_version_and_commit_are_current` (line 1704) is the natural control, but it runs under the *different*, un-sandboxed `skim_init_cmd` harness, so it does not establish fast-path reachability for this test's environment. The regression this test exists to catch (#478) could silently stop being caught.
- Fix: add a control assertion between step 1 and step 2, in the same harness:

```rust
    // Control: a plain re-run MUST hit the fast path, proving the bypass
    // assertion below is meaningful.
    common::skim_sandboxed(home_path)
        .arg("init")
        .args(["--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Already up to date"));

    // Step 2: Re-run with --wrappers — must NOT print "Already up to date".
```

**`pin_is_current` happy-path unit test silently passes when it cannot run** — `crates/rskim/src/cmd/init/state.rs` (`test_pin_is_current_matching_path_returns_true`)
**Confidence**: 85%

- Problem: the test does `let Some(running_path) = running else { return; };` — an early `return` from a `#[test]` fn is reported as a **pass**, not a skip. If `current_exe()` or `canonicalize()` ever fails in some environment, the only test asserting `pin_is_current() == true` becomes a silent no-op, and CI stays green with zero coverage of the affirmative branch. This is the same class of blind spot PF-015 describes (a provenance mechanism failing in a way its own tests cannot see).
- Fix: make the precondition a hard failure — the fallback chain already tolerates canonicalize failure, so only `current_exe()` can fail, and that is genuinely exceptional:

```rust
        let running_path = std::env::current_exe()
            .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
            .expect("current_exe() must resolve for pin_is_current coverage");
```

---

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`resolve_skim_binary()` — the PR's central unification — has zero direct tests and no symlinked-binary coverage** — `crates/rskim/src/cmd/init/helpers.rs:26`
**Confidence**: 90%

- Problem: this new helper is the single source of truth for the canonical binary path and is now consumed by four sites (`install.rs:747`, `install.rs:869`, `install.rs:938`, `state.rs:65`, plus `state.rs:121`). `helpers.rs` has a `mod tests` block (line 472) but adds no test for it. More importantly, **nothing tests the symlink-resolution behaviour that is the entire reason the helper exists.** The feature KB states this explicitly: "`resolve_skim_binary()` is machine-dependent... A test environment where the binary has no symlinks will pass even if the three-site invariant is broken — the failure only appears on symlinked-path machines" and "A green CI run is not proof the path-comparison invariant holds."
- Impact: if the `canonicalize()` call were dropped or reordered in a future refactor, every test in this PR would still pass on CI, and the infinite-reinstall-loop bug (ADR-004's motivating failure) would reappear only on developer machines with a symlinked binary (macOS `/tmp -> /private/tmp`, Homebrew cellar, `cargo install`). The new `test_pin_is_current_matching_path_returns_true` cannot catch it either: it derives its expected value with the same `canonicalize(current_exe())` algorithm the implementation uses, so on an unsymlinked path it is a tautology.
- Fix: add a Unix-gated E2E that installs *through* a symlink and asserts the hook pins the resolved target, not the symlink:

```rust
#[cfg(unix)]
#[test]
fn test_init_pins_canonical_path_when_invoked_via_symlink() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    std::fs::create_dir_all(home.join(".claude")).unwrap();

    let real_bin = common::skim_bin();
    let link = home.join("skim-link");
    std::os::unix::fs::symlink(&real_bin, &link).unwrap();

    common::skim_sandboxed_with_bin(home, &link)
        .args(["init", "--agent", "claude-code", "--no-guidance", "--no-wrappers"])
        .assert()
        .success();

    let script = std::fs::read_to_string(home.join(".claude/hooks/skim-rewrite.sh")).unwrap();
    let canonical = std::fs::canonicalize(&real_bin).unwrap();
    assert!(
        script.contains(&*canonical.to_string_lossy()),
        "hook must pin the canonical target, not the symlink, got:\n{script}"
    );
    assert!(
        !script.contains("skim-link"),
        "hook must not pin the symlink path, got:\n{script}"
    );
}
```
  A second re-run asserting `"Already up to date"` would additionally lock in the no-reinstall-loop property.

**Bash is in the `is_module_header_comment` allowlist with zero behavioural coverage** — `crates/rskim-core/src/transform/minimal.rs:290`
**Confidence**: 90%

- Problem: the new function matches on `Language::Python | Language::Ruby | Language::Sql | Language::Bash`. Python has five new direct unit tests; Ruby and SQL each got an inverted integration assertion. **Bash has nothing.** The only Bash minimal-mode test is `bash_transform.rs:290 test_bash_minimal_mode_runs_without_error`, which asserts `result.is_ok()` and nothing about comment content — a pure smoke test. The unit-test header comment claims "the grammar rules are identical for Python, Ruby, SQL, and Bash at the root-child / gap-bytes level", but that is an untested assertion about four different tree-sitter grammars, and Bash is the one where it is least obvious (the shebang is itself a comment node, and `is_shebang` already handles it, so the interaction between the two preserve-rules is unexercised).
- Impact: a Bash grammar update that nests root comments differently, or a change to `is_comment_node` for Bash, would go undetected. Bash files are also the highest-value case for header preservation (shebang + `set -euo pipefail` provenance blocks).
- Fix: add one behavioural test mirroring the Ruby/SQL pattern:

```rust
#[test]
fn test_bash_minimal_preserves_module_header_comments() {
    let source = "#!/usr/bin/env bash\n# Copyright 2024 Acme\n\n# strip me\nfoo() { echo hi; }\n";
    let result = transform(source, Language::Bash, Mode::Minimal).unwrap();
    assert!(result.contains("# Copyright 2024"), "header must survive, got:\n{result}");
    assert!(!result.contains("strip me"), "post-blank-line comment must be stripped, got:\n{result}");
}
```

**New `print_wrapper_install_result` output gating is untested** — `crates/rskim/src/cmd/init/install.rs:781-789`
**Confidence**: 88%

- Problem: the PATH-setup blurb ("To enable wrappers, add to ~/.zshrc...") is now gated on `result.created + result.updated > 0`. This is a user-visible behaviour change with no test on either side of the condition. `test_init_wrappers_bypasses_fast_path` only reaches the `created > 0` case (its step 2 is the first wrapper install), so the branch the change was written for — an idempotent `init --wrappers` re-run where all wrappers are already correct — is never executed.
- Impact: the noise-suppression fix can silently regress, and the inverse (blurb disappearing when wrappers *were* created, which would break onboarding) is equally uncovered.
- Fix: extend `test_init_wrappers_bypasses_fast_path` with a third invocation:

```rust
    // Step 3: third run — wrappers all already correct, blurb must be suppressed.
    let out3 = common::skim_sandboxed(home_path)
        .arg("init")
        .args(["--yes", "--wrappers"])
        .output()
        .unwrap();
    let stdout3 = String::from_utf8_lossy(&out3.stdout);
    assert!(stdout3.contains("Wrappers:"), "wrapper line still reported, got:\n{stdout3}");
    assert!(
        !stdout3.contains("To enable wrappers"),
        "PATH blurb must be suppressed when nothing was created or updated, got:\n{stdout3}"
    );
```
  (Step 2 should conversely assert the blurb IS present, pinning both sides.)

---

## Pre-existing Issues (Not Blocking)

**24 pre-existing `skim_init_cmd` call sites remain un-sandboxed** — `crates/rskim/tests/cli_init.rs` (26 total call sites, 2 of them new in this PR)
**Confidence**: 92%

- Informational only — these lines are untouched by this PR and the Iron Law says they do not block. The feature KB already records this as a "Known remaining gap". Now that `skim_sandboxed_with_bin` exists as the authoritative block, a follow-up PR that redefines `skim_init_cmd` to delegate to it (rather than converting 26 sites by hand) would close PF-017 for the whole file in one edit. Worth a tracking issue rather than scope creep here.

---

## Suggestions (Lower Confidence)

- **Hand-rolled hook-script fixture couples to the script format** — `crates/rskim/tests/cli_init.rs:1745-1757` (Confidence: 70%) — `test_init_rewrites_hook_when_pin_path_differs` builds its patched script with a `format!` literal rather than deriving it from the installed script. If `generate_hook_script`'s marker layout changes, this fixture silently stops representing a valid pinned script and the test could pass for the wrong reason. Reading the real installed script and doing a targeted string replace of the pin path would be more robust.
- **Pin-rewrite test asserts only the negative** — `crates/rskim/tests/cli_init.rs:1770-1774` (Confidence: 65%) — step 4 asserts the wrong path is gone but never asserts the *new* pin equals the running binary. Adding `assert!(updated.contains(&*std::fs::canonicalize(common::skim_bin()).unwrap().to_string_lossy()))` would turn "not wrong" into "correct".
- **`nth_root_comment` helper takes an unused `_source` parameter** — `crates/rskim-core/src/transform/minimal.rs:106` (Confidence: 70%) — dead parameter in new test-support code; drop it for clarity.

---

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 3 | - |
| Should Fix | - | 0 | 3 | - |
| Pre-existing | - | - | 1 | 0 |

**Testing Score**: 7/10

The tiering discipline in this PR is genuinely good: the doctor pin-mismatch branch is covered at both the unit tier (3 new `hook_status_line` tests, including the tarball-build `"unknown"` commit case) and the E2E tier (copy-binary technique), with a live negative control at `cli_doctor.rs:86` so neither is vacuous. `wrappers_blocks_fast_path` has full tri-state coverage with the load-bearing `None` case explicitly named. The six inverted assertions were restructured rather than deleted, so both halves of each behaviour still have a real case. The two interception surfaces are correctly not conflated. Points come off for the PF-017 self-contradiction (the PR extracts the sandbox helper, then adds a test that skips it), the untested symlink behaviour at the heart of `resolve_skim_binary()`, and three assertions that can pass without proving anything (`.failure()` vs `.code(1)`, the missing fast-path control, the silent `return` in the pin happy-path test).

**Recommendation**: CHANGES_REQUESTED

Blocking finding 1 (route the new init test through `skim_sandboxed`) and finding 2 (`.code(1)`) are one-line fixes. Findings 3 and 4 are small additions. The two MEDIUM Should-Fix coverage gaps (symlinked `resolve_skim_binary`, Bash header comments) are the highest-value additions for long-term safety and are worth doing in this PR while the context is fresh, since both guard behaviour this PR introduced.
