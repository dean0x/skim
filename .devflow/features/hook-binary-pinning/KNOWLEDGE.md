---
feature: hook-binary-pinning
name: Agent Hook Install, Binary Pinning & Handshake (+ Permissions Seeding)
description: "Use when modifying hook script generation, adding new agents, changing the hook script format, debugging version-skew or wrong-clone warnings, working on install/reinstall logic, touching wrapper symlink management, editing guidance_content() / the Command wrapping section, or working on skim init --permissions / the PermissionsProtocol subsystem, or modifying the proxy feature gate / cfg-gated registry entries, or touching the rewrite transparency marker (SKIM_REWRITTEN_FROM / rewrite_origin / rewrite_transparency_marker / view_differs), or working on doctor integrity checks / ScriptIntegrity / hook_status_line, or adding test harness sandboxing (skim_sandboxed / skim_sandboxed_with_bin / hermetic_path). Keywords: hook install, binary pinning, SKIM_HOOK_BINARY, SKIM_HOOK_COMMIT, resolve_skim_binary, generate_hook_script, is_hook_script_current, uses_pinned_binary, pin_is_current, script_has_pinned_marker, AwarenessOnly, codex, wrapper symlinks, wrappers_blocks_fast_path, guidance_content, guidance_content_mdc, Command wrapping, PermissionsProtocol, confirm_grant, READ_ONLY_SUBCOMMANDS, hook_config_dir, modifiedArgs, seed tier, sidecar manifest, proxy feature gate, cfg-gated registry, routing guard, cli_proxy_gating, KNOWN_SUBCOMMANDS, META_SUBCOMMANDS, wrapper_targets, uses_dedicated_hook_file, hook_event_key, print_install_summary, print_dry_run_actions, print_dry_run_permissions, SKIM_JSON_NAME, SKIM_REWRITTEN_FROM, rewrite_origin, rewrite_transparency_marker, view_differs, transparency marker, origin tag, ScriptIntegrity, classify_script_integrity, verify_script_integrity, hook_status_line, HookFacts, NoManifest, Tampered, Verified, Unreadable, binary pin mismatch, skim_sandboxed, skim_sandboxed_with_bin, hermetic_path, ADR-004, ADR-005, ADR-006, ADR-008, PF-015, PF-017."
category: domain-knowledge
directories: [crates/rskim/src/cmd/hooks, crates/rskim/src/cmd/init, crates/rskim/src/cmd/rewrite, crates/rskim/src/cmd/permissions]
created: 2026-07-04
updated: 2026-08-18
---

# Agent Hook Install, Binary Pinning & Handshake (+ Permissions Seeding)

## Overview

This feature area covers how skim installs itself as a PreToolUse hook into AI agent runtimes (Claude Code, Cursor, Gemini CLI, Copilot CLI, Crush) and as awareness-only files into Codex CLI. The central problem it solves is the **wrong-clone hazard**: multiple skim clones on different branches can report identical semver strings. Without binary pinning, the installed hook might silently exec the wrong clone — one that was built from a different branch — and produce subtly broken rewriting behavior.

