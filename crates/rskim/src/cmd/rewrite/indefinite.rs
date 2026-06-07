//! Indefinite-duration command detection (ADR-008 / Part C).
//!
//! Detects commands that run indefinitely (daemon processes, watch modes, live
//! log followers) so the dispatcher can pass them through with inherited stdio
//! rather than trying to capture and compress their output.
//!
//! # Design principles
//!
//! 1. **Program-aware, not flag-generic.** The detector is keyed on specific
//!    program + flag combinations. It never scans for generic patterns like
//!    "any command with `-f`" because that would misfire on `grep -f`, `rm -f`,
//!    `git push -f`, etc.
//!
//! 2. **Conservative false-negative over aggressive false-positive.** A missed
//!    daemon degrades gracefully to the old buffered path (the 64 MiB cap is
//!    still there, and `SKIM_PASSTHROUGH=1` is an explicit escape hatch).
//!    A false-positive only loses compression for that single run, never
//!    correctness — so it is the safer failure mode.
//!
//! 3. **`tokens` slice, not raw string.** Callers tokenize once; this function
//!    borrows the same `&[&str]` without re-parsing.
//!
//! # Categories
//!
//! - `watch <…>` — always indefinite
//! - Log followers: `tail`/`journalctl` + `-f`/`-F`/`--follow`; `docker [compose] logs` + `-f`/`--follow`; `kubectl logs` + `-f`/`--follow`
//! - Watch-mode builders/test runners: `tsc --watch/-w`; `jest --watch/--watchAll`; `webpack --watch/-w`/`webpack serve`; `vite`/`rollup`/`esbuild` + `--watch`; `vitest` bare or `--watch` (finite when `run` sub-command present); `nodemon`/`serve`/`http-server`/`live-server` — always
//! - Dev servers: `next dev`, `nuxt dev`, `astro dev`, `ng serve`, `vite` bare/`dev`/`serve`/`preview`
//! - Package-manager scripts: `npm|yarn|pnpm|bun` with script `dev|start|serve|watch`

/// Return `true` when `tokens` represents an indefinitely-running command.
///
/// The detection is heuristic and conservative: a false-negative (missed daemon)
/// degrades to the buffered capture path; a false-positive (wrongly flagged
/// finite command) only loses compression for that invocation.
///
/// The function must NEVER be called on zero-length slices — the assertion
/// guards against that precondition violation.
pub(crate) fn is_indefinite_command(tokens: &[&str]) -> bool {
    debug_assert!(!tokens.is_empty(), "tokens must not be empty");
    if tokens.is_empty() {
        return false;
    }

    // Strip a leading env-var assignment like `NODE_ENV=dev npm run dev`.
    // Walk past `KEY=VALUE` tokens at the start; the first token without `=`
    // is the program name.
    let program_idx = tokens.iter().position(|t| !t.contains('=')).unwrap_or(0);

    let program = tokens[program_idx];
    let rest = &tokens[program_idx + 1..];

    match program {
        // ── watch ─────────────────────────────────────────────────────────
        // `watch` is always indefinite regardless of what it runs.
        "watch" => true,

        // ── log followers ─────────────────────────────────────────────────
        "tail" | "journalctl" => has_follow_flag(rest),

        "docker" => docker_is_indefinite(rest),

        "kubectl" => kubectl_logs_is_indefinite(rest),

        // ── watch-mode build / test runners ───────────────────────────────
        "tsc" => has_watch_flag(rest),

        "jest" => has_jest_watch_flag(rest),

        "webpack" => webpack_is_indefinite(rest),

        "vite" => vite_is_indefinite(rest),

        "rollup" | "esbuild" => has_watch_flag(rest),

        "vitest" => vitest_is_indefinite(rest),

        // Always-indefinite dev-server tools
        "nodemon" | "serve" | "http-server" | "live-server" => true,

        // ── dev servers ───────────────────────────────────────────────────
        "next" => rest.first().is_some_and(|&s| s == "dev"),

        "nuxt" | "astro" => rest.first().is_some_and(|&s| s == "dev"),

        "ng" => rest.first().is_some_and(|&s| s == "serve"),

        // ── package manager scripts ───────────────────────────────────────
        "npm" | "yarn" | "pnpm" | "bun" => pm_is_indefinite(program, rest),

        _ => false,
    }
}

