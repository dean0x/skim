# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Skim** is a streaming code reader for AI agents built in Rust using tree-sitter. It transforms source code by stripping implementation details while preserving structure, signatures, and types - optimizing code for LLM context windows.

**Key Principle:** This is a **streaming reader** (like `cat` but smart), NOT a file compression tool. Output always goes to stdout for pipe workflows.

## Current Project State

✅ **PHASE 3 COMPLETE** (100% of original roadmap)

**What's Complete (Phases 1 & 2):**
- ✅ Full Rust project with comprehensive test suite (2,482 tests passing)
- ✅ 17 languages supported: TypeScript, JavaScript, Python, Rust, Go, Java, C, C++, C#, Ruby, SQL, Kotlin, Swift, Markdown, JSON, YAML, TOML
- ✅ 4 transformation modes: structure, signatures, types, full
- ✅ CLI with stdin/stdout streaming support
- ✅ Multi-file glob support with parallel processing
- ✅ Published to crates.io (`cargo install rskim`)
- ✅ Published to npm (`npm install -g rskim` / `npx rskim`)
- ✅ Cross-platform binaries (Linux, macOS x64/ARM, Windows)
- ✅ CI/CD pipeline with automated releases
- ✅ Comprehensive documentation (README, CLAUDE.md, API docs)
- ✅ Performance benchmarks verified (14.6ms for 3000-line files)

**Phase 3 Status:**
- ✅ Multi-file glob support (`rskim 'src/**/*.ts'`) - COMPLETE
- ✅ Parallel processing with rayon (`--jobs` flag for multi-file operations) - COMPLETE
- ✅ Performance benchmarks (verified: 14.6ms for 3000-line files) - COMPLETE
- ✅ Parser caching layer (`~/.cache/skim/` with mtime invalidation) - COMPLETE
  - Enabled by default, 40-50x speedup on cached reads
  - `--no-cache` flag to disable, `--clear-cache` to clear
  - Implemented at CLI layer, core library remains pure
- ✅ Token counting feature (`--show-stats`) - COMPLETE
  - Uses tiktoken (cl100k_base for GPT-3.5/GPT-4)
  - Works with single files, globs, and stdin
  - Output to stderr for clean piping

## Technology Stack

- **Language:** Rust (performance, zero-cost abstractions)
- **Parser:** tree-sitter (multi-language AST parsing)
- **CLI:** clap with derive API
- **Output:** Streaming to stdout via `BufWriter`
- **Distribution:** cargo-dist (cross-platform binaries + npm publishing)
- **Performance Target:** <50ms for 1000-line files

## Architecture

```
Parser Manager (language detection)
  ↓
┌─────────────────────────────────────┐
│     Strategy Pattern Dispatcher      │
│   Language::transform_source()      │
└─────────────┬───────────────────────┘
              │
    ┌─────────┴─────────┐
    ↓                   ↓
tree-sitter         serde-based
(TS/JS/Python/      (JSON, YAML, TOML)
 Rust/Go/Java/
 C/C++/MD)
    ↓                   ↓
Transformation Layer (modes: structure/signatures/types/full)
  ↓
Streaming Output (stdout, zero-copy when possible)

analytics/          ← Token savings persistence (SQLite)
  mod.rs            ← AnalyticsDb, AnalyticsStore trait, fire-and-forget recording
  schema.rs         ← Versioned migrations (v1: token_savings, v2: analytics_meta)
```

**ARCHITECTURE NOTE**: JSON, YAML, and TOML use serde-based parsers instead of tree-sitter because they are data formats, not code. The Strategy Pattern in `Language::transform_source()` routes each language to its appropriate parser, eliminating special-case conditionals.

**ANALYTICS NOTE**: The `analytics/` module persists token savings to `~/.cache/skim/analytics.db` (SQLite with WAL mode). Recording is fire-and-forget via background threads. The `AnalyticsStore` trait enables MockStore-based testing of the stats dashboard without a real database. `--clear-cache` does NOT touch `analytics.db`.

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

