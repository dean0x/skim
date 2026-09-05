//! E2E pipe-fidelity verification for the streamed raw-passthrough tier (#495).
//!
//! Skim's raw-passthrough family (`grep`, `rg`, `find`, `ls`, `wc`, `df`, `du`,
//! `ps`) must be byte-, latency-, and exit-code-faithful to the raw tool.  Before
//! the streaming layer these wrappers buffered the child's entire stdout and
//! wrote once at the end (PF-021), which produced three separate defects:
//!
//! - **Silent data loss on slow producers** — a reader that closed early got
//!   everything the raw tool had already emitted, and nothing from skim.
//! - **Latency** — nothing reached the reader until the child exited.
//! - **Non-UTF-8 corruption** — `read_pipe` decoded with a lossy fallback, so
//!   non-UTF-8 bytes reached the reader as U+FFFD.
//!
//! ## Which interception surface these tests exercise
//!
//! CLAUDE.md documents two independent interception surfaces that share the
//! per-tool handlers but NOT the dispatch front-end.  Every test in this file
//! drives the **explicit subcommand** path (`skim grep …`), so what is covered
//! here is the **handler body**, reached via `cmd::dispatch` — and *neither*
//! front-end:
//!
//! - **Not the rewrite engine.**  No test exercises `skim rewrite` / the
//!   PreToolUse hook text transformation, and none may be cited as coverage of
//!   it.
//! - **Not the PATH-wrapper front-end.**  No test sets `argv[0]`, so
//!   `detect_argv0_dispatch` never runs and neither does the wrapper-only
//!   fidelity gate above it (`main.rs`: `stdout_should_serve_raw()` →
//!   `cmd::run_inherited_passthrough`, #370).  The wrapper eventually calls the
//!   same `cmd::dispatch`, so the handler behaviour asserted here does apply
//!   there — but that is an inference about the shared body, not coverage of the
//!   wrapper path.  `crates/rskim/tests/cli_both_surfaces_paired.rs` is where
//!   `argv[0]` dispatch is actually driven.
//!
//! ## Anti-flake rules observed here
//!
//! - **Never race a live `grep`.**  Every test that spawns a child tool uses a
//!   stub on a prepended PATH that emits a fixture, so timing is a property of
//!   skim, not of the host filesystem.  (`t8_stdin_input_still_routes_to_the_buffered_path`
//!   is the one exception, and deliberately so: proving the stdin route did NOT
//!   spawn anything is the assertion.)
//! - **Never shell out to `head`.**  `assert_cmd` buffers the whole child and
//!   cannot close a pipe early, so early-close tests use
//!   `std::process::Command` + `Stdio::piped()` and drop the handle after N
//!   lines.
//! - **Assert on wide margins and ordering, never on exact byte counts past the
//!   cut.**  How much skim managed to write before the reader vanished is a
//!   kernel-scheduling detail.
//! - **Every blocking wait is bounded.**  Both `read_n_lines_then_close` and
//!   `wait_bounded` time out, because the regressions this file guards (a
//!   stalled pump, the PF-021 stderr-drain deadlock) present as a block, and
//!   there is no `.config/nextest.toml` `slow-timeout` or workflow
//!   `timeout-minutes` to catch one.
//! - **An absence is never the only assertion.**  Tests whose subject is "no
//!   stderr", "no sentinel", or "exited fast" first assert that skim actually
//!   produced output and took the pipe-closed path, via
//!   `read_n_lines_then_close_checked`; otherwise a run that emitted nothing
//!   passes them all for the wrong reason.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

mod common;

use common::{make_stub, make_stub_bytes, stub_path, write_stub_script};

/// How long a bounded wait loop will tolerate before declaring a hang.
///
/// Deliberately far above any legitimate runtime: these stubs finish in
/// milliseconds, so a value this large can only be reached by a real deadlock.
const HANG_TIMEOUT: Duration = Duration::from_secs(60);

/// Poll interval for the bounded wait loop.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Build a raw `std::process::Command` for skim with `stub_dir` first on PATH.
///
/// `assert_cmd` is deliberately not used: it buffers the child's whole output
/// and offers no way to close the read end early, which is precisely the
/// condition under test.
fn skim_with_stubs(stub_dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(common::skim_bin());
    c.args(args)
        .env("PATH", stub_path(stub_dir))
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .env_remove("SKIM_REWRITTEN_FROM");
    c
}

/// [`skim_with_stubs`] with the `SKIM_PASSTHROUGH=1` escape hatch enabled.
///
/// The escape hatch is a *different sink* from the family streaming sink: it is
/// `execution::run_parsed_command_with_exit`'s own pre-`obtain_output` branch,
/// and it carries a stricter byte contract (no trailing-newline guard on either
/// stream, no notices, no analytics).  Tests that assert escape-hatch behaviour
/// must go through here, not through [`skim_with_stubs`].
fn skim_escape_hatch(stub_dir: &Path, args: &[&str]) -> Command {
    let mut c = skim_with_stubs(stub_dir, args);
    c.env("SKIM_PASSTHROUGH", "1");
    c
}

/// A grep-shaped fixture of `n` lines, sized well past the pipe buffer.
///
/// The pipe-buffer threshold matters: below it the whole write lands before the
/// reader can close, so the early-close defect does not reproduce at all.  Each
/// line here is ~70 bytes, so 5 000 lines is ~350 KB — comfortably above the
/// 16–64 KiB pipe buffer on every supported platform.
fn grep_fixture(n: usize) -> String {
    (1..=n)
        .map(|i| {
            format!("src/module/file{i}.rs:{i}:    pub fn some_function_{i}() -> Result<()> {{}}\n")
        })
        .collect()
}

/// Read `count` lines, then drop the reader so the pipe's read end closes.
///
/// Returns the lines read.  Dropping the `BufReader` (which owns the
/// `ChildStdout`) is what makes skim's next write fail with `EPIPE` — the exact
/// thing `| head -N` does.
///
/// # Why the read is bounded
///
/// The read itself must have a timeout, not just the [`wait_bounded`] that
/// follows it.  `read_line` on a live pipe blocks forever, and "skim delivered
/// fewer than `count` lines and then stopped" is *precisely* the regression this
/// file guards (a stalled pump, or the PF-021 / AD-STR-8 stderr-drain deadlock).
/// On that regression the read blocks first, so `wait_bounded` is never reached
/// and the bound it advertises does not exist.  There is no `.config/nextest.toml`
/// `slow-timeout` and no `timeout-minutes` in any workflow, so an unbounded read
/// burns GitHub's 360-minute default instead of failing in seconds.
///
/// The reading is therefore done on a worker thread and collected with
/// `recv_timeout`; the main thread keeps `&mut Child`, so on timeout it can kill
/// skim and fail the test with a diagnosis.
fn read_n_lines_then_close(child: &mut Child, count: usize) -> Vec<String> {
    let stdout = child.stdout.take().expect("stdout must be piped");
    let (tx, rx) = mpsc::channel::<Option<String>>();

    let reader_thread = std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        for _ in 0..count {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(None);
                    return;
                }
                Ok(_) => {
                    if tx.send(Some(line)).is_err() {
                        return;
                    }
                }
            }
        }
        // Returning drops `reader` and with it the pipe's read end — the
        // `| head -N` moment.  Joining below is what makes that ordering
        // observable to the caller.
    });

    let mut lines = Vec::with_capacity(count);
    for _ in 0..count {
        match rx.recv_timeout(HANG_TIMEOUT) {
            Ok(Some(line)) => lines.push(line),
            Ok(None) => break,
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "skim delivered only {} of {count} lines and then blocked for \
                     {HANG_TIMEOUT:?}. A stalled pump or the stderr-drain deadlock \
                     (PF-021 / AD-STR-8) looks exactly like this — failing here \
                     instead of hanging CI is the whole point of the bound.",
                    lines.len()
                );
            }
        }
    }
    let _ = reader_thread.join();
    lines
}

