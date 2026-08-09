// Proxy analytics Phase 4 integration tests: tier derivation, model detection,
// three-outcome recording predicate (AD-PXY-21 / AD-PXY-22 / AD-PXY-23 /
// AD-PXY-24 / AD-PXY-25 / Cross-Plan Amendment #3).
//
// PF-012: all tests bind :0 and pass the bound listener to the server.
// No test uses a hardcoded port number.
//
// All tests require the `testing` feature for `run_server_with_listener`.
// Run with: cargo nextest run -p rskim-proxy --all-targets --features testing -j 4
#![cfg(feature = "testing")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use rskim_contract::contract::Outcome;
use rskim_contract::log::{DecisionRecord, DecisionSink, OutcomeReason};
use rskim_proxy::analytics::{AnalyticsHook, ProxyEvent, RequestTier};
use rskim_proxy::config::ProxyConfig;
use rskim_proxy::seam::{TransformContext, TransformPipeline, TransformStage};
use tokio::net::TcpListener;

// ============================================================================
// Synthetic transform stages for tier derivation tests
// ============================================================================

/// A synthetic stage named `"block-router"` that reduces the body by one byte.
///
/// When the body has ≥1 byte, it returns `body[..len-1]` — shorter bytes cause
/// `guarded_transform` to record a `Full` `DecisionRecord` with
/// `component = "block-router"` in the collecting sink, which makes
/// `derive_tier()` return `RequestTier::Full` (AD-PXY-21).
///
/// The never-inflate invariant is satisfied: `output.len() = input.len() - 1`.
struct BlockRouterFullStage;

impl TransformStage for BlockRouterFullStage {
    fn name(&self) -> &'static str {
        "block-router"
    }

    fn apply(
        &self,
        body: &[u8],
        ctx: &TransformContext<'_>,
        _sink: &dyn DecisionSink,
    ) -> Outcome {
        if body.is_empty() {
            return Outcome::passthrough(body.to_vec(), ctx.request_id, self.name());
        }
        // Return one fewer byte — `guarded_transform` in the pipeline emits Full.
        Outcome::modified(body[..body.len() - 1].to_vec(), body.len(), ctx.request_id, self.name())
    }
}

/// A synthetic stage named `"block-router"` that emits a `Degraded` record
/// directly to the sink and returns passthrough bytes.
///
/// AD-PXY-21: any Degraded record from "block-router" → `RequestTier::Degraded`.
/// Passthrough bytes ensure the pipeline does NOT call `guarded_transform`,
/// avoiding a second Full record for the same stage.
struct BlockRouterDegradedStage;

impl TransformStage for BlockRouterDegradedStage {
    fn name(&self) -> &'static str {
        "block-router"
    }

    fn apply(&self, body: &[u8], ctx: &TransformContext<'_>, sink: &dyn DecisionSink) -> Outcome {
        // Emit a Degraded record directly — passthrough bytes follow.
        let _ = sink.try_send(DecisionRecord::modified_with_reason(
            ctx.request_id,
            "block-router",
            body.len(),
            body.len(), // same-size (degraded parse, no shrinkage)
            OutcomeReason::Degraded,
        ));
        // Return passthrough so guarded_transform is NOT called again.
        Outcome::passthrough(body.to_vec(), ctx.request_id, self.name())
    }
}

/// A synthetic stage named `"cache-align"` that modifies bytes.
///
/// Cross-Plan Amendment #3: `derive_tier()` filters to `"block-router"` only.
/// A Full record from `"cache-align"` must NOT contribute to the tier.
/// The tier must remain `RequestTier::Passthrough`.
struct CacheAlignFullStage;

impl TransformStage for CacheAlignFullStage {
    fn name(&self) -> &'static str {
        "cache-align"
    }

    fn apply(
        &self,
        body: &[u8],
        ctx: &TransformContext<'_>,
        _sink: &dyn DecisionSink,
    ) -> Outcome {
        if body.is_empty() {
            return Outcome::passthrough(body.to_vec(), ctx.request_id, self.name());
        }
        Outcome::modified(body[..body.len() - 1].to_vec(), body.len(), ctx.request_id, self.name())
    }
}

