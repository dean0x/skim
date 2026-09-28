---
feature: search-scoreboard
name: Search scoreboard (quality gate) incl. the structural oracle
description: "Use when a search PR's Search Scoreboard CI job fails, when changing skim search retrieval/ranking/pagination/walker universe/text output or the --ast pattern catalog / AST indexing / AST size cap, when running the scoreboard locally, when ledgering or promoting a known HARD failure, when blessing baseline.json (incl. --accept-regression), when adding golden queries or [[ast]] entries, bumping a corpus pin or a tree-sitter grammar, when adding a structural-oracle query or oracle language in crates/rskim-oracle, or when editing the scoreboard harness (lexical oracle, structural oracle, catalog read, universe, runner, gate, bless), the CI search-path filter, or rskim-research pinned-clone / subprocess-timeout code. Keywords: scoreboard, Search Scoreboard, run, check, bless, golden-gen, golden-gen --ast, [[ast]], structural oracle, rskim-oracle, AC-3, tests/independence.rs, ALLOWED, include!, #[path], build.rs, queries/*.scm, ; Grammar: header, Cargo.lock, OracleInputs, fingerprint, structural_oracle_sha256, golden_digest, MATCH_CAPTURE, ORACLE_MATCH_LIMIT, OracleScratch, map_init, intent oracle, nested_loop_lines, PostFilter, EXT_CLASSES, AST_SIZE_CAP_BYTES, UNCOVERED, UNCLASSIFIED_REASON, catalog.rs, skim_catalog, catalog_coverage, all_patterns, LANGS, structural.recall, structural.precision, structural.coverage, structural.unscored_rows, neutral, line_on_match, intent_recall, expect_oracle_empty, false-positive guard, vacuous, uncovered_patterns, structural_oracle_wall_ms, pattern_calls_wall_ms, profile.dev.package.tree-sitter, eol=lf, baseline.json, known_failures.toml, ledger, XFAIL, XPASS, promote, HARD, RATCHET, INFO, bless required, --accept-regression, accepted_regressions, corpora.toml, universe.delta, skipped_by_reason_mismatch, coverage.tracked_text, oracle_less.full_rows, results.unique_paths, silent_fn, degraded, temporal_state, harness error, exit 2, SEARCH_PATHS, --no-renames, skim-release, scoreboard-report, ensure_pinned_history_clone, OWNERSHIP_MARKER, zeroPaddedFilemode, process_group, git_output_with_timeout, KILL_GRACE, caffeinate, .bench-corpus/scoreboard, #541, #542, #544, #545, #546, #547, #571, #572, SEARCH-ADR-007."
category: domain-knowledge
directories: [crates/rskim-bench/src/scoreboard/, crates/rskim-bench/src/bin/scoreboard.rs, crates/rskim-bench/scoreboard/, crates/rskim-bench/tests/scoreboard.rs, crates/rskim-oracle/, crates/rskim-research/src/clone.rs, .github/workflows/ci.yml]
referencedFiles:
  - crates/rskim-bench/src/scoreboard/pipeline.rs
  - crates/rskim-bench/src/scoreboard/runner.rs
  - crates/rskim-bench/src/scoreboard/metrics.rs
  - crates/rskim-bench/src/scoreboard/gate.rs
  - crates/rskim-bench/src/scoreboard/baseline.rs
  - crates/rskim-bench/src/scoreboard/golden.rs
  - crates/rskim-bench/src/scoreboard/golden_gen.rs
  - crates/rskim-bench/src/scoreboard/catalog.rs
  - crates/rskim-bench/src/scoreboard/structural_metrics.rs
  - crates/rskim-oracle/src/structural.rs
  - crates/rskim-oracle/tests/independence.rs
  - crates/rskim-bench/scoreboard/known_failures.toml
  - crates/rskim-research/src/clone.rs
  - .github/workflows/ci.yml
created: 2026-09-25
updated: 2026-09-28
---

# Search scoreboard (quality gate) incl. the structural oracle

## Overview