/// [`read_n_lines_then_close`] plus the precondition that skim actually wrote.
///
/// Every early-close assertion downstream is about what happens *after* the
/// reader leaves, so a run that produced nothing never reached the code under
/// test.  Without this check a test whose only assertions are absences — no
/// stderr, no sentinel — passes for the wrong reason.  Mirrors the guard
/// [`exit_and_stderr_after_early_close`] already applies.
fn read_n_lines_then_close_checked(child: &mut Child, count: usize, tag: &str) -> Vec<String> {
    let got = read_n_lines_then_close(child, count);
    assert_eq!(
        got.len(),
        count,
        "{tag}: skim delivered {} of {count} lines before the reader closed, so this \
         invocation never reached the behaviour under test — fix the fixture, not \
         the assertion",
        got.len()
    );
    got
}

/// Wait for `child`, failing the test rather than hanging CI if it never exits.
///
/// Without this bound a regression that reintroduces the stderr-drain deadlock
/// (PF-021 / AD-STR-8) would hang the test binary instead of failing it.
fn wait_bounded(child: &mut Child, what: &str) -> std::process::ExitStatus {
    let deadline = Instant::now() + HANG_TIMEOUT;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(status) => return status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "{what}: skim did not exit within {HANG_TIMEOUT:?} — \
                     the stderr drain thread is the only thing preventing this \
                     deadlock (PF-021 / AD-STR-8)"
                );
            }
            None => std::thread::sleep(POLL_INTERVAL),
        }
    }
}

/// Open a file to use as the child's stderr sink.
///
/// A `Stdio::piped()` stderr that nobody reads is itself a deadlock source once
/// the child writes more than one pipe buffer, so tests that care about stderr
/// route it to a regular file instead.
fn stderr_file(dir: &Path, name: &str) -> (std::path::PathBuf, Stdio) {
    let path = dir.join(name);
    let f = std::fs::File::create(&path).unwrap();
    (path, Stdio::from(f))
}

// ============================================================================
// T1 — first-N parity: the bytes that do arrive are raw's bytes, exit is 141
// ============================================================================

/// The first 20 lines skim delivers before a reader closes are byte-identical
/// to the raw tool's first 20 lines, and skim exits 141 (`128 + SIGPIPE`).
///
/// Exit 141 rather than 1 is load-bearing: for `grep`/`rg`/`diff`, exit 1 is the
/// wire protocol for "no matches found", so exiting 1 on pipe closure reports a
/// false negative to anything inspecting `$?`.
#[test]
fn t1_first_n_lines_are_raw_parity_and_exit_is_pipe_closed() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = grep_fixture(5_000);
    make_stub(dir.path(), "grep", &fixture, "", 0);

    let mut child = skim_with_stubs(dir.path(), &["grep", "-rn", "some_function", "."])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let got = read_n_lines_then_close(&mut child, 20);
    let status = wait_bounded(&mut child, "t1");

    let expected: Vec<String> = fixture.lines().take(20).map(|l| format!("{l}\n")).collect();
    assert_eq!(
        got, expected,
        "the lines delivered before the reader closed must be raw's own first 20 lines"
    );
    assert_eq!(
        status.code(),
        Some(141),
        "a closed downstream reader must exit 141 (128 + SIGPIPE), never 1 \
         (exit 1 is grep's 'no matches found')"
    );
}

// ============================================================================
// T2 — time to first byte: the decisive streaming test
// ============================================================================

/// How long the T2 stub stalls between its two output lines.
const T2_STALL: Duration = Duration::from_secs(3);

/// The reader observes the producer's own stall — proof that skim streams.
///
/// This is the test that proves streaming.  The stub emits one line, stalls for
/// [`T2_STALL`], then emits a second line.  A buffered wrapper reads the child
/// to EOF and writes both lines in a single `write_all`, so the reader sees them
/// microseconds apart; a streaming wrapper reproduces the producer's timeline,
/// so the reader sees the full stall between them.
///
/// # Why the gap and not an absolute deadline
///
/// The obvious formulation — "the first line arrives within N seconds of
/// `spawn()`" — measures skim's *process-start* cost as well as its buffering,
/// and that cost is not small or stable: a debug-build binary under 4-way
/// parallel `nextest` on macOS took ~1.5 s just to reach `main`, which flakes any
/// deadline tight enough to be meaningful.  The inter-line gap cancels start-up
/// cost entirely: the discrimination is ~0 s (buffered) versus [`T2_STALL`]
/// (streamed), and no amount of scheduling noise can stretch a single 27-byte
/// write into a multi-second gap.
#[test]
fn t2_reader_observes_the_producers_stall() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("first.txt"), "a.rs:1:first\n").unwrap();
    std::fs::write(dir.path().join("second.txt"), "a.rs:2:second\n").unwrap();
    // `cat` (an external process) rather than the shell's `echo` builtin: the
    // bytes are guaranteed to reach the pipe without depending on how the shell
    // buffers its own stdout.
    write_stub_script(
        dir.path(),
        "grep",
        &format!(
            "#!/bin/sh\ncat '{}'\nsleep {}\ncat '{}'\n",
            dir.path().join("first.txt").display(),
            T2_STALL.as_secs(),
            dir.path().join("second.txt").display()
        ),
    );

    let mut child = skim_with_stubs(dir.path(), &["grep", "-rn", "first", "."])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // Timestamp each line as it lands, on a thread, so a stall in the stream
    // cannot block the assertions below.
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(l) = line else { return };
            if tx.send((l, Instant::now())).is_err() {
                return;
            }
        }
    });

    let (first, t_first) = rx
        .recv_timeout(HANG_TIMEOUT)
        .expect("the first line never arrived");
    let (second, t_second) = rx
        .recv_timeout(HANG_TIMEOUT)
        .expect("the second line never arrived");
    wait_bounded(&mut child, "t2");

    assert_eq!(first, "a.rs:1:first");
    assert_eq!(second, "a.rs:2:second");

    let gap = t_second.duration_since(t_first);
    assert!(
        gap >= T2_STALL / 2,
        "the two lines arrived {gap:?} apart but the producer stalled {T2_STALL:?} between \
         them — skim buffered the whole stream and replayed it in one write instead of \
         streaming it"
    );
}

// ============================================================================
// T3 — stderr hygiene on the pipe-closed path (ADR-011 class 2)
// ============================================================================

