---
feature: file-wrapper-fidelity
name: File-Wrapper Output Fidelity (grep/rg/diff/status/log passthrough & budgets)
description: "Use when adding or modifying file/git command wrappers, debugging byte-fidelity issues, changing passthrough logic, working on the diff hunk-budget parser, modifying git status flag-stripping or AheadBehind rendering, adjusting memory caps on git log output, touching the skip_ansi_strip flag, working on the ANSI strip scanner, or modifying git-diff AST breadcrumb source resolution. Keywords: RawPassthrough, skip_ansi_strip, strip_escape_sequences, MAX_SEQ_SCAN, hunk budget, AheadBehind, elision marker, passthrough_parse, run_stdout_degrade, CONFLICTING_SHORT_OPTS, net-savings guard, source_matches_diff, is_show, get_file_source."
category: component-patterns
directories:
  - crates/rskim/src/cmd/file
  - crates/rskim/src/cmd/git
  - crates/rskim/src/output
  - crates/rskim/src/runner.rs
created: 2026-07-25
updated: 2026-08-12
---

# File-Wrapper Output Fidelity

## Overview

This feature area covers the end-to-end fidelity contract for skim's file and git command wrappers: grep, rg, diff, git status, git log, and git show. The #317 invariant — "never show less than raw" — is enforced at three distinct points: the `ParseResult::RawPassthrough` variant (byte-faithful zero-clone passthrough), the `skip_ansi_strip` flag (opt-out of the ANSI strip step), and the net-savings guard in `execution.rs`.

The wrappers share a three-tier degradation pipeline (`Full → Degraded → Passthrough`) mediated by `run_parsed_command_with_mode` in `execution.rs`. Several subsystems within this area have non-obvious invariants — the `@@`-budget authority in the diff hunk parser, the closed `CONFLICTING_SHORT_OPTS` set for git status, the three-state `AheadBehind` enum, the unconditional elision marker on the 64 MiB log ceiling, and the explicit `is_show` flag for git-diff AST breadcrumb source resolution — that must be preserved across changes.

## Core Responsibilities

**These wrappers MUST:**
- Emit exactly what the raw tool emits when compression produces no savings (net-savings guard)
- Preserve all bytes — including TABs and non-ESC C0 controls — in tools whose output reaches the reader unparsed (`skip_ansi_strip: true`)
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

