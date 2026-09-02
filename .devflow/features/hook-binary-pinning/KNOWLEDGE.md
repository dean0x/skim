---
feature: hook-binary-pinning
name: Agent Hook Install, Binary Pinning & Handshake (+ Permissions Seeding)
description: "Use when modifying hook script generation, adding new agents, changing the hook script format, debugging version-skew or wrong-clone warnings, working on install/reinstall logic, touching wrapper symlink management, editing guidance_content() / the Command wrapping section, working on skim init --permissions / the PermissionsProtocol subsystem, modifying the proxy feature gate / cfg-gated registry entries, touching the lossy-view transparency marker, working on doctor integrity checks / ScriptIntegrity / hook_status_line, adding test harness sandboxing, modifying the rewrite engine dispatch (try_rewrite / command_needs_exact_bytes / is_bare_cat_pipeline / dispatch_for_wrapper / dispatch_inner / dispatch_explicit), or working on the force-raw sidecar / stdout destination gate / Surface enum. Keywords: hook install, binary pinning, SKIM_HOOK_BINARY, SKIM_HOOK_COMMIT, resolve_skim_binary, generate_hook_script, uses_pinned_binary, pin_is_current, script_has_pinned_marker, AwarenessOnly, codex, wrapper symlinks, guidance_content, PermissionsProtocol, confirm_grant, READ_ONLY_SUBCOMMANDS, hook_config_dir, seed tier, sidecar manifest, proxy feature gate, KNOWN_SUBCOMMANDS, META_SUBCOMMANDS, WRAPPER_TARGETS, wrapper_targets, dispatch_for_wrapper, dispatch_inner, dispatch_explicit, passthrough_strips_json, Surface, command_needs_exact_bytes, is_bare_cat_pipeline, BYTE_EXACT_PIPE_CONSUMERS, force_raw_requested, set_force_raw, session_sidecar, isatty, ScriptIntegrity, classify_script_integrity, hook_status_line, HookFacts, NoManifest, Tampered, Verified, Unreadable, binary pin mismatch, skim_sandboxed, skim_sandboxed_with_bin, hermetic_path, SKIM_CACHE_DIR, require_flags_for_tool, skip_flags_for_tool, AD-RW-2, byte loss 304 6803, #514, ADR-004, ADR-005, ADR-006, ADR-008, PF-010, PF-015, PF-017."
category: domain-knowledge
directories: [crates/rskim/src/cmd/hooks, crates/rskim/src/cmd/init, crates/rskim/src/cmd/rewrite, crates/rskim/src/cmd/permissions]
created: 2026-07-04
updated: 2026-09-02
---

# Agent Hook Install, Binary Pinning & Handshake (+ Permissions Seeding)

## Overview

This feature area covers how skim installs itself as a PreToolUse hook into AI agent runtimes (Claude Code, Cursor, Gemini CLI, Copilot CLI, Crush) and as awareness-only files into Codex CLI. The central problem it solves is the **wrong-clone hazard**: multiple skim clones on different branches can report identical semver strings. Without binary pinning, the installed hook might silently exec the wrong clone.

