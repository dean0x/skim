# Regression Review Report

**Branch**: fix/init-pin-wrappers-header-comments -> main (PR #488)
**Date**: 2026-08-18 19:06
**Diff**: `git diff main...HEAD` (21 files, +1211/-158)

---

## Direct answers to the five highest-value questions

Recorded up front because the orchestrator asked for verdicts, not just findings.

### Q1 — Six inverted/renamed assertions: does any launder a real regression?

**No — none of the six launders a regression.** All six old assertions encoded the pre-#476
contract ("module-level non-doc comments are stripped"), which the PR deliberately reverses for
Python/Ruby/SQL/Bash. Each inversion is individually justified. **But two of them silently
delete the negative-case coverage of the new rule** — see MEDIUM-4 and MEDIUM-5.

| # | Location | Change | Verdict |
|---|----------|--------|---------|
| 1 | `crates/rskim-core/tests/integration.rs:1608` `test_python_minimal_nested_function_body_comments` | source rewritten to start with `x = 1`; `"top-level should be stripped"` → `"mid-file comment should be stripped"` | **Justified.** Old source's first line was a header under the new rule, so the old assertion is genuinely unsatisfiable. New source still exercises the strip path. |
| 2 | `crates/rskim-core/tests/integration.rs:2374` `test_sql_minimal_reduces_tokens` | fixture `sql/simple.sql` → `sql/comments.sql` | **Justified but weakens the guard.** See MEDIUM-5. |
| 3 | `crates/rskim-core/tests/ruby_transform.rs:142` `test_ruby_minimal_strips_comments` → `..._preserves_module_header_comments` | `!contains("# FIXTURE:")` → `contains("# FIXTURE:")` | **Justified.** `ruby/simple.rb:1-2` is a contiguous root-level comment run at byte 0. |
| 4 | `crates/rskim-core/tests/ruby_transform.rs:158` `test_ruby_minimal_preserves_body_comments` | comment moved from root level into the class body; `"Top-level comment"` → `"class-level comment"` | **Justified in outcome, but a coverage swap.** See MEDIUM-4. |
| 5 | `crates/rskim-core/tests/sql_transform.rs:126` `test_sql_minimal_strips_comments` → `..._preserves_module_header_comments` | `!contains("-- FIXTURE:")` → `contains("-- FIXTURE:")` | **Justified.** `sql/simple.sql:1-2`. |
| 6 | `crates/rskim/tests/cli.rs:404` `test_cli_minimal_mode_python_shebang` | `contains("# regular comment").not()` → `contains("# regular comment")` | **Justified.** The comment is contiguous with the shebang (single `\n` gap) → header. Removes the only CLI-tier strip assertion (see Suggestions). |

### Q2 — Does `is_module_header_comment()` leak?

**Yes, in one unbounded way; the language split is principled by its own stated criterion but
under-inclusive relative to its stated motivation.** Details in MEDIUM-6.

- **Blank-line break and preceding-code both work.** I verified the gap arithmetic against all
  four grammars: none of the four comment node types include the trailing newline
  (`tree-sitter-sequel` `comment: _ => /--.*/` grammar.js:641; `tree-sitter-bash`
  `comment: _ => token(prec(-10, /#.*/))` grammar.js:1103; `tree-sitter-ruby`
  `seq('#', /.*/)` and `/[\s*]*=end.*/` grammar.js:1057-1068; Python `token(seq('#', /.*/))`).
  So a blank line always yields ≥ 2 newlines in the gap and correctly terminates the run.
  The unit-test comment claiming "the grammar rules are identical … at the root-child /
  gap-bytes level" (`minimal.rs:96-97`) is **true** — but it was asserted, not tested.
- **The leak is that there is no cap.** A file that *opens* with a contiguous comment run has
  that entire run preserved — a leading commented-out code block, a 50-line licence banner, or a
  100%-comment file all yield 0% reduction in minimal *and* pseudo (pseudo is the mode ADR-008's
  wrapped `cat`/`head`/`tail` serves).
- **Affected**: Python, Ruby, SQL (`--` only), Bash. **Unaffected**: TS/JS, Rust, Go, Java,
  C/C++, C#, Kotlin, Swift, Markdown/JSON/YAML/TOML.
- The split is exactly `{L : is_doc_comment(L) ≡ false}` (`minimal.rs:174-178, 203-207, 216-223`)
  — principled by the doc-comment criterion, but not by the stated *motivation*
  ("copyright, SPDX, provenance markers"): Rust `// SPDX-License-Identifier:`, C `/* Copyright */`,
  TS `/* @license */`, and Go banners followed by a blank line are all still stripped.

### Q3 — What re-runs when `--wrappers` bypasses the fast path?

Confirmed by tracing `install.rs:503-514 → 557-569 → execute_install (630-679)`:

| Runs | Effect |
|------|--------|
| `create_hook_script` (`install.rs:880`) | Hook script **not** rewritten — `is_hook_script_current` early-returns "Skipped". **But the SHA-256 manifest is recomputed and rewritten.** |
| `migrate_cursor_legacy_settings` / `migrate_copilot_legacy` | Re-runs for those agents. |
| `patch_settings` (`install.rs:1288`) | → `backup_settings` (`install.rs:1271-1286`) does an unconditional `fs::copy` — **`settings.json.bak` is overwritten with the already-skim-patched settings**, destroying the pre-skim backup. Then `atomic_write_settings`. |
| `write_permissions` | Only if `grant_permissions` (needs `--permissions` + TTY consent) — unchanged. |
| `inject_guidance` (`guidance.rs:168`) | Guidance file rewritten (idempotent in content, but re-written). |
| `install_search_integration` (`install.rs:686`) | Installs search git hooks into the **cwd repo** and spawns a detached background `skim search --build` process. |

See MEDIUM-3 (side-effect breadth) and HIGH-1 (the `--project` variant, where none of this buys
a single wrapper).

### Q4 — Was the removed "stale" branch really unreachable?

**Yes — proven, and it is now correct only *because* of the `commit_ok`/"unknown" fix landed in
the same hunk.** No finding.

At `doctor/mod.rs:459` the code has already early-returned for `!hook_installed` (L385),
`Tampered`/`Unreadable` (L400-418), and `!hook_uses_pinned_binary` (L449). So
`hook_uses_pinned_binary == true` holds. With that:

```
DetectedState::hook_is_current()  (state.rs:87-113)
  = (hook_version == skim_version) ∧ hook_uses_pinned_binary ∧ commit_check
  where commit_check = (compiled_commit == "unknown") ? true : (hook_commit == Some(compiled_commit))
```

`doctor` uses `compiled_version = env!("CARGO_PKG_VERSION")` (L33) and
`compiled_commit = option_env!("SKIM_GIT_COMMIT").unwrap_or("unknown")` (L34) — byte-identical to
`state.rs:122` and `state.rs:97`. So `version_ok` ≡ the version term and, **after this PR's fix**,
`commit_ok` (L466-470) ≡ `commit_check`. Therefore `commit_ok ∧ version_ok ⇒ hook_is_current`,
and reaching the `else` terminal requires `!pin_is_current`. The `"stale"` string was dead.

Note the honest ordering: *before* this PR, `commit_ok` did **not** force `true` for `"unknown"`,
so on a tarball build the two predicates disagreed (state said current, doctor said
`commit mismatch (hook: abc1234, binary: unknown)`). The removal is safe only in combination with
that fix; they must not be separated. No residual `"stale"` string exists anywhere in
`crates/rskim/src` or `crates/rskim/tests`.

**Exit-code contract**: unchanged for every input that previously reached `"stale"` (none). It
*is* widened by the new `|| !facts.pin_is_current` disjunct — see Suggestions.

### Q5 — Does `-n` change for files that were previously correct?

**No.** `normalize_line_map_blanks` is called from exactly one place —
`pseudo.rs:343` (verified by grep; `types.rs:456` and `transform/mod.rs:147` use
`compute_line_map_by_text_matching`, so minimal/structure/signatures/types are untouched).

The new `if result.is_empty() { continue; }` (`transform/mod.rs:343-345`) fires only for blank
lines **before the first non-blank line of the intermediate text**. Those were previously
off-by-K. Any file whose intermediate text starts with content produces a byte-identical map.

Two edge cases checked:
- `consecutive_blanks` is not incremented during the skipped leading run (the `continue` precedes
  the `+= 1`), whereas `trim_and_normalize` (`minimal.rs:444-470`) does increment. This cannot
  diverge: the first non-blank line resets the counter to 0 in both.
- All-blank input: text = `"\n"` (1 line, trailing-newline restore at `minimal.rs:465-467`),
  map = `[]`. `format_with_line_numbers` (`format.rs:58-61`) degrades via `.get(i).unwrap_or(0)`
  and emits the line with **no** prefix. Previously the map was `[1, 2]` and the blank line got a
  spurious `1\t` prefix. The new behaviour is strictly more correct.

---

## Issues in Your Changes (BLOCKING)

### HIGH

**`skim init --project --wrappers` bypasses the fast path but can never install a wrapper — unbounded no-op reinstall churn** — `crates/rskim/src/cmd/init/install.rs:169-175`
**Confidence**: 90%

- Problem: `wrappers_blocks_fast_path` returns `true` for `Some(true)` **regardless of
  `flags.project`**, but `maybe_install_wrappers` is gated on `!flags.project` at *both* call
  sites (`install.rs:532` dry-run, `install.rs:567` real). The sibling predicate it mirrors,
  `permissions_blocks_fast_path` (`install.rs:184-206`), can never fire under `--project` because
  `flags.rs:389-395` rejects `--permissions --project` outright. `--wrappers` has no such guard
  (`flags.rs:317-333` only rejects `--wrappers` + `--no-wrappers`).
- Failure scenario: user runs `skim init --project --wrappers --yes` on a fully-current install.
  **Before**: `print_already_up_to_date()`, zero filesystem writes. **After**: full reinstall on
  every single invocation — `settings.json.bak` clobbered with the already-patched settings
  (`install.rs:1315-1323` → `1284`), guidance re-injected, SHA-256 manifest rewritten, search git
  hooks installed into the cwd repo, a detached `skim search --build` spawned
  (`install.rs:705-714`) — and **zero wrappers installed**, because `install.rs:567` skips
  `maybe_install_wrappers` for project scope. The install never converges back to the fast path;
  it repeats verbatim forever. This violates the project's own reliability rule that every repeat
  path be bounded, and the KB's stated purpose for this predicate ("bypass fast path *so wrappers
  are installed*").
- Fix:
  ```rust
  fn wrappers_blocks_fast_path(flags: &InitFlags) -> bool {
      match flags.wrappers {
          // Only block when wrappers can actually be installed: maybe_install_wrappers
          // is global-scope-only (see call sites at :532 / :567).
          Some(true) => !flags.project,
          Some(false) => false,
          None => false, // load-bearing: see doc comment
      }
  }
  ```
  Or add the mutual-exclusion guard in `flags.rs` mirroring `:389-395`. Either way add a test
  covering `--project --wrappers`; the three new unit tests
  (`test_wrappers_blocks_fast_path_{some_true,some_false,none}_*`) all build flags with
  `project: false` (`install.rs:2394`), so this case is unreachable from the current suite.

**`pin_is_current()` and `is_hook_script_current()` disagree on an unparseable pin — `skim init` never converges and `skim doctor` stays at exit 1 forever** — `crates/rskim/src/cmd/init/state.rs:59-76` + `crates/rskim/src/cmd/init/install.rs:868-876`
**Confidence**: 85%

- Problem: the two new pin checks handle "pin present but unusable" in opposite directions.
  - `script_has_pinned_marker` (`init/mod.rs:173-177`) returns `true` for **any** line starting
    with `export SKIM_HOOK_BINARY=`, including `export SKIM_HOOK_BINARY=''`.
  - `parse_binary_pin_from_script` (`state.rs:436-455`) returns **`None`** for that same line
    (`if !val.is_empty()` guard at `:449`).
  - `DetectedState::pin_is_current()` (`state.rs:60-62`): `None` → `false` ("treat as stale").
  - `is_hook_script_current()` (`install.rs:868`): `if let Some(pin) = …` — `None` means the
    whole pin check is **skipped**, and the function returns `true` ("current").
- Failure scenario: a hook script with `export SKIM_HOOK_BINARY=''` and a matching version and
  commit (the exact state the `B5b` comment at `install.rs:931-935` says the pinning work exists
  to eliminate, i.e. produced by an older skim or a hand-edit). Then:
  1. `run_install_single` — `hook_is_current()` true, `pin_is_current()` **false**
     (`install.rs:506`) → fast path bypassed → full install path taken.
  2. `create_hook_script` → `is_hook_script_current` returns **true** (`install.rs:877`) →
     prints `"Skipped: … (already v…)"` and returns at `install.rs:913`. **The script is never
     regenerated; the empty pin is never repaired.**
  3. `skim doctor` — `hook_uses_pinned_binary` is `true`, so it passes the unpinned guard at
     `doctor/mod.rs:449`, hits `!facts.pin_is_current` at `:459`, and exits **1** with
     `binary pin mismatch (hook: ?, running: /path)` plus the advice "run
     `./target/release/skim init --yes` to update" — advice that provably cannot fix the state.
  Every `skim init --yes` thereafter performs the full side-effect set (backup clobbered,
  guidance rewritten, background index build spawned) and still converges to nothing.
  The identical divergence exists for `resolve_skim_binary()` failure:
  `state.rs:74` → `false`, `install.rs:869` `if let Ok(…)` → check skipped → `true`.
- This is the anti-pattern named verbatim in the hook-binary-pinning KB: *"Adding a new required
  script line but wiring it into only one currency predicate … silently desyncs state detection
  from reinstall."* (applies ADR-004; avoids PF-015 only partially — the display path is now
  covered by `test_doctor_exits_1_on_binary_pin_mismatch`, but the *repair* path is not).
