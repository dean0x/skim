# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Test coverage improvements (in progress)
- Benchmark suite (planned)
- Multi-file glob support (planned)
- Parser caching (planned)

## [0.1.0] - 2025-10-06

### Added
- Initial release
- Multi-language support: TypeScript, JavaScript, Python, Rust, Go, Java
- Four transformation modes: structure, signatures, types, full
- CLI with stdin support
- Language auto-detection from file extensions
- DoS protection (stack overflow, memory exhaustion, UTF-8 panics)
- Path traversal protection
- Comprehensive error handling with clear messages

### Security
- Maximum recursion depth limit (500 levels) to prevent stack overflow
- Maximum input size limit (50MB) to prevent memory exhaustion
- Maximum AST node limit (100,000 nodes) to prevent memory exhaustion
- UTF-8 boundary validation to prevent panics on multi-byte Unicode
- Path component validation to prevent traversal attacks

### Fixed
- tree-sitter version compatibility (pinned to 0.23.x)
- Removed duplicate parser implementation
- Removed duplicate language mapping logic

### Technical
- Zero-copy string operations where possible
- Streaming stdout output with buffering
- Error-tolerant parsing (handles incomplete/broken code)
- No panics in library code (enforced by clippy lints)

## [0.0.1] - 2025-10-05

### Added
- Initial project structure
- TypeScript parsing proof of concept
- Basic structure transformation
- CLI scaffolding with clap

---

## Version History

- **0.1.0** (2025-10-06): First public release with security hardening
- **0.0.1** (2025-10-05): Initial prototype

## Links

- [Repository](https://github.com/dean0x/skim)
- [Issues](https://github.com/dean0x/skim/issues)
- [Security Policy](SECURITY.md)
