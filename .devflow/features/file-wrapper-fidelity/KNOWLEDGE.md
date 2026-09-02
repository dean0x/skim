---
feature: file-wrapper-fidelity
name: File-Wrapper Output Fidelity (grep/rg/diff/status/log passthrough & budgets)
description: "Use when adding or modifying file/git command wrappers, debugging byte-fidelity issues, changing passthrough logic, working on the JSON disclosure sink (emit_json_envelope, Completeness, LineTermination, lossy_json_view_marker), working on remedy_for / passthrough_strips_json, adding or updating strip_skim_flags, working on the fidelity gate (fidelity.rs::decide), modifying git status flag-stripping or AheadBehind rendering, adjusting memory caps on git log output, touching the skip_ansi_strip flag, working on the ANSI strip scanner, modifying git-diff AST breadcrumb source resolution, adding a raw_override field, working on the elision marker (elision_marker_line in rskim-core, passthrough_with_truncation in process.rs), working on cascade empty-output fallback (compact_marker_without_hint), or working on the SKIM_PASSTHROUGH convergence gate. Keywords: emit_json_envelope, Completeness, LineTermination, lossy_json_view_marker, remedy_for, RemedyCtx, passthrough_strips_json, elision_marker_line, passthrough_with_truncation, compact_marker_without_hint, strip_skim_flags, Surface, dispatch_explicit, dispatch_inner, require_flags_for_tool, RawPassthrough, skip_ansi_strip, strip_escape_sequences, MAX_SEQ_SCAN, AheadBehind, elision marker, run_stdout_degrade, CONFLICTING_SHORT_OPTS, net-savings guard, source_matches_diff, is_show, get_file_source, fidelity.rs, decide(), raw_override, never_passthrough, lossy_view_marker, emit_source_line, verify_ast_render, EmittedCursor, stream_passthrough_raw, command_needs_exact_bytes."
category: component-patterns
directories:
  - crates/rskim/src/cmd/file
  - crates/rskim/src/cmd/git
  - crates/rskim/src/output
  - crates/rskim/src/cmd/execution.rs
  - crates/rskim/src/process.rs
  - crates/rskim/src/cascade.rs
  - crates/rskim/src/runner.rs
created: 2026-07-25
updated: 2026-09-02
---

# File-Wrapper Output Fidelity

## Overview

This feature area covers the end-to-end fidelity contract for skim's file and git command wrappers: grep, rg, diff, git status, git log, and git show. The #317 invariant — "never show less than raw" — is enforced through three coordinated mechanisms: the `ParseResult::RawPassthrough` variant (byte-faithful zero-clone passthrough), the `skip_ansi_strip` flag (opt-out of the ANSI strip step), and the unified fidelity gate in `output/fidelity.rs`.

The dispatch layer is consolidated: a single `decide()` function in `output/fidelity.rs` replaces two diverging sites. `SKIM_PASSTHROUGH=1` is honored at `cmd/dispatch.rs`. The JSON output path now has a single disclosure sink (`emit_json_envelope` in `cmd/execution.rs`) that enforces `Completeness` declarations at compile time, replacing the deleted `ViewClass` type. The elision marker is single-sourced in `rskim_core::elision_marker_line`; `process.rs::passthrough_with_truncation` and the cascade path both call it. Standalone `diff` was reclassified as pure passthrough (PF-011) — its hunk-budget parser has been deleted.

## Core Responsibilities

**These wrappers MUST:**
- Emit exactly what the raw tool emits when compression produces no net saving (`fidelity.rs::decide()` → Passthrough)
- Preserve all bytes — including TABs and non-ESC C0 controls — in tools whose output reaches the reader unparsed (`skip_ansi_strip: true`)
- Forward unexpected non-zero exit codes as raw passthrough before any parsing
- Carry exact counts and the `SKIM_PASSTHROUGH=1` remedy in any elision marker (loss-bearing; unconditional; ADR-011 class-1)
- Debug-gate no-loss fallback banners (ADR-011 class-2 distinction: banner vs. marker)
- Feed the guard baseline, raw-fallback emission, and `SKIM_PASSTHROUGH=1` path from the same `raw_override` field — not the injected command's output (PF-024)
- Route every `--json` output through `emit_json_envelope` with an explicit `Completeness` value (ADR-015 / D1)

