---
feature: file-wrapper-fidelity
name: File-Wrapper Output Fidelity (grep/rg/diff/status/log passthrough & budgets)
description: "Use when adding or modifying file/git command wrappers, debugging byte-fidelity issues, changing passthrough logic, working on the JSON disclosure sink (emit_json_envelope, Completeness, LineTermination, lossy_json_view_marker), working on remedy_for / passthrough_strips_json, adding or updating strip_skim_flags, working on the fidelity gate (fidelity.rs::decide), modifying git status flag-stripping or AheadBehind rendering, adjusting memory caps on git log output, touching the skip_ansi_strip flag, working on the ANSI strip scanner, modifying git-diff AST breadcrumb source resolution, adding a raw_override field, working on the elision marker (elision_marker_line in rskim-core, passthrough_with_truncation in process.rs), working on cascade empty-output fallback (compact_marker_without_hint), working on the SKIM_PASSTHROUGH convergence gate, or working on the ADR-022 machine-contract passthrough gate for git (MACHINE_CONTRACT_FLAGS, CONTRACT_SHORT_OPTS, has_machine_contract_flag, json_disarms_the_gate, strip_git_view_flags) or the git show `<rev>:<path>` verbatim-by-default behavior (FileContentView, select_file_content_view, extract_show_mode_flag). Keywords: emit_json_envelope, Completeness, LineTermination, lossy_json_view_marker, remedy_for, RemedyCtx, passthrough_strips_json, elision_marker_line, passthrough_with_truncation, compact_marker_without_hint, strip_skim_flags, Surface, dispatch_explicit, dispatch_inner, RawPassthrough, skip_ansi_strip, strip_escape_sequences, MAX_SEQ_SCAN, AheadBehind, elision marker, run_stdout_degrade, CONFLICTING_SHORT_OPTS, net-savings guard, source_matches_diff, is_show, get_file_source, fidelity.rs, decide(), raw_override, never_passthrough, lossy_view_marker, emit_source_line, verify_ast_render, EmittedCursor, stream_passthrough_raw, command_needs_exact_bytes, MACHINE_CONTRACT_FLAGS, CONTRACT_SHORT_OPTS, has_machine_contract_flag, args_match_flag_set, caller_requested_json, json_disarms_the_gate, json_envelope_would_misreport, strip_git_view_flags, run_passthrough, FileContentView, select_file_content_view, extract_show_mode_flag, emit_raw_passthrough_exact, emit_raw_passthrough_split, ADR-022."
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
updated: 2026-09-29
---

# File-Wrapper Output Fidelity

## Overview

This feature area covers the end-to-end fidelity contract for skim's file and git command wrappers: grep, rg, diff, git status, git log, git show, git push, git fetch, and git commit. The #317 invariant — "never show less than raw" — is enforced through four coordinated mechanisms: the `ParseResult::RawPassthrough` variant (byte-faithful zero-clone passthrough), the `skip_ansi_strip` flag (opt-out of the ANSI strip step), the unified fidelity gate in `output/fidelity.rs`, and — git-specific — the **ADR-022 machine-contract passthrough gate** in `cmd/git/mod.rs`, which serves a closed set of contract flags/syntax raw *ahead of and independent of* the ADR-001 net-savings verdict (a byte-count guard cannot see caller *intent*, which is what a machine-contract format is a question about).

The dispatch layer is consolidated: a single `decide()` function in `output/fidelity.rs` replaces two diverging sites. `SKIM_PASSTHROUGH=1` is honored at `cmd/dispatch.rs`. The JSON output path has a single disclosure sink (`emit_json_envelope` in `cmd/execution.rs`) that enforces `Completeness` declarations at compile time. The elision marker is single-sourced in `rskim_core::elision_marker_line`; `process.rs::passthrough_with_truncation` and the cascade path both call it. Standalone `diff` was reclassified as pure passthrough (PF-011) — its hunk-budget parser has been deleted.

ADR-022 (2026-09-28) also extended the same "byte-count guards can't see intent" principle from flags to **syntax**: `git show <rev>:<path>` (blob extraction) now defaults to verbatim bytes instead of the `Mode::Pseudo` transform, superseding `AD-GIT-SHOW-PSEUDO` (see the dedicated subsection below).

## Core Responsibilities

**These wrappers MUST:**
- Emit exactly what the raw tool emits when compression produces no net saving (`fidelity.rs::decide()` → Passthrough)
- Preserve all bytes — including TABs and non-ESC C0 controls — in tools whose output reaches the reader unparsed (`skip_ansi_strip: true`)
- Forward unexpected non-zero exit codes as raw passthrough before any parsing
- Carry exact counts and the `SKIM_PASSTHROUGH=1` remedy in any elision marker (loss-bearing; unconditional; ADR-011 class-1)
- Debug-gate no-loss fallback banners (ADR-011 class-2 distinction: banner vs. marker)
- Feed the guard baseline, raw-fallback emission, and `SKIM_PASSTHROUGH=1` path from the same `raw_override` field — not the injected command's output (PF-024) — **or from an equivalent per-route mechanism** (see the `raw_override` gaps noted below; not every handler that injects a flag has been audited)
- Route every `--json` output through `emit_json_envelope` with an explicit `Completeness` value (ADR-015 / D1)
- Route git `status`/`diff`/`fetch`/`log`/`show`/`commit`/`push` through the ADR-022 machine-contract gate ahead of dispatch, so a contract flag or `<rev>:<path>` syntax is served raw before any handler-specific parsing is attempted

