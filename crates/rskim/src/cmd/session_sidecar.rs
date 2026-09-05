//! Session ID sidecar: PID-keyed files for fallback session attribution.
//!
//! ## Problem
//!
//! ~50% of skim analytics records are "untagged" (NULL `session_id`) because
//! direct skim invocations (e.g., `skim cargo test`) bypass the rewrite hook
//! that previously injected `--session-id=<value>`.
//!
//! ## Solution (AD-SC-1) — sidecar is now the PRIMARY attribution path
//!
//! The hook no longer injects `--session-id` into the rewritten command text
//! (#1.1 / fix/rewrite-compression-batch). Injecting the flag caused version-
//! skew hard-failures ("unexpected argument --session-id") on older binaries.
//! The sidecar is the canonical out-of-band attribution channel.
//!
//! **Write path** — On every hook invocation that carries a `session_id`, the
//! hook writes the value to `~/.cache/skim/sessions/{ppid}.id` (keyed by the
//! agent/shell PID, i.e., the parent of the hook process).
//!
//! **Read path** — Any skim invocation walks its process ancestry (up to
//! [`MAX_ANCESTRY_DEPTH`] levels) looking for a matching sidecar file.
//! The first fresh file found wins. Resolution priority in `main()`:
//! `sidecar > SKIM_SESSION_ID env var > --session-id flag (compat fallback)`.
//!
//! ## Security
//!
//! - Files are written with `0o600` permissions (owner-only).
//! - Content is validated through [`crate::analytics::is_safe_session_id`]
//!   before being accepted (alphanumeric + `-_.`, max 128 chars).
//! - All write and read failures are silently ignored — this is a best-effort
//!   mechanism that must never break the main skim pipeline.
//!
//! ## Performance
//!
//! Write path: ≤1 ms. Read path: ≤2 ms. Both are fire-and-forget / early-exit.

use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// Maximum age of a sidecar file before it is considered stale.
///
/// A sidecar older than 6 hours is skipped during ancestry walk. This covers
/// typical agent session lengths while preventing stale data from long-lived
/// shell processes.
const SIDECAR_MAX_AGE: Duration = Duration::from_secs(6 * 3600);

/// Maximum age before a sidecar file is removed during opportunistic cleanup.
const CLEANUP_MAX_AGE: Duration = Duration::from_secs(24 * 3600);

/// Subdirectory of the skim cache that holds sidecar files.
const SESSIONS_DIR: &str = "sessions";

/// Maximum number of ancestry levels to walk when searching for a sidecar.
///
/// Walking 5 levels covers agent → shell → hook → skim invocations in practice.
const MAX_ANCESTRY_DEPTH: usize = 5;

/// File-name suffix for the force-raw marker (see [`set_force_raw`]).
const FORCE_RAW_SUFFIX: &str = "raw";

/// Maximum age of a force-raw marker before it is ignored.
///
/// Much shorter than [`SIDECAR_MAX_AGE`]: the marker describes ONE command, and
/// the hook rewrites it before each command, so anything older than a few
/// minutes is a leftover from a crashed or hook-less invocation. Erring long
/// costs compression (lossless); erring short costs fidelity — so the bound is
/// generous enough to cover a slow command but far below a session lifetime.
const FORCE_RAW_MAX_AGE: Duration = Duration::from_secs(300);

// ============================================================================
// Public API
// ============================================================================

/// Write `session_id` to a PID-keyed sidecar file for fallback attribution.
///
/// The file is keyed by **PPID** (the caller's parent process ID) so that
/// sibling processes spawned by the same agent parent can later discover the
/// session via the ancestry walk in [`read_session_id`].
///
/// All failures are silently ignored — this is a fire-and-forget operation.
/// Callers should validate `session_id` through
/// [`crate::analytics::is_safe_session_id`] before calling this function.
pub(crate) fn write_session_id(session_id: &str, cache_dir: &Path) {
    // Defense-in-depth: reject malformed IDs even though callers should
    // have validated already.
    if !crate::analytics::is_safe_session_id(session_id) {
        return;
    }

    let Some(ppid) = get_ppid() else { return };

    let dir = cache_dir.join(SESSIONS_DIR);
    let _ = std::fs::create_dir_all(&dir);

    let file_path = dir.join(format!("{ppid}.id"));

    // On Unix, open with O_CREAT|O_WRONLY|O_TRUNC and mode 0o600 in a single
    // syscall so the file is never briefly world-readable (eliminates the
    // TOCTOU window that exists with fs::write followed by set_permissions).
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&file_path)
        {
            let _ = f.write_all(session_id.as_bytes());
        }
    }

    // On non-Unix platforms get_ppid() always returns None, so this branch
    // is unreachable in practice. It exists to keep the code compiling on
    // Windows without dead-code warnings.
    #[cfg(not(unix))]
    {
        let _ = std::fs::write(&file_path, session_id);
    }

    // Opportunistic cleanup — best-effort, errors ignored.
    cleanup_stale_rate_limited(&dir);
}

/// Walk process ancestry to find a session ID sidecar.
///
/// Starts from the current process PID and walks up to
/// [`MAX_ANCESTRY_DEPTH`] levels looking for a file at
/// `{cache_dir}/sessions/{pid}.id`. Returns the first fresh, valid session ID
/// found, or `None` if no matching file exists.
///
/// "Fresh" means the file's mtime is within [`SIDECAR_MAX_AGE`].
/// "Valid" means the content passes [`crate::analytics::is_safe_session_id`].
pub(crate) fn read_session_id(cache_dir: &Path) -> Option<String> {
    let sessions_dir = cache_dir.join(SESSIONS_DIR);
    let mut pid = std::process::id();

    for _ in 0..MAX_ANCESTRY_DEPTH {
        let file_path = sessions_dir.join(format!("{pid}.id"));

        if let Some(value) = try_read_sidecar(&file_path) {
            return Some(value);
        }

        pid = parent_of(pid)?;
    }

    None
}

// ============================================================================
// Force-raw marker (cross-surface fidelity parity)
// ============================================================================

/// Maximum length of a tool name embedded in a marker file name.
const MAX_MARKER_TOOL_LEN: usize = 64;

