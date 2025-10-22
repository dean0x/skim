# Skim 🔍

> **Rust based smart code reader for AI agents** - Strip implementation, keep structure

Skim transforms source code by removing implementation details while preserving structure, signatures, and types. Built with tree-sitter for fast, accurate multi-language parsing.

[![Crates.io](https://img.shields.io/crates/v/rskim.svg)](https://crates.io/crates/rskim)
[![npm](https://img.shields.io/npm/v/rskim.svg)](https://www.npmjs.com/package/rskim)
[![Downloads](https://img.shields.io/crates/d/rskim.svg)](https://crates.io/crates/rskim)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg?logo=rust)](https://www.rust-lang.org/)

## Why Skim?

Take a typical 80-file TypeScript project: 63,000 tokens. Modern LLMs handle 200k+ context, so capacity isn't the issue.

But **context capacity isn't the bottleneck — attention is.** That 63k contains maybe 5k of actual signal. The rest? Implementation noise: loop bodies, error handlers, validation chains the model doesn't need to reason about architecture.

**Large contexts degrade model performance.** Research consistently shows attention dilution in long contexts — models lose track of critical details even within their window. More tokens means higher latency, degraded recall, and weaker reasoning. The inverse scaling problem: past a threshold, *adding context makes outputs worse.*

**80% of the time, the model doesn't need implementation details.** It doesn't care *how* you loop through users or validate emails. It needs to understand *what* your code does and how pieces connect.

That's where Skim comes in.

| Mode       | Tokens | Reduction | Use Case                   |
|------------|--------|-----------|----------------------------|
| Full       | 63,198 | 0%        | Original source code       |
| Structure  | 25,119 | 60.3%     | Understanding architecture |
| Signatures | 7,328  | 88.4%     | API documentation          |
| Types      | 5,181  | 91.8%     | Type system analysis       |

For example:

```typescript
// Before: Full implementation (100 tokens)
export function processUser(user: User): Result {
    const validated = validateUser(user);
    if (!validated) throw new Error("Invalid");
    const normalized = normalizeData(user);
    return await saveToDatabase(normalized);
}

// After: Structure only (12 tokens)
export function processUser(user: User): Result { /* ... */ }
```

**One command. 60-90% smaller.** Your 63,000-token codebase? Now 5,000 tokens. Fits comfortably in a single prompt with room for your question.

That same 80-file project that wouldn't fit? Now you can ask: *"Explain the entire authentication flow"* or *"How do these services interact?"* — and the AI actually has enough context to answer.

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
# Basic usage (auto-detects language)
skim file.ts                    # Single file
skim src/                       # Directory (recursive)
skim 'src/**/*.ts'             # Glob pattern

# With options
skim file.ts --mode signatures  # Different mode
skim src/ --jobs 8             # Parallel processing
skim - --language typescript   # Stdin (requires --language)
```

**Common options:**
- `-m, --mode` - Transformation mode: `structure` (default), `signatures`, `types`, `full`
- `-l, --language` - Override auto-detection (required for stdin only)
- `-j, --jobs` - Parallel processing threads (default: CPU cores)
- `--no-cache` - Disable caching
- `--show-stats` - Show token reduction stats

📖 **[Full Usage Guide →](docs/usage.md)**

## Transformation Modes

Skim offers four modes with different levels of aggressiveness:

| Mode       | Reduction | What's Kept                              | Use Case                   |
|------------|-----------|------------------------------------------|----------------------------|
| Structure  | 70-80%    | Signatures, types, classes, imports      | Understanding architecture |
| Signatures | 85-92%    | Only callable signatures                 | API documentation          |
| Types      | 90-95%    | Only type definitions                    | Type system analysis       |
| Full       | 0%        | Everything (original source)             | Testing/comparison         |

```bash
skim file.ts --mode structure   # Default
skim file.ts --mode signatures  # More aggressive
skim file.ts --mode types       # Most aggressive
skim file.ts --mode full        # No transformation
```

📖 **[Detailed Mode Guide →](docs/modes.md)**

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

### TypeScript

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

📖 **[More Examples (All Languages) →](docs/examples.md)**

## Use Cases

### LLM Context Optimization

Reduce codebase size by 60-90% to fit in LLM context windows:

```bash
skim src/ --no-header | llm "Analyze this codebase"
skim src/app.ts | llm "Review this architecture"
```

### API Documentation

Extract function signatures for documentation:

```bash
skim src/ --mode signatures > api-docs.txt
skim 'lib/**/*.py' --mode signatures > python-api.txt
```

### Type System Analysis

Focus on type definitions and interfaces:

```bash
skim src/ --mode types --no-header
skim 'src/**/*.ts' --mode types
```

### Code Navigation

Quick overview without implementation details:

```bash
skim large-file.py | less
skim src/auth/ | less
```

📖 **[10 Detailed Use Cases →](docs/use-cases.md)**

## Caching

**Caching is enabled by default** for 40-50x faster repeated processing.

### Performance Impact

| Scenario | Time | Speedup |
|----------|------|---------|
| First run (no cache) | 244ms | 1.0x |
| **Second run (cached)** | **5ms** | **48.8x faster!** |

### Cache Management

```bash
ls ~/.cache/skim/           # View cache
skim --clear-cache          # Clear cache
skim file.ts --no-cache     # Disable for one run
```

**How it works:**
- Cache key: SHA256(file path + mtime + mode)
- Automatic invalidation when files change
- Platform-specific cache directory

**When to disable caching:**
- One-time LLM transformations
- Stdin processing
- Disk-constrained environments

📖 **[Caching Internals →](docs/caching.md)**

## Token Counting

See exactly how much context you're saving with `--show-stats`:

```bash
skim file.ts --show-stats
# [skim] 1,000 tokens → 200 tokens (80.0% reduction)

skim 'src/**/*.ts' --show-stats
# [skim] 15,000 tokens → 3,000 tokens (80.0% reduction) across 50 file(s)
```

Uses OpenAI's tiktoken (cl100k_base for GPT-3.5/GPT-4). Output to stderr for clean piping.

## Security

Skim includes built-in DoS protections:

- **Max recursion depth**: 500 levels
- **Max input size**: 50MB per file
- **Max AST nodes**: 100,000 nodes
- **Path traversal protection**: Rejects malicious paths
- **No code execution**: Only parses, never runs code

📖 **[Security Details & Best Practices →](docs/security.md)**
🔒 **[Vulnerability Disclosure →](SECURITY.md)**

## Architecture

Skim uses a clean, streaming architecture:

```
Language Detection → tree-sitter Parser → Transformation → Streaming Output
```

**Design principles:**
- **Streaming-first**: Output to stdout, no intermediate files
- **Zero-copy**: Uses `&str` slices to minimize allocations
- **Error-tolerant**: Handles incomplete/broken code gracefully
- **Type-safe**: Explicit error handling, no panics

📖 **[Architecture Deep Dive →](docs/architecture.md)**

## Performance

**Target**: <50ms for 1000-line files ✅ **Exceeded** (14.6ms for 3000-line files)

### Benchmark Results

| File Size | Lines | Time   | Speed      |
|-----------|-------|--------|------------|
| Small     | 300   | 1.3ms  | 4.3µs/line |
| Medium    | 1500  | 6.4ms  | 4.3µs/line |
| **Large** | **3000** | **14.6ms** | **4.9µs/line** |

### Real-World Token Reduction

**Production TypeScript Codebase:**

| Mode       | Tokens | Reduction | LLM Context Multiplier |
|------------|--------|-----------|------------------------|
| Full       | 63,198 | 0%        | 1.0x                   |
| **Structure**  | **25,119** | **60.3%** | **2.5x more code**     |
| **Signatures** | **7,328**  | **88.4%** | **8.6x more code**     |
| **Types**      | **5,181**  | **91.8%** | **12.2x more code**    |

📖 **[Full Performance Benchmarks →](docs/performance.md)**

## Development

### Quick Start

```bash
# Build and test
cargo build --release
cargo test --all-features

# Lint
cargo clippy -- -D warnings
cargo fmt -- --check

# Benchmark
cargo bench
```

### Adding New Languages

~30 minutes per language:

1. Add tree-sitter grammar to `Cargo.toml`
2. Update `Language` enum in `src/types.rs`
3. Add file extension mapping
4. Add test fixtures
5. Run tests

📖 **[Development Guide →](docs/development.md)**

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

## Documentation

Comprehensive guides for all aspects of Skim:

- 📖 **[Usage Guide](docs/usage.md)** - Complete CLI reference and options
- 🎯 **[Transformation Modes](docs/modes.md)** - Detailed mode comparison and examples
- 💡 **[Examples](docs/examples.md)** - Language-specific transformation examples
- 🚀 **[Use Cases](docs/use-cases.md)** - 10 practical scenarios with commands
- ⚡ **[Caching](docs/caching.md)** - Caching internals and best practices
- 🔒 **[Security](docs/security.md)** - DoS protections and security best practices
- 🏗️ **[Architecture](docs/architecture.md)** - System design and technical details
- ⏱️ **[Performance](docs/performance.md)** - Benchmarks and optimization guide
- 🛠️ **[Development](docs/development.md)** - Contributing and adding languages

## Contributing

Contributions welcome! Please:

1. Check [issues](https://github.com/dean0x/skim/issues) for existing work
2. Open an issue to discuss major changes
3. Follow existing code style (`cargo fmt`, `cargo clippy`)
4. Add tests for new features
5. Update documentation

📖 **See [Development Guide](docs/development.md) for detailed instructions**

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
