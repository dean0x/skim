//! Kubectl CLI parser with three-tier degradation (#117).
//!
//! Executes `kubectl` and parses the output into structured `InfraResult`.
//!
//! Dispatches to sub-parsers based on `(subcmd, action)`:
//! - `kubectl get`      → [`get`] — inject `-o json`, parse PodList/etc.
//! - `kubectl describe` → [`describe`] — text-only (NEVER inject `-o json`)
//! - `kubectl logs`     → [`logs`] — delegate to log compression pipeline
//!
//! # Safety invariants
//!
//! - `kubectl describe`: NEVER inject `-o json` — `describe` does not support it

pub(crate) mod describe;
pub(crate) mod get;
pub(crate) mod logs;

use std::process::ExitCode;

use crate::output::canonical::InfraResult;
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, run_infra_tool, InfraToolConfig};

const CONFIG: InfraToolConfig<'static> = InfraToolConfig {
    program: "kubectl",
    env_overrides: &[],
    install_hint: "Install kubectl: https://kubernetes.io/docs/tasks/tools/",
};

/// Run `skim kubectl [args...]`.
pub(crate) fn run(args: &[String], ctx: &crate::cmd::RunContext) -> anyhow::Result<ExitCode> {
    let subcmd = args.first().map(|s| s.as_str()).unwrap_or("");

    match subcmd {
        "get" => run_infra_tool(CONFIG, args, ctx, get::prepare_args, get::parse_impl),
        "describe" => run_infra_tool(
            CONFIG,
            args,
            ctx,
            describe::prepare_args,
            describe::parse_impl,
        ),
        "logs" => run_infra_tool(CONFIG, args, ctx, logs::prepare_args, logs::parse_impl),
        _ => run_infra_tool(CONFIG, args, ctx, |_| {}, passthrough_parse),
    }
}

/// Passthrough parser — returns raw combined output unchanged.
fn passthrough_parse(output: &CommandOutput) -> ParseResult<InfraResult> {
    let combined = combine_stdout_stderr(output);
    ParseResult::Passthrough(combined.into_owned())
}
