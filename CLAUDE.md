# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Skim** is a streaming code reader for AI agents, written in Rust on tree-sitter. It strips implementation detail while preserving structure, signatures, and types to optimize code for LLM context windows. It also compresses other agent context: test output, build errors, lint output, git diffs, logs, and raw shell commands.

**Key principle:** Skim is a *streaming reader* (`cat` but smart), not a file compressor. Output always goes to stdout for pipe workflows — never write intermediate files.

User-facing install/usage lives in `README.md`; release mechanics in `CHANGELOG.md`. This file is for working *in* the repo.

## Workspace

Cargo workspace, 10 crates:
- `rskim-core` — pure transform library (parsing, modes; no I/O side effects)
- `rskim` — CLI binary (`skim`): caching, analytics, command wrappers
- `rskim-proxy` — HTTP reverse proxy foundation for Layer-3 LLM request routing (hyper + tokio; isolated from default builds)
- `rskim-search` — code-search index (lexical n-gram, temporal, AST structural), stored in `<root>/.skim/search.db`
- `rskim-research` — offline tooling that generates AST weight tables
- `rskim-bench` — benchmarks
- `rskim-tokens` — offline + optional-network token counting (multi-provider; `net-anthropic` feature gates HTTP)
- `rskim-contract` — byte-faithful contract / guardrail layer for transcript mutation
- `rskim-llm` — LLM transcript parsing (OpenAI/Anthropic) + classifier
- `rskim-compress` — per-content-type block compression router for the Layer-3 proxy (BlockRouter, log compression)

`crates/rskim-search/src/ast_weights.rs` is **auto-generated — do not edit**. Regenerate via `rskim-research ast-run` then `ast-codegen`.

## Architecture

```
Parser Manager (language detection)
  ↓
Language::transform_source()          ← Strategy Pattern dispatcher
  ├─ tree-sitter  (15 code langs: TS/JS/Python/Rust/Go/Java/C/C++/C#/Ruby/SQL/Kotlin/Swift/Bash/Markdown)
  └─ serde-based  (JSON/YAML/TOML — data formats, not code)
  ↓
Transformation Layer (modes: structure / signatures / types / minimal / pseudo / full)
  ↓
Streaming output (stdout, zero-copy via &str slices where possible)
```

`transform_source()` routes each language to its parser via the Strategy Pattern, avoiding special-case conditionals — each language encapsulates its own strategy.

**Non-obvious behavior (gotchas):**
- **Analytics:** token savings persist to `~/.cache/skim/analytics.db` (SQLite/WAL; default location — relocates with `SKIM_CACHE_DIR`, see Environment Variables), recorded fire-and-forget on background threads. `--clear-cache` clears only the parser cache, NOT `analytics.db` — use `skim stats --clear` for that. The `AnalyticsStore` trait + `MockStore` make the stats dashboard testable without a real DB.
- **Search DB:** `rskim-search` stores hotspot/risk/co-change data in `<root>/.skim/search.db`. Migrations are forward-only via `PRAGMA user_version`; a DB written by a newer version errors rather than corrupting data.
- **AST index:** the n-gram index (`ast_index.skidx` / `.skpost`) is format v2 — v1 files are rejected with "please rebuild" (`skim search index --rebuild`). Synthetic n-gram markers (IDs ≥ 64900) resolve to `None` in `vocab_resolve()`, keeping them isolated from real vocabulary.

## Commands

To test changes in **this** clone, invoke its own build by path — `./target/release/skim` (refresh with `cargo build --release`). ⚠️ A bare `skim` on `$PATH` may resolve to a *different* local clone (this machine keeps parallel clones to avoid worktree churn), so it can silently exercise the wrong code.

```bash
cargo build --release          # production build
cargo test --all-features      # full test suite
cargo clippy -- -D warnings    # lint (warnings are errors)
cargo fmt -- --check           # format check
cargo bench                    # criterion benchmarks
cargo run --bin skim -- file.ts --mode=signatures   # run locally
```

`rskim` is bin-only (the `skim` binary; no `src/lib.rs`) — scope its tests with `cargo test -p rskim --bins` (or `--all-targets`). `cargo test -p rskim --lib` errors with "no library targets found" (a cargo target-selection behavior, not a skim bug). `rskim-core`/`rskim-search` are libraries and accept `--lib`.

### Build/test resource limits

