---
feature: cmd-search
name: Search CLI (skim search subcommand, the scan engine)
description: "Use when modifying the skim search CLI, adding or changing a search flag, changing how files are listed, read or left out, changing the labelled view, the word view, the roles, the ranking or the text and JSON output, changing the exit codes, changing project-root discovery, or changing the removal of old search hooks and old index data. Keywords: skim search, scan engine, labelled view, word view, roles, def, call, ref, import, partial, unlabelled, left_out, universe, Containment, sensitive_admission, SENSITIVE_PATH_POLICY, Limits, MAX_STORED_LINES, parse budget, count, label, Matcher, closed flag set, REMOVED_FLAGS, resolve_scan_root, RootRules, GIT_CEILING_DIRECTORIES, hooks.rs, legacy_cache.rs, LEGACY_ARTIFACTS, clear-cache, exit 141."
category: architecture
directories: [crates/rskim/src/cmd/search/]
referencedFiles:
  - crates/rskim/src/cmd/search/mod.rs
  - crates/rskim/src/cmd/search/args.rs
  - crates/rskim/src/cmd/search/filter.rs
  - crates/rskim/src/cmd/search/universe.rs
  - crates/rskim/src/cmd/search/matcher.rs
  - crates/rskim/src/cmd/search/scan.rs
  - crates/rskim/src/cmd/search/classifier.rs
  - crates/rskim/src/cmd/search/roles.rs
  - crates/rskim/src/cmd/search/rank.rs
  - crates/rskim/src/cmd/search/render.rs
  - crates/rskim/src/cmd/search/gitdir.rs
  - crates/rskim/src/cmd/search/hooks.rs
  - crates/rskim/src/cmd/search/legacy_cache.rs
created: 2026-06-21
updated: 2026-11-13
version: 9
---

# Search CLI (skim search subcommand, the scan engine)

## Rules

- **KB-INV-1** A query reads the files and builds nothing. It writes no file, keeps no cache and records no analytics
  row. It starts no other program: the git index is read in process.
- **KB-INV-2** The flag set is closed. `parse_flags` accepts only the flags in the help text. An unknown flag, a removed
  flag, a second pattern or a flag that cannot combine with another exits 2. A removed flag prints nothing on stdout, with or without `--json`. Any other usage error prints
  nothing on stdout, except with `--json`, where it prints one `{"error":"…"}` object.
- **KB-INV-3** Every file the query does not read is counted under one `LeftOut` reason and shown in the last line and
  in the JSON `left_out` object. No file leaves the universe silently.
- **KB-INV-4** Every bound is a named constant. A bound that hides lines prints a note with exact counts and the remedy.
  Nothing is dropped without a count.
- **KB-INV-5** Roles come from syntax. The `def` role reuses the `rskim-core` declaration rule (`Declaration.name_bytes`,
  `DeclScan.truncated`, `Parser::parse_bounded`). A call is a hit in callee position. Calls are not split by definition:
  a name with two or more definitions gets a note.
- **KB-INV-6** A tracked path and the work tree are untrusted. `Containment` canonicalizes each parent folder, requires it
  to stay inside the canonical root, and opens the file by that parent plus the file name with no link followed and no
  block on a FIFO. The opened handle is checked before a byte is read.
- **KB-INV-7** Text output never prints a control byte or a bidi format control (U+202A to U+202E, U+2066 to U+2069) from
  a file name or a line (`render::quote_path`, `escape_text`). A file name that starts with a quote is C-quoted too, so no
  name looks like the quoted form of another. Letters of a right-to-left script print as they are. JSON keeps the serde
  escapes and marks a path that is not UTF-8 with `"path_lossy": true`.
- **KB-INV-8** The hidden-file and secret-file rule lives in one function (`sensitive_admission`) and one constant
  (`SENSITIVE_PATH_POLICY`) in `universe.rs`. Today every tracked file is listed.
- **KB-INV-9** The old hook block and the old index data are removed only by `skim init`, `skim init --uninstall` and
  `skim --clear-cache`. A query never calls `hooks` or `legacy_cache`. `skim doctor` reports both and exits the same.
- **KB-AP-1** Do not add a flag that starts a program or reads a file the query did not list. The closed flag set and the
  `Containment` open are the trust boundary.
