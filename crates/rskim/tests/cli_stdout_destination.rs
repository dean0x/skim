//! Stdout-destination matrix: 9 destinations × 2 interception surfaces.
//!
//! Design constraint #317 says a wrapper may re-encode output but must never
//! show less than the raw tool. That obligation is destination-dependent: a
//! terminal or `| cat` wants compression (it is the whole point of skim), while
//! `| tee out.txt`, `$(…)` or a redirect onto a file wants the tool's exact
//! bytes. This file pins, end to end, which destinations get which.
//!
//! ## The two surfaces are NOT interchangeable
//!
//! Per CLAUDE.md ("Two interception surfaces"), a test that drives one surface
//! does not exercise the other. Every destination below is therefore measured
//! twice:
//!
//! - **Wrapper surface** — a hand-made `git` symlink to the skim binary on
//!   `PATH`. skim *is* the tool; `argv[0]` dispatch runs, `try_rewrite` never
//!   does. Decides from `fstat(fd 1)` + `isatty(1)`, plus the force-raw marker
//!   the hook leaves behind.
//! - **Rewrite surface** — `skim rewrite <cmd>` (the same text transformation
//!   the PreToolUse hook performs), whose emitted command is then executed with
//!   NO wrapper on `PATH`. This is the hook-only deployment.
//!
//! ## Where the surfaces agree, and the one structural divergence
//!
//! **`| cat` — agreement.** `<cmd> | cat` (bare `cat`, sole consumer) IS
//! rewritten: the source segment becomes `skim <cmd>` and `| cat` is preserved
//! verbatim, so the rewrite surface now serves the COMPRESSED view the wrapper
//! surface already served. `| cat` is a reader render — an agent defeating a
//! pager — and compressing what an agent is about to read is skim's purpose.
//! Every other pipe shape still makes the engine decline (#317 / AD-RW-2:
//! compressing a pipe producer changes what the downstream consumer sees), so
//! `| tee f`, `| cat -n`, `| cat > f`, a third stage and an interleaved
//! `&&`/`||`/`;` all pass through untouched.
//!
//! **Divergence: an AF_UNIX socket on fd 1 compresses on the rewrite surface.**
//! The parent installed that fd; no redirect syntax exists for the text scan to
//! see, and the explicit-subcommand path is deliberately not `fstat`-gated.
//! Only `fstat` can observe the socket, and it does — on the wrapper surface.
//!
//! It is inherent to what each surface can observe, not a defect to paper over.
//! See `stdout_should_serve_raw` in `main.rs` for the division of labour.
//!
//! ## Why `git log -n 5`
//!
//! `ls` is a pure passthrough in this build (byte-identical for `ls`, `ls -la`,
//! `ls -R`), so it cannot discriminate raw from compressed. `git log -n 5` can:
//! raw output starts `commit <40-hex>` and runs ~10 KB; the compressed view
//! starts `<7-hex> subject` and runs ~600 B. Raw is measured freshly in every
//! test rather than hardcoded, so history growth cannot rot the assertions.
//!
//! ## Sandboxing (PF-017)
//!
//! Every child runs against a `TempDir` `HOME` with all five agent config-dir
//! overrides plus `SKIM_CACHE_DIR` and `SKIM_WRAPPERS_DIR`. Wrapper symlinks are
//! created by hand — `skim init --wrappers` is never invoked, and a global
//! `skim init --uninstall` would remove hooks for every configured agent.

mod common;

#[cfg(unix)]
mod destination {
    use std::io::Read as _;
    use std::os::unix::io::{FromRawFd as _, OwnedFd};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    /// The command under test on every destination.
    const CMD: &str = "git log -n 5";

    // ========================================================================
    // Harness
    // ========================================================================

    fn skim_bin() -> PathBuf {
        if let Ok(path) = std::env::var("CARGO_BIN_EXE_skim") {
            return PathBuf::from(path);
        }
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.pop(); // crates/rskim → crates
        p.pop(); // crates → workspace root
        p.join("target").join("debug").join("skim")
    }