**`ParseResult::RawPassthrough` vs `ParseResult::Passthrough(String)` — two different tiers by design:**
`RawPassthrough` has no payload; its bytes bypass the parser and are served from `CommandOutput::stdout` directly. `Passthrough(String)` is parser-constructed and carries the string payload. Callers that call `content()` on a `RawPassthrough` result get `""` — the bytes live elsewhere. This distinction is easy to confuse and must be respected when adding new passthrough handlers (issue #464, investigated and closed invalid).

### skip_ansi_strip: true — Mandatory for RawPassthrough and Content-Bearing Wrappers

The ANSI strip step in `execution.rs` runs **before** `parse()` and **shadows the `output` binding** — `RawPassthrough` does NOT bypass it. The stripped bytes are what both the parser and the passthrough path serve to the reader.

**The current scanner (`strip_escape_sequences`, replacing the former `strip_ansi_escapes::strip_str`):**

`strip_ansi_escapes::strip_str` was removed from the workspace entirely (issue #465). It used a vte state machine that discarded ALL C0 control bytes — including TABs (`\t`, `0x09`) — whenever any ESC byte was present in the buffer. One colorized filename would silently destroy POSIX tab separators on every other line.

The replacement `strip_escape_sequences` (private to `output/mod.rs`) is ESC-scoped only:
- Removes only ESC-rooted sequences: CSI (`ESC [`), OSC (`ESC ]`), and 2-byte (`ESC x`)
- TABs, newlines, and all non-ESC C0 controls are **preserved**
- Bounded by `MAX_SEQ_SCAN = 2048` bytes per sequence body: on overrun, or when a non-CSI/OSC byte is encountered inside a sequence body (e.g. a newline), the consumed bytes are emitted **literally** — never silently dropped (#317)
- `strip_ansi_cow(input)` preserves the `Cow::Borrowed` fast path: when no `\x1b` byte is present (the common case), the entire buffer is returned as-is without allocation

**Historical root cause worth preserving:** The old `strip_ansi_cow()` had the same `Cow::Borrowed` no-ESC fast path — but any ESC byte anywhere caused `Cow::Owned` via `strip_ansi_escapes::strip_str`, which then destroyed ALL C0 controls from the entire buffer. So the fast path only skipped the allocation; a single coloured cell destroyed tabs on every other line in the buffer.

**Two wrapper classes that still require `skip_ansi_strip: true`:**

1. **RawPassthrough wrappers (grep, rg, and all `run_passthrough_tool` entries):** bytes in `output.stdout` are served directly to the reader after the strip step; any stripping — even ESC-sequence-only stripping — removes content the reader expects intact (PF-006). The `passthrough_config()` factory in `cmd/file/mod.rs` bakes `skip_ansi_strip: true` for the entire family.

2. **Content-bearing wrappers (ADR-012):** wrappers whose tool output may contain ANSI/ESC bytes that are literal file content (not UI coloring) must also set `skip_ansi_strip: true` to reach the reader byte-faithfully.

A cross-family `debug_assert!` in `execution.rs` fires whenever `parse()` returns `RawPassthrough` while `skip_ansi_strip` is `false` — catching any future hand-rolled `ToolRunConfig` literal that bypasses `passthrough_config`. Uses `debug_assert` (not `assert`) so a misconfigured wrapper fails the test suite without aborting a live command.

**Exception — tree:** `CONFIG_TREE.skip_ansi_strip` stays `false` because tree genuinely parses its output via `RE_TREE_ENTRY`, which matches box-drawing characters at line-start. An ANSI prefix ahead of a box-drawing character would break the regex. This is a deliberate asymmetry with the ls passthrough arm (in the same `ls.rs` file), which returns `RawPassthrough` and therefore requires `skip_ansi_strip: true`. A unit test in `ls.rs` pins `CONFIG_TREE.skip_ansi_strip == false` to make the asymmetry visible.

### Git-diff AST Breadcrumb Source Validation

The diff pipeline builds AST breadcrumbs by parsing the file at the relevant revision. Before this branch, `get_file_source` read the **working-tree file from disk** whenever argv had no `..`/`...` range — which is wrong for `git show <sha>` (whose hunk line numbers come from the blob at `<sha>`, not from the working tree) and for `git diff A B` (whose new-side lines come from `B`, not disk).

**The fix has two parts:**

**1. Explicit `is_show` parameter:**
`render_diff_file` and `try_ast_render` accept an `is_show: bool` parameter passed **explicitly by the caller** (`cmd/git/show.rs`). `get_file_source` never re-sniffs argv to infer the invocation context — doing so recreated the exact class of bug the design addresses (#467). When `is_show` is true, `get_file_source` calls `git show <ref>:<path>` using the ref extracted by `extract_show_ref`.

**2. `source_matches_diff` validation:**
Before building AST breadcrumbs, `render.rs` calls `source_matches_diff(source_lines, hunks)` which verifies every context (`' '`) and added (`+`) hunk line against `source_lines[new_line - 1]`. A single mismatch returns `None` → the caller falls back to `render_raw_hunks` with a `debug_log!`-gated notice (ADR-011: no-loss fallback, debug-gated). This backstop catches any residual source/diff revision mismatch, including the `git diff HEAD path.ts` pathspec trap.

**Pathspec trap (`git diff HEAD path.ts`):**
`git diff HEAD src/foo.ts` — a ref plus a `--`-less pathspec — looks identical to `git diff A B` (two refs) at the positional-argument level. `positional_refs()` in `source.rs` treats both as two positional refs. The resolution: attempt `git show src/foo.ts:src/foo.ts` — it fails because `src/foo.ts` is not a valid revision — then fall back to the working-tree read. This fallback is safe **only because** `source_matches_diff` independently rejects a mismatched source and routes to raw hunks.

**ADR-001 net-savings guard architectural blindspot:**
The ADR-001 net-savings guard measures compressed vs. raw bytes. It is structurally incapable of catching content-substitution corruption: wrong AST breadcrumbs that drop real context lines in favour of shorter summary lines pass the guard precisely because being wrong makes the output smaller. A byte-count guard can only detect when compression inflates output; it cannot detect when compression replaces content with shorter wrong content.

### diff.rs Hunk-Budget Parser

`try_parse_standalone_unified` is a stateful line-by-line parser. The `@@` hunk header is the sole authority on how many lines belong to the hunk body.

Critical invariants (all enforced in `diff.rs`):

**1. In-hunk guard is unconditional.** While `state.in_hunk == true`, every line is body content — regardless of prefix. A deleted SQL comment emitted by diff as `--- some comment` would otherwise match the `--- ` file-header check, trigger a false flush, fabricate a phantom path, and silently drop hunk content.

**2. An empty line inside an open hunk consumes both budgets.** The parser handles `None` (empty line) identically to `' '` (context line): both decrement `hunk_old_remaining` and `hunk_new_remaining`.

**3. `shown_count == entries.len()` contract.** `build_file_result` passes `entries.len()` as `shown_entry_count` to `FileResult::new`. The `total_entry_count` uses `patch_line_count` (not `patch_lines.len()`) because files past the display cap have `patch_lines` cleared while `patch_line_count` stays accurate.

**4. Elision marker travels in the footer, not as an entry.** The `footer` parameter to `FileResult::new` carries the marker.

**5. Hand-written fixtures MUST have hunk headers matching body counts.** A fixture with `@@ -1,3 +1,2 @@` followed by 2 lines triggers early `in_hunk=false`, leaving one expected line unaccounted.

### git status: CONFLICTING_SHORT_OPTS and AheadBehind

**Closed set of conflicting short options:**

```rust
// status.rs — the ONLY chars that conflict with --porcelain=v2
const CONFLICTING_SHORT_OPTS: &[char] = &['s', 'z'];
// 's' = short format (overrides porcelain)
// 'z' = NUL-termination (corrupts output.lines() line-based parse)
```

Conflicting flags are stripped before forwarding to git, with partial cluster rewriting. The scan stops at `--` — pathspecs after the terminator are not flags.

**AheadBehind three-state model** (never silently renders as in-sync):

```rust
enum AheadBehind {
    Absent,            // No # branch.ab line → remote ref gone → renders "[gone]"
    Counts { ahead: u64, behind: u64 },  // Parsed → renders [ahead N] / [behind N] / both / nothing
    Malformed(String), // Parse failed → raw payload in brackets (PF-008 fail-loud)
}
```

The `Absent` vs. `Counts(0, 0)` distinction is essential: when a remote ref is deleted, git omits the `# branch.ab` line entirely. Treating that as `(0, 0)` would falsely imply in-sync.

### git log: run_stdout_degrade + Unconditional Elision Marker

git log uses `runner.run_stdout_degrade()` instead of `runner.run()`. This reads stdout into a 64 MiB (`MAX_OUTPUT_BYTES`) ceiling via `read_pipe_degrade`, returning `(CommandOutput, stdout_truncated: bool)`.

When truncated, the partial output is kept and an **unconditional elision marker** is appended (loss-bearing per ADR-011). There is no commit count cap (ADR-010 removed the former silent 20-commit cap). When the net-savings guard falls back to raw even after truncation, the elision marker is still emitted.

## Error Handling

**Unexpected exit codes forward raw, before ANSI stripping.** `execution.rs` checks `classify_exit(output.exit_code, expected_exit_codes)` before the ANSI strip step. An `UnexpectedFailure` result calls `passthrough_raw(&output)` and returns immediately.

**Signal kill (exit_code: None) is always `UnexpectedFailure`.** This must be classified on the raw `Option<i32>` before any `unwrap_or` default.

**No-loss fallback banners are debug-gated (ADR-011).** When the net-savings guard falls back to raw because compressed >= raw, `execution.rs` emits no stderr notice by default. Only real elision (showing less than raw) gets an unconditional marker with `SKIM_PASSTHROUGH=1`.

## Anti-Patterns

**Returning `Passthrough(output.stdout.clone())` from a pure-passthrough handler.** This clones the entire stdout buffer into the parse result only to have `serialize_output` read it back immediately. Use `RawPassthrough` instead — `passthrough_parse` is the shared implementation.

**Setting `skip_ansi_strip: false` for any wrapper that returns `RawPassthrough` or handles content-bearing bytes.** Even though the new `strip_escape_sequences` scanner preserves TABs, it still removes ESC sequences that may be legitimate content bytes. The correct rule: any wrapper whose bytes reach the reader unparsed MUST set `skip_ansi_strip: true` (PF-006, ADR-012).

**Treating `AheadBehind::Absent` as `Counts(0, 0)`.** Absent means the remote ref is gone — rendering it as in-sync silently drops the `[gone]` signal that mirrors `git status -sb`.

**Adding a commit cap to git log.** ADR-010 forbids this.

**Calling `content()` on a `RawPassthrough` result and using the empty string as actual output.** `content()` returns `""` for this variant by design — the bytes live in `CommandOutput::stdout`.

**Re-sniffing argv inside `get_file_source` to infer `is_show`.** The `is_show` parameter must be passed explicitly by the caller. Inferring it from argv recreates the bug that `source_matches_diff` guards against — and the guard is a backstop, not a license to introduce known-wrong sources (#467).

**Assuming the ADR-001 net-savings guard catches content corruption.** The guard measures compressed bytes vs. raw bytes. Wrong breadcrumbs that replace real context lines with shorter wrong lines pass the guard by making the output smaller. Only a content-equality check (`source_matches_diff`) can catch this class of bug.

## Gotchas

**`to_json_envelope()` panics on `RawPassthrough` at runtime.** The `unreachable!()` is intentional and enforces that execution.rs handles this variant before `serialize_output`.

**The net-savings guard baseline for git status substitutions.** When the user supplies a format-substituting flag like `--short` or `-s`, skim replaces it with `--porcelain=v2 --branch`. The guard compares compressed output against what the user's literal command would have produced (`user_raw_override`), not against the porcelain output.

**diff.rs `---` header parsing only runs outside a hunk.** A hunk body can contain lines starting with `---` (deleted comments, SQL code). The outer `if state.in_hunk { ... continue; }` guard is the critical check.

**Empty-line hunk context (blank context line).** When `line.chars().next()` returns `None`, that's a blank context line consumed by both old and new budgets. Fixtures generated by removing trailing whitespace may silently change the budget accounting.

**`strip_ansi_cow` still allocates for ANY ESC byte.** Even with the fixed scanner that preserves TABs, if a buffer contains even one ESC byte, `strip_ansi_cow` returns `Cow::Owned` (the entire buffer is re-encoded). The fast path (`Cow::Borrowed`) only fires when no `\x1b` byte is present at all. This is correct behavior but means the "no allocation" guarantee requires a completely ESC-free buffer.

**`source_matches_diff` is a one-directional backstop.** It can only *reject* a source, never bless a wrong one. A false negative (rejecting a valid source, e.g. due to CRLF or trailing-whitespace differences) costs only the AST breadcrumbs — the caller falls back to raw hunks, which is always safe.

**`git diff HEAD path.ts` pathspec trap.** `positional_refs()` cannot distinguish `git diff A B` (two revisions) from `git diff HEAD src/foo.ts` (revision + pathspec) — both appear as two positional args. The resolution is to attempt the blob fetch and fall back to the working-tree read when it fails. `source_matches_diff` then independently validates the fallback source.

**Guardrail banner testability for git show.** `git show HEAD:file` uses Pseudo mode, which can never inflate output — so the net-savings guard never fires for file-content mode. Tests for the guardrail banner must target commit mode (structure-mode reads).

**analytics BPE short-circuit.** When compressed equals raw (passthrough tier), `execution.rs` passes the same string for both `original_stdout` and `compressed` to `try_record_command`. The analytics background thread detects this identical-input case and skips one BPE tokenization call.

## Key Files

- `crates/rskim/src/output/mod.rs` — `ParseResult` enum; `content()`, `tier_name()`, `to_json_envelope()` with `unreachable!()` arm; `strip_ansi()`, `strip_ansi_cow()`, `strip_escape_sequences()` (the new ESC-scoped scanner, `MAX_SEQ_SCAN=2048`); `elision_marker` / `elision_marker_unbounded`
- `crates/rskim/src/cmd/execution.rs` — `run_parsed_command_with_mode`; `RawPassthrough` fast-path; ANSI strip step; `savings_decision`; `emit_raw_passthrough`; `debug_assert!` for RawPassthrough+skip_ansi_strip invariant
- `crates/rskim/src/cmd/file/mod.rs` — `passthrough_parse` shared implementation; `MAX_DISPLAY_ENTRIES` / `MAX_INPUT_LINES`; `passthrough_config()` factory (bakes `skip_ansi_strip: true` for the whole passthrough family)
- `crates/rskim/src/cmd/file/grep.rs` — reference implementation for `RawPassthrough` + `skip_ansi_strip: true` + `expected_exit_codes: &[1]`
- `crates/rskim/src/cmd/file/rg.rs` — mirrors grep.rs exactly
- `crates/rskim/src/cmd/file/diff.rs` — hunk-budget parser (`DiffParserState`, `parse_hunk_counts`, `try_parse_standalone_unified`); `build_file_result` / `shown_count == entries.len()` contract
- `crates/rskim/src/cmd/git/diff/render.rs` — `render_diff_file` (accepts `is_show: bool`); `source_matches_diff` (validates source against hunk content before AST render); `render_raw_hunks` (safe fallback)
- `crates/rskim/src/cmd/git/diff/source.rs` — `get_file_source` (explicit `is_show` flag, pathspec fallback, path-traversal guard); `extract_show_ref`; `positional_refs`; `VALUE_FLAGS`
- `crates/rskim/src/cmd/git/show.rs` — caller that sets `is_show: true` before invoking `render_diff_file`
- `crates/rskim/src/cmd/git/status.rs` — `CONFLICTING_SHORT_OPTS`; `strip_conflicting_short_chars`; `AheadBehind` enum; `parse_ahead_behind`
- `crates/rskim/src/cmd/git/log.rs` — `run_stdout_degrade`; unconditional elision on truncation; `is_commit_line` for patch-body exclusion
- `crates/rskim/src/runner.rs` — `CommandRunner`; `MAX_OUTPUT_BYTES` (64 MiB); `read_pipe_degrade`; `run_stdout_degrade`

## Related

- **ADR-009**: `skim grep` byte-faithful passthrough — native `path:line:content` format; no grouped 2-line-per-match envelope
- **ADR-010**: git log commit cap removed entirely — no silent `-n 20` injection; no cap even for explicit rev-ranges
- **ADR-011**: Elision markers (loss-bearing) are unconditional; raw-fallback banners (no-loss) are debug-gated (`crate::debug_log!`)
- **ADR-002**: Count-cap oversized inputs degrade to lossless Passthrough; depth/size caps remain hard errors
- **PF-006**: `strip_ansi_escapes` (removed) destroyed `\t` — root cause of the ANSI rewrite; the replacement `strip_escape_sequences` preserves all non-ESC bytes
- **PF-008**: Short-flag cluster matching must use `CONFLICTING_SHORT_OPTS` char-set scan, not exact-token match; `AheadBehind::Malformed` for unparseable `# branch.ab` (never silent in-sync)
- **PF-004**: Rewrite engine must bail on stdout-to-file redirects; wrapper surface uses `fstat` on fd 1 — two independent fidelity surfaces
