//! Declarative rewrite rule table.
//!
//! 100 rules grouped into 7 category arrays: TEST (10), BUILD (4), GIT (7),
//! LINT (38), PKG (18), INFRA (14), FILE_OPS (9).
//! Only `engine.rs` consumes `all_rules()`.
//!
//! # Pipe-source exclusion (AD-RW-2)
//!
//! Rules with `exclude_pipe_source: true` are suppressed when the command is
//! the *source* side of a pipe expression (e.g. `ls | head`, `find . | head`,
//! `rg pat | head`).  The check is co-located with the rule — adding a new
//! excluded command only requires setting the flag in the rule struct.
//!
//! Current excluded commands: `ls` (catch-all), `grep` (catch-all), `find`,
//! `rg`.  Catch-alls are also guarded by `skip_if_flag_prefix` for `--help`,
//! `--version`, and `-V` so that informational invocations pass through
//! unmodified.  SEE: AD-RW-2.

use std::sync::LazyLock;

use super::types::{RewriteCategory, RewriteRule};

// ============================================================================
// TEST rules (10)
// ============================================================================

const TEST_RULES: &[RewriteRule] = &[
    // cargo (longest prefix first)
    RewriteRule {
        prefix: &["cargo", "nextest", "run"],
        rewrite_to: &["skim", "test", "cargo"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["cargo", "test"],
        rewrite_to: &["skim", "test", "cargo"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
    },
    // python (longest prefix first)
    RewriteRule {
        prefix: &["python3", "-m", "pytest"],
        rewrite_to: &["skim", "test", "pytest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["python", "-m", "pytest"],
        rewrite_to: &["skim", "test", "pytest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
    },
    // npx
    RewriteRule {
        prefix: &["npx", "vitest"],
        rewrite_to: &["skim", "test", "vitest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["npx", "jest"],
        rewrite_to: &["skim", "test", "jest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
    },
    // bare commands
    RewriteRule {
        prefix: &["pytest"],
        rewrite_to: &["skim", "test", "pytest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["vitest"],
        rewrite_to: &["skim", "test", "vitest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["jest"],
        rewrite_to: &["skim", "test", "jest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["go", "test"],
        rewrite_to: &["skim", "test", "go"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
    },
];

// ============================================================================
// BUILD rules (4)
// ============================================================================

const BUILD_RULES: &[RewriteRule] = &[
    RewriteRule {
        prefix: &["cargo", "clippy"],
        rewrite_to: &["skim", "build", "clippy"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["cargo", "build"],
        rewrite_to: &["skim", "build", "cargo"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
    },
    // npx
    RewriteRule {
        prefix: &["npx", "tsc"],
        rewrite_to: &["skim", "build", "tsc"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
    },
    // tsc bare
    RewriteRule {
        prefix: &["tsc"],
        rewrite_to: &["skim", "build", "tsc"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
    },
];

// ============================================================================
// GIT rules (7)
// ============================================================================

const GIT_RULES: &[RewriteRule] = &[
    RewriteRule {
        prefix: &["git", "status"],
        rewrite_to: &["skim", "git", "status"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Git,
        exclude_pipe_source: false,
    },
    // DESIGN NOTE (AD-RW-4, extended 2026-04-11): `--stat`, `--name-only` removed
    // from skip list. These are Group B flags (already-compact output).
    // Removing them allows `git diff --stat` and `git diff --name-only` to
    // flow through to the handler's passthrough branch. The handler's
    // `user_has_flag` check (diff/mod.rs) still catches these and calls
    // `run_passthrough`, so output is byte-identical to raw git. This also
    // fixes the `--staged` collision (previously eaten by loose `--stat`
    // prefix matching).
    //
    // Extension (AD-RW-11, see rewrite/acknowledge.rs): lint tools whose raw
    // output is already minimal (`prettier --check`, `rustfmt --check`,
    // `cargo fmt --check`) are acknowledged via ACK prefix patterns in
    // acknowledge.rs and short-circuit before the rule table. The prettier
    // and rustfmt entries further down in this table are therefore dead code
    // kept only to document the historical mapping — the ACK path in
    // engine.rs runs first. Removing them entirely is out of scope per the
    // "don't refactor rewrite engine" rule; the ACK tests in
    // cli_e2e_rewrite_alignment.rs prove they are unreachable.
    RewriteRule {
        prefix: &["git", "diff"],
        rewrite_to: &["skim", "git", "diff"],
        skip_if_flag_prefix: &["--shortstat", "--numstat", "--name-status", "--check"],
        category: RewriteCategory::Git,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["git", "fetch"],
        rewrite_to: &["skim", "git", "fetch"],
        skip_if_flag_prefix: &["--dry-run", "-q", "--quiet"],
        category: RewriteCategory::Git,
        exclude_pipe_source: false,
    },
    // DESIGN NOTE (AD-RW-4): `--format` and `--pretty` removed from skip list.
    // The log handler (log.rs) already detects these flags and calls
    // `run_passthrough`, so users see raw git output. Removing them from
    // the skip list means the rewrite rule fires and the handler decides.
    RewriteRule {
        prefix: &["git", "log"],
        rewrite_to: &["skim", "git", "log"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Git,
        exclude_pipe_source: false,
    },
    // git show — new rule (AD-GIT-5, updated 2026-04-11)
    //
    // Handles `git show <hash>`, `git show <hash>:<path>`, and defaults.
    // The handler (cmd/git/show.rs) dispatches to commit-mode or
    // file-content-mode based on argument shape.
    //
    // As of AD-GIT-8 (see show.rs), the commit-mode path preserves the full
    // commit message body AND `Merge: p1 p2` parent lines via the structured
    // `body: String` and `parents: Option<String>` fields on `CommitHeader`.
    // Earlier versions dropped both, which was corrected in the PR that
    // bundles this AD-GIT-5 update with the AD-GIT-8 body/parents-preservation work.
    RewriteRule {
        prefix: &["git", "show"],
        rewrite_to: &["skim", "git", "show"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Git,
        exclude_pipe_source: false,
    },
    // git commit (B.7)
    //
    // Parses commit output (both stdout and stderr) into a compact summary.
    // The handler (cmd/git/commit.rs) handles --amend, --allow-empty,
    // --no-verify, -v (verbose diff truncation), GPG-signed, merge, and root
    // commits. See AD-GC-1, AD-GC-2.
    RewriteRule {
        prefix: &["git", "commit"],
        rewrite_to: &["skim", "git", "commit"],
        skip_if_flag_prefix: &["--help"],
        category: RewriteCategory::Git,
        exclude_pipe_source: false,
    },
    // git push (B.8)
    //
    // Parses push output (stderr) into a compact summary. The handler
    // (cmd/git/push.rs) auto-injects --porcelain, scrubs credential URLs,
    // and handles dry-run, delete, force-with-lease, and LFS pre-push.
    // See AD-GP-1, AD-GP-2.
    RewriteRule {
        prefix: &["git", "push"],
        rewrite_to: &["skim", "git", "push"],
        skip_if_flag_prefix: &["--help"],
        category: RewriteCategory::Git,
        exclude_pipe_source: false,
    },
];

// ============================================================================
// LINT rules (38)
// ============================================================================

const LINT_RULES: &[RewriteRule] = &[
    // eslint
    RewriteRule {
        prefix: &["npx", "eslint"],
        rewrite_to: &["skim", "lint", "eslint"],
        skip_if_flag_prefix: &["--format", "-f"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["eslint"],
        rewrite_to: &["skim", "lint", "eslint"],
        skip_if_flag_prefix: &["--format", "-f"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    // ruff (longest prefix first)
    //
    // AD-LINT-20 (2026-04-15): `ruff format --check` and `ruff format` (apply mode)
    // are routed through the format-mode parse path in ruff.rs. The ruff parser
    // detects `is_format_mode` from the first user argument (`"format"`).
    RewriteRule {
        prefix: &["ruff", "format", "--check"],
        rewrite_to: &["skim", "lint", "ruff"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["ruff", "format"],
        rewrite_to: &["skim", "lint", "ruff"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["ruff", "check"],
        rewrite_to: &["skim", "lint", "ruff"],
        skip_if_flag_prefix: &["--output-format"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["ruff"],
        rewrite_to: &["skim", "lint", "ruff"],
        skip_if_flag_prefix: &["--output-format"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    // mypy (longest prefix first: python3 -m mypy, python -m mypy, mypy)
    RewriteRule {
        prefix: &["python3", "-m", "mypy"],
        rewrite_to: &["skim", "lint", "mypy"],
        skip_if_flag_prefix: &["--output"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["python", "-m", "mypy"],
        rewrite_to: &["skim", "lint", "mypy"],
        skip_if_flag_prefix: &["--output"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["mypy"],
        rewrite_to: &["skim", "lint", "mypy"],
        skip_if_flag_prefix: &["--output"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    // golangci-lint
    RewriteRule {
        prefix: &["golangci-lint", "run"],
        rewrite_to: &["skim", "lint", "golangci"],
        skip_if_flag_prefix: &["--out-format"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["golangci-lint"],
        rewrite_to: &["skim", "lint", "golangci"],
        skip_if_flag_prefix: &["--out-format"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    // prettier (longest prefix first: npx prettier, prettier)
    //
    // AD-LINT-20 (2026-04-15): `prettier --write` and `-w` are routed through the
    // format-mode parse path in prettier.rs. `is_format_mode` detects `--write`
    // or `-w` in the user arguments. Check-mode rules unchanged.
    RewriteRule {
        prefix: &["npx", "prettier", "--write"],
        rewrite_to: &["skim", "lint", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["npx", "prettier", "-w"],
        rewrite_to: &["skim", "lint", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["prettier", "--write"],
        rewrite_to: &["skim", "lint", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["prettier", "-w"],
        rewrite_to: &["skim", "lint", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["npx", "prettier", "--check"],
        rewrite_to: &["skim", "lint", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["prettier", "--check"],
        rewrite_to: &["skim", "lint", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    // rustfmt (longest prefix first)
    RewriteRule {
        prefix: &["cargo", "fmt", "--", "--check"],
        rewrite_to: &["skim", "lint", "rustfmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["cargo", "fmt", "--check"],
        rewrite_to: &["skim", "lint", "rustfmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["rustfmt", "--check"],
        rewrite_to: &["skim", "lint", "rustfmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    // black
    RewriteRule {
        prefix: &["black", "--check"],
        rewrite_to: &["skim", "lint", "black"],
        skip_if_flag_prefix: &["--diff"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["black"],
        rewrite_to: &["skim", "lint", "black"],
        skip_if_flag_prefix: &["--diff"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    // gofmt (longest prefix first)
    RewriteRule {
        prefix: &["gofmt", "-l"],
        rewrite_to: &["skim", "lint", "gofmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["gofmt", "-d"],
        rewrite_to: &["skim", "lint", "gofmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["gofmt"],
        rewrite_to: &["skim", "lint", "gofmt"],
        skip_if_flag_prefix: &["-w"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    // biome (longest prefix first)
    RewriteRule {
        prefix: &["npx", "biome", "check"],
        rewrite_to: &["skim", "lint", "biome", "check"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["biome", "check"],
        rewrite_to: &["skim", "lint", "biome", "check"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["npx", "biome", "format"],
        rewrite_to: &["skim", "lint", "biome", "format"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["biome", "format"],
        rewrite_to: &["skim", "lint", "biome", "format"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["npx", "biome", "lint"],
        rewrite_to: &["skim", "lint", "biome", "lint"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["biome", "lint"],
        rewrite_to: &["skim", "lint", "biome", "lint"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["npx", "biome"],
        rewrite_to: &["skim", "lint", "biome"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["biome"],
        rewrite_to: &["skim", "lint", "biome"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    // dprint (longest prefix first)
    RewriteRule {
        prefix: &["dprint", "check"],
        rewrite_to: &["skim", "lint", "dprint", "check"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["dprint", "fmt"],
        rewrite_to: &["skim", "lint", "dprint", "fmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["dprint"],
        rewrite_to: &["skim", "lint", "dprint"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    // oxlint
    RewriteRule {
        prefix: &["npx", "oxlint"],
        rewrite_to: &["skim", "lint", "oxlint"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["oxlint"],
        rewrite_to: &["skim", "lint", "oxlint"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
    },
];

// ============================================================================
// PKG rules (18)
// ============================================================================

const PKG_RULES: &[RewriteRule] = &[
    // cargo
    RewriteRule {
        prefix: &["cargo", "audit"],
        rewrite_to: &["skim", "pkg", "cargo", "audit"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    // npm (canonical + aliases)
    RewriteRule {
        prefix: &["npm", "audit"],
        rewrite_to: &["skim", "pkg", "npm", "audit"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["npm", "install"],
        rewrite_to: &["skim", "pkg", "npm", "install"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["npm", "i"],
        rewrite_to: &["skim", "pkg", "npm", "install"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["npm", "ci"],
        rewrite_to: &["skim", "pkg", "npm", "install"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["npm", "outdated"],
        rewrite_to: &["skim", "pkg", "npm", "outdated"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["npm", "list"],
        rewrite_to: &["skim", "pkg", "npm", "ls"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["npm", "ls"],
        rewrite_to: &["skim", "pkg", "npm", "ls"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    // pnpm
    RewriteRule {
        prefix: &["pnpm", "audit"],
        rewrite_to: &["skim", "pkg", "pnpm", "audit"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["pnpm", "install"],
        rewrite_to: &["skim", "pkg", "pnpm", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["pnpm", "i"],
        rewrite_to: &["skim", "pkg", "pnpm", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["pnpm", "outdated"],
        rewrite_to: &["skim", "pkg", "pnpm", "outdated"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    // pip (canonical + pip3 aliases)
    RewriteRule {
        prefix: &["pip", "install"],
        rewrite_to: &["skim", "pkg", "pip", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["pip", "check"],
        rewrite_to: &["skim", "pkg", "pip", "check"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["pip", "list"],
        rewrite_to: &["skim", "pkg", "pip", "list"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["pip3", "install"],
        rewrite_to: &["skim", "pkg", "pip", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["pip3", "check"],
        rewrite_to: &["skim", "pkg", "pip", "check"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["pip3", "list"],
        rewrite_to: &["skim", "pkg", "pip", "list"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
    },
];

// ============================================================================
// INFRA rules (14)
// ============================================================================

const INFRA_RULES: &[RewriteRule] = &[
    // gh (longest prefix first)
    //
    // DESIGN DECISION: --jq and --template skip because they apply custom
    // transformations to gh JSON output. Injecting --json fields would change
    // what the filter operates on, breaking user-defined projections.
    // --log and --log-failed skip for gh run view because they output raw CI
    // step logs — a completely different format from structured run metadata.
    // --web skips because it opens a browser tab, not stdout.
    // --watch skips because it produces a streaming TUI, not parseable output.
    RewriteRule {
        prefix: &["gh", "pr", "checks"],
        rewrite_to: &["skim", "infra", "gh", "pr", "checks"],
        skip_if_flag_prefix: &["--web", "--watch", "--jq", "--template"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["gh", "pr", "view"],
        rewrite_to: &["skim", "infra", "gh", "pr", "view"],
        skip_if_flag_prefix: &["--web", "--jq", "--template"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["gh", "pr", "list"],
        rewrite_to: &["skim", "infra", "gh", "pr", "list"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["gh", "issue", "view"],
        rewrite_to: &["skim", "infra", "gh", "issue", "view"],
        skip_if_flag_prefix: &["--web", "--jq", "--template"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["gh", "issue", "list"],
        rewrite_to: &["skim", "infra", "gh", "issue", "list"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["gh", "run", "view"],
        rewrite_to: &["skim", "infra", "gh", "run", "view"],
        skip_if_flag_prefix: &["--web", "--log", "--log-failed", "--jq", "--template"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
    },
    // gh run watch (B.5) — streaming output compression
    //
    // Routes to the streaming parser (cmd/infra/gh/run_watch.rs).
    // --help skips; --exit-status and --interval pass through to parser.
    RewriteRule {
        prefix: &["gh", "run", "watch"],
        rewrite_to: &["skim", "infra", "gh", "run", "watch"],
        skip_if_flag_prefix: &["--help"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["gh", "run", "list"],
        rewrite_to: &["skim", "infra", "gh", "run", "list"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
    },
    // gh release view (B.6) — structured release metadata
    //
    // Parses release body (capped at MAX_RELEASE_BODY_LINES outside fences),
    // assets (capped at MAX_RELEASE_ASSETS). See AD-RV-1.
    RewriteRule {
        prefix: &["gh", "release", "view"],
        rewrite_to: &["skim", "infra", "gh", "release", "view"],
        skip_if_flag_prefix: &["--help", "--web", "--jq", "--template"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["gh", "release", "list"],
        rewrite_to: &["skim", "infra", "gh", "release", "list"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
    },
    // gh api (B.4) — REST/GraphQL response compression
    //
    // Compacts JSON responses, handles pagination boundaries, --paginate,
    // base64 content fields, and binary passthrough. See AD-API-1.
    // --help skips; --jq/--template skip (user-defined transform).
    RewriteRule {
        prefix: &["gh", "api"],
        rewrite_to: &["skim", "infra", "gh", "api"],
        skip_if_flag_prefix: &["--help", "--jq", "--template"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
    },
    // aws
    RewriteRule {
        prefix: &["aws"],
        rewrite_to: &["skim", "infra", "aws"],
        skip_if_flag_prefix: &["--output"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
    },
    // curl
    RewriteRule {
        prefix: &["curl"],
        rewrite_to: &["skim", "infra", "curl"],
        skip_if_flag_prefix: &[
            "-o",
            "--output",
            "-X",
            "--request",
            "-F",
            "--upload-file",
            "-T",
        ],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
    },
    // wget
    RewriteRule {
        prefix: &["wget"],
        rewrite_to: &["skim", "infra", "wget"],
        skip_if_flag_prefix: &["-O", "-q", "--quiet"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
    },
];

// ============================================================================
// FILE_OPS rules (9)
// ============================================================================

const FILE_OPS_RULES: &[RewriteRule] = &[
    // find — pipe-source excluded so `find . | head` is not rewritten (AD-RW-2)
    RewriteRule {
        prefix: &["find"],
        rewrite_to: &["skim", "file", "find"],
        skip_if_flag_prefix: &["-exec", "-delete", "-printf", "-print0"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: true,
    },
    // ls (verbose/recursive only)
    RewriteRule {
        prefix: &["ls", "-la"],
        rewrite_to: &["skim", "file", "ls"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["ls", "-R"],
        rewrite_to: &["skim", "file", "ls"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: false,
    },
    // tree
    RewriteRule {
        prefix: &["tree"],
        rewrite_to: &["skim", "file", "tree"],
        skip_if_flag_prefix: &["-J", "--json"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: false,
    },
    // grep (recursive only)
    RewriteRule {
        prefix: &["grep", "-rn"],
        rewrite_to: &["skim", "file", "grep"],
        skip_if_flag_prefix: &["-c", "--count", "-l"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: false,
    },
    RewriteRule {
        prefix: &["grep", "-r"],
        rewrite_to: &["skim", "file", "grep"],
        skip_if_flag_prefix: &["-c", "--count", "-l"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: false,
    },
    // rg — pipe-source excluded so `rg pat | head` is not rewritten (AD-RW-2)
    RewriteRule {
        prefix: &["rg"],
        rewrite_to: &["skim", "file", "rg"],
        skip_if_flag_prefix: &["--json", "-c", "--count", "-l", "--files"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: true,
    },
    // ls catch-all (B.1) — DESIGN NOTE (AD-RW-2)
    //
    // Fires for any `ls` invocation not matched by a more-specific earlier rule
    // (e.g., `ls -la`, `ls -R`).  Guards on --help/--version/-V so that
    // informational invocations pass through unmodified.
    //
    // `exclude_pipe_source: true` prevents this rule from rewriting the source
    // side of a pipe (`ls | head`).  The compound pipeline engine skips rules
    // with this flag set on the pipe-source segment.  SEE: AD-RW-2.
    RewriteRule {
        prefix: &["ls"],
        rewrite_to: &["skim", "file", "ls"],
        skip_if_flag_prefix: &["--help", "--version", "-V"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: true,
    },
    // grep catch-all (B.2) — DESIGN NOTE (AD-RW-2)
    //
    // Fires for any `grep` invocation not matched by a more-specific earlier
    // rule (e.g., `grep -rn`, `grep -r`).  Guards on --help/--version/-V.
    //
    // `exclude_pipe_source: true` prevents `grep | head` from being rewritten on the
    // source side.  SEE: AD-RW-2.
    RewriteRule {
        prefix: &["grep"],
        rewrite_to: &["skim", "file", "grep"],
        skip_if_flag_prefix: &["--help", "--version", "-V"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: true,
    },
];

// ============================================================================
// Public iterator over all rules
// ============================================================================

/// All rules concatenated once at startup, in priority order:
/// TEST → BUILD → GIT → LINT → PKG → INFRA → FILE_OPS.
///
/// Using a `LazyLock`-backed `Vec` avoids re-chaining the seven category
/// slices on every call to `all_rules()`, which is invoked on every rewrite
/// attempt (potentially per-token in hook mode).
static ALL_RULES_VEC: LazyLock<Vec<&'static RewriteRule>> = LazyLock::new(|| {
    TEST_RULES
        .iter()
        .chain(BUILD_RULES.iter())
        .chain(GIT_RULES.iter())
        .chain(LINT_RULES.iter())
        .chain(PKG_RULES.iter())
        .chain(INFRA_RULES.iter())
        .chain(FILE_OPS_RULES.iter())
        .collect()
});

/// Iterate over all rewrite rules in priority order: TEST → BUILD → GIT →
/// LINT → PKG → INFRA → FILE_OPS.
///
/// The engine must see longer/more-specific prefixes before shorter ones
/// within the same leading token. Each category array maintains that invariant
/// internally; the chain order between categories does not affect correctness
/// because rules from different categories never share a leading token.
///
/// The return type is `impl Iterator` (not `&[&RewriteRule]`) to keep call sites
/// unchanged while the backing storage is a `LazyLock<Vec<…>>`.
pub(super) fn all_rules() -> impl Iterator<Item = &'static RewriteRule> {
    ALL_RULES_VEC.iter().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected rule count — update this constant together with the category arrays.
    /// TEST(10) + BUILD(4) + GIT(7) + LINT(38) + PKG(18) + INFRA(14) + FILE_OPS(9)
    const EXPECTED_RULE_COUNT: usize = 10 + 4 + 7 + 38 + 18 + 14 + 9;

    #[test]
    fn test_rule_count_matches_expected() {
        let count = all_rules().count();
        assert_eq!(
            count, EXPECTED_RULE_COUNT,
            "Update EXPECTED_RULE_COUNT when adding/removing rules (current: {})",
            count
        );
    }

    // ========================================================================
    // Rule integrity tests (AD-RW-2)
    // ========================================================================

    /// No two rules should share an identical prefix (would cause dead code).
    #[test]
    fn test_no_duplicate_rule_prefixes() {
        let rules: Vec<_> = all_rules().collect();
        for i in 0..rules.len() {
            for j in (i + 1)..rules.len() {
                assert_ne!(
                    rules[i].prefix, rules[j].prefix,
                    "Duplicate prefix found at rule indices {} and {}: {:?}",
                    i, j, rules[i].prefix
                );
            }
        }
    }

    /// A rule's prefix must not be a strict prefix of an earlier rule's prefix
    /// (which would shadow the later rule for all its inputs), EXCEPT for the
    /// intentional catch-all-after-specific pattern.
    ///
    /// Catch-alls (`ls` and `grep`) are placed AFTER their specific counterparts
    /// (`ls -la`, `grep -rn`, etc.), so specific rules still win via first-match.
    #[test]
    fn test_no_rule_shadowing() {
        let rules: Vec<_> = all_rules().collect();
        // Catch-all prefixes are single-token; they intentionally appear after
        // specific prefixes for the same leading token.
        let allowed_catch_alls: &[&[&str]] = &[&["ls"], &["grep"]];

        for i in 0..rules.len() {
            for j in (i + 1)..rules.len() {
                let earlier = rules[i].prefix;
                let later = rules[j].prefix;

                // Skip the allowed catch-all pattern (specific before catch-all).
                if allowed_catch_alls.contains(&later) {
                    continue;
                }

                // Check if earlier is a strict prefix of later (would shadow later).
                if earlier.len() < later.len() && later.starts_with(earlier) {
                    panic!(
                        "Rule {:?} (index {}) shadows rule {:?} (index {}) — \
                         move more-specific rule before less-specific",
                        earlier, i, later, j
                    );
                }
            }
        }
    }

    /// Catch-all ls rule fires for arbitrary ls flags.
    #[test]
    fn test_catch_all_ls_matches_all_flags() {
        use crate::cmd::rewrite::engine::try_rewrite;
        let cases: &[&[&str]] = &[
            &["ls"],
            &["ls", "-1"],
            &["ls", "--color"],
            &["ls", "-lh", "src/"],
        ];
        for tokens in cases {
            let result = try_rewrite(tokens);
            assert!(
                result.is_some(),
                "Expected ls catch-all to fire for: {tokens:?}"
            );
            let r = result.unwrap();
            assert!(
                r.tokens.iter().any(|t| t == "skim"),
                "Expected rewrite to skim for: {tokens:?}"
            );
        }
    }

    /// Specific ls rule wins over catch-all.
    #[test]
    fn test_specific_ls_still_wins() {
        use crate::cmd::rewrite::engine::try_rewrite;
        // `ls -la` matches the specific rule, not the catch-all.
        let tokens: &[&str] = &["ls", "-la"];
        let result = try_rewrite(tokens).expect("should rewrite ls -la");
        // Both specific and catch-all rewrite to skim file ls — result is identical.
        assert!(
            result.tokens.contains(&"skim".to_string()),
            "Expected skim rewrite"
        );
    }

    /// Catch-all grep rule fires for arbitrary grep invocations.
    #[test]
    fn test_catch_all_grep_matches_all_flags() {
        use crate::cmd::rewrite::engine::try_rewrite;
        let cases: &[&[&str]] = &[
            &["grep", "pattern", "file.txt"],
            &["grep", "-i", "foo", "bar.rs"],
            &["grep", "pattern", "file1", "file2", "file3"],
        ];
        for tokens in cases {
            let result = try_rewrite(tokens);
            assert!(
                result.is_some(),
                "Expected grep catch-all to fire for: {tokens:?}"
            );
        }
    }

    /// Specific grep rules win over catch-all.
    #[test]
    fn test_specific_grep_still_wins() {
        use crate::cmd::rewrite::engine::try_rewrite;
        // `grep -rn` matches the specific rule, not the catch-all.
        let tokens: &[&str] = &["grep", "-rn", "pattern", "src/"];
        let result = try_rewrite(tokens).expect("should rewrite grep -rn");
        assert!(result.tokens.contains(&"skim".to_string()));
    }

    /// ls --help skips the catch-all rule (passthrough).
    #[test]
    fn test_ls_help_skip() {
        use crate::cmd::rewrite::engine::try_rewrite;
        assert!(
            try_rewrite(&["ls", "--help"]).is_none(),
            "ls --help should pass through"
        );
    }

    /// ls --version skips the catch-all rule.
    #[test]
    fn test_ls_version_skip() {
        use crate::cmd::rewrite::engine::try_rewrite;
        assert!(
            try_rewrite(&["ls", "--version"]).is_none(),
            "ls --version should pass through"
        );
    }

    /// grep --help skips the catch-all rule.
    #[test]
    fn test_grep_help_skip() {
        use crate::cmd::rewrite::engine::try_rewrite;
        assert!(
            try_rewrite(&["grep", "--help"]).is_none(),
            "grep --help should pass through"
        );
    }
}