## Installation

### For End Users

```bash
# Via Homebrew (macOS/Linux)
brew install dean0x/tap/skim

# Via npm (recommended - try without installing)
npx rskim file.ts

# Via npm (global install for regular use)
npm install -g rskim

# Via cargo (recommended for Rust developers)
cargo install rskim

# Via binary download (GitHub releases)
curl -L https://github.com/dean0x/skim/releases/latest/download/rskim-x86_64-unknown-linux-gnu.tar.gz | tar xz
```

✅ **Package naming:** Successfully published as `rskim` on both npm and crates.io.

## Commands

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
cargo run --bin skim -- file.ts           # Run with default mode
cargo run --bin skim -- file.ts --mode=signatures
cargo run --bin skim -- stats             # Analytics dashboard
cargo run --bin skim -- stats --verbose   # With parse quality details
cargo clippy -- -D warnings    # Lint check
cargo fmt -- --check           # Format check
```

### Subcommands
- `agents` — Display detected AI agents and their hook/session status (`--json`)
- `build` — Build output compression (cargo, clippy, tsc)
- `completions` — Shell completion scripts
- `discover` — Scan agent sessions for missed skim optimization opportunities (`--since`, `--agent`, `--json`)
- `file` — File operations compression: find, ls, tree, grep, rg output parsing (`--json`, `--show-stats`)
- `git` — Git output compression: AST-aware diff (function boundaries, `--mode`), status, log, fetch, show. All support `--json`.
- `infra` — Infrastructure tool compression: gh, aws, curl, wget output parsing (`--json`, `--show-stats`)
- `init` — Install skim as an agent hook (Claude Code, Cursor, Codex, Gemini, Copilot, OpenCode)
- `learn` — Detect CLI error-retry patterns in agent sessions and generate correction rules (`--generate`, `--agent`, `--dry-run`)
- `lint` — Lint output compression (eslint, ruff, mypy, golangci-lint)
- `log` — Log compression: JSON structured + regex plaintext deduplication, debug filtering, stack trace collapsing (`--json`, `--show-stats`)
- `pkg` — Package manager output compression (npm, pnpm, pip, cargo)
- `rewrite` — Rewrite developer commands into skim equivalents (`--hook` for agent integration)
- `stats` — Token analytics dashboard (`--since`, `--format json`, `--verbose`, `--clear`)
- `test` — Test output compression (pytest, vitest, jest, go test)

### Environment Variables

**Analytics:**
- `SKIM_DISABLE_ANALYTICS` — Set to `1`/`true`/`yes` to disable recording
- `SKIM_INPUT_COST_PER_MTOK` — Override $/MTok for cost estimates (default: 3.0, rejects negative values)
- `SKIM_ANALYTICS_DB` — Override analytics database path

**Session provider paths (used by `discover`, `learn`, `agents`):**
- `SKIM_PROJECTS_DIR` — Override Claude Code projects directory (default: `~/.claude/projects/`)
- `SKIM_CODEX_SESSIONS_DIR` — Override Codex CLI sessions directory
- `SKIM_COPILOT_DIR` — Override Copilot CLI sessions directory
- `SKIM_CURSOR_DB_PATH` — Override Cursor workspace state database path
- `SKIM_GEMINI_DIR` — Override Gemini CLI sessions directory
- `SKIM_OPENCODE_DIR` — Override OpenCode sessions directory

**Debug:**
- `SKIM_DEBUG` — Set to `1`/`true`/`yes` to enable debug output (warnings/notices on stderr). Also available as `--debug` CLI flag.

**Cache:**
- `SKIM_CACHE_DIR` — Override skim cache directory (default: `~/.cache/skim/`)

### Benchmarking
```bash
cargo bench                    # Criterion benchmarks
hyperfine 'skim file.ts'       # CLI performance
cargo flamegraph --bin skim -- file.ts  # Profile hot paths
```

## Release Process

### Pre-Release Checklist
1. Verify all target PRs are merged to main
2. Run `cargo test --all-features` — confirm passing count
3. Create release branch: `git checkout -b release/vX.Y.Z main`

### Version Bump (3 files, 4 edits)
- `crates/rskim-core/Cargo.toml` — package version
- `crates/rskim/Cargo.toml` — package version + rskim-core dependency version
- Run `cargo check` to update Cargo.lock

### Documentation Updates
- `CHANGELOG.md` — convert [Unreleased] to [X.Y.Z] - YYYY-MM-DD, add new [Unreleased], add Version History entry
- `README.md` — update version string (line ~591) and test count (line ~620)
- `CLAUDE.md` — update test count (2 locations: line ~16 and line ~505)

### Release Commit & Merge
```
git commit -m "release: vX.Y.Z — <summary>"
git checkout main
git merge release/vX.Y.Z --no-ff -m "Merge release/vX.Y.Z into main"
git tag vX.Y.Z
git push origin main --tags
```

### Automated Pipeline (triggered by tag push)
`.github/workflows/release.yml` runs: test → build (7 targets) → GitHub Release → crates.io (rskim-core first, then rskim) → npm (7 platform pkgs + main) → Homebrew tap dispatch.

**Critical:** Cargo.toml versions MUST match the tag version exactly or the build job fails.

### Post-Release Verification
- `cargo install rskim` — shows new version
- `npx rskim --version` — shows new version
- GitHub Release page — 7 binary assets attached
- `brew update && brew info dean0x/tap/skim` — formula updated

### Rollback (if CI fails after tag push)
1. Delete the tag: `git tag -d vX.Y.Z && git push origin :refs/tags/vX.Y.Z`
2. Fix the issue on the release branch
3. Re-merge to main, re-tag, re-push

### Post-Release Cleanup
- Delete release branch: `git branch -d release/vX.Y.Z`

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

## Output Modes

```bash
skim file.ts                    # Default: structure-only
skim file.ts --mode=signatures  # Function/method signatures only
skim file.ts --mode=types       # Type definitions only
skim file.ts --mode=full        # No transformation (like cat)