- Fix — make `is_hook_script_current` fail closed so both predicates agree:
  ```rust
  // #477: also require the binary pin to match the running binary.
  let Some(pin) = parse_binary_pin_from_script(&contents) else {
      return false; // marker present but no usable pin → stale, force a rewrite
  };
  let Ok(running) = super::helpers::resolve_skim_binary() else {
      return false; // cannot compare → treat as stale (mirrors pin_is_current)
  };
  let pin_path = std::path::Path::new(pin.as_str());
  let canon_pin = std::fs::canonicalize(pin_path).unwrap_or_else(|_| pin_path.to_owned());
  if running != canon_pin {
      return false;
  }
  ```
  Add a unit test asserting `is_hook_script_current` is `false` for a script containing
  `export SKIM_HOOK_BINARY=''` with a current version+commit.

### MEDIUM

**`--wrappers` re-run now performs the full install side-effect set, including clobbering `settings.json.bak` and spawning a background index build** — `crates/rskim/src/cmd/init/install.rs:503-514`
**Confidence**: 85%

- Problem: the fix makes `--wrappers` fall through the *entire* `execute_install` pipeline
  (`install.rs:557-569` → `630-679`) just to reach `maybe_install_wrappers` at `:567`, which is
  55 lines and six unrelated side effects downstream. The most consequential:
  `backup_settings` (`install.rs:1271-1286`) unconditionally `fs::copy`s the current
  `settings.json` over `settings.json.bak`, so a routine `skim init --wrappers` on an up-to-date
  install **destroys the user's pre-skim settings backup**; and `install_search_integration`
  (`install.rs:686-715`) installs git hooks into whatever repo the cwd happens to be in and
  spawns a detached `skim search --build`.