    /// Workspace root — `git log` needs a git worktree to read.
    fn repo_root() -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.pop();
        p.pop();
        p
    }

    /// What actually landed at the destination.
    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    enum Served {
        Raw,
        Compressed,
        Nothing,
    }

    /// Drop ANSI CSI sequences so classification survives a colourising git on
    /// a pty. Deliberately minimal — this is a test-local reader, not a
    /// re-implementation of `output::strip_escape_sequences`.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c != '\u{1b}' {
                out.push(c);
                continue;
            }
            // Consume `[ … <final byte 0x40-0x7E>` (or a 2-byte escape).
            match chars.next() {
                Some('[') => {
                    for f in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&f) {
                            break;
                        }
                    }
                }
                _ => continue,
            }
        }
        out
    }

    /// Raw `git log` output begins `commit <40-hex>`; skim's compressed view
    /// begins with a 7-hex short sha and a subject.
    fn classify(bytes: &[u8]) -> Served {
        let text = strip_ansi(&String::from_utf8_lossy(bytes));
        let head = text.trim_start();
        if head.is_empty() {
            Served::Nothing
        } else if head.starts_with("commit ") {
            Served::Raw
        } else {
            Served::Compressed
        }
    }

    /// One isolated sandbox: temp HOME, temp cache, hand-made wrapper symlinks.
    struct Sandbox {
        home: tempfile::TempDir,
    }

    impl Sandbox {
        fn new() -> Self {
            let home = tempfile::tempdir().expect("tempdir");
            let sb = Sandbox { home };
            std::fs::create_dir_all(sb.wrappers()).expect("wrappers dir");
            std::fs::create_dir_all(sb.cache()).expect("cache dir");
            std::fs::create_dir_all(sb.work()).expect("work dir");
            // PF-017: create the symlink by hand. Never shell out to
            // `skim init --wrappers`, which would touch the real ~/.skim/bin.
            std::os::unix::fs::symlink(skim_bin(), sb.wrappers().join("git"))
                .expect("wrapper symlink");
            sb
        }

        fn wrappers(&self) -> PathBuf {
            self.home.path().join(".skim").join("bin")
        }
        fn cache(&self) -> PathBuf {
            self.home.path().join(".cache").join("skim")
        }
        fn work(&self) -> PathBuf {
            self.home.path().join("work")
        }
        fn out(&self, name: &str) -> PathBuf {
            self.work().join(name)
        }

        /// Base env shared by every child: sandboxed HOME + all agent config-dir
        /// overrides, deterministic git, no analytics.
        fn base(&self, program: &str) -> Command {
            let home = self.home.path();
            let mut c = Command::new(program);
            c.current_dir(repo_root())
                .env("HOME", home)
                .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
                .env("GEMINI_CONFIG_DIR", home.join(".gemini"))
                .env("COPILOT_CONFIG_DIR", home.join(".copilot"))
                .env("CODEX_HOME", home.join(".codex"))
                .env("CRUSH_CONFIG_DIR", home.join(".crush"))
                .env("SKIM_CACHE_DIR", self.cache())
                .env("SKIM_WRAPPERS_DIR", self.wrappers())
                .env("SKIM_DISABLE_ANALYTICS", "1")
                .env("NO_COLOR", "1")
                // A raw `git log` on a pty would otherwise launch a pager and
                // hang the test instead of failing it.
                .env("GIT_PAGER", "cat")
                .env("TERM", "dumb")
                .env_remove("SKIM_PASSTHROUGH")
                .env_remove("SKIM_DEBUG")
                .env_remove("SKIM_REWRITTEN_FROM");
            c
        }

        /// `sh -c <script>` with the wrapper directory FIRST on PATH — the
        /// wrapper surface.
        fn wrapped_sh(&self, script: &str) -> Command {
            let mut c = self.base("sh");
            c.arg("-c").arg(script).env(
                "PATH",
                format!(
                    "{}:{}",
                    self.wrappers().display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
            c
        }

        /// `sh -c <script>` with NO wrapper on PATH, but with the skim binary's
        /// directory available so an emitted `skim …` resolves — the rewrite
        /// surface (hook-only deployment) and the raw baseline.
        fn bare_sh(&self, script: &str) -> Command {
            let bin_dir = skim_bin().parent().expect("skim bin dir").to_path_buf();
            let mut c = self.base("sh");
            c.arg("-c").arg(script).env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
            c
        }

        /// Fire the PreToolUse hook for `command` exactly as the agent would.
        ///
        /// The hook keys its force-raw marker to its own PPID — this test
        /// process — and the wrapper later finds it by walking `git → sh →
        /// test`, the same ancestry an agent → shell → tool chain produces.
        fn fire_hook(&self, command: &str) {
            self.fire_hook_with(command, &[]);
        }

        /// As [`Sandbox::fire_hook`], with extra env, returning the hook's
        /// stdout (the agent-protocol JSON response, or empty for passthrough).
        fn fire_hook_with(&self, command: &str, extra_env: &[(&str, &str)]) -> String {
            use std::io::Write as _;
            let payload = serde_json::json!({ "tool_input": { "command": command } });
            let mut cmd = self.base(skim_bin().to_str().unwrap());
            cmd.args(["rewrite", "--hook", "--agent", "claude-code"]);
            for (k, v) in extra_env {
                cmd.env(k, v);
            }
            let mut child = cmd
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn hook");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(payload.to_string().as_bytes())
                .expect("write hook stdin");
            let out = child.wait_with_output().expect("hook must exit");
            assert!(out.status.success(), "hook must exit 0");
            String::from_utf8_lossy(&out.stdout).into_owned()
        }

        /// The rewrite engine's verdict: `Some(rewritten)` or `None` (declined).
        fn rewrite_verdict(&self, command: &str) -> Option<String> {
            let out = self
                .base(skim_bin().to_str().unwrap())
                .args(["rewrite", command])
                .output()
                .expect("skim rewrite must run");
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.is_empty() || s == command {
                None
            } else {
                Some(s)
            }
        }

        /// Freshly measured raw bytes for `CMD` — never hardcoded.
        fn raw_baseline(&self) -> Vec<u8> {
            let out = self
                .bare_sh(CMD)
                .output()
                .expect("raw baseline git log must run");
            assert!(
                !out.stdout.is_empty(),
                "raw baseline must be non-empty; stderr={}",
                String::from_utf8_lossy(&out.stderr)
            );
            out.stdout
        }
    }

    /// Upper bound on any single drain. Generous relative to the ~1 s these
    /// commands take.
    const DRAIN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

    /// Read from `f` until end-of-stream, or until `writer_done` is set and the
    /// fd has gone quiet, or until [`DRAIN_DEADLINE`]. Returns the bytes and
    /// whether the deadline was hit.
    ///
    /// **End-of-stream is not a reliable signal across these fd types.** A pty
    /// master returns EIO on macOS once the last slave closes where Linux gives
    /// a clean EOF; and an AF_UNIX socketpair end is bidirectional, so reading
    /// our half can stay open indefinitely after the child has exited (measured:
    /// all 10716 bytes arrived, then the read simply never returned). Waiting on
    /// EOF alone therefore hangs. The authoritative signal is the child process
    /// exiting — once it has, no further bytes can arrive, so a quiet fd means
    /// done.
    ///
    /// `poll` slices also bound the whole operation: a fidelity test that HANGS
    /// is strictly worse than one that fails, because it yields no diagnosis and
    /// stalls the entire run.
    fn drain_until(
        mut f: std::fs::File,
        writer_done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> (Vec<u8>, bool) {
        use std::os::unix::io::AsRawFd as _;
        use std::sync::atomic::Ordering;

        let fd = f.as_raw_fd();
        let deadline = std::time::Instant::now() + DRAIN_DEADLINE;
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];

        loop {
            if std::time::Instant::now() >= deadline {
                return (out, true);
            }
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `pfd` is a valid single-element pollfd array owned by this
            // frame, and `fd` is kept alive by `f` for the whole call.
            let rc = unsafe { libc::poll(&mut pfd, 1, 100) };
            if rc < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return (out, false);
            }
            if rc == 0 {
                // Quiet slice. Done iff the writer has already exited.
                if writer_done.load(Ordering::SeqCst) {
                    return (out, false);
                }
                continue;
            }
            match f.read(&mut buf) {
                Ok(0) => return (out, false),
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                // EIO here is a pty master whose last slave just closed.
                Err(_) => return (out, false),
            }
        }
    }

    /// Run `cmd` with fd 1 wired to `sink`, draining `reader` concurrently so
    /// the child cannot block on a full pipe/socket/pty buffer, and return what
    /// was read.
    ///
    /// `reader` MUST be close-on-exec. A socketpair end is bidirectional, so a
    /// copy leaked into the child would keep our read direction writable. Every
    /// `reader` passed here comes from a CLOEXEC-by-default constructor or has
    /// `FD_CLOEXEC` set by its factory.
    ///
    /// The reader runs for the whole life of the child — stopping early would
    /// deadlock a child blocked writing into a full buffer — and is told to
    /// finish only once `wait()` has confirmed the writer is gone.
    fn run_with_stdout(mut cmd: Command, sink: OwnedFd, reader: std::fs::File) -> Vec<u8> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let mut child = cmd.stdout(Stdio::from(sink)).spawn().expect("spawn child");
        let done = Arc::new(AtomicBool::new(false));
        let handle = {
            let done = Arc::clone(&done);
            std::thread::spawn(move || drain_until(reader, done))
        };
        child.wait().expect("child must exit");
        done.store(true, Ordering::SeqCst);
        let (bytes, timed_out) = handle.join().expect("reader thread");
        assert!(
            !timed_out,
            "drain hit its {DRAIN_DEADLINE:?} deadline after {} bytes",
            bytes.len()
        );
        bytes
    }

    // ---- fd factories -------------------------------------------------------

    /// Set `FD_CLOEXEC` so this fd is not inherited by spawned children.
    fn set_cloexec(fd: std::os::unix::io::RawFd) {
        // SAFETY: fcntl with F_GETFD/F_SETFD on a valid owned fd; both results
        // are checked.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            assert!(flags >= 0, "F_GETFD failed");
            assert_eq!(
                libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC),
                0,
                "F_SETFD FD_CLOEXEC failed"
            );
        }
    }

    /// A pty pair: `(master, slave)`. The master is close-on-exec.
    fn open_pty() -> (std::fs::File, OwnedFd) {
        // SAFETY: each libc call below is checked for failure before its result
        // is used, and both returned fds are freshly owned by this process.
        unsafe {
            let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
            assert!(master >= 0, "posix_openpt failed");
            assert_eq!(libc::grantpt(master), 0, "grantpt failed");
            assert_eq!(libc::unlockpt(master), 0, "unlockpt failed");
            let name = libc::ptsname(master);
            assert!(!name.is_null(), "ptsname failed");
            let cname = std::ffi::CStr::from_ptr(name).to_owned();
            let slave = libc::open(cname.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
            assert!(slave >= 0, "open pty slave failed");
            set_cloexec(master);
            (
                std::fs::File::from_raw_fd(master),
                OwnedFd::from_raw_fd(slave),
            )
        }
    }

    /// A connected AF_UNIX stream pair: `(ours, theirs)`.
    ///
    /// `UnixStream::pair()` is used rather than a raw `libc::socketpair` because
    /// it sets `SOCK_CLOEXEC` on both ends — see the hang described on
    /// [`run_with_stdout`].
    fn open_socketpair() -> (std::fs::File, OwnedFd) {
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        (
            std::fs::File::from(OwnedFd::from(ours)),
            OwnedFd::from(theirs),
        )
    }

    fn make_fifo(path: &Path) {
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: mkfifo takes a NUL-terminated path and a mode; both are valid.
        assert_eq!(
            unsafe { libc::mkfifo(c.as_ptr(), 0o600) },
            0,
            "mkfifo failed"
        );
    }

    /// Run `script`, which writes into the named FIFO at `fifo`, and return
    /// what came out the other end.
    fn run_into_fifo(mut cmd: Command, fifo: PathBuf) -> Vec<u8> {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let done = Arc::new(AtomicBool::new(false));
        // Opening a FIFO for reading blocks until a writer arrives, so the
        // reader must start before (or alongside) the writer.
        let handle = {
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                let f = std::fs::File::open(&fifo).expect("open fifo for reading");
                drain_until(f, done)
            })
        };
        let status = cmd.status().expect("fifo writer must run");
        assert!(status.success(), "fifo writer exited {status:?}");
        done.store(true, Ordering::SeqCst);
        let (bytes, timed_out) = handle.join().expect("fifo reader thread");
        assert!(!timed_out, "fifo drain hit its deadline");
        bytes
    }

    // ========================================================================
    // Wrapper surface — one test per destination
    // ========================================================================

    /// TTY → compressed. A terminal is a live reader; this is skim's purpose.
    #[test]
    fn wrapper_tty_compresses() {
        let sb = Sandbox::new();
        sb.fire_hook(CMD);
        let (master, slave) = open_pty();
        let bytes = run_with_stdout(sb.wrapped_sh(CMD), slave, master);
        assert_eq!(
            classify(&bytes),
            Served::Compressed,
            "a pty on fd 1 must compress; got {:?}",
            String::from_utf8_lossy(&bytes)
                .chars()
                .take(80)
                .collect::<String>()
        );
    }

    /// `| cat` → compressed. THE case that must not regress: an agent piping
    /// skim's output somewhere to read it is the single most common shape, and
    /// a blanket "not a TTY and not a pipe → raw" rule was rejected for it.
    #[test]
    fn wrapper_pipe_cat_compresses() {
        let sb = Sandbox::new();
        sb.fire_hook("git log -n 5 | cat");
        let out = sb
            .wrapped_sh("git log -n 5 | cat")
            .output()
            .expect("run | cat");
        assert_eq!(
            classify(&out.stdout),
            Served::Compressed,
            "`| cat` must keep compressing — this is skim's core value"
        );
    }

    /// `| tee f` → raw. `fstat` sees the same FIFO as `| cat`; only the hook's
    /// force-raw marker can tell them apart.
    #[test]
    fn wrapper_pipe_tee_serves_raw() {
        let sb = Sandbox::new();
        let out_file = sb.out("tee.txt");
        let script = format!("git log -n 5 | tee {}", out_file.display());
        sb.fire_hook(&script);
        sb.wrapped_sh(&script).output().expect("run | tee");
        let landed = std::fs::read(&out_file).expect("tee output file");
        assert_eq!(
            classify(&landed),
            Served::Raw,
            "`| tee FILE` must land the tool's raw bytes in the file"
        );
    }

    /// `> out.txt` → raw. Ground truth: fd 1 is a regular file.
    #[test]
    fn wrapper_redirect_to_file_serves_raw() {
        let sb = Sandbox::new();
        let out_file = sb.out("redir.txt");
        let script = format!("git log -n 5 > {}", out_file.display());
        sb.fire_hook(&script);
        sb.wrapped_sh(&script).output().expect("run > file");
        let landed = std::fs::read(&out_file).expect("redirect output file");
        assert_eq!(classify(&landed), Served::Raw, "`> FILE` must serve raw");
    }

    /// `> /dev/null` → raw. **The char-device bug.** `/dev/null` is a character
    /// device but not a terminal; the gate used to treat every char device as a
    /// TTY and compress into it.
    ///
    /// `/dev/null` discards its input, so the observable is the ADR-011 class-2
    /// debug banner the gate emits when it chooses raw — zero bytes without
    /// `SKIM_DEBUG`, which the companion test below pins.
    #[test]
    fn wrapper_redirect_to_devnull_serves_raw() {
        let sb = Sandbox::new();
        let script = "git log -n 5 > /dev/null";
        sb.fire_hook(script);
        let out = sb
            .wrapped_sh(script)
            .env("SKIM_DEBUG", "1")
            .output()
            .expect("run > /dev/null");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("serving raw"),
            "/dev/null is a char device but NOT a terminal — the gate must \
             choose raw, not mistake it for a TTY; stderr={stderr}"
        );
    }

    /// ADR-011 class 2: choosing raw loses nothing, so the banner is
    /// debug-gated and costs zero stderr bytes by default.
    #[test]
    fn wrapper_raw_fallback_banner_is_debug_gated() {
        let sb = Sandbox::new();
        let out_file = sb.out("quiet.txt");
        let script = format!("git log -n 5 > {}", out_file.display());
        sb.fire_hook(&script);
        let out = sb.wrapped_sh(&script).output().expect("run > file");
        assert!(
            !String::from_utf8_lossy(&out.stderr).contains("serving raw"),
            "a no-loss raw fallback must emit NO stderr without SKIM_DEBUG \
             (ADR-011 class 2); got: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Named FIFO → raw. `fstat` reports a FIFO, identical to `| cat`; the
    /// redirect is visible only to the rewrite surface's text scan.
    #[test]
    fn wrapper_named_fifo_serves_raw() {
        let sb = Sandbox::new();
        let fifo = sb.out("named.fifo");
        make_fifo(&fifo);
        let script = format!("git log -n 5 > {}", fifo.display());
        sb.fire_hook(&script);
        let landed = run_into_fifo(sb.wrapped_sh(&script), fifo);
        assert_eq!(
            classify(&landed),
            Served::Raw,
            "a redirect onto a named FIFO must serve raw"
        );
    }

    /// AF_UNIX socket → raw. Neither a terminal nor a FIFO.
    #[test]
    fn wrapper_socket_serves_raw() {
        let sb = Sandbox::new();
        sb.fire_hook(CMD);
        let (ours, theirs) = open_socketpair();
        let bytes = run_with_stdout(sb.wrapped_sh(CMD), theirs, ours);
        assert_eq!(
            classify(&bytes),
            Served::Raw,
            "an AF_UNIX socket on fd 1 must serve raw"
        );
    }

    /// `2>f >&2` → raw. fd 1 is dup'd from a file-bound fd 2, so `fstat` sees a
    /// regular file and decides correctly without any marker.
    #[test]
    fn wrapper_stderr_file_then_dup_serves_raw() {
        let sb = Sandbox::new();
        let out_file = sb.out("dup.txt");
        let script = format!("git log -n 5 2>{} >&2", out_file.display());
        sb.fire_hook(&script);
        sb.wrapped_sh(&script).output().expect("run 2>f >&2");
        let landed = std::fs::read(&out_file).expect("dup output file");
        assert_eq!(
            classify(&landed),
            Served::Raw,
            "`2>f >&2` routes stdout onto f — must serve raw"
        );
    }

    /// `$(…)` → raw. Command substitution is a FIFO to `fstat`; only the hook
    /// can see that the shell is capturing the value.
    #[test]
    fn wrapper_command_substitution_serves_raw() {
        let sb = Sandbox::new();
        let cap = sb.out("cap.txt");
        let script = format!("o=$(git log -n 5); printf '%s' \"$o\" > {}", cap.display());
        sb.fire_hook(&script);
        sb.wrapped_sh(&script).output().expect("run $( )");
        let landed = std::fs::read(&cap).expect("capture file");
        assert_eq!(
            classify(&landed),
            Served::Raw,
            "`$(…)` captures stdout as a value — must serve raw"
        );
    }

    // ========================================================================
    // Byte-comparison proofs for the two measured data-loss cases
    // ========================================================================

    /// `| tee f` must write the RAW byte count, not the compressed one.
    ///
    /// Measured before the fix on this branch: 623 bytes landed where raw git
    /// wrote 10716. The assertion is against a freshly measured baseline rather
    /// than those literals, but it fails loudly if compressed bytes reach the
    /// file.
    #[test]
    fn pipe_tee_writes_raw_byte_count_not_compressed() {
        let sb = Sandbox::new();
        let raw = sb.raw_baseline();
        let out_file = sb.out("tee-bytes.txt");
        let script = format!("git log -n 5 | tee {}", out_file.display());
        sb.fire_hook(&script);
        sb.wrapped_sh(&script).output().expect("run | tee");
        let landed = std::fs::read(&out_file).expect("tee output file");
        assert_eq!(
            landed.len(),
            raw.len(),
            "`| tee FILE` must write exactly the raw byte count \
             (raw={} B, landed={} B). A compressed view here is silent data \
             loss in a file the user asked to capture.",
            raw.len(),
            landed.len()
        );
        assert_eq!(
            landed, raw,
            "`| tee FILE` bytes must equal raw byte-for-byte"
        );
    }

    /// `2>f >&2` must write the RAW byte count, on the command the rewrite
    /// engine actually emits.
    ///
    /// Measured before the fix: the engine emitted
    /// `skim git log -n 5 2>/tmp/x.txt >&2`, which put 623 compressed bytes in
    /// the file where raw git wrote 10716. `2>` reads as stderr-only and `>&2`
    /// as a harmless fd-dup, so neither token alone revealed the redirect.
    #[test]
    fn stderr_file_then_dup_writes_raw_byte_count_not_compressed() {
        let sb = Sandbox::new();
        let raw = sb.raw_baseline();
        let out_file = sb.out("dup-bytes.txt");
        let script = format!("git log -n 5 2>{} >&2", out_file.display());

        // Run what the engine emits — declining leaves the original command.
        let emitted = sb
            .rewrite_verdict(&script)
            .unwrap_or_else(|| script.clone());
        sb.bare_sh(&emitted).output().expect("run emitted command");

        let landed = std::fs::read(&out_file).expect("dup output file");
        assert_eq!(
            landed.len(),
            raw.len(),
            "`2>f >&2` must write exactly the raw byte count \
             (raw={} B, landed={} B); emitted command was: {emitted}",
            raw.len(),
            landed.len()
        );
    }

    // ========================================================================
    // Rewrite surface — the engine's verdict per destination
    // ========================================================================

    /// A bare command with no destination syntax is rewritten (and compresses).
    #[test]
    fn rewrite_bare_command_is_rewritten() {
        let sb = Sandbox::new();
        let verdict = sb.rewrite_verdict(CMD);
        assert_eq!(
            verdict.as_deref(),
            Some("skim git log -n 5"),
            "a bare command must still be rewritten"
        );
    }

    /// Every destination that needs exact bytes must make the engine DECLINE,
    /// so the raw tool runs untouched on a hook-only deployment.
    ///
    /// `2>f >&2` is the case this work fixed: it used to be rewritten.
    #[test]
    fn rewrite_declines_for_every_byte_exact_destination() {
        let sb = Sandbox::new();
        for script in [
            "git log -n 5 | tee out.txt",
            "git log -n 5 > out.txt",
            "git log -n 5 > /dev/null",
            "git log -n 5 > named.fifo",
            "git log -n 5 2>f >&2",
            "o=$(git log -n 5)",
        ] {
            assert_eq!(
                sb.rewrite_verdict(script),
                None,
                "rewrite engine must decline `{script}` — its destination \
                 needs the tool's exact bytes"
            );
        }
    }

    /// `| cat` → rewritten, and the emitted command compresses.
    ///
    /// The narrow AD-RW-2 reversal: bare `cat` as the sole pipe consumer is a
    /// reader render, so the engine rewrites the SOURCE stage and leaves
    /// `| cat` verbatim. Both surfaces now compress this shape — the wrapper
    /// via `fstat` (`wrapper_pipe_cat_compresses`), the rewrite engine via the
    /// `skim …` it emits.
    #[test]
    fn rewrite_pipe_cat_is_rewritten_and_compresses() {
        let sb = Sandbox::new();
        let script = "git log -n 5 | cat";
        let emitted = sb
            .rewrite_verdict(script)
            .expect("`<cmd> | cat` must be rewritten on the rewrite surface");
        assert!(
            emitted.contains("skim git log"),
            "the pipe SOURCE is the stage that gets rewritten; got `{emitted}`"
        );
        let out = sb.bare_sh(&emitted).output().expect("run emitted command");
        assert_eq!(
            classify(&out.stdout),
            Served::Compressed,
            "`{emitted}` must land the compressed view at the far end of the pipe"
        );
    }

    /// AF_UNIX socket on fd 1 → COMPRESSED on the rewrite surface.
    ///
    /// The documented divergence, pinned. The parent installed this fd, so no
    /// redirect syntax exists for the text scan to see; and the
    /// explicit-subcommand path is deliberately not `fstat`-gated, because it
    /// cannot tell a user-authored `skim …` from a hook-injected one. The
    /// wrapper surface, which sees the socket, serves raw
    /// (`wrapper_socket_serves_raw`).
    #[test]
    fn rewrite_socket_compresses_on_explicit_subcommand_path() {
        let sb = Sandbox::new();
        let (ours, theirs) = open_socketpair();
        let bytes = run_with_stdout(sb.bare_sh("skim git log -n 5"), theirs, ours);
        assert_eq!(
            classify(&bytes),
            Served::Compressed,
            "the explicit-subcommand path is not fstat-gated: a socket on fd 1 \
             still receives the compressed view"
        );
    }

    /// End-to-end on the rewrite surface (hook-only, no wrappers): every
    /// byte-exact destination receives raw bytes.
    #[test]
    fn rewrite_surface_e2e_serves_raw_for_byte_exact_destinations() {
        let sb = Sandbox::new();

        let redir = sb.out("rw-redir.txt");
        let dup = sb.out("rw-dup.txt");
        let cap = sb.out("rw-cap.txt");
        let tee = sb.out("rw-tee.txt");

        let cases: Vec<(&str, String, PathBuf)> = vec![
            (
                "redirect",
                format!("git log -n 5 > {}", redir.display()),
                redir.clone(),
            ),
            (
                "stderr-dup",
                format!("git log -n 5 2>{} >&2", dup.display()),
                dup.clone(),
            ),
            (
                "substitution",
                format!("o=$(git log -n 5); printf '%s' \"$o\" > {}", cap.display()),
                cap.clone(),
            ),
            (
                "pipe-tee",
                format!("git log -n 5 | tee {}", tee.display()),
                tee.clone(),
            ),
        ];

        for (name, script, sink) in cases {
            let emitted = sb
                .rewrite_verdict(&script)
                .unwrap_or_else(|| script.clone());
            sb.bare_sh(&emitted).output().expect("run emitted command");
            let landed = std::fs::read(&sink).unwrap_or_default();
            assert_eq!(
                classify(&landed),
                Served::Raw,
                "rewrite surface / {name}: emitted `{emitted}` must leave raw \
                 bytes at the destination"
            );
        }
    }

    // ========================================================================
    // The accepted limitation, pinned
    // ========================================================================

    /// **No hook → no marker → `fstat`-only behaviour.**
    ///
    /// The force-raw marker is written only by the PreToolUse hook. A wrapper
    /// invoked from a plain shell — `~/.skim/bin` on PATH but no hook installed
    /// — cannot know the far end of its pipe and still compresses `| tee f`.
    ///
    /// This is a real, accepted limitation, pinned so it cannot be quietly
    /// claimed as fixed. It is asserted as an explicit *inequality* to raw so
    /// the day it does get closed, this test fails and forces the docs to move.
    #[test]
    fn no_hook_means_fstat_only_behaviour() {
        let sb = Sandbox::new();
        let raw = sb.raw_baseline();
        let out_file = sb.out("nohook.txt");
        let script = format!("git log -n 5 | tee {}", out_file.display());

        // Deliberately NOT firing the hook.
        sb.wrapped_sh(&script).output().expect("run | tee");
        let landed = std::fs::read(&out_file).expect("tee output file");

        assert_ne!(
            landed.len(),
            raw.len(),
            "ACCEPTED LIMITATION changed: `| tee f` now serves raw without a \
             hook. If the marker gained another writer, update the limitation \
             text on `force_raw_requested` in main.rs and delete this test."
        );
        assert_eq!(
            classify(&landed),
            Served::Compressed,
            "without a hook the wrapper has only fstat, which sees a FIFO"
        );
    }

    /// The marker is CLEARED, not merely set: a byte-exact command followed by
    /// a reader command must go back to compressing.
    #[test]
    fn force_raw_marker_does_not_outlive_its_command() {
        let sb = Sandbox::new();
        let tee_file = sb.out("first-tee.txt");

        // Command 1 sets the marker.
        let first = format!("git log -n 5 | tee {}", tee_file.display());
        sb.fire_hook(&first);
        sb.wrapped_sh(&first).output().expect("run | tee");
        assert_eq!(
            classify(&std::fs::read(&tee_file).unwrap()),
            Served::Raw,
            "precondition: the first command must serve raw"
        );

        // Command 2 must clear it.
        sb.fire_hook("git log -n 5 | cat");
        let out = sb
            .wrapped_sh("git log -n 5 | cat")
            .output()
            .expect("run | cat");
        assert_eq!(
            classify(&out.stdout),
            Served::Compressed,
            "a stale marker must not disable compression for the next command"
        );
    }

    // ========================================================================
    // The marker's scope: PPID is not a command identity
    // ========================================================================

    /// **A marker set for one command must not decide an unrelated one.**
    ///
    /// The marker is keyed by a PID in the wrapper's *ancestry*, and every
    /// command an agent runs shares that ancestor. So a wrapper invocation the
    /// hook never saw — inside a shell script, a `&` background job, a
    /// `Makefile` recipe, or a nested sub-agent that bypasses PreToolUse hooks
    /// (which is exactly why `--wrappers` exists) — walks up into the marker a
    /// *previous, different* command left behind.
    ///
    /// Here the hook only ever sees `cargo build | tee f`. The `git log | cat`
    /// below is a different command, and `| cat` is the shape whose compression
    /// is skim's core value. Before the marker was scoped to the command's own
    /// heads this served raw.
    ///
    /// Failure direction is compression, not bytes — but it is a real
    /// production exposure, not a test-parallelism artifact.
    #[test]
    fn marker_does_not_leak_to_an_unrelated_tool() {
        let sb = Sandbox::new();
        let tee_file = sb.out("other-command.txt");

        // The only command the hook sees. It is byte-exact, and it does not
        // name `git`.
        sb.fire_hook(&format!("cargo build | tee {}", tee_file.display()));

        // A different command, never seen by the hook, sharing an ancestor PID.
        let out = sb
            .wrapped_sh("git log -n 5 | cat")
            .output()
            .expect("run | cat");

        assert_eq!(
            classify(&out.stdout),
            Served::Compressed,
            "`git log | cat` must compress: the live marker belongs to \
             `cargo build | tee f`, a different command"
        );
    }

    /// **An unrelated command's hook must not delete a live marker.**
    ///
    /// Models two Bash tool calls in one agent turn: both hooks fire against the
    /// same agent PID before either command execs. With a PPID-only key the
    /// second hook cleared the first's marker, and the `tee` captured compressed
    /// bytes — a byte-fidelity loss (#317), the direction that actually costs
    /// the user data.
    #[test]
    fn concurrent_command_hook_does_not_clear_a_live_marker() {
        let sb = Sandbox::new();
        let tee_file = sb.out("concurrent-tee.txt");
        let script = format!("git log -n 5 | tee {}", tee_file.display());

        // Call 1's hook sets the marker …
        sb.fire_hook(&script);
        // … call 2's hook fires before call 1's shell execs `git`.
        sb.fire_hook("cargo build");

        sb.wrapped_sh(&script).output().expect("run | tee");

        assert_eq!(
            classify(&std::fs::read(&tee_file).expect("tee output file")),
            Served::Raw,
            "an unrelated concurrent command must not clear this command's \
             marker — the tee would capture compressed bytes"
        );
    }

    // ========================================================================
    // Regression pins
    // ========================================================================

    /// `SKIM_PASSTHROUGH=1` still bypasses compression on the WRAPPER surface.
    #[test]
    fn passthrough_env_still_serves_raw_on_wrapper_surface() {
        let sb = Sandbox::new();
        let out = sb
            .wrapped_sh("git log -n 5 | cat")
            .env("SKIM_PASSTHROUGH", "1")
            .output()
            .expect("run with passthrough");
        assert_eq!(
            classify(&out.stdout),
            Served::Raw,
            "SKIM_PASSTHROUGH=1 must still bypass compression on the wrapper surface"
        );
    }

    /// `SKIM_PASSTHROUGH=1` still suppresses interception on the REWRITE
    /// surface — i.e. in hook mode, where the interception actually happens.
    ///
    /// Asserted against `--hook` and not the `skim rewrite <cmd>` CLI: that CLI
    /// is a *classifier* (it answers "would this be rewritten?") and reports the
    /// rewrite regardless of the escape hatch. `run_hook_mode` is the gate.
    #[test]
    fn passthrough_env_still_suppresses_rewrite_surface() {
        let sb = Sandbox::new();

        // Precondition: without the hatch the hook DOES emit a rewrite, so an
        // empty response below means the hatch worked, not that nothing matched.
        let normal = sb.fire_hook_with(CMD, &[]);
        assert!(
            normal.contains("skim git log"),
            "precondition: the hook must rewrite this command; got: {normal}"
        );

        let gated = sb.fire_hook_with(CMD, &[("SKIM_PASSTHROUGH", "1")]);
        assert!(
            gated.trim().is_empty(),
            "SKIM_PASSTHROUGH=1 must suppress the hook response entirely; got: {gated}"
        );
    }

    /// The force-raw marker must not be written while `SKIM_PASSTHROUGH=1` short-
    /// circuits the hook — and it does not, because the gate returns before any
    /// sidecar write. Pinned so the marker cannot later be hoisted above it.
    #[test]
    fn passthrough_env_leaves_no_force_raw_marker() {
        let sb = Sandbox::new();
        let out_file = sb.out("pt.txt");
        let script = format!("git log -n 5 | tee {}", out_file.display());
        sb.fire_hook_with(&script, &[("SKIM_PASSTHROUGH", "1")]);

        let sessions = sb.cache().join("sessions");
        let markers: Vec<_> = std::fs::read_dir(&sessions)
            .map(|d| {
                d.flatten()
                    .filter(|e| e.path().extension().is_some_and(|x| x == "raw"))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            markers.is_empty(),
            "a short-circuited hook must not leave a force-raw marker: {markers:?}"
        );
    }

    // ========================================================================
    // Bail-shape fixes (reliability-2): heredoc, ${VAR}, unmatched quote
    // ========================================================================
    //
    // Three pipeline shapes where `split_compound` bails before reaching the
    // `|`, causing `pipe_consumer_needs_exact_bytes` to return `false` and the
    // hook to write no marker.  Without a marker the wrapper has only `fstat`,
    // which sees a FIFO and compresses into the tee file — silent byte loss.
    //
    // The fix: `Bail => cmd.contains('|')` — conservative when a pipe is
    // plausibly present.  Each test fires the hook with the bail-triggering form
    // and runs a valid equivalent; the wildcard marker (keyed by PPID, written
    // when command_heads returns an empty set) is found by the wrapper's ancestry
    // walk.  Byte comparison to the raw baseline distinguishes raw from compressed.
    //
    // Each test is written to FAIL before the fix (compressed bytes != raw bytes)
    // and PASS after (compressed bytes == raw bytes).

    /// `<cmd> <<EOF | tee f` (heredoc bail) → marker written → raw bytes in file.
    ///
    /// `check_bail` fires on `<<` before `split_compound` reaches the `|`, so
    /// the old code wrote no marker and the wrapper compressed.  The hook now
    /// returns true conservatively (contains `|`), causing the wildcard marker
    /// to be written.
    #[test]
    fn wrapper_pipe_tee_heredoc_bail_serves_raw() {
        let sb = Sandbox::new();
        let raw = sb.raw_baseline();
        let out_file = sb.out("heredoc-bail-tee.txt");

        // Fire the hook with the heredoc bail shape.  `split_compound` bails on
        // `<<`; after the fix `pipe_consumer_needs_exact_bytes` returns true
        // (contains `|`) and the hook writes a wildcard force-raw marker.
        let hook_cmd = format!("git log -n 5 <<EOF | tee {}", out_file.display());
        sb.fire_hook(&hook_cmd);

        // Verify the marker was written.
        let sessions = sb.cache().join("sessions");
        let markers: Vec<_> = std::fs::read_dir(&sessions)
            .map(|d| {
                d.flatten()
                    .filter(|e| e.path().extension().is_some_and(|x| x == "raw"))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !markers.is_empty(),
            "heredoc bail shape with `|` must write a force-raw marker; \
             got no .raw files under {sessions:?}"
        );

        // Run a valid equivalent command.  The wildcard marker (PPID-keyed) is
        // found by the ancestry walk, and the wrapper serves raw.
        let run_script = format!("git log -n 5 | tee {}", out_file.display());
        sb.wrapped_sh(&run_script).output().expect("run | tee");
        let landed = std::fs::read(&out_file).expect("tee output");
        assert_eq!(
            landed.len(),
            raw.len(),
            "heredoc bail: tee file must contain raw bytes \
             (raw={} B, landed={} B — compressed bytes signal a missing marker)",
            raw.len(),
            landed.len()
        );
        assert_eq!(landed, raw, "heredoc bail: bytes must match raw byte-for-byte");
    }

    /// `<cmd> | tee f` with unquoted `${VAR}` → marker written → raw bytes.
    ///
    /// `check_bail` fires on `${` before the `|`, so the old code wrote no
    /// marker.  `${VAR}` is NOT covered by rule S (`is_capture_shape`).
    #[test]
    fn wrapper_pipe_tee_unquoted_var_bail_serves_raw() {
        let sb = Sandbox::new();
        let raw = sb.raw_baseline();
        let out_file = sb.out("var-bail-tee.txt");

        // Hook sees the literal text including `${N}` — split_compound bails.
        let hook_cmd = format!("git log -n ${{N}} | tee {}", out_file.display());
        sb.fire_hook(&hook_cmd);

        // Verify the marker was written.
        let sessions = sb.cache().join("sessions");
        let markers: Vec<_> = std::fs::read_dir(&sessions)
            .map(|d| {
                d.flatten()
                    .filter(|e| e.path().extension().is_some_and(|x| x == "raw"))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !markers.is_empty(),
            "unquoted `${{N}}` bail with `|` must write a force-raw marker"
        );

        // Run with N expanded to 5.  The wildcard marker routes the wrapper to raw.
        let run_script = format!("N=5; git log -n $N | tee {}", out_file.display());
        sb.wrapped_sh(&run_script).output().expect("run | tee");
        let landed = std::fs::read(&out_file).expect("tee output");
        assert_eq!(
            landed.len(),
            raw.len(),
            "unquoted `${{N}}` bail: tee file must contain raw bytes \
             (raw={} B, landed={} B)",
            raw.len(),
            landed.len()
        );
        assert_eq!(landed, raw, "unquoted `${{N}}` bail: bytes must match raw");
    }

    /// `<cmd> | tee f` with an unmatched quote → marker written → raw bytes.
    ///
    /// `split_compound` bails at end-of-input when a quote is left open, before
    /// the consumer was identified.  Old code: no marker, compressed bytes.
    #[test]
    fn wrapper_pipe_tee_unmatched_quote_bail_serves_raw() {
        let sb = Sandbox::new();
        let raw = sb.raw_baseline();
        let out_file = sb.out("quote-bail-tee.txt");

        // Single-quote opens but never closes — split_compound bails at EOF.
        // The command contains `|`, so the fix returns true and the hook writes
        // a wildcard marker.
        let hook_cmd = format!("git log -n 5 | tee '{}", out_file.display());
        sb.fire_hook(&hook_cmd);

        // Verify the marker was written.
        let sessions = sb.cache().join("sessions");
        let markers: Vec<_> = std::fs::read_dir(&sessions)
            .map(|d| {
                d.flatten()
                    .filter(|e| e.path().extension().is_some_and(|x| x == "raw"))
                    .map(|e| e.path())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !markers.is_empty(),
            "unmatched-quote bail with `|` must write a force-raw marker"
        );

        // Run the valid equivalent; marker routes the wrapper to raw.
        let run_script = format!("git log -n 5 | tee {}", out_file.display());
        sb.wrapped_sh(&run_script).output().expect("run | tee");
        let landed = std::fs::read(&out_file).expect("tee output");
        assert_eq!(
            landed.len(),
            raw.len(),
            "unmatched-quote bail: tee file must contain raw bytes \
             (raw={} B, landed={} B)",
            raw.len(),
            landed.len()
        );
        assert_eq!(landed, raw, "unmatched-quote bail: bytes must match raw");
    }

    /// **Negative cell**: quoted `"${N}"` takes the full parsing path (no bail),
    /// so `cat` as the consumer is correctly identified as non-byte-exact and no
    /// marker is written.  The wrapper compresses normally.
    ///
    /// This confirms the fix does not over-broaden: a properly-quoted variable
    /// still keeps compression working for reader consumers.
    #[test]
    fn wrapper_pipe_cat_quoted_var_still_compresses() {
        let sb = Sandbox::new();

        // `"${N}"` is inside double quotes → check_bail is skipped → split_compound
        // returns Compound.  `cat` is not in BYTE_EXACT_PIPE_CONSUMERS → false →
        // no force-raw marker.
        let hook_cmd = r#"git log -n "${N}" | cat"#;
        sb.fire_hook(hook_cmd);

        // Run with N=5; the shell expands "${N}" to 5.
        let out = sb
            .wrapped_sh(r#"N=5; git log -n "${N}" | cat"#)
            .output()
            .expect(r#"run | cat with quoted "${N}""#);
        assert_eq!(
            classify(&out.stdout),
            Served::Compressed,
            r#"quoted `"${{N}}"` with `| cat` must still compress — \
             the fix must not over-broaden to properly-parsed commands"#
        );
    }

    /// ADR-009: `grep` stays byte-for-byte identical to the raw tool on the
    /// wrapper surface. The destination gate must not have perturbed it.
    #[test]
    fn grep_remains_byte_for_byte_passthrough() {
        let sb = Sandbox::new();
        std::os::unix::fs::symlink(skim_bin(), sb.wrappers().join("grep"))
            .expect("grep wrapper symlink");

        let script = "grep -rn 'compress, never truncate' CLAUDE.md";
        let wrapped = sb.wrapped_sh(script).output().expect("wrapped grep");
        let bare = sb.bare_sh(script).output().expect("bare grep");

        assert_eq!(
            wrapped.stdout, bare.stdout,
            "grep must be byte-for-byte identical to the raw tool (ADR-009)"
        );
        assert!(!bare.stdout.is_empty(), "fixture must actually match");
    }
}