// ============================================================================
// Capturing analytics hook for tier + model + upstream_error verification
// ============================================================================

/// Analytics hook that captures full `ProxyEvent`s for inspection.
///
/// Used in tier derivation tests and the three-outcome predicate tests.
/// Stores events behind a `Mutex` — each test drains and inspects after the
/// fact. `fired_count` is a separate atomic for tests that need to verify
/// "zero events fired".
struct FullCapturingHook {
    events: Arc<Mutex<Vec<ProxyEvent>>>,
    fired_count: Arc<AtomicUsize>,
}

impl FullCapturingHook {
    fn new() -> (Self, Arc<Mutex<Vec<ProxyEvent>>>, Arc<AtomicUsize>) {
        let events: Arc<Mutex<Vec<ProxyEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let fired_count = Arc::new(AtomicUsize::new(0));
        (
            Self {
                events: Arc::clone(&events),
                fired_count: Arc::clone(&fired_count),
            },
            events,
            fired_count,
        )
    }
}

impl AnalyticsHook for FullCapturingHook {
    fn on_request(&self, event: &ProxyEvent) {
        self.fired_count.fetch_add(1, Ordering::SeqCst);
        self.events.lock().unwrap().push(event.clone());
    }
}

// ============================================================================
// Test helpers (PF-012 compliant)
// ============================================================================

/// Bind to an OS-assigned ephemeral port.
///
/// PF-012: `TcpListener::bind("127.0.0.1:0")` — OS assigns a unique port,
/// zero TOCTOU risk. Pass the returned listener directly to
/// `run_server_with_listener`.
async fn bind_test_listener() -> TcpListener {
    TcpListener::bind("127.0.0.1:0")
        .await
        .expect("ephemeral port bind (PF-012)")
}

/// Start the proxy with a given upstream URL and a custom analytics hook.
///
/// Returns `(abort_handle, proxy_addr)`.
async fn start_proxy_with_hook(
    upstream_url: &str,
    analytics: Arc<dyn AnalyticsHook>,
) -> (tokio::task::AbortHandle, SocketAddr) {
    start_proxy_with_hook_and_pipeline(upstream_url, analytics, TransformPipeline::identity()).await
}

/// Start the proxy with a given upstream URL, analytics hook, and pipeline.
///
/// Returns `(abort_handle, proxy_addr)`.
async fn start_proxy_with_hook_and_pipeline(
    upstream_url: &str,
    analytics: Arc<dyn AnalyticsHook>,
    pipeline: TransformPipeline,
) -> (tokio::task::AbortHandle, SocketAddr) {
    let listener = bind_test_listener().await;
    let proxy_addr = listener.local_addr().expect("listener local_addr");
    let config = ProxyConfig::builder()
        .port(41001) // placeholder — run_server_with_listener ignores config.bind_addr
        .upstream_default(upstream_url)
        .build()
        .expect("proxy config");
    let task = tokio::spawn(rskim_proxy::testing::run_server_with_listener(
        listener,
        config,
        pipeline,
        analytics,
    ));
    let abort = task.abort_handle();
    tokio::time::sleep(Duration::from_millis(80)).await;
    (abort, proxy_addr)
}

/// Start the proxy with NO upstream configured and a custom analytics hook.
///
/// Anthropic requests will transform normally but hit post-transform 502
/// (no upstream URL) — the distinct upstream-errored event fires (AD-PXY-25).
async fn start_proxy_no_upstream_with_hook(
    analytics: Arc<dyn AnalyticsHook>,
) -> (tokio::task::AbortHandle, SocketAddr) {
    let listener = bind_test_listener().await;
    let proxy_addr = listener.local_addr().expect("listener local_addr");
    // No .upstream_default() → upstream_default = None.
    let config = ProxyConfig::builder()
        .port(41001)
        .build()
        .expect("proxy config (no upstream)");
    let pipeline = TransformPipeline::identity();
    let task = tokio::spawn(rskim_proxy::testing::run_server_with_listener(
        listener,
        config,
        pipeline,
        analytics,
    ));
    let abort = task.abort_handle();
    tokio::time::sleep(Duration::from_millis(80)).await;
    (abort, proxy_addr)
}