- Failure scenario: user who has never run `skim init` since v1 keeps a pristine
  `~/.claude/settings.json.bak`. They run `skim init --wrappers` once to add PATH wrappers to an
  otherwise-current install. The backup is silently replaced with the skim-patched settings; the
  original is unrecoverable. Previously this invocation printed "Already up to date" and wrote
  nothing.
- The new E2E test `test_init_wrappers_bypasses_fast_path` (`cli_init.rs:796`) asserts only that
  `"Already up to date"` is absent and `"Wrappers:"` is present — it does not pin the side-effect
  surface, so this breadth is untested in either direction.
- Fix (surgical, keeps the `None` load-bearing semantics intact): drop
  `wrappers_blocks_fast_path` from the fast-path conjunction and instead run wrappers *inside*
  the fast path before returning:
  ```rust
  if state.hook_installed && state.hook_is_current() && state.pin_is_current()
      && guidance_current && !permissions_blocked && manifest_present
  {
      print_already_up_to_date();
      if !flags.project {
          maybe_install_wrappers(flags.wrappers, flags.dry_run)?;
      }
      return Ok(std::process::ExitCode::SUCCESS);
  }
  ```
  This also resolves HIGH-1 as a side effect. If the current shape is kept deliberately, at
  minimum make `backup_settings` a no-op when `settings.json.bak` already exists and the live
  file already contains the skim entry.

