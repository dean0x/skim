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

/// Spawn a sandboxed skim binary with bounded retry on ETXTBSY (os error 26).
///
/// # Why this exists
///
/// On Linux, `execve(2)` fails with ETXTBSY ("Text file busy") when a
/// concurrently-forked child process holds an open writable file descriptor to
/// the binary being exec'd.  This race surfaces when parallel integration tests
/// do `std::fs::copy(src, dest)` and immediately exec `dest`: another test's
/// `fork()` can inherit the writable fd from `std::fs::copy` before the parent
/// closes it (the fd is O_CLOEXEC-marked, so it is released when *that child*
/// execs, but not before).  The kernel keeps the inode's write count nonzero
/// for the duration of that window — typically a few milliseconds under normal
/// load.
///
/// # Protocol
///
/// `configure` is called once per attempt so the command can be freshly
/// rebuilt without consuming a shared mutable builder.  Returns an [`Assert`]
/// ready for chaining `.success()`, `.failure()`, etc.
///
/// # Bound
///
/// Exactly `ETXTBSY_MAX_ATTEMPTS` (5) total tries.  Back-off is
/// `25 ms × 2^attempt`, giving cumulative sleep of at most
/// 25 + 50 + 100 + 200 = 375 ms across the four retries before giving up.
///
/// [`Assert`]: assert_cmd::assert::Assert
pub fn skim_sandboxed_with_bin_retried<F>(
    home: &std::path::Path,
    bin: &std::path::Path,
    configure: F,
) -> assert_cmd::assert::Assert
where
    F: Fn(&mut assert_cmd::Command),
{
    /// POSIX ETXTBSY — "Text file busy".  Value 26 is correct on Linux and macOS.
    const ETXTBSY: i32 = 26;
    /// Maximum spawn attempts before giving up.  Five covers the race window
    /// observed in CI (typically resolved in 1–2 retries) with headroom to spare.
    const ETXTBSY_MAX_ATTEMPTS: u32 = 5;

    for attempt in 0..ETXTBSY_MAX_ATTEMPTS {
        let mut cmd = skim_sandboxed_with_bin(home, bin);
        configure(&mut cmd);
        match cmd.output() {
            Ok(output) => return assert_cmd::assert::Assert::new(output),
            Err(e) if e.raw_os_error() == Some(ETXTBSY) => {
                // Another process holds a writable fd to the binary.  Sleep
                // briefly and let it exec (which closes the O_CLOEXEC fd).
                if attempt + 1 < ETXTBSY_MAX_ATTEMPTS {
                    std::thread::sleep(std::time::Duration::from_millis(25u64 << attempt));
                } else {
                    panic!(
                        "spawn still ETXTBSY after {} attempts: {}",
                        ETXTBSY_MAX_ATTEMPTS, e
                    );
                }
            }
            Err(e) => panic!("spawn failed (attempt {}): {}", attempt + 1, e),
        }
    }
    unreachable!("loop exits only via return or panic")
}

// ============================================================================
// Sandbox env-var plan (PF-017)
// ============================================================================
//
// These four tables are the *whole* contract for what a sandboxed invocation
// may see. Every environment variable the production crate reads must appear
// in exactly one of them, and `cli_init.rs` carries a test that scans
// `crates/rskim/src` and fails when a newly-added read is in none of them —
// PF-017's lesson was that a hand-maintained enumeration is always incomplete,
// so this one is checked against the source rather than trusted.

/// Env vars redirected into the sandbox home, as `(var, path relative to home)`.
///
/// An empty relative path means the home directory itself. Each entry closes a
/// path by which a child process could otherwise reach real user state — note
/// that these are the *agents' own* variable names, not `SKIM_`-prefixed ones.
pub const SANDBOX_REDIRECTED_VARS: &[(&str, &str)] = &[
    ("HOME", ""), // every `dirs::home_dir()` lookup, incl. wrapper + cache defaults
    ("CLAUDE_CONFIG_DIR", ".claude"), // Claude Code hook / settings / guidance
    ("CURSOR_CONFIG_DIR", ".cursor"), // Cursor config dir — read by `DetectionEnv`
    ("GEMINI_CONFIG_DIR", ".gemini"), // Gemini CLI hook / settings / guidance
    ("COPILOT_CONFIG_DIR", ".copilot"), // Copilot CLI hook / settings / guidance
    ("CODEX_CONFIG_DIR", ".codex"), // Codex hook/settings path — `DetectionEnv`
    ("CODEX_HOME", ".codex"), // Codex guidance path — `InstructionEnv`, a DIFFERENT var
    ("CRUSH_CONFIG_DIR", ".crush"), // Crush CLI hook / settings / guidance
    ("SKIM_WRAPPERS_DIR", ".skim/bin"), // `~/.skim/bin` wrapper symlinks
    ("SKIM_CACHE_DIR", ".cache/skim"), // parser cache, hook.log, force-raw sidecars
    ("SKIM_ANALYTICS_DB", ".cache/skim/analytics.db"), // outranks SKIM_CACHE_DIR
];

/// Env vars pinned to a fixed value so assertions are deterministic.
pub const SANDBOX_PINNED_VARS: &[(&str, &str)] = &[
    ("SKIM_DISABLE_ANALYTICS", "1"), // no rows written to any analytics DB
    ("NO_COLOR", "1"),               // deterministic, color-free output
];