/// A closed reader produces zero stderr bytes by default, and a debug-gated
/// banner under `SKIM_DEBUG=1`.
///
/// ADR-011 class 2: nothing was lost — the *reader* chose to stop reading, and
/// raw `grep | head` is silent in exactly this situation — so the notice is a
/// no-loss raw-fallback banner and must be debug-gated.  In particular
/// `Error: Broken pipe (os error 32)` must never appear.
#[test]
fn t3_pipe_close_is_silent_by_default_and_bannered_under_debug() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = grep_fixture(5_000);
    make_stub(dir.path(), "grep", &fixture, "", 0);

    // --- default: completely silent ---
    let (quiet_path, quiet_sink) = stderr_file(dir.path(), "quiet.err");
    let mut child = skim_with_stubs(dir.path(), &["grep", "-rn", "some_function", "."])
        .stdout(Stdio::piped())
        .stderr(quiet_sink)
        .spawn()
        .unwrap();
    read_n_lines_then_close_checked(&mut child, 20, "t3-quiet");
    let quiet_status = wait_bounded(&mut child, "t3-quiet");
    // Both assertions below are ABSENCES, which a run that emitted nothing and
    // exited would satisfy for the wrong reason.  Pin the pipe-closed exit first
    // so "silent" can only mean "silent on the path under test".
    assert_eq!(
        quiet_status.code(),
        Some(141),
        "t3-quiet: the run must actually have taken the pipe-closed path"
    );
    let quiet = std::fs::read_to_string(&quiet_path).unwrap();
    assert!(
        quiet.is_empty(),
        "a closed reader must cost zero stderr bytes by default (ADR-011 class 2); got: {quiet:?}"
    );
    assert!(
        !quiet.contains("Broken pipe"),
        "`Error: Broken pipe` must never reach stderr"
    );

    // --- SKIM_DEBUG=1: the banner appears ---
    let (debug_path, debug_sink) = stderr_file(dir.path(), "debug.err");
    let mut child = skim_with_stubs(dir.path(), &["grep", "-rn", "some_function", "."])
        .env("SKIM_DEBUG", "1")
        .stdout(Stdio::piped())
        .stderr(debug_sink)
        .spawn()
        .unwrap();
    read_n_lines_then_close(&mut child, 20);
    wait_bounded(&mut child, "t3-debug");
    let debug = std::fs::read_to_string(&debug_path).unwrap();
    assert!(
        debug.contains("closed the pipe"),
        "SKIM_DEBUG=1 must surface the debug-gated pipe-closed banner; got: {debug:?}"
    );
}

// ============================================================================
// T4 — deadlock guard: concurrent stderr drain (PF-021 / AD-STR-8)
// ============================================================================

/// ~1 MiB interleaved on stdout AND stderr completes without deadlocking.
///
/// Streaming stdout on the main thread reintroduces the hazard that the
/// two-reader-thread buffered runner made structurally impossible: if nobody
/// drains stderr, the child blocks writing stderr once that pipe fills, stops
/// writing stdout, and the stdout pump blocks forever.  [`wait_bounded`] turns
/// that regression into a test failure instead of a CI hang.
#[test]
fn t4_interleaved_stdout_and_stderr_does_not_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    // 64 KiB per chunk × 16 iterations = 1 MiB on each stream. Both chunks end
    // with a newline so the trailing-newline guard is not part of this test.
    let chunk_out: String = std::iter::repeat_n("o".repeat(63), 1024)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let chunk_err: String = std::iter::repeat_n("e".repeat(63), 1024)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(dir.path().join("chunk.out"), &chunk_out).unwrap();
    std::fs::write(dir.path().join("chunk.err"), &chunk_err).unwrap();
    write_stub_script(
        dir.path(),
        "grep",
        &format!(
            "#!/bin/sh\ni=0\nwhile [ $i -lt 16 ]; do\n  cat '{}'\n  cat '{}' >&2\n  i=$((i+1))\ndone\n",
            dir.path().join("chunk.out").display(),
            dir.path().join("chunk.err").display()
        ),
    );

    let (out_path, out_sink) = stderr_file(dir.path(), "big.out");
    let (err_path, err_sink) = stderr_file(dir.path(), "big.err");
    let mut child = skim_with_stubs(dir.path(), &["grep", "-rn", "o", "."])
        .stdout(out_sink)
        .stderr(err_sink)
        .spawn()
        .unwrap();
    let status = wait_bounded(&mut child, "t4");

    assert_eq!(status.code(), Some(0), "stub exits 0");
    let out_len = std::fs::metadata(&out_path).unwrap().len() as usize;
    let err_len = std::fs::metadata(&err_path).unwrap().len() as usize;
    assert_eq!(
        out_len,
        chunk_out.len() * 16,
        "every stdout byte must reach the reader (#317: compress, never truncate)"
    );
    assert!(
        err_len >= chunk_err.len() * 16,
        "child stderr is forwarded verbatim (forward_stderr: true); got {err_len} bytes"
    );
}

// ============================================================================
// T6 — the child is killed when the reader leaves
// ============================================================================

/// After an early close the child is reaped before it can finish its work.
///
/// The stub writes a large fixture, stalls, then touches a sentinel.  Raw
/// `grep … | head` dies immediately on SIGPIPE; skim must do the same via
/// `ChildGuard` kill-on-drop (ADR-008: there is no internal timeout — the guard
/// is the only thing bounding child lifetime).  A buffered wrapper instead waits
/// for the child to exit before writing anything, so the sentinel appears.
#[test]
fn t6_child_is_killed_when_the_reader_closes_early() {
    let dir = tempfile::tempdir().unwrap();
    let sentinel = dir.path().join("completed.sentinel");
    std::fs::write(dir.path().join("big.out"), grep_fixture(5_000)).unwrap();
    write_stub_script(
        dir.path(),
        "grep",
        &format!(
            "#!/bin/sh\ncat '{}'\nsleep 1\ntouch '{}'\n",
            dir.path().join("big.out").display(),
            sentinel.display()
        ),
    );

    let mut child = skim_with_stubs(dir.path(), &["grep", "-rn", "some_function", "."])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    read_n_lines_then_close_checked(&mut child, 20, "t6");
    let status = wait_bounded(&mut child, "t6");
    assert_eq!(
        status.code(),
        Some(141),
        "t6: the run must actually have taken the pipe-closed path — otherwise \
         the sentinel assertion below proves nothing about ChildGuard"
    );

    // The stub's path to the sentinel is `cat` (dies on EPIPE) -> `sleep 1` ->
    // `touch`, so a surviving child creates it ~1 s after skim exits.  Poll to a
    // deadline rather than sleeping a fixed 2 s: with only ~1 s of margin, a
    // loaded 4-way test runner could let the sentinel land *after* a fixed
    // check, and that failure mode is a silent PASS, not a flake.
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        assert!(
            !sentinel.exists(),
            "the child kept running after the reader left — ChildGuard must kill it, \
             exactly as raw grep dies on SIGPIPE"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// An early close returns promptly even when a GRANDCHILD still holds stderr.
///
/// Killing the child closes only *its* copy of the stderr write end.  A
/// grandchild that inherited fd 2 keeps the pipe open, so `drain_capped` never
/// reaches EOF — and `stream_child` must therefore **abandon** the drain thread
/// on the early-close path rather than join it.  Joining blocks skim for the
/// grandchild's entire lifetime while the raw tool returns at once, which is the
/// exact latency defect this tier exists to fix.
///
/// Real shape: `SKIM_PASSTHROUGH=1 skim cargo build | head` and
/// `skim yarn <sub> | head` — cargo/yarn/npm all spawn tool processes that
/// inherit stderr.  Measured before the fix: 26.5 s for a 25 s grandchild,
/// against 0.33 s for the raw tool.
///
/// The assertion is a *gap*, not an absolute deadline from `spawn()`: it starts
/// counting only after the reader has closed the pipe, so process startup under
/// a parallel test runner is not part of the budget.
#[test]
fn t6b_early_close_does_not_wait_for_a_grandchild_holding_stderr() {
    /// Grandchild lifetime.  Long enough that joining the drain is unmistakable.
    const GRANDCHILD_SECS: u64 = 20;
    /// Budget for skim to exit after the reader leaves.  Far below
    /// `GRANDCHILD_SECS`, far above a prompt teardown.
    const EXIT_BUDGET: Duration = Duration::from_secs(5);

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("big.out"), grep_fixture(5_000)).unwrap();
    // `( sleep N ) &` inherits fd 2 from the stub and outlives a kill of it.
    write_stub_script(
        dir.path(),
        "grep",
        &format!(
            "#!/bin/sh\n( sleep {GRANDCHILD_SECS} ) &\ncat '{}'\n",
            dir.path().join("big.out").display()
        ),
    );

    let mut child = skim_with_stubs(dir.path(), &["grep", "-rn", "some_function", "."])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    read_n_lines_then_close_checked(&mut child, 20, "t6b");

    let closed_at = Instant::now();
    let status = wait_bounded(&mut child, "t6b");
    let exit_gap = closed_at.elapsed();

    assert_eq!(
        status.code(),
        Some(141),
        "t6b: the run must actually have taken the pipe-closed path — a run that \
         emitted nothing would also exit fast and pass the budget below"
    );
    assert!(
        exit_gap < EXIT_BUDGET,
        "skim took {exit_gap:?} to exit after the reader closed the pipe, \
         budget {EXIT_BUDGET:?}. A grandchild is holding the child's stderr \
         write end open, so the drain thread never reaches EOF — the \
         early-close path must ABANDON that thread, not join it (ADR-008 \
         forbids an internal timeout, so not waiting is the only bound)."
    );
}

// ============================================================================
// T7 — non-UTF-8 bytes reach the reader verbatim
// ============================================================================

/// Non-UTF-8 bytes from the tool arrive byte-for-byte.
///
/// The buffered path decodes the child's pipe through
/// `String::from_utf8(..).unwrap_or_else(lossy)`, so `0xFF 0xFE` reached the
/// reader as U+FFFD — skim showing something *different* from raw with no
/// marker, a #317 violation.  The byte pump has no decode step at all.
#[test]
fn t7_non_utf8_bytes_pass_through_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let payload: &[u8] = b"a.rs:1:caf\xc3\xa9\ta.rs:2:\xff\xfe raw\x80bytes\n";
    make_stub_bytes(dir.path(), "grep", payload, b"", 0);

    let out = skim_with_stubs(dir.path(), &["grep", "-rn", "raw", "."])
        .output()
        .unwrap();

    assert_eq!(
        out.stdout, payload,
        "non-UTF-8 tool bytes must reach the reader verbatim, not as U+FFFD"
    );
}

