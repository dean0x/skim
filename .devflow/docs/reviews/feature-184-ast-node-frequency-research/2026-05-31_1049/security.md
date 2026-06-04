# Security Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`debug_assert` is not enforced in release builds for u16 vocabulary overflow** - `crates/rskim-research/src/ast_types.rs:157-162`
**Confidence**: 90%
- Problem: `NodeKindVocabulary::get_or_insert` uses `debug_assert!` to guard against exceeding `u16::MAX` node kinds. In release builds, `debug_assert!` is compiled out entirely. If a pathological or very large corpus produces more than 65,535 distinct node kinds, the `as NodeKindId` cast at line 162 silently wraps around to 0, causing ID collisions. Collisions corrupt the bigram/trigram DF maps, producing incorrect IDF weights without any error.
- Fix: Replace `debug_assert!` with an explicit check that returns an error or panics in both debug and release. Since `get_or_insert` returns `NodeKindId` (not `Result`), the cleanest approach is to use `assert!` instead of `debug_assert!`, or change the return type to `Result<NodeKindId, ...>`. Given that tree-sitter grammars produce O(100) kinds per language, the practical risk is very low, but the fix is trivial and aligns with the project's reliability principle of asserting invariants at module boundaries (`applies ADR-001` -- fix now rather than defer).
```rust
// Replace debug_assert! with assert! (or a Result return):
assert!(
    self.id_to_kind.len() < usize::from(NodeKindId::MAX),
    "NodeKindVocabulary overflow: {} kinds exceeds u16::MAX",
    self.id_to_kind.len()
);
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Code generation injects node-kind strings into Rust comments without sanitization** - `crates/rskim-research/src/ast_codegen.rs:189-190`
**Confidence**: 80%
- Problem: In `write_language_bigram_arrays` and `write_language_trigram_arrays`, the `parent_kind` and `child_kind` strings from the vocabulary (which originate from tree-sitter grammar node types) are interpolated directly into Rust line comments via format strings (`// {} -> {}`). While tree-sitter grammar node kinds are typically simple ASCII identifiers, a maliciously crafted or buggy grammar could theoretically produce a kind string containing a newline character, which would break out of the comment and inject arbitrary Rust source code into the generated `ast_weights.rs` file. The file is then compiled into the `rskim-search` binary.
- Mitigating factors: (1) The vocabulary comes from tree-sitter grammar definitions shipped as cargo dependencies, not from user input. (2) The `ast-corpus.toml` config only accepts known language names validated against `AST_VALID_LANGUAGES`. (3) This is a developer-only research binary (`publish=false`), not a user-facing tool. (4) The `{:?}` formatting used for vocabulary strings in `write_vocabulary` (line 139) does properly escape, so the vocabulary array itself is safe.
- Fix: Replace raw `{}` formatting with `{:?}` (debug formatting, which escapes newlines and special characters) for the comment strings, or strip/replace newlines:
```rust
// In write_language_bigram_arrays and write_language_trigram_arrays:
writeln!(
    buf,
    "    (0x{:08X}, {:.6}_f32), // {:?} -> {:?}",
    w.bigram, w.idf, w.parent_kind, w.child_kind
)?;
```

