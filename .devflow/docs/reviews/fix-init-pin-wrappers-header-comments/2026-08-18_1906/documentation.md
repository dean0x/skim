# Documentation Review Report

**Branch**: fix/init-pin-wrappers-header-comments -> main
**PR**: #488
**Date**: 2026-08-18 19:06
**Diff command**: `git diff main...HEAD` (21 files, +1211/-158)

## Scope note

This diff is documentation-heavy: four of the changed files are documentation
artifacts (`.devflow/features/analytics/KNOWLEDGE.md` NEW,
`.devflow/features/hook-binary-pinning/KNOWLEDGE.md` MODIFIED,
`.devflow/features/index.md` MODIFIED, `docs/modes.md` MODIFIED), and fix #479
was *entirely* a comment-correctness fix. Doc artifacts are therefore assessed
as first-class deliverables, and every factual claim below was checked against
the code it describes.

**Headline**: the #479 code comments are **correct** — I verified each of them
against the code (see "Verified correct" section). The defects are concentrated
in the two KNOWLEDGE.md artifacts and in doc surfaces the branch changed but did
not update.

---

## Issues in Your Changes (BLOCKING)

### HIGH

**New `analytics` feature KB is never registered in `index.md` — invisible to every agent** — `.devflow/features/index.md`
**Confidence**: 98%
- Problem: `.devflow/features/analytics/KNOWLEDGE.md` was added (commit `f00e37a`), but the only change to `.devflow/features/index.md` in this diff is the rewrite of the **hook-binary-pinning** line. The index still contains exactly 9 entries — `ast-index`, `build-parsers`, `cmd-search`, `cochange`, `file-wrapper-fidelity`, `hook-binary-pinning`, `research-ast`, `search-temporal`, `temporal-scoring`. `grep -c analytics .devflow/features/index.md` returns 0.
- Impact: `index.md` is the relevance-matching cache orchestrators consume to decide which KB to pass as `FEATURE_KNOWLEDGE`. A KB with no index row is real to a human browsing the directory and **nonexistent to every agent**. This is structurally the same failure shape as PF-014 (render artifact vs. source-of-record divergence) transplanted onto the feature-KB surface — the entry looks successfully authored right up until nobody can find it. 154 lines of researched context deliver zero value.
- Fix: add an alphabetically-first row derived from the KB frontmatter, in the exact documented format `- **{slug}** — {areas} — {Use-when description}`:
```markdown
- **analytics** — crates/rskim/src/analytics, crates/rskim/src — Use when adding new recording paths, changing cache-directory resolution, debugging silent analytics drops, writing tests that invoke skim, or tracing where session_id flows. Keywords: analytics, token savings, record_file_ops, flush_pending, register_thread, cache_root, SKIM_CACHE_DIR, SKIM_ANALYTICS_DB, SKIM_DISABLE_ANALYTICS, session_id, fire-and-forget.
```
  The `{areas}` field must be copied from the frontmatter `directories:` and the description from `description:` — `index.md` is only a cache; the frontmatter is authoritative.

---

**Analytics KB cites PF-002 four times; PF-002 does not exist** — `.devflow/features/analytics/KNOWLEDGE.md:24,126,144,154`
**Confidence**: 95%
- Problem: the ledger contains `PF-001`, then `PF-003` through `PF-019`. **There is no PF-002.** `grep -rn 'PF-002' .devflow/` returns nothing at all — not in `pitfalls.md`, not in the ledger, nowhere. The KB builds an entire narrative on it:
  - `:24` — "a prior drift (PF-002) caused `SKIM_CACHE_DIR` to relocate only the parser cache but not analytics.db"
  - `:126` — Anti-Pattern: "Divergence re-introduces PF-002"
  - `:144` — "proves the PF-002 fix is consistent across subsystems"
  - `:154` — "PF-002 (resolved): `SKIM_CACHE_DIR` only relocated parser cache…"
- Impact: violates the verbatim-IDs-only citation rule that `devflow:apply-decisions` makes an Iron Law. A future agent reading this KB will try to `Read` PF-002 for the full rationale, find nothing, and be unable to tell whether the pitfall was retired, mis-numbered, or invented. Fabricated citations are worse than none because they consume a lookup and destroy trust in the KB's other 8 citations (ADR-001, PF-001 — both of which **do** exist and check out).
- Fix: either (a) file the real pitfall through `assign-anchor` and cite the ID it returns, or (b) drop the ID and describe the drift narratively — "an earlier drift caused `SKIM_CACHE_DIR` to relocate only the parser cache" — which loses nothing since the mechanism is already fully explained in Rule 6.

