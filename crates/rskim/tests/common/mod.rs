//! Shared test harness for rskim integration tests.
//!
//! ## Design
//!
//! All helpers in this module are `pub` so each integration test binary (each
//! `tests/*.rs` file is compiled as a separate crate) can use them after adding
//! `mod common;` at the top of the file.
//!
//! ## Analytics isolation
//!
//! The safe default is **analytics OFF**: `skim()` sets `SKIM_DISABLE_ANALYTICS=1`
//! so test invocations never write to the developer's real `~/.cache/skim/analytics.db`.
//!
//! Tests that assert on recorded analytics data must use `skim_with_analytics(db)`
//! instead — it points at an isolated temp DB and re-enables recording.
//!
//! ## Dead-code suppression
//!
//! Each test binary compiles `common` independently. A binary that does not call
//! every helper will trigger an "unused" warning without this attribute.
#![allow(dead_code)]

/// Shared git fixture helpers (hermetic `git_init`, `git_commit`, etc.).
pub mod git_fixture;

/// Build a `skim` command with analytics disabled — the safe default.
///
/// Sets:
/// - `SKIM_DISABLE_ANALYTICS=1` — no rows written to any analytics DB.
/// - `NO_COLOR=1` — deterministic, color-free output for assertions.
///
/// Callers may chain additional `.env(...)` / `.env_remove(...)` / `.args(...)`
/// calls. Per-test env overrides applied after this call take precedence.
pub fn skim() -> assert_cmd::Command {
    let mut c = assert_cmd::Command::cargo_bin("skim").unwrap();
    c.env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .env_remove("SKIM_REWRITTEN_FROM"); // prevent host env from leaking into tests
    c
}

/// Build a sandboxed command for the given skim binary path.
///
/// This is the **single authoritative source** for the sandbox env-var block
/// used by `skim init`, `skim init --uninstall`, and `skim doctor` tests.
/// Both `skim_sandboxed` and any test that must run a non-default binary
/// (e.g. a copied binary for pin-mismatch coverage) must route through here
/// rather than hand-rolling their own env block (PF-017).
///
/// Sets every agent config-dir override that the installer reads, so the
/// invocation cannot escape the TempDir sandbox even if the developer has
/// exported `CODEX_HOME` or `CRUSH_CONFIG_DIR` in their shell:
///
/// - `HOME` — redirects all `dirs::home_dir()` lookups in the child process.
/// - `CLAUDE_CONFIG_DIR` — Claude Code hook / settings / guidance.
/// - `SKIM_CACHE_DIR` — parser cache, analytics DB, and hook.log.
/// - `SKIM_WRAPPERS_DIR` — `~/.skim/bin/` wrapper symlink directory.
/// - `GEMINI_CONFIG_DIR` — Gemini CLI hook / settings / guidance.
/// - `COPILOT_CONFIG_DIR` — Copilot CLI hook / settings / guidance.
/// - `CODEX_HOME` — Codex CLI config directory.
/// - `CRUSH_CONFIG_DIR` — Crush CLI config directory.
/// - `SKIM_DISABLE_ANALYTICS=1` — no rows written to any analytics DB.
/// - `NO_COLOR=1` — deterministic, color-free output for assertions.
///
/// Also removes env vars that carry real-session state (`SKIM_REWRITTEN_FROM`,
/// `SKIM_PASSTHROUGH`, `SKIM_HOOK_VERSION`, `SKIM_HOOK_BINARY`).
///
/// Tests may chain additional `.env(...)` calls to add or override vars.
pub fn skim_sandboxed_with_bin(
    home: &std::path::Path,
    bin: &std::path::Path,
) -> assert_cmd::Command {
    let mut c = assert_cmd::Command::new(bin);
    c.env("HOME", home)
        .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
        .env("SKIM_CACHE_DIR", home.join(".cache/skim"))
        .env("SKIM_WRAPPERS_DIR", home.join(".skim").join("bin"))
        .env("GEMINI_CONFIG_DIR", home.join(".gemini"))
        .env("COPILOT_CONFIG_DIR", home.join(".copilot"))
        .env("CODEX_HOME", home.join(".codex"))
        .env("CRUSH_CONFIG_DIR", home.join(".crush"))
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .env_remove("SKIM_REWRITTEN_FROM")
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_HOOK_VERSION")
        .env_remove("SKIM_HOOK_BINARY");
    c
}

/// Build a `skim` command sandboxed against a temporary home directory.
///
/// Thin delegation to `skim_sandboxed_with_bin` using the default cargo-built
/// binary. All sandbox env-var documentation lives on that function.
///
/// Use this for any `skim init`, `skim init --uninstall`, or `skim doctor`
/// invocation.  Tests may chain additional `.env(...)` calls to add or
/// override specific vars after calling this helper.
pub fn skim_sandboxed(home: &std::path::Path) -> assert_cmd::Command {
    skim_sandboxed_with_bin(home, &skim_bin())
}

/// Return the path to the skim binary built by cargo.
///
/// Use this when you need a `std::process::Command` rather than an
/// `assert_cmd::Command` (e.g. for `argv[0]` override via `CommandExt::arg0`).
pub fn skim_bin() -> std::path::PathBuf {
    assert_cmd::cargo::cargo_bin("skim")
}

