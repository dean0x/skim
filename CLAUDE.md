# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**Skim** is a streaming code reader for AI agents, written in Rust on tree-sitter. It strips implementation detail while preserving structure, signatures, and types to optimize code for LLM context windows. It also compresses other agent context: test output, build errors, lint output, git diffs, logs, and raw shell commands.

**Key principle:** Skim is a *streaming reader* (`cat` but smart), not a file compressor. Output always goes to stdout for pipe workflows — never write intermediate files.

User-facing install/usage lives in `README.md`; release mechanics in `CHANGELOG.md`. This file is for working *in* the repo.

## Workspace

Cargo workspace, 8 crates:
- `rskim-core` — pure transform library (parsing, modes; no I/O side effects)
- `rskim` — CLI binary (`skim`): caching, analytics, command wrappers
- `rskim-search` — code-search index (lexical n-gram, temporal, AST structural), stored in `<root>/.skim/search.db`
- `rskim-research` — offline tooling that generates both AST structural weight tables
  AND the lexical trigram IDF weight table (see codegen notes below)
- `rskim-bench` — benchmarks
- `rskim-tokens` — offline + optional-network token counting (multi-provider; `net-anthropic` feature gates HTTP)
- `rskim-contract` — byte-faithful contract / guardrail layer for transcript mutation
- `rskim-llm` — LLM transcript parsing (OpenAI/Anthropic) + classifier

`crates/rskim-search/src/ast_weights.rs` is **auto-generated — do not edit**. Regenerate via `rskim-research ast-run` then `ast-codegen`.

`crates/rskim-search/src/weights.rs` is **auto-generated — do not edit**. It contains the lexical trigram IDF weight table (`TRIGRAM_WEIGHTS`, `lookup_weight`, `trigram_weight`). Regenerate via `rskim-research trigram-run` then `trigram-codegen`. The old `rskim-research codegen` subcommand (bigram-based) now writes to a separate `bigram_weights_legacy.rs` artifact and must NOT be used for the live trigram table.

## Architecture

```
Parser Manager (language detection)
  ↓
Language::transform_source()          ← Strategy Pattern dispatcher
  ├─ tree-sitter  (14 code langs: TS/JS/Python/Rust/Go/Java/C/C++/C#/Ruby/SQL/Kotlin/Swift/Markdown)
  └─ serde-based  (JSON/YAML/TOML — data formats, not code)
  ↓
Transformation Layer (modes: structure / signatures / types / minimal / pseudo / full)
  ↓
Streaming output (stdout, zero-copy via &str slices where possible)
```

`transform_source()` routes each language to its parser via the Strategy Pattern, avoiding special-case conditionals — each language encapsulates its own strategy.

