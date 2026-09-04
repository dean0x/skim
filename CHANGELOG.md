# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### BREAKING

- **`skim search` temporal layer now walks the full commit DAG** (#407) — the
  commit population for `--hot`, `--cold`, `--risky`, and `--blast-radius` changes
  from first-parent-only traversal to a full DAG walk that skips merge commits, matching
  `git log --no-merges` and skim heatmap's population.  The measured effect on this
  repository: `crates/rskim/src/cmd/search/query.rs` moves from 21 first-parent commits
  to 67 full-DAG commits, and fix density rises from 0.095 to 0.522.  Hot/risky scores
  were previously undercounted by approximately 3× on branch-heavy workflows.

  **Upgrade note (one slow query per root):** the first query after upgrading costs one
  slow rebuild per project root while the `TEMPORAL_DATA_VERSION` self-heal (1 → 2)
  replaces the stored data.  Subsequent queries are fast until HEAD advances; the first
  query after each new commit re-walks history.  (If a previous history walk failed for
  the current HEAD, the retry backoff defers the heal until HEAD advances or you run an
  explicit `skim search --rebuild`.)

  **Adopted-root caveat:** a `--root` whose `temporal.db` records a different
  `git_toplevel` (e.g. the enclosing repository changed) is **refused** rather than
  silently re-anchored — skim exits 0 with a typed notice.  The remedy is an explicit
  `skim search --rebuild --root <path>` which re-anchors and writes the new toplevel
  (PF-017 by design).

  **`--blast-radius` consequence:** because Jaccard numerators and `file_counts`
  denominators now reflect the full commit population, co-change peer **sets and ordering**
  may differ from pre-#407 databases after the self-heal rebuild (AD-407-9).

  **Build-time performance (AC-23):** `skim search --rebuild` on this repository with
  a warm OS page cache: pre-#407 baseline (057ff42, first-parent-only) **7.95 s**;
  post-#407 (full-DAG) **8.06 s** — ratio **1.01×**, well within the ≤ 3× guard.
  The temporal history walk visits 2.5× more commits (264 first-parent → 662 full-DAG)
  but its cost is small relative to file indexing; the per-commit gix committer-time
  lookup adds no measurable overhead at this repository size.

  **Warm query-path performance (AC-24):** a warm `skim search --risky` and a warm
  `skim search <text>` on an already-current `temporal.db` both measured **< 10 ms**
  on this repository (well within the < 50 ms target); the query path is unchanged
  by #407 — only the build path (history walk) changes.  The single post-upgrade
  self-heal query is explicitly exempt: it re-runs the full history walk and may take
  several seconds; subsequent queries are fast.

- **`skim search` argv parsing is now strict and symmetric** (#412) — unknown
  single-dash flags (`-i`, `-w`, `-C`) are rejected with an `unrecognised flag` error
  and a pointer to the `--` escape hatch, matching the pre-existing long-flag behavior
  (previously they were silently folded into the query text). Combining a text query
  with an action flag (e.g. `skim search foo --rebuild`) now hard-errors as an ambiguous
  mixed form instead of silently running the action and discarding the query. Bare `-`
  remains a valid positional query token.

- **`skim search` AST index format: v2 → v3 (1 MiB size cap + coverage policy)** (#405):
  The AST structural index format version is bumped from 2 to 3 to reflect the raised
  per-file size cap (100 KiB → 1 MiB) and the new coverage taxonomy.  Any existing
  `ast_index.skidx` written by a prior version is automatically detected as stale and
  triggers a cold rebuild on the next query (`check_staleness`).  No manual `--rebuild`
  needed, but expect the rebuild to index more files (and produce a proportionally larger
  `ast_index.skpost`) if your repo contains files between 100 KiB and 1 MiB.

- **`skim search` AST extraction cache format: v1 → v2 (size-cap policy change)** (#405):
  The per-file AST extraction cache (`ast_index.skcache`) format version is bumped from 1
  to 2.  Cached entries computed under the old 100 KiB cap are silently discarded on the
  next build; affected files are re-extracted at up to 1 MiB.  No user action required.

- **`skim search` manifest format: v2 → v3 (FileId↔path ordering skew fix)** (#373) —
  Standalone `--ast` queries and all other FileId consumers (lexical, blast-radius, temporal)
  could return the wrong files whenever the project contained nested directories (e.g.
  `foo/bar.rs` and `foo.rs`), because FileIds were assigned in `PathBuf` component order
  but resolved in `BTreeMap<String>` byte order.  The fix aligns both sides to byte-wise
  string order.  The manifest on-disk JSONL layout is unchanged; the version bump forces a
  one-time automatic rebuild on the first query after upgrade so no pre-existing index
  silently serves wrong files.  No manual `--rebuild` needed.

- **`skim search` index format: v3 → v4 (posting lists now delta+varint compressed)** (#358) —
  Posting entries changed from a fixed 9-byte layout to a variable-length delta+varint encoding
  (`[varint delta_doc_id][u8 field_id][varint delta_position]`), reducing the lexical index size
  by approximately 61% on a representative corpus (measured: 3.53x source ratio vs 9.04x before
  compression).  Any existing `.skim/` index written by a prior version is silently stale; the
  first query triggers an automatic rebuild (`auto_refresh_if_stale`).  To force an immediate
  rebuild: `skim search --rebuild`.

- **`skim search` index format: v2 → v3 (n-gram key widened u16 → u32)** (#355) —
  The n-gram inverted-index key is now a 32-bit trigram `(b1<<16)|(b2<<8)|b3` instead
  of a 16-bit bigram `(b1<<8)|b2`.  Any existing `.skim/` index written by a prior
  version is silently stale; the first query triggers an automatic rebuild
  (`auto_refresh_if_stale`).  To force an immediate rebuild: `skim search --rebuild`.

### Added
- **`skim search` AST size cap raised 100 KiB → 1 MiB** (#405): Files up to 1 MiB are
  now eligible for AST structural indexing (named patterns and containment queries via
  `--ast`).  Files that exceed 1 MiB remain excluded and are fully text-searchable.
  The cap is language-aware (`ast_size_limit(Language)`): data formats (JSON/YAML/TOML)
  are never AST-indexed (`ast_size_limit` returns `None` for them — they are
  non-participants, not uncapped participants).  Expect a proportionally
  larger `ast_index.skpost` on repos with large source files.

- **`ast_coverage` in `--ast` JSON output and `--stats --json`** (#405): The
  `--ast` JSON envelopes (`AstJsonEnvelope`, `QueryOutput`) now include an `ast_coverage`
  key when any files are excluded from AST indexing by the size cap.  The key is absent
  when all files are within cap (`is_clean()`).  Fields: `size_eligible_files`,
  `size_excluded_files`, `undetermined_files`, `excluded_by_lang` (per-language breakdown),
  and `excluded` (bounded path-sorted sample of up to 10 excluded files).
  `skim search --stats --json` applies the same omit-when-clean guard as the `--ast` surfaces.

- **`skim search` coverage notice on `--build` / `--rebuild` / `--update` / `--ast` / `--stats` / first-ever build**
  (#405): When files are excluded from AST indexing by the 1 MiB cap, a notice is printed
  to stderr on every explicit build, refresh, standalone `--ast` query, `--stats` invocation,
  and the first-ever build triggered by a pure-lexical query (NoIndex case).
  The notice lists the file count and per-language breakdown, and is suppressed when all
  files are within cap.  It does NOT fire on incremental self-heals (D-4 cadence).

- **`skim search --stats` AST coverage section (text mode)** (#405): `skim search --stats`
  now prints `ast eligible`, `ast excluded`, and (when non-zero) `ast undetermined` lines
  after the existing stats when any files are excluded by the cap.  The section is omitted
  on clean repos (no excluded or undetermined files), keeping `--stats` output byte-identical
  to the pre-cap binary on clean codebases.

- **`skim search --phrase --near N` composition**: `--phrase` and `--near N` may now be
  combined: the composed semantic is ordered (strictly ascending word positions) AND total span
  ≤ N word tokens - a `PhraseNear(N)` verify mode.  Previously `--near` was silently discarded
  whenever `--phrase` was also set.  Fix applies at both the index-reader layer (posting
  intersection) and the verify/CLI-gate layer.  `--phrase --near (k-1)` is semantically
  equivalent to `--phrase` for a k-word query; `--phrase --near N` ⊆ `--near N` (never wider
  than the bare near result set). (#403)
- **`verify_mode` in `skim search --json` output**: the `QueryOutput` envelope now includes a
  `verify_mode` key (`"phrase"`, `"near"`, or `"phrase_near"`) when a non-default positional
  mode is active.  The key is absent (not null) for plain substring queries to maintain
  byte-identity for existing callers. (#403)
- **Inert-flag notice for `--phrase` / `--near` on non-text arms**: using `--phrase` or
  `--near` with `--build`, `--rebuild`, `--update`, `--stats`, `--ast` (standalone), or
  temporal-only queries now prints a notice to stderr instead of silently discarding the flags.
  Exit code remains 0. (#403)
- **`skim search --offset N`** — skip `N` verified results before collecting `--limit` results,
  enabling pagination across all query paths (pure-lexical, compound text+`--ast`,
  `--blast-radius`, and temporal).  The offset is applied AFTER the verify gate so page
  boundaries are stable even when stale/incidental-overlap candidates are dropped.
  Default: 0 (no skip). (#372)
- **`skim search --weights lexical,ast,temporal`** — tune the `--blast-radius` composite
  ranking; default `0.5,0.3,0.2`. Ratios only (not normalized; zero and non-sum-to-1 are
  accepted, negative/NaN/inf rejected). On the `--blast-radius` path only the lexical and
  temporal signals are active — the AST weight is inert until the full text+AST+temporal
  compound dispatch lands (#339). (#200, #202)
- **`skim search --blast-radius` union fusion** — co-change peers are now union-fused with
  lexical hits via reciprocal-rank fusion: a file that only co-changes with the target
  (no lexical match) now appears in results, scored by fused RRF rather than being dropped.
  (#198, #200)
- **Temporal DB auto-refresh on `skim search`** — after an index (re)build, the temporal
  database is rebuilt when git `HEAD` has changed, keeping `--hot`/`--cold`/`--risky`/
  `--blast-radius` current without a manual rebuild step. Non-fatal and debug-gated: a
  temporal failure never fails the lexical search. (#289)
- **Incremental AST n-gram cache** — the index reuses per-file AST structural entries
  (`ast_index.skcache`) across builds for files whose content is unchanged, avoiding a
  full AST re-parse on every rebuild. (#290)
- **`skim search --` end-of-flags separator** (#412) — every token after a bare `--`
  is treated as literal query text, so dash-leading terms (`-Werror`, `->`, `-5`) and
  even flag-looking words (`--rebuild`, `-h`) can be searched: `skim search -- -Werror`.
  Output flags (`--json`, `--limit`, `--root`, …) must appear BEFORE `--`, and the
  `--help`/`-h` scan is likewise bounded at the first `--`.
- **`skim search` text-mode summary now echoes the effective query** (#412) — the
  non-empty human summary reads `N result(s) for "query" in Tms` (previously
  `N result(s) in Tms`) so a mangled query can never masquerade as a successful search.
  JSON output is unchanged.
- **`git_head_state` in `skim search --stats --json`** (#413) — the `--stats --json`
  object now includes a `"git_head_state"` string key alongside the existing
  `"git_head"` key.  Values: `"resolved"` (HEAD resolves to a SHA), `"unresolved"`
  (HEAD exists but cannot be resolved to a commit — unborn branch or reftable backend),
  `"not_a_repo"` (root is not inside a git repository).  Unlike `"git_head"` (which
  is the SHA stored at the last index build), `"git_head_state"` reflects the live
  resolution status at call time and can legitimately diverge from `"git_head"` before
  the first post-upgrade rebuild (e.g. a linked worktree with `"git_head": null` and
  `"git_head_state": "resolved"`).  Additive — all pre-existing keys are byte-identical
  on upgrade; the key is absent from the `{"error": "no index found"}` object (AC21).

- **`temporal_state` and `staleness` in `skim search --stats --json`** (#414) — the
  `--stats --json` object now includes two additional keys:
  - `"temporal_state"` — health of the temporal database at query time.  Values:
    `"ready"` (populated and consistent), `"empty"` (present but zero hotspot rows),
    `"corrupt"` (SQLite-level corruption; DB discarded and one rebuild attempt follows),
    `"newer-schema"` (written by a newer binary; refused — upgrade skim to access it),
    `"missing"` (no `temporal.db` found).
  - `"staleness"` — lexical index staleness at query time.  Values: `"current"` (index
    HEAD matches current HEAD and working tree is unchanged),
    `"stale (HEAD changed: <prev8>…→<cur8>…)"` (HEAD changed since last build),
    `"stale (no HEAD recorded)"` (manifest has no stored HEAD, or an artifact is missing
    or below its current format version), `"stale (working tree changed: N modified,
    N added, N removed)"`.
    `--stats` reports on an index and never creates one: with no `index.skidx` present it
    still prints `{"error":"no index found","cache_dir":"<path>"}` and exits 1 (unchanged),
    so the internal cold-start verdict `"no index"` is not among the values this key can
    take.
  **Snapshot asymmetry (AD-414-10):** `git_head`, `temporal_state`, and `staleness` are
  captured from the PRE-self-heal state.  All other fields (`file_count`, `skipped`,
  `ast_coverage`) are from the POST-self-heal state.  A `temporal_state` of `"missing"`
  or `"corrupt"`, or a `staleness` of `"stale (no HEAD recorded)"`, can therefore coexist
  with a valid `file_count` — this is the intended observable contract.  Additive — all
  pre-existing keys are byte-identical on upgrade.

- **`degraded` array in `skim search --json` when temporal ranking is unavailable** (#414) —
  when `--hot`, `--cold`, `--risky`, or `--blast-radius` is requested but temporal data
  cannot be served (DB missing, corrupt, newer-schema, empty, or repository mismatch),
  the `--json` envelope now includes a `"degraded"` array.  Each element is an object
  with keys: `subsystem` (always `"temporal"` in this release), `reason` (`"missing"`,
  `"corrupt"`, `"newer-schema"`, `"empty"`, or `"mismatch"`), `requested` (the flag that
  was requested), `applied` (`"lexical"` on text-query arms including text +
  `--blast-radius`; `"none"` on standalone temporal and standalone `--blast-radius`
  arms — no results served there; `"ast"` reserved for #483, not emitted today),
  `message` (human-readable),
  `remediation` (actionable hint).  The field is absent (`skip_serializing_if`) on
  healthy queries to keep byte-identity for existing callers.

### Fixed
- **`skim search --build` on a repository with no commits now reports its empty
  history** (#414) — `skim search --build` (and `--rebuild` / `--update`) in a freshly
  `git init`-ed repository with files but no commits printed only the `indexed N files`
  line: no `temporal.db` was created and no temporal notice was emitted at all.  An
  unborn HEAD resolves to no SHA, and the temporal orchestrator returned on that absence
  before any temporal code ran, so the "no commits" case of the zero-row build notice was
  unreachable from every CLI arm.  A repository with no commits is an *empty history*,
  not a build failure: the explicit build arms now write a present-but-empty
  `temporal.db` and print one stderr line naming the cause
  (`temporal data is empty (0 rows) — this repository has no commit history skim can
  analyse`), which — correctly — does **not** mention `shallow` on a non-shallow
  repository.  The empty database records no `git_head`, so the first query after the
  repository's first commit rebuilds it automatically.  Plain queries are unaffected and
  stay silent.
- **`skim search` linked worktree HEAD resolution and temporal data** (#413) — in a
  repository with linked worktrees (`git worktree add`), `skim search` read HEAD from
  the per-worktree `.git` file and then attempted to resolve the symbolic ref in the same
  directory, finding nothing.  The result was `git HEAD : (none)`, a zero-byte temporal
  database, and `--hot`/`--cold`/`--risky` serving no temporal data in the linked worktree
  even though the history was fully present in the primary clone.  Fixed by following the
  `commondir` pointer from the per-worktree git dir to the shared repository, resolving the
  ref in the common directory's ref namespaces (loose refs then `packed-refs`).  Also fixes
  `--install-hooks`/`--remove-hooks` in a linked worktree (previously died with
  `Not a directory (os error 20)`): hooks now route to the shared `<commondir>/hooks`
  directory and both commands disclose the shared scope on stderr.  First-query cost after
  upgrade: one rebuild of `temporal.db` (previously the DB was absent or frozen).
  `--root <subdirectory-of-a-repo>` now also adopts the enclosing repository's HEAD
  instead of reporting no temporal data.
- **`skim search` symbolic-ref path validation tightened** (#482) — a `HEAD` file
  containing `ref: refs/../../../outside-sha` (a path that starts with `refs/` but
  escapes the git directory via `..` components) was read and its out-of-tree SHA was
  persisted into `index.skfiles` and `temporal.db`.  Fixed by applying the ADR-008
  canonical guard (`is_repo_relative_safe`) in addition to the existing `refs/` prefix
  check, so any symbolic-ref path containing `..`, an absolute root `/`, or a Windows
  prefix is rejected before the file is opened.

- **`skim search --rebuild` was a silent no-op when `temporal.db.build_backoff` was set**
  (#414, AD-414-16) — a per-HEAD backoff sentinel (`temporal.db.build_backoff`, written
  when a temporal build fails for a given HEAD commit) was checked unconditionally, so
  an explicit `--rebuild` was silently short-circuited as if it were an automatic quiet
  retry.  Fixed: `BuildLoudness::Loud` (the `--rebuild`/`--build`/`--update` path) now
  clears the sentinel at the start of the temporal rebuild (before the backoff gate),
  so an explicit rebuild always proceeds regardless of prior failures.  `--build` and
  `--rebuild` reach the clear unconditionally; `--update` reaches it only when
  `auto_refresh_if_stale` decides a rebuild is warranted.  Only the automatic quiet
  refresh path (`BuildLoudness::Silent`) still respects the sentinel to bound noisy
  retry loops.

- **`temporal.db` not rebuilt after SQLite-level corruption** (#414) — when
  `temporal.db` contained SQLite-level corruption (`SQLITE_CORRUPT` or `SQLITE_NOTADB`),
  skim now discards the corrupt file and immediately performs one rebuild attempt.
  Previously the corruption was reported in `temporal_state` but the file was left in
  place and no rebuild was triggered, leaving the temporal arm permanently degraded.

- **`--stats --json` emits a notice when `temporal.db` exists but has zero hotspot
  rows** (#414) — an existing but empty temporal DB (all hotspot rows deleted or a
  repo with only one commit) now reports `temporal_state: "empty"` and emits a
  degraded notice so callers know why temporal ranking returned no results.
- **`skim search --near` silently dropped when `--phrase` was also set** (#403):
  Two independent layers each had the same bug: reader.rs used `if want_phrase { phrase_alignments } else { near_match }`,
  and query.rs used `if phrase { Phrase } else if near { Near }`.  In both cases `--near N`
  was completely ignored when `--phrase` was present.  Fix: both layers now use an exhaustive
  `(phrase, near)` tuple match with a new `PhraseNear(N)` arm, so the composed semantic
  (ordered + total span ≤ N) is correctly dispatched at the posting-intersection layer and
  at the verify/CLI-gate layer. (#403)
- **`skim search --hot/--cold/--risky/--blast-radius` now honors `--offset`** (#404) —
  Standalone temporal paths silently ignored `--offset` because `limit` was threaded as a
  bare `usize` at the dispatch site and `offset` was never passed into `run_temporal_standalone`
  at all.  Fix: `Page{limit, offset}` is now propagated into `query_standalone` (all four
  temporal arms), `format_temporal_text` (page-aware headers and empty-page messages), and a
  bounded-page stderr notice is emitted when `has_more=true` so agents can detect the last
  page without the unsound `len < limit` heuristic.  `has_more` is also present in the JSON
  envelope on standalone temporal queries.  At `--offset 0` output is byte-identical to
  pre-#404 behavior for inputs with distinct temporal scores.  Equal-score results follow
  an updated path-ASC tiebreak (resolution 8) that differs from the implicit path-DESC
  ordering the old `.reverse()` produced — a negligible edge case in practice.

### Removed
- **`skim search index` legacy positional subcommand** (#375) — the bareword `index`
  as a leading positional to `skim search` was removed.  `skim search index` now
  runs a **lexical query** for the word "index" (exit 0, returns matching files) rather
  than building the index.  Builds go exclusively through the flag surface:
  `skim search --build` (incremental), `skim search --rebuild` (full rebuild from
  scratch), and `skim search --update` (refresh if stale).  A cold `skim search index`
  on a fresh project auto-builds the index before running the query (existing
  self-heal behavior — no change needed).  Historical `skim search index --rebuild`
  calls in previous release notes refer to past release behavior and are unchanged.

### Changed
- **`skim search --blast-radius` temporal ranking now uses co-change Jaccard strength** (#409) —
  The co-change POPULATION change (full DAG walk, merge commits skipped) shipped in #407 and is
  not re-described here.  This entry covers the RANKING change only.

  Prior to #409, the temporal RRF axis of a composite `skim search <text> --blast-radius FILE`
  query (including `--weights` variants) discarded each co-change partner's Jaccard score and
  assigned every partner a uniform temporal score of 1.0, sorted by internal FileId (which is
  the alphabetical index position of the file path in the manifest).  The result was that the
  co-change-strongest partner could rank last while the alphabetically-first file ranked first,
  regardless of co-change strength — a silent-degradation defect (ADR-009).

  After #409:
  - Each co-change partner carries its actual Jaccard co-change strength as its raw temporal score.
  - The blast-radius target (the `--blast-radius FILE` argument) ranks **first** in the temporal
    layer via a finite sentinel `SEED_STRENGTH = 2.0` (> maximum Jaccard of 1.0 by construction).
  - `merge_layer_scores`' per-layer sort is now a total comparator (score DESC, FileId ASC),
    eliminating any non-determinism from equal-score ties.
  - Unindexed co-change partners (paths recorded in `temporal.db` but absent from the lexical
    manifest — e.g., files outside a `--root` subtree) are now disclosed on stderr with a count
    of dropped partners (excluding the seed); exit code remains 0.

  **User-visible consequences:**
  - `skim search <text> --blast-radius FILE --limit 1 --json` now returns the **seed** (`FILE`
    itself), not the alphabetically-first co-change partner.
  - `skim search <text> --blast-radius FILE --hot` (and `--cold`, `--risky`) may return a
    **different set** because the hotspot re-sort window is drawn from the newly-ordered fused
    ranking, not the old alphabetical ordering.  This is a correctness improvement, not a
    regression: the window now reflects the strongest co-change partners rather than the
    alphabetically-earliest ones (AC-19 user-visible consequence).

- **`skim search` single-token queries now use AND-intersection + raw occurrence-count ranking** (#372) —
  Prior to this change, all lexical queries used a BM25F UNION pool: candidates ranked by BM25F score,
  which divides term-frequency by field length, penalising large files.  For single contiguous tokens
  (≥ 3 bytes, no interior whitespace — the common identifier search), the engine now:
  (1) AND-intersects the query's trigram posting lists (grep-exact, corpus-size-independent recall),
  (2) ranks surviving files by raw occurrence count (length-norm-free, AD-372-6) so large-file definers
  with multiple references are not buried by small stubs that have 1 occurrence in a tiny file.
  BM25F UNION is unchanged for multi-word queries.  The verify gate (literal substring membership)
  is preserved on both paths.  Result ordering for single-token queries may change after upgrade.
- **`skim search` lexical results are now filtered to files literally containing the query** (#355) —
  Prior to this change the n-gram index returned candidates ranked by BM25F score, but no
  literal-substring check was performed: a file could appear in results even if none of its
  bytes contained the exact query string (false positives from bigram-overlap noise).  A
  candidate-then-verify gate now reads each candidate file once and confirms the query string
  is present as a literal substring before the result is emitted.  Files that pass the trigram
  index but fail the literal check are silently dropped.  **Short queries (< 3 bytes)**:
  queries too short to generate any trigrams now trigger an all-files fallback (AD-355-7);
  the same verify gate applies, so only files literally containing the 1–2 byte query are
  returned.  Scores for short-query results are 0.0 (no trigram ranking possible).
- **`skim search --ast` output format** — text rows now render the score as `[N]`
  (previously `score: N`) and append a `:line` suffix plus a source snippet when the
  matched pattern node is line-recovered; degraded (non-recovered) rows omit both. JSON
  output gains optional `line` and `snippet` fields — additive, so existing consumers of
  `path`/`score` are unaffected. (#201)
- **`--root <subdirectory-of-a-repo>` now resolves the enclosing repository** (#413) —
  a `--root` that is a subdirectory of a git repository adopts that repository's HEAD
  (bounded ancestor walk, nearest match) and therefore builds real temporal data, where
  it previously reported `git HEAD : (none)` and `no temporal data`.  The temporal rows
  are scoped and re-anchored to the indexed subtree, so one `--root` is one result
  universe: every temporal arm returns only paths inside the subtree, and `--blast-radius`
  co-change peers outside it are dropped.  Pass the repository toplevel as `--root` for
  repository-wide ranking.  The repository that produced the rows is recorded as
  `meta.git_toplevel`; if the enclosing repository later changes, temporal arms refuse
  (no rows served, `temporal.db` byte-unchanged, exit 0) instead of silently rescoring —
  `--build`/`--rebuild`/`--update` re-anchor and disclose the old and the new toplevel.
  A subdirectory root gains its own `temporal.db`, so its first build walks history once.
  Staleness for adopted roots is scoped to a working-tree metadata scan (mtime + size)
  rather than a HEAD-SHA comparison: a commit that touches only files OUTSIDE the
  subtree does not trigger a rebuild of the subdirectory index, because the repo-wide
  HEAD advancing is not evidence that the subtree changed.
- **`skim search --install-hooks`/`--remove-hooks` now resolve git's own hooks directory**
  (#413) — the hooks path is resolved through the `commondir` chain (matching
  `git rev-parse --git-path hooks`) instead of being hand-built as `<root>/.git/hooks`.
  In a linked worktree this is the shared `<commondir>/hooks`, so installing or removing
  from one worktree changes hook behaviour for **every worktree** of the clone; both
  commands print the absolute resolved directory and disclose that scope on stderr, and
  `--remove-hooks` no longer prints a success line when nothing was removed.  Plain
  repositories and non-repository roots resolve to the same path as before.
  A `--root` that is a subdirectory of a git repository but has no `.git` entry of
  its own is **refused** — the command exits with an error naming the enclosing
  repository so the caller can re-run with the correct `--root`.
- **`skim search` temporal-arm degradation messages are now reason-specific** (#413) —
  previously, all "no temporal data" conditions on `--hot`/`--cold`/`--risky`/
  `--blast-radius` (and the temporal path of `--ast`) emitted a single generic message:
  `"no temporal data — run 'skim search' on a git repo to auto-populate"`.  That
  message is now reserved for the non-git case (root is not inside any git repository).
  Three reason-specific messages replace it for the remaining conditions:
  (1) **Unresolved HEAD** (unborn branch or reftable backend): starts with
  `"git HEAD could not be resolved to a SHA"`;
  (2) **Resolved HEAD, empty temporal build** (no git-log entries for the root):
  starts with `"git HEAD resolved but the temporal build produced no data"`;
  (3) **Resolved HEAD, anchor mismatch** (on-disk `temporal.db` was built from a
  different repository): starts with `"temporal data on disk was built from a
  different repository"`.
  Scripts or automation that matched the previous generic text will not match
  conditions (1)–(3); the stable prefix of each new message is the named constant
  in source (`HEAD_UNRESOLVED_TEMPORAL_MSG`, `TEMPORAL_BUILD_EMPTY_MSG`,
  `SUBDIR_ROOT_TEMPORAL_MSG`) for tests that need a reliable assertion target.

### Fixed
- **`skim search --rebuild`/`--build` now populate `temporal.db`** (#357) — explicit
  rebuilds previously produced only the lexical+AST index; temporal/co-change data
  (`--hot`/`--cold`/`--risky`/`--blast-radius`) was silently unavailable until the
  next auto-refresh cycle.  The rebuild path now calls `rebuild_temporal` after
  `build_index`, producing a complete index in one step.
- **`--hot`/`--cold`/`--risky`/`--blast-radius` self-heal a stale or missing `temporal.db`**
  (#357) — when the lexical index was Current but `temporal.db` was absent or its stored
  `META_GIT_HEAD` diverged from the current HEAD (e.g. post-upgrade, manual deletion, or
  a first rebuild after the above fix landed), these flags silently returned degraded
  output.  `auto_refresh_if_stale` now checks temporal staleness independently of lexical
  staleness and self-heals before the early-return.  Extends the #289 temporal auto-refresh
  to cover this previously-unguarded gap.  Non-fatal: a temporal failure never fails the
  lexical search (ADR-006/D5).
- **`skim search <text> --ast <pattern>` compound intersection no longer silently drops
  valid matches** (#356) — the old `CANDIDATE_POOL_K = 4` multiplier capped the lexical
  candidate pool at `limit * 4`, so files ranking beyond position `limit * 4` in the
  unfiltered lexical list were invisible to the intersection even when they satisfied both
  the text query and the AST pattern.  The lexical pool is now restricted to the exact
  AST-matched file set (`file_filter = AST set`, `sq.limit = |AST set|`), making the
  compound intersection complete by construction.  Part of the #198/#200 compound
  text+AST path.
### Changed
- **`skim proxy` is now gated behind a non-default `proxy` cargo feature** — default builds are
  HTTP/TLS-free (AC9); release binaries include the proxy. Source builds opt in via
  `cargo build -p rskim --features proxy` or `cargo install --path crates/rskim --features proxy`. (#352)
- **Proxy egress compression is now lossless-only (#427)** — All active engines on the
  `skim proxy` egress path are information-preserving:
  - **JSON minification** — structural whitespace removal only; value-equivalent, dup-key-safe,
    runtime-gated via `value_equivalent_raw` (budget: 200k nodes/request). Blocked on
    budget exhaustion or dup-key detection → byte-identical passthrough.
  - **Log deduplication** — annotated dedup with ×N counts and timestamp min–max ranges;
    all unique content preserved. Log header arithmetic fix: X lines → Y unique (Z duplicates
    removed) now correctly satisfies X−Z=Y.
  - **Code blocks** — pass through byte-identical on the proxy path (tree-sitter removed
    from the egress pipeline entirely). Measured win: ~14.6 ms → ~0.6 ms per 90 KB block.
  - **Policy tiers re-scoped** — `Policy::LosslessOnly` = byte-exact passthrough (no re-encoding,
    Subscription/Bearer auth); `Policy::Default` = lossless re-encoding allowed (ApiKey auth).

### Removed
- **Lossy egress transforms removed (#427)** — The following content-discarding egress
  transforms are removed from the proxy path:
  - Code-body stripping via tree-sitter (replaced by byte-identical passthrough; latency
    improvement ~14.6 ms → ~0.6 ms per 90 KB block).
  - JSON value placeholders (replaced by minification or byte-identical passthrough).
  - Content-discarding log axes.
  - Tabular re-encoding is explicitly excluded from v1 (tracked in #430 with evidence gate).
  - Per-block agent-consent code compression deferred to #429.

### Breaking Changes

- **Exit-code remap: parse errors → 2, unsupported language → 3** — Scripts that
  branched on a specific non-zero exit code (not just `!= 0`) may need updating.
  See **Changed** below for full details.
- **Unknown-extension files now degrade to lossless passthrough (exit 0)** —
  Files with an unrecognised extension or unrecognised shebang previously produced
  a non-zero exit; they now fall back to byte-faithful passthrough (exit 0,
  applies ADR-002). Scripts that relied on a non-zero exit to detect unrecognised
  input must instead inspect the output. See **Fixed** below for full details.
- **`skim -` (stdin) without a language hint now exits 0** — Bare stdin with no
  `--language` flag, no `--filename` hint, and no recognisable shebang previously
  errored (non-zero). It now degrades to lossless passthrough (exit 0,
  applies ADR-002). See **Fixed** below for full details.
- **`skim grep`/`rg` output format changed to native passthrough — grouped renderer removed (ADR-009)** —
  `skim grep` and `skim rg` now emit output byte-identical to raw `grep`/`rg`
  (one `path:line:content` line per match), replacing the previous grouped
  file-header-per-match envelope. **Consumers that parsed the grouped format (bare file
  path lines followed by indented `:line: content` entries) must switch to the native
  `path:line:content` shape.** Tabs and leading whitespace are preserved. Line-count
  consumers (`| head -N`, `| wc -l`, `| sed -n`, `awk NR<=`) now see line-count ==
  match-count as expected.
- **`skim git log` silent 20-commit cap removed (ADR-010)** — `skim git log` no longer
  injects a silent `-n 20` limit on invocations without an explicit count flag.
  **Scripts or agents that relied on log output being bounded to 20 commits will now
  see the full log.** Explicit caps (`-n N`, `--max-count=N`, rev-ranges such as
  `HEAD~N..HEAD`) still work as supplied. Note: `git log -p` on large repos may
  approach the 64 MiB output ceiling.
- **`wc`, `df`, `du`, `find`, `ps` output format changed to native passthrough (ADR-009)** —
  These wrappers now emit output byte-identical to the raw tool, including control bytes:
  TAB (0x09) column separators and ESC (0x1b) sequences are no longer stripped. This replaces
  the previous `<tool> N` header/entry envelope and its silent 100-entry display cap. `du`'s
  POSIX `size<TAB>path` format is preserved, and an ESC byte anywhere in the output — in a
  colorized path or in a file name — no longer destroys the tabs on every line. Two documented
  limits remain: output is decoded as lossy UTF-8, so non-UTF-8 bytes in path names become
  U+FFFD, and a trailing newline is appended when the tool's output does not end with one.
  Measured impact of the old path: `find crates -name '*.rs'` lost 355 of 457 paths; `ps aux`
  dropped 705 of 805 processes and produced output 180 bytes larger than native for the records
  it did show; `wc` reformatted `      300 total` into ` total: 300`,
  silently breaking `| tail -1 | awk '{print $1}'` pipes. **Consumers parsing the old envelope
  format must switch to native output.** For `ps` specifically, truncation was the only mechanism
  reducing output volume — callers wanting fewer rows should pipe through `head`.
- **`skim ls` output format changed to native passthrough (ADR-009)** — `skim ls` now emits
  output byte-identical to raw `ls`, including the native `total <blocks>` header and control
  bytes (TAB, ESC) — `ls -G` / `CLICOLOR_FORCE=1 ls` color sequences are preserved, subject to
  the same lossy-UTF-8 and trailing-newline limits noted above. The previous path silently
  dropped 102 of 202 entries at the display cap and omitted the `total` header.
  **`tree` is unchanged** and still compresses. Consumers parsing the old skim-formatted `ls`
  output must switch to native format.

### Added
- **`skim doctor` subcommand** — Reports the running binary (absolute path + commit),
  every `skim` binary on `$PATH` with its commit and which one wins, hook pin state for
  each installed agent (pinned commit vs running commit, staleness verdict), the wrapper
  directory, and the cache/analytics database locations. Exit `0` when no drift is
  detected; exit `1` on any drift, so the command works as a CI pre-flight check. Commit
  resolution reads from `--version` output (`skim x.y.z (sha)`) rather than a `--commit`
  flag, so it correctly identifies binaries that predate the doctor subcommand itself.

- **Transparency marker for hook-rewritten file reads** — When the PreToolUse hook
  rewrites `cat`/`head`/`tail` on a code file into `skim <file> --mode=pseudo` (or
  `--mode=structure` for declaration files), the rewritten command now carries a
  `SKIM_REWRITTEN_FROM=<cat|head|tail>` env token. After transformation, if the served
  view differs from raw file bytes, skim emits a one-line stderr notice:
  `[skim] transformed view (cat → skim --mode=pseudo): not raw file bytes — SKIM_PASSTHROUGH=1 for raw output`.
  Multi-file: one aggregate line (`2/3 files not raw bytes`). The marker is silent for
  byte-identical outputs (guardrail passthrough, `--mode=full`), direct `skim` calls
  (no tag), and unknown-value env vars (closed-vocabulary guard prevents injection).
  Cache hits also emit the marker when the cached view differs from raw.
- **Agent guidance documents command wrapping** — The guidance injected by `skim init`
  now includes a "Command wrapping" section explaining that the rewrite hook may run
  supported commands as `skim <tool>` (same arguments, same exit code, compressed
  output). Guidance wording corrected: file reads (`cat`, `head`, `tail`) are rewritten
  into direct skim reads showing a structured view (not raw contents), and skim prints
  a one-line stderr notice whenever the served view differs from the raw file.
  The previous claim "does not change what the command did" has been replaced with
  accurate wording. Agents are instructed to flag garbled or incomplete compressed
  output to the user rather than silently working around it. Existing installs pick up
  the new section on the next version bump + re-pin (per ADR-004; binary-side fixes
  below are effective immediately after rebuild).
- **Bash / shell language support** — Full tree-sitter-bash grammar integration
  (`Language::Bash`). Structure mode strips `function_definition` bodies to `{...}`
  while preserving function names and all top-level commands/variable assignments.
  Zero-function scripts (pure top-level commands like deploy.sh) render meaningfully:
  all commands and variable assignments are visible. Shebang auto-detection recognises
  `#!/bin/bash`, `#!/bin/sh`, `#!/usr/bin/env bash`, and the `env -S` form; supports
  `dash`, `zsh`, `ksh`, `mksh`, `fish` dialects. CRLF-tolerant (`\r\n` shebangs work).
  Also detects shebangs for `python`, `node`, and `ruby` on extensionless files.
  `--language bash` (alias `sh`) available for explicit override. Unknown-extension
  files with an unrecognised shebang degrade to lossless passthrough (ADR-002) with a
  `SKIM_DEBUG=1` notice; all 6 modes have explicit Bash arms.

### Fixed
- **`skim doctor` exit-code contract changes (#488)** — Two user-visible changes
  to which conditions drive `skim doctor`'s exit 1:

  - **Binary pin mismatch is now advisory (⚠), not drift (✗)** — Previously, when the
    hook script pinned a different binary path than the one currently running (same
    version and commit, different absolute path — the two-clone scenario), `skim doctor`
    exited 1. It now exits 0 and prints a `⚠` advisory line, because the running binary
    is identical in every meaningful way. **If you use `skim doctor` as a CI pre-flight
    to catch wrong-clone issues, note that this signal has been demoted.** The wrapper
    surface (below) now contributes to exit 1 instead, providing an equivalent signal
    via a different mechanism.

  - **Compiled SHA absent from the current repo no longer exits 1 (fix)** — When a user
    ran `skim doctor` inside their own project (not the skim source repository),
    `git cat-file -e <sha>^{commit}` failed in their repo (the sha has never been there)
    and doctor incorrectly exited 1. The staleness section now returns neutral (`–`) for
    an absent SHA rather than treating absence as drift.

  - **Wrapper target drift now exits 1 (new signal)** — `skim doctor` now resolves each
    wrapper symlink in `~/.skim/bin/` with `read_link` and compares the canonical target
    against the running binary. A wrapper pointing at a stale or foreign skim binary now
    reports `✗` and exits 1, partially restoring the wrong-clone detection signal removed
    by the pin-mismatch demotion above. Foreign symlinks (target stem ≠ `skim`/`rskim`)
    are reported as `⚠` and do not affect the exit code.

- **`skim init` now REPAIRS tampered hook scripts instead of laundering them** — When
  `skim init` ran on an install where the hook script had been manually edited (tampered)
  but the version/commit markers were unchanged, the previous self-heal path would
  re-hash the on-disk tampered bytes and write them into the integrity manifest, so a
  subsequent `skim doctor` would report Verified — for the wrong content. The installer
  now classifies script integrity before deciding whether to return early: a `Tampered`
  verdict falls through to the full regeneration path (restoring the known-good script
  content), while `Verified`/`NoManifest` continue to return early as before (applies
  PF-016, ADR-004).

- **`skim doctor` now detects tampered hook scripts (#471)** — Previously, doctor
  derived its hook-health verdict entirely from `SKIM_HOOK_*` markers parsed out of the
  hook script text, so a tampered or manually edited script could still pass as healthy
  (the verdict came from the very bytes under test). Doctor now consults the SHA-256
  manifest written at install time — an independent artefact the hook script cannot
  influence — before checking pin/currency state. A mismatched hash exits 1 and names
  the suppression coupling ("a failed integrity check also silences drift detection on
  this agent's hook channel") so users know both output channels are untrusted.
  **Intended behaviour change:** `skim doctor` now iterates all supported agents
  (`AgentKind::all_supported()`) for integrity checks, while hook-time integrity
  verification remains ClaudeCode-only (`rewrite/hook.rs`). Machines with hand-edited
  codex, gemini, cursor, copilot, or crush hook scripts will **newly report ✗ on
  `skim doctor`**; run `skim init --agent <name>` to reinstall and clear the flag.
  Pre-manifest installs (no `.sha256` sidecar) remain advisory (`⚠`) and do not drive
  exit 1 — backward compatibility is preserved for installs predating the manifest feature.
  **Widened (#471 follow-up):** the `NoManifest` path now falls through to the pin/currency
  checks rather than returning early, so drift in the hook version or binary pin is still
  reported even when the manifest is absent.  The advisory message is appended to the
  pin/currency verdict; drift=false is preserved.

- **`strip_ansi` no longer destroys TABs and other C0 controls when an ESC byte is
  present (#465)** — `strip_ansi()` previously delegated to `strip_ansi_escapes::strip_str`,
  which drives a `vte` state machine that emits only printables + `\n`, silently discarding
  ALL C0 control bytes including TABs whenever a single ESC byte appeared anywhere in the
  buffer. A single colour code on line 2 would destroy the `\t` column separators on lines 1
  and 3. The function now uses `strip_escape_sequences` — the same ESC-scoped scanner already
  used by `strip_ansi_cow` — which removes only ESC-rooted sequences and leaves every other
  byte unchanged. **Scope:** both `strip_ansi()` and `strip_ansi_cow()` were affected —
  `strip_ansi_cow`'s no-ESC fast path only skipped the *allocation*, and once any ESC byte
  was present it fell through to the same vte stripper. Both now route to the new scanner,
  which covers every caller of
  `strip_ansi()`: cargo/make/gradle/maven/tsc build output, pytest/jest/vitest combined output,
  `gh` streaming lines, `git status` raw baseline, and vitest regex parsing. TABs, BEL, BS,
  VT, FF, and CR bytes in build/test/VCS output now survive ANSI stripping in all paths.
  The `strip-ansi-escapes` crate dependency is removed from the workspace (no remaining callers).

- **`strip_escape_sequences` unterminated-sequence safety (#317 / #465 follow-up)** —
  Previously, an unterminated CSI (`ESC [` with no final byte before end-of-input) or OSC
  (`ESC ]` with no BEL or ST before end-of-input) silently discarded all remaining content
  in the buffer — a "compress, never truncate" violation. Both loops now cap at 2 KiB and
  emit the consumed bytes **literally** rather than dropping them when the cap is exceeded or
  the input ends mid-sequence.  Additionally, the bare-ESC arm no longer consumes the
  following byte unconditionally: only bytes in `0x20..=0x7e` (valid 2-byte-sequence second
  bytes) are consumed, so a lone ESC before `\n` no longer merges two lines.  The same
  guarantee now covers bytes that cannot legally appear *inside* a sequence body: a CSI byte
  outside `0x20..=0x3f`, or a C0 control other than BEL/ESC inside an OSC body, ends the scan
  and emits the consumed bytes literally.  Without this, a malformed `ESC [ 3 2` kept scanning
  past the line break to the first byte in `0x40..=0x7e` — the next line's first letter —
  swallowing the newline and that letter, merging two lines.

- **`skim git show` reads AST breadcrumb source from the commit, not the working tree
  (#467)** — `git show <ref>` rendered its AST breadcrumbs from whatever the file looks like
  on disk *now*, so any uncommitted edit produced breadcrumbs describing symbols that do not
  exist in the commit being shown. The rendering path now takes an explicit `is_show` intent
  parameter (rather than re-sniffing argv) and resolves the blob at the shown ref; `git diff
  A B` likewise resolves the new-side ref. Independently, a `source_matches_diff` backstop
  verifies every context and added line against the resolved source before any breadcrumb is
  emitted — on mismatch the file falls back to raw hunks. This catches what the ADR-001
  net-savings guard cannot: a wrong-revision render can be *smaller* than raw and so passes a
  size check while showing wrong content. The fallback is a no-loss raw fallback and is
  therefore `SKIM_DEBUG`-gated per ADR-011. `git diff <ref> <path>` written without `--`
  (e.g. `git diff HEAD src/foo.ts`) still resolves to the working tree.

- **`SKIM_WRAPPERS_DIR`** — new override for the `~/.skim/bin/` wrapper symlink directory
  used by `skim init --wrappers` and `skim init --uninstall`; an empty value is treated as
  unset. Added so the test suite can confine wrapper installation to a sandbox instead of
  mutating the developer's real home directory (#472). `InstructionEnv` now also honours
  `GEMINI_CONFIG_DIR` and `COPILOT_CONFIG_DIR` when resolving guidance files, matching the
  `DetectionEnv` behaviour those variables already had — previously `skim init --uninstall`
  could remove the real `~/.gemini/GEMINI.md` even under a redirected config dir.

- **`skim init` self-heals a missing SHA-256 manifest (#471 follow-up)** — Previously, if
  the `.sha256` sidecar was absent (e.g. due to a transient write failure at install time,
  a backup tool removing it, or a read-only filesystem edge case), re-running `skim init`
  on an already-current install would hit the "already up to date" fast path and return
  without restoring the manifest — making `skim doctor`'s advice ("run `skim init --agent
  {agent}`") a no-op.  Fixed by: (1) adding a manifest-presence check to the fast path so
  a missing manifest bypasses it; (2) writing the manifest on the "already current" script
  path in `create_hook_script` before printing "Skipped"; (3) propagating manifest write
  errors with `?` instead of swallowing them silently (the error names the manifest path and
  says the hook directory must be writable).  Manifest removal during uninstall now also runs
  outside the `if script_exists` guard, so a sidecar left behind by a hook whose script file
  was deleted out from under it is cleaned up along with the registration.  (An uninstall
  still short-circuits when neither the registration nor the script exists, so a fully
  orphaned sidecar is left in place — harmless, since re-installing always rewrites it.)

- **`skim init --yes` now re-pins the hook script after an in-place rebuild at the same
  version (#466)** — `hook_is_current()` previously compared only the binary path and
  version string, so a `cargo build` that incremented the commit SHA while keeping the
  same `x.y.z` version was treated as "already up to date" and the stale commit pin was
  left in place. The predicate now additionally compares `SKIM_HOOK_COMMIT` against the
  compiled-in SHA, so any rebuild triggers a re-pin.

- **`skim init` now fails loudly if it cannot resolve its own binary path** — A
  `.unwrap_or_default()` fallback in `generate_hook_script` would silently produce a hook
  script with an empty `SKIM_HOOK_BINARY` value, writing an unpinned script with no
  error. This is replaced by a loud `Result` failure and an empty-path assertion, so the
  error surfaces before any script is written.

- **`SKIM_DEBUG=1` startup provenance line routes to `hook.log` in hook mode, not
  stderr** — In prior versions, enabling `SKIM_DEBUG` while running under the hook emitted
  a startup notice to stderr, violating the GRANITE #361 Bug 3 requirement (skim must
  never write to stderr in hook mode). The startup line is now routed to `hook.log` when
  the hook context is active, keeping stderr byte-clean.

- **`skim git status` branch header now mirrors native `git status -sb` format** — The
  branch line now renders as `## branch...upstream [ahead N, behind M]` (matching native
  `git status -sb` output exactly), with counts derived from the `# branch.ab`
  porcelain-v2 field. A missing `# branch.ab` line (upstream ref deleted) renders
  `[gone]`. Previously the bracket format differed from native git output.
- **`skim diff` now includes unified patch content alongside file statistics** — In
  addition to the per-file `+N,-M` stat header, the diff renderer now emits the actual
  changed lines (unified patch body). Files beyond the display cap produce an elision
  marker with exact counts and a `SKIM_PASSTHROUGH=1` hint for lossless access
  (applies ADR-011).
- **No-loss raw-fallback stderr banners now gated behind `SKIM_DEBUG`/`--debug` (ADR-011)** —
  Notices that fire when skim chose to emit raw bytes (guardrail chose raw, unexpected
  tool exit, tool killed by signal) are now silent by default and appear only when
  `SKIM_DEBUG=1` (or `--debug`) is set. **Loss-bearing elision markers** (truncation
  notices with exact counts and a `SKIM_PASSTHROUGH=1` hint) and the ADR-008 lossy-view
  transparency marker **remain unconditional.** Set `SKIM_DEBUG=1` to restore diagnostic
  banner output.
- **Fileops dispatcher no longer intercepts tool-level `-h` as help** — `file/mod.rs`
  narrowed its help guard to `--help` only, mirroring the `db/mod.rs` hostname-flag
  precedent. `grep -h` (no-filename), `ls -h`/`du -h`/`df -h` (human-readable sizes)
  now reach their handlers instead of printing skim's fileops help. `rg -h`/`rg --help`
  added to rg's rewrite skip-list (for rg, unlike grep, `-h` is a genuine help flag;
  without the skip, `rg --help` would be rewritten and show skim's help instead of rg's).

- **Pseudo mode preserves function return types as API surface (A4 contract)** —
  Return type annotations are API contracts callers depend on; they are now preserved
  in pseudo mode alongside visibility modifiers, mirroring the commit c244a12
  visibility fix. For Python and TypeScript, param, variable, and property type
  annotations are still stripped. Rust preserves parameter types (pseudo mode strips
  only lifetimes, type parameters, where clauses, and attributes for Rust).
  Affected languages: Python (`-> T`), TypeScript (`: T` at return position), Rust
  (`-> T` via normal recursion — the former strip_rust_return_type special-case is
  removed). A position-aware guard (`is_return_type_annotation`) stops recursion at
  the `return_type` field child, preserving nested generics (`Promise<User>`,
  `tuple[int, str]`) wholesale.

- **`skim init` summary and dry-run output protocol-driven for Copilot and Gemini** —
  `print_install_summary` and `print_dry_run_actions` now accept an explicit
  `AgentKind` parameter and branch on `protocol.uses_dedicated_hook_file()`:
  Copilot CLI (the only dedicated-file agent) prints a "Register hook: …/skim.json"
  / "Would write: …/skim.json" line instead of the incorrect "Patch settings" /
  "Would patch settings.json" wording. Settings-based agents use `hook_event_key()`
  instead of a hardcoded `"PreToolUse"`, so Gemini shows `BeforeTool` and Cursor
  shows `preToolUse` in their respective output lines.

- **`skim gh pr checks` tabs destroyed by ANSI-strip + exit-8 raw-forwarded before
  parse** — `gh pr checks` emits TAB-separated output but gh's `ToolRunConfig` had
  `skip_ansi_strip: false`, so `strip_ansi_escapes` dropped `\t` before
  `RE_GH_CHECK_TAB` could match, causing fall-through to Passthrough. Independently,
  `expected_exit_codes: &[]` classified gh's exit 8 (pending/failing checks) as
  `UnexpectedFailure`, raw-forwarding the output before parsing. Fixed: set
  `skip_ansi_strip: true` (gh emits no ANSI when piped) and `expected_exit_codes:
  &[8]`. Exit 8 is still propagated so callers see the true check state. Blast
  radius: CONFIG is shared by all `run_tool` gh routes; `gh run watch` is unaffected
  (streaming path). A hypothetical gh exit-8 from a non-checks route falls to
  Tier-3 passthrough with exit code preserved.

- **`skim diff` tab-split header fused path to mtime** — `diff -u` emits
  `--- path\t<mtime>` headers and `try_parse_standalone_unified` splits on `\t` to
  extract the path. With `skip_ansi_strip: false`, the tab was dropped by
  `strip_ansi_escapes`, fusing path and timestamp into a single token. Fixed:
  set `skip_ansi_strip: true` (diff emits no ANSI). The ADR-001 net-savings guard
  is undisturbed.

- **`git diff` output could exceed raw size** — The default diff renderer walked the
  full AST node body for each hunk, emitting 2–5× the raw patch size for large files.
  Replaced with a hunk-scoped path (`render_default_scoped`) that emits only the
  container breadcrumb header plus the hunk's own changed lines, so output is ≤ raw in
  all cases. Orphan hunks (changed lines outside all AST node ranges, e.g. EOF
  deletions or between-function additions) render as raw patch lines so no content is
  silently dropped. An ADR-001 guardrail is wired into `run_diff` (text output) so a
  net-expansion is never forwarded.

- **`mypy` produced blank output on a clean run when injecting `--output json`** —
  mypy writes nothing to stdout in JSON mode when there are zero issues. Agents
  received empty output instead of "Success: …". A new `synthesize_success_line`
  knob on `ToolRunConfig` emits a configured line when exit code is 0 and compressed
  output is empty. mypy is configured with `synthesize_success_line = "mypy OK 0
  issues"` and `injected_format_flag = "--output"`. Synthesis is suppressed when the
  user already supplied `--output` themselves. A companion `injected_format_flag`
  field prevents prepare_args from injecting the format flag twice.

- **`ls -R` and multi-path `ls` lost per-directory section structure** — `try_parse_ls_long`
  was a flat single-pass parser that ignored `"dir:"` section headers from `ls -R` and
  multi-path invocations. Empty directories silently vanished (only `total`/`.`/`..`
  lines were present). Fix dispatches to a sectioned parser when the output contains
  section headers, rendering each section with its label header. Empty directories now
  produce a labelled section with 0 entries rather than disappearing.

- **`skim grep`/`rg` matched-content parser removed — native byte-faithful passthrough (ADR-009)** —
  The grouped parser (`try_parse_single_target`, `try_parse_file_line_content`,
  `extract_match_fields`) has been removed along with its content-normalization logic.
  `skim grep`/`rg` now emit output byte-identical to raw `grep`/`rg`, preserving all
  tabs and leading whitespace intact. See **Breaking Changes** for consumer impact
  (applies ADR-009).

- **Hook scripts used a bare `skim` exec that silently ran the wrong binary after
  `skim` was updated or reinstalled** — Generated hook scripts now embed `SKIM_HOOK_BINARY`
  (the canonicalized absolute install-time path) and `SKIM_HOOK_COMMIT` (short git SHA)
  at generation time via a `build.rs` compile-time constant. At runtime the hook
  executes via the pinned binary with a PATH fallback. `hook_is_current()` now checks
  for the `SKIM_HOOK_BINARY` export — hooks generated before this fix are treated as
  stale and `skim init` will prompt to reinstall. A version-mismatch helper warns when
  the running binary differs from the pinned binary SHA.

- **Manually repointing a pinned hook script's binary path triggers an integrity warning**
  — This is expected behaviour. Editing the script bytes by hand changes its SHA-256
  checksum, so the run-time integrity check (`check_hook_integrity`) reports the script
  as "tampered"; a repointed `SKIM_HOOK_BINARY` path also trips the binary-path check
  (`check_hook_binary_mismatch`). Both checks fire at hook run time — the install-time
  currency predicate (`hook_is_current`) is not involved and never yields "tampered".
  Run `skim init --uninstall --force` to remove the modified script (the `--force`
  flag bypasses the tamper guard on uninstall), then `skim init` to regenerate a
  clean script pointed at the current install location (applies ADR-004).

- **`--version` now prints `x.y.z (<shortsha>)`** — The version output includes the short
  git SHA of the build commit (compiled in via `build.rs`). This makes it straightforward
  to identify which exact commit a running binary was built from, which is useful when
  debugging hook-version mismatches reported by `hook_is_current()`.

- **Markdown headings appear in reverse order in structure/signatures output** — The
  `extract_markdown_headers_with_spans` function collected headings via a depth-first
  visit stack (LIFO) which emitted sibling headings in reverse source order; a document
  with `# A`, `## B`, `## C` produced `C → B → A` output. The fix adds a single
  ascending sort on `source_start_line` before the texts/spans/line-map pipeline, so
  headings are always emitted top-to-bottom regardless of tree traversal order.

- **`cargo nextest run` dropped the `run` subcommand token in rewrites** — The rewrite
  rule for `cargo nextest run` was `rewrite_to: &["skim", "cargo", "nextest"]`,
  silently dropping `run`. This caused the dispatch layer to receive `nextest` without
  `run`, fell through to the wrong handler, and also triggered a fragile
  `args.iter().any(|a| a == "nextest")` sniff in the cargo test driver that
  occasionally mis-identified standard `cargo test` runs. Three-layer fix: (a) preserve
  `run` in the rewrite rule; (b) replace the sniff with an explicit
  `runner_args.first() == Some("nextest")` check threaded from the dispatcher (A2
  contract); (c) correct the test-failure output path — nextest writes its entire report
  (including the summary) to *stderr*, leaving stdout empty, so its failures must be
  forwarded raw rather than routed into skim's stdout-keyed compress path (which would
  emit nothing — the net-savings guard baselines against the empty stdout — or
  mis-count, since the embedded per-process libtest line reports a single binary, not
  the whole run). nextest's test-failure exit `100` (distinct from libtest's `101`) is
  therefore deliberately *not* added to the compressible-exit set; every non-zero
  nextest exit forwards the full, accurate report verbatim. (#317 compress-never-truncate)

- **Pseudo mode stripped visibility/export modifiers, losing API surface** — Pseudo mode
  is intended to remove syntactic noise while preserving code semantics. Visibility
  modifiers (`pub`, `export`, `public`, `private`, `protected`, `internal`,
  `fileprivate`, `open` in Swift) are API surface — they affect what callers can see —
  not noise. Removing them silently changed the semantics an LLM reads. The fix removes
  visibility keywords and node kinds from all per-language `PseudoRules` strip lists.
  Non-visibility structural modifiers (`static`, `final`, `abstract`, `virtual`,
  `override`, `sealed`, Kotlin `open`/`data`) remain stripped as before. C++ access
  specifiers (`public:`, `private:`) are similarly preserved. (A4 contract)

- **`ls -la` output double-counted `.`/`..` entries and emitted a redundant header** —
  `try_parse_ls_long` matched the permission-line regex against `.` and `..` dotdir
  entries, inflating the dir count by 2. It also prepended a `"LS: N entries …"`
  summary line before the file list; `FileResult::render` then emitted a second `ls N`
  header, producing two headers in the rendered output. Fixes: skip `.` and `..` before
  counting (trimming the trailing `/` added by `-F`/`-p` before the name comparison);
  remove the prepended summary entry; fold the dir/file breakdown into the footer so it
  reads `"… — D dirs, F files"` when entries are elided or `"D dirs, F files"` when all
  fit. Empty directories (only `total`/`.`/`..` lines) return a well-formed `Full`
  result with 0 entries rather than `None`, preventing Tier-2 from mis-tokenising those
  lines.

- **Unified `SKIM_CACHE_DIR` resolution — honored by all cache subsystems (#359 Phase B)** —
  Previously `SKIM_CACHE_DIR` was silently ignored by the parser cache and the default
  `analytics.db` path (`cache::get_cache_dir` read only `dirs::cache_dir()`) while the
  search index and hook log respected it. This caused partial relocation: some skim state
  moved, some stayed under `~/.cache/skim` (PF-002).

  The fix introduces a single source of truth:
  - `cache::cache_root_from(override_dir)` — pure resolver (no I/O); filters empty paths.
  - `cache::cache_root()` — reads `SKIM_CACHE_DIR` and delegates to `cache_root_from`.
  - `cache::get_cache_dir()` now calls `cache_root()` before the mkdir/chmod block.
  - `cmd::hook_log::CacheEnv::resolve_cache_dir()` now delegates to `cache::cache_root_from`
    instead of its own inline resolver.

  **Behavior change:** `SKIM_CACHE_DIR` now also relocates the default `analytics.db`
  (previously it did not). `SKIM_ANALYTICS_DB` still takes precedence over the relocated
  default when explicitly set. Empty `SKIM_CACHE_DIR` is treated as unset (falls back to
  the platform default `~/.cache/skim`). **Caveat:** pre-existing history at the old
  `~/.cache/skim/analytics.db` is **not migrated** — setting `SKIM_CACHE_DIR` for the
  first time causes `skim stats` to start from an empty DB at the new location; the old
  file remains at `~/.cache/skim/analytics.db` and must be moved manually if you want to
  preserve history.

- **Plain `skim <file>` now always records token-savings analytics (#359 Phase A)** —
  Previously a plain `skim <file>` (and stdin, glob, and directory invocations) only
  recorded analytics when token counts were already present in the parser cache from a
  prior `--show-stats` run; cold-cache and plain-warmed-cache runs silently dropped all
  data. The fix introduces a unified `record_file_ops` path that records token counts
  independent of cache state, with the detected language attached to each row. Multi-file
  invocations (glob, directory) now emit one analytics row per file instead of a single
  aggregate. **Dashboard metric note:** `skim stats` computes `invocations` as a row count,
  so a 3-file run now contributes +3 to the invocations counter instead of +1; historical
  data recorded before this release used the old single-row-per-invocation convention, so
  `invocations` comparisons across the upgrade point will reflect this change in counting
  semantics.

- **Oversized files now degrade to a lossless raw passthrough instead of erroring** — Files
  exceeding the AST node cap or line cap previously aborted with "Too many AST nodes / Possible
  malicious input". They now fall back to a full byte-faithful raw pass-through (signalled via a
  typed `ComplexityLimit`), so agents always see content rather than an error. `--max-lines` and
  `--last-lines` are honored, so a head-style request still yields the requested window. AST depth
  caps (guarding against unbounded recursion) remain hard errors.

- **`skim -` (stdin) without a language hint now degrades to lossless passthrough (exit 0)
  instead of erroring** — Previously, piping content to skim with no `--language` flag,
  no `--filename` hint, and no recognisable shebang produced an error and exited non-zero.
  stdin now falls back to a full byte-faithful passthrough consistent with the file-path
  degrade policy. Shebang auto-detection (`#!/usr/bin/env python3`, `#!/bin/bash`, etc.)
  still applies before the passthrough decision — only plain content with no detectable
  language degrades. Use `--language` for reliable transformation when the language cannot
  be inferred (applies ADR-002).

- **`skim search index` no longer shadows the search term "index"** — Previously `skim search index`
  always triggered an index build, making the literal term "index" unsearchable. The dispatch now
  routes to the build path only when trailing args fit the build grammar (`--force`, `--root`,
  `--max-files`, `--index-dir`); with any query flag or extra positional terms it searches for the
  literal string "index". `skim search -- index` forces a search via the POSIX `--` escape. Bare
  `skim search index` (no extra args) still triggers a build (backward-compatible).

### Added
- **`rskim-tokens` crate (L3 Wave-1)** — Multi-provider token counting library (cl100k /
  o200k / Anthropic-offline / heuristic). Default build is HTTP-free; `net-anthropic` feature
  gates the API-backed counter. `skim`'s internal token counting now delegates here
  (`--show-stats` output unchanged). (#300)
- **`rskim-contract` crate (L3 Wave-1)** — Byte-faithful contract / fail-open guardrail layer
  for LLM transcript mutation; codifies 8 safety invariants as a typed contract + conformance
  harness. (#301)
- **`rskim-llm` crate (L3 Wave-1)** — Typed LLM request model for Anthropic/OpenAI bodies
  with byte-identical round-trips, provider auto-detection, and content-block classification.
  (#302)

### Fixed
- **`gh` output-steering flags now pass through on both paths** — Two bugs
  caused `gh issue view 93 -q .body` and similar invocations to be reformatted + truncated
  instead of passed through raw:
  1. The hook rewrite skip-list (`rules.rs`) only contained the long-form flags (`--jq`,
     `--template`, `--web`) but missed the short aliases (`-q`, `-t`, `-w`) and `--json`.
     Now every gh rule that skips on a long form also skips on the short alias; `--json`
     is added to every gh rule except `gh api` (which has no `--json` flag) and
     `gh run watch` (streaming TUI).
  2. The handler (`cmd/infra/gh/mod.rs`) — reached via the PATH wrapper and direct
     `skim gh …` — had no output-steering check, so it reformatted and truncated output
     regardless of flags. A new transparency gate fires before the subcommand match:
     when `--json`, `--jq`, `-q`, `--template`, or `-t` are present (before `--`), gh
     is invoked via `run_raw_passthrough` (UTF-8, capped at `MAX_OUTPUT_BYTES`; see
     tracking issue #317 for the streaming/non-UTF-8 follow-up).

  `--web`/`-w` are intentionally excluded from the handler gate (no stdout to corrupt;
  `-w` is `--workflow` on `gh run list`). Glued short values like `-q.body` are not
  matched by design — consistent with the existing engine strict-match semantics.
  Plain `gh issue view N` (no output flag) still compresses as before.
  `gh api` and `gh run watch` are exempt from the `--json` steering check at the
  handler gate (matching the rewrite skip-list); only `--jq`/`-q`/`--template`/`-t`
  trigger passthrough for those subcommands.
- **Test-runner exit-code fidelity** (#350) — A passing suite whose output skim cannot
  parse now exits 0 instead of 1. Previously `resolve_exit_code` treated an unparseable
  exit-0 result as a failure (exit 1); it now propagates the child's zero exit code
  verbatim. Genuine non-zero exits from failing suites are preserved unchanged on all paths.
- **Git diff changed-line de-duplication** (#350) — Lines appearing in a hunk covered by
  adjacent diff ranges are now emitted exactly once. A per-`FileDiff` `EmittedCursor` tracks
  the last-written position; overlapping ranges advance the cursor rather than re-emitting
  the shared lines. No change to diff output for non-overlapping hunks.

### Changed
- **Exit codes refined: parse errors → 2, unsupported language → 3** — Previously all
  `skim` errors that prevented transformation exited 1. The CLI now maps known failure
  classes to distinct codes: exit 2 for grammar/syntax parse failures
  (`SkimError::ParseError`), exit 3 for unrecognised language when a `--filename`
  hint carries an extension skim does not recognise (`SkimError::UnsupportedLanguage`).
  Exit 1 is preserved for all other errors (I/O, config, etc.). Shebang-only or
  extension-based detection failures degrade to lossless passthrough (exit 0) rather
  than exiting 3; exit 3 fires only when a `--filename` hint carries an extension skim
  does not recognise. Scripts that tested for `exit != 0` are unaffected; scripts that branched on
  the exact exit code may need updating.
- **Session-id attribution priority inverted: sidecar > env > flag** (#350) — The hook no longer
  injects `--session-id` into rewritten commands; flag injection caused hard failures
  (`"unexpected argument --session-id"`) on older binaries. Attribution now resolves in order:
  sidecar (written out-of-band by the hook; found via ancestry walk) → `SKIM_SESSION_ID` env var
  (wrapper-surface attribution; export alongside `PATH`) → `--session-id=VALUE` flag
  (forward-compat fallback; honoured so old hooks that still inject the flag are not lost).
- **Net-savings guard token-decision cap raised 64 KiB → 256 KiB; new 4 KiB longest-run guard
  for degenerate inputs** (#317 / #350) — The cap controlling when `savings_decision` falls
  back from exact token counts to fast byte comparison is raised from 64 KiB to 256 KiB,
  improving token-accurate decisions for moderately large outputs.  A complementary
  longest-run guard is added: when either string contains a non-whitespace run exceeding
  4 KiB (and both strings are below the size cap), the function falls back to byte comparison
  to avoid O(n²) BPE merge cost on minified JS / base64 / binary-as-text single-line inputs.
  Real line-oriented shell output never triggers the run guard; the "never expand" safety
  invariant is unchanged on all paths.
- **Analytics stores true (gross) compressed-token counts on expansion rows** (#317 / #350) —
  Previously `compressed_tokens` was clamped to `raw_tokens` when the output expanded.  It is
  now stored as the true value.  The `tokens_saved` aggregate (in `query_summary`, `query_daily`,
  and all other aggregate queries) is floored per-row to 0 via `CASE WHEN`, consistent with
  existing `query_by_command` / `query_by_language` / `query_by_mode` / `query_by_session`
  behavior.  Row-level `raw_tokens` / `compressed_tokens` now carry true gross counts, allowing
  accurate expansion-rate analysis.  **Note:** rows written before this change remain clamped
  (mixed historical data); this is acceptable for cumulative analytics — no migration is needed.
- **`--show-stats` token counts reused for analytics recording** (#317 / #350) — When
  `--show-stats` is active, the token counts already computed for the stats display are reused
  to record the analytics row via `try_record_command_with_counts`, avoiding redundant
  background re-tokenization.  The common path (no `--show-stats`) is unchanged.
- **`serde_json` `preserve_order` feature enabled workspace-wide — key ordering changes** — (#302)
  Enabling `preserve_order` switches `serde_json::Map` from `BTreeMap` (alphabetical) to
  `IndexMap` (declaration/insertion order) for every crate in the workspace. Visible effects:
  - `skim stats --json` and `skim init`'s settings.json rewrite now emit keys in
    logical/insertion order instead of alphabetical order.
  - `skim aws`, `skim gh api`, and `skim curl` JSON-compression output changes key ordering
    from alphabetical to source order in truncated responses (first-N-key truncation paths).
  - `skim aws` primary-data-key selection is now explicitly alphabetical (restored by sorting
    candidate keys) so the summarised dataset is stable regardless of AWS response field order.
  JSON-spec-compatible; no test pins key order. Downstream `| jq` pipelines that relied on
  alphabetical key ordering may need to add an explicit `| keys_unsorted` or `| to_entries`
  sort step.
- **Removed internal command-execution timeout caps (ADR-008)** — `CommandRunner` no longer
  imposes a wall-clock cap on wrapped commands. Previous versions killed `cargo test`,
  `npm build`, and other long-but-finite commands after 300 s (default) or 600 s (builds).
  Skim is a transparent command wrapper; a transparent wrapper must not change whether or
  when a command completes. Bind child-process lifetime externally when needed: CI step
  timeout, the shell `timeout(1)` utility, the agent tool timeout, or `Ctrl-C`. The 64 MiB
  output memory cap (`MAX_OUTPUT_BYTES`) is unchanged — only the TIME bound is removed.
  **Accepted side-effect**: long finite commands now produce no output until they exit
  (skim buffers to compress); `SKIM_PASSTHROUGH=1` is the human escape hatch.
- **`skim vitest` now runs in watch mode by default (ADR-008 Part C)** — bare `skim vitest`
  and `cat output | skim vitest` are detected as indefinite and passed through live (no
  compression). To compress a one-shot run, use `skim vitest run` (or set
  `SKIM_PASSTHROUGH=1` to forward piped content).

### Fixed
- **Kill-on-drop guard for spawned children (ADR-008)** — `CommandRunner` now wraps every
  spawned process in a `ChildGuard` RAII struct. On any early-return path (size-cap error,
  pipe-capture failure, reader-thread panic) the guard calls `kill()` + `wait()` on the
  still-running child, preventing orphan processes. On the normal path the child has already
  exited before drop fires, so `kill()` is a harmless no-op. This also fixes the pre-existing
  orphan on the 64 MiB size-cap path.

### Added
- **Daemon / streaming command passthrough (ADR-008 Part C)** — Indefinitely-running
  commands (`vite dev`, `npm run dev`, `jest --watch`, `tail -f`, `kubectl logs -f`, etc.)
  are now detected before skim tries to capture their output. They are passed through with
  fully inherited stdio: live output streams to the terminal, stdin is forwarded (interactive
  dev servers work), and `Ctrl-C` terminates the child normally. Detection is heuristic and
  conservative — a missed daemon degrades to the old buffered path (64 MiB cap still applies);
  `SKIM_PASSTHROUGH=1` is an explicit escape hatch.
  **Accepted limitation**: stdin-reading interactive commands on the buffered (non-daemon)
  path could block on stdin; this is pre-existing and out of scope.

### Added
- **Wave 3e: AST Pattern Library & Structural Index v2** — `rskim-search` AST index format bumped to v2 (breaking; re-index required). Adds per-file structural metrics (max depth, max block statements, max params, branch count) and synthetic n-gram markers (EMPTY_BODY, DEEP_NODE, LARGE_BODY, MANY_PARAMS) emitted via a single-pass extraction. Pattern library catalog with 29 named patterns (ErrorHandling, Performance, Concurrency, Quality, Structure) GOLD-verified against real parse output; each pattern is either exact (reliable subset of every occurrence) or approximate (description says "approximation" or "structural"). (#196)

### Migration Note
- **AST index format v2 (breaking change)** — Files written by Wave 3d (`format_version=1`) are rejected with "please rebuild". Run `skim search index --rebuild` (or `--force`) to regenerate. The v2 format adds 10 bytes per file entry for structural metrics and stores `avg_max_depth:f32` in the header. No data loss: v2 is a strict superset of v1.

### Added
- **Universal shell interception via PATH wrappers** — `skim init --wrappers` creates symlinks in `~/.skim/bin/` for all supported tools. The skim binary detects `argv[0]` and dispatches through the existing handlers. Recursion is prevented by stripping `~/.skim/bin` from PATH as the very first action in `main()`. `--no-wrappers` skips wrapper installation unconditionally; in TTY environments without either flag, the user is prompted interactively. Non-TTY environments default to skipping. (#258)
- **Hook scripts include PATH prepend for wrapper activation** — Generated hook scripts now prepend `~/.skim/bin` to PATH so that sub-agents in restricted PATH environments still resolve wrapper symlinks. The prepend is guarded by a `[ -d "$HOME/.skim/bin" ]` check, so hooks installed before `skim init --wrappers` was run are unaffected. The skim binary's startup PATH strip prevents infinite recursion. (#258)

### Changed
- **`skim init` prompts interactively for wrapper installation on TTY** — When neither `--wrappers` nor `--no-wrappers` is supplied and stdin is a TTY, `skim init` now asks whether to install PATH wrappers. Non-TTY environments (CI, scripted installs) are unaffected and default to skipping wrappers. Use `--no-wrappers` as an escape hatch in scripted TTY contexts. (#258)

### Added
- **Temporal search flags** — Composable sort and filter flags for `skim search`: `--hot` (sort by hotspot score), `--cold` (invert hotspot sort), `--risky` (sort by fix-commit risk score), and `--blast-radius FILE` (pre-filter results to co-change peers of FILE before ranking). Flags compose freely; `--blast-radius` narrows the candidate set, then sort flags rank within it. 4 new CLI flags, 6 new public methods on `SearchQuery`/`SearchResults`, schema v2 migration (performance indexes for top-N and per-file lookup queries). (#189)
- **`skim search index` subcommand** — Build or update the n-gram search index for the current project. Walk/classify/build pipeline with parallel tree-sitter classification (rayon), JSONL manifest sidecar for incremental builds (SHA-256 cache hits skip re-classification), atomic write ordering, minified file detection, and 50K file cap. `--force` flag for full rebuild, `--root` for explicit project root, `--max-files` override. (#182)
- **`skim dig` / `skim nslookup` subcommands** — DNS query output compression via two independent parsers: `dig` uses section-based parsing (QUESTION/ANSWER sections), `nslookup` uses key-value line parsing. Both support three-tier degradation, `--json` structured output, error state compression, and macOS + Linux format variants. `nslookup` includes no-args guard. 2 new rewrite rules (total: 148) (#168)
- **`skim make` / `skim gmake` subcommands** — GNU Make build output compression via three-tier parser: Tier 1 (GCC/Clang diagnostics regex + make failure lines), Tier 2 (noise-stripped invocation/directory-change lines), Tier 3 (passthrough). Includes `gmake` rewrite rule for hook integration. 17 unit tests, 2 E2E tests (#167)

### Fixed
- **`skim env` no longer leaks short credentials** — Credential redaction was previously gated
  behind the ADR-001 net-savings guard: a redacted view that was not shorter than raw lost to
  raw passthrough, emitting secrets verbatim. Measured leaks included `GITHUB_TOKEN=ab`,
  `GITHUB_TOKEN=abcd1234`, and `NPM_TOKEN=xy`. Redaction is a security control and is no longer
  subject to byte arithmetic; the redacted view is always served.
- **Lossless degrade at `MAX_INPUT_LINES` cap for system-utility wrappers** — `wc`, `df`,
  `du`, `find`, `ps`, and `env` previously used `break` / `.take(..)` at `MAX_INPUT_LINES`,
  silently truncating output with an elision marker that understated the true loss (totals were
  computed after the break, omitting dropped records). These wrappers now return `None` at the
  cap for a lossless passthrough degrade, consistent with the documented degrade policy;
  `env` returns `None` explicitly.

### Changed
- **ADR-012: escape sequences in wrapped-tool CONTENT are not filtered** — Skim does not strip
  terminal escape sequences originating in content processed by wrappers (e.g. `skim diff`
  patch-content lines), matching what the raw tool emits and the #317 byte-faithfulness MUST.
  A wrapped tool's own colorization is still neutralized at the child-invocation boundary via
  `--no-color`.

## [2.11.0] - 2026-07-11

Consent-gated permissions seeding for `skim init`; Copilot hook re-homed to `~/.copilot`; `ls -a` fidelity fix. 4,329 tests passing (up from 3,558 in v2.10.0).

### Added

- **`skim init --permissions` / `--no-permissions` / `--permissions-tier=<tier>`** —
  Consent-gated allowlist seeding for Claude, Gemini, Codex, and Copilot. Three tiers:
  - **seed** (default): seeds 8 arg-safe read-only wrapped tools (`df`, `diff`, `du`,
    `grep`, `ls`, `rg`, `tree`, `wc`) as allowlist entries. Excluded-for-cause:
    `find`, `env`, `printenv`, `dig`, `nslookup`, `ps` (network or process-enumeration
    risk). `Bash(skim <tool>:*)` prefix entries do NOT bound tool arguments — the seed
    set is individually arg-safety-vetted.
  - **mirror**: proposes exact-shape mirrors of the agent's existing allow rules, with
    mutating tools highlighted; sourced from the live settings file; deny/ask-aware (skips
    already-restricted rules). Claude-scoped only.
  - **blanket**: seeds all wrapped subcommands; requires a second hazard confirmation at
    the TTY prompt; refused for Codex (Codex is explicit-opt-in only for `--permissions`).
  - Interactive TTY consent is required before any entries are written — `--yes` never
    bypasses the grant prompt.
  - A sidecar manifest (`skim-permissions.json`) is written alongside the agent config on
    every seed; `skim init --uninstall` reads the sidecar for targeted removal so only
    skim-seeded entries are touched.
  - `--dry-run` enumerates the entries that would be written (with `[mutating tool]`
    annotations on mirror tier) without modifying any files.
  - Cursor and Crush have no permissions seeding (IDE-hook / no-hook channel only).

### Fixed

- **`skim ls -a` / `--all` now retains `.` and `..` entries** — `parse_ls` previously
  stripped `.` and `..` unconditionally, silently truncating listings requested with
  `-a`/`--all`. Fix threads an `include_dotdirs` flag through both long-format and
  plain-format accumulation paths; retained dotdirs are included in the entry count.
  Honors compress-never-truncate (#317).

- **Copilot CLI hook re-homed to `~/.copilot/hooks/skim.json`** — Skim previously
  installed Copilot hook config under `~/.github/` (a location the Copilot CLI never
  reads). The hook is now written to `~/.copilot/hooks/skim.json` (or
  `$COPILOT_HOME/hooks/skim.json`) using the documented hook-file envelope
  (`{"version":1,"hooks":{"preToolUse":[…]}}`). On re-init, guarded migration
  removes legacy `~/.github` artifacts (settings entry, hook script, SHA sidecar)
  only when the new-location artifacts already exist; each removal is non-fatal and
  idempotent.

- **Copilot hook response now uses `modifiedArgs`** — Skim's Copilot PreToolUse response
  previously included a `permissionDecision` field the protocol does not require. The
  response is now `{"modifiedArgs":{"command":"<rewritten>"}}` with no `permissionDecision`
  or `reason`. No-rewrite invocations emit no output (empty stdout, existing convention).
  Requires Copilot CLI >= 1.0.24 (released 2026-04-10); older CLIs silently ignore
  `modifiedArgs` — inert passthrough, not breakage. Schema verified 2026-07-11 against
  GitHub Copilot CLI hooks-reference and hooks-configuration documentation.

### Changed

- **Cursor is IDE-only** — Cursor CLI has no rewrite-capable hook event; only the IDE
  (`.cursor/rules/*.mdc` guidance injection) is supported by `skim init`. No permissions
  seeding is offered for Cursor. Documented in `session/types.rs` operational contract.

- **Per-agent `<AGENT>_CONFIG_DIR` overrides honored on the write path** —
  `DetectionEnv::resolve()` now applies agent-specific env overrides on the install and
  uninstall write path in addition to the pre-existing detection path. `CLAUDE_CONFIG_DIR`,
  `GEMINI_CONFIG_DIR`, `CODEX_CONFIG_DIR`, `COPILOT_CONFIG_DIR`, and equivalents redirect
  `skim init` to the specified directory, enabling fully sandboxed integration tests without
  touching real agent config locations.

### Two-Speed Rollout

Binary-side changes (`ls -a` fidelity, Copilot hook response strategy) take effect
immediately after upgrading the binary. Config/guidance changes (permissions seeding, Copilot
hook re-home and legacy migration, updated agent guidance) activate at the next `skim init`
re-run; the version bump advances the binary past the "already up to date" fast path, so
`skim init` re-runs on the first post-upgrade invocation. Because Copilot never loaded the
legacy `~/.github` hook config, all Copilot-facing changes (re-home, `modifiedArgs` response,
migration) activate only at re-init.

## [2.10.0] - 2026-05-13

Container, cloud, database compression; search crate foundation; heatmap insights. 3,558 tests passing (up from 3,310 in v2.9.0).

### Added
- **`rskim-search` crate (Wave 0)** — New workspace crate providing the search foundation: `SearchLayer`, `LayerBuilder`, and `FieldClassifier` traits, `SearchQuery`/`SearchResult`/`IndexStats` types, AST-aware `SearchField` classification (8 field variants), and typed `SearchError` hierarchy. Pure library with no I/O — CLI integration in future waves (#213)
- **`skim heatmap --insights` flag** — threshold-filtered one-liner findings for focused risk analysis. Reports only CRITICAL/WARNING severity metrics (fix-risk, bus-factor, churn, coupling) in text and JSON formats, skipping healthy files (#215)
- **`skim psql` subcommand** — PostgreSQL query output compression via three-tier degradation: Tier 1 (tabular `----+----` format), Tier 2 (regex row-count extraction), Tier 3 (passthrough). Supports `--json` for structured `DbResult` output (#117)
- **`skim mysql` subcommand** — MySQL query output compression: Tier 1 (TSV batch output), Tier 2 (bordered `+---+` table format), Tier 3 (passthrough). Handles empty-set detection and multi-result sets. Supports `--json` (#117)
- **`skim sqlite3` subcommand** — SQLite query output compression: Tier 1 (pipe-separated with `-header -separator |` injection), Tier 3 (passthrough for schema dumps and meta-commands). Supports `--json` (#117)
- **`DbResult` canonical type** — Structured database query result with column/row data, row count, truncation flag, and pre-rendered aligned table output. Part of the canonical output type system (#117)
- **`skim docker` subcommand** — Docker output compression for `ps`, `images`, `inspect`, `build`, `logs`, `compose` via three-tier degradation (#117)
- **`skim kubectl` subcommand** — Kubernetes output compression for `get`, `describe`, `logs`. Injects `-o json` for `get` (skipped for watch/existing format flags) (#117)
- **`skim terraform` subcommand** — Terraform output compression for `plan` and `apply`: Tier 1 (NDJSON from `-json`), Tier 2 (regex on human-readable text). Safety invariant: never injects `-json` for plan/apply to preserve interactive approval prompts (#117)
- **15 new rewrite rules** — Docker (ps, images, build, inspect, logs, compose ps, compose logs), kubectl (get, describe, logs), terraform (plan, apply), psql (-c), mysql (-e), sqlite3. Total: 122 rules across 8 categories (#117)

### Fixed
- **DB family ANSI strip bypass** — `strip_ansi_escapes` was stripping ASCII tab characters (0x09) from stdin content, causing MySQL TSV parsers to receive tab-free data and fall through to passthrough. DB family commands now bypass ANSI stripping to preserve tab separators (#117)
- **Heatmap `set_var`/`remove_var` unsafe blocks** — Rust 2024 edition requires explicit `unsafe {}` blocks for `std::env::set_var` and `std::env::remove_var` in test code
- **Test isolation for `SKIM_PASSTHROUGH`** — Hook and parser E2E tests now clear `SKIM_PASSTHROUGH` from inherited environment, preventing false failures when tests run inside a skim hook session

### Testing
- 3,558 tests passing (up from 3,310 in v2.9.0)
- 14 new E2E parser tests: psql (tier 1, empty, tier 2, tier 3, JSON), mysql (tier 1 TSV, tier 2 bordered, tier 3, empty set, JSON), sqlite3 (tier 1, empty, tier 3 schema, JSON)
- 19 new E2E rewrite tests: docker (5 positive, 1 skip), kubectl (3 positive, 2 skip), terraform (2 positive, 1 skip), DB tools (3 positive, 1 skip)
- 22 new `rskim-search` unit tests: type roundtrips, trait contracts, field classifier, serde agreement (#213)

## [2.9.0] - 2026-05-08

Heatmap analysis, system utility parsers, curl hardening. 3,310 tests passing (up from 3,103 in v2.8.0).

### Added
- **`skim heatmap` subcommand** — Git history risk and coupling analysis. Mines git log to produce 6 metrics: file churn, co-change coupling (blast radius), stability scores, author concentration (bus factor), fix-after-touch risk, and module encapsulation health. Supports adaptive dual windowing (max of 90 days / 200 commits), auto-exclusion of lock files and build artifacts, JSON and text output, and path scoping. 87 new tests (#163)
- **Heatmap file targeting** — positional file args, `--diff` flag for changed-files-only analysis, display filter for focusing output on specific paths (#171)
- **System utility parsers** — `df`, `du`, `env`, `printenv`, `ps`, `wc`, `diff`, `rg`, `tree` output compression via flat dispatch subcommands (#166)

### Changed
- **Curl parser hardened** — error status detection, redirect chain compression, HTML body truncation, verbose header filtering, write-out format support (#169)

### Fixed
- **CI release workflow** — fail hard on real publish errors, skip gracefully on already-published versions (#165)

### Testing
- 3,310 tests passing (up from 3,103 in v2.8.0)

## [2.8.0] - 2026-05-07

Flat dispatch, Crush agent, multi-file args. 3,103 tests passing (up from 3,002 in v2.7.0).

### Breaking Changes

- **Flat dispatch CLI syntax** — Tool names are now top-level subcommands instead of being grouped under family prefixes (#158). The old `skim <family> <tool>` syntax is removed. Hooks auto-adapt — no user action needed for rewrite rules.

  **Migration Guide:**

  | v2.7.x | v2.8.0 |
  |---|---|
  | `skim test cargo` | `skim cargo test` |
  | `skim test vitest` | `skim vitest` |
  | `skim test pytest` | `skim pytest` |
  | `skim test jest` | `skim jest` |
  | `skim test go` | `skim go test` |
  | `skim build cargo` | `skim cargo build` |
  | `skim build clippy` | `skim cargo clippy` |
  | `skim build tsc` | `skim tsc` |
  | `skim lint eslint` | `skim eslint` |
  | `skim lint ruff` | `skim ruff` |
  | `skim lint biome` | `skim biome` |
  | `skim pkg npm audit` | `skim npm audit` |
  | `skim pkg pip install` | `skim pip install` |
  | `skim file find .` | `skim find .` |
  | `skim file ls` | `skim ls` |
  | `skim file grep pattern` | `skim grep pattern` |
  | `skim infra gh pr view` | `skim gh pr view` |
  | `skim infra curl` | `skim curl` |
  | `skim infra aws` | `skim aws` |

- **OpenCode agent removed** — `skim init --agent opencode` is no longer supported. Use `skim init --agent crush` instead (#160).

### Added
- **Multiple file args** — CLI now accepts multiple file arguments and absolute glob paths (`skim src/main.rs src/lib.rs`, `skim /absolute/**/*.ts`) (#161)
- **Session ID sidecar** — fallback attribution for analytics when session ID is not available from the agent hook (#159)
- **Crush agent support** — `skim init --agent crush` installs hooks for the Crush AI agent; `HookProtocol` extended with config lifecycle methods (#160)

### Changed
- **Init guidance refactored** — prescriptive decision table replaced with principle-based agent guidance for clearer, more maintainable hook installation output (b79f6e3)
- **Dispatch architecture rewritten** — family-grouped subcommands (`test`, `build`, `lint`, `pkg`, `file`, `infra`) replaced with flat dispatch where tool names (`cargo`, `vitest`, `eslint`, `npm`, `find`, `gh`) are top-level subcommands (#158)

### Fixed
- **Clippy lint scoping** — `allow(unwrap/expect/panic)` attributes scoped to inline `#[cfg(test)]` modules in rskim-core, not the entire crate (#162)

### Removed
- **OpenCode agent** — removed from `HookProtocol` implementations; `skim init --agent opencode` no longer supported (#160)

### Testing
- 3,103 tests passing (up from 3,002 in v2.7.0)

## [2.7.0] - 2026-05-01

Line numbers, session tracking, output sanitization. 3,002 tests passing (up from 2,883 in v2.6.0).

### Added
- `-n`/`--line-numbers` flag — prefix output lines with source line numbers across all transformation modes (#155)
- Session tracking pipeline — `session_id` extraction from agent hooks (Claude, Cursor, Copilot, Gemini), injection into rewritten commands, per-session stats on analytics dashboard (#150)
- Schema v3 migration — nullable `session_id` column with index in analytics database
- Advanced $5/MTok pricing tier between Standard ($3) and Premium ($15)
- `RecordingContext<'a>` struct — eliminates parameter threading in analytics pipeline
- `is_safe_session_id()` — centralized session ID validation (128-char max, rejects metacharacters)

### Changed
- Output sanitization across all parsers: dropped command-type prefixes (BUILD/LINT/PKG/TEST/INFRA/LOG/FILE), lowercase status labels (pass/fail/skip), reduced indentation, collapsed body stubs (`{...}` from `{ /* ... */ }`), simplified multi-file separators and diff headers
- Git operation prefixes simplified: `[status]`/`[log]`/`[fetch]` → `status`/`log`/`fetch`
- `serde(skip_serializing)` added to all rendered fields in canonical types
- Init guidance uses `--line-numbers` instead of `-n` for clarity

### Testing
- 3,002 tests passing (up from 2,883 in v2.6.0)

## [2.6.0] - 2026-04-27

Terminal UX overhaul, non-interactive init, plugin ecosystem removal. 2,883 tests passing (up from 2,800 in v2.5.1).

### Added
- `--no-truncate` flag for `discover` and `learn` subcommands — disables terminal-width-aware table truncation (#154)
- Terminal UX primitives (`cmd/ux.rs`): `with_spinner` closure helper, `print_indented_table`, comfy-table formatting, colored output across `discover`, `learn`, `agents`, `init` (#153)
- Responsive table truncation — auto-detect terminal width, truncate wide columns to fit viewport, graceful fallback for non-TTY (#154)

### Changed
- `skim init` install is always non-interactive — removed scope/marketplace prompts and confirmation; `--yes` still accepted for backward compatibility (#151)
- Hook scripts use bare `exec skim rewrite --hook` (PATH-resolved) instead of hardcoded absolute binary paths — eliminates "unknown git subcommand" failures after npm upgrades (#151)
- Format-aware idempotency — old-format hooks with absolute paths are detected and force-regenerated even when version matches (#151)
- Stats dashboard shows savings percentage only once (on bar line), not redundantly next to token count (#153)

### Fixed
- Hook script absolute path failures after upgrading skim via npm (#151)

### Removed
- Plugin ecosystem — `.claude-plugin/` directory, `plugins/skimmer/` directory, `.github/workflows/sync-skimmer-plugin.yml` CI workflow, marketplace registration from init state (#153)
- `validate_shell_safe_path()`, `SHELL_UNSAFE_CHARS`, and 12 associated tests — attack surface no longer exists with bare command approach (#151)
- `prompt_choice()` helper (dead code after non-interactive install change) (#151)

### Testing
- 2,883 tests passing (up from 2,800 in v2.5.1)

## [2.5.1] - 2026-04-20

Hook safety and command coverage gaps — SKIM_PASSTHROUGH bypass, npx fallback, tiered test compression, 5 new parsers, streaming primitive. 2,800 tests passing (up from 2,629 in v2.5.0).

### Added
- **`skim git commit`** — new parser compressing `git commit` output: extracts commit hash, subject, and changed-files summary; terminates at verbose diff scissors line (`---...>8---`, AD-GC-1); skips hook noise (pre-commit, black, ruff, eslint output)
- **`skim git push`** — new parser compressing `git push` output: full porcelain mode (auto-injected `--porcelain`, AD-GP-2); per-ref status (`* new`, `= up to date`, `+ forced`, `! rejected`); credential URL scrubbing via `scrub_credential_url` (AD-GP-1)
- **`skim infra gh api`** — new parser compressing `gh api` / `gh api graphql` output: GraphQL `.data` unwrap + `.errors` prepend, base64 `content` field replacement, binary passthrough, depth-limited JSON compaction (AD-API-1)
- **`skim infra gh run watch`** — new streaming parser for `gh run watch`: emits job status transitions (queued → in-progress → completed/failed), suppresses progress noise, emits `Run complete: N/M succeeded` summary (AD-GRW-1)
- **`skim infra gh release view`** — new parser compressing `gh release view --json` output: extracts tag, name, dates, assets list (capped at 20), body truncation outside code fences (AD-RV-1)
- **Rewrite rules for new commands** — 7 new rules (93→100): 5 specific (`git commit`, `git push`, `gh run watch`, `gh release view`, `gh api`) + 2 catch-all (`ls`, `grep`) (AD-RW-2)
- **Catch-all ls/grep rewrite rules** (B.1/B.2) — any `ls` or `grep` invocation without a more-specific match is now rewritten; `--help`/`--version` pass through; pipe sources excluded from rewriting
- **Redirect stripping** (`strip_segment_redirects`) — per-segment redirect stripping in compound commands; redirects are restored at emission time (appended to end), preserving shell semantics
- **Streaming primitive** (`streaming.rs`) — `StreamingParser` trait, `run_streamed_spawned`, `DropGuard` for fire-and-forget analytics on streaming commands
- **Shared git helper** (`git/shared.rs`) — `scrub_credential_url` strips credential-embedded URLs (`https://<token>@github.com/...`) using lazy regex (AD-GP-1)
- **`SKIM_PASSTHROUGH` env var** — universal bypass for all compression (hook, vitest, go, pytest, generic); set to `1`/`true`/`yes` to disable skim rewriting for a single invocation
- **Cascading spawn fallback** — vitest/jest spawn attempts PATH → `./node_modules/.bin` → `npx --no-install`; prevents hook failures on projects where the binary is not globally installed
- **Tiered test compression** — on test failure, the last 50 lines of raw output are appended so agents see the full failure context without disabling compression
- **stderr hint on compressed test failures** — failed compressed test output includes a `SKIM_PASSTHROUGH=1` guidance hint on stderr for debugging
- **Troubleshooting section in `skim init` guidance output** — init instructions now include a troubleshooting block with `SKIM_PASSTHROUGH=1` usage
- **`emit_failure_context` shared helper** — DRY helper for appending raw failure context across vitest, go, pytest, and generic test handlers

### Fixed
- **`find` and `rg` pipe-source exclusion** — `find . | head` and `rg pattern | head` are no longer rewritten; `exclude_pipe_source: true` was missing from their rules despite having specific rule entries (regression vs. intended AD-RW-2 semantics). `is_catch_all` renamed to `exclude_pipe_source` to accurately describe the field's purpose.
- **`try_parse_porcelain` false-trigger** — porcelain parser now ignores informational lines starting with flag characters (`!`, `-`, `=`) that don't contain a ref path (`refs/` prefix) or src:dst notation (`:`). Prevents lines like `! [remote rejected] branch (pre-receive hook declined)` from being misparsed as ref-status entries. Root cause: `strip_ansi_escapes` strips tab bytes, so tab-based guard heuristics are unreliable.
- **Credential scrubbing on success and failure analytics paths** — `raw` (success) and `output.stdout` (failure) are now scrubbed line-by-line via `scrub_credential_url` before being passed to `finalize_git_output_owned` / `finalize_git_output_passthrough`. Previously, `analytics.db` could persist credential-bearing URLs from push/fetch output (PF-024).
- **`ssh://` credential scrubbing** — `CREDENTIAL_URL_RE` now matches `ssh://user@host/...` in addition to `https://` and `git://` (AD-GP-1). SSH-cloned repos with embedded credentials in push/fetch output are now scrubbed on the same code path.
- **`build_streaming_label` analytics label alignment** — `gh run watch` now produces analytics labels via the shared `build_streaming_label` helper, matching the `"skim {family} {program} {subcommand} {args}"` format used by non-streaming infra commands (PF-022). `gh api` uses the standard `ParsedCommandConfig` analytics path via `run_infra_tool` and does not need the streaming helper.
- **Node.js spawn fallback scoped to spawn failures only** — cascading PATH → `node_modules/.bin` → `npx` fallback only triggers on spawn errors (ENOENT/permission denied), not on non-zero test exit codes; stderr is routed correctly in passthrough mode

### Changed
- **`is_catch_all` renamed to `exclude_pipe_source`** — field semantics now describe the actual behavior (pipe-source suppression) rather than the matching strategy (AD-RW-2)
- **Rewrite engine deduplication** — `should_skip_by_flag` extracted as a named function (replacing two duplicated inline closures); `has_pipe_operator` and `reconstruct_pipe_parts` extracted to `compound.rs`; `splice_redirects_back` promoted to `pub(super)`. Dead index (`PIPE_EXCLUDED_SOURCES` slice) removed.

### Testing
- 2,800 tests passing (up from 2,629 in v2.5.0; +33 from hook safety (#149))
- Pipe-source exclusion tests for `find` and `rg` (standalone rewritten, piped suppressed, `||` chain not suppressed)
- Negative tests for compress-or-skip (`ls --help`, `grep --version` passthrough)
- Redirect stripping coverage for all 7 single-token forms + two-token `2> /dev/null`
- `scan_operator` regression test for `>&1&&` edge case
- Credential scrubbing error-path tests (stderr with `https://` and `ssh://` URLs)
- Porcelain parser false-trigger regression tests (informational `!`/`-` lines without tabs)
- Pipe/stdin E2E parsed-vs-raw compression comparison for `gh run watch` and `gh api`
- Hook safety E2E tests: `SKIM_PASSTHROUGH` bypass, spawn fallback (PATH/node_modules/.bin/npx), tiered failure context

## [2.5.0] - 2026-04-17

Formatter output compression — 8 new parsers for code formatter tools. 2,629 tests passing (up from 2,482 in v2.4.1).

### Added
- **`skim lint` formatter support** — 8 new parsers: ruff format, prettier --write, rustfmt, black, gofmt, biome, dprint, oxlint
- `LintResult.files_formatted` field to track formatted files separately from lint errors
- Path-aware regex patterns for files with spaces
- Rewrite rules for formatter commands (`ruff format`, `prettier --write`, `black`, `gofmt`, `biome check --write`, `dprint fmt`, `oxlint --fix`)

### Changed
- `skim lint` dispatcher unified — formatter and linter parsers use single dispatch path
- Tech debt consolidation: dispatch logic, canonical output helpers, file handler signatures unified across lint/file/infra/git modules

## [2.4.1] - 2026-04-15

### Changed
- **`skim stats` dashboard redesign** — weighted savings %, category grouping, column headers, by-command breakdown (top 15 commands by tokens saved), DRY render helpers, O(n) truncation
- **`skim stats --cost` deprecated** — cost estimates are now always shown; `--cost` flag prints a deprecation warning
- **`skim stats --verbose`** — new flag to show parse quality section
- **`skim stats --format json` schema updated** — `cost_estimate` is now always present (previously only included when `--cost` flag was passed). The `cost_estimate` object uses a `tier` key (e.g. `"Standard"`) instead of the former `model` key. Two new top-level fields are always present: `by_original_cmd` (array of top-15 commands by tokens saved, each with `original_cmd`, `invocations`, `tokens_saved`, `avg_savings_pct`, `avg_duration_ms`) and `summary.weighted_savings_pct` (tokens-weighted savings percentage, more accurate than `avg_savings_pct` for uneven workloads). Downstream consumers of `skim stats --format json` must update to use `tier` instead of `model` in `cost_estimate`.

### Fixed
- `skim git show` test now handles guardrail passthrough correctly when compressed output exceeds raw size
- Stats verbose test uses canonicalized fixture paths to work with `Language::from_path()` security check

## [2.4.0] - 2026-04-14

GitHub CLI compression, git subcommand completion, multi-agent handler fixes, quality improvements. 2,482 tests passing (up from 2,223 in v2.3.1).

### Added
- `skim infra gh pr view` now always renders `draft`, `mergeable`, and `ci` items so agents observe the full merge-readiness signal set even on clean PRs. A `[DRAFT]` prefix is added to the summary when the PR is a draft. CI aggregation: `FAILURE`/`CANCELLED`/`TIMED_OUT` → `failing`; `PENDING`/`QUEUED`/`IN_PROGRESS` → `pending`; else `passing`; null/empty → `none`. (AD-INFRA-9, commit 689e397, see `crates/rskim/src/cmd/infra/gh/pr_view.rs`)
- `prettier --check`, `rustfmt --check`, `cargo fmt --check`, and `cargo fmt -- --check` are now acknowledged as already-compact (AD-RW-11, compress-or-skip rule). `skim rewrite` echoes the original command instead of rewriting to `skim lint prettier/rustfmt`. This prevents the skim header from inflating output on clean or near-clean codebases.
- `parse_tier` field added to `GitResult` and propagated through all git handlers to the analytics DB (AD-GIT-12). Git invocations now appear with `"full"`, `"degraded"`, or `"passthrough"` tier labels, consistent with the file/lint/infra handler families.

### Fixed
- `skim git show <commit>` now preserves the full commit message body (multi-paragraph) and merge-parent hashes (`Merge: p1 p2` prefix in rendered output). Previously both were silently dropped. GPG/SSH signature blocks (`gpgsig`/`mergetag`) remain elided. (AD-GIT-8, commit 0f9c82b, see `ShowCommitResult::body` and `parents` fields in `crates/rskim/src/cmd/git/show.rs`)
- `skim git diff` now records a zero-compression analytics row when the diff is empty (previously uncounted). This unifies analytics recording across empty and non-empty invocations. ([#132](https://github.com/dean0x/skim/issues/132), [#135](https://github.com/dean0x/skim/pull/135))
- `skim git show <annotated-tag|blob|tree>` (non-commit passthrough) now records analytics (previously uncounted). This unifies analytics recording across all `git show` modes. ([#132](https://github.com/dean0x/skim/issues/132), [#135](https://github.com/dean0x/skim/pull/135))
- `skim git log`, `skim git status`, `skim git fetch`, `skim git diff` now record analytics on non-zero exit codes (previously, failed git invocations were silently dropped from the analytics DB). (AD-GIT-14, commit ea4e52f)
- `skim infra gh pr checks` and `skim infra gh run view` now include URLs for failing checks and run items so agents can navigate directly to the failure without a second command. (AD-INFRA-15, see `crates/rskim/src/cmd/infra/gh/pr_checks.rs` and `run_view.rs`)
- `skim pkg npm audit` and `skim pkg pnpm audit` now include advisory identifiers (`GHSA-xxxx-yyyy-zzzz` extracted from `via[i].url`, or `NPM-{source}` fallback for legacy numeric IDs) in rendered advisory details. Mirrors `skim pkg cargo audit` existing behaviour. (AD-PKG-18, commit 9041511)
- `skim test cargo` and `skim test vitest` now surface failing test names in their Tier-2 regex fallback paths (previously returned empty entries). Names are capped at 100 to match Tier-1 semantics. ANSI codes are stripped before regex matching. (commit cc662e6, see `crates/rskim/src/cmd/test/shared.rs::scrape_failures`)
- `skim test vitest` Tier-2 regex now tolerates leading whitespace on failing-test lines (e.g. `   × divides by zero`), matching real vitest output rather than only bare-`✕` hand-crafted fixtures. (AD-TEST-19, commit ea4e52f)
- `skim rewrite '<full command>'` with a single quoted-string argument now tokenizes the same way as stdin input (via `split_whitespace` inside `collect_input_tokens`). Previously the single-arg form produced a one-element vector that matched no rule and no ACK prefix, silently returning exit 1. (AD-RW-13, commit 48dded7)
- `skim log` pending_stack is now capped at 4 frames (sliding window); excess frames are elided incrementally rather than accumulating unboundedly. Output behavior is unchanged (last 3 frames shown + elision count in `LogResult.stack_frames_elided`), but memory is now bounded at O(1) regardless of input length. (DoS hardening, commit ec32165)
- `skim log` JSON log fields are now capped before processing: `level` at 32 chars, `message` at 16 KiB. Values that exceed the cap are truncated and suffixed with `[truncated]` so consumers can detect elision. Prevents resource exhaustion from adversarially large JSON log lines. (DoS hardening, commit ec32165)
- `skim log` deduplication allocations reduced: key construction no longer heap-allocates on the common (non-duplicate) path. (commit ec32165)
- `skim log` now tracks stack frames attached to the preceding log entry (up to 3 frames shown; elision count in `LogResult.stack_frames_elided`). Deduplication is level-aware (`level|message` key). (AD-LOG-10)
- `skim file ls` degradation marker now uses `"tree:"` prefix, matching the format contract for all `skim file` parsers. (commit e8fbf50)
- `skim infra curl` now surfaces up to 20 object keys (was 5) before truncating. The truncation notice displays the actual cap.
- `skim file tree` depth-capped output now reports the count of hidden deeper entries (`"(N deeper entries hidden)"`) instead of a generic cap notice.
- `skim lint golangci` severity now inferred from linter name and message text (was always `Warning`). (AD-LINT-16, see `crates/rskim/src/cmd/lint/golangci.rs::infer_severity_from_text`)
- `skim lint rustfmt` location messages now include the diff line number (`"formatting difference at line N"` vs. `"formatting difference detected"`). (AD-LINT-17, see `crates/rskim/src/cmd/lint/rustfmt.rs`)
- `skim git diff` warns on non-empty stderr at exit 0 (`[skim] git diff notice: ...`) so LF/CRLF replacement warnings are not silently discarded.
- PF-018 fully resolved: `finalize_git_output_passthrough` eliminates double-clone on passthrough paths across all git handlers. (commit 6f06e9d)
- PF-021 fully resolved: `run_passthrough` now uses `build_analytics_label` so format strings are not allocated when analytics are disabled. (commit 6f06e9d)

### Changed
- `git show` diff rendering (`render_show_diff`) now uses rayon parallel iteration for large multi-file diffs, consistent with other multi-file diff paths. (commit 6f06e9d)
- `parse_commit_header` refactored to borrow body slices from the input rather than cloning them, reducing allocations on the hot path. (commit 6f06e9d)
- `gh pr checks` parser extracted `ParsedCheck` struct and `non_empty_capture` helper, replacing positional tuple usage and eliminating silent field-swap risk. (commit d6ff12c)

### Testing
- **2,470 tests passing** (up from 2,223 in v2.3.1; +16 alignment E2E, +parse_tier unit tests, +stack-trace/dedup/npm-audit/gh-pr-view/scrape_failures/scrutinizer-regression unit tests, +Wave 1 regression tests for git show failure analytics and rewrite tokenization)
- New `cli_e2e_rewrite_alignment.rs` — 16 tests closing the rewrite→execute loop for all major command families
- Double ANSI strip eliminated in vitest Tier-2 test path (commit fd3ce4f)
- New regression tests: `git show` failure-path analytics, `skim rewrite` single-arg tokenization (commit fd3ce4f)

## [2.3.1] - 2026-04-09

Patch release: discover/rewrite alignment, rewritable gap closures, stats bar fix.

### Fixed
- `render_bar` zero-width ANSI color leak in stats dashboard
- Removed unjustified skip flags from rewrite rules (git status, git log, gh list)

### Added
- Jest / npx jest rewrite rules for test output compression
- `pub(crate) would_rewrite()` API for discover/rewrite alignment
- `--debug` flag for `skim discover` command

### Testing
- **2,223 tests passing** (consolidated from 2,306 in v2.3.0 via PR #130 alignment work)

## [2.3.0] - 2026-04-08

Minor release: Stats dashboard v3, debug-gated warnings, git fetch compression.

### Added
- `skim git fetch` subcommand — parses git fetch output (ref updates, new branches/tags, pruned refs, forced updates, submodule fetches)
- `--debug` flag and `SKIM_DEBUG` env var — gates `[skim:warning]`/`[skim:notice]` stderr markers (silent by default)
- `transform_with_quality()` public API in rskim-core — returns parse quality flag alongside content
- Enriched degradation markers across all 23 parsers (e.g., `"eslint: JSON parse failed, using regex"`)
- Stats dashboard v3 — green-only efficiency colors, daily trend subtitle, parse tier tracking

### Fixed
- Parse tier tracking through ProcessResult/cache/analytics chain
- Git diff rewrite rules: added `--shortstat`/`--numstat` to skip-flags
- `parse_fetch` submodule context bug (refs after submodule block incorrectly attributed)
- Debug module: atomic ordering upgraded to Release/Acquire, env var cached at startup
- Test pollution: `force_enable_debug()` now has `reset_debug_for_tests()` cleanup

### Testing
- **2,306 tests passing** (up from 2,226 in v2.2.0)

## [2.2.0] - 2026-04-06

Minor release: File, log, and infrastructure output compression (12 new tool parsers), learn command fix, rewrite/discover integration.

### Added — File Output Compression (`skim file`)
- New subcommand: `skim file <tool> [args...]`
- Supported tools: find, ls, tree, grep, rg
- grep/rg deduplication and match grouping
- Item limits and byte caps for large outputs

### Added — Log Output Compression (`skim log`)
- New subcommand: `skim log [args...]`
- JSON structured log deduplication with counts
- Regex-based plaintext log deduplication
- Debug/trace level filtering with `--debug-only` and `--keep-debug`
- Stack trace collapsing

### Added — Infrastructure Output Compression (`skim infra`)
- New subcommand: `skim infra <tool> [args...]`
- Supported tools: gh, aws, curl, wget
- Metadata stripping, pagination removal, response body extraction

### Added — Lint Extensions
- prettier check-mode output compression
- rustfmt check-mode output compression

### Added — Integration
- Rewrite rules updated for file, log, and infra subcommands
- Discover integration for new subcommands
- Shell completions for new subcommands

### Fixed
- Router: subcommands always take priority over filesystem paths (no more collisions with files/dirs named `test`, `log`, etc.)
- Learn command: fixed guidance injection and improved error handling (#115)

### Testing
- **2,226 tests passing** (up from 1,993 in v2.1.0 — 12% increase)

## [2.1.0] - 2026-04-01

Minor release: Kotlin + Swift language support (17 total), AST-aware git diff, lint and package manager output compression, canonical output types, and expanded rewrite rules.

### Added — Language Support
- **Kotlin** — data classes, sealed classes, coroutines, interfaces (tree-sitter-kotlin-ng)
- **Swift** — protocols, generics, SwiftUI structs (tree-sitter-swift)
- Now 17 languages total (was 15 at v2.0.0)

### Added — AST-Aware Git Diff (`skim git diff`)
- Function-boundary-aware diff rendering with `+`/`-` markers
- `--mode structure` — adds unchanged functions as signatures for architectural context
- `--mode full` — shows entire files with change markers
- `--json` output for machine-readable diff results
- Supports `--staged`, commit ranges (`HEAD~3`, `main..feature`), and all git diff flags

### Added — Lint Output Compression (`skim lint`)
- New subcommand: `skim lint <linter> [args...]`
- Supported linters: ESLint (JSON + text), Ruff (JSON + text), mypy (JSON + text), golangci-lint (JSON + text)
- Canonical `LintResult` output with severity grouping
- Three-tier degradation: Structured → Regex → Passthrough

### Added — Package Manager Output Compression (`skim pkg`)
- New subcommand: `skim pkg <tool> [subcmd] [args...]`
- npm: audit, install, ls, outdated
- pnpm: audit, install, outdated
- pip: install, check, outdated
- cargo: audit
- Input sanitization for terminal escape injection prevention

### Added — Infrastructure
- **Canonical output module** — strongly-typed `TestResult`, `LintResult`, `GitResult` types with `Display` that is compact on success, verbose on failure
- **Expanded rewrite rules** — `skim rewrite` now covers lint and pkg commands

### Changed — Architecture
- Git module refactored into `diff/`, `log.rs`, `status.rs` submodules
- AST diff pipeline: `parse.rs` → `ast.rs` → `source.rs` → `render.rs` → `types.rs`
- Tech debt from PRs #106 and #107 resolved

### Testing
- **1,993 tests passing** (up from 1,594 in v2.0.0 — 25% increase)
- New test suites: `cli_diff.rs`, `cli_e2e_lint_parsers.rs`, `cli_e2e_pkg_parsers.rs`, `cli_e2e_rewrite.rs`
- New fixtures: diff, lint, pkg, C#, Ruby, SQL, Kotlin, Swift

## [2.0.0] - 2026-03-28

Major release: skim evolves from a streaming code reader into a full context optimization engine for AI coding agents. Adds command output compression, agent hook integration, persistent analytics, and MCP server mode.

### Added — Command Output Compression
- **Test Output Parsers** — cargo test (`--message-format=json`), pytest (`--tb=short`), vitest/jest (`--reporter=json`), go test (`-json`) with three-tier degradation: Structured → Regex → Passthrough
- **Git Output Compression** — `skim git` compresses `git status`, `git diff`, `git log` output
- **Build Output Compression** — `skim build` compresses `cargo build`, `cargo clippy`, `tsc` output
- **Three-Tier Parse Degradation** — all parsers gracefully degrade: Structured JSON → Regex fallback → Passthrough (never corrupts output)
- **Output Guardrail** — compressed output is guaranteed never larger than raw input
- **Raw Output Recovery** — on parse failure, original output preserved via tee/recovery system

### Added — Agent Integration
- **`skim init`** — one-command hook installation for 6 AI agents: Claude Code, Cursor, Codex, Gemini, Copilot, OpenCode
- **`skim rewrite`** — declarative command rewriting engine with `--hook` mode for agent integration
- **Compound Shell Support** — rewrite engine handles piped commands and `&&`/`||` chains
- **`skim init --uninstall`** — clean hook removal with SHA-256 integrity verification
- **Multi-Agent Session Support** — discover and learn commands work across all 6 agent session formats

### Added — Token Analytics & Intelligence
- **`skim stats`** — persistent SQLite-based analytics dashboard with per-command savings, daily/weekly trends
- **`skim stats --cost`** — cost estimation with configurable $/MTok rate (`SKIM_INPUT_COST_PER_MTOK`)
- **`skim stats --format json`** — machine-readable analytics export
- **`skim discover`** — scan agent session history to identify missed optimization opportunities
- **`skim learn`** — detect CLI error-retry patterns and generate correction rules
- **Fire-and-forget recording** — analytics never blocks main output path (background threads)

### Added — Infrastructure
- **MCP Server Mode** — native Model Context Protocol integration for agent-native workflows
- **Shell Completions** — `skim completions bash|zsh|fish` for all subcommands
- **Homebrew Distribution** — `brew install dean0x/tap/skim`
- **`skim agents`** — display detected AI agents and their hook/session status (`--json`)
- **Pre-parse Router** — CLI subcommand dispatch without full argument parsing

### Added — Code Reading Enhancements
- **Pseudo Mode** — code-aware output with simplified bodies (human-readable, not just structural)
- **Gitignore Support** — `--no-ignore` override for directory traversal
- **`--last-lines N`** — show last N lines of output (tail mode)

### Changed
- Analytics database at `~/.cache/skim/analytics.db` (SQLite with WAL mode)
- `--clear-cache` clears parser cache only; `skim stats --clear` resets analytics
- Versioned database migrations (v1: token_savings, v2: analytics_meta)

### Architecture
- Extracted `cascade`, `process`, and `multi` modules from monolithic main.rs
- Agent detection centralized via `AgentKind` with SRP extraction
- `AnalyticsStore` trait enables mock-based testing without real database
- `OutputParser` trait standardizes command output compression interface

### Testing
- **1,594 tests passing** (up from 145 in v1.0.0 — 11x increase)
- End-to-end tests for all command compression paths
- Multi-agent session provider tests
- Analytics pipeline integration tests
- Three-tier degradation coverage for all parsers

## [1.0.0] - 2026-03-18

This is the first stable release. All publicly exported types and functions in `rskim-core` are
considered stable from this version forward. Users on `rskim-core = "0.9"` should update their
dependency to `rskim-core = "1.0"`.

### Added
- **Minimal Mode** (`--mode=minimal`) — aggressive transformation for maximum token reduction
- **Token Budget** (`--tokens N`) — cascade through modes to fit a target token count
- **Max Lines** (`--max-lines N`) — AST-aware smart truncation
- **C Language Support** — full C11 with tree-sitter
- **C++ Language Support** — C++20 including templates, namespaces, classes
- **TOML Language Support** — serde-based structure extraction
- **`--lang` alias** for `--language` flag
- **`--filename` flag** for stdin language detection from path
- **Skimmer plugin** for Claude Code — codebase orientation agent

### Changed
- Public API marked stable (`rskim-core` exports considered stable from this release)
- Unified node-kind tables and marker budget handling
- Encapsulated `truncate_to_token_budget` behind stable public API

### Fixed
- Eliminated redundant tokenization and binary search allocation churn
- Wave 1 tech debt cleanup (from issue #28)

### Performance
- Token budget cascade avoids re-parsing and redundant token counting

### Testing
- 145 tests passing (134 unit + 11 doc-tests)
- 12 languages supported: TypeScript, JavaScript, Python, Rust, Go, Java, C, C++, Markdown, JSON, YAML, TOML

## [0.9.0] - 2026-03-16

### Added
- **C Language Support** - Extract structure from C source files using tree-sitter
  - Full C11 support with excellent grammar coverage
  - Automatic language detection for `.c` and `.h` files
  - CLI support: `--language=c` for stdin processing
  - Supports all transformation modes (structure/signatures/types/full)
  - Function body stripping, struct/union/enum preservation
  - Test fixtures: functions, structs, enums, pointers, preprocessor directives

- **C++ Language Support** - Extract structure from C++ source files using tree-sitter
  - C++20 support including templates, classes, namespaces
  - Automatic language detection for `.cpp`, `.hpp`, `.cc`, `.hh`, `.cxx`, `.hxx` files
  - CLI support: `--language=cpp` for stdin processing
  - Supports all transformation modes (structure/signatures/types/full)
  - Class/template/namespace preservation with body stripping
  - Test fixtures: classes, templates, namespaces, inheritance, modern C++ features

- **TOML Language Support** - Extract structure from TOML configuration files
  - Strips all values, keeps only keys and nesting structure
  - Automatic language detection for `.toml` files
  - CLI support: `--language=toml` for stdin processing
  - Security limits: MAX_TOML_DEPTH=500, MAX_TOML_KEYS=10,000
  - Uses toml crate (Strategy Pattern for non-tree-sitter languages)
  - All modes (structure/signatures/types) produce identical output (TOML is data, not code)
  - Test fixtures: Cargo.toml-style configs, nested tables, arrays of tables

### Testing
- **400 total tests** - All passing (up from 186 in v0.8.0)
  - New C language tests (CLI and integration)
  - New C++ language tests (CLI and integration)
  - New TOML language tests (CLI and integration)

## [0.8.0] - 2025-12-06

### Added
- **YAML Language Support** - Extract structure from YAML files for LLM context optimization
  - Strips all values, keeps only keys and nesting structure
  - Multi-document support (preserves `---` separators between documents)
  - Automatic language detection for `.yaml` and `.yml` files
  - CLI support: `--language=yaml` or `--language=yml` for stdin processing
  - 60-80% token reduction for typical YAML files
  - Security limits: MAX_YAML_DEPTH=500, MAX_YAML_KEYS=10,000
  - Uses serde_yaml_ng (maintained fork, Strategy Pattern for non-tree-sitter languages)
  - All modes (structure/signatures/types) produce identical output (YAML is data, not code)
  - Real-world fixtures: Kubernetes manifests, GitHub Actions workflows
  - Note: Anchors/aliases are resolved by serde_yaml_ng parser (not preserved in output)

## [0.7.0] - 2025-11-16

### Added
- **JSON Language Support** - Extract structure from JSON files for LLM context optimization
  - Strips all values, keeps only keys and nesting structure
  - Automatic language detection for `.json` files
  - CLI support: `--language=json` for stdin processing
  - 60-80% token reduction for typical JSON files
  - Security limits: MAX_JSON_DEPTH=500, MAX_JSON_KEYS=10,000
  - Uses serde_json (Strategy Pattern for non-tree-sitter languages)
  - All modes (structure/signatures/types) produce identical output (JSON is data, not code)

### Changed
- **Architecture Improvement** - Strategy Pattern for language-specific parsing
  - `Language::transform_source()` routes each language to appropriate parser
  - Non-tree-sitter languages (JSON, future YAML/TOML) handled cleanly
  - Type-safe Option returns instead of unreachable!() panics
  - Eliminates special-case conditionals in transform function

### Fixed
- Type safety violations in transform modules (replaced unreachable!() with Option types)
- Missing JSON in CLI language argument options

## [0.6.1] - 2025-11-12

### Added
- **ARM64 Linux Support** - Added `aarch64-unknown-linux-gnu` target for Linux ARM64 systems
  - Fixes npm installation on ARM64 Linux (Raspberry Pi, AWS Graviton, etc.)
  - Uses cross-compilation via `cross-rs` for reliable builds
  - npm package now includes `bin/linux/arm64/skim` binary
  - Updated platform support documentation

### Security
- **HIGH**: Semantic version validation to prevent command injection in release workflow
- **CRITICAL**: Pin cross-rs to stable v0.2.5 from crates.io (supply chain hardening)
- **HIGH**: Pin Ubuntu runner to 22.04 for deterministic QEMU version
- **MEDIUM**: Quote glob patterns in artifact extraction (shell injection prevention)
- **MEDIUM**: Use quoted HERE-doc for package.json generation (JSON injection prevention)

### Fixed
- npm installation failure on ARM64 Linux platforms
- Version consistency checks now prevent Cargo.toml/tag mismatches
- Code formatting (rustfmt) across all source files

### Changed
- **Smoke Tests in CI** - All release binaries now verified before publishing
  - Native platform tests execute `--version` and basic transformation
  - ARM64 Linux tested via QEMU emulation
  - Prevents shipping broken cross-compiled binaries
- **Comprehensive Test Suites** - 38 new security regression tests
  - npm wrapper test suite (21 tests) validates platform detection and error handling
  - Version check validation tests (17 tests) ensure regex extraction correctness
  - Tests run automatically in CI before builds
  - Prevents security regressions (command injection, shell injection, JSON injection)
- **Improved npm Error Messages** - Better diagnostics for installation failures
  - Lists all supported platforms explicitly
  - Distinguishes "unsupported platform" from "packaging bug" from "libc mismatch"
  - Suggests `cargo install rskim` workaround when appropriate
  - Detects Alpine Linux (musl) incompatibility and provides guidance
- CI/CD now builds 5 platform targets (was 4)
- Release workflow uses `cross` for ARM64 Linux cross-compilation

## [0.6.0] - 2025-10-23

### Added
- **Directory Support** - Process entire directories recursively
  - Process directories: `skim src/`, `skim .`
  - Recursive traversal of all subdirectories
  - Mixed-language support (process `.ts`, `.py`, `.rs` files in same directory)
  - Works with all existing flags (`--mode`, `--jobs`, `--show-stats`, etc.)
  - Compatible with caching and parallel processing

### Security & Hardening
- **Critical security fix** - Symlink detection bug in directory traversal
  - Changed from `entry.metadata()` to `path.symlink_metadata()`
  - Previous bug: `metadata()` follows symlinks, so `is_symlink()` check never triggered
  - Impact: Directory processing could follow symlinks to sensitive files
  - Now correctly detects and rejects symlinks for security

### Improvements
- **Language Detection** - Auto-detection is now primary, `--language` is fallback
  - Language detection always tries auto-detect from file extension first
  - `--language` flag now acts as fallback/override when auto-detection fails
  - Better experience: Most files "just work" without manual flag
  - Breaking change: None (backward compatible behavior)

### Code Quality
- Added `MAX_JOBS` constant (was magic number 128)
- Extracted `MultiFileOptions` struct to reduce function parameters (8 → 3)
- Added explanatory comments for symlink security checks
- Refactored `process_files()` for reuse by glob and directory inputs

### Documentation
- Updated README with directory examples and real-world use cases
- Added real-world benchmarks: 60-91% token reduction on 80-file codebase
- Clarified auto-detection is default, `--language` is fallback
- Updated CLI help text with directory processing examples
- Refactored documentation structure: moved detailed content to `docs/` folder
- Modernized README messaging and improved storytelling
- Added professional badges (crates.io, npm, downloads, Rust version)

### Testing
- **128 total tests** - All passing (+13 tests from v0.5.0's 115)
  - Added 13 new directory processing tests (`cli_directory.rs`)
  - Tests cover single/mixed languages, recursive directories, edge cases
  - Comprehensive testing of symlink rejection security fix

### Performance
- Maintains <50ms target for individual file processing
- Caching works seamlessly with directory processing
- Parallel processing with `--jobs` flag applies to directories
- Real-world verified: 80-file Next.js codebase processed efficiently

### Breaking Changes
None. All changes are backward compatible.

## [0.5.0] - 2025-10-21

### Added
- **Markdown Support** - Extract document structure from markdown files
  - Support for `.md` and `.markdown` file extensions
  - **Structure mode** - Extracts H1-H3 headers only (document outline)
  - **Signatures/Types modes** - Extracts all headers H1-H6 (complete structure)
  - **Full mode** - Returns original markdown unchanged
  - Supports both ATX headers (`# Title`) and Setext headers (underlined)
  - Auto-detection from file extension

### Internal
- New test fixture: `tests/fixtures/markdown/simple.md`
- Added 10 CLI integration tests for markdown (`cli_markdown.rs`)
- Added 4 core library unit tests for markdown (`integration.rs`)
- Updated `supported_languages()` API to include Markdown
- Added `Language::Markdown` to CLI `LanguageArg` enum

### Security & Hardening
- Added `MAX_MARKDOWN_DEPTH` (500) limit to prevent stack overflow
- Added `MAX_MARKDOWN_HEADERS` (10,000) limit to prevent memory exhaustion
- Improved setext header detection using AST instead of text matching
- Depth tracking in markdown AST traversal

### Dependencies
- **Updated** tree-sitter from 0.23 to 0.25 (ABI 15 support)
- **Updated** tree-sitter-javascript from 0.23 to 0.25
- **Updated** tree-sitter-python from 0.23 to 0.25
- **Updated** tree-sitter-go from 0.23 to 0.25
- **Added** tree-sitter-md 0.5 (markdown grammar)

### Testing
- **115 total tests** - All passing (+14% increase from v0.4.0's 101 tests)
  - 10 new markdown CLI tests
  - 4 new markdown integration tests
  - All existing tests continue to pass

### Breaking Changes
None. Markdown support is additive and auto-detected by file extension.

## [0.4.0] - 2025-10-17

### Added
- **Multi-file Glob Support** - Process multiple files with wildcard patterns
  - Glob pattern matching: `skim 'src/**/*.ts'`, `skim '*.{js,ts}'`
  - File header separators for multi-file output
  - `--no-header` flag to disable headers in multi-file mode
  - Recursive directory traversal with glob patterns

- **Parallel Processing** - Rayon-powered multi-core processing
  - `--jobs` flag for configurable parallelism (default: number of CPUs)
  - 2.4x speedup demonstrated with `--jobs 4`
  - Efficient thread pool management
  - Scales linearly with CPU cores

- **File-based Caching** - Massive speedup on repeated processing
  - **Enabled by default** for 40-50x speedup on cached reads
  - SHA256 cache keys with mtime-based invalidation
  - Platform-specific cache directory (`~/.cache/skim/`)
  - `--no-cache` flag to disable caching
  - `--clear-cache` command to clear cache directory
  - Smart invalidation on file modification

- **Token Counting** - Measure LLM context window savings
  - `--show-stats` flag shows token reduction statistics
  - Uses tiktoken with cl100k_base encoding (GPT-3.5/GPT-4 compatible)
  - Works with single files, globs, and stdin
  - Aggregates stats across multiple files
  - Output to stderr for clean piping

### Performance
- **Verified benchmarks**: 14.6ms for 3000-line files (3x faster than 50ms target)
- **Cached reads**: 5ms average (40-50x speedup)
- **Parallel processing**: 2.4x speedup with 4 cores
- **Token reduction**: 60-95% depending on mode

### Internal
- New module: `crates/rskim/src/cache.rs` - Caching implementation
- New module: `crates/rskim/src/tokens.rs` - Token counting with tiktoken
- Major refactor: `crates/rskim/src/main.rs` - Integrated all Phase 3 features
- Architecture cleanup: Removed unused exports, clarified core/CLI boundaries
- Dependencies added: glob, rayon, dirs, serde, serde_json, sha2, tiktoken-rs

### Documentation
- Updated all READMEs with Phase 3 features
- Updated CLAUDE.md to reflect 100% completion (70 tests passing)
- Updated CONTRIBUTING.md with accurate crate names and performance targets
- Fixed benchmark imports for consistency

### Security & Hardening
- **Path traversal prevention** - Glob patterns reject absolute paths and `..` components
- **Symlink filtering** - Glob processing skips symlinks to prevent sensitive file access
- **Secure cache permissions** - Cache directory set to 0700, files to 0600 (Unix)
- **Integer overflow protection** - Fixed overflow in token reduction calculation for edge cases

### Performance Optimizations
- **Lazy tokenizer initialization** - Using `OnceLock` to avoid recreating tokenizer on every call
- **Token count caching** - Extended `CacheEntry` struct to store token counts, eliminating double file reads
- **Improved glob validation** - Added `--jobs` upper bound validation (max 128) to prevent resource exhaustion

### Code Quality Improvements
- **Named return types** - Replaced tuple returns with `ProcessResult` struct for clarity
- **Reduced function parameters** - Created `ProcessOptions` struct (5 params → 1 struct)
- **Helper functions** - Extracted `report_token_stats()` to eliminate code duplication
- **Clippy fixes** - Renamed `Mode::from_str()` to `Mode::parse()` to avoid standard library conflicts
- **Lifetime cleanup** - Removed unnecessary lifetime annotations

### Dependencies
- **Updated** tiktoken-rs from 0.5 to 0.7 (latest stable)
- **Updated** dirs from 5.0 to 6.0 (latest stable)

### Testing
- **101 total tests** - All passing (+44% increase from v0.3.3's 70 tests)
  - 8 unit tests
  - 19 CLI tests
  - 10 glob pattern tests (NEW)
  - 9 caching tests (NEW)
  - 12 token stats tests (NEW)
  - 11 rskim-core tests
  - 24 integration tests
  - 8 doc tests
- Verified parallel processing with CPU usage tests
- Verified caching with repeated file processing
- Verified token counting accuracy
- Comprehensive glob security testing (path traversal, symlink rejection)

### Breaking Changes
None. All new features are opt-in via CLI flags.

## [0.3.3] - 2025-10-16

### Fixed
- **CLI README (crates.io)** - Critical branding and command errors
  - Title changed from "# rskim" to "# Skim" (official brand name)
  - Overview text changed from "rskim transforms..." to "**Skim** transforms..."
  - Fixed broken npx commands: `npx skim file.ts` → `npx rskim file.ts` (2 occurrences)

**Context**: The CLI README is displayed on crates.io and was showing incorrect branding and broken commands that would not work.

**Important distinction:**
- **Brand name**: Skim (official name)
- **Package name**: rskim (for `npm install -g rskim`, `npx rskim`, `cargo install rskim`)
- **Binary name**: skim (after installation: `skim file.ts`)

## [0.3.2] - 2025-10-16

### Fixed
- **Main README** - Project status showed outdated version (v0.2.3 → v0.3.1)
- **Main README** - Planned features example still used old binary name (`rskim` → `skim`)
- **Core library README** - Dependency version example showed `"0.2"` instead of `"0.3"`
- **Core library** - Doc tests and integration tests used wrong crate name (`skim_core` → `rskim_core`)
  - Affected files: `lib.rs`, `types.rs`, `integration.rs`, `transform/mod.rs`
  - All doc examples now use correct `rskim_core` import
  - Fixed unused import warning in transform module

**Context**: Documentation and naming issues discovered after v0.3.1 release. The `skim_core` references were remnants from original project naming before the v0.2.1 rename to `rskim`.

## [0.3.1] - 2025-10-16

### Fixed
- CLI README documentation still referenced old language names (`type-script`, `java-script`)
- Test files using incorrect language flag format (should be `typescript`, not `type-script`)
- Test version assertion updated to match current version (0.3.0 → 0.3.1)

**Context**: These issues were overlooked in v0.3.0 release. Language names were changed to lowercase in v0.2.4, but some documentation and test references weren't updated.

## [0.3.0] - 2025-10-16

### Changed
- **BREAKING:** Binary name changed from `rskim` to `skim`
  - Installation still uses `rskim`: `npm install -g rskim` or `cargo install rskim`
  - Command usage now uses `skim`: `skim file.ts` (shorter, cleaner)
  - Official branded name: **Skim**
  - Package name remains `rskim` to avoid conflicts

### Migration
```bash
# Installation (unchanged)
npm install -g rskim
cargo install rskim

# Old command (v0.2.x)
rskim file.ts

# New command (v0.3.0+)
skim file.ts
```

**Rationale**: Shorter command for daily use. Package name `rskim` avoids npm/crates.io namespace conflicts.

## [0.2.4] - 2025-10-16

### Fixed
- **BREAKING:** Language flag names now use lowercase instead of kebab-case
  - `--language=type-script` → `--language=typescript`
  - `--language=java-script` → `--language=javascript`
  - Short aliases still work: `--lang=ts`, `--lang=js`
- All README files updated to reflect current state (npm live, correct package names)
- CHANGELOG now includes all historical versions (0.2.1, 0.2.2, 0.2.3)
- Error message fixed: `skim` → `rskim`

### Changed
- Installation documentation now recommends `npx` for trial usage
- Clarified npx performance trade-offs (~100-500ms overhead per invocation)

## [0.2.3] - 2025-10-15

### Fixed
- npm wrapper script syntax error (template literal escaping)
- Binary now works correctly when installed via npm

## [0.2.2] - 2025-10-15

### Added
- npm distribution via GitHub Actions
- Automated cross-platform binary building (Linux, macOS x64/ARM, Windows)
- npm package published as `rskim`

### Fixed
- GitHub Actions workflow for npm publishing

## [0.2.1] - 2025-10-15

### Changed
- **BREAKING:** Renamed all packages to `rskim` for consistency
  - `skim-core` → `rskim-core`
  - `skim-cli` → `rskim` (binary also renamed)
  - Updated repository URLs to https://github.com/dean0x/skim
- Simplified distribution strategy: native CLI only (removed WASM)
- Configured cargo-dist for npm distribution as `rskim`

### Migration Guide
```bash
# Old (v0.1.0)
cargo install skim-cli

# New (v0.2.0+)
cargo install rskim

# Or via npm
npm install -g rskim
npx rskim file.ts  # no install required
```

## [0.1.0] - 2025-10-15

### Added
- 🎉 **Initial release** - Production-ready CLI tool

**Core Features:**
- Multi-language support: TypeScript, JavaScript, Python, Rust, Go, Java
- Four transformation modes: structure (70-80%), signatures (85-92%), types (90-95%), full (0%)
- CLI with stdin support and language auto-detection
- UTF-8/Unicode support (emoji, Chinese, multi-byte characters)
- Streaming output to stdout for pipe workflows

**Testing:**
- 62 total tests (11 unit, 24 integration, 19 CLI, 8 doc tests)
- 100% test pass rate
- Performance benchmarking suite with criterion
- Real-world testing on complex codebases

**Security:**
- Stack overflow protection (max recursion depth: 500)
- Memory exhaustion protection (max input: 50MB, max nodes: 100k)
- UTF-8 boundary validation (prevents panics)
- Path traversal protection (rejects `..` components)
- DoS-resistant with comprehensive input validation

**Developer Experience:**
- Comprehensive error messages
- Help and version flags
- Language detection with file extensions
- Explicit language override with `--language` flag

### Fixed
- Overlapping replacements bug in structure mode (nested functions)
- Path traversal validation (now allows absolute paths correctly)
- tree-sitter version compatibility (pinned to 0.23.x)
- Removed duplicate parser implementation
- Cleaned up unused code warnings

### Technical
- Zero-copy string operations where possible
- Streaming stdout output with buffering
- Error-tolerant parsing (handles incomplete/broken code gracefully)
- No panics in library code (enforced by clippy lints)
- Clean builds with comprehensive test coverage

---

## Version History

- **2.10.0** (2026-05-13): Container/cloud/database compression, search crate foundation, heatmap insights (3,558 tests)
- **2.9.0** (2026-05-08): Heatmap analysis, system utility parsers, curl hardening (3,310 tests)
- **2.8.0** (2026-05-07): Flat dispatch, Crush agent, multi-file args (3,103 tests)
- **2.7.0** (2026-05-01): Line numbers, session tracking, output sanitization (3,002 tests)
- **2.6.0** (2026-04-27): Terminal UX overhaul, non-interactive init, plugin ecosystem removal (2,883 tests)
- **2.5.1** (2026-04-20): Hook safety, SKIM_PASSTHROUGH bypass, npx fallback, 5 new parsers (2,800 tests)
- **2.5.0** (2026-04-17): Formatter output compression, 8 new lint parsers (2,629 tests)
- **2.4.1** (2026-04-15): Stats dashboard redesign, weighted %, by-command breakdown, --cost deprecation (2,482 tests)
- **2.4.0** (2026-04-14): GitHub CLI compression, git subcommand completion, quality improvements (2,482 tests)
- **2.3.1** (2026-04-09): Discover/rewrite alignment, rewritable gap closures, stats bar fix
- **2.3.0** (2026-04-08): Stats dashboard v3, debug-gated warnings, git fetch compression, parse tier tracking
- **2.2.0** (2026-04-06): File, log, infra output compression (12 new parsers), learn fix, rewrite/discover integration
- **2.1.0** (2026-04-01): Kotlin + Swift, AST-aware git diff, lint/pkg compression, canonical output
- **2.0.0** (2026-03-28): Context optimization engine — command compression, agent hooks, analytics, MCP server
- **1.0.0** (2026-03-18): First stable release — minimal mode, token budgets, max-lines, C/C++/TOML, skimmer plugin
- **0.9.0** (2026-03-16): C, C++, and TOML language support (12 languages total, 400 tests)
- **0.8.0** (2025-12-06): YAML language support
- **0.4.0** (2025-10-17): Multi-file glob support, caching, parallel processing, token counting (Phase 3 complete)
- **0.3.3** (2025-10-16): CLI README branding and broken npx command fixes
- **0.3.2** (2025-10-16): README documentation alignment fixes
- **0.3.1** (2025-10-16): Hotfix for remaining language name references in docs/tests
- **0.3.0** (2025-10-16): Binary renamed to `skim`, package remains `rskim`
- **0.2.4** (2025-10-16): Fixed language flag names, updated all documentation
- **0.2.3** (2025-10-15): Fixed npm wrapper script syntax
- **0.2.2** (2025-10-15): npm distribution via GitHub Actions
- **0.2.1** (2025-10-15): Renamed package to rskim with comprehensive documentation
- **0.1.0** (2025-10-15): Initial release as skim-cli

---

## Links

- [Repository](https://github.com/dean0x/skim)
- [Issues](https://github.com/dean0x/skim/issues)
- [Security Policy](SECURITY.md)
- [Contributing Guide](CONTRIBUTING.md)