/// POST a JSON body to `/v1/messages` at the proxy and return the HTTP status.
async fn post_to_messages(proxy_addr: SocketAddr, body: &[u8]) -> u16 {
    use hyper::Uri;
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();
    let url: Uri = format!("http://{}/v1/messages", proxy_addr)
        .parse()
        .expect("url parse");
    let req = Request::post(url)
        .header("content-type", "application/json")
        .header("x-api-key", "test-key-tier")
        .body(Full::from(Bytes::from(body.to_vec())))
        .expect("request build");
    let resp = client.request(req).await.expect("proxy request");
    let status = resp.status().as_u16();
    // Consume body so AnalyticsCompletionBody fires.
    let _ = resp.into_body().collect().await;
    status
}

// ============================================================================
// Minimal echo upstream
// ============================================================================

/// Start a minimal echo upstream that returns 200 with the request body.
///
/// Returns the upstream URL string.
async fn start_echo_upstream() -> (String, tokio::task::AbortHandle) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("echo upstream bind");
    let addr = listener.local_addr().expect("local_addr");
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let svc = service_fn(|req: Request<hyper::body::Incoming>| async move {
                    let (_parts, body) = req.into_parts();
                    let body_bytes = body
                        .collect()
                        .await
                        .map(|b| b.to_bytes())
                        .unwrap_or_default();
                    let resp: Response<Full<Bytes>> = Response::builder()
                        .status(200)
                        .header("content-type", "application/json")
                        .body(Full::from(body_bytes))
                        .unwrap();
                    Ok::<_, std::convert::Infallible>(resp)
                });
                let _ = http1::Builder::new().serve_connection(io, svc).await;
            });
        }
    });
    (format!("http://{}", addr), task.abort_handle())
}

// ============================================================================
// AC23 / AD-PXY-21 — Tier derivation via synthetic stages
// ============================================================================

/// AD-PXY-21 / Cross-Plan Amendment #3: `RequestTier::Full` when
/// `"block-router"` emits a `Full` decision record.
///
/// The synthetic `BlockRouterFullStage` (named `"block-router"`) reduces the
/// body by one byte, causing `guarded_transform` to write a `Full` record to
/// the collecting sink. `derive_tier()` filters to `"block-router"` and
/// returns `RequestTier::Full`.
#[tokio::test]
async fn test_tier_full_when_block_router_modifies() {
    let (upstream_url, up_abort) = start_echo_upstream().await;

    let pipeline = TransformPipeline::from_stages(vec![Box::new(BlockRouterFullStage)]);

    let (hook, events, fired_count) = FullCapturingHook::new();
    let (proxy_abort, proxy_addr) =
        start_proxy_with_hook_and_pipeline(&upstream_url, Arc::new(hook), pipeline).await;

    // Send a non-empty body so BlockRouterFullStage can shrink it.
    let body = b"{\"model\":\"claude-3-opus-20240229\",\"messages\":[]}";
    let status = post_to_messages(proxy_addr, body).await;
    assert_eq!(status, 200, "upstream must echo successfully");

    // Give AnalyticsCompletionBody time to fire (it fires before .collect() returns).
    tokio::time::sleep(Duration::from_millis(20)).await;

    let count = fired_count.load(Ordering::SeqCst);
    assert_eq!(count, 1, "AD-PXY-24: exactly one event for success path (got {count})");

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].tier,
        RequestTier::Full,
        "AD-PXY-21: block-router Full record → RequestTier::Full"
    );

    proxy_abort.abort();
    up_abort.abort();
}