---

**Analytics KB Rule 5 describes a persist loop that does not exist in the code** — `.devflow/features/analytics/KNOWLEDGE.md:69` (and `:111`)
**Confidence**: 95%
- Problem: `:69` states, with a literal code snippet:
  > "then `for rec in records { persist_record(&rec); }` writes serially"

  The actual `record_file_ops` background thread (`crates/rskim/src/analytics/mod.rs:1191-1199`) does the opposite — it opens **one** connection for the whole batch and never calls `persist_record`:
```rust
// Open DB once for all rows; skip silently on open failure (mirrors persist_record).
if let Ok(db) = AnalyticsDb::open_default() {
    for rec in &records { let _ = db.record(rec); }
    if !records.is_empty() { db.maybe_prune(); }
}
```
  `persist_record` (`mod.rs:1003-1008`) is a *different*, per-row helper that opens a fresh connection and prunes on every call — it belongs to the subcommand path, not the file-op path. The consequential claim at `:111` ("`AnalyticsDb::maybe_prune()` runs after each `persist_record` call") is therefore also wrong for the file-op path, where prune runs **once per batch**, guarded by `if !records.is_empty()`.
- Impact: this is the KB's own Rule 3 violation — "Two recording paths — never conflate them" — committed by the KB itself. The snippet describes N connections + N prune checks for an N-file glob; the code deliberately does 1 + 1. An agent trusting the KB would either replicate the per-row-connection pattern in a new recorder (undoing a real optimization) or "discover" the batching as a bug and revert it.
- Fix: rewrite `:69` to match the code and name the distinction explicitly:
```markdown
Inside `record_file_ops`'s background thread: `rows.into_par_iter().filter_map(...)`
resolves all counts in parallel (rayon), then a **single** `AnalyticsDb::open_default()`
connection writes every record serially via `db.record(rec)`, followed by one
`db.maybe_prune()` for the whole batch. Note this does NOT go through
`persist_record` — that helper opens a fresh connection per row and is used only
by the subcommand path.
```
  And correct `:111` to scope the per-call prune claim to `persist_record`/the subcommand path.

---

**Analytics KB never mentions credential scrubbing, though the schema persists command text** — `.devflow/features/analytics/KNOWLEDGE.md:122-128` (Anti-Patterns), `:30`
**Confidence**: 85%
- Problem: the KB frames analytics as answering "how many tokens did skim save this week" (`:30`) and asserts "No data leaves the machine". Both true — but incomplete in a way that matters. `TokenSavingsRecord` also persists `original_cmd` (500-byte-capped raw command text, `mod.rs:424 MAX_CMD_LEN`) and `project_path`. The code **does** scrub correctly (`build_analytics_label` at `cmd/git/mod.rs:202-222` via `scrub_credential_url`, plus `scrub_db_args` / `scrub_infra_args`), but none of the KB's five Anti-Patterns and none of its Gotchas mention scrubbing, credentials, or redaction.
- Impact: the KB's stated purpose is "Use when adding new recording paths". An agent adding a wrapper for a tool that takes credentials in argv (`psql "postgres://user:pw@host"`, `curl -u`, `docker login -p`) would follow every documented rule faithfully and still write plaintext credentials into `analytics.db`. The scrubbing layer is an unwritten invariant guarding a real secrets path — exactly the class of knowledge a domain KB exists to capture. Note this is a *documentation* gap only; the current code is correct.
- Fix: add an Anti-Pattern entry:
```markdown
- **Adding a recording path without routing `original_cmd` through a scrubber** —
  `TokenSavingsRecord.original_cmd` persists up to 500 bytes of raw command text to
  disk. Any new wrapper whose tool accepts credentials in argv MUST scrub before
  recording (see `build_analytics_label` in `cmd/git/mod.rs`, `scrub_credential_url`,
  `scrub_db_args`, `scrub_infra_args`). Analytics is local-only, but "local" is not
  the same as "safe to write secrets to".
```
  and soften `:30` from "No data leaves the machine" to note that command text and project paths are persisted, scrubbed.

---