// ============================================================================
// Helper predicates
// ============================================================================

/// True when `rest` contains `-f`, `-F`, or `--follow`.
fn has_follow_flag(rest: &[&str]) -> bool {
    rest.iter().any(|&s| matches!(s, "-f" | "-F" | "--follow"))
}

/// True when `rest` contains `-w` or `--watch`.
fn has_watch_flag(rest: &[&str]) -> bool {
    rest.iter().any(|&s| matches!(s, "-w" | "--watch"))
}

/// Docker-specific: `docker logs -f/--follow` or `docker compose logs -f/--follow`.
fn docker_is_indefinite(rest: &[&str]) -> bool {
    match rest.first() {
        Some(&"logs") => has_follow_flag(&rest[1..]),
        // `docker compose logs [-f/--follow]`
        Some(&"compose") if rest.get(1) == Some(&"logs") => has_follow_flag(&rest[2..]),
        _ => false,
    }
}

/// kubectl-specific: `kubectl logs -f/--follow`.
fn kubectl_logs_is_indefinite(rest: &[&str]) -> bool {
    rest.first() == Some(&"logs") && has_follow_flag(&rest[1..])
}

/// Jest-specific: `--watch` or `--watchAll` (not `--watchman` etc.).
fn has_jest_watch_flag(rest: &[&str]) -> bool {
    rest.iter().any(|&s| matches!(s, "--watch" | "--watchAll"))
}

/// Webpack: `--watch`/`-w` or the `serve` subcommand (webpack dev server).
fn webpack_is_indefinite(rest: &[&str]) -> bool {
    has_watch_flag(rest) || rest.first().is_some_and(|&s| s == "serve")
}

/// Vite: bare (no args), or `dev`, `serve`, `preview` subcommand, or `--watch`.
fn vite_is_indefinite(rest: &[&str]) -> bool {
    if rest.is_empty() {
        return true; // bare `vite` starts the dev server
    }
    // First positional argument (skip flags)
    let first_positional = rest.iter().find(|&&s| !s.starts_with('-'));
    match first_positional {
        Some(&"dev") | Some(&"serve") | Some(&"preview") => true,
        // `vite build` is finite even with --watch ... keep conservative
        Some(&"build") => has_watch_flag(rest),
        // Any other subcommand: check for --watch
        Some(_) => has_watch_flag(rest),
        // No positional at all (only flags) — bare server
        None => true,
    }
}

/// Vitest: indefinite unless a `run` subcommand is present.
///
/// - `vitest` → indefinite (interactive watch mode by default)
/// - `vitest --watch` → indefinite
/// - `vitest run` → FINITE (runs once)
/// - `vitest run --reporter verbose` → FINITE
fn vitest_is_indefinite(rest: &[&str]) -> bool {
    // If `run` appears as a positional token, the invocation is finite.
    !rest.contains(&"run")
}

