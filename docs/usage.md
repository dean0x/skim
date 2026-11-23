# Usage Guide

## Command Syntax

```bash
skim [FILE|DIRECTORY] [OPTIONS]
```

## Arguments

### Input Types

- **Single file**: `skim file.ts` (auto-detects language from extension)
- **Directory**: `skim src/` (recursively processes all supported files)
- **Glob pattern**: `skim 'src/**/*.ts'` (processes matching files)
- **Several arguments**: `skim b.ts 'src/*.ts' a.ts` prints the files in command-line order, each after a `// path` header. A glob or a directory is sorted by path inside its own argument. A direct `skim` list compares resolved path text and keeps the first match. `z.ts` and `./z.ts` count as two paths. A glob such as `'*.ts'` adds `./`, so `skim z.ts '*.ts'` can print the same file twice. Rewritten `cat`, `head`, and `tail` commands keep repeated operands, as the original commands do. `SKIM_PASSTHROUGH=1` uses the same order.
- **Stdin**: `skim -` (`--language` recommended; else skim tries a shebang, then raw text within the default bound)

## Options

### Transformation Mode

```
-m, --mode <MODE>
```

Transformation mode [default starting mode: structure]

#### Default single-input limit

With no `--mode`, `--max-lines`, `--last-lines`, `--tokens` or `--symbol`, `skim FILE` and `skim -` have a default bound.
Stdout plus its view notice stays strictly below **29,000 characters** (Unicode scalar values, not bytes).
The count includes source line numbers and the elision marker.
The bound applies to every output destination: terminals, pipes and file redirects.
Skim tries **structure → signatures → types** and serves the first non-empty view that fits.
JSON, YAML and TOML keep their structure view instead of this step-down.

If no view fits, skim cuts the last non-empty view on whole lines.
Over-limit raw text also gets whole-line cuts.
The last stdout line counts hidden source lines with a complete source map, or hidden view lines without one.
One stderr notice names the served mode, the limit and any cut.
A warm cache read keeps the cold read's output and notice.

Use `--symbol NAME` for one declaration in a file.
Use `--max-lines N` for a line limit on files or stdin.
Use a typed `--mode`, such as `--mode structure`, to keep that mode without the character bound.
Use `SKIM_PASSTHROUGH=1` for raw file or stdin output.
Limits in other agents are not known.
Optional `--show-stats` and `SKIM_DEBUG` lines are outside the count.

#### Default multi-file limit

A default directory, glob or file list uses the same limit for the whole run.
A directory or glob with one match, or a list that resolves to one path, still uses the run limit.
The limit counts stdout, including headers, line numbers and markers, plus the view notice.
File errors, warnings, the `Processed` summary, `--show-stats` and `SKIM_DEBUG` do not count.

Skim tries structure, signatures and types for the run. If none fits, skim cuts whole lines and can omit trailing files in command-line order.
Cut markers count hidden view lines. The last stdout line counts omitted files, even with `--no-header`.
Use `--tokens N`, a typed `--mode`, or fewer files for more content.
Use `SKIM_PASSTHROUGH=1` or `--passthrough` for raw output.

Typed modes, `--max-lines`, `--last-lines`, `--tokens` and `--symbol` do not use the default character limit.
`--max-lines` and `--last-lines` apply per file. Hook file rewrites select a mode or a line limit.

**Examples:**
```bash
skim file.ts                       # Default bound
skim - --language typescript       # Same bound on stdin
skim file.ts --max-lines 50         # Explicit line limit
skim file.ts --symbol parse         # One declaration (files only)
skim file.ts --mode structure       # Explicit mode; no character bound
SKIM_PASSTHROUGH=1 skim file.ts      # Raw file bytes
```

**Values:**
- `structure` - Keep structure only (60-80% reduction)
- `signatures` - Function signatures only (85-92% reduction)
- `types` - Type definitions only (90-95% reduction)
- `full` - No transformation (0% reduction)
- `minimal` - Strip non-doc comments, keep all code (reduction unverified)
- `pseudo` - Strip syntactic noise, keep logic flow (reduction unverified)

Structure mode's only measured figure is 60.3%, on the production TypeScript
codebase in the repository README's reduction tables; the range is stated wide
enough to contain it. `minimal` and `pseudo` state no figure because none has
been measured — ADR-008 traces the `15-30%` and `30-50%` targets that once
circulated for them to an unsourced copy in 04b5f9f that was never re-derived.
No CI gate defends any of these numbers — the only reduction ratio the test
suite asserts is `> 0.30`, on the JSON and YAML structure-mode fixtures.

