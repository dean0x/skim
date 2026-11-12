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
- **Stdin**: `skim -` (requires `--language` flag)

## Options

### Transformation Mode

```
-m, --mode <MODE>
```

Transformation mode [default: structure]

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

Override language detection (required for stdin, optional fallback otherwise)

**Values:** `typescript`, `javascript`, `python`, `rust`, `go`, `java`, `c`, `cpp`, `csharp`, `ruby`, `sql`, `kotlin`, `swift`, `markdown`, `json`, `yaml`, `toml`

**Auto-detection:** Language is automatically detected from file extensions by default

**Use when:**
- Reading from stdin (required)
- Processing files with unusual extensions (fallback)

**Examples:**
```bash
# Required for stdin
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
- If the name matches two or more declarations, skim prints all of them in file order. A header line starts each block: `==> lib.rs:12-15 norm (1 of 2) <==`. An empty line separates the blocks.

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
| 1 | No declaration has that name. The message lists up to 10 declaration names of the file. If the search stopped at its limit of 100,000 syntax nodes, the message names that limit and lists no names. The name can be in the part of the file that skim did not search. |
| 2 | The command is not a symbol read: stdin, two or more files, a glob, a directory, an empty name, `--tokens`, or a `--mode` other than `full`. |
| 3 | The file is Markdown, JSON, YAML or TOML, or skim cannot detect its language. For the last case, add `--language`. |

**Known limits:**
- `--symbol` prints raw source only. It has no structure, signatures or types view.
- Markdown and the data formats (JSON, YAML, TOML) exit with code 3.
- Functions in C and C++ files are not found. Their name is inside the declarator.
- The printed range holds the declaration only. Attributes, doc comments and decorators above the declaration are not in it.
- For `.tsx` files, `--symbol` uses the TSX grammar. The structure mode still parses `.tsx` files with the TypeScript grammar.

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

Clear all cached files and exit

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
skim heatmap --detail <SECTION>     # all rows of one section
skim heatmap --insights             # ranked CRITICAL/WARNING findings
skim heatmap [OPTIONS] <FILE>...    # one "before you edit" card per file
```

#### Default View

The default text view is 30 lines or fewer. The first line names the repository (no absolute path), the `since..until` dates, the commit count and the window strategy, for example `Heatmap: skim-search 2026-02-24..2026-11-12, 191 commits, dual window (count)`. Up to two warning lines follow (for example, the count of commits with more than 50 files that coupling did not pair). Then three sections:

| Section | Rows | Row content |
|---------|------|-------------|
| Hotspots | 10 | Commits in the window, then the path. Ranked by commits, then path. |
| Coupling | 5 | Confidence, shared commits, then the two paths. Code-to-code pairs only: a pair with a doc file (`.md`, `.rst`, `.txt`, …) is not shown. |
| Fix risk | 5 | Fix commits/commits, lift and base rate, then the path. Only files whose fix rate is above the repository base rate. |

A capped section ends with one line that gives the exact hidden count and the section to open, for example `570 more (--detail coupling)`. Module health and bus factor are not in the default view; `--detail` shows them.

#### Sections (`--detail`)

`--detail <SECTION>` shows only that section, with all its rows (or `--top N` rows). An unknown name exits 1 and lists the valid names. `--detail` can be given once, and it does not combine with `--insights` or with file arguments.

| Name | Description |
|------|-------------|
| `hotspots` | Files changed most often (commits in the window). |
| `coupling` | Code-to-code file pairs that change together, with confidence and shared commits. |
| `fix-risk` | Files whose fix rate is above the repository base rate, with fix commits/commits, lift and base rate. |
| `modules` | Encapsulation of each module: the share of its commits that touch no other module. A module is the folder of a tracked `Cargo.toml` or `package.json`, else the top folder. |
| `bus-factor` | Files where one author has more than 80% of the commits. |

#### Noise Rules

| Rule | Value |
|------|-------|
| Fix-risk and bus-factor support | 5 commits of the file |
| Coupling support | 3 shared commits |
| Module support | 5 commits of the module |
| Fix-risk lift | The 95% lower bound (Wilson) of the file's fix rate is above the base rate (fix commits / all commits after excludes). At a base rate of 0% or 100%, one reason line replaces the rows. |
| Bus-factor gate | 2 or more distinct authors in the window |
| Large commits | A commit with more than 50 files adds no coupling pairs; a warning line counts these commits. |

