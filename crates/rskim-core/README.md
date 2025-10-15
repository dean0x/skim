# rskim-core

Core library for smart code reading and transformation.

[![Crates.io](https://img.shields.io/crates/v/rskim-core.svg)](https://crates.io/crates/rskim-core)
[![Documentation](https://docs.rs/rskim-core/badge.svg)](https://docs.rs/rskim-core)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## Overview

`rskim-core` is a Rust library that transforms source code by intelligently removing implementation details while preserving structure, signatures, and types. Perfect for optimizing code for LLM context windows.

## Features

- **6 Languages**: TypeScript, JavaScript, Python, Rust, Go, Java
- **4 Transformation Modes**: Structure, Signatures, Types, Full
- **Fast**: <50ms for 1000-line files
- **Safe**: Built-in DoS protections and memory limits
- **Zero-copy**: Efficient string slicing where possible

## Installation

```toml
[dependencies]
rskim-core = "0.2"
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

### Structure Mode (70-80% reduction)
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
| TypeScript | `.ts`, `.tsx` | Full support |
| JavaScript | `.js`, `.jsx`, `.mjs` | Full support |
| Python | `.py` | Full support |
| Rust | `.rs` | Full support |
| Go | `.go` | Full support |
| Java | `.java` | Full support |

## Security

Built-in protections against:
- **Stack overflow**: Max recursion depth (500)
- **Memory exhaustion**: Max input size (50MB), max AST nodes (100k)
- **UTF-8 violations**: Boundary validation before string slicing
- **Path traversal**: Rejects `..` in file paths

## Performance

- **Parse + Transform**: <50ms for 1000-line files
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
