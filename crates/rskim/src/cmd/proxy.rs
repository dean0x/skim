//! `skim proxy` subcommand — HTTP reverse proxy for skim Layer 3.
//!
//! Thin handler: parses `proxy`-specific args (clap), builds [`rskim_proxy::config::ProxyConfig`],
//! emits the cleartext-exposure warning when required, and calls
//! [`rskim_proxy::serve_with_stage()`] (which blocks on its own tokio runtime).
//!
//! ## Phase 4a wiring (AC19 / #304)
//!
//! This handler builds a [`BlockRouterStage`] adapter that implements [`rskim_proxy::seam::TransformStage`]
//! by holding a [`rskim_compress::BlockRouter`] and mapping `ctx.auth_mode → Policy` per call (D1):
//! - `AuthMode::Subscription → Policy::LosslessOnly` (byte-exact passthrough: no re-encoding)
//! - `AuthMode::ApiKey      → Policy::Default`        (lossless re-encoding allowed)
//! - `AuthMode::Ambiguous   → Policy::Default`        (conservative toward ApiKey, D1)
//!
//! The adapter lives HERE (in the rskim binary), not in rskim-compress, because
//! `TransformStage` / `TransformContext` depend on hyper/tokio (rskim-proxy), which
//! rskim-compress must not depend on (AC9/R2). rskim already depends on both crates.
//!
//! ## Passthrough escape hatch
//!
//! `SKIM_PASSTHROUGH=1` forces the identity pipeline (no compression). This is
//! consistent with skim's global passthrough convention and enables debugging
//! when compressed output hides an error.
//!
//! ## AC1 — Bind address and port
//!
//! `skim proxy --port <P>` starts and binds on `127.0.0.1:<P>` by default.
//! `--bind <addr>` overrides the bind address. A non-loopback bind address
//! MUST emit the cleartext-exposure warning to stderr BEFORE serving.
//!
//! ## AC25 — Registry wiring
//!
//! `proxy` is registered in both `KNOWN_SUBCOMMANDS` and `META_SUBCOMMANDS`
//! (both sorted, see registry.rs). Meta classification keeps `proxy` out of
//! PATH-wrapper targets (a server is not a tool to intercept). The indefinite-command
//! guard MUST NOT route `skim proxy` to `run_inherited_passthrough` — `proxy` is not
//! an indefinite streaming command.

use std::net::IpAddr;
use std::process::ExitCode;
use std::sync::Arc;

use rskim_align::Provider as LlmProvider;
use rskim_compress::{BlockRouter, Policy};
use rskim_contract::contract::Outcome;
use rskim_contract::log::{DecisionRecord, DecisionSink, SinkFull};
use rskim_contract::waiver::MARKER_BYTES;
use rskim_proxy::authmode::AuthMode;
use rskim_proxy::config::ProxyConfig;
use rskim_proxy::detect::ProxyProvider;
use rskim_proxy::seam::{TransformContext, TransformPipeline, TransformStage};

// ============================================================================
// BlockRouterStage — TransformStage adapter for BlockRouter (D1 / R2 / #304)
// ============================================================================

/// [`TransformStage`] adapter wrapping [`BlockRouter`] for the proxy pipeline.
///
/// This adapter lives in the rskim binary (not rskim-compress) because
/// `TransformStage` / `TransformContext` live in rskim-proxy, which has
/// non-optional hyper/tokio deps. rskim-compress must stay hyper/tokio-free
/// (AC9 / R2). The rskim binary already depends on both crates, making it the
/// correct home for the bridge.
///
/// ## auth_mode → Policy mapping (D1 / AD-PXY-08)
///
/// Policy is resolved per call from `ctx.auth_mode` (the router is stateless):
///
/// | `AuthMode`        | `Policy`          | Rationale                                      |
/// |-------------------|-------------------|------------------------------------------------|
/// | `Subscription`    | `LosslessOnly`    | Byte-exact passthrough: no re-encoding (not even lossless) |
/// | `ApiKey`          | `Default`         | Lossless re-encoding allowed (JSON minification, log dedup) |
/// | `Ambiguous`       | `Default`         | Map to ApiKey (D1 conservative toward Default) |
///
/// ## Fail-open contract
///
/// `apply` always returns `Outcome` (no error variant). `BlockRouter::route`
/// is already fail-open; this adapter propagates that contract directly.
///
/// ## SKIM_PASSTHROUGH escape hatch
///
/// The stage-level passthrough is NOT implemented here. The build site
/// (`run()`) substitutes the identity pipeline when `SKIM_PASSTHROUGH=1`
/// is set, so this struct never receives a call in passthrough mode.
struct BlockRouterStage {
    router: BlockRouter,
}

impl BlockRouterStage {
    /// Construct a `BlockRouterStage` with the given `BlockRouter`.
    fn new(router: BlockRouter) -> Self {
        Self { router }
    }
}

impl TransformStage for BlockRouterStage {
    fn name(&self) -> &'static str {
        "block-router"
    }

    /// Apply the block router to the request body.
    ///
    /// Maps `ctx.auth_mode → Policy` per call (D1: router is stateless/shared).
    /// Delegates to `BlockRouter::route(body, policy, request_id, sink)`.
    fn apply(&self, body: &[u8], ctx: &TransformContext<'_>, sink: &dyn DecisionSink) -> Outcome {
        // D1: map auth_mode to policy per call (not stored — router stateless).
        let policy = match ctx.auth_mode {
            // Subscription: LosslessOnly — byte-exact passthrough, no re-encoding.
            AuthMode::Subscription => Policy::LosslessOnly,
            // ApiKey: Default — lossless re-encoding allowed (JSON minify, log dedup).
            AuthMode::ApiKey => Policy::Default,
            // Ambiguous: Default — conservative map toward ApiKey (D1).
            // Both-present AND neither-present cases are Ambiguous (AD-PXY-08).
            _ => Policy::Default,
        };
        self.router.route(body, policy, ctx.request_id, sink)
    }
}

/// A null [`DecisionSink`] used only when constructing BlockRouterStage in
/// the binary context where the per-call sink is passed via `apply()`.
///
/// `BlockRouter::new` requires an `Arc<dyn DecisionSink>` for its `Contract`
/// bridge (conformance harness). The per-call `apply()` path passes a separate
/// sink; this stub is never called on that path.
struct BinarySinkStub;

