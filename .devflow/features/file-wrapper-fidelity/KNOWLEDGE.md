---
feature: file-wrapper-fidelity
name: File-Wrapper Output Fidelity (grep/rg/diff/status/log passthrough & budgets)
description: "Use when adding or modifying file/git command wrappers, debugging byte-fidelity issues, changing passthrough logic, working on the diff hunk-budget parser, modifying git status flag-stripping or AheadBehind rendering, adjusting memory caps on git log output, or touching the skip_ansi_strip flag. Keywords: RawPassthrough, skip_ansi_strip, hunk budget, AheadBehind, elision marker, passthrough_parse, run_stdout_degrade, CONFLICTING_SHORT_OPTS, net-savings guard."
category: component-patterns
directories:
  - crates/rskim/src/cmd/file
  - crates/rskim/src/cmd/git
  - crates/rskim/src/output
  - crates/rskim/src/runner.rs
created: 2026-07-25
updated: 2026-07-25
---

# File-Wrapper Output Fidelity

## Overview

This feature area covers the end-to-end fidelity contract for skim's file and git command wrappers: grep, rg, diff, git status, git log, and git show. The #317 invariant — "never show less than raw" — is enforced at three distinct points: the `ParseResult::RawPassthrough` variant (byte-faithful zero-clone passthrough), the `skip_ansi_strip` flag (tab preservation), and the net-savings guard in `execution.rs`.

The wrappers share a three-tier degradation pipeline (`Full → Degraded → Passthrough`) mediated by `run_parsed_command_with_mode` in `execution.rs`. Several subsystems within this area have non-obvious invariants — the `@@`-budget authority in the diff hunk parser, the closed `CONFLICTING_SHORT_OPTS` set for git status, the three-state `AheadBehind` enum, and the unconditional elision marker on the 64 MiB log ceiling — that must be preserved across changes.

## Core Responsibilities

**These wrappers MUST:**
- Emit exactly what the raw tool emits when compression produces no savings (net-savings guard)
- Preserve tab bytes in tools that emit TSV/tab-delimited output (`skip_ansi_strip: true`)
- Forward unexpected non-zero exit codes as raw passthrough before any parsing
- Carry exact counts and `SKIM_PASSTHROUGH=1` in any elision marker (loss-bearing; unconditional)
- Debug-gate no-loss fallback banners (ADR-011 distinction: banner vs. marker)

**These wrappers MUST NOT:**
- Clone `CommandOutput::stdout` into a parse-result payload when `RawPassthrough` is the right signal
- Silently render an ambiguous state as "in sync" (PF-008 fail-loud rule)
- Impose a commit cap on git log output (ADR-010: removed entirely)

## Standard Patterns

### ParseResult::RawPassthrough — Zero-Clone Passthrough

`RawPassthrough` is a payload-less enum variant that signals `execution.rs` to serve `CommandOutput::stdout` directly, bypassing the parse-result payload round-trip. Used by grep and rg, which have no compression opportunity.

The shared implementation for all pure-passthrough handlers lives in `cmd/file/mod.rs`:

```rust
// cmd/file/mod.rs — single implementation of the byte-faithful contract (ADR-009)
// Both grep.rs and rg.rs delegate to this function so a future fidelity fix
// lands in one place.
pub(super) fn passthrough_parse(_output: &CommandOutput) -> ParseResult<FileResult> {
    ParseResult::RawPassthrough
    // Note: _output is intentionally ignored — RawPassthrough carries no payload.
}
```

Key behavioral invariants for `RawPassthrough` (all verified in `output/mod.rs`):
- `content()` returns `""` — never call it expecting actual bytes
- `tier_name()` returns `"passthrough"` — this suppresses the compressed-output hint
- `to_json_envelope()` contains `unreachable!()` — execution.rs handles it before reaching `serialize_output`; correct callers never reach that arm

In `execution.rs`, `RawPassthrough` is detected first, before the net-savings guard:

```rust
// execution.rs lines ~773-815 — RawPassthrough bypasses guard entirely
let (mut compressed, effective_tier) = if matches!(result, ParseResult::RawPassthrough) {
    // No guard: serve output.stdout byte-faithfully without cloning it
    // into the parse result. JSON mode builds the envelope from output.stdout directly.
    let tier = emit_raw_passthrough(&output.stdout)?;
    (output.stdout.clone(), tier)
} else if output_format == OutputFormat::Text && tier_name != "passthrough" && !skip_net_savings_guard {
    // Net-savings guard runs only for non-passthrough, non-JSON results.
    ...
}
```

