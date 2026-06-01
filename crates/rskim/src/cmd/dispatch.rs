//! Dispatch infrastructure for skim CLI.
//!
//! Provides the top-level `dispatch()` router plus the private helpers used
//! by multi-category dispatchers: argument extraction, subcommand scaffolding,
//! raw passthrough, and per-family help printers.

use std::process::ExitCode;

use super::{
    DEFAULT_CMD_TIMEOUT, KNOWN_SUBCOMMANDS, agents, build, completions, db, discover, file, git,
    heatmap, infra, init, learn, lint, log, pkg, rewrite, sanitize_for_display, search, stats,
    test,
};

// ============================================================================
// Private argument helpers
// ============================================================================

/// Prepend a tool name to an arg slice.
fn prepend(tool: &str, args: &[String]) -> Vec<String> {
    let mut v = Vec::with_capacity(args.len() + 1);
    v.push(tool.to_string());
    v.extend_from_slice(args);
    v
}

/// Build a `Vec<String>` with `tool` prepended and the element at `skip_idx`
/// removed, pre-allocating the exact capacity needed.
fn prepend_without(tool: &str, args: &[String], skip_idx: usize) -> Vec<String> {
    debug_assert!(
        skip_idx < args.len(),
        "skip_idx {skip_idx} out of bounds for args len {}",
        args.len()
    );
    let mut v = Vec::with_capacity(args.len()); // remove one, prepend one → same len
    v.push(tool.to_string());
    v.extend(
        args.iter()
            .enumerate()
            .filter(|(i, _)| *i != skip_idx)
            .map(|(_, s)| s.clone()),
    );
    v
}

/// Shared scaffolding for multi-category dispatchers (`cargo`, `go`, …).
///
/// Handles flag interleaving: `skim cargo --show-stats test` works because
/// we skip leading flags to find the first positional (the subcommand token),
/// then the caller decides which args to forward.
///
/// Returns `Ok(Some((subcmd_str, subcmd_idx)))` when a subcommand is found, or
/// `Ok(None)` after printing the missing-subcommand error (caller should return
/// `ExitCode::FAILURE`).  The `tool` parameter is used only in the error message.
fn extract_subcmd<'a>(
    tool: &str,
    args: &'a [String],
    usage: &str,
    supported: &str,
) -> anyhow::Result<Option<(&'a str, usize)>> {
    match args.iter().position(|a| !a.starts_with('-')) {
        Some(idx) => Ok(Some((args[idx].as_str(), idx))),
        None => {
            eprintln!(
                "skim {tool}: missing subcommand\n\n\
                 Usage: {usage}\n\n\
                 Supported subcommands: {supported}"
            );
            Ok(None)
        }
    }
}

// ============================================================================
// Raw passthrough
// ============================================================================

/// Run a program with the given args and env vars, printing stdout/stderr and
/// returning the process exit code. Used by passthrough dispatchers for unknown
/// subcommands that skim does not compress.
pub(crate) fn run_raw_passthrough(
    program: &str,
    args: &[String],
    env: &[(&str, &str)],
) -> anyhow::Result<ExitCode> {
    let runner = crate::runner::CommandRunner::new(Some(DEFAULT_CMD_TIMEOUT));
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = runner.run_with_env(program, &arg_refs, env)?;
    print!("{}", output.stdout);
    if !output.stderr.is_empty() {
        eprint!("{}", output.stderr);
    }
    let code = output.exit_code.unwrap_or(1).clamp(0, 255) as u8;
    Ok(ExitCode::from(code))
}

/// Pass through an unknown subcommand to the underlying tool unchanged.
///
/// Logs a warning to stderr naming the unknown subcommand, then reconstructs
/// the full argument list (`unknown` + remaining `args` with the subcmd at
/// `subcmd_idx` stripped) and delegates to [`run_raw_passthrough`].
///
/// Used by multi-category dispatchers (`swift`, `dotnet`) where unknown
/// subcommands are forwarded rather than rejected.
fn passthrough_subcmd(
    tool: &str,
    unknown: &str,
    args: &[String],
    subcmd_idx: usize,
    supported: &str,
    env: &[(&str, &str)],
) -> anyhow::Result<ExitCode> {
    let safe = sanitize_for_display(unknown);
    eprintln!(
        "skim {tool}: unknown subcommand '{safe}' — passing through\n\
         Supported subcommands: {supported}"
    );
    run_raw_passthrough(tool, &prepend_without(unknown, args, subcmd_idx), env)
}