**Example:**
```bash
skim file.ts --mode signatures
```

See [Transformation Modes](./modes.md) for detailed information.

### Language Override

```
-l, --language <LANGUAGE>
```

Override language detection (recommended for stdin; skim also checks a shebang)

**Values:** `typescript`, `javascript`, `python`, `rust`, `go`, `java`, `c`, `cpp`, `csharp`, `ruby`, `sql`, `kotlin`, `swift`, `markdown`, `json`, `yaml`, `toml`

**Auto-detection:** Language is automatically detected from file extensions by default

**Use when:**
- Stdin without a shebang
- Processing files with unusual extensions (fallback)

**Examples:**
```bash
# Select the language for stdin
cat file.ts | skim - --language=typescript

# JSON from stdin
echo '{"api": {"key": "secret"}}' | skim - --language=json

# Fallback for unusual extension
skim weird.inc --language=typescript
```

### Parallel Processing

```
-j, --jobs <JOBS>
```

Number of parallel jobs for multi-file processing [default: number of CPUs]

**Example:**
```bash
skim 'src/**/*.ts' --jobs 8
```

### Output Control

```
--no-header
```

Don't print file path headers for multi-file output

**Example:**
```bash
skim src/ --no-header
```

### Symbol Read

```
--symbol <NAME>
```

Print the source lines of one named declaration. Use it when you need one function, method, type or constant and not the whole file.

**Examples:**
```bash
skim src/lib.rs --symbol parse          # the declaration named parse
skim src/lib.rs --symbol Point::norm    # only norm inside the impl of Point
skim src/app.ts --symbol Router.run     # only run inside the class Router
skim src/app.ts --symbol run -n         # lines are numbered from the first line of the declaration
```

**What it prints:**
- stdout: the raw source lines of the declaration, from its first line to its last line. Skim does not change them.
- stderr: one notice line, always. It states which lines you see and how many lines are not shown:
  `[skim] --symbol "norm": lines 12-15 of 35 shown (11 above, 20 below not shown) — omit --symbol for the whole file`
- If the search for declarations stopped at its limit (100,000 syntax nodes or a nesting depth of 500), the notice also states that more declarations with the name can exist.
- If the name matches two or more declarations, skim prints all of them in file order. A header line starts each block: `==> lib.rs:12-15 norm (1 of 2) <==`. An empty line separates the blocks. Skim prints each source line once. When a declaration lies inside an earlier one, or shares a line with it, the later header says `lines shown above` (or `lines 3-5 shown above` for a part), and the block holds only the lines not yet printed. With `-n`, a header line has no number.

**How a name matches:**
- The name must equal the declaration name. The match is exact and case-sensitive.
- Write `Type::name` or `Type.name` to select a declaration inside one container, such as an impl block or a class. Only one level of container is possible.
- The kinds of declaration depend on the language. They include functions, methods, classes, structs, enums, interfaces, traits, type aliases, modules, constants and fields.

**Combine with other options:**
- `--max-lines N` and `--last-lines N` apply to the selected lines. The elision marker counts toward the limit.
- `-n, --line-numbers` prints the source line number of each line. The first number is the first line of the declaration.
- `--language` names the language of a file that has an unusual name.
- `--mode` must be `full`, or must not be set. Any other mode exits with code 2.
- Skim caches each symbol read apart from the whole-file read and apart from the reads of other names.
- A symbol read adds no row to the token statistics.

**Exit codes:**

| Code | Cause |
|------|-------|
| 0 | The name matched at least one declaration. |
| 1 | No declaration has that name. The message lists up to 10 declaration names of the file. If the search stopped at its limit of 100,000 syntax nodes or at its nesting depth of 500, the message names both limits and lists no names. The name can be in the part of the file that skim did not search. |
| 2 | The command is not a symbol read: stdin, two or more files, a glob, a directory, an empty name, `--tokens`, or a `--mode` other than `full`. |
| 3 | The file is Markdown, JSON, YAML or TOML, or skim cannot detect its language. For the last case, add `--language`. |