**Takeaway:** Only use `RawPassthrough` when the parse function truly has nothing to return. Never call `content()` on a `RawPassthrough` result expecting the raw bytes.

### skip_ansi_strip: true — Mandatory for Tab-Emitting Handlers

`strip_ansi_escapes::strip_str` (used inside `execution.rs` when `skip_ansi_strip: false`) treats ASCII control codes — including `\t` (0x09) — as part of escape sequences and silently drops them. This destroys tab-delimited output before the parser sees it (PF-006).

Every wrapper for a tool that emits tabs — grep, rg, diff, database tools — MUST set `skip_ansi_strip: true` in its `ToolRunConfig`:

```rust
// grep.rs — correctness-critical comment; DO NOT remove or change to false
const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "grep",
    // skip_ansi_strip: true preserves TAB bytes (0x09) — native grep output is
    // byte-faithful (ADR-009); strip_ansi_escapes destroys \t per PF-006.
    skip_ansi_strip: true,
    expected_exit_codes: &[1],  // grep exit 1 = no matches (benign)
    ..
};
// rg.rs has the identical flag and identical rationale comment.
// diff.rs: standalone diff emits "--- path\tdate" headers; strip_ansi would
// fuse path and timestamp before the tab-split in try_parse_standalone_unified.
```

When `skip_ansi_strip: false`, `execution.rs` calls `strip_ansi_cow` on `output.stdout` before passing to the parse function. The parse function then sees mangled (tab-collapsed) bytes and falls through to `Passthrough` — emitting the already-mangled bytes, a #317 fidelity violation.

**Takeaway:** Any new wrapper for a tool with tab-delimited output starts with `skip_ansi_strip: true`. Audit sibling handlers whenever adding a new tab-aware handler.

### diff.rs Hunk-Budget Parser

`try_parse_standalone_unified` is a stateful line-by-line parser. The `@@` hunk header is the sole authority on how many lines belong to the hunk body.

Critical invariants (all enforced in `diff.rs`):

**1. In-hunk guard is unconditional.** While `state.in_hunk == true`, every line is body content — regardless of prefix. A deleted SQL comment emitted by diff as `--- some comment` would otherwise match the `--- ` file-header check, trigger a false flush, fabricate a phantom path, and silently drop hunk content.

**2. An empty line inside an open hunk consumes both budgets.** Whitespace-stripping tools may reduce a blank context line (` `) to `""`. The parser handles `None` (empty line) identically to `' '` (context line): both decrement `hunk_old_remaining` and `hunk_new_remaining`.

**3. `shown_count == entries.len()` contract.** `build_file_result` passes `entries.len()` as `shown_entry_count` to `FileResult::new`. The `total_entry_count` uses `patch_line_count` (not `patch_lines.len()`) because files past the display cap have `patch_lines` cleared while `patch_line_count` stays accurate.

**4. Elision marker travels in the footer, not as an entry.** The `footer` parameter to `FileResult::new` carries the marker, consistent with sibling handlers (du, df, find, etc.).

**5. Hand-written fixtures MUST have hunk headers matching body counts.** A fixture with `@@ -1,3 +1,2 @@` followed by 2 lines triggers early `in_hunk=false`, leaving one expected line unaccounted — which may be misclassified as a structural header.

### git status: CONFLICTING_SHORT_OPTS and AheadBehind

**Closed set of conflicting short options:**

```rust
// status.rs — the ONLY chars that conflict with --porcelain=v2
const CONFLICTING_SHORT_OPTS: &[char] = &['s', 'z'];
// 's' = short format (overrides porcelain)
// 'z' = NUL-termination (corrupts output.lines() line-based parse)
```

Conflicting flags are stripped before forwarding to git, with partial cluster rewriting:
- `-suno` → `-uno` (strip `s`, preserve `u`, `n`, `o`)
- `-sb` → `-b` (strip `s`, preserve `b` — the branch flag)
- `-z` → dropped entirely
- `-s` → dropped entirely
- `--short`, `--porcelain`, `--null`, `--long` → dropped entirely

The scan stops at `--` — pathspecs after the terminator are not flags.

