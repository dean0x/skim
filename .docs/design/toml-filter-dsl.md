# TOML Filter DSL Specification

**Issue:** #59
**Status:** Draft
**Date:** 2026-03-25

---

## 1. Purpose

Allow users to define custom output filter rules in TOML format, extending
skim's built-in transformation modes with project-specific, version-controlled
filtering logic.

Built-in modes (`structure`, `signatures`, `types`, `full`, `minimal`, `pseudo`)
cover common cases but cannot anticipate every project's needs. The TOML Filter
DSL enables:

- Stripping debug/logging statements before sending code to an LLM
- Collapsing import blocks to save tokens
- Preserving public API surfaces while removing internal implementation
- Replacing verbose patterns with compact summaries
- Applying language-specific or mode-specific rules

Filters compose with existing modes -- they run **after** the mode transformation,
providing a second pass of user-controlled refinement.

---

## 2. File Location and Discovery

### Project-level (recommended)

```
<project-root>/.skim.toml
```

This file is already created by `skim init`. The `[filters]` section is optional
and coexists with any future configuration sections.

### User-level (personal defaults)

```
~/.config/skim/filters.toml
```

User-level filters apply to all projects unless overridden by project-level rules
with the same `name`.

### Precedence

1. **Project-level** (`.skim.toml`) -- highest priority
2. **User-level** (`~/.config/skim/filters.toml`) -- lowest priority

When both files define a rule with the same `name`, the project-level rule wins.
Rules from both files are merged into a single priority-ordered chain.

---

## 3. Format

### Minimal example

```toml
[filters]

[[filters.rules]]
name = "strip-debug-logs"
description = "Remove console.log and debug statements"
match = { pattern = "console\\.(log|debug|warn)\\(.*\\)", language = ["typescript", "javascript"] }
action = "remove"
priority = 10
```

### Full example with all filter actions

```toml
[filters]
# Optional metadata
version = 1

# Rule 1: Remove debug logging
[[filters.rules]]
name = "strip-debug-logs"
description = "Remove console.log and debug statements"
match = { pattern = "console\\.(log|debug|warn)\\(.*\\)", language = ["typescript", "javascript"] }
action = "remove"
priority = 10

# Rule 2: Collapse import blocks
[[filters.rules]]
name = "collapse-imports"
description = "Collapse import blocks to single summary line"
match = { node_type = "import_statement", language = ["typescript"] }
action = "collapse"
priority = 20

# Rule 3: Always preserve public exports
[[filters.rules]]
name = "keep-public-api"
description = "Always preserve public exports regardless of mode"
match = { node_type = "export_statement" }
action = "keep"
priority = 100

# Rule 4: Replace test boilerplate with summary
[[filters.rules]]
name = "summarize-test-setup"
description = "Replace beforeEach/afterEach blocks with summary comment"
match = { pattern = "(beforeEach|afterEach)\\s*\\(", language = ["typescript", "javascript"] }
action = { replace = "/* {name}: test lifecycle hook */" }
priority = 15

# Rule 5: Mode-specific rule (only in structure mode)
[[filters.rules]]
name = "strip-comments-in-structure"
description = "Remove all comments in structure mode for maximum compression"
match = { node_type = "comment", mode = ["structure"] }
action = "remove"
priority = 5

# Rule 6: Pattern with node_type combined
[[filters.rules]]
name = "strip-logging-calls"
description = "Remove logging function calls"
match = { node_type = "expression_statement", pattern = "logger\\.(info|debug|trace)\\(" }
action = "remove"
priority = 12
```

---

## 4. Schema Reference

### Top-level

```toml
[filters]
version = 1  # Optional. Schema version for forward compatibility.

[[filters.rules]]
# ... rule definitions
```

### Rule fields

| Field         | Type                | Required | Description                                      |
|---------------|---------------------|----------|--------------------------------------------------|
| `name`        | `string`            | Yes      | Unique identifier for the rule                   |
| `description` | `string`            | No       | Human-readable description                       |
| `match`       | `MatchCriteria`     | Yes      | Conditions that determine which code to match     |
| `action`      | `Action`            | Yes      | What to do with matched code                      |
| `priority`    | `integer`           | Yes      | Execution order (higher = runs later, wins ties) |
| `enabled`     | `boolean`           | No       | Default: `true`. Set `false` to disable without deleting |

