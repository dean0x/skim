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
//! drives the **explicit subcommand** path (`skim grep …`).  That is the same
//! `cmd::dispatch` front-end the **PATH-wrapper** surface reaches via
//! `detect_argv0_dispatch`, so wrapper coverage follows for the handler body —
//! but it is **not** the rewrite engine.  No test here exercises
//! `skim rewrite` / the PreToolUse hook text transformation, and none should be
//! cited as coverage of it.
//!
//! ## Anti-flake rules observed here
//!
//! - **Never race a live `grep`.**  Every test uses a stub on a prepended PATH
//!   that emits a fixture, so timing is a property of skim, not of the host
//!   filesystem.
//! - **Never shell out to `head`.**  `assert_cmd` buffers the whole child and
//!   cannot close a pipe early, so early-close tests use
//!   `std::process::Command` + `Stdio::piped()` and drop the handle after N
//!   lines.
//! - **Assert on wide margins and ordering, never on exact byte counts past the
//!   cut.**  How much skim managed to write before the reader vanished is a
//!   kernel-scheduling detail.

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
fn read_n_lines_then_close(child: &mut Child, count: usize) -> Vec<String> {
    let stdout = child.stdout.take().expect("stdout must be piped");
    let mut reader = BufReader::new(stdout);
    let mut lines = Vec::with_capacity(count);
    for _ in 0..count {
        let mut line = String::new();
        if reader.read_line(&mut line).expect("read_line") == 0 {
            break;
        }
        lines.push(line);
    }
    drop(reader);
    lines
}

/// Wait for `child`, failing the test rather than hanging CI if it never exits.
///
/// Without this bound a regression that reintroduces the stderr-drain deadlock
/// (PF-023 / AD-STR-8) would hang the test binary instead of failing it.
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
                     deadlock (PF-023 / AD-STR-8)"
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
    read_n_lines_then_close(&mut child, 20);
    wait_bounded(&mut child, "t3-quiet");
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
// T4 — deadlock guard: concurrent stderr drain (PF-023 / AD-STR-8)
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
    read_n_lines_then_close(&mut child, 20);
    wait_bounded(&mut child, "t6");

    // Well past the stub's 1 s stall: if the child survived, the sentinel exists.
    std::thread::sleep(Duration::from_millis(2_000));
    assert!(
        !sentinel.exists(),
        "the child kept running after the reader left — ChildGuard must kill it, \
         exactly as raw grep dies on SIGPIPE"
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