**Known limits:**
- `--symbol` prints raw source only. It has no structure, signatures or types view.
- Markdown and the data formats (JSON, YAML, TOML) exit with code 3.
- Functions in C and C++ files are not found. Their name is inside the declarator.
- The printed range holds the declaration only. Attributes, doc comments and decorators above the declaration are not in it.

### Caching Control

```
--no-cache
```

Disable caching (caching is enabled by default)

**Example:**
```bash
skim file.ts --no-cache
```

```
--clear-cache
```

Clear the parser cache and the old search index data, and exit. It keeps every other file in the cache folder, such as `analytics.db`.

**Example:**
```bash
skim --clear-cache
```

See [Caching](./caching.md) for detailed information.

### Token Statistics

```
--show-stats
```

Show token reduction statistics (output to stderr)

**Example:**
```bash
skim file.ts --show-stats
# Output: [skim] 1,000 tokens → 200 tokens (80.0% reduction)
```

### Help and Version

```
-h, --help      Print help
-V, --version   Print version
```

## Subcommands

### skim heatmap

Analyzes git commit history to surface risk hotspots: high-churn files, tightly coupled file pairs, fix-after-touch patterns, bus-factor concentration, and module boundary violations. The output is short and ranked, so an agent can read it before it starts work.

```bash
skim heatmap [OPTIONS]              # short ranked view
skim heatmap --detail <SECTION>     # 25 rows of one section
skim heatmap --insights             # ranked CRITICAL/WARNING findings
skim heatmap --map [OPTIONS]        # token-bounded repository map
skim heatmap [OPTIONS] <FILE>...    # one "before you edit" card per file
```

#### Default View

The default text view is 30 lines or fewer. The first line names the repository (no absolute path), the `since..until` dates, the commit count and the window strategy, for example `Heatmap: skim-search 2026-02-24..2026-11-12, 191 commits, dual window (count)`. Up to two warning lines follow (for example, the count of commits with more than 50 files that coupling did not pair). Then three sections:

| Section | Rows | Row content |
|---------|------|-------------|
| Hotspots | 10 | `commits x lines = score`, then the path. |
| Coupling | 5 | Confidence, shared commits, then the two paths. Code-to-code pairs only: a pair with a doc file (`.md`, `.rst`, `.txt`, …) is not shown. |
| Fix risk | 5 | Fix commits/commits, lift and base rate, then the path. Only files whose fix rate is above the repository base rate. |

A capped section ends with one line that gives the exact hidden count and the section to open. For example: `570 more (--detail coupling)`. When the non-code rule leaves out paths, the same line counts them. For example: `864 more (--detail hotspots); 847 non-code files left out (--include-docs)`.

Module health and bus factor are not in the default view. Use `--detail` to see them. The last line points to the per-file card: `Before you edit a file: skim heatmap <file>`.

#### Hotspot Rank

The score is commits in the window × text lines at HEAD. Skim sorts by score, then by commit count, both from highest to lowest. If both values match, skim sorts by path, lowest first. Skim uses Git to count text lines at HEAD, so uncommitted edits do not change the line count. Empty files have a score of zero. Binary files at HEAD stay out of the ranked lists, even if their history has text changes. In a partial clone, HEAD reads do not fetch objects. If Git cannot turn off these fetches or if HEAD data is not available, skim warns and ranks by commits.

Skim joins history across renames that Git reports. When HEAD data is available, hotspots, coupling, fix risk and bus factor omit paths that are not regular files at HEAD. The hotspot summary counts those paths. A file card can still show the history of such a path.

#### Repository Map (`--map`)

