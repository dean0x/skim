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

**Values:** `typescript`, `javascript`, `python`, `rust`, `go`, `java`, `markdown`

**Auto-detection:** Language is automatically detected from file extensions by default

**Use when:**
- Reading from stdin (required)
- Processing files with unusual extensions (fallback)

**Examples:**
```bash
# Required for stdin
cat file.ts | skim - --language=typescript

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
