//! Declarative rewrite rule table.
//!
//! 154 rules grouped into 8 category arrays: TEST (18), BUILD (13), GIT (7),
//! LINT (43), PKG (26), INFRA (28), FILE_OPS (16), DB (3).
//! Only `engine.rs` consumes `all_rules()`.
//!
//! v2.8.0: Flat dispatch — `rewrite_to` uses tool names directly
//! (e.g. `["skim", "cargo", "test"]` instead of `["skim", "test", "cargo"]`).
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
// TEST rules (18)
// ============================================================================

const TEST_RULES: &[RewriteRule] = &[
    // cargo (longest prefix first)
    RewriteRule {
        prefix: &["cargo", "nextest", "run"],
        rewrite_to: &["skim", "cargo", "nextest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["cargo", "test"],
        rewrite_to: &["skim", "cargo", "test"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // python (longest prefix first)
    RewriteRule {
        prefix: &["python3", "-m", "pytest"],
        rewrite_to: &["skim", "pytest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["python", "-m", "pytest"],
        rewrite_to: &["skim", "pytest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // npx
    RewriteRule {
        prefix: &["npx", "vitest"],
        rewrite_to: &["skim", "vitest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["npx", "jest"],
        rewrite_to: &["skim", "jest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // bare commands
    RewriteRule {
        prefix: &["pytest"],
        rewrite_to: &["skim", "pytest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["vitest"],
        rewrite_to: &["skim", "vitest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["jest"],
        rewrite_to: &["skim", "jest"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["go", "test"],
        rewrite_to: &["skim", "go", "test"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // playwright (longest prefix first: npx playwright test, playwright test)
    RewriteRule {
        prefix: &["npx", "playwright", "test"],
        rewrite_to: &["skim", "playwright"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["playwright", "test"],
        rewrite_to: &["skim", "playwright"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // cypress (longest prefix first: npx cypress run, cypress run)
    RewriteRule {
        prefix: &["npx", "cypress", "run"],
        rewrite_to: &["skim", "cypress"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["cypress", "run"],
        rewrite_to: &["skim", "cypress"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // npm test/t (longest prefix first — specific aliases before short)
    RewriteRule {
        prefix: &["npm", "test"],
        rewrite_to: &["skim", "npm", "test"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["npm", "t"],
        rewrite_to: &["skim", "npm", "test"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // swift test
    RewriteRule {
        prefix: &["swift", "test"],
        rewrite_to: &["skim", "swift", "test"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // dotnet test
    RewriteRule {
        prefix: &["dotnet", "test"],
        rewrite_to: &["skim", "dotnet", "test"],
        skip_if_flag_prefix: &["--logger"],
        category: RewriteCategory::Test,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
];

// ============================================================================
// BUILD rules (13)
// ============================================================================

const BUILD_RULES: &[RewriteRule] = &[
    RewriteRule {
        prefix: &["cargo", "clippy"],
        rewrite_to: &["skim", "cargo", "clippy"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["cargo", "build"],
        rewrite_to: &["skim", "cargo", "build"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["cargo", "check"],
        rewrite_to: &["skim", "cargo", "check"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // npx
    RewriteRule {
        prefix: &["npx", "tsc"],
        rewrite_to: &["skim", "tsc"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // tsc bare
    RewriteRule {
        prefix: &["tsc"],
        rewrite_to: &["skim", "tsc"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // gmake (GNU make alias, common on BSD systems)
    RewriteRule {
        prefix: &["gmake"],
        rewrite_to: &["skim", "make"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // make bare
    RewriteRule {
        prefix: &["make"],
        rewrite_to: &["skim", "make"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // gradle (longest prefix first: ./gradlew, gradlew, gradle)
    // Verbose flags bypass compression — user wants full debug output
    RewriteRule {
        prefix: &["./gradlew"],
        rewrite_to: &["skim", "gradlew"],
        skip_if_flag_prefix: &["--stacktrace", "--info", "--debug", "--full-stacktrace"],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["gradlew"],
        rewrite_to: &["skim", "gradlew"],
        skip_if_flag_prefix: &["--stacktrace", "--info", "--debug", "--full-stacktrace"],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["gradle"],
        rewrite_to: &["skim", "gradle"],
        skip_if_flag_prefix: &["--stacktrace", "--info", "--debug", "--full-stacktrace"],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // maven (longest prefix first: ./mvnw, mvnw, mvn)
    // -X (debug) and -e (errors+stacktraces) bypass compression
    RewriteRule {
        prefix: &["./mvnw"],
        rewrite_to: &["skim", "mvnw"],
        skip_if_flag_prefix: &["-X", "-e"],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["mvnw"],
        rewrite_to: &["skim", "mvnw"],
        skip_if_flag_prefix: &["-X", "-e"],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["mvn"],
        rewrite_to: &["skim", "mvn"],
        skip_if_flag_prefix: &["-X", "-e"],
        category: RewriteCategory::Build,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
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
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
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
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["git", "fetch"],
        rewrite_to: &["skim", "git", "fetch"],
        skip_if_flag_prefix: &["--dry-run", "-q", "--quiet"],
        category: RewriteCategory::Git,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
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
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
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
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
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
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
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
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
];

// ============================================================================
// LINT rules (43)
// ============================================================================

const LINT_RULES: &[RewriteRule] = &[
    // eslint
    RewriteRule {
        prefix: &["npx", "eslint"],
        rewrite_to: &["skim", "eslint"],
        skip_if_flag_prefix: &["--format", "-f"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["eslint"],
        rewrite_to: &["skim", "eslint"],
        skip_if_flag_prefix: &["--format", "-f"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // ruff (longest prefix first)
    //
    // AD-LINT-20 (2026-04-15): `ruff format --check` and `ruff format` (apply mode)
    // are routed through the format-mode parse path in ruff.rs. The ruff parser
    // detects `is_format_mode` from the first user argument (`"format"`).
    RewriteRule {
        prefix: &["ruff", "format", "--check"],
        rewrite_to: &["skim", "ruff"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["ruff", "format"],
        rewrite_to: &["skim", "ruff"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["ruff", "check"],
        rewrite_to: &["skim", "ruff"],
        skip_if_flag_prefix: &["--output-format"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["ruff"],
        rewrite_to: &["skim", "ruff"],
        skip_if_flag_prefix: &["--output-format"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // mypy (longest prefix first: python3 -m mypy, python -m mypy, mypy)
    RewriteRule {
        prefix: &["python3", "-m", "mypy"],
        rewrite_to: &["skim", "mypy"],
        skip_if_flag_prefix: &["--output"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["python", "-m", "mypy"],
        rewrite_to: &["skim", "mypy"],
        skip_if_flag_prefix: &["--output"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["mypy"],
        rewrite_to: &["skim", "mypy"],
        skip_if_flag_prefix: &["--output"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // golangci-lint
    RewriteRule {
        prefix: &["golangci-lint", "run"],
        rewrite_to: &["skim", "golangci"],
        skip_if_flag_prefix: &["--out-format"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["golangci-lint"],
        rewrite_to: &["skim", "golangci"],
        skip_if_flag_prefix: &["--out-format"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // prettier (longest prefix first: npx prettier, prettier)
    //
    // AD-LINT-20 (2026-04-15): `prettier --write` and `-w` are routed through the
    // format-mode parse path in prettier.rs. `is_format_mode` detects `--write`
    // or `-w` in the user arguments. Check-mode rules unchanged.
    RewriteRule {
        prefix: &["npx", "prettier", "--write"],
        rewrite_to: &["skim", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["npx", "prettier", "-w"],
        rewrite_to: &["skim", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["prettier", "--write"],
        rewrite_to: &["skim", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["prettier", "-w"],
        rewrite_to: &["skim", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["npx", "prettier", "--check"],
        rewrite_to: &["skim", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["prettier", "--check"],
        rewrite_to: &["skim", "prettier"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // rustfmt (longest prefix first)
    RewriteRule {
        prefix: &["cargo", "fmt", "--", "--check"],
        rewrite_to: &["skim", "rustfmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["cargo", "fmt", "--check"],
        rewrite_to: &["skim", "rustfmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["rustfmt", "--check"],
        rewrite_to: &["skim", "rustfmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // cargo fmt (apply mode) — AFTER the longer `cargo fmt --check` and
    // `cargo fmt -- --check` rules above, so shortest prefix fires last.
    // `cargo fmt --check` is ACKed (engine.rs runs ACK before rule table),
    // so this rule only fires for bare `cargo fmt` and `cargo fmt -- [args]`.
    RewriteRule {
        prefix: &["cargo", "fmt"],
        rewrite_to: &["skim", "cargo", "fmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // black
    RewriteRule {
        prefix: &["black", "--check"],
        rewrite_to: &["skim", "black"],
        skip_if_flag_prefix: &["--diff"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["black"],
        rewrite_to: &["skim", "black"],
        skip_if_flag_prefix: &["--diff"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // gofmt (longest prefix first)
    RewriteRule {
        prefix: &["gofmt", "-l"],
        rewrite_to: &["skim", "gofmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["gofmt", "-d"],
        rewrite_to: &["skim", "gofmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["gofmt"],
        rewrite_to: &["skim", "gofmt"],
        skip_if_flag_prefix: &["-w"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // biome (longest prefix first)
    RewriteRule {
        prefix: &["npx", "biome", "check"],
        rewrite_to: &["skim", "biome", "check"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["biome", "check"],
        rewrite_to: &["skim", "biome", "check"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["npx", "biome", "format"],
        rewrite_to: &["skim", "biome", "format"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["biome", "format"],
        rewrite_to: &["skim", "biome", "format"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["npx", "biome", "lint"],
        rewrite_to: &["skim", "biome", "lint"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["biome", "lint"],
        rewrite_to: &["skim", "biome", "lint"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["npx", "biome"],
        rewrite_to: &["skim", "biome"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["biome"],
        rewrite_to: &["skim", "biome"],
        skip_if_flag_prefix: &["--reporter"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // dprint (longest prefix first)
    RewriteRule {
        prefix: &["dprint", "check"],
        rewrite_to: &["skim", "dprint", "check"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["dprint", "fmt"],
        rewrite_to: &["skim", "dprint", "fmt"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["dprint"],
        rewrite_to: &["skim", "dprint"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // oxlint
    RewriteRule {
        prefix: &["npx", "oxlint"],
        rewrite_to: &["skim", "oxlint"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["oxlint"],
        rewrite_to: &["skim", "oxlint"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // rubocop (bundle exec rubocop, rubocop)
    //
    // Skip if user passes --format/-f (explicit format) or -a/-A/--autocorrect
    // (write mode — skim only wraps check/read operations, not auto-fix).
    RewriteRule {
        prefix: &["bundle", "exec", "rubocop"],
        rewrite_to: &["skim", "rubocop"],
        skip_if_flag_prefix: &[
            "--format",
            "-f",
            "-a",
            "-A",
            "--autocorrect",
            "--autocorrect-all",
        ],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["rubocop"],
        rewrite_to: &["skim", "rubocop"],
        skip_if_flag_prefix: &[
            "--format",
            "-f",
            "-a",
            "-A",
            "--autocorrect",
            "--autocorrect-all",
        ],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // swiftlint (longest prefix first: bundle exec swiftlint, swiftlint)
    //
    // Skip if user passes --reporter (explicit reporter) or --fix/--autocorrect
    // (write mode — skim only wraps read operations, not auto-fix).
    RewriteRule {
        prefix: &["bundle", "exec", "swiftlint"],
        rewrite_to: &["skim", "swiftlint"],
        skip_if_flag_prefix: &["--reporter", "--fix", "--autocorrect"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["swiftlint"],
        rewrite_to: &["skim", "swiftlint"],
        skip_if_flag_prefix: &["--reporter", "--fix", "--autocorrect"],
        category: RewriteCategory::Lint,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
];

// ============================================================================
// PKG rules (26)
// ============================================================================

const PKG_RULES: &[RewriteRule] = &[
    // cargo
    RewriteRule {
        prefix: &["cargo", "audit"],
        rewrite_to: &["skim", "cargo", "audit"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // npm (canonical + aliases)
    RewriteRule {
        prefix: &["npm", "run"],
        rewrite_to: &["skim", "npm", "run"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["npm", "run-script"],
        rewrite_to: &["skim", "npm", "run"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["npm", "audit"],
        rewrite_to: &["skim", "npm", "audit"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["npm", "install"],
        rewrite_to: &["skim", "npm", "install"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["npm", "i"],
        rewrite_to: &["skim", "npm", "install"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["npm", "ci"],
        rewrite_to: &["skim", "npm", "install"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["npm", "outdated"],
        rewrite_to: &["skim", "npm", "outdated"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["npm", "list"],
        rewrite_to: &["skim", "npm", "ls"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["npm", "ls"],
        rewrite_to: &["skim", "npm", "ls"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // pnpm
    RewriteRule {
        prefix: &["pnpm", "audit"],
        rewrite_to: &["skim", "pnpm", "audit"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["pnpm", "install"],
        rewrite_to: &["skim", "pnpm", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["pnpm", "i"],
        rewrite_to: &["skim", "pnpm", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["pnpm", "outdated"],
        rewrite_to: &["skim", "pnpm", "outdated"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // pip (canonical + pip3 aliases)
    RewriteRule {
        prefix: &["pip", "install"],
        rewrite_to: &["skim", "pip", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["pip", "check"],
        rewrite_to: &["skim", "pip", "check"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["pip", "list"],
        rewrite_to: &["skim", "pip", "list"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["pip3", "install"],
        rewrite_to: &["skim", "pip", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["pip3", "check"],
        rewrite_to: &["skim", "pip", "check"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["pip3", "list"],
        rewrite_to: &["skim", "pip", "list"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // yarn (longest prefix first: install aliases, audit, outdated, catch-all)
    RewriteRule {
        prefix: &["yarn", "install"],
        rewrite_to: &["skim", "yarn", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["yarn", "add"],
        rewrite_to: &["skim", "yarn", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["yarn", "remove"],
        rewrite_to: &["skim", "yarn", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["yarn", "audit"],
        rewrite_to: &["skim", "yarn", "audit"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["yarn", "outdated"],
        rewrite_to: &["skim", "yarn", "outdated"],
        skip_if_flag_prefix: &["--json"],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // yarn bare — catch-all (dispatches to install handler for bare `yarn` or `yarn i`)
    RewriteRule {
        prefix: &["yarn", "i"],
        rewrite_to: &["skim", "yarn", "install"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Pkg,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
];

// ============================================================================
// INFRA rules (28)
// ============================================================================

/// Docker global value-consuming flags used by all seven docker rules.
///
/// These flags accept the following token as a value (e.g. `--host tcp://...`),
/// so the rewrite engine must skip both the flag and its value when locating
/// the subcommand token.  SEE: DESIGN NOTE (Fix 3) in the rule definitions.
const DOCKER_GLOBAL_FLAGS: &[&str] =
    &["--host", "-H", "--context", "--config", "--log-level", "-l"];

/// Kubectl global value-consuming flags used by all three kubectl rules.
///
/// SEE: DESIGN NOTE (Fix 3) in the rule definitions.
const KUBECTL_GLOBAL_FLAGS: &[&str] = &[
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

/// Output-steering skip flags for gh rules that support `--web`/`-w`.
///
/// Used by the four view/list rules (pr view, pr list, issue view, issue list)
/// whose skip-lists are byte-identical.  Rules with additional per-rule flags
/// (`--watch`, `--log`, `--help`) keep their own inline arrays.
///
/// Parity invariant: wherever `--jq` skips, `-q` also skips; wherever
/// `--template` skips, `-t` also skips; wherever `--web` skips, `-w` also
/// skips.  Enforced by `test_gh_rules_output_flag_parity`.
const GH_OUTPUT_SKIP_FLAGS: &[&str] = &["--web", "-w", "--jq", "-q", "--template", "-t", "--json"];

/// Output-steering skip flags for gh rules that do NOT support `--web`/`-w`.
///
/// Used by `gh run list` and `gh release list`, where `-w` means `--workflow`
/// (a filter flag), NOT `--web`.  Neither `--web` nor `-w` appear in their
/// skip-lists — this is the -w=--workflow safety property.
const GH_OUTPUT_SKIP_FLAGS_NO_WEB: &[&str] = &["--jq", "-q", "--template", "-t", "--json"];

const INFRA_RULES: &[RewriteRule] = &[
    // gh (longest prefix first)
    //
    // DESIGN DECISION: output-steering flags skip the rewrite so gh's raw bytes
    // pass through untouched on the hook path (the handler gate in
    // cmd/infra/gh/mod.rs handles the wrapper/direct path).
    //
    // Output-steering flags: --jq/-q, --template/-t, --json.
    //   --jq/--template apply user-defined projections; injecting --json fields
    //   would change what the filter sees.  --json returns caller-controlled JSON
    //   (no further formatting needed by skim).  Short aliases -q/-t are
    //   equivalent on every command where the long form skips.
    //
    // --web skips on commands that support it (pr list/view, issue list/view,
    //   run view, release view, pr checks) because it opens a browser tab, not
    //   stdout.  Short alias -w skips ONLY where --web skips.
    //   IMPORTANT: gh run list and gh release list do NOT support --web; -w on
    //   those means --workflow (a filter flag), so neither --web nor -w are in
    //   their skip-lists.  This is the -w=-workflow safety property.
    //
    // --log/--log-failed skip for gh run view: they output raw CI step logs.
    // --watch skips because it produces a streaming TUI.
    //
    // gh api has no --json flag (responses are always JSON natively) and no
    //   --web, so those are absent from its skip-list.
    //
    // Parity invariant: wherever --jq skips, -q also skips; wherever
    //   --template skips, -t also skips; wherever --web skips, -w also skips.
    //   Enforced by test_gh_rules_output_flag_parity.
    // GH_OUTPUT_SKIP_FLAGS and GH_OUTPUT_SKIP_FLAGS_NO_WEB named consts reduce the hand-typed
    //   inline flag clusters to single-reference sites; rules with extra flags
    //   (--watch, --log, --help) keep their own inline arrays.
    RewriteRule {
        prefix: &["gh", "pr", "checks"],
        rewrite_to: &["skim", "gh", "pr", "checks"],
        skip_if_flag_prefix: &[
            "--web",
            "-w",
            "--watch",
            "--jq",
            "-q",
            "--template",
            "-t",
            "--json",
        ],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["gh", "pr", "view"],
        rewrite_to: &["skim", "gh", "pr", "view"],
        skip_if_flag_prefix: GH_OUTPUT_SKIP_FLAGS,
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["gh", "pr", "list"],
        rewrite_to: &["skim", "gh", "pr", "list"],
        skip_if_flag_prefix: GH_OUTPUT_SKIP_FLAGS,
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["gh", "issue", "view"],
        rewrite_to: &["skim", "gh", "issue", "view"],
        skip_if_flag_prefix: GH_OUTPUT_SKIP_FLAGS,
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["gh", "issue", "list"],
        rewrite_to: &["skim", "gh", "issue", "list"],
        skip_if_flag_prefix: GH_OUTPUT_SKIP_FLAGS,
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["gh", "run", "view"],
        rewrite_to: &["skim", "gh", "run", "view"],
        skip_if_flag_prefix: &[
            "--web",
            "-w",
            "--log",
            "--log-failed",
            "--jq",
            "-q",
            "--template",
            "-t",
            "--json",
        ],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // gh run watch (B.5) — streaming output compression
    //
    // Routes to the streaming parser (cmd/infra/gh/run_watch.rs).
    // --help skips; --exit-status and --interval pass through to parser.
    // No --json (streaming TUI), no output-steering aliases.
    RewriteRule {
        prefix: &["gh", "run", "watch"],
        rewrite_to: &["skim", "gh", "run", "watch"],
        skip_if_flag_prefix: &["--help"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // gh run list: no --web (so no -w — -w means --workflow here), but does
    // support --jq/-q, --template/-t, and --json.
    RewriteRule {
        prefix: &["gh", "run", "list"],
        rewrite_to: &["skim", "gh", "run", "list"],
        skip_if_flag_prefix: GH_OUTPUT_SKIP_FLAGS_NO_WEB,
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // gh release view (B.6) — structured release metadata
    //
    // Parses release body (capped at MAX_RELEASE_BODY_LINES outside fences),
    // assets (capped at MAX_RELEASE_ASSETS). See AD-RV-1.
    RewriteRule {
        prefix: &["gh", "release", "view"],
        rewrite_to: &["skim", "gh", "release", "view"],
        skip_if_flag_prefix: &[
            "--help",
            "--web",
            "-w",
            "--jq",
            "-q",
            "--template",
            "-t",
            "--json",
        ],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // gh release list: no --web (so no -w), but supports --jq/-q, --template/-t, --json.
    RewriteRule {
        prefix: &["gh", "release", "list"],
        rewrite_to: &["skim", "gh", "release", "list"],
        skip_if_flag_prefix: GH_OUTPUT_SKIP_FLAGS_NO_WEB,
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // gh api (B.4) — REST/GraphQL response compression
    //
    // Compacts JSON responses, handles pagination boundaries, --paginate,
    // base64 content fields, and binary passthrough. See AD-API-1.
    // --help skips; --jq/-q and --template/-t skip (user-defined transform).
    // NO --json: gh api responses are always JSON natively (no --json flag).
    // NO --web/-w: gh api has no browser-open mode.
    RewriteRule {
        prefix: &["gh", "api"],
        rewrite_to: &["skim", "gh", "api"],
        skip_if_flag_prefix: &["--help", "--jq", "-q", "--template", "-t"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // aws
    RewriteRule {
        prefix: &["aws"],
        rewrite_to: &["skim", "aws"],
        skip_if_flag_prefix: &["--output"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // curl
    RewriteRule {
        prefix: &["curl"],
        rewrite_to: &["skim", "curl"],
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
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // dig
    //
    // Skip `+short`, `+yaml`, `+json`, `+trace` — these produce output formats
    // that are already minimal (short), structured for other consumers (yaml/json),
    // or streaming diagnostic (trace). skim only wraps standard dig output.
    RewriteRule {
        prefix: &["dig"],
        rewrite_to: &["skim", "dig"],
        skip_if_flag_prefix: &["+short", "+yaml", "+json", "+trace"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // nslookup
    RewriteRule {
        prefix: &["nslookup"],
        rewrite_to: &["skim", "nslookup"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // wget
    RewriteRule {
        prefix: &["wget"],
        rewrite_to: &["skim", "wget"],
        skip_if_flag_prefix: &["-O", "-q", "--quiet"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // docker compose (3-token prefix first — must precede 2-token docker rules)
    //
    // DESIGN NOTE: 3-token prefix rules listed first so `docker compose ps`
    // matches before the 2-token `docker ps` rule. The engine processes rules
    // in order, so longer prefixes take precedence when listed first.
    //
    // DESIGN NOTE (Fix 3): docker supports global flags between the binary
    // name and the subcommand, e.g. `docker --host tcp://host:2376 ps` or
    // `docker -H unix:///var/run/docker.sock ps`.  `DOCKER_GLOBAL_FLAGS`
    // lists flags that consume the following token so the engine can skip
    // them when matching the subcommand position.
    RewriteRule {
        prefix: &["docker", "compose", "ps"],
        rewrite_to: &["skim", "docker", "compose", "ps"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: DOCKER_GLOBAL_FLAGS,
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["docker", "compose", "logs"],
        rewrite_to: &["skim", "docker", "compose", "logs"],
        skip_if_flag_prefix: &["-f", "--follow"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: DOCKER_GLOBAL_FLAGS,
        require_flag: &[],
    },
    // docker (2-token prefix)
    RewriteRule {
        prefix: &["docker", "ps"],
        rewrite_to: &["skim", "docker", "ps"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: DOCKER_GLOBAL_FLAGS,
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["docker", "images"],
        rewrite_to: &["skim", "docker", "images"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: DOCKER_GLOBAL_FLAGS,
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["docker", "build"],
        rewrite_to: &["skim", "docker", "build"],
        skip_if_flag_prefix: &["--push", "--load"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: DOCKER_GLOBAL_FLAGS,
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["docker", "inspect"],
        rewrite_to: &["skim", "docker", "inspect"],
        skip_if_flag_prefix: &["--format"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: DOCKER_GLOBAL_FLAGS,
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["docker", "logs"],
        rewrite_to: &["skim", "docker", "logs"],
        skip_if_flag_prefix: &["-f", "--follow"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: DOCKER_GLOBAL_FLAGS,
        require_flag: &[],
    },
    // kubectl
    //
    // DESIGN NOTE (Fix 3): kubectl supports global flags between the binary
    // name and the subcommand, e.g. `kubectl -n mynamespace get pods` or
    // `kubectl --context prod get pods`.  `KUBECTL_GLOBAL_FLAGS` lists flags
    // that consume the following token so the engine can skip them to find
    // the real subcommand position.
    RewriteRule {
        prefix: &["kubectl", "get"],
        rewrite_to: &["skim", "kubectl", "get"],
        skip_if_flag_prefix: &["-o", "--output", "-w", "--watch"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: KUBECTL_GLOBAL_FLAGS,
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["kubectl", "describe"],
        rewrite_to: &["skim", "kubectl", "describe"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: KUBECTL_GLOBAL_FLAGS,
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["kubectl", "logs"],
        rewrite_to: &["skim", "kubectl", "logs"],
        skip_if_flag_prefix: &["-f", "--follow"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: KUBECTL_GLOBAL_FLAGS,
        require_flag: &[],
    },
    // terraform
    //
    // DESIGN NOTE (Fix 3): terraform supports `-chdir=<dir>` as a global
    // flag before the subcommand (e.g. `terraform -chdir=infra plan`).
    // Since `-chdir` uses the `=` form exclusively in terraform, it is
    // handled as a bool-flag skip (the `=value` part is attached, so no
    // separate token to consume).  Listed in `global_value_flags` for
    // completeness but the attached-value form is skipped by the
    // `starts_with("--") && contains('=')` guard in the engine.
    RewriteRule {
        prefix: &["terraform", "plan"],
        rewrite_to: &["skim", "terraform", "plan"],
        skip_if_flag_prefix: &["-destroy"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &["-chdir"],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["terraform", "apply"],
        rewrite_to: &["skim", "terraform", "apply"],
        skip_if_flag_prefix: &["-auto-approve", "-destroy"],
        category: RewriteCategory::Infra,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &["-chdir"],
        require_flag: &[],
    },
];

// ============================================================================
// DB rules (3)
// ============================================================================

const DB_RULES: &[RewriteRule] = &[
    // psql: rewrite `psql ... -c "..."` → `skim psql ... -c "..."`
    //
    // DESIGN NOTE (Fix 4): Prefix broadened from `["psql", "-c"]` to just
    // `["psql"]` so that `psql -h host -d mydb -c "SELECT 1"` is captured
    // (the -c flag appears after connection flags, not immediately after psql).
    // The `require_flag` guard ensures the rewrite only fires when `-c` or
    // `--command` is present, preserving the invariant that bare `psql`
    // (interactive sessions) are never rewritten.
    RewriteRule {
        prefix: &["psql"],
        rewrite_to: &["skim", "psql"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Db,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &["-c", "--command"],
    },
    // mysql: rewrite `mysql ... -e "..."` → `skim mysql ... -e "..."`
    //
    // DESIGN NOTE (Fix 4): Prefix broadened from `["mysql", "-e"]` to just
    // `["mysql"]` so that `mysql -h host -u user -e "SELECT 1"` is captured.
    // The `require_flag` guard ensures the rewrite only fires when `-e` or
    // `--execute` is present.
    RewriteRule {
        prefix: &["mysql"],
        rewrite_to: &["skim", "mysql"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::Db,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &["-e", "--execute"],
    },
    // sqlite3: rewrite `sqlite3 db.sqlite "..."` → `skim sqlite3 db.sqlite "..."`
    //
    // Single-token prefix (just `sqlite3`) is intentional and safe for agent
    // contexts.  Unlike psql (requires `-c`) and mysql (requires `-e`), sqlite3
    // has no mandatory batch-mode flag: it enters batch mode simply when stdin
    // is not a TTY.  In agent contexts (Claude Code, Cursor, Codex, etc.) the
    // hook always runs with piped stdin — sqlite3 reads EOF immediately and exits
    // without prompting.  This means `sqlite3 mydb.sqlite` through the rewrite
    // hook is non-interactive even with only a db-file argument.
    //
    // Explicit `-interactive` flag is still excluded as a defensive guard for
    // any invocation that forces interactive mode regardless of stdin state.
    RewriteRule {
        prefix: &["sqlite3"],
        rewrite_to: &["skim", "sqlite3"],
        skip_if_flag_prefix: &["-interactive"],
        category: RewriteCategory::Db,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
];

// ============================================================================
// FILE_OPS rules (16)
// ============================================================================

const FILE_OPS_RULES: &[RewriteRule] = &[
    // find — pipe-source excluded so `find . | head` is not rewritten (AD-RW-2)
    RewriteRule {
        prefix: &["find"],
        rewrite_to: &["skim", "find"],
        skip_if_flag_prefix: &["-exec", "-delete", "-printf", "-print0"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: true,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // ls (verbose/recursive only)
    RewriteRule {
        prefix: &["ls", "-la"],
        rewrite_to: &["skim", "ls"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["ls", "-R"],
        rewrite_to: &["skim", "ls"],
        skip_if_flag_prefix: &[],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // tree
    RewriteRule {
        prefix: &["tree"],
        rewrite_to: &["skim", "tree"],
        skip_if_flag_prefix: &["-J", "--json"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // grep (recursive only)
    RewriteRule {
        prefix: &["grep", "-rn"],
        rewrite_to: &["skim", "grep"],
        skip_if_flag_prefix: &["-c", "--count", "-l"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    RewriteRule {
        prefix: &["grep", "-r"],
        rewrite_to: &["skim", "grep"],
        skip_if_flag_prefix: &["-c", "--count", "-l"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // rg — pipe-source excluded so `rg pat | head` is not rewritten (AD-RW-2)
    RewriteRule {
        prefix: &["rg"],
        rewrite_to: &["skim", "rg"],
        skip_if_flag_prefix: &["--json", "-c", "--count", "-l", "--files"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: true,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // wc
    RewriteRule {
        prefix: &["wc"],
        rewrite_to: &["skim", "wc"],
        skip_if_flag_prefix: &["--help", "--version"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: true,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // du
    RewriteRule {
        prefix: &["du"],
        rewrite_to: &["skim", "du"],
        skip_if_flag_prefix: &["--help", "--version", "-0", "--null"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: true,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // df
    RewriteRule {
        prefix: &["df"],
        rewrite_to: &["skim", "df"],
        skip_if_flag_prefix: &["--help", "--version"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: false,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // ps
    RewriteRule {
        prefix: &["ps"],
        rewrite_to: &["skim", "ps"],
        skip_if_flag_prefix: &["--help", "--version"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: true,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // env
    RewriteRule {
        prefix: &["env"],
        rewrite_to: &["skim", "env"],
        skip_if_flag_prefix: &["--help", "--version", "-i", "-u", "-S"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: true,
        // `env VAR=val cmd` passes VAR=val to the child process; rewriting
        // would route to `skim env` which only handles printenv-style output.
        // Bare `env` (no `=` args) is still rewritten.  SEE: issue batch-b.
        skip_if_middle_contains_eq: true,
        global_value_flags: &[],
        require_flag: &[],
    },
    // printenv
    RewriteRule {
        prefix: &["printenv"],
        rewrite_to: &["skim", "printenv"],
        skip_if_flag_prefix: &["--help", "--version"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: true,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
    // diff
    RewriteRule {
        prefix: &["diff"],
        rewrite_to: &["skim", "diff"],
        skip_if_flag_prefix: &[
            "--help",
            "--version",
            "-y",
            "--side-by-side",
            "-q",
            "--brief",
            "-e",
            "--ed",
        ],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: true,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
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
        rewrite_to: &["skim", "ls"],
        skip_if_flag_prefix: &["--help", "--version", "-V"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: true,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
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
        rewrite_to: &["skim", "grep"],
        skip_if_flag_prefix: &["--help", "--version", "-V"],
        category: RewriteCategory::FileOps,
        exclude_pipe_source: true,
        skip_if_middle_contains_eq: false,
        global_value_flags: &[],
        require_flag: &[],
    },
];

// ============================================================================
// Public iterator over all rules
// ============================================================================

/// All rules concatenated once at startup, in priority order:
/// TEST → BUILD → GIT → LINT → PKG → INFRA → FILE_OPS → DB.
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
        .chain(DB_RULES.iter())
        .collect()
});

/// Iterate over all rewrite rules in priority order: TEST → BUILD → GIT →
/// LINT → PKG → INFRA → FILE_OPS → DB.
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
    /// TEST(18) + BUILD(13) + GIT(7) + LINT(43) + PKG(26) + INFRA(28) + FILE_OPS(16) + DB(3)
    const EXPECTED_RULE_COUNT: usize = 18 + 13 + 7 + 43 + 26 + 28 + 16 + 3;

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
    // gh output-flag parity test (PF-007)
    // ========================================================================

    /// Enforce that short aliases and --json membership are consistent across
    /// all gh rules:
    ///   - wherever --jq skips, -q must also skip
    ///   - wherever --template skips, -t must also skip
    ///   - wherever --web skips, -w must also skip
    ///   - --json must be present on every gh rule EXCEPT ["gh","api"] and
    ///     ["gh","run","watch"] (which are exempt by design)
    ///
    /// This prevents silent regressions where the long form is added to a rule
    /// but the short alias is forgotten, restoring the original bug.
    /// Each assertion is discriminating: a test that only checked `is_some()`
    /// on the rule list would pass even with a missing alias (avoids PF-007).
    #[test]
    fn test_gh_rules_output_flag_parity() {
        const ALIASES: &[(&str, &str)] = &[("--jq", "-q"), ("--template", "-t"), ("--web", "-w")];
        for rule in all_rules() {
            if rule.prefix.first() != Some(&"gh") {
                continue;
            }
            for (long, short) in ALIASES {
                if rule.skip_if_flag_prefix.contains(long) {
                    assert!(
                        rule.skip_if_flag_prefix.contains(short),
                        "gh rule {:?} skips {long} but not alias {short}",
                        rule.prefix
                    );
                }
            }
            let exempt = rule.prefix == ["gh", "api"] || rule.prefix == ["gh", "run", "watch"];
            assert_eq!(
                rule.skip_if_flag_prefix.contains(&"--json"),
                !exempt,
                "gh rule {:?} wrong --json membership (exempt={exempt})",
                rule.prefix
            );
        }
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
        // Both specific and catch-all rewrite to skim ls — result is identical.
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