**`Mode::Minimal` rustdoc contradicts the new behaviour on a published crate** — `crates/rskim-core/src/types.rs:567`, `:642`
**Confidence**: 92%
- Problem: both rustdoc sites still say Minimal mode strips "non-doc comments", with no header-comment exception. `docs/modes.md` was updated for the new `is_module_header_comment` preservation; this public API doc was not. Confirmed independently by a parallel reviewer.
- Impact: `rskim-core` is published to crates.io, so this rustdoc renders on docs.rs as the authoritative contract for the `Mode` enum. It is now provably false for Python/Ruby/SQL/Bash input. Per the Iron Law of this focus area, a doc that contradicts code actively misleads — and this one misleads library consumers who never read `docs/modes.md`.
- Fix: at both sites, align with the `docs/modes.md` wording: `strip non-doc comments (module header comments are preserved for Python, Ruby, SQL, and Bash)`.

### MEDIUM

**hook-binary-pinning KB says `hook_is_current()` does not check commit — it does** — `.devflow/features/hook-binary-pinning/KNOWLEDGE.md:56`, `:150`, `:252`
**Confidence**: 95%
- Problem: `:56` (an added line) asserts:
  > "**`DetectedState::hook_is_current()`** combines version match AND pinned format: `version matches && hook_uses_pinned_binary`."

  The actual predicate (`crates/rskim/src/cmd/init/state.rs:88-113`) is version + pinned format **+ commit**, and its own rustdoc says so: *"at the current version, uses the pinned binary format, AND pins the same git commit as this binary"* — with a B5c block comparing `hook_commit` against `option_env!("SKIM_GIT_COMMIT")`. The KB repeats the truncated definition at `:150` ("= version matches && pinned format") and `:252` ("`hook_is_current()` (version + pinned format)").
- Impact: this is not a harmless omission — it breaks the KB's own central argument. `:99` and `:230` justify deleting the `"stale"` terminal with *"`commit_ok ∧ version_ok ⇒ hook_is_current`"*. That implication is **only valid because `hook_is_current` checks commit**. Under `:56`'s stated definition the implication does not hold, so a future agent auditing the dead-code removal will derive a contradiction from the KB alone and may "restore" the terminal that this PR correctly proved dead. The KB also contradicts the new `pin_is_current` rustdoc added in this same diff (`state.rs:47`: "`hook_is_current` checks version + pinned format + commit").
- Fix: at `:56`, `:150`, and `:252`, state `version + pinned format + commit`, and keep the "does NOT check path" clarification — that part is correct and is the actual reason `pin_is_current()` exists.

---

**"all five conditions" introduces a 7-condition block; KB self-contradicts 90 lines later** — `.devflow/features/hook-binary-pinning/KNOWLEDGE.md:60` vs `:150`
**Confidence**: 98%
- Problem: `:60` reads "**Fast-path condition in `run_install_single`** now requires all five conditions:" and is immediately followed by a code block listing **seven**: `hook_installed`, `hook_is_current()`, `pin_is_current()`, `guidance_current`, `!permissions_blocked`, `!wrappers_blocked`, `manifest_present`. The State Transitions section at `:150` says "all 7 fast-path conditions true". Seven is correct — it matches `crates/rskim/src/cmd/init/install.rs:503-511` exactly.
- Impact: a reader who trusts the prose count over the block will believe two conditions are optional — and the two most likely to be dropped are precisely the ones this PR added (`pin_is_current`, `!wrappers_blocked`). Both are load-bearing: `pin_is_current` is the entire two-clone fix, and `!wrappers_blocked` is #478.
- Fix: change "five" to "seven" at `:60`. (Note `:304` separately says "all five agent config-dir overrides" — that one is correct: HOME plus 5 agent dirs.)

---

**`hook_status_line`'s docblock still describes the message this PR deleted** — `crates/rskim/src/cmd/doctor/mod.rs:371-372`
**Confidence**: 90%
- Problem: the function docblock still says the `Unreadable` branch "names the suppression coupling". This PR **removed** that claim from the branch body (`mod.rs:412-418`) precisely because it was false, and added a test at `mod.rs:1002-1008` that *forbids* the string `"silences drift detection"` from appearing. Confirmed independently by a parallel reviewer.
- Impact: this is the most pointed defect in the diff. Fix #479 was *entirely* a comment-correctness fix — its whole thesis is that a wrong comment about drift suppression is the bug. The parallel comment in `hook.rs` was fixed correctly; the docblock 40 lines above the code it describes was missed, so the exact false statement #479 set out to eliminate survives in this file, now contradicted by both the code below it and a test that asserts against it.
- Fix: update `:371-372` to match the corrected body — `Unreadable` → drift, early-return, and explicitly **does not** suppress drift detection (only `Tampered` does, via `check_hook_integrity` returning `true`).

