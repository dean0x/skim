//! `skim proxy` subcommand — HTTP reverse proxy for skim Layer 3.
//!
//! Thin handler: parses `proxy`-specific args (clap), builds [`rskim_proxy::config::ProxyConfig`],
//! emits the cleartext-exposure warning when required, and calls
//! [`rskim_proxy::serve()`] (which blocks on its own tokio runtime).
//!
//! Keeps hyper/tokio out of the binary's other code paths via the crate boundary
//! (AD-PXY-01: separate crate isolates async runtime compile cost).
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

use rskim_proxy::config::ProxyConfig;

/// Cleartext-exposure warning emitted to stderr when `--bind` is a non-loopback address.
///
/// AC1 / AD-PXY-03: this exact string is the contract; tests assert it appears on stderr.
const CLEARTEXT_WARNING: &str = "WARNING: skim proxy is bound to a non-loopback address. \
     Auth material (API keys, bearer tokens) will be transmitted in cleartext \
     unless the client uses TLS. Only bind to non-loopback addresses in trusted \
     network environments. Set SKIM_PROXY_BIND=127.0.0.1 or omit --bind to \
     restrict to loopback.";

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

    // Call serve() — blocks until SIGINT/SIGTERM and drain completes (AC23).
    match rskim_proxy::serve(config) {
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
             skim proxy --port 41500 --bind 0.0.0.0 --upstream-default https://api.openai.com\n\
         \n\
         NOTE: This is a Phase 1 skeleton — the proxy server body is not yet implemented."
    );
}

// ============================================================================
// Tests (AC25)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use rskim_proxy::config::{DEFAULT_PROXY_PORT, PORT_RANGE_MIN};

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
}
