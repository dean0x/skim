---
feature: build-parsers
name: Build Tool Output Parsers
description: "Use when adding a new build tool parser, modifying cargo/tsc/make/gradle/maven compression, or debugging three-tier parse degradation for build commands. Keywords: build, cargo, tsc, make, gradle, maven, ParseResult, BuildResult, three-tier, NDJSON, flag injection, flat dispatch, multi-category dispatch."
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
updated: 2026-06-03
---

# Build Tool Output Parsers

## Overview

The `cmd/build/` module compresses and filters output from build tools (cargo, make, tsc, gradle, maven) using a three-tier parse degradation strategy. It is reached via two dispatch paths:

- **Flat dispatch**: `skim tsc`, `skim gradle`, `skim make` — the tool name is `argv[0]` when invoked as a PATH wrapper
- **Multi-category dispatch**: `skim cargo build`, `skim cargo clippy`, `skim gradle build` — routed through `cmd/build/mod.rs::run()`

All handlers share the same `ParseResult<BuildResult>` output contract and delegate to `CommandRunner` for process execution.

## Three-Tier Parse Degradation

The `output::ParseResult<T>` enum drives the degradation strategy:

```
Full(T)               — clean structured parse, all fields populated
Degraded(T, warnings) — partial parse, forwarded with warning markers injected
Passthrough(String)   — unrecognized format, output returned as-is
```

Each build handler attempts `Full` parse first, degrades to `Degraded` on partial failures, and falls back to `Passthrough` only when the output format is entirely unrecognized. The degradation level is used by the analytics recording layer to track parse quality.

## Component Architecture

### `mod.rs` — Public Dispatch

Routes incoming args to the correct handler. Handles `--help` early exit. Extracts `--show-stats` flag via `cmd::extract_show_stats`. Match arms cover:
- `"build"` / `"check"` / `"fmt"` / `"clippy"` / `"nextest"` / `"audit"` → `cargo::run_*`
- `"gradle"` / `"gradlew"` → `gradle::run`
- `"make"` → `make::run`
- `"mvn"` / `"mvnw"` / `"maven"` → `maven::run`
- Unknown subcommand → `cargo::run` (default, matches previous skim behavior for bare `skim cargo` invocations)

### `cargo.rs` — Rust Toolchain

Handles cargo build, check, fmt, clippy, nextest, and audit. Injects `--message-format=json` (or `--message-format json-diagnostic-rendered-ansi` for clippy) when the output destination allows structured capture. Parses NDJSON output from cargo. Collapses duplicate warning lines, extracts error/warning counts for the stats footer.

### `gradle.rs` — Gradle / Gradlew

Handles gradle and gradlew invocations. Extracts task names from gradle's structured output lines (`:taskName OUTCOME`), filters build lifecycle noise, preserves error output verbatim.

### `make.rs` — GNU Make

Handles make invocations. Extracts target names, filters `Entering directory`/`Leaving directory` lines, preserves compiler error output verbatim.

### `maven.rs` — Maven / mvnw

Handles mvn and mvnw invocations. Filters reactor summary lines, extracts module names, preserves BUILD SUCCESS/FAILURE and error output.

### `tsc.rs` — TypeScript Compiler

Handles tsc invocations. Parses TypeScript diagnostic output (`file.ts(line,col): error TS1234: message`), groups by file, deduplicates identical messages.

### `output/mod.rs` — ParseResult + Infrastructure

Provides:
- `ParseResult<T>` enum (Full / Degraded / Passthrough)
- ANSI stripping (`strip_ansi_codes`)
- Progress line collapsing (cargo download progress, percentage lines)
- Token-aware truncation
- Filter transparency headers

### `output/canonical.rs` — BuildResult

`BuildResult` is the canonical structured form of build output. Fields capture: error count, warning count, duration, individual diagnostics, and the raw output for passthrough. All handlers produce `ParseResult<BuildResult>`.

### `runner.rs` — CommandRunner

Timeout-aware command runner. Executes via `Command::new().args()` (no shell). Captures stdout + stderr concurrently via threads to prevent pipe deadlocks. Exposes `CommandOutput { stdout, stderr, exit_code, duration }`. `CommandRunner` is dependency-injected into all handlers.

## Integration Points

- **Analytics**: every handler receives a `RecordingContext` and calls `analytics::record_build_result` on completion. Fire-and-forget background thread.
- **PATH wrappers**: when `skim` binary detects `argv[0] == "gradle"` etc., it strips `~/.skim/bin` from `PATH` then calls `cmd/build/mod.rs::run()` with the tool name prepended to args.
- **`--show-stats`**: handlers print a token-reduction stats footer to stderr when this flag is present.

## Anti-Patterns

- **Shell-expanding arguments**: `CommandRunner` uses `Command::new().args()`, not a shell. Never pass shell metacharacters in args — they are passed literally.
- **Calling `cargo::run` for non-cargo subcommands**: the default arm in `mod.rs` sends unknown subcommands to `cargo::run` only because bare `skim cargo` usage is the dominant case. Add explicit arms for any new tools.
- **Logging parse failures to stdout**: degradation warnings go through `ParseResult::Degraded` and are rendered as comment-style markers in the output, not raw stderr noise.

## Gotchas

- `cargo clippy` uses a different `--message-format` flag form than `cargo build`. The injected flag is `--message-format json-diagnostic-rendered-ansi` (space-separated), not `=` form.
- `gradle` and `gradlew` share the same handler (`gradle::run`). The program name is threaded through so the handler can re-invoke the correct binary.
- `maven` also matches `"mvnw"` (Maven wrapper). Both map to `maven::run`.
- `--show-stats` is consumed by `cmd::extract_show_stats` before dispatch and never passed to the subprocess.

## Key Files

- `crates/rskim/src/cmd/build/mod.rs` — dispatch table; add new tool here first
- `crates/rskim/src/cmd/build/cargo.rs` — NDJSON + Rust diagnostic parsing; most complex handler
- `crates/rskim/src/output/mod.rs` — `ParseResult<T>`, ANSI stripping, progress collapsing
- `crates/rskim/src/output/canonical.rs` — `BuildResult` struct (shared output type)
- `crates/rskim/src/runner.rs` — `CommandRunner` execution engine

## Related

- ADR-001: Fix all noticed issues immediately regardless of scope
- `crates/rskim/src/cmd/mod.rs` — top-level command dispatcher; routes `cargo`, `tsc`, `make` etc. to `cmd/build/`
- `crates/rskim/src/analytics/` — records parse tier and token savings per build invocation