/// The trailing-newline guard is preserved: output that does not end with a
/// newline still gets exactly one appended, matching the buffered sink.
///
/// This is deliberate parity, not an accident of the pump: `emit_raw_passthrough`
/// has always appended a trailing newline to a non-empty body, and the streamed
/// path must not silently change that for the same command.
#[test]
fn t7b_missing_trailing_newline_is_added_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let payload: &[u8] = b"a.rs:1:no trailing newline";
    make_stub_bytes(dir.path(), "grep", payload, b"", 0);

    let out = skim_with_stubs(dir.path(), &["grep", "-rn", "no", "."])
        .output()
        .unwrap();

    let mut expected = payload.to_vec();
    expected.push(b'\n');
    assert_eq!(out.stdout, expected);
}

// ============================================================================
// T8 — the three buffered-path exclusions still route to the buffered sink
// ============================================================================

/// `--json` keeps the buffered path: the JSON envelope needs the whole string.
#[test]
fn t8_json_output_still_routes_to_the_buffered_path() {
    let dir = tempfile::tempdir().unwrap();
    make_stub(dir.path(), "grep", "a.rs:1:hit\n", "", 0);

    let out = skim_with_stubs(dir.path(), &["grep", "--json", "-rn", "hit", "."])
        .output()
        .unwrap();

    let body = String::from_utf8_lossy(&out.stdout);
    assert!(
        body.starts_with("{\"tier\":\"passthrough\""),
        "--json must still produce the passthrough envelope; got: {body:?}"
    );
    assert!(body.contains("a.rs:1:hit"));
}

/// `--show-stats` keeps the buffered path so the reported token counts stay
/// byte-for-byte the numbers they were.
///
/// `record_and_report` tokenizes both the raw and compressed strings; the
/// streamed path never holds either, so approximating would silently change a
/// user-visible number.  That is exactly why the exclusion exists.
#[test]
fn t8_show_stats_still_routes_to_the_buffered_path() {
    let dir = tempfile::tempdir().unwrap();
    make_stub(dir.path(), "grep", "a.rs:1:hit\na.rs:2:hit\n", "", 0);

    let out = skim_with_stubs(dir.path(), &["grep", "--show-stats", "-rn", "hit", "."])
        .output()
        .unwrap();

    assert_eq!(out.stdout, b"a.rs:1:hit\na.rs:2:hit\n");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("[skim]") && err.contains("tokens"),
        "--show-stats must still print the token-stats line; got: {err:?}"
    );
}

/// Piped stdin keeps the buffered path — there is no child process to stream
/// from at all.
#[test]
fn t8_stdin_input_still_routes_to_the_buffered_path() {
    use std::io::Write;

    let dir = tempfile::tempdir().unwrap();
    // No args at all → `should_read_stdin` is true when stdin is not a terminal.
    let mut child = skim_with_stubs(dir.path(), &["wc"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"piped stdin body\n")
        .unwrap();
    let mut body = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut body)
        .unwrap();
    wait_bounded(&mut child, "t8-stdin");

    assert!(
        body.contains("piped stdin body"),
        "stdin must still be served by the buffered path; got: {body:?}"
    );
}

// ============================================================================
// T9–T14 — the SKIM_PASSTHROUGH=1 escape hatch (A2)
//
// The escape hatch is the surface a user reaches for *because* compressed
// output hid something.  Before A2 it was the least faithful sink skim had: it
// buffered the child's whole stdout through `runner::read_pipe`, which
// hard-errors at MAX_OUTPUT_BYTES and **discards the entire buffer** — so the
// documented remedy for "skim hid my output" returned nothing at all.
//
// Surface: every test below drives the **explicit subcommand** path
// (`skim grep …`), reaching the handler through `cmd::dispatch`.  None of them
// exercises the rewrite engine, and none sets `argv[0]`, so the PATH-wrapper
// front-end (`detect_argv0_dispatch` and the `stdout_should_serve_raw` gate) is
// not covered either — see the module header.
// ============================================================================

/// How long the T9 stub stalls between its two output lines.
const T9_STALL: Duration = Duration::from_secs(3);

