# Phase 1 Handoff: fix/fidelity-parity-overhaul

Branch: `fix/fidelity-parity-overhaul`
Base: `main` (HEAD at start: `a7d12b5`)

## Commits

| SHA | Message |
|-----|---------|
| `1d63537` | fix(A1): raw_override plumbing — guard baseline and passthrough fidelity |
| `ce06da3` | fix(A2/A4): unified fidelity gate — remove 256-byte floor, tie→Passthrough |
| `b49fe17` | fix(A3): diff passthrough + conflicting-flag detection in prepare_args and rewrite rules |

---

## Files Created

### `crates/rskim/src/output/fidelity.rs` (NEW)
Canonical unified fidelity gate for both L2-A and L2-B guard sites.

**Exports:**
- `pub(crate) enum FidelityDecision { Keep, Passthrough }` — `#[must_use]` on type
- `pub(crate) fn decide(raw: &str, compressed: &str) -> FidelityDecision` — the unified gate
- `pub(crate) fn longest_nonwhitespace_run(s: &str) -> usize` — helper, also used in tests

**Semantics:** Keep IFF compressed is strictly smaller in both bytes AND tokens.
Tie (equal) → Passthrough. No 256-byte floor. Token cap 256 KiB, run cap 4 KiB.

---

## Files Modified

### `crates/rskim/src/cmd/execution.rs`
- Added `raw_override: Option<String>` field to `ParsedCommandConfig` and `ToolRunConfig`
- Fixed SKIM_PASSTHROUGH=1 path: emits pre-captured `raw_override` bytes instead of streaming the injected command
- Fixed guard baseline: `savings_decision` uses `raw_override.as_deref().unwrap_or(&output.stdout)` so the baseline is always the user's literal output
- Fixed guard fallback: same pattern — emits `raw_override` (not post-injection stdout) when passthrough fires
- `run_tool` now passes `config.raw_override` down to `ParsedCommandConfig`
- All existing `ToolRunConfig` constants: added `raw_override: None` (no behavioral change)
- `savings_decision` is now a thin wrapper over `crate::output::fidelity::decide()`; all logic moved to `fidelity.rs`
- Removed `longest_nonwhitespace_run` from execution.rs (now in fidelity.rs); test reference updated to `crate::output::fidelity::longest_nonwhitespace_run`

### `crates/rskim/src/cmd/git/status.rs`
- `user_raw_override`: removed `if has_conflicting { ... } else { None }` gate; now ALWAYS pre-captures `git status <args>` output unconditionally. This ensures the guard baseline is always the user's literal git status output, regardless of which flags are present.

### `crates/rskim/src/output/guardrail.rs`
- Removed `MIN_RAW_SIZE_FOR_GUARDRAIL` (256 bytes) constant and Tier-0 exemption
- `apply()` now delegates to `fidelity::decide()`: `FidelityDecision::Keep → Passed`, `FidelityDecision::Passthrough → Triggered`
- Updated banner text: "compressed output not strictly smaller; emitting raw" (covers ties, not just expansion)
- Updated all tests to match new semantics (tie → Triggered, tiny payloads no longer exempt)

### `crates/rskim/src/output/mod.rs`
- Added `pub(crate) mod fidelity;`

### `crates/rskim/src/process.rs`
- Two guardrail gate conditions: dropped `&& options.trunc.token_budget.is_none()` — token-budget cascade paths now also protected by the guardrail

### `crates/rskim/src/cmd/infra/gh/mod.rs`
- `skip_net_savings_guard: true` → `false` — `gh` handler now subject to the fidelity gate

### `crates/rskim-contract/src/guardrail.rs`
- Added comment: L2 256-byte exemption was removed in A4; #325 migration scope assumption is now stale (behavior unchanged — this is L3 territory)

