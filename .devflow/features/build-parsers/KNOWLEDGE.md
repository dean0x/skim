---
feature: build-parsers
name: Build Tool Output Parsers
description: "Use when adding a new build tool parser, modifying cargo/tsc/make/gradle/maven compression, or debugging three-tier parse degradation for build commands. Keywords: build, cargo, tsc, make, gradle, maven, ParseResult, BuildResult, three-tier, NDJSON, flag injection, flat dispatch, multi-category dispatch, cmd refactor, dispatch.rs, execution.rs, registry.rs, security.rs."
category: domain-knowledge
directories: [crates/rskim/src/cmd/build/]
referencedFiles:
  - crates/rskim/src/cmd/build/mod.rs
  - crates/rskim/src/cmd/build/cargo.rs
  - crates/rskim/src/cmd/build/tsc.rs
  - crates/rskim/src/cmd/build/make.rs
  - crates/rskim/src/cmd/build/gradle.rs
  - crates/rskim/src/cmd/build/maven.rs
  - crates/rskim/src/output/mod.rs
  - crates/rskim/src/output/canonical.rs
  - crates/rskim/src/cmd/mod.rs
  - crates/rskim/src/runner.rs
created: 2026-06-03
updated: 2026-06-04
---

# Build Tool Output Parsers

## Overview

The `cmd/build/` module compresses and filters output from build tools (cargo, make, tsc, gradle,
maven) using a three-tier parse degradation strategy. It is reached via two dispatch paths:

- **Flat dispatch**: `skim tsc`, `skim gradle`, `skim make` — the tool name is `argv[0]` when
  invoked as a PATH wrapper
- **Multi-category dispatch**: `skim cargo build`, `skim cargo clippy`, `skim gradle build` —
  routed through `cmd/build/mod.rs::run()`

All handlers share the same `ParseResult<BuildResult>` output contract and delegate to
`CommandRunner` for process execution.

## cmd/mod.rs Architecture (post-refactor, PR #267)

`cmd/mod.rs` was split from a 2,070-line monolith into five focused submodules. The refactor is
transparent to all consumers: every `pub(crate)` symbol is re-exported from `mod.rs` unchanged.

| Module | Lines | Responsibility |
|--------|-------|----------------|
| `mod.rs` | ~420 | Module declarations, re-exports, shared stdin/flag parsing utilities |
| `dispatch.rs` | — | Multi-category dispatcher (`dispatch`, `run_raw_passthrough`); strict vs. passthrough models |
| `execution.rs` | — | `run_parsed_command_with_mode`, `run_tool`, `OutputFormat`, `ParsedCommandConfig`, `RunContext`, `ToolRunConfig` |
| `registry.rs` | ~288 | `KNOWN_SUBCOMMANDS`, `is_known_subcommand`, `is_meta_subcommand`, `wrapper_targets` |
| `security.rs` | ~494 | `scrub_db_args`, `scrub_infra_args`, `sanitize_for_display`, `TokenAction` enum; credential scrubbing |
| `test_support.rs` | — | Test helpers shared across subcommand test suites |

**Strict vs. passthrough dispatcher model** (documented in `dispatch.rs`):
- `cargo`, `go` — strict: unknown subcommands print an error and return `ExitCode::FAILURE`.
  These tools have finite, well-defined subcommand sets skim covers comprehensively.
- `swift`, `dotnet` — passthrough: unknown subcommands are forwarded verbatim. These tools expose
  a wide lifecycle surface; blocking unknown subcommands would make skim unusable.

## Three-Tier Parse Degradation

The `output::ParseResult<T>` enum drives the degradation strategy:

```
Full(T)               — clean structured parse, all fields populated
Degraded(T, warnings) — partial parse, forwarded with warning markers injected
Passthrough(String)   — unrecognized format, output returned as-is
```

Each build handler attempts `Full` parse first, degrades to `Degraded` on partial failures, and
falls back to `Passthrough` only when the output format is entirely unrecognized. The degradation
level is used by the analytics recording layer to track parse quality.

## Component Architecture

### `mod.rs` — Public Dispatch

Routes incoming args to the correct handler. Handles `--help` early exit. Extracts `--show-stats`
flag via `cmd::extract_show_stats`. Match arms cover:
- `"build"` / `"check"` / `"fmt"` / `"clippy"` / `"nextest"` / `"audit"` → `cargo::run_*`
- `"gradle"` / `"gradlew"` → `gradle::run`
- `"make"` → `make::run`
- `"mvn"` / `"mvnw"` / `"maven"` → `maven::run`
- Unknown subcommand → strict dispatch (error + failure exit)

### `cargo.rs` — Rust Toolchain

Handles cargo build, check, fmt, clippy, nextest, and audit via dedicated entry points:
- `run()` — cargo build: injects `--message-format=json`, parses NDJSON
- `run_check()` — cargo check: same NDJSON path as build, reuses `parse()`
- `run_fmt()` — cargo fmt: separate `parse_fmt()` path (no NDJSON; captures plain text diff/ok)
- `run_clippy()` — injects `--message-format json-diagnostic-rendered-ansi` (space-separated form)

All routes share `run_with_json_format()` for the NDJSON path. Collapses duplicate warning lines,
extracts error/warning counts for the stats footer.

### `gradle.rs` — Gradle / Gradlew

Handles gradle and gradlew invocations. Extracts task names from gradle's structured output lines
(`:taskName OUTCOME`), filters build lifecycle noise, preserves error output verbatim.

### `make.rs` — GNU Make