**These wrappers MUST NOT:**
- Clone `CommandOutput::stdout` into a parse-result payload when `RawPassthrough` is the right signal
- Silently render an ambiguous state as "in sync" (PF-008 fail-loud rule)
- Impose a commit cap on git log output (ADR-010: removed entirely)
- Allow the `SKIM_PASSTHROUGH=1` hatch to reach `env` (PF-012 security control; `never_passthrough: true`)
- Construct a `--json` response without an explicit `Completeness` value — the type has no `Default`
- Attempt any transform on `git show <rev>:<path>`'s default path — blob extraction is verbatim-by-default (ADR-022); a transform is only reachable via an explicit `--mode`

## Standard Patterns

### JSON Disclosure Sink: `emit_json_envelope` and `Completeness`

`cmd/execution.rs::emit_json_envelope(json, completeness, tool, elided, terminate)` is the **single exit** for every `--json` response. It writes the envelope to stdout, then — only when `completeness == Completeness::Lossy` — emits an ADR-011 class-1 disclosure marker on stderr via `output::lossy_json_view_marker`.

`Completeness` has **no `Default` impl** by design. A handler that attempts to build a JSON response without choosing `Complete`, `Reencoded`, or `Lossy` gets a compile error — the type-level enforcement that prevents silent `Complete` mislabelling (ADR-015 / D1). `ViewClass` was deleted when `Completeness` replaced it.

**Since ADR-022, most rows below are conditional, not unconditional.** For every subcommand covered by the ADR-022 gate (`status`/`diff`/`fetch`/`log`/`show`/`commit`/`push`), a machine-contract flag or `<rev>:<path>` on the argv routes to `run_passthrough` *ahead of* the handler in this table — `parse_tier == "passthrough"`, no `Completeness` is ever constructed, and the ADR-001 guard is skipped rather than consulted. The declarations below only apply to the invocations that reach the handler at all.

**Eight explicit handler declarations** and two tier-derived paths:

| Handler | Completeness | Rationale |
|---|---|---|
| `git diff` | `Reencoded` | All hunk content faithfully carried; only framing changes. Reached only absent an ADR-022 contract flag — `diff`'s own former `--stat`/`--name-only`/`--check` gate is now hoisted into the shared one. |
| `git show` (commit view) | `Reencoded` | Structured re-encoding of all commit fields. Also gated ahead of dispatch by ADR-022 and by `show.rs`'s own `PASSTHROUGH_FLAGS`/`ShowMode::MultiRef`; `<rev>:<path>` file-content mode is a separate default (below), not this row. |
| `RawPassthrough` + JSON | `Reencoded` | Envelope embeds `output.stdout` verbatim as JSON string |
| `git status` | `Lossy` | Parser summarises porcelain; elides per-file details. Reached only absent an ADR-022 contract flag (`--porcelain`/`-z`/`--null`/… route to `run_passthrough` first). |
| `git push` | `Lossy` | Parser classifies ref updates; elides raw progress lines. Reached only absent a contract flag or push's own `--help`. Push auto-injects `--porcelain` (AD-GP-2) yet carries `raw_override: None` — see the gap noted under `raw_override` below. |
| `git fetch` | `Lossy` | Parser classifies ref updates; elides fetch progress. Reached only absent a contract flag; `fetch.rs` also declines earlier for its own `--dry-run`/`-q`/`--quiet` check (the `-q`/`--quiet` half is now redundant with ADR-022, kept as defense in depth). |
| `git commit` | `Lossy` | Parser keeps hash + subject; elides body/stats. Reached only absent a contract flag or commit's own `--help`. |
| `git log` | `Lossy` | Keeps N entries; truncates to 64 MiB ceiling. Reached only absent a contract flag — `log`'s own former `--format`/`--pretty` gate is hoisted into the shared one; see `log::commit_shape_is_broken_by` below for the one subset that stays armed even under `--json`. |
| Tier-derived sites (`output/mod.rs`, `cmd/log.rs`) | Via `ParseResult::completeness()` | Passthrough → `Reencoded`; Full/Degraded → `Lossy` |

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

**This is a different flag set from `strip_git_view_flags`** (below), which runs only on the ADR-022 gate's own passthrough path and deliberately does NOT strip `--max-lines`/`--tokens`/`--last-lines` — see that subsection for why the two lists diverge.

### Unified Fidelity Gate: `fidelity.rs::decide()`

