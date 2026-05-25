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

/// Re-export for sub-module use.
pub(super) use super::combine_stdout_stderr;
pub(super) use super::inject_format_json;
pub(super) use super::log_result_to_infra;
use crate::cmd::{ToolRunConfig, run_tool};
use crate::analytics::CommandType;

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "docker",
    env_overrides: &[],
    install_hint: "Install Docker: https://docs.docker.com/get-docker/",
    family: "infra",
    skip_ansi_strip: false,
    command_type: CommandType::Infra,
};

/// Global docker flags that accept a value in the following token.
///
/// Used by [`super::find_subcommand_index`] to skip global flags before the
/// actual subcommand when dispatching (e.g. `docker --host tcp://h:2376 ps`).
const DOCKER_VALUE_FLAGS: &[&str] = &["--host", "-H", "--context", "--config", "--log-level", "-l"];

/// Run `skim docker [args...]`.
pub(crate) fn run(args: &[String], ctx: &crate::cmd::RunContext) -> anyhow::Result<ExitCode> {
    let sub_idx = super::find_subcommand_index(args, DOCKER_VALUE_FLAGS);
    let subcmd = args.get(sub_idx).map(|s| s.as_str()).unwrap_or("");
    let action = args.get(sub_idx + 1).map(|s| s.as_str()).unwrap_or("");

    match (subcmd, action) {
        ("ps", _) | ("container", "ls") => {
            run_tool(CONFIG, args, ctx, ps::prepare_args, ps::parse_impl)
        }
        ("images", _) => {
            run_tool(CONFIG, args, ctx, images::prepare_args, images::parse_impl)
        }
        ("inspect", _) => run_tool(
            CONFIG,
            args,
            ctx,
            inspect::prepare_args,
            inspect::parse_impl,
        ),
        ("build", _) => run_tool(CONFIG, args, ctx, build::prepare_args, build::parse_impl),
        ("logs", _) => run_tool(CONFIG, args, ctx, logs::prepare_args, logs::parse_impl),
        ("compose", "ps") => run_tool(CONFIG, args, ctx, |_| {}, compose::parse_ps),
        ("compose", "logs") => run_tool(CONFIG, args, ctx, |_| {}, compose::parse_logs),
        _ => run_tool(CONFIG, args, ctx, |_| {}, super::passthrough_parse),
    }
}
