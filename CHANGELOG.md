# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - 2025-10-17

### Added
- **Multi-file Glob Support** - Process multiple files with wildcard patterns
  - Glob pattern matching: `skim 'src/**/*.ts'`, `skim '*.{js,ts}'`
  - File header separators for multi-file output
  - `--no-header` flag to disable headers in multi-file mode
  - Recursive directory traversal with glob patterns

- **Parallel Processing** - Rayon-powered multi-core processing
  - `--jobs` flag for configurable parallelism (default: number of CPUs)
  - 2.4x speedup demonstrated with `--jobs 4`
  - Efficient thread pool management
  - Scales linearly with CPU cores

- **File-based Caching** - Massive speedup on repeated processing
  - **Enabled by default** for 40-50x speedup on cached reads
  - SHA256 cache keys with mtime-based invalidation
  - Platform-specific cache directory (`~/.cache/skim/`)
  - `--no-cache` flag to disable caching
  - `--clear-cache` command to clear cache directory
  - Smart invalidation on file modification

- **Token Counting** - Measure LLM context window savings
  - `--show-stats` flag shows token reduction statistics
  - Uses tiktoken with cl100k_base encoding (GPT-3.5/GPT-4 compatible)
  - Works with single files, globs, and stdin
  - Aggregates stats across multiple files
  - Output to stderr for clean piping

### Performance
- **Verified benchmarks**: 14.6ms for 3000-line files (3x faster than 50ms target)
- **Cached reads**: 5ms average (40-50x speedup)
- **Parallel processing**: 2.4x speedup with 4 cores
- **Token reduction**: 60-95% depending on mode

### Internal
- New module: `crates/rskim/src/cache.rs` - Caching implementation
- New module: `crates/rskim/src/tokens.rs` - Token counting with tiktoken
- Major refactor: `crates/rskim/src/main.rs` - Integrated all Phase 3 features
- Architecture cleanup: Removed unused exports, clarified core/CLI boundaries
- Dependencies added: glob, rayon, dirs, serde, serde_json, sha2, tiktoken-rs

### Documentation
- Updated all READMEs with Phase 3 features
- Updated CLAUDE.md to reflect 100% completion (70 tests passing)
- Updated CONTRIBUTING.md with accurate crate names and performance targets
- Fixed benchmark imports for consistency

### Testing
- **70 total tests** - All passing (8 + 19 + 11 + 24 + 8)
- Verified parallel processing with CPU usage tests
- Verified caching with repeated file processing
- Verified token counting accuracy

### Breaking Changes
None. All new features are opt-in via CLI flags.

## [0.3.3] - 2025-10-16

### Fixed
- **CLI README (crates.io)** - Critical branding and command errors
  - Title changed from "# rskim" to "# Skim" (official brand name)
  - Overview text changed from "rskim transforms..." to "**Skim** transforms..."
  - Fixed broken npx commands: `npx skim file.ts` → `npx rskim file.ts` (2 occurrences)

**Context**: The CLI README is displayed on crates.io and was showing incorrect branding and broken commands that would not work.

**Important distinction:**
- **Brand name**: Skim (official name)
- **Package name**: rskim (for `npm install -g rskim`, `npx rskim`, `cargo install rskim`)
- **Binary name**: skim (after installation: `skim file.ts`)

## [0.3.2] - 2025-10-16

### Fixed
- **Main README** - Project status showed outdated version (v0.2.3 → v0.3.1)
- **Main README** - Planned features example still used old binary name (`rskim` → `skim`)
- **Core library README** - Dependency version example showed `"0.2"` instead of `"0.3"`
- **Core library** - Doc tests and integration tests used wrong crate name (`skim_core` → `rskim_core`)
  - Affected files: `lib.rs`, `types.rs`, `integration.rs`, `transform/mod.rs`
  - All doc examples now use correct `rskim_core` import
  - Fixed unused import warning in transform module

**Context**: Documentation and naming issues discovered after v0.3.1 release. The `skim_core` references were remnants from original project naming before the v0.2.1 rename to `rskim`.

## [0.3.1] - 2025-10-16

### Fixed
- CLI README documentation still referenced old language names (`type-script`, `java-script`)
- Test files using incorrect language flag format (should be `typescript`, not `type-script`)
- Test version assertion updated to match current version (0.3.0 → 0.3.1)

**Context**: These issues were overlooked in v0.3.0 release. Language names were changed to lowercase in v0.2.4, but some documentation and test references weren't updated.

## [0.3.0] - 2025-10-16

### Changed
- **BREAKING:** Binary name changed from `rskim` to `skim`
  - Installation still uses `rskim`: `npm install -g rskim` or `cargo install rskim`
  - Command usage now uses `skim`: `skim file.ts` (shorter, cleaner)
  - Official branded name: **Skim**
  - Package name remains `rskim` to avoid conflicts

