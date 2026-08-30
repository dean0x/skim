//! Temporal-DB staleness helpers and anchor management.
//!
//! Owns: the `ReanchorPolicy` control type, the lightweight `read_temporal_meta`
//! probe, the `temporal_db_is_stale` staleness gate, `AnchorState` and its
//! checker `temporal_anchor_state`, the user-facing `warn_if_temporal_unverifiable`
//! advisory, and the non-fatal rebuild orchestrator `try_rebuild_temporal_nonfatal`.
//!
//! # Module boundary note
//!
//! Reads of `temporal.db` meta keys use a lightweight read-only SQLite connection
//! (no WAL pragma, no permission reset, no migrations) rather than the full
//! `TemporalDb::open` path. This is an intentional performance trade-off (ADR-003):
//! meta checks run on every query; the full open cost is justified only when data
//! is actually read. Writes always go through the domain API (`TemporalDb::set_meta`,
//! `temporal_build.rs`) — one key, one schema owner.

use std::path::{Path, PathBuf};

use super::gitdir::{HeadState, resolve_repo_toplevel};

// ============================================================================
// ReanchorPolicy
// ============================================================================

/// Controls whether [`try_rebuild_temporal_nonfatal`] and [`super::staleness::auto_refresh_if_stale`]
/// may overwrite the persisted `git_toplevel` anchor when an [`AnchorState::Differs`]
/// mismatch is detected (PF-017).
///
/// Using a named enum rather than a bare `bool` makes every call site
/// self-documenting and prevents the two failure modes of a wrong literal:
/// - `Refuse` on a build arm would silently break the `--rebuild` re-anchor recovery.
/// - `Allow` on a query arm would restore the PF-017 bug (query-triggered silent retarget).
///
/// The project's "Explicit over implicit" engineering rule (engineering.md) applies
/// directly here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReanchorPolicy {
    /// The caller is an explicit build arm (`--build`, `--rebuild`, `--update`):
    /// re-anchoring is permitted and will be disclosed to the user on stderr.
    Allow,
    /// The caller is a query-path or self-heal: an anchor mismatch must NOT
    /// silently retarget `temporal.db` (PF-017).
    Refuse,
}

// ============================================================================
// Lightweight meta reader (performance-optimised — no WAL / migrations)
// ============================================================================

/// Read a single TEXT value from the `meta` table of `temporal.db`.
///
/// Opens a lightweight read-only connection (no WAL pragma, no permission
/// reset, no migrations) and queries the `meta` table for `key`.  Returns
/// `None` when the file is absent, the connection cannot be opened, or the key
/// has no row.
///
/// Shared by [`temporal_db_is_stale`] (for both `git_head` and `data_version`
/// keys), [`warn_if_temporal_unverifiable`] (for `git_head`), and
/// [`temporal_anchor_state`] (for `git_toplevel`) — one implementation, no drift.
///
/// # Read/write symmetry note
///
/// Writes always go through `TemporalDb::set_meta` (domain API in `rskim-search`).
/// This function uses an intentionally lighter open for read-path performance
/// (ADR-003); both paths execute the same `SELECT value FROM meta WHERE key = ?1`
/// query. Keeping both in this module ensures schema drift is caught at review time.
fn read_temporal_meta(cache_dir: &Path, key: &str) -> Option<String> {
    let db_path = cache_dir.join("temporal.db");
    if !db_path.exists() {
        return None;
    }
    // Lightweight read-only open: no WAL pragma, no permission reset, no migrations.
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.query_row(
        "SELECT value FROM meta WHERE key = ?1",
        rusqlite::params![key],
        |row| row.get(0),
    )
    .ok()
}

// ============================================================================
// Staleness gates
// ============================================================================

