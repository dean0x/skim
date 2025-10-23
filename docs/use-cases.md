# Use Cases

This document describes practical use cases for Skim and provides examples for each scenario.

## 1. LLM Context Optimization

### Problem

Large codebases don't fit in LLM context windows. You need to give AI enough information to understand your code without overwhelming the token limit.

### Solution

Use Skim to reduce token count by 60-90% while preserving structure.

### Examples

**Single file code review:**
```bash
skim src/app.ts | llm "Review this architecture"
```

**Entire directory analysis:**
```bash
skim src/ --no-header | llm "Analyze this codebase"
```

**Specific subdirectory:**
```bash
skim src/components/ --mode signatures | llm "Review these components"
```

**Mixed-language codebase:**
```bash
# Auto-detects TypeScript, Python, Rust, etc.
skim . --no-header | llm "Explain the architecture"
```

### Tips

- Use `--no-header` to reduce noise in LLM context
- Choose mode based on how much detail you need:
  - Structure (60% reduction): Good for architecture discussions
  - Signatures (88% reduction): Focus on APIs
  - Types (91% reduction): Focus on data structures
- Use `--show-stats` to see token savings

### Real-World Results

On an 80-file TypeScript codebase:
- Original: 63,198 tokens (won't fit in many LLM contexts)
- With structure mode: 25,119 tokens (fits comfortably)
- With signatures mode: 7,328 tokens (fits with plenty of room)

## 2. Codebase Documentation

### Problem

Generating and maintaining API documentation is time-consuming and often out of sync with code.

### Solution

Use Skim to automatically extract API surfaces from your entire codebase.

### Examples

**Generate API docs from directory:**
```bash
skim src/ --mode signatures > api-docs.txt
```

**Process specific file types:**
```bash
skim 'lib/**/*.py' --mode signatures --jobs 8 > python-api.txt
```

**Document mixed-language codebase:**
```bash
skim . --no-header --mode signatures > full-api.txt
```

**Generate type documentation:**
```bash
skim src/ --mode types > types-reference.md
```

### CI/CD Integration

Add to your CI pipeline to keep docs up-to-date:

```yaml
# .github/workflows/docs.yml
- name: Generate API docs
  run: |
    skim src/ --mode signatures --no-cache > docs/api.txt
    git add docs/api.txt
```

### Tips

- Use signatures mode for clean API reference
- Use types mode to document data structures
- Add `--no-header` for cleaner output
- Use `--jobs` to speed up large codebases

## 3. Type System Analysis

### Problem

Need to understand or analyze type definitions across a large codebase.

### Solution

Use types mode to extract only type definitions.

### Examples

**Extract all types from directory:**
```bash
skim src/ --mode types --no-header
```

**Extract types from specific files:**
```bash
skim 'src/**/*.ts' --mode types --no-header
```

**Analyze type dependencies:**
```bash
skim src/models/ --mode types | grep "interface\|type"
```

**Generate type documentation:**
```bash
skim src/ --mode types > type-reference.md
```

### Use Cases

- **Schema extraction**: Extract data models for documentation
- **Type refactoring**: Understand type dependencies before changes
- **API contract review**: Review interface definitions
- **Type coverage analysis**: See what's typed and what isn't

### Tips

- Types mode gives 90-95% token reduction
- Perfect for understanding data flow
- Works great with TypeScript, Python (typing module), Rust

## 4. Code Navigation

### Problem

Need to quickly understand large files or modules without reading all implementation details.

### Solution

Use Skim to get a high-level overview.

### Examples

**Quick overview of large file:**
```bash
skim large-file.py | less
```

**Overview of entire directory:**
```bash
skim src/auth/ | less
```

**Overview of specific module:**
```bash
skim 'src/auth/*.ts' | less
```

**Search within structure:**
```bash
skim src/ --no-header | grep "async function"
```

### Tips

- Pipe to `less` for interactive browsing
- Use with `grep` to find specific patterns
- Combine with `bat` for syntax highlighting:
  ```bash
  skim src/app.ts | bat -l typescript
  ```

## 5. Code Review

### Problem

Pull requests with large changes are hard to review - too much implementation detail obscures the important changes.

### Solution

Use Skim to focus on structural changes.

### Examples

**Review PR changes:**
```bash
git diff main HEAD | skim - --language=typescript
```

**Compare structure before/after:**
```bash
git show main:src/app.ts | skim - -l typescript > before.txt
git show HEAD:src/app.ts | skim - -l typescript > after.txt
diff before.txt after.txt
```

**Review only signatures:**
```bash
skim src/ --mode signatures --no-header > current-api.txt
```

### Tips

- Focus on what's changing, not how
- Use signatures mode for API changes
- Use types mode for schema changes
- Combine with diff tools for before/after comparison

## 6. Onboarding New Developers

### Problem

New developers need to understand codebase structure without getting lost in implementation details.

### Solution

Provide skimmed versions of the codebase for initial exploration.

### Examples

**Create onboarding documentation:**
```bash
# High-level architecture
skim src/ --mode structure > docs/architecture.md

# API reference
skim src/ --mode signatures > docs/api.md

# Type system
skim src/ --mode types > docs/types.md
```

**Interactive exploration:**
```bash
# Let new developers explore structure
skim src/ | less

# Or with syntax highlighting
skim src/ | bat -l typescript
```

### Onboarding Kit

Create a documentation kit:
```bash
mkdir onboarding
skim src/ --mode structure > onboarding/01-architecture.txt
skim src/ --mode signatures > onboarding/02-api-reference.txt
skim src/ --mode types > onboarding/03-type-system.txt
```

## 7. Architecture Discussions

### Problem

Discussing architecture changes requires shared understanding without drowning in implementation details.

### Solution

Use Skim to create architecture diagrams from code.

### Examples

**Extract current architecture:**
```bash
skim src/ --mode structure --no-header > current-architecture.txt
```

**Compare architectures:**
```bash
skim feature-branch/src/ --mode structure > feature-arch.txt
skim main/src/ --mode structure > main-arch.txt
diff main-arch.txt feature-arch.txt
```

**Focus on specific layer:**
```bash
skim src/services/ --mode signatures
```

### Tips

- Structure mode gives best overview
- Use with diff tools for comparisons
- Share output in architecture documents
- Use as basis for architecture decision records (ADRs)

## 8. Test Coverage Analysis

### Problem

Need to understand what's tested vs what's not.

### Solution

Extract signatures and compare with test files.

### Examples

**Extract all functions:**
```bash
skim src/ --mode signatures --no-header > all-functions.txt
```

**Extract tested functions:**
```bash
skim tests/ --mode signatures --no-header | grep "test_" > tested-functions.txt
```

**Find untested code:**
```bash
comm -23 <(sort all-functions.txt) <(sort tested-functions.txt)
```

## 9. Refactoring Support

### Problem

Large refactorings are risky - need to understand impact across codebase.

### Solution

Use Skim to understand structure before refactoring.

### Examples

**Before refactoring:**
```bash
skim src/ --mode structure > before-refactor.txt
```

**After refactoring:**
```bash
skim src/ --mode structure > after-refactor.txt
diff before-refactor.txt after-refactor.txt
```

**Check API compatibility:**
```bash
skim src/ --mode signatures > v1-api.txt
# After changes
skim src/ --mode signatures > v2-api.txt
diff v1-api.txt v2-api.txt  # Shows API changes
```

## 10. Multi-Language Projects

### Problem

Projects with multiple languages are hard to navigate and document.

### Solution

Skim auto-detects all languages and processes them uniformly.

### Examples

**Process mixed codebase:**
```bash
# Automatically handles .ts, .py, .rs, .go, etc.
skim src/
```

**Generate unified documentation:**
```bash
skim . --no-header --mode signatures > full-api.txt
```

**Language-specific extraction:**
```bash
# Still works - processes only Python files
skim 'src/**/*.py' --mode signatures
```

### Real Example

A project with TypeScript frontend, Python backend, and Rust utils:
```bash
$ tree src/
src/
├── frontend/  # TypeScript
├── backend/   # Python
└── utils/     # Rust

$ skim src/  # Processes all three languages
```

## Performance Tips

For all use cases:

1. **Enable caching** (default) for repeated operations
2. **Use `--jobs`** for large codebases (speeds up by 4-8x)
3. **Choose the right mode** - more aggressive = faster processing
4. **Use `--no-header`** when piping to other tools
5. **Use `--show-stats`** to verify token reduction

## Best Practices

1. **Start with structure mode** - Good balance of information and reduction
2. **Use signatures for documentation** - Clean API reference
3. **Use types for schema docs** - Focus on data structures
4. **Pipe to other tools** - Combine with grep, diff, bat, less
5. **Create documentation scripts** - Automate doc generation
6. **Version your skims** - Save structure at each release for comparison

## Integration Examples

### With `bat` (syntax highlighter)
```bash
skim src/app.ts | bat -l typescript
```

### With `fzf` (fuzzy finder)
```bash
skim src/ --no-header | fzf
```

### With `grep`
```bash
skim src/ | grep "export function"
```

### With LLM CLIs
```bash
# With llm (Simon Willison's tool)
skim src/ --no-header | llm "Explain this code"

# With aider
skim src/ --no-header | aider "Refactor this"
```

### In Scripts
```bash
#!/bin/bash
# Generate daily architecture snapshot
DATE=$(date +%Y-%m-%d)
skim src/ --mode structure > "snapshots/arch-$DATE.txt"
```

## Real-World Results

See [Performance](./performance.md) for benchmarks showing token reduction on real codebases.
