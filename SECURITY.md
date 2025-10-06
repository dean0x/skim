# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

## Security Features

Skim includes built-in protections against common security vulnerabilities:

### DoS Protection

**Stack Overflow Prevention**
- Maximum AST recursion depth: 500 levels
- Protects against deeply nested code that could cause stack overflow
- Returns clear error message when limit exceeded

**Memory Exhaustion Prevention**
- Maximum input size: 50MB
- Maximum AST nodes: 100,000 nodes
- Input size validated before parsing
- Resource limits enforced during transformation

**UTF-8 Safety**
- All string slicing validates UTF-8 boundaries
- Safe handling of multi-byte Unicode (emoji, Chinese, etc.)
- Prevents panic attacks via malformed Unicode

### Path Traversal Protection

- Rejects paths with `..` (ParentDir) components
- Rejects absolute paths starting with `/` (RootDir)
- Future-proof for planned caching features

## Known Security Considerations

### tree-sitter Version Pinning

Skim is currently pinned to tree-sitter 0.23.x due to grammar compatibility:

```toml
tree-sitter = "0.23"  # NOT 0.24+ (ABI incompatibility)
```

**Security implication**: We may lag behind upstream security patches until grammar ecosystem upgrades.

**Mitigation**: We actively monitor tree-sitter security advisories and will upgrade as soon as grammar support is available.

### Resource Limits

Current limits are conservative defaults:

| Limit | Value | Rationale |
|-------|-------|-----------|
| Max input size | 50MB | Prevents memory exhaustion |
| Max AST depth | 500 levels | Prevents stack overflow |
| Max AST nodes | 100,000 | Prevents memory exhaustion |

**If you need higher limits**: Please open an issue to discuss your use case. We may make these configurable in future versions.

## Reporting a Vulnerability

**Please DO NOT open public issues for security vulnerabilities.**

Instead, please report security issues by emailing:

**security@[your-domain].com** (or create a private security advisory on GitHub)

### What to Include

1. **Description** of the vulnerability
2. **Steps to reproduce** (minimal example)
3. **Impact assessment** (what can an attacker do?)
4. **Suggested fix** (if you have one)

### Response Timeline

- **Initial response**: Within 48 hours
- **Triage and assessment**: Within 1 week
- **Fix timeline**: Depends on severity
  - Critical: Within 7 days
  - High: Within 14 days
  - Medium: Within 30 days
  - Low: Next release cycle

### Disclosure Policy

- We will acknowledge your report within 48 hours
- We will provide regular updates on our progress
- We will notify you when the vulnerability is fixed
- We will credit you in the security advisory (unless you prefer anonymity)
- We follow **coordinated disclosure**: we will not disclose the vulnerability until a fix is available

## Security Best Practices for Users

### When Using Skim

1. **Validate input sources**
   - Don't pass untrusted files without validation
   - Be cautious with files from untrusted repositories

2. **Resource limits**
   - Current limits (50MB, 500 depth, 100k nodes) should handle normal code
   - If you hit these limits with legitimate code, please report it

3. **Output validation**
   - Skim preserves structure but may not preserve all semantics
   - Don't execute transformed output without review

### When Integrating Skim

1. **Subprocess usage**
   ```rust
   // ✅ GOOD: Set timeout and resource limits
   Command::new("skim")
       .arg("file.ts")
       .timeout(Duration::from_secs(30))
       .spawn()?;
   ```

2. **Library usage**
   ```rust
   // ✅ GOOD: Handle errors explicitly
   match transform(&source, language, mode) {
       Ok(result) => process(result),
       Err(e) => handle_error(e), // Don't ignore errors
   }
   ```

3. **Don't disable safety features**
   - Don't patch out resource limits
   - Don't catch and ignore DoS protection errors

## Security Audit History

| Date | Type | Findings | Status |
|------|------|----------|--------|
| 2025-10-05 | Internal pre-PR review | 4 critical DoS vulnerabilities | ✅ Fixed in a5f3146 |
| 2025-10-05 | Architecture review | 2 critical duplications | ✅ Fixed in b91974c |

### Details: 2025-10-05 Security Fixes

**Fixed in commit a5f3146**:

1. ✅ Stack overflow DoS (CVSS 7.5)
   - Added MAX_AST_DEPTH limit

2. ✅ UTF-8 boundary panic DoS (CVSS 7.5)
   - Added `is_char_boundary()` validation

3. ✅ Memory exhaustion DoS (CVSS 7.5)
   - Added MAX_INPUT_SIZE and MAX_AST_NODES limits

4. ✅ Path traversal (CVSS 4.3)
   - Added path component validation

See `.docs/FIXES_APPLIED.md` for full details.

## Scope

### In Scope

- Denial of Service vulnerabilities
- Memory safety issues
- Path traversal vulnerabilities
- Input validation bypasses
- Resource exhaustion attacks

### Out of Scope

- Issues in dependencies (report to upstream)
- Social engineering
- Physical attacks
- Theoretical vulnerabilities without practical exploit

## Contact

For non-security issues, please use:
- GitHub Issues: https://github.com/dean0x/skim/issues
- Discussions: https://github.com/dean0x/skim/discussions

For security issues, use the private reporting method above.

---

**Security is a priority.** We take all reports seriously and appreciate responsible disclosure.