### MatchCriteria

At least one of `pattern` or `node_type` must be specified. When both are
present, **both must match** (logical AND).

| Field       | Type              | Required | Description                                       |
|-------------|-------------------|----------|---------------------------------------------------|
| `pattern`   | `string` (regex)  | No*      | Regex pattern matched against source text of node  |
| `node_type` | `string`          | No*      | tree-sitter AST node type to match                 |
| `language`  | `string[]`        | No       | Restrict to these languages. Default: all languages |
| `mode`      | `string[]`        | No       | Restrict to these modes. Default: all modes         |

\* At least one of `pattern` or `node_type` is required.

**Language values:** `typescript`, `javascript`, `python`, `rust`, `go`, `java`,
`c`, `cpp`, `markdown`, `json`, `yaml`, `toml`

**Mode values:** `structure`, `signatures`, `types`, `full`, `minimal`, `pseudo`

### Pattern matching details

- Patterns use Rust `regex` crate syntax (compatible with PCRE-like patterns)
- Patterns are matched against the **source text** of the matched node (or line
  if no `node_type` is specified)
- Backslashes must be escaped in TOML: `\\d` for regex `\d`
- Patterns are case-sensitive by default. Use `(?i)` prefix for case-insensitive

### Node type matching details

- Node types correspond to tree-sitter grammar node names
- Common node types by language:

| Language   | Common node types                                                        |
|------------|--------------------------------------------------------------------------|
| TypeScript | `import_statement`, `export_statement`, `function_declaration`, `class_declaration`, `comment`, `expression_statement`, `type_alias_declaration` |
| Python     | `import_statement`, `import_from_statement`, `function_definition`, `class_definition`, `comment`, `expression_statement`, `decorated_definition` |
| Rust       | `use_declaration`, `function_item`, `struct_item`, `impl_item`, `trait_item`, `macro_definition`, `line_comment`, `block_comment` |
| Go         | `import_declaration`, `function_declaration`, `method_declaration`, `type_declaration`, `comment` |
| Java       | `import_declaration`, `class_declaration`, `method_declaration`, `interface_declaration`, `line_comment`, `block_comment` |
| C/C++      | `preproc_include`, `function_definition`, `struct_specifier`, `comment`  |

---

## 5. Filter Actions

### `remove`

Delete the matched node/line entirely from output.

```toml
action = "remove"
```

**Example input:**
```typescript
import { readFile } from "fs";
console.log("starting up");
export function process(data: string): Result<Output> { /* ... */ }
console.debug("debug info");
```

**Rule:**
```toml
[[filters.rules]]
name = "strip-debug"
match = { pattern = "console\\.(log|debug)\\(" }
action = "remove"
priority = 10
```

**Expected output:**
```typescript
import { readFile } from "fs";
export function process(data: string): Result<Output> { /* ... */ }
```

### `collapse`

Replace the matched node with a single-line summary showing the node type
and count.

```toml
action = "collapse"
```

**Example input:**
```typescript
import { readFile } from "fs";
import { writeFile } from "fs/promises";
import { join, resolve } from "path";
import { Config } from "./config";
import { Logger } from "./logger";

export function main(): void { /* ... */ }
```

**Rule:**
```toml
[[filters.rules]]
name = "collapse-imports"
match = { node_type = "import_statement", language = ["typescript"] }
action = "collapse"
priority = 20
```

**Expected output:**
```typescript
/* 5 import statements collapsed */

export function main(): void { /* ... */ }
```

Consecutive matched nodes are collapsed into a single summary line. Non-consecutive
matches each produce their own summary.

### `keep`

Force the matched node to be preserved in output, even if the current mode
would normally strip it. This is an override that prevents other rules and
mode transformations from removing the node.

```toml
action = "keep"
```

**Example input (structure mode would strip function bodies):**
```typescript
export function publicApi(data: string): Result<Output> {
    return validate(data).map(transform);
}

function internalHelper(x: number): number {
    return x * 2;
}
```

**Rule:**
```toml
[[filters.rules]]
name = "keep-exports"
match = { node_type = "export_statement" }
action = "keep"
priority = 100
```

**Expected output (in structure mode):**
```typescript
export function publicApi(data: string): Result<Output> {
    return validate(data).map(transform);
}

function internalHelper(x: number): number { /* ... */ }
```

