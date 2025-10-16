# rskim

Smart code reader - streaming code transformation for AI agents.

[![Crates.io](https://img.shields.io/crates/v/rskim.svg)](https://crates.io/crates/rskim)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## Overview

`rskim` transforms source code by intelligently removing implementation details while preserving structure, signatures, and types - perfect for optimizing code for LLM context windows.

Think of it like `cat`, but smart about what code to show.

## Installation

### Try it (no install required)

```bash
npx rskim file.ts
```

### Install globally (recommended for regular use)

```bash
# Via npm
npm install -g rskim

# Via Cargo
cargo install rskim
```

> **Note**: Use `npx` for trying it out. For regular use, install globally to avoid npx overhead (~100-500ms per invocation).

## Quick Start

```bash
# Try it with npx (no install)
npx rskim file.ts

# Or install globally for better performance
npm install -g rskim

# Read TypeScript with structure mode
rskim file.ts

# Extract Python function signatures
rskim file.py --mode signatures

# Pipe to syntax highlighter
rskim file.rs | bat -l rust

# Read from stdin
cat code.ts | rskim - --language=type-script
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
rskim file.ts
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
rskim file.py --mode signatures
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
rskim file.ts --mode types
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
rskim file.rs --mode full
```

## Examples

### Explore a codebase

```bash
# Get overview of all TypeScript files
find src -name '*.ts' -exec rskim {} \;

# Extract all Python function signatures
rskim app.py --mode signatures > api.txt

# Review Rust types
rskim lib.rs --mode types | less
```

### Prepare code for LLMs

```bash
# Reduce token count before sending to GPT
rskim large_file.ts | wc -w
# Output: 150 (was 600)

# Get just the API surface
rskim server.py --mode signatures | pbcopy
```

### Pipe workflows

```bash
# Skim and highlight
rskim file.rs | bat -l rust

# Skim and search
rskim file.ts | grep "interface"

# Skim multiple files
cat *.py | rskim - --language=python
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