`files[]` in the JSON keeps the raw per-file values (for example `churn.commits` and `fix_risk.keyword_pct`) for files below these minimums.

#### Caps and `--top`

`--top N` sets the rows of each list in every view. Without it, each list has its own cap:

| View | Default cap | "More" line |
|------|-------------|-------------|
| Default view | hotspots 10, coupling 5, fix risk 5 | `N more (--detail <section>)` |
| `--detail <SECTION>` | all rows | — |
| `--insights` | 20 findings | `N more (raise --top)` |
| File cards | 5 cards; per card 3 owners, 4 partners, 3 fix commits | `N more files (raise --top)` (`1 more file` for one); one hidden-count line per card |

Fix commits on a card stay at 5 or fewer at any `--top`.

#### Time Windows

| Flag | Description |
|------|-------------|
| (default) | Dual mode: max(last 90 days, last 200 commits). Captures whichever window is wider. |
| `--window sprint` | Last 14 days |
| `--window month` | Last 30 days |
| `--window quarter` | Last 90 days |
| `--window half` | Last 180 days |
| `--window year` | Last 365 days |
| `--window all` | Entire repository history |

The default **dual mode** is designed for repositories of any age: it avoids under-counting on small repos (fewer than 200 commits) while still bounding analysis on large repos.

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
- A file with no commits in the window prints `<path>: not in history window (N commits since DATE)` (`1 commit` for one), where N and DATE describe the window.
- Control characters, bidirectional characters and invisible format characters (for example zero-width characters and Unicode tag characters) in subjects, author names and partner paths become U+FFFD. Each of these strings is cut at 72 characters.
- `./x`, an absolute path under the repository root, a path relative to the current folder and a path with `..` give the same card. A path outside the repository exits 1. skim does not open the file. An absolute path through a symbolic-link prefix (for example macOS `/var`) is refused.

Metrics come from the full window; a card only narrows the display.

#### File Targeting

- **`--path <DIR>`**: Scope the git log itself (commit-level filter). Composable with file arguments.
- **`--diff <BASE>`**: The default view, scoped to the files changed vs `BASE` (three-dot diff). It warns for a changed file with no history in the window. Mutually exclusive with file arguments. Needs git 2.24 or newer.

#### Default Excludes

By default the analysis leaves out lock files, build and dist output, minified bundles, logs, `vendor/` and `node_modules/`, IDE metadata, coverage output, and, at any depth, `CHANGELOG.md`, `*.snap`, `snapshots/`, `*.pb.go`, `*_pb2.py` and `generated/` folders. These files are absent from every section and from `files[]`. `--no-exclude` keeps them; `--exclude <PATTERN>` adds a pattern. The patterns match paths only: a file marked "generated" only in a header line stays in.

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
- `files[]` keeps its per-file keys; no support minimum applies to it.
- With file arguments, `cards[]` holds the cards (`path`, `in_window`, `commits`, `counts`, `owners`, `partners`, `fix_commits`) and `counts.cards` their counts.
- `--insights --json` gives `insights[]`, `top_files[]` and `flagged_modules[]` for the shown findings only, with `counts.insights`. Its `warnings[]` is the same list as in `--json`.
- `repository` gives the repository name, the same name as the text header, never the absolute path. This is the same in `--insights --json` and with file arguments.

#### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--since <VALUE>` | — | Analyze commits since epoch (seconds) or duration (`30d`, `2w`, `24h`) |
| `--last <N>` | — | Analyze last N commits |
| `--window <PRESET>` | — | Named window preset (see table above) |
| `--path <DIR>` | — | Scope git log to files under this directory |
| `--diff <BASE>` | — | Default view for the files changed vs BASE |
| `--detail <SECTION>` | — | Show all rows of one section: `hotspots`, `coupling`, `fix-risk`, `modules` or `bus-factor` |
| `--json`, `--format json` | false | Emit JSON version 2 instead of human-readable text |
| `--top <N>` | per list (see Caps and `--top`) | Rows of each list in every view |
| `--no-exclude` | false | Keep the files of the default excludes (see Default Excludes) |
| `--exclude <PATTERN>` | — | Add extra glob pattern to exclude (repeatable) |
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

# Analyze last 30 days using duration syntax
skim heatmap --since 30d

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
