# Architecture Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### HIGH

**Infinite rebuild loop for non-git projects and missing git state** - `staleness.rs:153-186` + `staleness.rs:199-252`
**Confidence**: 95%
- Problem: When `read_git_head()` returns `None` (non-git project, or `.git` removed), `build_index` stores `git_head: None` in the manifest. On the next query, `check_staleness` loads the manifest, sees `stored_git_head() == None`, and returns `NoStoredHead`. `auto_refresh_if_stale` unconditionally rebuilds on `NoStoredHead`. The rebuilt manifest again stores `git_head: None`. Every query triggers a full rebuild -- an O(project_size) operation on every search.

  Additionally, `check_staleness` lines 171-178: when the manifest has a valid stored HEAD (e.g. `Some("abc123")`) but `read_git_head` returns `None` (`.git` became temporarily unreadable), the function returns `NoStoredHead` with a comment that says "treat as NoStoredHead if the manifest also has no HEAD stored" -- but that condition is not actually checked. The function unconditionally returns `NoStoredHead` regardless of whether the manifest has a stored HEAD.

- Fix: Distinguish between "no git state available" and "git state is actually stale." Add a `NonGitProject` variant to `StalenessCheck` that `auto_refresh_if_stale` treats as `Current` (no rebuild needed) when the index already exists. Alternatively, short-circuit in `check_staleness`: if `stored_git_head().is_none()` and `read_git_head().is_none()`, return `Current` -- both the stored state and current state agree that there is no git HEAD.

```rust
// In check_staleness, after line 168:
let stored = match manifest.stored_git_head() {
    Some(h) => h.to_string(),
    None => {
        // No stored HEAD. If current HEAD is also None, the index
        // was built in a non-git context and nothing has changed.
        return match read_git_head(project_root) {
            None => StalenessCheck::Current,   // non-git, nothing changed
            Some(_) => StalenessCheck::NoStoredHead, // git appeared, rebuild
        };
    }
};

let current = match read_git_head(project_root) {
    Some(h) => h,
    None => {
        // Had a stored HEAD but git is now unreadable -- treat as current
        // rather than triggering a rebuild on every query.
        return StalenessCheck::Current;
    }
};
```

---

**`FileId -> path` invariant is implicit and fragile across producer/consumer error paths** - `query.rs:70-74`, `manifest.rs:268-272`, `index.rs:339-399`
**Confidence**: 82%
- Problem: The mapping `sorted_paths()[n] == path for FileId(n)` relies on a chain of implicit guarantees: (1) `walk_metadata` sorts entries, (2) the producer iterates in that order via a FIFO channel, (3) the consumer assigns FileIds sequentially only on success, (4) the consumer inserts into the manifest only on success, (5) `sorted_paths()` sorts identically. Any future change that disrupts this chain (e.g., parallel producer, out-of-order channel, or manifest entries added from another source) silently corrupts all search results. The invariant is documented in `sorted_paths()` but has no runtime enforcement.

- Fix: Add a debug assertion in `resolve_paths_and_snippets` that validates the invariant for at least the returned results. Alternatively, store the `FileId` directly in the manifest entry during indexing, making the mapping explicit rather than derived from sort position.

```rust
// Option A: Debug assertion in resolve_paths_and_snippets
#[cfg(debug_assertions)]
{
    // Verify sorted_paths ordering matches FileId assignment
    for (i, path) in sorted_paths.iter().enumerate() {
        debug_assert!(
            i == 0 || sorted_paths[i - 1] <= path,
            "sorted_paths invariant violated at index {i}"
        );
    }
}

// Option B (preferred): Store FileId explicitly in ManifestEntry
// This makes the mapping self-describing and resilient to sort changes.
```

### MEDIUM

**`parse_flags` uses hand-rolled flag parsing instead of `clap` used by sibling `index` subcommand** - `mod.rs:116-176`
**Confidence**: 85%
- Problem: The `index` subcommand uses `clap` derive API (`IndexCli` struct in `index.rs`) for argument parsing, while the parent `search` command introduces hand-rolled flag parsing. This creates an inconsistency in error handling, help text generation, and validation. The hand-rolled parser silently ignores invalid `--limit` values (non-numeric strings) by keeping the default, whereas `clap` would report an error. Unrecognized flags (e.g. `--typo`) are silently absorbed into the query text.