// ============================================================================
// Help printers
// ============================================================================

fn print_cargo_help() {
    print!(
        "skim cargo\n\
         \n\
           Cargo subcommand compression\n\
         \n\
         Usage: skim cargo <SUBCOMMAND> [args...]\n\
         \n\
         Subcommands:\n\
           test (t)   Run and compress cargo test output\n\
           nextest    Run and compress cargo nextest output\n\
           build (b)  Run and compress cargo build output\n\
           check (c)  Run and compress cargo check output\n\
           fmt        Run and compress cargo fmt output\n\
           clippy     Run and compress cargo clippy output\n\
           audit      Run and compress cargo audit output\n\
         \n\
         Examples:\n\
           skim cargo test\n\
           skim cargo t          (alias for test)\n\
           skim cargo build --release\n\
           skim cargo b --release  (alias for build)\n\
           skim cargo check\n\
           skim cargo fmt\n\
           skim cargo clippy -- -D warnings\n\
           skim cargo audit\n"
    );
}

fn print_swift_help() {
    print!(
        "skim swift\n\
         \n\
           Swift subcommand compression\n\
         \n\
         Usage: skim swift <SUBCOMMAND> [args...]\n\
         \n\
         Subcommands:\n\
           test       Run and compress swift test output\n\
         \n\
         Other subcommands (build, run, etc.) are passed through unmodified.\n\
         \n\
         Examples:\n\
           skim swift test\n\
           skim swift test --filter MyTests\n"
    );
}

fn print_dotnet_help() {
    print!(
        "skim dotnet\n\
         \n\
           .NET subcommand compression\n\
         \n\
         Usage: skim dotnet <SUBCOMMAND> [args...]\n\
         \n\
         Subcommands:\n\
           test       Run and compress dotnet test output\n\
         \n\
         Other subcommands (build, run, publish, restore, etc.) are passed through unmodified.\n\
         \n\
         Examples:\n\
           skim dotnet test\n\
           skim dotnet test --filter Category=Unit\n"
    );
}

fn print_go_help() {
    print!(
        "skim go\n\
         \n\
           Go subcommand compression\n\
         \n\
         Usage: skim go <SUBCOMMAND> [args...]\n\
         \n\
         Subcommands:\n\
           test       Run and compress go test output\n\
         \n\
         Examples:\n\
           skim go test ./...\n\
           skim go test -v ./pkg/...\n"
    );
}

// ============================================================================
// Multi-category dispatchers
// ============================================================================

