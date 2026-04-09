# Skim: The Most Intelligent Context Optimization Engine for Coding Agents

> **Code skimming. Command rewriting. Test, build, and git output compression. Token budget cascading.** 17 languages. 14ms for 3,000 lines. Built in Rust.

Other tools filter terminal noise. Skim understands your code. It parses ASTs across 17 languages, strips implementation while preserving architecture, then optimizes every other type of context your agent consumes: test output, build errors, git diffs, and raw commands. 14ms for 3,000 lines. 48x faster on cache hits.

[![Website](https://img.shields.io/badge/Website-skim-e87040)](https://dean0x.github.io/x/skim/)
[![CI](https://github.com/dean0x/skim/actions/workflows/ci.yml/badge.svg)](https://github.com/dean0x/skim/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/rskim.svg)](https://crates.io/crates/rskim)
[![npm](https://img.shields.io/npm/v/rskim.svg)](https://www.npmjs.com/package/rskim)
[![Downloads](https://img.shields.io/crates/d/rskim.svg)](https://crates.io/crates/rskim)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

## Why Skim?

**Context capacity is not the bottleneck. Attention is.** Every token you send to an LLM dilutes its focus. Research consistently shows attention dilution in long contexts -- models lose track of critical details even within their window. More tokens means higher latency, degraded recall, and weaker reasoning. Past a threshold, adding context makes outputs worse. While other tools stop at filtering command output, Skim parses your actual code structure and optimizes the full spectrum of agent context: code, test output, build errors, git diffs, and commands. Deeper, broader, and smarter than anything else available.

Take a typical 80-file TypeScript project: 63,000 tokens. That contains maybe 5,000 tokens of actual signal. The rest is implementation noise the model doesn't need for architectural reasoning.

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

### Code Skimming (the original, still unmatched)
- **17 languages** including TypeScript, JavaScript, Python, Rust, Go, Java, C, C++, C#, Ruby, SQL, Kotlin, Swift, Markdown, JSON, YAML, TOML
- **6 transformation modes** from full to minimal to pseudo to structure to signatures to types (15-95% reduction)
- **14.6ms** for 3,000-line files. **48x faster** on cache hits
- **Token budget cascading** that automatically selects the most aggressive mode fitting your budget
- **Parallel processing** with multi-file globs via rayon

### Command Rewriting (`skim init`)
- PreToolUse hook rewrites `cat`, `head`, `tail`, `cargo test`, `npm test`, `git diff` into skim equivalents
- Two-layer rule system with declarative prefix-swap and custom argument handlers
- One command installs the hook for automatic, invisible context savings

### Test Output Compression (`skim test`)
- Parses and compresses output from cargo, go, vitest, jest, pytest
- Extracts failures, assertions, pass/fail counts while stripping noise
- Three-tier degradation from structured parse to regex fallback to passthrough

### Build Output Compression (`skim build`)
- Parses cargo, clippy, tsc build output
- Extracts errors, warnings, and summaries

### Lint Output Compression (`skim lint`)
- Parses ESLint, Ruff, mypy, golangci-lint output
- Extracts errors and warnings with severity grouping
- Three-tier degradation from structured parse to regex fallback to passthrough

### Package Manager Compression (`skim pkg`)
- Parses npm, pnpm, pip, cargo audit/install/outdated output
- Extracts vulnerabilities, version conflicts, and dependency issues

### Git Output Compression (`skim git`)
- **`skim git diff`** -- AST-aware: shows changed functions with full boundaries and `+`/`-` markers, strips diff noise
  - `--mode structure` adds unchanged functions as signatures for context
  - `--mode full` shows entire files with change markers
  - Supports `--staged`, commit ranges (`HEAD~3`, `main..feature`)
- Compresses `git status` and `git log` with flag-aware passthrough
- All subcommands support `--json` for machine-readable output

### Intelligence
- `skim discover` scans agent session history for optimization opportunities
- `skim learn` detects CLI error-retry patterns and generates correction rules
- Output guardrail ensures compressed output is never larger than the original

## Installation

### Try it (no install required)

```bash
npx rskim file.ts
```

### Install globally (recommended for regular use)

```bash
# Via Homebrew (macOS/Linux)
brew install dean0x/tap/skim

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
- `-m, --mode` - Transformation mode: `structure` (default), `signatures`, `types`, `full`, `minimal`, `pseudo`
- `-l, --language` - Override auto-detection (required for stdin only)
- `-j, --jobs` - Parallel processing threads (default: CPU cores)
- `--no-cache` - Disable caching
- `--show-stats` - Show token reduction stats
- `--disable-analytics` - Disable analytics recording

📖 **[Full Usage Guide →](docs/usage.md)**

## Transformation Modes

Skim offers six modes with different levels of aggressiveness:

| Mode       | Reduction | What's Kept                              | Use Case                   |
|------------|-----------|------------------------------------------|----------------------------|
| Full       | 0%        | Everything (original source)             | Testing/comparison         |
| Minimal    | 15-30%    | All code, doc comments                   | Light cleanup              |
| Pseudo     | 30-50%    | Logic flow, names, values                | LLM context with logic     |
| Structure  | 70-80%    | Signatures, types, classes, imports      | Understanding architecture |
| Signatures | 85-92%    | Only callable signatures                 | API documentation          |
| Types      | 90-95%    | Only type definitions                    | Type system analysis       |

```bash
skim file.ts --mode structure   # Default
skim file.ts --mode pseudo      # Pseudocode (strips types, visibility, decorators)
skim file.ts --mode signatures  # More aggressive
skim file.ts --mode types       # Most aggressive
skim file.ts --mode full        # No transformation
```

**Note on JSON/YAML/TOML files:** JSON, YAML, and TOML always use structure extraction regardless of mode. Since they are data (not code), there are no "signatures" or "types" to extract—only structure. All modes produce identical output for these file types.

📖 **[Detailed Mode Guide →](docs/modes.md)**

## Supported Languages

| Language   | Status | Extensions         | Notes                           |
|------------|--------|--------------------|---------------------------------|
| TypeScript | ✅     | `.ts`, `.tsx`      | Excellent grammar               |
| JavaScript | ✅     | `.js`, `.jsx`      | Full ES2024 support             |
| Python     | ✅     | `.py`, `.pyi`      | Complete coverage               |
| Rust       | ✅     | `.rs`              | Up-to-date grammar              |
| Go         | ✅     | `.go`              | Stable                          |
| Java       | ✅     | `.java`            | Good coverage                   |
| C          | ✅     | `.c`, `.h`         | Full C11 support                |
| C++        | ✅     | `.cpp`, `.hpp`, `.cc`, `.hh`, `.cxx`, `.hxx` | C++20 support |
| Markdown   | ✅     | `.md`, `.markdown` | Header extraction               |
| JSON       | ✅     | `.json`            | Structure extraction (serde)    |
| YAML       | ✅     | `.yaml`, `.yml`    | Multi-document support (serde)  |
| TOML       | ✅     | `.toml`            | Structure extraction (toml)     |
| C#         | ✅     | `.cs`              | Full grammar, structs/interfaces|
| Ruby       | ✅     | `.rb`              | Classes, modules, methods       |
| SQL        | ✅     | `.sql`             | DDL/DML via tree-sitter-sequel  |
| Kotlin     | ✅     | `.kt`, `.kts`      | Data classes, coroutines, sealed classes |
| Swift      | ✅     | `.swift`           | Protocols, generics, SwiftUI structs |

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

### JSON

```json
// Input
{
  "user": {
    "profile": {
      "name": "Jane Smith",
      "age": 28,
      "tags": ["admin", "verified"]
    },
    "settings": {
      "theme": "dark",
      "notifications": true
    }
  },
  "items": [
    {"id": 1, "price": 100},
    {"id": 2, "price": 200}
  ]
}

// Output (structure mode)
{
  user: {
    profile: {
      name,
      age,
      tags
    },
    settings: {
      theme,
      notifications
    }
  },
  items: {
    id,
    price
  }
}
```

### YAML (Multi-Document)

```yaml
# Input (Kubernetes manifests)
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: app-config
data:
  database_url: postgres://localhost:5432
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: web-app
spec:
  replicas: 3

# Output (structure mode)
apiVersion
kind
metadata:
  name
data:
  database_url
---
apiVersion
kind
metadata:
  name
spec:
  replicas
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

## Claude Code Plugin

Skim includes a **Skimmer** plugin for Claude Code — a codebase orientation agent that maps project structure, finds task-relevant code, and generates integration plans.

### Install

**Option A: Via the skim marketplace**
```
/plugin marketplace add dean0x/skim
/plugin install skimmer
```

**Option B: Direct from the standalone repo**
```
/plugin marketplace add dean0x/skimmer
```

> **Note:** `dean0x/skim` is a custom marketplace. Unlike the official Claude Code plugin directory, custom marketplaces must be added explicitly before plugins become available.

### Usage

```
# Orient for a specific task
/skim add JWT authentication

# General codebase orientation
/skim
```

The Skimmer agent uses `rskim` to extract code structure, then maps relevant files, signatures, and integration points for your task.

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

## Analytics

Skim automatically tracks token savings from every invocation in a local SQLite database (`~/.cache/skim/analytics.db`). View your savings with the `stats` subcommand:

```bash
skim stats                       # All-time dashboard
skim stats --since 7d            # Last 7 days
skim stats --format json         # Machine-readable output
skim stats --cost                # Include cost savings estimates
skim stats --clear               # Reset analytics data
```

**Environment variables:**

| Variable | Description |
|----------|-------------|
| `SKIM_DISABLE_ANALYTICS` | Set to `1`, `true`, or `yes` to disable recording |
| `SKIM_INPUT_COST_PER_MTOK` | Override $/MTok for cost estimates (default: 3.0) |
| `SKIM_ANALYTICS_DB` | Override analytics database path |

Analytics recording is fire-and-forget (non-blocking) and does not affect command performance. Data is automatically pruned after 90 days.

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

**Current**: v2.3.1 — Stable

✅ **Core — Code Reading (17 languages):**
- TypeScript/JavaScript/Python/Rust/Go/Java/C/C++/C#/Ruby/SQL/Markdown/JSON/YAML/TOML
- 5 transformation modes: structure, signatures, types, minimal, full
- Token budget (`--tokens N`), max lines (`--max-lines N`), last lines (`--last-lines N`)
- Multi-file glob support, parallel processing, caching (40-50x speedup)

✅ **Command Output Compression:**
- Test runners: cargo test, pytest, vitest/jest, go test
- Build tools: cargo build, cargo clippy, tsc
- Git: status, diff, log
- File tools: find, ls, tree, grep, rg
- Log: JSON structured + plaintext dedup, debug filtering, stack trace collapsing
- Infra: gh, aws, curl, wget
- Three-tier degradation: Structured → Regex → Passthrough

✅ **Agent Integration:**
- `skim init` — hook installation for Claude Code, Cursor, Codex, Gemini, Copilot, OpenCode
- `skim rewrite` — command rewriting engine with `--hook` mode
- MCP server mode for agent-native workflows

✅ **Analytics & Intelligence:**
- `skim stats` — persistent SQLite dashboard with cost estimation
- `skim discover` — missed optimization finder across agent sessions
- `skim learn` — CLI error pattern detection and correction rules

✅ **Distribution:**
- cargo (`cargo install rskim`), npm (`npx rskim`), Homebrew (`brew install dean0x/tap/skim`)
- 2,223 tests passing, 14.6ms performance (3x under target)

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

## Part of the AI Development Stack

| Tool | Role | What It Does |
|------|------|-------------|
| **Skim** | Context Optimization | Code-aware AST parsing across 17 languages, command rewriting, test/build/git output compression |
| **[DevFlow](https://github.com/dean0x/devflow)** | Quality Orchestration | 18 parallel reviewers, working memory, self-learning, composable plugin system |
| **[Autobeat](https://github.com/dean0x/autobeat)** | Agent Orchestration | Autonomous orchestration. Eval loops, multi-agent pipelines, DAG dependencies, crash-proof persistence |

Skim optimizes every byte of context. DevFlow enforces production-grade quality. Autobeat scales execution across agents. No other stack covers all three.

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

- **Website**: https://dean0x.github.io/x/skim/
- **Repository**: https://github.com/dean0x/skim
- **Issues**: https://github.com/dean0x/skim/issues
- **Crates.io**: https://crates.io/crates/rskim
- **npm**: https://www.npmjs.com/package/rskim

---

**Built with ❤️ in Rust**
