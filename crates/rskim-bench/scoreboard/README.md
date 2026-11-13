# Search scoreboard

The scoreboard is the end-to-end quality gate for `skim search` (#203). It runs the **release `skim` binary** as a
subprocess, the same path an agent takes, against four pinned corpora. It checks every answer against oracles that
share no code with skim's search code. It also compares ranking with naive baselines.

It is the **required merge gate for search PRs** (owner decision 2026-09-25, SEARCH-ADR-007 amendment). In CI it is the
`Search Scoreboard` job in `.github/workflows/ci.yml`.

- Code: `crates/rskim-bench/src/scoreboard/`. The binary is `src/bin/scoreboard.rs`. The offline tests are
  `tests/scoreboard.rs` (a stub skim, a fixture corpus, no network).
- The role oracle is its own crate, `crates/rskim-oracle/` (`src/roles.rs`). It finds where a name is defined, called
  and imported, from tree-sitter queries in `crates/rskim-oracle/queries/roles/<role>.<lang>.scm`. `golden-gen` uses it
  to propose entries. No check scores skim with it yet (see [What it does not cover](#what-it-does-not-cover-yet)).
- Data: this directory.

| File | Holds |
|---|---|
| `corpora.toml` | The four corpora (skim, ripgrep, flask, zod): URL, a 40-hex commit pin, and language |
| `golden/<corpus>.toml` | The frozen golden queries for one corpus (`commit` must equal its pin) |
| `known_failures.toml` | The ledger: known HARD failures, each tied to a filed ticket |
| `baseline.json` | The blessed state: every HARD outcome and RATCHET value, pins and golden hashes. Written only by `bless` |

## What it measures

The harness calls `skim search --json` and reads the one-line JSON object. It builds nothing first, because each query
reads the files. Two kinds of expectation exist: an oracle computes the answer from the corpus (the regex oracle, the
text universe), or the answer is frozen in the golden file from a hand read.

| Class | Checks | Rule |
|---|---|---|
| **HARD** (per query) | `lexical.recall`, `lexical.precision` (skim's full list = the oracle's files that hold every word of the query) · `lexical.silent_fn` (a ground-truth file is missing) · `pagination.complete` / `.disjoint` / `.ordered` / `.has_more_honest` · `order.prefix_consistent` · `order.score_monotone` · `results.unique_paths` (no path twice in one list: the full list, a `--limit` list, or one page) · the frozen-answer checks `refs.recall`, `refs.calls_recall`, `clash.defs_complete`, `clash.def_count`, `clash.calls_recall`, `lexical.recall_tracked`, `universe.accounted`, `ident.def_first`, `concept.first_right_top5`, `stakes.lines`, `stakes.files`, `stakes.counts`, `stakes.exit` ([Frozen answers](#frozen-answers-refs-clash-stakes-and-the-text-universe)) | Tolerance 0. A failure passes only if it is ledgered (XFAIL). |
| **RATCHET** (per corpus + aggregate) | `universe.delta`, `universe.skipped_by_reason_mismatch` · `coverage.tracked_text` · `ident.def_top1`, `ident.mrr`, `ident.anchor_eq_def`, `ident.def_line_in_snippet` · `concept.p5`, `concept.p10` · `bytes.text_median`, `bytes.text_p90`, `bytes.first_correct_median`, `bytes.first_correct_misses` · the baseline columns (`*.baseline_alpha`, `*.baseline_count`, `bytes.rg_*`) · `refs.call_precision`, `roles.precision.<lang>` (`rust`, `python`, `typescript`, `tsx`, `javascript`, `go`) and the three cost counters `cost.files_parsed_median`, `cost.bytes_parsed_median`, `cost.bytes_read_median` (a row the search output cannot state has no value: it is absent, fails nothing and is not blessed) | A change in **either direction** fails with "bless required". Tolerance is 0 after rounding to 4 dp, except the byte medians and p90s at ±3%. |
| **INFO** (never gated) | Latency: p50/p95 of the golden entries' own skim calls (wall time, and `duration_ms` from the envelope) · the "beats baseline" column · the `rg -nw` reference rows `bytes.rgw_text_median` and `bytes.rgw_first_correct_median`, and `bytes.first_relevant_median`, in `info.reference` of each corpus in `report.json`: they are never gated and not in `baseline.json` | none |

A changed golden file, corpus pin, or HARD outcome (for example `xfail -> pass`) also means "bless required". A HARD
downgrade (`pass -> xfail`, or a blessed check, entry or corpus that no longer runs) is blessed like a RATCHET
regression: only with `--accept-regression "<reason>"`.

### What it does not cover yet

Manual adversarial dog-food (SEARCH-ADR-007) is still required for these:

- The text layout of the labelled view and the word view (`def` lines, section headers, the `other:` line, notices).
  The checks read the JSON envelope, the `-l` and `-c` output and the exit code.
- Role precision outside the frozen names. `refs.call_precision` covers the names in `[[refs]]` entries only, and
  `roles.precision.<lang>` covers the names of `[[refs]]` and `[[clash]]` entries only.
- Calls split by definition. A name with several definitions gets a note, and the calls are not split (`split = "none"`).
- Latency. It is INFO, because shared CI runners are noisy. The cost counters (`cost.*`) are the RATCHET.
- A corpus outside the four, and a file count above the largest corpus.
- Any new query flag, until it has golden entries here.

## Run it locally

Run from the workspace root; every default path is relative to it.

```bash
cargo build --release -p rskim                        # the binary under test
cargo run -p rskim-bench --bin scoreboard -- check --skim-bin target/release/skim
```

- The first run clones about 115 MB of full-history corpora into `.bench-corpus/scoreboard/` (gitignored), which
  takes about 30 s on a fast link.
- On macOS, run long checks under `caffeinate -i -s`. An unattended Mac can sleep in the middle of a run, and the
  per-call timers stop during sleep.
- Reports go to `target/scoreboard/report.{json,md}` (change this with `--out`).
- Follow CLAUDE.md's resource rules: never run two release builds at once.

| Subcommand | What it does |
|---|---|
| `run` | Runs every corpus and writes `report.json` + `report.md`. It never gates: exit 0 unless a harness error occurs. |
| `check` | `run`, then gates against `baseline.json` and `known_failures.toml`. This is what CI runs. |
| `bless --from <report.json> [--accept-regression "<reason>"]` | Rewrites `baseline.json` from a report (see [Blessing](#blessing)). |
| `golden-gen --corpus <name>` | Prints candidate `[[ident]]` entries for one corpus on stdout. Never run in CI. |

`run` and `check` take these flags:

- `--skim-bin` (default `target/release/skim`)
- `--corpus-dir` (default `.bench-corpus/scoreboard`)
- `--data-dir` (default `crates/rskim-bench/scoreboard`)
- `--only <corpus>`, a partial run that `bless` refuses
- `--out` (default `target/scoreboard`)

### Exit codes

| Code | Meaning |
|---|---|
| `0` | Gate passed (`check`), the run finished (`run`), or the baseline was written (`bless`). |
| `1` | Gate failure (`check`), or `bless` refused. |
| `2` | Harness error: network or clone verification, golden integrity, an invalid data file, a skim crash, timeout, unparsable output or `error` envelope, or a corpus changed by the run. A harness error is never reported as a regression, and no report is written. |

On a gate failure, stderr prints one `FAIL <check> [<ids>]: <message>` line per failure, and `report.md` lists them
under "Gate failures".

## The ledger (`known_failures.toml`)

```toml
[[xfail]]
issue = "#NNN"                          # a FILED ticket; placeholders such as #NEW are rejected
check = "pagination.has_more_honest"    # a HARD check's dotted name
ids = ["skim-G001", "skim-G002"]        # the exact failing ids, copied from report.json
note = "the cause, in one line"         # optional
```

| Outcome | Gate | What to do |
|---|---|---|
| Ledgered failure (**XFAIL**) | passes | Nothing. The ticket tracks it. |
| Unledgered failure (**FAIL**) | fails | Fix it. If it is a real bug you are not fixing in this PR, file a ticket first, then add an `[[xfail]]` entry with that number and the exact ids, and re-bless with `--accept-regression "<reason>"`: a blessed `pass -> xfail` is a downgrade. |
| Ledgered check that passes (**XPASS**) | fails, "promote" | Your change fixed it. Remove the id (or the whole entry) from `known_failures.toml`, then re-bless: `xfail -> pass` is a baseline change. |
| RATCHET value moved | fails, "bless required" | Re-bless. Improvements need no reason. Regressions need `--accept-regression "<reason>"`, which is recorded in `accepted_regressions[]`. A neutral value (a baseline reference column) needs no reason in either direction. |

A `(check, id)` pair may appear only once. An id that is not in the golden set, or a check that never runs on that
entry, is a golden-integrity error (exit 2), so a stale entry cannot silently XFAIL nothing.

## Blessing

`bless` rewrites `baseline.json` from a `report.json`. It refuses (exit 1) in these cases:

- a partial (`--only`) report;
- a report made from other golden files than the ones on disk;
- any `fail` or `xpass` record (fix the failure or ledger it first);
- a RATCHET regression or a HARD downgrade (`pass -> xfail`, or a blessed check, entry or corpus that no longer
  runs) without `--accept-regression`.

**Bless from the CI artifact**, because CI (ubuntu) is the platform that gates. Every `Search Scoreboard` run uploads
a `scoreboard-report` artifact (`report.json` + `report.md`, kept for 30 days):

```bash
gh run download <run-id> -n scoreboard-report -D /tmp/scoreboard-report
cargo run -p rskim-bench --bin scoreboard -- bless --from /tmp/scoreboard-report/report.json
# a regression you accept on purpose:
cargo run -p rskim-bench --bin scoreboard -- bless --from /tmp/scoreboard-report/report.json \
  --accept-regression "concept.p10 drops: three new cross-module concept queries"
git add crates/rskim-bench/scoreboard/baseline.json
```

Commit `baseline.json` in the same PR as the change that moved it. A local
`scoreboard run --out <dir>` report works the same way.

Promoting a fixed ledger entry therefore takes two steps:

1. Remove the entry, then push. The CI run fails with "bless required" (`xfail -> pass`).
2. Bless from that run's artifact, then push again.

## Adding a golden query

Golden data is declared **before** looking at skim's output (SEARCH-ADR-003), and CI never regenerates it. Each entry needs
an `id` that is unique and prefixed with `<corpus>-`. The existing id scheme:

- `L..` curated identifiers, `I..` generated identifiers;
- `C..` concepts;
- `S..` / `X..` / `K..` / `Z..` lexical cases, plus the seed's two-digit `G01` / `G02` language cases;
- `G0xx` pagination, `F0xx` prefix;
- `R..` frozen sites of one name (`[[refs]]`), `D..` a name with two or more definitions (`[[clash]]`), `Q..` stakes
  (`[[stakes]]`). The tables are described under [Frozen answers](#frozen-answers-refs-clash-stakes-and-the-text-universe).

```toml
[[ident]]      # definition ranking: def.line must contain `query` at the pinned commit
id = "skim-L01"
query = "check_staleness"
def = { path = "crates/rskim/src/cmd/search/staleness.rs", line = 321 }
origin = "seed"               # seed | generated | curated

[[concept]]    # ranking: relevance is a regex over file text, written from the corpus source
id = "skim-C20"
query = "hook watchdog"
relevant = '(?i)hook[_\s-]*watchdog'

[[lexical]]    # recall/precision edge case: every word of `query` must occur as a case-sensitive substring
id = "skim-X09"
query = "-D warnings"
category = "punct"            # substr | short | punct | case | lang | zero-hit
# lang = "toml"               # optional: passed as -t, applied by the oracle's own type table

[[pagination]] # --offset sweep per limit; full count must be <= min(limits) x 63
id = "skim-G009"
query = "elision marker"
limits = [3, 7, 20]

[[prefix]]     # --limit N must equal the first N rows of the full list
id = "skim-F005"
query = "staleness"
limits = [5, 20]
```

(The values above are illustrative; check each one against the pinned clone before you commit it.) The tables take no
other field. A field of the removed flags (`mode`, `near`, `flags` on a pagination or prefix entry) and the old `[[ast]]`
table are unknown fields: the load fails with exit 2.

Golden integrity runs before any query, and a violation is exit 2, never a gate failure. It checks:

- the pin matches `corpora.toml`;
- each `def.line` contains the query at the pinned commit;
- regexes compile;
- ids are unique, `zero-hit` entries have no ground truth, and pagination fits its bound;
- the frozen-answer tables hold their rules ([Frozen answers](#frozen-answers-refs-clash-stakes-and-the-text-universe)):
  a `[[refs]]` site set that differs from the whole-word line set at the pin, a `[[clash]]` with fewer than 2
  definitions, a `first` path outside the universe and a `[[stakes]]` exit code that disagrees with the oracle's hit
  count are all errors;
- every ledger entry refers to a real entry and check.

For identifiers, `scoreboard golden-gen --corpus <name>` proposes candidates: definitions from the role oracle with
exactly one site in the corpus, a name of at least 6 bytes, and 2–60 ground-truth files, ordered by
`sha256("<corpus>:<name>")`. Review the proposal, then paste it.

Adding any entry changes the golden hash, so the next `check` says "bless required". Bless from that run.

## Frozen answers: refs, clash, stakes and the text universe

Three golden tables and one concept field freeze the answers a plain-word search must give (#540). Each frozen set
comes from `git grep -nw` at the pin plus a hand read of every line, never from skim's output. Each corpus holds 3
`[[refs]]` entries, 1 `[[clash]]` entry (a name with two definitions at the pin), 2 concept entries with `first`, and
17 `[[stakes]]` entries (the same set of subjects on every corpus).

| Table | Holds | Integrity (exit 2 when broken) |
|---|---|---|
| `[[refs]]` | `id`, `query` (a bare identifier), `def {path, line}`, `sites [{path, line, role}]`, `origin`. Every line that holds the name as a whole word, with its role: `def`, `call`, `ref`, `import`, `comment`, `string` or `text`. | The `sites` equal the whole-word line set of the query over the text universe at the pin, and `def` is one of them. |
| `[[clash]]` | `id`, `query`, `defs [{path, line}]` (2 or more), `calls [{path, line, def}]`, `split`, `origin`. A name with two or more definitions. `def` of a call is an index into `defs` or `"unassigned"`. | At least 2 `defs`; each call index is in range; every def and call line holds the name as a whole word; `split = "none"` (the value `"exact"` is refused until calls are split by definition). |
| `[[stakes]]` | `id`, `query`, `flags` (rg-style, from a closed list), `expect` (`lines`, `files`, `counts` or `exit`) and `exit` for `expect = "exit"`. A plain-text query judged against the regex oracle. | `exit` is present exactly when `expect = "exit"`, and it is `0`, `1` or `2`. `1` needs no matching line in the text universe, `0` needs one, and `2` needs a query or flag set that the oracle rejects. |
| `[[concept]]` `first` | An optional list of paths: the files that hold the answer to a plain-word question. A concept entry without `first` still loads. | Each path is a file of the text universe at the pin. |

The stakes flags are `-g`, `-t`, `-l`, `-c`, `-E`, `-F`, `-i`, `-S`, `-w` and `-C`. The runner sets `--json`, `--limit`,
`--offset`, `--root` and `-h` itself, so an entry must not repeat them. `-l` and `-c` cannot be combined with `--json`,
so those calls read the text output. A stakes query with ASCII whitespace needs `-F` or `-E`, because the search reads
a bare multi-word query as a list of words, which the line oracle cannot judge.

| Check | Class | Runs on | Passes iff / measures |
|---|---|---|---|
| `refs.recall` | HARD | `[[refs]]` | every frozen site is among the occurrences of the full list |
| `refs.calls_recall` | HARD | `[[refs]]` | every frozen `call` site is an occurrence labelled `call` |
| `clash.defs_complete` | HARD | `[[clash]]` | every frozen definition is among `definitions.sites` |
| `clash.def_count` | HARD | `[[clash]]` | `definitions.count` equals the number of frozen definitions; an `at_least` count passes only as a valid lower bound (not above the frozen number, and within the files the parse budget left unlabelled) |
| `clash.calls_recall` | HARD | `[[clash]]` | every frozen call site of either definition is an occurrence labelled `call` (calls are not split by definition) |
| `lexical.recall_tracked` | HARD | `[[lexical]]` | every file of the text universe that holds the query is returned (no language allow-list) |
| `universe.accounted` | HARD | `[[refs]]` | `searched_files` plus each `left_out` reason count equal the oracle's counts for the text universe, reason by reason |
| `ident.def_first` | HARD | `[[ident]]` | `definitions.sites[0]` has the path and the line of the frozen `def` |
| `concept.first_right_top5` | HARD | `[[concept]]` with `first` | a frozen `first` file is among the first 5 files |
| `stakes.lines` | HARD | `[[stakes]]` with `expect = "lines"` | the matching lines equal the regex oracle's |
| `stakes.files` | HARD | `[[stakes]]` with `expect = "files"` | the matching files equal the regex oracle's |
| `stakes.counts` | HARD | `[[stakes]]` with `expect = "counts"` | the matching lines per file equal the regex oracle's |
| `stakes.exit` | HARD | `[[stakes]]` with `expect = "exit"` | the exit code equals the frozen one (`0` for a match, `1` for no match, `2` for a rejected query) |
| `refs.call_precision` | RATCHET | `[[refs]]` | of the occurrences labelled `call`, the share that are frozen calls (4 dp); no value when the output labels no `call` |
| `roles.precision.<lang>` | RATCHET | role oracle (`rskim-oracle`) | of the `def`, `call` and `import` labels that the full lists of the `[[refs]]` and `[[clash]]` queries put on files of the language, the share whose line the role oracle lists for the same role and name (4 dp, pooled over the corpora). A label on a file outside the oracle universe counts as not confirmed. A language with no label has no value: at this commit `tsx`, `javascript` and `go` have none, because the four corpora give no labelled site in them |
| `cost.files_parsed_median`, `cost.bytes_parsed_median`, `cost.bytes_read_median` | RATCHET | every call that states its cost | the median of each work counter (lower is better); no value while the output states no cost |
| `bytes.rgw_text_median`, `bytes.rgw_first_correct_median` | INFO | `[[ident]]`, `[[concept]]` | output bytes of the `rg -nw` simulation (the search an agent types for a bare identifier): in total, and through the definition line. Recorded in `info.reference` of `report.json`, never gated |

`results.unique_paths` also runs on these tables. A RATCHET row with no value is absent from the report values: it
fails nothing, and `bless` does not record it.

**The text universe.** One function, `text_universe` (`src/scoreboard/universe.rs`), gives the universe that
`lexical.recall`, `lexical.recall_tracked`, `universe.accounted` and the whole-word checks of golden integrity read. It
is every tracked text file of the work tree plus the untracked files that the ignore files allow, with no language
allow-list and no minified gate. Hidden and secret-bearing tracked files are in it: a plain-text search lists them, as
`git grep` does. A file leaves the universe only for a counted reason (`left_out`: `binary`, `not_utf8`, `too_large`,
`unreadable`, `vanished`, `outside_root`, `symlink`). The reason names equal the names in the search output.
`lexical.recall` and `lexical.recall_tracked` read the same data. Both stay under their names for the baseline and the
ledger.

**What the search output must state.** The adapter reads the `skim search --json` envelope. A file row gives its
`occurrences` with a `role` each, `definitions.sites` and `definitions.count`, the `searched_files` count and the
`left_out` counts, and the `parse` and `cost` counters. An unknown or missing role, a missing key or an `error`
envelope is a harness error (exit 2): a check whose data the output does not state never passes by default.

**Ledger.** A frozen-answer check that fails is a bug in the engine or in the golden data. Fix it. If you do not fix it
in this PR, file a ticket and ledger it (see [The ledger](#the-ledger-known_failurestoml)).

## Bumping a corpus pin

1. Set the new `commit` in `corpora.toml`: lowercase 40-hex, reachable from the repo's default branch.
2. Set the same `commit` in `golden/<corpus>.toml`. Re-verify every `def.line` against the new tree (integrity
   exits 2 on drift), and check that the concept regexes, zero-hit entries and pagination bounds still hold.
3. Run `check`. A local clone at the old pin fails reuse verification, and the harness deletes and re-clones it. It
   deletes only directories carrying its own marker. In CI, the corpus cache key hashes `corpora.toml`, so a new pin
   means a fresh clone.
4. Update the ledger ids for that corpus from the new `report.json` (file tickets for new failures first), then
   bless.

The skim corpus is bumped only by hand, together with a bless. It never tracks the live tree. The golden entries
of the skim corpus name files at its pin, so a path such as `crates/rskim/src/cmd/search/staleness.rs` can be a file
that the current work tree no longer holds.

## CI

Two jobs in `.github/workflows/ci.yml` run the gate: `changes` (**Detect Search Changes**) and `scoreboard`
(**Search Scoreboard**).

- **When it runs.** The scoreboard always runs on `workflow_dispatch` and on pushes to `main`. It never runs on pushes
  to `feature/*` or `wave/**`, because their pull-request run gates. On a pull request it runs only if the PR's own
  change set (`git diff --no-renames HEAD^1 HEAD` on the merge commit, so a file renamed out of a search path still
  counts as touching it) touches one of:
  - `crates/rskim/src/cmd/search/`, `crates/rskim-core/`, `crates/rskim-bench/`, `crates/rskim-oracle/`,
    `crates/rskim-research/`;
  - the `rskim` files outside `cmd/search/` that it depends on: `crates/rskim/src/cmd/mod.rs`
    (`is_repo_relative_safe`), `crates/rskim/src/cmd/execution.rs` (`StdoutStatus`, `pipe_closed_exit` and the stdout
    writers), `crates/rskim/src/cmd/dispatch.rs` (it dispatches `search`), `crates/rskim/src/debug.rs`
    (`is_debug_enabled`, `debug_log!`), `crates/rskim/src/output/mod.rs` (`elision_marker_with_remedy`),
    `crates/rskim/src/analytics/mod.rs` (`AnalyticsConfig`) with the `crates/rskim/src/analytics/schema.rs` and
    `crates/rskim/src/tokens.rs` it compiles in, `crates/rskim/src/main.rs` (dispatch) and `crates/rskim/Cargo.toml`;
  - the root `Cargo.toml` / `Cargo.lock`;
  - `.github/workflows/ci.yml`.

  Adding a `crate::` import to `cmd/search/` from another `rskim` file means adding that file here and to
  `SEARCH_PATHS` in the `changes` job. To find the files, run
  ``git grep -h -o -P 'crate::[a-z_]+(::[a-z_]+)*' -- crates/rskim/src/cmd/search`` and read each `use crate::{…}`
  group. Map each path `a::b` to `a/b.rs` or `a/b/mod.rs`. The frozen-answer tables and checks live in
  `crates/rskim-bench/` and `crates/rskim-oracle/`, which are already listed, so `SEARCH_PATHS` does not change for them.

  A PR outside these paths skips the job, and GitHub reports a skipped job as passing.
- **Binary.** Build Check uploads its release `skim` as the `skim-release` artifact (kept 1 day). The scoreboard
  downloads it and restores the exec bit. The scoreboard itself is built in the dev profile, where the root
  `Cargo.toml` optimises the `tree-sitter` runtime (`[profile.dev.package.tree-sitter] opt-level = 3`).
- **Fail closed.** If change detection fails, or Build Check fails on a run that touches a search path, the
  scoreboard job still runs, and its first step fails it. A failed build can never turn a search PR's gate into a
  green skip.
- **Caches.** The corpora are cached under a key on `corpora.toml`'s hash, and are saved only by a successful run.
  The scoreboard's cargo build has its own key prefix (`cargo-build-scoreboard-`).
- **Output.** `report.md` goes to the job's step summary. `report.json` + `report.md` are uploaded as the
  `scoreboard-report` artifact (kept 30 days).
- **Timeout:** 25 minutes.
