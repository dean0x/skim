// Joint e2e tests: #303 proxy × #304 BlockRouter (AC19 / Phase 4a / D4 / PF-007).
//
// These tests require the `testing` feature to access `rskim_proxy::testing::run_server_async`.
// Run with:
//   cargo nextest run -p rskim-proxy --features testing -j 4
//   (or: cargo test -p rskim-proxy --all-targets --features testing)
#![cfg(feature = "testing")]

//! ## What these tests prove
//!
//! Three joint assertions (AC19 / D4 / PF-007):
//!
//! 1. **Compressible Anthropic live-zone fixture** — upstream-received body is
//!    STRICTLY SMALLER than client-sent body (real compression through the running proxy).
//!    DISCRIMINATING: this test would FAIL if the router were the identity stage
//!    (upstream bytes == client bytes, not strictly smaller). The fixture is Anthropic
//!    (OpenAI is non-mutable → passthrough until #332).
//!
//! 2. **Passthrough-only Anthropic fixture** — upstream-received bytes are BYTE-IDENTICAL
//!    to client-sent (D4 — proves #303 AC19b byte-faithfulness holds under the REAL router,
//!    not just IdentityContract). This fixture's live-zone blocks are all below the prefilter
//!    floor or contain only Text class (passthrough-only engines).
//!    DISCRIMINATING: this test would FAIL if `BlockRouter::serialize()` introduced any
//!    spurious drift on an unmodified body (the router's early-exit path skips serialize()
//!    when no block is modified, so byte identity is structural — not coincidental).
//!
//! 3. **Subscription-auth request** — upstream-received bytes are BYTE-IDENTICAL to
//!    client-sent (LosslessOnly policy → no compression regardless of content).
//!    DISCRIMINATING: this test would FAIL if the Subscription auth_mode were incorrectly
//!    mapped to Policy::Default (which might compress the body).
//!
//! ## Infrastructure
//!
//! These tests use a test-local `BlockRouterStage` that is structurally identical to the
//! production adapter in `crates/rskim/src/cmd/proxy.rs`. The adapter lives here (in the
//! test file) because the production adapter lives in the rskim binary, which cannot be
//! imported as a library. The test-local adapter has identical logic — both are the
//! bridge between `rskim_proxy::seam::TransformStage` and `rskim_compress::BlockRouter`.
//!
//! `FakeUpstream` and `ProxyHandle` are re-defined locally (same pattern as
//! `conformance_and_determinism.rs`) because test crate infrastructure is not shared.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use rskim_compress::{BlockRouter, Policy};
use rskim_contract::contract::Outcome;
use rskim_contract::log::{DecisionRecord, DecisionSink, SinkFull};
use rskim_proxy::analytics::NoopAnalyticsHook;
use rskim_proxy::authmode::AuthMode;
use rskim_proxy::config::ProxyConfig;
use rskim_proxy::detect::ProxyProvider;
use rskim_proxy::seam::{HeaderView, TransformContext, TransformPipeline, TransformStage};
use tokio::net::TcpListener;

// ============================================================================
// Test-local BlockRouterStage adapter (mirrors production adapter in proxy.rs)
// ============================================================================

/// Null sink for the BlockRouter `Contract` bridge (not called on the `apply()` path).
struct NullSink;

impl DecisionSink for NullSink {
    fn try_send(&self, _record: DecisionRecord) -> Result<(), SinkFull> {
        Ok(())
    }
}

/// `TransformStage` adapter wrapping `BlockRouter` — structurally identical to the
/// production adapter in `crates/rskim/src/cmd/proxy.rs`.
///
/// Lives here (in test code) because the production adapter is in the rskim binary,
/// which cannot be imported as a library. The logic is identical — this is the
/// canonical test surface for verifying the proxy × router composition.
///
/// ## auth_mode → Policy mapping (D1 / AD-PXY-08)
///
/// | `AuthMode`     | `Policy`       | Rationale                                |
/// |----------------|----------------|------------------------------------------|
/// | `Subscription` | `LosslessOnly` | Conservative: no lossy compression       |
/// | `ApiKey`       | `Default`      | Full compression allowed                 |
/// | `Ambiguous`    | `Default`      | Conservative map toward ApiKey (D1)      |
struct BlockRouterStage {
    router: BlockRouter,
}

