---
feature: search-scoreboard
name: Search scoreboard (quality gate) and the role oracle
description: "Use when a search PR's Search Scoreboard CI job fails, when changing skim search retrieval, ranking, pagination, the file universe or the text and JSON output, when running the scoreboard locally, when ledgering or promoting a known HARD failure, when blessing baseline.json (incl. --accept-regression), when adding golden queries or frozen answers ([[refs]], [[clash]], [[stakes]]), bumping a corpus pin or a tree-sitter grammar, when editing the role oracle in crates/rskim-oracle, or when editing the scoreboard harness (regex oracle, text universe, runner, gate, bless), the CI search-path filter, or rskim-research pinned-clone and subprocess-timeout code. Keywords: scoreboard, Search Scoreboard, run, check, bless, golden-gen, rskim-oracle, role oracle, roles.rs, tests/independence.rs, ALLOWED, text_universe, LEFT_OUT_REASONS, refs.recall, refs.calls_recall, clash.defs_complete, stakes.exit, universe.accounted, ident.def_first, concept.first_right_top5, roles.precision, cost.files_parsed_median, baseline.json, known_failures.toml, ledger, XFAIL, XPASS, promote, HARD, RATCHET, INFO, bless required, --accept-regression, accepted_regressions, corpora.toml, universe.delta, results.unique_paths, silent_fn, harness error, exit 2, SEARCH_PATHS, --no-renames, skim-release, scoreboard-report, ensure_pinned_history_clone, OWNERSHIP_MARKER, process_group, git_output_with_timeout, KILL_GRACE, caffeinate, .bench-corpus/scoreboard, eol=lf."
category: domain-knowledge
directories: [crates/rskim-bench/src/scoreboard/, crates/rskim-bench/src/bin/scoreboard.rs, crates/rskim-bench/scoreboard/, crates/rskim-bench/tests/scoreboard.rs, crates/rskim-oracle/, crates/rskim-research/src/clone.rs, .github/workflows/ci.yml]
referencedFiles:
  - crates/rskim-bench/src/scoreboard/pipeline.rs
  - crates/rskim-bench/src/scoreboard/runner.rs
  - crates/rskim-bench/src/scoreboard/metrics.rs
  - crates/rskim-bench/src/scoreboard/gate.rs
  - crates/rskim-bench/src/scoreboard/baseline.rs
  - crates/rskim-bench/src/scoreboard/golden.rs
  - crates/rskim-bench/src/scoreboard/universe.rs
  - crates/rskim-bench/src/scoreboard/oracle.rs
  - crates/rskim-bench/src/scoreboard/types.rs
  - crates/rskim-bench/src/scoreboard/golden_gen.rs
  - crates/rskim-oracle/src/roles.rs
  - crates/rskim-oracle/src/structural.rs
  - crates/rskim-oracle/tests/independence.rs
  - crates/rskim-bench/tests/independence.rs
  - crates/rskim-bench/scoreboard/known_failures.toml
  - crates/rskim-research/src/clone.rs
  - .github/workflows/ci.yml
created: 2026-09-25
updated: 2026-11-13
---

# Search scoreboard (quality gate) and the role oracle

## Overview

