# Development

This guide helps you contribute to Skim, add new features, or extend it for your needs.

## Prerequisites

- **Rust**: 1.70+ (install from [rustup.rs](https://rustup.rs/))
- **Git**: For version control
- **Optional**: [hyperfine](https://github.com/sharkdp/hyperfine) for benchmarking

## Project Setup

### Clone the Repository

```bash
git clone https://github.com/dean0x/skim.git
cd skim
```

### Build from Source

```bash
# Debug build (faster compilation, slower runtime)
cargo build

# Release build (optimized)
cargo build --release

# Binary location
./target/release/skim
```

### Install Locally

```bash
# Install from local source
cargo install --path crates/rskim

# Verify installation
skim --version
```

## Testing

### Run All Tests

```bash
# Run all tests
cargo test --all-features

# Run with output
cargo test --all-features -- --nocapture

# Run specific test
cargo test test_typescript_structure
```

**Test count:** 400 tests covering:
- Language parsing (TypeScript, JavaScript, Python, Rust, Go, Java, C, C++, Markdown, JSON, YAML, TOML)
- Transformation modes (structure, signatures, types, full)
- CLI features (stdin, multi-file, glob, directory, caching)
- Error handling (invalid files, unsupported languages, etc.)

### Test Organization

```
tests/
├── cli_basic.rs           # Single-file CLI tests
├── cli_glob.rs            # Glob pattern tests
├── cli_directory.rs       # Directory processing tests
├── fixtures/              # Test files
│   ├── typescript/
│   │   ├── simple.ts
│   │   ├── class.ts
│   │   ├── async.ts
│   │   └── generics.ts
│   ├── python/
│   ├── rust/
│   └── ...
└── integration/           # Integration tests (future)
```

### Writing Tests

**Unit test example (transformation logic):**

```rust
#[test]
fn test_structure_mode_typescript() {
    let input = r#"
        export function greet(name: string): string {
            return `Hello, ${name}!`;
        }
    "#;

    let output = transform(input, Language::TypeScript, Mode::Structure).unwrap();

    assert!(output.contains("export function greet(name: string): string"));
    assert!(output.contains("/* ... */"));
    assert!(!output.contains("return"));
}
```

**CLI test example:**

```rust
#[test]
fn test_cli_basic_file() {
    let temp = TempDir::new().unwrap();
    let file = temp.path().join("test.ts");
    fs::write(&file, "function foo() { return 42; }").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_skim"))
        .arg(&file)
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("function foo()"));
    assert!(stdout.contains("/* ... */"));
}
```

## Linting and Formatting

### Run Clippy (Linter)

```bash
# Check for lint warnings
cargo clippy -- -D warnings

# Auto-fix some issues
cargo clippy --fix
```

**Clippy configuration** (`.cargo/config.toml`):
```toml
[clippy]
# Strict linting for high code quality
pedantic = true
```

### Format Code

```bash
# Check formatting
cargo fmt -- --check

# Auto-format all code
cargo fmt
```

**Formatting rules** (`rustfmt.toml`):
```toml
edition = "2021"
max_width = 100
use_small_heuristics = "Max"
```

## Benchmarking

### Run Criterion Benchmarks

```bash
# Run all benchmarks
cargo bench

# Run specific benchmark
cargo bench typescript

# Generate report
cargo bench -- --save-baseline main
```

**Benchmark output:**
```
typescript_small        time:   [32.8 µs 33.2 µs 33.7 µs]
typescript_medium       time:   [82.1 µs 83.4 µs 84.9 µs]
typescript_large        time:   [4.78 ms 4.84 ms 4.91 ms]
```

### CLI Performance Testing

```bash
# Install hyperfine
cargo install hyperfine

# Benchmark CLI
hyperfine 'skim file.ts' 'skim file.ts --mode signatures'

# Compare with other tools
hyperfine 'cat file.ts' 'bat file.ts' 'skim file.ts'
```

## Adding New Languages

**Time estimate:** ~30 minutes per language

### Step-by-Step Guide

**1. Add tree-sitter grammar to `Cargo.toml`:**

```toml
[workspace.dependencies]
# ... existing dependencies ...
tree-sitter-kotlin = "0.3"  # Add new language
```

**2. Update `Language` enum in `crates/rskim-core/src/types.rs`:**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    TypeScript,
    JavaScript,
    Python,
    Rust,
    Go,
    Java,
    Markdown,
    Kotlin,  // ← Add new variant
}
```

**3. Add tree-sitter mapping in `to_tree_sitter()` method:**

```rust
impl Language {
    pub fn to_tree_sitter(&self) -> tree_sitter::Language {
        match self {
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            // ... other languages ...
            Language::Kotlin => tree_sitter_kotlin::LANGUAGE.into(),  // ← Add mapping
        }
    }
}
```

**4. Add file extension detection in `from_extension()` method:**

```rust
impl Language {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "ts" | "tsx" => Some(Language::TypeScript),
            "js" | "jsx" => Some(Language::JavaScript),
            // ... other extensions ...
            "kt" | "kts" => Some(Language::Kotlin),  // ← Add extensions
            _ => None,
        }
    }
}
```

**5. Add test fixtures in `tests/fixtures/kotlin/`:**

```
tests/fixtures/kotlin/
├── simple.kt        # Basic function
├── class.kt         # Class with methods
├── data_class.kt    # Data classes
└── coroutines.kt    # Async/suspend functions
```

**Example fixture (`simple.kt`):**
```kotlin
fun greet(name: String): String {
    return "Hello, $name!"
}

