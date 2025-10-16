# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Planned for Future Versions
- Multi-file glob support (`skim src/**/*.ts`)
- Parser caching (mtime-based)
- Parallel processing with rayon

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

### v0.4.0 (Future)
- **Multi-file Support** - Glob patterns (`skim src/**/*.ts`)
- **Performance** - Parser caching and parallel processing with rayon
- **Streaming API** - Process large files incrementally
- **Custom Modes** - User-defined transformation rules

---

## Version History

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