impl BlockRouterStage {
    fn new(router: BlockRouter) -> Self {
        Self { router }
    }
}

impl TransformStage for BlockRouterStage {
    fn name(&self) -> &'static str {
        "block-router"
    }

    fn apply(&self, body: &[u8], ctx: &TransformContext<'_>, sink: &dyn DecisionSink) -> Outcome {
        let policy = match ctx.auth_mode {
            AuthMode::Subscription => Policy::LosslessOnly,
            AuthMode::ApiKey => Policy::Default,
            // Ambiguous → Default (D1: conservative toward ApiKey)
            _ => Policy::Default,
        };
        self.router.route(body, policy, ctx.request_id, sink)
    }
}

// ============================================================================
// Fake upstream server (same pattern as conformance_and_determinism.rs)
// ============================================================================

type CapturedBody = Vec<u8>;

struct FakeUpstream {
    addr: SocketAddr,
    captured: Arc<Mutex<Vec<CapturedBody>>>,
}

impl FakeUpstream {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake upstream bind");
        let addr = listener.local_addr().expect("fake upstream local_addr");
        let captured: Arc<Mutex<Vec<CapturedBody>>> = Arc::new(Mutex::new(Vec::new()));

        let captured_clone = Arc::clone(&captured);
        tokio::spawn(async move {
            loop {
                let (stream, _peer) = match listener.accept().await {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let captured_inner = Arc::clone(&captured_clone);
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let svc = service_fn(move |req: Request<hyper::body::Incoming>| {
                        let cap = Arc::clone(&captured_inner);
                        async move {
                            let body_bytes = req
                                .into_body()
                                .collect()
                                .await
                                .map(|b| b.to_bytes())
                                .unwrap_or_else(|_| Bytes::new());
                            let body_vec = body_bytes.to_vec();
                            cap.lock().expect("captured lock").push(body_vec.clone());
                            let response: Response<Full<Bytes>> = Response::builder()
                                .status(200)
                                .header("content-type", "application/json")
                                .body(Full::from(Bytes::from(body_vec)))
                                .expect("echo response build");
                            Ok::<_, std::convert::Infallible>(response)
                        }
                    });
                    if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                        let _ = e;
                    }
                });
            }
        });

        Self { addr, captured }
    }

    fn upstream_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn drain_captured(&self) -> Vec<CapturedBody> {
        self.captured
            .lock()
            .expect("captured lock")
            .drain(..)
            .collect()
    }
}

// ============================================================================
// Proxy test harness
// ============================================================================

struct ProxyHandle {
    abort_handle: tokio::task::AbortHandle,
    proxy_addr: SocketAddr,
}

/// Scan for a free port within the block_router_e2e subrange (41600-41700).
///
/// Uses a unique per-test hash of the thread ID to reduce collision probability
/// when nextest runs tests as separate processes — each process starts scanning
/// from a different offset.
///
/// D8/AD-PXY-03: all ports are within the 41000-49000 allowed range.
/// The 41600-41700 subrange is distinct from conformance_and_determinism.rs
/// (41100-41900 starting scan) and leaves room for other test files.
async fn find_proxy_test_port() -> u16 {
    use std::time::{SystemTime, UNIX_EPOCH};

    // Mix process ID + current time nanos for a unique per-invocation starting offset.
    // This avoids the TOCTOU race more reliably than a sequential scan when tests
    // start simultaneously in separate nextest processes.
    let pid = std::process::id() as u64;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(pid);
    let seed = (pid ^ nanos ^ (pid << 16)) as u16;

    // Scan within 41600-41700 (100-port window for block_router_e2e tests).
    let base: u16 = 41600;
    let range: u16 = 100;
    let start_offset = seed % range;
    for i in 0..range {
        let port = base + (start_offset + i) % range;
        if TcpListener::bind(format!("127.0.0.1:{port}")).await.is_ok() {
            return port;
        }
    }
    panic!("no free port found in 41600-41700 block_router_e2e subrange");
}

