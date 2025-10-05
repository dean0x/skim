# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Skim** is a streaming code reader for AI agents built in Rust using tree-sitter. It transforms source code by stripping implementation details while preserving structure, signatures, and types - optimizing code for LLM context windows.

**Key Principle:** This is a **streaming reader** (like `cat` but smart), NOT a file compression tool. Output always goes to stdout for pipe workflows.

## Current Project State

⚠️ **EARLY STAGE**: Only planning documents exist. No code implemented yet.

**What exists:**
- `vision.md` - Product vision, use cases, design principles
- `plan.md` - 12-week phased implementation plan
- `research.md` - Technology research (tree-sitter evaluation)

**What doesn't exist:**
- No Rust project (`Cargo.toml`)
- No source code
- No tests

## Technology Stack (Planned)

- **Language:** Rust (performance, zero-cost abstractions)
- **Parser:** tree-sitter (multi-language AST parsing)
- **CLI:** clap with derive API
- **Output:** Streaming to stdout via `BufWriter`
- **Distribution:** cargo-dist (cross-platform binaries + npm publishing)
- **Performance Target:** <50ms for 1000-line files

## Planned Architecture

```
Parser Manager (language detection)
  ↓
tree-sitter (TS/Python/Rust/Go/Java grammars)
  ↓
Transformation Layer (modes: structure/signatures/types/full)
  ↓
Streaming Output (stdout, zero-copy when possible)
```

## Implementation Phases

### Phase 1 (Weeks 1-4): Proof of Concept
- Single language (TypeScript)
- Basic structure extraction (strip function bodies)
- CLI with mode flags
- Streaming stdout output

### Phase 2 (Weeks 5-8): Multi-Language
- 5 languages (TypeScript, Python, Rust, Go, Java)
- Language detection from file extensions
- Performance optimization (<50ms target)
- CI pipeline

### Phase 3 (Weeks 9-12): Production
- Caching layer (mtime-based)
- Multi-file/glob support
- Parallel processing (rayon)
- Binary releases (cargo-dist for crates.io + npm)

## Installation (Once Implemented)

### For End Users

```bash
# Via npm (recommended for Node.js/TypeScript developers)
npm install -g @skim/cli        # Note: "skim" likely taken on npm
npx @skim/cli file.ts           # Zero-install usage

# Via cargo (recommended for Rust developers)
cargo install skim

# Via binary download (GitHub releases)
curl -L https://github.com/youruser/skim/releases/latest/download/skim-x86_64-unknown-linux-gnu.tar.gz | tar xz
```

⚠️ **Package naming:** "skim" is likely taken on npm. Will need `@skim/cli`, `skim-cli`, or similar scoped package.

## Commands (Once Implemented)

### Build & Test
```bash
cargo build --release          # Production build
cargo test                     # Run test suite
cargo test --all-features      # Run all tests
cargo bench                    # Run benchmarks
```

### Distribution (cargo-dist)
```bash
cargo dist init                # Initialize cargo-dist
cargo dist build               # Build for all targets
cargo dist plan                # Preview release artifacts
```

### Development
```bash
cargo run -- file.ts           # Run with default mode
cargo run -- file.ts --mode=signatures
cargo clippy -- -D warnings    # Lint check
cargo fmt -- --check           # Format check
```

### Benchmarking
```bash
cargo bench                    # Criterion benchmarks
hyperfine 'skim file.ts'       # CLI performance
cargo flamegraph --bin skim -- file.ts  # Profile hot paths
```

## Design Constraints

### MUST Follow

1. **Streaming-first** - Output to stdout, never write intermediate files
2. **Zero-copy when possible** - Use `&str` slices, avoid allocations
3. **Error-tolerant** - tree-sitter handles incomplete code gracefully
4. **Performance-critical** - <50ms for 1000 lines (benchmark regressions block)
5. **Multi-language from day 1** - No artificial TypeScript-only limitation

### MUST NOT Do

1. ❌ Add syntax highlighting (use `bat`)
2. ❌ Add linting (use language-specific linters)
3. ❌ Add type checking (use `tsc`, `mypy`, etc.)
4. ❌ Add LSP features (out of scope)
5. ❌ Silent failures (fail loud with clear error messages)

## Performance Requirements

- **Parse + Transform:** <50ms for 1000-line files
- **Token Reduction:** 60-80% (structure mode)
- **Startup Time:** <10ms (instant feel)
- **Multi-file:** <1 second for 100 files (parallel)

