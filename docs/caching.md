# Caching

**Caching is enabled by default** for maximum performance on repeated processing.

## Overview

Skim uses an intelligent caching system that dramatically speeds up repeated file processing. When you process a file, the transformed result is cached on disk. Subsequent processing of the same file (if unchanged) retrieves the result instantly from cache.

## How It Works

### Cache Key

The cache key is a SHA256 hash of three components:
- **File path** - Absolute path to the file
- **Modification time** - File's mtime (modification timestamp)
- **Transformation mode** - Structure, signatures, types, or full

This ensures cache hits only occur when:
- Same file is processed
- File hasn't been modified
- Same transformation mode is used

### Cache Location

```bash
# Platform-specific cache directory
~/.cache/skim/          # Linux
~/Library/Caches/skim/  # macOS
%LOCALAPPDATA%\skim\    # Windows
```

Each cached entry is stored as a JSON file with:
```json
{
  "path": "/path/to/file.ts",
  "mode": "structure",
  "mtime": 1634567890,
  "content": "transformed content...",
  "original_tokens": 1000,
  "transformed_tokens": 200
}
```

### Cache Invalidation

Cache is automatically invalidated when:
- File is modified (mtime changes)
- Different transformation mode is used
- Cache is manually cleared with `--clear-cache`

**No manual invalidation needed!** The mtime-based approach ensures cache is always fresh.

## Performance Impact

### Benchmarks

| Scenario | Time | Speedup |
|----------|------|---------|
| First run (no cache) | 244ms | 1.0x |
| **Second run (cached)** | **5ms** | **48.8x faster!** |

### Real-World Performance

On the Chorus project (80 TypeScript files):
- **First run**: 72ms
- **Cached run**: 16ms
- **Speedup**: 4.5x

The speedup is even more dramatic on larger files or slower hardware.

## Cache Management

### View Cache Location

```bash
# List cache directory
ls ~/.cache/skim/

# Check cache size
du -sh ~/.cache/skim/
```

### Clear Cache

```bash
# Clear all cached files
skim --clear-cache
```

This removes all cache entries. Useful when:
- Debugging caching issues
- Freeing disk space
- After upgrading Skim (if format changed)

### Disable Caching

```bash
# Disable for specific run
skim file.ts --no-cache

# Disable for multiple files
skim src/ --no-cache

# Disable for glob patterns
skim 'src/**/*.ts' --no-cache
```

## When Caching Helps

✅ **Repeated processing of same files**
- Watch mode scripts
- Development workflows
- Iterative testing

✅ **Large codebases with infrequent changes**
- Most files don't change between runs
- Massive speedup on unchanged files

✅ **CI/CD pipelines processing same files multiple times**
- Generate docs in multiple formats
- Run multiple transformation modes
- Process same codebase across different steps

✅ **Development workflows with hot reloading**
- Only changed files get re-processed
- Unchanged files come from cache instantly

### Example: Watch Mode

```bash
#!/bin/bash
# Watch script that benefits from caching
while inotifywait -e modify src/; do
    skim src/ > docs/api.txt
    # Only modified files are re-processed!
done
```

### Example: Multi-Mode Processing

```bash
# Generate three different docs - cache shared across modes
skim src/ --mode structure > structure.txt    # 244ms first run
skim src/ --mode signatures > signatures.txt  # 244ms (different cache key)
skim src/ --mode types > types.txt            # 244ms (different cache key)

# Second run (all files cached)
skim src/ --mode structure > structure.txt    # 5ms!
skim src/ --mode signatures > signatures.txt  # 5ms!
skim src/ --mode types > types.txt            # 5ms!
```

## When to Disable Caching

⚠️ **One-time transformations for LLM input**
- No benefit since file processed only once
- Save disk space by using `--no-cache`

```bash
# One-time LLM query
skim src/ --no-cache | llm "Explain this code"
```

⚠️ **Piping through stdin**
- Stdin has no file path or mtime
- Caching not supported