---

## Issues in Code You Touched (Should Fix)

### MEDIUM

**The inversions deleted the negative-case coverage of the very rule they were inverted for — Ruby, SQL and Bash now have none** — `crates/rskim-core/tests/ruby_transform.rs:158`, `crates/rskim-core/tests/sql_transform.rs:126`, `crates/rskim-core/tests/integration.rs:2374`
**Confidence**: 88%

- Problem: `is_module_header_comment` is bounded by exactly two terminating conditions — the
  blank-line break (`minimal.rs:317-322`) and the non-comment preceding sibling
  (`minimal.rs:323-327`). Those bounds are the *only* thing standing between "preserve the
  header" and "preserve every comment in the file". After this PR:
  - `test_ruby_minimal_preserves_body_comments` moved its strippable comment from **root level**
    into a **class body**, so it now exercises `is_inside_function_body` / the `is_root_child`
    guard — a different code path. Ruby has **no** test that a root-level comment after a
    blank-line break is stripped.
  - `test_sql_minimal_strips_comments` was inverted to assert *preservation*. Its replacement
    negative case, `test_sql_minimal_reduces_tokens`, asserts only `result.len() < source.len()`
    — and `sql/comments.sql:24` and `:34` are stripped by the *preceding-code* rule regardless of
    the gap check, so that assertion passes even if the blank-line break stopped working entirely
    for SQL.
  - Bash has no comment-stripping assertion at all (`bash_transform.rs:290` only asserts
    `result.is_ok()`).
  - The five new direct unit tests (`minimal.rs:121-210`) are **Python-only by explicit design
    comment** (`minimal.rs:94-97`).
  Net: Python is the only affected language with a live guard on the negative case
  (`test_python_minimal_strips_regular_comments`, `integration.rs:1177`, via
  `python/comments.py:5,7`). If the blank-line break silently regressed for Ruby/SQL/Bash — for
  example if a grammar bump changed a comment node to include its trailing newline — the whole
  suite would stay green while `skim --mode=minimal` quietly stopped compressing those languages.
  (I verified the grammars today and the gap semantics do hold; this is a coverage gap, not a
  live defect. See Q2.)