**These wrappers MUST NOT:**
- Clone `CommandOutput::stdout` into a parse-result payload when `RawPassthrough` is the right signal
- Silently render an ambiguous state as "in sync" (PF-008 fail-loud rule)
- Impose a commit cap on git log output (ADR-010: removed entirely)
- Allow the `SKIM_PASSTHROUGH=1` hatch to reach `env` (PF-012 security control; `never_passthrough: true`)
- Construct a `--json` response without an explicit `Completeness` value — the type has no `Default`

## Standard Patterns

### JSON Disclosure Sink: `emit_json_envelope` and `Completeness`

`cmd/execution.rs::emit_json_envelope(json, completeness, tool, elided, terminate)` is the **single exit** for every `--json` response. It writes the envelope to stdout, then — only when `completeness == Completeness::Lossy` — emits an ADR-011 class-1 disclosure marker on stderr via `output::lossy_json_view_marker`.

`Completeness` has **no `Default` impl** by design. A handler that attempts to build a JSON response without choosing `Complete`, `Reencoded`, or `Lossy` gets a compile error — the type-level enforcement that prevents silent `Complete` mislabelling (ADR-015 / D1). `ViewClass` was deleted when `Completeness` replaced it.

**Seven explicit handler declarations** and one generic path:

| Handler | Completeness | Rationale |
|---|---|---|
| `git diff` | `Reencoded` | All hunk content faithfully carried; only framing changes |
| `git show` (commit view) | `Reencoded` | Structured re-encoding of all commit fields |
| `RawPassthrough` + JSON | `Reencoded` | Envelope embeds `output.stdout` verbatim as JSON string |
| `git status` | `Lossy` | Parser summarises porcelain; elides per-file details |
| `git push` | `Lossy` | Parser classifies ref updates; elides raw progress lines |
| `git fetch` | `Lossy` | Parser classifies ref updates; elides fetch progress |
| `git commit` | `Lossy` | Parser keeps hash + subject; elides body/stats |
| `git log` | `Lossy` | Keeps N entries; truncates to 64 MiB ceiling |
| Generic (all other handlers) | Via `ParseResult::completeness()` | Tier-derived: Passthrough → `Reencoded`; Full/Degraded → `Lossy` |

**`LineTermination`** is a required parameter that preserves per-sink byte contracts. The generic path (`render_output`) passes `LineTermination::None` because it used `write_to_stdout` (no trailing newline) before routing through the sink; every other JSON exit uses `LineTermination::Newline`. This is load-bearing — changing it moves stdout bytes.

**`lossy_json_view_marker(tool, elided, remedy)`** formats the class-1 stderr notice. `elided = Some((kept, total, unit))` renders the countable form (`N units omitted (kept of total shown)`); `None` renders `"summarised, not the full tool output"`. The remedy comes from `remedy_for`.

### `remedy_for` and `passthrough_strips_json`

`fidelity::remedy_for(RemedyCtx { tool, output_format, passthrough_reproduces_argv })` returns the narrowest literally-reachable escape hatch:

- Default arm: `"SKIM_PASSTHROUGH=1 for full output"` (the legacy literal; keeps pinned test assertions green)
- `(OutputFormat::Json, false)` arm: `"run '{tool}' directly for the full output"` — used when `--json` is NOT stripped before the passthrough exec (e.g., `psql --json` hands `--json` to real psql and fails)

`dispatch::passthrough_strips_json(subcommand)` returns `true` only for `"git"` — the only tool where `--json` is skim-owned and stripped before the passthrough exec. Callers on the JSON path derive `passthrough_reproduces_argv` from this function. The two functions are colocated in `dispatch.rs` and pinned by a sync-guard test to prevent drift.

### `strip_skim_flags` — 8 Flag Types

`dispatch::strip_skim_flags(subcommand, args)` strips skim-owned flags before the `SKIM_PASSTHROUGH=1` exec, so the real tool never sees flags it does not understand. Returns `None` (allocation-free) when nothing was stripped; `Some(Vec<String>)` otherwise. Verified byte-identical across 24 test cells.

