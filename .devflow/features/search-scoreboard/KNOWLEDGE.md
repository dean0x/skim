---
feature: search-scoreboard
name: Search scoreboard (end-to-end retrieval quality gate)
description: "Use when a search PR's Search Scoreboard CI job fails, when changing skim search retrieval/ranking/pagination/walker universe/text output, when running the scoreboard locally, when ledgering or promoting a known HARD failure, when blessing baseline.json (incl. --accept-regression), when adding golden queries or bumping a corpus pin, or when editing the scoreboard harness (oracle, universe, runner, gate, bless), the CI search-path filter, or rskim-research pinned-clone / subprocess-timeout code. Keywords: scoreboard, Search Scoreboard, run, check, bless, golden-gen, baseline.json, known_failures.toml, ledger, XFAIL, XPASS, promote, HARD, RATCHET, INFO, bless required, --accept-regression, accepted_regressions, golden, corpora.toml, oracle, universe.delta, skipped_by_reason_mismatch, coverage.tracked_text, oracle_less.full_rows, results.unique_paths, silent_fn, degraded, temporal_state, harness error, exit 2, SEARCH_PATHS, --no-renames, skim-release, scoreboard-report, ensure_pinned_history_clone, verify_pinned_clone, OWNERSHIP_MARKER, zeroPaddedFilemode, process_group, git_output_with_timeout, KILL_GRACE, caffeinate, .bench-corpus/scoreboard, #544, #545, #547, #541, #542, ADR-007."
category: domain-knowledge
directories: [crates/rskim-bench/src/scoreboard/, crates/rskim-bench/src/bin/scoreboard.rs, crates/rskim-bench/scoreboard/, crates/rskim-bench/tests/scoreboard.rs, crates/rskim-research/src/clone.rs, .github/workflows/ci.yml]
referencedFiles:
  - crates/rskim-bench/src/scoreboard/mod.rs
  - crates/rskim-bench/src/scoreboard/pipeline.rs
  - crates/rskim-bench/src/scoreboard/runner.rs
  - crates/rskim-bench/src/scoreboard/metrics.rs
  - crates/rskim-bench/src/scoreboard/gate.rs
  - crates/rskim-bench/src/scoreboard/baseline.rs
  - crates/rskim-bench/src/scoreboard/golden.rs
  - crates/rskim-bench/src/scoreboard/oracle.rs
  - crates/rskim-bench/src/scoreboard/universe.rs
  - crates/rskim-bench/src/scoreboard/report.rs
  - crates/rskim-bench/src/scoreboard/types.rs
  - crates/rskim-bench/src/scoreboard/corpus.rs
  - crates/rskim-bench/src/bin/scoreboard.rs
  - crates/rskim-bench/scoreboard/README.md
  - crates/rskim-bench/scoreboard/known_failures.toml
  - crates/rskim-bench/scoreboard/corpora.toml
  - crates/rskim-bench/tests/scoreboard.rs
  - crates/rskim-research/src/clone.rs
  - .github/workflows/ci.yml
created: 2026-09-25
updated: 2026-09-25
---

# Search scoreboard (end-to-end retrieval quality gate)

## Overview