impl ProxyHandle {
    /// Start a proxy with the BlockRouter pipeline (Phase 4a wiring).
    async fn start_with_router(upstream_url: &str) -> Self {
        let port = find_proxy_test_port().await;
        let config = ProxyConfig::builder()
            .port(port)
            .upstream_default(upstream_url)
            .build()
            .expect("proxy config");
        let proxy_addr = config.bind_addr();

        let router = BlockRouter::new(Arc::new(NullSink));
        let stage = BlockRouterStage::new(router);
        let pipeline = TransformPipeline::from_stages(vec![Box::new(stage)]);
        let analytics = Arc::new(NoopAnalyticsHook);

        let task = tokio::spawn(rskim_proxy::testing::run_server_async(
            config, pipeline, analytics,
        ));
        let abort_handle = task.abort_handle();

        // Allow the proxy to bind and start accepting.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        Self {
            abort_handle,
            proxy_addr,
        }
    }

    fn proxy_addr(&self) -> SocketAddr {
        self.proxy_addr
    }

    fn stop(self) {
        self.abort_handle.abort();
    }
}

// ============================================================================
// HTTP client helpers
// ============================================================================

/// POST `body` to `http://{addr}/v1/messages` with an API-key auth header.
async fn post_with_api_key(proxy_addr: SocketAddr, body: &[u8]) -> Vec<u8> {
    use hyper::Uri;
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let url: Uri = format!("http://{}/v1/messages", proxy_addr)
        .parse()
        .expect("proxy URL parse");

    let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();

    let request = Request::post(url)
        .header("content-type", "application/json")
        .header("x-api-key", "test-api-key-not-real")
        .body(Full::from(Bytes::from(body.to_vec())))
        .expect("request build");

    let response = client.request(request).await.expect("proxy request");
    response
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes().to_vec())
        .unwrap_or_default()
}

/// POST `body` to `http://{addr}/v1/messages` with a Bearer subscription auth header.
async fn post_with_bearer(proxy_addr: SocketAddr, body: &[u8]) -> Vec<u8> {
    use hyper::Uri;
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let url: Uri = format!("http://{}/v1/messages", proxy_addr)
        .parse()
        .expect("proxy URL parse");

    let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();

    let request = Request::post(url)
        .header("content-type", "application/json")
        .header("authorization", "Bearer eyJhbGciOiJSUzI1NiJ9.TEST-NOT-REAL")
        .body(Full::from(Bytes::from(body.to_vec())))
        .expect("request build");

    let response = client.request(request).await.expect("proxy request");
    response
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes().to_vec())
        .unwrap_or_default()
}

// ============================================================================
// Fixture builders
// ============================================================================

/// Build an Anthropic request body containing a large Rust code block in the live zone.
///
/// The code is > 64 bytes (above MIN_SIZE_FLOOR) and contains multiple functions,
/// making it eligible for Code engine compression via rskim-core structure mode.
/// The user message has no preceding assistant message → the entire messages array
/// is live zone → the code block is a candidate.
///
/// IMPORTANT: This fixture MUST produce strictly-smaller output through the router.
/// Verified by test assertion. If this function changes, re-verify compression is > 0.
fn compressible_anthropic_body() -> Vec<u8> {
    // Build a Rust code block that is large enough to compress but small enough
    // to stay under the prefilter ceiling. ~600 bytes of valid Rust with multiple
    // functions — rskim-core structure mode will strip bodies, reducing byte count.
    let mut code = String::new();
    for i in 0..12 {
        code.push_str(&format!(
            "fn compute_{i}(a: u32, b: u32, c: u32) -> u32 {{\n    let x = a + b;\n    let y = x * c;\n    let z = y - a;\n    z / (b + 1)\n}}\n\n"
        ));
    }

    // JSON-escape the code block for embedding in the message content string.
    let escaped = code
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");

    // Use a code block with a Rust language hint so the Code engine is selected.
    let content = format!("```rust\n{escaped}\n```");
    let content_escaped = content
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r");

    format!(
        r#"{{"model":"claude-3-5-sonnet-20241022","max_tokens":1024,"messages":[{{"role":"user","content":"{content_escaped}"}}]}}"#
    )
    .into_bytes()
}

