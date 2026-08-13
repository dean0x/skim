---
feature: hook-binary-pinning
name: Agent Hook Install, Binary Pinning & Handshake (+ Permissions Seeding)
description: "Use when modifying hook script generation, adding new agents, changing the hook script format, debugging version-skew or wrong-clone warnings, working on install/reinstall logic, touching wrapper symlink management, editing guidance_content() / the Command wrapping section, or working on skim init --permissions / the PermissionsProtocol subsystem, or modifying the proxy feature gate / cfg-gated registry entries, or touching the rewrite transparency marker (SKIM_REWRITTEN_FROM / rewrite_origin / rewrite_transparency_marker / view_differs), or working on doctor integrity checks / ScriptIntegrity / hook_status_line, or adding test harness sandboxing (skim_sandboxed / hermetic_path). Keywords: hook install, binary pinning, SKIM_HOOK_BINARY, SKIM_HOOK_COMMIT, generate_hook_script, is_hook_script_current, uses_pinned_binary, script_has_pinned_marker, AwarenessOnly, codex, wrapper symlinks, guidance_content, guidance_content_mdc, Command wrapping, PermissionsProtocol, confirm_grant, READ_ONLY_SUBCOMMANDS, hook_config_dir, modifiedArgs, seed tier, sidecar manifest, proxy feature gate, cfg-gated registry, routing guard, cli_proxy_gating, KNOWN_SUBCOMMANDS, META_SUBCOMMANDS, wrapper_targets, uses_dedicated_hook_file, hook_event_key, print_install_summary, print_dry_run_actions, print_dry_run_permissions, SKIM_JSON_NAME, SKIM_REWRITTEN_FROM, rewrite_origin, rewrite_transparency_marker, view_differs, transparency marker, origin tag, ScriptIntegrity, classify_script_integrity, verify_script_integrity, hook_status_line, HookFacts, NoManifest, Tampered, Verified, Unreadable, skim_sandboxed, hermetic_path, ADR-004, ADR-005, ADR-006, ADR-008."
category: domain-knowledge
directories: [crates/rskim/src/cmd/hooks, crates/rskim/src/cmd/init, crates/rskim/src/cmd/rewrite, crates/rskim/src/cmd/permissions]
created: 2026-07-04
updated: 2026-08-12
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

### The Two Currency Predicates — One Shared Pinned-Marker Helper

Two separate predicates determine whether an existing hook script is stale, sharing a single source of truth: `script_has_pinned_marker` in `init/mod.rs`.

**`uses_pinned_binary` (`init/state.rs`)** delegates to the shared helper for state detection. **`is_hook_script_current` (`init/install.rs`)** ANDs its own version-line check with the shared marker scan. `DetectedState::hook_is_current()` combines both: `version matches && hook_uses_pinned_binary`.

The shared helper **structurally prevents** drift: changing the `SKIM_HOOK_BINARY` marker updates both predicates at once. A new **required line** added to the script format and gated in only one predicate re-introduces the lockstep hazard — fold any new required-line check into the shared helper.

### ScriptIntegrity: Doctor Derives Verdict from Manifest, Not Hook Text

`cmd/integrity.rs` provides a four-state enum and the classifier that `skim doctor` uses to determine tamper status:

```rust
pub(crate) enum ScriptIntegrity {
    Verified,    // hash matches stored manifest — script is unmodified
    NoManifest,  // no .sha256 manifest present — pre-manifest install (backward compat)
    Tampered,    // script contents differ from stored hash
    Unreadable,  // script file cannot be read (missing, permission denied)
}

pub(crate) fn classify_script_integrity(
    config_dir: &Path,
    agent_cli_name: &str,
    script_path: &Path,
) -> ScriptIntegrity { ... }
```

`verify_script_integrity` is a **thin bool wrapper** over `classify_script_integrity` that maps `Verified|NoManifest → Ok(true)`, `Tampered → Ok(false)`, `Unreadable → Err(...)`. It preserves the existing call contract for the two legacy callers (`cmd/rewrite/hook.rs` and `cmd/init/uninstall.rs`) that predate the full enum.

