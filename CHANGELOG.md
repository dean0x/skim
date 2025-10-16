# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Planned for Future Versions
- Multi-file glob support (`rskim src/**/*.ts`)
- Parser caching (mtime-based)
- Parallel processing with rayon

## [0.2.0] - 2025-10-15

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

### v0.3.0 (Future)
- **Multi-file Support** - Glob patterns (`rskim src/**/*.ts`)
- **Performance** - Parser caching and parallel processing with rayon
- **Streaming API** - Process large files incrementally
- **Custom Modes** - User-defined transformation rules

---

## Version History

- **0.1.0** (2025-10-15): First public release - Production-ready CLI with 6 languages, 4 modes, 62 tests

---

## Links

- [Repository](https://github.com/dean0x/skim)
- [Issues](https://github.com/dean0x/skim/issues)
- [Security Policy](SECURITY.md)
- [Contributing Guide](CONTRIBUTING.md)