**Benchmark against:**
- `bat`: 10ms for 1000 lines (syntax highlighting)
- `ripgrep`: 0.2s for 1M lines (search)

## Output Modes (Planned)

```bash
skim file.ts                    # Default: structure-only
skim file.ts --mode=signatures  # Function/method signatures only
skim file.ts --mode=types       # Type definitions only
skim file.ts --mode=full        # No transformation (like cat)
```

## tree-sitter Integration

### Adding New Language

```toml
# Cargo.toml
[dependencies]
tree-sitter-newlang = "0.24"
```

```rust
// src/language.rs
SourceLanguage::NewLang => tree_sitter_newlang::LANGUAGE.into()
```

Should take <30 minutes per language.

### Grammar Quality

| Language | Status | Notes |
|----------|--------|-------|
| TypeScript | ✅ Excellent | Maintained by tree-sitter team |
| Python | ✅ Excellent | Complete coverage |
| Rust | ✅ Excellent | Very up-to-date |
| Go | ✅ Good | Stable |
| Java | ⚠️ Good | Some edge cases |

## Error Handling Philosophy

**Fail fast, fail loud:**

```rust
// ✅ GOOD
Error: File not found: nonexistent.ts
Hint: Check the file path exists

// ❌ BAD
Error: parse failed
```

**Exit codes:**
- `0` - Success
- `1` - General error
- `2` - Parse error
- `3` - Unsupported language

## Testing Strategy

### Test Fixtures Required

```
tests/fixtures/
  typescript/{simple,class,async,generics}.ts
  python/{simple,class,async,decorators}.py
  rust/{simple,struct,impl,traits}.rs
  go/{simple,struct,interface}.go
  java/{Simple,Class,Interface}.java
```

**Minimum:** 4 fixtures per language.

### Real-World Testing

Test on actual open-source projects (not just fixtures):
- TypeScript: VSCode extension samples
- Python: Flask apps
- Rust: ripgrep source
- Go: Hugo samples
- Java: Spring Boot controllers

### Integration Tests

Validate:
- 95%+ parse success on real-world code
- Output produces valid code (lints without errors)
- All type information preserved
- Token reduction achieves 60-80% savings

## Known Edge Cases

1. **Incomplete code** - tree-sitter handles gracefully (error nodes)
2. **Very large files (>100MB)** - v1: Fail with error; v2: Use memmap2
3. **Binary files** - Detect and reject with clear error
4. **Stdin input** - Support `cat file.ts | skim`

## Caching Strategy (Phase 3)

**Cache transformed output, not AST:**

```rust
CacheKey {
  path: PathBuf,
  mtime: SystemTime,  // Invalidation trigger
  mode: String,       // "structure", "signatures", etc.
}
```

**Location:** `~/.cache/skim/`
**Invalidation:** File mtime change or mode change
**Rationale:** tree-sitter is fast enough that caching may not help v1

## Distribution Strategy (cargo-dist)

### Why cargo-dist?

**Recommended tool for Rust CLI → npm distribution:**
- Auto-generates GitHub Actions for cross-platform builds
- Publishes to crates.io AND npm simultaneously
- Handles platform-specific binary downloads
- Used by ripgrep, bat, fd (our performance benchmarks)
- Reduces manual CI maintenance

### Setup (Week 11 in plan.md)

```bash
# Install cargo-dist
cargo install cargo-dist

# Initialize (creates dist config in Cargo.toml)
cargo dist init

# Generates .github/workflows/release.yml
```

### Cargo.toml Configuration

```toml
[package]
name = "skim"
version = "1.0.0"

# cargo-dist configuration
[workspace.metadata.dist]
cargo-dist-version = "0.14.0"
ci = ["github"]
installers = ["shell", "npm"]
targets = [
  "x86_64-unknown-linux-gnu",
  "x86_64-apple-darwin",
  "aarch64-apple-darwin",
  "x86_64-pc-windows-msvc"
]

# npm-specific settings
[workspace.metadata.dist.npm]
scope = "@skim"  # Publishes as @skim/cli on npm
```

### Release Process

```bash
# 1. Update version in Cargo.toml
# 2. Commit and tag
git tag v1.0.0
git push --tags

# 3. GitHub Actions automatically:
#    - Builds for all platforms
#    - Creates GitHub release
#    - Publishes to crates.io
#    - Publishes to npm registry
```

