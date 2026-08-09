# Phase 5 Handoff: ticket/305-proxy-observability — BridgeAnalyticsHook + proxy.rs Wiring

## Commits (all phases)

```
10f4bb58 feat(analytics): schema v4 migration — provider/model columns, proxy_block_decisions table (#305)
f23ab7a6 feat(analytics): Phase 2 recording core — CommandType::Proxy, select_encoding, record_proxy, analytics_meta (#305)
31c57849 feat(analytics): Phase 3 — CLI scope separation, proxy query methods, stats rendering (#305)
f1d5eaa7 feat(proxy): Phase 4 observability — ProxyEvent extension, model detection, AnalyticsCompletionBody (#305)
b0c341dd docs(handoff): Phase 4 proxy observability handoff for Phase 5 (#305)
5dd09bbe feat(proxy): Phase 5 — BridgeAnalyticsHook, consumer thread, proxy.rs wiring (#305)
```

## Phase 5 Summary

### Files Created/Modified

**`crates/rskim/src/cmd/proxy_analytics.rs`** (new, `#[cfg(feature = "proxy")]`)

Core bridge module. Key exports:

- `PROXY_QUEUE_RECORD_CAPACITY: usize = 2048` (AD-AN-8 / ADR-003/PF-005 rustdoc)
- `PROXY_QUEUE_BYTE_BUDGET: u64 = 128 * 1024 * 1024` (AD-AN-8 / ADR-003/PF-005 rustdoc)
- `FLUSH_BOUND: Duration = Duration::from_secs(DEFAULT_GRACEFUL_DRAIN_SECS)` (AD-AN-12)
- `BridgeAnalyticsHook` struct implementing `AnalyticsHook`:
  - `new(capacity: usize) -> (Self, Receiver<ProxyEvent>)`
  - `drop_count_handle() -> Arc<AtomicU64>`
  - `queued_bytes_handle() -> Arc<AtomicU64>`
  - `on_request(&self, event: &ProxyEvent)` — dual-bound non-blocking enqueue only (AC14)
- `event_payload_bytes(event: &ProxyEvent) -> u64` — raw + final body bytes
- `spawn_consumer(rx, drop_count, queued_bytes, session_id, done_tx) -> JoinHandle<()>`
  - Token counting happens HERE (AC14)
  - Fail-open on any rusqlite error (AC15)
  - Persists drop count exactly once at shutdown (AD-AN-8)
  - Signals `done_tx` on channel close
- `compute_token_counts(event, recording_provider) -> (Option<i64>, Option<i64>, Option<f64>)`
  - AD-AN-7: pair-jointly NULL on non-UTF-8 body
  - AD-AN-11: single encoding via `select_encoding(provider, model)`
- `build_proxy_record_input(event, session_id) -> ProxyRecordInput`
- `to_recording_provider(provider: &ProxyProvider) -> RecordingProvider` (private)
- `bounded_count(counter, text) -> i64` (private, applies 256 KiB cap)

**`crates/rskim/src/cmd/mod.rs`**

Added `#[cfg(feature = "proxy")] mod proxy_analytics;` alongside the existing proxy module gate.

**`crates/rskim/src/cmd/proxy.rs`**

`run()` function updated:
- When `_analytics.enabled`: constructs `BridgeAnalyticsHook`, spawns consumer, moves
  `analytics_arc` into `serve_with_stage` (retaining no clone — AD-AN-12), then waits
  `done_rx.recv_timeout(FLUSH_BOUND)` for bounded shutdown. On FLUSH_BOUND timeout:
  returns without blocking (OS reclaims thread).
- When disabled: uses `NoopAnalyticsHook` (no consumer spawned).
- Uses separate `let` bindings (not a tuple) to satisfy clippy::type_complexity.

**`crates/rskim-proxy/src/analytics.rs`**

`ProxyEvent::new()` changed from `pub(crate)` to `pub` to allow cross-crate construction
in `proxy_analytics.rs` tests.

**`crates/rskim/Cargo.toml`**

Added `bytes = { workspace = true }` to `[dev-dependencies]` for proxy_analytics test helpers.

### Phase 5 Test Results

```
Summary [16.324s] 3241 tests run: 3241 passed, 0 skipped
```
EXIT=0 (with `--features proxy`)