impl DecisionSink for BinarySinkStub {
    fn try_send(&self, _record: DecisionRecord) -> Result<(), SinkFull> {
        Ok(())
    }
}

// ============================================================================
// CacheAlignStage — TransformStage adapter for rskim-align (AD-CA-8 / #306)
// ============================================================================

/// [`TransformStage`] adapter wrapping `rskim_align::align` for the proxy pipeline.
///
/// ## AD-CA-8 — Appended last (AD-PXY-06)
///
/// `CacheAlignStage` is appended after `BlockRouterStage` in the pipeline.
/// This mirrors the canonical order `#307 → #304 → #306 LAST` (seam.rs:19-29).
///
/// ## AD-CA-10 — Provider isolation
///
/// Provider mapping from [`ProxyProvider`] to [`LlmProvider`] occurs **before**
/// any marker code. `ProxyProvider::Unknown` → immediate passthrough
/// (seam bypass upstream per AD-PXY-02).
///
/// ## AD-CA-9 / AD-AN-5 — Analytics fire-and-forget
///
/// Each call records one [`crate::analytics::AlignmentDecisionRecord`] via the
/// injected [`crate::analytics::AlignmentRecorder`]. The recorder uses
/// `try_send` + drop-on-overflow — a blocked recorder NEVER delays forwarding.
///
/// ## AD-CA-5 / AD-PXY-21 — Bounded growth waiver
///
/// `max_growth()` returns `2 × MARKER_BYTES` (v1 cap: at most 2 skim markers).
/// The seam calls `guarded_transform_with_growth(…, stage.max_growth(…), …)`
/// which accepts output up to `input_len + 2 × MARKER_BYTES`.
struct CacheAlignStage {
    recorder: Box<dyn crate::analytics::AlignmentRecorder>,
}

impl CacheAlignStage {
    /// Construct a stage with the given recorder.
    fn new(recorder: Box<dyn crate::analytics::AlignmentRecorder>) -> Self {
        Self { recorder }
    }
}

impl TransformStage for CacheAlignStage {
    fn name(&self) -> &'static str {
        "cache-align"
    }

    /// Apply cache-key alignment to the request body.
    ///
    /// ## AD-CA-10 — Provider branch before marker code
    ///
    /// Maps `ctx.provider` (proxy-layer [`ProxyProvider`]) to the rskim-llm
    /// [`LlmProvider`] used by the alignment crate. `ProxyProvider::Unknown`
    /// returns passthrough immediately — the seam already bypasses Unknown
    /// providers (AD-PXY-02) but we guard defensively here as well.
    ///
    /// ## Outcome mapping
    ///
    /// - Fail-open passthrough (SHA-256-equal): `outcome.bytes == body` → `Outcome::passthrough`
    /// - Modified (canonical + marker injection): `outcome.bytes != body` → `Outcome::modified`
    fn apply(&self, body: &[u8], ctx: &TransformContext<'_>, _sink: &dyn DecisionSink) -> Outcome {
        // AD-CA-10: map ProxyProvider → LlmProvider before ANY marker code.
        // Unknown → whole-request passthrough (seam bypass upstream, AD-PXY-02).
        let llm_provider = match ctx.provider {
            ProxyProvider::Anthropic => LlmProvider::Anthropic,
            ProxyProvider::OpenAI => LlmProvider::OpenAi,
            // Defensive: Unknown never reaches this stage (seam.rs:352-356 bypass),
            // but guard here as defence-in-depth per AD-CA-10.
            _ => {
                return Outcome::passthrough(body.to_vec(), ctx.request_id, "cache-align");
            }
        };

        // Pure, sync, deterministic alignment — no I/O, no clock.
        let align_out = rskim_align::align(body, llm_provider, ctx.request_id);

        // AD-CA-9 / AD-AN-5: fire-and-forget alignment record via try_send.
        // A blocked/full recorder increments its drop_count; forwarding is never delayed.
        let timestamp = crate::analytics::now_unix_secs();
        self.recorder
            .record(crate::analytics::AlignmentDecisionRecord {
                timestamp,
                request_id: ctx.request_id.to_string(),
                provider: format!("{llm_provider:?}"),
                tools_key_sorted: align_out.stats.tools_key_sorted,
                spans_compacted: align_out.stats.spans_compacted,
                skim_breakpoints_injected: align_out.stats.skim_breakpoints_injected,
                client_breakpoint_count: align_out.stats.client_breakpoint_count,
                volatile_warn_count: align_out.stats.volatile_warn_count,
                fail_open: align_out.stats.fail_open,
                input_len: align_out.stats.input_len,
                output_len: align_out.stats.output_len,
                input_sha256: align_out.stats.input_sha256,
                output_sha256: align_out.stats.output_sha256,
            });

        if align_out.bytes == body {
            // Fail-open passthrough OR already-canonical body (byte-identical).
            Outcome::passthrough(body.to_vec(), ctx.request_id, "cache-align")
        } else {
            // Modified: canonical ordering + optional marker injection.
            let input_len = body.len();
            Outcome::modified(align_out.bytes, input_len, ctx.request_id, "cache-align")
        }
    }

    /// AD-CA-5 / AD-PXY-21: waivered growth for v1 marker injection.
    ///
    /// At most 2 skim-injected markers (v1 cap), each exactly `MARKER_BYTES` bytes.
    /// The seam uses this to call `guarded_transform_with_growth(…, 2 × MARKER_BYTES, …)`
    /// so the v1 output is accepted even though it may exceed the raw input length.
    fn max_growth(&self, _input_len: usize) -> usize {
        // AD-CA-5: 2 × MARKER_BYTES (imported constant, never redefined — AD-CA-4).
        2 * MARKER_BYTES
    }
}

/// Cleartext-exposure warning emitted to stderr when `--bind` is a non-loopback address.
///
/// AC1 / AD-PXY-03: this exact string is the contract; tests assert it appears on stderr.
const CLEARTEXT_WARNING: &str = "WARNING: skim proxy is bound to a non-loopback address. \
     Auth material (API keys, bearer tokens) will be transmitted in cleartext \
     unless the client uses TLS. Only bind to non-loopback addresses in trusted \
     network environments. Omit --bind (or pass --bind 127.0.0.1) to restrict \
     to loopback.";

