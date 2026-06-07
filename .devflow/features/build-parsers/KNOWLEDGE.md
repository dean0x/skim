---
feature: build-parsers
name: Build Tool Output Parsers
description: "Use when adding a new build tool parser, modifying cargo/tsc/make/gradle/maven compression, or debugging three-tier parse degradation for build commands. Keywords: build, cargo, tsc, make, gradle, maven, clippy, ParseResult, BuildResult, three-tier, NDJSON, flag injection, run_check, run_fmt, parse_fmt."
category: component-patterns
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
  - crates/rskim/src/cmd/execution.rs
  - crates/rskim/src/cmd/dispatch.rs
  - crates/rskim/src/cmd/security.rs
  - crates/rskim/src/cmd/registry.rs
  - crates/rskim/src/cmd/test_utils.rs
  - crates/rskim/src/runner.rs
created: 2026-05-14
updated: 2026-06-07
version: 11
---

# Build Tool Output Parsers

## Overview

The build parsers module (`crates/rskim/src/cmd/build/`) compresses output from build tools (cargo, clippy, tsc, make, gradle, maven) into compact summaries for AI context windows. Each tool has a dedicated file (`cargo.rs`, `tsc.rs`, `make.rs`, `gradle.rs`, `maven.rs`) plus a shared `mod.rs` that provides the dispatcher and the `run_parsed_command` infrastructure function used by all five.

The module is invoked via flat dispatch (`skim tsc`) or multi-category dispatch (`skim cargo build`, `skim cargo clippy`). All parsers share the same three-tier degradation model: Full (clean structured parse) → Degraded (partial parse with warning markers) → Passthrough (raw output returned unchanged).

## cmd/mod.rs Refactor (PR #267)

`cmd/mod.rs` was refactored to reduce complexity. Functionality previously inline in `mod.rs` is now split across dedicated submodules:

- `cmd/dispatch.rs` — `dispatch()`, `run_raw_passthrough()`, and per-tool dispatcher helpers (`dispatch_cargo`, `dispatch_go`, `dispatch_swift`, `dispatch_dotnet`, `passthrough_subcmd`, `extract_subcmd`, `prepend_without`)
- `cmd/execution.rs` — `OutputFormat`, `RunContext`, `ParsedCommandConfig<'_>`, `ToolRunConfig<'_>`, `run_tool<T>`, `run_parsed_command_with_mode`, `format_analytics_label`
- `cmd/security.rs` — `sanitize_for_display`, `scrub_db_args`, `scrub_infra_args`
- `cmd/registry.rs` — `KNOWN_SUBCOMMANDS` (sorted, binary-searchable), `is_known_subcommand`, `is_meta_subcommand`, `wrapper_targets`
- `cmd/test_utils.rs` — standalone `pub(crate)` module (compiled under `#[cfg(test)]` gate in `mod.rs`); canonical test helper source for all `cmd` subtree tests

`cmd/mod.rs` remains the coordination point (declares all submodules, re-exports public API) and still houses the inline helpers: `user_has_flag`, `inject_flag_before_separator`, `extract_show_stats`, `extract_json_flag`, `extract_output_format`, `combine_output`, `should_read_stdin`, `read_bounded`, `read_stdin_bounded`, `MAX_STDIN_BYTES`, passthrough checks, `resolve_cache_dir`, `obtain_output`, `render_output<T>`, `DEFAULT_CMD_TIMEOUT`.

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

The `compilation terminated.` check in make's Tier 2 is a literal `starts_with` check, not a regex — it is hardcoded alongside the four `LazyLock<Regex>` noise patterns.

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

## `run_parsed_command` — Shared Infrastructure

All five parsers call `super::run_parsed_command(...)` in `mod.rs` instead of spawning processes themselves. This function handles:
1. Command spawn with a 600-second timeout (compile times can be long)
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