class Calculator {
    fun add(a: Int, b: Int): Int {
        return a + b
    }
}
```

**6. Add tests in `tests/cli_basic.rs`:**

```rust
#[test]
fn test_kotlin_structure() {
    let temp = TempDir::new().unwrap();
    let file = temp.path().join("test.kt");
    fs::write(&file, include_str!("fixtures/kotlin/simple.kt")).unwrap();

    let output = run_skim(&file, &["--mode", "structure"]).unwrap();

    assert!(output.contains("fun greet(name: String): String"));
    assert!(output.contains("/* ... */"));
}
```

**7. Update documentation:**

- [ ] Add language to supported languages table in README
- [ ] Add example to `docs/examples.md`
- [ ] Update `docs/modes.md` with language-specific notes

**8. Test and verify:**

```bash
cargo test --all-features
cargo clippy -- -D warnings
cargo fmt -- --check
```

### Language-Specific Considerations

**C-like languages (C, C++, C#):**
- Easy to add (similar structure to existing languages)
- Good tree-sitter grammar support

**Functional languages (Haskell, OCaml, Elixir):**
- May need custom transformation logic
- Type definitions work differently

**Markup languages (HTML, XML, JSON):**
- Structure mode should extract tags/keys
- Consider separate transformation logic

**Scripting languages (Ruby, PHP, Lua):**
- Dynamic typing (less type information to extract)
- Focus on function signatures

## Project Structure

```
skim/
├── crates/
│   ├── rskim-core/              # Core library (pure logic)
│   │   ├── src/
│   │   │   ├── lib.rs           # Public API
│   │   │   ├── transformer.rs   # AST transformation logic
│   │   │   ├── types.rs         # Language/Mode enums
│   │   │   └── tokens.rs        # Token counting (tiktoken)
│   │   ├── Cargo.toml
│   │   └── README.md
│   │
│   └── rskim/                   # CLI binary (I/O layer)
│       ├── src/
│       │   └── main.rs          # CLI, file I/O, caching, multi-file
│       ├── Cargo.toml
│       └── README.md
│
├── tests/
│   ├── cli_basic.rs             # Single-file CLI tests
│   ├── cli_glob.rs              # Glob pattern tests
│   ├── cli_directory.rs         # Directory processing tests
│   └── fixtures/                # Test files for each language
│
├── benches/
│   └── benchmarks.rs            # Criterion benchmarks
│
├── docs/                        # Documentation
│   ├── usage.md
│   ├── modes.md
│   ├── examples.md
│   ├── use-cases.md
│   ├── caching.md
│   ├── security.md
│   ├── architecture.md
│   ├── performance.md
│   └── development.md
│
├── .github/
│   └── workflows/
│       └── release.yml          # CI/CD (cargo-dist)
│
├── Cargo.toml                   # Workspace configuration
├── README.md                    # Main documentation
├── CHANGELOG.md                 # Version history
├── SECURITY.md                  # Security policy
├── LICENSE                      # MIT License
└── CLAUDE.md                    # AI assistant instructions
```

### Crate Separation

**Why two crates?**

1. **rskim-core** (library):
   - Pure transformation logic
   - No file I/O
   - No CLI dependencies
   - Can be embedded in other Rust projects

2. **rskim** (binary):
   - CLI interface (clap)
   - File I/O (reading, writing, glob, directory)
   - Caching layer
   - Parallel processing (rayon)
   - Depends on rskim-core

**Benefits:**
- Core library is testable without I/O mocks
- Clear separation of concerns
- Can be used as library in other tools

## Code Style Guidelines

### Naming Conventions

**Types/Structs/Enums:** `PascalCase`
```rust
pub enum Language { ... }
pub struct TransformConfig { ... }
```

**Functions/Variables:** `snake_case`
```rust
pub fn transform_auto(source: &str, path: &Path, mode: Mode) -> Result<String>
let cache_key = calculate_cache_key(path, mtime, mode);
```

**Constants:** `SCREAMING_SNAKE_CASE`
```rust
const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;  // 50MB
const MAX_RECURSION_DEPTH: usize = 500;
```

### Error Handling

**Use `Result<T, E>` - never panic in library code:**

```rust
// ✅ GOOD - Explicit error handling
pub fn parse_file(path: &Path) -> Result<String, Error> {
    let contents = fs::read_to_string(path)?;
    Ok(contents)
}