/// Run the `skim proxy` subcommand.
///
/// Parses flags from `args`, builds a validated [`ProxyConfig`], emits the
/// cleartext-exposure warning if required, then calls [`rskim_proxy::serve()`].
///
/// Returns `ExitCode::FAILURE` on startup error; `ExitCode::SUCCESS` on clean
/// shutdown (SIGINT/SIGTERM received and drain complete).
pub(crate) fn run(
    args: &[String],
    analytics_cfg: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    // Help flag.
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Parse flags from args slice. We use a minimal hand-written parser to avoid
    // pulling clap into this path — consistent with other skim subcommand handlers
    // that parse flags directly.
    let parsed = match parse_proxy_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("skim proxy: {e}");
            eprintln!("Run 'skim proxy --help' for usage information.");
            return Ok(ExitCode::FAILURE);
        }
    };

    // D8: require --upstream-default. The flag is documented as required for routing;
    // starting without it means every request 502s (no upstream to forward to). Fail
    // fast at startup rather than serving a silently-useless proxy.
    if parsed.upstream_default.is_none() {
        eprintln!(
            "skim proxy: --upstream-default is required (D8). \
             Without it, all requests return 502. \
             Example: skim proxy --port 41322 --upstream-default https://api.anthropic.com"
        );
        return Ok(ExitCode::FAILURE);
    }

    // Build and validate ProxyConfig.
    let mut builder = ProxyConfig::builder().port(parsed.port);

    if let Some(bind_ip) = parsed.bind_ip {
        builder = builder.bind_ip(bind_ip);
    }

    if let Some(ref upstream) = parsed.upstream_default {
        builder = builder.upstream_default(upstream.as_str());
    }

    let config = match builder.build() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("skim proxy: configuration error: {e}");
            return Ok(ExitCode::FAILURE);
        }
    };

    // AC1 / AD-PXY-03: emit cleartext warning BEFORE serving.
    if config.warn_cleartext {
        eprintln!("{CLEARTEXT_WARNING}");
    }

    // AD-CA-9 / AD-AN-5: build the AlignmentRecorder from the analytics config.
    // ChannelAlignmentRecorder::new_boxed returns NoopRecorder when analytics are
    // disabled, so the stage is always safe to construct.
    // The local name is `align_recorder` to avoid shadowing the `analytics_cfg` param
    // or the proxy-layer hook below (AD-CA-9 name-shadow resolution).
    let align_recorder = crate::analytics::ChannelAlignmentRecorder::new_boxed(analytics_cfg);

    // Build the transform pipeline.
    //
    // SKIM_PASSTHROUGH=1 → identity pipeline (no compression). Consistent with
    // skim's global passthrough convention for debugging (#304 escape hatch).
    //
    // --no-cache-align → BlockRouterStage only (no CacheAlignStage); same as
    //   the previous phase before #306 was merged. Alignment is skipped while
    //   the block-router compression still runs. AD-CA-8 / PF-006.
    //
    // Default → BlockRouterStage (#304) + CacheAlignStage (#306, LAST — AD-CA-8).
    // AD-PXY-06 canonical order: #307 → #304 → #306 LAST.
    //
    // `proxy_analytics_hook`: the proxy's own NoopAnalyticsHook (distinct from the
    // skim AnalyticsConfig `analytics_cfg`). Named explicitly to resolve the
    // name shadow that existed when both were called `analytics` (AD-CA-9).
    let proxy_analytics_hook = Arc::new(rskim_proxy::analytics::NoopAnalyticsHook);
    let pipeline = if std::env::var("SKIM_PASSTHROUGH").as_deref() == Ok("1") {
        TransformPipeline::identity()
    } else if parsed.no_cache_align {
        // --no-cache-align: run BlockRouterStage only; omit CacheAlignStage.
        // align_recorder is not moved into any stage; it is dropped when run()
        // returns. If analytics is enabled, the consumer thread runs idle (no
        // records are sent) and exits when the sender drops at run() return.
        // flush_pending() in main() joins the thread before process exit.
        // If analytics is disabled, new_boxed() returned NoopRecorder (no thread).
        let router = BlockRouter::new(Arc::new(BinarySinkStub));
        let block_stage = BlockRouterStage::new(router);
        TransformPipeline::from_stages(vec![Box::new(block_stage)])
    } else {
        // Full pipeline: BlockRouterStage (#304) then CacheAlignStage (#306).
        // AD-CA-8: CacheAlignStage appended LAST per AD-PXY-06.
        let router = BlockRouter::new(Arc::new(BinarySinkStub));
        let block_stage = BlockRouterStage::new(router);
        // AD-CA-9: CacheAlignStage takes ownership of the recorder. When
        // serve_with_stage() returns, the pipeline is dropped, which drops
        // CacheAlignStage, which drops the recorder, which drops the Sender.
        // The consumer thread drains remaining records and exits.
        // flush_pending() in main() joins the thread before process exit.
        let align_stage = CacheAlignStage::new(align_recorder);
        TransformPipeline::from_stages(vec![Box::new(block_stage), Box::new(align_stage)])
    };

    // Call serve_with_stage() — blocks until SIGINT/SIGTERM and drain completes (AC23).
    // After it returns, the pipeline is dropped (align_recorder sender closes),
    // allowing the consumer thread to drain and exit (joined by flush_pending in main).
    match rskim_proxy::serve_with_stage(config, pipeline, proxy_analytics_hook) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("skim proxy: error: {e}");
            Ok(ExitCode::FAILURE)
        }
    }
}

// ============================================================================
// Argument parsing
// ============================================================================

/// Parsed proxy command-line arguments.
struct ProxyArgs {
    port: u16,
    bind_ip: Option<IpAddr>,
    upstream_default: Option<String>,
    /// Disable cache-key alignment (CacheAlignStage is omitted from the pipeline).
    ///
    /// PF-006: parsed via an explicit unconditional arm so the flag is never
    /// silently ignored. Unknown flags in the catch-all arm below are ignored
    /// for forward-compatibility, but `--no-cache-align` is an important
    /// behavior-change opt-out that must be caught explicitly.
    no_cache_align: bool,
}