/// AD-PXY-21 / Cross-Plan Amendment #3: `RequestTier::Degraded` when
/// `"block-router"` emits a `Degraded` decision record.
///
/// `BlockRouterDegradedStage` sends `Degraded` directly to the sink and
/// returns passthrough bytes. `derive_tier()` sees the Degraded record and
/// returns `RequestTier::Degraded` (trumps Full by AD-PXY-21 rule).
#[tokio::test]
async fn test_tier_degraded_when_block_router_degrades() {
    let (upstream_url, up_abort) = start_echo_upstream().await;

    let pipeline = TransformPipeline::from_stages(vec![Box::new(BlockRouterDegradedStage)]);

    let (hook, events, fired_count) = FullCapturingHook::new();
    let (proxy_abort, proxy_addr) =
        start_proxy_with_hook_and_pipeline(&upstream_url, Arc::new(hook), pipeline).await;

    let body = b"{\"model\":\"gpt-4o\",\"messages\":[]}";
    let status = post_to_messages(proxy_addr, body).await;
    assert_eq!(status, 200);

    tokio::time::sleep(Duration::from_millis(20)).await;

    let count = fired_count.load(Ordering::SeqCst);
    assert_eq!(count, 1, "AD-PXY-24: exactly one event (got {count})");

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].tier,
        RequestTier::Degraded,
        "AD-PXY-21: Degraded trumps all — tier must be Degraded"
    );

    proxy_abort.abort();
    up_abort.abort();
}

/// Cross-Plan Amendment #3: `RequestTier::Passthrough` when only a
/// `"cache-align"` stage (not `"block-router"`) emits a `Full` record.
///
/// `derive_tier()` filters to `"block-router"` — the `"cache-align"` Full
/// record is excluded. With no qualifying block-router records, the tier is
/// `RequestTier::Passthrough`.
#[tokio::test]
async fn test_tier_passthrough_when_only_cache_align_modifies() {
    let (upstream_url, up_abort) = start_echo_upstream().await;

    let pipeline = TransformPipeline::from_stages(vec![Box::new(CacheAlignFullStage)]);

    let (hook, events, fired_count) = FullCapturingHook::new();
    let (proxy_abort, proxy_addr) =
        start_proxy_with_hook_and_pipeline(&upstream_url, Arc::new(hook), pipeline).await;

    let body = b"{\"model\":\"claude-3-sonnet\",\"messages\":[]}";
    let status = post_to_messages(proxy_addr, body).await;
    assert_eq!(status, 200);

    tokio::time::sleep(Duration::from_millis(20)).await;

    let count = fired_count.load(Ordering::SeqCst);
    assert_eq!(count, 1, "AD-PXY-24: exactly one event (got {count})");

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].tier,
        RequestTier::Passthrough,
        "Cross-Plan Amendment #3: cache-align Full must NOT affect tier"
    );

    proxy_abort.abort();
    up_abort.abort();
}

/// AD-PXY-21: Degraded trumps Full when both records are present.
///
/// A pipeline with both `BlockRouterDegradedStage` followed by another stage
/// that would emit Full — the tier must be Degraded (priority rule).
/// Here we use a two-stage pipeline: [BlockRouterDegradedStage, BlockRouterFullStage].
/// The Degraded record arrives first; the Full record follows. Result: Degraded.
#[tokio::test]
async fn test_tier_degraded_trumps_full_in_mixed_records() {
    let (upstream_url, up_abort) = start_echo_upstream().await;

    // Two stages: first emits Degraded, second emits Full.
    // Degraded must trump Full per AD-PXY-21.
    let pipeline = TransformPipeline::from_stages(vec![
        Box::new(BlockRouterDegradedStage) as Box<dyn TransformStage>,
        Box::new(BlockRouterFullStage),
    ]);

    let (hook, events, fired_count) = FullCapturingHook::new();
    let (proxy_abort, proxy_addr) =
        start_proxy_with_hook_and_pipeline(&upstream_url, Arc::new(hook), pipeline).await;

    // Body must have ≥2 bytes so BlockRouterFullStage can shrink by 1 after passthrough.
    let body = b"{\"model\":\"test\",\"x\":1}";
    let status = post_to_messages(proxy_addr, body).await;
    assert_eq!(status, 200);

    tokio::time::sleep(Duration::from_millis(20)).await;

    let count = fired_count.load(Ordering::SeqCst);
    assert_eq!(count, 1, "one event per request (got {count})");

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].tier,
        RequestTier::Degraded,
        "AD-PXY-21 priority: Degraded must trump Full"
    );

    proxy_abort.abort();
    up_abort.abort();
}

