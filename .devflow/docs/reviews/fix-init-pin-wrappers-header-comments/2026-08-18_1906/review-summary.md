# Code Review Summary

**Branch**: fix/init-pin-wrappers-header-comments -> main (PR #488)
**Date**: 2026-08-18 19:06
**Diff**: `git diff main...HEAD` — 21 files, +1211/-158
**Reviewers**: 11 (architecture, complexity, compliance, consistency, documentation, performance, regression, reliability, rust, security, testing)

---

## Merge Recommendation: CHANGES_REQUESTED

**Reviewer votes**: 9 of 11 CHANGES_REQUESTED; 2 APPROVED_WITH_CONDITIONS (complexity, compliance).
**Score range**: 5/10 to 8/10 — reliability 5, performance 5, architecture 5, complexity 6, consistency 6, documentation 6, regression 6, security 6, rust 7, testing 7, compliance 8.

The PR's direction is right and every reviewer said so independently: `resolve_skim_binary()` unification is a genuine ADR-004 improvement, `pin_is_current()` closes the two-clone gap, the dead `"stale"` doctor terminal was **proven** dead by three reviewers working the deduction separately, the `-n` line-map fix changes nothing for previously-correct files, and the six inverted test assertions were each verified as justified rather than laundering a regression.

What blocks is a consistent pattern: **a PR whose thesis is unification ships a fourth derivation of the unified value, a second implementation of its own new predicate with inverted failure semantics, a triplicated commit rule, and a new test that bypasses the sandbox helper the same PR extracted.** Five defects converged across 5–7 independent reviewers each. That convergence is the strongest signal in this review set.

**Not escalated to BLOCK**, deliberately: exactly one reviewer (performance) labelled anything CRITICAL — the quadratic header walk — while five others rated the same defect HIGH. No reviewer recommended BLOCK. The severity is contested and, per the verification gap below, unmeasured. CHANGES_REQUESTED is the honest reading; a resolver who measures the walk and finds it worse should escalate.

---

## ⚠ Verification Gap — read before resolving

**Every finding in this review is source-reasoned, not empirically verified.** Reviewers were barred from running `cargo build` / `test` / `clippy`: an 11-way parallel fan-out of cargo would exhaust this machine's RAM (see CLAUDE.md "Build/test resource limits" — two clones with separate `target/` dirs compiling heavy deps once hard-restarted the machine).

Consequences the resolver must act on:

- **The quadratic-walk severity rests on a call-graph argument with no timing measurement.** Nobody ran a comment-dense fixture. The complexity *class* is also disputed (see cluster 1).
- No finding has been confirmed against a compiler error, a clippy lint, or a failing test.
- Claims about clippy behaviour (e.g. rust's assertion that `wrappers_blocks_fast_path`'s three-arm match will not trip `clippy::match_like_matches_macro`; consistency's inability to verify the arm-comment exemption) are **unverified predictions**.

**The fix pass MUST include, run serially and centrally — never inside a fan-out agent:**
```
cargo clippy -p rskim --all-features --all-targets -- -D warnings
cargo clippy -p rskim-core --all-features --all-targets -- -D warnings
cargo nextest run -p rskim-core -j 4
cargo nextest run -p rskim --all-targets -j 4     # --bins is unit-only, skips tests/
```
Plus a timing measurement on a comment-dense `.py`/`.sh`/`.sql` fixture before accepting or downgrading cluster 1.

---

## PR Annotation Status

The Git agent has **already posted 17 inline comments on PR #488** covering the 6 convergence clusters and 11 standalone ≥80% findings, plus **one summary comment consolidating 12 more items**. The summary comment carries the 3 blocking findings whose anchor lines fall outside the diff and therefore could not be posted inline:

- `crates/rskim-core/src/types.rs:567` (and `:642`) — `Mode::Minimal` rustdoc
- `CLAUDE.md:83` — doctor pin-state description
- `crates/rskim/src/cmd/doctor/mod.rs:371-372` — stale `hook_status_line` docblock

**Resolver: do not re-post these.** The PR is annotated. Work from this summary and the inline threads.

---

## Issue Summary (deduplicated across 11 reviewers)

| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| Blocking (Cat 1) | 0* | 11 | 20 | 1 | 32 |
| Should Fix (Cat 2) | - | 3 | 12 | - | 15 |
| Pre-existing (Cat 3) | - | - | 6 | 2 | 8 |

\* performance rated the header-walk defect CRITICAL; 5 other reviewers rated the same defect HIGH. Recorded as HIGH with the dissent preserved.

---

## Blocking Issues — Convergence Clusters

These five were found independently by 5–7 reviewers each. **Rank them above any singleton.** Multi-reviewer agreement here is genuine corroboration: the reviewers ran different lenses against different questions and arrived at the same file:line.

### C1. `is_module_header_comment` — unbounded, super-linear backward sibling walk
`crates/rskim-core/src/transform/minimal.rs:292-331` (loop at `:308-330`)
**6 reviewers** · **Confidence 100%** (highest single: reliability 92%) · **HIGH** (performance: CRITICAL)
*performance, security, reliability, rust, complexity, architecture. regression flagged the adjacent semantic-unboundedness (see C1b).*

- **Defect**: bare `loop` with no iteration cap, walking `prev_named_sibling()` backwards once per root-level comment. For a contiguous run of `L` root-level comments, comment *i* re-walks *i* siblings.
- **Reachability**: `is_doc_comment` returns a hardcoded `false` for Python/Ruby/SQL/Bash (`minimal.rs:174-178, 203-207, 216-223`), so the short-circuit chain never stops before the new helper — it runs for essentially every module-level comment in those four languages. `pseudo.rs:446` calls the same `is_removable_comment`, and `pseudo` is the mode the PreToolUse hook selects for every code-file read (`cmd/rewrite/handlers.rs:44-53`). This is the hot agent-facing path.
- **`MAX_AST_NODES` cannot save it** (three reviewers confirmed independently): the counter increments at `minimal.rs:86` *before* `is_removable_comment` runs at `:95`. It bounds nodes visited, never per-node work — so the ADR-002 `ComplexityLimit` → lossless-passthrough degrade is never reached; the process just stalls.
- **Also violates** the project reliability rule (fixed upper bound on every loop). The sibling pattern already exists: `MAX_PARENT_WALK: usize = 500` at `transform/utils.rs:43`.

**⚠ RECORD BOTH DERIVATIONS — the reviewers disagree on complexity class and the resolver must adjudicate:**

| Reviewer | Class | Derivation |
|---|---|---|
| **performance** | **Θ(L² log L)** | `ts_node__prev_sibling()` calls `ts_node_parent()` then iterates the parent's children — but `parser.c:1900-1920` balances long repeat chains via `ts_subtree_compress`, so the hidden `_repeat1` structure is **O(log L) deep**. Each step is O(log L), not O(L). |
| **reliability** | **O(N³)** | `ts_node__prev_sibling` (`node.c:190-227`) re-iterates the parent's children **from the beginning** on every call (`ts_node_iterate_children` → `while (ts_node_child_iterator_next(...))`). One call from index *i* costs O(i); a walk from *i* costs O(i²); once per comment over N comments = **O(N³)**. |

Both read tree-sitter 0.25.10; they read the sibling-lookup cost differently. security and rust independently landed at "O(N²) with a worse-than-constant factor," which is consistent with either. **The resolver should check the actual `ts_node__prev_sibling` implementation and a timing measurement rather than picking a side from these reports.** Note the fix is the same either way, so this need not block the fix — only the severity narrative.

- **Fix (all six reviewers converged on the same shape)**: the header block is a *prefix property of the file*, not a property of a node. Compute `module_header_end_byte(root, source, language)` once per transform with a single **forward** pass (`next_named_sibling` / `TreeCursor` is not the re-scanning direction), thread it through `CommentWalkContext` and pseudo's `ctx`, and reduce the predicate to `is_root_child && node.end_byte() <= header_end`. O(1) per node, O(L) per file, semantics byte-identical.
- **Stopgap if the hoist is deferred**: `const MAX_HEADER_WALK: usize = 512;` bounds the loop but is **not** semantics-preserving (a longer header would be stripped, regressing #476) and leaves the super-linear factor intact. Pair it with the hoist; do not substitute.
- **Guard test required**: the five new unit tests are 2–4 line fixtures and cannot observe this; no `.py`/`.sh`/`.sql` fixture in `tests/fixtures/` is comment-dense, so `cargo bench` cannot either. Add a large contiguous-comment fixture with a wall-clock or criterion assertion.

**C1b (adjacent, regression, 80%)**: the rule is also *semantically* unbounded — the entire leading comment run is preserved however long, so a leading commented-out block or a 100%-comment file yields 0% reduction in minimal **and** pseudo. Plus a SQL-internal inconsistency: block comments are node kind `marginalia`, not `comment` (tree-sitter-sequel grammar.js:643), so a `/* … */` banner gets no header treatment **and** causes a following `-- SPDX` line to be stripped.

---

### C2. `pin_is_current()` fails closed, `is_hook_script_current()` fails open — wired in series → `skim init` never converges
`crates/rskim/src/cmd/init/state.rs:59-77` + `crates/rskim/src/cmd/init/install.rs:864-876`
**7 reviewers** · **Confidence 100%** (highest single: consistency/security/reliability 88-90%) · **HIGH**
*architecture, complexity, consistency, regression, reliability, rust, security.*

Both predicates are **new in this diff** and both answer "does the recorded pin equal the running binary?" They disagree on both degenerate inputs, in opposite directions:

| Condition | `pin_is_current()` (state.rs:60-76) | `is_hook_script_current()` (install.rs:868-876) |
|---|---|---|
| pin absent / empty | `None` → `false` — **stale** (fails closed) | `if let Some(pin)` never fires → falls through → **`true`** (fails open) |
| `resolve_skim_binary()` errors | `Err(_)` → `false` — **stale** | `let Ok(running)` never fires → **`true`** |
| pin present and differs | `false` | `false` (agree) |

**They are wired in series on one CLI path**, so the fail-closed gate opens the door and the fail-open gate closes it again:

1. `script_has_pinned_marker` (`init/mod.rs:173-177`) matches the literal prefix `export SKIM_HOOK_BINARY=` and therefore returns `true` for `export SKIM_HOOK_BINARY=''`; `parse_binary_pin_from_script` (`state.rs:436-455`) rejects the empty value and returns `None`. Pre-B5b installs produced exactly this script — the `create_hook_script` comment at `install.rs:930-934` says so verbatim.
2. `hook_is_current()` = true, `pin_is_current()` = **false** → fast path bypassed at `install.rs:506` ✓
3. `create_hook_script` → `is_hook_script_current()` → version ✓, marker ✓, commit ✓, **pin check skipped** → `true` → prints `Skipped: … (already v2.11.x)`, returns at `:912` without rewriting.
4. `skim doctor` → `!facts.pin_is_current` → `✗ binary pin mismatch (hook: ?, running: …)` → **exit 1**, advising `skim init --yes`.
5. Go to 2. **Forever.** `flags.force` is not a term anywhere in `install.rs` outside test fixtures — no operator escape hatch short of deleting the script by hand.

Every repeat also performs the full side-effect set (see S3 below). This is PF-015 defect (3) — *"a separate version-only gate still short-circuited the CLI"* — recurring one layer down, in the PR that cites PF-015 in its own comments. `test_init_rewrites_hook_when_pin_path_differs` covers only the non-empty-wrong-path case, so the divergence is invisible to CI.

- **Fix (unanimous)**: make `is_hook_script_current` fail **closed** so both gates agree — `let Some(pin) = … else { return false };` / `let Ok(running) = … else { return false };`. Better: extract one `pin_matches(pin: Option<&str>, running: Option<&Path>) -> bool` used by both, so they cannot drift again. Add a unit test asserting `is_hook_script_current` is `false` for `export SKIM_HOOK_BINARY=''` at a current version+commit.
- **Sub-finding (complexity, 85%)**: the `&& let Ok(running) = resolve_skim_binary()` arm at `install.rs:869` is defensively dead — `detect_state` already calls `resolve_skim_binary()?` at `state.rs:121`, so `run_install_single` aborts first. The `let-else` form removes it.
- **Open question (architecture, 75%)**: should `is_hook_script_current` survive at all? `DetectedState` already carries `hook_version`/`hook_commit`/`hook_binary_pin` parsed from the same file, and `state.rs:175` claims single-source-of-truth for them. Deleting the second gate would close the recurrence permanently rather than keeping two gates aligned. **Not investigated far enough to assert — flagged for the resolver to decide.**
- **Reachability caveat, preserved honestly**: complexity put "confidence a user actually hits it" at 55% and regression/reliability at higher; all agreed the two-gate *structure* is the durable defect regardless of population size. Nobody constructed the end-to-end fixture.

---

### C3. Fourth hand-rolled canonical-path derivation, in a file that already has the helper
`crates/rskim/src/cmd/doctor/mod.rs:483-487`
**5 reviewers** (+ security at 74% as a suggestion) · **Confidence 100%** (highest single: consistency 96%) · **HIGH**
*architecture, complexity, consistency, reliability, rust.*

```rust
let running = std::env::current_exe().ok()
    .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)))
    .map(|p| p.to_string_lossy().into_owned())
    .unwrap_or_else(|| "?".to_string());
```

This is semantically identical to `current_exe_canonical()` **in the same file at `doctor/mod.rs:96-100`**, which is *already called* at `doctor/mod.rs:37` in the same `run()` flow — the value was computed and thrown away. The `.ok().or(Some(p))` closure is total, so the `and_then` is a `map` written the long way.

**Architecture supplies the root cause the others missed — this is the highest-leverage item in the review:**

`resolve_skim_binary()` declares itself *"the single source of truth"* in its own doc comment (`init/helpers.rs:14-25`), but it is `pub(super)` inside a **private** `mod helpers` (`init/mod.rs:20`). `pub(super)` from `cmd::init::helpers` means visible in `cmd::init` and below. Therefore **`cmd::doctor` and `cmd::rewrite` are physically incapable of calling it.** The claim is unenforceable by construction, and the diff proves it — doctor hand-rolls a copy in the same commit that makes the claim.

The three surviving implementations disagree on failure:

```rust
init/helpers.rs:26-31   Ok(std::fs::canonicalize(&p).unwrap_or(p))            // keeps RAW path
doctor/mod.rs:96-100    .map(|p| std::fs::canonicalize(&p).unwrap_or(p))      // keeps RAW path
rewrite/hook.rs:86      current_exe().and_then(canonicalize).ok()             // DISCARDS raw → None
```

`rewrite/hook.rs:86` is the value that decides hook-exec-time drift. `SKIM_HOOK_BINARY` is written by `resolve_skim_binary()`, which *keeps* the raw path; `DriftEnv.current_exe` *drops* it, so the comparison is skipped and drift **silently fails open** on exactly the symlinked machines (Homebrew cellar, `/tmp → /private/tmp`) the KB flags as the risk. (Pre-existing line — see P1 — but it converges for free under the same fix.)

- **Fix (two options, both endorsed)**:
  - *Minimal*: widen to `pub(crate)`, call it from doctor. Or thread the already-computed `running_path` from `doctor/mod.rs:37` into `hook_status_line` as a parameter.
  - *Structural (architecture, preferred)*: promote the concept to `crates/rskim/src/cmd/provenance.rs`, modelled on the existing `cmd/integrity.rs` — which is this pattern done right (`pub(crate)`, four-state enum, consumed by both `doctor` and `rewrite::hook`). Then `init/helpers.rs`, `doctor/mod.rs:96`, and `rewrite/hook.rs:86` all delegate, and the triplicated `"unknown"` rule (S5) collapses too. **This single change subsumes C3, S1, S5, and P1.**
- **Secondary (complexity + consistency + architecture)**: the inline `current_exe()` also breaks `hook_status_line`'s purity contract — every other environment-derived value (`compiled_version`, `compiled_commit`) arrives as a *parameter*. That is why the three new unit tests can only assert `line.contains("running:")`: they cannot assert the path, cannot construct a pin-equals-running case, and cannot assert the branch does *not* fire when paths match. **This is PF-015 defect (2) verbatim, reproduced inside the fix that cites PF-015.**

---

### C4. New test bypasses `skim_sandboxed` — PF-017 re-introduced by the PR that closes it
`crates/rskim/tests/cli_init.rs:1732` (two shell-outs, `:1737` and `:1758`)
**5 reviewers** · **Confidence 100%** (highest single: consistency 92%) · **HIGH** (testing) / MEDIUM (compliance, consistency, rust)
*testing, compliance, consistency, rust, security. Note: compliance + security at the same site were **merged, not boosted** — one gap seen through two regulatory lenses is not corroboration.*

`test_init_rewrites_hook_when_pin_path_differs` uses the legacy `skim_init_cmd(config)` (`cli_init.rs:17-22`), which sets **only** `CLAUDE_CONFIG_DIR`. It leaves `HOME`, `SKIM_CACHE_DIR`, `SKIM_WRAPPERS_DIR`, `GEMINI_CONFIG_DIR`, `COPILOT_CONFIG_DIR`, `CODEX_HOME`, and `CRUSH_CONFIG_DIR` pointing at the developer's real home. The sibling test added ~46 lines later, `test_init_wrappers_bypasses_fast_path` (`:1785`), correctly routes through `common::skim_sandboxed`. This is the same PR that promotes `skim_sandboxed_with_bin` to *"the single authoritative sandbox env-var block … rather than hand-rolling their own env block (PF-017)"* (`tests/common/mod.rs:39-49`).

**⚠ Preserve compliance's nuance — the leak is LATENT, not LIVE today.** Compliance traced every home-reaching path reachable under this test's env and found containment:
- `detect_installed_agents` (`init/flags.rs:220-242`) enters **override mode** because `CLAUDE_CONFIG_DIR` is set, so only claude-code is considered, via the TempDir. Real `~/.gemini`/`~/.codex`/`~/.copilot`/`~/.crush` are never probed.
- Guidance resolves through `InstructionEnv.claude_config_dir` (`cmd/session/types.rs:197-203`) → TempDir, not real `~/.claude/CLAUDE.md`.
- `maybe_install_wrappers` receives `wrappers: None` and stdin is not a TTY under `assert_cmd`, so it early-returns before `resolve_skim_binary()` — real `~/.skim/bin` untouched.

**The arming condition**: containment rests entirely on `any_override == true` and `wrappers: None`. Add `--wrappers` to this test — the natural next step given this PR's own `wrappers_blocks_fast_path` work — and it writes symlinks into the real `~/.skim/bin` with no consent gate, because the TTY prompt is skipped under `Some(true)`.

**Two live sub-exposures the containment does NOT cover** (do not lose these under the "latent" headline):
- **testing (90%)**: `SKIM_CACHE_DIR` is unset, so cache and `hook.log` resolve to the developer's **real** `~/.cache/skim` right now. Analytics writes are blocked only by `SKIM_DISABLE_ANALYTICS=1` from `common::skim()`.
- **security (75%, unresolved divergence)**: security claims `resolve_agent` (`flags.rs:262-270`) selects the first agent whose *real-`HOME`* config dir exists, so on a machine without `~/.claude` but with `~/.gemini` the test would install into the real `~/.gemini/`. Compliance's containment argument is about `detect_installed_agents` (`flags.rs:220-242`), a **different function**. **Open question: do these two code paths agree? Nobody reconciled them.** Routing through `skim_sandboxed` makes the question moot.

- **Fix**: `common::skim_sandboxed(home)` + `fs::create_dir_all(home.join(".claude"))` first (override mode requires the dir to exist), reading the hook from `home.join(".claude/hooks/skim-rewrite.sh")`.
- **Do NOT scope-creep**: the 24 *pre-existing* `skim_init_cmd` call sites are Category 3 (see P3). Testing suggests the durable fix is redefining `skim_init_cmd` to delegate to `skim_sandboxed_with_bin` in one edit — worth a tracking issue, not this PR.

---

### C5. `wrappers_blocks_fast_path` ignores `flags.project` — permanent idempotence break
`crates/rskim/src/cmd/init/install.rs:169-175`
**2 reviewers** (+ architecture at 72% as a suggestion) · **Confidence 98%** (highest single: rust 92%) · **HIGH**
*regression (90%), rust (92%), architecture (72%).*

The predicate returns `true` for `Some(true)` **regardless of `flags.project`**, but the effect it gates — `maybe_install_wrappers` — is guarded by `if !flags.project` at *both* call sites (`install.rs:532` dry-run, `:567` real). Unlike `--permissions`, `--wrappers` is **not** rejected alongside `--project`: `flags.rs:389-395` only errors for `permissions == Some(true) && project`; `flags.rs:317-333` only rejects `--wrappers` + `--no-wrappers`.

So `skim init --project --wrappers --yes` on a fully-current install: **before** → `print_already_up_to_date()`, zero writes. **After** → full reinstall on every single invocation, and **zero wrappers installed**. Never converges.

- **Fix**: `Some(true) => !flags.project` (keep the `None => false` arm — it is load-bearing; without it every non-TTY `skim init` reinstalls, which is #478). Or add the mutual-exclusion guard in `flags.rs` mirroring `:389-395`. Either way add a test: all three new unit tests build flags with `project: false` (`install.rs:2394`), so this case is unreachable from the current suite.

---

## Blocking Issues — Preserved Singletons

Each of these was produced by exactly one lens. **They are not noise** — they are the findings that only a specific lens could reach, and several are the sharpest items in the review.

### S1. The PATH-wrapper interception surface has no pin invariant, by construction
`crates/rskim/src/cmd/doctor/mod.rs:544-570` — **architecture only, 85%** — HIGH (Should-Fix)

`print_wrapper_section` calls `read_dir`, filters `is_symlink()`, and prints `✓ {dir} ({count} symlinks)`. It **never calls `read_link`**, never resolves a target, never compares to the running binary. It returns `()` — `doctor/mod.rs:64` invokes it with no assignment, unlike `print_hook_section` at `:57` and `print_staleness_section` at `:72`, which both feed the `drift` flag. **The wrapper section cannot contribute to the exit code by construction.**

Consequence: every symlink in `~/.skim/bin/` can point at clone A's binary while clone B runs, and `skim doctor` prints `✓ … (8 symlinks)` and exits `0 HEALTHY`. The surface that exists *specifically to intercept sub-agents that bypass hooks* is the surface with no provenance check, while CLAUDE.md sells doctor as *"Exit 0 healthy / 1 on any drift — works as a CI pre-flight."*

Not purely pre-existing: before this PR the two-clone case hit the fast path and *nothing* was repaired. After it, the hook is repaired and the wrapper is not — which makes doctor's green wrapper line **actively misleading about a state the tool now knows about**.

Fix: make it `fn print_wrapper_section() -> bool`, resolve each symlink via `read_link` + the shared canonicalizer, count stale targets, and `if print_wrapper_section() { drift = true; }` at `:64`. Nearly free once C3's shared module exists.

### S2. Analytics KB is unreachable, cites a pitfall that does not exist, and misdescribes its own subject
**documentation only** — three separate HIGH findings, all in `.devflow/features/analytics/KNOWLEDGE.md` (added in commit `f00e37a`)

- **Unreachable (98%)** — `.devflow/features/index.md` was changed in this diff, but **only** to rewrite the hook-binary-pinning line. The index still holds exactly 9 entries and `grep -c analytics` returns **0**. `index.md` is the relevance-matching cache orchestrators consume to select `FEATURE_KNOWLEDGE`. A KB with no index row is real to a human browsing the directory and **nonexistent to every agent** — 154 lines of researched context delivering zero value. Fix: one row, `- **analytics** — {directories from frontmatter} — {description from frontmatter}`. The frontmatter is authoritative; index.md is only a cache.
- **PF-002 does not exist (95%)** — cited at `:24`, `:126`, `:144`, `:154`, carrying an entire narrative about `SKIM_CACHE_DIR` drift. The ledger runs `PF-001`, then **`PF-003` through `PF-019`**. `grep -rn 'PF-002' .devflow/` returns nothing anywhere. This violates the verbatim-IDs-only Iron Law of `devflow:apply-decisions`, and fabricated citations are worse than none — they consume a lookup and destroy trust in the KB's 8 *correct* citations (ADR-001, PF-001 both verified to exist). Fix: file the real pitfall via `assign-anchor` and cite the returned ID, or drop the ID and describe the drift narratively (Rule 6 already explains the mechanism fully).
- **Rule 5 misdescribes its own subject (95%)** — `:69` states, with a literal snippet, *"then `for rec in records { persist_record(&rec); }` writes serially."* The actual `record_file_ops` background thread (`analytics/mod.rs:1191-1199`) opens **one** connection for the whole batch and **never calls `persist_record`**; it prunes once per batch guarded by `if !records.is_empty()`. `persist_record` (`mod.rs:1003-1008`) is a *different* per-row helper belonging to the subcommand path. The dependent claim at `:111` ("prune runs after each `persist_record` call") is wrong for the file-op path too. **This is the KB violating its own Rule 3 — "Two recording paths — never conflate them."** An agent trusting it would either replicate per-row connections in a new recorder (undoing a real optimization) or "discover" the batching as a bug and revert it.

### S3. A repeat `skim init --wrappers` now performs the full install side-effect set
`crates/rskim/src/cmd/init/install.rs:503-514` — **regression only, 85%** — MEDIUM

Regression traced `install.rs:503-514 → 557-569 → execute_install (630-679)` and enumerated what a `--wrappers` re-run on an up-to-date install now does, having previously printed "Already up to date" and written nothing:

| Effect | Consequence |
|---|---|
| `patch_settings` → `backup_settings` (`:1271-1286`) | **Unconditional `fs::copy` overwrites `settings.json.bak` with the already-skim-patched settings — the user's pre-skim backup is destroyed, unrecoverably.** |
| `inject_guidance` (`guidance.rs:168`) | Guidance file rewritten |
| `create_hook_script` (`:880`) | Script not rewritten, but **the SHA-256 manifest is recomputed and rewritten** |
| `install_search_integration` (`:686`) | Installs search git hooks into **whatever repo the cwd happens to be**, and spawns a **detached background `skim search --build`** |
| `migrate_cursor_legacy_settings` / `migrate_copilot_legacy` | Re-run |

The new E2E `test_init_wrappers_bypasses_fast_path` asserts only that `"Already up to date"` is absent and `"Wrappers:"` present — the side-effect surface is untested in either direction.

Fix (surgical, and it resolves C5 as a side effect): drop `wrappers_blocks_fast_path` from the conjunction and run wrappers *inside* the fast path before returning:
```rust
if state.hook_installed && state.hook_is_current() && state.pin_is_current()
    && guidance_current && !permissions_blocked && manifest_present {
    print_already_up_to_date();
    if !flags.project { maybe_install_wrappers(flags.wrappers, flags.dry_run)?; }
    return Ok(std::process::ExitCode::SUCCESS);
}
```
If the current shape is kept deliberately, at minimum make `backup_settings` a no-op when `.bak` already exists and the live file already contains the skim entry.

### S4. `doctor/mod.rs:371-372` docblock still carries the exact false claim fix #479 was written to delete
**consistency (95%) + documentation (90%)** · **2 reviewers → Confidence 100%** · MEDIUM (Should-Fix)

The `hook_status_line` docblock states `Unreadable → drift (✗), names the suppression coupling.` This PR **removed** precisely that claim from the branch body (`mod.rs:412-418`) and **added a test forbidding it** (`mod.rs:1002-1008`: *"Unreadable message must NOT claim drift detection is silenced"*).

Documentation calls this the most pointed defect in the diff, and the framing is worth preserving: **fix #479 was *entirely* a comment-correctness fix — its whole thesis is that a wrong comment about drift suppression is the bug.** The parallel comment in `hook.rs:598-608` was corrected. The docblock 40 lines above the code it describes was missed. So the exact false statement #479 exists to eliminate survives in this file, now contradicted by both the code below it and a test in the same file. It is also the first thing a maintainer reads.

*(Anchor line falls outside the diff — carried in the Git agent's summary comment, not inline.)*

### S5. The `"unknown"`-commit rule is triplicated, and the deleted `else`-fallback was the net that absorbed exactly that drift
`crates/rskim/src/cmd/doctor/mod.rs:466-489` — **complexity only, 90%** — HIGH

The rule "compiled commit == `"unknown"` ⇒ commit comparison indeterminate ⇒ treat as OK" is now implemented independently in three places:
- `init/state.rs:97-108` (`hook_is_current()`)
- `init/install.rs:856-863` (`is_hook_script_current()`)
- `doctor/mod.rs:466-470` (`hook_status_line()`) — **new in this PR**

Complexity verified the dead-code claim for the removed `stale` terminal and it holds **today**: with `hook_uses_pinned_binary` forced true by the early return at `:449`, `hook_is_current()` reduces to `version_ok ∧ commit_ok` under both branches, so `commit_ok ∧ version_ok ⇒ hook_is_current ⇒ !pin_is_current`. Reliability and regression independently derived the same proof. **All three also flagged that it is sound only in combination with the `commit_ok`/`"unknown"` fix landed in the same hunk — the two changes must never be separated.**

The complexity cost is the point: the deduction is sound *only because three separately-maintained copies agree*, with no shared predicate and no test asserting the equivalence. If `hook_is_current()` gains a fourth condition, the `else` at `:479-488` stops being a proof and becomes a **fabricated diagnostic** — printing `binary pin mismatch (hook: X, running: Y)` unconditionally, including when `X == Y`. The deleted `else` was the safety net that previously absorbed exactly that class of drift. A provenance tool that confidently misreports the cause is the failure shape PF-015 is named for.

Fix: extract `commit_matches(hook_commit: Option<&str>, compiled_commit: &str) -> bool`, call it from all three sites, and add a test pinning `hook_is_current() == (version_ok && commit_matches(..))` so a future divergence fails loudly instead of silently invalidating the removal.

### S6. Security: the pin gates validate `SKIM_HOOK_BINARY`, but the hook execs `$_SKIM_BIN`
`crates/rskim/src/cmd/init/install.rs:864-876` interacting with `:895-914` — **security only, 85%** — HIGH (Should-Fix)

`generate_hook_script` (`cmd/hooks/mod.rs:578-582`) writes the binary path into **two** independent shell constructs: `export SKIM_HOOK_BINARY={quoted}` and `_SKIM_BIN={quoted}` — and it is `_SKIM_BIN` that is `exec`'d. Both new gates parse **only** `SKIM_HOOK_BINARY` via `parse_binary_pin_from_script`. Nothing anywhere parses `_SKIM_BIN`. A script whose two values diverge passes both new gates while exec'ing a different binary.

The fail-open chain is self-reinforcing: a `_SKIM_BIN`-only edit → `Tampered` → doctor correctly says *"run `skim init` to reinstall"* → `is_hook_script_current` doesn't consult the manifest at all and returns `true` → the early-return branch calls `compute_file_hash` + `write_hash_manifest` on the **divergent on-disk bytes** (`:900-906`) → doctor now reports `Verified`, exit 0. **Doctor's own remediation advice launders the tamper into a clean bill of health.**

Security frames the threat model correctly and it should be preserved: anyone who can write the hook script already has code execution, so this is **not** a privilege escalation. The value at stake is *detection integrity* — the manifest, `pin_is_current()`, and doctor's exit code are marketed as a CI pre-flight, and a control that blesses a divergent script defeats the only mechanism that exists to notice it.

Fix (two independent, both cheap): (a) stop duplicating the value in the generated script — `exec "$SKIM_HOOK_BINARY"` — so there is one field to check, mirroring the single-source-of-truth the PR is built around; (b) make the early return integrity-aware so `Tampered`/`Unreadable` falls through and **regenerates** rather than re-hashing in place.

**Reliability reached the same self-heal defect from the other direction (S-1, 85%) — 2 reviewers, confidence 95%**: the two *new* fast-path terms (`pin_is_current()` at `:506`, `!wrappers_blocked` at `:509`) are each a new way to fall through to `create_hook_script` on an otherwise-current install. So `skim init --wrappers` on a machine with a tampered script now reaches `:900` and overwrites the good manifest. Before this diff the fast path returned at `:512` and left the `Tampered` verdict intact. **Ownership nuance both reviewers stated: the self-heal block is pre-existing (#471 Group 4); what this PR adds is the reachability.** That is why it is Should-Fix rather than Blocking.

---

## Blocking Issues — Remaining ≥80% (condensed)

| Finding | Location | Reviewers | Conf | Sev |
|---|---|---|---|---|
| `Mode::Minimal` rustdoc contradicts new behaviour on a **published crate** (docs.rs contract, now provably false for Py/Rb/SQL/Bash) | `rskim-core/src/types.rs:567`, `:642` | documentation, consistency | 98% | HIGH |
| hook-binary-pinning KB says `hook_is_current()` = version + pinned format — it is version + pinned format **+ commit**. Breaks the KB's own `commit_ok ∧ version_ok ⇒ hook_is_current` argument, so an agent auditing the dead-code removal derives a contradiction and may "restore" the terminal | KB `:56`, `:150`, `:252` | documentation | 95% | MEDIUM |
| KB says "all **five** conditions" above a block listing **seven** (self-contradicts at `:150`). The two a reader would drop are the two this PR added | KB `:60` | documentation | 98% | MEDIUM |
| `pin_is_current()` re-resolves the binary instead of reading `self.skim_binary` — which `detect_state` populates from that exact helper at `state.rs:121`. Makes the fixture's `skim_binary` value dead, the predicate non-hermetic, and hides syscalls behind a struct getter | `init/state.rs:59-77` | consistency, rust (+architecture 74%) | 98% | MEDIUM |
| Fast path still swallows `--dry-run` — `PF-018`'s recorded resolution named **three** items and two landed | `install.rs:504-514`, `:522` | consistency | 88% | MEDIUM |
| New status string breaks the reason-chain vocabulary (`running:` where siblings use `binary:`) and prints `{pin}` twice, since `:492-494` already emits `pin: {pin}` | `doctor/mod.rs:488` | consistency | 90% | MEDIUM |
| `wrappers_blocks_fast_path` filed under the "Permissions install helpers" banner, displacing `permissions_blocks_fast_path` from its own section | `install.rs:169` | consistency | 92% | MEDIUM |
| "mirrors `permissions_blocks_fast_path`" is inaccurate on the **load-bearing arm** (permissions' `None` *can* block; wrappers' never does) and the arm ordering is inverted between the two | `install.rs:157-174` | consistency | 88% | MEDIUM |
| Seven-term fast-path condition: mixed polarity, mixed eager/lazy, two terms perform hidden syscalls, and it discards *why* it declined | `install.rs:504-511` | complexity | 90% | MEDIUM |
| `resolve_skim_binary()` degrades **silently** on `canonicalize()` failure — the `current_exe()` half is exemplary (`map_err` + actionable hint), the `canonicalize` half has neither error nor `debug_log!`. The raw path then flows into the hook pin (`if [ -x "$_SKIM_BIN" ]` always fails → bare `exec skim` on `$PATH` = the wrong-clone hazard ADR-004 exists to eliminate) and into 8 wrapper symlinks | `init/helpers.rs:33` | reliability | 84% | MEDIUM |
| Re-canonicalizing the pin read from the script can only **loosen** the comparison (ADR-004 writes it canonical, so it's a no-op for legit scripts and only changes hand-written ones — the case the check exists to catch) + adds a symlink-retarget TOCTOU | `state.rs:67-72`, `install.rs:871-872` | security | 82% | MEDIUM |
| `is_module_header_comment` uses a `_ =>` wildcard where both siblings (`is_comment_node`, `is_doc_comment`) enumerate **every** `Language` — opts new languages out of header preservation with zero compiler signal, against CLAUDE.md's compiler-guided "Adding a Language" procedure | `minimal.rs:292-296` | architecture | 88% | MEDIUM |
| Analytics KB never mentions credential scrubbing though `token_savings` persists `original_cmd` (500-byte raw command text) + `project_path` for 90 days and `skim stats` re-displays them. Code is **correct** (`build_analytics_label` → `scrub_credential_url`); the KB that says "read me before adding a recording path" omits the invariant | KB `:122-128`, `:30` | documentation, compliance | 95% | MEDIUM |
| End-state residue — 5 instances of tombstone comments and rotting line refs (`doctor/mod.rs:473-474`, `helpers.rs:24-25` citing `install.rs:895-906` which this commit deleted, `state.rs:56-58`, the S4 docblock, `doctor/mod.rs:~847` "at line ~477") | multiple | complexity, documentation | 98% | HIGH |
| Wrong source line ref in a new comment: `transform/mod.rs:737` cites "trailing-newline restore at `minimal.rs:406-408`" — that is now the `"Range exceeds source length"` branch; the restore is at `:465`. **Drifted within its own commit** | `transform/mod.rs:737` | documentation | 92% | MEDIUM |
| CLAUDE.md describes doctor's pin state as SHA-only; it is now also path-based. An agent debugging `binary pin mismatch` will hunt a commit divergence that by construction cannot exist (the terminal is reachable only when version **and** commit match) | `CLAUDE.md:83` | documentation | 88% | MEDIUM |
| `docs/modes.md:10` "Non-doc comments (except headers)" reads as universal; applies only to `{Python, Ruby, SQL, Bash}`. Rust `// SPDX-`, C `/* Copyright */`, TS `/* @license */` are all still stripped — and the kept-column *is* correctly qualified, so the two halves of one row disagree | `docs/modes.md:10` | documentation, regression | 95% | MEDIUM |
| Doctor exit-code test asserts `.failure()` (any non-zero) not `.code(1)`, in a test *named* for exit 1, for a contract CLAUDE.md sells as a CI pre-flight | `cli_doctor.rs:204` | testing | 88% | MEDIUM |
| `test_init_wrappers_bypasses_fast_path` has no same-harness control proving the fast path was reachable — can pass vacuously, so the #478 regression it exists to catch could silently stop being caught | `cli_init.rs:1786-1818` | testing | 82% | MEDIUM |
| `test_pin_is_current_matching_path_returns_true` does `let Some(..) else { return; }` — an early return from a `#[test]` is reported as a **pass**, not a skip. The only test asserting the affirmative branch can become a silent no-op | `state.rs:1054` | testing, rust (+regression 70%) | 95% | MEDIUM |

---

## Should-Fix (Category 2, condensed)

| Finding | Location | Reviewers | Conf |
|---|---|---|---|
| `resolve_skim_binary()` — the PR's central unification — has **zero direct tests and no symlinked-binary coverage**. The KB itself says "a green CI run is not proof the path-comparison invariant holds." `test_pin_is_current_matching_path_returns_true` derives its expected value with the same algorithm the implementation uses → tautology on unsymlinked paths | `init/helpers.rs:26` | testing | 90% |
| Bash is in the `is_module_header_comment` allowlist with **zero** behavioural coverage (only `test_bash_minimal_mode_runs_without_error`, an `is_ok()` smoke test). Bash is where the shebang/header interaction is least obvious | `minimal.rs:290` | testing | 90% |
| The six assertion inversions deleted the **negative-case** coverage of the rule they were inverted for. Ruby's strippable comment moved into a class body (different code path); SQL's replacement asserts only `len <` and passes even if the blank-line break broke entirely; Bash has none. Python is the only affected language with a live negative guard | `ruby_transform.rs:158`, `sql_transform.rs:126`, `integration.rs:2374` | regression | 88% |
| `check_hook_integrity` narrows the four-state `ScriptIntegrity` to `bool`, so `Unreadable` logs **nothing** on `hook.log` — the only channel hook mode may write to. Doctor consumes the rich enum; the hook gets a bool that cannot express "could not verify." PF-016 rules out exactly this silence | `rewrite/hook.rs:577-609` | architecture | 82% |
| Blank-line policy implemented twice (`trim_and_normalize` text + `normalize_line_map_blanks` map) — **second desync in this pair**. PF-019's rule is "a derived index must be DERIVED, not maintained in parallel"; the fix adds a *second* hand-mirrored rule to the parallel array. The new invariant test asserts a **documented exception** in the same test — that is a snapshot, not an invariant | `minimal.rs:444-470`, `transform/mod.rs:328-359` | architecture, rust | 98% |
| `print_wrapper_install_result` PATH-blurb gating (`created + updated > 0`) untested on either side — the idempotent-re-run branch it was written for is never executed | `install.rs:781-789` | testing | 88% |
| `test_sql_minimal_reduces_tokens` repoint hides that minimal now yields **~0%** reduction on `sql/simple.sql`, while `docs/modes.md` still advertises 15-30% | `integration.rs:2374`, `docs/modes.md:10` | regression | 85% |
| Module-header rule unbounded in length + the language split `{Py,Rb,SQL,Bash}` matches `{L : is_doc_comment(L) ≡ false}` but **not** its stated motivation ("copyright, SPDX, provenance") — `skim --mode=minimal` keeps the licence header on `.py` and drops it on `.rs` | `minimal.rs:292-331` | regression, rust, architecture | 85% |
| `check_hook_integrity` docblock omits the `Unreadable` case entirely ("false if valid, missing, or check was skipped" — `Err(_)` is none of those) | `rewrite/hook.rs:559-562` | documentation | 85% |
| `hook_status_line` is now ~131 lines / cyclomatic ~13, doing four separable jobs; `let pin = …unwrap_or("?")` computed twice (`:460`, `:500`) | `doctor/mod.rs:377-507` | complexity | 85% |

---

## Pre-existing (Category 3 — informational, does not block)

- **P1.** `DriftEnv::from_process()` (`rewrite/hook.rs:86`) discards the raw path on canonicalize failure while `SKIM_HOOK_BINARY` retains it → hook-exec drift detection **fails open** on exactly the symlinked machines the KB flags. *architecture + complexity, 95%.* Folded into C3's fix for free.
- **P2.** `skim doctor` reports `– {agent} (detection error: {e})` with the **neutral** marker and exits **0** — the one remaining hole in the exit-code contract. Every other unanswerable channel is drift. Reachability unchanged by this diff. *reliability, 91%.*
- **P3.** 24 pre-existing un-sandboxed `skim_init_cmd` call sites in `cli_init.rs`. Already recorded as a "Known remaining gap" in the KB. *testing, 92%.*
- **P4.** `is_go_doc_comment` (`minimal.rs:235-260`) has the identical super-linear shape walking *forward*; lower severity because Go doc comments sit in short runs. The C1 hoist generalizes to it. *performance, 85%.*
- **P5.** `doctor/mod.rs:494` emits `run ./target/release/skim init --yes to update` — a repo-local dev path shipped to end users. *consistency.*
- **P6.** `is_inside_function_body` treating Ruby `body_statement` as a function body makes a class-level comment's fate depend on whether it precedes the first method. *regression.*

---

## Suggestions (Lower Confidence, 60-79%)

- **Unescaped hook-script content in doctor's status line** — `doctor/mod.rs:488` (security, 68%) — `{pin}` is read verbatim from the script and printed straight to the terminal; ANSI/CSI bytes can rewrite or clear the surrounding report, suppressing the very drift line meant to be read. ADR-012 governs skim's *reading* role; this is skim speaking, so escaping here is consistent with it, not a violation.
- **Doctor exit-code surface widened by `|| !facts.pin_is_current`** — `doctor/mod.rs:459` (regression, 75%) — anyone running doctor from a *different installation* than the one that installed the hook now flips 0 → 1. That is the intended ADR-004 detection, but CI that builds skim in one job and runs `skim doctor` from a cached artefact in another will start failing. **Worth a CHANGELOG note.**
- **Tree-sitter ERROR nodes make header preservation nondeterministic** — `minimal.rs:301-304` (reliability, 68%) — when the parser wraps the top of a file in an `ERROR` node, comments nest one level deeper, `is_root_child` is false, and the header is stripped. Same file parses differently before and after an unrelated syntax error appears. Worth a fixture.
- **Analytics KB frontmatter `directories` includes the whole crate root** — KB `:6` (documentation, 75%) — `["crates/rskim/src/analytics", "crates/rskim/src"]`; the second entry matches essentially every `rskim` change, so this KB will be selected as `FEATURE_KNOWLEDGE` for unrelated work and dilute the context budget. (Also: `updated: 2026-06-25` on a file committed 2026-08-18, while the sibling KB in the same diff correctly advanced — 78%.)
- **Sandbox hygiene gaps in `skim_sandboxed_with_bin`** — `tests/common/mod.rs:79`, `:82-84` (compliance, 65-70%) — `SKIM_HOOK_COMMIT` is not `env_remove`d though `DriftEnv::from_process()` reads all three `SKIM_HOOK_*`; `SKIM_ANALYTICS_DB` is not neutralised though it takes precedence over `SKIM_CACHE_DIR`. Both currently harmless (blocked by `SKIM_DISABLE_ANALYTICS=1`) — defense in depth would make containment independent of that second control.

---

## Action Plan

Ordered by leverage. Items 1-3 each close multiple findings at once.

1. **Hoist the module-header computation out of the per-node predicate** (C1) — one forward pass per file storing `header_end` on `CommentWalkContext` / pseudo's `ctx`; predicate becomes an integer compare. Semantics byte-identical. **Then measure** a comment-dense fixture and add it as a durable guard (neither the 5 new unit tests nor `cargo bench` can currently observe this). Also add `.take(2)` to the gap newline count. *Do not ship the `MAX_HEADER_WALK` stopgap alone — it is not semantics-preserving.*
2. **Align the two pin gates fail-closed** (C2) — `let-else` in `is_hook_script_current`, ideally via one shared `pin_matches()`. Add the `export SKIM_HOOK_BINARY=''` unit test. Drop the dead `Ok(running)` arm. This is the finding that leaves `skim doctor` at exit 1 recommending a command that provably cannot fix it.
3. **Promote provenance to a module `doctor` and `rewrite` can actually reach** (C3 + S1 + S5 + P1) — `cmd/provenance.rs` modelled on `cmd/integrity.rs`, exposing `running_binary()`, one `canonical()` policy, and `commit_matches()`. Delete the inline `current_exe()` at `doctor/mod.rs:483-487`, collapse the triplicated `"unknown"` rule, converge `rewrite/hook.rs:86`, and give `print_wrapper_section` the pin invariant + `-> bool` so the wrapper surface can affect the exit code. If the full module is too much for this PR, the minimum is `pub(crate)` + call it, and thread `running_path` into `hook_status_line` as a parameter (which also restores its testability).
4. **Fix `wrappers_blocks_fast_path` for `--project`** (C5) — `Some(true) => !flags.project`, keeping the `None => false` arm. Prefer regression's restructure (run wrappers *inside* the fast path), which resolves C5 **and** S3's backup-clobbering in one edit. Add the `--project --wrappers` test.
5. **Route `test_init_rewrites_hook_when_pin_path_differs` through `common::skim_sandboxed`** (C4) — plus `.code(1)` instead of `.failure()`, the fast-path control assertion, and `.expect()` instead of the silent `return`. All one-to-three-line fixes. Leave the 24 pre-existing call sites alone; file a tracking issue for redefining `skim_init_cmd` to delegate.
6. **Doc-artifact batch** — add the `analytics` row to `.devflow/features/index.md`; remove or replace the four PF-002 citations; rewrite KB Rule 5 to match the batched persist path; add the credential-scrubbing anti-pattern; fix `hook_is_current` (+commit) at KB `:56`/`:150`/`:252` and "five"→"seven" at `:60`; update `types.rs:567`/`:642`, `CLAUDE.md:83`, `docs/modes.md:10`; correct the `doctor/mod.rs:371-372` docblock; fix the `transform/mod.rs:737` line ref; strip the 5 residue instances. All mechanical text edits, but three of them are HIGH and one (S4) is the exact defect class this PR exists to fix.
7. **Run the verification gate** — the clippy and nextest commands in the Verification Gap section, serially, outside any fan-out agent.

---

## Convergence Status

**Cycle**: 1
**Prior Resolution**: none — no `resolution-summary.md` exists for this branch
**Prior FP Ratio**: **N/A — no false-positive history exists.** This is the first review cycle for this branch, so there is no prior resolution pass to have classified anything as a false positive. Any convergence or FP-ratio figure quoted for this run would be fabricated.
**Assessment**: **First cycle.** Convergence cannot be assessed — there is nothing to converge from. The next cycle will have a baseline.

What *can* be said about this cycle's internal signal, distinct from convergence-over-time: cross-reviewer agreement was unusually strong. Five defects were found independently by 5-7 reviewers each, from lenses that were asking different questions (performance measured call frequency, security measured DoS reachability, reliability measured loop bounds, and all three landed on `minimal.rs:292-331`). Reviewers also cross-verified each other's *negative* results — three independently proved the removed `"stale"` doctor terminal was genuinely dead, and each noted it is safe **only** in combination with the `commit_ok`/`"unknown"` fix in the same hunk. That is high-quality signal, but it is agreement among source-reasoning agents, **not** empirical verification (see the Verification Gap). Treat the clusters as high-confidence *hypotheses with strong corroboration*, and let the fix pass's clippy/test run be the arbiter.

**Unresolved open questions** (stated rather than investigated, per instruction):
1. Which complexity derivation for C1 is correct — performance's Θ(L² log L) or reliability's O(N³)? They read the same tree-sitter version differently. The fix is identical either way; only the severity narrative depends on it.
2. Should `is_hook_script_current` exist at all once `DetectedState` carries the same parsed fields (architecture, 75%)?
3. Do `resolve_agent` (`flags.rs:262-270`) and `detect_installed_agents` (`flags.rs:220-242`) agree on override-mode containment? Security and compliance reasoned from different functions and reached different conclusions about C4's live blast radius.
4. What is the real-world size of the empty-pin population that C2's non-convergence requires? The `install.rs:930-934` comment says pre-B5b code produced it; nobody confirmed the affected release range. The structural divergence stands regardless.