// ❌ BAD - Panics on error
pub fn parse_file(path: &Path) -> String {
    fs::read_to_string(path).unwrap()  // DON'T DO THIS
}
```

**Custom error types:**

```rust
#[derive(Debug, thiserror::Error)]
pub enum TransformError {
    #[error("Failed to parse {language} code")]
    ParseError { language: Language },

    #[error("Unsupported language: {0}")]
    UnsupportedLanguage(String),
}
```

### Documentation

**Public APIs must have doc comments:**

```rust
/// Transform source code by extracting structure, signatures, or types.
///
/// # Arguments
///
/// * `source` - The source code to transform
/// * `language` - The programming language
/// * `mode` - Transformation mode (structure, signatures, types, or full)
///
/// # Returns
///
/// The transformed code as a `String`, or an error if parsing fails.
///
/// # Examples
///
/// ```
/// use rskim_core::{transform, Language, Mode};
///
/// let source = "function foo() { return 42; }";
/// let result = transform(source, Language::JavaScript, Mode::Structure)?;
/// assert!(result.contains("/* ... */"));
/// ```
pub fn transform(source: &str, language: Language, mode: Mode) -> Result<String, TransformError> {
    // ...
}
```

### Performance Guidelines

**1. Use `&str` over `String` when possible:**

```rust
// ✅ GOOD - Borrows
pub fn process(source: &str) -> &str

// ❌ BAD - Takes ownership unnecessarily
pub fn process(source: String) -> String
```

**2. Avoid allocations in hot paths:**

```rust
// ✅ GOOD - Reuses buffer
let mut output = String::with_capacity(source.len());
for node in nodes {
    output.push_str(node.text());
}

// ❌ BAD - Many allocations
let output = nodes.iter()
    .map(|n| n.text().to_string())
    .collect::<Vec<_>>()
    .join("");
```

**3. Use `?` operator for error propagation:**

```rust
// ✅ GOOD - Idiomatic
pub fn process(path: &Path) -> Result<String> {
    let contents = fs::read_to_string(path)?;
    let transformed = transform(&contents)?;
    Ok(transformed)
}