**All-tools flags (stripped for every subcommand):**
- `--show-stats` (bare boolean)
- `--passthrough` (bare boolean)
- `--line-numbers` (bare boolean; `-n` is NOT stripped — `git log -n N` is a tool flag)
- `--debug` (bare boolean)
- `--max-lines[=N]` and `--max-lines N` (equals + space form)
- `--tokens[=N]` and `--tokens N` (equals + space form)

**Git-only flags (stripped only when `subcommand == "git"`):**
- `--json` (bare token only; `--json=value` and `gh pr list --json title` survive)
- `--mode[=value]` and `--mode value` (equals + space form)

POSIX `--` end-of-options: nothing is stripped after a bare `--`.

### Unified Fidelity Gate: `fidelity.rs::decide()`

`output/fidelity.rs::decide(raw, compressed)` is the **single** L2 guard used by both:
- **L2-A** (`output/guardrail.rs`): the file-transform path (`process.rs`)
- **L2-B** (`cmd/execution.rs::savings_decision`): the command-handler path

**Unified rule: Keep IFF compressed is strictly smaller than raw in BOTH bytes AND tokens. Tie (equal) → Passthrough.**

- **256-byte floor removed (A4)**: small inputs are no longer exempt.
- **Tie semantics unified**: both sites use the same `>=` early-exit.
- **L3 (`rskim-contract`)** is deliberately unchanged.

### Elision Marker: `elision_marker_line` (single-sourced in rskim-core)

`rskim_core::elision_marker_line(language, elided, side, hint)` is the canonical builder for all truncation markers. Shape: `<prefix> ... (N lines truncated/above) — <hint><suffix>`. Markdown is the only language with a non-empty suffix; the hint is placed **inside** the comment (`<!-- ... (N lines truncated) — SKIM_PASSTHROUGH=1 for full output -->`), never leaking outside. When `language` is `None` (unknown extension/stdin), the marker falls back to the `#` prefix.

**`process.rs::passthrough_with_truncation(text, language, max_lines, last_lines)`** calls `elision_marker_line` for both `--max-lines` (head truncation) and `--last-lines` (tail truncation). It fires in two situations:
1. Unknown-language lossless passthrough (ADR-002) — called during `run_transform`
2. **Post-guardrail bound enforcement** — called in `process_file` after `guardrail.rs` elects to serve raw, so the hard line cap holds regardless of whether the guardrail fired

