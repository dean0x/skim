//! Proxy analytics recording overhead — criterion benchmark (AC18-perf).
//!
//! ## AD-AN-18-PERF — Informational overhead bench (non-gating)
//!
//! This bench measures the per-request overhead added by the proxy analytics
//! recording path: the difference between `ChannelAnalyticsHook::on_request`
//! (analytics enabled) and `NoopAnalyticsHook::on_request` (analytics disabled
//! — the behaviour produced when `SKIM_DISABLE_ANALYTICS=1` is set, which
//! causes `proxy.rs` to wire `NoopAnalyticsHook` instead of the channel hook).
//!
//! **INFORMATIONAL ONLY. This bench is NOT wired into CI and asserts NO
//! absolute latency threshold.** The binding correctness gate is AC14 — a
//! structural test in the rskim suite that asserts `on_request` performs only
//! a bounded non-blocking enqueue (no DB open, no token count). Per ADR-003
//! and PF-005, no numeric threshold is invented or presented as a measured
//! basis; the output is a curiosity, not a gate.
//!
//! ## What is measured
//!
//! The per-request analytics path is a bounded non-blocking `try_send` on a
//! `crossbeam_channel` (see `ChannelAnalyticsHook::on_request`). This bench
//! reports the wall-clock cost of that call versus a pure no-op, isolating the
//! channel-send overhead from all other proxy processing. Criterion produces
//! mean, standard deviation, and confidence intervals. The p95 delta is the
//! informational signal; it is expected to be in the nanosecond-to-low-
//! microsecond range, far below request-level latency.
//!
//! ## Why this is informational rather than gated
//!
//! A single `try_send` minus a noop is dominated by measurement noise at
//! criterion's default sample size. Gating CI on nanosecond deltas would
//! produce frequent false positives from hardware variance. The binding gate
//! (AC14) asserts the *absence* of blocking work on the request path; the
//! *magnitude* of the channel-send cost is not a correctness property.
//! See ADR-003/PF-005.
//!
//! ## Running locally
//!
//! ```text
//! cargo bench -p rskim-bench --bench proxy_analytics_overhead
//! ```
//!
//! Criterion writes HTML reports to `target/criterion/`. Comparing runs:
//! ```text
//! cargo bench -p rskim-bench --bench proxy_analytics_overhead -- --save-baseline before
//! # make a change
//! cargo bench -p rskim-bench --bench proxy_analytics_overhead -- --baseline before
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use rskim_proxy::analytics::{
    AnalyticsHook, ChannelAnalyticsHook, NoopAnalyticsHook, ProxyEvent, RequestTier,
};
use rskim_proxy::detect::ProxyProvider;

// ============================================================================
// Synthetic ProxyEvent
// ============================================================================

/// Build a synthetic [`ProxyEvent`] representative of a realistic LLM request.
///
/// A ~4 KB body is used: small enough to keep bench setup fast, large enough
/// to represent a plausible API call. Body bytes are fixed so the bench
/// isolates hook overhead rather than allocation cost.
///
/// `duration` is a fixed stub value — the completion wrapper normally sets this
/// at stream-end, but the bench calls `on_request` directly.
fn make_event() -> ProxyEvent {
    let body = Bytes::from(vec![b'x'; 4_096]);
    ProxyEvent::new(
        ProxyProvider::Anthropic,
        Some("claude-3-5-sonnet-20241022".to_owned()),
        None,                    // turn_id — always None until #344
        RequestTier::Passthrough,
        body.clone(),
        body,
        vec![],  // block_decisions — empty for a passthrough request
        None,    // upstream_error_status — None for a normal relayed row
        Duration::from_millis(10),
    )
}

// ============================================================================
// Bench group: analytics_enabled vs analytics_disabled
// ============================================================================

/// AC18-perf — per-request hook overhead: analytics enabled vs disabled.
///
/// **Informational only — no absolute threshold is asserted.** See module docs.
///
/// Two arms mirror the production configuration toggle:
///
/// - **`analytics_enabled`**: `ChannelAnalyticsHook::on_request`, the path
///   taken in a live proxy when analytics recording is active. Each call
///   performs one bounded non-blocking `try_send`.
///
/// - **`analytics_disabled`**: `NoopAnalyticsHook::on_request`, the path taken
///   when `SKIM_DISABLE_ANALYTICS=1` is set — proxy.rs wires this hook and
///   spawns no consumer thread (see `cmd/proxy.rs`).
///
/// Criterion handles iteration count and timing internally — `Instant::now`
/// is intentionally absent from this file.
fn bench_analytics_recording_overhead(c: &mut Criterion) {
    let event = make_event();

    // Channel capacity matches the production default (PROXY_QUEUE_RECORD_CAPACITY = 2048,
    // AD-AN-8). The receiver is held for the lifetime of the bench so try_send
    // never observes a disconnected channel. The bench does NOT drain the receiver:
    // once the channel saturates, subsequent try_sends return Err immediately
    // (non-blocking drop-and-count), which is still the production path and still
    // measures the correct overhead.
    let (channel_hook, _rx) = ChannelAnalyticsHook::new(2048);
    let noop_hook = NoopAnalyticsHook;

    let mut group = c.benchmark_group("proxy_analytics_recording_overhead");
    // Small sample and measurement time: this is a nanosecond-scale operation
    // whose cost is dominated by the channel protocol, not throughput. Fast local
    // runs are preferable to statistically-tight but slow ones for an informational
    // bench. Adjust via --sample-size / --measurement-time flags if needed.
    group.sample_size(100);
    group.measurement_time(Duration::from_secs(3));

    // Arm A: analytics enabled — bounded non-blocking channel send (production path).
    group.bench_with_input(
        BenchmarkId::new("analytics_enabled_channel_send", "4kb_event"),
        &event,
        |b, event| {
            b.iter(|| channel_hook.on_request(black_box(event)));
        },
    );

    // Arm B: analytics disabled (SKIM_DISABLE_ANALYTICS=1) — pure no-op.
    // The delta between arm A and arm B is the marginal per-request cost of
    // analytics recording. It is expected to be small (nanoseconds to low
    // microseconds); this bench surfaces it for local inspection, not gating.
    group.bench_with_input(
        BenchmarkId::new("analytics_disabled_noop", "4kb_event"),
        &event,
        |b, event| {
            b.iter(|| noop_hook.on_request(black_box(event)));
        },
    );

    group.finish();
}

criterion_group!(analytics_benches, bench_analytics_recording_overhead);
criterion_main!(analytics_benches);
