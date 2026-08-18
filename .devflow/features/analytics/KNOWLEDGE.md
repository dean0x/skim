---
feature: analytics
name: File-Op Analytics Recording & Cache-Dir Resolution
description: "Use when adding new recording paths, changing cache-directory resolution, debugging silent analytics drops, writing tests that invoke skim, or tracing where session_id flows. Keywords: analytics, token savings, record_file_ops, flush_pending, register_thread, cache_root, SKIM_CACHE_DIR, SKIM_ANALYTICS_DB, SKIM_DISABLE_ANALYTICS, session_id, fire-and-forget."
category: domain-knowledge
directories: ["crates/rskim/src/analytics", "crates/rskim/src"]
referencedFiles:
  - crates/rskim/src/analytics/mod.rs
  - crates/rskim/src/analytics/schema.rs
  - crates/rskim/src/cache.rs
  - crates/rskim/src/cmd/hook_log.rs
  - crates/rskim/src/process.rs
  - crates/rskim/src/main.rs
  - crates/rskim/src/multi.rs
  - crates/rskim/tests/common/mod.rs
created: 2026-06-25
updated: 2026-06-25
---

# File-Op Analytics Recording & Cache-Dir Resolution

## Overview

This subsystem records per-invocation token-savings data to a local SQLite database (`~/.cache/skim/analytics.db`) so `skim stats` can show dashboards. The core challenge — which produced PF-001 — is that recording must happen AFTER stdout is flushed (streaming contract), token counting is expensive, and short-lived processes exit before background threads finish writing. The subsystem also resolves where all cache state lives; a prior drift (PF-002) caused `SKIM_CACHE_DIR` to relocate only the parser cache but not analytics.db.

Two recording paths exist and must not be conflated: the **file-op path** (`record_file_ops`) for `skim <file>` invocations, and the **subcommand path** (`record_fire_and_forget` / `try_record_command` / `try_record_command_with_counts`) for wrapped tool output (cargo, git, etc.).

## Business Context

Analytics are purely local and best-effort. They answer "how many tokens did skim save this week across all my projects?" No data leaves the machine. The recording model accepts that occasional rows are silently dropped (e.g. file deleted between main-thread run and background re-read) rather than blocking the primary output path.

## Core Business Rules

### Rule 1: Background threads must be registered or writes are lost

Every `std::thread::spawn` that touches the analytics DB must call `register_thread(handle)` immediately. `flush_pending()` in `main()` joins the `PENDING_THREADS` Mutex<Vec<JoinHandle<()>>> before returning `ExitCode`. Without `register_thread`, short-lived commands exit before the thread writes — this was the PF-001 failure mode for plain `skim <file>` before the unified recorder was introduced.

The mutex is poison-recovered (`into_inner()`) so a prior thread panic does not block subsequent handle registrations.

### Rule 2: Output-first contract

`record_file_ops` and `record_fire_and_forget` are always called AFTER `write_result_and_stats` / `writer.flush()`. Analytics recording must never be on the stdout critical path.

### Rule 3: Two recording paths — never conflate them

**File-op path** — for `skim <file>`, stdin, globs, directories, and multi-file explicit lists:

- Entry point: `record_file_ops(enabled, rows: Vec<FileOpRow>, common: FileOpCommon)`
- Spawns ONE background thread per invocation (not one per file)
- Inside the thread: tokenizes rows IN PARALLEL via `rayon into_par_iter` (when counts not already known), then persists SERIALLY to avoid SQLite write contention
- `command_type` is always `CommandType::File` ("file" in the DB)
- Language comes from `Language::as_str()` on `ProcessResult.language`

**Subcommand path** — for `skim cargo test`, `skim git diff`, etc.:

- Entry points: `try_record_command` (text-based, defers tokenization) and `try_record_command_with_counts` (counts already known)
- Both delegate to `record_fire_and_forget` / `record_with_counts` respectively
- Token counting for the text-based variant happens inside the background thread
- `command_type` varies (Test, Build, Git, Lint, etc.)

### Rule 4: FileCounts determines whether the background thread re-reads disk

`FileCounts::Known { raw, compressed }` — counts were already computed (e.g. `--show-stats` was on, or a count-carrying cache hit). No re-read, no double tokenization work.

`FileCounts::Tokenize { raw: RawSource, compressed }` — plain run / cold cache. For single files the background thread re-reads the file via `read_source`. For stdin the raw buffer is captured in `ProcessResult.stdin_raw` (stdin cannot be re-read) and moved as `RawSource::Inline`. TOCTOU: if the file is deleted or grown past the 50 MB guard between the main-thread run and the background re-read, `read_source` returns `Err` and the row is silently dropped via `filter_map`. Sibling rows in the same batch are unaffected.