The scoreboard (#203, PR #560; structural oracle #541, PR #574) is the **required merge gate for search PRs**, the
`Search Scoreboard` job in `.github/workflows/ci.yml`. It runs the **release `skim` binary as a subprocess** against
four pinned full-history corpora (skim `b8a0a79`, never the live tree; ripgrep, flask, zod), checks every answer
against oracles that share no code with skim's search stack, and compares ranking with naive baselines. It replaced
the manual SEARCH-ADR-007 dog-food campaign as the standing gate; manual dog-food remains only where it is blind:

- `--ast` patterns with no structural score (`uncovered_patterns`, full run): `deep-nesting`, `java-synchronized`,
  `ruby-begin-rescue` have no query; the four `go-*` patterns have one but no corpus Go file where the oracle or
  skim finds a match, so no entry;
- `--ast` rows no entry scores (a language with no oracle grammar, or an oracle language with no entry for that
  pattern): counted in `structural.unscored_rows.<pattern>`, never judged;
- containment (`a > b`) and compound `--ast` queries: only `[[prefix]]` / `[[pagination]]` self-consistency checks;
- occurrence- and line-level structural precision (scoring is per file), and ratchet-class structural precision;
- the temporal arms against `git log` (#542), and any new query flag or arm without golden entries.

The runbook is `crates/rskim-bench/scoreboard/README.md`; this file covers what it does not spell out: how the pieces
couple across the two crates, which invariant is enforced where, and the traps found in #203 and the #574 review.

## Business Context

- **Why it exists.** Dog-food rounds give only negative evidence, never "done". The old reader-API harness scored a
  different universe and bypassed the verify gate, anchors and pagination; this measures what the shipped CLI returns.
- **What "passing" means.** Every HARD outcome is PASS or ledgered XFAIL and nothing differs from the blessed
  `baseline.json` (RATCHET values, HARD states, pins, golden digests, corpus set). It does **not** mean skim beats the
  baselines ("beats baseline" is INFO; the baseline has skim *losing* aggregate `bytes.text_median` 2177 vs simulated
  rg 1573 and `concept.p5` 0.8538 vs occurrence-count 0.9).
- **Ratchets, not targets** (applies SEARCH-ADR-003): RATCHET values compare with blessed *measured* values, never
  the "bar" column. Golden data is declared before anyone looks at skim's output and never regenerated in CI.
- **Green is not merge authorization** (SEARCH-ADR-005), except its 2026-09-26 amendment: #540 phase PRs merge once
  reviewed, comments resolved and CI (Search Scoreboard included) green, and only within that scope.

## Core Business Rules

### Three check classes

| Class | Scope | Tolerance | Where defined |
|---|---|---|---|
| HARD | golden entry × check (`CheckId::ALL`, 14 checks) | 0; only a ledger entry excuses a failure | `types.rs` `CheckId`, `metrics.rs` `PlannedQuery::runs` |
| RATCHET | per corpus + aggregate | exact after `round4`; `bytes.text_*` / `bytes.*first_correct_median` / `bytes.rg_*` ±3% **relative to the baseline value** | `metrics.rs` `RATCHET_METRICS`, `ORACLE_LESS_ROWS_DEF`, `STRUCTURAL_FAMILIES`, `compare_ratchet` |
| INFO | latency, `unindexed_hits`, per-reason skip breakdown, beats-baseline | never gated | `report.rs` |

Which HARD checks run on an entry is decided in one place, `PlannedQuery::runs`:

- `lexical.*` recall / precision / `silent_fn` need the lexical oracle (none for `--ast`, `--blast-radius` or a
  standalone temporal run); `lexical.verify_mode` needs a text query.
- `structural.recall` / `.coverage` run on every `[[ast]]` entry, `structural.precision` only on `precision = "hard"`
  ones (a `ratchet` entry records `structural.precision.<id>` instead).
- `pagination.*` only on `[[pagination]]`, `order.prefix_consistent` only on `[[prefix]]` entries.
- `order.score_monotone` is skipped on `[[ast]]` entries (standalone `--ast` is path-ordered, #547) and when a
  temporal sort or `--blast-radius` overrides the rank. `results.unique_paths` runs on every entry, within each list.

Pagination and prefix checks compare skim with **its own full list** (`--limit 1_000_000`), so oracle-less entries
can be pagination entries. `lexical.silent_fn` fails only when a ground-truth file is missing **and** `degraded[]`
is empty; a disclosed miss counts against recall only.

### Ledger (`known_failures.toml`)

- Each `[[xfail]]` names `issue = "#<digits>"` (a filed ticket; `is_issue_ref` rejects placeholders and leading
  zeros), one dotted `check` and the exact failing `ids` from `report.json`. A `(check, id)` pair appears once.
- Ids map to a corpus by the **longest** `<corpus>-` prefix (`gate::corpus_of`). An id in no corpus, an id missing
  from golden, or a check that never runs on that entry (`order.score_monotone` on `--hot`, `structural.precision` on
  a `ratchet` entry) is exit 2 (`golden::check_ledger` via `CheckId::applies_to`, `gate::unplanned_ledger_refs`).

| Ticket | Ledgered check: ids | skim root cause | What fixing it requires |
|---|---|---|---|
| #544 | `pagination.complete` / `.ordered` / `.has_more_honest` on multi-word G-ids | candidate pool cut before verification (`cmd/search/query.rs`) | offset/limit applied to verified rows only; promote |
| #545 | pagination checks on `skim-G007`, `zod-G006`; `order.prefix_consistent` F-ids | `--hot` re-sorts only a 100-row `resort_window` (`temporal.rs`) | full-list temporal re-sort; promote |
| #547 | `order.score_monotone` on the four `<corpus>-F003` | `search_ast` returns FileId (path) order (`ast.rs`) | sort standalone `--ast` by score; promote |
| #546 | `structural.precision` on `skim-ast-try-catch-finally-javascript` (a false-positive guard) | two independent edges declared, the verify gate accepts either (`patterns.rs`, `compound/reparse.rs`) | one `try_statement` must carry both clauses; XPASS: remove the ledger entry, **keep** the golden guard, bless (no reason) |
| #571 | `structural.recall` on `zod-ast-try-catch-tsx`, `zod-ast-unhandled-result-tsx` | `.tsx` → `Language::TypeScript` → `LANGUAGE_TYPESCRIPT` (`rskim-core/src/types.rs`); JSX makes `_og.tsx` an ERROR root | skim parses `.tsx` with `LANGUAGE_TSX`; both XPASS; other `*-tsx` ratchets may move (a drop needs a reason) |
| #572 | `structural.precision` on `ripgrep-ast-empty-function-rust` | `is_counted_child` drops `self` (in `PUNCTUATION_KIND_IDS`), so `{ self }` counts as empty (`ast_index/structural.rs`) | count the tail expression as a body element, as the oracle does; promote |

"Promote" is lifecycle 1 below: remove the ids, then bless `xfail -> pass` (no reason needed).

### Bless rules (`baseline.rs::bless`, pure)

`bless` refuses (exit 1) a partial `--only` report; a report whose per-corpus golden digest differs from what this
scoreboard computes for the files on disk (`golden_hashes_on_disk`, oracle fingerprint folded in); any FAIL or XPASS;
and any RATCHET regression or HARD downgrade (`pass -> xfail`, `<blessed> -> not run`, a blessed corpus no longer
run) without a non-blank `--accept-regression "<reason>"`. Reasons and accepted lines append to
`accepted_regressions[]` (carried forward, never dropped); a needless `--accept-regression` is ignored with a note.

| RATCHET movement (`compare_ratchet`) | `check` | `bless` |
|---|---|---|
| within tolerance | ok | n/a |
| Improved | FAIL "bless required" | no reason needed |
| Regressed (bad direction) | FAIL | needs `--accept-regression` |
| Changed: a Neutral value (reference columns, `structural.unscored_rows.<pattern>`), or a ZeroBest sign flip | FAIL | no reason needed |
| new metric / no longer measured | FAIL | no reason needed (`regressions()` compares only shared metrics) |

`universe.delta` and `universe.skipped_by_reason_mismatch` are RATCHETs (ZeroBest, exact, bar "= 0"), so leaving 0
needs a reason. Fix the mismatch instead (SEARCH-ADR-008: the oracle reproduces walked ∪ tracked independently).

### Oracle independence

Nothing on the scoring path may import skim's search code (`rskim_search::query_substring_present`, a tokenizer,
`rskim_core::Language`, the AST search stack). Where an oracle must agree with skim it keeps **its own cited copy**,
so a policy change on skim's side shows up as a scoreboard diff instead of flowing through silently:

- `oracle.rs`: `LANGS` (a copy of `Language::from_extension` / `parse_lang_value`) and its own `is_word_byte`;
- `universe.rs`: `MAX_FILE_BYTES` (5 MiB), the `MINIFY_*` gate, `SERDE_EXTENSIONS`, the hidden-component rule,
  `MAX_INDEXED_FILES` (50 000), skip labels equal to `PersistedSkipReason::label()`;
- `rskim-oracle` `structural.rs`: `EXT_CLASSES` (skim's extension table × the languages skim AST-indexes × the
  grammar table), `UNKNOWN_EXTENSION`, and `AST_SIZE_CAP_BYTES` (1 MiB, **inclusive**: skim excludes only `len > cap`).
  `oracle::tests::langs_agree_with_the_structural_oracles_extension_table` keeps `EXT_CLASSES` and `LANGS` naming the
  same extensions the same way, except `.tsx` (`tsx` structurally, `typescript` in `LANGS`).

Independence holds at two strengths: **compile time** for `rskim-oracle` (below), **convention** for everything in
`rskim-bench` (`oracle.rs`, `universe.rs`, `metrics.rs`, `structural_metrics.rs`), whose crate depends on
`rskim-search` / `rskim-core`, so a forbidden import would compile. Two bench modules read skim on purpose and score
nothing: `catalog.rs` (the one catalog read) and `golden_gen.rs` (`rskim_core::Language` only as a dispatch key).

## State Transitions

HARD outcome classification (`gate::classify`) happens before the baseline comparison:

| raw result | ledgered? | outcome | gate | blessable |
|---|---|---|---|---|
| pass | no | PASS | ok | yes |
| fail | yes | XFAIL | ok | yes |
| fail | no | FAIL (Unledgered) | fails | no |
| pass | yes | XPASS | fails: "promote" | no |

The baseline stores only `pass` / `xfail` per `(id, check)`; any change, `new -> pass` included, is "bless required".

1. **You fixed a ledgered bug.** XPASS. Remove the ids (or entry) and push; CI shows `xfail -> pass; bless
   required`; bless from that run's `scoreboard-report` artifact and push `baseline.json` (or locally in one push:
   `scoreboard run --out <dir>`, then `bless --from <dir>/report.json`).
2. **A real bug you will not fix here.** File the ticket, add an `[[xfail]]` with the exact ids, re-bless with
   `--accept-regression "<reason>"` (`pass -> xfail` is a downgrade).
3. **You added a HARD check or golden entries.** `new -> pass` / `new -> xfail` plus a digest change: bless, no
   reason. New `[[ast]]` entries also shrink the `structural.unscored_rows.<pattern>` they now score (neutral).
4. **You removed a golden entry** (e.g. an `[[ast]]` entry a pin bump made vacuous). Its checks go `-> not run`, a
   downgrade: remove its ledger ids too, bless with `--accept-regression`.
5. **You bumped a corpus pin.** Update `corpora.toml` and the golden `commit` together, re-verify every `def.line`
   (drift is exit 2); CI re-clones (cache key `hashFiles(corpora.toml)`). Refresh ledger ids from the new
   `report.json`, confirm each `expect_oracle_empty` guard's oracle is still empty, bless.

## Technical Implementation Patterns

### Per-corpus pipeline (`pipeline::run_corpus`, in order)

`pipeline::run` compiles one `StructuralOracle` per run, only if some golden file has `[[ast]]` entries. Per corpus:

1. `materialize_verified` (clone or reuse at the pin, require `Reusable`); `Universe::compute` + `check_file_cap`.
2. `checked_plan`: golden integrity (catalog-aware for `[[ast]]`) plus ledger refs, then `metrics::plan`.
3. `structural_answers` (only with `[[ast]]` entries): the timed oracle pass, then `require_expected_empty_oracles`,
   **before skim runs** (it needs only the oracle).
4. `runner.build` (`--build` must exit 0), `runner.stats`, `require_temporal_data`.
5. `observe_plan`: one `--ast <pattern>` full-list call per catalog pattern (`call_patterns`), then every other
   entry's calls; an `[[ast]]` entry makes no call of its own (it is its pattern call's rows in its language). Then
   `require_oracle_less_rows` and `require_non_vacuous_structural`.