---

**`docs/modes.md` "What's Removed" cell overstates header-comment coverage** — `docs/modes.md:10`, `:280`
**Confidence**: 85%
- Problem: the Minimal row's removed-column now reads `Non-doc comments (except headers)`. Unqualified, "except headers" reads as a universal rule. The preservation applies to exactly `{Python, Ruby, SQL, Bash}` — verified as the set where `is_doc_comment` returns unconditional `false` (`minimal.rs:174-178, 203-207, 216-223`) and the exact set matched by `is_module_header_comment` (`minimal.rs:290-293`). Rust `// SPDX-License-Identifier:`, C/C++ `/* Copyright */`, TypeScript `/* @license */`, and Go banner comments are all still **stripped**.
- Impact: SPDX and license headers are the canonical case a reader checks this table for, and the languages where stripping them is most consequential (Rust, C, TS) are the ones silently excluded. The kept-column *is* correctly qualified ("Python/Ruby/SQL/Bash module header comments"), so the two halves of the same row disagree in scope — a reader scanning only the removed-column gets the wrong answer.
- Fix: `Non-doc comments (Python/Ruby/SQL/Bash module headers preserved)`. `:280` in the Pseudo section is already correctly qualified and needs no change. Consider adding one sentence noting that other languages' license/SPDX headers are still stripped unless they use that language's doc-comment syntax (`//!`, `/**`) — this is the question users will actually arrive with.

---

**Wrong source line reference in a new comment** — `crates/rskim-core/src/transform/mod.rs:737`
**Confidence**: 92%
- Problem: the new invariant test's closing comment says the divergence comes from the "trailing-newline restore at `minimal.rs:406-408`". After this branch, `minimal.rs:406-408` is the `"Range exceeds source length"` error branch inside `remove_ranges` — unrelated code. The actual trailing-newline restore is at `minimal.rs:465` (`if source.ends_with('\n')`).
- Impact: the comment documents a deliberate, accepted divergence between the line map and the output text — genuinely valuable reasoning. Pointing it at the wrong function makes a reader conclude the reasoning is stale and distrust the whole "Harmless" verdict. Hardcoded line numbers in comments drift on the very next edit; this one drifted within its own commit.
- Fix: reference the symbol, not the line: `(trailing-newline restore at the end of trim_and_normalize in minimal.rs)`.

---

**Transition residue: new helper's rustdoc cites code the same commit deleted** — `crates/rskim/src/cmd/init/helpers.rs:25`
**Confidence**: 88%
- Problem: `resolve_skim_binary()`'s doc closes with "preserving the behaviour of the pre-unification code at `install.rs:895-906`." That code no longer exists — this commit replaced it with a call to this very helper (`install.rs:928-940` in the diff).
- Impact: two defects in one sentence. It is a tombstone comment ("we no longer do X"), which the project's quality rule explicitly prohibits — *"Leave the end-state, not the transition… Git holds the history."* And it is a dangling line reference that sends a reader to unrelated code in a file that changed by +135/-25 in this same commit.
- Fix: keep the behavioural contract, drop the archaeology: `/// Canonicalize failure (e.g., binary deleted while running) falls back to the raw path from current_exe() rather than failing.` The rationale for *why* the three sites must agree is already stated above it and is the part worth keeping.

---