Use `skim heatmap --map` to get a text outline of the repository in [hotspot order](#hotspot-rank). The header gives the repository, window dates, commit count and window type. Each file block gives `path: C commits, L lines, score S`. If the file has a co-change partner, one `changes with: path` line gives the first partner.

Symbol rows have the form `kind name:line`, with the tree-sitter kind and a 1-based source line. A member name can have the form `Container::name`. Skim lists the first 12 eligible names per file in source order. A `+N more` line counts extra names from the scan. The rows have no signature text, bodies, initializers or default values.

| Language | Symbol rows |
|----------|-------------|
| Rust | Functions, methods, trait function declarations, structs, enums, unions, traits, modules and macros. No constants, statics, type aliases, fields or enum variants. |
| Go | Functions, methods and named types (`type_spec`). No constants or variables. |
| TypeScript (with TSX) and JavaScript (with JSX) | Functions, classes and methods, plus TypeScript interfaces and enums. A top-level `const`, `let` or `var` with one identifier gives a `variable_declarator` row. No function-local variables, destructured names, computed method keys or type aliases. |
| Python | Functions, methods and classes. |
| Other tree-sitter languages | Named functions, type definitions and containers when the grammar gives the node a `name` field. For example, C and C++ function nodes use `declarator` instead, so these nodes give no rows. |
| JSON, YAML, TOML and unsupported formats | No symbol rows. The map can still show file stats. |

The map uses the original regular-file blobs at HEAD for file sizes, line counts and symbols. It does not read the work tree, symbolic links or submodule entries. Uncommitted changes do not affect the map.

In a partial clone, the map does not fetch missing blobs for HEAD reads. The history walk can still fetch objects. The parse budget covers at most 500 file attempts, 32 MiB in total and 1 MiB per file. These limits do not use a timer.

Use `--tokens N` to set the cl100k token budget. The default is 1,000. The allowed range is 1 to 10,000,000. The budget covers all of stdout, with the header, warnings, whole file blocks and trailer. If skim cannot fit the header or verify that the output fits, it prints only the trailer. This trailer alone can exceed a very small budget.

The trailer appears if the budget omits a ranked file, a shown file has no symbols, or a declaration scan stops early. In `[skim] S of T files`, `T` counts the ranked files after filters. The line also counts shown files by reason and points to `--tokens`. The reasons are `language`, `no declarations`, `parse budget`, `parse error`, `read error`, `size cap` and `truncated scan`. For example:

```text
[skim] 1 of 1 files; language: 1 — adjust the map budget with --tokens
```

Use `--path`, [window flags](#time-windows) and [exclude flags](#default-excludes) to set the scope. The map uses the same [non-code path rules](#non-code-paths) as the hotspot list. If the selected history has no commits, skim exits 1. If one commit is in scope and HEAD data is available, the map ranks files by line count. If the clone is shallow, the shallow history warning is part of the budget.

The map has no JSON form in v1. Do not combine `--map` with `--json`, `--format json`, `--detail`, `--insights`, `--diff`, `--top` or file arguments. `--tokens` requires `--map`. A conflict or an invalid token budget exits 1.

#### Sections (`--detail`)

`--detail <SECTION>` shows only that section, with 25 rows (or `--top N` rows), then one `N more (raise --top)` line. An unknown name exits 1 and lists the valid names. `--detail` can be given once, and it does not combine with `--insights` or with file arguments.

| Name | Description |
|------|-------------|
| `hotspots` | Files with the highest scores (see [Hotspot Rank](#hotspot-rank)). |
| `coupling` | Code-to-code file pairs that change together, with confidence and shared commits. |
| `fix-risk` | Files whose fix rate is above the repository base rate, with fix commits/commits, lift and base rate. |
| `modules` | Encapsulation of each module: the share of its commits that touch no other module. A module is the folder of a tracked `Cargo.toml` or `package.json`, else the top folder. |
| `bus-factor` | Files where one author has more than 80% of the commits. |

#### Noise Rules

| Rule | Value |
|------|-------|
| Fix commit | The subject says that the commit fixes something: a `[fix]` tag, a `fix:` / `fix(scope):` / `fix!:` type (also `bug`, `hotfix`, `patch`, `revert`; a path-style type such as `net/http:` counts as a type), or one of these words first in the subject, for example `Revert "..."` or `docs: fix link`. A word inside a description does not count (`docs: explain later-fix proximity` is not a fix). |
| Fix-risk and bus-factor support | 5 commits of the file |
| Coupling support | 3 shared commits |
| Module support | 5 commits of the module |
| Fix-risk lift | The 95% lower bound (Wilson) of the file's fix rate is above the base rate (fix commits / all commits after excludes). At a base rate of 0% or 100%, one reason line replaces the rows. |
| Bus-factor gate | 2 or more distinct authors in the window |
| Large commits | A commit with more than 50 files after excludes adds no coupling pairs. A warning line counts these commits and gives the touch count for each named file on the card view. If no shown file has a touch, the card view omits the line. |

`files[]` in the JSON keeps the raw per-file values (for example `churn.commits` and `fix_risk.keyword_pct`) for files below these minimums.

#### Non-Code Paths

Doc files (`*.md`, `*.mdx`, `*.markdown`, `*.rst`, `*.txt`, `*.adoc`) stay out of hotspots, fix risk and insights. This rule also applies to files under the top folders `docs`, `doc`, `.github`, `.devflow`, `tests` and `test`. These folders also stay out of modules. The folder rule does not apply to a nested folder such as `crates/x/tests/`.

The summary line counts the paths that the rule leaves out. In JSON, `left_out.hotspots` counts non-code files in the hotspot list before the cap. `left_out.fix_risk` counts only non-code files that pass the fix-risk support and lift rules. `left_out.modules` counts non-code folders. With `--insights --json`, `counts.left_out` counts non-code findings.

Use `--include-docs` to keep non-code paths. `--diff`, file arguments and a `--path` that is a non-code folder bypass this filter. Coupling never pairs a doc file.

#### Caps and `--top`

Use `--top N` to set the row caps in the default, detail, insights, card and JSON views. Each list has its own default cap:

| View | Default cap | "More" line |
|------|-------------|-------------|
| Default view | hotspots 10, coupling 5, fix risk 5 | `N more (--detail <section>)` |
| `--detail <SECTION>` | 25 rows | `N more (raise --top)` |
| `--insights` | 20 findings | `N more (raise --top)` |
| File cards | 5 cards; per card 3 owners, 4 partners, 3 fix commits | `N more files (raise --top)` (`1 more file` for one); one hidden-count line per card |

Fix commits on a card stay at 5 or fewer at any `--top`.

#### Time Windows

| Flag | Description |
|------|-------------|
| (default) | The wider of the last 90 days and the last 200 commits |
| `--window sprint` | Last 14 days |
| `--window month` | Last 30 days |
| `--window quarter` | Last 90 days |
| `--window half` | Last 180 days |
| `--window year` | Last 365 days |
| `--window all` | No date cutoff, the same as `--since 0` |

An unknown `--window` preset exits 1 and lists the valid names. With `--window all` or `--since 0`, the header still shows `1970-01-01` as the start. The usual history filters still apply.

Use `--since YYYY-MM-DD` to set the cutoff to 00:00 UTC on that date. Use a valid calendar date from `1970-01-01` through `9999-12-31`. `--since` also accepts epoch seconds and durations such as `30d`, `2w` and `24h`.

#### File Cards

`skim heatmap <FILE>...` prints one "before you edit" card per file argument, at most 5 (or `--top N`), then `N more files (raise --top)` (`1 more file` for one). A duplicate path gives one card. `--insights <FILE>...` prints the same bytes. A card is 12 lines or fewer at the default caps:

```text
crates/rskim/src/cmd/heatmap/view.rs: 7 commits
  partners (confidence, shared commits):
     69%   5  crates/rskim/tests/cli_heatmap.rs
    100%   7  crates/rskim/src/cmd/heatmap/mod.rs
     66%   5  crates/rskim/src/cmd/heatmap/output.rs
  fix commits from history (newest first):
    81a22ee 2026-11-12 "fix(heatmap): cap file_targets and count missing --diff files"
    ca69af4 2026-11-12 "fix(heatmap): address self-review issues"
```

- **Owners** (`owners: "A" 89%, "B" 11%`) show only when the file has 2 or more distinct authors. Each name is in double quotes.
- **Partners** are files with 3 or more shared commits and a confidence at or above `--coupling-threshold`. Test files come first.
- **Fix commits** use the fix-keyword rule: a 7-character SHA, the date and the quoted subject, newest first. In the text view, a `"` or `\` in a subject or an owner name shows as `\"` or `\\`.
- If a filter leaves out a file, its card says `left out by the default excludes; --no-exclude keeps it` or `left out by --exclude; remove the matching patterns to keep it`. Otherwise, a file with no commits in the window prints `<path>: not in history window (N commits since DATE)` (`1 commit` for one). N and DATE describe the window.
- The large-commit warning gives the run count and the touch count for each named file. For example: `warning: coupling: 1 commit with more than 50 files not paired (1 touches src/main.rs)`. The line names up to three shown files in argument order, then gives the count of more names. If no shown file has a touch, skim omits the line.
- In subjects, author names and card partner paths, control characters, bidirectional characters and invisible format characters become U+FFFD. Skim limits each of these fields to 72 characters.
- `./x`, an absolute path under the repository root, a path relative to the current folder and a path with `..` give the same card. A path outside the repository exits 1. skim does not open the file. An absolute path through a symbolic-link prefix (for example macOS `/var`) is refused.

Metrics come from the full window; a card only narrows the display.

#### File Targeting

- **`--path <DIR>`**: Limit history and HEAD line counts to this path. Use it with file arguments if necessary.
- **`--diff <BASE>`**: The default view, scoped to the files changed vs `BASE` (three-dot diff). It warns for a changed file with no history in the window. Mutually exclusive with file arguments. Needs git 2.24 or newer.

#### Default Excludes

By default the analysis leaves out lock files, build and dist output, minified bundles, logs, `vendor/` and `node_modules/`, IDE metadata, coverage output, and, at any depth, `CHANGELOG.md`, `*.snap`, `snapshots/`, `*.pb.go`, `*_pb2.py` and `generated/` folders. These files are absent from every section and from `files[]`. Use `--exclude <PATTERN>` to add a pattern. `--no-exclude` disables both the default patterns and any `--exclude` patterns. The patterns match paths only: a file marked "generated" only in a header line stays in.

Skim refuses an invalid `--exclude` pattern and gives the reason.

#### Insights Mode

`--insights` emits the CRITICAL/WARNING findings, one per line, ranked by severity, then score, then support, then path. Each finding kind has the support minimum of its section (see Noise Rules), so no CRITICAL finding comes from too few commits. The header line and up to two warning lines come first, as in the default view. The view shows 20 findings, then one `N more (raise --top)` line.

```bash
skim heatmap --insights                  # Top 20 findings
skim heatmap --insights --top 50 --json  # Top 50 findings as JSON
```

#### JSON (version 2)

`--json` gives one compact line with `"version": 2`. This is a breaking change from version 1:

- Each array of rows has the cap of its section in the view (`files[]` holds the hotspot rows), and each `files[].blast_radius` holds at most 5 partners.
- `file_targets[]` holds at most 20 targets, and `counts.file_targets` gives their full and shown count. `warnings[]` has at most one line for each kind of warning: the `--diff` files that are not in the window give one line that counts them and names the first 3. `excluded_patterns[]` is the fixed pattern list: the default excludes and the `--exclude` values.
- `counts.<section>.{total,shown}` gives the full and the shown count of each section (`hotspots`, `coupling`, `fix_risk`, `modules`, `bus_factor`).
- `files[]` keeps its per-file keys. No support minimum applies. `files[].lines` gives the text line count at HEAD, or zero if there is no count. `files[].score` gives commits × lines.
- `left_out.{hotspots,fix_risk,modules}` counts non-code paths. `left_out.not_at_head` counts history paths that are not regular files at HEAD. Skim omits `left_out` if all counts are zero.
- With file arguments, `cards[]` holds the cards (`path`, `in_window`, `commits`, `counts`, `owners`, `partners`, `fix_commits`) and `counts.cards` their counts. An excluded card has `in_window: false` and an `excluded` field of `"default"` or `"user"`. Other cards omit `excluded`.
- `--insights --json` gives `insights[]`, `top_files[]` and `flagged_modules[]` for the shown findings only, with `counts.insights`. Its `warnings[]` is the same list as in `--json`.
- `repository` gives the repository name, the same name as the text header, never the absolute path. This is the same in `--insights --json` and with file arguments.

#### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--since <VALUE>` | — | Set the cutoff: epoch seconds, `YYYY-MM-DD` (00:00 UTC), or a duration (`30d`, `2w`, `24h`) |
| `--last <N>` | — | Analyze last N commits |
| `--window <PRESET>` | — | Select a preset (see [Time Windows](#time-windows)) |
| `--path <DIR>` | — | Limit history and HEAD line counts to this path |
| `--diff <BASE>` | — | Default view for the files changed vs BASE |
| `--detail <SECTION>` | — | Show 25 rows (or `--top N`) of one section: `hotspots`, `coupling`, `fix-risk`, `modules` or `bus-factor` |
| `--json`, `--format json` | false | Emit JSON version 2 instead of text; not valid with `--map` |
| `--map` | false | Show a text repository map in hotspot order |
| `--tokens <N>` | 1,000 | Set the map budget in cl100k tokens (1 to 10,000,000); requires `--map` |
| `--top <N>` | per list (see Caps and `--top`) | Set row caps; not valid with `--map` |
| `--no-exclude` | false | Disable the default and explicit exclude patterns (see [Default Excludes](#default-excludes)) |
| `--include-docs` | false | Keep non-code paths in the map, hotspots, fix risk, modules and insights (see [Non-Code Paths](#non-code-paths)) |
| `--exclude <PATTERN>` | — | Add a glob pattern to exclude (repeatable); `--no-exclude` disables it |
| `--coupling-threshold <FLOAT>` | 0.5 | Coupling confidence threshold (0.0–1.0) |
| `--fix-window <N>` | 5 | Proximity window for fix-after-touch detection (commit count) |
| `--insights` | false | Show ranked findings only |
| `--debug` | false | Enable debug output to stderr |
| `-h`, `--help` | — | Show help message |

#### Examples

```bash
# Default: short ranked view of the dual window
skim heatmap

# All coupling pairs, or the top 20 fix-risk files
skim heatmap --detail coupling
skim heatmap --detail fix-risk --top 20

# 3 rows per section
skim heatmap --top 3

# Analyze last 200 commits
skim heatmap --last 200

# Analyze last sprint
skim heatmap --window sprint

# Analyze the last 30 days
skim heatmap --since 30d

# Analyze commits from 00:00 UTC on November 1, 2026
skim heatmap --since 2026-11-01

# Get a repository map with the default budget
skim heatmap --map

# Set a 300-token budget for files under crates/rskim/src/
skim heatmap --map --tokens 300 --path crates/rskim/src/

# JSON output for programmatic consumption
skim heatmap --json

# Scope analysis to src/ directory
skim heatmap --path src/

# Default view for the files changed vs the main branch
skim heatmap --diff main

# "Before you edit" card for one file
skim heatmap src/main.rs

# Combine path-scope with a card
skim heatmap --path src/ src/main.rs

# Top 20 ranked findings
skim heatmap --insights

# Top 50 findings as JSON
skim heatmap --insights --top 50 --json

# Keep lock files, changelogs, snapshots and generated code
skim heatmap --no-exclude
```

## Common Usage Patterns

### Single File Processing

```bash
# Default (structure mode)
skim src/app.ts

# With specific mode
skim src/app.ts --mode signatures

# Show stats
skim src/app.ts --show-stats
```

### Directory Processing

```bash
# Process entire directory
skim src/

# Current directory
skim .

# With parallel processing
skim src/ --jobs 8

# Without file headers
skim src/ --no-header
```

### Glob Patterns

```bash
# All TypeScript files
skim 'src/**/*.ts'

# Multiple extensions
skim '*.{js,ts}'

# Specific subdirectory
skim 'src/components/*.tsx'
```

### Stdin Processing

```bash
# From pipe (language required)
cat file.ts | skim - --language=typescript

# From redirect
skim - --language=python < script.py

# With other commands
curl https://example.com/code.ts | skim - -l typescript
```

### Piping Output

```bash
# To syntax highlighter
skim src/app.ts | bat -l typescript

# To LLM CLI
skim src/ --no-header | llm "Explain this codebase"

# To file
skim src/ --mode signatures > api-docs.txt
```

## Exit Codes

- `0` - Success
- `1` - General error (invalid arguments, file not found, etc.)
- `2` - Parse error (invalid syntax in source file)
- `3` - Unsupported language

## Tips and Best Practices

### For LLM Context Optimization

1. Use `--no-header` to reduce noise in context
2. Choose the most aggressive mode that still gives you needed information
3. Use `--show-stats` to verify token reduction

```bash
skim src/ --mode signatures --no-header --show-stats | llm "Document this API"
```

### For Large Codebases

1. Use directory processing instead of globs for simplicity
2. Enable parallel processing with `--jobs`
3. Let caching work for you (enabled by default)

```bash
skim . --jobs 8  # Fast processing with caching
```

### For CI/CD

1. Disable cache in CI environments (ephemeral)
2. Use `--show-stats` to track token reduction over time

```bash
skim src/ --no-cache --show-stats > build/api-docs.txt
```

### For Mixed-Language Projects

1. Use directory processing - auto-detection handles everything
2. No need to specify language per file

```bash
skim src/  # Automatically processes .ts, .py, .rs, etc.
```