6. `verify_untouched`: `git status --porcelain --untracked-files=all --ignored` must be clean (skim wrote nothing).
7. `metrics::evaluate`, `gate::apply_ledger`, the report; aggregate RATCHET values and `gate::evaluate` run last.

### Runner contract (`runner.rs`)

- `search --root <clone> --json --limit N [--offset K] <flags> [-- <query>]`: flags before `--`, the query after, so
  `-D warnings` and `->` parse as text. One temp `HOME` per run (`SkimSandbox`) redirects `SKIM_CACHE_DIR`, agent
  config dirs and `SKIM_WRAPPERS_DIR`, sets `SKIM_DISABLE_ANALYTICS=1` / `NO_COLOR=1`, strips `SKIM_PASSTHROUGH` /
  `SKIM_DEBUG` / hook and session vars; the oracle's `git ls-files` shares its `GitIsolation`.
- A non-zero exit whose stdout parses as the arm's envelope is accepted; a signal, non-JSON or malformed envelope is a
  harness error, and so is an `ast_list` full list reporting `has_more`.
- Byte metrics come from a separate **text-mode** run at the default limit, counting **stdout + stderr**, with every
  `in <N>ms` normalized to `in 0ms`.

### Subprocess timeouts kill the whole process group (`clone.rs`)

