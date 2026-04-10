//! Declarative rewrite rule table.
//!
//! 69 rules, ordered longest-prefix-first within the same leading token.
//! Only `engine.rs` consumes `REWRITE_RULES`.

use super::types::{RewriteCategory, RewriteRule};

pub(super) const REWRITE_RULES: &[RewriteRule] = &[
    // cargo (longest prefix first)
    RewriteRule {
        prefix: &["cargo", "nextest", "run"],
        rewrite_to: &["skim", "test", "cargo"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    RewriteRule {
        prefix: &["cargo", "test"],
        rewrite_to: &["skim", "test", "cargo"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    RewriteRule {
        prefix: &["cargo", "audit"],
        rewrite_to: &["skim", "pkg", "cargo", "audit"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
    },
    RewriteRule {
        prefix: &["cargo", "clippy"],
        rewrite_to: &["skim", "build", "clippy"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
    },
    RewriteRule {
        prefix: &["cargo", "build"],
        rewrite_to: &["skim", "build", "cargo"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
    },
    // python (longest prefix first)
    RewriteRule {
        prefix: &["python3", "-m", "pytest"],
        rewrite_to: &["skim", "test", "pytest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    RewriteRule {
        prefix: &["python", "-m", "pytest"],
        rewrite_to: &["skim", "test", "pytest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    // npx
    RewriteRule {
        prefix: &["npx", "vitest"],
        rewrite_to: &["skim", "test", "vitest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    RewriteRule {
        prefix: &["npx", "jest"],
        rewrite_to: &["skim", "test", "jest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    RewriteRule {
        prefix: &["npx", "tsc"],
        rewrite_to: &["skim", "build", "tsc"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
    },
    // bare commands
    RewriteRule {
        prefix: &["pytest"],
        rewrite_to: &["skim", "test", "pytest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    RewriteRule {
        prefix: &["vitest"],
        rewrite_to: &["skim", "test", "vitest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    RewriteRule {
        prefix: &["jest"],
        rewrite_to: &["skim", "test", "jest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    RewriteRule {
        prefix: &["go", "test"],
        rewrite_to: &["skim", "test", "go"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
    },
    // git
    RewriteRule {
        prefix: &["git", "status"],
        rewrite_to: &["skim", "git", "status"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Git,
    },
    // DESIGN NOTE (AD-4): `--stat`, `--name-only` removed from skip list.
    // These are Group B flags (already-compact output). Removing them allows
    // `git diff --stat` and `git diff --name-only` to flow through to the
    // handler's passthrough branch. The handler's `user_has_flag` check
    // (diff/mod.rs) still catches these and calls `run_passthrough`, so
    // output is byte-identical to raw git. This also fixes the `--staged`
    // collision (previously eaten by loose `--stat` prefix matching).
    RewriteRule {
        prefix: &["git", "diff"],
        rewrite_to: &["skim", "git", "diff"],
        skip_if_flag_prefix: &["--shortstat", "--numstat", "--name-status", "--check"],
        category: RewriteCategory::Git,
    },
    RewriteRule {
        prefix: &["git", "fetch"],
        rewrite_to: &["skim", "git", "fetch"],
        skip_if_flag_prefix: &["--dry-run", "-q", "--quiet"],
        category: RewriteCategory::Git,
    },
    // DESIGN NOTE (AD-4): `--format` and `--pretty` removed from skip list.
    // The log handler (log.rs) already detects these flags and calls
    // `run_passthrough`, so users see raw git output. Removing them from
    // the skip list means the rewrite rule fires and the handler decides.
    RewriteRule {
        prefix: &["git", "log"],
        rewrite_to: &["skim", "git", "log"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Git,
    },
    // git show — new rule (AD-5)
    //
    // Handles `git show <hash>`, `git show <hash>:<path>`, and defaults.
    // The handler (cmd/git/show.rs) dispatches to commit-mode or
    // file-content-mode based on argument shape.
    RewriteRule {
        prefix: &["git", "show"],
        rewrite_to: &["skim", "git", "show"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Git,
    },
    // tsc bare
    RewriteRule {
        prefix: &["tsc"],
        rewrite_to: &["skim", "build", "tsc"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
    },
    // lint — eslint
    RewriteRule {
        prefix: &["npx", "eslint"],
        rewrite_to: &["skim", "lint", "eslint"],
        skip_if_flag_prefix: &["--format", "-f"],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["eslint"],
        rewrite_to: &["skim", "lint", "eslint"],
        skip_if_flag_prefix: &["--format", "-f"],
        category: RewriteCategory::Lint,
    },
    // lint — ruff
    RewriteRule {
        prefix: &["ruff", "check"],
        rewrite_to: &["skim", "lint", "ruff"],
        skip_if_flag_prefix: &["--output-format"],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["ruff"],
        rewrite_to: &["skim", "lint", "ruff"],
        skip_if_flag_prefix: &["--output-format"],
        category: RewriteCategory::Lint,
    },
    // lint — mypy (longest prefix first: python3 -m mypy, python -m mypy, mypy)
    RewriteRule {
        prefix: &["python3", "-m", "mypy"],
        rewrite_to: &["skim", "lint", "mypy"],
        skip_if_flag_prefix: &["--output"],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["python", "-m", "mypy"],
        rewrite_to: &["skim", "lint", "mypy"],
        skip_if_flag_prefix: &["--output"],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["mypy"],
        rewrite_to: &["skim", "lint", "mypy"],
        skip_if_flag_prefix: &["--output"],
        category: RewriteCategory::Lint,
    },
    // lint — golangci-lint
    RewriteRule {
        prefix: &["golangci-lint", "run"],
        rewrite_to: &["skim", "lint", "golangci"],
        skip_if_flag_prefix: &["--out-format"],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["golangci-lint"],
        rewrite_to: &["skim", "lint", "golangci"],
        skip_if_flag_prefix: &["--out-format"],
        category: RewriteCategory::Lint,
    },
    // pkg — npm (canonical + aliases)
    RewriteRule {
        prefix: &["npm", "audit"],
        rewrite_to: &["skim", "pkg", "npm", "audit"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
    },
    RewriteRule {
        prefix: &["npm", "install"],
        rewrite_to: &["skim", "pkg", "npm", "install"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
    },
    RewriteRule {
        prefix: &["npm", "i"],
        rewrite_to: &["skim", "pkg", "npm", "install"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
    },
    RewriteRule {
        prefix: &["npm", "ci"],
        rewrite_to: &["skim", "pkg", "npm", "install"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
    },
    RewriteRule {
        prefix: &["npm", "outdated"],
        rewrite_to: &["skim", "pkg", "npm", "outdated"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
    },
    RewriteRule {
        prefix: &["npm", "list"],
        rewrite_to: &["skim", "pkg", "npm", "ls"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
    },
    RewriteRule {
        prefix: &["npm", "ls"],
        rewrite_to: &["skim", "pkg", "npm", "ls"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
    },
    // pkg — pnpm
    RewriteRule {
        prefix: &["pnpm", "audit"],
        rewrite_to: &["skim", "pkg", "pnpm", "audit"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
    },
    RewriteRule {
        prefix: &["pnpm", "install"],
        rewrite_to: &["skim", "pkg", "pnpm", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
    },
    RewriteRule {
        prefix: &["pnpm", "i"],
        rewrite_to: &["skim", "pkg", "pnpm", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
    },
    RewriteRule {
        prefix: &["pnpm", "outdated"],
        rewrite_to: &["skim", "pkg", "pnpm", "outdated"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Pkg,
    },
    // pkg — pip (canonical + pip3 aliases)
    RewriteRule {
        prefix: &["pip", "install"],
        rewrite_to: &["skim", "pkg", "pip", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
    },
    RewriteRule {
        prefix: &["pip", "check"],
        rewrite_to: &["skim", "pkg", "pip", "check"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
    },
    RewriteRule {
        prefix: &["pip", "list"],
        rewrite_to: &["skim", "pkg", "pip", "list"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Pkg,
    },
    RewriteRule {
        prefix: &["pip3", "install"],
        rewrite_to: &["skim", "pkg", "pip", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
    },
    RewriteRule {
        prefix: &["pip3", "check"],
        rewrite_to: &["skim", "pkg", "pip", "check"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
    },
    RewriteRule {
        prefix: &["pip3", "list"],
        rewrite_to: &["skim", "pkg", "pip", "list"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Pkg,
    },
    // lint — prettier (longest prefix first: npx prettier, prettier)
    RewriteRule {
        prefix: &["npx", "prettier", "--check"],
        rewrite_to: &["skim", "lint", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["prettier", "--check"],
        rewrite_to: &["skim", "lint", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
    // lint — rustfmt (longest prefix first)
    RewriteRule {
        prefix: &["cargo", "fmt", "--", "--check"],
        rewrite_to: &["skim", "lint", "rustfmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["cargo", "fmt", "--check"],
        rewrite_to: &["skim", "lint", "rustfmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["rustfmt", "--check"],
        rewrite_to: &["skim", "lint", "rustfmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
    // infra — gh (longest prefix first)
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
    },
    RewriteRule {
        prefix: &["gh", "pr", "view"],
        rewrite_to: &["skim", "infra", "gh", "pr", "view"],
        skip_if_flag_prefix: &["--web", "--jq", "--template"],
        category: RewriteCategory::Infra,
    },
    RewriteRule {
        prefix: &["gh", "pr", "list"],
        rewrite_to: &["skim", "infra", "gh", "pr", "list"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Infra,
    },
    RewriteRule {
        prefix: &["gh", "issue", "view"],
        rewrite_to: &["skim", "infra", "gh", "issue", "view"],
        skip_if_flag_prefix: &["--web", "--jq", "--template"],
        category: RewriteCategory::Infra,
    },
    RewriteRule {
        prefix: &["gh", "issue", "list"],
        rewrite_to: &["skim", "infra", "gh", "issue", "list"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Infra,
    },
    RewriteRule {
        prefix: &["gh", "run", "view"],
        rewrite_to: &["skim", "infra", "gh", "run", "view"],
        skip_if_flag_prefix: &["--web", "--log", "--log-failed", "--jq", "--template"],
        category: RewriteCategory::Infra,
    },
    RewriteRule {
        prefix: &["gh", "run", "list"],
        rewrite_to: &["skim", "infra", "gh", "run", "list"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Infra,
    },
    RewriteRule {
        prefix: &["gh", "release", "list"],
        rewrite_to: &["skim", "infra", "gh", "release", "list"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Infra,
    },
    // infra — aws
    RewriteRule {
        prefix: &["aws"],
        rewrite_to: &["skim", "infra", "aws"],
        skip_if_flag_prefix: &["--output"],
        category: RewriteCategory::Infra,
    },
    // infra — curl
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
    },
    // infra — wget
    RewriteRule {
        prefix: &["wget"],
        rewrite_to: &["skim", "infra", "wget"],
        skip_if_flag_prefix: &["-O", "-q", "--quiet"],
        category: RewriteCategory::Infra,
    },
    // file — find
    RewriteRule {
        prefix: &["find"],
        rewrite_to: &["skim", "file", "find"],
        skip_if_flag_prefix: &["-exec", "-delete", "-printf", "-print0"],
        category: RewriteCategory::FileOps,
    },
    // file — ls (verbose/recursive only)
    RewriteRule {
        prefix: &["ls", "-la"],
        rewrite_to: &["skim", "file", "ls"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::FileOps,
    },
    RewriteRule {
        prefix: &["ls", "-R"],
        rewrite_to: &["skim", "file", "ls"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::FileOps,
    },
    // file — tree
    RewriteRule {
        prefix: &["tree"],
        rewrite_to: &["skim", "file", "tree"],
        skip_if_flag_prefix: &["-J", "--json"],
        category: RewriteCategory::FileOps,
    },
    // file — grep (recursive only)
    RewriteRule {
        prefix: &["grep", "-rn"],
        rewrite_to: &["skim", "file", "grep"],
        skip_if_flag_prefix: &["-c", "--count", "-l"],
        category: RewriteCategory::FileOps,
    },
    RewriteRule {
        prefix: &["grep", "-r"],
        rewrite_to: &["skim", "file", "grep"],
        skip_if_flag_prefix: &["-c", "--count", "-l"],
        category: RewriteCategory::FileOps,
    },
    // file — rg
    RewriteRule {
        prefix: &["rg"],
        rewrite_to: &["skim", "file", "rg"],
        skip_if_flag_prefix: &["--json", "-c", "--count", "-l", "--files"],
        category: RewriteCategory::FileOps,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected rule count — update this constant together with REWRITE_RULES.
    const EXPECTED_RULE_COUNT: usize = 69;

    #[test]
    fn test_rule_count_matches_expected() {
        assert_eq!(
            REWRITE_RULES.len(),
            EXPECTED_RULE_COUNT,
            "Update EXPECTED_RULE_COUNT when adding/removing rules (current: {})",
            REWRITE_RULES.len()
        );
    }
}
