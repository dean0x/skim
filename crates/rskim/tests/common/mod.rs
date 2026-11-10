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
// These four tables are the *whole* contract for what environment a sandboxed
// invocation may see. Every environment variable the shipped binary reads must
// appear in exactly one of them, and `cli_init.rs` carries a test that scans
// every crate linked into the `skim` binary and fails when a newly-added read
// is in none of them — PF-017's lesson was that a hand-maintained enumeration
// is always incomplete, so this one is checked against the source rather than
// trusted.
//
// Environment is not the only axis: `skim init` resolves a project root by
// walking ancestors of the process working directory, which no variable here
// can redirect. `skim_sandboxed_with_bin` pins that too — see its own docs.

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
/// Each of these is safe in its absence — either it has a default that resolves
/// *inside* the sandbox once `HOME` is redirected, or the feature it unlocks is
/// one no test wants reached — so removal is strictly safer than passing a host
/// value through. Removal also means the variable cannot silently satisfy an
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
    "ANTHROPIC_API_KEY",        // rskim-tokens credential; a host key reaches the live API
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
/// # The working directory is part of the sandbox
///
/// No environment variable can confine `skim init`'s *last* step. A successful
/// install ends in `install_search_integration`, which resolves its project root
/// with a 64-step ancestor walk from `std::env::current_dir()` and, on a hit,
/// writes `post-commit`, `post-merge` and `post-checkout` into that repository's
/// `.git/hooks` and spawns a detached `skim search --build`. Left at the cwd
/// cargo supplies (`crates/rskim`), that walk lands on the skim clone the test
/// binary was built from: the suite installs real git hooks into the developer's
/// working copy, and `SKIM_CACHE_DIR` cannot contain it because `.git/hooks` is
/// outside every path [`SANDBOX_REDIRECTED_VARS`] names.
///
/// Pinning the cwd to the sandbox home closes that axis for every caller at
/// once: the home is a bare `TempDir` with no `.git` above it, so the walk
/// returns `None` and the installer skips search integration entirely.
///
/// Tests may chain additional `.env(...)` or `.current_dir(...)` calls to add or
/// override; a chained call applied after this one wins, which is how the
/// `--project` tests point the install at a directory of their own.
pub fn skim_sandboxed_with_bin(
    home: &std::path::Path,
    bin: &std::path::Path,
) -> assert_cmd::Command {
    let mut c = assert_cmd::Command::new(bin);
    c.current_dir(home);
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
// PATH-hermetic rewrite harness (#317 Step 2b / PF-038)
// ============================================================================

/// Every program name the rewrite RULE TABLE can turn into `skim <program> …`.
///
/// One entry per distinct `prefix:` head in `cmd/rewrite/rules.rs`.  The list is
/// a LITERAL and not derived at run time, because `rskim` is bin-only (no
/// `src/lib.rs`): an integration test binary cannot `use` anything under
/// `crates/rskim/src`, so the table is unreachable as data.  A future rule whose
/// program is missing from here is caught by
/// `cli_rewrite.rs::test_rewrite_target_program_lists_match_the_engine`, which
/// re-derives both lists from the engine source and names the difference.
pub const REWRITE_RULE_TABLE_PROGRAMS: &[&str] = &[
    "./gradlew",
    "./mvnw",
    "aws",
    "biome",
    "black",
    "bundle",
    "cargo",
    "curl",
    "cypress",
    "df",
    "dig",
    "docker",
    "dotnet",
    "dprint",
    "du",
    "env",
    "eslint",
    "find",
    "gh",
    "git",
    "gmake",
    "go",
    "gofmt",
    "golangci-lint",
    "gradle",
    "gradlew",
    "grep",
    "jest",
    "kubectl",
    "ls",
    "make",
    "mvn",
    "mvnw",
    "mypy",
    "mysql",
    "npm",
    "npx",
    "nslookup",
    "oxlint",
    "pip",
    "pip3",
    "playwright",
    "pnpm",
    "prettier",
    "printenv",
    "ps",
    "psql",
    "pytest",
    "python",
    "python3",
    "rg",
    "rubocop",
    "ruff",
    "rustfmt",
    "sqlite3",
    "swift",
    "swiftlint",
    "terraform",
    "tree",
    "tsc",
    "vitest",
    "wc",
    "wget",
    "yarn",
];

/// Rewritable programs reached by `try_custom_handlers`, BELOW the rule walk.
///
/// These carry no `RewriteRule` and so appear in no `prefix:` field, but Step 2b
/// is global and gates them exactly like a table rule — `cat file.ts` does not
/// rewrite when no `cat` can be spawned.  Kept separate from
/// [`REWRITE_RULE_TABLE_PROGRAMS`] so each list can be checked against the
/// source construct that actually defines it.
pub const REWRITE_CUSTOM_HANDLER_PROGRAMS: &[&str] = &["cat", "head", "tail"];

/// A `PATH` under which EVERY rewritable program resolves, for a spawned skim.
///
/// # Why this exists
///
/// `try_rewrite`'s Step 2b declines to rewrite `<tool> …` when nothing named
/// `<tool>` can be spawned (`runner::program_resolves`), because a rewrite that
/// cannot run hands the reader a failing command in place of a working one
/// (#317, PF-038).  That makes the rewrite verdict a function of the HOST, not
/// only of the token stream: measured on the machine this harness was written
/// on, 30 of the 64 programs the rule table names — `aws`, `gradle`, `jest`,
/// `mysql`, `psql`, `rg`, `tsc`, `vitest`, … — resolve nowhere.  Without this
/// `PATH`, 43 rewrite assertions across the four rewrite-engine test files pass
/// only where the tool happens to be installed, and a further 15 that assert a
/// rewrite is DECLINED stop discriminating altogether: they go on passing while
/// the gate they were written to pin is never reached (PF-025).
///
/// # What it guarantees
///
/// Exactly one property: **every rewritable program resolves for the child.**
/// That is the whole input Step 2b consults, so a rewrite verdict taken under
/// this `PATH` is identical on every host.  It is NOT a guarantee about which
/// binary answers to a name — a program already installed keeps resolving to
/// the real thing.
///
/// # Shape
///
/// A process-lifetime stub directory PREPENDED to the inherited `PATH`, so
/// `git`, `sh` and the real toolchain stay reachable.  Initialised once per test
/// binary: 60-odd stub writes per test would be slow, and concurrent tests
/// writing the same paths would race.  The directory intentionally outlives the
/// `TempDir` guard's normal drop — `LazyLock` never runs destructors — matching
/// the `CACHE_SANDBOX` idiom in `cli_e2e_rewrite.rs`.
///
/// Set it on the CHILD (`cmd.env("PATH", common::rewrite_stub_path())`), never
/// on the test process: writing the environment is `unsafe` in a multi-threaded
/// test binary.
#[cfg(unix)]
pub fn rewrite_stub_path() -> &'static str {
    &REWRITE_STUB_PATH.1
}