/// Env vars stripped from the child so host session state cannot leak in.
///
/// Each of these has a safe default that resolves *inside* the sandbox once
/// `HOME` is redirected, so removal is strictly safer than passing a host value
/// through. Removal also means the variable cannot silently satisfy an
/// assertion: a host `SKIM_PASSTHROUGH=1`, for instance, makes hook mode
/// return empty stdout, which several hook tests assert as their success case.
pub const SANDBOX_REMOVED_VARS: &[&str] = &[
    "SKIM_REWRITTEN_FROM",      // rewrite-origin tag; drives the lossy-view marker
    "SKIM_PASSTHROUGH",         // would bypass compression and short-circuit hook mode
    "SKIM_HOOK_VERSION",        // hook handshake value — a host value fakes version drift
    "SKIM_HOOK_BINARY",         // hook binary pin — a host value fakes path drift
    "SKIM_HOOK_COMMIT",         // hook commit pin — a host value fakes commit drift
    "SKIM_HOOK_AUDIT",          // would append JSON lines to a hook-audit log
    "SKIM_SESSION_ID",          // analytics session attribution + sidecar keying
    "SKIM_DEBUG",               // adds raw-fallback banners that break stderr assertions
    "SKIM_INPUT_COST_PER_MTOK", // cost estimates must use the documented default
    "SKIM_PROJECTS_DIR",        // session-provider transcript dir (Claude)
    "SKIM_CODEX_SESSIONS_DIR",  // session-provider transcript dir (Codex)
    "SKIM_COPILOT_DIR",         // session-provider transcript dir (Copilot)
    "SKIM_CURSOR_DB_PATH",      // session-provider transcript DB (Cursor)
    "SKIM_GEMINI_DIR",          // session-provider transcript dir (Gemini)
    "SKIM_CRUSH_DIR",           // session-provider transcript dir (Crush)
];

/// Env vars deliberately inherited from the host.
///
/// `PATH` cannot be sandboxed: the child must still resolve `sh`, `git` and the
/// other real tools it shells out to. [`hermetic_path`] is the opt-in control
/// for the one behaviour that depends on PATH *content* — `skim doctor`'s scan
/// for which `skim` wins — and tests that assert on it pass it explicitly.
pub const SANDBOX_INHERITED_VARS: &[&str] = &["PATH"];

/// Resolve a [`SANDBOX_REDIRECTED_VARS`] entry against a sandbox home.
pub fn sandbox_var_path(home: &std::path::Path, relative: &str) -> std::path::PathBuf {
    if relative.is_empty() {
        home.to_path_buf()
    } else {
        home.join(relative)
    }
}

/// Build a sandboxed command for the given skim binary path.
///
/// This is the **single authoritative source** for the sandbox env-var block
/// used by `skim init`, `skim init --uninstall`, `skim doctor`, and
/// `skim rewrite --hook` tests. Both `skim_sandboxed` and any test that must
/// run a non-default binary (e.g. a copied binary for pin-mismatch coverage)
/// must route through here rather than hand-rolling their own env block: a
/// hand-rolled block drops entries and re-opens the leak (PF-017).
///
/// The env block is driven entirely by [`SANDBOX_REDIRECTED_VARS`],
/// [`SANDBOX_PINNED_VARS`] and [`SANDBOX_REMOVED_VARS`] so that the tables are
/// the only place the contract is written down.
///
/// Tests may chain additional `.env(...)` calls to add or override vars; a
/// chained call applied after this one wins.
pub fn skim_sandboxed_with_bin(
    home: &std::path::Path,
    bin: &std::path::Path,
) -> assert_cmd::Command {
    let mut c = assert_cmd::Command::new(bin);
    for (var, relative) in SANDBOX_REDIRECTED_VARS {
        c.env(var, sandbox_var_path(home, relative));
    }
    for (var, value) in SANDBOX_PINNED_VARS {
        c.env(var, value);
    }
    for var in SANDBOX_REMOVED_VARS {
        c.env_remove(var);
    }
    c
}

/// Return a `PATH` with the cargo-built skim binary's directory prepended.
///
/// `skim doctor` scans `$PATH` and reports drift when the binary that WINS on
/// PATH differs from the binary under test (e.g. a `target/release/skim` left
/// over from another build). Tests that assert a doctor exit code must pass
/// this so the verdict comes from the condition under test, not PATH state.
///
/// Deliberately *not* part of the sandbox env block — see
/// [`SANDBOX_INHERITED_VARS`] for why `PATH` is inherited rather than replaced.
pub fn hermetic_path() -> String {
    let bin = skim_bin();
    let bin_dir = bin.parent().expect("skim binary has a parent directory");
    let system_path = std::env::var("PATH").unwrap_or_default();
    format!("{}:{}", bin_dir.display(), system_path)
}

/// Build a `skim` command sandboxed against a temporary home directory.
///
/// Thin delegation to `skim_sandboxed_with_bin` using the default cargo-built
/// binary. All sandbox env-var documentation lives on that function.
///
/// Use this for any `skim init`, `skim init --uninstall`, `skim doctor`, or
/// `skim rewrite --hook` invocation — the last of those because the force-raw
/// sidecar is PPID-keyed and bleeds between tests in a shared nextest runner
/// unless `SKIM_CACHE_DIR` is per-test.  Tests may chain additional `.env(...)`
/// calls to add or override specific vars after calling this helper.
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