/// Return `true` when `tool` is safe to embed in a marker file name.
///
/// The command string is untrusted hook input (it arrives as JSON on stdin), so
/// a head like `../../../etc/cron.d/x` must never reach `Path::join`. Parse at
/// the boundary: a name that does not survive this check simply gets no
/// per-tool marker, and [`set_force_raw`] falls back to the wildcard.
///
/// Conservative basename alphabet, and the first byte may not be `.` — that
/// alone excludes `.`, `..` and hidden files without a special case.
///
/// Also called by `cmd::rewrite::compound::command_heads`, which must know
/// whether a head it extracted is *representable* before deciding the tool set
/// is knowable. Both sides share this one definition on purpose: if the
/// producer and the file-name contract could disagree, a head would be silently
/// dropped here and its tool would go unmarked — a byte loss.
pub(crate) fn is_safe_marker_tool(tool: &str) -> bool {
    if tool.len() > MAX_MARKER_TOOL_LEN {
        return false;
    }
    let mut bytes = tool.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_alphanumeric() || first == b'_') {
        return false;
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+' | b'.'))
}

/// Path of the force-raw marker for `pid`.
///
/// `Some(tool)` yields the per-tool marker `{pid}.{tool}.raw`; `None` yields the
/// wildcard marker `{pid}.raw`, which matches every tool.
fn marker_path(sessions_dir: &Path, pid: u32, tool: Option<&str>) -> std::path::PathBuf {
    match tool {
        Some(t) => sessions_dir.join(format!("{pid}.{t}.{FORCE_RAW_SUFFIX}")),
        None => sessions_dir.join(format!("{pid}.{FORCE_RAW_SUFFIX}")),
    }
}

/// Write (`present == true`) or remove (`present == false`) a marker file.
///
/// Write failures are logged to `hook.log`; all operations are non-fatal.
/// See [`set_force_raw`] for the cost model.
fn write_or_remove_marker(path: &Path, present: bool) {
    write_or_remove_marker_with_log(
        path,
        present,
        &crate::cmd::hook_log::CacheEnv::from_process(),
    );
}

/// Inner implementation of [`write_or_remove_marker`] with injected log env.
///
/// Separated so tests can supply an isolated cache directory for `hook.log`
/// without mutating process-global env vars.
///
/// On Unix the marker file is opened with `O_NOFOLLOW` so a symlink planted
/// by an attacker inside the sessions directory cannot redirect the write to
/// an arbitrary file. `remove_file` is intentionally *not* guarded: it
/// unlinks the directory entry (the symlink itself), not the symlink target,
/// so no follow occurs.
fn write_or_remove_marker_with_log(
    path: &Path,
    present: bool,
    log_env: &crate::cmd::hook_log::CacheEnv,
) {
    if !present {
        let _ = std::fs::remove_file(path);
        return;
    }

    // Mode 0o600 in the same syscall as create, matching write_session_id: the
    // file is never briefly world-readable. O_NOFOLLOW rejects a symlink on
    // the final path component — TOCTOU protection for the file itself
    // (the directory-level check in `set_force_raw` is the outer layer).
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
        {
            Ok(mut f) => {
                if let Err(e) = f.write_all(b"1") {
                    crate::cmd::hook_log::log_hook_warning_with_env(
                        &format!(
                            "force-raw: marker write failed for {:?}: {e} \
                             — a byte-exact pipe consumer (| tee, | sha256sum) \
                             may receive compressed output (#514)",
                            path
                        ),
                        log_env,
                    );
                }
            }
            Err(e) => {
                crate::cmd::hook_log::log_hook_warning_with_env(
                    &format!(
                        "force-raw: marker open failed for {:?}: {e} \
                         — a byte-exact pipe consumer (| tee, | sha256sum) \
                         may receive compressed output (#514)",
                        path
                    ),
                    log_env,
                );
            }
        }
    }

    #[cfg(not(unix))]
    {
        let _ = std::fs::write(path, b"1");
    }
}

