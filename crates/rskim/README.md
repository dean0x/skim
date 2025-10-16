# rskim

Smart code reader - streaming code transformation for AI agents.

[![Crates.io](https://img.shields.io/crates/v/rskim.svg)](https://crates.io/crates/rskim)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## Overview

`rskim` transforms source code by intelligently removing implementation details while preserving structure, signatures, and types - perfect for optimizing code for LLM context windows.

Think of it like `cat`, but smart about what code to show.

## Installation

### Via npx (Recommended - no install required)

```bash
npx rskim file.ts
```

### Via npm

```bash
npm install -g rskim
```

### Via Cargo

```bash
cargo install rskim
```

## Quick Start

```bash
# Read TypeScript with structure mode
npx rskim file.ts

# Extract Python function signatures
npx rskim file.py --mode signatures

# Pipe to syntax highlighter
npx rskim file.rs | bat -l rust

# Read from stdin
cat code.ts | npx rskim - --language=type-script
```

## Features

- **6 Languages**: TypeScript, JavaScript, Python, Rust, Go, Java
- **4 Transformation Modes**: Structure, Signatures, Types, Full
- **Fast**: <50ms for 1000-line files
- **Streaming**: Outputs to stdout for pipe workflows
- **Safe**: Built-in DoS protections

## Usage

### Basic Usage

```bash
npx rskim <FILE>

# Or if installed globally
rskim <FILE>
```

### Options

```bash
Options:
  -m, --mode <MODE>         Transformation mode [default: structure]
                            [possible values: structure, signatures, types, full]
  -l, --language <LANGUAGE> Override language detection
                            [possible values: type-script, java-script, python, rust, go, java]
  -h, --help                Print help
  -V, --version             Print version
```

## Transformation Modes

### Structure Mode (Default)

Removes function bodies while preserving signatures (70-80% reduction).

```bash
npx rskim file.ts
```

**Input:**
```typescript
function add(a: number, b: number): number {
    const result = a + b;
    console.log(`Adding ${a} + ${b} = ${result}`);
    return result;
}
```

**Output:**
```typescript
function add(a: number, b: number): number { /* ... */ }
```

### Signatures Mode

Extracts only function and method signatures (85-92% reduction).

```bash
npx rskim file.py --mode signatures
```

**Input:**
```python
def calculate_total(items: list[Item], tax_rate: float) -> Decimal:
    subtotal = sum(item.price for item in items)
    tax = subtotal * tax_rate
    return subtotal + tax
```

**Output:**
```python
def calculate_total(items: list[Item], tax_rate: float) -> Decimal:
```

### Types Mode

Extracts only type definitions (90-95% reduction).

```bash
npx rskim file.ts --mode types
```

**Input:**
```typescript
interface User {
    id: number;
    name: string;
}

function getUser(id: number): User {
    return db.users.find(id);
}
```

**Output:**
```typescript
interface User {
    id: number;
    name: string;
}
```

### Full Mode

Returns original code unchanged (0% reduction).

```bash
npx rskim file.rs --mode full
```

## Examples

### Explore a codebase

```bash
# Get overview of all TypeScript files
find src -name '*.ts' -exec npx rskim {} \;

# Extract all Python function signatures
npx rskim app.py --mode signatures > api.txt

# Review Rust types
npx rskim lib.rs --mode types | less
```

### Prepare code for LLMs

```bash
# Reduce token count before sending to GPT
npx rskim large_file.ts | wc -w
# Output: 150 (was 600)

# Get just the API surface
npx rskim server.py --mode signatures | pbcopy
```

### Pipe workflows

```bash
# Skim and highlight
npx rskim file.rs | bat -l rust

# Skim and search
npx rskim file.ts | grep "interface"

# Skim multiple files
cat *.py | npx rskim - --language=python
```

## Supported Languages

| Language   | Extensions         | Auto-detected |
|------------|--------------------|---------------|
| TypeScript | `.ts`, `.tsx`      | ✅            |
| JavaScript | `.js`, `.jsx`, `.mjs` | ✅         |
| Python     | `.py`              | ✅            |
| Rust       | `.rs`              | ✅            |
| Go         | `.go`              | ✅            |
| Java       | `.java`            | ✅            |

## Performance

- **Parse + Transform**: <50ms for 1000-line files
- **Token Reduction**: 60-95% depending on mode
- **Streaming**: Zero intermediate files

## Security

Built-in protections against:
- Stack overflow attacks (max depth: 500)
- Memory exhaustion (max input: 50MB)
- UTF-8 boundary violations
- Path traversal attacks

## Library

For programmatic usage, see the [`rskim-core`](https://crates.io/crates/rskim-core) library crate.

## Links

- [Repository](https://github.com/dean0x/skim)
- [Library Documentation](https://docs.rs/rskim-core)
- [npm Package](https://www.npmjs.com/package/rskim)

## License

MIT
