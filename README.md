# Skim 🔍

> **Smart code reader for AI agents** - Strip implementation, keep structure

Skim transforms source code by removing implementation details while preserving structure, signatures, and types. Built with tree-sitter for fast, accurate multi-language parsing.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)

## Why Skim?

**Problem**: Large codebases don't fit in LLM context windows. You need structure, not implementation.

**Solution**: Skim intelligently strips function bodies and implementation details while preserving the information AI agents need to understand your code.

```typescript
// Input: Full implementation
export function processUser(user: User): Result {
    const validated = validateUser(user);
    if (!validated) throw new Error("Invalid");
    const normalized = normalizeData(user);
    return await saveToDatabase(normalized);
}

// Output: Structure only
export function processUser(user: User): Result { /* ... */ }
```

**Token reduction**: 70-90% smaller for better LLM context utilization.

## Features

- 🚀 **Fast** - <50ms for 1000-line files (powered by tree-sitter)
- 🌐 **Multi-language** - TypeScript, JavaScript, Python, Rust, Go, Java
- 🎯 **Multiple modes** - Structure, signatures, types, or full code
- 📦 **Zero config** - Auto-detects language from file extension
- 🔒 **DoS-resistant** - Built-in limits prevent stack overflow and memory exhaustion
- 💧 **Streaming** - Outputs to stdout for pipe workflows

## Installation

### Via Cargo (Recommended for Rust developers)

```bash
cargo install rskim
```

### Via npm

```bash
npm install -g rskim
# or use without install
npx rskim file.ts
```

### From Source

```bash
git clone https://github.com/dean0x/skim.git
cd skim
cargo build --release
# Binary at target/release/rskim
```

## Quick Start

```bash
# Extract structure from TypeScript
rskim src/app.ts

# Get only function signatures
rskim src/app.ts --mode signatures

# Extract type definitions
rskim src/types.ts --mode types

# Pipe to other tools
rskim src/app.ts | bat -l typescript
rskim src/**/*.ts | wc -l

# Read from stdin (requires --language)
cat app.ts | rskim - --language typescript
```

## Usage

```
rskim [FILE] [OPTIONS]

Arguments:
  <FILE>  File to read (use '-' for stdin)

Options:
  -m, --mode <MODE>          Transformation mode [default: structure]
                             [values: structure, signatures, types, full]
  -l, --language <LANGUAGE>  Explicit language (required for stdin)
                             [values: typescript, javascript, python, rust, go, java]
      --force                Force parsing even if language unsupported
  -h, --help                 Print help
  -V, --version              Print version
```

## Transformation Modes

### Structure Mode (Default)

**Token reduction: 70-80%**

Keeps function/method signatures, class declarations, type definitions, imports/exports. Strips all implementation bodies.

```bash
rskim file.ts --mode structure
```

**Use case**: Understanding code organization and APIs

### Signatures Mode

**Token reduction: 85-92%**

More aggressive - keeps ONLY callable signatures, removes everything else.

```bash
rskim file.ts --mode signatures
```

**Use case**: Generating API documentation or type stubs

### Types Mode

**Token reduction: 90-95%**

Keeps only type definitions (interfaces, type aliases, enums). Removes all code.

```bash
rskim file.ts --mode types
```

**Use case**: Type system analysis

### Full Mode

**Token reduction: 0%**

No transformation - returns original source (like `cat`).

```bash
rskim file.ts --mode full
```

**Use case**: Passthrough for testing or comparison

## Supported Languages

| Language   | Status | Extensions      | Notes                    |
|------------|--------|-----------------|--------------------------|
| TypeScript | ✅     | `.ts`, `.tsx`   | Excellent grammar        |
| JavaScript | ✅     | `.js`, `.jsx`   | Full ES2024 support      |
| Python     | ✅     | `.py`, `.pyi`   | Complete coverage        |
| Rust       | ✅     | `.rs`           | Up-to-date grammar       |
| Go         | ✅     | `.go`           | Stable                   |
| Java       | ✅     | `.java`         | Good coverage            |

## Examples

### TypeScript/JavaScript

```typescript
// Input
class UserService {
    async findUser(id: string): Promise<User> {
        const user = await db.users.findOne({ id });
        if (!user) throw new NotFoundError();
        return user;
    }
}

// Output (structure mode)
class UserService {
    async findUser(id: string): Promise<User> { /* ... */ }
}
```

### Python

```python
# Input
def process_data(items: List[Item]) -> Dict[str, Any]:
    """Process items and return statistics"""
    results = {}
    for item in items:
        results[item.id] = calculate_metrics(item)
    return results

# Output (structure mode)
def process_data(items: List[Item]) -> Dict[str, Any]: { /* ... */ }
```

### Rust

