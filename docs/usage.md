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

Analyzes git commit history to surface risk hotspots: high-churn files, tightly coupled file pairs, fix-after-touch patterns, bus-factor concentration, and module boundary violations.

```bash
skim heatmap [OPTIONS] [FILE...]
```

#### Metrics

| Metric | Description |
|--------|-------------|
| Stability Score | Composite risk score (0–100, lower = riskier). Combines churn, recency, and fix density. |
| Churn | Number of commits touching the file within the analysis window. |
| Blast Radius | Files that tend to change together (coupling). High coupling = high blast radius. |
| Fix Risk | Percentage of commits that are fixes, or followed by a fix within the proximity window. |
| Bus Factor | Single-author concentration risk. Flags files where one author holds >80% of commits. |
| Module Health | Encapsulation score for top-level directories. Measures cross-boundary coupling. |

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

#### File Targeting

Positional file arguments and `--diff` scope the **output**, not the git history. Metrics are computed on the full commit history for accuracy — coupling and fix-risk scores reflect the complete picture, then the display is narrowed.

- **Positional args**: `skim heatmap src/main.rs` — show results only for that file
- **`--path <DIR>`**: Scope the git log itself (commit-level filter). Composable with file args.
- **`--diff <BASE>`**: Show only files changed vs `BASE` (three-dot diff). Mutually exclusive with positional file args.

#### Insights Mode

`--insights` emits only threshold-filtered CRITICAL/WARNING findings, one per line. Use when you want a quick list of hotspots without the full table.

```bash
skim heatmap --insights           # Text findings
skim heatmap --insights --json    # JSON array (agent-friendly)
```

#### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--since <VALUE>` | — | Analyze commits since epoch (seconds) or duration (`30d`, `2w`, `24h`) |
| `--last <N>` | — | Analyze last N commits |
| `--window <PRESET>` | — | Named window preset (see table above) |
| `--path <DIR>` | — | Scope git log to files under this directory |
| `--diff <BASE>` | — | Show only files changed vs BASE |
| `--json`, `--format json` | false | Emit JSON instead of human-readable text |
| `--top <N>` | 20 | Maximum number of files to display |
| `--no-exclude` | false | Disable default exclusion patterns (lock files, build dirs) |
| `--exclude <PATTERN>` | — | Add extra glob pattern to exclude (repeatable) |
| `--coupling-threshold <FLOAT>` | 0.5 | Coupling confidence threshold (0.0–1.0) |
| `--fix-window <N>` | 5 | Proximity window for fix-after-touch detection (commit count) |
| `--insights` | false | Show only threshold-filtered findings |
| `--debug` | false | Enable debug output to stderr |
| `-h`, `--help` | — | Show help message |

#### Examples

```bash
# Default: analyze last 90 days (dual mode)
skim heatmap

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

# Show only files changed vs main branch
skim heatmap --diff main

# Show results for a specific file (full metric history, display narrowed)
skim heatmap src/main.rs

# Combine path-scope with file-scope
skim heatmap --path src/ src/main.rs

# Threshold-filtered insights only
skim heatmap --insights

# Insights as JSON for agent pipelines
skim heatmap --insights --json
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