/// Build an Anthropic request body where all live-zone blocks are below the prefilter
/// floor (< 64 bytes) or are Text class (passthrough engine).
///
/// The router will exit early (no block modified) without calling `serialize()`,
/// ensuring byte-identical output. This proves the D4 serialize-fail-open path
/// does NOT affect the no-op case.
fn passthrough_only_anthropic_body() -> Vec<u8> {
    // A tiny user message well below MIN_SIZE_FLOOR (64 bytes).
    // The prefilter will skip it → no modification → serialize() never called
    // → upstream receives the exact input bytes.
    br#"{"model":"claude-3-5-sonnet-20241022","max_tokens":512,"messages":[{"role":"user","content":"hi"}]}"#.to_vec()
}

// ============================================================================
// Joint e2e tests (AC19 / D4 / PF-007)
// ============================================================================

/// AC19 / D4 / PF-007 Joint Test 1:
///
/// COMPRESSIBLE Anthropic live-zone fixture → upstream-received body is STRICTLY
/// SMALLER than client-sent body.
///
/// ## Discriminating property (PF-007)
///
/// This test FAILS if the router were identity (bytes would equal, not less).
/// The compressible fixture was chosen specifically so that structure-mode compression
/// of the code block produces a smaller output. A router that is merely an identity
/// stage cannot satisfy `upstream_len < client_len`.
///
/// ## Why Anthropic only?
///
/// OpenAI bodies are non-mutable (`list_blocks` returns empty, `mutate_block` →
/// `BlockNotMutable` per #332). The router correctly passes OpenAI bodies through
/// byte-identical. Only Anthropic bodies can be compressed today.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_joint_compressible_anthropic_produces_strictly_smaller_upstream_body() {
    let upstream = FakeUpstream::start().await;
    let proxy = ProxyHandle::start_with_router(&upstream.upstream_url()).await;

    let client_body = compressible_anthropic_body();
    let client_len = client_body.len();

    // POST through the proxy with ApiKey auth → Policy::Default → compression enabled.
    upstream.drain_captured();
    let response = post_with_api_key(proxy.proxy_addr(), &client_body).await;

    // Allow the upstream handler to finish recording.
    // Use a longer wait (100ms) to reduce timing sensitivity under parallel load.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let captured = upstream.drain_captured();
    assert!(
        !captured.is_empty(),
        "AC19/Joint-1: upstream must record the forwarded body \
         (proxy response was {} bytes; a 502 body is typical if upstream was unreachable)",
        response.len(),
    );

    let upstream_len = captured[0].len();

    // DISCRIMINATING assertion (PF-007): upstream body must be STRICTLY SMALLER.
    // If the router is identity, upstream_len == client_len → test FAILS.
    // If compression actually runs, upstream_len < client_len → test PASSES.
    assert!(
        upstream_len < client_len,
        "AC19/Joint-1 FAIL: upstream received {} bytes, client sent {} bytes — \
         expected upstream < client (real compression must shrink the body). \
         If this fails, check: (a) code block size >= MIN_SIZE_FLOOR, \
         (b) BlockRouter is wired (not IdentityStage), \
         (c) auth_mode=ApiKey → Policy::Default path is taken.",
        upstream_len,
        client_len,
    );

    proxy.stop();
}