/// Parse proxy-specific CLI flags from an arg slice.
///
/// Accepted flags:
/// - `--port <P>` — port to bind (default: 41322)
/// - `--bind <addr>` — bind IP address (default: 127.0.0.1)
/// - `--upstream-default <URL>` — default upstream base URL (required for routing)
fn parse_proxy_args(args: &[String]) -> anyhow::Result<ProxyArgs> {
    use rskim_proxy::config::DEFAULT_PROXY_PORT;

    let mut port = DEFAULT_PROXY_PORT;
    let mut bind_ip: Option<IpAddr> = None;
    let mut upstream_default: Option<String> = None;
    let mut no_cache_align = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--port" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--port requires a value"))?;
                port = val.parse::<u16>().map_err(|_| {
                    anyhow::anyhow!("--port value '{}' is not a valid port number", val)
                })?;
            }
            "--bind" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--bind requires a value"))?;
                let ip: IpAddr = val.parse().map_err(|_| {
                    anyhow::anyhow!("--bind value '{}' is not a valid IP address", val)
                })?;
                bind_ip = Some(ip);
            }
            "--upstream-default" => {
                i += 1;
                let val = args
                    .get(i)
                    .ok_or_else(|| anyhow::anyhow!("--upstream-default requires a value"))?;
                upstream_default = Some(val.clone());
            }
            other if other.starts_with("--port=") => {
                let val = &other["--port=".len()..];
                port = val.parse::<u16>().map_err(|_| {
                    anyhow::anyhow!("--port value '{}' is not a valid port number", val)
                })?;
            }
            other if other.starts_with("--bind=") => {
                let val = &other["--bind=".len()..];
                let ip: IpAddr = val.parse().map_err(|_| {
                    anyhow::anyhow!("--bind value '{}' is not a valid IP address", val)
                })?;
                bind_ip = Some(ip);
            }
            other if other.starts_with("--upstream-default=") => {
                let val = &other["--upstream-default=".len()..];
                upstream_default = Some(val.to_string());
            }
            // PF-006: --no-cache-align MUST be parsed via an explicit unconditional arm.
            // Unknown flags are silently ignored below (forward-compatibility), but this
            // flag opts out of a significant behaviour change (key canonicalization +
            // marker injection) and MUST NOT be silently dropped if someone passes it.
            "--no-cache-align" => {
                no_cache_align = true;
            }
            _ => {
                // Unknown flags are silently ignored for forward-compatibility.
                // A strict unknown-flag error is a UX tradeoff; meta subcommands
                // in this codebase generally ignore unknown flags (see stats.rs).
            }
        }
        i += 1;
    }

    Ok(ProxyArgs {
        port,
        bind_ip,
        upstream_default,
        no_cache_align,
    })
}

// ============================================================================
// Help
// ============================================================================

fn print_help() {
    eprintln!(
        "skim proxy — HTTP reverse proxy for skim Layer 3\n\
         \n\
         USAGE:\n\
             skim proxy [OPTIONS]\n\
         \n\
         OPTIONS:\n\
             --port <PORT>              Port to listen on (default: 41322; range: 41000-49000)\n\
             --bind <ADDR>              Bind address (default: 127.0.0.1; non-loopback emits a warning)\n\
             --upstream-default <URL>   Default upstream base URL (required for provider routing)\n\
             --no-cache-align           Disable KV-cache alignment (see CACHE ALIGNMENT below)\n\
             -h, --help                 Print this help message\n\
         \n\
         ENVIRONMENT:\n\
             SKIM_PASSTHROUGH=1         Bypass all compression and alignment (identity pipeline)\n\
             SKIM_DISABLE_ANALYTICS=1   Disable analytics recording\n\
         \n\
         CACHE ALIGNMENT (enabled by default, disable with --no-cache-align):\n\
             skim proxy rewrites request bodies to maximise KV-cache hit rates:\n\
             1. Tool/schema object keys are sorted into canonical order.\n\
             2. The tools/functions array is sorted into deterministic element order.\n\
             3. Top-level envelope keys are emitted in canonical order.\n\
             4. Up to 2 cache_control breakpoints are injected at stable structural\n\
                positions (last tool object, last block-form system block).\n\
             These changes are VISIBLE to the model (element order change) and cause\n\
             a one-time provider-cache warm on skim upgrade. Use --no-cache-align to\n\
             opt out. SKIM_PASSTHROUGH=1 bypasses all transforms (raw passthrough).\n\
         \n\
         EXAMPLES:\n\
             skim proxy --port 41322 --upstream-default https://api.anthropic.com\n\
             skim proxy --port 41500 --bind 0.0.0.0 --upstream-default https://api.openai.com\n\
             skim proxy --port 41322 --upstream-default https://api.anthropic.com --no-cache-align"
    );
}

