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
- `structure` - Keep structure only (70-80% reduction)
- `signatures` - Function signatures only (85-92% reduction)
- `types` - Type definitions only (90-95% reduction)
- `full` - No transformation (0% reduction)

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