The solution is a pinned-binary hook script format (introduced in F6 / PR #421) that embeds the canonicalized absolute path of the generating binary at install time, along with a git short SHA for same-version divergent-build detection. A SHA-256 manifest sidecar (`.sha256` file alongside the hook script) enables tamper detection independent of the script's own content — `skim doctor` derives its verdict from the manifest, not from the hook bytes that a tamper would edit.

v2.11.0 added the **permissions seeding subsystem** (`skim init --permissions`): a TTY-gated, human-consent-only channel for seeding agent-native allowlist entries for skim's read-only tool wrappers. This is entirely separate from runtime hook behavior — hooks never write permissions.

## Business Context

**The wrong-clone hazard in practice**: this machine keeps parallel skim clones to avoid worktree churn. If clone A generates the hook script and clone B builds to the same version (no semver bump during development), the hook script would exec clone A's binary, but `check_hook_binary_mismatch` only fires if the embedded `SKIM_HOOK_COMMIT` differs from the running binary's compile-time SHA. Without that check, a developer rebuilding in-place with `cargo build` would silently have the hook exec the previous build.

**Constraints that shape the design**:
- Hard zero-stderr invariant in hook mode (#361 Bug 3): mismatch warnings go to `hook.log` ONLY
- Hooks must never fail (passthrough on any error, timeout, or AwarenessOnly agent)
- Script content must be shell-safe for any file path, including paths with spaces and embedded single quotes
- Reinstall idempotence: a script that is already current must not be regenerated (but the manifest IS always rewritten — see self-heal below)
- Permissions seeding is user-scope only (`--project` is mutually exclusive); consent is TTY-gated with no bypass flag

## Core Business Rules

### The Generated Hook Script Format

`generate_hook_script` in `hooks/mod.rs` is the single source of truth for script content. All RealHook agents must produce scripts via this shared function. The format (F6) embeds `SKIM_HOOK_VERSION`, `SKIM_HOOK_BINARY`, `SKIM_HOOK_COMMIT`, and `_SKIM_BIN` — single-quoted via `shell_single_quote()`. The PATH fallback (`exec skim rewrite --hook`) is a safety net only.

### resolve_skim_binary() — Single Source of Truth for Binary Path

`resolve_skim_binary()` in `init/helpers.rs` calls `current_exe()` then `canonicalize()` and is the **only** place where the running binary's canonical absolute path is computed. Three sites that write or compare binary paths MUST all call this helper so they agree byte-for-byte:

1. `create_hook_script` — embeds the path as `SKIM_HOOK_BINARY` in the hook script
2. `detect_state` — stores the resolved path in `DetectedState.skim_binary`
3. `maybe_install_wrappers` — passes the path as the wrapper symlink target

Before this helper existed, `state.rs` used bare `current_exe()` while `create_hook_script` used `canonicalize()`. On macOS and any system where the binary sits behind a symlink (e.g. `/tmp → /private/tmp`, Homebrew cellar, `cargo install`), those two paths differed — causing `pin_is_current()` to return `false` on every run and triggering an infinite reinstall loop. **The failure is machine-dependent**: CI passes because the test binary's path has no symlinks, but a developer with a symlinked binary will see churn. A green CI run is not proof the path-comparison invariant holds.

### The Currency Predicates

Three predicates determine script staleness. Together they form a hierarchy; the fast-path idempotence check requires all three to be true.

**`uses_pinned_binary` (`init/state.rs`)** delegates to `script_has_pinned_marker` to check whether the installed script uses the F6 pinned format. This is the ONLY place the marker string is scanned; any change to the marker must go here.

**`is_hook_script_current` (`init/install.rs`)** ANDs its own version-line check with the pinned-marker scan, and **also validates the binary pin path** by calling `parse_binary_pin_from_script()` and comparing to `resolve_skim_binary()`. If the pin path in the script differs from the current binary's canonical path (two-clone same-commit scenario), it returns `false` and triggers a rewrite. This three-part check means a script that is correct in version, format, and path will not be needlessly regenerated.

**`DetectedState::hook_is_current()`** combines version match AND pinned format: `version matches && hook_uses_pinned_binary`. It does NOT check path — that is `pin_is_current()`'s job. The separation is intentional (see below).

**`DetectedState::pin_is_current()` (new in PR #488)** compares the canonical binary path stored in `hook_binary_pin` against the result of `resolve_skim_binary()`. It is **deliberately separate from `hook_is_current()`** for two reasons: (1) `hook_is_current()` + commit checks are what skim doctor uses to derive the *cause* of a mismatch — folding pin into `hook_is_current()` would collapse every pin mismatch into the generic `[stale]` bucket; (2) doctor can display the `hook_binary_pin` field independently via `HookFacts.pin_is_current` (PF-015 display-without-gate pattern). Absent pin → `false`.

**Fast-path condition in `run_install_single`** now requires all five conditions:
```
state.hook_installed
&& state.hook_is_current()
&& state.pin_is_current()     // ← new: catches two-clone same-commit case
&& guidance_current
&& !permissions_blocked
&& !wrappers_blocked          // ← new: --wrappers bypasses fast path
&& manifest_present
```

**`wrappers_blocks_fast_path(flags)`** mirrors `permissions_blocks_fast_path` with a tri-state `Option<bool>`: `Some(true)` blocks (explicit `--wrappers`), `Some(false)` never blocks (explicit `--no-wrappers`), and `None` **must return `false`** — this is load-bearing. If `None` blocked, every non-TTY `skim init` invocation (CI, test harness) would fall through and reinstall on each run, breaking idempotence tests. The `print_wrapper_install_result` PATH-setup blurb is now gated on `result.created + result.updated > 0` so it does not appear on idempotent re-runs.

### ScriptIntegrity: Doctor Derives Verdict from Manifest, Not Hook Text

`cmd/integrity.rs` provides a four-state enum and the classifier that `skim doctor` uses to determine tamper status:

```rust
pub(crate) enum ScriptIntegrity {
    Verified,    // hash matches stored manifest — script is unmodified
    NoManifest,  // no .sha256 manifest present — pre-manifest install (backward compat)
    Tampered,    // script contents differ from stored hash
    Unreadable,  // script file cannot be read (missing, permission denied)
}
```

`verify_script_integrity` is a **thin bool wrapper** over `classify_script_integrity` that maps `Verified|NoManifest → Ok(true)`, `Tampered → Ok(false)`, `Unreadable → Err(...)`. It preserves the existing call contract for the two legacy callers (`cmd/rewrite/hook.rs` and `cmd/init/uninstall.rs`).

**Why the manifest, not the script text:** the hook script bytes are exactly what a tamper modifies. A doctor that reads the `SKIM_HOOK_VERSION` comment out of the script and trusts it would be fooled by any modification that preserves the comment. The SHA-256 manifest is an independent artefact written at install time and stored alongside the script.

**`HookFacts.script_integrity`:** `hook_facts()` in `init/mod.rs` calls `classify_script_integrity` for every agent and stores the result in `HookFacts`. Doctor reads `HookFacts.script_integrity` and passes it to `hook_status_line()`.

**`hook_status_line()` in `doctor/mod.rs`:** a pure, testable function that produces `(bool, String)`. It checks `ScriptIntegrity` **before** pin/currency checks. Control flow by integrity state:
- `Tampered` → drift (`✗`), early-returns. Names the drift-suppression coupling (#479 on the hook-exec channel).
- `Unreadable` → drift (`✗`), early-returns. Does NOT claim drift detection is silenced — `Unreadable` maps to `integrity_failed=false` in `check_hook_integrity()`, so drift detection still runs at hook-exec time. Only `Tampered` silences drift.
- `NoManifest` → advisory note (`⚠ no integrity manifest…`) appended, **no early return** — falls through to pin/currency checks.
- `Verified` → falls through to pin/currency checks.

**Pin/currency block (`!hook_is_current || !pin_is_current`):** only reachable for `Verified` and `NoManifest`. The reason chain:
1. If `commit_ok` is false → `"commit mismatch (hook: …, binary: …)"`. When the compiled commit is `"unknown"` (tarball/non-git build), `commit_ok` is forced to `true` — the comparison is indeterminate and must not be reported as a mismatch.
2. Else if `version_ok` is false → `"version mismatch (hook: …, binary: …)"`.
3. Else (version and commit both match, so the mismatch is path-only) → `"binary pin mismatch (hook: {pin}, running: {current_exe})"`. This terminal replaces the former `"stale"` fallback, which was dead code (`commit_ok ∧ version_ok ⇒ hook_is_current`, so the only remaining mismatch is in the pin path).

### Two Fail-Opens Fixed (issue #471)

**Fail-open 1 — `NoManifest` early return suppressed all drift detection:**
Previously `NoManifest` returned early from the hook-status logic, silently bypassing pin/currency drift checks for pre-manifest installs. Now `NoManifest` falls through with an advisory note (`⚠ no integrity manifest...`) appended to whatever the pin/currency check produces. Pre-manifest hooks can still be reported as unpinned or stale.

**Fail-open 2 — `skim init` never regenerated a missing manifest:**
The idempotent early-return path (when `is_hook_script_current()` returns true) previously exited immediately without writing the manifest. Now the early-return path explicitly computes and writes/rewrites the manifest before printing "Skipped". Write errors propagate with `?` — the previous code used `let _ = write_hash_manifest(...)` which silently swallowed failures.

### `check_hook_binary_mismatch` — Same-Version Divergent Build Detection

This function in `rewrite/hook.rs` fires only when versions match. It compares (1) `SKIM_HOOK_BINARY` vs `canonicalize(current_exe())` and (2) `SKIM_HOOK_COMMIT` vs `option_env!("SKIM_GIT_COMMIT")`. Both comparisons skip if the env var is unset (backward compat) and skip the commit comparison if either side is `"unknown"`. Warnings go to `hook.log` via `warn_once_daily()` — never stderr.

**Tampered drift suppression (#479):** at hook-exec time, `detect_drift` is correctly skipped when the script is `Tampered`. Three of `DriftEnv::from_process()`'s six fields (`hook_version`, `hook_binary`, `hook_commit`) are read from env vars the hook script itself exports — a tampered script is one entire side of every comparison and cannot be trusted. The other three fields are binary-derived and tamper-proof. `Unreadable` does NOT suppress drift (`check_hook_integrity` returns `false` for Unreadable) — the unreadable state surfaces via `skim doctor` without blinding the drift channel.

### Codex Is HookSupport::AwarenessOnly

`CodexCliHook` returns `HookSupport::AwarenessOnly`: `generate_script()` returns `""`, `parse_input()` returns `None`, `format_response()` returns `Null`. In `run_hook_mode`, the awareness check fires before any stdin read. Tests for Codex assert NO script and NO handshake.

### Doctor-vs-Hook Asymmetry

**Hook-time integrity checking is claude-code-only.** In `run_hook_mode` (`rewrite/hook.rs`), `check_hook_integrity` is called only inside the `if agent_kind == AgentKind::ClaudeCode` branch. Other agents do not get hook-time integrity checking.

**Doctor checks all agents.** `print_hook_section` in `doctor/mod.rs` iterates `AgentKind::all_supported()` and calls `hook_status_line(&facts, ...)` for every agent. `hook_facts(agent)` calls `classify_script_integrity` for any agent with a hook script path. This means hand-edited gemini/codex/cursor/copilot scripts now report `✗` in `skim doctor` even though no integrity check fires at hook-exec time for those agents.

### Proxy Feature Gate (#352)

The `proxy` subcommand is compiled out of default builds (ADR-008). Enforced in three coordinated places: the registry pair-gate (`"proxy"` appears in both `KNOWN_SUBCOMMANDS` and `META_SUBCOMMANDS` under the same `#[cfg(feature = "proxy")]`), the routing guard in `main.rs`, and optional Cargo deps. The invariant test `test_proxy_registry_entries_gated_as_a_pair` asserts the cfg-pair in both feature configs.

### Transparency Marker for Hook-Rewritten File Reads (SKIM_REWRITTEN_FROM)

When the rewrite engine rewrites a `cat`/`head`/`tail` command into a skim file read, it injects `SKIM_REWRITTEN_FROM=<cat|head|tail>` at the front of the rewritten token list. `rewrite_origin()` reads this from the environment with a closed vocabulary (any other value returns `None`). `rewrite_transparency_marker()` builds the marker string; returns `None` when `differing == 0` (byte-identical output). This tag is rewrite-engine-only — PATH-wrapper-mediated reads are intentionally unmarked (PF-004).

### Permissions Seeding Subsystem (`cmd/permissions/`)

`PermissionsProtocol` is a format-agnostic trait for seeding agent-native allowlist entries. The seeded tool list is always `READ_ONLY_SUBCOMMANDS ∩ wrapper_targets()` — exactly 8 tools. `confirm_grant()` is the primary TTY-gated consent gate; non-TTY stdin → immediate `false`. The `--yes` flag is for hook uninstall confirmation only; it does NOT bypass `confirm_grant`. Three tiers: `Seed`, `Mirror`, `Blanket` — set via `--permissions-tier`.

## State Transitions

The `detect_state` → `run_install_single` flow:

```
detect_state()
  └─ reads hook script once → uses_pinned_binary + parse_version_from_script + binary pin
  └─ DetectedState::hook_is_current() = version matches && pinned format
  └─ DetectedState::pin_is_current() = canonical binary path matches
        │
        ├─ all 7 fast-path conditions true:
        │    hook_installed && hook_is_current() && pin_is_current()
        │    && guidance_current && !permissions_blocked && !wrappers_blocked && manifest_present
        │    → compute hash → write_hash_manifest (self-heal, ? propagated)
        │    → print "Already up to date", return
        └─ any condition false → create_hook_script()
                  → atomic_write_executable() → write_hash_manifest (? propagated)
                  → patch_settings() [or install_hook_registration() for Copilot]
                  → inject_guidance()
                  → [if --permissions] resolve_permissions_consent() → confirm_grant() → seed()
```

## Technical Implementation Patterns

### SHA-256 Sidecar Regeneration Order

In `create_hook_script()`: (1) `atomic_write_executable()` then (2) `compute_file_hash()` + `write_hash_manifest()`. A crash between steps leaves a stale sidecar; the integrity check reports "tampered" — conservatively safe. Write order is intentional. Errors from `write_hash_manifest` propagate with `?` — silently installing without tamper detection is worse than a hard error.

### Shell-Safe Embedding

Binary paths use `shell_single_quote()`. `generate_hook_script` asserts three parameters before embedding: `version` (alphanumeric + `.`/`-`), `agent_cli_name` (alphanumeric + `-`), `git_commit` (ascii-alphanumeric only). All `assert!` calls fire at `skim init` time.

### Wrapper Symlink Stem Rule

`uninstall_wrappers_in` uses `file_stem()` (not `file_name()`) — strips extensions, exact match on `"skim"` or `"rskim"`. `install_wrappers_in` never overwrites non-symlink files (PF-003 safety invariant).

### Test Hermeticity — `skim_sandboxed_with_bin`, `skim_sandboxed`, and `hermetic_path`

`tests/common/mod.rs` provides two helpers. `skim_sandboxed_with_bin(home, bin)` is the **single authoritative sandbox env-var block**; `skim_sandboxed(home)` delegates to it using the default cargo-built binary. Any test shelling out to `skim init`/`--uninstall`/`doctor` must route through one of these helpers or it will mutate the developer's real `~/.claude`, `~/.gemini`, `~/.skim/bin`, etc.

The sandbox sets:
- `HOME` — redirects all `dirs::home_dir()` lookups
- `CLAUDE_CONFIG_DIR`, `GEMINI_CONFIG_DIR`, `COPILOT_CONFIG_DIR`, `CODEX_HOME`, `CRUSH_CONFIG_DIR` — each agent's own config-dir override
- `SKIM_CACHE_DIR` — redirects parser cache and analytics DB
- `SKIM_WRAPPERS_DIR` — redirects wrapper symlink directory
- `SKIM_DISABLE_ANALYTICS=1`, `NO_COLOR=1` — no DB writes, deterministic output

Also removes `SKIM_REWRITTEN_FROM`, `SKIM_PASSTHROUGH`, `SKIM_HOOK_VERSION`, `SKIM_HOOK_BINARY` to start clean.

**`skim_sandboxed_with_bin` is essential for pin-mismatch coverage (PF-015).** The E2E test `test_doctor_exits_1_on_binary_pin_mismatch` copies the test binary to a second path, runs `init` from the copy (so the hook pins to the copy's path), then runs `doctor` from the original binary. Editing the hook script directly would trip `Tampered` (early-return before any pin logic) — making the pin unreachable. The copy-binary approach is the only structurally correct way to test the pin-mismatch path.

**Known remaining gap:** `cli_init.rs`'s older `skim_init_cmd` helper overrides only `CLAUDE_CONFIG_DIR` across 40+ tests — it does not call `skim_sandboxed`. Those tests are not fully hermetic.

**Two additional traps worth recording:**

1. **`detect_installed_agents()` in override-mode requires the config dir to already exist.** Tests must call `std::fs::create_dir_all(home.join(".claude"))` before calling `skim_sandboxed(home)` for init tests — otherwise `detect_installed_agents` returns empty (agent appears not installed).

2. **`skim doctor` scans `$PATH`, so a sibling clone's binary can win and produce spurious drift.** `cli_doctor.rs` has a `hermetic_path()` helper that prepends the test binary's directory to `$PATH`, ensuring the built binary under test wins. Tests that assert on drift must use `hermetic_path()` or they may see false drift from an unrelated release build on the developer's PATH.

## Anti-Patterns

**Adding a new required script line but wiring it into only one currency predicate**: the shared `script_has_pinned_marker` updates both predicates. A new required line gated in only one predicate silently desync state detection from reinstall.

**Emitting anything to stderr in hook mode**: all hook-mode diagnostics go to `hook.log` via `log_hook_warning` only. Zero-stderr invariant (GRANITE #361 Bug 3).

**Deriving doctor's verdict from the hook script text**: the hook bytes are exactly what a tamper modifies. Doctor must use `classify_script_integrity` (which reads the SHA-256 manifest) and the `HookFacts.script_integrity` field — not `parse_version_from_script` or any other scan of the script body.

**Treating both dispatch surfaces as equivalent for rewrite tests**: the rewrite engine (stdin JSON → `try_rewrite()` → JSON response) and the wrapper surface (argv0 dispatch) share per-tool handlers but have completely different front-ends. `SKIM_REWRITTEN_FROM` is rewrite-engine-only.

**Adding `--yes` bypass to `confirm_grant`**: the `--yes` flag is uninstall-only. `confirm_grant` must be called unconditionally whenever permissions are requested.

**Mis-gating the proxy registry entries**: `"proxy"` must appear in `KNOWN_SUBCOMMANDS` and `META_SUBCOMMANDS` under the **same** `#[cfg(feature = "proxy")]` attribute.

**Using `let _ = write_hash_manifest(...)` to swallow write errors**: the manifest write must use `?` — silently installing without tamper detection is worse than a hard error.

**Running `skim init` or `skim doctor` tests without `skim_sandboxed` / `skim_sandboxed_with_bin`**: tests that run init, uninstall, or doctor without sandboxing will mutate the developer's real `~/.claude/`, `~/.gemini/`, etc. (PF-017).

**Trying to reach pin-mismatch in doctor by editing the hook script**: editing any byte of the hook script invalidates the `.sha256` manifest and trips `Tampered`, which early-returns before the pin block is ever evaluated. To reach the pin-mismatch branch you must install from a binary at one path and run doctor from another — the copy-binary technique used in `test_doctor_exits_1_on_binary_pin_mismatch`.

**Setting `wrappers_blocks_fast_path` to return `true` for `None`**: `None` (no `--wrappers` flag) must return `false`. If it returned `true`, every non-TTY `skim init` would reinstall on every run and break idempotence.

## Gotchas

**`resolve_skim_binary()` is machine-dependent.** On macOS, `/tmp → /private/tmp`. On Homebrew installs, the binary sits behind a cellar symlink. A test environment where the binary has no symlinks will pass even if the three-site invariant is broken — the failure only appears on symlinked-path machines.

**`check_hook_binary_mismatch` fires only on same-version**: when versions differ, `check_hook_version_mismatch` logs the version mismatch and returns without calling the binary/commit check.

**`NoManifest` is not drift — but it no longer suppresses drift detection**: pre-manifest installs are advisory only (`⚠`), not drift. However, they now fall through to pin/currency checks so an unpinned pre-manifest hook is still reported as drift.

**Tampered suppresses drift at hook-exec time; Unreadable does NOT**: `check_hook_integrity()` returns `true` (integrity failed) only for `Tampered` — this triggers `detect_drift` to be skipped because three of its six inputs come from env vars the hook script exports. `Unreadable` maps to `Err(_) → false` (integrity check does not signal failure), so drift detection continues to run for an unreadable script. The earlier code comment claiming Unreadable silenced drift was incorrect and has been removed.

**The `"stale"` terminal in `hook_status_line` was dead code and has been removed**: `commit_ok ∧ version_ok` together with `!hook_is_current || !pin_is_current` logically implies the mismatch is in the pin path. The terminal is now `"binary pin mismatch (hook: …, running: …)"`.

**`commit_ok` in `hook_status_line` must mirror `hook_is_current()`**: when the compiled commit is `"unknown"` (tarball build), `commit_ok` is forced to `true` — the comparison is indeterminate. Without this, a genuine pin mismatch gets misattributed as `"commit mismatch (hook: abc1234, binary: unknown)"`. Both the init predicate and the doctor report must treat `"unknown"` consistently.

**`parse_version_from_script` reads the `# skim-hook v{version}` comment first**: because the comment appears before `export SKIM_HOOK_VERSION=` in the script, the scanner finds the comment first. This makes the version marker attacker-editable. Never use this function for security decisions.

**Doctor-vs-hook asymmetry for non-ClaudeCode agents**: `skim doctor` now reports `✗` for tampered codex/gemini/cursor/copilot scripts, but `skim rewrite --hook` only runs `check_hook_integrity` for ClaudeCode. So a tampered Gemini script shows up in doctor but produces no warning at hook-exec time.

**`detect_installed_agents()` in override-mode requires config dir to exist**: when `CLAUDE_CONFIG_DIR` (or any agent's config dir env var) is set, `detect_installed_agents` checks whether that directory exists. Tests using `skim_sandboxed` must `create_dir_all(home.join(".claude"))` before calling init or the agent appears uninstalled.

**`skim doctor` scans `$PATH` — use `hermetic_path()` in tests**: without restricting PATH to the test binary's directory, `skim doctor` may detect PATH drift from an unrelated release build and exit 1, making the test assertion wrong.

**`nextest --all-targets` does not build `target/debug/skim`**: the E2E test harness invokes the `skim` binary from `target/debug/skim`. Always run `cargo build -p rskim` before running `--all-targets` integration tests.

## Key Files

- `crates/rskim/src/cmd/integrity.rs` — `ScriptIntegrity` enum; `classify_script_integrity()`; `verify_script_integrity`; `compute_file_hash()`; `write_hash_manifest()`; `read_hash_manifest()`
- `crates/rskim/src/cmd/init/mod.rs` — `run()` init dispatch; `script_has_pinned_marker()` (single source of truth for the `SKIM_HOOK_BINARY` marker scan); `HookFacts` DTO (includes `pin_is_current`); `hook_facts()` (calls `classify_script_integrity` for every agent)
- `crates/rskim/src/cmd/init/helpers.rs` — `resolve_skim_binary()` (single source of truth for canonical binary path); `guidance_content()`, `guidance_content_mdc()`, `atomic_write_settings()`, `confirm_proceed()`, `confirm_grant()` (TTY-gated consent gate)
- `crates/rskim/src/cmd/doctor/mod.rs` — `hook_status_line()` (pure, testable; checks `ScriptIntegrity` before pin/currency; `!hook_is_current || !pin_is_current` condition; "binary pin mismatch" terminal; `commit_ok` treats "unknown" as indeterminate); `print_hook_section()` (iterates `AgentKind::all_supported()`)
- `crates/rskim/src/cmd/init/flags.rs` — `PermissionsTier`, `InitFlags`, `DetectionEnv`, `detect_installed_agents()`, `resolve_agent()`
- `crates/rskim/src/cmd/init/install.rs` — `create_hook_script()` (uses `resolve_skim_binary()`); `is_hook_script_current()` (checks version + format + binary pin); `atomic_write_executable()`; `wrappers_blocks_fast_path()` (tri-state; None is load-bearing)
- `crates/rskim/src/cmd/init/state.rs` — `detect_state()` (uses `resolve_skim_binary()`); `uses_pinned_binary()`; `DetectedState`; `hook_is_current()` (version + pinned format); `pin_is_current()` (canonical binary path match); `parse_version_from_script` (comment-first; attacker-editable, advisory only)
- `crates/rskim/src/cmd/hooks/mod.rs` — `generate_hook_script()`, `shell_single_quote()`, `HookProtocol` trait, `HookSupport` enum
- `crates/rskim/src/cmd/hooks/copilot.rs` — `CopilotCliHook`: `hook_config_dir` redirect, `uses_dedicated_hook_file`, `write_copilot_skim_json` (atomic); `SKIM_JSON_NAME` constant
- `crates/rskim/src/cmd/rewrite/hook.rs` — `run_hook_mode()`; `check_hook_binary_mismatch()`; `check_hook_integrity()` (ClaudeCode-only; Tampered→true suppresses drift, Unreadable→false does not); `detect_drift()`
- `crates/rskim/src/cmd/registry.rs` — `READ_ONLY_SUBCOMMANDS`, `KNOWN_SUBCOMMANDS`, `META_SUBCOMMANDS`, `wrapper_targets()`; `"proxy"` cfg-gated as a pair
- `crates/rskim/src/cmd/permissions/mod.rs` — `PermissionsProtocol` trait, `permissions_protocol_for_agent` factory
- `crates/rskim/build.rs` — `SKIM_GIT_COMMIT` build-time injection
- `crates/rskim/tests/common/mod.rs` — `skim_sandboxed_with_bin(home, bin)` (authoritative sandbox helper); `skim_sandboxed(home)` (delegates to above); `skim_bin()`
- `crates/rskim/tests/cli_doctor.rs` — `hermetic_path()` helper; `test_doctor_exits_1_on_binary_pin_mismatch` (copy-binary technique for pin-mismatch E2E coverage)
- `crates/rskim/tests/cli_init.rs` — `test_init_rewrites_hook_when_pin_path_differs`; `test_init_wrappers_bypasses_fast_path`; `test_init_skips_when_version_and_commit_are_current`

## Related

- ADR-004 (hook install pins absolute binary path + handshake): mandated the pinned-binary format, daily-rate-limited warn-only signaling; `resolve_skim_binary()` is the implementation of ADR-004's canonicalization requirement
- ADR-005 (guidance framed as calibrated trust): prohibits `SKIM_PASSTHROUGH` in guidance template; "flag it to the user" sentence byte-identical since v2.11.0
- ADR-006 (hook responses never self-approve — permissions seeding is consent-gated): per-host response matrix; `confirm_grant` enforces pre-checks
- ADR-007 (pseudo preserves return types): context for why TypeScript is the correct fixture language for transparency marker tests
- ADR-008 (default builds never link async/TLS/HTTP; `proxy` cargo feature is the opt-in): the cfg-pair registry gating and main.rs routing guard are the runtime enforcement
- PF-004 (two interception surfaces: rewrite engine vs PATH wrappers): `SKIM_REWRITTEN_FROM` exists only on the rewrite-engine surface; the wrapper surface is intentionally unmarked
- PF-015 (provenance/integrity mechanism fails in ways its own tests cannot see): `pin_is_current()` and `HookFacts.pin_is_current` follow the display-without-gate pattern documented here; the copy-binary E2E test in `cli_doctor.rs` closes the coverage gap PF-015 describes
- PF-016 (integrity check that returns empty finding set on failure fails open): the `ScriptIntegrity` four-state enum and `hook_status_line()` early-returns for `Tampered`/`Unreadable` are the fix documented here
- PF-017 (installer tests mutate the developer's real home): `skim_sandboxed_with_bin` sets `HOME` + all five agent config-dir overrides so no path escapes the TempDir
- Feature: `analytics` — session_id flows from hook JSON via sidecar, not via rewritten command flags
- Source: `crates/rskim/src/cmd/hook_log.rs` — `log_hook_warning()` (the only permitted diagnostic channel in hook mode)
- Tests: `crates/rskim/tests/cli_integrity.rs` — E2E tests for `classify_script_integrity` and `hook_status_line` behaviors