/// Return `true` when `temporal.db` is missing or its stored `META_GIT_HEAD`
/// does not match `current_head`.
///
/// `current_head` is the HEAD SHA already read by the caller (non-optional —
/// callers must check `current_head.is_some()` BEFORE calling this helper; on
/// non-git dirs the guard short-circuits before reaching this function).
///
/// # Performance (ADR-003)
///
/// Delegates to [`read_temporal_meta`] which uses the same lightweight
/// read-only SQLite open (no WAL pragma, no permission reset, no migrations).
/// This avoids the full `TemporalDb::open` cost on the steady-state
/// Current-path where the DB is checked but then immediately re-opened by the
/// dispatch arm.  The caller is responsible for the full `TemporalDb::open`
/// when it actually queries the DB.
///
/// # AD-TMP-2 / AD-TMP-3
///
/// AD-TMP-2: temporal.db staleness is INDEPENDENT of lexical staleness (#357
/// BUG B). The lexical-Current early-return in `auto_refresh_if_stale` (below)
/// skipped the temporal hook, so a missing or HEAD-divergent temporal.db stayed
/// stale forever while the lexical index was current (post-upgrade, manual
/// delete, or 2nd+ query after a temporal-less rebuild due to BUG A). This
/// helper checks temporal.db's stored META_GIT_HEAD against the `current_head`
/// already read at function entry in `auto_refresh_if_stale`. Self-heals the
/// stuck-stale (deadbeef) case. Non-fatal by ADR-006/D5.
///
/// AD-TMP-3: production temporal staleness uses file-IO HEAD comparison here,
/// not `check_temporal_staleness` from `temporal.rs` — that helper is
/// `#[cfg(test)]`-only and uses a `git rev-parse` subprocess, which is
/// inconsistent with this module's subprocess-free design. `current_head` is
/// the single HEAD read already performed at `auto_refresh_if_stale` entry;
/// passing it here avoids a second HEAD read and keeps one HEAD-reading
/// authority per call.
pub(super) fn temporal_db_is_stale(cache_dir: &Path, current_head: &str) -> bool {
    let db_path = cache_dir.join("temporal.db");
    if !db_path.exists() {
        return true;
    }

    // Check 1: HEAD match — absent row or mismatch both report stale.
    let stored_head = read_temporal_meta(cache_dir, rskim_search::META_GIT_HEAD);
    if stored_head.as_deref() != Some(current_head) {
        return true;
    }

    // AD-408-4: Check 2: data-version gate.
    // The DB is stale when the stored data_version is absent or numerically less
    // than TEMPORAL_DATA_VERSION, forcing a self-heal rebuild on the next query
    // (applies ADR-006; mirrors the lexical/AST/manifest self-heal in
    // check_staleness). Meta values are TEXT — version comparison is numeric to
    // correctly order multi-digit values (string compare mis-orders "10" vs "2").
    // An absent or non-integer stored value is treated as stale (pre-fix DB).
    // Uses `stored < current` (NOT `!=`) so a DB written by a newer binary is
    // NOT needlessly rebuilt by an older post-fix binary (no downgrade loop).
    let stored_version = read_temporal_meta(cache_dir, rskim_search::META_DATA_VERSION);
    match stored_version.as_deref() {
        Some(v) => match v.parse::<u64>() {
            Ok(n) => n < u64::from(rskim_search::TEMPORAL_DATA_VERSION),
            // Non-integer stored value → treat as stale.
            Err(_) => true,
        },
        // Absent data_version row → stale (pre-fix DB that lacks the ghost filter).
        None => true,
    }
}

// ============================================================================
// Temporal unverifiable advisory
// ============================================================================