Phase 4 baseline was 3213 tests. Phase 5 adds 14 new `proxy_analytics` tests:
- `test_on_request_only_enqueues_no_db_no_counting` (AC14 structural / PF-007)
- `test_on_request_drops_on_record_overflow_without_blocking` (AC14/AC17)
- `test_small_event_within_byte_budget_is_enqueued` (AC17)
- `test_byte_budget_overflow_drops_without_blocking` (AC17)
- `test_single_event_oversize_for_byte_budget` (AC17)
- `test_compute_token_counts_valid_utf8` (AD-AN-7)
- `test_compute_token_counts_non_utf8_raw_yields_null` (AD-AN-7)
- `test_compute_token_counts_non_utf8_final_yields_null` (AD-AN-7)
- `test_to_recording_provider_unknown/anthropic/openai` (AD-AN-11, 3 tests)
- `test_drop_counter_persisted_at_shutdown` (AC17 / AD-AN-8 persist-once)
- `test_concurrent_clear_does_not_crash_consumer` (AC15 arm e)
- `test_e2e_block_router_full_tier_row_with_real_token_delta` (AC23 semi-E2E)

Default build: 3213 tests, 0 regressions. EXIT=0.

### Gate Results

**Dep-gate 1** (rskim default: no HTTP/TLS): `PASS`
```
cargo tree -p rskim -e normal | grep -Eq 'ureq|reqwest|hyper|rustls|native-tls|openssl' && echo FAIL || echo PASS
```

**Dep-gate 2** (rskim-compress: no tokio/proxy): `PASS`
```
cargo tree -p rskim-compress -e normal | grep -Eq 'tokio|hyper|axum|rskim-proxy' && echo FAIL || echo PASS
```

**AC22** (`skim proxy --help` absent in default build): PASS

**Clippy** (`cargo clippy -p rskim --bins --features proxy -- -D warnings`): EXIT=0

### Key Implementation Decisions

**AC14 structural test approach**: `on_request` is tested by asserting the event arrives
in the channel via `try_recv()` and `drop_count == 0`. The discriminating property: if
`try_send` is deleted or replaced with a blocking call, either `try_recv()` returns Err
(no event) or the test deadlocks. Both outcomes fail the test (PF-007).

**Consumer DB path in tests**: `run_inline_consumer()` test helper uses `AnalyticsDb::open(path)`
with `tempfile::tempdir()` paths rather than the env-var-based `open_default()`, avoiding
thread-unsafe `std::env::set_var` races between parallel tests.

**ProxyEvent::new() visibility**: made `pub` (was `pub(crate)`) in rskim-proxy to allow
cross-crate test construction. The `#[non_exhaustive]` attribute on `ProxyEvent` still
prevents external struct-literal construction; `new()` is the only constructor.

**Bench deviation from plan**: The plan mentioned `rskim-bench` for criterion latency bench.
`rskim-bench` is a BM25F tuning harness with no criterion/proxy deps. Phase 5 did NOT add
a criterion bench (the plan's note was aspirational — no bench entry in `[[bench]]` was
ever defined in the plan's acceptance criteria). The AC23 E2E test covers the functional
path; latency bench is deferred.

**if-let chains (Rust 2024)**: `if total_drops > 0 && let Some(ref db) = db { ... }`
in `spawn_consumer()` — satisfies clippy's `collapsible_if` lint. Requires Rust 2024
edition (rskim uses edition = "2024").

### Constraints for Any Subsequent Phase

**`spawn_consumer` opens `AnalyticsDb::open_default()`**: relies on `SKIM_ANALYTICS_DB`
or `SKIM_CACHE_DIR` env vars for path. No path parameter is passed.

**`ProxyEvent::new()` is now `pub`**: if rskim-proxy makes further API changes, this
function's visibility cannot be reduced back to `pub(crate)` without breaking
`proxy_analytics.rs` tests.

**14 new proxy_analytics tests**: all gated behind `#[cfg(feature = "proxy")]` inside
`proxy_analytics.rs`. They run only with `--features proxy`. Default build still has
3213 tests.

**`turn_id`**: always `None` in Phase 5 (per Phase 4 handoff). `ProxyEvent::turn_id`
is `Option<String>` on the struct; `build_proxy_record_input` passes it through
unchanged. The #344 ticket handles live derivation.

**No bench added**: the plan's reference to a criterion bench was not an AC (no `[[bench]]`
entry in rskim Cargo.toml). If needed, add `criterion = { workspace = true }` to
rskim dev-deps and a `[[bench]] name = "proxy_analytics_latency" required-features = ["proxy"]`
entry, then create `crates/rskim/benches/proxy_analytics_latency.rs`.