- Fix: Use clap derive API for the parent search command, consistent with the index subcommand. If clap's subcommand dispatch is too rigid for this use case, at minimum add validation for unrecognized `--` flags to prevent them from leaking into query text.

```rust
// At minimum, reject unknown flags:
s if s.starts_with("--") => {
    anyhow::bail!("unrecognized flag: {s}");
}
// Positional args:
s => query_parts.push(s.to_string()),
```

---

**`check_staleness` loads the full manifest just to read `git_head`** - `staleness.rs:160-164`
**Confidence**: 80%
- Problem: `check_staleness` calls `FileManifest::load()` which reads and parses the entire manifest file (potentially tens of thousands of JSON entry lines) just to extract the `git_head` field from the header line. This is wasteful for the common case where the index is current -- the function should only need to read the first line (the header). In the query path, this manifest is then loaded a second time in `execute_query` (line 70) for path resolution.

- Fix: Add a `FileManifest::load_header_only(root, cache_dir)` method that reads just the first line and returns `ManifestHeader`. Use it in `check_staleness`. For the double-load in the query path, consider passing the already-loaded manifest from `auto_refresh_if_stale` into the query execution instead of loading it again.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Background process spawn in `install.rs` has no lifecycle management** - `install.rs:314-324`
**Confidence**: 82%
- Problem: The background index build spawned in `execute_install` (`std::process::Command::new(&exe).spawn()`) has no mechanism for: (1) detecting if a build is already running (concurrent `skim init` calls could spawn multiple builds), (2) signaling the user when the build completes, (3) cleaning up if the parent process is interrupted. The `child.id()` is printed to stderr but never recorded. If the user immediately runs `skim search "query"`, the query path will also trigger `auto_refresh_if_stale` which calls `build_index` -- potentially running two concurrent builds writing to the same cache directory.

- Fix: Use a lock file (e.g. `{cache_dir}/build.lock`) to prevent concurrent builds. `build_index` should acquire the lock at the start and release on completion. `auto_refresh_if_stale` should skip if the lock is held (another build is in progress).

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`StalenessCheck` should implement `Display` rather than relying on `Debug` formatting** - `mod.rs:271,287` (Confidence: 65%) -- `format!("{staleness_status:?}")` in the JSON output and `{staleness_status:?}` in the text output uses Debug formatting, which produces Rust-specific output like `HeadChanged { stored: "abc", current: "def" }`. User-facing output should use `Display`.

- **`query.rs` creates `SearchQuery` but does not set `fields` filter** - `query.rs:63-64` (Confidence: 60%) -- The `SearchQuery` is constructed with only `text` and `limit` set. If `SearchQuery` has a `fields` option to restrict which AST fields are searched, not setting it means all fields are searched. This may be intentional for the default case, but worth confirming the `SearchQuery` defaults are appropriate.

- **`sorted_paths()` allocates a new Vec on every call** - `manifest.rs:268-272` (Confidence: 70%) -- In the query path, `sorted_paths()` collects all keys into a Vec and sorts them. For a 50K-file project, this allocates ~50K string references. Consider caching the sorted list in the manifest after load, especially since it may be called multiple times (e.g., for stats + query in the same session).

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The module decomposition is clean -- `staleness`, `snippet`, `query`, `hooks`, and `types` each have a single clear responsibility. The streaming pipeline design with bounded channels is well-architected. The `FileId -> path` invariant is correctly maintained through the sort-order chain, though it would benefit from explicit enforcement. The primary concern is the infinite rebuild loop for non-git projects, which is a correctness issue that will manifest as severe performance degradation in production for any non-git use case. The hand-rolled flag parser is a consistency concern given the existing `clap` usage in the sibling module. After fixing the staleness loop, this is a solid addition.