`output/fidelity.rs::decide(raw, compressed)` is the **single** L2 guard used by both:
- **L2-A** (`output/guardrail.rs`): the file-transform path (`process.rs`)
- **L2-B** (`cmd/execution.rs::savings_decision`): the command-handler path

**Unified rule: Keep IFF compressed is strictly smaller than raw in BOTH bytes AND tokens. Tie (equal) → Passthrough.**

- **No size floor**: the net-savings guard measures all inputs; small inputs are not exempt.
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

- **`dispatch_for_wrapper(name, args, analytics)`** — the wrapper surface (PATH symlinks). A one-line tag: stamps the call as `Surface::Wrapper` and delegates to `dispatch_inner`. The D3/D4/D5 wrapper gates live **inside** `dispatch_inner` behind `if surface == Surface::Wrapper { … }`, making it structurally impossible to call the shared core on the wrapper surface without those gates running.

- **`dispatch_explicit(subcommand, args, analytics)`** — the explicit surface (user typed `skim <tool>`). Tags the call as `Surface::Explicit` and delegates to `dispatch_inner` directly. No D3/D4/D5 gates are applied on the explicit surface — those are wrapper-surface concerns.

`Surface` is a `pub(crate)` enum (`Surface::Explicit`, `Surface::Wrapper`) that makes the dispatch path a compile-time discriminant rather than a runtime flag.

**Wrapper gates D3/D4/D5** (all inside `dispatch_inner`, gated on `if surface == Surface::Wrapper`):
- **D3** (checked via `args_before_separator`): `--help`, `-h`, `--version`, `-V` exec the real tool's own help via `run_raw_passthrough`.
- **D4** (via `arg_matches_flag` + `skip_flags_for_tool`): tool-owned flags skim must not intercept (e.g., `rg --json`). `redaction_is_mandatory(subcommand)` blocks this gate for `env`/`printenv` — skipping it would serve unredacted credentials.
- **D5** (interactive-tool gate, via `interactive_tool_for` + `require_flags_for_tool`): if `interactive_tool_for(subcommand)` returns true and no required flags are present, exec via `run_inherited_passthrough` with inherited stdio so TTY line-editing works. `interactive_tool_for` asks "would this tool open a readline session?"; `require_flags_for_tool` asks "does the rewrite rule need this flag?" — both D4 and D5 use `arg_matches_flag` for consistent flag parsing.

### Force-Raw Marker: Accepted Limitations

The `{ppid}.{tool}.raw` sidecar marker carries the rewrite engine's stdout-destination verdict to the wrapper surface. The marker is set (or cleared) on every hook invocation via `session_sidecar::set_force_raw(force_raw, tools, cache_dir)`.

**Five hook early returns occur before `set_force_raw` is called** (stdin read failure, JSON parse failure, missing/unparseable field, and two others). These paths do not clear any previous marker — if a prior command set a force-raw marker, it may persist until the next full hook invocation.