/// Non-unix form: the inherited `PATH`, UNCHANGED — the guarantee does not hold.
///
/// Every piece of the harness is unix-only by construction: the stub is a
/// `#!/bin/sh` script, and `runner::is_executable` on a non-unix target checks
/// only `is_file()` with no `PATHEXT` handling, so a file dropped on `PATH`
/// under that name would not be what `Command::new` searches for.  Returning
/// the `PATH` unchanged keeps the four rewrite test files COMPILING everywhere
/// rather than silently making them unix-only, which would surface as a build
/// break the day the cross-OS matrix (#323) reaches `crates/rskim`.
///
/// It does NOT make them pass everywhere: on such a host the Step 2b verdicts
/// are host-dependent again, exactly as they were before this harness existed.
/// Closing that needs a `PATHEXT`-aware stub, which is #323's problem and not
/// this one's — said here so the next reader does not mistake a compiling
/// no-op for a working guarantee.
#[cfg(not(unix))]
pub fn rewrite_stub_path() -> &'static str {
    static INHERITED_PATH: std::sync::LazyLock<String> =
        std::sync::LazyLock::new(|| std::env::var("PATH").unwrap_or_default());
    &INHERITED_PATH
}

/// Backing state for [`rewrite_stub_path`] — the live `TempDir` and the `PATH`.
#[cfg(unix)]
static REWRITE_STUB_PATH: std::sync::LazyLock<(tempfile::TempDir, String)> =
    std::sync::LazyLock::new(|| {
        let dir = tempfile::tempdir().expect("rewrite stub TempDir must be creatable");
        let searched = path_dirs_a_spawned_skim_searches();

        for program in REWRITE_RULE_TABLE_PROGRAMS
            .iter()
            .chain(REWRITE_CUSTOM_HANDLER_PROGRAMS)
        {
            // A `/`-bearing program is an EXPLICIT PATH: `resolves_in` stats it
            // against the child's cwd and never searches `PATH`, so no directory
            // on `PATH` can serve it.  `./gradlew` and `./mvnw` are therefore
            // out of this harness's reach; a test that rewrites one needs a
            // stub in a `.current_dir()` of its own.
            if program.contains('/') {
                continue;
            }
            // Never SHADOW a tool the host really has.  Overwriting `git`,
            // `cat` or `cargo` with an inert stub would break every test in
            // these files that runs one — including the `cat`-based scripts
            // `make_stub` generates.  Skipping costs nothing: the program
            // already resolves, which is the only thing Step 2b asks.
            if resolves_for_spawned_skim(program, &searched) {
                continue;
            }
            write_resolvability_only_stub(dir.path(), program);
        }

        let path = stub_path(dir.path());
        (dir, path)
    });

