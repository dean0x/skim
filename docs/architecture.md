# Architecture

Skim is built with a clean, streaming architecture that prioritizes performance, type safety, and maintainability.

## System Architecture

```
┌─────────────────┐
│  Language       │
│  Detection      │
└────────┬────────┘
         │
┌────────▼────────┐
│  tree-sitter    │
│  Parser         │
└────────┬────────┘
         │
┌────────▼────────┐
│  Transformation │
│  Layer          │
└────────┬────────┘
         │
┌────────▼────────┐
│  Streaming      │
│  Output         │
└─────────────────┘
```

## Design Principles

### 1. Streaming-First

Output to stdout, no intermediate files. Skim follows the Unix philosophy of composable tools:

```bash
# Good: Streams through pipeline
skim file.ts | bat -l typescript

# Not: Writes temporary files
skim file.ts > temp.txt && bat temp.txt
```

**Benefits:**
- Zero disk I/O (except cache)
- Composable with other CLI tools
- Low memory footprint (doesn't buffer entire output)

### 2. Zero-Copy String Operations

Uses `&str` slices to avoid allocations wherever possible:

```rust
// ✅ GOOD - Borrows from source
let text = node.utf8_text(source.as_bytes())?;

// ❌ BAD - Allocates new String
let text = node.text().to_string();
```

**Performance impact:**
- Reduces allocations by ~60%
- Critical for hot paths (parsing thousands of nodes)
- Keeps memory usage constant regardless of file size

### 3. Error-Tolerant Parsing

tree-sitter handles incomplete/broken code gracefully:

```rust
// Even with syntax errors, tree-sitter produces partial AST
let tree = parser.parse(source, None)?;
// Error nodes are marked but parsing continues
```

**Real-world benefits:**
- Works on code being actively edited
- Handles incomplete files
- Gracefully degrades on syntax errors

### 4. Type-Safe Error Handling

Explicit error handling with `Result<T, E>` - no panics in library code:

```rust
pub fn transform(
    source: &str,
    language: Language,
    mode: Mode,
) -> Result<String, TransformError>
```

**Guarantees:**
- No unwraps in library code (only in tests)
- All errors are recoverable
- Clear error messages with context

## Component Breakdown

### Language Detection Layer

**Location:** `rskim/src/main.rs` (CLI) and `rskim-core/src/types.rs`

**Responsibilities:**
1. Detect language from file extension
2. Map to tree-sitter grammar
3. Provide fallback for stdin or unusual extensions

**Architecture decision (Option B):**
- Always try auto-detection first
- Use `--language` flag only as fallback when auto-detection fails
- Enables mixed-language directory processing

```rust
// ARCHITECTURE: Option B - Auto-detect first, explicit language as fallback
let result = match transform_auto(&contents, path, mode) {
    Ok(output) => output,
    Err(e) => {
        if let Some(language) = explicit_lang {
            transform(&contents, language, mode)?
        } else {
            return Err(e.into());
        }
    }
};
```

**Supported extensions:**
- TypeScript: `.ts`, `.tsx`
- JavaScript: `.js`, `.jsx`
- Python: `.py`, `.pyi`
- Rust: `.rs`
- Go: `.go`
- Java: `.java`
- Markdown: `.md`, `.markdown`

### Parser Layer

**Location:** `rskim-core/src/transformer.rs`

**Responsibilities:**
1. Initialize tree-sitter parser with correct grammar
2. Parse source code to AST
3. Handle parse errors gracefully

**Implementation:**
```rust
pub fn to_tree_sitter(&self) -> tree_sitter::Language {
    match self {
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        // ...
    }
}
```

**Performance characteristics:**
- **Parsing time:** 60-85µs for small files (<100 lines)
- **Scaling:** Linear with file size (~5µs per line)
- **Memory:** Proportional to AST complexity, not file size

### Transformation Layer

**Location:** `rskim-core/src/transformer.rs`

**Responsibilities:**
1. Walk AST using tree-sitter cursor
2. Extract relevant nodes based on mode
3. Format output with preserved indentation

**Mode implementations:**

**Structure Mode:**
- Keeps function/method signatures
- Replaces bodies with `/* ... */`
- Preserves type definitions
- Keeps imports/exports

**Signatures Mode:**
- Extracts only callable signatures
- Removes type definitions
- Removes imports
- Most aggressive code reduction

**Types Mode:**
- Keeps only type definitions
- Removes all implementation
- Includes interfaces, type aliases, enums

**Full Mode:**
- No transformation
- Returns source unchanged

**Key optimization:**
```rust
// Reuse cursor instead of creating new ones
let mut cursor = tree.walk();
traverse_tree(&mut cursor, source, &mut output, config);
```

### Streaming Output Layer

**Location:** `rskim/src/main.rs`

**Responsibilities:**
1. Buffer output for performance
2. Handle file headers for multi-file mode
3. Write to stdout efficiently

**Implementation:**
```rust
use std::io::{BufWriter, Write};

let mut stdout = BufWriter::new(io::stdout());
writeln!(stdout, "{}", output)?;
stdout.flush()?;
```

**Buffering strategy:**
- Uses 8KB buffer (default BufWriter size)
- Flushes after each file in multi-file mode
- Reduces syscalls by ~100x

## Data Flow

### Single File Processing

```
File Path
    ↓
Read to String (std::fs::read_to_string)
    ↓
Detect Language (from extension)
    ↓
Parse AST (tree-sitter)
    ↓
Transform AST (mode-specific visitor)
    ↓
Stream to stdout (BufWriter)
```

### Multi-File Processing

```
Glob Pattern / Directory
    ↓
Collect Matching Files (glob crate / recursive walk)
    ↓
Sort for Deterministic Order
    ↓
Parallel Processing (rayon)
    │
    ├─ File 1 → Parse → Transform → Buffer
    ├─ File 2 → Parse → Transform → Buffer
    └─ File N → Parse → Transform → Buffer
    ↓
Serialize Output (file headers + content)
    ↓
Stream to stdout
```

### Cache Hit Flow

```
File Path
    ↓
Calculate Cache Key (SHA256 of path + mtime + mode)
    ↓
Check Cache (~/.cache/skim/)
    ↓
[HIT] Read JSON → Return Cached Result (5ms)
[MISS] Full Parse → Transform → Write to Cache → Return
```

## Caching Architecture

### Cache Location

Platform-specific directories:
- Linux: `~/.cache/skim/`
- macOS: `~/Library/Caches/skim/`
- Windows: `%LOCALAPPDATA%\skim\`

### Cache Key Generation

```rust
SHA256(file_path + modification_time + transformation_mode)
```

**Example:**
```
Input:
  - path: "/workspace/skim/src/main.rs"
  - mtime: 1698765432
  - mode: "structure"

Output:
  - cache_key: "a3f2b8e1c5d9..."
  - cache_file: "~/.cache/skim/a3f2b8e1c5d9.json"
```

### Cache Entry Format

```json
{
  "path": "/workspace/skim/src/main.rs",
  "mode": "structure",
  "mtime": 1698765432,
  "content": "pub fn main() { /* ... */ }\n",
  "original_tokens": 1500,
  "transformed_tokens": 300
}
```

### Cache Invalidation

**Automatic invalidation triggers:**
1. File modification (mtime change)
2. Different transformation mode
3. Manual clear (`--clear-cache`)

**No manual invalidation needed** - mtime-based approach ensures cache is always fresh.

### Atomic Writes

Cache writes are atomic to prevent corruption:

```rust
// Write to temporary file
let temp_path = cache_path.with_extension(".tmp");
fs::write(&temp_path, json)?;

// Atomic rename
fs::rename(&temp_path, &cache_path)?;
```

## Parallelization Architecture

### Multi-File Parallelism

Uses **rayon** for work-stealing parallelism:

```rust
use rayon::prelude::*;

files.par_iter()  // Parallel iterator
    .map(|path| process_file(path, options))
    .collect::<Vec<_>>()?;
```

**Benefits:**
- Automatic work balancing
- Scales to available CPU cores
- Zero-cost abstraction (no threading overhead)

### Parallelism Strategy

**Default:** Number of CPU cores (detected at runtime)
```bash
skim 'src/**/*.ts'  # Uses all cores
```

**Custom:** Specify with `--jobs` flag
```bash
skim 'src/**/*.ts' --jobs 4  # Force 4 threads
```

**Optimal thread count:**
- I/O-bound (local files): `cores * 2`
- CPU-bound (large files): `cores`
- Network filesystems: `cores / 2` (avoid overwhelming NFS)

## Security Architecture

### Input Validation

**File size limit:** 50MB per file
```rust
if metadata.len() > MAX_FILE_SIZE {
    return Err(Error::FileTooLarge);
}
```

**Recursion depth limit:** 500 levels
```rust
fn traverse(cursor: &mut Cursor, depth: usize) {
    if depth > MAX_RECURSION_DEPTH {
        return Err(Error::RecursionLimitExceeded);
    }
    // ...
}
```

**AST node limit:** 100,000 nodes
```rust
if node_count > MAX_AST_NODES {
    return Err(Error::TooManyNodes);
}
```

### Path Traversal Protection

**Blocked patterns:**
- `../../../etc/passwd` - Parent directory traversal
- Symlinks in directory processing
- Absolute paths in glob patterns (security contexts)

```rust
fn reject_traversal(path: &Path) -> Result<()> {
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(Error::PathTraversal);
    }
    Ok(())
}
```

### Sandboxing

**No network access:** Skim never makes network requests

**No code execution:** Only parses code, never evaluates it

**Read-only by default:** Only writes to cache directory

## Project Structure

```
skim/
├── crates/
│   ├── rskim-core/          # Core library (pure logic, no I/O)
│   │   ├── src/
│   │   │   ├── lib.rs       # Public API
│   │   │   ├── transformer.rs  # AST transformation logic
│   │   │   ├── types.rs     # Language/Mode enums
│   │   │   └── tokens.rs    # Token counting (tiktoken)
│   │   └── Cargo.toml
│   │
│   └── rskim/               # CLI binary (I/O layer)
│       ├── src/
│       │   └── main.rs      # CLI, file I/O, caching, multi-file
│       └── Cargo.toml
│
├── tests/
│   ├── fixtures/            # Test files for each language
│   │   ├── typescript/
│   │   ├── python/
│   │   ├── rust/
│   │   └── ...
│   ├── cli_basic.rs         # Single-file CLI tests
│   ├── cli_glob.rs          # Glob pattern tests
│   ├── cli_directory.rs     # Directory processing tests
│   └── integration/         # Integration tests
│
└── benches/
    └── benchmarks.rs        # Criterion benchmarks
