---
feature: file-wrapper-fidelity
name: File-Wrapper Output Fidelity (grep/rg/diff/status/log passthrough & budgets)
description: "Use when adding or modifying file/git command wrappers, debugging byte-fidelity issues, changing passthrough logic, working on the fidelity gate (fidelity.rs::decide), modifying git status flag-stripping or AheadBehind rendering, adjusting memory caps on git log output, touching the skip_ansi_strip flag, working on the ANSI strip scanner, modifying git-diff AST breadcrumb source resolution, adding a raw_override field, changing the lossy-view marker, or working on the SKIM_PASSTHROUGH convergence gate. Keywords: RawPassthrough, skip_ansi_strip, strip_escape_sequences, MAX_SEQ_SCAN, AheadBehind, elision marker, passthrough_parse, run_stdout_degrade, CONFLICTING_SHORT_OPTS, net-savings guard, source_matches_diff, is_show, get_file_source, fidelity.rs, decide(), raw_override, never_passthrough, lossy_view_marker, emit_source_line, verify_ast_render, EmittedCursor, stream_passthrough_raw, command_needs_exact_bytes."
category: component-patterns
directories:
  - crates/rskim/src/cmd/file
  - crates/rskim/src/cmd/git
  - crates/rskim/src/output
  - crates/rskim/src/runner.rs
created: 2026-07-25
updated: 2026-08-27
---

# File-Wrapper Output Fidelity

## Overview

This feature area covers the end-to-end fidelity contract for skim's file and git command wrappers: grep, rg, diff, git status, git log, and git show. The #317 invariant — "never show less than raw" — is enforced through three coordinated mechanisms: the `ParseResult::RawPassthrough` variant (byte-faithful zero-clone passthrough), the `skip_ansi_strip` flag (opt-out of the ANSI strip step), and the unified fidelity gate in `output/fidelity.rs`.

Post-branch, the guard architecture is consolidated. A single `decide()` function in `crates/rskim/src/output/fidelity.rs` replaces two diverging sites that had different tie semantics and a 256-byte floor exemption. `SKIM_PASSTHROUGH=1` is now honored at the `cmd/dispatch.rs` convergence point rather than ~8 scattered per-handler sites, with a filter-vs-wrapper role distinction that prevents the escape hatch from discarding piped input. Standalone `diff` was reclassified as pure passthrough (PF-011) — its hunk-budget parser has been deleted.

## Core Responsibilities

**These wrappers MUST:**
- Emit exactly what the raw tool emits when compression produces no net saving (`fidelity.rs::decide()` → Passthrough)
- Preserve all bytes — including TABs and non-ESC C0 controls — in tools whose output reaches the reader unparsed (`skip_ansi_strip: true`)
- Forward unexpected non-zero exit codes as raw passthrough before any parsing
- Carry exact counts and `SKIM_PASSTHROUGH=1` in any elision marker (loss-bearing; unconditional; ADR-011 class-1)
- Debug-gate no-loss fallback banners (ADR-011 class-2 distinction: banner vs. marker)
- Feed the guard baseline, raw-fallback emission, and `SKIM_PASSTHROUGH=1` path from the same `raw_override` field — not the injected command's output (PF-024)

**These wrappers MUST NOT:**
- Clone `CommandOutput::stdout` into a parse-result payload when `RawPassthrough` is the right signal
- Silently render an ambiguous state as "in sync" (PF-008 fail-loud rule)
- Impose a commit cap on git log output (ADR-010: removed entirely)
- Allow the `SKIM_PASSTHROUGH=1` hatch to reach `env` (PF-012 security control; `never_passthrough: true`)

## Standard Patterns

### Unified Fidelity Gate: `fidelity.rs::decide()`

`output/fidelity.rs::decide(raw, compressed)` is the **single** L2 guard used by both:
- **L2-A** (`output/guardrail.rs`): the file-transform path (`process.rs`)
- **L2-B** (`cmd/execution.rs::savings_decision`): the command-handler path

**Unified rule: Keep IFF compressed is strictly smaller than raw in BOTH bytes AND tokens. Tie (equal) → Passthrough.**