**CLAUDE.md describes doctor's pin state as SHA-only; it is now also path-based** — `CLAUDE.md:83`
**Confidence**: 88%
- Problem: the `doctor` bullet says it reports "hook pin state per agent (**pinned SHA vs running SHA**)". This branch adds a path comparison (`pin_is_current()`) and a new terminal that reports paths, not SHAs: `"binary pin mismatch (hook: {pin}, running: {running})"` (`doctor/mod.rs:477-486`). It also removes the `"stale"` status from doctor's vocabulary entirely.
- Impact: CLAUDE.md is the agent-facing operational contract for this repo. An agent debugging a `binary pin mismatch` line will consult CLAUDE.md, read that pin state is a SHA comparison, and go looking for a commit divergence that by construction does not exist — the pin-mismatch terminal is only reachable when version **and** commit both match. That is a direct wrong-turn, and it lands on the two-clone hazard this repo is most exposed to (CLAUDE.md itself warns "this machine keeps parallel clones").
- Fix: `hook pin state per agent (pinned commit SHA *and* pinned absolute binary path vs the running binary)`.
- Verified NOT stale, for the record: the exit-code contract at the same line ("Exit `0` healthy / `1` on any drift") remains correct — `hook_status_line` returns `drift=true` for pin mismatch, which propagates through `print_hook_section`'s `any_drift` (`doctor/mod.rs:526-537`) to exit 1.

---

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`check_hook_integrity`'s return-value docblock omits the Unreadable case** — `crates/rskim/src/cmd/rewrite/hook.rs:559-562`
**Confidence**: 85%
- Problem: the docblock says the function returns "`false` if valid, missing, or **check was skipped**". The `Err(_)` arm — the unreadable-script case — is none of those three, and this PR added six lines of body comment (`hook.rs:601-606`) explaining exactly why it returns `false`.
- Impact: the docblock is the summary a caller reads; the fix #479 reasoning now lives only in the body. Since #479 exists *because* someone reasoned about this function from a comment rather than the code, leaving the summary incomplete recreates the exposure at the doc level directly above the fix.
- Fix: `/// Returns `true` only when the script is Tampered. Returns `false` when valid, when no manifest exists, when no script is installed, or when the script is unreadable — an unreadable script must not suppress drift detection.`

---

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`docs/modes.md` has no `## Minimal Mode` section although it claims six modes** — `docs/modes.md:3`, headings at `:16, :83, :150, :223, :254`
**Confidence**: 95%
- The intro says "Skim offers six transformation modes", and dedicated sections exist for Structure, Signatures, Types, Full, and Pseudo — but not Minimal. Minimal's entire normative documentation is the one comparison-table row this PR edited, plus a cross-reference from the Pseudo section (`:280`).
- Not introduced by this branch, and correctly out of scope. Worth noting because it explains why the header-comment change had only ~4 lines to land in: there is no Minimal section in which to state the language limit properly. A follow-up adding `## Minimal Mode` with What's Preserved / What's Removed / Per-Language would give the new behaviour a real home.

---

## Verified correct (no action — recording so the resolve pass does not re-litigate)

The #479 comment-correctness claims are the core of this PR, so each was checked
against the code rather than accepted:

| Claim | Location | Verdict |
|---|---|---|
| Drift reads 3 of `DriftEnv`'s 6 fields from hook-exported env vars; other 3 are binary-derived | `hook.rs:357-362` | **TRUE** — `DriftEnv` has exactly 6 fields (`hook.rs:51-67`); `hook_version`/`hook_binary`/`hook_commit` come from `std::env::var`, `current_exe`/`compiled_version`/`compiled_commit` do not (`hook.rs:76-91`) |
| `Unreadable → false`, so drift detection still runs; only `Tampered` suppresses it | `hook.rs:601-606` | **TRUE** — `Ok(true)=>false`, `Ok(false)=>true` (tampered), `Err(_)=>false` (`hook.rs:578-608`) |
| Doctor's Unreadable message must not claim drift is silenced | `doctor/mod.rs:412-418` + test `:1002-1008` | **TRUE** — claim removed from the body, test forbids its return (docblock at `:371` still stale — see finding above) |
| `is_doc_comment` returns unconditional `false` for Python/Ruby/SQL/Bash, so headers need the new guard | `minimal.rs:281-284` | **TRUE** — verified all four arms (`minimal.rs:174-178, 203-207, 216-223`) |
| Blank-line-break semantics: 1 newline = adjacent, ≥2 = break | `minimal.rs:287-291` | **TRUE** — matches `gap.bytes().filter(|&b| b == b'\n').count() > 1` |
| `wrappers_blocks_fast_path`: `None` must return `false` or non-TTY init reinstalls every run | `install.rs:157-168` | **TRUE** — matches implementation and the named idempotence test |
| Analytics KB Rule 1 (`register_thread` / `into_inner()` poison recovery) | KB `:36`, `:118` | **TRUE** — `mod.rs:947-958`, `985-999` |
| Analytics KB Rule 6/7 (cache-dir resolution, `SKIM_ANALYTICS_DB` precedence, `--clear-cache` does not clear analytics.db) | KB `:71-85`, `:132` | **TRUE** — matches `cache.rs:39,72`, `mod.rs:414`, and CLAUDE.md's Environment Variables section |
| Analytics KB constants: 500-byte `original_cmd` cap, 90-day prune, 24h gate, 5000 ms busy_timeout, schema v1–v3 | KB `:107-113` | **TRUE** — `mod.rs:424`, `:664-679`, `:402` |
| CLAUDE.md init fast path | CLAUDE.md `:86` | **NOT STALE** — CLAUDE.md documents `init`/`--wrappers`/`--permissions` but never describes the fast path, so `wrappers_blocks_fast_path` introduces no CLAUDE.md drift |
| `.devflow/features/index.md` is not a PF-014 render artifact | PF-014 body | **CONFIRMED** — PF-014 governs `.devflow/learning/{decisions,pitfalls,index}.md`, rendered from `decisions-ledger.jsonl`. Hand-editing `.devflow/features/index.md` is legitimate (`avoids PF-014`) |
| hook-binary-pinning KB citations ADR-004/005/006/007/008, PF-003/004/015/016/017 | KB `:265-276` | **ALL EXIST** in the ledger |
| Analytics KB freshness | frontmatter `updated: 2026-06-25` | **LOW RISK** — only one commit (`6f8edd8`, +11/-2) touched `crates/rskim/src/analytics` since that date |

