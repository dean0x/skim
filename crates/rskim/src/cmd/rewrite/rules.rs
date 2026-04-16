//! Declarative rewrite rule table.
//!
//! 93 rules grouped into 7 category arrays: TEST (10), BUILD (4), GIT (5),
//! LINT (38), PKG (18), INFRA (11), FILE_OPS (7).
//! Only `engine.rs` consumes `all_rules()`.

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
    },
    RewriteRule {
        prefix: &["cargo", "test"],
        rewrite_to: &["skim", "test", "cargo"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
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
    },
    RewriteRule {
        prefix: &["cargo", "build"],
        rewrite_to: &["skim", "build", "cargo"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
    },
    // npx
    RewriteRule {
        prefix: &["npx", "tsc"],
        rewrite_to: &["skim", "build", "tsc"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
    },
    // tsc bare
    RewriteRule {
        prefix: &["tsc"],
        rewrite_to: &["skim", "build", "tsc"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
    },
];

// ============================================================================
// GIT rules (5)
// ============================================================================

const GIT_RULES: &[RewriteRule] = &[
    RewriteRule {
        prefix: &["git", "status"],
        rewrite_to: &["skim", "git", "status"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Git,
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
    },
    RewriteRule {
        prefix: &["git", "fetch"],
        rewrite_to: &["skim", "git", "fetch"],
        skip_if_flag_prefix: &["--dry-run", "-q", "--quiet"],
        category: RewriteCategory::Git,
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
    },
    RewriteRule {
        prefix: &["eslint"],
        rewrite_to: &["skim", "lint", "eslint"],
        skip_if_flag_prefix: &["--format", "-f"],
        category: RewriteCategory::Lint,
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
    },
    RewriteRule {
        prefix: &["ruff", "format"],
        rewrite_to: &["skim", "lint", "ruff"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
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
    // mypy (longest prefix first: python3 -m mypy, python -m mypy, mypy)
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
    // golangci-lint
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
    },
    RewriteRule {
        prefix: &["npx", "prettier", "-w"],
        rewrite_to: &["skim", "lint", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["prettier", "--write"],
        rewrite_to: &["skim", "lint", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["prettier", "-w"],
        rewrite_to: &["skim", "lint", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
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
    // rustfmt (longest prefix first)
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
    // black
    RewriteRule {
        prefix: &["black", "--check"],
        rewrite_to: &["skim", "lint", "black"],
        skip_if_flag_prefix: &["--diff"],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["black"],
        rewrite_to: &["skim", "lint", "black"],
        skip_if_flag_prefix: &["--diff"],
        category: RewriteCategory::Lint,
    },
    // gofmt (longest prefix first)
    RewriteRule {
        prefix: &["gofmt", "-l"],
        rewrite_to: &["skim", "lint", "gofmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["gofmt", "-d"],
        rewrite_to: &["skim", "lint", "gofmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["gofmt"],
        rewrite_to: &["skim", "lint", "gofmt"],
        skip_if_flag_prefix: &["-w"],
        category: RewriteCategory::Lint,
    },
    // biome (longest prefix first)
    RewriteRule {
        prefix: &["npx", "biome", "check"],
        rewrite_to: &["skim", "lint", "biome", "check"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["biome", "check"],
        rewrite_to: &["skim", "lint", "biome", "check"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["npx", "biome", "format"],
        rewrite_to: &["skim", "lint", "biome", "format"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["biome", "format"],
        rewrite_to: &["skim", "lint", "biome", "format"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["npx", "biome", "lint"],
        rewrite_to: &["skim", "lint", "biome", "lint"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["biome", "lint"],
        rewrite_to: &["skim", "lint", "biome", "lint"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["npx", "biome"],
        rewrite_to: &["skim", "lint", "biome"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["biome"],
        rewrite_to: &["skim", "lint", "biome"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
    },
    // dprint (longest prefix first)
    RewriteRule {
        prefix: &["dprint", "check"],
        rewrite_to: &["skim", "lint", "dprint", "check"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["dprint", "fmt"],
        rewrite_to: &["skim", "lint", "dprint", "fmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["dprint"],
        rewrite_to: &["skim", "lint", "dprint"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
    },
    // oxlint
    RewriteRule {
        prefix: &["npx", "oxlint"],
        rewrite_to: &["skim", "lint", "oxlint"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Lint,
    },
    RewriteRule {
        prefix: &["oxlint"],
        rewrite_to: &["skim", "lint", "oxlint"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Lint,
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
    },
    // npm (canonical + aliases)
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
    // pnpm
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
    // pip (canonical + pip3 aliases)
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
];

// ============================================================================
// INFRA rules (11)
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
    // aws
    RewriteRule {
        prefix: &["aws"],
        rewrite_to: &["skim", "infra", "aws"],
        skip_if_flag_prefix: &["--output"],
        category: RewriteCategory::Infra,
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
    },
    // wget
    RewriteRule {
        prefix: &["wget"],
        rewrite_to: &["skim", "infra", "wget"],
        skip_if_flag_prefix: &["-O", "-q", "--quiet"],
        category: RewriteCategory::Infra,
    },
];

// ============================================================================
// FILE_OPS rules (7)
// ============================================================================

const FILE_OPS_RULES: &[RewriteRule] = &[
    // find
    RewriteRule {
        prefix: &["find"],
        rewrite_to: &["skim", "file", "find"],
        skip_if_flag_prefix: &["-exec", "-delete", "-printf", "-print0"],
        category: RewriteCategory::FileOps,
    },
    // ls (verbose/recursive only)
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
    // tree
    RewriteRule {
        prefix: &["tree"],
        rewrite_to: &["skim", "file", "tree"],
        skip_if_flag_prefix: &["-J", "--json"],
        category: RewriteCategory::FileOps,
    },
    // grep (recursive only)
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
    // rg
    RewriteRule {
        prefix: &["rg"],
        rewrite_to: &["skim", "file", "rg"],
        skip_if_flag_prefix: &["--json", "-c", "--count", "-l", "--files"],
        category: RewriteCategory::FileOps,
    },
];

// ============================================================================
// Public iterator over all rules
// ============================================================================

/// Iterate over all rewrite rules in priority order: TEST → BUILD → GIT →
/// LINT → PKG → INFRA → FILE_OPS.
///
/// The engine must see longer/more-specific prefixes before shorter ones
/// within the same leading token. Each category array maintains that invariant
/// internally; the chain order between categories does not affect correctness
/// because rules from different categories never share a leading token.
pub(super) fn all_rules() -> impl Iterator<Item = &'static RewriteRule> {
    TEST_RULES
        .iter()
        .chain(BUILD_RULES.iter())
        .chain(GIT_RULES.iter())
        .chain(LINT_RULES.iter())
        .chain(PKG_RULES.iter())
        .chain(INFRA_RULES.iter())
        .chain(FILE_OPS_RULES.iter())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected rule count — update this constant together with the category arrays.
    const EXPECTED_RULE_COUNT: usize = 10 + 4 + 5 + 38 + 18 + 11 + 7;

    #[test]
    fn test_rule_count_matches_expected() {
        let count = all_rules().count();
        assert_eq!(
            count, EXPECTED_RULE_COUNT,
            "Update EXPECTED_RULE_COUNT when adding/removing rules (current: {})",
            count
        );
    }
}
