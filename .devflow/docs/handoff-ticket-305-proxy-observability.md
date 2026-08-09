# Phase 4 Handoff: ticket/305-proxy-observability — ProxyEvent Extension + AnalyticsCompletionBody

## Commits (all phases)

```
10f4bb58 feat(analytics): schema v4 migration — provider/model columns, proxy_block_decisions table (#305)
f23ab7a6 feat(analytics): Phase 2 recording core — CommandType::Proxy, select_encoding, record_proxy, analytics_meta (#305)
31c57849 feat(analytics): Phase 3 — CLI scope separation, proxy query methods, stats rendering (#305)
f1d5eaa7 feat(proxy): Phase 4 observability — ProxyEvent extension, model detection, AnalyticsCompletionBody (#305)
```

## Phase 4 Summary

### Files Modified

**`crates/rskim-proxy/src/analytics.rs`**

New types added (all `pub`, `#[non_exhaustive]` preserved on ProxyEvent):

- `RequestTier` enum: `Full | Degraded | Passthrough` with `as_str()` method
  (`"full"` / `"degraded"` / `"passthrough"`)
- `BlockDecisionProjection` struct: `component: &'static str`, `outcome: &'static str`,
  `bytes_in: usize`, `bytes_out: usize`
- `ProxyEvent` extended with new fields:
  - `model: Option<String>` — verbatim from request body (AD-PXY-22)
  - `turn_id: Option<String>` — always `None` in Phase 4 (reserved for Phase 5)
  - `tier: RequestTier` — derived from block-router decisions (AD-PXY-21)
  - `raw_body: Bytes` — original request bytes (Arc-cheap)
  - `final_body: Bytes` — post-transform request bytes
  - `block_decisions: Vec<BlockDecisionProjection>` — per-block projection
  - `upstream_error_status: Option<u16>` — Some(502|504) for upstream-errored rows (AD-PXY-25)
- `ProxyEvent::request_bytes` now derived from `raw_body.len()` inside `new()` (not a param)
- `ProxyEvent::response_bytes` always 0 (streaming; not counted at relay time)
- `ProxyEvent::new()` now takes 9 params (7 new): provider, model, turn_id, tier, raw_body,
  final_body, block_decisions, upstream_error_status, duration

**`crates/rskim-proxy/src/detect.rs`**

- `detect_model(body: &[u8]) -> Option<String>` — `pub(crate)`, uses `ModelOnly`
  struct with `serde::Deserialize`, bounded by `SHAPE_SNIFF_LIMIT` (8KiB),
  returns model string verbatim with no normalization (AD-PXY-22).
  Returns `None` for non-UTF-8, parse failure, or absent `"model"` key.
  6 unit tests added to the existing `tests` module.

**`crates/rskim-proxy/src/seam.rs`**

- Updated AD-PXY-09 comment on `TransformContext` module, struct, and `new()`:
  "turn_id lives on ProxyEvent, not TransformContext"
- No code changes to `TransformContext` struct or constructor.

**`crates/rskim-proxy/src/server.rs`**

Major changes:

- **`NullSink` removed** (was dead code after Phase 4 changes)
- **`AnalyticsCompletionBody` added** (AD-PXY-23): observe-only body wrapper that:
  - Delegates every `poll_frame` unchanged (ADR-007 egress losslessness)
  - Fires `analytics.on_request(&event)` exactly once at: clean EOF (poll_frame returns None)
    or Drop (client disconnect / stream error)
  - `fired: bool` guard prevents double-fire
  - Sets `event.duration = start.elapsed()` at fire time (not header time)
  - `catch_unwind` per AC9