/// AC19 / D4 / PF-007 Joint Test 2:
///
/// PASSTHROUGH-ONLY Anthropic fixture → upstream-received bytes are BYTE-IDENTICAL
/// to client-sent bytes.
///
/// ## Why this is the D4 joint test
///
/// D4 extends AD-009: any `serialize()`/`parse()` failure → whole-request passthrough.
/// But the critical compositional guarantee is that the router's NO-OP path (no block
/// modified → `serialize()` never called) produces EXACT byte identity under the REAL
/// proxy forward path — not just at the seam level.
///
/// This proves #303's AC19b byte-faithfulness holds under the REAL router (not just
/// IdentityContract), and that BlockRouter's early-exit path (skip serialize() when
/// no block modified) is byte-faithful through the running proxy's HTTP stack.
///
/// ## Discriminating property (PF-007)
///
/// This test FAILS if `BlockRouter::serialize()` introduces spurious drift on an
/// unmodified body (e.g., if the no-modification path were incorrectly changed to
/// call `serialize()` and serde round-trip changed whitespace or key order).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_joint_passthrough_only_anthropic_is_byte_identical_upstream() {
    let upstream = FakeUpstream::start().await;
    let proxy = ProxyHandle::start_with_router(&upstream.upstream_url()).await;

    let client_body = passthrough_only_anthropic_body();

    // POST through the proxy with ApiKey auth → Policy::Default → compression attempted,
    // but all blocks are below prefilter floor → early exit → no serialize() call.
    upstream.drain_captured();
    let _response = post_with_api_key(proxy.proxy_addr(), &client_body).await;

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let captured = upstream.drain_captured();
    assert!(
        !captured.is_empty(),
        "AC19b/D4/Joint-2: upstream must record the forwarded body"
    );

    // DISCRIMINATING assertion (PF-007): upstream body must be BYTE-IDENTICAL.
    // If serialize() were called on the unmodified path and introduced drift
    // (e.g., serde re-emitting with different whitespace), bytes would differ → FAIL.
    assert_eq!(
        captured[0].as_slice(),
        client_body.as_slice(),
        "AC19b/D4/Joint-2 FAIL: upstream received different bytes than client sent. \
         \n  client ({} bytes): {:?}\
         \n  upstream ({} bytes): {:?}\
         \nThis proves #303 byte-faithfulness breaks under the real router. \
         Cause: serialize() must not be called when no block is modified.",
        client_body.len(),
        &client_body[..client_body.len().min(80)],
        captured[0].len(),
        &captured[0][..captured[0].len().min(80)],
    );

    proxy.stop();
}

/// AC19 / D4 / PF-007 Joint Test 3:
///
/// SUBSCRIPTION-AUTH request → upstream-received bytes are BYTE-IDENTICAL to
/// client-sent bytes (LosslessOnly policy → no compression regardless of content).
///
/// ## Discriminating property (PF-007)
///
/// This test FAILS if the Subscription auth_mode were incorrectly mapped to
/// Policy::Default. With a compressible body under Default policy, the router
/// would modify blocks and the upstream would receive fewer bytes.
/// With LosslessOnly, every candidate gets a PolicyPassthrough record and the
/// body is forwarded byte-identical.
///
/// The body used here IS compressible (same fixture as Joint Test 1). This ensures
/// that if Policy::Default were applied, the test would fail — making the auth_mode
/// mapping the sole discriminating variable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_joint_subscription_auth_is_byte_identical_upstream() {
    let upstream = FakeUpstream::start().await;
    let proxy = ProxyHandle::start_with_router(&upstream.upstream_url()).await;

    // Use the COMPRESSIBLE body — if Default policy ran, the upstream would
    // receive fewer bytes. If LosslessOnly runs, bytes are identical.
    // This makes the auth_mode → Policy mapping the discriminating variable.
    let client_body = compressible_anthropic_body();

    // POST through the proxy with Bearer subscription auth → Subscription auth_mode
    // → Policy::LosslessOnly → no compression.
    upstream.drain_captured();
    let _response = post_with_bearer(proxy.proxy_addr(), &client_body).await;

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let captured = upstream.drain_captured();
    assert!(
        !captured.is_empty(),
        "AC19/Joint-3: upstream must record the forwarded body"
    );

    // DISCRIMINATING assertion (PF-007): upstream body must be BYTE-IDENTICAL.
    // If Subscription were incorrectly mapped to Default, the upstream would
    // receive compressed bytes (strictly smaller) → assertion fails.
    assert_eq!(
        captured[0].as_slice(),
        client_body.as_slice(),
        "AC19/Joint-3 FAIL: Subscription-auth body must be byte-identical at upstream. \
         \n  client ({} bytes): {:?}\
         \n  upstream ({} bytes): {:?}\
         \nIf upstream is smaller: Subscription was incorrectly mapped to Default (not LosslessOnly). \
         If upstream is larger: whole_request_check rejected a valid compressed body (impossible). \
         Check the auth_mode → Policy mapping in BlockRouterStage::apply().",
        client_body.len(),
        &client_body[..client_body.len().min(80)],
        captured[0].len(),
        &captured[0][..captured[0].len().min(80)],
    );

    proxy.stop();
}