---

## Suggestions (Lower Confidence)

- **Analytics KB frontmatter `directories` includes the whole crate root** - `.devflow/features/analytics/KNOWLEDGE.md:6` (Confidence: 75%) — `["crates/rskim/src/analytics", "crates/rskim/src"]`; the second entry matches essentially every `rskim` change, so this KB will be selected as `FEATURE_KNOWLEDGE` for unrelated work and dilute the context budget. Narrowing to `analytics` + the specific integration files (`cache.rs`, `main.rs`, `multi.rs`, `process.rs`) would match the referencedFiles list already present.
- **Analytics KB `updated` is 2 months behind its commit date** - `.devflow/features/analytics/KNOWLEDGE.md:17` (Confidence: 78%) — `updated: 2026-06-25` on a file committed 2026-08-18, while the hook-binary-pinning KB in the same diff correctly advanced to `updated: 2026-08-18`. Inconsistent convention within one PR; readers use this field to gauge trust.
- **`is_module_header_comment` rustdoc lists shebangs as a motivating case** - `crates/rskim-core/src/transform/minimal.rs:281-282` (Confidence: 70%) — shebangs are already preserved by the separate `is_shebang` check at `minimal.rs:152`, and SQL has no shebang concept. Minor over-claim in an otherwise precise docblock.

---

## Open Questions

- I did not independently re-derive the `types.rs:567/:642` rustdoc text (cross-reviewer confirmed); folded in at 92% on their verification plus my own confirmation that the behaviour change makes any unqualified "strip non-doc comments" claim false.

---

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 5 | 6 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 1 | 0 |

**Documentation Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

**Rationale**: The code-comment work at the heart of #479 is genuinely good — I
verified every substantive claim in the new `hook.rs`, `minimal.rs`,
`install.rs`, `state.rs`, and `transform/mod.rs` comments against the code, and
they are accurate, well-reasoned, and explain *why* rather than *what*. The
`transform/mod.rs` and `pseudo.rs` line-map docs in particular are exemplary.

The blockers are in the doc artifacts this PR ships as deliverables. Three are
disqualifying on their own: the new analytics KB is **unreachable** (no
`index.md` row), **cites a pitfall that does not exist** (PF-002), and
**misdescribes the persist path it exists to document** — conflating the two
recording paths its own Rule 3 forbids conflating. Two more doc surfaces the
branch invalidated were left stale, including public rustdoc on a published
crate (`types.rs`) and — most pointedly — the `hook_status_line` docblock, which
still carries the exact false drift-suppression claim that fix #479 was written
to eliminate, now contradicted by a test in the same file.

None of this is a code-correctness risk and nothing here warrants BLOCK. All 12
findings are mechanical text edits in 8 files; the largest is a single new line
in `index.md`. Fixing the three analytics-KB HIGHs and the two stale docblocks
would move this to APPROVED.