The exported function retains its body because the `keep` rule overrides
structure mode's body-stripping behavior.

### `replace`

Replace the matched node with a custom string. The replacement string supports
template variables:

| Variable       | Expands to                                    |
|----------------|-----------------------------------------------|
| `{name}`       | The rule's `name` field                       |
| `{node_type}`  | The tree-sitter node type of the matched node |
| `{match_text}` | First 60 characters of the matched source text|
| `{line}`       | Line number of the matched node               |

```toml
action = { replace = "/* {name}: {node_type} at line {line} */" }
```

**Example input:**
```typescript
beforeEach(async () => {
    db = await createTestDatabase();
    cache = new MockCache();
    logger = new TestLogger();
    service = new UserService(db, cache, logger);
});
```

**Rule:**
```toml
[[filters.rules]]
name = "summarize-test-setup"
match = { pattern = "beforeEach\\s*\\(", language = ["typescript"] }
action = { replace = "/* {name}: test lifecycle hook */" }
priority = 15
```

**Expected output:**
```typescript
/* summarize-test-setup: test lifecycle hook */
```

---

## 6. Priority Chain

### Execution order

Rules execute in priority order, lowest first. Within the same priority level,
project-level rules execute before user-level rules.

### Built-in rule priorities

Built-in mode transformations (structure, signatures, etc.) have an implicit
priority of **0**. User rules with priority > 0 can override built-in behavior.

| Priority range | Owner        | Description                          |
|----------------|--------------|--------------------------------------|
| 0              | Built-in     | Mode transformations (structure, etc.)|
| 1 - 49         | User         | Low-priority refinements             |
| 50 - 99        | User         | Standard filtering rules             |
| 100+           | User         | High-priority overrides (`keep` rules)|

### Conflict resolution

When multiple rules match the same node:

1. Rules are applied in priority order (lowest first)
2. `keep` at any priority prevents `remove` at lower priority
3. `replace` at higher priority overrides `replace` at lower priority
4. `remove` at higher priority overrides `collapse` at lower priority
5. If two rules have the same priority and conflict, the **project-level**
   rule wins over the **user-level** rule
6. If two rules from the same file have the same priority and conflict,
   the rule defined **later** in the file wins (last-writer-wins)

### Conflict examples

```toml
# Rule A: priority 10, action "remove"
# Rule B: priority 100, action "keep"
# Result: Node is KEPT (B wins by priority)

# Rule C: priority 50, action "collapse" (user-level)
# Rule D: priority 50, action "remove" (project-level)
# Result: Node is REMOVED (D wins by source precedence)
```

---

## 7. Trust Model

### Trusted sources

Both configuration files are considered trusted:

1. **`.skim.toml`** -- under version control, reviewed in PRs. Trusted by default.
2. **`~/.config/skim/filters.toml`** -- user's own machine, user-controlled.
   Trusted by default.

### Security considerations

- **Regex complexity:** Patterns are compiled with a size limit to prevent
  ReDoS attacks. Patterns exceeding the limit are rejected at load time with
  a clear error message. Default limit: 1 MB compiled regex size.
- **Rule count:** Maximum 100 rules per file (200 total across both files).
  Prevents accidental performance degradation from excessive rules.
- **No code execution:** Filters are declarative only. No shell commands,
  no scripting, no dynamic evaluation. The `replace` action supports only
  the documented template variables.
- **No file system access:** Filters cannot read files, access environment
  variables, or interact with the system beyond the transformation pipeline.

### Untrusted input protection

If a future feature allows loading filters from untrusted sources (e.g.,
downloaded from a registry), the following safeguards must be added:

- Explicit opt-in: `skim verify --trust <source>`
- Content-addressed integrity (SHA-256 hash pinning)
- Sandboxed regex execution with timeout

These are **not implemented** in v1. Filters from `.skim.toml` and
`~/.config/skim/filters.toml` are trusted.

---

## 8. `skim verify` Command

### Purpose

Validate TOML syntax, check for conflicting rules, and report the
precedence chain. Intended for CI pipelines and pre-commit hooks.

### Usage

```bash
# Validate project-level filters
skim verify

# Validate a specific file
skim verify --file path/to/filters.toml

# Verbose output showing full precedence chain
skim verify --verbose
```

### Validation checks