/// Emit an advisory warning when git HEAD is unresolvable but `temporal.db`
/// has data that cannot be verified as current (AD-413-9).
///
/// Triple-gated (R5):
/// 1. `HeadState::Unresolved` (zero cost on healthy repos or non-repos).
/// 2. `temporal.db` exists (zero SQLite opens unless needed — AC24).
/// 3. A `git_head` row is recorded (no DB on the unborn-branch no-loop case).
///
/// Never called from `auto_refresh_if_stale` — that path is reached on every
/// query, so emitting there would produce permanent stderr noise on plain
/// non-temporal queries, which #414 SE-1/AC-30 forbids (A1 wiring correction).
/// See Step 7 wiring in the plan for the correct call sites.
pub(super) fn warn_if_temporal_unverifiable(cache_dir: &Path, head: &HeadState) {
    if !matches!(head, HeadState::Unresolved) {
        return; // zero cost on healthy repos and on non-repos
    }
    if !cache_dir.join("temporal.db").exists() {
        return; // zero SQLite opens unless needed (AC24 guard ordering)
    }
    let Some(stored) = read_temporal_meta(cache_dir, rskim_search::META_GIT_HEAD) else {
        return; // no recorded HEAD → no advisory (unborn-branch no-loop case, Case A)
    };
    // AD-412-4: `stored` is read verbatim from `temporal.db` (untrusted path bytes).
    // Use char-based truncation so a multi-byte sequence at position 7-8 does not
    // produce a byte-slice boundary panic or fall back to printing the full raw value.
    let sha_prefix: String = stored.chars().take(8).collect();
    eprintln!(
        "skim search: git HEAD is unresolvable here — temporal ranking is served from \
         recorded commit {sha_prefix}… and cannot be verified as current",
    );
}

// ============================================================================
// Anchor state
// ============================================================================

/// State of the repository anchor recorded in `temporal.db`'s `meta` table.
///
/// AD-413-16: the toplevel that produced temporal rows is persisted as
/// `meta.git_toplevel` so query arms can refuse rather than silently serving
/// data from a different repository when the indexed root has been retargeted.
#[derive(Debug, PartialEq)]
pub(super) enum AnchorState {
    /// Root has its own `.git` — the anchor mechanism is irrelevant (plain repo or submodule).
    /// Gate 1 of `temporal_anchor_state` returns this for every non-adopted root (AC32).
    NotAdopted,
    /// No `temporal.db` or no `git_toplevel` row — adopt and record on the next rebuild.
    Absent,
    /// Persisted toplevel matches the live resolution — temporal data is trustworthy.
    Agrees,
    /// Persisted toplevel was written by a DIFFERENT repository than the current one.
    /// Temporal-consuming query arms must refuse (no rows served, no rebuild, exit 0).
    /// Explicit build arms (`--build`/`--rebuild`/`--update`) re-anchor loudly.
    Differs { recorded: String, live: PathBuf },
}

/// AD-413-16: compare the persisted repository anchor in `temporal.db` against
/// the toplevel that would be adopted for `root` today.
///
/// Cost: `NotAdopted` is returned for every root that has a `.git` entry — both
/// AC32 corpora and every existing user — performing zero DB reads and zero
/// SQLite opens.  Only an adopted (subdirectory) root reads the anchor row.
pub(super) fn temporal_anchor_state(cache_dir: &Path, root: &Path) -> AnchorState {
    // Gate 1: root that owns `.git` is never re-pointed (AC17, AC32).
    let Some(top) = resolve_repo_toplevel(root) else {
        return AnchorState::NotAdopted;
    };
    // Gate 2: no DB means no anchor — adopt and record on the next build.
    if !cache_dir.join("temporal.db").exists() {
        return AnchorState::Absent;
    }
    match read_temporal_meta(cache_dir, rskim_search::META_GIT_TOPLEVEL) {
        None => AnchorState::Absent,
        Some(rec) if Path::new(&rec) == top.as_path() => AnchorState::Agrees,
        Some(rec) => AnchorState::Differs {
            recorded: rec,
            live: top,
        },
    }
}

// ============================================================================
// Non-fatal rebuild orchestrator
// ============================================================================