**Non-obvious behavior (gotchas):**
- **Analytics:** token savings persist to `~/.cache/skim/analytics.db` (SQLite/WAL), recorded fire-and-forget on background threads. `--clear-cache` clears only the parser cache, NOT `analytics.db` — use `skim stats --clear` for that. The `AnalyticsStore` trait + `MockStore` make the stats dashboard testable without a real DB.
- **Search DB:** `rskim-search` stores hotspot/risk/co-change data in `<root>/.skim/search.db`. Migrations are forward-only via `PRAGMA user_version`; a DB written by a newer version errors rather than corrupting data.
- **Lexical n-gram index:** `index.skidx` / `index.skpost` is format v4 (trigram, u32 key from #355 Part B + delta+varint variable-length posting codec from #358 Item 2). This is DISTINCT from the AST structural index. Both v2 (bigram, u16 key) and v3 (trigram, u32 key, fixed 9-byte postings) lexical indexes trigger an automatic rebuild on the next query via `check_staleness` — no manual `--rebuild` needed for either upgrade. Short queries (< 3 bytes, e.g. `fn`, `if`) cannot produce trigrams and fall back to an all-files score-0 candidate set, which is filtered down to matching files by the Part A substring-verify gate (AD-355-7); this is correct behavior, not a bug.
- **AST index:** the structural n-gram index (`ast_index.skidx` / `.skpost`) is format v2 — v1 files are rejected with "please rebuild" (`skim search index --rebuild`). Synthetic n-gram markers (IDs ≥ 64900) resolve to `None` in `vocab_resolve()`, keeping them isolated from real vocabulary.

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

- **Scope cargo per-crate** (`-p <crate>`). Never `--workspace` or `--all-features` *inside an agent* — those fan out across all 8 crates and their heavy deps simultaneously.
- **Never `cargo test -p rskim` in an agent**: it spawns a *nested* cargo (daemon meta-tests) on top of subprocess-spawning E2E tests. Use `cargo test -p rskim --bins` / `--all-targets` (see the scoping note above).
- **Prefer `cargo nextest run -p <crate> -j 4`** for unit/integration tests, **plus `cargo test -p <crate> --doc`** for doctests (nextest cannot run doctests).
- **Never run two release/LTO builds concurrently**, and never kick off a heavy build in both clones at the same time.
- **Defer the full `--all-features` regression** to the main loop or a human, run when the machine is otherwise idle.

Modes are set via `--mode` only (no config file): `structure` (default), `signatures`, `types`, `minimal`, `pseudo`, `full`.

### Subcommands

Most subcommands wrap a dev tool (cargo, git, npm, pytest, eslint, docker, psql, grep, …) and compress its output — run `skim --help` for the full catalog. The ones with non-obvious behavior:

- `search` — n-gram code search over a project index. Build/update: `skim search index` (`--rebuild`, `--force`, `--root`, `--max-files`). Query: `skim search <text>` (`--limit`, `--json`, `--stats`). Temporal sort/filter: `--hot`/`--cold` (hotspot score), `--risky` (fix-risk), `--blast-radius FILE` (co-change peers). Structural: `--ast <pattern>` — a named pattern (`try-catch`, `nested-loop`, `god-function`, …) or containment query (`for_statement > block`); composable with a text query, `--hot`/`--cold`/`--risky`, `--blast-radius`, `--limit`, and `--json`; degrades gracefully when heatmap data is absent (warns to stderr, returns unsorted, exit 0). Limitation: single-node queries (no `>` separator) are rejected (#283, unigram index not yet built). Composite ranking: `--weights lexical,ast,temporal` (default `0.5,0.3,0.2`, ratios only — not normalized, zero and non-sum-to-1 allowed, negative/NaN/inf rejected) tunes the `--blast-radius` RRF ranking (#200).
- `heatmap` — git-history risk/coupling analysis: churn, co-change, stability, fix-after-touch (`--json`, `--since`, `--window`, `--path`, `--insights`).
- `init` — install skim as an agent hook (Claude/Cursor/Codex/Gemini/Copilot/Crush); `--wrappers` adds PATH wrappers for sub-agent interception.
- `stats` — token analytics dashboard (`--since`, `--format json`, `--verbose`, `--clear`).
- `discover` / `learn` / `rewrite` — scan agent sessions for missed optimizations, learn error-retry correction rules, and rewrite commands into skim equivalents.

### Two interception surfaces (they work differently — don't conflate them)

skim intercepts a sub-agent's shell command through **two independent mechanisms**, and only one of them rewrites anything. Confusing them produces false coverage claims (e.g. "flag preservation verified on both surfaces" — it can't be; see below).

1. **Rewrite engine** — the PreToolUse hook and the `skim rewrite` CLI. Operates on the command *as text, before it runs*: `cmd/rewrite/` `try_rewrite()` transforms the string `grep -rn x` → `skim grep -rn x`. This is the **only** surface where flag preservation (Fix A — don't drop `-rn` during the rewrite), corruption-bail (Fix C), and pipe-source passthrough (Fix E) exist — they are properties of the *text transformation*.

2. **PATH wrappers** — `skim init --wrappers` symlinks `~/.skim/bin/<tool>` → the skim binary (with `~/.skim/bin` first on `PATH`) so sub-agent shells route through skim even when they bypass PreToolUse hooks. Here skim *is* the tool: the OS runs the binary with `argv[0]=<tool>`, `main()` calls `strip_skim_wrappers_from_path()` as its very first statement (before any thread spawns, so the real tool is found and recursion is impossible), then `detect_argv0_dispatch()` routes straight to `cmd::dispatch(tool, args)` — **`try_rewrite` is never called**. Flags arrive as ordinary argv and pass to the handler unchanged; there is no rewrite step to "preserve" them through. `SKIM_PASSTHROUGH=1` is the escape hatch. Wrapper install/uninstall only ever touches symlinks whose target stem is `skim`/`rskim` — never regular files.

**Testing / verification implication:** the two surfaces share the per-tool *handlers* (output compression) but NOT the dispatch front-end. A test that drives the `--hook`/`rewrite` path does **not** exercise the wrapper path, and vice-versa. When verifying behavior — and when confirming Snyk/CI actually cover a change — identify *which* surface a test hits and cover both where the behavior could diverge. Rewrite-engine guarantees (flag preservation, corruption-bail, pipe passthrough) simply do not apply to the wrapper surface.

## Environment Variables

- `SKIM_PASSTHROUGH=1` — bypass all compression (use when compressed output hides an error). Indefinite commands (`vite dev`, `jest --watch`, bare `skim vitest`) auto-pass-through live; use `skim vitest run` for a compressed one-shot.
- `SKIM_DEBUG=1` (or `--debug`) — warnings/notices on stderr.
- `SKIM_SESSION_ID` — analytics session attribution; priority `--session-id` > sidecar > env > none. Set it alongside the PATH export so sub-agents inherit it.
- `SKIM_CACHE_DIR` / `SKIM_ANALYTICS_DB` — override the cache dir / analytics DB path.
- `SKIM_DISABLE_ANALYTICS=1` — disable recording. `SKIM_INPUT_COST_PER_MTOK` — $/MTok for cost estimates (default 3.0).
- Session-provider overrides for `discover`/`learn`/`agents`: `SKIM_PROJECTS_DIR`, `SKIM_CODEX_SESSIONS_DIR`, `SKIM_COPILOT_DIR`, `SKIM_CURSOR_DB_PATH`, `SKIM_GEMINI_DIR`, `SKIM_CRUSH_DIR`.

## Design Constraints

**MUST:** stream to stdout (never write intermediate files) · prefer `&str` slices over allocation in the hot path · tolerate incomplete code (rely on tree-sitter error nodes) · stay under 50ms for 1000-line files (benchmark regressions block) · fail loud with actionable messages, never silently · modes via CLI flags only, no `.skimrc` · **compress, never truncate** (#317): wrappers may re-encode output but never show less than the raw tool; an unavoidable safety bound must use `output::elision_marker` (exact counts + `SKIM_PASSTHROUGH=1` hint); unexpected non-zero exits forward raw output instead of compressing; rewrites must reconstruct the command byte-faithfully or bail (never emit a command that errors or changes semantics).

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