Spawn failures (missing executable) use `anyhow::bail!` — a hard error with an install hint. This differs from the non-build path: `obtain_output` in `cmd/mod.rs` returns `Ok(None)` on spawn failure (soft fallback), which `run_parsed_command_with_mode` then converts to `Ok(ExitCode::FAILURE)`. Build commands have no stdin-passthrough path, so `ENOENT` is always fatal.

**Important**: build parsers use `run_parsed_command` (defined in `build/mod.rs`), not `run_parsed_command_with_mode` or `run_tool<T>` (both defined in `cmd/mod.rs`). The three are intentionally separate:
- Build: no `use_stdin`, no `--json` output mode, no `SKIM_PASSTHROUGH` bypass, no compressed-output hint on failure, plain `fn()` parser pointer, bail-on-spawn, 600s timeout
- Other families (lint, infra, db, file): use `run_tool<T>` (the generic runner added in #214) which wraps `run_parsed_command_with_mode`. `run_tool<T>` takes `ToolRunConfig<'a>` (program, env_overrides, install_hint, family, skip_ansi_strip, command_type), a `&RunContext`, a `prepare_args` closure, and a one-arg `parse_fn`

`run_tool<T>` in `cmd/mod.rs` explicitly documents this boundary: "build::run_parsed_command is intentionally not replaced: it has a different call shape (no `ctx: &RunContext`, different analytics path)." Switching build parsers to use `run_tool<T>` is not just a refactor — the signatures are incompatible.

The `ParsedCommandConfig` struct (in `cmd/mod.rs`) adds several fields not present in the build path: `family` (for analytics label disambiguation — prevents collision when `cargo` appears in both build and pkg), `skip_ansi_strip` (DB tools emit TSV; stripping would drop tab characters), and `output_format` (supports `--json` output mode). Build does none of these, which is why the two paths are separate rather than consolidated.

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

All build parser tests (and tests across the `cmd` subtree) use the shared helpers from `crate::cmd::test_utils`. This is now a **standalone file** (`cmd/test_utils.rs`) compiled under a `#[cfg(test)]` gate in `cmd/mod.rs` — it is NOT an inline `mod test_utils { ... }` block. As of #214, the ~34 local `make_output` definitions and ~41 local `load_fixture` definitions across `cmd` subtree modules were replaced by this single canonical source.

The four helpers available:
- `make_output(stdout: &str) -> CommandOutput` — success case: stderr empty, exit_code Some(0), duration ZERO
- `make_output_full(stdout, stderr, exit_code: Option<i32>) -> CommandOutput` — full control for non-zero exits, stderr content, signal-kill (None exit code)
- `make_output_stderr(stderr: &str) -> CommandOutput` — all output on stderr, exit_code Some(0); for tools that default to stderr (wget, curl)
- `load_fixture(subdir: &str, name: &str) -> String` — loads from `tests/fixtures/cmd/{subdir}/{name}`; panics with clear message on missing file

Build parser tests import as `use crate::cmd::test_utils::{make_output_full, load_fixture}`. Do not redeclare local versions — use the canonical source to prevent drift.

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

- **Using `super::super::test_utils` import path**: the module is now a standalone file `cmd/test_utils.rs`, not an inline block. The canonical import path is `crate::cmd::test_utils::{...}` for any module in the `cmd` subtree.

## Gotchas

- **Degradation markers and parse notices are silent by default**: `emit_markers` in `ParseResult` only writes to stderr when `SKIM_DEBUG=1` or `--debug` is set. In normal operation, Tier 2 (`Degraded`) and Tier 3 (`Passthrough`) passes are silent — no `[skim:warning]` or `[skim:notice]` lines appear. This means a build that degrades to regex fallback gives no on-screen indication unless debug mode is active. Enable `SKIM_DEBUG=1` when diagnosing unexpected parse behavior.

- **`--message-format` injection must happen before `--`**: `inject_flag_before_separator` places the flag before `--`. If it were appended after `--`, cargo would treat it as an argument to the compiled binary, not to cargo itself.

- **Cargo warning codes are grouped into `error_messages`, not a separate field**: clippy warning codes (`dead_code: 2 occurrence(s)`) are appended to `error_messages` in `BuildResult`. They are not rendered on a successful clippy run.

- **Make combines stdout and stderr**: make sends compiler diagnostics to whichever stream the child compiler uses. Always combine both streams before parsing.

- **`build-finished` is required for Tier 1 in cargo**: `try_tier1_json` returns `None` (falls through to Tier 2) if no `build-finished` NDJSON line appears. This means an incomplete JSON stream — e.g., a killed cargo process — degrades gracefully rather than reporting false success.

- **`note` severity in GCC diagnostics is not counted as an error**: `try_tier1_diagnostics` in make.rs counts `error` and `fatal error` as errors, `warning` as warnings, and skips `note`. Notes are still included in `error_messages` for context.

- **Make noop-after-errors must not discard accumulated diagnostics**: The `MAKE_NOOP_RE` check in `try_tier1_diagnostics` is guarded by `!any_match`. A "Nothing to be done" or "is up to date" line triggers an immediate success return ONLY when no diagnostic lines have been accumulated yet.

- **Signal-killed process (exit code `None`) in empty-output path is failure**: The empty-output early return uses `output.exit_code == Some(0)` for success, not `!= Some(1)`. A signal-killed process has `exit_code: None` — that must map to `success = false`.

- **Timeout is 600 seconds (10 minutes)**: build commands use a longer timeout than the default 300-second `DEFAULT_CMD_TIMEOUT` because compile times can be substantial.

- **Gradle success requires both zero exit code AND absence of `BUILD FAILED` text**: a process can exit 0 while gradle still emits `BUILD FAILED` in certain edge cases. Both conditions are checked in Tier 1.

- **Maven success requires `BUILD SUCCESS` text, not just zero exit code**: unlike most tools, maven's Tier 1 explicitly checks that `MAVEN_SUCCESS_RE.is_match(combined)` is true. A clean exit without the success marker sets `success = false`.

- **Gradle duration parsing only handles `X.XXX secs` format**: the `parse_gradle_duration` function splits on whitespace and parses the first token as `f64`. Multi-part durations like `1 min 2 secs` return `None` and `duration_ms` is left as `None` in `BuildResult`.

- **tsc empty-output check is placed after both tier 1 and tier 2**: unlike make.rs (which guards empty output at the top of `parse_make`), tsc.rs checks for empty output only after tier 1 and tier 2 both return `None`.

## Key Files

- `crates/rskim/src/cmd/build/mod.rs` — dispatcher (`run`), shared `run_parsed_command`, and `print_help`
- `crates/rskim/src/cmd/build/cargo.rs` — cargo build/check/clippy: `run`/`run_check`/`run_clippy` all delegate to `run_with_json_format` (NDJSON Tier 1, regex Tier 2); `run_fmt` uses a separate two-tier `parse_fmt` (empty → Full, non-empty → Passthrough)
- `crates/rskim/src/cmd/build/tsc.rs` — TypeScript compiler: regex-on-stderr Tier 1, combined Tier 2, empty-output success (checked after both tiers)
- `crates/rskim/src/cmd/build/make.rs` — GNU make: GCC diagnostics Tier 1, noise-strip Tier 2, eight `LazyLock<Regex>` patterns plus one literal `starts_with` check
- `crates/rskim/src/cmd/build/gradle.rs` — Gradle/Gradlew: task outcome + Java/Kotlin diagnostic Tier 1 (with duration), noise-strip Tier 2 (six `LazyLock<Regex>` patterns)
- `crates/rskim/src/cmd/build/maven.rs` — Maven/Mvnw: `[ERROR]`/`[WARNING]` + build summary Tier 1 (with duration), noise-strip Tier 2 (two `LazyLock<Regex>` patterns, two duration formats)
- `crates/rskim/src/output/mod.rs` — `ParseResult<T>` enum definition and helpers (`is_full`, `is_degraded`, `tier_name`, `content`, `into_content`, `emit_markers`); `strip_ansi` and `strip_ansi_cow` (zero-copy fast path: borrows when no ESC byte present); `to_json_envelope` (not used by build family — build has no `--json` mode); `OutputMode`, `clean`, `PassthroughTruncator`, `FilterTransparencyHeader` (used by other families); sub-modules `guardrail` and `tee` (not used by build parsers — used by other consumers of the output infrastructure)
- `crates/rskim/src/output/canonical.rs` — `BuildResult` struct with pre-rendered output; also owns `TestResult`, `GitResult`, `LintResult`, `DbResult`, `PkgResult` — the `DbResult::render` was decomposed into 5 private helpers in #214 as a model for growing render logic
- `crates/rskim/src/cmd/mod.rs` — coordination point: declares all submodules, re-exports public API; inline helpers: `user_has_flag`, `inject_flag_before_separator`, `extract_show_stats`, `extract_json_flag`, `extract_output_format`, `combine_output`, `should_read_stdin`, `read_bounded`, `read_stdin_bounded`, `MAX_STDIN_BYTES`; passthrough checks: `is_passthrough_mode`, `check_passthrough_str`, `check_passthrough_value`; resolver: `resolve_cache_dir`; private: `obtain_output` (soft spawn-failure path for non-build families), `render_output<T>`, `DEFAULT_CMD_TIMEOUT`
- `crates/rskim/src/cmd/dispatch.rs` — `dispatch()`, `run_raw_passthrough()`, per-tool dispatcher helpers (`dispatch_cargo`, `dispatch_go`, `dispatch_swift`, `dispatch_dotnet`, `passthrough_subcmd`, `extract_subcmd`, `prepend_without`)
- `crates/rskim/src/cmd/execution.rs` — `OutputFormat`, `RunContext`, `ParsedCommandConfig<'_>`, `ToolRunConfig<'_>`, `run_tool<T>`, `run_parsed_command_with_mode`, `format_analytics_label` (NOT used by build family — build uses `build/mod.rs::run_parsed_command`)
- `crates/rskim/src/cmd/security.rs` — `sanitize_for_display`, `scrub_db_args`, `scrub_infra_args`
- `crates/rskim/src/cmd/registry.rs` — `KNOWN_SUBCOMMANDS` (sorted, binary-searchable via `binary_search`), `is_known_subcommand`, `is_meta_subcommand`, `wrapper_targets`
- `crates/rskim/src/cmd/test_utils.rs` — standalone test helper module (compiled under `#[cfg(test)]` gate): `make_output`, `make_output_full`, `make_output_stderr`, `load_fixture`; import as `crate::cmd::test_utils`

## Related

- `crates/rskim/src/output/mod.rs` — owns `ParseResult<T>`, the type returned by all three-tier parsers across the whole codebase (lint, test, infra, build); `emit_markers` debug gate is defined here
- `crates/rskim/src/output/canonical.rs` — owns `BuildResult`, `TestResult`, `GitResult`, `LintResult`; build parsers use `BuildResult`
- `crates/rskim/src/runner.rs` — `CommandRunner`, `CommandOutput`, `is_spawn_error` — the execution layer called by `run_parsed_command`
- `crates/rskim/src/cmd/lint/` — sibling module using the same three-tier pattern with `LintResult` instead of `BuildResult`; lint parsers use `run_tool<T>` (via `run_parsed_command_with_mode` in `execution.rs`) rather than `run_parsed_command`
- `crates/rskim/src/cmd/test/` — sibling module using the same three-tier pattern with `TestResult`
- ADR-001: Fix all noticed issues immediately regardless of scope — applies when adding a new build parser: fix any spotted inconsistencies in other parsers in the same PR rather than deferring