/// Rebuild `temporal.db` non-fatally, swallowing any error per ADR-006/D5.
///
/// This is the single implementation of the D5 non-fatal-swallow contract that
/// was previously duplicated in three structurally-divergent copies across
/// `run_build` (mod.rs), the BUG-B self-heal (here), and the post-rebuild hook
/// (below). Centralising it prevents the copies from drifting independently —
/// a single edit here updates all three call sites.
///
/// # Contract (ADR-006/D5)
///
/// - `rebuild_temporal` is always called when `head` is `Some`.
/// - If `rebuild_temporal` returns `Err`, the error is SWALLOWED (never propagated).
/// - A debug-gated warning is emitted to stderr via `eprintln!` when the error
///   is swallowed and `SKIM_DEBUG=1` / `--debug` is set.
/// - Callers never see a temporal failure — only lexical/AST failures propagate.
///
/// # Parameters
///
/// - `root`: project root passed to `rebuild_temporal`.
/// - `cache_dir`: cache directory containing `temporal.db`.
/// - `head`: the git HEAD SHA to record; `None` skips the rebuild (non-git dir).
/// - `debug_label`: short label for the debug message (e.g. `"self-heal"`,
///   `"post-rebuild"`, `"--rebuild hook"`).
/// - `reanchor`: when [`ReanchorPolicy::Refuse`], a `Differs` anchor state (PF-017)
///   causes the temporal rebuild to be SKIPPED, leaving `temporal.db` byte-unchanged.
///   Pass [`ReanchorPolicy::Allow`] only from the explicit build arms (`--build`,
///   `--rebuild`, `--update`) so that only user-initiated rebuilds may retarget the
///   repository anchor.
///
///   With [`ReanchorPolicy::Allow`] a `Differs` anchor is re-anchored and DISCLOSED
///   on stderr naming both the recorded and the live toplevel (AD-413-16 / R17):
///   the retarget is a user action, so it must never be silent.
///
/// # Cost (AC24 / AC32)
///
/// [`temporal_anchor_state`]'s first gate is [`resolve_repo_toplevel`], which
/// returns `None` after a single `.git` existence probe for every root that owns
/// its own `.git` — every pre-existing user.  Such roots therefore perform zero
/// anchor reads and zero SQLite opens on both the `Allow` and `Refuse` paths.
pub(super) fn try_rebuild_temporal_nonfatal(
    root: &Path,
    cache_dir: &Path,
    head: Option<&str>,
    debug_label: &str,
    reanchor: ReanchorPolicy,
) {
    use super::temporal_build::{current_epoch_secs, rebuild_temporal};

    let Some(head) = head else { return };
    // PF-017: a changed `--root` toplevel also changes the adopted HEAD, so without
    // this gate `check_staleness` would report `HeadChanged`, `auto_refresh_if_stale`
    // would rebuild, and `record_temporal_anchor` would overwrite the anchor — on a
    // PLAIN LEXICAL QUERY that never asked for temporal data.  Only the three explicit
    // build arms pass `ReanchorPolicy::Allow`; every other caller (self-heal,
    // query-path post-rebuild) passes `ReanchorPolicy::Refuse`, leaving `temporal.db`
    // untouched on anchor mismatch.
    if let AnchorState::Differs { recorded, live } = temporal_anchor_state(cache_dir, root) {
        if reanchor == ReanchorPolicy::Refuse {
            if crate::debug::is_debug_enabled() {
                // AD-412-4: both `recorded` (from temporal.db — untrusted) and
                // `live` (a PathBuf — filesystem-derived) use `{:?}` so that ESC/CR/LF
                // bytes in either value are quoted rather than emitted raw to stderr.
                eprintln!(
                    "skim search [debug]: temporal rebuild skipped — anchor mismatch \
                     (recorded={:?}, live={:?}); use `skim search --rebuild` to re-anchor",
                    recorded,
                    live.display().to_string(),
                );
            }
            return;
        }
        // AD-413-16 / R17: an explicit build arm MAY retarget, but never silently —
        // this is the one line that turns the retarget into a user-visible action and
        // the documented recovery from the refusal.  Unconditional (not debug-gated):
        // AC33(f) asserts it on a plain `--rebuild`.  `{:?}` on both paths follows the
        // AD-412-4 hardening (untrusted path bytes are quoted, never raw).
        eprintln!(
            "skim search: re-anchoring temporal data to a different repository \
             (recorded: {:?}, live: {:?})",
            recorded,
            live.display().to_string(),
        );
    }
    if let Err(e) = rebuild_temporal(root, cache_dir, head, current_epoch_secs()) {
        // Ignore temporal errors — they must not fail the lexical/AST query (ADR-006/D5).
        if crate::debug::is_debug_enabled() {
            eprintln!("skim search [debug]: temporal {debug_label} error (non-fatal): {e}");
        }
    }
}
