# rskim-core

Core library for smart code reading and transformation.

[![Crates.io](https://img.shields.io/crates/v/rskim-core.svg)](https://crates.io/crates/rskim-core)
[![Documentation](https://docs.rs/rskim-core/badge.svg)](https://docs.rs/rskim-core)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## Overview

`rskim-core` is a Rust library that transforms source code by intelligently removing implementation details while preserving structure, signatures, and types. Perfect for optimizing code for LLM context windows.

## Features

- **18 Languages**: TypeScript, JavaScript, Python, Rust, Go, Java, C, C++, C#, Ruby, SQL, Kotlin, Swift, Bash, Markdown, JSON, YAML, TOML
- **6 Transformation Modes**: Structure, Signatures, Types, Full, Minimal, Pseudo
- **Fast**: 14.6ms for 3000-line files (verified benchmarks)
- **Safe**: Built-in DoS protections and memory limits
- **Zero-copy**: Efficient string slicing where possible
- **Pure Library**: No I/O - accepts `&str`, returns `Result<String>`

## Installation

```toml
[dependencies]
rskim-core = "1.0"
```

## Usage

```rust
use rskim_core::{transform, Language, Mode};

fn main() {
    let source = r#"
function add(a: number, b: number): number {
    return a + b;
}
    "#;

    let result = transform(source, Language::TypeScript, Mode::Structure)
        .expect("Transformation failed");

    println!("{}", result);
    // Output: function add(a: number, b: number): number { /* ... */ }
}
```

## Transformation Modes

Reduction figures below are targets, not gates. Structure mode's only measured
value is 60.3%, on the production TypeScript codebase in the repository
README's reduction tables, and the range is stated wide enough to contain it.
No CI gate defends any of these ranges — the only reduction ratio the test
suite asserts is `> 0.30`, on the JSON and YAML structure-mode fixtures.

### Structure Mode (60-80% reduction)
Removes function bodies while preserving signatures and structure.

```rust
let result = transform(code, Language::TypeScript, Mode::Structure)?;
```

### Signatures Mode (85-92% reduction)
Extracts only function and method signatures.

```rust
let result = transform(code, Language::Python, Mode::Signatures)?;
```

### Types Mode (90-95% reduction)
Extracts only type definitions (interfaces, enums, structs, etc.).

```rust
let result = transform(code, Language::Rust, Mode::Types)?;
```

### Full Mode (0% reduction)
Returns the original code unchanged.

```rust
let result = transform(code, Language::Java, Mode::Full)?;
```

### Minimal Mode (reduction unverified)
Strips non-doc comments at module and class level while keeping all code intact.
Doc comments, comments inside function bodies, module header comments, and
shebangs are preserved.

```rust
let result = transform(code, Language::Python, Mode::Minimal)?;
```

### Pseudo Mode (reduction unverified)
Strips syntactic noise — type annotations, decorators, semicolons — while
preserving logic flow, names, values, visibility modifiers, and function return
types. What is stripped varies by language: TypeScript and Rust keep parameter
types, and Rust removes only statement semicolons and non-doc comments.

```rust
let result = transform(code, Language::TypeScript, Mode::Pseudo)?;
```

## Auto-Detection

Use `transform_auto` for automatic language detection from file paths:

```rust
use rskim_core::transform_auto;
use std::path::Path;

let result = transform_auto(
    source,
    Path::new("example.ts"),
    Mode::Structure
)?;
```

## Supported Languages

| Language | Extensions | Node Types |
|----------|-----------|------------|
| TypeScript | `.ts`, `.tsx`, `.mts`, `.cts` | Full support |
| JavaScript | `.js`, `.jsx`, `.cjs`, `.mjs` | Full support |
| Python | `.py`, `.pyi` | Full support |
| Rust | `.rs` | Full support |
| Go | `.go` | Full support |
| Java | `.java` | Full support |
| C | `.c`, `.h` | Full support |
| C++ | `.cpp`, `.hpp`, `.cc`, `.hh`, `.cxx`, `.hxx` | Full support |
| C# | `.cs` | Full support |
| Ruby | `.rb` | Full support |
| SQL | `.sql` | Full support |
| Kotlin | `.kt`, `.kts` | Full support |
| Swift | `.swift` | Full support |
| Bash | `.sh`, `.bash` | Full support |
| Markdown | `.md`, `.markdown` | Full support |
| JSON | `.json` | Full support |
| YAML | `.yaml`, `.yml` | Full support |
| TOML | `.toml` | Full support |

## Security

Built-in protections against:
- **Stack overflow**: Max recursion depth (500)
- **Memory exhaustion**: Max input size (50MB), max AST nodes (100k)
- **UTF-8 violations**: Boundary validation before string slicing
- **Path traversal**: Rejects `..` in file paths

## Performance

- **Parse + Transform**: 14.6ms for 3000-line files (verified)
- **Token Reduction**: 60-95% depending on mode
- **Zero Allocations**: Uses `&str` slices where possible

## Error Handling

All functions return `Result<String, SkimError>`:

```rust
use rskim_core::{transform, SkimError};

match transform(source, Language::TypeScript, Mode::Structure) {
    Ok(result) => println!("{}", result),
    Err(SkimError::ParseError(msg)) => eprintln!("Parse error: {}", msg),
    Err(SkimError::UnsupportedLanguage(ext)) => eprintln!("Unsupported: {}", ext),
    Err(e) => eprintln!("Error: {}", e),
}
```

## CLI Tool

For command-line usage, see the [`rskim`](https://crates.io/crates/rskim) binary crate.

## Links

- [Documentation](https://docs.rs/rskim-core)
- [Repository](https://github.com/dean0x/skim)
- [CLI Tool](https://crates.io/crates/rskim)

## License

MIT