### Rule 5: Parallel tokenization, serial persist

Inside `record_file_ops`'s background thread: `rows.into_par_iter().filter_map(...)` resolves all counts in parallel (rayon), then `for rec in records { persist_record(&rec); }` writes serially. This is intentional: rayon is safe for read-only BPE tokenization; SQLite's WAL mode allows concurrent readers but a single writer is simpler and avoids contention errors.

### Rule 6: Cache-dir single source of truth

All cache subsystems (parser cache, tee output, hook.log, default analytics.db path) resolve their root through `cache::cache_root` / `cache_root_from`:

- `SKIM_CACHE_DIR` set and non-empty → used as-is (no "skim" suffix appended)
- `SKIM_CACHE_DIR=""` → treated as unset (hardened against empty string)
- Not set → `dirs::cache_dir().join("skim")` (e.g. `~/.cache/skim` on Linux)

`cache::get_cache_dir` and `hook_log::CacheEnv::resolve_cache_dir` both delegate to `cache_root_from`. The regression test in `cache.rs` (C2, serial-gated) asserts `cache_root() == cmd::resolve_cache_dir()` for both the set and unset cases.

`--clear-cache` clears parser-cache JSON files only. It does NOT touch `analytics.db`. Use `skim stats --clear` to wipe analytics data.

### Rule 7: SKIM_ANALYTICS_DB wins over SKIM_CACHE_DIR for the DB

`AnalyticsDb::open_default()` checks `SKIM_ANALYTICS_DB` first; if set, that path is used regardless of `SKIM_CACHE_DIR`. If unset, falls back to `cache::get_cache_dir()?.join("analytics.db")`.

## Technical Implementation Patterns

### AnalyticsConfig — read env once at the boundary

`AnalyticsConfig::from_process(cli_disable, session_id)` is called once in `main()` before any thread is spawned and threaded down to all callers. This replaces prior per-call env reads. Tests construct `AnalyticsConfig` directly with controlled values — no env mutation needed.

`SKIM_DISABLE_ANALYTICS` values "1", "true", "yes" (case-insensitive) disable recording. An empty string does NOT disable. `parse_disable_value` is a pure function testable independently.

### session_id resolution priority (in main)

1. Sidecar file (out-of-band, written by the hook rewrite pipeline, found via `session_sidecar::read_session_id`)
2. `SKIM_SESSION_ID` env var
3. `--session-id=VALUE` flag (forward-compat fallback only; OLD hook → NEW binary skew scenario)

`is_safe_session_id` validates the value: `[a-zA-Z0-9_\-.]`, max 128 chars. Anything else is rejected to prevent command injection.

`session_id` on `TokenSavingsRecord` is nullable (schema v3 `ALTER TABLE … ADD COLUMN session_id TEXT`). Pre-v3 rows have NULL and are excluded from per-session average calculations.

### Schema migrations — forward-only via PRAGMA user_version

Migrations in `schema::run_migrations` are guarded by `PRAGMA user_version` and run in ascending version order. Each is idempotent. A DB written by a newer version will have a higher `user_version` and no older migration will re-run. Current versions: v1 (token_savings table + indexes), v2 (analytics_meta table for prune tracking), v3 (session_id column + index).

### Auto-pruning and DB growth bounds

`AnalyticsDb::maybe_prune()` runs after each `persist_record` call. It prunes rows older than 90 days, gated behind `analytics_meta` key `last_prune` so the full table scan only runs once per 24h. The `invalid_records_cleaned` sentinel in `analytics_meta` ensures a one-time cleanup of pre-fix rows where `compressed_tokens > raw_tokens` runs at most once.

`original_cmd` is truncated to 500 bytes (UTF-8 boundary-safe) before INSERT to bound row size.

## Error Handling and Recovery

- `persist_record` opens a fresh `AnalyticsDb` connection each time; failure is silently discarded. This is intentional — analytics must never crash the main path.
- `PENDING_THREADS` mutex: poison-recovered via `into_inner()` in both `register_thread` and `flush_pending`.
- `filter_map` in `record_file_ops` drops rows where `read_source` or `count_tokens` fails — deleted/changed files produce no row, not a panic.
- `savings_percentage` returns 0.0 when `compressed_tokens >= raw_tokens` (expansion rows) — these are valid, not data errors.

## Anti-Patterns

