# Skim 🔍

> **Smart code reader for AI agents** - Strip implementation, keep structure

Skim transforms source code by removing implementation details while preserving structure, signatures, and types. Built with tree-sitter for fast, accurate multi-language parsing.

[![Crates.io](https://img.shields.io/crates/v/rskim.svg)](https://crates.io/crates/rskim)
[![npm](https://img.shields.io/npm/v/rskim.svg)](https://www.npmjs.com/package/rskim)
[![Downloads](https://img.shields.io/crates/d/rskim.svg)](https://crates.io/crates/rskim)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg?logo=rust)](https://www.rust-lang.org/)

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

- 🚀 **Fast** - 14.6ms for 3000-line files (powered by tree-sitter)
- ⚡ **Cached** - 40-50x faster on repeated processing (enabled by default)
- 🌐 **Multi-language** - TypeScript, JavaScript, Python, Rust, Go, Java, Markdown
- 🎯 **Multiple modes** - Structure, signatures, types, or full code
- 📁 **Directory support** - Process entire directories recursively (`skim src/`)
- 📂 **Multi-file** - Glob patterns (`src/**/*.ts`) with parallel processing
- 🤖 **Auto-detection** - Automatically detects language from file extension
- 🔒 **DoS-resistant** - Built-in limits prevent stack overflow and memory exhaustion
- 💧 **Streaming** - Outputs to stdout for pipe workflows

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

### From Source

```bash
git clone https://github.com/dean0x/skim.git
cd skim
cargo build --release
# Binary at target/release/skim
```

## Quick Start

```bash
# Try it with npx (no install)
npx rskim src/app.ts

# Or install globally for better performance
npm install -g rskim

# Extract structure from single file (auto-detects language)
skim src/app.ts

# Process entire directory recursively (auto-detects all languages)
skim src/

# Process current directory
skim .

# Process multiple files with glob patterns
skim 'src/**/*.ts'

# Process all TypeScript files with custom parallelism
skim '*.{js,ts}' --jobs 4

# Get only function signatures from multiple files
skim 'src/*.ts' --mode signatures --no-header

# Extract type definitions
skim src/types.ts --mode types

# Extract markdown headers (H1-H3 for structure, H1-H6 for signatures/types)
skim README.md --mode structure

# Pipe to other tools
skim src/app.ts | bat -l typescript

# Read from stdin (REQUIRES --language flag)
cat app.ts | skim - --language=typescript

# Override language detection for unusual file extensions
skim weird.inc --language=typescript

# Clear cache
skim --clear-cache

# Disable caching for pure transformation
skim file.ts --no-cache

# Show token reduction statistics
skim file.ts --show-stats
```

## Usage

```bash
skim [FILE|DIRECTORY] [OPTIONS]
```

**Arguments:**
- `<FILE>` - File, directory, or glob pattern to process (use '-' for stdin)
  - Single file: `skim file.ts` (auto-detects language from extension)
  - Directory: `skim src/` (recursively processes all supported files)
  - Glob pattern: `skim 'src/**/*.ts'` (processes matching files)
  - Stdin: `skim -` (requires `--language` flag)

**Options:**
- `-m, --mode <MODE>` - Transformation mode [default: structure]
  - Values: `structure`, `signatures`, `types`, `full`
- `-l, --language <LANGUAGE>` - Override language detection (required for stdin, optional fallback otherwise)
  - Values: `typescript`, `javascript`, `python`, `rust`, `go`, `java`, `markdown`
  - **Auto-detection:** Language is automatically detected from file extensions by default
  - **Use when:** Reading from stdin, or processing files with unusual extensions
- `-j, --jobs <JOBS>` - Number of parallel jobs for multi-file processing [default: number of CPUs]
- `--no-header` - Don't print file path headers for multi-file output
- `--no-cache` - Disable caching (caching is enabled by default)
- `--clear-cache` - Clear all cached files and exit
- `--show-stats` - Show token reduction statistics (output to stderr)
- `-h, --help` - Print help
- `-V, --version` - Print version

## Transformation Modes

### Structure Mode (Default)

**Token reduction: 70-80%**

Keeps function/method signatures, class declarations, type definitions, imports/exports. Strips all implementation bodies.

```bash
skim file.ts --mode structure
```

**Use case**: Understanding code organization and APIs

### Signatures Mode

**Token reduction: 85-92%**

More aggressive - keeps ONLY callable signatures, removes everything else.

```bash
skim file.ts --mode signatures
```

**Use case**: Generating API documentation or type stubs

### Types Mode

**Token reduction: 90-95%**

Keeps only type definitions (interfaces, type aliases, enums). Removes all code.

```bash
skim file.ts --mode types
```

**Use case**: Type system analysis

### Full Mode

**Token reduction: 0%**

No transformation - returns original source (like `cat`).

```bash
skim file.ts --mode full
```

**Use case**: Passthrough for testing or comparison

## Supported Languages

| Language   | Status | Extensions         | Notes                    |
|------------|--------|--------------------|--------------------------|
| TypeScript | ✅     | `.ts`, `.tsx`      | Excellent grammar        |
| JavaScript | ✅     | `.js`, `.jsx`      | Full ES2024 support      |
| Python     | ✅     | `.py`, `.pyi`      | Complete coverage        |
| Rust       | ✅     | `.rs`              | Up-to-date grammar       |
| Go         | ✅     | `.go`              | Stable                   |
| Java       | ✅     | `.java`            | Good coverage            |
| Markdown   | ✅     | `.md`, `.markdown` | Header extraction        |

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

### Markdown

```markdown
# Input
# Project Documentation

This is the introduction to our project.

## Getting Started

Follow these steps to get started.

### Prerequisites

You'll need Node.js installed.

#### Installation

Run npm install.

# Output (structure mode - H1-H3 only)
# Project Documentation
## Getting Started
### Prerequisites

# Output (signatures/types mode - H1-H6 all headers)
# Project Documentation
## Getting Started
### Prerequisites
#### Installation
```

## Use Cases

### 1. LLM Context Optimization

```bash
# Send only structure to AI for code review
skim src/app.ts | llm "Review this architecture"

# Process entire directory for LLM context (auto-detects all languages)
skim src/ --no-header | llm "Analyze this codebase"

# Process specific subdirectory
skim src/components/ --mode signatures | llm "Review these components"
```

### 2. Codebase Documentation

```bash
# Generate API surface documentation from directory
skim src/ --mode signatures > api-docs.txt

# Process specific file types with glob pattern
skim 'lib/**/*.py' --mode signatures --jobs 8 > python-api.txt

# Document mixed-language codebase (auto-detects each file)
skim . --no-header --mode signatures > full-api.txt
```

### 3. Type System Analysis

```bash
# Extract all type definitions from directory
skim src/ --mode types --no-header

# Extract types from specific files
skim 'src/**/*.ts' --mode types --no-header
```

### 4. Code Navigation

```bash
# Quick overview of file structure
skim large-file.py | less

# Overview of entire directory
skim src/auth/ | less

# Overview of specific module
skim 'src/auth/*.ts' | less
```

## Caching

**Caching is enabled by default** for maximum performance on repeated processing.

### How It Works

- **Cache key**: SHA256 hash of (file path + modification time + mode)
- **Location**: `~/.cache/skim/` (platform-specific)
- **Invalidation**: Automatic when file is modified (mtime-based)
- **Storage**: JSON files with metadata

### Performance Impact

| Scenario | Time | Speedup |
|----------|------|---------|
| First run (no cache) | 244ms | 1.0x |
| **Second run (cached)** | **5ms** | **48.8x faster!** |

### Cache Management

```bash
# View cache location
ls ~/.cache/skim/

# Clear all cache
skim --clear-cache

# Disable caching (for specific run)
skim file.ts --no-cache

# Caching works with all features
skim 'src/**/*.ts' --jobs 8 --mode signatures  # Cached by default
```

### When Caching Helps

- ✅ Repeated processing of same files (e.g., in watch mode)
- ✅ Large codebases with infrequent changes
- ✅ CI/CD pipelines processing same files multiple times
- ✅ Development workflows with hot reloading

### When to Disable Caching

- ⚠️ One-time transformations for LLM input (no benefit)
- ⚠️ Piping through stdin (caching not supported)
- ⚠️ Testing/debugging transformation logic
- ⚠️ Disk space constrained environments

## Token Counting

**Show token reduction statistics** with the `--show-stats` flag to understand context window savings.

```bash
skim file.ts --show-stats
# Output (stderr): [skim] 1,000 tokens → 200 tokens (80.0% reduction)

skim 'src/**/*.ts' --show-stats
# Output (stderr): [skim] 15,000 tokens → 3,000 tokens (80.0% reduction) across 50 file(s)
```

### Features
- Uses OpenAI's tiktoken (cl100k_base encoding for GPT-3.5/GPT-4)
- Works with single files, multi-file globs, and stdin
- Output to stderr (keeps stdout clean for piping)
- Aggregates stats across multiple files

### Use Cases
- **LLM optimization**: Measure how much context window you're saving
- **Mode comparison**: Compare reduction rates between structure/signatures/types modes
- **Benchmarking**: Track token efficiency improvements

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

**Target**: <50ms for 1000-line files ✅ **Exceeded** (14.6ms for 3000-line files)

### Benchmark Results

**Small files (<100 lines):**
- Go: 60µs (fastest)
- Rust: 68µs
- Python: 73µs
- Java: 84µs
- TypeScript: 33µs (simple) / 83µs (medium complexity)

**Scaling (structure mode):**
- 100 functions (300 lines): 1.3ms
- 500 functions (1500 lines): 6.4ms
- **1000 functions (3000 lines): 14.6ms** ✅

### Real-World Token Reduction

**Production TypeScript Codebase (80 files):**

| Mode | Original Tokens | Final Tokens | Reduction | Saved Tokens |
|------|----------------|--------------|-----------|--------------|
| Full (no transform) | 63,198 | 63,198 | 0% | 0 |
| **Structure** | 63,198 | 25,119 | **60.3%** | 38,079 |
| **Signatures** | 63,198 | 7,328 | **88.4%** | 55,870 |
| **Types** | 63,198 | 5,181 | **91.8%** | 58,017 |

**What this means:**
- Structure mode: Fit **2.5x more code** in your LLM context window
- Signatures mode: Fit **8.6x more code** for API documentation
- Types mode: Fit **12.2x more code** for type system analysis

**Use cases:**
- LLM context optimization: "Explain this entire codebase" (60-90% smaller)
- Documentation generation: Extract all public APIs in milliseconds
- Type analysis: Focus only on type definitions and interfaces

**Run benchmarks:**
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

**Current**: Production ready (v0.5.0+)

✅ **Implemented:**
- TypeScript/JavaScript/Python/Rust/Go/Java/Markdown support
- Structure/signatures/types/full modes
- CLI with stdin support
- **Directory support (`skim src/` - recursively processes all files)**
- Multi-file glob support (`skim 'src/**/*.ts'`)
- **Automatic language detection from file extensions**
- Parallel processing with rayon (`--jobs` flag)
- **Caching layer with mtime-based invalidation (enabled by default)**
- **Token counting with `--show-stats` (GPT-3.5/GPT-4 compatible)**
- DoS protections
- Comprehensive test suite (128 tests passing)
- Performance benchmarks (verified: 14.6ms for 3000-line files, 5ms cached)
- npm and cargo distribution

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
