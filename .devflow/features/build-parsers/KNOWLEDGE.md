---
feature: build-parsers
name: Build Tool Output Parsers
description: "Use when adding a new build tool parser, modifying cargo/tsc/make/gradle/maven compression, or debugging three-tier parse degradation for build commands. Keywords: build, cargo, tsc, make, gradle, maven, clippy, ParseResult, BuildResult, three-tier, NDJSON, flag injection, run_check, run_fmt, ChildGuard, indefinite, is_indefinite_command, expected_exit_codes, forward_stderr, ExitDisposition, classify_exit, run_parsed_command_with_exit, elision_marker, failure_context_body, parse_failure_details, compress-never-truncate."
category: component-patterns
directories: [crates/rskim/src/cmd/build/, crates/rskim/src/cmd/]
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
  - crates/rskim/src/cmd/execution.rs
  - crates/rskim/src/cmd/dispatch.rs
  - crates/rskim/src/cmd/security.rs
  - crates/rskim/src/cmd/registry.rs
  - crates/rskim/src/cmd/test_utils.rs
  - crates/rskim/src/runner.rs
  - crates/rskim/src/cmd/rewrite/indefinite.rs
  - crates/rskim/src/cmd/test/cargo.rs
  - crates/rskim/src/cmd/test/shared.rs
created: 2026-05-14
updated: 2026-06-17
version: 16
---

# Build Tool Output Parsers

## Overview

The build parsers module (`crates/rskim/src/cmd/build/`) compresses output from build tools (cargo, clippy, tsc, make, gradle, maven) into compact summaries for AI context windows. Each tool has a dedicated file (`cargo.rs`, `tsc.rs`, `make.rs`, `gradle.rs`, `maven.rs`) plus a shared `mod.rs` that provides the dispatcher and the `run_parsed_command` infrastructure function used by all five.

The module is invoked via flat dispatch (`skim tsc`) or multi-category dispatch (`skim cargo build`, `skim cargo clippy`). All parsers share the same three-tier degradation model: Full (clean structured parse) → Degraded (partial parse with warning markers) → Passthrough (raw output returned unchanged).

## Design Constraint: Compress, Never Truncate (#317)

**Core invariant**: wrappers may re-encode output (compress, summarize) but must never silently show _less_ than the raw tool would show.

Rules that flow from this constraint:
- A hard safety bound that cannot be avoided must use `output::elision_marker` (exact count of omitted items + `SKIM_PASSTHROUGH=1` hint).
- Unexpected non-zero exits (not in `expected_exit_codes`) forward raw stdout+stderr byte-faithfully instead of compressing.
- Rewrites must reconstruct the command byte-faithfully or bail — never emit a command that errors or changes semantics.
- Failure detail (panic location, assertion messages) must survive compression even on stable toolchains.

## cmd/mod.rs Refactor (PR #267)

`cmd/mod.rs` was refactored to reduce complexity. Functionality previously inline in `mod.rs` is now split across dedicated submodules:

- `cmd/dispatch.rs` — `dispatch()`, `run_raw_passthrough()`, and per-tool dispatcher helpers (`dispatch_cargo`, `dispatch_go`, `dispatch_swift`, `dispatch_dotnet`, `passthrough_subcmd`, `extract_subcmd`, `prepend_without`)
- `cmd/execution.rs` — `OutputFormat`, `RunContext`, `ParsedCommandConfig<'_>`, `ToolRunConfig<'_>`, `run_tool<T>`, `run_parsed_command_with_mode`, `run_parsed_command_with_exit`, `format_analytics_label`, `combine_output`, `obtain_output`, `render_output<T>`, `record_and_report`, `passthrough_raw`
- `cmd/security.rs` — `sanitize_for_display`, `scrub_db_args`, `scrub_infra_args`
- `cmd/registry.rs` — `KNOWN_SUBCOMMANDS` (sorted, binary-searchable), `is_known_subcommand`, `is_meta_subcommand`, `wrapper_targets`
- `cmd/test_utils.rs` — standalone `pub(crate)` module (compiled under `#[cfg(test)]` gate in `mod.rs`); canonical test helper source for all `cmd` subtree tests

`cmd/mod.rs` remains the coordination point (declares all submodules, re-exports public API) and still houses the inline helpers: `user_has_flag`, `inject_flag_before_separator`, `extract_show_stats`, `extract_json_flag`, `extract_output_format`, `should_read_stdin`, `read_bounded`, `read_stdin_bounded`, `MAX_STDIN_BYTES`, passthrough checks (`is_passthrough_mode`, `check_passthrough_str`, `check_passthrough_value`), `resolve_cache_dir`, and `skim_wrappers_dir`.

When adding a new parser, register the tool in `cmd/registry.rs` (`KNOWN_SUBCOMMANDS` array) and `cmd/dispatch.rs` (`dispatch()` match arm), not in `cmd/mod.rs`.

## Core Responsibilities

Each parser file should:
- Export a single `run()` function (plus `run_clippy()` for cargo)
- Inject tool-specific flags before spawning, when the tool supports structured output
- Delegate spawn + output capture to `run_parsed_command` in `mod.rs`
- Implement a private `parse_*` function that maps `&CommandOutput` to `ParseResult<BuildResult>`

`mod.rs` should:
- Dispatch by tool name and route to the correct handler
- Own `run_parsed_command`, the shared spawn-parse-emit-record loop
- Never contain tool-specific parsing logic

## Three-Tier Degradation Pattern

Every build parser implements the same three-tier cascade. Returning `None` from a tier signals "this tier cannot handle the input — try the next one":

The cascade is always tried in order from most structured to least:

```rust
fn parse_cargo(output: &CommandOutput) -> ParseResult<BuildResult> {
    // Tier 1: highest fidelity — JSON/NDJSON when available
    if let Some(result) = try_tier1_json(&output.stdout) {
        return result;  // Returns Full or Degraded
    }

    // Tier 2: regex fallback on stderr or combined output
    if let Some(result) = try_tier2_regex(&output.stderr) {
        return result;  // Returns Degraded with marker
    }

    // Tier 3: never returns None — always a valid ParseResult
    ParseResult::Passthrough(combined)
}
```

Key invariants:
- Tier 1 returns `ParseResult::Full` on clean parse, never `Degraded`
- Tier 2 returns `ParseResult::Degraded` with a human-readable marker string
- Tier 3 returns `ParseResult::Passthrough` — raw content, no marker
- The `parse_*` function is passed by function pointer to `run_parsed_command`, typed as `fn(&CommandOutput) -> ParseResult<BuildResult>`

## Tool-Specific Tier Strategies

Each tool assigns tiers differently based on what structured output the tool provides:

