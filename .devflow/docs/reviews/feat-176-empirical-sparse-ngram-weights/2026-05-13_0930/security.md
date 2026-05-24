# Security Review Report

**Branch**: feat-176-empirical-sparse-ngram-weights -> main
**Date**: 2026-05-13T09:30

## Issues in Your Changes (BLOCKING)

### HIGH

**Path traversal via crafted repo name in `extract_repo_name`** - `crates/rskim-research/src/clone.rs:51-57`
**Confidence**: 85%
- Problem: `extract_repo_name` extracts the last path segment of the URL using `rsplit('/')` and uses it as a directory name via `self.corpus_dir.join(&repo_name)`. A URL like `https://github.com/evil/..` would produce repo name `..`, causing `corpus_dir.join("..")` to escape the corpus directory. Similarly, `https://github.com/evil/foo%2F..%2Fbar` (after any URL decoding) or names containing path separators on the target OS could write outside the intended directory. While `corpus.toml` is developer-controlled and `publish = false`, the validation in `config.rs` does not reject path-special characters in the URL path segment.
- Fix: Sanitize the extracted repo name to reject path traversal components:
```rust
fn extract_repo_name(url: &str) -> anyhow::Result<String> {
    let name = url.rsplit('/')
        .next()
        .map(|s| s.trim_end_matches(".git").to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("cannot extract repo name from URL: {url}"))?;

    // Reject path traversal components.
    if name == "." || name == ".." || name.contains('/') || name.contains('\\') {
        anyhow::bail!("unsafe repo name derived from URL: {url}");
    }
    Ok(name)
}
```

**Insufficient URL validation allows arbitrary `git clone` targets** - `crates/rskim-research/src/config.rs:49-55`
**Confidence**: 82%
- Problem: The URL validation only checks `starts_with("https://")`. This permits URLs like `https://evil.com/malicious-repo` or `https://github.com/../../etc/passwd` (after git URL resolution). While `std::process::Command` does not invoke a shell (so no shell injection), git itself supports URL schemes and redirect behaviors that could be exploited. For a developer tool with `publish = false` this is mitigated by the trust boundary being the developer editing `corpus.toml`, but defense-in-depth would restrict URLs to known hosts.
- Fix: Add a host allowlist for the corpus config, or at minimum validate the URL structure more strictly:
```rust
fn validate_repo(index: usize, repo: &RepoEntry) -> anyhow::Result<()> {
    if !repo.url.starts_with("https://github.com/") {
        bail!(
            "repos[{}]: url must be a GitHub HTTPS URL (https://github.com/...), got: {}",
            index,
            repo.url
        );
    }
    // ... rest of validation
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`clone_repo` does not disable git credential helpers or redirect following** - `crates/rskim-research/src/clone.rs:65-117`
**Confidence**: 80%
- Problem: When `git clone` runs, it inherits the user's full git configuration including credential helpers, SSH agents, and redirect policies. A malicious or compromised URL in `corpus.toml` could trigger credential prompts or leak tokens via git's credential helper chain. The `-c credential.helper=` (empty) and `-c transfer.fsckObjects=true` flags would harden the clone operation.
- Fix: Add hardening flags to the git clone commands:
```rust
std::process::Command::new("git")
    .args([
        "-c", "credential.helper=",
        "-c", "transfer.fsckObjects=true",
        "clone", "--depth", "1", url,
    ])
    .arg(dest)
    .status()
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Symlink following in `walk_and_load`** - `crates/rskim-research/src/clone.rs:122-124` (Confidence: 65%) -- The `ignore::WalkBuilder` does not explicitly disable symlink following. A malicious repo could contain symlinks pointing outside the clone directory, causing the walker to read arbitrary files on the developer's machine. Consider adding `.follow_links(false)` to the builder.

- **`remove_dir_all` on shallow clone failure without path validation** - `crates/rskim-research/src/clone.rs:91-92` (Confidence: 60%) -- If `dest` were somehow manipulated (via the path traversal in `extract_repo_name`), `remove_dir_all` could delete unintended directories. This is mitigated if the path traversal fix above is applied.

- **No NaN/Infinity check on deserialized f32 IDF values** - `crates/rskim-research/src/codegen.rs:57-65` (Confidence: 62%) -- The codegen validates `idf <= 0.0` but `f32::NAN <= 0.0` is `false`, so a NaN value in the JSON would pass validation and be written into the generated Rust source. Adding `!w.idf.is_finite()` to the check would catch this edge case.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 0 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Security Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

### Rationale

This is a developer-only research tool (`publish = false`) that clones well-known open-source repos from a checked-in `corpus.toml`. The trust boundary is the developer editing that config file, which significantly reduces the practical risk of the findings. However, the path traversal in `extract_repo_name` is a real defect that should be fixed regardless of trust assumptions -- a typo or copy-paste error in a URL could cause unintended filesystem writes. The URL validation and git credential hardening are defense-in-depth improvements appropriate for a tool that executes `git clone` on external URLs.

No hardcoded secrets, no network-facing attack surface, no user input beyond CLI args and the developer-controlled config file. The generated `weights.rs` is a static const table with no runtime security implications. The crate's clippy deny lints (`unwrap_used`, `expect_used`, `panic`) demonstrate good safety discipline.