- Fix: add one negative-case assertion per affected language, e.g. in `sql_transform.rs`:
  ```rust
  #[test]
  fn test_sql_minimal_strips_comment_after_blank_line_break() {
      let src = "-- header\n\n-- strippable\nSELECT 1;\n";
      let result = transform(src, Language::Sql, Mode::Minimal).unwrap();
      assert!(result.contains("-- header"), "header preserved, got:\n{result}");
      assert!(!result.contains("-- strippable"),
          "comment after a blank-line break must be stripped, got:\n{result}");
  }
  ```
  and the Ruby (`# `) and Bash (`#`) equivalents.

**Repointing `test_sql_minimal_reduces_tokens` hides that minimal mode now yields ~0% reduction on `sql/simple.sql`, and `docs/modes.md` still advertises 15-30%** — `crates/rskim-core/tests/integration.rs:2374-2388`, `docs/modes.md:10`
**Confidence**: 85%

- Problem: `sql/simple.sql`'s only comments are its two header lines (`:1-2`). After #476 minimal
  mode returns that file essentially byte-for-byte, i.e. **0% reduction** — which is why the test
  had to move. The repoint makes the suite green but removes the only guard on "minimal mode
  reduces a representative file for this language", and no replacement pins a reduction floor for
  any of the four affected languages. Meanwhile the `docs/modes.md` Minimal row was edited to
  describe header preservation but its **`15-30%` figure was left unchanged**, so the documented
  contract now overstates what minimal delivers for header-only Python/Ruby/SQL/Bash files.
  `test_minimal_token_reduction` (`integration.rs:1593`) uses a TypeScript fixture and is
  unaffected, so nothing else catches this.