The scoreboard (#203, PR #560) is the **required merge gate for search PRs**. It is the `Search Scoreboard` job in
`.github/workflows/ci.yml`. It runs the **release `skim` binary as a subprocess**, the same way an agent does,
against four pinned full-history corpora (skim `b8a0a79`, ripgrep, flask, zod). It checks every answer against
oracles that share no code with skim's search stack, and it compares ranking with naive baselines. It replaced the
manual ADR-007 dog-food campaign as the standing gate. Manual dog-food is still required only where the scoreboard
cannot see: AST structural precision/recall (#541), the temporal arms against `git log` (#542), and any new
query flag or arm that has no golden entries yet.

The user-facing runbook is `crates/rskim-bench/scoreboard/README.md`. This file covers what the README does not
spell out: how the pieces couple, which invariants are enforced where, and the traps. Most of them were found the
hard way during #203.

## Business Context

- **Why it exists.** Dog-food rounds produce only negative evidence and never a "done" signal. The old reader-API
  `rskim-bench` harness scored a different file universe and bypassed the verify gate, anchors and pagination. The
  scoreboard measures what the shipped CLI actually returns (ADR-007 amendment, 2026-09-25).
- **What "passing" means.** `scoreboard check` passes when every HARD outcome is PASS or ledgered XFAIL, and
  nothing differs from the blessed `baseline.json`. That covers RATCHET values, HARD states, corpus pins, golden
  hashes and the corpus set. It does **not** mean skim beats the baselines. The "beats baseline" column is INFO.
  The committed baseline has skim *losing* some comparisons, for example aggregate `bytes.text_median` 2177 vs
  simulated rg 1573, and `concept.p5` 0.8538 vs occurrence-count 0.9.
- **Ratchets, not targets.** RATCHET values are compared with blessed *measured* values. The "bar" column is not
  compared (applies ADR-003: grounded regression guards instead of aspirational targets). Golden data is declared
  before anyone looks at skim's output, and it is never regenerated in CI.
- **A green scoreboard is not merge authorization.** Report the verdict and stop. The user asks for the merge
  (ADR-005).

## Core Business Rules

### Three check classes

| Class | Scope | Tolerance | Where defined |
|---|---|---|---|
| HARD | per golden entry × check (`CheckId::ALL`, 11 checks) | 0; only a ledger entry excuses a failure | `types.rs` `CheckId`, `metrics.rs` `PlannedQuery::runs` |
| RATCHET | per corpus + aggregate | exact after `round4`; `bytes.text_*` / `bytes.*first_correct_median` / `bytes.rg_*` are ±3% **relative to the baseline value** | `metrics.rs` `RATCHET_METRICS`, `compare_ratchet` |
| INFO | latency, `unindexed_hits`, per-reason skip breakdown, beats-baseline | never gated | `report.rs` |

Which HARD checks run on an entry is decided in one place, `PlannedQuery::runs`:

- recall / precision / `silent_fn` need an oracle. There is none for `--ast`, `--blast-radius`, or a standalone
  temporal run.
- `verify_mode` needs a text query.
- `pagination.*` runs only on `[[pagination]]` entries, and `order.prefix_consistent` only on `[[prefix]]` entries.
- `order.score_monotone` is skipped when a temporal sort or `--blast-radius` overrides the rank.
- `results.unique_paths` runs on every entry, within each single list. The same path on two different pages is
  `pagination.disjoint`.

Pagination and prefix checks compare skim with **its own full list** (`--limit 1_000_000`), not with the oracle.
That is why oracle-less entries can still be pagination entries.

`lexical.silent_fn` fails only when a ground-truth file is missing **and** `degraded[]` is empty. A miss that skim
discloses counts against recall, not against `silent_fn`.

### Ledger (`known_failures.toml`)

- Each `[[xfail]]` entry names `issue = "#<digits>"`, which must be a filed ticket (`is_issue_ref` rejects
  placeholders and leading zeros), plus one dotted `check` and the exact failing `ids` copied from `report.json`.
  A `(check, id)` pair may appear only once.
- Ids map to a corpus by the **longest** `<corpus>-` prefix (`gate::corpus_of`). An id that belongs to no corpus
  is a harness error when `Inputs::load` runs. So is an id missing from the golden set, or a check that never runs
  on that entry (for example `order.score_monotone` on a `--hot` entry): see `golden::check_ledger` and
  `gate::unplanned_ledger_refs`. A stale entry therefore cannot silently XFAIL nothing.
- The current ledger is seeded from the first real run and covers #544 (multi-word pagination pool cut before
  verification), #545 (text / `--ast` `--hot` re-sorts only a 100-row `resort_window`) and #547 (standalone
  `--ast` in path order, not score order).

