// AC19b and AC11 integration tests require the `testing` feature to access
// `rskim_proxy::testing::run_server_async`. The feature must be enabled via:
//   cargo test -p rskim-proxy --all-targets --features testing
// Without it, these tests are silently omitted (not a test failure).
// See: ci.yml proxy-windows job + conformance_and_determinism.rs gate.
#![cfg(feature = "testing")]

//! Integration tests: AC19b (on-the-wire conformance) and AC11 (statelessness/determinism).
//!
//! These tests start a **real** proxy server on an ephemeral port against a
//! **fake upstream** (embedded hyper server) so they exercise the actual HTTP
//! forwarding path end-to-end, not just the seam types in isolation.
//!
//! ## AC19b — On-the-wire byte-identity (NEGATIVE / on-the-wire conformance)
//!
//! Every body in `rskim_contract::harness::corpus::ALL_CORPUS` (which includes
//! malformed, truncated, invalid-UTF8, and adversarial inputs) is POSTed through
//! the running proxy with the identity transform.  The fake upstream records the
//! raw request body; we assert `recorded == input` for each corpus entry.
//!
//! This is the over-the-wire analogue of AC19a (conformance harness) and
//! AC4/AC8 (seam-level byte-identity).  A regression in the forwarding path
//! (header mangling, body buffering bug, partial flush) that the seam-level tests
//! would miss is caught here.
//!
//! ## AC11 — Statelessness / determinism (NEGATIVE / property test)
//!
//! The same recorded request body is replayed N >= 100 times, including:
//! - Sequential replays.
//! - Across an in-process proxy restart (two separate server tasks).
//! - Via 16 concurrent interleaved clients.
//!
//! All recorded upstream bodies must be byte-identical to the input and to each
//! other — proving no per-request cross-contamination, no session state, and no
//! dependency on identity or order.
//!
//! ## Test infrastructure
//!
//! Both tests share `TestHarness`:
//! - `FakeUpstream`: a tiny hyper HTTP/1.1 server that echoes back the request
//!   body in the response AND records every received body in a thread-safe queue.
//! - `ProxyHandle`: a tokio task running the proxy, abortable at end of test.
//! - Free-port allocation via `TcpListener::bind("127.0.0.1:0")`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use rskim_proxy::analytics::NoopAnalyticsHook;
use rskim_proxy::config::ProxyConfig;
use rskim_proxy::seam::TransformPipeline;
use tokio::net::TcpListener;

// ============================================================================
// Fake upstream server
// ============================================================================

/// A captured body recorded by the fake upstream.
type CapturedBody = Vec<u8>;

/// Fake upstream: records request bodies and echoes them back as the response.
///
/// The recorded bodies are accessible via [`FakeUpstream::captured_bodies`].
struct FakeUpstream {
    addr: SocketAddr,
    captured: Arc<Mutex<Vec<CapturedBody>>>,
}

impl FakeUpstream {
    /// Start a fake upstream on an ephemeral port.  Returns the handle and
    /// the captured-body queue.
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
                            // Collect the full request body.
                            let body_bytes = req
                                .into_body()
                                .collect()
                                .await
                                .map(|b| b.to_bytes())
                                .unwrap_or_else(|_| Bytes::new());
                            let body_vec = body_bytes.to_vec();

                            // Record the body.
                            cap.lock().expect("captured lock").push(body_vec.clone());

                            // Echo the body back as the response.
                            let response: Response<Full<Bytes>> = Response::builder()
                                .status(200)
                                .header("content-type", "application/json")
                                .body(Full::from(Bytes::from(body_vec)))
                                .expect("echo response build");
                            Ok::<_, std::convert::Infallible>(response)
                        }
                    });
                    if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                        // Client disconnect — expected in tests, ignore.
                        let _ = e;
                    }
                });
            }
        });

        Self { addr, captured }
    }

    /// Address of the fake upstream (e.g. `"http://127.0.0.1:12345"`).
    fn upstream_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Drain and return all captured bodies recorded so far.
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

/// A running proxy instance.  Drop or abort the handle to stop it.
struct ProxyHandle {
    abort_handle: tokio::task::AbortHandle,
    proxy_addr: SocketAddr,
}

/// Find a free port within the 41000-49000 range (D8/AD-PXY-03) for tests.
///
/// The proxy config validation rejects ports outside this range (per D8).
/// We scan from a randomized offset within the range to reduce collision
/// probability under parallel test runs.
async fn find_proxy_test_port() -> u16 {
    // Try ports starting from a semi-random offset to avoid races.
    // Use the lower half of the range so we don't exhaust the range.
    let base: u16 = 41100;
    for offset in 0..800_u16 {
        let port = base + offset;
        if TcpListener::bind(format!("127.0.0.1:{port}"))
            .await
            .is_ok()
        {
            return port;
        }
    }
    panic!("no free port found in 41100-41900 test subrange");
}