The solution is a pinned-binary hook script format (F6 / PR #421) embedding the canonicalized absolute path plus a git short SHA. A SHA-256 manifest sidecar enables tamper detection independent of the script's own content.

Post-branch changes in this area:
- **`WRAPPER_TARGETS` is now derived** from `KNOWN_SUBCOMMANDS` minus `META_SUBCOMMANDS` via `LazyLock` — ending the separately maintained list that had silently drifted (e.g., `golangci` vs. the real binary name `golangci-lint`).
- **Unknown input now execs the real tool** instead of `bail!`-ing — converting nine families of hard failures into correct native behavior.
- **`dispatch_for_wrapper()` is the canonical wrapper entry point** — it applies D3 (help/version passthrough), D4 (tool skip flags), and D5 (require-flag passthrough) before routing to the private `dispatch_inner(Surface::Wrapper, …)`.
- **`dispatch_explicit()`** is the public entry point for the explicit `skim <tool>` surface, tagging the call as `Surface::Explicit` before delegating to `dispatch_inner`.
- **`Surface` enum** (`Explicit` / `Wrapper`) is compile-time enforced: `dispatch_inner` requires it, making it impossible to call the shared core without declaring the surface.
- **AD-RW-2 reversal for `| cat`:** the rewrite engine now rewrites `<source> | cat` (bare `cat`, no args, no redirects, exactly two segments) to `<rewritten source> | cat`. All other pipe shapes still refuse.
- **Force-raw sidecar architecture** connects the rewrite engine's static view of pipeline shape to the wrapper surface's runtime fidelity gate.
- **The stdout destination gate uses a real `isatty(1)`** instead of `is_char_device()` as a proxy.
- **`cli_cross_surface_conformance.rs`** tests both surfaces in a single (tool × arg set) matrix.

## Business Context

**The wrong-clone hazard in practice**: parallel skim clones at the same version. Without the binary pin + SHA check, the hook would exec the wrong binary silently. `skim doctor` uses the manifest sidecar — not the hook text — to derive tamper verdicts.

**Constraints**: Hard zero-stderr invariant in hook mode (#361 Bug 3). Hooks must never fail. Script must be shell-safe for paths with spaces and single quotes. Reinstall idempotence. Permissions seeding is TTY-gated with no bypass flag.

## Core Business Rules

### `WRAPPER_TARGETS` — Derived, Not Hand-Maintained

```rust
// crates/rskim/src/cmd/registry.rs
static WRAPPER_TARGETS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    KNOWN_SUBCOMMANDS
        .iter()
        .copied()
        .filter(|&s| !is_meta_subcommand(s))
        .collect()
});
```

`WRAPPER_TARGETS` is now computed from `KNOWN_SUBCOMMANDS` minus `META_SUBCOMMANDS` at first use — no separate hand-maintained list. The prior hand-maintained list had silently drifted: `golangci` was installed as a wrapper symlink but the real binary is `golangci-lint`. The bare `golangci` alias has been removed from `dispatch_inner`; `golangci-lint` is the only routing key. Three tests guard the derivation: wrapper_targets contains no meta subcommands, wrapper_targets count equals `KNOWN_SUBCOMMANDS.len() - META_SUBCOMMANDS.len()`, and both must remain sorted (binary-search invariant).

### `dispatch_for_wrapper()` — Canonical Wrapper Entry Point

**Critical Invariant 4 (reintroducing this is a known bug):** `dispatch_for_wrapper()` must be the wrapper-surface entry point; `dispatch_inner()` must never be called directly on the wrapper surface. Calling `dispatch_inner` directly silently bypasses D3, D4, and D5.

`dispatch_for_wrapper` applies, in order:
- **D3**: Universal help/version passthrough for non-meta tool wrappers — `--help`, `-h`, `--version`, `-V` always exec the real tool's own help. Note: `-h` is the `host` flag for `psql`/`mysql` — this is over-broad but fail-safe (raw passthrough is always correct).
- **D4**: Tool-owned skip flags from rewrite rules (`skip_flags_for_tool`) — e.g., `rg --json` enables rg's own JSON output.
- **D5**: Require-flag passthrough via `require_flags_for_tool` — tools like `psql` (requires `-c`/`--command`) and `mysql` (requires `-e`/`--execute`) gate their rewrite rule on a required flag; without it, the tool opens an interactive session skim must not intercept. `require_flags_for_tool` reads the same `rules::all_rules()` table as `skip_flags_for_tool` so the two surfaces cannot drift independently.
- **Then**: routes to `dispatch_inner(Surface::Wrapper, …)`.

### `dispatch_explicit()` — Canonical Explicit-Surface Entry Point

`dispatch_explicit(subcommand, args, analytics)` is the public entry point for `main.rs`'s `Invocation::Subcommand` arm (the `skim <tool>` path). It tags the call as `Surface::Explicit` and delegates to `dispatch_inner`. No D3/D4/D5 gates are applied on the explicit surface — those are wrapper-surface concerns.

### `Surface` Enum and `dispatch_inner`

The `Surface` enum (`Explicit` | `Wrapper`) is compile-time enforced: `dispatch_inner(surface, …)` is a private function that requires it. This makes it impossible to call the shared core without explicitly declaring which surface the call is on. `passthrough_strips_json(subcommand)` is the single predicate that determines whether a bare `--json` token survives `strip_skim_flags` for a given tool (true for `git` only).

### `command_needs_exact_bytes` — Single Extension Point for Byte-Exact Destinations

**Critical Invariant 3:** `command_needs_exact_bytes` in `cmd/rewrite/compound.rs` is the **single extension point** for byte-exact destination shapes. `BYTE_EXACT_PIPE_CONSUMERS` is a **denylist by deliberate choice** (not an allowlist).

**Why a denylist:** pipes are ambiguous at the fd level — skim cannot tell `| cat` (compress is correct) from `| tee out.txt` (compress is data loss). The denylist names the specific consumers that require exact bytes. Unknown consumers default to compression (the tool's core value).

Rules in cost order:
- **Rule S** (stdout redirect `>`, `>>`, `1>`, `&>`): any stdout redirect → exact bytes required
- **Rule R** (pipe consumer is in `BYTE_EXACT_PIPE_CONSUMERS`): `tee`, `sha256sum`, `dd`, `base64`, etc.
- **Rule T** (command substitution, process substitution, backticks): `$(…)`, `<(…)`, `` `…` `` → wildcard sidecar, treated as exact-bytes-required (partial knowledge → conservative)

### AD-RW-2 Reversal: `| cat` Now Rewrites

`is_bare_cat_pipeline(segments)` returns `true` for **exactly one pipeline shape**: two segments joined by a single `|`, the source carrying no redirects, the consumer being the single token `cat` with no arguments and no trailing operator. `try_rewrite_compound` now bails on pipes only when `has_pipe_operator && !is_bare_cat_pipeline` — the bare-`cat` exception passes through.

The safety gate is `command_needs_exact_bytes(&rejoin_segments(segments))`: if the rejoined text triggers any of rules S/R/T, `is_bare_cat_pipeline` returns false and the pipeline still refuses. This is what keeps `| cat > f` (rule S), `| cat | tee f` (rule T), and `| cat -n` (no longer bare `cat`) out of scope.

The hook's `set_force_raw` verdict for `<source> | cat` is `false` by design — the explicit-subcommand path the rewrite emits compresses into the FIFO intentionally, and that is skim's entire purpose. An agent defeating a pager is the consumer, not a byte-persisting stage.

Remaining pipe-refusal shapes: `| tee f`, `| cat > f`, `| cat -n`, `| cat file`, three stages, mixed `&&`/`|`, any redirect on the source. Measured: `&&` and `;` compounds ARE rewritten segment by segment; pipes/redirects/`$(…)`/backticks/heredocs/`<(…)` refuse (`rewrite_would_corrupt` or the pipe guard).

### The Stdout Destination Gate and Force-Raw Sidecar

Two independent surfaces detect that skim's output will reach a file and must serve raw bytes:

**Rewrite surface (static text scan):** `stdout_redirected_to_file` in `cmd/rewrite/compound.rs` scans the command text before exec. When `command_needs_exact_bytes` detects a file-destination shape, `set_force_raw` writes a per-tool sidecar marker to `{SKIM_CACHE_DIR}/sessions/{ppid}.{tool}.raw`.

**Wrapper surface (runtime):** `stdout_should_serve_raw()` in `main.rs` uses a real `isatty(1)` test (not `is_char_device()` — the prior proxy misclassified `/dev/null`, `/dev/zero`, and `/dev/tty`). `force_raw_requested(tool)` additionally reads the sidecar written by the rewrite surface.

**Key: partial knowledge → wildcard.** When the rewrite engine cannot identify the pipe-source tool (exec-prefix launchers like `timeout 60 git log`, unrepresentable shapes, head-count overflow), it writes the wildcard marker `{ppid}.raw` which matches every tool. A sidecar was chosen over an env-var prefix because an env prefix shifts the command into a namespace host permission matchers no longer match (PF-010), and it is not semantics-preserving for compound shapes (PF-004).

**Sidecar key is `{ppid}.{tool}.raw`.** PPID alone is not a command identity: every command an agent runs shares that PID, so a PPID-only key made one command's verdict decide unrelated concurrent, background, and nested-sub-agent wrapper invocations. The tool component comes from `command_heads`; a shape that defeats head extraction falls back to the wildcard `{ppid}.raw`.

**Five early returns in `run_hook_mode` exit before `set_force_raw`:**
1. `SKIM_PASSTHROUGH=1` — returns `ExitCode::SUCCESS` immediately.
2. `AwarenessOnly` agents (Codex) — no hook mechanism, returns immediately.
3. stdin read error — returns `ExitCode::SUCCESS` (passthrough).
4. JSON parse error — returns `ExitCode::SUCCESS` (passthrough).
5. Missing/unparseable command field — returns `ExitCode::SUCCESS` (passthrough).

A marker left by a previously processed command (that the hook fired for) lives until the next successfully processed command calls `set_force_raw` (which clears as well as sets), or until the 300 s reap (`FORCE_RAW_MAX_AGE`). Critically: **a stale or missing marker does NOT merely cost compression — it can cost bytes**. Measured 304 vs 6803 bytes written into `| tee f` after a same-tool `git status` clear invalidated the `git log` marker (#514), and the same loss occurs when no hook fires at all. The append-only/monotone-OR redesign is an open owner decision (see #514 comment). The old claim "stale/missing marker costs compression, never bytes" was incorrect and has been removed from CLAUDE.md.

**Sidecar reaping:** markers are reaped on a 300 s clock (`FORCE_RAW_MAX_AGE`). Previously `sessions/` was unbounded because cleanup only ran from `write_session_id`, which never fires when no session id is supplied.

### `SKIM_CACHE_DIR` Requirement for Rewrite Hook Tests

**Critical Invariant 2:** Any test invoking `skim rewrite --hook` must override `SKIM_CACHE_DIR`. The D5 force-raw sidecar is keyed `{ppid}.{tool}.raw`. Under a shared nextest runner, the PPID is the same across tests in the same binary — a marker written by one test is readable by a different test and silently flips the wrapper to raw passthrough.

This caused four argv0-surface tests to fail while their hook-surface twins passed. The failure was not reproducible under `cargo test` (which forks per-test) but always reproducible under nextest. `cli_cross_surface_conformance.rs` sets `SKIM_CACHE_DIR` to a per-test temp dir.

Relates to PF-017: any test shelling out to `skim init`/`--uninstall`/`doctor`/`rewrite --hook` must use `skim_sandboxed_with_bin` or set `SKIM_CACHE_DIR` explicitly.

### `resolve_skim_binary()` — Single Source of Truth for Binary Path

`resolve_skim_binary()` in `init/helpers.rs` calls `current_exe()` then `canonicalize()`. Three sites that write or compare binary paths MUST all call this helper:
1. `create_hook_script` — embeds path as `SKIM_HOOK_BINARY`
2. `detect_state` — stores in `DetectedState.skim_binary`
3. `maybe_install_wrappers` — passes as the wrapper symlink target

On macOS, `/tmp → /private/tmp`. On Homebrew installs, the binary sits behind a cellar symlink. CI always passes (no symlinks in the test binary path) — the failure is machine-dependent.

### Currency Predicates and Fast Path

**`hook_is_current()`**: version matches && pinned format && commit matches. Does NOT check path — that is `pin_is_current()`'s job.

**`pin_is_current()`**: compares `hook_binary_pin` against `resolve_skim_binary()`. Used only by `hook_status_line` in `skim doctor` — it is **not a fast-path gate** (ADR-014: provenance machinery is advisory, not enforcing).

**Fast-path** (6 conditions):
```
state.hook_installed && state.hook_is_current()
&& guidance_current && !permissions_blocked
&& !flags.force && manifest_present
```
Wrappers are handled INSIDE the fast-path block (`maybe_install_wrappers` called before `print_already_up_to_date()`).

### ScriptIntegrity: Manifest, Not Script Text

`ScriptIntegrity` four-state enum: `Verified`, `NoManifest`, `Tampered`, `Unreadable`. Doctor derives verdict from the `.sha256` manifest — not from the hook bytes (the hook bytes are exactly what a tamper modifies).

`hook_status_line()` control flow by integrity state:
- `Tampered` → drift (`✗`), early-returns (suppresses drift at hook-exec time for ClaudeCode)
- `Unreadable` → drift (`✗`), early-returns (does NOT suppress drift detection — maps to `false` in `check_hook_integrity`, so drift still runs)
- `NoManifest` → advisory note appended, **falls through** to pin/currency checks (pre-manifest hooks still report stale)
- `Verified` → falls through to pin/currency checks

Terminal `"binary pin mismatch"` case is logically reachable only when version AND commit both match but `!pin_is_current`. The former `"stale"` fallback was dead code and has been removed.

### The Stdout Destination Matrix

`crates/rskim/tests/cli_stdout_destination.rs` pins 18 of 18 cells in a `(command-shape × surface)` matrix. The one remaining structural divergence: the socket cell (explicit-subcommand path is not `fstat`-gated — a parent installs that fd; no syntax exists for a text scan to see it). Two cells were added this session: `rewrite × | cat` (now rewrites) and `rewrite × socket` (structural divergence noted inline).

### Cross-Surface Conformance Testing

`crates/rskim/tests/cli_cross_surface_conformance.rs` drives a (tool × arg set) matrix through **both** rewrite→execute and argv0 wrapper, asserting stdout bytes, exit code, and stderr class. Each legitimate divergence is pinned inline with its reason. `cli_wrapper_argv0.rs` includes D5 tests with fake `psql`/`mysql` on PATH, exercising `require_flags_for_tool` via env controls.

The older `cli_both_surfaces_paired.rs` compared stdout only and its "hook surface" was `skim <tool>` (the subcommand path), not `skim rewrite`. The new conformance harness drives the actual `skim rewrite --hook` JSON path and the argv0 wrapper path independently. **Testing one surface does not verify the other** — they share per-tool handlers but have completely different dispatch front-ends.

### Proxy Feature Gate

The `proxy` subcommand is compiled out of default builds (ADR-008). Enforced at three coordinated places: registry pair-gate (both `KNOWN_SUBCOMMANDS` and `META_SUBCOMMANDS` under the same `#[cfg(feature = "proxy")]`), the routing guard in `main.rs`, and optional Cargo deps. `test_proxy_registry_entries_gated_as_a_pair` asserts the cfg-pair.

### Transparency Marker

`lossy_view_marker` (superseding `rewrite_transparency_marker`) fires when the served view differs from baseline. `rewrite_origin()` reads `SKIM_REWRITTEN_FROM` with a closed vocabulary. Marker is rewrite-engine-only — PATH-wrapper-mediated reads are intentionally unmarked (PF-004). See file-wrapper-fidelity KB for full treatment.

### Permissions Seeding Subsystem

`PermissionsProtocol` is a format-agnostic trait. Seeded list is always `READ_ONLY_SUBCOMMANDS ∩ wrapper_targets()` — exactly 8 tools. `confirm_grant()` is TTY-gated; `--yes` is uninstall-only and does NOT bypass it. Three tiers: `Seed`, `Mirror`, `Blanket`.

## State Transitions

```
detect_state()
  └─ reads hook script once → uses_pinned_binary + parse_version + binary pin
  └─ DetectedState::hook_is_current() = version && pinned format && commit
  └─ DetectedState::pin_is_current() = canonical binary path matches (advisory)
        │
        ├─ all 6 fast-path conditions true:
        │    → maybe_install_wrappers (inside fast path, before early return)
        │    → print "Already up to date", return
        └─ any false → create_hook_script()
                  → atomic_write_executable() → write_hash_manifest (? propagated)
                  → patch_settings() → inject_guidance()
                  → [if --permissions] confirm_grant() → seed()
```

## Anti-Patterns

**Calling `dispatch_inner()` directly on the wrapper surface.** It silently bypasses the D3 help/version passthrough, D4 tool skip flags, and D5 require-flag passthrough that `dispatch_for_wrapper()` applies. The wrapper entry point in `main.rs` is `dispatch_for_wrapper`.

**Calling `dispatch_for_wrapper()` on the explicit surface.** The explicit surface (`skim <tool>`) must call `dispatch_explicit`, not `dispatch_for_wrapper`. The D3/D4/D5 gates belong on the wrapper surface only.

**Adding a new byte-exact destination shape anywhere except `command_needs_exact_bytes`.** A second unmaintained gate will drift out of sync with the denylist.

**Invoking `skim rewrite --hook` in a test without overriding `SKIM_CACHE_DIR`.** Under nextest the D5 force-raw sidecar is PID-keyed globally — one test's sidecar silently flips another test's wrapper to passthrough.

**Treating both dispatch surfaces as equivalent for rewrite tests.** The `skim rewrite --hook` path (stdin JSON → `try_rewrite()`) and the argv0 wrapper surface share per-tool handlers but have completely different front-ends. A test driving one does not cover the other.

**Assuming a stale or missing force-raw marker costs only compression.** Measured 304 vs 6803 bytes into `| tee f` after a same-tool `git status` marker clear invalidated the `git log` marker (#514). Data loss is possible. Do not add "marker-safe" reasoning without measuring.

**Adding a new required script line gated in only one currency predicate.** The shared `script_has_pinned_marker` updates both predicates. A line gated in only one silently desyncs state detection from reinstall.

**Emitting anything to stderr in hook mode.** All hook-mode diagnostics go to `hook.log` via `log_hook_warning` only. Zero-stderr invariant (GRANITE #361 Bug 3).

**Deriving doctor's verdict from the hook script text.** The hook bytes are exactly what a tamper modifies. Doctor must use `classify_script_integrity` (reads the SHA-256 manifest) and the `HookFacts.script_integrity` field.

**Adding `--yes` bypass to `confirm_grant`.** The `--yes` flag is uninstall-only.

**Using `let _ = write_hash_manifest(...)`.** The manifest write must use `?`.

**Running init/doctor/rewrite-hook tests without `skim_sandboxed_with_bin`.** Tests without sandboxing will mutate the developer's real `~/.claude`, `~/.gemini`, `~/.skim/bin`, etc. (PF-017).

**Trying to reach pin-mismatch in doctor by editing the hook script.** Any edit trips `Tampered`, which early-returns before the pin block. Install from a binary at one path and run doctor from another — the copy-binary technique.

**Mis-gating the proxy registry entries.** `"proxy"` must appear in both `KNOWN_SUBCOMMANDS` and `META_SUBCOMMANDS` under the **same** `#[cfg(feature = "proxy")]`.

## Gotchas

**`WRAPPER_TARGETS` is now derived, not maintained.** Adding a tool to `KNOWN_SUBCOMMANDS` automatically adds it to wrapper_targets (if it is not in `META_SUBCOMMANDS`). The old hand-maintained list had `golangci` when the real binary is `golangci-lint`; the derived list uses whatever name is in `KNOWN_SUBCOMMANDS`. The bare `golangci` alias is gone from `dispatch_inner`.

**`is_bare_cat_pipeline` is the only pipe exception.** `| cat` (bare, no args, exactly two segments, no redirects on source, `command_needs_exact_bytes` returns `false`) now rewrites. Every other pipe shape refuses. Adding a second pipe exception anywhere outside `is_bare_cat_pipeline` / `try_rewrite_compound` creates an unmaintained gate.

**D5 `-h` is over-broad for psql/mysql.** `-h` means `host`, not `--help`, for those tools. The D3 check treats it the same as `--help` and passes through to the real tool. This is fail-safe (raw passthrough is always correct) but means `psql -h localhost` is never compressed. Filed as a known limitation.

**Unknown tool names now exec the real tool.** The wrapper dispatch previously `bail!`-ed on an unrecognised argv0, producing exit 1 with no output. Unknown tools now pass through to the real binary via the `_` arm in `dispatch_inner`.

**`isatty(1)` not `is_char_device()`.** Prior gate misclassified `/dev/null`, `/dev/zero`, and `/dev/tty` as terminals. Code using `FileType::is_char_device()` as an isatty proxy is wrong for these cases.

**Force-raw sidecar is per-PPID.** Under a shared nextest runner, multiple test processes share the same PPID — a sidecar written by one test binary is readable by another. Always override `SKIM_CACHE_DIR` to a temp dir in any test that exercises the rewrite→wrapper path.

**Sidecar reap clock.** Sidecars expire after 300 s (`FORCE_RAW_MAX_AGE`). A test that inspects sidecar behavior after 300 s will see it gone. The prior behavior was unbounded (no reap at all when no session id was set).

**Five early returns in `run_hook_mode` skip `set_force_raw`.** SKIM_PASSTHROUGH, AwarenessOnly, stdin read error, JSON parse error, and missing command field all return before the marker is written or cleared. A marker written by the previous hook invocation survives until the next successful invocation or the 300 s reap — it can cause byte loss, not just missed compression.

**`resolve_skim_binary()` is machine-dependent.** Green CI does not prove the three-site invariant — CI binaries have no symlinks. The failure appears on macOS with Homebrew installs or symlinked-bin layouts.

**`check_hook_binary_mismatch` fires only on same-version.** When versions differ, `check_hook_version_mismatch` logs and returns without calling the binary/commit check.

**`NoManifest` falls through to pin/currency checks.** Pre-manifest installs are advisory (`⚠`), not drift, but they no longer suppress the stale/path-mismatch checks.

**`Tampered` suppresses drift at hook-exec time; `Unreadable` does NOT.** Three of `DriftEnv::from_process()`'s six fields come from env vars the hook script exports — a tampered script cannot be trusted. `Unreadable` returns `false` from `check_hook_integrity()`, so drift detection still runs.

**Doctor-vs-hook asymmetry.** Hook-time integrity checking is ClaudeCode-only. Doctor checks all agents. A tampered Gemini script shows `✗` in `skim doctor` but produces no warning at hook-exec time.

**`detect_installed_agents()` in override-mode requires config dir to exist.** Tests must `create_dir_all(home.join(".claude"))` before calling init or the agent appears uninstalled.

**`skim doctor` scans `$PATH` — use `hermetic_path()` in tests.** Without restricting PATH, doctor may detect PATH drift from an unrelated release build.

## Key Files

- `crates/rskim/src/cmd/registry.rs` — `KNOWN_SUBCOMMANDS`, `META_SUBCOMMANDS`, `WRAPPER_TARGETS` (`LazyLock` derived), `wrapper_targets()`, `READ_ONLY_SUBCOMMANDS`; sort-invariant tests
- `crates/rskim/src/cmd/dispatch.rs` — `dispatch_for_wrapper()` (D3/D4/D5 gates); `dispatch_explicit()` (explicit surface entry); private `dispatch_inner(surface, …)` (shared core); `Surface` enum; `passthrough_strips_json()`; B1 `SKIM_PASSTHROUGH` convergence gate; `MULTI_LEVEL_DISPATCHERS`, `HANDLER_CONSUMED_TOKENS`
- `crates/rskim/src/cmd/rewrite/compound.rs` — `command_needs_exact_bytes()` (single extension point); `is_bare_cat_pipeline()` (AD-RW-2 `| cat` exception); `BYTE_EXACT_PIPE_CONSUMERS` (denylist); `try_rewrite_compound()` (pipe guard: `has_pipe_operator && !is_bare_cat_pipeline`); `rejoin_segments()`; `set_force_raw()`, `read_force_raw()`
- `crates/rskim/src/cmd/rewrite/mod.rs` — `skip_flags_for_tool()`; `require_flags_for_tool()` (D5 source); `classify_command()`
- `crates/rskim/src/cmd/rewrite/hook.rs` — `run_hook_mode()`; five early returns before `set_force_raw`; `check_hook_binary_mismatch()`; `check_hook_integrity()` (ClaudeCode-only)
- `crates/rskim/src/cmd/session_sidecar.rs` — `set_force_raw`/`read_force_raw`; `{ppid}.{tool}.raw` key; wildcard `{ppid}.raw`; `FORCE_RAW_MAX_AGE` (300 s)
- `crates/rskim/src/main.rs` — `dispatch_for_wrapper` call site; `dispatch_explicit` call site; `stdout_should_serve_raw()` (`isatty(1)`, not `is_char_device()`); `force_raw_requested()`
- `crates/rskim/src/cmd/integrity.rs` — `ScriptIntegrity` enum; `classify_script_integrity()`; `verify_script_integrity`; `compute_file_hash()`; `write_hash_manifest()`
- `crates/rskim/src/cmd/init/mod.rs` — `run()` init dispatch; `script_has_pinned_marker()`; `HookFacts`; `hook_facts()`
- `crates/rskim/src/cmd/init/helpers.rs` — `resolve_skim_binary()` (single canonical path source); `confirm_grant()` (TTY-gated consent gate)
- `crates/rskim/src/cmd/init/install.rs` — `run_install_single()` (fast path: 6 gates; wrappers inside fast path); `create_hook_script()`; `atomic_write_executable()`; `maybe_install_wrappers()`
- `crates/rskim/src/cmd/init/state.rs` — `detect_state()`; `hook_is_current()`; `pin_is_current()`; `parse_version_from_script`
- `crates/rskim/src/cmd/doctor/mod.rs` — `hook_status_line()` (manifest-first; "binary pin mismatch" terminal; `commit_ok` treats "unknown" as indeterminate); `print_hook_section()`
- `crates/rskim/src/cmd/hooks/mod.rs` — `generate_hook_script()`, `shell_single_quote()`, `HookProtocol` trait
- `crates/rskim/tests/common/mod.rs` — `skim_sandboxed_with_bin(home, bin)` (authoritative sandbox); `skim_sandboxed(home)` (delegates to above)
- `crates/rskim/tests/cli_cross_surface_conformance.rs` — (tool × arg set) matrix across both surfaces; SKIM_CACHE_DIR per-test temp dir; pinned divergences
- `crates/rskim/tests/cli_stdout_destination.rs` — 18/18 pinned stdout-destination matrix; socket cell is the one remaining structural divergence
- `crates/rskim/tests/cli_wrapper_argv0.rs` — D5 tests with fake psql/mysql on PATH; env controls
- `crates/rskim/tests/cli_doctor.rs` — `hermetic_path()`; `test_doctor_exits_1_on_binary_pin_mismatch` (copy-binary technique)

## Related

- ADR-006: Hook responses never self-approve; permissions seeding is consent-gated; `confirm_grant` enforces pre-checks
- ADR-011: Stderr notice taxonomy; no-loss raw-fallback banners are debug-gated; loss-bearing elision markers are unconditional
- ADR-015: (check index for content)
- PF-004: Two interception surfaces (rewrite engine vs PATH wrappers) — `dispatch_explicit` is the rewrite entry; `dispatch_for_wrapper` is the wrapper entry; they share `dispatch_inner` but have different front-ends
- PF-010: Rewrite creates a parallel command namespace — host permission matchers stop matching; env-var prefixing (rejected alternative to sidecar) has the same problem
- PF-015: Provenance mechanism fails in ways its own tests cannot see; copy-binary E2E technique for pin-mismatch coverage
- PF-017: Installer tests must sandbox `$HOME` + all five agent config-dir overrides; `SKIM_CACHE_DIR` additionally required for rewrite-hook tests
- PF-025 rule 8: `require_flags_for_tool` drift guard — D4 (`skip_flags_for_tool`) and D5 (`require_flags_for_tool`) read the same rule table; adding a tool-specific required flag in rules.rs automatically propagates to both surfaces
- PF-026: (check index for content)
- ADR-004: Hook install pins absolute binary path + commit SHA; `resolve_skim_binary()` is the canonicalization implementation
- ADR-005: Guidance framed as calibrated trust; prohibits `SKIM_PASSTHROUGH` in guidance template
- ADR-008: Default builds never link async/TLS/HTTP; `proxy` cfg-pair is the runtime enforcement
- Feature: `file-wrapper-fidelity` — `dispatch_for_wrapper()`, the B1 convergence gate for `SKIM_PASSTHROUGH`, and the force-raw sidecar architecture are shared between the two feature areas; `Surface` enum and `dispatch_inner`/`dispatch_explicit` split also documented there
- Source: `crates/rskim/src/cmd/hook_log.rs` — `log_hook_warning()` (only permitted diagnostic channel in hook mode)
- Tests: `crates/rskim/tests/cli_integrity.rs` — E2E tests for `classify_script_integrity` and `hook_status_line`