### Bless rules (`baseline.rs::bless`, pure)

`bless` refuses (exit 1) in these cases:

- a partial `--only` report (`complete: false`);
- a report whose per-corpus golden SHA-256 differs from the golden file on disk;
- any FAIL or XPASS record;
- any RATCHET regression or HARD downgrade without a non-blank `--accept-regression "<reason>"`.

HARD downgrades are `pass -> xfail`, `<blessed> -> not run` (a removed entry or check), and a blessed corpus that
is no longer run. The reason, with every accepted line, is appended to `accepted_regressions[]`. Existing entries
are carried forward and never dropped. `--accept-regression` with nothing to accept is ignored, with a note.

| RATCHET movement (`compare_ratchet`) | `check` | `bless` |
|---|---|---|
| within tolerance | ok | n/a |
| Improved | FAIL "bless required" | no reason needed |
| Regressed (bad direction) | FAIL | needs `--accept-regression` |
| Changed (a Neutral reference column, or a ZeroBest value that flips sign at equal magnitude) | FAIL | no reason needed |
| new metric / no longer measured | FAIL | no reason needed (`regressions()` compares only metrics present on both sides) |

`universe.delta` and `universe.skipped_by_reason_mismatch` are **RATCHETs** (ZeroBest, exact, bar "= 0"), not
HARD checks. Moving away from 0 is a regression, so it can only be blessed with a reason. In practice, fix the
mismatch instead (ADR-008: skim indexes walked ∪ tracked, and the oracle must reproduce that universe
independently).

### Oracle independence

Nothing on the scoring path may import skim's search code (`rskim_search::query_substring_present`, an
`rskim-search` tokenizer, `rskim_core::Language`). Where the oracle must agree with skim, it keeps **its own
copy** with a citation, so that a policy change on skim's side shows up as a scoreboard diff instead of flowing
through silently:

- `oracle.rs` `LANGS`: the extension allow-list and `--lang` names, a copy of `Language::from_extension` and
  `parse_lang_value`;
- `oracle.rs`: its own `is_word_byte`;
- `universe.rs`: `MAX_FILE_BYTES` (5 MiB), the `MINIFY_*` gate, `SERDE_EXTENSIONS`, the hidden-component rule,
  `MAX_INDEXED_FILES` (50 000), and the producer-phase skip labels that must equal skim's
  `PersistedSkipReason::label()`.

This is **convention only**. `rskim-bench` depends on `rskim-search` and `rskim-core` (for the BM25F bench and
`golden_gen`), so a forbidden import would compile. `golden_gen.rs` is the one sanctioned user: it passes
`rskim_core::Language` / `SearchField` only as the symbol extractor's dispatch key, and it only *proposes* entries
for human review.

## State Transitions

HARD outcome classification (`gate::classify`) happens before the baseline comparison:

| raw result | ledgered? | outcome | gate | blessable |
|---|---|---|---|---|
| pass | no | PASS | ok | yes |
| fail | yes | XFAIL | ok | yes |
| fail | no | FAIL (Unledgered) | fails | no |
| pass | yes | XPASS | fails: "promote" | no |

The baseline stores only `pass` / `xfail` per `(id, check)`. Any change, including `new -> pass`, fails the gate
with "bless required" (`hard_state_changes`).

**Lifecycles you will actually hit:**

1. **You fixed a ledgered bug.** CI shows XPASS. Remove the ids (or the whole entry) from `known_failures.toml`
   and push. CI then shows `xfail -> pass; bless required`. Bless from that run's `scoreboard-report` artifact,
   commit `baseline.json`, and push again. That is two CI rounds. You can also do it locally in one push:
   `scoreboard run --out <dir>` and then `bless --from <dir>/report.json`.
2. **You found a real bug you will not fix in this PR.** File the ticket first. Add an `[[xfail]]` entry with that
   number and the exact ids. Re-bless with `--accept-regression "<reason>"`, because `pass -> xfail` is a
   downgrade.