### npm Package Structure (auto-generated by cargo-dist)

```
@skim/cli/
  package.json
  bin/
    skim.js          # Wrapper that spawns correct binary
  npm/
    skim-linux-x64/
    skim-darwin-x64/
    skim-darwin-arm64/
    skim-win32-x64/
```

**Binary selection:** Wrapper detects `process.platform` and `process.arch`, downloads correct binary on first run.

### Platform Support

| Platform | Architecture | npm package |
|----------|--------------|-------------|
| Linux | x86_64 | `@skim/cli-linux-x64` |
| macOS | x86_64 | `@skim/cli-darwin-x64` |
| macOS | ARM64 (M1/M2) | `@skim/cli-darwin-arm64` |
| Windows | x86_64 | `@skim/cli-win32-x64` |

**Optional:** ARM Linux (`aarch64-unknown-linux-gnu`) if there's demand.

### Trade-offs

**Pros:**
✅ Target audience (Node.js/TS devs) can `npx @skim/cli`
✅ Lower barrier to entry (no Rust toolchain needed)
✅ Can be used in package.json scripts
✅ Automated release process

**Cons:**
❌ Larger npm package size (~5-10MB per platform)
❌ More complex CI (cross-compilation for 4+ platforms)
❌ Must keep Cargo.toml and package.json versions in sync
❌ GitHub Actions runner costs (macOS/Windows runners are more expensive)

### CI Cost Considerations

Cross-platform builds require different GitHub Actions runners:
- Linux: Free on public repos
- macOS: 10x credits (more expensive)
- Windows: 2x credits

**Estimated:** ~5-10 minutes per release × 4 platforms = 20-40 CI minutes per release.

## Starter Implementation Guide

### Week 1 Tasks (from plan.md)

1. `cargo new skim --bin`
2. Add tree-sitter dependencies
3. Basic CLI structure with clap
4. Parse simple TypeScript file
5. Create test fixtures
6. Validate AST access works

**NOTE:** cargo-dist setup happens in Phase 3 (Week 11), not Week 1.

### Critical First File: `src/parser.rs`

```rust
use tree_sitter::{Parser, Tree};

pub fn parse_typescript(source: &str) -> Result<Tree, String> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())?;
    parser.parse(source, None).ok_or("Parse failed")
}
```

### Critical Second File: `src/transform.rs`

Extract structure by:
- Visiting AST nodes with cursor
- Keeping signatures/types
- Replacing function bodies with `/* ... */`
- Preserving indentation and readability

## Success Criteria

**Phase 1 Gate:** Can `skim file.ts --mode=structure | head` and get reasonable output

**v1.0 Gate:**
- [ ] All tests pass
- [ ] Benchmarks meet <50ms target
- [ ] 5 languages supported
- [ ] `cargo install skim` works
- [ ] Documentation complete
- [ ] CI green on all platforms

## Resources

- **tree-sitter docs:** https://tree-sitter.github.io/tree-sitter/
- **tree-sitter grammars:** https://github.com/tree-sitter
- **Rust AST example:** See research.md lines 97-113

## Development Anti-Patterns

Given the vision, avoid:

1. **Adding config files** - Modes via CLI flags only (no `.skimrc`)
2. **Intermediate file writes** - Stream directly to stdout
3. **Line-buffered output** - Use `BufWriter` for performance
4. **String allocations in hot path** - Use `&str` slices
5. **Blocking on single file** - Use rayon for multi-file parallel processing

## Reference Implementation Patterns

### Zero-Copy String Slicing
```rust
// ✅ GOOD - Borrows source
let text = node.utf8_text(source.as_bytes())?;

// ❌ BAD - Allocates new String
let text = node.text().to_string();
```

### Buffered Streaming Output
```rust
use std::io::{BufWriter, Write};

let mut stdout = BufWriter::new(io::stdout());
writeln!(stdout, "{}", output)?;  // Buffered
stdout.flush()?;
```

### Language Detection
```rust
fn detect_language(path: &Path) -> Option<SourceLanguage> {
    match path.extension()?.to_str()? {
        "ts" | "tsx" => Some(SourceLanguage::TypeScript),
        "py" => Some(SourceLanguage::Python),
        // ...
        _ => None,
    }
}
```
