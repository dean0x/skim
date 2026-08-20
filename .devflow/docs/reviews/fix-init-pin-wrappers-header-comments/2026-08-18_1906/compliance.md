# Compliance Review Report

**Branch**: fix/init-pin-wrappers-header-comments -> main (PR #488)
**Date**: 2026-08-18 19:06
**Diff**: `git diff main...HEAD` — 21 files, +1211/-158

## Scope Declaration

This project's `CLAUDE.md` has **no `## Compliance` section**, so no framework is binding. Per
`devflow:compliance` ("Project Framework Declaration" → *Absent → generic controls only*), this
review applies only the baseline rules from `~/.claude/rules/devflow/compliance.md`, calibrated for
what skim is: a **local developer CLI that reads code on the developer's own machine**. There is no
PII/PHI/payment surface, no network egress, no multi-tenant data, and no IaC in this diff.

Suggesting a `## Compliance` declaration would be inappropriate here — no regulated data is present,
which is the precondition the skill sets for that LOW suggestion.

The three surfaces named in the review brief were each investigated against current code. Two of the
three came back clean; findings are recorded only where code or docs actually deviate.

---

## Issues in Your Changes (BLOCKING)

### CRITICAL
None.

### HIGH
None.

### MEDIUM

**New init test bypasses the sandbox helper, re-introducing the PF-017 enumeration anti-pattern** — `crates/rskim/tests/cli_init.rs:1732`
**Confidence**: 90%

- **Problem**: `test_init_rewrites_hook_when_pin_path_differs` (added in this diff) invokes
  `skim init --yes` twice through the legacy `skim_init_cmd(config)` helper
  (`cli_init.rs:17-22`), which sets **only** `CLAUDE_CONFIG_DIR` (plus `SKIM_DISABLE_ANALYTICS` /
  `NO_COLOR` inherited from `common::skim()`). It does **not** set `HOME`, `SKIM_CACHE_DIR`,
  `SKIM_WRAPPERS_DIR`, `GEMINI_CONFIG_DIR`, `COPILOT_CONFIG_DIR`, `CODEX_HOME`, or
  `CRUSH_CONFIG_DIR`.

  This is precisely the shape `PF-017` names as the root cause — *"sandbox `$HOME` itself rather
  than enumerating app-specific overrides (an enumeration is always incomplete)"* — added in the
  same PR that promotes `skim_sandboxed_with_bin` to "the single authoritative sandbox env-var
  block ... rather than hand-rolling their own env block (PF-017)"
  (`crates/rskim/tests/common/mod.rs:39-49`). The inconsistency is visible inside one file: the
  sibling test added ~46 lines later, `test_init_wrappers_bypasses_fast_path`
  (`cli_init.rs:1783`), correctly routes through `common::skim_sandboxed(home_path)`.
  **avoids PF-017** is the stated intent of this PR; this one call site does not.

- **Present-day escape: none — stated plainly so this is not over-read.** I traced every
  home-reaching path reachable from `skim init --yes` under this test's env:
  - `detect_installed_agents` (`init/flags.rs:220-242`) enters **override mode** because
    `CLAUDE_CONFIG_DIR` is set, so only claude-code is considered and only via the TempDir. Real
    `~/.gemini`, `~/.codex`, `~/.copilot`, `~/.crush` are never probed or written.
  - Guidance resolves through `InstructionEnv.claude_config_dir` (`cmd/session/types.rs:197-203`),
    landing in the TempDir, not `~/.claude/CLAUDE.md`.
  - `maybe_install_wrappers` (`init/install.rs:725-741`) receives `wrappers: None` and stdin is not
    a TTY under `assert_cmd`, so it early-returns before `resolve_skim_binary()` — real
    `~/.skim/bin` is untouched.

  So the leak is **latent, not live**.

- **Failure scenario (why it still matters)**: the containment above rests entirely on
  `any_override == true` and on `wrappers: None`. Add `--wrappers` to this test (the natural next
  step given this PR's own `wrappers_blocks_fast_path` work), or run it on a machine where a future
  change makes `DetectionEnv::from_process()` consult `dirs::home_dir()` outside override mode, and
  the test writes symlinks into the developer's real `~/.skim/bin` and hook/guidance files into real
  agent config dirs — with no consent gate, since `maybe_install_wrappers`'s TTY prompt is skipped
  under `Some(true)`. That is unauthorised modification of developer state (least-privilege /
  integrity), and it is exactly the arming condition PF-017 documents ("the hazard is LATENT only
  because `~/.skim/bin` does not exist yet ... the developer's first `skim init --wrappers` ARMS
  it").

- **Fix**: route the new test through the authoritative helper, matching its sibling.

  ```rust
  #[test]
  fn test_init_rewrites_hook_when_pin_path_differs() {
      let home = TempDir::new().unwrap();
      let home_path = home.path();
      // detect_installed_agents() in override mode requires the dir to exist.
      fs::create_dir_all(home_path.join(".claude")).unwrap();

      // Step 1: Fresh install — hook script records the real binary path.
      common::skim_sandboxed(home_path)
          .arg("init")
          .args(["--yes"])
          .assert()
          .success();

      let hook_path = home_path.join(".claude/hooks/skim-rewrite.sh");
      // ... steps 2-4 unchanged, against hook_path ...
  }
  ```

  Optionally, follow up separately on the 26 remaining `skim_init_cmd(` call sites in this file —
  the "Known remaining gap" already recorded at
  `.devflow/features/hook-binary-pinning/KNOWLEDGE.md:190`. That cleanup is pre-existing and should
  not block this PR.

### LOW

**New analytics KNOWLEDGE.md omits the credential-scrubbing invariant and the non-token columns it protects** — `.devflow/features/analytics/KNOWLEDGE.md:122`
**Confidence**: 88%

- **Problem**: this file is added in this diff, and its own frontmatter states it is the document to
  read *"when adding new recording paths"* (`KNOWLEDGE.md:4`). Its "Business Context" says analytics
  *"answer 'how many tokens did skim save this week'"* and *"No data leaves the machine"*
  (`:30`), and its Anti-Patterns section (`:122-128`) lists five invariants — none of which is the
  one that actually protects secrets.

  Verified against current code, the `token_savings` table
  (`crates/rskim/src/analytics/schema.rs:11-24`) persists more than token counts:
  `original_cmd` (the full command text, truncated to 500 bytes) and `project_path` (an absolute
  path). The production code **does** defend this correctly — `build_analytics_label`
  (`crates/rskim/src/cmd/git/mod.rs:202-222`) routes every arg through
  `shared::scrub_credential_url` before it reaches `original_cmd`, with an inline comment naming the
  exact hazard (*"A user invoking `skim git push https://TOKEN@host/repo` would otherwise have the
  token written to `~/.cache/skim/analytics.db`"*), and `scrub_db_args` / `scrub_infra_args`
  (`crates/rskim/src/cmd/security.rs:136`, `:281`) cover the db and infra wrappers.

  A grep of the new KB for `scrub|credential|secret|redact|project_path` returns **zero matches**.
  The invariant that keeps credentials out of a persisted, unencrypted, 90-day-retained local store
  is undocumented in the one file a future agent is told to consult before adding a recording path.

- **Failure scenario**: an agent adds a wrapper handler for a new credential-bearing tool (e.g. an
  `ssh`, `curl`, or registry-login wrapper), reads this KB, follows all five documented
  anti-patterns correctly, and calls `try_record_command` with a raw `format!("{tool} {args}")`
  label. The credential is written verbatim into `~/.cache/skim/analytics.db`, persists for 90 days
  under `maybe_prune`, and is echoed back by `skim stats` in the `by_original_cmd` breakdown
  (`cmd/stats.rs:480-497`) — a surface an agent may well paste into a transcript.

- **Fix**: add one Anti-Pattern bullet and one classification line to the KB.

  ```markdown
  ## Anti-Patterns
  ...
  - **Writing an unscrubbed command string into `original_cmd`** — `original_cmd` is PERSISTED
    (90-day retention) and re-displayed by `skim stats`. Every arg-bearing label must go through
    `git::shared::scrub_credential_url`, `security::scrub_db_args`, or `security::scrub_infra_args`
    before it reaches `try_record_command` / `record_fire_and_forget`. See
    `cmd/git/mod.rs::build_analytics_label` for the reference pattern.
  ```

  And under Core Business Rules, state what is stored beyond counts: *"`token_savings` persists
  `original_cmd` (command text, 500-byte cap) and `project_path` (absolute path) alongside token
  counts — treat both as scrub-required surfaces."*

---

## Issues in Code You Touched (Should Fix)

None beyond the above.

---

## Pre-existing Issues (Not Blocking)

None reported. The analytics recording code is **unchanged** in this diff, and its data lifecycle
holds up against the baseline rules on inspection:

| Baseline rule | Status | Evidence |
|---|---|---|
| Minimum collection / declared purpose | Met | Local token-savings dashboard only; `original_cmd` capped at 500 bytes |
| Retention period | Met | 90-day auto-prune in `AnalyticsDb::maybe_prune()`, gated to once/24h |
| Deletion path | Met | `skim stats --clear` (documented at `KNOWLEDGE.md:81`, `:132`) |
| Real opt-out | Met | `SKIM_DISABLE_ANALYTICS=1\|true\|yes`, read once at the boundary via `AnalyticsConfig::from_process` |
| Secrets not written to analytics | Met in code | `scrub_credential_url` / `scrub_db_args` / `scrub_infra_args` applied before `original_cmd` (documentation gap noted as LOW above) |
| No egress | Met | Local SQLite only; `net-anthropic` is a separate gated feature |

Absolute paths containing the developer's own username, stored on that developer's own disk in their
own cache directory, are not a PII exposure under these rules and are not reported as one.

---

## Verified Clean — Surfaces Named in the Brief

These were investigated and produced **no finding**. Recording the negatives so they are not
re-raised next cycle.

### 1. The new "binary pin mismatch" doctor line discloses nothing new

`crates/rskim/src/cmd/doctor/mod.rs:470-486` adds
`"binary pin mismatch (hook: {pin}, running: {running})"`. Both operands were **already** printed by
the same command before this diff:

- the running binary path — `doctor/mod.rs:36-44` (`"Running binary"` section, via
  `current_exe_canonical()`), and advertised in `--help` at `:792`;
- the hook pin — the hook section already displayed `hook_binary_pin`, which is the exact
  display-without-gate defect `PF-018` describes (*"doctor then prints the WRONG pin path on a green
  exit-0 line"*).

Net environment disclosure delta: **zero**. The change substitutes a specific cause string for the
former dead `"stale"` terminal, on a user-initiated, on-demand command. No finding.

### 2. Nothing new enters agent context or a persisted log

- **applies ADR-013** — the diff adds no `systemMessage` and no `additionalContext`; drift-vs-no-drift
  hook output stays byte-identical. `hook_status_line` is doctor-stdout-only and is never reached
  from `run_hook_mode`.
- **applies ADR-011** — no new stderr notice is introduced anywhere in this diff, so the
  marker-vs-banner taxonomy is untouched. The doctor line is stdout status output, not a compression
  notice, and correctly falls outside the taxonomy.
- **applies ADR-004** — `resolve_skim_binary()` (`init/helpers.rs:26-36`) unifies the three
  path-producing sites behind one canonicalizing resolver, which is ADR-004's canonicalization
  requirement. No path is written anywhere it was not written before.
- `cmd/rewrite/hook.rs:355-366` and `:598-608` are **comment-only** changes plus an
  `Err(_) => false` rewritten as a braced block with the same value — no behavioural change, no new
  log content.
- `install.rs:778-791` **narrows** output: the PATH-setup and `SKIM_SESSION_ID` blurb is now gated on
  `result.created + result.updated > 0`, so idempotent re-runs disclose *less* than before.

### 3. `skim_sandboxed_with_bin` closes PF-017 for every caller that uses it

`crates/rskim/tests/common/mod.rs:66-85` sets `HOME` — the single seam behind `dirs::home_dir()`
that PF-017 identifies as the root cause — **plus** all five agent config-dir overrides
(`CLAUDE_CONFIG_DIR`, `GEMINI_CONFIG_DIR`, `COPILOT_CONFIG_DIR`, `CODEX_HOME`, `CRUSH_CONFIG_DIR`),
`SKIM_CACHE_DIR`, `SKIM_WRAPPERS_DIR`, and `SKIM_DISABLE_ANALYTICS=1`. Cursor has no config-dir env
var and resolves from `home_dir()`, so setting `HOME` contains it. `InstructionEnv`
(`cmd/session/types.rs:197-225`) now carries per-agent override fields for gemini/codex/copilot/crush,
closing the defense-in-depth half of PF-017's stated resolution. `#[cfg(unix)]` on
`test_doctor_exits_1_on_binary_pin_mismatch` and its use of `skim_sandboxed_with_bin` for the copied
binary are both correct. **The helper itself is sound**; the only gap is the call site reported as
MEDIUM above.

---

## Suggestions (Lower Confidence)

- **`skim_sandboxed_with_bin` removes two of three `SKIM_HOOK_*` vars** — `crates/rskim/tests/common/mod.rs:82-84` (Confidence: 70%) — `SKIM_HOOK_VERSION` and `SKIM_HOOK_BINARY` are `env_remove`d but `SKIM_HOOK_COMMIT` is not, even though `DriftEnv::from_process()` reads all three. Cannot cause a write outside the sandbox — hygiene/determinism only, and arguably a testing-lens item rather than compliance.
- **`SKIM_ANALYTICS_DB` is not neutralised in the sandbox** — `crates/rskim/tests/common/mod.rs:79` (Confidence: 65%) — it takes precedence over `SKIM_CACHE_DIR` for the DB path, so a developer with it exported would point the child at a real DB. Currently harmless because `SKIM_DISABLE_ANALYTICS=1` blocks all writes; an `.env_remove("SKIM_ANALYTICS_DB")` would make containment independent of that second control (defense in depth).

---

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 1 |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Compliance Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

Condition: route `test_init_rewrites_hook_when_pin_path_differs` (`cli_init.rs:1732`) through
`common::skim_sandboxed`, so the PR that makes `skim_sandboxed_with_bin` authoritative does not ship
a new test that bypasses it. The analytics KB anti-pattern bullet is a cheap, high-leverage LOW worth
folding into the same commit.
