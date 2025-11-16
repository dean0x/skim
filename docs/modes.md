# Transformation Modes

Skim offers four transformation modes, each with different levels of aggressiveness and use cases. Choose the mode based on how much information you need to preserve.

## Mode Comparison

| Mode       | Token Reduction | What's Kept                              | What's Removed              |
|------------|-----------------|------------------------------------------|-----------------------------|
| Structure  | 70-80%          | Signatures, types, classes, imports      | Function bodies             |
| Signatures | 85-92%          | Only callable signatures                 | Everything else             |
| Types      | 90-95%          | Only type definitions                    | All code                    |
| Full       | 0%              | Everything (original source)             | Nothing                     |

## Structure Mode (Default)

### Overview

**Token reduction: 70-80%**

Structure mode is the default and most balanced mode. It keeps enough information to understand the codebase architecture while removing implementation details.

### What's Preserved

- Function and method signatures
- Class declarations
- Interface definitions
- Type aliases and enums
- Import/export statements
- Comments (structural comments like JSDoc)

### What's Removed

- Function bodies (replaced with `/* ... */`)
- Implementation logic
- Variable assignments
- Loop contents
- Conditional branches

### Usage

```bash
skim file.ts --mode structure

# Or simply (structure is default)
skim file.ts
```

### Example

**Input (TypeScript):**
```typescript
export class UserService {
    async findUser(id: string): Promise<User> {
        const user = await db.users.findOne({ id });
        if (!user) throw new NotFoundError();
        return user;
    }
}
```

**Output:**
```typescript
export class UserService {
    async findUser(id: string): Promise<User> { /* ... */ }
}
```

### Use Cases

- **Understanding code organization** - See how the codebase is structured
- **API exploration** - Understand what functions/classes are available
- **LLM context optimization** - Give AI enough context to understand architecture
- **Code review** - Focus on interface changes without implementation noise

### Best For

- Initial codebase exploration
- Architectural discussions
- Broad understanding of multiple files

## Signatures Mode

### Overview

**Token reduction: 85-92%**

Signatures mode is more aggressive - it keeps ONLY callable signatures and removes everything else, including type definitions and imports.

### What's Preserved

- Function declarations (name, parameters, return type)
- Method signatures
- Function names and their types

### What's Removed

- Function bodies
- Type definitions
- Interface declarations
- Imports/exports
- Class member variables
- Everything except callable functions

### Usage

```bash
skim file.ts --mode signatures
```

### Example

**Input (TypeScript):**
```typescript
interface User {
    id: string;
    name: string;
}

export class UserService {
    private db: Database;

    async findUser(id: string): Promise<User> {
        const user = await db.users.findOne({ id });
        if (!user) throw new NotFoundError();
        return user;
    }
}
```

**Output:**
```typescript
async findUser(id: string): Promise<User>
```

### Use Cases

- **API documentation generation** - Extract all public APIs
- **Type stub generation** - Create `.d.ts` files
- **Function catalog** - List all available functions
- **Extreme context compression** - Maximum token reduction

### Best For

- Generating API reference docs
- Creating type stubs for untyped code
- When you only need to know "what functions exist"

## Types Mode

### Overview

**Token reduction: 90-95%**

Types mode is the most aggressive transformation - it keeps ONLY type definitions and removes all code, including function signatures.

### What's Preserved

- Type aliases
- Interface declarations
- Enum definitions
- Type parameters
- Generic constraints

### What's Removed

- All implementation code
- Function signatures
- Class methods
- Variable declarations
- Imports (except type imports)

### Usage

```bash
skim file.ts --mode types
```

### Example

**Input (TypeScript):**
```typescript
export interface User {
    id: string;
    name: string;
}

export type UserRole = 'admin' | 'user';

export class UserService {
    async findUser(id: string): Promise<User> {
        const user = await db.users.findOne({ id });
        return user;
    }
}
```

**Output:**
```typescript
export interface User {
    id: string;
    name: string;
}

export type UserRole = 'admin' | 'user';
```

### Use Cases

- **Type system analysis** - Focus only on type structure
- **Schema extraction** - Extract data models
- **Type safety review** - Analyze type definitions
- **Documentation of types** - Document data structures

### Best For