3. **You added a HARD check or golden entries.** Every entry reports `new -> pass`, and the golden hash changes.
   Bless, with no reason needed. Example: adding `results.unique_paths` blessed 260 new records (71/64/63/62) in
   960b763.
4. **You removed a golden entry.** Its checks go `-> not run`, a HARD downgrade that needs `--accept-regression`.
   Its `oracle_less.full_rows.<id>` value (if any) goes "no longer measured".
5. **You bumped a corpus pin.** Update `corpora.toml` and the golden `commit` together, and re-verify every
   `def.line` (drift is exit 2). The CI corpus cache key is `hashFiles(corpora.toml)`, so CI does a fresh clone.
   Refresh the ledger ids from the new `report.json`, then bless.

## Technical Implementation Patterns

### Per-corpus pipeline (`pipeline::run_corpus`, in order)

1. `materialize_verified`: clone or reuse at the pin, then require `PinnedCloneState::Reusable`.
2. `Universe::compute` (the oracle's universe, with git isolated under the sandbox HOME), then `check_file_cap`.
3. `checked_plan`: golden integrity plus ledger refs, then `metrics::plan`. Any problem is exit 2.
4. `runner.build` (`skim search --build`, must exit 0), then `runner.stats`, then `require_temporal_data`.
5. `runner.observe` for each entry, then `require_oracle_less_rows`.
6. `verify_untouched`: the clone must still be `Reusable`. `git status --porcelain --untracked-files=all
   --ignored` catches anything skim wrote into the corpus root, even gitignored files.
7. `metrics::evaluate`, then `gate::apply_ledger`, then the report. The aggregate RATCHET values and
   `gate::evaluate` run after all corpora.

### Runner contract (`runner.rs`)

- **Argument shape.** `search --root <clone> --json --limit N [--offset K] <flags> [-- <query>]`. Every flag goes
  before `--` and the query always after it, so queries like `-D warnings` and `->` parse as text. `--offset` is
  emitted only when K > 0.
- **Sandbox.** There is one fresh temp `HOME` per run (`SkimSandbox`). It redirects `SKIM_CACHE_DIR`, every
  agent config dir and `SKIM_WRAPPERS_DIR`. It sets `SKIM_DISABLE_ANALYTICS=1` and `NO_COLOR=1`, and it strips
  `SKIM_PASSTHROUGH` / `SKIM_DEBUG` / hook and session variables. The oracle's `git ls-files` shares the same
  `GitIsolation` (the same `HOME`, `GIT_CONFIG_NOSYSTEM=1`), so global excludes cannot differ between the two
  sides.
- **Exit handling.** A non-zero exit whose stdout parses as the arm's envelope is accepted (so a future no-match
  exit code does not read as a crash). A signal, non-JSON stdout or a malformed envelope is a harness error.
- **Byte metrics** come from a separate **text-mode** run at the default limit (what an agent reads), counting
  **stdout + stderr**, with every `in <N>ms` normalized to `in 0ms`.

### Subprocess timeouts kill the whole process group (`clone.rs`)

Every subprocess the scoreboard starts goes through `rskim_research::clone::git_output_with_timeout`: skim calls
(120 s, `SKIM_TIMEOUT_SECS`), network git (300 s), local git (120 s) and `ls-files` (120 s). Before 6129a36, only
the direct child was killed. A grandchild that inherited the pipes kept `wait_with_output` blocked: a 120 s timeout
returned after 179 s. The fix, as it exists now:

```rust
// clone.rs — every timed child leads its own process group (unix)
fn spawn_in_own_group(cmd: &mut Command, label: &str) -> anyhow::Result<Child> {
    #[cfg(unix)]
    { use std::os::unix::process::CommandExt; cmd.process_group(0); } // pgid = child pid
    cmd.spawn().with_context(|| format!("spawning {label}"))
}

// run_with_timeout, on the deadline:
kill_process_tree(child_id);            // libc::kill(-pid, SIGKILL) then kill(pid, SIGKILL); taskkill /T off-unix
if rx.recv_timeout(KILL_GRACE).is_ok() { // KILL_GRACE = 2 s
    let _ = handle.join();              // join only a finished wait thread...
}                                       // ...otherwise detach it: never block past timeout + 2 s
```