```bash
# Stdin never uses cache
cat file.ts | skim - --language=typescript
```

⚠️ **Testing/debugging transformation logic**
- Ensure you're seeing latest transformation
- Avoid cache pollution during development

```bash
# Testing new transformation
skim file.ts --no-cache
```

⚠️ **Disk space constrained environments**
- CI runners with limited disk
- Containers with small storage
- Temporary environments

```bash
# CI pipeline
skim src/ --no-cache > docs/api.txt
```

## Advanced Usage

### Cache with Multiple Jobs

Caching works perfectly with parallel processing:

```bash
# First run: 8 threads processing, building cache
skim src/ --jobs 8

# Second run: 8 threads reading cache (even faster!)
skim src/ --jobs 8
```

### Cache with Glob Patterns

```bash
# First run caches all matched files
skim 'src/**/*.ts'

# Second run uses cache for all unchanged files
skim 'src/**/*.ts'

# Only files modified since last run get re-processed
```

### Cache with Directory Processing

```bash
# First run
skim src/  # Caches all files

# Edit one file
vim src/app.ts

# Second run
skim src/  # Only app.ts re-processed, rest from cache!
```

## Cache Statistics

### With Token Counting

Caching stores token counts, so `--show-stats` is fast even on cached runs:

```bash
$ skim src/ --show-stats
[skim] 63,198 tokens → 25,119 tokens (60.3% reduction) across 80 file(s)

$ skim src/ --show-stats  # Second run (cached)
[skim] 63,198 tokens → 25,119 tokens (60.3% reduction) across 80 file(s)
# Instant! Token counts retrieved from cache
```

## Cache Internals

### Cache Directory Structure

```
~/.cache/skim/
├── a3f2... .json  # Cached entry for file1.ts (structure mode)
├── b8e1... .json  # Cached entry for file2.ts (structure mode)
├── c5d9... .json  # Cached entry for file1.ts (signatures mode)
└── ...
```

Each file gets a separate cache entry per mode.

### Cache File Format

```json
{
  "path": "/workspace/skim/src/main.rs",
  "mode": "structure",
  "mtime": 1698765432,
  "content": "pub fn main() { /* ... */ }\n",
  "original_tokens": 1500,
  "transformed_tokens": 300
}
```

### Atomic Writes

Cache writes are atomic - either the full entry is written or none at all. This prevents corruption if the process is interrupted.

## Troubleshooting

### Cache Not Working

**Symptoms:** Files are re-processed every time even though they haven't changed.

**Solutions:**
1. Check if caching is enabled (not using `--no-cache`)
2. Verify cache directory is writable
3. Check disk space (cache writes may be failing)
4. Clear cache and try again: `skim --clear-cache`

### Cache Taking Too Much Space

**Symptoms:** Cache directory is very large.

**Solutions:**
```bash
# Check cache size
du -sh ~/.cache/skim/

# Clear cache
skim --clear-cache

# Or manually remove old entries
find ~/.cache/skim/ -mtime +30 -delete  # Remove entries older than 30 days
```

### Stale Cache

**Symptoms:** Getting old results even though file was modified.

**Cause:** Filesystem doesn't update mtime properly (rare on network filesystems).

**Solution:**
```bash
# Force cache clear
skim --clear-cache

# Then re-process
skim src/
```

## Best Practices

1. **Leave caching enabled by default** - It's smart enough to know when to invalidate
2. **Use `--no-cache` in CI** - Ephemeral environments don't benefit from caching
3. **Use `--clear-cache` after upgrades** - If cache format changes between versions
4. **Don't manually edit cache** - Let Skim manage it
5. **Monitor cache size** - If it grows too large, clear it periodically

## Future Enhancements

Potential caching improvements (not yet implemented):

- Cache size limits (auto-evict old entries)
- Cache compression for large files
- Distributed cache for team environments
- Cache statistics (hits/misses, size, etc.)

See the [GitHub issues](https://github.com/dean0x/skim/issues) for feature requests.