**A stale or missing marker can cost bytes.** Measured: 304 bytes delivered vs 6803 raw bytes when skim compressed into `| tee f` after the marker was absent (same-tool clear #514, or no hook). Three accepted limitations:
1. **No hook**: a bare wrapper invocation with no PreToolUse hook gets `fstat`-only behaviour (no marker).
2. **Same-tool clear (#514)**: a concurrent `git status` can clear a live `git log | tee f` marker because both share the `{ppid}.git.raw` key.
3. **PID reuse** (lossless): a recycled PID within the `FORCE_RAW_MAX_AGE` window (300 s) may cause the wrapper to find a stale marker and serve raw instead of compressing — extra raw bytes, none lost.

Limitations 1 and 2 cost bytes; limitation 3 fails toward lossless passthrough.

### `raw_override` — Consistent Baseline and Fallback Source

`Option<String>` field on `ParsedCommandConfig` carrying the user's literal (uninjected) command output. Three consumers:
1. **Guard baseline**: `savings_decision` compares compressed against `raw_override`
2. **Raw-fallback emission**: `emit_raw_passthrough` emits `raw_override` when present
3. **`SKIM_PASSTHROUGH=1` path**: emits `raw_override` verbatim instead of streaming the injected command

Only set for **read-only / idempotent** handlers. Generalizes what `git status` already did via `user_raw_override`.

**The generalization is partial — verify per handler before assuming it applies.** `git push` auto-injects `--porcelain` unless the caller already supplied a conflicting flag (AD-GP-2), yet `push.rs` builds its `ParsedCommandOptions` via `combined(...)`, which hardcodes `raw_override: None`. Its ADR-001 guard baseline (and its raw-fallback bytes) is therefore the *injected*-porcelain output, not the user's literal `git push` — exactly the PF-024 shape `user_raw_override` exists to close on `status`, just never ported to `push`. Every `cmd/pkg/*` subcommand shares the same gap through `run_pkg_subcommand`'s shared helper (also hardcoded `raw_override: None`) whenever its `inject_flags` closure injects something — e.g. `npm ls` with skim's own `--json` also injects npm's `--depth=0`. **`cmd/infra/gh/mod.rs`'s `CONFIG.raw_override: None` looks like the same pattern but is NOT the same gap**: `gh`'s PF-024 fix is a different, deliberate mechanism — `route_rerunnable(subcmd, action)` gates a per-route, per-invocation re-run of the user's original argv via `run_tool_rerunnable`, fired only when a route opts in AND `prepare_args` actually injected something (avoiding a second network round trip on every `gh` call). Do not treat a bare `raw_override: None` as evidence a handler is unfixed without first checking for a route-level compensating mechanism.

### `emit_raw_passthrough_exact` — Byte-Exact Sink for the ADR-001 `Passthrough` Verdict

`emit_raw_passthrough(raw)` guards a trailing newline (appends one if `raw` is non-empty and doesn't already end in one) — load-bearing at four call sites (`cmd/log.rs`, both `cmd/test/shared.rs` sinks, `cmd/file/passthrough_stream.rs`'s `ensure_newline` mirror for the streamed/buffered parity test), so the shared guard was left alone rather than flipped. `emit_raw_passthrough_exact(raw)` is a second sink — same shape, guard off — added specifically for the **ADR-001 `Passthrough`-verdict path**: on that verdict the served bytes are the wrapped tool's *own* stdout, and an appended newline the tool never emitted is an undisclosed divergence from raw at exit 0 with nothing on stderr — worse than cosmetic for a NUL-delimited format, which gains one extra empty record.

Two call sites, one of them measured dead: the generic tool arm in `cmd/execution.rs` (reachable, since the ADR-022 gate is git-only — measured on `skim curl -s file://<27B-body-no-trailing-newline>`: served 28 B instead of 27 B) and the git arm in `cmd/git/mod.rs` (measured **unreachable** today — every git format that is not newline-terminated, e.g. `-z`/`--null`/`--format=`, is now in `MACHINE_CONTRACT_FLAGS`, so ADR-022 routes it to `run_passthrough` before a savings verdict is ever computed; routed through this sink anyway as defense in depth against a future narrowing of that flag set). Pre-fix baseline measurement: `git status -z` served 51 B against 50 B raw — a trailing `0a` after the NUL terminator.

### `SKIM_PASSTHROUGH=1` Convergence Gate

Honored inside `cmd/dispatch.rs::dispatch_inner()`. The gate fires iff: `is_passthrough_mode() && !is_meta_subcommand && subcommand != "env" && !handler_reads_stdin(...)`.

**Filter role must not fire**: `cat out.log | skim cypress run` pipes into skim for compression. The gate skips exec when `handler_reads_stdin` is true, preventing the piped payload from being discarded.

### Lossy-View Marker (Text Mode): `lossy_view_marker`

Fires when the served view differs from raw bytes (byte-comparison, not hook-rewritten-from check). Class-1 marker — unconditional, not gated by `SKIM_DEBUG`. Returns `None` when `differing == 0` (lossless passthrough). Separate from `lossy_json_view_marker` which covers the JSON path.

### ADR-022 — Machine-Contract Passthrough Gate (`cmd/git/mod.rs`)

`has_machine_contract_flag(args)` is checked in `run()`'s match arm **ahead of** dispatch to `status`/`diff`/`fetch`/`log`/`show`/`commit`/`push` — a match routes straight to `run_passthrough`, so the flagged handler (and every `Completeness` row above) never runs at all for that invocation. Unknown subcommands are unaffected — they already stream through `run_raw_passthrough` in the `other` arm.

**Why a flag list, not a measurement.** "This output format is a machine contract" is a question about the caller's *intent*; the ADR-001 byte comparison structurally cannot answer it. Before this gate, a contract format survived only by coincidence of two blunt mechanisms — the net-savings guard (protects a format only while compression happens to lose, and reverses when repo state changes) and "non-zero exit ⇒ forward raw" (protects only failing invocations). Measured at `c2b4378`: `git log --stat -n 3` served 374 B against 32 733 B raw with **zero bytes on stderr** — the flag was swallowed without a trace, and all five of `--stat`/`--shortstat`/`--numstat`/`--name-only`/`--name-status` produced the identical 374 B.

**`MACHINE_CONTRACT_FLAGS`**: `--porcelain`, `--null`, `--stat`, `--shortstat`, `--numstat`, `--name-only`, `--name-status`, `--raw`, `--check`, `--quiet`, `-q`, `--exit-code`, `--graph`, `--format`, `--pretty` (matched via `user_has_flag`'s `=`-aware rule). **`CONTRACT_SHORT_OPTS = &['z']`** matches anywhere inside a single-dash cluster (`git status -sz`) — deliberately cluster-aware, because `status.rs`'s own `CONFLICTING_SHORT_OPTS` scan already was, so an exact-token gate here would have been *weaker* than what already shipped. `-s`/`--short` is deliberately absent — that is a human-facing rendering `status.rs` translates faithfully, not a contract.

**The error modes are asymmetric, and that asymmetry is why the gate over-includes.** A false positive costs nothing that matters: the reader gets byte-faithful raw git, and `run_passthrough` records `parse_tier == "passthrough"` — **guard-neutral**, not graded by ADR-001 at all. A false negative is the whole defect class: a NUL-delimited / tab-framed / exit-code-bearing stream gets reshaped into prose, silently, at exit 0, with no ADR-011 class-1 marker. Two deliberate consequences follow: the gate is **not** separator-aware (a pathspec literally named `--stat` after a bare `--` also serves raw — over-inclusion can only turn a match into a bigger match, never manufacture a false negative), and short-cluster matching scans every character of a single-dash token (`git log -Szebra` — the pickaxe value merely containing a `z` — also serves raw).

**`--json` disarms the gate, conditionally — and this is the one place where `--json` and the over-inclusion asymmetry point in opposite directions.** `json_disarms_the_gate(subcmd, args) = caller_requested_json(args) && !json_envelope_would_misreport(subcmd, args)`. `--json` is itself a flag the caller typed, and what it names is *skim's own* machine contract — the JSON envelope, emitted through `emit_json_envelope` with its mandatory `Completeness` and class-1 marker — so disarming for it is not the false negative the asymmetry warns about, **provided the envelope it produces is true**. `json_envelope_would_misreport` is `subcmd == "log" && log::commit_shape_is_broken_by(args)`: `parse_log` reads exactly one shape (the `%h`-prefixed line `run_log` itself injects), so a flag that **replaces** that shape (`--format`, `--pretty`), **replaces its record separator** (`-z`, `--null`), or **prefixes** it (`--graph`) leaves nothing for `is_commit_line` to match and the JSON count collapses to a **false** `"no commits"` — measured on a 3-commit fixture: `log --graph -n 2 --json` served 69 B claiming `"no commits"` against 369 B raw; `log --stat -n 2 --json` served 266 B correctly claiming `"2 commits"` against 605 B raw, because the stat family only *appends* a block beside the shape rather than breaking it. That split is `log.rs`'s business (`log::commit_shape_is_broken_by`), not the gate's own; every other gated subcommand's `--json` always disarms.

**`--mode` never disarms — it is stripped instead.** `strip_git_view_flags(args)` removes a bare `--json` and `--mode`/`--mode=<val>` from the argv that reaches `run_passthrough`'s child `git` (nothing is dropped at or after a bare `--`, matching `extract_json_flag`/`extract_diff_mode`/`dispatch::strip_skim_flags`). This is deliberately **narrower** than `dispatch::strip_skim_flags`: `--max-lines`/`--tokens`/`--last-lines` are left alone here, because no git handler implements them — measured, `skim git log -n 2 --max-lines 10` is `fatal: ambiguous argument '10'` with or without a contract flag, so stripping them would turn a hard error into a silently **unbounded** serve, the exact ADR-016 defect ("a bound the tool can exceed is not a bound"). Any drop is `SKIM_DEBUG`-gated (ADR-011 class 2 — the reader gets git's own bytes for a payload that had no skim view to select, a lossless raw fallback, not a loss-bearing one).

**`run_passthrough` is the one sink for the ADR-022 gate AND the five pre-existing per-command gates** (`show.rs`'s `PASSTHROUGH_FLAGS`/`ShowMode::MultiRef`, `fetch.rs`'s `--dry-run`/`-q`/`--quiet`, `commit.rs`'s and `push.rs`'s `--help`). Filtering inside `run_passthrough` rather than at each call site means a call site cannot reintroduce the leak by forgetting to filter, and narrowing `MACHINE_CONTRACT_FLAGS` cannot bring it back either. `show.rs` keeps its own `PASSTHROUGH_FLAGS` list — `MACHINE_CONTRACT_FLAGS` is now a strict superset reached first, so `show.rs` is unaffected; `--raw` was the last entry `show.rs` carried alone before being hoisted here.

### git status: CONFLICTING_SHORT_OPTS and AheadBehind

Conflicting flags stripped before forwarding: `CONFLICTING_SHORT_OPTS = &['s', 'z']`. The scan stops at `--`. **Now defense-in-depth, not the first line of defense**: `--porcelain`/`--porcelain=*`/`--null` and any `z`-containing cluster are ADR-022 machine-contract flags, so `has_machine_contract_flag` routes them to `run_passthrough` before `run_status` is even entered — including before the `raw_override`-capture subprocess (the extra `git status <args>` re-run used to build the guard baseline). The arms stay anyway: they stop a *stripped* user format flag from being forwarded alongside the injected `--porcelain=v2`, where git's last-flag-wins rule would hand the v2 parser a format it cannot read (PF-008) — narrowing `MACHINE_CONTRACT_FLAGS` later must not silently reintroduce that. `--short`, `--long`, and `s`-only clusters (`-s`, `-sb`) are **not** in the ADR-022 set and still reach `is_conflicting_status_flag` directly.

`AheadBehind` three-state model: `Absent` (no `# branch.ab` line → renders `[gone]`), `Counts { ahead, behind }`, `Malformed(String)` (PF-008 fail-loud). `Absent` vs. `Counts(0, 0)` distinction is essential.

### git log: run_stdout_degrade + Unconditional Elision Marker

Uses `runner.run_stdout_degrade()` with a 64 MiB (`MAX_OUTPUT_BYTES`) ceiling. When truncated, an **unconditional elision marker** is appended (ADR-011 class-1). No commit count cap (ADR-010). Gated by ADR-022 ahead of dispatch — `log`'s own former `--format`/`--pretty` gate is hoisted into the shared one; `log::commit_shape_is_broken_by` is the predicate consulted by `json_envelope_would_misreport` (above) to decide which of `log`'s contract flags stay armed under `--json`.

### git show `<rev>:<path>` — ADR-022 Verbatim Default (supersedes `AD-GIT-SHOW-PSEUDO`)

`run_show_file_content` has no transform ladder on the default path. `select_file_content_view(mode)` maps `None` (no `--mode`) **and** `Some(Mode::Full)` to `FileContentView::Verbatim` — served with no language detection, no guardrail, and no marker — and any other `Mode` to `FileContentView::Transformed(mode)`, which transforms via `rskim_core::transform` and discloses (ADR-011 class 1, unconditional) only when the guardrail actually serves the transformed view (`served_transformed = !guardrail.was_triggered()`).

**`Mode::Full` takes the same Verbatim branch rather than routing through the transform** — `Mode::Full` is defined as "no transformation," so serving the blob verbatim honours it exactly without making byte-faithfulness contingent on tree-sitter parsing the blob or the guardrail's tie-rule electing raw, neither of which has any say in what `full` means.

**What this replaced.** The prior default routed the blob through `Mode::Pseudo` (`AD-GIT-SHOW-PSEUDO`, previously pinned by three tests, now amended) — measured 18 of 50 lines differing on a TypeScript blob (`ok: boolean;` → `ok`, `const out: T[] = [];` → `const out = []`), with the loss undisclosed even under `SKIM_DEBUG=1` (no `lossy_view_marker` call existed anywhere under `cmd/git/` before this fix, though `process.rs` already emitted one for the identical transform on the plain file-read path), and no working escape hatch (`--mode=full` on `git show` was `fatal: unrecognized argument`, exit 1). `extract_show_mode_flag` now strips `--mode`/`--mode=<val>` before the child `git show` sees it (the same leak `strip_git_view_flags` closes for the ADR-022 gate above, on a different call path).

**Scope the fidelity claim to valid-UTF-8 blobs.** `CommandRunner::run` (and its siblings) convert the child's stdout via `String::from_utf8(buf).unwrap_or_else(|e| String::from_utf8_lossy(...))` — a non-UTF-8 blob (a binary file, or text with invalid byte sequences) is already lossily reencoded by the runner layer *before* `run_show_file_content` ever sees it, independent of ADR-022. "Verbatim" here means "no transform is applied," not "immune to the process layer's own UTF-8 boundary."

**Pseudo mode on Rust is token-neutral, so the `Transformed` branch is unreachable at the CLI for that language.** `rskim-core`'s Rust `PseudoRules` has empty `strip_kinds`/`strip_keywords`; the declaration-terminator/array-length `;` removal is its only content-affecting lever, and that removal was measured token-neutral (205 semicolons removed, 1129 → 1129 BPE tokens, 0.0%, because the tokenizer folds `;` into an adjacent token). Since `guardrail::apply_to_stderr` keeps the transformed side only when strictly smaller in *both* bytes and tokens, its `Keep` branch is unreachable for Rust input at any size that has been measured — `skim git show HEAD:foo.rs --mode=pseudo` on a Rust file resolves to the raw blob via the guardrail, and silently (no class-1 marker fires, because `served_transformed` is false). This is a property of the transform, not of `show.rs` specifically — it applies to any caller of `rskim_core::transform(_, Language::Rust, Mode::Pseudo)`, including the plain file-read path in `process.rs`.

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

**Pinning branch-only SHAs in tests.** Branch refs vanish on squash-merge; CI uses a depth-1 checkout that resolves no history. Pin commit SHAs that are reachable from main, or the Test Suite job will report "unknown revision". (The CI Test Suite job uses fetch-depth 0 — a depth-1 checkout cannot resolve historical SHAs.)

**Widening `MACHINE_CONTRACT_FLAGS` on a hunch, or narrowing it on a hunch.** The set is deliberately over-inclusive (asymmetric error cost, above) — a proposed addition or removal needs a measurement against the live binary, the way every current entry has one in `mod.rs`'s doc comments, not just a plausible argument.

**Assuming `raw_override: None` means a handler has the PF-024 guard-baseline defect.** `gh`'s `route_rerunnable` mechanism achieves the same correctness a different way (see `raw_override` above); check for a compensating per-route mechanism before flagging a handler as unfixed.

## Gotchas

**`Completeness::Lossy` fires `lossy_json_view_marker` unconditionally.** It is ADR-011 class-1 — not gated by `SKIM_DEBUG`. Handlers that summarise (lose) content must declare `Lossy`, even when the summary is high-quality.

**`LineTermination` is load-bearing, not cosmetic.** `LineTermination::None` preserves the generic path's byte contract. Changing it silently moves a stdout byte on every parsed-command `--json` invocation.

**`passthrough_strips_json` is the single source of truth for the `--json` strip predicate.** It is tested by a sync-guard alongside `strip_skim_flags`. If a new tool gains skim-owned `--json`, update both functions.

**`verify_ast_render` runs for ALL modes, not just Default.** It catches duplicate lines, backward line numbers, and added-as-context rendering for every mode.

**`to_json_envelope()` panics on `RawPassthrough` at runtime.** The `unreachable!()` is intentional — `execution.rs` handles this path before `serialize_output`.

**`SKIM_PASSTHROUGH=1` is a NO-OP in filter role.** `handler_reads_stdin(...)` returning true blocks the exec.

**`compact_marker_without_hint` is a string scan, not a structural check.** It looks for `"lines truncated)"` and absence of the hint literal. Do not change the marker wording without updating this function.

**Post-guardrail truncation fires for any language when ADR-001 serves raw.** The `passthrough_with_truncation` call in `process_file` applies `--max-lines`/`--last-lines` to whatever `final_output` is — including the raw source the guardrail elected to serve.

**Force-raw marker: compression into `| tee f` is possible** when the hook fires before `set_force_raw` (five early returns) or when the same-tool clear (#514) deletes a live marker. "Never byte loss" is an aspirational goal, not a verified invariant.

**The ADR-022 gate is evaluated before dispatch, so it sees the raw argv — including tokens after `--`.** It is deliberately not separator-aware (see the asymmetry argument above); do not "fix" this by routing it through `args_before_separator`, which would only manufacture false negatives.

**`git show`'s `--mode=pseudo` on a Rust blob is a documented no-op via the guardrail, not a bug to chase.** See the git show subsection above — Rust's pseudo `PseudoRules` are token-neutral, so the transformed view can never win the guardrail's strictly-smaller check.

## Key Files

- `crates/rskim/src/output/fidelity.rs` — `decide()` (unified L2 gate); `Completeness` (replaces deleted `ViewClass`); `FidelityDecision`; `remedy_for`; `RemedyCtx`; `passthrough_strips_json` (colocated in dispatch.rs); `view_differs`
- `crates/rskim/src/output/mod.rs` — `ParseResult` enum; `strip_escape_sequences` (ESC-scoped, preserves TABs, `MAX_SEQ_SCAN=2048`); `lossy_view_marker` (text mode; handles single and multi-file); `lossy_json_view_marker` (JSON mode); `mode_class_label`; `elision_marker`
- `crates/rskim/src/cmd/dispatch.rs` — `dispatch_for_wrapper()` (thin tag → `Surface::Wrapper`); `dispatch_explicit()` (→ `Surface::Explicit`); private `dispatch_inner(surface, …)` (shared core; D3/D4/D5 wrapper gates live here behind `if surface == Surface::Wrapper`); `redaction_is_mandatory()`; `Surface` enum; `strip_skim_flags` (8 flag types); `passthrough_strips_json`; `MULTI_LEVEL_DISPATCHERS`; `HANDLER_CONSUMED_TOKENS`
- `crates/rskim/src/cmd/execution.rs` — `emit_json_envelope(json, completeness, tool, elided, terminate)` (single JSON sink); `LineTermination`; `stream_passthrough_raw`; `emit_raw_passthrough`; `emit_raw_passthrough_exact` (byte-exact sink for the ADR-001 `Passthrough` verdict); `emit_raw_passthrough_split` (stdout/stderr on separate descriptors); `savings_decision`; `RawPassthrough` fast-path; ANSI strip step; `raw_override` consumers; `never_passthrough` gate
- `crates/rskim/src/process.rs` — `passthrough_with_truncation(text, language, max_lines, last_lines)` (uses `elision_marker_line`; CRLF-safe via `split_inclusive`); `write_result_and_stats`; guardrail integration; `view_differs` computation
- `crates/rskim/src/cascade.rs` — `cascade_for_token_budget`; empty-output guard (`saw_empty_output`); `compact_marker_without_hint`; `fallback_line_truncate`; ADR-016 stderr channel split
- `crates/rskim-core/src/transform/utils.rs` — `elision_marker_line(language, elided, side, hint)` — canonical elision marker builder; language-prefix/suffix; Markdown hint-inside-comment rule
- `crates/rskim-core/src/transform/pseudo.rs` — `PseudoRules` per language; Rust's is empty `strip_kinds`/`strip_keywords` (comment on the file header, and inline at the `;`-guard site) — the basis for "pseudo mode on Rust is guardrail-unreachable" above
- `crates/rskim/src/cmd/file/mod.rs` — `passthrough_parse` shared implementation; `passthrough_config()` factory
- `crates/rskim/src/cmd/file/diff.rs` — pure passthrough (no parser); `CONFIG` with `skip_ansi_strip: true` and `expected_exit_codes: &[1]`
- `crates/rskim/src/cmd/git/mod.rs` — `run()` dispatch; ADR-022 machine-contract gate: `MACHINE_CONTRACT_FLAGS`, `CONTRACT_SHORT_OPTS`, `has_machine_contract_flag`, `args_match_flag_set`, `caller_requested_json`, `json_disarms_the_gate`, `json_envelope_would_misreport`, `strip_git_view_flags`; `run_passthrough` (shared raw sink for the gate plus 5 per-command gates)
- `crates/rskim/src/cmd/git/diff/render.rs` — `emit_source_line`; `EmittedCursor`; `verify_ast_render` (4 checks, all modes); `render_raw_hunks`
- `crates/rskim/src/cmd/git/status.rs` — `CONFLICTING_SHORT_OPTS`; `AheadBehind` enum; `Completeness::Lossy`; `is_conflicting_status_flag`'s arms are defense-in-depth behind the ADR-022 gate
- `crates/rskim/src/cmd/git/log.rs` — `run_stdout_degrade`; unconditional elision on truncation; `Completeness::Lossy`; `commit_shape_is_broken_by` (consumed by `json_envelope_would_misreport` in `mod.rs`)
- `crates/rskim/src/cmd/git/show.rs` — ADR-022: `FileContentView` (`Verbatim`/`Transformed`), `select_file_content_view`, `extract_show_mode_flag` (strips `--mode` before the child `git show`); commit view: `Completeness::Reencoded`/`Lossy` split (D3/#510 patch-carrying rule); `is_show: true` passed to `render_diff_file`, forwarded to `get_file_source` (`diff/source.rs`)
- `crates/rskim/src/cmd/git/diff/mod.rs` — `Completeness::Reencoded`/`Lossy`; its own former `--stat`/`--name-only`/`--check` passthrough gate is now hoisted into the shared ADR-022 gate in `cmd/git/mod.rs`
- `crates/rskim/src/cmd/session_sidecar.rs` — `set_force_raw`/`read_force_raw`; `{ppid}.{tool}.raw` key; wildcard fallback; 300 s reap clock
- `crates/rskim/src/runner.rs` — `CommandRunner`; `MAX_OUTPUT_BYTES` (64 MiB); `read_pipe_degrade`; `run_stdout_degrade`; stdout is decoded via `String::from_utf8(...).unwrap_or_else(|e| String::from_utf8_lossy(...))` — the lossy-on-invalid-UTF-8 boundary the git show subsection above scopes its verbatim claim against

## Related

- **ADR-001**: Net-savings guard — byte comparison baseline, token fallback; the guard is blind to content-substitution corruption (only `verify_ast_render` catches that); marker bytes can tip it to raw
- **ADR-011**: Elision markers (class-1, unconditional) vs. raw-fallback banners (class-2, `SKIM_DEBUG`-gated); `lossy_json_view_marker` is class-1; `compact_marker_without_hint` triggers a class-1 stderr emission
- **ADR-016**: `--max-lines N` = N total incl. marker, N=1 exception; tight `--tokens` = count on stdout, remedy on stderr (the compact-marker channel split)
- **ADR-022**: Machine-contract passthrough gate for git — a closed flag/syntax set served raw ahead of and independent of the ADR-001 verdict; supersedes `AD-GIT-SHOW-PSEUDO` for `git show <rev>:<path>` blob extraction
- **PF-019**: Mode is a correctness boundary; `verify_ast_render` covers all modes, not just Default
- **PF-024**: Guard baseline and fallback must use the user's literal command output, not the injected-flag output; `raw_override` is the fix on `git status` — NOT yet ported to `git push` or `cmd/pkg/*`; `gh`'s `route_rerunnable` is an independent fix for the same defect class
- **PF-025**: Proposed invariants must be tested against known-corrupt inputs
- **PF-027**: Resizing fixtures until the guard agrees is a silent revert; always verify by diffing bytes
- Feature: `hook-binary-pinning` — the B1 `SKIM_PASSTHROUGH` gate and D3/D4/D5 wrapper gates inside `dispatch_inner`, force-raw sidecar architecture, and cross-surface conformance testing are shared between the two feature areas
- Feature: `build-parsers` — the PF-038 global PATH-resolvability bail lives in the rewrite engine (`cmd/rewrite/engine.rs`) that sits upstream of the wrapper/explicit surfaces documented here; `runner::program_resolves` is a sibling of `CommandRunner` in the same file