Handles make invocations. Extracts target names, filters `Entering directory`/`Leaving directory`
lines, preserves compiler error output verbatim.

### `maven.rs` — Maven / mvnw

Handles mvn and mvnw invocations. Filters reactor summary lines, extracts module names, preserves
BUILD SUCCESS/FAILURE and error output.

### `tsc.rs` — TypeScript Compiler

Handles tsc invocations. Parses TypeScript diagnostic output (`file.ts(line,col): error TS1234:
message`), groups by file, deduplicates identical messages.

### `output/mod.rs` — ParseResult + Infrastructure

Provides:
- `ParseResult<T>` enum (Full / Degraded / Passthrough)
- ANSI stripping (`strip_ansi_codes`)
- Progress line collapsing (cargo download progress, percentage lines)
- Token-aware truncation
- Filter transparency headers

### `output/canonical.rs` — BuildResult

`BuildResult` is the canonical structured form of build output. Fields capture: error count,
warning count, duration, individual diagnostics, and the raw output for passthrough. All handlers
produce `ParseResult<BuildResult>`.

### `runner.rs` — CommandRunner

Timeout-aware command runner. Executes via `Command::new().args()` (no shell). Captures stdout +
stderr concurrently via threads to prevent pipe deadlocks. Exposes `CommandOutput { stdout, stderr,
exit_code, duration }`. `CommandRunner` is dependency-injected into all handlers.

## Integration Points

- **Analytics**: every handler receives a `RecordingContext` and calls `analytics::record_build_result`
  on completion. Fire-and-forget background thread.
- **PATH wrappers**: when `skim` binary detects `argv[0] == "gradle"` etc., it strips `~/.skim/bin`
  from `PATH` then calls `cmd/build/mod.rs::run()` with the tool name prepended to args.
- **`--show-stats`**: handlers print a token-reduction stats footer to stderr when present.
- **npm test/run**: `skim npm test` and `skim npm run <script>` detect the project's test framework
  via `cmd/pkg/npm/script_tool.rs::ScriptTool` and delegate to the appropriate handler (vitest,
  jest, etc.). Not part of `cmd/build/` but shares the same `ParseResult<BuildResult>` contract.

## Anti-Patterns

- **Shell-expanding arguments**: `CommandRunner` uses `Command::new().args()`, not a shell. Never
  pass shell metacharacters in args — they are passed literally.
- **Calling `cargo::run` for non-cargo subcommands**: the `mod.rs` dispatch now uses strict mode —
  unknown subcommands return an error. Add explicit arms for any new tools.
- **Logging parse failures to stdout**: degradation warnings go through `ParseResult::Degraded` and
  are rendered as comment-style markers in the output, not raw stderr noise.
- **Bypassing `dispatch.rs` to add a new multi-category dispatcher**: new dispatchers belong in
  `dispatch.rs` with an explicit decision on strict vs. passthrough model, documented in the
  dispatcher behavioral models comment block at the top of that file.

## Gotchas

- `cargo clippy` uses a different `--message-format` flag form than `cargo build`. The injected
  flag is `--message-format json-diagnostic-rendered-ansi` (space-separated), not `=` form.
- `cargo check` reuses `run_with_json_format()` and `parse()` from the build path — no separate
  parse function is needed since the NDJSON format is identical.
- `cargo fmt` has its own `parse_fmt()` because fmt output is plain text (changed file paths or
  empty on success), not NDJSON.
- `gradle` and `gradlew` share the same handler (`gradle::run`). The program name is threaded
  through so the handler can re-invoke the correct binary.
- `maven` also matches `"mvnw"` (Maven wrapper). Both map to `maven::run`.
- `--show-stats` is consumed by `cmd::extract_show_stats` before dispatch and never passed to the
  subprocess.
- After the PR #267 refactor, `cmd/mod.rs` no longer contains `scrub_db_args` or
  `run_parsed_command_with_mode` inline — they live in `security.rs` and `execution.rs`
  respectively and are re-exported. If you can't find a symbol in `mod.rs`, check those files.

## Key Files

- `crates/rskim/src/cmd/build/mod.rs` — dispatch table; add new tool here first
- `crates/rskim/src/cmd/build/cargo.rs` — NDJSON + Rust diagnostic parsing; most complex handler
- `crates/rskim/src/cmd/dispatch.rs` — multi-category dispatcher; strict vs. passthrough model
- `crates/rskim/src/cmd/execution.rs` — `run_parsed_command_with_mode`, `run_tool`, `OutputFormat`
- `crates/rskim/src/cmd/registry.rs` — `KNOWN_SUBCOMMANDS`, `wrapper_targets`
- `crates/rskim/src/cmd/security.rs` — `scrub_db_args`, `scrub_infra_args`, credential scrubbing
- `crates/rskim/src/output/mod.rs` — `ParseResult<T>`, ANSI stripping, progress collapsing
- `crates/rskim/src/output/canonical.rs` — `BuildResult` struct (shared output type)
- `crates/rskim/src/runner.rs` — `CommandRunner` execution engine

## Related

- ADR-001: Fix all noticed issues immediately regardless of scope
- `crates/rskim/src/cmd/mod.rs` — top-level command dispatcher (420 lines post-refactor);
  re-exports all symbols from the five submodules
- `crates/rskim/src/analytics/` — records parse tier and token savings per build invocation
- `crates/rskim/src/cmd/pkg/npm/` — npm test/run delegation; uses `ScriptTool` to detect the
  project's test framework and route to the correct handler