# Multi-file with glob patterns (NEW in v0.4.0)
skim 'src/**/*.ts'              # Process all TypeScript files recursively
skim '*.{js,ts}' --jobs 4       # Parallel processing with 4 threads
skim 'src/*.py' --no-header     # Multi-file without headers
```

## tree-sitter Integration

### Adding New Language (tree-sitter based)

```toml
# Cargo.toml (workspace dependencies section)
[dependencies]
tree-sitter-newlang = "0.23"  # CRITICAL: Must match workspace version
```

```rust
// src/language.rs
SourceLanguage::NewLang => tree_sitter_newlang::LANGUAGE.into()
```

Should take <30 minutes per language.

### Adding Non-tree-sitter Languages

For data formats that don't fit tree-sitter model (e.g., JSON, YAML, TOML -- all already implemented):

1. **Add Language variant** to `Language` enum in `src/types.rs`
2. **Return None** from `to_tree_sitter()` method
3. **Implement transform module** (e.g., `src/transform/json.rs`)
4. **Use Strategy Pattern** in `Language::transform_source()` to route parsing
5. **Return None** from all `get_*_node_types()` functions
6. **Add security limits** (max depth, max keys, etc.)
7. **Update CLI** - add variant to `LanguageArg` enum in `crates/rskim/src/main.rs`

**Key Principle:** The Strategy Pattern in `Language::transform_source()` eliminates special-case conditionals. Each language encapsulates its own parsing strategy.

```rust
// Example: Language::transform_source() dispatches to appropriate parser
impl Language {
    pub(crate) fn transform_source(&self, source: &str, mode: Mode, config: &Config) -> Result<String> {
        match self {
            Language::Json => json::transform_json(source),   // serde_json
            Language::Yaml => yaml::transform_yaml(source),   // serde_yaml_ng
            Language::Toml => toml::transform_toml(source),   // toml crate
            _ => tree_sitter_transform(source, *self, mode),  // tree-sitter
        }
    }
}
```

### Grammar Quality

| Language | Status | Notes |
|----------|--------|-------|
| TypeScript | ✅ Excellent | Maintained by tree-sitter team |
| Python | ✅ Excellent | Complete coverage |
| Rust | ✅ Excellent | Very up-to-date |
| Go | ✅ Good | Stable |
| Java | ⚠️ Good | Some edge cases |
| C | ✅ Excellent | Full C11 support |
| C++ | ✅ Good | C++20 support |
| C# | ✅ Good | Structs, interfaces, generics |
| Ruby | ✅ Good | Classes, modules, methods |
| SQL | ✅ Good | DDL/DML via tree-sitter-sequel |
| Kotlin | ✅ Good | Data classes, coroutines, sealed classes |
| Swift | ✅ Good | Protocols, generics, SwiftUI structs |

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
  c/{simple,structs,enums,pointers,preprocessor}.c
  cpp/{simple,classes,templates,namespaces,modern}.cpp
  json/{simple,nested,array,edge}.json
  yaml/{simple,kubernetes,github-actions}.yaml
  toml/{simple,cargo,nested}.toml
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

**Also stored in `~/.cache/skim/`:** `analytics.db` (token savings database). Note: `--clear-cache` clears only the parser cache, not the analytics database. Use `skim stats --clear` to reset analytics.

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
version = "2.0.0"

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
git tag v2.0.0
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
| Linux | ARM64 | `@skim/cli-linux-arm64` |
| macOS | x86_64 | `@skim/cli-darwin-x64` |
| macOS | ARM64 (M1/M2) | `@skim/cli-darwin-arm64` |
| Windows | x86_64 | `@skim/cli-win32-x64` |

### Trade-offs

**Pros:**
✅ Target audience (Node.js/TS devs) can `npx @skim/cli`
✅ Lower barrier to entry (no Rust toolchain needed)
✅ Can be used in package.json scripts
✅ Automated release process

**Cons:**
❌ Larger npm package size (~5-10MB per platform)
❌ More complex CI (cross-compilation for 5 platforms)
❌ Must keep Cargo.toml and package.json versions in sync
❌ GitHub Actions runner costs (macOS/Windows runners are more expensive)

### CI Cost Considerations

Cross-platform builds require different GitHub Actions runners:
- Linux: Free on public repos
- macOS: 10x credits (more expensive)
- Windows: 2x credits

**Estimated:** ~5-10 minutes per release × 5 platforms = 25-50 CI minutes per release.

**Note:** ARM64 Linux uses cross-compilation (via `cross-rs`) on x86_64 runners, so no additional runner costs beyond the standard Linux minutes.

## Starter Implementation Guide

### Week 1 Tasks (from plan.md)

1. `cargo new rskim --bin`
2. Add tree-sitter dependencies
3. Basic CLI structure with clap
4. Parse simple TypeScript file
5. Create test fixtures
6. Validate AST access works

**NOTE:** All phases complete (100%). Phase 3 features (multi-file glob, caching, parallel processing, token counting) are fully implemented and tested with 2,482 tests passing. See README.md for full usage guide.

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

**Phase 1 Gate:** Can `rskim file.ts --mode=structure | head` and get reasonable output

**v1.0 Gate:**
- [x] All tests pass (400/400)
- [x] Benchmarks meet <50ms target (14.6ms for 3000-line files - 3x faster than target)
- [x] 17 languages supported (TypeScript, JavaScript, Python, Rust, Go, Java, C, C++, C#, Ruby, SQL, Kotlin, Swift, Markdown, JSON, YAML, TOML)
- [x] `cargo install rskim` works
- [x] Documentation complete
- [x] CI green on all platforms
- [x] npm distribution works (`npm install -g rskim`)

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