/// Build a `skim` command pointed at an isolated analytics DB, with recording
/// **enabled**.
///
/// Use this (and only this) for tests that assert on rows written to the
/// analytics database. Pass a `TempDir`-backed path so the DB is cleaned up
/// after the test.
///
/// Sets:
/// - `SKIM_ANALYTICS_DB=<db>` — all writes go to the isolated file.
/// - `NO_COLOR=1` — deterministic output.
///
/// `SKIM_DISABLE_ANALYTICS` is explicitly removed so recording is active.
pub fn skim_with_analytics(db: &std::path::Path) -> assert_cmd::Command {
    let mut c = assert_cmd::Command::cargo_bin("skim").unwrap();
    c.env("SKIM_ANALYTICS_DB", db)
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1");
    c
}

// ============================================================================
// Stub tools on a prepended PATH
// ============================================================================

/// Write `script` to `dir/name` and make it executable.
///
/// The caller owns the whole script body, which is what tests that need timing
/// control (`sleep`) or loops require.  Prefer [`make_stub`] /
/// [`make_stub_bytes`] for the common fixed-output case.
///
/// Unix-only: the executable bit requires `std::os::unix::fs::PermissionsExt`.
#[cfg(unix)]
pub fn write_stub_script(dir: &std::path::Path, name: &str, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    let script_path = dir.join(name);
    std::fs::write(&script_path, script).unwrap();
    std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Create a stub tool script that prints fixed stdout/stderr and exits `code`.
///
/// The payloads are written to sidecar files and `cat`-ed by the script, so no
/// shell escaping of the content is needed — and, because `cat` is an external
/// process, the bytes reach the pipe without depending on the shell's own
/// stdout buffering.
///
/// Unix-only: the script uses `#!/bin/sh`.
#[cfg(unix)]
pub fn make_stub(dir: &std::path::Path, name: &str, stdout: &str, stderr: &str, code: i32) {
    make_stub_bytes(dir, name, stdout.as_bytes(), stderr.as_bytes(), code);
}

/// Byte-payload variant of [`make_stub`].
///
/// Required for fidelity tests whose payload is deliberately not valid UTF-8 —
/// a `&str` parameter cannot express those bytes at all.
#[cfg(unix)]
pub fn make_stub_bytes(dir: &std::path::Path, name: &str, stdout: &[u8], stderr: &[u8], code: i32) {
    let out_path = dir.join(format!("{name}.out"));
    let err_path = dir.join(format!("{name}.err"));
    std::fs::write(&out_path, stdout).unwrap();
    std::fs::write(&err_path, stderr).unwrap();
    let script = format!(
        "#!/bin/sh\ncat '{}'\ncat '{}' >&2\nexit {code}\n",
        out_path.display(),
        err_path.display()
    );
    write_stub_script(dir, name, &script);
}

/// PATH with `dir` prepended so skim's spawned child resolves to the stub.
///
/// Unix-only: uses `:` as the PATH separator.
#[cfg(unix)]
pub fn stub_path(dir: &std::path::Path) -> String {
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

// ============================================================================
// Trivial Cargo project fixture
// ============================================================================

/// Creates a `TempDir` containing a zero-dependency Cargo project used to
/// exercise the cargo, clippy, and test wrappers without triggering a cold
/// compile of the entire skim workspace.
///
/// The crate compiles in ~1–2 s even on a cold runner, keeping the 120 s
/// timeout a comfortable bound rather than a latency time-bomb.
///
/// Workspace isolation: `TempDir::new()` places the directory under the system
/// temp root (`/tmp` on Linux, `/var/folders/…` on macOS), which is already
/// outside the skim workspace tree. The explicit `[workspace]` table in
/// `Cargo.toml` additionally guards against any future cargo heuristic that
/// might walk upward from an in-tree location.
///
/// `main.rs` includes one `#[test]` function (`probe`) so that both
/// `cargo build` / `cargo clippy` exercises (which ignore tests) and
/// `cargo test` exercises (which need at least one test) work with the same
/// fixture. `cargo build` and `cargo clippy` are unaffected by the `#[cfg(test)]`
/// block — it is compiled only when running `cargo test`.
///
/// Mirrors the `test_build_make_real_execution_success` `TempDir` pattern in
/// `cli_e2e_build_parsers.rs`. Extracted from that file (issue #447) so every
/// test binary that needs a real-cargo fixture can share it.
pub fn trivial_cargo_project() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("failed to create temp dir");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        concat!(
            "[package]\n",
            "name = \"skim_e2e_probe\"\n",
            "version = \"0.0.0\"\n",
            "edition = \"2021\"\n",
            "\n",
            "[[bin]]\n",
            "name = \"skim_e2e_probe\"\n",
            "path = \"main.rs\"\n",
            "\n",
            "[workspace]\n",
        ),
    )
    .expect("failed to write Cargo.toml");
    std::fs::write(
        dir.path().join("main.rs"),
        concat!(
            "fn main() {}\n",
            "\n",
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    #[test]\n",
            "    fn probe() {}\n",
            "}\n",
        ),
    )
    .expect("failed to write main.rs");
    dir
}