/// The escape hatch reproduces the producer's timeline instead of replaying it.
///
/// Same construction as [`t2_reader_observes_the_producers_stall`], and for the
/// same reason: the assertion is on the **gap between two emitted lines**, never
/// on an absolute deadline from `spawn()`.  A debug binary under 4-way parallel
/// `nextest` on macOS can take ~1.5 s just to reach `main`, which flakes any
/// absolute deadline tight enough to discriminate.  The gap cancels start-up
/// cost entirely: ~0 s buffered versus [`T9_STALL`] streamed.
#[test]
fn t9_escape_hatch_reader_observes_the_producers_stall() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("first.txt"), "a.rs:1:first\n").unwrap();
    std::fs::write(dir.path().join("second.txt"), "a.rs:2:second\n").unwrap();
    // `cat` rather than the shell's `echo` builtin: otherwise the *shell's* own
    // stdout buffering, not skim's, decides when bytes reach the pipe.
    write_stub_script(
        dir.path(),
        "grep",
        &format!(
            "#!/bin/sh\ncat '{}'\nsleep {}\ncat '{}'\n",
            dir.path().join("first.txt").display(),
            T9_STALL.as_secs(),
            dir.path().join("second.txt").display()
        ),
    );

    let mut child = skim_escape_hatch(dir.path(), &["grep", "-rn", "first", "."])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(l) = line else { return };
            if tx.send((l, Instant::now())).is_err() {
                return;
            }
        }
    });

    let (first, t_first) = rx
        .recv_timeout(HANG_TIMEOUT)
        .expect("the first line never arrived through the escape hatch");
    let (second, t_second) = rx
        .recv_timeout(HANG_TIMEOUT)
        .expect("the second line never arrived through the escape hatch");
    wait_bounded(&mut child, "t9");

    assert_eq!(first, "a.rs:1:first");
    assert_eq!(second, "a.rs:2:second");

    let gap = t_second.duration_since(t_first);
    assert!(
        gap >= T9_STALL / 2,
        "the two lines arrived {gap:?} apart but the producer stalled {T9_STALL:?} between \
         them — SKIM_PASSTHROUGH=1 buffered the whole stream instead of streaming it"
    );
}

/// The escape hatch delivers raw's own first 20 lines and exits 141.
///
/// `head` is deliberately not used: `assert_cmd` buffers the whole child and
/// cannot close a pipe early, so this drops a `BufReader` after N lines instead.
#[test]
fn t10_escape_hatch_first_n_parity_and_exit_is_pipe_closed() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = grep_fixture(5_000);
    make_stub(dir.path(), "grep", &fixture, "", 0);

    let mut child = skim_escape_hatch(dir.path(), &["grep", "-rn", "some_function", "."])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let got = read_n_lines_then_close(&mut child, 20);
    let status = wait_bounded(&mut child, "t10");

    let expected: Vec<String> = fixture.lines().take(20).map(|l| format!("{l}\n")).collect();
    assert_eq!(
        got, expected,
        "the escape hatch must deliver raw's own first 20 lines before the reader closed"
    );
    assert_eq!(
        status.code(),
        Some(141),
        "a closed downstream reader must exit 141 (128 + SIGPIPE), never 1"
    );
}

/// The escape hatch delivers output far past the buffered 64 MiB ceiling.
///
/// **This is the headline fix.**  `runner::read_pipe` hard-errors at
/// `MAX_OUTPUT_BYTES` and throws the accumulated buffer away, so before A2 this
/// exact invocation produced `Error: output exceeded 67108864 byte limit`, exit
/// 1, and **zero bytes of stdout** — measured, not theorised.  A byte pump has
/// no ceiling at all: memory is O(chunk) because each chunk is written out
/// before the next is read.
///
/// `#[ignore]` because it moves 70 MiB through a pipe; the always-run guard is
/// the pure-function `pump` test in `cmd::stream_pump`
/// (`test_pump_delivers_everything_past_a_buffered_style_ceiling`), which uses
/// an injectable limit in the `read_pipe_degrade_impl(reader, limit)` style.
/// Run with `cargo nextest run -p rskim --all-targets --run-ignored all`.
#[test]
#[ignore = "moves 70 MiB through a pipe; the pump unit test is the always-run guard"]
fn t11_escape_hatch_delivers_past_the_buffered_ceiling() {
    const MIB: usize = 1024 * 1024;
    const EMITTED_MIB: usize = 70;

    let dir = tempfile::tempdir().unwrap();
    write_stub_script(
        dir.path(),
        "grep",
        &format!(
            "#!/bin/sh\ndd if=/dev/zero bs={MIB} count={EMITTED_MIB} 2>/dev/null | tr '\\0' 'x'\n"
        ),
    );

    let (out_path, out_sink) = stderr_file(dir.path(), "huge.out");
    let (err_path, err_sink) = stderr_file(dir.path(), "huge.err");
    let mut child = skim_escape_hatch(dir.path(), &["grep", "-rn", "x", "."])
        .stdout(out_sink)
        .stderr(err_sink)
        .spawn()
        .unwrap();
    let status = wait_bounded(&mut child, "t11");

    let delivered = std::fs::metadata(&out_path).unwrap().len() as usize;
    let errs = std::fs::read_to_string(&err_path).unwrap();
    assert_eq!(
        delivered,
        EMITTED_MIB * MIB,
        "every byte must be DELIVERED past the old 64 MiB ceiling — the buffered \
         path discarded the whole buffer here and emitted nothing; stderr was: {errs:?}"
    );
    assert_eq!(status.code(), Some(0), "the stub exits 0");
    assert!(
        !errs.contains("byte limit"),
        "the escape hatch must not hard-error at a byte ceiling; got: {errs:?}"
    );
}

/// Non-UTF-8 bytes survive the escape hatch verbatim.
///
/// `runner::read_pipe` decodes with `String::from_utf8(..).unwrap_or_else(lossy)`,
/// so `0xFF 0xFE` reached the reader as two U+FFFD sequences — skim showing
/// something *different* from raw, with no marker (#317).  The byte pump has no
/// decode step.
#[test]
fn t12_escape_hatch_passes_non_utf8_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let payload: &[u8] = b"a.rs:1:caf\xc3\xa9\ta.rs:2:\xff\xfe raw\x80bytes\n";
    make_stub_bytes(dir.path(), "grep", payload, b"", 0);

    let out = skim_escape_hatch(dir.path(), &["grep", "-rn", "raw", "."])
        .output()
        .unwrap();

    assert_eq!(
        out.stdout, payload,
        "the escape hatch must forward non-UTF-8 tool bytes verbatim, not as U+FFFD"
    );
}

/// ~1 MiB interleaved on stdout AND stderr completes without deadlocking.
///
/// Streaming stdout on the calling thread reintroduces the pipe-full deadlock
/// that the two-reader-thread buffered runner made structurally impossible: with
/// nobody draining stderr the child blocks once that pipe fills, stops writing
/// stdout, and the stdout pump blocks forever (PF-021 / AD-STR-8).
///
/// [`wait_bounded`] is the explicit timeout: it polls `try_wait` against
/// [`HANG_TIMEOUT`] and **kills the child and panics** rather than blocking, so a
/// regression fails CI instead of hanging it.
#[test]
fn t13_escape_hatch_interleaved_stdout_and_stderr_does_not_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let chunk_out: String = std::iter::repeat_n("o".repeat(63), 1024)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let chunk_err: String = std::iter::repeat_n("e".repeat(63), 1024)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(dir.path().join("chunk.out"), &chunk_out).unwrap();
    std::fs::write(dir.path().join("chunk.err"), &chunk_err).unwrap();
    write_stub_script(
        dir.path(),
        "grep",
        &format!(
            "#!/bin/sh\ni=0\nwhile [ $i -lt 16 ]; do\n  cat '{}'\n  cat '{}' >&2\n  i=$((i+1))\ndone\n",
            dir.path().join("chunk.out").display(),
            dir.path().join("chunk.err").display()
        ),
    );

    let (out_path, out_sink) = stderr_file(dir.path(), "eh.out");
    let (err_path, err_sink) = stderr_file(dir.path(), "eh.err");
    let mut child = skim_escape_hatch(dir.path(), &["grep", "-rn", "o", "."])
        .stdout(out_sink)
        .stderr(err_sink)
        .spawn()
        .unwrap();
    let status = wait_bounded(&mut child, "t13");

    assert_eq!(status.code(), Some(0), "stub exits 0");
    assert_eq!(
        std::fs::metadata(&out_path).unwrap().len() as usize,
        chunk_out.len() * 16,
        "every stdout byte must reach the reader (#317: compress, never truncate)"
    );
    assert!(
        std::fs::metadata(&err_path).unwrap().len() as usize >= chunk_err.len() * 16,
        "the escape hatch forwards child stderr verbatim"
    );
}

