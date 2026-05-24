# Code Review Summary

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19_1136

## Merge Recommendation: CHANGES_REQUESTED

**Blocking issues in your changes require resolution before merge.** The primary concerns are an infinite rebuild loop for non-git projects (architecture/regression), unguarded memory allocation on snippet extraction (reliability), and missing concurrent build protection (reliability). All are correctness/robustness issues that will manifest in production. The remaining HIGH findings are consistency and test coverage gaps that improve maintainability.

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| **Blocking** | 0 | 6 | 4 | 0 |
| **Should Fix** | 0 | 0 | 4 | 0 |
| **Pre-existing** | 0 | 0 | 0 | 0 |

**Total Issues**: 18 across 9 focus areas
**Confidence >= 80%**: 15 issues
**Recommendation**: 6 issues block merge; 4 should be fixed together with changes; 8 informational

---

## Blocking Issues (Must Fix Before Merge)

### 1. Infinite Rebuild Loop for Non-Git Projects
**Files**: `staleness.rs:153-186, 199-252`  
**Confidence**: 95% (architecture) + 82% (regression) = **99% (deduplicated, 2 reviewers)**
**Severity**: HIGH

**Problem**: When `read_git_head()` returns `None` (non-git project), `build_index` stores `git_head: None`. On the next query, `check_staleness` loads the manifest, sees `stored_git_head() == None`, and returns `NoStoredHead`. `auto_refresh_if_stale` unconditionally rebuilds on `NoStoredHead`. The rebuilt manifest again stores `git_head: None`. Every query triggers a full O(project_size) rebuild on non-git projects — a severe performance regression.

**Fix**: Distinguish between "no git state available" and "git state is actually stale." In `check_staleness`, after loading the stored HEAD:
```rust
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

### 2. Unguarded Memory Allocation on Snippet Extraction
**File**: `snippet.rs:127`  
**Confidence**: 90%
**Severity**: HIGH

**Problem**: `extract_snippet` reads the entire file into memory via `std::fs::read(&abs_path)` without any size check. The index build pipeline enforces a 5 MB cap, but snippet extraction at query time bypasses this. A file that grew substantially since indexing could exhaust available memory. With the default limit of 20 results, up to 20 large files could be read simultaneously.

**Fix**: Add a metadata size check before reading:
```rust
const MAX_SNIPPET_FILE_BYTES: u64 = 5 * 1024 * 1024;
let meta = match std::fs::metadata(&abs_path) {
    Ok(m) => m,
    Err(_) => return SnippetOutcome::Unavailable,
};
if meta.len() > MAX_SNIPPET_FILE_BYTES {
    return SnippetOutcome::Unavailable;
}
let content = std::fs::read_to_string(&abs_path)?;
```

---

### 3. No Concurrent Build Protection (Race Condition)
**Files**: `install.rs:316`, `staleness.rs:199`  
**Confidence**: 85%
**Severity**: HIGH

**Problem**: `execute_install` spawns a background `skim search --build`. Separately, `auto_refresh_if_stale` (called on every query) can also trigger `build_index`. Git hooks run `skim search --update` which also calls `auto_refresh_if_stale`. There is no file lock or coordination mechanism. Two concurrent builds can corrupt the index by racing on writes to the same `index.skidx` and `index.skfiles` files.

**Fix**: Add an advisory file lock before building:
```rust
use fs2::FileExt;

let lock_path = cache_dir.join(".skim-build.lock");
let lock_file = File::create(&lock_path)?;
match lock_file.try_lock_exclusive() {
    Ok(_) => {
        // proceed with build, lock is released on drop
    }
    Err(_) => {
        // another build is in progress
        eprintln!("skim search: another build is in progress, skipping");
        return Ok(false);
    }
}
```

---

### 4. Duplicate Manifest Load on Query Hot Path
**Files**: `query.rs:55, 70`  
**Confidence**: 92%
**Severity**: HIGH

**Problem**: `execute_query` calls `auto_refresh_if_stale()` which calls `FileManifest::load()`, then `execute_query` calls `FileManifest::load()` again to build sorted paths. This parses the entire JSONL manifest file twice in a single query. For a 10K-file manifest, this doubles the parse cost.

**Fix**: Have `auto_refresh_if_stale` return the loaded manifest:
```rust
pub(super) fn auto_refresh_if_stale(
    root: &Path,
    cache_dir: &Path,
) -> anyhow::Result<(bool, FileManifest)> {
    // ... check staleness ...
    let manifest = FileManifest::load(...)?;
    // On Current: return (false, manifest)
    // On rebuild: reload after build, return (true, fresh_manifest)
}