// ============================================================================
// Smoke test: verify the test-local BlockRouterStage applies D1 mapping correctly
// (seam-level, no running proxy needed)
// ============================================================================

/// Seam-level sanity check: BlockRouterStage applies D1 auth_mode → Policy mapping.
///
/// This test drives the stage directly (no running proxy), verifying the adapter
/// bridge logic before the full e2e tests. If the bridge is broken, this fails
/// fast and clearly.
#[test]
fn test_block_router_stage_d1_auth_mode_to_policy_mapping() {
    use rskim_contract::log::{MockSink, OutcomeReason};

    // A tiny Anthropic body — has candidates (live-zone user message) but all
    // below the prefilter floor. Under Default, they're Passthrough (prefilter).
    // Under LosslessOnly, they're PolicyPassthrough.
    let body = br#"{"model":"claude-3-5-sonnet-20241022","max_tokens":1024,"messages":[{"role":"user","content":"hi"}]}"#;

    let build_stage = || {
        let router = BlockRouter::new(Arc::new(NullSink));
        BlockRouterStage::new(router)
    };

    let call = |auth_mode: AuthMode| -> Vec<rskim_contract::log::DecisionRecord> {
        let stage = build_stage();
        let headers: Vec<(String, String)> = vec![];
        let hv = HeaderView::new(&headers);
        let ctx = TransformContext::new(ProxyProvider::Anthropic, auth_mode, "test-req", &hv);
        let sink = MockSink::new();
        let _outcome = stage.apply(body, &ctx, &sink);
        sink.drain()
    };

    // Subscription → LosslessOnly: any records must be PolicyPassthrough (if candidates exist).
    let records_sub = call(AuthMode::Subscription);
    for r in &records_sub {
        assert_eq!(
            r.reason,
            OutcomeReason::PolicyPassthrough,
            "Subscription must produce PolicyPassthrough records, got {:?}",
            r.reason
        );
    }

    // ApiKey → Default: NO records must be PolicyPassthrough.
    let records_api = call(AuthMode::ApiKey);
    for r in &records_api {
        assert_ne!(
            r.reason,
            OutcomeReason::PolicyPassthrough,
            "ApiKey must NOT produce PolicyPassthrough records, got {:?}",
            r.reason
        );
    }

    // Ambiguous → Default: NO records must be PolicyPassthrough.
    let records_amb = call(AuthMode::Ambiguous);
    for r in &records_amb {
        assert_ne!(
            r.reason,
            OutcomeReason::PolicyPassthrough,
            "Ambiguous must NOT produce PolicyPassthrough records, got {:?}",
            r.reason
        );
    }
}