Uses `split_inclusive('\n')` (not `str::lines()`) to preserve CRLF byte-faithfully (#317 / ADR-002).

### cascade.rs: Empty-Output Fallback and Compact Marker

`cascade_for_token_budget` treats empty escalated output the same as `Ok(None)`: an empty string would satisfy any budget and silently suppress the fallback truncation path, violating #317. The cascade tracks `saw_empty_output` separately.

When all modes produced empty output (e.g., a Rust file containing only comments) the cascade recovers the raw source via `Mode::Full` and either:
- Returns `String::new()` if raw is also empty (no marker — nothing to elide)
- Line-truncates the raw source via `fallback_line_truncate` so the reader gets content

**`compact_marker_without_hint(output, hint)`** detects when `truncate_to_token_budget` dropped the remedy hint because the budget was too tight to fit it inline. When `true`, `cascade.rs` emits the hint on stderr instead (ADR-016 channel split; ADR-011 class-1, unconditional):

```
[skim] output truncated to the --tokens budget — SKIM_PASSTHROUGH=1 for full output
```

### `dispatch_for_wrapper` and `dispatch_explicit`

Two public entry points gate access to the shared `dispatch_inner(Surface, …)` core:

- **`dispatch_for_wrapper(name, args, analytics)`** — the wrapper surface (PATH symlinks). Applies three gates before `dispatch_inner(Surface::Wrapper, …)`:
  - **D3** (help/version flags): passes through without compression
  - **D4** (skip-flags): strips tool-specific flags via `skip_flags_for_tool`
  - **D5** (`require_flags_for_tool`): tools like psql and mysql require specific flags to enable machine-readable output; absent flags → passthrough instead of garbled compression

- **`dispatch_explicit(subcommand, args, analytics)`** — the explicit surface (user typed `skim <tool>`). Tags the call as `Surface::Explicit` and delegates to `dispatch_inner` directly.

`Surface` is a `pub(crate)` enum (`Surface::Explicit`, `Surface::Wrapper`) that makes the dispatch path a compile-time discriminant rather than a runtime flag.

### Force-Raw Marker: Accepted Limitations

The `{ppid}.{tool}.raw` sidecar marker carries the rewrite engine's stdout-destination verdict to the wrapper surface. The marker is set (or cleared) on every hook invocation via `session_sidecar::set_force_raw(force_raw, tools, cache_dir)`.

**Five hook early returns occur before `set_force_raw` is called** (stdin read failure, JSON parse failure, missing/unparseable field, and two others). These paths do not clear any previous marker — if a prior command set a force-raw marker, it may persist until the next full hook invocation.

**"Never byte loss" claim is false.** Measured: 304 bytes delivered vs 6803 raw bytes when skim compressed into `| tee f` after the marker was absent (same-tool clear #514, or no hook). Two accepted limitations:
1. **No hook**: a bare wrapper invocation with no PreToolUse hook gets `fstat`-only behaviour (no marker).
2. **Same-tool clear (#514)**: a concurrent `git status` can clear a live `git log | tee f` marker because both share the `{ppid}.git.raw` key.

Both failures compress rather than suppress — but into a pipe sink that needed exact bytes, so the compression is data-loss. The docs now say so explicitly.

### `raw_override` — Consistent Baseline and Fallback Source

`Option<String>` field on `ParsedCommandConfig` carrying the user's literal (uninjected) command output. Three consumers:
1. **Guard baseline**: `savings_decision` compares compressed against `raw_override`
2. **Raw-fallback emission**: `emit_raw_passthrough` emits `raw_override` when present
3. **`SKIM_PASSTHROUGH=1` path**: emits `raw_override` verbatim instead of streaming the injected command

Only set for **read-only / idempotent** handlers. Generalizes what `git status` already did via `user_raw_override`.

### `SKIM_PASSTHROUGH=1` Convergence Gate

Honored at `cmd/dispatch.rs::dispatch()`. The gate fires iff: `is_passthrough_mode() && !is_meta_subcommand && subcommand != "env" && !handler_reads_stdin(...)`.

**Filter role must not fire**: `cat out.log | skim cypress run` pipes into skim for compression. The gate skips exec when `handler_reads_stdin` is true, preventing the piped payload from being discarded.

### Lossy-View Marker (Text Mode): `lossy_view_marker`

Fires when the served view differs from raw bytes (byte-comparison, not hook-rewritten-from check). Class-1 marker — unconditional, not gated by `SKIM_DEBUG`. Returns `None` when `differing == 0` (lossless passthrough). `rewrite_transparency_marker` is a deprecated compatibility alias. Separate from `lossy_json_view_marker` which covers the JSON path.

### git status: CONFLICTING_SHORT_OPTS and AheadBehind

Conflicting flags stripped before forwarding: `CONFLICTING_SHORT_OPTS = &['s', 'z']`. The scan stops at `--`.

`AheadBehind` three-state model: `Absent` (no `# branch.ab` line → renders `[gone]`), `Counts { ahead, behind }`, `Malformed(String)` (PF-008 fail-loud). `Absent` vs. `Counts(0, 0)` distinction is essential.

### git log: run_stdout_degrade + Unconditional Elision Marker

Uses `runner.run_stdout_degrade()` with a 64 MiB (`MAX_OUTPUT_BYTES`) ceiling. When truncated, an **unconditional elision marker** is appended (ADR-011 class-1). No commit count cap (ADR-010).

## Error Handling

Unexpected exit codes forward raw before ANSI stripping. Signal kill (`exit_code: None`) is always `UnexpectedFailure` with an unconditional stderr notice (loss-bearing; class-1). No-loss fallback banners are debug-gated (ADR-011 class-2).

## Anti-Patterns

**Constructing a `--json` response without an explicit `Completeness` value.** `Completeness` has no `Default`; the compile error is the enforcement. Do not add `#[derive(Default)]` to work around it — that silently labels every new handler as `Complete`.

**Using the deleted `ViewClass` type.** It was replaced by `Completeness`. Searching for `ViewClass` in imports indicates a stale branch.

**Adding a direct `writeln!` into `render.rs` instead of routing through `emit_source_line`.** Two bugs at once: the `EmittedCursor` is not consulted (duplicate line) and the `Marker` is not stamped (added-as-context corruption). `verify_ast_render` catches the marker mismatch — but the fallback to raw hunks means the user loses AST context without warning.

**Assuming the ADR-001 net-savings guard catches content corruption.** The guard measures compressed bytes vs. raw bytes. Wrong breadcrumbs that are shorter than raw pass the guard. Only `verify_ast_render` (content equality) catches this class.

**Returning `Passthrough(output.stdout.clone())` from a pure-passthrough handler.** Use `RawPassthrough` instead.

**Setting `skip_ansi_strip: false` for any wrapper returning `RawPassthrough`.** Even the ESC-scoped scanner removes ESC sequences that may be legitimate content bytes (PF-006, ADR-012).

**Treating `AheadBehind::Absent` as `Counts(0, 0)`.** Absent means the remote ref is gone.

**Adding a commit cap to git log.** ADR-010 forbids this.

**Placing a security control (redaction, sanitization) inside only one branch of the fidelity guard.** `env`'s `never_passthrough: true` and its exclusion from the convergence gate are two independent layers.

**Sizing a test fixture until the guard agrees instead of fixing the behavior.** `decide()` grades on output SIZE — fixture size is a free parameter that flips the guard verdict without touching behavior (PF-027).

**Firing `lossy_view_marker` on lossless passthrough paths.** A no-loss raw-fallback notice is ADR-011 class-2 and must be `SKIM_DEBUG`-gated.

**Pinning branch-only SHAs in tests.** Branch refs vanish on squash-merge; CI uses a depth-1 checkout that resolves no history. Pin commit SHAs that are reachable from main, or the Test Suite job will report "unknown revision". (The Test Suite job now uses fetch-depth 0 to fix existing pinned-SHA failures.)

## Gotchas

**`Completeness::Lossy` fires `lossy_json_view_marker` unconditionally.** It is ADR-011 class-1 — not gated by `SKIM_DEBUG`. Handlers that summarise (lose) content must declare `Lossy`, even when the summary is high-quality.

**`LineTermination` is load-bearing, not cosmetic.** `LineTermination::None` preserves the generic path's byte contract. Changing it silently moves a stdout byte on every parsed-command `--json` invocation.

**`passthrough_strips_json` is the single source of truth for the `--json` strip predicate.** It is tested by a sync-guard alongside `strip_skim_flags`. If a new tool gains skim-owned `--json`, update both functions.

**`verify_ast_render` runs for ALL modes now (C1e), not just Default.** Prior to this branch, `--mode structure` and `--mode full` could ship duplicate lines and added-as-context renders without failing ADR-001.

**`to_json_envelope()` panics on `RawPassthrough` at runtime.** The `unreachable!()` is intentional — `execution.rs` handles this path before `serialize_output`.

**`SKIM_PASSTHROUGH=1` is a NO-OP in filter role.** `handler_reads_stdin(...)` returning true blocks the exec.

**`compact_marker_without_hint` is a string scan, not a structural check.** It looks for `"lines truncated)"` and absence of the hint literal. Do not change the marker wording without updating this function.

**Post-guardrail truncation fires for any language when ADR-001 serves raw.** The `passthrough_with_truncation` call in `process_file` applies `--max-lines`/`--last-lines` to whatever `final_output` is — including the raw source the guardrail elected to serve.

**Force-raw marker: compression into `| tee f` is possible** when the hook fires before `set_force_raw` (five early returns) or when the same-tool clear (#514) deletes a live marker. "Never byte loss" is an aspirational goal, not a verified invariant.

## Key Files

- `crates/rskim/src/output/fidelity.rs` — `decide()` (unified L2 gate); `Completeness` (replaces deleted `ViewClass`); `FidelityDecision`; `remedy_for`; `RemedyCtx`; `passthrough_strips_json` (colocated in dispatch.rs); `view_differs`
- `crates/rskim/src/output/mod.rs` — `ParseResult` enum; `strip_escape_sequences` (ESC-scoped, preserves TABs, `MAX_SEQ_SCAN=2048`); `lossy_view_marker` (text mode); `lossy_json_view_marker` (JSON mode); `multi_file_lossy_marker`; `mode_class_label`; `elision_marker`
- `crates/rskim/src/cmd/dispatch.rs` — `dispatch_for_wrapper()` (D3/D4/D5 wrapper gates → `dispatch_inner(Surface::Wrapper)`); `dispatch_explicit()` (→ `dispatch_inner(Surface::Explicit)`); `Surface` enum; `strip_skim_flags` (8 flag types); `passthrough_strips_json`; `MULTI_LEVEL_DISPATCHERS`; `HANDLER_CONSUMED_TOKENS`
- `crates/rskim/src/cmd/execution.rs` — `emit_json_envelope(json, completeness, tool, elided, terminate)` (single JSON sink); `LineTermination`; `stream_passthrough_raw`; `emit_raw_passthrough`; `savings_decision`; `RawPassthrough` fast-path; ANSI strip step; `raw_override` consumers; `never_passthrough` gate
- `crates/rskim/src/process.rs` — `passthrough_with_truncation(text, language, max_lines, last_lines)` (uses `elision_marker_line`; CRLF-safe via `split_inclusive`); `write_result_and_stats`; guardrail integration; `view_differs` computation
- `crates/rskim/src/cascade.rs` — `cascade_for_token_budget`; empty-output guard (`saw_empty_output`); `compact_marker_without_hint`; `fallback_line_truncate`; ADR-016 stderr channel split
- `crates/rskim-core/src/transform/utils.rs` — `elision_marker_line(language, elided, side, hint)` — canonical elision marker builder; language-prefix/suffix; Markdown hint-inside-comment rule
- `crates/rskim/src/cmd/file/mod.rs` — `passthrough_parse` shared implementation; `passthrough_config()` factory
- `crates/rskim/src/cmd/file/diff.rs` — pure passthrough (no parser); `CONFIG` with `skip_ansi_strip: true` and `expected_exit_codes: &[1]`
- `crates/rskim/src/cmd/git/diff/render.rs` — `emit_source_line`; `EmittedCursor`; `verify_ast_render` (4 checks, all modes); `render_raw_hunks`
- `crates/rskim/src/cmd/git/status.rs` — `CONFLICTING_SHORT_OPTS`; `AheadBehind` enum; `Completeness::Lossy`
- `crates/rskim/src/cmd/git/log.rs` — `run_stdout_degrade`; unconditional elision on truncation; `Completeness::Lossy`
- `crates/rskim/src/cmd/git/show.rs` — `Completeness::Reencoded`; `is_show: true` passed to `get_file_source`
- `crates/rskim/src/cmd/git/diff/mod.rs` — `Completeness::Reencoded`
- `crates/rskim/src/cmd/session_sidecar.rs` — `set_force_raw`/`read_force_raw`; `{ppid}.{tool}.raw` key; wildcard fallback; 300 s reap clock
- `crates/rskim/src/runner.rs` — `CommandRunner`; `MAX_OUTPUT_BYTES` (64 MiB); `read_pipe_degrade`; `run_stdout_degrade`

## Related

- **ADR-001**: Net-savings guard — byte comparison baseline, token fallback; the guard is blind to content-substitution corruption (only `verify_ast_render` catches that); marker bytes can tip it to raw
- **ADR-011**: Elision markers (class-1, unconditional) vs. raw-fallback banners (class-2, `SKIM_DEBUG`-gated); `lossy_json_view_marker` is class-1; `compact_marker_without_hint` triggers a class-1 stderr emission
- **ADR-016**: `--max-lines N` = N total incl. marker, N=1 exception; tight `--tokens` = count on stdout, remedy on stderr (the compact-marker channel split)
- **PF-019**: Mode is a correctness boundary; `verify_ast_render` now covers all modes
- **PF-024**: Guard baseline and fallback must use the user's literal command output, not the injected-flag output; `raw_override` is the fix
- **PF-025**: Proposed invariants must be tested against known-corrupt inputs
- **PF-026**: (referenced for JSON disclosure enforcement patterns)
- **PF-027**: Resizing fixtures until the guard agrees is a silent revert; always verify by diffing bytes
- Feature: `hook-binary-pinning` — `dispatch_for_wrapper()` also gates `SKIM_PASSTHROUGH` at the wrapper entry; force-raw sidecar architecture; cross-surface fidelity parity