| Tool | Tier 1 | Tier 2 | Tier 3 |
|------|--------|--------|--------|
| cargo build / check / clippy | NDJSON from `--message-format=json` (stdout) | Regex `error[E\d+]` on stderr | Passthrough combined |
| cargo fmt | `parse_fmt`: empty combined → `Full(success/failure by exit code)` | (n/a — two-tier only) | Non-empty combined → `Passthrough` |
| tsc | Regex `file(line,col): error TSxxxx: msg` on stderr | Same regex on combined stdout+stderr | Passthrough combined |
| make | GCC/Clang diagnostics + Makefile syntax errors + make failure + linker errors + noop detection on combined | Noise stripping (compiler invocations, directory changes, CMake progress, archiver lines, `compilation terminated.` literal) | Passthrough combined |
| gradle / gradlew | Regex on task outcomes (`> Task :name OUTCOME`), Java/Kotlin diagnostics, `BUILD SUCCESSFUL/FAILED` summary | Noise strip (daemon startup, download progress, configure project, UP-TO-DATE/FROM-CACHE task lines) | Passthrough combined |
| mvn / mvnw | Regex on `[ERROR]`/`[WARNING]` lines and `[INFO] BUILD SUCCESS/FAILURE` + total time | Noise strip (`Downloading from`, `Downloaded from`, `[INFO] ---` separator lines, scanning/building markers) | Passthrough combined |

Note that tsc uses regex for its Tier 1 (not JSON) because tsc has no native structured output mode. Its Tier 2 is the same regex applied to a different stream, promoted to `Degraded` rather than `Full`.

The `compilation terminated.` check in make's Tier 2 is a literal `starts_with` check, not a regex — it is hardcoded alongside the three `LazyLock<Regex>` noise patterns.

## Flag Injection Pattern

Tools with a structured-output flag (cargo `--message-format=json`) inject it automatically unless the user already supplied it. This prevents overriding user intent.

The helpers live in `crate::cmd` (not inside `cmd/build/`):
- `user_has_flag(&args, &["--message-format"])` — prefix-match check, handles `--flag=value` and `--flag value` styles
- `inject_flag_before_separator(&mut args, "--message-format=json")` — inserts the flag before `--` so it targets the tool, not arguments after the separator
- `extract_show_stats(&args)` — strips `--show-stats` from the arg list and returns `(filtered_args, bool)`. `build/mod.rs` calls this at the top of its `run()` function; the `bool` is threaded through to `run_parsed_command`. This was centralized from per-handler inline logic.

Gradle and Maven do not inject flags — they have no native structured output mode, so their `run()` functions pass `&[]` as `env_vars` and perform no flag mutation.

## `cargo check` and `cargo fmt` Handlers

`cargo check` (`run_check`) uses the exact same `run_with_json_format("check", ...)` path as `cargo build` — it injects `--message-format=json` and runs the same three-tier NDJSON parser. The JSON schema for `cargo check` is identical to `cargo build` (both go through the rustc compiler message pipeline).

`cargo fmt` (`run_fmt`) is deliberately different:

- **No flag injection**: `cargo fmt` has no structured output mode. `run_fmt` directly calls `run_parsed_command("cargo", &full_args, &[("CARGO_TERM_COLOR", "never")], ..., parse_fmt)` — no `--message-format` injection occurs.
- **Two-tier parser** (`parse_fmt`): If combined stdout+stderr (trimmed) is empty, returns `ParseResult::Full(BuildResult::new(success, ...))` where `success = output.exit_code == Some(0)`. If combined output is non-empty, returns `ParseResult::Passthrough(trimmed)` — no `Degraded` tier exists for fmt.
- **Signal-killed treated as failure**: `output.exit_code == Some(0)` strictly — `None` (signal kill) and non-zero both produce `success = false`.
- **Module placement**: `run_fmt` lives in `cmd/build/cargo.rs` despite `cargo fmt` being categorized as a LINT operation by the rewrite engine. This is intentional: `cargo fmt` shares the `cargo` executable, `run_parsed_command`, and all cargo plumbing. The rewrite engine's categorization and the handler's module location are deliberately decoupled.

Both `run_check` and `run_fmt` are dispatched through `build/mod.rs`: `"check" => cargo::run_check(...)`, `"fmt" => cargo::run_fmt(...)`. The dispatcher in `cmd/dispatch.rs` also routes `"check"` and `"fmt"` via `build::run(&prepend_without(...), analytics)`.

## Gradle Parser Details

Gradle (`gradle.rs`) supports both `gradle` and `gradlew` aliases via the `program` parameter passed through from the dispatcher. The `run()` signature is `run(program: &str, args: &[String], ...)` — the program name is forwarded directly to `run_parsed_command`.

Tier 1 recognizes four pattern types on the combined stdout+stderr stream:
- Task outcomes: `> Task :name OUTCOME` via `GRADLE_TASK_RE` — tasks with `FAILED` outcome increment error count
- Java diagnostics: `file.java:line: error|warning|note: message` via `JAVA_DIAG_RE`
- Kotlin diagnostics: `e: file.kt: (line, col): message` or `w:` via `KOTLIN_DIAG_RE`
- Build summary: `BUILD SUCCESSFUL in Xs` via `BUILD_SUCCESS_RE` (duration extracted) and `BUILD FAILED` via `BUILD_FAILED_RE`

Success determination in Tier 1 requires all three conditions: `exit_code == Some(0) && errors == 0 && !BUILD_FAILED_RE.is_match(combined)`. A zero exit code alone is not enough — explicit `BUILD FAILED` text also marks failure.

Duration parsing handles the `3.456 secs` format only (extracts the first token and multiplies by 1000). The `1 min 2 secs` format is not parsed — `parse_gradle_duration` returns `None` for multi-word durations.

## Maven Parser Details

Maven (`maven.rs`) supports `mvn`, `mvnw`, and the alias `maven` — all forwarded to `run_parsed_command` via the `program` parameter. No flag injection occurs.

Tier 1 pattern types on the combined stream:
- Error lines: `[ERROR] message` via `MAVEN_ERROR_RE`
- Warning lines: `[WARNING] message` via `MAVEN_WARN_RE`
- Build outcome: `[INFO] BUILD SUCCESS` and `[INFO] BUILD FAILURE` via `MAVEN_SUCCESS_RE` / `MAVEN_FAILURE_RE`
- Total time: `[INFO] Total time:  2.345 s` via `MAVEN_TIME_RE`

Success determination: `exit_code == Some(0) && MAVEN_SUCCESS_RE.is_match(combined) && errors == 0`. Unlike gradle, maven requires the `BUILD SUCCESS` marker to be present in the output — a zero exit code without the marker produces `success = false`.

Duration parsing handles two formats:
- `2.345 s` (seconds with decimal) → milliseconds
- `1:23 min` (minutes:seconds) → milliseconds

