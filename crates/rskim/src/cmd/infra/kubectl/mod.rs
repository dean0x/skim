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

use super::{run_infra_tool, InfraToolConfig};

/// Re-exports for sub-module use.
pub(super) use super::combine_stdout_stderr;
pub(super) use super::log_result_to_infra;

const CONFIG: InfraToolConfig<'static> = InfraToolConfig {
    program: "kubectl",
    env_overrides: &[],
    install_hint: "Install kubectl: https://kubernetes.io/docs/tasks/tools/",
};

/// Global kubectl flags that accept a value in the following token.
///
/// Used by [`find_subcommand_index`] to skip global flags before the
/// actual subcommand when dispatching (e.g. `kubectl -n ns get pods`).
const KUBECTL_VALUE_FLAGS: &[&str] = &[
    "--context",
    "-n",
    "--namespace",
    "--kubeconfig",
    "--server",
    "--as",
    "--as-group",
    "-v",
    "--v",
    "--request-timeout",
    "--cache-dir",
    "--cluster",
    "--token",
    "--user",
];

/// Run `skim kubectl [args...]`.
pub(crate) fn run(args: &[String], ctx: &crate::cmd::RunContext) -> anyhow::Result<ExitCode> {
    let sub_idx = super::find_subcommand_index(args, KUBECTL_VALUE_FLAGS);
    let subcmd = args.get(sub_idx).map(|s| s.as_str()).unwrap_or("");

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
        _ => run_infra_tool(CONFIG, args, ctx, |_| {}, super::passthrough_parse),
    }
}
