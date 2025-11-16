# Security

Skim includes built-in protections against denial-of-service (DoS) attacks and malicious input.

## Built-in Protections

### Max Recursion Depth

**Limit**: 500 levels

**Purpose**: Prevents stack overflow on deeply nested code

**Example of protected input:**
```typescript
// Extremely nested code (>500 levels)
function a() {
  function b() {
    function c() {
      // ... 500+ levels deep
    }
  }
}
```

**Error message:**
```
Error: Recursion depth exceeded (max: 500 levels)
```

### Max Input Size

**Limit**: 50MB per file

**Purpose**: Prevents memory exhaustion

**Error message:**
```
Error: File too large: 52,428,800 bytes exceeds maximum of 52,428,800 bytes (50MB)
```

**Rationale**: Files larger than 50MB are unusual and may indicate:
- Minified/bundled code (should be processed before skimming)
- Generated code (consider processing source instead)
- Malicious input attempting memory exhaustion

### Max AST Nodes

**Limit**: 100,000 nodes

**Purpose**: Prevents memory exhaustion from pathological code

**Example of protected input:**
```typescript
// Code with 100,000+ AST nodes
const x = [[[[[[...]]]]]]  // Extremely deep nesting
```

### Max JSON Nesting Depth

**Limit**: 500 levels (serde_json enforces 128 by default)

**Purpose**: Prevents stack overflow on deeply nested JSON objects

**Example of protected input:**
```json
{
  "level1": {
    "level2": {
      "level3": {
        // ... 500+ levels deep
      }
    }
  }
}
```

**Error message:**
```
Error: JSON nesting depth exceeded: 501 (max: 500). Possible malicious input.
```

**Note**: serde_json has a default recursion limit of 128, which provides primary protection. Our 500-level limit provides a secondary validation layer for consistency with other transformers.

### Max JSON Keys

**Limit**: 10,000 keys per file

**Purpose**: Prevents memory exhaustion from JSON with millions of keys

**Example of protected input:**
```json
{
  "key0": "value",
  "key1": "value",
  // ... 10,000+ keys total across all nested objects
}
```

**Error message:**
```
Error: JSON key count exceeded: 10001 (max: 10000). Possible malicious input.
```

**Rationale**: Processing JSON with millions of keys could exhaust memory. The 10,000 key limit matches the MAX_SIGNATURES limit used in other transformers, ensuring consistent protection.

### UTF-8 Validation

**Protection**: Safe handling of multi-byte Unicode characters

**Prevents**:
- Invalid UTF-8 sequences
- Buffer overruns
- Character encoding attacks

All input is validated as UTF-8 before processing.

### Path Traversal Protection

**Protection**: Rejects malicious file paths

**Blocked patterns:**
- `../../../etc/passwd` - Parent directory traversal
- Absolute paths in glob patterns (when security matters)
- Symlinks in directory processing

**Example:**
```bash
# Blocked for security
$ skim "../../../etc/passwd"
Error: Path traversal detected

# Blocked in glob mode
$ skim "/etc/*.conf"
Error: Glob pattern must be relative (cannot start with '/')
```

## Sandboxing

### No Network Access

Skim never makes network requests. All processing is local.

### No Code Execution

Skim only **parses** code, never executes it. Source code is treated as data, not instructions.

### Read-Only by Default

By default, Skim only reads files. Cache writes are the only file modifications, and they're:
- In user's cache directory only
- JSON format (not executable)
- Atomic (prevents partial writes)

## Vulnerability Disclosure

If you discover a security vulnerability, please:

1. **Do not** open a public GitHub issue
2. Email security details to the maintainers (see SECURITY.md)
3. Allow time for a fix before public disclosure

See [SECURITY.md](../SECURITY.md) for the full disclosure process.

## Security Best Practices

### When Processing Untrusted Code

If processing code from untrusted sources:

1. **Validate file size** - Check files aren't larger than expected
2. **Use `--no-cache`** - Avoid caching untrusted content
3. **Run in container** - Isolate Skim process
4. **Set resource limits** - Use ulimit or container limits

```bash
# Process untrusted code safely
ulimit -m 1000000  # 1GB memory limit
skim untrusted.ts --no-cache
```

### In CI/CD Pipelines

1. **Pin Skim version** - Don't use `latest`
2. **Use `--no-cache`** - Avoid cache poisoning
3. **Validate inputs** - Check file sizes before processing
4. **Set timeouts** - Limit processing time

```yaml
# GitHub Actions example
- name: Process code
  run: |
    # Timeout after 5 minutes
    timeout 300 skim src/ --no-cache > docs/api.txt
```

### Glob Pattern Security

When accepting glob patterns from users:

1. **Validate patterns** - Reject patterns starting with `/` or containing `..`
2. **Limit scope** - Restrict to specific directories
3. **Use allowlists** - Only allow known-good patterns

```bash
# Good: Relative pattern
skim "src/**/*.ts"

# Bad: Absolute pattern (rejected)
skim "/etc/**/*.conf"

# Bad: Parent traversal (rejected)
skim "../../../*.ts"
```

## Security Considerations

### Tree-sitter Security

Skim uses tree-sitter for parsing. Tree-sitter:
- ✅ Memory-safe (written in C with safety checks)
- ✅ Does not execute code
- ✅ Handles malformed input gracefully
- ✅ Well-tested on billions of lines of code (GitHub uses it)

### Rust Security

Skim is written in Rust, which provides:
- ✅ Memory safety without garbage collection
- ✅ No buffer overflows
- ✅ No null pointer dereferences
- ✅ Thread safety

### Dependencies

Skim has minimal dependencies. All dependencies are:
- Vetted for security issues
- Regularly updated
- From trusted sources (crates.io, npm)

## Threat Model

### In Scope

Skim protects against:
- ✅ Malicious code causing crashes (DoS)
- ✅ Malicious code causing memory exhaustion
- ✅ Path traversal attacks
- ✅ Malformed UTF-8

### Out of Scope

Skim does NOT protect against:
- ❌ Viewing sensitive code (Skim outputs what you give it)
- ❌ Information disclosure (you control the output)
- ❌ Supply chain attacks on dependencies (use cargo-audit)

## Security Testing

Skim includes security tests for:
- Maximum input size enforcement
- Path traversal rejection
- Symlink rejection
- Glob pattern validation

Run security tests:
```bash
cargo test --test security
```

## Reporting Security Issues

See [SECURITY.md](../SECURITY.md) for:
- Contact information
- Response timeline
- Disclosure policy
- Security policy

## Security Changelog

Security fixes are noted in [CHANGELOG.md](../CHANGELOG.md) with `[SECURITY]` prefix.

## Future Security Enhancements

Potential improvements (not yet implemented):
- Process isolation (sandbox)
- Resource limits (CPU time, memory)
- Content security policy for cache
- Audit logging for security events

See [GitHub issues](https://github.com/dean0x/skim/issues) for security enhancement requests.