Tier 2 noise patterns strip `[INFO] Downloading/Downloaded from`, `[INFO] ---` separator lines, `[INFO] Scanning for projects`, `[INFO] Building`, reactor summary lines, and empty `[INFO]` lines via `MAVEN_INFO_NOISE_RE`.

## `run_parsed_command` — Shared Infrastructure (Build Family)

All five build parsers call `super::run_parsed_command(...)` in `build/mod.rs` instead of spawning processes themselves. This function handles:
1. Command spawn — blocks on a plain `wait()` with no internal timeout (see ADR-008)
2. ANSI escape code stripping from both stdout and stderr before parsing
3. Calling the parser function pointer
4. Emitting degradation markers to stderr (only when `--debug` / `SKIM_DEBUG=1` is active — silent by default)
5. Printing result content to stdout
6. Token stats (if `--show-stats`)
7. Exit code determination from `BuildResult.success` or raw `output.exit_code`
8. Analytics recording via `format_analytics_label("build", program, &args.join(" "))` (fire-and-forget)

The full signature is:
```rust
pub(super) fn run_parsed_command(
    program: &str,
    args: &[String],
    env_vars: &[(&str, &str)],
    install_hint: &str,
    show_stats: bool,
    rec: crate::analytics::RecordingContext<'_>,  // constructed once in build::run()
    parser: fn(&CommandOutput) -> ParseResult<BuildResult>,  // fn pointer, not FnOnce
) -> anyhow::Result<ExitCode>
```

Two details worth noting: `rec` is a `RecordingContext` threaded from `build::run()` (which constructs it once from `AnalyticsConfig` with `CommandType::Build`); and `parser` is a plain `fn` pointer, not a closure — build parsers need no captured state. The analytics call annotates the tier via `rec.with_tier(result.tier_name())`.

Spawn failures (missing executable) use `anyhow::bail!` — a hard error with an install hint. This differs from the non-build path: `obtain_output` in `cmd/execution.rs` returns `Ok(None)` on spawn failure (soft fallback), which `run_parsed_command_with_mode` then converts to `Ok(ExitCode::FAILURE)`. Build commands have no stdin-passthrough path, so `ENOENT` is always fatal.