Every subprocess goes through `rskim_research::clone::git_output_with_timeout` (skim calls 120 s /
`SKIM_TIMEOUT_SECS`, network git 300 s, local git and `ls-files` 120 s). Killing only the direct child is not enough:
a grandchild holding the pipes keeps `wait_with_output` blocked (a 120 s timeout once returned after 179 s, 6129a36).
`spawn_in_own_group` sets `process_group(0)`; on the deadline `kill_process_tree` sends SIGKILL to `-pid` then
`pid` (`taskkill /T` off-unix), and the wait thread is joined only if it finishes within `KILL_GRACE` (2 s),
otherwise detached, so nothing blocks past timeout + 2 s. Reuse `git_output_with_timeout` / `git_run_with_timeout`
for any new subprocess; the error reports the elapsed time actually taken.

### The `rskim-oracle` crate (#541 AC-3)

The structural oracle is its own crate so that its independence is a **compile-time** fact: `rskim-bench` must
depend on `rskim-search` (catalog read, `golden_gen`, BM25F bench), so no compiler can enforce an import rule there. `crates/rskim-oracle/tests/independence.rs` checks:

- every entry of every dependency table (`[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, and their
  `[target.<cfg>.*]` forms; `workspace = true` resolved through `[workspace.dependencies]`) is a registry crate on
  `ALLOWED`: `tree-sitter`, the five grammars (rust, python, typescript, javascript, go), `anyhow`, `rayon`,
  `serde`, `sha2`, `toml`;
- a `rskim-*` crate fails with its own message however named (a `package =` rename included), as does any `path` /
  `git` source (a local crate could be skim's code under another name);
- no `build.rs` / `package.build`, and no file under `src/`, `tests/`, `benches/`, `examples/` contains `include!(`
  or a `#[path = "…"]` leaving its directory (`..` or absolute). It is a text scan: a comment spelling either fails
  too. `include_str!` of the `.scm` data is allowed.

The crate never sees skim's catalog: it knows its registry by pattern **name** (`QUERIES`, `UNCOVERED`, `INTENTS`,
`coverage_of`), and `rskim-bench` crosses the two.

### Queries, grammars and the answer model (`structural.rs`)

- One hand-written query per (pattern, language), `queries/<pattern>.<lang>.scm` (53 files, 26 patterns),
  `include_str!`ed by `oracle_query!` into `QUERIES`, sorted by pattern then language. Each encodes the catalog
  **description**, not skim's n-grams, and captures one `@match` whose first line (1-based) is the match line.
  `StructuralOracle::new` fails on a query that does not compile, lacks `@match` or its filter capture, or repeats a pair.
- **Line 2 names the grammar**: `; Grammar: <crate> <version>.`, with ` (LANGUAGE_TYPESCRIPT)` / ` (LANGUAGE_TSX)`
  before the full stop for `tree-sitter-typescript`, and no other `; Grammar:` line.
  `every_query_names_the_grammar_version_cargo_lock_resolves` compares it with this crate's `Cargo.lock` entry: a
  grammar bump fails until the headers are edited, and that edit moves the digest (bless).
- Six `OracleLang`s; `Tsx` is separate because the oracle parses `.tsx` with `LANGUAGE_TSX` (SEARCH-ADR-007: an
  independent oracle uses the real grammar), which is exactly what #571 ledgers.
- `PostFilter` on six queries: `Empty` (`empty-catch` ×3, `empty-function`), `AtLeast` 20 (`god-function`) and 5
  (`excessive-params`). It counts **body elements**: named, non-extra (comments are extras), non-`ATTRIBUTE_KINDS`
  children, so a Rust tail expression counts (the #572 difference).
- **Intent oracles** (`INTENTS`: TS/TSX/JS `nested-loop`, `rust-nested-loop`): a loop with a loop ancestor inside the
  same function; functions, arrows and closures are boundaries. `nested_loop_lines` is **one pre-order `TreeCursor`
  pass** carrying "inside a loop of this function" on a stack, bounded by `descendant_count`. A `Node::parent`
  ancestor walk re-descends from the root, quadratic in depth (a 3,000-arm `else if` chain: >120 s vs about 1 s); it
  survives only as the reference of the differential test `the_intent_answer_agrees_with_the_ancestor_walk`.
- Files with syntax errors are queried, not skipped (skim's walk visits error nodes too); only "no tree" is an error.
  `file_matches` returns `NotScored(LangClass)`, `OverSizeCap(lang)` or `Scored(FileReport)` (every pattern's and
  intent's lines for that language, from **one parse**).

### `OracleScratch` and `ORACLE_MATCH_LIMIT`

`OracleAnswers::compute` is rayon-parallel with `map_init(OracleScratch::new, …)`: one parser and one `QueryCursor`
per worker, reused file to file (the compiled oracle is shared; `Query` is `Send + Sync`). The cursor holds at most
`ORACLE_MATCH_LIMIT` = 4096 in-progress matches (tree-sitter's contract is `1..=65536`; unlimited, its `u16`
capture-list ids wrap past 65,535); the corpora never need more than 3 over 1,181 scored files. Past the limit
tree-sitter **drops** matches, so `did_exceed_match_limit()` is a harness error naming the `.scm`. The limit is **set
once, when the scratch is made**: tree-sitter caps how many capture lists the pool allocates and reuses freed ones
first, so lowering it on a used cursor would not bind (`with_match_limit` is `#[cfg(test)]`, forcing the error path
with 1). With several failing files, `first_failure_by_path` reports the first by path, independent of scheduling.

### The fingerprint and the golden digest

`structural::fingerprint()` is the `Display` of `OracleInputs`: the size cap, every `EXT_CLASSES` row and
`UNKNOWN_EXTENSION`, `ATTRIBUTE_KINDS`, every query (file name, post-filter, byte length, full text; sorted, so
registry order is not an input) and every `IntentSpec`, written explicitly (never `Debug`, so toolchain-stable) and
naming files, not directories. `rskim-bench` folds its hash into each golden file's digest:

```rust
// golden.rs load_golden — only a golden file with [[ast]] entries folds the oracle in
let oracle = (!file.asts.is_empty()).then(structural_oracle_sha256); // hex sha256 of fingerprint()
let sha256 = golden_digest(raw.as_bytes(), oracle.as_deref());
// golden_digest(raw, Some(o)) = sha256(raw ‖ "\0structural-oracle\0" ‖ o);  None → sha256(raw)
```

All four golden files carry `[[ast]]` entries, so **any** oracle-input edit moves all four `golden_sha256` values:
`check` prints `corpus <c>: golden file or structural-oracle fingerprint changed; bless required`, and `bless`
refuses a report made with other oracle inputs. Deliberately **excluded**: `MATCH_CAPTURE` (a query without `@match`
fails to compile into the oracle), `ORACLE_MATCH_LIMIT` (it can turn an answer into an error, never change one) and
grammar versions (carried by the headers). The `any_*_edit_changes_the_fingerprint` tests prove each input moves it
alone, via a helper that destructures `OracleInputs` without `..`: a new input fails to compile until tested.

### Catalog injection (`catalog.rs`)

`catalog::skim_catalog()` is the **only** reader of `rskim_search::all_patterns()`. It projects each pattern to
`CatalogPattern { name, exact, example }`, dropping the n-gram tables that encode how skim matches (AC-3), and is
called once by `Inputs::load` and once by `golden-gen --ast`, which pass the slice to integrity, the call set and
`uncovered_patterns`; `catalog_coverage` crosses names with `coverage_of`. `exact` only seeds the class `golden-gen`
proposes. `example` only feeds `catalog_tests.rs` (in `rskim-bench`, because only it may see the catalog): every
registered query must match its catalog example (or a hand-written positive) on the expected lines and reject a
near-miss, and every catalog pattern must be covered or listed in `UNCOVERED` with a reason. A pattern newer than the
oracle reads `UNCLASSIFIED_REASON` and fails; `uncovered_patterns_are_the_three_that_cannot_be_encoded` pins the list.

### Golden `[[ast]]` entries and scoring (`structural_metrics.rs`)

- 53 entries (skim 23, zod 18, ripgrep 9, flask 3), id `<corpus>-ast-<pattern>-<lang>`, at most one per
  (pattern, language). `precision` (`hard` | `ratchet`) is frozen in golden; the gate never re-reads `exact`.
- A corpus with any `[[ast]]` entry calls skim for **every** catalog pattern (29 × 4 = 116 calls). Rows split by
  `structural::classify` (`.tsx` is its own language); every row no entry scores lands in
  `structural.unscored_rows.<pattern>`, emitted for every called pattern, 0 included. Calling only entry patterns
  would silently drop rows (e.g. skim's god-function / excessive-params rows on flask and zod).
- **HARD**: `structural.recall` (every oracle file returned); `structural.precision` (`hard`: every returned file
  matches); `structural.coverage` (`ast_coverage.size_excluded_files` equals the oracle's `over_cap_count` over
  size-capped classes, Bash included, JSON/YAML/TOML/unknown not, and `undetermined_files` is 0); plus
  `results.unique_paths`. Coverage is corpus-wide but recorded **per `[[ast]]` id** (every HARD record, ledger ref and
  baseline state is keyed by a golden id): ledgering it needs every `[[ast]]` id of the corpus, and a new entry fails
  until ledgered too.
- **RATCHET** (`STRUCTURAL_FAMILIES`, all exact): `structural.precision.<id>` (`ratchet` class),
  `.line_on_match.<id>`, `.intent_recall.<id>` / `.intent_precision.<id>` (nested-loop entries) are HigherBetter;
  `structural.unscored_rows.<pattern>` is **Neutral** (summed over corpora in the aggregate): any move fails `check`,
  `bless` needs no reason, since no oracle judges those rows.
- `line_on_match` reads low by design: skim anchors on the child completing an edge (`catch_clause`, `class_body`),
  the oracle on the construct; try-*, class-method and impl-method read 0.
- **Vacuity** (`is_vacuous`, exit 2 in `require_non_vacuous_structural`): oracle and skim both empty in the entry's
  language. A **false-positive guard** (`expect_oracle_empty = true`) is exempt, since skim returning nothing is the
  fixed state it guards (recall and precision read 1 over an empty denominator; a returning row fails precision), but
  is vacuous itself when `scored_files(lang) == 0`. A guard whose oracle matches is exit 2 before skim runs
  (`require_expected_empty_oracles`), so the flag never hides recall. All share `golden integrity failed (N
  problem(s)):` + one `<id>: <reason>` line each; the report marks guards (`<class>, FP guard` in `report.md`).
- `OracleAnswers` pre-seeds **only registered** `(pattern, lang)` keys: absent means "no query" (an error in
  `definition()`), empty means "no match" (avoids PF-016). Never seed other keys.

### `golden-gen --ast`

It runs the same oracle, builds skim in a sandbox HOME, and calls every catalog pattern through the gate's own loop
(`pipeline::call_patterns`). `generate_ast` applies the gate's `is_vacuous` **twice** per covered pair (as an ordinary
entry, then as proposed, `expect_oracle_empty` iff `oracle files 0`), so it never proposes an entry the gate refuses.
Above the entries it prints `unscored_after` with every path through `comment_safe` (control characters, Unicode bidi
controls U+202A–202E / U+2066–2069 and `\` become Rust escapes), so skim's output can neither end the TOML comment
and forge an entry a reviewer pastes, nor reach the terminal as an escape sequence.

### Cost, latency and the dev-profile optimisation

CI builds the scoreboard in the **dev profile** (`target/debug/scoreboard`), so the root `Cargo.toml` sets
`[profile.dev.package.tree-sitter] opt-level = 3` (`cc` follows it, so the C core is optimised too): the oracle pass
takes 0.99 s over the four corpora (skim 635 ms, zod 203, ripgrep 130, flask 22), vs 5.12 s unoptimised, for ~4 s of compile.
Grammar crates stay default (their lexers are ~7% of the pass). `report.json` `latency` (INFO, outside baseline and
determinism) carries `structural_oracle_wall_ms` (absent without `[[ast]]`) and `pattern_calls_wall_ms` per pattern;
`calls`, percentiles and `entries_wall_ms` cover the entries' own calls only, so runs stay comparable.

### Adding a pattern query or an oracle language

- **Query**: write `queries/<pattern>.<lang>.scm` (title, `; Grammar:` on line 2, the quoted description, one
  `@match`); register it in sorted `QUERIES` (a disk ↔ registry test catches omissions); add a `PostFilter` only if
  the description states a count; drop it from `UNCOVERED` (and the pinned-list test) if listed; add a positive +
  near-miss fixture in `catalog_tests.rs`; `golden-gen --ast`; `check`; bless.
- **Language**: grammar crate in root `[workspace.dependencies]`, `crates/rskim-oracle/Cargo.toml` **and** `ALLOWED`;
  an `OracleLang` variant (`ALL`, `as_str`, `grammar`); its `EXT_CLASSES` row from `Unscored` to `Oracle` (`LANGS`
  must still agree); a `grammar_crate` arm in `structural_tests.rs`; queries, entries, bless.

### Pinned full-history clones (`ensure_pinned_history_clone`)

- Full history (`--no-checkout`, no `--depth` / `--filter`); `Shallow` is never reusable. An unreachable pin gets one
  `fetch origin <sha>`; it re-clones **once**, then errors. A non-reusable destination is deleted only if empty or
  holding `.git/skim-scoreboard-pinned-clone` (`OWNERSHIP_MARKER`), so a mis-set `--corpus-dir` errors, never deletes.
- `credential.helper=`, `transfer.fsckObjects=true`, one downgrade `fetch.fsck.zeroPaddedFilemode=ignore` (flask's
  `040000` tree modes, object `0b404df8…`); every call scrubs `GIT_DIR` / `GIT_WORK_TREE` / … and sets
  `GIT_CEILING_DIRECTORIES` to the destination's parent.

### Determinism

`report.json` is byte-identical across runs of one binary except `latency`. The workspace `serde_json` has
`preserve_order`, so every report / baseline map **must be a `BTreeMap`** (or a struct); lists are sorted, floats go
through `round4`, nothing records a timestamp, version or machine path. The oracle is order-independent too
(`BTreeMap` answers, sorted/deduped lines, path-ordered first failure). The baseline was blessed on macOS and gates
byte-identically on ubuntu CI; still bless from the CI artifact, because ubuntu is the platform that gates.

### CI wiring: fail closed, never a green skip

`changes` (**Detect Search Changes**) always gates `workflow_dispatch` and pushes to `main`, never pushes to
`feature/*` / `wave/**`, and fails closed on unknown events. On a PR, `git diff --name-only --no-renames -z HEAD^1
HEAD` is matched against `SEARCH_PATHS` (which includes `crates/rskim-oracle/`); `--no-renames` counts a file moved
*out of* a search path.

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

A job skipped by `if:` reports **success**, fine only outside the search paths. Build Check uploads its release
`skim` as `skim-release` (downloaded to `$RUNNER_TEMP`, never the cached `target/`); the scoreboard build cache prefix
is `cargo-build-scoreboard-`; the corpora cache is saved only by a successful job; `report.json` + `report.md` go to
the `scoreboard-report` artifact (30 days), which is what you bless from. Timeout 25 minutes.

## Operating the Gate

```bash
cargo build --release -p rskim                   # ALWAYS first: the scoreboard tests whatever binary is at --skim-bin (PF-019)
caffeinate -i -s cargo run -p rskim-bench --bin scoreboard -- check --skim-bin target/release/skim
# reports: target/scoreboard/report.{json,md}   (--out to change); run from the workspace root
cargo run -p rskim-bench --bin scoreboard -- bless --from target/scoreboard/report.json [--accept-regression "<why>"]
```

- **Run time:** the first run clones ~113 MB into gitignored `.bench-corpus/scoreboard/` (~30 s). A warm `check`
  takes **about 3.6 minutes** locally on Apple Silicon (212–221 s); the CI job **about 7 minutes**.
- `run` never gates; `check` = `run` + gate. `golden-gen --corpus <name>` proposes up to 20 `[[ident]]` entries (unique
  definition, name ≥ 6 bytes, 2–60 ground-truth files); with `--ast --skim-bin …`, `[[ast]]` entries. Never in CI.
- **Exit codes:** `0` pass; `1` gate failure or bless refused; `2` harness error (never a regression; no report).
  Each gate failure prints `FAIL <check> [<ids>]: <message>` (kinds `Unledgered`, `Xpass`, `Ratchet`, `Baseline`).
- `tests/scoreboard.rs` drives the real binary against a tempdir git fixture and a **stub `skim` script** (offline),
  including an `[[ast]]` entry whose stub serves an `--ast` list for every catalog pattern. Test harness changes there.

## Error Handling and Recovery

Every `Err` in `pipeline.rs` is exit 2, because scoring through it would give a false signal:

| Harness error | Why it is not a gate result |
|---|---|
| Golden integrity: pin mismatch, drifted `def.line`, bad regex, duplicate / unprefixed id, `zero-hit` with ground truth, pagination over `min(limits) × 63`, an `[[ast]]` pattern not in the catalog / uncovered / without a query in `lang`, two entries per pair, a stale ledger ref | The golden data itself is wrong |
| `temporal_state` ≠ `"ready"` while an entry ranks by temporal data (`require_temporal_data`); `degraded[]` on any page of a temporal-ranked entry (`ensure_ranking_applied`) | skim serves a fallback order; a ledgered check would **XPASS** and bake the breakage into ledger and baseline (939d1e9) |
| An empty full list on an oracle-less entry (`require_oracle_less_rows`) | It passes vacuously (the #547 XFAILs would XPASS, a4c7b3a); a shrink moves `oracle_less.full_rows.<id>` instead |
| A vacuous `[[ast]]` entry or guard; a guard whose oracle matches | It would measure nothing, or the flag would hide a recall loss |
| Oracle failure (no tree; over `ORACLE_MATCH_LIMIT`); an `--ast` full list with `has_more` | A partial answer scores as a miss or a false positive |
| Malformed envelope, signal, timeout, `--build` exit ≠ 0, `--stats` `{"error":…}` | skim is broken, not worse |
| Corpus not `Reusable` after the run; > 50 000 walk-accepted files; unknown `--only`; invalid data file | The environment or input is wrong |

`run` / `check` delete any previous `report.json` / `report.md` in `--out` **before** any work (2245a7b), so a failed
run never leaves an older passing report for `bless --from`.

## Anti-Patterns

- **Blessing without reading the failure.** "Bless required" means *look*; a vague `--accept-regression` leaves a
  permanent, unhelpful `accepted_regressions[]` record.
- **Following an XPASS "promote" when the temporal or AST layer might be broken**, or weakening any exit-2 guard
  (`require_temporal_data`, `ensure_ranking_applied`, `require_oracle_less_rows`, `require_non_vacuous_structural`,
  `require_expected_empty_oracles`) to get past it.
- **Ledgering without a filed ticket, or with guessed ids.** Copy the exact ids from `report.json`.
- **Deriving golden data or an oracle query from skim's output**, or regenerating golden in CI.
- **Importing skim code into the scoring modules**; giving `rskim-oracle` an `rskim-*` / `path` / `git` dependency,
  a `build.rs`, an `include!` or an escaping `#[path]`; reading `all_patterns()` outside `catalog.rs`. Mirror the
  policy with a citation instead, or the scoreboard stops catching the change.
- **Seeding `OracleAnswers` with unregistered keys**, or lowering the match limit on a used cursor (it would not bind).
- **Calling `--ast` only for patterns with entries**: skim's other rows would vanish from `unscored_rows`.
- **`HashMap` in any report, baseline or oracle-answer type** (breaks byte-determinism).
- **A subprocess that bypasses `git_output_with_timeout`**, or times out with a direct-child kill.
- **Editing `SEARCH_PATHS` in one place.** It is mirrored in `ci.yml`, the README "CI" section and CLAUDE.md.

## Gotchas

- **Stale binary.** Only `--skim-bin` is canonicalized; nothing records skim's version, so an old
  `target/release/skim` is silently what gets scored. Build first (PF-019); never run two release builds at once.
- **macOS.** Sleep once stretched a run to ~27 minutes while per-call timers looked normal (`caffeinate -i -s`). A
  freshly linked binary's first exec can stall at `_dyld_start` (XProtect): warm it with `--version` (PF-013).
- **Every run is a cold build** (fresh HOME): incremental / staleness / auto-refresh paths are **not** exercised.
- **Digest-changing edits force a re-bless.** Any golden byte (a comment included) and any oracle input (a `.scm`, a
  post-filter, an intent spec, the cap, the extension table, the attribute kinds) moves `golden_sha256`, and `bless`
  refuses a report made before the edit. `.gitattributes` pins `text eol=lf` on `crates/rskim-oracle/queries/**` and
  `crates/rskim-bench/scoreboard/**` so a CRLF checkout cannot ask for a bless.
- **Golden header bytes are hashed.** The four `golden/*.toml` headers still cite bare `(ADR-003)`, not
  `SEARCH-ADR-003` (#561 namespace rule); fixing that comment is a digest move needing a bless, so it is parked as a
  Polish item under #540. Do not "just fix the comment" in an unrelated PR.
- **Skim-side changes surface here, on purpose.** A new catalog pattern fails `catalog_tests` until it has a query or
  an `UNCOVERED` reason, and adds a `structural.unscored_rows.<pattern>` metric (bless). A skim AST size-cap or
  AST-indexed-language change shows as a `structural.coverage` or recall/precision diff until `AST_SIZE_CAP_BYTES` /
  `EXT_CLASSES` follow (fingerprint → bless). A new language / extension, or a change to walker limits, the minified
  gate, the hidden-path rule or the file cap, needs `LANGS` / `universe.rs` edits or `universe.delta` leaves 0.
- **`--only`** reports are partial: no aggregate or missing-corpus checks, `bless` refuses them, and
  `uncovered_patterns` then lists patterns whose entries are in other corpora.
- **Text-mode output is load-bearing.** `metrics::is_block_header` recognizes `<path>:<digit>…` or `<path>  [`; a
  renderer change can turn every `first_correct` into a miss. **stderr counts toward `bytes.text_*`**, so a new
  search-path notice moves byte ratchets (±3% is relative: a baseline of 0 tolerates nothing).
- **Reference columns** (`*.baseline_alpha`, `*.baseline_count`, `bytes.rg_*`) moving on a skim-only PR means the harness changed.
- **Lints.** `rskim-bench` and `rskim-oracle` deny `unwrap_used` / `expect_used` / `panic` (test modules carry
  `#[allow(...)]`); lint with `cargo clean -p <crate> && cargo clippy -p <crate> --all-targets -- -D warnings`
  (PF-009: `*_tests.rs` are `#[cfg(test)] #[path]` modules).

## Key Files

- `crates/rskim-bench/src/bin/scoreboard.rs`: CLI (`run|check|bless|golden-gen [--ast]`), exit codes, sandbox HOME.
- `crates/rskim-bench/src/scoreboard/pipeline.rs`: per-corpus orchestration, timed oracle pass, `call_patterns`, exit-2 guards.
- `crates/rskim-bench/src/scoreboard/runner.rs`: skim subprocess contract, `SkimSandbox`, `ast_list`, `ensure_ranking_applied`.
- `crates/rskim-bench/src/scoreboard/metrics.rs`: `plan` / `runs`, HARD checks, `RATCHET_METRICS`, `STRUCTURAL_FAMILIES`, `compare_ratchet`.
- `crates/rskim-bench/src/scoreboard/gate.rs` / `baseline.rs`: ledger, `classify`, `evaluate`; `baseline.json` and `bless`.
- `crates/rskim-bench/src/scoreboard/golden.rs` / `golden_gen.rs`: schema (`AstEntry`), integrity, `golden_digest`; proposals, `comment_safe`.
- `crates/rskim-bench/src/scoreboard/oracle.rs` / `universe.rs`: lexical oracle and `LANGS`; the oracle universe.
- `crates/rskim-bench/src/scoreboard/catalog.rs` (+ `catalog_tests.rs`): the one catalog read; per-query fixtures.
- `crates/rskim-bench/src/scoreboard/structural_metrics.rs`: `OracleAnswers`, row splitting, structural checks, vacuity.
- `crates/rskim-oracle/src/structural.rs` (+ `structural_tests.rs`): registry, `EXT_CLASSES`, cap, scratch, intent walk, fingerprint.
- `crates/rskim-oracle/tests/independence.rs` and `queries/*.scm`: the AC-3 guard; one query per (pattern, language).
- `crates/rskim-bench/scoreboard/{corpora.toml,golden/*.toml,known_failures.toml,baseline.json}`: the data (`baseline.json` only via `bless`).
- `crates/rskim-bench/tests/scoreboard.rs`: offline stub-skim tests; `crates/rskim-research/src/clone.rs`: pinned clones, process-group timeout.
- `.github/workflows/ci.yml`, root `Cargo.toml` (`[profile.dev.package.tree-sitter]`), `.gitattributes` (LF for hashed files).

## Related

- **SEARCH-ADR-007** (amended 2026-09-25): this is the required search gate; dog-food covers only its blind spots.
  **SEARCH-ADR-008**: walked ∪ tracked, `universe.delta` = 0. **SEARCH-ADR-003**: ratchets on measured values.
  **SEARCH-ADR-005**: green is not merge permission, outside the 2026-09-26 standing authorization for #540 PRs.
- **PF-019** build first; **PF-013** warm fresh macOS binaries; **PF-009** `clippy --all-targets` after `cargo clean
  -p`; **PF-016** keep key absence load-bearing (`OracleAnswers` seeds registered keys only).
- Feature knowledge `cmd-search`: the CLI judged here; #544 in `cmd/search/query.rs`, #545 in `temporal.rs`
  `resort_window`, #547 in `ast.rs`.
- Feature knowledge `ast-index`: the pattern catalog (`patterns.rs`), `is_counted_child` (#572), the verify gate
  behind #546, and the 1 MiB inclusive `ast_size_limit` the oracle mirrors.
- Feature knowledge `search-temporal` / `temporal-scoring` (temporal arms, `temporal_state`); `research-ast` / `cochange` (share `clone.rs`).