/// The `PATH` directories a skim spawned from this process will search.
///
/// Not simply `split_paths(PATH)`: skim's `main()` calls
/// `strip_skim_wrappers_from_path()` as its very first statement, removing the
/// wrapper directory (`SKIM_WRAPPERS_DIR`, else `~/.skim/bin`) from its own
/// `PATH`.  Mirrored here so a tool present ONLY as a `skim init --wrappers`
/// symlink is still seen as unresolvable and still gets a stub — otherwise this
/// harness would report "resolves" for a name the child cannot find, which is
/// precisely the host dependence it exists to remove.
#[cfg(unix)]
fn path_dirs_a_spawned_skim_searches() -> Vec<std::path::PathBuf> {
    // `$HOME` rather than `dirs::home_dir()`: identical on unix, and this module
    // is compiled into every integration test binary, so it stays free of
    // dependencies it does not need.
    let wrappers_dir: Option<std::path::PathBuf> = std::env::var_os("SKIM_WRAPPERS_DIR")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|h| std::path::PathBuf::from(h).join(".skim").join("bin"))
        })
        // Syntactic normalization only, matching `filter_wrappers_from_path`.
        .map(|p| p.components().collect());

    let Some(path_var) = std::env::var_os("PATH") else {
        return Vec::new();
    };

    // An empty `PATH` element is left exactly as `split_paths` produced it, so
    // it keeps meaning the current directory as POSIX requires and `execvp`
    // implements: `Path::new("").join("git")` is the relative path `git`, which
    // `fs::metadata` resolves against the cwd.  `resolves_in` relies on the same
    // identity, so normalizing here would make this walk DIVERGE from the spawn
    // it is modelling.
    std::env::split_paths(&path_var)
        .filter(|d| {
            let normalized: std::path::PathBuf = d.components().collect();
            wrappers_dir.as_ref() != Some(&normalized)
        })
        .collect()
}

/// `true` when `program` resolves in `searched` the way `Command::new` would.
///
/// Mirrors `runner::resolves_in`'s rule deliberately: an executable REGULAR
/// FILE, so a non-executable file and a directory bearing the name both read as
/// unresolvable, exactly as they do for the spawn.
#[cfg(unix)]
fn resolves_for_spawned_skim(program: &str, searched: &[std::path::PathBuf]) -> bool {
    use std::os::unix::fs::PermissionsExt;
    searched.iter().any(|dir| {
        std::fs::metadata(dir.join(program))
            .map(|m| m.is_file() && (m.permissions().mode() & 0o111) != 0)
            .unwrap_or(false)
    })
}

/// Write a stub whose ONLY job is to make `name` resolvable.
///
/// Deliberately not a plausible tool: all 11 direct-handler invocations in the
/// four rewrite test files were measured byte-identical with and without this
/// `PATH`, because each pipes a fixture and is served by `should_read_stdin`
/// rather than a spawn — so nothing here is ever executed, and a stub that
/// pretended to be a real tool would only be able to lie convincingly.  If a
/// later test does reach one, exit 97 is a code neither skim nor a real tool
/// produces and the stderr line names the cause, so the failure is loud and
/// diagnosable rather than a silent empty-output green (PF-025).
///
/// `echo` is a `/bin/sh` builtin, so the script resolves no program of its own
/// and cannot be perturbed by anything else in this directory.
#[cfg(unix)]
fn write_resolvability_only_stub(dir: &std::path::Path, name: &str) {
    let script = format!(
        "#!/bin/sh\necho \"skim test harness: the resolvability-only PATH stub for \
         {name} was EXECUTED. It exists to make {name} resolvable for the rewrite \
         engine and has no tool behaviour. See rewrite_stub_path in \
         tests/common/mod.rs; a test that spawns {name} needs a real stub from \
         make_stub.\" >&2\nexit 97\n"
    );
    write_stub_script(dir, name, &script);
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