- Failure scenario: a user follows `docs/modes.md`, pipes a shebang+licence-header shell script
  through `skim --mode=minimal` expecting 15-30% savings, and gets 0%. Under the ADR-008 wrapped
  `cat`/`head`/`tail` path (which serves pseudo) the same file now costs full tokens with no
  transparency marker, because the view no longer differs from raw.
- Fix: either qualify the docs figure (e.g. `15-30% (0% for files whose only comments are a
  module header)`), or restore a reduction assertion on `sql/simple.sql` that reflects the new
  expectation, plus a test that pins *which* comments `comments.sql` loses rather than only
  `len <`.

**The module-header rule is unbounded, and the language split does not match its stated motivation** — `crates/rskim-core/src/transform/minimal.rs:292-331`
**Confidence**: 80%

- Problem (leak): the backward walk terminates only at a blank line, a non-comment sibling, or
  the start of file. There is **no line or byte cap**, so the entire leading comment run is
  preserved however long it is. Concrete leaks where the run is not really a "module header":
  a leading commented-out code block (routine in dotfiles, generated SQL, and `.bashrc`-style
  scripts), an arbitrarily long licence banner, and a file that is 100% comments (0% reduction).
  This lands in `pseudo` as well as `minimal` (`pseudo.rs:446` → `is_removable_comment`), i.e. in
  the mode ADR-008 serves for wrapped `cat`/`head`/`tail`. It cannot *inflate* output beyond raw,
  so #317 is not violated — but it directly opposes the stated 60-80%/15-30% reduction targets.
- Problem (split): the affected set `{Python, Ruby, Sql, Bash}` is exactly
  `{L : is_doc_comment(L) ≡ false}` (`minimal.rs:174-178, 203-207, 216-223`), so it is principled
  *by that criterion*. It is not principled by the criterion the doc comment actually states
  ("copyright, SPDX, provenance markers"): Rust `// SPDX-License-Identifier: MIT` is stripped
  (only `//!`/`///` survive, `:179-185`), C/C++ `/* Copyright */` is stripped (`:195-198`),
  TS/JS `// Copyright` and `/* @license */` are stripped (`:170-173`), and a Go banner followed
  by a blank line before `package` is stripped (`is_go_doc_comment` returns `false` at
  `minimal.rs:246`). So `skim --mode=minimal` keeps the licence header on `.py` and drops it on
  `.rs` — an inconsistency a user will hit immediately in a polyglot repo.
