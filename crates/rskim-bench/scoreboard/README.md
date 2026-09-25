# Search scoreboard

The scoreboard is the end-to-end quality gate for `skim search` retrieval (#203). It runs the **release
`skim` binary** as a subprocess, the same path an agent takes, against four pinned corpora. It checks every answer
against oracles that share no code with skim's search stack, and it compares ranking with naive baselines.

It is the **required merge gate for search PRs** (owner decision 2026-09-25, ADR-007 amendment). In CI it is the
`Search Scoreboard` job in `.github/workflows/ci.yml`.

- Code: `crates/rskim-bench/src/scoreboard/`. The binary is `src/bin/scoreboard.rs`, and the offline tests are
  `tests/scoreboard.rs` (stub skim, fixture corpus, no network).
- Data: this directory.

| File | Holds |
|---|---|
| `corpora.toml` | The four corpora (skim, ripgrep, flask, zod): URL, a 40-hex commit pin, and language |
| `golden/<corpus>.toml` | The frozen golden queries for one corpus (`commit` must equal its pin) |
| `known_failures.toml` | The ledger: known HARD failures, each tied to a filed ticket |
| `baseline.json` | The blessed state: every HARD outcome and RATCHET value, pins and golden hashes. Written only by `bless` |

## What it measures

| Class | Checks | Rule |
|---|---|---|
| **HARD** (per query) | `lexical.recall`, `lexical.precision` (skim's full list = oracle ground truth on the indexed universe) · `lexical.silent_fn` (a ground-truth file is missing and `degraded[]` is empty) · `lexical.verify_mode` · `pagination.complete` / `.disjoint` / `.ordered` / `.has_more_honest` · `order.prefix_consistent` · `order.score_monotone` | Tolerance 0. A failure passes only if it is ledgered (XFAIL). |
| **RATCHET** (per corpus + aggregate) | `universe.delta`, `universe.skipped_by_reason_mismatch` · `coverage.tracked_text` · `ident.def_top1`, `ident.mrr`, `ident.anchor_eq_def`, `ident.def_line_in_snippet` · `concept.p5`, `concept.p10` · `bytes.text_median`, `bytes.text_p90`, `bytes.first_correct_median`, `bytes.first_correct_misses` · the baseline columns (`*.baseline_alpha`, `*.baseline_count`, `bytes.rg_*`) | A change in **either direction** fails with "bless required". Tolerance is 0 after rounding to 4 dp, except the byte medians and p90s at ±3%. |
| **INFO** (never gated) | Latency p50/p95 · `unindexed_hits` · the oracle's per-reason skip breakdown · the "beats baseline" column | none |

A changed golden file, corpus pin, or HARD outcome (for example `xfail -> pass`) also means "bless required".

What it does **not** cover yet, where manual adversarial dog-food (ADR-007) is still required:

- AST structural precision and recall (`--ast` patterns): #541. `--ast` entries are checked only for ordering and
  pagination.
- The temporal arms (`--hot` / `--cold` / `--risky` / `--blast-radius`) against `git log`: #542.
- Any new query flag or arm, until it has golden entries here.

## Run it locally

Run from the workspace root; every default path is relative to it.

```bash
cargo build --release -p rskim                        # the binary under test
cargo run -p rskim-bench --bin scoreboard -- check --skim-bin target/release/skim
```

- The first run clones about 115 MB of full-history corpora into `.bench-corpus/scoreboard/` (gitignored), which
  takes about 30 s on a fast link. After that a `check` takes about 3 minutes on Apple Silicon.
- On macOS, run long checks under `caffeinate -i -s`. An unattended Mac can sleep mid-run: one run stretched to
  about 27 minutes, while the per-call timers (which stop during sleep) still looked normal.
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
| `2` | Harness error: network or clone verification, golden integrity, an invalid data file, a skim crash, timeout or unparsable output, or a corpus changed by the run. A harness error is never reported as a regression, and no report is written. |

On a gate failure, stderr prints one `FAIL <check> [<ids>]: <message>` line per failure, and `report.md` lists them
under "Gate failures".

## The ledger (`known_failures.toml`)

```toml
[[xfail]]
issue = "#544"                          # a FILED ticket; placeholders such as #NEW are rejected
check = "pagination.has_more_honest"    # a HARD check's dotted name
ids = ["skim-G001", "skim-G002"]        # the exact failing ids, copied from report.json
note = "query.rs:692 pool_was_capped"   # optional
```

| Outcome | Gate | What to do |
|---|---|---|
| Ledgered failure (**XFAIL**) | passes | Nothing. The ticket tracks it. |
| Unledgered failure (**FAIL**) | fails | Fix it. If it is a real bug you are not fixing in this PR, file a ticket first, then add an `[[xfail]]` entry with that number and the exact ids. |
| Ledgered check that passes (**XPASS**) | fails, "promote" | Your change fixed it. Remove the id (or the whole entry) from `known_failures.toml`, then re-bless: `xfail -> pass` is a baseline change. |
| RATCHET value moved | fails, "bless required" | Re-bless. Improvements need no reason. Regressions need `--accept-regression "<reason>"`, which is recorded in `accepted_regressions[]`. |

A `(check, id)` pair may appear only once. An id that is not in the golden set, or a check that never runs on that
entry, is a golden-integrity error (exit 2), so a stale entry cannot silently XFAIL nothing.

## Blessing

`bless` rewrites `baseline.json` from a `report.json`. It refuses (exit 1) in these cases:

- a partial (`--only`) report;
- a report whose golden files differ from the ones on disk;
- any `fail` or `xpass` record (fix the failure or ledger it first);
- a RATCHET regression without `--accept-regression`.

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

Golden data is declared **before** looking at skim's output (ADR-003), and CI never regenerates it. Each entry needs
an `id` that is unique and prefixed with `<corpus>-`. The existing id scheme:

- `L..` curated identifiers, `I..` generated identifiers;
- `C..` concepts;
- `S..` / `X..` / `K..` / `Z..` lexical cases, plus the seed's two-digit `G01` / `G02` `--lang` cases;
- `P..` phrase / near;
- `G0xx` pagination, `F0xx` prefix.

```toml
[[ident]]      # definition ranking: def.line must contain `query` at the pinned commit
id = "skim-L10"
query = "resort_window"
def = { path = "crates/rskim/src/cmd/search/temporal.rs", line = 462 }
origin = "curated"            # seed | generated | curated

[[concept]]    # ranking: relevance is a regex over file text, written from the corpus source
id = "skim-C08"
query = "hook watchdog"
relevant = '(?i)hook[_\s-]*watchdog'

[[lexical]]    # recall/precision edge case
id = "skim-X09"
query = "-D warnings"
mode = "and"                  # and | phrase | near | pnear (near = N required for near/pnear)
category = "punct"            # substr | short | punct | case | lang | zero-hit | phrase | near
# lang = "toml"               # optional: passed as --lang, applied by the oracle's own extension map

[[pagination]] # --offset sweep per limit; full count must be <= min(limits) x 63
id = "skim-G009"
query = "elision marker"
flags = []                    # --phrase, --near N, --lang X, --hot|--cold|--risky, --ast P, --blast-radius F
limits = [3, 7, 20]

[[prefix]]     # --limit N must equal the first N rows of the full list
id = "skim-F005"
query = "fn"                  # omit for a standalone --ast / --hot run
flags = ["--hot"]
limits = [5, 20]
```

(The values above are illustrative; check each one against the pinned clone before you commit it.)

Golden integrity runs before any query, and a violation is exit 2, never a gate failure. It checks:

- the pin matches `corpora.toml`;
- each `def.line` contains the query at the pinned commit;
- regexes compile, and `near` is present exactly when the mode needs it;
- ids are unique, `zero-hit` entries have no ground truth, and pagination fits its bound;
- every ledger entry refers to a real entry and check.

For identifiers, `scoreboard golden-gen --corpus <name>` proposes candidates: definitions with exactly one site in
the corpus, a name of at least 6 bytes, and 2–60 ground-truth files, ordered by `sha256("<corpus>:<name>")`. Review
the proposal, then paste it.

Adding any entry changes the golden hash, so the next `check` says "bless required". Bless from that run.

## Bumping a corpus pin

1. Set the new `commit` in `corpora.toml`: lowercase 40-hex, reachable from the repo's default branch.
2. Set the same `commit` in `golden/<corpus>.toml`. Re-verify every `def.line` against the new tree (integrity
   exits 2 on drift), and check that the concept regexes, zero-hit entries and pagination bounds still hold.
3. Run `check`. A local clone at the old pin fails reuse verification, and the harness deletes and re-clones it. It
   deletes only directories carrying its own marker. In CI, the corpus cache key hashes `corpora.toml`, so a new pin
   means a fresh clone.
4. Update the ledger ids for that corpus from the new `report.json` (file tickets for new failures first), then
   bless.

The skim corpus is bumped only by hand, together with a bless. It never tracks the live tree.

## CI

Two jobs in `.github/workflows/ci.yml` run the gate: `changes` (**Detect Search Changes**) and `scoreboard`
(**Search Scoreboard**).

- **When it runs.** The scoreboard always runs on `workflow_dispatch` and on pushes to `main`. It never runs on pushes
  to `feature/*` or `wave/**`, because their pull-request run gates. On a pull request it runs only if the PR's own
  change set (`git diff HEAD^1 HEAD` on the merge commit) touches one of:
  - `crates/rskim-search/`, `crates/rskim/src/cmd/search/`, `crates/rskim-core/`, `crates/rskim-bench/`,
    `crates/rskim-research/`;
  - the root `Cargo.toml` / `Cargo.lock`;
  - `.github/workflows/ci.yml`.

  A PR outside these paths skips the job, and GitHub reports a skipped job as passing.
- **Binary.** Build Check uploads its release `skim` as the `skim-release` artifact (kept 1 day). The scoreboard
  downloads it and restores the exec bit.
- **Fail closed.** If Build Check or change detection fails, the scoreboard job still runs, and its first step fails
  it. A failed build can never turn into a green skip.
- **Caches.** The corpora are cached under a key on `corpora.toml`'s hash, and are saved only by a successful run.
  The scoreboard's cargo build has its own key prefix (`cargo-build-scoreboard-`).
- **Output.** `report.md` goes to the job's step summary. `report.json` + `report.md` are uploaded as the
  `scoreboard-report` artifact (kept 30 days).
- **Timeout:** 25 minutes.
