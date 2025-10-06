# Contributing to Skim

Thank you for your interest in contributing to Skim! We welcome contributions from the community.

## Code of Conduct

Be respectful, constructive, and professional. We're here to build great tools together.

## How to Contribute

### Reporting Bugs

Before creating a bug report:
1. Check [existing issues](https://github.com/dean0x/skim/issues) to avoid duplicates
2. Verify you're using the latest version
3. Test with minimal example to isolate the issue

**Good bug report includes:**
- Skim version (`skim --version`)
- Operating system and architecture
- Minimal code example that reproduces the issue
- Expected vs actual behavior
- Error messages (if any)

### Suggesting Features

Before suggesting a feature:
1. Check if it aligns with project scope (see CLAUDE.md for design constraints)
2. Search existing issues for similar requests
3. Consider if it can be implemented as a separate tool

**Good feature request includes:**
- Clear use case and motivation
- Example of how it would work
- Consideration of edge cases
- Willingness to contribute implementation

### Asking Questions

- Use [GitHub Discussions](https://github.com/dean0x/skim/discussions) for questions
- Use Issues only for bugs and feature requests
- Check existing discussions first

## Development Setup

### Prerequisites

- Rust 1.70+ (`rustup update`)
- Git
- Optional: `cargo-watch` for auto-rebuild during development

### Clone and Build

```bash
git clone https://github.com/dean0x/skim.git
cd skim
cargo build
cargo test
```

### Project Structure

```
skim/
├── crates/
│   ├── skim-core/          # Core library (no I/O, pure transforms)
│   │   ├── src/
│   │   │   ├── lib.rs      # Public API
│   │   │   ├── types.rs    # Core types (Language, Mode, etc.)
│   │   │   ├── parser/     # tree-sitter wrapper
│   │   │   └── transform/  # Transformation logic
│   │   └── tests/          # Integration tests
│   └── skim-cli/           # CLI binary (I/O layer)
│       └── src/main.rs     # Argument parsing, file I/O
├── tests/fixtures/         # Test files for each language
├── benches/                # Benchmarks (planned)
├── .docs/                  # Internal documentation
└── CLAUDE.md               # AI assistant instructions
```

**Design principles:**
- `skim-core`: No I/O, no CLI dependencies, pure library
- `skim-cli`: Thin I/O wrapper, delegates to core
- All business logic in core, tested there

### Development Workflow

```bash
# Watch mode (auto-rebuild on changes)
cargo watch -x build -x test

# Run specific test
cargo test test_name

# Run with debug output
RUST_LOG=debug cargo run -- file.ts

# Format code
cargo fmt

# Lint
cargo clippy -- -D warnings

# Check before commit
cargo fmt --check && cargo clippy -- -D warnings && cargo test
```

## Making Changes

### 1. Create a Branch

```bash
git checkout -b feature/my-feature
# or
git checkout -b fix/my-bugfix
```

### 2. Write Code

**Follow existing patterns:**

- Use `Result<T, SkimError>` for error handling (no panics)
- Add doc comments for public APIs
- Keep functions focused and testable
- Prefer `&str` over `String` for zero-copy
- Update tests alongside code changes

**Lint requirements:**
```rust
// ✅ GOOD: Explicit error handling
let result = parse(source)?;

// ❌ BAD: Will fail clippy
let result = parse(source).unwrap();  // unwrap_used = deny
```

### 3. Write Tests

**Every change needs tests:**

```rust
#[test]
fn test_my_feature() {
    let source = "test code";
    let result = transform(source, Language::TypeScript, Mode::Structure);
    assert!(result.is_ok());
}
```

**Test organization:**
- Unit tests: `#[cfg(test)] mod tests` in same file
- Integration tests: `tests/` directory
- Fixtures: `tests/fixtures/<language>/`

### 4. Update Documentation

- Add/update doc comments for public APIs
- Update README.md if adding user-facing features
- Update CHANGELOG.md under `[Unreleased]`
- Add examples if introducing new functionality

### 5. Commit

**Follow existing commit style:**

```bash
git commit -m "Add signature extraction for Python decorators

- Parse decorator nodes in AST traversal
- Preserve @decorator syntax in output
- Add test fixtures for common decorator patterns

Fixes #123"
```

**Commit message format:**
```
<Action> <what> (<context>)

- Bullet points explaining changes
- Focus on WHY, not just WHAT
- Reference issue numbers if applicable
```

### 6. Push and Create PR

```bash
git push origin feature/my-feature
```

Then create a Pull Request on GitHub with:
- Clear description of what changed and why
- Link to related issues
- Screenshots/examples if UI/output changed
- Confirmation that tests pass

## Adding a New Language

Adding language support is straightforward:

### 1. Find tree-sitter Grammar

Check https://github.com/tree-sitter for `tree-sitter-<language>`

### 2. Add Dependency

```toml
# Cargo.toml
[workspace.dependencies]
tree-sitter-newlang = "0.23"  # Must be 0.23.x
```

```toml
# crates/skim-core/Cargo.toml
[dependencies]
tree-sitter-newlang = { workspace = true }
```

### 3. Update Language Enum

```rust
// crates/skim-core/src/types.rs
pub enum Language {
    // ... existing
    NewLang,
}

impl Language {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            // ... existing
            "newext" => Some(Self::NewLang),
            _ => None,
        }
    }

    pub fn to_tree_sitter(self) -> tree_sitter::Language {
        match self {
            // ... existing
            Self::NewLang => tree_sitter_newlang::LANGUAGE.into(),
        }
    }
}
```

### 4. Add Node Types

```rust
// crates/skim-core/src/transform/structure.rs
fn get_node_types_for_language(language: Language) -> NodeTypes {
    match language {
        // ... existing
        Language::NewLang => NodeTypes {
            function: "function_definition",  // Check grammar docs
            method: "method_definition",
        },
    }
}
```

### 5. Add Test Fixtures

```bash
mkdir -p tests/fixtures/newlang
```

Create `tests/fixtures/newlang/simple.newext`:
```
// Simple example with functions, classes, etc.
```

### 6. Add Tests

```rust
#[test]
fn test_parser_newlang() {
    let source = "function example() { }";
    let mut parser = Parser::new(Language::NewLang).unwrap();
    let result = parser.parse(source);
    assert!(result.is_ok());
}
```

### 7. Update Documentation

- Add to language table in README.md
- Update CHANGELOG.md
- Add examples in README if syntax differs significantly

**Total time: ~30 minutes** for straightforward languages.

## Testing Guidelines

### Test Coverage

Run tests with coverage (requires `cargo-tarpaulin`):

```bash
cargo install cargo-tarpaulin
cargo tarpaulin --out Html
```

**Coverage goals:**
- Core library: >80%
- Transform logic: >90%
- CLI: >60% (I/O makes 100% hard)

### Test Categories

1. **Unit tests**: Test individual functions
2. **Integration tests**: Test full transformation pipeline
3. **Fixture tests**: Test real-world code samples
4. **Snapshot tests**: Use `insta` for output comparison (planned)

### Writing Good Tests

```rust
// ✅ GOOD: Clear, focused test
#[test]
fn test_preserves_function_signature() {
    let source = "function add(a: number, b: number): number { return a + b; }";
    let result = transform(source, Language::TypeScript, Mode::Structure).unwrap();
    assert!(result.contains("function add(a: number, b: number): number"));
    assert!(result.contains("{ /* ... */ }"));
}

// ❌ BAD: Testing too many things
#[test]
fn test_everything() {
    // Tests 10 different features in one test
}
```

## Performance Guidelines

**Performance is a first-class concern.**

### Benchmarking

```bash
cargo bench  # (planned - benchmark suite not yet implemented)
```

**Performance targets:**
- Parse + transform: <50ms for 1000-line files
- Memory: <10MB for typical files
- Startup: <10ms

### Performance Best Practices

```rust
// ✅ GOOD: Zero-copy with &str
let text = node.utf8_text(source.as_bytes())?;

// ❌ BAD: Unnecessary allocation
let text = node.text().to_string();
```

```rust
// ✅ GOOD: Pre-allocate with capacity
let mut result = String::with_capacity(source.len());

// ❌ BAD: Let it grow
let mut result = String::new();
```

## Code Review Process

1. **Automated checks** must pass:
   - `cargo fmt --check`
   - `cargo clippy -- -D warnings`
   - `cargo test`
   - CI pipeline (GitHub Actions)

2. **Manual review** focuses on:
   - Correctness and safety
   - Performance implications
   - API design
   - Test coverage
   - Documentation quality

3. **Approval**: At least one maintainer approval required

4. **Merge**: Squash and merge (keep history clean)

## Release Process

(For maintainers)

1. Update version in `Cargo.toml` files
2. Update `CHANGELOG.md` (move Unreleased to new version)
3. Commit: `git commit -m "Release v0.2.0"`
4. Tag: `git tag v0.2.0`
5. Push: `git push --tags`
6. CI will automatically publish to crates.io (when configured)

## Need Help?

- **General questions**: [GitHub Discussions](https://github.com/dean0x/skim/discussions)
- **Bug reports**: [GitHub Issues](https://github.com/dean0x/skim/issues)
- **Security issues**: See [SECURITY.md](SECURITY.md)

## Recognition

Contributors are credited in:
- Git commit history
- Release notes in CHANGELOG.md
- Special thanks in README.md (for significant contributions)

---

**Thank you for contributing to Skim!** 🎉