```

### Separation of Concerns

**rskim-core:**
- Pure transformation logic
- No file I/O
- No caching
- No CLI dependencies
- Can be used as library

**rskim:**
- CLI interface
- File I/O (single/multi-file)
- Caching layer
- Parallel processing
- Uses rskim-core internally

**Benefits:**
- Core library is testable without I/O
- Can be embedded in other tools
- Clear boundaries between logic and I/O

## Performance Characteristics

### Time Complexity

**Single file:**
- Language detection: O(1)
- Parsing: O(n) where n = file size
- Transformation: O(m) where m = AST nodes
- Total: **O(n)** - Linear scaling

**Multi-file:**
- Without parallelism: O(k * n) where k = file count
- With parallelism: O(k * n / c) where c = core count
- Glob matching: O(f) where f = total files in search path

### Space Complexity

**Single file:**
- Source buffer: O(n)
- AST: O(m) where m = AST nodes
- Output buffer: O(n * r) where r = reduction rate (0.1-0.4)
- Total: **O(n)** - Linear memory usage

**Cache:**
- Per-entry: O(n * r) + metadata (~50 bytes)
- Total cache: Unbounded (manual clearing required)

### Scalability Limits

**Tested configurations:**
- ✅ 3000-line files: 14.6ms
- ✅ 100 files parallel: <1s
- ✅ Mixed languages: No overhead
- ⚠️ 10,000+ files: Consider batching
- ⚠️ 100MB+ files: Will hit 50MB limit

## Extension Points

### Adding New Languages

**Required changes:**
1. Add grammar to `Cargo.toml`
2. Update `Language` enum in `types.rs`
3. Add mapping in `to_tree_sitter()` method
4. Add extension in `from_extension()`
5. Add test fixtures

**Estimated time:** ~30 minutes per language

**Example:**
```rust
// 1. Cargo.toml
tree-sitter-kotlin = "0.3"