/// The escape hatch stays byte-exact: it must NOT append a trailing newline.
///
/// This is the regression guard for the `choose_passthrough_sink` routing
/// decision.  The family streaming sink (`run_passthrough_streamed`) reproduces
/// `emit_raw_passthrough`'s trailing-newline guard and appends one — see
/// [`t7b_missing_trailing_newline_is_added_exactly_once`].  The escape hatch
/// reproduces `passthrough_raw`'s contract instead and appends nothing, because
/// a newline the raw tool never emitted is exactly the divergence
/// `SKIM_PASSTHROUGH=1` exists to escape.  If this test starts failing, the
/// `passthrough_mode` field was dropped from `choose_passthrough_sink` and the
/// escape hatch is being served by the wrong sink.
#[test]
fn t14_escape_hatch_does_not_append_a_trailing_newline() {
    let dir = tempfile::tempdir().unwrap();
    let payload: &[u8] = b"a.rs:1:no trailing newline";
    make_stub_bytes(dir.path(), "grep", payload, b"", 0);

    let out = skim_escape_hatch(dir.path(), &["grep", "-rn", "no", "."])
        .output()
        .unwrap();

    assert_eq!(
        out.stdout, payload,
        "SKIM_PASSTHROUGH=1 is byte-exact — it must not add a newline the tool never emitted"
    );
}

// ============================================================================
// T15 — `println!` must never panic on a closed pipe (exit 101)
//
// `println!`/`print!` PANIC when the downstream reader is gone:
//
//     thread 'main' panicked at library/std/src/io/stdio.rs:1165:9:
//     failed printing to stdout: Broken pipe (os error 32)
//
// A panic is not an `Err`, so neither the `StdoutStatus` sinks nor the
// `is_broken_pipe_chain` boundary in `main.rs` can catch it: the process exits
// **101** with a panic message on stderr where the raw tool exits **141** in
// silence.  Measured against the pre-fix binary, every command below reproduced
// exit 101 with one `panicked at` line.
//
// Surface: these drive the **explicit subcommand** path (`skim git log …`,
// `skim make`, `skim vitest run`), reaching the handler through `cmd::dispatch`.
// Neither front-end is covered: not the rewrite engine, and not the PATH-wrapper
// dispatch (no test sets `argv[0]`) — see the module header.
// ============================================================================

/// A `git log --format=%h %s (%cr) <%an>`-shaped fixture of `n` commits.
///
/// Shape matters here in a way it does not for [`grep_fixture`]: `git log`'s
/// parser only produces a large rendering (JSON or text) when it actually
/// recognises commit lines.  Fed grep-shaped bytes it parses zero commits, the
/// rendering is a few hundred bytes, the whole write fits the pipe buffer, and
/// the defect does not reproduce at all — the same size-dependence that made the
/// original bug look flaky.
fn git_log_fixture(n: usize) -> String {
    (1..=n)
        .map(|i| format!("a1b2c{i:04} refactor module {i} for clarity and speed ({i} days ago) <Dean Sharon>\n"))
        .collect()
}

/// `n` lines that no build/test-runner parser recognises.
///
/// The build and test-runner probes need the **passthrough** tier, where the
/// handler forwards the whole raw body.  Shape matters for the opposite reason
/// it does in [`git_log_fixture`]: fed [`grep_fixture`]'s `path:line:content`
/// lines, the vitest parser produces a `pass: 0 fail: 0 skip: 0` summary — a
/// 24-byte write that fits the pipe buffer, so the reader never closes on it and
/// the defect does not reproduce.  Deliberately anodyne prose keeps every parser
/// on its passthrough arm.
fn unparseable_fixture(n: usize) -> String {
    (1..=n)
        .map(|i| {
            format!("some arbitrary unparseable output line number {i} with padding text here\n")
        })
        .collect()
}

/// Run `skim <args>` with `stub_dir` first on PATH, let a reader take `lines`
/// lines and then vanish, and report `(exit code, stderr text)`.
///
/// `stdin` is `/dev/null` deliberately: several handlers call
/// `should_read_stdin`, and an inherited terminal stdin makes them block instead
/// of spawning the stub.
fn exit_and_stderr_after_early_close(
    stub_dir: &Path,
    args: &[&str],
    lines: usize,
    tag: &str,
) -> (Option<i32>, String) {
    let (err_path, err_sink) = stderr_file(stub_dir, &format!("{tag}.err"));
    let mut child = skim_with_stubs(stub_dir, args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(err_sink)
        .spawn()
        .unwrap();
    let got = read_n_lines_then_close(&mut child, lines);
    assert!(
        !got.is_empty(),
        "{tag}: skim produced no output at all, so this invocation never reached \
         the write under test — fix the fixture, not the assertion"
    );
    let status = wait_bounded(&mut child, tag);
    (
        status.code(),
        std::fs::read_to_string(&err_path).unwrap_or_default(),
    )
}

/// Assert the pipe-closed contract: exit 141, and **no panic** on stderr.
fn assert_panic_free_pipe_close(code: Option<i32>, stderr: &str, what: &str) {
    assert!(
        !stderr.contains("panicked at"),
        "{what}: `println!` panicked on the closed pipe — stderr was:\n{stderr}"
    );
    assert_ne!(
        code,
        Some(101),
        "{what}: exit 101 is the panic-abort code; raw exits 141 silently"
    );
    assert_eq!(
        code,
        Some(141),
        "{what}: a closed downstream reader must exit 141 (128 + SIGPIPE); stderr was:\n{stderr}"
    );
}

/// `cmd/git/mod.rs` — `run_passthrough`'s `print!("{}", output.stdout)`.
///
/// `--format` routes `git log` to the flag-aware passthrough, which forwards the
/// tool's stdout verbatim.  Pre-fix: exit 101 + panic.
#[test]
fn t15_git_mod_passthrough_does_not_panic_on_closed_pipe() {
    let dir = tempfile::tempdir().unwrap();
    make_stub(dir.path(), "git", &grep_fixture(5_000), "", 0);

    let (code, err) = exit_and_stderr_after_early_close(
        dir.path(),
        &["git", "log", "--format=%H"],
        3,
        "t15-git-mod",
    );
    assert_panic_free_pipe_close(code, &err, "git/mod.rs run_passthrough");
}

/// `cmd/git/mod.rs` — `run_parsed_command`'s compressed-result `println!("{s}")`.
///
/// `git status` over a large porcelain-v2 fixture keeps the compressed body
/// (the net-savings guard says Keep), which is the arm that used `println!`.
#[test]
fn t15_git_mod_parsed_command_does_not_panic_on_closed_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let mut fixture = String::from(
        "# branch.oid abc123\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n",
    );
    for i in 1..=5_000 {
        fixture.push_str(&format!(
            "1 .M N... 100644 100644 100644 aaa bbb src/module/file{i}.rs\n"
        ));
    }
    make_stub(dir.path(), "git", &fixture, "", 0);

    let (code, err) =
        exit_and_stderr_after_early_close(dir.path(), &["git", "status"], 3, "t15-git-status");
    assert_panic_free_pipe_close(code, &err, "git/mod.rs run_parsed_command");
}