### Migration
```bash
# Installation (unchanged)
npm install -g rskim
cargo install rskim

# Old command (v0.2.x)
rskim file.ts

# New command (v0.3.0+)
skim file.ts
```

**Rationale**: Shorter command for daily use. Package name `rskim` avoids npm/crates.io namespace conflicts.

## [0.2.4] - 2025-10-16

### Fixed
- **BREAKING:** Language flag names now use lowercase instead of kebab-case
  - `--language=type-script` → `--language=typescript`
  - `--language=java-script` → `--language=javascript`
  - Short aliases still work: `--lang=ts`, `--lang=js`
- All README files updated to reflect current state (npm live, correct package names)
- CHANGELOG now includes all historical versions (0.2.1, 0.2.2, 0.2.3)
- Error message fixed: `skim` → `rskim`

### Changed
- Installation documentation now recommends `npx` for trial usage
- Clarified npx performance trade-offs (~100-500ms overhead per invocation)

## [0.2.3] - 2025-10-15

### Fixed
- npm wrapper script syntax error (template literal escaping)
- Binary now works correctly when installed via npm

## [0.2.2] - 2025-10-15

### Added
- npm distribution via GitHub Actions
- Automated cross-platform binary building (Linux, macOS x64/ARM, Windows)
- npm package published as `rskim`

### Fixed
- GitHub Actions workflow for npm publishing

## [0.2.1] - 2025-10-15

### Changed
- **BREAKING:** Renamed all packages to `rskim` for consistency
  - `skim-core` → `rskim-core`
  - `skim-cli` → `rskim` (binary also renamed)
  - Updated repository URLs to https://github.com/dean0x/skim
- Simplified distribution strategy: native CLI only (removed WASM)
- Configured cargo-dist for npm distribution as `rskim`

### Migration Guide
```bash
# Old (v0.1.0)
cargo install skim-cli

# New (v0.2.0+)
cargo install rskim

# Or via npm
npm install -g rskim
npx rskim file.ts  # no install required
```

## [0.1.0] - 2025-10-15

### Added
- 🎉 **Initial release** - Production-ready CLI tool

**Core Features:**
- Multi-language support: TypeScript, JavaScript, Python, Rust, Go, Java
- Four transformation modes: structure (70-80%), signatures (85-92%), types (90-95%), full (0%)
- CLI with stdin support and language auto-detection
- UTF-8/Unicode support (emoji, Chinese, multi-byte characters)
- Streaming output to stdout for pipe workflows

**Testing:**
- 62 total tests (11 unit, 24 integration, 19 CLI, 8 doc tests)
- 100% test pass rate
- Performance benchmarking suite with criterion
- Real-world testing on complex codebases

**Security:**
- Stack overflow protection (max recursion depth: 500)
- Memory exhaustion protection (max input: 50MB, max nodes: 100k)
- UTF-8 boundary validation (prevents panics)
- Path traversal protection (rejects `..` components)
- DoS-resistant with comprehensive input validation

**Developer Experience:**
- Comprehensive error messages
- Help and version flags
- Language detection with file extensions
- Explicit language override with `--language` flag

### Fixed
- Overlapping replacements bug in structure mode (nested functions)
- Path traversal validation (now allows absolute paths correctly)
- tree-sitter version compatibility (pinned to 0.23.x)
- Removed duplicate parser implementation
- Cleaned up unused code warnings

### Technical
- Zero-copy string operations where possible
- Streaming stdout output with buffering
- Error-tolerant parsing (handles incomplete/broken code gracefully)
- No panics in library code (enforced by clippy lints)
- Clean builds with comprehensive test coverage

---

## Roadmap

### v0.5.0 (Future)
- **Streaming API** - Process large files incrementally
- **Custom Modes** - User-defined transformation rules via config
- **Watch Mode** - Auto-transform on file changes
- **Language Server** - LSP integration for editor plugins

---

## Version History

- **0.4.0** (2025-10-17): Multi-file glob support, caching, parallel processing, token counting (Phase 3 complete)
- **0.3.3** (2025-10-16): CLI README branding and broken npx command fixes
- **0.3.2** (2025-10-16): README documentation alignment fixes
- **0.3.1** (2025-10-16): Hotfix for remaining language name references in docs/tests
- **0.3.0** (2025-10-16): Binary renamed to `skim`, package remains `rskim`
- **0.2.4** (2025-10-16): Fixed language flag names, updated all documentation
- **0.2.3** (2025-10-15): Fixed npm wrapper script syntax
- **0.2.2** (2025-10-15): npm distribution via GitHub Actions
- **0.2.1** (2025-10-15): Renamed package to rskim with comprehensive documentation
- **0.1.0** (2025-10-15): Initial release as skim-cli

---

## Links

- [Repository](https://github.com/dean0x/skim)
- [Issues](https://github.com/dean0x/skim/issues)
- [Security Policy](SECURITY.md)
- [Contributing Guide](CONTRIBUTING.md)