**AheadBehind three-state model** (never silently renders as in-sync):

```rust
// status.rs — the three states have distinct render behavior
enum AheadBehind {
    Absent,            // No # branch.ab line → remote ref gone → renders "[gone]"
    Counts { ahead: u64, behind: u64 },  // Parsed → renders [ahead N] / [behind N] / both / nothing
    Malformed(String), // Parse failed → raw payload in brackets (PF-008 fail-loud)
}
// Malformed("unexpected-format") renders as "[unexpected-format]"
// Counts(0, 0) renders as no bracket (in-sync)
// Absent with an upstream configured renders as "[gone]" — NOT as in-sync
```

The `Absent` vs. `Counts(0, 0)` distinction is essential: when a remote ref is deleted, git omits the `# branch.ab` line entirely. Treating that as `(0, 0)` would falsely imply in-sync.

### git log: run_stdout_degrade + Unconditional Elision Marker

git log uses `runner.run_stdout_degrade()` instead of `runner.run()`. This function reads stdout into a 64 MiB (`MAX_OUTPUT_BYTES`) ceiling via `read_pipe_degrade`, returning `(CommandOutput, stdout_truncated: bool)`.

When truncated, the partial output is kept and an unconditional elision marker is appended:

```rust
// log.rs — loss-bearing: reader has less data than git produced
let elision = if stdout_truncated {
    Some(crate::output::elision_marker_unbounded(
        "first 64 MiB",
        "commits",
    ))
} else {
    None
};
```

This marker is unconditional per ADR-011 (truncation is loss-bearing, not a lossless fallback). There is no commit count cap (ADR-010 removed the former silent 20-commit cap).

When the net-savings guard passes through to raw even after truncation, the elision marker is still emitted:
```rust
SavingsDecision::Passthrough => {
    let tier = crate::cmd::execution::emit_raw_passthrough(&raw)?;
    if let Some(ref marker) = elision {
        println!("{marker}"); // still unconditional
    }
    ...
}
```

## Error Handling

**Unexpected exit codes forward raw, before ANSI stripping.** `execution.rs` checks `classify_exit(output.exit_code, expected_exit_codes)` before ANSI stripping. An `UnexpectedFailure` result calls `passthrough_raw(&output)` and returns immediately — the parser never sees the output.

**Signal kill (exit_code: None) is always `UnexpectedFailure`.** This must be classified on the raw `Option<i32>` before any `unwrap_or` default. Even for grep/rg/diff (which treat exit 1 as benign), a signal kill is loss-bearing and gets the unconditional stderr marker (not the debug-gated banner).

**No-loss fallback banners are debug-gated (ADR-011).** When the net-savings guard falls back to raw because compressed >= raw, `execution.rs` emits no stderr notice by default. Only real elision (showing less than raw) gets an unconditional marker with `SKIM_PASSTHROUGH=1`.

## Anti-Patterns

**Returning `Passthrough(output.stdout.clone())` from a pure-passthrough handler.** This clones the entire stdout buffer into the parse result only to have `serialize_output` read it back immediately. Use `RawPassthrough` instead — `passthrough_parse` is the shared implementation.

**Setting `skip_ansi_strip: false` for any tool with tab-separated output.** The bug is silent: the parser sees tab-fused tokens, fails to parse, and emits `Passthrough` of the already-mangled bytes. The mangling is invisible in test output unless the fixture contains actual tab bytes.

**Treating `AheadBehind::Absent` as `Counts(0, 0)`.** Absent means the remote ref is gone — rendering it as in-sync silently drops the `[gone]` signal that mirrors `git status -sb`.

**Adding a commit cap to git log.** ADR-010 forbids this. The previous cap silently truncated explicit ranges like `HEAD~30..HEAD` because `has_limit_flag` only recognized `-n`/`--max-count`, not rev-ranges.

**Calling `content()` on a `RawPassthrough` result and using the empty string as actual output.** `content()` returns `""` for this variant by design — the bytes live in `CommandOutput::stdout`.

## Gotchas

**`to_json_envelope()` panics on `RawPassthrough` at runtime.** The `unreachable!()` is intentional and enforces that execution.rs handles this variant before `serialize_output`. If you see this panic in tests, a new code path is calling `serialize_output` directly on a `RawPassthrough` result without going through `run_parsed_command_with_mode`.