/// `cmd/git/log.rs` — the `--json` arm's `println!("{json}")`.
#[test]
fn t15_git_log_json_does_not_panic_on_closed_pipe() {
    let dir = tempfile::tempdir().unwrap();
    make_stub(dir.path(), "git", &git_log_fixture(5_000), "", 0);

    let (code, err) =
        exit_and_stderr_after_early_close(dir.path(), &["git", "log", "--json"], 3, "t15-git-log");
    assert_panic_free_pipe_close(code, &err, "git/log.rs --json");
}

/// `cmd/build/mod.rs` — the passthrough-tier `println!("{content}")`.
#[test]
fn t15_build_does_not_panic_on_closed_pipe() {
    let dir = tempfile::tempdir().unwrap();
    make_stub(dir.path(), "make", &unparseable_fixture(5_000), "", 0);

    let (code, err) = exit_and_stderr_after_early_close(dir.path(), &["make"], 3, "t15-build");
    assert_panic_free_pipe_close(code, &err, "build/mod.rs");
}

/// `cmd/test/shared.rs` — the `ParseResult::Passthrough` arm's `println!("{raw}")`.
#[test]
fn t15_test_runner_does_not_panic_on_closed_pipe() {
    let dir = tempfile::tempdir().unwrap();
    make_stub(dir.path(), "vitest", &unparseable_fixture(5_000), "", 0);

    let (code, err) =
        exit_and_stderr_after_early_close(dir.path(), &["vitest", "run"], 3, "t15-test");
    assert_panic_free_pipe_close(code, &err, "test/shared.rs");
}

/// `cmd/git/show.rs` — the file-content / non-commit `print!("{raw}")` sinks.
///
/// Not in the originally reported set; found by the sweep and reproduced at
/// exit 101 the same way.
#[test]
fn t15_git_show_does_not_panic_on_closed_pipe() {
    let dir = tempfile::tempdir().unwrap();
    make_stub(dir.path(), "git", &grep_fixture(5_000), "", 0);

    let (code, err) =
        exit_and_stderr_after_early_close(dir.path(), &["git", "show", "HEAD"], 3, "t15-git-show");
    assert_panic_free_pipe_close(code, &err, "git/show.rs");
}

/// stderr stays clean on the pipe-closed path for a `println!`-family handler.
///
/// ADR-011 class 2: nothing is lost (the *reader* stopped reading) and the raw
/// tool is silent here, so the pipe-closed notice is a debug-gated banner and
/// the default run must cost **zero** stderr bytes.  The panic message this
/// replaces was 254 bytes of unconditional noise into every agent's transcript.
#[test]
fn t15_pipe_close_costs_zero_stderr_bytes() {
    let dir = tempfile::tempdir().unwrap();
    make_stub(dir.path(), "make", &unparseable_fixture(5_000), "", 0);

    let (code, err) = exit_and_stderr_after_early_close(dir.path(), &["make"], 3, "t15-silent");
    assert_eq!(code, Some(141));
    assert!(
        err.is_empty(),
        "a closed reader must cost zero stderr bytes by default (ADR-011 class 2); got:\n{err}"
    );
}

// ============================================================================
// T16 — `dispatch::run_raw_passthrough`, the missed sibling surface (PF-006)
//
// A *third* buffered passthrough, serving the unknown-subcommand fallback plus
// `gh` (output-steering gate) and `yarn`.  It is NOT the `SKIM_PASSTHROUGH=1`
// escape hatch, which is why A2 correctly left it — and it carried both defects
// A2 fixed elsewhere: the 64 MiB TOTAL-LOSS hard error and the lossy UTF-8
// decode.  It is a pure byte passthrough (it returns only an `ExitCode`; no
// caller inspects the text), so nothing needed the complete buffer.
//
// `skim yarn build` is the probe: `build` is not one of yarn's compressed
// subcommands, so it takes the raw-passthrough arm.
//
// Surface: explicit subcommand path, reaching the handler through
// `cmd::dispatch`.  Neither front-end is covered: not the rewrite engine, and
// not the PATH-wrapper dispatch (no test sets `argv[0]`) — see the module header.
// ============================================================================

/// How long the T16 stub stalls between its two output lines.
const T16_STALL: Duration = Duration::from_secs(3);

/// First-N parity and exit 141 when the reader leaves mid-stream.
///
/// Exit 141 was already correct before this change — the `write!` propagated its
/// `BrokenPipe` to the `main.rs` boundary — so this is a regression guard for the
/// exit contract while the sink underneath it changes.  What is new is that the
/// bytes now arrive incrementally (see [`t16_raw_passthrough_reader_observes_the_producers_stall`])
/// and the child is killed rather than run to completion.
#[test]
fn t16_raw_passthrough_first_n_parity_and_exit_is_pipe_closed() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = grep_fixture(5_000);
    make_stub(dir.path(), "yarn", &fixture, "", 0);

    let (err_path, err_sink) = stderr_file(dir.path(), "t16-parity.err");
    let mut child = skim_with_stubs(dir.path(), &["yarn", "build"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(err_sink)
        .spawn()
        .unwrap();

    let got = read_n_lines_then_close(&mut child, 20);
    let status = wait_bounded(&mut child, "t16-parity");
    let err = std::fs::read_to_string(&err_path).unwrap();

    let expected: Vec<String> = fixture.lines().take(20).map(|l| format!("{l}\n")).collect();
    assert_eq!(
        got, expected,
        "the lines delivered before the reader closed must be raw's own first 20 lines"
    );
    assert_eq!(
        status.code(),
        Some(141),
        "a closed downstream reader must exit 141 (128 + SIGPIPE), never 1"
    );
    assert!(
        !err.contains("Broken pipe"),
        "`Error: Broken pipe` must never reach stderr; got:\n{err}"
    );
}

/// The reader observes the producer's own stall — proof that this sink streams.
///
/// Same construction and the same reason as `t2` / `t9`: the assertion is on the
/// **gap between two emitted lines**, never on an absolute deadline from
/// `spawn()`.  A debug binary under 4-way parallel `nextest` on macOS can take
/// ~1.5 s just to reach `main`, which flakes any absolute deadline tight enough
/// to discriminate.  The gap cancels start-up cost entirely: ~0 s buffered
/// versus [`T16_STALL`] streamed.
#[test]
fn t16_raw_passthrough_reader_observes_the_producers_stall() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("first.txt"), "yarn:1:first\n").unwrap();
    std::fs::write(dir.path().join("second.txt"), "yarn:2:second\n").unwrap();
    // `cat` rather than the shell's `echo` builtin: otherwise the *shell's* own
    // stdout buffering, not skim's, decides when bytes reach the pipe.
    write_stub_script(
        dir.path(),
        "yarn",
        &format!(
            "#!/bin/sh\ncat '{}'\nsleep {}\ncat '{}'\n",
            dir.path().join("first.txt").display(),
            T16_STALL.as_secs(),
            dir.path().join("second.txt").display()
        ),
    );

    let mut child = skim_with_stubs(dir.path(), &["yarn", "build"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(l) = line else { return };
            if tx.send((l, Instant::now())).is_err() {
                return;
            }
        }
    });

    let (first, t_first) = rx
        .recv_timeout(HANG_TIMEOUT)
        .expect("the first line never arrived through run_raw_passthrough");
    let (second, t_second) = rx
        .recv_timeout(HANG_TIMEOUT)
        .expect("the second line never arrived through run_raw_passthrough");
    wait_bounded(&mut child, "t16-ttfb");

    assert_eq!(first, "yarn:1:first");
    assert_eq!(second, "yarn:2:second");

    let gap = t_second.duration_since(t_first);
    assert!(
        gap >= T16_STALL / 2,
        "the two lines arrived {gap:?} apart but the producer stalled {T16_STALL:?} between \
         them — run_raw_passthrough buffered the whole stream instead of streaming it"
    );
}