**Why the manifest, not the script text:** the hook script bytes are exactly what a tamper modifies. A doctor that reads the `SKIM_HOOK_VERSION` comment out of the script and trusts it would be fooled by any modification that preserves the comment. The SHA-256 manifest is an independent artefact written at install time and stored alongside the script — a tamper that also updates the manifest would need to know the expected hash.

**`HookFacts.script_integrity`:** `hook_facts()` in `init/mod.rs` calls `classify_script_integrity` for every agent (not just ClaudeCode) and stores the result in `HookFacts`. Doctor reads `HookFacts.script_integrity` and passes it to `hook_status_line()`.

**`hook_status_line()` in `doctor/mod.rs`:** a pure, testable function that produces `(bool, String)` — drift verdict and the display line. It checks `ScriptIntegrity` **before** pin/currency checks so the verdict always comes from the manifest. Behavior by state:
- `Tampered` → drift (`✗`), names the suppression coupling (#479)
- `Unreadable` → drift (`✗`), names the suppression coupling (#479)
- `NoManifest` → advisory only (`⚠`), **not** drift — falls through to pin/currency checks (Group 3 fix: no early return that suppresses drift detection for the pin state)
- `Verified` → falls through to pin/currency checks

### Two Fail-Opens Fixed (issue #471)

**Fail-open 1 — `NoManifest` early return suppressed all drift detection:**
Previously `NoManifest` returned early from the hook-status logic, silently bypassing pin/currency drift checks for pre-manifest installs. Now `NoManifest` falls through with an advisory note (`⚠ no integrity manifest...`) appended to whatever the pin/currency check produces. Pre-manifest hooks can still be reported as unpinned or stale.

**Fail-open 2 — `skim init` never regenerated a missing manifest:**
The idempotent early-return path (when `is_hook_script_current()` returns true) previously exited immediately without writing the manifest. Doctor's advice — "run `skim init --agent X`" — was therefore a dead end: running init again would hit the same early return and skip the manifest write again. Now the early-return path explicitly computes and writes/rewrites the manifest before printing "Skipped":

```rust
// install.rs — self-heal: always write manifest on the idempotent path
if script_path.exists() && is_hook_script_current(&script_path, &state.skim_version) {
    let hash = crate::cmd::integrity::compute_file_hash(&script_path)?;  // ? propagates errors
    crate::cmd::integrity::write_hash_manifest(...)?;                     // ? propagates errors
    println!("  {} Skipped: ...", ...);
    return Ok(());
}
```

Write errors are propagated with `?` — the previous code used `let _ = write_hash_manifest(...)` which silently swallowed failures.

### `check_hook_binary_mismatch` — Same-Version Divergent Build Detection

This function in `rewrite/hook.rs` fires only when versions match. It compares (1) `SKIM_HOOK_BINARY` vs `canonicalize(current_exe())` and (2) `SKIM_HOOK_COMMIT` vs `option_env!("SKIM_GIT_COMMIT")`. Both comparisons skip if the env var is unset (backward compat) and skip the commit comparison if either side is `"unknown"`. Warnings go to `hook.log` via `warn_once_daily()` — never stderr.

**Still-open decision (#479):** At hook-exec time (`rewrite/hook.rs:359`), drift detection is suppressed when integrity fails — an integrity failure subsumes drift detection deliberately. This is the correct behavior but is tracked as an issue to reconsider after the self-heal ships in a release (a user who installs with the self-heal can no longer reach the `NoManifest` state, at which point promoting it to drift would be safe).

**`parse_version_from_script` reads the comment first:** `parse_version_from_script` in `state.rs` scans lines and checks `# skim-hook v{version}` (the legacy comment line) **before** `export SKIM_HOOK_VERSION="..."`. Since the comment appears first in the generated script, it wins. This means an attacker who edits the script can change the comment and change what version `detect_state` sees. This function must **never** be used as a security input — it is a convenience parser for display and reinstall-skip decisions only, not for integrity classification (which uses the manifest).

### Codex Is HookSupport::AwarenessOnly

`CodexCliHook` returns `HookSupport::AwarenessOnly`: `generate_script()` returns `""`, `parse_input()` returns `None`, `format_response()` returns `Null`. In `run_hook_mode`, the awareness check fires before any stdin read. Tests for Codex assert NO script and NO handshake.

### Doctor-vs-Hook Asymmetry

**Hook-time integrity checking is claude-code-only.** In `run_hook_mode` (`rewrite/hook.rs`), `check_hook_integrity` is called only inside the `if agent_kind == AgentKind::ClaudeCode` branch (line ~353). Other agents do not get hook-time integrity checking.

**Doctor checks all agents.** `print_hook_section` in `doctor/mod.rs` iterates `AgentKind::all_supported()` and calls `hook_status_line(&facts, ...)` for every agent. `hook_facts(agent)` calls `classify_script_integrity` for any agent with a hook script path. This means hand-edited gemini/codex/cursor/copilot scripts now report `✗` in `skim doctor` even though no integrity check fires at hook-exec time for those agents.

### Proxy Feature Gate (#352)

The `proxy` subcommand is compiled out of default builds (ADR-008). Enforced in three coordinated places: the registry pair-gate (`"proxy"` appears in both `KNOWN_SUBCOMMANDS` and `META_SUBCOMMANDS` under the same `#[cfg(feature = "proxy")]`), the routing guard in `main.rs` (`#[cfg(not(feature = "proxy"))]` fires before file-op fallthrough), and optional Cargo deps (`proxy = ["dep:rskim-proxy", "dep:rskim-contract"]`). The invariant test `test_proxy_registry_entries_gated_as_a_pair` asserts the cfg-pair in both feature configs.

### Transparency Marker for Hook-Rewritten File Reads (SKIM_REWRITTEN_FROM)

When the rewrite engine rewrites a `cat`/`head`/`tail` command into a skim file read, it injects `SKIM_REWRITTEN_FROM=<cat|head|tail>` at the front of the rewritten token list. `rewrite_origin()` reads this from the environment with a closed vocabulary (any other value returns `None`). `rewrite_transparency_marker()` builds the marker string; returns `None` when `differing == 0` (byte-identical output). This tag is rewrite-engine-only — PATH-wrapper-mediated reads are intentionally unmarked (PF-004).

### Permissions Seeding Subsystem (`cmd/permissions/`)

`PermissionsProtocol` is a format-agnostic trait for seeding agent-native allowlist entries. The seeded tool list is always `READ_ONLY_SUBCOMMANDS ∩ wrapper_targets()` — exactly 8 tools. `confirm_grant()` is the primary TTY-gated consent gate; non-TTY stdin → immediate `false`. The `--yes` flag is for hook uninstall confirmation only; it does NOT bypass `confirm_grant`. Three tiers: `Seed`, `Mirror`, `Blanket` — set via `--permissions-tier`.

## State Transitions

The `detect_state` → `run_install_single` flow:

```
detect_state()
  └─ reads hook script once → uses_pinned_binary + parse_version_from_script
  └─ DetectedState::hook_is_current() = version matches && pinned format
        │
        ├─ true → compute hash → write_hash_manifest (self-heal, ? propagated)
        │          → print "Already up to date", return
        └─ false → create_hook_script()
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

### Test Hermeticity — `skim_sandboxed` and `hermetic_path`

Before this branch, `cli_init*` and `cli_doctor` tests ran global `skim init --uninstall` against the developer's real `$HOME`, deleting `~/.skim/bin` wrappers and `~/.gemini/GEMINI.md`.

`tests/common/mod.rs` now provides `skim_sandboxed(home)` which redirects:
- `HOME` — redirects all `dirs::home_dir()` lookups
- `CLAUDE_CONFIG_DIR` → `home/.claude`
- `SKIM_CACHE_DIR` → `home/.cache/skim`
- `SKIM_WRAPPERS_DIR` → `home/.skim/bin`
- `GEMINI_CONFIG_DIR` → `home/.gemini`
- `COPILOT_CONFIG_DIR` → `home/.copilot`

Also removes `SKIM_PASSTHROUGH`, `SKIM_HOOK_VERSION`, `SKIM_HOOK_BINARY` to start clean.

**Two traps worth recording:**

1. **`detect_installed_agents()` in override-mode requires the config dir to already exist.** Tests must call `std::fs::create_dir_all(home.join(".claude"))` before calling `skim_sandboxed(home)` for init tests — otherwise `detect_installed_agents` returns empty (agent appears not installed).

2. **`skim doctor` scans `$PATH`, so a sibling clone's binary can win and produce spurious drift.** `cli_doctor.rs` has a `hermetic_path()` helper that prepends the test binary's directory to `$PATH`, ensuring the built binary under test is the one that wins. Tests that assert on drift must use `hermetic_path()` or they may see false drift from an unrelated release build on the developer's PATH.

## Anti-Patterns

**Adding a new required script line but wiring it into only one currency predicate**: the shared `script_has_pinned_marker` updates both predicates. A new required line gated in only one predicate silently desync state detection from reinstall.

**Emitting anything to stderr in hook mode**: all hook-mode diagnostics go to `hook.log` via `log_hook_warning` only. Zero-stderr invariant (GRANITE #361 Bug 3).

**Deriving doctor's verdict from the hook script text**: the hook bytes are exactly what a tamper modifies. Doctor must use `classify_script_integrity` (which reads the SHA-256 manifest) and the `HookFacts.script_integrity` field — not `parse_version_from_script` or any other scan of the script body.

**Treating both dispatch surfaces as equivalent for rewrite tests**: the rewrite engine (stdin JSON → `try_rewrite()` → JSON response) and the wrapper surface (argv0 dispatch) share per-tool handlers but have completely different front-ends. `SKIM_REWRITTEN_FROM` is rewrite-engine-only.

**Adding `--yes` bypass to `confirm_grant`**: the `--yes` flag is uninstall-only. `confirm_grant` must be called unconditionally whenever permissions are requested.

**Adding `find`, `env`, `ps`, `dig`, or `nslookup` to `READ_ONLY_SUBCOMMANDS`**: these tools are excluded for cause. `Bash(skim <tool>:*)` entries do NOT bound tool arguments.

**Adding `SKIM_PASSTHROUGH` to the guidance template**: enforced by `!content.contains("SKIM_PASSTHROUGH")` negative test assert.

**Mis-gating the proxy registry entries**: `"proxy"` must appear in `KNOWN_SUBCOMMANDS` and `META_SUBCOMMANDS` under the **same** `#[cfg(feature = "proxy")]` attribute.

**Using `let _ = write_hash_manifest(...)` to swallow write errors**: the manifest write must use `?` — silently installing without tamper detection is worse than a hard error. The self-heal path in `create_hook_script` demonstrates the correct pattern.

**Running `skim init` or `skim doctor` tests without `skim_sandboxed`**: tests that run init, uninstall, or doctor without sandboxing will mutate the developer's real `~/.claude/`, `~/.gemini/`, etc., and `skim init --uninstall` (which defaults to all agents) will delete real wrappers and guidance files.

## Gotchas

**`check_hook_binary_mismatch` fires only on same-version**: when versions differ, `check_hook_version_mismatch` logs the version mismatch and returns without calling the binary/commit check.

**`NoManifest` is not drift — but it no longer suppresses drift detection**: pre-manifest installs are advisory only (`⚠`), not drift. However, they now fall through to pin/currency checks so an unpinned pre-manifest hook is still reported as drift. The still-open question is whether `NoManifest` should become drift after the self-heal ships in a release (#471 follow-on, tracked).

**`rewrite/hook.rs:359` suppresses drift on integrity failure deliberately — tracked as #479**: when `check_hook_integrity` returns `true` (integrity failed), `detect_drift` is not called. This is intentional — an integrity failure subsumes drift — but it means drift is invisible on a tampered script at hook-exec time.

**`parse_version_from_script` reads the `# skim-hook v{version}` comment first**: because the comment appears before `export SKIM_HOOK_VERSION=` in the script, the scanner finds the comment first. This makes the version marker attacker-editable: anyone who modifies the script can change what `detect_state` reads as the installed version. Never use this function for security decisions.

**Doctor-vs-hook asymmetry for non-ClaudeCode agents**: `skim doctor` now reports `✗` for tampered codex/gemini/cursor/copilot scripts (via `hook_facts` + `classify_script_integrity`), but `skim rewrite --hook` only runs `check_hook_integrity` for ClaudeCode. So a tampered Gemini script shows up in doctor but produces no warning at hook-exec time.

**`SKIM_HOOK_COMMIT` has no quotes**: `export SKIM_HOOK_COMMIT={git_commit}` — safe because hex SHAs and `"unknown"` contain no shell-special chars. `generate_hook_script` enforces ascii-alphanumeric only via `assert!`.

**`detect_installed_agents()` in override-mode requires config dir to exist**: when `CLAUDE_CONFIG_DIR` (or any agent's config dir env var) is set, `detect_installed_agents` checks whether that directory exists. Tests using `skim_sandboxed` must `create_dir_all(home.join(".claude"))` before calling init or the agent appears uninstalled.

**`skim doctor` scans `$PATH` — use `hermetic_path()` in tests**: the test binary and the release build may both appear on `$PATH`. Without restricting PATH to the test binary's directory, `skim doctor` may detect PATH drift from the unrelated release build and exit 1, making the test assertion wrong.

**`nextest --all-targets` does not build `target/debug/skim`**: the E2E test harness invokes the `skim` binary from `target/debug/skim`. Always run `cargo build -p rskim` before running `--all-targets` integration tests.

**`print_detected_state` still shows "Config: …/settings.json (will be created)" for Copilot**: known residual cosmetic gap. `print_detected_state` uses `state.settings_path` directly without branching on `uses_dedicated_hook_file()`. The actual install path is correct; only the detection-phase printout is cosmetically wrong.

## Key Files

- `crates/rskim/src/cmd/integrity.rs` — `ScriptIntegrity` enum; `classify_script_integrity()`; `verify_script_integrity` (thin bool wrapper); `compute_file_hash()`; `write_hash_manifest()`; `read_hash_manifest()`
- `crates/rskim/src/cmd/init/mod.rs` — `run()` init dispatch; `script_has_pinned_marker()` (single source of truth for the `SKIM_HOOK_BINARY` marker scan); `HookFacts` DTO; `hook_facts()` (calls `classify_script_integrity` for every agent)
- `crates/rskim/src/cmd/doctor/mod.rs` — `hook_status_line()` (pure, testable; checks `ScriptIntegrity` before pin/currency; verdict from manifest not script text); `print_hook_section()` (iterates `AgentKind::all_supported()`)
- `crates/rskim/src/cmd/init/helpers.rs` — `guidance_content()`, `guidance_content_mdc()`, `atomic_write_settings()`, `load_or_create_settings()`, `confirm_proceed()`, `confirm_grant()` (TTY-gated consent gate)
- `crates/rskim/src/cmd/init/flags.rs` — `PermissionsTier`, `InitFlags`, `DetectionEnv`, `detect_installed_agents()`, `resolve_agent()`
- `crates/rskim/src/cmd/init/install.rs` — `create_hook_script()` (self-heals manifest on idempotent path); `is_hook_script_current()`; `atomic_write_executable()`; `resolve_permissions_consent()`
- `crates/rskim/src/cmd/init/state.rs` — `detect_state()`, `uses_pinned_binary()`, `DetectedState`, `hook_is_current()`; `parse_version_from_script` (reads comment-first; attacker-editable, advisory only)
- `crates/rskim/src/cmd/hooks/mod.rs` — `generate_hook_script()`, `shell_single_quote()`, `HookProtocol` trait, `HookSupport` enum
- `crates/rskim/src/cmd/hooks/copilot.rs` — `CopilotCliHook`: `hook_config_dir` redirect, `uses_dedicated_hook_file`, `write_copilot_skim_json` (atomic); `SKIM_JSON_NAME` constant
- `crates/rskim/src/cmd/rewrite/hook.rs` — `run_hook_mode()`; `check_hook_binary_mismatch()`; `check_hook_integrity()` (ClaudeCode-only at hook time); `detect_drift()`; line ~359: drift suppressed when integrity fails (#479)
- `crates/rskim/src/cmd/rewrite/engine.rs` — `try_rewrite()`, `try_custom_handlers()` (injects `SKIM_REWRITTEN_FROM=<tool>` origin tag)
- `crates/rskim/src/output/mod.rs` — `REWRITE_ORIGIN_ENV`, `rewrite_origin()` (closed-vocabulary reader), `rewrite_transparency_marker()`
- `crates/rskim/src/cmd/registry.rs` — `READ_ONLY_SUBCOMMANDS`, `KNOWN_SUBCOMMANDS`, `META_SUBCOMMANDS`, `wrapper_targets()`; `"proxy"` cfg-gated as a pair
- `crates/rskim/src/cmd/permissions/mod.rs` — `PermissionsProtocol` trait, `permissions_protocol_for_agent` factory, `hash_if_bounded`, `seeded_entries`
- `crates/rskim/build.rs` — `SKIM_GIT_COMMIT` build-time injection
- `crates/rskim/tests/common/mod.rs` — `skim_sandboxed(home)` (6-env-var sandbox helper); `skim()` (`SKIM_REWRITTEN_FROM` removed)
- `crates/rskim/tests/cli_doctor.rs` — `hermetic_path()` helper; `do_sandboxed_init()`; E2E tests for tamper detection via manifest

## Related

- ADR-004 (hook install pins absolute binary path + handshake): mandated the pinned-binary format, daily-rate-limited warn-only signaling; also provides ordering precedent for `SKIM_REWRITTEN_FROM`
- ADR-005 (guidance framed as calibrated trust): prohibits `SKIM_PASSTHROUGH` in guidance template; "flag it to the user" sentence byte-identical since v2.11.0
- ADR-006 (hook responses never self-approve — permissions seeding is consent-gated): per-host response matrix; `confirm_grant` enforces pre-checks
- ADR-007 (pseudo preserves return types): context for why TypeScript is the correct fixture language for transparency marker tests
- ADR-008 (default builds never link async/TLS/HTTP; `proxy` cargo feature is the opt-in): the cfg-pair registry gating and main.rs routing guard are the runtime enforcement
- PF-004 (two interception surfaces: rewrite engine vs PATH wrappers): `SKIM_REWRITTEN_FROM` exists only on the rewrite-engine surface; the wrapper surface is intentionally unmarked
- PF-006 (strip_ansi destroys tabs — gh/diff skip_ansi_strip fix): covers wrapper configs that live outside this KB's directories
- Feature: `analytics` — session_id flows from hook JSON via sidecar, not via rewritten command flags
- Source: `crates/rskim/src/cmd/hook_log.rs` — `log_hook_warning()` (the only permitted diagnostic channel in hook mode)
- Tests: `crates/rskim/tests/cli_integrity.rs` — E2E tests for `classify_script_integrity` and `hook_status_line` behaviors
- Tests: `crates/rskim/tests/cli_doctor.rs` — E2E tests for tamper detection, healthy exit-0, hermetic PATH
- Tests: `crates/rskim/tests/cli_init.rs` — init isolation using `skim_sandboxed`; `create_dir_all` traps