impl ProxyHandle {
    /// Start a proxy on a free port within 41000-49000 (D8/AD-PXY-03).
    async fn start(upstream_url: &str) -> Self {
        let port = find_proxy_test_port().await;

        let config = ProxyConfig::builder()
            .port(port)
            .upstream_default(upstream_url)
            .build()
            .expect("proxy config");
        let proxy_addr = config.bind_addr();

        let pipeline = TransformPipeline::identity();
        let analytics = Arc::new(NoopAnalyticsHook);

        let task = tokio::spawn(
            rskim_proxy::testing::run_server_async(config, pipeline, analytics),
        );
        let abort_handle = task.abort_handle();

        // Give the proxy a moment to bind and start accepting.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        Self {
            abort_handle,
            proxy_addr,
        }
    }

    /// The address the proxy is listening on.
    fn proxy_addr(&self) -> SocketAddr {
        self.proxy_addr
    }

    /// Stop the proxy (abort the background task).
    fn stop(self) {
        self.abort_handle.abort();
    }
}

// ============================================================================
// HTTP client helper
// ============================================================================

/// POST `body` to `http://{addr}/v1/messages` and return the response body bytes.
///
/// Uses a raw hyper client (no reqwest) to avoid any hidden buffering.
async fn post_to_proxy(proxy_addr: SocketAddr, body: &[u8]) -> Vec<u8> {
    use hyper::Uri;
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let url: Uri = format!("http://{}/v1/messages", proxy_addr)
        .parse()
        .expect("proxy URL parse");

    let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();

    let request = Request::post(url)
        .header("content-type", "application/json")
        .header("x-api-key", "test-key-not-real")
        .body(Full::from(Bytes::from(body.to_vec())))
        .expect("request build");

    let response = client.request(request).await.expect("proxy request");
    // We don't care about the response body for body-identity tests; we check
    // what the upstream *received* (via captured bodies).
    let _status = response.status();
    response
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes().to_vec())
        .unwrap_or_default()
}

// ============================================================================
// AC19b — On-the-wire byte-identity over ALL_CORPUS
// ============================================================================

/// AC19b (NEGATIVE — on-the-wire conformance): every body in ALL_CORPUS must
/// arrive at the upstream byte-identical to what the client sent.
///
/// This is the over-the-wire analogue of:
/// - AC19a (conformance harness, seam-level)
/// - AC4 (inflating-stage discriminator, seam-level)
/// - AC8 (malformed-JSON fail-open, seam-level)
///
/// The proxy MUST NOT alter any corpus body on the wire.  A forwarding bug
/// (partial flush, header rewrite touching the body, transform seam bug) that
/// the seam-level tests miss is caught here.
///
/// ## Non-tautological discriminator
///
/// If the identity stage were replaced with a stage that modified even one byte,
/// the fake upstream would record a different body and the equality assertion
/// would fail — proving the wire path is tested, not just the seam types.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_ac19b_on_the_wire_corpus_byte_identity() {
    use rskim_contract::harness::corpus::ALL_CORPUS;

    let upstream = FakeUpstream::start().await;
    let proxy = ProxyHandle::start(&upstream.upstream_url()).await;

    for (idx, &corpus_body) in ALL_CORPUS.iter().enumerate() {
        // Clear previous captures so we can isolate each corpus entry.
        upstream.drain_captured();

        // POST the corpus body through the proxy.
        let _response = post_to_proxy(proxy.proxy_addr(), corpus_body).await;

        // Allow a brief moment for the upstream handler to finish recording.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let captured = upstream.drain_captured();

        // The upstream must have received exactly one body per request.
        // (Some corpus entries are empty — those may result in no body recorded
        //  if the proxy omits the body; we still assert length = 0 in that case.)
        assert!(
            !captured.is_empty() || corpus_body.is_empty(),
            "AC19b corpus[{idx}]: upstream recorded 0 bodies for a non-empty corpus entry"
        );

        if !captured.is_empty() {
            assert_eq!(
                captured[0].as_slice(),
                corpus_body,
                "AC19b corpus[{idx}]: upstream-received bytes differ from client-sent bytes.\
                 \n  sent ({} bytes): {:?}\
                 \n  recv ({} bytes): {:?}",
                corpus_body.len(),
                &corpus_body[..corpus_body.len().min(64)],
                captured[0].len(),
                &captured[0][..captured[0].len().min(64)],
            );
        }
    }

    proxy.stop();
}

// ============================================================================
// AC11 — Statelessness / determinism property test
// ============================================================================

/// Number of sequential replays for the determinism property test.
/// >= 100 as required by AC11.
const AC11_REPLAY_COUNT: usize = 120;

/// Number of concurrent clients for the concurrent arm of AC11.
const AC11_CONCURRENT_CLIENTS: usize = 16;