// ❌ BAD - Verbose
pub fn process(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(contents) => match transform(&contents) {
            Ok(transformed) => Ok(transformed),
            Err(e) => Err(e),
        },
        Err(e) => Err(e.into()),
    }
}
```

## Debugging

### Enable Logging

```bash
# Set log level
RUST_LOG=debug skim file.ts

# Very verbose
RUST_LOG=trace skim file.ts
```

**Add logging to code:**

```rust
use log::{debug, info, warn, error};

pub fn transform(source: &str, language: Language) -> Result<String> {
    debug!("Transforming {} code ({} bytes)", language, source.len());

    let tree = parse(source)?;
    info!("Parsed successfully, {} nodes", tree.root_node().child_count());

    // ...
}
```

### Debugging with rust-lldb (macOS/Linux)

```bash
# Build with debug symbols
cargo build

# Debug with lldb
rust-lldb ./target/debug/skim -- file.ts

# Set breakpoints
(lldb) breakpoint set --name transform
(lldb) run
```

### Debugging with rust-gdb (Linux)

```bash
cargo build
rust-gdb ./target/debug/skim -- file.ts

# Set breakpoints
(gdb) break transform
(gdb) run
```

## Contributing

We welcome contributions! Here's how to get started:

### 1. Check Existing Issues

Browse [issues](https://github.com/dean0x/skim/issues) to find something to work on, or open a new issue to discuss your idea.

### 2. Fork and Clone

```bash
# Fork on GitHub, then clone your fork
git clone https://github.com/YOUR_USERNAME/skim.git
cd skim
```

### 3. Create a Branch

```bash
git checkout -b feature/my-new-feature
```

### 4. Make Changes

- Write code following style guidelines
- Add tests for new features
- Update documentation
- Run tests and linters

### 5. Commit and Push

```bash
git add .
git commit -m "Add feature: my new feature"
git push origin feature/my-new-feature
```

### 6. Open Pull Request

- Go to GitHub and create a pull request
- Describe your changes clearly
- Link related issues

### Pull Request Checklist

- [ ] All tests pass (`cargo test --all-features`)
- [ ] No clippy warnings (`cargo clippy -- -D warnings`)
- [ ] Code is formatted (`cargo fmt -- --check`)
- [ ] Documentation is updated
- [ ] CHANGELOG.md is updated (if applicable)
- [ ] New features have tests
- [ ] Commit messages are clear

## Release Process

**For maintainers only.**

### 1. Update Version

Update version in `Cargo.toml`:

```toml
[package]
name = "rskim"
version = "0.6.0"  # ← Bump version
```

### 2. Update CHANGELOG.md

Add release notes:

```markdown
## [0.6.0] - 2024-01-15

### Added
- New feature X
- New language Y support

### Fixed
- Bug Z

### Changed
- Improved performance by 20%
```

### 3. Commit and Tag

```bash
git add Cargo.toml CHANGELOG.md
git commit -m "Release v0.6.0"
git tag v0.6.0
git push origin main --tags
```

### 4. Automated Release (cargo-dist)

GitHub Actions automatically:
- Builds for all platforms (Linux, macOS, Windows)
- Creates GitHub release with binaries
- Publishes to crates.io
- Publishes to npm

### 5. Verify Release

```bash
# Check crates.io
cargo search rskim

# Check npm
npm info rskim

# Test installation
cargo install rskim
npm install -g rskim
```

## Getting Help

- **Documentation**: Read docs in `/docs` folder
- **Issues**: [GitHub Issues](https://github.com/dean0x/skim/issues)
- **Discussions**: [GitHub Discussions](https://github.com/dean0x/skim/discussions)

## Resources

- **Rust Book**: https://doc.rust-lang.org/book/
- **tree-sitter**: https://tree-sitter.github.io/tree-sitter/
- **clap**: https://docs.rs/clap/
- **Criterion**: https://bheisler.github.io/criterion.rs/

---

Happy hacking! 🦀