// ============================================================================
// AD-PXY-22 — Model detection
// ============================================================================

/// AD-PXY-22: `model` field populated verbatim from request body.
///
/// A JSON body with `"model": "claude-3-opus-20240229"` must yield
/// `event.model == Some("claude-3-opus-20240229")`. No normalization.
#[tokio::test]
async fn test_model_detected_in_analytics_event() {
    let (upstream_url, up_abort) = start_echo_upstream().await;

    let (hook, events, _) = FullCapturingHook::new();
    let (proxy_abort, proxy_addr) =
        start_proxy_with_hook(&upstream_url, Arc::new(hook)).await;

    let body = br#"{"model":"claude-3-opus-20240229","messages":[]}"#;
    let status = post_to_messages(proxy_addr, body).await;
    assert_eq!(status, 200);

    tokio::time::sleep(Duration::from_millis(20)).await;

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].model.as_deref(),
        Some("claude-3-opus-20240229"),
        "AD-PXY-22: model must be extracted verbatim (no normalization)"
    );

    proxy_abort.abort();
    up_abort.abort();
}

/// AD-PXY-22: `model` is `None` when body has no `"model"` key.
#[tokio::test]
async fn test_model_none_when_body_has_no_model_key() {
    let (upstream_url, up_abort) = start_echo_upstream().await;

    let (hook, events, _) = FullCapturingHook::new();
    let (proxy_abort, proxy_addr) =
        start_proxy_with_hook(&upstream_url, Arc::new(hook)).await;

    // Valid JSON but no "model" key.
    let body = br#"{"messages":[]}"#;
    let status = post_to_messages(proxy_addr, body).await;
    assert_eq!(status, 200);

    tokio::time::sleep(Duration::from_millis(20)).await;

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert!(
        recorded[0].model.is_none(),
        "AD-PXY-22: model must be None when absent from body"
    );

    proxy_abort.abort();
    up_abort.abort();
}

// ============================================================================
// AD-PXY-24 / AD-PXY-25 — Three-outcome recording predicate
// ============================================================================

/// AD-PXY-24: no analytics event for pre-transform failures.
///
/// Unknown provider + no default upstream → 502 before transform runs →
/// analytics event must NOT fire (transform never ran, no tier data).
///
/// This verifies that the Unknown+no-default path is a pre-transform failure
/// and the recording predicate fires NO event.
#[tokio::test]
async fn test_no_event_for_unknown_provider_no_default_upstream() {
    // No upstream_default → Unknown provider + no default → pre-transform 502.
    let (hook, _events, fired_count) = FullCapturingHook::new();
    let (proxy_abort, proxy_addr) = start_proxy_no_upstream_with_hook(Arc::new(hook)).await;

    // POST to a non-Anthropic, non-OpenAI path — provider = Unknown.
    use hyper::Uri;
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();
    let url: Uri = format!("http://{}/v1/unknown", proxy_addr)
        .parse()
        .expect("url parse");
    let req = Request::post(url)
        .header("content-type", "application/json")
        .body(Full::from(Bytes::from(b"{}".as_ref())))
        .expect("request build");
    let resp = client.request(req).await.expect("proxy request");
    let status = resp.status().as_u16();
    let _ = resp.into_body().collect().await;

    assert_eq!(status, 502, "pre-transform Unknown+no-default must 502");

    // Brief wait — even if a fire happened synchronously, this covers it.
    tokio::time::sleep(Duration::from_millis(30)).await;

    let count = fired_count.load(Ordering::SeqCst);
    assert_eq!(
        count, 0,
        "AD-PXY-24: pre-transform failure must NOT fire any analytics event (got {count})"
    );

    proxy_abort.abort();
}

