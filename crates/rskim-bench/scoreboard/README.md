# Search scoreboard

The scoreboard is the end-to-end quality gate for `skim search` retrieval (#203). It runs the **release
`skim` binary** as a subprocess, the same path an agent takes, against four pinned corpora. It checks every answer
against oracles that share no code with skim's search stack, and it compares ranking with naive baselines.

It is the **required merge gate for search PRs** (owner decision 2026-09-25, SEARCH-ADR-007 amendment). In CI it is the
`Search Scoreboard` job in `.github/workflows/ci.yml`.

- Code: `crates/rskim-bench/src/scoreboard/`. The binary is `src/bin/scoreboard.rs`, and the offline tests are
  `tests/scoreboard.rs` (stub skim, fixture corpus, no network).
- The structural oracle is its own crate, `crates/rskim-oracle/` (`src/structural.rs`), with its tree-sitter queries
  in `crates/rskim-oracle/queries/<pattern>.<lang>.scm`, one per (pattern, language), compiled into the scoreboard
  and hashed into the golden digest (see [Structural oracle](#structural-oracle)).
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
| **HARD** (per query) | `lexical.recall`, `lexical.precision` (skim's full list = oracle ground truth on the indexed universe) · `lexical.silent_fn` (a ground-truth file is missing and `degraded[]` is empty) · `lexical.verify_mode` · `pagination.complete` / `.disjoint` / `.ordered` / `.has_more_honest` · `order.prefix_consistent` · `order.score_monotone` · `results.unique_paths` (no path twice in one list: the full list, a `--limit` list, or one page) · `structural.recall`, `structural.precision`, `structural.coverage` on `[[ast]]` entries ([Structural oracle](#structural-oracle)) | Tolerance 0. A failure passes only if it is ledgered (XFAIL). |
| **RATCHET** (per corpus + aggregate) | `universe.delta`, `universe.skipped_by_reason_mismatch` · `coverage.tracked_text` · `ident.def_top1`, `ident.mrr`, `ident.anchor_eq_def`, `ident.def_line_in_snippet` · `concept.p5`, `concept.p10` · `bytes.text_median`, `bytes.text_p90`, `bytes.first_correct_median`, `bytes.first_correct_misses` · the baseline columns (`*.baseline_alpha`, `*.baseline_count`, `bytes.rg_*`) · `oracle_less.full_rows.<id>`, the full-list row count of each entry with no oracle (`--ast`, `--blast-radius`, a standalone `--hot` / `--cold` / `--risky` run); a shrink is a regression · the structural families `structural.precision.<id>`, `structural.line_on_match.<id>`, `structural.intent_recall.<id>` / `structural.intent_precision.<id>` and `structural.unscored_rows.<pattern>` (neutral: see [Checks](#checks)) | A change in **either direction** fails with "bless required". Tolerance is 0 after rounding to 4 dp, except the byte medians and p90s at ±3%. |
| **INFO** (never gated) | Latency: p50/p95 of the golden entries' own skim calls, plus the `--ast` pattern calls and the structural oracle pass, each timed apart · `unindexed_hits` · the oracle's per-reason skip breakdown · the "beats baseline" column | none |

A changed golden file (the structural-oracle fingerprint is folded into its hash), corpus pin, or HARD outcome (for
example `xfail -> pass`) also means "bless required". A HARD downgrade (`pass -> xfail`, or a blessed check, entry or
corpus that no longer runs) is blessed like a RATCHET regression: only with `--accept-regression "<reason>"`.

What it does **not** cover yet, where manual adversarial dog-food (SEARCH-ADR-007) is still required:

- The `--ast` patterns the structural oracle does not score, listed under "Uncovered structural patterns" in
  `report.md` (for a full run): `deep-nesting`, `java-synchronized` and `ruby-begin-rescue` have no oracle query,
  and `go-channel-send`, `go-defer`, `go-goroutine` and `go-select` have no corpus Go file where the oracle or skim
  finds a match (see [Uncovered patterns](#uncovered-patterns)).
- `--ast` rows no entry scores: rows in a language the oracle has no grammar for (Java, C, C++, C#, Ruby, SQL,
  Kotlin, Swift, Markdown), or in an oracle language with no entry for that pattern. They are counted in
  `structural.unscored_rows.<pattern>`, not judged.
- Containment `--ast` queries (`for_statement > block`) and compound ones (a text query with `--ast`, or `--ast`
  with `--hot` / `--cold` / `--risky` / `--blast-radius`). The structural oracle scores named patterns only, so at
  most the self-consistency checks of `[[prefix]]` and `[[pagination]]` entries cover these.
- Occurrence- and line-level structural precision. Scoring is per file: a file with one true match passes however
  many wrong rows skim returns in it. `structural.line_on_match.<id>` only counts the rows that land on a match line.
- Ratchet-class structural precision. A `precision = "ratchet"` entry records its precision as the RATCHET
  `structural.precision.<id>`, which is not required to be 1.
- The temporal arms (`--hot` / `--cold` / `--risky` / `--blast-radius`) against `git log`: #542.
- Any new query flag or arm, until it has golden entries here.

## Run it locally

Run from the workspace root; every default path is relative to it.

```bash
cargo build --release -p rskim                        # the binary under test
cargo run -p rskim-bench --bin scoreboard -- check --skim-bin target/release/skim
```

- The first run clones about 115 MB of full-history corpora into `.bench-corpus/scoreboard/` (gitignored), which
  takes about 30 s on a fast link. After that a warm `check` takes about 3.6 minutes on Apple Silicon (212–221 s
  measured). The CI `Search Scoreboard` job takes about 7 minutes.
- On macOS, run long checks under `caffeinate -i -s`. An unattended Mac can sleep mid-run: one run stretched to
  about 27 minutes, while the per-call timers (which stop during sleep) still looked normal.
- Reports go to `target/scoreboard/report.{json,md}` (change this with `--out`).
- Follow CLAUDE.md's resource rules: never run two release builds at once.

| Subcommand | What it does |
|---|---|
| `run` | Runs every corpus and writes `report.json` + `report.md`. It never gates: exit 0 unless a harness error occurs. |
| `check` | `run`, then gates against `baseline.json` and `known_failures.toml`. This is what CI runs. |
| `bless --from <report.json> [--accept-regression "<reason>"]` | Rewrites `baseline.json` from a report (see [Blessing](#blessing)). |
| `golden-gen --corpus <name> [--ast]` | Prints candidate `[[ident]]` entries for one corpus on stdout; with `--ast`, candidate `[[ast]]` entries (it runs the structural oracle and skim, so it also takes `--skim-bin`). Never run in CI. |

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
| `2` | Harness error: network or clone verification, golden integrity, an invalid data file, a skim crash, timeout or unparsable output, temporal data that skim reports unusable (`--stats` `temporal_state` not `ready`, or `degraded[]` on a `--hot` / `--cold` / `--risky` / `--blast-radius` entry), an empty full list for an entry with no oracle (it would pass every check vacuously), a vacuous `[[ast]]` entry (see [Vacuity guard](#vacuity-guard)) or a stale `expect_oracle_empty` flag (see [False-positive guards](#false-positive-guards)), a structural oracle failure (a parser that returns no tree, a query over its match limit), or a corpus changed by the run. A harness error is never reported as a regression, and no report is written. |

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
| Unledgered failure (**FAIL**) | fails | Fix it. If it is a real bug you are not fixing in this PR, file a ticket first, then add an `[[xfail]]` entry with that number and the exact ids, and re-bless with `--accept-regression "<reason>"`: a blessed `pass -> xfail` is a downgrade. |
| Ledgered check that passes (**XPASS**) | fails, "promote" | Your change fixed it. Remove the id (or the whole entry) from `known_failures.toml`, then re-bless: `xfail -> pass` is a baseline change. |
| RATCHET value moved | fails, "bless required" | Re-bless. Improvements need no reason. Regressions need `--accept-regression "<reason>"`, which is recorded in `accepted_regressions[]`. A neutral value (a baseline reference column, `structural.unscored_rows.<pattern>`) needs no reason in either direction. |

A `(check, id)` pair may appear only once. An id that is not in the golden set, or a check that never runs on that
entry, is a golden-integrity error (exit 2), so a stale entry cannot silently XFAIL nothing.

## Blessing

`bless` rewrites `baseline.json` from a `report.json`. It refuses (exit 1) in these cases:

- a partial (`--only`) report;
- a report made from other golden files, or another structural-oracle fingerprint, than the ones on disk;
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

## Structural oracle

`--ast` named patterns are scored by a structural oracle (#541), the `rskim-oracle` crate
(`crates/rskim-oracle/src/structural.rs`). For each (pattern, language) it runs a hand-written tree-sitter query,
`crates/rskim-oracle/queries/<pattern>.<lang>.scm`, over every file of that language in the oracle's universe. The
query encodes the pattern's catalog **description**, not skim's n-grams, on the real grammar: `.tsx` is parsed with
the TSX grammar, although skim parses it as TypeScript (SEARCH-ADR-007 (independent ground-truth oracle), #571).

The oracle crate never sees skim's pattern catalog: it knows its own registry by pattern name. The scoreboard reads
the catalog once (`skim_catalog` in `src/scoreboard/catalog.rs`, over `rskim_search::all_patterns()`), projects each
entry to its name, `exact` flag and example snippet, and passes that projection to everything that needs it;
`catalog_coverage` crosses the names with the oracle's registry. `exact` only seeds the precision class `golden-gen`
proposes, and the examples only feed the fixture tests. The extension-to-grammar table, the list of languages skim
AST-indexes and the 1 MiB inclusive size cap are the oracle's own copies, with citations, and a unit test
(`oracle::tests::langs_agree_with_the_structural_oracles_extension_table`) keeps the extension table in step with the
lexical oracle's copy (`.tsx` excepted).

Independence holds at two strengths:

- **`rskim-oracle`, at compile time.** It depends on no `rskim-*` crate. `crates/rskim-oracle/tests/independence.rs`
  fails on any dependency, in any dependency table, that is not on its allow-list (a `rskim-*` crate however it is
  named, and any `path` or `git` source), on a build script, and on an `include!` or a `#[path]` that leaves the
  crate.
- **The scoring layer, by convention.** `src/scoreboard/structural_metrics.rs` is in `rskim-bench`, which depends on
  `rskim-search` and `rskim-core`, so only review keeps skim's search code out of it, as for the lexical oracle
  (`oracle.rs`, `universe.rs`). It imports `rskim_oracle` and the catalog projection only.

### `[[ast]]` golden entries

```toml
[[ast]]
id = "skim-ast-try-catch-finally-javascript"   # <corpus>-ast-<pattern>-<lang>
pattern = "try-catch-finally"                  # a catalog name the oracle has a query for in `lang`
lang = "javascript"                            # rust | python | typescript | tsx | javascript | go
precision = "hard"                             # hard | ratchet
expect_oracle_empty = true                     # optional, default false: a false-positive guard
```

- There is one entry per (pattern, language) where the corpus has files in that language and the oracle or skim
  finds at least one of them.
- `expect_oracle_empty = true` marks a **false-positive guard**: the oracle matches no file in `lang`, and the entry
  exists because skim returns one it should not. It guards precision only (see
  [False-positive guards](#false-positive-guards)).
- `precision` is declared here and never re-read from the catalog. `golden-gen` proposes `hard` iff the catalog
  marks the pattern `exact`. Reclassifying an entry is a reviewed golden edit plus a bless.
- skim is called once per (corpus, pattern): `skim search --root <clone> --json --limit 1000000 --ast <pattern>`.
  On a corpus with at least one `[[ast]]` entry it is called for **every** catalog pattern, including patterns with
  no entry in that corpus and the uncovered ones. The rows are split by extension into languages, so each entry sees
  only its own language's rows, and every row no entry scores is counted in `structural.unscored_rows.<pattern>`.
- Integrity errors (exit 2): an unknown `lang` or `precision`, a pattern that is not in the catalog, an uncovered
  pattern, a language the pattern has no query for, two entries for one (pattern, language), a vacuous entry
  ([Vacuity guard](#vacuity-guard)), or an `expect_oracle_empty` entry whose oracle matches a file. A
  `structural.*` ledger entry may name only `[[ast]]` ids, and `structural.precision` only `hard` ones.

To propose entries, build the release skim and run `golden-gen --ast`:

```bash
cargo build --release -p rskim
cargo run -p rskim-bench --bin scoreboard -- golden-gen --corpus zod --ast --skim-bin target/release/skim
```

It calls skim for every catalog pattern, as the gate does. It prints one `[[ast]]` entry per candidate, each under a
`# golden-gen: oracle files N; skim files M` comment, and adds `expect_oracle_empty = true` to a candidate with
`oracle files 0`. A comment above the entries lists, per pattern, the skim rows no proposed entry would score (the
gate's `structural.unscored_rows.<pattern>`), with a sample of `path:line` rows. The paths are skim's output, so they
are escaped (a control character, a Unicode bidirectional control or a backslash is written as its Rust escape) and
cannot end the comment line. Review the entries, append them to `golden/<corpus>.toml`, then run `check` and bless.
Ids are stable across regeneration. New entries bless without
`--accept-regression`: their checks go `new -> pass` or `new -> xfail`, neither of which is a downgrade, and the
`structural.unscored_rows.<pattern>` counts they absorb move neutrally.

### Checks

| Check | Class | Runs on | Passes iff / measures |
|---|---|---|---|
| `structural.recall` | HARD | every `[[ast]]` entry | every file the oracle matches in the entry's language is in skim's rows |
| `structural.precision` | HARD | `precision = "hard"` entries | every file skim returns in the entry's language is an oracle match |
| `structural.coverage` | HARD | every `[[ast]]` entry | skim's `ast_coverage.size_excluded_files` equals the oracle's own count of files over 1 MiB among the files skim's size-cap accounting counts (every tree-sitter language, Bash included; not JSON / YAML / TOML or an unknown extension), and `undetermined_files` is 0 (an absent `ast_coverage` reads as all zero) |
| `results.unique_paths` | HARD | every `[[ast]]` entry | no path twice in the entry's rows |
| `structural.precision.<id>` | RATCHET | `precision = "ratchet"` entries | file-level precision (4 dp) |
| `structural.line_on_match.<id>` | RATCHET | every `[[ast]]` entry | how many skim rows have a `line` that is the first line of an oracle match in that file |
| `structural.intent_recall.<id>`, `structural.intent_precision.<id>` | RATCHET | `nested-loop` (typescript, tsx, javascript), `rust-nested-loop` (rust) | recall and precision against the intent oracle: a loop with a loop ancestor inside the same function |
| `structural.unscored_rows.<pattern>` | RATCHET (neutral) | per corpus with an `[[ast]]` entry, every catalog pattern (0 included) | skim rows no entry scores: a language the oracle has no grammar for (Java, C, C++, Markdown, …), an oracle language with no entry for that pattern, or a pattern with no entry at all |

- `line_on_match` reads low by construction for multi-line constructs. skim anchors a row on the child node that
  completes the pattern's edge (`catch_clause`, `class_body`, …), while the oracle anchors on the construct the
  description names. It is a count, so it is defined even when there is no true positive.
- `unscored_rows` keeps rows no entry judges visible instead of dropping them: every row of every catalog pattern
  lands in an entry or in this count. It is emitted for every called pattern, 0 included, so a pattern's first
  unscored row is a visible RATCHET move. It is **neutral**: no oracle judges these rows, and a move either way has
  an ordinary cause (a new entry now scores them, or skim returns rows in a language no entry scores). So `check`
  fails with "bless required", and `bless` needs no `--accept-regression`.
- `report.json` carries, under `corpora[i].structural`, each entry's `oracle_files`, `skim_files`, `recall`,
  `precision`, the intent fields where they apply and `line_on_match`, plus the coverage comparison. A
  [false-positive guard](#false-positive-guards) also carries `"expect_oracle_empty": true`; the key is absent on
  every other entry. The top level carries `uncovered_patterns`. `report.md` has a "Structural (`--ast`)" table per
  corpus, where a guard's class reads `<class>, FP guard` with a one-line legend under the table, and the uncovered
  list.
- `[[prefix]]` and `[[pagination]]` entries with an `--ast` flag still check ordering, pagination and their row count.
- `report.json`'s `latency` (INFO) times the structural work apart from the golden entries: `pattern_calls_wall_ms`
  per `--ast` pattern call and `structural_oracle_wall_ms` for the oracle pass. `calls`, the percentiles and
  `entries_wall_ms` cover the golden entries' own calls only, and `report.md`'s latency table has a column for each.

### Vacuity guard

An `[[ast]]` entry where the oracle matches no file in its language AND skim returns no row there would pass every
check without testing anything. It is a golden error, exit 2 (`golden integrity failed (N problem(s)):`, then one
``<id>: vacuous [[ast]] entry: neither the structural oracle nor skim finds <pattern> in a <lang> file, …`` line per
entry, then the remediation once), and `golden-gen --ast` never proposes one. It can appear later, for example when
a pin bump empties an entry.
Remove the entry together with its ledger ids and bless with `--accept-regression`, because a blessed check that no
longer runs is a HARD downgrade. A false-positive guard is exempt while the corpus has a scored file in its language
(below).

### False-positive guards

An entry whose oracle is empty exists only because skim returns a file the oracle rejects: a false positive, such as
`skim-ast-try-catch-finally-javascript` (#546). It declares `expect_oracle_empty = true`, and then:

- It is scored even when skim returns no row either. That is the state a fix leaves, not a vacuous entry. With no
  oracle file and no skim row, `recall` and `precision` both read 1 (an empty denominator reads as 1) and every
  HARD check passes. If the false positive comes back, `structural.precision` fails again, so the entry keeps
  guarding after the fix.
- Its oracle must stay empty. If the oracle matches a file (after a pin bump or a query edit), the flag is stale and
  the run stops with exit 2 before skim is called (`golden integrity failed (N problem(s)):`, then
  ``<id>: declares `expect_oracle_empty = true` but the structural oracle matches N file(s), e.g. <path>, …``, then
  the remediation). Remove the flag in a reviewed golden edit, then bless. The flag can never hide a recall loss.
- The report says it is a guard (`"expect_oracle_empty": true` in `report.json`, `FP guard` in its `report.md`
  class cell), so a fixed guard's oracle 0 / skim 0 does not read as an ordinary entry.
- Its language must have scored files. A guard in a language where the corpus has no file the oracle scores (none at
  all, or every one over the 1 MiB cap) has nowhere for a false positive to land, so it would pass forever while
  measuring nothing. It is vacuous: exit 2, with the line
  `<id>: vacuous false-positive guard: no scored <lang> file, so a false positive has nowhere to land`.
- Unflagged entries keep the vacuity guard.

When the fix lands (for #546, `skim-ast-try-catch-finally-javascript`), the ledgered `structural.precision` XPASSes.
Remove its ledger entry (keep the golden entry) and re-bless. `xfail -> pass` needs no `--accept-regression`.

### Adding or editing a query

1. Write or edit `crates/rskim-oracle/queries/<pattern>.<lang>.scm`. Line 1 is a title comment. Line 2 must be
   `; Grammar: <crate> <locked version>.`, with `(LANGUAGE_TYPESCRIPT)` or `(LANGUAGE_TSX)` before the full stop for
   `tree-sitter-typescript`, and no other line may start `; Grammar:`. A unit test
   (`every_query_names_the_grammar_version_cargo_lock_resolves`) checks the version against the one the workspace
   `Cargo.lock` resolves, so a grammar bump fails it until the headers are edited. Then quote the catalog
   description, and capture exactly one `@match` node: its first line is the match line.
2. Register a new file in `QUERIES` in `crates/rskim-oracle/src/structural.rs`, sorted by pattern and then language
   (unit tests check the order, and that every `.scm` file on disk is registered exactly once). Add a `PostFilter`
   only when the description states a count (`empty-*`: zero body elements; `god-function`: at least 20;
   `excessive-params`: at least 5).
3. Add a fixture in `crates/rskim-bench/src/scoreboard/catalog_tests.rs`: the catalog example (or a hand-written
   positive) must match on the expected lines, and a near-miss must not. The tests fail on a `.scm` file that is not
   registered, a registered query with no fixture, or a query that does not compile for its grammar.
4. Run `check` and read the structural diff, then bless. The query text, the post-filters, the intent specs, the
   size cap, the extension table (every row's class, and the class of an unknown extension) and the attribute kinds
   the body-element count skips are hashed into the digest of every golden file with `[[ast]]` entries
   (`rskim_oracle::structural::fingerprint`, the `Display` of its `OracleInputs`). Any edit therefore reads as
   "golden file or structural-oracle fingerprint changed; bless required" on each of those corpora, and `bless`
   refuses a report made with other oracle inputs.

Two constants are left out of the fingerprint on purpose, because neither can change an answer. The capture name
every query marks its match with (`MATCH_CAPTURE`, `match`): a query without it fails to compile into the oracle,
and renaming it means editing every query, which moves the digest anyway. The match limit (`ORACLE_MATCH_LIMIT`,
4096 in-progress matches): a query that needs more is a harness error naming the query, never a partial answer. The
grammar versions are not hashed either: the `; Grammar:` headers carry them, so a grammar bump fails the header test
until the headers are edited, and that edit moves the digest.

A catalog pattern with neither a query nor a recorded reason fails a unit test, so a pattern added to skim cannot go
unscored silently.

### Adding an oracle language

1. Add the grammar crate to the root `[workspace.dependencies]`, to `crates/rskim-oracle/Cargo.toml` and to
   `ALLOWED` in `crates/rskim-oracle/tests/independence.rs`.
2. Add an `OracleLang` variant (`ALL`, `as_str`, `grammar`) and move its extensions in `EXT_CLASSES` from
   `LangClass::Unscored` to `LangClass::Oracle`. The lexical oracle's `LANGS` must still list the same extensions
   under the same names (`langs_agree_with_the_structural_oracles_extension_table`).
3. Add the language's arm to `grammar_crate` in `crates/rskim-oracle/src/structural_tests.rs`, for the grammar
   header test.
4. Write its queries as above, propose entries with `golden-gen --ast`, then run `check` and bless. The
   extension-table edit alone moves the fingerprint of every corpus with `[[ast]]` entries.

### Uncovered patterns

These stay under manual adversarial dog-food (SEARCH-ADR-007). `report.md` lists them under "Uncovered structural
patterns" (for a full run: the list is computed over the corpora the run covers, so an `--only` run also lists the
patterns whose entries are in other corpora):

| Pattern | Why |
|---|---|
| `deep-nesting` | No oracle: "depth >= 4" does not say where depth is measured from or which nodes count. |
| `java-synchronized` | No oracle: the construct exists only in the Java grammar. |
| `ruby-begin-rescue` | No oracle: the construct exists only in the Ruby grammar. |
| `go-channel-send`, `go-defer`, `go-goroutine`, `go-select` | The oracle has a Go query, but no corpus has a Go file where the oracle or skim finds a match, so there is no entry. |

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
  change set (`git diff --no-renames HEAD^1 HEAD` on the merge commit, so a file renamed out of a search path still
  counts as touching it) touches one of:
  - `crates/rskim-search/`, `crates/rskim/src/cmd/search/`, `crates/rskim-core/`, `crates/rskim-bench/`,
    `crates/rskim-oracle/`, `crates/rskim-research/`;
  - the `rskim` files outside `cmd/search/` that it depends on: `crates/rskim/src/cmd/mod.rs`
    (`is_repo_relative_safe`, `resolve_cache_dir`), `crates/rskim/src/debug.rs` (`is_debug_enabled`),
    `crates/rskim/src/analytics/mod.rs` (`AnalyticsConfig`) with the `crates/rskim/src/analytics/schema.rs` and
    `crates/rskim/src/tokens.rs` it compiles in, `crates/rskim/src/main.rs` (dispatch) and `crates/rskim/Cargo.toml`;
  - the root `Cargo.toml` / `Cargo.lock`;
  - `.github/workflows/ci.yml`.

  Adding a `crate::` import to `cmd/search/` from another `rskim` file means adding that file here and to
  `SEARCH_PATHS` in the `changes` job.

  A PR outside these paths skips the job, and GitHub reports a skipped job as passing.
- **Binary.** Build Check uploads its release `skim` as the `skim-release` artifact (kept 1 day). The scoreboard
  downloads it and restores the exec bit. The scoreboard itself is built in the dev profile, where the root
  `Cargo.toml` optimises the `tree-sitter` runtime (`[profile.dev.package.tree-sitter] opt-level = 3`): the
  structural oracle pass runs about 5x faster for a few seconds of extra compile.
- **Fail closed.** If change detection fails, or Build Check fails on a run that touches a search path, the
  scoreboard job still runs, and its first step fails it. A failed build can never turn a search PR's gate into a
  green skip.
- **Caches.** The corpora are cached under a key on `corpora.toml`'s hash, and are saved only by a successful run.
  The scoreboard's cargo build has its own key prefix (`cargo-build-scoreboard-`).
- **Output.** `report.md` goes to the job's step summary. `report.json` + `report.md` are uploaded as the
  `scoreboard-report` artifact (kept 30 days).
- **Timeout:** 25 minutes. A run takes about 7 minutes.