- **Spawning an analytics thread without calling `register_thread`** — the thread handle is dropped immediately, `flush_pending()` never joins it, and short-lived processes exit before the write completes. This was PF-001.
- **Calling `AnalyticsDb::open_default()` directly from integration tests** — it writes to the real developer `~/.cache/skim/analytics.db`. Always use the test harness helpers.
- **Adding a second `cache_root` resolver** — any new subsystem needing the cache dir must call `cache::cache_root` / `get_cache_dir`, not re-read `SKIM_CACHE_DIR` directly. Divergence re-introduces PF-002.
- **Mixing the two recording paths** — do not call `try_record_command` for file operations or `record_file_ops` for subcommand output. They set different `command_type` values and have different tokenization semantics.
- **Running token counting on the main thread before stdout flush** — any BPE tokenization must happen in the background thread, after output is written.

## Gotchas

- **`--clear-cache` does NOT clear analytics.db** — it only removes `.json` parser-cache files. `skim stats --clear` is the analytics wipe command. A future agent adding analytics-adjacent code should not assume `--clear-cache` resets analytics state.
- **`rskim` is bin-only** — scope cargo commands with `--bins` or `--all-targets`. `cargo test -p rskim --lib` fails with "no library targets found". `cargo clippy -p rskim` without `--all-targets` does NOT lint `tests/*.rs` integration tests, silently missing unused-import warnings there.
- **Each `tests/*.rs` file is its own crate** — it needs its own top-level `mod common;` declaration. A `mod common;` placed inside a `#[cfg(test)]` block in one integration test file does NOT make it available to sibling `tests/*.rs` files. This was a real defect encountered during this work.
- **`flush_pending()` is synchronous** — it joins all background threads before returning. On a slow network filesystem this could add 200 ms to exit latency. There is no timeout mechanism in stable Rust for JoinHandle; this is accepted as the lesser trade-off.
- **Parallel `cargo test` with analytics enabled** — tests that write to the real analytics.db from multiple threads can cause SQLite busy errors. The 5000 ms `busy_timeout` in `AnalyticsDb::open` mitigates but does not eliminate this on very slow disks.
- **`SKIM_CACHE_DIR` override is used as-is** — no `skim` suffix is appended. `SKIM_CACHE_DIR=/tmp/myskim` puts everything directly in `/tmp/myskim`, NOT `/tmp/myskim/skim`. This differs from the default path where `dirs::cache_dir()` returns e.g. `~/.cache` and we append `skim`.

## Key Files

- `crates/rskim/src/analytics/mod.rs` — all recording logic: `record_file_ops`, `record_fire_and_forget`, `register_thread`, `flush_pending`, `AnalyticsDb`, `AnalyticsConfig`, `FileCounts`, `FileOpRow`, `FileOpCommon`, `RawSource`
- `crates/rskim/src/analytics/schema.rs` — forward-only SQLite migrations (v1–v3)
- `crates/rskim/src/cache.rs` — `cache_root`, `cache_root_from`, `get_cache_dir` (single source of truth for cache-dir resolution)
- `crates/rskim/src/cmd/hook_log.rs` — `CacheEnv` delegates to `cache_root_from`; hook.log rotation; proves the PF-002 fix is consistent across subsystems
- `crates/rskim/src/main.rs` — `flush_pending()` call site; `AnalyticsConfig::from_process`; `record_file_analytics` helper; `parse_session_id`; `THREADS_SPAWNED` guard
- `crates/rskim/src/multi.rs` — `MultiFileOptions.analytics_enabled`; bulk `FileOpRow` construction for glob/dir paths; calls `record_file_ops`
- `crates/rskim/src/process.rs` — `ProcessResult.stdin_raw` (buffer for stdin analytics); `ProcessResult.language` / `parse_tier` fields consumed by recording layer
- `crates/rskim/tests/common/mod.rs` — `skim()` (analytics OFF by default), `skim_with_analytics(db)` (isolated DB, analytics ON), `skim_bin()` (std::process callers)

## Related

- ADR-001: net-savings compression guard caps tokenization INPUT at 256 KiB in the output/compression-decision guard — NOT in analytics. Analytics tokenization is bounded instead by the 50 MB `read_source` guard in `process.rs`.
- PF-001 (resolved): plain `skim <file>` analytics was gated on parser-cache-carried token counts; fixed by the `record_file_ops` background recorder with `register_thread`.
- PF-002 (resolved): `SKIM_CACHE_DIR` only relocated parser cache, not analytics.db; fixed by routing all subsystems through `cache::cache_root` / `cache_root_from`.