/// Non-UTF-8 bytes survive `run_raw_passthrough` verbatim.
///
/// `runner::read_pipe` decodes with `String::from_utf8(..).unwrap_or_else(lossy)`,
/// so `0xFF 0xFE 0x80` reached the reader as U+FFFD sequences — skim showing
/// something *different* from raw with no marker (#317).  Measured pre-fix on
/// this exact payload: 39 bytes out for 33 bytes in.  The byte pump never
/// decodes.
#[test]
fn t16_raw_passthrough_passes_non_utf8_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let payload: &[u8] = b"a.rs:1:caf\xc3\xa9\ta.rs:2:\xff\xfe raw\x80bytes\n";
    make_stub_bytes(dir.path(), "yarn", payload, b"", 0);

    let out = skim_with_stubs(dir.path(), &["yarn", "build"])
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert_eq!(
        out.stdout, payload,
        "run_raw_passthrough must forward non-UTF-8 tool bytes verbatim, not as U+FFFD"
    );
}

/// The byte contract is exact: no trailing newline is invented.
///
/// The buffered form wrote `write!(out, "{}", output.stdout)` with no
/// trailing-newline guard; the streamed form must not acquire one (that is the
/// family sink's contract, not this one).
#[test]
fn t16_raw_passthrough_does_not_append_a_trailing_newline() {
    let dir = tempfile::tempdir().unwrap();
    let payload: &[u8] = b"yarn:1:no trailing newline";
    make_stub_bytes(dir.path(), "yarn", payload, b"", 0);

    let out = skim_with_stubs(dir.path(), &["yarn", "build"])
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert_eq!(
        out.stdout, payload,
        "run_raw_passthrough is byte-exact — it must not add a newline the tool never emitted"
    );
}

/// The child's exit code is forwarded unchanged.
///
/// `run_raw_passthrough` has its own disposition — the child's code,
/// `unwrap_or(1)` on a signal kill, clamped to `[0, 255]` — which is *not* the
/// file family's exit matrix.  Migrating the sink must not import a different one.
#[test]
fn t16_raw_passthrough_forwards_the_child_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    make_stub(dir.path(), "yarn", "built\n", "", 7);

    let out = skim_with_stubs(dir.path(), &["yarn", "build"])
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert_eq!(out.stdout, b"built\n");
    assert_eq!(
        out.status.code(),
        Some(7),
        "the child's own exit code must be forwarded unchanged"
    );
}

/// ~1 MiB interleaved on stdout AND stderr completes without deadlocking.
///
/// Streaming stdout on the calling thread reintroduces the pipe-full deadlock
/// that the two-reader-thread buffered runner made structurally impossible: with
/// nobody draining stderr the child blocks once that pipe fills, stops writing
/// stdout, and the stdout pump blocks forever (PF-021 / AD-STR-8).
///
/// [`wait_bounded`] is the explicit timeout: it polls `try_wait` against
/// [`HANG_TIMEOUT`] and **kills the child and panics** rather than blocking, so a
/// regression fails CI instead of hanging it.
#[test]
fn t16_raw_passthrough_interleaved_stdout_and_stderr_does_not_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let chunk_out: String = std::iter::repeat_n("o".repeat(63), 1024)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let chunk_err: String = std::iter::repeat_n("e".repeat(63), 1024)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(dir.path().join("chunk.out"), &chunk_out).unwrap();
    std::fs::write(dir.path().join("chunk.err"), &chunk_err).unwrap();
    write_stub_script(
        dir.path(),
        "yarn",
        &format!(
            "#!/bin/sh\ni=0\nwhile [ $i -lt 16 ]; do\n  cat '{}'\n  cat '{}' >&2\n  i=$((i+1))\ndone\n",
            dir.path().join("chunk.out").display(),
            dir.path().join("chunk.err").display()
        ),
    );

    let (out_path, out_sink) = stderr_file(dir.path(), "t16.out");
    let (err_path, err_sink) = stderr_file(dir.path(), "t16.err");
    let mut child = skim_with_stubs(dir.path(), &["yarn", "build"])
        .stdin(Stdio::null())
        .stdout(out_sink)
        .stderr(err_sink)
        .spawn()
        .unwrap();
    let status = wait_bounded(&mut child, "t16-deadlock");

    assert_eq!(status.code(), Some(0), "stub exits 0");
    assert_eq!(
        std::fs::metadata(&out_path).unwrap().len() as usize,
        chunk_out.len() * 16,
        "every stdout byte must reach the reader (#317: compress, never truncate)"
    );
    assert!(
        std::fs::metadata(&err_path).unwrap().len() as usize >= chunk_err.len() * 16,
        "child stderr is forwarded verbatim"
    );
}

/// `run_raw_passthrough` delivers output far past the buffered 64 MiB ceiling.
///
/// **The headline defect-2 fix.**  `runner::read_pipe` hard-errors at
/// `MAX_OUTPUT_BYTES` and throws the accumulated buffer away, so before this
/// change `skim yarn build` on a 70 MiB log produced
/// `Error: output exceeded 67108864 byte limit`, exit 1, and **zero bytes of
/// stdout** — measured on this exact stub, not theorised.  A byte pump has no
/// ceiling at all.
///
/// `#[ignore]` because it moves 70 MiB through a pipe; the always-run guard is
/// the pure-function `pump` test in `cmd::stream_pump`
/// (`test_pump_delivers_everything_past_a_buffered_style_ceiling`), which now
/// covers this call site too because both share `stream_child`.
/// Run with `cargo nextest run -p rskim --all-targets --run-ignored all`.
#[test]
#[ignore = "moves 70 MiB through a pipe; the pump unit test is the always-run guard"]
fn t16_raw_passthrough_delivers_past_the_buffered_ceiling() {
    const MIB: usize = 1024 * 1024;
    const EMITTED_MIB: usize = 70;

    let dir = tempfile::tempdir().unwrap();
    write_stub_script(
        dir.path(),
        "yarn",
        &format!(
            "#!/bin/sh\ndd if=/dev/zero bs={MIB} count={EMITTED_MIB} 2>/dev/null | tr '\\0' 'x'\n"
        ),
    );

    let (out_path, out_sink) = stderr_file(dir.path(), "t16-huge.out");
    let (err_path, err_sink) = stderr_file(dir.path(), "t16-huge.err");
    let mut child = skim_with_stubs(dir.path(), &["yarn", "build"])
        .stdin(Stdio::null())
        .stdout(out_sink)
        .stderr(err_sink)
        .spawn()
        .unwrap();
    let status = wait_bounded(&mut child, "t16-huge");

    let delivered = std::fs::metadata(&out_path).unwrap().len() as usize;
    let errs = std::fs::read_to_string(&err_path).unwrap();
    assert_eq!(
        delivered,
        EMITTED_MIB * MIB,
        "every byte must be DELIVERED past the old 64 MiB ceiling — the buffered \
         path discarded the whole buffer here and emitted nothing; stderr was: {errs:?}"
    );
    assert_eq!(status.code(), Some(0), "the stub exits 0");
    assert!(
        !errs.contains("byte limit"),
        "run_raw_passthrough must not hard-error at a byte ceiling; got: {errs:?}"
    );
}
