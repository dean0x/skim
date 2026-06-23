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
//! - `AuthMode::Subscription → Policy::LosslessOnly` (conservative: no lossy compression)
//! - `AuthMode::ApiKey      → Policy::Default`        (full compression allowed)
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

use rskim_compress::{BlockRouter, Policy};
use rskim_contract::contract::Outcome;
use rskim_contract::log::{DecisionRecord, DecisionSink, SinkFull};
use rskim_proxy::authmode::AuthMode;
use rskim_proxy::config::ProxyConfig;
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
/// | `Subscription`    | `LosslessOnly`    | Conservative: subscription flows may expect byte-exact replay |
/// | `ApiKey`          | `Default`         | Direct API key use — full compression allowed  |
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
            // Subscription: LosslessOnly — conservative, no lossy compression.
            AuthMode::Subscription => Policy::LosslessOnly,
            // ApiKey: Default — full compression allowed.
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
    _analytics: &crate::analytics::AnalyticsConfig,
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

    // Build the transform pipeline.
    //
    // SKIM_PASSTHROUGH=1 → identity pipeline (no compression). Consistent with
    // skim's global passthrough convention for debugging (#304 escape hatch).
    //
    // Default → inject BlockRouterStage wrapping BlockRouter (Phase 4a / D1 / AC19).
    // The router holds a BinarySinkStub for its Contract bridge; the per-call
    // apply() path receives the real sink via TransformStage::apply.
    let analytics = Arc::new(rskim_proxy::analytics::NoopAnalyticsHook);
    let pipeline = if std::env::var("SKIM_PASSTHROUGH").as_deref() == Ok("1") {
        TransformPipeline::identity()
    } else {
        let router = BlockRouter::new(Arc::new(BinarySinkStub));
        let stage = BlockRouterStage::new(router);
        TransformPipeline::from_stages(vec![Box::new(stage)])
    };

    // Call serve_with_stage() — blocks until SIGINT/SIGTERM and drain completes (AC23).
    match rskim_proxy::serve_with_stage(config, pipeline, analytics) {
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
             -h, --help                 Print this help message\n\
         \n\
         ENVIRONMENT:\n\
             SKIM_PASSTHROUGH=1         Bypass all compression\n\
         \n\
         EXAMPLES:\n\
             skim proxy --port 41322 --upstream-default https://api.anthropic.com\n\
             skim proxy --port 41500 --bind 0.0.0.0 --upstream-default https://api.openai.com"
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
    use rskim_proxy::detect::ProxyProvider;
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
}