- Reuse `git_output_with_timeout` / `git_run_with_timeout` for any new subprocess. Never call `Child::kill` on
  the direct child and then `join` unconditionally.
- The error text reports the elapsed time it actually took, not only the configured bound.

### Pinned full-history clones (`ensure_pinned_history_clone`)

- The clone is full history (`--no-checkout`, no `--depth` / `--filter`) with a detached checkout. The temporal
  layer needs real history, and `Shallow` is never reusable. If the pin is not reachable from the cloned refs,
  the code runs `fetch origin <sha>` once. It re-clones **once**, and a second failure is an error (exit 2).
- The deletion guard: a non-reusable destination is deleted only if it is empty or holds
  `.git/skim-scoreboard-pinned-clone` (`OWNERSHIP_MARKER`). If `--corpus-dir` points at your own checkout, you
  get an error, never a deletion.
- Security arguments: `credential.helper=` and `transfer.fsckObjects=true`, with exactly one downgrade,
  `fetch.fsck.zeroPaddedFilemode=ignore`. pallets/flask's history has `040000` tree modes (object `0b404df8…`),
  and strict fsck rejects them. `hasDotgit` and every other check stay fatal.
- Every pinned-clone git call scrubs `GIT_DIR` / `GIT_WORK_TREE` / … and sets `GIT_CEILING_DIRECTORIES` to the
  destination's parent, so an empty directory never borrows an enclosing repo's HEAD.

### Determinism

`report.json` is byte-identical across runs of the same binary except its last section, `latency`. The
workspace `serde_json` has `preserve_order`, so every map in the report or baseline **must be a `BTreeMap`** (or a
struct). Lists are sorted before they are stored, floats go through `round4`, and nothing records a timestamp,
binary version or machine path. The committed baseline was blessed from a local macOS release run (960b763), and
the #203 session observed it gating byte-identically on ubuntu CI. The README still says to bless from the CI
artifact, because ubuntu is the platform that gates.

### CI wiring: fail closed, never a green skip

`changes` (**Detect Search Changes**) classifies the event. `workflow_dispatch` and pushes to `main` always run the
gate. Pushes to `feature/*` and `wave/**` never run it (their PR run gates). An unknown event runs it (fail
closed). On a PR, `git diff --name-only --no-renames -z HEAD^1 HEAD` on the merge commit (`fetch-depth: 2`) is
matched against `SEARCH_PATHS`. `--no-renames` makes a file moved *out of* a search path still count (80ac691).

```yaml
scoreboard:
  name: Search Scoreboard            # branch protection matches this exact name — never rename it
  needs: [build, changes]
  # !cancelled() drops the implicit success(): the job RUNS even when Build Check or change detection failed...
  if: (needs.changes.outputs.search == 'true' || needs.changes.result != 'success') && !cancelled()
  steps:
    - name: Require a successful Build Check and change detection
      if: needs.build.result != 'success' || needs.changes.result != 'success'
      run: exit 1                    # ...and its first step turns that into a red job instead of a skip
```

- A job skipped by `if:` reports **success**, which is fine only for a PR that touches no search path. A broken
  build must not turn a search PR green.
- Build Check uploads `target/release/skim` as `skim-release` (1 day). The scoreboard downloads it to
  `$RUNNER_TEMP` (never under `target/`, which is cached) and restores the exec bit. The scoreboard itself is
  built **debug** (`target/debug/scoreboard`) under its own cache prefix, `cargo-build-scoreboard-`.