// In execute_query:
let (rebuilt, manifest) = auto_refresh_if_stale(...)?;
// reuse manifest, skip second load at line 70
```

---

### 5. Inconsistent Flag Parser Return Type
**File**: `mod.rs:116`  
**Confidence**: 85%
**Severity**: HIGH

**Problem**: Both `init::flags::parse_flags` and `heatmap::args::parse_args` return `anyhow::Result<T>`. The new `search::parse_flags` returns bare `Flags`, silently ignoring invalid `--limit` values and missing `--root` values. This deviates from codebase conventions.

**Fix**: Return `anyhow::Result<Flags>`:
```rust
fn parse_flags(args: &[String]) -> anyhow::Result<Flags> {
    // ...
    "--limit" | "-n" => {
        i += 1;
        let n = args.get(i)
            .ok_or_else(|| anyhow::anyhow!("--limit requires a value"))?
            .parse::<usize>()
            .map_err(|_| anyhow::anyhow!("--limit requires a positive integer"))?;
        limit = n;
    }
    // ...
    Ok(Flags { ... })
}
```

---

### 6. Missing Edge-Case Test for Behavior Change
**File**: `mod.rs:84`  
**Confidence**: 95%
**Severity**: HIGH

**Problem**: The behavior of `skim search <query>` changed from "not yet implemented" (ExitCode::FAILURE) to query execution. The old test was deleted. No test validates that query execution in a directory with no `.git` and no index returns successfully without panicking or producing confusing errors.

**Fix**: Add a test:
```rust
#[test]
fn test_query_in_non_git_dir_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    let config = QueryConfig {
        text: "anything".to_string(),
        limit: 5,
        json: false,
        root: dir.path().to_path_buf(),
        cache_dir: dir.path().join("cache"),
    };
    std::fs::create_dir_all(&config.cache_dir).unwrap();
    // Should not panic; either Ok with 0 results or a graceful Err
    let result = execute_query(&config, &TEST_ANALYTICS);
    assert!(result.is_ok() || matches!(result, Err(_)));
}
```

---

## Should-Fix Issues (Fix While Touching)

### 1. Misleading Comment on Staleness Return Value
**File**: `staleness.rs:174-178`  
**Confidence**: 85%
**Severity**: HIGH

**Problem**: When the manifest HAS a stored HEAD but `read_git_head` returns `None` (git unreadable), the code returns `NoStoredHead`. The comment says "treat as NoStoredHead if the manifest also has no HEAD stored" — but the manifest DOES have a stored HEAD. This is logically inconsistent.

**Fix**: Add a new variant or clarify the logic:
```rust
// Option: treat as stale to be safe when git is unreadable
if read_git_head(project_root).is_none() {
    return StalenessCheck::HeadChanged {
        stored,
        current: "(unreadable)".to_string(),
    };
}
```

---

### 2. Unguarded Addition Overflow in Snippet Context Window
**File**: `snippet.rs:77`  
**Confidence**: 80%
**Severity**: HIGH

**Problem**: `match_line + context` can theoretically overflow if `match_line` is near `u32::MAX`. While practically impossible with real source files, the function accepts arbitrary `u32` inputs without guards. Line 76 uses `saturating_sub` showing awareness of this pattern.

**Fix**: Use `saturating_add`:
```rust
let end = match_line.saturating_add(context).min(total_lines);
```

---

### 3. Truncating `usize` to `u32` Without Check
**File**: `snippet.rs:67`  
**Confidence**: 82% (rust) + 80% (reliability) = **94% (deduplicated, 2 reviewers)**
**Severity**: MEDIUM

**Problem**: `lines.len() as u32` silently truncates if line count exceeds `u32::MAX`. While the 5 MB file size cap makes this practically unlikely, snippet extraction has no size cap (if Fix #2 above is not applied). A very large file could produce a truncated `total_lines` value.

**Fix**: Use `try_from` with a fallback:
```rust
let total_lines = u32::try_from(lines.len()).unwrap_or(u32::MAX);
```

---

### 4. SHA-256 Object Format Not Supported
**Files**: `staleness.rs:135-137`  
**Confidence**: 85% (rust) + 82% (reliability) + 70% (security) = **98% (deduplicated, 3 reviewers)**
**Severity**: MEDIUM

**Problem**: `is_hex_sha` accepts only 40-character hex strings (SHA-1). Git repositories using SHA-256 object format (enabled via `extensions.objectFormat = sha256`) produce 64-character hashes. Such repositories would fail staleness checks silently, causing perpetual rebuilds (treating index as having `NoStoredHead`).

**Fix**: Accept both SHA-1 and SHA-256:
```rust
fn is_hex_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.chars().all(|c| c.is_ascii_hexdigit())
}
```

---

## Test Coverage Gaps

### Critical Test Gaps (HIGH)

**1. Missing test for `auto_refresh_if_stale`** - `staleness.rs:199`  
**Confidence**: 85%  
The central orchestrator between staleness detection and index rebuild has no direct unit test. The `HeadChanged` and `NoStoredHead` refresh branches are never tested.

**Fix**: Add tests for the three main branches:
- Set up a built index, change HEAD, verify rebuild (returns `Ok(true)`)
- Set up no stored HEAD in manifest, verify rebuild is triggered
- Set up matching HEAD, verify no refresh (returns `Ok(false)`)

**2. Missing error-path test for `execute_query` with corrupt index** - `query.rs:58`  
**Confidence**: 82%  
No test verifies graceful error handling when the index file is corrupt.

**Fix**: Create a test with garbage bytes in `index.skidx` and assert `execute_query` returns `Err`.

**3. Missing tests for `parse_flags` argument parsing** - `mod.rs:116-176`  
**Confidence**: 85%  
The `--root`, `-n` short flag, and combined flag scenarios are untested. Since `--root` is used by every subcommand, a regression would break all of them.

**Fix**: Add tests for:
- `--root /tmp/proj`
- `--root=/tmp/proj`
- `-n 3` (short form)
- Combined flags: `--json --limit 5 query`

---

### Important Test Gaps (MEDIUM)

- **`byte_offset_to_line` out-of-bounds test** - `snippet.rs:42-49` (Confidence: 82%)
- **`format_text_output` with stale results** - `query.rs:146` (Confidence: 80%)
- **`test_format_text_output_empty_results` weak assertion** - `query_tests.rs:146-149` (Confidence: 83%)

---

## Consistency Issues (Should Fix)

### 1. Hand-Rolled Flag Parser vs. Established Patterns
**File**: `mod.rs:116-176`  
**Confidence**: 85%  
The sibling `index.rs` uses `clap::Parser`. Introducing hand-rolled parsing creates inconsistency in error handling and help text generation. Unknown flags are silently absorbed into query text instead of being rejected.

**Recommended**: Use clap derive API or at minimum validate unknown `--` flags.

### 2. Debug Formatting for User-Facing Output
**Files**: `mod.rs:271, 287`  
**Confidence**: 80%  
The `--stats` output uses `{staleness_status:?}` which exposes Rust enum internals to users (e.g., `HeadChanged { stored: "abc", current: "def" }`).

**Fix**: Implement `Display` for `StalenessCheck` or use explicit string mapping.

### 3. Duplicate Git Root Discovery Logic
**File**: `install.rs:332`  
**Confidence**: 85%  
`find_git_root_from_cwd()` duplicates `walk::discover_project_root()`. Both walk up looking for `.git` with a 256-ancestor cap.

**Fix**: Extract shared utility or delegate one to the other.

---

## Architectural Concerns

### 1. `FileId -> path` Mapping Is Implicit
**Files**: `query.rs:70-74`, `manifest.rs:268-272`, `index.rs:339-399`  
**Confidence**: 82%  
The mapping `sorted_paths()[n] == path for FileId(n)` relies on a chain of implicit guarantees across the build pipeline. Any future change that disrupts sort order silently corrupts search results.

**Recommendation**: Store `FileId` directly in manifest entries during indexing, or add a debug assertion verifying the invariant.

### 2. Duplicate Manifest Loads in Stats Command
**File**: `mod.rs:259, 261`  
**Confidence**: 85%  
`run_stats` loads the manifest to extract `git_head`, then `check_staleness` loads it again.

**Recommendation**: Extract manifest once and pass it to a staleness variant that accepts a pre-loaded manifest.

### 3. Complex Flags Structure with Mutually Exclusive Booleans
**File**: `mod.rs:102-114`  
**Confidence**: 80%  
The `Flags` struct has 6 boolean fields (`build`, `rebuild`, `update`, `stats`, `install_hooks`, `remove_hooks`) that are semantically mutually exclusive but not enforced by the type system.

**Recommendation**: Replace with an enum:
```rust
enum SearchMode {
    Build,
    Rebuild,
    Update,
    Stats,
    InstallHooks,
    RemoveHooks,
    Query(String),
}
```

---

## Performance Issues

### Redundant Operations

1. **`extract_context_window` collects all file lines** - `snippet.rs:66` (Confidence: 82%)  
   Allocates a `Vec` of all lines to extract only 7. For a 10K-line file, this wastes ~160KB.

2. **`sorted_paths()` allocates and sorts on every query** - `manifest.rs:268-272` (Confidence: 82%)  
   A 10K-file sort runs every query (~100-200us). Consider lazy caching inside `FileManifest`.

3. **`extract_context_window` uses `.chars()` instead of `.bytes()`** - `staleness.rs:136` (Confidence: 80%)  
   Minor: `is_hex_sha` should use `.bytes()` for ASCII-only hex validation.

---

## Security Notes

### Predictable Temp File Path
**File**: `hooks.rs:176`  
**Confidence**: 80%  
`write_hook_atomic` creates temp files at a predictable path (`.git/hooks/post-commit.tmp`), enabling symlink races. Should use `tempfile::NamedTempFile` (already a dependency, used correctly in manifest module).

### Unsanitized Ref Path
**File**: `staleness.rs:88-90, 104`  
**Confidence**: 80%  
`read_git_head` reads `.git/HEAD`, strips `ref: `, and uses the remainder directly as a file path component. A crafted `.git/HEAD` containing `ref: ../../etc/shadow` would read an arbitrary file. Defense-in-depth: validate that `ref_path` starts with `refs/` before using it.

---

## Summary of Fixes by Priority

### Must Fix (Blocking)
1. ✓ Non-git infinite rebuild loop (architecture/regression)
2. ✓ Unguarded snippet file read (reliability)
3. ✓ Missing concurrent build lock (reliability)
4. ✓ Duplicate manifest loads (performance)
5. ✓ Flag parser return type (consistency)
6. ✓ Missing edge-case test (regression)

### Should Fix (Maintainability)
1. ✓ Misleading staleness comment
2. ✓ Overflow in context window calculation
3. ✓ Truncating cast in line count
4. ✓ SHA-256 support

### Should Add (Test Coverage)
1. `auto_refresh_if_stale` unit tests
2. Corrupt index error-path test
3. `parse_flags` argument tests (`--root`, `-n`, combined)
4. Additional edge-case tests for snippet extraction

---

## Recommendation

**CHANGES_REQUESTED** — The PR adds valuable functionality with solid fundamentals (bounded resources, atomic writes, graceful degradation), but the blocking issues must be resolved:

- The infinite rebuild loop for non-git projects is a functional regression that will severely degrade performance in production for any non-git codebase.
- The unguarded memory allocation on snippet extraction creates an unbounded allocation path at query time.
- Missing concurrent build protection creates a realistic race condition given the architecture.
- The redundant manifest load on the query hot path doubles the cost of the largest I/O operation.

All fixes are straightforward, well-understood, and under 50 lines of code each. After these changes, the PR will be ready for merge.