| Check                    | Severity | Description                                   |
|--------------------------|----------|-----------------------------------------------|
| TOML syntax              | Error    | File must be valid TOML                       |
| Schema conformance       | Error    | All required fields present, correct types    |
| Unique rule names        | Error    | No duplicate `name` within a single file      |
| Valid regex patterns      | Error    | All `pattern` values must compile             |
| Valid node types          | Warning  | Node types checked against known grammar types|
| Valid language values     | Error    | Languages must be in supported set            |
| Valid mode values         | Error    | Modes must be in supported set                |
| Priority conflicts       | Warning  | Same-priority rules matching same criteria    |
| Shadowed rules           | Info     | Project rules that shadow user-level rules    |
| Rule count limit         | Error    | Exceeds 100 rules per file                    |
| Regex complexity          | Error    | Pattern exceeds compiled size limit           |
| Dead rules               | Warning  | `enabled = false` rules                       |

### Exit codes

| Code | Meaning                                                |
|------|--------------------------------------------------------|
| 0    | All checks pass (warnings printed to stderr)          |
| 1    | One or more errors found                               |
| 2    | File not found or not readable                         |

### Output format

**Default (human-readable):**

```
Validating .skim.toml...

  Rules: 6 (6 enabled, 0 disabled)
  Errors: 0
  Warnings: 1

  Priority chain:
    5  strip-comments-in-structure  [structure]  remove   comment
   10  strip-debug-logs             [ts, js]     remove   pattern
   12  strip-logging-calls          [all]        remove   expression_statement + pattern
   15  summarize-test-setup         [ts, js]     replace  pattern
   20  collapse-imports             [ts]         collapse import_statement
  100  keep-public-api              [all]        keep     export_statement

  Warnings:
    - Rule "strip-debug-logs" and "strip-logging-calls" may match
      overlapping content at different priorities (10 vs 12)

  Result: PASS (1 warning)
```

**JSON output (`skim verify --json`):**

```json
{
  "file": ".skim.toml",
  "rules_total": 6,
  "rules_enabled": 6,
  "errors": [],
  "warnings": [
    {
      "type": "potential_overlap",
      "rules": ["strip-debug-logs", "strip-logging-calls"],
      "message": "May match overlapping content at different priorities (10 vs 12)"
    }
  ],
  "priority_chain": [
    { "priority": 5, "name": "strip-comments-in-structure", "action": "remove" },
    { "priority": 10, "name": "strip-debug-logs", "action": "remove" },
    { "priority": 12, "name": "strip-logging-calls", "action": "remove" },
    { "priority": 15, "name": "summarize-test-setup", "action": "replace" },
    { "priority": 20, "name": "collapse-imports", "action": "collapse" },
    { "priority": 100, "name": "keep-public-api", "action": "keep" }
  ],
  "result": "pass"
}
```

---

## 9. Pipeline Integration

### Where filters are applied

Filters sit between the mode transformation and output emission in
skim's processing pipeline:

```
Source Code
    |
    v
Language Detection
    |
    v
tree-sitter Parse (AST)
    |
    v
Mode Transformation (structure/signatures/types/full/minimal/pseudo)
    |                                    <-- Built-in priority 0
    v
+-------------------------------+
| TOML Filter DSL               |       <-- User priorities 1+
|                               |
|  1. Load rules from files     |
|  2. Filter by language/mode   |
|  3. Sort by priority          |
|  4. Walk AST post-transform   |
|  5. Apply matching rules      |
+-------------------------------+
    |
    v
Truncation (--max-lines, --last-lines, --tokens)
    |
    v
Token Counting (--show-stats)
    |
    v
Caching (write to ~/.cache/skim/)
    |
    v
Output (stdout)
```

### Integration with existing modes

Filters see the **post-transformation** output, not the raw source. This means:

- In `structure` mode, function bodies are already replaced with `/* ... */`
  before filters run. A filter cannot match against the original body text.
- In `full` mode, filters see the complete source and can strip/collapse/keep
  any part of it.
- In `types` mode, only type definitions survive the mode pass. Filters
  can further refine which types to keep.

### Integration with caching

Cache keys must include a hash of the active filter rules to prevent stale
cache hits when rules change:

```
CacheKey {
    path: PathBuf,
    mtime: SystemTime,
    mode: String,
    filter_hash: Option<u64>,  // NEW: hash of applicable filter rules
}
```

When no filters are defined, `filter_hash` is `None` and caching works
exactly as before (backward compatible).