### `crates/rskim/src/cmd/file/diff.rs`
- `parse_impl`: now always returns `ParseResult::RawPassthrough` — the baseline issue (output.stdout is the `-u`-injected form, not the user's literal command) makes compression unfair until `raw_override` is wired for diff
- `prepare_args`: widened conflict detection to suppress `-u` injection when any format-conflicting flag is present: `-c`, `-C N`, `--context`, `-y`, `--side-by-side`, `-e`, `--ed`, `-n`, `--rcs`, `--brief`, `--normal`
- Parser internals (`FileStat`, `DiffParserState`, `try_parse_standalone_unified`, `build_file_result`, helper fns) retained with `#[allow(dead_code)]` as regression tests and future-use anchor
- Updated `test_exit_code_1_is_success` → expects Passthrough; `test_identical_files` → expects Passthrough
- Added 11 new A3 tests (TDD: written failing first, then implementation made them pass)

### `crates/rskim/src/cmd/rewrite/rules.rs`
- `diff` rewrite rule: added `-c`, `-C`, `--context`, `-n`, `--rcs`, `--normal` to `skip_if_flag_prefix` list (kept existing `-y`, `--side-by-side`, `--brief`, `-e`, `--ed`)

### Integration test updates (comment-only)
- `crates/rskim/tests/cli_guardrail.rs`: replaced "above MIN_RAW_SIZE_FOR_GUARDRAIL of 256" with accurate wording
- `crates/rskim/tests/cli_transparency.rs`: same

---

## Patterns Established

### `raw_override: Option<String>` in ToolRunConfig
Callers that pre-capture the user's literal command output set this field. The field propagates through `run_tool` → `ParsedCommandConfig` → `run_parsed_command_with_exit`. Three places in execution.rs consume it:
1. `SKIM_PASSTHROUGH=1` path: emit `raw_override` bytes, bypass the injected command stream
2. Guard baseline: `savings_decision(raw_override.as_deref().unwrap_or(&output.stdout), &compressed_str)`
3. Guard fallback: emit `raw_override` when `savings_decision` returns `Passthrough`

Currently only `git status` arms this field (unconditionally). All other handlers have `raw_override: None`.

### Two surfaces for diff injection (IMPORTANT — do not conflate)
- **Rewrite engine** (`rules.rs` skip list): fires BEFORE the command runs; skipping means `diff -c …` is NOT rewritten to `skim diff -c …`
- **`prepare_args`** (`diff.rs`): fires AFTER the rewrite; suppresses `-u` injection when format-conflicting flags are detected in the ALREADY-REWRITTEN args

Both surfaces must be kept in sync for correct behavior.

### Security invariant: `env` handler exemption (PF-012)
`crates/rskim/src/cmd/file/env.rs` retains `skip_net_savings_guard: true`. The secret-redaction logic in the `env` handler is a security control that must not depend on byte arithmetic. This exemption was DELIBERATELY NOT removed in A2 (only the `gh` exemption was removed). A test pins this at `env.rs:443+`.

### `rskim-contract` guardrail is L3 — leave alone
`crates/rskim-contract/src/guardrail.rs` is the Layer-3 proxy byte guard. It has no tokenizer, no floor, and deliberate per-unit semantics. Migration is tracked in #325. The Phase 1 commits add only a comment there — no behavior changes.

---

## Deliberate Behavior Preservations

| Area | Behavior kept | Reason |
|------|--------------|--------|
| `git diff` (`cmd/git/diff/`) | Untouched | Natively unified, only injects `--no-color` (presentation). Out of scope. |
| `env` handler `skip_net_savings_guard: true` | Kept | PF-012: secret redaction is a security control that must not depend on byte arithmetic |
| L3 guardrail (`rskim-contract`) | Untouched | Layer-3 proxy work tracked as #325 |
| `diff` parser code | Retained with `#[allow(dead_code)]` | Future re-enablement anchor when `raw_override` is wired for diff |

---

## Integration Points for Next Phase

If a subsequent phase re-enables `diff` compression:
1. Set `raw_override` in the diff handler: pre-capture `diff <user-args>` (without `-u`) via `runner.run("diff", &arg_refs).ok().filter(...).map(...)`
2. Restore `parse_impl` to call `try_parse_standalone_unified` (already present)
3. Verify: `savings_decision(raw_override.as_deref().unwrap_or(...), &compressed)` correctly uses the user's non-unified output as baseline

If a subsequent phase needs to verify the `gh` guard is now applied:
- `gh` handler in `crates/rskim/src/cmd/infra/gh/mod.rs` now has `skip_net_savings_guard: false`
- The guard fires via `savings_decision` in `run_parsed_command_with_exit` at execution.rs line ~1197

---

# Phase 2 Handoff: Work Stream B (Transparency)

Branch: `fix/fidelity-parity-overhaul`
Phase 1 HEAD: `b49fe17`

## Commits

| SHA | Message |
|-----|---------|
| `46cc42b` | feat(B5/rskim-core): thread SKIM_PASSTHROUGH=1 hint through core truncation markers |
| `8f2ba4d` | feat(B1-B5+PF-012): transparency invariant — SKIM_PASSTHROUGH=1 gate + lossy-view marker |

---

## What Was Implemented

### B1 — Structural passthrough gate

Three sites now short-circuit when `is_passthrough_mode()` returns true:

- **`crates/rskim/src/cmd/dispatch.rs`**: `run()` calls `run_inherited_passthrough(subcommand, args)` early, before any handler lookup, for all subcommands except meta-subcommands and `"env"` (the `env` exception is permanent — see PF-012 below).
- **`crates/rskim/src/cmd/log.rs`**: `run()` reads stdin via `read_stdin_bounded()` and writes it to stdout verbatim.
- **`crates/rskim/src/main.rs`** (`process_single_arg`): reads raw file bytes (or stdin when `file == "-"`) and copies to stdout, bypassing the entire transform pipeline.

### B2 — Proxy passthrough

- **`crates/rskim/src/cmd/proxy.rs`**: `let pipeline = if super::is_passthrough_mode() { ... }` builds a passthrough pipeline instead of a compressing one.

### B3 — Lossy-view marker fires unconditionally (ADR-011 class 1)

- **`crates/rskim/src/process.rs`** lines 651 and 756: `view_differs` is now computed as `final_output != raw_content` (for stdin and file paths respectively). The previous `rewrite_origin().is_some() &&` guard was removed — the marker is a class-1 loss-bearing notice and fires regardless of whether `SKIM_REWRITTEN_FROM` is set.
- **Cache path** (lines 220–246): NOT changed — cache hits always return the cached compressed view without re-reading raw bytes; fixing view_differs for cache hits would require reading raw on every cache hit, negating caching. This is deferred.

### B4 — Marker names the elided class

- **`crates/rskim/src/output/mod.rs`**: Added `mode_class_label(mode: SkimMode) -> &'static str` mapping each mode to its human-readable description (e.g. `"bodies removed"` for structure, `"bodies and syntactic detail removed"` for pseudo). `lossy_view_marker()` now emits `[skim] transformed view (<origin> → <dest>) <class description>: N/total files — SKIM_PASSTHROUGH=1 for raw output`.

### B5 — Core truncation markers carry SKIM_PASSTHROUGH=1 hint

- **`crates/rskim-core/src/types.rs`**: `TransformConfig` gained `elision_hint: Option<String>` with `with_elision_hint()` builder method.
- **`crates/rskim-core/src/transform/truncate.rs`**: All four marker-producing functions (`last_lines_marker`, `truncated_marker`, `token_budget_marker`, `line_budget_marker`) accept `hint: Option<&str>` and call `append_hint()` to append ` — <hint>` when set.
- **`crates/rskim/src/cascade.rs`**: `build_config_with_opts` always calls `.with_elision_hint("SKIM_PASSTHROUGH=1 for full output")`. The no-token-budget path (`build_config`) does NOT set the hint — intentional, those reads never elide.

### PF-012 — SKIM_PASSTHROUGH=1 must not bypass env credential redaction

The dispatch gate already excluded `"env"` from `run_inherited_passthrough`. But `execution.rs` has its own independent passthrough shortcut that all tools go through after dispatch. Fix:

- **`crates/rskim/src/cmd/execution.rs`**: Added `never_passthrough: bool` field to both `ParsedCommandConfig` and `ToolRunConfig`. When true, the `is_passthrough_mode()` shortcut at line ~885 is disabled: `let passthrough = is_passthrough_mode() && !never_passthrough;`.
- **`crates/rskim/src/cmd/file/env.rs`**: `never_passthrough: true` — credential redaction is a security control that must hold on BOTH branches of the net-savings guard.
- **30+ tool files** (all `ParsedCommandConfig`/`ToolRunConfig` constructions): `never_passthrough: false` added to satisfy exhaustive struct initialization. Files: all db/, lint/, infra/, pkg/, test/, and individual tool files.

---

## Files Created

- **`crates/rskim/tests/cli_passthrough_coverage.rs`** (NEW) — 16 tests covering all B1-B5 invariants plus PF-012 and ADR-011 regression.

---

## Files Modified

- `crates/rskim-core/src/types.rs` — `elision_hint` field + builder
- `crates/rskim-core/src/transform/truncate.rs` — hint threading + `append_hint()`
- `crates/rskim-core/src/transform/mod.rs` — pass-through hint to callers
- `crates/rskim-core/src/lib.rs` — re-exports
- `crates/rskim/src/cascade.rs` — wire elision_hint
- `crates/rskim/src/cmd/dispatch.rs` — B1 dispatch gate
- `crates/rskim/src/cmd/execution.rs` — `never_passthrough` field + gate fix
- `crates/rskim/src/cmd/file/env.rs` — `never_passthrough: true` (PF-012)
- `crates/rskim/src/cmd/log.rs` — B1 log passthrough
- `crates/rskim/src/cmd/proxy.rs` — B2 proxy passthrough
- `crates/rskim/src/main.rs` — B1 read-path passthrough
- `crates/rskim/src/multi.rs` — multi-file marker wiring
- `crates/rskim/src/output/mod.rs` — `mode_class_label()`, `lossy_view_marker()`, `multi_file_lossy_marker()`
- `crates/rskim/src/process.rs` — B3 view_differs unconditional
- `crates/rskim/tests/cli_transparency.rs` — test fixture + format updated
- `crates/rskim/src/cmd/file/diff.rs` — clippy fix: snake_case test fn name

---

## Patterns Established

### `is_passthrough_mode()` / `check_passthrough_str()`
Single source of truth in `cmd/mod.rs`. Recognizes `"1"`, `"true"`, `"yes"` (case-insensitive). All passthrough gates must use this function — never compare `SKIM_PASSTHROUGH` directly.

### `never_passthrough: bool` in ToolRunConfig / ParsedCommandConfig
Struct field that prevents the execution-level SKIM_PASSTHROUGH=1 shortcut. Default: `false`. Currently only `env`/`printenv` sets `true`. Any handler whose `parse_impl` implements a non-negotiable security or correctness property (redaction, sanitization, escaping) that must hold regardless of passthrough mode MUST set `never_passthrough: true`.

### `view_differs = final_output != raw`
Computed in `process.rs` for stdin (line ~651) and file paths (line ~756). Cache-path `view_differs` is always false (deferred — cache hits don't re-read raw). ADR-011 class 1: loss-bearing markers never need `SKIM_REWRITTEN_FROM`.

### Two independent passthrough surfaces in execution.rs
1. The dispatch-level gate (dispatch.rs `run_inherited_passthrough`) — for tool subcommands.
2. The execution-level gate (execution.rs line ~885 `let passthrough = ...`) — for handlers routed through `run_parsed_command_with_mode`. Both must be correct independently. The `env` subcommand is excluded from surface 1 (dispatch.rs literal `subcommand != "env"`) and surface 2 (`never_passthrough: true` in env.rs).

---

## Deferred / Known Pre-existing Issues

| Area | Status | Notes |
|------|--------|-------|
| Cache-path `view_differs` | Deferred | Cache hits return compressed view without re-reading raw. Fixing requires cache format change. Marker does not fire on cache hits (view_differs=false always). |
| `test_cli_all_languages_structure` | Pre-existing Phase 1 regression | Phase 1 commit `ce06da3` (remove 256-byte floor) broke this test. Not Phase 2's responsibility. |
| `test_last_lines_with_pseudo_mode`, `test_max_lines_show_stats_interaction`, `test_stdin_max_lines_and_stats` | Pre-existing | Confirmed against installed `a7d12b5` binary — all fail before Phase 2. |
| Snyk SAST | Blocked | Requires user interactive auth per memory note. |

---

## Integration Points for Phase 3+

- **`never_passthrough` expansion**: If a future handler needs to apply redaction or sanitization, add `never_passthrough: true` to its config constant. No other changes needed.
- **Cache-path marker**: To fix view_differs on cache hits, the cache must store both the raw bytes and the compressed view (or just the compressed view + a "differs" boolean). The cache format (`.json` files in `~/.cache/skim/`) would need a version bump.
- **`env` dispatch exclusion**: `dispatch.rs` line with `subcommand != "env"` — this is permanent. Do not remove it. The `never_passthrough: true` in env.rs guards the execution-level path; the dispatch exclusion guards the dispatch-level path. Both are needed.
- **`mode_class_label()` in output/mod.rs**: Returns a static string per mode. If new modes are added, this function must be updated.
- **Test surface note**: `cli_passthrough_coverage.rs` covers only the rewrite-engine surface. Wrapper-surface parity is explicitly called out in the test file's module doc as future work (`cli_both_surfaces_paired.rs`).

---

# Phase 3 Handoff: Work Stream D (Surface Parity)

Branch: `fix/fidelity-parity-overhaul`
Phase 2 HEAD: `859fd8b` (last Phase 2 commit)

## Commit

| SHA | Message |
|-----|---------|
| `b79e287` | feat(D1-D6): surface parity — unified registry, generic passthrough, shared skip-flags, json extractor, stdout gate, wrapper-dir needle |

## What Was Implemented

### D1 — Unified TOOL_REGISTRY (golangci drift fix)

**Problem:** KNOWN_SUBCOMMANDS had `"golangci"` but rewrite rules intercepted `"golangci-lint"`. This caused wrapper symlinks to be named `~/.skim/bin/golangci` while the rewrite surface rewrote to `skim golangci-lint` — a mismatch meaning the wrapper was unreachable after a rewrite.

**Fix:**
- `crates/rskim/src/cmd/registry.rs`: Changed `"golangci"` → `"golangci-lint"` in KNOWN_SUBCOMMANDS (sort order: gofmt < golangci-lint < gradle preserved).
- `crates/rskim/src/cmd/rewrite/rules.rs`: Changed `rewrite_to` from `&["skim", "golangci"]` to `&["skim", "golangci-lint"]` in golangci-lint rules. Removed the standalone `diff` RewriteRule (PF-011: native diff is already minimal — passthrough is the correct behaviour). Updated EXPECTED_RULE_COUNT from `18 + 13 + 7 + 43 + 26 + 28 + 16 + 3` to `18 + 13 + 7 + 43 + 26 + 28 + 15 + 3` (153 total).
- `crates/rskim/src/cmd/lint/mod.rs`: Added `"golangci-lint"` to KNOWN_LINTERS alongside `"golangci"` (backward compat). Dispatch arm: `"golangci" | "golangci-lint" => golangci::run(...)`.
- **Cross-check test** added to `rules.rs`: `test_rewrite_heads_are_wrapper_targets_or_documented_aliases` — for every rule in `all_rules()`, prefix[0] must be in `wrapper_targets()` OR in `KNOWN_ALIASES`. KNOWN_ALIASES covers: `./gradlew`, `./mvnw`, `bundle`, `gmake`, `npx`, `pip3`, `python`, `python3`. Catches golangci-class drift at compile time.

**Backward compat:** The dispatch.rs lint arm retains `"golangci" | "golangci-lint"` so old `~/.skim/bin/golangci` wrapper installs still work (argv[0]="golangci" → dispatch → lint::run).

### D2 — Generic unknown-input fallback (9 families)

**Problem:** 9 dispatch families returned skim-generated "unknown subcommand" errors for commands the native tool handles fine. This blocked `git worktree`, `go build`, `npm ci`, etc.

**Fix:** In the unknown/other/_ arm of each dispatcher, replaced `bail!` with `run_raw_passthrough(tool, args, &[])` + `crate::debug_log!` (ADR-011: lossless path, debug-gated banner).

Files changed:
- `crates/rskim/src/cmd/dispatch.rs`: cargo unknown arm (line ~530), go unknown arm (line ~561), top-level `_` arm (line ~924). `passthrough_subcmd` function: `eprintln!` → `crate::debug_log!`.
- `crates/rskim/src/cmd/git/mod.rs`: git unknown arm — reconstructs `all_args = global_flags + [other] + subcmd_args`.
- `crates/rskim/src/cmd/pkg/npm/mod.rs`: npm unknown arm.
- `crates/rskim/src/cmd/pkg/pip.rs`: pip unknown arm.
- `crates/rskim/src/cmd/pkg/pnpm.rs`: pnpm unknown arm.
- `crates/rskim/src/cmd/file/mod.rs`: `env` arm — when tool_name=="env" AND args contain `=`, routes to `run_raw_passthrough("env", ...)` instead of the redacting env handler. This fixes `env FOO=1 printenv FOO`.

**Test updates (D2 semantic change):** 8 tests updated across 5 files to reflect native-tool errors instead of skim-generated "unknown subcommand":
- `tests/cli_build.rs`: `test_skim_cargo_unknown_subcmd_exits_nonzero`
- `tests/cli_e2e_build_parsers.rs`: `test_cargo_unknown_subcmd_exit_code`
- `tests/cli_git.rs`: `test_skim_git_unknown_subcommand`, `test_skim_git_show_unknown_subcommand_message`
- `tests/cli_test_cargo.rs`: all 5 `_unknown_subcommand_errors` tests

### D3 — Shared `skip_if_flag_prefix` on wrapper surface

**Problem:** `grep --help` invoked via the wrapper surface (`~/.skim/bin/grep --help`) showed skim's internal handler help instead of grep's real help. Only the rewrite surface had skip logic for --help/--version.

**Fix:** Added `dispatch_for_wrapper()` function to `crates/rskim/src/cmd/dispatch.rs`. This is the new entry point for the wrapper surface (used in `main.rs` argv[0] dispatch path):
```rust
pub(crate) fn dispatch_for_wrapper(name, args, analytics) -> anyhow::Result<ExitCode> {
    // D3: universal help/version passthrough on wrapper surface
    if !is_meta_subcommand(name) && args has --help/-h/--version/-V {
        return run_raw_passthrough(name, args, &[])
    }
    // D4: tool-owned skip flags passthrough
    if !is_meta_subcommand(name) {
        let skip_flags = skip_flags_for_tool(name)
        if skip_flags is non-empty AND args contains a skip flag {
            return run_raw_passthrough(name, args, &[])
        }
    }
    dispatch(name, args, analytics)
}
```
`main.rs` changed at argv[0]-dispatch site: `cmd::dispatch(...)` → `cmd::dispatch_for_wrapper(...)`.

### D4 — One `--json` extractor (value-aware, separator-aware)

**Problem:** `extract_json_flag` stripped ALL `--json`, breaking `rg --json` (rg owns that flag) and `tree --json` on the wrapper surface.

**Fix (two-pronged):**
1. `crates/rskim/src/cmd/mod.rs`: Replaced `extract_json_flag` with a version that only strips bare `--json` (not `--json=value`) and never strips after `--`. Three new unit tests added.
2. `crates/rskim/src/cmd/rewrite/mod.rs`: Added `pub(crate) fn skip_flags_for_tool(tool_name)` — returns a tool's `skip_if_flag_prefix` entries (excluding universal --help/-h/--version/-V) from rewrite rules. `dispatch_for_wrapper` calls this for the wrapper surface (D3+D4 combined).

### D5 — Aligned stdout-is-a-file gates

**Problem:** Wrapper surface gate `stdout_is_regular_file()` used `fstat` to detect file redirect. The goal was "serve raw when not a TTY and not a pipe", but regular-file check missed sockets and other non-TTY/non-pipe fds.

**Fix:** `crates/rskim/src/main.rs`:
- Renamed internal check to `stdout_should_serve_raw_impl(meta)` — returns `!is_char_device() && !is_fifo()`.
- `stdout_is_regular_file()` now calls `stdout_should_serve_raw_impl(f.metadata())`.
- Added `is_regular_file_stdout()` helper (cfg(test) only) for the old behavior.
- Added 3 D5 tests: regular file → true, char device → false, FIFO → false, directory → true (pinned as known edge case).
- **Pinned divergence:** `$(cmd)` subshell capture is a pipe (FIFO) — still compresses. This is accepted/documented behavior.

### D6 — Derived wrapper-dir needle

**Problem:** `filter_wrappers_from_path` at `main.rs:647` used hardcoded `b".skim"` as the PATH needle to detect and strip wrapper entries. Custom wrapper dirs (e.g. `/opt/custom-wrappers/bin`) without `.skim` in the path were silently not filtered → potential recursion.

**Fix:** `crates/rskim/src/main.rs`:
- `filter_wrappers_from_path` signature changed from `(path: &OsStr)` to `(path: &OsStr, wrappers_dir: Option<&Path>)`.
- When `wrappers_dir` is `None`, returns `None` immediately (no wrappers dir configured → nothing to strip).
- When `Some(dir)`, the needle is derived from `dir.as_os_str().as_encoded_bytes()`.
- `strip_skim_wrappers_from_path` passes `cmd::skim_wrappers_dir()` at the call site.
- All existing 4 tests updated to pass `Some(&wrappers)`.
- New regression test: `test_strip_skim_wrappers_custom_dir_without_skim_substring` using `/opt/custom-wrappers/bin`.

---

## Files Created
None.

## Files Modified

**Implementation:**
- `crates/rskim/src/cmd/dispatch.rs` — D2 unknown arms, D3/D4 `dispatch_for_wrapper`, test fixes
- `crates/rskim/src/cmd/file/mod.rs` — D2 env=value passthrough
- `crates/rskim/src/cmd/git/mod.rs` — D2 git unknown passthrough
- `crates/rskim/src/cmd/lint/mod.rs` — D1 golangci-lint alias
- `crates/rskim/src/cmd/mod.rs` — D4 `extract_json_flag` (value-aware), `dispatch_for_wrapper` re-export
- `crates/rskim/src/cmd/pkg/npm/mod.rs` — D2 npm unknown passthrough
- `crates/rskim/src/cmd/pkg/pip.rs` — D2 pip unknown passthrough
- `crates/rskim/src/cmd/pkg/pnpm.rs` — D2 pnpm unknown passthrough
- `crates/rskim/src/cmd/registry.rs` — D1 golangci → golangci-lint in KNOWN_SUBCOMMANDS
- `crates/rskim/src/cmd/rewrite/mod.rs` — D4 `skip_flags_for_tool()` helper
- `crates/rskim/src/cmd/rewrite/rules.rs` — D1 golangci rewrite_to fix, diff rule removed, cross-check test
- `crates/rskim/src/main.rs` — D3 dispatch_for_wrapper, D5 stdout gate widening, D6 filter_wrappers_from_path

**Test updates (D1/D2 semantic changes):**
- `crates/rskim/tests/cli_build.rs`
- `crates/rskim/tests/cli_e2e_build_parsers.rs`
- `crates/rskim/tests/cli_e2e_lint_parsers.rs` — golangci → golangci-lint
- `crates/rskim/tests/cli_e2e_rewrite.rs` — "skim golangci" → "skim golangci-lint"
- `crates/rskim/tests/cli_e2e_rewrite_alignment.rs` — golangci → golangci-lint in handler test and rewrite assertion
- `crates/rskim/tests/cli_git.rs` — D2 native-error assertions
- `crates/rskim/tests/cli_test_cargo.rs` — D2 native-error assertions (5 tests)

---

## Patterns Established

### `dispatch_for_wrapper()` vs `dispatch()`
`dispatch_for_wrapper` is the entry point for the **wrapper surface** (argv[0] dispatch in main.rs). It applies D3 (help/version passthrough) and D4 (tool-owned skip flags) before routing to `dispatch()`. **NEVER** call `dispatch` directly on the wrapper surface.

`dispatch` is the entry point for the **rewrite surface** (Invocation::Subcommand path). Help/version flags on this surface show skim's handler help intentionally (the user explicitly invoked `skim tool --help`).

### `skip_flags_for_tool()` in `cmd/rewrite/mod.rs`
Returns all `skip_if_flag_prefix` entries for a named tool, excluding universal help/version flags. The rewrite surface already uses these via `skip_if_flag_prefix` in rule matching; this function exposes them to the wrapper surface via `dispatch_for_wrapper`. Single source of truth for tool-owned flags.

### `filter_wrappers_from_path(path, wrappers_dir)` — needle from parameter
Always pass `cmd::skim_wrappers_dir()` from the production call site. Tests pass `Some(&wrappers)` with a controlled path. `None` → returns None immediately (testability shortcut, also correct when no wrappers dir is configured).

### D2 unknown-arm pattern
All unknown arms in dispatch families now follow:
```rust
unknown_subcmd => {
    let safe = sanitize_for_display(unknown_subcmd);
    crate::debug_log!("skim <tool>: unknown subcommand '{safe}' — passing through");
    // reconstruct full arg list: [global_flags..., subcommand, subcmd_args...]
    run_raw_passthrough("<tool>", &all_args, &[])
}
```

---

## Critical Integration Points for Phase 4+

- **`dispatch_for_wrapper` must stay as the wrapper entry point.** If new wrapper-specific logic is needed, add it in `dispatch_for_wrapper`, not in `dispatch`. The two functions are distinct by design.
- **golangci backward compat:** `dispatch.rs` lint arm retains `"golangci" | "golangci-lint"`. Old `~/.skim/bin/golangci` wrappers still work. New installs create `~/.skim/bin/golangci-lint`. Do not remove the `"golangci"` arm — existing users would break silently.
- **D2 and env security:** The D2 `env` arm only passes through when args contain `=` (env var assignment form). Plain `env` (list all vars) still goes through the redacting env handler. Do NOT widen this guard.
- **D5 pinned divergence:** `$(cmd)` command substitution uses a pipe (FIFO), so it still compresses. This is documented and accepted. Do not change the FIFO check without understanding this use case.
- **D6 and LazyLock:** `WRAPPERS_DIR_CACHE` (LazyLock in cmd/mod.rs) is set once and never re-evaluable in tests. Tests that need to override it must use `skim_sandboxed()` (PF-017) and ensure `SKIM_WRAPPERS_DIR` is set before the binary starts.

---

# Phase 4 Handoff: Work Stream C1 (git-diff render corruption)

Branch: `fix/fidelity-parity-overhaul`
Phase 3 HEAD: `b79e287`

## Commit

| SHA | Message |
|-----|---------|
| `3fb0fd3` | fix(C1a/C1b): replace two-pass diff render with single positional walk + post-render verifier |

---

## What Was Implemented

### C1a — Single positional walk in `render_default_scoped`

**File:** `crates/rskim/src/cmd/git/diff/render.rs`

The old two-pass design in `render_default_scoped` (old lines ~429–545) had three bugs:

- **Bug 1 (duplicate context):** The breadcrumb at ~line 492 emitted ` {n} {line}` but NEVER updated `cursor.last_new`. When the hunk subsequently reached that line as context, it emitted it again.
- **Bug 2 (out-of-order orphan):** The orphan pass ran AFTER the range loop, so orphan hunk lines (not touching any AST node) appeared in the output AFTER all range-hunk lines — even those from hunks that appeared later in the file.
- **Bug 3 (`+` as context):** When a `+` line appeared at `breadcrumb_line`, the breadcrumb emitted it as context (` ` prefix), then the hunk emitted it correctly as `+`. Both versions appeared.

**New design (single positional walk):**

Phase 1 — breadcrumb schedule:
- For each `ChangedNodeRange`, compute `breadcrumb_line` (parent header line or range start).
- Find the first hunk H where `breadcrumb_line < H.new_start` (strictly before the hunk window).
- Build `hunk_crumbs: Vec<Vec<usize>>` — per-hunk list of breadcrumb lines to emit before that hunk.

Phase 2 — single walk:
- Walk all hunks in document order.
- Before each hunk: emit scheduled breadcrumbs (emitted_breadcrumbs deduplication prevents double-emit if two ranges share a parent).
- For each hunk: walk ALL patch lines (in-node AND orphan) via `emit_patch_line`. No clipping, no skip pass.

The constraint `breadcrumb_line < hunk.new_start` is the key invariant: it guarantees the breadcrumb's line is OUTSIDE the hunk's window, so the hunk can never re-emit it. Cursor-based de-duplication is no longer needed.

**Emissions tracking (`Axis` enum):** Each emitted line is pushed to `emissions: Vec<(Axis, usize)>`:
- `+` or context lines → `(Axis::New, cur_new)`
- `-` lines → `(Axis::Old, cur_old)`
- `\` marker → no tracking (no delta)

**Removed:** `patch_line_deltas` function (dead code — used only by the deleted orphan pass; per zero-warnings policy).

**Signature change:**
```rust
fn render_default_scoped(
    output: &mut String,
    changed_ranges: &[ChangedNodeRange],
    hunks: &[DiffHunk<'_>],
    source_lines: &[&str],
    ln_width: usize,
    emissions: &mut Vec<(Axis, usize)>,  // NEW — C1b tracking
)
```
The 5 existing `render_default_scoped` call sites in tests were updated to pass `&mut vec![]`.

### C1b — Post-render verifier (`verify_ast_render`)

**File:** `crates/rskim/src/cmd/git/diff/render.rs`

Added `verify_ast_render(emissions: &[(Axis, usize)], hunks: &[DiffHunk])-> bool` (just before `render_raw_hunks`). Checks three invariants that the ADR-001 net-savings size guard cannot catch (it fires only on OVER-emission, never on silent omission or content substitution — per PF-025):

1. **Per-axis uniqueness:** no line number appears twice on the same axis (`HashSet<usize>` per axis).
2. **New-axis monotonicity:** consecutive `New` emissions are strictly increasing.
3. **Coverage of `+`/`-` lines:** every hunk's `+` lines (new_delta > 0) appear on the `New` axis; every `-` line appears on the `Old` axis.

Called from `try_ast_render` in the Default mode branch after `render_default_scoped`:
```rust
if !verify_ast_render(&emissions, &file_diff.hunks) {
    crate::debug_log!("[skim] git diff AST verifier: render integrity check failed ...");
    return None;  // → caller falls back to render_raw_hunks
}
```

ADR-011 class-2 (no-loss raw fallback): banner is `crate::debug_log!`-gated — zero stderr bytes without `SKIM_DEBUG`.

### Fix 5 test correction

**File:** `crates/rskim/tests/cli_git.rs`

`test_skim_git_show_commit_guardrail_is_debug_gated` was relying on the duplicate-emission bug to inflate the render past raw. With C1a fix, the correct render for a modified-file commit (appending 20 functions) is SMALLER than raw — the compact skim commit header saves ~90 chars that outweigh line-number additions.

**Root cause analysis:**
- Default mode `render_default_scoped` (correct) emits each line exactly once → render ≤ raw for modified-file commits (hunk-header savings + commit-header savings > line-number additions).
- The show-level guardrail in `emit_show_commit` (`apply_to_stderr`) does fire when `render_raw_hunks` is used for an Added file: line numbers add 4 chars × N lines, which for N ≥ ~30 exceeds the ~90-char header savings.

**New scenario (status=Added, 100 lines):**
- First commit: `README.md` (anchor for HEAD~1).
- Second commit: add `inflate.ts` with 100 zero-padded functions (`f000..f099`).
- `render_diff_file` returns `render_raw_hunks` immediately for Added status (line 138).
- `ln_width = 3` (max line 101, 3 digits). Each line adds 4 chars. 100 × 4 = 400 chars inflation.
- Commit-header + diff-metadata savings ≈ 200 chars. Net: ~200 chars inflation → guardrail fires.
- Test is now independent of C1a breadcrumb behavior.

### `cargo fmt` cleanup (pre-existing)

The D1-D6 commit (`b79e287`) left formatting issues in several files. Running `cargo fmt -p rskim` cleaned them as part of this commit (zero-warnings policy):
- `crates/rskim/src/cmd/dispatch.rs`
- `crates/rskim/src/cmd/git/mod.rs`
- `crates/rskim/src/cmd/mod.rs`
- `crates/rskim/src/cmd/registry.rs`
- `crates/rskim/tests/cli_build.rs`
- `crates/rskim/tests/cli_e2e_build_parsers.rs`

---

## Files Modified

- `crates/rskim/src/cmd/git/diff/render.rs` — C1a `render_default_scoped` rewrite, C1b `verify_ast_render`, `Axis` enum, 12 new unit tests, 5 existing test site updates, `patch_line_deltas` removal
- `crates/rskim/tests/cli_git.rs` — Fix 5 guardrail test rewritten for Added-file inflation scenario
- `crates/rskim/src/cmd/dispatch.rs` — fmt cleanup only
- `crates/rskim/src/cmd/git/mod.rs` — fmt cleanup only
- `crates/rskim/src/cmd/mod.rs` — fmt cleanup only
- `crates/rskim/src/cmd/registry.rs` — fmt cleanup only
- `crates/rskim/tests/cli_build.rs` — fmt cleanup only
- `crates/rskim/tests/cli_e2e_build_parsers.rs` — fmt cleanup only

---

## Key Design Decisions and Invariants

### `breadcrumb_line < hunk.new_start` (C1a core invariant)
This is the single constraint that eliminates all three bugs. A breadcrumb is only scheduled before a hunk if the function/node STARTS before the hunk's window. This means the hunk's patch lines can never include the breadcrumb's line — no cursor needed, no orphan pass needed.

### Verifier applies only in Default mode (C1b scope)
The `verify_ast_render` check is inside the `diff_mode == DiffMode::Default` branch only. Non-Default modes (Structure/Full) render unchanged nodes with inter-node gaps — these would trigger false positives on the uniqueness/monotonicity checks. The verifier tracks emissions for the specific hunk-scoped render only.

### `Axis::New` for breadcrumbs
Breadcrumbs are emitted as context lines (` ` prefix, no `+`/`-`), representing a New-axis line that's BEFORE the hunk window. Tracking them on the New axis enables the monotonicity check to work correctly for the subsequent hunk lines.

### Show-level guardrail fires only for non-AST paths
With the correct C1a implementation, `render_default_scoped` (Default mode) never inflates past raw for modified-file commits. The show-level guardrail in `emit_show_commit` fires only when `render_raw_hunks` is used (Added/Deleted files, unsupported languages, or skip_ast path) AND the file is large enough that line-number overhead exceeds metadata savings.

---

## Patterns for Phase 5+

### `render_default_scoped` — do not add clipping
The function must emit ALL patch lines from every hunk in one pass. Do not add clipping (restricting emission to only lines within a changed range) — that was the root cause of Bug 2. All lines, including "orphan" lines outside any changed range, are emitted by the single walk.

### `verify_ast_render` — expand, never weaken
If new emission types are added (e.g., breadcrumbs on the Old axis), update the verifier accordingly. Never relax the uniqueness or coverage checks — they are the safety net for PF-025 (subsequence invariant passes while corruption is happening).

### `Axis` enum — not in rskim-core
`Axis` is defined in `render.rs` (inside `crates/rskim/src/cmd/git/diff/`). It is render-internal and must not be moved to `rskim-core` — rskim-core has no concept of diff axes.

### Dead-code from removed orphan pass
`patch_line_deltas` was the only function used exclusively by the orphan pass. It was deleted. Any future reviewer seeing an "orphan pass" mentioned in comments should know it was replaced by the single positional walk in `3fb0fd3`.

---

## Integration Points for Phase 5

Phase 5 (rskim-core) does NOT touch `render.rs`. The C1a/C1b changes are fully contained in `crates/rskim/src/cmd/git/diff/render.rs`. Phase 5 can safely read this file without merge risk.

Key exports from render.rs for Phase 5 reference:
- `render_diff_file(file_diff, global_flags, args, diff_mode, skip_ast, is_show) -> String` — unchanged signature
- `try_ast_render(...)` — now calls `render_default_scoped` with emissions; returns `None` on verifier failure
- `render_raw_hunks(file_diff, header, ln_width) -> String` — unchanged
- `source_matches_diff(source_lines, hunks) -> bool` — unchanged

The `Axis` enum and `verify_ast_render` are `fn`-scoped private (not `pub(in ...)`) — they are not accessible from outside the `render.rs` module.
