# Rust Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### HIGH

**Misleading staleness comment and incorrect return variant** - `staleness.rs:174-178`
**Confidence**: 85%
- Problem: When the manifest HAS a stored HEAD (line 166 returns `stored`), but `read_git_head` returns `None` (git unreadable), the code returns `StalenessCheck::NoStoredHead`. The comment says "treat as NoStoredHead if the manifest also has no HEAD stored" — but at this point the manifest DOES have a stored HEAD. This is logically inconsistent. The stored HEAD was verified non-None on line 166, yet the code pretends there is no stored HEAD. This masks a real state: "we had a HEAD, but we can't read the current one."
- Fix: Either add a new variant (e.g., `StalenessCheck::CurrentHeadUnreadable`) or change the return to `HeadChanged` to trigger a rebuild, since the safe assumption when git state is unreadable is that the index may be stale. At minimum, fix the comment.

```rust
// Option A: explicit variant
StalenessCheck::CurrentHeadUnreadable

// Option B: treat as stale to be safe
return StalenessCheck::HeadChanged {
    stored,
    current: "(unreadable)".to_string(),
};
```

**`lines.len() as u32` truncating cast in `extract_context_window`** - `snippet.rs:67`
**Confidence**: 82%
- Problem: `lines.len()` is `usize`. On 64-bit systems, a file with more than `u32::MAX` (4.3 billion) lines would silently truncate. While this is practically unlikely for source code, the project's CLAUDE.md mentions a 5 MB file size cap. With an average line length of ~1 byte (adversarial input), this is ~5M lines -- well within `u32`. However, the cast is still a silent truncation and violates Rust's "make illegal states unrepresentable" principle.
- Fix: Use `u32::try_from(lines.len()).unwrap_or(u32::MAX)` or add a `min` guard. Given the 5 MB cap, this is defensive but correct.

```rust
let total_lines = u32::try_from(lines.len()).unwrap_or(u32::MAX);
```

**Potential `u32` addition overflow in `extract_context_window`** - `snippet.rs:77`
**Confidence**: 80%
- Problem: `match_line + context` can overflow if `match_line` is near `u32::MAX`. While practically impossible with real source files, the function accepts arbitrary `u32` inputs and does not guard against it. The `saturating_sub` on line 76 shows awareness of this pattern, but line 77 does not use `saturating_add`.
- Fix: Use `saturating_add` for consistency and correctness:

```rust
let end = match_line.saturating_add(context).min(total_lines);
```

### MEDIUM

**`is_hex_sha` only accepts 40-char SHA-1, not SHA-256** - `staleness.rs:135-137`
**Confidence**: 85%
- Problem: Git is migrating to SHA-256 (64-char hashes). Repos using `extensions.objectFormat = sha256` will produce 64-char hex strings. `is_hex_sha` will reject them, causing `read_git_head` to return `None`, which means staleness detection silently degrades to `NoStoredHead` (perpetual rebuilds).
- Fix: Accept both 40-char and 64-char hex strings:

```rust
fn is_hex_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.chars().all(|c| c.is_ascii_hexdigit())
}
```

**Orphaned background process — `Child` dropped without `wait`** - `install.rs:316-323`
**Confidence**: 82%
- Problem: `std::process::Command::spawn()` returns a `Child`. When `child` is dropped at the end of the `if let` block, Rust does NOT wait for the child or kill it — but on Unix the child becomes an orphan (reparented to init/launchd). The child itself runs fine, but this means: (1) no cleanup if the parent crashes during the spawn's lifetime, and (2) more importantly, the `child` variable is only used for `.id()` and immediately dropped, which is a potential Clippy `let_underscore_must_use` concern in future.
- Fix: This is intentional fire-and-forget, which is fine. Add a brief comment acknowledging the intentional drop to prevent future "fix" attempts:

```rust
// `child` is intentionally dropped without wait — fire-and-forget background build.
```

**Unrecognized flags silently treated as query text** - `mod.rs:158-159`
**Confidence**: 85%
- Problem: `parse_flags` puts any unrecognized string into `query_parts`, including typos like `--buld` or `--jsno`. This means `skim search --buld` will try to search for "--buld" rather than giving a helpful error.
- Fix: Add a check for strings starting with `--` that don't match any known flag, and emit a warning to stderr:

```rust
s if s.starts_with("--") => {
    eprintln!("skim search: unrecognized flag {:?}, treating as query text", s);
    query_parts.push(s.to_string());
}
s => query_parts.push(s.to_string()),
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`config.text.clone()` called three times in `execute_query`** - `query.rs:43,63,80`
**Confidence**: 80%
- Problem: `config.text` is cloned at lines 43, 63, and 80. The `SearchQuery::new` takes ownership (line 63), so that clone is necessary. But lines 43 and 80 could share a reference since they're only reading. This is a minor unnecessary allocation pattern, not idiomatic zero-cost Rust.
- Fix: For the early-return path (line 43), clone is required since we return an owned `QueryOutput`. For line 80, the clone is also needed since `QueryOutput` owns the string. This is acceptable — no change needed, but consider whether `QueryOutput` should borrow from `config` instead.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`write_hook_atomic` temp file name collision** - `hooks.rs:176` (Confidence: 65%) — The temp path is always `<hook>.tmp`. If two processes install hooks concurrently, they race on the same temp file. Consider using `tempfile::NamedTempFile` for true atomicity, or add a PID/random suffix.

- **`sorted_paths()` rebuilds sort on every query** - `query.rs:71` (Confidence: 70%) — `manifest.sorted_paths()` allocates and sorts on every call. If this becomes a hot path, consider caching the sorted order inside `FileManifest` or computing it at load time.

- **`extract_context_window` collects all lines** - `snippet.rs:66` (Confidence: 65%) — `content.lines().collect()` allocates a `Vec` of all line references just to extract 7 lines (3 context + 1 match + 3 context). For large files, a streaming approach that counts newlines and extracts only the target window would be more efficient.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 3 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code demonstrates strong Rust idioms overall: proper use of `Result` and `?` propagation throughout, well-designed enums for state machines (`StalenessCheck`, `SnippetOutcome`), good use of borrowing (functions accept `&Path` and `&str`), no unsafe code, and comprehensive test coverage across all new modules. The `#[cfg(test)] #[path = "..."]` pattern for co-located tests is clean.

Conditions for approval:
1. Fix the `saturating_add` on `snippet.rs:77` (trivial, prevents theoretical overflow).
2. Fix or acknowledge the misleading comment in `staleness.rs:174-178` about the return value when git HEAD is unreadable but the manifest HAS a stored HEAD.
3. Consider the SHA-256 future-proofing in `is_hex_sha` — not urgent but prevents a silent degradation as git transitions.