- **KB-AP-2** Do not decide a bound's behaviour in the output code, and do not infer the cause of a cut from sums. Phase A
  of `scan` assigns the stored-line quota from the exact counts, in parse order. Phase B spends the context budget in
  parse order, on the lines each file needs. Each cap that cuts lines is recorded where it cuts (`CapsHit` in
  `ScanOutput`; `context_hidden` for the context cap), so the result does not depend on thread scheduling and the output
  only reads the record.
- **KB-AP-3** Do not treat a name that is missing from the declaration scan as absent when `DeclScan.truncated` is true.
  The file gets the `walk_cap` reason under `unlabelled`.
- **KB-AP-4** Use only two stderr forms: `skim search: <message>` (always on) and `skim search [debug]: <message>` (only
  with `SKIM_DEBUG=1`, through `crate::debug_log!`). Add no sub-qualifier. The one exception is the shared elision
  marker (`[skim] lines elided beyond N of M — ...`) that `output::elision_marker_with_remedy` gives a bound that hides
  lines.

## Overview

`skim search PATTERN` finds where a name is defined and where it is used. It reads the files at each query. There is no
index to build and nothing is cached or written. Two views answer two kinds of question:

- **Labelled view** (one pattern, for a bare identifier the default): the `def` lines first with their line range, a
  clash note when a name has two or more definitions, then the calls with the function that holds each one, then the
  other roles as counts (`--all` lists them, `--full` prints long sections whole).
- **Word view** (a quoted argument with several words): `files: N hold all M words; K listed`, then per file one line with
  the word counts and the best lines. Files that hold all words come first. If fewer than `FALLBACK_MIN_FILES` (3) files
  hold all words, files with fewer words follow.

Exit codes of a query: `0` a line matched, `1` no line matched, `2` a usage or input error, `141` on unix when the reader
of stdout closed the pipe (`pipe_closed_exit`).

## Module Structure

`crates/rskim/src/cmd/search/`:

| Module | Job |
|---|---|
| `mod.rs` | The only module that does I/O. `run`, `resolve_scan_root`, `build_query`, `search`, `emit`. |
| `args.rs` | `parse_flags`, `SearchAction`, `QueryFlags`, `QueryShape`, `REMOVED_FLAGS`, `SEARCH_HELP_TEXT`, `is_bare_identifier`. |
| `filter.rs` | `PathFilter`: the `-g` and `-t` filters, with the rules of `rg` from the `ignore` crate. |
| `universe.rs` | `list_universe`, `Containment`, `LeftOut`, `SensitivePolicy`, `sensitive_admission`. |
| `matcher.rs` | `Matcher`: one compiled pattern, the case rules, the whole-word rule, the regex size limits. |
| `scan.rs` | `count` (phase A) and `label` / `into_counts` (phase B), `Limits`, `ScanOutput`. |
| `classifier.rs`, `field.rs`, `fields/` | The syntactic field of a byte range: comment, string, code (a name or a body, kept so that the innermost range wins). The role rules read only the comment and string fields. Markdown and data formats have their own classifiers. |
| `roles.rs` | `Role`, `Labels`, `classify_line`, `Unlabelled`: the role of each hit. |
| `rank.rs` | `FilePrior`, `rank_labelled`, `rank_words`: the rank order of both views. |
| `render.rs` | `render`: the text views, `-l`, `-c` and the JSON envelope. Pure: it reads no file and writes no stream. |
| `gitdir.rs` | `RootRules`, `discover_project_root_with`, `resolve_git_dir`, `resolve_repo_toplevel`, `resolve_common_dir`. |
| `hooks.rs` | Removal of the search git hooks that skim 2.x wrote. |
| `legacy_cache.rs` | Removal of the index data that skim 2.x cached. |

Tests: `*_tests.rs` beside each module, `run_tests.rs` (root rule, query compile, output form), and the integration
files `crates/rskim/tests/cli_search.rs` (the real binary on small git repositories), `cli_search_security.rs`
(untrusted input: flags that start a program, paths that leave the root, caps, odd path names, files that block a
reader) and `cli_search_legacy.rs` (every removed flag exits 2 with nothing on stdout and nothing written).

## CLI Flag Surface