A machine-global `~/.cargo/config.toml` caps every cargo invocation at `jobs = 4` and `RUST_TEST_THREADS = 4`, and routes compilation through `sccache` (a compile cache shared across parallel clones). **That config file is the enforcement layer — it protects every branch and clone regardless of this doc; the guidance below exists because the cap alone can still be multiplied by parallelism.** Running unbounded parallel builds across two clones once exhausted 64 GB RAM (heavy tree-sitter/SQLite/rustls deps + release LTO/`codegen-units=1`) and hard-restarted the machine. The root multiplier was **two clones with separate `target/` dirs compiling identical heavy deps at once**. Rules for agents and workflows:

- **Scope cargo per-crate** (`-p <crate>`). Never `--workspace` or `--all-features` *inside an agent* — those fan out across all 10 crates and their heavy deps simultaneously.
- **Never `cargo test -p rskim` in an agent**: it spawns a *nested* cargo (the real-cargo E2E tests in `tests/cli_test_cargo.rs`, `tests/cli_build.rs`, `tests/cli_e2e_build_parsers.rs`, `tests/cli_e2e_pkg_parsers.rs`) on top of subprocess-spawning E2E tests. Use `cargo test -p rskim --bins` / `--all-targets` (see the scoping note above).
- **Prefer `cargo nextest run -p <crate> -j 4`** for unit/integration tests, **plus `cargo test -p <crate> --doc`** for doctests (nextest cannot run doctests).
- **Never run two release/LTO builds concurrently**, and never kick off a heavy build in both clones at the same time.
- **Defer the full `--all-features` regression** to the main loop or a human, run when the machine is otherwise idle.

Modes are set via `--mode` only (no config file): `structure` (default), `signatures`, `types`, `minimal`, `pseudo`, `full`.

### Bounded output (`--max-lines`, `--last-lines`, `--tokens`)

`--max-lines N` emits at most N lines total, elision marker included — this is what the rewrite hook turns `head -N` into, so a bound the tool can exceed is not a bound (ADR-016). The sole exception is N=1: one content line plus the marker (2 lines), because spending the only slot on the marker returns a view with no code, and silently dropping the marker is the defect this flag exists to prevent. `--last-lines N` is the tail mirror: at most N lines total including the leading marker. For `--tokens N`, the cascade escalates modes until the output fits; on tight budgets the exact elided count appears on stdout (compact marker) and `SKIM_PASSTHROUGH=1` remedy is printed unconditionally to stderr per ADR-011 class 1. The rewrite hook maps `head -N` / `tail -N` on code files to `--mode=full --max-lines N` / `--last-lines N` (verbatim slice), while `cat` stays on pseudo/structure (ADR-007); bare `head`/`tail` get the POSIX default bound of 10; signed counts (`+N`, `-N`) are never rewritten.

### Subcommands

Most subcommands wrap a dev tool (cargo, git, npm, pytest, eslint, docker, psql, grep, …) and compress its output — run `skim --help` for the full catalog. The ones with non-obvious behavior:

- `search` — n-gram code search over a project index. Build/update: `skim search index` (`--rebuild`, `--force`, `--root`, `--max-files`, `--index-dir`); routes to build only when trailing args match the build grammar — bare `skim search index` still builds (backward-compatible), but with query flags or extra positional terms it searches for the literal "index" (`skim search -- index` forces a search via POSIX `--`). Query: `skim search <text>` (`--limit`, `--json`, `--stats`). Temporal sort/filter: `--hot`/`--cold` (hotspot score), `--risky` (fix-risk), `--blast-radius FILE` (co-change peers). Structural: `--ast <pattern>` — a named pattern (`try-catch`, `nested-loop`, `god-function`, …) or containment query (`for_statement > block`); composable with text query and `--blast-radius`. `--ast` with temporal flags, or single-node queries, errors out (#202 / #283).
- `heatmap` — git-history risk/coupling analysis: churn, co-change, stability, fix-after-touch (`--json`, `--since`, `--window`, `--path`, `--insights`).
- `init` — install skim as an agent hook (Claude/Cursor/Codex/Gemini/Copilot/Crush); `--wrappers` adds PATH wrappers for sub-agent interception; `--permissions` seeds consent-gated allowlist entries (tiers: seed|mirror|blanket).
- `stats` — token analytics dashboard (`--since`, `--format json`, `--verbose`, `--clear`).
- `doctor` — provenance check: reports the running binary (path + commit), every `skim` on `$PATH` with its commit and which one wins, hook pin state per agent (pinned binary path vs running binary path; a path mismatch is advisory `⚠` and does not cause exit `1`), wrapper directory, and cache/analytics locations. Exit `0` healthy / `1` on version mismatch, commit mismatch, tampered script, or unreadable script — works as a CI pre-flight. Commit resolution reads `--version` output rather than a `--commit` flag, so it correctly identifies binaries that predate `skim doctor` itself.
- `discover` / `learn` / `rewrite` — scan agent sessions for missed optimizations, learn error-retry correction rules, and rewrite commands into skim equivalents.

### Two interception surfaces (they work differently — don't conflate them)

skim intercepts a sub-agent's shell command through **two independent mechanisms**, and only one of them rewrites anything. Confusing them produces false coverage claims (e.g. "flag preservation verified on both surfaces" — it can't be; see below).