/// AD-PXY-25: distinct analytics event with `upstream_error_status = Some(502)`
/// when the transform succeeded but no upstream URL is configured.
///
/// Config has no `upstream_default` and no per-provider upstream. An Anthropic
/// request (`/v1/messages`) passes the pre-transform check (provider != Unknown),
/// transform runs, then `upstream_url.is_empty()` → fire upstream-errored event
/// (AD-PXY-25) and return 502.
#[tokio::test]
async fn test_upstream_error_event_when_no_upstream_configured() {
    let (hook, events, fired_count) = FullCapturingHook::new();
    let (proxy_abort, proxy_addr) = start_proxy_no_upstream_with_hook(Arc::new(hook)).await;

    // Anthropic path, no upstream → post-transform 502.
    let body = br#"{"model":"claude-3-sonnet","messages":[]}"#;
    let status = post_to_messages(proxy_addr, body).await;

    assert_eq!(status, 502, "post-transform 502 — no upstream");

    tokio::time::sleep(Duration::from_millis(30)).await;

    let count = fired_count.load(Ordering::SeqCst);
    assert_eq!(
        count, 1,
        "AD-PXY-25: one distinct upstream-errored event must fire (got {count})"
    );

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].upstream_error_status,
        Some(502),
        "AD-PXY-25: upstream_error_status must be Some(502)"
    );

    proxy_abort.abort();
}

/// AD-PXY-23: analytics event fires at stream end (body fully consumed), not at
/// header time.
///
/// With `AnalyticsCompletionBody`, the event fires when the body's `poll_frame`
/// returns `None` (EOF). The `post_to_messages` helper calls `.collect().await`
/// which consumes the full body before returning, so the event must be present
/// immediately after the function returns (no extra sleep needed, but we add a
/// small margin for scheduling).
#[tokio::test]
async fn test_analytics_fires_at_stream_end_not_header_time() {
    let (upstream_url, up_abort) = start_echo_upstream().await;

    let (hook, events, fired_count) = FullCapturingHook::new();
    let (proxy_abort, proxy_addr) =
        start_proxy_with_hook(&upstream_url, Arc::new(hook)).await;

    let body = br#"{"model":"claude-3-haiku","messages":[]}"#;
    let status = post_to_messages(proxy_addr, body).await;
    assert_eq!(status, 200);

    // post_to_messages calls .collect().await — event should have fired by now.
    tokio::time::sleep(Duration::from_millis(10)).await;

    let count = fired_count.load(Ordering::SeqCst);
    assert_eq!(
        count, 1,
        "AD-PXY-23: event fires at stream end (got {count} fires)"
    );

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert!(
        recorded[0].duration.as_nanos() > 0,
        "AD-PXY-23: duration must be non-zero (measured at stream end)"
    );
    assert_eq!(
        recorded[0].upstream_error_status, None,
        "success path: upstream_error_status must be None"
    );

    proxy_abort.abort();
    up_abort.abort();
}

/// AD-PXY-21 / AD-PXY-23: `raw_body` and `final_body` populated correctly.
///
/// For the identity pipeline, `raw_body` == `final_body` (passthrough).
/// For a modifying stage, `raw_body` is the original bytes and
/// `final_body` is the transformed bytes.
#[tokio::test]
async fn test_raw_body_and_final_body_fields() {
    let (upstream_url, up_abort) = start_echo_upstream().await;

    let pipeline = TransformPipeline::from_stages(vec![Box::new(BlockRouterFullStage)]);

    let (hook, events, _) = FullCapturingHook::new();
    let (proxy_abort, proxy_addr) =
        start_proxy_with_hook_and_pipeline(&upstream_url, Arc::new(hook), pipeline).await;

    let body = b"hello-analytics-body";
    let status = post_to_messages(proxy_addr, body).await;
    assert_eq!(status, 200);

    tokio::time::sleep(Duration::from_millis(20)).await;

    let recorded = events.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    let event = &recorded[0];

    // raw_body must be the original request bytes.
    assert_eq!(
        event.raw_body.as_ref(),
        body.as_ref(),
        "raw_body must equal the original request body"
    );
    // final_body must be one byte shorter (BlockRouterFullStage shrinks by 1).
    assert_eq!(
        event.final_body.len(),
        body.len() - 1,
        "final_body must be the transformed (shrunk) body"
    );
    // request_bytes derived from raw_body.len()
    assert_eq!(
        event.request_bytes,
        body.len() as u64,
        "request_bytes must equal raw_body.len()"
    );

    proxy_abort.abort();
    up_abort.abort();
}