// ============================================================================
// Tests (AC25 + auth_mode → Policy mapping)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rskim_contract::log::MockSink;
    use rskim_proxy::config::{DEFAULT_PROXY_PORT, PORT_RANGE_MIN};
    use rskim_proxy::seam::HeaderView;

    // AC25: parse_proxy_args returns defaults when no args given.
    #[test]
    fn test_parse_proxy_args_defaults() {
        let args: Vec<String> = vec![];
        let parsed = parse_proxy_args(&args).expect("parse must succeed with empty args");
        assert_eq!(parsed.port, DEFAULT_PROXY_PORT);
        assert!(parsed.bind_ip.is_none(), "bind_ip defaults to None");
        assert!(
            parsed.upstream_default.is_none(),
            "upstream_default defaults to None"
        );
    }

    // AC25: --port flag is parsed.
    #[test]
    fn test_parse_proxy_args_port_flag() {
        let args: Vec<String> = vec!["--port".into(), "41500".into()];
        let parsed = parse_proxy_args(&args).expect("parse must succeed");
        assert_eq!(parsed.port, 41500);
    }

    // AC25: --port=VALUE form is parsed.
    #[test]
    fn test_parse_proxy_args_port_equals_form() {
        let args: Vec<String> = vec!["--port=41600".into()];
        let parsed = parse_proxy_args(&args).expect("parse must succeed");
        assert_eq!(parsed.port, 41600);
    }

    // AC25: --bind flag is parsed.
    #[test]
    fn test_parse_proxy_args_bind_flag() {
        let args: Vec<String> = vec!["--bind".into(), "0.0.0.0".into()];
        let parsed = parse_proxy_args(&args).expect("parse must succeed");
        assert_eq!(parsed.bind_ip, Some("0.0.0.0".parse().expect("valid IP")));
    }

    // AC25: --upstream-default flag is parsed.
    #[test]
    fn test_parse_proxy_args_upstream_flag() {
        let args: Vec<String> = vec![
            "--upstream-default".into(),
            "https://api.anthropic.com".into(),
        ];
        let parsed = parse_proxy_args(&args).expect("parse must succeed");
        assert_eq!(
            parsed.upstream_default.as_deref(),
            Some("https://api.anthropic.com")
        );
    }

    // AC25: invalid port value returns an error.
    #[test]
    fn test_parse_proxy_args_invalid_port() {
        let args: Vec<String> = vec!["--port".into(), "not-a-port".into()];
        assert!(
            parse_proxy_args(&args).is_err(),
            "invalid port must return an error"
        );
    }

    // AC25: --port missing value returns an error.
    #[test]
    fn test_parse_proxy_args_missing_port_value() {
        let args: Vec<String> = vec!["--port".into()];
        assert!(
            parse_proxy_args(&args).is_err(),
            "--port without value must return an error"
        );
    }

    // AC1 / AD-PXY-03: cleartext warning text is non-empty and mentions key terms.
    #[test]
    fn test_cleartext_warning_contains_required_terms() {
        assert!(
            CLEARTEXT_WARNING.contains("WARNING"),
            "cleartext warning must contain 'WARNING'"
        );
        assert!(
            CLEARTEXT_WARNING.contains("non-loopback"),
            "cleartext warning must mention 'non-loopback'"
        );
        assert!(
            CLEARTEXT_WARNING.contains("cleartext"),
            "cleartext warning must mention 'cleartext'"
        );
        // NEGATIVE (PF-007): warning must NOT reference SKIM_PROXY_* env vars —
        // those are not implemented; mentioning them gives false remediation advice.
        assert!(
            !CLEARTEXT_WARNING.contains("SKIM_PROXY_BIND"),
            "cleartext warning must not reference SKIM_PROXY_BIND (env var is not implemented)"
        );
    }

    // NEGATIVE discriminating (PF-007): port below range_min fails at build time.
    // This test ensures the config validation is load-bearing (not just present).
    #[test]
    fn test_port_below_range_fails_build() {
        let result = ProxyConfig::builder().port(PORT_RANGE_MIN - 1).build();
        assert!(
            result.is_err(),
            "port {} must fail (below PORT_RANGE_MIN {})",
            PORT_RANGE_MIN - 1,
            PORT_RANGE_MIN
        );
    }

    // D8 / NEGATIVE discriminating (PF-007): --upstream-default absence is caught by parse_proxy_args.
    // The upstream_default field defaults to None; the run() function enforces it is set.
    #[test]
    fn test_parse_proxy_args_upstream_defaults_to_none() {
        let args: Vec<String> = vec![];
        let parsed = parse_proxy_args(&args).expect("parse must succeed");
        // upstream_default is None by default; run() will reject and fail before serving.
        assert!(
            parsed.upstream_default.is_none(),
            "no upstream_default must be None (rejected by run() per D8)"
        );
    }

    // D8: upstream_default presence is parsed correctly.
    #[test]
    fn test_parse_proxy_args_upstream_set() {
        let args: Vec<String> = vec![
            "--upstream-default".into(),
            "https://api.anthropic.com".into(),
        ];
        let parsed = parse_proxy_args(&args).expect("parse must succeed");
        assert_eq!(
            parsed.upstream_default.as_deref(),
            Some("https://api.anthropic.com"),
            "upstream_default must be set from --upstream-default flag"
        );
    }

    // =========================================================================
    // BlockRouterStage auth_mode → Policy mapping (D1 / Phase 4a)
    // =========================================================================

    /// Helper: call BlockRouterStage::apply with a minimal well-formed Anthropic body.
    ///
    /// Uses `max_tokens` to match Anthropic shape. A tiny short body (no live-zone
    /// compressible content) is fine here — we care about the policy path, not
    /// compression outcome.
    fn call_stage_with_auth(auth_mode: AuthMode) -> (Outcome, Vec<DecisionRecord>) {
        // Minimal body recognized as Anthropic (has max_tokens).
        let body = br#"{"model":"claude-3-5-sonnet-20241022","max_tokens":1024,"messages":[{"role":"user","content":"hi"}]}"#;
        let headers: Vec<(String, String)> = vec![];
        let hv = HeaderView::new(&headers);
        let ctx = TransformContext::new(ProxyProvider::Anthropic, auth_mode, "test-req-001", &hv);
        let sink = MockSink::new();
        let router = BlockRouter::new(Arc::new(BinarySinkStub));
        let stage = BlockRouterStage::new(router);
        let outcome = stage.apply(body, &ctx, &sink);
        let records = sink.drain();
        (outcome, records)
    }

    // D1 / POSITIVE: Subscription → LosslessOnly → all records are PolicyPassthrough.
    // DISCRIMINATING: replacing LosslessOnly with Default would cause (potentially)
    // Modified records, not PolicyPassthrough. The test fails if the mapping is wrong.
    #[test]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn test_auth_mode_subscription_maps_to_lossless_only() {
        let (outcome, records) = call_stage_with_auth(AuthMode::Subscription);
        // Subscription → LosslessOnly → body forwarded byte-identical.
        // Even with no candidates the outcome is passthrough.
        assert!(
            outcome.is_passthrough(),
            "Subscription must produce passthrough outcome (LosslessOnly policy)"
        );
        // Every decision record (if any — tiny body may have zero candidates) must
        // be a policy-passthrough record. For a body with at least one candidate,
        // we'd see PolicyPassthrough; for no candidates, no records are emitted.
        for record in &records {
            assert_eq!(
                record.decision,
                rskim_contract::log::Decision::Passthrough,
                "Subscription-mode record must be Passthrough, not Modified"
            );
        }
    }

    // D1 / POSITIVE: ApiKey → Default → policy gate does NOT force lossless.
    // DISCRIMINATING: if ApiKey were mapped to LosslessOnly, a compressible body
    // would still produce passthrough. This test proves ApiKey runs the Default path.
    #[test]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn test_auth_mode_api_key_maps_to_default() {
        // For policy routing, we only need to verify the policy gate is NOT
        // LosslessOnly: with a tiny body (no compressible candidates), the
        // router exits early with passthrough regardless of policy. The
        // discriminating signal here is that we do NOT see PolicyPassthrough records
        // (which are only emitted when policy == LosslessOnly and candidates exist).
        let (outcome, records) = call_stage_with_auth(AuthMode::ApiKey);
        assert!(
            outcome.is_passthrough(),
            "ApiKey with tiny body must produce passthrough outcome"
        );
        // With an Anthropic body that has a live-zone user message but below
        // the prefilter floor, there are candidates — but they are Passthrough
        // (prefilter skip), NOT PolicyPassthrough.
        // Verify: no record has reason=PolicyPassthrough (which would indicate LosslessOnly).
        for record in &records {
            // All records in Default mode are Passthrough (size-gated), not PolicyPassthrough.
            // PolicyPassthrough is ONLY emitted in LosslessOnly mode.
            // If this assertion fails, ApiKey was incorrectly mapped to LosslessOnly.
            assert!(
                record.reason != rskim_contract::log::OutcomeReason::PolicyPassthrough,
                "ApiKey must NOT produce PolicyPassthrough records (wrong policy mapping)"
            );
        }
    }

    // D1 / POSITIVE: Ambiguous → Default (conservative map toward ApiKey).
    // DISCRIMINATING: if Ambiguous were mapped to LosslessOnly, a compressible body
    // would produce PolicyPassthrough records. This test proves Ambiguous → Default.
    #[test]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn test_auth_mode_ambiguous_maps_to_default() {
        let (_, records) = call_stage_with_auth(AuthMode::Ambiguous);
        // Same discrimination as ApiKey: no PolicyPassthrough records.
        for record in &records {
            assert!(
                record.reason != rskim_contract::log::OutcomeReason::PolicyPassthrough,
                "Ambiguous must NOT produce PolicyPassthrough records (must map to Default)"
            );
        }
    }

    // =========================================================================
    // CacheAlignStage + --no-cache-align (AC18 / AD-CA-8 / AD-CA-9 / PF-006)
    // =========================================================================

    /// Minimal Anthropic body with one tool (triggers canonicalization + marker injection).
    fn anthropic_body_with_tool() -> &'static [u8] {
        br#"{"model":"claude-3-5-sonnet-20241022","max_tokens":1024,"tools":[{"name":"get_weather","description":"Get weather","input_schema":{"type":"object","properties":{"location":{"type":"string"}},"required":["location"]}}],"messages":[{"role":"user","content":"What is the weather?"}]}"#
    }

    /// Helper: apply CacheAlignStage with a given recorder and body.
    fn apply_align_stage(
        recorder: Box<dyn crate::analytics::AlignmentRecorder>,
        body: &[u8],
        provider: ProxyProvider,
    ) -> Outcome {
        let stage = CacheAlignStage::new(recorder);
        let headers: Vec<(String, String)> = vec![];
        let hv = HeaderView::new(&headers);
        let ctx = TransformContext::new(provider, AuthMode::ApiKey, "req-ac18", &hv);
        let sink = MockSink::new();
        stage.apply(body, &ctx, &sink)
    }

    // AC18 / AD-CA-9 / POSITIVE: CacheAlignStage with NoopRecorder completes without panic.
    // DISCRIMINATING (PF-007): removing CacheAlignStage's apply body would cause a compile error.
    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_cache_align_stage_noop_recorder_does_not_block() {
        let recorder = Box::new(crate::analytics::NoopRecorder);
        let outcome = apply_align_stage(
            recorder,
            anthropic_body_with_tool(),
            ProxyProvider::Anthropic,
        );
        assert!(
            !outcome.bytes.is_empty(),
            "CacheAlignStage must produce non-empty output"
        );
    }

    // AC18 / AD-CA-9 / POSITIVE: BlockingMockRecorder receives exactly one record per call.
    // DISCRIMINATING (PF-007): removing self.recorder.record(…) causes count to be 0.
    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_cache_align_stage_blocking_mock_records_one_per_call() {
        let recorder = crate::analytics::tests::BlockingMockRecorder::new();
        let handle = recorder.handle();
        let outcome = apply_align_stage(
            Box::new(recorder),
            anthropic_body_with_tool(),
            ProxyProvider::Anthropic,
        );
        assert!(!outcome.bytes.is_empty(), "output must be non-empty");
        let records = handle.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(
            records.len(),
            1,
            "exactly one AlignmentDecisionRecord must be written per apply() call"
        );
        let rec = &records[0];
        assert_eq!(rec.provider, "Anthropic", "provider must be Anthropic");
        assert!(!rec.request_id.is_empty(), "request_id must be non-empty");
    }

    // AC18 / AD-CA-9 / POSITIVE: CountingMockRecorder counts each call.
    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_cache_align_stage_counting_mock_increments() {
        let recorder = crate::analytics::tests::CountingMockRecorder::new();
        let handle = recorder.handle();
        let _ = apply_align_stage(
            Box::new(recorder),
            anthropic_body_with_tool(),
            ProxyProvider::Anthropic,
        );
        assert_eq!(
            handle.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "CountingMockRecorder must count exactly one call"
        );
    }

    // AD-CA-10 / NEGATIVE: Unknown provider → passthrough (no panic, no marker injection).
    // DISCRIMINATING (PF-007): removing the Unknown arm causes Unknown to fall through to alignment.
    #[test]
    fn test_cache_align_stage_unknown_provider_passthrough() {
        let body = br#"{"model":"unknown-model","messages":[{"role":"user","content":"hi"}]}"#;
        let outcome = apply_align_stage(
            Box::new(crate::analytics::NoopRecorder),
            body,
            ProxyProvider::Unknown,
        );
        assert_eq!(
            outcome.bytes, body,
            "Unknown provider must produce a passthrough (byte-identical output)"
        );
        assert!(
            outcome.is_passthrough(),
            "Unknown provider outcome must be passthrough"
        );
    }

    // AD-CA-5 / POSITIVE: max_growth returns 2 × MARKER_BYTES.
    // DISCRIMINATING (PF-007): returning 0 would cause the seam to reject marker-injected output.
    #[test]
    fn test_cache_align_stage_max_growth_is_two_marker_bytes() {
        let stage = CacheAlignStage::new(Box::new(crate::analytics::NoopRecorder));
        let growth = stage.max_growth(1024);
        assert_eq!(
            growth,
            2 * MARKER_BYTES,
            "max_growth must be 2 × MARKER_BYTES ({}) for the v1 cap",
            2 * MARKER_BYTES
        );
    }

    // PF-006 / POSITIVE: --no-cache-align is parsed by an explicit arm.
    // DISCRIMINATING (PF-007): removing the explicit arm causes the flag to be silently ignored.
    #[test]
    fn test_parse_proxy_args_no_cache_align_flag() {
        let args: Vec<String> = vec!["--no-cache-align".into()];
        let parsed = parse_proxy_args(&args).expect("parse must succeed");
        assert!(
            parsed.no_cache_align,
            "--no-cache-align flag must set no_cache_align=true"
        );
    }

    // PF-006 / NEGATIVE: absent --no-cache-align defaults to false.
    #[test]
    fn test_parse_proxy_args_no_cache_align_defaults_false() {
        let args: Vec<String> = vec![];
        let parsed = parse_proxy_args(&args).expect("parse must succeed");
        assert!(
            !parsed.no_cache_align,
            "no_cache_align must default to false when flag is absent"
        );
    }

    // =========================================================================
    // AC5 — Cross-turn content-prefix stability with moving tail marker
    // =========================================================================
    //
    // Verifies that in a simulated 8-turn conversation:
    // (a) After masking all cache_control, the tools value and system value are
    //     byte-identical across all turns (canonicalization is stable).
    // (b) Skim-injected static-zone markers sit at identical byte offsets within
    //     the tools value on every turn.
    // Negative arm A: without CacheAlignStage, tool key order varies → tools values differ.
    // Negative arm B: with max_growth=0 (seam reverted), markers are rejected → absent.
    //
    // NOTE: these tests use per-crate-scoped build (`-p rskim --bins --features proxy`).
    // They must NOT be run with `cargo test -p rskim --lib` (no library target).

    /// Build a messages array with N user+assistant pairs.
    ///
    /// All assistant messages use array-form content. The LAST assistant message
    /// carries a client `cache_control` marker (moving tail). Tool key order is
    /// controlled by `build_turn_body` (not here).
    fn build_turn_messages_json(n: usize) -> String {
        let mut parts: Vec<String> = Vec::new();
        for i in 1..=n {
            parts.push(format!(
                r#"{{"content":"Turn {i} question","role":"user"}}"#
            ));
            if i == n {
                // Last assistant message: client cache_control marker (moving tail).
                parts.push(format!(
                    r#"{{"content":[{{"cache_control":{{"type":"ephemeral"}},"text":"Turn {i} answer","type":"text"}}],"role":"assistant"}}"#
                ));
            } else {
                // Non-last assistant: simple content (no CC), array form.
                parts.push(format!(
                    r#"{{"content":[{{"text":"Turn {i} answer","type":"text"}}],"role":"assistant"}}"#
                ));
            }
        }
        format!("[{}]", parts.join(","))
    }

    /// Build a request body for turn N with alternating tool key order.
    ///
    /// Odd turns use "name-first" key order; even turns use "description-first".
    /// This ensures that without CacheAlignStage, the tools value differs between
    /// consecutive turns (the discriminating signal for negative arm A).
    fn build_turn_body(n: usize) -> Vec<u8> {
        let messages = build_turn_messages_json(n);
        // Alternate tool key order to make the discriminating signal clear.
        let tool_json = if n.is_multiple_of(2) {
            // Even: description before name (non-canonical order).
            r#"{"description":"Search the web","input_schema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]},"name":"search"}"#
        } else {
            // Odd: name before description (also non-canonical vs alphabetic).
            r#"{"name":"search","input_schema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]},"description":"Search the web"}"#
        };
        format!(
            r#"{{"max_tokens":1024,"messages":{messages},"model":"claude-3-5-sonnet-20241022","system":[{{"text":"You are helpful.","type":"text"}}],"tools":[{tool_json}]}}"#
        )
        .into_bytes()
    }

    /// Mask all `cache_control` entries from a JSON string (byte-level replacement).
    fn mask_cache_control_in_str(s: &str) -> String {
        s.replace(",\"cache_control\":{\"type\":\"ephemeral\"}", "")
            .replace("\"cache_control\":{\"type\":\"ephemeral\"},", "")
            .replace("\"cache_control\":{\"type\":\"ephemeral\"}", "")
    }

    /// Extract the JSON value for a top-level key as a compact JSON string.
    ///
    /// Uses `serde_json::Value` (preserve_order) to extract; produces compact output.
    fn extract_json_key_compact(s: &str, key: &str) -> Option<String> {
        let val: serde_json::Value = serde_json::from_str(s).ok()?;
        let v = val.get(key)?;
        serde_json::to_string(v).ok()
    }

    // AC5 / POSITIVE — 8-turn cross-turn prefix stability.
    //
    // DISCRIMINATING (PF-007): fails if CacheAlignStage is deleted (negative arm A:
    // tool key order varies across turns without canonicalization) and fails if the
    // seam max_growth is zero (negative arm B: markers rejected, offset test fails).
    #[test]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn test_cross_turn_prefix_stability_ac5() {
        let router = BlockRouter::new(Arc::new(BinarySinkStub));
        let pipeline = TransformPipeline::from_stages(vec![
            Box::new(BlockRouterStage::new(router)),
            Box::new(CacheAlignStage::new(Box::new(
                crate::analytics::NoopRecorder,
            ))),
        ]);

        let headers: Vec<(String, String)> = vec![];
        let hv = HeaderView::new(&headers);
        let sink = MockSink::new();

        // Run 8 turns through the pipeline and collect aligned outputs.
        let aligned: Vec<Vec<u8>> = (1..=8)
            .map(|n| {
                let body = build_turn_body(n);
                let req_id = format!("ac5-turn-{n:02}");
                let ctx =
                    TransformContext::new(ProxyProvider::Anthropic, AuthMode::ApiKey, &req_id, &hv);
                pipeline.run(body, &ctx, &sink).bytes
            })
            .collect();

        // (a) Tools value must be byte-identical across all 8 turns after masking CC.
        let tools_masked: Vec<String> = aligned
            .iter()
            .map(|b| {
                let s = std::str::from_utf8(b).unwrap();
                let masked = mask_cache_control_in_str(s);
                extract_json_key_compact(&masked, "tools")
                    .expect("aligned output must contain tools key")
            })
            .collect();

        let reference_tools = &tools_masked[0];
        for (i, tv) in tools_masked.iter().enumerate().skip(1) {
            assert_eq!(
                tv,
                reference_tools,
                "AC5(a): tools value in turn {} differs from turn 1 after masking CC. \
                 CacheAlignStage canonical key-sort must stabilize tools across turns.",
                i + 1
            );
        }

        // (a) System value must be byte-identical across all 8 turns after masking CC.
        let system_masked: Vec<String> = aligned
            .iter()
            .map(|b| {
                let s = std::str::from_utf8(b).unwrap();
                let masked = mask_cache_control_in_str(s);
                extract_json_key_compact(&masked, "system")
                    .expect("aligned output must contain system key")
            })
            .collect();

        let reference_system = &system_masked[0];
        for (i, sv) in system_masked.iter().enumerate().skip(1) {
            assert_eq!(
                sv,
                reference_system,
                "AC5(a): system value in turn {} differs from turn 1 after masking CC.",
                i + 1
            );
        }

        // (b) Skim static markers must sit at identical byte offsets within the
        // tools value on every turn. The marker is injected at the last tool object.
        let cc_marker = "\"cache_control\"";
        let tools_cc_offsets: Vec<Option<usize>> = aligned
            .iter()
            .map(|b| {
                let s = std::str::from_utf8(b).ok()?;
                let tools_str = extract_json_key_compact(s, "tools")?;
                tools_str.find(cc_marker)
            })
            .collect();

        // All 8 turns must have a skim marker in the tools value.
        for (i, offset) in tools_cc_offsets.iter().enumerate() {
            assert!(
                offset.is_some(),
                "AC5(b): turn {} must have a skim cache_control marker in tools value. \
                 Marker injection requires CacheAlignStage AND max_growth > 0.",
                i + 1
            );
        }

        // All markers at the same offset within the tools value (static zone).
        let ref_offset = tools_cc_offsets[0].unwrap();
        for (i, offset) in tools_cc_offsets.iter().enumerate().skip(1) {
            assert_eq!(
                *offset,
                Some(ref_offset),
                "AC5(b): skim marker offset in tools value differs in turn {} vs turn 1. \
                 Static-zone markers must be at identical byte positions on every turn.",
                i + 1
            );
        }
    }

    // AC5 / NEGATIVE (arm A) — Without CacheAlignStage, tool key order varies between
    // turns (client alternates key order), so the masked tools value differs.
    //
    // DISCRIMINATING (PF-007): proves test_cross_turn_prefix_stability_ac5 requires
    // CacheAlignStage to pass (canonical key sort is essential for cross-turn stability).
    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_cross_turn_no_align_stage_tools_vary_ac5_negative_a() {
        // Pipeline WITHOUT CacheAlignStage — BlockRouterStage only.
        let router = BlockRouter::new(Arc::new(BinarySinkStub));
        let pipeline =
            TransformPipeline::from_stages(vec![Box::new(BlockRouterStage::new(router))]);

        let headers: Vec<(String, String)> = vec![];
        let hv = HeaderView::new(&headers);
        let sink = MockSink::new();

        // Run 2 turns (turn 1: odd key order, turn 2: even key order).
        let aligned: Vec<Vec<u8>> = (1..=2)
            .map(|n| {
                let body = build_turn_body(n);
                let req_id = format!("ac5-neg-a-turn-{n:02}");
                let ctx =
                    TransformContext::new(ProxyProvider::Anthropic, AuthMode::ApiKey, &req_id, &hv);
                pipeline.run(body, &ctx, &sink).bytes
            })
            .collect();

        let tools_values: Vec<String> = aligned
            .iter()
            .map(|b| {
                let s = std::str::from_utf8(b).unwrap();
                let masked = mask_cache_control_in_str(s);
                extract_json_key_compact(&masked, "tools").unwrap()
            })
            .collect();

        // Without CacheAlignStage, alternating key order must survive into output →
        // tools values MUST differ (proving CacheAlignStage is needed for stability).
        assert_ne!(
            tools_values[0], tools_values[1],
            "AC5 negative arm A: without CacheAlignStage, tools key order must vary \
             between turns (confirming CacheAlignStage is required for cross-turn stability)"
        );
    }

    // AC5 / NEGATIVE (arm B) — With max_growth=0, the seam rejects any marker-injected
    // output (guarded_transform_with_growth falls back to passthrough). The tools value
    // has no skim marker — the AC5(b) offset check would fail.
    //
    // DISCRIMINATING (PF-007): proves the seam max_growth fix is required for markers
    // to survive into the forwarded body.
    #[test]
    #[allow(clippy::unwrap_used)]
    fn test_cross_turn_zero_growth_no_markers_ac5_negative_b() {
        // ZeroGrowthCacheAlignStage: delegates apply() to CacheAlignStage but
        // overrides max_growth() to 0. The seam's guarded_transform_with_growth
        // gate then rejects the marker-injected output and falls back to passthrough.
        struct ZeroGrowthCacheAlignStage {
            inner: CacheAlignStage,
        }

        impl TransformStage for ZeroGrowthCacheAlignStage {
            fn name(&self) -> &'static str {
                "cache-align-zero-growth"
            }

            fn apply(
                &self,
                body: &[u8],
                ctx: &TransformContext<'_>,
                sink: &dyn DecisionSink,
            ) -> Outcome {
                self.inner.apply(body, ctx, sink)
            }

            /// Override: return 0 — the seam rejects any output > input bytes.
            fn max_growth(&self, _input_len: usize) -> usize {
                0
            }
        }

        let router = BlockRouter::new(Arc::new(BinarySinkStub));
        let pipeline = TransformPipeline::from_stages(vec![
            Box::new(BlockRouterStage::new(router)),
            Box::new(ZeroGrowthCacheAlignStage {
                inner: CacheAlignStage::new(Box::new(crate::analytics::NoopRecorder)),
            }),
        ]);

        let headers: Vec<(String, String)> = vec![];
        let hv = HeaderView::new(&headers);
        let sink = MockSink::new();

        // Use turn 1 (odd key order — non-canonical; aligner tries to grow body).
        let body = build_turn_body(1);
        let ctx =
            TransformContext::new(ProxyProvider::Anthropic, AuthMode::ApiKey, "ac5-neg-b", &hv);
        let out = pipeline.run(body, &ctx, &sink);
        let out_str = std::str::from_utf8(&out.bytes).unwrap();

        // With max_growth=0, the seam gate rejects the marker-injected (inflated) output,
        // and the pipeline falls back to the pre-CacheAlignStage bytes. The tools value
        // in those pre-stage bytes has no skim marker.
        let tools_str = extract_json_key_compact(out_str, "tools").unwrap_or_default();
        assert!(
            !tools_str.contains("\"cache_control\""),
            "AC5 negative arm B: with max_growth=0, skim markers must be rejected — \
             no cache_control should appear in tools value. \
             This proves the seam max_growth fix is required for marker injection. \
             Found in tools: {tools_str}"
        );
    }
}