- The corpora cache (`.bench-corpus/scoreboard`, about 115 MB) is saved only by a successful job, so a clone cut
  off halfway is never cached. `report.md` goes to the step summary, and `report.json` + `report.md` go to the
  `scoreboard-report` artifact (30 days). That artifact is what you bless from. The job timeout is 25 minutes.

## Operating the Gate

Run everything from the workspace root, because every default path is relative to it:

```bash
cargo build --release -p rskim                   # ALWAYS first: the scoreboard tests whatever binary is at --skim-bin (PF-019)
caffeinate -i -s cargo run -p rskim-bench --bin scoreboard -- check --skim-bin target/release/skim
# reports: target/scoreboard/report.{json,md}   (--out to change)
cargo run -p rskim-bench --bin scoreboard -- bless --from target/scoreboard/report.json [--accept-regression "<why>"]
```

- The first run clones the corpora into the gitignored `.bench-corpus/scoreboard/` (about 113 MB on disk, about
  30 s on a fast link). A warm `check` takes about 3 minutes on Apple Silicon.
- `run` never gates: it exits 0 unless a harness error occurs. `check` = `run` + gate. `golden-gen --corpus <name>`
  prints up to 20 `[[ident]]` candidates for review. Each candidate has a unique definition, a name of at least
  6 bytes and 2–60 ground-truth files, ordered by `sha256("<corpus>:<name>")`. Never run it in CI.
- **Exit codes:** `0` pass. `1` gate failure, or bless refused. `2` harness error: never a regression, and no
  report is written.
- Each gate failure prints one stderr line, `FAIL <check> [<ids>]: <message>`. The failures are also listed in
  `report.md` under "Gate failures". Failure kinds: `Unledgered`, `Xpass`, `Ratchet`, `Baseline`.
- Harness-only tests: `tests/scoreboard.rs` drives the real `scoreboard` binary against a tempdir git fixture and
  a **stub `skim` shell script** (offline, never `target/*/skim`). Change harness behaviour there, not by running
  real corpora.

## Error Handling and Recovery

Every `Err` in `pipeline.rs` is exit 2. Each class exists because scoring through it would give a false signal:

| Harness error | Why it is not a gate result |
|---|---|
| Golden integrity: pin mismatch, a `def.line` that no longer contains the query, a bad regex, a duplicate or unprefixed id, a `zero-hit` entry with ground truth, a pagination entry over `min(limits) × 63`, a stale ledger ref | The golden data itself is wrong |
| `temporal_state` ≠ `"ready"` after `--build` while any entry uses `--hot` / `--cold` / `--risky` / `--blast-radius` (`require_temporal_data`) | skim serves a fallback order; a ledgered `--hot` check would **XPASS** and ask for a promotion that bakes the breakage into ledger and baseline (939d1e9) |
| `degraded[]` on **any** page of a temporal-ranked entry (`ensure_ranking_applied`) | Same reason. On other entries, `degraded[]` just feeds `silent_fn` |
| An empty full list on an oracle-less entry (`require_oracle_less_rows`) | An empty list passes every check vacuously; the #547 XFAILs would XPASS (a4c7b3a). A list that shrinks without emptying moves `oracle_less.full_rows.<id>` instead (HigherBetter, exact) |
| An oracle-less pagination full list over the bound | Integrity cannot bound `--ast` / `--blast-radius` in advance |
| Non-JSON / malformed envelope, a signal, a timeout, `--build` exit ≠ 0, a `--stats` `{"error":…}` | skim is broken, not worse |
| Corpus not `Reusable` after the run; over 50 000 walk-accepted files; unknown `--only`; an invalid data file or schema | The environment or input is wrong |

`run` / `check` delete any previous `report.json` / `report.md` in `--out` **before** doing any work (2245a7b).
A failed run therefore can never leave an older passing report for `bless --from` to pick up.

## Anti-Patterns

- **Blessing to turn the gate green without reading the failure.** "Bless required" means *look*.
  `--accept-regression` with a vague reason leaves a permanent, unhelpful `accepted_regressions[]` record.