**Important**: build parsers use `run_parsed_command` (defined in `build/mod.rs`), not `run_parsed_command_with_mode` or `run_tool<T>` (both defined in `cmd/execution.rs`). The three are intentionally separate:
- Build: no `use_stdin`, no `--json` output mode, no `SKIM_PASSTHROUGH` bypass, no compressed-output hint on failure, plain `fn()` parser pointer, bail-on-spawn, no internal timeout
- Other families (lint, infra, db, file): use `run_tool<T>` (the generic runner added in #214) which wraps `run_parsed_command_with_mode`. `run_tool<T>` takes `ToolRunConfig<'a>` (program, env_overrides, install_hint, family, skip_ansi_strip, command_type, expected_exit_codes, forward_stderr), a `&RunContext`, a `prepare_args` closure, and a one-arg `parse_fn`

`run_tool<T>` in `cmd/execution.rs` explicitly documents this boundary: "build::run_parsed_command is intentionally not replaced: it has a different call shape (no `ctx: &RunContext`, different analytics path)." Switching build parsers to use `run_tool<T>` is not just a refactor — the signatures are incompatible.

The `ParsedCommandConfig` struct (in `cmd/execution.rs`) adds several fields not present in the build path: `family` (for analytics label disambiguation — prevents collision when `cargo` appears in both build and pkg), `skip_ansi_strip` (DB tools emit TSV; stripping would drop tab characters), `output_format` (supports `--json` output mode), `expected_exit_codes`, and `forward_stderr`. Build does none of these, which is why the two paths are separate rather than consolidated.

## Exit-Disposition Matrix (#317)

`cmd/execution.rs` implements an exit-disposition matrix that applies to all non-build families via `run_parsed_command_with_exit` / `run_parsed_command_with_mode`:

```
ExitDisposition::Success          — exit 0        → compress normally
ExitDisposition::ExpectedFailure  — non-zero in expected_exit_codes → compress
ExitDisposition::UnexpectedFailure — all other non-zero, or signal kill → forward raw
```

`classify_exit(code: Option<i32>, expected: &[i32]) -> ExitDisposition` must be called on the raw `Option<i32>` BEFORE any `unwrap_or(1)` default: a signal kill (`None`) is always `UnexpectedFailure` even if `1` is in `expected_exit_codes`.

**Unexpected failure path**: emits `[skim] {program} exited N; raw output (not compressed).` to stderr, records zero savings under the `"raw"` analytics tier, and calls `passthrough_raw` — byte-faithful forward of stdout+stderr before ANSI stripping.

**`expected_exit_codes`** — non-zero codes the parser meaningfully compresses:
- `grep`/`rg`: `&[1]` (no matches)
- `diff`: `&[1]` (files differ)
- `cargo test`: `&[101]` (test failures; also compile errors which fall through to Passthrough)
- `swiftlint`/`terraform`: `&[2]`
- `lint` family: `&[1]`
- `pkg`: `&[1]`
- `gofmt`/`db`/`infra`/`file`: `&[]` (expect only exit 0)

**`forward_stderr`** — when `true`, child stderr is forwarded verbatim on the compressed path (captured before ANSI stripping for byte-faithfulness). Used for `file` and `db` families whose parsers consume only stdout, so warnings/diagnostics on stderr are never silently dropped.

**Notice matrix**:
- Unexpected failure → `[skim] {program} exited N; raw output (not compressed).` (stderr)
- Expected failure at `Passthrough` tier → silent (matches raw tool behavior, e.g. grep no-match silence)
- Expected failure at `Full`/`Degraded` tier → `[skim] compressed output (exit N). SKIM_PASSTHROUGH=1 for full output.` (stderr)

**Build family is exempt**: `build::run_parsed_command` does not use this matrix — it has its own exit-code logic derived from `BuildResult.success`.

## `run_parsed_command_with_exit` — Parser-Derived Exit Codes (#317)

`run_parsed_command_with_mode` is now a thin wrapper around `run_parsed_command_with_exit`:

```rust
pub(crate) fn run_parsed_command_with_exit<T>(
    config: ParsedCommandConfig<'_>,
    parse: impl FnOnce(&CommandOutput) -> ParseResult<T>,
    derive_exit: impl FnOnce(&ParseResult<T>) -> Option<i32>,
) -> anyhow::Result<ExitCode>
```

`derive_exit` inspects the parsed result and may return a non-zero exit code. The final exit is `max(child_exit, derived)` — needed on the stdin path, where `obtain_output` fabricates `exit_code: Some(0)` and a piped failing test run would otherwise exit 0 even when `summary.fail > 0`.

`run_parsed_command_with_mode` passes `|_| None` for `derive_exit` (no derived exit needed for families whose parsers cannot observe failure independently of the exit code).

**`cargo test` uses `run_parsed_command_with_exit` directly** (not via `run_tool<T>`):
```rust
// In cmd/test/cargo.rs run():
run_parsed_command_with_exit(
    ParsedCommandConfig { ..., expected_exit_codes: &[101], forward_stderr: false, ... },
    move |output| parse_impl(output, is_nextest),
    |result| match result {
        ParseResult::Full(r) | ParseResult::Degraded(r, _) if r.summary.fail > 0 => Some(1),
        _ => None,
    },
)
```

## `TestResult` — Context Safety Net (#317)

`TestResult` gained a `context: Option<String>` field and a `TestResult::with_context` constructor:

```rust
pub(crate) struct TestResult {
    pub(crate) summary: TestSummary,
    pub(crate) entries: Vec<TestEntry>,
    pub(crate) context: Option<String>,  // raw failure-context block
    rendered: String,
}
```

When present, `context` is appended to the rendered output under a `--- failure context ---` banner. This is a safety net for parsers whose structured tiers cannot attach per-test `detail` — the diagnostic is never dropped even when `parse_failure_details` produces no block entries.

`TestResult::render` appends `context` last:
```
{summary}
 FAIL: test_name
  {detail}
...
--- failure context ---
{context}
```

## Cargo Tier-2: `parse_failure_details` + Context Safety Net (#317)

On stable Rust (`cargo test` without `--format json`), libtest prints each failing test's captured output in blocks:

```
---- module::test_name stdout ----
thread 'module::test_name' panicked at src/lib.rs:5:9:
assertion failed: …

failures:
    module::test_name

test result: FAILED. 0 passed; 1 failed; 0 ignored
```

`parse_failure_details(text: &str) -> HashMap<String, String>` (in `cmd/test/cargo.rs`) is a state machine that parses these blocks and returns a map from test name to failure body. Header pattern: `RE_FAILURE_BLOCK_HEADER = r"^---- (.+?) (?:stdout|stderr) ----$"` (lazy `.+?` to handle doctest names with spaces like `src/lib.rs - module (line 10)`). A block ends at the next header, a bare `failures:` recap line, or a `test result:` summary line. Multiple blocks for the same test name (stdout + stderr) are appended rather than overwritten.

`try_parse_regex` (Tier 2) uses `parse_failure_details` to attach `detail` to each `TestEntry`. If block parsing yields no `detail` for any entry (e.g. very old toolchain format), a `failure_context_body` safety net attaches the raw failure section as `context` so the `panicked at` diagnostic is never dropped.

## `failure_context_body` + `emit_failure_context` (shared.rs, #317)

`failure_context_body(raw_output: &str) -> Option<String>` (in `cmd/test/shared.rs`) extracts the failure-diagnostic section:

- Section starts at the FIRST line containing any `FAILURE_MARKERS` token: `["FAILED", "--- FAIL:", "panicked at", "failures:", "AssertionError", "error[", "✕", "✗"]`
- If no marker found → last 50 lines (legacy tail fallback)
- Section ≤ 350 lines → returned whole (compress, never truncate)
- Section > 350 lines → `head(300)` + `elision_marker(shown, total, "lines")` + `last_n_lines(50)` (recap block)
- Returns `None` when output is empty/whitespace

`emit_failure_context(raw_output: &str, exit_code: i32)` calls `failure_context_body` and, if `Some`, prints `--- failure context ---` to stdout followed by the body, then emits `[skim] compressed output (exit N). SKIM_PASSTHROUGH=1 for full output.` to stderr. Called by test-runner handlers (vitest, go, playwright, pytest) when failures are present.

The cargo `run_parsed_command_with_exit` path does NOT call `emit_failure_context` — the context is attached directly to `TestResult.context` and rendered inline.

## Loud Elision Markers (`output::elision_marker`, #317)

When a genuine safety bound must truncate output, wrappers use the shared elision helpers in `crates/rskim/src/output/mod.rs`:

```rust
// Returns None when shown >= total (nothing omitted — no marker needed)
pub(crate) fn elision_marker(shown: usize, total: usize, unit: &str) -> Option<String>
// → "[skim] {omitted} {unit} omitted ({shown} of {total} shown) — SKIM_PASSTHROUGH=1 for full output"

// For streaming sites where total is unknowable mid-stream
pub(crate) fn elision_marker_unbounded(shown_desc: &str, unit: &str) -> String
// → "[skim] {unit} elided beyond {shown_desc} — SKIM_PASSTHROUGH=1 for full output"
```

Always use these functions — never construct elision notices inline. `elision_marker` returns `None` when nothing is omitted, so callers can do `if let Some(m) = elision_marker(...) { ... }` cleanly.

## `CommandRunner` and `ChildGuard` — Execution Layer (ADR-008)

`CommandRunner` (in `crates/rskim/src/runner.rs`) is a **stateless unit struct** (`#[derive(Default)]`, `new()` takes no args). It imposes no internal wall-clock timeout. `run_with_env` blocks on a plain `wait()` until the child process exits naturally.

**No timeout inside skim.** Callers that need a time bound must apply one externally:
- CI step timeout (GitHub Actions `timeout-minutes:`)
- The shell `timeout(1)` utility
- Agent tool timeout
- `Ctrl-C`

The 64 MiB memory cap (`MAX_OUTPUT_BYTES`) is unchanged — ADR-008 removes the TIME bound only, not the MEMORY bound.

**`ChildGuard` kill-on-drop.** Every spawned child is immediately wrapped in `ChildGuard`, a RAII newtype around `std::process::Child`. Its `Drop` implementation calls `kill()` then `wait()`. On the normal execution path the child has already exited before drop fires, so `kill()` is a harmless `ESRCH` no-op. On any early-return path — the 64 MiB cap error, a pipe-capture failure, a reader-thread panic — the guard kills the still-running child before `run_with_env` returns, preventing zombie/orphan processes.

`RunnerError::Timeout` and `wait_with_timeout` no longer exist. Any code referencing them will not compile.

**Node.js tool resolution fallback.** `CommandRunner` also provides two higher-level methods for commands that may be installed as Node.js local packages rather than globally on `$PATH`:

- `run_with_node_fallback(program, args)` — tries in order: (1) direct `$PATH` lookup, (2) `./node_modules/.bin/{program}` (local bin check), (3) `npx --no-install {program}`. Returns the original spawn error when all three fail. Absolute or relative paths (containing `/`) skip fallback — the caller is explicit about the binary location. Only activates on `SpawnFailed` errors (ENOENT); other runner errors (`PipeCaptureFailed`, `ReaderPanicked`) are returned immediately without retry, since the binary was found and launched.
- `run_with_env_node_fallback(program, args, env_vars)` — same tri-strategy resolution with environment variable overrides forwarded to every candidate.

Build parsers do NOT use the Node.js fallback methods — they use `run_parsed_command` in `build/mod.rs`, which calls `CommandRunner::run_with_env` directly. The fallback is used by Node.js-ecosystem handlers (e.g., vitest, jest, eslint) where local package installs are the norm.

## Indefinite-Command Detection (`cmd/rewrite/indefinite.rs`)

`crates/rskim/src/cmd/rewrite/indefinite.rs` (added in ADR-008 Part C) provides `is_indefinite_command(tokens: &[&str]) -> bool`. It detects daemon processes, watch modes, and live log followers so the dispatcher can pass them through with inherited stdio rather than capturing and compressing their output.

Key design principles:
- **Program-aware, not flag-generic.** Detection is keyed on specific program + flag combinations. Generic patterns like "any `-f` flag" would misfire on `grep -f`, `rm -f`, `git push -f`.
- **Conservative.** A missed daemon degrades to the buffered capture path (64 MiB cap still applies; `SKIM_PASSTHROUGH=1` is an escape hatch). A false-positive only loses compression for that run, never correctness.
- **`--help`/`--version`/`-h`/`-V` always short-circuit to finite.** Without this guard, `skim vitest --help` would be misclassified as indefinite and routed to `run_inherited_passthrough` (exiting 127 if the real binary is absent) instead of printing skim's own help. Check is applied before the program-specific match.
- **Leading env-var assignment tokens are skipped.** `NODE_ENV=dev npm run dev` — the function walks past any leading `KEY=VALUE` tokens (containing `=`) to find the real program name. `program_idx` is the position of the first token without `=`; `program = tokens[program_idx]`; `rest = &tokens[program_idx + 1..]`.

Categories recognized as indefinite:
- `watch <…>` — always
- Log followers: `tail`/`journalctl` + `-f`/`-F`/`--follow`; `docker [compose] logs` + `-f`/`--follow`; `kubectl logs` + `-f`/`--follow`
- Watch-mode build/test runners: `tsc --watch/-w`; `jest --watch/--watchAll`; `webpack --watch/-w`/`webpack serve`; `vite`/`rollup`/`esbuild` + `--watch`; `vitest` bare or `--watch` (finite when `run` subcommand present); `nodemon`/`serve`/`http-server`/`live-server` — always
- Dev servers: `next dev`, `nuxt dev`, `astro dev`, `ng serve`, `vite` bare/`dev`/`serve`/`preview`
- Package-manager scripts: `npm|yarn|pnpm|bun` with script `dev|start|serve|watch`

**`vitest_is_indefinite` implementation**: `!rest.contains(&"run")`. If the `run` token appears anywhere in the remaining args, the invocation is finite. This means `vitest run --reporter verbose` is correctly classified as finite.

**`pm_is_indefinite` implementation**: extracts the script name from the arg list, then checks it against `INDEFINITE_SCRIPTS = ["dev", "start", "serve", "watch"]` and `FINITE_SCRIPTS = ["build", "test", "install", "ci", "lint", "audit", "add", "remove", "update"]`. Script extraction varies by package manager:
- `npm`/`pnpm`: if first positional is `run` or `run-script`, script is second positional; otherwise script is the first positional
- `yarn`/`bun`: walks past `run`/`run-script` to find the script name (supports both `yarn dev` and `yarn run dev` styles)

**Two entry points consume `is_indefinite_command`:**
1. **Hook / rewrite path** — when the rewrite engine detects an indefinite command it returns no-rewrite, leaving the command unchanged.
2. **`dispatch()` in `cmd/dispatch.rs`** — before routing to any handler, calls `is_indefinite_command` and, when true and `!is_passthrough_mode()`, delegates to `run_inherited_passthrough(subcommand, args)`. This spawns the real binary with fully inherited stdio (stdin, stdout, stderr) — no capture, no compression, no analytics. The check fires regardless of whether stdin is a TTY.

**`should_read_stdin` and the `vitest run` exception**: `should_read_stdin(args)` returns `true` when stdin is not a terminal AND `args` is empty OR `args == ["run"]`. The `["run"]` exception allows `cat output | skim vitest run` to compress piped input — `run` is a routing hint for vitest's finite mode, not a real test-file argument. Bare `cat output | skim vitest` now routes to `run_inherited_passthrough` (indefinite guard fires first) and runs vitest live instead.

## `BuildResult` — Output Type

`BuildResult` (in `crates/rskim/src/output/canonical.rs`) carries:
- `success: bool`
- `warnings: usize`
- `errors: usize`
- `duration_ms: Option<u64>` — cargo does not report duration in JSON; gradle and maven do report duration when `BUILD SUCCESSFUL` appears
- `error_messages: Vec<String>` — formatted per-error strings, plus grouped warning codes for clippy

The `rendered` field is pre-computed in `BuildResult::new()`. On success it renders as `OK warnings: N errors: 0`; on failure it appends each `error_messages` entry indented with a space. Error messages are only printed on failure — clippy warning code summaries are appended to `error_messages` but silently carried on a successful clippy run.

`BuildResult::render` is a private `fn` in `canonical.rs`. The other canonical types (`LintResult`, `DbResult`, `GitResult`, `PkgResult`) follow the same pattern. As of #214, `DbResult::render` was decomposed into 5 private helper functions — the decomposition pattern is the standard for any canonical type whose render logic grows beyond a single screen.

## Regex Compilation

All regex patterns are compiled once via `LazyLock<Regex>` at module scope. This avoids per-call recompilation and is the standard pattern across all build and lint parsers.

```rust
static CARGO_ERROR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"error\[E\d+\]").expect("valid regex"));
```

Use `.expect("valid regex")` not `.unwrap()` — the expect message documents intent and is visible in panics.

## Shared Test Helpers

All build parser tests (and tests across the `cmd` subtree) use the shared helpers from `crate::cmd::test_utils`. This is a **standalone file** (`cmd/test_utils.rs`) compiled under a `#[cfg(test)]` gate in `cmd/mod.rs` — it is NOT an inline `mod test_utils { ... }` block.

The module was renamed from `test_support` to `test_utils` in PR #126. All ~75 references across `crates/rskim/` were updated. Legacy `test_support` imports will not compile.

The four helpers available:
- `make_output(stdout: &str) -> CommandOutput` — success case: stderr empty, exit_code Some(0), duration ZERO
- `make_output_full(stdout, stderr, exit_code: Option<i32>) -> CommandOutput` — full control for non-zero exits, stderr content, signal-kill (None exit code)
- `make_output_stderr(stderr: &str) -> CommandOutput` — all output on stderr, exit_code Some(0); for tools that default to stderr (wget, curl)
- `load_fixture(subdir: &str, name: &str) -> String` — loads from `tests/fixtures/cmd/{subdir}/{name}`; both `subdir` and `name` must be single path components validated by a `Component::Normal` check (rejects `/`, `\`, `..`, absolute paths, and drive-relative paths); panics with a clear message if the file cannot be read

The `load_fixture` traversal guard uses `std::path::Path::new(s).components()` and asserts that the path parses to exactly one `Component::Normal(_)` component. This is more robust than character exclusion — it rejects OS-level traversal sequences that don't contain `/` or `\` directly (e.g., Windows drive-relative paths).

Build parser tests import as `use crate::cmd::test_utils::{make_output_full, load_fixture}`. Fixture files for build parsers live at `tests/fixtures/cmd/build/` (e.g., `cargo_build_fail.json`, `cargo_build_ok.json`, `clippy_fail.json`, `clippy_warnings.json`, `make_errors.txt`, `tsc_errors.txt`).

Cargo test parser fixtures live at `tests/fixtures/cmd/test/` (e.g., `cargo_pass.json`, `cargo_fail.json`, `cargo_panic_char_boundary.txt`).

Do not redeclare local versions — use the canonical source to prevent drift.

## Anti-Patterns

- **Adding tool-specific spawn logic to `run_parsed_command`**: the shared function must remain tool-agnostic. Tool-specific env vars (`CARGO_TERM_COLOR=never`) and flags belong in the per-tool `run()` function.

- **Returning `ParseResult::Degraded` from Tier 1**: Tier 1 must only emit `Full`. If the parse is partial, it is not a Tier 1 parse — fall through to Tier 2. Mixing `Full` and `Degraded` from the same tier breaks the diagnostic signal.

- **Using `try_tier1` signature for regex-only parsers**: tsc's "Tier 1" is regex on stderr, but it still returns `ParseResult::Full` (not `Degraded`) because stderr is the authoritative output stream. The Degraded tier is reserved for the combined-stream fallback where stream identity is unknown.

- **Omitting the empty-output success check**: gradle, maven, tsc, and make all treat empty stdout+stderr as a successful build when `exit_code == Some(0)`. Skipping this check causes `ParseResult::Passthrough("")` instead of `ParseResult::Full(success)`.

- **Switching build parsers to use `run_tool<T>` or `run_parsed_command_with_mode`**: the build family intentionally diverges. See the Infrastructure section above. The signatures are incompatible: `run_parsed_command` takes `fn(&CommandOutput)` (one arg, plain fn pointer); `run_tool<T>` takes `FnOnce(&CommandOutput)` via `run_parsed_command_with_mode` which takes `FnOnce(&CommandOutput, &[String])` (two args). Mixing them will fail to compile.

- **Declaring a new tool in `run()` dispatch without adding it to the help text and error messages**: the unknown-tool error message and `print_help()` list supported tools by name — both must be updated when adding a new parser. Also add the tool name to `KNOWN_SUBCOMMANDS` in `cmd/registry.rs` and the `dispatch()` match arm in `cmd/dispatch.rs`.

- **Adding a build tool as a passthrough dispatcher**: build tools should use the strict dispatcher model (error on unknown subcommand), not the passthrough model used by `swift`/`dotnet`. Build commands have a finite, well-defined subcommand set; passthrough is reserved for tools with wide lifecycle subcommand surfaces that agents invoke routinely.

- **Applying three-tier degradation to `cargo fmt`**: `parse_fmt` is intentionally two-tier (Full or Passthrough). Do not add a Tier 2 regex fallback to `parse_fmt` — `cargo fmt` produces no structured diagnostic output, so Degraded would carry no useful signal. The two-tier model is by design.

- **Declaring local `make_output` or `load_fixture` in build test modules**: use `crate::cmd::test_utils` instead. Local redeclarations drift from the canonical definition (e.g., duration field set to arbitrary millisecond values instead of `Duration::ZERO`).

- **Using `super::super::test_utils` or `test_support` import paths**: the module is now a standalone file `cmd/test_utils.rs`, not an inline block, and was renamed from `test_support` in PR #126. The canonical import path is `crate::cmd::test_utils::{...}` for any module in the `cmd` subtree.

- **Referencing `obtain_output` or `render_output<T>` as being in `cmd/mod.rs`**: both functions were moved to `cmd/execution.rs` during the PR #267 refactor. `cmd/mod.rs` no longer contains these — it is a coordination/re-export point only.

- **Adding `DEFAULT_CMD_TIMEOUT` or any internal timeout to `run_parsed_command`**: that constant has been deleted (ADR-008). `CommandRunner` is stateless and imposes no time bound. External timeout mechanisms are the caller's responsibility.

- **Using a generic watch-flag check for `is_indefinite_command`**: the function is keyed on specific program + flag pairs. Never add a generic "any command with `-f`" or "any command with `--watch`" branch — that would misfire on `grep -f`, `rm -f`, `git push -f`, etc. Add program-specific branches only.

- **Forgetting the `--help`/`--version` short-circuit in new indefinite programs**: when adding a new program to `is_indefinite_command`, the `has_help_or_version_flag` guard at the top of the function already protects all programs. Do not add per-program help checks — the universal guard handles it.

- **Omitting `expected_exit_codes` and `forward_stderr` when constructing `ToolRunConfig` or `ParsedCommandConfig`**: both fields have no defaults — every construction site must supply them explicitly. Missing them will fail to compile. Audit the appropriate values for the tool (see the Exit-Disposition Matrix section above).

- **Silently truncating output without `elision_marker`**: any hard bound that drops output must use `output::elision_marker` (or `elision_marker_unbounded` for streaming). Never emit a cap message inline — it would lack exact counts and the `SKIM_PASSTHROUGH=1` escape hatch.

- **Calling `classify_exit` on `unwrap_or(1)` output**: `classify_exit` must be called on the raw `Option<i32>` so that `None` (signal kill) maps to `UnexpectedFailure` regardless of what value `expected_exit_codes` contains.

## Gotchas

- **Degradation markers and parse notices are silent by default**: `emit_markers` in `ParseResult` only writes to stderr when `SKIM_DEBUG=1` or `--debug` is set. In normal operation, Tier 2 (`Degraded`) and Tier 3 (`Passthrough`) passes are silent — no `[skim:warning]` or `[skim:notice]` lines appear. This means a build that degrades to regex fallback gives no on-screen indication unless debug mode is active. Enable `SKIM_DEBUG=1` when diagnosing unexpected parse behavior.

- **`--message-format` injection must happen before `--`**: `inject_flag_before_separator` places the flag before `--`. If it were appended after `--`, cargo would treat it as an argument to the compiled binary, not to cargo itself.

- **Cargo warning codes are grouped into `error_messages`, not a separate field**: clippy warning codes (`dead_code: 2 occurrence(s)`) are appended to `error_messages` in `BuildResult`. They are not rendered on a successful clippy run.

- **Make combines stdout and stderr**: make sends compiler diagnostics to whichever stream the child compiler uses. Always combine both streams before parsing.

- **`build-finished` is required for Tier 1 in cargo**: `try_tier1_json` returns `None` (falls through to Tier 2) if no `build-finished` NDJSON line appears. This means an incomplete JSON stream — e.g., a killed cargo process — degrades gracefully rather than reporting false success.

- **`note` severity in GCC diagnostics is not counted as an error**: `try_tier1_diagnostics` in make.rs counts `error` and `fatal error` as errors, `warning` as warnings, and skips `note`. Notes are still included in `error_messages` for context.

- **Make noop-after-errors must not discard accumulated diagnostics**: The `MAKE_NOOP_RE` check in `try_tier1_diagnostics` is guarded by `!any_match`. A "Nothing to be done" or "is up to date" line triggers an immediate success return ONLY when no diagnostic lines have been accumulated yet.

- **Signal-killed process (exit code `None`) in empty-output path is failure**: The empty-output early return uses `output.exit_code == Some(0)` for success, not `!= Some(1)`. A signal-killed process has `exit_code: None` — that must map to `success = false`.

- **No internal timeout; long builds block until completion**: `run_parsed_command` blocks until the child process exits naturally. There is no wall-clock cap inside skim (ADR-008). A `cargo test --all-features` on a large workspace will hold the call for as long as it takes. `ChildGuard` ensures the child is reaped on early return, but it does not impose a time limit.

- **Gradle success requires both zero exit code AND absence of `BUILD FAILED` text**: a process can exit 0 while gradle still emits `BUILD FAILED` in certain edge cases. Both conditions are checked in Tier 1.

- **Maven success requires `BUILD SUCCESS` text, not just zero exit code**: unlike most tools, maven's Tier 1 explicitly checks that `MAVEN_SUCCESS_RE.is_match(combined)` is true. A clean exit without the success marker sets `success = false`.

- **Gradle duration parsing only handles `X.XXX secs` format**: the `parse_gradle_duration` function splits on whitespace and parses the first token as `f64`. Multi-part durations like `1 min 2 secs` return `None` and `duration_ms` is left as `None` in `BuildResult`.

- **tsc empty-output check is placed after both tier 1 and tier 2**: unlike make.rs (which guards empty output at the top of `parse_make`), tsc.rs checks for empty output only after tier 1 and tier 2 both return `None`.

- **`load_fixture` traversal guard uses `Component::Normal` check, not character exclusion**: the guard changed in PR #126 from `contains(['/', '\\']) || == ".."` to a `Path::new(s).components()` exhaustive check. This is more robust — it also rejects absolute paths (`/foo`) and drive-relative paths (`C:foo`). A single `Component::Normal(_)` with no second component is the only accepted form.

- **`is_indefinite_command` fires regardless of stdin TTY**: in `dispatch()`, the indefinite-command guard does not check `is_terminal()`. CI pipelines and PATH-wrapper sub-agents always have non-TTY stdin; gating on TTY would skip daemon detection for skim's primary consumers. Trade-off: `cat output | skim vitest` runs vitest live rather than parsing piped input (use `skim vitest run` to compress piped output instead).

- **`is_indefinite_command` skips leading env-var tokens**: `NODE_ENV=dev npm run dev` works correctly because the function finds `program_idx` by scanning for the first token without `=`. If you pass `["NODE_ENV=dev", "npm", "run", "dev"]`, `program` is `"npm"`, not `"NODE_ENV=dev"`. This is transparent to callers — tokenize as-is.

- **`vitest_is_indefinite` checks for `run` anywhere in `rest`**: `!rest.contains(&"run")` — if `run` appears at any position (not just first), vitest is classified as finite. This means `vitest --reporter verbose run` is also finite, matching vitest's actual behavior.

- **`pm_is_indefinite` guards against FINITE_SCRIPTS before returning false for unknown scripts**: both `INDEFINITE_SCRIPTS` and `FINITE_SCRIPTS` are checked. An unknown script name (not in either list) returns `false` — treat as finite. This is conservative: a new package-manager script that isn't in either list won't accidentally block compression.

- **`cargo test` Tier-2 on stable toolchain may produce no per-entry `detail` when block parsing fails**: `parse_failure_details` attaches per-test panic output from `---- name stdout ----` blocks. If no blocks are found (e.g. very old toolchain or unusual output format), the safety net sets `TestResult.context` to `failure_context_body(text)` so the raw failure section is still surfaced in the rendered output.

- **`failure_context_body` uses pointer arithmetic to slice from the marker line**: `let offset = first_marker_line.as_ptr() as usize - cleaned.as_ptr() as usize`. This is valid because `first_marker_line` is a `lines()` iterator element that borrows from `cleaned`. Do not replicate this pattern without confirming the lifetime guarantee.

- **`emit_failure_context` writes to stdout (body) and stderr (hint)**: the `--- failure context ---` banner and body go to stdout; the `[skim] compressed output` hint goes to stderr. This matches the convention where stderr carries skim's own notices, not the tool's diagnostic content.

- **`run_parsed_command_with_exit` applies `max(child, derived)` not `child.or(derived)`**: `max` means a child exit 101 is preserved even if `derive_exit` returns `Some(1)`. The derived exit only wins when the child exit is 0 (stdin fabrication path).

## Key Files

- `crates/rskim/src/cmd/build/mod.rs` — dispatcher (`run`), shared `run_parsed_command`, and `print_help`
- `crates/rskim/src/cmd/build/cargo.rs` — cargo build/check/clippy: `run`/`run_check`/`run_clippy` all delegate to `run_with_json_format` (NDJSON Tier 1, regex Tier 2); `run_fmt` uses a separate two-tier `parse_fmt` (empty → Full, non-empty → Passthrough)
- `crates/rskim/src/cmd/build/tsc.rs` — TypeScript compiler: regex-on-stderr Tier 1, combined Tier 2, empty-output success (checked after both tiers)
- `crates/rskim/src/cmd/build/make.rs` — GNU make: GCC diagnostics Tier 1, noise-strip Tier 2, eight `LazyLock<Regex>` patterns plus one literal `starts_with` check
- `crates/rskim/src/cmd/build/gradle.rs` — Gradle/Gradlew: task outcome + Java/Kotlin diagnostic Tier 1 (with duration), noise-strip Tier 2 (six `LazyLock<Regex>` patterns)
- `crates/rskim/src/cmd/build/maven.rs` — Maven/Mvnw: `[ERROR]`/`[WARNING]` + build summary Tier 1 (with duration), noise-strip Tier 2 (two `LazyLock<Regex>` patterns, two duration formats)
- `crates/rskim/src/output/mod.rs` — `ParseResult<T>` enum definition and helpers (`is_full`, `is_degraded`, `is_passthrough`, `tier_name`, `content`, `into_content`, `emit_markers`); `strip_ansi` and `strip_ansi_cow` (zero-copy fast path: borrows when no ESC byte present); `to_json_envelope` (not used by build family); `elision_marker`, `elision_marker_unbounded` (#317 loud elision helpers); `OutputMode`, `clean`, `clean_with_mode`, `PassthroughTruncator`, `FilterTransparencyHeader`
- `crates/rskim/src/output/canonical.rs` — `BuildResult` (pre-rendered output); `TestResult` (with `context: Option<String>` safety net and `with_context` constructor, #317); `TestEntry`, `TestSummary`, `TestOutcome`; `GitResult`, `LintResult`, `DbResult`, `PkgResult`, `InfraResult`, `LogResult`, `DiffResult`, `FileResult`
- `crates/rskim/src/cmd/mod.rs` — coordination point: declares all submodules, re-exports public API; inline helpers: `user_has_flag`, `inject_flag_before_separator`, `extract_show_stats`, `extract_json_flag`, `extract_output_format`, `should_read_stdin` (stdin-eligible when empty args OR `args == ["run"]`), `read_bounded`, `read_stdin_bounded`, `MAX_STDIN_BYTES`; passthrough checks: `is_passthrough_mode`, `check_passthrough_str`, `check_passthrough_value`; resolvers: `resolve_cache_dir`, `skim_wrappers_dir`
- `crates/rskim/src/cmd/dispatch.rs` — `dispatch()`, `run_raw_passthrough()`, `run_inherited_passthrough()` (inherited-stdio path for daemon/streaming commands); `spawn_status_to_code(status: io::Result<ExitStatus>) -> u8` (pure exit-code mapper: ENOENT→127, other error→1, signal-kill→1, otherwise clamp to `[0,255]`; `pub(crate)` and independently unit-tested); per-tool dispatcher helpers
- `crates/rskim/src/cmd/execution.rs` — `OutputFormat`, `RunContext`, `ParsedCommandConfig<'_>` (with `expected_exit_codes`, `forward_stderr`, #317), `ToolRunConfig<'_>` (with `expected_exit_codes`, `forward_stderr`, #317), `run_tool<T>`, `run_parsed_command_with_mode`, `run_parsed_command_with_exit` (#317), `ExitDisposition`, `classify_exit` (#317), `format_analytics_label`, `combine_output`, `obtain_output`, `render_output<T>`, `record_and_report`, `passthrough_raw`
- `crates/rskim/src/cmd/security.rs` — `sanitize_for_display`, `scrub_db_args`, `scrub_infra_args`
- `crates/rskim/src/cmd/registry.rs` — `KNOWN_SUBCOMMANDS` (sorted, binary-searchable via `binary_search`), `is_known_subcommand`, `is_meta_subcommand`, `wrapper_targets`
- `crates/rskim/src/cmd/test_utils.rs` — standalone test helper module (compiled under `#[cfg(test)]` gate): `make_output`, `make_output_full`, `make_output_stderr`, `load_fixture` (with `Component::Normal` traversal guard); import as `crate::cmd::test_utils`; renamed from `test_support` in PR #126
- `crates/rskim/src/runner.rs` — `CommandRunner` (stateless unit struct, `#[derive(Default)]`), `CommandOutput`, `ChildGuard` (kill-on-drop RAII), `is_spawn_error`, `MAX_OUTPUT_BYTES` (64 MiB); no timeout, no `RunnerError::Timeout`; `run_with_node_fallback` / `run_with_env_node_fallback` for three-strategy Node.js tool resolution (PATH → `./node_modules/.bin/` → `npx --no-install`; not used by build parsers)
- `crates/rskim/src/cmd/rewrite/indefinite.rs` — `is_indefinite_command(tokens: &[&str]) -> bool`; program-aware daemon/streaming detection with env-var prefix stripping; consumed by the rewrite hook path and by `dispatch()`'s `run_inherited_passthrough` gate
- `crates/rskim/src/cmd/test/cargo.rs` — `run()` for `skim cargo test`; uses `run_parsed_command_with_exit` with `expected_exit_codes: &[101]`; `parse_failure_details` state machine for stable-toolchain `---- name stdout ----` blocks (#317)
- `crates/rskim/src/cmd/test/shared.rs` — `run_test_runner`, `scrape_failures`, `failure_context_body`, `emit_failure_context` (#317), `try_read_stdin`, `TestKind`, `ExitSource`, `ArgPreparation`, `TestRunnerConfig`
- `crates/rskim/tests/fixtures/cmd/build/` — fixture files: `cargo_build_fail.json`, `cargo_build_ok.json`, `clippy_fail.json`, `clippy_warnings.json`, `make_errors.txt`, `make_nothing.txt`, `make_recursive.txt`, `make_success.txt`, `make_warnings_only.txt`, `tsc_errors.txt`
- `crates/rskim/tests/fixtures/cmd/test/` — fixture files: `cargo_pass.json`, `cargo_fail.json`, `cargo_panic_char_boundary.txt` (regression for Addendum 2 char-boundary panic)

## Related

- `crates/rskim/src/output/mod.rs` — owns `ParseResult<T>`, the type returned by all three-tier parsers across the whole codebase (lint, test, infra, build); `emit_markers` debug gate; `elision_marker`/`elision_marker_unbounded` (#317)
- `crates/rskim/src/output/canonical.rs` — owns `BuildResult`, `TestResult` (with `context` safety net), `GitResult`, `LintResult`
- `crates/rskim/src/runner.rs` — `CommandRunner` (stateless, ADR-008), `CommandOutput`, `ChildGuard` (kill-on-drop), `is_spawn_error`; `run_with_node_fallback`/`run_with_env_node_fallback` (Node.js tool resolution; not used by build parsers); no internal timeout, no `RunnerError::Timeout`
- `crates/rskim/src/cmd/rewrite/indefinite.rs` — `is_indefinite_command`; guards daemon/streaming commands from being captured; consumed by `dispatch()` and the rewrite hook path
- `crates/rskim/src/cmd/lint/` — sibling module using the same three-tier pattern with `LintResult` instead of `BuildResult`; lint parsers use `run_tool<T>` (via `run_parsed_command_with_mode` in `execution.rs`) rather than `run_parsed_command`
- `crates/rskim/src/cmd/test/` — sibling module using the same three-tier pattern with `TestResult`; cargo.rs uses `run_parsed_command_with_exit` directly
- ADR-008: Remove internal subprocess timeout/duration caps; bound child-process lifetime with `ChildGuard` kill-on-drop instead of an arbitrary timeout
- ADR-001: Fix all noticed issues immediately regardless of scope — applies when adding a new build parser: fix any spotted inconsistencies in other parsers in the same PR rather than deferring