/// Record — or clear — the rewrite surface's "this command's stdout needs
/// byte-exact output" verdict, scoped to the tools the command names.
///
/// # Why a sidecar rather than the command text
///
/// The wrapper surface (`~/.skim/bin/git`) and the rewrite surface (PreToolUse
/// hook) are different processes with no shared channel, and only the rewrite
/// surface can see pipeline shape: `| cat` and `| tee out.txt` both present the
/// wrapper with an indistinguishable FIFO on fd 1. The verdict therefore has to
/// travel out of band.
///
/// The alternative — prefixing an env assignment onto the emitted command — was
/// rejected twice over. It shifts the command into a parallel text namespace
/// that host permission matchers no longer match (PF-010: pre-approved commands
/// re-prompt, and hard-deny in headless sub-agents), and it would have to be
/// applied to commands the engine otherwise declines to touch, where prepending
/// text is not semantics-preserving for compound shapes (`X=1 a && b` scopes the
/// assignment to `a` alone). This mirrors the session-id channel instead
/// (ADR-004 / AD-SC-1): out-of-band, command text untouched.
///
/// # Keying: PPID **and** tool name
///
/// PPID alone is not a command identity. Every command an agent runs — in
/// parallel, in a background job, in a nested hook-less sub-agent — shares that
/// one PID, so a PPID-only marker is shared mutable state across unrelated
/// commands: one command's verdict silently decided another's, in both
/// directions (see the module tests and `force_raw_requested` in `main.rs`).
///
/// `tools` narrows the key to the command heads the hook actually saw, so
/// `git log | tee f` writes `{ppid}.git.raw` + `{ppid}.tee.raw` and leaves a
/// concurrent `cargo build`'s wrapper untouched. One hook invocation legitimately
/// produces several wrapper invocations (`a | b` with both wrapped), so markers
/// are *never* consumed on read — every reader sees the same file.
///
/// An **empty** `tools` means the command's shape defeated head extraction
/// (`$(…)`, backticks, process substitution). That falls back to the wildcard
/// marker `{ppid}.raw`, which matches every tool — erring wide costs
/// compression, erring narrow costs bytes.
///
/// # Lifetime
///
/// Called on EVERY hook invocation with the verdict for that command: `true`
/// writes the markers for `tools`, `false` removes them, so a marker does not
/// outlive the command that set it. [`FORCE_RAW_MAX_AGE`] bounds the leftovers
/// of a hook that crashed before it could clear.
///
/// All failures are silently ignored because a hook must never break the
/// pipeline. However, a marker that fails to write costs **bytes** for any
/// byte-exact pipe consumer — measured 304 bytes instead of 6803 into
/// `| tee f` (#514) — not merely lost compression. A failed `create_dir_all`
/// or marker open puts the system in the same missing-marker state as the
/// accepted same-tool key collision documented below; failures are logged to
/// `hook.log` so the cost is visible without breaking the pipeline.
///
/// # Accepted residual: same-tool key collision
///
/// Because the key is `{ppid}.{tool}.raw`, **two commands using the same
/// tool under one agent share a key**. `set_force_raw` is called on every
/// hook invocation and a `false` verdict calls [`write_or_remove_marker`]
/// to *remove* the marker. So a concurrent `git status` (verdict `false`)
/// deletes the live marker set by `git log -n 5 | tee out.txt` (verdict
/// `true`): the `git` wrapper then sees only a FIFO via `fstat` and
/// compresses into the tee. This costs bytes, not merely compression.
///
/// # Accepted residual: PID reuse
///
/// A PID recycled inside the [`FORCE_RAW_MAX_AGE`] window (300 s) makes the
/// wrapper for the new process find a stale marker and serve raw instead of
/// compressing. Unlike the same-tool key collision, this fails toward
/// **lossless** compression (extra raw bytes are served, none are lost), so
/// it needs no code change — noted here for completeness alongside the two
/// limitations that do cost bytes.
pub(crate) fn set_force_raw(force_raw: bool, tools: &[String], cache_dir: &Path) {
    let Some(ppid) = get_ppid() else { return };

    let dir = cache_dir.join(SESSIONS_DIR);

    let safe: Vec<&str> = tools
        .iter()
        .map(String::as_str)
        .filter(|t| is_safe_marker_tool(t))
        .collect();

    // The wildcard is written only when there is no usable tool set: a
    // per-tool marker is always the narrower, preferred encoding of the same
    // verdict. Any other case removes it, so an earlier opaque command's
    // wildcard cannot linger.
    let wildcard = force_raw && safe.is_empty();

    if force_raw {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            crate::debug_log!(
                "[skim] force-raw: failed to create sessions dir {:?}: {e} \
                 — marker suppressed; byte-exact pipe consumers may receive compressed output",
                dir
            );
            return;
        }

        // Safety: reject a sessions/ directory that is itself a symlink or
        // world-writable. An unprivileged attacker who controls a shared
        // SKIM_CACHE_DIR can plant a symlink before our first invocation and
        // receive arbitrary file overwrites via the marker write path. The
        // O_NOFOLLOW flag on the file open is a second, narrower layer for
        // the same race at the file level; this check closes the directory-
        // level window where the race is widest. Bail silently — neither
        // failure may break the pipeline, and both cost compression, not bytes.
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if std::fs::symlink_metadata(&dir).is_ok_and(|meta| {
                meta.file_type().is_symlink() || (meta.mode() & 0o022) != 0
            }) {
                return;
            }
        }
    }

    write_or_remove_marker(&marker_path(&dir, ppid, None), wildcard);
    for tool in &safe {
        write_or_remove_marker(&marker_path(&dir, ppid, Some(tool)), force_raw);
    }

    // Bound the sessions directory. `write_session_id` also cleans, but only
    // when the agent supplies a session id; this call runs on every hook
    // invocation, so `sessions/` has a bound that does not depend on
    // attribution being configured.
    cleanup_stale_rate_limited(&dir);
}

/// Return `true` when a fresh force-raw marker covers this process and `tool`.
///
/// Walks process ancestry up to [`MAX_ANCESTRY_DEPTH`] levels, mirroring
/// [`read_session_id`]. The walk is needed because the hook keys the marker to
/// the agent process while the wrapper runs two levels deeper (wrapper ← shell
/// ← agent). At each level two names match: the per-tool marker
/// `{pid}.{tool}.raw` and the wildcard `{pid}.raw`.
///
/// The marker is never removed on read: `git log | tee f` runs two wrapped
/// tools off one hook invocation, and both must see it.
///
/// A marker older than [`FORCE_RAW_MAX_AGE`] is ignored, so a leftover from a
/// crashed hook cannot disable compression indefinitely.
pub(crate) fn read_force_raw(cache_dir: &Path, tool: &str) -> bool {
    let sessions_dir = cache_dir.join(SESSIONS_DIR);
    // An unrepresentable tool name can still match the wildcard; it just has no
    // per-tool marker to look for.
    let tool = is_safe_marker_tool(tool).then_some(tool);
    let mut pid = std::process::id();

    for _ in 0..MAX_ANCESTRY_DEPTH {
        if is_fresh(&marker_path(&sessions_dir, pid, None), FORCE_RAW_MAX_AGE) {
            return true;
        }
        if let Some(t) = tool
            && is_fresh(&marker_path(&sessions_dir, pid, Some(t)), FORCE_RAW_MAX_AGE)
        {
            return true;
        }
        let Some(parent) = parent_of(pid) else {
            return false;
        };
        pid = parent;
    }

    false
}

/// Return `true` when `path` exists and its mtime is within `max_age`.
///
/// On the fidelity-critical **read** path a future mtime — `Err(_)` from
/// `duration_since` — means the marker was written by a clock ahead of ours
/// (NTP step correction, VM suspend/resume, NFS/SMB clock skew). The safe
/// direction is to treat the marker as **fresh**: the alternative is to
/// compress into a byte-exact consumer and lose bytes (#514). The **reap**
/// path (`cleanup_stale`) correctly keeps `unwrap_or(Duration::MAX)` because
/// a file that cannot predate itself is not yet due for removal.
fn is_fresh(path: &Path, max_age: Duration) -> bool {
    let Ok(mtime) = std::fs::metadata(path).and_then(|m| m.modified()) else {
        return false;
    };
    match SystemTime::now().duration_since(mtime) {
        Ok(age) => age <= max_age,
        // Err(_) means mtime > now: clock skew; err on the side of raw.
        Err(_) => true,
    }
}

// ============================================================================
// Private helpers
// ============================================================================

/// Try to read a sidecar file at `path`.
///
/// Returns `Some(session_id)` if:
/// 1. The file exists.
/// 2. Its mtime is within [`SIDECAR_MAX_AGE`] (not stale).
/// 3. Its content passes [`crate::analytics::is_safe_session_id`].
///
/// Returns `None` in all other cases (missing file, stale, invalid content).
fn try_read_sidecar(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let mtime = metadata.modified().ok()?;
    let age = SystemTime::now()
        .duration_since(mtime)
        .unwrap_or(Duration::MAX);
    if age > SIDECAR_MAX_AGE {
        return None;
    }

    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim();
    crate::analytics::is_safe_session_id(trimmed).then(|| trimmed.to_string())
}