- **Following an XPASS "promote" when the temporal or AST layer might be broken.** The harness errors above exist
  to stop exactly this. Never weaken `require_temporal_data`, `ensure_ranking_applied` or
  `require_oracle_less_rows` to get past an exit 2.
- **Ledgering without a filed ticket, or with guessed ids.** The parser rejects placeholders. Copy the exact ids
  from `report.json`.
- **Deriving golden data from skim's output**, or regenerating it in CI. Concept relevance regexes and
  `def` sites come from the corpus source, not from what skim ranks.
- **Importing skim code into `oracle.rs` / `universe.rs` / `metrics.rs`** to "stay in sync". Mirror it with a
  citation instead, or the scoreboard stops being able to catch the change.
- **Using `HashMap`** in any report or baseline type. It breaks byte-determinism.
- **Adding a subprocess that bypasses `git_output_with_timeout`**, or that times out with a direct-child kill.
- **Editing `SEARCH_PATHS` in only one place.** It is mirrored in `ci.yml`, the README "CI" section and the
  CLAUDE.md "Search quality gate" paragraph.

## Gotchas

- **Stale binary.** The scoreboard only canonicalizes `--skim-bin`. Neither the report nor the baseline records
  skim's version, so an old `target/release/skim` is silently what gets scored. Always build first (PF-019), and
  never run two release builds at once (CLAUDE.md resource rules).
- **macOS sleep.** An unattended Mac can sleep mid-run: one run stretched from about 3 minutes to about 27. The
  per-call timers stop during sleep, so the timings still look normal. Wrap long runs in `caffeinate -i -s`.
- **First exec on macOS** of the freshly linked `scoreboard`, `skim` or `rskim-bench` test binaries can stall at
  `_dyld_start` (XProtect scan). Warm each one with a throwaway exec (`--version` / `--help`) before you assume a
  hang (PF-013).
- **Every run is a cold build.** Each run gets a fresh temp HOME, so skim runs `--build` from scratch, including
  the full-history temporal walk. The incremental / staleness / auto-refresh paths are **not** exercised.
- **Any byte edit to a golden file**, even a comment, changes its `golden_sha256`, and that requires a bless. If
  you edit a golden file after the run, `bless` refuses the report.
- **`--only`** reports are partial. The gate still compares the covered corpus but skips the aggregate and
  missing-corpus checks, and `bless` refuses the report.
- **Mirrored skim policy.** Adding a language or extension to skim, or changing walker limits, the minified gate,
  the hidden-path rule or the file cap, requires the matching edit in `oracle.rs` `LANGS` / `universe.rs`.
  Otherwise `universe.delta` goes non-zero, and precision fails on files the oracle does not index. Adding a
  language also moves `coverage.tracked_text` (bless).
- **The text-mode output format is load-bearing.** `metrics::is_block_header` recognizes `<path>:<digit>…` or
  `<path>  [` as a result header. A renderer change can turn every `first_correct` into a miss
  (`bytes.first_correct_misses` is exact, LowerBetter). **stderr counts toward `bytes.text_*`**, so a new
  search-path stderr notice can move the byte ratchets. Small moves stay inside ±3%.
- **Reference columns** (`*.baseline_alpha`, `*.baseline_count`, `bytes.rg_*`) depend only on corpus, golden and
  oracle. If one moves on a PR that touches only skim, the harness changed.
- **The ±3% byte tolerance is relative to the baseline**, so a baseline of 0 tolerates no change.
- **`rskim-bench` denies `unwrap_used` / `expect_used` / `panic`**, so test modules carry `#[allow(...)]`. Lint
  with `cargo clean -p rskim-bench && cargo clippy -p rskim-bench --all-targets -- -D warnings` (PF-009:
  `gate_tests.rs` / `metrics_tests.rs` are `#[cfg(test)] #[path]` modules).