1. **Rewrite engine** — the PreToolUse hook and the `skim rewrite` CLI. Operates on the command *as text, before it runs*: `cmd/rewrite/` `try_rewrite()` transforms the string `grep -rn x` → `skim grep -rn x`. This is the **only** surface where flag preservation (Fix A — don't drop `-rn` during the rewrite), corruption-bail (Fix C), and pipe-source passthrough (Fix E) exist — they are properties of the *text transformation*.

2. **PATH wrappers** — `skim init --wrappers` symlinks `~/.skim/bin/<tool>` → the skim binary (with `~/.skim/bin` first on `PATH`) so sub-agent shells route through skim even when they bypass PreToolUse hooks. Here skim *is* the tool: the OS runs the binary with `argv[0]=<tool>`, `main()` calls `strip_skim_wrappers_from_path()` as its very first statement (before any thread spawns, so the real tool is found and recursion is impossible), then `detect_argv0_dispatch()` returns the tool name and args; `main()` interposes a fidelity gate (#370): `stdout_should_serve_raw()` compresses **iff fd 1 is a terminal (`isatty`) or a FIFO**, and bails to `cmd::run_inherited_passthrough` for every other sink — regular files, non-terminal character devices (`/dev/null`), sockets — so raw bytes reach them unmodified (#317); otherwise it calls `cmd::dispatch_for_wrapper(tool, args)`. **`try_rewrite` is never called**. Flags arrive as ordinary argv and pass to the handler unchanged; there is no rewrite step to "preserve" them through. `SKIM_PASSTHROUGH=1` is the escape hatch. Wrapper install/uninstall only ever touches symlinks whose target stem is `skim`/`rskim` — never regular files.

**Which surface decides what (stdout destination).** Each surface can observe something the other structurally cannot, so neither is authoritative alone:

- **`fstat` is ground truth, and wins wherever it can see.** It knows a regular file / char device / socket after the shell has already wired fd 1 up, when no redirect token remains in argv. `isatty(1)` — not `FileType::is_char_device()` — is what identifies a terminal; using the file-type bit as an `isatty` proxy misclassifies `/dev/null`, `/dev/zero` and `/dev/random` as terminals and compresses into them.
- **A pipe is ambiguous at the fd level**, and this is the one gap `fstat` cannot close: `| cat` (compress — skim's core value) and `| tee out.txt` / `$(…)` (compress = data loss) are the same FIFO. Only the rewrite engine sees pipeline *shape*. It records its verdict via `command_needs_exact_bytes` (`cmd/rewrite/compound.rs`) in a force-raw sidecar marker, which the wrapper discovers by ancestry walk (`session_sidecar::read_force_raw`). The marker is set *or cleared* by every hook invocation that reaches command extraction, so it never outlives a command the hook actually processed; five early exits (passthrough mode, AwarenessOnly agents, stdin read error, JSON parse error, missing command field) skip the write, and a marker left behind by one of them lives until the next processed command or the 300 s reap. **The key is `{ppid}.{tool}.raw` — PID *and* tool name.** PPID alone is not a command identity: every command an agent runs shares that PID, so a PPID-only key made one command's verdict decide unrelated concurrent, background, and nested-sub-agent wrapper invocations (and let their clears delete a live marker). The tool component comes from `command_heads`, the command heads the hook saw; a shape that defeats head extraction (`$(…)`, backticks, process substitution) falls back to the wildcard `{ppid}.raw`, which matches every tool. **Accepted limitations:** the marker exists only when the hook fires — a bare wrapper invocation with no PreToolUse hook gets `fstat`-only behaviour; and two *same-tool* commands under one agent still share a key, so a concurrent `git status` can clear a live `git log | tee f` marker. Both fail toward compression, and for a FIFO wired to a byte-exact consumer (`| tee f`, `| sha256sum`) that is byte loss, not a lossless fallback: measured 304 bytes written instead of 6803, silently. The same-tool clear is a narrowed remnant of the pre-marker behaviour; the hook-less case is pinned by `no_hook_means_fstat_only_behaviour`.
- **The rewrite engine's text scan is a hint, not an authority.** `stdout_redirected_to_file` tracks fd state left-to-right so `cmd 2>f >&2` (stderr to a file, then fd 1 dup'd from it) is recognised as a stdout→file redirect. The explicit-subcommand path (`skim git log > out.txt`) is deliberately NOT `fstat`-gated: the user typed `skim` there, and that path cannot tell a user-authored `skim …` from a hook-injected one.

**Testing / verification implication:** the two surfaces share the per-tool *handlers* (output compression) but NOT the dispatch front-end. A test that drives the `--hook`/`rewrite` path does **not** exercise the wrapper path, and vice-versa. When verifying behavior — and when confirming Snyk/CI actually cover a change — identify *which* surface a test hits and cover both where the behavior could diverge. The rewrite *text-transformation* guarantees (flag preservation, text-scan corruption-bail, pipe-source passthrough) are rewrite-engine-only and do not apply to the wrapper surface. The stdout-destination guarantee, however, exists on *both* surfaces via distinct mechanisms: the rewrite engine uses `stdout_redirected_to_file` / `command_needs_exact_bytes` (a text scan before exec); the wrapper uses `stdout_should_serve_raw` (`fstat` + `isatty` on fd 1 after the shell has already redirected) plus the force-raw marker. Do not conclude that wrappers have no output-fidelity protection (#370). The full 9-destination × 2-surface matrix is pinned in `crates/rskim/tests/cli_stdout_destination.rs`.

## Environment Variables

- `SKIM_PASSTHROUGH=1` (or `--passthrough` CLI flag) — bypass all compression and exec the real tool with raw argv. The `--passthrough` flag strips skim-only flags (`--json` for git, `--mode`, `--show-stats`) from the forwarded argv so the real tool never sees flags it does not understand. Indefinite commands (`vite dev`, `jest --watch`, bare `skim vitest`) auto-pass-through live; use `skim vitest run` for a compressed one-shot.
- `SKIM_DEBUG=1` (or `--debug`) — enables raw-fallback diagnostic banners on stderr for no-loss raw-fallback paths (see **Stderr notice taxonomy** in Design Constraints below; loss-bearing elision markers and the ADR-008 transparency marker are unconditional and not gated by this variable). In hook mode the startup provenance line goes to `hook.log`, never stderr (GRANITE #361 Bug 3); drift events are also logged to `hook.log` unconditionally — `skim doctor` is the primary on-demand diagnosis path.
- `SKIM_SESSION_ID` — analytics session attribution; priority sidecar > env > `--session-id` flag (flag is a forward-compat fallback only — the hook no longer injects it). Set it alongside the PATH export so sub-agents inherit it.
- `SKIM_CACHE_DIR` — relocates **all** skim cache state: parser cache (`.json` files),
  tee output (`tee/`), and the **default** `analytics.db` location. An empty value is
  treated as unset (falls back to `~/.cache/skim`). The path is used as-is (no `skim`
  suffix is appended by the resolver). **Caveat:** pre-existing analytics history at the
  old `~/.cache/skim/analytics.db` is **not migrated** — setting this variable for the
  first time causes `skim stats` to start from an empty DB at the new location; move
  the old file manually if you want to preserve history.
- `SKIM_ANALYTICS_DB` — overrides the analytics DB path directly; **takes precedence over
  `SKIM_CACHE_DIR`** for the DB location. When `SKIM_ANALYTICS_DB` is set, the DB is
  opened at that exact path regardless of `SKIM_CACHE_DIR`. To isolate all skim state
  in a sandbox it is sufficient to set `SKIM_CACHE_DIR` alone (the default analytics.db
  moves with it).
- `SKIM_DISABLE_ANALYTICS=1` — disable recording. `SKIM_INPUT_COST_PER_MTOK` — $/MTok for cost estimates (default 3.0).
- `SKIM_WRAPPERS_DIR` — overrides the `~/.skim/bin/` wrapper symlink directory used by `skim init --wrappers` and `skim init --uninstall`. Primarily used in tests via `skim_sandboxed()` to redirect wrapper installation into a TempDir sandbox so real `~/.skim/bin/` is not touched. An empty value is treated as unset (falls back to `~/.skim/bin`).
- Session-provider overrides for `discover`/`learn`/`agents`: `SKIM_PROJECTS_DIR`, `SKIM_CODEX_SESSIONS_DIR`, `SKIM_COPILOT_DIR`, `SKIM_CURSOR_DB_PATH`, `SKIM_GEMINI_DIR`, `SKIM_CRUSH_DIR`.
- **Agent config-dir overrides** (read by `skim init` / `init --uninstall` / `doctor` — these are the agents' *own* variable names, not `SKIM_`-prefixed): `CLAUDE_CONFIG_DIR`, `GEMINI_CONFIG_DIR`, `COPILOT_CONFIG_DIR`, `CODEX_HOME`, `CRUSH_CONFIG_DIR`. Each redirects that agent's hook script, settings file, permissions sidecar, **and guidance file** (`GEMINI.md`, `copilot-instructions.md`, …) away from the `~/.<agent>/` default. Do not confuse them with the `SKIM_GEMINI_DIR` / `SKIM_COPILOT_DIR` session-provider overrides above, which point at transcript directories and have no effect on install/uninstall. Any test that shells out to `skim init`/`--uninstall`/`doctor` must set them (use `common::skim_sandboxed`) or it will mutate the developer's real home directory.

## Design Constraints

**MUST:** stream to stdout (never write intermediate files) · prefer `&str` slices over allocation in the hot path · tolerate incomplete code (rely on tree-sitter error nodes) · stay under 50ms for 1000-line files (benchmark regressions block) · fail loud with actionable messages, never silently · modes via CLI flags only, no `.skimrc` · **compress, never truncate** (#317): wrappers may re-encode output but never show less than the raw tool; an unavoidable safety bound must use `output::elision_marker` (exact counts + `SKIM_PASSTHROUGH=1` hint); unexpected non-zero exits forward raw output instead of compressing; rewrites must reconstruct the command byte-faithfully or bail (never emit a command that errors or changes semantics). **git diff enhancement view must fit within the raw budget** — any enriched render (e.g. hunk-scoped AST breadcrumbs) is guarded by an ADR-001 net-savings check; if enrichment expands the output beyond the raw diff size, raw is emitted instead (git-diff raw-budget decision reversal: the unguarded enhancement view was replaced by a guardrail-protected one).

**Stderr notice taxonomy (ADR-011):** Every new `stderr` notice must be classified before it is added — (1) **loss-bearing markers** (elision markers with exact counts + `SKIM_PASSTHROUGH=1` hint; ADR-008 lossy-view transparency marker) fire when the reader sees less/different from raw and are **unconditional**; (2) **no-loss raw-fallback banners** (guardrail chose raw, unexpected exit, tool killed) fire only on lossless paths and are **gated behind `SKIM_DEBUG`/`--debug`** (`crate::debug_log!` / `io::sink()`). If a notice can fire when the reader sees less/different from raw, it is a marker (always on); if it fires only on a lossless raw fallback, it is a banner (debug-gated). Do not re-conflate the two classes.

**MUST NOT:** add syntax highlighting (use `bat`), linting (use linters), type checking (use `tsc`/`mypy`), or LSP features — all out of scope.

**Targets:** parse+transform <50ms/1000 lines · 60–80% token reduction (structure mode) · <10ms startup · <1s for 100 files (parallel via rayon).

**Exit codes:** `0` success · `1` general error · `2` parse error · `3` unsupported language.

## Adding a Language

**tree-sitter language:** add the `tree-sitter-<lang>` dep at the workspace version, then a match arm in `to_tree_sitter()`. ~30 min.

**Data format (non-tree-sitter, like JSON/YAML/TOML):**
1. Add a `Language` variant in `rskim-core/src/types.rs`; return `None` from `to_tree_sitter()` and from the `get_*_node_types()` functions.
2. Implement a transform module (`src/transform/<fmt>.rs`) with security limits (max depth, max keys).
3. Route it in `Language::transform_source()` (Strategy Pattern).
4. Add the variant to `LanguageArg` in `crates/rskim/src/main.rs`.

## Testing

Fixtures live in `tests/fixtures/<language>/`, ≥4 per language. Integration targets: ≥95% parse success on real-world code, output still parses, 60–80% token reduction.

**Known edge cases:** incomplete code → tree-sitter error nodes · files >100MB → error (memmap is future work) · binary files → detect and reject · stdin supported (`cat file.ts | skim`).

## Release

Run `./scripts/release-prep.sh <version>` (pre-flight checks + mechanical version bumps). You still create the `release/vX.Y.Z` branch and write the CHANGELOG entry by hand. The version lives in `crates/rskim-core/Cargo.toml` and `crates/rskim/Cargo.toml` (plus the `rskim-core` dependency version) — all MUST equal the tag exactly or the build job fails. Pushing tag `vX.Y.Z` triggers `.github/workflows/release.yml`: test → build (7 targets) → GitHub Release → crates.io (`rskim-core` then `rskim`) → npm → Homebrew tap.