**`lang_to_ident` does not validate that the result is a valid Rust identifier** - `crates/rskim-research/src/ast_codegen.rs:153-164`
**Confidence**: 80%
- Problem: The `lang_to_ident` function converts language names to Rust constant name fragments by uppercasing and replacing special characters. It is then interpolated into `pub const {ident}_AST_BIGRAM_WEIGHTS` (line 184). However, it does not verify the output starts with a letter or underscore, nor that it contains only valid identifier characters. If a language name contained unexpected characters (e.g., digits at the start, or Unicode), the generated Rust source would fail to compile. While compile failure is not a vulnerability per se, in a code generation context, insufficient output validation is a defense-in-depth gap.
- Mitigating factors: Language names are validated against `AST_VALID_LANGUAGES` which are all safe ASCII strings. The risk is only relevant if `lang_to_ident` is used outside the AST corpus pipeline with unchecked input.
- Fix: Add a post-condition check:
```rust
fn lang_to_ident(lang: &str) -> String {
    let ident = lang.chars()
        .map(|c| match c {
            '+' | '#' | '-' | ' ' => '_',
            _ => c.to_ascii_uppercase(),
        })
        .collect::<String>()
        .split("__")
        .collect::<Vec<_>>()
        .join("_");
    debug_assert!(
        ident.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'),
        "lang_to_ident produced invalid identifier: {ident:?}"
    );
    ident
}
```

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`checkout` commands in `clone_repo` bypass the subprocess timeout** - `crates/rskim-research/src/clone.rs:237-249, 269`
**Confidence**: 85%
- Problem: The `clone_repo` function correctly uses `git_run_with_timeout` for the `git clone` commands (with 300s SIGKILL timeout). However, the subsequent `git cat-file` and `git checkout` commands at lines 237-248 and 269+ use bare `std::process::Command::new("git")` with `.status()`, bypassing the timeout mechanism entirely. A malicious or corrupt repository could cause these commands to hang indefinitely. This is pre-existing code not modified in this PR.
- This is a defense-in-depth concern: the cloned repository is fetched from HTTPS-only validated URLs (public GitHub repos), so the practical risk is low.

## Suggestions (Lower Confidence)

- **`files.len() as u32` truncation** - `crates/rskim-research/src/ast_extract.rs:220` (Confidence: 65%) -- If the corpus contains more than 4 billion files, the `u32` cast silently truncates. Practically impossible for this tool, but a `u32::try_from(files.len())` would be more defensive.

- **`node_count` is `u32` compared against `MAX_AST_NODES as u32`** - `crates/rskim-research/src/ast_extract.rs:128` (Confidence: 65%) -- The constant `MAX_AST_NODES` is `usize` (100,000) which fits in u32 on all platforms, but the explicit `as u32` cast on a `usize` constant is a minor code smell that could mask issues if `MAX_AST_NODES` were ever increased above `u32::MAX`.

## Security Posture Assessment

The PR demonstrates strong security awareness throughout:

1. **HTTPS-only URL validation** (`config.rs:101`) prevents git:// and file:// protocol injection via TOML config.
2. **Path traversal protection** via `extract_repo_name` (reused by `AstGitCloneSource`).
3. **Subprocess timeout with SIGKILL** (300s deadline) prevents clone hangs.
4. **Hardened git flags** (`credential.helper=`, `transfer.fsckObjects=true`) in `clone_repo`.
5. **File size limits** (100 KiB `MAX_FILE_SIZE`), AST depth limits (500), node count limits (100K), and trigram caps (50K per file) provide defense against resource exhaustion from pathological inputs.
6. **SHA-256 content deduplication** prevents duplicate processing.
7. **Binary file detection** (null byte probe) prevents non-text processing.
8. **Language allowlist** (`AST_VALID_LANGUAGES`) restricts which tree-sitter parsers are invoked.
9. **Code generation validation** (`validate_ast_table`) rejects zero version, empty vocabulary, and non-finite/non-positive IDF values before generating Rust source.
10. **Developer-only binary** (`publish=false`) -- not distributed to end users, reducing attack surface.

The decisions context was reviewed: `ADR-001` (fix all noticed issues immediately) applies to the blocking finding. `PF-002` (do not classify findings as pre-existing to skip them) -- the pre-existing finding is informational per the review methodology's iron law but surfaced for the user's decision per the pitfall (`avoids PF-002`).

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Security Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The single HIGH blocking finding (`debug_assert` u16 overflow guard compiled out in release builds) requires a one-line fix (replace with `assert!`). The two MEDIUM should-fix items in code generation are defense-in-depth improvements for comment/identifier sanitization. Overall security posture is strong with comprehensive input validation, resource limits, and subprocess hardening already in place.