`-i`, `-S` (smart case), `-w`, `-F`, `-E`, `-g GLOB`, `-t TYPE` (repeat both; a leading `!` leaves files out; at most
`MAX_FILTERS` = 64 together), `-l`, `-c`, `-C N` (0 to `MAX_CONTEXT` = 100), `--all`, `--full`, `--json`, `--limit N`
(at most `MAX_LIMIT` = 1,000,000), `--offset N`, `--root DIR`, `-n` (accepted, does nothing). Short boolean flags join
(`-iwF`). `-l` and `-c` cannot combine with `--json`.

- A pattern with ASCII whitespace and no `-F` or `-E` is a list of words: split on whitespace, drop empty parts, keep the
  first copy of each, at most `MAX_WORDS` = 16. One word left is a plain pattern.
- `is_bare_identifier` (`^[\p{XID_Start}_$][\p{XID_Continue}$]*$`) selects the labelled view without `--all`.
- `REMOVED_FLAGS` holds the 15 flags that exit 2 with the replacement named: `--ast`, `--hot`, `--cold`, `--risky`,
  `--blast-radius`, `--weights`, `--phrase`, `--near`, `--build`, `--rebuild`, `--update`, `--stats`, `--install-hooks`,
  `--remove-hooks`, `--lang`. `-j` has its own message. The message quotes the flag name from the table, never the
  argument text, so an attached value cannot reach the terminal.

## Dispatch Flow (`mod.rs`)

1. `args::parse_flags` gives `Help`, `Removed(message)` or `Query(flags)`. A parse error is `fail`: exit 2, the message on
   stderr, and with `--json` the `{"error": …}` object on stdout.
2. `resolve_scan_root`: with `--root` the folder must exist. Without it the root is the repository that holds the current
   folder, found by `gitdir::discover_project_root_with` under `RootRules`; outside a repository it is the current folder
   alone. A notice on stderr names the root when it is not the current folder. The current folder `/` needs `--root /`.
3. `search`: `build_query` (compile the matcher, under the regex size limits), `PathFilter::new`, `universe::list_universe`,
   `listing.retain(filter)`, `scan::count` (phase A), then `label()` for the lines view or `into_counts()` for `-l` and
   `-c`, then `render::render`. Per-stage timings go through `crate::debug_log!`.
4. `emit`: stdout first, then the notes on stderr. A closed pipe at any write gives exit 141.

## The Universe (`universe.rs`)

- **The set.** Every git-tracked regular file, read from the git index in process through `gix` (gitlinks and
  skip-worktree entries are dropped), plus the files of an ignore-aware walk of the root (hidden and ignored files are
  skipped, links are not followed). A root that is a folder inside a repository (`--root`) lists the tracked files below
  it, read from the index of the repository that holds it (`tracked_source`). Outside a repository the walk is the
  whole set. There is no language allow-list, no minified gate and no file cap. Hidden and secret-bearing tracked files
  are listed (policy A).
- **Left out.** `LeftOut`: `binary` (a NUL byte in the first `BINARY_PROBE_BYTES` = 8192), `not_utf8`, `too_large`
  (above `MAX_FILE_BYTES` = 5 MiB), `unreadable` (an I/O error, a permission error, or a handle that is not a regular
  file), `vanished` (tracked but absent), `outside_root` (the real path leaves the canonical root, or the index names a
  path that is not a plain relative path) and `symlink` (a tracked mode `120000` entry or a walked link, counted and
  never read).
- **Policy A.** `SENSITIVE_PATH_POLICY = SensitivePolicy::List` and `sensitive_admission` admit every path. To leave
  hidden or secret-bearing tracked files out, add a variant, change the constant and the match in the function, and
  update the help text, README, CHANGELOG and the scoreboard text universe together. `-g '!NAME'` leaves one file out
  today.
- **Errors.** `UniverseError::GitIndex` when the root has a `.git` entry and its index cannot be read: exit 2.

## Scan (`scan.rs`)

One rayon pool of at most `MAX_SCAN_THREADS` = 8 threads. Two phases run one after the other.

- **Phase A (`count`)** reads every admitted file once. It counts the matching lines exactly (per word for a word
  query), notes whether a matching line holds a declaration keyword before the match, takes the file prior and a digest of
  the content. It keeps no line text, so memory is O(files).
