# Consistency Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Inconsistent `report::to_json` signature vs existing pattern** - `crates/rskim-bench/src/cochange/report.rs:20`
**Confidence**: 82%
- Problem: The existing `report.rs` in `rskim-bench` defines `to_json(result: &BenchResult, tuning: Option<&TuningResult>) -> anyhow::Result<String>`, where additional context is passed as a second parameter. The new `cochange::report::to_json` takes only `&CochangeValidationResult`. While the cochange result has all data embedded (no separate tuning phase), the function signatures show a divergent pattern: the original accepts optional secondary context while the new one bakes everything into a single struct. This is acceptable design-wise but worth noting — the two `to_json` functions in the same crate have different ergonomic signatures. Not blocking since the cochange result is self-contained.
- Fix: No action required. The `CochangeValidationResult` already contains all relevant data (including `run_metadata`). If a tuning phase is added later, extend the struct or add an optional parameter at that point.

**`capture_head_sha` duplicates timeout pattern from `rskim_research::clone::git_run_with_timeout`** - `crates/rskim-bench/src/cochange/validate.rs:646-693`
**Confidence**: 85%
- Problem: The `capture_head_sha` function implements its own timeout-with-kill pattern (spawn a thread, `recv_timeout`, SIGKILL on timeout) that is structurally identical to `rskim_research::clone::git_run_with_timeout` (which was already made `pub` in this PR). Both use the same `libc::kill` + SIGKILL pattern with `#[cfg(unix)]`/`#[cfg(not(unix))]` branches. This violates DRY and the codebase pattern of extracting shared git subprocess infrastructure into `rskim_research::clone`.
- Fix: Refactor `capture_head_sha` to use `git_run_with_timeout` from `rskim_research::clone`, capturing stdout output. Alternatively, add a `git_output_with_timeout` helper to `rskim_research::clone` that returns `Output` instead of just success/failure:

```rust
// In rskim_research::clone — new helper alongside git_run_with_timeout
pub fn git_output_with_timeout(mut cmd: std::process::Command, label: &str) -> anyhow::Result<std::process::Output> {
    // ... same timeout pattern, but return Output
}

// In validate.rs
fn capture_head_sha(repo_path: &Path) -> anyhow::Result<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-c").arg("credential.helper=")
       .arg("-C").arg(repo_path)
       .args(["rev-parse", "HEAD"])
       .stdout(std::process::Stdio::piped())
       .stderr(std::process::Stdio::null());
    let output = rskim_research::clone::git_output_with_timeout(cmd, "git rev-parse HEAD")?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`chrono_now` hand-rolled calendar implementation** - `crates/rskim-bench/src/bin/cochange_validate.rs:223-249` (Confidence: 65%) — The binary already depends on crates that transitively pull in `chrono` or `time`. The hand-rolled Hinnant algorithm is impressive but adds maintenance surface for date logic that a single `chrono::Utc::now().to_rfc3339()` call would handle. The doc comment explains the rationale (avoiding a dependency), which is a valid choice but worth verifying whether chrono is already transitively available.

- **`test_utils` module always compiled (not cfg(test)-gated)** - `crates/rskim-bench/src/cochange/mod.rs:35` (Confidence: 72%) — The `test_utils` module is always compiled and documented as intentional (for integration tests). This matches the pattern documented in the module comment. However, an alternative pattern used elsewhere in Rust is a `testkit` feature flag that avoids shipping test helpers in the production binary. Since this is a bench crate (not a library consumed by end-users), the current approach is acceptable.

- **Inconsistent `GIT_SUBPROCESS_TIMEOUT_SECS` constant location** - `crates/rskim-bench/src/cochange/validate.rs:644` vs `crates/rskim-research/src/clone.rs` (Confidence: 68%) — Both files define a git subprocess timeout constant (30s in validate.rs vs whatever is in clone.rs). If the codebase wants a single source of truth for git subprocess timeouts, it should live in one place. But since the two timeouts serve different operations (rev-parse vs clone), having separate values is defensible.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | - | 0 | 0 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new `cochange` module is internally consistent: naming conventions (snake_case modules, PascalCase types), error handling style (anyhow with `.context()`/`.with_context()`), doc comment format, `#[must_use]` annotations on pure functions, test structure (`#[cfg(test)]` with `#[allow(clippy::unwrap_used)]`), and section separators (`// ====...`) all match established patterns in the existing `rskim-bench` crate.

The only notable consistency gap is the duplicated timeout-with-kill pattern between `validate.rs::capture_head_sha` and `rskim_research::clone::git_run_with_timeout`. This PR already made `git_run_with_timeout` public, suggesting the intent was to share it, but `capture_head_sha` still implements its own copy because it needs stdout capture (not just exit status). Extracting a shared `git_output_with_timeout` helper would close this gap. (applies ADR-001 — fix noticed issues immediately)