/// Maximum interval between opportunistic cleanup runs.
///
/// Cleanup touches every file in the sessions directory — running it on every
/// hook write (potentially thousands per session) adds unbounded overhead.
/// This constant gates cleanup behind a sentinel file so it runs at most once
/// per hour regardless of write frequency.
const CLEANUP_RATE_LIMIT: Duration = Duration::from_secs(3600);

/// Sentinel file name written into `sessions_dir` after each cleanup run.
const CLEANUP_SENTINEL: &str = ".last_cleanup";

/// Run [`cleanup_stale`] only when the sentinel file is absent or older than
/// [`CLEANUP_RATE_LIMIT`].
///
/// Writes a fresh sentinel after each cleanup run. All errors are silently
/// ignored — this is best-effort.
fn cleanup_stale_rate_limited(sessions_dir: &Path) {
    let sentinel = sessions_dir.join(CLEANUP_SENTINEL);

    if let Ok(age) = std::fs::metadata(&sentinel)
        .and_then(|m| m.modified())
        .map(|mtime| {
            SystemTime::now()
                .duration_since(mtime)
                .unwrap_or(Duration::MAX)
        })
        && age < CLEANUP_RATE_LIMIT
    {
        // Cleaned up recently — skip.
        return;
    }

    cleanup_stale(sessions_dir);

    // Refresh sentinel (best-effort).
    let _ = std::fs::write(&sentinel, b"");
}