- **Phase B (`label`)** visits the matched files in parse order. It re-reads each file and compares the digest (a file
  that changed is counted under `changed` and not listed), labels the file with one parse, decides the role of
  every matching line and stores line records up to a fixed cap. Role counts are exact for every matching line, stored
  or not. With `-C` it then keeps the context lines of the stored records: the files run in waves of at most
  `MAX_SCAN_THREADS`, and after each wave the context budget goes to the files of the wave in parse order, on the lines
  each file needs. A wave holds the text of its files, so that memory is at most `MAX_SCAN_THREADS` files.

`Limits` (all named constants): `MAX_STORED_LINES` = 10,000; `MAX_CONTEXT_LINES` = 100,000; `PARSE_BUDGET_FILES` = 500;
`PARSE_BUDGET_BYTES` = 32 MiB; `PER_FILE_PARSE_CAP` = 1 MiB; `MAX_DEF_SITES_PER_FILE` = 100 (the
`Limits::max_def_sites_per_file` field); `MAX_DEF_SITES` = 10,000; `LINE_WIDTH_CAP` = 300. A file the budget leaves
unparsed keeps its lines under `unlabelled` with a reason (`Unlabelled`: `budget`, `parse_cap`, `parse_failed`,
`changed`, `walk_cap`). No bound reads the clock: the labels, and so the order, depend only on the files, so a page or
a repeat call under load gives the same answer.

The stored-line quota of a file counts every matching line, definitions included, because phase A cannot tell them
apart; the definitions have their own two caps. `ScanOutput.caps` (`CapsHit`) says which of three caps cut a line: the
stored-line cap, the per-file definition cap and the definition cap over all files. The note names them
(`stored-line cap`, `per-file definition cap`, `definition cap`) and the stderr marker joins the remedy of each cap that
fired. The per-file cap has its own remedy, `read the file with skim FILE --mode=signatures`, because `-c`, `-l` and a
narrower pattern do not show the cut definitions of one file.

## Roles (`roles.rs`)

`Role` is in tier order: `def`, `call`, `ref`, `unlabelled`, `import`, `string`, `comment`, `text`, `partial`. A line takes
the best role among its matches. Rule order for one match: `def` when the match range equals the name range of a
declaration; `call` when the identifier is in a callee position; `comment` or `string` from the classifier field;
`import` when an ancestor is an import node; `ref` for any other code. A file with no grammar, and Markdown prose, is
`text`. A hit with an identifier character before or after it, in an identifier query, is `partial` (decided from the
line, no parse). `MAX_CALLEE_HOPS` = 8 and `MAX_IMPORT_HOPS` = 256 bound the ancestor walks.

Known limit: a call written inside a macro argument (a Rust token tree) is labelled `ref`, because the tree has no call
node there.

## Rank (`rank.rs`)

- **Labelled view** key: best-role tier ascending, file prior ascending, matched lines descending, path ascending.
- **Word view** key: words matched descending, file prior ascending, the sum of `ln(1 + count)` over the words
  descending, path ascending. The default is `WORD_VIEW_DEFAULT_LIMIT` = 10 files, `WORD_VIEW_BEST_LINES` = 2 lines each.
- `FilePrior` (source, test, docs, changelog, generated) is decided by the first rule that fits: generated markers
  (the first `GENERATED_PROBE_LINES` lines or file names), changelog names, docs folders and extensions, test folders,
  then source.
- One function per view computes the key and the score together, so the score never increases down the ranked list.

## Output (`render.rs`)

- Text: `def` lines, the clash note, the `calls:` section (each line `path:line: [holder] text`), an `other:` line for the
  roles the default view only counts, and the `total:` line (`N lines in M files, all listed or counted; K files
  searched, L left out`).
- JSON: one compact line. Keys: `query`, `match_mode`, `case`, `view`, `total_lines`, `total_files`, `has_more`,
  `definitions` (`count`, `at_least`, `sites`), `files` (`path`, `rank`, `score`, `prior`, `labelled`, `words_matched`,
  `words_total`, `line_count`, `roles`, `occurrences`), `searched_files`, `left_out` (the seven keys), `parse`, `cost`,
  `notices` and `duration_ms`. An error is `{"error": "…"}`.
- Bounds print notes with the remedy: `CAP_REMEDY` ("use -c or -l for every file, or narrow the pattern") and
  `CONTEXT_REMEDY` ("use a smaller -C, or narrow the pattern").
- `--limit` and `--offset` page the files in rank order; `has_more` states whether more files follow.

## Root Discovery (`gitdir.rs`)