/// Package-manager script detection.
///
/// Indefinite scripts: `dev`, `start`, `serve`, `watch`.
/// Finite scripts: `build`, `test`, `install`, `ci`, `lint`, …
fn pm_is_indefinite(program: &str, rest: &[&str]) -> bool {
    // The script name depends on invocation style:
    //   npm run dev        → rest = ["run", "dev", ...]  → script after "run"
    //   npm start          → rest = ["start"]             → "start" is the script
    //   yarn dev           → rest = ["dev", ...]          → first positional
    //   pnpm serve         → rest = ["serve", ...]        → first positional
    //   bun dev            → rest = ["dev", ...]          → first positional

    const INDEFINITE_SCRIPTS: &[&str] = &["dev", "start", "serve", "watch"];

    // `npm install`, `npm ci`, `npm test`, `npm run build` etc. are finite.
    // So are `yarn install`, `pnpm install`, etc.
    // Guard: never flag `build`, `test`, `install`, `ci` even if they appear
    // after `run`.
    const FINITE_SCRIPTS: &[&str] = &[
        "build", "test", "install", "ci", "lint", "audit", "add", "remove", "update",
    ];

    let script = match program {
        // npm and pnpm both support `<pm> run <script>` and `<pm> run-script <script>`.
        // npm also has `npm start` (a builtin alias for `npm run start`).
        "npm" | "pnpm" => match rest.first() {
            Some(&"run") | Some(&"run-script") => rest.get(1).copied(),
            Some(&s) => Some(s),
            None => None,
        },
        // yarn and bun: first positional is either a built-in or the script name.
        // Both support `yarn run dev` and `bun run dev` but also `yarn dev` / `bun dev`.
        _ => {
            let first = rest.iter().copied().find(|s| !s.starts_with('-'));
            match first {
                Some("run") | Some("run-script") => {
                    // yarn run dev / bun run dev
                    rest.iter()
                        .copied()
                        .skip_while(|s| *s == "run" || *s == "run-script")
                        .find(|s| !s.starts_with('-'))
                }
                other => other,
            }
        }
    };

    match script {
        Some(s) if INDEFINITE_SCRIPTS.contains(&s) => true,
        Some(s) if FINITE_SCRIPTS.contains(&s) => false,
        _ => false,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(s: &str) -> Vec<&str> {
        s.split_whitespace().collect()
    }

    fn is_indefinite(cmd: &str) -> bool {
        let t = tokens(cmd);
        is_indefinite_command(&t)
    }

    // ─── Positive cases (should be true) ──────────────────────────────────

    #[test]
    fn test_indefinite_positive_cases() {
        let cases = [
            "watch ls",
            "tail -f log",
            "tail -F /var/log/syslog",
            "tail --follow log.txt",
            "journalctl --follow",
            "journalctl -f",
            "kubectl logs -f pod",
            "kubectl logs --follow mypod",
            "docker logs --follow c",
            "docker logs -f mycontainer",
            "docker compose logs -f",
            "docker compose logs --follow",
            "tsc --watch",
            "tsc -w",
            "jest --watch",
            "jest --watchAll",
            "webpack -w",
            "webpack --watch",
            "webpack serve",
            "vite",
            "vite dev",
            "vite serve",
            "vite preview",
            "vitest",
            "vitest --watch",
            "next dev",
            "nuxt dev",
            "astro dev",
            "ng serve",
            "nodemon app.js",
            "serve .",
            "http-server .",
            "live-server",
            "npm run dev",
            "npm start",
            "yarn dev",
            "pnpm serve",
            "bun dev",
            "npm run watch",
            "pnpm run dev",
        ];

        for cmd in &cases {
            assert!(
                is_indefinite(cmd),
                "Expected is_indefinite_command to return true for: {cmd:?}"
            );
        }
    }

    // ─── Negative cases (should be false) ─────────────────────────────────

    #[test]
    fn test_indefinite_negative_cases() {
        let cases = [
            "grep -w word file",
            "git push -f",
            "rm -f x",
            "vitest run",
            "vitest run --reporter verbose",
            "npm test",
            "npm install",
            "npm run build",
            "npm ci",
            "cargo build",
            "tsc",
            "jest --ci",
            "docker logs c", // no -f flag
            "kubectl get pods",
            "kubectl logs mypod", // no -f flag
            "tail -n 5 log",
            "journalctl -n 100",
            "rollup",               // bare rollup is a one-shot build
            "esbuild src/index.ts", // bare esbuild is a one-shot build
            "yarn install",
            "pnpm install",
            "bun install",
            "vite build",
        ];

        for cmd in &cases {
            assert!(
                !is_indefinite(cmd),
                "Expected is_indefinite_command to return false for: {cmd:?}"
            );
        }
    }
}