**The net-savings guard baseline for git status substitutions.** When the user supplies a format-substituting flag like `--short` or `-s`, skim replaces it with `--porcelain=v2 --branch`. The guard compares compressed output against what the user's literal command would have produced (C-7 / `user_raw_override`), not against the porcelain output. Without this, the guard might incorrectly favor raw short-format output on a small clean repo.

**diff.rs `---` header parsing only runs outside a hunk.** A hunk body can contain lines starting with `---` (deleted comments, SQL code). The outer `if state.in_hunk { ... continue; }` guard is the critical check — never move the `---` header check above or beside this guard.

**Empty-line hunk context (blank context line).** When `line.chars().next()` returns `None` (the `None =>` arm in the hunk match), that's a blank context line consumed by both old and new budgets. Fixtures generated by removing trailing whitespace may silently change the budget accounting.

**Guardrail banner testability for git show.** `git show HEAD:file` uses Pseudo mode, which can never inflate output — so the net-savings guard never fires, and the guardrail banner is never emitted for file-content mode. Tests for the guardrail banner must target commit mode (structure-mode reads) where the rendered diff can exceed the raw diff size. Paired `SKIM_DEBUG` present/absent assertions are the standard revert-detection pattern (one assertion confirms the banner fires, the paired assertion confirms it is debug-gated).

**analytics BPE short-circuit.** When compressed equals raw (passthrough tier), `execution.rs` passes the same string for both `original_stdout` and `compressed` to `try_record_command`. The analytics background thread detects this identical-input case and skips one BPE tokenization call.

## Key Files

- `crates/rskim/src/output/mod.rs` — `ParseResult` enum definition; `content()`, `tier_name()`, `to_json_envelope()` with `unreachable!()` arm; `elision_marker` / `elision_marker_unbounded`
- `crates/rskim/src/cmd/execution.rs` — `run_parsed_command_with_mode`; `RawPassthrough` fast-path; ANSI strip step; `savings_decision`; `emit_raw_passthrough`
- `crates/rskim/src/cmd/file/mod.rs` — `passthrough_parse` shared implementation; `MAX_DISPLAY_ENTRIES` / `MAX_INPUT_LINES`
- `crates/rskim/src/cmd/file/grep.rs` — reference implementation for `RawPassthrough` + `skip_ansi_strip: true` + `expected_exit_codes: &[1]`
- `crates/rskim/src/cmd/file/rg.rs` — mirrors grep.rs exactly
- `crates/rskim/src/cmd/file/diff.rs` — hunk-budget parser (`DiffParserState`, `parse_hunk_counts`, `try_parse_standalone_unified`); `build_file_result` / `shown_count == entries.len()` contract
- `crates/rskim/src/cmd/git/status.rs` — `CONFLICTING_SHORT_OPTS`; `strip_conflicting_short_chars`; `AheadBehind` enum; `parse_ahead_behind`
- `crates/rskim/src/cmd/git/log.rs` — `run_stdout_degrade`; unconditional elision on truncation; `is_commit_line` for patch-body exclusion
- `crates/rskim/src/runner.rs` — `CommandRunner`; `MAX_OUTPUT_BYTES` (64 MiB); `read_pipe_degrade`; `run_stdout_degrade`

## Related

- **ADR-009**: `skim grep` byte-faithful passthrough — native `path:line:content` format; no grouped 2-line-per-match envelope
- **ADR-010**: git log commit cap removed entirely — no silent `-n 20` injection; no cap even for explicit rev-ranges
- **ADR-011**: Elision markers (loss-bearing) are unconditional; raw-fallback banners (no-loss) are debug-gated (`crate::debug_log!`)
- **ADR-002**: Count-cap oversized inputs degrade to lossless Passthrough; depth/size caps remain hard errors
- **PF-006**: `strip_ansi_escapes` destroys `\t` — every tab-emitting wrapper must set `skip_ansi_strip: true`
- **PF-008**: Short-flag cluster matching must use `CONFLICTING_SHORT_OPTS` char-set scan, not exact-token match; `AheadBehind::Malformed` for unparseable `# branch.ab` (never silent in-sync)
- **PF-004**: Rewrite engine must bail on stdout-to-file redirects; wrapper surface uses `fstat` on fd 1 — two independent fidelity surfaces