The scoreboard (#203) is the **required merge gate for search PRs**, the `Search Scoreboard` job in
`.github/workflows/ci.yml`. It runs the **release `skim` binary as a subprocess** against four pinned full-history
corpora (skim, ripgrep, flask, zod; the pins are in `corpora.toml`, never the live tree), checks every answer against
oracles that share no code with skim's search code, and compares ranking with naive baselines. It replaced the manual
dog-food campaign as the standing gate. Manual dog-food remains only where the scoreboard is blind:

- the text layout of the labelled view, the word view and the notices (the checks read the JSON envelope, the `-l` and
  `-c` output and the exit code);
- role precision outside the frozen names (`refs.call_precision` covers the `[[refs]]` names only; `roles.precision.*`
  covers the `[[refs]]` and `[[clash]]` names only);
- calls split by definition (a name with several definitions gets a note; `split = "none"`);
- latency (INFO), a corpus outside the four, and any new query flag until it has golden entries.

The runbook is `crates/rskim-bench/scoreboard/README.md`. This file covers what it does not spell out: how the pieces
couple across the crates, which invariant is enforced where, and the traps.

## Rules

- **KB-INV-1** Nothing on the scoring path imports skim code. `rskim-bench` depends on `rskim-research` and
  `rskim-oracle` only. Where an oracle must agree with skim it keeps its own copy with a citation, so a policy change
  on skim's side shows up as a scoreboard diff. `crates/rskim-bench/tests/independence.rs` fails the build when the
  manifest of `rskim-bench` names another `rskim-*` crate, or the manifest of `rskim-research` names any (`rskim-bench`
  may name itself, for its `test-utils` dev-dependency).
- **KB-INV-2** Golden data and frozen answers come from `git grep -nw` at the pin plus a hand read, never from skim's
  output, and CI never regenerates them.
- **KB-INV-3** `rskim-oracle` has no `rskim-*`, `path` or `git` dependency, no `build.rs`, no `include!` and no
  `#[path]` that leaves its directory. `tests/independence.rs` fails the build otherwise.
- **KB-INV-4** Every report, baseline and oracle-answer map is a `BTreeMap` or a struct, never a `HashMap`. The
  workspace `serde_json` has `preserve_order`, so a hash map would break byte-stable reports.
- **KB-INV-5** A check whose data the page does not state fails. It never passes by default.
- **KB-INV-6** `SEARCH_PATHS` is one list in three places: `ci.yml`, the scoreboard README "CI" section and CLAUDE.md.
  Edit all three together.
- **KB-AP-1** Do not bless without reading the failure. "Bless required" means look. A vague `--accept-regression`
  leaves a permanent, unhelpful `accepted_regressions[]` record.
- **KB-AP-2** Do not ledger a failure without a filed ticket or with guessed ids. Copy the exact ids from `report.json`.
- **KB-AP-3** Do not derive golden data from skim's output.
- **KB-AP-4** Do not start a subprocess outside `git_output_with_timeout`, or time out with a direct-child kill.

## Business Context

- **Why it exists.** Dog-food rounds give only negative evidence, never "done". The scoreboard measures what the
  shipped CLI returns: the same binary, the same flags and the same exit codes an agent sees.
- **What "passing" means.** Every HARD outcome is PASS or ledgered XFAIL, and nothing differs from the blessed
  `baseline.json` (RATCHET values, HARD states, pins, golden digests, corpus set). It does **not** mean skim beats the
  baselines: "beats baseline" is INFO.
- **Ratchets, not targets.** RATCHET values compare with blessed *measured* values, never with the "bar" column.
- **Green is not merge authorization** outside the standing authorization the owner gave for the #540 phase PRs.

## Core Business Rules

### Three check classes

| Class | Scope | Tolerance | Where defined |
|---|---|---|---|
| HARD | golden entry × check (`CheckId::ALL`, 23 checks) | 0; only a ledger entry excuses a failure | `types.rs` `CheckId`, `metrics.rs` `PlannedQuery::checks` |
| RATCHET | per corpus + aggregate | exact after `round4`; `bytes.text_*` and `bytes.first_correct_*` ±3% **relative to the baseline value** | `metrics.rs` `RATCHET_METRICS`, `compare_ratchet` |
| INFO | latency, beats-baseline, `bytes.rgw_*` and `bytes.first_relevant_median` (`info.reference` of each corpus) | never gated, not in `baseline.json` | `report.rs`, `metrics.rs` `reference_values` |

Which HARD checks run on an entry is decided in one place, `PlannedQuery::checks` (the ledger is validated against the
same plan, so a ledger entry can never name a check that does not run):

- `lexical.recall`, `.precision`, `.silent_fn` compare skim's full list with the regex oracle's files that hold every
  word of the query (an AND set).
- `pagination.*` only on `[[pagination]]`, `order.prefix_consistent` only on `[[prefix]]` entries.
  `results.unique_paths` runs on every entry, within each list.
- The frozen-answer checks run on their own entry kind only (next section). No `lexical.*` or ordering check runs on
  `[[refs]]`, `[[clash]]` or `[[stakes]]` entries.

Pagination and prefix checks compare skim with **its own full list** (`--limit 1_000_000`), so oracle-less entries can
be pagination entries. `lexical.silent_fn` fails when a ground-truth file is missing from the list.

### Frozen answers (#540): `[[refs]]`, `[[clash]]`, `[[stakes]]`, concept `first`, the text universe

Every frozen set comes from `git grep -nw` at the pin plus a hand read of each line. Golden integrity (exit 2) checks
the sets against the oracle, so a missing or an extra site cannot move the target. Each corpus holds 3+ `[[refs]]`
(ids `<corpus>-R<nn>`), 1 `[[clash]]` (`-D<nn>`; skim's is `run_parsed_command`, two definitions), 2 concept entries with
`first`, and 17 `[[stakes]]` (`-Q01` a zero-hit query with `exit = 1`; `-Q02` a match with `exit = 0`; `-Q03` the
conflicting flags `-F -E` with `exit = 2`; `-Q04`..`-Q17` flag cases that expect `files`, `lines` or `counts`, built
from `-i`, `-S`, `-w`, `-F`, `-E`, `-g`, `-t`, `-l`, `-c` and `-C`). The set of stakes ids is the same on every corpus.

| Table / field | Checks | Class |
|---|---|---|
| `[[refs]]` (`def`, `sites` with roles) | `refs.recall` (every site is an occurrence), `refs.calls_recall` (every frozen `call` is labelled `call`), `universe.accounted` (`searched_files` + `left_out` reasons equal the oracle's text universe) | HARD |
| `[[refs]]` | `refs.call_precision` (share of the `call` group that is frozen calls; no value when nothing is labelled `call`) | RATCHET |
| `[[clash]]` (`defs`, `calls`, `split = "none"`) | `clash.defs_complete`, `clash.def_count` (`at_least` passes only as a valid lower bound), `clash.calls_recall` | HARD |
| `[[lexical]]` | `lexical.recall_tracked` (recall over the text universe, no language allow-list) | HARD |
| `[[ident]]` | `ident.def_first` (the first definition site is the frozen `def` path and line) | HARD |
| `[[concept]]` `first` (optional) | `concept.first_right_top5` (a frozen first file is among the first 5 files) | HARD |
| `[[stakes]]` | `stakes.lines`, `stakes.files`, `stakes.counts` (against the regex oracle), `stakes.exit` (the frozen exit code) | HARD |
| role oracle, cost counters | `roles.precision.<lang>`, `cost.files_parsed_median`, `cost.bytes_parsed_median`, `cost.bytes_read_median` | RATCHET (no value until the output states the data; an absent row fails nothing and is not blessed) |
| `rg -nw` simulation | `bytes.rgw_text_median`, `bytes.rgw_first_correct_median` | INFO |

- **Text universe** (`universe.rs` `text_universe`, one function, an independent re-implementation of the engine's
  `cmd/search/universe.rs`): `git ls-files -z` plus untracked, not-ignored files with no hidden path component.
  Gitlinks and skip-worktree entries are dropped. No language allow-list, no minified gate and no file cap. Hidden and
  secret-bearing tracked files are in it, as `git grep` lists them. A file leaves it only for a counted `left_out`
  reason (`LEFT_OUT_REASONS`: `binary`, `not_utf8`, `too_large` above `MAX_FILE_BYTES` = 5 MiB, `unreadable`,
  `vanished`, `outside_root`, `symlink`). The reason names equal the engine's `left_out` keys.
- **Known divergence** (untracked files only; empty on a verified-clean clone): the engine's walk honours `.ignore`
  files and `git ls-files --exclude-standard` does not.
- **Adapter** (`types.rs` `ResultPage::from_envelope`, `from_files_text`, `from_counts_text`): the page is parsed from
  the one-line JSON envelope. An `error` key, a missing key, a wrong type or an unknown role is a harness error
  (exit 2), never a regression.
- **Integrity** (`golden.rs` `check_refs`, `check_clash`, `check_stakes`): `sites` equal the whole-word line set at the
  pin and include a `call` site; a clash has 2+ `defs`, 1+ calls and `split = "none"`; a stakes exit agrees with the
  oracle's hit count, and a stakes `lines`, `files` or `counts` entry has at least one matching line (an empty answer
  passes any output).
- **Stakes calls** (`runner.rs` `observe_stakes`): the entry's own flags go to skim. `-l` and `-c` cannot combine with
  `--json`, so those calls read text output (a path per line, or `path:count`). An entry that expects an exit code
  reads the exit status of a text call. A stakes `-g` is a subset of the `rg` glob rules in `oracle.rs` (`* ? ** !`,
  a trailing `/`); the oracle refuses `[ ] { } \` and an empty glob. `-t` uses the oracle's own copy of the `rg` type
  table. The oracle reads `-i`, `-S`, `-w`, `-F`, `-E`, `-g`, `-t`, `-l`, `-c` and `-C`.
- **Removed fields.** `[[ast]]`, `mode` and `near` are unknown golden fields, and every golden table denies unknown
  fields, so a golden file that uses one fails to load (exit 2).

### Ledger (`known_failures.toml`)

- Each `[[xfail]]` names `issue = "#<digits>"` (a filed ticket; `is_issue_ref` rejects placeholders and leading zeros),
  one dotted `check` and the exact failing `ids` from `report.json`. A `(check, id)` pair appears once.
- Ids map to a corpus by the **longest** `<corpus>-` prefix (`gate::corpus_of`). An id in no corpus, an id missing from
  golden, or a check that never runs on that entry is exit 2 (`golden::check_ledger`, `gate::unplanned_ledger_refs`).
- The ledger holds no entry at this commit.
- "Promote" is lifecycle 1 below: remove the ids, then bless `xfail -> pass` (no reason needed).

### Bless rules (`baseline.rs::bless`, pure)

`bless` refuses (exit 1) a partial `--only` report; a report whose per-corpus golden digest differs from what this
scoreboard computes for the files on disk (`golden_hashes_on_disk`; the digest is the SHA-256 of the golden file
bytes); any FAIL or XPASS; and any RATCHET regression or HARD downgrade (`pass -> xfail`, `<blessed> -> not run`, a
blessed corpus no longer run) without a non-blank `--accept-regression "<reason>"`. Reasons and accepted lines append
to `accepted_regressions[]` (carried forward, never dropped); a needless `--accept-regression` is ignored with a note.

| RATCHET movement (`compare_ratchet`) | `check` | `bless` |
|---|---|---|
| within tolerance | ok | n/a |
| Improved | FAIL "bless required" | no reason needed |
| Regressed (bad direction) | FAIL | needs `--accept-regression` |
| Changed: a Neutral value (reference columns), or a ZeroBest sign flip | FAIL | no reason needed |
| new metric / no longer measured | FAIL | no reason needed (`regressions()` compares only shared metrics) |

`universe.delta` and `universe.skipped_by_reason_mismatch` are RATCHETs (ZeroBest, exact, bar "= 0"), so leaving 0
needs a reason. Fix the mismatch instead: the oracle reproduces the engine's universe independently.

### Oracle independence

Nothing on the scoring path may import skim's search code. Where an oracle must agree with skim it keeps **its own
cited copy**, so a policy change on skim's side shows up as a scoreboard diff instead of flowing through silently:

- `oracle.rs`: the word-character class, the `rg` `-t` type table and the `-g` glob subset, `LANGS`;
- `universe.rs`: `MAX_FILE_BYTES` (5 MiB), the left-out reasons, the hidden-component rule for untracked files;
- `rskim-oracle`: `structural.rs` carries the language table (`OracleLang`, `classify`, `EXT_CLASSES`).

Independence holds at two strengths: **compile time** for `rskim-oracle` (below), and for `rskim-bench` because its
manifest no longer lists `rskim-core`, `rskim-search` or any tree-sitter crate. It depends on `rskim-research` (the
pinned clones) and `rskim-oracle` only.

## State Transitions

HARD outcome classification (`gate::classify`) happens before the baseline comparison:

| raw result | ledgered? | outcome | gate | blessable |
|---|---|---|---|---|
| pass | no | PASS | ok | yes |
| fail | yes | XFAIL | ok | yes |
| fail | no | FAIL (Unledgered) | fails | no |
| pass | yes | XPASS | fails: "promote" | no |

The baseline stores only `pass` / `xfail` per `(id, check)`; any change, `new -> pass` included, is "bless required".

1. **You fixed a ledgered bug.** XPASS. Remove the ids (or entry) and push; CI shows `xfail -> pass; bless required`;
   bless from that run's `scoreboard-report` artifact and push `baseline.json` (or locally in one push:
   `scoreboard run --out <dir>`, then `bless --from <dir>/report.json`).
2. **A real bug you will not fix here.** File the ticket, add an `[[xfail]]` with the exact ids, re-bless with
   `--accept-regression "<reason>"` (`pass -> xfail` is a downgrade).
3. **You added a HARD check or golden entries.** `new -> pass` / `new -> xfail` plus a digest change: bless, no reason.
   New `[[concept]]` and `[[ident]]` entries change the pooled `concept.*`, `ident.*` and `bytes.text_*` values (and
   the baseline columns beside them): a pooled value that moves in its bad direction needs `--accept-regression`, even
   when only the entry set changed. Read each move before you write the reason.
4. **You removed a golden entry.** Its checks go `-> not run`, a downgrade: remove its ledger ids too, bless with
   `--accept-regression`.
5. **You bumped a corpus pin.** Update `corpora.toml` and the golden `commit` together, re-verify every `def.line` and
   every frozen `sites` set (drift is exit 2); CI re-clones (cache key `hashFiles(corpora.toml)`). Refresh ledger ids
   from the new `report.json`, bless.

## Technical Implementation Patterns

### Per-corpus pipeline (`pipeline::run_corpus`, in order)

The scan engine reads the files on every call, so a run has no build step, no stats call and no index to wait for.

1. `materialize_verified` (clone or reuse at the pin, require `Reusable`).
2. `text_universe` (the oracle universe) with the runner's `GitIsolation`, then `checked_plan`: golden integrity plus
   ledger refs, then `metrics::plan`.
3. `observe_plan`: for each planned entry `runner.observe`, which makes the calls the entry needs (below).
4. `verify_untouched`: the corpus must still be `Reusable` (skim wrote nothing into the clone).
5. `metrics::evaluate`, `corpus_report` (ledger applied); aggregate RATCHET values and `gate::evaluate` run last.

### Runner contract (`runner.rs`)

- `search --root <clone> --json --limit N [--offset K] <flags> -- <query>`: flags before `--`, the query after, so
  `-D warnings` and `->` parse as text. One temp `HOME` per run (`SkimSandbox`) redirects `SKIM_CACHE_DIR`, agent
  config dirs and `SKIM_WRAPPERS_DIR`, sets `SKIM_DISABLE_ANALYTICS=1` / `NO_COLOR=1`, strips `SKIM_PASSTHROUGH` /
  `SKIM_DEBUG` / hook and session vars; the oracle's `git ls-files` shares its `GitIsolation`.
- A non-zero exit whose stdout parses as the envelope is accepted, and the page keeps the real exit code
  (`ResultPage::exit_code`). A signal, a timeout, non-JSON stdout or an `error` envelope is a harness error.
- `observe` makes: the full list (`FULL_LIST_LIMIT`); for `[[pagination]]` one sweep per limit (at most `MAX_PAGES` =
  64 pages); for `[[prefix]]` one limited list per limit; for `[[ident]]` and `[[concept]]` one text-mode run; for
  `[[stakes]]` the one call its flags name.
- Byte metrics come from the text-mode run at the default limit and count **stdout + stderr** (`text_bytes`).
  `bytes_through_def_line` runs to the `def <path>:` line (ident); `bytes_through_file_block` runs to the end of the
  first relevant file's block (concept).

### Subprocess timeouts kill the whole process group (`clone.rs`)

Every subprocess goes through `rskim_research::clone::git_output_with_timeout` (skim calls 120 s /
`SKIM_TIMEOUT_SECS`, network git 300 s, local git and `ls-files` 120 s). Killing only the direct child is not enough: a
grandchild holding the pipes keeps `wait_with_output` blocked. `spawn_in_own_group` sets `process_group(0)`; on the
deadline `kill_process_tree` sends SIGKILL to `-pid` then `pid` (`taskkill /T` off-unix), and the wait thread is joined
only if it finishes within `KILL_GRACE` (2 s), otherwise detached, so nothing blocks past timeout + 2 s. Reuse
`git_output_with_timeout` / `git_run_with_timeout` for any new subprocess.

### The `rskim-oracle` crate and the role oracle

`rskim-oracle` is its own crate so that its independence is a **compile-time** fact.
`crates/rskim-oracle/tests/independence.rs` checks:

- every entry of every dependency table (`[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, and their
  `[target.<cfg>.*]` forms; `workspace = true` resolved through `[workspace.dependencies]`) is a registry crate on
  `ALLOWED`: `tree-sitter`, the five grammars (rust, python, typescript, javascript, go), `anyhow`, `rayon`, `serde`,
  `sha2`, `toml`;
- a `rskim-*` crate fails with its own message however named (a `package =` rename included), as does any `path` /
  `git` source (a local crate could be skim's code under another name);
- no `build.rs` / `package.build`, and no file under `src/`, `tests/`, `benches/`, `examples/` contains `include!(` or
  a `#[path = "…"]` leaving its directory. It is a text scan: a comment spelling either fails too. `include_str!` of
  the `.scm` data is allowed.

`roles.rs` is the role oracle. `role_sites(lang, source, name)` gives the lines where a name is declared (`def`), in a
callee position (`call`) and below an import node (`import`), for Rust, Python, TypeScript, TSX, JavaScript and Go.
`definition_names(lang, source)` lists every declared name with its line. The grammar knowledge is 18 query files,
`queries/roles/<role>.<lang>.scm`, and line 2 of each (`; Grammar: <crate> <version>`) must equal the version the
workspace `Cargo.lock` resolves. `.tsx` is parsed with the TSX grammar.

- **Consumers.** `golden-gen` (`golden_gen.rs` `definition_sites`) and the `roles.precision.<lang>` rows
  (`metrics.rs` `page_role_tallies`). For each `[[refs]]` and `[[clash]]` query, a `def`, `call` or `import` label of the
  full list is confirmed when `role_sites` lists the same line under the same role for that name. Labels are pooled per
  language over the corpora. A language with no label has no value (at this commit `tsx`, `javascript` and `go`).
- **`structural.rs`** supplies the language table (`OracleLang`, `classify`, `EXT_CLASSES`) and `OracleScratch` (one
  parser and one `QueryCursor` per rayon worker, reused file to file; the cursor holds at most `ORACLE_MATCH_LIMIT` =
  4096 in-progress matches, set once when the scratch is made, because lowering it on a used cursor would not bind).
  Its pattern queries (`queries/<pattern>.<lang>.scm`) and post-filters belong to the removed `--ast` flag. Nothing
  scores them, and a later change can delete them.

### `golden-gen`

`scoreboard golden-gen --corpus <name>` prints candidate `[[ident]]` entries: names with exactly one definition site in
the oracle universe (from the role oracle), at least `MIN_NAME_BYTES` long, whose ground truth covers
`GT_FILES_MIN..=GT_FILES_MAX` files, ordered by `sha256("<corpus>:<name>")`, the first `GENERATED_PER_CORPUS` taken. The
output is a proposal: a person reviews it, pastes it into `golden/<corpus>.toml` and freezes it there. Never run in
CI. A name with two role-oracle definition sites (a struct field and a method) is not proposed.

### Pinned full-history clones (`ensure_pinned_history_clone`)

- Full history (`--no-checkout`, no `--depth` / `--filter`); `Shallow` is never reusable. An unreachable pin gets one
  `fetch origin <sha>`; it re-clones **once**, then errors. A non-reusable destination is deleted only if empty or
  holding `.git/skim-scoreboard-pinned-clone` (`OWNERSHIP_MARKER`), so a mis-set `--corpus-dir` errors, never deletes.
- `credential.helper=`, `transfer.fsckObjects=true`, one downgrade `fetch.fsck.zeroPaddedFilemode=ignore` (flask's
  `040000` tree modes); every call scrubs `GIT_DIR` / `GIT_WORK_TREE` / … and sets `GIT_CEILING_DIRECTORIES` to the
  destination's parent.

### Determinism

`report.json` is byte-identical across runs of one binary except `latency`. Every report / baseline map **must be a
`BTreeMap`** (or a struct); lists are sorted, floats go through `round4`, nothing records a timestamp, version or
machine path. Bless from the CI `scoreboard-report` artifact, because ubuntu is the platform that gates.

### CI wiring: fail closed, never a green skip

`changes` (**Detect Search Changes**) always gates `workflow_dispatch` and pushes to `main`, never pushes to
`feature/*` / `wave/**`, and fails closed on unknown events. On a PR, `git diff --name-only --no-renames -z HEAD^1
HEAD` is matched against `SEARCH_PATHS`; `--no-renames` counts a file moved *out of* a search path. `SEARCH_PATHS`
lists `crates/rskim/src/cmd/search/` and the `rskim` files that module imports from or that dispatch to it (find them
with `git grep -n 'crate::' crates/rskim/src/cmd/search`, including grouped `use` lists), `crates/rskim-core/`,
`crates/rskim-bench/`, `crates/rskim-oracle/`, `crates/rskim-research/`, the root `Cargo.*` and `ci.yml`.

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

A job skipped by `if:` reports **success**, fine only outside the search paths. Build Check uploads its release `skim`
as `skim-release` (downloaded to `$RUNNER_TEMP`, never the cached `target/`); the scoreboard build cache prefix is
`cargo-build-scoreboard-`; the corpora cache is saved only by a successful job; `report.json` + `report.md` go to the
`scoreboard-report` artifact (30 days), which is what you bless from. Timeout 25 minutes.

## Operating the Gate

```bash
cargo build --release -p rskim                   # ALWAYS first: the scoreboard tests whatever binary is at --skim-bin
caffeinate -i -s cargo run -p rskim-bench --bin scoreboard -- check --skim-bin target/release/skim
# reports: target/scoreboard/report.{json,md}   (--out to change); run from the workspace root
cargo run -p rskim-bench --bin scoreboard -- bless --from target/scoreboard/report.json [--accept-regression "<why>"]
```

- **Run time:** the first run clones about 115 MB into gitignored `.bench-corpus/scoreboard/`. The run time of a warm
  `check` has not been measured for the scan engine.
- `run` never gates; `check` = `run` + gate. `golden-gen --corpus <name>` proposes up to 20 `[[ident]]` entries.
- **Exit codes:** `0` pass; `1` gate failure or bless refused; `2` harness error (never a regression; no report).
  Each gate failure prints `FAIL <check> [<ids>]: <message>` (kinds `Unledgered`, `Xpass`, `Ratchet`, `Baseline`).
- `tests/scoreboard.rs` drives the real binary against a tempdir git fixture and a **stub `skim` script** (offline); the
  stub is keyed by the hex of `query\nflags`. Change the test harness there.

## Error Handling and Recovery

Every `Err` in `pipeline.rs` is exit 2, because scoring through it would give a false signal:

| Harness error | Why it is not a gate result |
|---|---|
| Golden integrity: pin mismatch, drifted `def.line`, bad regex, duplicate / unprefixed id, `zero-hit` with ground truth, pagination over `min(limits) × 63`, a frozen set that differs from the oracle, an unknown field, a stale ledger ref | The golden data itself is wrong |
| Malformed envelope, an unknown role, signal, timeout, an `error` envelope | skim is broken, not worse |
| Corpus not `Reusable` after the run; unknown `--only`; invalid data file | The environment or input is wrong |

`run` / `check` delete any previous `report.json` / `report.md` in `--out` **before** any work, so a failed run never
leaves an older passing report for `bless --from`.

## Anti-Patterns

- **Following an XPASS "promote" when the cause might be a broken check**, or weakening any exit-2 guard to get past it.
- **Importing skim code into the scoring modules**, or giving `rskim-oracle` an `rskim-*` / `path` / `git` dependency, a
  `build.rs`, an `include!` or an escaping `#[path]`. Mirror the policy with a citation instead, or the scoreboard stops
  catching the change.
- **Passing a stakes flag the oracle does not read.** Teach the oracle the flag first (`oracle.rs`), then use it in a
  `[[stakes]]` entry.
- **Editing a frozen `sites` set to match skim's output.** The set is a hand read of the pinned source.

## Gotchas

- **Stale binary.** Only `--skim-bin` is canonicalized; nothing records skim's version, so an old `target/release/skim`
  is silently what gets scored. Build first; never run two release builds at once.
- **macOS.** Sleep can stretch a run while the per-call timers look normal (`caffeinate -i -s`). A freshly linked
  binary's first exec can stall at `_dyld_start`: warm it with `--version`.
- **Every call is a cold read.** There is no index and no cache, so no incremental path exists to exercise.
- **Digest-changing edits force a re-bless.** Any golden byte (a comment included) moves `golden_sha256`, and `bless`
  refuses a report made before the edit. `.gitattributes` pins `text eol=lf` on `crates/rskim-oracle/queries/**` and
  `crates/rskim-bench/scoreboard/**` so a CRLF checkout cannot ask for a bless.
- **Skim-side changes surface here, on purpose.** A new language or extension, or a change to the file size bound, the
  hidden-path rule or the left-out reasons, needs `oracle.rs` / `universe.rs` edits, or `universe.delta` leaves 0.
- **`--only`** reports are partial: no aggregate or missing-corpus checks, and `bless` refuses them.
- **Text-mode output is load-bearing.** `metrics::bytes_through_def_line` and `bytes_through_file_block` read the
  layout of the labelled and word views; a renderer change can turn every `first_correct` into a miss. **stderr counts
  toward `bytes.text_*`**, so a new search notice moves the byte ratchets (±3% is relative: a baseline of 0 tolerates
  nothing).
- **Reference columns** (`*.baseline_alpha`, `*.baseline_count`, `bytes.rg_*`) moving on a skim-only PR means the
  harness changed.
- **Lints.** `rskim-bench` and `rskim-oracle` deny `unwrap_used` / `expect_used` / `panic` (test modules carry
  `#[allow(...)]`); lint with `cargo clean -p <crate> && cargo clippy -p <crate> --all-targets -- -D warnings`, because
  `*_tests.rs` are `#[cfg(test)] #[path]` modules.

## Key Files

- `crates/rskim-bench/src/bin/scoreboard.rs`: CLI (`run|check|bless|golden-gen`), exit codes, sandbox HOME.
- `crates/rskim-bench/src/scoreboard/pipeline.rs`: per-corpus orchestration, exit-2 guards.
- `crates/rskim-bench/src/scoreboard/runner.rs`: skim subprocess contract, `SkimSandbox`, `observe`, `observe_stakes`.
- `crates/rskim-bench/src/scoreboard/metrics.rs`: `plan` / `checks`, HARD checks, `RATCHET_METRICS`, `compare_ratchet`.
- `crates/rskim-bench/src/scoreboard/gate.rs` / `baseline.rs`: ledger, `classify`, `evaluate`; `baseline.json` and `bless`.
- `crates/rskim-bench/src/scoreboard/golden.rs` / `golden_gen.rs`: schema, integrity, digest; proposals.
- `crates/rskim-bench/src/scoreboard/oracle.rs` / `universe.rs`: regex oracle, `rg` type and glob copies; the text universe.
- `crates/rskim-bench/src/scoreboard/types.rs`: `CheckId`, `Role`, `ResultPage` and the envelope parser.
- `crates/rskim-oracle/src/roles.rs` (+ `roles_tests.rs`): the role oracle. `src/structural.rs`: language table and scratch.
- `crates/rskim-oracle/tests/independence.rs` and `queries/roles/*.scm`: the independence guard and the role queries.
- `crates/rskim-bench/tests/independence.rs`: the manifest guard for `rskim-bench` and `rskim-research`.
- `crates/rskim-bench/scoreboard/{corpora.toml,golden/*.toml,known_failures.toml,baseline.json}`: the data (`baseline.json` only via `bless`).
- `crates/rskim-bench/tests/scoreboard.rs`: offline stub-skim tests; `crates/rskim-research/src/clone.rs`: pinned clones, process-group timeout.
- `.github/workflows/ci.yml`, root `Cargo.toml` (`[profile.dev.package.tree-sitter]`), `.gitattributes` (LF for hashed files).

## Related

- Feature knowledge `cmd-search`: the CLI judged here (the scan engine, its views, `left_out` reasons and exit codes).
- Feature knowledge `symbol-read`: the declaration rule that the engine's `def` role reuses.