- Analyzing type hierarchies
- Extracting data schemas
- Type system discussions
- When you only care about data structures

## Full Mode

### Overview

**Token reduction: 0%**

Full mode performs no transformation - it returns the original source code unchanged, similar to `cat`.

### What's Preserved

Everything (exact copy of source)

### Usage

```bash
skim file.ts --mode full
```

### Use Cases

- **Testing** - Verify skim is reading files correctly
- **Comparison** - Compare with other modes
- **Passthrough** - Use skim in pipelines without transformation
- **Debugging** - Check if issues are in parsing or transformation

### Best For

- Development and testing
- Verification workflows
- When you need the full source but want consistent tooling

## Choosing the Right Mode

### Decision Tree

```
Need implementation details? → Use Full mode
    ↓ No
Need only types/interfaces? → Use Types mode
    ↓ No
Need only functions/methods? → Use Signatures mode
    ↓ No
Need structure + signatures? → Use Structure mode (default)
```

### By Use Case

| Use Case                          | Recommended Mode |
|-----------------------------------|------------------|
| Understand codebase               | Structure        |
| Generate API docs                 | Signatures       |
| Analyze type system               | Types            |
| Initial exploration               | Structure        |
| Extract data models               | Types            |
| Create type stubs                 | Signatures       |
| Maximum token reduction           | Types            |
| LLM context (balanced)            | Structure        |
| LLM context (aggressive)          | Signatures       |
| Testing/debugging                 | Full             |

### By Language Features

**For TypeScript/JavaScript:**
- Structure: Keeps class structure, method signatures, type annotations
- Signatures: Keeps only function/method signatures
- Types: Keeps interfaces, types, enums

**For Python:**
- Structure: Keeps function signatures, class definitions, type hints
- Signatures: Keeps only function definitions with signatures
- Types: Keeps TypedDict, Protocol, type aliases (if using typing module)

**For Rust:**
- Structure: Keeps struct definitions, impl blocks, trait definitions
- Signatures: Keeps only function signatures from impls
- Types: Keeps struct fields, enum variants, type aliases

## Supported Languages

| Language   | Status | Extensions         | Notes                    |
|------------|--------|--------------------|--------------------------|
| TypeScript | ✅     | `.ts`, `.tsx`      | Excellent grammar        |
| JavaScript | ✅     | `.js`, `.jsx`      | Full ES2024 support      |
| Python     | ✅     | `.py`, `.pyi`      | Complete coverage        |
| Rust       | ✅     | `.rs`              | Up-to-date grammar       |
| Go         | ✅     | `.go`              | Stable                   |
| Java       | ✅     | `.java`            | Good coverage            |
| Markdown   | ✅     | `.md`, `.markdown` | Header extraction        |
| JSON       | ✅     | `.json`            | Structure extraction     |

### Language-Specific Notes

**Markdown:**
- Structure mode: Extracts H1-H3 headers
- Signatures/Types mode: Extracts H1-H6 headers
- Full mode: Original markdown content

**JSON:**
- All modes (structure/signatures/types/full) produce identical output
- JSON is data, not code, so there are no "signatures" or "types" to extract
- Extracts structure: keeps only keys and nesting, strips all values
- Example: `{"name": "John", "age": 30}` → `{name, age}`
- Security limits: MAX_JSON_DEPTH=500, MAX_JSON_KEYS=10,000

## Performance by Mode

All modes maintain similar parsing performance (~15ms for 3000-line files). The difference is only in the transformation logic:

- **Structure mode**: Slightly slower (needs to identify and replace bodies)
- **Signatures mode**: Fast (simpler filtering)
- **Types mode**: Fastest (smallest subset to extract)
- **Full mode**: Instant (no transformation)

The performance difference is negligible (<5ms) for typical files.

## Combining with Other Options

### With Token Stats

```bash
skim file.ts --mode signatures --show-stats
# Shows how many tokens each mode saves
```

### With Multiple Files

```bash
skim src/ --mode types --no-header
# Apply types mode to entire directory
```

### With Caching

All modes benefit from caching equally. The cache key includes the mode, so changing modes will cache separately.

```bash
skim file.ts --mode structure  # Cached
skim file.ts --mode signatures # Different cache entry
```