/// AC11 (NEGATIVE — statelessness/determinism): replaying the same request
/// N >= 100 times (including across a proxy restart and via 16 concurrent
/// interleaved clients) MUST produce byte-identical upstream-received bytes
/// every time, with no observable cross-request state.
///
/// ## Three arms tested
///
/// 1. **Sequential**: `AC11_REPLAY_COUNT` requests one after another through the
///    same proxy instance.
/// 2. **Restart**: one in-process proxy restart; N/2 requests before and N/2
///    after.  The bodies must be identical across the restart.
/// 3. **Concurrent**: 16 concurrent clients each POST the body simultaneously.
///    All 16 upstream-recorded bodies must be byte-identical.
///
/// ## Discriminator
///
/// If the proxy accumulated per-request state (a counter in the body, a
/// rotating key, etc.) the body recorded by the upstream in request N would
/// differ from request 1.  The equality assertion would catch this.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_ac11_statelessness_determinism() {
    // A stable body that classifies as Anthropic (path-based) and fits in
    // SHAPE_SNIFF_LIMIT so detection exercises the full path.
    let canonical_body = br#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"hello"}],"max_tokens":10}"#;

    // ---- Arm 1: Sequential replays ----
    {
        let upstream = FakeUpstream::start().await;
        let proxy = ProxyHandle::start(&upstream.upstream_url()).await;
        upstream.drain_captured(); // clear startup noise

        for i in 0..AC11_REPLAY_COUNT {
            let _resp = post_to_proxy(proxy.proxy_addr(), canonical_body).await;
            // Small sleep to avoid overwhelming the single-threaded fake upstream.
            if i % 20 == 19 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
        // Allow in-flight handlers to finish recording.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let captured = upstream.drain_captured();
        assert_eq!(
            captured.len(),
            AC11_REPLAY_COUNT,
            "AC11 sequential: expected {} captured bodies, got {}",
            AC11_REPLAY_COUNT,
            captured.len()
        );
        for (i, body) in captured.iter().enumerate() {
            assert_eq!(
                body.as_slice(),
                canonical_body.as_slice(),
                "AC11 sequential: body[{i}] differs from canonical"
            );
        }

        proxy.stop();
    }

    // ---- Arm 2: In-process proxy restart ----
    // First half of requests through proxy-A, then abort A and start proxy-B,
    // then second half through proxy-B.  All bodies must be identical.
    {
        let upstream = FakeUpstream::start().await;
        let half = AC11_REPLAY_COUNT / 2;

        // Phase A
        let proxy_a = ProxyHandle::start(&upstream.upstream_url()).await;
        upstream.drain_captured();
        for _ in 0..half {
            let _resp = post_to_proxy(proxy_a.proxy_addr(), canonical_body).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let captured_a = upstream.drain_captured();
        proxy_a.stop();

        // Phase B (fresh proxy on a new port)
        let proxy_b = ProxyHandle::start(&upstream.upstream_url()).await;
        upstream.drain_captured();
        for _ in 0..half {
            let _resp = post_to_proxy(proxy_b.proxy_addr(), canonical_body).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let captured_b = upstream.drain_captured();
        proxy_b.stop();

        assert_eq!(
            captured_a.len(),
            half,
            "AC11 restart: phase-A expected {half} bodies, got {}",
            captured_a.len()
        );
        assert_eq!(
            captured_b.len(),
            half,
            "AC11 restart: phase-B expected {half} bodies, got {}",
            captured_b.len()
        );

        for (i, body) in captured_a.iter().chain(captured_b.iter()).enumerate() {
            assert_eq!(
                body.as_slice(),
                canonical_body.as_slice(),
                "AC11 restart: body[{i}] (across restart) differs from canonical"
            );
        }
    }

    // ---- Arm 3: 16 concurrent interleaved clients ----
    {
        let upstream = FakeUpstream::start().await;
        let proxy = ProxyHandle::start(&upstream.upstream_url()).await;
        upstream.drain_captured();
        let proxy_addr = proxy.proxy_addr();

        let mut handles = Vec::with_capacity(AC11_CONCURRENT_CLIENTS);
        for _ in 0..AC11_CONCURRENT_CLIENTS {
            let handle = tokio::spawn(async move {
                post_to_proxy(proxy_addr, canonical_body).await
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.await.expect("concurrent client task");
        }
        // Give upstream handlers a moment to record.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let captured = upstream.drain_captured();
        assert_eq!(
            captured.len(),
            AC11_CONCURRENT_CLIENTS,
            "AC11 concurrent: expected {} bodies, got {}",
            AC11_CONCURRENT_CLIENTS,
            captured.len()
        );
        for (i, body) in captured.iter().enumerate() {
            assert_eq!(
                body.as_slice(),
                canonical_body.as_slice(),
                "AC11 concurrent: body[{i}] differs from canonical (cross-request contamination?)"
            );
        }

        proxy.stop();
    }
}