Breaking changes from the prior split implementation:
- **256-byte floor removed (A4)**: small inputs are no longer exempt. A 1-byte raw with a 100-byte compressed form now falls through to Passthrough rather than being silently kept.
- **Tie semantics unified**: both sites now use the same `>=` early-exit (formerly L2-A used `<=` — tied bytes were Kept).
- **L3 (`rskim-contract`) is deliberately unchanged**: it has per-unit byte-only gate, no tokeniser, no floor, and is tracked for migration separately (#325).

The `gh` wrapper no longer has a wholesale exemption from the guard. The `env` wrapper's exemption is **kept** via `never_passthrough: true` on `ToolRunConfig` — a new field meaning "this handler implements a non-negotiable security property; the escape hatch must not bypass it."

### `raw_override` — Consistent Baseline and Fallback Source

**New `Option<String>` field on `ParsedCommandConfig`** that carries the output of the command *the user typed*, before skim injected any flag.

Three consumers, all must agree:
1. **Guard baseline**: `savings_decision` compares compressed against `raw_override` (not the injected command's output)
2. **Raw-fallback emission**: `emit_raw_passthrough` emits `raw_override` when present
3. **`SKIM_PASSTHROUGH=1` path**: streams `raw_override`, not the injected-flag output

Without `raw_override`, all three consumers used the injected command's output — so the "escape hatch" emitted a different format than doing nothing (PF-024). The pattern generalizes what `git status` already did via `user_raw_override`; it is now armed unconditionally there and available to all handlers.

### `SKIM_PASSTHROUGH=1` Convergence Gate

Honored at `cmd/dispatch.rs::dispatch()` as a structural convergence point, replacing ~8 scattered per-handler sites that each re-implemented (and sometimes missed) the check.

**The critical role distinction that makes this correct:**
- **Wrapper role** (`skim git log -n 3`): skim runs the tool. The gate fires → exec the real binary with the user's literal argv.
- **Filter role** (`cat out.log | skim cypress run`): the caller already ran the tool and piped its output in for compression. The gate must NOT fire, or it discards the caller's piped payload.

The gate fires iff: `is_passthrough_mode() && !is_meta_subcommand && subcommand != "env" && !handler_reads_stdin(...)`.

**`MULTI_LEVEL_DISPATCHERS` and `HANDLER_CONSUMED_TOKENS`** handle two special cases: `cargo`, `dotnet`, `go`, and `swift` dispatch internally and are modelled in `MULTI_LEVEL_DISPATCHERS`; `cypress` and `playwright` consume their action token inside the handler, so `("cypress", "run")` etc. appear in `HANDLER_CONSUMED_TOKENS` so the dispatcher knows the full command string.

### Lossy-View Marker: `lossy_view_marker` and `multi_file_lossy_marker`

Renamed and generalized from `rewrite_transparency_marker`. Key changes:
- **Fires when the served view differs from baseline** — no longer requires `SKIM_REWRITTEN_FROM`, no longer gated on non-zero exit
- **`origin` parameter is `Option<&str>`**: `None` means direct invocation; `Some("cat")` means hook-rewrite origin
- **`mode_class_label(mode_str)`** names the elided class (e.g., "structural view", "pseudo view") in the marker text
- **Returns `None` when `differing == 0`** — byte-identical view fires no marker (correct: a lossless passthrough is an ADR-011 class-2 banner, not a class-1 marker)

`rewrite_transparency_marker` is now a deprecated compatibility alias that delegates to `lossy_view_marker`. ADR-011 class-1 → unconditional.

**Trap to avoid:** an early implementation fired the marker on lossless passthrough paths too (when compressed == raw → guard chose passthrough → marker fired incorrectly). That is an ADR-011 class-2 banner and must be `SKIM_DEBUG`-gated. The correct trigger is "view differs from raw baseline", not "guard chose passthrough."

### ParseResult::RawPassthrough — Zero-Clone Passthrough

`RawPassthrough` is a payload-less enum variant that signals `execution.rs` to serve `CommandOutput::stdout` directly, bypassing the parse-result payload round-trip. Used by grep, rg, diff, and all `run_passthrough_tool` entries.

The shared implementation lives in `cmd/file/mod.rs`:

```rust
// cmd/file/mod.rs — single implementation of the byte-faithful contract (ADR-009)
pub(super) fn passthrough_parse(_output: &CommandOutput) -> ParseResult<FileResult> {
    ParseResult::RawPassthrough
    // _output intentionally ignored — RawPassthrough carries no payload.
}
```

Key invariants:
- `content()` returns `""` — never call it expecting actual bytes
- `tier_name()` returns `"passthrough"` — suppresses the compressed-output hint
- `to_json_envelope()` contains `unreachable!()` — execution.rs handles it before `serialize_output`

**`ParseResult::RawPassthrough` vs `ParseResult::Passthrough(String)`:** `RawPassthrough` has no payload; bytes bypass the parser. `Passthrough(String)` is parser-constructed and carries a string payload. Callers that call `content()` on `RawPassthrough` get `""`.

### skip_ansi_strip: true — Mandatory for RawPassthrough

The ANSI strip step in `execution.rs` runs **before** `parse()` and **shadows the `output` binding** — `RawPassthrough` does NOT bypass it. `strip_ansi_escapes` has been removed from the workspace entirely; the replacement `strip_escape_sequences` (private to `output/mod.rs`) is ESC-scoped only and preserves TABs and all non-ESC C0 controls (PF-006).

Any wrapper whose bytes reach the reader unparsed MUST set `skip_ansi_strip: true`. A `debug_assert!` in `execution.rs` fires whenever `parse()` returns `RawPassthrough` while `skip_ansi_strip` is `false`.

### Standalone `diff` — Pure Passthrough (PF-011)

`skim diff` is now byte-identical to native `diff` across all 18 flag forms and under `SKIM_PASSTHROUGH=1`. The hunk-budget parser (`try_parse_standalone_unified`, `DiffParserState`, `parse_hunk_counts`, `build_file_result`) has been **deleted** from `diff.rs`.

**Measurement basis (52 controlled cases + 10 real file pairs):** no region of the space where skim's former `diff` compression beat native `diff`. Every sub-1.0× ratio came from the fidelity guard falling back to raw — the win belonged to unified encoding, not skim. The "compression" was header-collapse worth exactly 1 byte per path character (content-independent), while `FileResult::render` added one leading space per patch line. The worst region was scattered single-line changes — 5.02× worse than native at n=5000.

`git diff` is explicitly NOT in this class: it is natively unified, injects only `--no-color`, and genuinely compresses (0.64×–0.94× on single-file Rust diffs). PF-011 applies only to standalone `diff`.

### Git-diff AST Render: `emit_source_line` and `verify_ast_render`

**Critical Invariant 1 (reintroducing this is a known bug):** `emit_source_line` is the **only valid emission path** in `render.rs`. Every source-derived line must route through it. The function consults the `EmittedCursor` (preventing duplicate emissions) and stamps the diff's `Marker` (preventing added-as-context corruption).

A direct `writeln!(output, " {:>ln_width$} {line}")` reintroduces **two** bugs simultaneously:
1. A duplicate line (the cursor was not consulted)
2. An added (`+`) line rendered as unchanged context (the marker was not recorded)

**`verify_ast_render` now guards every mode** (previously Default only). It runs after both the `DiffMode::Default` scoped-render and the `DiffMode::Structure`/`DiffMode::Full` `render_with_unchanged_context` paths, at the single post-render call site in `render_diff_file`. **Four checks:**

1. **Per-axis uniqueness**: no line number emitted twice on the same axis
2. **New-axis monotonicity**: new-side line numbers don't go backward
3. **`+`/`-` coverage**: every hunk's added/removed lines reached the reader
4. **Marker fidelity (C1d — the critical one)**: each emitted line number's marker matches what the diff says. An added-as-context corruption emits the correct *number* with the wrong *prefix*, so uniqueness, monotonicity, and coverage all pass while the render lies. Only marker fidelity catches it.

Rejection → `render_raw_hunks` (no-loss path) → ADR-011 class-2 banner, `SKIM_DEBUG`-gated.

The ADR-001 size guard is **structurally blind** to content-substitution: a corrupt render that replaces real context lines with shorter wrong lines is often smaller than raw, so being wrong makes it pass a byte-count guard.

### Git-diff AST Breadcrumb Source Validation

`get_file_source` accepts an explicit `is_show: bool` parameter passed by the caller (`cmd/git/show.rs`). It never re-sniffs argv to infer the invocation context — doing so recreated the exact class of bug the design addresses (#467).

`source_matches_diff(source_lines, hunks)` validates every context and added hunk line against `source_lines[new_line - 1]`. A single mismatch → `None` → caller falls back to `render_raw_hunks` with a `debug_log!`-gated banner (ADR-011 class-2).

### git status: CONFLICTING_SHORT_OPTS and AheadBehind

Conflicting flags stripped before forwarding: `CONFLICTING_SHORT_OPTS = &['s', 'z']`. The scan stops at `--`.

`AheadBehind` three-state model: `Absent` (no `# branch.ab` line → renders `[gone]`), `Counts { ahead, behind }`, `Malformed(String)` (PF-008 fail-loud). `Absent` vs. `Counts(0, 0)` distinction is essential.

### git log: run_stdout_degrade + Unconditional Elision Marker

Uses `runner.run_stdout_degrade()` with a 64 MiB (`MAX_OUTPUT_BYTES`) ceiling. When truncated, an **unconditional elision marker** is appended (ADR-011 class-1). No commit count cap (ADR-010).

## Error Handling

Unexpected exit codes forward raw before ANSI stripping. Signal kill (`exit_code: None`) is always `UnexpectedFailure`. No-loss fallback banners are debug-gated (ADR-011 class-2).

## Anti-Patterns

**Adding a direct `writeln!` into `render.rs` instead of routing through `emit_source_line`.** This is two bugs at once: the `EmittedCursor` is not consulted (duplicate line) and the `Marker` is not stamped (added-as-context corruption). `verify_ast_render` catches the marker mismatch — but the fallback to raw hunks means the user loses AST context without warning.

**Assuming the ADR-001 net-savings guard catches content corruption.** The guard measures compressed bytes vs. raw bytes. Wrong breadcrumbs that are shorter than raw pass the guard. Only `verify_ast_render` (content equality) catches this class.

**Returning `Passthrough(output.stdout.clone())` from a pure-passthrough handler.** Use `RawPassthrough` instead — `passthrough_parse` is the shared implementation.

**Setting `skip_ansi_strip: false` for any wrapper returning `RawPassthrough`.** Even the ESC-scoped scanner removes ESC sequences that may be legitimate content bytes (PF-006, ADR-012).

**Treating `AheadBehind::Absent` as `Counts(0, 0)`.** Absent means the remote ref is gone.

**Adding a commit cap to git log.** ADR-010 forbids this.

**Placing a security control (redaction, sanitization) inside only one branch of the fidelity guard.** The guard may choose the other branch; every non-negotiable property must hold before or independently of the guard (PF-012). `env`'s `never_passthrough: true` and its exclusion from the B1 convergence gate are two independent layers.

**Sizing a test fixture until the guard agrees instead of fixing the behavior.** `decide()` grades on output SIZE — fixture size is a free parameter that flips the guard verdict without touching behavior (PF-027). Always verify by diffing stdout bytes, not by confirming the test is green.

**Firing `lossy_view_marker` on lossless passthrough paths.** If the view is byte-identical to raw, the marker must not fire. A no-loss raw-fallback notice is ADR-011 class-2 and must be `SKIM_DEBUG`-gated.

**Re-sniffing argv inside `get_file_source` to infer `is_show`.** The parameter must be passed explicitly by the caller.

## Gotchas

**`verify_ast_render` runs for ALL modes now (C1e), not just Default.** Prior to this branch, `--mode structure` and `--mode full` kept the old `EmittedCursor` path and were never measured — they shipped duplicate lines and added-as-context renders in ~27% of the files they AST-rendered, all of which passed ADR-001. Mode is a correctness boundary in this codebase, not a presentation flag (PF-019 generalization).

**`to_json_envelope()` panics on `RawPassthrough` at runtime.** The `unreachable!()` is intentional.

**Guard baseline for git status substitutions.** `raw_override` carries what the user's literal command would produce; the guard compares compressed against that — not against the porcelain output skim injected.

**`SKIM_PASSTHROUGH=1` is a NO-OP in filter role.** If the caller piped output into skim for compression (`cat log | skim cypress run`), the B1 gate correctly skips exec because `handler_reads_stdin` returns true. Without this guard, the piped payload would be discarded.

**`strip_ansi_cow` still allocates for ANY ESC byte.** The "no allocation" guarantee requires a completely ESC-free buffer. One coloured cell anywhere allocates for the entire buffer.

**`source_matches_diff` is one-directional.** It can only reject a source, never bless a wrong one. A false negative (CRLF, trailing-whitespace differences) costs only AST breadcrumbs — raw hunks are always safe.

**`git diff HEAD path.ts` pathspec trap.** `positional_refs()` cannot distinguish `git diff A B` from `git diff HEAD src/foo.ts`. The resolution is to attempt the blob fetch and fall back to the working-tree read; `source_matches_diff` independently validates the fallback.

**`analytics BPE short-circuit.** When compressed equals raw (passthrough tier), `execution.rs` passes the same string for both sides to `try_record_command`, which skips one BPE tokenization call.

## Key Files

- `crates/rskim/src/output/fidelity.rs` — `decide()` (unified L2 gate); `FidelityDecision` enum; 256-byte floor removed (A4); Tie → Passthrough rule
- `crates/rskim/src/output/mod.rs` — `ParseResult` enum; `strip_escape_sequences` (ESC-scoped, preserves TABs, `MAX_SEQ_SCAN=2048`); `lossy_view_marker`, `multi_file_lossy_marker`, `mode_class_label`; `elision_marker`
- `crates/rskim/src/cmd/dispatch.rs` — `dispatch_for_wrapper()` (D3/D4 wrapper entry point); `dispatch()` (B1 SKIM_PASSTHROUGH convergence gate); `MULTI_LEVEL_DISPATCHERS`; `HANDLER_CONSUMED_TOKENS`
- `crates/rskim/src/cmd/execution.rs` — `run_parsed_command_with_mode`; `RawPassthrough` fast-path; ANSI strip step; `savings_decision`; `emit_raw_passthrough`; `raw_override` consumers; `never_passthrough` gate
- `crates/rskim/src/cmd/file/mod.rs` — `passthrough_parse` shared implementation; `passthrough_config()` factory
- `crates/rskim/src/cmd/file/diff.rs` — pure passthrough (no parser); `CONFIG` with `skip_ansi_strip: true` and `expected_exit_codes: &[1]`
- `crates/rskim/src/cmd/file/grep.rs` — reference RawPassthrough + `skip_ansi_strip: true` + `expected_exit_codes: &[1]`
- `crates/rskim/src/cmd/file/env.rs` — `never_passthrough: true`; credential redaction before and independent of guard (PF-012)
- `crates/rskim/src/cmd/git/diff/render.rs` — `emit_source_line` (single emission path with cursor + marker); `EmittedCursor`; `verify_ast_render` (4 checks, all modes); `render_raw_hunks` (safe fallback)
- `crates/rskim/src/cmd/git/diff/source.rs` — `get_file_source` (explicit `is_show`, pathspec fallback); `extract_show_ref`; `positional_refs`
- `crates/rskim/src/cmd/git/status.rs` — `CONFLICTING_SHORT_OPTS`; `AheadBehind` enum; `parse_ahead_behind`
- `crates/rskim/src/cmd/git/log.rs` — `run_stdout_degrade`; unconditional elision on truncation
- `crates/rskim/src/cmd/git/show.rs` — caller that sets `is_show: true`
- `crates/rskim/src/main.rs` — `stdout_should_serve_raw()` using `isatty(1)`; `force_raw_requested()` reading session sidecar
- `crates/rskim/src/cmd/session_sidecar.rs` — `set_force_raw`/`read_force_raw`; `{ppid}.{tool}.raw` key; wildcard fallback; 300 s reap clock
- `crates/rskim/src/runner.rs` — `CommandRunner`; `MAX_OUTPUT_BYTES` (64 MiB); `read_pipe_degrade`; `run_stdout_degrade`

## Related

- **ADR-001**: Net-savings guard — byte comparison baseline, token fallback, cap strategy; the guard is blind to content-substitution corruption (only `verify_ast_render` catches that)
- **ADR-003**: git diff guardrail; generalized 2026-08-27 — size guard is blind to duplication and reordering, not just omission
- **ADR-009**: `skim grep` byte-faithful passthrough; native `path:line:content` format
- **ADR-010**: git log commit cap removed
- **ADR-011**: Elision markers (class-1, unconditional) vs. raw-fallback banners (class-2, `SKIM_DEBUG`-gated)
- **ADR-012**: Content bytes never filtered; tool colorization neutralized at child invocation (`--no-color`)
- **PF-004**: Two interception surfaces (rewrite engine vs PATH wrappers); `stdout_redirected_to_file` is rewrite-engine-only; wrapper uses `isatty(1)` + `fstat` on fd 1
- **PF-006**: `strip_ansi_escapes` removed; `strip_escape_sequences` preserves TABs
- **PF-008**: Short-flag cluster matching must use char-set scan; `AheadBehind::Malformed` for unparseable `# branch.ab`
- **PF-011**: Thin wrappers over already-minimal tools are net-negative; standalone `diff` is the newest confirmed member
- **PF-012**: Security controls must hold on both branches of the fidelity guard; `env` uses `never_passthrough: true`
- **PF-021**: Streaming passthrough required; buffered passthrough loses output under early-closing readers
- **PF-024**: Guard baseline and fallback must use the user's literal command output, not the injected-flag output; `raw_override` is the fix
- **PF-025**: Proposed invariants must be tested against known-corrupt inputs; `verify_ast_render`'s four checks each required a failing case to be trusted
- **PF-027**: Resizing fixtures until the guard agrees is a silent revert; always verify by diffing bytes
- Feature: `hook-binary-pinning` — `dispatch_for_wrapper()` also gates `SKIM_PASSTHROUGH` at the wrapper entry (cross-reference for the D3/D4 architecture)