/// Route `skim cargo <subcmd> [args...]` to the correct category handler.
fn dispatch_cargo(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_cargo_help();
        return Ok(ExitCode::SUCCESS);
    }

    let Some((subcmd, idx)) = extract_subcmd(
        "cargo",
        args,
        "skim cargo <test|build|check|fmt|clippy|audit|nextest> [args...]",
        "test, nextest, build, check, fmt, clippy, audit",
    )?
    else {
        return Ok(ExitCode::FAILURE);
    };

    // Each subcommand is dispatched to build::run with its own name prepended as
    // the leading token.  build::run matches on that token to select the correct
    // cargo handler (cargo::run, cargo::run_check, cargo::run_fmt, etc.).
    // All subcommands use their own name consistently — there is no legacy "cargo"
    // alias for "build" any more.
    match subcmd {
        "test" | "t" => test::run(&prepend_without("cargo", args, idx), analytics),
        // nextest: keep the "nextest" token — the test handler uses it to select
        // the nextest parse path instead of the plain cargo-test path.
        "nextest" => test::run(&prepend("cargo", args), analytics),
        "build" | "b" => build::run(&prepend_without("build", args, idx), analytics),
        "check" | "c" => build::run(&prepend_without("check", args, idx), analytics),
        "fmt" => build::run(&prepend_without("fmt", args, idx), analytics),
        "clippy" => build::run(&prepend_without("clippy", args, idx), analytics),
        // audit: keep "audit" in args — pkg::run uses it to select the audit parser.
        "audit" => pkg::run(&prepend("cargo", args), analytics),
        unknown => {
            let safe = sanitize_for_display(unknown);
            eprintln!(
                "skim cargo: unknown subcommand '{safe}'\n\n\
                 Usage: skim cargo <test|build|check|fmt|clippy|audit|nextest> [args...]\n\n\
                 Supported subcommands: test, nextest, build, check, fmt, clippy, audit"
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Route `skim go <subcmd> [args...]` to the correct category handler.
fn dispatch_go(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_go_help();
        return Ok(ExitCode::SUCCESS);
    }

    let Some((subcmd, idx)) = extract_subcmd("go", args, "skim go <test> [args...]", "test")?
    else {
        return Ok(ExitCode::FAILURE);
    };

    match subcmd {
        "test" => test::run(&prepend_without("go", args, idx), analytics),
        unknown => {
            let safe = sanitize_for_display(unknown);
            eprintln!(
                "skim go: unknown subcommand '{safe}'\n\n\
                 Usage: skim go <test> [args...]\n\n\
                 Supported subcommands: test"
            );
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Route `skim swift <subcmd> [args...]` to the correct category handler.
///
/// Only `swift test` is compressed. Other `swift` subcommands (build, run, etc.)
/// pass through as raw to avoid interrupting normal swift workflows.
fn dispatch_swift(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_swift_help();
        return Ok(ExitCode::SUCCESS);
    }

    let Some((subcmd, idx)) = extract_subcmd("swift", args, "skim swift <test> [args...]", "test")?
    else {
        return Ok(ExitCode::FAILURE);
    };

    match subcmd {
        "test" => test::run(&prepend_without("swift", args, idx), analytics),
        unknown => passthrough_subcmd("swift", unknown, args, idx, "test", &[]),
    }
}

/// Route `skim dotnet <subcmd> [args...]` to the correct category handler.
///
/// Only `dotnet test` is compressed. Other `dotnet` subcommands (build, run, publish, etc.)
/// pass through as raw to avoid interrupting normal dotnet workflows.
fn dispatch_dotnet(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() || args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_dotnet_help();
        return Ok(ExitCode::SUCCESS);
    }

    let Some((subcmd, idx)) =
        extract_subcmd("dotnet", args, "skim dotnet <test> [args...]", "test")?
    else {
        return Ok(ExitCode::FAILURE);
    };

    match subcmd {
        "test" => test::run(&prepend_without("dotnet", args, idx), analytics),
        // DOTNET_CLI_UI_LANGUAGE forces English output for reliable parsing
        // even in passthrough mode, matching the compressed-path behavior.
        unknown => passthrough_subcmd(
            "dotnet",
            unknown,
            args,
            idx,
            "test",
            &[("DOTNET_CLI_UI_LANGUAGE", "en-US")],
        ),
    }
}

// ============================================================================
// Top-level dispatcher
// ============================================================================

/// Dispatch a subcommand by name. Returns the process exit code.
///
/// v2.8.0: Flat dispatch — tool names are top-level subcommands.
/// `cargo` and `go` use multi-category dispatchers; other tools route
/// directly to their category handler with the tool name prepended.
pub(crate) fn dispatch(
    subcommand: &str,
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    match subcommand {
        // Unchanged meta/utility
        "agents" => agents::run(args, analytics),
        "completions" => completions::run(args, analytics),
        "discover" => discover::run(args, analytics),
        "git" => git::run(args, analytics),
        "heatmap" => heatmap::run(args, analytics),
        "init" => init::run(args, analytics),
        "learn" => learn::run(args, analytics),
        "log" => log::run(args, analytics),
        "rewrite" => rewrite::run(args, analytics),
        "search" => search::run(args, analytics),
        "stats" => stats::run(args, analytics),

        // Multi-category dispatchers
        "cargo" => dispatch_cargo(args, analytics),
        "go" => dispatch_go(args, analytics),

        // Multi-category dispatchers for tools with subcommands
        "swift" => dispatch_swift(args, analytics),
        "dotnet" => dispatch_dotnet(args, analytics),

        // Direct-to-category routing (prepend tool name for category dispatcher)
        "cypress" | "jest" | "playwright" | "pytest" | "vitest" => {
            test::run(&prepend(subcommand, args), analytics)
        }
        "gradle" | "gradlew" | "make" | "mvn" | "mvnw" | "tsc" => {
            build::run(&prepend(subcommand, args), analytics)
        }
        "biome" | "black" | "dprint" | "eslint" | "gofmt" | "golangci" | "mypy" | "oxlint"
        | "prettier" | "rubocop" | "ruff" | "rustfmt" | "swiftlint" => {
            lint::run(&prepend(subcommand, args), analytics)
        }
        "npm" | "pip" | "pnpm" | "yarn" => pkg::run(&prepend(subcommand, args), analytics),
        "aws" | "curl" | "dig" | "docker" | "gh" | "kubectl" | "nslookup" | "terraform"
        | "wget" => infra::run(&prepend(subcommand, args), analytics),
        "mysql" | "psql" | "sqlite3" => db::run(&prepend(subcommand, args), analytics),
        "df" | "diff" | "du" | "env" | "find" | "grep" | "ls" | "printenv" | "ps" | "rg"
        | "tree" | "wc" => file::run(&prepend(subcommand, args), analytics),

        _ => {
            let safe = sanitize_for_display(subcommand);
            anyhow::bail!(
                "Unknown subcommand: '{safe}'\n\
                 Available subcommands: {}\n\
                 Run 'skim --help' for usage information",
                KNOWN_SUBCOMMANDS.join(", ")
            );
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // extract_subcmd tests
    // ========================================================================

    /// Happy path: first non-flag arg is the subcommand.
    #[test]
    fn test_extract_subcmd_finds_first_positional() {
        let args: Vec<String> = vec!["test".into(), "--release".into()];
        let result = extract_subcmd("cargo", &args, "usage", "test").unwrap();
        assert_eq!(result, Some(("test", 0)));
    }

    /// Flags before the subcommand are skipped; the positional is found at the
    /// correct index so `prepend_without` will remove the right element.
    #[test]
    fn test_extract_subcmd_skips_leading_flags() {
        let args: Vec<String> = vec!["--show-stats".into(), "build".into(), "--release".into()];
        let result = extract_subcmd("cargo", &args, "usage", "build").unwrap();
        assert_eq!(result, Some(("build", 1)));
    }

    /// When every arg starts with `-` there is no subcommand; the function
    /// prints the error message and returns `None` (caller returns FAILURE).
    #[test]
    fn test_extract_subcmd_returns_none_when_all_flags() {
        let args: Vec<String> = vec!["--show-stats".into(), "--json".into()];
        let result = extract_subcmd("cargo", &args, "usage", "test").unwrap();
        assert!(result.is_none());
    }

    /// Empty arg slice → no subcommand found, returns `None`.
    #[test]
    fn test_extract_subcmd_empty_args() {
        let args: Vec<String> = vec![];
        let result = extract_subcmd("cargo", &args, "usage", "test").unwrap();
        assert!(result.is_none());
    }

    // ========================================================================
    // prepend_without tests
    // ========================================================================

    /// Removes an element from the middle and prepends the tool name.
    #[test]
    fn test_prepend_without_removes_middle_element() {
        let args: Vec<String> = vec!["--show-stats".into(), "test".into(), "--release".into()];
        // skip_idx=1 removes "test"; result is ["cargo", "--show-stats", "--release"]
        let result = prepend_without("cargo", &args, 1);
        assert_eq!(result, vec!["cargo", "--show-stats", "--release"]);
    }

    /// Removes the first element and prepends the tool name.
    #[test]
    fn test_prepend_without_removes_first_element() {
        let args: Vec<String> = vec!["test".into(), "--release".into()];
        // skip_idx=0 removes "test"; result is ["cargo", "--release"]
        let result = prepend_without("cargo", &args, 0);
        assert_eq!(result, vec!["cargo", "--release"]);
    }

    /// Removes the last element and prepends the tool name.
    #[test]
    fn test_prepend_without_removes_last_element() {
        let args: Vec<String> = vec!["--release".into(), "test".into()];
        // skip_idx=1 removes "test"; result is ["cargo", "--release"]
        let result = prepend_without("cargo", &args, 1);
        assert_eq!(result, vec!["cargo", "--release"]);
    }

    /// Single-element slice: removes that element, leaving only the tool name.
    #[test]
    fn test_prepend_without_single_element_slice() {
        let args: Vec<String> = vec!["test".into()];
        let result = prepend_without("cargo", &args, 0);
        assert_eq!(result, vec!["cargo"]);
    }

    /// Out-of-bounds skip_idx fires the debug_assert (debug builds only).
    ///
    /// This test documents the invariant: callers are responsible for passing a
    /// valid index.  The assert only fires in debug builds (`cfg(debug_assertions)`),
    /// so this test is gated on that condition.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "skip_idx 1 out of bounds for args len 1")]
    fn test_prepend_without_panics_on_out_of_bounds() {
        let args: Vec<String> = vec!["test".into()];
        prepend_without("cargo", &args, 1); // skip_idx=1 is out of bounds for len 1
    }

    // ========================================================================
    // dispatch() coverage — KNOWN_SUBCOMMANDS sync guard
    // ========================================================================

    /// Verify that every entry in KNOWN_SUBCOMMANDS routes through dispatch()
    /// without panicking.
    ///
    /// dispatch() calls real subcommand handlers which may fail for unrelated
    /// reasons (missing binary, empty args), but they must never panic. Any
    /// panic here means a match arm is missing for a registered subcommand.
    #[test]
    fn test_dispatch_covers_all_known_subcommands() {
        use std::panic;

        for &subcommand in KNOWN_SUBCOMMANDS {
            // Pass --help so handlers exit cleanly rather than spawning real
            // processes. Most category handlers print help and return SUCCESS
            // when --help is the only arg.
            let args: Vec<String> = vec!["--help".to_string()];

            // AnalyticsConfig is not UnwindSafe, so construct it inside the closure.
            let result = panic::catch_unwind(|| {
                let a = crate::analytics::AnalyticsConfig {
                    enabled: false,
                    session_id: None,
                    input_cost_per_mtok: None,
                };
                dispatch(subcommand, &args, &a)
            });

            if let Err(ref payload) = result {
                // Surface the panic payload so non-routing panics (real bugs) are
                // distinguishable from missing-match-arm panics in CI output.
                let msg = payload
                    .downcast_ref::<String>()
                    .map(|s| s.as_str())
                    .or_else(|| payload.downcast_ref::<&str>().copied())
                    .unwrap_or("<non-string panic payload>");
                eprintln!("dispatch() panicked for '{subcommand}' — panic payload: {msg}");
            }
            assert!(
                result.is_ok(),
                "dispatch() panicked for known subcommand '{subcommand}': \
                 handler should not panic (check handler implementation)"
            );
        }

        // Also verify that an unknown name correctly returns an Err from dispatch().
        let analytics = crate::analytics::AnalyticsConfig {
            enabled: false,
            session_id: None,
            input_cost_per_mtok: None,
        };
        let unknown_result = dispatch("__unknown_xyz__", &[], &analytics);
        assert!(
            unknown_result.is_err(),
            "dispatch() should return Err for unknown subcommand"
        );
        let err_msg = unknown_result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Unknown subcommand"),
            "error message should mention 'Unknown subcommand', got: {err_msg}"
        );
    }
}