### Integration with token counting

Filters may increase or decrease token count. The `--show-stats` output
should reflect the final post-filter token count:

```
Tokens: 1,234 -> 456 (63% reduction)
          ^        ^
          |        +-- After mode + filters
          +----------- Original source
```

### Integration with multi-file processing

Filters are loaded once at startup and shared across all files in a
multi-file/glob invocation. Per-file filtering uses the `language` and
`mode` fields to determine which rules apply to each file.

---

## 10. Rust Types (Implementation Reference)

These types are provided for implementors. They are NOT part of the public API
and may change during implementation.

```rust
use std::path::PathBuf;

/// A single filter rule parsed from TOML.
#[derive(Debug, Clone)]
pub struct FilterRule {
    pub name: String,
    pub description: Option<String>,
    pub match_criteria: MatchCriteria,
    pub action: FilterAction,
    pub priority: i32,
    pub enabled: bool,
    pub source: FilterSource,
}

/// Where a rule was loaded from (for conflict resolution).
#[derive(Debug, Clone, PartialEq)]
pub enum FilterSource {
    Project(PathBuf),
    User(PathBuf),
}

/// Conditions that determine which AST nodes to match.
#[derive(Debug, Clone)]
pub struct MatchCriteria {
    /// Regex pattern matched against source text.
    pub pattern: Option<regex::Regex>,
    /// tree-sitter node type name.
    pub node_type: Option<String>,
    /// Restrict to specific languages. None = all languages.
    pub languages: Option<Vec<String>>,
    /// Restrict to specific modes. None = all modes.
    pub modes: Option<Vec<String>>,
}

/// Action to take on matched nodes.
#[derive(Debug, Clone)]
pub enum FilterAction {
    /// Delete the node from output.
    Remove,
    /// Collapse consecutive matched nodes into a summary.
    Collapse,
    /// Force-keep the node (override mode stripping).
    Keep,
    /// Replace with a template string.
    Replace(String),
}

/// Loaded and validated filter configuration.
#[derive(Debug)]
pub struct FilterConfig {
    pub rules: Vec<FilterRule>,
    /// Precomputed hash for cache key integration.
    pub hash: u64,
}

impl FilterConfig {
    /// Load filters from project and user paths, merge and validate.
    pub fn load(
        project_path: Option<&Path>,
        user_path: Option<&Path>,
    ) -> Result<Self, FilterError> {
        // 1. Parse TOML from both files
        // 2. Validate schema
        // 3. Merge with project-level precedence
        // 4. Sort by priority
        // 5. Compute hash
        todo!()
    }

    /// Return only rules applicable to the given language and mode.
    pub fn rules_for(&self, language: &str, mode: &str) -> Vec<&FilterRule> {
        self.rules
            .iter()
            .filter(|r| r.enabled)
            .filter(|r| match &r.match_criteria.languages {
                Some(langs) => langs.iter().any(|l| l == language),
                None => true,
            })
            .filter(|r| match &r.match_criteria.modes {
                Some(modes) => modes.iter().any(|m| m == mode),
                None => true,
            })
            .collect()
    }
}

/// Errors from filter loading and validation.
#[derive(Debug, thiserror::Error)]
pub enum FilterError {
    #[error("TOML parse error in {path}: {source}")]
    TomlParse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid regex in rule '{rule}': {source}")]
    InvalidRegex {
        rule: String,
        source: regex::Error,
    },
    #[error("rule '{rule}' missing required field: {field}")]
    MissingField {
        rule: String,
        field: String,
    },
    #[error("duplicate rule name '{name}' in {path}")]
    DuplicateName {
        name: String,
        path: PathBuf,
    },
    #[error("too many rules in {path}: {count} (maximum: 100)")]
    TooManyRules {
        path: PathBuf,
        count: usize,
    },
    #[error("regex too complex in rule '{rule}': compiled size exceeds limit")]
    RegexTooComplex {
        rule: String,
    },
    #[error("unknown language '{language}' in rule '{rule}'")]
    UnknownLanguage {
        language: String,
        rule: String,
    },
    #[error("unknown mode '{mode}' in rule '{rule}'")]
    UnknownMode {
        mode: String,
        rule: String,
    },
}
```

---

## 11. Inline Test Examples

### Test: `remove` action strips matching lines