// 2. types.rs
pub enum Language {
    // ...
    Kotlin,
}

// 3. to_tree_sitter()
Language::Kotlin => tree_sitter_kotlin::LANGUAGE.into(),

// 4. from_extension()
"kt" => Some(Language::Kotlin),
```

### Adding New Modes

**Required changes:**
1. Add variant to `Mode` enum
2. Implement transformation logic in `transformer.rs`
3. Add tests
4. Update documentation

**Example use cases:**
- `--mode minimal` - Only exports
- `--mode headers` - Only top-level declarations
- `--mode imports` - Only import statements

## Testing Architecture

### Test Layers

**Unit tests** (in `rskim-core`):
- Transformation correctness
- Edge cases (empty files, syntax errors)
- Each language × each mode

**Integration tests** (in `tests/`):
- CLI argument parsing
- File I/O
- Multi-file processing
- Caching behavior
- Error handling

**Benchmark tests** (in `benches/`):
- Performance regression detection
- Scaling characteristics
- Real-world file performance

### Test Fixtures

**Structure:**
```
tests/fixtures/
├── typescript/
│   ├── simple.ts
│   ├── class.ts
│   ├── async.ts
│   └── generics.ts
├── python/
│   ├── simple.py
│   ├── class.py
│   └── async.py
└── ...
```

**Coverage goals:**
- ✅ All languages have fixtures
- ✅ All modes tested per language
- ✅ Edge cases (empty, errors, large files)
- ✅ Real-world code samples

## Future Architecture Improvements

Potential enhancements (not yet implemented):

1. **Incremental parsing** - Reuse AST for unchanged regions
2. **Streaming parser** - Process files larger than memory
3. **Plugin system** - Custom transformations via WebAssembly
4. **Distributed cache** - Shared cache for team environments
5. **Language server** - LSP for real-time skimming in editors

See [GitHub issues](https://github.com/dean0x/skim/issues) for feature requests and architecture proposals.