/// Remove sidecar files past their useful life: [`FORCE_RAW_MAX_AGE`] for
/// force-raw markers, [`CLEANUP_MAX_AGE`] for session-id sidecars.
///
/// Two clocks because the two file classes have very different lifetimes. A
/// `.raw` marker is dead the moment it exceeds [`FORCE_RAW_MAX_AGE`] — no
/// reader will honour it — so reaping it on the session sidecar's 24 h clock
/// would leave up to a day of provably-inert files behind. Reaping it on its
/// own clock is what makes `sessions/` bounded by *live* markers rather than by
/// a day of accumulated ones.
///
/// Called via [`cleanup_stale_rate_limited`] from both write paths
/// ([`write_session_id`] and [`set_force_raw`]). All errors are silently
/// ignored.
fn cleanup_stale(sessions_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(sessions_dir) else {
        return;
    };

    let now = SystemTime::now();

    for entry in entries.flatten() {
        // Skip the rate-limit sentinel itself.
        if entry.file_name() == CLEANUP_SENTINEL {
            continue;
        }
        let path = entry.path();
        let max_age = if path.extension().is_some_and(|e| e == FORCE_RAW_SUFFIX) {
            FORCE_RAW_MAX_AGE
        } else {
            CLEANUP_MAX_AGE
        };
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(mtime) = meta.modified() else { continue };
        let age = now.duration_since(mtime).unwrap_or(Duration::MAX);
        if age > max_age {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Return the PPID of the current process on Unix platforms.
///
/// Returns `None` on non-Unix platforms or if the result is ≤ 0.
#[cfg(unix)]
fn get_ppid() -> Option<u32> {
    // SAFETY: getppid() is always safe to call — it has no preconditions and
    // always succeeds. The result is a valid non-negative PID on success.
    let ppid = unsafe { libc::getppid() };
    if ppid <= 0 { None } else { Some(ppid as u32) }
}

#[cfg(not(unix))]
fn get_ppid() -> Option<u32> {
    None
}

/// Return the parent PID of `pid` on Linux by reading `/proc/{pid}/stat`.
///
/// The stat format is `"pid (comm) state ppid ..."`. The `comm` field may
/// contain spaces and parentheses, so we find the last `)` to locate field
/// boundaries reliably.
#[cfg(target_os = "linux")]
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Find the closing paren of the comm field (last `)` is safest).
    let after_comm = stat.rfind(')')? + 1;
    // Remaining fields: " state ppid ..."
    let fields: Vec<&str> = stat[after_comm..].split_whitespace().collect();
    // fields[0] = state, fields[1] = ppid
    let ppid: u32 = fields.get(1)?.parse().ok()?;
    if ppid == 0 { None } else { Some(ppid) }
}

/// Return the parent PID of `pid` on macOS using `proc_pidinfo(PROC_PIDTASKALLINFO)`.
///
/// `proc_pidinfo` fills a `proc_taskallinfo` struct whose `.pbsd.pbi_ppid`
/// field holds the parent PID. This avoids the deprecated `sysctl` path and
/// uses the stable libproc API available on macOS 10.5+.
#[cfg(target_os = "macos")]
fn parent_of(pid: u32) -> Option<u32> {
    use std::mem;

    // SAFETY: `proc_taskallinfo` is a plain C struct; zero-initialising it is
    // valid. `proc_pidinfo` fills it in-place via the raw pointer. The buffer
    // size matches the struct size exactly, as required by the API.
    // Flavor PROC_PIDTASKALLINFO (2) pairs with the proc_taskallinfo struct.
    let mut info: libc::proc_taskallinfo = unsafe { mem::zeroed() };
    let size = mem::size_of::<libc::proc_taskallinfo>() as libc::c_int;

    let ret = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTASKALLINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };

    // A return value of 0 or negative signals an error. A return value that
    // is positive but less than the expected struct size indicates a short
    // read — the remaining bytes were never filled in and would contain
    // zeroes from the mem::zeroed() initialisation. Both cases must be
    // rejected to avoid returning a garbage PPID.
    if ret < size {
        return None;
    }

    let ppid = info.pbsd.pbi_ppid;
    if ppid == 0 { None } else { Some(ppid) }
}

/// Fallback for non-Linux, non-macOS Unix (e.g., FreeBSD, Windows).
///
/// Ancestry walk is not supported on these platforms; the read path returns
/// `None` immediately when this is reached.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn parent_of(_pid: u32) -> Option<u32> {
    None
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Write a raw sidecar file into `dir/sessions/{pid}.id` with the given
    /// content, bypassing `write_session_id` so tests can control the exact
    /// content and mtime.
    fn write_raw_sidecar(sessions_dir: &Path, pid: u32, content: &str) -> PathBuf {
        std::fs::create_dir_all(sessions_dir).unwrap();
        let path = sessions_dir.join(format!("{pid}.id"));
        std::fs::write(&path, content).unwrap();
        path
    }

    /// Set the mtime of a file to `SystemTime::now() - age` using the `filetime`
    /// crate (the standard portable approach for tests).
    fn set_file_age(path: &Path, age: Duration) {
        use filetime::{FileTime, set_file_mtime};
        let target_mtime = SystemTime::now()
            .checked_sub(age)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let ft = FileTime::from_system_time(target_mtime);
        set_file_mtime(path, ft).unwrap();
    }

    // -----------------------------------------------------------------------
    // Force-raw marker
    // -----------------------------------------------------------------------

    /// Write a force-raw marker directly for `pid`, bypassing `set_force_raw`
    /// (which can only key the CURRENT process's PPID). `tool` selects the
    /// per-tool marker; `None` writes the wildcard.
    fn write_raw_marker(sessions_dir: &Path, pid: u32, tool: Option<&str>) -> PathBuf {
        std::fs::create_dir_all(sessions_dir).unwrap();
        let path = marker_path(sessions_dir, pid, tool);
        std::fs::write(&path, b"1").unwrap();
        path
    }

    /// Convenience: the tool-name vector `set_force_raw` takes.
    fn tools(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    /// A marker keyed to this process is found on the first step of the walk.
    #[test]
    fn test_read_force_raw_finds_marker_for_self() {
        let tmp = TempDir::new().unwrap();
        assert!(
            !read_force_raw(tmp.path(), "git"),
            "no marker means no force-raw"
        );
        write_raw_marker(&tmp.path().join(SESSIONS_DIR), std::process::id(), None);
        assert!(
            read_force_raw(tmp.path(), "git"),
            "marker for self must be found"
        );
    }

    /// A stale marker is ignored, so a crashed hook cannot disable compression
    /// indefinitely. Failing this way costs compression, never bytes.
    #[test]
    fn test_read_force_raw_ignores_stale_marker() {
        let tmp = TempDir::new().unwrap();
        let path = write_raw_marker(&tmp.path().join(SESSIONS_DIR), std::process::id(), None);
        set_file_age(&path, FORCE_RAW_MAX_AGE + Duration::from_secs(60));
        assert!(
            !read_force_raw(tmp.path(), "git"),
            "a marker older than FORCE_RAW_MAX_AGE must be ignored"
        );
    }

    /// `set_force_raw(false, …)` REMOVES the marker. The clear path is what
    /// keeps a marker from outliving the one command it describes.
    #[test]
    fn test_set_force_raw_clears_previous_marker() {
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join(SESSIONS_DIR);
        let ppid = get_ppid().expect("unix ppid");

        set_force_raw(true, &tools(&["git", "tee"]), tmp.path());
        let path = marker_path(&sessions, ppid, Some("git"));
        assert!(path.exists(), "set_force_raw(true) must write the marker");

        set_force_raw(false, &tools(&["git", "cat"]), tmp.path());
        assert!(
            !path.exists(),
            "set_force_raw(false) must remove the marker, not just skip writing"
        );
    }

    /// Clearing when nothing is there is a no-op, not an error — the hook calls
    /// it for every command that reaches extraction, most of which never set a marker.
    #[test]
    fn test_set_force_raw_clear_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        set_force_raw(false, &tools(&["ls"]), tmp.path());
        set_force_raw(false, &tools(&["ls"]), tmp.path());
        assert!(!read_force_raw(tmp.path(), "ls"));
    }

    /// The marker never collides with a session-id sidecar for the same PID:
    /// different suffixes, independent lifetimes.
    #[test]
    fn test_force_raw_marker_is_distinct_from_session_sidecar() {
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join(SESSIONS_DIR);
        let pid = std::process::id();
        write_raw_sidecar(&sessions, pid, "session-abc");
        write_raw_marker(&sessions, pid, None);

        assert_eq!(read_session_id(tmp.path()).as_deref(), Some("session-abc"));
        assert!(read_force_raw(tmp.path(), "git"));

        std::fs::remove_file(marker_path(&sessions, pid, None)).unwrap();
        assert!(!read_force_raw(tmp.path(), "git"));
        assert_eq!(
            read_session_id(tmp.path()).as_deref(),
            Some("session-abc"),
            "clearing the force-raw marker must not disturb session attribution"
        );
    }

    // -----------------------------------------------------------------------
    // Per-tool keying: PPID alone is not a command identity
    // -----------------------------------------------------------------------

    /// A marker written for `git` must not answer for `cargo`.
    ///
    /// This is the whole point of the tool component: PPID is shared by every
    /// command an agent runs, so a PPID-only marker made one command's verdict
    /// decide an unrelated one's.
    #[test]
    fn test_per_tool_marker_does_not_answer_for_another_tool() {
        let tmp = TempDir::new().unwrap();
        set_force_raw(true, &tools(&["git", "tee"]), tmp.path());

        assert!(
            read_force_raw(tmp.path(), "git"),
            "the marked tool must serve raw"
        );
        assert!(
            read_force_raw(tmp.path(), "tee"),
            "one hook invocation covers every tool the command names"
        );
        assert!(
            !read_force_raw(tmp.path(), "cargo"),
            "an unnamed tool must not inherit another command's verdict"
        );
    }

    /// An unrelated command's hook must not delete a live marker.
    ///
    /// Models two Bash tool calls in one agent turn: `git log | tee f` sets the
    /// marker, `cargo build`'s hook fires before `git` execs. With a PPID-only
    /// key the second call cleared the first's marker and the tee captured
    /// compressed bytes — a byte-fidelity loss (#317).
    #[test]
    fn test_unrelated_command_clear_preserves_live_marker() {
        let tmp = TempDir::new().unwrap();

        set_force_raw(true, &tools(&["git", "tee"]), tmp.path());
        set_force_raw(false, &tools(&["cargo"]), tmp.path());

        assert!(
            read_force_raw(tmp.path(), "git"),
            "a concurrent unrelated command must not clear another tool's marker"
        );
    }

    /// An empty tool set means "shape defeated head extraction" — `$(…)`,
    /// backticks, process substitution — and falls back to the wildcard, which
    /// matches every tool. Erring wide costs compression; erring narrow costs
    /// bytes.
    #[test]
    fn test_empty_tool_set_writes_matching_wildcard() {
        let tmp = TempDir::new().unwrap();
        set_force_raw(true, &[], tmp.path());

        assert!(read_force_raw(tmp.path(), "git"));
        assert!(read_force_raw(tmp.path(), "anything-at-all"));

        let ppid = get_ppid().expect("unix ppid");
        assert!(
            marker_path(&tmp.path().join(SESSIONS_DIR), ppid, None).exists(),
            "the fallback must be the wildcard file name"
        );
    }

    /// A named command never leaves the wildcard behind: the narrower per-tool
    /// encoding always replaces it, so an earlier `$(…)` cannot keep serving raw
    /// for unrelated tools.
    #[test]
    fn test_named_command_clears_a_previous_wildcard() {
        let tmp = TempDir::new().unwrap();
        set_force_raw(true, &[], tmp.path());
        assert!(read_force_raw(tmp.path(), "cargo"));

        set_force_raw(true, &tools(&["git"]), tmp.path());
        assert!(read_force_raw(tmp.path(), "git"));
        assert!(
            !read_force_raw(tmp.path(), "cargo"),
            "the wildcard must not survive a command that names its tools"
        );
    }

    /// The command string is untrusted hook input. A head that would escape the
    /// sessions directory is rejected outright — it must never reach `join`.
    #[test]
    fn test_unsafe_tool_names_are_rejected() {
        for bad in [
            "../../etc/passwd",
            "..",
            ".",
            ".hidden",
            "with/slash",
            "with space",
            "semi;colon",
            "",
        ] {
            assert!(!is_safe_marker_tool(bad), "{bad:?} must be rejected");
        }
        for good in ["git", "cargo", "sha256sum", "python3.11", "g++", "_x"] {
            assert!(is_safe_marker_tool(good), "{good:?} must be accepted");
        }
    }

    /// A traversal-shaped head writes no per-tool file anywhere, and because the
    /// safe set is then empty the command still gets wildcard coverage rather
    /// than silently losing its verdict.
    #[test]
    fn test_traversal_tool_name_writes_no_file_outside_sessions_dir() {
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join(SESSIONS_DIR);
        set_force_raw(true, &tools(&["../escape"]), tmp.path());

        assert!(
            !tmp.path().join("escape.raw").exists(),
            "a traversal head must not create a file outside sessions/"
        );
        let ppid = get_ppid().expect("unix ppid");
        assert!(
            marker_path(&sessions, ppid, None).exists(),
            "the rejected head must fall back to wildcard coverage"
        );
        assert!(read_force_raw(tmp.path(), "escape"));
    }

    /// `sessions/` is bounded without depending on session attribution being
    /// configured: `set_force_raw` runs on every hook invocation that reaches
    /// command extraction and reaps markers on the marker's own clock.
    #[test]
    fn test_set_force_raw_reaps_stale_markers() {
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join(SESSIONS_DIR);

        // A marker from a long-dead process, well past its useful life.
        let dead = write_raw_marker(&sessions, 99_999, Some("git"));
        set_file_age(&dead, FORCE_RAW_MAX_AGE + Duration::from_secs(600));
        // A session sidecar of the same age must NOT be reaped — different clock.
        let sidecar = write_raw_sidecar(&sessions, 99_998, "still-valid");
        set_file_age(&sidecar, FORCE_RAW_MAX_AGE + Duration::from_secs(600));

        set_force_raw(false, &tools(&["ls"]), tmp.path());

        assert!(
            !dead.exists(),
            "a marker past FORCE_RAW_MAX_AGE must be reaped on the write path"
        );
        assert!(
            sidecar.exists(),
            "session sidecars keep the 24 h clock; only .raw uses the short one"
        );
    }

    // -----------------------------------------------------------------------
    // write_session_id / read_session_id roundtrip
    // -----------------------------------------------------------------------

    /// AD-SC-1: Basic write→read roundtrip using the current process's own PID.
    ///
    /// `write_session_id` keys on PPID; we simulate that by writing directly to
    /// the sessions dir keyed to `std::process::id()` and reading back.
    #[test]
    fn test_write_read_roundtrip() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);

        // Write a sidecar keyed to our own PID so `read_session_id` depth-0 finds it.
        write_raw_sidecar(&sessions_dir, std::process::id(), "test-session-abc");

        let result = read_session_id(dir.path());
        assert_eq!(result, Some("test-session-abc".to_string()));
    }

    /// write_session_id creates the sessions directory if it does not exist.
    #[test]
    fn test_write_creates_sessions_dir() {
        let dir = TempDir::new().unwrap();
        // The sessions sub-directory does not exist yet.
        assert!(!dir.path().join(SESSIONS_DIR).exists());

        write_session_id("my-session", dir.path());

        assert!(dir.path().join(SESSIONS_DIR).exists());
    }

    /// A second write to the same sidecar overwrites the content and refreshes
    /// the mtime (staleness is reset).
    #[test]
    fn test_overwrite_updates_mtime() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);
        let Some(ppid) = get_ppid() else { return }; // non-Unix: skip

        // First write
        write_session_id("session-v1", dir.path());
        let path = sessions_dir.join(format!("{ppid}.id"));
        assert!(path.exists());

        // Age the file by 10 hours so it would be stale.
        set_file_age(&path, Duration::from_secs(10 * 3600));

        // Verify stale (sanity check)
        let meta = std::fs::metadata(&path).unwrap();
        let age = SystemTime::now()
            .duration_since(meta.modified().unwrap())
            .unwrap_or(Duration::MAX);
        assert!(age > SIDECAR_MAX_AGE, "should be stale before second write");

        // Second write should refresh mtime and update content.
        write_session_id("session-v2", dir.path());

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.trim(), "session-v2");

        let meta2 = std::fs::metadata(&path).unwrap();
        let age2 = SystemTime::now()
            .duration_since(meta2.modified().unwrap())
            .unwrap_or(Duration::MAX);
        assert!(
            age2 < SIDECAR_MAX_AGE,
            "mtime should be refreshed after second write"
        );
    }

    // -----------------------------------------------------------------------
    // read_session_id: negative cases
    // -----------------------------------------------------------------------

    /// Returns None when no matching sidecar file exists at any ancestry level.
    #[test]
    fn test_read_nonexistent() {
        let dir = TempDir::new().unwrap();
        let result = read_session_id(dir.path());
        assert_eq!(result, None);
    }

    /// Returns None for a sidecar that exceeds SIDECAR_MAX_AGE.
    #[test]
    fn test_read_stale_file() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);

        let path = write_raw_sidecar(&sessions_dir, std::process::id(), "stale-session");
        // Set mtime to 7 hours ago (past the 6h threshold).
        set_file_age(&path, Duration::from_secs(7 * 3600));

        let result = read_session_id(dir.path());
        assert_eq!(result, None, "stale sidecar should be ignored");
    }

    /// Returns None for content that fails is_safe_session_id (e.g., spaces).
    #[test]
    fn test_read_invalid_content() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);

        write_raw_sidecar(
            &sessions_dir,
            std::process::id(),
            "bad session id with spaces!",
        );

        let result = read_session_id(dir.path());
        assert_eq!(result, None, "invalid content should be rejected");
    }

    // -----------------------------------------------------------------------
    // Ancestry walk
    // -----------------------------------------------------------------------

    /// Depth-1 walk: sidecar keyed to PPID is found when there is no depth-0 sidecar.
    #[cfg(unix)]
    #[test]
    fn test_read_walks_ancestry() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);
        std::fs::create_dir_all(&sessions_dir).unwrap();

        let ppid = get_ppid().expect("must have ppid on unix");

        // Write only at the parent's PID — no sidecar for the current process.
        write_raw_sidecar(&sessions_dir, ppid, "parent-session");

        let result = read_session_id(dir.path());
        assert_eq!(result, Some("parent-session".to_string()));
    }

    // -----------------------------------------------------------------------
    // Isolation: different PID keys don't interfere
    // -----------------------------------------------------------------------

    #[test]
    fn test_concurrent_pids_isolated() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);

        // Write two different PIDs with different session IDs.
        let pid_a: u32 = 11111;
        let pid_b: u32 = 22222;

        write_raw_sidecar(&sessions_dir, pid_a, "session-for-a");
        write_raw_sidecar(&sessions_dir, pid_b, "session-for-b");

        // Read back each directly.
        let val_a = try_read_sidecar(&sessions_dir.join(format!("{pid_a}.id")));
        let val_b = try_read_sidecar(&sessions_dir.join(format!("{pid_b}.id")));

        assert_eq!(val_a, Some("session-for-a".to_string()));
        assert_eq!(val_b, Some("session-for-b".to_string()));
    }

    // -----------------------------------------------------------------------
    // cleanup_stale
    // -----------------------------------------------------------------------

    /// Files older than CLEANUP_MAX_AGE are removed when a new sidecar is written.
    ///
    /// Requires Unix because `write_session_id` calls `get_ppid()` which returns
    /// `None` (and returns early) on non-Unix platforms, so no cleanup is ever
    /// triggered there.
    #[cfg(unix)]
    #[test]
    fn test_cleanup_removes_old_files() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);

        // Plant a stale file (25 hours old — over the 24h cleanup threshold).
        let stale_pid: u32 = 99999;
        let stale_path = write_raw_sidecar(&sessions_dir, stale_pid, "old-session");
        set_file_age(&stale_path, Duration::from_secs(25 * 3600));

        assert!(
            stale_path.exists(),
            "stale file should exist before trigger"
        );

        // Trigger cleanup by writing a new sidecar (cleanup runs on every write).
        write_session_id("new-session", dir.path());

        assert!(
            !stale_path.exists(),
            "stale file should have been cleaned up"
        );
    }

    // -----------------------------------------------------------------------
    // Platform: parent_of / get_ppid
    // -----------------------------------------------------------------------

    /// parent_of(current_pid) returns a valid PID on Linux/macOS.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn test_parent_of_current_process() {
        let current_pid = std::process::id();
        let ppid = parent_of(current_pid);
        assert!(
            ppid.is_some(),
            "parent_of(current_pid) must return Some on Linux/macOS"
        );
        assert!(ppid.unwrap() > 0, "ppid must be > 0");
    }

    // -----------------------------------------------------------------------
    // Sidecar is primary: read_session_id is the first resolver in main()
    //
    // The sidecar round-trip must work correctly (write then read returns the
    // same value). Additionally, `or_else` short-circuits so a pre-resolved
    // Some value from an earlier ancestor sidecar is not overwritten.
    // -----------------------------------------------------------------------

    /// read_session_id returns the sidecar value when present, and a pre-
    /// resolved value (from an earlier priority step) is not clobbered.
    /// This tests the sidecar API itself — the priority ordering is in main().
    #[test]
    fn test_read_session_id_is_a_fallback() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);
        write_raw_sidecar(&sessions_dir, std::process::id(), "sidecar-value");

        // When no explicit session_id is present, the sidecar is used.
        let from_sidecar = None::<String>.or_else(|| read_session_id(dir.path()));
        assert_eq!(from_sidecar, Some("sidecar-value".to_string()));

        // When an explicit session_id is already present, the sidecar is not used.
        let explicit = Some("explicit-value".to_string());
        let resolved = explicit.or_else(|| read_session_id(dir.path()));
        assert_eq!(resolved, Some("explicit-value".to_string()));
    }

    // -----------------------------------------------------------------------
    // write_session_id: validation guard (issue write-validate)
    // -----------------------------------------------------------------------

    /// write_session_id must silently reject invalid session IDs and not create
    /// any file. This is the defense-in-depth guard added to the function itself
    /// regardless of whether the caller already validated.
    #[cfg(unix)]
    #[test]
    fn test_write_rejects_invalid_session_id() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);

        // A session ID with spaces fails is_safe_session_id.
        write_session_id("bad session id!", dir.path());

        // No sidecar file should have been created.
        let Some(ppid) = get_ppid() else { return };
        let sidecar = sessions_dir.join(format!("{ppid}.id"));
        assert!(
            !sidecar.exists(),
            "write_session_id must not create a file for an invalid session ID"
        );
    }

    // -----------------------------------------------------------------------
    // cleanup_stale_rate_limited (issue cleanup-hot-path)
    // -----------------------------------------------------------------------

    /// cleanup_stale_rate_limited must skip the full directory scan when the
    /// sentinel file is fresh (written within CLEANUP_RATE_LIMIT).
    #[test]
    fn test_cleanup_rate_limited_skips_when_sentinel_fresh() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);
        std::fs::create_dir_all(&sessions_dir).unwrap();

        // Plant a stale sidecar that would be removed if cleanup ran.
        let stale_pid: u32 = 88888;
        let stale_path = write_raw_sidecar(&sessions_dir, stale_pid, "old-session");
        set_file_age(&stale_path, Duration::from_secs(25 * 3600));

        // Write a fresh sentinel so cleanup is skipped.
        let sentinel = sessions_dir.join(CLEANUP_SENTINEL);
        std::fs::write(&sentinel, b"").unwrap();
        // Sentinel is brand new — within CLEANUP_RATE_LIMIT.

        cleanup_stale_rate_limited(&sessions_dir);

        assert!(
            stale_path.exists(),
            "stale file must NOT be removed when cleanup is rate-limited by a fresh sentinel"
        );
    }

    /// cleanup_stale_rate_limited must run cleanup when the sentinel is older
    /// than CLEANUP_RATE_LIMIT.
    #[test]
    fn test_cleanup_rate_limited_runs_when_sentinel_old() {
        let dir = TempDir::new().unwrap();
        let sessions_dir = dir.path().join(SESSIONS_DIR);
        std::fs::create_dir_all(&sessions_dir).unwrap();

        // Plant a stale sidecar.
        let stale_pid: u32 = 77777;
        let stale_path = write_raw_sidecar(&sessions_dir, stale_pid, "old-session");
        set_file_age(&stale_path, Duration::from_secs(25 * 3600));

        // Write an old sentinel (2 hours old — past the 1-hour rate limit).
        let sentinel = sessions_dir.join(CLEANUP_SENTINEL);
        std::fs::write(&sentinel, b"").unwrap();
        set_file_age(&sentinel, CLEANUP_RATE_LIMIT + Duration::from_secs(1));

        cleanup_stale_rate_limited(&sessions_dir);

        assert!(
            !stale_path.exists(),
            "stale file must be removed when sentinel is older than CLEANUP_RATE_LIMIT"
        );
    }

    // -----------------------------------------------------------------------
    // security-2: O_NOFOLLOW — symlink-planting attack
    // -----------------------------------------------------------------------

    /// A symlink planted inside `sessions/` must not cause the marker write to
    /// follow it and overwrite an arbitrary file.
    ///
    /// Regression for security-2: `write_or_remove_marker` opens with
    /// `O_NOFOLLOW`, so an attacker who pre-plants `{ppid}.{tool}.raw` as a
    /// symlink cannot redirect the write to a file outside `sessions/`.
    #[cfg(unix)]
    #[test]
    fn test_symlink_in_sessions_dir_is_not_followed() {
        let cache_tmp = TempDir::new().unwrap();
        let sessions = cache_tmp.path().join(SESSIONS_DIR);
        std::fs::create_dir_all(&sessions).unwrap();

        // Create a victim file outside sessions/ with known content.
        let victim = cache_tmp.path().join("victim_file.txt");
        std::fs::write(&victim, b"original content").unwrap();

        // Plant a symlink at the path that set_force_raw would use for "git".
        // Use a dummy PID so the marker name is deterministic.
        let symlink_name = "99990.git.raw";
        let symlink_path = sessions.join(symlink_name);
        std::os::unix::fs::symlink(&victim, &symlink_path).unwrap();

        // write_or_remove_marker must not truncate+overwrite victim via the symlink.
        // Use cache_tmp itself as the log_env so that any hook.log stays inside
        // the temp dir rather than leaking to the developer's ~/.cache/skim.
        let log_env = crate::cmd::hook_log::CacheEnv {
            cache_dir_override: Some(cache_tmp.path().to_path_buf()),
        };
        write_or_remove_marker_with_log(&symlink_path, true, &log_env);

        let content = std::fs::read_to_string(&victim).unwrap();
        assert_eq!(
            content, "original content",
            "write_or_remove_marker must not follow symlinks (O_NOFOLLOW): victim_file was modified"
        );
    }

    // -----------------------------------------------------------------------
    // reliability-7: future mtime is treated as fresh on the read path
    // -----------------------------------------------------------------------

    /// A marker whose mtime is in the future (NTP step, VM clock skew) must be
    /// treated as fresh on the read path. Discarding it would mean compressing
    /// into a byte-exact consumer — measured byte loss of 304 vs 6803 (#514).
    #[test]
    fn test_is_fresh_treats_future_mtime_as_fresh() {
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join(SESSIONS_DIR);
        let path = write_raw_marker(&sessions, std::process::id(), None);

        // Set mtime 60 seconds into the future — simulates NTP step / clock skew.
        use filetime::{FileTime, set_file_mtime};
        let future = SystemTime::now()
            .checked_add(Duration::from_secs(60))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        set_file_mtime(&path, FileTime::from_system_time(future)).unwrap();

        assert!(
            is_fresh(&path, FORCE_RAW_MAX_AGE),
            "a marker with a future mtime (clock skew) must be treated as fresh \
             — discarding it would compress into a byte-exact consumer (#514)"
        );
    }

    // -----------------------------------------------------------------------
    // reliability-6: marker write failure is logged to hook.log
    // -----------------------------------------------------------------------

    /// When `O_NOFOLLOW` rejects a symlink path, the failure must be logged to
    /// `hook.log` rather than discarded silently. A silent failure means a
    /// byte-exact pipe consumer receives compressed output with no diagnostic.
    #[cfg(unix)]
    #[test]
    fn test_marker_write_failure_is_logged_to_hook_log() {
        let cache_tmp = TempDir::new().unwrap();
        let sessions = cache_tmp.path().join(SESSIONS_DIR);
        std::fs::create_dir_all(&sessions).unwrap();

        // Plant a symlink — O_NOFOLLOW causes ELOOP, triggering the log path.
        let victim = cache_tmp.path().join("victim2.txt");
        std::fs::write(&victim, b"safe").unwrap();
        let symlink_path = sessions.join("99991.git.raw");
        std::os::unix::fs::symlink(&victim, &symlink_path).unwrap();

        // Inject a CacheEnv pointing at our temp dir so hook.log lands there.
        let log_env = crate::cmd::hook_log::CacheEnv {
            cache_dir_override: Some(cache_tmp.path().to_path_buf()),
        };
        write_or_remove_marker_with_log(&symlink_path, true, &log_env);

        // hook.log must contain a warning mentioning the failure.
        let hook_log = cache_tmp.path().join("hook.log");
        assert!(hook_log.exists(), "hook.log must be created on marker failure");
        let log_content = std::fs::read_to_string(&hook_log).unwrap();
        assert!(
            log_content.contains("force-raw:"),
            "hook.log must contain a force-raw warning, got: {log_content}"
        );
        assert!(
            log_content.contains("#514"),
            "hook.log warning must reference issue #514, got: {log_content}"
        );
    }
}