**Input** (`test.ts`, mode: `structure`):
```typescript
import { Result } from "./types";
console.log("booting");
export function handle(req: Request): Result<Response> { /* ... */ }
console.debug("req:", req);
export function health(): string { /* ... */ }
```

**Rules:**
```toml
[[filters.rules]]
name = "strip-console"
match = { pattern = "console\\.(log|debug)\\(", language = ["typescript"] }
action = "remove"
priority = 10
```

**Expected output:**
```typescript
import { Result } from "./types";
export function handle(req: Request): Result<Response> { /* ... */ }
export function health(): string { /* ... */ }
```

---

### Test: `collapse` action merges consecutive imports

**Input** (`app.ts`, mode: `full`):
```typescript
import { readFile } from "fs";
import { join } from "path";
import { Config } from "./config";

export class App {
    constructor(private config: Config) {}
}
```

**Rules:**
```toml
[[filters.rules]]
name = "collapse-imports"
match = { node_type = "import_statement", language = ["typescript"] }
action = "collapse"
priority = 20
```

**Expected output:**
```typescript
/* 3 import statements collapsed */

export class App {
    constructor(private config: Config) {}
}
```

---

### Test: `keep` action overrides mode stripping

**Input** (`lib.rs`, mode: `signatures`):
```rust
pub fn public_api(data: &str) -> Result<Output> {
    validate(data)?;
    transform(data)
}

fn internal_helper(x: i32) -> i32 {
    x * 2
}
```

**Rules:**
```toml
[[filters.rules]]
name = "keep-public"
match = { pattern = "^pub\\s+fn", language = ["rust"] }
action = "keep"
priority = 100
```

**Expected output:**
```rust
pub fn public_api(data: &str) -> Result<Output> {
    validate(data)?;
    transform(data)
}

fn internal_helper(x: i32) -> i32 { /* ... */ }
```

The `keep` rule preserves the full body of `public_api` even though
signatures mode would normally strip it.

---

### Test: `replace` action with template variables

**Input** (`test.spec.ts`, mode: `structure`):
```typescript
describe("UserService", () => {
    beforeEach(async () => {
        db = await createTestDb();
        cache = new MockCache();
        logger = new TestLogger();
        service = new UserService(db, cache, logger);
    });

    it("creates user", () => { /* ... */ });
});
```

**Rules:**
```toml
[[filters.rules]]
name = "summarize-setup"
match = { pattern = "beforeEach\\s*\\(", language = ["typescript"] }
action = { replace = "/* {name}: test setup ({node_type}) */" }
priority = 15
```

**Expected output:**
```typescript
describe("UserService", () => {
    /* summarize-setup: test setup (expression_statement) */

    it("creates user", () => { /* ... */ });
});
```

---

### Test: mode-restricted rule only fires in specified mode

**Input** (`util.py`, mode: `full`):
```python
# Helper utilities
def add(a: int, b: int) -> int:
    return a + b
```

**Rules:**
```toml
[[filters.rules]]
name = "strip-comments-structure"
match = { node_type = "comment", mode = ["structure"] }
action = "remove"
priority = 5
```

**Expected output (mode: `full`):**
```python
# Helper utilities
def add(a: int, b: int) -> int:
    return a + b
```

The rule does NOT fire because the current mode is `full`, not `structure`.
The same input in `structure` mode would have the comment removed.

---

### Test: combined `node_type` + `pattern` match (AND logic)

**Input** (`server.ts`, mode: `structure`):
```typescript
app.get("/health", healthHandler);
app.post("/users", createUser);
logger.info("server started");
logger.debug("debug mode");
```

**Rules:**
```toml
[[filters.rules]]
name = "strip-logger-calls"
match = { node_type = "expression_statement", pattern = "logger\\.(info|debug)\\(" }
action = "remove"
priority = 12
```

**Expected output:**
```typescript
app.get("/health", healthHandler);
app.post("/users", createUser);
```

Both `node_type` AND `pattern` must match. The `app.get` and `app.post` lines
are `expression_statement` nodes but don't match the `logger` pattern, so they
are preserved.

---

### Test: priority conflict resolution

**Input** (`api.ts`, mode: `structure`):
```typescript
console.log("request received");
export function handler(): void { /* ... */ }
```

