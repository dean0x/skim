//! Docker CLI parser with three-tier degradation (#117).
//!
//! Executes `docker` and parses the output into structured `InfraResult`.
//!
//! Dispatches to sub-parsers based on `(subcmd, action)`:
//! - `docker ps` / `docker container ls` → [`ps`]
//! - `docker images`                      → [`images`]
//! - `docker inspect`                     → [`inspect`]
//! - `docker build`                       → [`build`]
//! - `docker logs`                        → [`logs`]
//! - `docker compose ps`                  → [`compose::parse_ps`]
//! - `docker compose logs`                → [`compose::parse_logs`]
//!
//! # Safety invariants
//!
//! - `docker build`: NEVER inject `--format json` (build doesn't support it)
//! - `docker inspect`: NEVER inject `--format` (already outputs JSON)
//! - `docker logs`: NEVER inject any format flag (no format flag exists)

pub(crate) mod build;
pub(crate) mod compose;
pub(crate) mod images;
pub(crate) mod inspect;
pub(crate) mod logs;
pub(crate) mod ps;

use std::process::ExitCode;

use crate::output::canonical::InfraResult;
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr as infra_combine, run_infra_tool, InfraToolConfig};

/// Re-export for sub-module use.
pub(super) use super::combine_stdout_stderr;

const CONFIG: InfraToolConfig<'static> = InfraToolConfig {
    program: "docker",
    env_overrides: &[],
    install_hint: "Install Docker: https://docs.docker.com/get-docker/",
};

/// Run `skim docker [args...]`.
pub(crate) fn run(args: &[String], ctx: &crate::cmd::RunContext) -> anyhow::Result<ExitCode> {
    let subcmd = args.first().map(|s| s.as_str()).unwrap_or("");
    let action = args.get(1).map(|s| s.as_str()).unwrap_or("");

    match (subcmd, action) {
        ("ps", _) | ("container", "ls") => {
            run_infra_tool(CONFIG, args, ctx, ps::prepare_args, ps::parse_impl)
        }
        ("images", _) => {
            run_infra_tool(CONFIG, args, ctx, images::prepare_args, images::parse_impl)
        }
        ("inspect", _) => run_infra_tool(
            CONFIG,
            args,
            ctx,
            inspect::prepare_args,
            inspect::parse_impl,
        ),
        ("build", _) => run_infra_tool(CONFIG, args, ctx, build::prepare_args, build::parse_impl),
        ("logs", _) => run_infra_tool(CONFIG, args, ctx, logs::prepare_args, logs::parse_impl),
        ("compose", "ps") => run_infra_tool(CONFIG, args, ctx, |_| {}, compose::parse_ps),
        ("compose", "logs") => run_infra_tool(CONFIG, args, ctx, |_| {}, compose::parse_logs),
        _ => {
            // Passthrough: run the command unchanged, emit raw output
            run_infra_tool(CONFIG, args, ctx, |_| {}, passthrough_parse)
        }
    }
}

/// Passthrough parser — returns raw combined output unchanged.
fn passthrough_parse(output: &CommandOutput) -> ParseResult<InfraResult> {
    let combined = infra_combine(output);
    ParseResult::Passthrough(combined.into_owned())
}