- SQL-specific wrinkle: block comments are node kind `marginalia`, not `comment`
  (tree-sitter-sequel grammar.js:643), so `is_comment_node` is `false` for them
  (`minimal.rs:130`). Consequence: a SQL file whose banner is `/* … */` gets no header treatment,
  **and** a `-- SPDX` line *following* that banner is stripped, because its `prev_named_sibling`
  is a non-comment `marginalia` node (`minimal.rs:326`). The rule is therefore silently
  inconsistent within SQL itself.
- Fix: (a) cap the preserved run (e.g. 20 lines or the first blank line, whichever comes first)
  so a commented-out code block cannot defeat compression; (b) either extend the rule to the
  remaining languages by matching a header *shape* (`Copyright`, `SPDX-`, `@license`, shebang,
  `frozen_string_literal`) rather than by language identity, or narrow the doc comment at
  `minimal.rs:281-285` so it stops claiming a motivation the implementation only honours for
  4 of 15 languages; (c) treat `marginalia` as a comment for the purpose of the header walk
  (not for stripping) so the SQL walk is internally consistent.

---

## Pre-existing Issues (Not Blocking)

None material to this diff. `is_inside_function_body` treating Ruby `body_statement` as a
function body means a class-level comment placed *after* the first method is preserved while one
placed before it is stripped — pre-existing (`utils.rs:76`), untouched by this PR, and not worth
a separate issue at this severity.

---

## Suggestions (Lower Confidence)

- **`skim doctor` exit-code surface widened by `|| !facts.pin_is_current`** — `crates/rskim/src/cmd/doctor/mod.rs:459` (Confidence: 75%) — doctor now flips from exit 0 to exit 1 for anyone running it from a *different installation* than the one that installed the hook (cargo-built dev binary vs. a brew/npm binary on `$PATH`). That is the intended ADR-004 wrong-clone detection, but `doctor` is documented in CLAUDE.md as a CI pre-flight (`0` healthy / `1` on any drift): CI that builds skim in one job and runs `skim doctor` from a cached or downloaded artefact in another will start failing. Worth a CHANGELOG note.
- **`test_pin_is_current_matching_path_returns_true` can pass vacuously** — `crates/rskim/src/cmd/init/state.rs:1055` (Confidence: 70%) — the `let Some(running_path) = running else { return; }` guard turns a `current_exe()` failure into a silent pass rather than a skip-with-signal. Prefer `.expect("current_exe must resolve in the test environment")`.
- **`cli.rs:404` removes the only CLI-tier assertion that a Python comment is stripped in minimal mode** — `crates/rskim/tests/cli.rs:404` (Confidence: 70%) — the library-tier `test_python_minimal_strips_regular_comments` still covers it, but the E2E path (parser cache, `--mode` plumbing, output formatting) no longer has a negative case. Adding a third line separated by a blank line to the test fixture restores it in two lines of diff.

---

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | - |
| Should Fix | - | 0 | 3 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

Rationale: the four headline fixes are individually sound and I verified the two claims most
likely to be laundering a regression — the dead-branch removal (Q4) is provably correct, and the
pseudo line-map fix (Q5) changes nothing for previously-correct files. The six assertion
inversions are all justified. What blocks is the pair of new predicates that were added to only
one of the two sites that consume them: `wrappers_blocks_fast_path` ignores `--project`
(HIGH-1), and `is_hook_script_current` disagrees with `pin_is_current` on an unusable pin
(HIGH-2). Both produce a `skim init` that runs the full install pipeline on every invocation and
never converges, and HIGH-2 additionally leaves `skim doctor` permanently at exit 1 while
recommending a command that cannot fix it. MEDIUM-3 (backup clobbering) rides on the same
fast-path change and is fixed by the same one-line restructure suggested there.