- **`derive_tier(records: &[DecisionRecord]) -> RequestTier`** (AD-PXY-21):
  - Filters to `"block-router"` component only (Cross-Plan Amendment #3)
  - Priority: any Degraded → Degraded; any Full → Full; else Passthrough
- **`project_decision(record: &DecisionRecord) -> BlockDecisionProjection`** (AD-AN-13):
  - Maps `OutcomeReason` to outcome `&'static str`
  - Copies `bytes_in` / `bytes_out` directly
- **`fire_upstream_error_event(...)`** helper (AD-PXY-25):
  - Builds ProxyEvent with `upstream_error_status = Some(status)`
  - Called at all post-transform 502/504 return sites
  - `catch_unwind` per AC9
- **`handle_request` modified**:
  - `detect_model()` called after provider detection (AD-PXY-22)
  - `MockSink::new()` (from `rskim_contract::log`) replaces `NullSink` (AD-PXY-21)
  - `pipeline.run(..., &collecting_sink)` uses collecting sink
  - After transform: `collecting_sink.drain()` → `derive_tier()` → `project_decision` vec
  - Post-transform 502 (empty upstream URL): `fire_upstream_error_event(..., 502, ...)` (AD-PXY-25)
  - Post-transform 504 (timeout): `fire_upstream_error_event(..., 504, ...)` (AD-PXY-25)
  - Post-transform 502 (bad URL / connection error): `fire_upstream_error_event(..., 502, ...)` (AD-PXY-25)
  - Success path: build `ProxyEvent` (duration=ZERO), wrap relay body in
    `AnalyticsCompletionBody::new(relay_inner_body, event, analytics, start).boxed()`
  - Pre-transform failures (400, Unknown+no-default 502): NO analytics event (AD-PXY-24)
- **Unit tests added** to `#[cfg(test)]` block in server.rs:
  - `test_derive_tier_*` (6 tests): empty, Full, Degraded, Degraded-trumps-Full, filter exclusion
  - `test_project_decision_*` (3 tests): full, passthrough, degraded

**`crates/rskim-proxy/tests/proxy_analytics_tier_tests.rs`** (new)

Integration test file, all tests `#[cfg(feature = "testing")]`, PF-012 ephemeral ports:
- Synthetic stages: `BlockRouterFullStage`, `BlockRouterDegradedStage`, `CacheAlignFullStage`
- `FullCapturingHook` captures full `ProxyEvent` + atomic fired count
- Tests:
  - `test_tier_full_when_block_router_modifies` (AD-PXY-21)
  - `test_tier_degraded_when_block_router_degrades` (AD-PXY-21)
  - `test_tier_passthrough_when_only_cache_align_modifies` (Cross-Plan Amendment #3)
  - `test_tier_degraded_trumps_full_in_mixed_records` (AD-PXY-21 priority rule)
  - `test_model_detected_in_analytics_event` (AD-PXY-22)
  - `test_model_none_when_body_has_no_model_key` (AD-PXY-22)
  - `test_no_event_for_unknown_provider_no_default_upstream` (AD-PXY-24)
  - `test_upstream_error_event_when_no_upstream_configured` (AD-PXY-25)
  - `test_analytics_fires_at_stream_end_not_header_time` (AD-PXY-23)
  - `test_raw_body_and_final_body_fields` (AD-PXY-21)

### Phase 4 Test Results

```
Summary [  32.730s] 151 tests run: 151 passed, 0 skipped
```
EXIT=0

Phase 3 baseline was 3213 tests. Phase 4 adds 21 rskim-proxy tests (10 new
integration in `proxy_analytics_tier_tests.rs` + 9 unit tests in `server.rs` +
6 in `detect.rs` from Phase 4's detect_model). The cross-crate total after
Phase 4 is not measured here (rskim-proxy-only run).

### Key Constraints for Phase 5

**`turn_id`**: always `None` in Phase 4. Phase 5 should extract it from the
request body (same sniff window as `detect_model`). The field is already on
`ProxyEvent` (with `#[non_exhaustive]` in place) — Phase 5 passes the real value.

**`response_bytes`**: still 0 (sentinel). `AnalyticsCompletionBody` fires before
the downstream consumer reads the body, so response byte counting requires
accumulating frames. This is deferred; do NOT count response bytes in Phase 5
unless the plan explicitly scopes it.

**`AnalyticsCompletionBody` fire order**: the event fires when `poll_frame`
returns `Poll::Ready(None)` (clean EOF) OR on `Drop`. In the existing
`test_ac6_capturing_hook_one_event_per_request`, the 20ms sleep after
`post_body().await` is sufficient because `post_body` calls `.collect().await`
which exhausts the body before returning, triggering the clean EOF fire.

**Imports in server.rs**:
- `use rskim_contract::log::{DecisionRecord, MockSink, OutcomeReason};`
- `use crate::analytics::{AnalyticsHook, BlockDecisionProjection, ProxyEvent, RequestTier};`
- `use crate::detect::{ProxyProvider, detect_model, detect_provider};`
- `NullSink` is gone — do not re-add

**`#[non_exhaustive]`** on `ProxyEvent`, `ProxyProvider`, `AuthMode`, `RequestTier` (added in
Phase 4). Wildcard match arms are required everywhere they're matched outside the defining crate.

**`cargo nextest run -p rskim-proxy --features testing -j 4`** — do NOT use `--all-targets`
(stalls on bench binaries per project gotchas).

**`SKIM_PASSTHROUGH=1`** required for cargo/nextest commands to avoid skim hook buffering.

### Cross-Plan Amendment #3 (active)

`derive_tier()` filters to `"block-router"` component only. `CacheAlignStage` (#306)
records at `component = "cache-align"` — excluded from tier derivation by design.
The discriminating integration test `test_tier_passthrough_when_only_cache_align_modifies`
verifies this. Do NOT change `derive_tier()` to include other components without updating
Cross-Plan Amendment #3.

### Prior Phase Notes (preserved from Phase 3 handoff)

`record_proxy()` and `record()` remain separate paths. CLI rows use `record()`,
proxy rows use `record_proxy()`.

`select_encoding(provider, model)` is the canonical provider+model → Encoding mapping.
NOT stored in DB.

`cli_scope_clause` and `proxy_scope_clause` are module-private helpers in analytics/mod.rs.
Any new CLI aggregate method must call `cli_scope_clause`. Never use raw `WHERE` without it.

Schema is v4 (committed in Phase 1). No schema changes in Phases 2, 3, or 4.

`ProxyBlockDecisionRow` carries `#[allow(dead_code)]`; deferred `--audit` CLI (#469)
is the consumer. Do not remove the struct.