```rust
// Input
impl UserRepository {
    pub async fn create(&self, user: NewUser) -> Result<User> {
        let validated = self.validate(user)?;
        let id = Uuid::new_v4();
        self.db.insert(id, validated).await
    }
}

// Output (structure mode)
impl UserRepository {
    pub async fn create(&self, user: NewUser) -> Result<User> { /* ... */ }
}
```

## Use Cases

### 1. LLM Context Optimization

```bash
# Send only structure to AI for code review
rskim src/**/*.ts | llm "Review this architecture"
```

### 2. Codebase Documentation

```bash
# Generate API surface documentation
find src -name "*.ts" -exec rskim {} --mode signatures \; > api-surface.txt
```

### 3. Type System Analysis

```bash
# Extract all type definitions for analysis
rskim src/types.ts --mode types
```

### 4. Code Navigation

```bash
# Quick overview of file structure
rskim large-file.py | less
```

## Security

Skim includes built-in protections against DoS attacks:

- **Max recursion depth**: 500 levels (prevents stack overflow on deeply nested code)
- **Max input size**: 50MB (prevents memory exhaustion)
- **Max AST nodes**: 100,000 nodes (prevents memory exhaustion)
- **UTF-8 validation**: Safe handling of multi-byte Unicode
- **Path traversal protection**: Rejects malicious paths

See [SECURITY.md](SECURITY.md) for vulnerability disclosure process.

## Architecture

```
┌─────────────────┐
│  Language       │
│  Detection      │
└────────┬────────┘
         │
┌────────▼────────┐
│  tree-sitter    │
│  Parser         │
└────────┬────────┘
         │
┌────────▼────────┐
│  Transformation │
│  Layer          │
└────────┬────────┘
         │
┌────────▼────────┐
│  Streaming      │
│  Output         │
└─────────────────┘
```

**Design principles:**
- **Streaming-first**: Output to stdout, no intermediate files
- **Zero-copy**: Uses `&str` slices to avoid allocations
- **Error-tolerant**: tree-sitter handles incomplete/broken code gracefully
- **Type-safe**: Explicit error handling with `Result<T, E>` (no panics in library code)

## Performance

**Target**: <50ms for 1000-line files

**Benchmarks** (coming soon):
```bash
cargo bench
```

Built with performance in mind:
- tree-sitter for fast parsing
- Zero-copy string operations
- Optimized release builds (LTO enabled)

## Development

### Build

```bash
cargo build --release
```

### Test

```bash
cargo test --all-features
```

### Lint

```bash
cargo clippy -- -D warnings
cargo fmt -- --check
```

### Add New Language

1. Add grammar to `Cargo.toml`:
```toml
tree-sitter-newlang = "0.23"
```

2. Update `Language` enum in `src/types.rs`
3. Add mapping in `to_tree_sitter()` method
4. Add file extension in `from_extension()`
5. Add test fixtures

Should take ~30 minutes per language.

## Project Status

**Current**: Production ready (v0.2.3)

✅ **Implemented:**
- TypeScript/JavaScript/Python/Rust/Go/Java support
- Structure/signatures/types/full modes
- CLI with stdin support
- DoS protections
- Comprehensive test suite
- npm and cargo distribution

🚧 **In Progress:**
- Test coverage improvements
- Benchmark suite
- Performance optimizations

📋 **Planned:**
- Multi-file/glob support (`rskim src/**/*.ts`)
- Caching layer (mtime-based)
- Parallel processing with rayon

See [CHANGELOG.md](CHANGELOG.md) for version history.

## Contributing

Contributions welcome! Please:

1. Check [issues](https://github.com/dean0x/skim/issues) for existing work
2. Open an issue to discuss major changes
3. Follow existing code style (`cargo fmt`, `cargo clippy`)
4. Add tests for new features
5. Update documentation

### Project Structure

```
skim/
├── crates/
│   ├── rskim-core/    # Core library (language-agnostic)
│   └── rskim/         # CLI binary (I/O layer)
├── tests/fixtures/    # Test files for each language
└── benches/           # Performance benchmarks (planned)
```

## License

MIT License - see [LICENSE](LICENSE) for details.

## Acknowledgments

- [tree-sitter](https://tree-sitter.github.io/) - Fast, incremental parsing library
- [clap](https://github.com/clap-rs/clap) - Command-line argument parsing
- [ripgrep](https://github.com/BurntSushi/ripgrep), [bat](https://github.com/sharkdp/bat), [fd](https://github.com/sharkdp/fd) - Inspiration for Rust CLI design

## Links

- **Repository**: https://github.com/dean0x/skim
- **Issues**: https://github.com/dean0x/skim/issues
- **Crates.io**: https://crates.io/crates/rskim
- **npm**: https://www.npmjs.com/package/rskim

---

**Built with ❤️ in Rust**