- **The skim corpus is `dean0x/skim` pinned at `b8a0a79`.** It never tracks the live tree, and it is bumped only
  by hand together with a bless.

## Key Files

- `crates/rskim-bench/scoreboard/README.md`: the runbook (commands, ledger, blessing, adding golden entries, pin bumps, CI).
- `crates/rskim-bench/src/bin/scoreboard.rs`: the CLI (`run|check|bless|golden-gen`), exit codes, sandbox HOME per run, `clear_outputs`.
- `crates/rskim-bench/src/scoreboard/pipeline.rs`: per-corpus orchestration and the harness-error guards (`require_temporal_data`, `require_oracle_less_rows`).
- `crates/rskim-bench/src/scoreboard/runner.rs`: skim subprocess contract, `SkimSandbox`, sweeps, `ensure_ranking_applied`.
- `crates/rskim-bench/src/scoreboard/metrics.rs`: `plan` / `runs` (which checks run), pure HARD checks, `RATCHET_METRICS`, `compare_ratchet`, byte parsing.
- `crates/rskim-bench/src/scoreboard/gate.rs`: ledger parsing and validation, `classify`, `evaluate` (the gate verdict).
- `crates/rskim-bench/src/scoreboard/baseline.rs`: `baseline.json` schema, `bless`, `hard_downgrades`, `regressions`.
- `crates/rskim-bench/src/scoreboard/golden.rs`: golden schema, `QueryFlags` (`uses_temporal_data`, `has_rank_override`, `arm`), integrity.
- `crates/rskim-bench/src/scoreboard/oracle.rs`: independent predicates (and / phrase / near / pnear / lang), baselines, simulated `rg -n -F`.
- `crates/rskim-bench/src/scoreboard/universe.rs`: the oracle's file universe mirroring the CLI walker, `GitIsolation`, coverage.
- `crates/rskim-bench/src/scoreboard/types.rs`: `CheckId`, `Arm` envelopes, `ResultPage`, `StatsSnapshot`.
- `crates/rskim-bench/scoreboard/{corpora.toml,golden/*.toml,known_failures.toml,baseline.json}`: the data. `baseline.json` is written only by `bless`.
- `crates/rskim-bench/tests/scoreboard.rs`: offline end-to-end tests with a stub skim.
- `crates/rskim-research/src/clone.rs`: `ensure_pinned_history_clone`, `verify_pinned_clone`, the process-group timeout (`git_output_with_timeout`).
- `.github/workflows/ci.yml`: the `changes` and `scoreboard` jobs, plus the `skim-release` upload in Build Check.

## Related

- **ADR-007** (amended 2026-09-25): this scoreboard is the required search gate. Manual adversarial dog-food only
  covers what it cannot see: structural until #541, temporal until #542, and new flags or arms.
- **ADR-008**: skim indexes walked ∪ tracked. `universe.rs` reproduces that union independently, and
  `universe.delta` must stay 0.
- **ADR-003**: ratchets against blessed measured values rather than invented targets. Golden data is declared up
  front.
- **ADR-005**: a green `Search Scoreboard` is not permission to merge.
- **PF-019**: build the binary the run spawns first. **PF-013**: warm fresh binaries on macOS. **PF-009**:
  `clippy --all-targets` after `cargo clean -p`.
- Feature knowledge `cmd-search`: the CLI code the scoreboard judges. The ledgered bugs live in
  `cmd/search/query.rs` (#544), `temporal.rs` `resort_window` (#545) and `ast.rs` (#547).
- Feature knowledge `search-temporal` / `temporal-scoring`: `--hot` / `--cold` / `--risky` / `--blast-radius`
  and `temporal_state`, which `require_temporal_data` reads.
- Feature knowledge `ast-index`: the standalone `--ast` arm (#547 order, #541 coverage gap).
- Feature knowledge `research-ast` / `cochange`: share `clone.rs`, including the process-group timeout that now
  bounds their git calls too.
