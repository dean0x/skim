# Security Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31

## Issues in Your Changes (BLOCKING)

### HIGH

**Code injection via `lang_to_ident` uses `debug_assert!` instead of `assert!` for identifier validation** - `crates/rskim-research/src/ast_codegen.rs:177-190`
**Confidence**: 85%
- Problem: The `lang_to_ident` function uses `debug_assert!` to validate that the generated identifier starts with an alphabetic or underscore character and contains only alphanumeric or underscore characters. `debug_assert!` is compiled away in release builds, so a crafted language name in an `ast_weights.json` file could inject arbitrary Rust code into the generated `ast_weights.rs`. While the upstream config validation (`AST_VALID_LANGUAGES` allowlist in `config.rs`) constrains language names during the `ast-run` step, the `ast-codegen` command reads from an intermediate JSON file that is not re-validated against the allowlist. A manually crafted or tampered `ast_weights.json` with a language name like `RUST; fn backdoor() { /* ... */ } //` would pass through `lang_to_ident` unchecked in release mode and produce compilable but malicious Rust source.
- Fix: Replace `debug_assert!` with `assert!` so the validation fires in all build modes. Alternatively, add an explicit `anyhow::bail!` with a descriptive error rather than panicking:
```rust
// Replace the two debug_assert! blocks with:
if !result.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false) {
    anyhow::bail!(
        "lang_to_ident produced an empty or non-identifier-starting string for input: {lang:?}"
    );
}
if !result.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
    anyhow::bail!(
        "lang_to_ident produced a non-identifier character for input: {lang:?}"
    );
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Vocabulary strings in codegen rely on `{:?}` escaping** - `crates/rskim-research/src/ast_codegen.rs:139` (Confidence: 65%) -- Node-kind strings from tree-sitter are interpolated into generated Rust via `{:?}` (Debug formatting), which produces properly escaped string literals. This is correct, but the safety depends on Rust's `Debug` implementation for `String` always producing valid string literal syntax. An explicit sanitization step or comment documenting this trust boundary would make the safety property more visible and resilient.

- **Generated `.rs` file written to arbitrary path** - `crates/rskim-research/src/ast_codegen.rs:32-51` (Confidence: 60%) -- The `generate_ast_weights_rs` function accepts an `output_path` from the caller and writes generated Rust source to it. In the CLI context, this path comes from either `--workspace-root` flag or auto-detection, both of which are under operator control. No path traversal risk in normal usage, but the function itself does not validate that the output path is within the workspace.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Security Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

### Positive Security Observations

1. **Path traversal protection** -- `extract_repo_name` in `clone.rs` validates against `.`, `..`, `/`, and `\` (pre-existing, well-tested). The new `AstGitCloneSource` reuses this via `ensure_cloned` -- no new path traversal surface.

2. **HTTPS enforcement** -- `validate_ast_repo` in `config.rs:101` requires `https://` URLs for all repos, preventing protocol downgrade attacks. This mirrors the existing lexical config validation pattern.

3. **Hardened git clone** -- The `clone_repo` function already uses `credential.helper=''` (suppress prompts) and `transfer.fsckObjects=true` (reject corrupted objects). The new AST pipeline reuses this exact code path.

4. **SHA-pinned commits** -- `ast-corpus.toml` pins all 37 repos to 40-character hex SHAs. Config validation (`validate_ast_repo`) enforces this format. This prevents supply-chain attacks via HEAD-following that would silently pick up compromised commits.

5. **Input size limits** -- `MAX_FILE_SIZE` (100 KiB), `MAX_AST_DEPTH` (500), `MAX_AST_NODES` (100K), `MAX_TRIGRAMS_PER_FILE` (50K) all provide defense-in-depth against resource exhaustion from malicious source files. These are enforced via `const` values checked at runtime.

6. **Content deduplication via SHA-256** -- Files are deduplicated by `content_hash` (SHA-256), preventing hash collision-based attacks that could skew IDF weights.

7. **Codegen uses `{:?}` for string interpolation** -- Vocabulary strings are embedded using Rust's `Debug` formatter, which properly escapes special characters. Language names in match arms use `{:?}` as well. This is the correct pattern for code generation.

8. **Vocabulary overflow protection** -- `NodeKindVocabulary::get_or_insert` uses `assert!` (not `debug_assert!`) to prevent silent ID truncation when exceeding `u16::MAX` kinds.

### Decision/Pitfall Citations

- `applies ADR-001`: The single HIGH finding should be resolved now rather than deferred. The `debug_assert!` -> `assert!` change is a one-line fix per assertion.
- `avoids PF-002`: All findings are surfaced with clear severity and actionable fixes rather than classified as deferred/pre-existing to skip resolution.