**Rules:**
```toml
# Lower priority: remove all expression statements
[[filters.rules]]
name = "strip-expressions"
match = { node_type = "expression_statement" }
action = "remove"
priority = 10

# Higher priority: keep console.log for debugging
[[filters.rules]]
name = "keep-console"
match = { pattern = "console\\.log\\(" }
action = "keep"
priority = 50
```

**Expected output:**
```typescript
console.log("request received");
export function handler(): void { /* ... */ }
```

The `keep` rule at priority 50 overrides the `remove` rule at priority 10
for the `console.log` line.

---

## 12. Error Messages

### TOML parse error

```
error: invalid TOML in .skim.toml
  --> line 5, column 12
  |
  | match = { pattern = "unclosed
  |                      ^^^^^^^
  = expected closing quote

hint: validate your TOML at https://www.toml-lint.com/
```

### Invalid regex

```
error: invalid regex in rule 'strip-debug'
  pattern: console\.(log|debug\(
                               ^
  = unclosed group

hint: escape special characters with double backslash in TOML (e.g., \\()
```

### Missing required field

```
error: rule 'my-rule' in .skim.toml is missing required field 'action'

  [[filters.rules]]
  name = "my-rule"
  match = { pattern = "TODO" }
  # action = ???  <-- required

hint: action must be one of: "remove", "collapse", "keep", or { replace = "..." }
```

### Duplicate rule name

```
error: duplicate rule name 'strip-debug' in .skim.toml
  first definition at line 8
  duplicate at line 22

hint: rename one of the rules to make names unique
```

---

## 13. CLI Integration

### Flags

```bash
# Explicitly specify filter file (overrides auto-discovery)
skim file.ts --filters path/to/custom-filters.toml

# Disable all filters (even if .skim.toml exists)
skim file.ts --no-filters

# Show which filters matched (debug output to stderr)
skim file.ts --debug-filters
```

### Environment variables

| Variable            | Description                                      |
|---------------------|--------------------------------------------------|
| `SKIM_FILTERS_FILE` | Override filter file path (takes precedence over auto-discovery) |
| `SKIM_NO_FILTERS`   | Set to `1`/`true`/`yes` to disable all filters  |

---

## 14. Future Considerations

### Not in v1 (documented for future reference)

1. **Filter registry/sharing:** A community registry of filter presets
   (e.g., `skim filters add react-best-practices`). Requires trust model
   extensions (Section 7).

2. **Conditional actions:** Rules with `if`/`else` logic based on sibling
   nodes or parent context. Increases complexity significantly.

3. **Filter statistics:** `skim stats --filters` showing which rules fired
   most often, token savings per rule. Requires analytics pipeline
   integration.

4. **Live preview:** `skim verify --preview file.ts` showing the effect of
   filters on a specific file. Useful for iterating on rule definitions.

5. **Filter inheritance:** `.skim.toml` in subdirectories inheriting from
   parent directories. Adds resolution complexity.

6. **Negative patterns:** `match = { not_pattern = "..." }` for exclusion
   logic. Can be approximated with `keep` rules at higher priority.

---

## 15. Design Decisions

### Why TOML (not YAML or JSON)?

1. **Already in use:** `.skim.toml` exists from `skim init`. No new file format.
2. **Comment support:** TOML supports inline comments; JSON does not.
3. **Readability:** TOML is more human-friendly than JSON for configuration.
4. **Rust ecosystem:** TOML is the standard configuration format in Rust projects.
   The `toml` crate is already a dependency.

### Why post-transform filtering (not pre-transform)?

Filters run after mode transformation because:

1. **Composability:** Users can combine any mode with any filter set.
2. **Predictability:** The mode determines the baseline; filters refine it.
3. **Performance:** Filtering a smaller post-transform AST is faster than
   filtering the full source.
4. **Simplicity:** Pre-transform filtering would require two AST passes and
   complex interaction semantics with mode transformations.

### Why priority numbers (not ordered lists)?

1. **Mergeability:** Two files can define rules with interleaved priorities
   without knowing about each other.
2. **Overridability:** Project rules can slot between user-level rules.
3. **Explicitness:** The priority number makes conflict resolution visible
   and debuggable.

### Why AND logic for combined match criteria?

When both `pattern` and `node_type` are specified, both must match. This
provides precision: match only `expression_statement` nodes that contain
a specific pattern, not all nodes matching either condition. OR logic can
be achieved by defining two separate rules.