`discover_project_root_with(canonical, rules)` walks up at most `MAX_ANCESTORS` = 256 folders for the nearest `.git`
entry. `RootRules` stops the walk: it never steps into a `GIT_CEILING_DIRECTORIES` entry (absolute paths, at most
`MAX_CEILING_ENTRIES` = 64, canonical forms added), never reaches the home folder from below and never reaches `/`
from below. When the variable holds more than 64 absolute entries (`ceiling_overflow`), the walk does not go up at
all and the query prints a note. The start folder is never excluded. With no match the start folder is the root.
`resolve_git_dir`, `resolve_repo_toplevel` and `resolve_common_dir` serve `hooks.rs` (linked worktrees, submodules and
plain repositories resolve the way git does). `resolve_repo_toplevel` also gives `universe.rs` the repository that
holds a `--root` folder.

## Old Data Cleanup

- **`hooks.rs`**: `find_search_hooks` and `remove_search_hooks` handle the block between `# skim-search-start` and
  `# skim-search-end` in `post-commit`, `post-merge` and `post-checkout` (`HOOK_NAMES`). Removal deletes the lines from
  the start marker to the end marker and no other byte. A marker counts only as a whole line. A hook that is a link, is
  not UTF-8 or is larger than `MAX_HOOK_BYTES` (1 MiB) is not skim's and stays. The rewrite keeps the file permissions.
  Callers: `cmd/init/install.rs` (`skim init` and `skim init --uninstall`, with a dry run). `skim doctor` prints an
  advisory line through `cmd::init::old_search_hooks_dir` and never changes its exit code for it.
- **`legacy_cache.rs`**: `LEGACY_ARTIFACTS` names the 12 files skim 2.x wrote in `<cache>/search/<id>/` (`<id>` is 16
  lowercase hex digits). `clear_legacy_index_data` removes only those names, only as regular files in a folder with a
  valid `<id>`, then each empty project folder, then `<cache>/search` when empty. No step follows a link; one pass
  inspects at most `MAX_SEARCH_ENTRIES` = 4096 entries, and when more follow (`LegacyCleanup.more`) the clear prints a
  note to run it again. Callers: `cmd/mod.rs` (`skim --clear-cache`) and
  `cmd/doctor/mod.rs` (`legacy_index_report`, an advisory line with the size; it has the same bound, and when
  `LegacyReport.more` is set the line says "at least" for the folders and the size).

## Gotchas

- `cargo test -p rskim` spawns nested cargo runs; scope with `--bins` or `--test cli_search`.
- The help text pins its policy wording in `args_tests.rs`. Change both together.
- `REMOVED_FLAGS` and the legacy artifact list are data that tests pin (`cli_search_legacy.rs`, `legacy_cache_tests.rs`).
  Removing a row is a behaviour change.
- The gate for this module is the search scoreboard. A change to a view, a role rule, a bound, the universe or an exit
  code needs the scoreboard to stay green (see `search-scoreboard`).
- `hooks.rs` and `legacy_cache.rs` keep the old marker text and artifact names on purpose: removal must recognise what
  skim 2.x wrote.

## Key Files

- `crates/rskim/src/cmd/search/mod.rs`: dispatch, `resolve_scan_root`, `emit`.
- `crates/rskim/src/cmd/search/args.rs`: flags, `REMOVED_FLAGS`, `SEARCH_HELP_TEXT`.
- `crates/rskim/src/cmd/search/universe.rs`: the file set, `Containment`, `LeftOut`, policy A.
- `crates/rskim/src/cmd/search/scan.rs`: phase A and phase B, `Limits`.
- `crates/rskim/src/cmd/search/roles.rs`, `classifier.rs`: roles and fields.
- `crates/rskim/src/cmd/search/rank.rs`, `render.rs`: order and output.
- `crates/rskim/src/cmd/search/gitdir.rs`: root discovery.
- `crates/rskim/src/cmd/search/hooks.rs`, `legacy_cache.rs`: old data removal.
- `crates/rskim/tests/cli_search.rs`, `cli_search_security.rs`, `cli_search_legacy.rs`: the real-binary tests.

## Related

- Feature knowledge `search-scoreboard`: the gate that judges this module, and its independent oracles.
- Feature knowledge `symbol-read`: the declaration rule and `Parser::parse_bounded` that the `def` role reuses.
